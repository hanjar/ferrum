#!/usr/bin/env node
// Opt-in desktop smoke for the read-only Activity query in chat (ACT-08 / FER-235). NOT part of `npm test`:
// it drives the REAL Tauri window through WebView2 remote debugging and calls the configured provider
// (same pattern as scripts/ai-desktop-smoke.mjs; prerequisites in docs/ai/desktop-smoke.md).
//
//   1. start the app with remote debugging:
//        $env:FERRUM_PROFILE_DIR = "$env:TEMP/ferrum-smoke-profile"; $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = '--remote-debugging-port=9222'; ferrum.exe
//   2. provider configured (docs/ai/provider.md).
//   3. node scripts/ai-desktop-smoke-activity.mjs     (FERRUM_CDP overrides http://127.0.0.1:9222)
//
// The user's QUESTIONS reach the provider (as for any chat message); answers stay local.
//
// WHAT THIS CAN AND CANNOT PROVE. The webview's IPC (`__TAURI_INTERNALS__.invoke`, `ipc.postMessage`) is
// non-writable and does not appear in the CDP network log, so provider payloads and the list of invoked commands
// cannot be observed from here. "No history in any provider payload" and "no mutating command" are proven by unit
// tests (src/lib/ai/executorActivity.test.ts, src-tauri/src/ipc_activity_chat_tests.rs), NOT by this script.
// What is observable and checked: the model picks the read intent, the answer card appears without a Run button,
// the query leaves the sidebar Recent counters unchanged, find-by-name lists the folder, and "Buka di panel"
// navigates a pane. Exit code 0 = every check passed.
import { createRequire } from 'node:module';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const require = createRequire(import.meta.url);
const { chromium } = require('playwright-core');

const CDP = process.env.FERRUM_CDP ?? 'http://127.0.0.1:9222';
// The fixture is created under the user's home and removed at the end. Run the app with an ISOLATED activity
// profile (FERRUM_PROFILE_DIR=<temp dir>, set in step 1) so the first-run disclosure state and history are test data:
// this script clicks "Mengerti" itself and must never do that on a real profile.
const FOLDER = process.env.FERRUM_ACT_SMOKE_FOLDER ?? 'ferrum-act08-smoke-tmp';
const ROOT = path.join(os.homedir(), FOLDER);
const Q_FREQUENT = 'Folder apa yang baru-baru ini saya buka dalam 7 hari terakhir?';
const Q_FIND = `Cari folder bernama ${FOLDER} di riwayat aktivitas saya`;

