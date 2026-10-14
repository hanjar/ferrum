/** Persisted preferences (backend: load_prefs / save_prefs in app config dir). */
import { invoke } from '@tauri-apps/api/core';
import type {
  Density,
  FontLevel,
  IconStyle,
  IconTheme,
  PaletteName,
  PreviewSettings,
  TabsSettings,
  Theme,
} from '../uiState.svelte';
import { PREVIEW_DEFAULTS } from '../uiState.svelte';
import type { TerminalChoice } from './shellOpen';
import { FsError } from './types';
import { isInTauri } from './tauri';

/** One pane's saved session (FER-395): tab paths in order + active index. */
export interface PaneSession {
  tabs: string[];
  current: number;
}

/** Both panes' saved sessions. */
export interface SessionSnapshot {
  panes: PaneSession[];
}

export const MAX_SESSION_PANES = 2;
export const MAX_SESSION_TABS = 12;

/** FER-395: session data validates whole — anything malformed is dropped so a
 *  broken snapshot degrades to the default boot, never a partial restore. */
export function sanitizeSession(raw: unknown): SessionSnapshot | null {
  if (typeof raw !== 'object' || raw === null) return null;
  const panes = (raw as { panes?: unknown }).panes;
  if (!Array.isArray(panes)) return null;
  const out: PaneSession[] = [];
  for (const rawPane of panes.slice(0, MAX_SESSION_PANES)) {
    if (typeof rawPane !== 'object' || rawPane === null) continue;
    const { tabs, current } = rawPane as { tabs?: unknown; current?: unknown };
    if (!Array.isArray(tabs)) continue;
    const paths = tabs
      .filter((p): p is string => typeof p === 'string' && p.trim().length > 0)
      .slice(0, MAX_SESSION_TABS);
    if (paths.length === 0) continue;
    out.push({
      tabs: paths,
      current:
        typeof current === 'number' &&
        Number.isInteger(current) &&
        current >= 0 &&
        current < paths.length
          ? current
          : 0,
    });
  }
  return out.length > 0 ? { panes: out } : null;
}

export type ConflictPolicy = 'ask' | 'overwrite' | 'skip' | 'keepBoth';

export interface FileOperationPrefs {
  confirmBeforeDeleting: boolean;
  verifyAfterCopy: boolean;
  copyBufferKiB: number;
  /** FER-471: default copy worker count. 0 = automatic (adaptive), 1..16 fixed. */
  copyThreads: number;
  /** FER-471: flush every copied file to disk before publishing it. Off by default; Move always does. */
  copySafeWrite: boolean;
  /** FER-471: default copy priority; `background` yields CPU and disk to other apps. */
  copyPriority: 'normal' | 'background';
  conflictPolicy: ConflictPolicy;
  preserveTimestamps: boolean;
  preservePermissions: boolean;
}

export const DEFAULT_FILE_OPERATION_PREFS: FileOperationPrefs = {
  confirmBeforeDeleting: true,
  verifyAfterCopy: false,
  copyBufferKiB: 1024,
  copyThreads: 0,
  copySafeWrite: false,
  copyPriority: 'normal',
  conflictPolicy: 'ask',
  preserveTimestamps: true,
  preservePermissions: false,
};

/** Allowed values of the default thread setting (0 = automatic). */
export const COPY_THREAD_CHOICES = [0, 1, 2, 4, 8, 16] as const;

export const COPY_BUFFER_MIN_KIB = 64;
export const COPY_BUFFER_MAX_KIB = 8192;

/** FER-511: identity of the confirmed Rar.exe, computed by the backend. Read-only for the
 *  webview: `save_prefs` never writes this field (only `archive_rar_tool_set_path` does). */
export interface RarExeFingerprint {
  size: number;
  modified: number;
  sha256: string;
}

export interface Prefs extends Partial<FileOperationPrefs> {
  theme: Theme;
  palette: PaletteName;
  showHidden: boolean;
  /** Preferred terminal for Open Terminal Here (F2-25). */
  terminal: TerminalChoice;
  /** Left pane share of the dual-pane grid (FER-406), clamped [0.2, 0.8]. */
  paneSplit: number;
  /** Tab behavior (FER-395 Settings > Tabs). */
  tabs: TabsSettings;
  /** Preview panel behavior (FER-396 Settings > Preview). */
  preview: PreviewSettings;
  /** FER-395 last launch session; absent/cleared while restore is off. */
  session?: SessionSnapshot | null;
  /** Appearance (FER-370): row density, file-glyph style, UI font scale,
   *  extension display and folders-first sorting. */
  density: Density;
  iconStyle: IconStyle;
  /** FER-478: file icon shape (Line | Duotone | Label | Tile). */
  iconTheme: IconTheme;
  /** FER-370/D50: named text scale level (small|medium→s|m|l|xl). */
  uiFontLevel: FontLevel;
  showExtensions: boolean;
  foldersFirst: boolean;
  /** FER-460: offer the elevated fast NTFS scan (only the offer is
   *  remembered; absent in old configs = true). */
  diskUsageFastOffer?: boolean;
  /** FER-464: warn when a cached Disk usage scan is older than this many hours (1-8760, absent = 24). */
  diskUsageCacheWarnHours?: number;
  /** FER-491: address bar on its own full-width row (absent in old configs = false). */
  addressOwnRow?: boolean;
  /** FER-477a: folder sizes from the FerrumIndexer in the Size column (absent = false). */
  indexerFolderSizes?: boolean;
  /** FER-477b: sync may skip reading files the indexer journal proves unchanged (absent = false). */
  syncJournalSkip?: boolean;
  /** FER-505: path to WinRAR Rar.exe (absent/null = feature disabled).
   *  FER-511: the webview cannot change it — save_prefs keeps the disk value. */
  rarExePath?: string | null;
  /** FER-511: fingerprint of the confirmed Rar.exe; absent = the stored path is NOT
   *  confirmed and is never executed (legacy configs must re-confirm in Settings). */
  rarExeFingerprint?: RarExeFingerprint | null;
}

