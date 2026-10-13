// AI-02 / FER-179: command facade — prepare -> approve -> execute -> receipt.
//
// This module is the only bridge between AI intents and Ferrum's existing
// services. It never guesses an outcome from `runAction()` (which only reports
// that a handler was dispatched, e.g. a dialog was opened) and never builds an
// engine: adapters call the shared Pack/sync services.
//
// Trust model:
//   - `prepare` is read-only (zero mutation) and returns a `PreparedHandle`.
//   - `approve` is TRUSTED-ONLY (approval UI / policy, AI-05). A model can never
//     reach it: model output is parsed by `parseIntent` and cannot carry approval.
//   - `execute` accepts ONLY an `ApprovedPreparedCommand` issued by `approve`;
//     fabricated objects ({approved: true}, plans from the model) are rejected.
//   - One approved handle yields at most one execution; repeats return the same
//     result (double-click / retry never enqueue a second job).
import {
  ContractError,
  assertPreparedForPlan,
  parseExecutionReceipt,
  parseIntent,
  type ParseIntentOptions,
  parseResolvedPlan,
  type ExecutionReceipt,
  type IntentEnvelope,
  type PaneSnapshot,
  type PreparedHandle,
  type ProposedIntent,
  type ReadCapabilityId,
  type ResolvedOperation,
  type ResolvedPlan,
} from './contracts';

/** Capabilities with a prepared plan + approval (adapters, gates, risk). Unchanged set. */
export type MutatingCapabilityId = ProposedIntent['capability'];
/** ACT-08 read capability: answered locally, never planned or approved. */
export type { ReadCapabilityId };
/** Every capability id the provider may be offered. */
export type CapabilityId = MutatingCapabilityId | ReadCapabilityId;

export type CommandErrorCode =
  | 'invalid_intent'
  | 'blocked_capability'
  | 'resolve_failed'
  | 'invalid_plan'
  | 'unsupported_plan'
  | 'invalid_handle'
  | 'not_approved'
  | 'expired'
  | 'stale'
  | 'target_changed'
  | 'execution_failed';

export interface CommandError {
  code: CommandErrorCode;
  /** User-facing explanation (Indonesian), never model text. */
  message: string;
  /** Upstream code (context resolver / contract), for diagnostics and tests. */
  cause?: string;
}
export type Result<T> = { ok: true; value: T } | { ok: false; error: CommandError };
export const ok = <T>(value: T): Result<T> => ({ ok: true, value });
export const err = (code: CommandErrorCode, message: string, cause?: string): Result<never> => ({
  ok: false,
  error: cause === undefined ? { code, message } : { code, message, cause },
});

/** Local context for one request: the immutable snapshot taken at submit. */
export interface ResolvedContext {
  snapshot: PaneSnapshot;
}

export type Availability =
  | { available: true }
  | {
      available: false;
      code: 'pending' | 'action_missing' | 'action_disabled' | 'unknown_capability';
      reason: string;
    };

export interface PreviewInfo {
  /** One-paragraph preview shown for the single approval. */
  summary: string;
  itemCount: number;
  conflictCount: number;
  /** 0 = unknown (the backend gives no estimate yet). */
  totalBytes: number;
  outputPath?: string;
  warnings: string[];
}

/** What an adapter hands the facade after a read-only prepare. */
export interface AdapterPrepared<S = unknown> {
  operation: ResolvedOperation;
  preview: PreviewInfo;
  /** Stable strings that identify the exact work (manifest, target, facts). */
  digestParts: string[];
  /** Adapter-private frozen state (never leaves the facade). */
  state: S;
}

export interface AdapterRun {
  /** Queue operation ids (empty when the service completed in-process). */
  jobIds: string[];
  /** `queued` only when the queue still owns the work; otherwise `completed`. */
  state: 'queued' | 'completed';
  completed: number;
  conflictsSkipped: number;
  /** Items that could not be put on the queue (sync): the run must not be reported as fully done. */
  failedToQueue?: number;
  outputPath?: string;
  notes: string[];
}

export interface CommandAdapter<S = unknown> {
  readonly id: MutatingCapabilityId;
  /** Read-only. Must not call any mutating service. */
  prepare(envelope: IntentEnvelope, ctx: ResolvedContext): Promise<Result<AdapterPrepared<S>>>;
  /** Mutating. Re-validates snapshot/targets, then calls the shared service. */
  run(prepared: AdapterPrepared<S>, ctx: ResolvedContext): Promise<Result<AdapterRun>>;
  /** Release adapter-held handles (manifests); idempotent. */
  dispose?(prepared: AdapterPrepared<S>): void;
}

