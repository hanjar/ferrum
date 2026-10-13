import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { OperationProgress } from '../ipc/types';
import { PollingQueuePort } from './executorQueue';

const invokeMock = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invokeMock(...a) }));
const { backendExecutionLedger } = await import('./executorBridge');

const op = (id: string, over: Partial<OperationProgress> = {}): OperationProgress => ({
  operationId: id,
  kind: 'copy',
  state: 'queued',
  processedBytes: 0,
  processedFiles: 0,
  ...over,
});

describe('PollingQueuePort', () => {
  it('notifies only for changed jobs and exposes get()', () => {
    let rows: OperationProgress[] = [op('j1'), op('j2')];
    const cancel = vi.fn(async () => undefined);
    const port = new PollingQueuePort({ read: () => rows, cancel });
    const seen: string[] = [];
    const off = port.subscribe((p) => seen.push(`${p.operationId}:${p.state}`));
    port.tick();
    expect(seen).toEqual(['j1:queued', 'j2:queued']);
    port.tick();
    expect(seen).toHaveLength(2); // no change -> no event
    rows = [op('j1', { state: 'running', processedFiles: 2 }), op('j2')];
    port.tick();
    expect(seen.slice(2)).toEqual(['j1:running']);
    expect(port.get('j1')?.processedFiles).toBe(2);
    expect(port.get('nope')).toBeUndefined();
    off();
    rows = [op('j1', { state: 'completed' })];
    port.tick();
    expect(seen).toHaveLength(3);
  });

  it('cancel delegates to the source for the given job only', async () => {
    const cancel = vi.fn(async () => undefined);
    const port = new PollingQueuePort({ read: () => [], cancel });
    await port.cancel('job_1');
    expect(cancel).toHaveBeenCalledWith('job_1');
  });
});

describe('backend ledger bridge', () => {
  beforeEach(() => invokeMock.mockReset());

  it('sends only opaque ids and digests; maps null to undefined', async () => {
    invokeMock.mockResolvedValueOnce({ kind: 'fresh', key: 'idem_1' });
    await backendExecutionLedger.reserve('exe_1', 'plan_1', 'dig_1');
    expect(invokeMock).toHaveBeenCalledWith('ai_exec_reserve', {
      executionId: 'exe_1',
      planId: 'plan_1',
      planDigest: 'dig_1',
    });
    invokeMock.mockResolvedValueOnce(null);
    expect(await backendExecutionLedger.byExecution('exe_9')).toBeUndefined();
    invokeMock.mockResolvedValueOnce(undefined);
    await backendExecutionLedger.recordEnqueued('idem_1', ['job_1']);
    expect(invokeMock).toHaveBeenLastCalledWith('ai_exec_record_enqueued', {
      key: 'idem_1',
      jobIds: ['job_1'],
    });
  });
});
