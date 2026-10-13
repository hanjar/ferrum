// FER-195..198: executor dengan context lane nyata (ContextService + FakeHost), CommandFacade nyata,
// ledger memori (semantik = backend Rust), queue palsu, provider palsu. "Engine" palsu menghitung
// penulisan sungguhan, sehingga tes dapat membuktikan nol mutasi sebelum persetujuan.
import { describe, expect, it, vi } from 'vitest';
import type { OperationProgress } from '../ipc/types';
import { ContextService } from './context';
import { FakeHost, entry } from './contextTestKit';
import {
  CommandFacade,
  err,
  ok,
  type AdapterPrepared,
  type Availability,
  type CapabilitySource,
  type CommandAdapter,
} from './commands';
import type { ConflictView, RunState, RunUpdate } from './chatStore.svelte';
import { AiExecutor, type ExecutorDeps } from './executor';
import { MemoryExecutionLedger } from './executorLedger';
import { canTransition } from './executorState';
import type { QueuePort } from './executorQueue';
import type { IntentProvider, ProviderEvent, ProviderPlanRequest } from './provider';
import type { IntentEnvelope } from './contracts';
import { RemoteProviderError, type RemoteProviderErrorCode } from './remote-provider';

const LEFT = 'D:\\Projects\\Invoice';
const RIGHT = 'E:\\Backup';

class FakeEngine {
  archiveWrites = 0;
  enqueues = 0;
  nextJob = 1;
  hangEnqueue: Promise<void> | null = null;
  throwOnEnqueue = false;
  /** Pack menjawab gagal: `cause` = kontrak adapter (FER-165); tanpa cause = error tak dikenal (throw). */
  archiveFailure: { cause?: string } | null = null;
}

class FakeQueue implements QueuePort {
  jobs = new Map<string, OperationProgress>();
  cancelled: string[] = [];
  private listeners = new Set<(p: OperationProgress) => void>();
  get(id: string) {
    return this.jobs.get(id);
  }
  subscribe(l: (p: OperationProgress) => void) {
    this.listeners.add(l);
    return () => this.listeners.delete(l);
  }
  async cancel(id: string) {
    this.cancelled.push(id);
  }
  set(id: string, over: Partial<OperationProgress>) {
    const p: OperationProgress = {
      operationId: id,
      kind: 'copy',
      state: 'queued',
      processedBytes: 0,
      processedFiles: 0,
      ...this.jobs.get(id),
      ...over,
    };
    this.jobs.set(id, p);
    for (const l of [...this.listeners]) l(p);
  }
}

class Gate {
  promise: Promise<void>;
  open!: () => void;
  constructor() {
    this.promise = new Promise((r) => (this.open = r));
  }
}

class ScriptedProvider implements IntentProvider {
  requests: Array<ProviderPlanRequest & { capabilities?: readonly string[] }> = [];
  gate: Gate | null = null;
  script: (r: ProviderPlanRequest) => ProviderEvent | Promise<ProviderEvent> | never = (r) => ({
    kind: 'intent',
    intent: archiveIntent(r.requestId),
  });
  fail = false;
  failWith: unknown = undefined;
  async *plan(request: ProviderPlanRequest): AsyncGenerator<ProviderEvent> {
    this.requests.push(request);
    if (this.gate) await this.gate.promise; // sengaja mengabaikan AbortSignal: respons terlambat
    if (this.failWith !== undefined) throw this.failWith;
    if (this.fail) throw new Error('secret-key sk-LEAK boom');
    yield await this.script(request);
  }
}

function archiveIntent(requestId: string): IntentEnvelope {
  return {
    schemaVersion: 1,
    requestId,
    intent: {
      capability: 'archive.create',
      sourcePane: 'left',
      selector: { kind: 'allVisible', entries: 'all' },
      format: 'zip',
      target: { kind: 'sourceDirectory' },
    },
  };
}
function syncIntent(requestId: string): IntentEnvelope {
  return {
    schemaVersion: 1,
    requestId,
    intent: {
      capability: 'directories.sync',
      leftPane: 'left',
      rightPane: 'right',
      mode: 'bidirectional',
    },
  };
}

