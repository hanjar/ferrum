/**
 * Synchronize Dirs service (F2-19, extracted by FER-180). Holds the compare,
 * stale-check and queue-enqueue steps that used to live inside the compare
 * store, so the manual sync panel and the AI facade adapter share one
 * implementation. It forwards to the existing compare/copy IPC only; the
 * classification stays in `syncPlan.ts`. Delete confirmation is NOT done here:
 * deletes are returned and the caller decides (the manual panel opens its
 * delete dialog; the chat adapter refuses plans that contain deletes).
 *
 * FER-110/111/112: a plan built from a truncated / cancelled / errored report
 * is never executed; overwrites run only when the caller confirmed them; the
 * stale check honours the compare mode (content mode compares verdicts too);
 * every item gets an outcome and a failed batch never hides an earlier one.
 */
import { compareDirs, validateSyncRoots, type CompareDirsReport } from './ipc/compare';
import { enqueueCopy } from './ipc/mutations';
import { ui } from './uiState.svelte';
import {
  syncBaselineLoad,
  syncBaselineSave,
  SyncBaselineError,
  type FirstRunReason,
} from './ipc/syncBaseline';
import {
  syncLeaseAcquire,
  syncLeaseRelease,
  SyncLeaseError,
  type LeaseAcquire,
  type LeaseHolder,
} from './ipc/syncLease';
import type { OperationProgress } from './ipc/types';
import {
  isBaselineTrusted,
  reconcileBaseline,
  type ItemOutcome,
  type SyncBaseline,
} from './syncBaseline';
import {
  buildSyncPlan,
  describeBlockers,
  isPlanStale,
  planSummary,
  syncPlanBlockers,
  type PlanSummary,
  type SyncAction,
  type SyncOptions,
  type SyncPlanItem,
} from './syncPlan';

// ── Root leases (Automation P0.4, design §3.4) ─────────────────────────

/** The two lease calls, injectable so flows and tests need no backend. */
export interface SyncLeaseApi {
  acquire(left: string, right: string): Promise<LeaseAcquire>;
  release(leaseId: string): Promise<void>;
}

export const defaultSyncLeaseApi: SyncLeaseApi = {
  acquire: syncLeaseAcquire,
  release: syncLeaseRelease,
};

/** A held folder lease. `release` is idempotent and never throws. A lease that could not be
 *  released (store hiccup) stays held by the backend until the app reloads its page (which
 *  releases every manual lease), the backend's 12 h hold limit passes, or Ferrum exits - it does
 *  NOT expire by itself while the app runs, because the backend keeps beating it. */
export interface HeldSyncLease {
  release(): Promise<void>;
}

export type SyncLeaseOutcome =
  | { status: 'acquired'; lease: HeldSyncLease }
  | { status: 'busy'; message: string; holder?: LeaseHolder }
  | { status: 'failed'; message: string };

/** What the panel shows when the folders are already locked by someone else. */
export function syncLeaseBusyMessage(holder?: LeaseHolder): string {
  if (holder?.kind === 'runner' || holder?.kind === 'app') {
    return 'These folders are in use by an automation — wait for it to finish or stop it. Nothing was changed.';
  }
  if (holder?.kind === 'manual') {
    return 'These folders are in use by another synchronization — wait for it to finish. Nothing was changed.';
  }
  return 'These folders are in use by another sync or automation — wait or stop it. Nothing was changed.';
}

/** A message per failure code; nothing here carries a path. */
function leaseFailureMessage(e: unknown): string {
  const code = e instanceof SyncLeaseError ? e.code : '';
  switch (code) {
    case 'store_read_only':
      return 'Ferrum was updated - restart Ferrum. Nothing was changed.';
    case 'store_unavailable':
      return 'The automation store is unavailable, so the folders cannot be locked for synchronization. Restart Ferrum; if it persists, check the Ferrum data folder. Nothing was changed.';
    case 'store_untrusted':
      return 'The automation store location is not trusted (it is, or sits below, a link), so the folders cannot be locked. Check the Ferrum data folder. Nothing was changed.';
    case 'invalid_input':
      return 'These folders cannot be locked for synchronization: a path is not valid. Nothing was changed.';
    default: {
      const why = e instanceof Error && e.message ? ` (${e.message})` : '';
      return `The folders could not be locked for synchronization${why}. Nothing was changed.`;
    }
  }
}

