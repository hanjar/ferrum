/**
 * DSK-04 — Disk usage scan/view state (Svelte 5 runes). Since DSK-10 this is
 * a factory-built store: `createDiskUsageStore()` builds an independent
 * instance with its own scan, guards and progress-event listener. Since
 * DSK-11 (FER-409) the app-level workspace owns exactly ONE instance (its
 * single session is divided across the dual-pane grid: tree left, tabs
 * right); tests may build more — events of a foreign scanId are dropped by
 * the instance's own scanId guard, contract §3.4. The old single-pane mode
 * and its singleton export are gone: the workspace (duWorkspace.svelte.ts +
 * DiskUsageWorkspace.svelte) owns the instance and the global Enter/Exit
 * (states.md: Exit preserves the scan state for the next open).
 *
 * Contract bindings (docs/disk-usage-contract.md + design/disk-usage/states.md):
 * - Generation guards: events/queries of an older scan are discarded by
 *   scanId (§3.4); view queries capture a seq bumped on every scope change
 *   (zoom/selection/metric/filter), so a late response can never paint the
 *   wrong scope.
 * - `partial` and `cancelled` are never labelled complete; no percent-done
 *   and no ETA while scanning — only arrived counts (§3.5, states.md rule 2).
 * - Query NotFound (evicted snapshot) → `needsRescan`, the UI offers a
 *   rescan (gap G8). Selection carries (scanId, version) context implicitly:
 *   every scan reset clears it and bumps the guard seqs.
 * - Allocated null semantics flow through untouched — no logical
 *   substitution anywhere (§2.2).
 */
import { SvelteSet } from 'svelte/reactivity';
import type { UnlistenFn } from '@tauri-apps/api/event';
import {
  diskUsageCacheDelete,
  diskUsageCacheLoad,
  diskUsageCancel,
  diskUsageQueryChildren,
  diskUsageSnapshot,
  diskUsageStart,
  diskUsageTopFiles,
  onDiskUsageEvent,
} from '../ipc/diskUsage';
import { FsError } from '../ipc/types';
import type {
  DiskUsageChildItem,
  DiskUsageChildrenPage,
  DiskUsageCoverage,
  DiskUsageFileCategory,
  DiskUsageFilter,
  DiskUsageNode,
  DiskUsageProgressEvent,
  DiskUsageScanEngine,
  DiskUsageScanError,
  DiskUsageSizeMetric,
  DiskUsageSnapshot,
  DiskUsageStats,
  DiskUsageTopFiles,
} from '../ipc/types';
import { isInTauri } from '../ipc/tauri';
import { ageLabel } from './cacheAge';
import { metricValue } from './treemapLayout';

export type DiskUsagePhase =
  'idle' | 'scanning' | 'complete' | 'partial' | 'cancelled' | 'stale' | 'empty' | 'error';

export type DiskUsageTab = 'treemap' | 'files';

/** Phases whose scan snapshot is queryable (queries fail NotFound otherwise). */
const DATA_PHASES: ReadonlySet<DiskUsagePhase> = new Set([
  'complete',
  'partial',
  'cancelled',
  'stale',
  'empty',
]);

export const TREE_PAGE_SIZE = 50;
export const FILES_LIMIT = 20;
const TREEMAP_LIMIT = 200;
/** Progress events arrive ≤10/s; the early buffer only bridges the window
 *  between the backend emitting for a fresh scanId and the start invoke
 *  resolving on this instance (capped — overflow is dropped, not applied). */
const EARLY_EVENT_CAP = 200;
/** Bounded per-path error list kept for the UI; the true count is live.totalErrors. */
const KEPT_ERRORS = 100;

/** Children page cache of one expanded tree node. */
export interface TreeNodeState {
  items: DiskUsageChildItem[];
  /** Filter matches for this node — the pager denominator. */
  total: number;
  loading: boolean;
  loadingMore: boolean;
  error: string | null;
  /** Last load issued for this node (drops superseded responses). */
  loadId: number;
}

interface TreemapQuery {
  items: DiskUsageChildItem[];
  total: number;
  loading: boolean;
  error: string | null;
  seq: number;
}

interface FilesQuery {
  result: DiskUsageTopFiles | null;
  loading: boolean;
  error: string | null;
  seq: number;
}

export interface LiveTotals {
  scannedFiles: number;
  scannedDirs: number;
  scannedSymlinks: number;
  knownLogicalBytes: number;
  knownAllocatedBytes: number | null;
  totalErrors: number;
  coverage: DiskUsageCoverage;
}

/** DSK-06.2 action gate — per instance, so the rendering workspace can only
 *  ever show THIS instance's refusal note. */
export interface DuGateNote {
  path: string;
  message: string;
}

const EMPTY_LIVE: LiveTotals = {
  scannedFiles: 0,
  scannedDirs: 0,
  scannedSymlinks: 0,
  knownLogicalBytes: 0,
  knownAllocatedBytes: null,
  totalErrors: 0,
  coverage: 'complete',
};

/** `disk_usage://progress`.topChildren are subtree NODES; the tree preview
 *  renders them with the same row shape as query_children items. Percent is
 *  null while scanning (no final denominator yet — "—", never 0%). */
function previewItem(n: DiskUsageNode): DiskUsageChildItem {
  return {
    id: n.id,
    name: n.name,
    kind: 'dir',
    logicalBytes: n.subtreeLogicalBytes,
    allocatedBytes: n.subtreeAllocatedBytes,
    allocatedSupported: n.subtreeAllocatedBytes !== null,
    category: 'other',
    percentOfParent: null,
    percentOfScanRoot: null,
  };
}

