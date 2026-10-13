/**
 * Compare state (F2-18). Holds the directory-compare snapshot between the
 * two panes, the compare-by-content result, and the "Hide Same Files" name
 * filter derived from the report. Purely read-only — nothing here mutates
 * the filesystem; actions that operate on results address panes through the
 * registry (selectPaths/visibleEntries).
 */
import {
  clearCompareCache,
  compareDirs,
  compareFiles,
  type CompareEncoding,
  type CompareDirsReport,
  type CompareResult,
  type DirDiffState,
} from '../ipc/compare';
import { SyncFlow } from './syncFlow.svelte';
import { activePane, inactivePane, type PaneApi } from './panesRegistry.svelte';
import { compareRun } from './compareRun.svelte';

/** States that mean "this side has work" for Mark Newer. */
export function isNewerOnSide(state: DirDiffState, side: 'left' | 'right'): boolean {
  return side === 'left'
    ? state === 'newerLeft' || state === 'leftOnly'
    : state === 'newerRight' || state === 'rightOnly';
}

function normKey(p: string): string {
  return p.split('\\').join('/').toLowerCase();
}

/** `path` relative to `root` ('/'-separated), or undefined when not under it. */
function relativeTo(root: string, path: string): string | undefined {
  const r = root.split('\\').join('/').replace(/\/+$/, '');
  const p = path.split('\\').join('/');
  if (
    p.length > r.length + 1 &&
    p[r.length] === '/' &&
    p.slice(0, r.length).toLowerCase() === r.toLowerCase()
  ) {
    return p.slice(r.length + 1);
  }
  return undefined;
}

/** Sync flow state/methods live in syncFlow.svelte.ts (FER-389); inherited here so
 *  `compareStore.openSync()` & co keep working. */
class CompareStore extends SyncFlow {
  panelOpen = $state(false);
  busy = $state(false);
  byContent = $state(false);
  /** FER-107: descend into common subfolders (entries become relative paths). */
  recursive = $state(false);
  /** FER-106: encoding for compare-by-content text diffs. */
  encoding = $state<CompareEncoding>('auto');
  /** FER-109: set after the engine cache was cleared. */
  cacheCleared = $state(false);
  report = $state<CompareDirsReport | null>(null);
  fileResult = $state<(CompareResult & { leftPath: string; rightPath: string }) | null>(null);
  /** Names to hide (Hide Same Files); applies to both panes' listings. */
  hiddenSameNames = $state<Set<string> | null>(null);

  /** Compare the two pane roots. Requires both panes. */
  async compareDirectories(
    byContent: boolean = this.byContent,
    recursive: boolean = this.recursive,
  ): Promise<void> {
    const left = activePane();
    const right = inactivePane();
    if (!left || !right) {
      this.errorText = 'Both panes must show a folder to compare.';
      this.panelOpen = true;
      return;
    }
    this.panelOpen = true;
    this.busy = true;
    this.errorText = '';
    this.byContent = byContent;
    this.recursive = recursive;
    // A new run invalidates the previous result (never reused silently).
    this.report = null;
    this.hiddenSameNames = null;
    const id = await compareRun.begin();
    try {
      const report = await compareDirs(left.path, right.path, byContent, {
        recursive,
        opId: id,
      });
      if (!compareRun.isCurrent(id)) return; // superseded by a newer run
      this.report = report;
      this.fileResult = null;
    } catch (e) {
      if (compareRun.isCurrent(id)) this.errorText = e instanceof Error ? e.message : String(e);
    } finally {
      if (compareRun.isCurrent(id)) {
        compareRun.end(id);
        this.busy = false;
      }
    }
  }

  /** Compare two files by content and show the verdict in the panel. */
  async compareContents(
    leftPath: string,
    rightPath: string,
    encoding: CompareEncoding = this.encoding,
  ): Promise<void> {
    this.panelOpen = true;
    this.busy = true;
    this.errorText = '';
    this.encoding = encoding;
    this.fileResult = null;
    const id = await compareRun.begin();
    try {
      const result = await compareFiles(leftPath, rightPath, { opId: id, encoding });
      if (!compareRun.isCurrent(id)) return;
      this.fileResult = { ...result, leftPath, rightPath };
    } catch (e) {
      if (compareRun.isCurrent(id)) this.errorText = e instanceof Error ? e.message : String(e);
    } finally {
      if (compareRun.isCurrent(id)) {
        compareRun.end(id);
        this.busy = false;
      }
    }
  }

