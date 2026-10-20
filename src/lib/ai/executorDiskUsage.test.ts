// FER-466: the executor's read branch for `diskUsage.query`. The question never reaches prepare /
// approval / ResolvedPlan / guard / ledger / queue; the local answer is never sent to the provider.
import { describe, expect, it, vi } from 'vitest';
import type { RunState, RunUpdate } from './chatStore.svelte';
import { ok } from './commands';
import type { DiskUsageQueryKind, IntentEnvelope, PaneSnapshot } from './contracts';
import type { DiskUsageActiveScan, DiskUsageQueryPort } from './diskUsageQuery';
import { AiExecutor, DISK_USAGE_HINT_TEXT, type ExecutorDeps } from './executor';
import { RemoteProviderError } from './remote-provider';
import type { ExecutionLedgerPort } from './executorLedger';
import type { QueuePort } from './executorQueue';
import type { IntentProvider, ProviderEvent, ProviderPlanRequest } from './provider';
import type { DiskUsageFileItem, DiskUsageTopFiles } from '../ipc/types';

const SECRET_NAME = 'PayrollSecret2026.xlsx';
const SECRET_DIR = 'AcmeConfidential';
const ROOT = `D:\\${SECRET_DIR}`;

const scan: DiskUsageActiveScan = {
  scanId: 's1',
  root: ROOT,
  rootId: ROOT,
  viewId: ROOT,
  viewLabel: SECRET_DIR,
  metric: 'logical',
  phase: 'complete',
  engine: 'walk',
  scannedAtMs: 1,
  stats: null,
};

const bigFile: DiskUsageFileItem = {
  id: `${ROOT}\\${SECRET_NAME}`,
  parentId: ROOT,
  name: SECRET_NAME,
  logicalBytes: 123_456_789,
  allocatedBytes: 123_460_000,
  allocatedSupported: true,
  category: 'document',
  percentOfParent: 70,
  percentOfScanRoot: 70,
};

const result: DiskUsageTopFiles = {
  scanId: 's1',
  nodeId: ROOT,
  version: 1,
  scannedAtMs: 1,
  metric: 'logical',
  coverage: 'complete',
  limit: 5,
  totalMatching: 1,
  truncated: false,
  label: 'Top 5',
  items: [bigFile],
};

const QUERY: DiskUsageQueryKind = { kind: 'largestFiles', limit: 5 };

