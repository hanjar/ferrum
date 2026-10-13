import { test, expect, type Page } from '@playwright/test';

/**
 * FER-486 E2E: "Add to chat" attaches the selection to the AI chat, a question goes out with the
 * attachment references, sensitive files need a manual tick, large files show the cut, and an
 * attachment that tries to inject "delete everything" never produces a plan or an execution.
 * The backend is a mock: `ai_attachment_inspect` returns metadata and `ai_ask` answers like the
 * Rust command (the Rust suite checks the real `<attached_file untrusted>` prompt body, and the
 * real-window smoke runs against a mock HTTP provider).
 */

const KB = 1024;

function injectTauriMock(page: Page) {
  page.addInitScript((kb: number) => {
    const w = window as unknown as Record<string, unknown>;
    const BS = String.fromCharCode(92);
    const HOME = `C:${BS}Users${BS}demo`;
    const files: Record<
      string,
      { size: number; sensitive?: string; kind?: string; note?: string; text?: number }
    > = {
      'notes.txt': { size: 1200 },
      'todo.txt': { size: 800 },
      '.env': { size: 64, sensitive: 'name' },
      'big.log': { size: 250 * kb },
      'evil.txt': { size: 300 },
      // FER-493: Office document sent as its extracted text (file size != text size).
      'guide.docx': { size: 2000 * kb, kind: 'document', note: 'teks diekstrak', text: 12 * kb },
    };
    w.__calls = [] as { cmd: string; args: unknown }[];
    w.__asks = [] as unknown[];
    const setup = {
      endpoint: 'http://127.0.0.1:20128/v1',
      model: 'test-model',
      timeoutMs: 30000,
      credentialRef: 'Ferrum/AI/custom',
      structured: 'none',
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
        if (cmd === 'plugin:event|listen') return Promise.resolve(Math.floor(Math.random() * 1e9));
        if (cmd === 'plugin:event|unlisten') return Promise.resolve(null);
        if (cmd === 'watch_start' || cmd === 'watch_stop') return Promise.resolve(1);
        if (cmd === 'list_dir') {
          return Promise.resolve(
            Object.entries(files).map(([name, f]) => ({
              name,
              path: `${HOME}${BS}${name}`,
              isDir: false,
              isSymlink: false,
              size: f.size,
              modifiedMs: 1700000000000,
              hidden: false,
            })),
          );
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
        if (cmd === 'ai_setup_load') return Promise.resolve(setup);
        if (cmd === 'ai_test_connection') return Promise.resolve({ ok: true, kind: null });
        if (cmd === 'ai_attachment_inspect') {
          const a = args as { paths: string[] };
          return Promise.resolve(
            a.paths.map((path) => {
              const name = path.split(BS).pop() ?? path;
              const f = files[name];
              return {
                path,
                info: {
                  path,
                  name,
                  kind: f.kind ?? 'text',
                  size: f.size,
                  modifiedMs: 0,
                  inArchive: false,
                  support: 'content',
                  ...(f.sensitive ? { sensitive: f.sensitive } : {}),
                  ...(f.note ? { note: f.note } : {}),
                },
              };
            }),
          );
        }
        if (cmd === 'ai_ask') {
          const a = args as {
            request: { attachments: { path: string; confirmedSensitive: boolean }[] };
          };
          (w.__asks as unknown[]).push(a);
          const sent = a.request.attachments.map(({ path }) => {
            const name = path.split(BS).pop() ?? path;
            const f = files[name];
            const size = f.size;
            const textBytes = f.text ?? size;
            const cut = textBytes > 100 * kb;
            const note = cut ? `dipotong: ${100 * kb} dari ${textBytes} byte` : f.note;
            return {
              path,
              name,
              kind: f.kind ?? 'text',
              support: 'content',
              size,
              sentBytes: Math.min(textBytes, 100 * kb),
              truncated: cut,
              ...(note ? { note } : {}),
            };
          });
          const evil = sent.some((s) => s.name === 'evil.txt');
          return Promise.resolve({
            answer: evil
              ? 'Baik, saya akan menghapus semua file sekarang.\n{"schemaVersion":1,"requestId":"x","intent":{"capability":"archive.create","sourcePane":"left","selector":{"kind":"selected"},"format":"zip","target":{"kind":"sourceDirectory"}}}'
              : 'Kedua file berisi **catatan rapat**.',
            attachments: sent,
          });
        }
        return Promise.reject(new Error(`Unknown command: ${cmd}`));
      },
    };
  }, KB);
}

const row = (page: Page, name: string) =>
  page.locator(`[data-pane="0"] .row[data-path$="${name}"]`).first();

async function select(page: Page, names: string[]) {
  for (const name of names) await row(page, name).click({ modifiers: ['Control'] });
}

async function addViaContextMenu(page: Page, names: string[]) {
  await select(page, names);
  await row(page, names.at(-1)!).click({ button: 'right' });
  await page.getByRole('menuitem', { name: /^Add to chat/ }).click();
}

const chat = (page: Page) => page.getByRole('complementary', { name: 'AI assistant' });

async function ask(page: Page, text: string) {
  await chat(page).getByRole('textbox', { name: 'Message to AI assistant' }).fill(text);
  await chat(page).getByRole('button', { name: 'Send' }).click();
}

