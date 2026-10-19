import { beforeEach, describe, expect, it, vi } from 'vitest';
import { AutomationStore } from './automationStore.svelte';
import { AutomationError } from '../ipc/automation';
import { toast } from './toastStore.svelte';
import {
  PROFILE_ID,
  fakeBackend,
  fakeProfiles,
  fixture,
  profileSummary,
  rows,
  scheduleDetail,
} from '../components/automation/automationTestKit';

const NOW = Date.parse('2026-10-17T10:00:00Z');

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

/** Let the immediate (delay 0) validation settle. */
const settle = () => new Promise((r) => setTimeout(r, 0));

async function reachReview(store: AutomationStore) {
  await store.load();
  store.openNew();
  store.changeForm({ name: 'Nightly projects backup' });
  await settle();
  store.continueToReview();
}

beforeEach(() => {
  toast.message = '';
});

describe('AutomationStore', () => {
  it('loads the rows and the profiles, and counts attention (MH-9)', async () => {
    const { store } = make();
    await store.load();
    expect(store.rows).toHaveLength(3);
    expect(store.profiles).toHaveLength(1);
    expect(store.attentionCount).toBe(2);
    expect(store.listError).toBeNull();
  });

  it('shows a fixed message when the list cannot be read', async () => {
    const { store } = make({
      list: vi.fn(async () => {
        throw new AutomationError('unsupported');
      }),
    });
    await store.load();
    expect(store.listError).toBe('Automation is available only in the Ferrum desktop app.');
  });

  it('keeps Continue to review off while errors exist and until the backend validated', async () => {
    const { store } = make();
    await store.load();
    store.openNew();
    expect(store.canContinue).toBe(false);
    await settle();
    // No name yet: the local error remains even though the backend answered ok.
    expect(store.editor?.errors.map((e) => e.code)).toContain('name_required');
    expect(store.canContinue).toBe(false);
    store.changeForm({ name: 'Nightly' });
    expect(store.canContinue).toBe(false); // not validated for this form yet
    await settle();
    expect(store.canContinue).toBe(true);
    store.changeForm({ freq: 'weekly' });
    expect(store.editor?.errors.map((e) => e.code)).toContain('days_required');
    await settle();
    expect(store.canContinue).toBe(false);
  });

  it('merges backend field errors into the list', async () => {
    const { store } = make({
      validate: vi.fn(async () => fixture('automation-validate-errors.json') as never),
    });
    await store.load();
    store.openNew();
    store.changeForm({ name: 'Nightly' });
    await settle();
    const codes = store.editor?.errors.map((e) => e.code);
    expect(codes).toContain('time_invalid');
    expect(codes).toContain('retries_out_of_range');
    expect(store.canContinue).toBe(false);
  });

  it('only the newest validation answer is applied', async () => {
    let release!: () => void;
    const first = new Promise<void>((r) => (release = r));
    let n = 0;
    const { store } = make({
      validate: vi.fn(async () => {
        n++;
        if (n === 1) await first;
        return n === 1
          ? (fixture('automation-validate-errors.json') as never)
          : (fixture('automation-validate.json') as never);
      }),
    });
    await store.load();
    store.openNew(); // validation 1 (slow, bad)
    store.changeForm({ name: 'Nightly' }); // validation 2 (fast, good)
    await settle();
    release();
    await settle();
    expect(store.editor?.errors).toEqual([]);
    expect(store.canContinue).toBe(true);
  });

  it('marks the editor unavailable (and keeps it blocked) when validation is refused', async () => {
    const { store } = make({
      validate: vi.fn(async () => {
        throw new AutomationError('unsupported');
      }),
    });
    await store.load();
    store.openNew();
    store.changeForm({ name: 'Nightly' });
    await settle();
    expect(store.editor?.unavailable).toBeTruthy();
    expect(store.canContinue).toBe(false);
  });

  it('cannot approve before a passed test', async () => {
    const { store, backend } = make();
    await reachReview(store);
    expect(store.canApprove).toBe(false);
    await store.approve();
    expect(backend.approve).not.toHaveBeenCalled();
  });

  it('a failed test keeps approval disabled', async () => {
    const { store, backend } = make({
      test: vi.fn(async () => fixture('automation-test-failed.json') as never),
    });
    await reachReview(store);
    await store.runTest();
    expect(store.review?.test?.passed).toBe(false);
    expect(store.canApprove).toBe(false);
    await store.approve();
    expect(backend.approve).not.toHaveBeenCalled();
  });

  it('approve sends only the draft and the test id (new automation sends no id)', async () => {
    const { store, backend } = make();
    await reachReview(store);
    await store.runTest();
    expect(backend.test).toHaveBeenCalledWith({
      id: undefined,
      draft: expect.objectContaining({ name: 'Nightly projects backup', profileId: PROFILE_ID }),
    });
    expect(store.canApprove).toBe(true);
    await store.approve();
    expect(backend.approve).toHaveBeenCalledTimes(1);
    const arg = (backend.approve as ReturnType<typeof vi.fn>).mock.calls[0][0];
    expect(Object.keys(arg).sort()).toEqual(['draft', 'expectedConfigRev', 'id', 'testId']);
    expect(arg.id).toBeUndefined();
    expect(arg.expectedConfigRev).toBeUndefined();
    expect(arg.testId).toBe('c0ffee00-1234-4abc-8def-0123456789ab');
    // Nothing that looks like client-side approval travels with it.
    expect(JSON.stringify(arg)).not.toMatch(/approved|confirmed/i);
  });

  it('approved: back to the list, row highlighted, toast, list reloaded', async () => {
    const { store, backend } = make();
    await reachReview(store);
    await store.runTest();
    await store.approve();
    expect(store.view).toBe('list');
    expect(store.review).toBeNull();
    expect(store.highlightId).toBe('7d1c2b3a-4e5f-4a6b-8c7d-9e0f1a2b3c4d');
    expect(toast.message).toBe(
      "Nightly projects backup enabled — background runs won't ask again for rev 3",
    );
    expect(backend.list).toHaveBeenCalledTimes(2);
    store.dispose();
  });

  it('canceled keeps the user on the review and the test usable', async () => {
    const { store, backend } = make({
      approve: vi.fn(async () => fixture('automation-approve-canceled.json') as never),
    });
    await reachReview(store);
    await store.runTest();
    await store.approve();
    expect(store.view).toBe('review');
    expect(store.review?.notice?.text).toBe('Approval was canceled. Nothing was changed.');
    expect(store.canApprove).toBe(true);
    await store.approve();
    expect(backend.approve).toHaveBeenCalledTimes(2);
  });

  it('stale shows the fixed text and requires a new test', async () => {
    const { store } = make({
      approve: vi.fn(async () => fixture('automation-approve-stale.json') as never),
    });
    await reachReview(store);
    await store.runTest();
    await store.approve();
    expect(store.view).toBe('review');
    expect(store.review?.notice?.text).toBe('The profile or automation changed — review again');
    expect(store.review?.stale).toBe(true);
    expect(store.review?.test).toBeUndefined();
    expect(store.canApprove).toBe(false);
  });

  it('stale arriving as an error behaves the same', async () => {
    const { store } = make({
      approve: vi.fn(async () => {
        throw new AutomationError('stale');
      }),
    });
    await reachReview(store);
    await store.runTest();
    await store.approve();
    expect(store.review?.notice?.text).toBe('The profile or automation changed — review again');
    expect(store.canApprove).toBe(false);
  });

  it('an edit sends the id and the config revision it loaded', async () => {
    const { store, backend } = make();
    await store.load();
    await store.openEdit('7d1c2b3a-4e5f-4a6b-8c7d-9e0f1a2b3c4d');
    expect(store.editor?.id).toBe('7d1c2b3a-4e5f-4a6b-8c7d-9e0f1a2b3c4d');
    expect(store.editor?.form.name).toBe('Nightly projects backup');
    await settle();
    store.continueToReview();
    expect(store.review?.mode).toBe('edit');
    await store.runTest();
    await store.approve();
    expect(backend.approve).toHaveBeenCalledWith(
      expect.objectContaining({
        id: '7d1c2b3a-4e5f-4a6b-8c7d-9e0f1a2b3c4d',
        expectedConfigRev: scheduleDetail().automation.configRev,
      }),
    );
    store.dispose();
  });

  it('edits a watch automation: the editor opens with its watch fields (P2.2)', async () => {
    const { store } = make({ get: vi.fn(async () => fixture('automation-get.json') as never) });
    await store.load();
    await store.openEdit('b2c3d4e5-f6a7-4b8c-9d0e-1f2a3b4c5d6e');
    expect(store.notice).toBeNull();
    expect(store.view).toBe('editor');
    expect(store.editor?.form).toMatchObject({
      kind: 'watch',
      name: 'Photo watch',
      watchLeft: true,
      watchRight: false,
      onChanges: true,
      onAvailable: true,
      idleSecs: 10,
      maxWaitSecs: 900,
    });
    expect(store.editor?.expectedConfigRev).toBe(2);
    store.dispose();
  });

  it('a re-review lists the profile changes since the approved revision', async () => {
    const { store } = make({ get: vi.fn(async () => fixture('automation-get.json') as never) });
    await store.load();
    const row = rows()[1];
    await store.openReview(row);
    expect(store.view).toBe('review');
    expect(store.review?.mode).toBe('resume');
    expect(store.review?.changes).toEqual([
      { field: 'Delete policy', from: 'Recycle Bin', to: 'Permanent delete' },
    ]);
  });

  it('Enabled switch: canceled and needsChoice are reported, nothing is assumed', async () => {
    const { store, backend } = make();
    await store.load();
    (backend.setEnabled as ReturnType<typeof vi.fn>).mockResolvedValueOnce({ status: 'canceled' });
    await store.toggleEnabled(store.rows[1]);
    expect(store.notice?.text).toMatch(/Canceled/);
    (backend.setEnabled as ReturnType<typeof vi.fn>).mockResolvedValueOnce({
      status: 'needsChoice',
    });
    await store.toggleEnabled(store.rows[0]);
    // needsChoice opens the Pause dialog (RQ-8.2); nothing is paused until a button is chosen.
    expect(store.dialog).toEqual({ kind: 'pause', id: store.rows[0].id, name: store.rows[0].name });
    expect(backend.pause).not.toHaveBeenCalled();
    expect(backend.setEnabled).toHaveBeenNthCalledWith(1, store.rows[1].id, !store.rows[1].enabled);
  });

  it('approving never calls Run now, Retry, Stop or Recheck', async () => {
    const { store, backend } = make();
    await reachReview(store);
    await store.runTest();
    await store.approve();
    for (const fn of [backend.runNow, backend.retry, backend.stopRun, backend.recheck]) {
      expect(fn).not.toHaveBeenCalled();
    }
    store.dispose();
  });

  it('loads the basic run history with a filter', async () => {
    const { store, backend } = make();
    store.showHistory('attention');
    await settle();
    expect(backend.history).toHaveBeenCalledWith({ filter: 'attention', limit: 50 });
    expect(store.history.runs.length).toBeGreaterThan(0);
  });
});