function setup(opts: { conflicts?: number } = {}) {
  const host = new FakeHost();
  host.left.path = LEFT;
  host.left.all = [
    entry('a.txt', { path: `${LEFT}\\a.txt` }),
    entry('b.txt', { path: `${LEFT}\\b.txt` }),
  ];
  host.right.path = RIGHT;
  const svc = new ContextService(host);
  const engine = new FakeEngine();
  const queue = new FakeQueue();
  const blocked = new Set<string>();
  const prepareSpy = vi.fn();

  const checkStale = (ctx: { snapshot: Parameters<ContextService['check']>[0] }) => {
    for (const pane of ['left', 'right'] as const) {
      const c = svc.check(ctx.snapshot, pane);
      if (!c.ok) return err('stale', c.message, c.reason);
    }
    return null;
  };
  const baseOp = (capability: 'archive.create' | 'directories.sync'): AdapterPrepared =>
    capability === 'archive.create'
      ? {
          operation: {
            kind: 'archive.create',
            sourceManifest: 'man_1',
            targetDirectory: LEFT,
            outputName: 'Invoice.zip',
          },
          preview: {
            summary: 'Buat Invoice.zip',
            itemCount: 2,
            conflictCount: 0,
            totalBytes: 0,
            warnings: [],
          },
          digestParts: ['a'],
          state: {},
        }
      : {
          operation: {
            kind: 'directories.sync',
            leftRoot: LEFT,
            rightRoot: RIGHT,
            leftManifest: 'l_1',
            rightManifest: 'r_1',
          },
          preview: {
            summary: 'Sync',
            itemCount: 4,
            conflictCount: opts.conflicts ?? 0,
            totalBytes: 0,
            warnings: [],
          },
          digestParts: ['s'],
          state: {},
        };
  const archive: CommandAdapter = {
    id: 'archive.create',
    async prepare() {
      prepareSpy('archive');
      return ok(baseOp('archive.create'));
    },
    async run(_p, ctx) {
      const stale = checkStale(ctx);
      if (stale) return stale;
      if (engine.archiveFailure) {
        if (engine.archiveFailure.cause === undefined) throw new Error('ipc closed D:\\secret');
        return err(
          'execution_failed',
          'Pack gagal di D:\\secret\\path',
          engine.archiveFailure.cause,
        );
      }
      engine.archiveWrites++;
      return ok({
        jobIds: [],
        state: 'completed' as const,
        completed: 2,
        conflictsSkipped: 0,
        notes: [],
        outputPath: `${LEFT}\\Invoice.zip`,
      });
    },
  };
  const sync: CommandAdapter = {
    id: 'directories.sync',
    async prepare() {
      prepareSpy('sync');
      return ok(baseOp('directories.sync'));
    },
    async run(_p, ctx) {
      const stale = checkStale(ctx);
      if (stale) return stale;
      if (engine.hangEnqueue) await engine.hangEnqueue;
      if (engine.throwOnEnqueue) throw new Error('queue exploded after enqueue');
      engine.enqueues++;
      return ok({
        jobIds: [`job_${engine.nextJob++}`],
        state: 'queued' as const,
        completed: 0,
        conflictsSkipped: 0,
        notes: [],
      });
    },
  };
  const adapters: Record<string, CommandAdapter> = {
    'archive.create': archive,
    'directories.sync': sync,
  };
  const availability = (id: string): Availability =>
    blocked.has(id)
      ? { available: false, code: 'pending', reason: `${id} belum tersedia` }
      : { available: true };
  const caps: CapabilitySource & { describe(): Array<{ id: string; available: boolean }> } = {
    availability,
    adapterFor: (id) => {
      const a = availability(id);
      if (!a.available) return err('blocked_capability', a.reason, a.code);
      return ok(adapters[id]);
    },
    describe: () =>
      Object.keys(adapters).map((id) => ({ id, available: availability(id).available })),
  };
  let nowMs = 1_000;
  const facade = new CommandFacade(caps, { now: () => nowMs, ttlMs: 60_000 });
  const provider = new ScriptedProvider();
  const ledger = new MemoryExecutionLedger();
  const updates: RunUpdate[] = [];

  const make = (extra: Partial<ExecutorDeps> = {}, led = ledger) => {
    const ex = new AiExecutor({
      provider,
      capture: (id) => svc.capture(id),
      modelContext: (s) => svc.modelContext(s),
      facade,
      capabilities: caps,
      ledger: led,
      queue,
      now: () => nowMs,
      ...extra,
    });
    ex.subscribe((u) => updates.push(u));
    return ex;
  };
  const executor = make();
  let n = 0;
  const submit = (ex = executor, text = 'arsipkan') => {
    n++;
    const runId = `run-${n}`;
    ex.submit({ requestId: `req_${n}`, text, runId } as never);
    return { runId, requestId: `req_${n}` };
  };
  const stateOf = (runId: string): RunState | undefined =>
    [...updates].reverse().find((u) => u.runId === runId)?.state;
  const waitState = (runId: string, state: RunState) =>
    vi.waitFor(() => expect(stateOf(runId)).toBe(state));
  const lastOf = (runId: string) => [...updates].reverse().find((u) => u.runId === runId)!;
  return {
    host,
    svc,
    engine,
    queue,
    blocked,
    prepareSpy,
    facade,
    provider,
    ledger,
    updates,
    executor,
    make,
    submit,
    stateOf,
    waitState,
    lastOf,
    advance: (ms: number) => (nowMs += ms),
  };
}

/** Setiap urutan state yang di-emit harus sah menurut state machine. */
function assertLegal(updates: RunUpdate[], runId: string): void {
  const seq = updates.filter((u) => u.runId === runId).map((u) => u.state);
  let prev: RunState = 'planning';
  for (const s of seq) {
    if (s !== prev) expect(canTransition(prev, s), `${prev} -> ${s}`).toBe(true);
    prev = s;
  }
}

