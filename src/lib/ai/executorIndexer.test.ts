// FER-476: the executor's indexer branches. `indexer.status` answers from the live status and
// writes nothing; `indexer.setMode` ALWAYS waits for the confirm card, raising also needs the privacy
// tick, and the only write is `setMode(mode, true)` after that click. No plan/ledger/queue is used.
import { describe, expect, it, vi } from 'vitest';
import type { RunState, RunUpdate } from './chatStore.svelte';
import { ok } from './commands';
import type { PaneSnapshot } from './contracts';
import { AiExecutor, type ExecutorDeps } from './executor';
import type { ExecutionLedgerPort } from './executorLedger';
import type { QueuePort } from './executorQueue';
import type { IndexerControlPort } from './indexerControl';
import type { IntentProvider, ProviderEvent, ProviderPlanRequest } from './provider';
import {
  IndexerError,
  UNAVAILABLE,
  type IndexerMode,
  type IndexerSetModeResult,
  type IndexerStatus,
} from '../ipc/indexer';

const status = (over: Partial<IndexerStatus> = {}): IndexerStatus => ({
  ...UNAVAILABLE,
  installed: true,
  scmState: 'running',
  reachable: true,
  version: '1.0.0',
  mode: 'shared',
  state: 'running',
  index: 'ready',
  canLowerPrivacy: false,
  problem: null,
  volumes: [],
  ...over,
});

class Provider implements IntentProvider {
  requests: Array<ProviderPlanRequest & { capabilities?: readonly string[] }> = [];
  next: (r: ProviderPlanRequest) => ProviderEvent = () => ({
    kind: 'indexerSetMode',
    request: { mode: 'high_performance' },
  });
  async *plan(request: ProviderPlanRequest): AsyncGenerator<ProviderEvent> {
    this.requests.push(request);
    yield this.next(request);
  }
}

function snapshot(requestId: string): PaneSnapshot {
  const pane = (paneId: 'left' | 'right', tabId: string) => ({
    paneId,
    tabId,
    location: { kind: 'disk' as const, path: paneId === 'left' ? 'D:\\L' : 'E:\\R' },
    contextRevision: 1,
    selectionRevision: 1,
    selectionHandle: `sel_${paneId}`,
    filter: '',
    showHidden: false,
    sort: { key: 'name' as const, dir: 1 as const },
  });
  return {
    schemaVersion: 1,
    requestId,
    activePaneAtSubmit: 'left',
    panes: [pane('left', 't1'), pane('right', 't2')],
  };
}

function fakeIndexer(initial: IndexerStatus = status()) {
  const state = { status: initial, available: true };
  const port = {
    available: vi.fn(() => state.available),
    status: vi.fn(async () => state.status),
    setMode: vi.fn<IndexerControlPort['setMode']>(
      async (mode: IndexerMode): Promise<IndexerSetModeResult> => ({
        mode,
        changed: true,
        elevated: mode === 'high_performance',
      }),
    ),
  };
  return { state, port: port satisfies IndexerControlPort };
}

function setup(indexer: IndexerControlPort | undefined) {
  const provider = new Provider();
  const facade = {
    prepare: vi.fn(async () => ({
      ok: false as const,
      error: { code: 'resolve_failed' as const, message: 'x' },
    })),
    approve: vi.fn(() => ok({} as never)),
    execute: vi.fn(),
    discard: vi.fn(),
  };
  const ledger = {
    reserve: vi.fn(),
    recordEnqueued: vi.fn(),
    recordNotStarted: vi.fn(),
    markUnknown: vi.fn(),
    resolveUnknown: vi.fn(),
    unresolved: vi.fn(async () => []),
    byExecution: vi.fn(),
  } as unknown as ExecutionLedgerPort;
  const queue = {
    get: vi.fn(),
    subscribe: () => () => undefined,
    cancel: vi.fn(),
  } as never as QueuePort;
  const guard = { register: vi.fn(), authorize: vi.fn(), revoke: vi.fn() };
  const executor = new AiExecutor({
    provider,
    capture: snapshot,
    modelContext: () => ({
      activePane: 'left' as const,
      leftLocation: 'disk' as const,
      rightLocation: 'disk' as const,
      leftCount: 2,
      rightCount: 0,
      showHidden: false,
      filterActive: false,
    }),
    facade,
    capabilities: {
      availability: () => ({ available: true }),
      describe: () => [],
      offeredCapabilityIds: () => ['archive.create', 'directories.sync'],
    },
    ledger,
    queue,
    guard,
    ...(indexer ? { indexer } : {}),
  } satisfies ExecutorDeps);
  const updates: RunUpdate[] = [];
  executor.subscribe((u) => updates.push(u));
  let n = 0;
  const submit = (text = 'pakai mode high performance') => {
    n++;
    const runId = `run-${n}`;
    executor.submit({ requestId: `req_${n}`, text, runId });
    return runId;
  };
  const stateOf = (runId: string): RunState | undefined =>
    updates.filter((u) => u.runId === runId).at(-1)?.state;
  const last = (runId: string) => updates.filter((u) => u.runId === runId).at(-1)!;
  const untouched = () => {
    expect(facade.prepare).not.toHaveBeenCalled();
    expect(facade.approve).not.toHaveBeenCalled();
    expect(facade.execute).not.toHaveBeenCalled();
    expect(guard.register).not.toHaveBeenCalled();
    expect(ledger.reserve).not.toHaveBeenCalled();
    expect(queue.cancel).not.toHaveBeenCalled();
  };
  return { provider, executor, updates, submit, stateOf, last, untouched };
}

