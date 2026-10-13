import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import HotlistSettingsSection from './HotlistSettingsSection.svelte';
import { hotlist } from '../stores/hotlistStore.svelte';
import { HotlistError, resetMemoryBackend, type HotlistView } from '../ipc/hotlist';

const view = (extra: Partial<HotlistView> = {}): HotlistView => ({
  doc: { version: 2, items: [] },
  status: 'ok',
  readOnly: false,
  customLocation: null,
  ...extra,
});

afterEach(async () => {
  cleanup();
  resetMemoryBackend();
  await hotlist.load();
  vi.restoreAllMocks();
});

async function startEditing(path = 'D:\\Drive\\hotlist.json') {
  await fireEvent.click(screen.getByRole('button', { name: 'Change…' }));
  await fireEvent.input(screen.getByLabelText('Hotlist file path'), { target: { value: path } });
  await fireEvent.click(screen.getByRole('button', { name: 'Check' }));
}

describe('Settings › Hotlist location', () => {
  it('shows the default location and the custom one', async () => {
    render(HotlistSettingsSection);
    expect(screen.getByTestId('hotlist-location').textContent).toMatch(/Default/);
    hotlist.applyView(view({ customLocation: 'D:\\Drive\\hotlist.json' }));
    await waitFor(() =>
      expect(screen.getByTestId('hotlist-location').textContent).toBe('D:\\Drive\\hotlist.json'),
    );
  });

  it('offers MOVE when nothing exists at the new location, and applies it', async () => {
    render(HotlistSettingsSection);
    vi.spyOn(hotlist.api, 'checkLocation').mockResolvedValue(false);
    const set = vi
      .spyOn(hotlist.api, 'setLocation')
      .mockResolvedValue(view({ customLocation: 'D:\\Drive\\hotlist.json' }));
    await startEditing();
    const group = await screen.findByRole('group', { name: 'Location choice' });
    expect(group.textContent).toMatch(/no hotlist file there yet/i);
    expect(screen.queryByRole('button', { name: 'Use the existing file' })).toBeNull();
    await fireEvent.click(screen.getByRole('button', { name: 'Move my hotlist there' }));
    await waitFor(() =>
      expect(set).toHaveBeenCalledExactlyOnceWith('D:\\Drive\\hotlist.json', 'move'),
    );
    expect(await screen.findByText('Hotlist moved.')).toBeTruthy();
    expect(hotlist.customLocation).toBe('D:\\Drive\\hotlist.json');
  });

  it('offers only USE when a hotlist already exists there (never overwrites)', async () => {
    render(HotlistSettingsSection);
    vi.spyOn(hotlist.api, 'checkLocation').mockResolvedValue(true);
    const set = vi.spyOn(hotlist.api, 'setLocation').mockResolvedValue(view());
    await startEditing();
    const group = await screen.findByRole('group', { name: 'Location choice' });
    expect(group.textContent).toMatch(/already exists/i);
    expect(screen.queryByRole('button', { name: 'Move my hotlist there' })).toBeNull();
    await fireEvent.click(screen.getByRole('button', { name: 'Use the existing file' }));
    await waitFor(() => expect(set).toHaveBeenCalledWith('D:\\Drive\\hotlist.json', 'use'));
  });

  it('explains unusable and invalid locations without leaking anything', async () => {
    render(HotlistSettingsSection);
    const check = vi.spyOn(hotlist.api, 'checkLocation');
    check.mockRejectedValueOnce(new HotlistError('target_unusable'));
    await startEditing();
    expect((await screen.findByText(/can't be used/)).textContent).toMatch(/online-only/);
    check.mockRejectedValueOnce(new HotlistError('invalid_path'));
    await fireEvent.click(screen.getByRole('button', { name: 'Check' }));
    expect(await screen.findByText(/full local path to a .json file/)).toBeTruthy();
    expect(screen.queryByRole('group', { name: 'Location choice' })).toBeNull();
  });

  it('"Use default" goes through the same safe check', async () => {
    hotlist.applyView(view({ customLocation: 'D:\\x.json' }));
    render(HotlistSettingsSection);
    const check = vi.spyOn(hotlist.api, 'checkLocation').mockResolvedValue(false);
    await fireEvent.click(screen.getByRole('button', { name: 'Use default' }));
    await screen.findByRole('group', { name: 'Location choice' });
    expect(check).toHaveBeenCalledExactlyOnceWith(null);
  });

  it.each([
    ['cloud', /online-only/],
    ['unreadable', /can't be read right now/],
    ['invalid', /not a valid hotlist/],
    ['missing', /gone from this location/],
  ] as const)('explains the %s state', async (status, text) => {
    hotlist.applyView(view({ status, readOnly: true }));
    render(HotlistSettingsSection);
    expect(screen.getByText(text)).toBeTruthy();
  });

  it('Reload re-reads the file', async () => {
    render(HotlistSettingsSection);
    const load = vi.spyOn(hotlist.api, 'load').mockResolvedValue(view());
    await fireEvent.click(screen.getByRole('button', { name: 'Reload' }));
    expect(load).toHaveBeenCalled();
  });
});

describe('Settings › Hotlist unsafe locations (FER-467 R7)', () => {
  it.each([
    ['location_link', /junction or symbolic link/],
    ['location_network', /network drive/],
    ['location_reserved', /reserved by Windows/],
    ['location_stream', /data streams/],
  ])('explains a %s refusal from the check', async (kind, text) => {
    render(HotlistSettingsSection);
    vi.spyOn(hotlist.api, 'checkLocation').mockRejectedValue(new HotlistError(kind));
    await startEditing('J:\\via-junction\\hotlist.json');
    expect((await screen.findByText(text)).textContent).toMatch(text);
    expect(screen.queryByRole('group', { name: 'Location choice' })).toBeNull();
  });

  it('a saved location that became unsafe is read-only and says why', async () => {
    render(HotlistSettingsSection);
    hotlist.applyView(
      view({
        status: 'unsafe',
        readOnly: true,
        customLocation: 'D:\\sync\\hotlist.json',
        locationProblem: 'location_link',
      }),
    );
    await waitFor(() =>
      expect(screen.getByTestId('location-problem').textContent).toMatch(/junction/),
    );
    expect(screen.getByText(/not safe to use/)).toBeTruthy();
  });
});
