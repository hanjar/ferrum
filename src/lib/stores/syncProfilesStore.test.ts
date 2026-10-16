import { beforeEach, describe, expect, it, vi } from 'vitest';
import {
  SyncProfilesError,
  type ProfileDto,
  type ProfileSummary,
  type SyncPreview,
  type SyncProfilesBackend,
  type SyncRunEvent,
} from '../ipc/syncProfiles';
import { SyncProfilesStore } from './syncProfilesStore.svelte';
import { emptyForm } from '../components/sync/syncProfileModel';

const policy = {
  left: 'D:\\A',
  right: 'E:\\B',
  mode: 'mirror' as const,
  from: 'left' as const,
  deletePolicy: 'recycleBin' as const,
  compare: 'sizeAndDate' as const,
  include: [],
  exclude: ['*.tmp'],
  guard: { enabled: true, maxCount: 50, maxPercent: 20 },
};

const dto = (over: Partial<ProfileDto> = {}): ProfileDto => ({
  id: 'p1',
  name: 'Photos',
  revision: 1,
  policy,
  identities: {},
  changes: [],
  createdMs: 1,
  updatedMs: 1,
  readiness: { status: 'ready' },
  ...over,
});

const summary = (over: Partial<ProfileSummary> = {}): ProfileSummary => ({
  id: 'p1',
  name: 'Photos',
  revision: 1,
  left: 'D:\\A',
  right: 'E:\\B',
  modeLabel: 'Mirror left → right',
  deleteLabel: 'Recycle Bin',
  readiness: { status: 'ready' },
  usedBy: 0,
  ...over,
});

const preview = (over: Partial<SyncPreview> = {}): SyncPreview => ({
  previewId: 'pv1',
  summary: {
    total: 3,
    toRight: 3,
    toLeft: 0,
    overwrites: 0,
    deletes: 0,
    conflicts: 0,
    skipped: 0,
    userSkipped: 0,
  },
  conflicts: 0,
  guard: { verdict: 'ok' },
  ...over,
});

function fakeBackend(over: Partial<SyncProfilesBackend> = {}) {
  const b = {
    list: vi.fn(async () => [summary()]),
    get: vi.fn(async () => dto()),
    validate: vi.fn(async () => ({
      errors: [],
      identities: {},
      kinds: { left: 'fixed', right: 'fixed' },
    })),
    save: vi.fn(async () => ({ status: 'saved' as const, profile: dto({ revision: 2 }) })),
    delete: vi.fn(async () => ({ status: 'deleted' })),
    preview: vi.fn(async () => preview()),
    run: vi.fn(async () => ({ runId: 7 })),
    ...over,
  };
  return b as typeof b & SyncProfilesBackend;
}

function makeStore(backend = fakeBackend()) {
  let emit: (e: SyncRunEvent) => void = () => undefined;
  const unsubscribe = vi.fn();
  const subscribe = vi.fn(async (cb: (e: SyncRunEvent) => void) => {
    emit = cb;
    return unsubscribe;
  });
  const store = new SyncProfilesStore({ backend, subscribe });
  return { store, backend, subscribe, unsubscribe, emit: (e: SyncRunEvent) => emit(e) };
}

beforeEach(() => vi.useRealTimers());

describe('load', () => {
  it('stores the list and marks it loaded', async () => {
    const { store } = makeStore();
    await store.load();
    expect(store.profiles).toHaveLength(1);
    expect(store.loaded).toBe(true);
    expect(store.listError).toBeNull();
  });

  it('shows a message for a failed load and keeps the page usable', async () => {
    const { store } = makeStore(
      fakeBackend({
        list: vi.fn(async () => Promise.reject(new SyncProfilesError('store_unavailable'))),
      }),
    );
    await store.load();
    expect(store.loaded).toBe(true);
    expect(store.listError).toMatch(/unavailable/);
  });
});

