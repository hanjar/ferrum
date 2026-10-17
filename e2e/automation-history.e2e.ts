import { test, expect, type Page } from '@playwright/test';

/**
 * FER-527 E2E: Automation detail page, run progress, run history and the Pause / Stop dialogs in
 * browser mode with a mock IPC (same pattern as automation-schedule.e2e.ts). The Rust commands
 * have their own suites; here we pin the UI contract: a partial run -> Recheck -> Retry remaining
 * -> resolved loop, a conflict that is only ever reviewed in Sync, live progress, and the Stop and
 * Pause dialogs sending exactly `automation_stop_run{runId}` / `automation_pause{id, mode}`.
 */

type Calls = { cmd: string; args: Record<string, unknown> }[];

const PROFILE_ID = '3f2a9c1e-5b6d-4c8a-9e0f-1a2b3c4d5e6f';
const AUTOMATION_ID = '7d1c2b3a-4e5f-4a6b-8c7d-9e0f1a2b3c4d';

function injectTauriMock(page: Page, scenario: 'partial' | 'conflict' | 'running') {
  page.addInitScript(
    ([scen, profileId, id]: [string, string, string]) => {
      const w = window as unknown as Record<string, unknown>;
      const calls: { cmd: string; args: Record<string, unknown> }[] = [];
      const listeners: Record<string, number> = {};
      const mock = { enableAnswer: 'needsChoice' as 'needsChoice' | 'enabled' };
      w.__calls = calls;
      w.__mock = mock;
      w.__emit = (event: string, payload: unknown) => {
        const cb = w[`_${listeners[event]}`] as ((e: unknown) => void) | undefined;
        cb?.({ event, id: 0, payload });
      };

      const counts = { ok: 4, failed: 1, notStarted: 1, held: 0, skippedChanged: 0 };
      const base = {
        id: 1043,
        automationId: id,
        automationName: 'Documents mirror',
        profileId,
        profileRevision: 3,
        host: 'app',
        triggerKind: 'schedule',
        triggerText: 'Scheduled \u00b7 02:00 WIB',
        mergedCount: 0,
        startedMs: 1760000100000,
        endedMs: 1760000130000,
        phase: 'done',
        opsTotal: 6,
        counts,
        errorText: 'Access is denied',
        note: 'Not retried automatically \u2014 permission errors need you. Fix access, then Recheck.',
        needsAttention: true,
      };
      const state: { run: Record<string, unknown>; resolved: boolean } = {
        resolved: false,
        run:
          scen === 'conflict'
            ? {
                ...base,
                outcome: 'conflict',
                counts: { ok: 5, failed: 0, notStarted: 0, held: 1, skippedChanged: 0 },
                errorText: undefined,
                note: 'Conflicts are held, never resolved unattended.',
                result: {
                  kind: 'attention',
                  label: 'Conflict held',
                  sub: '1 conflict held for review',
                },
              }
            : {
                ...base,
                outcome: 'partial',
                result: {
                  kind: 'partial',
                  label: 'Partial',
                  sub: '4 succeeded \u00b7 1 failed \u00b7 1 not started',
                },
              },
      };
      const running = {
        id: 1050,
        automationId: id,
        automationName: 'Documents mirror',
        profileId,
        profileRevision: 3,
        host: 'app',
        triggerKind: 'schedule',
        triggerText: 'Scheduled \u00b7 02:00 WIB',
        mergedCount: 0,
        startedMs: Date.now() - 5000,
        phase: 'sync',
        opsTotal: 6,
        counts: { ok: 2, failed: 0, notStarted: 0, held: 0, skippedChanged: 0 },
        result: { kind: 'running', label: 'Running 2/6' },
        needsAttention: false,
      };

      const policy = {
        trigger: {
          kind: 'schedule',
          freq: { kind: 'daily' },
          time: '02:00',
          tz: 'Asia/Jakarta',
          missed: 'skip',
        },
        runtime: 'app',
        retries: 2,
        conditions: { skipOnBattery: false, skipOnMetered: false },
      };
      const row = () => ({
        id,
        name: 'Documents mirror',
        kind: 'schedule',
        profileId,
        profileName: 'Documents',
        profileRevision: 3,
        approvedRevision: 3,
        enabled: true,
        configRev: 1,
        status:
          scen === 'running'
            ? { kind: 'running', label: 'Running 2/6' }
            : state.resolved
              ? { kind: 'scheduled', label: 'Scheduled', sub: 'Next Sat 17 Oct, 02:00 WIB' }
              : { kind: 'attention', label: 'Needs attention', sub: 'new runs held' },
        trigger: 'Daily at 02:00 (Asia/Jakarta)',
        runtime: 'app',
        runtimeNote: 'Only while open',
        canRunNow: !(scen !== 'running' && !state.resolved),
        runNowReason:
          scen !== 'running' && !state.resolved
            ? 'A held run needs your attention first.'
            : undefined,
        showReview: false,
        attention:
          scen !== 'running' && !state.resolved
            ? {
                kind: scen === 'conflict' ? 'conflict' : 'partial',
                runId: 1043,
                text: '1 failed \u00b7 1 not started',
              }
            : undefined,
      });
      const allRuns = () => (scen === 'running' ? [] : [state.run]);
      const summary = {
        id: profileId,
        name: 'Documents',
        revision: 3,
        left: 'C:\\Users\\demo\\Documents',
        right: 'E:\\Backups\\Documents',
        modeLabel: 'Update left \u2192 right',
        deleteLabel: 'Recycle Bin',
        readiness: { status: 'ready' },
        usedBy: 1,
      };
      const detail = () => ({
        automation: row(),
        policy,
        profile: {
          id: profileId,
          name: 'Documents',
          revision: 3,
          left: summary.left,
          right: summary.right,
        },
        pipeline: {
          steps: ['Scheduled', 'Queued', 'Running', 'Finished'],
          current: scen === 'running' ? 2 : 0,
        },
        pendingRerun: false,
        currentRun: scen === 'running' ? running : undefined,
        waiting: [],
        recentRuns: scen === 'running' ? [running] : allRuns(),
        events: {
          events: [
            { id: 2, atMs: 1760000130000, kind: 'warn', text: 'Run #1043 partial', runId: 1043 },
            { id: 1, atMs: 1760000100000, kind: 'info', text: 'Run #1043 started' },
          ],
        },
      });
      const ops = [
        { seq: 0, kind: 'copyNew', side: 'right', relPath: 'docs\\a.docx', status: 'done' },
        {
          seq: 1,
          kind: 'update',
          side: 'right',
          relPath: 'docs\\b.xlsx',
          status: 'failed',
          errorText: 'Access is denied',
        },
        {
          seq: 2,
          kind: 'createFolder',
          side: 'right',
          relPath: 'docs\\archive',
          status: 'notStarted',
        },
      ];

      (w as { __TAURI_INTERNALS__: unknown }).__TAURI_INTERNALS__ = {
        metadata: {
          currentWindow: { label: 'main' },
          currentWebview: { windowLabel: 'main', label: 'main' },
        },
        plugins: {},
        transformCallback: (cb: unknown) => {
          const n = Math.floor(Math.random() * 1e9);
          w[`_${n}`] = cb;
          return n;
        },
        invoke: (cmd: string, args: Record<string, unknown> = {}) => {
          calls.push({ cmd, args });
          switch (cmd) {
            case 'plugin:path|resolve_directory':
              return Promise.resolve('C:\\Users\\demo');
            case 'plugin:event|listen':
              listeners[args.event as string] = args.handler as number;
              return Promise.resolve(Math.floor(Math.random() * 1e9));
            case 'load_prefs':
              return Promise.resolve({ theme: 'dark', palette: 'violet', paneSplit: 0.5 });
            case 'plugin:event|unlisten':
            case 'save_prefs':
            case 'validate_sync_roots':
              return Promise.resolve(null);
            case 'watch_start':
            case 'watch_stop':
              return Promise.resolve(1);
            case 'list_dir':
              return Promise.resolve([
                {
                  name: 'a.txt',
                  path: 'C:\\Users\\demo\\a.txt',
                  isDir: false,
                  isSymlink: false,
                  size: 10,
                  modifiedMs: 1700000000000,
                  hidden: false,
                },
              ]);
            case 'list_drives':
              return Promise.resolve([]);
            case 'sync_profile_list':
              return Promise.resolve([summary]);
            case 'automation_list':
              return Promise.resolve([row()]);
            case 'automation_get':
              return Promise.resolve(detail());
            case 'automation_history': {
              const runs = allRuns().filter((r) =>
                args.filter === 'attention' ? r.needsAttention === true : true,
              );
              return Promise.resolve({ runs });
            }
            case 'automation_run_ops':
              return Promise.resolve({ ops, total: 6, stored: 3 });
            case 'automation_recheck':
              state.run = {
                ...state.run,
                recheck:
                  scen === 'conflict'
                    ? {
                        state: 'conflict',
                        remaining: 1,
                        text: 'The file still changed on both sides. Decide in Sync \u2014 Ferrum won\u2019t choose a version.',
                        atMs: 1760000140000,
                      }
                    : {
                        state: 'remaining',
                        remaining: 2,
                        text: 'Fresh comparison: 4 earlier operations are in sync and won\u2019t be repeated \u00b7 2 operations remaining.',
                        atMs: 1760000140000,
                      },
              };
              return Promise.resolve({ status: 'requested' });
            case 'automation_retry':
              state.run = {
                ...state.run,
                needsAttention: false,
                recheck: undefined,
                resolvedBy: 1044,
                resolvedText: 'Resolved by run #1044',
              };
              state.resolved = true;
              return Promise.resolve({ status: 'queued' });
            case 'automation_run_now':
              return Promise.resolve({ status: 'waiting', waitsFor: 'Nightly photos to NAS' });
            case 'automation_set_enabled':
              return Promise.resolve({ status: mock.enableAnswer });
            case 'automation_pause':
              return Promise.resolve({ status: 'paused' });
            case 'automation_stop_run':
              return Promise.resolve({ status: 'requested' });
            default:
              // Everything else (compare_dirs, lease, baseline, disk usage...) is unavailable here.
              return Promise.reject({ code: 'unknown', message: `Unknown command: ${cmd}` });
          }
        },
      };
    },
    [scenario, PROFILE_ID, AUTOMATION_ID] as [string, string, string],
  );
}