  /** Cancel the in-flight compare (cooperative; the result is flagged partial). */
  async cancelRun(): Promise<void> {
    try {
      await compareRun.cancel();
    } catch (e) {
      this.errorText = e instanceof Error ? e.message : String(e);
    }
  }

  /** Drop cached content verdicts in the engine. */
  async clearCache(): Promise<void> {
    try {
      await clearCompareCache();
      this.cacheCleared = true;
    } catch (e) {
      this.errorText = e instanceof Error ? e.message : String(e);
    }
  }

  /** Mark Newer, Hide Same: on each pane select the entries whose state
   *  favors that side, and hide the names that are the same everywhere.
   *  A cancelled (partial) report is refused rather than used silently. */
  markNewerHideSame(): void {
    const report = this.report;
    if (!report) {
      this.errorText = 'Run Compare Directories first.';
      this.panelOpen = true;
      return;
    }
    if (report.cancelled) {
      this.errorText = 'The compare was cancelled — its partial result is not used. Run it again.';
      this.panelOpen = true;
      return;
    }
    this.hiddenSameNames = new Set(this.hideableNames());
    this.applySide(activePane(), 'left', report);
    this.applySide(inactivePane(), 'right', report);
  }

  private applySide(
    pane: PaneApi | undefined,
    side: 'left' | 'right',
    report: CompareDirsReport,
  ): void {
    if (!pane?.visibleEntries || !pane.selectPaths) return;
    const byName = new Map(report.entries.map((e) => [normKey(e.name), e]));
    const root = side === 'left' ? report.leftRoot : report.rightRoot;
    const selected: string[] = [];
    for (const entry of pane.visibleEntries()) {
      // Recursive entries are relative paths; match the pane file's path
      // relative to the compare root, falling back to its plain name.
      const diff = byName.get(normKey(relativeTo(root, entry.path) ?? entry.name));
      if (diff && isNewerOnSide(diff.state, side)) selected.push(entry.path);
    }
    pane.selectPaths(selected);
  }

  toggleHideSame(): void {
    this.hiddenSameNames = this.hiddenSameNames ? null : new Set(this.hideableNames());
  }

  private hideableNames(): string[] {
    const report = this.report;
    if (!report || report.cancelled) return [];
    const names = report.entries.filter((e) => e.state === 'same').map((e) => e.name);
    if (report.recursive) {
      // A top-level folder is hidden only when everything under it is the same
      // (and nothing under it was unreadable).
      // Local scratch map inside this function, not reactive state.
      // eslint-disable-next-line svelte/prefer-svelte-reactivity
      const groups = new Map<string, boolean>();
      for (const e of report.entries) {
        const i = e.name.indexOf('/');
        if (i < 0) continue;
        const top = e.name.slice(0, i);
        groups.set(top, (groups.get(top) ?? true) && e.state === 'same');
      }
      for (const err of report.errors ?? []) {
        const top = err.name.split('/')[0];
        if (groups.has(top)) groups.set(top, false);
      }
      for (const [top, allSame] of groups) if (allSame) names.push(top);
    }
    return names;
  }

  clearHiddenSame(): void {
    this.hiddenSameNames = null;
  }

  closePanel(): void {
    this.panelOpen = false;
  }

  reset(): void {
    this.panelOpen = false;
    this.busy = false;
    this.byContent = false;
    this.recursive = false;
    this.encoding = 'auto';
    this.cacheCleared = false;
    compareRun.reset();
    this.report = null;
    this.fileResult = null;
    this.errorText = '';
    this.hiddenSameNames = null;
    this.resetSync();
  }
}

export { SyncFlow } from './syncFlow.svelte';
export const compareStore = new CompareStore();
