/**
 * FER-598: the "Recent files and folders" view of a pane. It reuses the Branch View row source
 * (`branch.feed`, like search results fed to a pane), so sorting, filtering, marks, the cursor and
 * opening behave like any listing. The tab's own path and history are never touched: leaving the
 * view (Backspace, Back, navigating anywhere) simply shows the tab's folder again.
 *
 * The view is READ-ONLY: mutating commands check `PaneApi.readOnlyView` (see actions.ts).
 */
import type { FileEntry } from '../ipc/types';
import type { PaneId } from '../uiState.svelte';
import { branch } from './branchStore.svelte';
import { recentStore } from './recentStore.svelte';
import type { SortSpec } from '../selection';
import { RECENT_CAP, sourcesText, type RecentItem } from '../recent/recentModel';

export const RECENT_LABEL = 'Recent files and folders';

/**
 * FER-599: the Recent view's own sort. The Modified column carries the last-used time, so
 * "date descending" is newest used first. It lives in the pane component (never in the tab's
 * view state or any saved sort setting) and is dropped on leaving the view, so the tab's own
 * sort comes back untouched. Folders are never grouped first inside this view.
 */
export const RECENT_DEFAULT_SORT: SortSpec = { key: 'modified', dir: -1 };

/** Header click inside the Recent view: same key flips direction, a new key starts ascending. */
export function nextRecentSort(cur: SortSpec, key: SortSpec['key']): SortSpec {
  return cur.key === key ? { key, dir: cur.dir === 1 ? -1 : 1 } : { key, dir: 1 };
}

function toEntry(item: RecentItem): FileEntry {
  return {
    name: item.name,
    path: item.path,
    isDir: item.isDir === true,
    isSymlink: false,
    size: 0,
    // The Modified column shows the last-used time in this view.
    modifiedMs: item.lastUsedMs,
    hidden: false,
  };
}

function noteOf(item: RecentItem): string {
  const where = item.parent || '';
  return `${where} · ${sourcesText(item)}`;
}

function feed(paneId: PaneId, root: string): void {
  const items = recentStore.items.slice(0, RECENT_CAP);
  branch.feed(
    paneId,
    root,
    `${RECENT_LABEL} · ${items.length} items · Backspace to go back`,
    items.map(toEntry),
    { kind: 'recent', notes: new Map(items.map((i) => [i.path, noteOf(i)])) },
  );
}

/** Turn the pane into the Recent view: cached items at once, refreshed when the sources answer. */
export function showRecent(paneId: PaneId, root: string): void {
  feed(paneId, root);
  void recentStore.refresh().then(() => {
    // Only while the pane still shows the Recent view (the user may have left it meanwhile).
    if (branch.isRecent(paneId)) feed(paneId, root);
  });
}

/** Leave the Recent view; the tab shows its own folder again. */
export function leaveRecent(paneId: PaneId): boolean {
  if (!branch.isRecent(paneId)) return false;
  branch.exit(paneId);
  return true;
}
