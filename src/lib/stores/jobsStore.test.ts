import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const cancelMock = vi.fn();
let handler: ((e: Record<string, unknown>) => void) | null = null;
let listenCalls = 0;
vi.mock('../ipc/archive', () => ({
  archiveCancelRequest: (...args: unknown[]) => cancelMock(...args),
  listenArchiveProgress: async (h: (e: Record<string, unknown>) => void) => {
    listenCalls++;
    handler = h;
    return () => {};
  },
}));

const { jobs, jobPercent, isJobActive, baseName } = await import('./jobsStore.svelte');
const { toast } = await import('./toastStore.svelte');

beforeEach(() => {
  jobs.reset();
  cancelMock.mockReset();
  handler = null;
  listenCalls = 0;
  toast.show('', 0);
});

afterEach(() => {
  jobs.reset();
});

function start(over: Partial<Parameters<typeof jobs.begin>[0]> = {}) {
  const job = jobs.begin({ kind: 'extract', title: 'Unpack', subject: 'C:\\a\\big.zip', ...over });
  jobs.attempt(job, 'req-1');
  return job;
}

describe('jobsStore lifecycle', () => {
  it('begins running and not docked, with zeroed progress', () => {
    const job = start();
    expect(job).toMatchObject({
      kind: 'extract',
      state: 'running',
      docked: false,
      processedBytes: 0,
      requestId: 'req-1',
    });
    expect(jobs.runningCount).toBe(1);
    expect(jobs.docked).toHaveLength(0);
  });

  it('docking lists the job; a finished job that was never docked disappears', () => {
    const a = start();
    jobs.dock(a);
    expect(jobs.docked.map((j) => j.id)).toEqual([a.id]);

    const b = jobs.begin({ kind: 'pack', title: 'Pack', subject: 'x.zip' });
    jobs.finish(b, 'done');
    expect(jobs.list.find((j) => j.id === b.id)).toBeUndefined();
  });

  it('a docked job keeps its outcome, result and a toast after it ends', () => {
    const job = start();
    jobs.dock(job);
    const open = vi.fn();
    jobs.finish(job, 'done', {
      summary: 'Ekstrak selesai: 3 ditulis',
      result: { label: 'Lihat hasil', open },
    });
    expect(job).toMatchObject({ state: 'done', summary: 'Ekstrak selesai: 3 ditulis' });
    expect(isJobActive(job)).toBe(false);
    expect(jobs.runningCount).toBe(0);
    expect(jobs.hasFinished).toBe(true);
    expect(toast.message).toBe('Ekstrak selesai: 3 ditulis');
    jobs.openResult(job);
    expect(open).toHaveBeenCalledTimes(1);
    jobs.clearFinished();
    expect(jobs.list).toHaveLength(0);
  });

  it('a silent finish of a docked job does not toast', () => {
    const job = start();
    jobs.dock(job);
    jobs.finish(job, 'failed', { summary: 'Gagal', silent: true });
    expect(toast.message).toBe('');
    expect(job.state).toBe('failed');
  });

  it('dock does nothing to a job that already ended', () => {
    const job = start();
    jobs.finish(job, 'done');
    jobs.dock(job);
    expect(jobs.docked).toHaveLength(0);
  });
});

