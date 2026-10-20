// FER-576: a finished chain step hands over to the next one; the store adds the card for it.
import { describe, expect, it } from 'vitest';
import { ChatStore } from './chatStore.svelte';
import { FakeExecutor } from './fixtures/chatFixtures';

function setup() {
  const ex = new FakeExecutor();
  const store = new ChatStore(ex);
  store.send('arsipkan D:\\x\\a.csv lalu hapus');
  const { requestId } = ex.submitted.at(-1)!;
  const runId = store.runs.at(-1)!.runId;
  return { ex, store, requestId, runId };
}

describe('ChatStore: chain hand-over', () => {
  it('adds a card for the next step when a step succeeds with chainNext', () => {
    const { ex, store, requestId, runId } = setup();
    ex.emit({
      runId,
      requestId,
      state: 'succeeded',
      navigate: {
        summary: 'Membuka folder D:\\x dan memilih a.csv.',
        result: { ok: true, message: 'Folder dibuka di panel.' },
      },
      chain: { index: 0, total: 3 },
      chainNext: { runId: `${runId}-c1`, requestId: `${requestId}-c1`, userText: 'arsipkan' },
    });
    expect(store.runs.map((r) => r.runId)).toEqual([runId, `${runId}-c1`]);
    expect(store.runs[0].navigate?.summary).toContain('a.csv');
    expect(store.runs[0].chain).toEqual({ index: 0, total: 3 });
    expect(store.runs[1].state).toBe('planning');
    expect(store.messages.at(-1)).toMatchObject({ role: 'run', runId: `${runId}-c1` });
    // Updates for the new card are accepted like any other run's.
    ex.emit({ runId: `${runId}-c1`, requestId: `${requestId}-c1`, state: 'resolving' });
    expect(store.runs[1].state).toBe('resolving');
  });

  it('adds the card only once, and never for a step that did not succeed', () => {
    const { ex, store, requestId, runId } = setup();
    const next = { runId: `${runId}-c1`, requestId: `${requestId}-c1`, userText: 'x' };
    ex.emit({ runId, requestId, state: 'succeeded', chainNext: next });
    // A late duplicate: the first run is terminal, so it is dropped.
    ex.emit({ runId, requestId, state: 'succeeded', chainNext: next });
    expect(store.runs).toHaveLength(2);
    const other = setup();
    other.ex.emit({ runId: other.runId, requestId: other.requestId, state: 'failed' });
    expect(other.store.runs).toHaveLength(1);
  });

  it('keeps the chain step info and note on the card', () => {
    const { ex, store, requestId, runId } = setup();
    ex.emit({
      runId,
      requestId,
      state: 'awaitingApproval',
      chain: { index: 2, total: 3, note: 'Arsip lolos uji.' },
    });
    expect(store.runs[0].chain).toEqual({ index: 2, total: 3, note: 'Arsip lolos uji.' });
  });
});
