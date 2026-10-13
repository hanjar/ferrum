import { beforeEach, describe, expect, it } from 'vitest';
import { viewState } from './viewStateStore.svelte';
import { sortRows, compareRows, type SortSpec } from '../selection';
import { toRow } from './paneStore.svelte';
import type { FileEntry } from '../ipc/types';

function row(name: string, over: Partial<FileEntry> = {}) {
  const e: FileEntry = {
    name,
    path: `C:\\d\\${name}`,
    isDir: false,
    isSymlink: false,
    size: 1,
    modifiedMs: 1,
    hidden: false,
    ...over,
  };
  return toRow(e);
}

const ROWS = [
  row('b.txt', { size: 5, modifiedMs: 30 }),
  row('a.txt', { size: 9, modifiedMs: 20 }),
  row('c.md', { size: 7, modifiedMs: 10 }),
];

beforeEach(() => {
  // reset both panes
  viewState.setSort(0, 'name', 1);
  viewState.setSort(1, 'name', 1);
  viewState.setViewMode(0, 'full');
  viewState.setViewMode(1, 'full');
  viewState.setFilterMode(0, 'all');
  viewState.setFilterMode(1, 'all');
  if (viewState.reversed(0)) viewState.toggleReversed(0);
  if (viewState.reversed(1)) viewState.toggleReversed(1);
});

describe('per-pane view state (F2-15)', () => {
  it('is independent between panes', () => {
    viewState.setViewMode(0, 'brief');
    expect(viewState.viewMode(0)).toBe('brief');
    expect(viewState.viewMode(1)).toBe('full');
    viewState.toggleReversed(1);
    expect(viewState.reversed(1)).toBe(true);
    expect(viewState.reversed(0)).toBe(false);
  });

  it('same-key sort toggles direction, new key starts ascending', () => {
    viewState.setSort(0, 'size');
    expect(viewState.sort(0)).toEqual({ key: 'size', dir: 1 });
    viewState.setSort(0, 'size');
    expect(viewState.sort(0)).toEqual({ key: 'size', dir: -1 });
    viewState.setSort(0, 'name');
    expect(viewState.sort(0)).toEqual({ key: 'name', dir: 1 });
    // pane 1 untouched
    expect(viewState.sort(1)).toEqual({ key: 'name', dir: 1 });
  });

  it('filter mode and pattern can be set together or apart', () => {
    viewState.setFilterMode(0, 'custom', '*.md');
    expect(viewState.filterMode(0)).toBe('custom');
    expect(viewState.filterPattern(0)).toBe('*.md');
    viewState.setFilterMode(0, 'all');
    expect(viewState.filterMode(0)).toBe('all');
    expect(viewState.filterPattern(0)).toBe('*.md'); // pattern kept for next time
  });
});

describe('extension and unsorted sorting', () => {
  it('extension sorts by extension then name; reverse flips', () => {
    const spec: SortSpec = { key: 'extension', dir: 1 };
    expect(sortRows(ROWS, spec).map((r) => r.entry.name)).toEqual(['c.md', 'a.txt', 'b.txt']);
    expect(sortRows(ROWS, { ...spec, dir: -1 }).map((r) => r.entry.name)).toEqual([
      'b.txt',
      'a.txt',
      'c.md',
    ]);
  });

  it('extension is case-insensitive and dotfiles group under none', () => {
    const rows = [row('Z.PNG'), row('a.png'), row('.gitignore'), row('plain')];
    const names = sortRows(rows, { key: 'extension', dir: 1 }).map((r) => r.entry.name);
    // '' group: .gitignore + plain (sorted by name), then png group
    expect(names).toEqual(['.gitignore', 'plain', 'a.png', 'Z.PNG']);
  });

  it('unsorted keeps the listing order (a copy, original untouched)', () => {
    const out = sortRows(ROWS, { key: 'unsorted', dir: 1 });
    expect(out.map((r) => r.entry.name)).toEqual(['b.txt', 'a.txt', 'c.md']);
    out.push(row('zzz'));
    expect(ROWS).toHaveLength(3);
  });

  it('folders still sort first for keyed sorts', () => {
    const rows = [row('z.txt'), row('A-dir', { isDir: true }), row('a.txt')];
    expect(sortRows(rows, { key: 'extension', dir: 1 }).map((r) => r.entry.name)).toEqual([
      'A-dir',
      'a.txt',
      'z.txt',
    ]);
  });

  it('compareRows breaks ties by path deterministically', () => {
    const a = row('same', { path: 'C:\\d\\same' });
    const b = row('same', { path: 'C:\\e\\same' });
    expect(compareRows(a, b, { key: 'name', dir: 1 })).toBeLessThan(0);
  });

  it('100k rows sort + filter stays fast (F2-15 acceptance)', () => {
    const big = Array.from({ length: 100_000 }, (_, i) =>
      row(`f${i}.txt`, { size: i, modifiedMs: i }),
    );
    const t0 = performance.now();
    const sorted = sortRows(big, { key: 'extension', dir: 1 });
    const out = sorted.filter((r) => r.entry.size % 2 === 0);
    out.reverse();
    expect(sorted).toHaveLength(100_000);
    expect(out.length).toBe(50_000);
    expect(performance.now() - t0).toBeLessThan(2000);
  });
});
