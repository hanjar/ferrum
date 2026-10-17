import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import AutomationPage from './AutomationPage.svelte';
import RunHistory from './RunHistory.svelte';
import { AutomationStore } from '../../stores/automationStore.svelte';
import { toast } from '../../stores/toastStore.svelte';
import type { AutomationRun, HistoryPage } from '../../ipc/automation';
import { canRecheck, canReviewInSync, retryRemaining } from './automationModel';
import { fakeBackend, fakeProfiles, fixture, profileSummary } from './automationTestKit';

const held = (): AutomationRun => fixture<HistoryPage>('automation-history.json').runs[0];

function runWith(over: Partial<AutomationRun>): AutomationRun {
  return { ...held(), ...over };
}

function setup(runs: AutomationRun[], over: Parameters<typeof fakeBackend>[0] = {}) {
  const backend = fakeBackend({ history: vi.fn(async () => ({ runs }) as never), ...over });
  const openSync = vi.fn(async () => {});
  const store = new AutomationStore({
    backend,
    profiles: fakeProfiles([profileSummary()]),
    validateDelayMs: 0,
    openSync,
  });
  return { backend, store, openSync };
}

beforeEach(() => {
  toast.message = '';
});

describe('run actions model (RQ-10)', () => {
  it('Retry only after a recheck found remaining > 0, and never for a conflict', () => {
    expect(retryRemaining(held())).toBe(2);
    expect(retryRemaining(runWith({ recheck: undefined }))).toBe(0);
    for (const state of ['running', 'inSync', 'conflict'] as const) {
      expect(retryRemaining(runWith({ recheck: { ...held().recheck!, state } }))).toBe(0);
    }
    expect(retryRemaining(runWith({ recheck: { ...held().recheck!, remaining: 0 } }))).toBe(0);
    // A conflict never offers Retry, even if the recheck claims remaining operations.
    expect(retryRemaining(runWith({ outcome: 'conflict' }))).toBe(0);
    // A run that is no longer the held one offers nothing.
    expect(retryRemaining(runWith({ needsAttention: false }))).toBe(0);
  });

  it('Recheck only for the held run, not while a recheck runs; Review in Sync for conflicts', () => {
    expect(canRecheck(held())).toBe(true);
    expect(canRecheck(runWith({ needsAttention: false }))).toBe(false);
    expect(canRecheck(runWith({ recheck: { ...held().recheck!, state: 'running' } }))).toBe(false);
    expect(canReviewInSync(held())).toBe(false);
    expect(canReviewInSync(runWith({ outcome: 'conflict' }))).toBe(true);
    expect(canReviewInSync(runWith({ counts: { ...held().counts, held: 1 } }))).toBe(true);
  });
});

