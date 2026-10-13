/**
 * FER-414 — two-way baseline lifecycle: load before planning, save only after a verified run.
 *
 * WHAT IS REAL AND WHAT IS FAKE (read before trusting a green run):
 * - REAL: the temp directories and every file operation below (node:fs), plus the TS engine
 *   under test (syncPlan, syncBaseline, syncService).
 * - FAKE ("filesystem-backed IPC double", NOT the Rust engine): `compareDirs` (a stat/bytes based
 *   re-implementation), `enqueueCopy` (copyFileSync + a scripted job result), `validateSyncRoots`,
 *   and the baseline store (a JSON file per pair in a temp dir; its corrupt/restart behaviour is
 *   exercised, but the Rust store itself is proven in `src-tauri/tests/sync_baseline_fs.rs`).
 * The Rust compare/copy/store with real files is covered by the Rust tests, not here.
 */
import { afterAll, beforeEach, describe, expect, it, vi } from 'vitest';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import crypto from 'node:crypto';
import type { CompareDirsReport, DirDiffEntry, DirDiffState } from './ipc/compare';
import type { OperationProgress } from './ipc/types';
import type { SyncBaseline } from './syncBaseline';
import { ui } from './uiState.svelte';

// ── filesystem-backed IPC double ─────────────────────────────────────────
const T0 = 1_600_000_000; // seconds; fixed old mtimes so metadata compare is deterministic
const root = fs.mkdtempSync(path.join(os.tmpdir(), 'ferrum-fer414-'));
const storeDir = path.join(root, '_store');
fs.mkdirSync(storeDir);

const calls = vi.hoisted(() => ({
  validate: [] as [string, string][],
  compare: 0,
  baselineLoad: 0,
  baselineSave: 0,
}));
const jobs = new Map<string, OperationProgress>();
let jobSeq = 0;
let copyBehavior: 'ok' | 'fail' | 'cancel' | 'partial' | 'running' = 'ok';
let saveLimit: string | null = null;

function norm(p: string): string {
  return path.resolve(p).split(path.sep).join('/').toLowerCase();
}

function storeFile(l: string, r: string): string {
  const key = crypto
    .createHash('sha1')
    .update(`${norm(l)}|${norm(r)}`)
    .digest('hex');
  return path.join(storeDir, key);
}

function walk(dir: string, rel = ''): Map<string, fs.Stats> {
  const out = new Map<string, fs.Stats>();
  for (const name of fs.readdirSync(path.join(dir, rel))) {
    const r = rel ? `${rel}/${name}` : name;
    const st = fs.statSync(path.join(dir, r));
    if (st.isDirectory()) for (const [k, v] of walk(dir, r)) out.set(k, v);
    else out.set(r, st);
  }
  return out;
}

function fakeCompare(l: string, r: string, byContent: boolean): CompareDirsReport {
  calls.compare++;
  const lf = walk(l);
  const rf = walk(r);
  const names = [...new Set([...lf.keys(), ...rf.keys()])].sort();
  const entries: DirDiffEntry[] = names.map((name) => {
    const a = lf.get(name);
    const b = rf.get(name);
    const facts = {
      leftSize: a?.size ?? 0,
      leftModifiedMs: a ? Math.floor(a.mtimeMs) : 0,
      rightSize: b?.size ?? 0,
      rightModifiedMs: b ? Math.floor(b.mtimeMs) : 0,
    };
    let state: DirDiffState;
    if (a && !b) state = 'leftOnly';
    else if (!a && b) state = 'rightOnly';
    else if (a!.size !== b!.size) state = 'sizeDifferent';
    else if (
      byContent &&
      !fs.readFileSync(path.join(l, name)).equals(fs.readFileSync(path.join(r, name)))
    ) {
      state = 'contentDifferent';
    } else if (Math.abs(a!.mtimeMs - b!.mtimeMs) <= 2000) state = 'same';
    // Like the Rust compare: the content phase only demotes to contentDifferent; equal bytes with another mtime
    // stay newerLeft/newerRight (found by the desktop smoke, FER-168).
    else state = a!.mtimeMs > b!.mtimeMs ? 'newerLeft' : 'newerRight';
    return { name, state, ...facts };
  });
  return { leftRoot: l, rightRoot: r, entries, truncated: false, recursive: true };
}

