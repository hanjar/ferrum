/**
 * FER-479: main-thread responsiveness of the REAL release ferrum.exe while a Disk usage scan of a big tree runs
 * and finishes (Windows-only, WebView2 over CDP). Regular walker ("Scan biasa") only — never elevates.
 *
 * usage: node scripts/measure-du-freeze.mjs <path-to-ferrum.exe> --root <dir> [--timeout-s 900] [--json out.json]
 *
 * Measures, from Scan click until the result tree has rendered and settled (+5 s):
 *  - long tasks on the WebView main thread (PerformanceObserver 'longtask'): count, max, total;
 *  - the largest gap of a 50 ms setInterval heartbeat (= how long the page could not run anything);
 *  - encoded size + duration of every disk_usage_* IPC response (CDP Network over the ipc.localhost protocol);
 *  - JS heap (performance.memory) and working set / private bytes of the ferrum.exe process tree.
 * Isolation: FERRUM_PROFILE_DIR temp profile; the real profile is hashed before/after (exit 4 if it changed).
 */
import { spawn, spawnSync, execSync } from 'node:child_process';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { profileEnv, snapshotRealProfile, verifyRealProfile } from './lib/test-profile.mjs';

const argv = process.argv.slice(2);
const opt = (name, def) => {
  const i = argv.indexOf(`--${name}`);
  return i >= 0 ? argv[i + 1] : def;
};
const EXE = argv[0];
const ROOT = opt('root');
if (!EXE || !ROOT) {
  console.error('usage: node scripts/measure-du-freeze.mjs <ferrum.exe> --root <dir> [--json f]');
  process.exit(2);
}
const TIMEOUT_MS = Number(opt('timeout-s', 900)) * 1000;
const PORT = Number(opt('port', 9227));
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const profile = mkdtempSync(join(tmpdir(), 'ferrum-du-freeze-'));
const realBefore = snapshotRealProfile();
let child;
let ws;
let nextId = 1;
const pending = new Map();
const ipc = new Map(); // requestId -> { url, t0 }
const ipcDone = [];
let webviewGoneAt = 0;
let scanStartedAt = 0;

function killApp() {
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
  spawnSync(
    'powershell.exe',
    [
      '-NoProfile',
      '-NonInteractive',
      '-Command',
      `Get-CimInstance Win32_Process | Where-Object { $_.CommandLine -like '*remote-debugging-port=${PORT} *' } | ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }`,
    ],
    { stdio: 'ignore' },
  );
}
let cleaned = false;
function cleanup() {
  if (cleaned) return;
  cleaned = true;
  killApp();
  const proof = verifyRealProfile(realBefore);
  console.log(`real profile ${proof.unchanged ? 'unchanged' : 'MODIFIED'}: ${proof.detail}`);
  if (!proof.unchanged) process.exitCode = 4;
  try {
    rmSync(profile, { recursive: true, force: true });
  } catch {
    /* best effort */
  }
}
process.on('exit', cleanup);
process.on('SIGINT', () => {
  cleanup();
  process.exit(130);
});

function send(method, params = {}, timeoutMs = 10000) {
  const id = nextId++;
  ws.send(JSON.stringify({ id, method, params }));
  return new Promise((res, rej) => {
    pending.set(id, { res, rej });
    setTimeout(() => {
      if (pending.delete(id)) rej(new Error(`CDP timeout: ${method}`));
    }, timeoutMs);
  });
}
async function evaluate(expr, timeoutMs = 10000) {
  const r = await send(
    'Runtime.evaluate',
    { expression: expr, awaitPromise: true, returnByValue: true },
    timeoutMs,
  );
  if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description ?? 'eval');
  return r.result.value;
}

