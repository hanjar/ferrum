import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';

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

const openPath = vi.hoisted(() => vi.fn());
vi.mock('../stores/panesRegistry.svelte', () => ({
  activePane: () => ({ path: '/work', openPath }),
}));

import Sidebar from './Sidebar.svelte';
import { activity } from '../stores/activityStore.svelte';
import type { ActivityItemDto } from '../ipc/activity';

const item = (name: string, parent = '/work', n = 2): ActivityItemDto => ({
  entity: {
    id: `id-${parent}-${name}`,
    kind: 'folder',
    provider: 'local',
    volumeId: 'v',
    locator: `${parent}/${name}`,
    parentPath: parent,
    displayName: name,
    availability: 'available',
  },
  lastUtcMs: 1,
  episodeCount: n,
  activeDays: 1,
  score: null,
});

const state = () => ({ recordingEnabled: true, privacyEpoch: 1, disclosureAcked: true });

beforeEach(() => {
  for (const fn of Object.values(ipc)) fn.mockClear();
  openPath.mockClear();
  ipc.activityState.mockImplementation(async () => state() as never);
  activity.available = true;
  activity.recording = true;
  activity.privateSession = false;
  activity.privacyEpoch = 1;
  activity.disclosureAcked = true;
  activity.recent = [];
  activity.frequent = [];
  activity.favorites = [];
});

describe('Sidebar activity sections (FER-257)', () => {
  it('renders Recent, Frequent and Favorites with their items', async () => {
    ipc.activityRecent.mockResolvedValue([item('Alpha')] as never);
    ipc.activityFrequent.mockResolvedValue([item('Beta', '/work', 5)] as never);
    ipc.activityFavorites.mockResolvedValue([
      { path: '/pinned', label: 'Pinned', kind: 'folder' },
    ] as never);
    render(Sidebar);
    await screen.findByText('Alpha');
    const recent = screen.getByRole('group', { name: 'Recent' });
    expect(recent.textContent).toContain('Alpha');
    expect(recent.textContent).toContain('2 kunjungan');
    const frequent = screen.getByRole('group', { name: 'Frequent' });
    expect(frequent.textContent).toContain('Beta');
    expect(frequent.textContent).toContain('5 kunjungan');
    expect(screen.getByRole('button', { name: 'Pinned' })).toBeTruthy();
    expect(screen.getByTitle('/pinned')).toBeTruthy();
    expect(screen.getByText('Favorites')).toBeTruthy();
  });

  it('shows explicit empty messages for Recent and Frequent', async () => {
    render(Sidebar);
    const recent = await screen.findByRole('group', { name: 'Recent' });
    expect(recent.textContent).toContain('Belum ada folder yang baru dibuka');
    const frequent = screen.getByRole('group', { name: 'Frequent' });
    expect(frequent.textContent).toContain('Belum ada folder yang sering dibuka');
    expect(screen.getByText('Ctrl+Shift+D untuk pin folder aktif')).toBeTruthy();
  });

  it('shows an explicit unavailable state instead of vanishing', async () => {
    ipc.activityState.mockImplementation(async () => null);
    activity.available = false;
    render(Sidebar);
    const note = await screen.findByText(/Riwayat Activity tidak tersedia \(offline\)/);
    expect(note.getAttribute('role')).toBe('status');
    expect(screen.queryByRole('group', { name: 'Recent' })).toBeNull();
    expect(screen.queryByText('Lihat semua Activity…')).toBeNull();
  });

  it('private session hides suggestions and says so', async () => {
    activity.privateSession = true;
    render(Sidebar);
    await screen.findByText(/Sesi privat/);
    expect(screen.queryByRole('group', { name: 'Recent' })).toBeNull();
    expect(screen.queryByRole('group', { name: 'Frequent' })).toBeNull();
  });

  it('hide button is reachable by keyboard (focusable, not display:none) and hides the item', async () => {
    ipc.activityRecent.mockResolvedValue([item('Alpha')] as never);
    render(Sidebar);
    const hide = (await screen.findByRole('button', {
      name: 'Sembunyikan Alpha dari saran',
    })) as HTMLButtonElement;
    expect(hide.tabIndex).toBeGreaterThanOrEqual(0);
    expect(hide.hidden).toBe(false);
    hide.focus();
    expect(document.activeElement).toBe(hide);
    await fireEvent.keyDown(hide, { key: 'Enter' });
    await fireEvent.click(hide); // Enter on a button fires click in browsers
    await waitFor(() => expect(ipc.activitySetHidden).toHaveBeenCalledWith('id-/work-Alpha', true));
    expect(screen.queryByText('Alpha')).toBeNull();
  });

  it('unpin button carries an accessible label', async () => {
    ipc.activityFavorites.mockResolvedValue([
      { path: '/pinned', label: 'Pinned', kind: 'folder' },
    ] as never);
    render(Sidebar);
    expect(await screen.findByRole('button', { name: 'Lepas pin Pinned' })).toBeTruthy();
  });
});