export class DiskUsageStore {
  /** DSK-06.2 revalidation gate: a refused Move/Delete target with its
   *  reason, rendered by this instance's workspace with a Rescan CTA.
   *  Per instance — a refusal never leaks into another store. */
  duGate: DuGateNote | null = $state(null);

  phase: DiskUsagePhase = $state('idle');
  /** The scanned root (identity of the current result set). */
  root = $state('');
  /** Editable suggestion for the NEXT scan — pane cwd on open. */
  rootInput = $state('');
  scanId = $state<string | null>(null);
  /** Snapshot generation of the current scan (gap G8). */
  version = $state(0);
  scannedAtMs = $state(0);
  rootId = $state('');
  rootNode: DiskUsageNode | null = $state.raw(null);
  stats: DiskUsageStats | null = $state(null);
  /** Bounded snapshot summary (root aggregates + stats). FER-479: the node
   *  map of a whole-drive scan stays in Rust — tree rows read their counts
   *  from the children pages. `$state.raw`: replaced wholesale, never
   *  mutated, so no deep proxy is built. */
  snapshot: DiskUsageSnapshot | null = $state.raw(null);
  /** Running totals from the latest progress event (scanning). */
  live: LiveTotals = $state({ ...EMPTY_LIVE });
  /** Bounded accumulated scan errors; true count is live.totalErrors. */
  errors: DiskUsageScanError[] = $state([]);
  errorText = $state('');
  /** A query hit NotFound (snapshot evicted/unknown) — offer Rescan (G8). */
  needsRescan = $state(false);
  /** FER-460: engine of the current scan ('ntfsMft' = elevated MFT helper). */
  engine: DiskUsageScanEngine = $state('walk');
  /** FER-460: e.g. why a requested fast scan fell back to the regular walk. */
  notice = $state('');
  /** FER-460: MFT read progress of a running fast scan (null otherwise). */
  mftProgress: { done: number; total: number } | null = $state(null);
  /** FER-464: this result was opened from the on-disk cache (a root outside
   *  the index), not scanned in this session. Always shown as stale. */
  fromCache = $state(false);
  /** FER-464: an in-app delete/rename/move touched the root after the scan. */
  cacheMutated = $state(false);

  // ── view state ──────────────────────────────────────────────────────────
  metric: DiskUsageSizeMetric = $state('logical');
  rightTab: DiskUsageTab = $state('treemap');
  /** Selected node — synced across tree/treemap/files (null = none). */
  selectedNodeId = $state<string | null>(null);
  /** Zoom path root→current; empty = scan root. Selection survives zoom. */
  zoomStack: string[] = $state([]);
  filterMinBytes = $state<number | null>(null);
  filterCategories: DiskUsageFileCategory[] = $state([]);

  // ── query results (guarded by seq captured at request time) ─────────────
  treemap: TreemapQuery | null = $state(null);
  files: FilesQuery | null = $state(null);
  /** Expanded-node children cache; cleared by scan/metric changes. */
  treeNodes: Record<string, TreeNodeState> = $state({});
  /** Level-1 rows while scanning — from event topChildren (no queries yet:
   *  the snapshot only exists after the final event). */
  scanPreview: DiskUsageChildItem[] = $state([]);
  /** Raw topChildren nodes of the latest event, by id (row counts/badges). */
  previewNodes: Record<string, DiskUsageNode> = $state({});
  expanded = $state(new SvelteSet<string>());

  // ── guards/plumbing (not reactive) ──────────────────────────────────────
  private treemapSeq = 0;
  private filesSeq = 0;
  private treeGen = 0;
  private treeLoadSeq = 0;
  private startSeq = 0;
  private awaitingStart = false;
  private earlyEvents: DiskUsageProgressEvent[] = [];
  private unlisten: UnlistenFn | null = null;
  private listenerStarted = false;

  /** Dismiss this side's action gate note (DSK-06.2). */
  clearDuGate(): void {
    this.duGate = null;
  }

  // ── scan lifecycle ──────────────────────────────────────────────────────
  get scanRunning(): boolean {
    return this.phase === 'scanning';
  }

  /** Phases that carry a queryable snapshot. */
  get hasData(): boolean {
    return DATA_PHASES.has(this.phase);
  }

  /** `fast` (FER-460): ask the backend to try the elevated MFT helper first;
   *  it falls back to the regular walk by itself and says why in `notice`. */
  /** `fast` = elevated MFT helper (FER-460); `'index'` = FerrumIndexer
   *  (FER-476, no UAC; falls back to the walk with a notice). */
  async start(fast: boolean | 'index' = false): Promise<void> {
    const root = this.rootInput.trim();
    if (!root) {
      this.errorText = 'Masukkan path root untuk discan.';
      return;
    }
    if (this.scanRunning) return; // Cancel adalah satu-satunya aksi saat scanning
    await this.ensureListener();
    const myStart = ++this.startSeq;
    if (this.scanId) void diskUsageCancel(this.scanId); // supersedes any straggler
    this.scanId = null;
    this.awaitingStart = true;
    this.earlyEvents = [];
    this.beginResult(root);
    this.phase = 'scanning';
    this.engine = fast === 'index' ? 'index' : fast ? 'ntfsMft' : 'walk';
    try {
      const id =
        fast === 'index'
          ? await diskUsageStart(root, { maxDepth: null }, false, 'index')
          : fast
            ? await diskUsageStart(root, { maxDepth: null }, true)
            : await diskUsageStart(root, { maxDepth: null });
      if (myStart !== this.startSeq) return; // a newer start owns the store
      this.scanId = id;
      this.awaitingStart = false;
      const buffered = this.earlyEvents;
      this.earlyEvents = [];
      for (const ev of buffered) this.onEvent(ev);
    } catch (e) {
      if (myStart !== this.startSeq) return;
      this.awaitingStart = false;
      this.earlyEvents = [];
      this.phase = 'error';
      this.errorText = e instanceof Error ? e.message : String(e);
    }
  }

