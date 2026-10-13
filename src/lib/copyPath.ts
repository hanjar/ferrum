/**
 * FER-483: text built by Ctrl+1 / Ctrl+2 / Ctrl+3 (copy folder path, names,
 * full paths). Pure functions so the format rules are unit-tested without a
 * pane. Multi-item output is joined with CRLF and has no trailing newline.
 */

export interface CopyEntry {
  readonly name: string;
  readonly path: string;
}

const EOL = '\r\n';

/** Path as the address bar shows it: no `\\?\` prefix, backslashes only. */
export function displayPath(path: string): string {
  let p = path;
  if (p.startsWith('\\\\?\\UNC\\')) p = '\\\\' + p.slice(8);
  else if (p.startsWith('\\\\?\\') || p.startsWith('\\\\.\\')) p = p.slice(4);
  return p.replace(/\//g, '\\');
}

/** Ctrl+1: the folder with exactly one trailing backslash (root stays `C:\`). */
export function folderPathText(path: string): string {
  const p = displayPath(path);
  if (p === '') return '';
  return p.replace(/\\+$/, '') + '\\';
}

/** Ctrl+3 item: absolute path without a trailing backslash (a drive root keeps `C:\`). */
export function itemPathText(path: string): string {
  const p = displayPath(path);
  if (/^[A-Za-z]:\\*$/.test(p)) return p.slice(0, 2) + '\\';
  return p.replace(/\\+$/, '');
}

/** Parent folder of an item path (display form), used when a pane has no folder. */
export function parentFolder(path: string): string {
  const p = itemPathText(path);
  const i = p.lastIndexOf('\\');
  if (i < 0) return '';
  const parent = p.slice(0, i);
  return /^[A-Za-z]:$/.test(parent) ? parent + '\\' : parent;
}

/**
 * Items to copy: every selected entry in visible order, else the cursor item.
 * `selected` paths that are not visible (filtered out) are ignored.
 */
export function pickCopyEntries(
  visible: readonly CopyEntry[],
  selected: readonly string[],
  cursor: CopyEntry | undefined,
): CopyEntry[] {
  if (selected.length > 0) {
    const set = new Set(selected);
    const picked = visible.filter((e) => set.has(e.path));
    if (picked.length > 0) return picked;
  }
  return cursor ? [cursor] : [];
}

export function namesText(entries: readonly CopyEntry[]): string {
  return entries.map((e) => e.name).join(EOL);
}

export function pathsText(entries: readonly CopyEntry[]): string {
  return entries.map((e) => itemPathText(e.path)).join(EOL);
}

/** Ctrl+1 text: pane folder, or the cursor item's folder when the pane has none. */
export function folderCopyText(paneFolder: string, cursor: CopyEntry | undefined): string {
  if (paneFolder.trim() !== '') return folderPathText(paneFolder);
  return cursor ? folderPathText(parentFolder(cursor.path)) : '';
}
