import { test, expect, type Locator, type Page } from '@playwright/test';

/**
 * FER-128 / FER-130 E2E: Ctrl+B Branch View with mocked Tauri IPC. The mock
 * `branch_start` does not walk a disk: it records the call and the test pushes
 * `branch://progress` events through the registered listener, so progressive
 * results, Esc / Cancel button, the item-limit message, and F5 copy of a
 * selection (real per-item paths, duplicate basenames) run in the real
 * frontend. The Rust walker is covered by `branch_view::tests`.
 */

const HOME = 'C:\\Users\\demo';

interface Entry {
  name: string;
  path: string;
  isDir: boolean;
  isSymlink: boolean;
  size: number;
  modifiedMs: number;
  hidden: boolean;
}

const file = (name: string, dir: string): Entry => ({
  name,
  path: `${dir}\\${name}`,
  isDir: false,
  isSymlink: false,
  size: 10,
  modifiedMs: 1700000000000,
  hidden: false,
});

const summary = (over: Record<string, unknown> = {}) => ({
  total: 0,
  truncated: false,
  cancelled: false,
  failedCount: 0,
  durationMs: 5,
  ...over,
});

function injectTauriMock(page: Page) {
  page.addInitScript((home) => {
    const w = window as unknown as Record<string, unknown>;
    const handlers: number[] = [];
    w.__calls = [] as { cmd: string; args: unknown }[];
    w.__emitBranch = (payload: unknown) => {
      for (const h of handlers) {
        const cb = w[`_${h}`] as ((e: unknown) => void) | undefined;
        cb?.({ event: 'branch://progress', id: 1, payload });
      }
    };
    const entry = (name: string) => ({
      name,
      path: `${home}\\${name}`,
      isDir: false,
      isSymlink: false,
      size: 10,
      modifiedMs: 1700000000000,
      hidden: false,
    });
    (w as { __TAURI_INTERNALS__: unknown }).__TAURI_INTERNALS__ = {
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
      invoke: (cmd: string, args: unknown) => {
        (w.__calls as unknown[]).push({ cmd, args });
        if (cmd === 'plugin:path|resolve_directory') return Promise.resolve(home);
        if (cmd === 'plugin:event|listen') {
          const a = args as { event?: string; handler?: number };
          if (a.event === 'branch://progress' && a.handler !== undefined) handlers.push(a.handler);
          return Promise.resolve(Math.floor(Math.random() * 1e9));
        }
        if (cmd === 'plugin:event|unlisten') return Promise.resolve(null);
        if (cmd === 'watch_start' || cmd === 'watch_stop') return Promise.resolve(1);
        if (cmd === 'list_dir') return Promise.resolve([entry('top.txt')]);
        if (cmd === 'list_drives') return Promise.resolve([]);
        if (cmd === 'load_prefs') return Promise.resolve({ theme: 'dark', paneSplit: 0.5 });
        if (cmd === 'save_prefs') return Promise.resolve(null);
        if (cmd === 'branch_start') return Promise.resolve(null);
        if (cmd === 'branch_cancel') return Promise.resolve(true);
        if (cmd === 'plan_copy') {
          return Promise.resolve({ totalFiles: 1, totalBytes: 1, conflicts: [] });
        }
        return Promise.reject(new Error(`Unknown command: ${cmd}`));
      },
    };
  }, HOME);
}

