/**
 * FER-472 — the "Scan lebih cepat?" offer before a Sync scan. compareDirs is mocked at the IPC
 * boundary; the chooser is injected so no dialog or elevation is involved.
 */
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { compareStore } from './compareStore.svelte';
import { chooseSyncScanMode } from './syncFlow.svelte';
import { dialogs } from './dialogStore.svelte';
import { registerPane, type PaneApi } from './panesRegistry.svelte';
import { ui } from '../uiState.svelte';
import type { CompareDirsReport } from '../ipc/compare';

vi.mock('../ipc/compare', () => ({
  validateSyncRoots: vi.fn(async () => {}),
  compareDirs: (...args: unknown[]) => compareDirsMock(...args),
  compareFiles: vi.fn(),
  cancelCompare: vi.fn(),
}));
vi.mock('../ipc/diskUsage', () => ({
  diskUsageFastEligibility: (root: string) => eligibilityMock(root),
  diskUsageCacheLoad: vi.fn(async () => null),
  diskUsageCacheDelete: vi.fn(async () => 0),
  diskUsageCacheList: vi.fn(async () => []),
  diskUsageCacheNoteMutation: vi.fn(async () => undefined),
}));

const compareDirsMock = vi.fn();
const eligibilityMock = vi.fn();

function report(extra: Partial<CompareDirsReport> = {}): CompareDirsReport {
  return { leftRoot: 'C:\\L', rightRoot: 'D:\\R', entries: [], truncated: false, ...extra };
}

function fakePane(path: string): PaneApi {
  return {
    path,
    selectedPaths: () => [],
    cursorEntry: () => undefined,
    refresh: () => {},
    newTab: () => {},
    closeCurrentTab: () => {},
    goUp: () => {},
    openCursorEntry: () => {},
  };
}

beforeEach(() => {
  compareDirsMock.mockReset();
  eligibilityMock.mockReset();
  compareStore.reset();
  compareStore.syncScanChooser = async () => 'normal';
  dialogs.close();
  ui.diskUsageFastOffer = true;
  registerPane(0, fakePane('C:\\L'));
  registerPane(1, fakePane('D:\\R'));
});

describe('sync scan mode', () => {
  it('default (Enter) = ordinary scan: no fastScan flag is sent', async () => {
    compareDirsMock.mockResolvedValue(report());
    await compareStore.openSync();
    const opts = compareDirsMock.mock.calls[0][3] as Record<string, unknown>;
    expect(opts).not.toHaveProperty('fastScan');
  });

  it('choosing the fast scan sends fastScan for the preview compare', async () => {
    const ask = vi.fn(async () => 'fast' as const);
    compareStore.syncScanChooser = ask;
    compareDirsMock.mockResolvedValue(
      report({ fastScan: { left: true, right: false, note: 'x' } }),
    );
    await compareStore.openSync();
    expect(ask).toHaveBeenCalledWith('C:\\L', 'D:\\R');
    expect(compareDirsMock.mock.calls[0][3]).toMatchObject({ recursive: true, fastScan: true });
    expect(compareStore.syncReport?.fastScan?.left).toBe(true);
  });

  it('refresh and the content toggle reuse the choice instead of asking again', async () => {
    const ask = vi.fn(async () => 'fast' as const);
    compareStore.syncScanChooser = ask;
    compareDirsMock.mockResolvedValue(report());
    await compareStore.openSync();
    await compareStore.openSync();
    await compareStore.setSyncByContent(true);
    expect(ask).toHaveBeenCalledTimes(1);
    expect(compareDirsMock).toHaveBeenCalledTimes(3);
    for (const c of compareDirsMock.mock.calls) {
      expect(c[3]).toMatchObject({ fastScan: true });
    }
  });

  it('a different folder pair asks again while the panel stays open (L4)', async () => {
    const ask = vi.fn<(l: string, r: string) => Promise<'fast' | 'normal'>>(async () => 'fast');
    compareStore.syncScanChooser = ask;
    compareDirsMock.mockResolvedValue(report());
    await compareStore.openSync({ left: 'C:\\A', right: 'C:\\B' });
    await compareStore.openSync(); // same pair: reuse
    expect(ask).toHaveBeenCalledTimes(1);
    ask.mockResolvedValueOnce('normal');
    await compareStore.openSync({ left: 'C:\\X', right: 'C:\\Y' });
    expect(ask).toHaveBeenCalledTimes(2);
    expect(ask).toHaveBeenLastCalledWith('C:\\X', 'C:\\Y');
    expect(compareDirsMock.mock.calls[2][3]).not.toHaveProperty('fastScan');
  });

  it('a dismissed offer starts nothing and leaves the panel closed', async () => {
    compareStore.syncScanChooser = async () => null;
    await compareStore.openSync();
    expect(compareDirsMock).not.toHaveBeenCalled();
    expect(compareStore.syncOpen).toBe(false);
    expect(compareStore.syncBusy).toBe(false);
  });

  it('a failing chooser falls back to the ordinary scan', async () => {
    compareStore.syncScanChooser = async () => {
      throw new Error('boom');
    };
    compareDirsMock.mockResolvedValue(report());
    await compareStore.openSync();
    expect(compareDirsMock).toHaveBeenCalledTimes(1);
    expect(compareDirsMock.mock.calls[0][3]).not.toHaveProperty('fastScan');
  });
});

