import { test, expect, type Page } from '@playwright/test';

/**
 * FER-491 E2E: slim pane toolbar, smart breadcrumb and the "address bar on its
 * own row" option. Tauri IPC is mocked; prefs live in localStorage so reload
 * proves the save_prefs -> load_prefs round-trip at the frontend layer.
 */

const PREFS_KEY = 'fer-491-prefs-e2e-mock';
const SEP = String.fromCharCode(92);
const LONG = [
  'C:',
  'Users',
  'demo',
  'Projects',
  'clients',
  'acme-corp',
  '2026',
  'Q3-reports',
  'final-deliverables',
].join(SEP);

function injectTauriMock(page: Page) {
  page.addInitScript(
    ([key, long, sep]: string[]) => {
      const entry = (n: string, d: boolean) => ({
        name: n,
        path: `${long}${sep}${n}`,
        isDir: d,
        isSymlink: false,
        size: d ? 0 : 2048,
        modifiedMs: 1700000000000,
        hidden: false,
      });
      const DEFAULT_PREFS = {
        theme: 'dark',
        palette: 'violet',
        showHidden: false,
        terminal: 'auto',
        paneSplit: 0.5,
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
          if (cmd === 'plugin:path|resolve_directory') return Promise.resolve(long);
          if (cmd === 'plugin:event|listen') return Promise.resolve(1);
          if (cmd === 'watch_start' || cmd === 'watch_stop') return Promise.resolve(1);
          if (cmd === 'list_dir')
            return Promise.resolve([
              entry('reports', true),
              entry('Summary.docx', false),
              entry('budget.xlsx', false),
            ]);
          if (cmd === 'list_drives' || cmd === 'disk_usage_cache_list') return Promise.resolve([]);
          if (cmd === 'load_prefs') {
            const raw = window.localStorage.getItem(key);
            return Promise.resolve(raw === null ? DEFAULT_PREFS : JSON.parse(raw));
          }
          if (cmd === 'save_prefs') {
            const a = (args ?? {}) as { prefs?: unknown };
            window.localStorage.setItem(key, JSON.stringify(a.prefs ?? {}));
            return Promise.resolve(null);
          }
          return Promise.resolve(null);
        },
      };
    },
    [PREFS_KEY, LONG, SEP],
  );
}

