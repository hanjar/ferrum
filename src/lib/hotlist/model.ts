/**
 * Directory Hotlist schema v2 (FER-458) — pure helpers shared by the popup menu, the Configure
 * dialog and the store. The Rust side (`src-tauri/src/hotlist/model.rs`) is the authority and
 * validates again; this mirror exists so the UI never offers an edit the backend would refuse.
 */

export interface DirItem {
  type: 'dir';
  /** Display title; `&` marks the accelerator and is kept verbatim. */
  title: string;
  path: string;
  /** Folder for the opposite pane (TC `pathN`). */
  targetPath?: string;
}
export interface SubmenuItem {
  type: 'submenu';
  title: string;
  items: HotItem[];
}
export interface SeparatorItem {
  type: 'separator';
}
export interface CommandItem {
  type: 'command';
  title: string;
  command: string;
  args: string[];
}
export type HotItem = DirItem | SubmenuItem | SeparatorItem | CommandItem;

export interface HotlistDoc {
  version: 2;
  source?: string;
  items: HotItem[];
}

export const LIMITS = {
  /** Submenu nesting below the top level. */
  depth: 8,
  /** Every item in the tree, separators included. */
  items: 2000,
  title: 256,
  path: 4096,
  args: 32,
} as const;

export function emptyDoc(): HotlistDoc {
  return { version: 2, items: [] };
}

// ── validation (mirror of the backend rules) ──

/**
 * Unicode "format" characters (category Cf) that can reorder or hide text: bidi controls
 * (U+202A–202E, U+2066–2069, U+200E/F, U+061C), zero-width and invisible operators, BOM, tag
 * characters. ZWNJ/ZWJ (U+200C/D) stay allowed. Mirror of `is_format_char` in `hotlist/model.rs`.
 */
export function isFormatChar(n: number): boolean {
  return (
    n === 0x00ad ||
    (n >= 0x0600 && n <= 0x0605) ||
    n === 0x061c ||
    n === 0x06dd ||
    n === 0x070f ||
    (n >= 0x0890 && n <= 0x0891) ||
    n === 0x08e2 ||
    n === 0x180e ||
    n === 0x200b ||
    n === 0x200e ||
    n === 0x200f ||
    (n >= 0x202a && n <= 0x202e) ||
    (n >= 0x2060 && n <= 0x2064) ||
    (n >= 0x2066 && n <= 0x206f) ||
    n === 0xfeff ||
    (n >= 0xfff9 && n <= 0xfffb) ||
    n === 0x110bd ||
    n === 0x110cd ||
    (n >= 0x13430 && n <= 0x1343f) ||
    (n >= 0x1bca0 && n <= 0x1bca3) ||
    (n >= 0x1d173 && n <= 0x1d17a) ||
    n === 0xe0001 ||
    (n >= 0xe0020 && n <= 0xe007f)
  );
}

const hasControl = (s: string) =>
  [...s].some((c) => {
    const n = c.codePointAt(0) ?? 0;
    return n <= 0x1f || (n >= 0x7f && n <= 0x9f) || isFormatChar(n);
  });
const okTitle = (s: unknown): s is string =>
  typeof s === 'string' && s.trim() !== '' && [...s].length <= LIMITS.title && !hasControl(s);
const okPath = (s: unknown): s is string =>
  typeof s === 'string' &&
  s.trim() !== '' &&
  !/^[\\/\s]*$/.test(s) &&
  [...s].length <= LIMITS.path &&
  !hasControl(s);

/**
 * FER-467 H1, mirror of `is_device_path` in `hotlist/model.rs`: device and NT namespace spellings
 * of a folder (`\\.\…`, `\\?\…` except the local `\\?\X:\…`, `\??\…`, `\Device\…`,
 * `…\Device\Mup\…`), any separator mix. Some reach a network host without looking like UNC.
 */
