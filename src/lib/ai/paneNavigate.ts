// FER-576: `pane.navigate`. The model may only COPY a path the user typed; the app decides locally.
//
//   1. `pathTypedByUser`: the path must occur in the user's own request text (mirror of the Rust
//      `path_typed_by_user`; shared cases in contract-fixtures/ai/navigate-cases.json). A path the
//      model made up is never found in the text, so it can never be opened.
//   2. The contract (`isSafeNavigatePath`): absolute, a local drive letter, no traversal, no device
//      names, no stream syntax.
//   3. The disk (`NavigatePort.resolve`): the path must exist; a file opens its folder and selects
//      that file. Nothing is ever opened with a program or executed.
//
// No listing, file name or size from the navigation goes to the provider.

/** Quote characters that are ignored when matching a typed path. */
const QUOTES = new Set(['"', '`', '\u2018', '\u2019', '\u201c', '\u201d']);

function normalize(text: string): string[] {
  const out: string[] = [];
  for (const ch of text) {
    if (QUOTES.has(ch)) continue;
    out.push(...(ch === '/' ? '\\' : ch).toLowerCase());
  }
  return out;
}

const isAlnum = (ch: string): boolean => /[\p{L}\p{N}]/u.test(ch);
const isSpace = (ch: string): boolean => /\s/u.test(ch);

/**
 * Did the user type `path`? Case-insensitive, `/` equals `\`, quotes are ignored. The match must
 * start at a token start and end at the end of the text, at whitespace, at a separator or at
 * punctuation; a dot inside a name (`a.csv`) is not a boundary.
 */
export function pathTypedByUser(path: string, userText: string): boolean {
  const needle = normalize(path);
  while (needle.length > 3 && needle[needle.length - 1] === '\\') needle.pop();
  if (needle.length < 2) return false;
  const hay = normalize(userText);
  if (hay.length < needle.length) return false;
  for (let start = 0; start + needle.length <= hay.length; start++) {
    let same = true;
    for (let i = 0; i < needle.length; i++) {
      if (hay[start + i] !== needle[i]) {
        same = false;
        break;
      }
    }
    if (!same) continue;
    if (start > 0 && isAlnum(hay[start - 1])) continue;
    const end = start + needle.length;
    const next = hay[end];
    let afterOk: boolean;
    if (next === undefined) afterOk = true;
    else if (next === '.') afterOk = hay[end + 1] === undefined || isSpace(hay[end + 1]);
    else afterOk = isSpace(next) || "\\,;)!?:'".includes(next);
    if (afterOk) return true;
  }
  return false;
}

/** The folder part of a Windows path (`D:\a\b.csv` -> `D:\a`); a drive root stays itself. */
export function parentFolder(path: string): string | undefined {
  const flat = path.replace(/\//g, '\\').replace(/\\+$/, '');
  const cut = flat.lastIndexOf('\\');
  if (cut < 0) return undefined;
  if (cut === 2 && /^[A-Za-z]:/.test(flat)) return flat.slice(0, 3);
  if (cut < 2) return undefined;
  return flat.slice(0, cut);
}

/** What the disk says about a typed path. */
export type NavigateTarget =
  | { kind: 'dir'; path: string }
  /** A file (or an archive file): its folder opens and the file is selected. */
  | { kind: 'file'; path: string; folder: string }
  | { kind: 'missing' };

/** Port to the real panes and the disk; the executor never touches stores directly. */
export interface NavigatePort {
  /** Offered only while the real panes exist (desktop app). */
  available(): boolean;
  /** Read-only: classify an absolute path on disk (the path gate of a normal listing applies). */
  resolve(path: string): Promise<NavigateTarget>;
  /**
   * Open `target` in `pane` and resolve once the pane shows it (and, for a file, has it selected), or
   * `false` when that did not happen in time. Never executes anything.
   */
  /**
   * Resolve once both panes stopped re-listing (a step may have refreshed them). Best effort; a snapshot taken
   * too early is simply refused as stale by the next step, never acted on.
   */
  settled?(): Promise<void>;
  open(
    pane: 'left' | 'right',
    target: Exclude<NavigateTarget, { kind: 'missing' }>,
  ): Promise<boolean>;
}

export interface NavigateView {
  summary: string;
  /** Result line shown after the navigation. */
  result?: { ok: boolean; message: string };
}

const baseName = (path: string): string =>
  path
    .replace(/[\\/]+$/, '')
    .split(/[\\/]/)
    .pop() ?? path;

/** App-built text of a navigation (never model text). */
export function describeNavigate(target: Exclude<NavigateTarget, { kind: 'missing' }>): string {
  return target.kind === 'dir'
    ? `Membuka folder ${target.path}.`
    : `Membuka folder ${target.folder} dan memilih ${baseName(target.path)}.`;
}

const DELETE_EXACT = new Set([
  'delete',
  'deleted',
  'deleting',
  'del',
  'remove',
  'removed',
  'removing',
  'erase',
  'erased',
  'trash',
  'rm',
]);
const DELETE_STEMS = ['hapus', 'buang', 'singkirkan', 'hilangkan'];
const DELETE_PREFIXES = ['meng', 'men', 'mem', 'me', 'di', 'pe'];

/**
 * FER-576 (review R1, S5): did the user ask to delete something? A hallucinating or steered model must not turn
 * "jelaskan file ini" into a delete card. Chunks that look like a path (a backslash, slash or colon) are ignored,
 * so a folder called `Remove` is not a request. Mirror of Rust `user_asked_to_delete`; shared cases in
 * `contract-fixtures/ai/delete-verb-cases.json`.
 */
export function userAskedToDelete(userText: string): boolean {
  for (const chunk of userText.split(/\s+/u)) {
    if (/[\\/:]/u.test(chunk) || /[\p{L}\p{N}]\.[\p{L}\p{N}]/u.test(chunk)) continue;
    for (const raw of chunk.split(/[^\p{L}\p{N}]+/u)) {
      if (!raw) continue;
      const token = raw.toLowerCase();
      if (DELETE_EXACT.has(token)) return true;
      const prefix = DELETE_PREFIXES.find((p) => token.startsWith(p));
      const core = prefix ? token.slice(prefix.length) : token;
      if (DELETE_STEMS.some((stem) => core.startsWith(stem) || token.startsWith(stem))) return true;
    }
  }
  return false;
}

/**
 * FER-576 (review R1, S6): how many absolute drive paths the user wrote (`D:\`, `d:/`). A chain that deletes is
 * only accepted for ONE path. Mirror of Rust `drive_path_count`.
 */
export function drivePathCount(userText: string): number {
  const chars = [...userText];
  let count = 0;
  for (let i = 0; i + 2 < chars.length; i++) {
    if (
      /[A-Za-z]/u.test(chars[i]) &&
      chars[i + 1] === ':' &&
      (chars[i + 2] === '\\' || chars[i + 2] === '/') &&
      (i === 0 || !/[\p{L}\p{N}]/u.test(chars[i - 1]))
    ) {
      count++;
    }
  }
  return count;
}

const normalizePath = (path: string): string =>
  path.replace(/\//g, '\\').replace(/\\+$/, '').toLowerCase();

/** Same Windows path (case-insensitive, either separator, trailing separator ignored). */
export function samePath(a: string, b: string): boolean {
  return normalizePath(a) === normalizePath(b);
}

/** Is `child` strictly inside `parent` (case-insensitive, either separator)? */
export function isInsidePath(child: string, parent: string): boolean {
  const c = normalizePath(child);
  const p = normalizePath(parent);
  return c.length > p.length && c.startsWith(p) && c[p.length] === '\\';
}
