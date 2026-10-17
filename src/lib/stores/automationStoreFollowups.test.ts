/**
 * FER-527 review R1 follow-ups: per-action error texts (S1), the live operation list (S2), the
 * events cursor (S3) and the cleanup / coalescing nits.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
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
} from '../components/automation/automationTestKit';

const ID = 'b2c3d4e5-f6a7-4b8c-9d0e-1f2a3b4c5d6e';

function make(over: Parameters<typeof fakeBackend>[0] = {}) {
  const backend = fakeBackend(over);
  const store = new AutomationStore({
    backend,
    profiles: fakeProfiles([profileSummary()]),
    validateDelayMs: 0,
    openSync: vi.fn(async () => {}),
  });
  return { store, backend };
}

const heldRun = (): AutomationRun =>
  fixture<{ runs: AutomationRun[] }>('automation-history.json').runs[0];
const photoDetail = (): AutomationDetail => fixture<AutomationDetail>('automation-get.json');

function deferred<T>() {
  let resolve!: (v: T) => void;
  const promise = new Promise<T>((r) => (resolve = r));
  return { promise, resolve };
}

beforeEach(() => {
  toast.message = '';
});

describe('Action error texts (review S1)', () => {
  const fail = (code: string) =>
    vi.fn(async () => {
      throw new AutomationError(code);
    });

  it('not_ready on Stop, Retry and Recheck is about the run, not the profile', async () => {
    const { store } = make({
      stopRun: fail('not_ready'),
      retry: fail('not_ready'),
      recheck: fail('not_ready'),
    });
    store.askStop({ ...heldRun(), id: 5 });
    await store.confirmStop();
    expect(store.notice?.text).toBe('That run has already finished.');
    await store.retry(heldRun());
    expect(store.notice?.text).toBe('Recheck first — there is nothing to retry yet.');
    await store.recheck(heldRun());
    expect(store.notice?.text).toBe('That run is no longer the one that needs attention.');
  });

  it('busy on Run now and Retry is the queue, not a confirmation dialog', async () => {
    const { store } = make({ runNow: fail('busy'), retry: fail('busy') });
    await store.runNow({ id: 'a1' });
    expect(store.notice?.text).toBe(
      'Another automation is waiting for these folders — try again in a moment.',
    );
    store.notice = null;
    await store.retry(heldRun());
    expect((store.notice as { text: string } | null)?.text).toMatch(/waiting for these folders/);
  });

  it('other uses of the same codes keep their texts', async () => {
    const { store } = make({ runNow: fail('not_ready'), setEnabled: fail('busy') });
    await store.runNow({ id: 'a1' });
    expect(store.notice?.text).toMatch(/profile isn't ready/);
    await store.toggleEnabled({ id: 'a1', name: 'x', enabled: true });
    expect(store.notice?.text).toMatch(/confirmation dialog is open/);
  });
});

describe('Live operation list, events cursor and cleanup (review S2, S3, NITs)', () => {
  const evs = (hi: number, lo: number) =>
    Array.from({ length: hi - lo + 1 }, (_, i) => ({
      id: hi - i,
      atMs: 1,
      kind: 'info' as const,
      text: `e${hi - i}`,
    }));
  const detailWith = (hi: number, lo: number) => {
    const d = photoDetail();
    d.events = { events: evs(hi, lo), next: lo };
    return d;
  };
  const progress = (ok: number) => ({
    runId: 1044,
    automationId: ID,
    phase: 'sync' as const,
    opsTotal: 6,
    counts: { ok, failed: 0, notStarted: 0, held: 0, skippedChanged: 0 },
  });

  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it('S2: the current run operation list follows progress and refreshes', async () => {
    const { store, backend } = make({ get: vi.fn(async () => photoDetail()) });
    await store.openDetail(ID);
    store.toggleOps(store.detail!.data!.currentRun!);
    await vi.advanceTimersByTimeAsync(0);
    expect(backend.runOps).toHaveBeenCalledTimes(1);
    store.applyProgress(progress(3));
    await vi.advanceTimersByTimeAsync(1000);
    expect(backend.runOps).toHaveBeenCalledTimes(2);
    await store.refreshDetail();
    expect(backend.runOps).toHaveBeenCalledTimes(3);
    // Closed again: no more reloads.
    store.toggleOps(store.detail!.data!.currentRun!);
    await store.refreshDetail();
    expect(backend.runOps).toHaveBeenCalledTimes(3);
  });

  it('S3: new events do not make "Show earlier events" skip any', async () => {
    const get = vi
      .fn<(id: string) => Promise<AutomationDetail>>()
      .mockResolvedValueOnce(detailWith(100, 87))
      .mockResolvedValueOnce(detailWith(100, 87))
      .mockResolvedValue(detailWith(103, 90));
    const events = vi.fn(async (_id: string, before: number | undefined) => ({
      events: evs((before ?? 0) - 1, (before ?? 0) - 3),
      next: (before ?? 0) - 3,
    }));
    const { store } = make({ get, events });
    await store.openDetail(ID);
    await store.moreEvents();
    expect(events).toHaveBeenLastCalledWith(ID, 87, 50);
    expect(store.detail!.moreEvents.map((e) => e.id)).toEqual([86, 85, 84]);
    // Nothing new: the paged-in events stay.
    await store.refreshDetail();
    expect(store.detail!.moreEvents).toHaveLength(3);
    // Three newer events: the first screen is 103..90 and the cursor must restart there.
    await store.refreshDetail();
    expect(store.detail!.moreEvents).toEqual([]);
    expect(store.detail!.eventsNext).toBe(90);
    await store.moreEvents();
    expect(events).toHaveBeenLastCalledWith(ID, 90, 50);
    expect(store.detail!.moreEvents[0].id).toBe(89);
  });

  it('NIT-1: progress for an unknown run triggers one refresh at a time', async () => {
    const slow = deferred<AutomationDetail>();
    const get = vi
      .fn<(id: string) => Promise<AutomationDetail>>()
      .mockResolvedValueOnce(photoDetail())
      .mockReturnValue(slow.promise);
    const { store } = make({ get });
    await store.openDetail(ID);
    const other = { ...progress(1), runId: 2000 };
    store.applyProgress(other);
    store.applyProgress(other);
    store.applyProgress(other);
    expect(get).toHaveBeenCalledTimes(2);
    slow.resolve(photoDetail());
    await vi.advanceTimersByTimeAsync(0);
    store.applyProgress(other);
    expect(get).toHaveBeenCalledTimes(3);
  });

  it('NIT-2: leaving the detail stops the ops timer and clears the dialog', async () => {
    const { store, backend } = make({ get: vi.fn(async () => photoDetail()) });
    await store.openDetail(ID);
    store.toggleOps(store.detail!.data!.currentRun!);
    await vi.advanceTimersByTimeAsync(0);
    store.applyProgress(progress(3));
    store.askStop(store.detail!.data!.currentRun!);
    store.closeDetail();
    await vi.advanceTimersByTimeAsync(2000);
    expect(backend.runOps).toHaveBeenCalledTimes(1);
    expect(store.dialog).toBeNull();
    store.askStop(heldRun());
    store.dispose();
    expect(store.dialog).toBeNull();
  });

  it('NIT-4: the pager keeps the shown offset until the next page arrives', async () => {
    const gate = deferred<RunOpsPage>();
    const runOps = vi
      .fn<(r: number, o: number, l: number) => Promise<RunOpsPage>>()
      .mockResolvedValueOnce({ ops: [], total: 450, stored: 450 })
      .mockReturnValueOnce(gate.promise);
    const { store } = make({ runOps });
    await store.loadOps(7, 0);
    const next = store.loadOps(7, 200);
    expect(store.opsByRun[7].offset).toBe(0);
    gate.resolve({ ops: [], total: 450, stored: 450 });
    await next;
    expect(store.opsByRun[7].offset).toBe(200);
  });

  it('NIT-5: no "stopping" toast when the run already ended', async () => {
    const { store, backend } = make();
    await store.load();
    (backend.setEnabled as ReturnType<typeof vi.fn>).mockResolvedValueOnce(
      fixture('automation-set-enabled-needs-choice.json'),
    );
    await store.toggleEnabled(store.rows[0]);
    await store.pauseWith('stop');
    // The fixture row is scheduled (not running): the run is over.
    expect(toast.message).toBe(`${store.rows[0].name} paused`);
  });
});
