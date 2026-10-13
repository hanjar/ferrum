import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render } from '@testing-library/svelte';
import { tick } from 'svelte';
import ArchiveSettingsSection from './ArchiveSettingsSection.svelte';
import { ui } from '../uiState.svelte';

const archiveMocks = vi.hoisted(() => ({
  archiveRarToolDetect: vi.fn(),
  archiveRarToolProbe: vi.fn(),
  flush: vi.fn(),
  order: [] as string[],
}));

vi.mock('../ipc/archive', () => ({
  archiveRarToolDetect: () => archiveMocks.archiveRarToolDetect(),
  archiveRarToolProbe: () => {
    archiveMocks.order.push(`probe:${ui.rarExePath}`);
    return archiveMocks.archiveRarToolProbe();
  },
}));

vi.mock('../stores/prefsStore.svelte', () => ({
  prefs: {
    flush: () => {
      archiveMocks.order.push(`flush:${ui.rarExePath}`);
      return archiveMocks.flush();
    },
  },
}));

beforeEach(() => {
  archiveMocks.archiveRarToolDetect.mockReset();
  archiveMocks.archiveRarToolProbe.mockReset();
  archiveMocks.flush.mockReset();
  archiveMocks.flush.mockResolvedValue(true);
  archiveMocks.order.length = 0;
  ui.rarExePath = null;
});

afterEach(() => {
  ui.rarExePath = null;
});

describe('FER-505: ArchiveSettingsSection component', () => {
  it('renders section and disclaimer text', () => {
    const { getByText } = render(ArchiveSettingsSection);
    expect(getByText(/Ferrum tidak menyertakan WinRAR/i)).toBeTruthy();
  });

  it('Cari otomatis populates the path field and updates ui.rarExePath on success', async () => {
    archiveMocks.archiveRarToolDetect.mockResolvedValue('C:\\Program Files\\WinRAR\\Rar.exe');
    const { getByTestId } = render(ArchiveSettingsSection);
    await tick();

    const detectBtn = getByTestId('rar-detect-btn');
    await fireEvent.click(detectBtn);
    await tick();

    expect(archiveMocks.archiveRarToolDetect).toHaveBeenCalledTimes(1);
    expect(ui.rarExePath).toBe('C:\\Program Files\\WinRAR\\Rar.exe');

    const input = getByTestId('rar-exe-path-input') as HTMLInputElement;
    expect(input.value).toBe('C:\\Program Files\\WinRAR\\Rar.exe');

    const status = getByTestId('rar-probe-status');
    expect(status.textContent).toContain('Ditemukan: C:\\Program Files\\WinRAR\\Rar.exe');
  });

  it('Cari otomatis shows not found message if detection returns null', async () => {
    archiveMocks.archiveRarToolDetect.mockResolvedValue(null);
    const { getByTestId } = render(ArchiveSettingsSection);
    await tick();

    const detectBtn = getByTestId('rar-detect-btn');
    await fireEvent.click(detectBtn);
    await tick();

    expect(archiveMocks.archiveRarToolDetect).toHaveBeenCalledTimes(1);
    expect(ui.rarExePath).toBeNull();

    const status = getByTestId('rar-probe-status');
    expect(status.textContent).toContain('tidak ditemukan di lokasi standar');
  });

  it('Tes button calls probe and displays version when probe succeeds', async () => {
    ui.rarExePath = 'C:\\Program Files\\WinRAR\\Rar.exe';
    archiveMocks.archiveRarToolProbe.mockResolvedValue({
      ok: true,
      version: 'RAR 7.00 x64',
    });
    const { getByTestId } = render(ArchiveSettingsSection);
    await tick();

    const probeBtn = getByTestId('rar-probe-btn');
    await fireEvent.click(probeBtn);
    await tick();

    expect(archiveMocks.archiveRarToolProbe).toHaveBeenCalledTimes(1);
    const status = getByTestId('rar-probe-status');
    expect(status.textContent).toContain('RAR 7.00 x64');
  });

  it('Tes button displays reason when probe fails', async () => {
    ui.rarExePath = 'C:\\Bad\\Rar.exe';
    archiveMocks.archiveRarToolProbe.mockResolvedValue({
      ok: false,
      reason: 'File tidak ditemukan di path yang ditentukan',
    });
    const { getByTestId } = render(ArchiveSettingsSection);
    await tick();

    const probeBtn = getByTestId('rar-probe-btn');
    await fireEvent.click(probeBtn);
    await tick();

    expect(archiveRarToolProbeMockTimes(archiveMocks.archiveRarToolProbe)).toBe(1);
    const status = getByTestId('rar-probe-status');
    expect(status.textContent).toContain('File tidak ditemukan');
  });

  it('Tes saves the path being edited (flush, no debounce) before probing it', async () => {
    archiveMocks.archiveRarToolProbe.mockResolvedValue({ ok: true, version: 'RAR 7.00 x64' });
    const { getByTestId } = render(ArchiveSettingsSection);
    await tick();

    const input = getByTestId('rar-exe-path-input') as HTMLInputElement;
    await fireEvent.input(input, { target: { value: 'D:\\Tools\\WinRAR\\Rar.exe' } });
    await tick();
    await fireEvent.click(getByTestId('rar-probe-btn'));
    await tick();
    await tick();

    expect(archiveMocks.order).toEqual([
      'flush:D:\\Tools\\WinRAR\\Rar.exe',
      'probe:D:\\Tools\\WinRAR\\Rar.exe',
    ]);
  });

  it('Tes does not probe when saving the prefs fails', async () => {
    ui.rarExePath = 'C:\\Program Files\\WinRAR\\Rar.exe';
    archiveMocks.flush.mockResolvedValue(false);
    const { getByTestId } = render(ArchiveSettingsSection);
    await tick();

    await fireEvent.click(getByTestId('rar-probe-btn'));
    await tick();
    await tick();

    expect(archiveMocks.archiveRarToolProbe).not.toHaveBeenCalled();
    expect(getByTestId('rar-probe-status').textContent).toContain('Gagal menyimpan');
  });
});

function archiveRarToolProbeMockTimes(mock: ReturnType<typeof vi.fn>): number {
  return mock.mock.calls.length;
}
