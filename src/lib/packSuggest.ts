/**
 * FER-579: suggests the 7z "Ultra (text)" level (PPMd) when a Pack selection is mostly plain text.
 * Decided by file extension alone (no file is read). A folder or a file without a listed
 * extension counts as "not text", so a mixed selection does not trigger the hint.
 */

/** Lowercase extensions, without the dot, that PPMd compresses much better than LZMA2. */
export const TEXT_EXTENSIONS: ReadonlySet<string> = new Set([
  'csv',
  'tsv',
  'txt',
  'log',
  'json',
  'jsonl',
  'ndjson',
  'xml',
  'sql',
  'md',
  'yaml',
  'yml',
  'ini',
  'cfg',
  'conf',
  'html',
  'htm',
  'css',
  'srt',
  'tex',
]);

/** Share of the selected items that must be text for the suggestion. */
export const TEXT_SHARE_THRESHOLD = 0.8;

function extensionOf(path: string): string {
  const name = path.slice(Math.max(path.lastIndexOf('\\'), path.lastIndexOf('/')) + 1);
  const dot = name.lastIndexOf('.');
  return dot > 0 ? name.slice(dot + 1).toLowerCase() : '';
}

/** True when at least {@link TEXT_SHARE_THRESHOLD} of `paths` have a text extension. */
export function isMostlyText(paths: readonly string[]): boolean {
  if (paths.length === 0) return false;
  const text = paths.filter((p) => TEXT_EXTENSIONS.has(extensionOf(p))).length;
  return text / paths.length >= TEXT_SHARE_THRESHOLD;
}
