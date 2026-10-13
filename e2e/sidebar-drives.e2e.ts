import { test, expect, type Page } from '@playwright/test';

/**
 * FER-496 E2E: the sidebar Drives section shows real `list_drives` data (never the old fixture),
 * clicking a drive navigates the active pane, and empty / offline drives never break the sidebar.
 */

declare global {
  interface Window {
    __drives: unknown[];
    __mask: number;
    __listDelayMs: number;
    __listCalls: number;
  }
}

const D = (o: Record<string, unknown>) => ({
  label: '',
  freeBytes: 0,
  totalBytes: 0,
  driveType: 'fixed',
  fsName: 'NTFS',
  serial: '1A2B-3C4D',
  status: 'ok',
  ...o,
});

const GB = 1024 ** 3;

function injectTauriMock(page: Page, drives: unknown[], listDelayMs = 0) {
  page.addInitScript(
    ({ drives, listDelayMs }) => {
      window.__drives = drives;
      window.__mask = 0b1111;
      window.__listDelayMs = listDelayMs;
      window.__listCalls = 0;
      const file = (name: string, dir: string, isDir = false) => ({
        name,
        path: `${dir.replace(/\\$/, '')}\\${name}`,
        isDir,
        isSymlink: false,
        size: isDir ? 0 : 10,
        modifiedMs: 1700000000000,
        hidden: false,
      });
      const home = 'C:\\Users\\demo';
      const tree: Record<string, ReturnType<typeof file>[]> = {
        [home]: [file('notes.txt', home)],
        'C:': [file('Windows', 'C:', true), file('boot.ini', 'C:')],
        'D:': [file('Archive1', 'D:', true), file('zz.txt', 'D:')],
      };
      window.__TAURI_INTERNALS__ = {
        metadata: {
          currentWindow: { label: 'main' },
          currentWebview: { windowLabel: 'main', label: 'main' },
        },
        plugins: {},
        transformCallback: () => Math.floor(Math.random() * 1e9),
        invoke: (cmd, args) => {
          if (cmd === 'plugin:path|resolve_directory') return Promise.resolve(home);
          if (cmd === 'plugin:event|listen')
            return Promise.resolve(Math.floor(Math.random() * 1e9));
          if (cmd === 'watch_start' || cmd === 'watch_stop') return Promise.resolve(1);
          if (cmd === 'list_dir') {
            const raw = String((args as { path: string }).path).replace(/\\$/, '');
            const p = raw === 'C:\\Projects' || raw === 'D:\\Backup' ? home : raw;
            return tree[p]
              ? Promise.resolve(tree[p])
              : Promise.reject({ code: 'notFound', message: p });
          }
          if (cmd === 'list_drives') {
            window.__listCalls++;
            const snapshot = JSON.parse(JSON.stringify(window.__drives));
            return new Promise((res) => setTimeout(() => res(snapshot), window.__listDelayMs));
          }
          if (cmd === 'drive_letters_mask') return Promise.resolve(window.__mask);
          if (cmd === 'path_suggest')
            return Promise.resolve({ items: [], truncated: false, timedOut: false });
          if (cmd === 'path_resolve')
            return Promise.resolve({ path: (args as { input: string }).input, kind: 'dir' });
          return Promise.reject(new Error(`Unknown command: ${cmd}`));
        },
      };
    },
    { drives, listDelayMs },
  );
}

const REAL = [
  D({ label: 'Windows', path: 'C:\\', freeBytes: 103 * GB, totalBytes: 1862 * GB }),
  D({ label: '', path: 'D:\\', freeBytes: 500 * GB, totalBytes: 1000 * GB }),
  D({
    label: 'Flash',
    path: 'E:\\',
    driveType: 'removable',
    fsName: 'exFAT',
    freeBytes: 10 * GB,
    totalBytes: 64 * GB,
  }),
  D({ path: 'F:\\', driveType: 'optical', fsName: '', serial: '', status: 'noMedia' }),
  D({ path: 'Z:\\', driveType: 'network', fsName: '', serial: '', status: 'unavailable' }),
];

