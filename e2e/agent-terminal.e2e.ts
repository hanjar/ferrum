import { test, expect, type Page, type BrowserContext } from '@playwright/test';

/**
 * FER-494 E2E: "Buka di terminal". The backend is a FAKE LAUNCHER: `agent_terminal_open` records
 * the request and answers with the argv the Rust side composes (`wt -d . <exe>`, folder as cwd),
 * nothing is started. Checked: command palette (one command per terminal-ready agent), the
 * context submenu "Buka di agent ▸" (folder and file), and the composer button (question goes to
 * the clipboard only, never into the request).
 */

function injectTauriMock(page: Page) {
  page.addInitScript(() => {
    const w = window as unknown as Record<string, unknown>;
    const BS = String.fromCharCode(92);
    const HOME = `C:${BS}Users${BS}demo`;
    const entries: Record<string, { size: number; isDir: boolean; kind: string }> = {
      'proj;a %PATH% ^&b é': { size: 0, isDir: true, kind: 'folder' },
      'q3-report.pdf': { size: 2_400_000, isDir: false, kind: 'pdf' },
      // FER-494 T1: holds an `.aider.conf.yml` (the fake launcher reports it like terminal.rs).
      'repo-with-aider-conf': { size: 0, isDir: true, kind: 'folder' },
    };
    w.__calls = [] as { cmd: string; args: unknown }[];
    w.__launches = [] as unknown[];
    const exe = `C:${BS}Users${BS}demo${BS}.local${BS}bin${BS}claude.exe`;
    const base = {
      vendor: 'x',
      acknowledged: true,
      broadRead: false,
      enabled: false,
      terminal: false,
    };
    const view = {
      agents: [
        {
          ...base,
          id: 'claude',
          name: 'Claude Code',
          status: 'supported',
          path: exe,
          enabled: true,
          terminal: true,
        },
        {
          ...base,
          id: 'codex',
          name: 'Codex',
          status: 'supported',
          path: `C:${BS}npm${BS}codex.exe`,
          broadRead: true,
          terminal: true,
          terminalNote: 'Codex dapat membaca file lain di PC ini.',
        },
        {
          ...base,
          id: 'omp',
          name: 'omp',
          status: 'unsupported',
          path: `C:${BS}L${BS}omp.exe`,
          reason: 'tidak didukung: tidak ada mode baca-saja.',
          terminal: true,
        },
        {
          ...base,
          id: 'aider',
          name: 'Aider',
          status: 'unsupported',
          path: `C:${BS}py${BS}aider.exe`,
          terminal: true,
        },
        {
          ...base,
          id: 'antigravity',
          name: 'Antigravity',
          status: 'unsupported',
          path: `C:${BS}L${BS}agy.exe`,
          terminalReason: 'folder dapat ditulis oleh Everyone',
        },
        {
          ...base,
          id: 'opencode',
          name: 'opencode',
          status: 'notFound',
          terminalReason: 'tidak ditemukan di PATH atau folder instalasi biasa',
        },
      ],
      defaultAgent: 'claude',
      terminalDefault: 'claude',
      detectedAtMs: 1,
      testProfile: false,
    };
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
        (w.__calls as { cmd: string; args: unknown }[]).push({ cmd, args });
        if (cmd === 'plugin:path|resolve_directory') return Promise.resolve(HOME);
        if (cmd === 'plugin:event|listen') return Promise.resolve(1);
        if (cmd === 'plugin:event|unlisten') return Promise.resolve(null);
        if (cmd === 'watch_start' || cmd === 'watch_stop') return Promise.resolve(1);
        if (cmd === 'list_dir')
          return Promise.resolve(
            Object.entries(entries).map(([name, f]) => ({
              name,
              path: `${HOME}${BS}${name}`,
              isDir: f.isDir,
              isSymlink: false,
              size: f.size,
              modifiedMs: 1700000000000,
              hidden: false,
            })),
          );
        if (cmd === 'list_drives' || cmd === 'list_operations') return Promise.resolve([]);
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
        if (cmd === 'ai_setup_load')
          return Promise.resolve({
            endpoint: 'http://127.0.0.1:20128/v1',
            model: 'test-model',
            timeoutMs: 30000,
            credentialRef: 'Ferrum/AI/custom',
            structured: 'none',
          });
        if (cmd === 'ai_test_connection') return Promise.resolve({ ok: true, kind: null });
        if (cmd === 'agent_cli_status') return Promise.resolve(view);
        if (cmd === 'ai_attachment_inspect') {
          const a = args as { paths: string[] };
          return Promise.resolve(
            a.paths.map((path) => {
              const name = path.split(BS).pop() ?? path;
              const f = entries[name];
              return {
                path,
                info: {
                  path,
                  name,
                  kind: f.kind,
                  size: f.size,
                  modifiedMs: 0,
                  inArchive: false,
                  support: 'metadata',
                },
              };
            }),
          );
        }
        if (cmd === 'agent_terminal_open') {
          // Fake launcher: record, compose like terminal.rs, start nothing.
          const req = (
            args as {
              request: {
                agent: string;
                path: string;
                program: string;
                confirmedUntrusted: boolean;
              };
            }
          ).request;
          (w.__launches as unknown[]).push(req);
          const agent = view.agents.find((a) => a.id === req.agent);
          if (!agent?.terminal)
            return Promise.reject({ kind: 'agent_unavailable', message: agent?.terminalReason });
          const isFile = !(entries[req.path.split(BS).pop() ?? '']?.isDir ?? true);
          const cwd = isFile ? req.path.slice(0, req.path.lastIndexOf(BS)) : req.path;
          const warnings = cwd.endsWith('repo-with-aider-conf')
            ? [
                `${agent.name} akan membaca ${cwd}${BS}.aider.conf.yml: bisa menyalakan yes-always / auto-test dan menjalankan perintah otomatis.`,
              ]
            : [];
          const ownTrust = agent.id === 'claude' || agent.id === 'codex';
          if (warnings.length && !ownTrust && !req.confirmedUntrusted)
            return Promise.resolve({
              agent: req.agent,
              argv: [],
              cwd,
              dryRun: false,
              needsConfirm: true,
              warnings,
            });
          return Promise.resolve({
            agent: req.agent,
            via: 'windowsTerminal',
            argv: [
              `C:${BS}Users${BS}demo${BS}AppData${BS}Local${BS}Microsoft${BS}WindowsApps${BS}wt.exe`,
              '-d',
              '.',
              agent.path,
            ],
            cwd,
            dryRun: false,
            note: agent.terminalNote,
            needsConfirm: false,
            warnings,
          });
        }
        return Promise.reject(new Error(`Unknown command: ${cmd}`));
      },
    };
  });
}

