// FER-494: "Buka di terminal" — the webview sends only agent id + path + terminal preference;
// a typed question goes to the clipboard and NEVER into the request (and so never into argv).
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
  firstAttachmentFolder,
  openAgentSubmenu,
  openAgentTerminal,
  setTerminalPortForTests,
  terminalErrorText,
  type TerminalOpenRequest,
  type TerminalOpened,
} from './agentTerminal';
import { agentCli, resetAgentCliForTests } from './agentCli.svelte';
import { agentView, FakeAgentStore } from './fixtures/agentFixtures';
import { toast } from '../stores/toastStore.svelte';
import { dialogs } from '../stores/dialogStore.svelte';
import { contextMenu } from '../stores/contextMenuStore.svelte';
import { searchActions } from '../actions';
import { registerPane } from '../stores/panesRegistry.svelte';
import { ui } from '../uiState.svelte';

const QUESTION = 'Ringkas laporan ini; lalu "rm -rf" & %PATH% ^ | wt -p x';

function fakePort(result?: Partial<TerminalOpened>, fail?: unknown) {
  const calls: TerminalOpenRequest[] = [];
  setTerminalPortForTests({
    open(request) {
      calls.push(request);
      if (fail) return Promise.reject(fail);
      return Promise.resolve({
        agent: request.agent,
        via: 'windowsTerminal',
        argv: ['wt', '-d', '.', 'C:\\b\\claude.exe'],
        cwd: 'C:\\proj',
        dryRun: false,
        warnings: [],
        ...result,
      });
    },
  });
  return calls;
}

let writeText: ReturnType<typeof vi.fn>;

beforeEach(() => {
  writeText = vi.fn(() => Promise.resolve());
  Object.defineProperty(globalThis.navigator, 'clipboard', {
    value: { writeText },
    configurable: true,
  });
  dialogs.current = null;
  toast.message = '';
});

afterEach(() => {
  setTerminalPortForTests(undefined);
  resetAgentCliForTests();
  contextMenu.close();
});

describe('openAgentTerminal', () => {
  it('copies the question to the clipboard and never sends it to the backend', async () => {
    const calls = fakePort();
    const store = new FakeAgentStore();
    const out = await openAgentTerminal('claude', 'C:\\proj\\a;b\\r.pdf', {
      question: QUESTION,
      store,
    });
    expect(out?.argv).toEqual(['wt', '-d', '.', 'C:\\b\\claude.exe']);
    expect(writeText).toHaveBeenCalledWith(QUESTION.trim());
    expect(calls).toEqual([{ agent: 'claude', path: 'C:\\proj\\a;b\\r.pdf', program: 'auto' }]);
    // Nothing of the question reaches the request (and so the command line).
    expect(Object.keys(calls[0]).sort()).toEqual(['agent', 'path', 'program']);
    expect(JSON.stringify(calls)).not.toContain('Ringkas');
    expect(toast.message).toContain('Pertanyaan disalin — tempel di Claude Code (Ctrl+V)');
  });

  it('without a question nothing is copied; the agent note and extra note are shown', async () => {
    fakePort({ note: 'Codex dapat membaca file lain di PC ini.' });
    await openAgentTerminal('codex', 'C:\\proj', {
      store: new FakeAgentStore(),
      extraNote: 'Hanya folder lampiran pertama yang dibuka.',
    });
    expect(writeText).not.toHaveBeenCalled();
    expect(toast.message).toContain('Codex dibuka di terminal.');
    expect(toast.message).toContain('Codex dapat membaca file lain');
    expect(toast.message).toContain('Hanya folder lampiran pertama');
  });

  it('a test-profile dry run says nothing was started', async () => {
    fakePort({ dryRun: true });
    await openAgentTerminal('claude', 'C:\\proj', { store: new FakeAgentStore() });
    expect(toast.message).toContain('Profil uji: Claude Code tidak dijalankan.');
  });

  it('backend refusals become an error dialog with the reason', async () => {
    fakePort(undefined, { kind: 'agent_untrusted', message: 'folder dapat ditulis Everyone' });
    const out = await openAgentTerminal('omp', 'C:\\proj', { store: new FakeAgentStore() });
    expect(out).toBeUndefined();
    expect(dialogs.current).toMatchObject({
      kind: 'error',
      title: 'Buka omp',
      message: 'omp tidak dijalankan: folder dapat ditulis Everyone.',
    });
    expect(terminalErrorText('folder_invalid', 'x')).toContain('Folder ini tidak bisa dipakai');
    expect(terminalErrorText('agent_unavailable', 'Aider', 'tidak ditemukan')).toBe(
      'Aider tidak bisa dibuka di terminal: tidak ditemukan.',
    );
  });
});

