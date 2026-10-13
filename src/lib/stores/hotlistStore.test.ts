import { afterEach, describe, expect, it, vi } from 'vitest';
import { HotlistStore, locate, type LegacyStorage } from './hotlistStore.svelte';
import { KnownHosts } from '../networkPath';
import { HotlistError, type HotlistBackend, type HotlistView } from '../ipc/hotlist';
import { V1_KEY, type CommandItem, type DirItem, type HotItem } from '../hotlist/model';
import { registerPane, type PaneApi } from './panesRegistry.svelte';

const dir = (title: string, path: string): DirItem => ({ type: 'dir', title, path });

function view(items: HotItem[], extra: Partial<HotlistView> = {}): HotlistView {
  return {
    doc: { version: 2, items },
    status: 'ok',
    readOnly: false,
    customLocation: null,
    ...extra,
  };
}

/** In-memory backend with call recording and switchable failures. */
function fakeBackend(initial: HotlistView = view([])) {
  const state = { view: initial, failSave: null as string | null, failRun: null as string | null };
  const save = vi.fn(async (doc: HotlistView['doc']) => {
    if (state.failSave) throw new HotlistError(state.failSave);
    state.view = { ...state.view, doc, status: 'ok' };
    return state.view;
  });
  const runCommand = vi.fn<HotlistBackend['runCommand']>(async () => {
    if (state.failRun) throw new HotlistError(state.failRun);
    return { launched: true };
  });
  const backend: HotlistBackend = {
    load: async () => state.view,
    save,
    checkLocation: async () => false,
    setLocation: async () => state.view,
    importPreview: async () => {
      throw new HotlistError('unsupported');
    },
    runCommand,
  };
  return { backend, state, save, runCommand };
}

function memoryStorage(initial: string | null): LegacyStorage & { value: string | null } {
  const s = {
    value: initial,
    getItem: (k: string) => (k === V1_KEY ? s.value : null),
    removeItem: (k: string) => {
      if (k === V1_KEY) s.value = null;
    },
  };
  return s;
}

function pane(path: string, openPath = vi.fn()): PaneApi {
  return {
    path,
    selectedPaths: () => [],
    cursorEntry: () => undefined,
    refresh: () => {},
    newTab: () => {},
    closeCurrentTab: () => {},
    goUp: () => {},
    openCursorEntry: () => {},
    openPath,
  };
}

afterEach(() => vi.useRealTimers());

describe('HotlistStore load and migration', () => {
  it('mirrors the backend view', async () => {
    const f = fakeBackend(view([dir('A', 'C:\\a')], { customLocation: 'D:\\h.json' }));
    const store = new HotlistStore(f.backend, null);
    await store.load();
    expect(store.loaded).toBe(true);
    expect(store.entries()).toEqual([{ title: 'A', path: 'C:\\a' }]);
    expect(store.customLocation).toBe('D:\\h.json');
    expect(store.readOnly).toBe(false);
  });

  it('migrates the v1 localStorage list once, only on a brand-new hotlist', async () => {
    const legacy = memoryStorage(JSON.stringify([{ name: 'Code', path: 'C:\\code' }]));
    const f = fakeBackend(view([], { status: 'new' }));
    const store = new HotlistStore(f.backend, legacy);
    await store.load();
    expect(f.save).toHaveBeenCalledOnce();
    expect(store.entries()).toEqual([{ title: 'Code', path: 'C:\\code' }]);
    expect(legacy.value).toBeNull();
  });

  it('keeps the v1 list when the save fails, and skips migration when a file already exists', async () => {
    const raw = JSON.stringify([{ name: 'Code', path: 'C:\\code' }]);
    const failing = fakeBackend(view([], { status: 'new' }));
    failing.state.failSave = 'storage';
    const legacy = memoryStorage(raw);
    await new HotlistStore(failing.backend, legacy).load();
    expect(legacy.value).toBe(raw);

    const existing = fakeBackend(view([], { status: 'ok' }));
    await new HotlistStore(existing.backend, memoryStorage(raw)).load();
    expect(existing.save).not.toHaveBeenCalled();
  });
});

