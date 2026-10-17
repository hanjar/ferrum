import { beforeEach, describe, expect, it, vi } from 'vitest';
import { readFileSync, readdirSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const invokeMock = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invokeMock(...a) }));
const listenMock = vi.fn();
vi.mock('@tauri-apps/api/event', () => ({ listen: (...a: unknown[]) => listenMock(...a) }));

const {
  defaultBackend,
  AutomationError,
  reviewLabel,
  queueToast,
  listenAutomationChanged,
  listenAutomationProgress,
  AUTOMATION_CHANGED_EVENT,
  AUTOMATION_PROGRESS_EVENT,
} = await import('./automation');
import type {
  AutomationChanged,
  AutomationDraft,
  AutomationProgress,
  NeedsReview,
  QueueResult,
} from './automation';

const here = dirname(fileURLToPath(import.meta.url));
const fixtureDir = join(here, '../../../contract-fixtures/automation');
const fixture = (name: string): Record<string, unknown> =>
  JSON.parse(readFileSync(join(fixtureDir, name), 'utf-8'));

function fakeTauri() {
  (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {};
}
function unTauri() {
  delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__;
}

const draft = (fixture('automation-validate.request.json') as { draft: AutomationDraft }).draft;
const AUTO = '7d1c2b3a-4e5f-4a6b-8c7d-9e0f1a2b3c4d';
const TEST = 'c0ffee00-1234-4abc-8def-0123456789ab';

beforeEach(() => {
  invokeMock.mockReset().mockName('invoke');
  listenMock.mockReset().mockName('listen');
  unTauri();
});

describe('automation IPC wrappers', () => {
  it('sends exactly the request fixtures (the Rust side checks the same files)', async () => {
    fakeTauri();
    const backend = defaultBackend();

    invokeMock.mockResolvedValueOnce(fixture('automation-validate.json'));
    expect(await backend.validate(draft)).toEqual(fixture('automation-validate.json'));
    expect(invokeMock).toHaveBeenLastCalledWith(
      'automation_validate',
      fixture('automation-validate.request.json'),
    );

    invokeMock.mockResolvedValueOnce(fixture('automation-test.json'));
    expect(await backend.test({ draft })).toEqual(fixture('automation-test.json'));
    expect(invokeMock).toHaveBeenLastCalledWith(
      'automation_test',
      fixture('automation-test.request.json'),
    );

    invokeMock.mockResolvedValueOnce(fixture('automation-test.json'));
    await backend.test({ id: AUTO, draft });
    expect(invokeMock).toHaveBeenLastCalledWith(
      'automation_test',
      fixture('automation-test-update.request.json'),
    );

    invokeMock.mockResolvedValueOnce(fixture('automation-approve.json'));
    expect(await backend.approve({ draft, testId: TEST })).toEqual(
      fixture('automation-approve.json'),
    );
    expect(invokeMock).toHaveBeenLastCalledWith(
      'automation_approve',
      fixture('automation-approve.request.json'),
    );

    const update = fixture('automation-approve-update.request.json') as {
      draft: AutomationDraft;
    };
    invokeMock.mockResolvedValueOnce(fixture('automation-approve.json'));
    await backend.approve({
      id: AUTO,
      expectedConfigRev: 1,
      draft: update.draft,
      testId: TEST,
    });
    expect(invokeMock).toHaveBeenLastCalledWith('automation_approve', update);

    invokeMock.mockResolvedValueOnce(fixture('automation-set-enabled-enabled.json'));
    expect(await backend.setEnabled(AUTO, true)).toEqual({ status: 'enabled' });
    expect(invokeMock).toHaveBeenLastCalledWith(
      'automation_set_enabled',
      fixture('automation-set-enabled.request.json'),
    );

    invokeMock.mockResolvedValueOnce(fixture('automation-pause.json'));
    expect(await backend.pause(AUTO, 'finish')).toEqual({ status: 'paused' });
    expect(invokeMock).toHaveBeenLastCalledWith(
      'automation_pause',
      fixture('automation-pause.request.json'),
    );

    invokeMock.mockResolvedValueOnce(fixture('automation-delete.json'));
    expect(await backend.delete(AUTO)).toEqual({ status: 'deleted' });
    expect(invokeMock).toHaveBeenLastCalledWith(
      'automation_delete',
      fixture('automation-delete.request.json'),
    );
  });

  it('passes every answer through unchanged, including cancel and stale', async () => {
    fakeTauri();
    const backend = defaultBackend();
    for (const name of ['automation-approve-canceled.json', 'automation-approve-stale.json']) {
      invokeMock.mockResolvedValueOnce(fixture(name));
      expect(await backend.approve({ draft, testId: TEST })).toEqual(fixture(name));
    }
    invokeMock.mockResolvedValueOnce(fixture('automation-set-enabled-needs-choice.json'));
    expect(await backend.setEnabled(AUTO, false)).toEqual({ status: 'needsChoice' });
  });

  it('never sends a confirmation flag or dialog text (SE-1)', async () => {
    fakeTauri();
    const backend = defaultBackend();
    invokeMock.mockResolvedValue({ status: 'stale' });
    await backend.approve({ draft, testId: TEST });
    await backend.setEnabled(AUTO, true);
    await backend.pause(AUTO, 'stop');
    for (const call of invokeMock.mock.calls) {
      const text = JSON.stringify(call);
      expect(text).not.toMatch(/confirm|approved|native|dialog|prompt|force/i);
    }
  });

  it('maps backend errors to AutomationError with the closed code', async () => {
    fakeTauri();
    const backend = defaultBackend();
    invokeMock.mockRejectedValueOnce({ code: 'needs_review', message: 'Review it' });
    await expect(backend.setEnabled(AUTO, true)).rejects.toMatchObject({
      name: 'AutomationError',
      code: 'needs_review',
      detail: 'Review it',
    });
    invokeMock.mockRejectedValueOnce({ code: 'busy', message: 'Another confirmation is open' });
    await expect(backend.approve({ draft, testId: TEST })).rejects.toBeInstanceOf(AutomationError);
    invokeMock.mockRejectedValueOnce('boom');
    await expect(backend.delete(AUTO)).rejects.toMatchObject({ code: 'unknown' });
  });

  it('outside the desktop app nothing can be approved, enabled, tested or deleted', async () => {
    const backend = defaultBackend();
    await expect(backend.approve({ draft, testId: TEST })).rejects.toMatchObject({
      code: 'unsupported',
    });
    await expect(backend.setEnabled(AUTO, true)).rejects.toMatchObject({ code: 'unsupported' });
    await expect(backend.test({ draft })).rejects.toMatchObject({ code: 'unsupported' });
    await expect(backend.validate(draft)).rejects.toMatchObject({ code: 'unsupported' });
    await expect(backend.pause(AUTO, 'stop')).rejects.toMatchObject({ code: 'unsupported' });
    await expect(backend.delete(AUTO)).rejects.toMatchObject({ code: 'unsupported' });
    expect(invokeMock).not.toHaveBeenCalled();
  });
});

describe('host controls and events (P1.3)', () => {
  const RUN = 1042;

  it('sends exactly the request fixtures (the Rust side checks the same files)', async () => {
    fakeTauri();
    const backend = defaultBackend();

    invokeMock.mockResolvedValueOnce(fixture('automation-run-now-queued.json'));
    expect(await backend.runNow(AUTO)).toEqual({ status: 'queued' });
    expect(invokeMock).toHaveBeenLastCalledWith(
      'automation_run_now',
      fixture('automation-run-now.request.json'),
    );

    invokeMock.mockResolvedValueOnce(fixture('automation-stop-run.json'));
    expect(await backend.stopRun(RUN)).toEqual({ status: 'requested' });
    expect(invokeMock).toHaveBeenLastCalledWith(
      'automation_stop_run',
      fixture('automation-stop-run.request.json'),
    );

    invokeMock.mockResolvedValueOnce(fixture('automation-recheck.json'));
    expect(await backend.recheck(RUN)).toEqual({ status: 'requested' });
    expect(invokeMock).toHaveBeenLastCalledWith(
      'automation_recheck',
      fixture('automation-recheck.request.json'),
    );

    invokeMock.mockResolvedValueOnce(fixture('automation-retry.json'));
    expect(await backend.retry(RUN)).toEqual({ status: 'queued' });
    expect(invokeMock).toHaveBeenLastCalledWith(
      'automation_retry',
      fixture('automation-retry.request.json'),
    );
  });

  it('a control carries an id and nothing that could authorize or choose a policy', async () => {
    fakeTauri();
    const backend = defaultBackend();
    invokeMock.mockResolvedValue({ status: 'requested' });
    await backend.runNow(AUTO);
    await backend.stopRun(RUN);
    await backend.recheck(RUN);
    await backend.retry(RUN);
    await backend.pause(AUTO, 'stop');
    for (const [command, args] of invokeMock.mock.calls) {
      const keys = Object.keys(args as Record<string, unknown>).join(',');
      expect(`${command} ${keys}`).not.toMatch(
        /confirm|approved|force|native|policy|profile|root|path|hash|lease|gate|token|skip/i,
      );
    }
  });

  it('turns every queue answer of the fixtures into its toast', () => {
    const answers = readdirSync(fixtureDir).filter((n) => n.startsWith('automation-run-now-'));
    expect(answers.length).toBe(5);
    for (const name of answers) {
      const result = fixture(name) as unknown as QueueResult;
      expect(queueToast(result)).toMatch(/\S/);
    }
    expect(queueToast({ status: 'queued' })).toBe('Queued \u2014 starts shortly');
    expect(queueToast({ status: 'waiting', waitsFor: 'Nightly photos to NAS' })).toBe(
      'Waiting for \u201cNightly photos to NAS\u201d \u2014 same folders',
    );
    expect(queueToast({ status: 'merged' })).toContain('no duplicate job');
    expect(queueToast({ status: 'device' })).toContain('device');
    expect(queueToast({ status: 'requested' })).toContain('runner');
  });

  it('maps the refusals of a control to AutomationError with the closed code', async () => {
    fakeTauri();
    const backend = defaultBackend();
    for (const code of [
      'held',
      'needs_review',
      'paused',
      'runner_down',
      'rate_limited',
      'not_ready',
      'busy',
      'not_found',
    ]) {
      invokeMock.mockRejectedValueOnce({ code, message: 'Fixed text' });
      await expect(backend.runNow(AUTO)).rejects.toMatchObject({
        name: 'AutomationError',
        code,
        detail: 'Fixed text',
      });
    }
  });

  it('outside the desktop app no control reaches a backend', async () => {
    const backend = defaultBackend();
    await expect(backend.runNow(AUTO)).rejects.toMatchObject({ code: 'unsupported' });
    await expect(backend.stopRun(RUN)).rejects.toMatchObject({ code: 'unsupported' });
    await expect(backend.recheck(RUN)).rejects.toMatchObject({ code: 'unsupported' });
    await expect(backend.retry(RUN)).rejects.toMatchObject({ code: 'unsupported' });
    expect(invokeMock).not.toHaveBeenCalled();
  });

  it('listens to the two events with the fixture payloads', async () => {
    fakeTauri();
    const unlisten = vi.fn();
    listenMock.mockResolvedValue(unlisten);
    const changed: AutomationChanged[] = [];
    const progress: AutomationProgress[] = [];
    const off1 = await listenAutomationChanged((c) => changed.push(c));
    const off2 = await listenAutomationProgress((p) => progress.push(p));
    expect(off1).toBe(unlisten);
    expect(off2).toBe(unlisten);
    expect(listenMock.mock.calls.map((c) => c[0])).toEqual([
      AUTOMATION_CHANGED_EVENT,
      AUTOMATION_PROGRESS_EVENT,
    ]);
    expect(AUTOMATION_CHANGED_EVENT).toBe('automation://changed');
    expect(AUTOMATION_PROGRESS_EVENT).toBe('automation://progress');
    const [, changedHandler] = listenMock.mock.calls[0] as [string, (e: unknown) => void];
    const [, progressHandler] = listenMock.mock.calls[1] as [string, (e: unknown) => void];
    changedHandler({ payload: fixture('automation-event-changed.json') });
    progressHandler({ payload: fixture('automation-event-progress.json') });
    expect(changed).toEqual([{ seq: 57 }]);
    expect(progress[0]).toMatchObject({ runId: 1042, phase: 'sync', opsTotal: 6 });
  });

  it('outside the desktop app the listeners are inert', async () => {
    const off = await listenAutomationChanged(() => {});
    expect(typeof off).toBe('function');
    expect(listenMock).not.toHaveBeenCalled();
  });

  it('the events carry no secret', () => {
    for (const name of ['automation-event-changed.json', 'automation-event-progress.json']) {
      expect(readFileSync(join(fixtureDir, name), 'utf-8'), name).not.toMatch(
        /mac|key|lease|path|hash/i,
      );
    }
  });
});

describe('needs-review labels (AP-6)', () => {
  it('names the profile revisions of a profile change', () => {
    const fx = fixture('automation-dto-review-profile-changed.json') as {
      review: NeedsReview;
    };
    expect(reviewLabel(fx.review)).toBe('Profile changed (rev 3 → 4) · held');
  });

  it('has a label for every reason of the fixtures', () => {
    for (const name of readdirSync(fixtureDir).filter((n) =>
      n.startsWith('automation-dto-review-'),
    )) {
      const { review } = fixture(name) as { review: NeedsReview };
      expect(reviewLabel(review)).toMatch(/held$/);
    }
  });
});

describe('contract fixtures', () => {
  it('a lease id appears only in the lease fixtures', () => {
    for (const name of readdirSync(fixtureDir)) {
      if (name.startsWith('sync-lease')) continue;
      expect(readFileSync(join(fixtureDir, name), 'utf-8'), name).not.toMatch(/leaseId/);
    }
  });

  it('request fixtures carry no authorization flag', () => {
    for (const name of readdirSync(fixtureDir).filter(
      (n) => n.startsWith('automation-') && n.endsWith('.request.json'),
    )) {
      expect(Object.keys(fixture(name)).join(','), name).not.toMatch(/confirm|approved|force/i);
    }
  });
});