const sidebar = (page: Page) => page.getByRole('navigation', { name: 'Places' });
const drive = (page: Page, letter: string) =>
  sidebar(page).locator(`[data-drive="${letter}:\\\\"]`);

test.describe('sidebar drives (FER-496)', () => {
  test('shows list_drives data, not the old fixture', async ({ page }) => {
    injectTauriMock(page, REAL);
    await page.goto('/');
    await expect(drive(page, 'C')).toContainText('Windows (C:)');
    await expect(drive(page, 'C')).toContainText('103 GB free of 1.82 TB');
    await expect(drive(page, 'D')).toContainText('Local Disk (D:)');
    await expect(drive(page, 'E')).toContainText('Flash (E:)');
    await expect(sidebar(page)).not.toContainText('Samsung T9');
    await expect(sidebar(page)).not.toContainText('Archive (');
    await expect(sidebar(page)).not.toContainText('82 GB free of 512 GB');
    await expect(drive(page, 'C')).toHaveAttribute('title', /File system: NTFS/);
  });

  test('clicking a drive navigates the active pane to its root', async ({ page }) => {
    injectTauriMock(page, REAL);
    await page.goto('/');
    await drive(page, 'D').click();
    const left = page.getByRole('region', { name: 'Left pane' });
    const archive1 = left.locator('[data-path="D:\\\\Archive1"]');
    await expect(archive1).toBeVisible();
    await expect(archive1).toHaveClass(/cursor/);
    await expect(drive(page, 'D')).toHaveAttribute('aria-current', 'true');
    await expect(drive(page, 'C')).not.toHaveAttribute('aria-current', 'true');
  });

  test('drive without media shows no bar and does not navigate', async ({ page }) => {
    injectTauriMock(page, REAL);
    await page.goto('/');
    const f = drive(page, 'F');
    await expect(f).toContainText('CD/DVD Drive (F:)');
    await expect(f).toContainText('tidak ada media');
    await expect(f.locator('.bar')).toHaveCount(0);
    await f.click({ force: true });
    await expect(drive(page, 'F')).not.toHaveAttribute('aria-current', 'true');
  });

  test('offline network drive shows tidak tersedia and the sidebar stays usable', async ({
    page,
  }) => {
    injectTauriMock(page, REAL);
    await page.goto('/');
    const z = drive(page, 'Z');
    await expect(z).toContainText('Network Drive (Z:)');
    await expect(z).toContainText('tidak tersedia');
    await expect(z.locator('.bar')).toHaveCount(0);
    await drive(page, 'D').click();
    await expect(drive(page, 'D')).toHaveAttribute('aria-current', 'true');
  });

  test('slow list_drives does not freeze the UI', async ({ page }) => {
    injectTauriMock(page, REAL, 1500);
    await page.goto('/');
    await expect(sidebar(page)).toContainText('Memuat drive');
    // Rest of the sidebar stays interactive while the query is pending.
    await page.getByRole('button', { name: 'Tree' }).click();
    await page.getByRole('button', { name: 'Places' }).click();
    await expect(drive(page, 'C')).toBeVisible({ timeout: 5000 });
  });

  test('refreshes on window focus and when a drive is added or removed', async ({ page }) => {
    injectTauriMock(page, REAL);
    await page.goto('/');
    await expect(drive(page, 'C')).toBeVisible();
    await page.evaluate(() => {
      window.__drives = [
        ...window.__drives,
        {
          label: 'Samsung T9',
          path: 'G:\\',
          freeBytes: 1,
          totalBytes: 2,
          driveType: 'removable',
          fsName: 'exFAT',
          serial: '',
          status: 'ok',
        },
      ];
      window.dispatchEvent(new Event('focus'));
    });
    await expect(drive(page, 'G')).toContainText('Samsung T9 (G:)');
    // Mask change (drive removed) is picked up by the light poll without focus.
    await page.evaluate(() => {
      window.__drives = window.__drives.filter((d) => (d as { path: string }).path !== 'G:\\');
      window.__mask = 0b0111;
    });
    await expect(drive(page, 'G')).toHaveCount(0, { timeout: 12_000 });
  });
});
