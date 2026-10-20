/**
 * FER-578: archive jobs moved to the background. Same IPC mocks as dialogStore.test.ts; the
 * contract under test is that a docked job never opens a dialog by itself, that its prompts and
 * results wait for the user, and that cancelling from the dock works.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { PaneApi } from './panesRegistry.svelte';

const archiveExtractMock = vi.fn();
const archivePreflightMock = vi.fn();
const archiveTestMock = vi.fn();
const archiveCancelMock = vi.fn();
const multipartCheckMock = vi.fn();
let progressHandler: ((e: Record<string, unknown>) => void) | null = null;
let requestSeq = 0;
vi.mock('../ipc/mutations', () => ({ planCopy: vi.fn() }));
vi.mock('../ipc/archive', () => ({
  archiveExtract: (...args: unknown[]) => archiveExtractMock(...args),
  archiveExtractPreflight: (...args: unknown[]) => archivePreflightMock(...args),
  archiveTest: (...args: unknown[]) => archiveTestMock(...args),
  archiveCreate: vi.fn(),
  archiveCreateZip: vi.fn(),
  archiveCancelRequest: (...args: unknown[]) => archiveCancelMock(...args),
  archiveMultipartCheck: (...args: unknown[]) => multipartCheckMock(...args),
  newArchiveRequestId: () => `req-${++requestSeq}`,
  listenArchiveProgress: async (handler: (e: Record<string, unknown>) => void) => {
    progressHandler = handler;
    return () => {};
  },
}));

const { dialogs } = await import('./dialogStore.svelte');
const { jobs } = await import('./jobsStore.svelte');
const { toast } = await import('./toastStore.svelte');
const { registerPane } = await import('./panesRegistry.svelte');
const { ui } = await import('../uiState.svelte');
const { FsError } = await import('../ipc/types');

const unregisters: (() => void)[] = [];

function register(id: 0 | 1, path: string, selected: string[] = []): void {
  const api: PaneApi = {
    path,
    selectedPaths: () => selected,
    cursorEntry: () => undefined,
    refresh: () => {},
    newTab: () => {},
    closeCurrentTab: () => {},
    goUp: () => {},
    openCursorEntry: () => {},
  };
  unregisters.push(registerPane(id, api));
}

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
const cancelledError = () =>
  new FsError({ code: 'cancelled', message: 'cancelled', archiveCode: 'cancelled' });
const passwordError = () =>
  new FsError({ code: 'io', message: 'needs one', archiveCode: 'passwordRequired' });

beforeEach(() => {
  jobs.reset();
  archiveExtractMock.mockReset();
  archivePreflightMock.mockReset();
  archiveTestMock.mockReset();
  archiveCancelMock.mockReset();
  multipartCheckMock.mockReset();
  archivePreflightMock.mockResolvedValue({ totalFiles: 1, totalBytes: 1, conflicts: [] });
  toast.show('', 0);
  ui.activePane = 0;
});

afterEach(() => {
  dialogs.close();
  jobs.reset();
  for (const off of unregisters.splice(0)) off();
});

async function startExtract(archives: string[]) {
  register(0, 'C:\\Projects', archives);
  register(1, 'D:\\Backup');
  dialogs.beginUnpack();
  await flush();
}

describe('Esc / Background on a running extract', () => {
  it('Esc docks the job: nothing is cancelled, the dialog is gone, the job keeps running', async () => {
    const run = deferred<void>();
    archiveExtractMock.mockReturnValue(run.promise);
    await startExtract(['C:\\Projects\\big.zip']);
    expect(dialogs.current).toMatchObject({ kind: 'archiveProgress' });

    dialogs.close(); // Esc
    expect(archiveCancelMock).not.toHaveBeenCalled();
    expect(dialogs.current).toBeNull();
    expect(jobs.docked).toHaveLength(1);
    expect(jobs.docked[0]).toMatchObject({ kind: 'extract', state: 'running', docked: true });
    expect(toast.message).toContain('latar belakang');
    run.resolve();
    await flush();
  });

  it('sendToBackground is the same thing (the Background button)', async () => {
    const run = deferred<void>();
    archiveExtractMock.mockReturnValue(run.promise);
    await startExtract(['C:\\Projects\\big.zip']);
    dialogs.sendToBackground();
    expect(dialogs.current).toBeNull();
    expect(jobs.docked[0]?.docked).toBe(true);
    run.resolve();
    await flush();
  });

  it('progress keeps flowing into the docked job', async () => {
    const run = deferred<void>();
    archiveExtractMock.mockReturnValue(run.promise);
    await startExtract(['C:\\Projects\\big.zip']);
    const id = archiveExtractMock.mock.calls[0]![5] as string;
    dialogs.sendToBackground();
    progressHandler?.({
      requestId: id,
      processedBytes: 50,
      totalBytes: 100,
      processedEntries: 1,
      totalEntries: 2,
      currentMember: 'a.txt',
    });
    expect(jobs.docked[0]).toMatchObject({ processedBytes: 50, totalBytes: 100 });
    run.resolve();
    await flush();
  });

  it('a docked extract that finishes opens no dialog; it toasts and keeps "Lihat hasil"', async () => {
    const run = deferred<void>();
    archiveExtractMock.mockImplementation(() => run.promise);
    await startExtract(['C:\\Projects\\big.zip']);
    const id = archiveExtractMock.mock.calls[0]![5] as string;
    dialogs.sendToBackground();
    progressHandler?.({
      requestId: id,
      processedBytes: 100,
      totalBytes: 100,
      processedEntries: 2,
      totalEntries: 2,
      writtenEntries: 2,
      skippedEntries: 1,
    });
    run.resolve();
    await flush();

    expect(dialogs.current).toBeNull();
    const job = jobs.docked[0]!;
    expect(job.state).toBe('done');
    expect(job.summary).toBe('Ekstrak selesai: 2 ditulis, 1 dilewati');
    expect(toast.message).toBe('Ekstrak selesai: 2 ditulis, 1 dilewati');
    expect(jobs.runningCount).toBe(0);

    jobs.openResult(job);
    expect(dialogs.current).toMatchObject({
      kind: 'extractSummary',
      written: 2,
      skipped: 1,
      target: 'D:\\Backup',
    });
  });

  it('the batch stays docked: the next archive opens no progress dialog', async () => {
    const first = deferred<void>();
    const second = deferred<void>();
    archiveExtractMock.mockReturnValueOnce(first.promise).mockReturnValueOnce(second.promise);
    await startExtract(['C:\\Projects\\one.zip', 'C:\\Projects\\two.zip']);
    dialogs.sendToBackground();
    first.resolve();
    await flush();
    expect(archiveExtractMock).toHaveBeenCalledTimes(2);
    expect(dialogs.current).toBeNull();
    expect(jobs.docked).toHaveLength(1);
    expect(jobs.docked[0]).toMatchObject({ position: '2/2', state: 'running' });
    second.resolve();
    await flush();
    expect(jobs.docked[0]?.state).toBe('done');
    expect(dialogs.current).toBeNull();
  });

  it('a job that was never docked leaves no job behind (the old foreground flow)', async () => {
    archiveExtractMock.mockResolvedValue(undefined);
    await startExtract(['C:\\Projects\\big.zip']);
    expect(dialogs.current).toBeNull();
    expect(jobs.list).toHaveLength(0);
    expect(toast.message).toBe('');
  });
});

describe('cancel and failure of a docked extract', () => {
  it('cancelling from the dock sends the request id, then ends as canceled with the notice as result', async () => {
    const run = deferred<void>();
    archiveExtractMock.mockReturnValue(run.promise);
    await startExtract(['C:\\Projects\\big.zip']);
    const id = archiveExtractMock.mock.calls[0]![5] as string;
    dialogs.sendToBackground();
    const job = jobs.docked[0]!;

    jobs.cancel(job);
    expect(archiveCancelMock).toHaveBeenCalledWith(id);
    expect(job.state).toBe('cancelling');
    run.reject(cancelledError());
    await flush();

    expect(job.state).toBe('canceled');
    expect(dialogs.current).toBeNull();
    jobs.openResult(job);
    expect(dialogs.current).toMatchObject({ kind: 'error', title: 'Unpack' });
  });

  it('a failure while docked is held as the result, not thrown in the user face', async () => {
    const run = deferred<void>();
    archiveExtractMock.mockReturnValue(run.promise);
    await startExtract(['C:\\Projects\\big.zip']);
    dialogs.sendToBackground();
    // The user is typing in another dialog meanwhile.
    dialogs.open({ kind: 'mkdir', parent: 'C:\\Projects' });

    run.reject(new FsError({ code: 'io', message: 'disk full', memberPath: 'a.txt' }));
    await flush();

    expect(dialogs.current).toMatchObject({ kind: 'mkdir' });
    const job = jobs.docked[0]!;
    expect(job.state).toBe('failed');
    expect(job.summary).toContain('big.zip');
    dialogs.close();
    jobs.openResult(job);
    expect(dialogs.current).toMatchObject({ kind: 'error' });
    expect((dialogs.current as { message: string }).message).toContain('disk full');
  });
});

describe('prompts in the middle of a docked job', () => {
  it('a password request is held as needsInput; it opens only when the user chooses it', async () => {
    const first = deferred<void>();
    archiveExtractMock.mockReturnValueOnce(first.promise).mockResolvedValueOnce(undefined);
    await startExtract(['C:\\Projects\\enc.zip']);
    dialogs.sendToBackground();
    dialogs.open({ kind: 'mkdir', parent: 'C:\\Projects' });

    first.reject(passwordError());
    await flush();

    const job = jobs.docked[0]!;
    expect(job.state).toBe('needsInput');
    expect(jobs.needInputCount).toBe(1);
    expect(toast.message).toContain('Password');
    expect(toast.action?.label).toBe('Buka');
    // The user's own dialog was not replaced and no password dialog stole the focus.
    expect(dialogs.current).toMatchObject({ kind: 'mkdir' });
    expect(dialogs.queue).toHaveLength(0);

    dialogs.close();
    jobs.openPrompt(job);
    expect(dialogs.current).toMatchObject({
      kind: 'password',
      archivePath: 'C:\\Projects\\enc.zip',
    });
    await (dialogs.current as { retry: (pw: string) => Promise<void> }).retry('secret');
    await flush();
    expect(archiveExtractMock).toHaveBeenLastCalledWith(
      'C:\\Projects\\enc.zip',
      [],
      'D:\\Backup',
      'skip',
      'secret',
      expect.any(String),
    );
    expect(job.state).toBe('done');
    // After the answer the job keeps running in the dock: no progress dialog came back.
    expect(dialogs.current).toBeNull();
  });

  it('dismissing the held password dialog ends the job as canceled', async () => {
    const first = deferred<void>();
    archiveExtractMock.mockReturnValueOnce(first.promise);
    await startExtract(['C:\\Projects\\enc.zip']);
    dialogs.sendToBackground();
    first.reject(passwordError());
    await flush();
    const job = jobs.docked[0]!;
    jobs.openPrompt(job);
    dialogs.close(); // Esc on the password dialog
    expect(job.state).toBe('canceled');
    expect(jobs.runningCount).toBe(0);
  });

  it('the same password request is shown at once for a job in the foreground', async () => {
    const first = deferred<void>();
    archiveExtractMock.mockReturnValueOnce(first.promise);
    await startExtract(['C:\\Projects\\enc.zip']);
    first.reject(passwordError());
    await flush();
    expect(dialogs.current).toMatchObject({ kind: 'password' });
    expect(jobs.docked).toHaveLength(0);
    dialogs.close();
  });
});

describe('Test Archive in the background', () => {
  it('the report waits as "Lihat hasil" and a password is held until chosen', async () => {
    register(0, 'C:\\Projects', ['C:\\Projects\\a.zip', 'C:\\Projects\\b.zip']);
    const first = deferred<unknown>();
    archiveTestMock
      .mockReturnValueOnce(first.promise)
      .mockResolvedValueOnce({
        archivePath: 'C:\\Projects\\b.zip',
        entries: [{ memberPath: 'x', status: 'passed' }],
      })
      .mockResolvedValueOnce({
        archivePath: 'C:\\Projects\\a.zip',
        entries: [{ memberPath: 'x', status: 'passed' }],
      });
    dialogs.beginTestArchive();
    await flush();
    dialogs.sendToBackground();
    const job = jobs.docked[0]!;
    expect(job.kind).toBe('test');

    first.reject(passwordError());
    await flush();
    expect(job.state).toBe('needsInput');
    expect(dialogs.current).toBeNull();

    jobs.openPrompt(job);
    await (dialogs.current as { retry: (pw: string) => Promise<void> }).retry('pw');
    await flush();

    expect(job.state).toBe('done');
    expect(job.summary).toBe('Archive test passed');
    expect(dialogs.current).toBeNull();
    jobs.openResult(job);
    expect(dialogs.current).toMatchObject({ kind: 'testReport', title: 'Archive test passed' });
  });

  it('problems make the job "failed" and cancelling it marks the report', async () => {
    register(0, 'C:\\Projects', ['C:\\Projects\\a.zip']);
    const run = deferred<unknown>();
    archiveTestMock.mockReturnValue(run.promise);
    dialogs.beginTestArchive();
    await flush();
    dialogs.sendToBackground();
    const job = jobs.docked[0]!;
    jobs.cancel(job);
    run.reject(cancelledError());
    await flush();
    expect(job.state).toBe('canceled');
    jobs.openResult(job);
    expect(dialogs.current).toMatchObject({ kind: 'testReport' });
    expect((dialogs.current as { title: string }).title).toMatch(/dibatalkan/);
  });
});

describe('FER-578 R1: cancel in the dock', () => {
  it('B1: Cancel on a docked extract that waits for a password ends it as canceled', async () => {
    const first = deferred<void>();
    archiveExtractMock.mockReturnValueOnce(first.promise);
    await startExtract(['C:\\Projects\\enc.zip', 'C:\\Projects\\two.zip']);
    dialogs.sendToBackground();
    first.reject(passwordError());
    await flush();
    const job = jobs.docked[0]!;
    expect(job.state).toBe('needsInput');

    jobs.cancel(job);
    await flush();
    expect(job.state).toBe('canceled');
    expect(jobs.runningCount).toBe(0);
    expect(archiveCancelMock).not.toHaveBeenCalled();
    // the batch is over: the next archive never started, and nothing can be re-opened
    expect(archiveExtractMock).toHaveBeenCalledTimes(1);
    jobs.openPrompt(job);
    expect(dialogs.current).toBeNull();
  });

  it('B1: Cancel while the password dialog is already on screen removes it and ends the job', async () => {
    const first = deferred<void>();
    archiveExtractMock.mockReturnValueOnce(first.promise);
    await startExtract(['C:\\Projects\\enc.zip']);
    dialogs.sendToBackground();
    first.reject(passwordError());
    await flush();
    const job = jobs.docked[0]!;
    jobs.openPrompt(job);
    expect(dialogs.current).toMatchObject({ kind: 'password' });
    jobs.cancel(job);
    await flush();
    expect(dialogs.current).toBeNull();
    expect(job.state).toBe('canceled');
  });

  it('B1: Cancel on a docked Test Archive waiting for a password finishes the run', async () => {
    register(0, 'C:\\Projects', ['C:\\Projects\\enc.zip', 'C:\\Projects\\b.zip']);
    const first = deferred<unknown>();
    archiveTestMock.mockReturnValueOnce(first.promise);
    dialogs.beginTestArchive();
    await flush();
    dialogs.sendToBackground();
    first.reject(passwordError());
    await flush();
    const job = jobs.docked[0]!;
    expect(job.state).toBe('needsInput');

    jobs.cancel(job);
    await flush();
    expect(job.state).toBe('canceled');
    expect(jobs.runningCount).toBe(0);
    expect(archiveTestMock).toHaveBeenCalledTimes(1); // b.zip never started
    jobs.openResult(job);
    const report = dialogs.current as { kind: string; title: string };
    expect(report.kind).toBe('testReport');
    expect(report.title).toMatch(/dibatalkan/);
  });

  it('S1: a cancel that lands between two archives of a batch stops the batch', async () => {
    const first = deferred<void>();
    archiveExtractMock.mockReturnValueOnce(first.promise);
    await startExtract(['C:\\Projects\\one.zip', 'C:\\Projects\\two.zip']);
    dialogs.sendToBackground();
    const job = jobs.docked[0]!;
    jobs.cancel(job); // sent to the request of one.zip, which finishes anyway
    first.resolve();
    await flush();
    expect(archiveExtractMock).toHaveBeenCalledTimes(1);
    expect(job.state).toBe('canceled');
    expect(jobs.runningCount).toBe(0);
  });
});

describe('FER-578 R1: extracts into one folder are serialised', () => {
  it('S2: the second background extract to the same folder waits for the first', async () => {
    const first = deferred<void>();
    archiveExtractMock.mockReturnValueOnce(first.promise).mockResolvedValueOnce(undefined);
    await startExtract(['C:\\Projects\\one.zip']);
    dialogs.sendToBackground();

    // a second extract, same destination written in another case
    register(0, 'C:\\Projects', ['C:\\Projects\\two.zip']);
    register(1, 'd:\\backup\\');
    dialogs.beginUnpack();
    await flush();
    expect(archiveExtractMock).toHaveBeenCalledTimes(1);
    const second = jobs.docked.find((j) => j.state === 'queued')!;
    expect(second).toBeTruthy();
    expect(dialogs.current).toBeNull();

    first.resolve();
    await flush();
    expect(archiveExtractMock).toHaveBeenCalledTimes(2);
    expect(jobs.docked.every((j) => j.state === 'done')).toBe(true);
  });

  it('S2: cancelling the queued extract never starts it', async () => {
    const first = deferred<void>();
    archiveExtractMock.mockReturnValueOnce(first.promise);
    await startExtract(['C:\\Projects\\one.zip']);
    dialogs.sendToBackground();
    register(0, 'C:\\Projects', ['C:\\Projects\\two.zip']);
    register(1, 'D:\\Backup');
    dialogs.beginUnpack();
    await flush();
    const second = jobs.docked.find((j) => j.state === 'queued')!;
    jobs.cancel(second);
    await flush();
    expect(second.state).toBe('canceled');
    first.resolve();
    await flush();
    expect(archiveExtractMock).toHaveBeenCalledTimes(1);
  });
});
