#!/usr/bin/env node
// Opt-in desktop smoke for the AI-09 cases the first follow-up script does not reach (FER-166 / FER-170):
//   L1 a free-form follow-up that the local parser does NOT handle, so it is sent to the provider;
//   L2 the revision limit (MAX_REVISIONS = 20);
//   L3 a follow-up while the job is running: the active job must not change.
// Same prerequisites as scripts/ai-desktop-smoke.mjs (docs/ai/desktop-smoke.md). Not part of `npm test`.
//
//   node scripts/ai-desktop-smoke-followup-live.mjs     (FERRUM_AI_SMOKE_BIG_MB sets the L3 file size, default 300)
//
// Fixture: ~/ferrum-ai-smoke-followup-live-tmp, removed at the end. The model only receives counts, flags and
// its own earlier intent, never names or paths.
import { createRequire } from 'node:module';
import crypto from 'node:crypto';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const require = createRequire(import.meta.url);
const { chromium } = require('playwright-core');

const CDP = process.env.FERRUM_CDP ?? 'http://127.0.0.1:9222';
const ROOT_NAME = 'ferrum-ai-smoke-followup-live-tmp';
const ROOT = path.join(os.homedir(), ROOT_NAME);
const LEFT = path.join(ROOT, 'left');
const RIGHT = path.join(ROOT, 'right');
const ASK = process.env.FERRUM_AI_SMOKE_MESSAGE ?? 'Arsipkan semua file di folder kiri jadi ZIP';
// Starts with "sebenarnya", so none of the local patterns in src/lib/ai/followup.ts match: it goes to the provider.
const FREE =
  process.env.FERRUM_AI_SMOKE_FREE_FOLLOWUP ??
  'sebenarnya cukup file PDF dan file txt saja yang masuk';
const BIG_MB = Number(process.env.FERRUM_AI_SMOKE_BIG_MB ?? 300);
const LIMIT = 20;

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
const noZip = () => zipsIn(LEFT).length === 0 && zipsIn(RIGHT).length === 0;

