/**
 * Filesystem watcher bridge (F2-26, grown from backlog #2): the watch
 * lifecycle follows each pane's current tab — one OS watch root per distinct
 * pane path, started/stopped as tabs navigate. Coalesced `fs://changed`
 * batches are routed ONLY to the panes whose root was hit (per-pane scoping),
 * through a per-pane spacing gate so event storms collapse into one refresh
 * with at most one trailing refresh — the listing stays accurate without a
 * refresh loop. Scoped changes keep selection/cursor (incremental-if-safe);
 * a batch reporting the pane's own directory is ambiguous and forces a full
 * reset relist. FER-369 (opens D43): a pane INSIDE an archive watches its
 * parent ARCHIVE FILE instead — a hit means the archive content may have
 * changed, which is a scoped refresh (state-preserving refreshExternal, the
 * pane's stale-request guard rejects superseded listings), never the
 * directory-vanished ambiguity.
 */
import { watchStart, watchStop, onFsChanged, type FsChangeBatch } from '../ipc/watch';
import { isInTauri } from '../ipc/tauri';
import { SvelteSet } from 'svelte/reactivity';
import { refreshPane } from './panesRegistry.svelte';
import { splitArchiveLocation } from './archiveFetcher';
import type { PaneId } from '../uiState.svelte';

/** Minimum spacing between watcher-driven refreshes of one pane. Events
 *  arriving while a pane is cooling are collapsed into one trailing refresh. */
export const REFRESH_SPACING_MS = 300;

class WatchStore {
  private panePaths = new Map<number, string>();
  private watched = new SvelteSet<string>();
  private started = false;
  private unlisten: (() => void) | null = null;
  /** Per-pane anti-thrash state: cooling = refresh fired recently; the
   *  trailing map remembers whether a collapsed event needs the reset
   *  (ambiguous) refresh variant. */
  private cooling = new Set<number>();
  private trailing = new Map<number, boolean>();
  private timers = new Map<number, ReturnType<typeof setTimeout>>();
  /** Injectable for tests; production uses REFRESH_SPACING_MS. */
  spacingMs = REFRESH_SPACING_MS;

  async start() {
    if (this.started || !isInTauri()) return;
    this.started = true;
    try {
      this.unlisten = await onFsChanged((batch) => this.onBatch(batch));
    } catch {
      this.started = false; // jsdom / no event system: soft no-op
    }
  }

  stop() {
    for (const root of this.watched) void watchStop(root);
    for (const t of this.timers.values()) clearTimeout(t);
    this.timers.clear();
    this.cooling.clear();
    this.trailing.clear();
    this.watched.clear();
    this.panePaths.clear();
    this.unlisten?.();
    this.unlisten = null;
    this.started = false;
  }

  /** Called by each pane whenever its current tab path changes. */
  async registerPanePath(paneId: number, path: string) {
    if (this.panePaths.get(paneId) === path) return;
    this.panePaths.set(paneId, path);
    await this.sync();
  }

  /**
   * The on-disk root a pane's current location is watched through: the pane
   * path itself for disk locations, the parent ARCHIVE FILE for panes inside
   * an archive virtual location (FER-369).
   */
  private watchRootOf(panePath: string): string {
    const loc = splitArchiveLocation(panePath);
    return (loc ? loc.archivePath : panePath).replace(/[\\/]+$/, '');
  }

  /**
   * Route one coalesced backend batch: only panes rooted at a hit root
   * refresh. Disk panes whose watched DIRECTORY itself changed (path present
   * in the batch → appeared/vanished) get the full-reset variant; an archive
   * pane whose archive file changed refreshes state-preservingly — the file
   * always reports itself, and that is a content change, not an ambiguity.
   */
  onBatch(batch: FsChangeBatch) {
    for (const [paneId, panePath] of this.panePaths) {
      const root = this.watchRootOf(panePath);
      if (!batch.roots.includes(root)) continue;
      this.schedule(paneId, !splitArchiveLocation(panePath) && batch.paths.includes(root));
    }
  }

  /** Test hook: paths the OS watcher currently follows. */
  watchedRoots(): string[] {
    return [...this.watched];
  }

  private schedule(paneId: number, ambiguous: boolean) {
    if (this.cooling.has(paneId)) {
      // Collapse the storm into one trailing refresh, escalating to the
      // reset variant if ANY collapsed event was ambiguous.
      this.trailing.set(paneId, (this.trailing.get(paneId) ?? false) || ambiguous);
      return;
    }
    this.fire(paneId, ambiguous);
  }

  private fire(paneId: number, ambiguous: boolean) {
    this.cooling.add(paneId);
    // Pane ids come from FilePane's PaneId (0|1); the map is number-keyed so
    // late/unknown ids simply find no registered pane in refreshPane.
    refreshPane(paneId as PaneId, ambiguous);
    const t = setTimeout(() => {
      this.timers.delete(paneId);
      const pending = this.trailing.get(paneId);
      if (this.trailing.delete(paneId) && pending !== undefined) {
        this.fire(paneId, pending);
      } else {
        this.cooling.delete(paneId);
      }
    }, this.spacingMs);
    this.timers.set(paneId, t);
  }

  private async sync() {
    // Local dedupe list — a fixed small set (≤2 pane roots), not reactive state.
    const want: string[] = [];
    const wantHas = (root: string) => want.includes(root);
    const wantAdd = (root: string) => {
      if (!wantHas(root)) want.push(root);
    };
    for (const p of this.panePaths.values()) {
      if (p) wantAdd(this.watchRootOf(p));
    }
    for (const root of [...this.watched]) {
      if (!wantHas(root)) {
        this.watched.delete(root);
        void watchStop(root); // subscription cleanup on tab navigation
      }
    }
    for (const root of want) {
      if (!this.watched.has(root)) {
        try {
          await watchStart(root);
          this.watched.add(root);
        } catch {
          /* path may have vanished (device disconnect) — retried on the
             next navigation; the listing itself shows the error */
        }
      }
    }
  }
}

export const watcher = new WatchStore();
