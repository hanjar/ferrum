import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import AutomationPage from './AutomationPage.svelte';
import { AutomationStore } from '../../stores/automationStore.svelte';
import { toast } from '../../stores/toastStore.svelte';
import type { AutomationDetail, RunOp, RunOpsPage } from '../../ipc/automation';
import { fakeBackend, fakeProfiles, fixture, profileSummary } from './automationTestKit';

const ID = 'b2c3d4e5-f6a7-4b8c-9d0e-1f2a3b4c5d6e';

const photo = (): AutomationDetail => fixture<AutomationDetail>('automation-get.json');

function setup(over: Parameters<typeof fakeBackend>[0] = {}, detail = photo()) {
  const backend = fakeBackend({ get: vi.fn(async () => structuredClone(detail)), ...over });
  const openSync = vi.fn(async () => {});
  const store = new AutomationStore({
    backend,
    profiles: fakeProfiles([profileSummary()]),
    validateDelayMs: 0,
    openSync,
  });
  render(AutomationPage, { store });
  return { backend, store, openSync };
}

async function openDetail(store: AutomationStore) {
  await store.openDetail(ID);
  return screen.findByTestId('automation-detail');
}

beforeEach(() => {
  toast.message = '';
});

describe('AutomationDetail (MH-5)', () => {
  it('shows status, pipeline, current run, waiting entries, settings, recent runs and events', async () => {
    const { store } = setup();
    const view = within(await openDetail(store));
    expect(view.getByRole('heading', { name: 'Photo watch' })).toBeTruthy();
    expect(view.getAllByText('Running 2/6').length).toBeGreaterThan(0);

    const steps = view.getByTestId('pipeline').querySelectorAll('li');
    expect([...steps].map((li) => li.textContent?.trim())).toEqual([
      'Watching',
      'Changes detected',
      'Waiting for quiet',
      'Queued',
      'Running',
    ]);
    expect(steps[4].getAttribute('aria-current')).toBe('step');

    expect(view.getByTestId('run-phase').textContent).toBe('Syncing 3 of 6');
    expect(view.getByTestId('run-counts').textContent).toBe(
      '2 succeeded \u00b7 0 failed \u00b7 4 remaining',
    );
    const bar = view.getByRole('progressbar', { name: 'Run progress' });
    expect(bar.getAttribute('aria-valuenow')).toBe('2');
    expect(bar.getAttribute('aria-valuemax')).toBe('6');

    expect(view.getByTestId('waiting').textContent).toContain(
      '3 merged (Run now, Folder available again)',
    );
    expect(view.getByTestId('fact-trigger').textContent).toContain('Watches the left folder');
    expect(view.getByTestId('fact-roots').textContent).toContain('D:\\Photos');
    expect(view.getByText('Only while open')).toBeTruthy();

    expect(view.getByTestId('recent-runs').querySelectorAll('tr[data-run]')).toHaveLength(2);
    const events = view.getByTestId('events').querySelectorAll('li');
    expect(events).toHaveLength(2);
    expect(events[1].dataset.kind).toBe('mute');
    expect(view.getByRole('button', { name: 'Show earlier events' })).toBeTruthy();
  });

  it('shows at most 4 recent runs and 14 events', async () => {
    const d = photo();
    d.recentRuns = Array.from({ length: 9 }, (_, i) => ({ ...d.recentRuns[1], id: 2000 - i }));
    d.events.events = Array.from({ length: 30 }, (_, i) => ({
      id: 100 - i,
      atMs: 1760000000000 + i,
      kind: 'info' as const,
      text: `event ${i}`,
    }));
    const { store } = setup({}, d);
    const view = within(await openDetail(store));
    expect(view.getByTestId('recent-runs').querySelectorAll('tr[data-run]')).toHaveLength(4);
    expect(view.getByTestId('events').querySelectorAll('li')).toHaveLength(14);
  });

  it('draws the quiet countdown bar for a watch automation waiting for quiet', async () => {
    const d = photo();
    d.currentRun = undefined;
    d.quiet = { untilMs: Date.now() + 6000, remainingSecs: 6, idleSecs: 10 };
    const { store } = setup({}, d);
    const view = within(await openDetail(store));
    expect(view.getByTestId('quiet-bar')).toBeTruthy();
    expect(view.getByRole('progressbar', { name: 'Waiting for quiet' })).toBeTruthy();
  });

  it('a watch waiting for quiet: pipeline step 3, remaining seconds and the reset sub-line (WA-1.2, MH-5)', async () => {
    const d = fixture<AutomationDetail>('automation-get-quiet.json');
    // The countdown ticks from the backend's deadline: put it 7 s ahead of now.
    d.quiet = { ...d.quiet!, untilMs: Date.now() + 7000 };
    const { store } = setup({}, d);
    const view = within(await openDetail(store));
    expect(view.getByTestId('quiet-label').textContent).toMatch(/^Waiting for quiet · [67] s$/);
    const steps = [...view.getByTestId('pipeline').querySelectorAll('li')];
    expect(steps.map((li) => li.getAttribute('aria-current'))).toEqual([
      null,
      null,
      'step',
      null,
      null,
    ]);
    expect(steps[0].classList.contains('done')).toBe(true);
    expect(steps[3].classList.contains('done')).toBe(false);
    const label = view.getByTestId('quiet-label').textContent!;
    expect(label).toMatch(/^Waiting for quiet · [67] s$/);
    expect(view.getByTestId('quiet-sub').textContent).toBe(
      'Changes detected — new changes reset the countdown',
    );
    const bar = view.getByRole('progressbar', { name: 'Waiting for quiet' });
    expect(bar.getAttribute('aria-valuemax')).toBe('10');
    // The watch events of the backend are listed as written.
    expect(view.getByTestId('events').textContent).toContain(
      'Changes detected — waiting for 10 s without changes',
    );
    expect(view.queryByTestId('current-run')).toBeNull();
  });

  it('while quiet, the status chip and the quiet bar show the same countdown (one number on screen)', async () => {
    const d = fixture<AutomationDetail>('automation-get-quiet.json');
    // The backend snapshot still says 7 s, but the deadline is 3 s away: the chip must follow the bar.
    d.quiet = { ...d.quiet!, untilMs: Date.now() + 3000 };
    const { store } = setup({}, d);
    const view = within(await openDetail(store));
    const chip = document.querySelector('.chip[data-kind="quiet"] .label')!;
    const bar = view.getByTestId('quiet-label').textContent!;
    expect(chip.textContent).toBe(bar);
    expect(bar).toMatch(/^Waiting for quiet · [23] s$/);
    expect(view.queryByText('Waiting for quiet · 7 s')).toBeNull();
    // The explanation appears once (in the bar), not twice.
    expect(view.getAllByText('Changes detected — new changes reset the countdown')).toHaveLength(1);
  });

  it('Edit is available for a watch automation and opens its editor', async () => {
    const { store } = setup();
    const view = within(await openDetail(store));
    const edit = view.getByRole('button', { name: 'Edit' }) as HTMLButtonElement;
    expect(edit.disabled).toBe(false);
    await fireEvent.click(edit);
    await screen.findByTestId('watch-fields');
    expect(
      (screen.getByRole('checkbox', { name: 'Watch the left folder' }) as HTMLInputElement).checked,
    ).toBe(true);
  });

  it('says Stopping after the current file when the backend reports it, and disables Stop', async () => {
    const d = photo();
    d.automation.status = {
      kind: 'running',
      label: 'Running 2/6',
      sub: 'Stopping after the current file',
    };
    const { store } = setup({}, d);
    const view = within(await openDetail(store));
    expect(view.getByTestId('run-phase').textContent).toBe('Stopping after the current file\u2026');
    expect(
      (view.getByRole('button', { name: 'Stop current run' }) as HTMLButtonElement).disabled,
    ).toBe(true);
  });

  it('Comparing folders while the run compares', async () => {
    const d = photo();
    d.currentRun!.phase = 'compare';
    d.currentRun!.opsTotal = 0;
    const { store } = setup({}, d);
    const view = within(await openDetail(store));
    expect(view.getByTestId('run-phase').textContent).toBe('Comparing folders\u2026');
  });

  it('a progress event moves the bar and the counts without a reload', async () => {
    const { store, backend } = setup();
    const view = within(await openDetail(store));
    const calls = (backend.get as ReturnType<typeof vi.fn>).mock.calls.length;
    store.applyProgress({
      runId: 1044,
      automationId: ID,
      phase: 'sync',
      opsTotal: 6,
      counts: { ok: 3, failed: 0, notStarted: 3, held: 0, skippedChanged: 0 },
    });
    await waitFor(() =>
      expect(view.getByTestId('run-counts').textContent).toBe(
        '3 succeeded \u00b7 0 failed \u00b7 3 remaining',
      ),
    );
    expect(view.getByTestId('run-phase').textContent).toBe('Syncing 4 of 6');
    expect((backend.get as ReturnType<typeof vi.fn>).mock.calls.length).toBe(calls);
  });

  it('Run now is dimmed with the reason when the backend says it cannot run', async () => {
    const d = photo();
    d.automation.canRunNow = false;
    d.automation.runNowReason = 'Review the profile change before running';
    const { store, backend } = setup({}, d);
    const view = within(await openDetail(store));
    const btn = view.getByRole('button', { name: 'Run now' }) as HTMLButtonElement;
    expect(btn.disabled).toBe(true);
    expect(view.getByTestId('run-now-reason').textContent).toBe(
      'Review the profile change before running',
    );
    await fireEvent.click(btn);
    expect(backend.runNow).not.toHaveBeenCalled();
  });

  it('Run now asks the backend and shows the toast', async () => {
    const { store, backend } = setup({
      runNow: vi.fn(async () => fixture('automation-run-now-merged.json') as never),
    });
    const view = within(await openDetail(store));
    await fireEvent.click(view.getByRole('button', { name: 'Run now' }));
    expect(backend.runNow).toHaveBeenCalledWith(ID);
    await waitFor(() =>
      expect(toast.message).toBe(
        'Merged into the pending run for these folders \u2014 no duplicate job',
      ),
    );
  });

  it('FER-577 B4 R1: Alt+letters under an open Stop dialog never reach the page behind; Run now has no access letter', async () => {
    const { store, backend } = setup({
      stopRun: vi.fn(async () => ({ status: 'requested' as const })),
    });
    const view = within(await openDetail(store));
    expect(view.getByRole('button', { name: 'Run now' }).hasAttribute('data-mnemonic')).toBe(false);
    await fireEvent.click(view.getByRole('button', { name: 'Stop current run' }));
    const dlg = await screen.findByRole('alertdialog');
    for (const k of 'abcdefghijklmnopqrstuvwxyz')
      await fireEvent.keyDown(dlg, { key: k, altKey: true });
    expect(backend.runNow).not.toHaveBeenCalled();
    expect(backend.stopRun).not.toHaveBeenCalled();
    expect(store.view).toBe('detail');
  });

  it('Stop current run opens the Stop dialog with the no-rollback note; Stop run sends stop_run', async () => {
    const { store, backend } = setup({
      stopRun: vi.fn(async () => ({ status: 'requested' as const })),
    });
    const view = within(await openDetail(store));
    await fireEvent.click(view.getByRole('button', { name: 'Stop current run' }));
    const dlg = within(await screen.findByRole('alertdialog'));
    expect(dlg.getByRole('heading', { name: 'Stop the current run?' })).toBeTruthy();
    expect(screen.getByRole('alertdialog').textContent).toMatch(/there is no automatic\s+rollback/);
    expect(backend.stopRun).not.toHaveBeenCalled();
    await fireEvent.click(dlg.getByRole('button', { name: 'Stop run' }));
    expect(backend.stopRun).toHaveBeenCalledWith(1044);
    await waitFor(() => expect(screen.queryByRole('alertdialog')).toBeNull());
  });

  it('Pause with a run in progress asks with three buttons; the default is "Pause & let it finish"', async () => {
    const { store, backend } = setup({
      setEnabled: vi.fn(async () => fixture('automation-set-enabled-needs-choice.json') as never),
    });
    const view = within(await openDetail(store));
    await fireEvent.click(view.getByRole('button', { name: 'Pause' }));
    const dlg = within(await screen.findByRole('alertdialog'));
    const names = dlg.getAllByRole('button').map((b) => b.textContent?.replace(/\s+/g, ' ').trim());
    expect(names.sort()).toEqual(['Cancel', 'Pause & let it finish', 'Pause & stop current run']);
    await waitFor(() =>
      expect(document.activeElement).toBe(
        dlg.getByRole('button', { name: 'Pause & let it finish' }),
      ),
    );
    await fireEvent.click(dlg.getByRole('button', { name: 'Pause & let it finish' }));
    expect(backend.pause).toHaveBeenCalledWith(ID, 'finish');
    expect(backend.pause).toHaveBeenCalledTimes(1);
  });

  it('"Pause & stop current run" sends mode stop; Cancel and Escape send nothing', async () => {
    const { store, backend } = setup({
      setEnabled: vi.fn(async () => fixture('automation-set-enabled-needs-choice.json') as never),
    });
    const view = within(await openDetail(store));
    await fireEvent.click(view.getByRole('button', { name: 'Pause' }));
    let dlg = within(await screen.findByRole('alertdialog'));
    await fireEvent.click(dlg.getByRole('button', { name: 'Cancel' }));
    expect(screen.queryByRole('alertdialog')).toBeNull();

    await fireEvent.click(view.getByRole('button', { name: 'Pause' }));
    await screen.findByRole('alertdialog');
    await fireEvent.keyDown(screen.getByRole('alertdialog'), { key: 'Escape' });
    expect(screen.queryByRole('alertdialog')).toBeNull();
    expect(backend.pause).not.toHaveBeenCalled();
    // Escape closed the dialog only, not the detail page.
    expect(store.view).toBe('detail');

    await fireEvent.click(view.getByRole('button', { name: 'Pause' }));
    dlg = within(await screen.findByRole('alertdialog'));
    await fireEvent.click(dlg.getByRole('button', { name: 'Pause & stop current run' }));
    expect(backend.pause).toHaveBeenCalledWith(ID, 'stop');
  });

  it('pages the operation list at 200 and shows the stored limit', async () => {
    const ops = (from: number): RunOp[] =>
      Array.from({ length: 200 }, (_, i) => ({
        seq: from + i,
        kind: 'copyNew',
        side: 'right',
        relPath: `f${from + i}.txt`,
        status: 'done',
      }));
    const d = photo();
    d.currentRun!.opsTotal = 6000;
    const runOps = vi.fn(async (_r: number, offset: number): Promise<RunOpsPage> => ({
      ops: ops(offset),
      total: 6000,
      stored: 5000,
    }));
    const { store } = setup({ runOps }, d);
    const view = within(await openDetail(store));
    await fireEvent.click(view.getByRole('button', { name: 'Show operations' }));
    await waitFor(() =>
      expect(view.getByTestId('ops-range').textContent).toBe('Showing 1\u2013200 of 5000'),
    );
    expect(runOps).toHaveBeenCalledWith(1044, 0, 200);
    await fireEvent.click(view.getByRole('button', { name: 'Next 200' }));
    await waitFor(() =>
      expect(view.getByTestId('ops-range').textContent).toBe('Showing 201\u2013400 of 5000'),
    );
    expect(runOps).toHaveBeenLastCalledWith(1044, 200, 200);
    expect(view.getByText(/Showing the first 5000 of 6000 operations/)).toBeTruthy();
    await fireEvent.click(view.getByRole('button', { name: 'Previous 200' }));
    await waitFor(() =>
      expect(view.getByTestId('ops-range').textContent).toBe('Showing 1\u2013200 of 5000'),
    );
  });

  it('draws paths and error texts as text, never as markup', async () => {
    const d = photo();
    const runOps = vi.fn(async (): Promise<RunOpsPage> => ({
      ops: [
        {
          seq: 0,
          kind: 'update',
          side: 'right',
          relPath: '<img src=x onerror=alert(1)>.txt',
          status: 'failed',
          errorText: '<b>Access is denied</b>',
        },
      ],
      total: 1,
      stored: 1,
    }));
    const { store } = setup({ runOps }, d);
    const view = within(await openDetail(store));
    await fireEvent.click(view.getByRole('button', { name: 'Show operations' }));
    await waitFor(() =>
      expect(view.getByTestId('run-ops-1044').querySelectorAll('tr[data-seq]')).toHaveLength(1),
    );
    const el = view.getByTestId('run-ops-1044');
    expect(el.querySelector('img')).toBeNull();
    expect(el.querySelector('b')).toBeNull();
    expect(el.textContent).toContain('<img src=x onerror=alert(1)>.txt');
  });

  it('Resolve held run expands the held run in the recent runs', async () => {
    const d = photo();
    const held = fixture<{ runs: AutomationDetail['recentRuns'] }>('automation-history.json')
      .runs[0];
    d.currentRun = undefined;
    d.recentRuns = [held];
    d.automation.status = { kind: 'attention', label: 'Needs attention' };
    d.automation.attention = {
      kind: 'partial',
      runId: held.id,
      text: '1 failed \u00b7 1 not started',
    };
    const { store } = setup({}, d);
    const view = within(await openDetail(store));
    expect(view.getByTestId('attention-hold').textContent).toContain('1 failed');
    await fireEvent.click(view.getByRole('button', { name: 'Resolve held run' }));
    expect(await view.findByTestId('run-error')).toBeTruthy();
  });

  it('opens from the list by the automation name', async () => {
    const { store, backend } = setup({}, photo());
    await screen.findByText('Nightly projects backup');
    await fireEvent.click(screen.getByRole('button', { name: 'Photo watch' }));
    expect(await screen.findByTestId('automation-detail')).toBeTruthy();
    expect(backend.get).toHaveBeenCalledWith(ID);
    await fireEvent.click(screen.getByRole('button', { name: '\u2190 Automations' }));
    expect(store.view).toBe('list');
  });
});
