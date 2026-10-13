import { test, expect, type Locator, type Page } from '@playwright/test';

/**
 * FER-482 E2E: Total Commander pane shortcuts. Ctrl+Right / Ctrl+Left open the
 * item under the cursor (folder or archive) in the OTHER pane, falling back to
 * the active pane's current folder; Ctrl+U swaps both panes completely. Tauri IPC
 * is mocked with a tiny in-memory filesystem (same pattern as the other e2e files).
 */

function injectTauriMock(page: Page) {
  page.addInitScript(() => {
    const file = (name: string, dir: string, isDir = false) => ({
      name,
      path: `${dir}\\${name}`,
      isDir,
      isSymlink: false,
      size: isDir ? 0 : 10,
      modifiedMs: 1700000000000,
      hidden: false,
    });
    const home = 'C:\\Users\\demo';
    const tree: Record<string, ReturnType<typeof file>[]> = {
      [home]: [
        file('invoice', home, true),
        file('notes.txt', home),
        file('pack.zip', home),
        file('zeta.txt', home),
      ],
      [`${home}\\invoice`]: [file('inv1.txt', `${home}\\invoice`)],
    };
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
        if (cmd === 'plugin:path|resolve_directory') return Promise.resolve(home);
        if (cmd === 'plugin:event|listen') return Promise.resolve(Math.floor(Math.random() * 1e9));
        if (cmd === 'watch_start' || cmd === 'watch_stop') return Promise.resolve(1);
        if (cmd === 'list_dir') {
          const raw = String((args as { path: string }).path).replace(/\\$/, '');
          // New tabs open at the mock home folders; serve the demo listing there too.
          const p = raw === 'C:\\Projects' || raw === 'D:\\Backup' ? home : raw;
          return tree[p]
            ? Promise.resolve(tree[p])
            : Promise.reject({ code: 'notFound', message: p });
        }
        if (cmd === 'archive_list') {
          const loc = (args as { location: unknown }).location;
          return Promise.resolve({
            location: loc,
            requestToken: 1,
            entries: [
              {
                memberPath: 'inner.txt',
                name: 'inner.txt',
                isDir: false,
                size: 5,
                modifiedMs: 1700000000000,
              },
            ],
          });
        }
        if (cmd === 'list_drives') {
          return Promise.resolve([
            { label: 'System', path: 'C:\\', freeBytes: 1e9, totalBytes: 5e11 },
          ]);
        }
        return Promise.reject(new Error(`Unknown command: ${cmd}`));
      },
    };
  });
}

test.describe('TC pane shortcuts (FER-482)', () => {
  let page: Page;
  const left = () => page.getByRole('region', { name: 'Left pane' });
  const right = () => page.getByRole('region', { name: 'Right pane' });
  const row = (pane: () => Locator, name: string) => pane().locator(`[data-path$="\\\\${name}"]`);
  const crumbs = (pane: () => Locator) => pane().getByRole('navigation', { name: 'Path' });

  test.beforeEach(async ({ browser }) => {
    page = await browser.newPage();
    injectTauriMock(page);
    await page.goto('/');
    await expect(row(left, 'invoice')).toBeVisible();
    await expect(row(right, 'invoice')).toBeVisible();
  });

  test.afterEach(async () => {
    await page.close();
  });

  test('Ctrl+Right on a folder opens it in the right pane, focus stays left', async () => {
    await row(left, 'invoice').click();
    await page.keyboard.press('Control+ArrowRight');
    await expect(crumbs(right)).toContainText('invoice');
    await expect(right().locator('[data-path$="inv1.txt"]')).toBeVisible();
    await expect(crumbs(left)).not.toContainText('invoice');
    // Focus stays in the left pane: Ctrl+T adds a tab there, not on the right.
    await page.keyboard.press('Control+t');
    await expect(left().getByRole('tab')).toHaveCount(2);
    await expect(right().getByRole('tab')).toHaveCount(1);
  });

  test('Ctrl+Right on a zip enters the archive in the right pane', async () => {
    await row(left, 'pack.zip').click();
    await page.keyboard.press('Control+ArrowRight');
    await expect(right().locator('[data-path$="inner.txt"]')).toBeVisible();
    await expect(crumbs(left)).not.toContainText('pack.zip');
  });

  test('Ctrl+Right on a plain file opens the current folder in the other pane', async () => {
    // Move the right pane away first, then prove it comes back to the left folder.
    await row(right, 'invoice').dblclick();
    await expect(crumbs(right)).toContainText('invoice');
    await row(left, 'notes.txt').click();
    await page.keyboard.press('Control+ArrowRight');
    await expect(row(right, 'notes.txt')).toBeVisible();
    await expect(crumbs(right)).not.toContainText('invoice');
  });

  test('Ctrl+Left from the right pane opens the cursor item in the left pane', async () => {
    await row(right, 'invoice').click();
    await page.keyboard.press('Control+ArrowLeft');
    await expect(crumbs(left)).toContainText('invoice');
    await expect(left().locator('[data-path$="inv1.txt"]')).toBeVisible();
    await expect(crumbs(right)).not.toContainText('invoice');
  });

  test('Ctrl+Left in the left pane opens the current folder in the right pane', async () => {
    await row(right, 'invoice').dblclick();
    await expect(crumbs(right)).toContainText('invoice');
    await row(left, 'invoice').click();
    await page.keyboard.press('Control+ArrowLeft');
    await expect(row(right, 'notes.txt')).toBeVisible();
  });

  test('Ctrl+U swaps both panes with their tabs and keeps focus on the same side', async () => {
    await row(left, 'zeta.txt').click();
    await page.keyboard.press('Control+t');
    await expect(left().getByRole('tab')).toHaveCount(2);
    await row(left, 'invoice').dblclick();
    await expect(crumbs(left)).toContainText('invoice');

    await page.keyboard.press('Control+u');
    await expect(right().getByRole('tab')).toHaveCount(2);
    await expect(left().getByRole('tab')).toHaveCount(1);
    await expect(crumbs(right)).toContainText('invoice');
    await expect(crumbs(left)).not.toContainText('invoice');

    // Focus still on the left side: a new tab lands there.
    await page.keyboard.press('Control+t');
    await expect(left().getByRole('tab')).toHaveCount(2);
    await expect(right().getByRole('tab')).toHaveCount(2);
  });

  test('shortcuts are inert while typing in the address bar', async () => {
    await row(left, 'invoice').click();
    await left().getByLabel('Edit path').click();
    const input = left().getByRole('combobox', { name: 'Address' });
    await expect(input).toBeFocused();
    await input.press('Control+ArrowRight');
    await input.press('Control+u');
    await expect(crumbs(right)).not.toContainText('invoice');
    await expect(right().getByRole('tab')).toHaveCount(1);
    await expect(input).toBeVisible();
  });
});
