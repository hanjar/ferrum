import { test, expect, type Page } from '@playwright/test';

/**
 * FER-484 E2E: Alt+Down folder history dropdown. Tauri IPC is mocked: a tiny directory tree and
 * an activity store with three frequent folders (one drive root, one archive location).
 */

function injectTauriMock(page: Page, opts: { recording: boolean }) {
  page.addInitScript((o) => {
    const dirs = ['C:\\', 'C:\\Users', 'C:\\Users\\demo', 'C:\\Users\\demo\\Documents'];
    const w = window as unknown as { __calls: { cmd: string; args: unknown }[] };
    w.__calls = [];
    const item = (locator: string, episodeCount: number) => ({
      entity: {
        id: locator,
        kind: 'folder',
        provider: 'local',
        volumeId: 'v',
        locator,
        parentPath: '',
        displayName: locator,
        availability: 'available',
      },
      lastUtcMs: 1700000000000 + episodeCount,
      episodeCount,
      activeDays: 1,
      score: null,
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
        w.__calls.push({ cmd, args });
        if (cmd === 'plugin:path|resolve_directory') return Promise.resolve('C:\\Users\\demo');
        if (cmd === 'plugin:event|listen') return Promise.resolve(Math.floor(Math.random() * 1e9));
        if (cmd === 'watch_start' || cmd === 'watch_stop') return Promise.resolve(1);
        if (cmd === 'list_dir') return Promise.resolve([]);
        if (cmd === 'path_suggest') {
          return Promise.resolve({ items: [], truncated: false, timedOut: false });
        }
        if (cmd === 'path_resolve') {
          const clean = (args as { input: string }).input.replace(/\\$/, '');
          return Promise.resolve({ path: clean, kind: dirs.includes(clean) ? 'dir' : 'missing' });
        }
        if (cmd === 'activity_state') {
          return Promise.resolve({
            recordingEnabled: o.recording,
            privacyEpoch: 1,
            retentionDays: 90,
            hiddenCount: 0,
            disclosureAcked: true,
          });
        }
        if (cmd === 'activity_frequent') {
          return Promise.resolve([
            item('\\\\?\\C:\\Users\\demo\\Documents', 12),
            item('C:\\', 7),
            item('D:\\Takeout\\file.zip\\Takeout', 3),
          ]);
        }
        if (cmd === 'activity_recent' || cmd === 'activity_favorites') return Promise.resolve([]);
        return Promise.resolve(null);
      },
    };
  }, opts);
}

const calls = (page: Page, cmd: string) =>
  page.evaluate(
    (c) =>
      (window as unknown as { __calls: { cmd: string; args: unknown }[] }).__calls.filter(
        (x) => x.cmd === c,
      ),
    cmd,
  );

async function goTo(page: Page, path: string) {
  const pane = page.locator('section.pane').first();
  await pane.getByLabel('Edit path').click();
  const input = pane.getByRole('combobox', { name: 'Address' });
  await input.fill(path);
  await input.press('Enter');
  await expect(input).toHaveCount(0);
}

async function setup(page: Page, recording: boolean) {
  injectTauriMock(page, { recording });
  await page.goto('/');
  const pane = page.locator('section.pane').first();
  await expect(pane).toBeVisible();
  await expect(pane.getByRole('navigation', { name: 'Path' })).toContainText('demo');
  // History in the first pane's tab: demo -> Documents -> Users (current).
  await goTo(page, 'C:\\Users\\demo\\Documents');
  await goTo(page, 'C:\\Users');
  await expect.poll(async () => (await calls(page, 'activity_state')).length).toBeGreaterThan(1);
  // Neutral focus target: the pane itself (not a text field).
  await pane.locator('.topLine').click({ force: true });
  return pane;
}

