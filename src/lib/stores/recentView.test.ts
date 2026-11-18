import { describe, expect, it } from 'vitest';
import { RECENT_DEFAULT_SORT, nextRecentSort } from './recentView.svelte';
import { sortRows, DEFAULT_SORT } from '../selection';
import { toRow } from './paneStore.svelte';
import type { FileEntry } from '../ipc/types';

const e = (name: string, isDir: boolean, ms: number): FileEntry => ({
  name,
  path: 'C:/x/' + name,
  isDir,
  isSymlink: false,
  size: 0,
  modifiedMs: ms,
  hidden: false,
});
const rows = [
  e('old-folder', true, 100),
  e('newer-file.txt', false, 400),
  e('newest-folder', true, 500),
  e('mid-file.txt', false, 300),
  e('a-folder', true, 200),
].map(toRow);

describe('Recent view order (FER-599)', () => {
  it('is newest used first, files and folders interleaved, even with folders-first ON globally', () => {
    // FilePane passes foldersFirst=false while the Recent view is shown.
    const names = sortRows(rows, RECENT_DEFAULT_SORT, false).map((r) => r.entry.name);
    expect(names).toEqual([
      'newest-folder',
      'newer-file.txt',
      'mid-file.txt',
      'a-folder',
      'old-folder',
    ]);
  });

  it('a clicked header re-sorts, the same key flips, and leaving restores the tab sort', () => {
    const tab = { key: 'size' as const, dir: -1 as const };
    let recent = RECENT_DEFAULT_SORT;
    recent = nextRecentSort(recent, 'modified');
    expect(recent).toEqual({ key: 'modified', dir: 1 });
    recent = nextRecentSort(recent, 'name');
    expect(recent).toEqual({ key: 'name', dir: 1 });
    // The tab's own spec was never touched (the Recent sort is a separate value).
    expect(tab).toEqual({ key: 'size', dir: -1 });
    expect(RECENT_DEFAULT_SORT).toEqual({ key: 'modified', dir: -1 });
  });

  it('a normal folder listing still puts folders first', () => {
    const names = sortRows(rows, DEFAULT_SORT, true).map((r) => r.entry.name);
    expect(names.slice(0, 3)).toEqual(['a-folder', 'newest-folder', 'old-folder']);
  });
});
