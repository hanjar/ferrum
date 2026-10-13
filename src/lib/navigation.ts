/**
 * Power-user navigation (F2-22): source/target controls, Go Back, CD Tree
 * and the Desktop folder. Pure pane-registry orchestration so actions.ts
 * stays a thin registry and every move here is unit-testable.
 */
import { activePane, inactivePane } from './stores/panesRegistry.svelte';
import { dialogs } from './stores/dialogStore.svelte';
import { ui } from './uiState.svelte';
import { viewState } from './stores/viewStateStore.svelte';
import { isArchiveFile } from './stores/archiveFetcher';
import { desktopDir } from '@tauri-apps/api/path';

/** Ctrl+U: swap the two panes completely — tabs (with history), location,
 *  selection and cursor, plus the per-pane sort / view mode / filter. Focus
 *  stays on the same screen side. Panes that cannot hand over their content
 *  fall back to exchanging only the displayed directory. */
export function swapPanes(): boolean {
  const source = activePane();
  const target = inactivePane();
  if (!source || !target) return false;
  if (source.takeContents && source.putContents && target.takeContents && target.putContents) {
    const a = source.takeContents();
    const b = target.takeContents();
    source.putContents(b);
    target.putContents(a);
    const left = { ...viewState.panes[0], sort: { ...viewState.panes[0].sort } };
    const right = { ...viewState.panes[1], sort: { ...viewState.panes[1].sort } };
    Object.assign(viewState.panes[0], right);
    Object.assign(viewState.panes[1], left);
    return true;
  }
  if (!source.openPath || !target.openPath) return false;
  const sourcePath = source.path;
  const targetPath = target.path;
  if (sourcePath === targetPath) return true; // nothing to exchange
  target.openPath(sourcePath);
  source.openPath(targetPath);
  return true;
}

/** Ctrl+→ / Ctrl+← (Total Commander): the OTHER pane opens what is under the
 *  cursor of the active pane when `towardOther` (the arrow points at the other
 *  pane) and the cursor is a folder or an archive; in every other case (plain
 *  file, "..", nothing under the cursor, or the arrow pointing away from the
 *  other pane) it opens the active pane's current folder. Focus stays put and
 *  only the other pane's active tab changes. */
export function openInOtherPane(towardOther: boolean): boolean {
  const source = activePane();
  const target = inactivePane();
  if (!source || !target?.openPath) return false;
  let path = source.path;
  if (towardOther) {
    const item = source.cursorEntry();
    if (item && item.name !== '..' && (item.isDir || isArchiveFile(item.name))) path = item.path;
  }
  if (!path) return false;
  target.openPath(path);
  return true;
}

/** Target pane adopts the source pane's location (TC "Target = Source"). */
export function targetFromSource(): boolean {
  const source = activePane();
  const target = inactivePane();
  if (!source || !target?.openPath) return false;
  target.openPath(source.path);
  return true;
}

/** Alt+Left: history back on the active pane (clears its transient state). */
export function goBack(): boolean {
  const pane = activePane();
  if (!pane?.back) return false;
  pane.back();
  return true;
}

/** Alt+F10: CD Tree reuses the existing tree sidebar — open it, never
 *  reimplement it (spec F2-22 subtask 3). */
export function openCdTree(): boolean {
  ui.sidebarOpen = true;
  return true;
}

/** Open the user's Desktop folder in the active pane. */
export async function openDesktop(): Promise<boolean> {
  const pane = activePane();
  if (!pane?.openPath) return false;
  try {
    const dir = await desktopDir();
    if (!dir) throw new Error('Desktop folder is not available on this system');
    pane.openPath(dir);
    return true;
  } catch (e) {
    dialogs.open({
      kind: 'error',
      title: 'Open Desktop Folder',
      message: e instanceof Error ? e.message : String(e),
    });
    return false;
  }
}
