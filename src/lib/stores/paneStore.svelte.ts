/**
 * Per-pane browsing state: tabs with independent back/forward history.
 * A pane always keeps exactly ≥1 tab; closing the last tab resets it to
 * `homePath` instead of destroying the pane (decision D19).
 */
import { formatDate, formatSize, parentOf } from '../format';
import { normalizeDrivePath } from '../crumbs';
import { kindOf, type Kind } from '../kind';
import { FsError } from '../ipc/types';
import type { FileEntry } from '../ipc/types';
import { listDir } from '../ipc/tauri';
import { ui } from '../uiState.svelte';

/** Display-ready row: FileEntry + derived visuals, computed once per load. */
export interface Row {
  entry: FileEntry;
  kind: Kind;
  sizeText: string;
  modText: string;
}

export interface TabState {
  label: string;
  path: string;
  back: string[];
  fwd: string[];
  rows: Row[];
  loading: boolean;
  error: FsError | null;
  /** Small notice after the tab was moved to a nearer existing folder (FER-457). Cleared on the next navigation. */
  notice?: string | null;
}

export type Fetcher = (path: string) => Promise<FileEntry[]>;

/**
 * Listing rows are replaced wholesale on every load and never mutated in place, so they must NOT be wrapped by
 * `$state`'s deep proxy: on a 10k-file folder that proxy cost ~24 MB of JS heap (a Proxy + handler closures + a
 * signal map per row and per entry; FER-455). Svelte only proxies plain objects/arrays (exact Object/Array
 * prototype), so an Array subclass stays raw — assigning `tab.rows = …` is still reactive through the tab proxy.
 */
class RawRows extends Array<Row> {}
export function rawRows(rows: Row[]): Row[] {
  return RawRows.from(rows);
}

/** Label for a tab: last path segment (or the path itself for roots). */
export function baseName(path: string): string {
  const norm = path.replace(/[\\/]+$/, '');
  const i = Math.max(norm.lastIndexOf('/'), norm.lastIndexOf('\\'));
  if (i === -1) return norm || path;
  const base = norm.slice(i + 1);
  return base || norm; // "C:" for drive roots
}

export function makeTab(path: string): TabState {
  return { label: baseName(path), path, back: [], fwd: [], rows: [], loading: false, error: null };
}

// ── FER-395 session-persistence hook ──────────────────────────────────────
// prefsStore registers a listener here; every tab/path mutation notifies it
// so the session snapshot can be re-collected. Default noop keeps PaneStore
// dependency-free (store tests never touch persistence).
let tabsChangedHook: () => void = () => {};
export function setTabsChangedHook(hook: () => void): void {
  tabsChangedHook = hook;
}
function notifyTabsChanged(): void {
  tabsChangedHook();
}

/** Keep in sync with MAX_SESSION_TABS (ipc/prefs). */
const RESTORE_MAX_TABS = 12;

export class PaneStore {
  tabs = $state<TabState[]>([]);
  current = $state(0);

  /**
   * Successful deliberate navigations (open/back/forward/up). Refresh,
   * new-tab, startup restore and failed loads never call it (phase-4 spec §4).
   */
  onVisit: ((path: string) => void) | null = null;

  private seq = 0;

  /** Real home folder, the last resort when a restored folder and all its parents are gone (FER-457). */
  recoveryHome: string | null = null;

  constructor(
    private fetcher: Fetcher = listDir,
    /** Home folder for new tabs. Desktop sets it from the real home dir at boot (FilePane.bootTabs). */
    public homePath: string = 'C:\\',
  ) {}

  get tab(): TabState {
    return this.tabs[this.current] ?? this.tabs[0];
  }

  get path(): string {
    return this.tab?.path ?? '';
  }

  get rows(): Row[] {
    return this.tab?.rows ?? [];
  }

  get loading(): boolean {
    return this.tab?.loading ?? false;
  }

  get error(): FsError | null {
    return this.tab?.error ?? null;
  }

  get notice(): string | null {
    return this.tab?.notice ?? null;
  }

  get canBack(): boolean {
    return (this.tab?.back.length ?? 0) > 0;
  }

  get canFwd(): boolean {
    return (this.tab?.fwd.length ?? 0) > 0;
  }

  /** History push for `open`: truncate fwd, keep back bounded. */
  private pushHistory(from: string) {
    const t = this.tab;
    t.back.push(from);
    if (t.back.length > 100) t.back.shift();
    t.fwd = [];
  }

