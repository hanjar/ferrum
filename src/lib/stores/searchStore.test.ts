/**
 * F2-16 — search store: progressive events routed by id, stale-search
 * drops, saved-search persistence (injected storage), and result navigation.
 */
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import {
  loadSavedSearches,
  persistSavedSearches,
  searchProgressPct,
  searchStore,
  type SavedSearch,
} from './searchStore.svelte';
import { registerPane, type PaneApi } from './panesRegistry.svelte';
import type { FileEntry } from '../ipc/types';

function entry(name: string, path?: string): FileEntry {
  return {
    name,
    path: path ?? `C:\\dir\\${name}`,
    isDir: false,
    isSymlink: false,
    size: 1,
    modifiedMs: 1,
    hidden: false,
  };
}

function fakePane(path = 'C:\\dir'): PaneApi & { opened: string[] } {
  const state = { opened: [] as string[] };
  return {
    path,
    opened: state.opened,
    selectedPaths: () => [],
    cursorEntry: () => undefined,
    refresh: () => {},
    newTab: () => {},
    closeCurrentTab: () => {},
    goUp: () => {},
    openCursorEntry: () => {},
    openPath: (p: string) => {
      state.opened.push(p);
    },
  };
}

/** In-memory Storage stand-in (jsdom's localStorage persists between tests). */
function fakeStorage(): Storage {
  const map = new Map<string, string>();
  return {
    get length() {
      return map.size;
    },
    clear: () => map.clear(),
    getItem: (k: string) => map.get(k) ?? null,
    key: (i: number) => [...map.keys()][i] ?? null,
    removeItem: (k: string) => map.delete(k),
    setItem: (k: string, v: string) => map.set(k, v),
  };
}

let pane: ReturnType<typeof fakePane>;
let storage: Storage;

beforeEach(() => {
  searchStore.reset();
  pane = fakePane();
  registerPane(0, pane);
  registerPane(1, fakePane());
  storage = fakeStorage();
});

afterEach(() => {
  searchStore.reset();
});

describe('progressive events and staleness', () => {
  it('routes batches into results only for the current search id', () => {
    searchStore.testInFlight('search-1');
    searchStore.testEvent({ id: 'search-1', entries: [entry('a.txt')], done: false });
    searchStore.testEvent({ id: 'search-OTHER', entries: [entry('b.txt')], done: false });
    expect(searchStore.results).toHaveLength(1);
    expect(searchStore.running).toBe(true);
  });

  it('the final event closes the search and records stats or errors', () => {
    searchStore.testInFlight('search-1');
    searchStore.testEvent({
      id: 'search-1',
      entries: [],
      done: true,
      stats: {
        matched: 2,
        visited: 100,
        errors: 1,
        truncated: false,
        cancelled: false,
        timedOut: false,
        durationMs: 42,
      },
    });
    expect(searchStore.running).toBe(false);
    expect(searchStore.stats?.visited).toBe(100);
    expect(searchStore.stats?.timedOut).toBe(false);
    expect(searchStore.errorText).toBe('');

    searchStore.reset();
    searchStore.testInFlight('search-2');
    searchStore.testEvent({ id: 'search-2', entries: [], done: true, error: 'boom' });
    expect(searchStore.errorText).toBe('boom');
  });

  it('events after reset (stale search) never leak into a new run', () => {
    searchStore.testInFlight('search-1');
    searchStore.reset();
    searchStore.testInFlight('search-2');
    searchStore.testEvent({ id: 'search-1', entries: [entry('late.txt')], done: true });
    expect(searchStore.results).toHaveLength(0);
    expect(searchStore.running).toBe(true); // search-2 keeps running
  });
});

describe('result navigation', () => {
  it('jumps the active pane to the folder containing a result', () => {
    searchStore.openResultFolder('C:\\deep\\nested\\found.txt');
    expect(pane.opened).toEqual(['C:\\deep\\nested']);
    searchStore.openResultFolder('C:\\root.txt');
    expect(pane.opened[1]).toBe('C:\\');
  });
});

describe('saved searches', () => {
  it('persist round-trip through a storage backend', () => {
    const saved: SavedSearch[] = [
      { name: 'logs', root: 'C:\\log', options: { ...searchStore.options, pattern: '*.log' } },
    ];
    persistSavedSearches(saved, storage);
    const back = loadSavedSearches(storage);
    expect(back).toHaveLength(1);
    expect(back[0].name).toBe('logs');
    expect(back[0].options.pattern).toBe('*.log');
  });

  it('corrupt or foreign storage content yields an empty list', () => {
    storage.setItem('ferrum.savedSearches.v1', 'not json');
    expect(loadSavedSearches(storage)).toEqual([]);
    storage.setItem('ferrum.savedSearches.v1', '{"nope":true}');
    expect(loadSavedSearches(storage)).toEqual([]);
    storage.setItem('ferrum.savedSearches.v1', '[{"name":42}]');
    expect(loadSavedSearches(storage)).toEqual([]);
  });

  it('saveCurrent keeps newest first, de-duplicates by name, caps at 50', () => {
    searchStore.root = 'C:\\dir';
    searchStore.options = { ...searchStore.options, pattern: '*.txt' };
    searchStore.saveName = 'first';
    searchStore.saveCurrent();
    searchStore.options = { ...searchStore.options, pattern: '*.md' };
    searchStore.saveName = 'second';
    searchStore.saveCurrent();
    searchStore.saveName = 'first'; // same name replaces with CURRENT options
    searchStore.saveCurrent();
    expect(searchStore.saved.map((s) => s.name)).toEqual(['first', 'second']);
    expect(searchStore.saved[0].options.pattern).toBe('*.md'); // replaced snapshot
    for (let i = 0; i < 60; i++) {
      searchStore.saveName = `bulk-${i}`;
      searchStore.saveCurrent();
    }
    expect(searchStore.saved.length).toBe(50);
  });

  it('applySaved restores root and options; removeSaved drops by name', () => {
    searchStore.root = 'C:\\dir';
    searchStore.options = { ...searchStore.options, pattern: '*.log', caseSensitive: true };
    searchStore.saveName = 'logs';
    searchStore.saveCurrent();
    // disturb the live state, then restore from the saved snapshot
    searchStore.root = 'C:\\elsewhere';
    searchStore.options = { ...searchStore.options, pattern: '', caseSensitive: false };
    const saved = searchStore.saved[0];
    searchStore.applySaved(saved);
    expect(searchStore.root).toBe('C:\\dir');
    expect(searchStore.options.pattern).toBe('*.log');
    expect(searchStore.options.caseSensitive).toBe(true);
    searchStore.removeSaved('logs');
    expect(searchStore.saved).toHaveLength(0);
  });

  it('saving without a name does nothing', () => {
    searchStore.saveName = '   ';
    searchStore.saveCurrent();
    expect(searchStore.saved).toHaveLength(0);
  });
});

describe('searchProgressPct', () => {
  it('is bounded and handles the default cap', () => {
    expect(searchProgressPct(500, undefined)).toBe(50);
    expect(searchProgressPct(5000, undefined)).toBe(100);
    expect(searchProgressPct(1, 0)).toBe(0);
  });
});
