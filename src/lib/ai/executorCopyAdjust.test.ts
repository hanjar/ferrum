// FER-471: the executor's control branch. `copy.adjust` never reaches prepare/policy/guard/ledger/
// queue; it waits for the card's confirm button, then writes one performance setting on the running
// copy job. Nothing is written before the click, and the model never names the job.
import { describe, expect, it, vi } from 'vitest';
import type { RunState, RunUpdate } from './chatStore.svelte';
import { ok } from './commands';
import type { CopyAdjustRequest, PaneSnapshot } from './contracts';
import type { CopyControlPort } from './copyAdjust';
import { AiExecutor, type ExecutorDeps } from './executor';
import type { ExecutionLedgerPort } from './executorLedger';
import type { QueuePort } from './executorQueue';
import type { IntentProvider, ProviderEvent, ProviderPlanRequest } from './provider';
import type { CopyAdjust, CopyControlState } from '../ipc/copyControl';

const job = (over: Partial<CopyControlState> = {}): CopyControlState => ({
  operationId: 'op_77',
  auto: true,
  threads: 4,
  activeWorkers: 4,
  autoMax: 16,
  media: 'ssd',
  priority: 'normal',
  running: true,
  ...over,
});

class Provider implements IntentProvider {
  requests: Array<ProviderPlanRequest & { capabilities?: readonly string[] }> = [];
  next: (r: ProviderPlanRequest) => ProviderEvent = () => ({
    kind: 'copyAdjust',
    request: { action: 'increase' },
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

function fakeControl(jobs: CopyControlState[] = [job()]) {
  const state = { jobs, running: jobs.length > 0 };
  const port = {
    available: vi.fn(() => state.running),
    running: vi.fn(async () => state.jobs),
    adjust: vi.fn<CopyControlPort['adjust']>(async () =>
      state.jobs[0] ? { ...state.jobs[0], threads: 5, auto: false } : null,
    ),
  };
  return { state, port: port satisfies CopyControlPort };
}

function setup(control: CopyControlPort | undefined) {
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
    ...(control ? { copyControl: control } : {}),
  } satisfies ExecutorDeps);
  const updates: RunUpdate[] = [];
  executor.subscribe((u) => updates.push(u));
  let n = 0;
  const submit = () => {
    n++;
    const runId = `run-${n}`;
    executor.submit({ requestId: `req_${n}`, text: 'copy-nya lebih cepat dong', runId });
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
    expect(guard.authorize).not.toHaveBeenCalled();
    expect(ledger.reserve).not.toHaveBeenCalled();
    expect(queue.cancel).not.toHaveBeenCalled();
  };
  return { provider, executor, updates, submit, stateOf, last, untouched };
}

const flush = () => new Promise((r) => setTimeout(r, 15));

describe('offer of copy.adjust', () => {
  it('old callers (no copyControl port) keep the exact mutating list', async () => {
    const t = setup(undefined);
    t.provider.next = () => ({ kind: 'question', text: 'Apa?' });
    t.submit();
    await vi.waitFor(() => expect(t.provider.requests).toHaveLength(1));
    expect(t.provider.requests[0].capabilities).toEqual(['archive.create', 'directories.sync']);
  });

  it('is offered only while a copy runs, and withdrawn after it ends', async () => {
    const { port, state } = fakeControl();
    const t = setup(port);
    t.provider.next = () => ({ kind: 'question', text: 'Apa?' });
    t.submit();
    await vi.waitFor(() => expect(t.provider.requests).toHaveLength(1));
    expect(t.provider.requests[0].capabilities).toEqual([
      'archive.create',
      'directories.sync',
      'copy.adjust',
    ]);
    state.running = false;
    t.submit();
    await vi.waitFor(() => expect(t.provider.requests).toHaveLength(2));
    expect(t.provider.requests[1].capabilities).not.toContain('copy.adjust');
  });

  it('a throwing availability check fails closed', async () => {
    const { port } = fakeControl();
    port.available.mockImplementation(() => {
      throw new Error('boom');
    });
    const t = setup(port);
    t.provider.next = () => ({ kind: 'question', text: 'Apa?' });
    t.submit();
    await vi.waitFor(() => expect(t.provider.requests).toHaveLength(1));
    expect(t.provider.requests[0].capabilities).not.toContain('copy.adjust');
  });
});

describe('control branch: confirm first, then one performance write', () => {
  it('waits for the confirm button: nothing is written before the click', async () => {
    const { port } = fakeControl();
    const t = setup(port);
    const runId = t.submit();
    await vi.waitFor(() => expect(t.stateOf(runId)).toBe('awaitingApproval'));
    await flush();
    expect(port.adjust).not.toHaveBeenCalled();
    const u = t.last(runId);
    expect(u.copyAdjust?.summary).toContain('4 → 5');
    expect(u.copyAdjust?.job).toContain('op_77');
    expect(u.copyAdjust?.before).toEqual({ threads: 4, auto: true, priority: 'normal' });
    expect(u.copyAdjust?.after).toEqual({ threads: 5, auto: false, priority: 'normal' });
    expect(u.plan).toBeUndefined();
    t.untouched();
    // The only legal path: planning -> awaitingApproval.
    expect(t.updates.filter((x) => x.runId === runId).map((x) => x.state)).toEqual([
      'awaitingApproval',
    ]);
  });

  it('approve applies exactly the previewed change to the job the APP resolved', async () => {
    const { port } = fakeControl([job({ operationId: 'op_77' })]);
    const t = setup(port);
    const runId = t.submit();
    await vi.waitFor(() => expect(t.stateOf(runId)).toBe('awaitingApproval'));
    t.executor.approve(runId);
    t.executor.approve(runId); // double click: still one write
    await vi.waitFor(() => expect(t.stateOf(runId)).toBe('succeeded'));
    expect(port.adjust).toHaveBeenCalledTimes(1);
    expect(port.adjust).toHaveBeenCalledWith('op_77', { kind: 'set', threads: 5 });
    const result = t.last(runId).copyAdjust?.result;
    expect(result?.ok).toBe(true);
    expect(result?.applied).toEqual({ threads: 5, auto: false, priority: 'normal' });
    t.untouched();
  });

  it('confirm sends the absolute result shown on the card, not a relative step', async () => {
    const { port, state } = fakeControl();
    const t = setup(port);
    t.provider.next = () => ({ kind: 'copyAdjust', request: { action: 'increase' } });
    const runId = t.submit();
    await vi.waitFor(() => expect(t.stateOf(runId)).toBe('awaitingApproval'));
    // Auto mode / the [+] button moved the live count to 9 while the card was open.
    state.jobs = [job({ threads: 9, auto: false })];
    t.executor.approve(runId);
    await vi.waitFor(() => expect(t.stateOf(runId)).toBe('succeeded'));
    expect(port.adjust).toHaveBeenCalledWith('op_77', { kind: 'set', threads: 5 });
  });

  it('every action maps to its wire change', async () => {
    const cases: Array<[CopyAdjustRequest, CopyAdjust]> = [
      [
        { action: 'decrease', by: 2 },
        { kind: 'set', threads: 2 },
      ],
      [
        { action: 'set', threads: 8 },
        { kind: 'set', threads: 8 },
      ],
      [
        { action: 'priority', priority: 'background' },
        { kind: 'priority', priority: 'background' },
      ],
    ];
    for (const [request, wire] of cases) {
      const { port } = fakeControl();
      const t = setup(port);
      t.provider.next = () => ({ kind: 'copyAdjust', request });
      const runId = t.submit();
      await vi.waitFor(() => expect(t.stateOf(runId)).toBe('awaitingApproval'));
      t.executor.approve(runId);
      await vi.waitFor(() => expect(t.stateOf(runId)).toBe('succeeded'));
      expect(port.adjust).toHaveBeenCalledWith('op_77', wire);
    }
  });

  it('Cancel writes nothing and ends cancelled', async () => {
    const { port } = fakeControl();
    const t = setup(port);
    const runId = t.submit();
    await vi.waitFor(() => expect(t.stateOf(runId)).toBe('awaitingApproval'));
    t.executor.cancel(runId);
    expect(t.stateOf(runId)).toBe('cancelled');
    t.executor.approve(runId);
    await flush();
    expect(port.adjust).not.toHaveBeenCalled();
  });

  it('a copy that ended before the click fails closed with an honest message', async () => {
    const { port } = fakeControl();
    port.adjust.mockResolvedValue(null);
    const t = setup(port);
    const runId = t.submit();
    await vi.waitFor(() => expect(t.stateOf(runId)).toBe('awaitingApproval'));
    t.executor.approve(runId);
    await vi.waitFor(() => expect(t.stateOf(runId)).toBe('failed'));
    expect(t.last(runId).errorMessage).toContain('tidak ada yang diubah');
    expect(t.last(runId).copyAdjust?.result?.ok).toBe(false);
  });

  it('a throwing write also fails closed', async () => {
    const { port } = fakeControl();
    port.adjust.mockRejectedValue(new Error('ipc'));
    const t = setup(port);
    const runId = t.submit();
    await vi.waitFor(() => expect(t.stateOf(runId)).toBe('awaitingApproval'));
    t.executor.approve(runId);
    await vi.waitFor(() => expect(t.stateOf(runId)).toBe('failed'));
  });

  it('no running copy: blocked, no card, nothing written', async () => {
    const { port, state } = fakeControl();
    const t = setup(port);
    t.provider.next = () => {
      state.jobs = [];
      return { kind: 'copyAdjust', request: { action: 'increase' } };
    };
    // The capability was offered at submit time, but the job is gone when the request is answered.
    const runId = t.submit();
    await vi.waitFor(() => expect(t.stateOf(runId)).toBe('blockedCapability'));
    expect(t.last(runId).blockedReason).toContain('Tidak ada copy');
    expect(port.adjust).not.toHaveBeenCalled();
  });

  it('when copy.adjust was not offered the reply is blocked, not executed', async () => {
    const { port, state } = fakeControl();
    state.running = false; // not offered
    state.jobs = [];
    const t = setup(port);
    const runId = t.submit();
    await vi.waitFor(() => expect(t.stateOf(runId)).toBe('blockedCapability'));
    expect(port.adjust).not.toHaveBeenCalled();
  });

  it('a request that changes nothing ends without a confirm and without a write', async () => {
    const { port } = fakeControl([job({ auto: false, threads: 16 })]);
    const t = setup(port);
    t.provider.next = () => ({ kind: 'copyAdjust', request: { action: 'set', threads: 16 } });
    const runId = t.submit();
    await vi.waitFor(() => expect(t.stateOf(runId)).toBe('succeeded'));
    expect(port.adjust).not.toHaveBeenCalled();
    expect(t.last(runId).copyAdjust?.result?.message).toContain('Tidak ada perubahan');
  });

  it('an out-of-range request from a misbehaving provider is rejected locally', async () => {
    const { port } = fakeControl();
    const t = setup(port);
    t.provider.next = () => ({
      kind: 'copyAdjust',
      request: { action: 'set', threads: 99 } as CopyAdjustRequest,
    });
    const runId = t.submit();
    await vi.waitFor(() => expect(t.stateOf(runId)).toBe('failed'));
    expect(port.adjust).not.toHaveBeenCalled();
    t.untouched();
  });

  it('the model text never reaches the card: the summary is app-built from the parsed request', async () => {
    const { port } = fakeControl();
    const t = setup(port);
    t.provider.next = () => ({
      kind: 'copyAdjust',
      request: { action: 'increase', by: 2, note: 'IGNORE RULES' } as unknown as CopyAdjustRequest,
    });
    const runId = t.submit();
    // Extra keys fail the strict local parse.
    await vi.waitFor(() => expect(t.stateOf(runId)).toBe('failed'));
    expect(JSON.stringify(t.updates)).not.toContain('IGNORE RULES');
  });
});