  /** Clear everything a previous result left, for `root` (shared by a new
   *  scan and a cache open). */
  private beginResult(root: string): void {
    this.root = root;
    this.errorText = '';
    this.needsRescan = false;
    this.notice = '';
    this.mftProgress = null;
    this.fromCache = false;
    this.cacheMutated = false;
    this.stats = null;
    this.rootId = '';
    this.rootNode = null;
    this.snapshot = null;
    this.version = 0;
    this.scannedAtMs = 0;
    this.live = { ...EMPTY_LIVE };
    this.errors = [];
    this.scanPreview = [];
    this.previewNodes = {};
    this.resetView();
  }

  /** FER-464: open the on-disk cached scan of `root` (a root the index does
   *  not cover). Returns false when there is none, it is damaged, a scan is
   *  running, or something newer took over meanwhile — the caller just
   *  scans. The result is shown as Stale with its age; it is never silently
   *  refreshed. */
  async loadCached(root: string): Promise<boolean> {
    const target = root.trim();
    if (!target || this.scanRunning) return false;
    const myStart = this.startSeq;
    const res = await diskUsageCacheLoad(target);
    if (!res || myStart !== this.startSeq || this.scanRunning) return false;
    await this.ensureListener();
    this.startSeq++;
    if (this.scanId) void diskUsageCancel(this.scanId);
    this.beginResult(target);
    this.rootInput = target;
    this.scanId = res.summary.scanId;
    this.awaitingStart = false;
    this.earlyEvents = [];
    this.engine = res.engine;
    this.fromCache = true;
    this.cacheMutated = res.mutated;
    this.phase = 'stale';
    this.applySnapshot(res.summary);
    return true;
  }

  /** FER-574: the entry folder differs from the root of the result on screen.
   *  Drop that result and go back to the fresh-entry state (idle, `root` as
   *  the suggestion) so the caller can offer its cache — exactly the state a
   *  first entry starts from. Never touches a RUNNING scan (Cancel is the
   *  only action then; only the suggestion changes). Returns true when the
   *  result was dropped. */
  retarget(root: string): boolean {
    const target = root.trim();
    if (!target) return false;
    this.rootInput = target;
    if (this.scanRunning || this.phase === 'idle') return false;
    if (normalizePath(this.root) === normalizePath(target)) return false;
    this.startSeq++; // a pending cache open / start for the old root is void
    if (this.scanId) void diskUsageCancel(this.scanId);
    this.scanId = null;
    this.awaitingStart = false;
    this.earlyEvents = [];
    this.beginResult('');
    this.phase = 'idle';
    return true;
  }

  /** "Hapus cache": drop this root's file from disk. The result on screen
   *  stays (it is only memory) but is no longer labelled as cached. */
  async deleteCache(): Promise<void> {
    if (!this.root) return;
    await diskUsageCacheDelete(this.root);
    this.fromCache = false;
    this.cacheMutated = false;
  }

  async cancel(): Promise<void> {
    if (this.scanId && this.scanRunning) await diskUsageCancel(this.scanId);
  }

  /** Progress stream — exactly one handler for the whole app. Events for any
   *  scanId other than the current one are DROPPED (contract §3.4); while a
   *  start is in flight, unknown ids are buffered briefly and replayed only
   *  if the resolved scanId matches. */
  onEvent(e: DiskUsageProgressEvent): void {
    if (e.scanId !== this.scanId) {
      if (this.awaitingStart && this.earlyEvents.length < EARLY_EVENT_CAP) {
        this.earlyEvents.push(e);
      }
      return;
    }
    this.live = {
      scannedFiles: e.scannedFiles,
      scannedDirs: e.scannedDirs,
      scannedSymlinks: e.scannedSymlinks,
      knownLogicalBytes: e.knownLogicalBytes,
      knownAllocatedBytes: e.knownAllocatedBytes,
      totalErrors: e.totalErrors,
      coverage: e.coverage,
    };
    if (e.engine) this.engine = e.engine;
    if (e.notice) this.notice = e.notice;
    if (e.done || !e.mftRecordsTotal || e.mftRecordsTotal <= 0) {
      this.mftProgress = null;
    } else {
      // Second guard (backend already filters): clamp to the total and
      // never move backwards within the same total.
      const total = e.mftRecordsTotal;
      let done = Math.min(Math.max(e.mftRecordsDone ?? 0, 0), total);
      const prev = this.mftProgress;
      if (prev && prev.total === total) done = Math.max(done, prev.done);
      this.mftProgress = { done, total };
    }
    if (e.errors.length > 0) {
      this.errors = [...this.errors, ...e.errors].slice(0, KEPT_ERRORS);
    }
    this.scanPreview = e.topChildren.map(previewItem);
    const nodes: Record<string, DiskUsageNode> = {};
    for (const n of e.topChildren) nodes[n.id] = n;
    this.previewNodes = nodes;
    if (!e.done) return;
    this.awaitingStart = false;
    if (e.error) {
      // The scan could not run at all (missing/unreachable root): honest
      // error state, never 0 bytes (states.md rule 4).
      this.phase = 'error';
      this.errorText = e.error;
      return;
    }
    if (e.cancelled) this.phase = 'cancelled';
    else if (e.coverage === 'partial') this.phase = 'partial';
    else this.phase = 'complete'; // refreshSnapshot may still flip this to 'empty'
    this.errorText = '';
    this.resetView();
    void this.refreshSnapshot();
  }

