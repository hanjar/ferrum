// AI-11 / FER-215..217: `directories.sync` adapter (chat "sinkronkan kedua panel dua arah").
//
// No reconciliation lives here. The adapter only (1) resolves the two roots of the snapshot, (2) asks the
// SAME service the manual Synchronize panel uses for a dry-run (`buildSyncDryRun`, mode `both`, which loads
// the stored baseline of the root pair), (3) turns the plan into ONE preview for ONE approval, and (4) after
// the approval runs the plan through `runSyncPlan` (existing operation queue) and hands the finished jobs to
// `settleSyncRun`, the only path that is allowed to move the baseline.
//
// Rules enforced here (docs/ai/sync-chat.md):
//   - a plan that contains a `delete`, a truncated / cancelled / erroneous compare, or nothing executable is
//     never offered for approval;
//   - conflicts are listed, never decided and never executed (chat has no "keep left / keep right");
//   - the approval is bound to the exact dry-run (item facts) and both roots; a changed folder, a changed
//     pane or a stale snapshot means a NEW plan (`stale`), nothing is written;
//   - the model never sees names or paths: only the local UI does (summary / items / conflicts).
import type { IntentEnvelope, ResolvedPlan } from '../contracts';
import type { ContextService } from '../context';
import type { ContextFlow } from '../contextFlow';
import type { ConflictView, PlanItemRow } from '../chatStore.svelte';
import type { SyncGuardScope } from '../backendGuard';
import { FsError, type OperationProgress } from '../../ipc/types';
import {
  buildSyncDryRun,
  runSyncPlan,
  settleSyncRun,
  type SettleResult,
  type SyncDryRun,
} from '../../syncService';
import { planFacts, syncPlanBlockers, type SyncPlanItem } from '../../syncPlan';
import {
  digestOf,
  err,
  ok,
  type AdapterPrepared,
  type AdapterRun,
  type CommandAdapter,
  type ResolvedContext,
  type Result,
} from '../commands';

/** Newest preview entries kept for the item list / backend guard scope (bounded; old plans are evicted). */
const MAX_PREVIEWS = 16;

const isCopy = (a: SyncPlanItem['action']) =>
  a === 'copyToRight' || a === 'copyToLeft' || a === 'updateRight' || a === 'updateLeft';
const isUpdate = (a: SyncPlanItem['action']) => a === 'updateRight' || a === 'updateLeft';
const toRight = (a: SyncPlanItem['action']) => a === 'copyToRight' || a === 'updateRight';

function parentDir(path: string): string {
  const i = Math.max(path.lastIndexOf('/'), path.lastIndexOf('\\'));
  return i > 0 ? path.slice(0, i) : path;
}

/** The frozen, local description of one prepared sync plan. */
export interface SyncPreview {
  leftRoot: string;
  rightRoot: string;
  /** Actionable items only (copies / overwrites / conflicts), in report order. */
  items: readonly SyncPlanItem[];
  scope: SyncGuardScope;
}

/** Local registry of recent previews: feeds the plan card (items, conflicts) and the backend guard scope. */
export class SyncPreviewStore {
  private readonly entries = new Map<string, SyncPreview>();

  put(key: string, preview: SyncPreview): void {
    this.entries.set(key, preview);
    while (this.entries.size > MAX_PREVIEWS) {
      const oldest = this.entries.keys().next().value;
      if (oldest === undefined) break;
      this.entries.delete(oldest);
    }
  }

  private of(plan: ResolvedPlan): SyncPreview | undefined {
    return plan.operation.kind === 'directories.sync'
      ? this.entries.get(plan.operation.leftManifest)
      : undefined;
  }

  /** What the backend must fingerprint for this plan (undefined = unknown plan: the guard fails closed). */
  scopeOf(plan: ResolvedPlan): SyncGuardScope | undefined {
    return this.of(plan)?.scope;
  }