vi.mock('./ipc/compare', () => ({
  compareDirs: async (
    l: string,
    r: string,
    byContent: boolean,
    opts?: { incremental?: boolean },
  ) => {
    const rep = fakeCompare(l, r, byContent);
    // FER-477b double: an incremental compare returns fresh journal marks (one per call).
    if (opts?.incremental) {
      const usn = String(calls.compare * 1000);
      rep.journal = {
        left: { journalId: '7', usn, rootFrn: '5', path: 'C:/L' },
        right: { journalId: '8', usn, rootFrn: '6', path: 'D:/R' },
        takenAtMs: 1_800_000_000_000,
      };
    }
    return rep;
  },
  compareFiles: vi.fn(),
  cancelCompare: vi.fn(),
  validateSyncRoots: async (l: string, r: string) => {
    calls.validate.push([l, r]);
    const a = norm(l);
    const b = norm(r);
    if (a === b || a.startsWith(b + '/') || b.startsWith(a + '/')) {
      throw new Error('roots are same or nested');
    }
  },
}));

vi.mock('./ipc/mutations', () => ({
  enqueueCopy: async (sources: string[], targetDir: string) => {
    const id = `job-${++jobSeq}`;
    const base = {
      operationId: id,
      kind: 'copy' as const,
      processedBytes: 0,
      processedFiles: sources.length,
    };
    if (copyBehavior === 'running') {
      jobs.set(id, { ...base, state: 'running' });
    } else if (copyBehavior === 'fail') {
      jobs.set(id, {
        ...base,
        state: 'failed',
        error: 'disk error',
        failures: sources.map((s) => ({ path: s, message: 'disk error' })),
      });
    } else if (copyBehavior === 'cancel') {
      jobs.set(id, { ...base, state: 'cancelled' });
    } else {
      for (const s of sources) {
        const dest = path.join(targetDir, path.basename(s));
        fs.copyFileSync(s, dest);
        // The Rust copy engine does not preserve mtime (FER-393): the copy is "now".
        fs.utimesSync(dest, Date.now() / 1000 + 100, Date.now() / 1000 + 100);
      }
      jobs.set(
        id,
        copyBehavior === 'partial'
          ? {
              ...base,
              state: 'failed',
              error: '1 failed',
              failures: [{ path: sources[0], message: 'locked' }],
            }
          : { ...base, state: 'completed' },
      );
    }
    return id;
  },
}));

vi.mock('./ipc/syncBaseline', async (importOriginal) => {
  const actual = await importOriginal<typeof import('./ipc/syncBaseline')>();
  return {
    ...actual,
    syncBaselineLoad: async (pair: { leftRoot: string; rightRoot: string }) => {
      calls.baselineLoad++;
      const file = storeFile(pair.leftRoot, pair.rightRoot);
      if (!fs.existsSync(file)) return { status: 'firstRun', reason: 'none' };
      try {
        const parsed = JSON.parse(fs.readFileSync(file, 'utf8')) as SyncBaseline;
        if (parsed.version !== 1) return { status: 'firstRun', reason: 'unknownVersion' };
        // Like the Rust store: identity is proven by the pair key, roots echo the request.
        return {
          status: 'ok',
          baseline: { ...parsed, leftRoot: pair.leftRoot, rightRoot: pair.rightRoot },
        };
      } catch {
        return { status: 'firstRun', reason: 'corrupt' };
      }
    },
    syncBaselineSave: async (pair: { leftRoot: string; rightRoot: string }, b: SyncBaseline) => {
      calls.baselineSave++;
      if (saveLimit) throw new actual.SyncBaselineError(saveLimit, 'refused');
      const file = storeFile(pair.leftRoot, pair.rightRoot);
      fs.writeFileSync(file + '.tmp', JSON.stringify(b));
      fs.renameSync(file + '.tmp', file);
    },
    syncBaselineClear: async () => {},
  };
});

const { buildSyncDryRun, runSyncPlan, settleSyncRun, mapJobOutcomes, waitForJobs } =
  await import('./syncService');
const { applyDecisions } = await import('./syncPlan');
type Decisions = Parameters<typeof applyDecisions>[1];

// ── fixture helpers ──────────────────────────────────────────────────────
let seq = 0;
function pairDirs(): { l: string; r: string } {
  const base = path.join(root, `case-${++seq}`);
  const l = path.join(base, 'left');
  const r = path.join(base, 'right');
  fs.mkdirSync(l, { recursive: true });
  fs.mkdirSync(r, { recursive: true });
  return { l, r };
}

