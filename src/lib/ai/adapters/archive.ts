// AI-02 / FER-179: `archive.create` adapter. Resolution is the context lane's
// (ContextFlow); the work itself is the SAME Pack service the manual dialog uses
// (`runPack` -> existing `archive_create`). No ZIP engine lives here.
import type { ArchiveIntentFormat, ArchiveIntentLevel, IntentEnvelope } from '../contracts';
import type { CheckResult, ContextService, PaneName } from '../context';
import type { ContextFlow } from '../contextFlow';
import type { ScopeOk, ScopeResolver } from '../contextResolver';
import type { TargetOk } from '../contextTarget';
import { parseActivityLocation, type ActivityLocation } from '../activityQuery';
import { FsError } from '../../ipc/types';
import type { ArchiveCompressionLevel } from '../../ipc/archive';
import {
  measurePack,
  packFailureCause,
  runPack,
  type PackFormat,
  type PackMeasure,
} from '../../packService';
import { SelectionHandleStore, type SelectionHandle } from '../commandsSelection';
import {
  err,
  ok,
  type AdapterPrepared,
  type AdapterRun,
  type CommandAdapter,
  type ResolvedContext,
  type Result,
} from '../commands';

export interface ArchiveAdapterDeps {
  flow: Pick<ContextFlow, 'resolveArchive' | 'resolveArchiveEntity'>;
  resolver: Pick<ScopeResolver, 'entries' | 'release'>;
  /** Snapshot vs live panes (location/filter/tab/swap). */
  ctx: Pick<ContextService, 'check'>;
  /** Live collision check at execute time (the output name must still be free). */
  outputExists(pane: PaneName, name: string): boolean;
  /** Called after the archive was really created (e.g. refresh panes). */
  onCompleted?(): void;
  /** Local selection handles (FER-182); defaults to a private store. */
  selections?: SelectionHandleStore;
  /**
   * Is the Pack service reachable (FER-206)? `false` closes the capability in the registry, so the
   * provider is not offered it and prepare/execute are blocked. Defaults to available.
   */
  packAvailable?: () => boolean;
  /** Read-only size estimate for the preview (FER-165); defaults to the occupied-space walk. */
  measure?: (paths: readonly string[], targetDirectory: string) => Promise<PackMeasure | undefined>;
  /**
   * FER-433 (ACT-08 R7 v2): read-only lookup of one history entity (`activity_chat_locate`). Absent =
   * the `entity` selector is refused. Paths stay in the renderer.
   */
  locateEntity?: (entityId: string) => Promise<unknown>;
}

const TRAILING_SEP = /[\\/]+$/;
const norm = (p: string): string => p.replace(TRAILING_SEP, '').replace(/\//g, '\\').toLowerCase();
const baseName = (p: string): string => p.replace(TRAILING_SEP, '').split(/[\\/]/).pop() ?? p;

/** Is `child` strictly inside `parent` (Windows paths: case-insensitive, either separator)? */
function isInside(child: string, parent: string): boolean {
  const c = norm(child);
  const p = norm(parent);
  return c.length > p.length && c.startsWith(p) && c[p.length] === '\\';
}

function formatBytes(bytes: number): string {
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit++;
  }
  return `${unit === 0 ? value : value.toFixed(1)} ${units[unit]}`;
}

/** Preview facts derived from the size walk; the card shows `0` as "size unknown". */
function measureFacts(
  m: PackMeasure | undefined,
  label: string,
): { totalBytes: number; warnings: string[] } {
  if (!m) return { totalBytes: 0, warnings: [] };
  const warnings: string[] = [];
  if (m.unreadable.length > 0) {
    const first = baseName(m.unreadable[0]);
    const more = m.unreadable.length > 1 ? ` dan ${m.unreadable.length - 1} lainnya` : '';
    warnings.push(
      `${first}${more} tidak dapat dibaca; ${label} akan gagal tanpa menulis apa pun bila item itu tetap tidak terbaca.`,
    );
  }
  if (m.freeBytes !== undefined && m.bytes > m.freeBytes) {
    warnings.push(
      `Ruang kosong di drive tujuan (${formatBytes(m.freeBytes)}) lebih kecil dari ukuran sumber (${formatBytes(m.bytes)}); ${label} bisa gagal karena disk penuh.`,
    );
  }
  return { totalBytes: m.complete ? m.bytes : 0, warnings };
}