describe('HotlistStore editing', () => {
  it('adds a folder once (deduped, case-insensitive) with a basename title', async () => {
    const f = fakeBackend();
    const store = new HotlistStore(f.backend, null);
    await store.load();
    expect(await store.addFolder('C:\\Work\\Project\\')).toBe(true);
    expect(store.entries()[0]).toEqual({ title: 'Project', path: 'C:\\Work\\Project\\' });
    expect(await store.addFolder('c:\\work\\project')).toBe(false);
    expect(store.notice).toMatch(/already/);
    expect(f.save).toHaveBeenCalledOnce();
  });

  it('removes every entry for a folder, including nested ones', async () => {
    const f = fakeBackend(
      view([
        dir('A', 'C:\\x'),
        { type: 'submenu', title: 'S', items: [dir('B', 'C:\\X\\'), dir('C', 'C:\\y')] },
      ]),
    );
    const store = new HotlistStore(f.backend, null);
    await store.load();
    expect(await store.removeFolder('C:\\x')).toBe(true);
    expect(store.entries()).toEqual([{ title: 'C', path: 'C:\\y' }]);
    expect(await store.removeFolder('C:\\none')).toBe(false);
  });

  it('refuses to write while read-only and says why', async () => {
    const f = fakeBackend(view([dir('A', 'C:\\a')], { status: 'unreadable', readOnly: true }));
    const store = new HotlistStore(f.backend, null);
    await store.load();
    expect(await store.addFolder('C:\\b')).toBe(false);
    expect(f.save).not.toHaveBeenCalled();
    expect(store.notice).toMatch(/unavailable/);
  });

  it('on a conflict reloads the file and asks the user to repeat the change', async () => {
    const f = fakeBackend(view([dir('A', 'C:\\a')]));
    const store = new HotlistStore(f.backend, null);
    await store.load();
    f.state.view = view([dir('FROM-OTHER-PC', 'C:\\o')]);
    f.state.failSave = 'conflict';
    expect(await store.addFolder('C:\\b')).toBe(false);
    expect(store.entries()).toEqual([{ title: 'FROM-OTHER-PC', path: 'C:\\o' }]);
    expect(store.notice).toMatch(/changed on disk/);
  });

  it('import appends or replaces and reports what did not fit', async () => {
    const f = fakeBackend(view([dir('Mine', 'C:\\m')]));
    const store = new HotlistStore(f.backend, null);
    await store.load();
    const incoming = [dir('TC', 'C:\\tc')];
    expect(await store.importItems(incoming, 'Total Commander', false)).toEqual({
      ok: true,
      dropped: 0,
    });
    expect(store.entries().map((e) => e.title)).toEqual(['Mine', 'TC']);
    expect(store.doc.source).toBe('Total Commander');
    await store.importItems(incoming, undefined, true);
    expect(store.entries().map((e) => e.title)).toEqual(['TC']);
  });

  it('notices disappear on their own', () => {
    vi.useFakeTimers();
    const store = new HotlistStore(fakeBackend().backend, null);
    store.say('hello');
    expect(store.notice).toBe('hello');
    vi.advanceTimersByTime(6000);
    expect(store.notice).toBe('');
  });
});

