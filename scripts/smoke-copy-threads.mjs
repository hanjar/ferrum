/**
 * Copy-thread controls smoke over a REAL WebView2 window (Windows-only). FER-471.
 *
 * Launches the built ferrum.exe with an isolated FERRUM_PROFILE_DIR + remote debugging, starts one real copy
 * job through the app's own Tauri IPC (`enqueue_copy`), then drives the Background Transfer Manager like a
 * user: [+] and [-] on the thread count, the Otomatis checkbox and the priority selector. After every click
 * the backend state (`copy_control_snapshot`) must agree with what the dialog shows. At the end the copy is
 * compared file by file against the source, and no staging file may remain.
 *
 *   node scripts/smoke-copy-threads.mjs <path-to-ferrum.exe> [fileCount]
 *
 * Fixture: a temp folder with many small files plus a few large ones, removed at the end. Screenshots go to
 * FERRUM_SMOKE_ARTIFACTS (default %TEMP%\ferrum-copy-smoke-shots); they are not committed.
 * ISOLATION: FERRUM_PROFILE_DIR; the real profile is hashed before/after and the run FAILS if it changed.
 */
import { createRequire } from 'node:module';
import { spawn, execSync } from 'node:child_process';
import {
  mkdtempSync,
  mkdirSync,
  rmSync,
  writeFileSync,
  readdirSync,
  readFileSync,
  statSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { profileEnv, snapshotRealProfile, verifyRealProfile } from './lib/test-profile.mjs';

const require = createRequire(import.meta.url);
const { chromium } = require('playwright-core');

const EXE = process.argv[2];
const FILES = Number(process.argv[3] ?? 24000);
const PORT = 9227;
if (!EXE) {
  console.error('usage: node scripts/smoke-copy-threads.mjs <path-to-ferrum.exe> [fileCount]');
  process.exit(2);
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const root = mkdtempSync(join(tmpdir(), 'ferrum-copy-smoke-'));
const profile = join(root, 'profile');
const src = join(root, 'src');
const dest = join(root, 'dest');
const shotDir = process.env.FERRUM_SMOKE_ARTIFACTS ?? join(tmpdir(), 'ferrum-copy-smoke-shots');
mkdirSync(shotDir, { recursive: true });
const realBefore = snapshotRealProfile();

const failures = [];
const check = (ok, label) => {
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${label}`);
  if (!ok) failures.push(label);
};

function makeFixture() {
  mkdirSync(src, { recursive: true });
  mkdirSync(dest, { recursive: true });
  const dirs = 40;
  for (let d = 0; d < dirs; d++) mkdirSync(join(src, `d${d}`), { recursive: true });
  for (let i = 0; i < FILES; i++) {
    const size = 600 + ((i * 7919) % 40000);
    const buf = Buffer.alloc(size, i % 251);
    buf.writeUInt32LE(i, 0);
    writeFileSync(join(src, `d${i % dirs}`, `f${i}.dat`), buf);
  }
  for (let i = 0; i < 4; i++) {
    const buf = Buffer.alloc(48 * 1024 * 1024 + i, 7 + i);
    buf.writeUInt32LE(i, 0);
    writeFileSync(join(src, `big${i}.bin`), buf);
  }
}

function walk(dir, base = dir, out = new Map()) {
  for (const e of readdirSync(dir, { withFileTypes: true })) {
    const p = join(dir, e.name);
    if (e.isDirectory()) walk(p, base, out);
    else out.set(p.slice(base.length), statSync(p).size);
  }
  return out;
}

let child;
function killApp() {
  try {
    if (child?.pid) execSync(`taskkill /PID ${child.pid} /T /F`, { stdio: 'ignore' });
  } catch {
    /* already exited */
  }
  child = undefined;
}

let browser;
let pg;
const invoke = (cmd, args) =>
  pg.evaluate(([c, a]) => window.__TAURI_INTERNALS__.invoke(c, a), [cmd, args ?? {}]);
const control = async () => (await invoke('copy_control_snapshot'))[0];
const shot = (name) => pg.screenshot({ path: join(shotDir, `${name}.png`) });
const shownThreads = async () =>
  Number((await pg.getByTestId('copy-threads').textContent()) ?? 'NaN');

async function waitUntil(fn, ms, label) {
  const end = Date.now() + ms;
  let last;
  while (Date.now() < end) {
    last = await fn();
    if (last) return last;
    await sleep(100);
  }
  throw new Error(`timeout: ${label}`);
}

async function main() {
  console.log(`fixture: ${FILES} small files + 4 x 48 MB in ${src}`);
  makeFixture();
  child = spawn(EXE, [], {
    stdio: 'ignore',
    env: {
      ...process.env,
      ...profileEnv(profile),
      WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${PORT} --no-first-run`,
    },
  });
  await waitUntil(
    async () => {
      try {
        browser = await chromium.connectOverCDP(`http://127.0.0.1:${PORT}`);
        return true;
      } catch {
        return false;
      }
    },
    40000,
    'CDP connect',
  );
  pg = browser.contexts()[0].pages()[0];
  await pg.waitForSelector('[data-pane="0"]', { timeout: 30000 });
  pg.on('console', (m) => {
    if (m.type() === 'error') console.log(`PAGE console.error: ${m.text().slice(0, 200)}`);
  });
  pg.on('pageerror', (e) => console.log(`PAGE error: ${String(e).slice(0, 200)}`));

  // Open the Background Transfer Manager, then start one real copy through the app's IPC.
  await pg.getByTitle('Operations').click();
  const t0 = Date.now();
  const id = await invoke('enqueue_copy', {
    sources: [src],
    targetDir: dest,
    policy: 'overwrite',
  });
  const state0 = await waitUntil(control, 15000, 'copy control appears');
  check(state0.running && state0.operationId === id, 'a running copy exposes its control');
  console.log(
    `start: media=${state0.media} auto=${state0.auto} threads=${state0.threads} active=${state0.activeWorkers} autoMax=${state0.autoMax}`,
  );
  check(state0.auto === true, 'default is automatic mode');
  try {
    await pg.getByRole('group', { name: 'Kontrol thread copy' }).waitFor({ timeout: 10000 });
  } catch (e) {
    await shot('fail-no-controls');
    console.log('list_operations:', JSON.stringify(await invoke('list_operations')).slice(0, 400));
    console.log(
      'panel text:',
      await pg.evaluate(() => document.querySelector('aside.panel')?.textContent ?? '(no panel)'),
    );
    throw e;
  }
  await shot('1-running-auto');

  // [+] x3: target rises, auto turns off, workers follow.
  const before = (await control()).threads;
  for (let i = 0; i < 3; i++) await pg.getByLabel('Tambah thread').click();
  const afterUp = await waitUntil(
    async () => {
      const c = await control();
      return c && c.threads >= before + 1 && !c.auto ? c : null;
    },
    8000,
    'threads up',
  );
  check(afterUp.auto === false, '[+] turns Otomatis off');
  check(
    (await shownThreads()) === afterUp.threads,
    'dialog shows the backend thread count after [+]',
  );
  const grown = await waitUntil(
    async () => {
      const c = await control();
      return c && c.activeWorkers >= Math.min(c.threads, 6) ? c : null;
    },
    8000,
    'workers grow',
  ).catch(() => control());
  console.log(`after +: threads=${grown.threads} active=${grown.activeWorkers}`);
  check(grown.activeWorkers >= 2, `workers grew live (active=${grown.activeWorkers})`);
  await shot('2-after-plus');

  // [-] to the floor: workers retire at file boundaries.
  for (let i = 0; i < 20; i++) {
    const c = await control();
    if (!c || c.threads <= 1) break;
    await pg.getByLabel('Kurangi thread').click();
  }
  const floor = await waitUntil(
    async () => {
      const c = await control();
      return c && c.threads === 1 && c.activeWorkers <= 1 ? c : null;
    },
    10000,
    'workers retire',
  ).catch(() => control());
  console.log(`after -: threads=${floor?.threads} active=${floor?.activeWorkers}`);
  check(
    floor?.threads === 1 && floor.activeWorkers <= 1,
    'workers shrink to one at file boundaries',
  );
  check((await shownThreads()) === 1, 'dialog shows 1 thread');
  await shot('3-after-minus');

  // Priority: Latar belakang (THREAD_MODE_BACKGROUND) and back.
  await pg.getByLabel('Prioritas copy').selectOption('background');
  check(
    (await waitUntil(async () => (await control())?.priority === 'background', 5000, 'prio bg')) ===
      true,
    'priority Latar belakang reaches the backend',
  );
  await shot('4-background');
  await pg.getByLabel('Prioritas copy').selectOption('normal');
  await waitUntil(async () => (await control())?.priority === 'normal', 5000, 'prio normal');

  // Back to a higher manual count, then Otomatis.
  for (let i = 0; i < 4; i++) await pg.getByLabel('Tambah thread').click();
  await pg.getByLabel('Otomatis').check();
  const autoAgain = await waitUntil(
    async () => {
      const c = await control();
      return c && c.auto ? c : null;
    },
    5000,
    'auto on',
  ).catch(() => undefined);
  check(!!autoAgain, 'Otomatis switches adaptive mode back on');
  await shot('5-auto-again');

  // Wait for the job to finish.
  const finished = await waitUntil(
    async () => {
      const ops = await invoke('list_operations');
      const op = ops.find((o) => o.operationId === id);
      return op && ['completed', 'failed', 'cancelled'].includes(op.state) ? op : null;
    },
    300000,
    'copy finished',
  );
  const secs = ((Date.now() - t0) / 1000).toFixed(1);
  console.log(`job ${finished.state} in ${secs}s files=${finished.processedFiles}`);
  check(finished.state === 'completed', 'copy completes');
  await shot('6-done');

  // Integrity: same files, same sizes, same content on a sample, no staging leftovers.
  const a = walk(src);
  const b = walk(join(dest, 'src'));
  check(a.size === b.size && a.size === FILES + 4, `file count matches (${b.size}/${a.size})`);
  let mismatched = 0;
  for (const [rel, size] of a) if (b.get(rel) !== size) mismatched++;
  check(mismatched === 0, 'every size matches');
  let contentBad = 0;
  let n = 0;
  for (const rel of a.keys()) {
    if (n++ % 40 !== 0 && !rel.includes('big')) continue;
    if (!readFileSync(join(src, rel)).equals(readFileSync(join(dest, 'src', rel)))) contentBad++;
  }
  check(contentBad === 0, 'sampled content (every 40th + all big files) is identical');
  check(![...b.keys()].some((k) => k.includes('.ferrum-stage-')), 'no staging file is left behind');
  check(!(await control()), 'the control disappears with the job');
}

try {
  await main();
} catch (e) {
  check(false, `unexpected: ${e.stack ?? e}`);
} finally {
  try {
    await browser?.close();
  } catch {
    /* ignore */
  }
  killApp();
  await sleep(1500);
  rmSync(root, { recursive: true, force: true });
  const proof = verifyRealProfile(realBefore);
  check(proof.unchanged, `real profile untouched (${proof.detail ?? proof.files ?? ''})`);
}

console.log(failures.length ? `\n${failures.length} FAILED` : '\nALL PASSED');
console.log(`screenshots: ${shotDir}`);
process.exit(failures.length ? 1 : 0);
