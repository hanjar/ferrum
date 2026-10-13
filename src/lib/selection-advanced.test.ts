import { describe, expect, it } from 'vitest';
import { invertSelection, selectByExtension, selectByPattern } from './selection';
import { toRow } from './stores/paneStore.svelte';
import type { FileEntry } from './ipc/types';

const e = (name: string): FileEntry => ({
  name,
  path: `C:\\t\\${name}`,
  isDir: false,
  isSymlink: false,
  size: 10,
  modifiedMs: 1000,
  hidden: false,
});

const rows = [e('a.txt'), e('b.txt'), e('c.md'), e('d.zip'), e('e.txt')].map(toRow);

describe('invertSelection', () => {
  it('selects visible entries not in current selection', () => {
    const current = new Set(['C:\\t\\a.txt']);
    const result = invertSelection(rows, current);
    expect([...result].sort()).toEqual([
      'C:\\t\\b.txt',
      'C:\\t\\c.md',
      'C:\\t\\d.zip',
      'C:\\t\\e.txt',
    ]);
  });

  it('returns all when current is empty', () => {
    expect(invertSelection(rows, new Set()).size).toBe(rows.length);
  });
});

describe('selectByPattern', () => {
  it('matches wildcard * correctly', () => {
    const result = selectByPattern(rows, '*.txt');
    expect([...result].sort()).toEqual(['C:\\t\\a.txt', 'C:\\t\\b.txt', 'C:\\t\\e.txt']);
  });

  it('matches ? single character', () => {
    const result = selectByPattern(rows, '?.txt');
    expect([...result]).toEqual(['C:\\t\\a.txt', 'C:\\t\\b.txt', 'C:\\t\\e.txt']);
  });

  it('exact match with no wildcards', () => {
    const result = selectByPattern(rows, 'c.md');
    expect([...result]).toEqual(['C:\\t\\c.md']);
  });

  it('case insensitive', () => {
    const result = selectByPattern(rows, '*.TXT');
    expect(result.size).toBe(3);
  });

  it('no match returns empty', () => {
    expect(selectByPattern(rows, '*.xyz').size).toBe(0);
  });
});

describe('selectByExtension', () => {
  it('selects all files with same extension', () => {
    const result = selectByExtension(rows, 'C:\\t\\a.txt');
    expect([...result].sort()).toEqual(['C:\\t\\a.txt', 'C:\\t\\b.txt', 'C:\\t\\e.txt']);
  });

  it('no extension returns empty', () => {
    const noext = [e('Makefile'), e('Dockerfile')].map(toRow);
    expect(selectByExtension(noext, 'C:\\t\\Makefile').size).toBe(0);
  });
});
