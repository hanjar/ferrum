import { test, expect, type Page } from '@playwright/test';

/**
 * FER-529 E2E: watch-folder automations in browser mode with a mock IPC (same pattern as
 * automation-schedule.e2e.ts and automation-history.e2e.ts). Pinned here: the watch fields of the
 * editor (idle delay clamp, a network root disabled with its note), "Simulate a change" as a pure
 * local preview that sends nothing, the approve call carrying the watch draft, and the detail page
 * following a watch through Watching -> Waiting for quiet -> Queued -> Running when the host says
 * the store changed (mock events). The Rust side has its own suites.
 */

type Calls = { cmd: string; args: Record<string, unknown> }[];

const PROFILE_ID = '3f2a9c1e-5b6d-4c8a-9e0f-1a2b3c4d5e6f';
const NEW_ID = '7d1c2b3a-4e5f-4a6b-8c7d-9e0f1a2b3c4d';
const WATCH_ID = 'b2c3d4e5-f6a7-4b8c-9d0e-1f2a3b4c5d6e';

function injectTauriMock(page: Page) {
  page.addInitScript(
    ([profileId, newId, watchId]: [string, string, string]) => {
      const w = window as unknown as Record<string, unknown>;
      const calls: { cmd: string; args: Record<string, unknown> }[] = [];
      const listeners: Record<string, number> = {};
      const mock = { phase: 'watching' as 'watching' | 'quiet' | 'queued' | 'running' };
      w.__calls = calls;
      w.__mock = mock;
      w.__emit = (event: string, payload: unknown) => {
        const cb = w[`_${listeners[event]}`] as ((e: unknown) => void) | undefined;
        cb?.({ event, id: 0, payload });
      };

      const watchTrigger = {
        kind: 'watch',
        watchLeft: true,
        watchRight: false,
        onChanges: true,
        onAvailable: true,
        idleSecs: 10,
        maxWaitSecs: 900,
      };
      const policy = {
        trigger: watchTrigger,
        runtime: 'app',
        retries: 2,
        conditions: { skipOnBattery: false, skipOnMetered: false },
      };
      const statusOf = () => {
        switch (mock.phase) {
          case 'quiet':
            return {
              kind: 'quiet',
              label: 'Waiting for quiet \u00b7 7 s',
              sub: 'Runs after 10 s without changes',
            };
          case 'queued':
            return { kind: 'queued', label: 'Queued', sub: 'Starting' };
          case 'running':
            return { kind: 'running', label: 'Running 2/6', sub: 'Changes detected' };
          default:
            return { kind: 'watching', label: 'Watching', sub: 'Monitoring idle' };
        }
      };
      const currentStep = () =>
        ({ watching: 0, quiet: 2, queued: 3, running: 4 })[mock.phase] as number;
      const row = () => ({
        id: watchId,
        name: 'Photo watch',
        kind: 'watch',
        profileId,
        profileName: 'Photos to backup',
        profileRevision: 3,
        approvedRevision: 3,
        enabled: true,
        configRev: 3,
        status: statusOf(),
        trigger:
          'Watches the left folder \u00b7 runs after 10 s without changes \u00b7 runs when a folder comes back',
        runtime: 'app',
        runtimeNote: 'Only while open',
        canRunNow: true,
        showReview: false,
      });
      const run = {
        id: 1,
        automationId: watchId,
        automationName: 'Photo watch',
        profileId,
        profileRevision: 3,
        host: 'app',
        triggerKind: 'watch',
        triggerText: 'Changes detected',
        mergedCount: 0,
        startedMs: Date.now() - 4000,
        phase: 'sync',
        opsTotal: 6,
        counts: { ok: 2, failed: 0, notStarted: 0, held: 0, skippedChanged: 0 },
        result: { kind: 'running', label: 'Running 2/6' },
        needsAttention: false,
      };
      const events = () => {
        const list: Record<string, unknown>[] = [
          { id: 1, atMs: Date.now() - 60000, kind: 'info', text: 'Monitoring resumed' },
        ];
        if (mock.phase !== 'watching') {
          list.unshift({
            id: 2,
            atMs: Date.now() - 5000,
            kind: 'info',
            text: 'Changes detected \u2014 waiting for 10 s without changes',
          });
        }
        return list;
      };

      const profileSummary = {
        id: profileId,
        name: 'Photos to backup',
        revision: 3,
        left: 'C:\\Users\\demo\\Photos',
        right: '\\\\nas\\photos',
        modeLabel: 'Update left \u2192 right',
        deleteLabel: 'Recycle Bin',
        readiness: { status: 'ready' },
        usedBy: 0,
      };
      const fail = (code: string) => Promise.reject({ code, message: code });
      let approved: Record<string, unknown> | null = null;

      (w as { __TAURI_INTERNALS__: unknown }).__TAURI_INTERNALS__ = {
        metadata: {
          currentWindow: { label: 'main' },
          currentWebview: { windowLabel: 'main', label: 'main' },
        },
        plugins: {},
        transformCallback: (cb: unknown) => {
          const id = Math.floor(Math.random() * 1e9);
          w[`_${id}`] = cb;
          return id;
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
              return Promise.resolve([profileSummary]);
            case 'automation_list':
              return Promise.resolve(approved ? [row(), approved] : [row()]);
            case 'automation_get': {
              if (args.id !== watchId) return fail('not_found');
              const quiet =
                mock.phase === 'quiet'
                  ? { untilMs: Date.now() + 7000, remainingSecs: 7, idleSecs: 10 }
                  : undefined;
              return Promise.resolve({
                automation: row(),
                policy,
                profile: {
                  id: profileId,
                  name: 'Photos to backup',
                  revision: 3,
                  left: 'C:\\Users\\demo\\Photos',
                  right: '\\\\nas\\photos',
                },
                pipeline: {
                  steps: ['Watching', 'Changes detected', 'Waiting for quiet', 'Queued', 'Running'],
                  current: currentStep(),
                },
                quiet,
                pendingRerun: false,
                currentRun: mock.phase === 'running' ? run : undefined,
                waiting:
                  mock.phase === 'queued'
                    ? [
                        {
                          id: 7,
                          automationId: watchId,
                          automationName: 'Photo watch',
                          triggerKind: 'watch',
                          triggerText: 'Changes detected',
                          mergedCount: 0,
                          mergedReasons: [],
                          enqueuedMs: Date.now(),
                          hostClass: 'app',
                        },
                      ]
                    : [],
                recentRuns: [],
                events: { events: events() },
              });
            }
            case 'automation_validate':
              return Promise.resolve({
                errors: [],
                checks: [
                  { id: 'profile', status: 'ok', text: 'The sync profile is ready.' },
                  {
                    id: 'watch',
                    status: 'ok',
                    text: 'Change notifications work for the left folder.',
                  },
                ],
                next: [],
              });
            case 'automation_test':
              return Promise.resolve({
                testId: 'c0ffee00-1234-4abc-8def-0123456789ab',
                automationId: (args.id as string | null) ?? newId,
                policyHash: 'h',
                passed: true,
                steps: [
                  { id: 'profile', status: 'ok', text: 'Sync profile is valid and ready.' },
                  {
                    id: 'compare',
                    status: 'ok',
                    text: 'Comparison: 6 operations (2 update, 4 new, 0 delete).',
                  },
                ],
              });
            case 'automation_approve': {
              const draft = args.draft as { name: string; policy: unknown };
              const id = (args.id as string | null) ?? newId;
              if (id !== watchId) {
                approved = { ...row(), id, name: draft.name, status: statusOf() };
              }
              return Promise.resolve({
                status: 'approved',
                automation: {
                  id,
                  name: draft.name,
                  kind: 'watch',
                  profileId,
                  profileName: 'Photos to backup',
                  profileRevision: 3,
                  policy: draft.policy,
                  enabled: true,
                  configRev: 1,
                  createdMs: 1,
                  updatedMs: 1,
                },
              });
            }
            default:
              return Promise.reject(new Error(`Unknown command: ${cmd}`));
          }
        },
      };
    },
    [PROFILE_ID, NEW_ID, WATCH_ID] as [string, string, string],
  );
}

