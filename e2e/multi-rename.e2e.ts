import { test, expect, type Page } from '@playwright/test';

/**
 * FER-117 E2E: Multi-Rename Tool end to end in browser mode (mock IPC, same
 * pattern as settings-tabs.e2e.ts). Covers opening via Ctrl+M with a
 * selection, live preview, blocking an invalid (Windows reserved) target,
 * running the batch (the mock `rename_batch` stands in for the Rust executor,
 * which has its own suite and a shared preview fixture), the post-run
 * report + refreshed listing, and session Undo.
 */

const FILES = ['a.txt', 'b.txt', 'c.txt'];

function injectTauriMock(page: Page) {
  page.addInitScript((files: string[]) => {
    const dir = 'C:\\Users\\demo';
    const state = { names: [...files], calls: [] as unknown[] };
    (window as unknown as Record<string, unknown>).__renameMock = state;
    const entries = () =>
      state.names.map((name) => ({
        name,
        path: `${dir}\\${name}`,
        isDir: false,
        isSymlink: false,
        size: 10,
        modifiedMs: 1700000000000,
        hidden: false,
      }));
    const bare = (p: string) => p.slice(p.lastIndexOf('\\') + 1);

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
        if (cmd === 'plugin:path|resolve_directory') return Promise.resolve(dir);
        if (cmd === 'plugin:event|listen') return Promise.resolve(Math.floor(Math.random() * 1e9));
        if (cmd === 'watch_start' || cmd === 'watch_stop') return Promise.resolve(1);
        if (cmd === 'list_dir') return Promise.resolve(entries());
        if (cmd === 'list_drives') return Promise.resolve([]);
        if (cmd === 'load_prefs') return Promise.resolve(null);
        if (cmd === 'save_prefs') return Promise.resolve(null);
        if (cmd === 'rename_batch') {
          const pairs = (args as { pairs: { from: string; to: string }[] }).pairs;
          state.calls.push(pairs);
          const journal: { from: string; to: string }[] = [];
          for (const p of pairs) {
            const i = state.names.indexOf(bare(p.from));
            if (i >= 0 && !state.names.includes(p.to)) {
              state.names[i] = p.to;
              journal.push(p);
            }
          }
          return Promise.resolve({
            requested: pairs.length,
            renamed: journal.length,
            failed: [],
            journal,
          });
        }
        return Promise.reject(new Error(`Unknown command: ${cmd}`));
      },
    };
  }, FILES);
}

test.describe('FER-117 multi-rename E2E (mock IPC)', () => {
  let page: Page;

  test.beforeEach(async ({ browser }) => {
    page = await browser.newPage();
    injectTauriMock(page);
    await page.goto('/');
    await page.waitForSelector('[data-pane="0"] .row[data-path]', {
      state: 'attached',
      timeout: 10_000,
    });
  });

  test.afterEach(async () => {
    await page.close();
  });

  const pane0 = () => page.locator('[data-pane="0"]');
  const dialog = () => page.getByRole('dialog', { name: 'Multi-Rename Tool' });
  const rows = () => dialog().getByLabel('Rename preview').locator('li');
  const renameBtn = () => dialog().getByRole('button', { name: 'Rename', exact: true });

  async function selectAllAndOpen() {
    await pane0().locator('.row[data-path$="a.txt"]').click();
    await page.keyboard.press('Control+a');
    await page.keyboard.press('Control+m');
    await expect(dialog()).toBeVisible();
    await expect(dialog()).toContainText('3 selected items');
  }

  const names = () =>
    page.evaluate(
      () => (window as unknown as { __renameMock: { names: string[] } }).__renameMock.names,
    );

  test('previews live, blocks a reserved name, renames the batch and undoes it', async () => {
    await selectAllAndOpen();

    // Live preview with a counter pattern; extension preserved.
    await dialog().getByPlaceholder(/\[N\]/).fill('file_[C]');
    await expect(rows()).toHaveCount(3);
    await expect(rows().nth(0)).toContainText('file_1.txt');
    await expect(rows().nth(2)).toContainText('file_3.txt');
    await expect(renameBtn()).toBeEnabled();

    // A Windows reserved device name is flagged and Rename is disabled.
    await dialog().getByPlaceholder(/\[N\]/).fill('CON');
    await expect(dialog().getByRole('alert')).toContainText('problem');
    await expect(rows().nth(0)).toContainText('invalid name');
    await expect(renameBtn()).toBeDisabled();

    // Back to a valid rule and run it.
    await dialog().getByPlaceholder(/\[N\]/).fill('file_[C]');
    await expect(renameBtn()).toBeEnabled();
    await renameBtn().click();
    await expect(dialog()).toContainText('Renamed 3 of 3');
    expect(await names()).toEqual(['file_1.txt', 'file_2.txt', 'file_3.txt']);
    await expect(pane0().locator('.row[data-path$="file_2.txt"]')).toBeVisible();

    // Session undo restores the original names.
    await dialog().getByRole('button', { name: 'Undo last rename' }).click();
    await expect(dialog()).toContainText('Renamed 3 of 3');
    expect(await names()).toEqual(FILES);
  });

  test('Escape closes the dialog without calling the executor', async () => {
    await selectAllAndOpen();
    await dialog().press('Escape');
    await expect(dialog()).toBeHidden();
    const calls = await page.evaluate(
      () => (window as unknown as { __renameMock: { calls: unknown[] } }).__renameMock.calls.length,
    );
    expect(calls).toBe(0);
  });
});
