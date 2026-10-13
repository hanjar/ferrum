import { beforeEach, describe, expect, it, vi } from 'vitest';

const ipc = vi.hoisted(() => ({
  activityOverview: vi.fn(),
  activityPreview: vi.fn(),
}));
const store = vi.hoisted(() => ({
  privacyEpoch: 1,
  clearAll: vi.fn(),
  forget: vi.fn(),
  excludeRoot: vi.fn(),
  setRetention: vi.fn(),
  hideSuggestion: vi.fn(),
}));
vi.mock('../ipc/activity', () => ipc);
vi.mock('./activityStore.svelte', () => ({ activity: store }));

import { activityPage } from './activityPageStore.svelte';

const overview = (epoch: number, ids: string[] = ['a']) => ({
  days: 30,
  sort: 'recent',
  items: ids.map((id) => ({
    entity: {
      id,
      kind: 'folder',
      provider: 'local',
      volumeId: 'C:',
      locator: `C:\\${id}`,
      parentPath: 'C:\\',
      displayName: id,
      availability: 'available',
    },
    lastUtcMs: 1,
    episodeCount: 2,
    activeDays: 0,
    score: null,
    hidden: false,
  })),
  daily: [],
  totalItems: ids.length,
  totalEpisodes: 2,
  oldestUtcMs: 1,
  truncated: false,
  retentionDays: 90,
  privacyEpoch: epoch,
});
const preview = (scope: string) => ({
  scope,
  items: 3,
  episodes: 9,
  sample: ['C:\\a'],
  truncated: false,
  favoritesKept: 1,
  hiddenKept: 0,
});

beforeEach(() => {
  ipc.activityOverview.mockReset();
  ipc.activityPreview.mockReset();
  for (const k of [
    'clearAll',
    'forget',
    'excludeRoot',
    'setRetention',
    'hideSuggestion',
  ] as const) {
    store[k].mockReset();
    store[k].mockResolvedValue(undefined);
  }
  store.privacyEpoch = 1;
  activityPage.reset();
  activityPage.days = 30;
  activityPage.kind = 'folder';
  activityPage.sort = 'recent';
});

describe('activity page: loading', () => {
  it('window, kind and sort changes re-query with the new parameters', async () => {
    ipc.activityOverview.mockResolvedValue(overview(1));
    await activityPage.setDays(7);
    expect(ipc.activityOverview).toHaveBeenLastCalledWith(7, 'folder', 'recent');
    await activityPage.setKind('file');
    await activityPage.setSort('frequent');
    expect(ipc.activityOverview).toHaveBeenLastCalledWith(7, 'file', 'frequent');
    expect(activityPage.overview?.items).toHaveLength(1);
  });

  it('a pending query that finishes after a newer one is ignored', async () => {
    let releaseSlow!: (v: unknown) => void;
    ipc.activityOverview
      .mockReturnValueOnce(new Promise((r) => (releaseSlow = r)))
      .mockResolvedValue(overview(1, ['fresh']));
    const slow = activityPage.load();
    await activityPage.setDays(90);
    releaseSlow(overview(1, ['zombie']));
    await slow;
    expect(activityPage.overview?.items.map((i) => i.entity.id)).toEqual(['fresh']);
  });

  it('a snapshot older than the known privacy epoch is never painted', async () => {
    store.privacyEpoch = 5;
    ipc.activityOverview.mockResolvedValue(overview(4, ['cleared']));
    await activityPage.load();
    expect(activityPage.overview).toBeNull();
    expect(ipc.activityOverview).toHaveBeenCalledTimes(3); // initial + 2 retries
  });

  it('an epoch bump while the query was pending triggers a re-read', async () => {
    ipc.activityOverview
      .mockImplementationOnce(async () => {
        store.privacyEpoch = 2; // another window cleared while we waited
        return overview(1, ['cleared']);
      })
      .mockResolvedValue(overview(2, []));
    await activityPage.load();
    expect(activityPage.overview?.items).toEqual([]);
  });

  it('selection survives reload only while the item still exists', async () => {
    ipc.activityOverview.mockResolvedValue(overview(1, ['a', 'b']));
    await activityPage.load();
    activityPage.select('b');
    expect(activityPage.selected?.entity.id).toBe('b');
    ipc.activityOverview.mockResolvedValue(overview(1, ['a']));
    await activityPage.load();
    expect(activityPage.selectedId).toBeNull();
  });
});

describe('activity page: scope preview before execution', () => {
  it('previews first and never mutates until confirmed', async () => {
    ipc.activityPreview.mockResolvedValue(preview('all'));
    ipc.activityOverview.mockResolvedValue(overview(1));
    await activityPage.requestScope('all', 'semua riwayat');
    expect(activityPage.pending?.preview?.items).toBe(3);
    expect(store.clearAll).not.toHaveBeenCalled();
    expect(await activityPage.confirmScope()).toBe('done');
    expect(store.clearAll).toHaveBeenCalledTimes(1);
    expect(activityPage.pending).toBeNull();
  });

  it('cancel drops the scope without executing', async () => {
    ipc.activityPreview.mockResolvedValue(preview('forget'));
    await activityPage.requestScope('forget', 'a', 'l1');
    activityPage.cancelScope();
    expect(await activityPage.confirmScope()).toBe('none');
    expect(store.forget).not.toHaveBeenCalled();
  });

  it('executes each scope through its own store action', async () => {
    ipc.activityOverview.mockResolvedValue(overview(1));
    for (const [scope, target, spy] of [
      ['forget', 'l1', store.forget],
      ['exclude', 'C:\\Work', store.excludeRoot],
    ] as const) {
      ipc.activityPreview.mockResolvedValue(preview(scope));
      await activityPage.requestScope(scope, 'x', target);
      await activityPage.confirmScope();
      expect(spy).toHaveBeenCalledWith(target);
    }
    ipc.activityPreview.mockResolvedValue(preview('retention'));
    await activityPage.requestScope('retention', '30 hari', '30');
    await activityPage.confirmScope();
    expect(store.setRetention).toHaveBeenCalledWith(30);
  });

  it('a privacy epoch change after preview forces a fresh preview, not execution', async () => {
    ipc.activityPreview.mockResolvedValue(preview('all'));
    ipc.activityOverview.mockResolvedValue(overview(2));
    await activityPage.requestScope('all', 'semua riwayat');
    store.privacyEpoch = 2; // another window excluded something meanwhile
    ipc.activityPreview.mockResolvedValue({ ...preview('all'), items: 1 });
    expect(await activityPage.confirmScope()).toBe('refreshed');
    expect(store.clearAll).not.toHaveBeenCalled();
    expect(activityPage.pending?.refreshed).toBe(true);
    expect(activityPage.pending?.preview?.items).toBe(1);
    expect(await activityPage.confirmScope()).toBe('done');
    expect(store.clearAll).toHaveBeenCalledTimes(1);
  });

  it('hide needs no preview and reloads the page', async () => {
    ipc.activityOverview.mockResolvedValue(overview(1));
    await activityPage.setHidden('a', true);
    expect(store.hideSuggestion).toHaveBeenCalledWith('a', true);
    expect(ipc.activityPreview).not.toHaveBeenCalled();
  });
});
