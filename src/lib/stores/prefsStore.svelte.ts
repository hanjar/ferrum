/**
 * Preference persistence. Loaded once at startup and applied to the `ui`
 * store; from then on every change to a persisted ui field is saved
 * automatically (debounced) — which is why the Settings dialog needs no
 * "Save" button. A failed load keeps the current ui values (browser dev /
 * backend unavailable) and still reports ready. A failed SAVE surfaces as
 * `saveError` (shown in Settings) and retries on the next change; nothing
 * ever claims "saved" that isn't (FER-370 APP-03).
 */
import { normalizeIconTheme } from '../fileIcon';
import {
  ui,
  sanitizePaneSplit,
  sanitizeTabs,
  sanitizePreview,
  type Density,
  type FontLevel,
  type IconStyle,
  type PaneId,
} from '../uiState.svelte';
import {
  defaultPrefs,
  loadPrefs,
  normalizeFileOperationPrefs,
  sanitizeWarnHours,
  savePrefs,
  sanitizeSession,
  MAX_SESSION_TABS,
  type FileOperationPrefs,
  type Prefs,
  type SessionSnapshot,
} from '../ipc/prefs';
import type { TerminalChoice } from '../ipc/shellOpen';
import { paneById } from './panesRegistry.svelte';
import { setTabsChangedHook } from './paneStore.svelte';

const SAVE_DEBOUNCE_MS = 300;

const DENSITIES: Density[] = ['compact', 'comfortable'];
const ICON_STYLES: IconStyle[] = ['colored', 'monochrome'];
const FONT_LEVELS: FontLevel[] = ['s', 'm', 'l', 'xl'];
const TERMINALS: TerminalChoice[] = ['auto', 'wt', 'cmd', 'powershell'];
const fileOperations = $state<FileOperationPrefs>(normalizeFileOperationPrefs(defaultPrefs()));

function operationPrefs(): unknown[] {
  return [
    fileOperations.confirmBeforeDeleting,
    fileOperations.verifyAfterCopy,
    fileOperations.copyBufferKiB,
    fileOperations.copyThreads,
    fileOperations.copySafeWrite,
    fileOperations.copyPriority,
    fileOperations.conflictPolicy,
    fileOperations.preserveTimestamps,
    fileOperations.preservePermissions,
  ];
}

/** Snapshot key of the persisted ui fields — dedupes the initial-apply echo.
 *  Reads every leaf field explicitly (object references would break the
 *  $effect tracking) and folds in the current session snapshot. */
function snapshot(session: unknown): string {
  return JSON.stringify([
    ui.theme,
    ui.palette,
    ui.showHidden,
    ui.terminal,
    ui.paneSplit,
    ui.tabs.openNextToCurrent,
    ui.tabs.restoreTabs,
    ui.tabs.tabWidth,
    ui.tabs.closeOnMiddleClick,
    ui.preview.openOnLaunch,
    ui.preview.maxTextBytes,
    ui.preview.syntaxHighlight,
    ui.preview.autoplayVideo,
    ui.preview.readMetadata,
    ui.density,
    ui.iconStyle,
    ui.iconTheme,
    ui.uiFontLevel,
    ui.showExtensions,
    ui.foldersFirst,
    ui.diskUsageFastOffer,
    ui.diskUsageCacheWarnHours,
    ui.addressOwnRow,
    ui.indexerFolderSizes,
    ui.syncJournalSkip,
    ui.rarExePath,
    ...operationPrefs(),
    session,
  ]);
}

/** FER-395: both panes' live session as plain data (validated like the
 *  backend — non-empty paths, capped tabs, in-range current). */
