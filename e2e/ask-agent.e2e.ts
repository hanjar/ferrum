import { test, expect, type Page } from '@playwright/test';

/**
 * FER-490 E2E: "Ask AI agent". A PDF attachment (metadata only for AI Ferrum) offers the default
 * agent; choosing an agent target, acknowledging its disclosure and sending streams the answer
 * with a status line; Batal cancels the run. The backend is a mock: `agent_ask` emits
 * `agent_cli://event` and resolves like the Rust command (the Rust suite runs the real flow
 * against a fake agent process, the real-window smoke does it in a test profile).
 */

function injectTauriMock(page: Page) {
  page.addInitScript(() => {
    const w = window as unknown as Record<string, unknown>;
    const BS = String.fromCharCode(92);
    const HOME = `C:${BS}Users${BS}demo`;
    const files: Record<string, { size: number; kind: string; support: string }> = {
      'q3-report.pdf': { size: 2_400_000, kind: 'pdf', support: 'metadata' },
      'notes.txt': { size: 1200, kind: 'text', support: 'content' },
    };
    w.__calls = [] as { cmd: string; args: unknown }[];
    w.__agentAsks = [] as unknown[];
    const handlers: Record<string, number[]> = {};
    let pending: { reject: (e: unknown) => void; id: string } | null = null;
    const agents = (ackClaude: boolean) => ({
      agents: [
        {
          id: 'claude',
          name: 'Claude Code',
          vendor: 'Anthropic',
          status: 'supported',
          path: `C:${BS}npm${BS}node_modules${BS}@anthropic-ai${BS}claude-code${BS}bin${BS}claude.exe`,
          version: '2.1.295 (Claude Code)',
          mode: 'claude -p (headless), tool hanya Read/Grep/Glob',
          acknowledged: ackClaude,
          broadRead: false,
          enabled: true,
        },
        {
          // FER-490 E2: broad read, not switched on -> never offered as a target.
          id: 'codex',
          name: 'Codex',
          vendor: 'OpenAI',
          status: 'supported',
          path: `C:${BS}npm${BS}codex.exe`,
          mode: 'codex exec --sandbox read-only',
          acknowledged: false,
          broadRead: true,
          enabled: false,
        },
        {
          id: 'omp',
          name: 'omp',
          vendor: 'x',
          status: 'unsupported',
          path: `C:${BS}L${BS}omp.exe`,
          reason: 'tidak didukung: tidak ada mode baca-saja.',
          acknowledged: false,
          broadRead: false,
          enabled: false,
        },
      ],
      defaultAgent: 'claude',
      detectedAtMs: 1,
      testProfile: false,
    });
    let acked = false;
    const emit = (payload: unknown) => {
      for (const id of handlers['agent_cli://event'] ?? []) {
        const cb = w[`_${id}`] as ((e: unknown) => void) | undefined;
        cb?.({ event: 'agent_cli://event', id: 1, payload });
      }
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
        if (cmd === 'plugin:event|listen') {
          const a = args as { event: string; handler: number };
          (handlers[a.event] ??= []).push(a.handler);
          return Promise.resolve(a.handler);
        }
        if (cmd === 'plugin:event|unlisten') return Promise.resolve(null);
        if (cmd === 'watch_start' || cmd === 'watch_stop') return Promise.resolve(1);
        if (cmd === 'list_dir')
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
        if (cmd === 'agent_cli_status') return Promise.resolve(agents(acked));
        if (cmd === 'agent_cli_acknowledge') {
          acked = true;
          return Promise.resolve(agents(acked));
        }
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
                  kind: f.kind,
                  size: f.size,
                  modifiedMs: 0,
                  inArchive: false,
                  support: f.support,
                  ...(f.support === 'metadata'
                    ? { note: 'isi tidak didukung (metadata saja)' }
                    : {}),
                },
              };
            }),
          );
        }
        if (cmd === 'agent_ask') {
          const a = args as { request: { requestId: string; question: string } };
          (w.__agentAsks as unknown[]).push(a);
          const id = a.request.requestId;
          const hold = a.request.question.includes('lama');
          return new Promise((resolve, reject) => {
            setTimeout(
              () => emit({ requestId: id, kind: 'status', text: 'agent sedang membaca file…' }),
              50,
            );
            setTimeout(
              () => emit({ requestId: id, kind: 'text', text: 'Membaca q3-report.pdf.' }),
              150,
            );
            if (hold) {
              pending = { reject, id };
              return;
            }
            setTimeout(
              () =>
                resolve({
                  agent: 'claude',
                  answer:
                    'Laporan Q3: pendapatan naik **12%**. <img src=x onerror=alert(1)> [klik](https://evil.example)',
                  truncated: false,
                  skipped: [{ path: `${HOME}${BS}secret${BS}.env`, reason: 'sensitive' }],
                }),
              900,
            );
          });
        }
        if (cmd === 'ai_cancel_plan') {
          const a = args as { requestId: string };
          if (pending && pending.id === a.requestId) {
            pending.reject({ kind: 'cancelled' });
            pending = null;
          }
          return Promise.resolve(null);
        }
        return Promise.reject(new Error(`Unknown command: ${cmd}`));
      },
    };
  });
}

