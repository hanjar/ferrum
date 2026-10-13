/**
 * FER-477 smoke over a REAL WebView2 window (Windows-only), WITHOUT a FerrumIndexer that speaks
 * IPC v4 (none installed, or an older service still on v3): the new features must fall back.
 * - `indexer_folder_sizes` answers "not available" (sizes null) and the file list keeps "—" in the
 *   Size column of folder rows, with the setting switched on;
 * - Settings > Indexer shows the folder-size toggle (checked) and the status does not break;
 * - a recursive compare by content with `incremental: true` runs the ordinary compare, reports
 *   `incremental.used = false` with a reason, and its verdicts are unchanged.
 * Launches ferrum.exe with an isolated FERRUM_PROFILE_DIR + CDP and proves the real profile is
 * untouched. Only the spawned test process is ended; nothing is installed or changed on the service.
 * usage: node scripts/smoke-folder-sizes-fallback.mjs <ferrum.exe>
 */
import { spawn, execSync } from 'node:child_process';
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { profileEnv, snapshotRealProfile, verifyRealProfile } from './lib/test-profile.mjs';

const EXE = process.argv[2];
const PORT = 9347;
if (!EXE) {
  console.error('usage: node scripts/smoke-folder-sizes-fallback.mjs <ferrum.exe>');
  process.exit(2);
}
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const root = mkdtempSync(join(tmpdir(), 'ferrum-fs-smoke-'));
const realBefore = snapshotRealProfile();
const fix = join(root, 'fixture');
mkdirSync(join(fix, 'Projects', 'deep'), { recursive: true });
mkdirSync(join(fix, 'Empty'), { recursive: true });
writeFileSync(join(fix, 'Projects', 'a.bin'), 'a'.repeat(3000));
writeFileSync(join(fix, 'Projects', 'deep', 'b.bin'), 'b'.repeat(5000));
writeFileSync(join(fix, 'readme.txt'), 'r'.repeat(100));
const L = join(root, 'L');
const R = join(root, 'R');
for (const d of [L, R]) {
  mkdirSync(join(d, 'sub'), { recursive: true });
  writeFileSync(join(d, 'sub', 'same.txt'), 'identical');
}
writeFileSync(join(L, 'x.bin'), 'AAAA');
writeFileSync(join(R, 'x.bin'), 'BBBB');
mkdirSync(join(root, 'config'), { recursive: true });
writeFileSync(
  join(root, 'config', 'config.json'),
  JSON.stringify({
    indexerFolderSizes: true,
    showExtensions: true,
    session: { panes: [{ tabs: [fix], current: 0 }] },
  }),
);

let child,
  ws,
  nextId = 1;
const pending = new Map();
const send = (method, params = {}) => {
  const id = nextId++;
  ws.send(JSON.stringify({ id, method, params }));
  return new Promise((resolve, reject) => {
    pending.set(id, { resolve, reject });
    setTimeout(
      () => pending.has(id) && (pending.delete(id), reject(new Error(`timeout ${method}`))),
      30000,
    );
  });
};
const ev = async (expr) => {
  const r = await send('Runtime.evaluate', {
    expression: expr,
    returnByValue: true,
    awaitPromise: true,
  });
  if (r.exceptionDetails)
    throw new Error(r.exceptionDetails.exception?.description ?? r.exceptionDetails.text);
  return r.result?.value;
};
const check = (name, ok, detail = '') => {
  console.log(`${ok ? 'PASS' : 'FAIL'} ${name}${detail ? ` — ${detail}` : ''}`);
  if (!ok) process.exitCode = 1;
};
const key = async (k, code, vk, modifiers = 0) => {
  for (const type of ['keyDown', 'keyUp'])
    await send('Input.dispatchKeyEvent', {
      type,
      key: k,
      code,
      modifiers,
      windowsVirtualKeyCode: vk,
    });
};

