/**
 * Quick View PDF smoke over a REAL WebView2 window (Windows-only). FER-453 / FER-501.
 *
 * Launches the built ferrum.exe with an isolated FERRUM_PROFILE_DIR and CDP, then:
 *   1. The seeded profile (restored tab session + preview.openOnLaunch) opens Quick View on the
 *      fixture folder, which holds one small generated PDF.
 *   2. The sandboxed pdf.js viewer (an iframe target of its own in WebView2) draws a canvas with
 *      real pixels: the in-thread worker works under the Tauri header CSP (P1).
 *   3. No CSP violation and no page error comes from the viewer target during the render.
 *   4. The main page console has zero errors (clean after FER-500).
 *   5. P2 regression: from INSIDE the viewer, `__TAURI_INTERNALS__.invoke('save_prefs', ...)` and
 *      `window.ipc.postMessage(...)` do not change prefs: config.json is hashed before and after.
 *   6. CSP blocks fetch from inside the viewer to IPC endpoints (http://ipc.localhost,
 *      ipc://localhost, http://tauri.localhost).
 *
 * ISOLATION: FERRUM_PROFILE_DIR; the real profile is hashed before/after and the run FAILS if it changed.
 * Artifacts (screenshots) go to FERRUM_SMOKE_ARTIFACTS (default %TEMP%\ferrum-pdf-smoke), never committed.
 *
 * usage: node scripts/smoke-pdf-preview.mjs <path-to-ferrum.exe>
 */
import { spawn, execSync } from 'node:child_process';
import { mkdtempSync, mkdirSync, rmSync, writeFileSync, readFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createHash } from 'node:crypto';
import { profileEnv, snapshotRealProfile, verifyRealProfile } from './lib/test-profile.mjs';

