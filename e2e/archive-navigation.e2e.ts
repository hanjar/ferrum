import { test, expect, type Page } from '@playwright/test';

/**
 * F2-12 E2E: archive virtual folder navigation via the webview.
 * Mocks Tauri IPC to serve a synthetic ZIP listing with real UI flow.
 */

function injectTauriMock(page: Page) {
  page.addInitScript(() => {
    (window as unknown as { __archiveInvocations: unknown[] }).__archiveInvocations = [];
    (window as unknown as { __extraFiles: unknown[] }).__extraFiles = [];
    window.__TAURI_INTERNALS__ = {
      // Minimal runtime surface: Titlebar reads metadata.currentWindow at
      // mount; without it the app crashes before the first listing.
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
        const ZIP_PATH = 'C:\\fixtures\\sample.zip';
        // Boot: resolve the home dir (BaseDirectory.Home = 21) so panes get
        // their initial path, and register event listeners.
        if (cmd === 'plugin:path|resolve_directory') {
          return Promise.resolve('C:\\Users\\demo');
        }
        if (cmd === 'plugin:event|listen') {
          return Promise.resolve(Math.floor(Math.random() * 1e9));
        }
        if (cmd === 'watch_start' || cmd === 'watch_stop') {
          return Promise.resolve(1);
        }
        if (cmd === 'list_dir') {
          const extras = (window as unknown as { __extraFiles: unknown[] }).__extraFiles || [];
          return Promise.resolve([
            ...extras,
            {
              name: 'sample.zip',
              path: ZIP_PATH,
              isDir: false,
              isSymlink: false,
              size: 1024,
              modifiedMs: 1700000000000,
              hidden: false,
            },
            {
              name: 'sample.rar',
              path: 'C:\\fixtures\\sample.rar',
              isDir: false,
              isSymlink: false,
              size: 2048,
              modifiedMs: 1700000000000,
              hidden: false,
            },
            {
              name: 'sample.7z',
              path: 'C:\\fixtures\\sample.7z',
              isDir: false,
              isSymlink: false,
              size: 4096,
              modifiedMs: 1700000000000,
              hidden: false,
            },
            {
              name: 'encrypted.zip',
              path: 'C:\\fixtures\\encrypted.zip',
              isDir: false,
              isSymlink: false,
              size: 512,
              modifiedMs: 1700000000000,
              hidden: false,
            },
            {
              name: 'clash.zip',
              path: 'C:\\fixtures\\clash.zip',
              isDir: false,
              isSymlink: false,
              size: 256,
              modifiedMs: 1700000000000,
              hidden: false,
            },
            {
              name: 'other.txt',
              path: 'C:\\fixtures\\other.txt',
              isDir: false,
              isSymlink: false,
              size: 5,
              modifiedMs: 1700000000000,
              hidden: false,
            },
          ]);
        }
        if (cmd === 'archive_list') {
          const loc = args?.location as { archivePath: string; memberPath: string };
          const entries = [
            { memberPath: 'docs', name: 'docs', isDir: true, size: 0, modifiedMs: 1700000000000 },
            {
              memberPath: 'docs/readme.md',
              name: 'readme.md',
              isDir: false,
              size: 100,
              modifiedMs: 1700000000000,
            },
            {
              memberPath: 'img.png',
              name: 'img.png',
              isDir: false,
              size: 2048,
              modifiedMs: 1700000000001,
            },
          ];
          const member = loc.memberPath ?? '';
          const filtered = entries
            .filter((e) => (member ? e.memberPath.startsWith(member + '/') : true))
            .map((e) => {
              const rest = member ? e.memberPath.slice(member.length + 1) : e.memberPath;
              return {
                memberPath: e.memberPath,
                name: rest.includes('/') ? rest.split('/')[0] : rest,
                isDir: rest.includes('/') || e.isDir,
                size: e.size,
                modifiedMs: e.modifiedMs,
              };
            });
          return Promise.resolve({
            location: loc,
            requestToken: 42,
            entries: filtered,
          });
        }
        if (cmd === 'archive_create') {
          const req = args?.request as { destinationPath: string };
          (window as unknown as { __archiveInvocations: unknown[] }).__archiveInvocations.push({
            cmd: 'archive_create',
            request: req,
          });
          return Promise.resolve(req.destinationPath);
        }
        if (cmd === 'archive_extract') {
          const req = args?.request as {
            archivePath: string;
            memberPaths: string[];
            destinationPath: string;
            password?: string | null;
          };
          (window as unknown as { __archiveInvocations: unknown[] }).__archiveInvocations.push({
            cmd: 'archive_extract',
            request: req,
          });
          if (req.archivePath.endsWith('encrypted.zip') && !req.password) {
            return Promise.reject({
              code: 'io',
              archiveCode: 'passwordRequired',
              message: 'Encrypted member requires a password',
            });
          }
          const extras = (window as unknown as { __extraFiles: unknown[] }).__extraFiles || [];
          extras.push({
            name: 'extracted-file.txt',
            path: `${req.destinationPath}\\extracted-file.txt`,
            isDir: false,
            isSymlink: false,
            size: 123,
            modifiedMs: 1700000000000,
            hidden: false,
          });
          return Promise.resolve(null);
        }
        if (cmd === 'archive_extract_preflight') {
          // FER-368 (D21): read-only conflict probe before an extract. The
          // synthetic archives have an empty destination, so no conflicts.
          // encrypted.zip models an archive this mock can NEVER unlock (the
          // typed password still lists locked, e.g. a wrong password), which
          // drives the fallback to direct extract.
          const req = args?.request as {
            archivePath: string;
            memberPaths: string[];
            destinationPath: string;
            password?: string | null;
          };
          (window as unknown as { __archiveInvocations: unknown[] }).__archiveInvocations.push({
            cmd: 'archive_extract_preflight',
            request: req,
          });
          if (req.archivePath.endsWith('encrypted.zip')) {
            return Promise.reject({
              code: 'io',
              archiveCode: 'passwordRequired',
              message: 'Encrypted member requires a password',
            });
          }
          if (req.archivePath.endsWith('clash.zip')) {
            return Promise.resolve({
              totalFiles: 1,
              totalBytes: 100,
              conflicts: [
                {
                  memberPath: 'docs/readme.md',
                  destinationPath: 'C:\\Users\\demo\\docs\\readme.md',
                },
              ],
            });
          }
          return Promise.resolve({ totalFiles: 3, totalBytes: 2148, conflicts: [] });
        }
        if (cmd === 'archive_test') {
          const req = args?.request as { archivePath: string };
          (window as unknown as { __archiveInvocations: unknown[] }).__archiveInvocations.push({
            cmd: 'archive_test',
            request: req,
          });
          return Promise.resolve({
            archivePath: req.archivePath,
            entries: [
              { memberPath: 'docs/readme.md', status: 'passed' },
              { memberPath: 'img.png', status: 'passed' },
            ],
          });
        }
        if (cmd === 'list_drives') {
          return Promise.resolve([
            { label: 'System', path: 'C:\\', freeBytes: 1e9, totalBytes: 5e11 },
          ]);
        }
        return Promise.reject(new Error(`Unknown command: ${cmd}`));
      },
    };
  });
}

