/**
 * FER-578: a checksum verification / archive-member hash can run in the background. The dialog
 * closes, the dock lists the job, the result comes back through "Lihat hasil".
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const verifyMock = vi.fn();
const memberMock = vi.fn();
const cancelMock = vi.fn();
let progressHandler: ((p: Record<string, unknown>) => void) | null = null;
vi.mock('../ipc/fileTools', async (importOriginal) => {
  const original = await importOriginal<typeof import('../ipc/fileTools')>();
  return {
    ...original,
    verifyChecksums: (...a: unknown[]) => verifyMock(...a),
    checksumArchiveMember: (...a: unknown[]) => memberMock(...a),
    cancelChecksumRequest: (...a: unknown[]) => cancelMock(...a),
    onChecksumProgress: async (h: (p: Record<string, unknown>) => void) => {
      progressHandler = h;
      return () => {};
    },
  };
});
vi.mock('../ipc/archive', () => ({
  archiveCancelRequest: vi.fn(),
  listenArchiveProgress: async () => () => {},
}));

const { fileTools } = await import('./fileToolsStore.svelte');
const { jobs } = await import('./jobsStore.svelte');
const { toast } = await import('./toastStore.svelte');
const { FsError } = await import('../ipc/types');

interface Deferred<T> {
  promise: Promise<T>;
  resolve: (v: T) => void;
  reject: (e: unknown) => void;
}
function deferred<T>(): Deferred<T> {
  let resolve!: (v: T) => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}
const flush = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

beforeEach(() => {
  jobs.reset();
  verifyMock.mockReset();
  memberMock.mockReset();
  cancelMock.mockReset();
  cancelMock.mockResolvedValue(undefined);
  progressHandler = null;
  toast.show('', 0);
});

afterEach(() => {
  fileTools.current = null;
  jobs.reset();
});

const report = (over: Record<string, unknown> = {}) => ({
  requested: 2,
  ok: 2,
  mismatched: 0,
  outcomes: [
    { path: 'C:\\d\\a.bin', status: 'ok' },
    { path: 'C:\\d\\b.bin', status: 'ok' },
  ],
  ...over,
});

describe('verify in the background', () => {
  it('the dialog closes, the job is docked, progress flows, and the result is restored on demand', async () => {
    const run = deferred<unknown>();
    verifyMock.mockReturnValue(run.promise);
    fileTools.open({ kind: 'checksumVerify', paths: ['C:\\d\\a.bin', 'C:\\d\\b.bin'] });
    await flush();
    expect(fileTools.verifying).toBe(true);
    expect(fileTools.canBackground).toBe(true);

    const id = verifyMock.mock.calls[0]![1] as string;
    progressHandler?.({
      requestId: id,
      processedBytes: 10,
      filesDone: 1,
      filesTotal: 2,
      current: 'C:\\d\\a.bin',
    });

    fileTools.sendToBackground();
    expect(fileTools.current).toBeNull();
    expect(jobs.docked).toHaveLength(1);
    const job = jobs.docked[0]!;
    expect(job).toMatchObject({
      kind: 'verify',
      state: 'running',
      processedEntries: 1,
      totalEntries: 2,
    });
    expect(toast.message).toContain('latar belakang');

    progressHandler?.({
      requestId: id,
      processedBytes: 20,
      filesDone: 2,
      filesTotal: 2,
      current: 'C:\\d\\b.bin',
    });
    expect(job.processedBytes).toBe(20);

    run.resolve(report());
    await flush();
    expect(job.state).toBe('done');
    expect(job.summary).toBe('Verify: 2 OK, 0 mismatched, 0 other');
    expect(fileTools.current).toBeNull(); // nothing pops up

    jobs.openResult(job);
    expect(fileTools.current).toMatchObject({ kind: 'checksumVerify' });
    expect(fileTools.verify?.ok).toBe(2);
    expect(fileTools.verifying).toBe(false);
  });

  it('a mismatch makes the background job "failed" and the result still shows the report', async () => {
    const run = deferred<unknown>();
    verifyMock.mockReturnValue(run.promise);
    fileTools.open({ kind: 'checksumVerify', paths: ['C:\\d\\a.bin'] });
    await flush();
    fileTools.sendToBackground();
    run.resolve(
      report({
        ok: 0,
        mismatched: 1,
        outcomes: [{ path: 'C:\\d\\a.bin', status: 'mismatch' }],
      }),
    );
    await flush();
    const job = jobs.docked[0]!;
    expect(job.state).toBe('failed');
    jobs.openResult(job);
    expect(fileTools.verify?.mismatched).toBe(1);
  });

  it('cancelling from the dock calls the checksum cancel with the same request id', async () => {
    const run = deferred<unknown>();
    verifyMock.mockReturnValue(run.promise);
    fileTools.open({ kind: 'checksumVerify', paths: ['C:\\d\\a.bin'] });
    await flush();
    const id = verifyMock.mock.calls[0]![1] as string;
    fileTools.sendToBackground();
    const job = jobs.docked[0]!;
    jobs.cancel(job);
    expect(cancelMock).toHaveBeenCalledWith(id);
    expect(job.state).toBe('cancelling');
    run.resolve(
      report({ cancelled: true, ok: 1, outcomes: [{ path: 'C:\\d\\a.bin', status: 'ok' }] }),
    );
    await flush();
    expect(job.state).toBe('canceled');
  });

  it('a failure while docked is kept as the result, not shown in front of the user', async () => {
    const run = deferred<unknown>();
    verifyMock.mockReturnValue(run.promise);
    fileTools.open({ kind: 'checksumVerify', paths: ['C:\\d\\a.bin'] });
    await flush();
    fileTools.sendToBackground();
    run.reject(new FsError({ code: 'io', message: 'read error' }));
    await flush();
    const job = jobs.docked[0]!;
    expect(job.state).toBe('failed');
    expect(job.summary).toContain('read error');
    expect(fileTools.current).toBeNull();
    jobs.openResult(job);
    expect(fileTools.error).toContain('read error');
  });

  it('a second verification is refused while one runs in the background', async () => {
    const run = deferred<unknown>();
    verifyMock.mockReturnValue(run.promise);
    fileTools.open({ kind: 'checksumVerify', paths: ['C:\\d\\a.bin'] });
    await flush();
    fileTools.sendToBackground();
    fileTools.open({ kind: 'checksumVerify', paths: ['C:\\d\\other.bin'] });
    expect(fileTools.current).toBeNull();
    expect(verifyMock).toHaveBeenCalledTimes(1);
    expect(toast.message).toContain('masih berjalan');
    // another kind of file tool still opens and can be closed
    fileTools.open({ kind: 'split', path: 'C:\\d\\big.iso' });
    expect(fileTools.current).toMatchObject({ kind: 'split' });
    fileTools.close();
    expect(fileTools.current).toBeNull();
    run.resolve(report());
    await flush();
  });

  it('there is nothing to background when nothing runs', () => {
    fileTools.open({ kind: 'split', path: 'C:\\d\\big.iso' });
    expect(fileTools.canBackground).toBe(false);
    fileTools.sendToBackground();
    expect(jobs.list).toHaveLength(0);
    expect(fileTools.current).toMatchObject({ kind: 'split' });
  });
});

describe('archive member hash in the background', () => {
  it('keeps hashing; the hash shows in the summary and in the restored dialog', async () => {
    const run = deferred<unknown>();
    memberMock.mockReturnValue(run.promise);
    fileTools.open({ kind: 'archiveChecksum', archivePath: 'C:\\a.zip', memberPath: 'big/x.bin' });
    void fileTools.runArchiveHash();
    await flush();
    expect(fileTools.hashing).toBe(true);
    fileTools.sendToBackground();
    const job = jobs.docked[0]!;
    expect(job.subject).toBe('C:\\a.zip\\big/x.bin');
    run.resolve({
      archivePath: 'C:\\a.zip',
      memberPath: 'big/x.bin',
      algorithm: 'sha256',
      hash: 'abc123',
    });
    await flush();
    expect(job.state).toBe('done');
    expect(job.summary).toBe('SHA256: abc123');
    jobs.openResult(job);
    expect(fileTools.current).toMatchObject({ kind: 'archiveChecksum' });
    expect(fileTools.memberHash?.hash).toBe('abc123');
  });
});
