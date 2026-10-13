import { beforeEach, describe, expect, it, vi } from 'vitest';

const archiveMocks = vi.hoisted(() => ({
  archiveCreate: vi.fn(),
  archiveCancelRequest: vi.fn(),
  newArchiveRequestId: vi.fn(() => 'req-pack-auto'),
  listenArchiveProgress: vi.fn(),
}));
vi.mock('./ipc/archive', () => archiveMocks);

const { defaultPackTarget, runPack, cancelPack } = await import('./packService');

beforeEach(() => {
  archiveMocks.archiveCreate.mockReset();
  archiveMocks.archiveCancelRequest.mockReset();
  archiveMocks.listenArchiveProgress.mockReset();
});

describe('defaultPackTarget', () => {
  it('appends the format extension to the pane path', () => {
    expect(defaultPackTarget('C:\\Data\\Invoice', 'zip')).toBe('C:\\Data\\Invoice.zip');
    expect(defaultPackTarget('C:\\Data\\Invoice', 'sevenz')).toBe('C:\\Data\\Invoice.7z');
  });

  it('does not double an extension that is already there', () => {
    expect(defaultPackTarget('C:\\Data\\x.zip', 'zip')).toBe('C:\\Data\\x.zip');
    expect(defaultPackTarget('C:\\Data\\x.7z', 'sevenz')).toBe('C:\\Data\\x.7z');
  });
});

describe('runPack', () => {
  it('delegates to the existing archive IPC forwarding paths, target, format, and a unique requestId', async () => {
    archiveMocks.archiveCreate.mockResolvedValue('C:\\o.zip');
    const result = await runPack({ paths: ['C:\\a', 'C:\\b'], target: 'C:\\o.zip', format: 'zip' });
    expect(archiveMocks.archiveCreate).toHaveBeenCalledTimes(1);
    expect(archiveMocks.archiveCreate).toHaveBeenCalledWith(
      ['C:\\a', 'C:\\b'],
      'C:\\o.zip',
      'zip',
      undefined,
      'req-pack-auto',
    );
    expect(result).toEqual({ outputPath: 'C:\\o.zip', itemCount: 2 });
  });

  it('preserves an explicit requestId passed by the caller', async () => {
    archiveMocks.archiveCreate.mockResolvedValue('C:\\o.zip');
    await runPack({
      paths: ['C:\\a'],
      target: 'C:\\o.zip',
      format: 'zip',
      requestId: 'custom-req-id',
    });
    expect(archiveMocks.archiveCreate).toHaveBeenCalledWith(
      ['C:\\a'],
      'C:\\o.zip',
      'zip',
      undefined,
      'custom-req-id',
    );
  });

  it('passes the path array as a copy (later caller mutation cannot change the job)', async () => {
    archiveMocks.archiveCreate.mockResolvedValue('C:\\o.zip');
    const paths = ['C:\\a'];
    await runPack({ paths, target: 'C:\\o.zip', format: 'zip' });
    expect(archiveMocks.archiveCreate.mock.calls[0]![0]).not.toBe(paths);
  });

  it('forwards an explicit conflict policy and keeps the requestId', async () => {
    archiveMocks.archiveCreate.mockResolvedValue('C:\\o (2).zip');
    const result = await runPack({
      paths: ['C:\\a'],
      target: 'C:\\o.zip',
      format: 'zip',
      conflict: 'rename',
    });
    expect(archiveMocks.archiveCreate).toHaveBeenLastCalledWith(
      ['C:\\a'],
      'C:\\o.zip',
      'zip',
      'rename',
      'req-pack-auto',
    );
    expect(result.outputPath).toBe('C:\\o (2).zip'); // the path actually written

    archiveMocks.archiveCreate.mockClear();
    await runPack({ paths: ['C:\\a'], target: 'C:\\o.zip', format: 'zip' });
    expect(archiveMocks.archiveCreate.mock.calls[0]).toEqual([
      ['C:\\a'],
      'C:\\o.zip',
      'zip',
      undefined,
      'req-pack-auto',
    ]);
  });

  it('subscribes to progress and forwards matching events to onProgress', async () => {
    archiveMocks.archiveCreate.mockResolvedValue('C:\\o.zip');
    const unlisten = vi.fn();
    let handler: ((e: unknown) => void) | undefined;
    archiveMocks.listenArchiveProgress.mockImplementation(async (cb: (e: unknown) => void) => {
      handler = cb;
      return unlisten;
    });

    const progressEvents: unknown[] = [];
    const packPromise = runPack({
      paths: ['C:\\a'],
      target: 'C:\\o.zip',
      format: 'zip',
      requestId: 'my-req',
      onProgress: (ev) => progressEvents.push(ev),
    });

    await vi.waitFor(() => expect(handler).toBeDefined());
    handler!({ requestId: 'my-req', processedBytes: 100, kind: 'create' });
    handler!({ requestId: 'other-req', processedBytes: 200, kind: 'create' });

    await packPromise;
    expect(progressEvents).toEqual([{ requestId: 'my-req', processedBytes: 100, kind: 'create' }]);
    expect(unlisten).toHaveBeenCalled();
  });

  it('cancelPack delegates to archiveCancelRequest', async () => {
    await cancelPack('req-pack-cancel');
    expect(archiveMocks.archiveCancelRequest).toHaveBeenCalledWith('req-pack-cancel');
  });

  it('rejects with the IPC error and reports no result on failure', async () => {
    archiveMocks.archiveCreate.mockImplementation(() => Promise.reject(new Error('disk is full')));
    const outcome = await runPack({ paths: ['C:\\a'], target: 'C:\\o.zip', format: 'zip' }).then(
      () => 'resolved',
      (e: unknown) => String(e),
    );
    expect(outcome).toContain('disk is full');
  });

  it('refuses an empty path list without calling the engine', async () => {
    const outcome = await runPack({ paths: [], target: 'C:\\o.zip', format: 'zip' }).then(
      () => 'resolved',
      (e: unknown) => String(e),
    );
    expect(outcome).toMatch(/nothing/i);
    expect(archiveMocks.archiveCreate).not.toHaveBeenCalled();
  });
});
