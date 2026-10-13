#!/usr/bin/env node
// Extension of the opt-in desktop smoke (FER-224 / AI-13.2). NOT part of `npm test` and NOT yet executed:
// it was written and syntax-checked (`node --check`) only. It drives the REAL Tauri window over WebView2
// remote debugging, like scripts/ai-desktop-smoke.mjs (which it does not modify), and covers what that script
// lists as "not covered":
//
//   S1  "the selected ones" while the selection is EMPTY  -> clear message, no preview with Run, no ZIP
//       (never a silent select-all);
//   S2  pane filter active + hidden items OFF             -> preview states filter and hidden exclusions,
//       ZIP members == exactly what the preview lists (and nothing hidden / filtered out);
//   S3  pane filter active + hidden items ON               -> hidden item that passes the filter is included.
//
//   1. start the app with remote debugging (PowerShell):
//        $env:FERRUM_PROFILE_DIR = "$env:TEMP/ferrum-smoke-profile"; $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = '--remote-debugging-port=9222'; npx tauri dev
//   2. configure the provider (docs/ai/provider.md); the key lives in Credential Manager.
//   3. node scripts/ai-desktop-smoke-extra.mjs      (FERRUM_CDP overrides http://127.0.0.1:9222)
//
// Windows only (hidden items are created with `attrib +H`). The fixture is a temporary folder under the user's
// home, removed at the end; the pane filter and the hidden-files toggle are restored. The model receives counts
// and flags only, never names or paths. S1 depends on the live model choosing the "selected" scope for the wording
// below: if it does not, the check reports "inconclusive" (a failure to investigate, not an app bug by itself).
// Exit code 0 = every check passed.
import { createRequire } from 'node:module';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const require = createRequire(import.meta.url);
const { chromium } = require('playwright-core');

const CDP = process.env.FERRUM_CDP ?? 'http://127.0.0.1:9222';
const ROOT = path.join(os.homedir(), 'ferrum-ai-smoke-extra-tmp');
const LEFT = path.join(ROOT, 'left');
const RIGHT = path.join(ROOT, 'right');
const FOLDER = path.basename(ROOT);
const MSG_SELECTED =
  process.env.FERRUM_AI_SMOKE_SELECTED_MESSAGE ??
  'Arsipkan item yang saya pilih di folder kiri jadi ZIP';
const MSG_VISIBLE =
  process.env.FERRUM_AI_SMOKE_VISIBLE_MESSAGE ??
  'Arsipkan semua file yang tampil di folder kiri jadi ZIP';

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
const zips = () =>
  fs
    .readdirSync(LEFT)
    .filter((f) => f.toLowerCase().endsWith('.zip'))
    .sort();

