// LIMIT-4: "Periksa hasil" pada kartu unknownOutcome. Store hanya meneruskan ke executor; status baru tetap
// datang dari RunUpdate executor (rekonsiliasi ledger/queue), tidak pernah dari store sendiri.
import { describe, expect, it } from 'vitest';
import { ChatStore, type CheckResult, type RunState } from './chatStore.svelte';
import { FakeExecutor, samplePlan } from './fixtures/chatFixtures';

class CheckingExecutor extends FakeExecutor {
  checks: string[] = [];
  next: (runId: string) => Promise<CheckResult> = async () => ({
    ok: true,
    state: 'unknownOutcome',
    message: 'checked',
  });
  checkResult(runId: string): Promise<CheckResult> {
    this.checks.push(runId);
    return this.next(runId);
  }
}

function unknownRun(ex: FakeExecutor, store: ChatStore) {
  store.send('sync both');
  const { requestId } = ex.submitted.at(-1)!;
  const runId = store.runs.at(-1)!.runId;
  ex.emit({ runId, requestId, state: 'queued', plan: samplePlan() });
  ex.emit({
    runId,
    requestId,
    state: 'unknownOutcome',
    errorMessage: 'Hasil operasi tidak dapat dipastikan.',
  });
  return { runId, requestId };
}

describe('ChatStore.checkResult', () => {
  it('calls the executor once and shows its note; the state only changes through executor updates', async () => {
    const ex = new CheckingExecutor();
    const store = new ChatStore(ex);
    const { runId, requestId } = unknownRun(ex, store);
    expect(store.canCheckResult).toBe(true);
    ex.next = async (id) => {
      ex.emit({ runId: id, requestId, state: 'succeeded' });
      return { ok: true, state: 'succeeded', message: 'Status diambil dari antrean operasi.' };
    };
    await store.checkResult(runId);
    expect(ex.checks).toEqual([runId]);
    expect(store.run(runId)!.state).toBe('succeeded');
    expect(store.run(runId)!.checkNote).toBe('Status diambil dari antrean operasi.');
    expect(store.run(runId)!.checking).toBe(false);
    // succeeded is final: a late update cannot change it
    ex.emit({ runId, requestId, state: 'failed' });
    expect(store.run(runId)!.state).toBe('succeeded');
  });

  it('double click while a check is running reaches the executor once', async () => {
    const ex = new CheckingExecutor();
    const store = new ChatStore(ex);
    const { runId } = unknownRun(ex, store);
    let release!: () => void;
    ex.next = () =>
      new Promise(
        (r) => (release = () => r({ ok: true, state: 'unknownOutcome', message: 'no job' })),
      );
    const first = store.checkResult(runId);
    expect(store.run(runId)!.checking).toBe(true);
    await store.checkResult(runId);
    release();
    await first;
    expect(ex.checks).toHaveLength(1);
    expect(store.run(runId)!.state).toBe('unknownOutcome');
  });

  it('only for unknownOutcome runs; a failing executor leaves the state unknown with a note', async () => {
    const ex = new CheckingExecutor();
    const store = new ChatStore(ex);
    store.send('zip left');
    const pending = store.runs.at(-1)!.runId;
    await store.checkResult(pending);
    expect(ex.checks).toEqual([]);
    const { runId } = unknownRun(ex, store);
    ex.next = async () => {
      throw new Error('boom');
    };
    await store.checkResult(runId);
    expect(store.run(runId)!.state).toBe('unknownOutcome');
    expect(store.run(runId)!.checkNote).toMatch(/tidak dapat diperiksa/);
  });

  it('other final states still ignore late updates', () => {
    const ex = new CheckingExecutor();
    const store = new ChatStore(ex);
    for (const final of ['failed', 'partial', 'cancelled', 'stale'] as RunState[]) {
      store.send(`req ${final}`);
      const { requestId } = ex.submitted.at(-1)!;
      const runId = store.runs.at(-1)!.runId;
      ex.emit({ runId, requestId, state: final });
      ex.emit({ runId, requestId, state: 'succeeded' });
      expect(store.run(runId)!.state).toBe(final);
    }
  });

  it('without executor support there is no check action', () => {
    const store = new ChatStore(new FakeExecutor());
    expect(store.canCheckResult).toBe(false);
  });
});
