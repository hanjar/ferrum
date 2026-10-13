/**
 * Nested Directory Hotlist smoke over a REAL WebView2 window (Windows-only). FER-458.
 *
 * Launches the built ferrum.exe with an isolated FERRUM_PROFILE_DIR + CDP (same plumbing as
 * scripts/smoke-archive.mjs) and drives the real UI with real Tauri IPC (no mock):
 *   1. Ctrl+D opens the popup; accelerators 1 -> 6 -> 4 walk two submenus and open the folder.
 *   2. The active folder is marked with a check; a top-level accelerator opens another folder.
 *   3. "Add current folder" writes hotlist.json atomically (and keeps hotlist.json.bak).
 *   4. A command item asks for confirmation; Cancel never runs it (no approvals file appears).
 *   5. Configure > Import from Total Commander previews a COPY of wincmd.ini (temp folder, never the
 *      user's file), merges it into the draft, OK saves; the ini is byte-identical afterwards.
 *   6. Reopening the app (same profile) shows the saved hotlist (persistence through the backend).
 *
 * Fixtures live under the user's profile directory and are removed at the end. Screenshots go to
 * FERRUM_SMOKE_ARTIFACTS (default %TEMP%\ferrum-hotlist-smoke-shots) and are never committed.
 *
 * ISOLATION: FERRUM_PROFILE_DIR; the real profile is hashed before/after and the run FAILS if it changed.
 */
/* global __s -- injected into the page by installHelpers; only evaluated in WebView2. */
import { spawn, execSync } from 'node:child_process';
import {
  mkdtempSync,
  existsSync,
  rmSync,
  mkdirSync,
  writeFileSync,
  readFileSync,
  copyFileSync,
} from 'node:fs';
import { tmpdir, homedir } from 'node:os';
import { basename, join } from 'node:path';
import { createHash } from 'node:crypto';
import { profileEnv, snapshotRealProfile, verifyRealProfile } from './lib/test-profile.mjs';

