/**
 * F2-19 — sync execution flows: mandatory dry-run preview, stale-plan
 * blocking, queued copies, and delete confirmation. compareDirs and
 * enqueueCopy are mocked at the IPC boundary.
 */
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { compareStore } from './compareStore.svelte';
import { dialogs } from './dialogStore.svelte';
import { registerPane, type PaneApi } from './panesRegistry.svelte';
import type { CompareDirsReport, DirDiffEntry, DirDiffState } from '../ipc/compare';

vi.mock('../ipc/compare', () => ({
  validateSyncRoots: vi.fn(async () => {}),
  compareDirs: (...args: unknown[]) => compareDirsMock(...args),
  compareFiles: vi.fn(),
  cancelCompare: (...args: unknown[]) => cancelCompareMock(...args),
}));
vi.mock('../ipc/mutations', () => ({
  enqueueCopy: (...args: unknown[]) => enqueueCopyMock(...args),
  enqueueMove: vi.fn(),
  createEntry: vi.fn(),
  renameEntry: vi.fn(),
  deleteEntries: vi.fn(),
  planCopy: vi.fn(),
}));

const compareDirsMock = vi.fn();
const enqueueCopyMock = vi.fn();
const cancelCompareMock = vi.fn();

function diff(name: string, state: DirDiffState): DirDiffEntry {
  return { name, state, leftSize: 1, leftModifiedMs: 1, rightSize: 1, rightModifiedMs: 1 };
}

function report(entries: DirDiffEntry[]): CompareDirsReport {
  return { leftRoot: 'C:\\L', rightRoot: 'D:\\R', entries, truncated: false };
}

function fakePane(path: string): PaneApi {
  return {
    path,
    selectedPaths: () => [],
    cursorEntry: () => undefined,
    refresh: () => {},
    newTab: () => {},
    closeCurrentTab: () => {},
    goUp: () => {},
    openCursorEntry: () => {},
  };
}

beforeEach(() => {
  compareDirsMock.mockReset();
  enqueueCopyMock.mockReset();
  cancelCompareMock.mockReset();
  compareStore.reset();
  dialogs.close();
  registerPane(0, fakePane('C:\\L'));
  registerPane(1, fakePane('D:\\R'));
});

describe('openSync (mandatory dry-run)', () => {
  it('compares live folders first and builds the preview plan', async () => {
    compareDirsMock.mockResolvedValue(report([diff('a.txt', 'leftOnly'), diff('b.txt', 'same')]));
    await compareStore.openSync();
    expect(compareDirsMock).toHaveBeenCalledWith('C:\\L', 'D:\\R', false, {
      recursive: true,
      opId: expect.any(String),
    });
    expect(compareStore.syncOpen).toBe(true);
    expect(compareStore.syncPlan).toHaveLength(2);
    expect(compareStore.syncSummary?.toRight).toBe(1);
    expect(compareStore.syncSummary?.skipped).toBe(1);
  });

  it('direction changes regenerate the plan from the same snapshot', async () => {
    compareDirsMock.mockResolvedValue(
      report([diff('a.txt', 'newerLeft'), diff('b.txt', 'newerRight')]),
    );
    await compareStore.openSync();
    compareStore.setSyncDirection('leftToRight');
    expect(compareStore.syncPlan?.map((p) => p.action)).toEqual(['updateRight', 'skip']);
    compareStore.setSyncDirection('both');
    // AI-10: no timestamp winner in 'both' without a baseline
    expect(compareStore.syncPlan?.map((p) => p.action)).toEqual(['conflict', 'conflict']);
    expect(compareDirsMock).toHaveBeenCalledTimes(1); // no extra compares
  });

  it('explains when a pane is missing', async () => {
    registerPane(1, fakePane('D:\\R'))();
    await compareStore.openSync();
    expect(compareStore.syncPlan).toBeNull();
    expect(compareStore.errorText).toMatch(/both panes/i);
  });
});

