/**
 * Archive-flow smoke E2E over a REAL WebView2 window (Windows-only). FER-448.
 *
 * Same plumbing as scripts/smoke-activity.mjs: launches the built ferrum.exe with an isolated
 * FERRUM_PROFILE_DIR + CDP and drives the real UI (real Tauri IPC, no mock). Scenarios:
 *   1. Enter on a ZIP opens it as a virtual folder, member list is right, Backspace goes back.
 *   2. Same for a 7z.
 *   3. Alt+F9 (unpack to the other pane), then every extracted file is compared byte for byte.
 *   4. Alt+F5 (pack) several items into a ZIP, then the ZIP is parsed in Node and compared; Alt+Shift+F9
 *      (test archive) reports OK for the good archives.
 *   5. Quick View (Ctrl+Q) on a text member and an image member inside the ZIP and the 7z.
 *   6. Corrupt archives show a safe error message and the app stays alive and usable.
 *
 * Fixtures (zip, 7z, nested folders, tiny PNG, corrupt archives) are generated at runtime by the tiny
 * encoders below (no 7-Zip, no downloads) under the user's profile directory (the pane can only reach hidden
 * %TEMP% via showHidden) and removed at the end. Screenshots go to FERRUM_SMOKE_ARTIFACTS
 * (default: %TEMP%\ferrum-archive-smoke-shots) and are NOT removed; they are never committed.
 *
 * Note: Enter on a file row hands the file to the OS association (`open_external`). The Enter checks below
 * therefore press Enter on a ZIP/7z once each; if Enter does not enter the archive (FER-450) the OS may try to open it.
 *
 * ISOLATION: FERRUM_PROFILE_DIR (see smoke-activity); the real profile is hashed before/after and the run FAILS if it changed.
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
  readdirSync,
  statSync,
} from 'node:fs';
import { tmpdir, homedir } from 'node:os';
import { join, parse } from 'node:path';
import { profileEnv, snapshotRealProfile, verifyRealProfile } from './lib/test-profile.mjs';
import zlib from 'node:zlib';

const EXE = process.argv[2];
const PORT = 9224;
if (!EXE) {
  console.error('usage: node scripts/smoke-archive.mjs <path-to-ferrum.exe>');
  process.exit(2);
}

const root = mkdtempSync(join(tmpdir(), 'ferrum-arc-smoke-'));
// Isolation is FERRUM_PROFILE_DIR (backend); the real profile is only hashed before/after as proof.
const realBefore = snapshotRealProfile();
const shotDir = process.env.FERRUM_SMOKE_ARTIFACTS ?? join(tmpdir(), 'ferrum-archive-smoke-shots');
mkdirSync(shotDir, { recursive: true });

// ---- fixture encoders (ZIP deflate/stored, 7z Copy, PNG) -------------------------------------------------------
const crc32 = (buf) => zlib.crc32(buf) >>> 0;
const u16 = (n) => {
  const b = Buffer.alloc(2);
  b.writeUInt16LE(n);
  return b;
};
const u32 = (n) => {
  const b = Buffer.alloc(4);
  b.writeUInt32LE(n >>> 0);
  return b;
};
const u64 = (n) => {
  const b = Buffer.alloc(8);
  b.writeBigUInt64LE(BigInt(n));
  return b;
};

/** entries: [{ name, data? }] - a name ending in "/" is a directory entry. */
function buildZip(entries) {
  const locals = [];
  const central = [];
  let offset = 0;
  for (const e of entries) {
    const isDir = e.name.endsWith('/');
    const raw = isDir ? Buffer.alloc(0) : e.data;
    const comp = isDir ? raw : zlib.deflateRawSync(raw);
    const method = isDir ? 0 : 8;
    const name = Buffer.from(e.name, 'utf8');
    const crc = crc32(raw);
    const local = Buffer.concat([
      u32(0x04034b50),
      u16(20),
      u16(0x0800),
      u16(method),
      u16(0),
      u16(0x21),
      u32(crc),
      u32(comp.length),
      u32(raw.length),
      u16(name.length),
      u16(0),
      name,
      comp,
    ]);
    central.push(
      Buffer.concat([
        u32(0x02014b50),
        u16(20),
        u16(20),
        u16(0x0800),
        u16(method),
        u16(0),
        u16(0x21),
        u32(crc),
        u32(comp.length),
        u32(raw.length),
        u16(name.length),
        u16(0),
        u16(0),
        u16(0),
        u16(0),
        u32(isDir ? 0x10 : 0),
        u32(offset),
        name,
      ]),
    );
    locals.push(local);
    offset += local.length;
  }
  const cd = Buffer.concat(central);
  return Buffer.concat([
    ...locals,
    cd,
    u32(0x06054b50),
    u16(0),
    u16(0),
    u16(entries.length),
    u16(entries.length),
    u32(cd.length),
    u32(offset),
    u16(0),
  ]);
}

