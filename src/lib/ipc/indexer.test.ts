import { afterEach, describe, expect, it, vi } from 'vitest';

const invoke = vi.hoisted(() => vi.fn());
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import {
  asStatus,
  indexerFolderSizes,
  indexerSetMode,
  indexerStatus,
  IndexerError,
  readiness,
  squattedNote,
  UNAVAILABLE,
} from './indexer';

function inTauri(on: boolean) {
  const w = window as unknown as Record<string, unknown>;
  if (on) w.__TAURI_INTERNALS__ = {};
  else delete w.__TAURI_INTERNALS__;
}

afterEach(() => {
  inTauri(false);
  invoke.mockReset();
});

describe('indexer IPC (FER-474)', () => {
  it('parses a full status and drops anything malformed', () => {
    expect(
      asStatus({
        installed: true,
        scmState: 'running',
        reachable: true,
        version: '0.1.0',
        mode: 'high_performance',
        state: 'running',
        index: 'none',
        canLowerPrivacy: false,
      }),
    ).toEqual({
      installed: true,
      scmState: 'running',
      reachable: true,
      version: '0.1.0',
      mode: 'high_performance',
      state: 'running',
      index: 'none',
      canLowerPrivacy: false,
      problem: null,
      squatterPid: null,
      indexPaused: false,
      degraded: false,
      volumes: [],
    });
    const bad = asStatus({ installed: 'yes', mode: 'turbo', version: 'x'.repeat(500) });
    expect(bad.installed).toBe(false);
    expect(bad.mode).toBeNull();
    expect(bad.version).toBeNull();
    expect(asStatus(null).problem).toBe('ipc');
  });

  it('FER-480: parses the squatter PID strictly and words the squatted note', () => {
    const base = { installed: true, reachable: false, problem: 'squatted' };
    expect(asStatus({ ...base, squatterPid: 4242 }).squatterPid).toBe(4242);
    for (const bad of [0, -1, 1.5, '42', 2 ** 32, null]) {
      expect(asStatus({ ...base, squatterPid: bad }).squatterPid).toBeNull();
    }
    expect(squattedNote(asStatus({ ...base, squatterPid: 4242 }))).toBe(
      'Nama pipe FerrumIndexer dipakai proses lain (PID 4242) — indeks tidak dipakai, scan biasa aktif.',
    );
    expect(squattedNote(asStatus(base))).toBe(
      'Nama pipe FerrumIndexer dipakai proses lain — indeks tidak dipakai, scan biasa aktif.',
    );
    expect(squattedNote(asStatus({ ...base, problem: 'untrusted', squatterPid: 4242 }))).toBeNull();
  });

  it('FER-476: parses volumes strictly and summarises readiness', () => {
    const s = asStatus({
      installed: true,
      reachable: true,
      index: 'building',
      indexPaused: true,
      volumes: [
        { letter: 'C', state: 'ready', entries: 12.7, progressPermille: 1000, live: true },
        {
          letter: 'D',
          state: 'error',
          error: 'access_denied',
          entries: -1,
          progressPermille: 5000,
        },
        { letter: 'cc', state: 'ready' },
        { letter: 'E', state: 'weird' },
        null,
      ],
    });
    expect(s.indexPaused).toBe(true);
    expect(s.volumes).toEqual([
      { letter: 'C', state: 'ready', error: null, entries: 12, progressPermille: 1000, live: true },
      {
        letter: 'D',
        state: 'error',
        error: 'access_denied',
        entries: 0,
        progressPermille: 1000,
        live: false,
      },
    ]);
    expect(readiness(s)).toBe('ready');
    expect(readiness({ ...s, volumes: [], index: 'building' })).toBe('building');
    expect(readiness({ ...s, reachable: false })).toBe('stopped');
    expect(readiness({ ...s, installed: false })).toBe('not_installed');
    expect(readiness({ ...s, volumes: [], index: 'error' })).toBe('error');
    // FER-476 review H1: degraded wins over the index state.
    expect(readiness({ ...s, degraded: true })).toBe('degraded');
    expect(asStatus({ installed: true, reachable: true, degraded: true }).degraded).toBe(true);
  });

  it('is unavailable outside the desktop app and never calls the backend', async () => {
    expect(await indexerStatus()).toEqual(UNAVAILABLE);
    await expect(indexerSetMode('shared', true)).rejects.toMatchObject({ code: 'unsupported' });
    expect(invoke).not.toHaveBeenCalled();
  });

  it('degrades a failing status call to "not available"', async () => {
    inTauri(true);
    invoke.mockRejectedValue(new Error('Unknown command'));
    const s = await indexerStatus();
    expect(s.installed).toBe(false);
    expect(s.problem).toBe('ipc');
  });

  it('passes the confirmation flag and maps backend errors to codes', async () => {
    inTauri(true);
    invoke.mockResolvedValueOnce({ mode: 'shared', changed: true, elevated: false });
    expect(await indexerSetMode('shared', true)).toEqual({
      mode: 'shared',
      changed: true,
      elevated: false,
    });
    expect(invoke).toHaveBeenCalledWith('indexer_set_mode', { mode: 'shared', confirmed: true });

    invoke.mockRejectedValueOnce({
      code: 'declined',
      message: 'administrator permission was declined',
    });
    const err = await indexerSetMode('high_performance', true).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(IndexerError);
    expect((err as IndexerError).code).toBe('declined');

    invoke.mockResolvedValueOnce({ mode: 'bogus' });
    await expect(indexerSetMode('shared', true)).rejects.toMatchObject({ code: 'failed' });
  });
});

describe('folder sizes IPC (FER-477a)', () => {
  it('passes parent and names, keeps only finite non-negative sizes', async () => {
    inTauri(true);
    invoke.mockResolvedValue({ available: true, sizes: [5, null, -1, 'x'], stale: true });
    const r = await indexerFolderSizes('C:\\Data', ['a', 'b', 'c', 'd']);
    expect(invoke).toHaveBeenCalledWith('indexer_folder_sizes', {
      parent: 'C:\\Data',
      names: ['a', 'b', 'c', 'd'],
    });
    expect(r).toEqual({ available: true, reason: null, sizes: [5, null, null, null], stale: true });
  });

  it('a reply of the wrong length, an IPC error or no desktop app is "not available"', async () => {
    expect((await indexerFolderSizes('C:\\', ['a'])).reason).toBe('unavailable');
    inTauri(true);
    invoke.mockResolvedValue({ available: true, sizes: [1, 2] });
    expect(await indexerFolderSizes('C:\\', ['a'])).toEqual({
      available: false,
      reason: 'protocol',
      sizes: [null],
      stale: false,
    });
    invoke.mockRejectedValue(new Error('boom'));
    expect((await indexerFolderSizes('C:\\', ['a'])).reason).toBe('ipc');
    invoke.mockResolvedValue({ available: false, reason: 'shared_mode', sizes: [null] });
    expect((await indexerFolderSizes('C:\\', ['a'])).reason).toBe('shared_mode');
  });
});
