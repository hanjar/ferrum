// FER-207/208/210 di sisi store: pesan lanjutan diarahkan ke run + plan/revision yang TAMPIL, bukan ke
// pesan lama; penolakan executor menjadi catatan di chat; run final tidak pernah dimutasi.
import { describe, expect, it } from 'vitest';
import { ChatStore, type FollowUpRef, type FollowUpResult } from './chatStore.svelte';
import { FakeExecutor, samplePlan } from './fixtures/chatFixtures';

class FollowExecutor extends FakeExecutor {
  calls: Array<{ runId: string; ref: FollowUpRef; text: string }> = [];
  result: FollowUpResult = { accepted: true, revision: 2, local: true };
  followUp(runId: string, ref: FollowUpRef, text: string): FollowUpResult {
    this.calls.push({ runId, ref, text });
    return this.result;
  }
}

function setup(withFollowUp = true) {
  const ex = withFollowUp ? new FollowExecutor() : new FakeExecutor();
  const store = new ChatStore(ex);
  return { ex: ex as FollowExecutor, store };
}
function start(store: ChatStore, ex: FakeExecutor, text = 'arsipkan kiri') {
  store.send(text);
  const { requestId } = ex.submitted.at(-1)!;
  const runId = store.runs.at(-1)!.runId;
  return { requestId, runId };
}
function preview(
  ex: FakeExecutor,
  runId: string,
  requestId: string,
  revision = 1,
  planId = 'plan-1',
) {
  ex.emit({
    runId,
    requestId,
    state: 'awaitingApproval',
    plan: samplePlan({ planId }),
    revision,
    revisionNotes: revision > 1 ? [`Revisi ${revision}: Nama output: x.zip`] : [],
  });
}

describe('FER-208 follow-up binds to the displayed plan id and revision', () => {
  it('a message while a plan awaits approval goes to executor.followUp with that plan id/revision, not a new run', () => {
    const { ex, store } = setup();
    const { runId, requestId } = start(store, ex);
    preview(ex, runId, requestId, 1, 'plan-A');
    store.send('namanya invoice-september.zip');
    expect(ex.submitted).toHaveLength(1);
    expect(ex.calls).toEqual([
      { runId, ref: { planId: 'plan-A', revision: 1 }, text: 'namanya invoice-september.zip' },
    ]);
    expect(store.runs).toHaveLength(1);
    expect(store.messages.filter((m) => m.role === 'user').map((m) => m.text)).toEqual([
      'arsipkan kiri',
      'namanya invoice-september.zip',
    ]);
    expect(store.draft).toBe('');
  });

  it('after a new revision the next follow-up carries the NEW plan id/revision', () => {
    const { ex, store } = setup();
    const { runId, requestId } = start(store, ex);
    preview(ex, runId, requestId, 1, 'plan-A');
    store.send('namanya a.zip');
    preview(ex, runId, requestId, 2, 'plan-B');
    store.send('taruh di kanan');
    expect(ex.calls[1].ref).toEqual({ planId: 'plan-B', revision: 2 });
    expect(store.run(runId)!.revision).toBe(2);
    expect(store.run(runId)!.revisionNotes).toEqual(['Revisi 2: Nama output: x.zip']);
  });

  it('an explicit reference to an older plan is passed as-is and the rejection becomes a notice', () => {
    const { ex, store } = setup();
    const { runId, requestId } = start(store, ex);
    preview(ex, runId, requestId, 2, 'plan-B');
    ex.result = {
      accepted: false,
      code: 'stale_ref',
      message: 'Rencana yang Anda maksud (revisi 1) sudah berganti.',
    };
    const res = store.followUp(runId, 'taruh di kanan', { planId: 'plan-A', revision: 1 });
    expect(ex.calls[0].ref).toEqual({ planId: 'plan-A', revision: 1 });
    expect(res).toMatchObject({ accepted: false, code: 'stale_ref' });
    const last = store.messages.at(-1)!;
    expect(last.role).toBe('notice');
    expect(last.text).toContain('sudah berganti');
  });

  it('approval can be pressed again for the new revision after a follow-up', () => {
    const { ex, store } = setup();
    const { runId, requestId } = start(store, ex);
    preview(ex, runId, requestId, 1, 'plan-A');
    store.send('hanya pdf');
    ex.emit({ runId, requestId, state: 'resolving' });
    expect(store.run(runId)!.canApprove).toBe(false);
    preview(ex, runId, requestId, 2, 'plan-B');
    expect(store.run(runId)!.canApprove).toBe(true);
    store.approve(runId);
    expect(ex.approved).toEqual([runId]);
  });

  it('invalid edits keep the draft for correction; running/final rejections clear it', () => {
    const { ex, store } = setup();
    const { runId, requestId } = start(store, ex);
    preview(ex, runId, requestId);
    ex.result = { accepted: false, code: 'invalid_edit', message: 'Nama tidak aman.' };
    store.draft = 'namanya ../x.zip';
    store.send('namanya ../x.zip');
    expect(store.draft).toBe('namanya ../x.zip');
    ex.result = { accepted: false, code: 'running', message: 'Pekerjaan sudah berjalan.' };
    store.draft = 'taruh di kanan';
    store.send('taruh di kanan');
    expect(store.draft).toBe('');
    expect(store.messages.filter((m) => m.role === 'notice')).toHaveLength(2);
  });
});

