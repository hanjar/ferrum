import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render } from '@testing-library/svelte';
import { tick } from 'svelte';
import ArchiveSettingsSection from './ArchiveSettingsSection.svelte';
import { FsError } from '../ipc/types';
import { ui } from '../uiState.svelte';

const archiveMocks = vi.hoisted(() => ({
  archiveRarToolDetect: vi.fn(),
  archiveRarToolProbe: vi.fn(),
  archiveRarToolStatus: vi.fn(),
  archiveRarToolSetPath: vi.fn(),
}));

vi.mock('../ipc/archive', () => ({
  archiveRarToolDetect: () => archiveMocks.archiveRarToolDetect(),
  archiveRarToolProbe: () => archiveMocks.archiveRarToolProbe(),
  archiveRarToolStatus: () => archiveMocks.archiveRarToolStatus(),
  archiveRarToolSetPath: (path: string | null) => archiveMocks.archiveRarToolSetPath(path),
}));

// FER-511: the section must have NO prefs-save path at all. Both are spied so a regression
// that starts saving the path through save_prefs/flush fails these tests.
const prefsMocks = vi.hoisted(() => ({
  savePrefs: vi.fn(),
  flush: vi.fn(),
}));

vi.mock('../ipc/prefs', () => ({
  savePrefs: () => prefsMocks.savePrefs(),
}));

vi.mock('../stores/prefsStore.svelte', () => ({
  prefs: {
    flush: () => prefsMocks.flush(),
  },
}));

const READY = { path: 'C:\\Program Files\\WinRAR\\Rar.exe', state: 'ready' };

function statusState(state: string): { path: string | null; state: string } {
  return { path: state === 'unset' ? null : 'C:\\Program Files\\WinRAR\\Rar.exe', state };
}

beforeEach(() => {
  for (const mock of Object.values(archiveMocks)) mock.mockReset();
  for (const mock of Object.values(prefsMocks)) mock.mockReset();
  archiveMocks.archiveRarToolStatus.mockResolvedValue({ path: null, state: 'unset' });
  ui.rarExePath = null;
});

afterEach(() => {
  ui.rarExePath = null;
});

