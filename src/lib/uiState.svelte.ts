/**
 * Global shell state (runes). Filesystem state is intentionally NOT here —
 * it arrives with task 04+ via typed IPC services. This module only owns
 * chrome-level state shared across components.
 */
import type { TerminalChoice } from './ipc/shellOpen';
import { DEFAULT_ICON_THEME, type IconTheme } from './fileIcon';

export type { IconTheme };

export type PaneId = 0 | 1;
export type Theme = 'dark' | 'light' | 'system';
/** Appearance topic (FER-370): six stable palette IDs (TOKEN-CONTRACT §1). */
export type PaletteName = 'violet' | 'electric' | 'aqua' | 'green' | 'yellow' | 'orange';
/** Tab sizing (FER-395): fit shrinks to the label, fixed is uniform width. */
export type TabWidth = 'fit' | 'fixed';

/** FER-395 Settings > Tabs — how tabs open, close, size and persist. */
export interface TabsSettings {
  /** New tabs open right after the ACTIVE tab instead of at the end. */
  openNextToCurrent: boolean;
  /** Reopen the last session's tabs per pane at launch. */
  restoreTabs: boolean;
  tabWidth: TabWidth;
  /** Middle-click (auxclick button 1) closes the clicked tab. */
  closeOnMiddleClick: boolean;
}

/** Defaults mirror the backend (prefs.rs TabsPrefs::default); serde only
 *  fills absent fields, saved user choices are never overwritten. */
export const TABS_DEFAULTS: TabsSettings = {
  openNextToCurrent: true,
  restoreTabs: true,
  tabWidth: 'fit',
  closeOnMiddleClick: true,
};

/** Saved-value sanitizer (FER-395): unknown or partial shapes fall back
 *  field-wise, mirroring the backend's load_from validation. */
export function sanitizeTabs(raw: unknown): TabsSettings {
  const r = (typeof raw === 'object' && raw !== null ? raw : {}) as Partial<TabsSettings>;
  return {
    openNextToCurrent:
      typeof r.openNextToCurrent === 'boolean'
        ? r.openNextToCurrent
        : TABS_DEFAULTS.openNextToCurrent,
    restoreTabs: typeof r.restoreTabs === 'boolean' ? r.restoreTabs : TABS_DEFAULTS.restoreTabs,
    tabWidth: r.tabWidth === 'fixed' || r.tabWidth === 'fit' ? r.tabWidth : TABS_DEFAULTS.tabWidth,
    closeOnMiddleClick:
      typeof r.closeOnMiddleClick === 'boolean'
        ? r.closeOnMiddleClick
        : TABS_DEFAULTS.closeOnMiddleClick,
  };
}

/** FER-396 Settings > Preview — what the Quick View panel renders. */
export interface PreviewSettings {
  /** Open the preview panel at launch (applied once, after prefs load). */
  openOnLaunch: boolean;
  /** Text preview ceiling in decimal bytes; larger files show metadata only.
   *  Always one of PREVIEW_TEXT_LIMITS (no unlimited option). */
  maxTextBytes: number;
  /** Colorize text/JSON/Markdown with the safe built-in tokenizer. */
  syntaxHighlight: boolean;
  /** Start supported videos automatically (still muted-by-policy in WebView). */
  autoplayVideo: boolean;
  /** Extract and show EXIF / media metadata. OFF: no extraction at all. */
  readMetadata: boolean;
}

/** Allowed text limits, decimal bytes (labels say MB/KB, never MiB). The
 *  backend ceiling (4 MiB) is above the largest choice. */
export const PREVIEW_TEXT_LIMITS: readonly number[] = [
  100_000, 500_000, 1_000_000, 2_000_000, 4_000_000,
];

/** Defaults mirror the backend (prefs.rs PreviewPrefs::default). */
export const PREVIEW_DEFAULTS: PreviewSettings = {
  openOnLaunch: false,
  maxTextBytes: 2_000_000,
  syntaxHighlight: true,
  autoplayVideo: false,
  readMetadata: true,
};

