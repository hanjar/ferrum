// FER-490: fake agent CLI port + store for ChatStore/ChatPanel/Settings tests.
import {
  AgentAskError,
  AgentCliStore,
  type AgentAskInput,
  type AgentAsker,
  type AgentCliView,
  type AgentId,
  type AgentOutcome,
} from '../agentCli.svelte';

export function agentView(overrides: Partial<AgentCliView> = {}): AgentCliView {
  return {
    agents: [
      {
        id: 'claude',
        name: 'Claude Code',
        vendor: 'Anthropic',
        status: 'supported',
        path: 'C:\\npm\\node_modules\\@anthropic-ai\\claude-code\\bin\\claude.exe',
        version: '2.1.295 (Claude Code)',
        mode: 'claude -p (headless), tool hanya Read/Grep/Glob',
        acknowledged: true,
        broadRead: false,
        enabled: true,
        terminal: true,
      },
      {
        id: 'codex',
        name: 'Codex',
        vendor: 'OpenAI',
        status: 'supported',
        path: 'C:\\npm\\codex.exe',
        version: 'codex-cli 0.159.1',
        mode: 'codex exec --sandbox read-only',
        acknowledged: false,
        broadRead: true,
        enabled: true,
        terminal: true,
        terminalNote: 'Codex dapat membaca file lain di PC ini.',
      },
      {
        id: 'omp',
        name: 'omp',
        vendor: 'x',
        status: 'unsupported',
        path: 'C:\\L\\omp\\omp.exe',
        reason: 'tidak didukung: tidak ada mode baca-saja.',
        acknowledged: false,
        broadRead: false,
        enabled: false,
        terminal: true,
      },
      {
        id: 'antigravity',
        name: 'Antigravity',
        vendor: 'Google',
        status: 'notFound',
        reason: 'tidak ditemukan di PATH',
        acknowledged: false,
        broadRead: false,
        enabled: false,
        terminal: false,
        terminalReason: 'tidak ditemukan di PATH',
      },
      {
        id: 'opencode',
        name: 'opencode',
        vendor: 'x',
        status: 'unsupported',
        path: 'C:\\Users\\u\\Links\\opencode.exe',
        reason: 'tidak didukung untuk baca-saja',
        acknowledged: false,
        broadRead: false,
        enabled: false,
        terminal: false,
        terminalReason: 'folder dapat ditulis oleh Everyone',
      },
    ],
    defaultAgent: 'claude',
    detectedAtMs: 1,
    testProfile: false,
    ...overrides,
  };
}

/** Store with a fixed view; commands are recorded instead of invoked. */
export class FakeAgentStore extends AgentCliStore {
  calls: { cmd: string; agent?: AgentId }[] = [];
  constructor(view: AgentCliView = agentView()) {
    super();
    this.view = view;
  }
  override load(): Promise<void> {
    this.calls.push({ cmd: 'load' });
    return Promise.resolve();
  }
  override redetect(): Promise<void> {
    this.calls.push({ cmd: 'detect' });
    return Promise.resolve();
  }
  override setDefault(id: AgentId): Promise<void> {
    this.calls.push({ cmd: 'default', agent: id });
    if (this.view) this.view = { ...this.view, defaultAgent: id };
    return Promise.resolve();
  }
  override setEnabled(id: AgentId, enabled: boolean): Promise<void> {
    this.calls.push({ cmd: enabled ? 'enable' : 'disable', agent: id });
    if (this.view)
      this.view = {
        ...this.view,
        agents: this.view.agents.map((a) => (a.id === id ? { ...a, enabled } : a)),
      };
    return Promise.resolve();
  }
  override setTerminalDefault(id: AgentId): Promise<void> {
    this.calls.push({ cmd: 'terminalDefault', agent: id });
    if (this.view) this.view = { ...this.view, terminalDefault: id };
    return Promise.resolve();
  }
  override acknowledge(id: AgentId): Promise<void> {
    this.calls.push({ cmd: 'ack', agent: id });
    if (this.view)
      this.view = {
        ...this.view,
        agents: this.view.agents.map((a) => (a.id === id ? { ...a, acknowledged: true } : a)),
      };
    return Promise.resolve();
  }
}

export class FakeAgentAsker implements AgentAsker {
  asks: AgentAskInput[] = [];
  reply: AgentOutcome | Error = {
    agent: 'claude',
    answer: 'Ini laporan Q3.',
    truncated: false,
    skipped: [],
  };
  /** Events emitted before the reply. */
  stream: { kind: 'text' | 'status'; text: string }[] = [];
  hold = false;
  pending: { resolve: (v: AgentOutcome) => void; reject: (e: unknown) => void } | null = null;
  constructor(private readonly store?: FakeAgentStore) {}
  ask(input: AgentAskInput): Promise<AgentOutcome> {
    this.asks.push(input);
    for (const e of this.stream) input.onEvent(e.kind, e.text);
    if (this.hold)
      return new Promise((resolve, reject) => {
        this.pending = { resolve, reject };
        input.signal.addEventListener('abort', () => reject(new AgentAskError('cancelled')));
      });
    return this.reply instanceof Error ? Promise.reject(this.reply) : Promise.resolve(this.reply);
  }
  acknowledge(agent: AgentId): Promise<void> {
    return this.store ? this.store.acknowledge(agent) : Promise.resolve();
  }
}