function put(dir: string, name: string, body: string, sec = T0): void {
  const full = path.join(dir, name);
  fs.mkdirSync(path.dirname(full), { recursive: true });
  fs.writeFileSync(full, body);
  fs.utimesSync(full, sec, sec);
}

function readJob(id: string): OperationProgress | undefined {
  return jobs.get(id);
}

async function syncOnce(l: string, r: string, decisions: Decisions = {}) {
  const dry = await buildSyncDryRun(l, r, { direction: 'both' });
  const plan = applyDecisions(dry.plan, decisions);
  const result = await runSyncPlan(dry.report, plan, { allowOverwrite: true });
  if (result.status !== 'queued') throw new Error(`run not queued: ${result.status}`);
  const settle = await settleSyncRun({
    report: dry.report,
    plan,
    result,
    readJob,
    wait: { timeoutMs: 50, pollMs: 5 },
  });
  return { dry, plan, result, settle };
}

function actions(plan: { name: string; action: string; reason?: string }[]) {
  return Object.fromEntries(
    plan.map((p) => [p.name, p.reason ? `${p.action}:${p.reason}` : p.action]),
  );
}

function storedBytes(l: string, r: string): string | null {
  const f = storeFile(l, r);
  return fs.existsSync(f) ? fs.readFileSync(f, 'utf8') : null;
}

/** Union fixture already synced once: a.txt (both), l.txt (left), r.txt (right). */
async function syncedPair() {
  const { l, r } = pairDirs();
  put(l, 'a.txt', 'same');
  put(r, 'a.txt', 'same');
  put(l, 'l.txt', 'from left');
  put(r, 'r.txt', 'from right');
  const first = await syncOnce(l, r);
  return { l, r, first };
}

beforeEach(() => {
  calls.validate.length = 0;
  calls.compare = 0;
  calls.baselineLoad = 0;
  calls.baselineSave = 0;
  copyBehavior = 'ok';
  saveLimit = null;
  jobs.clear();
});

afterAll(() => {
  fs.rmSync(root, { recursive: true, force: true });
});

// ── tests ────────────────────────────────────────────────────────────────

describe('first run (no stored baseline)', () => {
  it('unions both sides without deleting and without a baseline moving until verified', async () => {
    const { l, r } = pairDirs();
    put(l, 'a.txt', 'same');
    put(r, 'a.txt', 'same');
    put(l, 'l.txt', 'from left');
    put(r, 'r.txt', 'from right');
    expect(storedBytes(l, r)).toBeNull();

    const { dry, result, settle } = await syncOnce(l, r);
    expect(dry.baselineStatus).toBe('none');
    expect(actions(dry.plan)).toEqual({
      'a.txt': 'skip',
      'l.txt': 'copyToRight',
      'r.txt': 'copyToLeft',
    });
    expect(result.status === 'queued' && result.deletes).toEqual([]);
    // Real files converged.
    expect(fs.readFileSync(path.join(r, 'l.txt'), 'utf8')).toBe('from left');
    expect(fs.readFileSync(path.join(l, 'r.txt'), 'utf8')).toBe('from right');
    expect(settle).toEqual({ status: 'saved', entries: 3 });
  });

  it('without a baseline a differing file is a conflict, never a timestamp winner', async () => {
    const { l, r } = pairDirs();
    put(l, 'x.txt', 'left newer', T0 + 9000);
    put(r, 'x.txt', 'right older', T0);
    const dry = await buildSyncDryRun(l, r, { direction: 'both' });
    expect(actions(dry.plan)).toEqual({ 'x.txt': 'conflict:noBaseline' });
  });
});

describe('restart keeps the baseline (reloaded from disk)', () => {
  it('a second run sees the saved baseline and plans nothing for unchanged files', async () => {
    const { l, r } = await syncedPair();
    const second = await buildSyncDryRun(l, r, { direction: 'both' });
    expect(second.baselineStatus).toBe('trusted');
    // Copies got fresh mtimes (no mtime preservation), yet nothing changed since verified-same.
    expect(Object.values(actions(second.plan)).every((a) => a === 'skip')).toBe(true);
  });

  it('is found through an alias spelling of the same pair', async () => {
    const { l, r } = await syncedPair();
    const alias = path.join(l, '..', 'left');
    const dry = await buildSyncDryRun(alias, r, { direction: 'both' });
    expect(dry.baselineStatus).toBe('trusted');
  });
});

