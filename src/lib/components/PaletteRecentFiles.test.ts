import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/svelte';
import { tick } from 'svelte';

const ipc = vi.hoisted(() => ({
  activityFavorites: vi.fn(async () => []),
  activityFrequent: vi.fn(async () => []),
  activityRecent: vi.fn(),
  activitySearch: vi.fn(async () => []),
  activityState: vi.fn(async () => null),
  onActivityChanged: vi.fn(async () => () => undefined),
  recordActivityVisit: vi.fn(),
  shellOpen: vi.fn(async () => true),
}));
vi.mock('../ipc/activity', () => ipc);
const recentIpc = vi.hoisted(() => ({
  recentWindows: vi.fn(),
  recentNewItems: vi.fn(),
  recentDefaultFolders: vi.fn(async () => ['C:\\Users\\a\\Downloads']),
  recentValidateFolders: vi.fn(async (f: string[]) => f),
}));
vi.mock('../ipc/recent', () => recentIpc);

import CommandPalette from './CommandPalette.svelte';
import { ui } from '../uiState.svelte';
import { activity } from '../stores/activityStore.svelte';
import { recentStore } from '../stores/recentStore.svelte';
import { registerPane } from '../stores/panesRegistry.svelte';
import { recentCommands } from '../stores/recentCommands.svelte';
import { resetModalDepth } from '../dialogKit';

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
  window.matchMedia = vi.fn(
    () => ({ matches: false, addEventListener: vi.fn(), removeEventListener: vi.fn() }) as never,
  );
  recentCommands.clear();
  recentStore.reset();
  activity.favorites = [];
  activity.recent = [];
  activity.frequent = [];
  activity.privateSession = false;
  activity.recording = true;
  ui.includeWindowsRecent = true;
  ui.recentNewItems = true;
  ui.recentWatchFolders = null;
  ui.paletteOpen = false;
  resetModalDepth();
  ipc.shellOpen.mockClear();
  ipc.activityRecent.mockImplementation(async (kind: string) =>
    kind === 'file'
      ? [act('file', 'C:\\Docs\\ferrum-only.txt', 100), act('file', 'C:\\Docs\\shared.txt', 150)]
      : [act('folder', 'C:\\Docs', 10)],
  );
  recentIpc.recentWindows.mockResolvedValue([
    dto('c:\\docs\\SHARED.txt', 500, 'windows', 'Windows'),
    dto('D:\\Other\\budget-2026.xlsx', 400, 'windows', 'Windows'),
  ]);
  recentIpc.recentNewItems.mockResolvedValue([
    dto('C:\\Users\\a\\Downloads\\installer.exe', 450, 'new', 'Downloads'),
  ]);
});
afterEach(() => {
  ui.paletteOpen = false;
  activity.privateSession = false;
  ui.includeWindowsRecent = true;
  resetModalDepth();
});

async function openPalette() {
  render(CommandPalette);
  ui.paletteOpen = true;
  await tick();
  const pal = screen.getByRole('dialog', { name: 'Command palette' });
  await vi.waitFor(() => expect(recentStore.loading).toBe(false));
  await tick();
  return pal;
}
const group = (root: HTMLElement, name: string) =>
  root.querySelector(`[role="group"][aria-label="${name}"]`);
const labels = (root: HTMLElement, name: string) =>
  Array.from(group(root, name)?.querySelectorAll('.label') ?? []).map((el) => el.textContent);