describe('FER-210 follow-up while the job is queued or running', () => {
  it('goes to the executor, which refuses; the card and job are untouched and "batal" still cancels', () => {
    const { ex, store } = setup();
    const { runId, requestId } = start(store, ex);
    preview(ex, runId, requestId);
    ex.emit({
      runId,
      requestId,
      state: 'running',
      jobs: [{ jobId: 'job-1', state: 'running', done: 1, total: 4 }],
    });
    ex.result = {
      accepted: false,
      code: 'running',
      message: 'Perubahan tidak mengubah job yang aktif.',
    };
    store.send('namanya baru.zip');
    expect(ex.submitted).toHaveLength(1);
    expect(store.run(runId)!.state).toBe('running');
    expect(store.run(runId)!.jobs).toHaveLength(1);
    expect(store.messages.at(-1)).toMatchObject({ role: 'notice' });
    store.send('Batalkan');
    expect(ex.cancelled).toEqual([runId]);
    expect(store.run(runId)!.cancelRequested).toBe(true);
  });
});

describe('FER-208 after the run is final', () => {
  it('a follow-up phrase after completion is NOT a new request and never reaches the executor', () => {
    const { ex, store } = setup();
    const { runId, requestId } = start(store, ex);
    ex.emit({ runId, requestId, state: 'succeeded' });
    store.send('namanya invoice-september.zip');
    store.send('taruh di kanan');
    store.send('hanya PDF');
    expect(ex.submitted).toHaveLength(1);
    expect(ex.calls).toHaveLength(0);
    const notices = store.messages.filter((m) => m.role === 'notice');
    expect(notices).toHaveLength(3);
    expect(notices[0].text).toMatch(/selesai|sudah berakhir/i);
    expect(store.run(runId)!.state).toBe('succeeded');
  });

  it('"batal" with nothing active says so and does not start a request', () => {
    const { ex, store } = setup();
    const { runId, requestId } = start(store, ex);
    ex.emit({ runId, requestId, state: 'succeeded' });
    store.send('batal');
    expect(ex.submitted).toHaveLength(1);
    expect(ex.cancelled).toHaveLength(0);
    expect(store.messages.at(-1)!.role).toBe('notice');
  });

  it('an ordinary new request after completion still starts a fresh run with a fresh plan', () => {
    const { ex, store } = setup();
    const { runId, requestId } = start(store, ex);
    ex.emit({ runId, requestId, state: 'succeeded' });
    store.send('arsipkan folder kanan jadi zip');
    expect(ex.submitted).toHaveLength(2);
    expect(store.runs).toHaveLength(2);
  });
});

describe('questions and legacy executors', () => {
  it('a reply to a model question (no plan yet) starts a new run and supersedes the old card', () => {
    const { ex, store } = setup();
    const { runId, requestId } = start(store, ex);
    ex.emit({ runId, requestId, state: 'needsInput', modelText: 'Folder mana?' });
    store.send('folder kiri');
    expect(ex.submitted).toHaveLength(2);
    expect(store.messages.filter((m) => m.role === 'notice')).toHaveLength(0);
    // Kartu pertama tidak lagi menjadi target follow-up setelah run baru selesai.
    const second = store.runs[1];
    ex.emit({ runId: second.runId, requestId: second.requestId, state: 'succeeded' });
    store.send('namanya x.zip');
    expect(ex.calls).toHaveLength(0); // tanpa plan: tidak ada acuan; kartu lama bukan target
    expect(ex.submitted).toHaveLength(2);
  });

  it('an executor without followUp keeps the legacy behaviour (busy blocks, cancel works)', () => {
    const { ex, store } = setup(false);
    const { runId, requestId } = start(store, ex);
    preview(ex, runId, requestId);
    store.send('namanya x.zip');
    expect(ex.submitted).toHaveLength(1);
    expect(store.messages.filter((m) => m.role === 'notice')).toHaveLength(0);
    store.send('batal');
    expect(ex.cancelled).toEqual([runId]);
  });

  it('canSend allows typing while busy only when the executor can take follow-ups', () => {
    const a = setup(true);
    start(a.store, a.ex);
    expect(a.store.canSend('namanya x.zip')).toBe(true);
    expect(a.store.canSend('   ')).toBe(false);
    const b = setup(false);
    start(b.store, b.ex);
    expect(b.store.canSend('namanya x.zip')).toBe(false);
    expect(b.store.canSend('batal')).toBe(true);
  });
});
