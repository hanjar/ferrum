import { beforeEach, describe, expect, it, vi } from 'vitest';
import { readFileSync, readdirSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const invokeMock = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invokeMock(...a) }));

const { defaultBackend, AutomationError, reviewLabel } = await import('./automation');
import type { AutomationDraft, NeedsReview } from './automation';

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
