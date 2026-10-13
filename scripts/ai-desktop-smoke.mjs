#!/usr/bin/env node
// Opt-in desktop smoke for the AI assistant (FER-165 / AI-08). NOT part of `npm test`: it drives the
// REAL Tauri window through WebView2 remote debugging and calls the configured provider.
//
//   1. start the app with remote debugging (PowerShell):
//        $env:FERRUM_PROFILE_DIR = "$env:TEMP/ferrum-smoke-profile"; $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = '--remote-debugging-port=9222'; npx tauri dev
//   2. configure the provider (docs/ai/provider.md) and store the key in Credential Manager.
//   3. node scripts/ai-desktop-smoke.mjs            (FERRUM_CDP overrides http://127.0.0.1:9222)
//
// Fixture: a temporary folder in the user's home, removed at the end. The model only receives counts
// and flags, never names or paths. Exit code 0 = every check passed.
import { createRequire } from 'node:module';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const require = createRequire(import.meta.url);
const { chromium } = require('playwright-core');

const CDP = process.env.FERRUM_CDP ?? 'http://127.0.0.1:9222';
const ROOT = path.join(os.homedir(), 'ferrum-ai-smoke-tmp');
const LEFT = path.join(ROOT, 'left');
const RIGHT = path.join(ROOT, 'right');
const MESSAGE =
  process.env.FERRUM_AI_SMOKE_MESSAGE ?? 'Arsipkan semua file di folder kiri jadi ZIP';
const EXPECTED = ['file1.txt', 'file2.txt', 'notes.md', 'sub/inner.txt'];

