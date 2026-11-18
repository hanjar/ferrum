/**
 * FER-598: reactive cache of the recent files and folders, merged from three sources:
 *  - Ferrum's own Activity history (hidden in a private session, like the sidebar),
 *  - Windows Recent (`shell:recent`), only while "Include Windows Recent" is on and not private,
 *  - newly added items of the watched folders, only while that setting is on and not private.
 *
 * The raw per-source lists are cached; the privacy / toggle filters are applied when reading, so
 * flipping a setting or the private session hides items at once without a refetch. Every source is
 * time-bounded and failure-tolerant: a slow or broken source contributes nothing, it never blocks
 * the palette or the Recent view.
 */
import { activityRecent } from '../ipc/activity';
import {
  recentDefaultFolders,
  recentNewItems,
  recentWindows,
  type RecentFileDto,
} from '../ipc/recent';
import { ui } from '../uiState.svelte';
import { activity } from './activityStore.svelte';
import {
  RECENT_CAP,
  fromActivity,
  fromRecentFiles,
  mergeRecent,
  type RecentItem,
} from '../recent/recentModel';

/** Longest wait for any one source. */
export const SOURCE_TIMEOUT_MS = 4000;

function bounded<T>(work: Promise<T>, fallback: T): Promise<T> {
  return new Promise<T>((resolve) => {
    const timer = setTimeout(() => resolve(fallback), SOURCE_TIMEOUT_MS);
    work.then(
      (v) => {
        clearTimeout(timer);
        resolve(v);
      },
      () => {
        clearTimeout(timer);
        resolve(fallback);
      },
    );
  });
}

class RecentStore {
  ferrum = $state<RecentItem[]>([]);
  windows = $state<RecentItem[]>([]);
  added = $state<RecentItem[]>([]);
  loading = $state(false);
  #seq = 0;

  /** Merged view with the current privacy settings applied. */
  get items(): RecentItem[] {
    const lists: RecentItem[][] = [];
    if (!activity.privateSession) lists.push(this.ferrum);
    if (ui.includeWindowsRecent && !activity.privateSession) lists.push(this.windows);
    if (ui.recentNewItems && !activity.privateSession) lists.push(this.added);
    return mergeRecent(lists, RECENT_CAP);
  }

  /** Files only (the Ctrl+K group). Unknown kind counts as a file. */
  get files(): RecentItem[] {
    return this.items.filter((i) => i.isDir !== true);
  }

  /** Reload every enabled source. A source that is off is NOT read at all. */
  async refresh(): Promise<void> {
    const seq = ++this.#seq;
    this.loading = true;
    const privateNow = activity.privateSession;
    const wantWindows = ui.includeWindowsRecent && !privateNow;
    const wantAdded = ui.recentNewItems && !privateNow;
    const days = ui.recentNewDays;
    const configured = ui.recentWatchFolders;
    const [files, folders, windows, added] = await Promise.all([
      privateNow ? Promise.resolve([]) : bounded(activityRecent('file', RECENT_CAP), []),
      privateNow ? Promise.resolve([]) : bounded(activityRecent('folder', RECENT_CAP), []),
      wantWindows ? bounded(recentWindows(RECENT_CAP), [] as RecentFileDto[]) : Promise.resolve([]),
      wantAdded
        ? bounded(
            (async () => {
              const watch = configured ?? (await recentDefaultFolders());
              return watch.length > 0 ? recentNewItems(watch, days, RECENT_CAP) : [];
            })(),
            [] as RecentFileDto[],
          )
        : Promise.resolve([]),
    ]);
    if (seq !== this.#seq) return; // a newer refresh owns the result
    this.ferrum = fromActivity([...files, ...folders]);
    this.windows = fromRecentFiles(windows);
    this.added = fromRecentFiles(added);
    this.loading = false;
  }

  /** Test hook. */
  reset(): void {
    this.#seq++;
    this.ferrum = [];
    this.windows = [];
    this.added = [];
    this.loading = false;
  }
}

export const recentStore = new RecentStore();
