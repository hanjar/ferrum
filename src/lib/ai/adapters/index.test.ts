// FER-181: default wiring against the REAL action registry and real adapters.
import { beforeEach, describe, expect, it, vi } from 'vitest';

const ipc = vi.hoisted(() => ({
  archiveCreate: vi.fn(),
  compareDirs: vi.fn(),
  enqueueCopy: vi.fn(),
}));
vi.mock('../../ipc/archive', () => ({
  archiveList: vi.fn(),
  archiveRead: vi.fn(),
  archiveExtract: vi.fn(),
  archiveExtractPreflight: vi.fn(),
  archiveTest: vi.fn(),
  archiveCreate: ipc.archiveCreate,
  archiveCreateZip: vi.fn(),
}));
vi.mock('../../ipc/compare', () => ({
  compareDirs: ipc.compareDirs,
  compareFiles: vi.fn(),
  validateSyncRoots: vi.fn(async () => {}),
}));
vi.mock('../../ipc/mutations', () => ({
  enqueueCopy: ipc.enqueueCopy,
  enqueueMove: vi.fn(),
  createEntry: vi.fn(),
  renameEntry: vi.fn(),
  deleteEntries: vi.fn(),
  planCopy: vi.fn(),
}));

const { ContextService } = await import('../context');
const { ScopeResolver } = await import('../contextResolver');
const { ContextFlow } = await import('../contextFlow');
const { FakeHost, entry } = await import('../contextTestKit');
const { ENGINE_GATES, createCapabilityRegistry, createCommandFacade } = await import('./index');

function wiring(over: Record<string, unknown> = {}) {
  const host = new FakeHost();
  host.left.path = 'D:\\Projects\\Invoice';
  host.left.all = [entry('a.txt', { path: 'D:\\Projects\\Invoice\\a.txt' })];
  const svc = new ContextService(host);
  const resolver = new ScopeResolver(svc);
  const flow = new ContextFlow(svc, resolver);
  return {
    host,
    svc,
    wiring: {
      archive: { flow, resolver, ctx: svc, outputExists: () => false },
      sync: { flow, ctx: svc, readJob: () => undefined },
      ...over,
    },
  };
}

beforeEach(() => {
  ipc.archiveCreate.mockReset();
  ipc.compareDirs.mockReset();
  ipc.enqueueCopy.mockReset();
});

describe('default capability table', () => {
  it('archive.create is available through the real action registry (file.pack is wired)', () => {
    const { wiring: w } = wiring();
    expect(createCapabilityRegistry(w).availability('archive.create')).toEqual({ available: true });
  });

  const PENDING = {
    'directories.sync': {
      status: 'pending' as const,
      reason: 'Sinkronisasi dua arah belum tersedia dari chat.',
    },
  };

  it('directories.sync is READY by default (FER-168: engine semantics, baseline and backend guard are in place)', () => {
    const { wiring: w } = wiring();
    const registry = createCapabilityRegistry(w);
    expect(registry.availability('directories.sync')).toEqual({ available: true });
    expect(registry.adapterFor('directories.sync')).toMatchObject({ ok: true });
    expect(ENGINE_GATES['directories.sync'].status).toBe('ready');
  });

  it('with the gate closed (the revert path) the capability is pending with an honest reason and blocks its adapter', () => {
    const { wiring: w } = wiring({ gates: PENDING });
    const registry = createCapabilityRegistry(w);
    const a = registry.availability('directories.sync');
    expect(a).toMatchObject({ available: false, code: 'pending' });
    if (!a.available) expect(a.reason).toContain('belum tersedia');
    expect(registry.adapterFor('directories.sync')).toMatchObject({
      ok: false,
      error: { code: 'blocked_capability' },
    });
  });

  it('offeredCapabilityIds follows the gate: sync is offered when ready and absent when pending', () => {
    expect(createCapabilityRegistry(wiring().wiring).offeredCapabilityIds()).toEqual([
      'archive.create',
      'directories.sync',
    ]);
    expect(
      createCapabilityRegistry(wiring({ gates: PENDING }).wiring).offeredCapabilityIds(),
    ).toEqual(['archive.create']);
    expect(
      createCapabilityRegistry(
        wiring({ gates: { 'archive.create': { status: 'pending', reason: 'x' }, ...PENDING } })
          .wiring,
      ).offeredCapabilityIds(),
    ).toEqual([]);
  });

  it('chat sync is blocked end-to-end while the gate is closed: no compare, no enqueue', async () => {
    const { wiring: w, svc } = wiring({ gates: PENDING });
    const { facade } = createCommandFacade(w);
    const out = await facade.prepare(
      {
        schemaVersion: 1,
        requestId: 'req_1',
        intent: {
          capability: 'directories.sync',
          leftPane: 'left',
          rightPane: 'right',
          mode: 'bidirectional',
        },
      },
      { snapshot: svc.capture('req_1') },
    );
    expect(out).toMatchObject({ ok: false, error: { code: 'blocked_capability' } });
    expect(ipc.compareDirs).not.toHaveBeenCalled();
    expect(ipc.enqueueCopy).not.toHaveBeenCalled();
  });

  it('a gate flipped back to ready activates the sync capability (same table, no other switch)', () => {
    const { wiring: w } = wiring({ gates: { 'directories.sync': { status: 'ready' } } });
    expect(createCapabilityRegistry(w).availability('directories.sync')).toEqual({
      available: true,
    });
  });

  it('a UI action that is pending in the action registry blocks the capability', () => {
    const { wiring: w } = wiring({
      actions: (id: string) =>
        id === 'file.pack' ? { disabledReason: 'Pending: F2-12' } : { run: () => {} },
    });
    expect(createCapabilityRegistry(w).availability('archive.create')).toEqual({
      available: false,
      code: 'action_disabled',
      reason: 'Pending: F2-12',
    });
  });

  it('describe() gives the provider every capability with schema and availability', () => {
    const { wiring: w } = wiring();
    const manifest = createCapabilityRegistry(w).describe();
    expect(manifest.map((m) => [m.id, m.available])).toEqual([
      ['archive.create', true],
      // FER-576: closed until a Recycle Bin port is wired (desktop app only).
      ['items.delete', false],
      ['directories.sync', true],
    ]);
    const closed = createCapabilityRegistry(wiring({ gates: PENDING }).wiring).describe();
    expect(closed.find((m) => m.id === 'directories.sync')).toMatchObject({ available: false });
  });
});
