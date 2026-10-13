#!/usr/bin/env node
// Opt-in desktop smoke for the CHAT sync workflow (FER-168 / AI-11): "sinkronkan kedua panel dua arah" through
// the AI panel (preview -> Run -> result -> second run). NOT part of `npm test`: it drives the REAL Tauri window
// through WebView2 remote debugging and calls the configured provider (same pattern as scripts/ai-desktop-smoke.mjs).
//
// STATUS: written and syntax-checked only (`node --check`); NOT YET EXECUTED against a real build.
//
//   1. start the app with remote debugging (PowerShell):
//        $env:FERRUM_PROFILE_DIR = "$env:TEMP/ferrum-smoke-profile"; $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = '--remote-debugging-port=9222'; npx tauri dev
//   2. configure the provider (docs/ai/provider.md) and store the key in Credential Manager.
//   3. node scripts/ai-desktop-smoke-sync.mjs        (FERRUM_CDP overrides http://127.0.0.1:9222)
//
// Fixture: a temporary folder in the user's home, removed at the end. The model only receives counts and flags,
// never names or paths. Scenario:
//   run 1 (first run, no baseline): the union of both folders, nothing deleted, nothing wins by timestamp;
//   run 2 (baseline saved by run 1): a left-only edit is an overwrite listed in the preview, a both-changed file
//   is a CONFLICT that is listed and skipped, a one-sided delete is NOT propagated, a new right-only file is copied;
//   guard: a file changed after the preview makes Run stale with the BACKEND wording and writes nothing.
// Exit code 0 = every check passed.
import { createRequire } from 'node:module';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { cleanSmokeBaselines } from './ai-smoke-baselines.mjs';

const require = createRequire(import.meta.url);
const { chromium } = require('playwright-core');

const CDP = process.env.FERRUM_CDP ?? 'http://127.0.0.1:9222';
const ROOT = path.join(os.homedir(), 'ferrum-sync-smoke-tmp');
const LEFT = path.join(ROOT, 'left');
const RIGHT = path.join(ROOT, 'right');
const MESSAGE = process.env.FERRUM_AI_SMOKE_MESSAGE ?? 'sinkronkan kedua panel dua arah';