const row = (page: Page, name: string) =>
  page.locator(`[data-pane="0"] .row[data-path$="${name}"]`).first();
const chat = (page: Page) => page.getByRole('complementary', { name: 'AI assistant' });

async function ask(page: Page, text: string) {
  await chat(page).getByRole('textbox', { name: 'Message to AI assistant' }).fill(text);
  await chat(page).getByRole('button', { name: 'Send' }).click();
}

test.describe('FER-490 Ask AI agent (mock IPC)', () => {
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

  test('a PDF offers the agent; the answer streams with status and stays text', async () => {
    await row(page, 'q3-report.pdf').click();
    await page.keyboard.press('Control+Shift+L');
    const offer = chat(page).getByTestId('agent-offer');
    await expect(offer).toContainText('tidak bisa dibaca utuh oleh AI Ferrum');
    await offer.getByRole('button', { name: 'Tanya lewat Claude Code?' }).click();
    const target = chat(page).getByTestId('chat-target');
    await expect(target).toHaveValue('claude');
    // Unsupported agents are not offered as a target.
    await expect(target.locator('option')).toHaveText(['AI Ferrum', 'Agent: Claude Code']);

    // One-time disclosure before anything can be sent.
    const disclosure = chat(page).getByTestId('agent-disclosure');
    await expect(disclosure).toContainText('dikirim ke layanan Anthropic');
    await expect(disclosure).toContainText('akun Anda');
    const send = chat(page).getByRole('button', { name: 'Send' });
    await chat(page).getByRole('textbox', { name: 'Message to AI assistant' }).fill('ringkas');
    await expect(send).toBeDisabled();
    await disclosure.getByRole('button', { name: 'Mengerti' }).click();
    await expect(disclosure).toHaveCount(0);

    await ask(page, 'ringkas laporan ini');
    const answer = chat(page).getByTestId('chat-answer');
    await expect(chat(page).getByTestId('agent-status')).toContainText('agent sedang membaca');
    await expect(answer).toContainText('Membaca q3-report.pdf.');
    await expect(answer).toContainText('pendapatan naik');
    await expect(chat(page).getByTestId('agent-status')).toHaveCount(0);
    await expect(chat(page).getByTestId('answer-agent')).toContainText('Claude Code');
    // E1: what the snapshot left out is named under the answer.
    await expect(chat(page).getByTestId('agent-skipped')).toContainText(
      'Tidak disalin ke agent: .env (terlihat sensitif)',
    );
    // Text only: no link, no image, no HTML from the agent.
    await expect(answer.locator('a, img')).toHaveCount(0);
    await expect(answer).toContainText('<img src=x onerror=alert(1)>');

    const asks = await page.evaluate(
      () => (window as unknown as { __agentAsks: unknown[] }).__agentAsks,
    );
    expect(asks).toHaveLength(1);
    const req = (asks[0] as { request: Record<string, unknown> }).request;
    expect(req.agent).toBe('claude');
    expect(Object.keys(req).sort()).toEqual([
      'agent',
      'attachments',
      'history',
      'question',
      'requestId',
    ]);
    const cmds = await page.evaluate(() =>
      (window as unknown as { __calls: { cmd: string }[] }).__calls.map((c) => c.cmd),
    );
    for (const forbidden of ['ai_ask', 'ai_plan_intent', 'ai_exec_reserve', 'enqueue_delete'])
      expect(cmds, forbidden).not.toContain(forbidden);
  });

  test('Batal cancels a running agent', async () => {
    await row(page, 'notes.txt').click();
    await page.keyboard.press('Control+K');
    await page.keyboard.type('tanya agent');
    await page
      .getByRole('option', { name: /Ask AI agent about selection/ })
      .first()
      .click();
    await expect(chat(page).getByTestId('chat-target')).toHaveValue('claude');
    await chat(page)
      .getByTestId('agent-disclosure')
      .getByRole('button', { name: 'Mengerti' })
      .click();
    await ask(page, 'proses lama');
    await expect(chat(page).getByTestId('chat-answer')).toContainText('Membaca q3-report.pdf.');
    await chat(page).getByTestId('agent-status').getByRole('button', { name: 'Batal' }).click();
    await expect(chat(page).getByTestId('chat-answer')).toContainText('Dibatalkan.');
    await expect(chat(page).getByTestId('chat-answer')).not.toContainText('Membaca q3');
    const cancels = await page.evaluate(() =>
      (window as unknown as { __calls: { cmd: string }[] }).__calls.filter(
        (c) => c.cmd === 'ai_cancel_plan',
      ),
    );
    expect(cancels).toHaveLength(1);
  });
});
