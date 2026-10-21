import { test, expect, type Page } from '@playwright/test';

/**
 * FER-585 (FER-577 B5) E2E: a big Delete is a backend job. Tauri IPC is mocked; `delete_start`
 * returns at once and the test emits `delete://progress` / `delete://done` like the backend does.
 * The confirmation stays in front of the delete; Esc sends the running job to the dock; Cancel
 * asks the backend to stop and the dialog says what is left.
 */

type Call = { cmd: string; args: Record<string, unknown> };

function injectTauriMock(page: Page) {
  page.addInitScript(() => {
    type W = {
      __calls: Call[];
      __listeners: Record<string, number[]>;
      __emit: (event: string, payload: unknown) => void;
    };
    const w = window as unknown as W & Record<string, unknown>;
    w.__calls = [];
    w.__listeners = {};
    w.__emit = (event, payload) => {
      for (const id of w.__listeners[event] ?? []) {
        (w[`_${id}`] as (e: unknown) => void)({ event, id, payload });
      }
    };
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
      [home]: [file('bigfolder', home, true), file('note.txt', home)],
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
        if (cmd === 'delete_start') return Promise.resolve(); // returns at once; the job runs "in Rust"
        if (cmd === 'delete_cancel') return Promise.resolve();
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

test.describe('async delete (FER-585)', () => {
  let page: Page;
  const left = () => page.getByRole('region', { name: 'Left pane' });
  const row = (name: string) => left().locator(`[data-path$="${name}"]`);
  const deleteDlg = () => page.locator('[role="dialog"][data-kind="delete"]');
  const FOLDER = 'C:\\Users\\demo\\bigfolder';

  test.beforeEach(async ({ browser }) => {
    page = await browser.newPage({ viewport: { width: 1600, height: 800 } });
    injectTauriMock(page);
    await page.goto('/');
    await expect(row('bigfolder')).toBeVisible();
  });
  test.afterEach(async () => {
    await page.close();
  });

  async function startBigDelete(): Promise<string> {
    await row('bigfolder').click();
    await page.keyboard.press('F8');
    await expect(deleteDlg()).toBeVisible();
    // Nothing is deleted before the confirmation.
    expect(await calls(page, 'delete_start')).toHaveLength(0);
    await deleteDlg().getByRole('button', { name: 'Delete', exact: true }).click();
    await expect.poll(async () => (await calls(page, 'delete_start')).length).toBe(1);
    const start = (await calls(page, 'delete_start'))[0]!;
    expect(start.args['paths']).toEqual([FOLDER]);
    expect(start.args['permanent']).toBe(false);
    return String(start.args['requestId']);
  }

  test('a big delete: progress appears, Esc sends it to the dock, it ends with a toast and a refresh', async () => {
    const requestId = await startBigDelete();
    await emit(page, 'delete://progress', {
      requestId,
      done: 0,
      total: 1,
      currentName: 'bigfolder',
      entries: 1200,
    });
    // Only a delete that takes a moment shows its progress.
    const bar = deleteDlg().getByTestId('delete-progress');
    await expect(bar).toBeVisible();
    await expect(deleteDlg()).toContainText('1200 entri');
    await expect(deleteDlg()).toContainText('bigfolder');

    const listCalls = (await calls(page, 'list_dir')).length;
    await page.keyboard.press('Escape');
    await expect(page.locator('[role="dialog"]')).toHaveCount(0);
    expect(await calls(page, 'delete_cancel')).toHaveLength(0);
    await expect(page.getByTestId('toast')).toContainText('latar belakang');
    await expect(page.getByTestId('ops-badge')).toHaveText('1');
    await expect(left().locator('[data-pane-list]')).toBeFocused();

    // The dock follows the job.
    await page.keyboard.press('Alt+B');
    const job = page.getByTestId('bg-job');
    await expect(job).toContainText('Delete');
    await emit(page, 'delete://progress', {
      requestId,
      done: 0,
      total: 1,
      currentName: 'bigfolder',
      entries: 5000,
    });
    await expect(job).toContainText('5000 entri');
    await page.keyboard.press('Escape');

    await emit(page, 'delete://done', {
      requestId,
      result: {
        report: { requested: 1, deleted: 1, failed: [] },
        cancelled: false,
        notProcessed: [],
        partial: [],
      },
    });
    await expect(page.getByTestId('toast')).toContainText('Hapus selesai');
    await expect(page.locator('[role="dialog"]')).toHaveCount(0);
    await expect(page.getByTestId('ops-badge')).toHaveCount(0);
    await expect
      .poll(async () => (await calls(page, 'list_dir')).length)
      .toBeGreaterThan(listCalls);
    await page.keyboard.press('Alt+B');
    await expect(page.getByTestId('bg-job')).toHaveAttribute('data-state', 'done');
  });

  test('Cancel in the middle: the backend is asked to stop and the dialog names what is left', async () => {
    const requestId = await startBigDelete();
    await emit(page, 'delete://progress', {
      requestId,
      done: 0,
      total: 1,
      currentName: 'bigfolder',
      entries: 300,
    });
    await expect(deleteDlg().getByTestId('delete-progress')).toBeVisible();
    await deleteDlg().getByRole('button', { name: 'Cancel' }).click();
    await expect.poll(async () => (await calls(page, 'delete_cancel')).length).toBe(1);
    expect((await calls(page, 'delete_cancel'))[0]!.args['requestId']).toBe(requestId);
    await expect(deleteDlg()).toContainText('Membatalkan');

    await emit(page, 'delete://done', {
      requestId,
      result: {
        report: { requested: 1, deleted: 0, failed: [] },
        cancelled: true,
        notProcessed: [],
        partial: [FOLDER],
      },
    });
    await expect(deleteDlg().locator('.error')).toContainText('Cancelled');
    await expect(deleteDlg().locator('.error')).toContainText('only partly deleted');
    await expect(deleteDlg().locator('.error')).toContainText('bigfolder');
    await expect(page.getByTestId('ops-badge')).toHaveCount(0);
  });
});