function procMem(pid) {
  const ps = `$all = Get-CimInstance Win32_Process | Select-Object ProcessId,ParentProcessId,Name,WorkingSetSize,PrivatePageCount;
$ids = @(${pid}); $changed = $true
while ($changed) { $changed = $false; foreach ($p in $all) { if ($ids -contains $p.ParentProcessId -and -not ($ids -contains $p.ProcessId)) { $ids += $p.ProcessId; $changed = $true } } }
$all | Where-Object { $ids -contains $_.ProcessId } | ForEach-Object { "$($_.ProcessId)|$($_.WorkingSetSize)|$($_.PrivatePageCount)" }`;
  const out = spawnSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command', ps], {
    encoding: 'utf8',
  }).stdout;
  const rows = out
    .trim()
    .split(/\r?\n/)
    .filter(Boolean)
    .map((l) => {
      const [id, wsb, pb] = l.split('|');
      return {
        main: Number(id) === pid,
        wsMB: Number(wsb) / 1048576,
        privMB: Number(pb) / 1048576,
      };
    });
  const r1 = (n) => Math.round(n * 10) / 10;
  const by = (pred) => rows.filter(pred).reduce((a, p) => a + p.privMB, 0);
  return {
    ferrumPrivateMB: r1(by((p) => p.main)),
    webviewPrivateMB: r1(by((p) => !p.main)),
    maxWebviewWorkingSetMB: r1(Math.max(0, ...rows.filter((p) => !p.main).map((p) => p.wsMB))),
  };
}

