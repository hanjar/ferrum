/**
 * CSP / IPC-transport measurement of the REAL ferrum.exe over WebView2 (Windows-only, FER-500).
 *
 * Answers the two acceptance questions for the main-page CSP change in tauri.conf.json:
 *   1. How many CSP violations does the MAIN page log (baseline noise: blocked ipc.localhost fetches,
 *      blocked data: fonts), split by kind?
 *   2. How long does a simple invoke take (load_prefs), and does the invoke path itself add new CSP
 *      violations (fetch to ipc.localhost blocked -> postMessage fallback)?
 *
 * usage: node scripts/measure-ipc-csp.mjs <path-to-ferrum.exe> [--invokes 20] [--json out.json]
 *
 * Isolation: FERRUM_PROFILE_DIR temp profile (see ./lib/test-profile.mjs); the REAL profile is hashed
 * before/after and the exit code is 4 if it changed. CDP port 9350 (smoke-pdf-preview uses 9341).
 */
import { spawn, execSync } from 'node:child_process';
import { mkdtempSync, mkdirSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { profileEnv, snapshotRealProfile, verifyRealProfile } from './lib/test-profile.mjs';

const PORT = 9350;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const round = (n, d = 1) => Math.round(n * 10 ** d) / 10 ** d;
const median = (v) => {
  const s = [...v].sort((a, b) => a - b);
  return s.length % 2 ? s[(s.length - 1) / 2] : (s[s.length / 2 - 1] + s[s.length / 2]) / 2;
};
const p95 = (v) => {
  const s = [...v].sort((a, b) => a - b);
  return s[Math.min(s.length - 1, Math.max(0, Math.ceil(0.95 * s.length) - 1))];
};

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
  const INVOKES = Number(opt('invokes', 20));
  const jsonPath = opt('json');
  if (!EXE) {
    console.error(
      'usage: node scripts/measure-ipc-csp.mjs <path-to-ferrum.exe> [--invokes 20] [--json out.json]',
    );
    process.exit(2);
  }

  const root = mkdtempSync(join(tmpdir(), 'ferrum-csp-measure-'));
  const realBefore = snapshotRealProfile();

  let child;
  const problems = [];

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
      if (child?.pid) execSync(`taskkill /PID ${child.pid} /T /F`, { stdio: 'ignore' });
    } catch {
      /* already exited */
    }
    child = undefined;
  }

  // Minimal CDP websocket client; every error-level console/log message is kept in `problems`,
  // every network request URL in `requests` (proves which transport invoke really uses).
  function openCdp(url, requests) {
    const sock = new WebSocket(url);
    const pending = new Map();
    let nextId = 1;
    const ready = new Promise((res, rej) => {
      sock.onopen = res;
      sock.onerror = rej;
    });
    sock.onmessage = (event) => {
      const msg = JSON.parse(event.data);
      if (msg.id && pending.has(msg.id)) {
        const p = pending.get(msg.id);
        pending.delete(msg.id);
        if (msg.error) p.reject(new Error(msg.error.message));
        else p.resolve(msg.result);
        return;
      }
      if (msg.method === 'Runtime.exceptionThrown') {
        const d = msg.params.exceptionDetails;
        problems.push(`exception: ${d.exception?.description ?? d.text}`.split('\n')[0]);
      }
      if (
        msg.method === 'Runtime.consoleAPICalled' &&
        ['error', 'warning'].includes(msg.params.type)
      )
        problems.push(
          `console.${msg.params.type}: ${(msg.params.args ?? [])
            .map((a) => a.value ?? a.description ?? '')
            .join(' ')}`,
        );
      if (msg.method === 'Log.entryAdded')
        problems.push(`${msg.params.entry.level}: ${msg.params.entry.text}`);
      if (msg.method === 'Network.requestWillBeSent') requests.push(msg.params.request?.url ?? '');
    };
    const send = (method, params = {}) => {
      const id = nextId++;
      sock.send(JSON.stringify({ id, method, params }));
      return new Promise((resolve, reject) => {
        pending.set(id, { resolve, reject });
        setTimeout(() => {
          if (pending.has(id)) {
            pending.delete(id);
            reject(new Error(`CDP timeout: ${method}`));
          }
        }, 20000);
      });
    };
    const evaluate = async (expression) => {
      const r = await send('Runtime.evaluate', {
        expression,
        returnByValue: true,
        awaitPromise: true,
      });
      if (r.exceptionDetails)
        throw new Error(r.exceptionDetails.exception?.description ?? r.exceptionDetails.text);
      return r.result?.value;
    };
    return { ready, send, evaluate, close: () => sock.close() };
  }

  async function listTargets() {
    return (await fetch(`http://127.0.0.1:${PORT}/json/list`)).json();
  }
  async function waitUntil(fn, timeoutMs, label) {
    const deadline = Date.now() + timeoutMs;
    let last;
    while (Date.now() < deadline) {
      try {
        last = await fn();
        if (last) return last;
      } catch (e) {
        last = e;
      }
      await sleep(250);
    }
    throw new Error(
      `timeout waiting for ${label}${last instanceof Error ? `: ${last.message}` : ''}`,
    );
  }

  const categorize = (m) =>
    /ipc\.localhost|ipc:/i.test(m)
      ? 'ipc-fetch'
      : /data:.*font|font.*data:/i.test(m)
        ? 'font-data'
        : /Content Security Policy|violates/i.test(m)
          ? 'csp-other'
          : 'other';

  let page;
  const requests = [];
  const result = { exe: EXE, invokes: INVOKES };
  let failed = false;
  try {
    // A stale WebView2 from an aborted earlier run would keep holding the CDP port, and the
    // freshly launched app could never serve /json/list on it (same guard as measure-app.mjs).
    const freeDeadline = Date.now() + 15000;
    while (Date.now() < freeDeadline) {
      try {
        await fetch(`http://127.0.0.1:${PORT}/json/version`);
      } catch {
        break; // nothing listening - the port is free
      }
      await sleep(200);
      if (Date.now() >= freeDeadline)
        throw new Error(`port ${PORT} still in use by a stale WebView2`);
    }
    launch();
    const mainTarget = await waitUntil(
      async () =>
        (await listTargets()).find(
          (t) =>
            t.type === 'page' &&
            t.webSocketDebuggerUrl &&
            /tauri\.localhost|localhost:5173/.test(t.url),
        ) ??
        // fallback: the first page target if the URL hasn't been reported yet
        (await listTargets()).find((t) => t.type === 'page' && t.webSocketDebuggerUrl),
      40000,
      'CDP page target',
    );
    result.pageUrl = mainTarget.url;
    page = openCdp(mainTarget.webSocketDebuggerUrl, requests);
    await page.ready;
    await page.send('Runtime.enable');
    await page.send('Log.enable');
    await page.send('Network.enable');

    // UI alive and IPC responsive before counting anything
    await waitUntil(
      () =>
        page.evaluate(
          `(async () => { if (!window.__TAURI_INTERNALS__) return false; try { await window.__TAURI_INTERNALS__.invoke('load_prefs'); return true; } catch { return false; } })()`,
        ),
      40000,
      'Tauri IPC ready',
    );
    await sleep(3000); // settle: let startup-driven CSP violations finish arriving

    result.docUrl = await page.evaluate(`document.location.href`);
    const baseline = problems.slice();
    const byKindGrouped = {};
    for (const m of baseline) (byKindGrouped[categorize(m)] ??= []).push(m.slice(0, 200));
    result.baseline = {
      total: baseline.length,
      byKind: Object.fromEntries(Object.entries(byKindGrouped).map(([k, v]) => [k, v.length])),
      samples: Object.fromEntries(
        Object.entries(byKindGrouped).map(([k, v]) => [k, v.slice(0, 3)]),
      ),
    };
    console.log(
      `baseline main-page console problems: ${baseline.length} ${JSON.stringify(result.baseline.byKind)}`,
    );

    // invoke latency: 1 warmup + INVOKES timed load_prefs calls
    const times = [];
    const problemsBeforeInvokes = problems.length;
    const ipcReqBeforeInvokes = requests.filter((u) => u.includes('ipc.localhost')).length;
    await page.evaluate(
      `(async () => { await window.__TAURI_INTERNALS__.invoke('load_prefs'); return true; })()`,
    );
    for (let i = 0; i < INVOKES; i++) {
      const ms = await page.evaluate(
        `(async () => { const t0 = performance.now(); await window.__TAURI_INTERNALS__.invoke('load_prefs'); return performance.now() - t0; })()`,
      );
      times.push(round(ms, 2));
    }
    const ipcFetchDuringInvokes =
      requests.filter((u) => u.includes('ipc.localhost')).length - ipcReqBeforeInvokes;
    result.invoke = {
      warmup: 'excluded',
      times,
      meanMs: round(times.reduce((a, b) => a + b, 0) / times.length, 2),
      medianMs: round(median(times), 2),
      p95Ms: round(p95(times), 2),
      newProblemsDuringInvokes: problems.slice(problemsBeforeInvokes).length,
      ipcFetchRequestsDuringInvokes: ipcFetchDuringInvokes,
    };
    console.log(
      `invoke load_prefs x${INVOKES}: mean ${result.invoke.meanMs} ms, median ${result.invoke.medianMs} ms, p95 ${result.invoke.p95Ms} ms; ipc.localhost requests during invokes: ${ipcFetchDuringInvokes}; new console problems: ${result.invoke.newProblemsDuringInvokes}`,
    );
    console.log(`startup ipc.localhost requests: ${ipcReqBeforeInvokes}`);
    console.log(`document URL: ${result.docUrl}`);
  } catch (e) {
    failed = true;
    result.error = e instanceof Error ? e.message : String(e);
    console.error(`FAILED: ${result.error}`);
  } finally {
    page?.close();
    killApp();
    await sleep(800);
    const proof = verifyRealProfile(realBefore);
    result.realProfileUnchanged = proof.unchanged;
    if (!proof.unchanged) {
      console.error(`real profile MODIFIED: ${proof.detail}`);
      process.exitCode = 4;
    }
    try {
      rmSync(root, { recursive: true, force: true });
    } catch {
      /* best effort */
    }
  }

  if (jsonPath) {
    mkdirSync(join(jsonPath, '..'), { recursive: true });
    const { writeFileSync } = await import('node:fs');
    writeFileSync(jsonPath, `${JSON.stringify(result, null, 2)}\n`);
  } else {
    console.log(`\n${JSON.stringify(result, null, 2)}`);
  }
  if (failed) process.exitCode = 1;
}
