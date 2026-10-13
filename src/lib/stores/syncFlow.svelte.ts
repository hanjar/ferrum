/**
 * F2-19 Synchronize Dirs flow state (moved out of compareStore by FER-389).
 * `CompareStore extends SyncFlow`, so `compareStore.openSync()` and friends
 * keep working. The sync report is its OWN snapshot (recursive, optional
 * content mode) and never overwrites the compare panel's `report`.
 *
 * Rules enforced here: a partial report (truncated / cancelled / errors)
 * yields no runnable plan and shows why; overwrites need an explicit
 * confirmation; undecided conflicts are never executed; every item gets a
 * logged outcome; deletes always pass through the delete confirmation.
 */
import { cancelCompare, type CompareDirsReport } from '../ipc/compare';
import {
  compareForSync,
  loadSyncBaseline,
  runSyncPlan,
  settleSyncRun,
  type BaselineStatus,
  type SettleResult,
  type SyncItemResult,
} from '../syncService';
import type { OperationProgress } from '../ipc/types';
import { isInTauri } from '../ipc/tauri';
import {
  applyDecisions,
  buildSyncPlan,
  describeBlockers,
  isResolvable,
  planSummary,
  syncPlanBlockers,
  type SyncDecision,
  type SyncDirection,
  type SyncPlanItem,
} from '../syncPlan';
import type { SyncBaseline } from '../syncBaseline';
import { activePane, inactivePane } from './panesRegistry.svelte';
import { dialogs } from './dialogStore.svelte';
import { ui } from '../uiState.svelte';
import { diskUsageFastEligibility } from '../ipc/diskUsage';
import type { DiskUsageFastEligibility } from '../ipc/types';
import { chooseScanMode, type OfferAnswer, type ScanMode } from '../diskUsage/fastScanOffer';
import { ops } from './opsStore.svelte';

export interface SyncDone {
  toRight: number;
  toLeft: number;
  deletes: number;
  failed: number;
  skipped: number;
  cancelled: boolean;
}

/** Explicit left/right folders for `openSync` (instead of the active/inactive pane pair). */
export interface SyncRoots {
  left: string;
  right: string;
}

/** FER-472: the Disk Usage offer pattern for Sync. The offer appears when the preference allows it
 *  AND at least one of the two folders is on a local NTFS drive with the helper installed. */
export async function chooseSyncScanMode(left: string, right: string): Promise<ScanMode | null> {
  return chooseScanMode(left, {
    offerEnabled: () => ui.diskUsageFastOffer,
    eligibility: async (): Promise<DiskUsageFastEligibility> => {
      const [a, b] = await Promise.all([
        diskUsageFastEligibility(left),
        diskUsageFastEligibility(right),
      ]);
      const vols = [a, b]
        .filter((e) => e.eligible && e.volume)
        .map((e) => e.volume as string)
        .filter((v, idx, all) => all.indexOf(v) === idx);
      return vols.length > 0
        ? { eligible: true, volume: vols.join(': dan ') }
        : { eligible: false };
    },
    ask: (elig) =>
      new Promise<OfferAnswer>((resolve) => {
        let settled = false;
        dialogs.open({
          kind: 'fastScanOffer',
          purpose: 'sync',
          root: left,
          rightRoot: right,
          volume: elig.volume ?? '',
          choose: (m, dontAskAgain) => {
            if (settled) return;
            settled = true;
            resolve({ mode: m, dontAskAgain });
            if (dialogs.current?.kind === 'fastScanOffer') dialogs.close();
          },
        });
      }),
    disableOffer: () => {
      ui.diskUsageFastOffer = false;
    },
  });
}

export class SyncFlow {
  /** Shared with the compare panel's error line (declared here so the base owns it). */
  errorText = $state('');

