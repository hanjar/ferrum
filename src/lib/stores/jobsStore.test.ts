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
    expect(toast.message).toBe('Password diperlukan: big.zip (Alt+B)');
    expect(toast.action?.label).toBe('Buka');
    expect(open).not.toHaveBeenCalled();

    toast.runAction();
    expect(open).toHaveBeenCalledTimes(1);
    // Still waiting for the answer (the dialog may be queued behind another one).
    expect(job.state).toBe('needsInput');
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

describe('FER-578 R1: cancel while waiting', () => {
  it('Cancel on a needsInput job dismisses the prompt and sends nothing to the backend', () => {
    const job = start();
    jobs.dock(job);
    const dismiss = vi.fn();
    jobs.holdPrompt(job, { label: 'Password', open: vi.fn(), dismiss }, 'Password diperlukan');
    jobs.cancel(job);
    expect(dismiss).toHaveBeenCalledTimes(1);
    expect(cancelMock).not.toHaveBeenCalled();
    expect(job.prompt).toBeUndefined();
    // the flow settles it the way a dismissed prompt does
    jobs.finish(job, 'canceled', { silent: true });
    expect(job.state).toBe('canceled');
    expect(jobs.runningCount).toBe(0);
    // the toast "Buka" can no longer bring it back
    jobs.openPrompt(job);
    expect(job.state).toBe('canceled');
  });
});

describe('FER-578 R1: one extract per destination folder', () => {
  it('the second job waits (queued, docked), starts when the first finishes; case-insensitive', () => {
    const a = jobs.begin({ kind: 'extract', title: 'Unpack', subject: 'a.zip' });
    const b = jobs.begin({ kind: 'extract', title: 'Unpack', subject: 'b.zip' });
    const startA = vi.fn();
    const startB = vi.fn();
    jobs.claimTarget(a, 'D:\\Out\\Dir\\', startA, vi.fn());
    jobs.claimTarget(b, 'd:/out/DIR', startB, vi.fn());
    expect(startA).toHaveBeenCalledTimes(1);
    expect(startB).not.toHaveBeenCalled();
    expect(b).toMatchObject({ state: 'queued', docked: true });
    expect(jobs.runningCount).toBe(2);

    jobs.finish(a, 'done', { silent: true });
    expect(startB).toHaveBeenCalledTimes(1);
    expect(b.state).toBe('running');
    jobs.finish(b, 'done', { silent: true });
  });

  it('different folders run in parallel', () => {
    const a = jobs.begin({ kind: 'extract', title: 'Unpack', subject: 'a.zip' });
    const b = jobs.begin({ kind: 'extract', title: 'Unpack', subject: 'b.zip' });
    const startB = vi.fn();
    jobs.claimTarget(a, 'D:\\One', vi.fn(), vi.fn());
    jobs.claimTarget(b, 'D:\\Two', startB, vi.fn());
    expect(startB).toHaveBeenCalledTimes(1);
  });

  it('cancelling a queued job leaves the line and aborts without starting; the holder is untouched', () => {
    const a = jobs.begin({ kind: 'extract', title: 'Unpack', subject: 'a.zip' });
    const b = jobs.begin({ kind: 'extract', title: 'Unpack', subject: 'b.zip' });
    const c = jobs.begin({ kind: 'extract', title: 'Unpack', subject: 'c.zip' });
    const startB = vi.fn();
    const abortB = vi.fn();
    const startC = vi.fn();
    jobs.claimTarget(a, 'D:\\Out', vi.fn(), vi.fn());
    jobs.claimTarget(b, 'D:\\Out', startB, abortB);
    jobs.claimTarget(c, 'D:\\Out', startC, vi.fn());
    jobs.cancel(b);
    expect(abortB).toHaveBeenCalledTimes(1);
    expect(cancelMock).not.toHaveBeenCalled();
    jobs.finish(b, 'canceled', { silent: true });
    jobs.finish(a, 'done', { silent: true });
    expect(startB).not.toHaveBeenCalled();
    expect(startC).toHaveBeenCalledTimes(1);
  });
});

describe('FER-585: delete jobs', () => {
  it('a delete job is counted while it runs, cancels through its own hook and docks with its outcome', () => {
    const job = jobs.begin({ kind: 'delete', title: 'Delete', subject: '3 item' });
    jobs.attempt(job, 'del-1');
    const hook = vi.fn();
    jobs.setCancelHook(job, hook);
    jobs.update(job, { processedEntries: 1, totalEntries: 3, currentMember: 'big' });
    expect(jobs.runningCount).toBe(1);
    expect(jobPercent(job)).toBe(33);

    jobs.dock(job);
    expect(jobs.docked).toHaveLength(1);

    jobs.cancel(job);
    expect(job.state).toBe('cancelling');
    expect(hook).toHaveBeenCalledTimes(1);
    // The delete cancel is not an archive request.
    expect(cancelMock).not.toHaveBeenCalled();
    jobs.cancel(job); // idempotent
    expect(hook).toHaveBeenCalledTimes(1);

    jobs.finish(job, 'canceled', { summary: 'Delete cancelled: 1/3 deleted' });
    expect(job.state).toBe('canceled');
    expect(jobs.runningCount).toBe(0);
    expect(jobs.docked).toHaveLength(1); // stays in the dock with its outcome
    expect(toast.message).toContain('Delete cancelled');
    jobs.clearFinished();
    expect(jobs.list).toHaveLength(0);
  });

  it('a foreground delete that finishes leaves no trace', () => {
    const job = jobs.begin({ kind: 'delete', title: 'Delete', subject: 'a.txt' });
    jobs.attempt(job, 'del-2');
    jobs.finish(job, 'done', { silent: true });
    expect(jobs.list).toHaveLength(0);
  });
});