describe('with a baseline: both-changed, modify-vs-delete, no-delete default', () => {
  it('both sides changed => conflict bothChanged', async () => {
    const { l, r } = await syncedPair();
    put(l, 'a.txt', 'left edit!', T0 + 5000);
    put(r, 'a.txt', 'right edit', T0 + 6000);
    const dry = await buildSyncDryRun(l, r, { direction: 'both' });
    expect(actions(dry.plan)['a.txt']).toBe('conflict:bothChanged');
  });

  it('only one side changed => that side wins (update), the other is untouched', async () => {
    const { l, r } = await syncedPair();
    put(l, 'a.txt', 'left edit and longer', T0 + 5000);
    const dry = await buildSyncDryRun(l, r, { direction: 'both' });
    expect(actions(dry.plan)['a.txt']).toBe('updateRight');
  });

  it('modified on one side, deleted on the other => conflict modifyDelete', async () => {
    const { l, r } = await syncedPair();
    put(l, 'a.txt', 'left modified later', T0 + 5000);
    fs.rmSync(path.join(r, 'a.txt'));
    const dry = await buildSyncDryRun(l, r, { direction: 'both' });
    expect(actions(dry.plan)['a.txt']).toBe('conflict:modifyDelete');
  });

  it('deleted on one side only => no delete is planned (deletedRight conflict)', async () => {
    const { l, r } = await syncedPair();
    fs.rmSync(path.join(r, 'l.txt'));
    const { dry, result } = await syncOnce(l, r);
    expect(actions(dry.plan)['l.txt']).toBe('conflict:deletedRight');
    expect(result.status === 'queued' && result.deletes).toEqual([]);
    expect(fs.existsSync(path.join(l, 'l.txt'))).toBe(true); // survivor kept
    expect(fs.existsSync(path.join(r, 'l.txt'))).toBe(false); // and not resurrected silently
  });
});

describe('baseline does not move for failed work (H3)', () => {
  async function runWith(behavior: typeof copyBehavior) {
    const { l, r } = await syncedPair();
    const before = storedBytes(l, r);
    put(l, 'a.txt', 'left edit and longer', T0 + 5000);
    put(l, 'new.txt', 'brand new');
    copyBehavior = behavior;
    const out = await syncOnce(l, r);
    return { l, r, before, out };
  }

  it('failed job: baseline file is byte-identical', async () => {
    const { l, r, before, out } = await runWith('fail');
    expect(storedBytes(l, r)).toBe(before);
    expect(out.settle.status).toBe('unchanged');
  });

  it('cancelled job: baseline file is byte-identical', async () => {
    const { l, r, before } = await runWith('cancel');
    expect(storedBytes(l, r)).toBe(before);
  });

  it('partial job (completed copies but a failure listed): nothing of it is recorded', async () => {
    const { l, r, before } = await runWith('partial');
    expect(storedBytes(l, r)).toBe(before);
  });

  it('job that never finishes: nothing is saved, honest skip', async () => {
    const { l, r } = await syncedPair();
    const before = storedBytes(l, r);
    put(l, 'new.txt', 'brand new');
    copyBehavior = 'running';
    const { settle } = await syncOnce(l, r);
    expect(settle).toEqual({ status: 'skipped', reason: 'jobsNotFinished' });
    expect(storedBytes(l, r)).toBe(before);
  });

  it('successful job: baseline moves for the finished items', async () => {
    const { l, r, before } = await runWith('ok');
    const after = storedBytes(l, r);
    expect(after).not.toBe(before);
    const parsed = JSON.parse(after!) as SyncBaseline;
    expect(Object.keys(parsed.entries).sort()).toEqual(['a.txt', 'l.txt', 'new.txt', 'r.txt']);
  });

  it('a refused save (limit) keeps the old baseline and reports it', async () => {
    const { l, r } = await syncedPair();
    const before = storedBytes(l, r);
    put(l, 'new.txt', 'brand new');
    saveLimit = 'limit_exceeded';
    const { settle } = await syncOnce(l, r);
    expect(settle).toEqual({ status: 'saveFailed', code: 'limit_exceeded' });
    expect(storedBytes(l, r)).toBe(before);
  });
});

