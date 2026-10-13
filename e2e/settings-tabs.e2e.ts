import { test, expect, type Page } from '@playwright/test';

/**
 * FER-395 E2E: the Settings > Tabs section drives real behavior — tab
 * position (Ctrl+T), middle-click close, tab width class, and per-pane
 * session restore — and every toggle survives page.reload() through the
 * localStorage-backed load_prefs/save_prefs mock (same pattern as
 * pane-resize.e2e.ts). The real config.json write/validation is covered by
 * the Rust suite (prefs.rs). Browser-mode limits: Ctrl+T and Ctrl+, are
 * delivered as page key events; no narrow-window layout assertions here.
 */

const PREFS_KEY = 'fer-prefs-e2e-mock';

const TAB_DEFAULTS = {
  openNextToCurrent: true,
  restoreTabs: true,
  tabWidth: 'fit',
  closeOnMiddleClick: true,
};

function injectTauriMock(page: Page) {
  page.addInitScript(
    ({ key, tabs }: { key: string; tabs: typeof TAB_DEFAULTS }) => {
      const DEFAULT_PREFS = {
        theme: 'dark',
        palette: 'violet',
        showHidden: false,
        terminal: 'auto',
        paneSplit: 0.5,
        tabs,
        session: null,
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
    },
    { key: PREFS_KEY, tabs: TAB_DEFAULTS },
  );
}

interface SavedPrefs {
  tabs?: Partial<typeof TAB_DEFAULTS>;
  session?: { panes: { tabs: string[]; current: number }[] } | null;
}

async function savedPrefs(page: Page): Promise<SavedPrefs> {
  const raw = await page.evaluate((k) => window.localStorage.getItem(k), PREFS_KEY);
  return raw === null ? {} : (JSON.parse(raw) as SavedPrefs);
}

test.describe('FER-395 settings tabs E2E (mock IPC)', () => {
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

  const pane0 = () => page.locator('[data-pane="0"]');
  const tabsOf = () => pane0().locator('.tabs [role="tab"]');
  const dialog = () => page.getByRole('dialog', { name: 'Settings' });

  async function openSettings() {
    await page.keyboard.press('Control+,');
    await expect(dialog()).toBeVisible();
  }

  async function closeSettings() {
    await dialog().press('Escape');
    await expect(dialog()).toBeHidden();
  }

  const checkbox = (label: string) => page.getByRole('switch', { name: label });

  /** Waits until the debounced save wrote `expectFn` as true. */
  async function savedTabs(expectFn: (t: NonNullable<SavedPrefs['tabs']>) => boolean) {
    await expect
      .poll(async () => {
        const p = await savedPrefs(page);
        return p.tabs !== undefined && expectFn(p.tabs);
      })
      .toBe(true);
  }

  test('toggles and width choice persist across reload', async () => {
    await openSettings();
    await checkbox('Restore tabs on launch').uncheck();
    await checkbox('Open new tabs next to current').uncheck();
    await page.getByRole('radio', { name: 'Fixed' }).click();
    await savedTabs(
      (t) => t.restoreTabs === false && t.openNextToCurrent === false && t.tabWidth === 'fixed',
    );
    await closeSettings();

    // Restart proof: reload, the mock load_prefs serves what save_prefs wrote.
    await page.reload();
    await page.waitForSelector('[data-pane="0"] .row[data-path]', { state: 'attached' });
    await openSettings();
    await expect(checkbox('Restore tabs on launch')).not.toBeChecked();
    await expect(checkbox('Open new tabs next to current')).not.toBeChecked();
    await expect(checkbox('Close tab with middle click')).toBeChecked();
    await expect(page.getByRole('radio', { name: 'Fixed' })).toHaveAttribute(
      'aria-checked',
      'true',
    );
  });

  test('Ctrl+T respects "open next to current" ON and OFF', async () => {
    // A Ctrl+T / new-tab-button tab opens the real home (homeDir(), the same
    // one the boot tab uses) — never the browser-dev mock home C:\Projects.
    // ON (default): make tab 0 distinguishable, then insert after it.
    await pane0().locator('.row', { hasText: 'Archive' }).dblclick();
    await expect(tabsOf().nth(0)).toHaveText(/Archive/);
    await page.keyboard.press('Control+t');
    await expect(tabsOf()).toHaveCount(2);
    await expect(tabsOf().nth(0)).toHaveText(/Archive/);
    await expect(tabsOf().nth(1)).toHaveText(/demo/);
    await expect(tabsOf().nth(1)).toHaveAttribute('aria-selected', 'true');

    // OFF: the next tab appends at the end instead.
    await openSettings();
    await checkbox('Open new tabs next to current').uncheck();
    await savedTabs((t) => t.openNextToCurrent === false);
    await closeSettings();

    await tabsOf().nth(0).click(); // active tab is now the FIRST of three
    await page.keyboard.press('Control+t');
    await expect(tabsOf()).toHaveCount(3);
    await expect(tabsOf().nth(0)).toHaveText(/Archive/);
    await expect(tabsOf().nth(2)).toHaveAttribute('aria-selected', 'true');
  });

  test('middle-click closes a tab when enabled, not when disabled', async () => {
    await page.keyboard.press('Control+t');
    await expect(tabsOf()).toHaveCount(2);

    // ON (default): middle click on the first (inactive) tab closes it.
    await tabsOf().nth(0).click({ button: 'middle' });
    await expect(tabsOf()).toHaveCount(1);

    // OFF: middle click leaves the tab strip alone.
    await openSettings();
    await checkbox('Close tab with middle click').uncheck();
    await savedTabs((t) => t.closeOnMiddleClick === false);
    await closeSettings();

    await page.keyboard.press('Control+t');
    await expect(tabsOf()).toHaveCount(2);
    await tabsOf().nth(0).click({ button: 'middle' });
    await expect(tabsOf()).toHaveCount(2);
  });

  test('fixed tab width binds the strip class and persists', async () => {
    await expect(pane0().locator('.tabs')).not.toHaveClass(/fixed/);
    await openSettings();
    await page.getByRole('radio', { name: 'Fixed' }).click();
    await expect(pane0().locator('.tabs')).toHaveClass(/fixed/);
    await savedTabs((t) => t.tabWidth === 'fixed');

    await page.reload();
    await page.waitForSelector('[data-pane="0"] .row[data-path]', { state: 'attached' });
    await expect(pane0().locator('.tabs')).toHaveClass(/fixed/);
  });

  test('session restore: tabs and active tab come back after reload (ON)', async () => {
    // Pane 0: navigate tab 0 into Archive, add a second tab, then make the
    // FIRST tab active again — the saved current must survive the reload.
    await pane0().locator('.row', { hasText: 'Archive' }).dblclick();
    await expect(tabsOf().nth(0)).toHaveText(/Archive/);
    await page.keyboard.press('Control+t');
    await expect(tabsOf()).toHaveCount(2);
    await tabsOf().nth(0).click();
    await expect(tabsOf().nth(0)).toHaveAttribute('aria-selected', 'true');

    await expect
      .poll(async () => (await savedPrefs(page)).session?.panes[0])
      .toEqual({ tabs: ['C:\\Users\\demo\\Archive', 'C:\\Users\\demo'], current: 0 });

    await page.reload();
    await page.waitForSelector('[data-pane="0"] .row[data-path]', { state: 'attached' });
    await expect(tabsOf()).toHaveCount(2);
    await expect(tabsOf().nth(0)).toHaveText(/Archive/);
    await expect(tabsOf().nth(1)).toHaveText(/demo/);
    await expect(tabsOf().nth(0)).toHaveAttribute('aria-selected', 'true');
  });

  test('session restore OFF boots the default single home tab', async () => {
    await openSettings();
    await checkbox('Restore tabs on launch').uncheck();
    await savedTabs((t) => t.restoreTabs === false);
    await closeSettings();

    // The stored snapshot is cleared on the persist that followed the toggle.
    await expect.poll(async () => (await savedPrefs(page)).session).toBeNull();

    // Navigate somewhere else; with restore off nothing is captured.
    await pane0().locator('.row', { hasText: 'Archive' }).dblclick();
    await expect(tabsOf().nth(0)).toHaveText(/Archive/);

    await page.reload();
    await page.waitForSelector('[data-pane="0"] .row[data-path]', { state: 'attached' });
    await expect(tabsOf()).toHaveCount(1);
    await expect(tabsOf().nth(0)).toHaveText(/demo/); // home
    await expect(tabsOf().nth(0)).toHaveAttribute('aria-selected', 'true');
  });
});
