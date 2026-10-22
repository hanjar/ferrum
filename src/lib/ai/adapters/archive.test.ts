// FER-165 / FER-203..206: ZIP from chat at its edges. Real executor + facade + registry + archive adapter; only the
// IPC boundary (`archive_create`) is mocked here. The engine side of every edge (output inside the source subtree,
// unreadable input, full drive) is tested on real files in `src-tauri/src/archive_create_test.rs` (`edge_tests`),
// and end to end by `scripts/ai-desktop-smoke-zip-edge.mjs`. These tests do not replace those.
import { beforeEach, describe, expect, it, vi } from 'vitest';

const ipc = vi.hoisted(() => ({ archiveCreate: vi.fn() }));
vi.mock('../../ipc/archive', () => ({ archiveCreate: ipc.archiveCreate }));

const { FsError } = await import('../../ipc/types');
const { ContextService } = await import('../context');
const { ScopeResolver } = await import('../contextResolver');
const { ContextFlow } = await import('../contextFlow');
const { FakeHost, entry } = await import('../contextTestKit');
const { createCommandFacade, PACK_UNAVAILABLE_REASON } = await import('./index');
const { AiExecutor } = await import('../executor');
const { MemoryExecutionLedger } = await import('../executorLedger');
const { PollingQueuePort } = await import('../executorQueue');
const { PACK_NO_SPACE, PACK_SOURCE_UNREADABLE } = await import('../../packService');
import type { PackMeasure } from '../../packService';
import type { RunState, RunUpdate } from '../chatStore.svelte';
import type { IntentProvider, ProviderEvent, ProviderPlanRequest } from '../provider';

const LEFT = 'D:\\Projects\\Invoice';

interface BuildOptions {
  packAvailable?: () => boolean;
  measure?: (paths: readonly string[], targetDirectory: string) => Promise<PackMeasure | undefined>;
  target?: { kind: 'sourceDirectory' } | { kind: 'pane'; pane: 'left' | 'right' };
  rightPath?: string;
  format?: 'zip' | '7z';
  level?: 'store' | 'fast' | 'normal' | 'maximum' | 'ultra' | 'ultra-text';
  outputName?: string;
}

function build(options: BuildOptions = {}) {
  const host = new FakeHost();
  host.left.path = LEFT;
  host.left.all = [
    entry('a.txt', { path: `${LEFT}\\a.txt` }),
    entry('b.txt', { path: `${LEFT}\\b.txt` }),
    entry('sub', { path: `${LEFT}\\sub`, isDir: true }),
  ];
  host.right.path = options.rightPath ?? 'E:\\Backup';
  const svc = new ContextService(host);
  const resolver = new ScopeResolver(svc);
  const flow = new ContextFlow(svc, resolver);
  const measure = vi.fn(options.measure ?? (async () => undefined));
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
      packAvailable: options.packAvailable,
      measure,
    },
    sync: { flow, ctx: svc, readJob: () => undefined },
    actions: () => ({ run: () => undefined }),
  });
  const requests: Array<ProviderPlanRequest & { capabilities?: readonly string[] }> = [];
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
            format: options.format ?? 'zip',
            ...(options.level !== undefined ? { level: options.level } : {}),
            ...(options.outputName !== undefined ? { outputName: options.outputName } : {}),
            target: options.target ?? { kind: 'sourceDirectory' },
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
  const submit = async (runId = 'run-1') => {
    ex.submit({
      requestId: `req_${runId}`,
      text: 'arsipkan semua file di folder kiri jadi zip',
      runId,
    } as never);
    await vi.waitFor(() =>
      expect(['awaitingApproval', 'blockedCapability', 'failed']).toContain(stateOf(runId)),
    );
    return updates.find((u) => u.runId === runId && u.state === 'awaitingApproval');
  };
  /** Straight through the facade (no executor): the adapter's own result, code and cause. */
  const runFacade = async () => {
    const snapshot = svc.capture('req_facade');
    const prepared = await facade.prepare(
      {
        schemaVersion: 1,
        requestId: 'req_facade',
        intent: {
          capability: 'archive.create',
          sourcePane: 'left',
          selector: { kind: 'allVisible', entries: 'all' },
          format: 'zip',
          target: options.target ?? { kind: 'sourceDirectory' },
        },
      },
      { snapshot },
    );
    if (!prepared.ok) throw new Error(prepared.error.message);
    const approved = facade.approve(prepared.value.handle.handleId);
    if (!approved.ok) throw new Error(approved.error.message);
    return facade.execute(approved.value);
  };
  return { host, ex, updates, stateOf, submit, requests, registry, measure, runFacade };
}

