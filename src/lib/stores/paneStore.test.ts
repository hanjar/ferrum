import { types } from 'node:util';
import { describe, expect, it, beforeEach, afterEach, vi } from 'vitest';
import { PaneStore, baseName, setTabsChangedHook, toRow, volumeRoot } from './paneStore.svelte';
import { FsError, type FileEntry } from '../ipc/types';
import { ui, TABS_DEFAULTS } from '../uiState.svelte';

const entry = (name: string, isDir = false, size = 10): FileEntry => ({
  name,
  path: `C:\\t\\${name}`,
  isDir,
  isSymlink: false,
  size,
  modifiedMs: 1790512000000,
  hidden: false,
});

/** Auto-resolving fetcher scripted per path; unscripted paths 404. */
function scripted(map: Record<string, FileEntry[]> = {}) {
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

describe('PaneStore tabs and history (task 07)', () => {
  const M: Record<string, FileEntry[]> = {
    'C:\\a': [entry('a1')],
    'C:\\b': [entry('b1')],
    'C:\\c': [entry('c1')],
    'C:\\d': [entry('d1')],
  };

  it('records history; navigating truncates the forward stack', async () => {
    const pane = new PaneStore(scripted(M).fetcher, 'C:\\home');
    await pane.newTab('C:\\a');
    await pane.open('C:\\b');
    await pane.open('C:\\c');
    expect(pane.path).toBe('C:\\c');
    expect(pane.canBack).toBe(true);

    await pane.back();
    expect(pane.path).toBe('C:\\b');
    expect(pane.canFwd).toBe(true);

    await pane.open('C:\\d'); // from mid-history
    expect(pane.canFwd).toBe(false);
    expect(pane.path).toBe('C:\\d');
  });

  it('forward returns to the visited path', async () => {
    const pane = new PaneStore(scripted(M).fetcher, 'C:\\home');
    await pane.newTab('C:\\a');
    await pane.open('C:\\b');
    await pane.back();
    expect(pane.path).toBe('C:\\a');
    await pane.forward();
    expect(pane.path).toBe('C:\\b');
    expect(pane.canBack).toBe(true);
  });

  it('tabs keep independent history and listings', async () => {
    const pane = new PaneStore(
      scripted({
        'C:\\one': [entry('one-file')],
        'C:\\one\\sub': [entry('sub-file')],
        'C:\\two': [entry('two-file')],
      }).fetcher,
      'C:\\home',
    );
    await pane.newTab('C:\\one');
    await pane.open('C:\\one\\sub');

    await pane.newTab('C:\\two');
    expect(pane.tabs.length).toBe(2);
    expect(pane.path).toBe('C:\\two');
    expect(pane.rows.map((r) => r.entry.name)).toEqual(['two-file']);
    expect(pane.canBack).toBe(false); // fresh tab: no history

    pane.selectTab(0);
    expect(pane.path).toBe('C:\\one\\sub');
    expect(pane.rows.map((r) => r.entry.name)).toEqual(['sub-file']);
    expect(pane.canBack).toBe(true); // tab 0 kept its history
  });

  it('closing a tab focuses the right neighbor, else the left one', async () => {
    const pane = new PaneStore(scripted(M).fetcher, 'C:\\h');
    await pane.newTab('C:\\a');
    await pane.newTab('C:\\b');
    await pane.newTab('C:\\c');
    expect(pane.current).toBe(2);

    await pane.closeTab(2); // close current (last) → left neighbor
    expect(pane.current).toBe(1);
    expect(pane.path).toBe('C:\\b');

    pane.selectTab(0); // tabs are now [a, b]
    await pane.closeTab(0); // right neighbor takes its index
    expect(pane.current).toBe(0);
    expect(pane.path).toBe('C:\\b');
  });

  it('closing the LAST tab resets it to home instead of dying (D19)', async () => {
    const pane = new PaneStore(scripted(M).fetcher, 'C:\\home');
    await pane.newTab('C:\\a');
    await pane.open('C:\\b');

    await pane.closeTab(0);
    expect(pane.tabs.length).toBe(1);
    expect(pane.path).toBe('C:\\home');
    expect(pane.canBack).toBe(false);
    expect(pane.canFwd).toBe(false);
  });

  it('goUp navigates to parent', async () => {
    const pane = new PaneStore(scripted(M).fetcher, 'C:\\home');
    await pane.newTab('C:\\home\\sub\\leaf');
    await pane.goUp();
    expect(pane.path).toBe('C:\\home\\sub\\'); // parentOf keeps root-style separators
    await pane.goUp();
    expect(pane.path).toBe('C:\\home\\');
  });

  it('stale responses never overwrite a newer tab state', async () => {
    // Manual control: first request stays pending forever.
    let releaseFirst!: (rows: FileEntry[]) => void;
    let first = true;
    const fetcher = (p: string): Promise<FileEntry[]> => {
      if (p === 'C:\\a' && first) {
        first = false;
        return new Promise((res) => (releaseFirst = res));
      }
      return Promise.resolve([entry(p === 'C:\\b' ? 'from-b' : `stale-${p}`)]);
    };
    const pane = new PaneStore(fetcher, 'C:\\h');

    void pane.newTab('C:\\a'); // stays pending
    const p2 = pane.open('C:\\b');
    await p2;
    expect(pane.rows.map((r) => r.entry.name)).toEqual(['from-b']);

    releaseFirst([entry('from-a-stale')]); // late: must be dropped
    await new Promise((r) => setTimeout(r, 0));
    expect(pane.rows.map((r) => r.entry.name)).toEqual(['from-b']);
  });
});

describe('activity visit reporting (phase-4 ACT-03)', () => {
  const V: Record<string, FileEntry[]> = {
    'C:\\a': [entry('a1')],
    'C:\\b': [entry('b1')],
    'C:\\c': [entry('c1')],
  };

  it('reports open/back/forward, never refresh, newTab, or failed loads', async () => {
    const pane = new PaneStore(scripted(V).fetcher, 'C:\\home');
    const visits: string[] = [];
    pane.onVisit = (p) => visits.push(p);

    await pane.newTab('C:\\a'); // startup restore: no visit
    await pane.open('C:\\b');
    await pane.open('C:\\c');
    await pane.back();
    await pane.forward();
    await pane.refresh(); // technical reload: no visit
    expect(visits).toEqual(['C:\\b', 'C:\\c', 'C:\\b', 'C:\\c']);
  });

  it('failed navigation records nothing', async () => {
    const pane = new PaneStore(scripted({ 'C:\\a': [entry('a1')] }).fetcher, 'C:\\home');
    const visits: string[] = [];
    pane.onVisit = (p) => visits.push(p);
    await pane.newTab('C:\\a');
    await pane.open('C:\\missing');
    expect(pane.error).toBeInstanceOf(FsError);
    expect(visits).toEqual([]);
  });

  it('superseded stale loads do not report a visit', async () => {
    let releaseFirst!: (rows: FileEntry[]) => void;
    let first = true;
    const fetcher = (p: string): Promise<FileEntry[]> => {
      if (p === 'C:\\a' && first) {
        first = false;
        return new Promise((res) => (releaseFirst = res));
      }
      return Promise.resolve([entry(`row-${p}`)]);
    };
    const pane = new PaneStore(fetcher, 'C:\\h');
    const visits: string[] = [];
    pane.onVisit = (p) => visits.push(p);
    void pane.newTab('C:\\a'); // pending
    await pane.open('C:\\b'); // supersedes it
    releaseFirst([entry('stale')]);
    await new Promise((r) => setTimeout(r, 0));
    expect(visits).toEqual(['C:\\b']);
  });
});

describe('FER-395 tab position, restore and session hook', () => {
  const M: Record<string, FileEntry[]> = {
    'C:\\a': [entry('a1')],
    'C:\\b': [entry('b1')],
    'C:\\c': [entry('c1')],
    'C:\\d': [entry('d1')],
  };

  beforeEach(() => {
    ui.tabs = { ...TABS_DEFAULTS };
    setTabsChangedHook(() => {});
  });

  afterEach(() => {
    ui.tabs = { ...TABS_DEFAULTS };
    setTabsChangedHook(() => {});
  });

  it('openNextToCurrent inserts right after the ACTIVE tab and activates it', async () => {
    ui.tabs.openNextToCurrent = true;
    const pane = new PaneStore(scripted(M).fetcher, 'C:\\home');
    await pane.newTab('C:\\a');
    await pane.newTab('C:\\b'); // current: [a, b], active 1
    pane.selectTab(0);
    await pane.newTab('C:\\c');
    expect(pane.tabs.map((t) => t.path)).toEqual(['C:\\a', 'C:\\c', 'C:\\b']);
    expect(pane.current).toBe(1);
    // Regression: tab/path still reference the ACTIVE tab after the insert.
    expect(pane.tab.path).toBe('C:\\c');
    expect(pane.path).toBe('C:\\c');
  });

  it('pref off appends at the end (classic behavior)', async () => {
    ui.tabs.openNextToCurrent = false;
    const pane = new PaneStore(scripted(M).fetcher, 'C:\\home');
    await pane.newTab('C:\\a');
    await pane.newTab('C:\\b');
    pane.selectTab(0);
    await pane.newTab('C:\\c');
    expect(pane.tabs.map((t) => t.path)).toEqual(['C:\\a', 'C:\\b', 'C:\\c']);
    expect(pane.current).toBe(2);
    expect(pane.path).toBe('C:\\c');
  });

  it('restore opens saved paths in order, selects saved current, tolerates dead paths', async () => {
    const pane = new PaneStore(
      scripted({ 'C:\\a': [entry('a1')], 'C:\\b': [entry('b1')] }).fetcher,
      'C:\\home',
    );
    await pane.restore(['C:\\a', 'C:\\gone', 'C:\\b'], 2);
    expect(pane.tabs.map((t) => t.path)).toEqual(['C:\\a', 'C:\\gone', 'C:\\b']);
    expect(pane.current).toBe(2);
    expect(pane.tab.error).toBeNull(); // active tab loaded fine
    pane.selectTab(1);
    expect(pane.error?.code).toBe('notFound'); // dead location surfaces in ITS tab
    pane.selectTab(0);
    expect(pane.rows.map((r) => r.entry.name)).toEqual(['a1']);
  });

  it('restore repairs legacy drive-relative tabs (C: and /C:) to C:\\', async () => {
    const pane = new PaneStore(scripted({ 'C:\\': [entry('r1')] }).fetcher, 'C:\\home');
    await pane.restore(['/C:', 'C:'], 0);
    expect(pane.tabs.map((t) => t.path)).toEqual(['C:\\', 'C:\\']);
    expect(pane.rows.map((r) => r.entry.name)).toEqual(['r1']);
  });

  it('restore clamps an out-of-range current and no-ops on nothing restorable', async () => {
    const pane = new PaneStore(scripted(M).fetcher, 'C:\\home');
    await pane.restore(['C:\\a', 'C:\\b'], 9);
    expect(pane.current).toBe(0);

    const untouched = new PaneStore(scripted(M).fetcher, 'C:\\home');
    await untouched.restore([], 0);
    await untouched.restore([''], 0);
    expect(untouched.tabs.length).toBe(0);
  });

  it('every tab/path mutation notifies the session hook', async () => {
    const hits = vi.fn();
    setTabsChangedHook(hits);
    const pane = new PaneStore(scripted(M).fetcher, 'C:\\home');
    await pane.newTab('C:\\a'); // insert + load
    await pane.open('C:\\b');
    await pane.back();
    await pane.forward();
    pane.selectTab(0);
    await pane.closeTab(0);
    expect(hits.mock.calls.length).toBeGreaterThanOrEqual(7);
  });
});

describe('baseName', () => {
  it('last segment wins; roots survive', () => {
    expect(baseName('C:\\Users\\you')).toBe('you');
    expect(baseName('C:\\')).toBe('C:');
    expect(baseName('/home/arif')).toBe('arif');
    expect(baseName('/')).toBe('/');
    expect(baseName('D:\\a\\b\\')).toBe('b');
  });
});

describe('toRow', () => {
  it('derives display fields once', () => {
    const row = toRow(entry('main.rs', false, 88_264));
    expect(row.kind).toBe('code');
    expect(row.sizeText).toBe('86.2 KB');
    expect(row.modText).toMatch(/^\d{4}-\d{2}-\d{2} \d{2}:\d{2}$/);
  });

  it('directories have empty size text', () => {
    expect(toRow(entry('src', true)).sizeText).toBe('');
  });
});

describe('error normalization', () => {
  it('non-FsError rejections become FsError(io)', async () => {
    const pane = new PaneStore(() => Promise.reject('raw'), 'C:\\h');
    await pane.newTab('C:\\x');
    expect(pane.error).toBeInstanceOf(FsError);
    expect(pane.error?.code).toBe('io');
  });

  it('structured rejections pass through', async () => {
    const pane = new PaneStore(
      () => Promise.reject(new FsError({ code: 'permissionDenied', message: 'no' })),
      'C:\\h',
    );
    await pane.newTab('C:\\locked');
    expect(pane.error?.code).toBe('permissionDenied');
  });
});

describe('PaneStore listing rows stay out of the $state deep proxy (FER-455)', () => {
  it('rows and their entries are raw objects, not Svelte proxies', async () => {
    const { fetcher } = scripted({ 'C:\\big': [entry('a'), entry('b')] });
    const pane = new PaneStore(fetcher, 'C:\\h');
    await pane.newTab('C:\\big');
    expect(pane.rows).toHaveLength(2);
    expect(types.isProxy(pane.rows)).toBe(false);
    expect(types.isProxy(pane.rows[0])).toBe(false);
    expect(types.isProxy(pane.rows[0].entry)).toBe(false);
    // The tab itself is still reactive: replacing rows is observable through the tab proxy.
    expect(types.isProxy(pane.tab)).toBe(true);
  });
});

describe('PaneStore missing-folder recovery (FER-457)', () => {
  const alive: Record<string, FileEntry[]> = {
    'C:\\': [entry('r1')],
    'C:\\a\\': [entry('a1')],
    'C:\\home': [entry('h1')],
  };

  it('restore climbs to the nearest existing parent and sets a notice', async () => {
    const pane = new PaneStore(scripted(alive).fetcher, 'C:\\home');
    await pane.restore(['C:\\a\\gone\\deeper'], 0);
    expect(pane.path).toBe('C:\\a\\');
    expect(pane.error).toBeNull();
    expect(pane.rows).toHaveLength(1);
    expect(pane.notice).toContain('C:\\a\\gone\\deeper');
  });

  it('climbs all the way to the volume root when only the root exists', async () => {
    const pane = new PaneStore(scripted({ 'C:\\': [entry('r1')] }).fetcher, 'C:\\home');
    await pane.restore(['C:\\gone\\x\\y'], 0);
    expect(pane.path).toBe('C:\\');
    expect(pane.notice).not.toBeNull();
  });

  it('falls back to home for paths without a volume (mock/posix) when no parent exists', async () => {
    const pane = new PaneStore(scripted({ '/home': [entry('h1')] }).fetcher, '/home');
    await pane.restore(['/gone/x'], 0);
    expect(pane.path).toBe('/home');
    expect(pane.notice).not.toBeNull();
  });

  it('shows the error when nothing, not even home, can be opened', async () => {
    const pane = new PaneStore(scripted({}).fetcher, '/home');
    await pane.restore(['/gone'], 0);
    expect(pane.error?.code).toBe('notFound');
    expect(pane.notice).toBeNull();
  });

  it('a missing DRIVE keeps the original path and error (no climb, tab not rewritten)', async () => {
    const s = scripted({ 'C:\\home': [entry('h1')] });
    const pane = new PaneStore(s.fetcher, 'C:\\home');
    await pane.restore(['E:\\data\\keep'], 0);
    expect(pane.path).toBe('E:\\data\\keep');
    expect(pane.error?.code).toBe('notFound');
    expect(pane.notice).toBeNull();
    // one try for the path itself, one for the drive root; no parent-by-parent probing, no home fallback
    expect(s.calls).toEqual(['E:\\data\\keep', 'E:\\']);
  });

  it('an unreachable UNC share is probed once at the share root, never as \\\\server\\ or \\\\', async () => {
    const s = scripted({});
    const pane = new PaneStore(s.fetcher, 'C:\\home');
    await pane.restore(['\\\\srv\\share\\dir\\sub'], 0);
    expect(pane.path).toBe('\\\\srv\\share\\dir\\sub');
    expect(pane.error?.code).toBe('notFound');
    expect(pane.notice).toBeNull();
    expect(s.calls).toEqual(['\\\\srv\\share\\dir\\sub', '\\\\srv\\share\\']);
  });

  it('volumeRoot recognises the long-path forms \\\\?\\X:\\ and \\\\?\\UNC\\ (FER-462 R2)', () => {
    expect(volumeRoot('\\\\?\\D:\\data\\x')).toBe('\\\\?\\D:\\');
    expect(volumeRoot('//?/d:/data')).toBe('\\\\?\\d:\\');
    expect(volumeRoot('\\\\?\\UNC\\srv\\share\\dir\\sub')).toBe('\\\\?\\UNC\\srv\\share\\');
    expect(volumeRoot('\\\\?\\unc\\srv\\share')).toBe('\\\\?\\UNC\\srv\\share\\');
    // Unchanged: plain forms, and device namespaces that are not a volume.
    expect(volumeRoot('C:\\a')).toBe('C:\\');
    expect(volumeRoot('\\\\srv\\share\\a')).toBe('\\\\srv\\share\\');
    expect(volumeRoot('\\\\?\\GLOBALROOT\\Device\\x')).toBeNull();
    expect(volumeRoot('\\\\?\\UNC\\')).toBeNull();
    expect(volumeRoot('\\\\.\\pipe\\x')).toBeNull();
    expect(volumeRoot('/home/x')).toBeNull();
  });

  it('a missing \\\\?\\ DRIVE keeps the original path and probes only its root (FER-462 R2)', async () => {
    const s = scripted({ 'C:\\home': [entry('h1')] });
    const pane = new PaneStore(s.fetcher, 'C:\\home');
    await pane.restore(['\\\\?\\E:\\data\\keep'], 0);
    expect(pane.path).toBe('\\\\?\\E:\\data\\keep');
    expect(pane.error?.code).toBe('notFound');
    expect(pane.notice).toBeNull();
    expect(s.calls).toEqual(['\\\\?\\E:\\data\\keep', '\\\\?\\E:\\']);
  });

  it('a missing folder on a reachable \\\\?\\UNC\\ share climbs but stops at the share root', async () => {
    const root = '\\\\?\\UNC\\srv\\share\\';
    const s = scripted({ [root]: [entry('s1')] });
    const pane = new PaneStore(s.fetcher, 'C:\\home');
    await pane.restore([`${root}gone\\x`], 0);
    expect(pane.path).toBe(root);
    expect(pane.notice).not.toBeNull();
    expect(s.calls.every((c) => c.startsWith(root))).toBe(true);
  });

  it('a missing folder on a reachable UNC share climbs but stops at the share root', async () => {
    const pane = new PaneStore(scripted({ '\\\\srv\\share\\': [entry('s1')] }).fetcher, 'C:\\home');
    await pane.restore(['\\\\srv\\share\\gone\\x'], 0);
    expect(pane.path).toBe('\\\\srv\\share\\');
    expect(pane.notice).not.toBeNull();
  });

  it('a drive that disappears while open keeps the tab path on refresh', async () => {
    const map: Record<string, FileEntry[]> = { 'C:\\home': [entry('h1')], 'E:\\': [], 'E:\\d': [] };
    const pane = new PaneStore(scripted(map).fetcher, 'C:\\home');
    await pane.newTab('E:\\d');
    delete map['E:\\'];
    delete map['E:\\d'];
    await pane.refresh();
    expect(pane.path).toBe('E:\\d');
    expect(pane.error?.code).toBe('notFound');
  });

  it('a folder removed while open climbs on refresh', async () => {
    const map: Record<string, FileEntry[]> = { ...alive, 'C:\\a\\sub': [entry('s1')] };
    const pane = new PaneStore(scripted(map).fetcher, 'C:\\home');
    await pane.newTab('C:\\a\\sub');
    expect(pane.error).toBeNull();
    delete map['C:\\a\\sub'];
    await pane.refresh();
    expect(pane.path).toBe('C:\\a\\');
    expect(pane.notice).toContain('no longer exists');
  });

  it('a deliberate open of a missing folder still reports the error (no silent climb)', async () => {
    const pane = new PaneStore(scripted(alive).fetcher, 'C:\\home');
    await pane.newTab('C:\\a\\');
    await pane.open('C:\\a\\nope');
    expect(pane.path).toBe('C:\\a\\nope');
    expect(pane.error?.code).toBe('notFound');
  });
});