function makeFixture() {
  fs.rmSync(ROOT, { recursive: true, force: true });
  fs.mkdirSync(LEFT, { recursive: true });
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
    const down = path.relative(path.parse(os.homedir()).root, os.homedir()).split(path.sep);
    for (let step = 0; step < 12; step++) {
      for (let t = 0; t < 8 && !(await rows(i)).length; t++) await page.waitForTimeout(250);
      if ((await rows(i)).map(base).includes(ROOT_NAME)) break;
      if ((await rows(i)).some((p) => /^[A-Za-z]:\\[^\\]+$/.test(p ?? ''))) {
        for (const seg of down) await enter(i, seg);
        continue;
      }
      await pane(i).locator('button[title="Parent folder"]').click({ timeout: 5000 });
      await page.waitForTimeout(1000);
    }
    await enter(i, ROOT_NAME);
    await enter(i, folder);
  };
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
  const card = () => chat.locator('article, .run, [data-run]').last();
  const revision = async () => {
    const found = [...(await text()).matchAll(/Revision (\d+)/g)];
    return found.length ? Number(found[found.length - 1][1]) : 1;
  };
  const send = async (message) => {
    await chat.locator('textarea').fill(message);
    await chat.getByRole('button', { name: 'Send' }).click();
  };
  const waitRevision = async (n, tries = 60) => {
    for (let i = 0; i < tries && (await revision()) < n; i++) await page.waitForTimeout(400);
    return revision();
  };
  const run = chat.getByRole('button', { name: 'Run', exact: true });
  const newPlan = async () => {
    const before = await run.count();
    await send(ASK);
    for (let i = 0; i < 120; i++) {
      if ((await run.count()) > before || ((await run.count()) && (await run.last().isVisible())))
        break;
      await page.waitForTimeout(500);
    }
    await page.waitForTimeout(500);
    return run
      .last()
      .isVisible()
      .catch(() => false);
  };
  const cancelPlan = async () => {
    await send('batal');
    await page.waitForTimeout(1500);
  };
  const count = (s, re) => (s.match(re) ?? []).length;

  // ---- L1: free-form follow-up through the provider -------------------------------------------------------
  check(await newPlan(), 'L1: a plan preview appears (revision 1)');
  check(noZip(), 'L1: no ZIP before approval');
  await send(FREE);
  const r1 = await waitRevision(2, 150); // provider round trip
  check(r1 === 2, `L1: the free-form follow-up "${FREE}" creates revision 2 (via provider)`);
  await chat
    .getByRole('button', { name: 'Show items' })
    .last()
    .click()
    .catch(() => {});
  await page.waitForTimeout(800);
  const items = await text();
  const tail = items.slice(items.lastIndexOf('Revision 2'));
  check(
    /c\.pdf/.test(tail) && /d\.pdf/.test(tail) && /a\.txt/.test(tail) && !/notes\.md/.test(tail),
    "L1: the model's reading is applied: PDF + TXT listed, notes.md left out",
  );
  check(noZip(), 'L1: still nothing written after the provider revision');
  await cancelPlan();
  check(noZip(), 'L1: "batal" ends the plan and writes nothing');

  // ---- L2: the 20-revision limit (local renames are deterministic) -------------------------------------------
  check(await newPlan(), 'L2: a new plan preview appears');
  let reached = await revision();
  for (let n = 2; n <= LIMIT; n++) {
    await send(`namanya r${n}.zip`);
    reached = await waitRevision(n);
    if (reached !== n) break;
  }
  check(reached === LIMIT, `L2: ${LIMIT - 1} follow-ups reach revision ${LIMIT}`);
  const notices = count(await text(), /Batas 20 revisi tercapai/g);
  await send('namanya r21.zip');
  await page.waitForTimeout(1500);
  check(
    count(await text(), /Batas 20 revisi tercapai/g) > notices,
    'L2: the next follow-up is refused with the revision limit',
  );
  check((await revision()) === LIMIT, `L2: the plan stays at revision ${LIMIT}`);
  check(noZip(), 'L2: nothing written');
  await cancelPlan();

  // ---- L3: a follow-up while the job runs must not change the active job ------------------------------------
  const big = path.join(LEFT, 'big.bin');
  const chunk = crypto.randomBytes(1024 * 1024);
  const fd = fs.openSync(big, 'w');
  for (let i = 0; i < BIG_MB; i++) fs.writeSync(fd, chunk);
  fs.closeSync(fd);
  await page.waitForTimeout(2500); // let the pane pick up big.bin
  check(await newPlan(), 'L3: a plan preview appears');
  const outs = [...(await text()).matchAll(/Output\s+(\S.*?\.zip)/g)];
  const planned = base(outs.length ? outs[outs.length - 1][1] : '');
  await run.last().click();
  for (let i = 0; i < 40 && !/Running|Berjalan|running/.test(await text()); i++)
    await page.waitForTimeout(100);
  const runningMessages = count(await text(), /Perubahan tidak mengubah job yang aktif/g);
  await send('namanya lain.zip');
  await page.waitForTimeout(800);
  check(
    count(await text(), /Perubahan tidak mengubah job yang aktif/g) > runningMessages,
    'L3: a follow-up while the job runs is refused ("Perubahan tidak mengubah job yang aktif")',
  );
  for (
    let i = 0;
    i < 600 &&
    !/Completed|Failed|Outcome unknown/.test(
      await card()
        .innerText()
        .catch(() => ''),
    );
    i++
  ) {
    if (zipsIn(LEFT).includes(planned) && /Completed/.test(await text())) break;
    await page.waitForTimeout(500);
  }
  check(/Completed/.test(await text()), 'L3: the job finishes Completed');
  check(
    Boolean(planned) && zipsIn(LEFT).includes(planned),
    `L3: the ZIP has the approved name (${planned})`,
  );
  check(
    !zipsIn(LEFT).includes('lain.zip') && !zipsIn(RIGHT).includes('lain.zip'),
    'L3: no lain.zip was created',
  );
  check(zipsIn(LEFT).length === 1 && zipsIn(RIGHT).length === 0, 'L3: exactly one ZIP');

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