/** Saved-value sanitizer (FER-396): field-wise fallback to defaults. */
export function sanitizePreview(raw: unknown): PreviewSettings {
  const r = (typeof raw === 'object' && raw !== null ? raw : {}) as Partial<PreviewSettings>;
  const bool = (v: unknown, d: boolean) => (typeof v === 'boolean' ? v : d);
  return {
    openOnLaunch: bool(r.openOnLaunch, PREVIEW_DEFAULTS.openOnLaunch),
    maxTextBytes:
      typeof r.maxTextBytes === 'number' && PREVIEW_TEXT_LIMITS.includes(r.maxTextBytes)
        ? r.maxTextBytes
        : PREVIEW_DEFAULTS.maxTextBytes,
    syntaxHighlight: bool(r.syntaxHighlight, PREVIEW_DEFAULTS.syntaxHighlight),
    autoplayVideo: bool(r.autoplayVideo, PREVIEW_DEFAULTS.autoplayVideo),
    readMetadata: bool(r.readMetadata, PREVIEW_DEFAULTS.readMetadata),
  };
}

/** Left pane's share of the dual-pane grid (FER-406), as a fraction. */
export const PANE_SPLIT_MIN = 0.2;
export const PANE_SPLIT_MAX = 0.8;
export const PANE_SPLIT_DEFAULT = 0.5;

/** Arithmetic clamp for drag/keyboard results — always yields a valid split. */
export function clampPaneSplit(v: number): number {
  if (!Number.isFinite(v)) return PANE_SPLIT_DEFAULT;
  return Math.min(PANE_SPLIT_MAX, Math.max(PANE_SPLIT_MIN, v));
}

/** Saved-value sanitizer (FER-406): anything non-finite or outside the range
 *  resets to 50/50, mirroring the backend's load_from validation. */
export function sanitizePaneSplit(v: unknown): number {
  return typeof v === 'number' && Number.isFinite(v) && v >= PANE_SPLIT_MIN && v <= PANE_SPLIT_MAX
    ? v
    : PANE_SPLIT_DEFAULT;
}

export type Density = 'compact' | 'comfortable';
export type IconStyle = 'colored' | 'monochrome';
/** Named UI text scale (D50): global --uiStep offset of -1/0/+2/+4 px. */
export type FontLevel = 's' | 'm' | 'l' | 'xl';

/** The six Appearance palettes with their Settings labels (v3: FER-370). */
export const PALETTES: { value: PaletteName; label: string; dot: string }[] = [
  { value: 'violet', label: 'Pop Violet', dot: '#7B3FFF' },
  { value: 'electric', label: 'Electric', dot: '#2563FF' },
  { value: 'aqua', label: 'Aqua Signal', dot: '#0096E6' },
  { value: 'green', label: 'Fresh Mint', dot: '#00B86B' },
  { value: 'yellow', label: 'Sunshine', dot: '#FFC400' },
  { value: 'orange', label: 'Tangerine', dot: '#FF6A00' },
];

