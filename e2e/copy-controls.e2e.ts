import { test, expect, type Page } from '@playwright/test';

/**
 * FER-471 E2E: the Background Transfer Manager shows live copy-thread controls for a running copy
 * job and Settings > File operations exposes the default thread count and priority. The backend is
 * a mock that behaves like the Rust control (the real engine is covered by the Rust suite and the
 * real-window smoke).
 */

const PREFS_KEY = 'fer-prefs-copy-e2e-mock';

function injectTauriMock(page: Page) {
  page.addInitScript((key: string) => {
    const w = window as unknown as Record<string, unknown>;
    const BS = String.fromCharCode(92);
    const HOME = `C:${BS}Users${BS}demo`;
    let state = {
      operationId: 'op_e2e_1',
      auto: true,
      threads: 4,
      activeWorkers: 4,
      autoMax: 16,
      media: 'ssd',
      priority: 'normal',
      running: true,
    };
    w.__adjustCalls = [] as unknown[];
    window.__TAURI_INTERNALS__ = {
      metadata: {
        currentWindow: { label: 'main' },
        currentWebview: { windowLabel: 'main', label: 'main' },
      },
      plugins: {},
      transformCallback: (cb: unknown) => {
        const id = Math.floor(Math.random() * 1e9);
        w[`_${id}`] = cb;
        return id;
      },
      invoke: (cmd, args) => {
        if (cmd === 'plugin:path|resolve_directory') return Promise.resolve(HOME);
        if (cmd === 'plugin:event|listen') return Promise.resolve(Math.floor(Math.random() * 1e9));
        if (cmd === 'plugin:event|unlisten') return Promise.resolve(null);
        if (cmd === 'watch_start' || cmd === 'watch_stop') return Promise.resolve(1);
        if (cmd === 'list_dir') {
          return Promise.resolve([
            {
              name: 'big.bin',
              path: `${HOME}${BS}big.bin`,
              isDir: false,
              isSymlink: false,
              size: 1024,
              modifiedMs: 1700000000000,
              hidden: false,
            },
          ]);
        }
        if (cmd === 'list_drives') return Promise.resolve([]);
        if (cmd === 'list_operations') {
          return Promise.resolve([
            {
              operationId: 'op_e2e_1',
              kind: 'copy',
              state: 'running',
              processedBytes: 40,
              totalBytes: 100,
              processedFiles: 4,
              totalFiles: 10,
              currentPath: `${HOME}${BS}big.bin`,
            },
          ]);
        }
        if (cmd === 'copy_control_snapshot') return Promise.resolve([state]);
        if (cmd === 'copy_control_adjust') {
          const a = args as { operationId: string; change: Record<string, unknown> };
          (w.__adjustCalls as unknown[]).push(a.change);
          const c = a.change;
          if (c.kind === 'up') state = { ...state, auto: false, threads: state.threads + 1 };
          if (c.kind === 'down') state = { ...state, auto: false, threads: state.threads - 1 };
          if (c.kind === 'auto') state = { ...state, auto: c.enabled as boolean };
          if (c.kind === 'priority') state = { ...state, priority: c.priority as string };
          state = { ...state, activeWorkers: state.threads };
          return Promise.resolve(state);
        }
        if (cmd === 'load_prefs') {
          const raw = window.localStorage.getItem(key);
          return Promise.resolve(
            raw === null
              ? {
                  theme: 'dark',
                  palette: 'violet',
                  showHidden: false,
                  terminal: 'auto',
                  paneSplit: 0.5,
                  session: null,
                }
              : JSON.parse(raw),
          );
        }
        if (cmd === 'save_prefs') {
          const a = (args ?? {}) as { prefs?: unknown };
          window.localStorage.setItem(key, JSON.stringify(a.prefs ?? {}));
          return Promise.resolve(null);
        }
        return Promise.reject(new Error(`Unknown command: ${cmd}`));
      },
    };
  }, PREFS_KEY);
}

test.describe('FER-471 copy thread controls (mock IPC)', () => {
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

  test('the progress panel steps threads, toggles Otomatis and changes priority live', async () => {
    await page.getByTitle('Operations').click();
    const group = page.getByRole('group', { name: 'Kontrol thread copy' });
    await expect(group).toBeVisible();
    await expect(page.getByTestId('copy-threads')).toHaveText('4');
    await expect(page.getByLabel('Otomatis')).toBeChecked();

    await page.getByLabel('Tambah thread').click();
    await expect(page.getByTestId('copy-threads')).toHaveText('5');
    await expect(page.getByLabel('Otomatis')).not.toBeChecked();

    await page.getByLabel('Kurangi thread').click();
    await page.getByLabel('Kurangi thread').click();
    await expect(page.getByTestId('copy-threads')).toHaveText('3');

    await page.getByLabel('Prioritas copy').selectOption('background');
    await expect(page.getByLabel('Prioritas copy')).toHaveValue('background');

    await page.getByLabel('Otomatis').check();
    await expect(page.getByLabel('Otomatis')).toBeChecked();

    const calls = await page.evaluate(
      () => (window as unknown as { __adjustCalls: unknown[] }).__adjustCalls,
    );
    expect(calls).toEqual([
      { kind: 'up' },
      { kind: 'down' },
      { kind: 'down' },
      { kind: 'priority', priority: 'background' },
      { kind: 'auto', enabled: true },
    ]);
  });

  test('Settings keeps the default thread count and priority across a reload', async () => {
    await page.keyboard.press('Control+,');
    const dialog = page.getByRole('dialog', { name: 'Settings' });
    await expect(dialog).toBeVisible();
    await dialog.getByRole('combobox', { name: 'Thread copy' }).selectOption('8');
    await dialog.getByRole('combobox', { name: 'Prioritas copy' }).selectOption('background');
    await expect
      .poll(async () => {
        const raw = await page.evaluate((k) => window.localStorage.getItem(k), PREFS_KEY);
        const saved = raw ? (JSON.parse(raw) as Record<string, unknown>) : {};
        return [saved.copyThreads, saved.copyPriority];
      })
      .toEqual([8, 'background']);
    await page.reload();
    await page.waitForSelector('[data-pane="0"] .row[data-path]', { state: 'attached' });
    await page.keyboard.press('Control+,');
    const again = page.getByRole('dialog', { name: 'Settings' });
    await expect(again.getByRole('combobox', { name: 'Thread copy' })).toHaveValue('8');
    await expect(again.getByRole('combobox', { name: 'Prioritas copy' })).toHaveValue('background');
  });
});
