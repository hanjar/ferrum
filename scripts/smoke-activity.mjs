/**
 * ACT smoke E2E over a REAL WebView2 window (Windows-only).
 *
 * Launches the built ferrum.exe with an isolated FERRUM_PROFILE_DIR + CDP and drives the
 * real UI. Scenarios: first-run disclosure (+ack, no re-show after restart), visit -> Recent, palette history,
 * Favorites, pause recording, Activity page (7/30/90, coverage), hide/forget, private session, rename identity,
 * persistence across a real restart, clear history (preview + confirm).
 *
 * Privacy of the log: only counts/booleans are printed; real path names stay inside the page. The fixture folder is
 * created under the user's profile directory (the pane can only reach hidden %TEMP% via showHidden) and removed at
 * the end.
 *
 * ISOLATION: FERRUM_PROFILE_DIR roots config.json (prefs/tab session), AI stores, the Activity DB and WebView2 data in a
 * temp profile (scripts/lib/test-profile.mjs); the real profile is hashed before/after and the run FAILS if it changed.
 * Nothing here opens a file (Enter/double-click on a file would
 * launch an external app) - only folders are activated.
 */
/* global __s -- injected into the page by installHelpers; only evaluated in WebView2. */
import { spawn, execSync } from 'node:child_process';
import { mkdtempSync, existsSync, rmSync, mkdirSync, writeFileSync } from 'node:fs';
import { tmpdir, homedir } from 'node:os';
import { join, parse } from 'node:path';
import { profileEnv, snapshotRealProfile, verifyRealProfile } from './lib/test-profile.mjs';

const EXE = process.argv[2];
const PORT = 9223;
if (!EXE) {
  console.error('usage: node scripts/smoke-activity.mjs <path-to-ferrum.exe>');
  process.exit(2);
}

const root = mkdtempSync(join(tmpdir(), 'ferrum-act-smoke-'));
// Isolation is FERRUM_PROFILE_DIR (backend); the real profile is only hashed before/after as proof.
const realBefore = snapshotRealProfile();

const fxRoot = join(homedir(), `ferrum-smoke-fx-${Date.now().toString(36)}`);
const DIRS = ['alpha', 'beta', 'gamma', 'delta', 'epsilon'];
for (const d of DIRS) {
  mkdirSync(join(fxRoot, d), { recursive: true });
  writeFileSync(join(fxRoot, d, 'note.txt'), 'x');
}
const fxName = parse(fxRoot).base;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const results = [];
const check = (name, ok, detail = '') => {
  results.push({ name, ok });
  console.log(`${ok ? 'PASS' : 'FAIL'} ${name}${detail ? ` - ${detail}` : ''}`);
  return ok;
};
const startedAt = Date.now();

// ---- process + CDP plumbing -------------------------------------------------
let child;
let ws;
let nextId = 1;
let paneIdx = 0; // pane that holds the fixture walk (the one whose path can reach the home folder)
const pending = new Map();