  syncOpen = $state(false);
  syncDirection = $state<SyncDirection>('both');
  syncDeleteExtras = $state(false);
  /** FER-112: compare contents (plan AND stale re-validation). */
  syncByContent = $state(false);
  syncReport = $state<CompareDirsReport | null>(null);
  /** The previewed plan with user decisions applied. */
  syncPlan = $state<SyncPlanItem[] | null>(null);
  syncBusy = $state(false);
  syncStale = $state(false);
  /** Non-empty when the plan was refused (truncated / cancelled / read errors). */
  syncBlocked = $state('');
  /** Set when executeSync needs the user to confirm overwrites first. */
  syncConfirm = $state<{ overwrites: number; names: string[] } | null>(null);
  syncDecisions = $state<Record<string, SyncDecision>>({});
  syncDone = $state<SyncDone | null>(null);
  /** Per-item outcome of the last run (queued / skipped / failed + reason). */
  syncLog = $state<SyncItemResult[]>([]);
  /** Trusted two-way baseline of the current root pair (loaded from the local store, FER-414). */
  syncBaseline = $state<SyncBaseline | null>(null);
  /** Where `syncBaseline` came from; anything but `trusted` = first-run behaviour. */
  syncBaselineStatus = $state<BaselineStatus>('notUsed');
  /** Result of the last post-run baseline settlement (saved / unchanged / skipped / saveFailed). */
  syncSettle = $state<SettleResult | null>(null);
  /** Settlement of the last run (promise), so callers/tests can await the baseline save. */
  lastSettlement: Promise<SettleResult> | null = null;
  /** Reads live queue progress for settlement; overridable in tests. */
  syncJobReader: (jobId: string) => OperationProgress | undefined = (id) => ops.ops.get(id);
  /** The baseline store exists only in the desktop shell; overridable in tests. */
  syncBaselineEnabled: () => boolean = () => isInTauri();

  /** Folder pair pinned from outside the panes (the chat result card's "Resolve in Sync panel").
   *  Null = the usual active/inactive pane pair. Cleared on every plain open and on close. */
  private syncRoots: SyncRoots | null = null;
  private syncBasePlan: SyncPlanItem[] | null = null;
  /** FER-472: the scan mode picked when the panel was opened; Refresh and the content toggle
   *  reuse it instead of asking again. */
  private syncFastScan = false;
  /** The folder pair the choice above was made for; another pair asks again (FER-472 L4). */
  private syncFastPair = '';
  /** FER-472: asks "Scan lebih cepat?" for a folder pair. `null` = dismissed (no scan).
   *  Overridable in tests. */
  syncScanChooser: (left: string, right: string) => Promise<ScanMode | null> = (l, r) =>
    chooseSyncScanMode(l, r);
  private syncCancelRequested = false;
  private syncOpId: string | null = null;

