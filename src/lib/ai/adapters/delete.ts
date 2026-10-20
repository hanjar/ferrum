// FER-576: `items.delete` adapter. Moves the items SELECTED in a pane to the Recycle Bin and nothing else.
//
// Safety properties (each pinned by a test):
//   - The target is only the pane selection (frozen into a manifest at prepare time). The model can name no
//     item, path or name; the intent has no field for it.
//   - It only ever calls the Recycle Bin command (`RecyclePort.deleteToRecycle`). There is no permanent
//     option anywhere in this module or in the port.
//   - An item that cannot go to the Recycle Bin (drive without one, network path, reserved name) refuses the
//     WHOLE plan at prepare time and again in the backend command: nothing is deleted, nothing falls back.
//   - The usual guards apply: the pane/tab/location must be unchanged, the backend approval ledger
//     fingerprints the items at preview and re-checks them right before the delete, and the Rust command
//     applies the same path gate and protected-folder rules as the manual Delete.
//   - Nothing here is auto-approvable: policy classifies it `mutating` and the card needs a click.
import type { IntentEnvelope } from '../contracts';
import type { CheckResult, ContextService } from '../context';
import type { ScopeOk, ScopeResolver } from '../contextResolver';
import { samePath } from '../paneNavigate';
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

/** Why an item cannot go to the Recycle Bin (stable codes from the backend). */
export type RecycleBlocker = 'network_or_unc' | 'reserved_name' | 'no_recycle_bin';

export interface RecycleCheckRow {
  path: string;
  ok: boolean;
  reason?: string | null;
  /** The REAL recursive size of the item (links are not followed); 0 when unknown. */
  bytes?: number;
}

export interface RecycleDeleteReport {
  requested: number;
  /** Items verified to be inside the Recycle Bin. */
  deleted: number;
  failed: Array<{ path: string; message: string }>;
  /** Items the shell deleted for good (or that left no trace in the bin); the batch stopped there. */
  bypassed?: string[];
  /** Items not attempted because a permanent delete was detected before them. */
  skipped?: number;
}

/** The only delete the chat can reach. There is deliberately no `permanent` parameter. */
export interface RecyclePort {
  /** Can each path go to the Recycle Bin? (read-only). */
  check(paths: readonly string[]): Promise<RecycleCheckRow[]>;
  /** Move the paths to the Recycle Bin. The backend refuses the whole batch when any item cannot go. */
  deleteToRecycle(paths: readonly string[]): Promise<RecycleDeleteReport>;
}

export interface DeleteAdapterDeps {
  resolver: Pick<ScopeResolver, 'resolve' | 'entries' | 'release'>;
  /** Snapshot vs live panes (location/filter/tab/swap). */
  ctx: Pick<ContextService, 'check'>;
  recycle: RecyclePort;
  /** Called after items really went to the Recycle Bin (refresh the panes). */
  onCompleted?(): void;
  selections?: SelectionHandleStore;
}

const baseName = (p: string): string =>
  p
    .replace(/[\\/]+$/, '')
    .split(/[\\/]/)
    .pop() ?? p;

export const DELETE_UNAVAILABLE_REASON =
  'Hapus ke Recycle Bin dari chat hanya berjalan di aplikasi desktop Ferrum.';

const REASON_TEXT: Record<string, string> = {
  network_or_unc: 'drive jaringan atau path UNC tidak punya Recycle Bin',
  reserved_name: 'nama khusus Windows (nul, con, ...) tidak bisa masuk Recycle Bin',
  no_recycle_bin: 'drive ini tidak punya Recycle Bin (mis. flashdisk)',
  recycle_disabled:
    'Recycle Bin drive ini dimatikan (pengaturan "hapus langsung", ukuran 0, atau kebijakan)',
  recycle_unknown: 'pengaturan Recycle Bin drive ini tidak bisa dibaca',
  mount_point: 'item berada di volume yang di-mount ke folder (bukan drive huruf)',
  redirected: 'folder item dialihkan lewat junction/symlink ke tempat lain',
  volume_unknown: 'volume item tidak dapat ditentukan',
  link_item: 'item adalah symlink/junction',
  size_unknown: 'ukuran item tidak dapat diukur lengkap',
  capacity_unknown: 'batas ukuran Recycle Bin drive ini tidak diketahui',
  too_big: 'ukuran melebihi kuota Recycle Bin drive ini, jadi Windows akan menghapusnya permanen',
};

function namesOf(rows: readonly { path: string }[], max = 3): string {
  const names = rows.slice(0, max).map((r) => `"${baseName(r.path)}"`);
  return rows.length > max
    ? `${names.join(', ')} dan ${rows.length - max} lainnya`
    : names.join(', ');
}

/** Frozen, adapter-private state of one prepared delete. */
export interface DeleteState {
  scope: ScopeOk;
  selection: SelectionHandle;
}

