/**
 * FER-585 (FER-577 B5): the confirmed Delete runs as a backend job. The dialog shows progress
 * only when it takes longer than a moment; Esc / Background docks it (FER-578); Cancel stops it
 * and a cancel never hides what is left; quit counts it.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render } from '@testing-library/svelte';
import { tick } from 'svelte';
import Dialogs from './Dialogs.svelte';
import { dialogs } from '../stores/dialogStore.svelte';
import { jobs } from '../stores/jobsStore.svelte';
import { toast } from '../stores/toastStore.svelte';
import { activeWorkCount } from '../quit';
import type { DeleteJobResult, DeleteProgress } from '../ipc/deleteJobs';

const h = vi.hoisted(() => ({
  runDelete: vi.fn(),
  deleteCancel: vi.fn(),
  noteScopeMutation: vi.fn(),
}));

vi.mock('../ipc/deleteJobs', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../ipc/deleteJobs')>()),
  runDelete: (...a: unknown[]) => h.runDelete(...a),
  deleteCancel: (...a: unknown[]) => h.deleteCancel(...a),
  newDeleteRequestId: () => 'del-test',
}));
vi.mock('../ipc/mutations', () => ({
  planCopy: vi.fn(),
  createEntry: vi.fn(),
  deleteEntries: vi.fn(),
  enqueueCopy: vi.fn(),
  enqueueMove: vi.fn(),
  renameEntry: vi.fn(),
}));
vi.mock('../ipc/clipboardFiles', () => ({ clipboardFinishCut: vi.fn() }));
vi.mock('../ipc/archive', () => ({
  archiveList: vi.fn(),
  archiveRead: vi.fn(),
  archiveExtract: vi.fn(),
  archiveExtractPreflight: vi.fn(),
  archiveTest: vi.fn(),
  archiveCreate: vi.fn(),
  archiveCreateZip: vi.fn(),
  archiveCancelRequest: vi.fn(),
  archiveRarToolProbe: vi.fn(async () => ({ ok: false })),
  archiveRarToolDetect: vi.fn(async () => null),
  newArchiveRequestId: () => 'req-x',
  listenArchiveProgress: vi.fn(async () => () => {}),
}));
vi.mock('../diskUsage/invalidation', () => ({
  noteScopeMutation: (...a: unknown[]) => h.noteScopeMutation(...a),
  recordQueuedMove: vi.fn(),
}));

const result = (over: Partial<DeleteJobResult> = {}, deleted = 2): DeleteJobResult => ({
  report: { requested: 2, deleted, failed: [] },
  cancelled: false,
  notProcessed: [],
  partial: [],
  ...over,
});

/** A job the test finishes and feeds progress to by hand. */
function controllable() {
  let finish!: (r: DeleteJobResult) => void;
  let onProgress!: (p: DeleteProgress) => void;
  h.runDelete.mockImplementationOnce(
    (_paths: string[], _perm: boolean, _id: string, cb: (p: DeleteProgress) => void) => {
      onProgress = cb;
      return new Promise<DeleteJobResult>((resolve) => {
        finish = resolve;
      });
    },
  );
  return {
    finish: (r: DeleteJobResult) => finish(r),
    progress: (p: DeleteProgress) => onProgress(p),
  };
}

const modal = () => document.querySelector<HTMLElement>('[role="dialog"]')!;
const button = (label: string) =>
  Array.from(modal().querySelectorAll('button')).find((b) => b.textContent?.trim() === label)!;
const PATHS = ['C:/x/a', 'C:/x/b'];

async function confirm(extra: Record<string, unknown> = {}, permanent = false) {
  render(Dialogs);
  dialogs.open({ kind: 'delete', paths: [...PATHS], permanent, ...extra });
  await tick();
  fireEvent.click(button(permanent ? 'Delete permanently' : 'Delete'));
  await tick();
}

const progressBar = () => document.querySelector('[data-testid="delete-progress"]');

