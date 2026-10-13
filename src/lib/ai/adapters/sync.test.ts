// FER-215/216/217: unit tests of the chat sync adapter. EVERYTHING below the adapter boundary is a MOCK
// (compare/copy/baseline IPC); the filesystem-backed, whole-path proof is `syncChatFlow.test.ts`.
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { FsError, type OperationProgress } from '../../ipc/types';
import type { CompareDirsReport, DirDiffEntry, DirDiffState } from '../../ipc/compare';

const ipc = vi.hoisted(() => ({
  compareDirs: vi.fn(),
  enqueueCopy: vi.fn(),
  validateSyncRoots: vi.fn<(left: string, right: string) => Promise<void>>(async () => {}),
  baselineLoad: vi.fn<(pair: unknown) => Promise<unknown>>(async () => ({
    status: 'firstRun',
    reason: 'none',
  })),
}));
vi.mock('../../ipc/compare', () => ({
  compareDirs: ipc.compareDirs,
  compareFiles: vi.fn(),
  validateSyncRoots: ipc.validateSyncRoots,
}));
vi.mock('../../ipc/mutations', () => ({ enqueueCopy: ipc.enqueueCopy }));
vi.mock('../../ipc/syncBaseline', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../../ipc/syncBaseline')>();
  return { ...actual, syncBaselineLoad: ipc.baselineLoad };
});

const { ContextService } = await import('../context');
const { ScopeResolver } = await import('../contextResolver');
const { ContextFlow } = await import('../contextFlow');
const { FakeHost } = await import('../contextTestKit');
const { CommandFacade } = await import('../commands');
const { createSyncAdapter, SyncPreviewStore, guardScopeOf } = await import('./sync');
import type { Availability, CapabilitySource, CommandAdapter } from '../commands';
import type { SyncAdapterDeps } from './sync';
import type { SettleResult } from '../../syncService';
import type { SyncPlanItem } from '../../syncPlan';

const LEFT = 'D:\\L';
const RIGHT = 'E:\\R';

function diff(name: string, state: DirDiffState, over: Partial<DirDiffEntry> = {}): DirDiffEntry {
  return {
    name,
    state,
    leftSize: 10,
    leftModifiedMs: 1,
    rightSize: 10,
    rightModifiedMs: 1,
    ...over,
  };
}
function report(entries: DirDiffEntry[], over: Partial<CompareDirsReport> = {}): CompareDirsReport {
  return { leftRoot: LEFT, rightRoot: RIGHT, entries, truncated: false, recursive: true, ...over };
}
const envelope = {
  schemaVersion: 1,
  requestId: 'req_1',
  intent: {
    capability: 'directories.sync',
    leftPane: 'left',
    rightPane: 'right',
    mode: 'bidirectional',
  },
};

function setup(over: Partial<SyncAdapterDeps> = {}) {
  const host = new FakeHost();
  host.left.path = LEFT;
  host.right.path = RIGHT;
  const svc = new ContextService(host);
  const flow = new ContextFlow(svc, new ScopeResolver(svc));
  const settle = vi.fn(async (): Promise<SettleResult> => ({ status: 'saved', entries: 1 }));
  const readJob = vi.fn<(jobId: string) => OperationProgress | undefined>(() => undefined);
  const store = new SyncPreviewStore();
  const adapter = createSyncAdapter({ flow, ctx: svc, readJob, settle, store, ...over });
  const caps: CapabilitySource = {
    availability: (): Availability => ({ available: true }),
    adapterFor: () => ({ ok: true, value: adapter as CommandAdapter }),
  };
  const facade = new CommandFacade(caps, { ttlMs: 60_000 });
  const ctx = () => ({ snapshot: svc.capture('req_1') });
  const approve = async () => {
    const out = await facade.prepare(envelope, ctx());
    if (!out.ok) throw new Error(`prepare failed: ${out.error.code}`);
    const approved = facade.approve(out.value.handle.handleId);
    if (!approved.ok) throw new Error('approve failed');
    return { prepared: out.value, approved: approved.value };
  };
  return { host, svc, facade, adapter, settle, readJob, store, ctx, approve };
}

beforeEach(() => {
  ipc.compareDirs.mockReset();
  ipc.enqueueCopy.mockReset();
  ipc.validateSyncRoots.mockReset();
  ipc.validateSyncRoots.mockImplementation(async () => {});
  ipc.baselineLoad.mockReset();
  ipc.baselineLoad.mockImplementation(async () => ({ status: 'firstRun', reason: 'none' }));
});