test.describe('pane toolbar + breadcrumb (FER-491)', () => {
  let page: Page;
  let pageErrors: string[];

  test.beforeEach(async ({ browser }) => {
    // 590px panes: 2 * 590 + sidebar/divider chrome.
    page = await browser.newPage({ viewport: { width: 1392, height: 600 } });
    // An uncaught error in a render effect can abort the rest of that render flush (this once
    // left the pane without rows now and then), so any page error fails the test.
    pageErrors = [];
    page.on('pageerror', (e) => pageErrors.push(String(e.stack)));
    injectTauriMock(page);
    await page.goto('/');
    await page.waitForSelector('[data-path]', { state: 'attached', timeout: 10_000 });
  });

  test.afterEach(async () => {
    await page.close();
    expect(pageErrors).toEqual([]);
  });

  const pane = () => page.locator('section.pane').first();

  test('590px pane: active folder stays whole, the middle folds into an ellipsis button', async () => {
    const box = await pane().boundingBox();
    expect(Math.round(box?.width ?? 0)).toBe(590);
    const crumbs = pane().locator('nav[aria-label="Path"]');
    await expect(crumbs.getByRole('button', { name: 'final-deliverables' })).toBeVisible();
    await expect(crumbs.getByRole('button', { name: 'Hidden folders' })).toBeVisible();
    // No initials any more: every visible crumb carries a real folder name.
    const labels = await crumbs.locator('button.crumb').allInnerTexts();
    expect(labels.every((l) => l.trim().length > 2 || l.trim() === 'C:')).toBe(true);
    // Hover shows the full path.
    await expect(crumbs).toHaveAttribute('title', LONG);
    // The active folder is not clipped.
    const last = crumbs.getByRole('button', { name: 'final-deliverables' });
    expect(await last.evaluate((el) => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
  });

  test('clicking the ellipsis lists hidden folders; keyboard picks one and navigates', async () => {
    const crumbs = pane().locator('nav[aria-label="Path"]');
    await crumbs.getByRole('button', { name: 'Hidden folders' }).click();
    const menu = pane().getByRole('menu', { name: 'Hidden folders' });
    await expect(menu).toBeVisible();
    const items = menu.getByRole('menuitem');
    expect(await items.count()).toBeGreaterThan(1);
    await expect(items.first()).toBeFocused();
    await page.keyboard.press('ArrowDown');
    await expect(items.nth(1)).toBeFocused();
    const name = (await items.nth(1).innerText()).trim();
    await page.keyboard.press('Enter');
    await expect(menu).toBeHidden();
    // Navigated: the chosen folder is now the active (last) crumb.
    await expect(crumbs.locator('button.crumb.last')).toHaveText(name);
    await expect(crumbs).toHaveAttribute('title', new RegExp(`${name}$`));
  });

  test('Escape closes the ellipsis menu and returns focus to its button', async () => {
    const more = pane().getByRole('button', { name: 'Hidden folders' });
    await more.click();
    await page.keyboard.press('Escape');
    await expect(pane().getByRole('menu', { name: 'Hidden folders' })).toBeHidden();
    await expect(more).toBeFocused();
  });

  test('view dropdown switches between Details and Grid', async () => {
    const btn = pane().getByRole('button', { name: /^View:/ });
    await expect(btn).toHaveAttribute('aria-label', 'View: Details');
    await btn.click();
    await expect(pane().getByRole('menuitemradio', { name: 'List' })).toBeDisabled();
    await pane().getByRole('menuitemradio', { name: 'Grid' }).click();
    await expect(pane().getByRole('button', { name: 'View: Grid' })).toBeVisible();
    await pane().getByRole('button', { name: 'View: Grid' }).click();
    await pane().getByRole('menuitemradio', { name: 'Details' }).click();
    await expect(pane().getByRole('button', { name: 'View: Details' })).toBeVisible();
  });

  test('filter icon expands into an input and filters the list', async () => {
    await expect(pane().locator('.filter input')).toHaveCount(0);
    await pane().getByRole('button', { name: 'Filter' }).click();
    const input = pane().getByRole('textbox', { name: 'Filter' });
    await expect(input).toBeFocused();
    await input.fill('Summary');
    await expect(pane().locator('.row[data-path]')).toHaveCount(1);
    await expect(pane().locator('.row[data-path]').first()).toContainText('Summary');
    // A live query keeps the box open after blur.
    await pane()
      .locator('nav[aria-label="Path"]')
      .click({ position: { x: 4, y: 4 } });
    await expect(input).toBeVisible();
    // Escape clears it and folds back to the icon.
    await input.focus();
    await page.keyboard.press('Escape');
    await expect(pane().getByRole('button', { name: 'Filter' })).toBeVisible();
    await expect(pane().locator('.row[data-path]').first()).toBeVisible();
  });

  test('"Address bar on its own row" moves the address bar above the toolbar and survives reload', async () => {
    const addrY = async () => (await pane().locator('nav[aria-label="Path"]').boundingBox())!.y;
    const backY = async () => (await pane().getByRole('button', { name: 'Back' }).boundingBox())!.y;
    // Default: back button and breadcrumb share one row.
    expect(Math.abs((await addrY()) - (await backY()))).toBeLessThan(14);

    await page.keyboard.press('Control+,');
    const dialog = page.getByRole('dialog', { name: 'Settings' });
    await dialog.getByRole('switch', { name: 'Address bar on its own row' }).click();
    await dialog.getByTitle('Close').click();

    const nav = await pane().locator('nav[aria-label="Path"]').boundingBox();
    const pb = await pane().boundingBox();
    expect(await backY()).toBeGreaterThan((await addrY()) + 20);
    // The address row spans (nearly) the whole pane width.
    expect(nav!.width).toBeGreaterThan(pb!.width - 90);

    await expect
      .poll(() => page.evaluate((k) => window.localStorage.getItem(k), PREFS_KEY))
      .toContain('"addressOwnRow":true');
    await page.reload();
    await page.waitForSelector('[data-path]', { state: 'attached', timeout: 10_000 });
    expect(await backY()).toBeGreaterThan((await addrY()) + 20);
  });

  test('file list column resize: dragging Size handle resizes column and survives reload', async () => {
    const head = pane().locator('.head');
    await expect(head).toBeVisible();
    const sizeHandle = head.locator('.hSize [role="separator"]');
    await expect(sizeHandle).toBeVisible();

    const initialTemplate = await head.evaluate((el) => el.style.gridTemplateColumns);
    expect(initialTemplate).toContain('72px');

    const box = await sizeHandle.boundingBox();
    expect(box).not.toBeNull();

    // Drag the handle to the left (by 30px) to widen Size column
    await page.mouse.move(box!.x + box!.width / 2, box!.y + box!.height / 2);
    await page.mouse.down();
    await page.mouse.move(box!.x + box!.width / 2 - 30, box!.y + box!.height / 2, { steps: 5 });
    await page.mouse.up();

    // Template should reflect the increased width
    const updatedTemplate = await head.evaluate((el) => el.style.gridTemplateColumns);
    expect(updatedTemplate).not.toBe(initialTemplate);
    expect(updatedTemplate).toContain('102px');

    // Reload and verify persistence
    await page.reload();
    await page.waitForSelector('[data-path]', { state: 'attached', timeout: 10_000 });
    const reloadedTemplate = await pane()
      .locator('.head')
      .evaluate((el) => el.style.gridTemplateColumns);
    expect(reloadedTemplate).toContain('102px');
  });
});
