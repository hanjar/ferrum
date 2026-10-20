// FER-179/180: manual UI and the AI adapters must reach the SAME shared
// services (spies wrap the real packService/syncService), and dispatching a
// UI action (opening a dialog) is not an operation result.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render } from '@testing-library/svelte';
import { tick } from 'svelte';
import type { CompareDirsReport, DirDiffEntry } from '../ipc/compare';

const ipc = vi.hoisted(() => ({
  archiveCreate: vi.fn(),
  compareDirs: vi.fn(),
  enqueueCopy: vi.fn(),
}));
vi.mock('../ipc/archive', () => ({
  archiveList: vi.fn(),
  archiveRead: vi.fn(),
  archiveExtract: vi.fn(),
  archiveExtractPreflight: vi.fn(),
  archiveTest: vi.fn(),
  archiveCreate: ipc.archiveCreate,
  archiveCreateZip: vi.fn(),
  archiveRarToolProbe: vi.fn(async () => ({ ok: false })),
  archiveRarToolDetect: vi.fn(async () => null),
}));
vi.mock('../ipc/compare', () => ({
  compareDirs: ipc.compareDirs,
  compareFiles: vi.fn(),
  validateSyncRoots: vi.fn(async () => {}),
}));
vi.mock('../ipc/mutations', () => ({
  enqueueCopy: ipc.enqueueCopy,
  enqueueMove: vi.fn(),
  createEntry: vi.fn(),
  renameEntry: vi.fn(),
  deleteEntries: vi.fn(),
  planCopy: vi.fn(),
}));
const spies = vi.hoisted(() => ({ runPack: vi.fn(), runSyncPlan: vi.fn() }));
vi.mock('../packService', async (importOriginal) => {
  const real = await importOriginal<typeof import('../packService')>();
  spies.runPack.mockImplementation(real.runPack);
  return { ...real, runPack: spies.runPack };
});
vi.mock('../syncService', async (importOriginal) => {
  const real = await importOriginal<typeof import('../syncService')>();
  spies.runSyncPlan.mockImplementation(real.runSyncPlan);
  return { ...real, runSyncPlan: spies.runSyncPlan };
});

const Dialogs = (await import('../components/Dialogs.svelte')).default;
const { dialogs } = await import('../stores/dialogStore.svelte');
const { compareStore } = await import('../stores/compareStore.svelte');
const { registerPane } = await import('../stores/panesRegistry.svelte');
const { ui } = await import('../uiState.svelte');
const { getAction, runAction } = await import('../actions');
const { ContextService } = await import('./context');
const { ScopeResolver } = await import('./contextResolver');
const { ContextFlow } = await import('./contextFlow');
const { FakeHost, entry } = await import('./contextTestKit');
const { CommandFacade } = await import('./commands');
const { createArchiveAdapter } = await import('./adapters/archive');
const { createSyncAdapter } = await import('./adapters/sync');
import type { CommandAdapter } from './commands';
import type { PaneApi } from '../stores/panesRegistry.svelte';

const LEFT = 'D:\\Projects\\Invoice';
const RIGHT = 'E:\\Backup';
const FILES = [`${LEFT}\\a.txt`, `${LEFT}\\b.txt`];
const unregisters: (() => void)[] = [];

function registerFake(id: 0 | 1, path: string, selected: string[] = []) {
  const api: PaneApi = {
    path,
    selectedPaths: () => selected,
    cursorEntry: () => undefined,
    refresh: () => {},
    newTab: () => {},
    closeCurrentTab: () => {},
    goUp: () => {},
    openCursorEntry: () => {},
  };
  unregisters.push(registerPane(id, api));
}

function aiStack() {
  const host = new FakeHost();
  host.left.path = LEFT;
  host.left.all = FILES.map((p, i) => entry(i ? 'b.txt' : 'a.txt', { path: p }));
  host.right.path = RIGHT;
  const svc = new ContextService(host);
  const resolver = new ScopeResolver(svc);
  const flow = new ContextFlow(svc, resolver);
  const archive = createArchiveAdapter({ flow, resolver, ctx: svc, outputExists: () => false });
  const sync = createSyncAdapter({
    flow,
    ctx: svc,
    readJob: () => undefined,
    settle: async () => ({ status: 'unchanged', entries: 0 }),
  });
  const adapters: Record<string, CommandAdapter> = {
    'archive.create': archive as CommandAdapter,
    'directories.sync': sync as CommandAdapter,
  };
  const facade = new CommandFacade({
    availability: () => ({ available: true }),
    adapterFor: (id) => ({ ok: true, value: adapters[id]! }),
  });
  return { facade, ctx: () => ({ snapshot: svc.capture('req_1') }) };
}

async function runViaFacade(facade: InstanceType<typeof CommandFacade>, raw: unknown, ctx: never) {
  const out = await facade.prepare(raw, ctx);
  if (!out.ok) throw new Error(`prepare failed: ${out.error.code}`);
  const approved = facade.approve(out.value.handle.handleId);
  if (!approved.ok) throw new Error('approve failed');
  return facade.execute(approved.value);
}

