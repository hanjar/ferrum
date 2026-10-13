import { beforeEach, describe, expect, it, vi } from 'vitest';
import {
  SeparateTreeStore,
  cwdAncestors,
  parseSeparateTreeState,
  persistSeparateTreeState,
  separateTree,
} from './separateTreeStore.svelte';
import { TreeStore } from './treeStore.svelte';
import { FsError, type FileEntry } from '../ipc/types';

const KEY = 'ferrum.separateTree.v1';

const entry = (name: string, isDir = false, parent = 'C:\\'): FileEntry => ({
  name,
  path: `${parent}${name}`,
  isDir,
  isSymlink: false,
  size: isDir ? 0 : 10,
  modifiedMs: 1790512000000,
  hidden: false,
});

/** Auto-resolving fetcher scripted per path; unscripted paths 404. */
function scripted(map: Record<string, FileEntry[]>) {
  const calls: string[] = [];
  const fetcher = (p: string): Promise<FileEntry[]> => {
    calls.push(p);
    const rows = map[p];
    if (!rows) {
      return Promise.reject(new FsError({ code: 'notFound', message: `no script for ${p}` }));
    }
    return Promise.resolve(rows);
  };
  return { calls, fetcher };
}

function provider(roots: string[]) {
  return (): Promise<{ path: string; label: string; meta: string }[]> =>
    Promise.resolve(roots.map((path) => ({ path, label: path, meta: '' })));
}

/** Map-backed Storage stand-in with optional seed data and failure mode. */
function memoryStorage(
  seed: Record<string, string> = {},
  opts: { failWrites?: boolean } = {},
): Storage {
  const map = new Map<string, string>(Object.entries(seed));
  return {
    get length() {
      return map.size;
    },
    clear: () => map.clear(),
    getItem: (k: string) => (map.has(k) ? map.get(k)! : null),
    key: (i: number) => [...map.keys()][i] ?? null,
    removeItem: (k: string) => void map.delete(k),
    setItem: (k: string, v: string) => {
      if (opts.failWrites) throw new DOMException('quota', 'QuotaExceededError');
      map.set(k, String(v));
    },
  } as Storage;
}

function makeFactory(fetcher: (p: string) => Promise<FileEntry[]>, roots: string[]) {
  return () => new TreeStore(fetcher, provider(roots));
}

beforeEach(() => {
  separateTree.resetAll();
});

describe('separate tree store — toggle semantics (FER-366)', () => {
  it('defaults to OFF for both panes and writes nothing until used', () => {
    const storage = memoryStorage();
    const store = new SeparateTreeStore(makeFactory(scripted({}).fetcher, ['C:\\']), storage);
    expect(store.isEnabled(0)).toBe(false);
    expect(store.isEnabled(1)).toBe(false);
    expect(storage.getItem(KEY)).toBeNull(); // echo guard: no redundant write
  });

  it('toggles panes independently and persists each change', () => {
    const storage = memoryStorage();
    const store = new SeparateTreeStore(makeFactory(scripted({}).fetcher, ['C:\\']), storage);
    store.toggle(0);
    expect(store.isEnabled(0)).toBe(true);
    expect(store.isEnabled(1)).toBe(false);
    expect(JSON.parse(storage.getItem(KEY)!).enabled).toEqual([true, false]);
    store.toggle(1);
    expect(JSON.parse(storage.getItem(KEY)!).enabled).toEqual([true, true]);
    store.toggle(1);
    expect(store.isEnabled(1)).toBe(false);
    expect(JSON.parse(storage.getItem(KEY)!).enabled).toEqual([true, false]);
  });

  it('keeps the live tree (expansion included) across off→on', async () => {
    const { fetcher } = scripted({ 'C:\\': [entry('a', true)], 'C:\\a': [entry('sub', true)] });
    const storage = memoryStorage();
    const store = new SeparateTreeStore(makeFactory(fetcher, ['C:\\']), storage);
    store.toggle(0);
    await store.trees[0].toggle('C:\\a');
    expect(store.trees[0].expanded.has('C:\\a')).toBe(true);
    store.toggle(0); // off
    store.toggle(0); // on again — already hydrated, no reset
    expect(store.trees[0].expanded.has('C:\\a')).toBe(true);
  });
});

