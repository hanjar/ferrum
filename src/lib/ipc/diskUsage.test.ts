/** Disk usage backend wrapper tests (DSK-04 foundation) — mock Tauri like
 *  `backends.test.ts`; the store layer is tested separately with its own
 *  event harness. */
import { beforeEach, describe, expect, it, vi } from 'vitest';

const invokeMock = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invokeMock(...a) }));
// eslint-disable-next-line @typescript-eslint/no-unused-vars -- pass-through mock, args forwarded verbatim
const listenMock = vi.fn(async (..._a: unknown[]) => () => {});
vi.mock('@tauri-apps/api/event', () => ({
  listen: (...a: unknown[]) => listenMock(...a),
}));

const {
  diskUsageStart,
  diskUsageFastEligibility,
  diskUsageCancel,
  diskUsageSnapshot,
  diskUsageQueryChildren,
  diskUsageTopFiles,
  onDiskUsageEvent,
} = await import('./diskUsage');
const { FsError } = await import('./types');

function fakeTauri() {
  (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {};
}
function unTauri() {
  delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__;
}

beforeEach(() => {
  invokeMock.mockReset().mockName('invoke');
  listenMock.mockClear();
  unTauri();
});

describe('disk usage backend wrapper', () => {
  it('rejects start/queries outside Tauri; cancel and snapshot stay silent', async () => {
    await expect(diskUsageStart('C:\\t', {})).rejects.toMatchObject({ code: 'unsupported' });
    await expect(diskUsageQueryChildren('s', 'C:\\t', 0, 50)).rejects.toMatchObject({
      code: 'unsupported',
    });
    await expect(diskUsageTopFiles('s', 'C:\\t', 20)).rejects.toMatchObject({
      code: 'unsupported',
    });
    await expect(diskUsageCancel('s')).resolves.toBe(false);
    await expect(diskUsageSnapshot('s')).resolves.toBeNull();
  });

  it('invokes disk_usage_start and returns the backend scanId', async () => {
    fakeTauri();
    invokeMock.mockResolvedValueOnce('scan-1');
    await expect(diskUsageStart('C:\\t', { maxDepth: null })).resolves.toBe('scan-1');
    expect(invokeMock).toHaveBeenCalledWith('disk_usage_start', {
      root: 'C:\\t',
      options: { maxDepth: null },
      fast: false,
    });
    invokeMock.mockRejectedValueOnce({ code: 'notFound', message: 'missing root' });
    await expect(diskUsageStart('C:\\gone', {})).rejects.toBeInstanceOf(FsError);
  });

  it('FER-460: fast flag is forwarded; eligibility degrades to not-eligible', async () => {
    await expect(diskUsageFastEligibility('C:/')).resolves.toMatchObject({ eligible: false });
    expect(invokeMock).not.toHaveBeenCalled();
    fakeTauri();
    invokeMock.mockResolvedValueOnce('scan-2');
    await diskUsageStart('C:/', { maxDepth: null }, true);
    expect(invokeMock).toHaveBeenLastCalledWith('disk_usage_start', {
      root: 'C:/',
      options: { maxDepth: null },
      fast: true,
    });
    invokeMock.mockResolvedValueOnce({ eligible: true, volume: 'C' });
    await expect(diskUsageFastEligibility('C:/')).resolves.toEqual({ eligible: true, volume: 'C' });
    expect(invokeMock).toHaveBeenLastCalledWith('disk_usage_fast_eligibility', { root: 'C:/' });
    invokeMock.mockRejectedValueOnce(new Error('boom'));
    await expect(diskUsageFastEligibility('C:/')).resolves.toEqual({ eligible: false });
  });

  it('cancel maps raw rejections to false and snapshot maps failures to null', async () => {
    fakeTauri();
    invokeMock.mockResolvedValueOnce(true);
    await expect(diskUsageCancel('scan-1')).resolves.toBe(true);
    expect(invokeMock).toHaveBeenCalledWith('disk_usage_cancel', { scanId: 'scan-1' });
    invokeMock.mockRejectedValueOnce('raw');
    await expect(diskUsageCancel('scan-1')).resolves.toBe(false);
    invokeMock.mockRejectedValueOnce('raw');
    await expect(diskUsageSnapshot('scan-1')).resolves.toBeNull();
  });

  it('query_children forwards offset/limit/sort/filter and maps errors', async () => {
    fakeTauri();
    const page = { scanId: 's', nodeId: 'n', items: [], total: 0, offset: 0, limit: 50 };
    invokeMock.mockResolvedValueOnce(page);
    await expect(
      diskUsageQueryChildren('s', 'n', 0, 50, 'largestFirst', { metric: 'allocated' }),
    ).resolves.toEqual(page);
    expect(invokeMock).toHaveBeenCalledWith('disk_usage_query_children', {
      scanId: 's',
      nodeId: 'n',
      offset: 0,
      limit: 50,
      sort: 'largestFirst',
      filter: { metric: 'allocated' },
    });
    invokeMock.mockRejectedValueOnce({ code: 'notFound', message: 'evicted' });
    await expect(diskUsageQueryChildren('s', 'n', 0, 50)).rejects.toMatchObject({
      code: 'notFound',
    });
  });

  it('top_files forwards limit/filter; onDiskUsageEvent subscribes the stream', async () => {
    fakeTauri();
    const files = { scanId: 's', nodeId: 'n', items: [], label: 'Top 20 files', totalMatching: 0 };
    invokeMock.mockResolvedValueOnce(files);
    await expect(diskUsageTopFiles('s', 'n', 20, { minBytes: 10 })).resolves.toEqual(files);
    expect(invokeMock).toHaveBeenCalledWith('disk_usage_top_files', {
      scanId: 's',
      nodeId: 'n',
      limit: 20,
      filter: { minBytes: 10 },
    });
    const cb = vi.fn();
    await onDiskUsageEvent(cb);
    expect(listenMock).toHaveBeenCalledWith('disk_usage://progress', expect.any(Function));
  });
});
