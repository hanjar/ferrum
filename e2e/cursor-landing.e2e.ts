import { test, expect, type Locator, type Page } from '@playwright/test';

/**
 * FER-489 E2E: after navigation the cursor lands like Total Commander (first row, folder just
 * left, remembered position on back, kept on refresh) and keyboard focus stays in the list.
 */

const BS = String.fromCharCode(92, 92);
const ADDR = ['C:', 'Users', 'demo', 'invoice'].join(String.fromCharCode(92));

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
      [`${home}\\invoice`]: [
        file('sub', `${home}\\invoice`, true),
        file('inv1.txt', `${home}\\invoice`),
        file('inv2.txt', `${home}\\invoice`),
      ],
      [`${home}\\invoice\\sub`]: [file('deep.txt', `${home}\\invoice\\sub`)],
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
        if (cmd === 'path_suggest') {
          return Promise.resolve({ items: [], truncated: false, timedOut: false });
        }
        if (cmd === 'path_resolve') {
          const { input } = args as { input: string };
          return Promise.resolve({ path: input, kind: 'dir' });
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

test.describe('cursor landing (FER-489)', () => {
  let page: Page;
  const left = () => page.getByRole('region', { name: 'Left pane' });
  const right = () => page.getByRole('region', { name: 'Right pane' });
  const row = (pane: () => Locator, name: string) => pane().locator(`[data-path$="${BS}${name}"]`);
  const focusInside = (pane: () => Locator) =>
    pane()
      .locator('[data-pane-list]')
      .evaluate((el) => el.contains(document.activeElement));

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

  test('Enter on a folder puts the cursor on the first row; arrows and Enter work without a click', async () => {
    await row(left, 'invoice').click();
    await page.keyboard.press('Enter');
    await expect(row(left, 'sub')).toHaveClass(/cursor/);
    expect(await focusInside(left)).toBe(true);
    await page.keyboard.press('ArrowDown');
    await expect(row(left, 'inv1.txt')).toHaveClass(/cursor/);
    await page.keyboard.press('ArrowUp');
    await page.keyboard.press('Enter');
    await expect(row(left, 'deep.txt')).toBeVisible();
    await expect(row(left, 'deep.txt')).toHaveClass(/cursor/);
  });

  test('double-click into a folder keeps keyboard focus in the list', async () => {
    await row(left, 'invoice').dblclick();
    await expect(row(left, 'sub')).toHaveClass(/cursor/);
    expect(await focusInside(left)).toBe(true);
  });

  test('Backspace puts the cursor on the folder just left', async () => {
    await row(left, 'invoice').click();
    await page.keyboard.press('Enter');
    await row(left, 'sub').click();
    await page.keyboard.press('Enter');
    await expect(row(left, 'deep.txt')).toBeVisible();
    await page.keyboard.press('Backspace');
    await expect(row(left, 'sub')).toHaveClass(/cursor/);
    await page.keyboard.press('Backspace');
    await expect(row(left, 'invoice')).toHaveClass(/cursor/);
    expect(await focusInside(left)).toBe(true);
  });

  test('Back restores the remembered cursor, Forward too', async () => {
    await row(left, 'notes.txt').click();
    await page.keyboard.press('Alt+d');
    const input = left().getByRole('combobox', { name: 'Address' });
    await input.fill(ADDR);
    await input.press('Enter');
    // Address-bar navigation hands focus to the list.
    await expect(row(left, 'sub')).toHaveClass(/cursor/);
    expect(await focusInside(left)).toBe(true);
    await page.keyboard.press('ArrowDown');
    await page.keyboard.press('Alt+ArrowLeft');
    await expect(row(left, 'notes.txt')).toHaveClass(/cursor/);
    await left().getByTitle('Forward').click();
    await expect(row(left, 'inv1.txt')).toHaveClass(/cursor/);
  });

  test('Refresh keeps the cursor', async () => {
    await row(left, 'zeta.txt').click();
    await page.keyboard.press('Control+r');
    await expect(row(left, 'zeta.txt')).toHaveClass(/cursor/);
    await page.keyboard.press('ArrowUp');
    await expect(row(left, 'pack.zip')).toHaveClass(/cursor/);
  });

  test('Ctrl+Right sets the other pane cursor without stealing focus', async () => {
    await row(left, 'invoice').click();
    await page.keyboard.press('Control+ArrowRight');
    await expect(row(right, 'sub')).toHaveClass(/cursor/);
    await expect(left()).toHaveClass(/active/);
    expect(await focusInside(left)).toBe(true);
    expect(await focusInside(right)).toBe(false);
  });

  test('grid view lands on the first cell and keeps working', async () => {
    await page.keyboard.press('Control+Shift+F2');
    await row(left, 'invoice').click();
    await page.keyboard.press('Enter');
    await expect(row(left, 'sub')).toHaveClass(/cursor/);
    await page.keyboard.press('Backspace');
    await expect(row(left, 'invoice')).toHaveClass(/cursor/);
  });
});
