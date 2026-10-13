/**
 * Directory Hotlist (F2-22, Ctrl+D; FER-458: nested, stored by the backend). The tree lives in
 * `hotlist.json` (schema v2) owned by the Rust side; this store mirrors it, drives the popup menu
 * and the Configure dialog, and runs the selected item.
 *
 * Safety: the backend refuses writes when the file is unavailable or changed from outside (the
 * store then reloads and tells the user); `command` items only run through the backend, which finds
 * the item by its place in the stored tree and asks in a NATIVE dialog the first time and whenever
 * the item, its arguments, its place or the hotlist file change (FER-467 R4/R5). Nothing here
 * launches anything on load/import.
 *
 * FER-467 R8: folder items on a network share (`\\host\share`) make Windows contact that host
 * (SMB/NTLM). They are marked in the menu and the palette, and opening one asks once per host that
 * was never confirmed before (`pendingNetwork`).
 */
import {
  appendItems,
  basename,
  dirEntries,
  emptyDoc,
  findAllDirs,
  findDir,
  insertAt,
  migrateV1,
  removeMany,
  V1_KEY,
  type CommandItem,
  type DirItem,
  type HotItem,
  type HotlistDoc,
} from '../hotlist/model';
import {
  defaultBackend,
  HotlistError,
  type HotlistBackend,
  type HotlistStatus,
  type HotlistView,
} from '../ipc/hotlist';
import { activePane, inactivePane } from './panesRegistry.svelte';
import { KnownHosts, networkHost, networkHosts } from '../networkPath';

export interface LegacyStorage {
  getItem(key: string): string | null;
  removeItem(key: string): void;
}

/** A network folder waiting for the user's OK (hosts never confirmed before). */
export interface PendingNetwork {
  item: DirItem;
  hosts: string[];
}

/** Index path of `item` (by identity) in `items`, or `null`. */
export function locate(items: HotItem[], item: HotItem): number[] | null {
  for (let i = 0; i < items.length; i++) {
    const it = items[i];
    if (it === item) return [i];
    if (it.type === 'submenu') {
      const sub = locate(it.items, item);
      if (sub) return [i, ...sub];
    }
  }
  return null;
}

function legacyStorage(): LegacyStorage | null {
  try {
    if (typeof localStorage !== 'undefined') return localStorage;
  } catch {
    /* storage unavailable (privacy mode) */
  }
  return null;
}

const NOTICE_MS = 5000;

export class HotlistStore {
  doc = $state<HotlistDoc>(emptyDoc());
  status = $state<HotlistStatus>('new');
  readOnly = $state(false);
  customLocation = $state<string | null>(null);
  loaded = $state(false);
  /** One-line message for the user (conflict, read-only, launch failure). */
  notice = $state('');
  /** Popup menu open state; `menuPane` decides where it anchors. */
  menuOpen = $state(false);
  menuPane = $state<0 | 1>(0);
  configureOpen = $state(false);
  pendingNetwork = $state<PendingNetwork | null>(null);
  /** FER-467 R7: why the custom location is refused, if it is. */
  locationProblem = $state<string | null>(null);

  private noticeTimer: ReturnType<typeof setTimeout> | undefined;

  constructor(
    private backend: HotlistBackend = defaultBackend(),
    private legacy: LegacyStorage | null = legacyStorage(),
    private knownHosts: KnownHosts = new KnownHosts(),
  ) {}

  private apply(view: HotlistView): void {
    this.doc = view.doc;
    this.status = view.status;
    this.readOnly = view.readOnly;
    this.customLocation = view.customLocation;
    this.locationProblem = view.locationProblem ?? null;
    this.loaded = true;
  }

  say(message: string): void {
    this.notice = message;
    clearTimeout(this.noticeTimer);
    if (message) this.noticeTimer = setTimeout(() => (this.notice = ''), NOTICE_MS);
  }

  /** (Re)read the hotlist; first run also migrates the old localStorage list. */
  async load(): Promise<void> {
    try {
      this.apply(await this.backend.load());
    } catch {
      this.loaded = true;
      return;
    }
    await this.migrateLegacy();
  }

  private async migrateLegacy(): Promise<void> {
    if (!this.legacy || this.status !== 'new' || this.doc.items.length > 0) return;
    const items = migrateV1(this.legacy.getItem(V1_KEY));
    if (items.length === 0) return;
    try {
      this.apply(await this.backend.save({ version: 2, items }));
      this.legacy.removeItem(V1_KEY);
    } catch {
      /* keep the v1 list for the next start */
    }
  }

  /** Flat list of folder items — path suggestions and "already listed" checks. */
  entries(): { title: string; path: string }[] {
    return dirEntries(this.doc.items);
  }

  count(): number {
    return this.entries().length;
  }

  isListed(path: string): boolean {
    return findDir(this.doc.items, path) !== null;
  }

  // ── popup menu / dialog ──

  openMenu(pane?: 0 | 1): void {
    this.menuPane = pane ?? 0;
    this.menuOpen = true;
    // Show what we have right away; pick up changes made on another PC while it is open.
    void this.load();
  }

  closeMenu(): void {
    this.menuOpen = false;
  }