  /** Navigate the CURRENT tab to a new path (recorded in history). */
  async open(path: string): Promise<void> {
    const t = this.tab;
    if (!t) return;
    if (path !== t.path) this.pushHistory(t.path);
    await this.loadInto(t, path, true);
  }

  /** Refresh current path without touching history. */
  refresh(): Promise<void> {
    // A folder removed from outside while open climbs to the nearest existing parent (FER-457).
    return this.loadInto(this.tab, this.tab.path, false, true);
  }

  async back(): Promise<void> {
    const t = this.tab;
    if (!t || t.back.length === 0) return;
    const target = t.back.pop()!;
    t.fwd.push(t.path);
    await this.loadInto(t, target, true);
  }

  async forward(): Promise<void> {
    const t = this.tab;
    if (!t || t.fwd.length === 0) return;
    const target = t.fwd.pop()!;
    t.back.push(t.path);
    await this.loadInto(t, target, true);
  }

  async goUp(): Promise<void> {
    const parent = parentOf(this.tab.path);
    if (parent) await this.open(parent);
  }

  newTab(path: string = this.homePath): Promise<void> {
    const t = makeTab(path);
    // FER-395: "open next to current" inserts right after the ACTIVE tab and
    // activates it; the pref off keeps the classic append-at-the-end. The
    // index clamps to the strip bounds (an empty strip inserts at 0).
    if (ui.tabs.openNextToCurrent) {
      const at = Math.min(this.current + 1, this.tabs.length);
      this.tabs.splice(at, 0, t);
      this.current = at;
    } else {
      this.tabs.push(t);
      this.current = this.tabs.length - 1;
    }
    notifyTabsChanged();
    return this.loadInto(this.tab, path, false);
  }

  /** FER-482 Ctrl+U: hand over the tab strip (tabs with their rows/history + active index). */
  takeTabs(): { tabs: TabState[]; current: number } {
    return { tabs: [...this.tabs], current: this.current };
  }

  /** FER-482 Ctrl+U: adopt another pane's tab strip as-is (no reload; an in-flight load keeps writing into its tab). */
  adoptTabs(tabs: TabState[], current: number): void {
    if (tabs.length === 0) return;
    this.tabs = tabs;
    this.current = current >= 0 && current < tabs.length ? current : 0;
    notifyTabsChanged();
  }

  selectTab(i: number) {
    if (i >= 0 && i < this.tabs.length) {
      this.current = i;
      notifyTabsChanged();
    }
  }

  /** Close tab i. Focus goes to the right neighbor, else the left one.
   *  Closing the LAST tab resets that tab to home (D19) — a pane never dies. */
  async closeTab(i: number): Promise<void> {
    if (i < 0 || i >= this.tabs.length) return;
    if (this.tabs.length === 1) {
      this.tabs[0] = makeTab(this.homePath);
      this.current = 0;
      notifyTabsChanged();
      return this.loadInto(this.tabs[0], this.homePath, false);
    }
    this.tabs.splice(i, 1);
    this.current = Math.min(i, this.tabs.length - 1);
    notifyTabsChanged();
  }

  /** FER-395 session restore: open every saved path as a tab and select the
   *  saved active one. Restore only navigates — per-tab fetch errors surface
   *  inside their tab (no dialogs, nothing is executed). Tabs load
   *  sequentially because the store's stale-response guard supersedes
   *  concurrent loads. A pathless restore leaves the pane untouched. */
  async restore(paths: string[], current: number): Promise<void> {
    const clean = paths
      .filter((p) => typeof p === 'string' && p.length > 0)
      .map(normalizeDrivePath) // legacy `C:` / `/C:` tabs saved by the old breadcrumb
      .slice(0, RESTORE_MAX_TABS);
    if (clean.length === 0) return;
    this.tabs = clean.map(makeTab);
    this.current =
      Number.isInteger(current) && current >= 0 && current < clean.length ? current : 0;
    notifyTabsChanged();
    await this.loadInto(this.tabs[this.current], clean[this.current], false, true); // active tab first
    for (let i = 0; i < clean.length; i += 1) {
      if (i !== this.current) await this.loadInto(this.tabs[i], clean[i], false, true);
    }
  }

