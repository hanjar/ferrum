import { test, expect, type Page } from '@playwright/test';

/**
 * FER-456 E2E: the editable address bar (click → type → suggestions → open).
 * Tauri IPC is mocked with a tiny in-memory tree.
 */

function injectTauriMock(page: Page) {
  page.addInitScript(() => {
    const dirs = ['C:\\', 'C:\\Users', 'C:\\Users\\demo', 'C:\\Users\\demo\\Documents'];
    const sub: Record<string, string[]> = {
      'C:\\Users\\demo': ['Documents', 'Downloads', 'Desktop'],
    };
    const entry = (parent: string, name: string) => ({
      name,
      path: `${parent}\\${name}`,
      isDir: true,
      isSymlink: false,
      size: 0,
      modifiedMs: 1700000000000,
      hidden: false,
    });
    (window as unknown as { __calls: unknown[] }).__calls = [];
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
        (window as unknown as { __calls: unknown[] }).__calls.push({ cmd, args });
        if (cmd === 'plugin:path|resolve_directory') return Promise.resolve('C:\\Users\\demo');
        if (cmd === 'plugin:event|listen') return Promise.resolve(Math.floor(Math.random() * 1e9));
        if (cmd === 'watch_start' || cmd === 'watch_stop') return Promise.resolve(1);
        if (cmd === 'list_dir') {
          const p = (args as { path: string }).path;
          return Promise.resolve((sub[p] ?? []).map((n) => entry(p, n)));
        }
        if (cmd === 'path_suggest') {
          const { input } = args as { input: string };
          const i = input.lastIndexOf('\\');
          const folder = input.slice(0, i);
          const prefix = input.slice(i + 1).toLowerCase();
          const items = (sub[folder] ?? [])
            .filter((n) => n.toLowerCase().startsWith(prefix))
            .map((n) => ({ name: n, path: `${folder}\\${n}`, kind: 'dir', link: false }));
          return Promise.resolve({ items, truncated: false, timedOut: false });
        }
        if (cmd === 'path_resolve') {
          const { input } = args as { input: string };
          const clean = input.replace(/\\$/, '');
          const exists = dirs.includes(clean) || /Downloads|Desktop/.test(clean);
          return Promise.resolve({ path: clean, kind: exists ? 'dir' : 'missing' });
        }
        return Promise.resolve(null);
      },
    };
  });
}

test.describe('address bar (FER-456)', () => {
  test.beforeEach(async ({ page }) => {
    injectTauriMock(page);
    await page.goto('/');
    await expect(page.locator('section.pane').first()).toBeVisible();
  });

  test('click empty area, type, pick a suggestion with Tab, Enter opens the folder', async ({
    page,
  }) => {
    const pane = page.locator('section.pane').first();
    await pane.getByLabel('Edit path').click();
    const input = pane.getByRole('combobox', { name: 'Address' });
    await expect(input).toBeFocused();

    await input.fill('C:\\Users\\demo\\dow');
    const opts = pane.getByRole('option');
    await expect(opts).toHaveCount(1);
    await expect(opts.first()).toContainText('Downloads');

    await input.press('Tab');
    await expect(input).toHaveValue('C:\\Users\\demo\\Downloads\\');
    await input.press('Enter');
    await expect(input).toHaveCount(0);
    await expect(pane.getByRole('navigation', { name: 'Path' })).toContainText('Downloads');
  });

  test('invalid path keeps the input open with a message; Escape returns to breadcrumb', async ({
    page,
  }) => {
    const pane = page.locator('section.pane').first();
    await pane.getByLabel('Edit path').click();
    const input = pane.getByRole('combobox', { name: 'Address' });
    await input.fill('C:\\no\\such');
    await input.press('Enter');
    await expect(pane.getByRole('alert')).toContainText('not found');
    await expect(input).toBeVisible();
    await input.press('Escape');
    await expect(input).toHaveCount(0);
    await expect(pane.getByRole('navigation', { name: 'Path' })).toBeVisible();
  });

  test('Alt+D opens the editor from the keyboard', async ({ page }) => {
    await page.keyboard.press('Alt+d');
    await expect(page.getByRole('combobox', { name: 'Address' }).first()).toBeFocused();
  });

  test('arrow keys move the active option', async ({ page }) => {
    const pane = page.locator('section.pane').first();
    await pane.getByLabel('Edit path').click();
    const input = pane.getByRole('combobox', { name: 'Address' });
    await input.fill('C:\\Users\\demo\\D');
    await expect(pane.getByRole('option')).toHaveCount(3);
    await input.press('ArrowDown');
    await input.press('ArrowDown');
    const active = await input.getAttribute('aria-activedescendant');
    await expect(pane.locator(`#${active}`)).toHaveAttribute('aria-selected', 'true');
    await expect(pane.locator(`#${active}`)).toContainText('Downloads');
  });
});
