/**
 * Shared isolation for every script that launches the real ferrum.exe (FER-457).
 *
 * The backend roots ALL persisted state (config.json incl. tab session, AI setup/journal, sync baselines, Activity DB,
 * WebView2 user data) under FERRUM_PROFILE_DIR when it is set. A script therefore never needs to back up and restore the
 * real config.json: it points the app at a temp profile, and (as proof) hashes the REAL profile before and after.
 *
 * usage:
 *   const real = snapshotRealProfile();
 *   child = spawn(EXE, [], { env: { ...process.env, ...profileEnv(root), WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: ... } });
 *   ...
 *   const proof = verifyRealProfile(real);   // { unchanged, files, detail } - fail the run when !unchanged
 */
import { createHash } from 'node:crypto';
import { existsSync, readdirSync, readFileSync, statSync } from 'node:fs';
import { join } from 'node:path';

const APP_ID = 'dev.ferrum.app';

/** Environment that points the app at the temp profile `dir`. USERPROFILE/TEMP stay real (WebView2 needs them). */
export function profileEnv(dir) {
  return { FERRUM_PROFILE_DIR: dir };
}

/** Real locations the app uses without an override (Known Folders resolve to the same places as these env vars). */
function realRoots() {
  const roaming = process.env.APPDATA;
  const local = process.env.LOCALAPPDATA;
  return [roaming && join(roaming, APP_ID), local && join(local, APP_ID, 'activity')].filter(
    Boolean,
  );
}

function walk(dir, out) {
  if (!existsSync(dir)) return;
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) walk(p, out);
    else out.push(p);
  }
}

/**
 * Real WebView2 web-storage dirs (localStorage / IndexedDB / sessionStorage). The whole EBWebView folder is too big
 * and churns (caches), so only these are checked, by file list + size + mtime instead of a content hash.
 */
function webStorageRoots() {
  const local = process.env.LOCALAPPDATA;
  if (!local) return [];
  const base = join(local, APP_ID, 'EBWebView', 'Default');
  return ['Local Storage', 'IndexedDB', 'Session Storage'].map((n) => join(base, n));
}

/**
 * sha256 of every file under the real config/data/activity dirs, plus size+mtime of the real WebView2 web-storage
 * files (a running app would change them - stop it first).
 */
export function snapshotRealProfile() {
  const files = [];
  for (const root of realRoots()) walk(root, files);
  const hashes = new Map();
  for (const f of files.sort())
    hashes.set(f, createHash('sha256').update(readFileSync(f)).digest('hex'));
  const stored = [];
  for (const root of webStorageRoots()) walk(root, stored);
  for (const f of stored.sort()) {
    const st = statSync(f);
    hashes.set(f, `stat:${st.size}:${Math.trunc(st.mtimeMs)}`);
  }
  return hashes;
}

export function verifyRealProfile(before) {
  const after = snapshotRealProfile();
  const changed = [];
  for (const [f, h] of after)
    if (before.get(f) !== h) changed.push(before.has(f) ? `modified ${f}` : `new ${f}`);
  for (const f of before.keys()) if (!after.has(f)) changed.push(`removed ${f}`);
  const short = (m, f) => (m.get(f) ?? '-').slice(0, 12);
  const detail = changed.length
    ? changed.join('; ')
    : [...after.keys()]
        .map((f) => `${f.split(/[\\/]/).slice(-2).join('/')}=${short(after, f)}`)
        .join(' ');
  return { unchanged: changed.length === 0, files: after.size, detail };
}
