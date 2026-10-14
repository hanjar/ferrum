import { beforeEach, describe, expect, it, vi } from 'vitest';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const invokeMock = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invokeMock(...a) }));

const { syncLeaseAcquire, syncLeaseRelease, SyncLeaseError } = await import('./syncLease');
const { acquireSyncLease, syncLeaseBusyMessage } = await import('../syncService');

const here = dirname(fileURLToPath(import.meta.url));
const fixtureDir = join(here, '../../../contract-fixtures/automation');
const fixture = (name: string): unknown =>
  JSON.parse(readFileSync(join(fixtureDir, name), 'utf-8'));

function fakeTauri() {
  (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {};
}
function unTauri() {
  delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__;
}

beforeEach(() => {
  invokeMock.mockReset().mockName('invoke');
  unTauri();
});

describe('sync lease IPC wrappers', () => {
  it('sends exactly the request fixtures that the Rust command tests are held to', async () => {
    fakeTauri();
    const acquire = fixture('sync-lease-acquire.request.json') as { left: string; right: string };
    invokeMock.mockResolvedValueOnce(fixture('sync-lease-acquire.json'));
    await syncLeaseAcquire(acquire.left, acquire.right);
    // Flat top-level arguments, never wrapped in { req: ... }.
    expect(invokeMock).toHaveBeenLastCalledWith('sync_lease_acquire', acquire);

    const release = fixture('sync-lease-release.request.json') as { leaseId: string };
    invokeMock.mockResolvedValueOnce(fixture('sync-lease-release.json'));
    await syncLeaseRelease(release.leaseId);
    expect(invokeMock).toHaveBeenLastCalledWith('sync_lease_release', release);

    for (const call of invokeMock.mock.calls) {
      expect(Object.keys(call[1] ?? {})).not.toContain('req');
    }
  });

  it('maps the response fixtures: a lease id, or busy with the holder kind', async () => {
    fakeTauri();
    invokeMock.mockResolvedValueOnce(fixture('sync-lease-acquire.json'));
    expect(await syncLeaseAcquire('D:\\a', 'E:\\b')).toEqual(fixture('sync-lease-acquire.json'));
    invokeMock.mockResolvedValueOnce(fixture('sync-lease-acquire-busy.json'));
    expect(await syncLeaseAcquire('D:\\a', 'E:\\b')).toEqual({
      status: 'busy',
      holder: { kind: 'runner', runId: 42 },
    });
  });

  it('an unknown holder kind or a nonsense answer never becomes a lease', async () => {
    fakeTauri();
    invokeMock.mockResolvedValueOnce({ status: 'busy', holder: { kind: 'martian' } });
    expect(await syncLeaseAcquire('D:\\a', 'E:\\b')).toEqual({
      status: 'busy',
      holder: { kind: 'unknown' },
    });
    for (const bad of [null, {}, { leaseId: '' }, { status: 'ok' }, 'lease']) {
      invokeMock.mockResolvedValueOnce(bad);
      await expect(syncLeaseAcquire('D:\\a', 'E:\\b')).rejects.toBeInstanceOf(SyncLeaseError);
    }
  });

  it('backend errors keep their closed-set code and never need a path', async () => {
    fakeTauri();
    invokeMock.mockRejectedValueOnce({ code: 'store_unavailable', message: 'Store is down' });
    await expect(syncLeaseAcquire('D:\\a', 'E:\\b')).rejects.toMatchObject({
      code: 'store_unavailable',
      message: 'Store is down',
    });
    invokeMock.mockRejectedValueOnce('weird');
    await expect(syncLeaseRelease('x')).rejects.toMatchObject({ code: 'unknown' });
  });

  it('outside the desktop app a stand-in lease is returned and nothing is invoked', async () => {
    expect(await syncLeaseAcquire('D:\\a', 'E:\\b')).toEqual({ leaseId: 'local' });
    await syncLeaseRelease('local');
    expect(invokeMock).not.toHaveBeenCalled();
  });
});

describe('acquireSyncLease', () => {
  it('returns a lease whose release is idempotent and never throws', async () => {
    const api = {
      acquire: vi.fn(async () => ({ leaseId: 'abc' })),
      release: vi.fn(async () => {
        throw new Error('store hiccup');
      }),
    };
    const out = await acquireSyncLease('D:\\a', 'E:\\b', api);
    expect(out.status).toBe('acquired');
    if (out.status !== 'acquired') return;
    await out.lease.release();
    await out.lease.release();
    expect(api.release).toHaveBeenCalledTimes(1);
    expect(api.release).toHaveBeenCalledWith('abc');
  });

  it('distinguishes busy (by holder kind) from failure', async () => {
    const busy = (kind: 'manual' | 'app' | 'runner' | 'unknown') => ({
      acquire: vi.fn(async () => ({ status: 'busy' as const, holder: { kind } })),
      release: vi.fn(),
    });
    for (const kind of ['runner', 'app'] as const) {
      const out = await acquireSyncLease('D:\\a', 'E:\\b', busy(kind));
      expect(out).toMatchObject({ status: 'busy', message: expect.stringContaining('automation') });
    }
    expect(syncLeaseBusyMessage({ kind: 'manual' })).toContain('another synchronization');
    expect(syncLeaseBusyMessage()).toContain('another sync or automation');

    const failed = await acquireSyncLease('D:\\a', 'E:\\b', {
      acquire: vi.fn(async () => {
        throw new SyncLeaseError('store_read_only', 'Ferrum was updated - restart Ferrum');
      }),
      release: vi.fn(),
    });
    expect(failed).toMatchObject({ status: 'failed' });
    if (failed.status === 'failed') {
      expect(failed.message).toContain('restart Ferrum');
      expect(failed.message).toContain('Nothing was changed');
    }
  });
});