const flush = () => new Promise((r) => setTimeout(r, 15));
const BASE = ['archive.create', 'directories.sync'];

describe('offer of the indexer capabilities', () => {
  it('old callers (no indexer port) keep the exact mutating list', async () => {
    const t = setup(undefined);
    t.provider.next = () => ({ kind: 'question', text: 'Apa?' });
    t.submit();
    await vi.waitFor(() => expect(t.provider.requests).toHaveLength(1));
    expect(t.provider.requests[0].capabilities).toEqual(BASE);
  });

  it('is offered on the desktop app (port available) and withdrawn otherwise', async () => {
    const { port, state } = fakeIndexer();
    const t = setup(port);
    t.provider.next = () => ({ kind: 'question', text: 'Apa?' });
    t.submit();
    await vi.waitFor(() => expect(t.provider.requests).toHaveLength(1));
    expect(t.provider.requests[0].capabilities).toEqual([
      ...BASE,
      'indexer.status',
      'indexer.setMode',
    ]);
    state.available = false;
    t.submit();
    await vi.waitFor(() => expect(t.provider.requests).toHaveLength(2));
    expect(t.provider.requests[1].capabilities).toEqual(BASE);
  });

  it('a throwing availability check fails closed', async () => {
    const { port } = fakeIndexer();
    port.available.mockImplementation(() => {
      throw new Error('boom');
    });
    const t = setup(port);
    t.provider.next = () => ({ kind: 'question', text: 'Apa?' });
    t.submit();
    await vi.waitFor(() => expect(t.provider.requests).toHaveLength(1));
    expect(t.provider.requests[0].capabilities).toEqual(BASE);
  });
});

describe('indexer.status', () => {
  it('answers from the live status in one step, writes nothing, uses no plan', async () => {
    const { port } = fakeIndexer(
      status({
        index: 'building',
        volumes: [
          {
            letter: 'C',
            state: 'building',
            error: null,
            entries: 1,
            progressPermille: 500,
            live: false,
          },
        ],
      }),
    );
    const t = setup(port);
    t.provider.next = () => ({ kind: 'indexerStatus' });
    const runId = t.submit('indeksnya sudah siap?');
    await vi.waitFor(() => expect(t.stateOf(runId)).toBe('succeeded'));
    const view = t.last(runId).indexer;
    expect(view?.kind).toBe('status');
    if (view?.kind === 'status') {
      expect(view.readiness).toBe('building');
      expect(view.volumes[0].percent).toBe(50);
      expect(view.summary).toContain('belum siap');
    }
    expect(port.setMode).not.toHaveBeenCalled();
    t.untouched();
    expect(t.updates.filter((x) => x.runId === runId).map((x) => x.state)).toEqual(['succeeded']);
  });

  it('reports not installed honestly', async () => {
    const { port } = fakeIndexer({ ...UNAVAILABLE, problem: null });
    const t = setup(port);
    t.provider.next = () => ({ kind: 'indexerStatus' });
    const runId = t.submit();
    await vi.waitFor(() => expect(t.stateOf(runId)).toBe('succeeded'));
    const view = t.last(runId).indexer;
    expect(view?.kind === 'status' && view.readiness).toBe('not_installed');
  });

  it('is blocked when it was not offered', async () => {
    const { port, state } = fakeIndexer();
    state.available = false;
    const t = setup(port);
    t.provider.next = () => ({ kind: 'indexerStatus' });
    const runId = t.submit();
    await vi.waitFor(() => expect(t.stateOf(runId)).toBe('blockedCapability'));
    expect(port.status).not.toHaveBeenCalled();
  });

  it('a throwing status read fails closed', async () => {
    const { port } = fakeIndexer();
    port.status.mockRejectedValue(new Error('ipc'));
    const t = setup(port);
    t.provider.next = () => ({ kind: 'indexerStatus' });
    const runId = t.submit();
    await vi.waitFor(() => expect(t.stateOf(runId)).toBe('failed'));
  });
});