describe('prepare (FER-215/216)', () => {
  it('builds a both-way dry-run (recursive, same defaults as the manual panel) and enqueues nothing', async () => {
    const t = setup();
    ipc.compareDirs.mockResolvedValue(
      report([diff('l.txt', 'leftOnly'), diff('r.txt', 'rightOnly'), diff('same.txt', 'same')]),
    );
    const out = await t.facade.prepare(envelope, t.ctx());
    expect(out.ok).toBe(true);
    if (!out.ok) return;
    expect(ipc.validateSyncRoots).toHaveBeenCalledWith(LEFT, RIGHT);
    expect(ipc.compareDirs).toHaveBeenCalledWith(LEFT, RIGHT, false, { recursive: true });
    expect(ipc.baselineLoad).toHaveBeenCalledTimes(1); // the dry-run loads its own baseline
    expect(ipc.enqueueCopy).not.toHaveBeenCalled();
    const p = out.value.preview;
    expect(p.summary).toContain(`kiri ${LEFT}`);
    expect(p.summary).toContain(`kanan ${RIGHT}`);
    expect(p.summary).toContain('Kiri \u2192 kanan: 1 item (0 menimpa).');
    expect(p.summary).toContain('Kanan \u2192 kiri: 1 item (0 menimpa).');
    expect(p.summary).toContain('Konflik: 0 (dilewati). Hapus: 0. Dilewati/identik: 1.');
    expect(p).toMatchObject({ itemCount: 2, conflictCount: 0, totalBytes: 20 });
    expect(p.warnings.join(' ')).toContain('Run pertama');
  });

  it('lists overwrites, conflicts and the baseline status; conflicts are skipped, never decided', async () => {
    const t = setup();
    ipc.baselineLoad.mockResolvedValue({
      status: 'ok',
      baseline: {
        version: 1,
        leftRoot: LEFT,
        rightRoot: RIGHT,
        entries: {
          'm.txt': { left: { size: 5, modifiedMs: 1 }, right: { size: 5, modifiedMs: 1 } },
        },
      },
    });
    ipc.compareDirs.mockResolvedValue(
      report([
        diff('l.txt', 'leftOnly'),
        // baseline: only the left changed => overwrite right (listed in the approval)
        diff('m.txt', 'newerLeft', {
          leftSize: 9,
          leftModifiedMs: 50,
          rightSize: 5,
          rightModifiedMs: 1,
        }),
        diff('c.txt', 'contentDifferent'), // no baseline entry => conflict
      ]),
    );
    const out = await t.facade.prepare(envelope, t.ctx());
    if (!out.ok) throw new Error('prepare failed');
    const p = out.value.preview;
    expect(p.summary).toContain('Kiri \u2192 kanan: 2 item (1 menimpa).');
    expect(p.summary).toContain('Konflik: 1 (dilewati).');
    expect(p.warnings.join(' ')).toContain('Baseline tersimpan dipakai');
    expect(p.warnings.join(' ')).toContain('1 file yang sudah ada akan ditimpa');
    expect(p.warnings.join(' ')).toContain('1 konflik dilewati');
    expect(p).toMatchObject({ itemCount: 3, conflictCount: 1 });
    // the card gets the conflict LIST and a paged item list from the local store
    expect(t.store.conflictsOf(out.value.plan)).toEqual([
      { itemRef: 'c.txt', label: 'c.txt', kind: 'noBaseline' },
    ]);
    expect(t.store.page(out.value.plan, 0, 10)).toMatchObject({ total: 3 });
    expect(t.store.page(out.value.plan, 1, 1).rows).toEqual([
      { ref: 'm.txt', label: 'm.txt', detail: 'kiri \u2192 kanan (menimpa)' },
    ]);
  });

  it('a plan that contains a delete is refused (chat never deletes)', async () => {
    const t = setup();
    ipc.compareDirs.mockResolvedValue(report([diff('l.txt', 'leftOnly')]));
    const svc = await import('../../syncService');
    const real = svc.buildSyncDryRun;
    const spy = vi.spyOn(svc, 'buildSyncDryRun').mockImplementationOnce(async (...a) => {
      const dry = await real(...a);
      const plan = [
        ...dry.plan,
        { ...dry.plan[0], name: 'gone.txt', action: 'deleteRight' as const },
      ];
      return { ...dry, plan, summary: { ...dry.summary, deletes: 1 } };
    });
    const out = await t.facade.prepare(envelope, t.ctx());
    spy.mockRestore();
    expect(out).toMatchObject({ ok: false, error: { code: 'unsupported_plan', cause: 'deletes' } });
  });

  it.each([
    ['truncated', { truncated: true }, 'truncated'],
    ['cancelled', { cancelled: true }, 'cancelled'],
    ['errors', { errors: [{ name: 'x', message: 'denied' }] }, 'errors'],
  ] as const)('a %s compare yields no plan at all', async (_n, over, cause) => {
    const t = setup();
    ipc.compareDirs.mockResolvedValue(
      report([diff('l.txt', 'leftOnly')], over as Partial<CompareDirsReport>),
    );
    const out = await t.facade.prepare(envelope, t.ctx());
    expect(out).toMatchObject({ ok: false, error: { code: 'unsupported_plan', cause } });
    expect(
      t.store.page({ operation: { kind: 'directories.sync', leftManifest: 'x' } } as never, 0, 5)
        .total,
    ).toBe(0);
  });

  it('only conflicts / nothing to do are not offered for approval, with an honest reason', async () => {
    const t = setup();
    ipc.compareDirs.mockResolvedValue(report([diff('c.txt', 'contentDifferent')]));
    const only = await t.facade.prepare(envelope, t.ctx());
    expect(only).toMatchObject({
      ok: false,
      error: { code: 'unsupported_plan', cause: 'only_conflicts' },
    });
    if (!only.ok) expect(only.error.message).toContain('chat tidak memutuskan konflik');
    ipc.compareDirs.mockResolvedValue(report([diff('s.txt', 'same')]));
    expect(await t.facade.prepare(envelope, t.ctx())).toMatchObject({
      ok: false,
      error: { cause: 'nothing_to_sync' },
    });
  });

  it('same / nested panes are rejected before any compare; an alias is rejected by the engine validator', async () => {
    const t = setup();
    t.host.right.path = LEFT;
    expect(await t.facade.prepare(envelope, t.ctx())).toMatchObject({
      ok: false,
      error: { cause: 'same_root' },
    });
    t.host.right.path = `${LEFT}\\sub`;
    expect(await t.facade.prepare(envelope, t.ctx())).toMatchObject({
      ok: false,
      error: { cause: 'nested_roots' },
    });
    expect(ipc.compareDirs).not.toHaveBeenCalled();

    t.host.right.path = 'E:\\alias-of-left'; // looks different, resolves to the same folder
    ipc.validateSyncRoots.mockRejectedValue(
      new FsError({ code: 'invalidPath', message: 'same folder', path: LEFT }),
    );
    const alias = await t.facade.prepare(envelope, t.ctx());
    expect(alias).toMatchObject({
      ok: false,
      error: { code: 'resolve_failed', cause: 'invalid_roots' },
    });
    if (!alias.ok) expect(JSON.stringify(alias.error)).not.toContain(LEFT.replace('\\', '\\\\'));
    expect(ipc.compareDirs).not.toHaveBeenCalled();
  });

  it('a compare error is a typed error without leaking the raw message', async () => {
    const t = setup();
    ipc.compareDirs.mockRejectedValue(new Error('access denied: D:\\secret\\file'));
    const out = await t.facade.prepare(envelope, t.ctx());
    expect(out).toMatchObject({
      ok: false,
      error: { code: 'resolve_failed', cause: 'compare_failed' },
    });
    expect(JSON.stringify(out)).not.toContain('secret');
  });

  it('the approval digest is bound to the dry-run: other facts or a changed baseline = another digest', async () => {
    const t = setup();
    ipc.compareDirs.mockResolvedValue(report([diff('l.txt', 'leftOnly')]));
    const a = await t.facade.prepare(envelope, t.ctx());
    ipc.compareDirs.mockResolvedValue(report([diff('l.txt', 'leftOnly', { leftSize: 11 })]));
    const b = await t.facade.prepare(envelope, t.ctx());
    if (!a.ok || !b.ok) throw new Error('prepare failed');
    expect(a.value.plan.previewDigest).not.toBe(b.value.plan.previewDigest);
  });
});