/** User-facing cause of a Pack failure (Indonesian); never the engine's raw message. */
function packFailureMessage(error: FsError, output: string): string {
  const tail = `${output} tidak dibuat; tidak ada file yang ditulis di tujuan.`;
  switch (packFailureCause(error)) {
    case 'sourceUnreadable':
      return `Item sumber ${error.path ? `"${baseName(error.path)}" ` : ''}tidak dapat dibaca (sedang dipakai program lain atau akses ditolak). ${tail}`;
    case 'noSpace':
      return `Ruang disk di drive tujuan tidak cukup. ${tail}`;
    case 'permissionDenied':
      return `Akses ditolak. ${tail}`;
    default:
      return `Engine arsip gagal (${error.code}). ${tail}`;
  }
}

/**
 * Cause codes for `execution_failed` (agreed with FER-163): `sourceUnreadable` and `diskFull` come from the
 * engine message contract, anything else is the IPC error code (permissionDenied, notFound, io, ...).
 */
export function engineCause(error: FsError): string {
  const cause = packFailureCause(error);
  if (cause === 'sourceUnreadable') return 'sourceUnreadable';
  if (cause === 'noSpace') return 'diskFull';
  return error.code;
}

/** FER-576: the model's format name -> the Pack service's. */
export function packFormatOf(format: ArchiveIntentFormat): PackFormat {
  return format === '7z' ? 'sevenz' : 'zip';
}

/** FER-576: chat level names -> engine levels. `maximum` and `ultra` are both the engine's strongest. */
export function packLevelOf(
  level: ArchiveIntentLevel | undefined,
): ArchiveCompressionLevel | undefined {
  switch (level) {
    case undefined:
      return undefined;
    case 'maximum':
    case 'ultra':
      return 'max';
    default:
      return level;
  }
}

/** Frozen, adapter-private state of one prepared archive. */
export interface ArchiveState {
  format: ArchiveIntentFormat;
  level?: ArchiveIntentLevel;
  scope: ScopeOk;
  target: TargetOk;
  /** Frozen manifest (FER-182); live selection changes cannot alter it. */
  selection: SelectionHandle;
}

