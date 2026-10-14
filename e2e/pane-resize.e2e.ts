import { test, expect, type Page } from '@playwright/test';

/**
 * FER-406 E2E: the dual-pane divider resizes both panes by pointer and
 * keyboard, and the ratio survives an app restart. Mocks Tauri IPC (same
 * pattern as archive-navigation.e2e.ts). load_prefs/save_prefs are backed by
 * localStorage so `page.reload()` proves the full persistence round-trip
 * (debounced save_prefs → load_prefs on boot) at the frontend layer — the
 * real config.json write/validation is covered by the Rust suite (prefs.rs).
 */

const PREFS_KEY = 'fer-prefs-e2e-mock';

function injectTauriMock(page: Page) {
  page.addInitScript((key: string) => {
    const DEFAULT_PREFS = {
      theme: 'dark',
      palette: 'violet',
      showHidden: false,
      terminal: 'auto',
      paneSplit: 0.5,
    };

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
        if (cmd === 'plugin:event|listen') return Promise.resolve(Math.floor(Math.random() * 1e9));
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
            {
              name: 'notes.txt',
              path: 'C:\\Users\\demo\\notes.txt',
              isDir: false,
              isSymlink: false,
              size: 5,
              modifiedMs: 1700000000000,
              hidden: false,
            },
            {
              name: 'Archive',
              path: 'C:\\Users\\demo\\Archive',
              isDir: true,
              isSymlink: false,
              size: 0,
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
        if (cmd === 'load_prefs') {
          const raw = window.localStorage.getItem(key);
          return Promise.resolve(raw === null ? DEFAULT_PREFS : JSON.parse(raw));
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

test.describe('FER-406 dual-pane divider E2E (mock IPC)', () => {
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

  const grid = () => page.locator('[data-pane-grid]');
  const divider = () => page.locator('[data-testid="pane-divider"]');
  const pane0 = () => page.locator('[data-pane="0"]');
  const pane1 = () => page.locator('[data-pane="1"]');

  const inlineTemplate = (axis: 'columns' | 'rows') =>
    grid().evaluate(
      (el, ax) => (ax === 'columns' ? el.style.gridTemplateColumns : el.style.gridTemplateRows),
      axis,
    );

  /** Drag the divider so the left/top pane owns `fraction` of the grid. */
  async function dragTo(fraction: number): Promise<number> {
    const [g, d] = await Promise.all([grid().boundingBox(), divider().boundingBox()]);
    if (!g || !d) throw new Error('grid/divider not visible');
    const tx = g.x + g.width * fraction;
    const ty = g.y + g.height * fraction;
    await page.mouse.move(d.x + d.width / 2, d.y + d.height / 2);
    await page.mouse.down();
    await page.mouse.move(tx, ty, { steps: 8 });
    await page.mouse.up();
    return fraction;
  }

  /** Waits for the debounced save and returns the persisted paneSplit. */
  async function savedPaneSplit(): Promise<number> {
    await expect
      .poll(() => page.evaluate((k) => window.localStorage.getItem(k), PREFS_KEY))
      .not.toBeNull();
    const raw = await page.evaluate((k) => window.localStorage.getItem(k), PREFS_KEY);
    return (JSON.parse(raw as string) as { paneSplit: number }).paneSplit;
  }

  test('pointer drag resizes both panes and persists the ratio across reload', async () => {
    await expect(pane0()).toBeVisible();
    await expect(pane1()).toBeVisible();
    await expect(divider()).toBeVisible();
    await expect(divider()).toHaveAttribute('aria-orientation', 'vertical');
    await expect(inlineTemplate('columns')).resolves.toContain('0.5fr');

    await dragTo(0.7);

    const now = await divider().getAttribute('aria-valuenow');
    expect(Number(now)).toBeGreaterThan(50);
    expect(Number(now)).toBeLessThanOrEqual(80);
    await expect(inlineTemplate('columns')).resolves.not.toContain('0.5fr');
    // Both panes still render; the left one is now the wider one.
    const [w0, w1] = await Promise.all([
      pane0().evaluate((el) => el.getBoundingClientRect().width),
      pane1().evaluate((el) => el.getBoundingClientRect().width),
    ]);
    expect(w0).toBeGreaterThan(w1);

    // Restart proof: reload, the mock load_prefs serves what save_prefs wrote.
    const saved = await savedPaneSplit();
    await page.reload();
    await page.waitForSelector('[data-path]', { state: 'attached', timeout: 10_000 });
    await expect(divider()).toHaveAttribute('aria-valuenow', String(Math.round(saved * 100)));
    // The browser serializes the inline template ("0" → "0px", float noise
    // rounded), so compare the leading fr value with a tolerance instead.
    const tpl = await inlineTemplate('columns');
    const leftFr = Number(/,\s*([\d.]+)fr/.exec(tpl)?.[1]);
    expect(Math.abs(leftFr - saved)).toBeLessThan(0.005);
    await expect(pane0()).toBeVisible();
    await expect(pane1()).toBeVisible();
  });

  test('keyboard adjusts the ratio in 2% steps and clamps to [20, 80]', async () => {
    await divider().focus();
    await page.keyboard.press('End');
    await expect(divider()).toHaveAttribute('aria-valuenow', '80');
    await page.keyboard.press('Home');
    await expect(divider()).toHaveAttribute('aria-valuenow', '20');
    await page.keyboard.press('ArrowRight');
    await expect(divider()).toHaveAttribute('aria-valuenow', '22');
    await expect(divider()).toHaveAttribute('aria-valuetext', '22% / 78%');
  });

  test('double-click resets to 50/50', async () => {
    await dragTo(0.7);
    await divider().dblclick();
    await expect(divider()).toHaveAttribute('aria-valuenow', '50');
    await expect(inlineTemplate('columns')).resolves.toContain('0.5fr');
  });

  test('vertical arrangement flips the divider to a horizontal row resizer', async () => {
    await page.locator('button[data-menu-trigger]', { hasText: 'View' }).click();
    await page.getByRole('menuitem', { name: 'Vertical Arrangement' }).click();

    await expect(divider()).toHaveAttribute('aria-orientation', 'horizontal');
    await expect(inlineTemplate('rows')).resolves.toContain('0.5fr');

    await dragTo(0.3);
    const now = await divider().getAttribute('aria-valuenow');
    expect(Number(now)).toBeLessThan(50);
    await expect(inlineTemplate('rows')).resolves.not.toContain('0.5fr');
    await expect(pane0()).toBeVisible();
    await expect(pane1()).toBeVisible();
  });
});