function makeFixture() {
  fs.rmSync(ROOT, { recursive: true, force: true });
  fs.mkdirSync(LEFT, { recursive: true });
  fs.mkdirSync(RIGHT, { recursive: true });
  fs.writeFileSync(path.join(LEFT, 'report.pdf'), 'pdf one\n');
  fs.writeFileSync(path.join(LEFT, 'invoice.pdf'), 'pdf two\n');
  fs.writeFileSync(path.join(LEFT, 'notes.txt'), 'plain text\n');
  const hidden = path.join(LEFT, 'secret.pdf');
  fs.writeFileSync(hidden, 'hidden pdf\n');
  execFileSync('attrib', ['+H', hidden]); // hidden attribute: what the app treats as a hidden item
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
  const rows = (i) =>
    pane(i)
      .locator('.row[data-path]')
      .evaluateAll((els) => els.map((e) => e.getAttribute('data-path')));
  const base = (p) => p.split(/[\\/]/).pop();
  const names = async (i) => (await rows(i)).map(base);
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
  const goInto = async (i, folder) => {
    for (let step = 0; step < 10; step++) {
      if ((await names(i)).includes(FOLDER)) break;
      await pane(i).locator('button[title="Parent folder"]').click({ timeout: 5000 });
      await page.waitForTimeout(700);
    }
    await enter(i, FOLDER);
    await enter(i, folder);
  };
  const blur = () =>
    page.evaluate(
      () => document.activeElement instanceof HTMLElement && document.activeElement.blur(),
    );
  const setFilter = async (value) => {
    const input = pane(0).locator('input[placeholder="Filter"]');
    await input.fill(value);
    await blur();
    await page.waitForTimeout(600);
  };
  const toggleHidden = async () => {
    await blur();
    await page.locator('body').press('Control+h');
    await page.waitForTimeout(800);
  };
  const hiddenShown = async () => (await names(0)).includes('secret.pdf');

  await dismissDialogs();
  await goInto(0, 'left');
  await goInto(1, 'right');
  check((await names(0)).includes('report.pdf'), 'left pane shows the fixture folder');

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

  const initialHidden = await hiddenShown();
  if (initialHidden) await toggleHidden(); // start from "hidden items OFF"
  check(!(await hiddenShown()), 'precondition: hidden items are not listed');

  const settle = async () => {
    for (let i = 0; i < 40; i++) {
      const listed = (await names(0)).filter((n) => n.toLowerCase().endsWith('.zip')).length;
      if (listed === zips().length) break;
      await page.waitForTimeout(250);
    }
    await page.waitForTimeout(600);
  };
  const send = async (message) => {
    await settle();
    await chat.locator('textarea').fill(message);
    await chat.getByRole('button', { name: 'Send' }).click();
  };
  const runButton = chat.getByRole('button', { name: 'Run', exact: true });
  const waitRun = async () => {
    for (let i = 0; i < 90 && !(await runButton.isVisible().catch(() => false)); i++)
      await page.waitForTimeout(500);
    return runButton;
  };
  const newestZip = (before) => zips().find((n) => !before.includes(n));

  // ---- S1: "selected" with an EMPTY selection --------------------------------------------------------------
  // Fresh navigation leaves nothing selected. Expect a clear message and NO runnable preview; never select-all.
  await send(MSG_SELECTED);
  let sawRun = false;
  let message = '';
  for (let i = 0; i < 60; i++) {
    sawRun = await runButton.isVisible().catch(() => false);
    message = await chat.innerText();
    if (sawRun || /Belum ada item yang dipilih|belum ada item/i.test(message)) break;
    await page.waitForTimeout(500);
  }
  if (sawRun) {
    check(
      false,
      'S1 inconclusive: the model chose a scope other than "selected" (a Run preview appeared); do NOT click Run, rephrase via FERRUM_AI_SMOKE_SELECTED_MESSAGE',
    );
  } else {
    check(
      /Belum ada item yang dipilih|belum ada item/i.test(message),
      'S1: empty selection gives the "nothing selected" message',
    );
  }
  check(
    zips().length === 0,
    'S1: no ZIP exists (selection was empty, nothing was selected silently)',
  );
  if (sawRun)
    await chat
      .getByRole('button', { name: /Cancel|Batal/i })
      .first()
      .click()
      .catch(() => {});

  // ---- S2: filter active + hidden OFF -----------------------------------------------------------------------
  await setFilter('pdf');
  const visible = await names(0);
  check(
    ['report.pdf', 'invoice.pdf'].every((n) => visible.includes(n)) &&
      !visible.includes('notes.txt') &&
      !visible.includes('secret.pdf'),
    `S2 precondition: pane lists only the visible .pdf files (${visible.join(', ')})`,
  );
  const before2 = zips();
  await send(MSG_VISIBLE);
  const run2 = await waitRun();
  check(await run2.isVisible(), 'S2: a plan preview with a Run button appears');
  await chat
    .getByRole('button', { name: 'Show items' })
    .click()
    .catch(() => {});
  const preview2 = await chat.innerText();
  const scope2 = await chat
    .getByTestId('scope-note')
    .last()
    .innerText()
    .catch(() => '');
  check(
    /filter/i.test(scope2 + preview2),
    'S2: the preview states that the pane filter limits the scope',
  );
  check(
    /tersembunyi|hidden/i.test(scope2 + preview2),
    'S2: the preview states that hidden items are excluded',
  );
  check(zips().length === before2.length, 'S2: no ZIP exists before approval');
  check(
    /report\.pdf/.test(preview2) && /invoice\.pdf/.test(preview2),
    'S2: the preview lists the two visible .pdf files',
  );
  check(
    !/notes\.txt/.test(preview2) && !/secret\.pdf/.test(preview2),
    'S2: the preview does not list filtered-out or hidden items',
  );
  await run2.click();
  for (let i = 0; i < 60 && zips().length <= before2.length; i++) await page.waitForTimeout(500);
  const z2 = newestZip(before2);
  check(Boolean(z2), 'S2: exactly one new ZIP was created');
  if (z2) {
    const members = zipNames(path.join(LEFT, z2));
    check(
      JSON.stringify(members) === JSON.stringify(['invoice.pdf', 'report.pdf']),
      `S2: ZIP members == preview (${members.join(', ')})`,
    );
  }

  // ---- S3: filter active + hidden ON ------------------------------------------------------------------------
  await toggleHidden();
  check(await hiddenShown(), 'S3 precondition: the hidden item is now listed (Ctrl+H)');
  const before3 = zips();
  await send(MSG_VISIBLE);
  const run3 = await waitRun();
  check(await run3.isVisible(), 'S3: a plan preview with a Run button appears');
  await chat
    .getByRole('button', { name: 'Show items' })
    .click()
    .catch(() => {});
  const preview3 = await chat.innerText();
  check(
    /secret\.pdf/.test(preview3),
    'S3: the preview lists the hidden .pdf that passes the filter',
  );
  await run3.click();
  for (let i = 0; i < 60 && zips().length <= before3.length; i++) await page.waitForTimeout(500);
  const z3 = newestZip(before3);
  check(Boolean(z3), 'S3: exactly one new ZIP was created');
  if (z3) {
    const members = zipNames(path.join(LEFT, z3));
    check(
      JSON.stringify(members) === JSON.stringify(['invoice.pdf', 'report.pdf', 'secret.pdf']),
      `S3: ZIP members include the hidden item and exclude notes.txt (${members.join(', ')})`,
    );
  }

  // restore the UI state we changed
  await setFilter('');
  if (!initialHidden && (await hiddenShown())) await toggleHidden();
  await browser.close();
}

try {
  await main();
} catch (error) {
  failures.push(`unexpected error: ${error.message}`);
  console.log(`FAIL  unexpected error: ${error.message}`);
} finally {
  try {
    fs.rmSync(ROOT, { recursive: true, force: true });
  } catch {
    // a hidden file can keep the folder; the next run recreates the fixture from scratch
  }
}
console.log(failures.length ? `\n${failures.length} check(s) failed` : '\nall checks passed');
process.exit(failures.length ? 1 : 0);
