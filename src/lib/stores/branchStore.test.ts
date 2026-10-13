import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { asBranchEvent, relativePath, type BranchEvent } from '../ipc/branchView';
import { FsError } from '../ipc/types';
import type { FileEntry } from '../ipc/types';
import { APPLY_INTERVAL_MS, BranchStore, type BranchApi } from './branchStore.svelte';

function entry(path: string, name: string, size = 1): FileEntry {
  return {
    name,
    path,
    isDir: false,
    isSymlink: false,
    size,
    modifiedMs: 1790512000000,
    hidden: false,
  };
}

/** Fake backend: records calls and lets the test push `branch://progress` events. */
function fakeApi() {
  let handler: ((e: BranchEvent) => void) | null = null;
  const api = {
    start: vi.fn<(id: string, root: string) => Promise<void>>(async () => {}),
    cancel: vi.fn<(id: string) => Promise<boolean>>(async () => true),
    listen: vi.fn(async (h: (e: BranchEvent) => void) => {
      handler = h;
      return () => {};
    }),
  };
  const ids = () => api.start.mock.calls.map((c) => c[0]);
  const lastId = () => ids()[ids().length - 1];
  const emit = (over: Partial<BranchEvent> & { id?: string }) =>
    handler?.({ id: lastId(), entries: [], failed: [], done: false, ...over });
  return { api: api as unknown as BranchApi & typeof api, emit, ids, lastId };
}

let store: BranchStore;
let fx: ReturnType<typeof fakeApi>;

beforeEach(() => {
  vi.useFakeTimers();
  fx = fakeApi();
  store = new BranchStore(fx.api);
});

afterEach(() => {
  store.exit(0);
  store.exit(1);
  vi.useRealTimers();
});

describe('relativePath', () => {
  it('strips the branch root and keeps the subpath unambiguous', () => {
    expect(relativePath('C:\\data\\2026\\q1.csv', 'C:\\data')).toBe('2026\\q1.csv');
    expect(relativePath('C:\\data\\dup.txt', 'C:\\data')).toBe('dup.txt');
    expect(relativePath('D:\\elsewhere\\x', 'C:\\data')).toBe('D:\\elsewhere\\x');
    expect(relativePath('C:\\data', 'C:\\data')).toBe('C:\\data'); // root itself
  });

  it('handles posix separators and trailing slashes in the root', () => {
    expect(relativePath('/srv/files/a/b.txt', '/srv/files/')).toBe('a/b.txt');
  });
});

describe('asBranchEvent', () => {
  it('accepts a batch and a done event, rejects garbage', () => {
    const batch = asBranchEvent({
      id: 'b1',
      entries: [entry('C:\\d\\a', 'a')],
      done: false,
    });
    expect(batch.entries).toHaveLength(1);
    expect(batch.failed).toEqual([]);
    const done = asBranchEvent({
      id: 'b1',
      entries: [],
      done: true,
      summary: { total: 1, truncated: true, cancelled: false, failedCount: 2, durationMs: 5 },
    });
    expect(done.summary?.truncated).toBe(true);
    expect(done.summary?.failedCount).toBe(2);
    expect(() => asBranchEvent({ id: 1 })).toThrow();
    expect(() => asBranchEvent(null)).toThrow();
  });
});

