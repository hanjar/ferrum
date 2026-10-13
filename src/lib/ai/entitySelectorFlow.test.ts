// FER-433 / ACT-08 R7 v2: selector `{kind:'entity', entityId}`. Real executor + facade + registry + archive
// adapter + ContextFlow; only the IPC boundary (`archive_create`, `activity_chat_locate`) is faked.
// What is proven: the selector is app-only (never accepted from the model), it is resolved locally at prepare
// time, a vanished/hidden/paused item plans nothing, the plan then follows the normal approval + guard path,
// and no path or entity id is sent to the provider.
import { beforeEach, describe, expect, it, vi } from 'vitest';

const ipc = vi.hoisted(() => ({ archiveCreate: vi.fn() }));
vi.mock('../ipc/archive', () => ({ archiveCreate: ipc.archiveCreate }));

const { ContextService } = await import('./context');
const { ScopeResolver } = await import('./contextResolver');
const { ContextFlow } = await import('./contextFlow');
const { FakeHost, entry: fsEntry } = await import('./contextTestKit');
const { createCommandFacade } = await import('./adapters/index');
const { AiExecutor } = await import('./executor');
const { MemoryExecutionLedger } = await import('./executorLedger');
const { PollingQueuePort } = await import('./executorQueue');
const { parseIntent, ContractError } = await import('./contracts');
const { applyEdit, previousIntentSummary } = await import('./followup');
const { entityArchiveEnvelope, entityArchiveName } = await import('./entitySelector');
import type { ActivityQueryPort } from './activityQuery';
import type { RunState, RunUpdate } from './chatStore.svelte';
import type { IntentProvider, ProviderEvent, ProviderPlanRequest } from './provider';

const LEFT = 'D:\\Work';
const ENTITY_ID = 'l00000000000000a1';
const FOLDER = { path: 'C:\\Clients\\Acme', parentPath: 'C:\\Clients', kind: 'folder' };

const listed = (over: Record<string, unknown> = {}) => ({
  id: ENTITY_ID,
  displayName: 'Acme',
  parentHint: '\u2026\\Clients',
  kind: 'folder',
  availability: 'available',
  reasons: ['recent'],
  ...over,
});

function build(
  options: {
    locate?: () => Promise<unknown>;
    entries?: unknown[];
    available?: boolean;
    withLocate?: boolean;
  } = {},
) {
  const host = new FakeHost();
  host.left.path = LEFT;
  host.left.all = [fsEntry('a.txt', { path: `${LEFT}\\a.txt` })];
  host.right.path = 'E:\\Backup';
  const svc = new ContextService(host);
  const resolver = new ScopeResolver(svc);
  const flow = new ContextFlow(svc, resolver);
  const locate = vi.fn(options.locate ?? (async () => FOLDER));
  const measure = vi.fn<(paths: readonly string[], dir: string) => Promise<undefined>>(
    async () => undefined,
  );
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
      measure,
      ...(options.withLocate === false ? {} : { locateEntity: locate }),
    },
    sync: { flow, ctx: svc, readJob: () => undefined },
    actions: () => ({ run: () => undefined }),
  });
  const requests: ProviderPlanRequest[] = [];
  const provider: IntentProvider = {
    async *plan(request: ProviderPlanRequest): AsyncGenerator<ProviderEvent> {
      requests.push(request);
      yield {
        kind: 'activityQuery',
        request: { query: { kind: 'recent', periodDays: 7, entity: 'folder' } },
      };
    },
  };
  const port: ActivityQueryPort = {
    available: vi.fn(() => options.available !== false),
    query: vi.fn(async () => ({
      status: 'entities',
      entries: options.entries ?? [listed()],
      truncated: false,
    })),
    locate,
  };
  const guard = {
    register: vi.fn(async () => ({ ok: true as const })),
    authorize: vi.fn(async () => ({ ok: true as const })),
    revoke: vi.fn(),
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
    activity: port,
    guard,
  });
  ex.subscribe((u) => updates.push(u));
  const stateOf = (runId: string): RunState | undefined =>
    updates.filter((u) => u.runId === runId).at(-1)?.state;
  const last = (runId: string) => updates.filter((u) => u.runId === runId).at(-1)!;
  /** A normal history question: leaves run-1 `answered` with the listed item. */
  const ask = async () => {
    ex.submit({ requestId: 'req_q', text: 'folder apa minggu ini', runId: 'run-1' });
    await vi.waitFor(() => expect(stateOf('run-1')).toBe('answered'));
  };
  const zip = (entityId = ENTITY_ID, runId = 'run-2') =>
    ex.submitEntityArchive({ requestId: `req_${runId}`, runId, sourceRunId: 'run-1', entityId });
  return {
    ex,
    svc,
    facade,
    host,
    updates,
    stateOf,
    last,
    ask,
    zip,
    locate,
    measure,
    guard,
    port,
    requests,
  };
}

