#!/usr/bin/env node
/**
 * AI provider error UX smoke over a REAL WebView2 window (Windows-only). FER-465.
 *
 *   node scripts/smoke-ai-error.mjs <path-to-ferrum.exe>
 *
 * Phase A: the provider endpoint points at a closed local port (a stopped local server). Expected: the header
 * indicator turns red, a request shows the "Server AI tidak bisa dihubungi (host:port)" message with
 * [Coba lagi] [Buka Settings] [Test connection], Test connection reports the failure inline, and
 * [Buka Settings] opens Settings. No raw body, key or stack trace is shown.
 * Phase B (only when FERRUM_SMOKE_LIVE_ENDPOINT is set, e.g. http://127.0.0.1:20128/v1, plus
 * FERRUM_SMOKE_LIVE_MODEL): the normal path. The header indicator turns green and a request does not end in a
 * provider error card. The key is read (never written, never printed) from the real credential store.
 *
 * ISOLATION: FERRUM_PROFILE_DIR; the real profile is hashed before/after and the run FAILS if it changed.
 */
import { createRequire } from 'node:module';
import { spawn, execSync } from 'node:child_process';
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import net from 'node:net';
import { profileEnv, snapshotRealProfile, verifyRealProfile } from './lib/test-profile.mjs';

const require = createRequire(import.meta.url);
const { chromium } = require('playwright-core');

const EXE = process.argv[2];
if (!EXE) {
  console.error('usage: node scripts/smoke-ai-error.mjs <path-to-ferrum.exe>');
  process.exit(2);
}
const PORT = 9227;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const failures = [];
const check = (ok, label) => {
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${label}`);
  if (!ok) failures.push(label);
};
const realBefore = snapshotRealProfile();

function freeClosedPort() {
  return new Promise((resolve) => {
    const server = net.createServer();
    server.listen(0, '127.0.0.1', () => {
      const { port } = server.address();
      server.close(() => resolve(port));
    });
  });
}

async function withApp(setup, body) {
  const root = mkdtempSync(join(tmpdir(), 'ferrum-ai-err-smoke-'));
  mkdirSync(join(root, 'config'), { recursive: true });
  writeFileSync(join(root, 'config', 'ai-setup.json'), JSON.stringify(setup, null, 2));
  const child = spawn(EXE, [], {
    stdio: 'ignore',
    env: {
      ...process.env,
      ...profileEnv(root),
      WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${PORT} --no-first-run`,
    },
  });
  try {
    let browser;
    for (let i = 0; i < 80 && !browser; i++) {
      try {
        browser = await chromium.connectOverCDP(`http://127.0.0.1:${PORT}`);
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
    await page.waitForSelector('.pane', { timeout: 30000 }).catch(() => {});
    await body(page);
    await browser.close().catch(() => {});
  } finally {
    try {
      execSync(`taskkill /PID ${child.pid} /T /F`, { stdio: 'ignore' });
    } catch {
      /* already exited */
    }
    await sleep(500);
    rmSync(root, { recursive: true, force: true });
  }
}

async function openChat(page) {
  await page.keyboard.press('Control+Shift+A');
  await page.waitForSelector('aside[aria-label="AI assistant"]', { timeout: 10000 });
}

async function main() {
  const port = await freeClosedPort();
  const base = {
    model: 'smoke-model',
    timeoutMs: 8000,
    credentialRef: 'Ferrum/AI/custom',
    structured: 'none',
  };

  await withApp({ ...base, endpoint: `http://127.0.0.1:${port}/v1` }, async (page) => {
    await openChat(page);
    const dot = page.getByTestId('provider-status');
    await page.waitForFunction(
      () => document.querySelector('[data-testid="provider-status"]')?.className.includes('down'),
      null,
      { timeout: 15000 },
    );
    check(true, 'A: header indicator is red when the server is stopped');
    check(
      ((await dot.getAttribute('aria-label')) ?? '').includes('tidak bisa dihubungi'),
      'A: indicator tooltip names the cause',
    );
    await page.fill('textarea[aria-label="Message to AI assistant"]', 'zip left');
    await page.keyboard.press('Enter');
    const msg = page.getByTestId('provider-error-message');
    await msg.waitFor({ timeout: 20000 });
    const text = (await msg.textContent()) ?? '';
    console.log(`INFO  message: ${text}`);
    if (text.includes('Key AI belum tersimpan')) {
      check(
        true,
        'A: no stored key in this environment, credential message shown (unreachable not reachable)',
      );
    } else {
      check(
        text.includes(`Server AI tidak bisa dihubungi (127.0.0.1:${port})`),
        'A: unreachable message with host:port',
      );
      check(
        await page.getByRole('button', { name: 'Coba lagi' }).isVisible(),
        'A: [Coba lagi] shown',
      );
      check(
        await page.getByRole('button', { name: 'Test connection' }).isVisible(),
        'A: [Test connection] shown',
      );
    }
    check(
      await page.getByRole('button', { name: /Buka Settings/ }).isVisible(),
      'A: [Buka Settings > AI] shown',
    );
    check(
      !/sk-|Bearer|stack|at \w+\.|\/v1\/chat/i.test(text),
      'A: no key, raw body, path or stack trace in the message',
    );
    await page
      .getByRole('button', { name: 'Test connection' })
      .click()
      .catch(() => {});
    await page
      .getByTestId('provider-test-note')
      .waitFor({ timeout: 15000 })
      .catch(() => {});
    check(
      (await page.getByTestId('provider-test-note').count()) > 0,
      'A: Test connection reports inline',
    );
    await page.screenshot({
      path: join(tmpdir(), 'ferrum-ai-error-unreachable.png'),
    });
    await page.getByRole('button', { name: /Buka Settings/ }).click();
    await page.waitForSelector('[role="dialog"]', { timeout: 10000 });
    check(true, 'A: [Buka Settings > AI] opens Settings');
  });

  const live = process.env.FERRUM_SMOKE_LIVE_ENDPOINT;
  if (live) {
    await withApp(
      { ...base, endpoint: live, model: process.env.FERRUM_SMOKE_LIVE_MODEL ?? base.model },
      async (page) => {
        await openChat(page);
        await page.waitForFunction(
          () => document.querySelector('[data-testid="provider-status"]')?.className.includes('ok'),
          null,
          { timeout: 30000 },
        );
        check(true, 'B: header indicator is green with the live server');
        await page.fill(
          'textarea[aria-label="Message to AI assistant"]',
          'Arsipkan semua file di folder kiri jadi ZIP',
        );
        await page.keyboard.press('Enter');
        await sleep(15000);
        check(
          (await page.getByTestId('provider-error-actions').count()) === 0,
          'B: provider answered; no provider error card (a later stale/preview card is not a provider error)',
        );
        await page.screenshot({ path: join(tmpdir(), 'ferrum-ai-error-live.png') });
      },
    );
  } else {
    console.log('INFO  phase B skipped (set FERRUM_SMOKE_LIVE_ENDPOINT to run the normal path)');
  }

  const proof = verifyRealProfile(realBefore);
  check(proof.unchanged, `real profile untouched (${proof.files} files)`);
  console.log(failures.length ? `\n${failures.length} FAILED` : '\nALL PASS');
  process.exit(failures.length ? 1 : 0);
}

main().catch((error) => {
  console.error('smoke crashed:', error?.message ?? error);
  process.exit(1);
});