describe('AiExecutor: happy path and zero mutation before approval', () => {
  it('plans, previews and waits for approval with zero engine writes', async () => {
    const t = setup();
    const { runId } = t.submit();
    await t.waitState(runId, 'awaitingApproval');
    expect(t.engine.archiveWrites + t.engine.enqueues).toBe(0);
    expect(t.queue.jobs.size).toBe(0);
    const u = t.lastOf(runId);
    expect(u.plan?.capability).toBe('archive.create');
    assertLegal(t.updates, runId);
    expect(t.updates.filter((x) => x.runId === runId).map((x) => x.state)).toEqual([
      'resolving',
      'preview',
      'awaitingApproval',
    ]);
  });

  it('carries the locally built scope summary and warnings from the facade preview to the run', async () => {
    const t = setup();
    const { runId } = t.submit();
    await t.waitState(runId, 'awaitingApproval');
    const u = t.lastOf(runId);
    expect(u.scopeNote).toBe('Buat Invoice.zip');
    expect(u.warnings).toEqual([]);
    const preview = t.updates.find((x) => x.runId === runId && x.state === 'preview');
    expect(preview?.scopeNote).toBe('Buat Invoice.zip');
  });

  it('offers the provider the registry capabilities (not hardcoded) and passes the abort signal', async () => {
    const t = setup();
    t.blocked.add('directories.sync');
    t.submit();
    await vi.waitFor(() => expect(t.provider.requests).toHaveLength(1));
    expect(t.provider.requests[0].capabilities).toEqual(['archive.create']);
    expect(t.provider.requests[0].signal).toBeInstanceOf(AbortSignal);
    // Registry baru menyediakan offeredCapabilityIds(): dipakai apa adanya, bukan daftar tetap.
    const t2 = setup();
    const ex2 = t2.make({
      capabilities: {
        availability: () => ({ available: true }),
        describe: () => [],
        offeredCapabilityIds: () => ['directories.sync'],
      },
    });
    ex2.submit({ requestId: 'req_x', text: 'x', runId: 'run-x' } as never);
    await vi.waitFor(() =>
      expect(t2.provider.requests.at(-1)?.capabilities).toEqual(['directories.sync']),
    );
  });

  it('approve runs the archive once and success comes from the engine result', async () => {
    const t = setup();
    const { runId } = t.submit();
    await t.waitState(runId, 'awaitingApproval');
    t.executor.approve(runId);
    t.executor.approve(runId); // double click
    await t.waitState(runId, 'succeeded');
    expect(t.engine.archiveWrites).toBe(1);
    const u = t.lastOf(runId);
    expect(u.receipt).toMatchObject({ state: 'completed', completed: 2, failed: 0 });
    assertLegal(t.updates, runId);
  });

  it('approve outside awaitingApproval or for unknown runs does nothing', async () => {
    const t = setup();
    t.executor.approve('run-nope');
    const { runId } = t.submit();
    t.executor.approve(runId); // still planning/resolving
    await t.waitState(runId, 'awaitingApproval');
    expect(t.engine.archiveWrites).toBe(0);
  });

  it('submit with the same runId twice plans once', async () => {
    const t = setup();
    t.executor.submit({ requestId: 'req_1', text: 'a', runId: 'run-1' } as never);
    t.executor.submit({ requestId: 'req_1', text: 'a', runId: 'run-1' } as never);
    await t.waitState('run-1', 'awaitingApproval');
    expect(t.provider.requests).toHaveLength(1);
  });
});

describe('AiExecutor: honest blocked / needsInput / failures', () => {
  it('a pending capability is blocked with the registry reason; nothing is prepared or written', async () => {
    const t = setup();
    t.blocked.add('directories.sync');
    t.provider.script = (r) => ({ kind: 'intent', intent: syncIntent(r.requestId) });
    const { runId } = t.submit();
    await t.waitState(runId, 'blockedCapability');
    expect(t.lastOf(runId).blockedReason).toContain('belum tersedia');
    expect(t.prepareSpy).not.toHaveBeenCalled();
    expect(t.engine.enqueues + t.engine.archiveWrites).toBe(0);
  });

  it('a model question becomes needsInput and never a status', async () => {
    const t = setup();
    t.provider.script = () => ({ kind: 'question', text: 'Folder mana?' });
    const { runId } = t.submit();
    await t.waitState(runId, 'needsInput');
    expect(t.lastOf(runId).modelText).toBe('Folder mana?');
  });

  it('an invalid/forged intent (extra approved flag) fails without preparing anything', async () => {
    const t = setup();
    t.provider.script = (r) =>
      ({
        kind: 'intent',
        intent: {
          ...archiveIntent(r.requestId),
          intent: { ...archiveIntent(r.requestId).intent, approved: true },
        },
      }) as never;
    const { runId } = t.submit();
    await t.waitState(runId, 'failed');
    expect(t.prepareSpy).not.toHaveBeenCalled();
    expect(t.engine.archiveWrites).toBe(0);
  });

  it('provider failure shows a generic message and leaks nothing from the error', async () => {
    const t = setup();
    t.provider.fail = true;
    const { runId } = t.submit();
    await t.waitState(runId, 'failed');
    expect(JSON.stringify(t.lastOf(runId))).not.toContain('sk-LEAK');
  });

  it('FER-465: a stopped server is classified as unreachable with host:port, actions and no leak', async () => {
    const t = setup();
    const seen: string[] = [];
    const ex = t.make({
      providerEndpoint: () => 'http://127.0.0.1:20128/v1',
      onProviderFailure: (code) => seen.push(code),
    });
    t.provider.failWith = new RemoteProviderError('unreachable', 'Provider request failed');
    const { runId } = t.submit(ex);
    await t.waitState(runId, 'failed');
    const u = t.lastOf(runId);
    expect(u.errorMessage).toContain('Server AI tidak bisa dihubungi (127.0.0.1:20128)');
    expect(u.errorMessage).toContain('server lokal');
    expect(u.errorMessage).not.toContain('/v1');
    expect(u.providerError?.code).toBe('unreachable');
    expect(u.providerError?.actions).toEqual(['retry', 'settings', 'test']);
    expect(seen).toEqual(['unreachable']);
  });

  it.each<[RemoteProviderErrorCode, string]>([
    ['timeout', 'timeout'],
    ['auth', 'auth'],
    ['model_unavailable', 'model_not_found'],
    ['bad_request', 'bad_request'],
    ['rate_limited', 'rate_limited'],
    ['server_error', 'server_error'],
    ['malformed', 'invalid_response'],
    ['refused', 'not_supported'],
  ])('FER-465: backend kind %s -> UI code %s', async (kind, ui) => {
    const t = setup();
    t.provider.failWith = new RemoteProviderError(kind, 'Provider request failed');
    const { runId } = t.submit();
    await t.waitState(runId, 'failed');
    expect(t.lastOf(runId).providerError?.code).toBe(ui);
  });

  it('FER-465: not_supported lists what the assistant can do now', async () => {
    const t = setup();
    t.provider.failWith = new RemoteProviderError('refused', 'x');
    const { runId } = t.submit();
    await t.waitState(runId, 'failed');
    const message = t.lastOf(runId).errorMessage ?? '';
    expect(message).toContain('arsip ZIP');
    expect(message).toContain('sinkronisasi');
    expect(t.lastOf(runId).providerError?.actions).toEqual([]);
  });

  it('a resolve failure (empty selection) is needsInput with the local message', async () => {
    const t = setup();
    const orig = t.facade.prepare.bind(t.facade);
    vi.spyOn(t.facade, 'prepare').mockImplementation(async (raw, ctx) => {
      void orig;
      void raw;
      void ctx;
      return err('resolve_failed', 'Belum ada item yang dipilih.', 'empty_selection');
    });
    const { runId } = t.submit();
    await t.waitState(runId, 'needsInput');
    expect(t.lastOf(runId).errorMessage).toBe('Belum ada item yang dipilih.');
  });
});