  private newOpId(): string {
    this.syncOpId = `sync-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
    return this.syncOpId;
  }

  /** `roots` = pin an explicit folder pair; `null` = back to the active/inactive panes;
   *  omitted = keep whatever pair the panel currently has (Refresh, content toggle).
   *  Open the sync panel: runs a fresh compare first, so the dry-run
   *  preview always reflects on-disk reality, then builds the plan. */
  async openSync(roots?: SyncRoots | null): Promise<void> {
    if (roots !== undefined) this.syncRoots = roots;
    const pinned = this.syncRoots;
    const left = pinned ? { path: pinned.left } : activePane();
    const right = pinned ? { path: pinned.right } : inactivePane();
    const pairKey = left && right ? `${left.path}\n${right.path}` : '';
    const freshOpen = !this.syncOpen || pairKey !== this.syncFastPair;
    this.syncOpen = true;
    this.syncStale = false;
    this.syncDone = null;
    this.syncLog = [];
    this.syncConfirm = null;
    if (!left || !right) {
      this.errorText = 'Both panes must show a folder to synchronize.';
      this.syncPlan = null;
      this.syncBasePlan = null;
      return;
    }
    this.syncBusy = true;
    this.errorText = '';
    this.syncBlocked = '';
    this.syncCancelRequested = false;
    try {
      if (freshOpen) {
        // FER-472: UAC offer for local NTFS folders (default / Enter = ordinary scan).
        const mode = await this.syncScanChooser(left.path, right.path).catch(
          () => 'normal' as const,
        );
        if (mode === null) {
          this.syncOpen = false;
          this.syncBusy = false;
          return;
        }
        this.syncFastScan = mode === 'fast';
        this.syncFastPair = pairKey;
      }
      this.syncReport = await compareForSync(left.path, right.path, {
        byContent: this.syncByContent,
        opId: this.newOpId(),
        ...(this.syncFastScan ? { fastScan: true } : {}),
      });
      // Stored baseline of this pair; anything untrusted = first-run (never blocks the preview).
      const loaded = await loadSyncBaseline(this.syncReport.leftRoot, this.syncReport.rightRoot);
      this.syncBaseline = loaded.baseline;
      this.syncBaselineStatus = loaded.status;
    } catch (e) {
      this.errorText = e instanceof Error ? e.message : String(e);
      this.syncReport = null;
    } finally {
      this.syncBusy = false;
    }
    this.syncDecisions = {};
    this.regenerateSyncPlan();
  }

  regenerateSyncPlan(): void {
    const report = this.syncReport;
    if (!report) {
      this.syncPlan = null;
      this.syncBasePlan = null;
      return;
    }
    if (syncPlanBlockers(report).length > 0) {
      // Never offer a runnable plan from a partial snapshot.
      this.syncBlocked = describeBlockers(report);
      this.syncPlan = null;
      this.syncBasePlan = null;
      return;
    }
    this.syncBlocked = '';
    this.syncBasePlan = buildSyncPlan(report, {
      direction: this.syncDirection,
      deleteExtras: this.syncDeleteExtras,
      baseline: this.syncBaseline,
    });
    this.syncPlan = applyDecisions(this.syncBasePlan, this.syncDecisions);
  }

  private resetRunState(): void {
    this.syncStale = false;
    this.syncDone = null;
    this.syncLog = [];
    this.syncConfirm = null;
  }

  setSyncDirection(direction: SyncDirection): void {
    this.syncDirection = direction;
    if (direction === 'both') this.syncDeleteExtras = false;
    this.syncDecisions = {};
    this.resetRunState();
    this.regenerateSyncPlan();
  }

  setSyncDeleteExtras(deleteExtras: boolean): void {
    this.syncDeleteExtras = deleteExtras;
    this.syncDecisions = {};
    this.resetRunState();
    this.regenerateSyncPlan();
  }

  /** FER-112: content mode changes the compare itself, so re-run it. */
  async setSyncByContent(byContent: boolean): Promise<void> {
    this.syncByContent = byContent;
    await this.openSync(this.syncRoots ?? undefined);
  }

  /** Decide one conflict (keep left / keep right / skip). */
  decideConflict(name: string, decision: SyncDecision | null): void {
    const next = { ...this.syncDecisions };
    if (decision) next[name] = decision;
    else delete next[name];
    this.syncDecisions = next;
    this.syncConfirm = null;
    this.regenerateSyncPlan();
  }

  /** "Apply to all": the same decision for every still-undecided conflict
   *  that may legally be resolved that way (skip is always allowed). */
  decideAllConflicts(decision: SyncDecision): void {
    const base = this.syncBasePlan;
    if (!base) return;
    const next = { ...this.syncDecisions };
    for (const item of base.filter((p) => p.action === 'conflict')) {
      if (decision === 'skip' || isResolvable(item)) next[item.name] = decision;
    }
    this.syncDecisions = next;
    this.syncConfirm = null;
    this.regenerateSyncPlan();
  }

  get syncSummary() {
    return this.syncPlan ? planSummary(this.syncPlan) : null;
  }

  /** Ask a running sync (re-validation compare / between batches) to stop. */
  cancelSync(): void {
    if (!this.syncBusy) return;
    this.syncCancelRequested = true;
    if (this.syncOpId) void Promise.resolve(cancelCompare(this.syncOpId)).catch(() => {});
  }

  /**
   * Execute the reviewed plan. MANDATORY dry-run: the current preview is the
   * only thing that executes, and it is first re-validated against a fresh
   * compare (same mode) — a stale plan is never executed, it is regenerated
   * for another review. Overwrites need `confirmOverwrite`; without it the
   * first call only raises `syncConfirm` and enqueues nothing.
   */
  async executeSync(opts: { confirmOverwrite?: boolean } = {}): Promise<void> {
    const plan = this.syncPlan;
    const report = this.syncReport;
    if (!plan || !report || this.syncBusy) return;
    const summary = planSummary(plan);
    if (summary.overwrites > 0 && !opts.confirmOverwrite) {
      this.syncConfirm = {
        overwrites: summary.overwrites,
        names: plan
          .filter((p) => p.action === 'updateRight' || p.action === 'updateLeft')
          .map((p) => p.name),
      };
      return;
    }
    this.syncConfirm = null;
    this.syncBusy = true;
    this.syncStale = false;
    this.syncCancelRequested = false;
    this.errorText = '';
    try {
      const result = await runSyncPlan(report, plan, {
        byContent: this.syncByContent,
        allowOverwrite: opts.confirmOverwrite === true,
        shouldCancel: () => this.syncCancelRequested,
        opId: this.newOpId(),
      });
      if (result.status === 'stale') {
        this.syncReport = result.fresh;
        this.regenerateSyncPlan();
        this.syncStale = true;
        this.errorText =
          'Sources changed since the preview — the plan was refreshed. Review it before running again.';
        return;
      }
      if (result.status === 'blocked') {
        this.syncBlocked = result.reasons.includes('cancelled') ? '' : result.message;
        this.errorText = result.reasons.includes('cancelled')
          ? 'Synchronization cancelled — nothing was changed.'
          : result.message;
        this.syncLog = [];
        this.syncDone = null;
        return;
      }
      this.syncLog = result.items;
      this.syncDone = {
        toRight: result.toRight,
        toLeft: result.toLeft,
        deletes: result.deletes.length,
        failed: result.failed,
        skipped: result.skipped,
        cancelled: result.cancelled,
      };
      if (result.failed > 0) {
        const why = result.items.find((i) => i.status === 'failed')?.reason ?? 'unknown error';
        this.errorText = `${result.failed} item(s) failed to queue (${why}); the rest of the run is reported below.`;
      } else if (result.cancelled) {
        this.errorText = 'Synchronization cancelled — remaining actions were not run.';
      }
      // Baseline: after the jobs finish, take a fresh compare and record only verified items.
      // Runs in the background (the panel stays responsive); failures/cancels never move it.
      this.syncSettle = null;
      this.lastSettlement = !this.syncBaselineEnabled()
        ? null
        : settleSyncRun({
            report,
            plan,
            result,
            readJob: this.syncJobReader,
          })
            .catch((): SettleResult => ({ status: 'skipped', reason: 'verifyFailed' }))
            .then((settled) => {
              this.syncSettle = settled;
              return settled;
            });
      // Deletions ALWAYS require the explicit delete confirmation dialog
      // (trash default, D05; FER-364 carries the permanent flag explicitly).
      if (result.deletes.length > 0) {
        dialogs.open({ kind: 'delete', paths: result.deletes, permanent: false });
      }
      this.syncPlan = null; // executed plan is consumed; a fresh preview is required for another run
      this.syncBasePlan = null;
    } catch (e) {
      this.errorText = e instanceof Error ? e.message : String(e);
    } finally {
      this.syncBusy = false;
    }
  }

  closeSync(): void {
    this.syncOpen = false;
    this.syncRoots = null;
    this.syncPlan = null;
    this.syncBasePlan = null;
    this.syncReport = null;
    this.syncBlocked = '';
    this.syncDecisions = {};
    this.resetRunState();
  }

  /** Reset every sync field (called from the owner's reset()). */
  resetSync(): void {
    this.syncOpen = false;
    this.syncRoots = null;
    this.syncDirection = 'both';
    this.syncDeleteExtras = false;
    this.syncByContent = false;
    this.syncReport = null;
    this.syncPlan = null;
    this.syncBasePlan = null;
    this.syncBusy = false;
    this.syncBlocked = '';
    this.syncDecisions = {};
    this.syncBaseline = null;
    this.syncBaselineStatus = 'notUsed';
    this.syncSettle = null;
    this.lastSettlement = null;
    this.syncCancelRequested = false;
    this.syncOpId = null;
    this.resetRunState();
  }
}
