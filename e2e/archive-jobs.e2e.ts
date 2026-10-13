import { test, expect, type Page } from '@playwright/test';

/**
 * FER-79 / FER-73 / FER-75 E2E: cancellable extract and Test Archive, the password prompt in
 * Test Archive, and the multipart messages. Tauri IPC is mocked; the UI flow is real.
 */

type Call = { cmd: string; args: Record<string, unknown> };

function injectTauriMock(page: Page) {
  page.addInitScript(() => {
    const w = window as unknown as {
      __calls: { cmd: string; args: Record<string, unknown> }[];
      __pending: Record<string, { reject: (e: unknown) => void }>;
    };
    w.__calls = [];
    w.__pending = {};
    const cancelled = {
      code: 'cancelled',
      message: 'Extraction cancelled',
      archiveCode: 'cancelled',
    };
    const file = (name: string, size = 1024) => ({
      name,
      path: `C:\\fixtures\\${name}`,
      isDir: false,
      isSymlink: false,
      size,
      modifiedMs: 1700000000000,
      hidden: false,
    });
    const listing = (loc: { archivePath: string; memberPath: string }) => ({
      location: loc,
      requestToken: 1,
      entries: loc.memberPath
        ? []
        : [
            {
              memberPath: 'readme.md',
              name: 'readme.md',
              isDir: false,
              size: 10,
              modifiedMs: 1700000000000,
            },
          ],
    });
    const fsErr = (archiveCode: string, message: string) => ({
      code: 'unsupported',
      message,
      archiveCode,
    });
    const pendingJob = (id: string) =>
      new Promise((_resolve, reject) => {
        w.__pending[id] = { reject };
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
        const a = (args ?? {}) as Record<string, unknown>;
        w.__calls.push({ cmd, args: JSON.parse(JSON.stringify(a)) });
        if (cmd === 'plugin:path|resolve_directory') return Promise.resolve('C:\\Users\\demo');
        if (cmd === 'plugin:event|listen') return Promise.resolve(Math.floor(Math.random() * 1e9));
        if (cmd === 'plugin:event|unlisten') return Promise.resolve();
        if (cmd === 'watch_start' || cmd === 'watch_stop') return Promise.resolve(1);
        if (cmd === 'list_drives') {
          return Promise.resolve([
            { label: 'System', path: 'C:\\', freeBytes: 1e9, totalBytes: 5e11 },
          ]);
        }
        if (cmd === 'list_dir') {
          return Promise.resolve([
            file('hang.zip'),
            file('enc.zip'),
            file('x.part1.rar'),
            file('x.part2.rar'),
            file('y.part1.rar'),
          ]);
        }
        const req = (a.request ?? {}) as { archivePath?: string; password?: string | null };
        const archivePath = req.archivePath ?? '';
        if (cmd === 'archive_list') {
          const loc = a.location as { archivePath: string; memberPath: string };
          if (loc.archivePath.endsWith('x.part2.rar')) {
            return Promise.reject(
              fsErr(
                'multipartNotFirst',
                'Ini bagian 2 dari arsip multipart. Buka bagian pertama: x.part1.rar',
              ),
            );
          }
          if (loc.archivePath.endsWith('y.part1.rar')) {
            return Promise.reject(
              fsErr('multipartMissing', 'Bagian arsip multipart hilang: y.part2.rar.'),
            );
          }
          return Promise.resolve(listing(loc));
        }
        if (cmd === 'archive_multipart_check') {
          const p = String(a.path);
          if (p.endsWith('x.part2.rar')) {
            return Promise.resolve({
              kind: 'notFirst',
              part: 2,
              firstName: 'x.part1.rar',
              firstPath: 'C:\\fixtures\\x.part1.rar',
              firstExists: true,
            });
          }
          if (p.endsWith('y.part1.rar')) {
            return Promise.resolve({ kind: 'missing', missing: ['y.part2.rar'] });
          }
          return Promise.resolve({ kind: 'single' });
        }
        if (cmd === 'archive_extract_preflight') {
          if (archivePath.endsWith('x.part2.rar')) {
            return Promise.reject(
              fsErr(
                'multipartNotFirst',
                'Ini bagian 2 dari arsip multipart. Buka bagian pertama: x.part1.rar',
              ),
            );
          }
          if (archivePath.endsWith('y.part1.rar')) {
            return Promise.reject(
              fsErr('multipartMissing', 'Bagian arsip multipart hilang: y.part2.rar.'),
            );
          }
          return Promise.resolve({ totalFiles: 1, totalBytes: 1, conflicts: [] });
        }
        if (cmd === 'archive_extract') {
          if (archivePath.endsWith('hang.zip')) return pendingJob(String(a.requestId));
          return Promise.resolve();
        }
        if (cmd === 'archive_test') {
          if (archivePath.endsWith('hang.zip')) return pendingJob(String(a.requestId));
          if (archivePath.endsWith('enc.zip')) {
            const pw = req.password;
            const entry = (status: string, code?: string) => ({
              memberPath: 'secret.txt',
              status,
              ...(code ? { error: { code, message: 'encrypted' } } : {}),
            });
            const entries = !pw
              ? [entry('failed', 'passwordRequired')]
              : pw === 'right'
                ? [entry('passed')]
                : [entry('failed', 'badPassword')];
            return Promise.resolve({ archivePath, entries });
          }
          return Promise.resolve({
            archivePath,
            entries: [{ memberPath: 'a.txt', status: 'passed' }],
          });
        }
        if (cmd === 'archive_cancel_request') {
          const id = String(a.requestId);
          w.__pending[id]?.reject(cancelled);
          delete w.__pending[id];
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

test.describe('archive jobs: cancel, password, multipart', () => {
  let page: Page;

  test.beforeEach(async ({ browser }) => {
    page = await browser.newPage();
    injectTauriMock(page);
    await page.goto('/');
    await page.waitForSelector('[data-path]', { state: 'attached', timeout: 10_000 });
  });

  test.afterEach(async () => {
    await page.close();
  });

  const left = () => page.getByRole('region', { name: 'Left pane' });
  const row = (name: string) => left().locator(`[data-path$="${name}"]`);

  test('Batal on a running extract cancels it by its request id and reports the cancel', async () => {
    await row('hang.zip').click();
    await page.keyboard.press('Alt+F9');
    const progress = page.getByRole('dialog', { name: 'archiveProgress' });
    await expect(progress).toBeVisible();
    await expect(progress).toContainText('hang.zip');
    await expect.poll(async () => (await calls(page, 'archive_extract')).length).toBe(1);
    const requestId = (await calls(page, 'archive_extract'))[0]!.args['requestId'];
    expect(typeof requestId).toBe('string');

    await progress.getByRole('button', { name: 'Batal' }).click();
    await expect.poll(async () => (await calls(page, 'archive_cancel_request')).length).toBe(1);
    expect((await calls(page, 'archive_cancel_request'))[0]!.args['requestId']).toBe(requestId);

    const done = page.getByRole('dialog', { name: 'error' });
    await expect(done).toBeVisible();
    await expect(done).toContainText('Dibatalkan');
    await expect(progress).toBeHidden();
  });

  test('Esc on a running extract cancels it too', async () => {
    await row('hang.zip').click();
    await page.keyboard.press('Alt+F9');
    await expect(page.getByRole('dialog', { name: 'archiveProgress' })).toBeVisible();
    await page.keyboard.press('Escape');
    await expect.poll(async () => (await calls(page, 'archive_cancel_request')).length).toBe(1);
    const requestId = (await calls(page, 'archive_extract'))[0]!.args['requestId'];
    expect((await calls(page, 'archive_cancel_request'))[0]!.args['requestId']).toBe(requestId);
    await expect(page.getByRole('dialog', { name: 'error' })).toContainText('Dibatalkan');
  });

  test('Test Archive: Batal cancels with the test request id and the report says dibatalkan', async () => {
    await row('hang.zip').click();
    await page.keyboard.press('Alt+Shift+F9');
    const progress = page.getByRole('dialog', { name: 'archiveProgress' });
    await expect(progress).toBeVisible();
    await expect.poll(async () => (await calls(page, 'archive_test')).length).toBe(1);
    const requestId = (await calls(page, 'archive_test'))[0]!.args['requestId'];
    await progress.getByRole('button', { name: 'Batal' }).click();
    await expect.poll(async () => (await calls(page, 'archive_cancel_request')).length).toBe(1);
    expect((await calls(page, 'archive_cancel_request'))[0]!.args['requestId']).toBe(requestId);
    const report = page.getByRole('dialog', { name: 'testReport' });
    await expect(report).toBeVisible();
    await expect(report).toContainText('Dibatalkan');
  });

  test('Test Archive: Esc cancels the test', async () => {
    await row('hang.zip').click();
    await page.keyboard.press('Alt+Shift+F9');
    await expect(page.getByRole('dialog', { name: 'archiveProgress' })).toBeVisible();
    await page.keyboard.press('Escape');
    await expect.poll(async () => (await calls(page, 'archive_cancel_request')).length).toBe(1);
    await expect(page.getByRole('dialog', { name: 'testReport' })).toContainText('Dibatalkan');
  });

  test('Test Archive on an encrypted archive asks for a masked password, retries on a wrong one', async () => {
    await row('enc.zip').click();
    await page.keyboard.press('Alt+Shift+F9');
    const pw = page.getByRole('dialog', { name: 'password' });
    await expect(pw).toBeVisible();
    const input = pw.locator('input');
    await expect(input).toHaveAttribute('type', 'password');
    await page.keyboard.type('wrong');
    await page.keyboard.press('Enter');

    // Asked again, with the wrong-password message and an empty field.
    await expect(pw).toContainText('Password salah');
    await expect(input).toHaveValue('');
    await page.keyboard.type('right');
    await page.keyboard.press('Enter');

    const report = page.getByRole('dialog', { name: 'testReport' });
    await expect(report).toBeVisible();
    await expect(report).toContainText('Archive test passed');
    const tests = await calls(page, 'archive_test');
    expect(tests.map((c) => (c.args['request'] as { password: string | null }).password)).toEqual([
      null,
      'wrong',
      'right',
    ]);
    // Each attempt is its own cancellable job.
    expect(new Set(tests.map((c) => c.args['requestId'])).size).toBe(3);
    // The password is neither in the report nor anywhere in the page text.
    await expect(page.locator('body')).not.toContainText('right');
  });

  test('part 2 of a multipart archive explains itself and opens the first part', async () => {
    await row('x.part2.rar').click();
    await page.keyboard.press('Alt+F9');
    const dlg = page.getByRole('dialog', { name: 'multipart' });
    await expect(dlg).toBeVisible();
    await expect(dlg).toContainText('Ini bagian 2 dari arsip multipart');
    await expect(dlg).toContainText('x.part1.rar');
    expect(await calls(page, 'archive_extract')).toHaveLength(0);

    await dlg.getByRole('button', { name: 'Buka bagian pertama' }).click();
    await expect(dlg).toBeHidden();
    await expect
      .poll(async () =>
        (await calls(page, 'archive_list')).some((c) =>
          String((c.args['location'] as { archivePath: string }).archivePath).endsWith(
            'x.part1.rar',
          ),
        ),
      )
      .toBe(true);
  });

  test('opening part 2 (Enter) shows the same explanation instead of a bare error', async () => {
    await row('x.part2.rar').click();
    await page.keyboard.press('Enter');
    const dlg = page.getByRole('dialog', { name: 'multipart' });
    await expect(dlg).toBeVisible();
    await expect(dlg).toContainText('Ini bagian 2 dari arsip multipart');
  });

  test('a first part with a missing volume names it and nothing is extracted or tested', async () => {
    await row('y.part1.rar').click();
    await page.keyboard.press('Alt+F9');
    const dlg = page.getByRole('dialog', { name: 'multipart' });
    await expect(dlg).toBeVisible();
    await expect(dlg).toContainText('Bagian arsip multipart hilang');
    await expect(dlg).toContainText('y.part2.rar');
    await expect(dlg.getByRole('button', { name: 'Buka bagian pertama' })).toHaveCount(0);
    expect(await calls(page, 'archive_extract')).toHaveLength(0);
    await page.keyboard.press('Escape');
    await expect(dlg).toBeHidden();
  });
});
