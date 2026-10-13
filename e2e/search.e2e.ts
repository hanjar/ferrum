import { test, expect, type Page } from '@playwright/test';

/**
 * FER-101 / FER-99 E2E: Alt+F7 search panel with mocked Tauri IPC (same
 * pattern as pane-resize.e2e.ts). The mock `search_start` does not walk a
 * disk: it records the call and the test pushes `search://progress` events
 * through the registered listener, so progressive results, cancel,
 * permission-error stats (unreadable dirs), query-change races, keyboard
 * navigation, Copy and Feed-to-pane are exercised end to end in the real
 * frontend. The Rust walker itself is covered by `search::tests` (including
 * a real ACL-denied directory). Search INSIDE archives is not supported.
 */

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

function stats(over: Record<string, unknown> = {}) {
  return {
    matched: 1,
    visited: 10,
    errors: 0,
    truncated: false,
    cancelled: false,
    timedOut: false,
    durationMs: 5,
    ...over,
  };
}

function injectTauriMock(page: Page) {
  page.addInitScript(() => {
    const w = window as unknown as Record<string, unknown>;
    let searchHandler: number | null = null;
    w.__calls = [] as { cmd: string; args: unknown }[];
    w.__emitSearch = (payload: unknown) => {
      const cb = w[`_${searchHandler}`] as ((e: unknown) => void) | undefined;
      cb?.({ event: 'search://progress', id: 1, payload });
    };
    const demo = [
      {
        name: 'README.md',
        path: 'C:\\Users\\demo\\README.md',
        isDir: false,
        isSymlink: false,
        size: 1024,
        modifiedMs: 1700000000000,
        hidden: false,
      },
    ];
    w.__TAURI_INTERNALS__ = {
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
        if (cmd === 'plugin:path|resolve_directory') return Promise.resolve('C:\\Users\\demo');
        if (cmd === 'plugin:event|listen') {
          const a = args as { event?: string; handler?: number };
          if (a.event === 'search://progress') searchHandler = a.handler ?? null;
          return Promise.resolve(Math.floor(Math.random() * 1e9));
        }
        if (cmd === 'plugin:event|unlisten') return Promise.resolve(null);
        if (cmd === 'watch_start' || cmd === 'watch_stop') return Promise.resolve(1);
        if (cmd === 'list_dir') return Promise.resolve(demo);
        if (cmd === 'list_drives') return Promise.resolve([]);
        if (cmd === 'load_prefs') return Promise.resolve({ theme: 'dark', paneSplit: 0.5 });
        if (cmd === 'save_prefs') return Promise.resolve(null);
        if (cmd === 'search_start') return Promise.resolve(null);
        if (cmd === 'search_cancel') return Promise.resolve(true);
        return Promise.reject(new Error(`Unknown command: ${cmd}`));
      },
    };
  });
}