async function boot(page: Page, scenario: 'partial' | 'conflict' | 'running') {
  injectTauriMock(page, scenario);
  await page.goto('/');
  await page.waitForSelector('[data-pane="0"] .row[data-path]', {
    state: 'attached',
    timeout: 10_000,
  });
}

const calls = (page: Page, cmd: string) =>
  page.evaluate(
    (c) =>
      ((window as unknown as { __calls: Calls }).__calls ?? [])
        .filter((x) => x.cmd === c)
        .map((x) => x.args),
    cmd,
  );

async function openPage(page: Page) {
  await page
    .getByRole('group', { name: 'Sync & Backup' })
    .getByRole('button', { name: 'Automation' })
    .click();
  await expect(page.getByRole('dialog', { name: 'Automation' })).toBeVisible();
}

test.describe('FER-527 Automation detail, history and dialogs (mock IPC)', () => {
  let page: Page;

  test.beforeEach(async ({ browser }) => {
    page = await browser.newPage();
    await page.setViewportSize({ width: 1280, height: 900 });
  });

  test.afterEach(async () => {
    await page.close();
  });

  test('partial run: Recheck, Retry remaining 2, resolved', async () => {
    await boot(page, 'partial');
    await openPage(page);
    await page.getByRole('tab', { name: /Run history/ }).click();
    await page.getByRole('button', { name: 'Needs attention' }).click();
    await page.getByRole('button', { name: 'Run #1043, expand' }).click();

    await expect(page.getByTestId('run-error')).toHaveText('Error: Access is denied');
    await expect(page.getByTestId('run-note')).toContainText('Not retried automatically');
    await expect(page.locator('tr[data-seq]')).toHaveCount(3);
    // Retry is not offered before a recheck found something remaining.
    await expect(page.getByRole('button', { name: /Retry remaining/ })).toHaveCount(0);

    await page.getByRole('button', { name: 'Recheck' }).click();
    expect(await calls(page, 'automation_recheck')).toEqual([{ runId: 1043 }]);
    await expect(page.getByTestId('run-recheck')).toContainText('2 operations remaining.');

    await page.getByRole('button', { name: 'Retry remaining 2' }).click();
    expect(await calls(page, 'automation_retry')).toEqual([{ runId: 1043 }]);
    await expect(page.getByText('Queued \u2014 starts shortly')).toBeVisible();
    // The run is resolved: no longer in the attention filter.
    await expect(page.getByTestId('history-empty')).toBeVisible();
    await page.getByRole('button', { name: 'All runs' }).click();
    // The row stays expanded across the reload.
    await expect(page.getByTestId('run-resolved')).toHaveText('Resolved by run #1044');
    await expect(page.getByRole('button', { name: /Retry remaining/ })).toHaveCount(0);
  });

  test('conflict: Review in Sync opens Synchronize Dirs for the profile roots, no Retry', async () => {
    await boot(page, 'conflict');
    await openPage(page);
    await page.getByRole('tab', { name: /Run history/ }).click();
    await page.getByRole('button', { name: 'Run #1043, expand' }).click();
    await page.getByRole('button', { name: 'Recheck' }).click();
    await expect(page.getByTestId('run-recheck')).toContainText('Decide in Sync');
    await expect(page.getByRole('button', { name: /Retry remaining/ })).toHaveCount(0);

    await page.getByRole('button', { name: 'Review in Sync \u2197' }).click();
    await expect(
      page.getByRole('complementary', { name: 'Synchronize Directories' }),
    ).toBeVisible();
    await expect(page.getByRole('dialog', { name: 'Automation' })).toHaveCount(0);
    const compared = await calls(page, 'compare_dirs');
    expect(compared.length).toBeGreaterThan(0);
    expect(compared[0].left).toBe('C:\\Users\\demo\\Documents');
    expect(compared[0].right).toBe('E:\\Backups\\Documents');
    // Review in Sync is a preview only: nothing is copied, deleted, leased or queued by it.
    const all = await page.evaluate(() =>
      (window as unknown as { __calls: Calls }).__calls.map((c) => c.cmd),
    );
    const forbidden = all.filter(
      (c) =>
        c !== 'copy_control_snapshot' &&
        /copy|move|delete|sync_baseline_save|sync_lease_acquire|automation_run_now|automation_retry/.test(
          c,
        ),
    );
    expect(forbidden).toEqual([]);
  });

  test('detail page: live progress, Stop dialog and Pause dialog', async () => {
    await boot(page, 'running');
    await openPage(page);
    await page.getByRole('button', { name: 'Documents mirror', exact: true }).click();
    await expect(page.getByTestId('automation-detail')).toBeVisible();
    await expect(page.getByTestId('run-phase')).toHaveText('Syncing 3 of 6');
    await expect(page.getByTestId('run-counts')).toHaveText(
      '2 succeeded \u00b7 0 failed \u00b7 4 remaining',
    );

    await page.evaluate(() =>
      (window as unknown as { __emit: (e: string, p: unknown) => void }).__emit(
        'automation://progress',
        {
          runId: 1050,
          automationId: '7d1c2b3a-4e5f-4a6b-8c7d-9e0f1a2b3c4d',
          phase: 'sync',
          opsTotal: 6,
          counts: { ok: 4, failed: 1, notStarted: 1, held: 0, skippedChanged: 0 },
        },
      ),
    );
    await expect(page.getByTestId('run-counts')).toHaveText(
      '4 succeeded \u00b7 1 failed \u00b7 1 remaining',
    );
    await expect(page.getByTestId('run-phase')).toHaveText('Syncing 6 of 6');

    // Run now is a request; the backend's answer is the toast (RQ-4).
    await page.getByRole('button', { name: 'Run now', exact: true }).click();
    expect(await calls(page, 'automation_run_now')).toEqual([
      { id: '7d1c2b3a-4e5f-4a6b-8c7d-9e0f1a2b3c4d' },
    ]);
    await expect(
      page.getByText('Waiting for “Nightly photos to NAS” — same folders'),
    ).toBeVisible();

    // Stop: the dialog explains there is no rollback; nothing is sent until "Stop run".
    await page.getByRole('button', { name: 'Stop current run' }).click();
    const stop = page.getByRole('alertdialog');
    await expect(stop.getByRole('heading', { name: 'Stop the current run?' })).toBeVisible();
    await expect(stop).toContainText('no automatic rollback');
    expect(await calls(page, 'automation_stop_run')).toHaveLength(0);
    await stop.getByRole('button', { name: 'Stop run' }).click();
    expect(await calls(page, 'automation_stop_run')).toEqual([{ runId: 1050 }]);
    await expect(page.getByRole('alertdialog')).toHaveCount(0);

    // Pause with a run in progress: three buttons, the default sends mode "finish".
    await page.getByRole('button', { name: 'Pause', exact: true }).click();
    const pause = page.getByRole('alertdialog');
    await expect(pause.getByRole('button', { name: 'Pause & let it finish' })).toBeFocused();
    await expect(pause.getByRole('button', { name: 'Pause & stop current run' })).toBeVisible();
    await expect(pause.getByRole('button', { name: 'Cancel' })).toBeVisible();
    await pause.getByRole('button', { name: 'Pause & let it finish' }).click();
    expect(await calls(page, 'automation_pause')).toEqual([
      { id: '7d1c2b3a-4e5f-4a6b-8c7d-9e0f1a2b3c4d', mode: 'finish' },
    ]);
  });

  test('Run now is dimmed with the backend reason while a run is held; Resolve opens the detail', async () => {
    await boot(page, 'partial');
    await openPage(page);
    const runNow = page.getByRole('button', { name: 'Run now: Documents mirror' });
    await expect(runNow).toBeDisabled();
    await expect(runNow).toHaveAttribute('title', 'A held run needs your attention first.');
    expect(await calls(page, 'automation_run_now')).toHaveLength(0);

    // Resolve held run goes to the detail page where the run is expanded.
    await page.getByRole('button', { name: 'Resolve' }).click();
    await expect(page.getByTestId('attention-hold')).toContainText('1 failed');
    await page.getByRole('button', { name: 'Resolve held run' }).click();
    await expect(page.getByTestId('run-error')).toBeVisible();
  });
});