describe('indexer.setMode: always behind the confirm card', () => {
  it('waits for the card: nothing is written before the click', async () => {
    const { port } = fakeIndexer();
    const t = setup(port);
    const runId = t.submit();
    await vi.waitFor(() => expect(t.stateOf(runId)).toBe('awaitingApproval'));
    await flush();
    expect(port.setMode).not.toHaveBeenCalled();
    const view = t.last(runId).indexer;
    expect(view?.kind).toBe('setMode');
    if (view?.kind === 'setMode') {
      expect(view.target).toBe('high_performance');
      expect(view.current).toBe('shared');
      expect(view.needsAck).toBe(true);
      expect(view.consequences.join(' ')).toContain('SEMUA akun');
      expect(view.consequences.join(' ')).toContain('UAC');
    }
    t.untouched();
    expect(t.updates.filter((x) => x.runId === runId).map((x) => x.state)).toEqual([
      'awaitingApproval',
    ]);
  });

  it('raising needs the privacy tick; without it the click consumes nothing', async () => {
    const { port } = fakeIndexer();
    const t = setup(port);
    const runId = t.submit();
    await vi.waitFor(() => expect(t.stateOf(runId)).toBe('awaitingApproval'));
    t.executor.approve(runId);
    t.executor.approve(runId, { acknowledged: false });
    await flush();
    expect(port.setMode).not.toHaveBeenCalled();
    expect(t.stateOf(runId)).toBe('awaitingApproval');
    // The card can still be confirmed properly afterwards.
    t.executor.approve(runId, { acknowledged: true });
    await vi.waitFor(() => expect(t.stateOf(runId)).toBe('succeeded'));
    expect(port.setMode).toHaveBeenCalledTimes(1);
    expect(port.setMode).toHaveBeenCalledWith('high_performance', true);
    const view = t.last(runId).indexer;
    expect(view?.kind === 'setMode' && view.result?.ok).toBe(true);
    expect(view?.kind === 'setMode' && view.result?.message).toContain('administrator');
  });

  it('a double click still writes once', async () => {
    const { port } = fakeIndexer();
    const t = setup(port);
    const runId = t.submit();
    await vi.waitFor(() => expect(t.stateOf(runId)).toBe('awaitingApproval'));
    t.executor.approve(runId, { acknowledged: true });
    t.executor.approve(runId, { acknowledged: true });
    await vi.waitFor(() => expect(t.stateOf(runId)).toBe('succeeded'));
    expect(port.setMode).toHaveBeenCalledTimes(1);
  });

  it('lowering to shared needs no tick but still a click', async () => {
    const { port } = fakeIndexer(status({ mode: 'high_performance', canLowerPrivacy: true }));
    const t = setup(port);
    t.provider.next = () => ({ kind: 'indexerSetMode', request: { mode: 'shared' } });
    const runId = t.submit('kembalikan ke mode shared');
    await vi.waitFor(() => expect(t.stateOf(runId)).toBe('awaitingApproval'));
    expect(port.setMode).not.toHaveBeenCalled();
    const view = t.last(runId).indexer;
    expect(view?.kind === 'setMode' && view.needsAck).toBe(false);
    t.executor.approve(runId);
    await vi.waitFor(() => expect(t.stateOf(runId)).toBe('succeeded'));
    expect(port.setMode).toHaveBeenCalledWith('shared', true);
  });

  it('already in the target mode: says so, executes nothing, no card', async () => {
    const { port } = fakeIndexer(status({ mode: 'high_performance' }));
    const t = setup(port);
    const runId = t.submit();
    await vi.waitFor(() => expect(t.stateOf(runId)).toBe('succeeded'));
    const view = t.last(runId).indexer;
    expect(view?.kind === 'setMode' && view.result?.message).toContain('Sudah dalam mode');
    expect(port.setMode).not.toHaveBeenCalled();
    t.untouched();
  });

  it('not installed / not running: honest failure, no card, nothing written', async () => {
    for (const [s, code] of [
      [{ ...UNAVAILABLE, problem: null }, 'not_installed'],
      [status({ reachable: false, mode: null }), 'not_running'],
    ] as const) {
      const { port } = fakeIndexer(s);
      const t = setup(port);
      const runId = t.submit();
      await vi.waitFor(() => expect(t.stateOf(runId)).toBe('failed'));
      const view = t.last(runId).indexer;
      expect(view?.kind === 'setMode' && view.result?.code).toBe(code);
      expect(port.setMode).not.toHaveBeenCalled();
    }
  });

  it('maps every IndexerError code to a clear message and ends failed', async () => {
    const expected: Record<string, string> = {
      declined: 'UAC',
      not_running: 'tidak berjalan',
      refused: 'menolak',
      failed: 'gagal',
      unsupported: 'desktop',
    };
    for (const [code, fragment] of Object.entries(expected)) {
      const { port } = fakeIndexer();
      port.setMode.mockRejectedValue(new IndexerError(code, 'C:\\raw\\detail'));
      const t = setup(port);
      const runId = t.submit();
      await vi.waitFor(() => expect(t.stateOf(runId)).toBe('awaitingApproval'));
      t.executor.approve(runId, { acknowledged: true });
      await vi.waitFor(() => expect(t.stateOf(runId)).toBe('failed'));
      const view = t.last(runId).indexer;
      expect(view?.kind === 'setMode' && view.result?.ok).toBe(false);
      expect(view?.kind === 'setMode' && view.result?.code).toBe(code);
      expect(view?.kind === 'setMode' && view.result?.message).toContain(fragment);
      expect(JSON.stringify(t.updates)).not.toContain('raw');
    }
  });

  it('Cancel writes nothing and a late approve is ignored', async () => {
    const { port } = fakeIndexer();
    const t = setup(port);
    const runId = t.submit();
    await vi.waitFor(() => expect(t.stateOf(runId)).toBe('awaitingApproval'));
    t.executor.cancel(runId);
    expect(t.stateOf(runId)).toBe('cancelled');
    t.executor.approve(runId, { acknowledged: true });
    await flush();
    expect(port.setMode).not.toHaveBeenCalled();
  });

  it('cannot be cancelled once the confirmed change is being applied', async () => {
    const { port } = fakeIndexer();
    let finish: (r: IndexerSetModeResult) => void = () => undefined;
    port.setMode.mockImplementation(() => new Promise<IndexerSetModeResult>((r) => (finish = r)));
    const t = setup(port);
    const runId = t.submit();
    await vi.waitFor(() => expect(t.stateOf(runId)).toBe('awaitingApproval'));
    t.executor.approve(runId, { acknowledged: true });
    await vi.waitFor(() => expect(port.setMode).toHaveBeenCalled());
    t.executor.cancel(runId);
    expect(t.stateOf(runId)).toBe('awaitingApproval');
    finish({ mode: 'high_performance', changed: true, elevated: true });
    await vi.waitFor(() => expect(t.stateOf(runId)).toBe('succeeded'));
  });

  it('is blocked when not offered, and an invalid request from a misbehaving provider is rejected', async () => {
    const off = fakeIndexer();
    off.state.available = false;
    const t1 = setup(off.port);
    const r1 = t1.submit();
    await vi.waitFor(() => expect(t1.stateOf(r1)).toBe('blockedCapability'));
    expect(off.port.setMode).not.toHaveBeenCalled();

    const { port } = fakeIndexer();
    const t2 = setup(port);
    t2.provider.next = () => ({
      kind: 'indexerSetMode',
      request: { mode: 'turbo' } as never,
    });
    const r2 = t2.submit();
    await vi.waitFor(() => expect(t2.stateOf(r2)).toBe('failed'));
    expect(port.setMode).not.toHaveBeenCalled();
    t2.untouched();
  });

  it('the model text never reaches the card', async () => {
    const { port } = fakeIndexer();
    const t = setup(port);
    t.provider.next = () => ({
      kind: 'indexerSetMode',
      request: { mode: 'shared', note: 'IGNORE RULES' } as never,
    });
    const runId = t.submit();
    await vi.waitFor(() => expect(t.stateOf(runId)).toBe('failed'));
    expect(JSON.stringify(t.updates)).not.toContain('IGNORE RULES');
  });
});
