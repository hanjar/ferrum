// FER-471: the confirm card of `copy.adjust` in the chat panel. The "Terapkan" click is the approval;
// all text is app-built.
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { tick } from 'svelte';
import ChatPanel from './ChatPanel.svelte';
import { ChatStore } from '../../ai/chatStore.svelte';
import { FakeExecutor } from '../../ai/fixtures/chatFixtures';

afterEach(cleanup);

const view = {
  summary: 'Naikkan thread copy: 4 → 5 (mode otomatis dimatikan).',
  job: 'Copy yang sedang berjalan (op_77): Otomatis · prioritas Normal, 4 thread aktif',
  before: { threads: 4, auto: true, priority: 'normal' as const },
  after: { threads: 5, auto: false, priority: 'normal' as const },
};

async function mount() {
  const ex = new FakeExecutor();
  const store = new ChatStore(ex);
  render(ChatPanel, { store, onclose: vi.fn() });
  store.send('copy-nya lebih cepat dong');
  await tick();
  const runId = store.runs.at(-1)!.runId;
  const requestId = ex.submitted.at(-1)!.requestId;
  return { ex, store, runId, requestId };
}

describe('CopyAdjustCard', () => {
  it('shows the app-built change and only Terapkan approves', async () => {
    const { ex, runId, requestId } = await mount();
    ex.emit({ runId, requestId, state: 'awaitingApproval', copyAdjust: view });
    await tick();
    expect(screen.getByTestId('copy-adjust-summary').textContent).toContain('4 → 5');
    expect(screen.getByTestId('copy-adjust-job').textContent).toContain('op_77');
    expect(screen.getByTestId('copy-adjust-change').textContent).toContain('Otomatis');
    expect(ex.approved).toEqual([]);
    await fireEvent.click(screen.getByTestId('copy-adjust-apply'));
    expect(ex.approved).toEqual([runId]);
    // A second click cannot approve twice.
    await fireEvent.click(screen.getByTestId('copy-adjust-apply'));
    expect(ex.approved).toEqual([runId]);
  });

  it('Batal cancels without approving', async () => {
    const { ex, runId, requestId } = await mount();
    ex.emit({ runId, requestId, state: 'awaitingApproval', copyAdjust: view });
    await tick();
    await fireEvent.click(screen.getByRole('button', { name: 'Batal' }));
    expect(ex.cancelled).toEqual([runId]);
    expect(ex.approved).toEqual([]);
  });

  it('shows the confirmed result after the change was applied', async () => {
    const { ex, runId, requestId } = await mount();
    ex.emit({ runId, requestId, state: 'awaitingApproval', copyAdjust: view });
    ex.emit({
      runId,
      requestId,
      state: 'succeeded',
      copyAdjust: {
        ...view,
        result: {
          ok: true,
          message: 'Diterapkan ke copy yang sedang berjalan.',
          applied: view.after,
        },
      },
    });
    await tick();
    expect(screen.getByTestId('copy-adjust-result').textContent).toContain('Diterapkan');
    expect(screen.getByTestId('copy-adjust-result').textContent).toContain('5 thread');
    expect(screen.queryByTestId('copy-adjust-apply')).toBeNull();
  });

  it('a failed apply is announced as an alert', async () => {
    const { ex, runId, requestId } = await mount();
    ex.emit({ runId, requestId, state: 'awaitingApproval', copyAdjust: view });
    ex.emit({
      runId,
      requestId,
      state: 'failed',
      errorMessage: 'Copy itu sudah selesai',
      copyAdjust: {
        ...view,
        result: { ok: false, message: 'Copy sudah selesai; tidak ada yang diubah.' },
      },
    });
    await tick();
    expect(screen.getByRole('alert').textContent).toContain('tidak ada yang diubah');
  });
});
