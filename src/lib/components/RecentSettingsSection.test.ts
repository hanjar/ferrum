import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/svelte';

const ipc = vi.hoisted(() => ({
  recentWindows: vi.fn(async () => []),
  recentNewItems: vi.fn(async () => []),
  recentDefaultFolders: vi.fn(async () => ['C:\\Users\\a\\Downloads']),
  recentValidateFolders: vi.fn(async (f: string[]) => f.filter((p) => /^[A-Za-z]:\\/.test(p))),
}));
vi.mock('../ipc/recent', () => ipc);
vi.mock('../ipc/activity', async (orig) => ({
  ...(await orig<typeof import('../ipc/activity')>()),
  activityRecent: vi.fn(async () => []),
}));

import RecentSettingsSection from './RecentSettingsSection.svelte';
import { ui } from '../uiState.svelte';

beforeEach(() => {
  ui.includeWindowsRecent = true;
  ui.recentNewItems = true;
  ui.recentWatchFolders = null;
  ui.recentNewDays = 7;
});

describe('Recent files settings (FER-598)', () => {
  it('defaults: Windows Recent on, Downloads watched', async () => {
    render(RecentSettingsSection);
    expect(
      (screen.getByRole('switch', { name: 'Include Windows Recent' }) as HTMLInputElement).checked,
    ).toBe(true);
    await vi.waitFor(() => expect(screen.getByText('C:\\Users\\a\\Downloads')).toBeTruthy());
  });

  it('the Windows Recent switch writes the setting', async () => {
    render(RecentSettingsSection);
    await fireEvent.click(screen.getByRole('switch', { name: 'Include Windows Recent' }));
    expect(ui.includeWindowsRecent).toBe(false);
  });

  it('adds a valid folder, refuses an invalid one with a message, removes a folder', async () => {
    render(RecentSettingsSection);
    await vi.waitFor(() => expect(screen.getByText('C:\\Users\\a\\Downloads')).toBeTruthy());
    const input = screen.getByRole('textbox', { name: 'Folder to watch' });
    await fireEvent.input(input, { target: { value: 'relative\\dir' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Add' }));
    await vi.waitFor(() => expect(screen.getByRole('alert')).toBeTruthy());
    expect(ui.recentWatchFolders).toBeNull();

    await fireEvent.input(input, { target: { value: 'D:\\Work' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Add' }));
    await vi.waitFor(() =>
      expect(ui.recentWatchFolders).toEqual(['C:\\Users\\a\\Downloads', 'D:\\Work']),
    );

    await fireEvent.click(screen.getByRole('button', { name: 'Remove C:\\Users\\a\\Downloads' }));
    expect(ui.recentWatchFolders).toEqual(['D:\\Work']);
  });

  it('the day window is one of 1/3/7/14/30', async () => {
    render(RecentSettingsSection);
    const select = screen.getByLabelText('New items window in days') as HTMLSelectElement;
    expect(Array.from(select.options).map((o) => o.value)).toEqual(['1', '3', '7', '14', '30']);
    await fireEvent.change(select, { target: { value: '14' } });
    expect(ui.recentNewDays).toBe(14);
  });
});
