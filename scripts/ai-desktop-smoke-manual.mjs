#!/usr/bin/env node
// Opt-in desktop smoke for matrix row 22 (FER-170): the MANUAL Pack flow still works after the AI refactor that made
// chat and the Files > Pack dialog share one Pack service. No provider call. Same prerequisites as
// scripts/ai-desktop-smoke.mjs (app started with remote debugging); see docs/ai/desktop-smoke.md.
//
//   node scripts/ai-desktop-smoke-manual.mjs
//
// Fixture: ~/ferrum-ai-smoke-manual-tmp (left: two files + sub/inner.txt; output left.zip next to it), removed at the end.
import { createRequire } from 'node:module';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const require = createRequire(import.meta.url);
const { chromium } = require('playwright-core');

const CDP = process.env.FERRUM_CDP ?? 'http://127.0.0.1:9222';
const ROOT_NAME = 'ferrum-ai-smoke-manual-tmp';
const ROOT = path.join(os.homedir(), ROOT_NAME);
const LEFT = path.join(ROOT, 'left');
const EXPECTED = ['a.txt', 'b.txt', 'sub/inner.txt']; // directory entries are ignored

const failures = [];
const check = (ok, label) => {
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${label}`);
  if (!ok) failures.push(label);
};
// The manual Pack default target is `<pane folder>.zip` next to the folder (packService.defaultPackTarget).
const zips = () =>
  [LEFT, ROOT]
    .flatMap((d) =>
      fs
        .readdirSync(d)
        .filter((f) => f.toLowerCase().endsWith('.zip'))
        .map((f) => path.join(d, f)),
    )
    .sort();
function zipNames(file) {
  const buf = fs.readFileSync(file);
  let eocd = -1;
  for (let i = buf.length - 22; i >= 0; i--)
    if (buf.readUInt32LE(i) === 0x06054b50) {
      eocd = i;
      break;
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

async function main() {
  fs.rmSync(ROOT, { recursive: true, force: true });
  fs.mkdirSync(path.join(LEFT, 'sub'), { recursive: true });
  fs.writeFileSync(path.join(LEFT, 'a.txt'), 'a\n');
  fs.writeFileSync(path.join(LEFT, 'b.txt'), 'b\n');
  fs.writeFileSync(path.join(LEFT, 'sub', 'inner.txt'), 'inner\n');

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

  const pane = page.locator('section').nth(0);
  const base = (p) => p.split(/[\\/]/).pop();
  const rows = () =>
    pane
      .locator('.row[data-path]')
      .evaluateAll((els) => els.map((e) => e.getAttribute('data-path')));
  const enter = async (name) => {
    await pane.locator(`.row[data-path$="${name}"]`).first().dblclick({ timeout: 10_000 });
    await page.waitForTimeout(800);
  };
  const down = path.relative(path.parse(os.homedir()).root, os.homedir()).split(path.sep);
  for (let step = 0; step < 12; step++) {
    for (let t = 0; t < 8 && !(await rows()).length; t++) await page.waitForTimeout(250);
    if ((await rows()).map(base).includes(ROOT_NAME)) break;
    if ((await rows()).some((p) => /^[A-Za-z]:\\[^\\]+$/.test(p ?? ''))) {
      for (const seg of down) await enter(seg);
      continue;
    }
    await pane.locator('button[title="Parent folder"]').click({ timeout: 5000 });
    await page.waitForTimeout(1000);
  }
  await enter(ROOT_NAME);
  await enter('left');
  check((await rows()).map(base).includes('a.txt'), 'left pane shows the fixture folder');

  // Select everything in the left pane, then Files > Pack through the command palette.
  await pane.locator('.row[data-path$="a.txt"]').first().click();
  await page.keyboard.press('Control+A');
  await page.waitForTimeout(300);
  await page.keyboard.press('Control+K');
  await page.locator('input[placeholder="Type a command…"]').fill('Pack');
  await page.waitForTimeout(400);
  await page.keyboard.press('Enter');
  const dialog = page.locator('[role="dialog"]');
  await dialog.waitFor({ timeout: 5000 }).catch(() => {});
  const dialogText = await dialog.innerText().catch(() => '');
  check(
    /Pack to archive/.test(dialogText) && /Pack 3 items/.test(dialogText),
    `the manual Pack dialog opens for 3 items`,
  );
  check(zips().length === 0, 'nothing is written before the dialog is confirmed');
  await dialog.getByRole('button', { name: 'Pack', exact: true }).click();
  for (let i = 0; i < 40 && zips().length === 0; i++) await page.waitForTimeout(250);
  await page.waitForTimeout(800);
  check(
    zips().length === 1 && zips()[0] === path.join(ROOT, 'left.zip'),
    `the manual Pack created one ZIP at the default target (${zips()
      .map((z) => path.relative(ROOT, z))
      .join(', ')})`,
  );
  if (zips().length === 1) {
    const names = zipNames(zips()[0]);
    const files = names.filter((n) => !n.endsWith('/'));
    check(JSON.stringify(files) === JSON.stringify(EXPECTED), `ZIP payload (${names.join(', ')})`);
  }
  check(
    ![LEFT, ROOT].some((d) => fs.readdirSync(d).some((n) => n.includes('.ferrum-tmp'))),
    'no staging file left behind',
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
