/**
 * FER-578: background jobs (extract / pack / test) in the operations panel (Alt+B), the Titlebar
 * badge, and the keyboard contract of the job rows.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { tick } from 'svelte';
import OpsPanel from './OpsPanel.svelte';
import Titlebar from './Titlebar.svelte';
import { ops } from '../stores/opsStore.svelte';
import { jobs } from '../stores/jobsStore.svelte';
import { toast } from '../stores/toastStore.svelte';
import type { OperationProgress } from '../ipc/types';

const win = vi.hoisted(() => ({
  minimize: vi.fn().mockResolvedValue(undefined),
  toggleMaximize: vi.fn().mockResolvedValue(undefined),
  close: vi.fn().mockResolvedValue(undefined),
  isMaximized: vi.fn().mockResolvedValue(false),
  onResized: vi.fn().mockResolvedValue(() => {}),
  tauri: ((window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {
    invoke: async () => undefined,
  }),
}));
vi.mock('@tauri-apps/api/window', () => ({ getCurrentWindow: () => win }));

const cancelMock = vi.fn();
vi.mock('../ipc/archive', () => ({
  archiveCancelRequest: (...args: unknown[]) => cancelMock(...args),
  listenArchiveProgress: async () => () => {},
}));

function dockedJob(over: Partial<Parameters<typeof jobs.begin>[0]> = {}) {
  const job = jobs.begin({ kind: 'extract', title: 'Unpack', subject: 'C:\\a\\big.zip', ...over });
  jobs.attempt(job, `req-${job.id}`);
  jobs.dock(job);
  return job;
}

const op = (over: Partial<OperationProgress> = {}): OperationProgress => ({
  operationId: 'op_1',
  kind: 'copy',
  state: 'running',
  processedBytes: 10,
  totalBytes: 100,
  processedFiles: 1,
  totalFiles: 10,
  ...over,
});

beforeEach(() => {
  ops.ops.clear();
  jobs.reset();
  cancelMock.mockReset();
  toast.show('', 0);
  ops.panelOpen = true;
});
afterEach(() => {
  cleanup();
  ops.panelOpen = false;
  ops.ops.clear();
  jobs.reset();
});

describe('dock in the operations panel', () => {
  it('lists a docked job with its progress bar and percentage, next to the queue', async () => {
    ops.ops.set('op_1', op());
    const job = dockedJob();
    job.processedBytes = 25;
    job.totalBytes = 100;
    render(OpsPanel);
    await tick();
    const row = screen.getByTestId('bg-job');
    expect(row.textContent).toContain('Extract');
    expect(row.textContent).toContain('C:\\a\\big.zip');
    expect(row.textContent).toContain('25%');
    expect(row.querySelector('[role="progressbar"]')?.getAttribute('aria-valuenow')).toBe('25');
    // the copy queue is still there
    expect(screen.getByText('Transfers')).toBeTruthy();
    expect(screen.getByText('Background jobs')).toBeTruthy();
  });

  it('follows progress updates of an open panel', async () => {
    const job = dockedJob();
    render(OpsPanel);
    await tick();
    expect(screen.getByTestId('bg-job').textContent).not.toContain('%');
    job.processedBytes = 50;
    job.totalBytes = 100;
    await tick();
    expect(screen.getByTestId('bg-job').textContent).toContain('50%');
  });

  it('the panel is not "empty" when only a job exists; a job that is not docked is not listed', async () => {
    dockedJob();
    const fg = jobs.begin({ kind: 'pack', title: 'Pack', subject: 'x.zip' });
    jobs.attempt(fg, 'req-fg');
    render(OpsPanel);
    await tick();
    expect(screen.queryByText('No transfers.')).toBeNull();
    expect(screen.getAllByTestId('bg-job')).toHaveLength(1);
  });

  it('Cancel on a running job cancels by request id and shows Cancelling', async () => {
    const job = dockedJob();
    render(OpsPanel);
    await tick();
    await fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(cancelMock).toHaveBeenCalledWith(job.requestId);
    await tick();
    expect(screen.getByTestId('bg-job').textContent).toContain('Cancelling');
    expect((screen.getByRole('button', { name: 'Cancel' }) as HTMLButtonElement).disabled).toBe(
      true,
    );
  });

  it('a finished job offers "Lihat hasil" (opens it and closes the panel) and Dismiss', async () => {
    const job = dockedJob();
    const open = vi.fn();
    jobs.finish(job, 'done', { summary: 'Selesai', result: { label: 'Lihat hasil', open } });
    render(OpsPanel);
    await tick();
    expect(screen.getByTestId('bg-job').textContent).toContain('Completed');
    await fireEvent.click(screen.getByRole('button', { name: 'Lihat hasil' }));
    await tick();
    await tick();
    expect(open).toHaveBeenCalledTimes(1);
    expect(ops.panelOpen).toBe(false);
  });

  it('Dismiss removes a finished job', async () => {
    const job = dockedJob();
    jobs.finish(job, 'failed', { summary: 'Gagal' });
    render(OpsPanel);
    await tick();
    expect(screen.getByTestId('bg-job').textContent).toContain('Failed');
    await fireEvent.click(screen.getByRole('button', { name: 'Dismiss' }));
    expect(jobs.list).toHaveLength(0);
  });

  it('Clear history clears finished jobs and leaves running ones', async () => {
    const done = dockedJob();
    jobs.finish(done, 'done', { silent: true });
    dockedJob({ subject: 'C:\\b\\two.zip' });
    render(OpsPanel);
    await tick();
    await fireEvent.click(screen.getByRole('button', { name: /Clear history/ }));
    expect(jobs.list).toHaveLength(1);
    expect(jobs.list[0]?.subject).toBe('C:\\b\\two.zip');
  });
});

describe('needsInput in the dock', () => {
  it('shows the state and a button that opens the held prompt (and closes the panel first)', async () => {
    const job = dockedJob();
    const open = vi.fn();
    jobs.holdPrompt(job, { label: 'Password', open }, 'Password diperlukan: big.zip');
    render(OpsPanel);
    await tick();
    const row = screen.getByTestId('bg-job');
    expect(row.getAttribute('data-state')).toBe('needsInput');
    expect(row.textContent).toContain('Needs your input');
    expect(open).not.toHaveBeenCalled();
    await fireEvent.click(screen.getByRole('button', { name: 'Password…' }));
    await tick();
    await tick();
    expect(open).toHaveBeenCalledTimes(1);
    expect(ops.panelOpen).toBe(false);
  });
});

describe('keyboard in the dock', () => {
  it('opens with focus on the first row; arrows walk jobs and queue rows alike', async () => {
    ops.ops.set('op_1', op());
    dockedJob();
    render(OpsPanel);
    await tick();
    await tick();
    const rows = Array.from(document.querySelectorAll<HTMLElement>('.panel .job'));
    expect(rows).toHaveLength(2);
    expect(document.activeElement).toBe(rows[0]);
    await fireEvent.keyDown(rows[0]!, { key: 'ArrowDown' });
    expect(document.activeElement).toBe(rows[1]);
    await fireEvent.keyDown(rows[1]!, { key: 'Home' });
    expect(document.activeElement).toBe(rows[0]);
  });

  it('Enter on a finished row opens its result; Delete dismisses a finished row', async () => {
    const a = dockedJob();
    const open = vi.fn();
    jobs.finish(a, 'done', { silent: true, result: { label: 'Lihat hasil', open } });
    const b = dockedJob({ subject: 'C:\\b\\two.zip' });
    jobs.finish(b, 'failed', { silent: true });
    render(OpsPanel);
    await tick();
    const rows = screen.getAllByTestId('bg-job');
    await fireEvent.keyDown(rows[1]!, { key: 'Delete' });
    expect(jobs.list).toHaveLength(1);
    await fireEvent.keyDown(screen.getByTestId('bg-job'), { key: 'Enter' });
    await tick();
    await tick();
    expect(open).toHaveBeenCalledTimes(1);
  });

  it('Delete never dismisses a running job', async () => {
    dockedJob();
    render(OpsPanel);
    await tick();
    await fireEvent.keyDown(screen.getByTestId('bg-job'), { key: 'Delete' });
    expect(jobs.list).toHaveLength(1);
  });

  it('Enter on a needsInput row opens the prompt', async () => {
    const job = dockedJob();
    const open = vi.fn();
    jobs.holdPrompt(job, { label: 'Password', open }, 'Password diperlukan');
    render(OpsPanel);
    await tick();
    await fireEvent.keyDown(screen.getByTestId('bg-job'), { key: 'Enter' });
    await tick();
    await tick();
    expect(open).toHaveBeenCalledTimes(1);
  });

  it('Esc closes the panel', async () => {
    dockedJob();
    render(OpsPanel);
    await tick();
    await fireEvent.keyDown(screen.getByRole('dialog'), { key: 'Escape' });
    expect(ops.panelOpen).toBe(false);
  });
});

describe('Titlebar badge', () => {
  it('counts queued operations plus running jobs; finished jobs do not count', async () => {
    ops.panelOpen = false;
    ops.ops.set('op_1', op());
    render(Titlebar);
    await tick();
    expect(screen.getByTestId('ops-badge').textContent).toBe('1');
    const job = dockedJob();
    await tick();
    expect(screen.getByTestId('ops-badge').textContent).toBe('2');
    jobs.finish(job, 'done', { silent: true });
    await tick();
    expect(screen.getByTestId('ops-badge').textContent).toBe('1');
  });

  it('a foreground (not docked) job counts too: it is still running work', async () => {
    ops.panelOpen = false;
    const fg = jobs.begin({ kind: 'pack', title: 'Pack', subject: 'x.zip' });
    jobs.attempt(fg, 'req-fg');
    render(Titlebar);
    await tick();
    expect(screen.getByTestId('ops-badge').textContent).toBe('1');
  });
});
