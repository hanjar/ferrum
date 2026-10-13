import { describe, expect, it } from 'vitest';
import { ChatStore, statusLabel, isTerminal, jobsProgress } from './chatStore.svelte';
import { FakeExecutor, samplePlan } from './fixtures/chatFixtures';

function setup() {
  const ex = new FakeExecutor();
  const store = new ChatStore(ex);
  return { ex, store };
}
function start(store: ChatStore, ex: FakeExecutor, text = 'zip left') {
  store.send(text);
  const { requestId } = ex.submitted.at(-1)!;
  const runId = store.runs.at(-1)!.runId;
  return { requestId, runId };
}

describe('ChatStore', () => {
  it('ignores blank input and submits trimmed text with an opaque request id', () => {
    const { ex, store } = setup();
    store.send('   ');
    expect(ex.submitted).toHaveLength(0);
    store.send('  zip left  ');
    expect(ex.submitted[0].text).toBe('zip left');
    expect(ex.submitted[0].requestId).toMatch(/^[A-Za-z0-9_-]{1,128}$/);
    expect(store.messages.map((m) => m.role)).toEqual(['user', 'run']);
    expect(store.runs[0].state).toBe('planning');
    expect(store.busy).toBe(true);
  });

  it('labels come from executor state only, never from model text', () => {
    const { ex, store } = setup();
    const { requestId, runId } = start(store, ex);
    ex.emit({ runId, requestId, state: 'running', modelText: 'Done! All finished successfully.' });
    expect(store.runs[0].label).toBe(statusLabel('running'));
    expect(store.runs[0].label).not.toMatch(/done/i);
    expect(store.runs[0].modelText).toBe('Done! All finished successfully.');
  });

  it('terminal states are final: late updates after cancel are dropped', () => {
    const { ex, store } = setup();
    const { requestId, runId } = start(store, ex);
    store.cancel(runId);
    expect(ex.cancelled).toEqual([runId]);
    ex.emit({ runId, requestId, state: 'cancelled' });
    ex.emit({ runId, requestId, state: 'preview', plan: samplePlan() });
    expect(store.runs[0].state).toBe('cancelled');
    expect(store.runs[0].plan).toBeUndefined();
    expect(isTerminal('cancelled')).toBe(true);
    expect(isTerminal('running')).toBe(false);
  });

  it('local "batal" cancels the active run without calling the provider', () => {
    const { ex, store } = setup();
    const { runId } = start(store, ex);
    store.send('batal');
    expect(ex.cancelled).toEqual([runId]);
    expect(ex.submitted).toHaveLength(1);
  });

  it('updates for unknown runs or mismatched request ids are ignored', () => {
    const { ex, store } = setup();
    const { runId } = start(store, ex);
    ex.emit({ runId: 'nope', requestId: 'x', state: 'running' });
    ex.emit({ runId, requestId: 'other', state: 'running' });
    expect(store.runs[0].state).toBe('planning');
  });

  it('rejects an invalid plan payload from the executor as failed instead of rendering it', () => {
    const { ex, store } = setup();
    const { requestId, runId } = start(store, ex);
    const bad = { ...samplePlan(), risk: 'safe' } as unknown as ReturnType<typeof samplePlan>;
    ex.emit({ runId, requestId, state: 'awaitingApproval', plan: bad });
    expect(store.runs[0].state).toBe('failed');
    expect(store.runs[0].errorMessage).toMatch(/invalid/i);
  });

  it('approve fires once, only from awaitingApproval (double-click safe)', () => {
    const { ex, store } = setup();
    const { requestId, runId } = start(store, ex);
    store.approve(runId);
    expect(ex.approved).toHaveLength(0);
    ex.emit({ runId, requestId, state: 'awaitingApproval', plan: samplePlan() });
    store.approve(runId);
    store.approve(runId);
    expect(ex.approved).toEqual([runId]);
    expect(store.runs[0].canApprove).toBe(false);
  });

  it('edit asks executor to invalidate approval and refills the draft', () => {
    const { ex, store } = setup();
    const { requestId, runId } = start(store, ex, 'zip left');
    ex.emit({ runId, requestId, state: 'awaitingApproval', plan: samplePlan() });
    store.edit(runId);
    expect(ex.edited).toEqual([runId]);
    expect(store.draft).toBe('zip left');
  });

  it('blocked capability carries the executor-provided reason and ends busy', () => {
    const { ex, store } = setup();
    const { requestId, runId } = start(store, ex);
    ex.emit({
      runId,
      requestId,
      state: 'blockedCapability',
      blockedReason: 'ZIP is not available in this version.',
    });
    expect(store.runs[0].label).toBe(statusLabel('blockedCapability'));
    expect(store.runs[0].blockedReason).toContain('not available');
    expect(store.busy).toBe(false);
  });

  it('computes job progress from executor job views', () => {
    expect(jobsProgress([])).toBeNull();
    expect(
      jobsProgress([
        { jobId: 'a', state: 'running', done: 5, total: 10 },
        { jobId: 'b', state: 'queued', done: 0, total: 10 },
      ]),
    ).toBe(25);
    expect(jobsProgress([{ jobId: 'a', state: 'completed', done: 0, total: 0 }])).toBe(100);
  });

  it('paginates plan detail rows without loading everything', async () => {
    const { ex, store } = setup();
    ex.itemTotal = 100_000;
    const { requestId, runId } = start(store, ex);
    ex.emit({ runId, requestId, state: 'awaitingApproval', plan: samplePlan() });
    await store.loadItems(runId, 0);
    expect(ex.itemCalls).toEqual([[runId, 0, 50]]);
    const page = store.runs[0].items!;
    expect(page.rows).toHaveLength(50);
    expect(page.total).toBe(100_000);
    await store.loadItems(runId, 50);
    expect(store.runs[0].items!.offset).toBe(50);
    expect(store.runs[0].items!.rows[0].ref).toBe('item-50');
  });

  it('dispose unsubscribes from executor', () => {
    const { ex, store } = setup();
    const { requestId, runId } = start(store, ex);
    store.dispose();
    ex.emit({ runId, requestId, state: 'running' });
    expect(store.runs[0].state).toBe('planning');
  });
});

