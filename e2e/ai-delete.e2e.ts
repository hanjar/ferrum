import { test, expect, type Page } from '@playwright/test';

/**
 * FER-576 E2E (mock IPC + mock provider): "hapus yang kupilih" builds a WARNING card for the marked items. The
 * confirm button waits for the item names, is not the default focus, has no mnemonic, and no key press in the
 * chat approves it (review R1 S4/S8). Nothing is deleted and no approval is consumed in this test: the backend
 * commands that write (`ai_delete_to_recycle`, `ai_authorize_execute`) must never be called.
 */

function injectTauriMock(page: Page) {
  page.addInitScript(() => {
    const BS = String.fromCharCode(92);
    const w = window as unknown as Record<string, unknown>;
    const calls: { cmd: string; args: unknown }[] = [];
    w.__calls = calls;
    w.__plans = [] as unknown[];
    const dir = `C:${BS}Users${BS}demo`;
    const file = (name: string) => ({
      name,
      path: `${dir}${BS}${name}`,
      isDir: false,
      isSymlink: false,
      size: 10,
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
        w[`_${id}`] = cb;
        return id;
      },
      invoke: (cmd, args) => {
        calls.push({ cmd, args });
        if (cmd === 'plugin:path|resolve_directory') return Promise.resolve(dir);
        if (cmd === 'plugin:event|listen') return Promise.resolve(Math.floor(Math.random() * 1e9));
        if (cmd === 'plugin:event|unlisten') return Promise.resolve(null);
        if (cmd === 'watch_start' || cmd === 'watch_stop') return Promise.resolve(1);
        if (cmd === 'list_dir') return Promise.resolve([file('README.md'), file('notes.txt')]);
        if (cmd === 'list_drives') return Promise.resolve([]);
        if (cmd === 'list_operations') return Promise.resolve([]);
        if (cmd === 'load_prefs')
          return Promise.resolve({
            theme: 'dark',
            palette: 'violet',
            showHidden: true,
            terminal: 'auto',
            paneSplit: 0.5,
            session: null,
          });
        if (cmd === 'save_prefs') return Promise.resolve(null);
        if (cmd === 'ai_setup_load') {
          return Promise.resolve({
            endpoint: 'http://127.0.0.1:20128/v1',
            model: 'test-model',
            timeoutMs: 30000,
            credentialRef: 'Ferrum/AI/custom',
            structured: 'none',
          });
        }
        if (cmd === 'ai_test_connection') return Promise.resolve({ ok: true, kind: null });
        if (cmd === 'ai_exec_unresolved') return Promise.resolve([]);
        if (cmd === 'ai_plan_intent') {
          const a = args as { request: { requestId: string } };
          (w.__plans as unknown[]).push(JSON.parse(JSON.stringify(a)));
          return Promise.resolve({
            schemaVersion: 1,
            requestId: a.request.requestId,
            intent: {
              capability: 'items.delete',
              sourcePane: 'left',
              selector: { kind: 'selected' },
            },
          });
        }
        if (cmd === 'ai_recycle_check') {
          const a = args as { paths: string[] };
          return Promise.resolve(
            a.paths.map((path) => ({
              path,
              ok: true,
              reason: null,
              bytes: 10,
              entries: 1,
              links: 0,
            })),
          );
        }
        if (cmd === 'ai_register_approval') return Promise.resolve({ scopeDigest: 'scope-1' });
        if (cmd === 'ai_revoke_approval') return Promise.resolve(null);
        return Promise.reject(new Error(`Unknown command: ${cmd}`));
      },
    };
  });
}

const chat = (page: Page) => page.getByRole('complementary', { name: 'AI assistant' });

const calls = (page: Page) =>
  page.evaluate(() =>
    (window as unknown as { __calls: { cmd: string }[] }).__calls.map((c) => c.cmd),
  );

