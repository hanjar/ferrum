// Shared helper of the sync desktop smoke: removes stale sync baselines left by earlier smoke runs.
// A baseline file stores the (lower-cased, canonical) root pair, so a file that mentions the smoke fixture folder
// belongs to the smoke. Every other file may be a user's baseline and is never touched.
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

export const SMOKE_MARKER = 'ferrum-sync-smoke-tmp';

/** `%APPDATA%\dev.ferrum.app\sync-baselines` (override with FERRUM_SYNC_BASELINES_DIR, used by tests). */
export function baselineDir() {
  if (process.env.FERRUM_SYNC_BASELINES_DIR) return process.env.FERRUM_SYNC_BASELINES_DIR;
  // FER-457: a test profile keeps baselines under <profile>/data (same layout as the backend's profile module).
  if (process.env.FERRUM_PROFILE_DIR?.trim())
    return path.join(process.env.FERRUM_PROFILE_DIR, 'data', 'sync-baselines');
  const appData = process.env.APPDATA ?? path.join(os.homedir(), 'AppData', 'Roaming');
  return path.join(appData, 'dev.ferrum.app', 'sync-baselines');
}

/** Deletes only `.json` files in `dir` whose content contains the smoke marker. Returns the removed file names. */
export function cleanSmokeBaselines(dir = baselineDir(), marker = SMOKE_MARKER) {
  const removed = [];
  let names;
  try {
    names = fs.readdirSync(dir);
  } catch {
    return removed; // directory absent: nothing to clean
  }
  const needle = marker.toLowerCase();
  for (const name of names) {
    if (!name.endsWith('.json')) continue;
    const file = path.join(dir, name);
    try {
      if (!fs.statSync(file).isFile()) continue;
      if (fs.readFileSync(file, 'utf8').toLowerCase().includes(needle)) {
        fs.rmSync(file);
        removed.push(name);
      }
    } catch {
      // unreadable or vanished: leave it alone
    }
  }
  return removed;
}
