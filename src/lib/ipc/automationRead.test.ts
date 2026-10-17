import { beforeEach, describe, expect, it, vi } from 'vitest';
import { readFileSync, readdirSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const invokeMock = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invokeMock(...a) }));

const { defaultBackend, attentionBadge, reviewLabel } = await import('./automation');
import type {
  AutomationDetail,
  AutomationRow,
  AutomationStatusKind,
  EventsPage,
  HistoryPage,
  RunOpsPage,
} from './automation';

const here = dirname(fileURLToPath(import.meta.url));
const fixtureDir = join(here, '../../../contract-fixtures/automation');
const fixture = <T = Record<string, unknown>>(name: string): T =>
  JSON.parse(readFileSync(join(fixtureDir, name), 'utf-8')) as T;

function fakeTauri() {
  (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {};
}
function unTauri() {
  delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__;
}

const ALL_KINDS: AutomationStatusKind[] = [
  'watching',
  'quiet',
  'checking',
  'device',
  'recheck',
  'unsupported',
  'scheduled',
  'queued',
  'pending',
  'running',
  'paused',
  'completed',
  'partial',
  'cancelled',
  'skipped',
  'attention',
  'review',
  'warn',
];

beforeEach(() => {
  invokeMock.mockReset().mockName('invoke');
  unTauri();
});

describe('automation read IPC wrappers (P1.2)', () => {
  it('sends exactly the request fixtures (the Rust side checks the same files)', async () => {
    fakeTauri();
    const backend = defaultBackend();

    invokeMock.mockResolvedValueOnce(fixture('automation-list.json'));
    expect(await backend.list()).toEqual(fixture('automation-list.json'));
    // `automation_list` has no arguments at all.
    expect(invokeMock).toHaveBeenLastCalledWith('automation_list', undefined);

    const get = fixture<{ id: string }>('automation-get.request.json');
    invokeMock.mockResolvedValueOnce(fixture('automation-get.json'));
    expect(await backend.get(get.id)).toEqual(fixture('automation-get.json'));
    expect(invokeMock).toHaveBeenLastCalledWith('automation_get', get);

    const h = fixture<{
      filter: 'all' | 'attention';
      automationId: string;
      before: number;
      limit: number;
    }>('automation-history.request.json');
    invokeMock.mockResolvedValueOnce(fixture('automation-history.json'));
    expect(
      await backend.history({
        filter: h.filter,
        automationId: h.automationId,
        before: h.before,
        limit: h.limit,
      }),
    ).toEqual(fixture('automation-history.json'));
    expect(invokeMock).toHaveBeenLastCalledWith('automation_history', h);

    const o = fixture<{ runId: number; offset: number; limit: number }>(
      'automation-run-ops.request.json',
    );
    invokeMock.mockResolvedValueOnce(fixture('automation-run-ops.json'));
    expect(await backend.runOps(o.runId, o.offset, o.limit)).toEqual(
      fixture('automation-run-ops.json'),
    );
    expect(invokeMock).toHaveBeenLastCalledWith('automation_run_ops', o);

    const e = fixture<{ id: string; before: number; limit: number }>(
      'automation-events.request.json',
    );
    invokeMock.mockResolvedValueOnce(fixture('automation-events.json'));
    expect(await backend.events(e.id, e.before, e.limit)).toEqual(
      fixture('automation-events.json'),
    );
    expect(invokeMock).toHaveBeenLastCalledWith('automation_events', e);
  });

  it('sends null for an absent cursor or filter, never undefined', async () => {
    fakeTauri();
    const backend = defaultBackend();
    invokeMock.mockResolvedValue({ runs: [] });
    await backend.history({ filter: 'all', limit: 25 });
    expect(invokeMock).toHaveBeenLastCalledWith('automation_history', {
      filter: 'all',
      automationId: null,
      before: null,
      limit: 25,
    });
    invokeMock.mockResolvedValue({ events: [] });
    await backend.events('7d1c2b3a-4e5f-4a6b-8c7d-9e0f1a2b3c4d', undefined, 14);
    expect(invokeMock).toHaveBeenLastCalledWith('automation_events', {
      id: '7d1c2b3a-4e5f-4a6b-8c7d-9e0f1a2b3c4d',
      before: null,
      limit: 14,
    });
  });

  it('never sends an authorization flag or dialog text (SE-1)', async () => {
    fakeTauri();
    const backend = defaultBackend();
    invokeMock.mockResolvedValue({});
    await backend.list();
    await backend.get('7d1c2b3a-4e5f-4a6b-8c7d-9e0f1a2b3c4d');
    await backend.history({ filter: 'attention', limit: 10 });
    await backend.runOps(1, 0, 10);
    await backend.events('7d1c2b3a-4e5f-4a6b-8c7d-9e0f1a2b3c4d', 5, 10);
    for (const call of invokeMock.mock.calls) {
      expect(JSON.stringify(call)).not.toMatch(/confirm|approved|native|dialog|prompt|force/i);
    }
  });

  it('maps backend errors to AutomationError with the closed code', async () => {
    fakeTauri();
    const backend = defaultBackend();
    invokeMock.mockRejectedValueOnce({
      code: 'not_found',
      message: 'The automation does not exist',
    });
    await expect(backend.get('7d1c2b3a-4e5f-4a6b-8c7d-9e0f1a2b3c4d')).rejects.toMatchObject({
      name: 'AutomationError',
      code: 'not_found',
    });
    invokeMock.mockRejectedValueOnce({
      code: 'invalid_input',
      message: 'The page size is not valid',
    });
    await expect(backend.history({ filter: 'all', limit: 0 })).rejects.toMatchObject({
      code: 'invalid_input',
    });
    invokeMock.mockRejectedValueOnce('boom');
    await expect(backend.list()).rejects.toMatchObject({ code: 'unknown' });
  });

  it('outside the desktop app the read side answers unsupported and calls nothing', async () => {
    const backend = defaultBackend();
    await expect(backend.list()).rejects.toMatchObject({ code: 'unsupported' });
    await expect(backend.get('x')).rejects.toMatchObject({ code: 'unsupported' });
    await expect(backend.history({ filter: 'all', limit: 5 })).rejects.toMatchObject({
      code: 'unsupported',
    });
    await expect(backend.runOps(1, 0, 5)).rejects.toMatchObject({ code: 'unsupported' });
    await expect(backend.events('x', undefined, 5)).rejects.toMatchObject({
      code: 'unsupported',
    });
    expect(invokeMock).not.toHaveBeenCalled();
  });
});

describe('read fixtures as typed values', () => {
  it('every status in the fixtures has a known kind, a label and a short sub-line', () => {
    const list = fixture<AutomationRow[]>('automation-list.json');
    const detail = fixture<AutomationDetail>('automation-get.json');
    const history = fixture<HistoryPage>('automation-history.json');
    const chips = [
      ...list.map((r) => r.status),
      ...list.flatMap((r) => (r.lastRun ? [r.lastRun.result] : [])),
      detail.automation.status,
      ...(detail.currentRun ? [detail.currentRun.result] : []),
      ...detail.recentRuns.map((r) => r.result),
      ...history.runs.map((r) => r.result),
    ];
    expect(chips.length).toBeGreaterThan(6);
    for (const chip of chips) {
      expect(ALL_KINDS).toContain(chip.kind);
      expect(chip.label.length).toBeGreaterThan(0);
      if (chip.sub !== undefined) expect(chip.sub.length).toBeGreaterThan(0);
    }
  });

  it('the row with a review carries the label the UI shows, the held row says why', () => {
    const [scheduled, review, attention] = fixture<AutomationRow[]>('automation-list.json');
    expect(review.review).toBeDefined();
    expect(review.status.sub).toBe(reviewLabel(review.review!));
    expect(review.canRunNow).toBe(false);
    expect(review.showReview).toBe(true);
    expect(attention.canRunNow).toBe(false);
    expect(attention.runNowReason).toBe(attention.status.sub);
    expect(attention.status.sub).toBe(`${attention.attention!.text} · new runs held`);
    expect(scheduled.canRunNow).toBe(true);
    expect(scheduled.runNowReason).toBeUndefined();
  });

  it('the sidebar badge counts automations needing review or attention (MH-9)', () => {
    const list = fixture<AutomationRow[]>('automation-list.json');
    expect(attentionBadge(list)).toBe(2);
    expect(attentionBadge([list[0]])).toBe(0);
    expect(attentionBadge([])).toBe(0);
  });

  it('operations page over the stored rows while the total is the true count', () => {
    const page = fixture<RunOpsPage>('automation-run-ops.json');
    expect(page.total).toBeGreaterThanOrEqual(page.stored);
    expect(page.ops.map((o) => o.seq)).toEqual([0, 1, 2]);
    const failed = page.ops.find((o) => o.status === 'failed');
    expect(failed?.errorCode).toBe('access_denied');
  });

  it('pages carry a cursor only when there is more', () => {
    const events = fixture<EventsPage>('automation-events.json');
    expect(events.next).toBeUndefined();
    const history = fixture<HistoryPage>('automation-history.json');
    expect(history.next).toBe(history.runs[history.runs.length - 1].id);
  });

  it('the detail pipeline marks one step of the watch lifecycle', () => {
    const d = fixture<AutomationDetail>('automation-get.json');
    expect(d.pipeline.steps).toEqual([
      'Watching',
      'Changes detected',
      'Waiting for quiet',
      'Queued',
      'Running',
    ]);
    expect(d.pipeline.current).toBe(4);
    expect(d.waiting[0].mergedReasons.length).toBeLessThanOrEqual(10);
  });

  it('no read fixture carries a lease id, a MAC or a key', () => {
    for (const name of readdirSync(fixtureDir).filter(
      (n) =>
        /^automation-(list|get|history|run-ops|events)/.test(n) && !n.endsWith('.request.json'),
    )) {
      const text = readFileSync(join(fixtureDir, name), 'utf-8');
      expect(text, name).not.toMatch(/leaseId|"mac"|policyHash|approvalKey/);
    }
  });
});
