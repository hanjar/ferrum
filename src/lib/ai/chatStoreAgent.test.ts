// FER-490 "Ask AI agent": tujuan agent CLI di ChatStore. Hanya referensi lampiran yang dikirim;
// jawaban di-stream sebagai teks dan tidak pernah menjadi rencana/eksekusi.
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { ChatStore } from './chatStore.svelte';
import { FakeExecutor } from './fixtures/chatFixtures';
import { FakeAsker } from './fixtures/attachmentFixtures';
import { FakeAgentAsker, FakeAgentStore } from './fixtures/agentFixtures';
import { resetDisclosuresForTests, type AttachmentInfo } from './attachments';
import {
  AgentAskError,
  agentErrorText,
  needsAgent,
  parseSkipped,
  parseView,
  skippedText,
} from './agentCli.svelte';

const DIR = 'C:\\Users\\demo\\docs';

function info(name: string, extra: Partial<AttachmentInfo> = {}): AttachmentInfo {
  return {
    path: `${DIR}\\${name}`,
    name,
    kind: 'text',
    size: 120,
    modifiedMs: 1,
    inArchive: false,
    support: 'content',
    ...extra,
  };
}

function setup() {
  const ex = new FakeExecutor();
  const asker = new FakeAsker();
  const agents = new FakeAgentStore();
  const agentAsker = new FakeAgentAsker(agents);
  const store = new ChatStore(ex, asker, agentAsker, agents);
  return { ex, asker, agents, agentAsker, store };
}

const flush = () => new Promise((r) => setTimeout(r, 0));

beforeEach(() => resetDisclosuresForTests());
afterEach(() => resetDisclosuresForTests());

