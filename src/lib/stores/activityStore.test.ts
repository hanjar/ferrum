import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const ipc = vi.hoisted(() => ({
  activityAckDisclosure: vi.fn(),
  activityAddFavorite: vi.fn(),
  activityClear: vi.fn(),
  activityExclude: vi.fn(),
  activityFavorites: vi.fn(),
  activityForget: vi.fn(),
  activityFrequent: vi.fn(),
  activityRecent: vi.fn(),
  activityRemoveFavorite: vi.fn(),
  activitySetHidden: vi.fn(),
  activitySetRecording: vi.fn(),
  activitySetRetention: vi.fn(),
  activityState: vi.fn(),
  onActivityChanged: vi.fn(),
  recordActivityVisit: vi.fn(),
}));
vi.mock('../ipc/activity', () => ipc);

import { activity } from './activityStore.svelte';

const entity = (id: string, name: string) => ({
  id,
  kind: 'folder' as const,
  provider: 'local',
  volumeId: 'C:',
  locator: `C:\\${name}`,
  parentPath: 'C:\\',
  displayName: name,
  availability: 'available',
});
const item = (id: string, name: string) => ({
  entity: entity(id, name),
  lastUtcMs: 1,
  episodeCount: 1,
  activeDays: 1,
  score: null,
});
const state = (epoch: number, extra: Record<string, unknown> = {}) => ({
  recordingEnabled: true,
  privacyEpoch: epoch,
  retentionDays: 90,
  hiddenCount: 0,
  disclosureAcked: true,
  ...extra,
});

function reset() {
  for (const fn of Object.values(ipc)) fn.mockReset();
  ipc.activityFavorites.mockResolvedValue([]);
  ipc.activityRecent.mockResolvedValue([]);
  ipc.activityFrequent.mockResolvedValue([]);
  ipc.onActivityChanged.mockResolvedValue(() => undefined);
  activity.stopSync();
  activity.privateSession = false;
  activity.recording = true;
  activity.privacyEpoch = 0;
  activity.recent = [];
  activity.frequent = [];
  activity.disclosureAcked = true;
  activity.available = false;
}

describe('activity store: privacy modes', () => {
  beforeEach(reset);

  it('private session blocks writes in this window only and hides suggestions', async () => {
    ipc.activityState.mockResolvedValue(state(1));
    ipc.activityRecent.mockResolvedValue([item('a', 'Alpha')]);
    await activity.refresh();
    expect(activity.recent).toHaveLength(1);

    activity.recordVisit('C:\\Alpha');
    expect(ipc.recordActivityVisit).toHaveBeenCalledTimes(1);

    activity.setPrivateSession(true);
    expect(activity.recent).toEqual([]); // immediate, before any refresh lands
    await activity.refresh();
    expect(activity.recent).toEqual([]);
    expect(activity.frequent).toEqual([]);
    // History lists are not even requested while private.
    ipc.activityRecent.mockClear();
    await activity.refresh();
    expect(ipc.activityRecent).not.toHaveBeenCalled();

    activity.recordVisit('C:\\Beta');
    expect(ipc.recordActivityVisit).toHaveBeenCalledTimes(1);
    expect(activity.recordingActive).toBe(false);

    activity.setPrivateSession(false);
    await activity.refresh();
    expect(activity.recent).toHaveLength(1);
  });

  it('pause stops writes everywhere but private flag stays independent', async () => {
    ipc.activitySetRecording.mockResolvedValue(state(2, { recordingEnabled: false }));
    await activity.setRecording(false);
    expect(activity.recording).toBe(false);
    expect(activity.privateSession).toBe(false);
    activity.recordVisit('C:\\Alpha');
    expect(ipc.recordActivityVisit).not.toHaveBeenCalled();
  });

  it('hide/forget/clear update local lists immediately, before the service answers', async () => {
    activity.recent = [item('a', 'Alpha'), item('b', 'Beta')];
    activity.frequent = [item('a', 'Alpha')];
    let release!: (v: unknown) => void;
    ipc.activitySetHidden.mockReturnValue(new Promise((r) => (release = r)));
    ipc.activityState.mockResolvedValue(state(1));
    const pending = activity.hideSuggestion('a');
    expect(activity.recent.map((i) => i.entity.id)).toEqual(['b']);
    expect(activity.frequent).toEqual([]);
    release(state(1, { hiddenCount: 1 }));
    await pending;

    ipc.activityForget.mockResolvedValue(undefined);
    await activity.forget('b');
    expect(activity.recent).toEqual([]);
    expect(ipc.activityForget).toHaveBeenCalledWith('b');

    activity.recent = [item('c', 'Gamma')];
    ipc.activityClear.mockResolvedValue(3);
    const clearing = activity.clearAll();
    expect(activity.recent).toEqual([]);
    await clearing;
  });

  it('retention setting is applied from the service state', async () => {
    ipc.activitySetRetention.mockResolvedValue(state(1, { retentionDays: 30 }));
    ipc.activityState.mockResolvedValue(state(1, { retentionDays: 30 }));
    await activity.setRetention(30);
    expect(ipc.activitySetRetention).toHaveBeenCalledWith(30);
    expect(activity.retentionDays).toBe(30);
  });

  it('first-run disclosure shows once recording is on and clears on ack', async () => {
    ipc.activityState.mockResolvedValue(state(0, { disclosureAcked: false }));
    await activity.refresh();
    expect(activity.needsDisclosure).toBe(true);
    ipc.activityAckDisclosure.mockResolvedValue(state(0, { disclosureAcked: true }));
    await activity.ackDisclosure();
    expect(activity.needsDisclosure).toBe(false);
  });

  it('disclosure is not shown while recording is off', async () => {
    ipc.activityState.mockResolvedValue(
      state(0, { disclosureAcked: false, recordingEnabled: false }),
    );
    await activity.refresh();
    expect(activity.needsDisclosure).toBe(false);
  });
});