describe('AiExecutor: late provider response, cancel, edit', () => {
  it('cancel while planning: a late provider response creates no plan, no prepare, no job', async () => {
    const t = setup();
    t.provider.gate = new Gate();
    const { runId } = t.submit();
    await vi.waitFor(() => expect(t.provider.requests).toHaveLength(1));
    t.executor.cancel(runId);
    expect(t.stateOf(runId)).toBe('cancelled');
    expect(t.provider.requests[0].signal.aborted).toBe(true);
    t.provider.gate.open(); // provider answers anyway
    await new Promise((r) => setTimeout(r, 20));
    expect(t.stateOf(runId)).toBe('cancelled');
    expect(t.prepareSpy).not.toHaveBeenCalled();
    expect(t.updates.filter((u) => u.runId === runId).map((u) => u.state)).toEqual(['cancelled']);
    expect(t.engine.archiveWrites + t.engine.enqueues).toBe(0);
  });

  it('cancel at awaitingApproval discards the plan; a later approve is ignored', async () => {
    const t = setup();
    const { runId } = t.submit();
    await t.waitState(runId, 'awaitingApproval');
    t.executor.cancel(runId);
    t.executor.approve(runId);
    await new Promise((r) => setTimeout(r, 10));
    expect(t.stateOf(runId)).toBe('cancelled');
    expect(t.engine.archiveWrites).toBe(0);
  });

  it('edit invalidates the approval: approving the old plan does nothing; a new run needs its own approval', async () => {
    const t = setup();
    const first = t.submit();
    await t.waitState(first.runId, 'awaitingApproval');
    const digest = t.lastOf(first.runId).plan!.previewDigest;
    t.executor.edit(first.runId);
    expect(t.stateOf(first.runId)).toBe('needsInput');
    t.executor.approve(first.runId, { planDigest: digest });
    await new Promise((r) => setTimeout(r, 10));
    expect(t.engine.archiveWrites).toBe(0);
    const second = t.submit();
    await t.waitState(second.runId, 'awaitingApproval');
    expect(t.engine.archiveWrites).toBe(0);
    t.executor.approve(second.runId);
    await t.waitState(second.runId, 'succeeded');
    expect(t.engine.archiveWrites).toBe(1);
  });

  it('a late provider response after edit is dropped', async () => {
    const t = setup();
    t.provider.gate = new Gate();
    const { runId } = t.submit();
    await vi.waitFor(() => expect(t.provider.requests).toHaveLength(1));
    t.executor.edit(runId);
    t.provider.gate.open();
    await new Promise((r) => setTimeout(r, 20));
    expect(t.stateOf(runId)).toBe('needsInput');
    expect(t.prepareSpy).not.toHaveBeenCalled();
  });

  it('approving with a digest that is not the one presented is stale and writes nothing', async () => {
    const t = setup();
    const { runId } = t.submit();
    await t.waitState(runId, 'awaitingApproval');
    t.executor.approve(runId, { planDigest: 'not-the-shown-digest' });
    expect(t.stateOf(runId)).toBe('stale');
    expect(t.engine.archiveWrites).toBe(0);
  });
});

describe('AiExecutor: stale snapshot, target changes, backend guard', () => {
  it('pane navigation between preview and approval is stale; ledger releases the plan; nothing written', async () => {
    const t = setup();
    const { runId } = t.submit();
    await t.waitState(runId, 'awaitingApproval');
    t.host.left.listing = {}; // pane navigated/relisted
    t.executor.approve(runId);
    await t.waitState(runId, 'stale');
    expect(t.engine.archiveWrites).toBe(0);
    expect(await t.ledger.unresolved()).toEqual([]);
  });

  it('pane swap while waiting is stale', async () => {
    const t = setup();
    const { runId } = t.submit();
    await t.waitState(runId, 'awaitingApproval');
    t.host.swap();
    t.executor.approve(runId);
    await t.waitState(runId, 'stale');
    expect(t.engine.archiveWrites).toBe(0);
  });

  it('approval past its expiry is stale', async () => {
    const t = setup();
    const { runId } = t.submit();
    await t.waitState(runId, 'awaitingApproval');
    t.advance(61_000);
    t.executor.approve(runId);
    await t.waitState(runId, 'stale');
    expect(t.engine.archiveWrites).toBe(0);
  });

  it('backend guard rejection (target/source changed) is stale and nothing is written', async () => {
    const t = setup();
    const guard = {
      authorize: vi.fn(async () => ({ ok: false as const, message: 'Rencana usang (1 alasan).' })),
    };
    const ex = t.make({ guard });
    const { runId } = t.submit(ex);
    await t.waitState(runId, 'awaitingApproval');
    ex.approve(runId);
    await t.waitState(runId, 'stale');
    expect(guard.authorize).toHaveBeenCalledTimes(1);
    expect(t.engine.archiveWrites).toBe(0);
    expect(t.lastOf(runId).errorMessage).toContain('usang');
  });

  it('a throwing guard fails closed', async () => {
    const t = setup();
    const ex = t.make({
      guard: {
        authorize: async () => {
          throw new Error('ipc down');
        },
      },
    });
    const { runId } = t.submit(ex);
    await t.waitState(runId, 'awaitingApproval');
    ex.approve(runId);
    await t.waitState(runId, 'stale');
    expect(t.engine.archiveWrites).toBe(0);
  });
});