async function main() {
  child = spawn(EXE, [], {
    stdio: 'ignore',
    env: {
      ...process.env,
      ...profileEnv(profile),
      WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${PORT} --no-first-run`,
    },
  });
  let target;
  const until = Date.now() + 30000;
  while (!target && Date.now() < until) {
    try {
      const r = await fetch(`http://127.0.0.1:${PORT}/json/list`);
      target = (await r.json()).find(
        (t) => t.type === 'page' && t.webSocketDebuggerUrl && t.url.includes('tauri.localhost'),
      );
    } catch {
      /* not up */
    }
    if (!target) await sleep(100);
  }
  if (!target) throw new Error('CDP endpoint never appeared');
  ws = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise((res, rej) => {
    ws.onopen = res;
    ws.onerror = rej;
  });
  // A WebView2 crash (e.g. renderer/browser OOM) drops the CDP socket:
  // record it and fail every pending call instead of hanging until timeout.
  ws.onclose = () => {
    webviewGoneAt = Date.now();
    for (const [id, p] of pending) {
      pending.delete(id);
      p.rej(new Error('WebView2 connection closed (crash?)'));
    }
  };
  ws.onmessage = (ev) => {
    const msg = JSON.parse(ev.data);
    if (msg.id && pending.has(msg.id)) {
      const { res, rej } = pending.get(msg.id);
      pending.delete(msg.id);
      if (msg.error) rej(new Error(msg.error.message));
      else res(msg.result);
      return;
    }
    if (msg.method === 'Network.requestWillBeSent') {
      const url = msg.params.request.url;
      if (url.includes('disk_usage_'))
        ipc.set(msg.params.requestId, { url, t0: msg.params.timestamp });
    } else if (msg.method === 'Network.loadingFinished') {
      const r = ipc.get(msg.params.requestId);
      if (r) {
        ipcDone.push({
          cmd: r.url.split('/').pop(),
          bytes: msg.params.encodedDataLength,
          ms: Math.round((msg.params.timestamp - r.t0) * 1000),
        });
        ipc.delete(msg.params.requestId);
      }
    }
  };
  await send('Runtime.enable');
  await send('Network.enable', { maxTotalBufferSize: 1024 * 1024, maxResourceBufferSize: 1024 });
  // UI ready = a pane rendered.
  const readyBy = Date.now() + 30000;
  while (Date.now() < readyBy) {
    if (await evaluate(`!!document.querySelector('button[title="Disk usage (Alt+Shift+D)"]')`))
      break;
    await sleep(200);
  }
  const memIdle = procMem(child.pid);

  await evaluate(`(() => {
    window.__fr = { long: [], maxGap: 0, last: performance.now(), t0: performance.now() };
    new PerformanceObserver((l) => { for (const e of l.getEntries()) window.__fr.long.push(Math.round(e.duration)); })
      .observe({ type: 'longtask', buffered: false });
    setInterval(() => { const n = performance.now(); window.__fr.maxGap = Math.max(window.__fr.maxGap, n - window.__fr.last); window.__fr.last = n; }, 50);
    document.querySelector('button[title="Disk usage (Alt+Shift+D)"]').click();
    return true;
  })()`);
  await sleep(500);
  await evaluate(`(() => {
    const i = document.querySelector('[aria-label="Path root scan disk usage"]');
    i.value = ${JSON.stringify(ROOT)};
    i.dispatchEvent(new Event('input', { bubbles: true }));
    return true;
  })()`);
  await sleep(200);
  const t0 = Date.now();
  scanStartedAt = t0;
  await evaluate(`(() => {
    const b = [...document.querySelectorAll('[data-testid="du-workspace"] button')].find((x) => /^(Scan|Rescan)$/.test(x.textContent.trim()));
    b.click();
    return true;
  })()`);
  // Fast-scan offer → always the regular walk (no elevation in this harness).
  for (let i = 0; i < 20; i++) {
    await sleep(150);
    const clicked = await evaluate(`(() => {
      const b = [...document.querySelectorAll('button')].find((x) => x.textContent.trim() === 'Scan biasa');
      if (b) { b.click(); return true; }
      return false;
    })()`);
    if (clicked) break;
  }
  let chip = '';
  let doneAt = 0;
  const evalStalls = [];
  while (Date.now() - t0 < TIMEOUT_MS) {
    const q0 = Date.now();
    try {
      chip = await evaluate(
        `document.querySelector('[data-testid="du-workspace"] .chip')?.textContent?.trim() ?? ''`,
        TIMEOUT_MS,
      );
    } catch (e) {
      chip = `eval failed: ${e.message}`;
    }
    const q = Date.now() - q0;
    if (q > 500) evalStalls.push(q);
    if (/^(Complete|Partial|Cancelled|Error)/i.test(chip)) {
      doneAt = Date.now();
      break;
    }
    await sleep(250);
  }
  // Settle: tree rows rendered.
  let rowsAt = 0;
  while (doneAt && Date.now() - doneAt < TIMEOUT_MS) {
    const q0 = Date.now();
    const n = await evaluate(
      `document.querySelectorAll('[data-testid="du-tree"] [role="treeitem"]').length`,
      TIMEOUT_MS,
    );
    if (Date.now() - q0 > 500) evalStalls.push(Date.now() - q0);
    if (n > 1) {
      rowsAt = Date.now();
      break;
    }
    await sleep(100);
  }
  await sleep(5000);
  const page = await evaluate(
    `(() => ({
    long: window.__fr.long,
    maxGap: Math.round(window.__fr.maxGap),
    heapMB: performance.memory ? Math.round(performance.memory.usedJSHeapSize / 1048576) : null,
    rows: document.querySelectorAll('[data-testid="du-tree"] [role="treeitem"]').length,
    counts: document.querySelector('[data-testid="du-workspace"] .counts')?.textContent?.trim() ?? '',
  }))()`,
    TIMEOUT_MS,
  );
  const memAfter = procMem(child.pid);
  const long = page.long;
  const result = {
    exe: EXE,
    root: ROOT,
    chip,
    scanMs: doneAt ? doneAt - t0 : null,
    doneToTreeMs: rowsAt ? rowsAt - doneAt : null,
    longTasks: {
      count: long.length,
      maxMs: long.length ? Math.max(...long) : 0,
      totalMs: long.reduce((a, b) => a + b, 0),
      over200: long.filter((d) => d > 200).length,
    },
    heartbeatMaxGapMs: page.maxGap,
    cdpEvalStallsMs: evalStalls,
    ipc: ipcDone.filter((r) => !r.cmd.startsWith('disk_usage_fast_eligibility')),
    jsHeapMB: page.heapMB,
    treeRows: page.rows,
    counts: page.counts,
    memIdle,
    memAfter,
  };
  console.log(JSON.stringify(result, null, 2));
  const out = opt('json');
  if (out) writeFileSync(out, JSON.stringify(result, null, 2));
}

main()
  .catch((e) => {
    console.error(e);
    if (webviewGoneAt) {
      const result = {
        exe: EXE,
        root: ROOT,
        webviewCrashedAfterScanStartMs: webviewGoneAt - scanStartedAt,
        memAfter: child?.pid ? procMem(child.pid) : null,
      };
      console.log(JSON.stringify(result, null, 2));
      const out = opt('json');
      if (out) writeFileSync(out, JSON.stringify(result, null, 2));
    }
    process.exitCode = 1;
  })
  .finally(() => {
    cleanup();
    process.exit();
  });
