// FER-207..209: follow-up dengan facade, registry, resolver, dan adapter archive NYATA; hanya batas IPC
// (archive_create) yang di-mock. Membuktikan filter ekstensi diterapkan pada manifest penuh, nama/tujuan
// revisi sampai ke Pack, dan tidak ada penulisan sebelum persetujuan atau setelah ditolak.
import { beforeEach, describe, expect, it, vi } from 'vitest';

const ipc = vi.hoisted(() => ({
  archiveCreate: vi.fn(),
  compareDirs: vi.fn(),
  enqueueCopy: vi.fn(),
}));
vi.mock('../ipc/archive', () => ({ archiveCreate: ipc.archiveCreate }));
vi.mock('../ipc/compare', () => ({ compareDirs: ipc.compareDirs, compareFiles: vi.fn() }));
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
const RIGHT = 'E:\\Backup';

function build() {
  const host = new FakeHost();
  host.left.path = LEFT;
  host.left.all = [
    entry('a.txt', { path: `${LEFT}\\a.txt` }),
    entry('b.pdf', { path: `${LEFT}\\b.pdf` }),
    entry('C.PDF', { path: `${LEFT}\\C.PDF` }),
    entry('docs', { path: `${LEFT}\\docs`, isDir: true }),
  ];
  host.right.path = RIGHT;
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
  });
  const requests: ProviderPlanRequest[] = [];
  const provider: IntentProvider = {
    async *plan(request: ProviderPlanRequest): AsyncGenerator<ProviderEvent> {
      requests.push(request);
      yield {
        kind: 'intent',
        intent: {
          schemaVersion: 1,
          requestId: request.requestId,
          intent: {
            capability: 'archive.create',
            sourcePane: 'left',
            selector: { kind: 'allVisible', entries: 'all' },
            format: 'zip',
            target: { kind: 'sourceDirectory' },
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
  const lastPlan = (runId: string) =>
    [...updates].reverse().find((u) => u.runId === runId && u.plan)!;
  const ref = (runId: string) => ({
    planId: lastPlan(runId).plan!.planId,
    revision: lastPlan(runId).revision!,
  });
  const follow = async (runId: string, text: string) => {
    const before = lastPlan(runId).plan!.planId;
    const res = ex.followUp(runId, ref(runId), text);
    if (res.accepted) await vi.waitFor(() => expect(lastPlan(runId).plan!.planId).not.toBe(before));
    return res;
  };
  return { host, ex, updates, stateOf, lastPlan, ref, follow, requests };
}

beforeEach(() => {
  ipc.archiveCreate.mockReset();
  ipc.compareDirs.mockReset();
  ipc.enqueueCopy.mockReset();
});

async function started() {
  const t = build();
  t.ex.submit({ requestId: 'req_1', text: 'zip kiri', runId: 'run-1' } as never);
  await vi.waitFor(() => expect(t.stateOf('run-1')).toBe('awaitingApproval'));
  return t;
}

describe('follow-up with the real archive adapter', () => {
  it('"hanya PDF" + "namanya ..." + "taruh di kanan": only the final revision reaches archive_create, once', async () => {
    ipc.archiveCreate.mockResolvedValue(`${RIGHT}\\invoice-september.zip`);
    const t = await started();
    expect(t.lastPlan('run-1').plan!.preview.itemCount).toBe(4);

    await t.follow('run-1', 'hanya PDF');
    expect(t.lastPlan('run-1').plan!.preview.itemCount).toBe(2); // b.pdf + C.PDF; folder dan txt dilewati
    expect(t.lastPlan('run-1').scopeNote).toMatch(/\.pdf/);
    await t.follow('run-1', 'namanya invoice-september.zip');
    await t.follow('run-1', 'taruh di kanan');
    const plan = t.lastPlan('run-1').plan!;
    expect(plan.operation).toMatchObject({
      kind: 'archive.create',
      targetDirectory: RIGHT,
      outputName: 'invoice-september.zip',
    });
    expect(t.lastPlan('run-1').revision).toBe(4);
    expect(ipc.archiveCreate).not.toHaveBeenCalled(); // nol penulisan sebelum persetujuan

    t.ex.approve('run-1', { planDigest: plan.previewDigest });
    await vi.waitFor(() => expect(t.stateOf('run-1')).toBe('succeeded'));
    expect(ipc.archiveCreate).toHaveBeenCalledTimes(1);
    const [sources, dest, format, conflict] = ipc.archiveCreate.mock.calls[0];
    expect(sources).toEqual([`${LEFT}\\b.pdf`, `${LEFT}\\C.PDF`]);
    expect(dest).toBe(`${RIGHT}\\invoice-september.zip`);
    expect(format).toBe('zip');
    expect(conflict).toBe('skip');
    expect(t.requests).toHaveLength(1);
  });

  it('a filter that matches nothing leaves the plan unchanged-in-history and writes nothing', async () => {
    const t = await started();
    const before = t.lastPlan('run-1').plan!.planId;
    const res = t.ex.followUp('run-1', t.ref('run-1'), 'hanya docx');
    expect(res.accepted).toBe(true);
    await vi.waitFor(() => expect(t.stateOf('run-1')).toBe('needsInput'));
    const last = [...t.updates].reverse().find((u) => u.runId === 'run-1')!;
    expect(last.errorMessage).toMatch(/\.docx/);
    expect(t.lastPlan('run-1').plan!.planId).toBe(before);
    // Perubahan gagal tidak menempel: edit berikutnya dihitung dari revision terakhir yang berhasil.
    const ok = await t.follow('run-1', 'hanya pdf');
    expect(ok).toMatchObject({ accepted: true, revision: 2 });
    expect(ipc.archiveCreate).not.toHaveBeenCalled();
  });

  it('adversarial names are refused locally; nothing is prepared or written', async () => {
    const t = await started();
    const before = t.lastPlan('run-1').plan!.planId;
    for (const text of [
      'namanya ../x.zip',
      'namanya a:b.zip',
      'namanya CON.zip',
      'namanya ..\\x.zip',
    ]) {
      expect(t.ex.followUp('run-1', t.ref('run-1'), text), text).toMatchObject({
        accepted: false,
        code: 'invalid_edit',
      });
    }
    expect(t.lastPlan('run-1').plan!.planId).toBe(before);
    expect(ipc.archiveCreate).not.toHaveBeenCalled();
  });

  it('after the archive ran, a follow-up is refused and archive_create is not called again', async () => {
    ipc.archiveCreate.mockResolvedValue(`${LEFT}\\Invoice.zip`);
    const t = await started();
    const r = t.ref('run-1');
    t.ex.approve('run-1');
    await vi.waitFor(() => expect(t.stateOf('run-1')).toBe('succeeded'));
    const res = t.ex.followUp('run-1', r, 'namanya lain.zip');
    expect(res).toMatchObject({ accepted: false, code: 'final' });
    expect(ipc.archiveCreate).toHaveBeenCalledTimes(1);
  });

  it('an edit after a pane navigated is stale and archive_create is never called', async () => {
    const t = await started();
    t.host.left.listing = {};
    t.ex.followUp('run-1', t.ref('run-1'), 'taruh di kanan');
    await vi.waitFor(() => expect(t.stateOf('run-1')).toBe('stale'));
    expect(ipc.archiveCreate).not.toHaveBeenCalled();
  });
});
