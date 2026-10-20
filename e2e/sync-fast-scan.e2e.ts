import { test, expect, type Page } from '@playwright/test';

/**
 * FER-472 E2E: the "Scan lebih cepat?" offer before a Sync scan, in browser mode with a mock IPC
 * (same pattern as multi-rename.e2e.ts). The Rust engine and the UAC helper have their own suites;
 * here we pin the UI contract: the offer shows for an eligible folder pair, Enter = ordinary scan
 * (no `fastScan` flag, no elevation), choosing the fast scan sends the flag, a skipped fast scan
 * is explained, and an ineligible pair never asks.
 */

type Calls = { compare: Record<string, unknown>[]; eligibility: string[] };

function injectTauriMock(page: Page, opts: { eligible: boolean; note?: string }) {
  page.addInitScript((o: { eligible: boolean; note?: string }) => {
    const dir = 'C:\\Users\\demo';
    const calls = { compare: [] as unknown[], eligibility: [] as string[] };
    (window as unknown as Record<string, unknown>).__syncMock = calls;
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
        if (cmd === 'plugin:path|resolve_directory') return Promise.resolve(dir);
        if (cmd === 'plugin:event|listen') return Promise.resolve(Math.floor(Math.random() * 1e9));
        if (cmd === 'watch_start' || cmd === 'watch_stop') return Promise.resolve(1);
        if (cmd === 'list_dir')
          return Promise.resolve([
            {
              name: 'a.txt',
              path: `${dir}\\a.txt`,
              isDir: false,
              isSymlink: false,
              size: 10,
              modifiedMs: 1700000000000,
              hidden: false,
            },
          ]);
        if (cmd === 'list_drives') return Promise.resolve([]);
        if (cmd === 'load_prefs' || cmd === 'save_prefs') return Promise.resolve(null);
        if (cmd === 'validate_sync_roots') return Promise.resolve(null);
        if (cmd === 'sync_baseline_load')
          return Promise.resolve({ status: 'firstRun', reason: 'neverSaved' });
        if (cmd === 'disk_usage_fast_eligibility') {
          calls.eligibility.push((args as { root: string }).root);
          return Promise.resolve(
            o.eligible ? { eligible: true, volume: 'C' } : { eligible: false },
          );
        }
        if (cmd === 'compare_dirs') {
          const a = args as Record<string, unknown>;
          calls.compare.push(a);
          return Promise.resolve({
            leftRoot: a.left,
            rightRoot: a.right,
            entries: [],
            truncated: false,
            recursive: true,
            ...(a.fastScan ? { fastScan: { left: !o.note, right: false, note: o.note } } : {}),
          });
        }
        return Promise.reject(new Error(`Unknown command: ${cmd}`));
      },
    };
  }, opts);
}

async function boot(page: Page, opts: { eligible: boolean; note?: string }) {
  injectTauriMock(page, opts);
  await page.goto('/');
  await page.waitForSelector('[data-pane="0"] .row[data-path]', {
    state: 'attached',
    timeout: 10_000,
  });
}

const calls = (page: Page) =>
  page.evaluate(() => (window as unknown as { __syncMock: Calls }).__syncMock);

async function openSync(page: Page) {
  await page.keyboard.press('Control+k');
  await page.getByRole('combobox', { name: 'Search commands' }).fill('Synchronize Dirs');
  await page.keyboard.press('Enter');
}

test.describe('FER-472 sync fast-scan offer (mock IPC)', () => {
  let page: Page;

  test.beforeEach(async ({ browser }) => {
    page = await browser.newPage();
  });

  test.afterEach(async () => {
    await page.close();
  });

  test('the offer defaults to the ordinary scan (Escape/Enter): no fastScan flag', async () => {
    await boot(page, { eligible: true });
    await openSync(page);
    const offer = page.locator('[role="dialog"][data-kind="fastScanOffer"]');
    await expect(offer).toBeVisible();
    await expect(offer).toContainText('C:\\Users\\demo');
    await expect(offer.getByRole('button', { name: 'Scan biasa' })).toBeFocused();
    await page.keyboard.press('Enter');
    await expect(
      page.getByRole('complementary', { name: 'Synchronize Directories' }),
    ).toBeVisible();
    const c = await calls(page);
    expect(c.compare).toHaveLength(1);
    expect(c.compare[0]).not.toHaveProperty('fastScan');
  });

  test('choosing the fast scan sends fastScan and explains a fallback', async () => {
    await boot(page, { eligible: true, note: 'Scan cepat dilewati: izin admin ditolak' });
    await openSync(page);
    await page.getByRole('button', { name: 'Scan cepat (admin)' }).click();
    const panel = page.getByRole('complementary', { name: 'Synchronize Directories' });
    await expect(panel).toContainText('izin admin ditolak');
    const c = await calls(page);
    expect(c.compare).toHaveLength(1);
    expect(c.compare[0]).toMatchObject({ fastScan: true, recursive: true });
  });

  test('a folder pair that cannot use the MFT never asks', async () => {
    await boot(page, { eligible: false });
    await openSync(page);
    await expect(
      page.getByRole('complementary', { name: 'Synchronize Directories' }),
    ).toBeVisible();
    await expect(page.locator('[role="dialog"][data-kind="fastScanOffer"]')).toHaveCount(0);
    const c = await calls(page);
    expect(c.compare).toHaveLength(1);
    expect(c.compare[0]).not.toHaveProperty('fastScan');
  });
});