describe('AiExecutor: backend approval slot (FER-410)', () => {
  const okGuard = () => ({
    register: vi.fn(async () => ({ ok: true as const })),
    authorize: vi.fn(async () => ({ ok: true as const })),
    revoke: vi.fn(),
  });

  it('registers the approval BEFORE the preview is shown, with the ticket and the trusted plan', async () => {
    const t = setup();
    const guard = okGuard();
    const ex = t.make({ guard });
    const { runId } = t.submit(ex);
    await t.waitState(runId, 'awaitingApproval');
    expect(guard.register).toHaveBeenCalledTimes(1);
    const [ticket, plan] = guard.register.mock.calls[0] as unknown as [
      { handleId: string },
      { capability: string },
    ];
    expect(ticket.handleId).toMatch(/\S/);
    expect(plan.capability).toBe('archive.create');
    expect(guard.authorize).not.toHaveBeenCalled();
    expect(t.engine.archiveWrites).toBe(0);
  });

  it('a guard that cannot register fails closed: no preview, nothing can be approved or written', async () => {
    const t = setup();
    const guard = {
      ...okGuard(),
      register: vi.fn(async () => ({ ok: false as const, message: 'Item tidak terbaca.' })),
    };
    const ex = t.make({ guard });
    const { runId } = t.submit(ex);
    await t.waitState(runId, 'failed');
    expect(t.updates.filter((u) => u.runId === runId).map((u) => u.state)).not.toContain(
      'awaitingApproval',
    );
    expect(t.lastOf(runId).errorMessage).toContain('Item tidak terbaca');
    expect(t.engine.archiveWrites + t.engine.enqueues).toBe(0);
    ex.approve(runId);
    await Promise.resolve();
    expect(t.engine.archiveWrites).toBe(0);
  });

  it('a throwing register fails closed as well', async () => {
    const t = setup();
    const guard = {
      ...okGuard(),
      register: vi.fn(async () => {
        throw new Error('ipc down');
      }),
    };
    const ex = t.make({ guard });
    const { runId } = t.submit(ex);
    await t.waitState(runId, 'failed');
    expect(t.engine.archiveWrites).toBe(0);
  });

  it('revokes the slot when the run is cancelled before approval', async () => {
    const t = setup();
    const guard = okGuard();
    const ex = t.make({ guard });
    const { runId } = t.submit(ex);
    await t.waitState(runId, 'awaitingApproval');
    ex.cancel(runId);
    await t.waitState(runId, 'cancelled');
    expect(guard.revoke).toHaveBeenCalled();
    expect(guard.authorize).not.toHaveBeenCalled();
  });

  it('authorizes exactly once on approve and the engine writes once', async () => {
    const t = setup();
    const guard = okGuard();
    const ex = t.make({ guard });
    const { runId } = t.submit(ex);
    await t.waitState(runId, 'awaitingApproval');
    ex.approve(runId);
    await t.waitState(runId, 'succeeded');
    expect(guard.authorize).toHaveBeenCalledTimes(1);
    expect(t.engine.archiveWrites).toBe(1);
  });
});

describe('AiExecutor: policy and conflicts', () => {
  it('auto-create runs an archive without approval, exactly once', async () => {
    const t = setup();
    const ex = t.make({ prefs: () => ({ autoCreate: true }) });
    const { runId } = t.submit(ex);
    await t.waitState(runId, 'succeeded');
    expect(t.engine.archiveWrites).toBe(1);
    expect(t.updates.filter((u) => u.runId === runId).map((u) => u.state)).not.toContain(
      'awaitingApproval',
    );
  });

  it('auto-create never skips approval for sync', async () => {
    const t = setup();
    t.provider.script = (r) => ({ kind: 'intent', intent: syncIntent(r.requestId) });
    const ex = t.make({ prefs: () => ({ autoCreate: true }) });
    const { runId } = t.submit(ex);
    await t.waitState(runId, 'awaitingApproval');
    expect(t.engine.enqueues).toBe(0);
  });

  const conflict: ConflictView = { itemRef: 'c1', label: '<b>a.txt</b>', kind: 'both-changed' };
  it('asks the conflict port once per batch; skip proceeds', async () => {
    const t = setup({ conflicts: 2 });
    t.provider.script = (r) => ({ kind: 'intent', intent: syncIntent(r.requestId) });
    const port = { decide: vi.fn(async () => 'skip' as const) };
    const ex = t.make({
      conflicts: port,
      conflictViews: () => [conflict, { ...conflict, itemRef: 'c2' }],
    });
    const { runId } = t.submit(ex);
    await t.waitState(runId, 'awaitingApproval');
    expect(t.lastOf(runId).conflicts).toHaveLength(2);
    ex.approve(runId);
    await t.waitState(runId, 'queued');
    expect(port.decide).toHaveBeenCalledTimes(1);
    expect(t.engine.enqueues).toBe(1);
  });

  it('cancelling the conflict dialog runs nothing', async () => {
    const t = setup({ conflicts: 1 });
    t.provider.script = (r) => ({ kind: 'intent', intent: syncIntent(r.requestId) });
    const ex = t.make({
      conflicts: { decide: async () => 'cancel' as const },
      conflictViews: () => [conflict],
    });
    const { runId } = t.submit(ex);
    await t.waitState(runId, 'awaitingApproval');
    ex.approve(runId);
    await t.waitState(runId, 'cancelled');
    expect(t.engine.enqueues).toBe(0);
  });
});

