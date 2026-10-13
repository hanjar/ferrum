#!/usr/bin/env node
// Opt-in desktop smoke for AI follow-ups (FER-166 / AI-09). Drives the REAL Tauri window through WebView2 remote
// debugging, like scripts/ai-desktop-smoke.mjs (same prerequisites: see docs/ai/desktop-smoke.md).
//
//   node scripts/ai-desktop-smoke-followup.mjs      (FERRUM_CDP overrides http://127.0.0.1:9222)
//
// Flow: ask for a ZIP, then refine the SAME plan before approving ("namanya laporan.zip", "taruh di kanan",
// "hanya PDF"): every refinement is a new revision, nothing is written until Run, and a refinement after the run has
// finished must not start or change anything. The three refinements are handled locally (no provider call).
import { createRequire } from 'node:module';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const require = createRequire(import.meta.url);
const { chromium } = require('playwright-core');

const CDP = process.env.FERRUM_CDP ?? 'http://127.0.0.1:9222';
const ROOT = path.join(os.homedir(), 'ferrum-ai-smoke-followup-tmp');
const LEFT = path.join(ROOT, 'left');
const RIGHT = path.join(ROOT, 'right');
const ASK = process.env.FERRUM_AI_SMOKE_MESSAGE ?? 'Arsipkan semua file di folder kiri jadi ZIP';