describe('progress events', () => {
  it('one global listener updates only the job with that request id', async () => {
    const a = start();
    const b = jobs.begin({ kind: 'test', title: 'Test', subject: 'b.zip' });
    jobs.attempt(b, 'req-2');
    await jobs.ensureListening();
    await jobs.ensureListening();
    expect(listenCalls).toBe(1);

    handler?.({
      requestId: 'req-1',
      processedBytes: 2048,
      totalBytes: 4096,
      processedEntries: 1,
      totalEntries: 2,
      currentMember: 'docs/readme.md',
      writtenEntries: 1,
    });
    expect(a).toMatchObject({
      processedBytes: 2048,
      totalBytes: 4096,
      currentMember: 'docs/readme.md',
      writtenEntries: 1,
    });
    expect(jobPercent(a)).toBe(50);
    expect(b.processedBytes).toBe(0);
    expect(jobPercent(b)).toBeNull();
  });

  it('events keep arriving for a docked job and a new attempt resets the numbers', async () => {
    const job = start();
    jobs.dock(job);
    await jobs.ensureListening();
    handler?.({ requestId: 'req-1', processedBytes: 10, totalBytes: 100, processedEntries: 1 });
    expect(job.processedBytes).toBe(10);
    jobs.attempt(job, 'req-2', { subject: 'two.zip', position: '2/2' });
    expect(job).toMatchObject({
      processedBytes: 0,
      subject: 'two.zip',
      position: '2/2',
      state: 'running',
    });
    // a late event of the previous step is ignored
    handler?.({ requestId: 'req-1', processedBytes: 99, processedEntries: 9 });
    expect(job.processedBytes).toBe(0);
  });

  it('an event for a finished request is ignored', async () => {
    const job = start();
    jobs.dock(job);
    await jobs.ensureListening();
    jobs.finish(job, 'done', { silent: true });
    handler?.({ requestId: 'req-1', processedBytes: 77, processedEntries: 1 });
    expect(job.processedBytes).toBe(0);
  });

  it('the mirror sink of the dialog gets the event until the step ends', async () => {
    const job = jobs.begin({ kind: 'pack', title: 'Pack', subject: 'x.zip' });
    const sink = vi.fn();
    jobs.attempt(job, 'req-s', undefined, sink);
    await jobs.ensureListening();
    handler?.({ requestId: 'req-s', processedBytes: 1, processedEntries: 1 });
    expect(sink).toHaveBeenCalledTimes(1);
    jobs.endAttempt(job);
    handler?.({ requestId: 'req-s', processedBytes: 2, processedEntries: 1 });
    expect(sink).toHaveBeenCalledTimes(1);
  });
});

describe('cancel', () => {
  it('cancels by request id once and shows cancelling', () => {
    const job = start();
    jobs.dock(job);
    jobs.cancel(job);
    jobs.cancel(job);
    expect(cancelMock).toHaveBeenCalledTimes(1);
    expect(cancelMock).toHaveBeenCalledWith('req-1');
    expect(job.state).toBe('cancelling');
    expect(jobs.runningCount).toBe(1); // still running until the backend settles
  });

  it('a job with a cancel hook uses it instead of the archive request', () => {
    const job = jobs.begin({ kind: 'verify', title: 'Verify', subject: 'a.sfv' });
    const hook = vi.fn();
    jobs.setCancelHook(job, hook);
    jobs.cancel(job);
    expect(hook).toHaveBeenCalledTimes(1);
    expect(cancelMock).not.toHaveBeenCalled();
  });

  it('cancelling a finished job does nothing', () => {
    const job = start();
    jobs.dock(job);
    jobs.finish(job, 'done', { silent: true });
    jobs.cancel(job);
    expect(cancelMock).not.toHaveBeenCalled();
  });
});

describe('needsInput', () => {
  it('holds the prompt, flags the job, toasts with an action, and opens only on request', () => {
    const job = start();
    jobs.dock(job);
    const open = vi.fn();
    jobs.holdPrompt(job, { label: 'Password', open }, 'Password diperlukan: big.zip');
    expect(job.state).toBe('needsInput');
    expect(jobs.needInputCount).toBe(1);
    expect(jobs.runningCount).toBe(1);
    expect(toast.message).toBe('Password diperlukan: big.zip');
    expect(toast.action?.label).toBe('Buka');
    expect(open).not.toHaveBeenCalled();

    toast.runAction();
    expect(open).toHaveBeenCalledTimes(1);
    expect(job.state).toBe('running');
    expect(job.prompt).toBeUndefined();
    // the prompt does not open twice
    jobs.openPrompt(job);
    expect(open).toHaveBeenCalledTimes(1);
  });
});

describe('helpers', () => {
  it('jobPercent falls back to entries and is 100 when done', () => {
    const job = start();
    expect(jobPercent(job)).toBeNull();
    job.totalEntries = 4;
    job.processedEntries = 1;
    expect(jobPercent(job)).toBe(25);
    jobs.dock(job);
    jobs.finish(job, 'done', { silent: true });
    expect(jobPercent(job)).toBe(100);
  });

  it('baseName handles both separators', () => {
    expect(baseName('C:\\a\\b.zip')).toBe('b.zip');
    expect(baseName('a/b/c.7z')).toBe('c.7z');
    expect(baseName('plain')).toBe('plain');
  });
});
