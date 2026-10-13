// FER-476 (+ FER-471 verification): the full chain from the chat to the backend invoke, through the
// REAL runtime wiring (createAiRuntime: executor, ports, RemoteIntentProvider, parseModelReply):
//   offer list -> backend reply -> parsed intent -> executor -> confirm card state -> approve -> invoke.
// Only the Tauri `invoke` boundary is faked. Proves copy.adjust (raise/lower copy threads) works from
// the chat, and the same for indexer.status / indexer.setMode.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const invokeMock = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invokeMock(...a) }));

import { ops } from '../stores/opsStore.svelte';
import { createAiRuntime, type AiRuntime } from './aiRuntime';
import { registerPaneContext } from './contextPanes';
import { FakePane } from './contextTestKit';
import type { CopyControlState } from '../ipc/copyControl';

const SETUP = {
  endpoint: 'http://127.0.0.1:20128/v1',
  model: 'm',
  timeoutMs: 5000,
  credentialRef: 'Ferrum/AI/custom',
  structured: 'none',
};

const copyJob = (over: Partial<CopyControlState> = {}): CopyControlState => ({
  operationId: 'op_77',
  auto: true,
  threads: 4,
  activeWorkers: 4,
  autoMax: 16,
  media: 'ssd',
  priority: 'normal',
  running: true,
  ...over,
});

let runtime: AiRuntime;
let unregister: Array<() => void> = [];
let planRequests: Array<{ capabilities?: string[]; userText: string }>;
let intent: Record<string, unknown>;
let indexerState: Record<string, unknown>;

beforeEach(() => {
  localStorage.clear();
  (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {};
  // The chat captures both panes before every request (as in the real app).
  const left = new FakePane('left');
  const right = new FakePane('right');
  unregister = [
    registerPaneContext('left', () => left.probe()),
    registerPaneContext('right', () => right.probe()),
  ];
  planRequests = [];
  intent = { capability: 'copy.adjust', action: 'increase' };
  indexerState = {
    installed: true,
    scmState: 'running',
    reachable: true,
    version: '1.0.0',
    mode: 'shared',
    state: 'running',
    index: 'ready',
    canLowerPrivacy: false,
    problem: null,
    squatterPid: null,
    indexPaused: false,
    degraded: false,
    volumes: [],
  };
  invokeMock.mockReset();
  invokeMock.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
    switch (cmd) {
      case 'ai_plan_intent': {
        const request = args?.request as {
          requestId: string;
          capabilities?: string[];
          userText: string;
        };
        planRequests.push(request);
        return { schemaVersion: 1, requestId: request.requestId, intent };
      }
      case 'copy_control_snapshot':
        return [copyJob()];
      case 'copy_control_adjust':
        return copyJob({ threads: 5, activeWorkers: 4, auto: false });
      case 'indexer_status':
        return indexerState;
      case 'indexer_set_mode':
        return { mode: (args as { mode: string }).mode, changed: true, elevated: true };
      default:
        return [];
    }
  });
  runtime = createAiRuntime(SETUP as never);
});

afterEach(() => {
  runtime.dispose();
  for (const off of unregister) off();
  ops.copyControls.clear();
  delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__;
  localStorage.clear();
});

const stateOf = (id: string) => runtime.chat.run(id)?.state;

