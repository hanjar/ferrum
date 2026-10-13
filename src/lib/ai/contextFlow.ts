// AI-03 / FER-186: orkestrasi intent -> scope + target lokal, dengan penjaga
// respons terlambat. Semua target berasal dari snapshot; tidak pernah dari panel
// "saat ini". Perubahan konteks menghasilkan error stale, bukan retarget diam-diam.
import type { IntentEnvelope, PaneSnapshot } from './contracts';
import type { CheckResult, ContextService, PaneName } from './context';
import type { ScopeOk, ScopeResolver } from './contextResolver';
import { resolveArchiveTarget, type TargetOk } from './contextTarget';
import type { ActivityLocation } from './activityQuery';
import { entityArchiveName } from './entitySelector';

export type FlowErrorCode =
  | 'request_mismatch'
  | 'wrong_capability'
  | 'cancelled'
  | 'same_root'
  | 'nested_roots'
  | 'empty_selection'
  | 'empty_scope'
  | 'unsupported_view'
  | 'stale_context'
  | 'pane_unavailable'
  | 'invalid_name'
  | 'target_read_only';

export interface FlowError {
  code: FlowErrorCode;
  message: string;
}
export type ArchiveFlowResult =
  | { ok: true; scope: ScopeOk; target: TargetOk; explanation: string }
  | { ok: false; error: FlowError };
export type SyncFlowResult =
  | { ok: true; leftRoot: string; rightRoot: string; explanation: string }
  | { ok: false; error: FlowError };

const norm = (p: string): string => p.replace(/\\/g, '/').replace(/\/+$/, '').toLowerCase();
const nestedIn = (a: string, b: string): boolean => a === b || a.startsWith(`${b}/`);

export class ContextFlow {
  constructor(
    private readonly ctx: ContextService,
    private readonly resolver: ScopeResolver,
  ) {}

  resolveArchive(
    snapshot: PaneSnapshot,
    envelope: IntentEnvelope,
    opts: { cancelled?: () => boolean } = {},
  ): ArchiveFlowResult {
    const fail = (code: FlowErrorCode, message: string): ArchiveFlowResult => ({
      ok: false,
      error: { code, message },
    });
    if (envelope.requestId !== snapshot.requestId) {
      return fail('request_mismatch', 'Respons tidak cocok dengan permintaan aktif; diabaikan.');
    }
    const intent = envelope.intent;
    if (intent.capability !== 'archive.create')
      return fail('wrong_capability', 'Intent bukan pembuatan arsip.');
    if (opts.cancelled?.())
      return fail('cancelled', 'Permintaan dibatalkan; respons terlambat diabaikan.');

    if (intent.selector.kind === 'entity')
      return fail('wrong_capability', 'Selector riwayat diselesaikan oleh resolveArchiveEntity.');

    const scope = this.resolver.resolve(snapshot, intent.sourcePane, intent.selector);
    if (!scope.ok) return { ok: false, error: scope.error };
    const target = resolveArchiveTarget(this.ctx, snapshot, {
      sourcePane: intent.sourcePane,
      target: intent.target,
      outputName: intent.outputName,
      scope,
      manifest: this.resolver.entries(scope.handle),
    });
    if (!target.ok) {
      this.resolver.release(scope.handle);
      return { ok: false, error: target.error };
    }
    return { ok: true, scope, target, explanation: target.previewText };
  }

