// FER-476: the FerrumIndexer card in the chat panel. The "Ganti mode" click is the approval; raising
// to High performance also needs the privacy tick. All text is app-built.
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { tick } from 'svelte';
import ChatPanel from './ChatPanel.svelte';
import { ChatStore } from '../../ai/chatStore.svelte';
import { FakeExecutor } from '../../ai/fixtures/chatFixtures';
import {
  describeIndexerStatus,
  describeModeChange,
  type IndexerModeView,
} from '../../ai/indexerControl';
import { UNAVAILABLE } from '../../ipc/indexer';

afterEach(cleanup);

const raise = describeModeChange('high_performance', 'shared', false);
const lower = describeModeChange('shared', 'high_performance', true);

async function mount(text = 'pakai mode high performance') {
  const ex = new FakeExecutor();
  const store = new ChatStore(ex);
  render(ChatPanel, { store, onclose: vi.fn() });
  store.send(text);
  await tick();
  const runId = store.runs.at(-1)!.runId;
  const requestId = ex.submitted.at(-1)!.requestId;
  return { ex, store, runId, requestId };
}

describe('IndexerCard: mode change', () => {
  it('explains the privacy consequence and keeps Ganti mode disabled until the tick', async () => {
    const { ex, runId, requestId } = await mount();
    ex.emit({ runId, requestId, state: 'awaitingApproval', indexer: raise });
    await tick();
    const text = screen.getByTestId('indexer-consequences').textContent ?? '';
    expect(text).toContain('SEMUA akun');
    expect(text).toContain('satu orang');
    expect(text).toContain('UAC');
    expect(screen.getByTestId('indexer-current').textContent).toContain('Bersama');
    const apply = screen.getByTestId('indexer-apply') as HTMLButtonElement;
    expect(apply.disabled).toBe(true);
    await fireEvent.click(apply);
    expect(ex.approved).toEqual([]);

    const ack = screen.getByTestId('indexer-ack') as HTMLInputElement;
    expect(screen.getByText('Saya mengerti, PC ini hanya dipakai oleh saya.')).toBeTruthy();
    await fireEvent.click(ack);
    expect(apply.disabled).toBe(false);
    await fireEvent.click(apply);
    expect(ex.approved).toEqual([runId]);
    expect(ex.approveOptions).toEqual([{ acknowledged: true }]);
    // A second click cannot approve twice.
    await fireEvent.click(apply);
    expect(ex.approved).toEqual([runId]);
  });

  it('un-ticking disables the button again', async () => {
    const { ex, runId, requestId } = await mount();
    ex.emit({ runId, requestId, state: 'awaitingApproval', indexer: raise });
    await tick();
    const ack = screen.getByTestId('indexer-ack');
    const apply = screen.getByTestId('indexer-apply') as HTMLButtonElement;
    await fireEvent.click(ack);
    expect(apply.disabled).toBe(false);
    await fireEvent.click(ack);
    expect(apply.disabled).toBe(true);
  });

  it('lowering to Bersama shows no tick and approves with one click', async () => {
    const { ex, runId, requestId } = await mount('kembalikan ke mode shared');
    ex.emit({ runId, requestId, state: 'awaitingApproval', indexer: lower });
    await tick();
    expect(screen.queryByTestId('indexer-ack')).toBeNull();
    expect(screen.getByTestId('indexer-consequences').textContent).toContain(
      'akun Windows-nya sendiri',
    );
    await fireEvent.click(screen.getByTestId('indexer-apply'));
    expect(ex.approved).toEqual([runId]);
  });

  it('Batal cancels without approving', async () => {
    const { ex, runId, requestId } = await mount();
    ex.emit({ runId, requestId, state: 'awaitingApproval', indexer: raise });
    await tick();
    await fireEvent.click(screen.getByRole('button', { name: 'Batal' }));
    expect(ex.cancelled).toEqual([runId]);
    expect(ex.approved).toEqual([]);
  });

  it('while the change is being applied the buttons are gone and UAC is mentioned', async () => {
    const { ex, runId, requestId } = await mount();
    ex.emit({ runId, requestId, state: 'awaitingApproval', indexer: raise });
    await tick();
    await fireEvent.click(screen.getByTestId('indexer-ack'));
    await fireEvent.click(screen.getByTestId('indexer-apply'));
    await tick();
    expect(screen.queryByTestId('indexer-apply')).toBeNull();
    expect(screen.queryByRole('button', { name: 'Batal' })).toBeNull();
    expect(screen.getByTestId('indexer-applying').textContent).toContain('UAC');
  });

  it('shows the success result', async () => {
    const { ex, runId, requestId } = await mount();
    ex.emit({ runId, requestId, state: 'awaitingApproval', indexer: raise });
    const done: IndexerModeView = {
      ...raise,
      needsAck: false,
      result: { ok: true, message: 'Mode diganti ke High performance.' },
    };
    ex.emit({ runId, requestId, state: 'succeeded', indexer: done });
    await tick();
    expect(screen.getByTestId('indexer-result').textContent).toContain('High performance');
    expect(screen.queryByTestId('indexer-apply')).toBeNull();
    expect(screen.queryByTestId('indexer-consequences')).toBeNull();
  });

  it('a failed change (UAC declined) is announced as an alert', async () => {
    const { ex, runId, requestId } = await mount();
    ex.emit({ runId, requestId, state: 'awaitingApproval', indexer: raise });
    ex.emit({
      runId,
      requestId,
      state: 'failed',
      errorMessage: 'x',
      indexer: {
        ...raise,
        needsAck: false,
        result: {
          ok: false,
          code: 'declined',
          message: 'Persetujuan administrator (UAC) ditolak. Mode tidak berubah.',
        },
      },
    });
    await tick();
    expect(screen.getByRole('alert').textContent).toContain('Mode tidak berubah');
  });
});

describe('IndexerCard: status answer', () => {
  it('shows readiness, per-volume progress and mode without buttons or paths', async () => {
    const { ex, runId, requestId } = await mount('indeksnya sudah siap?');
    const view = describeIndexerStatus({
      ...UNAVAILABLE,
      installed: true,
      reachable: true,
      scmState: 'running',
      mode: 'shared',
      index: 'building',
      problem: null,
      volumes: [
        {
          letter: 'C',
          state: 'building',
          error: null,
          entries: 1,
          progressPermille: 420,
          live: false,
        },
        {
          letter: 'D',
          state: 'ready',
          error: null,
          entries: 5,
          progressPermille: 1000,
          live: true,
        },
      ],
    });
    ex.emit({ runId, requestId, state: 'succeeded', indexer: view });
    await tick();
    expect(screen.getByTestId('indexer-readiness').textContent).toBe('Sedang disusun');
    expect(screen.getByTestId('indexer-summary').textContent).toContain('belum siap');
    expect(screen.getByTestId('indexer-mode-line').textContent).toContain('Bersama');
    const volumes = screen.getByTestId('indexer-volumes').textContent ?? '';
    expect(volumes).toContain('C:');
    expect(volumes).toContain('42%');
    expect(volumes).toContain('D:');
    expect(screen.queryByTestId('indexer-apply')).toBeNull();
  });

  it('reports the not-installed state', async () => {
    const { ex, runId, requestId } = await mount('indeksnya sudah siap?');
    ex.emit({
      runId,
      requestId,
      state: 'succeeded',
      indexer: describeIndexerStatus({ ...UNAVAILABLE, problem: null }),
    });
    await tick();
    expect(screen.getByTestId('indexer-readiness').textContent).toBe('Belum terpasang');
    expect(screen.queryByTestId('indexer-volumes')).toBeNull();
  });
});
