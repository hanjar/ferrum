import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import type { IndexerStatus } from '../ipc/indexer';

const api = vi.hoisted(() => ({
  indexerStatus: vi.fn(),
  indexerSetMode: vi.fn(),
}));
vi.mock('../ipc/indexer', async (orig) => {
  const real = await orig<typeof import('../ipc/indexer')>();
  return { ...real, indexerStatus: api.indexerStatus, indexerSetMode: api.indexerSetMode };
});

import IndexerSettingsSection from './IndexerSettingsSection.svelte';
import { ui } from '../uiState.svelte';
import { IndexerError } from '../ipc/indexer';

const running = (extra: Partial<IndexerStatus> = {}): IndexerStatus => ({
  installed: true,
  scmState: 'running',
  reachable: true,
  version: '0.1.0',
  mode: 'shared',
  state: 'running',
  index: 'none',
  canLowerPrivacy: false,
  problem: null,
  squatterPid: null,
  indexPaused: false,
  degraded: false,
  volumes: [],
  ...extra,
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe('Settings › Indexer (FER-474)', () => {
  it('FER-477: fast sync with the journal is opt-in, off by default, with its risk explained', async () => {
    api.indexerStatus.mockResolvedValue(running());
    ui.syncJournalSkip = false;
    render(IndexerSettingsSection);
    const box = screen.getByRole('checkbox', { name: /Sync cepat dengan journal indexer/ });
    expect((box as HTMLInputElement).checked).toBe(false);
    expect(screen.getByTestId('indexer-sync-journal-skip-risk').textContent).toContain('7 hari');
    await fireEvent.click(box);
    expect(ui.syncJournalSkip).toBe(true);
    await fireEvent.click(box);
    expect(ui.syncJournalSkip).toBe(false);
  });

  it('FER-476: shows the index state honestly (building with progress, ready, stopped)', async () => {
    api.indexerStatus.mockResolvedValue(
      running({
        index: 'building',
        volumes: [
          {
            letter: 'C',
            state: 'ready',
            error: null,
            entries: 10,
            progressPermille: 1000,
            live: true,
          },
          {
            letter: 'D',
            state: 'building',
            error: null,
            entries: 0,
            progressPermille: 425,
            live: false,
          },
        ],
      }),
    );
    const r = render(IndexerSettingsSection);
    expect((await screen.findByTestId('indexer-index')).textContent).toMatch(
      /Sedang membangun indeks \(D: 42%\)/,
    );
    r.unmount();
    api.indexerStatus.mockResolvedValue(
      running({
        index: 'ready',
        volumes: [
          {
            letter: 'C',
            state: 'ready',
            error: null,
            entries: 10,
            progressPermille: 1000,
            live: true,
          },
        ],
      }),
    );
    const r2 = render(IndexerSettingsSection);
    expect((await screen.findByTestId('indexer-index')).textContent).toMatch(/Siap/);
    r2.unmount();
    api.indexerStatus.mockResolvedValue(running({ index: 'ready', degraded: true }));
    const r3 = render(IndexerSettingsSection);
    expect((await screen.findByTestId('indexer-index')).textContent).toMatch(
      /degraded.*scan biasa/s,
    );
    r3.unmount();
    api.indexerStatus.mockResolvedValue(running({ reachable: false, scmState: 'stopped' }));
    render(IndexerSettingsSection);
    expect((await screen.findByTestId('indexer-index')).textContent).toMatch(/Service mati/);
  });

  it('shows the fallback when the service is not installed', async () => {
    api.indexerStatus.mockResolvedValue(
      running({ installed: false, reachable: false, scmState: null, mode: null, version: null }),
    );
    render(IndexerSettingsSection);
    expect(await screen.findByText(/belum terpasang/)).toBeTruthy();
    expect(screen.getByTestId('indexer-status').textContent).toMatch(/Tidak terpasang/);
    expect(screen.queryByRole('button', { name: /Ganti ke/ })).toBeNull();
  });

  it('explains a skipped install outside Program Files', async () => {
    api.indexerStatus.mockResolvedValue(
      running({
        installed: false,
        reachable: false,
        scmState: null,
        mode: null,
        version: null,
        problem: 'location',
      }),
    );
    render(IndexerSettingsSection);
    expect(await screen.findByText(/di luar Program Files/)).toBeTruthy();
  });

  it('shows installed-but-stopped without offering a mode change', async () => {
    api.indexerStatus.mockResolvedValue(
      running({ scmState: 'stopped', reachable: false, mode: null }),
    );
    render(IndexerSettingsSection);
    expect(await screen.findByText(/tidak menjawab/)).toBeTruthy();
    expect(screen.getByTestId('indexer-status').textContent).toMatch(/Berhenti/);
    expect(screen.queryByRole('button', { name: /Ganti ke/ })).toBeNull();
  });

  it('FER-480: says the pipe name is held by another process (PID), not just "untrusted"', async () => {
    api.indexerStatus.mockResolvedValue(
      running({ reachable: false, mode: null, problem: 'squatted', squatterPid: 4242 }),
    );
    render(IndexerSettingsSection);
    expect(
      await screen.findByText(
        'Nama pipe FerrumIndexer dipakai proses lain (PID 4242) — indeks tidak dipakai, scan biasa aktif.',
      ),
    ).toBeTruthy();
    expect(screen.queryByText(/Tidak tepercaya/)).toBeNull();
  });

  it('shows status, version and mode of a running service', async () => {
    api.indexerStatus.mockResolvedValue(running({ mode: 'high_performance' }));
    render(IndexerSettingsSection);
    await waitFor(() =>
      expect(screen.getByTestId('indexer-mode').textContent).toMatch(/High performance/),
    );
    expect(screen.getByTestId('indexer-status').textContent).toMatch(/Berjalan/);
    expect(screen.getByTestId('indexer-status').textContent).toMatch(/0\.1\.0/);
    expect(screen.getByRole('button', { name: 'Ganti ke Bersama…' })).toBeTruthy();
  });

  it('lowering privacy needs the explicit acknowledgement in the dialog', async () => {
    api.indexerStatus.mockResolvedValue(running());
    api.indexerSetMode.mockResolvedValue({
      mode: 'high_performance',
      changed: true,
      elevated: true,
    });
    render(IndexerSettingsSection);
    await fireEvent.click(
      await screen.findByRole('button', { name: 'Ganti ke High performance…' }),
    );
    const dlg = screen.getByRole('alertdialog');
    expect(dlg.textContent).toMatch(/profil akun\s+pengguna lain/);
    expect(dlg.textContent).toMatch(/UAC/);
    const go = screen.getByRole('button', { name: 'Ganti mode' }) as HTMLButtonElement;
    expect(go.disabled).toBe(true);
    await fireEvent.click(go);
    expect(api.indexerSetMode).not.toHaveBeenCalled();
    await fireEvent.click(screen.getByRole('checkbox', { name: /Saya mengerti/ }));
    expect(go.disabled).toBe(false);
    await fireEvent.click(go);
    await waitFor(() =>
      expect(api.indexerSetMode).toHaveBeenCalledExactlyOnceWith('high_performance', true),
    );
    expect((await screen.findByTestId('indexer-message')).textContent).toMatch(/Mode diganti/);
  });

  it('cancel and Escape close the dialog without changing anything', async () => {
    api.indexerStatus.mockResolvedValue(running({ mode: 'high_performance' }));
    render(IndexerSettingsSection);
    await fireEvent.click(await screen.findByRole('button', { name: 'Ganti ke Bersama…' }));
    expect(screen.getByRole('alertdialog').textContent).toMatch(/difilter sesuai hak akses/);
    await fireEvent.click(screen.getByRole('button', { name: 'Batal' }));
    expect(screen.queryByRole('alertdialog')).toBeNull();
    await fireEvent.click(screen.getByRole('button', { name: 'Ganti ke Bersama…' }));
    const dlg = screen.getByRole('alertdialog');
    const parentKey = vi.fn();
    dlg.parentElement!.addEventListener('keydown', parentKey);
    await fireEvent.keyDown(dlg, { key: 'Escape' });
    expect(screen.queryByRole('alertdialog')).toBeNull();
    expect(parentKey).not.toHaveBeenCalled();
    expect(api.indexerSetMode).not.toHaveBeenCalled();
  });

  it('a declined UAC prompt is reported and the mode stays', async () => {
    api.indexerStatus.mockResolvedValue(running());
    api.indexerSetMode.mockRejectedValue(new IndexerError('declined'));
    render(IndexerSettingsSection);
    await fireEvent.click(
      await screen.findByRole('button', { name: 'Ganti ke High performance…' }),
    );
    await fireEvent.click(screen.getByRole('checkbox', { name: /Saya mengerti/ }));
    await fireEvent.click(screen.getByRole('button', { name: 'Ganti mode' }));
    expect(await screen.findByText(/Izin administrator ditolak/)).toBeTruthy();
    expect(screen.getByTestId('indexer-mode').textContent).toMatch(/Bersama/);
  });
});