  /** Conflicts of the plan for the card; chat only LISTS them (they are skipped, never decided). */
  conflictsOf(plan: ResolvedPlan): ConflictView[] {
    return (this.of(plan)?.items ?? [])
      .filter((i) => i.action === 'conflict')
      .map((i) => ({ itemRef: i.name, label: i.name, kind: i.reason ?? 'conflict' }));
  }

  /** One page of the actionable items (virtualised in the UI). */
  page(plan: ResolvedPlan, offset: number, limit: number): { rows: PlanItemRow[]; total: number } {
    const items = this.of(plan)?.items ?? [];
    return {
      total: items.length,
      rows: items.slice(offset, offset + limit).map((i) => ({
        ref: i.name,
        label: i.name,
        detail: describeAction(i),
      })),
    };
  }
}

function describeAction(item: SyncPlanItem): string {
  switch (item.action) {
    case 'copyToRight':
      return 'kiri → kanan';
    case 'copyToLeft':
      return 'kanan → kiri';
    case 'updateRight':
      return 'kiri → kanan (menimpa)';
    case 'updateLeft':
      return 'kanan → kiri (menimpa)';
    case 'conflict':
      return `konflik (${item.reason ?? 'berbeda'}) — dilewati`;
    default:
      return item.action;
  }
}

/**
 * What the backend guard must see for the executable part of the plan: both roots, each source, each
 * existing target that will be overwritten, the directories that hold them (a file added to one of them
 * changes it), and the targets of NEW copies, which must still not exist when the plan is authorized.
 */
export function guardScopeOf(
  leftRoot: string,
  rightRoot: string,
  items: readonly SyncPlanItem[],
): SyncGuardScope {
  const present = new Set<string>([leftRoot, rightRoot]);
  const absent = new Set<string>();
  for (const item of items) {
    if (!isCopy(item.action) || !item.leftPath || !item.rightPath) continue;
    const right = toRight(item.action);
    const source = right ? item.leftPath : item.rightPath;
    const target = right ? item.rightPath : item.leftPath;
    present.add(source);
    present.add(parentDir(source));
    present.add(parentDir(target));
    if (isUpdate(item.action)) present.add(target);
    else absent.add(target);
  }
  return { leftRoot, rightRoot, sourcePaths: [...present], absentPaths: [...absent] };
}

export interface SyncAdapterDeps {
  flow: Pick<ContextFlow, 'resolveSync'>;
  ctx: Pick<ContextService, 'check'>;
  /**
   * Reads the live progress of a queue job (the executor's queue port). Used ONLY by `settleSyncRun`, which
   * waits for the jobs and then moves the baseline for verified items; there is no other baseline writer.
   */
  readJob(jobId: string): OperationProgress | undefined;
  /** Compare contents instead of size+mtime (default false, the same default as the manual panel). */
  byContent?: boolean;
  /** Local preview registry shared with the plan card and the backend guard (defaults to a private one). */
  store?: SyncPreviewStore;
  /** Called with the baseline settlement of a run (UI / diagnostics); never carries names or paths. */
  onSettled?(result: SettleResult, previewKey?: string): void;
  /** Called after the copies were queued (e.g. refresh the panes when the jobs end). */
  onQueued?(): void;
  /** Test seam; defaults to the real `settleSyncRun`. */
  settle?: typeof settleSyncRun;
}

export interface SyncState {
  leftRoot: string;
  rightRoot: string;
  dryRun: SyncDryRun;
  /** Key of the local preview entry (the plan's `leftManifest`); lets the chat card find its run. */
  previewKey: string;
}

let previewSeq = 0;

function baselineNote(status: SyncDryRun['baselineStatus']): string {
  if (status === 'trusted') return 'Baseline tersimpan dipakai.';
  if (status === 'loadFailed') return 'Baseline tidak terbaca; diperlakukan sebagai run pertama.';
  return 'Run pertama untuk pasangan folder ini (belum ada baseline tepercaya).';
}

