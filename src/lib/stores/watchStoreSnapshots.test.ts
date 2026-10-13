/**
 * FER-142 gap: external changes while search results or the compare view are
 * open. Documented behavior pinned here (no new feature): both are
 * point-in-time SNAPSHOTS. A watcher batch refreshes the affected pane
 * listing only — it never re-runs, clears or mutates the search results or
 * the compare report, and never triggers a search/compare IPC call (so an
 * external change storm cannot start a refresh loop through those views).
 * The user re-runs Search/Compare explicitly to see the new state.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { registerPane, type PaneApi } from './panesRegistry.svelte';
import type { FileEntry } from '../ipc/types';
import type { CompareDirsReport } from '../ipc/compare';

const watchStartMock = vi.fn();
const watchStopMock = vi.fn();
vi.mock('../ipc/watch', () => ({
  watchStart: (...a: unknown[]) => watchStartMock(...a),
  watchStop: (...a: unknown[]) => watchStopMock(...a),
  onFsChanged: vi.fn(async () => () => {}),
}));

const searchStartMock = vi.fn();
vi.mock('../ipc/search', () => ({
  defaultSearchOptions: () => ({ pattern: '' }),
  onSearchEvent: vi.fn(async () => () => {}),
  searchCancel: vi.fn(async () => {}),
  searchStart: (...a: unknown[]) => searchStartMock(...a),
}));

const compareDirsMock = vi.fn();
const compareFilesMock = vi.fn();
vi.mock('../ipc/compare', () => ({
  validateSyncRoots: vi.fn(async () => {}),
  compareDirs: (...a: unknown[]) => compareDirsMock(...a),
  compareFiles: (...a: unknown[]) => compareFilesMock(...a),
}));

const { watcher, REFRESH_SPACING_MS } = await import('./watchStore.svelte');
const { searchStore } = await import('./searchStore.svelte');
const { compareStore } = await import('./compareStore.svelte');

const wait = (ms: number) => new Promise((r) => setTimeout(r, ms));

function entry(name: string): FileEntry {
  return {
    name,
    path: `C:\\alpha\\${name}`,
    isDir: false,
    isSymlink: false,
    size: 1,
    modifiedMs: 1,
    hidden: false,
  };
}

function fakePane(path: string, refresh = vi.fn(), refreshExternal = vi.fn()): PaneApi {
  return {
    path,
    selectedPaths: () => [],
    cursorEntry: () => undefined,
    refresh,
    refreshExternal,
    newTab: () => {},
    closeCurrentTab: () => {},
    goUp: () => {},
    openCursorEntry: () => {},
  };
}

const diffReport = {
  entries: [
    {
      name: 'a.txt',
      state: 'same',
      leftSize: 1,
      leftModifiedMs: 1,
      rightSize: 1,
      rightModifiedMs: 1,
    },
  ],
} as unknown as CompareDirsReport;

beforeEach(() => {
  watcher.stop();
  watcher.spacingMs = 20;
  watchStartMock.mockReset().mockResolvedValue(undefined);
  watchStopMock.mockReset().mockResolvedValue(undefined);
  searchStartMock.mockReset();
  compareDirsMock.mockReset();
  compareFilesMock.mockReset();
  searchStore.results = [entry('hit1.txt'), entry('hit2.txt')];
  searchStore.panelOpen = true;
  compareStore.report = diffReport;
  compareStore.panelOpen = true;
});

afterEach(() => {
  watcher.stop();
  watcher.spacingMs = REFRESH_SPACING_MS;
  searchStore.results = [];
  searchStore.panelOpen = false;
  compareStore.report = null;
  compareStore.panelOpen = false;
});

describe('watcher batches with search results / compare view open (FER-142)', () => {
  it('refreshes the pane but leaves search results untouched and starts no search', async () => {
    const refreshExternal = vi.fn();
    const un = registerPane(0, fakePane('C:\\alpha', vi.fn(), refreshExternal));
    await watcher.registerPanePath(0, 'C:\\alpha');

    watcher.onBatch({ roots: ['C:\\alpha'], paths: ['C:\\alpha\\hit1.txt'] }); // a hit was changed
    expect(refreshExternal).toHaveBeenCalledTimes(1);
    expect(searchStore.results.map((r) => r.name)).toEqual(['hit1.txt', 'hit2.txt']);
    expect(searchStore.panelOpen).toBe(true);
    expect(searchStartMock).not.toHaveBeenCalled();
    un();
  });

  it('leaves the compare report and panel intact and runs no compare', async () => {
    const left = fakePane('C:\\alpha', vi.fn(), vi.fn());
    const right = fakePane('C:\\beta', vi.fn(), vi.fn());
    const un0 = registerPane(0, left);
    const un1 = registerPane(1, right);
    await watcher.registerPanePath(0, 'C:\\alpha');
    await watcher.registerPanePath(1, 'C:\\beta');

    watcher.onBatch({ roots: ['C:\\alpha'], paths: ['C:\\alpha\\a.txt'] });
    expect(left.refreshExternal).toHaveBeenCalledTimes(1);
    expect(right.refreshExternal).not.toHaveBeenCalled();
    expect(compareStore.report).toEqual(diffReport); // $state proxies the object
    expect(compareStore.panelOpen).toBe(true);
    expect(compareDirsMock).not.toHaveBeenCalled();
    expect(compareFilesMock).not.toHaveBeenCalled();
    un0();
    un1();
  });

  it('a storm plus an ambiguous (root vanished) batch still never touches either view', async () => {
    const refresh = vi.fn();
    const refreshExternal = vi.fn();
    const un = registerPane(0, fakePane('C:\\alpha', refresh, refreshExternal));
    await watcher.registerPanePath(0, 'C:\\alpha');

    for (let i = 0; i < 20; i++) {
      watcher.onBatch({ roots: ['C:\\alpha'], paths: [`C:\\alpha\\f${i}`] });
    }
    watcher.onBatch({ roots: ['C:\\alpha'], paths: ['C:\\alpha'] }); // root itself
    await wait(80); // trailing edge fires, then the pane cools: no loop
    expect(refreshExternal).toHaveBeenCalledTimes(1);
    expect(refresh).toHaveBeenCalledTimes(1); // one escalated reset, not a loop
    await wait(80);
    expect(refresh).toHaveBeenCalledTimes(1);

    expect(searchStore.results).toHaveLength(2);
    expect(compareStore.report).toEqual(diffReport); // $state proxies the object
    expect(searchStartMock).not.toHaveBeenCalled();
    expect(compareDirsMock).not.toHaveBeenCalled();
    un();
  });
});