beforeEach(() => ipc.archiveCreate.mockReset());

describe('the selector is app-only', () => {
  const env = (selector: unknown) => ({
    schemaVersion: 1,
    requestId: 'req_1',
    intent: {
      capability: 'archive.create',
      sourcePane: 'left',
      selector,
      format: 'zip',
      target: { kind: 'sourceDirectory' },
    },
  });

  it('parseIntent (model output) rejects kind "entity"; the app flag accepts only the exact shape', () => {
    const e = env({ kind: 'entity', entityId: ENTITY_ID });
    expect(() => parseIntent(structuredClone(e), 'req_1')).toThrow(ContractError);
    const ok = parseIntent(structuredClone(e), 'req_1', { allowEntitySelector: true });
    expect(ok.intent).toMatchObject({ selector: { kind: 'entity', entityId: ENTITY_ID } });
    for (const bad of [
      { kind: 'entity' },
      { kind: 'entity', entityId: '' },
      { kind: 'entity', entityId: 'C:\\secret\\path' },
      { kind: 'entity', entityId: 'x'.repeat(129) },
      { kind: 'entity', entityId: ENTITY_ID, extensions: ['pdf'] },
      { kind: 'entity', entityId: 42 },
    ]) {
      expect(() =>
        parseIntent(structuredClone(env(bad)), 'req_1', { allowEntitySelector: true }),
      ).toThrow(ContractError);
    }
    // `entityId` on another selector kind is still an unknown field.
    expect(() =>
      parseIntent(structuredClone(env({ kind: 'selected', entityId: ENTITY_ID })), 'req_1', {
        allowEntitySelector: true,
      }),
    ).toThrow(ContractError);
  });

  it('the facade refuses the selector unless the executor passes the app-only flag', async () => {
    const t = build();
    const snapshot = t.svc.capture('req_x');
    const refused = await t.facade.prepare(entityArchiveEnvelope('req_x', ENTITY_ID), { snapshot });
    expect(refused.ok).toBe(false);
    if (!refused.ok) expect(refused.error.code).toBe('invalid_intent');
    expect(t.locate).not.toHaveBeenCalled();
  });
});

