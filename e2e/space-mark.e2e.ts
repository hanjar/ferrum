import { test, expect, type Locator, type Page } from '@playwright/test';

/**
 * FER-492 E2E: Space / Insert toggle the mark of the cursor row (and step down) instead of the
 * focusable list scrolling like PageDown; PageUp/PageDown/End move the cursor; text inputs keep
 * typing spaces. The old suites never pressed Space/PageDown and used 4-row folders that cannot
 * scroll, so a native scroll would not have been visible to them.
 */

const BS = String.fromCharCode(92, 92);

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
        ...Array.from({ length: 60 }, (_, i) =>
          file(`file${String(i).padStart(2, '0')}.txt`, home),
        ),
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
        if (cmd === 'plan_copy') {
          const w = window as unknown as { __plans?: unknown[] };
          (w.__plans ??= []).push((args as { sources: string[] }).sources);
          return Promise.resolve({ totalFiles: 1, totalBytes: 1, conflicts: [] });
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

test.describe('Space marks, does not scroll (FER-492)', () => {
  let page: Page;
  const left = () => page.getByRole('region', { name: 'Left pane' });
  const right = () => page.getByRole('region', { name: 'Right pane' });
  const row = (pane: () => Locator, name: string) => pane().locator(`[data-path$="${BS}${name}"]`);
  const scrollTop = () =>
    left()
      .locator('[data-pane-list]')
      .evaluate((el) => el.scrollTop);

  test.beforeEach(async ({ browser }) => {
    page = await browser.newPage();
    await page.setViewportSize({ width: 1280, height: 600 });
    injectTauriMock(page);
    await page.goto('/');
    await expect(row(left, 'invoice')).toBeVisible();
    await expect(row(right, 'invoice')).toBeVisible();
  });

  test.afterEach(async () => {
    await page.close();
  });

  test('Space toggles the mark, steps down and never scrolls the list', async () => {
    await row(left, 'invoice').click();
    await page.keyboard.press('ArrowDown'); // file00
    const before = await scrollTop();
    await page.keyboard.press('Space');
    await expect(row(left, 'file00.txt')).toHaveClass(/selected/);
    await expect(row(left, 'file01.txt')).toHaveClass(/cursor/);
    await page.keyboard.press('Control+ArrowUp'); // cursor back without touching the marks
    await page.keyboard.press('Space'); // un-mark
    await expect(row(left, 'file00.txt')).not.toHaveClass(/selected/);
    expect(await scrollTop()).toBe(before);
  });

  test('Insert toggles like Space; repeated Space marks several rows', async () => {
    await row(left, 'file03.txt').click();
    await page.keyboard.press('Insert');
    await page.keyboard.press('Space');
    await expect(row(left, 'file03.txt')).toHaveClass(/selected/);
    await expect(row(left, 'file04.txt')).toHaveClass(/selected/);
    await expect(row(left, 'file05.txt')).toHaveClass(/cursor/);
  });

  test('Space works in the grid view', async () => {
    await page.keyboard.press('Control+Shift+F2');
    await row(left, 'file02.txt').click();
    const before = await scrollTop();
    await page.keyboard.press('Space');
    await expect(row(left, 'file02.txt')).toHaveClass(/selected/);
    await expect(row(left, 'file03.txt')).toHaveClass(/cursor/);
    expect(await scrollTop()).toBe(before);
  });

  test('Space after Tab moves to the other pane', async () => {
    await row(left, 'file01.txt').click();
    await page.keyboard.press('Tab');
    await expect(right()).toHaveClass(/active/);
    await page.keyboard.press('Space');
    // Cursor landed on the first row of the right pane (invoice) and it is now marked.
    await expect(row(right, 'invoice')).toHaveClass(/selected/);
  });

  test('Space right after entering a folder marks the auto-placed cursor', async () => {
    await row(left, 'invoice').click();
    await page.keyboard.press('Enter');
    await expect(row(left, 'sub')).toHaveClass(/cursor/);
    await page.keyboard.press('Space');
    await expect(row(left, 'sub')).toHaveClass(/selected/);
    await expect(row(left, 'inv1.txt')).toHaveClass(/cursor/);
  });

  test('PageDown / PageUp / End / Home move the cursor and the list follows', async () => {
    await row(left, 'invoice').click();
    await page.keyboard.press('PageDown');
    const first = left().locator('.cursor');
    await expect(first).not.toHaveAttribute('data-path', new RegExp(BS + 'invoice$'));
    await expect(first).toBeVisible();
    expect(await scrollTop()).toBeGreaterThan(0);
    await page.keyboard.press('End');
    await expect(row(left, 'zeta.txt')).toHaveClass(/cursor/);
    await expect(row(left, 'zeta.txt')).toBeVisible();
    await page.keyboard.press('PageUp');
    await expect(row(left, 'zeta.txt')).not.toHaveClass(/cursor/);
    await page.keyboard.press('Home');
    await expect(row(left, 'invoice')).toHaveClass(/cursor/);
    await expect.poll(scrollTop).toBe(0);
  });

  test('Space types a space in the filter and address inputs', async () => {
    await left().getByRole('button', { name: 'Filter' }).click();
    const filter = left().getByRole('textbox', { name: 'Filter' });
    await filter.fill('a');
    await page.keyboard.press('Space');
    await page.keyboard.type('b');
    await expect(filter).toHaveValue('a b');
    await expect(left().locator('.selected')).toHaveCount(0);

    await page.keyboard.press('Escape');
    await row(left, 'file01.txt').click();
    await page.keyboard.press('Alt+d');
    const addr = left().getByRole('combobox', { name: 'Address' });
    await addr.fill('C:' + String.fromCharCode(92) + 'x');
    await page.keyboard.press('Space');
    await page.keyboard.type('y');
    await expect(addr).toHaveValue(/x y$/);
  });

  const plans = () =>
    page.evaluate(() => (window as unknown as { __plans?: string[][] }).__plans ?? []);
  const names = (paths: string[]) => paths.map((p) => p.split(String.fromCharCode(92)).pop());

  test('Space, Down, Down, Space gives two marks and arrows keep them', async () => {
    await row(left, 'file00.txt').click();
    await page.keyboard.press('Space');
    await page.keyboard.press('ArrowDown');
    await page.keyboard.press('ArrowDown');
    await page.keyboard.press('Space');
    await expect(left().locator('.row.selected')).toHaveCount(2);
    await expect(row(left, 'file00.txt')).toHaveClass(/selected/);
    await expect(row(left, 'file03.txt')).toHaveClass(/selected/);
    await page.keyboard.press('ArrowUp');
    await page.keyboard.press('ArrowUp');
    await page.keyboard.press('End');
    await page.keyboard.press('Home');
    await expect(left().locator('.row.selected')).toHaveCount(2);
    await expect(left().locator('.status')).toContainText('2 of');
  });

  test('F5 copies the marks; without marks it copies the cursor item', async () => {
    await row(left, 'file00.txt').click();
    await page.keyboard.press('Space');
    await page.keyboard.press('ArrowDown');
    await page.keyboard.press('Space');
    await page.keyboard.press('ArrowDown');
    await page.keyboard.press('F5');
    await expect.poll(async () => (await plans()).length).toBe(1);
    expect(names((await plans())[0])).toEqual(['file00.txt', 'file02.txt']);
    await page.keyboard.press('Escape');
    await expect(page.getByRole('dialog')).toHaveCount(0);

    await row(left, 'file07.txt').click(); // plain click clears the marks
    await expect(left().locator('.row.selected')).toHaveCount(0);
    await page.keyboard.press('F5');
    await expect.poll(async () => (await plans()).length).toBe(2);
    expect(names((await plans())[1])).toEqual(['file07.txt']);
  });

  test('F8 deletes the marks, not the cursor row', async () => {
    await row(left, 'file00.txt').click();
    await page.keyboard.press('Space');
    await page.keyboard.press('ArrowDown');
    await page.keyboard.press('Space');
    await page.keyboard.press('F8');
    const dlg = page.getByRole('dialog');
    await expect(dlg).toBeVisible();
    await expect(dlg).toContainText(/2 (items|files|selected)|file00\.txt/);
    await expect(dlg).not.toContainText('file01.txt');
  });

  test('plain click clears marks, Ctrl+click adds, Shift+Down marks a range', async () => {
    await row(left, 'file00.txt').click({ modifiers: ['Control'] });
    await row(left, 'file05.txt').click({ modifiers: ['Control'] });
    await expect(left().locator('.row.selected')).toHaveCount(2);
    await row(left, 'file09.txt').click();
    await expect(left().locator('.row.selected')).toHaveCount(0);
    await expect(row(left, 'file09.txt')).toHaveClass(/cursor/);
    await page.keyboard.press('Shift+ArrowDown');
    await page.keyboard.press('Shift+ArrowDown');
    await expect(left().locator('.row.selected')).toHaveCount(3);
    await expect(row(left, 'file11.txt')).toHaveClass(/selected/);
  });

  test('grid view follows the same cursor/mark rules', async () => {
    await page.keyboard.press('Control+Shift+F2');
    await row(left, 'file00.txt').click();
    await page.keyboard.press('Space');
    await page.keyboard.press('ArrowDown');
    await page.keyboard.press('ArrowDown');
    await page.keyboard.press('Space');
    await expect(left().locator('.selected')).toHaveCount(2);
    await row(left, 'file20.txt').click();
    await expect(left().locator('.selected')).toHaveCount(0);
  });
});
