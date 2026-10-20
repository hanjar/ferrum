import { expect, test, type Page } from '@playwright/test';

// FER-370 APP-05 — browser-mode E2E (vite webServer). Persistence across a
// real app restart needs the Tauri backend and is covered by the desktop
// run; here we verify apply-immediately semantics on the live document.

async function bootApp(page: Page) {
  await page.goto('/');
  await expect(page.locator('[data-pane="0"] .row[data-path]').first()).toBeVisible();
}

async function openSettings(page: Page) {
  await bootApp(page);
  await page.keyboard.press('Control+,');
  const dialog = page.getByRole('dialog', { name: 'Settings' });
  await expect(dialog).toBeVisible();
  return dialog;
}

test.describe('Settings → Appearance', () => {
  test('palette/mode/density/icons/font apply immediately to the document', async ({ page }) => {
    const dialog = await openSettings(page);

    await dialog.getByRole('radio', { name: 'Fresh Mint' }).click();
    await dialog.getByRole('radio', { name: 'Light' }).click();
    const html = page.locator('html');
    await expect(html).toHaveAttribute('data-palette', 'green');
    await expect(html).toHaveAttribute('data-theme', 'light');

    await dialog.getByRole('radio', { name: 'Comfortable' }).click();
    await expect(html).toHaveAttribute('data-density', 'comfortable');
    // `.row` alone also matches rows of other components — file rows carry
    // data-path (same hook the archive navigation E2E uses).
    const row = page.locator('.row[data-path]').first();
    await expect(row).toHaveCSS('height', '30px');

    await dialog.getByRole('radio', { name: 'Monochrome' }).click();
    await expect(html).toHaveAttribute('data-icon', 'monochrome');

    await dialog.getByLabel('Interface font size').selectOption('l');
    await expect(html).toHaveAttribute('data-ui', 'l');

    // Closing the dialog must not revert anything (apply-immediately contract).
    await dialog.getByTitle('Close').click();
    await expect(dialog).toBeHidden();
    await expect(html).toHaveAttribute('data-palette', 'green');
    await expect(html).toHaveAttribute('data-density', 'comfortable');
  });

  test('radiogroup keyboard: ArrowRight moves the color selection (radio semantics)', async ({
    page,
  }) => {
    const dialog = await openSettings(page);
    const violet = dialog.getByRole('radio', { name: 'Pop Violet' });
    await violet.click();
    await violet.press('ArrowRight');
    await expect(page.locator('html')).toHaveAttribute('data-palette', 'electric');
    // Focus followed the selection to Electric; the next key goes to the focused radio.
    await expect(dialog.getByRole('radio', { name: 'Electric' })).toBeFocused();
    await dialog.getByRole('radio', { name: 'Electric' }).press('ArrowLeft');
    await expect(page.locator('html')).toHaveAttribute('data-palette', 'violet');
  });

  test('context menu: Open Terminal Here on a folder wires through to the shell store', async ({
    page,
  }) => {
    await bootApp(page);
    const folder = page.locator('.row[data-path]', { hasText: 'archive-2025' }).first();
    await folder.click({ button: 'right' });
    await page.getByRole('menuitem', { name: 'Open Terminal Here' }).click();
    // Browser mode has no Tauri backend: the shell store surfaces the
    // unsupported error as a dialog — proving menu → store → IPC wiring.
    await expect(page.getByText('desktop app')).toBeVisible();
  });
});

test.describe('Settings → Appearance → Icon theme (FER-478)', () => {
  test('four themes apply immediately and combine with Monochrome', async ({ page }) => {
    const dialog = await openSettings(page);
    const group = dialog.getByRole('radiogroup', { name: 'Icon theme' });
    await expect(group.getByRole('radio')).toHaveText(['Line', 'Duotone', 'Label', 'Tile']);
    await expect(group.getByRole('radio', { name: 'Tile' })).toHaveAttribute(
      'aria-checked',
      'true',
    );
    const html = page.locator('html');
    for (const t of ['Line', 'Label', 'Tile', 'Duotone']) {
      await group.getByRole('radio', { name: t }).click();
      await expect(html).toHaveAttribute('data-icon-theme', t);
    }
    await dialog.getByRole('radio', { name: 'Monochrome' }).click();
    await group.getByRole('radio', { name: 'Label' }).click();
    await expect(html).toHaveAttribute('data-icon', 'monochrome');
    await expect(html).toHaveAttribute('data-icon-theme', 'Label');
  });
});
