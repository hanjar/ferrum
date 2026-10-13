/**
 * Comments view state (F2-27). `Show > Comments` loads each pane's
 * directory sidecar map (file name → text) for the listing column; the
 * Edit Comment dialog writes through the same store. Comments are keyed by
 * NAME within a directory — see comments.rs / decisions D34 for the
 * rename/move contract.
 */
import { commentSet, commentsForDir } from '../ipc/comments';
import { isInTauri } from '../ipc/tauri';
import type { PaneId } from '../uiState.svelte';

const EMPTY: Record<string, string> = {};

class CommentStore {
  /** Per-pane map of the CURRENT directory (replaced on every load). */
  panes = $state<Record<PaneId, Record<string, string>>>({ 0: {}, 1: {} });
  /** Directory each pane's map belongs to — stale maps are never shown. */
  private dirs: Record<PaneId, string> = { 0: '', 1: '' };

  map(pane: PaneId): Record<string, string> {
    return this.panes[pane] ?? EMPTY;
  }

  /** Comment text of one file name in the pane's current directory. */
  for(pane: PaneId, name: string): string {
    return this.map(pane)[name] ?? '';
  }

  /**
   * Read the map for a pane's directory. Always re-reads: the pane effect
   * also fires after listing refreshes, so a rename migration shows up
   * without extra plumbing. A read failure yields an honest empty view —
   * writes still report their errors through the dialog.
   */
  async load(pane: PaneId, dir: string): Promise<void> {
    if (!dir) return;
    this.dirs[pane] = dir;
    if (!isInTauri()) {
      this.panes[pane] = {};
      return;
    }
    try {
      this.panes[pane] = await commentsForDir(dir);
    } catch {
      this.panes[pane] = {};
    }
  }

  /** Re-read a pane's directory map (after a write). */
  async refresh(pane: PaneId, dir: string): Promise<void> {
    await this.load(pane, dir);
  }

  /** Write one file's comment and refresh its pane's map in place. */
  async set(pane: PaneId, path: string, text: string): Promise<void> {
    await commentSet(path, text.trim() || null);
    const i = Math.max(path.lastIndexOf('\\'), path.lastIndexOf('/'));
    const dir = i > 0 ? path.slice(0, i) : path;
    await this.load(pane, dir);
  }

  /** Test hook: resets all per-pane state. */
  reset(): void {
    this.panes = { 0: {}, 1: {} };
    this.dirs = { 0: '', 1: '' };
  }
}

export const comments = new CommentStore();