/**
 * Take the cross-process lease for a folder pair BEFORE anything is re-validated or enqueued
 * (design §3.4). `busy` and `failed` both mean: run nothing. Failing to lock is never turned
 * into running unlocked, because without a lease the backend refuses the baseline save and a
 * concurrent automation could touch the same folders.
 */
export async function acquireSyncLease(
  leftRoot: string,
  rightRoot: string,
  api: SyncLeaseApi = defaultSyncLeaseApi,
): Promise<SyncLeaseOutcome> {
  let answer: LeaseAcquire;
  try {
    answer = await api.acquire(leftRoot, rightRoot);
  } catch (e) {
    if (e instanceof SyncLeaseError && e.code === 'lease_busy') {
      return {
        status: 'busy',
        message:
          'Too many synchronizations are running at once — wait for one to finish. Nothing was changed.',
      };
    }
    return { status: 'failed', message: leaseFailureMessage(e) };
  }
  if ('status' in answer) {
    return {
      status: 'busy',
      message: syncLeaseBusyMessage(answer.holder),
      holder: answer.holder,
    };
  }
  const leaseId = answer.leaseId;
  let released = false;
  return {
    status: 'acquired',
    lease: {
      async release() {
        if (released) return;
        released = true;
        try {
          await api.release(leaseId);
        } catch {
          // Expires by itself; nothing the caller can do.
        }
      },
    },
  };
}

/** Where the baseline used for a plan came from (FER-414). */
export type BaselineStatus = 'trusted' | FirstRunReason | 'loadFailed' | 'notUsed';

export interface SyncDryRun {
  report: CompareDirsReport;
  plan: SyncPlanItem[];
  summary: PlanSummary;
  /** `trusted` = stored baseline used; anything else = first-run behaviour. */
  baselineStatus: BaselineStatus;
}

export interface SyncCompareOptions {
  /** Compare file contents (FER-112). Default false (size+mtime). */
  byContent?: boolean;
  /** Descend into sub-folders (default true for sync). */
  recursive?: boolean;
  /** Operation id so the engine can cancel this compare. */
  opId?: string;
  /** FER-472: read local NTFS roots through the MFT helper (UAC). Only the preview scan asks
   *  for it; the re-validation before a run always uses the ordinary scan. */
  fastScan?: boolean;
}

export interface SyncRunOptions {
  /** Must match the mode the plan was built with (FER-112). */
  byContent?: boolean;
  /** Overwrites run only when true (FER-111); otherwise they are skipped and reported. */
  allowOverwrite?: boolean;
  /** Polled before every step; true stops further actions (cancel). */
  shouldCancel?: () => boolean;
  opId?: string;
}

export type SyncItemStatus = 'queued' | 'skipped' | 'failed' | 'awaitingConfirm';

/** Per-item outcome of one run (never silently dropped). */
export interface SyncItemResult {
  name: string;
  action: SyncAction;
  status: SyncItemStatus;
  reason?: string;
  /** Queue job that carries this item (queued items only); maps results back to the item. */
  jobId?: string;
}

export type SyncRunResult =
  | { status: 'stale'; fresh: CompareDirsReport }
  | {
      status: 'blocked';
      /** Codes from syncPlanBlockers, or 'cancelled'. */
      reasons: string[];
      message: string;
    }
  | {
      status: 'queued';
      /** Queue operation ids, right-bound copy first, then left-bound. */
      jobIds: string[];
      toRight: number;
      toLeft: number;
      /** Paths the plan wants deleted; NOT executed here (caller confirms). */
      deletes: string[];
      /** Conflict entries: left untouched, never executed. */
      conflicts: number;
      /** Items whose copy batch failed to enqueue. */
      failed: number;
      /** Items not executed (unresolved conflict, unconfirmed overwrite, cancelled, …). */
      skipped: number;
      /** True when a cancel cut the run short. */
      cancelled: boolean;
      items: SyncItemResult[];
    };