/** Minimal ZIP reader for verification (central directory + inflate). Returns Map(name -> Buffer|null for dirs). */
function readZip(buf) {
  let eocd = -1;
  for (let i = buf.length - 22; i >= 0; i--)
    if (buf.readUInt32LE(i) === 0x06054b50) {
      eocd = i;
      break;
    }
  if (eocd < 0) throw new Error('no EOCD');
  const n = buf.readUInt16LE(eocd + 10);
  let p = buf.readUInt32LE(eocd + 16);
  const out = new Map();
  for (let i = 0; i < n; i++) {
    const method = buf.readUInt16LE(p + 10);
    const csize = buf.readUInt32LE(p + 20);
    const nlen = buf.readUInt16LE(p + 28);
    const xlen = buf.readUInt16LE(p + 30);
    const clen = buf.readUInt16LE(p + 32);
    const lho = buf.readUInt32LE(p + 42);
    const name = buf.subarray(p + 46, p + 46 + nlen).toString('utf8');
    const lnlen = buf.readUInt16LE(lho + 26);
    const lxlen = buf.readUInt16LE(lho + 28);
    const start = lho + 30 + lnlen + lxlen;
    const comp = buf.subarray(start, start + csize);
    out.set(
      name,
      name.endsWith('/') ? null : method === 8 ? zlib.inflateRawSync(comp) : Buffer.from(comp),
    );
    p += 46 + nlen + xlen + clen;
  }
  return out;
}

const num7z = (n) => {
  if (n < 0x80) return Buffer.from([n]);
  if (n < 0x4000) return Buffer.from([0x80 | (n >> 8), n & 0xff]);
  if (n < 0x200000) return Buffer.from([0xc0 | (n >> 16), n & 0xff, (n >> 8) & 0xff]);
  throw new Error('7z fixture number too large');
};

/** entries: [{ name, data? }] (dir when name ends with "/"). One Copy folder per file, plain (unencoded) header. */
function build7z(entries) {
  const dirsFirstNames = entries.map((e) => e.name.replace(/\/$/, ''));
  const files = entries.filter((e) => !e.name.endsWith('/') && e.data.length > 0);
  const packed = Buffer.concat(files.map((f) => f.data));
  const h = [];
  const push = (...b) => h.push(Buffer.from(b));
  push(0x01); // Header
  if (files.length > 0) {
    push(0x04); // MainStreamsInfo
    push(0x06);
    h.push(num7z(0), num7z(files.length)); // PackInfo: packPos, numPackStreams
    push(0x09);
    for (const f of files) h.push(num7z(f.data.length));
    push(0x00);
    push(0x07); // UnPackInfo
    push(0x0b);
    h.push(num7z(files.length));
    push(0x00); // Folder, external=0
    for (let i = 0; i < files.length; i++) {
      push(0x01);
      push(0x01);
      push(0x00);
    } // 1 coder, idSize 1, Copy
    push(0x0c);
    for (const f of files) h.push(num7z(f.data.length)); // CodersUnpackSize
    push(0x0a, 0x01);
    for (const f of files) h.push(u32(crc32(f.data))); // CRCs, allDefined
    push(0x00);
    push(0x08, 0x00); // SubStreamsInfo (default: one substream per folder)
    push(0x00);
  }
  push(0x05);
  h.push(num7z(entries.length)); // FilesInfo
  const emptyStream = entries.map((e) => e.name.endsWith('/') || e.data.length === 0);
  if (emptyStream.some(Boolean)) {
    const bits = Buffer.alloc(Math.ceil(entries.length / 8));
    emptyStream.forEach((v, i) => {
      if (v) bits[i >> 3] |= 0x80 >> (i & 7);
    });
    push(0x0e);
    h.push(num7z(bits.length), bits);
    const emptyIdx = entries.filter((_, i) => emptyStream[i]);
    if (emptyIdx.some((e) => !e.name.endsWith('/'))) {
      const eb = Buffer.alloc(Math.ceil(emptyIdx.length / 8));
      emptyIdx.forEach((e, i) => {
        if (!e.name.endsWith('/')) eb[i >> 3] |= 0x80 >> (i & 7);
      });
      push(0x0f);
      h.push(num7z(eb.length), eb);
    }
  }
  const names = Buffer.concat(dirsFirstNames.map((n) => Buffer.from(`${n}\0`, 'utf16le')));
  push(0x11);
  h.push(num7z(names.length + 1), Buffer.from([0]), names);
  push(0x00); // end FilesInfo
  push(0x00); // end Header
  const header = Buffer.concat(h);
  const startHeader = Buffer.concat([u64(packed.length), u64(header.length), u32(crc32(header))]);
  return Buffer.concat([
    Buffer.from([0x37, 0x7a, 0xbc, 0xaf, 0x27, 0x1c, 0x00, 0x04]),
    u32(crc32(startHeader)),
    startHeader,
    packed,
    header,
  ]);
}