/** The facade's view of the capability registry (FER-181 implements it). */
export interface CapabilitySource {
  availability(id: string): Availability;
  adapterFor(id: string): Result<CommandAdapter>;
}

export interface PreparedCommand {
  handle: PreparedHandle;
  plan: ResolvedPlan;
  preview: PreviewInfo;
}

declare const approvedBrand: unique symbol;
/** Issued only by `CommandFacade.approve`; a plain object never qualifies. */
export interface ApprovedPreparedCommand {
  readonly handleId: string;
  readonly planDigest: string;
  readonly [approvedBrand]: true;
}

export interface CommandReceipt {
  executionId: string;
  planId: string;
  jobIds: string[];
  state: 'queued' | 'completed';
  completed: number;
  conflictsSkipped: number;
  failedToQueue?: number;
  outputPath?: string;
  notes: string[];
}

/** Map to the AI-01 wire receipt (validated). `queued` never implies success. */
export function toExecutionReceipt(receipt: CommandReceipt): ExecutionReceipt {
  return parseExecutionReceipt({
    schemaVersion: 1,
    executionId: receipt.executionId,
    planId: receipt.planId,
    jobIds: receipt.jobIds,
    state: receipt.state,
    completed: receipt.state === 'queued' ? 0 : receipt.completed,
    failed: 0,
    errors: [],
  });
}

/** Non-cryptographic 128-bit digest (4 x FNV-1a). Integrity binding for local handles. */
export function digestOf(parts: readonly string[]): string {
  const seeds = [0x811c9dc5, 0x01000193, 0x9e3779b1, 0x85ebca6b];
  const hashes = seeds.map((seed) => {
    let h = seed >>> 0;
    for (const part of parts) {
      for (let i = 0; i < part.length; i++) h = Math.imul(h ^ part.charCodeAt(i), 0x01000193) >>> 0;
      h = Math.imul(h ^ 0x1f, 0x01000193) >>> 0; // part separator
    }
    return h.toString(16).padStart(8, '0');
  });
  return hashes.join('');
}

interface Entry {
  handle: PreparedHandle;
  plan: ResolvedPlan;
  preview: PreviewInfo;
  adapter: CommandAdapter;
  prepared: AdapterPrepared;
  ctx: ResolvedContext;
  approved?: ApprovedPreparedCommand;
  execution?: Promise<Result<CommandReceipt>>;
}

export interface FacadeOptions {
  now?: () => number;
  /** Approval lifetime (default 10 minutes). */
  ttlMs?: number;
}

export class CommandFacade {
  private readonly entries = new Map<string, Entry>();
  private readonly issued = new WeakSet<object>();
  private seq = 0;
  private readonly now: () => number;
  private readonly ttlMs: number;

  constructor(
    private readonly capabilities: CapabilitySource,
    options: FacadeOptions = {},
  ) {
    this.now = options.now ?? Date.now;
    this.ttlMs = options.ttlMs ?? 10 * 60_000;
  }

  private nextId(prefix: string): string {
    return `${prefix}_${(++this.seq).toString(36)}_${Math.floor(Math.random() * 36 ** 4).toString(36)}`;
  }