describe('separate tree store — pane-isolated trees', () => {
  it('follows each pane cwd in ITS OWN tree only', async () => {
    const { calls, fetcher } = scripted({
      'C:\\': [entry('a', true), entry('b', true)],
      'C:\\a': [entry('sub', true)],
      'D:\\': [entry('solo', true)],
    });
    const store = new SeparateTreeStore(makeFactory(fetcher, ['C:\\', 'D:\\']), memoryStorage());

    await store.follow(0, 'C:\\a\\sub');
    expect([...store.trees[0].expanded]).toEqual(['C:\\', 'C:\\a']);
    expect(store.trees[0].selected).toBe('C:\\a\\sub');
    // Pane 1's tree never moved: no shared expansion, selection, or fetches.
    expect(store.trees[1].expanded.size).toBe(0);
    expect(store.trees[1].selected).toBeNull();
    expect(calls.every((p) => p.startsWith('C:'))).toBe(true);

    await store.follow(1, 'D:\\solo\\inner');
    // 'D:\solo' is unscripted (404): the node still marks expanded and the
    // error stays per-node, exactly like the shared tree's failure model.
    expect([...store.trees[1].expanded]).toEqual(['D:\\', 'D:\\solo']);
    expect(store.trees[1].errorOf('D:\\solo')?.code).toBe('notFound');
    expect(store.trees[1].selected).toBe('D:\\solo\\inner');
    expect([...store.trees[0].expanded]).toEqual(['C:\\', 'C:\\a']); // untouched
  });

  it('each pane owns a distinct TreeStore, disjoint from the global sidebar', () => {
    const store = new SeparateTreeStore(makeFactory(scripted({}).fetcher, ['C:\\']));
    expect(store.trees[0]).not.toBe(store.trees[1]);
    expect(store.trees[0]).not.toBe(separateTree.trees[0]);
  });
});

describe('cwdAncestors (tree-row path identity)', () => {
  it('keeps the drive root separator and drops deeper trailing separators', () => {
    expect(cwdAncestors('C:\\a\\sub')).toEqual(['C:\\', 'C:\\a']);
    expect(cwdAncestors('C:\\Projects')).toEqual(['C:\\']);
    expect(cwdAncestors('C:\\')).toEqual([]); // a drive root has no ancestors
  });

  it('handles POSIX paths without a drive letter', () => {
    expect(cwdAncestors('/home/user')).toEqual(['/home']);
    expect(cwdAncestors('/etc')).toEqual([]);
  });
});

describe('separate tree store — persistence', () => {
  it('round-trips toggles and expansion through storage', async () => {
    const { fetcher } = scripted({
      'C:\\': [entry('a', true)],
      'C:\\a': [entry('sub', true)],
      'D:\\': [],
    });
    const storage = memoryStorage();
    const first = new SeparateTreeStore(makeFactory(fetcher, ['C:\\', 'D:\\']), storage);
    first.toggle(0);
    first.trees[0].expanded.add('C:\\a'); // as the component effect would
    first.persist();
    const saved = JSON.parse(storage.getItem(KEY)!);
    expect(saved.enabled).toEqual([true, false]);
    expect(saved.expanded['0']).toEqual(['C:\\a']);

    // A fresh instance over the same storage restores toggle + expansion
    // (hydration refetches the restored node's children lazily).
    const second = new SeparateTreeStore(makeFactory(fetcher, ['C:\\', 'D:\\']), storage);
    expect(second.isEnabled(0)).toBe(true);
    expect(second.isEnabled(1)).toBe(false);
    await vi.waitFor(() => expect(second.trees[0].expanded.has('C:\\a')).toBe(true));
    await vi.waitFor(() =>
      expect(second.trees[0].childrenOf('C:\\a').map((e) => e.name)).toEqual(['sub']),
    );
  });

  it('recovers from malformed storage with the defaults (OFF)', () => {
    for (const bad of ['not json', '42', 'null', '{"enabled":"yes"}', '{"enabled":[true]}']) {
      const storage = memoryStorage({ [KEY]: bad });
      const store = new SeparateTreeStore(makeFactory(scripted({}).fetcher, ['C:\\']), storage);
      expect(store.isEnabled(0), bad).toBe(false);
      expect(store.isEnabled(1), bad).toBe(false);
    }
  });

  it('drops invalid expansion entries but keeps valid ones', () => {
    const raw = JSON.stringify({
      enabled: [false, true],
      expanded: { 0: ['C:\\ok', '', 42, 'x'.repeat(2000)], 1: ['D:\\fine'] },
    });
    const parsed = parseSeparateTreeState(raw);
    expect(parsed.enabled).toEqual([false, true]);
    expect(parsed.expanded[0]).toEqual(['C:\\ok']);
    expect(parsed.expanded[1]).toEqual(['D:\\fine']);
  });

  it('survives unavailable storage (persist is best-effort)', () => {
    const storage = memoryStorage({}, { failWrites: true });
    const store = new SeparateTreeStore(makeFactory(scripted({}).fetcher, ['C:\\']), storage);
    expect(() => store.toggle(0)).not.toThrow();
    expect(store.isEnabled(0)).toBe(true);
  });

  it('persistSeparateTreeState handles a missing storage gracefully', () => {
    expect(() =>
      persistSeparateTreeState({ enabled: [true, false], expanded: {} }, undefined),
    ).not.toThrow();
  });

  it('resetAll returns the singleton and storage to the OFF defaults', () => {
    separateTree.toggle(0);
    separateTree.trees[0].expanded.add('C:\\x');
    separateTree.persist();
    expect(JSON.parse(globalThis.localStorage!.getItem(KEY)!).enabled).toEqual([true, false]);
    separateTree.resetAll();
    expect(separateTree.isEnabled(0)).toBe(false);
    expect(separateTree.trees[0].expanded.size).toBe(0);
    expect(globalThis.localStorage!.getItem(KEY)).toBeNull();
  });
});
