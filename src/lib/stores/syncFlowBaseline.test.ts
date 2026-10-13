/**
 * FER-414 — SyncFlow x baseline: the stored baseline is loaded when the preview opens and is
 * saved only after the queue jobs finished and a fresh compare verified them.
 * All IPC (compare, enqueue, baseline store) is MOCKED here; real-filesystem behaviour is in
 * `src/lib/syncBaselineFlow.test.ts` (fs-backed double) and the Rust tests.
 */
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { compareStore } from './compareStore.svelte';
import { dialogs } from './dialogStore.svelte';
import { registerPane, type PaneApi } from './panesRegistry.svelte';
import type { CompareDirsReport, DirDiffEntry, DirDiffState } from '../ipc/compare';
import type { OperationProgress } from '../ipc/types';
import type { SyncBaseline } from '../syncBaseline';

const compareDirsMock = vi.hoisted(() => vi.fn());
const enqueueCopyMock = vi.hoisted(() => vi.fn());
const loadMock = vi.hoisted(() => vi.fn());
const saveMock = vi.hoisted(() => vi.fn());

vi.mock('../ipc/compare', () => ({
  validateSyncRoots: vi.fn(async () => {}),
  compareDirs: compareDirsMock,
  compareFiles: vi.fn(),
  cancelCompare: vi.fn(),
}));
vi.mock('../ipc/mutations', () => ({
  enqueueCopy: enqueueCopyMock,
  enqueueMove: vi.fn(),
  createEntry: vi.fn(),
  renameEntry: vi.fn(),
  deleteEntries: vi.fn(),
  planCopy: vi.fn(),
}));
vi.mock('../ipc/syncBaseline', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../ipc/syncBaseline')>();
  return { ...actual, syncBaselineLoad: loadMock, syncBaselineSave: saveMock };
});

const L = 'C:\\L';
const R = 'D:\\R';

function diff(name: string, state: DirDiffState, ms = 1): DirDiffEntry {
  return { name, state, leftSize: 1, leftModifiedMs: ms, rightSize: 1, rightModifiedMs: ms };
}
function report(entries: DirDiffEntry[]): CompareDirsReport {
  return { leftRoot: L, rightRoot: R, entries, truncated: false };
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
function job(state: OperationProgress['state'], extra: Partial<OperationProgress> = {}) {
  return {
    operationId: 'op_1',
    kind: 'copy',
    state,
    processedBytes: 0,
    processedFiles: 1,
    ...extra,
  } as OperationProgress;
}

const baseline: SyncBaseline = {
  version: 1,
  leftRoot: L,
  rightRoot: R,
  entries: { 'a.txt': { left: { size: 1, modifiedMs: 1 }, right: { size: 1, modifiedMs: 1 } } },
};

beforeEach(() => {
  compareDirsMock.mockReset();
  enqueueCopyMock.mockReset();
  loadMock.mockReset();
  saveMock.mockReset();
  loadMock.mockResolvedValue({ status: 'firstRun', reason: 'none' });
  saveMock.mockResolvedValue(undefined);
  compareStore.reset();
  compareStore.syncBaselineEnabled = () => true;
  dialogs.close();
  registerPane(0, fakePane(L));
  registerPane(1, fakePane(R));
});

// The loaded baseline needs IPC-shaped `isInTauri` truthiness, which the mocked load bypasses.
describe('openSync loads the baseline', () => {
  it('uses a trusted stored baseline for the two-way plan', async () => {
    loadMock.mockResolvedValue({ status: 'ok', baseline });
    // a.txt: left changed vs baseline, right unchanged => update right, not a conflict
    compareDirsMock.mockResolvedValue(
      report([
        {
          name: 'a.txt',
          state: 'newerLeft',
          leftSize: 2,
          leftModifiedMs: 60_000,
          rightSize: 1,
          rightModifiedMs: 1,
        },
      ]),
    );
    await compareStore.openSync();
    expect(loadMock).toHaveBeenCalledWith({ leftRoot: L, rightRoot: R });
    expect(compareStore.syncBaselineStatus).toBe('trusted');
    expect(compareStore.syncPlan?.[0].action).toBe('updateRight');
  });

  it('first-run reasons are surfaced and the plan stays conservative', async () => {
    loadMock.mockResolvedValue({ status: 'firstRun', reason: 'corrupt' });
    compareDirsMock.mockResolvedValue(report([diff('a.txt', 'newerLeft', 99_000)]));
    await compareStore.openSync();
    expect(compareStore.syncBaselineStatus).toBe('corrupt');
    expect(compareStore.syncPlan?.[0]).toMatchObject({ action: 'conflict', reason: 'noBaseline' });
  });

  it('a failing store never blocks the preview (first-run)', async () => {
    loadMock.mockRejectedValue(new Error('boom'));
    compareDirsMock.mockResolvedValue(report([diff('a.txt', 'leftOnly')]));
    await compareStore.openSync();
    expect(compareStore.syncBaselineStatus).toBe('loadFailed');
    expect(compareStore.syncPlan?.[0].action).toBe('copyToRight');
  });
});

describe('executeSync settles the baseline after the jobs', () => {
  const plan = report([diff('new.txt', 'leftOnly')]);
  const verified = report([diff('new.txt', 'same')]);

  async function run(jobResult: OperationProgress) {
    compareDirsMock.mockResolvedValue(plan);
    await compareStore.openSync();
    compareDirsMock.mockResolvedValue(plan); // run re-validation
    enqueueCopyMock.mockResolvedValue('op_1');
    compareStore.syncJobReader = () => jobResult;
    // settle compare (content mode) sees the verified-identical result
    compareDirsMock.mockImplementation(async (_l: string, _r: string, byContent: boolean) =>
      byContent ? verified : plan,
    );
    await compareStore.executeSync();
    return compareStore.lastSettlement;
  }

  it('saves the reconciled baseline once the job completed and the compare verified it', async () => {
    const settled = await run(job('completed'));
    expect(settled).toEqual({ status: 'saved', entries: 1 });
    expect(saveMock).toHaveBeenCalledTimes(1);
    const [pair, saved] = saveMock.mock.calls[0] as [unknown, SyncBaseline];
    expect(pair).toEqual({ leftRoot: L, rightRoot: R });
    expect(Object.keys(saved.entries)).toEqual(['new.txt']);
    expect(compareStore.syncSettle).toEqual({ status: 'saved', entries: 1 });
  });

  it('does not save for a failed job', async () => {
    const settled = await run(job('failed', { error: 'disk full' }));
    expect(settled?.status).toBe('unchanged');
    expect(saveMock).not.toHaveBeenCalled();
  });

  it('does not save for a cancelled job', async () => {
    await run(job('cancelled'));
    expect(saveMock).not.toHaveBeenCalled();
  });

  it('outside the desktop shell no settlement is started', async () => {
    compareStore.syncBaselineEnabled = () => false;
    compareDirsMock.mockResolvedValue(plan);
    await compareStore.openSync();
    compareDirsMock.mockResolvedValue(plan);
    enqueueCopyMock.mockResolvedValue('op_1');
    await compareStore.executeSync();
    expect(compareStore.lastSettlement).toBeNull();
    expect(saveMock).not.toHaveBeenCalled();
  });
});
