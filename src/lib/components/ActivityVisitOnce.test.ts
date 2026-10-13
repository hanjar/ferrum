/**
 * FER-251: a deliberate folder open from the sidebar or the palette produces
 * exactly ONE activity receipt (recordActivityVisit), through the real
 * PaneStore.onVisit -> activity.recordVisit chain used by FilePane.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';

const ipc = vi.hoisted(() => ({
  activityAckDisclosure: vi.fn(),
  activityAddFavorite: vi.fn(),
  activityFavorites: vi.fn(async () => []),
  activityFrequent: vi.fn(async () => []),
  activityRecent: vi.fn(async () => []),
  activityRemoveFavorite: vi.fn(),
  activitySearch: vi.fn<(q: string, kind: string, limit?: number) => Promise<unknown[] | null>>(
    async () => [],
  ),
  activitySetHidden: vi.fn(),
  activityState: vi.fn(async () => null),
  onActivityChanged: vi.fn(async () => () => undefined),
  recordActivityVisit: vi.fn(),
  shellOpen: vi.fn(async () => true),
}));
vi.mock('../ipc/activity', () => ipc);
vi.mock('../ipc/tauri', async (orig) => ({
  ...(await orig<typeof import('../ipc/tauri')>()),
  isInTauri: () => true,
}));

import Sidebar from './Sidebar.svelte';
import CommandPalette from './CommandPalette.svelte';
import { PaneStore } from '../stores/paneStore.svelte';
import { registerPane } from '../stores/panesRegistry.svelte';
import { activity } from '../stores/activityStore.svelte';
import { ui } from '../uiState.svelte';
import type { ActivityItemDto } from '../ipc/activity';
import type { FileEntry } from '../ipc/types';

const folder = (name: string, parent = 'C:\\Work'): ActivityItemDto => ({
  entity: {
    id: `id:${name}`,
    kind: 'folder',
    provider: 'local',
    volumeId: 'v',
    locator: `${parent}\\${name}`,
    parentPath: parent,
    displayName: name,
    availability: 'available',
  },
  lastUtcMs: 1,
  episodeCount: 2,
  activeDays: 1,
  score: null,
});

let store: PaneStore;
let unregister: () => void;

function seedRecent(list: ActivityItemDto[]) {
  activity.recent = list;
  ipc.activityRecent.mockImplementation(async () => list as never);
}

beforeEach(async () => {
  for (const fn of Object.values(ipc)) fn.mockClear();
  ipc.activityState.mockImplementation(
    async () => ({ recordingEnabled: true, privacyEpoch: 1, disclosureAcked: true }) as never,
  );
  activity.available = true;
  activity.recording = true;
  activity.privateSession = false;
  activity.disclosureAcked = true;
  activity.privacyEpoch = 1;
  activity.favorites = [];
  activity.frequent = [];
  seedRecent([folder('Docs')]);
  ui.activePane = 0;
  ui.paletteOpen = false;

  const fetcher = async (): Promise<FileEntry[]> => [];
  store = new PaneStore(fetcher, 'C:\\home');
  await store.newTab('C:\\home'); // startup restore: not a visit
  store.onVisit = (p) => activity.recordVisit(p); // same wiring as FilePane
  unregister = registerPane(0, {
    get path() {
      return store.path;
    },
    selectedPaths: () => [],
    cursorEntry: () => undefined,
    refresh: () => void store.refresh(),
    newTab: () => void store.newTab(),
    closeCurrentTab: () => undefined,
    goUp: () => void store.goUp(),
    openCursorEntry: () => undefined,
    openPath: (p: string) => void store.open(p),
  });
});
afterEach(() => {
  unregister();
  ui.paletteOpen = false;
});

const settle = () => new Promise((r) => setTimeout(r, 30));

describe('exactly one receipt per deliberate folder open (FER-251)', () => {
  it('sidebar Recent click records once', async () => {
    render(Sidebar);
    await fireEvent.click(await screen.findByRole('button', { name: /^Docs/ }));
    await waitFor(() => expect(store.path).toBe('C:\\Work\\Docs'));
    await settle();
    expect(ipc.recordActivityVisit).toHaveBeenCalledTimes(1);
    expect(ipc.recordActivityVisit).toHaveBeenCalledWith('C:\\Work\\Docs', 'folder');
  });

  it('palette Enter on a folder (empty query group) records once', async () => {
    render(CommandPalette);
    ui.paletteOpen = true;
    await tick();
    await screen.findByRole('option', { name: /Docs/ });
    await fireEvent.keyDown(screen.getByRole('dialog'), { key: 'Enter' });
    await waitFor(() => expect(store.path).toBe('C:\\Work\\Docs'));
    await settle();
    expect(ipc.recordActivityVisit).toHaveBeenCalledTimes(1);
    expect(ipc.recordActivityVisit).toHaveBeenCalledWith('C:\\Work\\Docs', 'folder');
  });

  it('palette Enter on a searched history folder records once', async () => {
    seedRecent([]);
    ipc.activitySearch.mockResolvedValue([
      { entity: folder('Photos').entity, matchClass: 'exact' },
    ]);
    render(CommandPalette);
    ui.paletteOpen = true;
    await tick();
    await fireEvent.input(screen.getByRole('combobox'), { target: { value: 'photo' } });
    await screen.findByRole('option', { name: /Photos/ });
    await fireEvent.keyDown(screen.getByRole('dialog'), { key: 'Enter' });
    await waitFor(() => expect(store.path).toBe('C:\\Work\\Photos'));
    await settle();
    expect(ipc.recordActivityVisit).toHaveBeenCalledTimes(1);
  });

  it('paused recording writes no receipt at all', async () => {
    activity.recording = false;
    ipc.activityState.mockImplementation(
      async () => ({ recordingEnabled: false, privacyEpoch: 1, disclosureAcked: true }) as never,
    );
    render(Sidebar);
    await fireEvent.click(await screen.findByRole('button', { name: /^Docs/ }));
    await waitFor(() => expect(store.path).toBe('C:\\Work\\Docs'));
    await settle();
    expect(ipc.recordActivityVisit).not.toHaveBeenCalled();
  });
});