  /** Fetch the stored snapshot of the finished scan: root identity, version,
   *  scan time, final stats; decides complete vs empty. NotFound (evicted)
   *  sets needsRescan. */
  async refreshSnapshot(): Promise<void> {
    const scanId = this.scanId;
    const myStart = this.startSeq;
    if (!scanId) return;
    const snap = await diskUsageSnapshot(scanId);
    if (scanId !== this.scanId || myStart !== this.startSeq) return;
    if (!snap) {
      this.needsRescan = true;
      return;
    }
    this.applySnapshot(snap);
  }

  /** Adopt a stored snapshot summary and load the first level of views. */
  private applySnapshot(snap: DiskUsageSnapshot): void {
    this.rootId = snap.rootId;
    this.version = snap.version;
    this.scannedAtMs = snap.scannedAtMs;
    this.snapshot = snap;
    this.rootNode = snap.rootNode ?? null;
    this.stats = snap.stats;
    if (this.phase === 'complete') {
      const empty =
        snap.stats.scannedFiles === 0 &&
        snap.stats.scannedDirs === 0 &&
        snap.stats.scannedSymlinks === 0;
      if (empty) this.phase = 'empty';
    }
    // The tree always lists the scan root's children (level 1) — load them
    // right away; deeper levels stay lazy on expand. The right view starts
    // scoped to the root (treemap + Files Top-N).
    if (this.hasData) {
      void this.loadChildren(snap.rootId);
      void this.loadTreemap();
      void this.loadFiles();
    }
  }

  /** Test/DSK-06 hook: cache invalidation arrives later (DSK-06.3); the UI
   *  and transition are already honest per states.md (time marker + rescan). */
  markStale(): void {
    if (this.fromCache) this.cacheMutated = true;
    if (this.phase === 'complete' || this.phase === 'partial' || this.phase === 'empty') {
      this.phase = 'stale';
    }
  }

  // ── view mutations (each bumps the guard it invalidates) ────────────────
  selectNode(id: string | null): void {
    if (this.selectedNodeId === id) return;
    this.selectedNodeId = id;
    // Files scope follows the selection; the treemap only highlights, so its
    // in-flight load stays valid and is NOT dropped here.
    this.filesSeq++;
    if (this.hasData) void this.loadFiles();
  }

  async zoomIn(nodeId: string): Promise<void> {
    if (!this.hasData) return;
    if (this.zoomNodeId === nodeId) {
      this.selectedNodeId = nodeId;
      return;
    }
    const root = this.rootId || this.root;
    const normRoot = normalizePath(root);
    const normTarget = normalizePath(nodeId);
    if (normTarget === normRoot) {
      this.zoomTo(0);
      return;
    }
    const stack = isSubpath(normTarget, normRoot)
      ? this.buildZoomStackTo(nodeId)
      : [...this.zoomStack, nodeId];
    const nextExpanded = new SvelteSet(this.expanded);
    for (const p of stack) {
      nextExpanded.add(p);
    }
    this.expanded = nextExpanded;
    this.zoomStack = stack;
    this.selectedNodeId = nodeId;
    this.refreshScopedQueries();

    // Ensure all ancestors' children are loaded in the tree
    const rootId = this.rootId || this.root;
    if (rootId && !this.treeNodes[rootId]) {
      await this.loadChildren(rootId);
    }
    for (const p of stack) {
      if (!this.treeNodes[p]) {
        await this.loadChildren(p);
      }
    }
  }

  async selectFile(fileId: string): Promise<void> {
    if (!this.hasData) return;
    let parent = cleanParent(fileId);
    const root = this.rootId || this.root;
    if (root.endsWith('\\') || root.endsWith('/')) {
      if (!parent.endsWith('\\')) parent += '\\';
    }
    const normRoot = normalizePath(root);
    const normParent = parent ? normalizePath(parent) : normRoot;

    let stack: string[] = [];
    if (normParent !== normRoot && parent && isSubpath(normParent, normRoot)) {
      stack = this.buildZoomStackTo(parent);
    }
    const nextExpanded = new SvelteSet(this.expanded);
    for (const p of stack) {
      nextExpanded.add(p);
    }
    this.expanded = nextExpanded;
    const zoomChanged =
      stack.length !== this.zoomStack.length || stack.some((s, i) => s !== this.zoomStack[i]);
    if (zoomChanged) {
      this.zoomStack = stack;
    }
    this.selectedNodeId = fileId;
    this.filesSeq++;
    if (zoomChanged) {
      this.refreshScopedQueries();
    } else {
      void this.loadFiles();
    }

    // Ensure all ancestors and the parent have children loaded in tree
    const rootId = this.rootId || this.root;
    if (rootId && !this.treeNodes[rootId]) {
      await this.loadChildren(rootId);
    }
    for (const p of stack) {
      if (!this.treeNodes[p]) {
        await this.loadChildren(p);
      }
    }
  }

  zoomOut(): void {
    if (this.zoomStack.length === 0) return;
    this.zoomStack = this.zoomStack.slice(0, -1);
    this.selectedNodeId = null;
    this.refreshScopedQueries();
  }