describe('untrusted folder confirmation (FER-516)', () => {
  it('sends no confirmation flag: the backend raises its own native dialog', async () => {
    const calls: TerminalOpenRequest[] = [];
    const warning =
      'Aider akan membaca C:/proj/.aider.conf.yml: bisa menjalankan perintah otomatis.';
    setTerminalPortForTests({
      open(request) {
        calls.push(request);
        return Promise.resolve({
          agent: request.agent,
          via: 'windowsTerminal',
          argv: ['wt', '-d', '.', 'C:/b/aider.exe'],
          cwd: 'C:/proj',
          dryRun: false,
          warnings: [warning],
        });
      },
    });
    await openAgentTerminal('aider', 'C:/proj', {
      question: 'tolong',
      store: new FakeAgentStore(),
    });
    expect(calls).toEqual([{ agent: 'aider', path: 'C:/proj', program: 'auto' }]);
    expect(Object.keys(calls[0]).sort()).toEqual(['agent', 'path', 'program']);
    // No webview-drawn confirmation any more.
    expect(dialogs.current).toBeNull();
    expect(writeText).toHaveBeenCalledTimes(1);
    expect(toast.message).toContain('Aider dibuka di terminal.');
    expect(toast.message).toContain('Pertanyaan disalin');
  });

  it('"cancelled" (native dialog answered No) is a neutral note, not an error dialog', async () => {
    fakePort(undefined, { kind: 'cancelled' });
    const out = await openAgentTerminal('aider', 'C:/proj', { store: new FakeAgentStore() });
    expect(out).toBeUndefined();
    expect(dialogs.current).toBeNull();
    expect(toast.message).toBe('Aider tidak dibuka.');
  });

  it('"busy" says another confirmation is still open', async () => {
    fakePort(undefined, { kind: 'busy' });
    const out = await openAgentTerminal('aider', 'C:/proj', { store: new FakeAgentStore() });
    expect(out).toBeUndefined();
    expect(dialogs.current).toBeNull();
    expect(toast.message).toBe('Konfirmasi lain masih terbuka. Selesaikan dulu, lalu coba lagi.');
    expect(terminalErrorText('busy', 'x')).toContain('Konfirmasi lain masih terbuka');
  });

  it('Claude/Codex only get the warnings as a note', async () => {
    fakePort({ warnings: ['Claude Code akan membaca C:/p/.mcp.json: server MCP.'] });
    await openAgentTerminal('claude', 'C:/p', { store: new FakeAgentStore() });
    expect(dialogs.current).toBeNull();
    expect(toast.message).toContain('Catatan: Claude Code akan membaca');
  });
});

describe('context submenu', () => {
  it('lists only the agents that can open in a terminal, at the old menu position', async () => {
    const calls = fakePort();
    const store = new FakeAgentStore();
    const owner = {};
    contextMenu.open(40, 50, [{ label: 'x', run: () => undefined }], owner);
    contextMenu.close();
    await openAgentSubmenu('C:\\proj\\notes.txt', store);
    expect(contextMenu.isOpen).toBe(true);
    expect(contextMenu.owner).toBe(owner);
    const labels = contextMenu.items.map((i) => ('label' in i ? i.label : '-'));
    expect(labels).toEqual(['Buka di agent:', 'Claude Code', 'Codex', 'omp']);
    const omp = contextMenu.items[3];
    if ('run' in omp) omp.run();
    await Promise.resolve();
    expect(calls).toEqual([{ agent: 'omp', path: 'C:\\proj\\notes.txt', program: 'auto' }]);
  });

  it('says so when no agent is available', async () => {
    const store = new FakeAgentStore(
      agentView({ agents: agentView().agents.map((a) => ({ ...a, terminal: false })) }),
    );
    await openAgentSubmenu('C:\\proj', store);
    expect(contextMenu.isOpen).toBe(false);
    expect(toast.message).toContain('Tidak ada agent yang bisa dibuka di terminal');
  });
});

describe('composer folder', () => {
  it('uses the first attachment folder (a file means its parent) and flags other folders', () => {
    expect(firstAttachmentFolder([])).toEqual({ folder: undefined, others: false });
    expect(
      firstAttachmentFolder([
        { path: 'C:\\a;b\\r.pdf', isDir: false },
        { path: 'C:\\a;b\\s.pdf', isDir: false },
      ]),
    ).toEqual({ folder: 'C:\\a;b', others: false });
    expect(
      firstAttachmentFolder([
        { path: 'C:\\r.pdf', isDir: false },
        { path: 'D:\\docs', isDir: true },
      ]),
    ).toEqual({ folder: 'C:\\', others: true });
  });
});

describe('palette', () => {
  it('offers one "Buka <Agent> di sini" per terminal-ready agent with Indonesian keywords', () => {
    agentCli.view = agentView();
    const off = registerPane(0, {
      path: 'C:\\proj',
      selectedPaths: () => [],
      cursorEntry: () => undefined,
    } as never);
    ui.activePane = 0;
    try {
      const ids = (q: string) => searchActions(q).map((a) => a.id);
      expect(ids('claude')).toContain('ai.agentTerminal.claude');
      expect(ids('terminal ai')).toEqual(
        expect.arrayContaining([
          'ai.agentTerminal.claude',
          'ai.agentTerminal.codex',
          'ai.agentTerminal.omp',
        ]),
      );
      // Not found / ACL failed -> not offered.
      expect(ids('terminal ai')).not.toContain('ai.agentTerminal.antigravity');
      expect(ids('opencode')).not.toContain('ai.agentTerminal.opencode');
      const claude = searchActions('Buka Claude Code di sini')[0];
      expect(claude.id).toBe('ai.agentTerminal.claude');
      const calls = fakePort();
      claude.run?.();
      expect(calls[0]).toEqual({
        agent: 'claude',
        path: 'C:\\proj',
        program: 'auto',
      });
    } finally {
      off();
    }
  });
});