const failures = [];
const check = (ok, label, detail = '') => {
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${label}${detail ? ` — ${detail}` : ''}`);
  if (!ok) failures.push(label);
};

async function main() {
  // Created BEFORE the page (re)loads so the pane listing includes the fixture.
  fs.rmSync(ROOT, { recursive: true, force: true });
  fs.mkdirSync(path.join(ROOT, 'alpha'), { recursive: true });
  fs.mkdirSync(path.join(ROOT, 'beta'), { recursive: true });
  const browser = await chromium.connectOverCDP(CDP);
  const page = browser.contexts()[0].pages()[0];
  check(
    await page.evaluate(() => '__TAURI_INTERNALS__' in window),
    'running inside the real Tauri webview',
  );
  await page.reload();
  await page.waitForSelector('section', { timeout: 20_000 });
  await page.waitForTimeout(1500);

  if (
    !(await page
      .locator('aside.chat')
      .isVisible()
      .catch(() => false))
  ) {
    await page.keyboard.press('Control+Shift+A');
    await page.waitForTimeout(600);
  }
  const chat = page.locator('aside.chat');
  check(await chat.isVisible(), 'Ctrl+Shift+A opens the AI panel');

  // Seed local history: visit two fixture folders through the pane (real navigation => real receipts).
  // The chat panel can overlap the panes: close it while navigating, reopen afterwards.
  if (await chat.isVisible().catch(() => false)) {
    await page.keyboard.press('Control+Shift+A');
    await page.waitForTimeout(500);
  }
  const pane = page.locator('section').first();
  const enter = async (name) => {
    await pane.locator(`.row[data-path$="${name}"]`).first().dblclick({ timeout: 10_000 });
    await page.waitForTimeout(700);
  };
  // Whatever the restored location is: climb to the drive root, then descend Users/<name> to the home folder.
  for (let step = 0; step < 14 && (await pane.locator('.crumb').count()) > 1; step++) {
    await pane.locator('button[title="Parent folder"]').click({ timeout: 5000 });
    await page.waitForTimeout(500);
  }
  for (const segment of path.relative(path.parse(os.homedir()).root, os.homedir()).split(path.sep))
    await enter(segment);
  await enter(FOLDER);
  await enter('alpha');
  await pane.locator('button[title="Parent folder"]').click();
  await page.waitForTimeout(600);
  await enter('beta');
  await page.waitForTimeout(1500);
  if (!(await chat.isVisible().catch(() => false))) {
    await page.keyboard.press('Control+Shift+A');
    await page.waitForTimeout(600);
  }

  // --- 0) consent gate (#5): before the first-run disclosure is acknowledged, activity.query is NOT offered ----
  const ackBtn = page.getByRole('button', { name: 'Mengerti' });
  check(
    await ackBtn.isVisible().catch(() => false),
    'the first-run disclosure is shown (isolated profile)',
  );
  check(
    /pertanyaan\s+yang\s+Anda ketik dikirim ke provider AI/.test(
      await page
        .getByRole('region', { name: /pencatatan activity/i })
        .innerText()
        .catch(() => ''),
    ),
    'the disclosure states that the typed question goes to the AI provider',
  );
  await chat.locator('textarea').fill(Q_FREQUENT);
  await chat.getByRole('button', { name: 'Send' }).click();
  await page.waitForTimeout(20_000);
  check(
    !(await chat
      .locator('[data-testid="activity-answer"]')
      .first()
      .isVisible()
      .catch(() => false)),
    'before the disclosure is acknowledged no history answer card is produced',
  );
  await ackBtn.click();
  await page.waitForTimeout(800);
  check(
    !(await ackBtn.isVisible().catch(() => false)),
    'the disclosure disappears after "Mengerti"',
  );
  await page.reload();
  await page.waitForSelector('section', { timeout: 20_000 });
  await page.waitForTimeout(1500);
  if (
    !(await page
      .locator('aside.chat')
      .isVisible()
      .catch(() => false))
  ) {
    await page.keyboard.press('Control+Shift+A');
    await page.waitForTimeout(600);
  }

  const ask = async (text) => {
    await chat.locator('textarea').fill(text);
    await chat.getByRole('button', { name: 'Send' }).click();
  };
  const waitAnswerOrPlan = async () => {
    for (let i = 0; i < 90; i++) {
      if (
        await chat
          .locator('[data-testid="activity-answer"]')
          .last()
          .isVisible()
          .catch(() => false)
      )
        return 'answer';
      if (
        await chat
          .getByRole('button', { name: 'Run', exact: true })
          .isVisible()
          .catch(() => false)
      )
        return 'plan';
      await page.waitForTimeout(500);
    }
    return 'timeout';
  };
  // Sidebar Recent/Frequent text carries the visit counters ("N kunjungan"); a read-only query must not move them.
  const recentText = () =>
    page.evaluate(() => {
      const places = document.querySelector('.places');
      return places ? places.innerText.replace(/\s+/g, ' ') : '';
    });

  // --- A) frequent -----------------------------------------------------------------------------
  const sidebarBefore = await recentText();
  await ask(Q_FREQUENT);
  const outcomeA = await waitAnswerOrPlan();
  check(outcomeA === 'answer', 'a read-only answer card appears for a history question', outcomeA);
  check(
    !(await chat
      .getByRole('button', { name: 'Run', exact: true })
      .isVisible()
      .catch(() => false)),
    'no Run button (no approval flow for a read-only answer)',
  );
  const card = chat.locator('[data-testid="activity-answer"]').last();
  const textA = ((await card.innerText().catch(() => '')) || '').replace(/\s+/g, ' ');
  console.log(`      card A: ${JSON.stringify(textA.slice(0, 220))}`);
  check(/riwayat lokal/i.test(textA), 'the card says it was answered from local history');
  check(
    await chat
      .locator('[data-testid="answer-privacy"]')
      .last()
      .isVisible()
      .catch(() => false),
    'the card states the local-only disclosure',
  );
  check(
    (await chat.locator('[data-testid="answer-item"]').count()) > 0,
    'the card lists at least one history item (seeded visits exist)',
  );
  const sidebarAfter = await recentText();
  check(
    sidebarBefore === sidebarAfter,
    'the query left the sidebar Recent/Frequent counters unchanged',
  );

  // --- B) find by name + open in panel ---------------------------------------------------------
  await page.reload();
  await page.waitForSelector('section', { timeout: 20_000 });
  await page.waitForTimeout(1500);
  if (
    !(await page
      .locator('aside.chat')
      .isVisible()
      .catch(() => false))
  ) {
    await page.keyboard.press('Control+Shift+A');
    await page.waitForTimeout(600);
  }
  await ask(Q_FIND);
  const outcomeB = await waitAnswerOrPlan();
  check(outcomeB === 'answer', 'a find-by-name question gives an answer card', outcomeB);
  const textB = (
    await chat
      .locator('[data-testid="activity-answer"]')
      .last()
      .innerText()
      .catch(() => '')
  ).replace(/\s+/g, ' ');
  console.log(`      card B: ${JSON.stringify(textB.slice(0, 220))}`);
  check(
    textB.toLowerCase().includes(FOLDER.toLowerCase()),
    'the card lists the requested folder name',
  );
  const openBtn = chat.locator('[data-testid="open-item"]').first();
  if (await openBtn.isVisible().catch(() => false)) {
    await openBtn.click();
    await page.waitForTimeout(1500);
    const crumbs = await page.evaluate(() =>
      [...document.querySelectorAll('section')].map((s) =>
        [...s.querySelectorAll('.crumb')].map((c) => c.textContent.trim()),
      ),
    );
    check(
      crumbs.some((c) => c[c.length - 1]?.toLowerCase() === FOLDER.toLowerCase()),
      '"Buka di panel" navigated a pane to the folder',
      JSON.stringify(crumbs.map((c) => c.slice(-2))),
    );
  } else {
    check(false, '"Buka di panel" button is available for a listed folder');
  }

  fs.rmSync(ROOT, { recursive: true, force: true });
  await browser.close().catch(() => {});
}

main()
  .catch((error) => {
    check(false, 'unexpected error', String(error?.message ?? error).split('\n')[0]);
  })
  .finally(() => {
    console.log(failures.length ? `\n${failures.length} check(s) failed` : '\nall checks passed');
    process.exit(failures.length ? 1 : 0);
  });