beforeEach(() => {
  h.runDelete.mockReset();
  h.deleteCancel.mockReset();
  h.noteScopeMutation.mockReset();
  jobs.reset();
});
afterEach(() => {
  dialogs.close();
  dialogs.endDeleteRun();
  jobs.reset();
  toast.show('', 0);
});

describe('FER-585: Delete as a backend job', () => {
  it('starts the job with the confirmed paths and the permanent flag (confirmation unchanged)', async () => {
    const job = controllable();
    await confirm({}, true);
    expect(h.runDelete).toHaveBeenCalledWith(PATHS, true, 'del-test', expect.any(Function));
    job.finish(result());
    await vi.waitFor(() => expect(dialogs.current).toBeNull());
  });

  it('a quick delete closes without ever showing a progress bar and leaves no job behind', async () => {
    const job = controllable();
    await confirm();
    expect(progressBar()).toBeNull();
    job.finish(result());
    await vi.waitFor(() => expect(dialogs.current).toBeNull());
    expect(jobs.list).toHaveLength(0);
    expect(h.noteScopeMutation).toHaveBeenCalledWith(PATHS);
  });

  it('a long delete shows progress after the delay, with Background and Cancel', async () => {
    const job = controllable();
    await confirm();
    job.progress({ done: 1, total: 2, currentName: 'b', entries: 40, bytes: 2048 });
    await vi.waitFor(() => expect(progressBar()).not.toBeNull(), { timeout: 2000 });
    expect(progressBar()?.getAttribute('aria-valuenow')).toBe('50');
    expect(modal().textContent).toContain('1 / 2');
    // FER-586: entries inside folders move per entry.
    expect(modal().textContent).toContain('40 entries removed, 2.00 KB');
    expect(button('Background')).toBeTruthy();
    expect(jobs.running).toHaveLength(1);
    expect(jobs.running[0]).toMatchObject({ kind: 'delete', processedEntries: 1, totalEntries: 2 });
    job.finish(result());
    await vi.waitFor(() => expect(dialogs.current).toBeNull());
  });

  it('Esc sends a running delete to the dock; it keeps running and ends there', async () => {
    const job = controllable();
    await confirm();
    await vi.waitFor(() => expect(progressBar()).not.toBeNull(), { timeout: 2000 });
    fireEvent.keyDown(modal(), { key: 'Escape' });
    await tick();
    expect(dialogs.current).toBeNull();
    expect(h.deleteCancel).not.toHaveBeenCalled();
    expect(jobs.docked).toHaveLength(1);
    expect(jobs.docked[0]!.state).toBe('running');
    expect(activeWorkCount()).toBe(1); // quit / close guard still counts it

    job.progress({ done: 2, total: 2, entries: 9 });
    job.finish(result());
    await vi.waitFor(() => expect(jobs.docked[0]!.state).toBe('done'));
    expect(jobs.docked[0]!.summary).toBe('Delete finished: 2 items');
    expect(dialogs.current).toBeNull(); // a docked delete never pops a dialog
    expect(activeWorkCount()).toBe(0);
  });

  it('the Background button does the same as Esc', async () => {
    const job = controllable();
    await confirm();
    await vi.waitFor(() => expect(progressBar()).not.toBeNull(), { timeout: 2000 });
    fireEvent.click(button('Background'));
    await tick();
    expect(dialogs.current).toBeNull();
    expect(jobs.docked).toHaveLength(1);
    job.finish(result());
    await vi.waitFor(() => expect(jobs.docked[0]!.state).toBe('done'));
  });

  it('a docked delete with failures keeps them as "Lihat hasil" instead of opening a dialog', async () => {
    const job = controllable();
    await confirm();
    await vi.waitFor(() => expect(progressBar()).not.toBeNull(), { timeout: 2000 });
    fireEvent.keyDown(modal(), { key: 'Escape' });
    job.finish(
      result({
        report: {
          requested: 2,
          deleted: 1,
          failed: [{ path: 'C:/x/b', code: 'permissionDenied', message: 'locked' }],
        },
      }),
    );
    await vi.waitFor(() => expect(jobs.docked[0]!.state).toBe('failed'));
    expect(dialogs.current).toBeNull();
    jobs.openResult(jobs.docked[0]!);
    expect(dialogs.current).toMatchObject({ kind: 'error' });
    expect((dialogs.current as unknown as { message: string }).message).toContain('locked');
  });

  it('Cancel asks the backend to stop; a cancelled result keeps the dialog and says what is left', async () => {
    const job = controllable();
    await confirm();
    await vi.waitFor(() => expect(progressBar()).not.toBeNull(), { timeout: 2000 });
    fireEvent.click(button('Cancel'));
    await tick();
    expect(h.deleteCancel).toHaveBeenCalledWith('del-test');
    expect(jobs.running[0]!.state).toBe('cancelling');
    expect(dialogs.current?.kind).toBe('delete');

    job.finish(result({ cancelled: true, partial: ['C:/x/big'], notProcessed: ['C:/x/b'] }, 1));
    await vi.waitFor(() =>
      expect(modal().querySelector('.error')?.textContent).toContain('Cancelled'),
    );
    const text = modal().querySelector('.error')!.textContent!;
    expect(text).toContain('only partly deleted');
    expect(text).toContain('big');
    expect(text).toContain('not touched');
    expect(jobs.list).toHaveLength(0);
    // Another Delete continues with exactly what is left, not with what is gone.
    const d = dialogs.current as unknown as { paths: string[] };
    expect(d.paths).toEqual(['C:/x/big', 'C:/x/b']);
    expect(h.noteScopeMutation).toHaveBeenCalled(); // the partly deleted folder changed the disk
  });

  it('after Cancel the focus stays in the dialog, so Esc still sends the job to the dock', async () => {
    const job = controllable();
    await confirm();
    await vi.waitFor(() => expect(progressBar()).not.toBeNull(), { timeout: 2000 });
    fireEvent.click(button('Cancel'));
    await tick();
    await tick();
    expect(button('Cancel').hasAttribute('disabled')).toBe(true);
    expect(document.activeElement).toBe(modal());
    fireEvent.keyDown(document.activeElement as HTMLElement, { key: 'Escape' });
    await tick();
    expect(dialogs.current).toBeNull();
    expect(jobs.docked).toHaveLength(1);
    expect(jobs.docked[0]!.state).toBe('cancelling');
    job.finish(result({ cancelled: true, notProcessed: [...PATHS] }, 0));
    await vi.waitFor(() => expect(jobs.docked[0]!.state).toBe('canceled'));
  });

  it('a delete that holds a folder lease cannot go to the background but can be cancelled', async () => {
    const job = controllable();
    const onSettled = vi.fn();
    await confirm({ onSettled });
    await vi.waitFor(() => expect(progressBar()).not.toBeNull(), { timeout: 2000 });
    const labels = Array.from(modal().querySelectorAll('button')).map((b) => b.textContent);
    expect(labels.some((l) => l?.includes('Background'))).toBe(false);
    fireEvent.keyDown(modal(), { key: 'Escape' });
    await tick();
    expect(dialogs.current?.kind).toBe('delete');
    expect(onSettled).not.toHaveBeenCalled();
    fireEvent.click(button('Cancel'));
    expect(h.deleteCancel).toHaveBeenCalledWith('del-test');
    job.finish(result({ cancelled: true, notProcessed: [...PATHS] }, 0));
    await vi.waitFor(() => expect(modal().querySelector('.error')).toBeTruthy());
  });

  it('a refused start (guard) shows the error and leaves nothing running', async () => {
    h.runDelete.mockRejectedValueOnce(new Error('Path is inside a protected folder'));
    await confirm();
    await vi.waitFor(() => expect(modal().querySelector('.error')).toBeTruthy());
    expect(jobs.list).toHaveLength(0);
    expect(dialogs.current?.kind).toBe('delete');
    expect(h.noteScopeMutation).not.toHaveBeenCalled();
  });
});
