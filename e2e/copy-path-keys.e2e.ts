import { test, expect, type Page } from '@playwright/test';

/**
 * FER-483 E2E: Ctrl+1 / Ctrl+2 / Ctrl+3 copy the folder path / names / full paths.
 * navigator.clipboard.writeText is replaced by a recorder; Tauri IPC is mocked
 * with a tiny in-memory filesystem (same pattern as the other e2e files).
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

test.describe('Copy path/name shortcuts (FER-483)', () => {
  let page: Page;
  const left = () => page.getByRole('region', { name: 'Left pane' });
  const row = (name: string) => left().locator(`[data-path$="\\\\${name}"]`);
  const clip = () => page.evaluate(() => (window as unknown as { __clip: string[] }).__clip);

  test.beforeEach(async ({ browser }) => {
    page = await browser.newPage();
    await page.addInitScript(() => {
      (window as unknown as { __clip: string[] }).__clip = [];
      Object.defineProperty(navigator, 'clipboard', {
        configurable: true,
        value: {
          writeText: (t: string) => {
            (window as unknown as { __clip: string[] }).__clip.push(t);
            return Promise.resolve();
          },
        },
      });
    });
    injectTauriMock(page);
    await page.goto('/');
    await expect(row('invoice')).toBeVisible();
  });

  test.afterEach(async () => {
    await page.close();
  });

  test('Ctrl+1 copies the folder with a trailing backslash', async () => {
    await row('notes.txt').click();
    await page.keyboard.press('Control+1');
    await expect(page.getByTestId('toast')).toHaveText('Disalin: path folder');
    expect(await clip()).toEqual(['C:\\Users\\demo\\']);
  });

  test('Ctrl+2 with 3 selected items copies 3 names joined by CRLF', async () => {
    await row('invoice').click({ modifiers: ['Control'] });
    await row('notes.txt').click({ modifiers: ['Control'] });
    await row('zeta.txt').click({ modifiers: ['Control'] });
    await page.keyboard.press('Control+2');
    await expect(page.getByTestId('toast')).toHaveText('Disalin: 3 nama');
    expect(await clip()).toEqual(['invoice\r\nnotes.txt\r\nzeta.txt']);
  });

  test('Ctrl+3 without a selection copies the cursor item path', async () => {
    await row('pack.zip').click();
    await page.keyboard.press('Control+3');
    await expect(page.getByTestId('toast')).toHaveText('Disalin: 1 path');
    expect(await clip()).toEqual(['C:\\Users\\demo\\pack.zip']);
  });

  test('the shortcuts do nothing while the address bar has focus', async () => {
    await page.keyboard.press('Alt+d');
    const input = left().getByRole('combobox', { name: 'Address' });
    await expect(input).toBeFocused();
    await page.keyboard.press('Control+1');
    await page.keyboard.press('Control+2');
    await page.keyboard.press('Control+3');
    expect(await clip()).toEqual([]);
    await expect(page.getByTestId('toast')).toHaveCount(0);
  });
});
