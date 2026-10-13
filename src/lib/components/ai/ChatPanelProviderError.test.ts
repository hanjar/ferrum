// FER-465: kartu galat provider (pesan + tombol) dan indikator status provider di header chat.
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import ChatPanel from './ChatPanel.svelte';
import { aiChatHost } from '../../ai/aiChatHost.svelte';
import { ChatStore } from '../../ai/chatStore.svelte';
import { FakeExecutor } from '../../ai/fixtures/chatFixtures';
import { describeProviderError } from '../../ai/providerErrors';
import { ProviderStatus } from '../../ai/providerStatus.svelte';

afterEach(() => {
  cleanup();
  aiChatHost.revealSettings = false;
});

function mount(result: { ok: true } | { ok: false; kind: string } = { ok: true }) {
  const ex = new FakeExecutor();
  const store = new ChatStore(ex);
  const test = vi.fn(async () => result);
  const status = new ProviderStatus({
    setup: () => ({ endpoint: 'http://127.0.0.1:20128/v1' }),
    test,
    now: () => Date.now(),
  });
  const r = render(ChatPanel, { store, onclose: () => {}, status });
  return { ex, store, status, test, ...r };
}

async function failRun(
  m: ReturnType<typeof mount>,
  code: Parameters<typeof describeProviderError>[0],
) {
  m.store.send('zip left');
  await tick();
  const requestId = m.ex.submitted.at(-1)!.requestId;
  const runId = m.store.runs.at(-1)!.runId;
  const view = describeProviderError(code, { hostPort: '127.0.0.1:20128' });
  m.ex.emit({
    runId,
    requestId,
    state: 'failed',
    errorMessage: view.message,
    providerError: view,
  });
  await tick();
}

describe('provider error card', () => {
  it('shows the unreachable message with Coba lagi, Buka Settings and Test connection', async () => {
    const m = mount();
    await failRun(m, 'unreachable');
    expect(screen.getByTestId('provider-error-message').textContent).toContain(
      'Server AI tidak bisa dihubungi (127.0.0.1:20128)',
    );
    expect(screen.getByRole('button', { name: 'Coba lagi' })).toBeTruthy();
    expect(screen.getByRole('button', { name: /Buka Settings/ })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Test connection' })).toBeTruthy();
  });

  it('Coba lagi sends the same request again', async () => {
    const m = mount();
    await failRun(m, 'unreachable');
    await fireEvent.click(screen.getByRole('button', { name: 'Coba lagi' }));
    expect(m.ex.submitted.map((s) => s.text)).toEqual(['zip left', 'zip left']);
  });

  it('Buka Settings opens Settings at the AI section', async () => {
    const m = mount();
    await failRun(m, 'auth');
    expect(screen.queryByRole('button', { name: 'Coba lagi' })).toBeNull();
    await fireEvent.click(screen.getByRole('button', { name: /Buka Settings/ }));
    expect(aiChatHost.revealSettings).toBe(true);
  });

  it('Test connection runs a forced check and reports the result inline', async () => {
    const m = mount({ ok: false, kind: 'unreachable' });
    await waitFor(() => expect(m.test).toHaveBeenCalledTimes(1)); // auto check on open
    await failRun(m, 'unreachable');
    await fireEvent.click(screen.getByRole('button', { name: 'Test connection' }));
    await waitFor(() => expect(m.test).toHaveBeenCalledTimes(2));
    await waitFor(() =>
      expect(screen.getByTestId('provider-test-note').textContent).toContain(
        'tidak bisa dihubungi',
      ),
    );
  });

  it('not_supported shows what can be done and no buttons', async () => {
    const m = mount();
    m.store.send('hapus semua');
    await tick();
    const requestId = m.ex.submitted.at(-1)!.requestId;
    const runId = m.store.runs.at(-1)!.runId;
    const view = describeProviderError('not_supported', { capabilities: ['archive.create'] });
    m.ex.emit({
      runId,
      requestId,
      state: 'failed',
      errorMessage: view.message,
      providerError: view,
    });
    await tick();
    expect(screen.getByTestId('provider-error-message').textContent).toContain('arsip ZIP');
    expect(screen.queryByTestId('provider-error-actions')).toBeNull();
  });
});

describe('provider status indicator', () => {
  it('is red with the reason after a failed check on open', async () => {
    mount({ ok: false, kind: 'unreachable' });
    await waitFor(() => expect(screen.getByTestId('provider-status').className).toContain('down'));
    expect(screen.getByTestId('provider-status').getAttribute('aria-label')).toContain(
      'tidak bisa dihubungi',
    );
  });

  it('is green when the check succeeds', async () => {
    mount({ ok: true });
    await waitFor(() => expect(screen.getByTestId('provider-status').className).toContain('ok'));
    expect(screen.getByTestId('provider-status').getAttribute('aria-label')).toContain(
      '127.0.0.1:20128',
    );
  });
});