const dump = (updates: RunUpdate[]): string => JSON.stringify(updates);

beforeEach(() => {
  ipc.archiveCreate.mockReset();
});

describe('preview size (totalBytes) — FER-165', () => {
  it('is the measured payload of exactly the selected paths, before any write', async () => {
    const t = build({
      measure: async () => ({
        bytes: 123_456,
        files: 9,
        complete: true,
        unreadable: [],
        freeBytes: 10 ** 12,
      }),
    });
    const preview = await t.submit();
    expect(preview?.plan?.preview.totalBytes).toBe(123_456);
    expect(t.measure).toHaveBeenCalledTimes(1);
    expect(t.measure.mock.calls[0][0]).toEqual([
      `${LEFT}\\a.txt`,
      `${LEFT}\\b.txt`,
      `${LEFT}\\sub`,
    ]);
    expect(t.measure.mock.calls[0][1]).toBe(LEFT);
    expect(ipc.archiveCreate).not.toHaveBeenCalled();
  });

  it('is reported as unknown (0) when the walk was partial, never as a misleading partial number', async () => {
    const t = build({
      measure: async () => ({ bytes: 500, files: 1, complete: false, unreadable: [] }),
    });
    const preview = await t.submit();
    expect(preview?.plan?.preview.totalBytes).toBe(0);
  });

  it('a failing estimate does not block the preview (size unknown)', async () => {
    const t = build({ measure: async () => Promise.reject(new Error('walk broke')) });
    const preview = await t.submit();
    expect(t.stateOf('run-1')).toBe('awaitingApproval');
    expect(preview?.plan?.preview.totalBytes).toBe(0);
  });

  it('warns before approval about unreadable items and about a destination with too little space', async () => {
    const t = build({
      measure: async () => ({
        bytes: 5 * 1024 ** 3,
        files: 3,
        complete: false,
        unreadable: [`${LEFT}\\sub\\private`, `${LEFT}\\sub\\other`],
        freeBytes: 1024 ** 3,
      }),
    });
    const preview = await t.submit();
    const warnings = preview?.warnings ?? [];
    expect(warnings.some((w) => w.includes('private dan 1 lainnya tidak dapat dibaca'))).toBe(true);
    expect(
      warnings.some((w) => /Ruang kosong di drive tujuan \(1\.0 GB\).*\(5\.0 GB\)/.test(w)),
    ).toBe(true);
    expect(ipc.archiveCreate).not.toHaveBeenCalled();
  });
});

describe('output inside the source subtree — FER-205', () => {
  it('a ZIP placed inside a selected folder is announced in the preview; the archive call is unchanged', async () => {
    ipc.archiveCreate.mockResolvedValue(`${LEFT}\\sub\\Invoice.zip`);
    const t = build({ target: { kind: 'pane', pane: 'right' }, rightPath: `${LEFT}\\sub` });
    const preview = await t.submit();
    expect(preview?.plan?.operation).toMatchObject({
      kind: 'archive.create',
      targetDirectory: `${LEFT}\\sub`,
    });
    expect(
      preview?.warnings?.some((w) =>
        /di dalam folder yang diarsipkan; ZIP itu sendiri tidak ikut/.test(w),
      ),
    ).toBe(true);
    t.ex.approve('run-1');
    await vi.waitFor(() => expect(t.stateOf('run-1')).toBe('succeeded'));
    // Sources are the approved manifest; the engine (not the chat) drops its own output from the input.
    expect(ipc.archiveCreate.mock.calls[0][0]).toEqual([
      `${LEFT}\\a.txt`,
      `${LEFT}\\b.txt`,
      `${LEFT}\\sub`,
    ]);
    expect(ipc.archiveCreate.mock.calls[0][3]).toBe('skip');
  });

  it('a ZIP next to the sources (default target) carries no subtree warning', async () => {
    const t = build();
    const preview = await t.submit();
    expect(preview?.warnings?.some((w) => w.includes('di dalam folder yang diarsipkan'))).toBe(
      false,
    );
  });
});