  /** Fetch and store into a specific tab with stale-response protection.
   *  History is managed by the callers (open/back/forward); `visit` marks
   *  deliberate navigation whose successful commit reports onVisit. */
  private async loadInto(
    t: TabState,
    path: string,
    visit: boolean,
    recover = false,
  ): Promise<void> {
    const mySeq = ++this.seq;
    t.path = path;
    t.label = baseName(path);
    notifyTabsChanged(); // open/back/forward/navigation mutates the session
    t.loading = true;
    t.error = null;
    if (!recover) t.notice = null;
    try {
      const entries = await this.fetcher(path);
      if (mySeq !== this.seq) return; // superseded by a newer request
      t.rows = rawRows(entries.map(toRow));
      t.loading = false;
      if (visit) this.onVisit?.(path);
    } catch (e) {
      if (mySeq !== this.seq) return;
      const err = e instanceof FsError ? e : FsError.fromIPC(e);
      if (recover && (err.code === 'notFound' || err.code === 'notADirectory')) {
        const found = await this.nearestExisting(path);
        if (mySeq !== this.seq) return;
        if (found) {
          t.path = found.path;
          t.label = baseName(found.path);
          t.rows = rawRows(found.entries.map(toRow));
          t.loading = false;
          t.notice = `"${path}" no longer exists. Showing ${found.path} instead.`;
          notifyTabsChanged();
          return;
        }
      }
      t.rows = [];
      t.loading = false;
      t.error = err;
    }
  }

  /**
   * Nearest parent of `path` that still lists; for paths with no volume concept (mock/posix) the home folder is
   * the last resort. null when none works. The volume root (`X:\` or `\\server\share\`) is tried ONCE first: if it
   * is unreachable (drive unplugged, network share down) nothing is changed and the caller keeps the original
   * error and path, so a temporarily missing volume never rewrites the saved session.
   */
  private async nearestExisting(
    path: string,
  ): Promise<{ path: string; entries: FileEntry[] } | null> {
    const root = volumeRoot(path);
    let rootEntries: FileEntry[] | null = null;
    if (root) {
      try {
        rootEntries = await this.fetcher(root);
      } catch {
        return null;
      }
    }
    const candidates: string[] = [];
    for (let p = parentOf(path); p && p !== path && !candidates.includes(p); p = parentOf(p)) {
      if (root && p.length <= root.length) break; // the volume root is tried last; never climb above it
      candidates.push(p);
    }
    for (const candidate of candidates) {
      try {
        return { path: candidate, entries: await this.fetcher(candidate) };
      } catch {
        // keep climbing
      }
    }
    if (root && rootEntries && root !== path) return { path: root, entries: rootEntries };
    if (root) return null;
    const home = this.recoveryHome ?? this.homePath;
    if (home === path) return null;
    try {
      return { path: home, entries: await this.fetcher(home) };
    } catch {
      return null;
    }
  }
}

/**
 * `X:\` for drive paths, `\\server\share\` for UNC paths, null for anything else (mock/posix). The long-path
 * forms keep their prefix so the root is a prefix of every parent of the path (FER-462 R2): `\\?\X:\...` gives
 * `\\?\X:\` and `\\?\UNC\server\share\...` gives `\\?\UNC\server\share\`.
 */
export function volumeRoot(path: string): string | null {
  const longDrive = /^[\\/]{2}\?[\\/]([A-Za-z]):[\\/]/.exec(path);
  if (longDrive) return `\\\\?\\${longDrive[1]}:\\`;
  const longUnc = /^[\\/]{2}\?[\\/]UNC[\\/]+([^\\/?.][^\\/]*)[\\/]+([^\\/]+)/i.exec(path);
  if (longUnc) return `\\\\?\\UNC\\${longUnc[1]}\\${longUnc[2]}\\`;
  const drive = /^([A-Za-z]):[\\/]/.exec(path);
  if (drive) return `${drive[1]}:\\`;
  const unc = /^[\\/]{2}([^\\/?.][^\\/]*)[\\/]+([^\\/]+)/.exec(path);
  if (unc) return `\\\\${unc[1]}\\${unc[2]}\\`;
  return null;
}

export function toRow(entry: FileEntry): Row {
  return {
    entry,
    kind: kindOf(entry),
    sizeText: entry.isDir ? '' : formatSize(entry.size),
    modText: formatDate(entry.modifiedMs),
  };
}