  /** Read-only. Validates the model's envelope, resolves it locally, builds the plan. */
  async prepare(
    raw: unknown,
    ctx: ResolvedContext,
    options: ParseIntentOptions = {},
  ): Promise<Result<PreparedCommand>> {
    let envelope: IntentEnvelope;
    try {
      // `options.allowEntitySelector` (FER-433) is passed only by the executor for envelopes the APP built.
      envelope = parseIntent(raw, ctx.snapshot.requestId, options);
    } catch (e) {
      return err(
        'invalid_intent',
        'Perintah dari model tidak valid; tidak ada yang dijalankan.',
        e instanceof ContractError ? e.code : 'unknown',
      );
    }
    const id = envelope.intent.capability;
    const adapter = this.capabilities.adapterFor(id);
    if (!adapter.ok) return adapter;
    const out = await adapter.value.prepare(envelope, ctx);
    if (!out.ok) return out;
    const prepared = out.value;

    const planId = this.nextId('plan');
    const previewDigest = digestOf([
      id,
      ctx.snapshot.requestId,
      ...ctx.snapshot.panes.flatMap((p) => [p.tabId, String(p.contextRevision), p.location.path]),
      JSON.stringify(prepared.operation),
      ...prepared.digestParts,
    ]);
    let plan: ResolvedPlan;
    try {
      const candidate: ResolvedPlan = {
        schemaVersion: 1,
        planId,
        requestId: ctx.snapshot.requestId,
        revision: 1,
        capability: id,
        sourceSnapshot: ctx.snapshot,
        operation: prepared.operation,
        preview: {
          itemCount: prepared.preview.itemCount,
          conflictCount: prepared.preview.conflictCount,
          totalBytes: prepared.preview.totalBytes,
        },
        steps: [{ id: 'step_1', capability: id, dependsOn: [] }],
        previewDigest,
        risk: 'mutating',
        requiresApproval: true,
        stepCount: 1,
      };
      plan = parseResolvedPlan(JSON.parse(JSON.stringify(candidate)));
    } catch (e) {
      adapter.value.dispose?.(prepared);
      return err(
        'invalid_plan',
        'Rencana yang dihasilkan tidak lolos validasi kontrak; tidak ada yang dijalankan.',
        e instanceof ContractError ? e.code : 'unknown',
      );
    }
    const handle: PreparedHandle = {
      schemaVersion: 1,
      handleId: this.nextId('hnd'),
      planId,
      requestId: plan.requestId,
      revision: plan.revision,
      planDigest: previewDigest,
      expiresAtMs: this.now() + this.ttlMs,
    };
    this.entries.set(handle.handleId, {
      handle,
      plan,
      preview: prepared.preview,
      adapter: adapter.value,
      prepared,
      ctx,
    });
    return ok({ handle, plan, preview: prepared.preview });
  }

  /** TRUSTED-ONLY: record the user's approval of one prepared plan. */
  approve(handleId: string): Result<ApprovedPreparedCommand> {
    const entry = this.entries.get(handleId);
    if (!entry) return err('invalid_handle', 'Rencana tidak dikenal atau sudah dibuang.');
    if (this.now() >= entry.handle.expiresAtMs) {
      return err('expired', 'Persetujuan kedaluwarsa; susun ulang rencana.');
    }
    if (!entry.approved) {
      const approved = Object.freeze({
        handleId,
        planDigest: entry.handle.planDigest,
      }) as unknown as ApprovedPreparedCommand;
      this.issued.add(approved);
      entry.approved = approved;
    }
    return ok(entry.approved);
  }

  /**
   * Run an approved plan. Resolves with a receipt that distinguishes `queued`
   * (the operation queue owns the work) from `completed` (the service finished).
   * Repeated calls for the same approval return the same result.
   */
  execute(approved: ApprovedPreparedCommand): Promise<Result<CommandReceipt>> {
    if (typeof approved !== 'object' || approved === null || !this.issued.has(approved)) {
      return Promise.resolve(
        err('not_approved', 'Rencana belum disetujui oleh pengguna; tidak dijalankan.'),
      );
    }
    const entry = this.entries.get(approved.handleId);
    if (!entry) {
      return Promise.resolve(err('invalid_handle', 'Rencana tidak dikenal atau sudah dibuang.'));
    }
    if (entry.execution) return entry.execution; // idempotent: no second run
    entry.execution = this.runOnce(entry);
    return entry.execution;
  }

  /** Drop a prepared (not yet executed) plan and release its manifests. */
  discard(handleId: string): void {
    const entry = this.entries.get(handleId);
    if (!entry || entry.execution) return;
    entry.adapter.dispose?.(entry.prepared);
    this.entries.delete(handleId);
  }

  private async runOnce(entry: Entry): Promise<Result<CommandReceipt>> {
    try {
      assertPreparedForPlan(entry.handle, entry.plan, this.now());
    } catch {
      return err('expired', 'Persetujuan kedaluwarsa atau tidak cocok dengan rencana.');
    }
    // Capability may have been disabled after prepare; pending always blocks.
    const gate = this.capabilities.adapterFor(entry.plan.capability);
    if (!gate.ok) return gate;
    try {
      const run = await entry.adapter.run(entry.prepared, entry.ctx);
      if (!run.ok) return run;
      const r = run.value;
      const receipt: CommandReceipt = {
        executionId: this.nextId('exe'),
        planId: entry.plan.planId,
        jobIds: [...r.jobIds],
        state: r.state,
        completed: r.completed,
        conflictsSkipped: r.conflictsSkipped,
        notes: r.notes,
      };
      if (r.failedToQueue) receipt.failedToQueue = r.failedToQueue;
      if (r.outputPath !== undefined) receipt.outputPath = r.outputPath;
      return ok(receipt);
    } catch (e) {
      return err('execution_failed', e instanceof Error ? e.message : String(e));
    } finally {
      entry.adapter.dispose?.(entry.prepared);
    }
  }
}