export function createArchiveAdapter(deps: ArchiveAdapterDeps): CommandAdapter<ArchiveState> {
  const selections = deps.selections ?? new SelectionHandleStore();
  const revalidate = (ctx: ResolvedContext, state: ArchiveState): CheckResult => {
    // Selection edits after approval do NOT invalidate the run (spec 5: the
    // approved manifest stays); pane/tab/location/filter changes do.
    for (const pane of new Set<PaneName>([state.scope.pane, state.target.targetPane])) {
      const c = deps.ctx.check(ctx.snapshot, pane);
      if (!c.ok) return c;
    }
    return { ok: true };
  };

  return {
    id: 'archive.create',

    async prepare(
      envelope: IntentEnvelope,
      ctx: ResolvedContext,
    ): Promise<Result<AdapterPrepared<ArchiveState>>> {
      let flow: ReturnType<typeof deps.flow.resolveArchive>;
      if (
        envelope.intent.capability === 'archive.create' &&
        envelope.intent.selector.kind === 'entity'
      ) {
        // The entity is resolved HERE (prepare time), never earlier: a moved/hidden/offline item or a
        // paused history gives no location and nothing is planned.
        let location: ActivityLocation | null = null;
        try {
          const raw = deps.locateEntity
            ? await deps.locateEntity(envelope.intent.selector.entityId)
            : null;
          location = parseActivityLocation(raw);
        } catch {
          location = null;
        }
        if (!location) {
          return err(
            'resolve_failed',
            'Item riwayat ini tidak dapat ditemukan lagi (dipindah, disembunyikan, offline, atau riwayat dijeda). Tidak ada yang dijalankan.',
          );
        }
        flow = deps.flow.resolveArchiveEntity(ctx.snapshot, envelope, location);
      } else {
        flow = deps.flow.resolveArchive(ctx.snapshot, envelope);
      }
      if (!flow.ok) return err('resolve_failed', flow.error.message, flow.error.code);
      const { scope, target } = flow;
      const intent = envelope.intent;
      const format: ArchiveIntentFormat =
        intent.capability === 'archive.create' ? intent.format : 'zip';
      // `ultra` and `maximum` run the SAME engine level: one name, so the digest never implies a difference.
      const rawLevel = intent.capability === 'archive.create' ? intent.level : undefined;
      const level: ArchiveIntentLevel | undefined = rawLevel === 'ultra' ? 'maximum' : rawLevel;
      const label = format === '7z' ? '7z' : 'ZIP';
      const selection = selections.create(scope, deps.resolver.entries(scope.handle));
      const paths = selection.paths();
      // Read-only: the occupied-space walk never writes. A failed estimate only means "size unknown".
      const measured = await (deps.measure ?? measurePack)(paths, target.targetDirectory).catch(
        () => undefined,
      );
      const facts = measureFacts(measured, label);
      const warnings = [...target.warnings, ...facts.warnings];
      if (paths.some((p) => isInside(target.outputPath, p))) {
        // The engine drops its own output from the input (FER-205); say so instead of surprising the user.
        warnings.push(
          `${target.finalName} disimpan di dalam folder yang diarsipkan; ${label} itu sendiri tidak ikut dimasukkan.`,
        );
      }
      return ok({
        operation: {
          kind: 'archive.create',
          sourceManifest: scope.handle,
          targetDirectory: target.targetDirectory,
          outputName: target.finalName,
        },
        preview: {
          summary: flow.explanation,
          itemCount: scope.counts.total,
          conflictCount: 0, // a taken name is renamed to a unique one, never overwritten
          totalBytes: facts.totalBytes,
          outputPath: target.outputPath,
          warnings,
        },
        digestParts: [
          scope.handle,
          target.outputPath,
          format,
          level ?? '',
          String(scope.contextRevision),
          String(scope.selectionRevision),
          selection.digest,
          ...paths,
        ],
        state: { format, level, scope, target, selection },
      });
    },

    async run(
      prepared: AdapterPrepared<ArchiveState>,
      ctx: ResolvedContext,
    ): Promise<Result<AdapterRun>> {
      const { format, level, scope, target, selection } = prepared.state;
      const stale = revalidate(ctx, prepared.state);
      if (!stale.ok) {
        return err(
          'stale',
          `${stale.message} Rencana ditahan; susun ulang dan setujui lagi.`,
          stale.reason,
        );
      }
      if (deps.outputExists(target.targetPane, target.finalName)) {
        return err(
          'target_changed',
          `${target.finalName} kini sudah ada di tujuan. Tidak ditimpa; susun ulang rencana.`,
        );
      }
      // The ONE archive path: same service as the manual Pack dialog.
      // `skip`: the backend itself refuses to replace an existing file, so a name that appeared after
      // the check above (or is invisible in the panel listing) is still never overwritten.
      let result;
      try {
        result = await runPack({
          paths: selection.paths(),
          target: target.outputPath,
          format: packFormatOf(format),
          conflict: 'skip',
          level: packLevelOf(level),
        });
      } catch (error) {
        if (error instanceof FsError && error.code === 'alreadyExists') {
          return err(
            'target_changed',
            `${target.finalName} sudah ada di tujuan. Tidak ditimpa; susun ulang rencana.`,
          );
        }
        if (error instanceof FsError && error.code !== 'cancelled') {
          // Contract with the executor (FER-163/LIMIT-5): `execution_failed` WITH a cause = the engine answered
          // with a failure. archive_create writes to a staging file and publishes only on success, so nothing
          // was written at the destination; the executor turns this into `failed` + cause. The message is
          // safe (no engine text, no full path); without a cause the outcome stays unknown.
          return err(
            'execution_failed',
            packFailureMessage(error, target.finalName),
            engineCause(error),
          );
        }
        throw error; // not an engine answer (e.g. the IPC itself broke): outcome unknown
      }
      deps.onCompleted?.();
      return ok({
        jobIds: [], // archive_create is awaited by the backend, not a queue job
        state: 'completed',
        completed: scope.counts.total,
        conflictsSkipped: 0,
        outputPath: result.outputPath,
        notes: ['Pack tidak berjalan lewat antrean operasi: tanpa job ID, progress, atau cancel.'],
      });
    },

    dispose(prepared) {
      deps.resolver.release(prepared.state.scope.handle);
      selections.release(prepared.state.selection.handleId);
    },
  };
}
