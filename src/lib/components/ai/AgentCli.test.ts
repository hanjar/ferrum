// FER-490: Settings → AI → Agent CLI and the agent parts of the chat panel.
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { tick } from 'svelte';
import AgentCliSettings from './AgentCliSettings.svelte';
import ChatPanel from './ChatPanel.svelte';
import { ChatStore } from '../../ai/chatStore.svelte';
import { FakeExecutor } from '../../ai/fixtures/chatFixtures';
import { FakeAsker } from '../../ai/fixtures/attachmentFixtures';
import { FakeAgentAsker, FakeAgentStore, agentView } from '../../ai/fixtures/agentFixtures';
import { agentCli, resetAgentCliForTests } from '../../ai/agentCli.svelte';
import { resetDisclosuresForTests, type AttachmentInfo } from '../../ai/attachments';
import { setTerminalPortForTests, type TerminalOpenRequest } from '../../ai/agentTerminal';
import { toast } from '../../stores/toastStore.svelte';

beforeEach(() => resetDisclosuresForTests());
afterEach(() => {
  cleanup();
  resetAgentCliForTests();
});

describe('AgentCliSettings (FER-490)', () => {
  it('lists every agent with full path, version, status and read-only mode', async () => {
    const store = new FakeAgentStore();
    render(AgentCliSettings, { store });
    await tick();
    const rows = screen.getAllByTestId('agent-row');
    expect(rows.map((r) => r.dataset.agent)).toEqual([
      'claude',
      'codex',
      'omp',
      'antigravity',
      'opencode',
    ]);
    expect(rows[0].textContent).toContain(
      'C:\\npm\\node_modules\\@anthropic-ai\\claude-code\\bin\\claude.exe',
    );
    expect(rows[0].textContent).toContain('2.1.295');
    expect(rows[0].textContent).toContain('didukung');
    expect(rows[0].textContent).toContain('Mode baca-saja: claude -p');
    expect(rows[2].textContent).toContain('tidak didukung: tidak ada mode baca-saja');
    expect(rows[2].textContent).not.toContain('Mode baca-saja');
    expect(rows[3].textContent).toContain('tidak ditemukan');
    expect(store.calls).toContainEqual({ cmd: 'load' });
  });

  it('FER-494: "Buka di terminal" is a separate capability with its own default', async () => {
    const store = new FakeAgentStore();
    render(AgentCliSettings, { store });
    await tick();
    const term = (i: number) =>
      screen.getAllByTestId('agent-row')[i].querySelector('[data-testid="agent-terminal-text"]')
        ?.textContent;
    expect(term(0)).toBe('tersedia');
    // omp: no read-only mode for the chat, but it can open in a terminal.
    expect(screen.getAllByTestId('agent-row')[2].textContent).toContain('tidak didukung');
    expect(term(2)).toBe('tersedia');
    expect(term(3)).toBe('tidak tersedia (tidak ditemukan di PATH)');
    expect(term(4)).toBe('tidak tersedia (folder dapat ditulis oleh Everyone)');
    expect(screen.getAllByTestId('agent-row')[1].textContent).toContain(
      'Codex dapat membaca file lain',
    );
    const sel = screen.getByTestId('agent-terminal-default') as HTMLSelectElement;
    expect(Array.from(sel.options).map((o) => o.value)).toEqual(['claude', 'codex', 'omp']);
    expect(sel.value).toBe('claude');
    await fireEvent.change(sel, { target: { value: 'omp' } });
    expect(store.calls).toContainEqual({ cmd: 'terminalDefault', agent: 'omp' });
    expect(store.terminalDefault?.id).toBe('omp');
  });

  it('default agent offers supported agents only; re-detect calls the backend', async () => {
    const store = new FakeAgentStore();
    render(AgentCliSettings, { store });
    await tick();
    const select = screen.getByTestId('agent-default') as HTMLSelectElement;
    expect([...select.options].map((o) => o.value)).toEqual(['claude', 'codex']);
    await fireEvent.change(select, { target: { value: 'codex' } });
    expect(store.calls).toContainEqual({ cmd: 'default', agent: 'codex' });
    await fireEvent.click(screen.getByTestId('agent-redetect'));
    expect(store.calls).toContainEqual({ cmd: 'detect' });
  });

  it('Codex (broad read) is OFF until the red opt-in is ticked (E2)', async () => {
    const view = agentView();
    view.agents[1] = { ...view.agents[1], enabled: false };
    const store = new FakeAgentStore(view);
    render(AgentCliSettings, { store });
    await tick();
    const codex = screen.getAllByTestId('agent-row')[1];
    expect(codex.textContent).toContain('baca luas — nonaktif');
    expect(codex.querySelector('[data-testid="agent-broad-warning"]')?.textContent).toContain(
      'Codex dapat membaca file lain di PC ini; isi bisa terkirim ke OpenAI',
    );
    const select = screen.getByTestId('agent-default') as HTMLSelectElement;
    expect([...select.options].map((o) => o.value)).toEqual(['claude']);
    const box = codex.querySelector('[data-testid="agent-enable"]') as HTMLInputElement;
    expect(box.checked).toBe(false);
    await fireEvent.click(box);
    expect(store.calls).toContainEqual({ cmd: 'enable', agent: 'codex' });
    await tick();
    expect(screen.getAllByTestId('agent-row')[1].textContent).toContain('baca luas (diaktifkan)');
    // Claude has no opt-in.
    expect(
      screen.getAllByTestId('agent-row')[0].querySelector('[data-testid="agent-enable"]'),
    ).toBeNull();
  });

  it('says when the test profile is active', async () => {
    render(AgentCliSettings, { store: new FakeAgentStore(agentView({ testProfile: true })) });
    await tick();
    expect(screen.getByTestId('agent-cli-test-profile')).toBeTruthy();
  });
});