describe('engine failures carry a cause and are never success — FER-205', () => {
  // Contract with FER-163 (executor): `execution_failed` + cause => `failed` with that cause; no success, no retry,
  // no engine text.
  const cases = [
    {
      name: 'unreadable input',
      error: () =>
        new FsError({
          code: 'io',
          message: `${PACK_SOURCE_UNREADABLE}: locked.txt: D:\\secret\\proses lain memakai file ini (os error 32)`,
          path: `${LEFT}\\sub\\locked.txt`,
        }),
      cause: 'sourceUnreadable',
      message:
        /^Item sumber "locked\.txt" tidak dapat dibaca .*Invoice\.zip tidak dibuat; tidak ada file yang ditulis/,
    },
    {
      name: 'full drive',
      error: () =>
        new FsError({ code: 'io', message: `${PACK_NO_SPACE}; nothing was written D:\\secret` }),
      cause: 'diskFull',
      message: /^Ruang disk di drive tujuan tidak cukup\./,
    },
    {
      name: 'access denied',
      error: () => new FsError({ code: 'permissionDenied', message: 'engine says D:\\secret' }),
      cause: 'permissionDenied',
      message: /^Akses ditolak\./,
    },
    {
      name: 'other engine error',
      error: () => new FsError({ code: 'notFound', message: 'engine says D:\\secret' }),
      cause: 'notFound',
      message: /^Engine arsip gagal \(notFound\)\./,
    },
  ];

  for (const c of cases) {
    it(`${c.name}: execution_failed with cause "${c.cause}" and a safe message`, async () => {
      ipc.archiveCreate.mockRejectedValue(c.error());
      const result = await build().runFacade();
      expect(result.ok).toBe(false);
      if (result.ok) return;
      expect(result.error.code).toBe('execution_failed');
      expect(result.error.cause).toBe(c.cause);
      expect(result.error.message).toMatch(c.message);
      expect(result.error.message).not.toContain('secret');
    });

    it(`${c.name}: through the executor it is failed, not retried and shows no engine text`, async () => {
      ipc.archiveCreate.mockRejectedValue(c.error());
      const t = build();
      await t.submit();
      t.ex.approve('run-1');
      await vi.waitFor(() => expect(t.stateOf('run-1')).toBe('failed'));
      expect(t.updates.some((u) => u.state === 'succeeded')).toBe(false);
      expect(dump(t.updates)).not.toContain('secret');
      expect(dump(t.updates)).not.toContain('os error');
      await new Promise((r) => setTimeout(r, 30));
      expect(ipc.archiveCreate).toHaveBeenCalledTimes(1);
    });
  }

  it('a broken IPC (not an engine answer) has no cause: the outcome stays unknown', async () => {
    ipc.archiveCreate.mockRejectedValue(new TypeError('IPC channel closed'));
    const result = await build().runFacade();
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.error.cause).toBeUndefined();
    const t = build();
    await t.submit();
    t.ex.approve('run-1');
    await vi.waitFor(() => expect(t.stateOf('run-1')).toBe('unknownOutcome'));
    expect(t.updates.some((u) => u.state === 'succeeded' || u.state === 'failed')).toBe(false);
  });

  it('a cancelled engine call is not reported as a failure with a cause', async () => {
    ipc.archiveCreate.mockRejectedValue(new FsError({ code: 'cancelled', message: 'stopped' }));
    const result = await build().runFacade();
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.error.cause).toBeUndefined();
  });
});

describe('cancel during Pack (LIMIT-6) — FER-205', () => {
  it('reports honestly that Pack cannot be cancelled, then the real result', async () => {
    let finish!: (path: string) => void;
    ipc.archiveCreate.mockImplementation(
      () => new Promise<string>((resolve) => (finish = resolve)),
    );
    const t = build();
    await t.submit();
    t.ex.approve('run-1');
    await vi.waitFor(() => expect(ipc.archiveCreate).toHaveBeenCalledTimes(1));
    t.ex.cancel('run-1');
    await vi.waitFor(() =>
      expect(
        t.updates.some((u) =>
          /tidak berjalan lewat antrean dan tidak dapat dibatalkan/.test(u.errorMessage ?? ''),
        ),
      ).toBe(true),
    );
    expect(t.stateOf('run-1')).not.toBe('cancelled');
    finish(`${LEFT}\\Invoice.zip`);
    await vi.waitFor(() => expect(t.stateOf('run-1')).toBe('succeeded'));
    expect(t.updates.some((u) => u.state === 'cancelled')).toBe(false);
    expect(ipc.archiveCreate).toHaveBeenCalledTimes(1);
  });
});

describe('capability follows the Pack service — FER-206', () => {
  it('Pack unavailable: archive is not offered, a forced intent is blocked, the engine is never called', async () => {
    const t = build({ packAvailable: () => false });
    expect(t.registry.availability('archive.create')).toEqual({
      available: false,
      code: 'pending',
      reason: PACK_UNAVAILABLE_REASON,
    });
    expect(t.registry.offeredCapabilityIds()).not.toContain('archive.create');
    await t.submit();
    expect(t.stateOf('run-1')).toBe('blockedCapability');
    expect(t.updates.at(-1)?.blockedReason).toBe(PACK_UNAVAILABLE_REASON);
    expect(t.requests[0].capabilities).not.toContain('archive.create');
    expect(ipc.archiveCreate).not.toHaveBeenCalled();
    expect(t.measure).not.toHaveBeenCalled();
  });

  it('Pack available: the capability is offered and ready', () => {
    const t = build({ packAvailable: () => true });
    expect(t.registry.availability('archive.create')).toEqual({ available: true });
    expect(t.registry.offeredCapabilityIds()).toContain('archive.create');
  });
});