/** Fresh read-only compare of two roots (dry-run input), recursive by default. */
export async function compareForSync(
  leftRoot: string,
  rightRoot: string,
  opts: SyncCompareOptions = {},
): Promise<CompareDirsReport> {
  const recursive = opts.recursive ?? true;
  const byContent = opts.byContent ?? false;
  // Same/nested/aliased roots would copy or delete a tree into itself.
  await validateSyncRoots(leftRoot, rightRoot);
  // Top-level metadata compare keeps the original 3-argument call.
  if (!recursive && !opts.opId && !byContent) return compareDirs(leftRoot, rightRoot, byContent);
  return compareDirs(leftRoot, rightRoot, byContent, {
    recursive,
    // Sync by content must re-read bytes, never trust a cached verdict.
    ...(byContent ? { noCache: true } : {}),
    ...(opts.opId ? { opId: opts.opId } : {}),
    ...(opts.fastScan ? { fastScan: true } : {}),
    // FER-477b: a recursive compare by content may skip the content reads that the
    // FerrumIndexer's change lists prove unnecessary (Rust decides; any doubt = full read).
    // Opt-in (Settings > Indexer, default off; the backend checks the pref too).
    ...(byContent && recursive && ui.syncJournalSkip ? { incremental: true } : {}),
  });
}

/**
 * Load the stored baseline of a root pair. Anything that is not a trusted baseline (never
 * saved, corrupt, unknown version, other roots, volume detached, store unavailable) is
 * `baseline: null` = first-run; a read failure never blocks planning, it only makes it
 * more conservative.
 */
export async function loadSyncBaseline(
  leftRoot: string,
  rightRoot: string,
): Promise<{ baseline: SyncBaseline | null; status: BaselineStatus }> {
  try {
    const r = await syncBaselineLoad({ leftRoot, rightRoot });
    if (r.status === 'ok' && isBaselineTrusted(r.baseline, leftRoot, rightRoot)) {
      return { baseline: r.baseline, status: 'trusted' };
    }
    return { baseline: null, status: r.status === 'ok' ? 'mismatch' : r.reason };
  } catch {
    return { baseline: null, status: 'loadFailed' };
  }
}

/** Read-only dry-run: compare + classify. Touches nothing. Two-way mode loads the stored
 *  baseline of the pair (unless the caller supplied one); without it the plan is first-run. */
export async function buildSyncDryRun(
  leftRoot: string,
  rightRoot: string,
  options: SyncOptions,
  compareOpts: SyncCompareOptions = {},
): Promise<SyncDryRun> {
  const report = await compareForSync(leftRoot, rightRoot, compareOpts);
  let baselineStatus: BaselineStatus = 'notUsed';
  let planOptions = options;
  if (options.baseline === undefined && options.direction === 'both') {
    const loaded = await loadSyncBaseline(report.leftRoot, report.rightRoot);
    baselineStatus = loaded.status;
    planOptions = { ...options, baseline: loaded.baseline };
  } else if (options.direction === 'both') {
    baselineStatus = options.baseline ? 'trusted' : 'none';
  }
  const plan = buildSyncPlan(report, planOptions);
  return { report, plan, summary: planSummary(plan), baselineStatus };
}

function parentDir(path: string): string {
  const i = Math.max(path.lastIndexOf('/'), path.lastIndexOf('\\'));
  return i > 0 ? path.slice(0, i) : path;
}

/** enqueueCopy copies INTO a directory, so nested items are grouped by their
 *  destination parent (a relative `sub/a.txt` must land in `<root>/sub`). */
function destDirOf(item: SyncPlanItem, toRight: boolean): string {
  return parentDir(toRight ? item.rightPath! : item.leftPath!);
}

const isCopy = (a: SyncAction) =>
  a === 'copyToRight' || a === 'copyToLeft' || a === 'updateRight' || a === 'updateLeft';
const isToRight = (a: SyncAction) => a === 'copyToRight' || a === 'updateRight';
const isDelete = (a: SyncAction) => a === 'deleteRight' || a === 'deleteLeft';

