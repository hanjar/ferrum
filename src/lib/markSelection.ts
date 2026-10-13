/**
 * Mark menu logic (F2-14): pattern selection, same-extension selection,
 * save/restore, and clipboard formatting. Pure and unit-tested — selection
 * is manipulated over entry identity (path), never over DOM rows, so it
 * works identically on virtualized 100k listings.
 */

/** Plain-data snapshot of one visible row (see PaneEntryInfo). */
export interface EntryInfo {
  name: string;
  path: string;
  isDir: boolean;
  size: number;
  modifiedMs: number;
}

// ── Glob matching (Select/Unselect Group) ────────────────────────────────

/** Compile a Total-Commander-style mask (`*` = any run, `?` = one char)
 *  into a full-match tester. Wildcards never cross `/` or `\\`, so a mask
 *  can never smuggle a path in; everything else is literal, including dots. */
export function globToRegExp(pattern: string, caseInsensitive = true): RegExp {
  let out = '';
  for (const ch of pattern) {
    if (ch === '*') out += '[^\\\\/]*';
    else if (ch === '?') out += '[^\\\\/]';
    else out += ch.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  }
  return new RegExp(`^${out}$`, caseInsensitive ? 'i' : '');
}

/** Whole-name glob test; an empty pattern never matches (safety). */
export function matchName(name: string, pattern: string, caseInsensitive = true): boolean {
  const p = pattern.trim();
  if (!p) return false;
  return globToRegExp(p, caseInsensitive).test(name);
}

/** Match a ';'- or ','-separated mask list, Total-Commander style. */
export function matchMasks(name: string, patternList: string, caseInsensitive = true): boolean {
  const parts = patternList
    .split(/[;,]/)
    .map((p) => p.trim())
    .filter(Boolean);
  return parts.some((p) => matchName(name, p, caseInsensitive));
}

/** File extension without the dot; dotfiles and extensionless names → ''. */
export function extensionOf(name: string): string {
  const dot = name.lastIndexOf('.');
  if (dot <= 0) return ''; // no dot, or dotfile like ".gitignore"
  return name.slice(dot + 1);
}

export interface GroupSelectOptions {
  caseInsensitive?: boolean;
  /** true = add matches (Select Group), false = remove them (Unselect Group). */
  select: boolean;
}

/** Add or remove every visible entry whose name matches the mask list.
 *  Returns a new set; the input is never mutated. */
export function selectMatching(
  entries: readonly EntryInfo[],
  pattern: string,
  opts: GroupSelectOptions,
  current: ReadonlySet<string> = new Set(),
): Set<string> {
  const next = new Set(current);
  const ci = opts.caseInsensitive ?? true;
  for (const e of entries) {
    if (!matchMasks(e.name, pattern, ci)) continue;
    if (opts.select) next.add(e.path);
    else next.delete(e.path);
  }
  return next;
}

/** Every visible entry NOT currently selected (Invert Selection). */
export function invertSelection(
  entries: readonly EntryInfo[],
  current: ReadonlySet<string>,
): Set<string> {
  const next = new Set<string>();
  for (const e of entries) if (!current.has(e.path)) next.add(e.path);
  return next;
}

/** Select All With Same Extension. Directories select directories; a file
 *  with no extension selects extensionless files — dotfiles form their own
 *  group so a dotfile cursor selects dotfiles, never plain files. */
export function selectSameExtension(
  entries: readonly EntryInfo[],
  cursor: Pick<EntryInfo, 'name' | 'isDir'> | undefined,
): Set<string> {
  const next = new Set<string>();
  if (!cursor) return next;
  const wantDir = cursor.isDir;
  const ext = extensionOf(cursor.name).toLowerCase();
  const cursorDot = cursor.name.startsWith('.');
  for (const e of entries) {
    if (wantDir) {
      if (e.isDir) next.add(e.path);
    } else if (!e.isDir && extensionOf(e.name).toLowerCase() === ext) {
      if (e.name.startsWith('.') !== cursorDot) continue; // separate groups
      next.add(e.path);
    }
  }
  return next;
}

