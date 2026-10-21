import { test, expect, type Page } from '@playwright/test';

/**
 * FER-577 B4 E2E, keyboard only: pane tabs (roving tablist, Ctrl+Tab, Ctrl+W) and type-to-select
 * that must not move the cursor behind an open dialog (N4).
 */

function injectTauriMock(page: Page) {
  page.addInitScript(() => {
    const home = 'C:\\Users\\demo';
    const file = (name: string, dir: string) => ({
      name,
      path: `${dir}\\${name}`,
      isDir: false,
      isSymlink: false,
      size: 10,
      modifiedMs: 1700000000000,
      hidden: false,
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
        if (cmd === 'plugin:path|resolve_directory') return Promise.resolve(home);
        if (cmd === 'plugin:event|listen') return Promise.resolve(Math.floor(Math.random() * 1e9));
        if (cmd === 'watch_start' || cmd === 'watch_stop') return Promise.resolve(1);
        if (cmd === 'list_dir') {
          const raw = String((args as { path: string }).path).replace(/\\$/, '');
          return raw === home
            ? Promise.resolve([file('notes.txt', home), file('zeta.txt', home)])
            : Promise.resolve([]);
        }
        if (cmd === 'list_drives') {
          return Promise.resolve([
            { label: 'System', path: 'C:\\', freeBytes: 1e9, totalBytes: 5e11 },
          ]);
        }
        return Promise.resolve(null);
      },
    };
  });
}

test.describe('Keyboard B4', () => {
  let page: Page;
  const left = () => page.getByRole('region', { name: 'Left pane' });
  const tabs = () => left().getByRole('tab');

  test.beforeEach(async ({ browser }) => {
    page = await browser.newPage({ viewport: { width: 1600, height: 900 } });
    injectTauriMock(page);
    await page.goto('/');
    await expect(left().locator('[data-path$="notes.txt"]')).toBeVisible();
    await left().locator('[data-path$="notes.txt"]').click();
  });
  test.afterEach(async () => {
    await page.close();
  });

  test('pane tabs: Ctrl+T, Ctrl+Tab / Ctrl+Shift+Tab cycle, arrows rove, Ctrl+W closes', async () => {
    await page.keyboard.press('Control+t');
    await page.keyboard.press('Control+t');
    await expect(tabs()).toHaveCount(3);
    await expect(tabs().nth(2)).toHaveAttribute('aria-selected', 'true');

    await page.keyboard.press('Control+Tab'); // wraps to the first tab
    await expect(tabs().nth(0)).toHaveAttribute('aria-selected', 'true');
    await page.keyboard.press('Control+Shift+Tab'); // back to the last
    await expect(tabs().nth(2)).toHaveAttribute('aria-selected', 'true');
    // The keyboard stays on the file list, not on the tab strip.
    await expect(left().locator('[data-pane-list]')).toBeFocused();

    // Roving tablist: one tab stop (the selected tab), arrows and Home/End select.
    await tabs().nth(2).focus();
    await expect(tabs().nth(2)).toHaveAttribute('tabindex', '0');
    await expect(tabs().nth(0)).toHaveAttribute('tabindex', '-1');
    await page.keyboard.press('Home');
    await expect(tabs().nth(0)).toBeFocused();
    await expect(tabs().nth(0)).toHaveAttribute('aria-selected', 'true');
    await page.keyboard.press('ArrowRight');
    await expect(tabs().nth(1)).toBeFocused();
    await expect(tabs().nth(1)).toHaveAttribute('aria-selected', 'true');
    await page.keyboard.press('End');
    await expect(tabs().nth(2)).toHaveAttribute('aria-selected', 'true');

    await left().locator('[data-pane-list]').focus();
    await page.keyboard.press('Control+w');
    await expect(tabs()).toHaveCount(2);
  });

  test('Ctrl+Tab from a focused tab button moves focus to the newly selected tab', async () => {
    await page.keyboard.press('Control+t');
    await expect(tabs()).toHaveCount(2);
    await tabs().nth(1).focus();
    await page.keyboard.press('Control+Tab');
    await expect(tabs().nth(0)).toHaveAttribute('aria-selected', 'true');
    await expect(tabs().nth(0)).toBeFocused();
  });

  test('N4: typing a letter behind an open dialog does not move the file cursor', async () => {
    const list = left().locator('[data-pane-list]');
    await list.focus();
    await expect(left().locator('.row.cursor')).toContainText('notes');
    await page.keyboard.press('F8');
    const dlg = page.getByRole('dialog');
    await expect(dlg).toBeVisible();
    await page.keyboard.press('z'); // would jump to zeta through the window-level type-to-select
    await expect(left().locator('.row.cursor')).toContainText('notes');
    await page.keyboard.press('Escape');
    await expect(dlg).toHaveCount(0);
    await expect(list).toBeFocused();
    await page.keyboard.press('z'); // and works again once the dialog is gone
    await expect(left().locator('.row.cursor')).toContainText('zeta');
  });
});
