import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { registerPane, type PaneApi } from './panesRegistry.svelte';

const watchStartMock = vi.fn();
const watchStopMock = vi.fn();
const onFsChangedMock = vi.fn();
vi.mock('../ipc/watch', () => ({
  watchStart: (...a: unknown[]) => watchStartMock(...a),
  watchStop: (...a: unknown[]) => watchStopMock(...a),
  onFsChanged: (...a: unknown[]) => onFsChangedMock(...a),
}));

const { watcher, REFRESH_SPACING_MS } = await import('./watchStore.svelte');

type Batch = { roots: string[]; paths: string[] };

function fakePane(refresh = vi.fn(), refreshExternal = vi.fn()): PaneApi {
  return {
    path: 'C:\\t',
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

function pane(id: 0 | 1, refresh = vi.fn(), refreshExternal = vi.fn()): () => void {
  return registerPane(id, fakePane(refresh, refreshExternal));
}

const wait = (ms: number) => new Promise((r) => setTimeout(r, ms));

beforeEach(() => {
  watcher.stop(); // reset singleton state (timers, maps, cooling)
  watcher.spacingMs = 20; // fast, deterministic anti-thrash window
  watchStartMock.mockReset().mockResolvedValue(undefined);
  watchStopMock.mockReset().mockResolvedValue(undefined);
  onFsChangedMock.mockReset().mockResolvedValue(() => {});
});

afterEach(() => {
  watcher.stop();
  watcher.spacingMs = REFRESH_SPACING_MS;
});

describe('watcher store (F2-26)', () => {
  it('watches one root per distinct pane path and cleans up on navigation', async () => {
    const un0 = pane(0);
    const un1 = pane(1);
    await watcher.registerPanePath(0, 'C:\\alpha');
    await watcher.registerPanePath(1, 'C:\\alpha'); // shared root → one watch
    await wait(0);
    expect(watchStartMock).toHaveBeenCalledTimes(1);
    expect(watchStartMock).toHaveBeenCalledWith('C:\\alpha');
    expect(watcher.watchedRoots()).toEqual(['C:\\alpha']);

    // Both panes navigate away from alpha → subscription is dropped.
    await watcher.registerPanePath(0, 'C:\\beta');
    await watcher.registerPanePath(1, 'C:\\gamma');
    await wait(0);
    expect(watchStopMock).toHaveBeenCalledWith('C:\\alpha');
    expect(watcher.watchedRoots().sort()).toEqual(['C:\\beta', 'C:\\gamma'].sort());
    un0();
    un1();
  });

  it('routes batches only to the pane whose root was hit (per-pane scoping)', async () => {
    const un0 = pane(0);
    const un1 = pane(1);
    await watcher.registerPanePath(0, 'C:\\alpha');
    await watcher.registerPanePath(1, 'C:\\beta');
    const leftRefresh = vi.fn();
    const leftExternal = vi.fn();
    const rightRefresh = vi.fn();
    const rightExternal = vi.fn();
    registerPane(0, { ...fakePane(), refresh: leftRefresh, refreshExternal: leftExternal });
    registerPane(1, { ...fakePane(), refresh: rightRefresh, refreshExternal: rightExternal });

    watcher.onBatch({ roots: ['C:\\alpha'], paths: ['C:\\alpha\\new.txt'] });
    expect(leftExternal).toHaveBeenCalledTimes(1);
    expect(leftRefresh).not.toHaveBeenCalled();
    expect(rightRefresh).not.toHaveBeenCalled();
    expect(rightExternal).not.toHaveBeenCalled();
    un0();
    un1();
  });

  it('uses a state-preserving refresh for scoped changes and a full reset when ambiguous', async () => {
    const un0 = pane(0);
    await watcher.registerPanePath(0, 'C:\\alpha');
    const refresh = vi.fn();
    const refreshExternal = vi.fn();
    registerPane(0, { ...fakePane(), refresh, refreshExternal });

    watcher.onBatch({ roots: ['C:\\alpha'], paths: ['C:\\alpha\\new.txt'] });
    expect(refreshExternal).toHaveBeenCalledTimes(1);
    expect(refresh).not.toHaveBeenCalled();

    // The watched directory itself in the payload → ambiguous. It lands in
    // the same spacing window, so the reset refresh arrives via the trailing
    // edge WITH the ambiguous semantics preserved.
    watcher.onBatch({ roots: ['C:\\alpha'], paths: ['C:\\alpha'] });
    expect(refresh).not.toHaveBeenCalled(); // still cooling
    await wait(50);
    expect(refresh).toHaveBeenCalledTimes(1);
    expect(refreshExternal).toHaveBeenCalledTimes(1); // never reset twice
    un0();
  });

  it('collapses an event storm into one refresh plus at most one trailing refresh', async () => {
    const un0 = pane(0);
    await watcher.registerPanePath(0, 'C:\\alpha');
    const refreshExternal = vi.fn();
    registerPane(0, { ...fakePane(), refreshExternal });

    // Storm: many coalesced batches inside one spacing window.
    for (let i = 0; i < 10; i++) {
      watcher.onBatch({ roots: ['C:\\alpha'], paths: [`C:\\alpha\\f${i}.txt`] });
    }
    expect(refreshExternal).toHaveBeenCalledTimes(1); // no thrash

    await wait(70); // > 2 × spacingMs
    expect(refreshExternal).toHaveBeenCalledTimes(2); // single trailing refresh
    await wait(70); // cooled down, nothing pending → no refresh loop
    expect(refreshExternal).toHaveBeenCalledTimes(2);
    un0();
  });

  it('keeps panes cool independently of each other', async () => {
    const un0 = pane(0);
    const un1 = pane(1);
    await watcher.registerPanePath(0, 'C:\\alpha');
    await watcher.registerPanePath(1, 'C:\\beta');
    const left = vi.fn();
    const right = vi.fn();
    registerPane(0, { ...fakePane(), refreshExternal: left });
    registerPane(1, { ...fakePane(), refreshExternal: right });

    watcher.onBatch({ roots: ['C:\\alpha'], paths: ['C:\\alpha\\x'] });
    watcher.onBatch({ roots: ['C:\\beta'], paths: ['C:\\beta\\y'] });
    expect(left).toHaveBeenCalledTimes(1);
    expect(right).toHaveBeenCalledTimes(1);
    un0();
    un1();
  });

  it('survives watch_start failure (device disconnect) and retries on navigation', async () => {
    const un0 = pane(0);
    watchStartMock.mockRejectedValueOnce(new Error('device gone'));
    await expect(watcher.registerPanePath(0, 'C:\\lost')).resolves.toBeUndefined();
    await wait(0);
    expect(watcher.watchedRoots()).toEqual([]); // swallowed, not crashed

    await watcher.registerPanePath(0, 'C:\\ok');
    await wait(0);
    expect(watcher.watchedRoots()).toEqual(['C:\\ok']);

    await watcher.registerPanePath(0, 'C:\\lost'); // back to the reconnected path
    await wait(0);
    // Navigating away from C:\ok stopped its subscription; only the current
    // path stays watched.
    expect(watcher.watchedRoots()).toEqual(['C:\\lost']);
    un0();
  });

  it('stop() unsubscribes the event and stops every root', async () => {
    (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {};
    const unlisten = vi.fn();
    let delivered: ((batch: Batch) => void) | null = null;
    onFsChangedMock.mockImplementation(async (cb: (batch: Batch) => void) => {
      delivered = cb;
      return unlisten;
    });
    const un0 = pane(0);
    await watcher.registerPanePath(0, 'C:\\alpha');
    await watcher.start();
    expect(delivered).not.toBeNull();

    const refreshExternal = vi.fn();
    registerPane(0, { ...fakePane(), refreshExternal });
    watcher.stop();
    expect(unlisten).toHaveBeenCalled();
    expect(watchStopMock).toHaveBeenCalledWith('C:\\alpha');
    expect(watcher.watchedRoots()).toEqual([]);

    // A late event after stop() must not reach any pane.
    delivered!({ roots: ['C:\\alpha'], paths: [] });
    expect(refreshExternal).not.toHaveBeenCalled();
    delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__;
    un0();
  });
});

describe('watcher archive-parent routing (FER-369, opens D43)', () => {
  beforeEach(() => {
    watcher.stop();
    watcher.spacingMs = 20;
    watchStartMock.mockReset().mockResolvedValue(undefined);
    watchStopMock.mockReset().mockResolvedValue(undefined);
    onFsChangedMock.mockReset().mockResolvedValue(() => {});
  });

  afterEach(() => {
    watcher.stop();
    watcher.spacingMs = REFRESH_SPACING_MS;
  });

  it('watches the parent ARCHIVE FILE while a pane is inside the archive', async () => {
    const un0 = pane(0);
    // Deep inside a virtual location and at the archive root: both route to
    // the on-disk archive file, never to the virtual path.
    await watcher.registerPanePath(0, 'C:\\dir\\bundle.zip\\members\\sub');
    await wait(0);
    expect(watchStartMock).toHaveBeenCalledWith('C:\\dir\\bundle.zip');
    expect(watcher.watchedRoots()).toEqual(['C:\\dir\\bundle.zip']);

    await watcher.registerPanePath(0, 'C:\\other\\solo.7z');
    await wait(0);
    expect(watchStartMock).toHaveBeenCalledWith('C:\\other\\solo.7z');
    expect(watchStopMock).toHaveBeenCalledWith('C:\\dir\\bundle.zip');

    // Navigating out of archives entirely drops the archive subscription.
    await watcher.registerPanePath(0, 'C:\\plain');
    await wait(0);
    expect(watchStopMock).toHaveBeenCalledWith('C:\\other\\solo.7z');
    expect(watcher.watchedRoots()).toEqual(['C:\\plain']);
    un0();
  });

  it('refreshes the pane (state-preserving) when the archive file changes', async () => {
    const un0 = pane(0);
    await watcher.registerPanePath(0, 'C:\\dir\\bundle.zip\\members');
    const refresh = vi.fn();
    const refreshExternal = vi.fn();
    registerPane(0, { ...fakePane(), refresh, refreshExternal });

    // The archive file always reports itself; that is a content change, not
    // the directory-vanished ambiguity → scoped, selection-preserving.
    watcher.onBatch({ roots: ['C:\\dir\\bundle.zip'], paths: ['C:\\dir\\bundle.zip'] });
    expect(refreshExternal).toHaveBeenCalledTimes(1);
    expect(refresh).not.toHaveBeenCalled();
    un0();
  });

  it('ignores disk events for unrelated files and sibling directories', async () => {
    const un0 = pane(0);
    const un1 = pane(1);
    await watcher.registerPanePath(0, 'C:\\dir\\bundle.zip\\members');
    await watcher.registerPanePath(1, 'C:\\plain');
    const archiveRefresh = vi.fn();
    const archiveExternal = vi.fn();
    const diskExternal = vi.fn();
    registerPane(0, { ...fakePane(), refresh: archiveRefresh, refreshExternal: archiveExternal });
    registerPane(1, { ...fakePane(), refreshExternal: diskExternal });

    // A different archive file changed: neither pane cares.
    watcher.onBatch({ roots: ['C:\\dir\\other.zip'], paths: ['C:\\dir\\other.zip'] });
    // Sibling disk noise in the archive's folder: still not the watched root.
    watcher.onBatch({ roots: ['C:\\dir'], paths: ['C:\\dir\\note.txt'] });
    // The archive pane's own root hit is the only refresh.
    watcher.onBatch({ roots: ['C:\\dir\\bundle.zip'], paths: ['C:\\dir\\bundle.zip'] });
    expect(archiveExternal).toHaveBeenCalledTimes(1);
    expect(archiveRefresh).not.toHaveBeenCalled();
    expect(diskExternal).not.toHaveBeenCalled();
    un0();
    un1();
  });

  it('keeps a disk pane scoped-refreshing while the archive pane refreshes too', async () => {
    // Both roots are hit when the zip changes next to a watched disk pane.
    const un0 = pane(0);
    const un1 = pane(1);
    await watcher.registerPanePath(0, 'C:\\dir');
    await watcher.registerPanePath(1, 'C:\\dir\\bundle.zip');
    const diskExternal = vi.fn();
    const diskRefresh = vi.fn();
    const archiveExternal = vi.fn();
    registerPane(0, { ...fakePane(), refresh: diskRefresh, refreshExternal: diskExternal });
    registerPane(1, { ...fakePane(), refreshExternal: archiveExternal });

    watcher.onBatch({ roots: ['C:\\dir', 'C:\\dir\\bundle.zip'], paths: ['C:\\dir\\bundle.zip'] });
    expect(archiveExternal).toHaveBeenCalledTimes(1);
    // The disk pane sees a scoped listing change (the zip entry changed),
    // NOT the ambiguous directory reset.
    expect(diskExternal).toHaveBeenCalledTimes(1);
    expect(diskRefresh).not.toHaveBeenCalled();
    un0();
    un1();
  });
});