describe('editor', () => {
  it('useCurrentPanes fills the active pane as left and the inactive pane as right (SP-1.2)', async () => {
    const { store, backend } = makeStore();
    store.useCurrentPanes('D:\\Work', 'E:\\Backup');
    expect(store.editor?.form.left).toBe('D:\\Work');
    expect(store.editor?.form.right).toBe('E:\\Backup');
    await vi.waitFor(() => expect(backend.validate).toHaveBeenCalled());
  });

  it('shows the errors the backend reports and blocks the save', async () => {
    const validate = vi.fn(async () => ({
      errors: [{ field: 'name', code: 'name_required' }],
      identities: {},
      kinds: {},
    }));
    const { store, backend } = makeStore(fakeBackend({ validate }));
    store.openNew();
    await store.save();
    expect(store.editor?.errors).toEqual([{ field: 'name', code: 'name_required' }]);
    expect(backend.save).not.toHaveBeenCalled();
  });

  it('sends only the fields that apply to the mode and never a confirmation flag', async () => {
    const { store, backend } = makeStore();
    store.openNew({ ...emptyForm(), name: 'x', left: 'D:\\A', right: 'E:\\B', mode: 'twoWay' });
    await store.save();
    const arg = (backend.save as ReturnType<typeof vi.fn>).mock.calls[0][0];
    expect(arg.draft).not.toHaveProperty('from');
    expect(arg.draft.propagateDeletes).toBe(false);
    expect(arg.draft).not.toHaveProperty('versioningRoot');
    expect(JSON.stringify(arg)).not.toMatch(/confirm/i);
  });

  it('saves, closes the editor, reloads and reports the new revision', async () => {
    const { store, backend } = makeStore();
    await store.openEdit('p1');
    expect(store.editor?.revision).toBe(1);
    store.editor!.form.name = 'Renamed';
    store.touch();
    await store.save();
    expect(backend.save).toHaveBeenCalledWith(
      expect.objectContaining({ id: 'p1', expectedRevision: 1 }),
    );
    expect(store.editor).toBeNull();
    expect(store.notice?.text).toMatch(/revision 2/);
    expect(backend.list).toHaveBeenCalled();
  });

  it('a canceled native confirmation saves nothing and says so', async () => {
    const { store } = makeStore(
      fakeBackend({ save: vi.fn(async () => ({ status: 'canceled' as const })) }),
    );
    store.openNew({ name: 'x', left: 'D:\\A', right: 'E:\\B' });
    await store.save();
    expect(store.editor?.notice?.text).toMatch(/nothing was saved/i);
    expect(store.editor).not.toBeNull();
  });

  it('a stale save reloads the current version with the explanation (SP-6.2)', async () => {
    const fresh = dto({ name: 'Changed elsewhere', revision: 4 });
    const get = vi.fn().mockResolvedValueOnce(dto()).mockResolvedValue(fresh);
    const { store } = makeStore(
      fakeBackend({ get, save: vi.fn(async () => ({ status: 'stale' as const })) }),
    );
    await store.openEdit('p1');
    store.editor!.form.name = 'My edit';
    store.touch();
    await store.save();
    expect(store.editor?.form.name).toBe('Changed elsewhere');
    expect(store.editor?.revision).toBe(4);
    expect(store.editor?.notice?.text).toBe(
      'This profile was changed elsewhere. Review the current version.',
    );
    expect(store.editor?.dirty).toBe(false);
  });

  it('a stale error thrown by the backend takes the same path', async () => {
    const get = vi
      .fn()
      .mockResolvedValueOnce(dto())
      .mockResolvedValue(dto({ revision: 3 }));
    const { store } = makeStore(
      fakeBackend({ get, save: vi.fn(async () => Promise.reject(new SyncProfilesError('stale'))) }),
    );
    await store.openEdit('p1');
    await store.save();
    expect(store.editor?.revision).toBe(3);
  });

  it('shows a failed validation call instead of saving blind', async () => {
    const { store, backend } = makeStore(
      fakeBackend({
        validate: vi.fn(async () => Promise.reject(new SyncProfilesError('unsupported'))),
      }),
    );
    store.openNew({ name: 'x' });
    await store.save();
    expect(backend.save).not.toHaveBeenCalled();
    expect(store.editor?.notice?.text).toMatch(/desktop app/);
  });

  it('validates shortly after an edit (debounced)', async () => {
    vi.useFakeTimers();
    const { store, backend } = makeStore();
    store.openNew();
    store.touch();
    store.touch();
    expect(backend.validate).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(300);
    expect(backend.validate).toHaveBeenCalledTimes(1);
  });
});

