/**
 * Branch View state (F2-23 / FER-128): an OPTIONAL per-pane listing mode that
 * swaps the pane's row source from "current directory" to "every file of the
 * subtree below it". The pane's tab path, history, and selection identity
 * are untouched — toggling off simply returns to the directory view.
 *
 * The traversal is STREAMED and cancellable: Rust walks the tree and sends
 * batches over `branch://progress`; this store appends them to the pane's rows
 * while the walk runs. Each walk has its own client-generated id, so events of
 * a superseded/cancelled walk are ignored by id and can never overwrite the
 * active one.
 *
 * Sort/filter are NOT special-cased: the pane re-runs its normal pipeline on
 * every APPLIED batch. To keep that cheap the store applies batches at most
 * every APPLY_INTERVAL_MS (the first batch immediately, so the list appears
 * at once; later ones coalesced), then once more when the walk is done.
 * Re-sorting 100k rows is a few tens of ms, so a ~4/s cadence stays smooth,
 * while re-applying per raw batch (~10/s from Rust) would double the cost for
 * no visible gain. Selection and cursor address rows by path, so growing the
 * list never moves them.
 */
import {
  branchCancel,
  branchStart,
  DEFAULT_MAX_ENTRIES,
  onBranchEvent,
  type BranchEvent,
} from '../ipc/branchView';
import type { UnlistenFn } from '@tauri-apps/api/event';
import { FsError, type FileEntry } from '../ipc/types';
import { rawRows, toRow, type Row } from './paneStore.svelte';

export interface BranchState {
  root: string;
  rows: Row[];
  loading: boolean;
  error: FsError | null;
  truncated: boolean;
  failedCount: number;
  /** The user stopped the walk; the rows streamed so far are kept (partial). */
  cancelled: boolean;
  /** Set when the rows were FED from elsewhere (e.g. search results) instead
   *  of traversed from `root`; shown in the pane status line. */
  label?: string;
}

/** What the store needs from the backend (injectable for tests). */
export interface BranchApi {
  start(id: string, root: string): Promise<void>;
  cancel(id: string): Promise<boolean>;
  listen(handler: (e: BranchEvent) => void): Promise<UnlistenFn>;
}

const tauriApi: BranchApi = {
  start: (id, root) => branchStart(id, root),
  cancel: branchCancel,
  listen: onBranchEvent,
};

/** Minimum gap between two UI applications of streamed batches. */
export const APPLY_INTERVAL_MS = 250;

interface Walk {
  id: string;
  paneId: 0 | 1;
  root: string;
  rows: Row[];
  pending: Row[];
  failedCount: number;
  lastApply: number;
  timer?: ReturnType<typeof setTimeout>;
}

let walkCounter = 0;

export class BranchStore {
  /** paneId → active branch state ({} when the pane shows its directory). */
  private states = $state<Record<number, BranchState | undefined>>({});
  /** paneId → the walk currently feeding that pane (not reactive). */
  private walks: Partial<Record<number, Walk>> = {};
  private listening: Promise<UnlistenFn> | null = null;

  constructor(private api: BranchApi = tauriApi) {}

  activeFor(paneId: 0 | 1): boolean {
    return this.states[paneId] !== undefined;
  }

  stateFor(paneId: 0 | 1): BranchState | undefined {
    return this.states[paneId];
  }

  /** True while the pane's walk is still running (Esc / Cancel apply). */
  isLoading(paneId: 0 | 1): boolean {
    return this.states[paneId]?.loading === true;
  }

  /** True while a running walk has no rows yet (the list shows "Loading…"). */
  isSettling(paneId: 0 | 1): boolean {
    const s = this.states[paneId];
    return s !== undefined && s.loading && s.rows.length === 0;
  }

  /** Rows for the pane's branch, each labelled with its relative path in
   *  `display` so duplicate basenames stay unambiguous in lists. */
  rows(paneId: 0 | 1): Row[] {
    return this.states[paneId]?.rows ?? [];
  }

  /** Status-line suffix: loading / cancelled / limit reached / unreadable dirs. */
  noteFor(paneId: 0 | 1): string {
    const s = this.states[paneId];
    if (!s || s.label !== undefined) return '';
    const parts: string[] = [];
    if (s.loading) parts.push('loading… (Esc to cancel)');
    if (s.cancelled) parts.push('cancelled, list is partial');
    if (s.truncated) {
      parts.push(`limit of ${DEFAULT_MAX_ENTRIES.toLocaleString('en-US')} items reached`);
    }
    if (s.failedCount > 0) parts.push(`${s.failedCount} folder(s) could not be read`);
    return parts.length > 0 ? ` · ${parts.join(' · ')}` : '';
  }