describe('Ctrl+K Recent files group (FER-598)', () => {
  it('shows recent files from every source, de-duplicated, newest first, files only', async () => {
    const pal = await openPalette();
    expect(labels(pal, 'Recent files')).toEqual([
      'SHARED.txt', // Windows time wins over the Ferrum one (same path, other case)
      'installer.exe',
      'budget-2026.xlsx',
      'ferrum-only.txt',
    ]);
    expect(labels(pal, 'Recent files')).not.toContain('Docs'); // folders are not files
    const rows = Array.from(group(pal, 'Recent files')!.querySelectorAll('[role="option"]'));
    expect(rows[0].textContent).toContain('Ferrum + Windows');
    expect(rows[1].textContent).toContain('Downloads');
  });

  it('caps the group at 5 rows', async () => {
    ipc.activityRecent.mockImplementation(async (kind: string) =>
      kind === 'file'
        ? Array.from({ length: 12 }, (_, i) => act('file', `C:\\Many\\f${i}.txt`, i))
        : [],
    );
    const pal = await openPalette();
    expect(labels(pal, 'Recent files')).toHaveLength(5);
  });

  it('a private session hides every recent source and reads none', async () => {
    activity.privateSession = true;
    const pal = await openPalette();
    expect(group(pal, 'Recent files')).toBeNull();
    expect(ipc.activityRecent).not.toHaveBeenCalled();
    expect(recentIpc.recentWindows).not.toHaveBeenCalled();
    expect(recentIpc.recentNewItems).not.toHaveBeenCalled();
  });

  it('"Include Windows Recent" off hides those items and never reads the folder', async () => {
    ui.includeWindowsRecent = false;
    const pal = await openPalette();
    expect(recentIpc.recentWindows).not.toHaveBeenCalled();
    expect(labels(pal, 'Recent files')).toEqual(['installer.exe', 'shared.txt', 'ferrum-only.txt']);
  });

  it('Enter opens the file with the default app, recorded', async () => {
    const pal = await openPalette();
    const first = group(pal, 'Recent files')!.querySelector('[role="option"]') as HTMLElement;
    await fireEvent.click(first);
    expect(ipc.shellOpen).toHaveBeenCalledWith('c:\\docs\\SHARED.txt', true);
    expect(ui.paletteOpen).toBe(false);
  });

  it('Ctrl+Enter goes to the folder with the item selected', async () => {
    const revealPath = vi.fn();
    const off = registerPane(0, {
      path: 'C:\\',
      selectedPaths: () => [],
      cursorEntry: () => undefined,
      refresh: () => undefined,
      newTab: () => undefined,
      closeCurrentTab: () => undefined,
      goUp: () => undefined,
      openCursorEntry: () => undefined,
      revealPath,
    });
    const pal = await openPalette();
    // Move the highlight to the first Recent files row, then Ctrl+Enter.
    const options = screen.getAllByRole('option');
    const at = options.findIndex((o) => o.textContent?.includes('SHARED.txt'));
    for (let i = 0; i < at; i++) await fireEvent.keyDown(pal, { key: 'ArrowDown' });
    await fireEvent.keyDown(pal, { key: 'Enter', ctrlKey: true });
    expect(revealPath).toHaveBeenCalledWith('c:\\docs\\SHARED.txt');
    expect(ipc.shellOpen).not.toHaveBeenCalled();
    off();
  });

  it('typing matches Windows Recent and new items by name, not only Ferrum history', async () => {
    await openPalette();
    const input = screen.getByRole('combobox');
    await fireEvent.input(input, { target: { value: 'budget' } });
    await tick();
    await vi.waitFor(() =>
      expect(
        screen.getAllByRole('option').some((o) => o.textContent?.includes('budget-2026.xlsx')),
      ).toBe(true),
    );
    await fireEvent.input(input, { target: { value: 'install' } });
    await tick();
    await vi.waitFor(() =>
      expect(
        screen.getAllByRole('option').some((o) => o.textContent?.includes('installer.exe')),
      ).toBe(true),
    );
  });

  it('File mode with an empty input lists recent files only', async () => {
    await openPalette();
    const input = screen.getByRole('combobox');
    await fireEvent.input(input, { target: { value: 'ab' } });
    await tick();
    await fireEvent.click(screen.getByRole('button', { name: 'File' }));
    await fireEvent.input(input, { target: { value: '' } });
    await tick();
    const pal = screen.getByRole('dialog', { name: 'Command palette' });
    expect(group(pal, 'Perintah')).toBeNull();
    expect(labels(pal, 'Recent files').length).toBeGreaterThanOrEqual(4);
  });
});
