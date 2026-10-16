import { test, expect, type Page } from '@playwright/test';

/**
 * FER-481 E2E: hotlist items in the Command Palette (Ctrl+K). Same mocked hotlist as hotlist.e2e.ts.
 */

function injectTauriMock(page: Page) {
  page.addInitScript(() => {
    const w = window as unknown as {
      __calls: { cmd: string; args: unknown }[];
      __hotlist: unknown;
      __approved: boolean;
    };
    w.__calls = [];
    w.__approved = false;
    w.__hotlist = {
      version: 2,
      items: [
        {
          type: 'submenu',
          title: '&1. Drive',
          items: [
            { type: 'dir', title: '&1. Drive', path: 'C:\\Users\\demo\\Drive\\' },
            {
              type: 'submenu',
              title: '&6. personal',
              items: [
                {
                  type: 'dir',
                  title: '3. &Bob',
                  path: 'C:\\Users\\demo\\Drive\\personal\\bob\\',
                },
                { type: 'dir', title: '&Alice', path: 'C:/Users/demo/Drive/personal/alice/' },
                {
                  type: 'dir',
                  title: '4. &Charlie',
                  path: 'C:\\Users\\demo\\Drive\\personal\\charlie\\',
                },
              ],
            },
          ],
        },
        { type: 'separator' },
        { type: 'dir', title: '&Downloads', path: 'C:\\Users\\demo\\Downloads' },
        { type: 'submenu', title: 'Dummy', items: [] },
        { type: 'command', title: 'kando', command: 'C:\\tools\\forward to kando.exe', args: [] },
        { type: 'dir', title: 'Team share', path: '\\\\filesrv\\team' },
      ],
    };
    const view = () => ({
      doc: w.__hotlist,
      status: 'ok',
      readOnly: false,
      customLocation: null,
    });
    window.__TAURI_INTERNALS__ = {
      metadata: {
        currentWindow: { label: 'main' },
        currentWebview: { windowLabel: 'main', label: 'main' },
      },
      plugins: {},
      transformCallback: (cb: unknown) => {
        const id = Math.floor(Math.random() * 1e9);
        (window as unknown as Record<string, unknown>)[`_${id}`] = cb;
        return id;
      },
      invoke: (cmd, args) => {
        w.__calls.push({ cmd, args });
        if (cmd === 'plugin:path|resolve_directory') return Promise.resolve('C:\\Users\\demo');
        if (cmd === 'plugin:event|listen') return Promise.resolve(Math.floor(Math.random() * 1e9));
        if (cmd === 'watch_start' || cmd === 'watch_stop') return Promise.resolve(1);
        if (cmd === 'list_dir') return Promise.resolve([]);
        if (cmd === 'hotlist_load') return Promise.resolve(view());
        if (cmd === 'hotlist_save') {
          w.__hotlist = (args as { doc: unknown }).doc;
          return Promise.resolve(view());
        }
        if (cmd === 'hotlist_run_command') {
          // The real backend shows a native confirmation itself (FER-467 R5).
          return Promise.resolve({ launched: true });
        }
        if (cmd === 'hotlist_import_preview') {
          return Promise.resolve({
            doc: {
              version: 2,
              items: [{ type: 'dir', title: '&Imported', path: 'C:\\Users\\demo\\Imported' }],
            },
            warnings: [{ index: 4, kind: 'no_action' }],
          });
        }
        return Promise.resolve(null);
      },
    };
  });
}

const calls = (page: Page, cmd: string) =>
  page.evaluate(
    (c) =>
      (window as unknown as { __calls: { cmd: string; args: unknown }[] }).__calls.filter(
        (x) => x.cmd === c,
      ),
    cmd,
  );

test.describe('hotlist in the command palette (FER-481)', () => {
  test.beforeEach(async ({ page }) => {
    injectTauriMock(page);
    await page.goto('/');
    const pane = page.locator('section.pane').first();
    await expect(pane).toBeVisible();
    await expect(pane.getByRole('navigation', { name: 'Path' })).toContainText('demo');
    await expect.poll(async () => (await calls(page, 'hotlist_load')).length).toBeGreaterThan(0);
    await page.keyboard.press('Control+k');
    await expect(page.getByRole('dialog', { name: 'Command palette' })).toBeVisible();
  });

  test('empty query shows a Hotlist section, no empty submenu, no & marks', async ({ page }) => {
    const group = page.getByRole('group', { name: 'Hotlist' });
    await expect(group).toBeVisible();
    await expect(group.getByRole('option')).toHaveCount(7);
    await expect(group).toContainText('Drive › personal › Alice');
    await expect(group).not.toContainText('Dummy');
    await expect(group).not.toContainText('&');
    await page.screenshot({ path: 'test-results/palette-hotlist-empty.png' });
  });

  test('typing "alice" finds the breadcrumb item and Enter navigates', async ({ page }) => {
    await page.keyboard.type('alice');
    const opt = page.getByRole('option', { name: /Drive › personal › Alice/ });
    await expect(opt).toBeVisible();
    await expect(opt).toContainText('Hotlist');
    await page.screenshot({ path: 'test-results/palette-hotlist-alice.png' });
    await page.keyboard.press('Enter');
    await expect(page.getByRole('dialog', { name: 'Command palette' })).toBeHidden();
    await expect
      .poll(async () =>
        (await calls(page, 'list_dir')).map((c) => (c.args as { path: string }).path),
      )
      .toContain('C:/Users/demo/Drive/personal/alice/');
  });

  test('a command item goes to the backend by its place in the tree (FER-467)', async ({
    page,
  }) => {
    await page.keyboard.type('kando');
    await page.getByRole('option', { name: /kando/ }).first().click();
    await expect.poll(async () => (await calls(page, 'hotlist_run_command')).length).toBe(1);
    const [run] = await calls(page, 'hotlist_run_command');
    expect(run.args).toMatchObject({ position: [4], command: 'C:\\tools\\forward to kando.exe' });
    expect(JSON.stringify(run.args)).not.toMatch(/confirm/i);
  });

  test('a network folder shows the "net" tag and asks before opening (FER-467 R8)', async ({
    page,
  }) => {
    await page.keyboard.type('team share');
    const opt = page.getByRole('group', { name: 'Hotlist' }).getByRole('option').first();
    await expect(opt).toContainText('Team share');
    await expect(opt.locator('.badge.net')).toHaveText('net');
    await opt.click();
    await expect(page.getByRole('dialog', { name: 'Open network folder' })).toBeVisible();
  });

  test('typo "alcie" still finds the hotlist item (FER-485)', async ({ page }) => {
    await page.keyboard.type('alcie');
    const opt = page.getByRole('group', { name: 'Hotlist' }).getByRole('option').first();
    await expect(opt).toContainText('Drive › personal › Alice');
  });

  test('typo "hotlsit" finds the Directory Hotlist command first (FER-485)', async ({ page }) => {
    await page.keyboard.type('hotlsit');
    const first = page.getByRole('group', { name: 'Perintah' }).getByRole('option').first();
    await expect(first).toContainText('Directory Hotlist');
  });
});
