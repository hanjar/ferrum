// FER-179: facade prepare/approve/execute/receipt with real context lane
// (ContextService/ScopeResolver/ContextFlow over a fake host) and the shared
// services; only the IPC boundary is mocked.
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { FsError } from '../ipc/types';
import type { CompareDirsReport, DirDiffEntry, DirDiffState } from '../ipc/compare';

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
const { CommandFacade, toExecutionReceipt } = await import('./commands');
const { createArchiveAdapter } = await import('./adapters/archive');
const { SelectionHandleStore } = await import('./commandsSelection');
const { createSyncAdapter } = await import('./adapters/sync');
import type { Availability, CapabilitySource, CommandAdapter } from './commands';

const LEFT = 'D:\\Projects\\Invoice';
const OUT = 'D:\\Projects\\Invoice\\Invoice.zip';

const archiveEnvelope = (over: Record<string, unknown> = {}, requestId = 'req_1') => ({
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
});
const syncEnvelope = () => ({
  schemaVersion: 1,
  requestId: 'req_1',
  intent: {
    capability: 'directories.sync',
    leftPane: 'left',
    rightPane: 'right',
    mode: 'bidirectional',
  },
});

function diff(name: string, state: DirDiffState, ms = 1): DirDiffEntry {
  return { name, state, leftSize: 1, leftModifiedMs: ms, rightSize: 1, rightModifiedMs: ms };
}
function report(entries: DirDiffEntry[], truncated = false): CompareDirsReport {
  return { leftRoot: LEFT, rightRoot: 'E:\\Backup', entries, truncated };
}

function setup(opts: { blocked?: Set<string> } = {}) {
  const host = new FakeHost();
  host.left.path = LEFT;
  host.left.all = [
    entry('a.txt', { path: `${LEFT}\\a.txt` }),
    entry('b.txt', { path: `${LEFT}\\b.txt` }),
    entry('sub', { path: `${LEFT}\\sub`, isDir: true }),
  ];
  host.right.path = 'E:\\Backup';
  const svc = new ContextService(host);
  const resolver = new ScopeResolver(svc);
  const flow = new ContextFlow(svc, resolver);
  const onCompleted = vi.fn();
  const selections = new SelectionHandleStore();
  const archive = createArchiveAdapter({
    flow,
    resolver,
    ctx: svc,
    outputExists: (pane, name) =>
      host
        .probe(pane)!
        .allEntries()
        .some((e) => e.name.toLowerCase() === name.toLowerCase()),
    onCompleted,
    selections,
  });
  const sync = createSyncAdapter({
    flow,
    ctx: svc,
    readJob: () => undefined,
    settle: async () => ({ status: 'unchanged', entries: 0 }),
  });
  const blocked = opts.blocked ?? new Set<string>();
  const adapters: Record<string, CommandAdapter> = {
    'archive.create': archive as CommandAdapter,
    'directories.sync': sync as CommandAdapter,
  };
  const prepareSpy = vi.spyOn(archive, 'prepare');
  const caps: CapabilitySource = {
    availability: (id): Availability =>
      blocked.has(id)
        ? { available: false, code: 'pending', reason: 'pending in test' }
        : { available: true },
    adapterFor: (id) => {
      const a = adapters[id];
      if (!a) return { ok: false, error: { code: 'blocked_capability', message: 'unknown' } };
      if (blocked.has(id)) {
        return { ok: false, error: { code: 'blocked_capability', message: 'pending in test' } };
      }
      return { ok: true, value: a };
    },
  };
  let nowMs = 1_000;
  const facade = new CommandFacade(caps, { now: () => nowMs, ttlMs: 60_000 });
  return {
    host,
    svc,
    resolver,
    facade,
    onCompleted,
    selections,
    prepareSpy,
    blocked,
    advance: (ms: number) => (nowMs += ms),
    ctx: () => ({ snapshot: svc.capture('req_1') }),
  };
}

beforeEach(() => {
  ipc.archiveCreate.mockReset();
  ipc.compareDirs.mockReset();
  ipc.enqueueCopy.mockReset();
});

