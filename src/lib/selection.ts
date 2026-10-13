/**
 * Sorting, filtering, and selection semantics for file lists — pure and
 * unit-tested. Components keep NO logic of their own here.
 */
import type { Row } from './stores/paneStore.svelte';

/** 'extension' (F2-15) sorts by extension then name; 'unsorted' keeps the
 *  filesystem's own listing order. */
export type SortKey = 'name' | 'size' | 'modified' | 'extension' | 'unsorted';
export type SortDir = 1 | -1;

export interface SortSpec {
  key: SortKey;
  dir: SortDir;
}

export const DEFAULT_SORT: SortSpec = { key: 'name', dir: 1 };

/** Numeric-aware chunk compare: file2 < file10, case-insensitive outside digits. */
export function compareNames(a: string, b: string): number {
  const A = a.split(/(\d+)/);
  const B = b.split(/(\d+)/);
  const n = Math.min(A.length, B.length);
  for (let i = 0; i < n; i++) {
    const x = A[i];
    const y = B[i];
    if (x === y) continue;
    if (i % 2 === 1) {
      // digit chunk: longer number wins after padding-free numeric compare
      const nx = Number(x);
      const ny = Number(y);
      if (nx !== ny) return nx < ny ? -1 : 1;
      continue; // equal numbers, e.g. "007" vs "7" → fall through to text
    }
    const c = x.toLowerCase() < y.toLowerCase() ? -1 : x.toLowerCase() > y.toLowerCase() ? 1 : 0;
    if (c !== 0) return c;
  }
  return A.length - B.length;
}

/** Extension without the dot, lowercased; dotfiles and extensionless
 *  names group under '' (same rule as markSelection.extensionOf). */
export function extensionKeyOf(name: string): string {
  const dot = name.lastIndexOf('.');
  if (dot <= 0) return '';
  return name.slice(dot + 1).toLowerCase();
}

/** Stable order: folders first (D13 default; the FER-379 Settings toggle
 *  disables the pre-pass so a descending sort interleaves), then key, ties
 *  broken by path. */
export function compareRows(a: Row, b: Row, spec: SortSpec, foldersFirst = true): number {
  const dir = spec.dir;
  let c: number;
  if (foldersFirst && a.entry.isDir !== b.entry.isDir) c = a.entry.isDir ? -1 : 1;
  else if (spec.key === 'name') c = compareNames(a.entry.name, b.entry.name) * dir;
  else if (spec.key === 'extension')
    c =
      (extensionKeyOf(a.entry.name).localeCompare(extensionKeyOf(b.entry.name)) ||
        compareNames(a.entry.name, b.entry.name)) * dir;
  else if (spec.key === 'size')
    c = (a.entry.size - b.entry.size || compareNames(a.entry.name, b.entry.name)) * dir;
  else
    c = (a.entry.modifiedMs - b.entry.modifiedMs || compareNames(a.entry.name, b.entry.name)) * dir;
  if (c !== 0) return c;
  return a.entry.path < b.entry.path ? -1 : a.entry.path > b.entry.path ? 1 : 0;
}

/** Non-mutating sort (returns a copy). 'unsorted' keeps the listing's own
 *  order — still a copy, so callers can reverse/filter it safely. */
export function sortRows(rows: readonly Row[], spec: SortSpec, foldersFirst = true): Row[] {
  if (spec.key === 'unsorted') return [...rows];
  return [...rows].sort((a, b) => compareRows(a, b, spec, foldersFirst));
}

export interface FilterOptions {
  /** Substring match on name, case-insensitive. '' shows everything. */
  query?: string;
  /** Hidden entries are dropped unless true (D08). */
  showHidden?: boolean;
}

export function filterRows(rows: readonly Row[], opts: FilterOptions): Row[] {
  const q = (opts.query ?? '').trim().toLowerCase();
  if (!q && opts.showHidden) return [...rows];
  return rows.filter((r) => {
    if (!opts.showHidden && r.entry.hidden) return false;
    if (!q) return true;
    return r.entry.name.toLowerCase().includes(q);
  });
}

// ── Selection (identity = entry.path; survives re-sort/re-filter) ────────

export function selectSingle(path: string): Set<string> {
  return new Set([path]);
}

export function toggleSelect(selected: ReadonlySet<string>, path: string): Set<string> {
  const next = new Set(selected);
  if (next.has(path)) next.delete(path);
  else next.add(path);
  return next;
}

/** Range from anchor to `to` (either direction). Replaces the current
 *  selection unless `additive`, in which case it unions into it. */
export function selectRange(
  visible: readonly Row[],
  anchor: string,
  to: string,
  current: ReadonlySet<string> = new Set(),
  additive = false,
): Set<string> {
  const ai = visible.findIndex((r) => r.entry.path === anchor);
  const ti = visible.findIndex((r) => r.entry.path === to);
  if (ai === -1 || ti === -1) return additive ? new Set(current) : new Set();
  const [lo, hi] = ai <= ti ? [ai, ti] : [ti, ai];
  const range = visible.slice(lo, hi + 1).map((r) => r.entry.path);
  if (!additive) return new Set(range);
  const next = new Set(current);
  for (const p of range) next.add(p);
  return next;
}

export function selectAll(visible: readonly Row[]): Set<string> {
  return new Set(visible.map((r) => r.entry.path));
}

/** Select all entries NOT currently selected (invert). */
export function invertSelection(
  visible: readonly Row[],
  current: ReadonlySet<string>,
): Set<string> {
  const next = new Set<string>();
  for (const r of visible) {
    if (!current.has(r.entry.path)) next.add(r.entry.path);
  }
  return next;
}

/** Select entries whose name matches a glob-like pattern (`*`, `?`). */
export function selectByPattern(visible: readonly Row[], pattern: string): Set<string> {
  const regex = pattern
    .replace(/[.+^${}()|[\]\\]/g, '\\$&')
    .replace(/\*/g, '.*')
    .replace(/\?/g, '.');
  const re = new RegExp(`^${regex}$`, 'i');
  const next = new Set<string>();
  for (const r of visible) {
    if (re.test(r.entry.name)) next.add(r.entry.path);
  }
  return next;
}

/** Select all entries sharing the same extension as `path`. */
export function selectByExtension(visible: readonly Row[], path: string): Set<string> {
  const ext = path.lastIndexOf('.') >= 0 ? path.slice(path.lastIndexOf('.')).toLowerCase() : '';
  if (!ext) return new Set();
  const next = new Set<string>();
  for (const r of visible) {
    if (r.entry.name.toLowerCase().endsWith(ext)) next.add(r.entry.path);
  }
  return next;
}

/** Next cursor index for arrow movement over the visible list. */
export function moveCursor(
  visible: readonly Row[],
  cursorPath: string | undefined,
  delta: number,
): string | undefined {
  if (visible.length === 0) return undefined;
  if (!cursorPath)
    return delta > 0 ? visible[0].entry.path : visible[visible.length - 1].entry.path;
  const i = visible.findIndex((r) => r.entry.path === cursorPath);
  if (i === -1) return visible[0].entry.path;
  const j = Math.max(0, Math.min(visible.length - 1, i + delta));
  return visible[j].entry.path;
}