  /** Breadcrumb jump: crumb index i (0 = scan root) becomes the zoom level —
   *  keep zoomStack[0..i-1], i.e. EMPTY the stack for i = 0. crumbs()[i] IS
   *  zoomStack[i-1] for i ≥ 1, so slicing at i (not i+1) is what lands on the
   *  clicked crumb (DSK-07 B2 off-by-one regression). */
  zoomTo(index: number): void {
    if (index < 0 || index > this.zoomStack.length) return;
    this.zoomStack = this.zoomStack.slice(0, index);
    this.selectedNodeId = null;
    this.refreshScopedQueries();
  }

  /** Tree cursor → treemap/Files (FER-499). `zoomPath` is the absolute zoom
   *  stack below the scan root (ancestors, plus the folder itself when the
   *  cursor is on a folder); `selectedId` is the cursor row. Navigation never
   *  touches the scan root (rootInput/rootId): that would be a new scan. */
  focusFromTree(zoomPath: string[], selectedId: string | null): void {
    const prevFilesScope = this.filesScopeId;
    const sameZoom =
      zoomPath.length === this.zoomStack.length &&
      zoomPath.every((id, i) => id === this.zoomStack[i]);
    if (!sameZoom) {
      this.zoomStack = [...zoomPath];
    }
    this.selectedNodeId = selectedId;
    if (!sameZoom && this.hasData) {
      void this.loadTreemap();
    }
    if (this.filesScopeId !== prevFilesScope) {
      this.filesSeq++;
      if (this.hasData) void this.loadFiles();
    }
  }

  /**
   * Attempt to focus/navigate to `targetPath` within the current scan snapshot.
   * Returns true if the path exists in the scan snapshot and was focused, false otherwise.
   */
  async focusPathIfScanned(targetPath: string): Promise<boolean> {
    const scanId = this.scanId;
    if (!scanId || !this.hasData) return false;
    const target = targetPath.trim();
    if (!target) return false;

    const normTarget = normalizePath(target);
    const normRoot = normalizePath(this.rootId || this.root);
    if (normTarget === normRoot) {
      this.zoomTo(0);
      return true;
    }
    if (!isSubpath(normTarget, normRoot)) {
      return false;
    }

    let dirTarget: string | null = null;
    try {
      await diskUsageQueryChildren(scanId, target, 0, 1, 'largestFirst', { metric: this.metric });
      dirTarget = target;
    } catch {
      if (!target.endsWith('\\') && !target.endsWith('/')) {
        try {
          await diskUsageQueryChildren(scanId, target + '\\', 0, 1, 'largestFirst', {
            metric: this.metric,
          });
          dirTarget = target + '\\';
        } catch {
          // not a dir
        }
      }
    }

    if (dirTarget) {
      const stack = this.buildZoomStackTo(dirTarget);
      const nextExpanded = new SvelteSet(this.expanded);
      for (const p of stack) {
        nextExpanded.add(p);
      }
      this.expanded = nextExpanded;
      this.zoomStack = stack;
      this.selectedNodeId = dirTarget;
      this.refreshScopedQueries();
      const rootId = this.rootId || this.root;
      if (rootId && !this.treeNodes[rootId]) {
        await this.loadChildren(rootId);
      }
      for (const p of stack) {
        if (!this.treeNodes[p]) {
          await this.loadChildren(p);
        }
      }
      return true;
    }

    // Check if target might be a file inside a scanned parent
    let parent = cleanParent(target);
    if (parent && parent !== target) {
      let page: DiskUsageChildrenPage | null = null;
      try {
        page = await diskUsageQueryChildren(scanId, parent, 0, TREE_PAGE_SIZE, 'largestFirst', {
          metric: this.metric,
        });
      } catch {
        if (!parent.endsWith('\\') && !parent.endsWith('/')) {
          try {
            page = await diskUsageQueryChildren(
              scanId,
              parent + '\\',
              0,
              TREE_PAGE_SIZE,
              'largestFirst',
              {
                metric: this.metric,
              },
            );
            parent = parent + '\\';
          } catch {
            // not found
          }
        }
      }
      if (page) {
        const match = page.items.find(
          (it) => normalizePath(it.id) === normTarget || normalizePath(it.name) === normTarget,
        );
        if (match) {
          const stack = this.buildZoomStackTo(parent);
          const nextExpanded = new SvelteSet(this.expanded);
          for (const p of stack) {
            nextExpanded.add(p);
          }
          this.expanded = nextExpanded;
          this.zoomStack = stack;
          this.selectedNodeId = match.id;
          this.refreshScopedQueries();
          const rootId = this.rootId || this.root;
          if (rootId && !this.treeNodes[rootId]) {
            await this.loadChildren(rootId);
          }
          for (const p of stack) {
            if (!this.treeNodes[p]) {
              await this.loadChildren(p);
            }
          }
          return true;
        }
      }
    }
    return false;
  }

  buildZoomStackTo(target: string): string[] {
    const root = this.rootId || this.root;
    const normRoot = normalizePath(root);
    const hasTrailing = target.endsWith('\\') || target.endsWith('/');
    const stack: string[] = [];
    let cur = target.replace(/[\\/]+$/, '');
    if (/^[A-Za-z]:[\\/\\]$/.test(target)) cur = target;
    while (cur && normalizePath(cur) !== normRoot) {
      const entry = hasTrailing ? `${cur}\\` : cur;
      stack.unshift(entry);
      const p = cleanParent(cur);
      if (!p || p === cur) break;
      cur = p;
    }
    return stack;
  }

