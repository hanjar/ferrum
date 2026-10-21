import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

// FER-583: Rust holds every main-window close and emits an event; the frontend acks and decides.
const h = vi.hoisted(() => {
  const state: { handler: (() => void) | null } = { handler: null };
  return {
    state,
    destroy: vi.fn().mockResolvedValue(undefined),
    invoke: vi.fn().mockResolvedValue(undefined),
    listen: vi.fn(async (_name: string, cb: () => void) => {
      state.handler = cb;
      return () => {
        state.handler = null;
      };
    }),
  };
});
vi.mock('@tauri-apps/api/window', () => ({ getCurrentWindow: () => ({ destroy: h.destroy }) }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: h.invoke }));
vi.mock('@tauri-apps/api/event', () => ({ listen: h.listen }));
vi.mock('./ipc/tauri', () => ({ isInTauri: () => true, invoke: vi.fn() }));

import {
  CLOSE_EVENT,
  activeWorkCount,
  cancelAllAndQuit,
  handleCloseRequest,
  installCloseGuard,
  requestQuit,
  resetQuitState,
} from './quit';
import { dialogs } from './stores/dialogStore.svelte';
import { jobs } from './stores/jobsStore.svelte';
import { ops } from './stores/opsStore.svelte';

function op(id: string, state: 'running' | 'queued' = 'running') {
  return { operationId: id, kind: 'copy', state, processedBytes: 0, processedFiles: 0 } as never;
}

async function close() {
  await handleCloseRequest();
}

describe('quit flow', () => {
  beforeEach(() => {
    ops.ops = new Map();
    jobs.list = [];
    dialogs.close();
    resetQuitState();
    vi.clearAllMocks();
    h.invoke.mockResolvedValue(undefined);
  });
  afterEach(() => {
    ops.ops = new Map();
    jobs.list = [];
    dialogs.close();
    vi.restoreAllMocks();
  });

  it('listens for the close event from Rust', async () => {
    await installCloseGuard();
    expect(h.listen).toHaveBeenCalledWith(CLOSE_EVENT, expect.any(Function));
  });

  it('acks first, then destroys when idle', async () => {
    await close();
    expect(h.invoke).toHaveBeenCalledWith('close_ack');
    expect(h.destroy).toHaveBeenCalledTimes(1);
    expect(dialogs.current).toBeNull();
  });

  it('acks and asks when a background job runs', async () => {
    jobs.list = [{ id: 'j1', state: 'running' } as never];
    await close();
    expect(h.invoke).toHaveBeenCalledWith('close_ack');
    expect(dialogs.current).toMatchObject({ kind: 'quitConfirm', operations: 1 });
    expect(h.destroy).not.toHaveBeenCalled();
  });

  it('counts ops and jobs together; repeated close does not stack dialogs', async () => {
    ops.ops = new Map([['op_1', op('op_1')]]);
    jobs.list = [{ id: 'j1', state: 'running' } as never];
    await close();
    await close();
    expect(dialogs.current).toMatchObject({ kind: 'quitConfirm', operations: 2 });
    dialogs.close();
    expect(dialogs.current).toBeNull();
  });

  it('a failing ack does not stop the flow', async () => {
    h.invoke.mockRejectedValueOnce(new Error('ipc down'));
    await close();
    expect(h.destroy).toHaveBeenCalledTimes(1);
  });

  it('a handler error fails open: the window is destroyed', async () => {
    jobs.list = [{ id: 'j1', state: 'running' } as never];
    vi.spyOn(dialogs, 'open').mockImplementation(() => {
      throw new Error('boom');
    });
    await close();
    expect(h.destroy).toHaveBeenCalledTimes(1);
  });

  it('Cancel (dialog dismissed) leaves the window open', () => {
    jobs.list = [{ id: 'j1', state: 'running' } as never];
    requestQuit();
    dialogs.close();
    expect(h.destroy).not.toHaveBeenCalled();
  });

  it('confirm cancels jobs and operations, then destroys', async () => {
    const cancelJob = vi.spyOn(jobs, 'cancel').mockImplementation((j) => {
      j.state = 'canceled';
    });
    const cancelOp = vi.spyOn(ops, 'cancel').mockImplementation(async (id) => {
      ops.ops = new Map([...ops.ops].filter(([k]) => k !== id));
    });
    ops.ops = new Map([['op_1', op('op_1')]]);
    jobs.list = [{ id: 'j1', state: 'running' } as never];
    await cancelAllAndQuit(500);
    expect(cancelJob).toHaveBeenCalledTimes(1);
    expect(cancelOp).toHaveBeenCalledWith('op_1');
    expect(h.destroy).toHaveBeenCalledTimes(1);
  });

  it('does not wait for a queued op (cancelled without any event)', async () => {
    vi.spyOn(ops, 'cancel').mockImplementation(async (id) => {
      // running op reports terminal; the queued one stays "queued" in the store
      if (id === 'run') ops.ops = new Map([...ops.ops].filter(([k]) => k !== 'run'));
    });
    ops.ops = new Map([
      ['run', op('run')],
      ['wait', op('wait', 'queued')],
    ]);
    const t0 = Date.now();
    await cancelAllAndQuit(2500);
    expect(Date.now() - t0).toBeLessThan(1000);
    expect(h.destroy).toHaveBeenCalledTimes(1);
  });

  it('does not hang when the backend never settles', async () => {
    vi.spyOn(jobs, 'cancel').mockImplementation(() => {});
    jobs.list = [{ id: 'j1', state: 'running' } as never];
    const t0 = Date.now();
    await cancelAllAndQuit(150);
    expect(Date.now() - t0).toBeLessThan(2000);
    expect(h.destroy).toHaveBeenCalledTimes(1);
    expect(activeWorkCount()).toBe(1);
  });

  it('while quitting, a second close shows no dialog and does not destroy twice', async () => {
    vi.spyOn(jobs, 'cancel').mockImplementation(() => {});
    jobs.list = [{ id: 'j1', state: 'running' } as never];
    const p = cancelAllAndQuit(200);
    await close();
    requestQuit();
    expect(dialogs.current).toBeNull();
    await p;
    expect(h.invoke).toHaveBeenCalledWith('close_ack');
    expect(h.destroy).toHaveBeenCalledTimes(1);
  });
});
