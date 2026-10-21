import { beforeEach, describe, expect, it, vi } from 'vitest';

const h = vi.hoisted(() => ({
  handlers: new Map<string, (ev: { payload: unknown }) => void>(),
  unlisten: vi.fn(),
  invoke: vi.fn(),
  inTauri: true,
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async (event: string, cb: (ev: { payload: unknown }) => void) => {
    h.handlers.set(event, cb);
    return h.unlisten;
  }),
}));
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => h.invoke(...a) }));
vi.mock('./tauri', () => ({ isInTauri: () => h.inTauri }));

import { deleteCancel, newDeleteRequestId, runDelete } from './deleteJobs';

const emit = (event: string, payload: unknown) => h.handlers.get(event)!({ payload });

beforeEach(() => {
  h.handlers.clear();
  h.unlisten.mockReset();
  h.invoke.mockReset();
  h.inTauri = true;
});

describe('runDelete', () => {
  it('registers both listeners before the job starts, relays progress and resolves with the result', async () => {
    const seen: unknown[] = [];
    h.invoke.mockImplementation(async (cmd: string) => {
      expect(cmd).toBe('delete_start');
      expect(h.handlers.has('delete://progress')).toBe(true);
      expect(h.handlers.has('delete://done')).toBe(true);
      queueMicrotask(() => {
        emit('delete://progress', { requestId: 'other', done: 9, total: 9, entries: 9 });
        emit('delete://progress', { requestId: 'r1', done: 1, total: 2, entries: 5 });
        emit('delete://done', {
          requestId: 'r1',
          result: {
            report: { requested: 2, deleted: 2, failed: [] },
            cancelled: false,
            notProcessed: [],
            partial: [],
          },
        });
      });
    });
    const result = await runDelete(['a', 'b'], true, 'r1', (p) => seen.push(p));
    expect(h.invoke).toHaveBeenCalledWith('delete_start', {
      paths: ['a', 'b'],
      permanent: true,
      requestId: 'r1',
    });
    expect(result.report.deleted).toBe(2);
    expect(seen).toEqual([{ requestId: 'r1', done: 1, total: 2, entries: 5 }]); // other ids ignored
    expect(h.unlisten).toHaveBeenCalledTimes(2);
  });

  it('a refused start rejects with the backend error and stops listening', async () => {
    h.invoke.mockRejectedValue({ code: 'invalidPath', message: 'protected folder' });
    await expect(runDelete(['a'], false, 'r2', () => {})).rejects.toMatchObject({
      code: 'invalidPath',
      message: 'protected folder',
    });
    expect(h.unlisten).toHaveBeenCalledTimes(2);
  });

  it('a done event with an error rejects', async () => {
    h.invoke.mockImplementation(async () => {
      queueMicrotask(() =>
        emit('delete://done', { requestId: 'r3', error: { code: 'io', message: 'worker died' } }),
      );
    });
    await expect(runDelete(['a'], false, 'r3', () => {})).rejects.toMatchObject({
      message: 'worker died',
    });
  });

  it('is unsupported outside the desktop app', async () => {
    h.inTauri = false;
    await expect(runDelete(['a'], false, 'r4', () => {})).rejects.toMatchObject({
      code: 'unsupported',
    });
    expect(h.invoke).not.toHaveBeenCalled();
  });
});

describe('deleteCancel / ids', () => {
  it('forwards the cancel by request id and never throws', async () => {
    h.invoke.mockResolvedValue(undefined);
    await deleteCancel('r1');
    expect(h.invoke).toHaveBeenCalledWith('delete_cancel', { requestId: 'r1' });
    h.invoke.mockRejectedValue(new Error('gone'));
    await expect(deleteCancel('r1')).resolves.toBeUndefined();
  });

  it('makes distinct ids', () => {
    expect(newDeleteRequestId()).not.toBe(newDeleteRequestId());
    expect(newDeleteRequestId().startsWith('del-')).toBe(true);
  });
});