describe('Buat ZIP from a history answer', () => {
  it('resolves the item at prepare time into a one-item plan in the ACTIVE pane, then follows approval + guard', async () => {
    const t = build();
    await t.ask();
    expect(t.locate).not.toHaveBeenCalled(); // nothing is resolved while only listing
    expect(t.zip()).toBe(true);
    await vi.waitFor(() => expect(t.stateOf('run-2')).toBe('awaitingApproval'));
    expect(t.locate).toHaveBeenCalledWith(ENTITY_ID);
    const plan = t.last('run-2').plan!;
    expect(plan.operation).toMatchObject({
      kind: 'archive.create',
      targetDirectory: LEFT,
      outputName: 'Acme.zip',
    });
    expect(plan.preview.itemCount).toBe(1);
    expect(t.last('run-2').scopeNote).toContain('0 file dan 1 folder');
    expect(t.measure.mock.calls[0][0]).toEqual(['C:\\Clients\\Acme']);
    expect(t.guard.register).toHaveBeenCalledTimes(1);
    expect(ipc.archiveCreate).not.toHaveBeenCalled(); // nothing is written before approval

    ipc.archiveCreate.mockResolvedValue(`${LEFT}\\Acme.zip`);
    t.ex.approve('run-2');
    await vi.waitFor(() => expect(t.stateOf('run-2')).toBe('succeeded'));
    expect(ipc.archiveCreate.mock.calls[0][0]).toEqual(['C:\\Clients\\Acme']);
    expect(t.guard.authorize).toHaveBeenCalledTimes(1);
  });

  it('a file item is zipped under its stem', async () => {
    const t = build({
      locate: async () => ({
        path: 'C:\\Docs\\laporan 2026.pdf',
        parentPath: 'C:\\Docs',
        kind: 'file',
      }),
      entries: [listed({ kind: 'file', displayName: 'laporan 2026.pdf' })],
    });
    await t.ask();
    t.zip();
    await vi.waitFor(() => expect(t.stateOf('run-2')).toBe('awaitingApproval'));
    expect(t.last('run-2').plan!.operation).toMatchObject({ outputName: 'laporan 2026.zip' });
    expect(t.last('run-2').scopeNote).toContain('1 file dan 0 folder');
  });

  it.each([
    ['moved/hidden/paused/private (locate gives null)', async () => null],
    ['locate throws', async () => Promise.reject(new Error('ipc'))],
    [
      'locate gives a malformed location',
      async () => ({ path: '..\\x', parentPath: 'C:\\', kind: 'file' }),
    ],
  ])('plans nothing when %s', async (_name, locate) => {
    const t = build({ locate });
    await t.ask();
    t.zip();
    await vi.waitFor(() => expect(t.stateOf('run-2')).toBe('needsInput'));
    expect(t.last('run-2').errorMessage).toContain('tidak dapat ditemukan lagi');
    expect(t.last('run-2').plan).toBeUndefined();
    expect(t.guard.register).not.toHaveBeenCalled();
    expect(ipc.archiveCreate).not.toHaveBeenCalled();
  });

  it('refuses ids the source answer never listed, missing/offline items, and a run that is not an answer', async () => {
    const t = build({
      entries: [
        listed(),
        listed({ id: 'l00000000000000b2', availability: 'missing' }),
        listed({ id: 'l00000000000000c3', availability: 'offline' }),
      ],
    });
    expect(t.zip()).toBe(false); // no source run yet
    await t.ask();
    expect(t.zip('l00000000000000ff')).toBe(false);
    expect(t.zip('l00000000000000b2')).toBe(false);
    expect(t.zip('l00000000000000c3')).toBe(false);
    expect(t.zip(ENTITY_ID, 'run-1')).toBe(false); // run id already used
    expect(t.locate).not.toHaveBeenCalled();
  });

  it('is refused while the activity service is unavailable, or when the port has no locate', async () => {
    const t = build();
    await t.ask();
    vi.mocked(t.port.available).mockReturnValue(false);
    expect(t.zip()).toBe(false);
    const u = build();
    await u.ask();
    delete (u.port as { locate?: unknown }).locate;
    expect(u.zip()).toBe(false);
  });

  it('without a locateEntity dependency the archive adapter refuses the selector', async () => {
    const t = build({ withLocate: false });
    await t.ask();
    t.zip();
    await vi.waitFor(() => expect(t.stateOf('run-2')).toBe('needsInput'));
    expect(t.guard.register).not.toHaveBeenCalled();
  });

  it('a non-directory active pane (e.g. search results) cannot take the ZIP', async () => {
    const t = build();
    t.host.left.viewKind = 'search';
    await t.ask();
    t.zip();
    await vi.waitFor(() => expect(t.stateOf('run-2')).toBe('needsInput'));
    expect(t.last('run-2').errorMessage).toContain('hasil pencarian');
  });

  it('the provider only ever saw the question: no path, no entity id, no second turn', async () => {
    const t = build();
    await t.ask();
    t.zip();
    await vi.waitFor(() => expect(t.stateOf('run-2')).toBe('awaitingApproval'));
    expect(t.requests).toHaveLength(1);
    expect(JSON.stringify(t.requests)).not.toContain(ENTITY_ID);
    expect(JSON.stringify(t.requests)).not.toContain('Clients');
  });
});

