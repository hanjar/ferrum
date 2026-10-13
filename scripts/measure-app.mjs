/**
 * Startup / memory / idle-CPU measurement of the REAL release ferrum.exe over WebView2 (Windows-only, FER-446).
 *
 * usage: node scripts/measure-app.mjs <path-to-ferrum.exe> [--runs 10] [--files 10000] [--idle-ms 10000]
 *                                      [--cpu-ms 30000] [--json out.json] [--md out.md]
 *
 * Metrics
 *  - startup: wall time from spawn() to "UI ready" = the first pane has rendered at least one file row and is no
 *    longer aria-busy, detected over CDP (polled every ~40 ms, so resolution is about 50 ms). N cold launches
 *    (each launch kills the previous process tree); reports median and p95 (nearest rank) plus every run.
 *  - memory: working set and private bytes, summed over the ferrum.exe process tree (ferrum.exe plus its WebView2
 *    children), sampled (a) after the idle period and (b) after opening a folder with 10k files.
 *    NOTE: summed working sets double-count pages shared between WebView2 processes; private bytes do not.
 *  - cpu idle: average CPU of that process tree over --cpu-ms (default 30 s) right after the idle period, as a
 *    percentage of ONE logical core and of the whole machine.
 *
 * Sampling uses Windows PowerShell (Get-CimInstance + Get-Process); nothing is downloaded or installed.
 * Isolation: FERRUM_PROFILE_DIR points every store (config.json incl. tab session, Activity DB, WebView2 data) at a temp
 * profile, so every launch starts in the home folder with nothing to back up or restore. The REAL profile is hashed
 * before and after as proof it was untouched (exit code 4 if not). Refuses to run while another smoke-*.mjs is active
 * (CPU noise would skew the numbers) unless --force.
 * The 10k-file fixture is created under the user profile (the pane reaches hidden %TEMP% only via showHidden)
 * and removed at the end. No CI threshold: this only records a baseline.
 */
import { spawn, spawnSync, execSync } from 'node:child_process';
import { mkdtempSync, rmSync, mkdirSync, writeFileSync, statSync } from 'node:fs';
import { tmpdir, homedir, cpus, totalmem, release } from 'node:os';
import { join, parse, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { profileEnv, snapshotRealProfile, verifyRealProfile } from './lib/test-profile.mjs';

// ---- pure helpers (exported for tests) --------------------------------------
export function median(values) {
  if (!values.length) return NaN;
  const s = [...values].sort((a, b) => a - b);
  const mid = s.length >> 1;
  return s.length % 2 ? s[mid] : (s[mid - 1] + s[mid]) / 2;
}
/** Nearest-rank percentile (p in 0..100). */
export function percentile(values, p) {
  if (!values.length) return NaN;
  const s = [...values].sort((a, b) => a - b);
  const rank = Math.max(1, Math.ceil((p / 100) * s.length));
  return s[Math.min(rank, s.length) - 1];
}
/** Per-pid CPU delta (seconds) for pids present in both samples, summed. */
export function cpuDeltaSeconds(before, after) {
  const prev = new Map(before.map((p) => [p.id, p.cpu]));
  let total = 0;
  for (const p of after) if (prev.has(p.id)) total += Math.max(0, p.cpu - prev.get(p.id));
  return total;
}
export function sumMem(procs) {
  return {
    processes: procs.length,
    workingSetMB: round(procs.reduce((a, p) => a + p.ws, 0) / 1048576),
    privateMB: round(procs.reduce((a, p) => a + p.pm, 0) / 1048576),
  };
}
const round = (n, d = 1) => Math.round(n * 10 ** d) / 10 ** d;

const isMain =
  process.argv[1] && resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url));
if (isMain) await main();