function collectSession(): SessionSnapshot | null {
  const panes: SessionSnapshot['panes'] = [];
  for (const id of [0, 1] as PaneId[]) {
    const raw = paneById(id)?.getSession?.();
    if (!raw) continue;
    const tabs = raw.tabs
      .filter((p) => typeof p === 'string' && p.length > 0)
      .slice(0, MAX_SESSION_TABS);
    if (tabs.length === 0) continue;
    panes.push({ tabs, current: raw.current >= 0 && raw.current < tabs.length ? raw.current : 0 });
  }
  return panes.length > 0 ? { panes } : null;
}
export class PrefsStore {
  /** True once init() settled (load attempted, ui applied). */
  ready = $state(false);
  /** True while the last save attempt failed; cleared on a successful save. */
  saveError = $state(false);
  /** Mirrored reads of the persisted ui fields for convenient binding. */
  theme = $derived(ui.theme);
  palette = $derived(ui.palette);
  showHidden = $derived(ui.showHidden);
  fileOperations = fileOperations;

  private loading = false;
  private timer: ReturnType<typeof setTimeout> | undefined;
  private lastSaved: string | null = null;
  private stopPersisting: (() => void) | undefined;
  /** Shared boot promise so FilePane mount and App startup await ONE load. */
  private boot: Promise<void> | undefined;
  /** FER-395 session RESTORED FROM DISK at init (the only one FilePane's
   *  boot may reopen — the live snapshot below never re-boots this run). */
  private loadedSession: SessionSnapshot | null = null;
  /** FER-395 live session of the running app, kept fresh by the paneStore
   *  hook and written by persist(). */
  private liveSession: SessionSnapshot | null = null;

  /** Load persisted prefs into `ui`, then keep persisting changes.
   *  Concurrent callers share the one in-flight load. */
  init(): Promise<void> {
    if (!this.boot) this.boot = this.load();
    return this.boot;
  }

  private async load(): Promise<void> {
    this.loading = true;
    try {
      const p = await loadPrefs();
      ui.setTheme(p.theme); // also reapplies the theme to the document
      ui.palette = p.palette;
      ui.showHidden = p.showHidden;
      // Older configs predate these fields; the backend normalizes, but a
      // defensive guard keeps unknown values out of the UI (as with terminal).
      if (DENSITIES.includes(p.density)) ui.density = p.density;
      if (ICON_STYLES.includes(p.iconStyle)) ui.iconStyle = p.iconStyle;
      ui.iconTheme = normalizeIconTheme(p.iconTheme);
      if (FONT_LEVELS.includes(p.uiFontLevel)) ui.uiFontLevel = p.uiFontLevel;
      ui.showExtensions = p.showExtensions;
      ui.foldersFirst = p.foldersFirst;
      ui.diskUsageFastOffer = p.diskUsageFastOffer !== false;
      ui.diskUsageCacheWarnHours = sanitizeWarnHours(p.diskUsageCacheWarnHours);
      ui.addressOwnRow = p.addressOwnRow === true;
      ui.indexerFolderSizes = p.indexerFolderSizes === true;
      ui.syncJournalSkip = p.syncJournalSkip === true;
      ui.rarExePath = typeof p.rarExePath === 'string' ? p.rarExePath : null;
      // Older configs have no terminal field; the backend normalizes, but a
      // defensive guard keeps an unknown value from reaching the launcher.
      if (TERMINALS.includes(p.terminal)) ui.terminal = p.terminal;
      // Same defensive stance for the pane split (FER-406): garbage from an
      // old or hand-edited config falls back to 50/50 instead of breaking
      // the grid template.
      ui.paneSplit = sanitizePaneSplit(p.paneSplit);
      // FER-395: the tabs section sanitizes field-wise like the backend.
      ui.tabs = sanitizeTabs(p.tabs);
      // FER-396: same field-wise stance for the preview section.
      ui.preview = sanitizePreview(p.preview);
      // The session is restore data, not a preference: restore=off ignores
      // it, and the next persist clears the stored snapshot (the pref
      // itself is never touched).
      this.loadedSession = ui.tabs.restoreTabs ? sanitizeSession(p.session) : null;
      Object.assign(fileOperations, normalizeFileOperationPrefs(p));
    } catch {
      // Browser dev or backend failure: keep the current ui defaults.
      console.warn('[prefs] load failed — keeping in-memory defaults');
    } finally {
      this.loading = false;
      this.lastSaved = snapshot(this.sessionForPersist());
      this.ready = true;
      this.startPersisting();
    }
  }

