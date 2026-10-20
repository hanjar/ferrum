import { test, expect, type Page } from '@playwright/test';

/**
 * FER-577 (B0+B1) E2E: dialogs, the transfer panel, the context menu and the menubar can be run
 * with the keyboard alone, and Enter on a button never runs the dialog's primary action.
 */

function injectTauriMock(page: Page) {
  page.addInitScript(() => {
    const file = (name: string, dir: string, isDir = false) => ({
      name,
      path: dir.endsWith('\\') ? `${dir}${name}` : `${dir}\\${name}`,
      isDir,
      isSymlink: false,
      size: isDir ? 0 : 10,
      modifiedMs: 1700000000000,
      hidden: false,
    });
    const home = 'C:\\Users\\demo';
    const tree: Record<string, ReturnType<typeof file>[]> = {
      'C:': [file('Users', 'C:\\', true)],
      'C:\\Users': [file('demo', 'C:\\Users', true)],
      [home]: [file('notes.txt', home), file('zeta.txt', home)],
    };
    const calls: { cmd: string; args: unknown }[] = [];
    (window as unknown as { __calls: typeof calls }).__calls = calls;
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
        calls.push({ cmd, args });
        if (cmd === 'plugin:path|resolve_directory') return Promise.resolve(home);
        if (cmd === 'plugin:event|listen') return Promise.resolve(Math.floor(Math.random() * 1e9));
        if (cmd === 'watch_start' || cmd === 'watch_stop') return Promise.resolve(1);
        if (cmd === 'list_dir') {
          const raw = String((args as { path: string }).path).replace(/\\$/, '');
          return tree[raw]
            ? Promise.resolve(tree[raw])
            : Promise.reject({ code: 'notFound', message: raw });
        }
        if (cmd === 'list_drives') {
          return Promise.resolve([
            { label: 'System', path: 'C:\\', freeBytes: 1e9, totalBytes: 5e11 },
          ]);
        }
        if (cmd === 'archive_rar_tool_probe') return Promise.resolve({ ok: true, version: 'x' });
        if (cmd === 'archive_create') return Promise.resolve('C:\\Users\\demo\\notes.7z');
        if (cmd === 'delete_entries') {
          const { paths } = args as { paths: string[] };
          return Promise.resolve({ requested: paths.length, failed: [] });
        }
        return Promise.reject(new Error(`Unknown command: ${cmd}`));
      },
    };
  });
}

const callsOf = (page: Page, cmd: string) =>
  page.evaluate(
    (c) =>
      (window as unknown as { __calls: { cmd: string; args: unknown }[] }).__calls.filter(
        (x) => x.cmd === c,
      ),
    cmd,
  );