async function main() {
  const argv = process.argv.slice(2);
  const opt = (name, def) => {
    const i = argv.indexOf(`--${name}`);
    return i >= 0 ? argv[i + 1] : def;
  };
  const EXE = argv.find((a, i) => !a.startsWith('--') && !argv[i - 1]?.startsWith('--'));
  if (!EXE) {
    console.error(
      'usage: node scripts/measure-app.mjs <path-to-ferrum.exe> [--runs 10] [--files 10000] [--json f] [--md f]',
    );
    process.exit(2);
  }
  const RUNS = Number(opt('runs', 10));
  const FILES = Number(opt('files', 10000));
  const IDLE_MS = Number(opt('idle-ms', 10000));
  const CPU_MS = Number(opt('cpu-ms', 30000));
  const PORT = 9224;
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

  // ---- isolation + cleanup registry ----
  // Concurrent smoke runs would skew timings and memory; profiles are isolated, so this is not a correctness guard.
  if (!argv.includes('--force')) {
    const other = spawnSync(
      'powershell.exe',
      [
        '-NoProfile',
        '-NonInteractive',
        '-Command',
        "Get-CimInstance Win32_Process | Where-Object { $_.Name -eq 'node.exe' -and $_.CommandLine -match 'smoke-.*[.]mjs' } | Measure-Object | ForEach-Object Count",
      ],
      { encoding: 'utf8' },
    );
    if (Number(other.stdout.trim()) > 0) {
      console.error(
        'another Ferrum smoke script is running (it would skew the measurement); retry later or pass --force',
      );
      process.exit(3);
    }
  }
  const root = mkdtempSync(join(tmpdir(), 'ferrum-measure-'));
  // Isolation is FERRUM_PROFILE_DIR (backend); the real profile is only hashed before/after as proof.
  const realBefore = snapshotRealProfile();
  const fxRoot = join(homedir(), `ferrum-measure-fx-${Date.now().toString(36)}`);
  const fxName = parse(fxRoot).base;

  let child;
  let ws;
  let nextId = 1;
  const pending = new Map();
  let cleaned = false;

  function killApp() {
    try {
      ws?.close();
    } catch {
      /* already gone */
    }
    ws = undefined;
    try {
      if (child?.pid) execSync(`taskkill /PID ${child.pid} /T /F`, { stdio: 'ignore' });
    } catch {
      /* already exited */
    }
    child = undefined;
    // WebView2 children can outlive the parent briefly; kill leftovers that carry OUR debugging port only.
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
  async function waitPortFree() {
    const end = Date.now() + 15000;
    while (Date.now() < end) {
      try {
        await fetch(`http://127.0.0.1:${PORT}/json/version`);
      } catch {
        return;
      }
      await sleep(200);
    }
    throw new Error(`port ${PORT} still in use by a stale WebView2`);
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

  // ---- CDP ----
  function launch() {
    const t0 = performance.now();
    child = spawn(EXE, [], {
      stdio: 'ignore',
      env: {
        ...process.env,
        ...profileEnv(root),
        WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${PORT} --no-first-run`,
      },
    });
    return t0;
  }
  function send(method, params = {}) {
    const id = nextId++;
    ws.send(JSON.stringify({ id, method, params }));
    return new Promise((res, rej) => {
      pending.set(id, { res, rej });
      setTimeout(() => {
        if (pending.delete(id)) rej(new Error(`CDP timeout: ${method}`));
      }, 5000);
    });
  }
  async function connect(deadlineAt) {
    let target;
    while (performance.now() < deadlineAt && !target) {
      try {
        const r = await fetch(`http://127.0.0.1:${PORT}/json/list`);
        target = (await r.json()).find(
          (t) => t.type === 'page' && t.webSocketDebuggerUrl && t.url.includes('tauri.localhost'),
        );
      } catch {
        /* endpoint not up yet */
      }
      if (!target) await sleep(40);
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
      }
    };
    await send('Runtime.enable');
  }
  function installHelpers() {
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
    const guarded = `(window.__m || (${installHelpers.toString()})(), ${expression})`;
    const r = await send('Runtime.evaluate', {
      expression: guarded,
      returnByValue: true,
      awaitPromise: true,
    });
    if (r.exceptionDetails)
      throw new Error(
        `page eval failed: ${r.exceptionDetails.exception?.description ?? r.exceptionDetails.text}`,
      );
    return r.result?.value;
  }
  async function waitFor(expression, timeoutMs, label, pollMs = 100) {
    const end = Date.now() + timeoutMs;
    while (Date.now() < end) {
      try {
        const v = await evaluate(expression);
        if (v) return v;
      } catch {
        /* not ready */
      }
      await sleep(pollMs);
    }
    throw new Error(`timeout waiting for ${label}`);
  }

  // ---- process-tree sampling via PowerShell ----
  const PS = `
$rootId = ${'$'}args[0]
$all = Get-CimInstance Win32_Process | Select-Object ProcessId, ParentProcessId
$ids = New-Object System.Collections.Generic.List[int]
$ids.Add([int]$rootId)
for ($i = 0; $i -lt $ids.Count; $i++) {
  foreach ($c in $all) { if ($c.ParentProcessId -eq $ids[$i] -and -not $ids.Contains([int]$c.ProcessId)) { $ids.Add([int]$c.ProcessId) } }
}
$out = foreach ($p in (Get-Process -Id $ids.ToArray() -ErrorAction SilentlyContinue)) {
  [pscustomobject]@{ id = $p.Id; name = $p.ProcessName; ws = $p.WorkingSet64; pm = $p.PrivateMemorySize64; cpu = $p.TotalProcessorTime.TotalSeconds }
}
ConvertTo-Json -InputObject @($out) -Compress
`;
  const psFile = join(root, 'sample.ps1');
  writeFileSync(psFile, PS);
  function sampleTree() {
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
      {
        encoding: 'utf8',
      },
    );
    if (r.status !== 0) throw new Error(`process sampling failed: ${r.stderr}`);
    const parsed = JSON.parse(r.stdout.trim() || '[]');
    return Array.isArray(parsed) ? parsed : [parsed];
  }

  // ---- fixture ----
  mkdirSync(fxRoot, { recursive: true });
  for (let i = 0; i < FILES; i++)
    writeFileSync(join(fxRoot, `file-${String(i).padStart(5, '0')}.txt`), 'x');

  const result = { exe: EXE, runs: RUNS, files: FILES };
  try {
    console.log(`measure-app: ${RUNS} cold startups, then idle/memory/CPU run (${FILES} files)`);
    // 1. startup
    const startup = [];
    for (let i = 0; i < RUNS; i++) {
      await waitPortFree();
      const t0 = launch();
      await connect(t0 + 30000);
      await waitFor('__m.ready()', 30000, 'UI ready', 40);
      const ms = performance.now() - t0;
      startup.push(round(ms, 0));
      console.log(`  startup run ${i + 1}/${RUNS}: ${round(ms, 0)} ms`);
      killApp();
      await waitPortFree();
      await sleep(500);
    }
    result.startupMs = {
      runs: startup,
      median: median(startup),
      p95: percentile(startup, 95),
      min: Math.min(...startup),
      max: Math.max(...startup),
    };

    // 2. idle memory + cpu, then 10k folder memory (one launch)
    await waitPortFree();
    const t0 = launch();
    await connect(t0 + 30000);
    await waitFor('__m.ready()', 30000, 'UI ready', 40);
    await sleep(IDLE_MS);
    const memIdle = sampleTree();
    result.memoryIdle = {
      afterMs: IDLE_MS,
      ...sumMem(memIdle),
      perProcess: memIdle.map((p) => ({
        name: p.name,
        wsMB: round(p.ws / 1048576),
        pmMB: round(p.pm / 1048576),
      })),
    };

    const cpu0 = sampleTree();
    const w0 = performance.now();
    await sleep(CPU_MS);
    const cpu1 = sampleTree();
    const wallS = (performance.now() - w0) / 1000;
    const busyS = cpuDeltaSeconds(cpu0, cpu1);
    const cores = cpus().length;
    result.cpuIdle = {
      windowS: round(wallS),
      cpuSeconds: round(busyS, 3),
      pctOfOneCore: round((busyS / wallS) * 100, 2),
      pctOfMachine: round((busyS / wallS / cores) * 100, 2),
      logicalCores: cores,
    };

    // open the 10k-file folder: home crumb -> fixture row
    const home = parse(homedir()).base;
    let paneIdx = -1;
    for (const i of [0, 1]) {
      if (
        await evaluate(
          `__m.crumbs(${i}).some((x) => x.textContent.trim() === ${JSON.stringify(home)})`,
        )
      ) {
        paneIdx = i;
        break;
      }
    }
    if (paneIdx < 0) throw new Error('home crumb not found in either pane');
    await evaluate(
      `(() => { const c = __m.crumbs(${paneIdx}); __m.click(c.find((x) => x.textContent.trim() === ${JSON.stringify(home)})); return true; })()`,
    );
    await waitFor(
      `__m.crumbLast(${paneIdx}) === ${JSON.stringify(home)} && !__m.busy() && __m.rows(${paneIdx}).length > 0`,
      20000,
      'home listing',
    );
    // The fixture row may sit outside the virtualized window: scroll the listing until it renders.
    const findRow = `(() => { const r = __m.row(${paneIdx}, ${JSON.stringify(fxName)}); if (r) { r.scrollIntoView(); return true; } return false; })()`;
    let found = await evaluate(findRow);
    for (let k = 0; !found && k < 60; k++) {
      await evaluate(
        `(() => { const s = document.querySelector('.pane[data-pane="${paneIdx}"] [data-path]')?.closest('[class*="scroll"], [class*="list"], [style*="overflow"]'); if (s) s.scrollTop += s.clientHeight; return true; })()`,
      );
      await sleep(120);
      found = await evaluate(findRow);
    }
    if (!found) throw new Error('fixture folder row not reachable in the home listing');
    const tOpen = performance.now();
    await evaluate(
      `(() => { __m.dblclick(__m.row(${paneIdx}, ${JSON.stringify(fxName)})); return true; })()`,
    );
    await waitFor(
      `__m.crumbLast(${paneIdx}) === ${JSON.stringify(fxName)} && !__m.busy() && __m.rows(${paneIdx}).length > 0`,
      60000,
      '10k listing',
      50,
    );
    const openMs = performance.now() - tOpen;
    await sleep(IDLE_MS / 2);
    const memLoaded = sampleTree();
    const domRows = await evaluate(`__m.rows(${paneIdx}).length`);
    result.memoryFolder = {
      files: FILES,
      openListingMs: round(openMs, 0),
      renderedRows: domRows,
      ...sumMem(memLoaded),
      perProcess: memLoaded.map((p) => ({
        name: p.name,
        wsMB: round(p.ws / 1048576),
        pmMB: round(p.pm / 1048576),
      })),
    };
  } finally {
    cleanup();
  }

  // ---- output ----
  const exeStat = statSync(EXE);
  result.host = {
    os: `Windows ${release()}`,
    cpu: cpus()[0]?.model?.trim(),
    logicalCores: cpus().length,
    ramGB: round(totalmem() / 2 ** 30),
    exeMB: round(exeStat.size / 1048576, 2),
    exeBuilt: exeStat.mtime.toISOString(),
    measuredAt: new Date().toISOString(),
  };
  const sm = result.startupMs;
  const summary = [
    `startup (spawn -> first pane rows rendered), ${sm.runs.length} cold runs: median ${sm.median} ms, p95 ${sm.p95} ms (min ${sm.min}, max ${sm.max})`,
    `memory idle (+${IDLE_MS / 1000}s), ${result.memoryIdle.processes} procs: working set ${result.memoryIdle.workingSetMB} MB, private ${result.memoryIdle.privateMB} MB`,
    `memory after opening ${FILES} files (${result.memoryFolder.renderedRows} DOM rows, listing ${result.memoryFolder.openListingMs} ms): working set ${result.memoryFolder.workingSetMB} MB, private ${result.memoryFolder.privateMB} MB`,
    `cpu idle over ${result.cpuIdle.windowS} s: ${result.cpuIdle.pctOfOneCore}% of one core (${result.cpuIdle.pctOfMachine}% of ${result.cpuIdle.logicalCores} logical cores)`,
  ].join('\n');
  console.log(`\n${summary}`);
  const jsonPath = opt('json');
  if (jsonPath) writeFileSync(jsonPath, `${JSON.stringify(result, null, 2)}\n`);
  const mdPath = opt('md');
  if (mdPath) {
    writeFileSync(
      mdPath,
      `# Ferrum perf baseline ${result.host.measuredAt.slice(0, 10)}\n\n\`\`\`text\n${summary}\n\`\`\`\n\n\`\`\`json\n${JSON.stringify(result, null, 2)}\n\`\`\`\n`,
    );
  }
  if (!jsonPath && !mdPath) console.log(`\n${JSON.stringify(result)}`);
}