test.describe('FER-576 AI delete card (mock IPC, mock provider)', () => {
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

  async function askToDelete() {
    // Mark one file (Ctrl+click toggles a mark).
    await page
      .locator('[data-pane="0"] .row[data-path*="README"]')
      .click({ modifiers: ['Control'] });
    await page.getByRole('button', { name: 'AI assistant', exact: true }).click();
    await expect(chat(page)).toBeVisible();
    await chat(page)
      .getByRole('textbox', { name: 'Message to AI assistant' })
      .fill('hapus file yang kupilih');
    await chat(page).getByRole('button', { name: 'Send' }).click();
  }

  test('shows a warning card; the confirm button needs the names, is not focused and has no mnemonic', async () => {
    await askToDelete();
    const warning = chat(page).getByTestId('delete-warning');
    await expect(warning).toBeVisible();
    await expect(warning).toContainText('Recycle Bin');
    // The names of what will be deleted are on screen, and only then can the delete be confirmed.
    await expect(chat(page).getByTestId('delete-items')).toContainText('README.md');
    const confirm = chat(page).getByTestId('confirm-delete');
    await expect(confirm).toBeEnabled();
    await expect(confirm).not.toBeFocused();
    await expect(confirm).not.toHaveAttribute('accesskey', /.+/);
    await expect(confirm.locator('u')).toHaveCount(0);
    // Cancel comes first in the tab order.
    const labels = await chat(page)
      .getByTestId('delete-warning')
      .locator('xpath=ancestor::section[1]')
      .getByRole('button')
      .allTextContents();
    expect(labels.map((t) => t.trim()).indexOf('Cancel')).toBeLessThan(
      labels.map((t) => t.trim()).indexOf('Pindahkan ke Recycle Bin'),
    );
  });

  test('no key press in the chat approves the delete card', async () => {
    await askToDelete();
    await expect(chat(page).getByTestId('delete-warning')).toBeVisible();
    const input = chat(page).getByRole('textbox', { name: 'Message to AI assistant' });
    await input.focus();
    await page.keyboard.press('Enter');
    await page.keyboard.press('Space');
    await page.keyboard.press('Control+Enter');
    await expect(chat(page).getByTestId('delete-warning')).toBeVisible();
    const ran = await calls(page);
    expect(ran).not.toContain('ai_authorize_execute');
    expect(ran).not.toContain('ai_delete_to_recycle');
    expect(ran).not.toContain('delete_entries');
  });

  test('the provider is offered the delete capability and never sees a file name', async () => {
    await askToDelete();
    await expect(chat(page).getByTestId('delete-warning')).toBeVisible();
    const plans = await page.evaluate(() => (window as unknown as { __plans: unknown[] }).__plans);
    expect(plans).toHaveLength(1);
    const sent = JSON.stringify(plans);
    expect(sent).toContain('items.delete');
    for (const leak of ['README', 'notes.txt', 'Users']) {
      expect(sent, leak).not.toContain(leak);
    }
  });

  test('FER-577 B4 keyboard: access letters reach the composer, none approves the delete; Esc returns to the opener', async () => {
    await askToDelete();
    await expect(chat(page).getByTestId('delete-warning')).toBeVisible();
    const input = chat(page).getByRole('textbox', { name: 'Message to AI assistant' });
    await page.keyboard.press('Alt+m');
    await expect(input).toBeFocused();
    // Every other letter and digit, except the plan's own Cancel (l) and Close (c).
    for (const k of 'abdefghijknopqrstuvwxyz0123456789') await page.keyboard.press(`Alt+${k}`);
    await expect(chat(page).getByTestId('delete-warning')).toBeVisible();
    const confirm = chat(page).getByTestId('confirm-delete');
    await expect(confirm).toBeEnabled();
    await expect(confirm).not.toHaveAttribute('aria-keyshortcuts', /.+/);
    const ran = await calls(page);
    expect(ran).not.toContain('ai_authorize_execute');
    expect(ran).not.toContain('ai_delete_to_recycle');
    expect(ran).not.toContain('delete_entries');
    await page.keyboard.press('Escape');
    await expect(chat(page)).toHaveCount(0);
    await expect(page.getByRole('button', { name: 'AI assistant', exact: true })).toBeFocused();
  });
});
