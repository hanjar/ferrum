/**
 * Multi-Rename Tool dialog state (F2-20). Owns the open dialog's selection,
 * the live rule options, the last execution report, and the session undo
 * journal. The journal is deliberately session-scoped: undo replays the
 * last successful run's (to → from) pairs and is only offered while the
 * renamed paths have not changed again (the executor refuses otherwise).
 */
import { activePane } from './panesRegistry.svelte';
import { activity } from './activityStore.svelte';
import { renameBatch, type RenamePair } from '../ipc/renameBatch';
import { FsError } from '../ipc/types';
import {
  DEFAULT_OPTIONS,
  buildPreview,
  executablePairs,
  type RenameOptions,
  type RenamePreviewItem,
} from '../multiRename';

export interface MultiRenameReport {
  requested: number;
  renamed: number;
  failures: { from: string; to: string; message: string }[];
}

class MultiRenameStore {
  open = $state(false);
  options = $state<RenameOptions>({ ...DEFAULT_OPTIONS });
  /** Selection the dialog was opened with (paths stay stable while open). */
  paths = $state<string[]>([]);
  /** All names in the directory — the collision pool outside the selection. */
  directoryNames = $state<string[]>([]);
  busy = $state(false);
  error = $state('');
  lastReport = $state<MultiRenameReport | null>(null);
  /** Session journal of every successful run, oldest first. */
  journal = $state<RenamePair[]>([]);

  /** Preview for the CURRENT options, recomputed on read (Svelte 5 derived). */
  preview(): RenamePreviewItem[] {
    const names = this.paths.map(
      (p) =>
        p
          .replace(/[\\/]+$/, '')
          .split(/[\\/]/)
          .pop() ?? p,
    );
    return buildPreview(names, this.options, this.directoryNames);
  }

  /** The run stays locked until every item previews without an issue. */
  canRun(): boolean {
    return !this.busy && this.paths.length > 0 && this.preview().every((item) => !item.issue);
  }

  canUndo(): boolean {
    return !this.busy && this.journal.length > 0;
  }

  /** Open bound to the active pane's selection. */
  openForSelection() {
    this.paths = [];
    this.directoryNames = [];
    this.options = { ...DEFAULT_OPTIONS };
    this.lastReport = null;
    this.error = '';
    const pane = activePane();
    const paths = pane?.selectedPaths() ?? [];
    if (paths.length === 0) return;
    this.paths = [...paths];
    const all = pane?.listNames?.() ?? [];
    const selectedNames = new Set(this.names());
    this.directoryNames = all.filter((n) => !selectedNames.has(n));
    this.open = true;
  }

  close() {
    if (this.busy) return;
    this.open = false;
  }

  names(): string[] {
    return this.paths.map(
      (p) =>
        p
          .replace(/[\\/]+$/, '')
          .split(/[\\/]/)
          .pop() ?? p,
    );
  }

  /** Execute the valid preview pairs; the journal grows for Undo. */
  async run(): Promise<void> {
    if (!this.canRun()) return;
    this.busy = true;
    this.error = '';
    try {
      const pairs = executablePairs(this.paths, this.preview());
      const report = await renameBatch(pairs);
      this.journal = [...this.journal, ...report.journal];
      this.lastReport = {
        requested: report.requested,
        renamed: report.renamed,
        failures: report.failed.map((f) => ({ from: f.from, to: f.to, message: f.message })),
      };
      if (report.renamed > 0) {
        activity.noteFsChange();
        activePane()?.refresh();
      }
    } catch (e) {
      this.error = e instanceof FsError ? e.message : String(e);
    } finally {
      this.busy = false;
    }
  }

  /** Session Undo: replay the journal reversed (to → from). The executor
   *  refuses anything whose path changed again, so stale entries fail
   *  loudly and are dropped from the journal. */
  async undo(): Promise<void> {
    if (!this.canUndo()) return;
    this.busy = true;
    this.error = '';
    try {
      const reverse: RenamePair[] = this.journal.map((p) => {
        const dir = dirOf(p.from);
        return { from: dir + p.to, to: p.from.slice(dir.length) };
      });
      const report = await renameBatch(reverse.reverse());
      // A successfully undone entry restores `from`; drop only those. The
      // report's `to` is a bare name — reattach the directory it came from.
      const restored = new Set(report.journal.map((p) => dirOf(p.from) + p.to));
      this.journal = this.journal.filter((p) => !restored.has(p.from));
      this.lastReport = {
        requested: report.requested,
        renamed: report.renamed,
        failures: report.failed.map((f) => ({ from: f.from, to: f.to, message: f.message })),
      };
      if (report.renamed > 0) {
        activity.noteFsChange();
        activePane()?.refresh();
      }
    } catch (e) {
      this.error = e instanceof FsError ? e.message : String(e);
    } finally {
      this.busy = false;
    }
  }
}

/** Directory part of a path, keeping the trailing separator. */
function dirOf(path: string): string {
  const i = Math.max(path.lastIndexOf('/'), path.lastIndexOf('\\'));
  return i === -1 ? '' : path.slice(0, i + 1);
}

export const multiRename = new MultiRenameStore();
