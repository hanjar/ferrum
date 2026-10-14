/**
 * Automation P0.4 (RQ-2, design §3.4) - SyncFlow x folder lease: `executeSync` takes the
 * cross-process lease BEFORE the re-validation compare, holds it through the run and the
 * baseline settle, and releases it on every way out (settled, cancelled, error, stale, blocked,
 * closed). A busy lease runs nothing and says why. All IPC is mocked; the fake lease backend
 * mirrors the real one (a held pair is busy for the next acquire).
 */
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { compareStore } from './compareStore.svelte';
import { dialogs } from './dialogStore.svelte';
import { registerPane, type PaneApi } from './panesRegistry.svelte';
import type { CompareDirsReport, DirDiffEntry, DirDiffState } from '../ipc/compare';
import type { OperationProgress } from '../ipc/types';
import type { LeaseAcquire } from '../ipc/syncLease';
import { SyncLeaseError } from '../ipc/syncLease';
import type { SyncLeaseApi } from '../syncService';

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
function report(entries: DirDiffEntry[], extra: Partial<CompareDirsReport> = {}) {
  return { leftRoot: L, rightRoot: R, entries, truncated: false, ...extra } as CompareDirsReport;
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
function job(state: OperationProgress['state']) {
  return {
    operationId: 'op_1',
    kind: 'copy',
    state,
    processedBytes: 0,
    processedFiles: 1,
  } as OperationProgress;
}
function deferred<T>() {
  let resolve!: (v: T) => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}
const tick = () => new Promise<void>((r) => setTimeout(r, 0));

const plan = report([diff('new.txt', 'leftOnly')]);
const verified = report([diff('new.txt', 'same')]);

let events: string[];
let held: string | null;
let leaseSeq: number;
let api: SyncLeaseApi & {
  acquire: ReturnType<typeof vi.fn>;
  release: ReturnType<typeof vi.fn>;
};

/** Mirrors the backend: a held pair answers busy to the next acquire. */
function makeApi(): typeof api {
  return {
    acquire: vi.fn(async (left: string, right: string): Promise<LeaseAcquire> => {
      events.push(`acquire:${left}|${right}`);
      if (held) return { status: 'busy', holder: { kind: 'manual' } };
      held = `lease-${++leaseSeq}`;
      return { leaseId: held };
    }),
    release: vi.fn(async (id: string) => {
      events.push('release');
      if (held === id) held = null;
    }),
  };
}

beforeEach(() => {
  events = [];
  held = null;
  leaseSeq = 0;
  api = makeApi();
  compareDirsMock.mockReset();
  enqueueCopyMock.mockReset();
  loadMock.mockReset();
  saveMock.mockReset();
  loadMock.mockResolvedValue({ status: 'firstRun', reason: 'none' });
  saveMock.mockImplementation(async () => {
    events.push('save');
  });
  compareStore.reset();
  compareStore.syncBaselineEnabled = () => true;
  compareStore.syncLeaseApi = api;
  compareStore.syncJobReader = () => job('completed');
  dialogs.close();
  registerPane(0, fakePane(L));
  registerPane(1, fakePane(R));
});

/** Preview with the leftOnly plan, then mark the start of the run in the event log. */
async function preview() {
  compareDirsMock.mockImplementation(async (_l: string, _r: string, byContent: boolean) => {
    events.push(`compare:${byContent ? 'content' : 'meta'}`);
    return byContent ? verified : plan;
  });
  enqueueCopyMock.mockImplementation(async () => {
    events.push('enqueue');
    return 'op_1';
  });
  await compareStore.openSync();
  events.length = 0;
}

describe('executeSync x folder lease', () => {
  it('acquires before the re-validation compare and the enqueue', async () => {
    await preview();
    await compareStore.executeSync();
    expect(events.slice(0, 3)).toEqual([`acquire:${L}|${R}`, 'compare:meta', 'enqueue']);
    expect(api.acquire).toHaveBeenCalledTimes(1);
    await compareStore.lastSettlement;
  });

  it('holds the lease through the baseline settle and releases after it', async () => {
    await preview();
    const gate = deferred<void>();
    saveMock.mockImplementation(async () => {
      events.push('save');
      await gate.promise;
    });
    await compareStore.executeSync();
    // The run is queued and executeSync has returned, but the settle is still saving.
    await tick();
    expect(compareStore.syncBusy).toBe(false);
    expect(api.release).not.toHaveBeenCalled();
    expect(held).not.toBeNull();
    gate.resolve();
    await compareStore.lastSettlement;
    expect(api.release).toHaveBeenCalledTimes(1);
    expect(events.indexOf('save')).toBeLessThan(events.lastIndexOf('release'));
    expect(events.at(-1)).toBe('release');
    expect(held).toBeNull();
    expect(compareStore.syncSettle).toEqual({ status: 'saved', entries: 1 });
  });

  it('releases even when the settle fails', async () => {
    await preview();
    saveMock.mockRejectedValue(new Error('io'));
    await compareStore.executeSync();
    const settled = await compareStore.lastSettlement;
    expect(settled?.status).toBe('saveFailed');
    expect(api.release).toHaveBeenCalledTimes(1);
  });

  it('releases right away when no baseline settle runs', async () => {
    compareStore.syncBaselineEnabled = () => false;
    await preview();
    await compareStore.executeSync();
    expect(compareStore.lastSettlement).toBeNull();
    expect(api.release).toHaveBeenCalledTimes(1);
    expect(held).toBeNull();
  });

  it('releases on cancel and enqueues nothing', async () => {
    await preview();
    const revalidate = deferred<CompareDirsReport>();
    compareDirsMock.mockImplementation(() => revalidate.promise);
    const run = compareStore.executeSync();
    await tick();
    expect(compareStore.syncBusy).toBe(true);
    compareStore.cancelSync();
    revalidate.resolve(plan);
    await run;
    expect(enqueueCopyMock).not.toHaveBeenCalled();
    expect(api.release).toHaveBeenCalledTimes(1);
    expect(held).toBeNull();
    expect(compareStore.errorText).toMatch(/cancelled/i);
    expect(compareStore.syncBusy).toBe(false);
  });

  it('cancel during the lease request itself still releases the lease', async () => {
    await preview();
    const gate = deferred<LeaseAcquire>();
    api.acquire.mockImplementationOnce(async () => {
      events.push('acquire');
      return gate.promise;
    });
    const run = compareStore.executeSync();
    await tick();
    compareStore.cancelSync();
    gate.resolve({ leaseId: 'late-lease' });
    await run;
    expect(enqueueCopyMock).not.toHaveBeenCalled();
    expect(api.release).toHaveBeenCalledWith('late-lease');
  });

  it('releases on error', async () => {
    await preview();
    compareDirsMock.mockRejectedValue(new Error('compare exploded'));
    await compareStore.executeSync();
    expect(compareStore.errorText).toBe('compare exploded');
    expect(enqueueCopyMock).not.toHaveBeenCalled();
    expect(api.release).toHaveBeenCalledTimes(1);
    expect(held).toBeNull();
  });

  it('releases when the plan turned out stale', async () => {
    await preview();
    compareDirsMock.mockResolvedValue(report([diff('new.txt', 'leftOnly'), diff('x', 'leftOnly')]));
    await compareStore.executeSync();
    expect(compareStore.syncStale).toBe(true);
    expect(enqueueCopyMock).not.toHaveBeenCalled();
    expect(api.release).toHaveBeenCalledTimes(1);
  });

  it('releases when the fresh compare is partial (blocked)', async () => {
    await preview();
    compareDirsMock.mockResolvedValue(report([diff('new.txt', 'leftOnly')], { truncated: true }));
    await compareStore.executeSync();
    expect(enqueueCopyMock).not.toHaveBeenCalled();
    expect(api.release).toHaveBeenCalledTimes(1);
  });

  it('closing the panel mid-run cancels it and releases the lease', async () => {
    await preview();
    const revalidate = deferred<CompareDirsReport>();
    compareDirsMock.mockImplementation(() => revalidate.promise);
    const run = compareStore.executeSync();
    await tick();
    compareStore.closeSync();
    revalidate.resolve(plan);
    await run;
    expect(enqueueCopyMock).not.toHaveBeenCalled();
    expect(api.release).toHaveBeenCalledTimes(1);
    expect(held).toBeNull();
  });

  it('closing the panel after the copies were queued keeps the lease until the settle ends', async () => {
    await preview();
    const gate = deferred<void>();
    saveMock.mockImplementation(async () => {
      events.push('save');
      await gate.promise;
    });
    await compareStore.executeSync();
    await tick();
    compareStore.closeSync();
    expect(api.release).not.toHaveBeenCalled();
    gate.resolve();
    await compareStore.lastSettlement;
    expect(api.release).toHaveBeenCalledTimes(1);
    expect(held).toBeNull();
  });
});

describe('executeSync x busy lease', () => {
  it('runs nothing and shows the busy message when the folders are in use', async () => {
    await preview();
    held = 'someone-else';
    api.acquire.mockResolvedValueOnce({ status: 'busy', holder: { kind: 'runner', runId: 7 } });
    await compareStore.executeSync();
    expect(compareDirsMock).toHaveBeenCalledTimes(1); // only the preview compare
    expect(enqueueCopyMock).not.toHaveBeenCalled();
    expect(saveMock).not.toHaveBeenCalled();
    expect(api.release).not.toHaveBeenCalled();
    expect(compareStore.syncLeaseBusy).toMatch(/in use by an automation/);
    expect(compareStore.syncBusy).toBe(false);
    // The previewed plan survives so the user can try again.
    expect(compareStore.syncPlan).not.toBeNull();
    expect(compareStore.syncDone).toBeNull();
  });

  it('the next preview clears the busy message and a retry succeeds once the lease is free', async () => {
    await preview();
    api.acquire.mockResolvedValueOnce({ status: 'busy', holder: { kind: 'manual' } });
    await compareStore.executeSync();
    expect(compareStore.syncLeaseBusy).toMatch(/another synchronization/);
    await compareStore.executeSync(); // retry: the lease is free now
    expect(compareStore.syncLeaseBusy).toBe('');
    expect(enqueueCopyMock).toHaveBeenCalledTimes(1);
    await compareStore.lastSettlement;
  });

  it('a lease error other than busy also runs nothing (fail closed)', async () => {
    await preview();
    api.acquire.mockRejectedValueOnce(new SyncLeaseError('store_unavailable', 'Store is down'));
    await compareStore.executeSync();
    expect(compareDirsMock).toHaveBeenCalledTimes(1);
    expect(enqueueCopyMock).not.toHaveBeenCalled();
    expect(compareStore.errorText).toMatch(/automation store is unavailable/);
    expect(compareStore.errorText).toMatch(/Nothing was changed/);
    expect(compareStore.syncLeaseBusy).toBe('');
    expect(compareStore.syncBusy).toBe(false);
  });

  it('the too-many-leases backend error has its own message, not "folders in use"', async () => {
    await preview();
    api.acquire.mockRejectedValueOnce(new SyncLeaseError('lease_busy', 'Too many'));
    await compareStore.executeSync();
    expect(compareStore.syncLeaseBusy).toMatch(/Too many synchronizations/);
    expect(compareStore.syncLeaseBusy).not.toMatch(/in use/);
    expect(enqueueCopyMock).not.toHaveBeenCalled();
  });

  it.each([
    ['store_read_only', /Ferrum was updated - restart Ferrum/],
    ['store_unavailable', /automation store is unavailable/],
    ['store_untrusted', /not trusted/],
    ['invalid_input', /path is not valid/],
    ['something_else', /could not be locked/],
  ])('a %s lease failure gets its own message', async (code, text) => {
    await preview();
    api.acquire.mockRejectedValueOnce(new SyncLeaseError(code, 'backend text'));
    await compareStore.executeSync();
    expect(compareStore.errorText).toMatch(text);
    expect(compareStore.errorText).toMatch(/Nothing was changed/);
    expect(enqueueCopyMock).not.toHaveBeenCalled();
  });

  it('a second run while the first one still settles is busy (its copies are still running)', async () => {
    await preview();
    const gate = deferred<void>();
    saveMock.mockImplementation(async () => {
      events.push('save');
      await gate.promise;
    });
    await compareStore.executeSync();
    await tick();
    // New preview + run on the same folders while the first settle waits.
    await compareStore.openSync();
    await compareStore.executeSync();
    expect(compareStore.syncLeaseBusy).not.toBe('');
    expect(enqueueCopyMock).toHaveBeenCalledTimes(1);
    gate.resolve();
    await compareStore.lastSettlement;
  });

  it('asks for the overwrite confirmation before any lease is taken', async () => {
    compareDirsMock.mockResolvedValue(report([diff('a.txt', 'newerLeft', 5)]));
    await compareStore.openSync();
    compareStore.setSyncDirection('leftToRight');
    events.length = 0;
    await compareStore.executeSync();
    expect(compareStore.syncConfirm).not.toBeNull();
    expect(api.acquire).not.toHaveBeenCalled();
  });
});

/** P2-2: the delete phase of Synchronize Dirs (confirmation dialog + the deletes) is part of the
 *  run, so the lease outlives the baseline settle until that phase is over. */
describe('executeSync x delete phase', () => {
  const withDelete = report([diff('new.txt', 'leftOnly'), diff('gone.txt', 'rightOnly')]);

  async function previewWithDelete() {
    compareDirsMock.mockImplementation(async (_l: string, _r: string, byContent: boolean) => {
      events.push(`compare:${byContent ? 'content' : 'meta'}`);
      return byContent
        ? report([diff('new.txt', 'same'), diff('gone.txt', 'rightOnly')])
        : withDelete;
    });
    enqueueCopyMock.mockImplementation(async () => {
      events.push('enqueue');
      return 'op_1';
    });
    await compareStore.openSync();
    compareStore.setSyncDirection('leftToRight');
    compareStore.setSyncDeleteExtras(true);
    events.length = 0;
  }

  it('keeps the lease after the settle while the delete dialog is open, releases when it closes', async () => {
    await previewWithDelete();
    await compareStore.executeSync();
    expect(dialogs.current?.kind).toBe('delete');
    await compareStore.lastSettlement;
    await tick();
    expect(compareStore.syncSettle).not.toBeNull();
    expect(api.release).not.toHaveBeenCalled();
    expect(held).not.toBeNull();
    dialogs.close(); // cancelled (or confirmed and done)
    await vi.waitFor(() => expect(api.release).toHaveBeenCalledTimes(1));
    expect(held).toBeNull();
  });

  it('waits for the settle too when the dialog ends first', async () => {
    await previewWithDelete();
    const gate = deferred<void>();
    saveMock.mockImplementation(async () => {
      events.push('save');
      await gate.promise;
    });
    await compareStore.executeSync();
    dialogs.close();
    await tick();
    expect(api.release).not.toHaveBeenCalled();
    gate.resolve();
    await vi.waitFor(() => expect(api.release).toHaveBeenCalledTimes(1));
  });

  it('a replaced delete dialog and a reset both end the phase', async () => {
    await previewWithDelete();
    await compareStore.executeSync();
    await compareStore.lastSettlement;
    dialogs.open({ kind: 'delete', paths: ['C:/other'], permanent: false });
    await vi.waitFor(() => expect(api.release).toHaveBeenCalledTimes(1));
    dialogs.close();
    expect(api.release).toHaveBeenCalledTimes(1); // still exactly once

    api.release.mockClear();
    await previewWithDelete();
    await compareStore.executeSync();
    await compareStore.lastSettlement;
    dialogs.reset();
    await vi.waitFor(() => expect(api.release).toHaveBeenCalledTimes(1));
  });

  it('without a baseline settle the lease ends with the delete phase', async () => {
    compareStore.syncBaselineEnabled = () => false;
    await previewWithDelete();
    await compareStore.executeSync();
    await tick();
    expect(api.release).not.toHaveBeenCalled();
    dialogs.close();
    await vi.waitFor(() => expect(api.release).toHaveBeenCalledTimes(1));
  });

  it('a dialog that cannot be opened does not leak the lease', async () => {
    await previewWithDelete();
    const open = vi.spyOn(dialogs, 'open').mockImplementation(() => {
      throw new Error('no dialog');
    });
    await compareStore.executeSync();
    await compareStore.lastSettlement;
    open.mockRestore();
    await vi.waitFor(() => expect(api.release).toHaveBeenCalledTimes(1));
  });
});