describe('guard scope (FER-216)', () => {
  const item = (over: Partial<SyncPlanItem>): SyncPlanItem => ({
    name: 'x',
    state: 'leftOnly',
    action: 'copyToRight',
    leftPath: `${LEFT}\\x`,
    rightPath: `${RIGHT}\\x`,
    ...over,
  });

  it('new copies fingerprint the source and require the target to stay absent; overwrites fingerprint the target', () => {
    const scope = guardScopeOf(LEFT, RIGHT, [
      item({ name: 'a.txt', leftPath: `${LEFT}\\a.txt`, rightPath: `${RIGHT}\\a.txt` }),
      item({
        name: 'sub\\b.txt',
        action: 'updateLeft',
        leftPath: `${LEFT}\\sub\\b.txt`,
        rightPath: `${RIGHT}\\sub\\b.txt`,
      }),
      item({ name: 'c', action: 'conflict' }),
    ]);
    expect(scope.leftRoot).toBe(LEFT);
    expect(scope.rightRoot).toBe(RIGHT);
    expect([...scope.absentPaths]).toEqual([`${RIGHT}\\a.txt`]);
    expect(scope.sourcePaths).toEqual(
      expect.arrayContaining([
        LEFT,
        RIGHT,
        `${LEFT}\\a.txt`,
        `${RIGHT}\\sub\\b.txt`, // source of the left-bound overwrite
        `${LEFT}\\sub\\b.txt`, // the existing target that will be replaced
        `${LEFT}\\sub`,
        `${RIGHT}\\sub`,
      ]),
    );
    expect(scope.sourcePaths).not.toContain(`${LEFT}\\x`); // conflicts are not executed => not part of the scope
  });

  it('the store gives the backend guard exactly this scope for a prepared plan, nothing for unknown plans', async () => {
    const t = setup();
    ipc.compareDirs.mockResolvedValue(report([diff('l.txt', 'leftOnly')]));
    const out = await t.facade.prepare(envelope, t.ctx());
    if (!out.ok) throw new Error('prepare failed');
    expect(t.store.scopeOf(out.value.plan)).toMatchObject({
      leftRoot: LEFT,
      rightRoot: RIGHT,
      absentPaths: [`${RIGHT}\\l.txt`],
    });
    expect(
      t.store.scopeOf({
        ...out.value.plan,
        operation: { ...out.value.plan.operation, leftManifest: 'cmp_unknown_l' },
      } as never),
    ).toBeUndefined();
    expect(t.store.scopeOf({ operation: { kind: 'archive.create' } } as never)).toBeUndefined();
  });

  it('the store is bounded (old previews are evicted)', () => {
    const store = new SyncPreviewStore();
    const scope = { leftRoot: LEFT, rightRoot: RIGHT, sourcePaths: [LEFT], absentPaths: [] };
    for (let i = 0; i < 40; i++)
      store.put(`k${i}`, { leftRoot: LEFT, rightRoot: RIGHT, items: [], scope });
    const plan = (k: string) =>
      ({ operation: { kind: 'directories.sync', leftManifest: k } }) as never;
    expect(store.scopeOf(plan('k0'))).toBeUndefined();
    expect(store.scopeOf(plan('k39'))).toBeDefined();
  });
});