export function isDevicePath(s: string): boolean {
  const p = s.trim().replace(/\//g, '\\');
  const lower = p.toLowerCase();
  if (lower.startsWith('\\??\\') || lower.startsWith('\\device\\')) return true;
  if (lower.includes('\\device\\mup\\')) return true;
  if (p.startsWith('\\\\?\\')) {
    const rest = p.slice(4);
    return !/^[A-Za-z]:\\/.test(rest) || rest.slice(2).includes(':');
  }
  return /^\\\\[?.](?:\\|$)/.test(p);
}

const okDirPath = (s: unknown): s is string => okPath(s) && !isDevicePath(s);

// ── command items: what may run, and exactly how ──

/** `scheme:` with two or more characters before the colon; a lone letter is a drive (`C:`). */
export const hasUriScheme = (s: string): boolean => /^[A-Za-z][A-Za-z0-9+.-]+:/.test(s.trim());

const isSep = (c: string | undefined): boolean => c === '\\' || c === '/';

/** The reason a command string is refused, or `null`. Mirror of `valid_command` in `hotlist/model.rs`. */
function commandStringProblem(s: string): 'program' | 'uri' | 'network' | null {
  const t = s.trim();
  if (hasUriScheme(t)) return 'uri';
  // Two separators in any mix (`\\`, `//`, `\/`, `/\`) or the NT prefix `\??\`: UNC/WebDAV/device.
  if ((isSep(t[0]) && isSep(t[1])) || t.startsWith('\\??\\')) return 'network';
  // `" < > | * ?` cannot be part of a program path; `:` only right after the drive letter (ADS).
  if (/["<>|*?]/.test(s)) return 'program';
  for (let i = t.indexOf(':'); i >= 0; i = t.indexOf(':', i + 1)) {
    if (i !== 1 || !/[A-Za-z]/.test(t[0])) return 'program';
  }
  return null;
}

/**
 * A command is a local program path or a bare program name. Refused: URI schemes, UNC/WebDAV/device
 * paths, `" < > | * ?`, and `:` anywhere but after a drive letter. Shared vectors:
 * `contract-fixtures/hotlist/command-cases.json`.
 */
export const okCommand = (s: unknown): s is string => okPath(s) && commandStringProblem(s) === null;

/** cmd.exe interprets these in arguments of a batch file (CVE-2024-24576 "BatBadBut"). */
const CMD_META = /["%&|<>^\r\n]/;

/**
 * True unless `command` is a plain `.exe`/`.com` (trailing dots and spaces are ignored by Windows).
 * Everything else may end up in cmd.exe: `.bat`/`.cmd`, shortcuts that point at one, names with
 * another or no extension (PATHEXT), so its arguments get the strict check.
 */
export function needsCmdSafeArgs(command: string): boolean {
  const name = (
    command
      .trim()
      .replace(/[. ]+$/, '')
      .split(/[\\/]/)
      .pop() ?? ''
  ).toLowerCase();
  const dot = name.lastIndexOf('.');
  return dot < 0 ? true : !['exe', 'com'].includes(name.slice(dot + 1));
}

export const batchUnsafeArgs = (command: string, args: string[]): boolean =>
  needsCmdSafeArgs(command) && args.some((a) => CMD_META.test(a));

export type CommandProblem = 'program' | 'uri' | 'network' | 'args_unsafe';

/** Why a command item is not acceptable (for field hints), or `null`. */
export function commandProblem(command: string, args: string[]): CommandProblem | null {
  const problem = commandStringProblem(command);
  if (problem) return problem;
  if (!okPath(command) || hasControl(command)) return 'program';
  if (batchUnsafeArgs(command, args)) return 'args_unsafe';
  return null;
}

/**
 * Kinds of program that deserve a warning before they run: scripts and anything that hides its real
 * target (shortcuts, internet shortcuts). `bat`/`cmd` run through cmd.exe.
 */
export function commandRisk(command: string): 'script' | 'shortcut' | null {
  const ext = /\.([A-Za-z0-9]+)[. ]*$/.exec(command.trim())?.[1]?.toLowerCase() ?? '';
  if (['lnk', 'url'].includes(ext)) return 'shortcut';
  if (
    [
      'ps1',
      'vbs',
      'vbe',
      'js',
      'jse',
      'wsf',
      'wsh',
      'hta',
      'scr',
      'pif',
      'msc',
      'reg',
      'bat',
      'cmd',
    ].includes(ext)
  ) {
    return 'script';
  }
  return null;
}

/** One argument quoted like `shell_open::quote_arg` (CommandLineToArgvW rules); shared test vectors. */
export function quoteArg(arg: string): string {
  if (arg !== '' && !/[\s"]/.test(arg)) return arg;
  let out = '"';
  let backslashes = 0;
  for (const c of arg) {
    if (c === '\\') {
      backslashes++;
    } else if (c === '"') {
      out += '\\'.repeat(backslashes * 2 + 1) + '"';
      backslashes = 0;
    } else {
      out += '\\'.repeat(backslashes) + c;
      backslashes = 0;
    }
  }
  return out + '\\'.repeat(backslashes * 2) + '"';
}

/**
 * The command line the backend hands to ShellExecute, for the confirmation dialog: the program
 * (quoted only for display when it has spaces) followed by each argument quoted exactly like the
 * backend does, so `["a b"]` and `["a","b"]` look different.
 */
export function commandLine(command: string, args: string[]): string {
  const program = /[\s"]/.test(command) ? `"${command}"` : command;
  return [program, ...args.map(quoteArg)].join(' ');
}

function parseItems(raw: unknown, depth: number, count: { n: number }): HotItem[] | null {
  if (!Array.isArray(raw)) return null;
  const out: HotItem[] = [];
  for (const r of raw) {
    if (typeof r !== 'object' || r === null) return null;
    if (++count.n > LIMITS.items) return null;
    const o = r as Record<string, unknown>;
    switch (o.type) {
      case 'dir': {
        if (!okTitle(o.title) || !okDirPath(o.path)) return null;
        const item: DirItem = { type: 'dir', title: o.title, path: o.path };
        if (o.targetPath !== undefined && o.targetPath !== null) {
          if (!okDirPath(o.targetPath)) return null;
          item.targetPath = o.targetPath;
        }
        out.push(item);
        break;
      }
      case 'submenu': {
        if (!okTitle(o.title) || depth >= LIMITS.depth) return null;
        const items = parseItems(o.items ?? [], depth + 1, count);
        if (!items) return null;
        out.push({ type: 'submenu', title: o.title, items });
        break;
      }
      case 'separator':
        out.push({ type: 'separator' });
        break;
      case 'command': {
        const args = o.args ?? [];
        if (!okTitle(o.title) || !okCommand(o.command)) return null;
        if (
          !Array.isArray(args) ||
          args.length > LIMITS.args ||
          args.some((a) => typeof a !== 'string' || [...a].length > LIMITS.path || hasControl(a))
        ) {
          return null;
        }
        if (batchUnsafeArgs(o.command, args as string[])) return null;
        out.push({ type: 'command', title: o.title, command: o.command, args: args as string[] });
        break;
      }
      default:
        return null;
    }
  }
  return out;
}

/** Validate a document; `null` for anything that is not a valid v2 hotlist. */
export function parseDoc(raw: unknown): HotlistDoc | null {
  if (typeof raw !== 'object' || raw === null) return null;
  const o = raw as Record<string, unknown>;
  if (o.version !== 2) return null;
  const items = parseItems(o.items ?? [], 0, { n: 0 });
  if (!items) return null;
  const doc: HotlistDoc = { version: 2, items };
  if (o.source !== undefined && o.source !== null) {
    if (
      typeof o.source !== 'string' ||
      [...o.source].length > LIMITS.path ||
      hasControl(o.source)
    ) {
      return null;
    }
    doc.source = o.source;
  }
  return doc;
}

export function countItems(items: HotItem[]): number {
  let n = 0;
  for (const it of items) n += 1 + (it.type === 'submenu' ? countItems(it.items) : 0);
  return n;
}

/** Deepest submenu nesting of a subtree (a lone leaf = 0). */
function depthOf(item: HotItem): number {
  if (item.type !== 'submenu') return 0;
  return 1 + item.items.reduce((m, i) => Math.max(m, depthOf(i)), 0);
}

// ── v1 (flat list in localStorage) migration ──

export const V1_KEY = 'ferrum.hotlist.v1';

/** Convert the old flat `{name, path}[]` payload; malformed entries are dropped. */
export function migrateV1(raw: string | null): HotItem[] {
  if (!raw) return [];
  let data: unknown;
  try {
    data = JSON.parse(raw);
  } catch {
    return [];
  }
  if (!Array.isArray(data)) return [];
  const seen = new Set<string>();
  const out: HotItem[] = [];
  for (const e of data) {
    if (typeof e !== 'object' || e === null) continue;
    const { name, path } = e as Record<string, unknown>;
    if (!okTitle(name) || !okDirPath(path) || seen.has(path)) continue;
    seen.add(path);
    out.push({ type: 'dir', title: name, path });
  }
  return out;
}

// ── titles and accelerators ──

export interface TitleParts {
  /** Accelerator shown in the menu, lower-cased (`'1'`, `'c'`), or `null`. */
  key: string | null;
  /** Every key that selects the item: the shown one plus the `&` letter (`"4. &Charlie"` → 4 and c). */
  keys: string[];
  /** Label without `&` markers (and without the redundant leading number). */
  label: string;
}

/**
 * `&x` marks an accelerator (`&&` is a literal ampersand); a single leading digit (`"2. Project"`)
 * is one too. A leading number wins as the SHOWN key (so 1 → 6 → 4 walks to "4. &Charlie") and is
 * dropped from the label, because the accelerator column already shows it; the `&` letter keeps
 * working as a second key.
 */
export function titleParts(title: string): TitleParts {
  let amp: string | null = null;
  let label = '';
  for (let i = 0; i < title.length; i++) {
    const c = title[i];
    if (c === '&') {
      const next = title[i + 1];
      if (next === '&') {
        label += '&';
        i++;
      } else if (next !== undefined && amp === null && /\S/.test(next)) {
        amp = next.toLowerCase();
      }
      continue;
    }
    label += c;
  }
  const digit = /^\s*(\d)(?!\d)/.exec(label)?.[1] ?? null;
  if (digit !== null) {
    const stripped = label.replace(new RegExp(`^\\s*${digit}[.)]?\\s+`), '');
    if (stripped.trim() !== '') label = stripped;
  }
  const key = digit ?? amp;
  const keys = [...new Set([digit, amp].filter((k): k is string => k !== null))];
  return { key, keys, label: label.trim() === '' ? title : label };
}

// ── tree access by index path ──

export type TreePath = number[];

export function itemsAt(items: HotItem[], parent: TreePath): HotItem[] | null {
  let cur = items;
  for (const i of parent) {
    const it = cur[i];
    if (!it || it.type !== 'submenu') return null;
    cur = it.items;
  }
  return cur;
}

export function getAt(items: HotItem[], path: TreePath): HotItem | null {
  const parent = itemsAt(items, path.slice(0, -1));
  return parent?.[path[path.length - 1]] ?? null;
}

/** Copy of the tree with the list at `parent` replaced by `fn(list)`. */
function withList(
  items: HotItem[],
  parent: TreePath,
  fn: (list: HotItem[]) => HotItem[],
): HotItem[] | null {
  if (parent.length === 0) return fn(items);
  const [head, ...rest] = parent;
  const node = items[head];
  if (!node || node.type !== 'submenu') return null;
  const inner = withList(node.items, rest, fn);
  if (!inner) return null;
  const copy = [...items];
  copy[head] = { ...node, items: inner };
  return copy;
}

export function canInsert(items: HotItem[], parent: TreePath, item: HotItem): boolean {
  if (countItems(items) + 1 + (item.type === 'submenu' ? countItems(item.items) : 0) > LIMITS.items)
    return false;
  return parent.length + depthOf(item) <= LIMITS.depth;
}

/** Insert `item` into the list at `parent` (index clamped); `null` when refused. */
export function insertAt(
  items: HotItem[],
  parent: TreePath,
  index: number,
  item: HotItem,
): HotItem[] | null {
  if (!canInsert(items, parent, item)) return null;
  return withList(items, parent, (list) => {
    const at = Math.max(0, Math.min(index, list.length));
    return [...list.slice(0, at), item, ...list.slice(at)];
  });
}

export function removeAt(items: HotItem[], path: TreePath): HotItem[] | null {
  if (path.length === 0 || !getAt(items, path)) return null;
  const idx = path[path.length - 1];
  return withList(items, path.slice(0, -1), (list) => list.filter((_, i) => i !== idx));
}

export function replaceAt(items: HotItem[], path: TreePath, item: HotItem): HotItem[] | null {
  if (path.length === 0 || !getAt(items, path)) return null;
  const idx = path[path.length - 1];
  return withList(items, path.slice(0, -1), (list) => list.map((it, i) => (i === idx ? item : it)));
}

/** Swap with the sibling above/below; returns the new tree and the item's new path. */
export function moveWithin(
  items: HotItem[],
  path: TreePath,
  delta: -1 | 1,
): { items: HotItem[]; path: TreePath } | null {
  const idx = path[path.length - 1];
  const target = idx + delta;
  const parent = path.slice(0, -1);
  const list = itemsAt(items, parent);
  if (!list || !list[idx] || target < 0 || target >= list.length) return null;
  const next = withList(items, parent, (l) => {
    const c = [...l];
    [c[idx], c[target]] = [c[target], c[idx]];
    return c;
  });
  return next ? { items: next, path: [...parent, target] } : null;
}

const startsWith = (path: TreePath, prefix: TreePath) =>
  prefix.length <= path.length && prefix.every((v, i) => path[i] === v);

function findRef(items: HotItem[], ref: HotItem, prefix: TreePath = []): TreePath | null {
  for (let i = 0; i < items.length; i++) {
    if (items[i] === ref) return [...prefix, i];
    const it = items[i];
    if (it.type === 'submenu') {
      const hit = findRef(it.items, ref, [...prefix, i]);
      if (hit) return hit;
    }
  }
  return null;
}

/**
 * Move an item to `index` inside `toParent` (both expressed against the CURRENT tree). A submenu
 * can never move into itself or its own descendants. Returns the new tree and the new path.
 * Done as insert-a-copy then remove-the-original-by-identity, so no index arithmetic can drift.
 */
export function moveTo(
  items: HotItem[],
  from: TreePath,
  toParent: TreePath,
  index: number,
): { items: HotItem[]; path: TreePath } | null {
  const item = getAt(items, from);
  if (!item || startsWith(toParent, from)) return null;
  if (toParent.length + depthOf(item) > LIMITS.depth) return null;
  const copy: HotItem = { ...item };
  const withCopy = withList(items, toParent, (list) => {
    const at = Math.max(0, Math.min(index, list.length));
    return [...list.slice(0, at), copy, ...list.slice(at)];
  });
  if (!withCopy) return null;
  const original = findRef(withCopy, item);
  if (!original) return null;
  const next = removeAt(withCopy, original);
  const path = next && findRef(next, copy);
  return next && path ? { items: next, path } : null;
}

/**
 * Move the item into the submenu directly above it (as that submenu's last entry). `null` when the
 * item above is not a submenu, the item is first in its list, or the depth/size limits refuse it.
 */
export function indent(
  items: HotItem[],
  path: TreePath,
): { items: HotItem[]; path: TreePath } | null {
  const idx = path[path.length - 1];
  if (path.length === 0 || idx === undefined || idx < 1) return null;
  const parent = path.slice(0, -1);
  const above = getAt(items, [...parent, idx - 1]);
  if (!above || above.type !== 'submenu') return null;
  return moveTo(items, path, [...parent, idx - 1], above.items.length);
}

/**
 * Move the item out of its submenu, to the level of that submenu, right after it. `null` for an
 * item that is already at the top level.
 */
export function outdent(
  items: HotItem[],
  path: TreePath,
): { items: HotItem[]; path: TreePath } | null {
  if (path.length < 2) return null;
  const owner = path.slice(0, -1);
  return moveTo(items, path, owner.slice(0, -1), owner[owner.length - 1] + 1);
}

// ── folder helpers ──

export function normalizeFolder(p: string): string {
  const t = p.trim().replace(/\//g, '\\');
  const trimmed = t.replace(/\\+$/, '');
  // Keep "C:\" as is: "C:" alone means "current dir on C".
  return (/^[A-Za-z]:$/.test(trimmed) ? `${trimmed}\\` : trimmed).toLowerCase();
}

export function samePath(a: string, b: string): boolean {
  return normalizeFolder(a) === normalizeFolder(b);
}

export function basename(path: string): string {
  return (
    path
      .replace(/[\\/]+$/, '')
      .split(/[\\/]/)
      .pop() || path
  );
}

/** Every folder item (any depth) as a flat list — for suggestions and "is this already listed". */
export function dirEntries(items: HotItem[]): { title: string; path: string }[] {
  const out: { title: string; path: string }[] = [];
  for (const it of items) {
    if (it.type === 'dir') out.push({ title: titleParts(it.title).label, path: it.path });
    else if (it.type === 'submenu') out.push(...dirEntries(it.items));
  }
  return out;
}

/** Index path of the first folder item pointing at `path`, or `null`. */
export function findDir(items: HotItem[], path: string, prefix: TreePath = []): TreePath | null {
  for (let i = 0; i < items.length; i++) {
    const it = items[i];
    if (it.type === 'dir' && samePath(it.path, path)) return [...prefix, i];
    if (it.type === 'submenu') {
      const hit = findDir(it.items, path, [...prefix, i]);
      if (hit) return hit;
    }
  }
  return null;
}

/** First-level and nested paths of every folder item matching `path` (Remove current folder). */
export function findAllDirs(items: HotItem[], path: string, prefix: TreePath = []): TreePath[] {
  const out: TreePath[] = [];
  items.forEach((it, i) => {
    if (it.type === 'dir' && samePath(it.path, path)) out.push([...prefix, i]);
    else if (it.type === 'submenu') out.push(...findAllDirs(it.items, path, [...prefix, i]));
  });
  return out;
}

/** Remove several items (given as index paths) without index drift: deepest/last first. */
export function removeMany(items: HotItem[], paths: TreePath[]): HotItem[] {
  const ordered = [...paths].sort((a, b) => {
    for (let i = 0; i < Math.max(a.length, b.length); i++) {
      if (a[i] === undefined) return 1;
      if (b[i] === undefined) return -1;
      if (a[i] !== b[i]) return b[i] - a[i];
    }
    return 0;
  });
  let cur = items;
  for (const p of ordered) cur = removeAt(cur, p) ?? cur;
  return cur;
}

/** Append `extra` to `base` honouring the limits; items that do not fit are dropped. */
export function appendItems(
  base: HotItem[],
  extra: HotItem[],
): { items: HotItem[]; dropped: number } {
  let cur = base;
  let dropped = 0;
  for (const it of extra) {
    const next = insertAt(cur, [], cur.length, it);
    if (next) cur = next;
    else dropped++;
  }
  return { items: cur, dropped };
}