describe('AiExecutor: queue progress, ownership, cancel, partial', () => {
  async function queuedSync(t: ReturnType<typeof setup>) {
    t.provider.script = (r) => ({ kind: 'intent', intent: syncIntent(r.requestId) });
    const { runId } = t.submit();
    await t.waitState(runId, 'awaitingApproval');
    t.executor.approve(runId);
    await t.waitState(runId, 'queued');
    await vi.waitFor(() => expect(t.lastOf(runId).receipt?.jobIds).toEqual(['job_1']));
    return runId;
  }

  it("progress and completion come only from the run's own job IDs", async () => {
    const t = setup();
    const runId = await queuedSync(t);
    t.queue.set('job_999', { state: 'completed', processedFiles: 99 }); // someone else's job
    expect(t.stateOf(runId)).toBe('queued');
    t.queue.set('job_1', { state: 'running', processedFiles: 2, totalFiles: 4 });
    expect(t.stateOf(runId)).toBe('running');
    expect(t.lastOf(runId).jobs).toEqual([{ jobId: 'job_1', state: 'running', done: 2, total: 4 }]);
    t.queue.set('job_1', { state: 'completed', processedFiles: 4, totalFiles: 4 });
    expect(t.stateOf(runId)).toBe('succeeded');
    expect(t.lastOf(runId).receipt).toMatchObject({
      state: 'completed',
      completed: 4,
      jobIds: ['job_1'],
    });
    assertLegal(t.updates, runId);
  });

  it('queued is never reported as success', async () => {
    const t = setup();
    const runId = await queuedSync(t);
    expect(t.stateOf(runId)).toBe('queued');
    expect(t.lastOf(runId).receipt?.state).toBe('queued');
  });

  it('cancel after a partial copy: cancels only owned jobs and ends partial with copied count', async () => {
    const t = setup();
    const runId = await queuedSync(t);
    t.queue.set('job_1', { state: 'running', processedFiles: 5, totalFiles: 10 });
    t.queue.set('job_777', { state: 'running' }); // unrelated job in the queue
    t.executor.cancel(runId);
    await vi.waitFor(() => expect(t.queue.cancelled).toEqual(['job_1']));
    expect(t.stateOf(runId)).toBe('running'); // waits for the engine to acknowledge
    t.queue.set('job_1', { state: 'cancelled', processedFiles: 5 });
    expect(t.stateOf(runId)).toBe('partial');
    const u = t.lastOf(runId);
    expect(u.receipt).toMatchObject({ state: 'partial', completed: 5 });
    expect(u.errorMessage).toMatch(/tanpa rollback/);
    expect(t.queue.cancelled).not.toContain('job_777');
  });

  it('cancel while queued with no progress ends cancelled', async () => {
    const t = setup();
    const runId = await queuedSync(t);
    t.executor.cancel(runId);
    await vi.waitFor(() => expect(t.queue.cancelled).toEqual(['job_1']));
    t.queue.set('job_1', { state: 'cancelled' });
    expect(t.stateOf(runId)).toBe('cancelled');
  });

  it('cancel while execute is in flight cancels the job as soon as it is known', async () => {
    const t = setup();
    const gate = new Gate();
    t.engine.hangEnqueue = gate.promise;
    t.provider.script = (r) => ({ kind: 'intent', intent: syncIntent(r.requestId) });
    const { runId } = t.submit();
    await t.waitState(runId, 'awaitingApproval');
    t.executor.approve(runId);
    await t.waitState(runId, 'queued');
    await vi.waitFor(() => expect(t.engine.enqueues).toBe(0));
    t.executor.cancel(runId);
    gate.open();
    await vi.waitFor(() => expect(t.queue.cancelled).toEqual(['job_1']));
    t.queue.set('job_1', { state: 'cancelled' });
    expect(t.stateOf(runId)).toBe('cancelled');
  });

  it('a failed job with per-item failures reports failed/partial from the engine', async () => {
    const t = setup();
    const runId = await queuedSync(t);
    t.queue.set('job_1', {
      state: 'failed',
      processedFiles: 3,
      failures: [{ path: 'D:\\x', message: 'disk full' }],
    });
    expect(t.stateOf(runId)).toBe('partial');
    expect(t.lastOf(runId).receipt).toMatchObject({ state: 'partial', completed: 3, failed: 1 });
  });

  it('cancel on an in-process Pack (no job id) reports honestly that it cannot be cancelled', async () => {
    const t = setup();
    const gate = new Gate();
    const orig = t.facade.execute.bind(t.facade);
    vi.spyOn(t.facade, 'execute').mockImplementation(async (a) => {
      await gate.promise;
      return orig(a);
    });
    const { runId } = t.submit();
    await t.waitState(runId, 'awaitingApproval');
    t.executor.approve(runId);
    await t.waitState(runId, 'queued');
    t.executor.cancel(runId);
    gate.open();
    await t.waitState(runId, 'succeeded'); // engine finished; cancel was not an engine capability
  });
});

