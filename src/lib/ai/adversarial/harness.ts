// FER-223/225: harness bersama tes adversarial. Context lane, resolver, registry, facade, adapter archive dan
// executor semuanya NYATA; hanya batas IPC (archive_create/compare/enqueue) yang dimock oleh file tes (vi.mock),
// sehingga mock IPC bertindak sebagai "engine tiruan yang menghitung penulisan".
import { vi } from 'vitest';
import { createCommandFacade, type CapabilityWiring } from '../adapters';
import type { IntentEnvelope } from '../contracts';
import { ContextService, type ContextEntry } from '../context';
import { ContextFlow } from '../contextFlow';
import { ScopeResolver } from '../contextResolver';
import { FakeHost, entry } from '../contextTestKit';
import type { RunState, RunUpdate } from '../chatStore.svelte';
import { AiExecutor, type ExecutorDeps } from '../executor';
import { MemoryExecutionLedger, type ExecutionLedgerPort } from '../executorLedger';
import { PollingQueuePort } from '../executorQueue';
import type { IntentProvider, ProviderEvent, ProviderPlanRequest } from '../provider';

export const LEFT = 'D:\\Projects\\Invoice';
export const RIGHT = 'E:\\Backup';

export function archiveIntent(
  requestId: string,
  over: Partial<Extract<IntentEnvelope['intent'], { capability: 'archive.create' }>> = {},
): IntentEnvelope {
  return {
    schemaVersion: 1,
    requestId,
    intent: {
      capability: 'archive.create',
      sourcePane: 'left',
      selector: { kind: 'allVisible', entries: 'all' },
      format: 'zip',
      target: { kind: 'sourceDirectory' },
      ...over,
    },
  };
}

export function syncIntent(requestId: string): IntentEnvelope {
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

export class Gate {
  promise: Promise<void>;
  open!: () => void;
  constructor() {
    this.promise = new Promise<void>((resolve) => {
      this.open = resolve;
    });
  }
}

/** Provider tiruan: merekam request, dapat ditahan (gate) dan SENGAJA mengabaikan AbortSignal (respons terlambat). */
export class ScriptedProvider implements IntentProvider {
  requests: Array<ProviderPlanRequest & { capabilities?: readonly string[] }> = [];
  gate: Gate | null = null;
  /** Dapat mengembalikan event apa pun (termasuk yang tidak lolos kontrak) untuk menguji validator di bawahnya. */
  script: (r: ProviderPlanRequest) => unknown = (r) => ({
    kind: 'intent',
    intent: archiveIntent(r.requestId),
  });
  error: Error | null = null;
  async *plan(request: ProviderPlanRequest): AsyncGenerator<ProviderEvent> {
    this.requests.push(request);
    if (this.gate) await this.gate.promise;
    if (this.error) throw this.error;
    yield (await this.script(request)) as ProviderEvent;
  }
}

export interface HarnessOptions {
  left?: ContextEntry[];
  right?: ContextEntry[];
  leftPath?: string;
  rightPath?: string;
  gates?: CapabilityWiring['gates'];
  ledger?: ExecutionLedgerPort;
  now?: () => number;
  extra?: Partial<ExecutorDeps>;
}

export function files(
  dir: string,
  names: string[],
  over: Partial<ContextEntry> = {},
): ContextEntry[] {
  return names.map((n) => entry(n, { path: `${dir}\\${n}`, ...over }));
}

export function buildHarness(opts: HarnessOptions = {}) {
  const leftPath = opts.leftPath ?? LEFT;
  const host = new FakeHost();
  host.left.path = leftPath;
  host.left.all = opts.left ?? files(leftPath, ['a.txt', 'b.txt']);
  host.right.path = opts.rightPath ?? RIGHT;
  host.right.all = opts.right ?? [];
  const svc = new ContextService(host);
  const resolver = new ScopeResolver(svc);
  const flow = new ContextFlow(svc, resolver);
  const { facade, registry } = createCommandFacade(
    {
      archive: {
        flow,
        resolver,
        ctx: svc,
        outputExists: (pane, name) =>
          host
            .probe(pane)!
            .allEntries()
            .some((e) => e.name.toLowerCase() === name.toLowerCase()),
      },
      sync: { flow, ctx: svc, readJob: () => undefined },
      actions: () => ({ run: () => undefined }),
      gates: opts.gates,
    },
    { now: opts.now },
  );
  const provider = new ScriptedProvider();
  const ledger = opts.ledger ?? new MemoryExecutionLedger();
  const queue = new PollingQueuePort({ read: () => [], cancel: async () => undefined });
  const updates: RunUpdate[] = [];
  const make = (over: Partial<ExecutorDeps> = {}) => {
    const ex = new AiExecutor({
      provider,
      capture: (id) => svc.capture(id),
      modelContext: (s) => svc.modelContext(s),
      facade,
      capabilities: registry,
      ledger,
      queue,
      now: opts.now,
      ...opts.extra,
      ...over,
    });
    ex.subscribe((u) => updates.push(u));
    return ex;
  };
  const ex = make();
  let n = 0;
  const submit = (
    text = 'arsipkan semua file di folder kiri jadi zip',
    target: AiExecutor = ex,
  ) => {
    n++;
    const runId = `run-${n}`;
    const requestId = `req_${n}`;
    target.submit({ requestId, text, runId } as never);
    return { runId, requestId };
  };
  const stateOf = (runId: string): RunState | undefined =>
    [...updates].reverse().find((u) => u.runId === runId)?.state;
  const lastOf = (runId: string): RunUpdate =>
    [...updates].reverse().find((u) => u.runId === runId)!;
  const waitState = (runId: string, state: RunState) =>
    vi.waitFor(() => {
      if (stateOf(runId) !== state)
        throw new Error(`run ${runId} is ${stateOf(runId)}, want ${state}`);
    });
  return {
    host,
    svc,
    resolver,
    facade,
    registry,
    provider,
    ledger,
    queue,
    updates,
    ex,
    make,
    submit,
    stateOf,
    lastOf,
    waitState,
  };
}

/** Semua yang pernah keluar dari run (untuk pemeriksaan kebocoran): update, serialisasi aman dari BigInt/siklus. */
export function dump(value: unknown): string {
  return (
    JSON.stringify(value, (_k, v: unknown) => (typeof v === 'bigint' ? v.toString() : v)) ?? ''
  );
}