const EXE = process.argv[2];
const PORT = 9226;
if (!EXE) {
  console.error('usage: node scripts/smoke-hotlist.mjs <path-to-ferrum.exe>');
  process.exit(2);
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const root = mkdtempSync(join(tmpdir(), 'ferrum-hot-smoke-'));
const realBefore = snapshotRealProfile();
const shotDir = process.env.FERRUM_SMOKE_ARTIFACTS ?? join(tmpdir(), 'ferrum-hotlist-smoke-shots');
mkdirSync(shotDir, { recursive: true });

// ---- fixtures -------------------------------------------------------------------------------------------------
const fix = join(homedir(), `ferrum-hotlist-smoke-${Date.now()}`);
const dirs = {
  proj: join(fix, 'proj'),
  beta: join(fix, 'proj', 'beta'),
  other: join(fix, 'other'),
};
for (const d of Object.values(dirs)) mkdirSync(d, { recursive: true });
writeFileSync(join(dirs.beta, 'beta.txt'), 'beta');
writeFileSync(join(dirs.other, 'other.txt'), 'other');

const configDir = join(root, 'config');
mkdirSync(configDir, { recursive: true });
const hotlistFile = join(configDir, 'hotlist.json');
writeFileSync(
  hotlistFile,
  JSON.stringify({
    version: 2,
    items: [
      {
        type: 'submenu',
        title: '&1. Fixture',
        items: [
          { type: 'dir', title: '&1. proj', path: dirs.proj },
          {
            type: 'submenu',
            title: '&6. sub',
            items: [{ type: 'dir', title: '4. &Beta', path: dirs.beta }],
          },
        ],
      },
      { type: 'separator' },
      { type: 'dir', title: '&Other', path: dirs.other },
      {
        type: 'command',
        title: 'safe-tool',
        command: 'C:\\Windows\\System32\\whoami.exe',
        args: [],
      },
    ],
  }),
);

// A COPY of the user's TC hotlist (the repo contract fixture) in a temp folder.
const iniDir = mkdtempSync(join(tmpdir(), 'ferrum-hot-ini-'));
const iniCopy = join(iniDir, 'wincmd.ini');
writeFileSync(
  iniCopy,
  `[Configuration]\nx=1\n${readFileSync(new URL('../contract-fixtures/hotlist/tc-dirmenu.ini', import.meta.url), 'utf8')}[righttabs]\nz=1\n`,
);
const iniHash = createHash('sha256').update(readFileSync(iniCopy)).digest('hex');

// ---- process + CDP plumbing -----------------------------------------------------------------------------------
let child;
let ws;
let nextId = 1;
const pending = new Map();
const pageErrors = [];
const results = [];

function launch() {
  child = spawn(EXE, [], {
    stdio: 'ignore',
    env: {
      ...process.env,
      ...profileEnv(root),
      WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${PORT} --no-first-run`,
    },
  });
}
function killApp() {
  try {
    ws?.close();
  } catch {
    /* gone */
  }
  ws = undefined;
  try {
    if (child?.pid) execSync(`taskkill /PID ${child.pid} /T /F`, { stdio: 'ignore' });
  } catch {
    /* already exited */
  }
  child = undefined;
}
function send(method, params = {}) {
  const id = nextId++;
  ws.send(JSON.stringify({ id, method, params }));
  return new Promise((resolve, reject) => {
    pending.set(id, { resolve, reject });
    setTimeout(() => {
      if (pending.has(id)) {
        pending.delete(id);
        reject(new Error(`CDP timeout: ${method}`));
      }
    }, 20000);
  });
}
async function evaluate(expression) {
  const res = await send('Runtime.evaluate', {
    expression: `(window.__s || (${installHelpers.toString()})(), ${expression})`,
    returnByValue: true,
    awaitPromise: true,
  });
  if (res.exceptionDetails) {
    throw new Error(
      `page eval failed: ${res.exceptionDetails.exception?.description ?? res.exceptionDetails.text}`,
    );
  }
  return res.result?.value;
}
const page = (fn, ...args) => evaluate(`(${fn.toString()})(...${JSON.stringify(args)})`);
async function waitFor(expression, timeoutMs, label) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const value = await evaluate(expression);
      if (value) return value;
    } catch {
      /* page not ready yet */
    }
    await sleep(150);
  }
  throw new Error(`timeout waiting for ${label ?? expression}`);
}
async function connect() {
  const deadline = Date.now() + 30000;
  let target;
  while (Date.now() < deadline && !target) {
    try {
      const res = await fetch(`http://127.0.0.1:${PORT}/json/list`);
      target = (await res.json()).find((t) => t.type === 'page' && t.webSocketDebuggerUrl);
    } catch {
      /* not up yet */
    }
    if (!target) await sleep(300);
  }
  if (!target) throw new Error('CDP endpoint never appeared');
  ws = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise((resolve, reject) => {
    ws.onopen = resolve;
    ws.onerror = reject;
  });
  ws.onmessage = (event) => {
    const msg = JSON.parse(event.data);
    if (msg.method === 'Runtime.exceptionThrown') {
      pageErrors.push(
        String(
          msg.params.exceptionDetails?.exception?.description ?? msg.params.exceptionDetails?.text,
        )
          .split('\n')[0]
          .slice(0, 200),
      );
      return;
    }
    if (msg.id && pending.has(msg.id)) {
      const { resolve, reject } = pending.get(msg.id);
      pending.delete(msg.id);
      if (msg.error) reject(new Error(msg.error.message));
      else resolve(msg.result);
    }
  };
  await send('Runtime.enable');
  await send('Page.enable');
}
let shotN = 0;
async function shot(name) {
  await sleep(450); // let the pop animation settle so the artifact shows the real state
  try {
    const res = await send('Page.captureScreenshot', { format: 'png' });
    writeFileSync(
      join(shotDir, `${String(++shotN).padStart(2, '0')}-${name}.png`),
      Buffer.from(res.data, 'base64'),
    );
  } catch (e) {
    console.log(`WARN screenshot ${name} failed: ${e}`);
  }
}

const KEYS = {
  Enter: [13, 'Enter'],
  Escape: [27, 'Escape'],
  d: [68, 'KeyD'],
  1: [49, 'Digit1'],
  4: [52, 'Digit4'],
  6: [54, 'Digit6'],
  o: [79, 'KeyO'],
  Backspace: [8, 'Backspace'],
};
async function key(k, { ctrl = false } = {}) {
  const [vk, code] = KEYS[k];
  const base = {
    key: k,
    code,
    windowsVirtualKeyCode: vk,
    nativeVirtualKeyCode: vk,
    modifiers: ctrl ? 2 : 0,
  };
  await send('Input.dispatchKeyEvent', { type: 'rawKeyDown', ...base });
  await send('Input.dispatchKeyEvent', { type: 'keyUp', ...base });
}

/* In-page helpers (serialized into the page; must not close over Node scope). */
function installHelpers() {
  const q = (s, r = document) => r.querySelector(s);
  const qa = (s, r = document) => [...r.querySelectorAll(s)];
  const S = {
    q,
    qa,
    crumbLast: () => qa('.pane[data-pane="0"] .crumb').at(-1)?.textContent?.trim() ?? '',
    menu: () => q('[role="menu"][aria-label="Directory Hotlist"]'),
    item: (name) => qa('[role="menuitem"]').find((r) => r.getAttribute('aria-label') === name),
    click: (el) => {
      el.dispatchEvent(new MouseEvent('mousedown', { bubbles: true, cancelable: true }));
      el.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true }));
    },
    btn: (text, root = document) =>
      qa('button', root).find((b) => b.textContent.trim().startsWith(text)),
    setInput: (el, v) => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(el, v);
      el.dispatchEvent(new Event('input', { bubbles: true }));
    },
    dialog: () => q('[role="dialog"]'),
    activate: () =>
      q('.pane[data-pane="0"] [data-path]')?.dispatchEvent(
        new MouseEvent('mousedown', { bubbles: true, cancelable: true }),
      ),
  };
  window.__s = S;
}

