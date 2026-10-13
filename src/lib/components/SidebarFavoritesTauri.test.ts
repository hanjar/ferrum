import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/svelte';

const ipc = vi.hoisted(() => ({
  activityAckDisclosure: vi.fn(),
  activityAddFavorite: vi.fn(),
  activityFavorites: vi.fn(async () => []),
  activityFrequent: vi.fn(async () => []),
  activityRecent: vi.fn(async () => []),
  activityRemoveFavorite: vi.fn(async () => true),
  activitySetHidden: vi.fn(async () => null),
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
vi.mock('../ipc/drives', async (orig) => ({
  ...(await orig<typeof import('../ipc/drives')>()),
  listDrives: vi.fn(async () => []),
}));

import Sidebar from './Sidebar.svelte';
import { activity } from '../stores/activityStore.svelte';

beforeEach(() => {
  activity.available = false;
  activity.favorites = [];
  activity.recent = [];
  activity.frequent = [];
});

describe('Sidebar Favorites in Tauri', () => {
  it('does not show browser-dev mock favorites while activity is unavailable', async () => {
    render(Sidebar);
    await waitFor(() => expect(screen.getByText('Favorites tidak tersedia')).toBeTruthy());
    expect(screen.queryByText('Projects')).toBeNull();
    expect(screen.queryByText('Backup')).toBeNull();
    expect(screen.queryByText('Downloads')).toBeNull();
  });
});