// ── Clipboard formats (plain text, paste-friendly into editors/sheets) ──

function pad(n: number): string {
  return n < 10 ? `0${n}` : String(n);
}

/** `YYYY-MM-DD HH:MM` in local time — sortable, sheet-friendly. */
export function formatTimestamp(ms: number): string {
  if (!Number.isFinite(ms) || ms <= 0) return '';
  const d = new Date(ms);
  return (
    `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ` +
    `${pad(d.getHours())}:${pad(d.getMinutes())}`
  );
}

/** One name per line, in visible order. */
export function formatNames(entries: readonly EntryInfo[]): string {
  return entries.map((e) => e.name).join('\n');
}

/** One full path per line, in visible order. */
export function formatPaths(entries: readonly EntryInfo[]): string {
  return entries.map((e) => e.path).join('\n');
}

/** TSV: name, size in bytes, modification time. Directories report 0. */
export function formatDetails(entries: readonly EntryInfo[]): string {
  return entries.map((e) => `${e.name}\t${e.size}\t${formatTimestamp(e.modifiedMs)}`).join('\n');
}

/** TSV with paths: path, size in bytes, modification time. */
export function formatPathDetails(entries: readonly EntryInfo[]): string {
  return entries.map((e) => `${e.path}\t${e.size}\t${formatTimestamp(e.modifiedMs)}`).join('\n');
}

// ── Save/restore (per tab in memory; to/from versioned files) ───────────

export const SELECTION_FILE_VERSION = 1;

/** On-disk format: versioned, simple, reviewed by load. */
export interface SelectionFile {
  version: number;
  /** Directory the names were captured in (informational). */
  path: string;
  /** Basenames only — never absolute paths, never '..'. */
  names: string[];
}

export function serializeSelectionFile(path: string, names: readonly string[]): string {
  const doc: SelectionFile = { version: SELECTION_FILE_VERSION, path, names: [...names] };
  return JSON.stringify(doc, null, 2);
}

/** Parse + validate a selection file. Names must be plain basenames; a name
 *  with any separator or traversal is rejected instead of sanitized. */
export function parseSelectionFile(json: string): SelectionFile {
  let raw: unknown;
  try {
    raw = JSON.parse(json);
  } catch {
    throw new Error('Not a valid selection file (JSON expected).');
  }
  if (typeof raw !== 'object' || raw === null) throw new Error('Not a valid selection file.');
  const doc = raw as Record<string, unknown>;
  if (doc.version !== SELECTION_FILE_VERSION)
    throw new Error(`Unsupported selection file version: ${String(doc.version)}`);
  if (typeof doc.path !== 'string' || !doc.path) throw new Error('Selection file has no path.');
  if (!Array.isArray(doc.names) || doc.names.some((n) => typeof n !== 'string' || !n))
    throw new Error('Selection file has an invalid name list.');
  const names = doc.names as string[];
  const bad = names.find((n) => /[\\/]/.test(n) || n === '.' || n === '..' || n.includes('\0'));
  if (bad !== undefined) throw new Error(`Selection file contains a non-basename: "${bad}"`);
  return { version: SELECTION_FILE_VERSION, path: doc.path, names };
}

export interface AppliedSelection {
  /** Paths actually selected (present in the pane's visible listing). */
  selected: string[];
  /** Saved names that no longer exist (or are filtered out) — reported,
   *  never silently dropped (F2-14 acceptance). */
  missing: string[];
}

/** Match saved names against the pane's current listing. Restores by
 *  identity; entries outside the pane can never be selected this way. */
export function applySavedSelection(
  entries: readonly EntryInfo[],
  saved: SelectionFile,
): AppliedSelection {
  const byLower = new Map(entries.map((e) => [e.name.toLowerCase(), e]));
  const selected: string[] = [];
  const missing: string[] = [];
  for (const name of saved.names) {
    const hit = byLower.get(name.toLowerCase());
    if (hit) selected.push(hit.path);
    else missing.push(name);
  }
  return { selected, missing };
}
