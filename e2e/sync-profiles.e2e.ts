import { test, expect, type Page } from '@playwright/test';

/**
 * FER-521 E2E: "Sync & Backup" > Sync profiles, in browser mode with a mock IPC (same pattern as
 * sync-fast-scan.e2e.ts). The Rust commands have their own suites; here we pin the UI contract:
 * create, edit (revision shown), delete refused when an automation uses the profile, preview
 * summary, and a run that needs the overwrite confirmation. The mock is deterministic: it answers
 * synchronously from in-page state and emits run events itself, with no timers.
 */

type SeedProfile = { id: string; name: string; left: string; right: string; usedBy?: number };
type Calls = { cmd: string; args: Record<string, unknown> }[];

function injectTauriMock(page: Page, seed: SeedProfile[], theme: string) {
  page.addInitScript(
    ([seeds, themeName]: [SeedProfile[], string]) => {
      const w = window as unknown as Record<string, unknown>;
      const dir = 'C:\\Users\\demo';
      const calls: { cmd: string; args: Record<string, unknown> }[] = [];
      const runHandlers: number[] = [];
      const sp = { overwrites: 0, deletes: 1, cancelNative: false, forceStale: false };
      w.__calls = calls;
      w.__sp = sp;

      type Policy = Record<string, unknown>;
      type Rec = {
        id: string;
        name: string;
        revision: number;
        policy: Policy;
        usedBy: number;
        changes: unknown[];
      };
      const store = new Map<string, Rec>();
      let nextId = 1;
      const basePolicy = (left: string, right: string): Policy => ({
        left,
        right,
        mode: 'oneWay',
        from: 'left',
        deletePolicy: 'recycleBin',
        compare: 'sizeAndDate',
        include: [],
        exclude: [],
        guard: { enabled: true, maxCount: 50, maxPercent: 20 },
      });
      for (const s of seeds) {
        store.set(s.id, {
          id: s.id,
          name: s.name,
          revision: 1,
          policy: basePolicy(s.left, s.right),
          usedBy: s.usedBy ?? 0,
          changes: [],
        });
      }

      const modeLabel = (p: Policy) =>
        p.mode === 'twoWay'
          ? 'Two-way'
          : p.mode === 'mirror'
            ? 'Mirror left → right'
            : 'Update left → right';
      const summary = (r: Rec) => ({
        id: r.id,
        name: r.name,
        revision: r.revision,
        left: r.policy.left,
        right: r.policy.right,
        modeLabel: modeLabel(r.policy),
        deleteLabel: 'Recycle Bin',
        readiness: { status: 'ready' },
        usedBy: r.usedBy,
      });
      const dto = (r: Rec) => ({
        id: r.id,
        name: r.name,
        revision: r.revision,
        policy: r.policy,
        identities: {},
        changes: r.changes,
        createdMs: 1,
        updatedMs: 1,
        readiness: { status: 'ready' },
      });
      const fail = (code: string) => Promise.reject({ code, message: code });

      const splitPolicy = (draft: Record<string, unknown>): Policy => {
        const { name: _name, ...policy } = draft;
        void _name;
        return policy;
      };

      const emitRun = (payload: unknown) => {
        for (const h of runHandlers) {
          const cb = w[`_${h}`] as ((e: unknown) => void) | undefined;
          cb?.({ event: 'sync-profile://run', id: 1, payload });
        }
      };

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
            case 'plugin:event|listen': {
              if (args.event === 'sync-profile://run' && typeof args.handler === 'number')
                runHandlers.push(args.handler);
              return Promise.resolve(Math.floor(Math.random() * 1e9));
            }
            case 'load_prefs':
              return Promise.resolve({ theme: themeName, palette: 'violet', paneSplit: 0.5 });
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
              return Promise.resolve([...store.values()].map(summary));
            case 'sync_profile_get': {
              const r = store.get(args.id as string);
              return r ? Promise.resolve(dto(r)) : fail('not_found');
            }
            case 'sync_profile_validate': {
              const d = args.draft as Record<string, string>;
              const errors: { field: string; code: string }[] = [];
              if (!d.name.trim()) errors.push({ field: 'name', code: 'name_required' });
              if (!d.left.trim()) errors.push({ field: 'left', code: 'root_required' });
              if (!d.right.trim()) errors.push({ field: 'right', code: 'root_required' });
              else if (d.right === 'Z:\\none')
                errors.push({ field: 'right', code: 'root_missing' });
              const id = {
                kind: 'local',
                volumeSerial: 'AAAA-0001',
                fileId: '1',
                fsName: 'NTFS',
                drive: 'fixed',
              };
              return Promise.resolve({
                errors,
                identities: errors.length ? {} : { left: id, right: id },
                kinds: errors.length ? {} : { left: 'fixed', right: 'fixed' },
              });
            }
            case 'sync_profile_save': {
              const draft = args.draft as Record<string, unknown>;
              const policy = splitPolicy(draft);
              const existing = args.id ? store.get(args.id as string) : undefined;
              if (existing && (sp.forceStale || args.expectedRevision !== existing.revision))
                return Promise.resolve({ status: 'stale' });
              if (policy.deletePolicy === 'permanent' && sp.cancelNative)
                return Promise.resolve({ status: 'canceled' });
              let rec: Rec;
              if (existing) {
                const changed = JSON.stringify(existing.policy) !== JSON.stringify(policy);
                existing.name = draft.name as string;
                if (changed) {
                  existing.revision += 1;
                  existing.changes = [
                    {
                      revision: existing.revision,
                      changedMs: 1,
                      changes: [
                        {
                          field: 'Direction',
                          from: String(existing.policy.mode),
                          to: String(policy.mode),
                        },
                      ],
                    },
                    ...existing.changes,
                  ];
                }
                existing.policy = policy;
                rec = existing;
              } else {
                rec = {
                  id: `p${nextId++}`,
                  name: draft.name as string,
                  revision: 1,
                  policy,
                  usedBy: 0,
                  changes: [],
                };
                store.set(rec.id, rec);
              }
              return Promise.resolve({ status: 'saved', profile: dto(rec) });
            }
            case 'sync_profile_delete': {
              const r = store.get(args.id as string);
              if (!r) return fail('not_found');
              if (r.usedBy > 0) return fail('in_use');
              store.delete(r.id);
              return Promise.resolve({ status: 'deleted' });
            }
            case 'sync_profile_preview':
              return Promise.resolve({
                previewId: `pv-${args.id}`,
                summary: {
                  total: 5,
                  toRight: 4,
                  toLeft: 0,
                  overwrites: sp.overwrites,
                  deletes: sp.deletes,
                  conflicts: 0,
                  skipped: 0,
                  userSkipped: 0,
                },
                conflicts: 0,
                guard: { verdict: 'ok' },
              });
            case 'sync_profile_run': {
              if (args.previewId !== `pv-${args.id}`) return fail('stale');
              if (sp.overwrites > 0 && args.confirmOverwrite !== true) return fail('invalid_input');
              emitRun({
                runId: 9,
                phase: 'syncing',
                counts: {
                  total: 5,
                  done: 2,
                  failed: 0,
                  skipped: 0,
                  changed: 0,
                  held: 0,
                  notStarted: 3,
                },
              });
              emitRun({
                runId: 9,
                result: {
                  kind: 'completed',
                  counts: {
                    total: 5,
                    done: 5,
                    failed: 0,
                    skipped: 0,
                    changed: 0,
                    held: 0,
                    notStarted: 0,
                  },
                  pendingRerun: false,
                  settle: { state: 'notAttempted' },
                  problems: [],
                },
              });
              return Promise.resolve({ runId: 9 });
            }
            default:
              return Promise.reject(new Error(`Unknown command: ${cmd}`));
          }
        },
      };
    },
    [seed, theme] as [SeedProfile[], string],
  );
}