async function main() {
  child = spawn(EXE, [], {
    stdio: 'ignore',
    env: {
      ...process.env,
      ...profileEnv(root),
      WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${PORT} --no-first-run`,
    },
  });
  let target;
  for (let i = 0; i < 100 && !target; i++) {
    try {
      target = (await (await fetch(`http://127.0.0.1:${PORT}/json/list`)).json()).find(
        (t) => t.type === 'page' && t.webSocketDebuggerUrl,
      );
    } catch {
      /* not up */
    }
    if (!target) await sleep(300);
  }
  if (!target) throw new Error('no CDP');
  ws = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise((r, j) => ((ws.onopen = r), (ws.onerror = j)));
  ws.onmessage = (e) => {
    const m = JSON.parse(e.data);
    const p = m.id && pending.get(m.id);
    if (p) {
      pending.delete(m.id);
      if (m.error) p.reject(new Error(m.error.message));
      else p.resolve(m.result);
    }
  };
  await send('Runtime.enable');
  for (let i = 0; i < 100; i++) {
    if (await ev(`document.querySelectorAll('.row[data-path]').length`)) break;
    await sleep(200);
  }

  const st = await ev(`window.__TAURI_INTERNALS__.invoke('indexer_status')`);
  console.log(
    `indexer_status: installed=${st.installed} reachable=${st.reachable} problem=${st.problem ?? '-'} version=${st.version ?? '-'}`,
  );

  // 1. The command itself: not available, one null per name, never an error.
  const fs1 = await ev(
    `window.__TAURI_INTERNALS__.invoke('indexer_folder_sizes', { parent: ${JSON.stringify(fix)}, names: ['Projects', 'Empty', '..'] })`,
  );
  check(
    'indexer_folder_sizes: not available, sizes null',
    fs1.available === false &&
      Array.isArray(fs1.sizes) &&
      fs1.sizes.length === 3 &&
      fs1.sizes.every((s) => s === null),
    JSON.stringify(fs1),
  );

  // 2. The file list keeps "—" for folders (setting on), files keep their size.
  await sleep(1500);
  const cells = await ev(`(() => {
    const out = {};
    for (const r of document.querySelectorAll('[data-pane="0"] .row[data-path]')) {
      const n = r.querySelector('.nameText')?.textContent ?? '';
      out[n] = r.querySelector('.size')?.textContent ?? null;
    }
    return out;
  })()`);
  check('folder row Projects shows —', cells.Projects === '—', JSON.stringify(cells));
  check('folder row Empty shows —', cells.Empty === '—', JSON.stringify(cells));
  check('file row keeps its size', cells['readme.txt'] === '100 B', JSON.stringify(cells));

  // 3. Settings > Indexer: the toggle is there and checked; status still renders.
  await key(',', 'Comma', 188, 2);
  let toggle = null;
  for (let i = 0; i < 50 && toggle === null; i++) {
    toggle = await ev(
      `(() => { const b = [...document.querySelectorAll('[role=dialog] button')].find((x) => x.textContent.trim() === 'Indexer'); b?.click(); const c = document.querySelector('[data-testid=indexer-folder-sizes] input'); return c ? c.checked : null; })()`,
    );
    if (toggle === null) await sleep(200);
  }
  check('Settings: folder-size toggle shown and checked', toggle === true, String(toggle));
  const svc = await ev(`document.querySelector('[data-testid=indexer-status]')?.textContent ?? ''`);
  check('Settings: status renders', svc.length > 0, svc.trim().slice(0, 120));
  const shot = await send('Page.captureScreenshot', { format: 'png' });
  const shotPath = join(tmpdir(), 'ferrum-folder-sizes-settings-smoke.png');
  writeFileSync(shotPath, Buffer.from(shot.data, 'base64'));
  console.log(`screenshot: ${shotPath}`);
  await key('Escape', 'Escape', 27);

  // 4. Incremental compare without a v4 service = the ordinary compare.
  const rep = await ev(
    `window.__TAURI_INTERNALS__.invoke('compare_dirs', { left: ${JSON.stringify(L)}, right: ${JSON.stringify(R)}, byContent: true, recursive: true, opId: null, noCache: true, incremental: true })`,
  );
  const state = (n) => rep.entries.find((e) => e.name === n)?.state;
  check(
    'incremental compare not used, with a reason',
    rep.incremental?.used === false && typeof rep.incremental?.reason === 'string',
    JSON.stringify(rep.incremental),
  );
  check('no journal marks without the service', rep.journal === undefined);
  check('content difference still found', state('x.bin') === 'contentDifferent', state('x.bin'));
  check('equal file still same', state('sub/same.txt') === 'same', state('sub/same.txt'));
}

try {
  await main();
} catch (e) {
  console.error('SMOKE FAIL', e.message);
  process.exitCode = 1;
} finally {
  try {
    ws?.close();
  } catch {
    /* gone */
  }
  try {
    if (child?.pid) execSync(`taskkill /PID ${child.pid} /T /F`, { stdio: 'ignore' });
  } catch {
    /* exited */
  }
  await sleep(500);
  try {
    rmSync(root, { recursive: true, force: true });
  } catch {
    /* ignore */
  }
  const proof = verifyRealProfile(realBefore);
  console.log(`real profile unchanged: ${proof.unchanged} (${proof.files} files)`);
  if (!proof.unchanged) {
    console.error(proof.detail);
    process.exitCode = 1;
  }
}