test.describe('FER-128/130 Branch View streaming E2E (mock IPC)', () => {
  let page: Page;
  const left = () => page.getByRole('region', { name: 'Left pane' });
  const rowByPath = (pane: () => Locator, path: string) =>
    pane().locator(`[data-path="${path.split('\\').join('\\\\')}"]`);

  const calls = (cmd: string) =>
    page.evaluate(
      (c) =>
        ((window as unknown as { __calls: { cmd: string; args: unknown }[] }).__calls ?? [])
          .filter((x) => x.cmd === c)
          .map((x) => x.args),
      cmd,
    );

  const emit = (payload: unknown) =>
    page.evaluate(
      (p) => (window as unknown as { __emitBranch: (x: unknown) => void }).__emitBranch(p),
      payload,
    );

  /** Ctrl+B on the left pane and return the walk id the frontend generated. */
  async function startBranch(): Promise<string> {
    const before = (await calls('branch_start')).length;
    await rowByPath(left, `${HOME}\\top.txt`).click();
    await page.keyboard.press('Control+b');
    await expect.poll(async () => (await calls('branch_start')).length).toBe(before + 1);
    const all = (await calls('branch_start')) as { id: string; root: string }[];
    expect(all[all.length - 1].root).toBe(HOME);
    return all[all.length - 1].id;
  }

  test.beforeEach(async ({ browser }) => {
    page = await browser.newPage();
    await page.setViewportSize({ width: 1280, height: 600 });
    injectTauriMock(page);
    await page.goto('/');
    await page.waitForSelector('[data-path]', { state: 'attached', timeout: 10_000 });
  });

  test.afterEach(async () => {
    await page.close();
  });

  test('rows appear progressively while loading, and Cancel disappears when done', async () => {
    const id = await startBranch();
    await expect(left().locator('[aria-label="File list"]')).toContainText('Loading');
    await emit({ id, entries: [file('a.txt', `${HOME}\\x`)], done: false });
    await expect(rowByPath(left, `${HOME}\\x\\a.txt`)).toBeVisible();
    await expect(left().getByRole('button', { name: 'Cancel' })).toBeVisible();
    await expect(left().locator('.status')).toContainText('loading');
    await emit({ id, entries: [file('b.txt', `${HOME}\\y`)], done: false });
    await expect(rowByPath(left, `${HOME}\\y\\b.txt`)).toBeVisible({ timeout: 3_000 });
    await emit({ id, entries: [], done: true, summary: summary({ total: 2 }) });
    await expect(left().getByRole('button', { name: 'Cancel' })).toHaveCount(0);
    await expect(left().locator('.status')).not.toContainText('loading');
    expect(await calls('branch_cancel')).toHaveLength(0);
  });

  test('Esc stops a loading walk: backend cancel, rows kept, marked partial', async () => {
    const id = await startBranch();
    await emit({ id, entries: [file('a.txt', `${HOME}\\x`)], done: false });
    const row = rowByPath(left, `${HOME}\\x\\a.txt`);
    await expect(row).toBeVisible();
    await row.click(); // keyboard focus in the list
    await page.keyboard.press('Escape');
    await expect.poll(async () => (await calls('branch_cancel')).length).toBe(1);
    expect(((await calls('branch_cancel')) as { id: string }[])[0].id).toBe(id);
    await expect(left().getByRole('button', { name: 'Cancel' })).toHaveCount(0);
    await expect(row).toBeVisible(); // partial rows stay
    await expect(left().locator('.status')).toContainText('cancelled');
    // A late batch of the cancelled walk is ignored.
    await emit({ id, entries: [file('late.txt', `${HOME}\\x`)], done: false });
    await expect(rowByPath(left, `${HOME}\\x\\late.txt`)).toHaveCount(0);
  });

  test('the Cancel button stops the walk too', async () => {
    const id = await startBranch();
    await emit({ id, entries: [file('a.txt', `${HOME}\\x`)], done: false });
    await left().getByRole('button', { name: 'Cancel' }).click();
    await expect.poll(async () => (await calls('branch_cancel')).length).toBe(1);
    await expect(left().locator('.status')).toContainText('cancelled');
  });

  test('reaching the item limit shows a message', async () => {
    const id = await startBranch();
    await emit({ id, entries: [file('a.txt', `${HOME}\\x`)], done: false });
    await emit({ id, entries: [], done: true, summary: summary({ total: 1, truncated: true }) });
    await expect(left().locator('.status')).toContainText('limit of 100,000 items reached');
  });

  test('F5 copies the marked branch items using their REAL paths (duplicate basenames)', async () => {
    const id = await startBranch();
    const a1 = `${HOME}\\x\\same.txt`;
    const a2 = `${HOME}\\y\\deep\\same.txt`;
    await emit({
      id,
      entries: [file('same.txt', `${HOME}\\x`), file('same.txt', `${HOME}\\y\\deep`)],
      done: true,
      summary: summary({ total: 2 }),
    });
    await expect(rowByPath(left, a1)).toBeVisible();
    await expect(rowByPath(left, a2)).toBeVisible();
    await rowByPath(left, a1).click();
    await page.keyboard.press('Space'); // marks a1, cursor steps to a2
    await page.keyboard.press('Space'); // marks a2
    await expect(left().locator('.row.selected')).toHaveCount(2);
    await page.keyboard.press('F5');
    await expect.poll(async () => (await calls('plan_copy')).length).toBe(1);
    const plan = ((await calls('plan_copy')) as { sources: string[] }[])[0];
    expect([...plan.sources].sort()).toEqual([a1, a2].sort());
  });
});
