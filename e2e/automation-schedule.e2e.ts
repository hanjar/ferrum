import { test, expect, type Page } from '@playwright/test';

/**
 * FER-526 E2E: Automation page (scheduled automations) in browser mode with a mock IPC, the same
 * pattern as sync-profiles.e2e.ts. The Rust commands have their own suites; here we pin the UI
 * contract end to end: create -> validate -> review -> test -> approve, the approve call carrying
 * only {draft, testId}, `canceled` / `stale` outcomes, and the re-review list after a profile
 * change. The mock answers synchronously from in-page state with no timers. The UI is behind a
 * default-off flag; playwright.config.ts turns it on for the dev server.
 */

type Calls = { cmd: string; args: Record<string, unknown> }[];

const PROFILE_ID = '3f2a9c1e-5b6d-4c8a-9e0f-1a2b3c4d5e6f';
const AUTOMATION_ID = '7d1c2b3a-4e5f-4a6b-8c7d-9e0f1a2b3c4d';
const REVIEW_ID = 'b2c3d4e5-f6a7-4b8c-9d0e-1f2a3b4c5d6e';

function injectTauriMock(page: Page, seedReview: boolean) {
  page.addInitScript(
    ([withReview, profileId, newId, reviewId]: [boolean, string, string, string]) => {
      const w = window as unknown as Record<string, unknown>;
      const dir = 'C:\\Users\\demo';
      const calls: { cmd: string; args: Record<string, unknown> }[] = [];
      const mock = { approve: 'approved' as 'approved' | 'canceled' | 'stale', testPasses: true };
      w.__calls = calls;
      w.__mock = mock;

      const policy = (name: string, trigger: unknown) => ({
        name,
        profileId,
        policy: {
          trigger,
          runtime: 'app',
          retries: 2,
          conditions: { skipOnBattery: false, skipOnMetered: false },
        },
      });
      const watchTrigger = {
        kind: 'watch',
        watchLeft: true,
        watchRight: false,
        onChanges: true,
        onAvailable: true,
        idleSecs: 10,
        maxWaitSecs: 900,
      };
      const baseRow = (id: string, name: string, kind: string, trigger: string) => ({
        id,
        name,
        kind,
        profileId,
        profileName: 'Projects backup',
        profileRevision: 3,
        approvedRevision: 3,
        enabled: true,
        configRev: 1,
        status: { kind: 'scheduled', label: 'Scheduled', sub: 'Next Thu 15 Oct, 19:00 UTC' },
        trigger,
        runtime: 'app',
        runtimeNote: 'Only while open',
        canRunNow: true,
        showReview: false,
      });
      const rows: Record<string, unknown>[] = [];
      if (withReview) {
        rows.push({
          ...baseRow(reviewId, 'Photo watch', 'watch', 'Watches the left folder'),
          profileRevision: 4,
          approvedRevision: 3,
          configRev: 2,
          status: {
            kind: 'review',
            label: 'Needs review',
            sub: 'Profile changed (rev 3 → 4) · held',
          },
          canRunNow: false,
          showReview: true,
          review: { reason: 'profileChanged', from: 3, to: 4 },
        });
      }

      const profileSummary = {
        id: profileId,
        name: 'Projects backup',
        revision: 3,
        left: 'C:\\Users\\demo\\Projects',
        right: 'E:\\Backups\\Projects',
        modeLabel: 'Update left → right',
        deleteLabel: 'Recycle Bin',
        readiness: { status: 'ready' },
        usedBy: 0,
      };
      const fail = (code: string) => Promise.reject({ code, message: code });

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
              return Promise.resolve(dir);
            case 'plugin:event|listen':
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
                  path: `${dir}\\a.txt`,
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
            case 'sync_profile_get':
              return Promise.resolve({
                id: profileId,
                name: 'Projects backup',
                revision: 4,
                policy: {},
                identities: {},
                changes: [
                  {
                    revision: 4,
                    changedMs: 1,
                    changes: [
                      { field: 'Delete policy', from: 'Recycle Bin', to: 'Permanent delete' },
                      { field: 'Direction', from: 'Update left → right', to: 'Two-way' },
                    ],
                  },
                ],
                createdMs: 1,
                updatedMs: 1,
                readiness: { status: 'ready' },
              });
            case 'automation_list':
              return Promise.resolve(rows);
            case 'automation_get':
              if (args.id !== reviewId) return fail('not_found');
              return Promise.resolve({
                automation: rows.find((r) => r.id === reviewId),
                ...policy('Photo watch', watchTrigger),
                profile: {},
                pipeline: { steps: [] },
                pendingRerun: false,
                waiting: [],
                recentRuns: [],
                events: { events: [] },
              });
            case 'automation_validate': {
              const t = new Date(Date.now() + (9 * 60 + 15) * 60000 + 20000);
              return Promise.resolve({
                errors: [],
                checks: [
                  { id: 'profile', status: 'ok', text: 'The sync profile is ready.' },
                  { id: 'runtime', status: 'ok', text: 'Runs while Ferrum is open.' },
                ],
                next: [0, 1, 2].map((i) => ({
                  atMs: t.getTime() + i * 86400000,
                  label: `Run ${i + 1}`,
                })),
              });
            }
            case 'automation_test':
              return Promise.resolve({
                testId: 'c0ffee00-1234-4abc-8def-0123456789ab',
                automationId: newId,
                policyHash: 'h',
                passed: mock.testPasses,
                steps: mock.testPasses
                  ? [
                      { id: 'profile', status: 'ok', text: 'Sync profile is valid and ready.' },
                      {
                        id: 'compare',
                        status: 'ok',
                        text: 'Comparison: 6 operations (2 update, 4 new, 0 delete).',
                      },
                    ]
                  : [
                      { id: 'profile', status: 'ok', text: 'Sync profile is valid and ready.' },
                      { id: 'source', status: 'fail', text: 'The source folder is unavailable.' },
                    ],
              });
            case 'automation_approve': {
              if (mock.approve === 'canceled') return Promise.resolve({ status: 'canceled' });
              if (mock.approve === 'stale') return Promise.resolve({ status: 'stale' });
              const draft = args.draft as { name: string; policy: unknown };
              const id = (args.id as string | null) ?? newId;
              const idx = rows.findIndex((r) => r.id === id);
              const row = {
                ...baseRow(id, draft.name, 'schedule', 'Daily at 02:00 (Asia/Jakarta)'),
              };
              if (idx >= 0) rows[idx] = row;
              else rows.push(row);
              return Promise.resolve({
                status: 'approved',
                automation: {
                  id,
                  name: draft.name,
                  kind: 'schedule',
                  profileId,
                  profileName: 'Projects backup',
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
    [seedReview, PROFILE_ID, AUTOMATION_ID, REVIEW_ID] as [boolean, string, string, string],
  );
}

async function boot(page: Page, seedReview = false) {
  injectTauriMock(page, seedReview);
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

const setMock = (page: Page, patch: Record<string, unknown>) =>
  page.evaluate((p) => Object.assign((window as unknown as { __mock: object }).__mock, p), patch);

async function openPage(page: Page) {
  await page
    .getByRole('group', { name: 'Sync & Backup' })
    .getByRole('button', { name: 'Automation' })
    .click();
  await expect(page.getByRole('dialog', { name: 'Automation' })).toBeVisible();
}

async function fillEditor(page: Page, name: string) {
  await page.getByRole('button', { name: 'New automation' }).click();
  await page.getByRole('textbox', { name: 'Name' }).fill(name);
  await page.getByRole('radio', { name: /Projects backup/ }).click();
}

test.describe('FER-526 Automation page (mock IPC)', () => {
  let page: Page;

  test.beforeEach(async ({ browser }) => {
    page = await browser.newPage();
    await page.setViewportSize({ width: 1280, height: 900 });
  });

  test.afterEach(async () => {
    await page.close();
  });

  test('new scheduled automation: editor -> review -> test -> approve sends only the draft', async () => {
    await boot(page);
    await openPage(page);
    await expect(page.getByTestId('automation-empty')).toBeVisible();

    await page.getByRole('button', { name: 'New automation' }).click();
    // Validation messages are listed and Continue is disabled until the form is valid.
    await expect(page.getByTestId('editor-errors')).toContainText('Enter a name');
    await expect(page.getByRole('button', { name: 'Continue to review' })).toBeDisabled();

    await page.getByRole('textbox', { name: 'Name' }).fill('Nightly projects backup');
    await page.getByRole('radio', { name: /Projects backup/ }).click();
    await page.getByRole('radio', { name: 'Weekly' }).click();
    await expect(page.getByTestId('editor-errors')).toContainText('Pick at least one weekday');
    await expect(page.getByRole('button', { name: 'Continue to review' })).toBeDisabled();
    await page.getByRole('button', { name: 'Monday' }).click();
    await page.getByRole('button', { name: 'Friday' }).click();

    // The Background runtime card is shown unavailable with its reason.
    const bg = page.locator('[data-runtime="background"]');
    await expect(bg).toBeDisabled();
    await expect(bg).toContainText("The background runner isn't included in this Ferrum build.");

    // The next three runs come from the backend answer, with the relative time.
    await expect(page.getByTestId('next-runs').getByRole('listitem')).toHaveCount(3);
    await expect(page.getByTestId('next-runs')).toContainText('in 9 h 15 min');

    await expect(page.getByRole('button', { name: 'Continue to review' })).toBeEnabled();
    await page.getByRole('button', { name: 'Continue to review' }).click();

    // Review: Approve is disabled until the test passed.
    const approve = page.getByRole('button', { name: 'Approve & enable' });
    await expect(approve).toBeDisabled();
    await page.getByRole('button', { name: 'Run test' }).click();
    await expect(page.getByTestId('test-passed')).toContainText(
      'Test passed — no files were changed. The comparison found 6 operations',
    );
    await expect(approve).toBeEnabled();
    await approve.click();

    // Back on the list: the new row, highlighted, plus the toast.
    await expect(page.getByRole('tab', { name: /Automations \(1\)/ })).toBeVisible();
    await expect(page.locator(`[data-automation="${AUTOMATION_ID}"]`)).toContainText(
      'Nightly projects backup',
    );
    await expect(
      page.getByText("enabled — background runs won't ask again for rev 3"),
    ).toBeVisible();

    const sent = await calls(page, 'automation_approve');
    expect(sent).toHaveLength(1);
    expect(Object.keys(sent[0]).sort()).toEqual(['draft', 'expectedConfigRev', 'id', 'testId']);
    expect(sent[0].id).toBeNull();
    expect(sent[0].testId).toBe('c0ffee00-1234-4abc-8def-0123456789ab');
    expect(JSON.stringify(sent[0])).not.toMatch(/approved|confirmed/i);
    // Approving runs nothing by itself.
    expect(await calls(page, 'automation_run_now')).toHaveLength(0);
  });

  test('a failed test shows its step and blocks approval; canceled and stale stay on review', async () => {
    await boot(page);
    await openPage(page);
    await fillEditor(page, 'Nightly');
    await page.getByRole('button', { name: 'Continue to review' }).click();

    await setMock(page, { testPasses: false });
    await page.getByRole('button', { name: 'Run test' }).click();
    await expect(page.getByTestId('test-failed')).toHaveText('The source folder is unavailable.');
    await expect(page.getByRole('button', { name: 'Approve & enable' })).toBeDisabled();

    await setMock(page, { testPasses: true, approve: 'canceled' });
    await page.getByRole('button', { name: 'Run test again' }).click();
    await page.getByRole('button', { name: 'Approve & enable' }).click();
    await expect(page.getByTestId('review-notice')).toHaveText(
      'Approval was canceled. Nothing was changed.',
    );
    await expect(page.getByTestId('automation-review')).toBeVisible();

    await setMock(page, { approve: 'stale' });
    await page.getByRole('button', { name: 'Approve & enable' }).click();
    await expect(page.getByTestId('review-notice')).toHaveText(
      'The profile or automation changed — review again',
    );
    await expect(page.getByRole('button', { name: 'Approve & enable' })).toBeDisabled();
  });

  test('re-review after a profile change lists from -> to and approves with the stored id', async () => {
    await boot(page, true);
    await openPage(page);
    await expect(page.getByTestId('attention-banner')).toContainText('Photo watch');
    await page.getByRole('button', { name: 'Review changes' }).click();

    const changes = page.getByTestId('profile-changes');
    await expect(changes).toContainText('Delete policy: Recycle Bin → Permanent delete');
    await expect(changes).toContainText('Direction: Update left → right → Two-way');
    await expect(page.getByTestId('review-reason')).toContainText(
      'Profile changed (rev 3 → 4) · held',
    );

    await page.getByRole('button', { name: 'Run test' }).click();
    await expect(page.getByTestId('test-passed')).toBeVisible();
    await page.getByRole('button', { name: 'Approve & resume' }).click();
    await expect(page.getByRole('tab', { name: /Automations/ })).toBeVisible();

    const sent = await calls(page, 'automation_approve');
    expect(sent).toHaveLength(1);
    expect(sent[0].id).toBe(REVIEW_ID);
    expect(sent[0].expectedConfigRev).toBe(2);
  });
});