describe('ChatStore agent target (FER-490)', () => {
  it('sends references to the chosen agent, streams text/status and keeps the answer as text', async () => {
    const { ex, asker, agentAsker, store } = setup();
    await store.addAttachments(asker.add(info('q3.pdf', { kind: 'pdf', support: 'metadata' })));
    store.setTarget('claude');
    agentAsker.hold = true;
    agentAsker.stream = [
      { kind: 'status', text: 'agent sedang membaca file…' },
      { kind: 'text', text: 'Membaca.' },
      { kind: 'text', text: '{"intent":"delete","paths":["C:\\\\"]}' },
    ];
    expect(store.canSend('ringkas file ini')).toBe(true);
    store.send('ringkas file ini');
    await flush();
    expect(asker.asks).toHaveLength(0);
    expect(ex.submitted).toHaveLength(0);
    expect(agentAsker.asks).toHaveLength(1);
    const req = agentAsker.asks[0];
    expect(req.agent).toBe('claude');
    expect(req.question).toBe('ringkas file ini');
    expect(req.attachments).toEqual([{ path: `${DIR}\\q3.pdf`, confirmedSensitive: false }]);
    const answer = store.messages.at(-1)!;
    expect(answer).toMatchObject({ role: 'answer', pending: true, agent: 'Claude Code' });
    expect(answer.status).toBe('agent sedang membaca file…');
    expect(answer.text).toBe('Membaca.\n\n{"intent":"delete","paths":["C:\\\\"]}');
    expect(store.busy).toBe(true);
    agentAsker.pending!.resolve({
      agent: 'claude',
      answer: 'Laporan Q3.',
      truncated: false,
      skipped: [{ path: `${DIR}\\.env`, reason: 'sensitive' }],
    });
    await flush();
    expect(answer).toMatchObject({ pending: false, text: 'Laporan Q3.', truncated: false });
    expect(answer.agentSkipped).toEqual([{ path: `${DIR}\\.env`, reason: 'sensitive' }]);
    expect(skippedText(answer.agentSkipped![0])).toBe('.env (terlihat sensitif)');
    expect(answer.status).toBeUndefined();
    // Text only: no run, no plan, nothing for the executor.
    expect(store.runs).toHaveLength(0);
    expect(ex.submitted).toHaveLength(0);
  });

  it('needs attachments for an agent question', () => {
    const { agentAsker, store } = setup();
    store.setTarget('claude');
    expect(store.canSend('halo')).toBe(false);
    store.send('halo');
    expect(agentAsker.asks).toHaveLength(0);
    expect(store.messages.at(-1)?.text).toContain('Lampirkan file dulu');
  });

  it('asks for the per-agent disclosure once before anything is sent', async () => {
    const { asker, agents, store } = setup();
    await store.addAttachments(asker.add(info('a.txt')));
    store.setTarget('codex');
    expect(store.pendingAgentDisclosure?.id).toBe('codex');
    expect(store.canSend('apa ini?')).toBe(false);
    // The Ferrum provider disclosure does not apply on the agent path.
    expect(store.pendingDisclosure).toBeUndefined();
    await store.ackAgentDisclosure();
    expect(agents.calls).toContainEqual({ cmd: 'ack', agent: 'codex' });
    expect(store.pendingAgentDisclosure).toBeUndefined();
    expect(store.canSend('apa ini?')).toBe(true);
  });

  it('sensitive attachments still need the manual tick', async () => {
    const { asker, agentAsker, store } = setup();
    await store.addAttachments(asker.add(info('.env', { sensitive: 'name' })));
    store.setTarget('claude');
    expect(store.canSend('apa ini?')).toBe(false);
    store.confirmAttachment(`${DIR}\\.env`, true);
    expect(store.canSend('apa ini?')).toBe(true);
    store.send('apa ini?');
    await flush();
    expect(agentAsker.asks[0].attachments).toEqual([
      { path: `${DIR}\\.env`, confirmedSensitive: true },
    ]);
  });

  it('offers the default agent for PDF/Office, metadata-only and oversized attachments', async () => {
    const { asker, store } = setup();
    await store.addAttachments(asker.add(info('a.txt')));
    expect(store.agentOffer).toBeUndefined();
    await store.addAttachments(asker.add(info('big.log', { size: 300 * 1024 })));
    expect(store.agentOffer?.id).toBe('claude');
    store.setTarget(store.agentOffer!.id);
    expect(store.target).toBe('claude');
    expect(store.agentOffer).toBeUndefined();
    expect(
      needsAgent({ kind: 'document', support: 'metadata', size: 1, inArchive: false }, 100),
    ).toBe(true);
    expect(needsAgent({ kind: 'pdf', support: 'metadata', size: 1, inArchive: true }, 100)).toBe(
      false,
    );
    expect(needsAgent({ kind: 'link', support: 'metadata', size: 1, inArchive: false }, 100)).toBe(
      false,
    );
  });

  it('cancel aborts the run and drops the streamed text', async () => {
    const { asker, agentAsker, store } = setup();
    await store.addAttachments(asker.add(info('a.txt')));
    store.setTarget('claude');
    agentAsker.hold = true;
    agentAsker.stream = [{ kind: 'text', text: 'setengah jawaban' }];
    store.send('apa ini?');
    await flush();
    expect(agentAsker.asks[0].signal.aborted).toBe(false);
    store.cancelAsk();
    await flush();
    expect(agentAsker.asks[0].signal.aborted).toBe(true);
    const answer = store.messages.at(-1)!;
    expect(answer).toMatchObject({ pending: false, error: 'Dibatalkan.' });
    expect(answer.text).toBeUndefined();
    expect(store.asking).toBe(false);
  });

  it('agent errors become app text; a sensitive refusal clears the tick', async () => {
    const { asker, agentAsker, store } = setup();
    await store.addAttachments(asker.add(info('.env', { sensitive: 'name' })));
    store.confirmAttachment(`${DIR}\\.env`, true);
    store.setTarget('claude');
    agentAsker.reply = new AgentAskError('sensitive_unconfirmed', `${DIR}\\.env`);
    store.send('apa ini?');
    await flush();
    expect(store.messages.at(-1)?.error).toContain('sensitif');
    expect(store.attachments[0].confirmed).toBe(false);
    expect(
      agentErrorText(new AgentAskError('agent_failed', undefined, 'Not logged in'), 'Codex'),
    ).toBe('Codex gagal: Not logged in');
    expect(agentErrorText(new AgentAskError('timeout'), 'Codex')).toContain('5 menit');
  });

  it('history does not cross from AI Ferrum to an agent', async () => {
    const { asker, agentAsker, store } = setup();
    await store.addAttachments(asker.add(info('a.txt')));
    store.send('pertanyaan pertama');
    await flush();
    store.setTarget('claude');
    store.send('pertanyaan kedua');
    await flush();
    expect(agentAsker.asks[0].history).toEqual([]);
    store.send('pertanyaan ketiga');
    await flush();
    expect(agentAsker.asks[1].history).toEqual([
      { role: 'user', text: 'pertanyaan kedua' },
      { role: 'assistant', text: 'Ini laporan Q3.' },
    ]);
  });

  it('an unsupported agent is never a target', () => {
    const { store } = setup();
    store.setTarget('omp');
    expect(store.targetAgent).toBeUndefined();
    expect(store.canSend('x')).toBe(false);
  });

  it('a broad-read agent (Codex) is no target until switched on in Settings (E2)', async () => {
    const { asker, agents, store } = setup();
    await agents.setEnabled('codex', false);
    expect(agents.supported.map((a) => a.id)).toEqual(['claude']);
    await store.addAttachments(asker.add(info('a.txt')));
    store.setTarget('codex');
    expect(store.targetAgent).toBeUndefined();
    expect(store.canSend('apa ini?')).toBe(false);
    await agents.setEnabled('codex', true);
    expect(store.targetAgent?.id).toBe('codex');
  });

  it('parseSkipped keeps only known reasons', () => {
    expect(
      parseSkipped([
        { path: 'C:\\a\\.env', reason: 'sensitive' },
        { path: 'C:\\a\\x', reason: 'weird' },
        { reason: 'link' },
        'junk',
      ]),
    ).toEqual([{ path: 'C:\\a\\.env', reason: 'sensitive' }]);
  });

  it('parseView drops unknown agents and statuses', () => {
    const v = parseView({
      agents: [
        { id: 'claude', name: 'Claude Code', status: 'supported', acknowledged: true },
        { id: 'bash', name: 'x', status: 'supported' },
        { id: 'codex', name: 'Codex', status: 'weird' },
      ],
      defaultAgent: 'bash',
      detectedAtMs: 5,
      testProfile: true,
    });
    expect(v.agents.map((a) => a.id)).toEqual(['claude']);
    expect(v.defaultAgent).toBeUndefined();
    expect(v.testProfile).toBe(true);
  });
});
