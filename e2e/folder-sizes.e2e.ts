import { test, expect, type Page } from '@playwright/test';

/**
 * FER-477a E2E (mock IPC): folder sizes from the FerrumIndexer in the Size column. Opt-in through
 * Settings > Indexer; the service answers only in High performance mode (shared mode: "—"). The
 * service itself cannot run in a browser: its refusal in shared mode is proven by the Rust tests
 * (`console_ipc.rs`), this only drives the UI.
 */

interface Mock {
  mode: 'shared' | 'high_performance';
  prefs: Record<string, unknown>;
  sizeCalls: { parent: string; names: string[] }[];
}

const SEP = String.fromCharCode(92);

function inject(page: Page, mode: Mock['mode'], folderSizes: boolean) {
  page.addInitScript(
    ({ mode, folderSizes, sep }: { mode: Mock['mode']; folderSizes: boolean; sep: string }) => {
      const home = ['C:', 'Users', 'demo'].join(sep);
      const state: Mock = {
        mode,
        prefs: folderSizes ? { indexerFolderSizes: true } : {},
        sizeCalls: [],
      };
      (window as unknown as { __mock: Mock }).__mock = state;
      const entry = (name: string, isDir: boolean, size: number) => ({
        name,
        path: [home, name].join(sep),
        isDir,
        isSymlink: false,
        size,
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
          if (cmd === 'plugin:event|listen')
            return Promise.resolve(Math.floor(Math.random() * 1e9));
          if (cmd === 'watch_start' || cmd === 'watch_stop') return Promise.resolve(1);
          if (cmd === 'list_dir') {
            return Promise.resolve([
              entry('Projects', true, 0),
              entry('Empty', true, 0),
              entry('README.md', false, 1024),
            ]);
          }
          if (cmd === 'list_drives') {
            return Promise.resolve([
              { label: 'Data', path: 'D:' + sep, freeBytes: 1e9, totalBytes: 5e11 },
            ]);
          }
          if (cmd === 'load_prefs') return Promise.resolve(state.prefs);
          if (cmd === 'save_prefs') {
            state.prefs = ((args ?? {}) as { prefs?: Record<string, unknown> }).prefs ?? {};
            return Promise.resolve(null);
          }
          if (cmd === 'indexer_status') {
            return Promise.resolve({
              installed: true,
              scmState: 'running',
              reachable: true,
              version: '0.1.0',
              mode: state.mode,
              state: 'running',
              index: 'ready',
              canLowerPrivacy: false,
            });
          }
          if (cmd === 'indexer_folder_sizes') {
            const a = (args ?? {}) as { parent: string; names: string[] };
            state.sizeCalls.push({ parent: a.parent, names: a.names });
            if (state.mode === 'shared') {
              return Promise.resolve({
                available: false,
                reason: 'shared_mode',
                sizes: a.names.map(() => null),
                stale: false,
              });
            }
            return Promise.resolve({
              available: true,
              sizes: a.names.map((n) => (n === 'Projects' ? 5 * 1024 * 1024 : 0)),
              stale: false,
            });
          }
          return Promise.reject(new Error(`Unknown command: ${cmd}`));
        },
      };
    },
    { mode, folderSizes, sep: SEP },
  );
}

function sizeCell(page: Page, name: string) {
  return page
    .locator('[data-pane="0"] .row[data-path]')
    .filter({ has: page.locator('.nameText', { hasText: new RegExp(`^${name}$`) }) })
    .locator('.size');
}

async function open(page: Page) {
  await page.goto('/');
  await page.waitForSelector('[data-pane="0"] .row[data-path]', {
    state: 'attached',
    timeout: 10_000,
  });
}

async function sizeCalls(page: Page) {
  return page.evaluate(() => (window as unknown as { __mock: Mock }).__mock.sizeCalls);
}

test.describe('FER-477a folder sizes (mock IPC)', () => {
  test('High performance + setting on: folder rows show their indexed size', async ({ page }) => {
    inject(page, 'high_performance', true);
    await open(page);
    await expect(sizeCell(page, 'Projects')).toHaveText('5.00 MB');
    await expect(sizeCell(page, 'Empty')).toHaveText('0 B');
    // A file keeps its own size (the extension may be hidden by the partial mock prefs).
    await expect(sizeCell(page, 'README(\\.md)?')).toHaveText('1.00 KB');
    const calls = await sizeCalls(page);
    expect(calls[0].parent).toBe(['C:', 'Users', 'demo'].join(SEP));
    expect([...calls[0].names].sort()).toEqual(['Empty', 'Projects']);
  });

  test('shared mode: the service refuses and the cells stay "—"', async ({ page }) => {
    inject(page, 'shared', true);
    await open(page);
    await expect.poll(async () => (await sizeCalls(page)).length).toBeGreaterThan(0);
    await expect(sizeCell(page, 'Projects')).toHaveText('—');
  });

  test('setting off (default): nothing is asked; the toggle lives in Settings > Indexer', async ({
    page,
  }) => {
    inject(page, 'high_performance', false);
    await open(page);
    await expect(sizeCell(page, 'Projects')).toHaveText('—');
    expect(await sizeCalls(page)).toEqual([]);
    await page.keyboard.press('Control+,');
    const dialog = page.getByRole('dialog', { name: 'Settings' });
    await dialog.getByRole('button', { name: 'Indexer' }).click();
    await dialog.getByTestId('indexer-folder-sizes').getByRole('checkbox').check();
    await page.keyboard.press('Escape');
    await expect(sizeCell(page, 'Projects')).toHaveText('5.00 MB');
  });
});
