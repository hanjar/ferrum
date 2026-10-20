import { test, expect, type Page } from '@playwright/test';

/**
 * FER-466 E2E (mock IPC + mock provider): the AI answers "paling gede itu apa" from the Disk Usage
 * result that is on screen. The provider is the mocked `ai_plan_intent` command (no real key). The
 * test checks the card shows the right file, that what crossed to the provider holds no scan data,
 * and that no write command ran.
 */

function injectTauriMock(page: Page) {
  page.addInitScript(() => {
    const BS = String.fromCharCode(92);
    const GIB = 1024 * 1024 * 1024;
    const ROOT = `D:${BS}Data`;
    const BIG = `${ROOT}${BS}Media${BS}holiday-2026.mkv`;
    const w = window as unknown as Record<string, unknown>;
    const handlers: ((e: { event: string; id: number; payload: unknown }) => void)[] = [];
    const calls: { cmd: string; args: unknown }[] = [];
    w.__calls = calls;
    w.__plans = [] as unknown[];

    const node = (id: string, name: string, over: Record<string, unknown> = {}) => ({
      id,
      path: id,
      name,
      childrenIds: [],
      directFiles: 0,
      directSymlinks: 0,
      directLogicalBytes: 0,
      directAllocatedBytes: null,
      subtreeFiles: 1,
      subtreeDirs: 0,
      subtreeSymlinks: 0,
      subtreeLogicalBytes: 0,
      subtreeAllocatedBytes: null,
      subtreeErrors: 0,
      skippedVolumeBoundaries: 0,
      subtreeSkippedVolumes: 0,
      ...over,
    });
    const rootNode = node(ROOT, 'Data', {
      subtreeFiles: 2,
      subtreeDirs: 1,
      subtreeLogicalBytes: 3 * GIB + 2048,
    });
    const stats = {
      scannedFiles: 2,
      scannedDirs: 2,
      scannedSymlinks: 0,
      skippedVolumeBoundaries: 0,
      logicalBytes: 3 * GIB + 2048,
      allocatedAvailableFiles: 0,
      allocatedUnavailableFiles: 2,
      knownAllocatedBytes: null,
      totalErrors: 0,
      coverage: 'complete',
      cancelled: false,
      depthCapped: false,
      durationMs: 5,
    };
    const fileItem = (id: string, name: string, size: number) => ({
      id,
      parentId: id.slice(0, id.lastIndexOf(BS)),
      name,
      logicalBytes: size,
      allocatedBytes: null,
      allocatedSupported: false,
      category: name.endsWith('.mkv') ? 'video' : 'document',
      percentOfParent: 99,
      percentOfScanRoot: 99,
    });

    window.__TAURI_INTERNALS__ = {
      metadata: {
        currentWindow: { label: 'main' },
        currentWebview: { windowLabel: 'main', label: 'main' },
      },
      plugins: {},
      transformCallback: (cb: unknown) => {
        const id = Math.floor(Math.random() * 1e9);
        w[`_${id}`] = cb;
        return id;
      },
      invoke: (cmd, args) => {
        calls.push({ cmd, args });
        if (cmd === 'plugin:path|resolve_directory')
          return Promise.resolve(`C:${BS}Users${BS}demo`);
        if (cmd === 'plugin:event|listen') {
          const a = (args ?? {}) as { event?: string; handler?: number };
          if (a.event === 'disk_usage://progress' && typeof a.handler === 'number') {
            handlers.push(w[`_${a.handler}`] as (typeof handlers)[number]);
          }
          return Promise.resolve(Math.floor(Math.random() * 1e9));
        }
        if (cmd === 'plugin:event|unlisten') return Promise.resolve(null);
        if (cmd === 'watch_start' || cmd === 'watch_stop') return Promise.resolve(1);
        if (cmd === 'list_dir') {
          return Promise.resolve([
            {
              name: 'README.md',
              path: `C:${BS}Users${BS}demo${BS}README.md`,
              isDir: false,
              isSymlink: false,
              size: 10,
              modifiedMs: 1700000000000,
              hidden: false,
            },
          ]);
        }
        if (cmd === 'list_drives') return Promise.resolve([]);
        if (cmd === 'list_operations') return Promise.resolve([]);
        if (cmd === 'load_prefs')
          return Promise.resolve({
            theme: 'dark',
            palette: 'violet',
            showHidden: true,
            terminal: 'auto',
            paneSplit: 0.5,
            session: null,
          });
        if (cmd === 'save_prefs') return Promise.resolve(null);
        if (cmd === 'ai_setup_load') {
          return Promise.resolve({
            endpoint: 'http://127.0.0.1:20128/v1',
            model: 'test-model',
            timeoutMs: 30000,
            credentialRef: 'Ferrum/AI/custom',
            structured: 'none',
          });
        }
        if (cmd === 'ai_test_connection') return Promise.resolve({ ok: true, kind: null });
        if (cmd === 'ai_plan_intent') {
          const a = args as { request: { requestId: string; capabilities?: string[] } };
          (w.__plans as unknown[]).push(JSON.parse(JSON.stringify(a)));
          // FER-573: Disk Usage closed -> the app offers `ui.hint`; the mock model points to it.
          if (a.request.capabilities?.includes('ui.hint'))
            return Promise.resolve({ kind: 'hint', hint: 'openDiskUsage' });
          return Promise.resolve({
            schemaVersion: 1,
            requestId: a.request.requestId,
            intent: { capability: 'diskUsage.query', query: { kind: 'largestFiles', limit: 3 } },
          });
        }
        if (cmd === 'disk_usage_start') {
          setTimeout(() => {
            for (const h of handlers) {
              h({
                event: 'disk_usage://progress',
                id: 1,
                payload: {
                  scanId: 'scan-1',
                  scannedFiles: 2,
                  scannedDirs: 2,
                  scannedSymlinks: 0,
                  knownLogicalBytes: 3 * GIB + 2048,
                  knownAllocatedBytes: null,
                  allocatedAvailableFiles: 0,
                  allocatedUnavailableFiles: 2,
                  coverage: 'complete',
                  errors: [],
                  totalErrors: 0,
                  topChildren: [],
                  done: true,
                  cancelled: false,
                  durationMs: 5,
                },
              });
            }
          }, 30);
          return Promise.resolve('scan-1');
        }
        if (cmd === 'disk_usage_snapshot') {
          return Promise.resolve({
            scanId: 'scan-1',
            rootPath: ROOT,
            rootId: ROOT,
            version: 1,
            scannedAtMs: 1700000000000,
            rootNode,
            stats,
          });
        }
        if (cmd === 'disk_usage_query_children') {
          return Promise.resolve({
            scanId: 'scan-1',
            nodeId: ROOT,
            version: 1,
            scannedAtMs: 1700000000000,
            metric: 'logical',
            coverage: 'complete',
            offset: 0,
            limit: 50,
            total: 0,
            items: [],
          });
        }
        if (cmd === 'disk_usage_top_files') {
          const a = args as { limit: number };
          const items = [
            fileItem(BIG, 'holiday-2026.mkv', 3 * GIB),
            fileItem(`${ROOT}${BS}notes.txt`, 'notes.txt', 2048),
          ].slice(0, a.limit);
          return Promise.resolve({
            scanId: 'scan-1',
            nodeId: ROOT,
            version: 1,
            scannedAtMs: 1700000000000,
            metric: 'logical',
            coverage: 'complete',
            limit: a.limit,
            totalMatching: 2,
            truncated: false,
            label: `Top ${a.limit}`,
            items,
          });
        }
        return Promise.reject(new Error(`Unknown command: ${cmd}`));
      },
    };
  });
}