describe('AiExecutor: idempotency, unknown outcome, restart', () => {
  it('double approve + provider retry (new run, same plan content) never doubles the job', async () => {
    const t = setup();
    t.provider.script = (r) => ({ kind: 'intent', intent: syncIntent(r.requestId) });
    const { runId } = t.submit();
    await t.waitState(runId, 'awaitingApproval');
    t.executor.approve(runId);
    t.executor.approve(runId);
    await t.waitState(runId, 'queued');
    await vi.waitFor(() => expect(t.lastOf(runId).receipt?.jobIds).toEqual(['job_1']));
    expect(t.engine.enqueues).toBe(1);
    expect(t.queue.cancelled).toEqual([]);
    expect((await t.ledger.byExecution(t.lastOf(runId).receipt!.executionId))?.jobIds).toEqual([
      'job_1',
    ]);
  });

  it('an exception after enqueue is unknownOutcome: no replay, ledger marks it unresolved', async () => {
    const t = setup();
    t.engine.throwOnEnqueue = true;
    t.provider.script = (r) => ({ kind: 'intent', intent: syncIntent(r.requestId) });
    const { runId } = t.submit();
    await t.waitState(runId, 'awaitingApproval');
    t.executor.approve(runId);
    await t.waitState(runId, 'unknownOutcome');
    expect(t.engine.enqueues).toBe(0);
    const pending = await t.executor.recover();
    expect(pending).toHaveLength(1);
    expect(pending[0].state).toBe('unknownOutcome');
    t.executor.approve(runId); // already final
    await new Promise((r) => setTimeout(r, 10));
    expect(t.engine.enqueues).toBe(0);
  });

  it('crash between enqueue and receipt: after restart the ledger reports unknown and reconcile resolves it', async () => {
    const t = setup();
    t.provider.script = (r) => ({ kind: 'intent', intent: syncIntent(r.requestId) });
    // Simulasikan proses mati tepat setelah reserve: ledger menyimpan niat, execute tak pernah kembali.
    const dying = new MemoryExecutionLedger();
    const crashing = Object.create(dying) as MemoryExecutionLedger;
    crashing.recordEnqueued = () => new Promise(() => undefined); // receipt tak pernah tersimpan
    const ex = t.make({}, crashing);
    const { runId } = t.submit(ex);
    await t.waitState(runId, 'awaitingApproval');
    ex.approve(runId);
    await vi.waitFor(() => expect(t.engine.enqueues).toBe(1)); // enqueue terjadi
    // proses baru dengan journal yang sama
    const restartedLedger = MemoryExecutionLedger.restart(dying.persisted());
    const ex2 = t.make({}, restartedLedger);
    const unresolved = await ex2.recover();
    expect(unresolved).toHaveLength(1);
    expect(unresolved[0].state).toBe('unknownOutcome');
    expect(t.engine.enqueues).toBe(1); // tidak ada replay
    // pengguna memeriksa queue dan menemukan job_1
    t.queue.set('job_1', { state: 'completed', processedFiles: 4 });
    const entry = await ex2.reconcile(unresolved[0].key, { kind: 'found', jobIds: ['job_1'] });
    expect(entry.state).toBe('enqueued');
    expect(await ex2.summaryOf(entry.executionId)).toMatchObject({
      outcome: 'completed',
      completedFiles: 4,
    });
    expect(await ex2.recover()).toEqual([]);
  });

  it('a ledger that cannot reserve fails closed without writing', async () => {
    const t = setup();
    const broken = new MemoryExecutionLedger();
    broken.reserve = async () => {
      throw new Error('disk full');
    };
    const ex = t.make({}, broken);
    const { runId } = t.submit(ex);
    await t.waitState(runId, 'awaitingApproval');
    ex.approve(runId);
    await t.waitState(runId, 'failed');
    expect(t.engine.archiveWrites).toBe(0);
  });

  it('listItems pages through the injected lister and falls back to the preview count', async () => {
    const t = setup();
    const { runId } = t.submit();
    await t.waitState(runId, 'awaitingApproval');
    expect(await t.executor.listItems(runId, 0, 10)).toEqual({ rows: [], total: 2 });
    const lister = vi.fn(async () => ({ rows: [{ ref: 'r1', label: 'a.txt' }], total: 2 }));
    const ex = t.make({ listItems: lister });
    const second = t.submit(ex);
    await t.waitState(second.runId, 'awaitingApproval');
    expect(await ex.listItems(second.runId, 0, 1)).toEqual({
      rows: [{ ref: 'r1', label: 'a.txt' }],
      total: 2,
    });
    expect(await ex.listItems('run-unknown', 0, 1)).toEqual({ rows: [], total: 0 });
  });
});

describe('AiExecutor: engine failure causes (LIMIT-5)', () => {
  for (const cause of ['permissionDenied', 'sourceUnreadable', 'diskFull', 'notFound', 'io']) {
    it(`execution_failed with cause ${cause} ends as failed with the cause, ledger closed, never replayed`, async () => {
      const t = setup();
      t.engine.archiveFailure = { cause };
      const { runId } = t.submit();
      await t.waitState(runId, 'awaitingApproval');
      const planId = t.lastOf(runId).plan!.planId;
      t.executor.approve(runId);
      await t.waitState(runId, 'failed');
      expect(t.lastOf(runId).errorMessage).toMatch(/^Operasi gagal: /);
      expect(JSON.stringify(t.updates)).not.toContain('secret');
      expect(t.updates.some((u) => u.runId === runId && u.state === 'unknownOutcome')).toBe(false);
      // Ledger: nothing left unresolved, and the same plan can never be reserved (replayed) again.
      expect(await t.executor.recover()).toEqual([]);
      expect((await t.ledger.reserve('exe_again', planId, 'dig')).kind).toBe('existing');
      t.executor.approve(runId);
      await new Promise((r) => setTimeout(r, 10));
      expect(t.engine.archiveWrites).toBe(0);
      assertLegal(t.updates, runId);
    });
  }

  it('the cause label is app text: each mapped cause has a distinct message', async () => {
    const { engineFailureMessage } = await import('./executor');
    const causes = [
      'permissionDenied',
      'sourceUnreadable',
      'diskFull',
      'notFound',
      'invalidPath',
      'notADirectory',
      'unsupported',
      'io',
    ];
    const msgs = causes.map((c) => engineFailureMessage(c));
    expect(msgs.every((m) => typeof m === 'string')).toBe(true);
    expect(new Set(msgs).size).toBe(msgs.length);
    expect(engineFailureMessage('toString')).toBeUndefined();
    expect(engineFailureMessage('cancelled')).toBeUndefined();
  });

  it('an unknown cause or a thrown error stays unknownOutcome (conservative)', async () => {
    for (const failure of [{ cause: 'weird_new_code' }, {}]) {
      const t = setup();
      t.engine.archiveFailure = failure;
      const { runId } = t.submit();
      await t.waitState(runId, 'awaitingApproval');
      t.executor.approve(runId);
      await t.waitState(runId, 'unknownOutcome');
      expect(await t.executor.recover()).toHaveLength(1);
    }
  });
});