const info = (name: string, extra: Partial<AttachmentInfo> = {}): AttachmentInfo => ({
  path: `C:\\d\\${name}`,
  name,
  kind: 'text',
  size: 2048,
  modifiedMs: 0,
  inArchive: false,
  support: 'content',
  ...extra,
});

async function mountPanel() {
  // The panel reads the shared agent store; give it a fixed view.
  agentCli.view = agentView();
  const ex = new FakeExecutor();
  const asker = new FakeAsker();
  const agentAsker = new FakeAgentAsker();
  const store = new ChatStore(ex, asker, agentAsker, agentCli);
  render(ChatPanel, { store, onclose: () => undefined, paneHint: () => ({ leftSelected: 0 }) });
  await tick();
  return { ex, asker, agentAsker, store };
}

describe('ChatPanel agent target (FER-490)', () => {
  it('offers the agent for a PDF and switches the target', async () => {
    const { asker, store } = await mountPanel();
    await store.addAttachments(asker.add(info('q3.pdf', { kind: 'pdf', support: 'metadata' })));
    await tick();
    const select = screen.getByTestId('chat-target') as HTMLSelectElement;
    expect([...select.options].map((o) => o.textContent)).toEqual([
      'AI Ferrum',
      'Agent: Claude Code',
      'Agent: Codex',
    ]);
    await fireEvent.click(screen.getByRole('button', { name: 'Tanya lewat Claude Code?' }));
    await tick();
    expect(store.target).toBe('claude');
    expect(select.value).toBe('claude');
    expect(screen.getByTestId('attach-destination').textContent).toContain(
      'dibaca sendiri oleh agent Claude Code',
    );
  });

  it('shows the disclosure once for an agent not yet acknowledged', async () => {
    const { asker, store } = await mountPanel();
    await store.addAttachments(asker.add(info('a.txt')));
    store.setTarget('codex');
    await tick();
    expect(screen.getByTestId('agent-disclosure').textContent).toContain(
      'dikirim ke layanan OpenAI',
    );
    expect(screen.getByTestId('agent-disclosure').textContent).toContain('akun Anda');
  });

  it('streams with a status line and a Batal button, then renders text only', async () => {
    const { asker, agentAsker, store } = await mountPanel();
    await store.addAttachments(asker.add(info('a.txt')));
    store.setTarget('claude');
    agentAsker.hold = true;
    agentAsker.stream = [{ kind: 'text', text: 'Sebagian <b>jawaban</b> [link](https://x.y)' }];
    store.send('apa ini?');
    await tick();
    await tick();
    expect(screen.getByTestId('agent-status').textContent).toContain('agent sedang membaca');
    expect(screen.getByTestId('answer-agent').textContent).toContain('Claude Code');
    const answer = screen.getAllByTestId('chat-answer').at(-1)!;
    expect(answer.querySelector('a, b')).toBeNull();
    expect(answer.textContent).toContain('<b>jawaban</b>');
    await fireEvent.click(screen.getByRole('button', { name: 'Batal' }));
    await tick();
    await tick();
    expect(store.messages.at(-1)?.error).toBe('Dibatalkan.');
  });
});

describe('ChatPanel "Buka di terminal" (FER-494)', () => {
  afterEach(() => setTerminalPortForTests(undefined));

  it('opens the terminal default at the first attachment; the question only goes to the clipboard', async () => {
    const calls: TerminalOpenRequest[] = [];
    setTerminalPortForTests({
      open: (request) => {
        calls.push(request);
        return Promise.resolve({
          agent: request.agent,
          via: 'windowsTerminal',
          argv: ['wt', '-d', '.', 'C:\\b\\claude.exe'],
          cwd: 'C:\\d',
          dryRun: false,
          needsConfirm: false,
          warnings: [],
        });
      },
    });
    let clip = '';
    Object.defineProperty(globalThis.navigator, 'clipboard', {
      value: { writeText: (t: string) => ((clip = t), Promise.resolve()) },
      configurable: true,
    });
    const { asker, store } = await mountPanel();
    expect(screen.queryByTestId('open-in-terminal')).toBeNull();
    await store.addAttachments(
      asker.add(info('q3.pdf', { kind: 'pdf', support: 'metadata' }), {
        ...info('other'),
        path: 'D:\\x\\other.txt',
      }),
    );
    await tick();
    // Only while an Agent target is chosen.
    expect(screen.queryByTestId('open-in-terminal')).toBeNull();
    store.setTarget('claude');
    store.draft = 'apa isi q3? & del *';
    await tick();
    const btn = screen.getByTestId('open-in-terminal');
    expect(btn.textContent).toContain('Buka di terminal (Claude Code)');
    expect(screen.getByTestId('open-in-terminal-note').textContent).toContain(
      'hanya folder pertama',
    );
    await fireEvent.click(btn);
    await tick();
    await tick();
    expect(calls).toEqual([
      { agent: 'claude', path: 'C:\\d\\q3.pdf', program: 'auto', confirmedUntrusted: false },
    ]);
    expect(JSON.stringify(calls)).not.toContain('apa isi');
    expect(clip).toBe('apa isi q3? & del *');
    expect(toast.message).toContain('Pertanyaan disalin — tempel di Claude Code (Ctrl+V)');
    expect(toast.message).toContain('Hanya folder lampiran pertama');
  });
});