const failures = [];
const check = (ok, label) => {
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${label}`);
  if (!ok) failures.push(label);
};
const read = (dir, name) =>
  fs.existsSync(path.join(dir, name)) ? fs.readFileSync(path.join(dir, name), 'utf8') : null;
const put = (dir, name, body) => fs.writeFileSync(path.join(dir, name), body);

function makeFixture() {
  fs.rmSync(ROOT, { recursive: true, force: true });
  fs.mkdirSync(LEFT, { recursive: true });
  fs.mkdirSync(RIGHT, { recursive: true });
  for (const dir of [LEFT, RIGHT]) put(dir, 'same.txt', 'identical');
  put(LEFT, 'only-left.txt', 'from left');
  put(RIGHT, 'only-right.txt', 'from right');
  for (const dir of [LEFT, RIGHT]) put(dir, 'edit-left.txt', 'v1');
  for (const dir of [LEFT, RIGHT]) put(dir, 'both.txt', 'b1');
  for (const dir of [LEFT, RIGHT]) put(dir, 'gone.txt', 'will be deleted on the right');
}

async function main() {
  // A baseline left by an earlier smoke run would turn run 1 into a "second" run: drop only the smoke's own files.
  const removed = cleanSmokeBaselines();
  console.log(`INFO  stale smoke baselines removed: ${removed.length}`);
  makeFixture();
  const browser = await chromium.connectOverCDP(CDP);
  const page = browser.contexts()[0].pages()[0];
  check(
    await page.evaluate(() => '__TAURI_INTERNALS__' in window),
    'running inside the real Tauri webview',
  );
  // Chat history lives in memory: start from a clean UI state.
  await page.reload();
  await page.waitForSelector('section', { timeout: 20_000 });
  await page.waitForTimeout(1500);

  const pane = (i) => page.locator('section').nth(i);
  const rows = (i) =>
    pane(i)
      .locator('.row[data-path]')
      .evaluateAll((els) => els.map((e) => e.getAttribute('data-path')));
  const base = (p) => p.split(/[\\/]/).pop();
  const enter = async (i, name) => {
    await pane(i).locator(`.row[data-path$="${name}"]`).first().dblclick({ timeout: 10_000 });
    await page.waitForTimeout(800);
  };
  const goInto = async (i, folder) => {
    // The fixture lives directly under the home folder; climb with the pane's "Parent folder" button.
    for (let step = 0; step < 10; step++) {
      if ((await rows(i)).map(base).includes('ferrum-sync-smoke-tmp')) break;
      await pane(i).locator('button[title="Parent folder"]').click({ timeout: 5000 });
      await page.waitForTimeout(700);
    }
    await enter(i, 'ferrum-sync-smoke-tmp');
    await enter(i, folder);
  };
  const reopenFixture = async () => {
    await goInto(0, 'left');
    await goInto(1, 'right');
  };
  if (await page.locator('[role="dialog"]').count()) await page.keyboard.press('Escape');
  await reopenFixture();
  check((await rows(0)).map(base).includes('same.txt'), 'left pane shows the fixture folder');

  if (
    !(await page
      .locator('aside.chat')
      .isVisible()
      .catch(() => false))
  ) {
    await page.keyboard.press('Control+Shift+A');
    await page.waitForTimeout(500);
  }
  const chat = page.locator('aside.chat');
  check(await chat.isVisible(), 'Ctrl+Shift+A opens the AI panel');

  // After a run the panes refresh asynchronously; a request sent inside that window is (correctly) out of date.
  const ask = async () => {
    await page.waitForTimeout(2500);
    await chat.locator('textarea').fill(MESSAGE);
    await chat.getByRole('button', { name: 'Send' }).click();
    const run = chat.getByRole('button', { name: 'Run', exact: true });
    for (let i = 0; i < 90 && !(await run.isVisible().catch(() => false)); i++)
      await page.waitForTimeout(500);
    return run;
  };
  const text = async () => (await chat.innerText()).replace(/\s+/g, ' ');
  const waitFor = async (re, tries = 90) => {
    for (let i = 0; i < tries; i++) {
      if (re.test(await text())) return true;
      await page.waitForTimeout(500);
    }
    return false;
  };
  // The panel keeps earlier cards, so count terminal labels instead of searching for one.
  const count = async (re) => (await text()).split(re).length - 1;

  // ── run 1: first run (no baseline) ──
  const run1 = await ask();
  check(await run1.isVisible(), 'run 1: a plan preview with a Run button appears');
  const preview1 = await text();
  check(
    /Kiri → kanan: 1 item/.test(preview1) && /Kanan → kiri: 1 item/.test(preview1),
    'run 1: preview lists both directions',
  );
  check(/Hapus: 0/.test(preview1), 'run 1: delete count is 0');
  check(/Run pertama/.test(preview1), 'run 1: the preview says there is no baseline yet');
  check(
    read(RIGHT, 'only-left.txt') === null && read(LEFT, 'only-right.txt') === null,
    'run 1: nothing is copied before approval',
  );
  await run1.click();
  check(await waitFor(/Completed|Partially completed|Failed/), 'run 1: a terminal result appears');
  check(/Completed/.test(await text()), 'run 1: chat reports Completed from the engine result');
  // 61e9425: the engine reports the last copied file, so the card shows real counts ("2 completed", job 1/1),
  // not "0 completed" / N-1. Run 1 copies one item each way.
  for (let i = 0; i < 20 && !/[1-9]\d* completed · 0 failed/.test(await text()); i++)
    await page.waitForTimeout(500);
  const result1 = await text();
  const receipt = [...result1.matchAll(/(\d+) completed · (\d+) failed/g)].pop();
  check(
    receipt && Number(receipt[1]) === 2 && receipt[2] === '0',
    `run 1: the result card says "2 completed · 0 failed" (${receipt?.[0]})`,
  );
  const jobs = [...result1.matchAll(/completed (\d+)\/(\d+)/g)];
  check(
    jobs.length > 0 && jobs.every((j) => j[1] === j[2] && Number(j[2]) > 0),
    `run 1: every job reads N/N (${jobs.map((j) => `${j[1]}/${j[2]}`).join(', ')})`,
  );
  check(
    read(RIGHT, 'only-left.txt') === 'from left',
    'run 1: the left-only file reached the right',
  );
  check(
    read(LEFT, 'only-right.txt') === 'from right',
    'run 1: the right-only file reached the left',
  );
  check(
    read(LEFT, 'gone.txt') !== null && read(RIGHT, 'gone.txt') !== null,
    'run 1: nothing was deleted',
  );
  // The baseline is saved in the background after the jobs finished and a fresh content compare verified them.
  await page.waitForTimeout(5000);

  // ── changes between the runs ──
  put(LEFT, 'edit-left.txt', 'v2 changed only on the left');
  put(LEFT, 'both.txt', 'left edit');
  put(RIGHT, 'both.txt', 'right edit!');
  fs.rmSync(path.join(RIGHT, 'gone.txt'));
  put(RIGHT, 'new-right.txt', 'created on the right after run 1');
  await page.reload();
  await page.waitForSelector('section', { timeout: 20_000 });
  await page.waitForTimeout(1500);
  await reopenFixture();
  if (!(await chat.isVisible().catch(() => false))) await page.keyboard.press('Control+Shift+A');

  // ── run 2: baseline exists (it lives on disk, so it survives the reload) ──
  const run2 = await ask();
  check(await run2.isVisible(), 'run 2: a plan preview with a Run button appears');
  const preview2 = await text();
  console.log(`      preview 2: ${preview2.slice(-700)}`);
  check(/Baseline tersimpan dipakai/.test(preview2), 'run 2: the stored baseline is used');
  check(
    /Kiri → kanan: 1 item \(1 menimpa\)/.test(preview2),
    'run 2: the left-only edit is an overwrite listed in the preview',
  );
  check(
    /Konflik: 2 \(dilewati\)/.test(preview2) && /Hapus: 0/.test(preview2),
    'run 2: two conflicts are listed and skipped, delete count is 0',
  );
  await run2.click();
  const before2 = await count(/Completed/);
  await waitFor(/Completed|Partially completed|Failed/);
  for (let i = 0; i < 60 && (await count(/Completed/)) <= before2; i++)
    await page.waitForTimeout(500);
  check(
    read(RIGHT, 'edit-left.txt') === 'v2 changed only on the left',
    'run 2: the left-only edit was applied to the right',
  );
  check(
    read(LEFT, 'new-right.txt') === 'created on the right after run 1',
    'run 2: the new right-only file was copied to the left',
  );
  check(
    read(LEFT, 'both.txt') === 'left edit' && read(RIGHT, 'both.txt') === 'right edit!',
    'run 2: the conflict was NOT executed (both sides intact)',
  );
  check(
    read(LEFT, 'gone.txt') !== null && read(RIGHT, 'gone.txt') === null,
    'run 2: the one-sided delete was neither propagated nor resurrected',
  );
  check(
    /2 konflik dilewati/.test(await text()),
    'run 2: the result does not claim everything is in sync',
  );
  await page.waitForTimeout(5000);

  // ── backend guard (FER-216): a file changed AFTER the preview makes Run stale, nothing written ──
  put(LEFT, 'late-left.txt', 'created for the guard check');
  await page.waitForTimeout(2500);
  const run3 = await ask();
  check(await run3.isVisible(), 'guard: a third plan preview appears');
  put(LEFT, 'late-left.txt', 'changed after the preview, and a little longer');
  const stale0 = await count(/Out of date/);
  await run3.click();
  for (let i = 0; i < 40 && (await count(/Out of date/)) <= stale0; i++)
    await page.waitForTimeout(500);
  check(
    (await count(/Out of date/)) > stale0,
    'guard: a file changed after the preview makes the run stale',
  );
  check(
    /Rencana usang/.test(await text()),
    "guard: the refusal is the backend guard's stale report (Rust wording)",
  );
  check(read(RIGHT, 'late-left.txt') === null, 'guard: nothing was written for the stale run');

  await browser.close();
}

try {
  await main();
} catch (error) {
  failures.push(`unexpected error: ${error.message}`);
  console.log(`FAIL  unexpected error: ${error.message}`);
} finally {
  fs.rmSync(ROOT, { recursive: true, force: true });
}
console.log(failures.length ? `\n${failures.length} check(s) failed` : '\nall checks passed');
process.exit(failures.length ? 1 : 0);
