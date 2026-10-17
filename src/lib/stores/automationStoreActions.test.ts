/**
 * FER-527 (P1.6): the store's request flows - Run now / Retry / Stop / Recheck / Pause, the detail
 * page, progress, operation paging and Review in Sync. Every call is checked on the fake backend:
 * the UI only ever sends ids (and the pause mode), and answers are shown as the backend gave them.
 */
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { AutomationStore } from './automationStore.svelte';
import {
  AutomationError,
  type AutomationDetail,
  type AutomationRun,
  type RunOpsPage,
} from '../ipc/automation';
import { toast } from './toastStore.svelte';
import {
  fakeBackend,
  fakeProfiles,
  fixture,
  profileSummary,
  rows,
  scheduleDetail,
} from '../components/automation/automationTestKit';

const NOW = Date.parse('2026-10-17T10:00:00Z');

function make(over: Parameters<typeof fakeBackend>[0] = {}, openSync = vi.fn(async () => {})) {
  const backend = fakeBackend(over);
  const store = new AutomationStore({
    backend,
    profiles: fakeProfiles([profileSummary()]),
    validateDelayMs: 0,
    now: () => NOW,
    openSync,
  });
  return { store, backend, openSync };
}

const heldRun = (): AutomationRun =>
  fixture<{ runs: AutomationRun[] }>('automation-history.json').runs[0];
const photoDetail = (): AutomationDetail => fixture<AutomationDetail>('automation-get.json');

/** A promise the test settles by hand, to order two answers. */
function deferred<T>() {
  let resolve!: (v: T) => void;
  const promise = new Promise<T>((r) => (resolve = r));
  return { promise, resolve };
}

beforeEach(() => {
  toast.message = '';
});

describe('Run now (RQ-4)', () => {
  it.each([
    ['automation-run-now-queued.json', 'Queued \u2014 starts shortly'],
    [
      'automation-run-now-waiting.json',
      'Waiting for \u201cNightly photos to NAS\u201d \u2014 same folders',
    ],
    [
      'automation-run-now-merged.json',
      'Merged into the pending run for these folders \u2014 no duplicate job',
    ],
    ['automation-run-now-device.json', 'Waiting for the device \u2014 a folder is not available'],
    ['automation-run-now-requested.json', 'Asked the background runner to start it'],
  ])('%s shows its toast and sends only the id', async (file, text) => {
    const { store, backend } = make({ runNow: vi.fn(async () => fixture(file) as never) });
    await store.runNow({ id: 'a1' });
    expect(backend.runNow).toHaveBeenCalledWith('a1');
    expect(toast.message).toBe(text);
    expect(store.notice).toBeNull();
  });

  it('a second click while the request is in flight is ignored', async () => {
    const gate = deferred<{ status: 'queued' }>();
    const { store, backend } = make({ runNow: vi.fn(() => gate.promise) });
    const first = store.runNow({ id: 'a1' });
    void store.runNow({ id: 'a1' });
    expect(store.pending['run:a1']).toBe(true);
    expect(backend.runNow).toHaveBeenCalledTimes(1);
    gate.resolve({ status: 'queued' });
    await first;
    expect(store.pending['run:a1']).toBeUndefined();
  });

  it.each([
    ['held', /held run needs your attention/],
    ['paused', /paused/],
    ['needs_review', /Review the profile change/],
    ['runner_down', /background runner is not running/],
    ['rate_limited', /Too many requests/],
  ])('a refusal (%s) is an error notice, not a toast', async (code, text) => {
    const { store } = make({
      runNow: vi.fn(async () => {
        throw new AutomationError(code);
      }),
    });
    await store.runNow({ id: 'a1' });
    expect(store.notice).toEqual({ kind: 'error', text: expect.stringMatching(text) });
    expect(toast.message).toBe('');
  });
});

