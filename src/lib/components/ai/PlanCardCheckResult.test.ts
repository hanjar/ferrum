// LIMIT-4: kartu unknownOutcome menawarkan "Check result" yang memanggil executor (rekonsiliasi), bukan replay.
import { afterEach, describe, expect, it } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { tick } from 'svelte';
import PlanCard from './PlanCard.svelte';
import {
  ChatStore,
  type CheckResult,
  type RunState,
  type RunView,
} from '../../ai/chatStore.svelte';
import { FakeExecutor, samplePlan } from '../../ai/fixtures/chatFixtures';

class CheckingExecutor extends FakeExecutor {
  checks: string[] = [];
  result: CheckResult = {
    ok: true,
    state: 'unknownOutcome',
    message: 'No queue job was recorded for this run.',
  };
  async checkResult(runId: string): Promise<CheckResult> {
    this.checks.push(runId);
    return this.result;
  }
}

async function mount(state: RunState, ex: FakeExecutor = new CheckingExecutor()) {
  const store = new ChatStore(ex);
  store.send('zip left');
  const runId = store.runs.at(-1)!.runId;
  const requestId = ex.submitted.at(-1)!.requestId;
  ex.emit({ runId, requestId, state: 'queued', plan: samplePlan() });
  ex.emit({ runId, requestId, state });
  await tick();
  render(PlanCard, { run: store.runs.at(-1) as RunView, store });
  return { store, runId };
}

afterEach(() => cleanup());

describe('PlanCard: Check result', () => {
  it('unknownOutcome card offers Check result; clicking reconciles via the executor and shows the note', async () => {
    const ex = new CheckingExecutor();
    const { runId } = await mount('unknownOutcome', ex);
    const button = screen.getByTestId('check-result');
    expect(button.textContent).toMatch(/Check result/);
    await fireEvent.click(button);
    await tick();
    await tick();
    expect(ex.checks).toEqual([runId]);
    expect(screen.getByTestId('check-note').textContent).toMatch(/No queue job/);
    expect(ex.approved).toEqual([]); // no replay path
  });

  it('is not offered on other final states', async () => {
    for (const state of ['failed', 'succeeded', 'partial'] as RunState[]) {
      await mount(state);
      expect(screen.queryByTestId('check-result')).toBeNull();
      cleanup();
    }
  });

  it('is not offered when the executor cannot check', async () => {
    await mount('unknownOutcome', new FakeExecutor());
    expect(screen.queryByTestId('check-result')).toBeNull();
  });
});
