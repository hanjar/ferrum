/**
 * Memory profile of the REAL release ferrum.exe when opening a big folder (FER-455, Windows-only).
 *
 * usage: node scripts/profile-mem.mjs <path-to-ferrum.exe> [--files 10000] [--out profile.json]
 *
 * Launches the app isolated like scripts/measure-app.mjs, opens a folder with N files and reports, idle vs loaded:
 *  - private/working set per process (ferrum.exe vs each WebView2 process, with its --type),
 *  - renderer JS heap (Runtime.getHeapUsage after a forced GC) and DOM counters (Memory.getDOMCounters),
 *  - a heap-snapshot summary: self size and count per constructor/type, top 25.
 * Isolation is FERRUM_PROFILE_DIR; the real profile is hashed before/after (exit 4 if it changed). Refuses to run while another smoke/measure script is active (CPU noise) unless --force.
 */
import { spawn, spawnSync, execSync } from 'node:child_process';
import { mkdtempSync, rmSync, mkdirSync, writeFileSync } from 'node:fs';
import { tmpdir, homedir } from 'node:os';
import { join, parse } from 'node:path';
import { profileEnv, snapshotRealProfile, verifyRealProfile } from './lib/test-profile.mjs';

const argv = process.argv.slice(2);
const opt = (n, d) => {
  const i = argv.indexOf(`--${n}`);
  return i >= 0 ? argv[i + 1] : d;
};
const EXE = argv.find((a, i) => !a.startsWith('--') && !argv[i - 1]?.startsWith('--'));
if (!EXE) {
  console.error('usage: node scripts/profile-mem.mjs <ferrum.exe> [--files N] [--out f.json]');
  process.exit(2);
}
const FILES = Number(opt('files', 10000));
const PORT = 9225;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const mb = (n) => Math.round((n / 1048576) * 10) / 10;

if (!argv.includes('--force')) {
  const other = spawnSync(
    'powershell.exe',
    [
      '-NoProfile',
      '-NonInteractive',
      '-Command',
      "Get-CimInstance Win32_Process | Where-Object { $_.Name -eq 'node.exe' -and $_.CommandLine -match '(smoke-.*|measure-app)[.]mjs' } | Measure-Object | ForEach-Object Count",
    ],
    { encoding: 'utf8' },
  );
  if (Number(other.stdout.trim()) > 0) {
    console.error('another Ferrum smoke/measure script is running; retry later or --force');
    process.exit(3);
  }
}
const root = mkdtempSync(join(tmpdir(), 'ferrum-prof-'));
// Isolation is FERRUM_PROFILE_DIR (backend); the real profile is only hashed before/after as proof.
const realBefore = snapshotRealProfile();
const fxRoot = join(homedir(), `ferrum-prof-fx-${Date.now().toString(36)}`);
const fxName = parse(fxRoot).base;
let child,
  ws,
  nextId = 1,
  cleaned = false;
const pending = new Map();
const listeners = [];

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
    /* exited */
  }
  child = undefined;
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
function cleanup() {
  if (cleaned) return;
  cleaned = true;
  killApp();
  const proof = verifyRealProfile(realBefore);
  console.log(
    `real profile ${proof.unchanged ? 'unchanged' : 'MODIFIED'} (${proof.files} files hashed): ${proof.detail}`,
  );
  if (!proof.unchanged) process.exitCode = 4;
  try {
    rmSync(fxRoot, { recursive: true, force: true });
  } catch {
    /* best effort */
  }
  try {
    rmSync(root, { recursive: true, force: true });
  } catch {
    /* best effort */
  }
}
process.on('SIGINT', () => {
  cleanup();
  process.exit(130);
});
process.on('exit', cleanup);