  /** What persist() would write right now: the live session once panes have
   *  reported, falling back to the disk session so a successful restore that
   *  re-collects identical content doesn't re-save (echo dedupe). */
  private sessionForPersist(): SessionSnapshot | null {
    return ui.tabs.restoreTabs ? (this.liveSession ?? this.loadedSession) : null;
  }

  /** The launch session restored from disk, if any (read by FilePane's
   *  boot; live captures after boot never restore). */
  get session(): SessionSnapshot | null {
    return this.loadedSession;
  }

  /** FER-395: paneStore's tab-mutation hook — refresh the live session
   *  snapshot and ride the same debounced save as the prefs (the
   *  snapshot-key dedupe skips writes that change nothing). */
  noteSessionChanged(): void {
    if (!ui.tabs.restoreTabs) return; // off: the field is cleared on the next persist
    this.liveSession = collectSession();
    if (this.ready) this.scheduleSave();
  }

  /** Save the current prefs NOW (no debounce), dropping any pending debounced
   *  save. Used where the backend must read the just-edited value from disk
   *  (FER-505: the Rar.exe "Tes" probe reads `rarExePath` from prefs on disk).
   *  Resolves `true` when the save succeeded; a no-op `false` before init. */
  async flush(): Promise<boolean> {
    clearTimeout(this.timer);
    this.timer = undefined;
    if (this.loading || !this.ready) return false;
    await this.persist();
    return !this.saveError;
  }

  /** Detach the persist effect and drop a pending save. Exists so tests (and
   *  any future teardown path) can dispose a store without leaking roots. */
  stop(): void {
    this.stopPersisting?.();
    this.stopPersisting = undefined;
    clearTimeout(this.timer);
  }

  /** Subscribe to the persisted ui fields; every change schedules a
   *  debounced save. Runs once, after the initial application. */
  private startPersisting(): void {
    this.stopPersisting = $effect.root(() => {
      $effect(() => {
        void snapshot(this.sessionForPersist()); // track every persisted ui field + session
        this.scheduleSave();
      });
    });
  }

  private scheduleSave(): void {
    if (this.loading || !this.ready) return;
    const snap = snapshot(this.sessionForPersist());
    if (snap === this.lastSaved) return; // echo of the initial application
    clearTimeout(this.timer);
    this.timer = setTimeout(() => void this.persist(), SAVE_DEBOUNCE_MS);
  }

  private async persist(): Promise<void> {
    // Full snapshot every time: rapid switching across controls coalesces
    // into one save of the FINAL state — no intermediate state can be lost,
    // and a retry after failure never writes a stale partial set.
    const p: Prefs = {
      theme: ui.theme,
      palette: ui.palette,
      showHidden: ui.showHidden,
      terminal: ui.terminal,
      paneSplit: ui.paneSplit,
      tabs: { ...ui.tabs },
      preview: { ...ui.preview },
      session: this.sessionForPersist(),
      density: ui.density,
      iconStyle: ui.iconStyle,
      iconTheme: ui.iconTheme,
      uiFontLevel: ui.uiFontLevel,
      showExtensions: ui.showExtensions,
      foldersFirst: ui.foldersFirst,
      diskUsageFastOffer: ui.diskUsageFastOffer,
      diskUsageCacheWarnHours: sanitizeWarnHours(ui.diskUsageCacheWarnHours),
      addressOwnRow: ui.addressOwnRow,
      indexerFolderSizes: ui.indexerFolderSizes,
      syncJournalSkip: ui.syncJournalSkip,
      rarExePath: ui.rarExePath,
      ...normalizeFileOperationPrefs(fileOperations),
    };
    this.lastSaved = snapshot(this.sessionForPersist());
    try {
      await savePrefs(p);
      this.saveError = false;
    } catch (e) {
      this.saveError = true; // visible in Settings; next change retries
      console.warn('[prefs] save failed', e);
    }
  }
}

export const prefs = new PrefsStore();

// FER-395: pane mutations (open/close/navigate) refresh the session snapshot
// through this hook, so panes stay oblivious to persistence.
setTabsChangedHook(() => prefs.noteSessionChanged());