function buildPng() {
  const chunk = (type, data) => {
    const td = Buffer.concat([Buffer.from(type, 'ascii'), data]);
    return Buffer.concat([u32be(data.length), td, u32be(crc32(td))]);
  };
  const w = 8;
  const h = 8;
  const ihdr = Buffer.concat([u32be(w), u32be(h), Buffer.from([8, 2, 0, 0, 0])]);
  const rows = [];
  for (let y = 0; y < h; y++) {
    const row = [0];
    for (let x = 0; x < w; x++) row.push(200 - x * 10, 40 + y * 10, 120);
    rows.push(Buffer.from(row));
  }
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk('IHDR', ihdr),
    chunk('IDAT', zlib.deflateSync(Buffer.concat(rows))),
    chunk('IEND', Buffer.alloc(0)),
  ]);
}
function u32be(n) {
  const b = Buffer.alloc(4);
  b.writeUInt32BE(n >>> 0);
  return b;
}

// ---- fixture tree ------------------------------------------------------------------------------------------------
const fxRoot = join(homedir(), `ferrum-arcsmoke-fx-${Date.now().toString(36)}`);
const fxName = parse(fxRoot).base;
const MEMBERS = [
  { name: 'docs/' },
  { name: 'docs/deep/' },
  { name: 'img/' },
  {
    name: 'readme.txt',
    data: Buffer.from('FERRUM-ARC-README line one\nline two: umlaut ü ok\n', 'utf8'),
  },
  { name: 'docs/notes.txt', data: Buffer.from('FERRUM-ARC-NOTES '.repeat(40), 'utf8') },
  { name: 'docs/deep/inner.txt', data: Buffer.from('FERRUM-ARC-INNER nested member\n', 'utf8') },
  { name: 'img/dot.png', data: buildPng() },
];
const MEMBER_FILES = MEMBERS.filter((m) => m.data);
const ROOT_NAMES = [
  'src',
  'out',
  'data.zip',
  'data.7z',
  'dirs.zip',
  'dirs.7z',
  'bad.zip',
  'bad.7z',
];
const TOP_NAMES = ['docs', 'img', 'readme.txt']; // members listed at the archive root
const SRC_FILES = {
  'a.txt': Buffer.from('pack source a\n'),
  'b.txt': Buffer.from('pack source b\n'.repeat(30)),
  'sub/c.txt': Buffer.from('pack source c in sub\n'),
};