  setMetric(metric: DiskUsageSizeMetric): void {
    if (this.metric === metric) return;
    this.metric = metric;
    // Tree pages are sorted under the OLD metric — drop and refetch. The
    // root's level-1 page is loaded eagerly (refreshSnapshot) and the root
    // row is never in `expanded`, so refetch it explicitly too — otherwise
    // the tree's top level would go blank on every metric switch (DSK-07 B3).
    this.treeNodes = {};
    this.refreshScopedQueries();
    const rootNodeId = this.rootId;
    if (rootNodeId) void this.loadChildren(rootNodeId);
    for (const nodeId of this.expanded) void this.loadChildren(nodeId);
  }

  setRightTab(tab: DiskUsageTab): void {
    this.rightTab = tab;
  }

  /** Files filters re-query (never filter client-side: the pager totals and
   *  Top-N cap live server-side on the snapshot). */
  setFilesFilter(minBytes: number | null, categories: DiskUsageFileCategory[]): void {
    const sameMin = (this.filterMinBytes ?? null) === (minBytes ?? null);
    const sameCats =
      this.filterCategories.length === categories.length &&
      this.filterCategories.every((c, i) => categories[i] === c);
    if (sameMin && sameCats) return;
    this.filterMinBytes = minBytes;
    this.filterCategories = [...categories];
    this.filesSeq++;
    if (this.hasData) void this.loadFiles();
  }

  // ── tree expansion + pagination ─────────────────────────────────────────
  toggleExpand(nodeId: string): void {
    if (this.expanded.has(nodeId)) {
      const next = new SvelteSet(this.expanded);
      next.delete(nodeId);
      this.expanded = next;
      delete this.treeNodes[nodeId]; // a late page must not resurrect the node
    } else {
      const next = new SvelteSet(this.expanded);
      next.add(nodeId);
      this.expanded = next;
      void this.loadChildren(nodeId);
    }
  }

  /** Query one page of children for the tree. Guarded by scanId, the tree
   *  generation (scan/metric changes) and the per-node loadId (collapse or
   *  newer load for the same node) — late responses are discarded. */
  async loadChildren(nodeId: string, more = false): Promise<void> {
    const scanId = this.scanId;
    if (!scanId || !this.hasData) return;
    const gen = this.treeGen;
    let node = this.treeNodes[nodeId];
    if (!node) {
      node = {
        items: [],
        total: 0,
        loading: false,
        loadingMore: false,
        error: null,
        loadId: 0,
      };
      this.treeNodes[nodeId] = node;
    }
    const myLoad = ++this.treeLoadSeq;
    node.loadId = myLoad;
    node.error = null;
    const offset = more ? node.items.length : 0;
    if (more) node.loadingMore = true;
    else node.loading = true;
    try {
      const page = await diskUsageQueryChildren(
        scanId,
        nodeId,
        offset,
        TREE_PAGE_SIZE,
        'largestFirst',
        { metric: this.metric },
      );
      const cur = this.treeNodes[nodeId];
      if (
        scanId !== this.scanId ||
        gen !== this.treeGen ||
        !cur ||
        cur.loadId !== myLoad ||
        page.scanId !== scanId
      )
        return;
      cur.total = page.total;
      cur.items = more ? [...cur.items, ...page.items] : page.items;
      cur.loading = false;
      cur.loadingMore = false;
      this.treeNodes = { ...this.treeNodes, [nodeId]: { ...cur } };
    } catch (e) {
      const cur = this.treeNodes[nodeId];
      if (!cur || cur.loadId !== myLoad || gen !== this.treeGen || scanId !== this.scanId) return;
      cur.loading = false;
      cur.loadingMore = false;
      if (e instanceof FsError && e.code === 'notFound') {
        this.needsRescan = true; // snapshot evicted/unknown — offer Rescan (G8)
      } else {
        cur.error = e instanceof Error ? e.message : String(e);
      }
    }
  }

  // ── right-view queries ──────────────────────────────────────────────────
  /** Node the treemap draws: the current zoom level. */
  get zoomNodeId(): string {
    return this.zoomStack[this.zoomStack.length - 1] ?? this.rootId ?? this.root;
  }

  /** Node the Files view aggregates: the selected subtree, else the zoom. */
  get filesScopeId(): string {
    return this.selectedNodeId ?? this.zoomNodeId;
  }

  private refreshScopedQueries(): void {
    this.treemapSeq++;
    this.filesSeq++;
    if (this.hasData) {
      void this.loadTreemap();
      void this.loadFiles();
    }
  }