describe('Recheck, Retry, Stop', () => {
  it('Recheck sends the run id and refreshes the history', async () => {
    const { store, backend } = make({
      recheck: vi.fn(async () => ({ status: 'requested' as const })),
    });
    store.tab = 'history';
    await store.recheck(heldRun());
    expect(backend.recheck).toHaveBeenCalledWith(1043);
    expect(toast.message).toMatch(/Recheck requested/);
    expect(backend.history).toHaveBeenCalled();
  });

  it('Retry sends the run id and shows the queue toast', async () => {
    const { store, backend } = make({
      retry: vi.fn(async () => fixture('automation-retry.json') as never),
    });
    await store.retry(heldRun());
    expect(backend.retry).toHaveBeenCalledWith(1043);
    expect(toast.message).toBe('Queued \u2014 starts shortly');
  });

  it('Retry and Recheck ignore a double click', async () => {
    const gate = deferred<{ status: 'requested' }>();
    const { store, backend } = make({ recheck: vi.fn(() => gate.promise) });
    const a = store.recheck(heldRun());
    void store.recheck(heldRun());
    expect(backend.recheck).toHaveBeenCalledTimes(1);
    gate.resolve({ status: 'requested' });
    await a;
  });

  it('Stop asks first; only the confirmed dialog sends stop_run', async () => {
    const { store, backend } = make({
      stopRun: vi.fn(async () => ({ status: 'requested' as const })),
    });
    store.askStop({ ...heldRun(), id: 1044, automationName: 'Photo watch' });
    expect(store.dialog).toEqual({ kind: 'stop', runId: 1044, name: 'Photo watch' });
    expect(backend.stopRun).not.toHaveBeenCalled();
    store.closeDialog();
    expect(store.dialog).toBeNull();
    expect(backend.stopRun).not.toHaveBeenCalled();

    store.askStop({ ...heldRun(), id: 1044, automationName: 'Photo watch' });
    await store.confirmStop();
    expect(backend.stopRun).toHaveBeenCalledWith(1044);
    expect(store.dialog).toBeNull();
    expect(toast.message).toMatch(/Stop requested/);
  });

  it('a Stop that the backend refuses closes the dialog and shows the error', async () => {
    const { store } = make({
      stopRun: vi.fn(async () => {
        throw new AutomationError('rate_limited');
      }),
    });
    store.askStop({ ...heldRun(), id: 5 });
    await store.confirmStop();
    expect(store.dialog).toBeNull();
    expect(store.notice?.kind).toBe('error');
  });
});

describe('Pause dialog (RQ-8.2)', () => {
  async function needsChoice(store: AutomationStore, backend: ReturnType<typeof make>['backend']) {
    await store.load();
    (backend.setEnabled as ReturnType<typeof vi.fn>).mockResolvedValueOnce(
      fixture('automation-set-enabled-needs-choice.json'),
    );
    await store.toggleEnabled(store.rows[0]);
  }

  it('needsChoice opens the dialog and sends nothing yet', async () => {
    const { store, backend } = make();
    await needsChoice(store, backend);
    expect(store.dialog?.kind).toBe('pause');
    expect(backend.pause).not.toHaveBeenCalled();
    expect(store.notice).toBeNull();
  });

  it('"Pause & let it finish" sends mode finish', async () => {
    const { store, backend } = make();
    await needsChoice(store, backend);
    await store.pauseWith('finish');
    expect(backend.pause).toHaveBeenCalledWith(rows()[0].id, 'finish');
    expect(store.dialog).toBeNull();
  });

  it('"Pause & stop current run" sends mode stop', async () => {
    const { store, backend } = make();
    await needsChoice(store, backend);
    await store.pauseWith('stop');
    expect(backend.pause).toHaveBeenCalledWith(rows()[0].id, 'stop');
  });

  it('Cancel closes without pausing', async () => {
    const { store, backend } = make();
    await needsChoice(store, backend);
    store.closeDialog();
    expect(store.dialog).toBeNull();
    expect(backend.pause).not.toHaveBeenCalled();
  });

  it('a double click on a choice sends one pause', async () => {
    const gate = deferred<{ status: 'paused' }>();
    const { store, backend } = make({ pause: vi.fn(() => gate.promise) });
    await needsChoice(store, backend);
    const a = store.pauseWith('finish');
    void store.pauseWith('stop');
    store.closeDialog(); // cannot close while the request runs
    expect(store.dialog).not.toBeNull();
    expect(backend.pause).toHaveBeenCalledTimes(1);
    gate.resolve({ status: 'paused' });
    await a;
    expect(store.dialog).toBeNull();
  });
});

