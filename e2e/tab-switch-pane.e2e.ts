import { test, expect, type Page } from '@playwright/test';

/**
 * FER-489 E2E: Tab / Shift+Tab inside a file list moves focus to the OTHER pane's list (Total
 * Commander style) and makes that pane active. Elsewhere (address bar, dialogs) Tab is untouched.
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

test.describe('Tab switches pane (FER-489)', () => {
  let page: Page;
  const left = () => page.getByRole('region', { name: 'Left pane' });
  const right = () => page.getByRole('region', { name: 'Right pane' });
  const listOf = (pane: () => ReturnType<typeof left>) => pane().locator('[data-pane-list]');
  const focusInside = (pane: () => ReturnType<typeof left>) =>
    pane()
      .locator('[data-pane-list]')
      .evaluate((el) => el.contains(document.activeElement));
  const activeIs = async (name: 'Left' | 'Right') => {
    const pane = name === 'Left' ? left : right;
    await expect(pane()).toHaveClass(/active/);
  };

  test.beforeEach(async ({ browser }) => {
    page = await browser.newPage();
    injectTauriMock(page);
    await page.goto('/');
    await expect(left().locator('[data-path$="notes.txt"]')).toBeVisible();
    await expect(right().locator('[data-path$="notes.txt"]')).toBeVisible();
  });

  test.afterEach(async () => {
    await page.close();
  });

  test('Tab moves focus left -> right list and back', async () => {
    await left().locator('[data-path$="notes.txt"]').click();
    await page.keyboard.press('Tab');
    await activeIs('Right');
    expect(await focusInside(right)).toBe(true);
    expect(await focusInside(left)).toBe(false);
    await page.keyboard.press('Tab');
    await activeIs('Left');
    expect(await focusInside(left)).toBe(true);
  });

  test('Shift+Tab also toggles between panes', async () => {
    await left().locator('[data-path$="notes.txt"]').click();
    await page.keyboard.press('Shift+Tab');
    await activeIs('Right');
    expect(await focusInside(right)).toBe(true);
    await page.keyboard.press('Shift+Tab');
    await activeIs('Left');
    expect(await focusInside(left)).toBe(true);
  });

  test('keyboard works right away in the new pane (arrow moves its cursor)', async () => {
    await left().locator('[data-path$="notes.txt"]').click();
    await page.keyboard.press('Tab');
    await page.keyboard.press('ArrowDown');
    await expect(right().locator('[data-path$="notes.txt"]')).toHaveClass(/cursor|selected/);
  });

  test('Tab in the filter and in an edited address bar returns to the same list', async () => {
    await left().locator('[data-path$="notes.txt"]').click();
    await page.keyboard.press('Alt+d');
    const input = left().getByRole('combobox', { name: 'Address' });
    await expect(input).toBeFocused();
    await page.keyboard.press('Tab');
    await activeIs('Left');
    expect(await focusInside(left)).toBe(true);
    expect(await focusInside(right)).toBe(false);
    await right().getByRole('button', { name: 'Filter' }).click();
    await expect(right().getByRole('textbox', { name: 'Filter' })).toBeFocused();
    await page.keyboard.press('Tab');
    await activeIs('Right');
    expect(await focusInside(right)).toBe(true);
  });

  test('focus on the tab strip: Tab enters the other pane list', async () => {
    await left().locator('[data-path$="notes.txt"]').click();
    await left().getByRole('tab').first().focus();
    await page.keyboard.press('Tab');
    await activeIs('Right');
    expect(await focusInside(right)).toBe(true);
  });

  test('repeated Tab never lands on a button or tab and the list always has a cursor row', async () => {
    await page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur());
    for (let i = 0; i < 6; i++) {
      await page.keyboard.press(i % 2 ? 'Shift+Tab' : 'Tab');
      const info = await page.evaluate(() => {
        const el = document.activeElement as HTMLElement | null;
        return {
          inList: !!el?.closest('[data-pane-list]'),
          tag: el?.tagName,
          cursors: el?.closest('section.pane')?.querySelectorAll('.row.cursor').length ?? 0,
        };
      });
      expect(info.inList).toBe(true);
      expect(info.cursors).toBe(1);
    }
  });

  test('Tab in the view dropdown closes it and returns to the same list', async () => {
    await left().locator('[data-path$="notes.txt"]').click();
    await left()
      .getByRole('button', { name: /^View:/ })
      .click();
    await expect(left().getByRole('menu')).toBeVisible();
    await page.keyboard.press('Tab');
    await expect(left().getByRole('menu')).toHaveCount(0);
    await activeIs('Left');
    expect(await focusInside(left)).toBe(true);
  });

  test('Tab in Settings stays inside Settings', async () => {
    await left().locator('[data-path$="notes.txt"]').click();
    await page.keyboard.press('Control+,');
    const dialog = page.getByRole('dialog');
    await expect(dialog).toBeVisible();
    await page.keyboard.press('Tab');
    await activeIs('Left');
    expect(await focusInside(right)).toBe(false);
    expect(await dialog.evaluate((el) => el.contains(document.activeElement))).toBe(true);
  });

  test('Tab inside a dialog is not hijacked', async () => {
    await left().locator('[data-path$="notes.txt"]').click();
    await page.keyboard.press('F7');
    const dialog = page.getByRole('dialog');
    await expect(dialog).toBeVisible();
    await page.keyboard.press('Tab');
    await activeIs('Left');
    expect(await focusInside(right)).toBe(false);
    await expect(dialog).toBeVisible();
  });

  test('grid view panes switch too', async () => {
    await page.keyboard.press('Control+Shift+F2'); // thumbnail grid on the active (left) pane
    await left().locator('[data-path$="notes.txt"]').click();
    await page.keyboard.press('Tab');
    await activeIs('Right');
    expect(await focusInside(right)).toBe(true);
    await page.keyboard.press('Control+Shift+F2'); // right pane in grid as well
    await right().locator('[data-path$="notes.txt"]').click();
    await page.keyboard.press('Tab');
    await activeIs('Left');
    expect(await focusInside(left)).toBe(true);
    await expect(listOf(left)).toBeVisible();
  });
});