  /** Toggle Ctrl+B on a pane rooted at the pane's CURRENT path. Exiting
   *  keeps the tab/history untouched (and cancels a walk still running);
   *  re-entering refetches fresh. Resolves once the walk has STARTED — the
   *  rows arrive afterwards. */
  async toggle(paneId: 0 | 1, root: string): Promise<void> {
    if (this.activeFor(paneId)) {
      this.exit(paneId);
      return;
    }
    this.dropWalk(paneId, true);
    const walk: Walk = {
      id: `branch-${paneId}-${++walkCounter}-${Date.now().toString(36)}`,
      paneId,
      root,
      rows: [],
      pending: [],
      failedCount: 0,
      lastApply: 0,
    };
    this.walks[paneId] = walk;
    this.states = {
      ...this.states,
      [paneId]: {
        root,
        rows: [],
        loading: true,
        error: null,
        truncated: false,
        failedCount: 0,
        cancelled: false,
      },
    };
    try {
      this.listening ??= this.api.listen((e) => this.onEvent(e));
      await this.listening;
      await this.api.start(walk.id, root);
    } catch (e) {
      if (this.walks[paneId] !== walk) return; // superseded meanwhile
      delete this.walks[paneId];
      this.setState(paneId, {
        root,
        rows: [],
        loading: false,
        error: e instanceof FsError ? e : FsError.fromIPC(e),
        truncated: false,
        failedCount: 0,
        cancelled: false,
      });
    }
  }

  /** Stop the running walk of a pane (Esc / Cancel button). Rows streamed so
   *  far stay visible and are marked partial; the Rust side stops at its next
   *  entry. No-op when nothing is loading. */
  cancel(paneId: 0 | 1): void {
    const walk = this.walks[paneId];
    const state = this.states[paneId];
    if (!walk || !state?.loading) return;
    this.applyPending(walk);
    this.dropWalk(paneId, true);
    const current = this.states[paneId];
    if (current) this.setState(paneId, { ...current, loading: false, cancelled: true });
  }

  /** Feed an already-computed list of entries (FER-99 search results) into the
   *  pane as a virtual listing. Reuses the branch row source, so sort/filter/
   *  selection/copy/open behave like any pane listing. Cancels an in-flight
   *  traversal so it can never overwrite the fed rows. */
  feed(paneId: 0 | 1, root: string, label: string, entries: FileEntry[]): void {
    this.dropWalk(paneId, true);
    this.states = {
      ...this.states,
      [paneId]: {
        root,
        rows: rawRows(entries.map((entry) => toRow(entry))),
        loading: false,
        error: null,
        truncated: false,
        failedCount: 0,
        cancelled: false,
        label,
      },
    };
  }

  /** Back to the directory view (Ctrl+B again or navigation). */
  exit(paneId: 0 | 1): void {
    this.dropWalk(paneId, true);
    const next = { ...this.states };
    delete next[paneId];
    this.states = next;
  }

  // ── streaming internals ────────────────────────────────────────────────

  private setState(paneId: 0 | 1, state: BranchState) {
    this.states = { ...this.states, [paneId]: state };
  }

  /** Forget the pane's walk: stop its timer, drop late events (by id), and
   *  optionally tell Rust to stop walking. */
  private dropWalk(paneId: 0 | 1, cancelBackend: boolean) {
    const walk = this.walks[paneId];
    if (!walk) return;
    if (walk.timer !== undefined) clearTimeout(walk.timer);
    delete this.walks[paneId];
    if (cancelBackend) void this.api.cancel(walk.id);
  }

  private onEvent(ev: BranchEvent) {
    let walk: Walk | undefined;
    for (const w of Object.values(this.walks)) if (w?.id === ev.id) walk = w;
    if (!walk) return; // late event of a cancelled / superseded walk
    for (const entry of ev.entries) walk.pending.push(toRow(entry));
    walk.failedCount += ev.failed.length;
    if (ev.done) {
      if (walk.timer !== undefined) clearTimeout(walk.timer);
      walk.timer = undefined;
      delete this.walks[walk.paneId];
      this.applyPending(walk, ev);
      return;
    }
    if (walk.pending.length === 0 && ev.failed.length === 0) return;
    const wait = APPLY_INTERVAL_MS - (Date.now() - walk.lastApply);
    if (wait <= 0) {
      if (walk.timer !== undefined) clearTimeout(walk.timer);
      walk.timer = undefined;
      this.applyPending(walk);
    } else if (walk.timer === undefined) {
      walk.timer = setTimeout(() => {
        walk.timer = undefined;
        if (this.walks[walk.paneId] === walk) this.applyPending(walk);
      }, wait);
    }
  }

  /** Move buffered rows into the pane state (one reactive update). `final`
   *  is the walk's done event: it ends loading and reports summary/error. */
  private applyPending(walk: Walk, final?: BranchEvent) {
    walk.lastApply = Date.now();
    if (walk.pending.length > 0) {
      walk.rows = rawRows([...walk.rows, ...walk.pending]);
      walk.pending = [];
    }
    const prev = this.states[walk.paneId];
    if (!prev) return;
    if (final?.error) {
      this.setState(walk.paneId, {
        ...prev,
        rows: walk.rows,
        loading: false,
        error: final.error,
        failedCount: walk.failedCount,
      });
      return;
    }
    this.setState(walk.paneId, {
      ...prev,
      rows: walk.rows,
      loading: final ? false : prev.loading,
      truncated: final?.summary?.truncated ?? prev.truncated,
      cancelled: final?.summary?.cancelled ?? prev.cancelled,
      failedCount: final?.summary ? final.summary.failedCount : walk.failedCount,
    });
  }
}

export const branch = new BranchStore();