describe('conflict decisions only move the baseline for items that finished (H4)', () => {
  async function bothChanged() {
    const { l, r } = await syncedPair();
    put(l, 'a.txt', 'left edit!', T0 + 5000);
    put(r, 'a.txt', 'right edit', T0 + 6000);
    return { l, r, before: storedBytes(l, r) };
  }

  it('keepLeft + job ok: right is overwritten and the entry is re-based', async () => {
    const { l, r, before } = await bothChanged();
    const { dry, settle } = await syncOnce(l, r, { 'a.txt': 'keepLeft' });
    expect(actions(dry.plan)['a.txt']).toBe('conflict:bothChanged');
    expect(fs.readFileSync(path.join(r, 'a.txt'), 'utf8')).toBe('left edit!');
    expect(settle.status).toBe('saved');
    expect(storedBytes(l, r)).not.toBe(before);
  });

  it('keepLeft + job failed: entry and file stay as they were', async () => {
    const { l, r, before } = await bothChanged();
    copyBehavior = 'fail';
    await syncOnce(l, r, { 'a.txt': 'keepLeft' });
    expect(storedBytes(l, r)).toBe(before);
    expect(fs.readFileSync(path.join(r, 'a.txt'), 'utf8')).toBe('right edit');
  });

  it('skip decision and undecided conflicts do not move the baseline', async () => {
    const { l, r, before } = await bothChanged();
    await syncOnce(l, r, { 'a.txt': 'skip' });
    expect(storedBytes(l, r)).toBe(before);
    await syncOnce(l, r);
    expect(storedBytes(l, r)).toBe(before);
  });
});

describe('corrupt / unknown baseline => first-run', () => {
  it('garbage on disk behaves as first-run (conflict), not a guess', async () => {
    const { l, r } = await syncedPair();
    fs.writeFileSync(storeFile(l, r), '{not json');
    put(l, 'a.txt', 'left edit and longer', T0 + 5000);
    const dry = await buildSyncDryRun(l, r, { direction: 'both' });
    expect(dry.baselineStatus).toBe('corrupt');
    expect(actions(dry.plan)['a.txt']).toBe('conflict:noBaseline');
  });

  it('unknown version is first-run too', async () => {
    const { l, r } = await syncedPair();
    const parsed = JSON.parse(storedBytes(l, r)!) as SyncBaseline;
    fs.writeFileSync(storeFile(l, r), JSON.stringify({ ...parsed, version: 99 }));
    const dry = await buildSyncDryRun(l, r, { direction: 'both' });
    expect(dry.baselineStatus).toBe('unknownVersion');
  });

  it('swapped panels are another pair: first-run', async () => {
    const { l, r } = await syncedPair();
    const dry = await buildSyncDryRun(r, l, { direction: 'both' });
    expect(dry.baselineStatus).toBe('none');
  });
});

describe('two root pairs do not affect each other', () => {
  it('syncing pair B leaves the stored baseline of pair A byte-identical', async () => {
    const a = await syncedPair();
    const snapshotA = storedBytes(a.l, a.r);
    const b = await syncedPair();
    put(b.l, 'extra.txt', 'only in B');
    await syncOnce(b.l, b.r);
    expect(storedBytes(a.l, a.r)).toBe(snapshotA);
    expect(storedBytes(b.l, b.r)).not.toBeNull();
  });
});

describe('root validation guards every path (H1)', () => {
  it('nested / same roots are rejected before compare, load or save', async () => {
    const { l } = pairDirs();
    const inner = path.join(l, 'inner');
    fs.mkdirSync(inner);
    await expect(buildSyncDryRun(l, inner, { direction: 'both' })).rejects.toThrow(/nested/);
    await expect(buildSyncDryRun(l, l, { direction: 'both' })).rejects.toThrow();
    expect(calls.compare).toBe(0);
    expect(calls.baselineLoad).toBe(0);
    expect(calls.baselineSave).toBe(0);
  });

  it('dry-run, run revalidation and the settle compare each validate the roots', async () => {
    const { l, r } = pairDirs();
    put(l, 'a.txt', 'x');
    await syncOnce(l, r);
    // dry-run + run (fresh compare) + settle (fresh compare)
    expect(calls.validate.length).toBeGreaterThanOrEqual(3);
    expect(calls.validate.every(([a, b]) => a === l && b === r)).toBe(true);
  });

  it('roots that stop validating before settle: nothing is saved', async () => {
    const { l, r } = pairDirs();
    put(l, 'a.txt', 'x');
    const dry = await buildSyncDryRun(l, r, { direction: 'both' });
    const result = await runSyncPlan(dry.report, dry.plan, {});
    if (result.status !== 'queued') throw new Error('expected queued');
    // Make the pair nested by settling with a report pointing at nested roots.
    const nested = { ...dry.report, rightRoot: path.join(l, 'inner') };
    fs.mkdirSync(nested.rightRoot, { recursive: true });
    const before = calls.baselineSave;
    const settle = await settleSyncRun({ report: nested, plan: dry.plan, result, readJob });
    expect(settle).toEqual({ status: 'skipped', reason: 'verifyFailed' });
    expect(calls.baselineSave).toBe(before);
  });
});