class UiState {
  activePane: PaneId = $state(0);
  sidebarOpen: boolean = $state(true);
  /** Sidebar content (F2-15 Tree action flips this to 'tree'). */
  sidebarMode: 'places' | 'tree' = $state('places');
  /** Panes stacked vertically instead of side by side (F2-15). */
  verticalLayout: boolean = $state(false);
  /** Left/top pane share of the grid, persisted (FER-406), kept in [0.2, 0.8]. */
  paneSplit: number = $state(PANE_SPLIT_DEFAULT);
  /** Quick View panel (F2-17). */
  quickViewOpen: boolean = $state(false);
  paletteOpen: boolean = $state(false);
  settingsOpen: boolean = $state(false);
  activityOpen: boolean = $state(false);
  /** Global "show hidden files" (D08) — one switch for both panes. */
  showHidden: boolean = $state(false);
  /** Comments column (F2-27 Show > Comments) — one switch for both panes. */
  showComments: boolean = $state(false);
  theme: Theme = $state('dark');
  palette: PaletteName = $state('violet');
  /** Row density: compact 26px / comfortable 30px (v3), token --row. */
  density: Density = $state('compact');
  /** File glyphs colored (per-palette --ft*) or monochrome (--text3). */
  iconStyle: IconStyle = $state('colored');
  /** FER-478 file icon shape: Line | Duotone | Label | Tile (independent of iconStyle). */
  iconTheme: IconTheme = $state(DEFAULT_ICON_THEME);
  /** Interface text scale level (D50) — drives the global --uiStep offset. */
  uiFontLevel: FontLevel = $state('m');
  /** Display-only extension hiding; real names stay for operations. */
  showExtensions: boolean = $state(true);
  /** Folders before files regardless of sort direction (D13 default). */
  foldersFirst: boolean = $state(true);
  /** FER-460: offer the elevated fast NTFS scan before Disk usage scans. */
  diskUsageFastOffer: boolean = $state(true);
  /** FER-464: a cached Disk usage scan older than this many hours is flagged. */
  diskUsageCacheWarnHours: number = $state(24);
  /** FER-491: address bar on its own full-width row above the pane toolbar. */
  addressOwnRow: boolean = $state(false);
  /** FER-477a: folder sizes from the FerrumIndexer (High performance mode only). */
  indexerFolderSizes: boolean = $state(false);
  /** FER-477b: sync may skip content reads the indexer journal proves unnecessary (opt-in). */
  syncJournalSkip: boolean = $state(false);
  /** FER-505/511: display-only mirror of the Rar.exe path stored on disk. Never persisted by
   *  the webview — it changes only through archive_rar_tool_set_path (native confirmation). */
  rarExePath: string | null = $state(null);
  /** Preferred terminal for Open Terminal Here (F2-25), persisted in prefs. */
  terminal: TerminalChoice = $state('auto');
  /** Tab behavior (FER-395 Settings > Tabs), persisted in prefs. */
  tabs: TabsSettings = $state({ ...TABS_DEFAULTS });
  /** Preview behavior (FER-396 Settings > Preview), persisted in prefs. */
  preview: PreviewSettings = $state({ ...PREVIEW_DEFAULTS });

  activate(pane: PaneId) {
    this.activePane = pane;
  }

  /** Tab alternates between the two panes (task 06). */
  togglePane() {
    this.activePane = this.activePane === 0 ? 1 : 0;
  }

  toggleVerticalLayout() {
    this.verticalLayout = !this.verticalLayout;
  }

  toggleTheme() {
    this.theme = this.effectiveTheme() === 'dark' ? 'light' : 'dark';
  }

  /** Resolve 'system' against the OS preference. */
  effectiveTheme(): 'dark' | 'light' {
    if (this.theme !== 'system') return this.theme;
    return typeof window !== 'undefined' &&
      window.matchMedia('(prefers-color-scheme: light)').matches
      ? 'light'
      : 'dark';
  }

  setTheme(t: Theme) {
    this.theme = t;
    this.applyToDocument();
  }

  toggleHidden() {
    this.showHidden = !this.showHidden;
  }

  applyToDocument(doc: Document = document) {
    doc.documentElement.dataset.theme = this.effectiveTheme();
    doc.documentElement.dataset.palette = this.palette;
    doc.documentElement.dataset.density = this.density;
    doc.documentElement.dataset.icon = this.iconStyle;
    doc.documentElement.dataset.iconTheme = this.iconTheme;
    doc.documentElement.dataset.ui = this.uiFontLevel;
  }

  /**
   * Live-apply OS theme changes while theme === 'system' (APP-01). The
   * stored preference stays 'system'. Returns a cleanup function — wire it
   * as the return value of an $effect so Svelte removes the listener.
   */
  watchSystemTheme(doc: Document = document): () => void {
    if (typeof window === 'undefined' || !window.matchMedia) return () => {};
    const mq = window.matchMedia('(prefers-color-scheme: light)');
    const onChange = () => {
      if (this.theme === 'system') this.applyToDocument(doc);
    };
    mq.addEventListener('change', onChange);
    return () => mq.removeEventListener('change', onChange);
  }
}

export const ui = new UiState();

/**
 * FER-561: true while a full-page view covers the file panes (today only Activity). Keyboard
 * shortcuts that act on files/panes must not run then, because the active pane is hidden.
 * Add future full pages (for example Sync profiles) here, in this one place.
 */
export function fullPageOpen(): boolean {
  return ui.activityOpen;
}
