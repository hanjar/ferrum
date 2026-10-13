#!/usr/bin/env node
// Opt-in desktop smoke: a REAL folder with 100,000 entries in the left pane (FER-160 / AI-03, matrix row 3).
// NOT part of `npm test`: it drives the real Tauri window through WebView2 remote debugging and calls the
// configured provider. Same prerequisites as scripts/ai-desktop-smoke.mjs (docs/ai/desktop-smoke.md).
//
//   node scripts/ai-desktop-smoke-100k.mjs                  preview only (resolver / context), then Cancel
//   FERRUM_AI_SMOKE_100K_RUN=1 node scripts/ai-desktop-smoke-100k.mjs   also approve and verify the ZIP
//
// Fixture: ~/ferrum-ai-smoke-100k-tmp (left: 100,000 small files, right: empty), removed at the end unless
// FERRUM_AI_SMOKE_KEEP=1. The model only receives counts and flags, never names or paths.
import { createRequire } from 'node:module';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const require = createRequire(import.meta.url);
const { chromium } = require('playwright-core');

const CDP = process.env.FERRUM_CDP ?? 'http://127.0.0.1:9222';
const ROOT_NAME = 'ferrum-ai-smoke-100k-tmp';
const ROOT = path.join(os.homedir(), ROOT_NAME);
const LEFT = path.join(ROOT, 'left');
const RIGHT = path.join(ROOT, 'right');
const COUNT = Number(process.env.FERRUM_AI_SMOKE_100K_COUNT ?? 100_000);
const RUN = process.env.FERRUM_AI_SMOKE_100K_RUN === '1';
const MESSAGE =
  process.env.FERRUM_AI_SMOKE_MESSAGE ?? 'Arsipkan semua file di folder kiri jadi ZIP';