beforeEach(() => {
  ipc.archiveCreate.mockReset();
  ipc.compareDirs.mockReset();
  ipc.enqueueCopy.mockReset();
  spies.runPack.mockClear();
  spies.runSyncPlan.mockClear();
  compareStore.reset();
  ui.activePane = 0;
});
afterEach(() => {
  dialogs.close();
  for (const off of unregisters.splice(0)) off();
});

describe('Pack: dialog and adapter share packService.runPack', () => {
  it('manual dialog and chat adapter both call runPack and reach archiveCreate with the same shape', async () => {
    ipc.archiveCreate.mockResolvedValue(`${LEFT}.zip`);
    registerFake(0, LEFT, FILES);
    dialogs.beginPack('zip');
    const { getByRole } = render(Dialogs);
    await tick();
    await fireEvent.click(getByRole('button', { name: 'Pack' }));
    await vi.waitFor(() => expect(dialogs.current).toBeNull());
    expect(spies.runPack).toHaveBeenCalledTimes(1);
    const manualCall = ipc.archiveCreate.mock.calls[0];

    const { facade, ctx } = aiStack();
    const run = await runViaFacade(
      facade,
      {
        schemaVersion: 1,
        requestId: 'req_1',
        intent: {
          capability: 'archive.create',
          sourcePane: 'left',
          selector: { kind: 'allVisible', entries: 'all' },
          format: 'zip',
          target: { kind: 'sourceDirectory' },
        },
      },
      ctx() as never,
    );
    expect(run.ok).toBe(true);
    expect(spies.runPack).toHaveBeenCalledTimes(2);
    expect(ipc.archiveCreate).toHaveBeenCalledTimes(2);
    const aiCall = ipc.archiveCreate.mock.calls[1];
    // Same service, same arguments shape. The destination differs BY DESIGN: the manual
    // default is the sibling `<folder>.zip`, while the chat default (spec 4) is
    // `<folder>\<folder>.zip` inside the source directory.
    expect(aiCall![0]).toEqual(manualCall![0]);
    expect(aiCall![2]).toBe(manualCall![2]);
    // The conflict policy differs BY DESIGN (FER-411): the dialog keeps its explicit replace, chat never replaces.
    expect(manualCall![3]).toBeUndefined();
    expect(aiCall![3]).toBe('skip');
    expect(manualCall![1]).toBe(`${LEFT}.zip`);
    expect(aiCall![1]).toBe(`${LEFT}\\Invoice.zip`);
  });

  it('dispatching file.pack only opens the dialog: no engine call and no operation result', () => {
    registerFake(0, LEFT, FILES);
    const action = getAction('archive.pack');
    expect(action).toBeDefined();
    expect(runAction(action!)).toBe(true); // "dispatched" is all this means
    expect(dialogs.current).toMatchObject({ kind: 'pack' });
    expect(ipc.archiveCreate).not.toHaveBeenCalled();
    expect(spies.runPack).not.toHaveBeenCalled();
  });
});

describe('Sync: manual panel and chat adapter share syncService.runSyncPlan', () => {
  const diffs: DirDiffEntry[] = [
    {
      name: 'l',
      state: 'leftOnly',
      leftSize: 1,
      leftModifiedMs: 1,
      rightSize: 0,
      rightModifiedMs: 0,
    },
    {
      name: 'r',
      state: 'rightOnly',
      leftSize: 0,
      leftModifiedMs: 0,
      rightSize: 1,
      rightModifiedMs: 1,
    },
  ];
  const rep: CompareDirsReport = {
    leftRoot: LEFT,
    rightRoot: RIGHT,
    entries: diffs,
    truncated: false,
  };

  it('same runSyncPlan, same enqueueCopy calls', async () => {
    ipc.compareDirs.mockResolvedValue(rep);
    ipc.enqueueCopy.mockResolvedValue('op-x');
    registerFake(0, LEFT);
    registerFake(1, RIGHT);
    await compareStore.openSync();
    await compareStore.executeSync();
    expect(spies.runSyncPlan).toHaveBeenCalledTimes(1);
    const manualCalls = [...ipc.enqueueCopy.mock.calls];
    expect(manualCalls).toHaveLength(2);

    ipc.enqueueCopy.mockClear();
    const { facade, ctx } = aiStack();
    const run = await runViaFacade(
      facade,
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
      ctx() as never,
    );
    expect(run).toMatchObject({ ok: true, value: { state: 'queued', jobIds: ['op-x', 'op-x'] } });
    expect(spies.runSyncPlan).toHaveBeenCalledTimes(2);
    expect(ipc.enqueueCopy.mock.calls).toEqual(manualCalls);
  });

  it('opening the manual sync panel (runAction) enqueues nothing', async () => {
    ipc.compareDirs.mockResolvedValue(rep);
    registerFake(0, LEFT);
    registerFake(1, RIGHT);
    expect(runAction(getAction('commands.synchronize')!)).toBe(true);
    await vi.waitFor(() => expect(compareStore.syncPlan).not.toBeNull());
    expect(ipc.enqueueCopy).not.toHaveBeenCalled();
    expect(spies.runSyncPlan).not.toHaveBeenCalled();
  });
});
