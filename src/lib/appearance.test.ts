import { describe, expect, it } from 'vitest';
import { displayName } from './columns';
import { sortRows } from './selection';
import { PALETTES, ui } from './uiState.svelte';
import { toRow, type Row } from './stores/paneStore.svelte';
import type { FileEntry } from './ipc/types';

function row(name: string, isDir: boolean, size = 100): Row {
  const e: FileEntry = {
    name,
    path: `C:\\t\\${name}`,
    isDir,
    isSymlink: false,
    size,
    modifiedMs: 1_790_512_000_000,
    hidden: false,
  };
  return toRow(e);
}

describe('appearance contract (FER-370 APP-05)', () => {
  it('registry holds the six stable palette IDs with v3 labels', () => {
    expect(PALETTES.map((p) => p.value)).toEqual([
      'violet',
      'electric',
      'aqua',
      'green',
      'yellow',
      'orange',
    ]);
    expect(PALETTES.map((p) => p.label)).toEqual([
      'Pop Violet',
      'Electric',
      'Aqua Signal',
      'Fresh Mint',
      'Sunshine',
      'Tangerine',
    ]);
  });

  it('applyToDocument mirrors every appearance field onto <html> data attributes', () => {
    const el = document.documentElement;
    ui.setTheme('light');
    ui.palette = 'green';
    ui.density = 'comfortable';
    ui.iconStyle = 'monochrome';
    ui.uiFontLevel = 'l';
    ui.applyToDocument();
    expect(el.dataset.theme).toBe('light');
    expect(el.dataset.palette).toBe('green');
    expect(el.dataset.density).toBe('comfortable');
    expect(el.dataset.icon).toBe('monochrome');
    expect(el.dataset.ui).toBe('l');
    // Restore defaults so other suites start from the documented baseline.
    ui.setTheme('dark');
    ui.palette = 'violet';
    ui.density = 'compact';
    ui.iconStyle = 'colored';
    ui.uiFontLevel = 'm';
    ui.applyToDocument();
    expect(el.dataset.theme).toBe('dark');
    expect(el.dataset.density).toBe('compact');
  });

  it('foldersFirst keeps dirs on top even descending; off lets the sort interleave', () => {
    const rows = [row('zeta.txt', false, 30), row('arc', true, 0), row('mid.txt', false, 20)];
    const desc = { key: 'size' as const, dir: -1 as const };
    const on = sortRows(rows, desc, true);
    expect(on.map((r) => r.entry.isDir)).toEqual([true, false, false]);
    const off = sortRows(rows, desc, false);
    expect(off.map((r) => r.entry.name)).toEqual(['zeta.txt', 'mid.txt', 'arc']);
  });

  it('displayName hides only the last suffix of files (display only)', () => {
    expect(displayName('report.pdf', false, true)).toBe('report.pdf');
    expect(displayName('report.pdf', false, false)).toBe('report');
    expect(displayName('trace.jsonl.gz', false, false)).toBe('trace.jsonl');
    expect(displayName('.gitignore', false, false)).toBe('.gitignore');
    expect(displayName('README', false, false)).toBe('README');
    expect(displayName('src', true, false)).toBe('src');
  });
});