describe('Run history (MH-6)', () => {
  it('lists the columns, then expands a row to the error, note, recheck text and operations', async () => {
    const { store } = setup([held()]);
    render(RunHistory, { store, runs: [held()] });
    const headers = [...document.querySelectorAll('th')].map((t) => t.textContent?.trim());
    expect(headers).toEqual([
      'Run',
      'Automation',
      'Trigger',
      'Started → ended',
      'Result',
      'Succeeded',
      'Failed',
      'Not started',
    ]);
    expect(screen.queryByTestId('run-error')).toBeNull();
    await fireEvent.click(screen.getByRole('button', { name: 'Run #1043, expand' }));
    expect(screen.getByTestId('run-error').textContent).toBe('Error: Access is denied');
    expect(screen.getByTestId('run-note').textContent).toContain('Not retried automatically');
    expect(screen.getByTestId('run-recheck').textContent).toContain('2 operations remaining.');
    await waitFor(() => expect(screen.getByTestId('run-ops-1043')).toBeTruthy());
    await waitFor(() =>
      expect(document.querySelectorAll('tr[data-seq]').length).toBeGreaterThan(0),
    );
    expect(
      screen.getByRole('button', { name: 'Run #1043, collapse' }).getAttribute('aria-expanded'),
    ).toBe('true');
  });

  it('offers Recheck and Retry remaining N for a recheck with remaining operations, no Review in Sync', async () => {
    const { store, backend } = setup([held()], {
      retry: vi.fn(async () => fixture('automation-retry.json') as never),
      recheck: vi.fn(async () => ({ status: 'requested' as const })),
    });
    render(RunHistory, { store, runs: [held()] });
    await fireEvent.click(screen.getByRole('button', { name: 'Run #1043, expand' }));
    expect(screen.queryByRole('button', { name: /Review in Sync/ })).toBeNull();
    await fireEvent.click(screen.getByRole('button', { name: 'Recheck' }));
    expect(backend.recheck).toHaveBeenCalledWith(1043);
    await fireEvent.click(screen.getByRole('button', { name: 'Retry remaining 2' }));
    expect(backend.retry).toHaveBeenCalledWith(1043);
    await waitFor(() => expect(toast.message).toBe('Queued — starts shortly'));
  });

  it('hides Retry until a recheck was made', async () => {
    const run = runWith({ recheck: undefined });
    const { store } = setup([run]);
    render(RunHistory, { store, runs: [run] });
    await fireEvent.click(screen.getByRole('button', { name: 'Run #1043, expand' }));
    expect(screen.getByRole('button', { name: 'Recheck' })).toBeTruthy();
    expect(screen.queryByRole('button', { name: /Retry remaining/ })).toBeNull();
  });

  it('a conflict offers Review in Sync and Recheck but never Retry', async () => {
    const run = runWith({
      outcome: 'conflict',
      result: { kind: 'attention', label: 'Conflict held', sub: '1 conflict held for review' },
      recheck: {
        state: 'remaining',
        remaining: 3,
        text: 'Fresh comparison: 3 operations remaining.',
        atMs: 1,
      },
      profileId: profileSummary().id,
      automationId: undefined,
    });
    const { store, openSync } = setup([run]);
    await store.load();
    render(RunHistory, { store, runs: [run] });
    await fireEvent.click(screen.getByRole('button', { name: 'Run #1043, expand' }));
    expect(screen.getByRole('button', { name: 'Recheck' })).toBeTruthy();
    expect(screen.queryByRole('button', { name: /Retry remaining/ })).toBeNull();
    await fireEvent.click(screen.getByRole('button', { name: 'Review in Sync ↗' }));
    await waitFor(() =>
      expect(openSync).toHaveBeenCalledWith({
        left: profileSummary().left,
        right: profileSummary().right,
      }),
    );
  });

  it('an old resolved run has no actions and shows who resolved it', async () => {
    const run = runWith({
      needsAttention: false,
      resolvedBy: 1044,
      resolvedText: 'Resolved by run #1044',
      recheck: undefined,
    });
    const { store } = setup([run]);
    render(RunHistory, { store, runs: [run] });
    await fireEvent.click(screen.getByRole('button', { name: 'Run #1043, expand' }));
    expect(screen.getByTestId('run-resolved').textContent).toBe('Resolved by run #1044');
    expect(screen.queryByRole('button', { name: 'Recheck' })).toBeNull();
    expect(screen.queryByRole('button', { name: /Retry/ })).toBeNull();
  });

  it('Open automation goes to the detail page', async () => {
    const { store, backend } = setup([held()]);
    render(RunHistory, { store, runs: [held()] });
    await fireEvent.click(screen.getByRole('button', { name: 'Run #1043, expand' }));
    await fireEvent.click(screen.getByRole('button', { name: 'Open automation' }));
    expect(backend.get).toHaveBeenCalledWith(held().automationId);
    expect(store.view).toBe('detail');
  });

  it('draws error text as text, not markup', async () => {
    const run = runWith({ errorText: '<script>alert(1)</script>', note: '<i>x</i>' });
    const { store } = setup([run]);
    render(RunHistory, { store, runs: [run] });
    await fireEvent.click(screen.getByRole('button', { name: 'Run #1043, expand' }));
    const panel = document.querySelector('[data-run-detail]')!;
    expect(panel.querySelector('script')).toBeNull();
    expect(panel.querySelector('i')).toBeNull();
    expect(panel.textContent).toContain('<script>alert(1)</script>');
  });
});

describe('Run history tab (filter All / Needs attention)', () => {
  it('switches the filter and asks the backend each time', async () => {
    const { store, backend } = setup([held()]);
    render(AutomationPage, { store });
    await fireEvent.click(await screen.findByRole('tab', { name: /Run history/ }));
    await waitFor(() => expect(backend.history).toHaveBeenCalledWith({ filter: 'all', limit: 50 }));
    const group = screen.getByRole('group', { name: 'Run history filter' });
    await fireEvent.click(within(group).getByRole('button', { name: 'Needs attention' }));
    await waitFor(() =>
      expect(backend.history).toHaveBeenLastCalledWith({ filter: 'attention', limit: 50 }),
    );
    expect(within(group).getByRole('button', { name: 'Needs attention' }).className).toContain(
      'on',
    );
    expect(
      screen.getByText(/Counts are file operations from the engine's comparison/),
    ).toBeTruthy();
  });
});
