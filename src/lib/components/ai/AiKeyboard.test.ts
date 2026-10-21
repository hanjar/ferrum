// FER-577 B4: keyboard of the AI panel. Access letters, Esc, no mnemonic on the delete confirm.
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { tick } from 'svelte';
import ChatPanel from './ChatPanel.svelte';
import { ChatStore } from '../../ai/chatStore.svelte';
import { FakeExecutor, samplePlan } from '../../ai/fixtures/chatFixtures';
import { duplicateMnemonics, modalDepth, resetModalDepth } from '../../dialogKit';

afterEach(() => {
  cleanup();
  resetModalDepth();
});

async function panelWithPlan(plan = samplePlan()) {
  const ex = new FakeExecutor();
  const store = new ChatStore(ex);
  const onclose = vi.fn();
  const view = render(ChatPanel, { store, onclose });
  store.send('zip left');
  await tick();
  const runId = store.runs.at(-1)!.runId;
  ex.emit({
    runId,
    requestId: ex.submitted.at(-1)!.requestId,
    state: 'awaitingApproval',
    plan,
  });
  await tick();
  return { ex, store, onclose, runId, ...view };
}
const alt = (el: Element, key: string) => fireEvent.keyDown(el, { key, altKey: true });

describe('AI panel keyboard (FER-577 B4)', () => {
  it('has no duplicate access letters and does not count as a modal', async () => {
    const { container } = await panelWithPlan();
    expect(duplicateMnemonics(container)).toEqual([]);
    expect(modalDepth()).toBe(0);
    const letters = [...container.querySelectorAll<HTMLElement>('[data-mnemonic]')].map(
      (e) => e.dataset.mnemonic,
    );
    expect(letters).toEqual(expect.arrayContaining(['c', 'm', 'r', 'e', 'l', 's']));
  });

  it('Alt+R from the composer runs the waiting plan, Alt+L would cancel it', async () => {
    const { ex, runId } = await panelWithPlan();
    const input = screen.getByLabelText('Message to AI assistant');
    expect(document.activeElement).toBe(input);
    await alt(input, 'r');
    expect(ex.approved).toEqual([runId]);
  });

  it('Alt+M returns to the composer and Alt+C closes the panel', async () => {
    const { onclose } = await panelWithPlan();
    const input = screen.getByLabelText('Message to AI assistant');
    screen.getByRole('button', { name: 'Run' }).focus();
    await alt(document.activeElement!, 'm');
    expect(document.activeElement).toBe(input);
    await alt(input, 'c');
    expect(onclose).toHaveBeenCalledTimes(1);
  });

  it('Escape closes the panel and does not leak to window', async () => {
    const { onclose } = await panelWithPlan();
    const leak = vi.fn();
    window.addEventListener('keydown', leak);
    await fireEvent.keyDown(screen.getByLabelText('Message to AI assistant'), { key: 'Escape' });
    window.removeEventListener('keydown', leak);
    expect(onclose).toHaveBeenCalledTimes(1);
    expect(leak).not.toHaveBeenCalled();
  });

  it('Ctrl+Enter sends like Enter', async () => {
    const ex = new FakeExecutor();
    const store = new ChatStore(ex);
    render(ChatPanel, { store, onclose: vi.fn() });
    const input = screen.getByLabelText('Message to AI assistant') as HTMLTextAreaElement;
    await fireEvent.input(input, { target: { value: 'one' } });
    await fireEvent.keyDown(input, { key: 'Enter', ctrlKey: true });
    expect(ex.submitted.map((s) => s.text)).toEqual(['one']);
  });

  it('Alt+S sends the draft', async () => {
    const ex = new FakeExecutor();
    const store = new ChatStore(ex);
    render(ChatPanel, { store, onclose: vi.fn() });
    const input = screen.getByLabelText('Message to AI assistant') as HTMLTextAreaElement;
    await fireEvent.input(input, { target: { value: 'two' } });
    await alt(input, 's');
    expect(ex.submitted.map((s) => s.text)).toEqual(['two']);
  });

  it('after Alt+S the keyboard stays in the composer (the Send button disables itself)', async () => {
    const ex = new FakeExecutor();
    const store = new ChatStore(ex);
    render(ChatPanel, { store, onclose: vi.fn() });
    const input = screen.getByLabelText('Message to AI assistant') as HTMLTextAreaElement;
    await fireEvent.input(input, { target: { value: 'three' } });
    await alt(input, 's');
    await tick();
    expect(document.activeElement).toBe(input);
  });

  it('the delete confirm button has NO access letter, is not focused, and no Alt key approves it', async () => {
    const plan = samplePlan({
      capability: 'items.delete',
      operation: { kind: 'items.delete', sourceManifest: 'man-del', sourceDirectory: 'D:\\Work' },
      preview: { itemCount: 1, conflictCount: 0, totalBytes: 0 },
      steps: [{ id: 's1', capability: 'items.delete', dependsOn: [] }],
    });
    const { ex, container } = await panelWithPlan(plan);
    const confirm = screen.getByTestId('confirm-delete');
    expect(confirm.hasAttribute('data-mnemonic')).toBe(false);
    expect(confirm.hasAttribute('aria-keyshortcuts')).toBe(false);
    expect(document.activeElement).not.toBe(confirm);
    const input = screen.getByLabelText('Message to AI assistant');
    for (const k of 'abcdefghijklmnopqrstuvwxyz0123456789') await alt(input, k);
    await fireEvent.keyDown(input, { key: 'Enter', ctrlKey: true });
    expect(ex.approved).toEqual([]);
    // Tab reaches it (no trap): Cancel comes first, confirm is a normal tabbable button.
    expect(confirm.tabIndex).toBe(0);
    expect(container.querySelector('[data-testid="confirm-delete"][disabled]')).toBeNull();
  });
});