const FOLDER = 'proj;a %PATH% ^&b é';
const row = (page: Page, name: string) =>
  page.locator(`[data-pane="0"] .row[data-path$="${name}"]`).first();
const chat = (page: Page) => page.getByRole('complementary', { name: 'AI assistant' });
const launches = (page: Page) =>
  page.evaluate(() => (window as unknown as { __launches: Record<string, unknown>[] }).__launches);

test.describe('FER-494 Buka di terminal (fake launcher)', () => {
  let context: BrowserContext;
  let page: Page;

  test.beforeEach(async ({ browser }) => {
    context = await browser.newContext({ permissions: ['clipboard-read', 'clipboard-write'] });
    page = await context.newPage();
    injectTauriMock(page);
    await page.goto('/');
    await page.waitForSelector('[data-pane="0"] .row[data-path]', {
      state: 'attached',
      timeout: 10_000,
    });
  });

  test.afterEach(async () => {
    await context.close();
  });

  test('command palette: one "Buka <Agent> di sini" per ready agent, at the active folder', async () => {
    await row(page, 'q3-report.pdf').click();
    await page.keyboard.press('Control+K');
    await page.keyboard.type('terminal ai');
    const opts = page.getByRole('option', { name: /^Buka .* di sini/ });
    await expect(page.getByRole('option', { name: /Buka Claude Code di sini/ })).toBeVisible();
    await expect(page.getByRole('option', { name: /Buka omp di sini/ })).toBeVisible();
    await expect(page.getByRole('option', { name: /Buka Codex di sini/ })).toBeVisible();
    // ACL failed / not found: not offered.
    await expect(page.getByRole('option', { name: /Buka Antigravity di sini/ })).toHaveCount(0);
    await expect(page.getByRole('option', { name: /Buka opencode di sini/ })).toHaveCount(0);
    await expect(page.getByRole('option', { name: /Buka Aider di sini/ })).toBeVisible();
    expect(await opts.count()).toBe(4);
    await page.getByRole('option', { name: /Buka Claude Code di sini/ }).click();
    await expect.poll(() => launches(page)).toHaveLength(1);
    const [req] = await launches(page);
    expect(req).toEqual({
      agent: 'claude',
      path: 'C:\\Users\\demo',
      program: 'auto',
      confirmedUntrusted: false,
    });
    await expect(page.getByTestId('toast')).toContainText('Claude Code dibuka di terminal.');
  });

  test('context menu: "Buka di agent ▸" on a folder and on a file', async () => {
    await row(page, FOLDER).click({ button: 'right' });
    await page.getByRole('menuitem', { name: 'Buka di agent ▸' }).click();
    const items = page.getByRole('menu', { name: 'Context menu' }).getByRole('menuitem');
    await expect(items).toHaveText(['Buka di agent:', 'Claude Code', 'Codex', 'omp', 'Aider']);
    await page.getByRole('menuitem', { name: 'Codex' }).click();
    await expect.poll(() => launches(page)).toHaveLength(1);
    expect((await launches(page))[0]).toEqual({
      agent: 'codex',
      path: `C:\\Users\\demo\\${FOLDER}`,
      program: 'auto',
      confirmedUntrusted: false,
    });
    // Codex note: it can read other files.
    await expect(page.getByTestId('toast')).toContainText('Codex dapat membaca file lain');

    await row(page, 'q3-report.pdf').click({ button: 'right' });
    await page.getByRole('menuitem', { name: 'Buka di agent ▸' }).click();
    await page.getByRole('menuitem', { name: 'omp' }).click();
    await expect.poll(() => launches(page)).toHaveLength(2);
    // The file path goes to the backend, which opens its parent folder.
    expect((await launches(page))[1]).toEqual({
      agent: 'omp',
      path: 'C:\\Users\\demo\\q3-report.pdf',
      program: 'auto',
      confirmedUntrusted: false,
    });
  });

  test('composer: "Buka di terminal" copies the question, never sends it', async () => {
    await row(page, 'q3-report.pdf').click();
    await page.keyboard.press('Control+Shift+L');
    await expect(chat(page).getByTestId('open-in-terminal')).toHaveCount(0);
    await chat(page).getByTestId('chat-target').selectOption('claude');
    const question = 'ringkas laporan ini; lalu "hapus" & %PATH% ^ | wt -p x';
    await chat(page).getByRole('textbox', { name: 'Message to AI assistant' }).fill(question);
    const btn = chat(page).getByTestId('open-in-terminal');
    await expect(btn).toHaveText('Buka di terminal (Claude Code)');
    await btn.click();
    await expect.poll(() => launches(page)).toHaveLength(1);
    const [req] = await launches(page);
    expect(req).toEqual({
      agent: 'claude',
      path: 'C:\\Users\\demo\\q3-report.pdf',
      program: 'auto',
      confirmedUntrusted: false,
    });
    expect(JSON.stringify(await launches(page))).not.toContain('ringkas');
    // No other command carried the question either (nothing was asked or planned).
    const calls = await page.evaluate(() =>
      (window as unknown as { __calls: { cmd: string; args: unknown }[] }).__calls.filter((c) =>
        JSON.stringify(c.args ?? null).includes('ringkas'),
      ),
    );
    expect(calls).toEqual([]);
    await expect(page.getByTestId('toast')).toContainText(
      'Pertanyaan disalin — tempel di Claude Code (Ctrl+V)',
    );
    expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(question);
  });

  test('a folder with project configuration asks first (focus on Batal); Buka tetap opens', async () => {
    const folder = 'repo-with-aider-conf';
    const openAider = async () => {
      await row(page, folder).click({ button: 'right' });
      await page.getByRole('menuitem', { name: 'Buka di agent ▸' }).click();
      await page.getByRole('menuitem', { name: 'Aider' }).click();
    };
    await openAider();
    const dialog = page.getByRole('dialog', { name: 'agentTerminalConfirm' });
    await expect(dialog).toContainText('Buka Aider di folder ini?');
    await expect(dialog.getByTestId('agent-terminal-warnings')).toContainText('.aider.conf.yml');
    await expect(dialog.getByTestId('agent-terminal-warnings')).toContainText(
      'menjalankan perintah otomatis',
    );
    // Default focus: the safe choice.
    await expect(dialog.getByRole('button', { name: 'Batal' })).toBeFocused();
    expect(await launches(page)).toHaveLength(1);
    // Enter on the focused Batal cancels; nothing more is requested.
    await page.keyboard.press('Enter');
    await expect(dialog).toHaveCount(0);
    expect(await launches(page)).toHaveLength(1);

    await openAider();
    await dialog.getByRole('button', { name: 'Buka tetap' }).click();
    await expect.poll(() => launches(page)).toHaveLength(3);
    const reqs = await launches(page);
    expect(reqs[1]).toMatchObject({ agent: 'aider', confirmedUntrusted: false });
    expect(reqs[2]).toEqual({
      agent: 'aider',
      path: `C:\\Users\\demo\\${folder}`,
      program: 'auto',
      confirmedUntrusted: true,
    });
    await expect(page.getByTestId('toast')).toContainText('Aider dibuka di terminal.');

    // Claude has its own folder-trust dialog: opened at once, the finding is only a note.
    await row(page, folder).click({ button: 'right' });
    await page.getByRole('menuitem', { name: 'Buka di agent ▸' }).click();
    await page.getByRole('menuitem', { name: 'Claude Code' }).click();
    await expect.poll(() => launches(page)).toHaveLength(4);
    await expect(dialog).toHaveCount(0);
    await expect(page.getByTestId('toast')).toContainText('Catatan: Claude Code akan membaca');
  });
});
