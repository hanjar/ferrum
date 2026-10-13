/**
 * Show-menu filter presets (F2-15): All files, Programs, Custom mask,
 * Only selected. Pure and unit-tested; applied after sort + query filter.
 */
import { matchMasks } from './markSelection';
import type { Row } from './stores/paneStore.svelte';
import type { FilterMode } from './stores/viewStateStore.svelte';

/** Executable/installer extensions. Windows-first (D01); the list is
 *  deliberately short and honest — anything else is data, not a program. */
export const PROGRAM_EXTENSIONS: readonly string[] = [
  'exe',
  'com',
  'bat',
  'cmd',
  'msi',
  'ps1',
  'scr',
  'jar',
  'app',
  'deb',
  'rpm',
  'appimage',
  'sh',
];

export function isProgramName(name: string): boolean {
  const dot = name.lastIndexOf('.');
  if (dot <= 0) return false;
  return PROGRAM_EXTENSIONS.includes(name.slice(dot + 1).toLowerCase());
}

export interface ViewFilterOptions {
  mode: FilterMode;
  /** Mask list for the 'custom' mode; '' matches nothing there. */
  pattern: string;
  selectedPaths: ReadonlySet<string>;
}

export function filterByView(rows: readonly Row[], opts: ViewFilterOptions): Row[] {
  switch (opts.mode) {
    case 'all':
      return [...rows];
    case 'programs':
      return rows.filter((r) => !r.entry.isDir && isProgramName(r.entry.name));
    case 'custom':
      return rows.filter((r) => matchMasks(r.entry.name, opts.pattern));
    case 'selected':
      return rows.filter((r) => opts.selectedPaths.has(r.entry.path));
  }
}
