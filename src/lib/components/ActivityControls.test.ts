import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';

const ipc = vi.hoisted(() => ({
  activityAckDisclosure: vi.fn(),
  activityAddFavorite: vi.fn(),
  activityClear: vi.fn(),
  activityExclude: vi.fn(),
  activityFavorites: vi.fn(async () => []),
  activityForget: vi.fn(),
  activityFrequent: vi.fn(async () => []),
  activityRecent: vi.fn(async () => []),
  activityRemoveFavorite: vi.fn(),
  activitySetHidden: vi.fn(),
  activitySetRecording: vi.fn(),
  activitySetRetention: vi.fn(),
  activityState: vi.fn(),
  activityOverview: vi.fn(),
  activityPreview: vi.fn(),
  onActivityChanged: vi.fn(async () => () => undefined),
  recordActivityVisit: vi.fn(),
  shellOpen: vi.fn(),
}));
vi.mock('../ipc/activity', () => ipc);
vi.mock('../stores/panesRegistry.svelte', () => ({
  activePane: () => ({ path: 'C:\\Work' }),
}));

import ActivityPrivacyControls from './ActivityPrivacyControls.svelte';
import ActivityDisclosure from './ActivityDisclosure.svelte';
import { activity } from '../stores/activityStore.svelte';
import { activityPage } from '../stores/activityPageStore.svelte';

const state = (extra: Record<string, unknown> = {}) => ({
  recordingEnabled: true,
  privacyEpoch: 1,
  retentionDays: 90,
  hiddenCount: 0,
  disclosureAcked: false,
  ...extra,
});

beforeEach(() => {
  for (const fn of Object.values(ipc)) fn.mockClear();
  activityPage.reset();
  activity.available = true;
  activity.recording = true;
  activity.privateSession = false;
  activity.privacyEpoch = 1;
  activity.retentionDays = 90;
  activity.disclosureAcked = false;
});

describe('first-run disclosure', () => {
  it('shows the short notice until acknowledged, then disappears', async () => {
    ipc.activityAckDisclosure.mockResolvedValue(state({ disclosureAcked: true }));
    render(ActivityDisclosure);
    expect(screen.getByRole('region', { name: /pencatatan activity/i })).toBeTruthy();
    expect(screen.getByText(/tidak\s+dikirim ke mana pun/)).toBeTruthy();
    // Chat consent: the typed question goes to the provider, the history result stays local.
    expect(screen.getByText(/pertanyaan\s+yang\s+Anda ketik dikirim ke provider AI/)).toBeTruthy();
    await fireEvent.click(screen.getByRole('button', { name: 'Mengerti' }));
    await waitFor(() => expect(screen.queryByRole('region')).toBeNull());
    expect(ipc.activityAckDisclosure).toHaveBeenCalledTimes(1);
  });
});

describe('privacy controls', () => {
  it('clear-all previews the scope and does nothing until confirmed', async () => {
    ipc.activityPreview.mockResolvedValue({
      scope: 'all',
      items: 4,
      episodes: 11,
      sample: ['C:\\Work\\A'],
      truncated: false,
      favoritesKept: 2,
      hiddenKept: 0,
    });
    ipc.activityClear.mockResolvedValue(11);
    ipc.activityState.mockResolvedValue(state());
    ipc.activityOverview.mockResolvedValue(null);
    render(ActivityPrivacyControls);
    await fireEvent.click(screen.getByRole('button', { name: 'Hapus semua riwayat activity' }));
    const dialog = await screen.findByRole('alertdialog');
    await waitFor(() => expect(dialog.textContent).toContain('4 item'));
    expect(dialog.textContent).toContain('11 kunjungan');
    expect(dialog.textContent).toContain('Favorites (2)');
    expect(ipc.activityClear).not.toHaveBeenCalled();

    await fireEvent.click(screen.getByRole('button', { name: 'Batal' }));
    expect(screen.queryByRole('alertdialog')).toBeNull();
    expect(ipc.activityClear).not.toHaveBeenCalled();
  });

  it('shortening retention previews; lengthening applies immediately', async () => {
    ipc.activityPreview.mockResolvedValue({
      scope: 'retention',
      items: 1,
      episodes: 2,
      sample: [],
      truncated: false,
      favoritesKept: 0,
      hiddenKept: 0,
    });
    ipc.activitySetRetention.mockResolvedValue(state({ retentionDays: 30 }));
    ipc.activityState.mockResolvedValue(state({ retentionDays: 30 }));
    render(ActivityPrivacyControls);
    await fireEvent.click(screen.getByRole('radio', { name: '30 hari' }));
    await screen.findByRole('alertdialog');
    expect(ipc.activitySetRetention).not.toHaveBeenCalled();
    await fireEvent.click(screen.getByRole('button', { name: 'Konfirmasi' }));
    await waitFor(() => expect(ipc.activitySetRetention).toHaveBeenCalledWith(30));

    await fireEvent.click(screen.getByRole('radio', { name: '90 hari' }));
    await waitFor(() => expect(ipc.activitySetRetention).toHaveBeenCalledWith(90));
    expect(ipc.activityPreview).toHaveBeenCalledTimes(1);
  });

  it('private session toggle is per-window and blocks local writes', async () => {
    render(ActivityPrivacyControls);
    await fireEvent.click(screen.getByRole('checkbox', { name: /Sesi privat/ }));
    expect(activity.privateSession).toBe(true);
    expect(ipc.activitySetRecording).not.toHaveBeenCalled(); // not a global pause
    activity.recordVisit('C:\\Work');
    expect(ipc.recordActivityVisit).not.toHaveBeenCalled();
  });
});