describe('mapJobOutcomes / waitForJobs', () => {
  const item = (name: string, jobId?: string) => ({
    name,
    action: 'copyToRight' as const,
    status: 'queued' as const,
    jobId,
  });
  const job = (state: OperationProgress['state'], extra: Partial<OperationProgress> = {}) =>
    ({
      operationId: 'j',
      kind: 'copy',
      state,
      processedBytes: 0,
      processedFiles: 0,
      ...extra,
    }) as OperationProgress;

  it('maps terminal states conservatively', () => {
    const out = mapJobOutcomes(
      [item('ok', 'a'), item('fail', 'b'), item('cancel', 'c'), item('run', 'd'), item('none')],
      {
        a: job('completed'),
        b: job('failed'),
        c: job('cancelled'),
        d: job('running'),
      },
    );
    expect(out).toEqual({ ok: 'ok', fail: 'failed', cancel: 'cancelled' });
  });

  it('a completed job that still lists failures is partial => failed', () => {
    const out = mapJobOutcomes([item('x', 'a')], {
      a: job('completed', { failures: [{ path: 'p', message: 'm' }] }),
    });
    expect(out).toEqual({ x: 'failed' });
  });

  it('only queued items carry outcomes', () => {
    const out = mapJobOutcomes([{ name: 's', action: 'skip', status: 'skipped', jobId: 'a' }], {
      a: job('completed'),
    });
    expect(out).toEqual({});
  });

  it('waitForJobs returns when jobs finish and gives up at the deadline', async () => {
    let t = 0;
    const reads = [job('running'), job('running'), job('completed')];
    let i = 0;
    const done = await waitForJobs(['a'], () => reads[Math.min(i++, 2)], {
      sleep: async () => {
        t += 10;
      },
      now: () => t,
      timeoutMs: 1000,
    });
    expect(done.a?.state).toBe('completed');
    t = 0;
    const stuck = await waitForJobs(['a'], () => job('running'), {
      sleep: async () => {
        t += 400;
      },
      now: () => t,
      timeoutMs: 1000,
    });
    expect(stuck.a?.state).toBe('running');
  });
});

describe('journal marks in the stored baseline (FER-477b)', () => {
  // Opt-in (FER-477 review): only with the setting on does sync ask for marks.
  beforeEach(() => {
    ui.syncJournalSkip = true;
  });
  afterAll(() => {
    ui.syncJournalSkip = false;
  });

  it('with the setting off (default) no marks are asked for or stored', async () => {
    ui.syncJournalSkip = false;
    const { l, r } = await syncedPair();
    const stored = JSON.parse(storedBytes(l, r)!) as SyncBaseline;
    expect(stored.journal).toBeUndefined();
  });

  it('a fully clean run stores the marks of its settle compare', async () => {
    const { l, r } = await syncedPair();
    const stored = JSON.parse(storedBytes(l, r)!) as SyncBaseline;
    expect(stored.journal?.left.journalId).toBe('7');
    expect(stored.journal?.right.journalId).toBe('8');
  });

  it('a run that leaves a difference never stores marks with its entries', async () => {
    const { l, r } = await syncedPair();
    // An unresolved conflict stays different; the new file is copied and recorded.
    put(l, 'a.txt', 'left edit', T0 + 50);
    put(r, 'a.txt', 'right edit!!', T0 + 60);
    put(l, 'new.txt', 'only left');
    const { settle } = await syncOnce(l, r);
    expect(settle.status).toBe('saved');
    const stored = JSON.parse(storedBytes(l, r)!) as SyncBaseline;
    expect(Object.keys(stored.entries)).toContain('new.txt');
    expect(stored.journal).toBeUndefined();
  });

  it('a failed run that moves nothing keeps the stored file (and its marks) byte-identical', async () => {
    const { l, r } = await syncedPair();
    const before = storedBytes(l, r);
    put(l, 'new.txt', 'only left');
    copyBehavior = 'fail';
    await syncOnce(l, r);
    expect(storedBytes(l, r)).toBe(before);
  });
});
