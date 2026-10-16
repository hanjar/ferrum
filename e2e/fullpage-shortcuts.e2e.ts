import { test, expect, type Page } from '@playwright/test';

/**
 * FER-561 E2E: while the full-page Activity view is open, file shortcuts (Delete / F8 / F2 / F5 / F7)
 * must not act on the hidden active pane. Closing Activity restores them. IPC is mocked.
 */

function injectTauriMock(page: Page) {
  page.addInitScript(() => {
    const home = 'C:\\Users\\demo';
    const file = (name: string) => ({
      name,
      path: `${home}\\${name}`,
      isDir: false,
      isSymlink: false,
      size: 10,
      modifiedMs: 1700000000000,
      hidden: false,
    });
    const w = window as unknown as { __calls: { cmd: string; args: unknown }[] };
    w.__calls = [];
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
        if (cmd === 'plugin:path|resolve_directory') return Promise.resolve(home);
        if (cmd === 'plugin:event|listen') return Promise.resolve(Math.floor(Math.random() * 1e9));
        if (cmd === 'watch_start' || cmd === 'watch_stop') return Promise.resolve(1);
        if (cmd === 'list_dir') return Promise.resolve([file('a.txt'), file('b.txt')]);
        if (cmd === 'activity_state') {
          return Promise.resolve({
            recordingEnabled: false,
            privacyEpoch: 1,
            retentionDays: 90,
            hiddenCount: 0,
            disclosureAcked: true,
          });
        }
        if (cmd.startsWith('activity_')) return Promise.resolve([]);
        return Promise.resolve(null);
      },
    };
  });
}

const calls = (page: Page) =>
  page.evaluate(() =>
    (window as unknown as { __calls: { cmd: string }[] }).__calls.map((c) => c.cmd),
  );

test.describe('full page blocks file shortcuts (FER-561)', () => {
  let page: Page;

  test.beforeEach(async ({ browser }) => {
    page = await browser.newPage();
    injectTauriMock(page);
    await page.goto('/');
    await page.locator('[data-path$="a.txt"]').first().click();
  });

  test.afterEach(async () => {
    await page.close();
  });

  test('file shortcuts do nothing while Activity is open and work again after closing', async () => {
    await page.keyboard.press('Control+k');
    await page.getByRole('combobox').first().fill('Activity');
    await page.keyboard.press('Enter');
    const activity = page.getByRole('dialog', { name: 'Activity' });
    await expect(activity).toBeVisible();
    // Focus a button on the page, as a keyboard user would.
    await activity.getByRole('button', { name: 'Close' }).focus();

    for (const k of ['Delete', 'F8', 'Shift+F8', 'F2', 'F7', 'F5', 'F6']) {
      await page.keyboard.press(k);
    }
    await expect(page.getByRole('dialog')).toHaveCount(1); // only Activity itself
    await expect(activity).toBeVisible();
    const seen = await calls(page);
    expect(
      seen.filter(
        (c) =>
          /^(enqueue_|delete|move_|copy_|rename|mkdir|create_dir)/.test(c) &&
          c !== 'copy_control_snapshot',
      ),
    ).toEqual([]);

    // A global shortcut still works: Ctrl+K opens the palette.
    await page.keyboard.press('Control+k');
    await expect(page.getByRole('combobox').first()).toBeVisible();
    await page.keyboard.press('Escape');

    // Escape inside the page still closes it.
    await activity.getByRole('button', { name: 'Close' }).focus();
    await page.keyboard.press('Escape');
    await expect(page.getByRole('dialog', { name: 'Activity' })).toHaveCount(0);

    // Back on the file list, F8 opens the delete dialog again.
    await page.locator('[data-path$="a.txt"]').first().click();
    await page.keyboard.press('F8');
    await expect(page.getByRole('dialog')).toContainText('Delete');
  });
});
