/**
 * Multi-Rename Tool rule engine (F2-20) — pure, unit-tested, no IPC.
 *
 * Pipeline per entry (Total Commander semantics, Ferrum tokens):
 *   1. tokens: `[N]` basename without extension, `[E]` extension without dot,
 *      `[C]` counter (start/step/pad), `[DATE]` today as `yyyy-mm-dd`,
 *      `[TIME]` now as `HH-mm` (colons are invalid in Windows names).
 *      When the pattern has no `[E]` the original extension is preserved
 *      (TC's default extension mask); an empty pattern keeps the name.
 *   2. search/replace on the substituted name — literal, or regex when
 *      `regex` is set (global, on the substituted string).
 *   3. case transform on the whole resulting name (TC "name+ext" behaviour).
 *
 * Validation before a run: structural invalidity (empty, separators,
 * reserved Windows device names, trailing dot/space), duplicate targets
 * inside the selection, and collisions with directory entries OUTSIDE the
 * selection (compared case-insensitively — safe on every platform).
 * Swap/cycle targets (another selection member's original name) are legal
 * and handled by the executor via temporary names.
 */

export type CaseMode = 'none' | 'lower' | 'upper' | 'firstUpper';

export interface RenameOptions {
  /** Token pattern; `''` keeps the original name. */
  pattern: string;
  find: string;
  replace: string;
  /** When true `find` is a global regular expression instead of literal text. */
  regex: boolean;
  caseMode: CaseMode;
  counterStart: number;
  counterStep: number;
  /** 1–6 zero padding for `[C]`; 0 disables padding. */
  counterPad: number;
}

export const DEFAULT_OPTIONS: RenameOptions = {
  pattern: '[N]',
  find: '',
  replace: '',
  regex: false,
  caseMode: 'none',
  counterStart: 1,
  counterStep: 1,
  counterPad: 0,
};

export type PreviewIssue = 'invalid' | 'duplicate' | 'collision';

export interface RenamePreviewItem {
  original: string;
  target: string;
  /** Undefined when the rename is legal (an unchanged name is not an issue). */
  issue?: PreviewIssue;
}

export interface RenamePair {
  /** Absolute path of the entry to rename. */
  from: string;
  /** New bare name (single path segment). */
  to: string;
}

/** Split `archive.tar.gz` into base `archive.tar` + ext `gz`; dotfiles have no ext. */
export function splitName(name: string): { base: string; ext: string } {
  const dot = name.lastIndexOf('.');
  if (dot <= 0) return { base: name, ext: '' }; // ".gitignore" and "noext"
  return { base: name.slice(0, dot), ext: name.slice(dot + 1) };
}

function pad(n: number, width: number): string {
  const digits = Math.trunc(n).toString();
  if (width <= 0) return digits;
  return digits.length >= width ? digits : digits.padStart(width, '0');
}

function applyCase(name: string, mode: CaseMode): string {
  switch (mode) {
    case 'lower':
      return name.toLowerCase();
    case 'upper':
      return name.toUpperCase();
    case 'firstUpper':
      return name.length === 0 ? name : name[0].toUpperCase() + name.slice(1);
    default:
      return name;
  }
}

/** `yyyy-mm-dd` — filename-safe current date in LOCAL time. */
export function dateToken(now: Date = new Date()): string {
  const p = (n: number) => String(n).padStart(2, '0');
  return `${now.getFullYear()}-${p(now.getMonth() + 1)}-${p(now.getDate())}`;
}

/** `HH-mm` — filename-safe current time (colons are illegal on Windows). */
export function timeToken(now: Date = new Date()): string {
  const p = (n: number) => String(n).padStart(2, '0');
  return `${p(now.getHours())}-${p(now.getMinutes())}`;
}

function substituteFindReplace(name: string, opts: RenameOptions): string {
  if (!opts.find) return name;
  if (opts.regex) {
    try {
      return name.replace(new RegExp(opts.find, 'g'), opts.replace);
    } catch {
      // Invalid regex: treat literally rather than silently dropping the rule.
      return name.split(opts.find).join(opts.replace);
    }
  }
  return name.split(opts.find).join(opts.replace);
}