describe('copy.adjust from the chat (FER-471 end to end)', () => {
  it('is offered while a copy runs; the card appears; Terapkan writes the absolute thread count', async () => {
    ops.applyCopyControl(copyJob()); // what the operations://copy-control event does
    runtime.chat.send('copy-nya lebih cepat dong');
    const runId = runtime.chat.runs.at(-1)!.runId;
    await vi.waitFor(() => expect(stateOf(runId)).toBe('awaitingApproval'));

    expect(planRequests[0].capabilities).toContain('copy.adjust');
    const run = runtime.chat.run(runId)!;
    expect(run.copyAdjust?.summary).toContain('4 → 5');
    expect(run.canApprove).toBe(true);
    // Nothing was written before the click.
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === 'copy_control_adjust')).toBe(false);

    runtime.chat.approve(runId);
    await vi.waitFor(() => expect(stateOf(runId)).toBe('succeeded'));
    expect(invokeMock).toHaveBeenCalledWith('copy_control_adjust', {
      operationId: 'op_77',
      change: { kind: 'set', threads: 5 },
    });
    expect(runtime.chat.run(runId)?.copyAdjust?.result?.applied?.threads).toBe(5);
  });

  it('can also lower the threads', async () => {
    intent = { capability: 'copy.adjust', action: 'decrease', by: 2 };
    ops.applyCopyControl(copyJob({ auto: false, threads: 6 }));
    invokeMock.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
      if (cmd === 'ai_plan_intent') {
        const request = args?.request as {
          requestId: string;
          capabilities?: string[];
          userText: string;
        };
        planRequests.push(request);
        return { schemaVersion: 1, requestId: request.requestId, intent };
      }
      if (cmd === 'copy_control_snapshot') return [copyJob({ auto: false, threads: 6 })];
      if (cmd === 'copy_control_adjust') return copyJob({ auto: false, threads: 4 });
      return [];
    });
    runtime.chat.send('turunkan thread copy 2');
    const runId = runtime.chat.runs.at(-1)!.runId;
    await vi.waitFor(() => expect(stateOf(runId)).toBe('awaitingApproval'));
    runtime.chat.approve(runId);
    await vi.waitFor(() => expect(stateOf(runId)).toBe('succeeded'));
    expect(invokeMock).toHaveBeenCalledWith('copy_control_adjust', {
      operationId: 'op_77',
      change: { kind: 'set', threads: 4 },
    });
  });

  it('is NOT offered when no copy runs', async () => {
    runtime.chat.send('copy-nya lebih cepat dong');
    const runId = runtime.chat.runs.at(-1)!.runId;
    await vi.waitFor(() => expect(planRequests).toHaveLength(1));
    expect(planRequests[0].capabilities).not.toContain('copy.adjust');
    await vi.waitFor(() => expect(stateOf(runId)).not.toBe('planning'));
    // The (misbehaving) reply for a capability that was not offered is rejected, never executed.
    expect(stateOf(runId)).toBe('failed');
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === 'copy_control_adjust')).toBe(false);
  });
});

describe('indexer capabilities from the chat', () => {
  it('both ids are offered on the desktop app', async () => {
    intent = { capability: 'indexer.status' };
    runtime.chat.send('indeksnya sudah siap?');
    await vi.waitFor(() => expect(planRequests).toHaveLength(1));
    expect(planRequests[0].capabilities).toEqual(
      expect.arrayContaining(['indexer.status', 'indexer.setMode']),
    );
  });

  it('indexer.status answers from indexer_status without any write', async () => {
    intent = { capability: 'indexer.status' };
    indexerState = {
      ...indexerState,
      index: 'building',
      volumes: [
        {
          letter: 'C',
          state: 'building',
          error: null,
          entries: 10,
          progressPermille: 250,
          live: false,
        },
      ],
    };
    runtime.chat.send('indeksnya sudah siap?');
    const runId = runtime.chat.runs.at(-1)!.runId;
    await vi.waitFor(() => expect(stateOf(runId)).toBe('succeeded'));
    const view = runtime.chat.run(runId)?.indexer;
    expect(view?.kind === 'status' && view.readiness).toBe('building');
    expect(view?.kind === 'status' && view.volumes[0].percent).toBe(25);
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === 'indexer_set_mode')).toBe(false);
  });

  it('indexer.setMode: card first, tick required, then indexer_set_mode with confirmed=true', async () => {
    intent = { capability: 'indexer.setMode', mode: 'high_performance' };
    runtime.chat.send('pakai mode high performance');
    const runId = runtime.chat.runs.at(-1)!.runId;
    await vi.waitFor(() => expect(stateOf(runId)).toBe('awaitingApproval'));
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === 'indexer_set_mode')).toBe(false);

    runtime.chat.approve(runId); // no tick: ignored
    runtime.chat.approve(runId, { acknowledged: false });
    expect(runtime.chat.run(runId)?.approvalSent).toBe(false);
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === 'indexer_set_mode')).toBe(false);

    runtime.chat.approve(runId, { acknowledged: true });
    await vi.waitFor(() => expect(stateOf(runId)).toBe('succeeded'));
    expect(invokeMock).toHaveBeenCalledWith('indexer_set_mode', {
      mode: 'high_performance',
      confirmed: true,
    });
  });

  it('a UAC refusal from the backend ends the card with the declined message', async () => {
    intent = { capability: 'indexer.setMode', mode: 'high_performance' };
    const base = invokeMock.getMockImplementation()!;
    invokeMock.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
      if (cmd === 'indexer_set_mode') throw { code: 'declined', message: 'UAC cancelled' };
      return base(cmd, args);
    });
    runtime.chat.send('pakai mode high performance');
    const runId = runtime.chat.runs.at(-1)!.runId;
    await vi.waitFor(() => expect(stateOf(runId)).toBe('awaitingApproval'));
    runtime.chat.approve(runId, { acknowledged: true });
    await vi.waitFor(() => expect(stateOf(runId)).toBe('failed'));
    const view = runtime.chat.run(runId)?.indexer;
    expect(view?.kind === 'setMode' && view.result?.code).toBe('declined');
    expect(view?.kind === 'setMode' && view.result?.message).toContain('UAC');
  });
});
