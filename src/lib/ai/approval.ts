// AI-05 / FER-192/193: persetujuan terikat pada prepared plan.
//
// Approval dibuat HANYA lewat modul ini dari tiket yang sudah dipresentasikan
// secara lokal (preview). Tiket mengikat: planId, plan digest, revision (plan +
// epoch edit), root, item scope, jumlah item, keputusan konflik, dan kedaluwarsa.
// Pengguna (UI) harus mengembalikan `bindingDigest` yang ia lihat; selisih sekecil
// apa pun = stale. Model tidak punya jalur ke sini: tidak ada field "approved".
import {
  digestOf,
  err,
  ok,
  type ApprovedPreparedCommand,
  type PreparedCommand,
  type Result,
} from './commands';
import { isIssuedDecision, type PolicyDecision } from './policy';
import type { ConflictView } from './chatStore.svelte';

/** Keputusan konflik per batch. Tidak ada `overwrite`: tidak ada overwrite tersirat. */
export type ConflictDecision = 'skip' | 'rename' | 'cancel';
const DECISIONS: readonly ConflictDecision[] = ['skip', 'rename', 'cancel'];

/** Port ke dialog konflik existing (satu pertanyaan per batch). */
export interface ConflictPort {
  decide(conflicts: readonly ConflictView[]): Promise<ConflictDecision>;
}

/** Satu keputusan untuk seluruh batch. Gagal/ditutup = cancel (bukan overwrite). */
export async function decideConflicts(
  port: ConflictPort,
  conflicts: readonly ConflictView[],
): Promise<ConflictDecision> {
  if (conflicts.length === 0) return 'skip';
  try {
    const d = await port.decide(conflicts);
    return DECISIONS.includes(d) ? d : 'cancel';
  } catch {
    return 'cancel';
  }
}

export interface ApprovalTicket {
  readonly runId: string;
  readonly handleId: string;
  readonly planId: string;
  readonly planDigest: string;
  /** plan.revision + jumlah edit; edit membatalkan approval lama. */
  readonly revision: number;
  readonly root: string;
  readonly itemScope: string;
  readonly itemCount: number;
  readonly conflictDecision: ConflictDecision;
  readonly expiresAtMs: number;
  readonly bindingDigest: string;
}

export interface ApprovalFacade {
  approve(handleId: string): Result<ApprovedPreparedCommand>;
}

interface RunApproval {
  epoch: number;
  ticket?: ApprovalTicket;
  approved?: ApprovedPreparedCommand;
  count: number;
}

export interface ApprovalOptions {
  now?: () => number;
}

export class ApprovalManager {
  private readonly runs = new Map<string, RunApproval>();
  private readonly now: () => number;

  constructor(
    private readonly facade: ApprovalFacade,
    options: ApprovalOptions = {},
  ) {
    this.now = options.now ?? Date.now;
  }

  private state(runId: string): RunApproval {
    let s = this.runs.get(runId);
    if (!s) this.runs.set(runId, (s = { epoch: 0, count: 0 }));
    return s;
  }

  /** Bangun tiket dari plan tepercaya hasil facade.prepare. Tidak menyetujui apa pun. */
  present(
    runId: string,
    prepared: PreparedCommand,
    opts: { conflictDecision?: ConflictDecision } = {},
  ): ApprovalTicket {
    const decision = opts.conflictDecision ?? 'skip';
    if (!DECISIONS.includes(decision)) {
      throw new Error('Conflict decision overwrite is not allowed from chat.');
    }
    const { plan, handle } = prepared;
    const s = this.state(runId);
    const op = plan.operation;
    const root =
      op.kind === 'archive.create' ? op.targetDirectory : `${op.leftRoot}|${op.rightRoot}`;
    const itemScope =
      op.kind === 'archive.create' ? op.sourceManifest : `${op.leftManifest}|${op.rightManifest}`;
    const revision = plan.revision + s.epoch;
    const itemCount = plan.preview.itemCount;
    const bindingDigest = digestOf([
      runId,
      plan.planId,
      plan.previewDigest,
      String(revision),
      root,
      itemScope,
      String(itemCount),
      decision,
      String(handle.expiresAtMs),
    ]);
    const ticket: ApprovalTicket = Object.freeze({
      runId,
      handleId: handle.handleId,
      planId: plan.planId,
      planDigest: plan.previewDigest,
      revision,
      root,
      itemScope,
      itemCount,
      conflictDecision: decision,
      expiresAtMs: handle.expiresAtMs,
      bindingDigest,
    });
    s.ticket = ticket;
    s.approved = undefined;
    return ticket;
  }

  ticketOf(runId: string): ApprovalTicket | undefined {
    return this.runs.get(runId)?.ticket;
  }

  approvalCount(runId: string): number {
    return this.runs.get(runId)?.count ?? 0;
  }

  /** Persetujuan pengguna: harus mengembalikan digest tiket yang ditampilkan. */
  approve(runId: string, confirmation: { bindingDigest: string }): Result<ApprovedPreparedCommand> {
    const s = this.runs.get(runId);
    const ticket = s?.ticket;
    if (!s || !ticket) {
      return err('not_approved', 'Tidak ada pratinjau yang menunggu persetujuan untuk run ini.');
    }
    if (confirmation.bindingDigest !== ticket.bindingDigest) {
      return err('stale', 'Persetujuan tidak cocok dengan pratinjau terbaru; tinjau ulang.');
    }
    if (this.now() >= ticket.expiresAtMs) {
      return err('expired', 'Persetujuan kedaluwarsa; susun ulang rencana.');
    }
    // Satu persetujuan per batch: pemanggilan ulang mengembalikan token yang sama.
    if (s.approved) return ok(s.approved);
    const approved = this.facade.approve(ticket.handleId);
    if (!approved.ok) return approved;
    s.approved = approved.value;
    s.count++;
    return approved;
  }

  /** Auto-create: hanya dengan keputusan policy lokal bertipe autoApproved. */
  autoApprove(runId: string, decision: PolicyDecision): Result<ApprovedPreparedCommand> {
    if (!isIssuedDecision(decision) || decision.kind !== 'autoApproved') {
      return err('not_approved', 'Kebijakan lokal tidak mengizinkan eksekusi otomatis.');
    }
    const ticket = this.ticketOf(runId);
    if (!ticket) return err('not_approved', 'Tidak ada pratinjau untuk run ini.');
    return this.approve(runId, { bindingDigest: ticket.bindingDigest });
  }

  /** Plan diedit / dibatalkan: approval lama tidak berlaku lagi; epoch naik. */
  invalidate(runId: string): void {
    const s = this.runs.get(runId);
    if (!s) return;
    s.epoch++;
    s.ticket = undefined;
    s.approved = undefined;
  }

  forget(runId: string): void {
    this.runs.delete(runId);
  }
}