class Provider implements IntentProvider {
  requests: ProviderPlanRequest[] = [];
  next: (r: ProviderPlanRequest) => ProviderEvent = () => ({
    kind: 'diskUsageQuery',
    query: QUERY,
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

function archiveIntent(requestId: string): IntentEnvelope {
  return {
    schemaVersion: 1,
    requestId,
    intent: {
      capability: 'archive.create',
      sourcePane: 'left',
      selector: { kind: 'selected' },
      format: 'zip',
      target: { kind: 'sourceDirectory' },
    },
  };
}

function fakePort(over: Partial<DiskUsageQueryPort> = {}) {
  return {
    available: vi.fn(() => true),
    current: vi.fn(() => scan as DiskUsageActiveScan | { none: 'none' }),
    topFiles: vi.fn(async () => result),
    children: vi.fn(),
    ...over,
  } as DiskUsageQueryPort & {
    available: ReturnType<typeof vi.fn>;
    topFiles: ReturnType<typeof vi.fn>;
    current: ReturnType<typeof vi.fn>;
  };
}

function setup(port: DiskUsageQueryPort | undefined, extra: Partial<ExecutorDeps> = {}) {
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
  const queue: QueuePort = {
    get: vi.fn(),
    subscribe: () => () => undefined,
    cancel: vi.fn(),
  } as never;
  const guard = { register: vi.fn(), authorize: vi.fn(), revoke: vi.fn() };
  const modelContext = vi.fn(() => ({
    activePane: 'left' as const,
    leftLocation: 'disk' as const,
    rightLocation: 'disk' as const,
    leftCount: 2,
    rightCount: 0,
    showHidden: false,
    filterActive: false,
  }));
  const executor = new AiExecutor({
    provider,
    capture: snapshot,
    modelContext,
    facade,
    capabilities: {
      availability: () => ({ available: true }),
      describe: () => [],
      offeredCapabilityIds: () => ['archive.create', 'directories.sync'],
    },
    ledger,
    queue,
    guard,
    ...(port ? { diskUsage: port } : {}),
    ...extra,
  });
  const updates: RunUpdate[] = [];
  executor.subscribe((u) => updates.push(u));
  let n = 0;
  const submit = (text = 'paling gede itu apa ya') => {
    n++;
    const runId = `run-${n}`;
    executor.submit({ requestId: `req_${n}`, text, runId });
    return runId;
  };
  const stateOf = (runId: string): RunState | undefined =>
    updates.filter((u) => u.runId === runId).at(-1)?.state;
  const last = (runId: string) => updates.filter((u) => u.runId === runId).at(-1)!;
  return {
    provider,
    facade,
    ledger,
    queue,
    guard,
    modelContext,
    executor,
    updates,
    submit,
    stateOf,
    last,
  };
}

describe('offer of diskUsage.query', () => {
  it('old callers (no port) keep the exact mutating list', async () => {
    const t = setup(undefined);
    t.provider.next = () => ({ kind: 'question', text: 'Apa?' });
    t.submit();
    await vi.waitFor(() => expect(t.provider.requests).toHaveLength(1));
    expect(t.provider.requests[0].capabilities).toEqual(['archive.create', 'directories.sync']);
  });

  it('is offered while a Disk Usage result is available, withdrawn when it is not', async () => {
    const port = fakePort();
    const t = setup(port);
    t.submit();
    await vi.waitFor(() => expect(t.provider.requests).toHaveLength(1));
    expect(t.provider.requests[0].capabilities).toEqual([
      'archive.create',
      'directories.sync',
      'diskUsage.query',
    ]);
    port.available.mockReturnValue(false);
    t.provider.next = () => ({ kind: 'question', text: 'Apa?' });
    t.submit();
    await vi.waitFor(() => expect(t.provider.requests).toHaveLength(2));
    // FER-573: with Disk Usage closed the model may only point the user to it ('ui.hint').
    expect(t.provider.requests[1].capabilities).toEqual([
      'archive.create',
      'directories.sync',
      'ui.hint',
    ]);
  });

  it('a throwing availability check fails closed (not offered)', async () => {
    const port = fakePort({
      available: () => {
        throw new Error('boom');
      },
    });
    const t = setup(port);
    t.provider.next = () => ({ kind: 'question', text: 'Apa?' });
    t.submit();
    await vi.waitFor(() => expect(t.provider.requests).toHaveLength(1));
    expect(t.provider.requests[0].capabilities).not.toContain('diskUsage.query');
  });
});

describe('FER-573: size question while Disk Usage is closed', () => {
  it('ui.hint and diskUsage.query are never offered together', async () => {
    const t = setup(fakePort());
    t.submit();
    await vi.waitFor(() => expect(t.provider.requests).toHaveLength(1));
    expect(t.provider.requests[0].capabilities).not.toContain('ui.hint');
    const closed = setup(fakePort({ available: vi.fn(() => false) }));
    closed.provider.next = () => ({ kind: 'question', text: 'Apa?' });
    closed.submit();
    await vi.waitFor(() => expect(closed.provider.requests).toHaveLength(1));
    expect(closed.provider.requests[0].capabilities).toContain('ui.hint');
    expect(closed.provider.requests[0].capabilities).not.toContain('diskUsage.query');
    // Callers without a Disk Usage port never offer it.
    const none = setup(undefined);
    none.provider.next = () => ({ kind: 'question', text: 'Apa?' });
    none.submit();
    await vi.waitFor(() => expect(none.provider.requests).toHaveLength(1));
    expect(none.provider.requests[0].capabilities).not.toContain('ui.hint');
  });

  it('a hint becomes a fixed Indonesian note plus the openDiskUsage action, nothing is planned', async () => {
    const port = fakePort({ available: vi.fn(() => false) });
    const t = setup(port);
    t.provider.next = () => ({ kind: 'hint', hint: 'openDiskUsage' });
    const runId = t.submit('berapa ukuran size semua folder dan file di panel kanan?');
    await vi.waitFor(() => expect(t.stateOf(runId)).toBe('needsInput'));
    const u = t.last(runId);
    expect(u.uiHint).toBe('openDiskUsage');
    expect(u.modelText).toBe(DISK_USAGE_HINT_TEXT);
    expect(u.modelText).toContain('Buka Disk Usage (Alt+Shift+D)');
    expect(t.facade.prepare).not.toHaveBeenCalled();
    expect(t.facade.approve).not.toHaveBeenCalled();
    expect(port.topFiles).not.toHaveBeenCalled();
    // The provider request carries no pane path or size.
    expect(JSON.stringify(t.provider.requests[0])).not.toContain('D:');
  });

  it('fails closed: a hint when Disk Usage is open (or not offered) is an invalid reply', async () => {
    const open = setup(fakePort());
    open.provider.next = () => ({ kind: 'hint', hint: 'openDiskUsage' });
    const runId = open.submit();
    await vi.waitFor(() => expect(open.stateOf(runId)).toBe('failed'));
    expect(open.last(runId).uiHint).toBeUndefined();
    const noPort = setup(undefined);
    noPort.provider.next = () => ({ kind: 'hint', hint: 'openDiskUsage' });
    const id2 = noPort.submit();
    await vi.waitFor(() => expect(noPort.stateOf(id2)).toBe('failed'));
  });

  it('shows the provider message and model on a failed run, without the raw error', async () => {
    const t = setup(undefined, { providerModel: () => 'ag/gemini-4-argon-high' });
    t.provider.next = () => {
      throw new RemoteProviderError(
        'model_unavailable',
        'Provider request failed',
        '[antigravity/gemini-4-argon-high] [404]: Requested entity was not found.',
      );
    };
    const runId = t.submit();
    await vi.waitFor(() => expect(t.stateOf(runId)).toBe('failed'));
    const view = t.last(runId).providerError!;
    expect(view.code).toBe('model_not_found');
    expect(view.detail).toBe(
      'Provider said: [antigravity/gemini-4-argon-high] [404]: Requested entity was not found. (model ag/gemini-4-argon-high)',
    );
    expect(view.message).not.toContain('Requested entity');
  });
});

describe('read branch: answered locally, never planned', () => {
  it('runs the query on the snapshot and ends answered without prepare/approval/guard/ledger/queue', async () => {
    const port = fakePort();
    const t = setup(port);
    const runId = t.submit();
    await vi.waitFor(() => expect(t.stateOf(runId)).toBe('answered'));
    expect(port.topFiles).toHaveBeenCalledTimes(1);
    const u = t.last(runId);
    expect(u.diskUsage?.status).toBe('files');
    expect(u.diskUsageQuery).toBe('file terbesar, tampilan sekarang');
    expect(u.plan).toBeUndefined();
    expect(t.facade.prepare).not.toHaveBeenCalled();
    expect(t.facade.approve).not.toHaveBeenCalled();
    expect(t.facade.execute).not.toHaveBeenCalled();
    expect(t.guard.register).not.toHaveBeenCalled();
    expect(t.guard.authorize).not.toHaveBeenCalled();
    expect(t.ledger.reserve).not.toHaveBeenCalled();
    expect(t.queue.cancel).not.toHaveBeenCalled();
    expect(t.updates.filter((x) => x.runId === runId).map((x) => x.state)).toEqual(['answered']);
    t.executor.approve(runId);
    expect(t.facade.approve).not.toHaveBeenCalled();
  });

  it('nothing from the scan reaches the provider: one request per question, no names in any payload', async () => {
    const t = setup(fakePort());
    const first = t.submit();
    await vi.waitFor(() => expect(t.stateOf(first)).toBe('answered'));
    expect(t.provider.requests).toHaveLength(1); // no second turn with the result
    t.provider.next = (r) => ({ kind: 'intent', intent: archiveIntent(r.requestId) });
    const second = t.submit('zip semua');
    await vi.waitFor(() => expect(t.provider.requests).toHaveLength(2));
    await vi.waitFor(() => expect(t.stateOf(second)).toBeDefined());
    const sent = JSON.stringify(t.provider.requests.map((r) => ({ ...r, signal: undefined })));
    for (const secret of [SECRET_NAME, SECRET_DIR, '123456789', 'bigFile', 'totalMatching']) {
      expect(sent, secret).not.toContain(secret);
    }
    expect(JSON.stringify(t.modelContext.mock.results.map((r) => r.value))).not.toContain(
      SECRET_NAME,
    );
    // The first request carried only the user's words and the capability list.
    expect(t.provider.requests[0].userText).toBe('paling gede itu apa ya');
    expect(Object.keys(t.provider.requests[0]).sort()).toEqual(
      ['capabilities', 'context', 'requestId', 'signal', 'userText'].sort(),
    );
    // A follow-up on the answered run is refused: there is no plan and nothing is re-asked.
    const res = t.executor.followUp(first, { planId: 'x', revision: 1 }, 'yang kedua');
    expect(res.accepted).toBe(false);
    expect(t.provider.requests).toHaveLength(2);
  });

  it('no scan on screen: answers locally, honestly, without touching the snapshot', async () => {
    const port = fakePort({ current: vi.fn(() => ({ none: 'none' as const })) });
    const t = setup(port);
    const runId = t.submit();
    await vi.waitFor(() => expect(t.stateOf(runId)).toBe('answered'));
    expect(t.last(runId).diskUsage).toEqual({ status: 'noScan', reason: 'none' });
    expect(port.topFiles).not.toHaveBeenCalled();
  });

  it('the workspace vanishing after the offer is answered as no scan, not guessed', async () => {
    const port = fakePort();
    const t = setup(port);
    t.provider.next = (r) => {
      port.available.mockReturnValue(false); // closed between offer and answer
      void r;
      return { kind: 'diskUsageQuery', query: QUERY };
    };
    const runId = t.submit();
    await vi.waitFor(() => expect(t.stateOf(runId)).toBe('answered'));
    expect(t.last(runId).diskUsage).toEqual({ status: 'noScan', reason: 'none' });
    expect(port.topFiles).not.toHaveBeenCalled();
  });

  it('a query error becomes an app-text answer; no backend message leaks', async () => {
    const port = fakePort({
      topFiles: vi.fn(async () => {
        throw new Error(`C:\\${SECRET_DIR} exploded`);
      }),
    });
    const t = setup(port);
    const runId = t.submit();
    await vi.waitFor(() => expect(t.stateOf(runId)).toBe('answered'));
    expect(t.last(runId).diskUsage).toEqual({ status: 'unavailable', reason: 'failed' });
    expect(JSON.stringify(t.updates)).not.toContain('exploded');
  });

  it('an invalid question from a fake provider fails the run and reads nothing', async () => {
    const port = fakePort();
    const t = setup(port);
    t.provider.next = () =>
      ({
        kind: 'diskUsageQuery',
        query: { kind: 'largestFiles', limit: 500 },
      }) as ProviderEvent;
    const runId = t.submit();
    await vi.waitFor(() => expect(t.stateOf(runId)).toBe('failed'));
    expect(port.topFiles).not.toHaveBeenCalled();
  });

  it('cancel while the query runs drops the late answer', async () => {
    let release!: (v: DiskUsageTopFiles) => void;
    const port = fakePort({
      topFiles: vi.fn(() => new Promise<DiskUsageTopFiles>((r) => (release = r))),
    });
    const t = setup(port);
    const runId = t.submit();
    await vi.waitFor(() => expect(port.topFiles).toHaveBeenCalled());
    t.executor.cancel(runId);
    release(result);
    await new Promise((r) => setTimeout(r, 0));
    expect(t.stateOf(runId)).toBe('cancelled');
    expect(t.updates.some((u) => u.state === 'answered')).toBe(false);
  });

  it('mutating intents still take the prepared-plan path when the capability is offered', async () => {
    const t = setup(fakePort());
    t.provider.next = (r) => ({ kind: 'intent', intent: archiveIntent(r.requestId) });
    const runId = t.submit();
    await vi.waitFor(() => expect(t.facade.prepare).toHaveBeenCalledTimes(1));
    await vi.waitFor(() => expect(t.stateOf(runId)).toBe('needsInput'));
  });
});