const failures = [];
const check = (ok, label) => {
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${label}`);
  if (!ok) failures.push(label);
};

/** Minimal ZIP central-directory reader: entry names only (enough to verify the payload). */
function zipNames(file) {
  const buf = fs.readFileSync(file);
  let eocd = -1;
  for (let i = buf.length - 22; i >= 0; i--) {
    if (buf.readUInt32LE(i) === 0x06054b50) {
      eocd = i;
      break;
    }
  }
  if (eocd < 0) throw new Error('not a ZIP (no end-of-central-directory)');
  const count = buf.readUInt16LE(eocd + 10);
  let offset = buf.readUInt32LE(eocd + 16);
  const names = [];
  for (let i = 0; i < count; i++) {
    if (buf.readUInt32LE(offset) !== 0x02014b50) throw new Error('corrupt central directory');
    const nameLen = buf.readUInt16LE(offset + 28);
    const extraLen = buf.readUInt16LE(offset + 30);
    const commentLen = buf.readUInt16LE(offset + 32);
    names.push(buf.toString('utf8', offset + 46, offset + 46 + nameLen).replace(/\\/g, '/'));
    offset += 46 + nameLen + extraLen + commentLen;
  }
  return names.sort();
}
const zips = () =>
  fs
    .readdirSync(LEFT)
    .filter((f) => f.toLowerCase().endsWith('.zip'))
    .sort();

function makeFixture() {
  fs.rmSync(ROOT, { recursive: true, force: true });
  fs.mkdirSync(path.join(LEFT, 'sub'), { recursive: true });
  fs.mkdirSync(RIGHT, { recursive: true });
  fs.writeFileSync(path.join(LEFT, 'file1.txt'), 'one\n');
  fs.writeFileSync(path.join(LEFT, 'file2.txt'), 'two\n');
  fs.writeFileSync(path.join(LEFT, 'notes.md'), '# notes\n');
  fs.writeFileSync(path.join(LEFT, 'sub', 'inner.txt'), 'nested\n');
}

async function main() {
  makeFixture();
  const browser = await chromium.connectOverCDP(CDP);
  const page = browser.contexts()[0].pages()[0];
  check(
    await page.evaluate(() => '__TAURI_INTERNALS__' in window),
    'running inside the real Tauri webview',
  );
  // Start from a clean UI state: chat history lives in memory and would survive a previous run.
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
  const dismissDialogs = async () => {
    if (await page.locator('[role="dialog"]').count()) {
      await page.keyboard.press('Escape');
      await page.waitForTimeout(300);
    }
  };
  const goHomeThenInto = async (i, folder) => {
    // The fixture lives directly under the home folder. Climb with the pane's "Parent folder" button:
    // it also recovers a pane that restored an unreadable folder (no rows, so no ".." row to click).
    for (let step = 0; step < 10; step++) {
      const names = (await rows(i)).map(base);
      if (names.includes('ferrum-ai-smoke-tmp')) break;
      await pane(i).locator('button[title="Parent folder"]').click({ timeout: 5000 });
      await page.waitForTimeout(700);
    }
    await enter(i, 'ferrum-ai-smoke-tmp');
    await enter(i, folder);
  };

  await dismissDialogs();
  await goHomeThenInto(0, 'left');
  await goHomeThenInto(1, 'right');
  check((await rows(0)).map(base).includes('file1.txt'), 'left pane shows the fixture folder');

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

  // After a run the panels refresh asynchronously; a request sent inside that window is (correctly)
  // reported as out of date, so wait until the left pane lists every ZIP that exists on disk.
  const settle = async () => {
    for (let i = 0; i < 40; i++) {
      const listed = (await rows(0))
        .map(base)
        .filter((n) => n.toLowerCase().endsWith('.zip')).length;
      if (listed === zips().length) break;
      await page.waitForTimeout(250);
    }
    await page.waitForTimeout(600);
  };
  const ask = async () => {
    await settle();
    await chat.locator('textarea').fill(MESSAGE);
    await chat.getByRole('button', { name: 'Send' }).click();
    const run = chat.getByRole('button', { name: 'Run', exact: true });
    for (let i = 0; i < 90 && !(await run.isVisible().catch(() => false)); i++)
      await page.waitForTimeout(500);
    return run;
  };
  const finish = async () => {
    for (let i = 0; i < 90; i++) {
      if (/Completed|Failed|Partial|Cancelled/.test(await chat.innerText())) return;
      await page.waitForTimeout(500);
    }
  };

  // 1) preview must not write anything; scope note and item list must be visible
  const run1 = await ask();
  check(await run1.isVisible(), 'a plan preview with a Run button appears');
  await chat
    .getByRole('button', { name: 'Show items' })
    .click()
    .catch(() => {});
  check(zips().length === 0, 'no ZIP exists before approval');
  check(
    await chat
      .getByTestId('scope-note')
      .isVisible()
      .catch(() => false),
    'preview states what the scope includes',
  );
  const items = await chat.innerText();
  check(/sub\s+folder/.test(items), '"all files" included the subfolder (entries: all)');

  // 2) approve; the real ZIP must match
  await run1.click();
  await finish();
  check(/Completed/.test(await chat.innerText()), 'chat reports Completed from the engine result');
  check(zips().length === 1, 'exactly one ZIP was created');
  if (zips().length === 1) {
    const names = zipNames(path.join(LEFT, zips()[0]));
    check(
      JSON.stringify(names) === JSON.stringify(EXPECTED),
      `ZIP payload matches (${names.join(', ')})`,
    );
  }

  // 3) same request again: the existing ZIP must be kept and the new one renamed
  const before = zips();
  const run2 = await ask();
  check(await run2.isVisible(), 'second plan preview appears');
  await run2.click();
  // The panel still shows the first run's "Completed", so wait on the disk, not on the text.
  for (let i = 0; i < 60 && zips().length <= before.length; i++) await page.waitForTimeout(500);
  const after = zips();
  check(
    after.length === before.length + 1,
    'second run created a second ZIP (nothing overwritten)',
  );
  check(
    after.some((name) => /\(2\)\.zip$/i.test(name)),
    'the new ZIP got a unique name (collision rename)',
  );
  check(
    before.every((name) => after.includes(name)),
    'the first ZIP is still there',
  );

  // 4) backend guard (FER-410): a source that changes AFTER the preview must make Run stale, with no write
  const countBefore = zips().length;
  const run3 = await ask();
  check(await run3.isVisible(), 'third plan preview appears');
  fs.appendFileSync(
    path.join(LEFT, 'file1.txt'),
    'changed after the preview ' + 'and a bit longer',
  );
  await run3.click();
  for (let i = 0; i < 40 && !/Out of date/.test(await chat.innerText()); i++)
    await page.waitForTimeout(500);
  check(
    /Out of date/.test(await chat.innerText()),
    'a source changed after the preview makes the run stale',
  );
  // The wording comes from the Rust StaleReport, so this proves the BACKEND guard (not the TS context check) refused.
  check(
    /Rencana usang/.test(await chat.innerText()),
    "the refusal is the backend guard's stale report",
  );
  await page.waitForTimeout(1500);
  check(zips().length === countBefore, 'nothing was written for the stale run');

  // 5) FER-411: a destination name that appears AFTER the preview is never overwritten
  const staleCards = (await chat.innerText()).split('Out of date').length - 1;
  const run4 = await ask();
  check(await run4.isVisible(), 'fourth plan preview appears');
  const outputs = [...(await chat.innerText()).matchAll(/Output\s+(\S.*?\.zip)/g)];
  const planned = outputs.length ? outputs[outputs.length - 1][1] : '';
  const plannedName = planned.split(/[\\/]/).pop();
  check(
    Boolean(plannedName) && !fs.existsSync(path.join(LEFT, plannedName)),
    `the planned name is free (${plannedName})`,
  );
  fs.writeFileSync(path.join(LEFT, plannedName), 'precious data that must survive');
  await run4.click();
  for (
    let i = 0;
    i < 40 && (await chat.innerText()).split('Out of date').length - 1 <= staleCards;
    i++
  ) {
    await page.waitForTimeout(500);
  }
  check(
    (await chat.innerText()).split('Out of date').length - 1 > staleCards,
    'a name that appeared after the preview refuses the run',
  );
  check(
    fs.readFileSync(path.join(LEFT, plannedName), 'utf8') === 'precious data that must survive',
    'the file that appeared was NOT overwritten',
  );

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
