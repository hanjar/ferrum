import { beforeEach, describe, expect, it, vi } from 'vitest';

const ipc = vi.hoisted(() => ({
  recentWindows: vi.fn(),
  recentNewItems: vi.fn(),
  recentDefaultFolders: vi.fn(),
  recentValidateFolders: vi.fn(),
}));
vi.mock('../ipc/recent', () => ipc);
const activityRecent = vi.hoisted(() => vi.fn());
vi.mock('../ipc/activity', async (orig) => ({
  ...(await orig<typeof import('../ipc/activity')>()),
  activityRecent,
}));

import { recentStore } from './recentStore.svelte';
import { activity } from './activityStore.svelte';
import { ui } from '../uiState.svelte';

const act = (kind: 'file' | 'folder', locator: string, ms: number) => ({
  entity: {
    id: locator,
    kind,
    provider: 'fs',
    volumeId: 'v',
    locator,
    parentPath: locator.slice(0, locator.lastIndexOf('\\')),
    displayName: locator.split('\\').pop(),
    availability: 'available',
  },
  lastUtcMs: ms,
  episodeCount: 1,
  activeDays: 1,
  score: null,
});
const dto = (path: string, ms: number, source: 'windows' | 'new', label: string) => ({
  path,
  name: path.split('\\').pop()!,
  isDir: false,
  lastUsedMs: ms,
  source,
  label,
});

beforeEach(() => {
  recentStore.reset();
  activity.privateSession = false;
  ui.includeWindowsRecent = true;
  ui.recentNewItems = true;
  ui.recentWatchFolders = null;
  ui.recentNewDays = 7;
  for (const fn of Object.values(ipc)) fn.mockReset();
  activityRecent.mockReset();
  activityRecent.mockImplementation(async (kind: string) =>
    kind === 'file' ? [act('file', 'C:\\Docs\\a.txt', 100)] : [act('folder', 'C:\\Docs', 50)],
  );
  ipc.recentWindows.mockResolvedValue([
    dto('c:\\docs\\A.TXT', 300, 'windows', 'Windows'),
    dto('D:\\Other\\b.pdf', 200, 'windows', 'Windows'),
  ]);
  ipc.recentDefaultFolders.mockResolvedValue(['C:\\Users\\a\\Downloads']);
  ipc.recentNewItems.mockResolvedValue([
    dto('C:\\Users\\a\\Downloads\\new.zip', 400, 'new', 'Downloads'),
  ]);
});

describe('recentStore (FER-598)', () => {
  it('merges all three sources, de-duplicates by path, newest first', async () => {
    await recentStore.refresh();
    expect(recentStore.items.map((i) => i.path)).toEqual([
      'C:\\Users\\a\\Downloads\\new.zip',
      'c:\\docs\\A.TXT',
      'D:\\Other\\b.pdf',
      'C:\\Docs',
    ]);
    const dup = recentStore.items.find((i) => i.path.toLowerCase() === 'c:\\docs\\a.txt')!;
    expect(dup.sources).toEqual(['Ferrum', 'Windows']);
    expect(dup.lastUsedMs).toBe(300);
    // files only
    expect(recentStore.files.map((i) => i.name)).not.toContain('Docs');
  });

  it('uses the Downloads default when no folder list is configured, the list otherwise', async () => {
    await recentStore.refresh();
    expect(ipc.recentNewItems).toHaveBeenCalledWith(['C:\\Users\\a\\Downloads'], 7, 200);
    ui.recentWatchFolders = ['D:\\Work'];
    ui.recentNewDays = 14;
    await recentStore.refresh();
    expect(ipc.recentNewItems).toHaveBeenLastCalledWith(['D:\\Work'], 14, 200);
  });

  it('a private session hides Ferrum items and new items at once, and reads none of them', async () => {
    await recentStore.refresh();
    activity.privateSession = true;
    expect(recentStore.items.map((i) => i.path)).toEqual(['c:\\docs\\A.TXT', 'D:\\Other\\b.pdf']);
    activityRecent.mockClear();
    ipc.recentNewItems.mockClear();
    await recentStore.refresh();
    expect(activityRecent).not.toHaveBeenCalled();
    expect(ipc.recentNewItems).not.toHaveBeenCalled();
    activity.privateSession = false;
  });

  it('"Include Windows Recent" off hides the items and never reads the folder', async () => {
    await recentStore.refresh();
    ui.includeWindowsRecent = false;
    expect(
      recentStore.items.some((i) => i.sources.includes('Windows') && i.sources.length === 1),
    ).toBe(false);
    ipc.recentWindows.mockClear();
    await recentStore.refresh();
    expect(ipc.recentWindows).not.toHaveBeenCalled();
    expect(recentStore.items.map((i) => i.path)).not.toContain('D:\\Other\\b.pdf');
  });

  it('the watched-folder source has its own switch', async () => {
    ui.recentNewItems = false;
    ipc.recentNewItems.mockClear();
    await recentStore.refresh();
    expect(ipc.recentNewItems).not.toHaveBeenCalled();
    expect(recentStore.items.map((i) => i.path)).not.toContain('C:\\Users\\a\\Downloads\\new.zip');
  });

  it('a failing or hanging source contributes nothing and does not break the others', async () => {
    ipc.recentWindows.mockRejectedValue(new Error('boom'));
    vi.useFakeTimers();
    ipc.recentNewItems.mockReturnValue(new Promise(() => undefined)); // never answers
    const done = recentStore.refresh();
    await vi.advanceTimersByTimeAsync(5000);
    await done;
    vi.useRealTimers();
    expect(recentStore.items.map((i) => i.path)).toEqual(['C:\\Docs\\a.txt', 'C:\\Docs']);
  });
});