describe('Detail page and progress (MH-5, RQ-5)', () => {
  it('opens the detail of one automation', async () => {
    const { store, backend } = make({ get: vi.fn(async () => photoDetail()) });
    await store.openDetail('b2c3d4e5-f6a7-4b8c-9d0e-1f2a3b4c5d6e');
    expect(store.view).toBe('detail');
    expect(backend.get).toHaveBeenCalledWith('b2c3d4e5-f6a7-4b8c-9d0e-1f2a3b4c5d6e');
    expect(store.detail?.data?.currentRun?.id).toBe(1044);
    expect(store.detail?.eventsNext).toBe(30);
  });

  it('shows a fixed message when the detail cannot be read', async () => {
    const { store } = make({
      get: vi.fn(async () => {
        throw new AutomationError('not_found');
      }),
    });
    await store.openDetail('gone');
    expect(store.detail?.error).toBe('This automation or its sync profile no longer exists.');
    expect(store.detail?.loading).toBe(false);
  });

  it('drops a slower answer for an older request (stale response)', async () => {
    const slow = deferred<AutomationDetail>();
    const fresh = photoDetail();
    fresh.currentRun!.counts.ok = 5;
    const get = vi
      .fn<(id: string) => Promise<AutomationDetail>>()
      .mockResolvedValueOnce(photoDetail()) // openDetail
      .mockReturnValueOnce(slow.promise) // refresh #1 (slow)
      .mockResolvedValueOnce(fresh); // refresh #2 (fast, newer)
    const { store } = make({ get });
    await store.openDetail('x');
    const first = store.refreshDetail();
    await store.refreshDetail();
    expect(store.detail?.data?.currentRun?.counts.ok).toBe(5);
    const old = photoDetail();
    old.currentRun!.counts.ok = 1;
    slow.resolve(old);
    await first;
    expect(store.detail?.data?.currentRun?.counts.ok).toBe(5);
  });

  it('an answer that arrives after leaving the detail is dropped', async () => {
    const slow = deferred<AutomationDetail>();
    const { store } = make({ get: vi.fn(() => slow.promise) });
    const opening = store.openDetail('x');
    store.closeDetail();
    slow.resolve(photoDetail());
    await opening;
    expect(store.detail).toBeNull();
    expect(store.view).toBe('list');
  });

  it('applies a progress event to the current run (ids, phase, counts only)', async () => {
    const { store } = make({ get: vi.fn(async () => photoDetail()) });
    await store.openDetail('b2c3d4e5-f6a7-4b8c-9d0e-1f2a3b4c5d6e');
    store.applyProgress({
      runId: 1044,
      automationId: 'b2c3d4e5-f6a7-4b8c-9d0e-1f2a3b4c5d6e',
      phase: 'sync',
      opsTotal: 6,
      counts: { ok: 4, failed: 1, notStarted: 1, held: 0, skippedChanged: 0 },
    });
    const run = store.detail!.data!.currentRun!;
    expect(run.counts.ok).toBe(4);
    expect(run.counts.failed).toBe(1);
    // A progress event of another automation changes nothing.
    store.applyProgress({
      runId: 1044,
      automationId: 'someone-else',
      phase: 'done',
      opsTotal: 9,
      counts: { ok: 9, failed: 0, notStarted: 0, held: 0, skippedChanged: 0 },
    });
    expect(store.detail!.data!.currentRun!.opsTotal).toBe(6);
  });

  it('pages earlier events by cursor and does not repeat ids', async () => {
    const events = vi.fn(async () => ({
      events: [
        { id: 30, atMs: 1, kind: 'mute' as const, text: 'dup' },
        { id: 29, atMs: 1, kind: 'ok' as const, text: 'older' },
      ],
    }));
    const { store } = make({ get: vi.fn(async () => photoDetail()), events });
    await store.openDetail('x');
    await store.moreEvents();
    expect(events).toHaveBeenCalledWith('x', 30, 50);
    expect(store.detail!.moreEvents.map((e) => e.id)).toEqual([29]);
    expect(store.detail!.eventsNext).toBeUndefined();
  });
});