describe('AiExecutor: checkResult on unknownOutcome (LIMIT-4)', () => {
  async function unknownSync(t: ReturnType<typeof setup>, ex = t.executor) {
    t.provider.script = (r) => ({ kind: 'intent', intent: syncIntent(r.requestId) });
    const { runId } = t.submit(ex);
    await t.waitState(runId, 'awaitingApproval');
    ex.approve(runId);
    return runId;
  }

  it('no receipt: reconciles the ledger as notFound, keeps the card honest, never replays', async () => {
    const t = setup();
    t.engine.throwOnEnqueue = true;
    const runId = await unknownSync(t);
    await t.waitState(runId, 'unknownOutcome');
    const reconcile = vi.spyOn(t.executor, 'reconcile');
    expect(await t.executor.recover()).toHaveLength(1);
    const res = await t.executor.checkResult(runId);
    expect(res).toMatchObject({ ok: true, state: 'unknownOutcome' });
    expect(res.message).toMatch(/tidak akan menjalankannya ulang/);
    expect(reconcile).toHaveBeenCalledWith(expect.stringMatching(/^idem_/), { kind: 'notFound' });
    expect(await t.executor.recover()).toEqual([]);
    expect(t.engine.enqueues).toBe(0);
    // A second check: still no replay and no ledger error.
    expect((await t.executor.checkResult(runId)).ok).toBe(true);
    expect(t.engine.enqueues).toBe(0);
  });

  it('jobs vanished from the queue: check reads the real engine state once they are visible again', async () => {
    const t = setup();
    const runId = await unknownSync(t);
    await vi.waitFor(() => expect(t.lastOf(runId).receipt?.jobIds).toEqual(['job_1']));
    t.queue.set('job_1', { state: 'running', processedFiles: 1 });
    await t.waitState(runId, 'running');
    t.queue.jobs.delete('job_1');
    (t.executor as unknown as { onProgress(p: { operationId: string }): void }).onProgress({
      operationId: 'job_1',
    });
    await t.waitState(runId, 'unknownOutcome');
    const pending = await t.executor.checkResult(runId);
    expect(pending).toMatchObject({ ok: true, state: 'unknownOutcome' });
    expect(pending.message).toMatch(/tidak ada di antrean/);
    t.queue.jobs.set('job_1', {
      operationId: 'job_1',
      kind: 'copy',
      state: 'completed',
      processedBytes: 0,
      processedFiles: 4,
    });
    const res = await t.executor.checkResult(runId);
    expect(res).toMatchObject({ ok: true, state: 'succeeded' });
    expect(t.stateOf(runId)).toBe('succeeded');
    expect(t.engine.enqueues).toBe(1);
    assertLegal(t.updates, runId);
  });

  it('a job still waiting in the queue resumes as running, never back to queued', async () => {
    const t = setup();
    const runId = await unknownSync(t);
    await vi.waitFor(() => expect(t.lastOf(runId).receipt?.jobIds).toEqual(['job_1']));
    t.queue.set('job_1', { state: 'running' });
    t.queue.jobs.delete('job_1');
    (t.executor as unknown as { onProgress(p: { operationId: string }): void }).onProgress({
      operationId: 'job_1',
    });
    await t.waitState(runId, 'unknownOutcome');
    t.queue.jobs.set('job_1', {
      operationId: 'job_1',
      kind: 'copy',
      state: 'queued',
      processedBytes: 0,
      processedFiles: 0,
    });
    const res = await t.executor.checkResult(runId);
    expect(res).toMatchObject({ ok: true, state: 'running' });
    t.queue.set('job_1', { state: 'completed', processedFiles: 2 });
    await t.waitState(runId, 'succeeded');
    assertLegal(t.updates, runId);
  });

  it('a ledger left in reserved (markUnknown failed) is marked unknown and reconciled', async () => {
    const t = setup();
    t.engine.throwOnEnqueue = true;
    const led = new MemoryExecutionLedger();
    const flaky = Object.create(led) as MemoryExecutionLedger;
    flaky.markUnknown = async () => {
      throw new Error('journal busy');
    };
    const ex = t.make({}, flaky);
    const runId = await unknownSync(t, ex);
    await t.waitState(runId, 'unknownOutcome');
    expect(led.persisted().entries[0].state).toBe('reserved');
    flaky.markUnknown = (key) => led.markUnknown(key);
    const res = await ex.checkResult(runId);
    expect(res.ok).toBe(true);
    expect(led.persisted().entries[0].state).toBe('notStarted');
  });

  it('double click: a second check while one is running does not reconcile twice', async () => {
    const t = setup();
    t.engine.throwOnEnqueue = true;
    const runId = await unknownSync(t);
    await t.waitState(runId, 'unknownOutcome');
    const spy = vi.spyOn(t.ledger, 'resolveUnknown');
    const [a, b] = await Promise.all([
      t.executor.checkResult(runId),
      t.executor.checkResult(runId),
    ]);
    expect([a.ok, b.ok].sort()).toEqual([false, true]);
    expect(spy).toHaveBeenCalledTimes(1);
  });

  it('refuses runs that are not unknownOutcome or unknown to this executor', async () => {
    const t = setup();
    const { runId } = t.submit();
    await t.waitState(runId, 'awaitingApproval');
    expect((await t.executor.checkResult(runId)).ok).toBe(false);
    expect((await t.executor.checkResult('run-from-old-runtime')).ok).toBe(false);
  });

  it('a ledger read error keeps the result unknown', async () => {
    const t = setup();
    t.engine.throwOnEnqueue = true;
    const runId = await unknownSync(t);
    await t.waitState(runId, 'unknownOutcome');
    t.ledger.byExecution = async () => {
      throw new Error('ipc down');
    };
    const res = await t.executor.checkResult(runId);
    expect(res.ok).toBe(false);
    expect(t.stateOf(runId)).toBe('unknownOutcome');
  });
});