describe('executeSync', () => {
  const plan = report([diff('new.txt', 'leftOnly'), diff('old.txt', 'rightOnly')]);

  it('queues copies per side through the existing engine', async () => {
    compareDirsMock.mockResolvedValue(plan); // preview…
    await compareStore.openSync();
    compareDirsMock.mockResolvedValue(plan); // …and re-validation sees the same facts
    enqueueCopyMock.mockResolvedValue('op_1');
    await compareStore.executeSync();
    expect(enqueueCopyMock).toHaveBeenCalledTimes(2);
    expect(enqueueCopyMock).toHaveBeenNthCalledWith(1, ['C:\\L\\new.txt'], 'D:\\R', 'overwrite');
    expect(enqueueCopyMock).toHaveBeenNthCalledWith(2, ['D:\\R\\old.txt'], 'C:\\L', 'overwrite');
    expect(compareStore.syncDone).toMatchObject({ toRight: 1, toLeft: 1, deletes: 0, failed: 0 });
    // the plan is consumed — another run requires a fresh preview
    expect(compareStore.syncPlan).toBeNull();
  });

  it('blocks execution when sources changed since the preview (stale plan)', async () => {
    compareDirsMock.mockResolvedValue(plan); // preview
    await compareStore.openSync();
    // reality moved on: an extra file appeared on the right
    compareDirsMock.mockResolvedValue(
      report([
        diff('new.txt', 'leftOnly'),
        diff('old.txt', 'newerRight'),
        diff('x.txt', 'rightOnly'),
      ]),
    );
    await compareStore.executeSync();
    expect(enqueueCopyMock).not.toHaveBeenCalled();
    expect(compareStore.syncStale).toBe(true);
    expect(compareStore.errorText).toMatch(/changed since the preview/i);
    // the plan was refreshed from the fresh compare for another review
    expect(compareStore.syncPlan).toHaveLength(3);
  });

  it('routes delete actions through the explicit delete confirmation dialog', async () => {
    compareDirsMock.mockResolvedValue(report([diff('extra.txt', 'rightOnly')]));
    await compareStore.openSync();
    compareStore.setSyncDirection('leftToRight');
    compareStore.setSyncDeleteExtras(true);
    expect(compareStore.syncPlan?.[0].action).toBe('deleteRight');
    compareDirsMock.mockResolvedValue(report([diff('extra.txt', 'rightOnly')]));
    await compareStore.executeSync();
    expect(enqueueCopyMock).not.toHaveBeenCalled(); // nothing to copy
    expect(dialogs.current).toMatchObject({ kind: 'delete', paths: ['D:\\R\\extra.txt'] });
    expect(compareStore.syncDone?.deletes).toBe(1);
  });

  it('surfaces engine errors without pretending success', async () => {
    compareDirsMock.mockResolvedValue(plan);
    await compareStore.openSync();
    compareDirsMock.mockResolvedValue(plan);
    enqueueCopyMock.mockRejectedValue(new Error('Disk full'));
    await compareStore.executeSync();
    expect(compareStore.errorText).toMatch(/Disk full/);
    expect(compareStore.syncDone).toMatchObject({ toRight: 0, toLeft: 0, failed: 2 });
    expect(compareStore.syncLog.every((r) => r.status === 'failed')).toBe(true);
  });

  it('does nothing without a plan', async () => {
    await compareStore.executeSync();
    expect(enqueueCopyMock).not.toHaveBeenCalled();
  });
});

describe('closeSync', () => {
  it('clears plan state for the next session', async () => {
    compareDirsMock.mockResolvedValue(report([diff('a.txt', 'leftOnly')]));
    await compareStore.openSync();
    compareStore.closeSync();
    expect(compareStore.syncOpen).toBe(false);
    expect(compareStore.syncPlan).toBeNull();
    expect(compareStore.syncStale).toBe(false);
    expect(compareStore.syncDone).toBeNull();
  });
});

function full(name: string, state: DirDiffState, over: Partial<DirDiffEntry> = {}): DirDiffEntry {
  return { ...diff(name, state), ...over };
}

async function preview(entries: DirDiffEntry[], extra: Partial<CompareDirsReport> = {}) {
  compareDirsMock.mockResolvedValue({ ...report(entries), ...extra });
  await compareStore.openSync();
}
/** Re-validation sees the same facts as the preview. */
function sameOnRevalidate(entries: DirDiffEntry[], extra: Partial<CompareDirsReport> = {}) {
  compareDirsMock.mockResolvedValue({ ...report(entries), ...extra });
}

describe('FER-113 dry-run: no write', () => {
  it('openSync/regenerate/decisions never enqueue or delete', async () => {
    const entries = [full('a', 'leftOnly'), full('b', 'rightOnly'), full('c', 'contentDifferent')];
    await preview(entries);
    compareStore.setSyncDirection('leftToRight');
    compareStore.setSyncDeleteExtras(true);
    compareStore.decideAllConflicts('skip');
    expect(compareStore.syncPlan?.length).toBe(3);
    expect(enqueueCopyMock).not.toHaveBeenCalled();
    expect(dialogs.current).toBeNull();
  });
});

