import { test, expect, type Page } from '@playwright/test';

/**
 * FER-578 (FER-577 B2) E2E: long archive jobs move to the background. Tauri IPC is mocked; the
 * progress events are emitted by the test. The app must stay usable while the job runs, the dock
 * (Alt+B) must show it, and nothing may pop up in front of the user when it needs input or ends.
 */

type Call = { cmd: string; args: Record<string, unknown> };

function injectTauriMock(page: Page) {
  page.addInitScript(() => {
    type W = {
      __calls: Call[];
      __listeners: Record<string, number[]>;
      __pending: Record<string, { resolve: (v: unknown) => void; reject: (e: unknown) => void }>;
      __emit: (event: string, payload: unknown) => void;
      __finish: (requestId: string, value?: unknown) => void;
      __fail: (requestId: string, error: unknown) => void;
    };
    const w = window as unknown as W & Record<string, unknown>;
    w.__calls = [];
    w.__listeners = {};
    w.__pending = {};
    w.__emit = (event, payload) => {
      for (const id of w.__listeners[event] ?? []) {
        (w[`_${id}`] as (e: unknown) => void)({ event, id, payload });
      }
    };
    w.__finish = (requestId, value) => {
      w.__pending[requestId]?.resolve(value);
      delete w.__pending[requestId];
    };
    w.__fail = (requestId, error) => {
      w.__pending[requestId]?.reject(error);
      delete w.__pending[requestId];
    };
    const pendingJob = (requestId: string) =>
      new Promise((resolve, reject) => {
        w.__pending[requestId] = { resolve, reject };
      });
    const file = (name: string, dir: string, isDir = false) => ({
      name,
      path: dir.endsWith('\\') ? `${dir}${name}` : `${dir}\\${name}`,
      isDir,
      isSymlink: false,
      size: isDir ? 0 : 1024,
      modifiedMs: 1700000000000,
      hidden: false,
    });
    const home = 'C:\\Users\\demo';
    const tree: Record<string, ReturnType<typeof file>[]> = {
      'C:': [file('Users', 'C:\\', true)],
      'C:\\Users': [file('demo', 'C:\\Users', true)],
      [home]: [
        file('docs', home, true),
        file('big.txt', home),
        file('enc.zip', home),
        file('test.zip', home),
      ],
      [`${home}\\docs`]: [file('inner.txt', `${home}\\docs`)],
    };
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
        const a = (args ?? {}) as Record<string, unknown>;
        w.__calls.push({ cmd, args: JSON.parse(JSON.stringify(a)) });
        if (cmd === 'plugin:path|resolve_directory') return Promise.resolve(home);
        if (cmd === 'plugin:event|listen') {
          const event = String(a.event);
          (w.__listeners[event] ??= []).push(Number(a.handler));
          return Promise.resolve(Math.floor(Math.random() * 1e9));
        }
        if (cmd === 'plugin:event|unlisten') return Promise.resolve();
        if (cmd === 'watch_start' || cmd === 'watch_stop') return Promise.resolve(1);
        if (cmd === 'list_operations') return Promise.resolve([]);
        if (cmd === 'list_dir') {
          const raw = String(a.path).replace(/\\$/, '');
          return tree[raw]
            ? Promise.resolve(tree[raw])
            : Promise.reject({ code: 'notFound', message: raw });
        }
        if (cmd === 'list_drives') {
          return Promise.resolve([
            { label: 'System', path: 'C:\\', freeBytes: 1e9, totalBytes: 5e11 },
          ]);
        }
        if (cmd === 'archive_rar_tool_probe') return Promise.resolve({ ok: true, version: 'x' });
        if (cmd === 'archive_create') return pendingJob(String(a.requestId));
        if (cmd === 'archive_extract_preflight') {
          return Promise.resolve({ totalFiles: 1, totalBytes: 1, conflicts: [] });
        }
        if (cmd === 'archive_extract') return pendingJob(String(a.requestId));
        if (cmd === 'archive_test') return pendingJob(String(a.requestId));
        if (cmd === 'archive_cancel_request') {
          w.__fail(String(a.requestId), {
            code: 'cancelled',
            message: 'cancelled',
            archiveCode: 'cancelled',
          });
          return Promise.resolve();
        }
        return Promise.reject(new Error(`Unknown command: ${cmd}`));
      },
    };
  });
}

async function calls(page: Page, cmd: string): Promise<Call[]> {
  const all = await page.evaluate(() => (window as unknown as { __calls: Call[] }).__calls);
  return all.filter((c) => c.cmd === cmd);
}

const emit = (page: Page, event: string, payload: unknown) =>
  page.evaluate(
    ([e, p]) => (window as unknown as { __emit: (e: string, p: unknown) => void }).__emit(e, p),
    [event, payload] as const,
  );
