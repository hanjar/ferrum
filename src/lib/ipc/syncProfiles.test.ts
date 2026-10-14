import { beforeEach, describe, expect, it, vi } from 'vitest';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const invokeMock = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invokeMock(...a) }));

const { defaultBackend, SyncProfilesError, resetMemoryBackend, createMemoryBackend } =
  await import('./syncProfiles');

const here = dirname(fileURLToPath(import.meta.url));
const fixtureDir = join(here, '../../../contract-fixtures/automation');
const fixture = (name: string): unknown =>
  JSON.parse(readFileSync(join(fixtureDir, name), 'utf-8'));

function fakeTauri() {
  (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {};
}
function unTauri() {
  delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__;
}

const DRAFT = {
  name: 'Photos to NAS',
  left: 'D:\\Photos',
  right: '\\\\nas\\photos',
  mode: 'mirror' as const,
  from: 'left' as const,
  deletePolicy: 'versioning' as const,
  versioningRoot: 'E:\\Versions',
  compare: 'sizeAndDate' as const,
  include: [],
  exclude: ['*.tmp'],
  guard: { enabled: true, maxCount: 50, maxPercent: 20 },
};

beforeEach(() => {
  invokeMock.mockReset().mockName('invoke');
  unTauri();
  resetMemoryBackend();
});

describe('sync profile IPC wrappers', () => {
  it('maps the backend replies and keeps verbatim arguments', async () => {
    fakeTauri();
    const backend = defaultBackend();
    const list = fixture('sync-profile-list.json');
    invokeMock.mockResolvedValueOnce(list);
    expect(await backend.list()).toEqual(list);
    expect(invokeMock).toHaveBeenLastCalledWith('sync_profile_list', undefined);

    const got = fixture('sync-profile-get.json');
    invokeMock.mockResolvedValueOnce(got);
    expect(await backend.get('3f2a9c1e-5b6d-4c8a-9e0f-1a2b3c4d5e6f')).toEqual(got);
    expect(invokeMock).toHaveBeenLastCalledWith('sync_profile_get', {
      id: '3f2a9c1e-5b6d-4c8a-9e0f-1a2b3c4d5e6f',
    });

    const validated = fixture('sync-profile-validate.json');
    invokeMock.mockResolvedValueOnce(validated);
    expect(await backend.validate(DRAFT)).toEqual(validated);
    expect(invokeMock).toHaveBeenLastCalledWith('sync_profile_validate', { draft: DRAFT });
  });

  it('sends the draft and the loaded revision, never a "confirmed" flag (SE-1)', async () => {
    fakeTauri();
    const saved = fixture('sync-profile-save.json');
    invokeMock.mockResolvedValueOnce(saved);
    const r = await defaultBackend().save({ draft: DRAFT });
    expect(r).toEqual(saved);
    expect(invokeMock).toHaveBeenLastCalledWith('sync_profile_save', {
      id: null,
      expectedRevision: null,
      draft: DRAFT,
    });
    // The webview has no way to say "already confirmed" — the native dialog is backend-only.
    expect(JSON.stringify(invokeMock.mock.lastCall)).not.toMatch(/confirm/i);

    invokeMock.mockResolvedValueOnce({ status: 'stale' });
    const r2 = await defaultBackend().save({
      id: '3f2a9c1e-5b6d-4c8a-9e0f-1a2b3c4d5e6f',
      expectedRevision: 3,
      draft: DRAFT,
    });
    expect(r2).toEqual({ status: 'stale' });
    expect(invokeMock).toHaveBeenLastCalledWith('sync_profile_save', {
      id: '3f2a9c1e-5b6d-4c8a-9e0f-1a2b3c4d5e6f',
      expectedRevision: 3,
      draft: DRAFT,
    });
  });

  it('sends exactly the request fixtures that the Rust command tests are held to', async () => {
    fakeTauri();
    type Body = { id?: string | null; expectedRevision?: number | null; draft: typeof DRAFT };
    // Flat top-level arguments - never wrapped in { req: ... } (the shape Tauri maps per
    // parameter name). The Rust side asserts the same fixtures against the command signature.
    const create = fixture('sync-profile-save.request.json') as Body;
    invokeMock.mockResolvedValueOnce({ status: 'stale' });
    await defaultBackend().save({ draft: create.draft });
    expect(invokeMock).toHaveBeenLastCalledWith('sync_profile_save', create);

    const update = fixture('sync-profile-save-update.request.json') as Body;
    invokeMock.mockResolvedValueOnce({ status: 'stale' });
    await defaultBackend().save({
      id: update.id as string,
      expectedRevision: update.expectedRevision as number,
      draft: update.draft,
    });
    expect(invokeMock).toHaveBeenLastCalledWith('sync_profile_save', update);

    const validate = fixture('sync-profile-validate.request.json') as Body;
    invokeMock.mockResolvedValueOnce(fixture('sync-profile-validate.json'));
    await defaultBackend().validate(validate.draft);
    expect(invokeMock).toHaveBeenLastCalledWith('sync_profile_validate', validate);

    const get = fixture('sync-profile-get.request.json') as { id: string };
    invokeMock.mockResolvedValueOnce(fixture('sync-profile-get.json'));
    await defaultBackend().get(get.id);
    expect(invokeMock).toHaveBeenLastCalledWith('sync_profile_get', get);

    const del = fixture('sync-profile-delete.request.json') as {
      id: string;
      expectedRevision: number;
    };
    invokeMock.mockResolvedValueOnce(fixture('sync-profile-delete.json'));
    await defaultBackend().delete(del.id, del.expectedRevision);
    expect(invokeMock).toHaveBeenLastCalledWith('sync_profile_delete', del);

    for (const call of invokeMock.mock.calls) {
      expect(Object.keys(call[1] ?? {})).not.toContain('req');
      expect(JSON.stringify(call)).not.toMatch(/confirm/i);
    }
  });

  it('sends the id and the expected revision to delete (SP-9.5 guarded in the backend)', async () => {
    fakeTauri();
    invokeMock.mockResolvedValueOnce(fixture('sync-profile-delete.json'));
    expect(await defaultBackend().delete('3f2a9c1e-5b6d-4c8a-9e0f-1a2b3c4d5e6f', 4)).toEqual({
      status: 'deleted',
    });
    expect(invokeMock).toHaveBeenLastCalledWith('sync_profile_delete', {
      id: '3f2a9c1e-5b6d-4c8a-9e0f-1a2b3c4d5e6f',
      expectedRevision: 4,
    });
    expect(JSON.stringify(invokeMock.mock.lastCall)).not.toMatch(/confirm/i);
  });

  it('maps backend rejections to closed-set codes', async () => {
    fakeTauri();
    invokeMock.mockRejectedValueOnce({ code: 'stale', message: 'changed elsewhere' });
    await expect(defaultBackend().save({ draft: DRAFT })).rejects.toMatchObject({ code: 'stale' });
    invokeMock.mockRejectedValueOnce({ code: 'store_read_only', message: 'restart' });
    await expect(defaultBackend().list()).rejects.toBeInstanceOf(SyncProfilesError);
    invokeMock.mockRejectedValueOnce('boom');
    await expect(defaultBackend().get('x')).rejects.toMatchObject({ code: 'unknown' });
  });

  it('keeps every wire field of the contract fixtures intact', () => {
    // list
    const list = fixture('sync-profile-list.json') as import('./syncProfiles').ProfileSummary[];
    expect(list).toHaveLength(2);
    expect(list[0].readiness).toEqual({ status: 'ready' });
    expect(list[1].readiness).toEqual({ status: 'notReady', reason: 'identityMismatch' });
    expect(list[0].usedBy).toBe(0);
    // get: identities and the revision changes list survive
    const got = fixture('sync-profile-get.json') as import('./syncProfiles').ProfileDto;
    expect(got.identities.left).toMatchObject({ kind: 'local', drive: 'fixed' });
    expect(got.identities.right).toEqual({ kind: 'network', uncPrefix: '\\\\nas\\photos' });
    expect(got.changes[0].changes[0]).toEqual({
      field: 'rightRoot',
      from: '\\\\nas\\old\\photos',
      to: '\\\\nas\\photos',
    });
    // validate: empty errors with probed identities and kinds
    const validated = fixture(
      'sync-profile-validate.json',
    ) as import('./syncProfiles').ValidateResult;
    expect(validated.errors).toEqual([]);
    expect(validated.kinds).toEqual({ left: 'fixed', right: 'network' });
    // save: the saved profile echoes the policy
    const save = fixture('sync-profile-save.json') as import('./syncProfiles').SaveResult;
    expect(save.status).toBe('saved');
    if (save.status === 'saved') {
      expect(save.profile.policy.deletePolicy).toBe('permanent');
      expect(save.profile.revision).toBe(1);
    }
  });
});

describe('browser-dev fallback', () => {
  it('keeps profiles in memory with revision bookkeeping', async () => {
    const backend = createMemoryBackend([DRAFT]);
    const list = await backend.list();
    expect(list).toHaveLength(1);
    const saved = await backend.save({
      id: list[0].id,
      expectedRevision: 1,
      draft: { ...DRAFT, name: 'Renamed' },
    });
    expect(saved.status).toBe('saved');
    if (saved.status === 'saved') expect(saved.profile.revision).toBe(2);
    await expect(
      backend.save({ id: list[0].id, expectedRevision: 1, draft: DRAFT }),
    ).rejects.toMatchObject({ code: 'stale' });
    await expect(backend.delete(list[0].id, 1)).rejects.toMatchObject({ code: 'stale' });
    await expect(backend.delete(list[0].id, 2)).resolves.toEqual({ status: 'deleted' });
    await expect(backend.get(list[0].id)).rejects.toMatchObject({ code: 'not_found' });
    await expect(backend.validate(DRAFT)).rejects.toMatchObject({ code: 'unsupported' });
    await expect(backend.preview('x')).rejects.toMatchObject({ code: 'unsupported' });
    await expect(
      backend.run({ id: 'x', previewId: 'y', confirmOverwrite: false }),
    ).rejects.toMatchObject({ code: 'unsupported' });
    expect(invokeMock).not.toHaveBeenCalled();
  });
});

describe('manual preview and run wrappers (P0.2)', () => {
  it('sends exactly the request fixtures that the Rust command tests are held to', async () => {
    fakeTauri();
    const preview = fixture('sync-profile-preview.request.json') as { id: string };
    invokeMock.mockResolvedValueOnce(fixture('sync-profile-preview.json'));
    const got = await defaultBackend().preview(preview.id);
    expect(got).toEqual(fixture('sync-profile-preview.json'));
    expect(invokeMock).toHaveBeenLastCalledWith('sync_profile_preview', preview);

    const run = fixture('sync-profile-run.request.json') as {
      id: string;
      previewId: string;
      confirmOverwrite: boolean;
    };
    invokeMock.mockResolvedValueOnce(fixture('sync-profile-run.json'));
    const started = await defaultBackend().run(run);
    expect(started).toEqual({ runId: 1790000000123 });
    // Flat top-level arguments, exactly the fixture; no nested request object.
    expect(invokeMock).toHaveBeenLastCalledWith('sync_profile_run', run);
    expect(Object.keys(invokeMock.mock.lastCall?.[1] ?? {}).sort()).toEqual([
      'confirmOverwrite',
      'id',
      'previewId',
    ]);
  });

  it('never lets the webview say a permanent delete was confirmed (SE-1)', async () => {
    fakeTauri();
    invokeMock.mockResolvedValueOnce({ runId: 1 });
    await defaultBackend().run({
      id: 'a',
      previewId: 'b',
      confirmOverwrite: true,
      // @ts-expect-error extra keys are not part of the wrapper input and must not be forwarded
      confirmed: true,
      confirmPermanent: true,
    });
    const args = invokeMock.mock.lastCall?.[1] as Record<string, unknown>;
    expect(Object.keys(args)).not.toContain('confirmed');
    expect(Object.keys(args)).not.toContain('confirmPermanent');
  });

  it('keeps every wire field of the preview and event fixtures intact', () => {
    const ok = fixture('sync-profile-preview.json') as import('./syncProfiles').SyncPreview;
    expect(ok.summary.deletes).toBe(3);
    expect(ok.guard).toEqual({ verdict: 'ok' });
    expect(ok.blockers).toBeUndefined();
    const tripped = fixture(
      'sync-profile-preview-tripped.json',
    ) as import('./syncProfiles').SyncPreview;
    expect(tripped.guard).toEqual({
      verdict: 'tripped',
      side: 'right',
      deletes: 51,
      entries: 100,
      percent: 51,
    });
    const blocked = fixture(
      'sync-profile-preview-blocked.json',
    ) as import('./syncProfiles').SyncPreview;
    expect(blocked.previewId).toBe('');
    expect(blocked.blockers).toEqual(['truncated']);
    const phase = fixture(
      'sync-profile-run-event-phase.json',
    ) as import('./syncProfiles').SyncRunEvent;
    expect(phase.phase).toBe('comparing');
    const counts = fixture(
      'sync-profile-run-event-counts.json',
    ) as import('./syncProfiles').SyncRunEvent;
    expect(counts.counts?.notStarted).toBe(3);
    const result = fixture(
      'sync-profile-run-event-result.json',
    ) as import('./syncProfiles').SyncRunEvent;
    expect(result.result?.kind).toBe('partial');
    expect(result.result?.settle).toEqual({ state: 'saved', entries: 12 });
    expect(result.result?.problems.map((p) => p.status)).toEqual([
      'changed',
      'failed',
      'notStarted',
    ]);
    expect(result.result?.problems[1].reason).toBe('access_denied');
  });

  it('maps rejections of the new commands to closed-set codes', async () => {
    fakeTauri();
    for (const code of ['stale', 'not_ready', 'lease_busy', 'canceled', 'busy', 'invalid_input']) {
      invokeMock.mockRejectedValueOnce({ code, message: 'm' });
      await expect(defaultBackend().preview('x')).rejects.toMatchObject({ code });
      invokeMock.mockRejectedValueOnce({ code, message: 'm' });
      await expect(
        defaultBackend().run({ id: 'x', previewId: 'y', confirmOverwrite: false }),
      ).rejects.toMatchObject({ code });
    }
  });
});