async function boot(page: Page) {
  injectTauriMock(page);
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

const allCalls = (page: Page) =>
  page.evaluate(() => (window as unknown as { __calls: Calls }).__calls.map((c) => c.cmd));

async function openPage(page: Page) {
  await page
    .getByRole('group', { name: 'Sync & Backup' })
    .getByRole('button', { name: 'Automation' })
    .click();
  await expect(page.getByRole('dialog', { name: 'Automation' })).toBeVisible();
}

/** Tell the page that the store changed (what the host does after a trigger or a run). */
const phase = (page: Page, next: 'watching' | 'quiet' | 'queued' | 'running') =>
  page.evaluate((p) => {
    const win = window as unknown as {
      __mock: { phase: string };
      __emit: (e: string, payload: unknown) => void;
    };
    win.__mock.phase = p;
    win.__emit('automation://changed', { seq: Date.now() });
  }, next);

test.describe('FER-529 watch automations (mock IPC)', () => {
  let page: Page;

  test.beforeEach(async ({ browser }) => {
    page = await browser.newPage();
    await page.setViewportSize({ width: 1280, height: 900 });
  });

  test.afterEach(async () => {
    await page.close();
  });

  test('editor: idle delay clamp, network root disabled, Simulate a change sends nothing, approve sends the watch draft', async () => {
    await boot(page);
    await openPage(page);
    await page.getByRole('button', { name: 'New automation' }).click();
    await page.getByRole('radio', { name: 'When files change' }).click();
    await expect(page.getByTestId('watch-fields')).toBeVisible();
    await expect(page.getByTestId('watch-note')).toHaveText(
      'File notifications only trigger a fresh comparison by the sync engine; they are never the list of what gets copied.',
    );

    await page.getByRole('textbox', { name: 'Name' }).fill('Photo watch 2');
    await page.getByRole('radio', { name: /Photos to backup/ }).click();

    // The right folder is a network share: disabled, unticked, with the WA-2.3 note.
    const right = page.getByRole('checkbox', { name: 'Watch the right folder' });
    await expect(right).toBeDisabled();
    await expect(right).not.toBeChecked();
    await expect(page.getByTestId('unsupported-right')).toHaveText(
      "No change notifications on this provider \u2014 can't be watched",
    );
    await expect(page.getByRole('checkbox', { name: 'Watch the left folder' })).toBeEnabled();

    // Idle delay: 2..3600.
    const idle = page.getByRole('textbox', { name: 'Idle delay' });
    await idle.fill('1');
    await idle.blur();
    await expect(idle).toHaveValue('2');
    await idle.fill('99999');
    await idle.blur();
    await expect(idle).toHaveValue('3600');
    await idle.fill('2');
    await idle.blur();
    await expect(idle).toHaveValue('2');

    const cont = page.getByRole('button', { name: 'Continue to review' });
    await expect(cont).toBeEnabled();

    // "Simulate a change": a local countdown that sends nothing.
    const before = (await allCalls(page)).length;
    await page.getByRole('button', { name: 'Simulate a change' }).click();
    await expect(page.getByTestId('watch-preview-count')).toHaveText(
      /Waiting for quiet \u00b7 [12] s/,
    );
    await expect(page.getByTestId('watch-preview-text')).toContainText(
      'Any new change resets the countdown to 2 s',
    );
    await expect(page.getByTestId('watch-preview-text')).toContainText(
      'Preview only \u2014 1 change batched into one run',
      { timeout: 6000 },
    );
    expect((await allCalls(page)).slice(before)).toEqual([]);

    await cont.click();
    await expect(page.getByTestId('automation-review')).toContainText(
      'Watches the left folder \u00b7 runs after 2 s without changes \u00b7 runs when a folder comes back',
    );
    await expect(page.getByTestId('automation-review')).toContainText(
      'every new change restarts the 2 s countdown',
    );
    await page.getByRole('button', { name: 'Run test' }).click();
    await expect(page.getByTestId('test-passed')).toBeVisible();
    await page.getByRole('button', { name: 'Approve & enable' }).click();
    await expect(page.getByRole('tab', { name: /Automations \(2\)/ })).toBeVisible();

    const sent = await calls(page, 'automation_approve');
    expect(sent).toHaveLength(1);
    expect(sent[0].id).toBeNull();
    expect((sent[0].draft as { policy: { trigger: unknown } }).policy.trigger).toEqual({
      kind: 'watch',
      watchLeft: true,
      watchRight: false,
      onChanges: true,
      onAvailable: true,
      idleSecs: 2,
      maxWaitSecs: 900,
    });
    // The preview and the approval never ran or queued anything.
    expect(await calls(page, 'automation_run_now')).toHaveLength(0);
  });

  test('detail: the pipeline follows the host events, the quiet bar counts down, Edit opens the watch editor', async () => {
    await boot(page);
    await openPage(page);
    await page.getByRole('button', { name: 'Photo watch', exact: true }).click();
    await expect(page.getByTestId('automation-detail')).toBeVisible();

    const current = page.locator('[data-testid="pipeline"] li[aria-current="step"]');
    await expect(current).toHaveText('Watching');
    await expect(page.getByTestId('quiet-bar')).toHaveCount(0);

    // A change was seen: waiting for quiet, with the countdown and its sub-line (WA-1.2).
    await phase(page, 'quiet');
    await expect(current).toHaveText('Waiting for quiet');
    await expect(page.getByTestId('quiet-bar')).toBeVisible();
    await expect(page.getByTestId('quiet-label')).toHaveText(/^Waiting for quiet \u00b7 [67] s$/);
    await expect(page.getByTestId('quiet-sub')).toHaveText(
      'Changes detected \u2014 new changes reset the countdown',
    );
    await expect(page.getByTestId('events')).toContainText(
      'Changes detected \u2014 waiting for 10 s without changes',
    );

    // The countdown moves on its own between the host's events.
    await expect
      .poll(async () => (await page.getByTestId('quiet-label').textContent()) ?? '', {
        timeout: 4000,
      })
      .toMatch(/\u00b7 [3-5] s$/);

    await phase(page, 'queued');
    await expect(current).toHaveText('Queued');
    await expect(page.getByTestId('quiet-bar')).toHaveCount(0);
    await expect(page.getByTestId('waiting')).toContainText('Changes detected');

    await phase(page, 'running');
    await expect(current).toHaveText('Running');
    await expect(page.getByTestId('current-run')).toBeVisible();

    await phase(page, 'watching');
    await expect(current).toHaveText('Watching');
    await expect(page.getByTestId('current-run')).toHaveCount(0);

    // Edit is open for a watch: the editor loads its fields, a change is approved with the stored id.
    await page.getByRole('button', { name: 'Edit', exact: true }).click();
    await expect(page.getByTestId('watch-fields')).toBeVisible();
    await expect(page.getByRole('checkbox', { name: 'Watch the left folder' })).toBeChecked();
    await expect(page.getByRole('textbox', { name: 'Idle delay' })).toHaveValue('10');
    await expect(page.getByRole('textbox', { name: 'Longest wait' })).toHaveValue('15');
    // The kind of a stored automation cannot be switched.
    await expect(page.getByRole('radio', { name: 'On a schedule' })).toBeDisabled();

    const idle = page.getByRole('textbox', { name: 'Idle delay' });
    await idle.fill('45');
    await idle.blur();
    const cont = page.getByRole('button', { name: 'Continue to review' });
    await expect(cont).toBeEnabled();
    await cont.click();
    await page.getByRole('button', { name: 'Run test' }).click();
    await expect(page.getByTestId('test-passed')).toBeVisible();
    await page.getByRole('button', { name: 'Approve & save' }).click();

    const sent = await calls(page, 'automation_approve');
    expect(sent).toHaveLength(1);
    expect(sent[0].id).toBe(WATCH_ID);
    expect(sent[0].expectedConfigRev).toBe(3);
    expect(
      (sent[0].draft as { policy: { trigger: { idleSecs: number } } }).policy.trigger.idleSecs,
    ).toBe(45);
  });
});