describe('archive.create facade', () => {
  it('prepare is read-only: no archive call, a valid plan and an unapproved handle', async () => {
    const t = setup();
    const out = await t.facade.prepare(archiveEnvelope(), t.ctx());
    expect(out.ok).toBe(true);
    if (!out.ok) return;
    expect(ipc.archiveCreate).not.toHaveBeenCalled();
    expect(ipc.enqueueCopy).not.toHaveBeenCalled();
    expect(out.value.plan.operation).toMatchObject({
      kind: 'archive.create',
      targetDirectory: LEFT,
      outputName: 'Invoice.zip',
    });
    expect(out.value.plan.requiresApproval).toBe(true);
    expect(out.value.preview).toMatchObject({ itemCount: 3, outputPath: OUT });
    expect(out.value.preview.summary).toContain('2 file dan 1 folder');
    expect(out.value.handle.planDigest).toBe(out.value.plan.previewDigest);
    // Item names/paths stay local: the plan, handle and preview never carry them.
    expect(JSON.stringify(out.value)).not.toMatch(/a\.txt|b\.txt/);
  });

  it('rejects model output outside the allowlist before any service is touched', async () => {
    const t = setup();
    const bad = [
      { schemaVersion: 1, requestId: 'req_1', intent: { capability: 'shell.exec', cmd: 'del *' } },
      archiveEnvelope({ approved: true }),
      archiveEnvelope({ outputName: '..\\evil.zip' }),
      archiveEnvelope({}, 'req_other'),
      'not an object',
    ];
    for (const raw of bad) {
      const out = await t.facade.prepare(raw, t.ctx());
      expect(out).toMatchObject({ ok: false, error: { code: 'invalid_intent' } });
    }
    expect(t.prepareSpy).not.toHaveBeenCalled();
    expect(ipc.archiveCreate).not.toHaveBeenCalled();
  });

  it('execute refuses anything that was not issued by approve()', async () => {
    const t = setup();
    const out = await t.facade.prepare(archiveEnvelope(), t.ctx());
    if (!out.ok) throw new Error('prepare failed');
    const forged = [
      {
        handleId: out.value.handle.handleId,
        planDigest: out.value.handle.planDigest,
        approved: true,
      },
      out.value.handle,
      out.value.plan,
      null,
    ];
    for (const f of forged) {
      const r = await t.facade.execute(f as never);
      expect(r).toMatchObject({ ok: false, error: { code: 'not_approved' } });
    }
    expect(ipc.archiveCreate).not.toHaveBeenCalled();
  });

  it('approve + execute calls the shared Pack service once and reports completed (not queued)', async () => {
    const t = setup();
    ipc.archiveCreate.mockResolvedValue(OUT);
    const out = await t.facade.prepare(archiveEnvelope(), t.ctx());
    if (!out.ok) throw new Error('prepare failed');
    const approved = t.facade.approve(out.value.handle.handleId);
    if (!approved.ok) throw new Error('approve failed');
    const run = await t.facade.execute(approved.value);
    expect(ipc.archiveCreate).toHaveBeenCalledTimes(1);
    expect(ipc.archiveCreate).toHaveBeenCalledWith(
      [`${LEFT}\\a.txt`, `${LEFT}\\b.txt`, `${LEFT}\\sub`],
      OUT,
      'zip',
      'skip', // the backend itself refuses to replace an existing file
      expect.any(String),
    );
    expect(run.ok).toBe(true);
    if (!run.ok) return;
    expect(run.value).toMatchObject({
      state: 'completed',
      jobIds: [],
      outputPath: OUT,
      completed: 3,
    });
    expect(toExecutionReceipt(run.value)).toMatchObject({ state: 'completed', failed: 0 });
    expect(t.onCompleted).toHaveBeenCalledTimes(1);
  });

  it('double execute (concurrent and later) runs the engine once and returns the same receipt', async () => {
    const t = setup();
    ipc.archiveCreate.mockResolvedValue(OUT);
    const out = await t.facade.prepare(archiveEnvelope(), t.ctx());
    if (!out.ok) throw new Error('prepare failed');
    const approved = t.facade.approve(out.value.handle.handleId);
    if (!approved.ok) throw new Error('approve failed');
    const [a, b] = await Promise.all([
      t.facade.execute(approved.value),
      t.facade.execute(approved.value),
    ]);
    const c = await t.facade.execute(approved.value);
    expect(ipc.archiveCreate).toHaveBeenCalledTimes(1);
    expect(a).toEqual(b);
    expect(c).toEqual(a);
  });

  it('an engine failure is a typed error, is not retried and does not report success', async () => {
    const t = setup();
    ipc.archiveCreate.mockRejectedValue(new Error('disk is full'));
    const out = await t.facade.prepare(archiveEnvelope(), t.ctx());
    if (!out.ok) throw new Error('prepare failed');
    const approved = t.facade.approve(out.value.handle.handleId);
    if (!approved.ok) throw new Error('approve failed');
    const first = await t.facade.execute(approved.value);
    const second = await t.facade.execute(approved.value);
    expect(first).toMatchObject({
      ok: false,
      error: { code: 'execution_failed', message: 'disk is full' },
    });
    expect(second).toEqual(first);
    expect(ipc.archiveCreate).toHaveBeenCalledTimes(1);
    expect(t.onCompleted).not.toHaveBeenCalled();
  });

  it('a pane change after approval holds the plan: stale, nothing written', async () => {
    const t = setup();
    const out = await t.facade.prepare(archiveEnvelope(), t.ctx());
    if (!out.ok) throw new Error('prepare failed');
    const approved = t.facade.approve(out.value.handle.handleId);
    if (!approved.ok) throw new Error('approve failed');
    t.host.swap();
    const run = await t.facade.execute(approved.value);
    expect(run).toMatchObject({ ok: false, error: { code: 'stale' } });
    expect(ipc.archiveCreate).not.toHaveBeenCalled();
  });

  it('keeps the approved manifest when the live selection changes afterwards', async () => {
    const t = setup();
    ipc.archiveCreate.mockResolvedValue(OUT);
    t.host.left.selected = [`${LEFT}\\a.txt`];
    const out = await t.facade.prepare(
      archiveEnvelope({ selector: { kind: 'selected' } }),
      t.ctx(),
    );
    if (!out.ok) throw new Error('prepare failed');
    const approved = t.facade.approve(out.value.handle.handleId);
    if (!approved.ok) throw new Error('approve failed');
    t.host.left.selected = [`${LEFT}\\b.txt`, `${LEFT}\\sub`]; // user keeps clicking
    const run = await t.facade.execute(approved.value);
    expect(run.ok).toBe(true);
    expect(ipc.archiveCreate).toHaveBeenCalledWith(
      [`${LEFT}\\a.txt`],
      OUT,
      'zip',
      'skip',
      expect.any(String),
    );
    expect(t.host.left.selected).toEqual([`${LEFT}\\b.txt`, `${LEFT}\\sub`]); // never touched
  });

  it('refuses to overwrite when the output name appeared after prepare', async () => {
    const t = setup();
    const out = await t.facade.prepare(archiveEnvelope(), t.ctx());
    if (!out.ok) throw new Error('prepare failed');
    const approved = t.facade.approve(out.value.handle.handleId);
    if (!approved.ok) throw new Error('approve failed');
    t.host.left.all = [...t.host.left.all, entry('Invoice.zip', { path: OUT })];
    const run = await t.facade.execute(approved.value);
    expect(run).toMatchObject({ ok: false, error: { code: 'target_changed' } });
    expect(ipc.archiveCreate).not.toHaveBeenCalled();
  });

  it('a name the panel could not see is still never overwritten: the backend refusal becomes target_changed', async () => {
    const t = setup();
    // Listing says the name is free (outputExists passes) but the file is on disk (filtered/hidden/just created).
    ipc.archiveCreate.mockRejectedValue(new FsError({ code: 'alreadyExists', message: 'exists' }));
    const out = await t.facade.prepare(archiveEnvelope(), t.ctx());
    if (!out.ok) throw new Error('prepare failed');
    const approved = t.facade.approve(out.value.handle.handleId);
    if (!approved.ok) throw new Error('approve failed');
    const run = await t.facade.execute(approved.value);
    expect(run).toMatchObject({ ok: false, error: { code: 'target_changed' } });
    expect(ipc.archiveCreate).toHaveBeenCalledTimes(1);
    expect(ipc.archiveCreate).toHaveBeenCalledWith(
      expect.any(Array),
      OUT,
      'zip',
      'skip',
      expect.any(String),
    );
  });

  it('any other engine failure is still surfaced, not mistaken for a name collision', async () => {
    const t = setup();
    ipc.archiveCreate.mockRejectedValue(new FsError({ code: 'io', message: 'disk is full' }));
    const out = await t.facade.prepare(archiveEnvelope(), t.ctx());
    if (!out.ok) throw new Error('prepare failed');
    const approved = t.facade.approve(out.value.handle.handleId);
    if (!approved.ok) throw new Error('approve failed');
    const run = await t.facade.execute(approved.value);
    expect(run.ok).toBe(false);
    if (!run.ok) expect(run.error.code).not.toBe('target_changed');
  });

  it('approval expires; an expired handle cannot run', async () => {
    const t = setup();
    const out = await t.facade.prepare(archiveEnvelope(), t.ctx());
    if (!out.ok) throw new Error('prepare failed');
    const approved = t.facade.approve(out.value.handle.handleId);
    if (!approved.ok) throw new Error('approve failed');
    t.advance(61_000);
    expect(await t.facade.execute(approved.value)).toMatchObject({
      ok: false,
      error: { code: 'expired' },
    });
    expect(t.facade.approve(out.value.handle.handleId)).toMatchObject({
      ok: false,
      error: { code: 'expired' },
    });
    expect(ipc.archiveCreate).not.toHaveBeenCalled();
  });

  it('a pending capability blocks prepare and also a later execute', async () => {
    const t = setup({ blocked: new Set(['archive.create']) });
    const out = await t.facade.prepare(archiveEnvelope(), t.ctx());
    expect(out).toMatchObject({ ok: false, error: { code: 'blocked_capability' } });
    expect(t.prepareSpy).not.toHaveBeenCalled();

    const t2 = setup();
    const ready = await t2.facade.prepare(archiveEnvelope(), t2.ctx());
    if (!ready.ok) throw new Error('prepare failed');
    const approved = t2.facade.approve(ready.value.handle.handleId);
    if (!approved.ok) throw new Error('approve failed');
    t2.blocked.add('archive.create'); // capability becomes inactive after approval
    expect(await t2.facade.execute(approved.value)).toMatchObject({
      ok: false,
      error: { code: 'blocked_capability' },
    });
    expect(ipc.archiveCreate).not.toHaveBeenCalled();
  });

  it('empty selection and stale context surface as resolve errors without a plan', async () => {
    const t = setup();
    const empty = await t.facade.prepare(
      archiveEnvelope({ selector: { kind: 'selected' } }),
      t.ctx(),
    );
    expect(empty).toMatchObject({
      ok: false,
      error: { code: 'resolve_failed', cause: 'empty_selection' },
    });
    const ctx = t.ctx();
    t.host.swap();
    const stale = await t.facade.prepare(archiveEnvelope(), ctx);
    expect(stale).toMatchObject({
      ok: false,
      error: { code: 'resolve_failed', cause: 'stale_context' },
    });
  });

  it('discard and execution release the frozen manifest', async () => {
    const t = setup();
    const a = await t.facade.prepare(archiveEnvelope(), t.ctx());
    if (!a.ok) throw new Error('prepare failed');
    expect(t.resolver.manifestCount()).toBe(1);
    expect(t.selections.size).toBe(1);
    t.facade.discard(a.value.handle.handleId);
    expect(t.resolver.manifestCount()).toBe(0);
    expect(t.selections.size).toBe(0);
    expect(t.facade.approve(a.value.handle.handleId)).toMatchObject({
      ok: false,
      error: { code: 'invalid_handle' },
    });

    ipc.archiveCreate.mockResolvedValue(OUT);
    const b = await t.facade.prepare(archiveEnvelope(), t.ctx());
    if (!b.ok) throw new Error('prepare failed');
    const approved = t.facade.approve(b.value.handle.handleId);
    if (!approved.ok) throw new Error('approve failed');
    await t.facade.execute(approved.value);
    expect(t.resolver.manifestCount()).toBe(0);
    expect(t.selections.size).toBe(0);
  });
});

