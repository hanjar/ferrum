// FER-196/198: semantik ledger (double submit, crash boundary, reconcile) pada implementasi memori
// yang meniru backend Rust (executor.rs).
import { describe, expect, it } from 'vitest';
import { LedgerError, MemoryExecutionLedger, idempotencyKey, ownedJobIds } from './executorLedger';

const fresh = async (l: MemoryExecutionLedger, e = 'exe_1', p = 'plan_1', d = 'd1') => {
  const r = await l.reserve(e, p, d);
  if (r.kind !== 'fresh') throw new Error('expected fresh');
  return r.key;
};

describe('MemoryExecutionLedger', () => {
  it('key is bound to execution id and plan digest', () => {
    expect(idempotencyKey('e1', 'd1')).toBe(idempotencyKey('e1', 'd1'));
    expect(idempotencyKey('e1', 'd1')).not.toBe(idempotencyKey('e2', 'd1'));
    expect(idempotencyKey('e1', 'd1')).not.toBe(idempotencyKey('e1', 'd2'));
  });

  it('double submit: second reserve is existing and carries the job mapping', async () => {
    const l = new MemoryExecutionLedger();
    const key = await fresh(l);
    await l.recordEnqueued(key, ['job_1']);
    const again = await l.reserve('exe_1', 'plan_1', 'd1');
    expect(again).toMatchObject({
      kind: 'existing',
      entry: { state: 'enqueued', jobIds: ['job_1'] },
    });
  });

  it('a new execution id for the same plan (provider retry / reconnect) gets existing', async () => {
    const l = new MemoryExecutionLedger();
    await fresh(l);
    expect((await l.reserve('exe_2', 'plan_1', 'd1')).kind).toBe('existing');
  });

  it('concurrent reserves yield exactly one fresh', async () => {
    const l = new MemoryExecutionLedger();
    const all = await Promise.all(
      Array.from({ length: 8 }, () => l.reserve('exe_1', 'plan_1', 'd1')),
    );
    expect(all.filter((r) => r.kind === 'fresh')).toHaveLength(1);
  });

  it('crash between enqueue and receipt: restart yields unknownOutcome, never replayed', async () => {
    const l = new MemoryExecutionLedger();
    await fresh(l); // enqueue happened (or not) but record_enqueued never ran
    const restarted = MemoryExecutionLedger.restart(l.persisted());
    expect((await restarted.unresolved()).map((e) => e.state)).toEqual(['unknownOutcome']);
    expect((await restarted.reserve('exe_1', 'plan_1', 'd1')).kind).toBe('existing');
    expect((await restarted.reserve('exe_9', 'plan_1', 'd1')).kind).toBe('existing');
    expect(await ownedJobIds(restarted, 'exe_1')).toEqual([]); // owns nothing until reconciled
  });

  it('reconcile: found adopts jobs, notFound releases the plan', async () => {
    const l = MemoryExecutionLedger.restart(
      (() => {
        const seed = new MemoryExecutionLedger();
        void seed;
        return {
          entries: [
            {
              key: idempotencyKey('exe_1', 'd1'),
              executionId: 'exe_1',
              planId: 'plan_1',
              planDigest: 'd1',
              state: 'reserved' as const,
              jobIds: [],
            },
            {
              key: idempotencyKey('exe_2', 'd2'),
              executionId: 'exe_2',
              planId: 'plan_2',
              planDigest: 'd2',
              state: 'reserved' as const,
              jobIds: [],
            },
          ],
        };
      })(),
    );
    const found = await l.resolveUnknown(idempotencyKey('exe_1', 'd1'), {
      kind: 'found',
      jobIds: ['job_7'],
    });
    expect(found.state).toBe('enqueued');
    expect(await ownedJobIds(l, 'exe_1')).toEqual(['job_7']);
    const nf = await l.resolveUnknown(idempotencyKey('exe_2', 'd2'), { kind: 'notFound' });
    expect(nf.state).toBe('notStarted');
    expect((await l.reserve('exe_3', 'plan_2', 'd2')).kind).toBe('fresh');
    await expect(
      l.resolveUnknown(idempotencyKey('exe_1', 'd1'), { kind: 'notFound' }),
    ).rejects.toMatchObject({
      code: 'bad_state',
    });
  });

  it('notStarted (safe failure) frees the plan; markUnknown blocks replay', async () => {
    const l = new MemoryExecutionLedger();
    const k = await fresh(l);
    await l.recordNotStarted(k);
    expect((await l.reserve('exe_1', 'plan_1', 'd1')).kind).toBe('fresh');
    const l2 = new MemoryExecutionLedger();
    const k2 = await fresh(l2);
    await l2.markUnknown(k2);
    expect((await l2.reserve('exe_1', 'plan_1', 'd1')).kind).toBe('existing');
    await l2.recordEnqueued(k2, ['job_1']); // late receipt is accepted
    expect(await ownedJobIds(l2, 'exe_1')).toEqual(['job_1']);
  });

  it('ownership: only the execution that enqueued a job owns it', async () => {
    const l = new MemoryExecutionLedger();
    await l.recordEnqueued(await fresh(l, 'exe_1', 'plan_1', 'd1'), ['job_1']);
    await l.recordEnqueued(await fresh(l, 'exe_2', 'plan_2', 'd2'), ['job_2']);
    expect(await ownedJobIds(l, 'exe_1')).toEqual(['job_1']);
    expect(await ownedJobIds(l, 'exe_unknown')).toEqual([]);
  });

  it('rejects invalid references and unknown keys', async () => {
    const l = new MemoryExecutionLedger();
    await expect(l.reserve('a b', 'plan_1', 'd1')).rejects.toBeInstanceOf(LedgerError);
    await expect(l.recordEnqueued('nope', [])).rejects.toMatchObject({ code: 'unknown_key' });
    const k = await fresh(l);
    await expect(l.recordEnqueued(k, ['bad id'])).rejects.toMatchObject({
      code: 'invalid_reference',
    });
  });
});
