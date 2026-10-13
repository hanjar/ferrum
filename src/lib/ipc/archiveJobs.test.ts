import { beforeEach, describe, expect, it, vi } from 'vitest';

// FER-79 / FER-73 / FER-75: the IPC bridge for cancellable archive jobs and the structured
// error branching. invoke / listen are mocked; the contract under test is what goes over the wire.
const tauri = vi.hoisted(() => ({
  invoke: vi.fn(),
  listen: vi.fn(),
  inTauri: true,
}));
vi.mock('@tauri-apps/api/core', () => ({ invoke: tauri.invoke }));
vi.mock('@tauri-apps/api/event', () => ({ listen: tauri.listen }));
vi.mock('./tauri', () => ({ isInTauri: () => tauri.inTauri }));

const {
  archiveCreate,
  archiveExtract,
  archiveTest,
  archiveCancelRequest,
  archiveMultipartCheck,
  listenArchiveProgress,
  newArchiveRequestId,
  ARCHIVE_PROGRESS_EVENT,
} = await import('./archive');
const { isCancelledError, isMultipartError, isPasswordError } = await import('./archiveErrors');
const { FsError } = await import('./types');

beforeEach(() => {
  tauri.invoke.mockReset();
  tauri.listen.mockReset();
  tauri.inTauri = true;
});

describe('request ids', () => {
  it('are unique and non-empty', () => {
    const ids = new Set(Array.from({ length: 50 }, () => newArchiveRequestId()));
    expect(ids.size).toBe(50);
    for (const id of ids) expect(id.length).toBeGreaterThan(8);
  });
});

describe('extract, test and create send their request id', () => {
  it('archive_extract carries requestId next to the request', async () => {
    tauri.invoke.mockResolvedValue(undefined);
    await archiveExtract('C:\\a.zip', ['x'], 'D:\\out', 'skip', 'pw', 'req-9');
    const [cmd, args] = tauri.invoke.mock.calls[0]!;
    expect(cmd).toBe('archive_extract');
    expect(args.requestId).toBe('req-9');
    expect(args.request).toMatchObject({ archivePath: 'C:\\a.zip', password: 'pw' });
  });

  it('archive_create carries requestId next to the request', async () => {
    tauri.invoke.mockResolvedValue('C:\\out.zip');
    await archiveCreate(['C:\\a.txt'], 'C:\\out.zip', 'zip', 'overwrite', 'req-create-1');
    const [cmd, args] = tauri.invoke.mock.calls[0]!;
    expect(cmd).toBe('archive_create');
    expect(args.requestId).toBe('req-create-1');
    expect(args.request).toEqual({
      format: 'zip',
      sourcePaths: ['C:\\a.txt'],
      destinationPath: 'C:\\out.zip',
      conflict: 'overwrite',
    });
  });

  it('archive_test carries requestId and the password only inside the request', async () => {
    tauri.invoke.mockResolvedValue({ archivePath: 'C:\\a.7z', entries: [] });
    await archiveTest('C:\\a.7z', 'pw', 'req-3');
    const [cmd, args] = tauri.invoke.mock.calls[0]!;
    expect(cmd).toBe('archive_test');
    expect(args.requestId).toBe('req-3');
    expect(args.request).toEqual({ archivePath: 'C:\\a.7z', password: 'pw' });
  });

  it('without a request id the job is simply not cancellable (null on the wire)', async () => {
    tauri.invoke.mockResolvedValue(undefined);
    await archiveExtract('C:\\a.zip', [], 'D:\\out', 'skip');
    expect(tauri.invoke.mock.calls[0]![1].requestId).toBeNull();
  });
});

describe('cancel and progress', () => {
  it('archiveCancelRequest calls archive_cancel_request with the id, and never throws', async () => {
    tauri.invoke.mockRejectedValue(new Error('gone'));
    await expect(archiveCancelRequest('req-1')).resolves.toBeUndefined();
    expect(tauri.invoke).toHaveBeenCalledWith('archive_cancel_request', { requestId: 'req-1' });
  });

  it('listenArchiveProgress subscribes to archive://progress and unwraps the payload', async () => {
    const unlisten = vi.fn();
    tauri.listen.mockImplementation(
      async (_name: string, cb: (e: { payload: unknown }) => void) => {
        cb({
          payload: { requestId: 'r', processedBytes: 1, processedEntries: 0, kind: 'extract' },
        });
        return unlisten;
      },
    );
    const seen: unknown[] = [];
    const off = await listenArchiveProgress((e) => seen.push(e));
    expect(tauri.listen.mock.calls[0]![0]).toBe(ARCHIVE_PROGRESS_EVENT);
    expect(ARCHIVE_PROGRESS_EVENT).toBe('archive://progress');
    expect(seen).toEqual([
      { requestId: 'r', processedBytes: 1, processedEntries: 0, kind: 'extract' },
    ]);
    off();
    expect(unlisten).toHaveBeenCalled();
  });

  it('outside Tauri there is nothing to cancel or listen to', async () => {
    tauri.inTauri = false;
    await archiveCancelRequest('x');
    const off = await listenArchiveProgress(() => {});
    off();
    expect(tauri.invoke).not.toHaveBeenCalled();
    expect(tauri.listen).not.toHaveBeenCalled();
  });
});

describe('multipart check', () => {
  it('asks the backend about the file next to its siblings', async () => {
    tauri.invoke.mockResolvedValue({ kind: 'missing', missing: ['a.part2.rar'] });
    const info = await archiveMultipartCheck('C:\\a.part1.rar');
    expect(tauri.invoke).toHaveBeenCalledWith('archive_multipart_check', {
      path: 'C:\\a.part1.rar',
    });
    expect(info).toEqual({ kind: 'missing', missing: ['a.part2.rar'] });
  });
});

describe('structured archive errors', () => {
  it('FsError.fromIPC keeps archiveCode', () => {
    const e = FsError.fromIPC({
      code: 'io',
      message: 'm',
      path: 'C:\\a.zip',
      archiveCode: 'badPassword',
    });
    expect(e.archiveCode).toBe('badPassword');
    expect(FsError.fromIPC({ code: 'io', message: 'm' }).archiveCode).toBeUndefined();
  });

  it('classifies by code, never by message', () => {
    const mk = (archiveCode: string | undefined, message = 'x', code = 'io') =>
      new FsError({ code: code as 'io', message, archiveCode: archiveCode as undefined });
    expect(isPasswordError(mk('passwordRequired'))).toBe(true);
    expect(isPasswordError(mk('badPassword'))).toBe(true);
    expect(isPasswordError(mk('corruptArchive', 'wrong password'))).toBe(false);
    expect(isPasswordError(mk(undefined, 'Password required'))).toBe(false);
    expect(isPasswordError('password')).toBe(false);
    expect(isCancelledError(mk('cancelled', 'x', 'cancelled'))).toBe(true);
    expect(isCancelledError(mk(undefined, 'x', 'cancelled'))).toBe(true);
    expect(isCancelledError(mk('io'))).toBe(false);
    expect(isMultipartError(mk('multipartNotFirst'))).toBe(true);
    expect(isMultipartError(mk('multipartMissing'))).toBe(true);
    expect(isMultipartError(mk('io'))).toBe(false);
  });
});
