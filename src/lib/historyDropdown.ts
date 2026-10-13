/**
 * FER-484: Alt+Down folder history dropdown (Total Commander style). Pure list building:
 * the top section is the most visited folders (activity store), the bottom one the
 * chronological history of the active tab, newest last, the current folder at the very end.
 */
import { isArchiveFile } from './stores/archiveFetcher';
import { isNetworkPath } from './networkPath';

export const FREQUENT_MAX = 20;
export const HISTORY_MAX = 30;

export interface HistoryRow {
  /** Where Enter / click navigates (no `\\?\` prefix). */
  path: string;
  kind: 'dir' | 'archive';
  /** FER-467 R8: on a network share (UNC / WebDAV): opening it contacts that host. */
  network: boolean;
  /** Visit count, top section only. */
  count?: number;
  /** The tab's current folder (last row of the bottom section). */
  current?: boolean;
}

export interface HistoryList {
  top: HistoryRow[];
  bottom: HistoryRow[];
  /** `top` then `bottom`: the keyboard order. */
  rows: HistoryRow[];
}

/** The user-facing spelling: `\\?\C:\x` -> `C:\x`, `\\?\UNC\srv\sh` -> `\\srv\sh`. */
export function displayPath(path: string): string {
  if (/^\\\\\?\\UNC\\/i.test(path)) return '\\\\' + path.slice(8);
  if (/^\\\\[?.]\\/.test(path)) return path.slice(4);
  return path;
}

/** Identity for de-duplication: case-insensitive, `/` = `\`, no trailing separator (drive roots keep one). */
export function pathKey(path: string): string {
  const p = displayPath(path).replace(/\//g, '\\').toLowerCase();
  const trimmed = p.replace(/\\+$/, '');
  return /^[a-z]:$/.test(trimmed) ? trimmed + '\\' : trimmed || p;
}

function rowOf(path: string): HistoryRow {
  const shown = displayPath(path);
  return {
    path: shown,
    kind: isArchiveFile(shown) ? 'archive' : 'dir',
    network: isNetworkPath(shown),
  };
}

export interface FrequentInput {
  path: string;
  count: number;
  lastUtcMs: number;
}

/** Most visited first; the same folder spelled twice keeps the higher count. */
export function frequentRows(items: readonly FrequentInput[], max = FREQUENT_MAX): HistoryRow[] {
  const best = new Map<string, FrequentInput>();
  for (const it of items) {
    if (!it.path) continue;
    const key = pathKey(it.path);
    const have = best.get(key);
    if (!have || it.count > have.count) best.set(key, it);
  }
  return [...best.values()]
    .sort((a, b) => b.count - a.count || b.lastUtcMs - a.lastUtcMs)
    .slice(0, max)
    .map((it) => ({ ...rowOf(it.path), count: it.count }));
}

/**
 * Bottom section: tab history (oldest first) + current folder, consecutive duplicates collapsed,
 * the newest `max` kept. An empty tab history falls back to `fallbackRecent` (newest first, as the
 * activity store returns it), shown oldest first so the newest sits next to the current folder.
 */
export function historyRows(
  tabBack: readonly string[],
  current: string,
  fallbackRecent: readonly string[] = [],
  max = HISTORY_MAX,
): HistoryRow[] {
  const source = tabBack.length > 0 ? [...tabBack] : [...fallbackRecent].reverse();
  const seq = current ? [...source, current] : source;
  const out: HistoryRow[] = [];
  let lastKey = '';
  for (const p of seq) {
    if (!p) continue;
    const key = pathKey(p);
    if (key === lastKey) {
      out[out.length - 1] = rowOf(p); // the later spelling wins (the current folder keeps its own)
      continue;
    }
    lastKey = key;
    out.push(rowOf(p));
  }
  const kept = out.slice(-max);
  if (current && kept.length > 0) kept[kept.length - 1].current = true;
  return kept;
}

export function buildHistoryList(input: {
  frequent: readonly FrequentInput[];
  tabBack: readonly string[];
  current: string;
  fallbackRecent?: readonly string[];
}): HistoryList {
  const top = frequentRows(input.frequent);
  const bottom = historyRows(input.tabBack, input.current, input.fallbackRecent);
  return { top, bottom, rows: [...top, ...bottom] };
}
