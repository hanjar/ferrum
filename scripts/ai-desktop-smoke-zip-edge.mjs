#!/usr/bin/env node
// Opt-in desktop smoke for the ZIP edge cases of the AI assistant (FER-165 / FER-205). NOT part of `npm test`:
// it drives the REAL Tauri window through WebView2 remote debugging, calls the configured provider and lets the
// real `archive_create` engine write to a temporary fixture.
//
//   1. start the app with remote debugging (PowerShell):
//        $env:FERRUM_PROFILE_DIR = "$env:TEMP/ferrum-smoke-profile"; $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = '--remote-debugging-port=9222'; npx tauri dev
//   2. configure the provider (docs/ai/provider.md) and store the key in Credential Manager.
//   3. node scripts/ai-desktop-smoke-zip-edge.mjs    (FERRUM_CDP overrides http://127.0.0.1:9222)
//
// Scenarios (fixture `~/ferrum-ai-smoke-zipedge-tmp`, removed at the end):
//   E1 output inside the source subtree: left = proj, right = proj\sub, "save the ZIP in the right panel".
//   E2 unreadable input: proj\locked.txt is held open with no sharing by a child PowerShell process.
//   E3 cancel during Pack (LIMIT-6): a large incompressible file keeps the engine busy; "batal" while it runs.
// Not covered here (no safe way to fill a real drive without admin rights): a full disk. That path is tested on the
// engine with a writer that fails like ERROR_DISK_FULL (`src-tauri/src/archive_create_test.rs`, `edge_tests`).
import { createRequire } from 'node:module';
import { spawn } from 'node:child_process';
import crypto from 'node:crypto';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const require = createRequire(import.meta.url);
const { chromium } = require('playwright-core');

const CDP = process.env.FERRUM_CDP ?? 'http://127.0.0.1:9222';
const FIXTURE = 'ferrum-ai-smoke-zipedge-tmp';
const ROOT = path.join(os.homedir(), FIXTURE);
const PROJ = path.join(ROOT, 'proj');
const SUB = path.join(PROJ, 'sub');
const LOCKED = path.join(ROOT, 'locked');
const BIG = path.join(ROOT, 'big');
const BIG_MB = Number(process.env.FERRUM_AI_SMOKE_BIG_MB ?? 300);
const MSG_RIGHT =
  process.env.FERRUM_AI_SMOKE_RIGHT_MESSAGE ??
  'Arsipkan semua file di folder kiri jadi ZIP dan simpan di panel kanan';
const MSG_LEFT =
  process.env.FERRUM_AI_SMOKE_MESSAGE ?? 'Arsipkan semua file di folder kiri jadi ZIP';

