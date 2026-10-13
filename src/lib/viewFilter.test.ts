import { describe, expect, it } from 'vitest';
import { filterByView, isProgramName, PROGRAM_EXTENSIONS } from './viewFilter';
import { toRow } from './stores/paneStore.svelte';
import type { FileEntry } from './ipc/types';

function file(name: string, size = 1): FileEntry {
  return {
    name,
    path: `C:\\dir\\${name}`,
    isDir: false,
    isSymlink: false,
    size,
    modifiedMs: 1,
    hidden: false,
  };
}

function dir(name: string): FileEntry {
  return { ...file(name), isDir: true, size: 0 };
}

const ROWS = [
  toRow(file('setup.exe', 10)),
  toRow(file('notes.txt')),
  toRow(file('install.MSI', 5)),
  toRow(file('Makefile')),
  toRow(dir('docs')),
  toRow(file('setup.ps1')),
];

describe('isProgramName', () => {
  it('matches executable extensions case-insensitively', () => {
    expect(isProgramName('setup.exe')).toBe(true);
    expect(isProgramName('install.MSI')).toBe(true);
    expect(isProgramName('tool.appimage')).toBe(true);
    expect(isProgramName('notes.txt')).toBe(false);
    expect(isProgramName('Makefile')).toBe(false);
    expect(isProgramName('.exe')).toBe(false); // dotfile, not extension "exe"
  });

  it('keeps a small honest list', () => {
    expect(PROGRAM_EXTENSIONS).toContain('exe');
    expect(PROGRAM_EXTENSIONS).not.toContain('txt');
  });
});

describe('filterByView (F2-15 Show presets)', () => {
  const selected = new Set(['C:\\dir\\notes.txt', 'C:\\dir\\docs']);

  it('all keeps every row', () => {
    expect(filterByView(ROWS, { mode: 'all', pattern: '', selectedPaths: selected })).toHaveLength(
      ROWS.length,
    );
  });

  it('programs shows only program files, never folders', () => {
    const rows = filterByView(ROWS, { mode: 'programs', pattern: '', selectedPaths: selected });
    expect(rows.map((r) => r.entry.name).sort()).toEqual(['install.MSI', 'setup.exe', 'setup.ps1']);
  });

  it('custom applies the mask list to names', () => {
    const rows = filterByView(ROWS, {
      mode: 'custom',
      pattern: '*.txt; setup*',
      selectedPaths: new Set(),
    });
    expect(rows.map((r) => r.entry.name).sort()).toEqual(['notes.txt', 'setup.exe', 'setup.ps1']);
    expect(
      filterByView(ROWS, { mode: 'custom', pattern: '', selectedPaths: new Set() }),
    ).toHaveLength(0); // empty mask hides everything, never "all"
  });

  it('selected keeps only selected paths', () => {
    const rows = filterByView(ROWS, { mode: 'selected', pattern: '', selectedPaths: selected });
    expect(rows.map((r) => r.entry.name).sort()).toEqual(['docs', 'notes.txt']);
  });
});
