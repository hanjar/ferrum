import { expect, test } from '@playwright/test';

// FER-370 APP-05 — browser-mode E2E (vite webServer). Persistence across a
// real app restart needs the Tauri backend and is covered by the desktop
// run; here we verify apply-immediately semantics on the live document.

test.describe('Settings → Appearance', () => {
  test('palette/mode/density/icons/font apply immediately to the document', async ({ page }) => {
    await page.goto('/');
    await page.keyboard.press('Control+,');
    const dialog = page.getByRole('dialog', { name: 'Settings' });
    await expect(dialog).toBeVisible();

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
    await page.goto('/');
    await page.keyboard.press('Control+,');
    const dialog = page.getByRole('dialog', { name: 'Settings' });
    const violet = dialog.getByRole('radio', { name: 'Pop Violet' });
    await violet.click();
    await violet.press('ArrowRight');
    await expect(page.locator('html')).toHaveAttribute('data-palette', 'electric');
    await violet.press('ArrowLeft');
    await expect(page.locator('html')).toHaveAttribute('data-palette', 'violet');
  });

  test('context menu: Open Terminal Here on a folder wires through to the shell store', async ({
    page,
  }) => {
    const logs: string[] = [];
    page.on('console', (m) => logs.push(`${m.type()}: ${m.text()}`));
    page.on('pageerror', (e) => logs.push(`pageerror: ${e.message}`));
    await page.goto('/');
    const folder = page.locator('.row[data-path]', { hasText: 'archive-2025' });
    await folder.click({ button: 'right' });
    console.log(
      'E2E-MENU',
      JSON.stringify(await page.locator('[role=menuitem]').allTextContents()),
    );
    await page.getByRole('menuitem', { name: 'Open Terminal Here' }).click();
    await page.waitForTimeout(800);
    console.log('E2E-DIAG', JSON.stringify(logs));
    // Browser mode has no Tauri backend: the shell store surfaces the
    // unsupported error as a dialog — proving menu → store → IPC wiring.
    await expect(page.getByText('desktop app')).toBeVisible();
  });
});

test.describe('Settings → Appearance → Icon theme (FER-478)', () => {
  test('four themes apply immediately and combine with Monochrome', async ({ page }) => {
    await page.goto('/');
    await page.keyboard.press('Control+,');
    const dialog = page.getByRole('dialog', { name: 'Settings' });
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