describe('ChatStore.adoptHistory (provider change keeps the conversation)', () => {
  it('copies messages and runs, closes unfinished runs and keeps ids unique', () => {
    const { ex, store } = setup();
    const done = start(store, ex, 'pertama');
    ex.emit({ runId: done.runId, requestId: done.requestId, state: 'succeeded' });
    const pre = start(store, ex, 'kedua');
    ex.emit({
      runId: pre.runId,
      requestId: pre.requestId,
      state: 'awaitingApproval',
      plan: samplePlan(),
    });
    store.notice('catatan');

    const next = setup();
    next.store.adoptHistory(store);
    expect(next.store.messages.map((m) => m.role)).toEqual(store.messages.map((m) => m.role));
    expect(next.store.runs.map((r) => r.state)).toEqual(['succeeded', 'cancelled']);
    expect(next.store.runs.every((r) => !r.canApprove)).toBe(true);
    expect(next.store.busy).toBe(false);
    // The old store no longer follows its executor.
    ex.emit({ runId: pre.runId, requestId: pre.requestId, state: 'succeeded' });
    expect(store.run(pre.runId)!.state).toBe('awaitingApproval');
    // New requests do not collide with adopted ids.
    next.store.send('baru');
    const ids = next.store.runs.map((r) => r.runId);
    expect(new Set(ids).size).toBe(ids.length);
  });

  it('marks a run that was already queued or running as outcome unknown', () => {
    const { ex, store } = setup();
    const r = start(store, ex, 'zip');
    ex.emit({ runId: r.runId, requestId: r.requestId, state: 'running' });
    const next = setup();
    next.store.adoptHistory(store);
    expect(next.store.runs[0].state).toBe('unknownOutcome');
    expect(next.store.runs[0].label).toBe(statusLabel('unknownOutcome'));
  });
});
