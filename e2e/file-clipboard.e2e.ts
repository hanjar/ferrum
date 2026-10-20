import { test, expect, type Page } from '@playwright/test';

/**
 * FER-572 E2E: Ctrl+C / Ctrl+X / Ctrl+V move FILES through the Windows clipboard commands.
 * The Rust side (CF_HDROP codec, validation) is covered by cargo tests; here Tauri IPC is mocked
 * with a tiny in-memory filesystem and every call is recorded in `window.__calls`.
 */

function injectTauriMock(page: Page, pasteResult: unknown) {
  page.addInitScript((paste) => {
    const w = window as unknown as {
      __calls: { cmd: string; args: unknown }[];
      __TAURI_INTERNALS__: unknown;
    };
    w.__calls = [];
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
      [home]: [file('invoice', home, true), file('notes.txt', home), file('zeta.txt', home)],
    };
    w.__TAURI_INTERNALS__ = {
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
      invoke: (cmd: string, args: unknown) => {
        w.__calls.push({ cmd, args });
        if (cmd === 'plugin:path|resolve_directory') return Promise.resolve(home);
        if (cmd === 'plugin:event|listen') return Promise.resolve(Math.floor(Math.random() * 1e9));
        if (cmd === 'watch_start' || cmd === 'watch_stop') return Promise.resolve(1);
        if (cmd === 'list_dir') {
          const raw = String((args as { path: string }).path).replace(/\\$/, '');
          const p = raw === 'C:\\Projects' || raw === 'D:\\Backup' ? home : raw;
          return tree[p]
            ? Promise.resolve(tree[p])
            : Promise.reject({ code: 'notFound', message: p });
        }
        if (cmd === 'list_drives') {
          return Promise.resolve([
            { label: 'System', path: 'C:\\', freeBytes: 1e9, totalBytes: 5e11 },
          ]);
        }
        if (cmd === 'clipboard_copy_files') return Promise.resolve(11);
        if (cmd === 'clipboard_read_files') return Promise.resolve(paste);
        if (cmd === 'plan_copy') {
          return Promise.resolve({ totalFiles: 1, totalBytes: 10, conflicts: [] });
        }
        if (cmd === 'enqueue_copy' || cmd === 'enqueue_move') return Promise.resolve('op-1');
        return Promise.reject(new Error(`Unknown command: ${cmd}`));
      },
    };
  }, pasteResult);
}

async function open(page: Page, pasteResult: unknown) {
  injectTauriMock(page, pasteResult);
  await page.goto('/');
  await expect(
    page.getByRole('region', { name: 'Left pane' }).locator('[data-path$="\\\\notes.txt"]'),
  ).toBeVisible();
}

const calls = (page: Page, cmd: string) =>
  page.evaluate(
    (c) =>
      (window as unknown as { __calls: { cmd: string; args: unknown }[] }).__calls
        .filter((x) => x.cmd === c)
        .map((x) => x.args),
    cmd,
  );

test.describe('File clipboard (FER-572)', () => {
  let page: Page;
  const row = (name: string) =>
    page.getByRole('region', { name: 'Left pane' }).locator(`[data-path$="\\\\${name}"]`);

  test.afterEach(async () => {
    await page.close();
  });

  test('Ctrl+C puts the selected file on the clipboard as a copy', async ({ browser }) => {
    page = await browser.newPage();
    await open(page, null);
    await row('notes.txt').click();
    await page.keyboard.press('Control+c');
    await expect.poll(() => calls(page, 'clipboard_copy_files')).toHaveLength(1);
    expect(await calls(page, 'clipboard_copy_files')).toEqual([
      { paths: ['C:\\Users\\demo\\notes.txt'], cut: false },
    ]);
  });

  test('Ctrl+X puts the selected files on the clipboard as a cut', async ({ browser }) => {
    page = await browser.newPage();
    await open(page, null);
    await row('notes.txt').click({ modifiers: ['Control'] });
    await row('zeta.txt').click({ modifiers: ['Control'] });
    await page.keyboard.press('Control+x');
    await expect.poll(() => calls(page, 'clipboard_copy_files')).toHaveLength(1);
    expect(await calls(page, 'clipboard_copy_files')).toEqual([
      { paths: ['C:\\Users\\demo\\notes.txt', 'C:\\Users\\demo\\zeta.txt'], cut: true },
    ]);
  });

  test('Ctrl+V of an Explorer copy runs the normal copy flow into the active folder', async ({
    browser,
  }) => {
    page = await browser.newPage();
    await open(page, { paths: ['E:\\from\\explorer.txt'], cut: false, sequence: 5 });
    await row('notes.txt').click();
    await page.keyboard.press('Control+v');
    await expect.poll(() => calls(page, 'plan_copy')).toHaveLength(1);
    expect(await calls(page, 'plan_copy')).toEqual([
      { sources: ['E:\\from\\explorer.txt'], targetDir: 'C:\\Users\\demo' },
    ]);
    await page.getByRole('dialog').getByRole('button', { name: 'Copy', exact: true }).click();
    await expect.poll(() => calls(page, 'enqueue_copy')).toHaveLength(1);
    expect(await calls(page, 'enqueue_move')).toHaveLength(0);
  });

  test('Ctrl+V of an Explorer cut runs the normal move flow', async ({ browser }) => {
    page = await browser.newPage();
    await open(page, { paths: ['E:\\from\\explorer.txt'], cut: true, sequence: 6 });
    await row('notes.txt').click();
    await page.keyboard.press('Control+v');
    await expect.poll(() => calls(page, 'plan_copy')).toHaveLength(1);
    await page.getByRole('dialog').getByRole('button', { name: 'Move', exact: true }).click();
    await expect.poll(() => calls(page, 'enqueue_move')).toHaveLength(1);
    expect(await calls(page, 'enqueue_copy')).toHaveLength(0);
  });

  test('Ctrl+V with no files on the clipboard starts nothing', async ({ browser }) => {
    page = await browser.newPage();
    await open(page, null);
    await row('notes.txt').click();
    await page.keyboard.press('Control+v');
    await expect.poll(() => calls(page, 'clipboard_read_files')).toHaveLength(1);
    expect(await calls(page, 'plan_copy')).toHaveLength(0);
    await expect(page.getByRole('dialog')).toHaveCount(0);
  });
});
