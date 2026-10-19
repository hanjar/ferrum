import { describe, expect, it, vi } from 'vitest';
import { AutomationStore } from './automationStore.svelte';
import {
  fakeBackend,
  fakeProfiles,
  fixture,
  profileSummary,
} from '../components/automation/automationTestKit';
import { UNTICKED_ON_OPEN_NOTE } from '../components/automation/automationModel';
import type { AutomationDetail, AutomationValidateResult } from '../ipc/automation';

const NOW = Date.parse('2026-10-17T10:00:00Z');
const WATCH_ID = 'b2c3d4e5-f6a7-4b8c-9d0e-1f2a3b4c5d6e';

function make(over: Parameters<typeof fakeBackend>[0] = {}, profiles = [profileSummary()]) {
  const backend = fakeBackend(over);
  const store = new AutomationStore({
    backend,
    profiles: fakeProfiles(profiles),
    validateDelayMs: 0,
    now: () => NOW,
  });
  return { store, backend };
}

const settle = () => new Promise((r) => setTimeout(r, 0));

describe('AutomationStore watch editor (P2.2)', () => {
  it('switching a new automation to Watch validates a watch draft', async () => {
    const { store, backend } = make();
    await store.load();
    store.openNew();
    store.changeForm({ kind: 'watch', name: 'Photo watch', idleSecs: 30 });
    await settle();
    const draft = backend.validate.mock.calls.at(-1)![0];
    expect(draft.policy.trigger).toEqual({
      kind: 'watch',
      watchLeft: true,
      watchRight: false,
      onChanges: true,
      onAvailable: true,
      idleSecs: 30,
      maxWaitSecs: 900,
    });
    expect(store.canContinue).toBe(true);
    store.dispose();
  });

  it('a network root cannot stay ticked: it is dropped locally, the other side is kept', async () => {
    const unc = profileSummary({ left: 'C:\\Photos', right: '\\\\nas\\photos' });
    const { store } = make({}, [unc]);
    await store.load();
    store.openNew();
    store.changeForm({ kind: 'watch', name: 'Photo watch', watchLeft: true, watchRight: true });
    expect(store.editor?.form.watchRight).toBe(false);
    expect(store.editor?.form.watchLeft).toBe(true);
    expect(store.unsupportedRoots()).toEqual({ left: false, right: true });
    await settle();
    expect(store.canContinue).toBe(true);
    store.dispose();
  });

  it('with only network roots the editor asks for a watchable folder and cannot continue', async () => {
    const unc = profileSummary({ left: '\\\\nas\\a', right: '\\\\nas\\b' });
    const { store } = make({}, [unc]);
    await store.load();
    store.openNew();
    store.changeForm({ kind: 'watch', name: 'Photo watch' });
    await settle();
    expect(store.editor?.form.watchLeft).toBe(false);
    expect(store.editor?.errors.map((e) => e.code)).toContain('watch_root_required');
    expect(store.canContinue).toBe(false);
    store.dispose();
  });

  it('a root the backend refuses (a mapped drive) is unticked and validated again', async () => {
    const refused: AutomationValidateResult = {
      errors: [{ field: 'trigger.watchLeft', code: 'watch_unsupported_root' }],
      checks: [],
      next: [],
    };
    const ok = fixture<AutomationValidateResult>('automation-validate.json');
    const validate = vi.fn(async (draft: { policy: { trigger: unknown } }) => {
      const t = draft.policy.trigger as { watchLeft?: boolean };
      return t.watchLeft ? refused : ok;
    });
    const other = profileSummary({ id: '11111111-2222-4333-8444-555555555555', name: 'Other' });
    const { store } = make({ validate: validate as never }, [
      profileSummary({ left: 'Z:\\Mapped', right: 'E:\\Backups' }),
      other,
    ]);
    await store.load();
    store.openNew();
    store.changeForm({ kind: 'watch', name: 'Photo watch', watchRight: true });
    await settle();
    await settle();
    expect(validate.mock.calls.length).toBeGreaterThanOrEqual(2);
    expect(store.editor?.form).toMatchObject({ watchLeft: false, watchRight: true });
    expect(store.unsupportedRoots().left).toBe(true);
    expect(store.canContinue).toBe(true);
    // Another profile starts fresh.
    store.changeForm({ profileId: other.id });
    expect(store.unsupportedRoots()).toEqual({ left: false, right: false });
    store.dispose();
  });

  it('edit -> review -> test -> approve sends the watch draft with id and expectedConfigRev', async () => {
    const { store, backend } = make({
      get: vi.fn(async () => fixture('automation-get.json') as never),
    });
    await store.load();
    await store.openEdit(WATCH_ID);
    store.changeForm({ idleSecs: 45, watchRight: true });
    await settle();
    expect(store.canContinue).toBe(true);
    store.continueToReview();
    expect(store.review?.mode).toBe('edit');
    await store.runTest();
    await store.approve();
    const sent = backend.approve.mock.calls[0][0];
    expect(sent.id).toBe(WATCH_ID);
    expect(sent.expectedConfigRev).toBe(2);
    expect(sent.draft.policy.trigger).toMatchObject({
      kind: 'watch',
      idleSecs: 45,
      watchLeft: true,
      watchRight: true,
    });
    store.dispose();
  });

  it('opening a saved watch whose profile is now a network share unticks the root, marks it changed and says why', async () => {
    const detail = fixture<AutomationDetail>('automation-get.json');
    const unc = profileSummary({
      id: detail.automation.profileId,
      left: String.raw`\\nas\photos`,
      right: String.raw`D:\Backup`,
    });
    const { store } = make({ get: vi.fn(async () => detail as never) }, [unc]);
    await store.load();
    await store.openEdit(WATCH_ID);
    expect(store.editor?.form.watchLeft).toBe(false);
    expect(store.editor?.dirty).toBe(true);
    expect(store.editor?.note).toBe(UNTICKED_ON_OPEN_NOTE);
    store.dispose();
  });

  it('opening a saved watch with watchable roots stays clean and shows no note', async () => {
    const { store } = make({ get: vi.fn(async () => fixture('automation-get.json') as never) });
    await store.load();
    await store.openEdit(WATCH_ID);
    expect(store.editor?.dirty).toBe(false);
    expect(store.editor?.note).toBeUndefined();
    store.dispose();
  });

  it('the trigger kind of a stored automation is fixed in the editor state, a new one can switch', async () => {
    const { store } = make({ get: vi.fn(async () => fixture('automation-get.json') as never) });
    await store.load();
    await store.openEdit(WATCH_ID);
    expect(store.editor?.id).toBe(WATCH_ID);
    expect(store.editor?.form.kind).toBe('watch');
    store.closeEditor();
    store.openNew();
    expect(store.editor?.id).toBeUndefined();
    expect(store.editor?.form.kind).toBe('schedule');
    store.dispose();
  });
});
