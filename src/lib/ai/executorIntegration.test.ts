// FER-195/198: executor + facade + adapter archive NYATA + registry NYATA; hanya batas IPC yang di-mock.
// Membuktikan: nol panggilan archive_create sebelum persetujuan, tepat satu setelahnya, sync (gate PENDING)
// diblokir jujur tanpa fallback copy.
import { beforeEach, describe, expect, it, vi } from 'vitest';

const ipc = vi.hoisted(() => ({
  archiveCreate: vi.fn(),
  compareDirs: vi.fn(),
  enqueueCopy: vi.fn(),
}));
vi.mock('../ipc/archive', () => ({ archiveCreate: ipc.archiveCreate }));
vi.mock('../ipc/compare', () => ({
  compareDirs: ipc.compareDirs,
  compareFiles: vi.fn(),
  validateSyncRoots: vi.fn(async () => {}),
}));
vi.mock('../ipc/mutations', () => ({ enqueueCopy: ipc.enqueueCopy }));

const { ContextService } = await import('./context');
const { ScopeResolver } = await import('./contextResolver');
const { ContextFlow } = await import('./contextFlow');
const { FakeHost, entry } = await import('./contextTestKit');
const { createCommandFacade } = await import('./adapters');
const { AiExecutor } = await import('./executor');
const { MemoryExecutionLedger } = await import('./executorLedger');
const { PollingQueuePort } = await import('./executorQueue');
import type { RunState, RunUpdate } from './chatStore.svelte';
import type { IntentProvider, ProviderEvent, ProviderPlanRequest } from './provider';

const LEFT = 'D:\\Projects\\Invoice';

function build(gates?: Record<string, { status: 'ready' | 'pending'; reason?: string }>) {
  const host = new FakeHost();
  host.left.path = LEFT;
  host.left.all = [
    entry('a.txt', { path: `${LEFT}\\a.txt` }),
    entry('b.txt', { path: `${LEFT}\\b.txt` }),
  ];
  host.right.path = 'E:\\Backup';
  const svc = new ContextService(host);
  const resolver = new ScopeResolver(svc);
  const flow = new ContextFlow(svc, resolver);
  const { facade, registry } = createCommandFacade({
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
    gates,
  });
  let intent: 'archive' | 'sync' = 'archive';
  const requests: Array<ProviderPlanRequest & { capabilities?: readonly string[] }> = [];
  const provider: IntentProvider = {
    async *plan(request: ProviderPlanRequest): AsyncGenerator<ProviderEvent> {
      requests.push(request);
      yield {
        kind: 'intent',
        intent:
          intent === 'archive'
            ? {
                schemaVersion: 1,
                requestId: request.requestId,
                intent: {
                  capability: 'archive.create',
                  sourcePane: 'left',
                  selector: { kind: 'allVisible', entries: 'all' },
                  format: 'zip',
                  target: { kind: 'sourceDirectory' },
                },
              }
            : {
                schemaVersion: 1,
                requestId: request.requestId,
                intent: {
                  capability: 'directories.sync',
                  leftPane: 'left',
                  rightPane: 'right',
                  mode: 'bidirectional',
                },
              },
      };
    },
  };
  const updates: RunUpdate[] = [];
  const ex = new AiExecutor({
    provider,
    capture: (id) => svc.capture(id),
    modelContext: (s) => svc.modelContext(s),
    facade,
    capabilities: registry,
    ledger: new MemoryExecutionLedger(),
    queue: new PollingQueuePort({ read: () => [], cancel: async () => undefined }),
  });
  ex.subscribe((u) => updates.push(u));
  const stateOf = (runId: string): RunState | undefined =>
    [...updates].reverse().find((u) => u.runId === runId)?.state;
  return {
    host,
    ex,
    updates,
    stateOf,
    requests,
    setIntent: (i: 'archive' | 'sync') => (intent = i),
  };
}

beforeEach(() => {
  ipc.archiveCreate.mockReset();
  ipc.compareDirs.mockReset();
  ipc.enqueueCopy.mockReset();
});

describe('executor with the real facade, registry and archive adapter', () => {
  it('zero engine calls before approval, exactly one after (double approve)', async () => {
    ipc.archiveCreate.mockResolvedValue(`${LEFT}\\Invoice.zip`);
    const t = build();
    t.ex.submit({ requestId: 'req_1', text: 'zip kiri', runId: 'run-1' } as never);
    await vi.waitFor(() => expect(t.stateOf('run-1')).toBe('awaitingApproval'));
    expect(ipc.archiveCreate).not.toHaveBeenCalled();
    expect(ipc.enqueueCopy).not.toHaveBeenCalled();
    const plan = t.updates.find((u) => u.state === 'awaitingApproval')!.plan!;
    expect(plan.operation).toMatchObject({ kind: 'archive.create', targetDirectory: LEFT });
    t.ex.approve('run-1', { planDigest: plan.previewDigest });
    t.ex.approve('run-1', { planDigest: plan.previewDigest });
    await vi.waitFor(() => expect(t.stateOf('run-1')).toBe('succeeded'));
    expect(ipc.archiveCreate).toHaveBeenCalledTimes(1);
    expect(ipc.archiveCreate.mock.calls[0][0]).toEqual([`${LEFT}\\a.txt`, `${LEFT}\\b.txt`]);
  });

  it('with the sync gate closed the provider is offered only the ready capability and a sync intent is blocked honestly', async () => {
    const t = build({
      'directories.sync': {
        status: 'pending',
        reason: 'Sinkronisasi dua arah belum tersedia dari chat.',
      },
    });
    t.setIntent('sync');
    t.ex.submit({ requestId: 'req_1', text: 'sinkronkan', runId: 'run-1' } as never);
    await vi.waitFor(() => expect(t.stateOf('run-1')).toBe('blockedCapability'));
    expect(t.requests[0].capabilities).toEqual(['archive.create']);
    expect(t.updates.at(-1)?.blockedReason).toMatch(/belum tersedia/i);
    expect(ipc.compareDirs).not.toHaveBeenCalled();
    expect(ipc.enqueueCopy).not.toHaveBeenCalled();
    expect(ipc.archiveCreate).not.toHaveBeenCalled();
  });

  it('a pane navigated after preview is stale: archive_create is never called', async () => {
    const t = build();
    t.ex.submit({ requestId: 'req_1', text: 'zip kiri', runId: 'run-1' } as never);
    await vi.waitFor(() => expect(t.stateOf('run-1')).toBe('awaitingApproval'));
    t.host.left.listing = {}; // pane relisted / navigated
    t.ex.approve('run-1');
    await vi.waitFor(() => expect(t.stateOf('run-1')).toBe('stale'));
    expect(ipc.archiveCreate).not.toHaveBeenCalled();
  });

  it('an output name that appeared after preview is stale and the old file is not overwritten', async () => {
    const t = build();
    t.ex.submit({ requestId: 'req_1', text: 'zip kiri', runId: 'run-1' } as never);
    await vi.waitFor(() => expect(t.stateOf('run-1')).toBe('awaitingApproval'));
    t.host.left.all = [...t.host.left.all, entry('Invoice.zip', { path: `${LEFT}\\Invoice.zip` })];
    t.ex.approve('run-1');
    await vi.waitFor(() => expect(t.stateOf('run-1')).toBe('stale'));
    expect(ipc.archiveCreate).not.toHaveBeenCalled();
  });
});
