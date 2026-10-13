/**
 * Icon theme smoke over a REAL WebView2 window (Windows-only). FER-478.
 * Launches ferrum.exe with an isolated FERRUM_PROFILE_DIR + CDP, drives Settings > Appearance and saves one
 * screenshot per (theme mode x palette x icon theme x icon style): details list and grid view, each with the Quick View panel = "other" preview. usage: node scripts/smoke-icon-themes.mjs <ferrum.exe> [outDir]
 */
import { spawn, execSync } from 'node:child_process';
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { randomBytes } from 'node:crypto';
import { profileEnv, snapshotRealProfile, verifyRealProfile } from './lib/test-profile.mjs';

const EXE = process.argv[2];
const OUT = process.argv[3] ?? join(tmpdir(), 'ferrum-icon-theme-shots');
const PORT = 9227;
if (!EXE) {
  console.error('usage: node scripts/smoke-icon-themes.mjs <ferrum.exe> [outDir]');
  process.exit(2);
}
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const root = mkdtempSync(join(tmpdir(), 'ferrum-icon-smoke-'));
const realBefore = snapshotRealProfile();
mkdirSync(OUT, { recursive: true });

const fix = join(root, 'fixture');
const fix2 = join(root, 'fixture-grid');
for (const d of [fix, fix2]) mkdirSync(d, { recursive: true });
const names = [
  'report.pdf',
  'main.rs',
  'photo.png',
  'clip.mp4',
  'song.mp3',
  'bundle.zip',
  'sheet.xlsx',
  'data.csv',
  'settings.json',
  'notes.docx',
  'README.md',
  'blob.bin',
  'script.ps1',
  'movie.mkv',
];
for (const d of [fix, fix2]) {
  mkdirSync(join(d, 'Folder'), { recursive: true });
  for (const n of names) writeFileSync(join(d, n), randomBytes(512));
}
mkdirSync(join(root, 'config'), { recursive: true });
writeFileSync(
  join(root, 'config', 'config.json'),
  JSON.stringify({
    theme: 'dark',
    preview: { openOnLaunch: true },
    session: {
      panes: [
        { tabs: [fix], current: 0 },
        { tabs: [fix2], current: 0 },
      ],
    },
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
      20000,
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
const key = (k, extra = {}) =>
  send('Input.dispatchKeyEvent', { type: 'keyDown', key: k, ...extra }).then(() =>
    send('Input.dispatchKeyEvent', { type: 'keyUp', key: k, ...extra }),
  );
const clickRadio = (label, group) =>
  ev(`(() => {
    const scope = ${group ? `document.querySelector('[role=radiogroup][aria-label="${group}"]')` : 'document'};
    const b = [...scope.querySelectorAll('[role=radio]')].find(x => (x.querySelector('.chipLabel')?.textContent.trim() === ${JSON.stringify(label)} || x.textContent.trim() === ${JSON.stringify(label)} || x.getAttribute('aria-label') === ${JSON.stringify(label)} || x.title === ${JSON.stringify(label)}));
    if (!b) return false; b.click(); return true; })()`);

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
  await sleep(1000);

  const shots = [];
  for (const mode of ['Dark', 'Light']) {
    for (const palette of ['Pop Violet', 'Aqua Signal']) {
      for (const style of ['Colored', 'Monochrome']) {
        for (const th of ['Line', 'Duotone', 'Label', 'Tile']) {
          await key(',', { modifiers: 2, code: 'Comma', windowsVirtualKeyCode: 188 });
          await sleep(250);
          const ok = [
            await clickRadio(mode),
            await clickRadio(palette),
            await clickRadio(style),
            await clickRadio(th, 'Icon theme'),
          ];
          if (ok.includes(false))
            throw new Error(`radio missing ${mode}/${palette}/${style}/${th}: ${ok}`);
          await ev(
            `document.querySelector('[role=dialog][aria-label="Settings"] [title="Close"]')?.click()`,
          );
          await sleep(350);
          const attr = await ev(
            `document.documentElement.dataset.iconTheme + '|' + document.documentElement.dataset.icon`,
          );
          if (attr !== `${th}|${style.toLowerCase()}`) throw new Error(`dataset mismatch ${attr}`);
          for (const view of ['Details', 'Grid']) {
            await ev(`document.querySelector('button[title="${view}"]').click()`);
            await sleep(400);
            // the Quick View panel follows the cursor: put it on sheet.xlsx ("other" preview)
            await ev(`document.querySelector('[data-path$="sheet.xlsx"]')?.click()`);
            await sleep(700);
            const shot = await send('Page.captureScreenshot', { format: 'png' });
            const f = `${mode.toLowerCase()}-${palette.startsWith('Pop') ? 'violet' : 'aqua'}-${style.toLowerCase()}-${th.toLowerCase()}-${view === 'Grid' ? 'grid' : 'list'}.png`;
            writeFileSync(join(OUT, f), Buffer.from(shot.data, 'base64'));
            shots.push(f);
          }
        }
      }
    }
  }
  console.log(`saved ${shots.length} screenshots to ${OUT}`);
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
  console.log('real profile unchanged:', proof.unchanged);
  if (!proof.unchanged) process.exitCode = 1;
}
