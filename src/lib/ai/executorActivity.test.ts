// ACT-08 R5: the executor's non-mutating branch. `activity.query` never reaches prepare/approval/
// ResolvedPlan/guard/ledger/queue; the local answer is never sent back to the provider.
import { beforeEach, describe, expect, it, vi } from 'vitest';

const invokeMock = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invokeMock(...a) }));

import { tauriActivityQueryPort, type ActivityQueryPort } from './activityQuery';
import type { RunState, RunUpdate } from './chatStore.svelte';
import { ok } from './commands';
import type { ActivityQuery, IntentEnvelope, PaneSnapshot } from './contracts';
import { AiExecutor, type ExecutorDeps } from './executor';
import type { ExecutionLedgerPort } from './executorLedger';
import { canTransition } from './executorState';
import type { QueuePort } from './executorQueue';
import type { IntentProvider, ProviderEvent, ProviderPlanRequest } from './provider';

const SECRET_NAME = 'Invoice2026Secret';
const SECRET_HINT = '\u2026\\Clients\\AcmePrivate';

const entry = (over: Record<string, unknown> = {}) => ({
  id: 'l00000000000000a1',
  displayName: SECRET_NAME,
  parentHint: SECRET_HINT,
  kind: 'folder',
  availability: 'available',
  reasons: ['recent'],
  ...over,
});

const RECENT: ActivityQuery = {
  query: { kind: 'recent', periodDays: 7, entity: 'folder' },
  limit: 5,
};