const failures = [];
const check = (ok, label) => {
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${label}`);
  if (!ok) failures.push(label);
};
const info = (label) => console.log(`INFO  ${label}`);
const seconds = (t0) => ((Date.now() - t0) / 1000).toFixed(1);

/** Entry count from the ZIP end-of-central-directory, ZIP64 aware (100k > 65,535). */
function zipEntryCount(file) {
  const fd = fs.openSync(file, 'r');
  try {
    const size = fs.fstatSync(fd).size;
    const tail = Buffer.alloc(Math.min(size, 66_000));
    fs.readSync(fd, tail, 0, tail.length, size - tail.length);
    let eocd = -1;
    for (let i = tail.length - 22; i >= 0; i--)
      if (tail.readUInt32LE(i) === 0x06054b50) {
        eocd = i;
        break;
      }
    if (eocd < 0) throw new Error('not a ZIP (no end-of-central-directory)');
    const count16 = tail.readUInt16LE(eocd + 10);
    if (count16 !== 0xffff) return count16;
    const loc = eocd - 20; // ZIP64 end-of-central-directory locator
    if (loc < 0 || tail.readUInt32LE(loc) !== 0x07064b50) throw new Error('ZIP64 locator missing');
    const rec = Buffer.alloc(56);
    fs.readSync(fd, rec, 0, 56, Number(tail.readBigUInt64LE(loc + 8)));
    if (rec.readUInt32LE(0) !== 0x06064b50) throw new Error('ZIP64 record missing');
    return Number(rec.readBigUInt64LE(32));
  } finally {
    fs.closeSync(fd);
  }
}
const zips = (dir) => fs.readdirSync(dir).filter((f) => f.toLowerCase().endsWith('.zip'));

function makeFixture() {
  fs.rmSync(ROOT, { recursive: true, force: true });
  fs.mkdirSync(LEFT, { recursive: true });
  fs.mkdirSync(RIGHT, { recursive: true });
  const t0 = Date.now();
  for (let i = 0; i < COUNT; i++) {
    fs.writeFileSync(path.join(LEFT, `f${String(i).padStart(6, '0')}.txt`), `${i}\n`);
  }
  info(`fixture: ${COUNT} files written in ${seconds(t0)} s`);
}

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
  if (await page.locator('[role="dialog"]').count()) await page.keyboard.press('Escape');

  const pane = (i) => page.locator('section').nth(i);
  const rows = (i) =>
    pane(i)
      .locator('.row[data-path]')
      .evaluateAll((els) => els.map((e) => e.getAttribute('data-path')));
  const base = (p) => p.split(/[\\/]/).pop();
  const enter = async (i, name) => {
    await pane(i).locator(`.row[data-path$="${name}"]`).first().dblclick({ timeout: 10_000 });
  };
  // Climb with "Parent folder" until the home folder is listed. A slow listing can make the pane climb past
  // home to the drive root, so from there walk back down the home path segment by segment.
  const goHome = async (i) => {
    const down = path.relative(path.parse(os.homedir()).root, os.homedir()).split(path.sep);
    for (let step = 0; step < 12; step++) {
      for (let t = 0; t < 8 && !(await rows(i)).length; t++) await page.waitForTimeout(250);
      const listed = (await rows(i)).map(base);
      if (listed.includes(ROOT_NAME)) return;
      const atRoot = (await rows(i)).some((p) => /^[A-Za-z]:\\[^\\]+$/.test(p ?? ''));
      if (atRoot) {
        for (const seg of down) {
          await enter(i, seg);
          await page.waitForTimeout(1000);
        }
        continue;
      }
      await pane(i).locator('button[title="Parent folder"]').click({ timeout: 5000 });
      await page.waitForTimeout(1000);
    }
  };

  await goHome(1);
  await enter(1, ROOT_NAME);
  await page.waitForTimeout(800);
  await enter(1, 'right');
  await goHome(0);
  await enter(0, ROOT_NAME);
  await page.waitForTimeout(800);
  const tList = Date.now();
  await enter(0, 'left');
  const first = `f${'0'.repeat(6)}.txt`;
  for (let i = 0; i < 240 && !(await rows(0)).map(base).includes(first); i++)
    await page.waitForTimeout(250);
  const rendered = (await rows(0)).length;
  info(`left pane listed the 100k folder in ${seconds(tList)} s; ${rendered} rows in the DOM`);
  check((await rows(0)).map(base).includes(first), 'left pane opens the 100k folder');
  check(
    rendered > 0 && rendered < COUNT,
    `the pane is virtualized (${rendered} DOM rows < ${COUNT})`,
  );

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
  check(await chat.isVisible(), 'AI panel is open');

  // Preview: must cover the WHOLE listing, not the rendered window, and write nothing.
  await page.waitForTimeout(1500);
  const tPlan = Date.now();
  await chat.locator('textarea').fill(MESSAGE);
  await chat.getByRole('button', { name: 'Send' }).click();
  const run = chat.getByRole('button', { name: 'Run', exact: true });
  for (let i = 0; i < 360 && !(await run.isVisible().catch(() => false)); i++) {
    if (/Failed|tidak lolos validasi/.test(await chat.innerText())) break;
    await page.waitForTimeout(500);
  }
  info(`preview after ${seconds(tPlan)} s`);
  check(
    await run.isVisible().catch(() => false),
    'a plan preview with a Run button appears (no contract failure)',
  );
  const text = await chat.innerText();
  const counted = [...text.matchAll(/([\d.,]+)\s+items\b/g)].map((m) =>
    Number(m[1].replace(/[.,]/g, '')),
  );
  const itemCount = counted.length ? counted[counted.length - 1] : NaN;
  check(itemCount === COUNT, `preview counts every entry (${itemCount} items, expected ${COUNT})`);
  await chat
    .getByRole('button', { name: 'Show items' })
    .click()
    .catch(() => {});
  await page.waitForTimeout(800);
  const total = (await chat.innerText()).match(/of\s+([\d.,]+)/);
  check(
    total && Number(total[1].replace(/[.,]/g, '')) === COUNT,
    `paged item list reports the full total (${total?.[1]})`,
  );
  check(zips(LEFT).length === 0 && zips(RIGHT).length === 0, 'no ZIP exists before approval');

  if (!RUN) {
    await chat
      .getByRole('button', { name: 'Cancel', exact: true })
      .last()
      .click()
      .catch(() => {});
    await page.waitForTimeout(1000);
    check(zips(LEFT).length === 0, 'cancelled preview wrote nothing');
  } else {
    const tRun = Date.now();
    await run.click();
    for (let i = 0; i < 3600; i++) {
      if (/Completed|Failed|Partial|Cancelled|cannot be confirmed/.test(await chat.innerText()))
        break;
      await page.waitForTimeout(500);
    }
    info(`run finished after ${seconds(tRun)} s`);
    const after = await chat.innerText();
    check(/Completed/.test(after), 'chat reports Completed from the engine result');
    const out = zips(LEFT);
    check(out.length === 1, `exactly one ZIP was created (${out.join(', ')})`);
    if (out.length === 1) {
      const n = zipEntryCount(path.join(LEFT, out[0]));
      check(n === COUNT, `ZIP holds every entry (${n})`);
    }
  }

  await browser.close();
}

main()
  .catch((error) => {
    console.error(error);
    failures.push(String(error?.message ?? error));
  })
  .finally(() => {
    if (process.env.FERRUM_AI_SMOKE_KEEP !== '1') fs.rmSync(ROOT, { recursive: true, force: true });
    console.log(failures.length ? `\n${failures.length} check(s) failed` : '\nall checks passed');
    process.exit(failures.length ? 1 : 0);
  });