const failures = [];
const check = (ok, label) => {
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${label}`);
  if (!ok) failures.push(label);
};
const zipsIn = (dir) =>
  fs
    .readdirSync(dir)
    .filter((f) => f.toLowerCase().endsWith('.zip'))
    .sort();

/** Names of the ZIP members, read from the central directory. */
function zipNames(file) {
  const buf = fs.readFileSync(file);
  let eocd = -1;
  for (let i = buf.length - 22; i >= 0; i--) {
    if (buf.readUInt32LE(i) === 0x06054b50) {
      eocd = i;
      break;
    }
  }
  if (eocd < 0) throw new Error('not a ZIP');
  const count = buf.readUInt16LE(eocd + 10);
  let offset = buf.readUInt32LE(eocd + 16);
  const names = [];
  for (let i = 0; i < count; i++) {
    const nameLen = buf.readUInt16LE(offset + 28);
    const extraLen = buf.readUInt16LE(offset + 30);
    const commentLen = buf.readUInt16LE(offset + 32);
    names.push(buf.toString('utf8', offset + 46, offset + 46 + nameLen).replace(/\\/g, '/'));
    offset += 46 + nameLen + extraLen + commentLen;
  }
  return names.sort();
}

function makeFixture() {
  fs.rmSync(ROOT, { recursive: true, force: true });
  fs.mkdirSync(path.join(LEFT, 'sub'), { recursive: true });
  fs.mkdirSync(RIGHT, { recursive: true });
  for (const [name, body] of [
    ['a.txt', 'a'],
    ['b.txt', 'b'],
    ['c.pdf', 'c'],
    ['d.pdf', 'd'],
    ['notes.md', 'n'],
  ]) {
    fs.writeFileSync(path.join(LEFT, name), `${body}\n`);
  }
  fs.writeFileSync(path.join(LEFT, 'sub', 'inner.txt'), 'inner\n');
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

  const pane = (i) => page.locator('section').nth(i);
  const base = (p) => p.split(/[\\/]/).pop();
  const rows = (i) =>
    pane(i)
      .locator('.row[data-path]')
      .evaluateAll((els) => els.map((e) => e.getAttribute('data-path')));
  const enter = async (i, name) => {
    await pane(i).locator(`.row[data-path$="${name}"]`).first().dblclick({ timeout: 10_000 });
    await page.waitForTimeout(800);
  };
  const goInto = async (i, folder) => {
    for (let step = 0; step < 10; step++) {
      if ((await rows(i)).map(base).includes('ferrum-ai-smoke-followup-tmp')) break;
      await pane(i).locator('button[title="Parent folder"]').click({ timeout: 5000 });
      await page.waitForTimeout(700);
    }
    await enter(i, 'ferrum-ai-smoke-followup-tmp');
    await enter(i, folder);
  };
  if (await page.locator('[role="dialog"]').count()) {
    await page.keyboard.press('Escape');
    await page.waitForTimeout(300);
  }
  await goInto(0, 'left');
  await goInto(1, 'right');
  check((await rows(0)).map(base).includes('a.txt'), 'left pane shows the fixture folder');
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
  const text = () => chat.innerText();
  const revision = async () => {
    const found = [...(await text()).matchAll(/Revision (\d+)/g)];
    return found.length ? Number(found[found.length - 1][1]) : 1;
  };
  const lastOutput = async () => {
    const found = [...(await text()).matchAll(/Output\s+(\S.*?\.zip)/g)];
    return found.length ? found[found.length - 1][1] : '';
  };
  const send = async (message) => {
    await chat.locator('textarea').fill(message);
    await chat.getByRole('button', { name: 'Send' }).click();
  };
  const waitRevision = async (n) => {
    for (let i = 0; i < 60 && (await revision()) < n; i++) await page.waitForTimeout(400);
    return revision();
  };
  const run = chat.getByRole('button', { name: 'Run', exact: true });

  // 1) the first plan (model call): default output name, inside the source folder
  await send(ASK);
  for (
    let i = 0;
    i < 90 &&
    !(await run
      .last()
      .isVisible()
      .catch(() => false));
    i++
  )
    await page.waitForTimeout(500);
  check(await run.last().isVisible(), 'a plan preview with a Run button appears');
  check((await revision()) === 1, 'the first plan is revision 1');
  check(zipsIn(LEFT).length === 0 && zipsIn(RIGHT).length === 0, 'no ZIP exists before approval');

  // 2) "namanya laporan.zip": a new revision, local, still nothing written
  await send('namanya laporan.zip');
  check((await waitRevision(2)) === 2, 'follow-up "namanya laporan.zip" creates revision 2');
  check(
    /laporan\.zip$/i.test(await lastOutput()),
    `the output name changed (${base(await lastOutput())})`,
  );
  check(
    zipsIn(LEFT).length === 0 && zipsIn(RIGHT).length === 0,
    'still no ZIP after the rename follow-up',
  );

  // 3) "taruh di kanan": a new revision with the output in the right pane's folder
  await send('taruh di kanan');
  check((await waitRevision(3)) === 3, 'follow-up "taruh di kanan" creates revision 3');
  check(
    (await lastOutput()).includes('ferrum-ai-smoke-followup-tmp') &&
      /[\\/]right[\\/]/.test(await lastOutput()),
    'the output moved to the right pane folder',
  );

  // 4) "hanya PDF": revision 4; the preview lists only the PDFs
  await send('hanya PDF');
  check((await waitRevision(4)) === 4, 'follow-up "hanya PDF" creates revision 4');
  await chat
    .getByRole('button', { name: 'Show items' })
    .last()
    .click()
    .catch(() => {});
  await page.waitForTimeout(600);
  const card = await text();
  check(/c\.pdf/.test(card) && /d\.pdf/.test(card), 'the preview lists the two PDFs');
  check(zipsIn(LEFT).length === 0 && zipsIn(RIGHT).length === 0, 'still no ZIP before approval');

  // 5) approve the LAST revision only: one ZIP in the right folder with just the PDFs
  await run.last().click();
  for (let i = 0; i < 90 && zipsIn(RIGHT).length === 0; i++) await page.waitForTimeout(500);
  check(
    zipsIn(RIGHT).length === 1 && zipsIn(RIGHT)[0] === 'laporan.zip',
    'exactly one ZIP, laporan.zip, in the right folder',
  );
  check(zipsIn(LEFT).length === 0, 'nothing was written in the left folder');
  if (zipsIn(RIGHT).length === 1) {
    const names = zipNames(path.join(RIGHT, zipsIn(RIGHT)[0]));
    check(
      JSON.stringify(names) === JSON.stringify(['c.pdf', 'd.pdf']),
      `ZIP payload is only the PDFs (${names.join(', ')})`,
    );
  }

  // 6) a refinement AFTER the run finished must not start or change anything
  await page.waitForTimeout(1500);
  const before = JSON.stringify([zipsIn(LEFT), zipsIn(RIGHT)]);
  await send('namanya lain.zip');
  await page.waitForTimeout(2500);
  check(
    JSON.stringify([zipsIn(LEFT), zipsIn(RIGHT)]) === before,
    'a follow-up after the run created nothing new',
  );
  check(
    /berakhir|selesai|final|ended/i.test(await text()),
    'the chat says the plan has already finished',
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