const chat = (page: Page) => page.getByRole('complementary', { name: 'AI assistant' });
const workspace = (page: Page) => page.getByTestId('du-workspace');

async function ask(page: Page, text: string) {
  await chat(page).getByRole('textbox', { name: 'Message to AI assistant' }).fill(text);
  await chat(page).getByRole('button', { name: 'Send' }).click();
}

async function openChat(page: Page) {
  await page.getByRole('button', { name: 'AI assistant', exact: true }).click();
  await expect(chat(page)).toBeVisible();
}

const calls = (page: Page) =>
  page.evaluate(() =>
    (window as unknown as { __calls: { cmd: string }[] }).__calls.map((c) => c.cmd),
  );

test.describe('FER-466 AI diskUsage.query (mock IPC, mock provider)', () => {
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

  async function openAndScan() {
    await page.locator('button[title="Disk usage (Alt+Shift+D)"]').first().click();
    await expect(workspace(page)).toBeVisible();
    await workspace(page).getByLabel('Path root scan disk usage').fill('D:\\Data');
    await workspace(page).getByRole('button', { name: 'Scan', exact: true }).click();
    await expect(workspace(page).locator('.chip')).toHaveText('Complete', { timeout: 5_000 });
  }

  test('"paling gede itu apa" lists the biggest file from the scan and sends no scan data', async () => {
    await openAndScan();
    await openChat(page);
    await ask(page, 'paling gede itu apa ya');
    const card = chat(page).getByTestId('du-answer');
    await expect(card).toBeVisible();
    await expect(card.getByTestId('du-item').first()).toContainText('holiday-2026.mkv');
    await expect(card.getByTestId('du-item').first()).toContainText('3.00 GB');
    await expect(card.getByTestId('du-item').first()).toContainText('Media\\holiday-2026.mkv');

    // The provider got the capability in its list and nothing from the scan.
    const plans = await page.evaluate(() => (window as unknown as { __plans: unknown[] }).__plans);
    expect(plans).toHaveLength(1);
    const sent = JSON.stringify(plans);
    expect(sent).toContain('diskUsage.query');
    for (const leak of ['holiday', 'Media', 'D:\\\\Data', 'notes.txt', '3221225472']) {
      expect(sent, leak).not.toContain(leak);
    }

    // Read-only: nothing was written, queued or planned.
    const ran = await calls(page);
    expect(
      ran.filter(
        (c) =>
          /^(enqueue_|delete|move_|copy_|archive_create|ai_exec_|ai_authorize)/.test(c) &&
          !['copy_control_snapshot', 'ai_exec_unresolved'].includes(c),
      ),
    ).toEqual([]);
  });

  test('FER-573: a size question while Disk Usage is closed points to it, with a button that opens it', async () => {
    await openChat(page);
    await ask(page, 'berapa ukuran size semua folder dan file di panel kanan?');
    await expect(
      chat(page).getByText(/Buka Disk Usage \(Alt\+Shift\+D\) di panel itu/),
    ).toBeVisible();
    await expect(workspace(page)).toBeHidden();
    const plans = await page.evaluate(
      () => (window as unknown as { __plans: { request: { capabilities: string[] } }[] }).__plans,
    );
    expect(plans).toHaveLength(1);
    expect(plans[0].request.capabilities).toContain('ui.hint');
    expect(plans[0].request.capabilities).not.toContain('diskUsage.query');
    await chat(page)
      .getByRole('button', { name: /Buka Disk Usage/ })
      .click();
    await expect(workspace(page)).toBeVisible();
    // Open now: the button is gone (it would only close Disk Usage).
    await expect(chat(page).getByRole('button', { name: /Buka Disk Usage/ })).toHaveCount(0);
  });

  test('without any scan the AI says so and offers to open Disk Usage', async () => {
    await page.locator('button[title="Disk usage (Alt+Shift+D)"]').first().click();
    await expect(workspace(page)).toBeVisible();
    await openChat(page);
    await ask(page, 'paling gede itu apa ya');
    const card = chat(page).getByTestId('du-answer');
    await expect(card.getByTestId('du-message')).toHaveText(
      'Belum ada hasil Disk Usage. Jalankan scan dulu.',
    );
    await expect(card.getByTestId('du-item')).toHaveCount(0);
    await expect(card.getByTestId('du-open')).toBeVisible();
  });
});
