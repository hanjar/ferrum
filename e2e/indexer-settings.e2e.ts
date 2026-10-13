import { test, expect, type Page } from '@playwright/test';

/**
 * FER-474 E2E (mock IPC): Settings > Indexer shows the FerrumIndexer status and only changes the
 * privacy mode after the confirmation dialog. The service itself (SCM, pipe, UAC) cannot run in a
 * browser; that is covered by the Rust console IPC tests and the manual steps in
 * docs/indexer-service.md.
 */

interface MockState {
  installed: boolean;
  mode: 'shared' | 'high_performance';
  calls: { mode: string; confirmed: boolean }[];
}

function injectTauriMock(page: Page, installed: boolean) {
  page.addInitScript(
    ({ installed }: { installed: boolean }) => {
      const state: MockState = { installed, mode: 'shared', calls: [] };
      (window as unknown as { __indexerMock: MockState }).__indexerMock = state;
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
          if (cmd === 'plugin:path|resolve_directory') return Promise.resolve('C:\\Users\\demo');
          if (cmd === 'plugin:event|listen')
            return Promise.resolve(Math.floor(Math.random() * 1e9));
          if (cmd === 'watch_start' || cmd === 'watch_stop') return Promise.resolve(1);
          if (cmd === 'list_dir') {
            return Promise.resolve([
              {
                name: 'README.md',
                path: 'C:\\Users\\demo\\README.md',
                isDir: false,
                isSymlink: false,
                size: 1024,
                modifiedMs: 1700000000000,
                hidden: false,
              },
            ]);
          }
          if (cmd === 'list_drives') {
            return Promise.resolve([
              { label: 'Data', path: 'D:\\', freeBytes: 1e9, totalBytes: 5e11 },
            ]);
          }
          if (cmd === 'load_prefs') return Promise.resolve({});
          if (cmd === 'save_prefs') return Promise.resolve(null);
          if (cmd === 'indexer_status') {
            return Promise.resolve(
              state.installed
                ? {
                    installed: true,
                    scmState: 'running',
                    reachable: true,
                    version: '0.1.0',
                    mode: state.mode,
                    state: 'running',
                    index: 'none',
                    canLowerPrivacy: false,
                  }
                : { installed: false, reachable: false, canLowerPrivacy: false },
            );
          }
          if (cmd === 'indexer_set_mode') {
            const a = (args ?? {}) as { mode: 'shared' | 'high_performance'; confirmed: boolean };
            state.calls.push({ mode: a.mode, confirmed: a.confirmed });
            if (!a.confirmed) return Promise.reject({ code: 'not_confirmed', message: 'x' });
            state.mode = a.mode;
            return Promise.resolve({ mode: a.mode, changed: true, elevated: true });
          }
          return Promise.reject(new Error(`Unknown command: ${cmd}`));
        },
      };
    },
    { installed },
  );
}

async function openIndexer(page: Page) {
  await page.goto('/');
  await page.waitForSelector('[data-pane="0"] .row[data-path]', {
    state: 'attached',
    timeout: 10_000,
  });
  await page.keyboard.press('Control+,');
  const dialog = page.getByRole('dialog', { name: 'Settings' });
  await expect(dialog).toBeVisible();
  await dialog.getByRole('button', { name: 'Indexer' }).click();
  return dialog;
}

async function calls(page: Page) {
  return page.evaluate(
    () => (window as unknown as { __indexerMock: MockState }).__indexerMock.calls,
  );
}

test.describe('FER-474 Settings > Indexer (mock IPC)', () => {
  test('not installed: status and fallback text, no mode change offered', async ({ page }) => {
    injectTauriMock(page, false);
    const dialog = await openIndexer(page);
    await expect(dialog.getByTestId('indexer-status')).toContainText('Tidak terpasang');
    await expect(dialog.getByText(/belum terpasang/)).toBeVisible();
    await expect(dialog.getByRole('button', { name: /Ganti ke/ })).toHaveCount(0);
  });

  test('raising to High performance needs the privacy acknowledgement', async ({ page }) => {
    injectTauriMock(page, true);
    const dialog = await openIndexer(page);
    await expect(dialog.getByTestId('indexer-mode')).toContainText('Bersama');
    await dialog.getByRole('button', { name: 'Ganti ke High performance…' }).click();
    const confirm = page.getByRole('alertdialog');
    await expect(confirm).toContainText('pengguna lain');
    const go = confirm.getByRole('button', { name: 'Ganti mode' });
    await expect(go).toBeDisabled();

    // Escape closes only the confirmation, not Settings.
    await confirm.press('Escape');
    await expect(confirm).toBeHidden();
    await expect(dialog).toBeVisible();
    expect(await calls(page)).toEqual([]);

    await dialog.getByRole('button', { name: 'Ganti ke High performance…' }).click();
    await page.getByRole('alertdialog').getByRole('checkbox').check();
    await page.getByRole('alertdialog').getByRole('button', { name: 'Ganti mode' }).click();
    await expect(dialog.getByTestId('indexer-message')).toContainText('Mode diganti');
    await expect(dialog.getByTestId('indexer-mode')).toContainText('High performance');
    expect(await calls(page)).toEqual([{ mode: 'high_performance', confirmed: true }]);
  });
});