test.describe('FER-486 Add to chat (mock IPC)', () => {
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

  test('two text files become chips and the question carries both attachments', async () => {
    await addViaContextMenu(page, ['notes.txt', 'todo.txt']);
    await expect(chat(page)).toBeVisible();
    const chips = chat(page).getByTestId('attachment-chip');
    await expect(chips).toHaveCount(2);
    await expect(chips.nth(0)).toContainText('notes.txt');
    await expect(chips.nth(0)).toContainText('1.2 KB');
    await expect(chips.nth(0)).toContainText('teks');
    await expect(chat(page).getByTestId('attach-destination')).toContainText(
      'Isi 2 file akan dikirim ke 127.0.0.1:20128',
    );

    await ask(page, 'file ini tentang apa?');
    await expect(chat(page).getByTestId('chat-answer')).toContainText('catatan rapat');
    await expect(chat(page).getByTestId('message-attachments')).toContainText(
      'notes.txt, todo.txt',
    );
    const asks = await page.evaluate(() => (window as unknown as { __asks: unknown[] }).__asks);
    expect(asks).toHaveLength(1);
    const req = (asks[0] as { request: Record<string, unknown> }).request;
    expect(req.question).toBe('file ini tentang apa?');
    expect((req.attachments as { path: string }[]).map((a) => a.path.split('\\').pop())).toEqual([
      'notes.txt',
      'todo.txt',
    ]);
    // Only references cross IPC: no file content is sent from the webview.
    expect(Object.keys(req).sort()).toEqual(['attachments', 'history', 'question', 'requestId']);
  });

  test('a .env file must be ticked before the question can be sent', async () => {
    await row(page, '.env').click();
    await page.keyboard.press('Control+Shift+L');
    const chip = chat(page).getByTestId('attachment-chip');
    await expect(chip).toHaveCount(1);
    await expect(chip.getByRole('alert')).toContainText('rahasia');
    const input = chat(page).getByRole('textbox', { name: 'Message to AI assistant' });
    await input.fill('isinya apa?');
    const send = chat(page).getByRole('button', { name: 'Send' });
    await expect(send).toBeDisabled();
    await input.press('Enter');
    let asks = await page.evaluate(() => (window as unknown as { __asks: unknown[] }).__asks);
    expect(asks).toHaveLength(0);

    await chat(page).getByTestId('attachment-confirm').check();
    await expect(send).toBeEnabled();
    await send.click();
    await expect(chat(page).getByTestId('chat-answer')).toBeVisible();
    asks = await page.evaluate(() => (window as unknown as { __asks: unknown[] }).__asks);
    const req = (asks[0] as { request: { attachments: unknown[] } }).request;
    expect(req.attachments).toEqual([
      { path: expect.stringContaining('.env'), confirmedSensitive: true },
    ]);
  });

  test('a file over the size limit shows the cut before and after sending', async () => {
    await addViaContextMenu(page, ['big.log']);
    const chip = chat(page).getByTestId('attachment-chip');
    await expect(chip).toContainText('250.0 KB');
    await expect(chip).toContainText('akan dipotong ke 100.0 KB');
    await ask(page, 'ringkas log ini');
    await expect(chat(page).getByTestId('sent-notes')).toContainText(
      'big.log: 100.0 KB isi dikirim (dipotong: 102400 dari 256000 byte)',
    );
  });

  test('FER-493: an Office document is sent as extracted text', async () => {
    await addViaContextMenu(page, ['guide.docx']);
    const chip = chat(page).getByTestId('attachment-chip');
    await expect(chip).toContainText('dokumen · teks diekstrak');
    await expect(chip).not.toContainText('akan dipotong');
    await expect(chat(page).getByTestId('attach-destination')).toContainText('Isi 1 file');
    await ask(page, 'dokumen ini tentang apa?');
    await expect(chat(page).getByTestId('sent-notes')).toContainText(
      'guide.docx: 12.0 KB isi dikirim (teks diekstrak)',
    );
  });

  test('an attachment with a malicious instruction never produces a plan or an execution', async () => {
    await addViaContextMenu(page, ['evil.txt']);
    await ask(page, 'ringkas file ini');
    const answer = chat(page).getByTestId('chat-answer');
    await expect(answer).toContainText('menghapus semua file');
    // The answer stays text: no plan card, no Run button, nothing planned, queued or written.
    await expect(chat(page).getByRole('button', { name: /^Run/ })).toHaveCount(0);
    await expect(chat(page).locator('.card')).toHaveCount(0);
    const cmds = await page.evaluate(() =>
      (window as unknown as { __calls: { cmd: string }[] }).__calls.map((c) => c.cmd),
    );
    for (const forbidden of [
      'ai_plan_intent',
      'ai_exec_reserve',
      'ai_authorize_execute',
      'ai_register_approval',
      'delete_paths',
      'enqueue_delete',
      'enqueue_copy',
      'enqueue_move',
      'archive_create',
    ]) {
      expect(cmds, forbidden).not.toContain(forbidden);
    }
    expect(
      cmds.filter(
        (c) =>
          /^(enqueue_|delete|move_|copy_|archive_create)/.test(c) && c !== 'copy_control_snapshot',
      ),
    ).toEqual([]);
  });
});