describe('directories.sync facade', () => {
  const entries = () => [
    diff('l', 'leftOnly'),
    diff('r', 'rightOnly'),
    diff('c', 'contentDifferent'),
    diff('s', 'same'),
  ];

  it('prepare runs the read-only dry-run and enqueues nothing', async () => {
    const t = setup();
    ipc.compareDirs.mockResolvedValue(report(entries()));
    const out = await t.facade.prepare(syncEnvelope(), t.ctx());
    expect(out.ok).toBe(true);
    if (!out.ok) return;
    expect(ipc.compareDirs).toHaveBeenCalledWith(LEFT, 'E:\\Backup', false, { recursive: true });
    expect(ipc.enqueueCopy).not.toHaveBeenCalled();
    expect(out.value.preview.summary).toContain('kanan: 1 item (0 menimpa).');
    expect(out.value.preview.summary).toContain('kiri: 1 item (0 menimpa).');
    expect(out.value.preview.summary).toContain('Konflik: 1 (dilewati). Hapus: 0.');
    expect(out.value.preview).toMatchObject({ itemCount: 3, conflictCount: 1 });
    expect(out.value.preview.warnings.some((w) => w.includes('1 konflik'))).toBe(true);
  });

  it('execute enqueues through the shared service; receipt is queued with the real job ids', async () => {
    const t = setup();
    ipc.compareDirs.mockResolvedValue(report(entries()));
    ipc.enqueueCopy.mockResolvedValueOnce('op-1').mockResolvedValueOnce('op-2');
    const out = await t.facade.prepare(syncEnvelope(), t.ctx());
    if (!out.ok) throw new Error('prepare failed');
    const approved = t.facade.approve(out.value.handle.handleId);
    if (!approved.ok) throw new Error('approve failed');
    const run = await t.facade.execute(approved.value);
    expect(ipc.enqueueCopy.mock.calls).toEqual([
      [[`${LEFT}\\l`], 'E:\\Backup', 'overwrite'],
      [['E:\\Backup\\r'], LEFT, 'overwrite'],
    ]);
    expect(run.ok).toBe(true);
    if (!run.ok) return;
    expect(run.value).toMatchObject({
      state: 'queued',
      jobIds: ['op-1', 'op-2'],
      conflictsSkipped: 1,
    });
    expect(toExecutionReceipt(run.value)).toMatchObject({
      state: 'queued',
      completed: 0,
      failed: 0,
    });
  });

  it('nothing to copy is not offered for approval (no empty plan to approve)', async () => {
    const t = setup();
    ipc.compareDirs.mockResolvedValue(report([diff('s', 'same')]));
    expect(await t.facade.prepare(syncEnvelope(), t.ctx())).toMatchObject({
      ok: false,
      error: { code: 'unsupported_plan', cause: 'nothing_to_sync' },
    });
    expect(ipc.enqueueCopy).not.toHaveBeenCalled();
  });

  it('files changed after preview: stale, no copy enqueued', async () => {
    const t = setup();
    ipc.compareDirs.mockResolvedValueOnce(report([diff('l', 'leftOnly', 1)]));
    const out = await t.facade.prepare(syncEnvelope(), t.ctx());
    if (!out.ok) throw new Error('prepare failed');
    const approved = t.facade.approve(out.value.handle.handleId);
    if (!approved.ok) throw new Error('approve failed');
    ipc.compareDirs.mockResolvedValueOnce(report([diff('l', 'leftOnly', 99)]));
    const run = await t.facade.execute(approved.value);
    expect(run).toMatchObject({ ok: false, error: { code: 'stale', cause: 'stale_plan' } });
    expect(ipc.enqueueCopy).not.toHaveBeenCalled();
  });

  it('a truncated compare cannot be approved', async () => {
    const t = setup();
    ipc.compareDirs.mockResolvedValue(report([diff('l', 'leftOnly')], true));
    expect(await t.facade.prepare(syncEnvelope(), t.ctx())).toMatchObject({
      ok: false,
      error: { code: 'unsupported_plan', cause: 'truncated' },
    });
  });

  it('nested roots are rejected by the context lane before any compare', async () => {
    const t = setup();
    t.host.right.path = `${LEFT}\\sub`;
    const out = await t.facade.prepare(syncEnvelope(), t.ctx());
    expect(out).toMatchObject({
      ok: false,
      error: { code: 'resolve_failed', cause: 'nested_roots' },
    });
    expect(ipc.compareDirs).not.toHaveBeenCalled();
  });

  it('compare failure is a typed error; double execute enqueues once', async () => {
    const t = setup();
    ipc.compareDirs.mockRejectedValueOnce(new Error('access denied'));
    expect(await t.facade.prepare(syncEnvelope(), t.ctx())).toMatchObject({
      ok: false,
      error: { code: 'resolve_failed', cause: 'compare_failed' },
    });
    ipc.compareDirs.mockResolvedValue(report([diff('l', 'leftOnly')]));
    ipc.enqueueCopy.mockResolvedValue('op-1');
    const out = await t.facade.prepare(syncEnvelope(), t.ctx());
    if (!out.ok) throw new Error('prepare failed');
    const approved = t.facade.approve(out.value.handle.handleId);
    if (!approved.ok) throw new Error('approve failed');
    await Promise.all([t.facade.execute(approved.value), t.facade.execute(approved.value)]);
    expect(ipc.enqueueCopy).toHaveBeenCalledTimes(1);
  });
});
