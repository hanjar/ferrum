/**
 * DSK-06.3 — invalidation tests: a successful delete/rename/move that touches
 * the scanned scope marks the scan STALE (chip + rescan CTA), never claims
 * freed space. Queued moves are observed to their terminal state through the
 * recorded-op path (the same handler the operations://progress listener feeds).
 * The full path is proven through the REAL Dialogs.svelte confirm button.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render } from '@testing-library/svelte';
import { tick } from 'svelte';

const duMocks = vi.hoisted(() => ({
  start: vi.fn(),
  cancel: vi.fn(),
  snapshot: vi.fn(),
  queryChildren: vi.fn(),
  topFiles: vi.fn(),
}));
vi.mock('../ipc/diskUsage', () => ({
  diskUsageStart: duMocks.start,
  diskUsageCancel: duMocks.cancel,
  diskUsageSnapshot: duMocks.snapshot,
  diskUsageQueryChildren: duMocks.queryChildren,
  diskUsageTopFiles: duMocks.topFiles,
  onDiskUsageEvent: vi.fn(async () => () => {}),
  diskUsageCacheLoad: vi.fn(async () => null),
  diskUsageCacheDelete: vi.fn(async () => 0),
  diskUsageCacheList: vi.fn(async () => []),
  diskUsageCacheNoteMutation: vi.fn(async () => undefined),
}));

const mutMocks = vi.hoisted(() => ({
  planCopy: vi.fn(),
  createEntry: vi.fn(),
  deleteEntries: vi.fn(),
  enqueueCopy: vi.fn(),
  enqueueMove: vi.fn(),
  renameEntry: vi.fn(),
}));
vi.mock('../ipc/mutations', () => mutMocks);

// FER-585: the dialog deletes through the async job; the mock above stands for its backend result.
vi.mock('../ipc/deleteJobs', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../ipc/deleteJobs')>()),
  runDelete: async (paths: string[], permanent: boolean) => ({
    report: await mutMocks.deleteEntries(paths, permanent),
    cancelled: false,
    notProcessed: [],
    partial: [],
  }),
}));

// DSK-11: invalidation walks the ONE workspace store; this file exercises it
// (a foreign instance built below proves the isolation).
const { duWorkspace } = await import('./duWorkspace.svelte');
const { createDiskUsageStore } = await import('./diskUsageStore.svelte');
const diskUsage = duWorkspace.store;
const inv = await import('./invalidation');
const { dialogs } = await import('../stores/dialogStore.svelte');

const ROOT = 'C:\\data';

function op(over: Partial<import('../ipc/types').OperationProgress>) {
  return {
    operationId: 'op1',
    kind: 'move' as const,
    state: 'completed' as const,
    processedBytes: 10,
    processedFiles: 1,
    ...over,
  };
}

beforeEach(() => {
  diskUsage.reset();
  inv.resetInvalidationForTests();
  duMocks.start.mockReset().mockResolvedValue('s2');
  mutMocks.deleteEntries.mockReset();
});

afterEach(() => {
  cleanup();
  dialogs.close();
});

describe('withinScope', () => {
  it('matches the root itself and everything inside (case/separator insensitive)', () => {
    expect(inv.withinScope(ROOT, ROOT)).toBe(true);
    expect(inv.withinScope(ROOT, `${ROOT}\\a.txt`)).toBe(true);
    expect(inv.withinScope(ROOT, 'c:/DATA/sub/x.bin')).toBe(true);
  });

  it('never matches sibling prefixes or unrelated paths', () => {
    expect(inv.withinScope(ROOT, 'C:\\data2\\a.txt')).toBe(false); // prefix ≠ scope
    expect(inv.withinScope(ROOT, 'C:\\dat')).toBe(false);
    expect(inv.withinScope(ROOT, 'D:\\data\\a.txt')).toBe(false);
    expect(inv.withinScope(ROOT, '')).toBe(false);
    expect(inv.withinScope('', `${ROOT}\\a.txt`)).toBe(false);
  });
});

describe('noteScopeMutation (DSK-06.3)', () => {
  it('a successful delete inside the scope marks the scan stale (chip warn + time marker)', () => {
    diskUsage.testData('s1', 'complete', ROOT);
    inv.noteScopeMutation([`${ROOT}\\big.vhd`]);
    expect(diskUsage.phase).toBe('stale');
    expect(diskUsage.chip.tone).toBe('warn');
    expect(diskUsage.chip.text).toMatch(/^Stale · /); // marker until rescan (states.md rule 3)
  });

  it('a rename inside the scope (old or new path) marks stale', () => {
    diskUsage.testData('s1', 'complete', ROOT);
    inv.noteScopeMutation([`${ROOT}\\old.txt`, `${ROOT}\\new.txt`]);
    expect(diskUsage.phase).toBe('stale');
  });

  it('mutations outside the scope never stale the scan', () => {
    diskUsage.testData('s1', 'complete', ROOT);
    inv.noteScopeMutation(['D:\\elsewhere\\x', 'C:\\data2\\y']);
    expect(diskUsage.phase).toBe('complete');
  });

  it('DSK-11: invalidation touches only the ONE workspace store — a foreign instance is untouched', () => {
    diskUsage.testData('s1', 'complete', ROOT);
    const foreign = createDiskUsageStore();
    foreign.testData('s9', 'complete', ROOT);
    inv.noteScopeMutation([`${ROOT}\\a.txt`]);
    expect(diskUsage.phase).toBe('stale');
    expect(foreign.phase).toBe('complete');
  });

  it('phases markStale does not cover stay honest (cancelled is not flipped)', () => {
    diskUsage.testData('s1', 'cancelled', ROOT);
    inv.noteScopeMutation([`${ROOT}\\a`]);
    expect(diskUsage.phase).toBe('cancelled');
  });

  it('the Rescan CTA path clears the stale marker (start → scanning)', async () => {
    diskUsage.testData('s1', 'complete', ROOT);
    inv.noteScopeMutation([`${ROOT}\\big.vhd`]);
    expect(diskUsage.phase).toBe('stale');
    diskUsage.rootInput = ROOT;
    await diskUsage.start();
    expect(diskUsage.phase).toBe('scanning');
    expect(diskUsage.needsRescan).toBe(false);
  });
});

describe('queued move observation (enqueueMove → BTM terminal state)', () => {
  it('a completed recorded move inside the scope marks stale', () => {
    diskUsage.testData('s1', 'complete', ROOT);
    inv.recordQueuedMove('op1', [`${ROOT}\\big.vhd`], 'D:\\other');
    inv.handleOperationProgress(op({ state: 'running', processedFiles: 0 }));
    expect(diskUsage.phase).toBe('complete'); // not yet — enqueue is not success
    inv.handleOperationProgress(op({ state: 'completed' }));
    expect(diskUsage.phase).toBe('stale');
  });

  it('a move INTO the scope also invalidates (files appear)', () => {
    diskUsage.testData('s1', 'complete', ROOT);
    inv.recordQueuedMove('op2', ['D:\\stuff'], ROOT);
    inv.handleOperationProgress(op({ operationId: 'op2', state: 'completed' }));
    expect(diskUsage.phase).toBe('stale');
  });

  it('a failed/cancelled move with zero processed files does not mark stale', () => {
    diskUsage.testData('s1', 'complete', ROOT);
    inv.recordQueuedMove('op3', [`${ROOT}\\big.vhd`], 'D:\\other');
    inv.handleOperationProgress(op({ operationId: 'op3', state: 'failed', processedFiles: 0 }));
    expect(diskUsage.phase).toBe('complete');
  });

  it('a failed move that already processed files marks stale (they may have moved)', () => {
    diskUsage.testData('s1', 'complete', ROOT);
    inv.recordQueuedMove('op4', [`${ROOT}\\big.vhd`], 'D:\\other');
    inv.handleOperationProgress(op({ operationId: 'op4', state: 'cancelled', processedFiles: 2 }));
    expect(diskUsage.phase).toBe('stale');
  });

  it('untracked operations are ignored', () => {
    diskUsage.testData('s1', 'complete', ROOT);
    inv.handleOperationProgress(op({ operationId: 'unknown-op' }));
    expect(diskUsage.phase).toBe('complete');
  });
});

describe('full path through the existing delete dialog', () => {
  it('confirming the EXISTING delete dialog marks a touching scan stale', async () => {
    const Dialogs = (await import('../components/Dialogs.svelte')).default;
    diskUsage.testData('s1', 'complete', ROOT);
    mutMocks.deleteEntries.mockResolvedValueOnce({ requested: 1, deleted: 1, failed: [] });
    dialogs.open({ kind: 'delete', paths: [`${ROOT}\\a.txt`], permanent: false });
    render(Dialogs);
    await tick();
    const del = Array.from(document.querySelectorAll('[role="dialog"] button')).find(
      (b) => b.textContent === 'Delete',
    ) as HTMLElement;
    fireEvent.click(del);
    await vi.waitFor(() => expect(diskUsage.phase).toBe('stale'));
    expect(mutMocks.deleteEntries).toHaveBeenCalledWith([`${ROOT}\\a.txt`], false);
  });

  it('a failed delete (nothing removed) leaves the scan fresh', async () => {
    const Dialogs = (await import('../components/Dialogs.svelte')).default;
    diskUsage.testData('s1', 'complete', ROOT);
    mutMocks.deleteEntries.mockResolvedValueOnce({
      requested: 1,
      deleted: 0,
      failed: [{ path: `${ROOT}\\a.txt`, code: 'permissionDenied', message: 'locked' }],
    });
    dialogs.open({ kind: 'delete', paths: [`${ROOT}\\a.txt`], permanent: false });
    render(Dialogs);
    await tick();
    const del = Array.from(document.querySelectorAll('[role="dialog"] button')).find(
      (b) => b.textContent === 'Delete',
    ) as HTMLElement;
    fireEvent.click(del);
    await vi.waitFor(() =>
      expect(mutMocks.deleteEntries).toHaveBeenCalledWith([`${ROOT}\\a.txt`], false),
    );
    await tick();
    expect(diskUsage.phase).toBe('complete');
  });
});