mkdirSync(join(fxRoot, 'src', 'sub'), { recursive: true });
mkdirSync(join(fxRoot, 'out'), { recursive: true });
for (const [rel, data] of Object.entries(SRC_FILES)) writeFileSync(join(fxRoot, 'src', rel), data);
// data.* carry IMPLICIT folders only. dirs.* carry explicit folder entries (what Explorer/7-Zip write): a 7z of that
// kind hangs the pane (FER-449) and a ZIP of that kind cannot be unpacked (FER-451); they are checked separately so
// the rest of the flow still gets exercised.
writeFileSync(join(fxRoot, 'data.zip'), buildZip(MEMBER_FILES));
writeFileSync(join(fxRoot, 'dirs.zip'), buildZip(MEMBERS));
writeFileSync(join(fxRoot, 'data.7z'), build7z(MEMBER_FILES));
writeFileSync(join(fxRoot, 'dirs.7z'), build7z(MEMBERS));
writeFileSync(
  join(fxRoot, 'bad.zip'),
  Buffer.concat([buildZip(MEMBERS).subarray(0, 60), Buffer.from('garbage-not-a-zip')]),
);
{
  const good = build7z(MEMBERS);
  const bad = Buffer.from(good);
  bad.fill(0x5a, bad.length - 40); // destroy the tail (header area) -> header CRC mismatch
  writeFileSync(join(fxRoot, 'bad.7z'), bad);
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const results = [];
const check = (name, ok, detail = '') => {
  results.push({ name, ok });
  console.log(`${ok ? 'PASS' : 'FAIL'} ${name}${detail ? ` - ${detail}` : ''}`);
  return ok;
};
const startedAt = Date.now();

// ---- process + CDP plumbing -------------------------------------------------------------------------------------
let child;
let ws;
let nextId = 1;
let paneIdx = 0; // pane under test (A = fixture root); the other one is the unpack target
const pending = new Map();
const shots = [];
const pageErrors = []; // uncaught page exceptions / console.error lines (first line only)

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

const appAlive = () => !!child && child.exitCode === null && child.signalCode === null;

function send(method, params = {}) {
  const id = nextId++;
  if (process.env.FERRUM_SMOKE_DEBUG && method !== 'Runtime.evaluate')
    console.log(`DBG ${method} ${JSON.stringify(params).slice(0, 100)}`);
  if (process.env.FERRUM_SMOKE_DEBUG && method === 'Runtime.evaluate')
    console.log(`DBG eval ${String(params.expression).slice(-110).replace(/\s+/g, ' ')}`);
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
const tryWait = (expression, ms, label) =>
  waitFor(expression, ms, label).then(
    () => true,
    () => false,
  );

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
    if (msg.method === 'Runtime.exceptionThrown') {
      pageErrors.push(
        String(
          msg.params.exceptionDetails?.exception?.description ?? msg.params.exceptionDetails?.text,
        )
          .split(String.fromCharCode(10))[0]
          .slice(0, 200),
      );
      return;
    }
    if (msg.method === 'Runtime.consoleAPICalled' && msg.params.type === 'error') {
      pageErrors.push(
        msg.params.args
          .map((x) => x.value ?? x.description ?? '')
          .join(' ')
          .split(String.fromCharCode(10))[0]
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

async function shot(name) {
  try {
    const res = await send('Page.captureScreenshot', { format: 'png' });
    const file = join(shotDir, `${String(shots.length + 1).padStart(2, '0')}-${name}.png`);
    writeFileSync(file, Buffer.from(res.data, 'base64'));
    shots.push(file);
  } catch (e) {
    console.log(`WARN screenshot ${name} failed: ${e}`);
  }
}

// Real key events through CDP (not synthetic window events), so the registry sees what a user's keyboard sends.
const KEYS = {
  Enter: { code: 'Enter', vk: 13 },
  Backspace: { code: 'Backspace', vk: 8 },
  Escape: { code: 'Escape', vk: 27 },
  F5: { code: 'F5', vk: 116 },
  F9: { code: 'F9', vk: 120 },
  q: { code: 'KeyQ', vk: 81 },
};
async function key(k, { alt = false, ctrl = false, shift = false } = {}) {
  const def = KEYS[k];
  const modifiers = (alt ? 1 : 0) | (ctrl ? 2 : 0) | (shift ? 8 : 0);
  const base = {
    key: k,
    code: def.code,
    windowsVirtualKeyCode: def.vk,
    nativeVirtualKeyCode: def.vk,
    modifiers,
  };
  await send('Input.dispatchKeyEvent', { type: 'rawKeyDown', ...base });
  await send('Input.dispatchKeyEvent', { type: 'keyUp', ...base });
}

// In-page helpers (serialized into the page; must not close over Node scope).
function installHelpers() {
  const q = (s, r = document) => r.querySelector(s);
  const qa = (s, r = document) => [...r.querySelectorAll(s)];
  const last = (p) =>
    String(p)
      .replace(/[\\/]+$/, '')
      .split(/[\\/]/)
      .pop();
  const S = {
    q,
    qa,
    last,
    pane: () => q(`.pane[data-pane="${window.__paneIdx}"]`),
    rows: () => qa(`.pane[data-pane="${window.__paneIdx}"] [data-path]`),
    row: (name) => S.rows().find((r) => last(r.getAttribute('data-path')) === name),
    names: () => S.rows().map((r) => last(r.getAttribute('data-path'))),
    crumbs: () => qa(`.pane[data-pane="${window.__paneIdx}"] .crumb`),
    paneCrumbs: (i) => qa(`.pane[data-pane="${i}"] .crumb`),
    crumbLast: () => S.crumbs().at(-1)?.textContent?.trim() ?? '',
    busy: () => !!q('[aria-busy="true"]'),
    paneError: () =>
      q(`.pane[data-pane="${window.__paneIdx}"] [role="alert"] .stateText`)?.textContent?.trim() ??
      '',
    dblclick: (el) => {
      el.dispatchEvent(new MouseEvent('mousedown', { bubbles: true, cancelable: true }));
      el.dispatchEvent(new MouseEvent('dblclick', { bubbles: true, cancelable: true }));
    },
    click: (el, o = {}) => {
      el.dispatchEvent(new MouseEvent('mousedown', { bubbles: true, cancelable: true, ...o }));
      el.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true, ...o }));
    },
    activate: () =>
      S.pane()?.dispatchEvent(new MouseEvent('mousedown', { bubbles: true, cancelable: true })),
    selected: () =>
      S.rows()
        .filter((r) => r.getAttribute('aria-selected') === 'true')
        .map((r) => last(r.getAttribute('data-path'))),
    dialog: () => q('.modal, [role="dialog"], [role="alertdialog"]'),
    dialogText: () => (S.dialog()?.innerText ?? '').replace(/\s+/g, ' ').trim(),
    btn: (text, root = document) =>
      qa('button', root).find((b) => b.textContent.trim().startsWith(text)),
    qv: () => q('aside[aria-label="Quick View"]'),
  };
  window.__s = S;
}

// ---- Node-side UI drivers -----------------------------------------------------------------------------------------
const idle = () => waitFor(`!__s.busy()`, 10000, 'listing idle');

async function gotoDir(i, subName) {
  paneIdx = i;
  // Pane paths can be collapsed ("C: > U... > h..."), so always restart from the first (drive) crumb and descend.
  const parts = homedir()
    .split(/[\\/]+/)
    .filter(Boolean);
  const drive = parts.shift();
  const clicked = await page(() => {
    const c = __s.crumbs();
    if (c.length === 0) return false;
    __s.click(c[0]);
    return true;
  });
  if (!clicked) throw new Error(`no crumbs in pane ${i}`);
  await waitFor(
    `__s.crumbLast() === ${JSON.stringify(drive)} && !__s.busy() && __s.rows().length > 0`,
    12000,
    'drive listing',
  );
  for (const part of [...parts, fxName]) {
    await waitFor(`!!__s.row(${JSON.stringify(part)})`, 10000, `row ${part}`);
    await page((n) => __s.dblclick(__s.row(n)), part);
    await waitFor(`__s.crumbLast() === ${JSON.stringify(part)}`, 10000, `crumb ${part}`);
    await idle();
  }
  await waitFor(`!!__s.row('src')`, 10000, 'fixture root listing');
  if (subName) {
    await page((n) => __s.dblclick(__s.row(n)), subName);
    await waitFor(`__s.crumbLast() === ${JSON.stringify(subName)}`, 8000, `crumb ${subName}`);
    await idle();
  }
}

async function activatePane(i) {
  paneIdx = i;
  await evaluate(`__s.activate()`);
  await sleep(150);
}

/** Click a row (sets cursor + selection). ctrl = add to selection. */
async function selectRow(name, ctrl = false) {
  const ok = await page(
    (n, c) => {
      const r = __s.row(n);
      if (!r) return false;
      __s.click(r, { ctrlKey: c });
      return true;
    },
    name,
    ctrl,
  );
  if (!ok) throw new Error(`row not found: ${name}`);
  await sleep(150);
}

const listing = async () => (await evaluate(`__s.names()`)).sort();
const sameSet = (a, b) => JSON.stringify([...a].sort()) === JSON.stringify([...b].sort());

async function enterByKey(name) {
  await activatePane(paneIdx);
  await selectRow(name);
  await key('Enter');
}
async function enterByDblclick(name) {
  await page((n) => __s.dblclick(__s.row(n)), name);
}
const crumbCount = () => evaluate(`__s.crumbs().length`);

/** Open an archive in pane A (key first when asked), return how it entered. */
async function openArchive(name, via) {
  const before = await crumbCount();
  if (via === 'key') await enterByKey(name);
  else await enterByDblclick(name);
  const entered = await tryWait(
    `__s.crumbs().length > ${before} || !!__s.paneError()`,
    6000,
    'archive open',
  );
  await sleep(300);
  return { entered, crumbs: await crumbCount(), before };
}

async function goBack() {
  const before = await crumbCount();
  await activatePane(paneIdx);
  await key('Backspace');
  await tryWait(`__s.crumbs().length < ${before}`, 6000, 'backspace up');
  await idle().catch(() => undefined);
}

/** Walk a directory into Map(relpath(/) -> Buffer). */
function walk(dir, base = dir, out = new Map()) {
  for (const e of readdirSync(dir)) {
    const p = join(dir, e);
    if (statSync(p).isDirectory()) walk(p, base, out);
    else out.set(p.slice(base.length + 1).replaceAll('\\', '/'), readFileSync(p));
  }
  return out;
}
const emptyDir = (dir) => {
  for (const e of readdirSync(dir)) rmSync(join(dir, e), { recursive: true, force: true });
};

function compareExtracted(dir) {
  const got = walk(dir);
  // The engine may extract flat into the target or under a folder named after the archive; accept both, exactly.
  const prefixes = [
    '',
    ...new Set(
      [...got.keys()]
        .map((k) => k.split('/')[0])
        .filter((k) => !MEMBER_FILES.some((m) => m.name === k)),
    ),
  ];
  for (const pre of prefixes) {
    const want = MEMBER_FILES.map((m) => (pre ? `${pre}/${m.name}` : m.name));
    if (want.every((w) => got.has(w)) && got.size === want.length) {
      const identical = MEMBER_FILES.every((m, i) => got.get(want[i]).equals(m.data));
      return { layout: pre ? `under "${pre}/"` : 'flat', identical, count: got.size };
    }
  }
  return { layout: 'mismatch', identical: false, count: got.size, names: [...got.keys()] };
}

async function dismissDialogs() {
  for (let i = 0; i < 4; i++) {
    if (!(await evaluate(`!!__s.dialog()`))) return;
    await key('Escape');
    await sleep(250);
  }
}

// ---- run ----------------------------------------------------------------------------------------------------------
try {
  // WebView2 occasionally fails to expose CDP right after another instance was killed: retry the launch once.
  for (let attempt = 1; ; attempt++) {
    launch();
    try {
      await connect();
      break;
    } catch (e) {
      killApp();
      if (attempt >= 2) throw e;
      console.log(`NOTE CDP endpoint not reachable on attempt ${attempt}, relaunching`);
      await sleep(3000);
    }
  }
  await waitFor(`__s.crumbs().length > 0`, 25000, 'pane rendered');
  await sleep(500);
  // Disclosure banners are not in scope; dismiss the first-run notice if it blocks nothing (best effort).
  await evaluate(`__s.btn('Mengerti')?.click()`).catch(() => undefined);

  await waitFor(
    `__s.paneCrumbs(0).length > 0 && __s.paneCrumbs(1).length > 0`,
    20000,
    'both panes rendered',
  );
  const A = 0;
  const B = 1;
  await gotoDir(A);
  await gotoDir(B, 'out');
  await activatePane(A);
  await evaluate(`1`);
  await shot('00-fixture-ready');
  check(
    'fixture listing in pane A',
    sameSet(await listing(), ROOT_NAMES),
    (await listing()).join(','),
  );
  check(
    'running inside the real Tauri webview (real IPC, not mocked)',
    (await evaluate(
      `'__TAURI_INTERNALS__' in window && location.host.endsWith('tauri.localhost')`,
    )) === true,
  );

  // ===== 1 + 2: open ZIP / 7z with Enter, listing, Backspace =====
  for (const [fmt, file] of [
    ['ZIP', 'data.zip'],
    ['7z', 'data.7z'],
  ]) {
    paneIdx = A;
    let r = await openArchive(file, 'key');
    const enterEntered = r.crumbs > r.before;
    check(
      `${fmt}: Enter on the archive enters it as a folder`,
      enterEntered,
      `crumbs ${r.before}->${r.crumbs}`,
    );
    if (!enterEntered) {
      // Fall back to double-click (the other documented entry path) so the remaining checks still run.
      r = await openArchive(file, 'dbl');
      check(
        `${fmt}: double-click on the archive enters it (fallback path)`,
        r.crumbs > r.before,
        `crumbs ${r.before}->${r.crumbs}`,
      );
    }
    await idle().catch(() => undefined);
    await waitFor(`__s.rows().length > 0 || !!__s.paneError()`, 8000, 'member listing').catch(
      () => undefined,
    );
    await sleep(300);
    const names = await listing();
    check(`${fmt}: root member list is correct`, sameSet(names, TOP_NAMES), names.join(','));
    await shot(`${fmt}-archive-root`);
    // nested folder
    await page((n) => __s.dblclick(__s.row(n)), 'docs');
    await waitFor(`!!__s.row('notes.txt')`, 6000, 'docs listing').catch(() => undefined);
    const docs = await listing();
    check(
      `${fmt}: nested folder list is correct`,
      sameSet(docs, ['notes.txt', 'deep']),
      docs.join(','),
    );
    await goBack();
    await goBack();
    const back = await listing();
    check(
      `${fmt}: Backspace returns to the fixture folder`,
      sameSet(back, ROOT_NAMES),
      back.join(','),
    );
  }

  // ===== 2b: 7z with explicit folder entries (what 7-Zip writes) must list, not hang (FER-449) =====
  {
    paneIdx = A;
    const r = await openArchive('dirs.7z', 'dbl');
    const ok = await tryWait(`!!__s.row('readme.txt')`, 8000, 'dirs.7z listing');
    const state = await evaluate(
      `JSON.stringify({ rows: __s.rows().length, busy: __s.busy(), err: __s.paneError() })`,
    );
    check(
      '7z with explicit folder entries lists its members (FER-449)',
      ok && sameSet(await listing(), TOP_NAMES),
      `${state}; entered=${r.crumbs > r.before}`,
    );
    await shot('dirs-7z-listing');
    // recover from a stuck "Loading" state through the parent crumb (Backspace needs a settled pane)
    await page(() => {
      const c = __s.crumbs();
      __s.click(c[c.length - 2]);
    });
    await waitFor(`!!__s.row('data.zip')`, 8000, 'fixture root after dirs.7z');
    await idle();
  }

  // ===== 3: Alt+F9 unpack to the other pane (B at fixture/out), byte compare =====
  const outDir = join(fxRoot, 'out');
  for (const [fmt, file] of [
    ['ZIP', 'data.zip'],
    ['7z', 'data.7z'],
    ['ZIP (explicit folder entries, FER-451)', 'dirs.zip'],
    ['7z (explicit folder entries)', 'dirs.7z'],
  ]) {
    emptyDir(outDir);
    paneIdx = A;
    await activatePane(A);
    await selectRow(file);
    check(
      `${fmt}: archive row is selected before Alt+F9`,
      (await evaluate(`__s.selected()`)).includes(file),
    );
    await key('F9', { alt: true });
    // May show a conflict dialog (none expected: target is empty); wait until files appear or an error dialog does.
    const done = await (async () => {
      const deadline = Date.now() + 20000;
      while (Date.now() < deadline) {
        if (readdirSync(outDir).length > 0 && !(await evaluate(`__s.busy()`))) {
          await sleep(800);
          return true;
        }
        const dlg = await evaluate(`__s.dialogText()`);
        if (dlg) return false;
        await sleep(250);
      }
      return false;
    })();
    const dlgAfter = await evaluate(`__s.dialogText()`);
    await shot(`${fmt}-after-alt-f9`);
    if (!done)
      check(
        `${fmt}: Alt+F9 extracted files to the target pane folder`,
        false,
        `no files; dialog="${dlgAfter.slice(0, 160)}"`,
      );
    else {
      const cmp = compareExtracted(outDir);
      check(
        `${fmt}: Alt+F9 extracted all ${MEMBER_FILES.length} files`,
        cmp.layout !== 'mismatch',
        `layout ${cmp.layout}, ${cmp.count} files${cmp.names ? ` got=${cmp.names.join(',')}` : ''}`,
      );
      check(`${fmt}: extracted contents identical byte for byte`, cmp.identical === true);
    }
    await dismissDialogs();
  }
  emptyDir(outDir);

  // ===== 4: Alt+F5 pack several items into a ZIP, verify; then test-archive =====
  paneIdx = A;
  await page((n) => __s.dblclick(__s.row(n)), 'src');
  await waitFor(`__s.crumbLast() === 'src' && !!__s.row('a.txt')`, 8000, 'src listing');
  await idle();
  await activatePane(A);
  await selectRow('a.txt');
  await selectRow('b.txt', true);
  await selectRow('sub', true);
  const sel = await evaluate(`__s.selected()`);
  check(
    'pack: three items selected (a.txt, b.txt, sub)',
    sameSet(sel, ['a.txt', 'b.txt', 'sub']),
    sel.join(','),
  );
  await key('F5', { alt: true });
  const packOpen = await tryWait(
    `__s.dialogText().includes('Pack to archive')`,
    6000,
    'pack dialog',
  );
  const packText = await evaluate(`__s.dialogText()`);
  check(
    'Alt+F5 opens the Pack dialog with a ZIP target',
    packOpen && /\.zip/i.test(packText),
    packText.slice(0, 140),
  );
  await shot('pack-dialog');
  await evaluate(`__s.btn('Pack', __s.dialog())?.click()`);
  const packed = join(fxRoot, 'src.zip');
  const created = await (async () => {
    const deadline = Date.now() + 15000;
    while (Date.now() < deadline) {
      if (existsSync(packed) && !(await evaluate(`!!__s.dialog()`))) return true;
      await sleep(250);
    }
    return false;
  })();
  check(
    'pack: archive file created and dialog closed',
    created,
    `dialog="${(await evaluate(`__s.dialogText()`)).slice(0, 120)}"`,
  );
  if (created) {
    let zip;
    try {
      zip = readZip(readFileSync(packed));
    } catch (e) {
      zip = null;
      check('pack: output parses as a ZIP', false, String(e));
    }
    if (zip) {
      const files = [...zip.entries()].filter(([, v]) => v !== null);
      const wantNames = Object.keys(SRC_FILES);
      const prefixes = [
        ...new Set(
          files
            .map(([n]) => n.split('/')[0])
            .filter((n) => !wantNames.some((w) => w.split('/')[0] === n)),
        ),
      ];
      const pre = prefixes.length === 1 ? `${prefixes[0]}/` : '';
      const okNames = files.length === wantNames.length && wantNames.every((w) => zip.has(pre + w));
      check('pack: ZIP lists exactly a.txt, b.txt, sub/c.txt', okNames, [...zip.keys()].join(','));
      check(
        'pack: ZIP member contents identical byte for byte',
        okNames && wantNames.every((w) => zip.get(pre + w).equals(SRC_FILES[w])),
      );
    }
  }
  // back to the fixture root, then Alt+Shift+F9 on the good archives and the packed one
  await goBack();
  await waitFor(
    `!!__s.row('data.zip') && !!__s.row('src.zip')`,
    8000,
    'fixture root with src.zip',
  ).catch(() => undefined);
  for (const file of ['data.zip', 'data.7z', 'dirs.zip', 'dirs.7z', 'src.zip']) {
    await activatePane(A);
    await selectRow(file);
    await key('F9', { alt: true, shift: true });
    const opened = await tryWait(
      `!!__s.dialog() && __s.dialogText().length > 0`,
      15000,
      'test dialog',
    );
    const text = await evaluate(`__s.dialogText()`);
    check(
      `test archive (Alt+Shift+F9) reports OK for ${file}`,
      opened && /OK/.test(text) && !/FAIL|error|broken|damaged/i.test(text),
      text.slice(0, 160),
    );
    if (file === 'data.zip') await shot('test-archive-result');
    await dismissDialogs();
  }

  // ===== 5: Quick View on members =====
  paneIdx = A;
  await activatePane(A);
  await key('q', { ctrl: true });
  check('Ctrl+Q opens Quick View', await tryWait(`!!__s.qv()`, 5000, 'quick view'));
  for (const [fmt, file] of [
    ['ZIP', 'data.zip'],
    ['7z', 'data.7z'],
  ]) {
    paneIdx = A;
    let r = await openArchive(file, 'dbl');
    if (!(r.crumbs > r.before)) throw new Error(`could not enter ${file}`);
    await waitFor(`!!__s.row('readme.txt')`, 8000, 'archive listing');
    await selectRow('readme.txt');
    const txt = await tryWait(
      `__s.qv()?.innerText.includes('FERRUM-ARC-README')`,
      10000,
      'text preview',
    );
    const qvText = await evaluate(
      `(__s.qv()?.innerText ?? '').replace(/\\s+/g, ' ').slice(0, 220)`,
    );
    check(`${fmt}: Quick View shows the text member`, txt, qvText);
    await shot(`${fmt}-quickview-text`);
    await page((n) => __s.dblclick(__s.row(n)), 'img');
    await waitFor(`!!__s.row('dot.png')`, 6000, 'img listing');
    await selectRow('dot.png');
    const img = await tryWait(
      `(() => { const i = __s.qv()?.querySelector('img.image'); return !!i && i.complete && i.naturalWidth === 8 && i.naturalHeight === 8; })()`,
      10000,
      'image preview',
    );
    const imgInfo = await evaluate(
      `(() => { const i = __s.qv()?.querySelector('img.image'); return i ? i.naturalWidth + 'x' + i.naturalHeight : 'no <img>: ' + (__s.qv()?.innerText ?? '').replace(/\\s+/g, ' ').slice(0, 120); })()`,
    );
    check(`${fmt}: Quick View shows the image member (8x8 decoded)`, img, imgInfo);
    await shot(`${fmt}-quickview-image`);
    await goBack();
    await goBack();
  }
  await key('q', { ctrl: true });

  // ===== 6: corrupt archives =====
  for (const file of ['bad.zip', 'bad.7z']) {
    paneIdx = A;
    await waitFor(`!!__s.row(${JSON.stringify(file)})`, 8000, 'fixture root');
    const r = await openArchive(file, 'dbl');
    await tryWait(`!!__s.paneError()`, 8000, 'archive error');
    const err = await evaluate(`__s.paneError()`);
    await shot(`${file.replace('.', '-')}-error`);
    const safe =
      err.length > 0 &&
      err.length < 400 &&
      !/panick|stack|backtrace|RUST_BACKTRACE|0x[0-9a-f]{6,}/i.test(err) &&
      !err.includes(fxRoot);
    check(
      `${file}: shows a safe error message (no panic/stack/full fixture path)`,
      safe,
      `"${err.slice(0, 200)}" (entered=${r.crumbs > r.before})`,
    );
    // alive and usable: process still running, UI answers, parent navigation works
    const alive = appAlive() && (await evaluate(`1 + 1`)) === 2;
    check(`${file}: app did not crash`, alive);
    await goBack();
    await waitFor(`!!__s.row('data.zip')`, 8000, 'back at fixture root').catch(() => undefined);
    check(
      `${file}: pane is usable afterwards (Backspace back to folder)`,
      (await listing()).includes('data.zip'),
    );
  }
  // Test-archive on a corrupt one must say so, not claim success.
  await activatePane(A);
  await selectRow('bad.zip');
  await key('F9', { alt: true, shift: true });
  await tryWait(`!!__s.dialog() && __s.dialogText().length > 0`, 15000, 'test dialog bad');
  const badText = await evaluate(`__s.dialogText()`);
  await shot('test-archive-bad-zip');
  check(
    'test archive on corrupt ZIP does not report OK',
    badText.length > 0 &&
      !/^OK\b/.test(badText.replace(/^.*?(OK|FAIL)/, '$1')) &&
      !/OK —.*bad\.zip/.test(badText),
    badText.slice(0, 200),
  );
  await dismissDialogs();
  check('app still alive at the end', appAlive() && (await evaluate(`1 + 1`)) === 2);
} catch (error) {
  check('smoke run completed without error', false, String(error));
  try {
    const st = await evaluate(
      `JSON.stringify({ pane: window.__paneIdx, crumbs: __s.crumbs().map((c) => c.textContent.trim()), rows: __s.rows().length, busy: __s.busy(), err: __s.paneError(), dlg: __s.dialogText().slice(0, 120) })`,
    );
    console.log(`STATE ${st}`);
  } catch {
    /* page not reachable */
  }
  await shot('failure').catch(() => undefined);
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

// FER-454: a Svelte effect loop is a bug, not noise.
const effectLoops = pageErrors.filter((e) => e.includes('effect_update_depth_exceeded')).length;
check(
  'no Svelte effect_update_depth_exceeded page error',
  effectLoops === 0,
  effectLoops ? `${effectLoops} seen` : '',
);
if (pageErrors.length > 0)
  console.log(
    `NOTE page errors seen (${pageErrors.length}): ${[...new Set(pageErrors)].slice(0, 5).join(' || ')}`,
  );
console.log(`SCREENSHOTS ${shotDir} (${shots.length} files, kept, not committed)`);
const failed = results.filter((r) => !r.ok);
console.log(
  `\nSMOKE RESULT: ${results.length - failed.length}/${results.length} passed in ${Math.round((Date.now() - startedAt) / 1000)}s`,
);
process.exit(failed.length === 0 ? 0 : 1);