describe('FER-110 delete-extras, recursive and folders', () => {
  it('rightToLeft + deleteExtras plans deleteLeft and routes it to the delete dialog', async () => {
    const entries = [full('extra.txt', 'leftOnly'), full('r.txt', 'rightOnly')];
    await preview(entries);
    compareStore.setSyncDirection('rightToLeft');
    expect(compareStore.syncPlan?.map((p) => p.action)).toEqual(['skip', 'copyToLeft']);
    compareStore.setSyncDeleteExtras(true);
    expect(compareStore.syncPlan?.map((p) => p.action)).toEqual(['deleteLeft', 'copyToLeft']);
    sameOnRevalidate(entries);
    enqueueCopyMock.mockResolvedValue('op_1');
    await compareStore.executeSync();
    expect(enqueueCopyMock).toHaveBeenCalledWith(['D:\\R\\r.txt'], 'C:\\L', 'overwrite');
    expect(dialogs.current).toMatchObject({ kind: 'delete', paths: ['C:\\L\\extra.txt'] });
  });

  it('copies nested entries into their own destination folder, not the root', async () => {
    const entries = [
      full('sub/deep/a.txt', 'leftOnly'),
      full('sub/deep/b.txt', 'leftOnly'),
      full('top.txt', 'leftOnly'),
    ];
    await preview(entries, { recursive: true });
    sameOnRevalidate(entries, { recursive: true });
    enqueueCopyMock.mockResolvedValue('op_n');
    await compareStore.executeSync();
    const calls = enqueueCopyMock.mock.calls.map((c) => [c[0], c[1]]);
    expect(calls).toContainEqual([
      ['C:\\L\\sub\\deep\\a.txt', 'C:\\L\\sub\\deep\\b.txt'],
      'D:\\R\\sub\\deep',
    ]);
    expect(calls).toContainEqual([['C:\\L\\top.txt'], 'D:\\R']);
  });

  it('a one-sided folder is exactly one copy action (not expanded)', async () => {
    const entries = [full('photos', 'leftOnly', { isDir: true })];
    await preview(entries, { recursive: true });
    expect(compareStore.syncPlan).toHaveLength(1);
    expect(compareStore.syncPlan?.[0]).toMatchObject({ action: 'copyToRight', isDir: true });
    sameOnRevalidate(entries, { recursive: true });
    enqueueCopyMock.mockResolvedValue('op_d');
    await compareStore.executeSync();
    expect(enqueueCopyMock).toHaveBeenCalledTimes(1);
    expect(enqueueCopyMock).toHaveBeenCalledWith(['C:\\L\\photos'], 'D:\\R', 'overwrite');
  });

  it('a folder/file mismatch is a conflict that only allows skip', async () => {
    await preview([full('x', 'newerLeft', { isDir: true })]);
    expect(compareStore.syncPlan?.[0]).toMatchObject({
      action: 'conflict',
      reason: 'typeMismatch',
    });
    compareStore.decideConflict('x', 'keepLeft');
    expect(compareStore.syncPlan?.[0].action).toBe('conflict');
    compareStore.decideConflict('x', 'skip');
    expect(compareStore.syncPlan?.[0]).toMatchObject({ action: 'skip', decision: 'skip' });
  });
});

describe('FER-110 blocked plans (truncated / cancelled / errors)', () => {
  it.each([
    ['truncated', { truncated: true }, /truncated/i],
    ['cancelled', { cancelled: true }, /cancelled/i],
    [
      'read errors (symlink/permission)',
      { errors: [{ name: 'locked/secret.db', message: 'Access is denied' }] },
      /locked\/secret\.db: Access is denied/,
    ],
  ])('%s -> no plan, reason shown, nothing runs', async (_n, extra, pattern) => {
    await preview([full('a', 'leftOnly')], extra as Partial<CompareDirsReport>);
    expect(compareStore.syncPlan).toBeNull();
    expect(compareStore.syncBlocked).toMatch(pattern);
    await compareStore.executeSync();
    expect(enqueueCopyMock).not.toHaveBeenCalled();
  });

  it('a partial snapshot surfacing only at re-validation also blocks the run', async () => {
    const entries = [full('a', 'leftOnly')];
    await preview(entries);
    sameOnRevalidate(entries, { errors: [{ name: 'a', message: 'vanished' }] });
    await compareStore.executeSync();
    expect(enqueueCopyMock).not.toHaveBeenCalled();
    expect(compareStore.errorText).toMatch(/vanished/);
  });
});