describe('activity store: privacy epoch and multi-window invalidation', () => {
  beforeEach(reset);

  it('discards lists fetched across an epoch bump and re-reads', async () => {
    // First read: epoch 1 before, 2 after (a clear happened mid-flight).
    ipc.activityState
      .mockResolvedValueOnce(state(1))
      .mockResolvedValueOnce(state(2))
      .mockResolvedValue(state(2));
    ipc.activityRecent
      .mockResolvedValueOnce([item('old', 'Cleared')]) // stale snapshot
      .mockResolvedValue([]);
    await activity.refresh();
    expect(activity.recent).toEqual([]);
    expect(activity.privacyEpoch).toBe(2);
  });

  it('a slow refresh superseded by a newer one never paints', async () => {
    let releaseFirst!: (v: unknown) => void;
    ipc.activityState
      .mockReturnValueOnce(new Promise((r) => (releaseFirst = r)))
      .mockResolvedValue(state(1));
    ipc.activityRecent.mockResolvedValue([item('new', 'Fresh')]);
    const slow = activity.refresh();
    await activity.refresh(); // newer call completes first
    expect(activity.recent.map((i) => i.entity.id)).toEqual(['new']);
    ipc.activityRecent.mockResolvedValue([item('zombie', 'Zombie')]);
    releaseFirst(state(1));
    await slow;
    expect(activity.recent.map((i) => i.entity.id)).toEqual(['new']);
  });

  it('a response older than the known epoch is dropped', async () => {
    activity.privacyEpoch = 5;
    ipc.activityState.mockResolvedValue(state(4));
    ipc.activityRecent.mockResolvedValue([item('x', 'Stale')]);
    await activity.refresh();
    expect(activity.recent).toEqual([]);
    expect(activity.privacyEpoch).toBe(5);
    expect(activity.applyState(state(3))).toBe(false);
  });

  it('another window clearing history invalidates cached suggestions at once', async () => {
    let emit!: (s: ReturnType<typeof state>) => void;
    ipc.onActivityChanged.mockImplementation(async (cb) => {
      emit = cb;
      return () => undefined;
    });
    activity.recent = [item('a', 'Alpha')];
    activity.privacyEpoch = 1;
    ipc.activityState.mockResolvedValue(state(2));
    await activity.startSync();
    emit(state(2));
    expect(activity.recent).toEqual([]); // dropped synchronously
    expect(activity.privacyEpoch).toBe(2);
    await vi.waitFor(() => expect(ipc.activityRecent).toHaveBeenCalled());
  });

  it('a stale broadcast does not wipe a newer view', async () => {
    let emit!: (s: ReturnType<typeof state>) => void;
    ipc.onActivityChanged.mockImplementation(async (cb) => {
      emit = cb;
      return () => undefined;
    });
    activity.recent = [item('a', 'Alpha')];
    activity.privacyEpoch = 7;
    await activity.startSync();
    emit(state(6));
    expect(activity.recent).toHaveLength(1);
  });

  it('startSync is idempotent', async () => {
    await activity.startSync();
    await activity.startSync();
    expect(ipc.onActivityChanged).toHaveBeenCalledTimes(1);
  });
});

describe('activity store: filesystem changes refresh labels', () => {
  beforeEach(() => {
    reset();
    vi.useFakeTimers();
  });
  afterEach(() => vi.useRealTimers());

  it('noteFsChange re-reads suggestions once (debounced) so a renamed folder gets its new label', async () => {
    ipc.activityState.mockResolvedValue(state(1));
    ipc.activityRecent.mockResolvedValue([item('a', 'Old')]);
    await activity.refresh();
    expect(activity.recent[0].entity.displayName).toBe('Old');

    ipc.activityRecent.mockResolvedValue([item('a', 'New')]);
    activity.noteFsChange();
    activity.noteFsChange(); // burst coalesces
    expect(activity.recent[0].entity.displayName).toBe('Old'); // not before the debounce
    await vi.advanceTimersByTimeAsync(1000);
    expect(activity.recent[0].entity.displayName).toBe('New');
    expect(ipc.activityRecent).toHaveBeenCalledTimes(2); // initial + one coalesced refresh
  });

  it('noteFsChange still refreshes while recording is paused (structural updates are not recording)', async () => {
    ipc.activityState.mockResolvedValue(state(1, { recordingEnabled: false }));
    activity.recording = false;
    ipc.activityRecent.mockResolvedValue([item('a', 'New')]);
    activity.noteFsChange();
    await vi.advanceTimersByTimeAsync(1000);
    expect(activity.recent[0].entity.displayName).toBe('New');
  });
});

describe('activity store: fail-closed disclosure default (review N1)', () => {
  it('starts NOT acknowledged but shows no banner until the backend state arrives', async () => {
    vi.resetModules();
    const { activity: fresh } = await import('./activityStore.svelte');
    expect(fresh.disclosureAcked).toBe(false);
    // Banner needs `available`, which is only set once state arrives: no flash on startup.
    expect(fresh.available).toBe(false);
    expect(fresh.needsDisclosure).toBe(false);
  });
});