/** Windows reserved device names (with or without extension). */
const RESERVED_BASES = new Set([
  'CON',
  'PRN',
  'AUX',
  'NUL',
  ...Array.from({ length: 9 }, (_, i) => `COM${i + 1}`),
  ...Array.from({ length: 9 }, (_, i) => `LPT${i + 1}`),
]);

/** Characters illegal in a Windows name (separators included). */
const INVALID_CHARS = /[<>:"/\\|?*]/;

function hasControlChars(name: string): boolean {
  for (let i = 0; i < name.length; i++) {
    if (name.charCodeAt(i) < 32) return true;
  }
  return false;
}

/** True when the name is a legal single path segment on the host platform. */
export function isValidTargetName(name: string): boolean {
  if (name.length === 0 || name.trim().length === 0) return false;
  if (INVALID_CHARS.test(name) || hasControlChars(name)) return false;
  if (name.endsWith('.') || name.endsWith(' ')) return false;
  if (name === '.' || name === '..') return false;
  const { base } = splitName(name);
  if (RESERVED_BASES.has(base.toUpperCase())) return false;
  return true;
}

/** Apply the full rule pipeline to one name. `counter` is the 1-based item index. */
export function applyRule(
  name: string,
  opts: RenameOptions,
  counter: number,
  now: Date = new Date(),
): string {
  if (opts.pattern === '') return name;
  const { base, ext } = splitName(name);
  let out = opts.pattern;
  out = out.split('[N]').join(base);
  out = out.split('[E]').join(ext);
  out = out
    .split('[C]')
    .join(pad(opts.counterStart + (counter - 1) * opts.counterStep, opts.counterPad));
  out = out.split('[DATE]').join(dateToken(now));
  out = out.split('[TIME]').join(timeToken(now));
  // Without an explicit [E] the original extension is preserved (TC default:
  // the extension mask stays "[E]"); an explicit [E] positions it exactly.
  if (!opts.pattern.includes('[E]') && ext !== '') out = `${out}.${ext}`;
  out = substituteFindReplace(out, opts);
  return applyCase(out, opts.caseMode);
}

/** Case-insensitive key so preview validation is safe on every platform. */
function fold(name: string): string {
  return name.toLowerCase();
}

/**
 * Build the original→target preview for a selection. `otherNames` are the
 * directory entries NOT being renamed (collision pool). Duplicate targets
 * inside the selection and collisions with `otherNames` are flagged so the
 * run button can stay disabled until the preview is fully valid.
 */
export function buildPreview(
  names: readonly string[],
  opts: RenameOptions,
  otherNames: readonly string[] = [],
): RenamePreviewItem[] {
  const now = new Date();
  const items: RenamePreviewItem[] = names.map((name, i) => ({
    original: name,
    target: applyRule(name, opts, i + 1, now),
  }));

  const originalsFolded = new Set(names.map(fold));
  const collisionPool = new Set(otherNames.map(fold));
  for (const item of items) {
    if (item.target !== item.original && !isValidTargetName(item.target)) {
      item.issue = 'invalid';
    } else if (
      item.target !== item.original &&
      !originalsFolded.has(fold(item.target)) &&
      collisionPool.has(fold(item.target))
    ) {
      item.issue = 'collision';
    }
  }
  // Duplicate targets: every item after the first holder of a target is flagged.
  const seen = new Map<string, string>();
  for (const item of items) {
    if (item.issue) continue;
    const key = fold(item.target);
    const first = seen.get(key);
    if (first !== undefined && first !== item.original) item.issue = 'duplicate';
    else seen.set(key, item.original);
  }
  return items;
}

/** Valid, actually-changing pairs ready for `renameBatch`. */
export function executablePairs(
  paths: readonly string[],
  preview: readonly RenamePreviewItem[],
): RenamePair[] {
  const pairs: RenamePair[] = [];
  for (let i = 0; i < preview.length && i < paths.length; i++) {
    const item = preview[i];
    if (!item.issue && item.target !== item.original)
      pairs.push({ from: paths[i], to: item.target });
  }
  return pairs;
}

/** True when any target name is (another member's) original name — swap/cycle. */
export function hasCrossRenames(preview: readonly RenamePreviewItem[]): boolean {
  const originals = new Set(preview.map((p) => fold(p.original)));
  return preview.some((p) => !p.issue && p.target !== p.original && originals.has(fold(p.target)));
}