class Provider implements IntentProvider {
  requests: Array<ProviderPlanRequest & { capabilities?: readonly string[] }> = [];
  next: (r: ProviderPlanRequest) => ProviderEvent = () => ({
    kind: 'activityQuery',
    request: RECENT,
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

function setup(port: ActivityQueryPort | undefined, extra: Partial<ExecutorDeps> = {}) {
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
    ...(port ? { activity: port } : {}),
    ...extra,
  });
  const updates: RunUpdate[] = [];
  executor.subscribe((u) => updates.push(u));
  let n = 0;
  const submit = () => {
    n++;
    const runId = `run-${n}`;
    executor.submit({ requestId: `req_${n}`, text: 'folder apa yang saya buka minggu ini', runId });
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

function fakePort(answer: unknown = { status: 'entities', entries: [entry()], truncated: false }) {
  return {
    available: vi.fn(() => true),
    query: vi.fn(async () => answer),
    locate: vi.fn(async () => ({
      path: 'C:\\Clients\\AcmePrivate\\Invoice2026Secret',
      parentPath: 'C:\\Clients\\AcmePrivate',
      kind: 'folder',
    })),
  };
}

beforeEach(() => invokeMock.mockReset());

describe('offer of activity.query', () => {
  it('old callers (no activity port) keep the exact mutating list', async () => {
    const t = setup(undefined);
    t.provider.next = () => ({ kind: 'question', text: 'Apa?' });
    t.submit();
    await vi.waitFor(() => expect(t.provider.requests).toHaveLength(1));
    expect(t.provider.requests[0].capabilities).toEqual(['archive.create', 'directories.sync']);
  });

  it('is offered while the activity service is available, and withdrawn when it is not', async () => {
    const port = fakePort();
    const t = setup(port);
    t.submit();
    await vi.waitFor(() => expect(t.provider.requests).toHaveLength(1));
    expect(t.provider.requests[0].capabilities).toEqual([
      'archive.create',
      'directories.sync',
      'activity.query',
    ]);
    port.available.mockReturnValue(false);
    t.provider.next = () => ({ kind: 'question', text: 'Apa?' });
    t.submit();
    await vi.waitFor(() => expect(t.provider.requests).toHaveLength(2));
    expect(t.provider.requests[1].capabilities).toEqual(['archive.create', 'directories.sync']);
  });

  it('a throwing availability check fails closed (not offered)', async () => {
    const port = {
      ...fakePort(),
      available: () => {
        throw new Error('boom');
      },
    };
    const t = setup(port);
    t.provider.next = () => ({ kind: 'question', text: 'Apa?' });
    t.submit();
    await vi.waitFor(() => expect(t.provider.requests).toHaveLength(1));
    expect(t.provider.requests[0].capabilities).not.toContain('activity.query');
  });
});

describe('read branch: answered, never planned', () => {
  it('runs the query locally and ends answered without prepare/approval/guard/ledger/queue', async () => {
    const port = fakePort();
    const t = setup(port);
    const runId = t.submit();
    await vi.waitFor(() => expect(t.stateOf(runId)).toBe('answered'));
    expect(port.query).toHaveBeenCalledTimes(1);
    expect(port.query).toHaveBeenCalledWith(RECENT);
    const u = t.last(runId);
    expect(u.activity).toEqual({ status: 'entities', entries: [entry()], truncated: false });
    expect(u.activityQuery).toBe('folder terakhir dibuka (7 hari)');
    expect(u.plan).toBeUndefined();
    expect(t.facade.prepare).not.toHaveBeenCalled();
    expect(t.facade.approve).not.toHaveBeenCalled();
    expect(t.facade.execute).not.toHaveBeenCalled();
    expect(t.guard.register).not.toHaveBeenCalled();
    expect(t.guard.authorize).not.toHaveBeenCalled();
    expect(t.ledger.reserve).not.toHaveBeenCalled();
    expect(t.queue.cancel).not.toHaveBeenCalled();
    // Legal transitions only: planning -> answered (terminal).
    expect(t.updates.filter((x) => x.runId === runId).map((x) => x.state)).toEqual(['answered']);
    expect(canTransition('answered', 'queued')).toBe(false);
    t.executor.approve(runId);
    expect(t.facade.approve).not.toHaveBeenCalled();
  });

  it('the answer never reaches the provider: one request per question, no names in any payload', async () => {
    const port = fakePort({
      status: 'ambiguous',
      candidates: [
        entry({ id: 'l1', parentHint: '\u2026\\A\\AcmePrivate' }),
        entry({ id: 'l2', parentHint: '\u2026\\B\\AcmePrivate' }),
      ],
    });
    const t = setup(port);
    const first = t.submit();
    await vi.waitFor(() => expect(t.stateOf(first)).toBe('answered'));
    expect(t.provider.requests).toHaveLength(1); // no second turn with the result
    // The next request (and its model context) carries nothing from the answer.
    t.provider.next = (r) => ({ kind: 'intent', intent: archiveIntent(r.requestId) });
    const second = t.submit();
    await vi.waitFor(() => expect(t.provider.requests).toHaveLength(2));
    await vi.waitFor(() => expect(t.stateOf(second)).toBeDefined());
    const sent = JSON.stringify(t.provider.requests.map((r) => ({ ...r, signal: undefined })));
    for (const secret of [
      SECRET_NAME,
      'AcmePrivate',
      'l00000000000000a1',
      'candidates',
      'ambiguous',
    ]) {
      expect(sent).not.toContain(secret);
    }
    expect(JSON.stringify(t.modelContext.mock.results.map((r) => r.value))).not.toContain(
      SECRET_NAME,
    );
    // A follow-up on the answered run is refused: there is no plan to change and nothing is re-asked.
    const res = t.executor.followUp(first, { planId: 'x', revision: 1 }, 'yang kedua');
    expect(res.accepted).toBe(false);
    expect(t.provider.requests).toHaveLength(2);
  });

  it('answers with extra fields (locator/count/score) are refused, nothing shown', async () => {
    const t = setup(
      fakePort({ status: 'resolved', entry: entry({ locator: 'C:\\Clients\\AcmePrivate\\x' }) }),
    );
    const runId = t.submit();
    await vi.waitFor(() => expect(t.stateOf(runId)).toBe('failed'));
    const u = t.last(runId);
    expect(u.activity).toBeUndefined();
    expect(u.errorMessage).toContain('tidak valid');
    expect(JSON.stringify(t.updates)).not.toContain('AcmePrivate');
  });

  it.each([
    ['withheld', { status: 'withheld' }],
    ['stale', { status: 'stale' }],
    ['notFound', { status: 'notFound' }],
    ['unavailable', { status: 'unavailable' }],
  ])('maps %s to an answered run', async (status, payload) => {
    const t = setup(fakePort(payload));
    const runId = t.submit();
    await vi.waitFor(() => expect(t.stateOf(runId)).toBe('answered'));
    expect(t.last(runId).activity?.status).toBe(status);
  });

  it('IPC rejection of an invalid query -> rejected; any other failure -> unavailable', async () => {
    const bad = {
      ...fakePort(),
      query: vi.fn(async () => Promise.reject({ code: 'invalidPath', message: 'C:\\x' })),
    };
    const t = setup(bad);
    const a = t.submit();
    await vi.waitFor(() => expect(t.stateOf(a)).toBe('answered'));
    expect(t.last(a).activity).toEqual({ status: 'rejected', reason: 'invalid query' });
    const broken = {
      ...fakePort(),
      query: vi.fn(async () => Promise.reject(new Error('ipc closed'))),
    };
    const t2 = setup(broken);
    const b = t2.submit();
    await vi.waitFor(() => expect(t2.stateOf(b)).toBe('answered'));
    expect(t2.last(b).activity).toEqual({ status: 'unavailable' });
  });

  it('service gone between offer and answer -> unavailable without reading', async () => {
    const port = fakePort();
    const t = setup(port);
    let calls = 0;
    port.available.mockImplementation(() => ++calls === 1);
    const runId = t.submit();
    await vi.waitFor(() => expect(t.stateOf(runId)).toBe('answered'));
    expect(t.last(runId).activity).toEqual({ status: 'unavailable' });
    expect(port.query).not.toHaveBeenCalled();
  });

  it('a model reply naming activity.query without an activity port is answered unavailable, never planned', async () => {
    const t = setup(undefined);
    const runId = t.submit();
    await vi.waitFor(() => expect(t.stateOf(runId)).toBe('answered'));
    expect(t.last(runId).activity).toEqual({ status: 'unavailable' });
    expect(t.facade.prepare).not.toHaveBeenCalled();
  });

  it('cancel while the query runs drops the late answer', async () => {
    let release!: (v: unknown) => void;
    const port = { ...fakePort(), query: vi.fn(() => new Promise((r) => (release = r))) };
    const t = setup(port);
    const runId = t.submit();
    await vi.waitFor(() => expect(port.query).toHaveBeenCalled());
    t.executor.cancel(runId);
    release({ status: 'entities', entries: [entry()], truncated: false });
    await new Promise((r) => setTimeout(r, 0));
    expect(t.stateOf(runId)).toBe('cancelled');
    expect(t.updates.some((u) => u.state === 'answered')).toBe(false);
  });

  it('mutating intents still take the prepared-plan path when the read capability is offered', async () => {
    const t = setup(fakePort());
    t.provider.next = (r) => ({ kind: 'intent', intent: archiveIntent(r.requestId) });
    const runId = t.submit();
    await vi.waitFor(() => expect(t.facade.prepare).toHaveBeenCalledTimes(1));
    await vi.waitFor(() => expect(t.stateOf(runId)).toBe('needsInput'));
  });
});

describe('locateActivityItem (R7 v1)', () => {
  it('locates only items listed in this run, never missing/offline ones or a different kind', async () => {
    const port = fakePort({
      status: 'entities',
      entries: [
        entry(),
        entry({ id: 'l2', availability: 'missing' }),
        entry({ id: 'l3', kind: 'file', displayName: 'a.txt' }),
      ],
      truncated: false,
    });
    const t = setup(port);
    const runId = t.submit();
    await vi.waitFor(() => expect(t.stateOf(runId)).toBe('answered'));
    expect(await t.executor.locateActivityItem(runId, 'l00000000000000a1')).toEqual({
      path: 'C:\\Clients\\AcmePrivate\\Invoice2026Secret',
      parentPath: 'C:\\Clients\\AcmePrivate',
      kind: 'folder',
    });
    expect(await t.executor.locateActivityItem(runId, 'l2')).toBeNull(); // missing
    expect(await t.executor.locateActivityItem(runId, 'l3')).toBeNull(); // kind mismatch with locate result
    expect(await t.executor.locateActivityItem(runId, 'l999')).toBeNull(); // not in this answer
    expect(await t.executor.locateActivityItem('run-x', 'l00000000000000a1')).toBeNull();
    expect(port.locate.mock.calls.map((c) => (c as unknown[])[0])).toEqual([
      'l00000000000000a1',
      'l3',
    ]);
    // Locating never plans anything.
    expect(t.facade.prepare).not.toHaveBeenCalled();
    expect(t.provider.requests).toHaveLength(1);
  });

  it('a malformed location is refused', async () => {
    const port = {
      ...fakePort(),
      locate: vi.fn(async () => ({ path: 'C:\\x', parentPath: 'C:\\', kind: 'folder', score: 1 })),
    };
    const t = setup(port);
    const runId = t.submit();
    await vi.waitFor(() => expect(t.stateOf(runId)).toBe('answered'));
    expect(await t.executor.locateActivityItem(runId, 'l00000000000000a1')).toBeNull();
  });
});

describe('real Tauri port end to end (invoke mocked)', () => {
  it('a history question only invokes activity_chat_query: no recording, no provider round-trip', async () => {
    invokeMock.mockImplementation(async (cmd: string) =>
      cmd === 'activity_chat_query' ? { status: 'resolved', entry: entry() } : undefined,
    );
    const t = setup(tauriActivityQueryPort({ available: () => true, disclosureAcked: () => true }));
    const runId = t.submit();
    await vi.waitFor(() => expect(t.stateOf(runId)).toBe('answered'));
    expect(invokeMock.mock.calls.map((c) => c[0])).toEqual(['activity_chat_query']);
    expect(invokeMock).not.toHaveBeenCalledWith('activity_record_visit', expect.anything());
    expect(t.provider.requests).toHaveLength(1);
  });

  it('first-run disclosure not acknowledged -> activity.query is not offered (FER-432)', async () => {
    let acked = false;
    const t = setup(
      tauriActivityQueryPort({ available: () => true, disclosureAcked: () => acked }),
    );
    t.provider.next = () => ({ kind: 'question', text: 'Apa?' });
    t.submit();
    await vi.waitFor(() => expect(t.provider.requests).toHaveLength(1));
    expect(t.provider.requests[0].capabilities).not.toContain('activity.query');
    acked = true;
    t.submit();
    await vi.waitFor(() => expect(t.provider.requests).toHaveLength(2));
    expect(t.provider.requests[1].capabilities).toContain('activity.query');
  });
});
