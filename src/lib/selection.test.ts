import { describe, expect, it } from 'vitest';
import {
  compareNames,
  filterRows,
  moveCursor,
  selectAll,
  selectRange,
  selectSingle,
  sortRows,
  toggleSelect,
  type SortSpec,
} from './selection';
import { toRow } from './stores/paneStore.svelte';
import type { FileEntry } from './ipc/types';

const e = (name: string, opts: Partial<FileEntry> = {}): FileEntry => ({
  name,
  path: `C:\\t\\${name}`,
  isDir: false,
  isSymlink: false,
  size: 10,
  modifiedMs: 1000,
  hidden: false,
  ...opts,
});

const rows = (...es: FileEntry[]) => es.map(toRow);

describe('compareNames (numeric-aware)', () => {
  it('orders embedded numbers numerically', () => {
    expect(compareNames('file2.txt', 'file10.txt')).toBeLessThan(0);
    expect(compareNames('file10.txt', 'file2.txt')).toBeGreaterThan(0);
    expect(compareNames('file1.txt', 'file1a.txt')).toBeLessThan(0);
    expect(compareNames('v2', 'v2')).toBe(0);
  });

  it('is case-insensitive outside digits', () => {
    expect(compareNames('Apple.txt', 'apple.txt')).toBe(0);
    expect(compareNames('Zebra', 'apple')).toBeGreaterThan(0);
  });
});

describe('sortRows', () => {
  const data = rows(
    e('zebra.txt', { size: 5 }),
    e('src', { isDir: true }),
    e('Apple.txt', { size: 50 }),
    e('file10', { size: 100 }),
    e('file2', { size: 1 }),
  );

  it('folders first, then name ascending (D13 default)', () => {
    const out = sortRows(data, { key: 'name', dir: 1 });
    expect(out.map((r) => r.entry.name)).toEqual([
      'src',
      'Apple.txt',
      'file2',
      'file10',
      'zebra.txt',
    ]);
  });

  it('name descending still keeps folders first', () => {
    const out = sortRows(data, { key: 'name', dir: -1 });
    expect(out[0].entry.name).toBe('src');
    expect(out.map((r) => r.entry.name).slice(1)).toEqual([
      'zebra.txt',
      'file10',
      'file2',
      'Apple.txt',
    ]);
  });

  it('sorts by size with name tiebreak', () => {
    const out = sortRows(data, { key: 'size', dir: 1 });
    expect(out.map((r) => r.entry.name)).toEqual([
      'src',
      'file2',
      'zebra.txt',
      'Apple.txt',
      'file10',
    ]);
  });

  it('sorts by modified', () => {
    const data2 = rows(
      e('old.txt', { modifiedMs: 1 }),
      e('new.txt', { modifiedMs: 999 }),
      e('dir', { isDir: true, modifiedMs: 5 }),
    );
    const asc: SortSpec = { key: 'modified', dir: 1 };
    expect(sortRows(data2, asc).map((r) => r.entry.name)).toEqual(['dir', 'old.txt', 'new.txt']);
  });

  it('does not mutate the input', () => {
    const copy = [...data];
    sortRows(data, { key: 'size', dir: -1 });
    expect(data).toEqual(copy);
  });
});

describe('filterRows', () => {
  const data = rows(
    e('README.md'),
    e('.hidden', { hidden: true }),
    e('readme-notes.txt'),
    e('Cargo.toml'),
  );

  it('hides hidden entries by default (D08)', () => {
    expect(filterRows(data, {}).map((r) => r.entry.name)).not.toContain('.hidden');
    expect(filterRows(data, { showHidden: true }).map((r) => r.entry.name)).toContain('.hidden');
  });

  it('substring filter is case-insensitive on name', () => {
    const out = filterRows(data, { query: 'readme' }).map((r) => r.entry.name);
    expect(out).toEqual(['README.md', 'readme-notes.txt']);
  });

  it('empty query shows everything (visible)', () => {
    expect(filterRows(data, { query: '   ' })).toHaveLength(3);
  });
});

describe('selection model (path identity)', () => {
  const vis = rows(
    e('a', { path: 'C:\\t\\a' }),
    e('b', { path: 'C:\\t\\b' }),
    e('c', { path: 'C:\\t\\c' }),
    e('d', { path: 'C:\\t\\d' }),
  );

  it('single select replaces', () => {
    const cur = new Set(['C:\\t\\x']);
    expect([...selectSingle('C:\\t\\a')]).toEqual(['C:\\t\\a']);
    expect(cur.size).toBe(1);
  });

  it('ctrl toggle adds and removes', () => {
    let sel = new Set(['C:\\t\\a']);
    sel = toggleSelect(sel, 'C:\\t\\b');
    expect([...sel].sort()).toEqual(['C:\\t\\a', 'C:\\t\\b']);
    sel = toggleSelect(sel, 'C:\\t\\a');
    expect([...sel]).toEqual(['C:\\t\\b']);
  });

  it('shift range replaces; works reversed; additive unions', () => {
    const r1 = selectRange(vis, 'C:\\t\\a', 'C:\\t\\c');
    expect([...r1]).toEqual(['C:\\t\\a', 'C:\\t\\b', 'C:\\t\\c']);
    const r2 = selectRange(vis, 'C:\\t\\d', 'C:\\t\\b');
    expect([...r2]).toEqual(['C:\\t\\b', 'C:\\t\\c', 'C:\\t\\d']);
    const r3 = selectRange(vis, 'C:\\t\\c', 'C:\\t\\d', new Set(['C:\\t\\a']), true);
    expect([...r3].sort()).toEqual(['C:\\t\\a', 'C:\\t\\c', 'C:\\t\\d']);
  });

  it('selection keyed by path survives re-sort', () => {
    const sel = selectRange(vis, 'C:\\t\\a', 'C:\\t\\b');
    const shuffled = [...vis].reverse();
    const still = shuffled.filter((r) => sel.has(r.entry.path));
    expect([...still.map((r) => r.entry.path)]).toEqual(['C:\\t\\b', 'C:\\t\\a']);
  });

  it('select all', () => {
    expect(selectAll(vis)).toEqual(new Set(['C:\\t\\a', 'C:\\t\\b', 'C:\\t\\c', 'C:\\t\\d']));
  });
});

describe('moveCursor', () => {
  const vis = rows(e('a', { path: 'C:\\a' }), e('b', { path: 'C:\\b' }), e('c', { path: 'C:\\c' }));

  it('arrows move by one and clamp at the ends', () => {
    expect(moveCursor(vis, undefined, 1)).toBe('C:\\a');
    expect(moveCursor(vis, 'C:\\a', 1)).toBe('C:\\b');
    expect(moveCursor(vis, 'C:\\c', 1)).toBe('C:\\c');
    expect(moveCursor(vis, 'C:\\a', -1)).toBe('C:\\a');
  });

  it('cursor not in list resets to first', () => {
    expect(moveCursor(vis, 'C:\\gone', 1)).toBe('C:\\a');
  });

  it('empty list yields undefined', () => {
    expect(moveCursor([], 'x', 1)).toBeUndefined();
  });
});