function send(method, params = {}, timeout = 60000) {
  const id = nextId++;
  ws.send(JSON.stringify({ id, method, params }));
  return new Promise((res, rej) => {
    pending.set(id, { res, rej });
    setTimeout(() => {
      if (pending.delete(id)) rej(new Error(`CDP timeout: ${method}`));
    }, timeout);
  });
}
async function connect() {
  let target;
  for (let i = 0; i < 600 && !target; i++) {
    try {
      target = (await (await fetch(`http://127.0.0.1:${PORT}/json/list`)).json()).find(
        (t) => t.type === 'page' && t.webSocketDebuggerUrl && t.url.includes('tauri.localhost'),
      );
    } catch {
      /* not up */
    }
    if (!target) await sleep(50);
  }
  if (!target) throw new Error('CDP endpoint never appeared');
  ws = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise((res, rej) => {
    ws.onopen = res;
    ws.onerror = rej;
  });
  ws.onmessage = (ev) => {
    const msg = JSON.parse(ev.data);
    if (msg.id && pending.has(msg.id)) {
      const { res, rej } = pending.get(msg.id);
      pending.delete(msg.id);
      if (msg.error) rej(new Error(msg.error.message));
      else res(msg.result);
    } else if (msg.method) for (const l of listeners) l(msg);
  };
  await send('Runtime.enable');
}
function helpers() {
  const qa = (s, r = document) => [...r.querySelectorAll(s)];
  const last = (p) =>
    String(p)
      .replace(/[\\/]+$/, '')
      .split(/[\\/]/)
      .pop();
  const M = {
    ready: () =>
      !!document.querySelector('.pane[data-pane="0"] [data-path]') &&
      !document.querySelector('[aria-busy="true"]'),
    rows: (i) => qa(`.pane[data-pane="${i}"] [data-path]`),
    row: (i, name) => M.rows(i).find((r) => last(r.getAttribute('data-path')) === name),
    crumbs: (i) => qa(`.pane[data-pane="${i}"] .crumb`),
    crumbLast: (i) => M.crumbs(i).at(-1)?.textContent?.trim() ?? '',
    busy: () => !!document.querySelector('[aria-busy="true"]'),
    click: (el) => {
      el.dispatchEvent(new MouseEvent('mousedown', { bubbles: true, cancelable: true }));
      el.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true }));
    },
    dblclick: (el) => {
      el.dispatchEvent(new MouseEvent('mousedown', { bubbles: true, cancelable: true }));
      el.dispatchEvent(new MouseEvent('dblclick', { bubbles: true, cancelable: true }));
    },
  };
  window.__m = M;
}
async function evaluate(expression) {
  const r = await send('Runtime.evaluate', {
    expression: `(window.__m || (${helpers.toString()})(), ${expression})`,
    returnByValue: true,
    awaitPromise: true,
  });
  if (r.exceptionDetails)
    throw new Error(
      `eval failed: ${r.exceptionDetails.exception?.description ?? r.exceptionDetails.text}`,
    );
  return r.result?.value;
}
async function waitFor(expr, timeoutMs, label) {
  const end = Date.now() + timeoutMs;
  while (Date.now() < end) {
    try {
      if (await evaluate(expr)) return;
    } catch {
      /* not ready */
    }
    await sleep(100);
  }
  throw new Error(`timeout waiting for ${label}`);
}
const psFile = join(root, 'sample.ps1');
writeFileSync(
  psFile,
  `
$rootId = $args[0]
$all = Get-CimInstance Win32_Process | Select-Object ProcessId, ParentProcessId, CommandLine
$ids = New-Object System.Collections.Generic.List[int]; $ids.Add([int]$rootId)
for ($i = 0; $i -lt $ids.Count; $i++) { foreach ($c in $all) { if ($c.ParentProcessId -eq $ids[$i] -and -not $ids.Contains([int]$c.ProcessId)) { $ids.Add([int]$c.ProcessId) } } }
$out = foreach ($p in (Get-Process -Id $ids.ToArray() -ErrorAction SilentlyContinue)) {
  $cl = ($all | Where-Object { $_.ProcessId -eq $p.Id }).CommandLine
  $type = if ($cl -match '--type=([a-z-]+)') { $Matches[1] } else { 'main' }
  [pscustomobject]@{ id = $p.Id; name = $p.ProcessName; type = $type; ws = $p.WorkingSet64; pm = $p.PrivateMemorySize64 } }
ConvertTo-Json -InputObject @($out) -Compress`,
);
function sample() {
  const r = spawnSync(
    'powershell.exe',
    [
      '-NoProfile',
      '-NonInteractive',
      '-ExecutionPolicy',
      'Bypass',
      '-File',
      psFile,
      String(child.pid),
    ],
    { encoding: 'utf8' },
  );
  const p = JSON.parse(r.stdout.trim() || '[]');
  return (Array.isArray(p) ? p : [p]).map((x) => ({
    name: x.name,
    type: x.type,
    pid: x.id,
    wsMB: mb(x.ws),
    pmMB: mb(x.pm),
  }));
}
async function jsState() {
  await send('HeapProfiler.enable');
  await send('HeapProfiler.collectGarbage');
  const heap = await send('Runtime.getHeapUsage');
  const dom = await send('Memory.getDOMCounters');
  return {
    heapUsedMB: mb(heap.usedSize),
    heapTotalMB: mb(heap.totalSize),
    nodes: dom.nodes,
    jsEventListeners: dom.jsEventListeners,
    documents: dom.documents,
  };
}
async function heapSummary() {
  const chunks = [];
  listeners.push((m) => {
    if (m.method === 'HeapProfiler.addHeapSnapshotChunk') chunks.push(m.params.chunk);
  });
  await send(
    'HeapProfiler.takeHeapSnapshot',
    { reportProgress: false, captureNumericValue: false },
    180000,
  );
  const snap = JSON.parse(chunks.join(''));
  const { node_fields: nf, node_types: nt } = snap.snapshot.meta;
  const stride = nf.length,
    iT = nf.indexOf('type'),
    iN = nf.indexOf('name'),
    iS = nf.indexOf('self_size');
  const types = nt[0],
    agg = new Map();
  for (let i = 0; i < snap.nodes.length; i += stride) {
    const t = types[snap.nodes[i + iT]];
    const name =
      t === 'object' || t === 'native' || t === 'closure' || t === 'array'
        ? snap.strings[snap.nodes[i + iN]]
        : `(${t})`;
    const key = `${t}:${name.slice(0, 60)}`;
    const a = agg.get(key) ?? { key, count: 0, selfMB: 0 };
    a.count++;
    a.selfMB += snap.nodes[i + iS] / 1048576;
    agg.set(key, a);
  }
  listeners.length = 0;
  return [...agg.values()]
    .sort((a, b) => b.selfMB - a.selfMB)
    .slice(0, 25)
    .map((a) => ({ ...a, selfMB: Math.round(a.selfMB * 100) / 100 }));
}

