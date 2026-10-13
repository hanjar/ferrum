#!/usr/bin/env node
/**
 * FER-490 "Ask AI agent" smoke over a REAL WebView2 window (Windows-only).
 *
 *   node scripts/smoke-ask-agent.mjs <path-to-ferrum.exe>
 *
 * The app runs on a temp FERRUM_PROFILE_DIR. In a test profile Ferrum never detects or starts a
 * real agent: `config/agent-cli-test.json` names FAKE agents (node + fake-agent.mjs), so no model
 * is called and no quota is used. Checks:
 *   1. Settings → AI → Agent CLI shows the test profile and the fake agent's full path;
 *   2. a PDF attachment offers "Tanya lewat Claude Code?", the disclosure must be acknowledged,
 *      the answer streams with a status line;
 *   3. the real backend started the fake with the read-only flags, the question and paths only on
 *      STDIN, no FERRUM_* variable, cwd inside data\agent-runs (MODE:report);
 *   4. Batal kills the agent AND its grandchild (pids from MODE:sleep);
 *   5. an agent printing an intent/HTML/link still gives text only (no plan card, no link);
 *   6. data\agent-runs is empty afterwards; the real profile is unchanged.
 */
import { createRequire } from 'node:module';
import { spawn, execSync } from 'node:child_process';
import { mkdtempSync, mkdirSync, writeFileSync, rmSync, existsSync, readdirSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { profileEnv, snapshotRealProfile, verifyRealProfile } from './lib/test-profile.mjs';

const require = createRequire(import.meta.url);
const { chromium } = require('playwright-core');

const EXE = process.argv[2];
if (!EXE) {
  console.error('usage: node scripts/smoke-ask-agent.mjs <path-to-ferrum.exe>');
  process.exit(2);
}
const HERE = dirname(fileURLToPath(import.meta.url));
const FAKE = resolve(
  HERE,
  '..',
  'src-tauri',
  'src',
  'ai',
  'agent_cli',
  'testdata',
  'fake-agent.mjs',
);
const CDP_PORT = Number(process.env.FERRUM_SMOKE_CDP_PORT ?? 9249);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const failures = [];
const check = (ok, label) => {
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${label}`);
  if (!ok) failures.push(label);
};
const realBefore = snapshotRealProfile();

function alive(pid) {
  try {
    const out = execSync(`tasklist /FI "PID eq ${pid}" /NH`, { encoding: 'utf8' });
    return out.includes(` ${pid} `);
  } catch {
    return false;
  }
}

async function main() {
  const root = mkdtempSync(join(tmpdir(), 'ferrum-fer490-smoke-'));
  const fix = join(root, 'files');
  mkdirSync(fix, { recursive: true });
  writeFileSync(join(fix, 'q3-report.pdf'), '%PDF-1.4\n% dummy\n');
  writeFileSync(join(fix, 'notes.txt'), 'Rapat Senin.');
  mkdirSync(join(fix, 'proj'), { recursive: true });
  writeFileSync(join(fix, 'proj', 'readme.md'), 'Proyek.');
  writeFileSync(join(fix, 'proj', '.env'), 'DB_PASSWORD=supersecretvalue');
  mkdirSync(join(root, 'config'), { recursive: true });
  writeFileSync(
    join(root, 'config', 'agent-cli-test.json'),
    JSON.stringify({
      agents: [
        { id: 'claude', program: process.execPath, script: FAKE },
        { id: 'codex', program: process.execPath, script: FAKE },
      ],
    }),
  );
  writeFileSync(
    join(root, 'config', 'config.json'),
    JSON.stringify({
      theme: 'dark',
      showHidden: true,
      session: {
        panes: [
          { tabs: [fix], current: 0 },
          { tabs: [fix], current: 0 },
        ],
      },
    }),
  );
  const child = spawn(EXE, [], {
    stdio: 'ignore',
    env: {
      ...process.env,
      ...profileEnv(root),
      WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${CDP_PORT} --no-first-run`,
    },
  });
  const grandPids = [];
  try {
    let browser;
    for (let i = 0; i < 80 && !browser; i++) {
      try {
        browser = await chromium.connectOverCDP(`http://127.0.0.1:${CDP_PORT}`);
      } catch {
        await sleep(500);
      }
    }
    if (!browser) throw new Error('could not attach to the app window');
    let page;
    for (let i = 0; i < 40 && !page; i++) {
      page = browser.contexts()[0]?.pages()[0];
      if (!page) await sleep(250);
    }
    const row = (name) => page.locator(`[data-pane="0"] .row[data-path$="\\\\${name}"]`).first();
    await row('notes.txt').waitFor({ timeout: 30000 });

    // 1. Settings → AI → Agent CLI
    await page.keyboard.press('Control+K');
    await page.keyboard.type('AI assistant settings');
    await page.keyboard.press('Enter');
    const settings = page.getByTestId('agent-cli-settings');
    await settings.waitFor({ timeout: 15000 });
    await page.getByTestId('agent-cli-test-profile').waitFor({ timeout: 15000 });
    check(true, '1: settings say the test profile is active');
    const claudeRow = settings.locator('[data-agent="claude"]');
    await claudeRow.getByTestId('agent-path').waitFor({ timeout: 15000 });
    const rowText = (await claudeRow.textContent()) ?? '';
    check(
      rowText.includes('fake-agent.mjs') && rowText.includes('node.exe'),
      '1: fake agent path shown',
    );
    check(rowText.includes('fake-agent 1.0.0'), '1: version read with --version');
    check(rowText.includes('Mode baca-saja: claude -p'), '1: read-only mode shown');
    const ompRow = (await settings.locator('[data-agent="omp"]').textContent()) ?? '';
    check(ompRow.includes('profil uji'), `1: real omp not detected in the test profile`);
    // E2: Codex is broad-read, OFF until the red opt-in is ticked.
    const codexRow = settings.locator('[data-agent="codex"]');
    const codexText = (await codexRow.textContent()) ?? '';
    check(codexText.includes('baca luas — nonaktif'), '1: Codex off by default (baca luas)');
    check(
      codexText.includes('Codex dapat membaca file lain di PC ini; isi bisa terkirim ke OpenAI'),
      '1: red broad-read warning shown',
    );
    const defaults = await page.getByTestId('agent-default').locator('option').allTextContents();
    check(JSON.stringify(defaults) === '["Claude Code"]', `1: default list ${defaults}`);
    await codexRow.getByTestId('agent-enable').check();
    await page.waitForFunction(
      () =>
        document
          .querySelector('[data-agent="codex"]')
          ?.textContent.includes('baca luas (diaktifkan)'),
      null,
      { timeout: 10000 },
    );
    check(true, '1: Codex enabled through the explicit opt-in');
    await page.keyboard.press('Escape');
    await sleep(300);

    // 2. PDF offer, disclosure, streaming
    await row('q3-report.pdf').click();
    await page.keyboard.press('Control+Shift+L');
    const chat = page.locator('aside[aria-label="AI assistant"]');
    const input = chat.locator('textarea[aria-label="Message to AI assistant"]');
    await chat.getByTestId('agent-offer').waitFor({ timeout: 15000 });
    await chat.getByRole('button', { name: 'Tanya lewat Claude Code?' }).click();
    check(
      (await chat.getByTestId('chat-target').inputValue()) === 'claude',
      '2: target switched to Claude Code',
    );
    await chat.getByTestId('agent-disclosure').waitFor({ timeout: 5000 });
    await input.fill('apa isi file ini?');
    check(
      await chat.getByRole('button', { name: 'Send' }).isDisabled(),
      '2: Send needs the disclosure',
    );
    await chat.getByTestId('agent-disclosure').getByRole('button', { name: 'Mengerti' }).click();
    await chat.getByRole('button', { name: 'Send' }).click();
    await chat.getByTestId('agent-status').waitFor({ timeout: 10000 });
    check(true, '2: status line while the agent works');
    const answers = chat.getByTestId('chat-answer');
    await page.waitForFunction(
      () =>
        document.querySelector('[data-testid="chat-answer"]')?.getAttribute('aria-busy') ===
        'false',
      null,
      { timeout: 30000 },
    );
    const a1 = (await answers.last().textContent()) ?? '';
    check(a1.includes('Jawaban palsu: 1 lampiran (q3-report.pdf)'), `2: answer shown`);

    // 3. flags / stdin / env as seen by the agent process
    await input.fill('MODE:report cek-91c2');
    await input.press('Enter');
    await page.waitForFunction(
      () => document.querySelectorAll('[data-testid="chat-answer"][aria-busy="false"]').length >= 2,
      null,
      { timeout: 30000 },
    );
    const rep = (await answers.last().textContent()) ?? '';
    check(rep.includes('REPORT agent=claude'), '3: report received');
    for (const flag of [
      '-p',
      '--restricted',
      '--strict-mcp-config',
      '--safe-mode',
      '--no-session-persistence',
    ])
      check(rep.includes(` ${flag}`), `3: flag ${flag}`);
    check(!rep.includes('--add-dir'), '3: no --add-dir (E1: agent reads only the copy)');
    check(rep.includes('questionInArgv=false'), '3: question not on the command line');
    check(rep.includes('fileInArgv=false'), '3: file path not on the command line');
    check(rep.includes('ferrumEnv=false'), '3: no FERRUM_* variable reaches the agent');
    check(rep.includes('cwdInRuns=true'), '3: cwd is a fresh folder under agent-runs');

    // Codex flavour through the target select.
    await chat.getByTestId('chat-target').selectOption('codex');
    await chat.getByTestId('agent-disclosure').getByRole('button', { name: 'Mengerti' }).click();
    await input.fill('MODE:report cek-codex');
    await input.press('Enter');
    await page.waitForFunction(
      () => document.querySelectorAll('[data-testid="chat-answer"][aria-busy="false"]').length >= 3,
      null,
      { timeout: 30000 },
    );
    const repc = (await answers.last().textContent()) ?? '';
    check(
      repc.includes('REPORT agent=codex') &&
        repc.includes('--sandbox') &&
        !repc.includes('--add-dir'),
      '3: codex started with --sandbox read-only and no --add-dir',
    );
    await chat.getByTestId('chat-target').selectOption('claude');

    // 3b. E1: the agent's working folder holds only the copy of the attachment.
    const done = (n) =>
      page.waitForFunction(
        (k) =>
          document.querySelectorAll('[data-testid="chat-answer"][aria-busy="false"]').length >= k,
        n,
        { timeout: 30000 },
      );
    await input.fill('MODE:tree');
    await input.press('Enter');
    await done(4);
    const t1 = (await answers.last().textContent()) ?? '';
    check(
      t1.includes('TREE files|files\\q3-report.pdf|tmp') && !t1.includes('notes.txt'),
      `3b: only the copy is visible (${t1.trim()})`,
    );
    // 3c. a folder: secrets inside are not copied and are named under the answer.
    await chat.getByRole('button', { name: 'Hapus semua' }).click();
    await row('proj').click();
    await page.keyboard.press('Control+Shift+L');
    await page.waitForFunction(
      () =>
        [...document.querySelectorAll('[data-testid="attachment-chip"]')].some(
          (c) => c.textContent.includes('proj') && !c.textContent.includes('memeriksa'),
        ),
      null,
      { timeout: 10000 },
    );
    await input.fill('MODE:tree folder');
    await input.press('Enter');
    await done(5);
    const t2 = /TREE \S*/.exec((await answers.last().textContent()) ?? '')?.[0] ?? '';
    check(
      t2.includes('files\\proj\\readme.md') && !t2.includes('.env'),
      `3c: folder copied without .env (${t2.trim()})`,
    );
    const sk = (await chat.getByTestId('agent-skipped').last().textContent()) ?? '';
    check(sk.includes('.env (terlihat sensitif)'), `3c: skipped item named: ${sk.trim()}`);

    // 4. Batal kills the process tree
    await input.fill('MODE:sleep');
    await input.press('Enter');
    await page.waitForFunction(
      () =>
        [...document.querySelectorAll('[data-testid="chat-answer"]')]
          .at(-1)
          ?.textContent.includes('PIDS'),
      null,
      { timeout: 20000 },
    );
    const pidText = (await answers.last().textContent()) ?? '';
    const m = /PIDS (\d+) (\d+)/.exec(pidText);
    if (m) grandPids.push(Number(m[1]), Number(m[2]));
    check(grandPids.length === 2 && grandPids.every(alive), '4: agent and grandchild running');
    await chat.getByTestId('agent-status').getByRole('button', { name: 'Batal' }).click();
    await page.waitForFunction(
      () =>
        [...document.querySelectorAll('[data-testid="chat-answer"]')]
          .at(-1)
          ?.textContent.includes('Dibatalkan.'),
      null,
      { timeout: 15000 },
    );
    await sleep(800);
    check(grandPids.length === 2 && !grandPids.some(alive), '4: Batal killed the whole tree');

    // 5. hostile agent output stays text
    await input.fill('MODE:evil');
    await input.press('Enter');
    await page.waitForFunction(
      () =>
        [...document.querySelectorAll('[data-testid="chat-answer"]')]
          .at(-1)
          ?.textContent.includes('EVIL'),
      null,
      { timeout: 20000 },
    );
    const last = answers.last();
    check((await last.locator('a, img, script').count()) === 0, '5: no link/HTML rendered');
    await sleep(1000);
    check((await chat.locator('section.card').count()) === 0, '5: no plan card');
    check((await chat.getByRole('button', { name: /^Run/ }).count()) === 0, '5: no Run button');
    await browser.close().catch(() => {});
  } finally {
    try {
      execSync(`taskkill /PID ${child.pid} /T /F`, { stdio: 'ignore' });
    } catch {
      /* already exited */
    }
    await sleep(800);
    const runs = join(root, 'data', 'agent-runs');
    const leftover = existsSync(runs) ? readdirSync(runs) : [];
    check(leftover.length === 0, `6: agent-runs empty (${leftover.join(', ')})`);
    rmSync(root, { recursive: true, force: true });
    const proof = verifyRealProfile(realBefore);
    check(proof.unchanged, `6: real profile unchanged (${proof.files} files hashed)`);
  }
}

main()
  .catch((e) => {
    console.error(`ERROR ${e?.message ?? e}`);
    failures.push('exception');
  })
  .finally(() => {
    console.log(failures.length ? `\n${failures.length} FAILED` : '\nALL PASS');
    process.exit(failures.length ? 1 : 0);
  });