describe('delete (SP-9.5)', () => {
  it('asks first, then deletes with the loaded revision', async () => {
    const { store, backend } = makeStore();
    await store.load();
    store.askDelete('p1');
    expect(backend.delete).not.toHaveBeenCalled();
    await store.confirmDelete();
    expect(backend.delete).toHaveBeenCalledWith('p1', 1);
    expect(store.notice?.kind).toBe('success');
  });

  it('refuses a profile that an automation uses and says how many', async () => {
    const { store } = makeStore(
      fakeBackend({
        list: vi.fn(async () => [summary({ usedBy: 2 })]),
        delete: vi.fn(async () => Promise.reject(new SyncProfilesError('in_use'))),
      }),
    );
    await store.load();
    store.askDelete('p1');
    await store.confirmDelete();
    expect(store.notice).toEqual({
      kind: 'error',
      text: 'Used by 2 automations — delete or change them first.',
    });
    expect(store.profiles).toHaveLength(1);
  });

  it('cancel does nothing', async () => {
    const { store, backend } = makeStore();
    await store.load();
    store.askDelete('p1');
    store.cancelDelete();
    await store.confirmDelete();
    expect(backend.delete).not.toHaveBeenCalled();
  });
});

describe('preview and run', () => {
  it('shows the preview summary', async () => {
    const { store } = makeStore();
    await store.load();
    await store.previewProfile('p1');
    expect(store.preview?.data.summary.total).toBe(3);
    expect(store.canRun('p1')).toBe(true);
  });

  it('cannot run without a preview', async () => {
    const { store, backend } = makeStore();
    await store.load();
    await store.requestRun('p1');
    expect(backend.run).not.toHaveBeenCalled();
  });

  it('a run without overwrites starts with confirmOverwrite false', async () => {
    const { store, backend } = makeStore();
    await store.load();
    await store.previewProfile('p1');
    await store.requestRun('p1');
    expect(backend.run).toHaveBeenCalledWith({
      id: 'p1',
      previewId: 'pv1',
      confirmOverwrite: false,
    });
    expect(store.run?.status).toBe('running');
    expect(store.run?.runId).toBe(7);
    expect(store.preview).toBeNull();
  });

  it('overwrites need the prompt: nothing runs until it is confirmed', async () => {
    const withOverwrites = preview({
      summary: { ...preview().summary, overwrites: 2 },
    });
    const { store, backend } = makeStore(
      fakeBackend({ preview: vi.fn(async () => withOverwrites) }),
    );
    await store.load();
    await store.previewProfile('p1');
    await store.requestRun('p1');
    expect(store.runPrompt).toEqual({
      profileId: 'p1',
      previewId: 'pv1',
      overwrites: 2,
      deletes: 0,
    });
    expect(backend.run).not.toHaveBeenCalled();
    await store.confirmRunPrompt();
    expect(backend.run).toHaveBeenCalledWith({
      id: 'p1',
      previewId: 'pv1',
      confirmOverwrite: true,
    });
  });

  it('confirmRunPrompt without an open prompt does nothing', async () => {
    const { store, backend } = makeStore();
    await store.load();
    await store.previewProfile('p1');
    await store.confirmRunPrompt();
    expect(backend.run).not.toHaveBeenCalled();
  });

  it('cancelling the prompt keeps the preview and starts nothing', async () => {
    const withOverwrites = preview({ summary: { ...preview().summary, overwrites: 1 } });
    const { store, backend } = makeStore(
      fakeBackend({ preview: vi.fn(async () => withOverwrites) }),
    );
    await store.load();
    await store.previewProfile('p1');
    await store.requestRun('p1');
    store.cancelRunPrompt();
    expect(store.runPrompt).toBeNull();
    expect(backend.run).not.toHaveBeenCalled();
    expect(store.preview).not.toBeNull();
  });

  it('blockers and an empty source disable the run', async () => {
    const blocked = makeStore(
      fakeBackend({
        preview: vi.fn(async () => preview({ previewId: '', blockers: ['truncated'] })),
      }),
    );
    await blocked.store.load();
    await blocked.store.previewProfile('p1');
    expect(blocked.store.canRun('p1')).toBe(false);

    const empty = makeStore(
      fakeBackend({
        preview: vi.fn(async () => preview({ guard: { verdict: 'emptySource', empty: 'left' } })),
      }),
    );
    await empty.store.load();
    await empty.store.previewProfile('p1');
    expect(empty.store.canRun('p1')).toBe(false);
  });

  it('a profile that is not ready cannot run', async () => {
    const { store } = makeStore(
      fakeBackend({
        list: vi.fn(async () => [
          summary({ readiness: { status: 'notReady', reason: 'rootMissing' } }),
        ]),
      }),
    );
    await store.load();
    await store.previewProfile('p1');
    expect(store.canRun('p1')).toBe(false);
  });

  it.each([
    ['canceled', /canceled/i, 'info'],
    ['busy', /another confirmation dialog/i, 'error'],
    ['lease_busy', /in use by another sync/i, 'error'],
    ['not_ready', /isn't ready/i, 'error'],
    ['held', /source folder is empty/i, 'error'],
  ])('shows a clear message for %s and keeps the preview', async (code, text, kind) => {
    const { store, unsubscribe } = makeStore(
      fakeBackend({ run: vi.fn(async () => Promise.reject(new SyncProfilesError(code))) }),
    );
    await store.load();
    await store.previewProfile('p1');
    await store.requestRun('p1');
    expect(store.run?.status).toBe('failed');
    expect(store.run?.error?.text).toMatch(text);
    expect(store.run?.error?.kind).toBe(kind);
    expect(store.preview).not.toBeNull();
    expect(unsubscribe).toHaveBeenCalled();
  });

  it('stale on run drops the preview and asks for a new one', async () => {
    const { store } = makeStore(
      fakeBackend({ run: vi.fn(async () => Promise.reject(new SyncProfilesError('stale'))) }),
    );
    await store.load();
    await store.previewProfile('p1');
    await store.requestRun('p1');
    expect(store.run?.error?.text).toMatch(/Preview the profile again/);
    expect(store.preview).toBeNull();
  });

  it('follows run events and finishes with the result', async () => {
    const m = makeStore();
    await m.store.load();
    await m.store.previewProfile('p1');
    await m.store.requestRun('p1');
    m.emit({
      runId: 7,
      phase: 'syncing',
      counts: { total: 3, done: 1, failed: 0, skipped: 0, changed: 0, held: 0, notStarted: 2 },
    });
    expect(m.store.run?.phase).toBe('syncing');
    expect(m.store.run?.counts?.done).toBe(1);
    m.emit({ runId: 99, phase: 'comparing' }); // another run: ignored
    expect(m.store.run?.phase).toBe('syncing');
    m.emit({
      runId: 7,
      result: {
        kind: 'completed',
        pendingRerun: false,
        settle: { state: 'notAttempted' },
        problems: [],
      },
    });
    expect(m.store.run?.status).toBe('finished');
    expect(m.unsubscribe).toHaveBeenCalled();
  });

  it('an event that arrives before the run call resolves is not lost', async () => {
    let resolveRun: (v: { runId: number }) => void = () => undefined;
    const run = vi.fn(() => new Promise<{ runId: number }>((r) => (resolveRun = r)));
    const m = makeStore(fakeBackend({ run }));
    await m.store.load();
    await m.store.previewProfile('p1');
    const started = m.store.requestRun('p1');
    await vi.waitFor(() => expect(run).toHaveBeenCalled());
    m.emit({
      runId: 7,
      result: {
        kind: 'nothingToDo',
        pendingRerun: false,
        settle: { state: 'notAttempted' },
        problems: [],
      },
    });
    resolveRun({ runId: 7 });
    await started;
    expect(m.store.run?.status).toBe('finished');
  });

  it('a changed revision in the list drops the preview', async () => {
    const list = vi
      .fn()
      .mockResolvedValueOnce([summary()])
      .mockResolvedValue([summary({ revision: 2 })]);
    const { store } = makeStore(fakeBackend({ list }));
    await store.load();
    await store.previewProfile('p1');
    await store.load();
    expect(store.preview).toBeNull();
  });
});

describe('run prompt for deletions (SP-9.4)', () => {
  const withDeletes = (label: string, overwrites = 0) => ({
    list: vi.fn(async () => [summary({ deleteLabel: label })]),
    preview: vi.fn(async () =>
      preview({ summary: { ...preview().summary, deletes: 3, overwrites } }),
    ),
  });

  it('a Recycle Bin run with deletions asks first, and sends no overwrite flag', async () => {
    const { store, backend } = makeStore(fakeBackend(withDeletes('Recycle Bin')));
    await store.load();
    await store.previewProfile('p1');
    await store.requestRun('p1');
    expect(store.runPrompt).toMatchObject({ overwrites: 0, deletes: 3 });
    expect(backend.run).not.toHaveBeenCalled();
    await store.confirmRunPrompt();
    expect(backend.run).toHaveBeenCalledWith({
      id: 'p1',
      previewId: 'pv1',
      confirmOverwrite: false,
    });
  });

  it('a versioning run with deletions and overwrites uses ONE prompt naming both', async () => {
    const { store, backend } = makeStore(fakeBackend(withDeletes('Versioning · F:\\V', 2)));
    await store.load();
    await store.previewProfile('p1');
    await store.requestRun('p1');
    expect(store.runPrompt).toMatchObject({ overwrites: 2, deletes: 3 });
    await store.confirmRunPrompt();
    expect(backend.run).toHaveBeenCalledWith(expect.objectContaining({ confirmOverwrite: true }));
  });

  it('a permanent run is left to the backend native dialog (no webview prompt)', async () => {
    const { store, backend } = makeStore(fakeBackend(withDeletes('Permanent')));
    await store.load();
    await store.previewProfile('p1');
    await store.requestRun('p1');
    expect(store.runPrompt).toBeNull();
    expect(backend.run).toHaveBeenCalledTimes(1);
  });

  it('a run without deletions or overwrites starts without a prompt', async () => {
    const { store, backend } = makeStore();
    await store.load();
    await store.previewProfile('p1');
    await store.requestRun('p1');
    expect(store.runPrompt).toBeNull();
    expect(backend.run).toHaveBeenCalledTimes(1);
  });

  it('refuses a prompt that belongs to another preview', async () => {
    const previews = [preview({ previewId: 'a' }), preview({ previewId: 'b' })];
    const fb = withDeletes('Recycle Bin');
    let n = 0;
    const { store, backend } = makeStore(
      fakeBackend({
        ...fb,
        preview: vi.fn(async () => {
          const base = previews[n++];
          return { ...base, summary: { ...base.summary, deletes: 1 } };
        }),
      }),
    );
    await store.load();
    await store.previewProfile('p1');
    await store.requestRun('p1');
    await store.previewProfile('p1'); // a new preview clears the old prompt...
    expect(store.runPrompt).toBeNull();
    store.runPrompt = { profileId: 'p1', previewId: 'a', overwrites: 0, deletes: 1 }; // ...a stale one
    await store.confirmRunPrompt();
    expect(backend.run).not.toHaveBeenCalled();
  });
});

describe('run events are tied to our run id', () => {
  it('ignores foreign events held before the response and replays only ours', async () => {
    let resolveRun: (v: { runId: number }) => void = () => undefined;
    const run = vi.fn(() => new Promise<{ runId: number }>((r) => (resolveRun = r)));
    const m = makeStore(fakeBackend({ run }));
    await m.store.load();
    await m.store.previewProfile('p1');
    const started = m.store.requestRun('p1');
    await vi.waitFor(() => expect(run).toHaveBeenCalled());
    const counts = (done: number) => ({
      total: 3,
      done,
      failed: 0,
      skipped: 0,
      changed: 0,
      held: 0,
      notStarted: 0,
    });
    m.emit({ runId: 99, phase: 'comparing', counts: counts(1) });
    m.emit({
      runId: 99,
      result: {
        kind: 'failed',
        pendingRerun: false,
        settle: { state: 'notAttempted' },
        problems: [],
      },
    });
    m.emit({ runId: 7, phase: 'syncing', counts: counts(2) });
    resolveRun({ runId: 7 });
    await started;
    expect(m.store.run).toMatchObject({ status: 'running', runId: 7, phase: 'syncing' });
    expect(m.store.run?.counts?.done).toBe(2);
    m.emit({
      runId: 99,
      result: {
        kind: 'failed',
        pendingRerun: false,
        settle: { state: 'notAttempted' },
        problems: [],
      },
    });
    expect(m.store.run?.status).toBe('running');
  });

  it('drops buffered events when the run is refused', async () => {
    const m = makeStore(
      fakeBackend({
        run: vi.fn(async () => {
          m.emit({ runId: 5, phase: 'syncing' });
          throw new SyncProfilesError('lease_busy');
        }),
      }),
    );
    await m.store.load();
    await m.store.previewProfile('p1');
    await m.store.requestRun('p1');
    expect(m.store.run?.status).toBe('failed');
    expect(m.store.run?.phase).toBeUndefined();
  });

  it('a run whose result never arrives can be dismissed and unblocks Run now', async () => {
    const subscribe = vi.fn(async () => {
      throw new Error('no events');
    });
    const backend = fakeBackend();
    const store = new SyncProfilesStore({ backend, subscribe });
    await store.load();
    await store.previewProfile('p1');
    await store.requestRun('p1');
    expect(store.run).toMatchObject({ status: 'running', progressUnavailable: true });
    await store.previewProfile('p1');
    expect(store.canRun('p1')).toBe(false);
    store.dismissRun();
    expect(store.run).toBeNull();
    expect(store.canRun('p1')).toBe(true);
  });
});

describe('editor details', () => {
  it('shows a duplicate name on the name field (SP-1.3)', async () => {
    const { store } = makeStore(
      fakeBackend({
        save: vi.fn(async () =>
          Promise.reject(new SyncProfilesError('invalid_input', 'This name is already in use')),
        ),
      }),
    );
    store.openNew({ name: 'Photos', left: 'D:\\A', right: 'E:\\B' });
    await store.save();
    expect(store.editor?.errors).toEqual([{ field: 'name', code: 'name_taken' }]);
    expect(store.editor?.notice).toBeNull();
  });

  it('leaving a dirty editor asks first; a clean one just closes', () => {
    const { store } = makeStore();
    store.openNew();
    expect(store.leaveEditor('page')).toBe(true);
    expect(store.editor).toBeNull();
    store.openNew();
    store.touch();
    expect(store.leaveEditor('page')).toBe(false);
    expect(store.discardTarget).toBe('page');
    expect(store.editor).not.toBeNull();
    store.closeEditor();
    expect(store.discardTarget).toBeNull();
    expect(store.editor).toBeNull();
  });

  it('clamps guard numbers to the wire types so the backend reports the range error', async () => {
    const { store, backend } = makeStore();
    store.openNew({ name: 'x', left: 'D:\\A', right: 'E:\\B' });
    store.editor!.form.guardMaxCount = 1e12;
    store.editor!.form.guardMaxPercent = 1000;
    await store.save();
    const arg = (backend.save as ReturnType<typeof vi.fn>).mock.calls[0][0];
    expect(arg.draft.guard).toMatchObject({ maxCount: 4294967295, maxPercent: 255 });
  });
});