const result = { files: FILES };
try {
  mkdirSync(fxRoot, { recursive: true });
  for (let i = 0; i < FILES; i++)
    writeFileSync(join(fxRoot, `file-${String(i).padStart(5, '0')}.txt`), 'x');
  child = spawn(EXE, [], {
    stdio: 'ignore',
    env: {
      ...process.env,
      ...profileEnv(root),
      WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${PORT} --no-first-run`,
    },
  });
  await connect();
  await waitFor('__m.ready()', 30000, 'UI ready');
  await sleep(8000);
  result.idle = { procs: sample(), js: await jsState() };
  result.idle.heapTop = await heapSummary();

  const home = parse(homedir()).base;
  let pane = -1;
  for (const i of [0, 1])
    if (
      await evaluate(
        `__m.crumbs(${i}).some((x) => x.textContent.trim() === ${JSON.stringify(home)})`,
      )
    ) {
      pane = i;
      break;
    }
  if (pane < 0) throw new Error('home crumb not found');
  await evaluate(
    `(() => { const c = __m.crumbs(${pane}); __m.click(c.find((x) => x.textContent.trim() === ${JSON.stringify(home)})); return true; })()`,
  );
  await waitFor(
    `__m.crumbLast(${pane}) === ${JSON.stringify(home)} && !__m.busy() && __m.rows(${pane}).length > 0`,
    20000,
    'home listing',
  );
  const findRow = `(() => { const r = __m.row(${pane}, ${JSON.stringify(fxName)}); if (r) { r.scrollIntoView(); return true; } return false; })()`;
  let found = await evaluate(findRow);
  for (let k = 0; !found && k < 60; k++) {
    await evaluate(
      `(() => { const s = document.querySelector('.pane[data-pane="${pane}"] [data-path]')?.closest('[class*="scroll"], [class*="list"], [style*="overflow"]'); if (s) s.scrollTop += s.clientHeight; return true; })()`,
    );
    await sleep(120);
    found = await evaluate(findRow);
  }
  if (!found) throw new Error('fixture row not reachable');
  await evaluate(
    `(() => { __m.dblclick(__m.row(${pane}, ${JSON.stringify(fxName)})); return true; })()`,
  );
  await waitFor(
    `__m.crumbLast(${pane}) === ${JSON.stringify(fxName)} && !__m.busy() && __m.rows(${pane}).length > 0`,
    60000,
    '10k listing',
  );
  await sleep(5000);
  result.loaded = { procs: sample(), js: await jsState() };
  result.loaded.heapTop = await heapSummary();
} finally {
  cleanup();
}
const out = opt('out');
if (out) writeFileSync(out, JSON.stringify(result, null, 2));
const fmt = (s) =>
  s.procs.map((p) => `${p.name}/${p.type}#${p.pid} ws ${p.wsMB} pm ${p.pmMB}`).join('\n  ');
for (const k of ['idle', 'loaded']) {
  console.log(`\n== ${k} ==\n  ${fmt(result[k])}\n  js: ${JSON.stringify(result[k].js)}`);
  console.log(
    result[k].heapTop
      .slice(0, 15)
      .map((a) => `  ${a.selfMB} MB  x${a.count}  ${a.key}`)
      .join('\n'),
  );
}