const EXE = process.argv[2];
const PORT = 9341;
if (!EXE) {
  console.error('usage: node scripts/smoke-pdf-preview.mjs <path-to-ferrum.exe>');
  process.exit(2);
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const root = mkdtempSync(join(tmpdir(), 'ferrum-pdf-smoke-'));
const profileDir = join(root, 'profile');
const configDirPath = join(profileDir, 'config'); // the backend keeps prefs under <profile>/config
const fixtureDir = join(root, 'fixture');
const shotDir = process.env.FERRUM_SMOKE_ARTIFACTS ?? join(tmpdir(), 'ferrum-pdf-smoke');
for (const d of [configDirPath, fixtureDir, shotDir]) mkdirSync(d, { recursive: true });
const realBefore = snapshotRealProfile();

// ---- fixture: one page, 200x100 points, black "Hi" (valid PDF with a correct xref table) ----
function smallPdf() {
  const stream = 'BT /F1 40 Tf 10 40 Td (Hi) Tj ET';
  const objects = [
    '<< /Type /Catalog /Pages 2 0 R >>',
    '<< /Type /Pages /Kids [3 0 R] /Count 1 >>',
    '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 4 0 R /Resources << >> >>',
    `<< /Length ${stream.length} >>\nstream\n${stream}\nendstream`,
  ];
  let out = '%PDF-1.4\n';
  const offsets = [];
  objects.forEach((body, i) => {
    offsets.push(out.length);
    out += `${i + 1} 0 obj\n${body}\nendobj\n`;
  });
  const xref = out.length;
  out += `xref\n0 ${objects.length + 1}\n0000000000 65535 f \n`;
  for (const off of offsets) out += `${String(off).padStart(10, '0')} 00000 n \n`;
  out += `trailer\n<< /Size ${objects.length + 1} /Root 1 0 R >>\nstartxref\n${xref}\n%%EOF\n`;
  return Buffer.from(out, 'latin1');
}
writeFileSync(join(fixtureDir, 'small.pdf'), smallPdf());

// ---- seeded prefs: restore the fixture folder in pane 0, open Quick View on launch ----
const configFile = join(configDirPath, 'config.json');
writeFileSync(
  configFile,
  JSON.stringify({
    showHidden: false,
    tabs: { restoreTabs: true },
    session: { panes: [{ tabs: [fixtureDir], current: 0 }] },
    preview: { openOnLaunch: true },
  }),
);
const configHash = () => createHash('sha256').update(readFileSync(configFile)).digest('hex');

const results = [];
let child;
const mainProblems = [];
const viewerProblems = [];

function check(name, ok, detail = '') {
  results.push({ name, ok: !!ok, detail: String(detail).slice(0, 300) });
  console.log(
    `${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? ` - ${String(detail).slice(0, 200)}` : ''}`,
  );
}

function launch() {
  child = spawn(EXE, [], {
    stdio: 'ignore',
    env: {
      ...process.env,
      ...profileEnv(profileDir),
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

/** One CDP session on a target's websocket. Problems from its events go to `problems`. */
function openCdp(url, problems) {
  const sock = new WebSocket(url);
  const pending = new Map();
  let nextId = 1;
  const ready = new Promise((resolve, reject) => {
    sock.onopen = resolve;
    sock.onerror = reject;
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
      problems.push(
        `exception: ${d.exception?.description ?? d.text}`.split('\n')[0].slice(0, 300),
      );
    }
    if (msg.method === 'Runtime.consoleAPICalled' && msg.params.type === 'error') {
      problems.push(
        `console.error: ${(msg.params.args ?? []).map((a) => a.value ?? a.description ?? '').join(' ')}`.slice(
          0,
          300,
        ),
      );
    }
    if (msg.method === 'Log.entryAdded' && msg.params.entry.level === 'error') {
      problems.push(`log: ${msg.params.entry.text}`.slice(0, 400));
    }
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
    const res = await send('Runtime.evaluate', {
      expression,
      returnByValue: true,
      awaitPromise: true,
    });
    if (res.exceptionDetails) {
      throw new Error(res.exceptionDetails.exception?.description ?? res.exceptionDetails.text);
    }
    return res.result?.value;
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

async function main() {
  let failed = false;
  let page;
  let viewer;
  try {
    launch();
    const mainTarget = await waitUntil(
      async () => (await listTargets()).find((t) => t.type === 'page' && t.webSocketDebuggerUrl),
      40000,
      'CDP page target',
    );
    page = openCdp(mainTarget.webSocketDebuggerUrl, mainProblems);
    await page.ready;
    await page.send('Runtime.enable');
    await page.send('Log.enable');

    // 1. Quick View open on launch
    await waitUntil(
      () => page.evaluate(`!!document.querySelector('aside[aria-label="Quick View"]')`),
      40000,
      'Quick View panel',
    );
    check('Quick View opens on launch (preview.openOnLaunch)', true);

    // 2. The viewer is its own iframe target: attach to it
    const viewerTarget = await waitUntil(
      async () =>
        (await listTargets()).find(
          (t) => t.type === 'iframe' && t.url.includes('/pdf-viewer.html'),
        ),
      30000,
      'pdf-viewer iframe target',
    );
    check('viewer iframe target exists (sandboxed pdf.js frame)', !!viewerTarget);
    // Diagnostic recorder: every message the main page receives (sources are opaque, so only data).
    await page.evaluate(`(() => {
      window.__pdfMsgs = [];
      window.addEventListener('message', (e) => window.__pdfMsgs.push(JSON.stringify(e.data).slice(0, 120)));
    })()`);
    viewer = openCdp(viewerTarget.webSocketDebuggerUrl, viewerProblems);
    await viewer.ready;
    await viewer.send('Runtime.enable');
    await viewer.send('Log.enable');

    // 3. canvas with real pixels, drawn inside the viewer
    const dark = await waitUntil(
      async () => {
        const n = await viewer.evaluate(`(() => {
          const c = document.querySelector('#pages canvas');
          if (!c || c.width === 0) return 0;
          const d = c.getContext('2d').getImageData(0, 0, c.width, c.height).data;
          let n = 0;
          for (let i = 0; i < d.length; i += 4) if (d[i] < 64 && d[i + 3] > 0) n++;
          return n;
        })()`);
        return n > 20 ? n : false;
      },
      40000,
      'rendered PDF page pixels',
    );
    check(
      'PDF renders: canvas has real dark pixels (fixture "Hi")',
      dark > 20,
      `dark pixels=${dark}`,
    );
    const canvases = await viewer.evaluate(`document.querySelectorAll('#pages canvas').length`);
    check('one page canvas for the one-page fixture', canvases === 1, `canvases=${canvases}`);
    const status = await viewer.evaluate(`document.getElementById('status').textContent`);
    check('viewer shows no error text', status === '', `status="${status}"`);
    const shot = await page.send('Page.captureScreenshot', { format: 'png' });
    writeFileSync(join(shotDir, 'pdf-preview-rendered.png'), Buffer.from(shot.data, 'base64'));

    // 4. no CSP violation or page error from the viewer target during the render
    await sleep(1000);
    const viewerCsp = viewerProblems.filter((m) =>
      /Content Security Policy|violates|Refused to|blob:|Worker|script-src|worker-src/i.test(m),
    );
    check('no CSP violation from the viewer target', viewerCsp.length === 0, viewerCsp.join(' | '));
    check(
      'no page error from the viewer target',
      viewerProblems.length === 0,
      viewerProblems.join(' | '),
    );
    check('main page console has no errors', mainProblems.length === 0, mainProblems.join(' | '));

    // 5. P2 regression: IPC from inside the viewer must not change prefs
    await sleep(1500);
    const before = configHash();
    const probe = await viewer.evaluate(`(async () => {
      const race = (p) => Promise.race([p, new Promise((r) => setTimeout(() => r('timeout'), 3000))]);
      const out = { hasInternals: typeof window.__TAURI_INTERNALS__, hasIpc: typeof window.ipc };
      try {
        out.invoke = await race(window.__TAURI_INTERNALS__.invoke('save_prefs', { prefs: { showHidden: true } }));
      } catch (e) { out.invokeErr = String(e).slice(0, 120); }
      try {
        window.ipc.postMessage(JSON.stringify({ cmd: 'save_prefs', callback: 0, error: 0, payload: { prefs: { showHidden: true } } }));
        out.ipc = 'sent';
      } catch (e) { out.ipcErr = String(e).slice(0, 120); }
      return out;
    })()`);
    await sleep(2500);
    const after = configHash();
    const cfg = JSON.parse(readFileSync(configFile, 'utf8'));
    check(
      'frame IPC did not change prefs (config.json unchanged)',
      before === after,
      `probe=${JSON.stringify(probe)}`,
    );
    check(
      'showHidden is still false after the frame attempts',
      cfg.showHidden === false,
      `showHidden=${cfg.showHidden}`,
    );

    const fetchProbe = await viewer.evaluate(`(async () => {
      const urls = [
        'http://ipc.localhost/load_prefs',
        'ipc://localhost/load_prefs',
        'http://tauri.localhost/',
      ];
      const out = {};
      for (const url of urls) {
        const controller = new AbortController();
        const timer = setTimeout(() => controller.abort('timeout'), 3000);
        try {
          const res = await fetch(url, { signal: controller.signal });
          out[url] = { ok: true, status: res.status, statusText: res.statusText };
        } catch (e) {
          out[url] = { ok: false, error: String(e).slice(0, 120) };
        } finally {
          clearTimeout(timer);
        }
      }
      return out;
    })()`);
    const allFetchesBlocked =
      Boolean(fetchProbe) &&
      Object.keys(fetchProbe).length === 3 &&
      Object.values(fetchProbe).every((r) => !r.ok);
    const fetchDetail = Object.entries(fetchProbe ?? {})
      .map(([url, r]) => `${url}: ${r.ok ? `response (${r.status})` : r.error}`)
      .join('; ');
    check('frame cannot fetch IPC endpoints (CSP blocks)', allFetchesBlocked, fetchDetail);
  } catch (e) {
    failed = true;
    check('smoke ran to completion', false, e instanceof Error ? e.message : String(e));
    try {
      if (viewer) {
        const vinfo = await viewer.evaluate(
          `JSON.stringify({ status: document.getElementById('status').textContent, canvases: document.querySelectorAll('#pages canvas').length, pages: document.getElementById('pages').innerHTML.slice(0, 200) })`,
        );
        console.log(`diagnostic viewer: ${vinfo}`);
        if (page)
          console.log(
            `diagnostic parent: ${await page.evaluate(`JSON.stringify({ text: document.querySelector('.pdfViewer')?.innerText ?? null, msgs: (window.__pdfMsgs ?? []).slice(0, 8) })`).catch((e) => String(e))}`,
          );
        console.log(`diagnostic viewer problems: ${JSON.stringify(viewerProblems).slice(0, 1500)}`);
        console.log(
          `diagnostic main problems: ${JSON.stringify(mainProblems.filter((m) => !/ipc\.localhost|font/.test(m))).slice(0, 1500)}`,
        );
      }
    } catch (diagErr) {
      console.log(
        `diagnostic viewer failed: ${diagErr instanceof Error ? diagErr.message : diagErr}`,
      );
    }
    try {
      if (page) {
        const info = await page.evaluate(
          `JSON.stringify({ title: document.title, quickView: !!document.querySelector('aside[aria-label="Quick View"]') })`,
        );
        console.log(`diagnostic main page: ${info}`);
        const snap = await page.send('Page.captureScreenshot', { format: 'png' });
        writeFileSync(join(shotDir, 'pdf-preview-failure.png'), Buffer.from(snap.data, 'base64'));
      }
    } catch (diagErr) {
      console.log(`diagnostic failed: ${diagErr instanceof Error ? diagErr.message : diagErr}`);
    }
  } finally {
    viewer?.close();
    page?.close();
    killApp();
    await sleep(800);
    const proof = verifyRealProfile(realBefore);
    check('real profile unchanged (isolation)', proof.unchanged, proof.detail);
    try {
      rmSync(root, { recursive: true, force: true });
    } catch {
      /* best effort */
    }
  }
  const passed = results.filter((r) => r.ok).length;
  console.log(`\n${passed}/${results.length} checks passed${failed ? ' (run aborted)' : ''}`);
  process.exit(failed || results.some((r) => !r.ok) ? 1 : 0);
}

main();
