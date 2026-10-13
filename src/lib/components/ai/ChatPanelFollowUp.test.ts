import { afterEach, describe, expect, it } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { tick } from 'svelte';
import ChatPanel from './ChatPanel.svelte';
import { ChatStore, type FollowUpRef, type FollowUpResult } from '../../ai/chatStore.svelte';
import { FakeExecutor, samplePlan } from '../../ai/fixtures/chatFixtures';

afterEach(cleanup);

class FollowExecutor extends FakeExecutor {
  calls: Array<{ ref: FollowUpRef; text: string }> = [];
  result: FollowUpResult = { accepted: true, revision: 2, local: true };
  followUp(_runId: string, ref: FollowUpRef, text: string): FollowUpResult {
    this.calls.push({ ref, text });
    return this.result;
  }
}

async function mount() {
  const ex = new FollowExecutor();
  const store = new ChatStore(ex);
  render(ChatPanel, { store, onclose: () => undefined });
  store.send('arsipkan kiri');
  await tick();
  const requestId = ex.submitted.at(-1)!.requestId;
  const runId = store.runs.at(-1)!.runId;
  return { ex, store, requestId, runId };
}

describe('ChatPanel follow-up', () => {
  it('lets the user type a change while the plan awaits approval and sends it bound to the shown plan', async () => {
    const { ex, store, requestId, runId } = await mount();
    ex.emit({
      runId,
      requestId,
      state: 'awaitingApproval',
      plan: samplePlan({ planId: 'plan-A' }),
      revision: 1,
    });
    await tick();
    const input = screen.getByLabelText('Message to AI assistant') as HTMLTextAreaElement;
    expect(input.placeholder).toMatch(/namanya/);
    await fireEvent.input(input, { target: { value: 'taruh di kanan' } });
    const send = screen.getByRole('button', { name: 'Send' }) as HTMLButtonElement;
    expect(send.disabled).toBe(false);
    await fireEvent.keyDown(input, { key: 'Enter' });
    expect(ex.calls).toEqual([{ ref: { planId: 'plan-A', revision: 1 }, text: 'taruh di kanan' }]);
    expect(ex.submitted).toHaveLength(1);
    expect(store.draft).toBe('');
  });

  it('shows the revision badge and the local change notes', async () => {
    const { ex, requestId, runId } = await mount();
    ex.emit({
      runId,
      requestId,
      state: 'awaitingApproval',
      plan: samplePlan({ planId: 'plan-B' }),
      revision: 2,
      revisionNotes: ['Revisi 2: Nama output: invoice-september.zip'],
    });
    await tick();
    expect(screen.getByTestId('revision').textContent).toContain('2');
    expect(screen.getByTestId('revision-notes').textContent).toContain('invoice-september.zip');
  });

  it('renders a rejection as a plain-text notice (no HTML) and keeps the job card unchanged', async () => {
    const { ex, store, requestId, runId } = await mount();
    ex.emit({ runId, requestId, state: 'running', plan: samplePlan(), revision: 1 });
    await tick();
    expect(screen.getByTestId('running-hint').textContent).toMatch(/do not alter this job/);
    ex.result = {
      accepted: false,
      code: 'running',
      message: 'Perubahan tidak mengubah job yang aktif. <img src=x onerror=alert(1)>',
    };
    store.send('namanya baru.zip');
    await tick();
    const notice = screen.getByTestId('chat-notice');
    expect(notice.textContent).toContain('<img src=x');
    expect(notice.querySelector('img')).toBeNull();
    expect(screen.getByRole('button', { name: 'Stop' })).toBeTruthy();
  });
});