const failures = [];
const check = (ok, label) => {
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${label}`);
  if (!ok) failures.push(label);
};

/** Minimal ZIP central-directory reader: entry names only. */
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
const listZips = (dir) =>
  fs
    .readdirSync(dir)
    .filter((f) => f.toLowerCase().endsWith('.zip'))
    .sort();
const staging = (dir) => {
  const out = [];
  for (const e of fs.readdirSync(dir, { withFileTypes: true, recursive: true })) {
    if (e.isFile() && e.name.endsWith('.ferrum-tmp')) out.push(e.name);
  }
  return out;
};

function makeFixture() {
  fs.rmSync(ROOT, { recursive: true, force: true });
  fs.mkdirSync(SUB, { recursive: true });
  fs.writeFileSync(path.join(PROJ, 'a.txt'), 'alpha\n');
  fs.writeFileSync(path.join(PROJ, 'b.txt'), 'beta\n');
  fs.writeFileSync(path.join(SUB, 'inner.txt'), 'nested\n');
  fs.mkdirSync(LOCKED, { recursive: true });
  fs.writeFileSync(path.join(LOCKED, 'free.txt'), 'readable\n');
  fs.writeFileSync(path.join(LOCKED, 'locked.txt'), 'held open by another process\n');
  fs.mkdirSync(BIG, { recursive: true });
  const fd = fs.openSync(path.join(BIG, 'big.bin'), 'w');
  const chunk = 8 * 1024 * 1024;
  for (let written = 0; written < BIG_MB * 1024 * 1024; written += chunk)
    fs.writeSync(fd, crypto.randomBytes(chunk));
  fs.closeSync(fd);
}

/** Hold `file` open with FileShare.None in a separate process (like an editor or Office locking it). */
function lockFile(file) {
  const ps = `$f=[System.IO.File]::Open($env:LOCK_TARGET,'Open','Read','None'); [Console]::Out.WriteLine('locked'); Start-Sleep -Seconds 600; $f.Close()`;
  const child = spawn('powershell.exe', ['-NoProfile', '-Command', ps], {
    env: { ...process.env, LOCK_TARGET: file },
    stdio: ['ignore', 'pipe', 'inherit'],
  });
  const ready = new Promise((resolve, reject) => {
    child.stdout.on('data', (d) => String(d).includes('locked') && resolve());
    child.on('exit', (code) => reject(new Error(`locker exited early (${code})`)));
    setTimeout(() => reject(new Error('locker did not start')), 15_000);
  });
  return { child, ready };
}

let locker;

async function main() {
  makeFixture();
  const browser = await chromium.connectOverCDP(CDP);
  const page = browser.contexts()[0].pages()[0];
  check(
    await page.evaluate(() => '__TAURI_INTERNALS__' in window),
    'running inside the real Tauri webview',
  );
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
  if (await page.locator('[role="dialog"]').count()) {
    await page.keyboard.press('Escape');
    await page.waitForTimeout(300);
  }
  /** Navigate pane `i` to ~/<FIXTURE>/<...parts>. */
  const goTo = async (i, ...parts) => {
    for (let step = 0; step < 12; step++) {
      const names = (await rows(i)).map(base);
      if (names.includes(FIXTURE)) break;
      await pane(i).locator('button[title="Parent folder"]').click({ timeout: 5000 });
      await page.waitForTimeout(700);
    }
    await enter(i, FIXTURE);
    for (const part of parts) await enter(i, part);
  };

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
  const cards = () => chat.locator('section.card');
  const lastCard = async () => {
    const n = await cards().count();
    return n
      ? await cards()
          .nth(n - 1)
          .innerText()
      : await chat.innerText();
  };
  const ask = async (message) => {
    await page.waitForTimeout(1200); // panes refresh after a run; a request inside that window is (correctly) stale
    await chat.locator('textarea').fill(message);
    await chat.getByRole('button', { name: 'Send' }).click();
    const run = chat.getByRole('button', { name: 'Run', exact: true });
    for (let i = 0; i < 90 && !(await run.isVisible().catch(() => false)); i++)
      await page.waitForTimeout(500);
    return run;
  };
  const FINAL = /Completed|Failed|Partially completed|Cancelled|Outcome unknown|Out of date/;
  const waitFinal = async (limitMs = 120_000) => {
    for (let t = 0; t < limitMs; t += 500) {
      const text = await lastCard();
      const m = text.match(FINAL);
      if (m) return { label: m[0], text };
      await page.waitForTimeout(500);
    }
    return { label: '', text: await lastCard() };
  };

  // ── E1: output inside the source subtree ───────────────────────────────────────────────
  await goTo(0, 'proj');
  await goTo(1, 'proj', 'sub');
  check(
    (await rows(1)).map(base).includes('inner.txt'),
    'E1 right pane shows proj\\sub (inside the left folder)',
  );
  const run1 = await ask(MSG_RIGHT);
  check(await run1.isVisible(), 'E1 a plan preview with a Run button appears');
  const preview1 = await lastCard();
  check(
    /Output\s+\S.*[\\/]proj[\\/]sub[\\/]\S*\.zip/.test(preview1),
    'E1 the planned ZIP is inside proj\\sub',
  );
  check(
    /di dalam folder yang diarsipkan/.test(preview1),
    'E1 preview says the ZIP lands inside the archived folder',
  );
  check(!/size unknown/.test(preview1), 'E1 preview shows a computed size (totalBytes > 0)');
  check(
    listZips(SUB).length === 0 && listZips(PROJ).length === 0,
    'E1 no ZIP exists before approval',
  );
  await run1.click();
  const r1 = await waitFinal();
  check(r1.label === 'Completed', `E1 run completed (${r1.label || 'no final state'})`);
  const made = listZips(SUB);
  check(made.length === 1, `E1 exactly one ZIP in proj\\sub (${made.join(', ')})`);
  if (made.length === 1) {
    const names = zipNames(path.join(SUB, made[0]));
    check(
      JSON.stringify(names) === JSON.stringify(['a.txt', 'b.txt', 'sub/inner.txt']),
      `E1 ZIP payload is the sources only, not itself (${names.join(', ')})`,
    );
  }
  check(staging(ROOT).length === 0, 'E1 no staging file left behind');

  // ── E2: unreadable input ───────────────────────────────────────────────────────────────
  await goTo(0, 'locked');
  await goTo(1, 'locked');
  locker = lockFile(path.join(LOCKED, 'locked.txt'));
  await locker.ready;
  let lockedForReal = false;
  try {
    fs.readFileSync(path.join(LOCKED, 'locked.txt'));
  } catch {
    lockedForReal = true;
  }
  check(lockedForReal, 'E2 locked.txt cannot be read while the other process holds it');
  const run2 = await ask(MSG_LEFT);
  check(await run2.isVisible(), 'E2 a plan preview with a Run button appears');
  check(listZips(LOCKED).length === 0, 'E2 no ZIP exists before approval');
  await run2.click();
  const r2 = await waitFinal();
  console.log(`      E2 final state: ${r2.label}`);
  check(r2.label !== 'Completed' && r2.label !== '', 'E2 the run is NOT reported as completed');
  check(
    /Failed|Outcome unknown/.test(r2.label),
    'E2 the run ends failed (or outcome unknown until FER-163 maps the cause)',
  );
  check(listZips(LOCKED).length === 0, 'E2 no ZIP (and no placeholder) at the destination');
  check(staging(ROOT).length === 0, 'E2 no staging file left behind');
  check(!/os error|Source could not be read/.test(r2.text), 'E2 no raw engine text in the chat');
  locker.child.kill();
  locker = undefined;

  // ── E3: cancel during Pack (LIMIT-6) ──────────────────────────────────────────────────
  await goTo(0, 'big');
  await goTo(1, 'big');
  const run3 = await ask(MSG_LEFT);
  check(await run3.isVisible(), 'E3 a plan preview with a Run button appears');
  await run3.click();
  await page.waitForTimeout(400);
  const busy = !FINAL.test(await lastCard());
  check(busy, 'E3 Pack is still running when the cancel is sent');
  await chat.locator('textarea').fill('batal');
  await chat.getByRole('button', { name: 'Send' }).click();
  let honest = false;
  for (let i = 0; i < 40 && !honest; i++) {
    honest = /tidak dapat dibatalkan/.test(await chat.innerText());
    if (!honest) await page.waitForTimeout(250);
  }
  check(honest, 'E3 chat says Pack cannot be cancelled (no queue job)');
  const r3 = await waitFinal(300_000);
  check(
    r3.label === 'Completed',
    `E3 the real result is reported afterwards (${r3.label || 'no final state'})`,
  );
  check(!/Cancelled/.test(r3.label), 'E3 never reported as cancelled');
  const bigZips = listZips(BIG);
  check(bigZips.length === 1, 'E3 the ZIP exists (Pack ran to completion)');
  if (bigZips.length === 1) {
    check(
      JSON.stringify(zipNames(path.join(BIG, bigZips[0]))) === JSON.stringify(['big.bin']),
      'E3 ZIP payload is big.bin',
    );
  }
  check(staging(ROOT).length === 0, 'E3 no staging file left behind');

  await browser.close();
}

try {
  await main();
} catch (error) {
  failures.push(`unexpected error: ${error.message}`);
  console.log(`FAIL  unexpected error: ${error.message}`);
} finally {
  if (locker) locker.child.kill();
  await new Promise((r) => setTimeout(r, 500));
  fs.rmSync(ROOT, { recursive: true, force: true });
}
console.log(failures.length ? `\n${failures.length} check(s) failed` : '\nall checks passed');
process.exit(failures.length ? 1 : 0);