test.describe('FER-101 search panel E2E (mock IPC)', () => {
  let page: Page;

  test.beforeEach(async ({ browser }) => {
    const context = await browser.newContext();
    await context.grantPermissions(['clipboard-read', 'clipboard-write']);
    page = await context.newPage();
    injectTauriMock(page);
    await page.goto('/');
    await page.waitForSelector('[data-path]', { state: 'attached', timeout: 10_000 });
    await page.keyboard.press('Alt+F7');
    await expect(page.locator('aside[aria-label="Search"]')).toBeVisible();
  });

  test.afterEach(async () => {
    await page.context().close();
  });

  const calls = (cmd: string) =>
    page.evaluate(
      (c) =>
        ((window as unknown as { __calls: { cmd: string; args: unknown }[] }).__calls ?? [])
          .filter((x) => x.cmd === c)
          .map((x) => x.args),
      cmd,
    );

  /** Run a search and return the id the frontend generated. */
  async function runSearch(pattern: string): Promise<string> {
    const before = (await calls('search_start')).length;
    await page.getByLabel('Search pattern').fill(pattern);
    await page.getByRole('button', { name: 'Search', exact: true }).click();
    await expect.poll(async () => (await calls('search_start')).length).toBe(before + 1);
    const all = (await calls('search_start')) as { id: string }[];
    return all[all.length - 1].id;
  }

  const emit = (payload: unknown) =>
    page.evaluate(
      (p) => (window as unknown as { __emitSearch: (x: unknown) => void }).__emitSearch(p),
      payload,
    );

  test('results appear progressively before the search is done', async () => {
    const id = await runSearch('*.txt');
    await emit({ id, entries: [file('a.txt', 'C:\\x')], done: false });
    await expect(page.locator('button.result')).toHaveCount(1);
    await expect(page.locator('aside[aria-label="Search"] .summary')).toContainText('Searching');
    await emit({ id, entries: [file('b.txt', 'C:\\x')], done: false });
    await expect(page.locator('button.result')).toHaveCount(2);
  });

  test('clicking a result opens its folder in the active pane', async () => {
    const id = await runSearch('*.txt');
    await emit({ id, entries: [file('a.txt', 'C:\\x\\y')], done: true, stats: stats() });
    await page.locator('button.result').first().click();
    await expect
      .poll(async () => JSON.stringify(await calls('list_dir')))
      .toContain('C:\\\\x\\\\y');
  });

  test('keyboard: Down/Up/Home/End move the cursor, Escape closes when idle', async () => {
    const id = await runSearch('*.txt');
    await emit({
      id,
      entries: [file('a.txt', 'C:\\x'), file('b.txt', 'C:\\x'), file('c.txt', 'C:\\x')],
      done: true,
      stats: stats(),
    });
    const list = page.getByRole('listbox', { name: 'Search results' });
    await list.focus();
    const cursorName = () => page.locator('button.result.cursor .name .nt').textContent();
    await page.keyboard.press('ArrowDown');
    expect(await cursorName()).toBe('a.txt');
    await page.keyboard.press('ArrowDown');
    expect(await cursorName()).toBe('b.txt');
    await page.keyboard.press('End');
    expect(await cursorName()).toBe('c.txt');
    await page.keyboard.press('ArrowUp');
    expect(await cursorName()).toBe('b.txt');
    await page.keyboard.press('Home');
    expect(await cursorName()).toBe('a.txt');
    await page.keyboard.press('Escape');
    await expect(page.locator('aside[aria-label="Search"]')).toHaveCount(0);
  });

  test('Escape while running cancels instead of closing', async () => {
    await runSearch('*.txt');
    await page.getByRole('listbox', { name: 'Search results' }).focus();
    await page.keyboard.press('Escape');
    await expect.poll(async () => (await calls('search_cancel')).length).toBeGreaterThan(0);
    await expect(page.locator('aside[aria-label="Search"]')).toBeVisible();
  });

  test('Copy results writes one path per line to the clipboard', async () => {
    const id = await runSearch('*.txt');
    await emit({
      id,
      entries: [file('a.txt', 'C:\\x'), file('b.txt', 'C:\\x\\z')],
      done: true,
      stats: stats(),
    });
    await page.getByRole('button', { name: 'Copy results' }).click();
    await expect(page.getByText('Copied 2 paths')).toBeVisible();
    const text = await page.evaluate(() => navigator.clipboard.readText());
    expect(text.split(/\r?\n/)).toEqual(['C:\\x\\a.txt', 'C:\\x\\z\\b.txt']);
  });

  test('Feed to pane lists the results in the active pane', async () => {
    const id = await runSearch('*.txt');
    await emit({
      id,
      entries: [file('a.txt', 'C:\\far'), file('b.txt', 'C:\\away')],
      done: true,
      stats: stats(),
    });
    await page.getByRole('button', { name: 'Feed to pane' }).click();
    await expect(page.locator('[data-path="C:\\\\far\\\\a.txt"]').first()).toBeAttached();
    await expect(page.getByText('Search results · 2 items')).toBeVisible();
  });

  test('permission-denied directories are reported as unreadable, not as a failure', async () => {
    const id = await runSearch('*.txt');
    await emit({
      id,
      entries: [file('ok.txt', 'C:\\x')],
      done: true,
      stats: stats({ errors: 2 }),
    });
    await expect(page.locator('aside[aria-label="Search"] .summary')).toContainText('2 unreadable');
    await expect(page.locator('aside[aria-label="Search"] .error')).toHaveCount(0);
    await expect(page.locator('button.result')).toHaveCount(1);
  });

  test('events from a superseded search are dropped when the query changes', async () => {
    const first = await runSearch('*.txt');
    await emit({ id: first, entries: [file('old.txt', 'C:\\x')], done: false });
    await expect(page.locator('button.result')).toHaveCount(1);
    // Search is disabled while running: the user stops it, the worker reports
    // the cancel, then a new query starts.
    await page.getByRole('button', { name: 'Stop' }).click();
    await expect.poll(async () => (await calls('search_cancel')).length).toBeGreaterThan(0);
    await emit({ id: first, entries: [], done: true, stats: stats({ cancelled: true }) });
    const second = await runSearch('*.md');
    expect(second).not.toBe(first);
    await emit({ id: first, entries: [file('late.txt', 'C:\\x')], done: true, stats: stats() });
    await emit({ id: second, entries: [file('new.md', 'C:\\x')], done: true, stats: stats() });
    await expect(page.locator('button.result .name')).toHaveText(['new.md']);
  });
});