describe('AutomationStore rework R1', () => {
  it('R2-1: stale in EDIT mode keeps the old revision and Reload reopens the latest version', async () => {
    const detail = scheduleDetail();
    let latest = {
      ...detail,
      automation: { ...detail.automation, name: 'Changed elsewhere', configRev: 9 },
    };
    let served = detail;
    const approve = vi.fn().mockResolvedValueOnce(fixture('automation-approve-stale.json'));
    const { store } = make({ approve: approve as never, get: vi.fn(async () => served) });
    await store.load();
    await store.openEdit(detail.automation.id);
    await settle();
    store.continueToReview();
    await store.runTest();
    served = latest;
    await store.approve();
    expect(store.review?.notice?.text).toBe('The profile or automation changed — review again');
    expect(store.review?.expectedConfigRev).toBe(detail.automation.configRev); // not rebased
    expect(store.canApprove).toBe(false);
    await store.reloadEdit();
    expect(store.view).toBe('editor');
    expect(store.editor?.expectedConfigRev).toBe(9);
    expect(store.editor?.form.name).toBe('Changed elsewhere');
    expect(store.review).toBeNull();
    latest = served;
    store.dispose();
  });

  it('R2-1/R2-4: stale in RESUME mode re-reads revision, draft and the profile changes; retest + approve succeeds', async () => {
    const base = fixture<Record<string, unknown>>('automation-get.json') as never as ReturnType<
      typeof scheduleDetail
    >;
    let rev = 2;
    const approve = vi
      .fn()
      .mockResolvedValueOnce(fixture('automation-approve-stale.json'))
      .mockResolvedValue(fixture('automation-approve.json'));
    const profiles = fakeProfiles();
    const { store } = make({
      approve: approve as never,
      get: vi.fn(async () => ({ ...base, automation: { ...base.automation, configRev: rev } })),
    });
    (store as unknown as { profiles: unknown }).profiles = store.profiles;
    await store.load();
    await store.openReview(rows()[1]);
    await store.runTest();
    rev = 9;
    await store.approve();
    expect(store.review?.expectedConfigRev).toBe(9);
    expect(store.review?.changes.length).toBeGreaterThan(0);
    expect(store.canApprove).toBe(false);
    await store.runTest();
    await store.approve();
    expect(approve).toHaveBeenLastCalledWith(expect.objectContaining({ expectedConfigRev: 9 }));
    expect(store.view).toBe('list');
    void profiles;
    store.dispose();
  });

  it('R2-2: an expired Once in a re-review is blocked with a pointer to Edit, no test loop', async () => {
    const base = fixture<Record<string, unknown>>('automation-get.json') as never as ReturnType<
      typeof scheduleDetail
    >;
    const policy = {
      ...base.policy,
      trigger: {
        kind: 'schedule' as const,
        freq: { kind: 'once' as const, date: '2026-01-01' },
        time: '02:00',
        tz: 'UTC',
        missed: 'skip' as const,
      },
    };
    const { store, backend } = make({ get: vi.fn(async () => ({ ...base, policy })) });
    await store.load();
    await store.openReview(rows()[1]);
    await store.runTest();
    expect(store.review?.test?.passed).toBe(true);
    expect(store.resumeBlock).toMatch(/one-time schedule has passed/);
    expect(store.canApprove).toBe(false);
    await store.approve();
    expect(backend.approve).not.toHaveBeenCalled();
    expect(store.review?.test).toBeTruthy(); // the test is not thrown away
  });

  it('S3: a Once time that passed while waiting blocks Continue and Approve', async () => {
    let now = NOW;
    const backend = fakeBackend();
    const store = new AutomationStore({
      backend,
      profiles: fakeProfiles(),
      validateDelayMs: 0,
      now: () => now,
    });
    await store.load();
    store.openNew();
    store.changeForm({ name: 'Once', freq: 'once', tz: 'UTC', date: '2026-10-17', time: '10:30' });
    await settle();
    expect(store.canContinue).toBe(true);
    now = Date.parse('2026-10-17T10:45:00Z');
    store.continueToReview();
    expect(store.view).toBe('editor');
    expect(store.editor?.errors.map((e) => e.code)).toContain('time_passed');

    now = NOW;
    store.changeForm({ time: '10:30' });
    await settle();
    store.continueToReview();
    await store.runTest();
    now = Date.parse('2026-10-17T10:45:00Z');
    await store.approve();
    expect(backend.approve).not.toHaveBeenCalled();
    expect(store.review?.notice?.text).toMatch(/already passed/);
    expect(store.canApprove).toBe(false);
  });

  it('S5: Back, change, Continue reviews the NEW draft and drops the old test', async () => {
    const { store } = make();
    await reachReview(store);
    await store.runTest();
    expect(store.review?.test?.testId).toBeTruthy();
    store.backFromReview();
    expect(store.view).toBe('editor');
    store.changeForm({ name: 'Renamed' });
    await settle();
    store.continueToReview();
    expect(store.review?.draft.name).toBe('Renamed');
    expect(store.review?.test).toBeUndefined();
    expect(store.canApprove).toBe(false);
  });

  it('S5: a test answer arriving after Back is ignored', async () => {
    let release!: () => void;
    const gate = new Promise<void>((r) => (release = r));
    const { store } = make({
      test: vi.fn(async () => {
        await gate;
        return fixture('automation-test.json') as never;
      }),
    });
    await reachReview(store);
    const run = store.runTest();
    const old = store.review;
    store.backFromReview();
    store.continueToReview();
    release();
    await run;
    expect(old?.test).toBeUndefined();
    expect(store.review?.test).toBeUndefined();
    expect(store.canApprove).toBe(false);
  });

  it('S5/S2: a double click on Approve calls the backend once; Back is a no-op meanwhile', async () => {
    let release!: (v: unknown) => void;
    const pending = new Promise((r) => (release = r));
    const { store, backend } = make({ approve: vi.fn(() => pending as never) });
    await reachReview(store);
    await store.runTest();
    const a = store.approve();
    const b = store.approve();
    store.backFromReview();
    expect(store.view).toBe('review');
    release(fixture('automation-approve-canceled.json'));
    await Promise.all([a, b]);
    expect(backend.approve).toHaveBeenCalledTimes(1);
  });

  it('S6: an edit keeps the stored runtime and refuses Background as unavailable', async () => {
    const detail = scheduleDetail();
    detail.policy = { ...detail.policy, runtime: 'background' };
    const { store } = make({ get: vi.fn(async () => detail) });
    await store.load();
    await store.openEdit(detail.automation.id);
    expect(store.editor?.form.runtime).toBe('background');
    expect(store.editor?.errors.map((e) => e.code)).toContain('runtime_unavailable');
    expect(store.canContinue).toBe(false);
  });

  it('N4: a stale history answer is ignored', async () => {
    let first!: (v: unknown) => void;
    const history = vi
      .fn()
      .mockImplementationOnce(() => new Promise((r) => (first = r)))
      .mockResolvedValue({ runs: [], next: undefined });
    const { store } = make({ history: history as never });
    const a = store.loadHistory('all');
    await store.loadHistory('attention');
    first(fixture('automation-history.json'));
    await a;
    expect(store.history.filter).toBe('attention');
    expect(store.history.runs).toEqual([]);
  });

  it('N4: of two quick Review clicks only the last one opens', async () => {
    const gates: ((v: unknown) => void)[] = [];
    const get = vi.fn(() => new Promise((r) => gates.push(r)));
    const { store } = make({ get: get as never });
    await store.load();
    const a = store.openReview(rows()[1]);
    const b = store.openReview(rows()[2]);
    gates[1](fixture('automation-get.json'));
    await b;
    gates[0](fixture('automation-get.json'));
    await a;
    expect(store.review?.row?.id).toBe(rows()[2].id);
  });
});