const norm = (p) =>
  String(p ?? '')
    .replace(/[\\/]+$/, '')
    .toLowerCase();

/** Ctrl+D until the popup is really there (a stray toggle must not hide it). */
async function openMenu() {
  for (let i = 0; i < 3; i++) {
    await key('d', { ctrl: true });
    if (await waitFor(`!!__s.menu()`, 2500).catch(() => false)) return;
    await page(() => __s.activate());
  }
  throw new Error('popup did not open');
}

function check(name, ok, detail = '') {
  results.push({ name, ok });
  console.log(`${ok ? 'PASS' : 'FAIL'} ${name}${detail ? ` - ${detail}` : ''}`);
}

async function scenario() {
  await waitFor(`__s.qa('.pane[data-pane="0"] .crumb').length > 0`, 30000, 'pane crumbs');
  await sleep(1500); // let the hotlist backend load finish
  await shot('start');

  // 1. Ctrl+D, 1 -> 6 -> 4
  await key('d', { ctrl: true });
  check('Ctrl+D opens the popup', await waitFor(`!!__s.menu()`, 5000, 'menu').catch(() => false));
  check(
    'top level shows folder, submenu, command and footer actions',
    await page(() =>
      ['Fixture', 'Other', 'safe-tool', 'Add current folder', 'Configure…'].every(
        (n) => !!__s.item(n),
      ),
    ),
  );
  await shot('menu-open');
  await key('1');
  check(
    'accelerator 1 opens the submenu',
    await waitFor(`!!__s.item('sub')`, 3000).catch(() => false),
  );
  await key('6');
  check(
    'accelerator 6 opens the nested submenu',
    await waitFor(`!!__s.item('Beta')`, 3000).catch(() => false),
  );
  await shot('nested-submenu');
  await key('4');
  check(
    'accelerator 4 opens the folder in the active pane',
    await waitFor(`__s.crumbLast() === 'beta'`, 8000).catch(() => false),
    'crumb=beta',
  );
  check('popup closed after choosing', await page(() => !__s.menu()));

  // 2. check mark + top-level accelerator
  await openMenu();
  await key('1');
  await waitFor(`!!__s.item('sub')`, 3000);
  await key('6');
  await waitFor(`!!__s.item('Beta')`, 3000);
  check(
    'active folder is marked with a check',
    await page(() => __s.item('Beta')?.textContent.includes('✓')),
  );
  await shot('check-mark');
  await key('Escape');
  await key('Escape');
  await key('Escape');
  await waitFor(`!__s.menu()`, 3000);
  await openMenu();
  await key('o');
  check(
    'top-level accelerator opens another folder',
    await waitFor(`__s.crumbLast() === 'other'`, 8000).catch(() => false),
  );

  // 3. Add current folder: go up to the (unlisted) fixture root first, because Add is disabled for a
  //    folder that is already in the hotlist.
  await page(() => __s.activate());
  await key('Backspace');
  await waitFor(`__s.crumbLast() === ${JSON.stringify(basename(fix))}`, 8000, 'parent folder');
  await openMenu();
  const before = readFileSync(hotlistFile, 'utf8');
  await page(() => __s.click(__s.item('Add current folder')));
  await sleep(1200);
  const after = readFileSync(hotlistFile, 'utf8');
  check(
    'hotlist.json now lists the folder',
    after !== before && norm(JSON.parse(after).items.at(-1)?.path) === norm(fix),
    `last=${JSON.stringify(JSON.parse(after).items.at(-1))}`,
  );
  check('a .bak of the previous version exists', existsSync(`${hotlistFile}.bak`));
  check(
    'no temp file left behind',
    !(await import('node:fs')).readdirSync(configDir).some((f) => f.includes('.tmp-')),
  );

  // 4. command item: confirmation, Cancel never runs it
  await openMenu();
  await page(() => __s.click(__s.item('safe-tool')));
  check(
    'command item asks for confirmation first',
    await waitFor(`/whoami/.test(__s.dialog()?.innerText ?? '')`, 5000).catch(() => false),
  );
  check(
    'confirmation shows the exact command line',
    await page(
      () =>
        /exactly the command line/.test(__s.dialog()?.innerText ?? '') &&
        !!__s.q('[data-testid="command-line"][dir="ltr"]'),
    ),
  );
  await shot('command-confirm');
  await page(() => __s.click(__s.btn('Cancel', __s.dialog())));
  await sleep(500);
  check('dialog closed on Cancel', await page(() => !__s.dialog()));
  check('nothing was approved', !existsSync(join(configDir, 'hotlist-approvals.json')));

  // 5. Configure > Import from a COPY of wincmd.ini
  await openMenu();
  await page(() => __s.click(__s.item('Configure…')));
  await waitFor(`!!__s.dialog()`, 5000, 'configure dialog');
  await shot('configure');
  await page(() => __s.click(__s.btn('Import from Total Commander', __s.dialog())));
  await waitFor(`!!__s.q('input[aria-label="wincmd.ini path"]')`, 3000);
  await page((p) => __s.setInput(__s.q('input[aria-label="wincmd.ini path"]'), p), iniCopy);
  await page(() => __s.click(__s.btn('Preview', __s.dialog())));
  check(
    'import preview lists the Total Commander items',
    await waitFor(
      `/Telegram/.test(__s.q('[aria-label="Import preview"]')?.innerText ?? '') && /Downloads/.test(__s.q('[aria-label="Import preview"]').innerText)`,
      8000,
    ).catch(() => false),
  );
  await shot('import-preview');
  check('hotlist.json untouched by the preview', readFileSync(hotlistFile, 'utf8') === after);
  await page(() =>
    __s.click(
      __s.btn('Import', __s.dialog()).textContent.trim() === 'Import'
        ? __s.btn('Import', __s.dialog())
        : null,
    ),
  );
  await sleep(500);
  await page(() => __s.click(__s.btn('OK', __s.dialog())));
  await waitFor(`!__s.dialog()`, 5000);
  await sleep(800);
  const saved = JSON.parse(readFileSync(hotlistFile, 'utf8'));
  const flat = JSON.stringify(saved);
  check(
    'OK saved the imported tree (nested, with spaces and @ in paths)',
    flat.includes('Telegram Desktop') && flat.includes('eve@example.com') && saved.items.length > 4,
  );
  check(
    'imported command item was not approved or run',
    !existsSync(join(configDir, 'hotlist-approvals.json')),
  );
  check(
    'the wincmd.ini copy is byte-identical',
    createHash('sha256').update(readFileSync(iniCopy)).digest('hex') === iniHash,
  );
}

