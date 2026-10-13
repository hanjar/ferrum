import { describe, expect, it } from 'vitest';
import { TreeStore, ancestorPaths, defaultDriveProvider, sortFolders } from './treeStore.svelte';
import { FsError, type FileEntry } from '../ipc/types';
import { mockHome } from '../mockData';

const entry = (name: string, isDir = false): FileEntry => ({
  name,
  path: `C:\\t\\${name}`,
  isDir,
  isSymlink: false,
  size: isDir ? 0 : 10,
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

function provider(roots: string[] = ['C:\\a', 'C:\\b']) {
  const calls: number[] = [];
  return {
    calls,
    provide: (): Promise<{ path: string; label: string; meta: string }[]> => {
      calls.push(1);
      return Promise.resolve(roots.map((path) => ({ path, label: path, meta: '' })));
    },
  };
}

describe('TreeStore roots (init)', () => {
  it('loads roots once from the injected drive provider', async () => {
    const { provide } = provider(['C:\\x']);
    const tree = new TreeStore(scripted().fetcher, provide);
    expect(tree.roots).toEqual([]);
    await tree.init();
    expect(tree.roots.map((r) => r.path)).toEqual(['C:\\x']);
    expect(tree.rootsLoading).toBe(false);
    expect(tree.rootsError).toBeNull();
  });

  it('stores a root error without throwing', async () => {
    const tree = new TreeStore(scripted().fetcher, () =>
      Promise.reject(new FsError({ code: 'unsupported', message: 'no desktop app' })),
    );
    await tree.init();
    expect(tree.roots).toEqual([]);
    expect(tree.rootsError?.code).toBe('unsupported');
  });
});

describe('TreeStore expand/collapse (lazy)', () => {
  const M: Record<string, FileEntry[]> = {
    'C:\\a': [entry('sub1', true), entry('notes.txt'), entry('sub2', true)],
    'C:\\b': [entry('only-file.txt')],
  };

  it('fetches children once; collapse just hides and re-expansion reuses cache', async () => {
    const { calls, fetcher } = scripted(M);
    const tree = new TreeStore(fetcher);

    await tree.toggle('C:\\a'); // expand → fetch
    expect(tree.expanded.has('C:\\a')).toBe(true);
    expect(tree.childrenOf('C:\\a').map((e) => e.name)).toEqual(['sub1', 'sub2']);
    expect(calls).toEqual(['C:\\a']);

    await tree.toggle('C:\\a'); // collapse
    expect(tree.expanded.has('C:\\a')).toBe(false);
    expect(tree.childrenOf('C:\\a')).toHaveLength(2); // cache kept

    await tree.toggle('C:\\a'); // re-expand → no second fetch
    expect(calls).toEqual(['C:\\a']);
    expect(tree.childrenOf('C:\\a')).toHaveLength(2);
  });

  it('refresh forces a re-fetch', async () => {
    const { calls, fetcher } = scripted(M);
    const tree = new TreeStore(fetcher);
    await tree.toggle('C:\\a');
    await tree.refresh('C:\\a');
    expect(calls).toEqual(['C:\\a', 'C:\\a']);
    expect(tree.expanded.has('C:\\a')).toBe(true);
  });

  it('stores a per-node error and retries on re-expand', async () => {
    const { calls, fetcher } = scripted(M);
    const tree = new TreeStore(fetcher);

    await tree.toggle('C:\\missing');
    expect(tree.expanded.has('C:\\missing')).toBe(true);
    expect(tree.errorOf('C:\\missing')?.code).toBe('notFound');
    expect(tree.childrenOf('C:\\missing')).toEqual([]);
    expect(tree.isLoading('C:\\missing')).toBe(false);

    // Unscripted again on re-expand: the failed node is retried, not cached.
    await tree.toggle('C:\\missing');
    await tree.toggle('C:\\missing');
    expect(calls).toEqual(['C:\\missing', 'C:\\missing']);
  });

  it('drops a stale response when a newer request for the node exists', async () => {
    let release!: (rows: FileEntry[]) => void;
    let first = true;
    const fetcher = (p: string): Promise<FileEntry[]> => {
      if (p === 'C:\\a' && first) {
        first = false;
        return new Promise((res) => (release = res));
      }
      return Promise.resolve([entry('fresh', true)]);
    };
    const tree = new TreeStore(fetcher);

    void tree.toggle('C:\\a'); // stays pending
    await tree.refresh('C:\\a');
    expect(tree.childrenOf('C:\\a').map((e) => e.name)).toEqual(['fresh']);

    release([entry('stale', true)]); // late: must be dropped
    await new Promise((r) => setTimeout(r, 0));
    expect(tree.childrenOf('C:\\a').map((e) => e.name)).toEqual(['fresh']);
  });
});

describe('TreeStore node semantics', () => {
  it('filters to folders and sorts folders case-insensitively', async () => {
    const tree = new TreeStore(
      scripted({
        'C:\\a': [
          entry('Zebra', true),
          entry('apple', true),
          entry('Banana', true),
          entry('file.txt'),
        ],
      }).fetcher,
    );
    await tree.toggle('C:\\a');
    expect(tree.childrenOf('C:\\a').map((e) => e.name)).toEqual(['apple', 'Banana', 'Zebra']);
    expect(tree.hasFetched('C:\\a')).toBe(true);
    expect(tree.hasFetched('C:\\never')).toBe(false);
  });

  it('select tracks the highlighted path', () => {
    const tree = new TreeStore(scripted().fetcher);
    expect(tree.selected).toBeNull();
    tree.select('C:\\a');
    expect(tree.selected).toBe('C:\\a');
  });
});

describe('ancestorPaths (follow-to-cwd keys)', () => {
  it('matches listed row paths — trailing separator only on the drive root', () => {
    expect(ancestorPaths('C:\\Users\\you')).toEqual(['C:\\', 'C:\\Users']);
    expect(ancestorPaths('C:\\a')).toEqual(['C:\\']);
    expect(ancestorPaths('C:\\')).toEqual([]);
    expect(ancestorPaths('/home/user')).toEqual(['/home']);
  });

  it('followActivePath expands the listed ancestors and selects the path', async () => {
    const prov = provider(['C:\\']);
    const { calls, fetcher } = scripted({
      'C:\\': [entry('a', true)],
      'C:\\a': [entry('sub', true)],
    });
    const tree = new TreeStore(fetcher, prov.provide);
    await tree.init();
    await tree.followActivePath('C:\\a\\sub');
    expect(calls).toContain('C:\\a');
    expect(tree.childrenOf('C:\\a').map((e) => e.name)).toEqual(['sub']);
    expect(tree.selected).toBe('C:\\a\\sub');
  });
});

describe('sortFolders', () => {
  it('keeps only directories in numeric-aware name order', () => {
    const out = sortFolders([
      entry('file2', true),
      entry('file10', true),
      entry('doc.txt'),
      entry('img.png'),
    ]);
    expect(out.map((e) => e.name)).toEqual(['file2', 'file10']);
  });
});

describe('defaultDriveProvider (browser dev)', () => {
  it('returns the mockHome entries as pseudo-roots', async () => {
    const roots = await defaultDriveProvider();
    expect(roots.map((r) => r.path)).toEqual([...mockHome]);
    expect(roots[0].label).toBe('Projects');
    expect(roots[0].meta).toBe('C:');
  });
});