export function createSyncAdapter(deps: SyncAdapterDeps): CommandAdapter<SyncState> {
  const store = deps.store ?? new SyncPreviewStore();
  const byContent = deps.byContent ?? false;

  return {
    id: 'directories.sync',

    async prepare(
      envelope: IntentEnvelope,
      ctx: ResolvedContext,
    ): Promise<Result<AdapterPrepared<SyncState>>> {
      const roots = deps.flow.resolveSync(ctx.snapshot, envelope);
      if (!roots.ok) return err('resolve_failed', roots.error.message, roots.error.code);
      let dryRun: SyncDryRun;
      try {
        // Read-only: validate roots (same / nested / alias), compare, load baseline, classify.
        // Same defaults as the manual panel (recursive); nothing is enqueued.
        dryRun = await buildSyncDryRun(
          roots.leftRoot,
          roots.rightRoot,
          { direction: 'both' },
          { byContent },
        );
      } catch (e) {
        if (e instanceof FsError && (e.code === 'invalidPath' || e.code === 'notADirectory')) {
          return err(
            'resolve_failed',
            'Folder kiri dan kanan sama, saling bersarang, atau menunjuk lokasi yang sama lewat alias; sinkronisasi tidak dapat disusun.',
            'invalid_roots',
          );
        }
        return err(
          'resolve_failed',
          'Perbandingan folder gagal; tidak ada yang dijalankan.',
          'compare_failed',
        );
      }
      const blockers = syncPlanBlockers(dryRun.report);
      if (blockers.length > 0) {
        const why = blockers.includes('truncated')
          ? 'Perbandingan terpotong (terlalu banyak entri)'
          : blockers.includes('cancelled')
            ? 'Perbandingan dibatalkan'
            : 'Sebagian entri tidak dapat dibaca';
        return err(
          'unsupported_plan',
          `${why}; sinkronisasi tidak dapat disetujui. Tidak ada rencana yang dibuat.`,
          blockers[0],
        );
      }
      const s = dryRun.summary;
      if (s.deletes > 0) {
        return err(
          'unsupported_plan',
          'Rencana mengandung penghapusan; tidak didukung dari chat.',
          'deletes',
        );
      }
      const copies = s.toRight + s.toLeft;
      if (copies === 0) {
        return err(
          'unsupported_plan',
          s.conflicts > 0
            ? `Tidak ada yang bisa disalin otomatis; ${s.conflicts} konflik harus diputuskan di panel Synchronize (chat tidak memutuskan konflik).`
            : 'Kedua folder sudah sama; tidak ada yang perlu disinkronkan.',
          s.conflicts > 0 ? 'only_conflicts' : 'nothing_to_sync',
        );
      }

      const items = dryRun.plan.filter((p) => isCopy(p.action) || p.action === 'conflict');
      const sizes = new Map(dryRun.report.entries.map((e) => [e.name, e]));
      let totalBytes = 0;
      let bytesKnown = true;
      for (const item of items) {
        if (!isCopy(item.action)) continue;
        const entry = sizes.get(item.name);
        if (!entry || item.isDir) bytesKnown = false;
        else totalBytes += toRight(item.action) ? entry.leftSize : entry.rightSize;
      }
      const updRight = dryRun.plan.filter((p) => p.action === 'updateRight').length;
      const updLeft = dryRun.plan.filter((p) => p.action === 'updateLeft').length;

      const warnings: string[] = [baselineNote(dryRun.baselineStatus)];
      if (updRight + updLeft > 0) {
        warnings.push(
          `${updRight + updLeft} file yang sudah ada akan ditimpa (tercantum dalam persetujuan ini).`,
        );
      }
      if (s.conflicts > 0) warnings.push(`${s.conflicts} konflik dilewati dan tidak diubah.`);

      const facts = digestOf([
        ...planFacts(dryRun.report, true),
        ...dryRun.plan.map((p) => `${p.name}|${p.action}`),
      ]);
      const key = `${facts}_${(++previewSeq).toString(36)}`;
      const scope = guardScopeOf(roots.leftRoot, roots.rightRoot, items);
      store.put(`cmp_${key}_l`, {
        leftRoot: roots.leftRoot,
        rightRoot: roots.rightRoot,
        items,
        scope,
      });
      return ok({
        operation: {
          kind: 'directories.sync',
          leftRoot: roots.leftRoot,
          rightRoot: roots.rightRoot,
          leftManifest: `cmp_${key}_l`,
          rightManifest: `cmp_${key}_r`,
        },
        preview: {
          summary:
            `Sinkronisasi dua arah: kiri ${roots.leftRoot} ↔ kanan ${roots.rightRoot}. ` +
            `Kiri → kanan: ${s.toRight} item (${updRight} menimpa). ` +
            `Kanan → kiri: ${s.toLeft} item (${updLeft} menimpa). ` +
            `Konflik: ${s.conflicts} (dilewati). Hapus: ${s.deletes}. ` +
            `Dilewati/identik: ${s.skipped}.`,
          itemCount: copies + s.conflicts,
          conflictCount: s.conflicts,
          totalBytes: bytesKnown ? totalBytes : 0,
          warnings,
        },
        digestParts: [facts, roots.leftRoot, roots.rightRoot, dryRun.baselineStatus],
        state: {
          leftRoot: roots.leftRoot,
          rightRoot: roots.rightRoot,
          dryRun,
          previewKey: `cmp_${key}_l`,
        },
      });
    },

    async run(
      prepared: AdapterPrepared<SyncState>,
      ctx: ResolvedContext,
    ): Promise<Result<AdapterRun>> {
      const { dryRun } = prepared.state;
      for (const pane of ['left', 'right'] as const) {
        const c = deps.ctx.check(ctx.snapshot, pane);
        if (!c.ok) {
          return err(
            'stale',
            `${c.message} Rencana ditahan; susun ulang dan setujui lagi.`,
            c.reason,
          );
        }
      }
      // Same service as the manual panel: roots validated again, FRESH compare, stale check, queue.
      // `allowOverwrite`: the approved preview listed every overwrite; deletes are impossible (rejected
      // in prepare) and conflicts are never executed by `runSyncPlan`.
      const result = await runSyncPlan(dryRun.report, dryRun.plan, {
        byContent,
        allowOverwrite: true,
      });
      if (result.status === 'stale') {
        return err(
          'stale',
          'Isi folder berubah sejak pratinjau; rencana baru perlu disetujui.',
          'stale_plan',
        );
      }
      if (result.status === 'blocked') {
        return err(
          'unsupported_plan',
          'Sinkronisasi diblokir (perbandingan ulang tidak lengkap atau dibatalkan); tidak ada yang ditulis.',
          result.reasons[0] ?? 'blocked',
        );
      }
      if (result.jobIds.length === 0) {
        return err(
          'resolve_failed',
          'Penyalinan gagal masuk antrean; tidak ada yang dijalankan.',
          'enqueue_failed',
        );
      }
      deps.onQueued?.();
      // Baseline: only the verified settlement may move it. Background: the chat card tracks the jobs.
      const settle = deps.settle ?? settleSyncRun;
      void Promise.resolve()
        .then(() =>
          settle({ report: dryRun.report, plan: dryRun.plan, result, readJob: deps.readJob }),
        )
        .then((settled) => deps.onSettled?.(settled, prepared.state.previewKey))
        .catch(() =>
          deps.onSettled?.(
            { status: 'skipped', reason: 'verifyFailed' },
            prepared.state.previewKey,
          ),
        );
      const notes = ['Penyalinan masuk antrean; hasil akhir mengikuti status job.'];
      if (result.failed > 0) notes.push(`${result.failed} item gagal masuk antrean.`);
      return ok({
        jobIds: result.jobIds,
        state: 'queued',
        completed: 0,
        conflictsSkipped: result.conflicts,
        failedToQueue: result.failed,
        notes,
      });
    },
  };
}
