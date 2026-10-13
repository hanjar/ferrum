/**
 * DSK-11 (FER-409) — the app-level Disk usage WORKSPACE: a full mode that
 * replaces the dual-pane Files grid while active. ONE scan session divided
 * across the dual-pane grid: the largest-first tree fills the LEFT pane,
 * the Treemap|Files tabs fill the RIGHT pane, and the global header (root,
 * metric, counts, status chip, Rescan/Cancel, Exit) spans the top — the
 * DSK-10 pair of independent per-side stores is gone. One
 * `createDiskUsageStore()` instance, so tree selection ↔ treemap highlight
 * and double-click zoom stay in sync by construction.
 *
 * Entry (ONE path only): Alt+Shift+D (`commands.diskUsage`) or a pane toolbar's
 * Disk usage button → `openFromPanes()`/`toggle()`. The root suggestion is
 * the ENTRY pane's current folder; `entryPane` also decides Reveal's return
 * pane and Move's opposite-pane destination. Exit (the single global Exit /
 * Escape) → back to the normal dual-pane Files: both FilePanes stay
 * mounted-hidden, so their tabs/scroll/selection survive, and the scan
 * state is kept — re-entering shows the previous results WITHOUT an
 * auto-rescan (states.md).
 */
import { activePane, paneById } from '../stores/panesRegistry.svelte';
import { ui } from '../uiState.svelte';
import { createDiskUsageStore, type DiskUsageStore } from './diskUsageStore.svelte';

export class DuWorkspace {
  /** True while the Disk usage workspace replaces the Files grid. */
  open: boolean = $state(false);

  /** The ONE session store — the left tree and the right tabs share it. */
  readonly store: DiskUsageStore = createDiskUsageStore();

  /** The Files pane the workspace was entered from (0/1). Reveal drives THIS
   *  pane back; Move targets the OPPOSITE pane's folder. Stable while the
   *  panes are hidden — nothing can re-focus them. */
  entryPane: 0 | 1 = $state(0);

  /** Enter: the root suggestion is the entry pane's current folder. Previous
   *  results stay visible until a new scan starts. */
  openFromPanes(): void {
    this.entryPane = ui.activePane === 1 ? 1 : 0;
    const cwd = activePane()?.path.trim() || paneById(this.entryPane === 0 ? 1 : 0)?.path.trim();
    if (cwd) this.store.rootInput = cwd;
    void this.store.ensureListener();
    this.open = true;
    // FER-464: a root outside the index may have a cached scan: show it
    // right away (as Stale). Only when nothing is on screen yet; the backend
    // returns nothing for index-covered roots, which are scanned fresh.
    if (this.store.phase === 'idle' && this.store.rootInput.trim()) {
      void this.store.loadCached(this.store.rootInput);
    }
  }

  /** Alt+Shift+D / toolbar toggle: closed → enter, open → global Exit. */
  toggle(): void {
    if (this.open) this.exit();
    else this.openFromPanes();
  }

  /** The ONE global Exit: back to the normal dual-pane Files. Scan results
   *  are kept for the next open (no reset anywhere). */
  exit(): void {
    this.open = false;
  }
}

export const duWorkspace = new DuWorkspace();