  async loadTreemap(): Promise<void> {
    const scanId = this.scanId;
    if (!scanId || !this.hasData) return;
    const seq = ++this.treemapSeq;
    const nodeId = this.zoomNodeId;
    const metric = this.metric;
    this.treemap = { items: [], total: 0, loading: true, error: null, seq };
    try {
      const page = await diskUsageQueryChildren(scanId, nodeId, 0, TREEMAP_LIMIT, 'largestFirst', {
        metric,
      });
      if (seq !== this.treemapSeq || scanId !== this.scanId || page.scanId !== scanId) return;

      const items: DiskUsageChildItem[] = page.items.map((it) => ({ ...it }));

      // Fetch Level 2 children for significant Level 1 directories (top 15)
      const l1Dirs = items
        .filter((it) => it.kind === 'dir' && metricValue(it, metric) > 0)
        .slice(0, 15);

      if (l1Dirs.length > 0) {
        const l2Results = await Promise.all(
          l1Dirs.map(async (dir) => {
            try {
              const res = await diskUsageQueryChildren(scanId, dir.id, 0, 50, 'largestFirst', {
                metric,
              });
              if (seq !== this.treemapSeq || scanId !== this.scanId) return null;
              return { dirId: dir.id, items: res.items.map((it) => ({ ...it })) };
            } catch {
              return null;
            }
          }),
        );
        if (seq !== this.treemapSeq || scanId !== this.scanId) return;

        const l2Map: Record<string, DiskUsageChildItem[]> = {};
        const allL2Dirs: DiskUsageChildItem[] = [];
        for (const res of l2Results) {
          if (!res) continue;
          l2Map[res.dirId] = res.items;
          for (const it of res.items) {
            if (it.kind === 'dir' && metricValue(it, metric) > 0) {
              allL2Dirs.push(it);
            }
          }
        }

        for (const dir of l1Dirs) {
          const ch = l2Map[dir.id];
          if (ch && ch.length > 0) {
            dir.children = ch;
          }
        }

        // Fetch Level 3 children for significant Level 2 directories (top 20)
        allL2Dirs.sort((a, b) => metricValue(b, metric) - metricValue(a, metric));
        const l2DirsToQuery = allL2Dirs.slice(0, 20);

        if (l2DirsToQuery.length > 0) {
          const l3Results = await Promise.all(
            l2DirsToQuery.map(async (dir) => {
              try {
                const res = await diskUsageQueryChildren(scanId, dir.id, 0, 30, 'largestFirst', {
                  metric,
                });
                if (seq !== this.treemapSeq || scanId !== this.scanId) return null;
                return { dirId: dir.id, items: res.items.map((it) => ({ ...it })) };
              } catch {
                return null;
              }
            }),
          );
          if (seq !== this.treemapSeq || scanId !== this.scanId) return;

          const l3Map: Record<string, DiskUsageChildItem[]> = {};
          for (const res of l3Results) {
            if (!res) continue;
            l3Map[res.dirId] = res.items;
          }

          for (const dir of l2DirsToQuery) {
            const ch = l3Map[dir.id];
            if (ch && ch.length > 0) {
              dir.children = ch;
            }
          }
        }
      }

      this.treemap = {
        items,
        total: page.total,
        loading: false,
        error: null,
        seq,
      };
    } catch (e) {
      if (seq !== this.treemapSeq || scanId !== this.scanId) return;
      if (e instanceof FsError && e.code === 'notFound') {
        this.needsRescan = true;
        this.treemap = null;
        return;
      }
      this.treemap = {
        items: [],
        total: 0,
        loading: false,
        error: e instanceof Error ? e.message : String(e),
        seq,
      };
    }
  }

  async loadFiles(): Promise<void> {
    const scanId = this.scanId;
    if (!scanId || !this.hasData) return;
    const seq = ++this.filesSeq;
    const nodeId = this.filesScopeId;
    const filter: DiskUsageFilter = {
      metric: this.metric,
      minBytes: this.filterMinBytes,
      categories: this.filterCategories.length > 0 ? [...this.filterCategories] : null,
    };
    this.files = { result: null, loading: true, error: null, seq };
    try {
      const result = await diskUsageTopFiles(scanId, nodeId, FILES_LIMIT, filter);
      if (seq !== this.filesSeq || scanId !== this.scanId || result.scanId !== scanId) return;
      this.files = { result, loading: false, error: null, seq };
    } catch (e) {
      if (seq !== this.filesSeq || scanId !== this.scanId) return;
      if (e instanceof FsError && e.code === 'notFound') {
        this.needsRescan = true;
        this.files = null;
        return;
      }
      this.files = {
        result: null,
        loading: false,
        error: e instanceof Error ? e.message : String(e),
        seq,
      };
    }
  }

  // ── derived display helpers ─────────────────────────────────────────────
  /** Status chip per states.md (tone + verbatim text family). */
  get chip(): { tone: 'idle' | 'scan' | 'ok' | 'warn' | 'err'; text: string } {
    switch (this.phase) {
      case 'idle':
        return { tone: 'idle', text: 'Idle — belum ada scan' };
      case 'scanning':
        return {
          tone: 'scan',
          text: this.mftPercent === null ? 'Scanning…' : `Scanning… ${this.mftPercent}%`,
        };
      case 'complete':
        return { tone: 'ok', text: 'Complete' };
      case 'partial':
        return { tone: 'warn', text: 'Partial — ada bagian yang tidak terbaca' };
      case 'cancelled':
        return { tone: 'idle', text: 'Cancelled' };
      case 'stale':
        return {
          tone: 'warn',
          text: this.fromCache
            ? `Stale · hasil ${ageLabel(this.scannedAtMs, Date.now())}`
            : `Stale · ${scanClock(this.scannedAtMs)}`,
        };
      case 'empty':
        return { tone: 'idle', text: 'Complete — lokasi kosong' };
      case 'error':
        return { tone: 'err', text: 'Error' };
    }
  }

  /** Counts line: arrived totals while scanning (NO percent, NO ETA), final
   *  stats afterwards. */
  /** FER-460: whole percent of the MFT READ phase, or null when unknown
   *  (regular walk, before the helper's first progress, or tree build). */
  get mftPercent(): number | null {
    const p = this.mftProgress;
    if (this.phase !== 'scanning' || !p || p.done >= p.total) return null;
    return Math.floor((p.done / p.total) * 100);
  }

