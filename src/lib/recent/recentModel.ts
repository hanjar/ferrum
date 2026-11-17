/**
 * FER-598: the merged "recent files and folders" model. Pure functions: sources are fetched
 * elsewhere; this module only converts, merges, de-duplicates and matches.
 */
import type { ActivityItemDto } from '../ipc/activity';
import type { RecentFileDto } from '../ipc/recent';
import { parentOf } from '../format';

export const RECENT_CAP = 200;

export interface RecentItem {
  path: string;
  name: string;
  parent: string;
  /** null = unknown (a Windows Recent link without cached attributes). */
  isDir: boolean | null;
  lastUsedMs: number;
  /** Every source label that listed it, e.g. ['Ferrum', 'Windows']. */
  sources: string[];
}

/** Identity of a path: separators unified, trailing separator dropped, case-insensitive (Windows). */
export function pathKey(path: string): string {
  const unified = path.replace(/\//g, '\\');
  const trimmed = unified.replace(/\\+$/, '');
  return (trimmed === '' || /^[A-Za-z]:$/.test(trimmed) ? unified : trimmed).toLowerCase();
}

function nameOf(path: string): string {
  const norm = path.replace(/[\\/]+$/, '');
  const i = Math.max(norm.lastIndexOf('\\'), norm.lastIndexOf('/'));
  return i === -1 ? norm || path : norm.slice(i + 1) || norm;
}

export function fromActivity(items: readonly ActivityItemDto[]): RecentItem[] {
  return items.map((it) => ({
    path: it.entity.locator,
    name: it.entity.displayName || nameOf(it.entity.locator),
    parent: it.entity.parentPath || parentOf(it.entity.locator),
    isDir: it.entity.kind === 'folder',
    lastUsedMs: it.lastUtcMs,
    sources: ['Ferrum'],
  }));
}

export function fromRecentFiles(items: readonly RecentFileDto[]): RecentItem[] {
  return items.map((it) => ({
    path: it.path,
    name: it.name || nameOf(it.path),
    parent: parentOf(it.path),
    isDir: it.isDir,
    lastUsedMs: it.lastUsedMs,
    sources: [it.label || (it.source === 'windows' ? 'Windows' : 'New')],
  }));
}

/**
 * Merge several lists: de-duplicate by path (case-insensitive), keep the newest time and the
 * union of source labels, newest first, capped.
 */
export function mergeRecent(
  lists: readonly (readonly RecentItem[])[],
  cap = RECENT_CAP,
): RecentItem[] {
  const byKey = new Map<string, RecentItem>();
  for (const list of lists) {
    for (const item of list) {
      const key = pathKey(item.path);
      const cur = byKey.get(key);
      if (!cur) {
        byKey.set(key, { ...item, sources: [...item.sources] });
        continue;
      }
      const newer = item.lastUsedMs > cur.lastUsedMs;
      const sources = [...cur.sources];
      for (const s of item.sources) if (!sources.includes(s)) sources.push(s);
      byKey.set(key, {
        ...(newer ? item : cur),
        // A known kind wins over an unknown one.
        isDir: (newer ? item.isDir : cur.isDir) ?? (newer ? cur.isDir : item.isDir),
        sources,
      });
    }
  }
  return [...byKey.values()]
    .sort((a, b) => b.lastUsedMs - a.lastUsedMs || a.path.localeCompare(b.path))
    .slice(0, cap);
}

/** Case-insensitive name match for typed palette search: prefix first, then substring. */
export function matchRecentByName(
  items: readonly RecentItem[],
  query: string,
  limit: number,
): RecentItem[] {
  const q = query.trim().toLowerCase();
  if (q === '') return [];
  const scored: { item: RecentItem; rank: number }[] = [];
  for (const item of items) {
    const name = item.name.toLowerCase();
    const at = name.indexOf(q);
    if (at < 0) continue;
    scored.push({ item, rank: at === 0 ? 0 : 1 });
  }
  scored.sort((a, b) => a.rank - b.rank || b.item.lastUsedMs - a.item.lastUsedMs);
  return scored.slice(0, limit).map((s) => s.item);
}

export const sourcesText = (item: RecentItem): string => item.sources.join(' + ');
