// FER-576 test kit: a real executor + facade + registry + archive and delete adapters over a fake two-pane
// host. Only the IPC boundary is faked: the Recycle Bin port, the navigation port, Test Archive and the backend
// guard are spies. The caller mocks `../../ipc/archive` (`archiveCreate`) itself.
import { vi } from 'vitest';
import { createCommandFacade } from './adapters';
import { ContextService } from './context';
import { ContextFlow } from './contextFlow';
import { ScopeResolver } from './contextResolver';
import { entry, FakeHost } from './contextTestKit';
import { AiExecutor, type ExecutionGuard } from './executor';
import { MemoryExecutionLedger } from './executorLedger';
import { PollingQueuePort } from './executorQueue';
import type { NavigateTarget } from './paneNavigate';
import type { IntentProvider, ProviderEvent, ProviderPlanRequest } from './provider';
import type { RunState, RunUpdate } from './chatStore.svelte';
import type { RecycleCheckRow, RecycleDeleteReport } from './adapters/delete';

export const FOLDER = 'D:\\Downloads\\Telegram Desktop\\pc_20250814';
export const CSV = `${FOLDER}\\pc_202508014.csv`;
export const OTHER = `${FOLDER}\\keep-me.txt`;
export const USER_TEXT = `${CSV} -> tolong arsipkan ke paling kecil gunakan 7zip lalu hapus ya`;

export interface KitOptions {
  /** Events the scripted provider yields, one array per request. */
  events: ProviderEvent[];
  /** Paths the fake disk knows (dir / file). Defaults to the owner example. */
  disk?: Record<string, NavigateTarget>;
  /** `check` answers; default: everything can go to the Recycle Bin. */
  recycleCheck?: (paths: readonly string[]) => RecycleCheckRow[];
  recycleDelete?: (paths: readonly string[]) => Promise<RecycleDeleteReport>;
  verify?: (
    path: string,
    expected?: { members?: number },
  ) => Promise<{ ok: boolean; message: string }>;
  /** What the size/entry census says about the archive sources (default: 1 member, no links). */
  census?: { archiveMembers: number; links: number } | 'absent';
  /** Marks kept by a pane refresh (default false: the real refresh clears them). */
  keepSelectionOnRefresh?: boolean;
  /** After the navigation, the pane also marks this other file. */
  navSelectsExtra?: boolean;
  noVerify?: boolean;
  noDelete?: boolean;
  noNavigate?: boolean;
  guard?: ExecutionGuard;
  autoCreate?: boolean;
  active?: 'left' | 'right';
  userText?: string;
}

