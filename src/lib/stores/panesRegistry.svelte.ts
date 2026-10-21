/**
 * Registry of live pane APIs so global actions (dialogs, shortcuts, task 13
 * command palette) can address the ACTIVE pane without prop drilling.
 */
import type { PaneId } from '../uiState.svelte';
import { ui } from '../uiState.svelte';
import { SvelteMap } from 'svelte/reactivity';

/** Plain-data snapshot of one visible row — the input for Mark operations
 *  (F2-14) so selection logic never needs component internals. */
export interface PaneEntryInfo {
  readonly name: string;
  readonly path: string;
  /** True for real directories; symlinks report false (see cursorEntry). */
  readonly isDir: boolean;
  readonly size: number;
  readonly modifiedMs: number;
}

/** FER-482: everything Ctrl+U moves between the two panes (opaque to callers). */
export interface PaneContents {
  tabs: import('./paneStore.svelte').TabState[];
  current: number;
  selected: string[];
  cursor?: string;
  anchor?: string;
}

export interface PaneApi {
  readonly path: string;
  /** True while the pane's current tab has back history (Go Back, Alt+Left). */
  readonly canGoBack?: boolean;
  /** Operation targets: the marked rows, else the cursor row (FER-492). */
  selectedPaths(): string[];
  /** Only the explicitly marked rows (empty when nothing is marked). */
  markedPaths?(): string[];
  cursorEntry(): { name: string; path: string; isDir: boolean } | undefined;
  /** Re-list current dir and reset transient selection state. */
  refresh(): void;
  /** F2-26 watcher refresh: re-list while KEEPING selection/cursor that
   *  still exist in the new listing (incremental-if-safe update). Panes
   *  without it fall back to the reset behaviour of `refresh`. */
  refreshExternal?(): void;
  newTab(): void;
  /** FER-577 (B4): select the next (+1) or previous (-1) tab of this pane, wrapping around. */
  cycleTab?(delta: 1 | -1): void;
  closeCurrentTab(): void;
  goUp(): void;
  /** Enter behaviour: open the cursor entry when it is a directory. */
  openCursorEntry(): void;
  /** Navigate to a path when the pane implements it (tree sidebar → pane). */
  openPath?: (path: string) => void;
  /** FER-456: switch the breadcrumb to the editable address bar. */
  editPath?: () => void;
  /**
   * Reveal a FILE in this pane: open its parent, then select + cursor the
   * entry (phase-4 palette default UX). Optional like openPath.
   */
  revealPath?: (path: string) => void;
  /** Clear all selected entries. */
  clearSelection?(): void;
  /** F2-14: visible rows as plain data (post sort/filter), in list order. */
  visibleEntries?(): PaneEntryInfo[];
  /** F2-14: replace the whole selection by path identity; optionally move
   *  the cursor onto one of the selected entries. */
  selectPaths?(paths: Iterable<string>, cursorPath?: string): void;
  /** FER-482 Ctrl+U: detach / attach the pane's whole content (tabs, selection, cursor). */
  takeContents?(): PaneContents;
  putContents?(contents: PaneContents): void;
  /** FER-489: move DOM focus to this pane's file list (cursor on the first row when unset). */
  focusList?(): void;
  /** Go back one history step (F2-22 Go Back, Alt+Left). */
  back?: () => void;
  /** FER-484: the current tab's back history (oldest first), for the Alt+Down dropdown. */
  historyBack?: () => string[];
  /** All entry names in the current listing (F2-20 collision pool). */
  listNames?: () => string[];
  /** FER-395 session provider: open tab paths in order + active index,
   *  collected by prefsStore through the paneStore mutation hook. */
  getSession?: () => { tabs: string[]; current: number };
}

const panes = new SvelteMap<PaneId, PaneApi>();

export function registerPane(id: PaneId, api: PaneApi): () => void {
  panes.set(id, api);
  return () => panes.delete(id);
}

export function getPane(id: PaneId): PaneApi | undefined {
  return panes.get(id);
}

export function activePane(): PaneApi | undefined {
  return panes.get(ui.activePane);
}

/** The INACTIVE pane — copy/move target in a dual-pane flow. */
export function inactivePane(): PaneApi | undefined {
  return panes.get(ui.activePane === 0 ? 1 : 0);
}

/** One pane's API by id. DSK-06: the Disk usage mode's move target is the
 *  pane OPPOSITE the mode's pane, which is not necessarily `inactivePane()`
 *  (ui.activePane can switch while the mode stays on its pane). */
export function paneById(id: PaneId): PaneApi | undefined {
  return panes.get(id);
}

export function refreshAllPanes(): void {
  for (const api of panes.values()) api.refresh();
}

/**
 * Refresh ONE pane (F2-26 filesystem watcher). `ambiguous` (the watched
 * directory itself was reported) forces the reset behaviour; scoped
 * listing changes use the state-preserving refresh when available, so
 * external edits never clobber a selection that still resolves.
 */
export function refreshPane(id: PaneId, ambiguous = false): void {
  const api = panes.get(id);
  if (!api) return;
  if (!ambiguous && api.refreshExternal) api.refreshExternal();
  else api.refresh();
}

/**
 * Refresh any pane whose current directory matches `targetPath` (e.g. after extract).
 */
export function refreshPanesMatching(targetPath: string): void {
  const norm = targetPath.replace(/[\\/]+$/, '').toLowerCase();
  for (const api of panes.values()) {
    if (api.path.replace(/[\\/]+$/, '').toLowerCase() === norm) {
      if (api.refreshExternal) api.refreshExternal();
      else api.refresh();
    }
  }
}
