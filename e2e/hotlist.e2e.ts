import { test, expect, type Page } from '@playwright/test';

/**
 * FER-458 E2E: nested Directory Hotlist — Ctrl+D popup with accelerators, submenus, the ☆▾ button,
 * Configure, Total Commander import preview and the first-run confirmation of command items.
 * Tauri IPC is mocked; the hotlist backend is a tiny in-page store.
 */

function injectTauriMock(page: Page) {
  page.addInitScript(() => {
    const w = window as unknown as {
      __calls: { cmd: string; args: unknown }[];
      __hotlist: unknown;
      __approved: boolean;
    };
    w.__calls = [];
    w.__approved = false;
    w.__hotlist = {
      version: 2,
      items: [
        {
          type: 'submenu',
          title: '&1. Drive',
          items: [
            { type: 'dir', title: '&1. Drive', path: 'C:\\Users\\demo\\Drive\\' },
            {
              type: 'submenu',
              title: '&6. personal',
              items: [
                {
                  type: 'dir',
                  title: '3. &Bob',
                  path: 'C:\\Users\\demo\\Drive\\personal\\bob\\',
                },
                {
                  type: 'dir',
                  title: '4. &Charlie',
                  path: 'C:\\Users\\demo\\Drive\\personal\\charlie\\',
                },
              ],
            },
          ],
        },
        { type: 'separator' },
        { type: 'dir', title: '&Downloads', path: 'C:\\Users\\demo\\Downloads' },
        { type: 'submenu', title: 'Dummy', items: [] },
        { type: 'command', title: 'kando', command: 'C:\\tools\\forward to kando.exe', args: [] },
        { type: 'dir', title: '&Server share', path: '\\\\filesrv\\team' },
      ],
    };
    const view = () => ({
      doc: w.__hotlist,
      status: 'ok',
      readOnly: false,
      customLocation: null,
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
        if (cmd === 'hotlist_load') return Promise.resolve(view());
        if (cmd === 'hotlist_save') {
          w.__hotlist = (args as { doc: unknown }).doc;
          return Promise.resolve(view());
        }
        if (cmd === 'hotlist_run_command') {
          // The real backend shows a native confirmation itself (FER-467 R5).
          return Promise.resolve({ launched: true });
        }
        if (cmd === 'hotlist_import_preview') {
          return Promise.resolve({
            doc: {
              version: 2,
              items: [{ type: 'dir', title: '&Imported', path: 'C:\\Users\\demo\\Imported' }],
            },
            warnings: [{ index: 4, kind: 'no_action' }],
          });
        }
        return Promise.resolve(null);
      },
    };
  });
}

const calls = (page: Page, cmd: string) =>
  page.evaluate(
    (c) =>
      (window as unknown as { __calls: { cmd: string; args: unknown }[] }).__calls.filter(
        (x) => x.cmd === c,
      ),
    cmd,
  );

