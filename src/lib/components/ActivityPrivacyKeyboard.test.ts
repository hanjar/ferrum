/**
 * FER-577 (B3): the privacy confirmation (alertdialog) is a real modal step. It sits inside
 * Settings (or the Activity page), so Esc must cancel ONLY the confirmation.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

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

import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import ActivityPrivacyControls from './ActivityPrivacyControls.svelte';
import { activity } from '../stores/activityStore.svelte';
import { activityPage } from '../stores/activityPageStore.svelte';
import { dialog, modalDepth, resetModalDepth } from '../dialogKit';

let host: HTMLDivElement;
let outerEscape: ReturnType<typeof vi.fn<() => void>>;
let outer: { destroy(): void };

beforeEach(() => {
  for (const fn of Object.values(ipc)) fn.mockClear();
  resetModalDepth();
  activityPage.reset();
  activity.available = true;
  activity.recording = true;
  activity.privateSession = false;
  activity.privacyEpoch = 1;
  activity.retentionDays = 90;
  ipc.activityPreview.mockResolvedValue({
    scope: 'all',
    items: 4,
    episodes: 11,
    sample: [],
    truncated: false,
    favoritesKept: 0,
    hiddenKept: 0,
  });
  // Stand-in for the Settings dialog around the controls.
  host = document.createElement('div');
  host.tabIndex = -1;
  document.body.append(host);
  outerEscape = vi.fn<() => void>();
  outer = dialog(host, { onEscape: () => outerEscape() });
  render(ActivityPrivacyControls, { target: host });
});
afterEach(() => {
  outer.destroy();
  host.remove();
  activityPage.reset();
  resetModalDepth();
});

async function ask() {
  const trigger = screen.getByRole('button', { name: 'Hapus semua riwayat activity' });
  trigger.focus();
  await fireEvent.click(trigger);
  await waitFor(() => expect(screen.getByRole('alertdialog')).toBeTruthy());
  return trigger;
}

describe('privacy confirmation keyboard', () => {
  it('opens on the safe Batal, keeps Tab inside, counts as a modal', async () => {
    await ask();
    const batal = screen.getByRole('button', { name: 'Batal' });
    const konfirmasi = screen.getByRole('button', { name: 'Konfirmasi' });
    expect(document.activeElement).toBe(batal);
    expect(modalDepth()).toBe(2); // Settings + the confirmation
    konfirmasi.focus();
    await fireEvent.keyDown(konfirmasi, { key: 'Tab' });
    expect(document.activeElement).toBe(batal);
    await fireEvent.keyDown(batal, { key: 'Tab', shiftKey: true });
    expect(document.activeElement).toBe(konfirmasi);
  });

  it('Esc cancels only the confirmation and focus goes back to the button that asked', async () => {
    const trigger = await ask();
    await fireEvent.keyDown(screen.getByRole('button', { name: 'Batal' }), { key: 'Escape' });
    expect(activityPage.pending).toBeNull();
    expect(outerEscape).not.toHaveBeenCalled();
    await waitFor(() => expect(document.activeElement).toBe(trigger));
    // With nothing pending, Esc reaches the surrounding dialog again.
    await fireEvent.keyDown(trigger, { key: 'Escape' });
    expect(outerEscape).toHaveBeenCalledTimes(1);
  });

  it('Enter on Batal cancels and does not clear anything', async () => {
    await ask();
    const batal = screen.getByRole('button', { name: 'Batal' });
    const ev = new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true });
    batal.dispatchEvent(ev);
    expect(ev.defaultPrevented).toBe(false); // native activation, no dialog handler
    expect(ipc.activityClear).not.toHaveBeenCalled();
  });

  it('arrowing over the retention choices only moves focus (a shorter one would open the confirmation)', async () => {
    const r90 = screen.getByRole('radio', { name: '90 hari' });
    r90.focus();
    await fireEvent.keyDown(r90, { key: 'ArrowLeft' });
    expect(document.activeElement).toBe(screen.getByRole('radio', { name: '30 hari' }));
    expect(activityPage.pending).toBeNull();
    expect(ipc.activityPreview).not.toHaveBeenCalled();
  });
});