export function buildKit(options: KitOptions) {
  const host = new FakeHost();
  host.active = options.active ?? 'left';
  host.left.path = 'D:\\Work';
  host.left.all = [entry('start.txt', { path: 'D:\\Work\\start.txt' })];
  host.right.path = 'E:\\Backup';
  host.right.all = [entry('secret-neighbor.txt', { path: 'E:\\Backup\\secret-neighbor.txt' })];
  const svc = new ContextService(host);
  const resolver = new ScopeResolver(svc);
  const flow = new ContextFlow(svc, resolver);

  const disk: Record<string, NavigateTarget> = options.disk ?? {
    [CSV]: { kind: 'file', path: CSV, folder: FOLDER },
    [FOLDER]: { kind: 'dir', path: FOLDER },
  };
  const order: string[] = [];
  let censusCalls = 0;
  const navigate = {
    available: () => true,
    resolve: vi.fn(async (path: string): Promise<NavigateTarget> => {
      order.push(`resolve:${path}`);
      return disk[path] ?? { kind: 'missing' };
    }),
    open: vi.fn(
      async (side: 'left' | 'right', target: Exclude<NavigateTarget, { kind: 'missing' }>) => {
        order.push(`open:${side}`);
        const pane = side === 'left' ? host.left : host.right;
        const folder = target.kind === 'dir' ? target.path : target.folder;
        pane.path = folder;
        pane.all = [
          entry('pc_202508014.csv', { path: CSV }),
          entry('keep-me.txt', { path: OTHER }),
        ];
        pane.selected =
          target.kind === 'file' ? [target.path, ...(options.navSelectsExtra ? [OTHER] : [])] : [];
        pane.listing = {};
        return true;
      },
    ),
    settled: vi.fn(async () => undefined),
  };
  const recycle = {
    check: vi.fn(async (paths: readonly string[]) =>
      options.recycleCheck
        ? options.recycleCheck(paths)
        : paths.map((path) => ({ path, ok: true }) as RecycleCheckRow),
    ),
    deleteToRecycle: vi.fn(async (paths: readonly string[]) => {
      order.push(`recycle:${paths.join('|')}`);
      return options.recycleDelete
        ? options.recycleDelete(paths)
        : { requested: paths.length, deleted: paths.length, failed: [] };
    }),
  };
  const verify = vi.fn(async (path: string, expected?: { members?: number }) => {
    order.push(`verify:${path}`);
    return options.verify
      ? options.verify(path, expected)
      : { ok: true, message: '1 entri terverifikasi' };
  });

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
      // A real refresh: the new archive shows up and the listing is a new object, the selection stays.
      onCompleted: () => {
        order.push('refresh');
        host.left.all = [
          ...host.left.all,
          entry('pc_20250814.7z', { path: `${FOLDER}\\pc_20250814.7z` }),
        ];
        host.left.listing = {};
        // The real refresh resets the marks; only a test can ask to keep them.
        if (options.keepSelectionOnRefresh !== true) host.left.selected = [];
      },
      measure: async () => undefined,
      ...(options.census === 'absent'
        ? {}
        : {
            census: async () => {
              censusCalls++;
              const census = options.census;
              return census && census !== 'absent' ? census : { archiveMembers: 1, links: 0 };
            },
          }),
    },
    sync: { flow, ctx: svc, readJob: () => undefined },
    ...(options.noDelete
      ? {}
      : {
          delete: { resolver, ctx: svc, recycle, onCompleted: () => order.push('refresh-delete') },
        }),
    actions: () => ({ run: () => undefined }),
  });

  const requests: Array<ProviderPlanRequest & { capabilities?: readonly string[] }> = [];
  const queue = [...options.events];
  const provider: IntentProvider = {
    async *plan(request: ProviderPlanRequest): AsyncGenerator<ProviderEvent> {
      requests.push(request);
      const next = queue.shift();
      if (!next) throw new Error('no scripted event');
      yield next;
    },
  };
  const updates: RunUpdate[] = [];
  const executor = new AiExecutor({
    provider,
    capture: (id) => svc.capture(id),
    modelContext: (s) => svc.modelContext(s),
    facade,
    capabilities: registry,
    ledger: new MemoryExecutionLedger(),
    queue: new PollingQueuePort({ read: () => [], cancel: async () => undefined }),
    prefs: () => ({ autoCreate: options.autoCreate === true }),
    // A guard with a stable backend fingerprint by default (a chain that deletes needs one).
    guard: options.guard ?? {
      register: async () => ({ ok: true as const }),
      authorize: async () => ({ ok: true as const }),
      scopeDigestOf: () => 'kit-digest',
    },
    listItems: async (plan, offset, limit) => {
      if (plan.operation.kind !== 'archive.create' && plan.operation.kind !== 'items.delete')
        return { rows: [], total: 0 };
      const entries = resolver.entries(plan.operation.sourceManifest);
      return {
        rows: entries
          .slice(offset, offset + limit)
          .map((e) => ({ ref: e.path, label: e.name, detail: e.isDir ? 'folder' : 'file' })),
        total: entries.length,
      };
    },
    ...(options.noNavigate ? {} : { navigate }),
    ...(options.noVerify ? {} : { verifyArchive: verify }),
  });
  executor.subscribe((u) => updates.push(u));

  const stateOf = (runId: string): RunState | undefined =>
    [...updates].reverse().find((u) => u.runId === runId)?.state;
  const runIds = (): string[] => [...new Set(updates.map((u) => u.runId))];
  const last = (runId: string): RunUpdate | undefined =>
    [...updates].reverse().find((u) => u.runId === runId);
  return {
    host,
    executor,
    updates,
    requests,
    order,
    navigate,
    recycle,
    verify,
    registry,
    stateOf,
    censusCalls: () => censusCalls,
    runIds,
    last,
    submit(runId = 'run-1', text = options.userText ?? USER_TEXT) {
      executor.submit({ requestId: 'req_1', text, runId });
    },
  };
}

export type Kit = ReturnType<typeof buildKit>;

/** The owner's three steps as a provider chain event. */
export function ownerChain(
  pane: 'activeAtSubmit' | 'left' | 'right' = 'activeAtSubmit',
): ProviderEvent {
  return {
    kind: 'chain',
    requestId: 'req_1',
    steps: [
      { capability: 'pane.navigate', pane, path: CSV },
      {
        capability: 'archive.create',
        sourcePane: pane,
        selector: { kind: 'selected' },
        format: '7z',
        level: 'ultra',
        target: { kind: 'sourceDirectory' },
      },
      { capability: 'items.delete', sourcePane: pane, selector: { kind: 'selected' } },
    ],
  };
}