describe('run (FER-217)', () => {
  const entries = () => [
    diff('l.txt', 'leftOnly'),
    diff('r.txt', 'rightOnly'),
    diff('c.txt', 'contentDifferent'),
  ];

  it('runs through runSyncPlan (queue), reports queued job ids, and settles the baseline with readJob from the deps', async () => {
    const t = setup();
    ipc.compareDirs.mockResolvedValue(report(entries()));
    ipc.enqueueCopy.mockResolvedValueOnce('op-1').mockResolvedValueOnce('op-2');
    const { approved } = await t.approve();
    const run = await t.facade.execute(approved);
    expect(ipc.enqueueCopy.mock.calls).toEqual([
      [[`${LEFT}\\l.txt`], RIGHT, 'overwrite'],
      [[`${RIGHT}\\r.txt`], LEFT, 'overwrite'],
    ]);
    expect(run).toMatchObject({
      ok: true,
      value: { state: 'queued', jobIds: ['op-1', 'op-2'], conflictsSkipped: 1 },
    });
    await vi.waitFor(() => expect(t.settle).toHaveBeenCalledTimes(1));
    const arg = (
      t.settle.mock.calls[0] as unknown as [
        { readJob: unknown; plan: SyncPlanItem[]; result: { jobIds: string[] } },
      ]
    )[0];
    expect(arg.readJob).toBe(t.readJob);
    expect(arg.result.jobIds).toEqual(['op-1', 'op-2']);
    // the executed plan still lists the conflict as a conflict: chat decided nothing
    expect(arg.plan.find((p) => p.name === 'c.txt')?.action).toBe('conflict');
  });

  it('the settlement result is reported through onSettled and a failing settlement is swallowed', async () => {
    const onSettled = vi.fn();
    const t = setup({ onSettled });
    ipc.compareDirs.mockResolvedValue(report(entries()));
    ipc.enqueueCopy.mockResolvedValue('op-1');
    t.settle.mockResolvedValueOnce({ status: 'unchanged', entries: 3 });
    await t.facade.execute((await t.approve()).approved);
    await vi.waitFor(() =>
      expect(onSettled).toHaveBeenCalledWith(
        { status: 'unchanged', entries: 3 },
        expect.stringMatching(/^cmp_/),
      ),
    );

    const t2 = setup({ onSettled });
    ipc.compareDirs.mockResolvedValue(report(entries()));
    t2.settle.mockRejectedValueOnce(new Error('boom'));
    await t2.facade.execute((await t2.approve()).approved);
    await vi.waitFor(() =>
      expect(onSettled).toHaveBeenLastCalledWith(
        { status: 'skipped', reason: 'verifyFailed' },
        expect.stringMatching(/^cmp_/),
      ),
    );
  });

  it('a pane that changed after the preview is stale: nothing enqueued, no settlement', async () => {
    const t = setup();
    ipc.compareDirs.mockResolvedValue(report(entries()));
    const { approved } = await t.approve();
    t.host.left.listing = {}; // pane relisted / navigated
    const run = await t.facade.execute(approved);
    expect(run).toMatchObject({ ok: false, error: { code: 'stale' } });
    expect(ipc.enqueueCopy).not.toHaveBeenCalled();
    expect(t.settle).not.toHaveBeenCalled();
  });

  it('folders changed after the preview: stale plan, nothing enqueued', async () => {
    const t = setup();
    ipc.compareDirs.mockResolvedValueOnce(report([diff('l.txt', 'leftOnly')]));
    const { approved } = await t.approve();
    ipc.compareDirs.mockResolvedValueOnce(
      report([diff('l.txt', 'leftOnly', { leftModifiedMs: 99 })]),
    );
    const run = await t.facade.execute(approved);
    expect(run).toMatchObject({ ok: false, error: { code: 'stale', cause: 'stale_plan' } });
    expect(ipc.enqueueCopy).not.toHaveBeenCalled();
    expect(t.settle).not.toHaveBeenCalled();
  });

  it('a fresh compare that is incomplete blocks the run', async () => {
    const t = setup();
    ipc.compareDirs.mockResolvedValueOnce(report([diff('l.txt', 'leftOnly')]));
    const { approved } = await t.approve();
    ipc.compareDirs.mockResolvedValueOnce(report([diff('l.txt', 'leftOnly')], { truncated: true }));
    const run = await t.facade.execute(approved);
    expect(run).toMatchObject({
      ok: false,
      error: { code: 'unsupported_plan', cause: 'truncated' },
    });
    expect(ipc.enqueueCopy).not.toHaveBeenCalled();
  });

  it('double dispatch enqueues once and settles once', async () => {
    const t = setup();
    ipc.compareDirs.mockResolvedValue(report([diff('l.txt', 'leftOnly')]));
    ipc.enqueueCopy.mockResolvedValue('op-1');
    const { approved } = await t.approve();
    const [a, b] = await Promise.all([t.facade.execute(approved), t.facade.execute(approved)]);
    expect(a).toEqual(b);
    expect(ipc.enqueueCopy).toHaveBeenCalledTimes(1);
    await vi.waitFor(() => expect(t.settle).toHaveBeenCalledTimes(1));
  });

  it('nothing reached the queue: a typed failure, no settlement', async () => {
    const t = setup();
    ipc.compareDirs.mockResolvedValue(report([diff('l.txt', 'leftOnly')]));
    ipc.enqueueCopy.mockRejectedValue(new Error('queue down'));
    const run = await t.facade.execute((await t.approve()).approved);
    expect(run).toMatchObject({
      ok: false,
      error: { code: 'resolve_failed', cause: 'enqueue_failed' },
    });
    expect(t.settle).not.toHaveBeenCalled();
  });

  it('a batch that failed to queue is reported (failedToQueue) next to the one that did', async () => {
    const t = setup();
    ipc.compareDirs.mockResolvedValue(
      report([diff('l.txt', 'leftOnly'), diff('r.txt', 'rightOnly')]),
    );
    ipc.enqueueCopy.mockResolvedValueOnce('op-1').mockRejectedValueOnce(new Error('queue full'));
    const run = await t.facade.execute((await t.approve()).approved);
    expect(run).toMatchObject({
      ok: true,
      value: { state: 'queued', jobIds: ['op-1'], failedToQueue: 1 },
    });
    if (run.ok) expect(run.value.notes.join(' ')).toContain('1 item gagal masuk antrean');
  });
});