  openConfigure(): void {
    this.menuOpen = false;
    this.configureOpen = true;
    void this.load();
  }

  closeConfigure(): void {
    this.configureOpen = false;
  }

  // ── editing ──

  /** Persist a whole new tree. `false` = refused (conflict/read-only/invalid); state is reloaded. */
  async replace(items: HotItem[], source?: string): Promise<boolean> {
    if (this.readOnly) {
      this.say('The hotlist file is unavailable — changes are paused until it can be read again.');
      return false;
    }
    const next: HotlistDoc = { version: 2, items };
    const origin = source ?? this.doc.source;
    if (origin) next.source = origin;
    try {
      this.apply(await this.backend.save(next));
      return true;
    } catch (e) {
      const kind = e instanceof HotlistError ? e.kind : 'unknown';
      if (kind === 'conflict') {
        await this.load();
        this.say('The hotlist file changed on disk and was reloaded. Repeat your change.');
      } else if (kind === 'read_only') {
        await this.load();
        this.say('The hotlist file is unavailable — nothing was saved.');
      } else {
        this.say('The hotlist could not be saved.');
      }
      return false;
    }
  }

  /** Add a folder at the end of the top level (deduped by path). */
  async addFolder(path: string, title?: string): Promise<boolean> {
    const trimmed = path.trim();
    if (trimmed === '') return false;
    if (this.isListed(trimmed)) {
      this.say('That folder is already in the hotlist.');
      return false;
    }
    const item: DirItem = { type: 'dir', title: title?.trim() || basename(trimmed), path: trimmed };
    const next = insertAt(this.doc.items, [], this.doc.items.length, item);
    if (!next) {
      this.say('The hotlist is full.');
      return false;
    }
    return this.replace(next);
  }

  /** Remove every entry that points at `path`. */
  async removeFolder(path: string): Promise<boolean> {
    const hits = findAllDirs(this.doc.items, path);
    if (hits.length === 0) return false;
    return this.replace(removeMany(this.doc.items, hits));
  }

  /** Append imported items (already validated by the backend) to the top level. */
  async importItems(items: HotItem[], source: string | undefined, replaceAll: boolean) {
    const base = replaceAll ? [] : this.doc.items;
    const { items: next, dropped } = appendItems(base, items);
    const ok = await this.replace(next, source);
    return { ok, dropped };
  }

  // ── running items ──

  /**
   * Open a folder item in the active pane (and its target path in the other pane). A network path
   * to a host the user never confirmed waits in `pendingNetwork` instead (FER-467 R8); the folder
   * a pane already shows counts as confirmed.
   */
  openDir(item: DirItem): void {
    const here = [activePane()?.path, inactivePane()?.path]
      .map((p) => (p ? networkHost(p) : null))
      .filter((h): h is string => h !== null);
    if (here.length > 0) this.knownHosts.add(here);
    const unknown = this.knownHosts.unknown(networkHosts([item.path, item.targetPath]));
    if (unknown.length > 0) {
      this.pendingNetwork = { item, hosts: unknown };
      return;
    }
    this.openDirNow(item);
  }

  private openDirNow(item: DirItem): void {
    const pane = activePane();
    if (!pane?.openPath) return;
    pane.openPath(item.path);
    if (item.targetPath) inactivePane()?.openPath?.(item.targetPath);
  }

  /** The user agreed to contact the pending hosts: remember them and open the folder. */
  confirmNetwork(): void {
    const pending = this.pendingNetwork;
    this.pendingNetwork = null;
    if (!pending) return;
    this.knownHosts.add(pending.hosts);
    this.openDirNow(pending.item);
  }

  cancelNetwork(): void {
    this.pendingNetwork = null;
  }

  /**
   * Run a command item at `position` (its index path; found by identity when omitted). The backend
   * checks the stored hotlist and shows the native confirmation when needed.
   */
  async runCommand(item: CommandItem, position?: number[]): Promise<void> {
    const at = position ?? locate(this.doc.items, item);
    if (!at) {
      this.say('That command is no longer in the hotlist.');
      return;
    }
    try {
      await this.backend.runCommand(at, item);
    } catch (e) {
      const kind = e instanceof HotlistError ? e.kind : 'unknown';
      if (kind === 'stale') {
        await this.load();
        this.say('The hotlist changed on disk — nothing was started. Pick the item again.');
        return;
      }
      this.say(
        kind === 'not_found'
          ? 'Could not start the command: program not found.'
          : kind === 'location_unsafe'
            ? 'The hotlist file location is not safe, so commands are disabled. Check Settings › Hotlist.'
            : kind === 'unsafe_args'
              ? 'Not started: the arguments contain characters a script could misread.'
              : 'Could not start the command.',
      );
    }
  }

  /** Activate any non-submenu item (menu pick, palette). `position` = its index path, if known. */
  activate(item: HotItem, position?: number[]): void {
    if (item.type === 'dir') this.openDir(item);
    else if (item.type === 'command') void this.runCommand(item, position);
  }

  // ── backend pass-throughs for Settings and import ──

  get api(): HotlistBackend {
    return this.backend;
  }

  applyView(view: HotlistView): void {
    this.apply(view);
  }
}

export const hotlist = new HotlistStore();