describe('branchStore (streaming)', () => {
  it('toggles on with the pane root, shows the first batch at once, stays pane-local', async () => {
    await store.toggle(0, 'C:\\data');
    expect(fx.api.start).toHaveBeenCalledWith(expect.stringMatching(/^branch-0-/), 'C:\\data');
    expect(store.activeFor(0)).toBe(true);
    expect(store.activeFor(1)).toBe(false); // panes are independent
    expect(store.isSettling(0)).toBe(true); // nothing streamed yet: "Loading…"
    fx.emit({ entries: [entry('C:\\data\\a.txt', 'a.txt')] });
    // The FIRST batch is applied immediately, not after the coalescing delay.
    expect(store.rows(0).map((r) => r.entry.path)).toEqual(['C:\\data\\a.txt']);
    expect(store.isSettling(0)).toBe(false);
    expect(store.isLoading(0)).toBe(true); // still walking
  });

  it('coalesces later batches and keeps every row in order', async () => {
    await store.toggle(0, 'C:\\data');
    fx.emit({ entries: [entry('C:\\data\\1', '1')] });
    fx.emit({ entries: [entry('C:\\data\\2', '2')] });
    fx.emit({ entries: [entry('C:\\data\\3', '3')] });
    expect(store.rows(0)).toHaveLength(1); // 2 and 3 wait for the throttle window
    vi.advanceTimersByTime(APPLY_INTERVAL_MS + 1);
    expect(store.rows(0).map((r) => r.entry.name)).toEqual(['1', '2', '3']);
  });

  it('the done event flushes the buffer, ends loading and reports truncation + failures', async () => {
    await store.toggle(0, 'C:\\big');
    fx.emit({ entries: [entry('C:\\big\\x', 'x')] });
    fx.emit({ entries: [entry('C:\\big\\y', 'y')] }); // still buffered
    fx.emit({
      done: true,
      summary: { total: 2, truncated: true, cancelled: false, failedCount: 1, durationMs: 9 },
    });
    const state = store.stateFor(0)!;
    expect(state.rows.map((r) => r.entry.name)).toEqual(['x', 'y']);
    expect(state.loading).toBe(false);
    expect(state.truncated).toBe(true);
    expect(state.failedCount).toBe(1);
    const note = store.noteFor(0);
    expect(note).toContain('limit of 100,000 items reached');
    expect(note).toContain('1 folder(s) could not be read');
    // No timer may fire after done and double-apply.
    vi.advanceTimersByTime(1000);
    expect(store.rows(0)).toHaveLength(2);
  });

  it('toggling again exits without touching tab/history and cancels the backend walk', async () => {
    await store.toggle(1, 'C:\\d');
    const id = fx.lastId();
    await store.toggle(1, 'C:\\d');
    expect(store.activeFor(1)).toBe(false);
    expect(fx.api.cancel).toHaveBeenCalledWith(id);
    expect(fx.api.start).toHaveBeenCalledTimes(1);
  });

  it('Cancel keeps the streamed rows, marks them partial and ignores late events', async () => {
    await store.toggle(0, 'C:\\data');
    const id = fx.lastId();
    fx.emit({ entries: [entry('C:\\data\\a', 'a')] });
    fx.emit({ entries: [entry('C:\\data\\b', 'b')] }); // buffered, must not be lost
    store.cancel(0);
    expect(fx.api.cancel).toHaveBeenCalledWith(id);
    const state = store.stateFor(0)!;
    expect(state.loading).toBe(false);
    expect(state.cancelled).toBe(true);
    expect(state.rows.map((r) => r.entry.name)).toEqual(['a', 'b']);
    expect(store.noteFor(0)).toContain('cancelled');
    // The walker's last batch / done event arrive after the cancel: ignored.
    fx.emit({ id, entries: [entry('C:\\data\\late', 'late')] });
    fx.emit({
      id,
      done: true,
      summary: { total: 3, truncated: false, cancelled: true, failedCount: 0, durationMs: 1 },
    });
    vi.advanceTimersByTime(1000);
    expect(store.rows(0).map((r) => r.entry.name)).toEqual(['a', 'b']);
    expect(store.isLoading(0)).toBe(false);
  });

  it('cancel is a no-op when nothing is loading', async () => {
    await store.toggle(0, 'C:\\d');
    fx.emit({
      done: true,
      summary: { total: 0, truncated: false, cancelled: false, failedCount: 0, durationMs: 1 },
    });
    fx.api.cancel.mockClear();
    store.cancel(0);
    expect(fx.api.cancel).not.toHaveBeenCalled();
    expect(store.stateFor(0)?.cancelled).toBe(false);
  });

  it('surfaces traversal errors and clears the branch state', async () => {
    await store.toggle(0, 'C:\\gone');
    fx.emit({
      done: true,
      error: new FsError({ code: 'notFound', message: 'root gone' }),
    });
    expect(store.stateFor(0)?.error?.message).toBe('root gone');
    expect(store.stateFor(0)?.loading).toBe(false);
    expect(store.rows(0)).toEqual([]);
    store.exit(0);
    expect(store.activeFor(0)).toBe(false);
  });

  it('a start failure becomes an error state', async () => {
    fx.api.start.mockRejectedValueOnce(new FsError({ code: 'invalidPath', message: 'bad root' }));
    await store.toggle(0, 'C:\\bad');
    expect(store.stateFor(0)?.error?.message).toBe('bad root');
    expect(store.stateFor(0)?.loading).toBe(false);
  });

  it('events of a superseded walk never touch a newer branch of the same pane', async () => {
    await store.toggle(0, 'C:\\old-root');
    const oldId = fx.lastId();
    store.exit(0); // the user leaves the half-loaded branch...
    await store.toggle(0, 'C:\\new'); // ...then branches somewhere else
    const newId = fx.lastId();
    expect(newId).not.toBe(oldId);
    fx.emit({ id: newId, entries: [entry('C:\\new\\fresh.txt', 'fresh.txt')] });
    fx.emit({ id: oldId, entries: [entry('C:\\old\\stale.txt', 'stale.txt')] });
    fx.emit({
      id: oldId,
      done: true,
      summary: { total: 1, truncated: false, cancelled: false, failedCount: 0, durationMs: 1 },
    });
    expect(store.stateFor(0)?.root).toBe('C:\\new');
    expect(store.stateFor(0)?.loading).toBe(true); // the OLD done must not end the new walk
    expect(store.rows(0).map((r) => r.entry.name)).toEqual(['fresh.txt']);
  });

  it('feed cancels a running walk so it cannot overwrite the fed rows', async () => {
    await store.toggle(0, 'C:\\data');
    const id = fx.lastId();
    store.feed(0, 'C:\\data', 'Search results · 1 items', [entry('C:\\x\\hit.txt', 'hit.txt')]);
    expect(fx.api.cancel).toHaveBeenCalledWith(id);
    fx.emit({ id, entries: [entry('C:\\data\\a', 'a')] });
    expect(store.rows(0).map((r) => r.entry.name)).toEqual(['hit.txt']);
    expect(store.noteFor(0)).toBe(''); // fed listings keep their own label
  });

  it('rows keep their ORIGINAL deep paths across batches (operations use real paths)', async () => {
    await store.toggle(0, 'C:\\data');
    fx.emit({ entries: [entry('C:\\data\\x\\same.txt', 'same.txt')] });
    fx.emit({
      entries: [entry('C:\\data\\y\\same.txt', 'same.txt')],
      done: true,
      summary: { total: 2, truncated: false, cancelled: false, failedCount: 0, durationMs: 1 },
    });
    expect(store.rows(0).map((r) => r.entry.path)).toEqual([
      'C:\\data\\x\\same.txt',
      'C:\\data\\y\\same.txt',
    ]);
  });
});