  /**
   * FER-433 (ACT-08 R7 v2): arsip satu item riwayat. `location` sudah dibaca lokal oleh pemanggil
   * (`activity_chat_locate`); selector envelope harus `entity`. Target selalu pane (bukan sourceDirectory:
   * item riwayat tidak berada di panel).
   */
  resolveArchiveEntity(
    snapshot: PaneSnapshot,
    envelope: IntentEnvelope,
    location: ActivityLocation,
    opts: { cancelled?: () => boolean } = {},
  ): ArchiveFlowResult {
    const fail = (code: FlowErrorCode, message: string): ArchiveFlowResult => ({
      ok: false,
      error: { code, message },
    });
    if (envelope.requestId !== snapshot.requestId) {
      return fail('request_mismatch', 'Respons tidak cocok dengan permintaan aktif; diabaikan.');
    }
    const intent = envelope.intent;
    if (intent.capability !== 'archive.create' || intent.selector.kind !== 'entity')
      return fail('wrong_capability', 'Intent bukan pembuatan arsip item riwayat.');
    if (opts.cancelled?.())
      return fail('cancelled', 'Permintaan dibatalkan; respons terlambat diabaikan.');

    const scope = this.resolver.resolveEntity(
      snapshot,
      intent.sourcePane,
      intent.selector.entityId,
      location,
    );
    if (!scope.ok) return { ok: false, error: scope.error };
    // `sourceDirectory` target tidak bermakna untuk entity: dipaksa ke pane aktif.
    const target = resolveArchiveTarget(this.ctx, snapshot, {
      sourcePane: intent.sourcePane,
      target:
        intent.target.kind === 'pane' ? intent.target : { kind: 'pane', pane: 'activeAtSubmit' },
      outputName: intent.outputName ?? entityArchiveName(location),
      scope,
      manifest: this.resolver.entries(scope.handle),
    });
    if (!target.ok) {
      this.resolver.release(scope.handle);
      return { ok: false, error: target.error };
    }
    return { ok: true, scope, target, explanation: target.previewText };
  }

  /** Cek ulang sebelum eksekusi: sumber (+ selection bila relevan) dan tujuan masih sama. */
  revalidate(snapshot: PaneSnapshot, r: { scope: ScopeOk; target?: TargetOk }): CheckResult {
    const src = this.resolver.revalidate(snapshot, r.scope);
    if (!src.ok) return src;
    if (r.target && r.target.targetPane !== r.scope.pane)
      return this.ctx.check(snapshot, r.target.targetPane);
    return src;
  }

  /** Root pasangan panel untuk sync: harus folder biasa, berbeda, dan tidak bersarang. */
  resolveSync(snapshot: PaneSnapshot, envelope: IntentEnvelope): SyncFlowResult {
    const fail = (code: FlowErrorCode, message: string): SyncFlowResult => ({
      ok: false,
      error: { code, message },
    });
    if (envelope.requestId !== snapshot.requestId) {
      return fail('request_mismatch', 'Respons tidak cocok dengan permintaan aktif; diabaikan.');
    }
    if (envelope.intent.capability !== 'directories.sync')
      return fail('wrong_capability', 'Intent bukan sinkronisasi.');
    const panes: PaneName[] = ['left', 'right'];
    for (const p of panes) {
      const c = this.ctx.check(snapshot, p);
      if (!c.ok)
        return fail('stale_context', `${c.message} Susun ulang rencana dengan konteks terbaru.`);
    }
    const probes = panes.map((p) => this.ctx.host.probe(p));
    if (!probes[0] || !probes[1])
      return fail('pane_unavailable', 'Salah satu panel tidak tersedia.');
    for (const [i, p] of probes.entries()) {
      if (p!.viewKind === 'archive' || p!.viewKind === 'search' || p!.viewKind === 'branch') {
        return fail(
          'unsupported_view',
          `Panel ${panes[i]} bukan folder biasa; sinkronisasi belum didukung di tampilan tersebut.`,
        );
      }
    }
    const l = norm(probes[0].path);
    const r = norm(probes[1].path);
    if (l === r) return fail('same_root', 'Kedua panel menunjuk folder yang sama.');
    if (nestedIn(l, r) || nestedIn(r, l))
      return fail('nested_roots', 'Folder kiri dan kanan saling bersarang.');
    return {
      ok: true,
      leftRoot: probes[0].path,
      rightRoot: probes[1].path,
      explanation: `Sinkronisasi dua arah antara ${probes[0].path} dan ${probes[1].path}. Alias/symlink diperiksa lagi oleh backend sebelum menulis.`,
    };
  }
}