describe('HotlistStore running items', () => {
  it('opens a folder in the active pane and its target path in the other pane', () => {
    const left = vi.fn();
    const right = vi.fn();
    registerPane(0, pane('C:\\l', left));
    registerPane(1, pane('C:\\r', right));
    const store = new HotlistStore(fakeBackend().backend, null);
    store.openDir({ type: 'dir', title: 'x', path: 'C:\\a', targetPath: 'D:\\b' });
    expect(left).toHaveBeenCalledWith('C:\\a');
    expect(right).toHaveBeenCalledWith('D:\\b');
    store.openDir(dir('y', 'C:\\only'));
    expect(left).toHaveBeenLastCalledWith('C:\\only');
    expect(right).toHaveBeenCalledOnce();
  });

  const cmd: CommandItem = { type: 'command', title: 'k', command: 'C:\\k.exe', args: ['-a'] };

  it('runs a command by its place in the tree; the backend owns the confirmation', async () => {
    const tree: HotItem[] = [dir('a', 'C:\\a'), { type: 'submenu', title: 's', items: [cmd] }];
    const f = fakeBackend(view(tree));
    const store = new HotlistStore(f.backend, null);
    await store.load();
    const shown = (store.doc.items[1] as { items: HotItem[] }).items[0] as CommandItem;
    expect(locate(store.doc.items, shown)).toEqual([1, 0]);
    await store.runCommand(shown);
    expect(f.runCommand).toHaveBeenLastCalledWith([1, 0], shown);
    // An explicit position (menu rows know theirs) is passed through as is.
    await store.runCommand(shown, [1, 0]);
    expect(f.runCommand).toHaveBeenCalledTimes(2);
    // Something that is not in the tree never reaches the backend.
    await store.runCommand({ ...cmd });
    expect(f.runCommand).toHaveBeenCalledTimes(2);
    expect(store.notice).toMatch(/no longer/);
  });

  it('failures become a calm notice, never a throw; a stale item reloads', async () => {
    const f = fakeBackend(view([cmd]));
    const store = new HotlistStore(f.backend, null);
    await store.load();
    f.state.failRun = 'not_found';
    await store.runCommand(store.doc.items[0] as CommandItem);
    expect(store.notice).toMatch(/not found/);
    f.state.failRun = 'location_unsafe';
    await store.runCommand(store.doc.items[0] as CommandItem);
    expect(store.notice).toMatch(/not safe/);
    f.state.failRun = 'stale';
    const load = vi.spyOn(store, 'load');
    await store.runCommand(store.doc.items[0] as CommandItem);
    expect(load).toHaveBeenCalled();
    expect(store.notice).toMatch(/changed on disk/);
  });

  it('a network folder asks once per new host; a host a pane already shows does not', () => {
    const open = vi.fn();
    registerPane(0, pane('C:\\l', open));
    registerPane(1, pane('\\\\here\\share', vi.fn()));
    const mem = new Map<string, string>();
    const memory = {
      getItem: (k: string) => mem.get(k) ?? null,
      setItem: (k: string, v: string) => void mem.set(k, v),
    };
    const store = new HotlistStore(fakeBackend().backend, null, new KnownHosts(memory));
    store.openDir(dir('far', '\\\\Far\\x'));
    expect(open).not.toHaveBeenCalled();
    expect(store.pendingNetwork?.hosts).toEqual(['far']);
    store.cancelNetwork();
    expect(store.pendingNetwork).toBeNull();
    expect(open).not.toHaveBeenCalled();
    store.openDir(dir('far', '\\\\far\\y'));
    store.confirmNetwork();
    expect(open).toHaveBeenCalledExactlyOnceWith('\\\\far\\y');
    store.openDir(dir('far again', '//FAR/z'));
    expect(store.pendingNetwork).toBeNull();
    expect(open).toHaveBeenCalledTimes(2);
    // The other pane is already on \\here: no question.
    store.openDir(dir('here', '\\\\here\\other'));
    expect(open).toHaveBeenCalledTimes(3);
    // A remembered host survives a new store (same storage).
    const again = new HotlistStore(fakeBackend().backend, null, new KnownHosts(memory));
    again.openDir(dir('far', '\\\\far\\q'));
    expect(again.pendingNetwork).toBeNull();
  });

  it('activating a folder or a command routes to the right action', () => {
    const open = vi.fn();
    registerPane(0, pane('C:\\l', open));
    const f = fakeBackend();
    const store = new HotlistStore(f.backend, null);
    store.activate(dir('a', 'C:\\a'));
    expect(open).toHaveBeenCalledWith('C:\\a');
    store.activate({ type: 'separator' });
    store.activate({ type: 'submenu', title: 's', items: [] });
    expect(f.runCommand).not.toHaveBeenCalled();
    store.activate(cmd, [3]);
    expect(f.runCommand).toHaveBeenCalledExactlyOnceWith([3], cmd);
  });
});