async function main() {
  let failed = false;
  try {
    launch();
    await connect();
    await scenario();
    // 6. persistence: restart on the same profile
    killApp();
    await sleep(1500);
    launch();
    await connect();
    await waitFor(`__s.qa('.pane[data-pane="0"] .crumb').length > 0`, 30000, 'pane crumbs');
    await sleep(1500);
    await key('d', { ctrl: true });
    await waitFor(`!!__s.menu()`, 5000);
    check(
      'saved hotlist (incl. imported items) is back after a restart',
      await page(
        () =>
          !!__s.item('Telegram') ||
          __s.qa('[role="menuitem"]').some((r) => /Telegram/.test(r.textContent)),
      ),
    );
    await shot('after-restart');
  } catch (e) {
    failed = true;
    console.log(`FAIL scenario crashed - ${e.message}`);
    await shot('crash').catch(() => {});
  } finally {
    killApp();
    await sleep(800);
    const proof = verifyRealProfile(realBefore);
    check('real profile untouched', proof.unchanged, proof.detail);
    for (const d of [fix, root, iniDir]) {
      try {
        rmSync(d, { recursive: true, force: true });
      } catch {
        /* best effort */
      }
    }
    if (pageErrors.length) console.log(`page errors: ${[...new Set(pageErrors)].join(' | ')}`);
    console.log(`screenshots: ${shotDir}`);
  }
  const bad = results.filter((r) => !r.ok);
  console.log(`\n${results.length - bad.length}/${results.length} checks passed`);
  process.exit(failed || bad.length ? 1 : 0);
}
void copyFileSync;
await main();