/** FER-464: hours after which a cached Disk usage scan is flagged old.
 *  Anything outside 1..8760 (or not a number) means the default, 24. */
export const DEFAULT_CACHE_WARN_HOURS = 24;
export function sanitizeWarnHours(v: unknown): number {
  return typeof v === 'number' && Number.isInteger(v) && v >= 1 && v <= 8760
    ? v
    : DEFAULT_CACHE_WARN_HOURS;
}

export function normalizeFileOperationPrefs(
  value: Partial<FileOperationPrefs> | null | undefined,
): FileOperationPrefs {
  const rawBuffer = value?.copyBufferKiB;
  const policy = value?.conflictPolicy;
  return {
    confirmBeforeDeleting:
      typeof value?.confirmBeforeDeleting === 'boolean' ? value.confirmBeforeDeleting : true,
    verifyAfterCopy:
      typeof value?.verifyAfterCopy === 'boolean'
        ? value.verifyAfterCopy
        : DEFAULT_FILE_OPERATION_PREFS.verifyAfterCopy,
    copyBufferKiB:
      typeof rawBuffer === 'number' && Number.isFinite(rawBuffer)
        ? Math.min(COPY_BUFFER_MAX_KIB, Math.max(COPY_BUFFER_MIN_KIB, Math.round(rawBuffer)))
        : DEFAULT_FILE_OPERATION_PREFS.copyBufferKiB,
    copyThreads:
      typeof value?.copyThreads === 'number' && Number.isFinite(value.copyThreads)
        ? Math.min(16, Math.max(0, Math.round(value.copyThreads)))
        : DEFAULT_FILE_OPERATION_PREFS.copyThreads,
    copySafeWrite:
      typeof value?.copySafeWrite === 'boolean'
        ? value.copySafeWrite
        : DEFAULT_FILE_OPERATION_PREFS.copySafeWrite,
    copyPriority:
      value?.copyPriority === 'background' || value?.copyPriority === 'normal'
        ? value.copyPriority
        : DEFAULT_FILE_OPERATION_PREFS.copyPriority,
    conflictPolicy:
      policy === 'ask' || policy === 'overwrite' || policy === 'skip' || policy === 'keepBoth'
        ? policy
        : DEFAULT_FILE_OPERATION_PREFS.conflictPolicy,
    preserveTimestamps:
      typeof value?.preserveTimestamps === 'boolean'
        ? value.preserveTimestamps
        : DEFAULT_FILE_OPERATION_PREFS.preserveTimestamps,
    preservePermissions:
      typeof value?.preservePermissions === 'boolean'
        ? value.preservePermissions
        : DEFAULT_FILE_OPERATION_PREFS.preservePermissions,
  };
}

export function normalizePrefs(value: Partial<Prefs>): Prefs {
  return { ...value, ...normalizeFileOperationPrefs(value) } as Prefs;
}

/** Permanent deletion must never be silently enabled by preferences. */
export function requiresDeleteConfirmation(
  permanent: boolean,
  confirmBeforeDeleting: boolean,
): boolean {
  return permanent || confirmBeforeDeleting;
}

export function supportsPermissionPreservation(platform: string): boolean {
  return /linux|mac|unix/i.test(platform);
}

export function boundedCopyBufferBytes(copyBufferKiB: number): number {
  return normalizeFileOperationPrefs({ copyBufferKiB }).copyBufferKiB * 1024;
}

export function defaultPrefs(): Prefs {
  return {
    theme: 'dark',
    palette: 'violet',
    showHidden: false,
    terminal: 'auto',
    paneSplit: 0.5,
    tabs: { openNextToCurrent: true, restoreTabs: true, tabWidth: 'fit', closeOnMiddleClick: true },
    session: null,
    density: 'compact',
    iconStyle: 'colored',
    iconTheme: 'Tile',
    uiFontLevel: 'm',
    showExtensions: true,
    foldersFirst: true,
    diskUsageFastOffer: true,
    diskUsageCacheWarnHours: 24,
    addressOwnRow: false,
    indexerFolderSizes: false,
    syncJournalSkip: false,
    rarExePath: null,
    rarExeFingerprint: null,
    preview: { ...PREVIEW_DEFAULTS },
    ...DEFAULT_FILE_OPERATION_PREFS,
  };
}

export async function loadPrefs(): Promise<Prefs> {
  if (!isInTauri()) {
    throw new FsError({ code: 'unsupported', message: 'Prefs require the desktop app' });
  }
  try {
    return normalizePrefs(await invoke<Prefs>('load_prefs'));
  } catch (e) {
    throw FsError.fromIPC(e);
  }
}

export async function savePrefs(prefs: Prefs): Promise<void> {
  if (!isInTauri()) return;
  try {
    await invoke('save_prefs', { prefs: normalizePrefs(prefs) });
  } catch (e) {
    throw FsError.fromIPC(e);
  }
}