export function createDeleteAdapter(deps: DeleteAdapterDeps): CommandAdapter<DeleteState> {
  const selections = deps.selections ?? new SelectionHandleStore();
  const revalidate = (ctx: ResolvedContext, state: DeleteState): CheckResult =>
    // The approved manifest stays when the selection is edited afterwards (what the card showed is what is
    // deleted); a changed tab, location, filter or pane swap invalidates the run.
    deps.ctx.check(ctx.snapshot, state.scope.pane);

  return {
    id: 'items.delete',

    async prepare(
      envelope: IntentEnvelope,
      ctx: ResolvedContext,
    ): Promise<Result<AdapterPrepared<DeleteState>>> {
      const intent = envelope.intent;
      if (intent.capability !== 'items.delete') {
        return err('invalid_intent', 'Intent bukan penghapusan ke Recycle Bin.');
      }
      const scope = deps.resolver.resolve(ctx.snapshot, intent.sourcePane, { kind: 'selected' });
      if (!scope.ok) return err('resolve_failed', scope.error.message, scope.error.code);
      const entries = deps.resolver.entries(scope.handle);
      const paths = entries.map((e) => e.path);
      let rows: RecycleCheckRow[];
      try {
        rows = await deps.recycle.check(paths);
      } catch {
        deps.resolver.release(scope.handle);
        return err(
          'resolve_failed',
          'Kelayakan Recycle Bin tidak dapat diperiksa; tidak ada yang dihapus.',
        );
      }
      const blocked = rows.filter((r) => !r.ok);
      if (blocked.length > 0 || rows.length !== paths.length) {
        deps.resolver.release(scope.handle);
        const first = blocked[0]?.reason
          ? (REASON_TEXT[blocked[0].reason] ?? 'alasan tidak dikenal')
          : '';
        return err(
          'resolve_failed',
          `${blocked.length || paths.length} item tidak bisa dipindahkan ke Recycle Bin (${first || 'pemeriksaan tidak lengkap'}): ` +
            `${namesOf(blocked.length ? blocked : rows)}. Tidak ada yang dihapus: chat hanya memindahkan ke Recycle Bin dan menolak bila itu tidak terjamin.`,
        );
      }
      const selection = selections.create(scope, entries);
      const { files, folders } = scope.counts;
      const totalBytes = rows.reduce((sum, row) => sum + (row.bytes ?? 0), 0);
      const warnings = [
        'Item akan dipindahkan ke Recycle Bin (bukan hapus permanen) dan bisa dikembalikan dari sana.',
        'Windows tidak menanyakan lagi: jika Recycle Bin ternyata tidak menampung item, penghapusan dihentikan dan dilaporkan.',
      ];
      if (folders > 0) warnings.push(`${folders} folder ikut dipindahkan beserta seluruh isinya.`);
      return ok({
        operation: {
          kind: 'items.delete',
          sourceManifest: scope.handle,
          sourceDirectory: scope.sourceDirectory,
        },
        preview: {
          summary:
            `Pindahkan ${scope.counts.total} item (${files} file, ${folders} folder) dari ${scope.sourceDirectory} ` +
            `ke Recycle Bin: ${namesOf(entries, 5)}.`,
          itemCount: scope.counts.total,
          conflictCount: 0,
          totalBytes,
          warnings,
          selectionDigest: selection.digest,
          selectionIsOnly: (path) => paths.length === 1 && samePath(paths[0], path),
        },
        digestParts: [
          'recycle',
          scope.handle,
          scope.sourceDirectory,
          String(scope.contextRevision),
          String(scope.selectionRevision),
          selection.digest,
          ...paths,
        ],
        state: { scope, selection },
      });
    },

    async run(
      prepared: AdapterPrepared<DeleteState>,
      ctx: ResolvedContext,
    ): Promise<Result<AdapterRun>> {
      const { scope, selection } = prepared.state;
      const stale = revalidate(ctx, prepared.state);
      if (!stale.ok) {
        return err(
          'stale',
          `${stale.message} Rencana ditahan; susun ulang dan setujui lagi.`,
          stale.reason,
        );
      }
      const paths = selection.paths();
      let report: RecycleDeleteReport;
      try {
        report = await deps.recycle.deleteToRecycle(paths);
      } catch (error) {
        // The backend refuses a batch it cannot recycle BEFORE touching anything. Anything else (the IPC itself
        // broke) is an unknown outcome and is never retried.
        const code = (error as { code?: unknown } | null)?.code;
        if (code === 'unsupported' || code === 'invalidPath' || code === 'permissionDenied') {
          return err(
            'execution_failed',
            'Backend menolak penghapusan ke Recycle Bin; tidak ada yang dihapus.',
            'recycleRefused',
          );
        }
        throw error;
      }
      if (report.deleted > 0 || (report.bypassed?.length ?? 0) > 0) deps.onCompleted?.();
      const failed = report.failed.length;
      const bypassed = report.bypassed ?? [];
      if (bypassed.length > 0) {
        // The shell deleted something for good although everything promised a Recycle Bin. Never a success.
        const loud =
          `PERINGATAN: ${namesOf(bypassed.map((path) => ({ path })))} TIDAK masuk Recycle Bin dan sudah terhapus ` +
          `permanen oleh Windows. Penghapusan dihentikan; ${report.skipped ?? 0} item sisanya tidak disentuh.`;
        if (report.deleted === 0) return err('execution_failed', loud, 'recycleBypassed');
        return ok({
          jobIds: [],
          state: 'completed',
          completed: report.deleted,
          conflictsSkipped: 0,
          failedToQueue: bypassed.length + failed + (report.skipped ?? 0),
          failureNote: `${loud} ${report.deleted} item sebelumnya masuk Recycle Bin.`,
          notes: [],
        });
      }
      if (report.deleted === 0 && failed > 0) {
        return err(
          'execution_failed',
          `Tidak ada item yang dapat dipindahkan ke Recycle Bin (${failed} gagal). Tidak ada yang dihapus.`,
          'io',
        );
      }
      return ok({
        jobIds: [],
        state: 'completed',
        completed: report.deleted,
        conflictsSkipped: 0,
        ...(failed > 0
          ? {
              failedToQueue: failed,
              failureNote:
                `${report.deleted} item masuk Recycle Bin, ${failed} gagal dan tetap di tempatnya` +
                `: ${namesOf(report.failed)}.`,
            }
          : {}),
        notes: [`${scope.counts.total} item diminta; hanya ke Recycle Bin.`],
      });
    },

    dispose(prepared) {
      deps.resolver.release(prepared.state.scope.handle);
      selections.release(prepared.state.selection.handleId);
    },
  };
}
