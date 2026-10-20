import { test, expect, type Page } from '@playwright/test';

/**
 * FER-577 B3 + FER-580 E2E, keyboard only: Recent commands in the Command Palette, Ctrl+K over a
 * modal (S3), Settings categories, the Hotlist configure dialog, and the docked Search panel.
 */

function injectTauriMock(page: Page) {
  page.addInitScript(() => {
    const w = window as unknown as {
      __calls: { cmd: string; args: unknown }[];
      __hotlist: unknown;
    };
    w.__calls = [];
    w.__hotlist = {
      version: 2,
      items: [
        { type: 'dir', title: '&Downloads', path: 'C:\\Users\\demo\\Downloads' },
        { type: 'dir', title: 'Work', path: 'C:\\Users\\demo\\Work' },
      ],
    };
    const view = () => ({ doc: w.__hotlist, status: 'ok', readOnly: false, customLocation: null });
    const file = (name: string, dir: string) => ({
      name,
      path: `${dir}\\${name}`,
      isDir: false,
      isSymlink: false,
      size: 10,
      modifiedMs: 1700000000000,
      hidden: false,
    });
    const home = 'C:\\Users\\demo';
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
        if (cmd === 'plugin:path|resolve_directory') return Promise.resolve(home);
        if (cmd === 'plugin:event|listen') return Promise.resolve(Math.floor(Math.random() * 1e9));
        if (cmd === 'watch_start' || cmd === 'watch_stop') return Promise.resolve(1);
        if (cmd === 'list_dir') {
          const raw = String((args as { path: string }).path).replace(/\\$/, '');
          return raw === home
            ? Promise.resolve([file('notes.txt', home), file('zeta.txt', home)])
            : Promise.resolve([]);
        }
        if (cmd === 'list_drives') {
          return Promise.resolve([
            { label: 'System', path: 'C:\\', freeBytes: 1e9, totalBytes: 5e11 },
          ]);
        }
        if (cmd === 'hotlist_load') return Promise.resolve(view());
        if (cmd === 'hotlist_save') {
          w.__hotlist = (args as { doc: unknown }).doc;
          return Promise.resolve(view());
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

test.describe('Keyboard B3 and Recent commands', () => {
  let page: Page;
  const left = () => page.getByRole('region', { name: 'Left pane' });
  const palette = () => page.getByRole('dialog', { name: 'Command palette' });

  test.beforeEach(async ({ browser }) => {
    page = await browser.newPage({ viewport: { width: 1600, height: 900 } });
    injectTauriMock(page);
    await page.goto('/');
    await expect(left().locator('[data-path$="notes.txt"]')).toBeVisible();
    await left().locator('[data-path$="notes.txt"]').click();
  });
  test.afterEach(async () => {
    await page.close();
  });

  test('Recent commands: run by shortcut and palette, shown first, Enter reruns, survives a reload', async () => {
    // Nothing run yet: the palette has no Recent commands section.
    await page.keyboard.press('Control+k');
    await expect(palette()).toBeVisible();
    await expect(palette().getByRole('group', { name: 'Recent commands' })).toHaveCount(0);
    await page.keyboard.press('Escape');
    await expect(palette()).toHaveCount(0);
    await expect(left().locator('[data-pane-list]')).toBeFocused(); // focus came back

    // 1) a keyboard shortcut (Ctrl+H: toggle hidden files)
    await page.keyboard.press('Control+h');
    // 2) through the palette
    await page.keyboard.press('Control+k');
    await page.getByRole('combobox').fill('vertical arr');
    await page.keyboard.press('Enter');
    await expect(palette()).toHaveCount(0);

    await page.keyboard.press('Control+k');
    const recent = palette().getByRole('group', { name: 'Recent commands' });
    await expect(recent).toBeVisible();
    // Newest first, and it is the very first group of the palette.
    await expect(recent.getByRole('option')).toHaveText([/Vertical Arrangement/, /hidden files/]);
    await expect(palette().getByRole('group').first()).toHaveAccessibleName('Recent commands');
    // Arrow down walks within the section, Enter reruns the highlighted command.
    await expect(recent.getByRole('option').first()).toHaveAttribute('aria-selected', 'true');
    await page.keyboard.press('ArrowDown');
    await expect(recent.getByRole('option').nth(1)).toHaveAttribute('aria-selected', 'true');
    await page.keyboard.press('Enter');
    await expect(palette()).toHaveCount(0);

    // The run moved "hidden files" to the front, and only ids are stored.
    await page.keyboard.press('Control+k');
    await expect(
      palette().getByRole('group', { name: 'Recent commands' }).getByRole('option').first(),
    ).toHaveText(/hidden files/);
    await page.keyboard.press('Escape');
    const stored = await page.evaluate(() => localStorage.getItem('ferrum.recentCommands.v1'));
    expect(JSON.parse(stored!)).toEqual(['view.hidden', 'show.vertical']);

    await page.reload();
    await expect(left().locator('[data-path$="notes.txt"]')).toBeVisible();
    await left().locator('[data-path$="notes.txt"]').click();
    await page.keyboard.press('Control+k');
    await expect(
      palette().getByRole('group', { name: 'Recent commands' }).getByRole('option'),
    ).toHaveCount(2);
  });

  test('S3: Ctrl+K does not open the palette over Settings or Multi-rename', async () => {
    await page.keyboard.press('Control+,');
    const settings = page.getByRole('dialog', { name: 'Settings' });
    await expect(settings).toBeVisible();
    await page.keyboard.press('Control+k');
    await expect(palette()).toHaveCount(0);
    await page.keyboard.press('Escape');
    await expect(settings).toHaveCount(0);

    await page.keyboard.press('Control+m');
    const rename = page.getByRole('dialog', { name: /rename/i });
    await expect(rename).toBeVisible();
    await page.keyboard.press('Control+k');
    await expect(palette()).toHaveCount(0);
    await page.keyboard.press('Escape');
    await expect(rename).toHaveCount(0);

    // With no modal the palette opens again.
    await page.keyboard.press('Control+k');
    await expect(palette()).toBeVisible();
  });

  test('Palette: Tab stays inside, Home/End/PageDown move the highlight, Esc returns focus', async () => {
    await page.keyboard.press('Control+k');
    await expect(palette()).toBeVisible();
    await page.getByRole('combobox').fill('re');
    const chips = palette().getByRole('toolbar', { name: 'Filter hasil' }).getByRole('button');
    await page.keyboard.press('Tab');
    await expect(chips.first()).toBeFocused();
    await page.keyboard.press('ArrowRight');
    await expect(chips.nth(1)).toBeFocused();
    await page.keyboard.press('Tab');
    await expect(page.getByRole('combobox')).toBeFocused(); // wrapped, did not leave the palette
    await page.keyboard.press('Control+End');
    const options = palette().getByRole('option');
    await expect(options.last()).toHaveAttribute('aria-selected', 'true');
    await page.keyboard.press('Control+Home');
    await expect(options.first()).toHaveAttribute('aria-selected', 'true');
    await page.keyboard.press('Escape');
    await expect(palette()).toHaveCount(0);
    await expect(left().locator('[data-pane-list]')).toBeFocused();
  });

  test('Settings: Alt+letter, arrows and Ctrl+PageDown move between categories; Esc returns focus', async () => {
    await page.keyboard.press('Control+,');
    const settings = page.getByRole('dialog', { name: 'Settings' });
    await expect(settings).toBeVisible();
    await page.keyboard.press('Alt+t');
    await expect(settings.locator('.pHead h3')).toHaveText('Tabs');
    await expect(settings.getByRole('button', { name: 'Tabs' })).toBeFocused();
    await page.keyboard.press('ArrowDown');
    await expect(settings.getByRole('button', { name: 'File Operations' })).toBeFocused();
    await expect(settings.locator('.pHead h3')).toHaveText('File Operations');
    await page.keyboard.press('Control+PageDown');
    await expect(settings.locator('.pHead h3')).toHaveText('Preview');
    await expect(settings.locator(':focus')).toBeVisible();
    await page.keyboard.press('Control+PageUp');
    await expect(settings.locator('.pHead h3')).toHaveText('File Operations');
    // Back to Appearance: the Theme segmented control moves and selects with the arrows.
    await page.keyboard.press('Alt+a');
    await page.keyboard.press('ArrowRight'); // enter the first control of the section
    const dark = settings.getByRole('radio', { name: 'Dark' });
    await dark.focus();
    await page.keyboard.press('ArrowRight');
    await expect(settings.getByRole('radio', { name: 'Light' })).toBeChecked();
    await page.keyboard.press('Home');
    await expect(dark).toBeChecked();
    await page.keyboard.press('Escape');
    await expect(settings).toHaveCount(0);
    await expect(left().locator('[data-pane-list]')).toBeFocused();
  });

  test('Hotlist configure: Alt+I adds an item, Enter in a field is OK, Esc is staged', async () => {
    await page.keyboard.press('Control+d');
    await expect(page.getByRole('menu', { name: 'Directory Hotlist' })).toBeVisible();
    await page.keyboard.press('End');
    await page.keyboard.press('Enter');
    const dialog = page.getByRole('dialog', { name: 'Configure Directory Hotlist' });
    await expect(dialog).toBeVisible();
    await expect(dialog.getByRole('tree')).toBeFocused();

    // Esc leaves the import view first, the dialog on the second press.
    await page.keyboard.press('Alt+m');
    await expect(dialog.getByRole('region', { name: 'Import from Total Commander' })).toBeVisible();
    await page.keyboard.press('Escape');
    await expect(dialog.getByRole('region', { name: 'Import from Total Commander' })).toHaveCount(
      0,
    );
    await expect(dialog).toBeVisible();

    // Alt+I adds an item; it needs a title and a path, then Enter in the Title field is OK.
    await page.keyboard.press('Alt+i');
    const title = dialog.getByLabel('Title');
    await title.fill('Reports');
    await dialog.getByLabel('Path', { exact: true }).fill('C:\\Users\\demo\\Reports');
    expect((await calls(page, 'hotlist_save')).length).toBe(0);
    await title.press('Enter');
    await expect(dialog).toHaveCount(0);
    const saved = (await calls(page, 'hotlist_save')).at(-1)!.args as {
      doc: { items: { title: string }[] };
    };
    expect(saved.doc.items.map((i) => i.title)).toContain('Reports');
  });

  test('Search panel: Alt+F7 puts the cursor in the pattern, Esc closes, focus returns', async () => {
    await page.keyboard.press('Alt+F7');
    const panel = page.getByRole('complementary', { name: 'Search' });
    await expect(panel).toBeVisible();
    await expect(panel.getByLabel('Search pattern')).toBeFocused();
    await page.keyboard.press('Escape');
    await expect(panel).toHaveCount(0);
    await expect(left().locator('[data-pane-list]')).toBeFocused();
  });
});