async function boot(page: Page, seed: SeedProfile[] = [], theme = 'dark') {
  injectTauriMock(page, seed, theme);
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
  page.evaluate((p) => Object.assign((window as unknown as { __sp: object }).__sp, p), patch);

async function openPage(page: Page) {
  await page
    .getByRole('group', { name: 'Sync & Backup' })
    .getByRole('button', { name: 'Sync profiles' })
    .click();
  await expect(page.getByRole('dialog', { name: 'Sync profiles' })).toBeVisible();
}

test.describe('FER-521 Sync profiles page (mock IPC)', () => {
  let page: Page;

  test.beforeEach(async ({ browser }) => {
    page = await browser.newPage();
    await page.setViewportSize({ width: 1280, height: 900 });
  });

  test.afterEach(async () => {
    await page.close();
  });

  test('the sidebar section opens the page with an empty state (SP-1.1, SP-9.1)', async () => {
    await boot(page);
    const section = page.getByRole('group', { name: 'Sync & Backup' });
    // The Automation entry is live because playwright.config.ts turns the (default-off) UI flag on.
    await expect(section.getByRole('button', { name: 'Automation' })).toBeEnabled();
    await openPage(page);
    await expect(page.getByTestId('sync-empty')).toContainText('No sync profiles yet');
    await expect(page.getByRole('button', { name: 'New sync profile' })).toBeVisible();
    await expect(page.getByRole('button', { name: 'Use current panes' })).toBeVisible();
  });

  test('create: the editor validates through the backend, then saves revision 1', async () => {
    await boot(page);
    await openPage(page);
    await page.getByRole('button', { name: 'New sync profile' }).click();
    const form = page.getByRole('form', { name: 'New sync profile' });
    await expect(form.getByRole('button', { name: 'Save' })).toBeEnabled();
    await form.getByLabel('Left folder').fill('D:\\Photos');
    await form.getByLabel('Right folder').fill('Z:\\none');
    await form.getByLabel('Profile name').fill('');
    await expect(form.getByLabel('Problems')).toContainText('Enter a name for this profile.');
    await expect(form.getByLabel('Problems')).toContainText("This folder doesn't exist.");
    await expect(form.getByRole('button', { name: 'Save' })).toBeDisabled();

    await form.getByLabel('Profile name').fill('Photos backup');
    await form.getByLabel('Right folder').fill('E:\\Backups\\Photos');
    await expect(form.getByLabel('Problems')).toHaveText('');
    await expect(form.getByText('Local disk · NTFS').first()).toBeVisible();
    await form.getByRole('button', { name: 'Save' }).click();

    const row = page.locator('li.profile', { hasText: 'Photos backup' });
    await expect(row).toContainText('Revision 1');
    await expect(row).toContainText('D:\\Photos');
    await expect(
      page.getByRole('dialog', { name: 'Sync profiles' }).getByRole('status').first(),
    ).toContainText('Saved "Photos backup" as revision 1.');
    const saves = await calls(page, 'sync_profile_save');
    expect(saves).toHaveLength(1);
    expect(JSON.stringify(saves[0])).not.toMatch(/confirm/i);
  });

  test('edit: changing the direction creates revision 2 and the editor shows it', async () => {
    await boot(page, [{ id: 'p0', name: 'Docs', left: 'D:\\Docs', right: 'E:\\Docs' }]);
    await openPage(page);
    await page.locator('li.profile').getByRole('button', { name: 'Edit' }).click();
    const form = page.getByRole('form', { name: 'Edit sync profile' });
    await expect(form.getByTestId('editor-revision')).toContainText('Revision 1');
    await form.getByRole('radio', { name: 'Two-way' }).click();
    await expect(form.getByTestId('mode-sentence')).toContainText('Conflicts are held');
    await form.getByRole('button', { name: 'Save' }).click();
    const row = page.locator('li.profile', { hasText: 'Docs' });
    await expect(row).toContainText('Revision 2');
    await row.getByRole('button', { name: 'Edit' }).click();
    await expect(page.getByTestId('editor-revision')).toContainText('Revision 2');
    await expect(page.getByTestId('editor-revision')).toContainText('Direction: oneWay → twoWay');
  });

  test('a stale save reloads the current version with the explanation (SP-6.2)', async () => {
    await boot(page, [{ id: 'p0', name: 'Docs', left: 'D:\\Docs', right: 'E:\\Docs' }]);
    await openPage(page);
    await page.locator('li.profile').getByRole('button', { name: 'Edit' }).click();
    const form = page.getByRole('form', { name: 'Edit sync profile' });
    await form.getByLabel('Profile name').fill('My edit');
    await setMock(page, { forceStale: true });
    await form.getByRole('button', { name: 'Save' }).click();
    await expect(form.getByRole('alert').first()).toContainText(
      'This profile was changed elsewhere. Review the current version.',
    );
    await expect(form.getByLabel('Profile name')).toHaveValue('Docs');
  });

  test('delete is refused when an automation uses the profile (SP-9.5)', async () => {
    await boot(page, [
      { id: 'p0', name: 'Used one', left: 'D:\\A', right: 'E:\\A', usedBy: 2 },
      { id: 'p1', name: 'Free one', left: 'D:\\B', right: 'E:\\B' },
    ]);
    await openPage(page);
    const used = page.locator('li.profile', { hasText: 'Used one' });
    await expect(used).toContainText('Used by 2 automations');
    await used.getByRole('button', { name: 'Delete' }).click();
    await used.getByRole('button', { name: 'Delete profile' }).click();
    await expect(page.getByRole('alert')).toHaveText(
      'Used by 2 automations — delete or change them first.',
    );
    await expect(used).toBeVisible();

    const free = page.locator('li.profile', { hasText: 'Free one' });
    await free.getByRole('button', { name: 'Delete' }).click();
    await free.getByRole('button', { name: 'Delete profile' }).click();
    await expect(free).toHaveCount(0);
  });

  test('preview shows the effect summary; Run now needs it first', async () => {
    await boot(page, [{ id: 'p0', name: 'Docs', left: 'D:\\Docs', right: 'E:\\Docs' }]);
    await openPage(page);
    const row = page.locator('li.profile');
    await expect(row.getByRole('button', { name: 'Run now' })).toBeDisabled();
    await row.getByRole('button', { name: 'Preview sync' }).click();
    await expect(row.getByTestId('preview-summary')).toHaveText(
      '5 changes · 4 to right · 1 deletion',
    );
    await expect(row.getByRole('button', { name: 'Run now' })).toBeEnabled();
  });

  test('run with overwrites needs the overwrite confirmation, then reports the result', async () => {
    await boot(page, [{ id: 'p0', name: 'Docs', left: 'D:\\Docs', right: 'E:\\Docs' }]);
    await setMock(page, { overwrites: 2 });
    await openPage(page);
    const row = page.locator('li.profile');
    await row.getByRole('button', { name: 'Preview sync' }).click();
    await expect(row.getByTestId('preview-summary')).toContainText('2 overwrites');

    await row.getByRole('button', { name: 'Run now' }).click();
    const confirm = row.getByRole('alertdialog', { name: 'Confirm run' });
    await expect(confirm).toContainText('2 files will be overwritten');
    expect(await calls(page, 'sync_profile_run')).toHaveLength(0);

    await confirm.getByRole('button', { name: 'Cancel' }).click();
    await expect(confirm).toHaveCount(0);
    expect(await calls(page, 'sync_profile_run')).toHaveLength(0);

    await row.getByRole('button', { name: 'Run now' }).click();
    await row.getByRole('button', { name: 'Confirm and run' }).click();
    await expect(row.getByRole('status', { name: 'Run status' })).toContainText(
      'Completed. 5 done.',
    );
    const runs = await calls(page, 'sync_profile_run');
    expect(runs).toEqual([{ id: 'p0', previewId: 'pv-p0', confirmOverwrite: true }]);
  });

  test('a run with deletions (Recycle Bin) asks first, without any overwrite flag (SP-9.4)', async () => {
    await boot(page, [{ id: 'p0', name: 'Docs', left: 'D:\\Docs', right: 'E:\\Docs' }]);
    await openPage(page);
    const row = page.locator('li.profile');
    await row.getByRole('button', { name: 'Preview sync' }).click();
    await row.getByRole('button', { name: 'Run now' }).click();
    const confirm = row.getByRole('alertdialog', { name: 'Confirm run' });
    await expect(confirm).toContainText('1 item will be deleted (Recycle Bin)');
    await expect(confirm).not.toContainText('overwritten');
    expect(await calls(page, 'sync_profile_run')).toHaveLength(0);
    await confirm.getByRole('button', { name: 'Confirm and run' }).click();
    await expect(row.getByRole('status', { name: 'Run status' })).toContainText('Completed');
    expect(await calls(page, 'sync_profile_run')).toEqual([
      { id: 'p0', previewId: 'pv-p0', confirmOverwrite: false },
    ]);
  });

  test('a run with nothing to overwrite or delete starts straight away', async () => {
    await boot(page, [{ id: 'p0', name: 'Docs', left: 'D:\\Docs', right: 'E:\\Docs' }]);
    await setMock(page, { deletes: 0 });
    await openPage(page);
    const row = page.locator('li.profile');
    await row.getByRole('button', { name: 'Preview sync' }).click();
    await row.getByRole('button', { name: 'Run now' }).click();
    await expect(row.getByRole('status', { name: 'Run status' })).toContainText('Completed');
    expect(await calls(page, 'sync_profile_run')).toEqual([
      { id: 'p0', previewId: 'pv-p0', confirmOverwrite: false },
    ]);
  });

  test('closing a dirty editor asks first (X, Cancel and Escape share one path)', async () => {
    await boot(page);
    await openPage(page);
    await page.getByRole('button', { name: 'New sync profile' }).click();
    const form = page.getByRole('form', { name: 'New sync profile' });
    await form.getByLabel('Profile name').fill('Draft');
    await page
      .getByRole('dialog', { name: 'Sync profiles' })
      .getByRole('button', { name: 'Close' })
      .click();
    const ask = page.getByRole('alertdialog', { name: 'Discard changes' });
    await expect(ask).toBeVisible();
    await ask.getByRole('button', { name: 'Keep editing' }).click();
    await expect(form.getByLabel('Profile name')).toHaveValue('Draft');
    await form.getByRole('button', { name: 'Cancel' }).click();
    await expect(ask).toBeVisible();
    await ask.getByRole('button', { name: 'Discard changes' }).click();
    await expect(page.getByRole('button', { name: 'New sync profile' })).toBeVisible();
  });

  test('a canceled native confirmation is reported and nothing is saved', async () => {
    await boot(page);
    await setMock(page, { cancelNative: true });
    await openPage(page);
    await page.getByRole('button', { name: 'New sync profile' }).click();
    const form = page.getByRole('form', { name: 'New sync profile' });
    await form.getByLabel('Profile name').fill('Wipe');
    await form.getByLabel('Left folder').fill('D:\\A');
    await form.getByLabel('Right folder').fill('E:\\B');
    await form.getByRole('radio', { name: /Delete permanently/ }).click();
    await form.getByRole('button', { name: 'Save' }).click();
    await expect(form.getByRole('status')).toContainText('Save canceled. Nothing was saved.');
    await expect(form).toBeVisible();
  });
});

const shotsDir = process.env.FER_521_SHOTS;
test.describe('FER-577 B4 Sync profiles keyboard only (mock IPC)', () => {
  let page: Page;
  test.beforeEach(async ({ browser }) => {
    page = await browser.newPage();
  });
  test.afterEach(async () => {
    await page.close();
  });

  test('create with access letters, walk the list with arrows, Enter edits, Delete asks with Cancel focused', async () => {
    await boot(page, [
      { id: 'seed-a', name: 'Docs', left: 'D:\\Docs', right: 'E:\\Docs' },
      { id: 'seed-b', name: 'Music', left: 'D:\\Music', right: 'E:\\Music' },
    ]);
    await openPage(page);
    const modal = page.getByRole('dialog', { name: 'Sync profiles' });
    await page.keyboard.press('Alt+n'); // New sync profile
    const form = page.getByRole('form', { name: 'New sync profile' });
    await expect(form).toBeVisible();
    await page.keyboard.press('Alt+n'); // Profile name
    await expect(form.getByLabel('Profile name')).toBeFocused();
    await page.keyboard.type('Keyboard made');
    await page.keyboard.press('Alt+l'); // Left folder
    await expect(form.getByLabel('Left folder')).toBeFocused();
    await page.keyboard.type('D:\\Photos');
    await page.keyboard.press('Alt+r'); // Right folder
    await expect(form.getByLabel('Right folder')).toBeFocused();
    await page.keyboard.type('E:\\Backups\\Photos');
    // Direction radios rove with the arrows (Update -> Mirror) and select.
    await form.getByRole('radio', { name: 'Update' }).focus();
    await page.keyboard.press('ArrowRight');
    await expect(form.getByRole('radio', { name: 'Mirror' })).toBeFocused();
    await expect(form.getByRole('radio', { name: 'Mirror' })).toHaveAttribute(
      'aria-checked',
      'true',
    );
    await expect(form.getByLabel('Problems')).toHaveText('');
    await page.keyboard.press('Alt+s'); // Save
    const row = page.locator('li.profile', { hasText: 'Keyboard made' });
    await expect(row).toContainText('Revision 1');

    // List rows are one tab stop each; arrows / Home / End walk them.
    const rows = page.locator('li.profile');
    await expect(rows).toHaveCount(3);
    await rows.first().focus();
    await page.keyboard.press('ArrowDown');
    await expect(rows.nth(1)).toBeFocused();
    await page.keyboard.press('End');
    await expect(rows.nth(2)).toBeFocused();
    await page.keyboard.press('Home');
    await expect(rows.first()).toBeFocused();

    // Enter edits, Esc comes back to the list.
    await page.keyboard.press('Enter');
    await expect(page.getByRole('form', { name: 'Edit sync profile' })).toBeVisible();
    await page.keyboard.press('Escape');
    await expect(page.getByRole('form', { name: 'Edit sync profile' })).toHaveCount(0);

    // Delete asks first: Cancel has the focus, Enter there deletes nothing, Esc closes it.
    await rows.first().focus();
    await page.keyboard.press('Delete');
    const confirm = modal.getByRole('alertdialog', { name: 'Confirm delete' });
    await expect(confirm).toBeVisible();
    await expect(confirm.getByRole('button', { name: 'Cancel' })).toBeFocused();
    await expect(confirm.getByRole('button', { name: 'Delete profile' })).not.toBeFocused();
    await page.keyboard.press('Enter');
    await expect(confirm).toHaveCount(0);
    await expect(rows).toHaveCount(3);
    expect(await calls(page, 'sync_profile_delete')).toHaveLength(0);
  });
});

test.describe('FER-521 Sync profiles screenshots', () => {
  test.skip(!shotsDir, 'FER_521_SHOTS not set');
  for (const theme of ['dark', 'light']) {
    test(`list and editor ${theme}`, async ({ browser }) => {
      const page = await browser.newPage({ viewport: { width: 1280, height: 900 } });
      await boot(
        page,
        [
          { id: 'p0', name: 'Photos backup', left: 'D:\\Photos', right: 'E:\\Backups\\Photos' },
          { id: 'p1', name: 'Work docs', left: 'D:\\Work', right: '\\\\nas\\work', usedBy: 1 },
        ],
        theme,
      );
      await openPage(page);
      await page
        .locator('li.profile')
        .first()
        .getByRole('button', { name: 'Preview sync' })
        .click();
      await expect(page.getByTestId('preview-summary')).toBeVisible();
      await expect(page.locator('html')).toHaveAttribute('data-theme', theme);
      await page.waitForTimeout(500); // let colour transitions finish before the shot
      await page.screenshot({ path: `${shotsDir}/sync-profiles-list-${theme}.png` });
      await page.locator('li.profile').first().getByRole('button', { name: 'Edit' }).click();
      await expect(page.getByTestId('editor-revision')).toBeVisible();
      await page.waitForTimeout(500); // let colour transitions finish before the shot
      await page.screenshot({ path: `${shotsDir}/sync-profiles-editor-${theme}.png` });
      await page.close();
    });
  }
});
