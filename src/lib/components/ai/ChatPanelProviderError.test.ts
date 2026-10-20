// FER-465: kartu galat provider (pesan + tombol) dan indikator status provider di header chat.
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import ChatPanel from './ChatPanel.svelte';
import { aiChatHost } from '../../ai/aiChatHost.svelte';
import { ChatStore } from '../../ai/chatStore.svelte';
import { FakeExecutor } from '../../ai/fixtures/chatFixtures';
import { describeProviderError } from '../../ai/providerErrors';
import { duWorkspace } from '../../diskUsage/duWorkspace.svelte';
import * as actions from '../../actions';

vi.mock('../../actions', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../actions')>()),
  runAction: vi.fn(() => true),
}));
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

  it('FER-573: shows "Provider said" (text only) under the failed message', async () => {
    const m = mount();
    m.store.send('zip left');
    await tick();
    const requestId = m.ex.submitted.at(-1)!.requestId;
    const runId = m.store.runs.at(-1)!.runId;
    const view = describeProviderError('model_not_found', {
      hostPort: '127.0.0.1:20128',
      detail: '[antigravity/gemini-4-argon-high] [404]: Requested entity was not found. <b>x</b>',
      model: 'ag/gemini-4-argon-high',
    });
    m.ex.emit({
      runId,
      requestId,
      state: 'failed',
      errorMessage: view.message,
      providerError: view,
    });
    await tick();
    const detail = screen.getByTestId('provider-error-detail');
    expect(detail.textContent).toBe(
      'Provider said: [antigravity/gemini-4-argon-high] [404]: Requested entity was not found. <b>x</b> (model ag/gemini-4-argon-high)',
    );
    expect(detail.querySelector('b')).toBeNull();
    expect(screen.getByTestId('provider-error-message').textContent).toContain(
      'tidak mengenal model',
    );
  });

  it('without a provider message there is no detail line', async () => {
    const m = mount();
    await failRun(m, 'server_error');
    expect(screen.queryByTestId('provider-error-detail')).toBeNull();
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

describe('FER-573: size question while Disk Usage is closed', () => {
  async function hintRun(m: ReturnType<typeof mount>) {
    m.store.send('berapa ukuran size semua folder dan file di panel kanan?');
    await tick();
    const requestId = m.ex.submitted.at(-1)!.requestId;
    const runId = m.store.runs.at(-1)!.runId;
    m.ex.emit({
      runId,
      requestId,
      state: 'needsInput',
      modelText:
        'Ukuran file dan folder dihitung lewat Disk Usage. Buka Disk Usage (Alt+Shift+D) di panel itu, lalu tanya lagi.',
      uiHint: 'openDiskUsage',
    });
    await tick();
  }

  it('shows the fixed note and a button that runs the Disk Usage action once', async () => {
    duWorkspace.open = false;
    vi.mocked(actions.runAction).mockClear();
    const m = mount();
    await hintRun(m);
    expect(screen.getByText(/Buka Disk Usage \(Alt\+Shift\+D\) di panel itu/)).toBeTruthy();
    await fireEvent.click(screen.getByRole('button', { name: /Buka Disk Usage/ }));
    expect(actions.runAction).toHaveBeenCalledTimes(1);
    expect(vi.mocked(actions.runAction).mock.calls[0][0].id).toBe('commands.diskUsage');
  });

  it('hides the button once Disk Usage is open (the action would only close it)', async () => {
    duWorkspace.open = true;
    try {
      const m = mount();
      await hintRun(m);
      expect(screen.queryByRole('button', { name: /Buka Disk Usage/ })).toBeNull();
    } finally {
      duWorkspace.open = false;
    }
  });
});