describe('follow-up on an entity plan', () => {
  it('rename and target edits work; filter and "hanya file" are refused', async () => {
    const t = build();
    await t.ask();
    t.zip();
    await vi.waitFor(() => expect(t.stateOf('run-2')).toBe('awaitingApproval'));
    const plan = t.last('run-2').plan!;
    const ref = { planId: plan.planId, revision: 1 };

    const refused = t.ex.followUp('run-2', ref, 'hanya file');
    expect(refused).toMatchObject({ accepted: false, code: 'invalid_edit' });
    expect(t.stateOf('run-2')).toBe('awaitingApproval');

    const renamed = t.ex.followUp('run-2', ref, 'namanya arsip-acme.zip');
    expect(renamed).toMatchObject({ accepted: true, local: true });
    await vi.waitFor(() => {
      expect(t.stateOf('run-2')).toBe('awaitingApproval');
      expect(t.last('run-2').revision).toBe(2);
    });
    expect(t.last('run-2').plan!.operation).toMatchObject({ outputName: 'arsip-acme.zip' });
    expect(t.locate).toHaveBeenCalledTimes(2); // re-resolved at the new prepare, not cached
  });

  it('applyEdit rejects extension filters for the entity selector and keeps the envelope valid', () => {
    const env = entityArchiveEnvelope('req_1', ENTITY_ID);
    expect(applyEdit(env, { kind: 'filter', extensions: ['pdf'] })).toMatchObject({ ok: false });
    expect(applyEdit(env, { kind: 'clearFilter' })).toMatchObject({ ok: false });
    expect(applyEdit(env, { kind: 'filesOnly' })).toMatchObject({ ok: false });
    const renamed = applyEdit(env, { kind: 'rename', outputName: 'x.zip' });
    expect(renamed.ok).toBe(true);
  });

  it('the previous-plan summary sent to a provider never carries the entity id', () => {
    const summary = previousIntentSummary(entityArchiveEnvelope('req_1', ENTITY_ID));
    expect(summary.selector).toEqual({ kind: 'entity' });
    expect(JSON.stringify(summary)).not.toContain(ENTITY_ID);
  });
});

describe('entityArchiveName', () => {
  const loc = (path: string, kind: 'file' | 'folder') => ({ path, parentPath: 'C:\\', kind });
  it('uses the folder name, the file stem, and leaves dotfiles whole', () => {
    expect(entityArchiveName(loc('C:\\a\\Acme', 'folder'))).toBe('Acme.zip');
    expect(entityArchiveName(loc('C:\\a\\Acme.v2', 'folder'))).toBe('Acme.v2.zip');
    expect(entityArchiveName(loc('C:\\a\\laporan.pdf', 'file'))).toBe('laporan.zip');
    expect(entityArchiveName(loc('C:\\a\\.env', 'file'))).toBe('.env.zip');
  });
  it('gives undefined for names the ZIP rules refuse (reserved, trailing dot), so the target default applies', () => {
    expect(entityArchiveName(loc('C:\\a\\CON', 'folder'))).toBeUndefined();
    expect(entityArchiveName(loc('C:\\a\\x.', 'folder'))).toBeUndefined();
  });
});
