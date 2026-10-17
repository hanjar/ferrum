/** FER-456 address bar: pure text helpers (no IPC, no DOM). */

export interface InputSplit {
  /** Everything up to and including the last separator. */
  folder: string;
  /** The last, still-being-typed segment. */
  prefix: string;
}

/**
 * FER-515 P3-5: a typed `\\?\C:\x` (verbatim) location is rewritten to the plain `C:\x` form at the
 * UI boundary, so every path the panes hold (and later send to a mutating command) is one the Rust
 * guard accepts. The guard itself stays strict: a verbatim spelling that is NOT a plain drive or
 * UNC path (`\\?\GLOBALROOT\...`, `\\.\C:`) is left untouched and refused there.
 */
export function stripVerbatimPrefix(path: string): string {
  const p = path.replace(/\//g, '\\');
  if (/^\\\\\?\\UNC\\[^\\]+\\[^\\]+/i.test(p)) return '\\\\' + p.slice(8);
  if (/^\\\\\?\\[A-Za-z]:(\\|$)/.test(p)) return p.slice(4);
  return path;
}

export function splitInput(text: string): InputSplit {
  const i = Math.max(text.lastIndexOf('\\'), text.lastIndexOf('/'));
  return { folder: text.slice(0, i + 1), prefix: text.slice(i + 1) };
}

/** Text after Tab/click completes `name` (dirs and archives that can be
 *  entered get a trailing separator for directories only). */
export function completeText(
  text: string,
  name: string,
  kind: 'drive' | 'dir' | 'archive',
): string {
  const { folder } = splitInput(text);
  if (kind === 'drive') return name.endsWith('\\') ? name : `${name}\\`;
  const sep = folder.includes('/') && !folder.includes('\\') ? '/' : '\\';
  return `${folder}${name}${kind === 'dir' ? sep : ''}`;
}

export interface Piece {
  text: string;
  bold: boolean;
}

/** Split `name` into the case-insensitively matched prefix and the rest. */
export function highlightPrefix(name: string, prefix: string): Piece[] {
  const n = name.toLowerCase().startsWith(prefix.toLowerCase()) ? prefix.length : 0;
  const out: Piece[] = [];
  if (n > 0) out.push({ text: name.slice(0, n), bold: true });
  if (n < name.length) out.push({ text: name.slice(n), bold: false });
  return out;
}

/** Wrap-around index step for the suggestion list; -1 means "none". */
export function stepIndex(current: number, delta: 1 | -1, count: number): number {
  if (count <= 0) return -1;
  if (current < 0) return delta === 1 ? 0 : count - 1;
  return (current + delta + count) % count;
}

/** UNC locations only (`\\host\...`), de-duplicated: the hosts the backend
 *  may treat as known when suggesting. */
export function uncPathsOf(paths: Iterable<string>): string[] {
  const out = new Set<string>();
  for (const p of paths) {
    if (p.startsWith('\\\\') || p.startsWith('//')) out.add(p);
  }
  return [...out];
}

export interface SuggestCache<T extends { name: string }> {
  base: string;
  folder: string;
  prefix: string;
  items: T[];
  /** Nothing was cut off (limit, deadline, unknown host): the full match set. */
  complete: boolean;
}

/**
 * Narrow a cached, complete suggestion list to a longer prefix in the SAME
 * folder without asking the backend again. Returns undefined when the cache
 * cannot answer (other folder/base, shorter prefix, incomplete list).
 */
export function refineFromCache<T extends { name: string }>(
  cache: SuggestCache<T> | undefined,
  base: string,
  text: string,
): T[] | undefined {
  if (!cache || !cache.complete || cache.base !== base) return undefined;
  const { folder, prefix } = splitInput(text);
  if (folder.toLowerCase() !== cache.folder.toLowerCase()) return undefined;
  const want = prefix.toLowerCase();
  if (!want.startsWith(cache.prefix.toLowerCase())) return undefined;
  return cache.items.filter((i) => i.name.toLowerCase().startsWith(want));
}
