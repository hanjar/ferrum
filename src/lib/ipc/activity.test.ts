import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
  activityAckDisclosure,
  activityAddFavorite,
  activityOverview,
  activityPreview,
  activitySetHidden,
  activitySetRetention,
  activityFavorites,
  activityFrequent,
  activityRecent,
  activityRemoveFavorite,
  activitySearch,
  activitySetRecording,
  activityState,
  recordActivityVisit,
  shellOpen,
} from './activity';

const invoke = vi.hoisted(() => vi.fn());
vi.mock('@tauri-apps/api/core', () => ({ invoke }));

/** Flip the desktop environment the wrapper checks per call. */
function setTauri(on: boolean) {
  if (on) {
    (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {};
  } else {
    delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__;
  }
}

const entity = {
  id: 'l0123',
  kind: 'folder',
  provider: 'local',
  volumeId: 'C:',
  locator: 'C:\\Work\\Invoices',
  parentPath: 'C:\\Work\\',
  displayName: 'Invoices',
  availability: 'available',
};

describe('activity IPC wrappers', () => {
  beforeEach(() => {
    setTauri(true);
    invoke.mockClear();
  });
  afterEach(() => setTauri(false));

  it('recent/frequent pass kind and limit through', async () => {
    invoke.mockResolvedValueOnce([
      { entity, lastUtcMs: 1, episodeCount: 2, activeDays: 2, score: null },
    ]);
    await activityRecent('folder', 5);
    expect(invoke).toHaveBeenCalledWith('activity_recent', { kind: 'folder', limit: 5 });

    invoke.mockResolvedValueOnce([]);
    await activityFrequent('folder', 5);
    expect(invoke).toHaveBeenCalledWith('activity_frequent', { kind: 'folder', limit: 5 });
  });

  it('workspace root is forwarded only when given (FER-254)', async () => {
    invoke.mockResolvedValue([]);
    await activityRecent('folder', 5, 'C:\\Work\\A');
    expect(invoke).toHaveBeenLastCalledWith('activity_recent', {
      kind: 'folder',
      limit: 5,
      workspaceRoot: 'C:\\Work\\A',
    });
    await activityFrequent('file', 3, 'C:\\Work\\A');
    expect(invoke).toHaveBeenLastCalledWith('activity_frequent', {
      kind: 'file',
      limit: 3,
      workspaceRoot: 'C:\\Work\\A',
    });
    await activitySearch('invo', 'all', 6, 'C:\\Work\\A');
    expect(invoke).toHaveBeenLastCalledWith('activity_search', {
      query: 'invo',
      kind: 'all',
      limit: 6,
      workspaceRoot: 'C:\\Work\\A',
    });
    // null / undefined means "no workspace filter": the key is not sent at all.
    await activityRecent('folder', 5, null);
    expect(invoke.mock.lastCall?.[1]).toEqual({ kind: 'folder', limit: 5 });
    expect('workspaceRoot' in (invoke.mock.lastCall?.[1] as object)).toBe(false);
    invoke.mockReset();
  });

  it('match and item DTOs expose matchKind and reasonCodes (FER-254)', async () => {
    const hit = {
      entity,
      matchClass: 'fuzzy',
      matchKind: 'fuzzy',
      reasonCodes: ['similarName', 'frequent'],
    };
    invoke.mockResolvedValueOnce([hit]);
    const [found] = (await activitySearch('invoce', 'folder', 5)) ?? [];
    expect(found.matchKind).toBe('fuzzy');
    expect(found.reasonCodes).toEqual(['similarName', 'frequent']);
    invoke.mockResolvedValueOnce([
      { entity, lastUtcMs: 1, episodeCount: 2, activeDays: 2, score: 1, reasonCodes: ['frequent'] },
    ]);
    const [row] = await activityFrequent('folder', 5);
    expect(row.reasonCodes).toEqual(['frequent']);
  });

  it('search trims, gates short queries, and forwards scope', async () => {
    invoke.mockResolvedValueOnce([{ entity, matchClass: 'fuzzy' }]);
    const found = await activitySearch('  invo  ', 'all', 6);
    expect(found).toHaveLength(1);
    expect(invoke).toHaveBeenCalledWith('activity_search', {
      query: 'invo',
      kind: 'all',
      limit: 6,
    });

    expect(await activitySearch('a', 'all', 6)).toEqual([]);
    expect(invoke).toHaveBeenCalledTimes(1);
  });

  it('state, shell open, and recording map to their commands', async () => {
    invoke.mockResolvedValueOnce({ recordingEnabled: true, privacyEpoch: 3 });
    expect(await activityState()).toEqual({ recordingEnabled: true, privacyEpoch: 3 });

    invoke.mockResolvedValueOnce(true);
    // FER-432: `record` is mandatory and always forwarded explicitly.
    expect(await shellOpen('C:\\x.txt', true)).toBe(true);
    expect(invoke).toHaveBeenCalledWith('shell_open', { path: 'C:\\x.txt', record: true });
    await shellOpen('C:\\x.txt', false);
    expect(invoke).toHaveBeenLastCalledWith('shell_open', { path: 'C:\\x.txt', record: false });

    invoke.mockResolvedValueOnce({ recordingEnabled: false, privacyEpoch: 4 });
    await activitySetRecording(false);
    expect(invoke).toHaveBeenCalledWith('activity_set_recording', { enabled: false });
  });

  it('recordActivityVisit swallows backend failures (never breaks navigation)', async () => {
    invoke.mockRejectedValueOnce(new Error('store unavailable'));
    expect(() => recordActivityVisit('C:\\Work', 'folder')).not.toThrow();
    await vi.waitFor(() => {
      expect(invoke).toHaveBeenCalledWith('activity_record_visit', {
        path: 'C:\\Work',
        kind: 'folder',
      });
    });
  });

  it('favorites wrappers pass paths and surface pin results', async () => {
    invoke.mockResolvedValueOnce([{ path: 'C:\\Work', label: 'Work', kind: 'folder' }]);
    expect(await activityFavorites()).toEqual([
      { path: 'C:\\Work', label: 'Work', kind: 'folder' },
    ]);
    expect(invoke).toHaveBeenCalledWith('activity_favorites', {});

    invoke.mockResolvedValueOnce(true);
    expect(await activityAddFavorite('C:\\Work\\book.xlsx', 'file')).toBe(true);
    expect(invoke).toHaveBeenCalledWith('activity_add_favorite', {
      path: 'C:\\Work\\book.xlsx',
      kind: 'file',
    });

    invoke.mockResolvedValueOnce(false);
    expect(await activityRemoveFavorite('C:\\Work')).toBe(false);
    expect(invoke).toHaveBeenCalledWith('activity_remove_favorite', { path: 'C:\\Work' });
  });

  it('settings and scope commands map to their invoke names', async () => {
    invoke.mockResolvedValue(null);
    await activitySetRetention(30);
    expect(invoke).toHaveBeenLastCalledWith('activity_set_retention', { days: 30 });
    await activitySetHidden('l1', true);
    expect(invoke).toHaveBeenLastCalledWith('activity_set_hidden', {
      entityId: 'l1',
      hidden: true,
    });
    await activityAckDisclosure();
    expect(invoke).toHaveBeenLastCalledWith('activity_ack_disclosure', {});
    await activityOverview(90, 'file', 'frequent', 50);
    expect(invoke).toHaveBeenLastCalledWith('activity_overview', {
      days: 90,
      kind: 'file',
      sort: 'frequent',
      limit: 50,
    });
    await activityPreview('exclude', 'C:\\Work');
    expect(invoke).toHaveBeenLastCalledWith('activity_preview', {
      scope: 'exclude',
      target: 'C:\\Work',
    });
    await activityPreview('all');
    expect(invoke).toHaveBeenLastCalledWith('activity_preview', { scope: 'all', target: null });
    invoke.mockReset();
  });

  it('outside Tauri every call degrades without invoking', async () => {
    setTauri(false);
    expect(await activityRecent('folder', 5)).toEqual([]);
    expect(await activityFrequent('file', 5)).toEqual([]);
    expect(await activitySearch('invoice', 'all', 6)).toEqual([]);
    expect(await activityState()).toBeNull();
    expect(await shellOpen('C:\\x.txt', false)).toBe(false);
    expect(await activityFavorites()).toEqual([]);
    expect(await activityAddFavorite('C:\\Work', 'folder')).toBe(false);
    expect(await activityRemoveFavorite('C:\\Work')).toBe(false);
    recordActivityVisit('C:\\Work', 'folder');
    expect(invoke).not.toHaveBeenCalled();
  });
});
