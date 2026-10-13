import { test, expect, type Page } from '@playwright/test';

/**
 * FER-495 E2E: the Files/Mark/Commands/View/Sort/Show menu lives in the titlebar row (no separate
 * menu row), collapses to a hamburger in a narrow window, F10 / lone Alt focus it, and the FER-489
 * Tab rule still holds. Also covers Ctrl+\ (Go to root).
 *
 * Optional: FER_SHOTS=<dir> additionally writes screenshots (1920 and 900 wide, dark and light).
 */

function injectTauriMock(page: Page) {
  page.addInitScript(() => {
    const file = (name: string, dir: string, isDir = false) => ({
      name,
      path: dir.endsWith('\\') ? `${dir}${name}` : `${dir}\\${name}`,
      isDir,
      isSymlink: false,
      size: isDir ? 0 : 10,
      modifiedMs: 1700000000000,
      hidden: false,
    });
    const home = 'C:\\Users\\demo';
    const tree: Record<string, ReturnType<typeof file>[]> = {
      'C:': [file('Users', 'C:\\', true), file('Windows', 'C:\\', true)],
      'C:\\Users': [file('demo', 'C:\\Users', true)],
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
          const p = raw === 'C:\\Projects' || raw === 'D:\\Backup' ? home : raw;
          return tree[p]
            ? Promise.resolve(tree[p])
            : Promise.reject({ code: 'notFound', message: p });
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

test.describe('Menu in the titlebar (FER-495)', () => {
  let page: Page;
  const left = () => page.getByRole('region', { name: 'Left pane' });
  const right = () => page.getByRole('region', { name: 'Right pane' });
  const trigger = (name: string) => page.locator('header.titlebar').getByRole('menuitem', { name });

  test.beforeEach(async ({ browser }) => {
    page = await browser.newPage({ viewport: { width: 1600, height: 800 } });
    injectTauriMock(page);
    await page.goto('/');
    await expect(left().locator('[data-path$="notes.txt"]')).toBeVisible();
    await expect(right().locator('[data-path$="notes.txt"]')).toBeVisible();
  });

  test.afterEach(async () => {
    await page.close();
  });

  test('the menu sits in the titlebar row and there is no separate menu row', async () => {
    const bar = await page.locator('header.titlebar').boundingBox();
    const menu = await page.locator('header.titlebar [role="menubar"]').boundingBox();
    expect(bar && menu).toBeTruthy();
    expect(menu!.y).toBeGreaterThanOrEqual(bar!.y);
    expect(menu!.y + menu!.height).toBeLessThanOrEqual(bar!.y + bar!.height + 0.5);
    await expect(page.locator('[role="menubar"]')).toHaveCount(1);
    await expect(page.locator('.shell > [role="menubar"]')).toHaveCount(0);
    expect(bar!.height).toBeLessThanOrEqual(41);
    // Order: sidebar toggle, menu, search box.
    const toggle = await page.getByRole('button', { name: 'Sidebar' }).boundingBox();
    const search = await page.getByTitle(/Search files or run a command/).boundingBox();
    expect(toggle!.x + toggle!.width).toBeLessThanOrEqual(menu!.x + 0.5);
    expect(menu!.x + menu!.width).toBeLessThanOrEqual(search!.x);
    for (const name of ['Files', 'Mark', 'Commands', 'View', 'Sort', 'Show']) {
      await expect(trigger(name)).toBeVisible();
    }
  });

  test('menu buttons are not drag regions but the empty bar is', async () => {
    const info = await page.evaluate(() => {
      const t = document.querySelector<HTMLElement>('[data-menu-trigger="0"]')!;
      return {
        bar: document.querySelector('header.titlebar')!.hasAttribute('data-tauri-drag-region'),
        trigger: t.hasAttribute('data-tauri-drag-region'),
        menubar: t.closest('[role="menubar"]')!.hasAttribute('data-tauri-drag-region'),
      };
    });
    expect(info).toEqual({ bar: true, trigger: false, menubar: true });
  });

  test('clicking Files opens its dropdown and an item runs', async () => {
    await trigger('Files').click();
    const menu = page.getByRole('menu', { name: 'Files' });
    await expect(menu).toBeVisible();
    await expect(menu.getByRole('menuitem', { name: /^Open/ })).toBeVisible();
    // Dropdown paints over the file panes rather than being clipped by the titlebar.
    const top = await menu.evaluate((el) => {
      const r = el.getBoundingClientRect();
      const hit = document.elementFromPoint(r.x + 20, r.y + 20);
      return el.contains(hit);
    });
    expect(top).toBe(true);
    await page.keyboard.press('Escape');
    await expect(menu).toHaveCount(0);
    await expect(trigger('Files')).toBeFocused();
  });

  test('a narrow window collapses the menu into a hamburger with the same menu', async () => {
    await page.setViewportSize({ width: 900, height: 700 });
    const burger = page.locator('[data-menu-burger]');
    await expect(burger).toBeVisible();
    await expect(page.locator('[data-menu-trigger="0"]')).toHaveCount(0);
    await burger.click();
    await expect(trigger('Files')).toBeVisible();
    await trigger('Files').click();
    const menu = page.getByRole('menu', { name: 'Files' });
    await expect(menu).toBeVisible();
    await expect(menu.getByRole('menuitem', { name: /^Open/ })).toBeVisible();
    // The search box keeps a usable width.
    const search = await page.getByTitle(/Search files or run a command/).boundingBox();
    expect(search!.width).toBeGreaterThanOrEqual(120);
    // Growing the window brings the full menu back.
    await page.keyboard.press('Escape');
    await page.keyboard.press('Escape');
    await page.setViewportSize({ width: 1600, height: 700 });
    await expect(burger).toHaveCount(0);
    await expect(trigger('Sort')).toBeVisible();
  });

  test('F10 focuses the menu, arrows move and Escape returns to the list', async () => {
    await left().locator('[data-path$="notes.txt"]').click();
    await page.keyboard.press('F10');
    await expect(trigger('Files')).toBeFocused();
    await page.keyboard.press('ArrowRight');
    await expect(trigger('Mark')).toBeFocused();
    await page.keyboard.press('ArrowDown');
    await expect(page.getByRole('menu', { name: 'Mark' })).toBeVisible();
    await page.keyboard.press('Escape');
    await expect(trigger('Mark')).toBeFocused();
    await page.keyboard.press('Escape');
    expect(
      await left()
        .locator('[data-pane-list]')
        .evaluate((el) => el.contains(document.activeElement)),
    ).toBe(true);
  });

  test('F10 in the narrow window focuses the hamburger', async () => {
    await page.setViewportSize({ width: 900, height: 700 });
    await expect(page.locator('[data-menu-burger]')).toBeVisible();
    await page.keyboard.press('F10');
    await expect(page.locator('[data-menu-burger]')).toBeFocused();
    await page.keyboard.press('ArrowDown');
    await expect(trigger('Files')).toBeFocused();
    await page.keyboard.press('ArrowRight');
    await expect(page.getByRole('menu', { name: 'Files' })).toBeVisible();
  });

  test('Alt pressed alone focuses the menu, Alt+D still opens the address bar', async () => {
    await left().locator('[data-path$="notes.txt"]').click();
    await page.keyboard.down('Alt');
    await page.keyboard.up('Alt');
    await expect(trigger('Files')).toBeFocused();
    await page.keyboard.press('Escape');
    await left().locator('[data-path$="notes.txt"]').click();
    await page.keyboard.press('Alt+d');
    await expect(left().getByRole('combobox', { name: 'Address' })).toBeFocused();
    await expect(trigger('Files')).not.toBeFocused();
  });

  test('Tab with the menu closed still only switches pane', async () => {
    await left().locator('[data-path$="notes.txt"]').click();
    await page.keyboard.press('Tab');
    await expect(right()).toHaveClass(/active/);
    await page.keyboard.press('F10');
    await expect(trigger('Files')).toBeFocused();
    await page.keyboard.press('Tab');
    await expect(
      page.locator('header.titlebar [role="menubar"] [role="menuitem"]:focus'),
    ).toHaveCount(0);
    expect(await page.evaluate(() => !!document.activeElement?.closest('[data-pane-list]'))).toBe(
      true,
    );
  });

  test('Tab while a menu is open closes it', async () => {
    await trigger('Files').click();
    await expect(page.getByRole('menu', { name: 'Files' })).toBeVisible();
    await page.keyboard.press('Tab');
    await expect(page.getByRole('menu', { name: 'Files' })).toHaveCount(0);
  });

  test('Ctrl+\\ goes from a nested folder to the drive root', async () => {
    await left().locator('[data-path$="invoice"]').dblclick();
    await expect(left().locator('[data-path$="inv1.txt"]')).toBeVisible();
    await page.keyboard.press('Control+\\');
    await expect(left().locator('[data-path$="Users"]')).toBeVisible();
    await expect(left().locator('[data-path$="inv1.txt"]')).toHaveCount(0);
    // The cursor lands on the first row.
    await expect(left().locator('[data-path$="Users"]')).toHaveClass(/cursor/);
  });

  test('Ctrl+\\ is inactive while typing in the address bar', async () => {
    await left().locator('[data-path$="notes.txt"]').click();
    await page.keyboard.press('Alt+d');
    const input = left().getByRole('combobox', { name: 'Address' });
    await expect(input).toBeFocused();
    await page.keyboard.press('Control+\\');
    await expect(input).toBeFocused();
    await expect(left().locator('[data-path$="Users"]')).toHaveCount(0);
  });

  test('Go to root is in the Commands menu and the palette', async () => {
    await trigger('Commands').click();
    await expect(
      page.getByRole('menu', { name: 'Commands' }).getByRole('menuitem', { name: /Go to root/ }),
    ).toBeVisible();
  });
});

const shotsDir = process.env.FER_SHOTS;
test.describe('Menu in the titlebar screenshots', () => {
  test.skip(!shotsDir, 'FER_SHOTS not set');
  for (const theme of ['dark', 'light']) {
    for (const width of [1920, 900]) {
      test(`${theme} ${width}`, async ({ browser }) => {
        const page = await browser.newPage({ viewport: { width, height: 900 } });
        injectTauriMock(page);
        await page.goto(`/?theme=${theme}`);
        await expect(
          page.getByRole('region', { name: 'Left pane' }).locator('[data-path$="notes.txt"]'),
        ).toBeVisible();
        const tag = process.env.FER_SHOTS_TAG ?? 'after';
        await page.screenshot({ path: `${shotsDir}/${tag}-${theme}-${width}.png` });
        await page.close();
      });
    }
  }
});