test.describe('nested hotlist (FER-458)', () => {
  test.beforeEach(async ({ page }) => {
    injectTauriMock(page);
    await page.goto('/');
    await expect(page.locator('section.pane').first()).toBeVisible();
    await expect.poll(async () => (await calls(page, 'hotlist_load')).length).toBeGreaterThan(0);
  });

  test('Ctrl+D, accelerators 1 → 6 → 4 open Charlie in the active pane', async ({ page }) => {
    await page.keyboard.press('Control+d');
    const menu = page.getByRole('menu', { name: 'Directory Hotlist' });
    await expect(menu).toBeVisible();
    await expect(page.getByRole('menuitem', { name: 'Drive' })).toBeVisible();
    await expect(page.getByRole('menuitem', { name: 'kando' })).toBeVisible();
    await page.keyboard.press('1');
    await expect(page.getByRole('menuitem', { name: 'personal' })).toBeVisible();
    await page.keyboard.press('6');
    await expect(page.getByRole('menuitem', { name: 'Charlie' })).toBeVisible();
    await page.keyboard.press('4');
    await expect(menu).toBeHidden();
    await expect
      .poll(async () =>
        (await calls(page, 'list_dir')).map((c) => (c.args as { path: string }).path),
      )
      .toContain('C:\\Users\\demo\\Drive\\personal\\charlie\\');
  });

  test('the ☆▾ button opens the same menu; Escape steps out of a submenu, then closes', async ({
    page,
  }) => {
    await page
      .locator('section.pane')
      .first()
      .getByTitle(/Directory Hotlist/)
      .click();
    const menu = page.getByRole('menu', { name: 'Directory Hotlist' });
    await expect(menu).toBeVisible();
    await page.keyboard.press('ArrowRight');
    await expect(page.getByRole('menu', { name: 'Submenu' })).toBeVisible();
    await page.keyboard.press('Escape');
    await expect(page.getByRole('menu', { name: 'Submenu' })).toBeHidden();
    await expect(menu).toBeVisible();
    await page.keyboard.press('Escape');
    await expect(menu).toBeHidden();
  });

  test('a command item is sent by its place in the tree; the webview never confirms', async ({
    page,
  }) => {
    await page.keyboard.press('Control+d');
    await page.getByRole('menuitem', { name: 'kando' }).click();
    await expect.poll(async () => (await calls(page, 'hotlist_run_command')).length).toBe(1);
    const [run] = await calls(page, 'hotlist_run_command');
    expect(run.args).toEqual({
      position: [4],
      title: 'kando',
      command: 'C:\\tools\\forward to kando.exe',
      args: [],
    });
    // FER-467 R5: no webview dialog and no "confirmed" flag; the backend asks natively.
    expect(JSON.stringify(run.args)).not.toMatch(/confirm/i);
    await expect(page.getByRole('dialog')).toHaveCount(0);
  });

  test('a network folder is tagged and asks once for its host (FER-467 R8)', async ({ page }) => {
    await page.keyboard.press('Control+d');
    const share = page.getByRole('menuitem', { name: 'Server share' });
    await expect(share.locator('.net')).toHaveText('net');
    await expect(page.getByRole('menuitem', { name: 'Downloads' }).locator('.net')).toHaveCount(0);
    await share.click();
    const dialog = page.getByRole('dialog', { name: 'Open network folder' });
    await expect(dialog).toContainText('filesrv');
    const listed = async () =>
      (await calls(page, 'list_dir')).map((c) => (c.args as { path: string }).path);
    expect(await listed()).not.toContain('\\\\filesrv\\team');
    await dialog.getByRole('button', { name: 'Open' }).click();
    await expect(dialog).toBeHidden();
    await expect.poll(listed).toContain('\\\\filesrv\\team');
    // Known now: no second question.
    await page.keyboard.press('Control+d');
    await page.getByRole('menuitem', { name: 'Server share' }).click();
    await expect(page.getByRole('dialog', { name: 'Open network folder' })).toHaveCount(0);
  });

  test('Add current folder saves through the backend', async ({ page }) => {
    await page.keyboard.press('Control+d');
    await page.getByRole('menuitem', { name: 'Add current folder' }).click();
    await expect.poll(async () => (await calls(page, 'hotlist_save')).length).toBeGreaterThan(0);
    const saved = (await calls(page, 'hotlist_save')).at(-1)!.args as {
      doc: { items: { type: string; path?: string }[] };
    };
    expect(saved.doc.items.at(-1)).toMatchObject({ type: 'dir', path: 'C:\\Users\\demo' });
  });

  test('Configure: edit a title, import preview merges into the draft, OK saves', async ({
    page,
  }) => {
    await page.keyboard.press('Control+d');
    await page.getByRole('menuitem', { name: 'Configure…' }).click();
    const dialog = page.getByRole('dialog', { name: 'Configure Directory Hotlist' });
    await expect(dialog).toBeVisible();
    await dialog.getByRole('button', { name: /Import from Total Commander/ }).click();
    await dialog.getByRole('button', { name: 'Preview' }).click();
    await expect(dialog.getByRole('list', { name: 'Import preview' })).toContainText('Imported');
    await expect(dialog.getByRole('list', { name: 'Import warnings' })).toContainText('Entry 4');
    await dialog.getByRole('button', { name: 'Import', exact: true }).click();
    await expect(dialog.getByRole('treeitem', { name: /Imported/ })).toBeVisible();
    // Still a draft: nothing saved yet.
    expect((await calls(page, 'hotlist_save')).length).toBe(0);
    await dialog.getByRole('button', { name: 'OK' }).click();
    await expect(dialog).toBeHidden();
    const saved = (await calls(page, 'hotlist_save')).at(-1)!.args as {
      doc: { items: { title: string }[] };
    };
    expect(saved.doc.items.some((i) => i.title === '&Imported')).toBe(true);
    expect(saved.doc.items.length).toBe(7);
  });

  test('Configure: dragging a folder onto an empty submenu moves it inside; OK saves', async ({
    page,
  }) => {
    await page.keyboard.press('Control+d');
    await page.getByRole('menuitem', { name: 'Configure…' }).click();
    const dialog = page.getByRole('dialog', { name: 'Configure Directory Hotlist' });
    await expect(dialog).toBeVisible();
    const from = await dialog.getByRole('treeitem', { name: /Downloads/ }).boundingBox();
    const to = await dialog.getByRole('treeitem', { name: /Dummy/ }).boundingBox();
    expect(from && to).toBeTruthy();
    await page.mouse.move(from!.x + 30, from!.y + from!.height / 2);
    await page.mouse.down();
    await page.mouse.move(from!.x + 32, from!.y + from!.height / 2 + 8, { steps: 3 });
    await page.mouse.move(to!.x + 30, to!.y + to!.height / 2, { steps: 5 });
    await page.mouse.up();
    // Dummy is now a level above Downloads.
    await expect(dialog.getByRole('treeitem', { name: /Downloads/ })).toHaveAttribute(
      'aria-level',
      '2',
    );
    await dialog.getByRole('button', { name: 'OK' }).click();
    await expect(dialog).toBeHidden();
    const saved = (await calls(page, 'hotlist_save')).at(-1)!.args as {
      doc: { items: { title: string; items?: { title: string }[] }[] };
    };
    const dummy = saved.doc.items.find((i) => i.title === 'Dummy');
    expect(dummy?.items?.map((i) => i.title)).toEqual(['&Downloads']);
    expect(saved.doc.items.some((i) => i.title === '&Downloads')).toBe(false);
  });

  test('Configure: clicking the submenu arrow folds and unfolds it (real pointer capture)', async ({
    page,
  }) => {
    await page.keyboard.press('Control+d');
    await page.getByRole('menuitem', { name: 'Configure…' }).click();
    const dialog = page.getByRole('dialog', { name: 'Configure Directory Hotlist' });
    await expect(dialog).toBeVisible();
    const bob = dialog.getByRole('treeitem', { name: /Bob/ });
    const personal = dialog.getByRole('treeitem', { name: /^&6. personal$/ });
    await expect(bob).toBeVisible();
    await expect(personal).toHaveAttribute('aria-expanded', 'true');
    await personal.locator('.glyph').click();
    await expect(bob).toBeHidden();
    await expect(personal).toHaveAttribute('aria-expanded', 'false');
    await personal.locator('.glyph').click();
    await expect(bob).toBeVisible();
    await expect(personal).toHaveAttribute('aria-expanded', 'true');
  });
});