describe('FER-111 overwrite confirmation', () => {
  const entries = [full('new.txt', 'leftOnly'), full('old.txt', 'newerLeft')];

  it('does not enqueue until the overwrite summary is confirmed', async () => {
    await preview(entries);
    compareStore.setSyncDirection('leftToRight');
    sameOnRevalidate(entries);
    enqueueCopyMock.mockResolvedValue('op_1');
    await compareStore.executeSync();
    expect(compareStore.syncConfirm).toEqual({ overwrites: 1, names: ['old.txt'] });
    expect(enqueueCopyMock).not.toHaveBeenCalled();

    await compareStore.executeSync({ confirmOverwrite: true });
    expect(compareStore.syncConfirm).toBeNull();
    expect(enqueueCopyMock).toHaveBeenCalledWith(
      ['C:\\L\\new.txt', 'C:\\L\\old.txt'],
      'D:\\R',
      'overwrite',
    );
  });

  it('a changed direction discards a pending confirmation', async () => {
    await preview(entries);
    compareStore.setSyncDirection('leftToRight');
    await compareStore.executeSync();
    expect(compareStore.syncConfirm).not.toBeNull();
    compareStore.setSyncDirection('both');
    expect(compareStore.syncConfirm).toBeNull();
  });
});

describe('FER-111 conflict decisions', () => {
  const entries = [
    full('c1', 'contentDifferent'),
    full('c2', 'sizeDifferent'),
    full('c3', 'contentDifferent'),
    full('plain', 'leftOnly'),
  ];

  it('keep left / keep right per item; undecided conflicts are visible and not executed', async () => {
    await preview(entries);
    compareStore.decideConflict('c1', 'keepLeft');
    compareStore.decideConflict('c2', 'keepRight');
    // c3 stays undecided
    expect(compareStore.syncPlan?.map((p) => p.action)).toEqual([
      'updateRight',
      'updateLeft',
      'conflict',
      'copyToRight',
    ]);
    expect(compareStore.syncSummary).toMatchObject({ conflicts: 1, overwrites: 2 });
    sameOnRevalidate(entries);
    enqueueCopyMock.mockResolvedValue('op');
    await compareStore.executeSync({ confirmOverwrite: true });
    const all = enqueueCopyMock.mock.calls.flatMap((c) => c[0] as string[]);
    expect(all).toContain('C:\\L\\c1');
    expect(all).toContain('D:\\R\\c2');
    expect(all).not.toContain('C:\\L\\c3');
    expect(all).not.toContain('D:\\R\\c3');
    const c3 = compareStore.syncLog.find((r) => r.name === 'c3');
    expect(c3).toMatchObject({ status: 'skipped', reason: expect.stringMatching(/unresolved/) });
  });

  it('apply-to-all applies one decision to every conflict', async () => {
    await preview(entries);
    compareStore.decideAllConflicts('keepRight');
    expect(compareStore.syncPlan?.map((p) => p.action)).toEqual([
      'updateLeft',
      'updateLeft',
      'updateLeft',
      'copyToRight',
    ]);
    compareStore.decideAllConflicts('skip');
    expect(compareStore.syncSummary).toMatchObject({ conflicts: 0, userSkipped: 3 });
    compareStore.decideConflict('c1', null);
    expect(compareStore.syncPlan?.[0].action).toBe('conflict');
  });

  it('skipped-by-user items appear in the run log as skipped', async () => {
    await preview(entries);
    compareStore.decideAllConflicts('skip');
    sameOnRevalidate(entries);
    enqueueCopyMock.mockResolvedValue('op');
    await compareStore.executeSync();
    expect(enqueueCopyMock).toHaveBeenCalledTimes(1); // only 'plain'
    expect(compareStore.syncLog.filter((r) => r.reason === 'skipped by user')).toHaveLength(3);
  });
});

describe('FER-111/113 two-way and partial failure', () => {
  it('two directions queue in both directions with a per-item log', async () => {
    const entries = [full('l', 'leftOnly'), full('r', 'rightOnly')];
    await preview(entries);
    sameOnRevalidate(entries);
    enqueueCopyMock.mockResolvedValueOnce('op_a').mockResolvedValueOnce('op_b');
    await compareStore.executeSync();
    expect(compareStore.syncLog).toEqual([
      { name: 'l', action: 'copyToRight', status: 'queued', jobId: 'op_a' },
      { name: 'r', action: 'copyToLeft', status: 'queued', jobId: 'op_b' },
    ]);
  });

  it('reports BOTH batches when the second fails after the first was queued', async () => {
    const entries = [full('l', 'leftOnly'), full('r', 'rightOnly')];
    await preview(entries);
    sameOnRevalidate(entries);
    enqueueCopyMock.mockResolvedValueOnce('op_a').mockRejectedValueOnce(new Error('Disk full'));
    await compareStore.executeSync();
    expect(compareStore.syncDone).toMatchObject({ toRight: 1, toLeft: 0, failed: 1 });
    expect(compareStore.syncLog).toEqual([
      { name: 'l', action: 'copyToRight', status: 'queued', jobId: 'op_a' },
      { name: 'r', action: 'copyToLeft', status: 'failed', reason: 'Disk full' },
    ]);
    expect(compareStore.errorText).toMatch(/Disk full/);
  });
});