describe('7z and compression level — FER-576', () => {
  it('7z + ultra: the preview says so, the output is .7z, and the Pack call is sevenz/max', async () => {
    ipc.archiveCreate.mockResolvedValue(`${LEFT}\\Invoice.7z`);
    const t = build({ format: '7z', level: 'ultra' });
    const preview = await t.submit();
    expect(preview?.plan?.operation).toMatchObject({
      kind: 'archive.create',
      outputName: 'Invoice.7z',
    });
    const text = JSON.stringify(preview);
    expect(text).toContain('Format 7z');
    expect(text).toContain('maksimum');
    t.ex.approve('run-1');
    await vi.waitFor(() => expect(t.stateOf('run-1')).toBe('succeeded'));
    const call = ipc.archiveCreate.mock.calls[0];
    expect(call[1]).toBe(`${LEFT}\\Invoice.7z`);
    expect(call[2]).toBe('sevenz');
    expect(call[3]).toBe('skip'); // never replaces an existing file
    expect(call[5]).toBe('max');
  });

  it('a user-typed .zip name is turned into .7z, not stacked', async () => {
    ipc.archiveCreate.mockResolvedValue(`${LEFT}\\data.7z`);
    const t = build({ format: '7z', outputName: 'data.zip' });
    const preview = await t.submit();
    expect(preview?.plan?.operation).toMatchObject({ outputName: 'data.7z' });
  });

  it('maps every chat level to an engine level; maximum and ultra are the strongest', async () => {
    const { packLevelOf, packFormatOf } = await import('./archive');
    expect(packLevelOf(undefined)).toBeUndefined();
    expect(packLevelOf('store')).toBe('store');
    expect(packLevelOf('fast')).toBe('fast');
    expect(packLevelOf('normal')).toBe('normal');
    expect(packLevelOf('maximum')).toBe('max');
    expect(packLevelOf('ultra')).toBe('max');
    expect(packLevelOf('ultra-text')).toBe('ultra-text');
    expect(packFormatOf('7z')).toBe('sevenz');
    expect(packFormatOf('zip')).toBe('zip');
  });

  it('zip without a level keeps the old call shape (no level argument)', async () => {
    ipc.archiveCreate.mockResolvedValue(`${LEFT}\\Invoice.zip`);
    const t = build();
    await t.submit();
    t.ex.approve('run-1');
    await vi.waitFor(() => expect(t.stateOf('run-1')).toBe('succeeded'));
    expect(ipc.archiveCreate.mock.calls[0][2]).toBe('zip');
    expect(ipc.archiveCreate.mock.calls[0][5]).toBeUndefined();
  });
});

describe('7z ultra-text (PPMd) — FER-579', () => {
  it('7z + ultra-text: its own card text and the Pack call carries ultra-text', async () => {
    ipc.archiveCreate.mockResolvedValue(`${LEFT}\\Invoice.7z`);
    const t = build({ format: '7z', level: 'ultra-text' });
    const preview = await t.submit();
    const text = JSON.stringify(preview);
    expect(text).toContain('PPMd');
    expect(text).not.toContain('maksimum'); // a different method than maximum/ultra
    t.ex.approve('run-1');
    await vi.waitFor(() => expect(t.stateOf('run-1')).toBe('succeeded'));
    const call = ipc.archiveCreate.mock.calls[0];
    expect(call[2]).toBe('sevenz');
    expect(call[5]).toBe('ultra-text');
  });

  it('the card tells ultra-text apart from maximum', async () => {
    const a = await build({ format: '7z', level: 'ultra-text' }).submit();
    const b = await build({ format: '7z', level: 'maximum' }).submit();
    expect(a?.scopeNote).not.toBe(b?.scopeNote);
  });
});

describe('ultra and maximum are one level (FER-576 review S1)', () => {
  it('show the same card text, and the word ultra never reaches the card', async () => {
    const a = await build({ format: '7z', level: 'ultra' }).submit();
    const b = await build({ format: '7z', level: 'maximum' }).submit();
    expect(a?.scopeNote).toBe(b?.scopeNote);
    expect(JSON.stringify(a)).not.toContain('ultra');
  });
});