test.describe('Keyboard-first dialogs (FER-577)', () => {
  let page: Page;
  const left = () => page.getByRole('region', { name: 'Left pane' });

  test.beforeEach(async ({ browser }) => {
    page = await browser.newPage({ viewport: { width: 1600, height: 800 } });
    injectTauriMock(page);
    await page.goto('/');
    await expect(left().locator('[data-path$="notes.txt"]')).toBeVisible();
    await left().locator('[data-path$="notes.txt"]').click();
  });
  test.afterEach(async () => {
    await page.close();
  });

  test('Delete: Enter on Cancel closes without deleting; Alt+P toggles permanently; Esc cancels', async () => {
    await page.keyboard.press('F8');
    const dlg = page.getByRole('dialog', { name: 'Delete' });
    await expect(dlg).toBeVisible();
    await expect(dlg).toBeFocused(); // the dialog itself, not the Delete button

    await page.keyboard.press('Alt+p');
    await expect(dlg.getByRole('checkbox')).toBeChecked();
    await page.keyboard.press('Alt+p');
    await expect(dlg.getByRole('checkbox')).not.toBeChecked();

    // Alt+P left the focus on the checkbox. Tab -> Cancel; Enter activates Cancel, which must
    // not delete.
    await page.keyboard.press('Tab');
    await expect(dlg.getByRole('button', { name: 'Cancel' })).toBeFocused();
    await page.keyboard.press('Enter');
    await expect(dlg).toHaveCount(0);
    expect(await callsOf(page, 'delete_entries')).toHaveLength(0);
    // Focus is back on the file list.
    await expect(left().locator('[data-pane-list]')).toBeFocused();

    // Esc also cancels; a bare Enter on the dialog is the (deliberate) F8 + Enter confirm.
    await page.keyboard.press('F8');
    await expect(dlg).toBeVisible();
    await page.keyboard.press('Escape');
    await expect(dlg).toHaveCount(0);
    expect(await callsOf(page, 'delete_entries')).toHaveLength(0);
    await page.keyboard.press('F8');
    await page.keyboard.press('Enter');
    await expect(dlg).toHaveCount(0);
    expect(await callsOf(page, 'delete_entries')).toHaveLength(1);
  });

  test('Pack: choose 7Z + Maximum and Pack with the keyboard only', async () => {
    await page.keyboard.press('Alt+F5');
    const dlg = page.getByRole('dialog', { name: 'Pack to archive' });
    await expect(dlg).toBeVisible();
    await expect(dlg.getByText('SEVENZ')).toHaveCount(0);
    await page.keyboard.press('Alt+7');
    await expect(dlg.getByRole('radio', { name: '7Z' })).toBeChecked();
    await page.keyboard.press('Alt+m');
    await expect(dlg.getByRole('radio', { name: 'Maximum' })).toBeChecked();
    // Arrows roving inside the group: Left goes to Normal.
    await page.keyboard.press('ArrowLeft');
    await expect(dlg.getByRole('radio', { name: 'Normal' })).toBeChecked();
    await page.keyboard.press('Alt+m');
    await page.keyboard.press('Alt+p');
    await expect(dlg).toHaveCount(0);
    const calls = await callsOf(page, 'archive_create');
    expect(calls).toHaveLength(1);
    expect(calls[0].args).toMatchObject({
      request: { format: 'sevenz', level: 'max', destinationPath: expect.stringMatching(/\.7z$/) },
    });
  });

  test('Pack: Enter on Cancel does not start the pack, Tab stays inside the dialog', async () => {
    await page.keyboard.press('Alt+F5');
    const dlg = page.getByRole('dialog', { name: 'Pack to archive' });
    await expect(dlg).toBeVisible();
    for (let i = 0; i < 12; i++) {
      await page.keyboard.press('Tab');
      expect(await dlg.evaluate((el) => el.contains(document.activeElement))).toBe(true);
    }
    await dlg.getByRole('button', { name: 'Cancel' }).focus();
    await page.keyboard.press('Enter');
    await expect(dlg).toHaveCount(0);
    expect(await callsOf(page, 'archive_create')).toHaveLength(0);
  });

  test('shortcuts do not leak behind the transfer panel; Esc and Alt+B close it', async () => {
    await page.keyboard.press('Alt+b');
    const panel = page.getByRole('dialog', { name: 'Background Transfer Manager' });
    await expect(panel).toBeVisible();
    expect(await panel.evaluate((el) => el.contains(document.activeElement))).toBe(true);
    await page.keyboard.press('F7');
    await expect(page.getByRole('dialog', { name: 'New folder' })).toHaveCount(0);
    await page.keyboard.press('Escape');
    await expect(panel).toHaveCount(0);
    await expect(left().locator('[data-pane-list]')).toBeFocused();
    await page.keyboard.press('Alt+b');
    await expect(panel).toBeVisible();
    await page.keyboard.press('Alt+b');
    await expect(panel).toHaveCount(0);
    // Without a modal the shortcut works again.
    await page.keyboard.press('F7');
    await expect(page.getByRole('dialog', { name: 'New folder' })).toBeVisible();
  });

  test('context menu from the keyboard: Shift+F10, arrows, Home/End, accelerator, Esc', async () => {
    await left().locator('[data-pane-list]').focus();
    await page.keyboard.press('Shift+F10');
    const menu = page.getByRole('menu', { name: 'Context menu' });
    await expect(menu).toBeVisible();
    const box = await menu.boundingBox();
    const row = await left().locator('[data-path$="notes.txt"]').boundingBox();
    expect(Math.abs(box!.y - (row!.y + row!.height))).toBeLessThan(80); // anchored to the row
    await expect(menu.getByRole('menuitem').first()).toBeFocused();
    await page.keyboard.press('End');
    await expect(menu.getByRole('menuitem').last()).toBeFocused();
    await page.keyboard.press('Home');
    await expect(menu.getByRole('menuitem').first()).toBeFocused();
    await page.keyboard.press('Escape');
    await expect(menu).toHaveCount(0);
    await expect(left().locator('[data-pane-list]')).toBeFocused();
    // Accelerator letter runs an item: Delete opens the delete dialog.
    await page.keyboard.press('ContextMenu');
    await expect(menu).toBeVisible();
    const del = menu.getByRole('menuitem', { name: /^Delete\b/ }).first();
    const letter = await del.evaluate((el) => (el as HTMLElement).dataset.mnemonic!);
    await page.keyboard.press(letter);
    // A dangerous item is only focused by its letter (FER-577 S2); Enter runs it.
    await expect(del).toBeFocused();
    await expect(page.getByRole('dialog', { name: 'Delete' })).toHaveCount(0);
    await page.keyboard.press('Enter');
    await expect(page.getByRole('dialog', { name: 'Delete' })).toBeVisible();
  });

  test('menubar: Alt+F opens Files, Alt+W opens Show, underlined letters are visible', async () => {
    const trigger = (name: string) =>
      page.locator('header.titlebar').getByRole('menuitem', { name, exact: true });
    await expect(trigger('Files').locator('u')).toHaveText('F');
    await page.keyboard.press('Alt+f');
    await expect(page.getByRole('menu', { name: 'Files' })).toBeVisible();
    await page.keyboard.press('Alt+w');
    await expect(page.getByRole('menu', { name: 'Show' })).toBeVisible();
    await page.keyboard.press('Escape');
    await expect(page.getByRole('menu')).toHaveCount(0);
    // Alt+D (address bar) is untouched by the menubar.
    await page.keyboard.press('Alt+d');
    await expect(page.getByRole('menu')).toHaveCount(0);
  });
});