describe('chooseSyncScanMode', () => {
  it('no offer (and no probe) when the preference is off', async () => {
    ui.diskUsageFastOffer = false;
    expect(await chooseSyncScanMode('C:\\L', 'D:\\R')).toBe('normal');
    expect(eligibilityMock).not.toHaveBeenCalled();
  });

  it('no offer when neither folder is on a local NTFS drive', async () => {
    eligibilityMock.mockResolvedValue({ eligible: false });
    expect(await chooseSyncScanMode('\\\\srv\\a', '\\\\srv\\b')).toBe('normal');
    expect(dialogs.current).toBeNull();
  });

  it('offers once for the pair; Enter-equivalent choice is the ordinary scan', async () => {
    eligibilityMock.mockImplementation(async (root: string) =>
      root.startsWith('C') ? { eligible: true, volume: 'C' } : { eligible: false },
    );
    const pending = chooseSyncScanMode('C:\\L', 'D:\\R');
    await vi.waitFor(() => expect(dialogs.current?.kind).toBe('fastScanOffer'));
    const d = dialogs.current;
    if (d?.kind !== 'fastScanOffer') throw new Error('offer expected');
    expect(d).toMatchObject({ purpose: 'sync', root: 'C:\\L', rightRoot: 'D:\\R', volume: 'C' });
    d.choose('normal', false);
    expect(await pending).toBe('normal');
    expect(dialogs.current).toBeNull();
  });

  it('"do not ask again" turns the shared offer preference off', async () => {
    eligibilityMock.mockResolvedValue({ eligible: true, volume: 'C' });
    const pending = chooseSyncScanMode('C:\\L', 'C:\\R');
    await vi.waitFor(() => expect(dialogs.current?.kind).toBe('fastScanOffer'));
    const d = dialogs.current;
    if (d?.kind !== 'fastScanOffer') throw new Error('offer expected');
    d.choose('fast', true);
    expect(await pending).toBe('fast');
    expect(ui.diskUsageFastOffer).toBe(false);
  });

  it('closing the dialog without a choice = dismissed', async () => {
    eligibilityMock.mockResolvedValue({ eligible: true, volume: 'C' });
    const pending = chooseSyncScanMode('C:\\L', 'C:\\R');
    await vi.waitFor(() => expect(dialogs.current?.kind).toBe('fastScanOffer'));
    dialogs.close();
    expect(await pending).toBeNull();
  });
});
