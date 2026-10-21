import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

// FER-583: every close path funnels into one quit flow.
const win = vi.hoisted(() => {
  const state: { handler: ((e: { preventDefault: () => void }) => void) | null } = {
    handler: null,
  };
  return {
    state,
    destroy: vi.fn().mockResolvedValue(undefined),
    onCloseRequested: vi.fn(async (h: (e: { preventDefault: () => void }) => void) => {
      state.handler = h;
      return () => {
        state.handler = null;
      };
    }),
  };
});
vi.mock('@tauri-apps/api/window', () => ({ getCurrentWindow: () => win }));
vi.mock('./ipc/tauri', () => ({ isInTauri: () => true, invoke: vi.fn() }));

import { activeWorkCount, cancelAllAndQuit, installCloseGuard, requestQuit } from './quit';
import { dialogs } from './stores/dialogStore.svelte';
import { jobs } from './stores/jobsStore.svelte';
import { ops } from './stores/opsStore.svelte';

function running(id: string) {
  return {
    operationId: id,
    kind: 'copy',
    state: 'running',
    processedBytes: 0,
    processedFiles: 0,
  } as never;
}

function closeEvent() {
  const e = { preventDefault: vi.fn() };
  win.state.handler!(e);
  return e;
}

describe('quit flow', () => {
  beforeEach(() => {
    ops.ops = new Map();
    jobs.list = [];
    dialogs.close();
    vi.clearAllMocks();
  });
  afterEach(() => {
    ops.ops = new Map();
    jobs.list = [];
    dialogs.close();
    vi.restoreAllMocks();
  });

  it('lets the window close when nothing is running', async () => {
    await installCloseGuard();
    const e = closeEvent();
    expect(e.preventDefault).not.toHaveBeenCalled();
    expect(dialogs.current).toBeNull();
  });

  it('prevents close and asks when a background job runs', async () => {
    await installCloseGuard();
    jobs.list = [{ id: 'j1', state: 'running' } as never];
    const e = closeEvent();
    expect(e.preventDefault).toHaveBeenCalledTimes(1);
    expect(dialogs.current).toMatchObject({ kind: 'quitConfirm', operations: 1 });
    expect(win.destroy).not.toHaveBeenCalled();
  });

  it('counts operations and jobs together; repeated close does not stack dialogs', async () => {
    await installCloseGuard();
    ops.ops = new Map([['op_1', running('op_1')]]);
    jobs.list = [{ id: 'j1', state: 'running' } as never];
    closeEvent();
    closeEvent();
    expect(dialogs.current).toMatchObject({ kind: 'quitConfirm', operations: 2 });
    dialogs.close();
    expect(dialogs.current).toBeNull(); // no queued duplicate
  });

  it('requestQuit destroys the window directly when idle', async () => {
    requestQuit();
    await Promise.resolve();
    expect(win.destroy).toHaveBeenCalledTimes(1);
    expect(dialogs.current).toBeNull();
  });

  it('Cancel (dialog dismissed) leaves the window open', async () => {
    jobs.list = [{ id: 'j1', state: 'running' } as never];
    requestQuit();
    dialogs.close();
    expect(win.destroy).not.toHaveBeenCalled();
  });

  it('confirm cancels jobs and operations, then destroys', async () => {
    const cancelJob = vi.spyOn(jobs, 'cancel').mockImplementation((j) => {
      j.state = 'canceled';
    });
    const cancelOp = vi.spyOn(ops, 'cancel').mockImplementation(async (id) => {
      ops.ops = new Map([...ops.ops].filter(([k]) => k !== id));
    });
    ops.ops = new Map([['op_1', running('op_1')]]);
    jobs.list = [{ id: 'j1', state: 'running' } as never];
    await cancelAllAndQuit(500);
    expect(cancelJob).toHaveBeenCalledTimes(1);
    expect(cancelOp).toHaveBeenCalledWith('op_1');
    expect(win.destroy).toHaveBeenCalledTimes(1);
  });

  it('does not hang when the backend never settles', async () => {
    vi.spyOn(jobs, 'cancel').mockImplementation(() => {});
    jobs.list = [{ id: 'j1', state: 'running' } as never];
    const t0 = Date.now();
    await cancelAllAndQuit(150);
    expect(Date.now() - t0).toBeLessThan(2000);
    expect(win.destroy).toHaveBeenCalledTimes(1);
    expect(activeWorkCount()).toBe(1);
  });
});