function errMessage(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

/**
 * Execute a reviewed plan. MANDATORY dry-run: the snapshot is first
 * re-validated against a fresh compare (same mode as the plan); a stale or
 * blocked plan enqueues nothing. Copies go through the existing operation
 * queue (overwrite policy, only for confirmed overwrites).
 */
export async function runSyncPlan(
  report: CompareDirsReport,
  plan: readonly SyncPlanItem[],
  opts: SyncRunOptions = {},
): Promise<SyncRunResult> {
  const cancelled = () => opts.shouldCancel?.() === true;
  if (cancelled()) return { status: 'blocked', reasons: ['cancelled'], message: 'Cancelled.' };

  // A plan derived from a partial report must never run, whatever the UI did.
  const reportBlockers = syncPlanBlockers(report);
  if (reportBlockers.length > 0) {
    return { status: 'blocked', reasons: reportBlockers, message: describeBlockers(report) };
  }

  const fresh = await compareForSync(report.leftRoot, report.rightRoot, {
    byContent: opts.byContent,
    recursive: report.recursive === true,
    opId: opts.opId,
  });
  const freshBlockers = syncPlanBlockers(fresh);
  if (freshBlockers.length > 0) {
    return { status: 'blocked', reasons: freshBlockers, message: describeBlockers(fresh) };
  }
  if (cancelled()) return { status: 'blocked', reasons: ['cancelled'], message: 'Cancelled.' };
  if (isPlanStale(report, fresh, { byContent: opts.byContent })) return { status: 'stale', fresh };

  const items: SyncItemResult[] = [];
  const skip = (item: SyncPlanItem, reason: string) =>
    items.push({ name: item.name, action: item.action, status: 'skipped', reason });

  // Classify what is allowed to run at all.
  const runnable: SyncPlanItem[] = [];
  for (const item of plan) {
    if (item.action === 'conflict') {
      skip(item, 'unresolved conflict — not executed');
    } else if (item.action === 'skip') {
      if (item.decision === 'skip') skip(item, 'skipped by user');
    } else if (!item.leftPath || !item.rightPath) {
      skip(item, 'missing path');
    } else if (
      (item.action === 'updateRight' || item.action === 'updateLeft') &&
      !opts.allowOverwrite
    ) {
      skip(item, 'overwrite not confirmed');
    } else {
      runnable.push(item);
    }
  }

  // Group copies into batches by (direction, destination dir).
  const batches = new Map<string, { toRight: boolean; dest: string; items: SyncPlanItem[] }>();
  for (const item of runnable.filter((i) => isCopy(i.action))) {
    const toRight = isToRight(item.action);
    const dest = destDirOf(item, toRight);
    const key = `${toRight ? 'R' : 'L'}|${dest}`;
    const batch = batches.get(key) ?? { toRight, dest, items: [] };
    batch.items.push(item);
    batches.set(key, batch);
  }
  const ordered = [...batches.values()].sort((a, b) => Number(b.toRight) - Number(a.toRight));

  const jobIds: string[] = [];
  let toRight = 0;
  let toLeft = 0;
  let failed = 0;
  let wasCancelled = false;
  for (const batch of ordered) {
    if (wasCancelled || cancelled()) {
      wasCancelled = true;
      for (const item of batch.items) skip(item, 'cancelled before this step');
      continue;
    }
    const sources = batch.items.map((i) => (batch.toRight ? i.leftPath! : i.rightPath!));
    try {
      const jobId = await enqueueCopy(sources, batch.dest, 'overwrite');
      jobIds.push(jobId);
      for (const item of batch.items)
        items.push({ name: item.name, action: item.action, status: 'queued', jobId });
      if (batch.toRight) toRight += batch.items.length;
      else toLeft += batch.items.length;
    } catch (e) {
      failed += batch.items.length;
      for (const item of batch.items) {
        items.push({
          name: item.name,
          action: item.action,
          status: 'failed',
          reason: errMessage(e),
        });
      }
    }
  }

  // Deletes only after every preceding copy was queued without error. When
  // that cannot be established (a failure, or a cancel), they are skipped.
  const deletes: string[] = [];
  for (const item of runnable.filter((i) => isDelete(i.action))) {
    if (wasCancelled || cancelled()) {
      wasCancelled = true;
      skip(item, 'cancelled before this step');
    } else if (failed > 0) {
      skip(item, 'delete skipped: a preceding copy failed');
    } else {
      deletes.push(item.action === 'deleteRight' ? item.rightPath! : item.leftPath!);
      items.push({ name: item.name, action: item.action, status: 'awaitingConfirm' });
    }
  }

  return {
    status: 'queued',
    jobIds,
    toRight,
    toLeft,
    deletes,
    conflicts: plan.filter((p) => p.action === 'conflict').length,
    failed,
    skipped: items.filter((i) => i.status === 'skipped').length,
    cancelled: wasCancelled,
    items,
  };
}

// ── Baseline settlement (FER-414 / H3) ─────────────────────────────────

const TERMINAL = new Set(['completed', 'failed', 'cancelled']);

/**
 * Map the terminal progress of each queue job to a per-item outcome. Conservative on purpose:
 * an item is `ok` only when its whole job `completed` with NO failure at all. A failed job,
 * a completed job that still lists failures (partial), or a cancelled job gives its items
 * `failed` / `cancelled`; a job that is not terminal (or unknown) gives NO outcome. Items
 * without a job (skipped, conflicts, deletes awaiting confirmation, enqueue failures) get none.
 * `reconcileBaseline` additionally requires the fresh compare to show the item identical.
 */
export function mapJobOutcomes(
  items: readonly SyncItemResult[],
  jobs: Readonly<Record<string, OperationProgress | undefined>>,
): Record<string, ItemOutcome> {
  const outcomes: Record<string, ItemOutcome> = {};
  for (const item of items) {
    if (item.status !== 'queued' || !item.jobId) continue;
    const job = jobs[item.jobId];
    if (!job || !TERMINAL.has(job.state)) continue;
    if (job.state === 'cancelled') outcomes[item.name] = 'cancelled';
    else if (job.state === 'completed' && !(job.failures?.length ?? 0) && !job.error) {
      outcomes[item.name] = 'ok';
    } else outcomes[item.name] = 'failed';
  }
  return outcomes;
}

export interface WaitOptions {
  /** Give up after this long (default 4 h); jobs still running then yield no outcome. */
  timeoutMs?: number;
  pollMs?: number;
  /** Test seams. */
  sleep?: (ms: number) => Promise<void>;
  now?: () => number;
}

/** Poll until every job is terminal or the timeout passes. Never throws. */
export async function waitForJobs(
  jobIds: readonly string[],
  read: (jobId: string) => OperationProgress | undefined,
  opts: WaitOptions = {},
): Promise<Record<string, OperationProgress | undefined>> {
  const timeoutMs = opts.timeoutMs ?? 4 * 60 * 60 * 1000;
  const pollMs = opts.pollMs ?? 500;
  const sleep = opts.sleep ?? ((ms: number) => new Promise<void>((r) => setTimeout(r, ms)));
  const now = opts.now ?? Date.now;
  const deadline = now() + timeoutMs;
  for (;;) {
    const snapshot: Record<string, OperationProgress | undefined> = {};
    let pending = 0;
    for (const id of jobIds) {
      const p = read(id);
      snapshot[id] = p;
      if (!p || !TERMINAL.has(p.state)) pending++;
    }
    if (pending === 0 || now() >= deadline) return snapshot;
    await sleep(pollMs);
  }
}

export type SettleResult =
  /** `contentSkipped` (FER-477b): files whose content read the settle compare skipped. */
  | { status: 'saved'; entries: number; contentSkipped?: number }
  /** Verified result equals what is already stored: nothing written. */
  | { status: 'unchanged'; entries: number; contentSkipped?: number }
  | {
      status: 'skipped';
      /** jobsNotFinished: timed out; notVerified: fresh compare partial; verifyFailed: compare error. */
      reason: 'jobsNotFinished' | 'notVerified' | 'verifyFailed';
    }
  | { status: 'saveFailed'; code: string };

export interface SettleInput {
  /** The snapshot the executed plan was built from. */
  report: CompareDirsReport;
  /** The EXECUTED plan with conflict decisions applied (what actually ran). */
  plan: readonly SyncPlanItem[];
  result: Extract<SyncRunResult, { status: 'queued' }>;
  /** Reads the live progress of a queue job (e.g. the ops store). */
  readJob: (jobId: string) => OperationProgress | undefined;
  wait?: WaitOptions;
  opId?: string;
}

// One settlement at a time per root pair, so two quick runs cannot interleave load/save.
const settleChains = new Map<string, Promise<unknown>>();

function pairKey(report: CompareDirsReport): string {
  return `${report.leftRoot}|${report.rightRoot}`.split('\\').join('/').toLowerCase();
}

/**
 * After a run: wait for the jobs, map their results to items, take a FRESH compare (by content,
 * roots validated again) and move the baseline only for items that finished `ok` AND are
 * verifiably identical. A failed/cancelled/partial/unfinished job, a conflict or a skipped item
 * leaves its entry exactly as it was; if nothing can be verified nothing is saved. The save is
 * atomic on the Rust side, so a crash here keeps the previous baseline.
 */
export function settleSyncRun(input: SettleInput): Promise<SettleResult> {
  const key = pairKey(input.report);
  const run = (settleChains.get(key) ?? Promise.resolve()).then(() => settleOnce(input));
  const tail = run.catch(() => undefined);
  settleChains.set(key, tail);
  void tail.then(() => {
    if (settleChains.get(key) === tail) settleChains.delete(key);
  });
  return run;
}

async function settleOnce(input: SettleInput): Promise<SettleResult> {
  const { report, plan, result } = input;
  const jobs = await waitForJobs(result.jobIds, input.readJob, input.wait);
  if (result.jobIds.some((id) => !jobs[id] || !TERMINAL.has(jobs[id]!.state))) {
    return { status: 'skipped', reason: 'jobsNotFinished' };
  }
  const outcomes = mapJobOutcomes(result.items, jobs);

  let fresh: CompareDirsReport;
  try {
    // Content mode: copies do not preserve mtime (FER-393), so only bytes prove "identical".
    fresh = await compareForSync(report.leftRoot, report.rightRoot, {
      byContent: true,
      recursive: report.recursive === true,
      opId: input.opId,
    });
  } catch {
    return { status: 'skipped', reason: 'verifyFailed' };
  }
  if (syncPlanBlockers(fresh).length > 0) return { status: 'skipped', reason: 'notVerified' };

  const { baseline: previous } = await loadSyncBaseline(fresh.leftRoot, fresh.rightRoot);
  const next = reconcileBaseline(previous, verifiedByContent(fresh), plan, outcomes);
  const entries = Object.keys(next.entries).length;
  const same =
    JSON.stringify(sortedEntries(previous ?? { ...next, entries: {} })) ===
      JSON.stringify(sortedEntries(next)) &&
    // FER-477b: fresh marks of a fully clean run must reach the store. Unchanged entries with
    // no new marks keep the stored file as is: its marks (if any) still date every entry,
    // and whatever changed since them is in the journal's change list.
    (next.journal === undefined ||
      JSON.stringify(previous?.journal ?? null) === JSON.stringify(next.journal));
  const skipped = fresh.incremental?.contentSkipped ?? 0;
  const extra = skipped > 0 ? { contentSkipped: skipped } : {};
  if (same) return { status: 'unchanged', entries, ...extra };
  try {
    await syncBaselineSave({ leftRoot: fresh.leftRoot, rightRoot: fresh.rightRoot }, next);
  } catch (e) {
    return { status: 'saveFailed', code: e instanceof SyncBaselineError ? e.code : 'io' };
  }
  return { status: 'saved', entries, ...extra };
}

/**
 * The settlement compare is by content, and the content phase only ever demotes a size-equal pair to
 * `contentDifferent`: a pair that is still `newerLeft`/`newerRight` has byte-identical content and differs only
 * in mtime (copies do not preserve it, FER-393). That is "verified identical" for the baseline, which records
 * both sides' facts so a later run sees both sides unchanged.
 */
function verifiedByContent(report: CompareDirsReport): CompareDirsReport {
  return {
    ...report,
    entries: report.entries.map((e) =>
      !e.isDir && (e.state === 'newerLeft' || e.state === 'newerRight')
        ? { ...e, state: 'same' }
        : e,
    ),
  };
}

function sortedEntries(b: SyncBaseline): [string, unknown][] {
  return Object.entries(b.entries).sort(([a], [c]) => (a < c ? -1 : a > c ? 1 : 0));
}