describe('FER-112 content mode and delete ordering', () => {
  it('content mode compares by content for the plan AND the stale re-validation', async () => {
    compareDirsMock.mockResolvedValue(report([full('a', 'contentDifferent')]));
    await compareStore.setSyncByContent(true);
    expect(compareDirsMock).toHaveBeenNthCalledWith(1, 'C:\\L', 'D:\\R', true, expect.anything());
    compareStore.setSyncDirection('leftToRight');
    enqueueCopyMock.mockResolvedValue('op');
    await compareStore.executeSync({ confirmOverwrite: true });
    expect(compareDirsMock).toHaveBeenNthCalledWith(2, 'C:\\L', 'D:\\R', true, expect.anything());
    expect(enqueueCopyMock).toHaveBeenCalledTimes(1);
  });

  it('content mode: identical size+mtime but a changed verdict is stale', async () => {
    compareDirsMock.mockResolvedValue(report([full('a', 'contentDifferent')]));
    await compareStore.setSyncByContent(true);
    compareStore.setSyncDirection('leftToRight');
    // metadata identical, content now equal
    compareDirsMock.mockResolvedValue(report([full('a', 'same')]));
    await compareStore.executeSync({ confirmOverwrite: true });
    expect(enqueueCopyMock).not.toHaveBeenCalled();
    expect(compareStore.syncStale).toBe(true);
  });

  it('metadata mode: verdict-only difference is not treated as stale (facts are mtime+size)', async () => {
    await preview([full('a', 'leftOnly')]);
    compareDirsMock.mockResolvedValue(report([full('a', 'newerLeft')]));
    enqueueCopyMock.mockResolvedValue('op');
    await compareStore.executeSync();
    expect(compareStore.syncStale).toBe(false);
  });

  it('deletes are skipped and reported when a preceding copy failed', async () => {
    const entries = [full('c', 'leftOnly'), full('x', 'rightOnly')];
    await preview(entries);
    compareStore.setSyncDirection('leftToRight');
    compareStore.setSyncDeleteExtras(true);
    sameOnRevalidate(entries);
    enqueueCopyMock.mockRejectedValue(new Error('locked'));
    await compareStore.executeSync();
    expect(dialogs.current).toBeNull();
    expect(compareStore.syncLog).toContainEqual({
      name: 'x',
      action: 'deleteRight',
      status: 'skipped',
      reason: expect.stringMatching(/preceding copy failed/),
    });
    expect(compareStore.syncDone?.deletes).toBe(0);
  });
});

describe('FER-113 cancel', () => {
  it('cancel between batches: no further actions, no deletes, clear result', async () => {
    const entries = [full('l', 'leftOnly'), full('r', 'rightOnly'), full('x', 'rightOnly')];
    await preview(entries);
    sameOnRevalidate(entries);
    enqueueCopyMock.mockImplementationOnce(async () => {
      compareStore.cancelSync(); // user hits Cancel while the first batch is in flight
      return 'op_a';
    });
    await compareStore.executeSync();
    expect(enqueueCopyMock).toHaveBeenCalledTimes(1);
    expect(dialogs.current).toBeNull();
    expect(compareStore.syncDone).toMatchObject({ toRight: 1, toLeft: 0, cancelled: true });
    const skipped = compareStore.syncLog.filter((r) => r.status === 'skipped');
    expect(skipped.map((r) => r.name)).toEqual(['r', 'x']);
    expect(skipped.every((r) => /cancelled/.test(r.reason ?? ''))).toBe(true);
    expect(compareStore.errorText).toMatch(/cancelled/i);
    expect(cancelCompareMock).toHaveBeenCalledTimes(1);
  });

  it('cancel during re-validation compare enqueues nothing', async () => {
    const entries = [full('l', 'leftOnly')];
    await preview(entries);
    compareDirsMock.mockImplementationOnce(async () => {
      compareStore.cancelSync();
      return { ...report(entries), cancelled: true };
    });
    await compareStore.executeSync();
    expect(enqueueCopyMock).not.toHaveBeenCalled();
    expect(compareStore.errorText).toMatch(/cancelled/i);
    expect(compareStore.syncDone).toBeNull();
  });
});
