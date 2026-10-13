import { afterEach, describe, expect, it } from 'vitest';
import { cleanup, fireEvent, render, screen, within } from '@testing-library/svelte';
import { tick } from 'svelte';
import ChatPanel from './ChatPanel.svelte';
import { ChatStore, type FollowUpRef, type FollowUpResult } from '../../ai/chatStore.svelte';
import { FakeExecutor, samplePlan } from '../../ai/fixtures/chatFixtures';
import { followUpChips, starterChips, type PaneHint } from './suggestionChips';

afterEach(cleanup);

class FollowExecutor extends FakeExecutor {
  calls: Array<{ ref: FollowUpRef; text: string }> = [];
  followUp(_runId: string, ref: FollowUpRef, text: string): FollowUpResult {
    this.calls.push({ ref, text });
    return { accepted: true, revision: 2, local: true };
  }
}

const hint = (h: Partial<PaneHint> = {}): PaneHint => ({
  leftPath: 'D:\\Projects\\Invoice',
  rightPath: 'E:\\Backup',
  leftSelected: 0,
  ...h,
});

const syncPlan = () =>
  samplePlan({
    capability: 'directories.sync',
    operation: {
      kind: 'directories.sync',
      leftRoot: 'D:\\Projects\\Invoice',
      rightRoot: 'E:\\Backup',
      leftManifest: 'man-l',
      rightManifest: 'man-r',
    },
    steps: [{ id: 's1', capability: 'directories.sync', dependsOn: [] }],
  });

function mount(paneHint: PaneHint = hint(), configured = true) {
  const ex = new FollowExecutor();
  const store = new ChatStore(ex);
  render(ChatPanel, { store, onclose: () => undefined, configured, paneHint: () => paneHint });
  return { ex, store };
}

const chipNames = (testId: string) =>
  within(screen.getByTestId(testId))
    .getAllByRole('button')
    .map((b) => b.textContent?.trim());

describe('starterChips', () => {
  it('offers sync only for two different folders and "selected" only with a selection', () => {
    expect(starterChips(hint()).map((c) => c.id)).toEqual(['zip-left', 'sync']);
    expect(starterChips(hint({ rightPath: 'd:\\projects\\invoice\\' })).map((c) => c.id)).toEqual([
      'zip-left',
    ]);
    expect(starterChips(hint({ rightPath: undefined })).map((c) => c.id)).toEqual(['zip-left']);
    expect(starterChips(hint({ leftSelected: 2 })).map((c) => c.id)).toEqual([
      'zip-left',
      'sync',
      'zip-selected',
    ]);
  });
});

describe('followUpChips', () => {
  it('is empty without a plan, after approval, and outside preview/approval', () => {
    expect(followUpChips(undefined)).toEqual([]);
    const run = { plan: samplePlan(), state: 'awaitingApproval', approvalSent: true } as never;
    expect(followUpChips(run)).toEqual([]);
    const running = { plan: samplePlan(), state: 'running', approvalSent: false } as never;
    expect(followUpChips(running)).toEqual([]);
  });

  it('offers "taruh di kiri" when the ZIP already goes to the right pane', () => {
    const plan = samplePlan({
      operation: {
        kind: 'archive.create',
        sourceManifest: 'man-1',
        targetDirectory: 'E:\\Backup',
        outputName: 'Invoice.zip',
      },
    });
    const ids = followUpChips({
      plan,
      state: 'awaitingApproval',
      approvalSent: false,
    } as never).map((c) => c.id);
    expect(ids).toEqual(['rename', 'to-left', 'only-pdf', 'cancel']);
  });
});

describe('ChatPanel suggestion chips', () => {
  it('shows starter chips in the empty state and sends the chip text', async () => {
    const { ex } = mount(hint({ leftSelected: 1 }));
    expect(chipNames('starter-chips')).toEqual([
      'Arsipkan semua file di panel kiri jadi ZIP',
      'Sinkronkan kedua panel dua arah',
      'Arsipkan item yang dipilih di panel kiri jadi ZIP',
    ]);
    await fireEvent.click(screen.getByRole('button', { name: 'Sinkronkan kedua panel dua arah' }));
    await tick();
    expect(ex.submitted.at(-1)?.text).toBe('Sinkronkan kedua panel dua arah');
    expect(screen.queryByTestId('starter-chips')).toBeNull();
  });

  it('hides starter chips while the provider is not configured', () => {
    mount(hint(), false);
    expect(screen.queryByTestId('starter-chips')).toBeNull();
  });

  it('shows follow-up chips for a ZIP plan awaiting approval and binds them to the plan', async () => {
    const { ex, store } = mount();
    expect(screen.queryByTestId('followup-chips')).toBeNull();
    store.send('arsipkan kiri');
    await tick();
    const requestId = ex.submitted.at(-1)!.requestId;
    const runId = store.runs.at(-1)!.runId;
    ex.emit({
      runId,
      requestId,
      state: 'awaitingApproval',
      plan: samplePlan({ planId: 'plan-A' }),
      revision: 1,
    });
    await tick();
    expect(chipNames('followup-chips')).toEqual([
      'namanya…',
      'taruh di kanan',
      'hanya PDF',
      'batal',
    ]);

    await fireEvent.click(screen.getByRole('button', { name: 'taruh di kanan' }));
    expect(ex.calls.at(-1)).toEqual({
      ref: { planId: 'plan-A', revision: 1 },
      text: 'taruh di kanan',
    });

    await fireEvent.click(screen.getByRole('button', { name: 'namanya…' }));
    const input = screen.getByLabelText('Message to AI assistant') as HTMLTextAreaElement;
    expect(input.value).toBe('namanya ');
    expect(document.activeElement).toBe(input);
    expect(ex.calls).toHaveLength(1);
  });

  it('offers only "batal" for a sync plan, and it cancels the run', async () => {
    const { ex, store } = mount();
    store.send('sinkronkan');
    await tick();
    const requestId = ex.submitted.at(-1)!.requestId;
    const runId = store.runs.at(-1)!.runId;
    ex.emit({ runId, requestId, state: 'awaitingApproval', plan: syncPlan(), revision: 1 });
    await tick();
    expect(chipNames('followup-chips')).toEqual(['batal']);
    await fireEvent.click(screen.getByRole('button', { name: 'batal' }));
    expect(ex.cancelled).toContain(runId);
  });
});
