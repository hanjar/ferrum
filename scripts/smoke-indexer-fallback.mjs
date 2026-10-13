/**
 * FER-476 smoke over a REAL WebView2 window (Windows-only), WITHOUT the FerrumIndexer service installed:
 * - Settings > Indexer reports "Tidak terpasang" honestly (service + index lines);
 * - search_start falls back to the walker (stats.source = 'walker') and finds the fixture files;
 * - disk_usage_index_eligibility says not eligible (service not running);
 * - disk_usage_start with source 'index' falls back to the regular walk, with a notice, and completes.
 * Launches ferrum.exe with an isolated FERRUM_PROFILE_DIR + CDP and proves the real profile is untouched.
 * usage: node scripts/smoke-indexer-fallback.mjs <ferrum.exe>
 */
import { spawn, execSync } from 'node:child_process';
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { profileEnv, snapshotRealProfile, verifyRealProfile } from './lib/test-profile.mjs';

const EXE = process.argv[2];
const PORT = 9229;
if (!EXE) {
  console.error('usage: node scripts/smoke-indexer-fallback.mjs <ferrum.exe>');
  process.exit(2);
}
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const root = mkdtempSync(join(tmpdir(), 'ferrum-idx-smoke-'));
const realBefore = snapshotRealProfile();
const fix = join(root, 'fixture');
mkdirSync(join(fix, 'sub'), { recursive: true });
writeFileSync(join(fix, 'alpha.txt'), 'a'.repeat(1000));
writeFileSync(join(fix, 'sub', 'beta.txt'), 'b'.repeat(2000));
writeFileSync(join(fix, 'gamma.md'), 'c');
mkdirSync(join(root, 'config'), { recursive: true });
writeFileSync(
  join(root, 'config', 'config.json'),
  JSON.stringify({ session: { panes: [{ tabs: [fix], current: 0 }] } }),
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

/** Listen to a Tauri event from the page, collecting payloads in window[key]. */
const listen = (event, key) =>
  ev(`(async () => {
    window.${key} = [];
    const T = window.__TAURI_INTERNALS__;
    const cb = T.transformCallback((e) => window.${key}.push(e.payload));
    await T.invoke('plugin:event|listen', { event: '${event}', target: { kind: 'Any' }, handler: cb });
    return true;
  })()`);

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

  // 1. Status over IPC and in Settings.
  const st = await ev(`window.__TAURI_INTERNALS__.invoke('indexer_status')`);
  check(
    'indexer_status: not installed',
    st.installed === false && st.reachable === false,
    JSON.stringify(st),
  );
  await send('Input.dispatchKeyEvent', {
    type: 'keyDown',
    key: ',',
    modifiers: 2,
    code: 'Comma',
    windowsVirtualKeyCode: 188,
  });
  await send('Input.dispatchKeyEvent', {
    type: 'keyUp',
    key: ',',
    modifiers: 2,
    code: 'Comma',
    windowsVirtualKeyCode: 188,
  });
  let idx = '';
  for (let i = 0; i < 50 && !idx; i++) {
    idx = await ev(`document.querySelector('[data-testid=indexer-index]')?.textContent ?? ''`);
    if (!idx) await sleep(200);
  }
  const svc = await ev(`document.querySelector('[data-testid=indexer-status]')?.textContent ?? ''`);
  check('Settings: service "Tidak terpasang"', /Tidak terpasang/.test(svc), svc.trim());
  check('Settings: index line honest', /Tidak terpasang.*scan biasa/.test(idx), idx.trim());
  const shot = await send('Page.captureScreenshot', { format: 'png' });
  const shotPath = join(tmpdir(), 'ferrum-indexer-settings-smoke.png');
  writeFileSync(shotPath, Buffer.from(shot.data, 'base64'));
  console.log(`screenshot: ${shotPath}`);
  await ev(
    `document.querySelector('[role=dialog][aria-label="Settings"] [title="Close"]')?.click()`,
  );

  // 2. Search falls back to the walker.
  await listen('search://progress', '__searchEv');
  await ev(
    `window.__TAURI_INTERNALS__.invoke('search_start', { id: 'smoke-1', root: ${JSON.stringify(fix)}, options: { pattern: '*.txt', mode: 'glob', caseSensitive: false, recursive: true } })`,
  );
  let done;
  for (let i = 0; i < 100 && !done; i++) {
    done = await ev(`window.__searchEv.find((e) => e.done) ?? null`);
    if (!done) await sleep(100);
  }
  const found = await ev(
    `window.__searchEv.flatMap((e) => e.entries.map((x) => x.name)).sort().join(',')`,
  );
  check('search done', !!done && !done.error, JSON.stringify(done?.stats ?? done?.error));
  check('search source = walker', done?.stats?.source === 'walker', done?.stats?.source);
  check('search results', found === 'alpha.txt,beta.txt', found);

  // 3. Disk usage "Instan (indeks)" not offered; forced index falls back with a notice.
  const elig = await ev(
    `window.__TAURI_INTERNALS__.invoke('disk_usage_index_eligibility', { root: ${JSON.stringify(fix)} })`,
  );
  check('index eligibility: no', elig.eligible === false, JSON.stringify(elig));
  await listen('disk_usage://progress', '__duEv');
  const scanId = await ev(
    `window.__TAURI_INTERNALS__.invoke('disk_usage_start', { root: ${JSON.stringify(fix)}, options: { maxDepth: null }, fast: false, source: 'index' })`,
  );
  let duDone;
  for (let i = 0; i < 100 && !duDone; i++) {
    duDone = await ev(
      `window.__duEv.find((e) => e.scanId === ${JSON.stringify(scanId)} && e.done) ?? null`,
    );
    if (!duDone) await sleep(100);
  }
  check('disk usage done', !!duDone && !duDone.error, duDone?.error ?? '');
  check('disk usage engine = walk', (duDone?.engine ?? 'walk') === 'walk', duDone?.engine);
  check(
    'disk usage notice explains the fallback',
    /Indeks tidak dipakai/.test(duDone?.notice ?? ''),
    duDone?.notice,
  );
  check('disk usage counted the files', duDone?.scannedFiles === 3, String(duDone?.scannedFiles));
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