describe('Operation pages (RQ-5: pages of 200)', () => {
  const page = (stored: number): RunOpsPage => ({ ops: [], total: stored, stored });

  it('asks for 200 per page and keeps the offset', async () => {
    const runOps = vi.fn(async () => page(450));
    const { store } = make({ runOps });
    await store.loadOps(7, 0);
    await store.loadOps(7, 200);
    await store.loadOps(7, 400);
    expect(runOps.mock.calls).toEqual([
      [7, 0, 200],
      [7, 200, 200],
      [7, 400, 200],
    ]);
    expect(store.opsByRun[7].offset).toBe(400);
  });

  it('a slow older page does not replace a newer one', async () => {
    const slow = deferred<RunOpsPage>();
    const runOps = vi
      .fn<(r: number, o: number, l: number) => Promise<RunOpsPage>>()
      .mockReturnValueOnce(slow.promise)
      .mockResolvedValueOnce({ ops: [], total: 999, stored: 999 });
    const { store } = make({ runOps });
    const a = store.loadOps(7, 0);
    await store.loadOps(7, 200);
    slow.resolve({ ops: [], total: 1, stored: 1 });
    await a;
    expect(store.opsByRun[7].page?.total).toBe(999);
    expect(store.opsByRun[7].offset).toBe(200);
  });

  it('keeps the last page and shows a fixed message on error', async () => {
    const runOps = vi
      .fn<(r: number, o: number, l: number) => Promise<RunOpsPage>>()
      .mockResolvedValueOnce(page(3))
      .mockRejectedValueOnce(new AutomationError('store_unavailable'));
    const { store } = make({ runOps });
    await store.loadOps(7, 0);
    await store.loadOps(7, 0);
    expect(store.opsByRun[7].page?.total).toBe(3);
    expect(store.opsByRun[7].error).toMatch(/store isn't available/);
  });

  it('expanding a run loads its operations once, collapsing keeps them', async () => {
    const { store, backend } = make();
    const run = heldRun();
    store.toggleRun(run);
    expect(store.expanded[run.id]).toBe(true);
    expect(backend.runOps).toHaveBeenCalledWith(1043, 0, 200);
    store.toggleRun(run);
    expect(store.expanded[run.id]).toBe(false);
  });
});

describe('Review in Sync (RQ-6)', () => {
  it('uses the roots of the profile shown on the detail page', async () => {
    const { store, openSync } = make({ get: vi.fn(async () => photoDetail()) });
    await store.openDetail('b2c3d4e5-f6a7-4b8c-9d0e-1f2a3b4c5d6e');
    await store.reviewInSync({ ...heldRun(), profileId: photoDetail().profile.id });
    expect(openSync).toHaveBeenCalledWith({ left: 'D:\\Photos', right: '\\nas\\photos' });
  });

  it('falls back to the profile list, then to the automation detail', async () => {
    const { store, openSync, backend } = make({ get: vi.fn(async () => photoDetail()) });
    await store.load();
    const known = profileSummary();
    await store.reviewInSync({ ...heldRun(), profileId: known.id });
    expect(openSync).toHaveBeenLastCalledWith({ left: known.left, right: known.right });
    expect(backend.get).not.toHaveBeenCalledWith(heldRun().automationId);

    await store.reviewInSync({ ...heldRun(), id: 9, profileId: photoDetail().profile.id });
    expect(backend.get).toHaveBeenCalledWith(heldRun().automationId);
    expect(openSync).toHaveBeenLastCalledWith({ left: 'D:\\Photos', right: '\\nas\\photos' });
  });

  it('opens nothing when the folders cannot be found', async () => {
    const { store, openSync } = make({ get: vi.fn(async () => scheduleDetail()) });
    await store.reviewInSync({ ...heldRun(), profileId: 'unknown-profile' });
    expect(openSync).not.toHaveBeenCalled();
    expect(store.notice?.kind).toBe('error');
  });
});