const finish = (page: Page, requestId: string, value?: unknown) =>
  page.evaluate(
    ([id, v]) =>
      (window as unknown as { __finish: (i: string, v: unknown) => void }).__finish(
        id as string,
        v,
      ),
    [requestId, value] as const,
  );
const fail = (page: Page, requestId: string, error: unknown) =>
  page.evaluate(
    ([id, e]) =>
      (window as unknown as { __fail: (i: string, e: unknown) => void }).__fail(id as string, e),
    [requestId, error] as const,
  );

test.describe('background jobs (FER-578)', () => {
  let page: Page;
  const left = () => page.getByRole('region', { name: 'Left pane' });
  const row = (name: string) => left().locator(`[data-path$="${name}"]`);
  const progressDlg = () => page.locator('[role="dialog"][data-kind="archiveProgress"]');

  test.beforeEach(async ({ browser }) => {
    page = await browser.newPage({ viewport: { width: 1600, height: 800 } });
    injectTauriMock(page);
    await page.goto('/');
    await expect(row('big.txt')).toBeVisible();
  });
  test.afterEach(async () => {
    await page.close();
  });

  test('a big pack: Esc sends it to the background, the app stays usable, the dock shows progress, the end only toasts', async () => {
    await row('big.txt').click();
    await page.keyboard.press('Alt+F5');
    const pack = page.getByRole('dialog', { name: 'Pack to archive' });
    await expect(pack).toBeVisible();
    await page.keyboard.press('Alt+p');
    await expect(progressDlg()).toBeVisible();
    await expect.poll(async () => (await calls(page, 'archive_create')).length).toBe(1);
    const requestId = String((await calls(page, 'archive_create'))[0]!.args['requestId']);

    await emit(page, 'archive://progress', {
      requestId,
      processedBytes: 500,
      totalBytes: 1000,
      processedEntries: 1,
      totalEntries: 2,
      currentMember: 'big.txt',
    });
    await expect(progressDlg().getByRole('progressbar')).toHaveAttribute('aria-valuenow', '50');

    // Esc = Background, not cancel.
    await page.keyboard.press('Escape');
    await expect(page.locator('[role="dialog"]')).toHaveCount(0);
    expect(await calls(page, 'archive_cancel_request')).toHaveLength(0);
    await expect(page.getByTestId('toast')).toContainText('latar belakang');
    await expect(page.getByTestId('ops-badge')).toHaveText('1');
    // Focus is back on the file list.
    await expect(left().locator('[data-pane-list]')).toBeFocused();

    // Ferrum is usable: go into a folder with the keyboard while the pack runs.
    await row('docs').click();
    await page.keyboard.press('Enter');
    await expect(row('inner.txt')).toBeVisible();

    // The dock shows the running job and follows its progress.
    await page.keyboard.press('Alt+B');
    const job = page.getByTestId('bg-job');
    await expect(job).toContainText('Pack');
    await expect(job.getByRole('progressbar')).toHaveAttribute('aria-valuenow', '50');
    await emit(page, 'archive://progress', {
      requestId,
      processedBytes: 800,
      totalBytes: 1000,
      processedEntries: 2,
      totalEntries: 2,
    });
    await expect(job.getByRole('progressbar')).toHaveAttribute('aria-valuenow', '80');
    await page.keyboard.press('Escape');
    await expect(job).toHaveCount(0);

    // The pack ends while the user works: a toast, no dialog, no focus theft.
    await row('inner.txt').click();
    await finish(page, requestId, 'C:\\Users\\demo\\big.zip');
    await expect(page.getByTestId('toast')).toContainText('Pack selesai');
    await expect(page.locator('[role="dialog"]')).toHaveCount(0);
    await expect(page.getByTestId('ops-badge')).toHaveCount(0);
    await expect(left().locator('[data-pane-list]')).toBeFocused();

    await page.keyboard.press('Alt+B');
    await expect(page.getByTestId('bg-job')).toHaveAttribute('data-state', 'done');
  });

  test('the Background button works with the mouse and Alt+G, and Batal still cancels', async () => {
    await row('big.txt').click();
    await page.keyboard.press('Alt+F5');
    await page.keyboard.press('Alt+p');
    await expect(progressDlg()).toBeVisible();
    // Initial focus is the safe button.
    await expect(progressDlg().getByRole('button', { name: 'Background' })).toBeFocused();
    await page.keyboard.press('Alt+g');
    await expect(page.locator('[role="dialog"]')).toHaveCount(0);
    expect(await calls(page, 'archive_cancel_request')).toHaveLength(0);
    const requestId = String((await calls(page, 'archive_create'))[0]!.args['requestId']);
    await finish(page, requestId, 'x');
    await expect(page.getByTestId('toast')).toContainText('Pack selesai');

    // A second pack: Batal cancels, nothing is docked.
    await page.keyboard.press('Alt+F5');
    await page.keyboard.press('Alt+p');
    await expect(progressDlg()).toBeVisible();
    await progressDlg().getByRole('button', { name: 'Batal' }).click();
    await expect.poll(async () => (await calls(page, 'archive_cancel_request')).length).toBe(1);
    await expect(progressDlg()).toHaveCount(0);
  });

  test('an extract that needs a password while in the background waits as "needs input" until chosen', async () => {
    await row('enc.zip').click();
    await page.keyboard.press('Alt+F9');
    await expect(progressDlg()).toBeVisible();
    await expect.poll(async () => (await calls(page, 'archive_extract')).length).toBe(1);
    const first = String((await calls(page, 'archive_extract'))[0]!.args['requestId']);
    await page.keyboard.press('Escape');
    await expect(page.locator('[role="dialog"]')).toHaveCount(0);

    // The user keeps working; then the backend asks for a password.
    await row('docs').click();
    await fail(page, first, { code: 'io', message: 'needs one', archiveCode: 'passwordRequired' });
    await expect(page.getByTestId('toast')).toContainText('Password diperlukan');
    await expect(page.getByTestId('toast-action')).toBeVisible();
    await expect(page.locator('[role="dialog"]')).toHaveCount(0);
    await expect(page.getByTestId('ops-badge')).toHaveText('1');
    await expect(left().locator('[data-pane-list]')).toBeFocused();

    // Choose it from the dock with the keyboard.
    await page.keyboard.press('Alt+B');
    const job = page.getByTestId('bg-job');
    await expect(job).toHaveAttribute('data-state', 'needsInput');
    await expect(job).toContainText('Needs your input');
    await expect(job).toBeFocused();
    await page.keyboard.press('Enter');
    const pw = page.locator('[role="dialog"][data-kind="password"]');
    await expect(pw).toBeVisible();
    await expect(pw.locator('input')).toHaveAttribute('type', 'password');
    await page.keyboard.type('hunter2');
    await page.keyboard.press('Enter');

    // The retry runs in the background: no progress dialog comes back.
    await expect.poll(async () => (await calls(page, 'archive_extract')).length).toBe(2);
    const second = (await calls(page, 'archive_extract'))[1]!;
    expect((second.args['request'] as { password?: string }).password).toBe('hunter2');
    await expect(page.locator('[role="dialog"]')).toHaveCount(0);
    const secondId = String(second.args['requestId']);
    await emit(page, 'archive://progress', {
      requestId: secondId,
      processedBytes: 10,
      totalBytes: 10,
      processedEntries: 3,
      totalEntries: 3,
      writtenEntries: 3,
    });
    await finish(page, secondId);
    await expect(page.getByTestId('toast')).toContainText('Ekstrak selesai: 3 ditulis');
    await expect(page.locator('[role="dialog"]')).toHaveCount(0);

    // The result is kept behind "Lihat hasil".
    await page.keyboard.press('Alt+B');
    await page.getByRole('button', { name: 'Lihat hasil' }).click();
    await expect(page.locator('[role="dialog"][data-kind="extractSummary"]')).toContainText('3');
  });

  test('Test Archive in the background: the report waits behind "Lihat hasil"; quitting asks while it runs', async () => {
    await row('test.zip').click();
    await page.keyboard.press('Alt+Shift+F9');
    await expect(progressDlg()).toBeVisible();
    await page.keyboard.press('Escape');
    await expect(page.locator('[role="dialog"]')).toHaveCount(0);

    // Quit is guarded by the running job.
    await page.keyboard.press('Control+k');
    await page.keyboard.type('Quit');
    await page.keyboard.press('Enter');
    const quit = page.locator('[role="dialog"][data-kind="quitConfirm"]');
    await expect(quit).toBeVisible();
    await expect(quit).toContainText('1 operation is');
    await page.keyboard.press('Escape');
    await expect(quit).toHaveCount(0);

    const requestId = String((await calls(page, 'archive_test'))[0]!.args['requestId']);
    await finish(page, requestId, {
      archivePath: 'C:\\Users\\demo\\test.zip',
      entries: [{ memberPath: 'a.txt', status: 'passed' }],
    });
    await expect(page.getByTestId('toast')).toContainText('Archive test passed');
    await expect(page.locator('[role="dialog"]')).toHaveCount(0);
    await page.keyboard.press('Alt+B');
    await page.getByRole('button', { name: 'Lihat hasil' }).click();
    await expect(page.locator('[role="dialog"][data-kind="testReport"]')).toContainText(
      'Archive test passed',
    );
  });
});