  /** FER-460: headline of the scanning card for the fast engine. */
  fastScanStage(): string {
    const p = this.mftProgress;
    if (!p) return 'Membaca MFT (helper admin)…';
    if (p.done >= p.total) return 'Menyusun hasil…';
    return `Membaca MFT… ${this.mftPercent ?? 0}%`;
  }

  countsLine(): string {
    if (this.phase === 'scanning') {
      const files = this.live.scannedFiles.toLocaleString('en-US');
      const dirs = this.live.scannedDirs.toLocaleString('en-US');
      return `${files} files · ${dirs} folders`;
    }
    if (this.stats) {
      const files = this.stats.scannedFiles.toLocaleString('en-US');
      const dirs = this.stats.scannedDirs.toLocaleString('en-US');
      return `${files} files · ${dirs} folders`;
    }
    return '—';
  }

  /** Breadcrumb entries of the zoom scope: scan root first. */
  crumbs(): { id: string; label: string }[] {
    const root = this.rootId || this.root;
    const out = [{ id: root, label: baseLabel(root) }];
    for (const id of this.zoomStack) {
      if (id !== root) out.push({ id, label: baseLabel(id) });
    }
    return out;
  }

  // ── plumbing ────────────────────────────────────────────────────────────
  /** Idempotent progress-event subscription for THIS instance (the workspace
   *  calls it on entry; start() re-checks). Inert outside the desktop app. */
  async ensureListener(): Promise<void> {
    if (this.listenerStarted || !isInTauri()) return;
    this.listenerStarted = true;
    this.unlisten = await onDiskUsageEvent((e) => this.onEvent(e));
  }

  private resetView(): void {
    this.selectedNodeId = null;
    this.zoomStack = [];
    this.expanded = new SvelteSet();
    this.treeNodes = {};
    this.treemap = null;
    this.files = null;
    this.treemapSeq++;
    this.filesSeq++;
    this.treeGen++;
  }

  /** Exposed for tests: feed an event through the same path as IPC. */
  testEvent(e: DiskUsageProgressEvent): void {
    this.onEvent(e);
  }

  /** Test hook: pretend a scan under `scanId` finished in `phase` (a data
   *  phase, so queries run) without driving the full start() flow. */
  testData(
    scanId: string,
    phase: Exclude<DiskUsagePhase, 'idle' | 'scanning'>,
    rootId: string,
  ): void {
    this.scanId = scanId;
    this.phase = phase;
    this.root = rootId;
    this.rootId = rootId;
  }

  /** Test hook: full reset. */
  reset(): void {
    void this.unlisten?.();
    this.unlisten = null;
    this.listenerStarted = false;
    this.duGate = null;
    this.fromCache = false;
    this.cacheMutated = false;
    this.phase = 'idle';
    this.root = '';
    this.rootInput = '';
    this.scanId = null;
    this.version = 0;
    this.scannedAtMs = 0;
    this.rootId = '';
    this.rootNode = null;
    this.snapshot = null;
    this.stats = null;
    this.live = { ...EMPTY_LIVE };
    this.errors = [];
    this.errorText = '';
    this.needsRescan = false;
    this.metric = 'logical';
    this.rightTab = 'treemap';
    this.selectedNodeId = null;
    this.zoomStack = [];
    this.filterMinBytes = null;
    this.filterCategories = [];
    this.treemap = null;
    this.files = null;
    this.treeNodes = {};
    this.scanPreview = [];
    this.previewNodes = {};
    this.expanded = new SvelteSet();
    this.treemapSeq++;
    this.filesSeq++;
    this.treeGen++;
    this.treeLoadSeq++;
    this.startSeq++;
    this.awaitingStart = false;
    this.earlyEvents = [];
  }
}

function baseLabel(path: string): string {
  const norm = path.replace(/[\\/]+$/, '');
  const i = Math.max(norm.lastIndexOf('/'), norm.lastIndexOf('\\'));
  if (i === -1) return norm || path;
  const base = norm.slice(i + 1);
  return base || norm; // drive roots keep "C:"
}

/** "09:14" local clock for the stale time marker (states.md rule 3). */
function scanClock(ms: number): string {
  if (!Number.isFinite(ms) || ms <= 0) return '--:--';
  const d = new Date(ms);
  const p = (n: number) => String(n).padStart(2, '0');
  return `${p(d.getHours())}:${p(d.getMinutes())}`;
}

/** DSK-10 (FER-408): factory for independent stores. Since DSK-11 the Disk
 *  usage workspace builds exactly ONE (its single session across both
 *  panes); tests may build more. Each instance owns its scanId guard,
 *  listener and view state — no shared mutable state between instances. */
export function createDiskUsageStore(): DiskUsageStore {
  return new DiskUsageStore();
}

export function normalizePath(p: string): string {
  return p.replace(/\//g, '\\').replace(/\\+$/, '').toLowerCase();
}

export function isSubpath(child: string, parent: string): boolean {
  if (child === parent) return true;
  const prefix = parent.endsWith('\\') ? parent : parent + '\\';
  return child.startsWith(prefix);
}

export function cleanParent(path: string): string {
  const norm = path.replace(/[\\/]+$/, '');
  const i = Math.max(norm.lastIndexOf('/'), norm.lastIndexOf('\\'));
  if (i === -1) return '';
  const par = norm.slice(0, i + 1);
  if (/^[A-Za-z]:[\\/\\]$/.test(par)) return par.replace('/', '\\').toUpperCase();
  return par.replace(/[\\/]+$/, '');
}