test.describe('folder history dropdown (FER-484)', () => {
  test('Alt+Down opens two sections, the last row is the current folder', async ({ page }) => {
    const pane = await setup(page, true);
    await page.keyboard.press('Alt+ArrowDown');
    const menu = page.getByRole('listbox', { name: 'Folder history' });
    await expect(menu).toBeVisible();
    const rows = menu.getByRole('option');
    // 3 frequent + home, Documents, Users.
    await expect(rows).toHaveCount(6);
    await expect(menu.getByRole('separator')).toHaveCount(1);
    // Most visited first, with counts; the \\?\ prefix is never shown.
    await expect(rows.nth(0)).toContainText('C:\\Users\\demo\\Documents');
    await expect(rows.nth(0)).not.toContainText('?');
    await expect(rows.nth(0)).toContainText('12');
    await expect(rows.nth(2)).toContainText('D:\\Takeout\\file.zip\\Takeout');
    // Chronological part: current folder last, highlighted and under the cursor.
    await expect(rows.nth(5)).toHaveAttribute('aria-current', 'location');
    await expect(rows.nth(5)).toHaveAttribute('aria-selected', 'true');
    await expect(rows.nth(5)).toContainText('C:\\Users');
    await expect(rows.nth(4)).toContainText('Documents');
    // Width follows the pane.
    const paneBox = await pane.boundingBox();
    const menuBox = await menu.boundingBox();
    expect(Math.abs((menuBox?.width ?? 0) - (paneBox?.width ?? 1))).toBeLessThan(2);
    if (process.env.FER484_SHOT) await page.screenshot({ path: process.env.FER484_SHOT });
    // Alt+Down again closes.
    await page.keyboard.press('Alt+ArrowDown');
    await expect(menu).toHaveCount(0);
  });

  test('Up then Enter navigates the active pane', async ({ page }) => {
    const pane = await setup(page, true);
    await page.keyboard.press('Alt+ArrowDown');
    const menu = page.getByRole('listbox', { name: 'Folder history' });
    await expect(menu).toBeVisible();
    await page.keyboard.press('ArrowUp');
    await page.keyboard.press('Enter');
    await expect(menu).toHaveCount(0);
    await expect(pane.getByRole('navigation', { name: 'Path' })).toContainText('Documents');
    await expect
      .poll(async () =>
        (await calls(page, 'list_dir')).some(
          (c) => (c.args as { path: string }).path === 'C:\\Users\\demo\\Documents',
        ),
      )
      .toBe(true);
  });

  test('Esc closes without navigating', async ({ page }) => {
    const pane = await setup(page, true);
    await page.keyboard.press('Alt+ArrowDown');
    const menu = page.getByRole('listbox', { name: 'Folder history' });
    await expect(menu).toBeVisible();
    await page.keyboard.press('ArrowUp');
    await page.keyboard.press('Escape');
    await expect(menu).toHaveCount(0);
    await expect(pane.getByRole('navigation', { name: 'Path' })).toContainText('Users');
    await expect(pane.getByRole('navigation', { name: 'Path' })).not.toContainText('Documents');
  });

  test('recording off: the frequent section is empty, only history and a note', async ({
    page,
  }) => {
    await setup(page, false);
    await page.keyboard.press('Alt+ArrowDown');
    const menu = page.getByRole('listbox', { name: 'Folder history' });
    await expect(menu).toBeVisible();
    await expect(menu.getByRole('option')).toHaveCount(3);
    await expect(menu.getByRole('separator')).toHaveCount(0);
    await expect(menu.getByRole('status')).toContainText('Frequent folders are hidden');
    await expect(menu.getByRole('option').last()).toHaveAttribute('aria-current', 'location');
    // The dropdown itself read nothing from the activity store (limit 5 is the sidebar's own call).
    const mine = (await calls(page, 'activity_frequent')).filter(
      (c) => (c.args as { limit: number }).limit === 20,
    );
    expect(mine).toHaveLength(0);
  });

  test('Alt+Down inside the address bar does not open the dropdown', async ({ page }) => {
    const pane = await setup(page, true);
    await pane.getByLabel('Edit path').click();
    const input = pane.getByRole('combobox', { name: 'Address' });
    await expect(input).toBeFocused();
    await page.keyboard.press('Alt+ArrowDown');
    await expect(page.getByRole('listbox', { name: 'Folder history' })).toHaveCount(0);
  });
});