function launch() {
  child = spawn(EXE, [], {
    stdio: 'ignore',
    env: {
      ...process.env,
      ...profileEnv(root),
      // USERPROFILE/TEMP must stay REAL: overriding them prevents WebView2 from starting at all.
      WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${PORT} --no-first-run`,
    },
  });
}

function killApp() {
  try {
    ws?.close();
  } catch {
    /* socket already gone */
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
    }, 15000);
  });
}

async function evaluate(expression) {
  // The page may reload during startup; helpers are (re)installed lazily whenever they are missing.
  const guarded = `(window.__paneIdx = ${paneIdx}, window.__s || (${installHelpers.toString()})(), ${expression})`;
  const res = await send('Runtime.evaluate', {
    expression: guarded,
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

/** Run a self-contained function in the page with JSON-serializable args. */
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
      /* endpoint not up yet */
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
    if (msg.id && pending.has(msg.id)) {
      const { resolve, reject } = pending.get(msg.id);
      pending.delete(msg.id);
      if (msg.error) reject(new Error(msg.error.message));
      else resolve(msg.result);
    }
  };
  await send('Runtime.enable');
}

// In-page helpers (serialized into the page; they must not close over Node scope).
function installHelpers() {
  const q = (s, r = document) => r.querySelector(s);
  const qa = (s, r = document) => [...r.querySelectorAll(s)];
  const last = (p) =>
    String(p)
      .replace(/[\\/]+$/, '')
      .split(/[\\/]/)
      .pop();
  const norm = (p) => String(p).replace(/\//g, '\\').replace(/\\+$/, '').toLowerCase();
  const S = {
    q,
    qa,
    last,
    norm,
    pane: () => q(`.pane[data-pane="${window.__paneIdx}"]`),
    rows: () => qa(`.pane[data-pane="${window.__paneIdx}"] [data-path]`),
    row: (name) => S.rows().find((r) => last(r.getAttribute('data-path')) === name),
    rowByPath: (p) => S.rows().find((r) => norm(r.getAttribute('data-path')) === norm(p)),
    crumbs: () => qa(`.pane[data-pane="${window.__paneIdx}"] .crumb`),
    paneCrumbs: (i) => qa(`.pane[data-pane="${i}"] .crumb`),
    crumbLast: () => S.crumbs().at(-1)?.textContent?.trim() ?? '',
    busy: () => !!q('[aria-busy="true"]'),
    group: (t) =>
      q(`.places [role="group"][aria-label="${t}"]`) ??
      qa('.places .section').find((e) => q('.sectionTitle', e)?.textContent === t),
    names: (t) =>
      qa('.favRow .label', S.group(t) ?? document.createElement('i')).map((e) =>
        e.textContent.trim(),
      ),
    hasEmptyMsg: (t) => !!q('.favEmpty', S.group(t) ?? document.createElement('i')),
    // mousedown first: a real pointer press is what makes a pane the active one (ui.activate).
    dblclick: (el) => {
      el.dispatchEvent(new MouseEvent('mousedown', { bubbles: true, cancelable: true }));
      el.dispatchEvent(new MouseEvent('dblclick', { bubbles: true, cancelable: true }));
    },
    click: (el) => {
      el.dispatchEvent(new MouseEvent('mousedown', { bubbles: true, cancelable: true }));
      el.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true }));
    },
    activate: () =>
      S.pane()?.dispatchEvent(new MouseEvent('mousedown', { bubbles: true, cancelable: true })),
    keyWin: (key, o = {}) =>
      window.dispatchEvent(
        new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true, ...o }),
      ),
    setInput: (input, v) => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(input, v);
      input.dispatchEvent(new Event('input', { bubbles: true }));
    },
    actPage: () => q('[role="dialog"][aria-label="Activity"]'),
    actRows: () => qa('.row[role="option"]', S.actPage() ?? document.createElement('i')),
    actNames: () => S.actRows().map((r) => q('.name', r)?.textContent.trim()),
    settingsOpen: () => !!qa('.modal').find((m) => m.textContent.includes('Activity & privacy')),
    checkBox: (labelPart) =>
      qa('.check')
        .find((l) => l.textContent.includes(labelPart))
        ?.querySelector('input'),
    btn: (text, root = document) =>
      qa('button', root).find((b) => b.textContent.trim().startsWith(text)),
  };
  window.__s = S;
}

// ---- Node-side UI drivers ---------------------------------------------------
const idle = () => waitFor(`!__s.busy()`, 10000, 'listing idle');

async function enterDir(name) {
  const ok = await page((n) => {
    const r = __s.row(n);
    if (!r) return false;
    __s.dblclick(r);
    return true;
  }, name);
  if (!ok) throw new Error(`dir row not found (${name.length} chars)`);
  await waitFor(`__s.crumbLast() === ${JSON.stringify(name)}`, 8000, 'crumb after enter');
  await idle();
}
async function crumbUp() {
  const before = await evaluate(`__s.crumbs().length`);
  await page(() => {
    const c = __s.crumbs();
    __s.click(c[c.length - 2]);
  });
  await waitFor(`__s.crumbs().length === ${before - 1}`, 8000, 'crumb up');
  await idle();
}
async function gotoFixtureRoot() {
  // The drive-root crumb ("C:") does not list (os error 123, see verification.md), so start from the home crumb.
  const home = parse(homedir()).base;
  const found = await waitFor(
    `([0, 1].find((i) => __s.paneCrumbs(i).some((x) => x.textContent.trim() === ${JSON.stringify(home)})) ?? -1) + 1`,
    20000,
    'home crumb in either pane',
  ).catch(() => 0);
  paneIdx = found > 0 ? found - 1 : 0;
  const clicked = await page((h) => {
    const c = __s.crumbs();
    const i = c.findIndex((x) => x.textContent.trim() === h);
    if (i < 0) return false;
    __s.click(c[i]);
    return true;
  }, home);
  if (!clicked) throw new Error('home crumb not found in the pane path');
  await waitFor(
    `__s.crumbLast() === ${JSON.stringify(home)} && !__s.busy() && __s.rows().length > 0`,
    12000,
    'home listing',
  );
  await page((n) => __s.dblclick(__s.row(n)), fxName);
  await waitFor(
    `__s.crumbLast() === ${JSON.stringify(fxName)} && !!__s.row('alpha')`,
    10000,
    'fixture root listing',
  );
  await idle();
}
/** Visit a fixture child and return to the fixture root (two explicit navigations). */
async function visit(name) {
  await enterDir(name);
  await crumbUp();
}
const recentNames = () => evaluate(`__s.names('Recent')`);
const frequentNames = () => evaluate(`__s.names('Frequent')`);

async function openSettings() {
  await evaluate(`document.querySelector('[title="Settings (Ctrl+,)"]')?.click()`);
  await waitFor(`__s.settingsOpen()`, 6000, 'settings open');
}
async function closeSettings() {
  await evaluate(
    `__s.qa('.modal').find((m) => m.textContent.includes('Activity & privacy'))?.querySelector('button[title="Close"]')?.click()`,
  );
  await waitFor(`!document.querySelector('.modal')`, 5000, 'settings closed');
}
async function openActivity() {
  await evaluate(`document.querySelector('[title="Activity"]')?.click()`);
  await waitFor(`!!__s.actPage()`, 6000, 'activity page open');
  await waitFor(
    `__s.actPage() && __s.actPage().querySelector('.list')?.getAttribute('aria-busy') !== 'true' && !!document.querySelector('.chartTitle, .banner.err')`,
    10000,
    'activity overview',
  );
}
async function closeActivity() {
  await page(() => __s.actPage()?.querySelector('button[aria-label="Close"]')?.click());
  await waitFor(`!__s.actPage()`, 5000, 'activity page closed');
}
async function confirmScope(preview) {
  await waitFor(
    `(() => { const b = __s.btn('Konfirmasi'); return !!b && !b.disabled; })()`,
    10000,
    'scope preview ready',
  );
  const text = await evaluate(`document.querySelector('.scopeCount')?.innerText ?? ''`);
  const m = /(\d+)\+?\s*item\s*·\s*(\d+)\s*kunjungan/.exec(text.replace(/\s+/g, ' '));
  preview.items = m ? Number(m[1]) : -1;
  preview.episodes = m ? Number(m[2]) : -1;
  await evaluate(`__s.btn('Konfirmasi')?.click()`);
  await waitFor(`!document.querySelector('[role="alertdialog"]')`, 10000, 'scope dialog closed');
}

// ---- run --------------------------------------------------------------------
try {
  launch();
  await connect();

  // S1. Startup: restore is not a visit -> Recent empty (section is always shown with an empty message).
  await waitFor(`__s.crumbs().length > 0`, 25000, 'pane rendered');
  await waitFor(`!!__s.group('Recent')`, 15000, 'sidebar Recent group');
  check(
    'startup Recent empty (empty message shown)',
    (await recentNames()).length === 0 && (await evaluate(`__s.hasEmptyMsg('Recent')`)) === true,
  );

  // S2. First-run disclosure: shown, ack, gone.
  const discShown = await waitFor(
    `!!document.querySelector('[role="region"][aria-label*="Pemberitahuan pencatatan"]')`,
    8000,
    'disclosure',
  ).catch(() => false);
  check('first-run disclosure shown', discShown === true);
  await evaluate(`__s.btn('Mengerti')?.click()`);
  const discGone = await waitFor(
    `!document.querySelector('[role="region"][aria-label*="Pemberitahuan pencatatan"]')`,
    6000,
    'disclosure ack',
  ).catch(() => false);
  check('disclosure acknowledged and hidden', discGone === true);

  // S3. Navigate to the safe fixture folder, then make explicit visits.
  await gotoFixtureRoot();
  check('navigated to fixture folder via UI', true);
  for (const d of ['alpha', 'beta', 'gamma', 'delta']) await visit(d);
  await visit('alpha'); // second visit
  await waitFor(
    `__s.names('Recent').includes('gamma') && __s.names('Recent').includes('delta')`,
    10000,
    'Recent items',
  );
  let rec = await recentNames();
  check(
    'visits add Recent items',
    ['alpha', 'beta', 'gamma', 'delta'].every((n) => rec.includes(n)),
    `${rec.length} items`,
  );

  // S4. Palette history provider (query by fixture name, click the history result).
  await evaluate(`__s.keyWin('k', { ctrlKey: true })`);
  await waitFor(`!!document.querySelector('.palette input')`, 5000, 'palette open');
  await page(() => __s.setInput(document.querySelector('.palette input'), 'beta'));
  await waitFor(
    `document.querySelector('.palette')?.innerText.includes('Pernah diakses di Ferrum')`,
    8000,
    'palette history results',
  );
  await evaluate(`(() => {
    const o = [...document.querySelectorAll('.palette [role="option"]')].find(e => e.innerText.includes('beta'));
    if (!o) return false;
    o.dispatchEvent(new MouseEvent('mousemove', { bubbles: true }));
    o.click();
    return true;
  })()`);
  const palNav = await waitFor(
    `!document.querySelector('.palette') && __s.crumbLast() === 'beta'`,
    6000,
    'palette navigation',
  ).catch(() => false);
  check('palette history result navigates to folder', palNav === true);
  await idle();
  await crumbUp();

  // S5. Favorites: context-menu pin of a never-visited folder, clear keeps it (S14), unpin at the end.
  const favCount = () =>
    evaluate(`__s.qa('.favRow', __s.group('Favorites') ?? document.createElement('i')).length`);
  const menuPin = await page(async () => {
    const row = __s.row('epsilon');
    row.dispatchEvent(
      new MouseEvent('contextmenu', {
        bubbles: true,
        cancelable: true,
        clientX: 300,
        clientY: 300,
      }),
    );
    await new Promise((r) => setTimeout(r, 120));
    const pin = [...document.querySelectorAll('[role="menuitem"]')].find((b) =>
      b.textContent?.includes('Pin to Favorites'),
    );
    if (!pin) return 'no-pin-item';
    pin.click();
    return 'pinned';
  });
  check('context menu offers Pin to Favorites', menuPin === 'pinned', String(menuPin));
  await waitFor(
    `(__s.qa('.favRow', __s.group('Favorites') ?? document.createElement('i')).length) > 0`,
    8000,
    'Favorites row',
  ).catch(() => undefined);
  const favAfterPin = await favCount();
  check('pin adds a Favorites item', favAfterPin === 1, `count=${favAfterPin}`);

  // S6. Pause recording: no new visits while paused.
  await openSettings();
  check(
    'recording toggle is on by default',
    (await evaluate(`__s.checkBox('Catat folder')?.checked`)) === true,
  );
  await evaluate(`__s.checkBox('Catat folder').click()`);
  await sleep(500);
  await closeSettings();
  const before = (await recentNames()).length;
  await enterDir('beta');
  await crumbUp();
  await sleep(1200);
  const afterPaused = await recentNames();
  check(
    'paused recording does not add/alter Recent',
    afterPaused.length === before,
    `${before} -> ${afterPaused.length}`,
  );
  await openSettings();
  await evaluate(`__s.checkBox('Catat folder').click()`);
  await sleep(500);
  check('recording re-enabled', (await evaluate(`__s.checkBox('Catat folder')?.checked`)) === true);
  await closeSettings();

  // S7. Activity page: 7/30/90 filters, items visible, coverage label.
  await openActivity();
  for (const d of [7, 30, 90]) {
    await evaluate(`__s.btn('${d} hari', __s.actPage())?.click()`);
    await waitFor(
      `__s.btn('${d} hari', __s.actPage())?.getAttribute('aria-checked') === 'true' && __s.actPage().querySelector('.list')?.getAttribute('aria-busy') !== 'true'`,
      8000,
      `filter ${d}`,
    );
    await sleep(250);
    const names = await evaluate(`__s.actNames()`);
    const coverage = await evaluate(
      `document.querySelector('[data-testid="coverage"]')?.textContent.trim().length ?? 0`,
    );
    check(
      `Activity page ${d} days: items visible + coverage label`,
      ['alpha', 'beta', 'gamma', 'delta'].every((n) => names.includes(n)) && coverage > 0,
      `${names.length} items, coverage ${coverage} chars`,
    );
  }
  await closeActivity();

  // S8. Hide from sidebar: gone from Recent, still on the Activity page (marked hidden).
  await evaluate(`(() => {
    const row = [...document.querySelectorAll('.favRow')].find(r => r.querySelector('.label')?.textContent.trim() === 'gamma' && r.closest('[aria-label="Recent"]'));
    row?.querySelector('.unpin')?.click();
  })()`);
  const hiddenFromRecent = await waitFor(
    `!__s.names('Recent').includes('gamma')`,
    8000,
    'gamma hidden',
  ).catch(() => false);
  check('hide (X) removes item from sidebar Recent', hiddenFromRecent === true);
  await openActivity();
  const hidInfo = await evaluate(
    `(() => { const r = __s.actRows().find(x => x.querySelector('.name')?.textContent.trim() === 'gamma'); return r ? (r.querySelector('.tag')?.textContent ?? 'visible-no-tag') : null; })()`,
  );
  check(
    'hidden item still listed on Activity page (marked)',
    hidInfo !== null && hidInfo.includes('disembunyikan'),
    `row=${hidInfo === null ? 'missing' : 'present'}`,
  );

  // S9. Forget (preview + confirm) from the Activity page: gone from page and sidebar.
  await evaluate(
    `__s.click(__s.actRows().find(x => x.querySelector('.name')?.textContent.trim() === 'beta'))`,
  );
  await waitFor(`!!__s.btn('Lupakan item')`, 5000, 'forget button');
  await evaluate(`__s.btn('Lupakan item').click()`);
  const forgetPrev = {};
  await confirmScope(forgetPrev);
  await waitFor(`!__s.actNames().includes('beta')`, 8000, 'beta gone from page').catch(
    () => undefined,
  );
  const forgetPage = !(await evaluate(`__s.actNames()`)).includes('beta');
  await closeActivity();
  const forgetSidebar =
    !(await recentNames()).includes('beta') && !(await frequentNames()).includes('beta');
  check(
    'forget removes item from Activity page and sidebar',
    forgetPage && forgetSidebar,
    `preview items=${forgetPrev.items} episodes=${forgetPrev.episodes}; page=${forgetPage} sidebar=${forgetSidebar}`,
  );

  // S10. Rename a visited folder via F2: identity must be kept (no duplicate Recent entry).
  const recBeforeRename = await recentNames();
  await evaluate(`__s.click(__s.row('delta'))`);
  await sleep(200);
  await evaluate(`__s.keyWin('F2')`);
  const dlg = `[...document.querySelectorAll('h2')].find((h) => h.textContent.trim() === 'Rename')?.parentElement`;
  const renameOpen = await waitFor(
    `!!(${dlg})?.querySelector('input')`,
    4000,
    'rename dialog',
  ).catch(() => false);
  check('F2 opens the Rename dialog on the cursor folder', renameOpen === true);
  if (renameOpen) {
    await evaluate(
      `(() => { const d = ${dlg}; __s.setInput(d.querySelector('input'), 'delta-renamed'); })()`,
    );
    await sleep(150);
    await evaluate(
      `[...document.querySelectorAll('button.primary')].find((b) => b.textContent.trim() === 'Rename')?.click()`,
    );
    await waitFor(`!!__s.row('delta-renamed')`, 8000, 'renamed row').catch(() => undefined);
    await sleep(800);
    const renamedOk = await evaluate(`!!__s.row('delta-renamed') && !__s.row('delta')`);
    check('folder renamed on disk via UI', renamedOk === true);
    const recAfterRename = await recentNames();
    await visit('delta-renamed');
    await sleep(600);
    const recAfterVisit = await recentNames();
    const deltaEntries = recAfterVisit.filter((n) => n === 'delta' || n === 'delta-renamed');
    check(
      'rename keeps identity: Recent has one entry (no duplicate) after revisit',
      deltaEntries.length === 1 &&
        deltaEntries[0] === 'delta-renamed' &&
        recAfterVisit.length === recBeforeRename.length,
      `before-revisit: old-name-still-listed=${recAfterRename.includes('delta')} new-name-listed=${recAfterRename.includes('delta-renamed')} (sidebar refresh lag if old); entries(delta*)=${deltaEntries.length} total ${recBeforeRename.length} -> ${recAfterVisit.length}`,
    );
  }

  // S11. Private session: visits are not recorded in this window.
  await openSettings();
  await evaluate(`__s.checkBox('Sesi privat').click()`);
  await sleep(400);
  await closeSettings();
  const privHidden = await waitFor(`!__s.group('Recent')`, 6000, 'Recent hidden in private').catch(
    () => false,
  );
  check('private session hides Recent section', privHidden === true);
  await visit('epsilon');
  await sleep(1000);
  await openSettings();
  await evaluate(`__s.checkBox('Sesi privat').click()`);
  await sleep(400);
  await closeSettings();
  await waitFor(`!!__s.group('Recent')`, 6000, 'Recent back');
  await sleep(600);
  const recPriv = await recentNames();
  await openActivity();
  const pageNamesPriv = await evaluate(`__s.actNames()`);
  await closeActivity();
  check(
    'private session visit not recorded (Recent + Activity page)',
    !recPriv.includes('epsilon') && !pageNamesPriv.includes('epsilon'),
    `recent=${recPriv.includes('epsilon')} page=${pageNamesPriv.includes('epsilon')}`,
  );

  // S12. Real restart: disclosure stays acknowledged, Recent/Frequent/Favorites persist.
  await sleep(1500);
  const snapRecent = [...(await recentNames())].sort();
  const snapFreq = [...(await frequentNames())].sort();
  const snapFav = await favCount();
  check(
    'pre-restart snapshot has items',
    snapRecent.length > 0,
    `recent=${snapRecent.length} frequent=${snapFreq.length} fav=${snapFav}`,
  );
  killApp();
  await sleep(2500);
  launch();
  await connect();
  await waitFor(`__s.crumbs().length > 0`, 25000, 'pane rendered after restart');
  await evaluate(`__s.activate()`);
  await waitFor(`__s.names('Recent').length > 0`, 15000, 'Recent after restart').catch(
    () => undefined,
  );
  await sleep(800);
  const rRecent = [...(await recentNames())].sort();
  const rFreq = [...(await frequentNames())].sort();
  const rFav = await favCount();
  check(
    'Recent persists after real restart',
    JSON.stringify(rRecent) === JSON.stringify(snapRecent),
    `${snapRecent.length} -> ${rRecent.length}`,
  );
  check(
    'Frequent unchanged after real restart',
    JSON.stringify(rFreq) === JSON.stringify(snapFreq),
    `${snapFreq.length} -> ${rFreq.length}${snapFreq.length === 0 ? ' (VACUOUS: Frequent was empty; not exercised)' : ''}`,
  );
  check('Favorites persist after real restart', rFav === snapFav, `${snapFav} -> ${rFav}`);
  check(
    'disclosure does not reappear after restart',
    (await evaluate(
      `!document.querySelector('[role="region"][aria-label*="Pemberitahuan pencatatan"]')`,
    )) === true,
  );
  check(
    'hidden/forgotten items stay gone after restart',
    !rRecent.includes('gamma') && !rRecent.includes('beta'),
  );

  // S13. Clear history: cancel keeps data, preview then confirm empties everything (Favorites stay).
  await openSettings();
  await evaluate(`__s.btn('Hapus semua riwayat')?.click()`);
  await waitFor(`!!document.querySelector('[role="alertdialog"]')`, 6000, 'clear preview');
  await waitFor(
    `!!__s.btn('Konfirmasi') && !__s.btn('Konfirmasi').disabled`,
    10000,
    'clear preview ready',
  );
  const previewText = await evaluate(
    `document.querySelector('.scopeCount')?.innerText.replace(/\\s+/g, ' ') ?? ''`,
  );
  const pm = /(\d+)\+?\s*item\s*·\s*(\d+)\s*kunjungan/.exec(previewText);
  check(
    'clear history shows preview with counts before confirm',
    !!pm && Number(pm[1]) > 0,
    pm ? `items=${pm[1]} episodes=${pm[2]}` : 'no counts',
  );
  await evaluate(`__s.btn('Batal')?.click()`);
  await waitFor(`!document.querySelector('[role="alertdialog"]')`, 5000, 'clear cancelled');
  check('cancel keeps history', (await recentNames()).length > 0);
  await evaluate(`__s.btn('Hapus semua riwayat').click()`);
  const clearPrev = {};
  await confirmScope(clearPrev);
  await closeSettings();
  const emptied = await waitFor(
    `__s.names('Recent').length === 0 && __s.names('Frequent').length === 0`,
    8000,
    'sidebar emptied',
  ).catch(() => false);
  await openActivity();
  const pageEmpty = (await evaluate(`__s.actNames()`)).length === 0;
  await closeActivity();
  check(
    'clear history empties Recent, Frequent and Activity page',
    emptied === true && pageEmpty,
    `sidebar=${emptied} page=${pageEmpty}`,
  );
  const favKept = await favCount();
  check('clear history keeps Favorites', favKept === snapFav, `${snapFav} -> ${favKept}`);
  await evaluate(`document.querySelector('.favRow .unpin')?.click()`);
  await waitFor(
    `__s.qa('.favRow', __s.group('Favorites') ?? document.createElement('i')).length === 0`,
    8000,
    'unpin',
  ).catch(() => undefined);
  check('sidebar unpin removes the favorite', (await favCount()) === 0);

  check(
    'activity.sqlite3 in app-local data dir',
    existsSync(join(root, 'activity', 'activity.sqlite3')),
  );
} catch (error) {
  check('smoke run completed without error', false, String(error));
} finally {
  killApp();
  await sleep(1000);
  const proof = verifyRealProfile(realBefore);
  check(
    'real profile (config.json + Activity DB) unchanged',
    proof.unchanged,
    `${proof.files} files hashed: ${proof.detail}`,
  );
  for (const p of [fxRoot, root]) {
    try {
      rmSync(p, { recursive: true, force: true });
    } catch {
      /* best-effort */
    }
  }
}

const failed = results.filter((r) => !r.ok);
console.log(
  `\nSMOKE RESULT: ${results.length - failed.length}/${results.length} passed in ${Math.round((Date.now() - startedAt) / 1000)}s`,
);
process.exit(failed.length === 0 ? 0 : 1);