describe('FER-505/511: ArchiveSettingsSection component', () => {
  it('renders section and disclaimer text', () => {
    const { getByText } = render(ArchiveSettingsSection);
    expect(getByText(/Ferrum tidak menyertakan WinRAR/i)).toBeTruthy();
  });

  it('Cari otomatis fills the draft input only — nothing is stored', async () => {
    archiveMocks.archiveRarToolDetect.mockResolvedValue('C:\\Program Files\\WinRAR\\Rar.exe');
    const { getByTestId } = render(ArchiveSettingsSection);
    await vi.waitFor(() => expect(archiveMocks.archiveRarToolStatus).toHaveBeenCalled());

    await fireEvent.click(getByTestId('rar-detect-btn'));
    await tick();

    expect(archiveMocks.archiveRarToolDetect).toHaveBeenCalledTimes(1);
    const input = getByTestId('rar-exe-path-input') as HTMLInputElement;
    expect(input.value).toBe('C:\\Program Files\\WinRAR\\Rar.exe');
    expect(archiveMocks.archiveRarToolSetPath).not.toHaveBeenCalled();
    expect(prefsMocks.savePrefs).not.toHaveBeenCalled();
    expect(prefsMocks.flush).not.toHaveBeenCalled();
  });

  it('Cari otomatis shows not found message if detection returns null', async () => {
    archiveMocks.archiveRarToolDetect.mockResolvedValue(null);
    const { getByTestId } = render(ArchiveSettingsSection);
    await vi.waitFor(() => expect(archiveMocks.archiveRarToolStatus).toHaveBeenCalled());

    await fireEvent.click(getByTestId('rar-detect-btn'));
    await tick();

    const status = getByTestId('rar-probe-status');
    expect(status.textContent).toContain('tidak ditemukan di lokasi standar');
  });

  it('"Pakai path ini" calls archive_rar_tool_set_path — never save_prefs', async () => {
    archiveMocks.archiveRarToolStatus.mockResolvedValue(READY);
    archiveMocks.archiveRarToolSetPath.mockResolvedValue({ saved: true });
    const { getByTestId } = render(ArchiveSettingsSection);
    await vi.waitFor(() => expect(archiveMocks.archiveRarToolStatus).toHaveBeenCalled());

    const input = getByTestId('rar-exe-path-input') as HTMLInputElement;
    await fireEvent.input(input, { target: { value: 'D:\\Tools\\WinRAR\\Rar.exe' } });
    await fireEvent.click(getByTestId('rar-apply-btn'));
    await tick();
    await tick();

    expect(archiveMocks.archiveRarToolSetPath).toHaveBeenCalledTimes(1);
    expect(archiveMocks.archiveRarToolSetPath).toHaveBeenCalledWith('D:\\Tools\\WinRAR\\Rar.exe');
    // FER-511: the path never travels through save_prefs or the prefs store flush.
    expect(prefsMocks.savePrefs).not.toHaveBeenCalled();
    expect(prefsMocks.flush).not.toHaveBeenCalled();
    expect(getByTestId('rar-probe-status').textContent).toContain('Path tersimpan');
  });

  it('cancelling the native dialog shows that nothing was stored', async () => {
    archiveMocks.archiveRarToolSetPath.mockRejectedValue(
      new FsError({
        code: 'cancelled',
        message: 'Konfirmasi WinRAR dibatalkan; path tidak disimpan',
      }),
    );
    const { getByTestId } = render(ArchiveSettingsSection);
    await vi.waitFor(() => expect(archiveMocks.archiveRarToolStatus).toHaveBeenCalled());

    const input = getByTestId('rar-exe-path-input') as HTMLInputElement;
    await fireEvent.input(input, { target: { value: 'D:\\Tools\\WinRAR\\Rar.exe' } });
    await fireEvent.click(getByTestId('rar-apply-btn'));
    await vi.waitFor(() =>
      expect(getByTestId('rar-probe-status').textContent).toContain('tidak disimpan'),
    );

    expect(archiveMocks.archiveRarToolSetPath).toHaveBeenCalledTimes(1);
    expect(ui.rarExePath).toBeNull();
  });

  it('shows the re-confirmation-required status for unconfirmed legacy paths', async () => {
    archiveMocks.archiveRarToolStatus.mockResolvedValue(statusState('unconfirmed'));
    const { getByTestId } = render(ArchiveSettingsSection);
    await vi.waitFor(() =>
      expect(getByTestId('rar-status').textContent).toContain('BELUM dikonfirmasi'),
    );

    // The stored-but-unconfirmed path may not be probed either (the backend would refuse).
    expect((getByTestId('rar-probe-btn') as HTMLButtonElement).disabled).toBe(true);
  });

  it('shows the changed-exe warning when the fingerprint no longer matches', async () => {
    archiveMocks.archiveRarToolStatus.mockResolvedValue(statusState('changed'));
    const { getByTestId } = render(ArchiveSettingsSection);
    await vi.waitFor(() =>
      expect(getByTestId('rar-status').textContent).toContain('berubah sejak dikonfirmasi'),
    );
  });

  it('Tes probes the SAVED confirmed path (no prefs flush first) and shows the version', async () => {
    archiveMocks.archiveRarToolStatus.mockResolvedValue(READY);
    ui.rarExePath = 'C:\\Program Files\\WinRAR\\Rar.exe';
    archiveMocks.archiveRarToolProbe.mockResolvedValue({ ok: true, version: 'RAR 7.00 x64' });
    const { getByTestId } = render(ArchiveSettingsSection);
    await vi.waitFor(() => expect(archiveMocks.archiveRarToolStatus).toHaveBeenCalled());

    // Editing the draft must not touch the probe: it runs the stored exe only.
    const input = getByTestId('rar-exe-path-input') as HTMLInputElement;
    await fireEvent.input(input, { target: { value: 'D:\\Elsewhere\\Rar.exe' } });
    await fireEvent.click(getByTestId('rar-probe-btn'));
    await tick();
    await tick();

    expect(archiveMocks.archiveRarToolProbe).toHaveBeenCalledTimes(1);
    expect(prefsMocks.flush).not.toHaveBeenCalled();
    expect(getByTestId('rar-probe-status').textContent).toContain('RAR 7.00 x64');
  });

  it('Tes shows the backend reason when the probe fails', async () => {
    archiveMocks.archiveRarToolStatus.mockResolvedValue(READY);
    archiveMocks.archiveRarToolProbe.mockResolvedValue({
      ok: false,
      reason: 'WinRAR berubah sejak dikonfirmasi; atur ulang di Settings',
    });
    const { getByTestId } = render(ArchiveSettingsSection);
    await vi.waitFor(() => expect(archiveMocks.archiveRarToolStatus).toHaveBeenCalled());

    await fireEvent.click(getByTestId('rar-probe-btn'));
    await tick();

    expect(getByTestId('rar-probe-status').textContent).toContain('berubah sejak dikonfirmasi');
  });

  it('Hapus clears the stored setting through set_path(null) without any dialog', async () => {
    archiveMocks.archiveRarToolStatus.mockResolvedValue(READY);
    ui.rarExePath = 'C:\\Program Files\\WinRAR\\Rar.exe';
    archiveMocks.archiveRarToolSetPath.mockResolvedValue({ saved: true });
    const { getByTestId } = render(ArchiveSettingsSection);
    await vi.waitFor(() => expect(archiveMocks.archiveRarToolStatus).toHaveBeenCalled());

    await fireEvent.click(getByTestId('rar-clear-btn'));
    await tick();
    await tick();

    expect(archiveMocks.archiveRarToolSetPath).toHaveBeenCalledWith(null);
    expect(ui.rarExePath).toBeNull();
  });

  it('FER-514: the stored path prefills the draft and is shown in the status line', async () => {
    archiveMocks.archiveRarToolStatus.mockResolvedValue(READY);
    const { getByTestId } = render(ArchiveSettingsSection);
    await vi.waitFor(() =>
      expect((getByTestId('rar-exe-path-input') as HTMLInputElement).value).toBe(READY.path),
    );
    expect(getByTestId('rar-status-path').textContent).toBe(READY.path);
    expect(getByTestId('rar-status').textContent).toContain('siap digunakan');
  });

  it('FER-514: a stored path does not overwrite what the user is typing', async () => {
    let release: (v: { path: string; state: string }) => void = () => {};
    archiveMocks.archiveRarToolStatus.mockReturnValue(
      new Promise((resolve) => {
        release = resolve;
      }),
    );
    const { getByTestId } = render(ArchiveSettingsSection);
    const input = getByTestId('rar-exe-path-input') as HTMLInputElement;
    await fireEvent.input(input, { target: { value: 'D:\\typed\\Rar.exe' } });
    release(READY);
    await tick();
    await tick();
    expect(input.value).toBe('D:\\typed\\Rar.exe');
  });
});