type Invocation = { cmd: string; request: Record<string, unknown> };

async function invocations(page: Page): Promise<Invocation[]> {
  return page.evaluate(
    () => (window as unknown as { __archiveInvocations: Invocation[] }).__archiveInvocations,
  );
}

async function countInvocations(page: Page, cmd: string): Promise<number> {
  return (await invocations(page)).filter((i) => i.cmd === cmd).length;
}

test.describe('F2-12 archive navigation E2E', () => {
  let page: Page;

  test.beforeEach(async ({ browser }) => {
    page = await browser.newPage();
    injectTauriMock(page);
    await page.goto('/');
    // Wait for the file list to render.
    await page.waitForSelector('[data-path]', { state: 'attached', timeout: 10_000 });
  });

  test.afterEach(async () => {
    await page.close();
  });

  // Dual-pane app: scope every row lookup to the left pane, otherwise the
  // mirrored listing makes strict mode see the same path twice.
  const left = () => page.getByRole('region', { name: 'Left pane' });
  const right = () => page.getByRole('region', { name: 'Right pane' });
  const zipRow = () => left().locator('[data-path="C:\\\\fixtures\\\\sample.zip"]');

  test('shows sample.zip in the file list', async () => {
    const row = zipRow();
    await expect(row).toBeVisible();
    await expect(row).toContainText('sample.zip');
  });

  test('double-click on .zip enters virtual folder with archive entries', async () => {
    await zipRow().dblclick();

    // After entering the archive, the list shows the archive's root members.
    await left().locator('[data-path$="docs"]').waitFor({ state: 'attached', timeout: 5000 });
    const docsRow = left().locator('[data-path$="docs"]');
    await expect(docsRow).toBeVisible();
    await expect(docsRow).toContainText('docs');
  });

  test('status bar shows read-only when inside archive', async () => {
    await zipRow().dblclick();
    await left().locator('[data-path$="docs"]').waitFor({ state: 'attached', timeout: 5000 });

    // The status bar should indicate read-only.
    await expect(left().locator('.status')).toContainText('read-only');
  });

  test('navigating into archive subdirectory shows nested members', async () => {
    await zipRow().dblclick();
    await left().locator('[data-path$="docs"]').waitFor({ state: 'attached', timeout: 5000 });

    await left().locator('[data-path$="docs"]').dblclick();
    await left().locator('[data-path*="readme.md"]').waitFor({ state: 'attached', timeout: 5000 });
    await expect(left().locator('[data-path*="readme.md"]')).toContainText('readme.md');
  });

  test('navigating into a RAR shows the read-only status', async () => {
    await left().locator('[data-path$="sample.rar"]').dblclick();
    await left().locator('[data-path$="docs"]').waitFor({ state: 'attached', timeout: 5000 });
    await expect(left().locator('.status')).toContainText('read-only');
  });

  test('navigating into a 7z virtual folder lists its members', async () => {
    await left().locator('[data-path$="sample.7z"]').dblclick();
    await left().locator('[data-path$="docs"]').waitFor({ state: 'attached', timeout: 5000 });
    const docsRow = left().locator('[data-path$="docs"]');
    await expect(docsRow).toBeVisible();
    await expect(docsRow).toContainText('docs');
    await expect(left().locator('.status')).toContainText('read-only');
  });

  test('Alt+F5 packs the selection via keyboard (Enter submits)', async () => {
    await left().locator('[data-path$="other.txt"]').click();
    await page.keyboard.press('Alt+F5');
    const packDialog = page.getByRole('dialog', { name: 'pack' });
    await expect(packDialog).toBeVisible();
    await page.keyboard.press('Enter');
    await expect(packDialog).toBeHidden();
    const invocations = await page.evaluate(
      () =>
        (
          window as unknown as {
            __archiveInvocations: { cmd: string; request: Record<string, unknown> }[];
          }
        ).__archiveInvocations,
    );
    const create = invocations.find((i) => i.cmd === 'archive_create');
    expect(create).toBeDefined();
    expect(create?.request['format']).toBe('zip');
    expect(create?.request['sourcePaths']).toEqual(['C:\\fixtures\\other.txt']);
    // TC parity: packed archive lands in the active pane's directory.
    expect(String(create?.request['destinationPath'])).toContain('C:\\Users\\demo');
  });

  test('Escape cancels the pack dialog without invoking archive_create', async () => {
    await left().locator('[data-path$="other.txt"]').click();
    await page.keyboard.press('Alt+F5');
    await expect(page.getByRole('dialog', { name: 'pack' })).toBeVisible();
    await page.keyboard.press('Escape');
    await expect(page.getByRole('dialog', { name: 'pack' })).toBeHidden();
    const invocations = await page.evaluate(
      () => (window as unknown as { __archiveInvocations: unknown[] }).__archiveInvocations,
    );
    expect(invocations).toHaveLength(0);
  });

  test('Alt+F9 unpacks the selected archive into the OTHER pane', async () => {
    await left().locator('[data-path$="sample.zip"]').click();
    await page.keyboard.press('Alt+F9');
    await expect
      .poll(
        async () =>
          page.evaluate(
            () =>
              (
                window as unknown as {
                  __archiveInvocations: { cmd: string; request: Record<string, unknown> }[];
                }
              ).__archiveInvocations.filter((i) => i.cmd === 'archive_extract').length,
          ),
        { timeout: 5000 },
      )
      .toBe(1);
    const invocations = await page.evaluate(
      () =>
        (
          window as unknown as {
            __archiveInvocations: { cmd: string; request: Record<string, unknown> }[];
          }
        ).__archiveInvocations,
    );
    const extract = invocations.find((i) => i.cmd === 'archive_extract');
    expect(extract?.request['archivePath']).toBe('C:\\fixtures\\sample.zip');
    // memberPaths empty = full archive; target is the OTHER pane's path.
    expect(extract?.request['memberPaths']).toEqual([]);
    expect(extract?.request['destinationPath']).toBe('C:\\Users\\demo');
    // FER-368: the preflight ran first and the extract kept the safe policy.
    expect(invocations.filter((i) => i.cmd === 'archive_extract_preflight')).toHaveLength(1);
    expect(extract?.request['conflict']).toBe('skip');
  });

  test('encrypted archive unlocks the preflight, then extracts WITH the password', async () => {
    await left().locator('[data-path$="encrypted.zip"]').click();
    await page.keyboard.press('Alt+F9');
    const pwDialog = page.getByRole('dialog', { name: 'password' });
    await expect(pwDialog).toBeVisible();
    await page.keyboard.type('secret');
    await page.keyboard.press('Enter');
    await expect
      .poll(
        async () =>
          page.evaluate(
            () =>
              (
                window as unknown as {
                  __archiveInvocations: { cmd: string; request: Record<string, unknown> }[];
                }
              ).__archiveInvocations.filter((i) => i.cmd === 'archive_extract').length,
          ),
        { timeout: 5000 },
      )
      // FER-368 replaced the legacy double-extract retry: the direct extract
      // happens exactly once, WITH the password (preflight handles the rest).
      .toBe(1);
    const invocations = await page.evaluate(
      () =>
        (
          window as unknown as {
            __archiveInvocations: { cmd: string; request: Record<string, unknown> }[];
          }
        ).__archiveInvocations,
    );
    // FER-368 flow: preflight → password dialog → preflight retry (still
    // locked for this header-encrypted mock) → direct extract WITH the
    // password. The conflict ask never blocks the unlocked archive.
    const preflights = invocations.filter((i) => i.cmd === 'archive_extract_preflight');
    expect(preflights).toHaveLength(2);
    // FER-504: the wire carries `password: null` when absent (same as
    // archive_read/extract); the first probe unlocked nothing and the retry
    // carries the typed password.
    expect(preflights[0].request['password']).toBeNull();
    expect(preflights[1].request['password']).toBe('secret');
    const extracts = invocations.filter((i) => i.cmd === 'archive_extract');
    expect(extracts).toHaveLength(1);
    expect(extracts[0].request['password']).toBe('secret');
    await expect(pwDialog).toBeHidden();
  });

  test('Alt+Shift+F9 tests the selected archive and reports the result', async () => {
    await left().locator('[data-path$="sample.zip"]').click();
    await page.keyboard.press('Alt+Shift+F9');
    const report = page.getByRole('dialog', { name: 'testReport' });
    await expect(report).toBeVisible();
    await expect(report).toContainText('Archive test passed');
    await expect(report).toContainText('sample.zip');
    await expect(report).toContainText('Lulus');
    await expect(report).toContainText('dtk');
    const invocations = await page.evaluate(
      () =>
        (
          window as unknown as {
            __archiveInvocations: { cmd: string; request: Record<string, unknown> }[];
          }
        ).__archiveInvocations,
    );
    const tested = invocations.find((i) => i.cmd === 'archive_test');
    expect(tested?.request['archivePath']).toBe('C:\\fixtures\\sample.zip');
  });

  test('Alt+F9 extracts a RAR through the mock with the safe skip policy', async () => {
    await left().locator('[data-path$="sample.rar"]').click();
    await page.keyboard.press('Alt+F9');
    await expect.poll(() => countInvocations(page, 'archive_extract'), { timeout: 5000 }).toBe(1);
    const extract = (await invocations(page)).find((i) => i.cmd === 'archive_extract');
    expect(extract?.request['archivePath']).toBe('C:\\fixtures\\sample.rar');
    expect(extract?.request['destinationPath']).toBe('C:\\Users\\demo');
    expect(extract?.request['conflict']).toBe('skip');
  });

  test('Alt+Shift+F9 tests a RAR and lists it in the same report', async () => {
    await left().locator('[data-path$="sample.rar"]').click();
    await page.keyboard.press('Alt+Shift+F9');
    const report = page.getByRole('dialog', { name: 'testReport' });
    await expect(report).toBeVisible();
    await expect(report).toContainText('sample.rar');
    const tested = (await invocations(page)).find((i) => i.cmd === 'archive_test');
    expect(tested?.request['archivePath']).toBe('C:\\fixtures\\sample.rar');
  });

  test('pack to 7z from the command palette sends format sevenz', async () => {
    await left().locator('[data-path$="other.txt"]').click();
    await page.keyboard.press('Control+k');
    await page.keyboard.type('Pack to archive (7z)');
    await page.keyboard.press('Enter');
    const packDialog = page.getByRole('dialog', { name: 'pack' });
    await expect(packDialog).toBeVisible();
    await expect(packDialog).toContainText('SEVENZ');
    await page.keyboard.press('Enter');
    await expect(packDialog).toBeHidden();
    const create = (await invocations(page)).find((i) => i.cmd === 'archive_create');
    expect(create?.request['format']).toBe('sevenz');
    expect(create?.request['sourcePaths']).toEqual(['C:\\fixtures\\other.txt']);
  });

  test('extract with a name clash asks once and Extract runs with skip', async () => {
    await left().locator('[data-path$="clash.zip"]').click();
    await page.keyboard.press('Alt+F9');
    const ask = page.getByRole('dialog', { name: 'extractConflicts' });
    await expect(ask).toBeVisible();
    await expect(ask).toContainText('readme.md');
    expect(await countInvocations(page, 'archive_extract')).toBe(0);
    await ask.getByRole('button', { name: 'Extract' }).click();
    await expect(ask).toBeHidden();
    await expect.poll(() => countInvocations(page, 'archive_extract'), { timeout: 5000 }).toBe(1);
    const extract = (await invocations(page)).find((i) => i.cmd === 'archive_extract');
    expect(extract?.request['archivePath']).toBe('C:\\fixtures\\clash.zip');
    expect(extract?.request['conflict']).toBe('skip');
  });

  test('Escape on the extract conflict ask runs no extract', async () => {
    await left().locator('[data-path$="clash.zip"]').click();
    await page.keyboard.press('Alt+F9');
    const ask = page.getByRole('dialog', { name: 'extractConflicts' });
    await expect(ask).toBeVisible();
    await page.keyboard.press('Escape');
    await expect(ask).toBeHidden();
    expect(await countInvocations(page, 'archive_extract')).toBe(0);
  });

  test('F5 on an archive member extracts only that member to the other pane', async () => {
    await zipRow().dblclick();
    await left().locator('[data-path$="docs"]').waitFor({ state: 'attached', timeout: 5000 });
    await left().locator('[data-path$="docs"]').dblclick();
    await left().locator('[data-path*="readme.md"]').click();
    await page.keyboard.press('F5');
    await expect.poll(() => countInvocations(page, 'archive_extract'), { timeout: 5000 }).toBe(1);
    const extract = (await invocations(page)).find((i) => i.cmd === 'archive_extract');
    expect(extract?.request['archivePath']).toBe('C:\\fixtures\\sample.zip');
    expect(extract?.request['memberPaths']).toEqual(['docs/readme.md']);
    expect(extract?.request['destinationPath']).toBe('C:\\Users\\demo');
  });

  test('FER-53: archive breadcrumb navigation, read-only status in root and subfolder, and pane isolation', async () => {
    // Record initial right pane crumbs to verify isolation
    const initialRightCrumbs = await right().locator('.crumbs').textContent();

    // Enter sample.zip in left pane
    await zipRow().dblclick();
    await left().locator('[data-path$="docs"]').waitFor({ state: 'attached', timeout: 5000 });

    // Breadcrumb shows sample.zip with archive marker
    const leftCrumbs = left().locator('.crumbs');
    await expect(leftCrumbs).toContainText('sample.zip');
    const zipCrumb = leftCrumbs.locator('.crumb[data-archive="true"]');
    await expect(zipCrumb).toHaveText('sample.zip');

    // Status bar shows read-only in archive root
    await expect(left().locator('.status')).toContainText('read-only');

    // Enter docs subfolder inside archive
    await left().locator('[data-path$="docs"]').dblclick();
    await left().locator('[data-path*="readme.md"]').waitFor({ state: 'attached', timeout: 5000 });

    // Breadcrumb shows docs and status bar remains read-only
    await expect(leftCrumbs).toContainText('docs');
    await expect(left().locator('.status')).toContainText('read-only');

    // Right pane was not affected
    const currentRightCrumbs = await right().locator('.crumbs').textContent();
    expect(currentRightCrumbs).toBe(initialRightCrumbs);

    // Click disk segment in breadcrumbs (fixtures) to exit archive
    const fixturesCrumb = leftCrumbs.locator('.crumb', { hasText: 'fixtures' });
    await fixturesCrumb.click();

    // Returned to disk directory C:\fixtures
    await expect(zipRow()).toBeVisible();
    await expect(left().locator('.status')).not.toContainText('read-only');
  });

  test('FER-55: Alt+F9 extract automatically refreshes destination pane showing extracted file', async () => {
    // Destination pane (right) does not initially have extracted-file.txt
    await expect(right().locator('[data-path*="extracted-file.txt"]')).toHaveCount(0);

    // Unpack sample.zip from left pane into right pane
    await left().locator('[data-path$="sample.zip"]').click();
    await page.keyboard.press('Alt+F9');

    // Wait for archive_extract invocation
    await expect.poll(() => countInvocations(page, 'archive_extract'), { timeout: 5000 }).toBe(1);

    // Right pane automatically refreshed and displays extracted-file.txt without manual refresh
    await expect(right().locator('[data-path*="extracted-file.txt"]')).toBeVisible({
      timeout: 5000,
    });
  });
});
