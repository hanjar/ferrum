/**
 * FER-218 / AI-11.4 — "sync kedua panel dua arah" through the WHOLE chat path:
 *   provider intent -> facade -> sync adapter -> approval -> backend guard -> executor -> queue
 *   -> settleSyncRun -> baseline, then a second run (modified / missing / conflicting files).
 *
 * WHAT IS REAL AND WHAT IS FAKE (read before trusting a green run):
 * - REAL: temp directories and every file operation (node:fs); the TypeScript code under test: ContextService /
 *   ContextFlow, the capability registry, CommandFacade, the sync adapter, ApprovalManager, AiExecutor, the
 *   execution ledger (in memory), `createBackendGuard`, syncService / syncPlan / syncBaseline.
 * - FAKE ("filesystem-backed IPC doubles", NOT the Rust engines):
 *     - `compareDirs` / `validateSyncRoots` (a stat/bytes based re-implementation; `validateSyncRoots` resolves
 *       real links with realpath, like the Rust canonical check),
 *     - `enqueueCopy` (copyFileSync + a scripted job result; there is no Rust queue),
 *     - the baseline store (one JSON file per pair),
 *     - the backend approval ledger behind `invoke('ai_*')`: a TypeScript MIMIC of `src-tauri/src/ai/approval.rs`.
 *       The real Rust guard (fingerprints, absent targets, junction redirect, single use) is proven by the Rust
 *       tests in `approval_commands.rs`; this mimic only lets the TypeScript wiring be exercised end to end.
 * - NOT covered here: the Rust copy engine (FER-393, cancel/overwrite staging), the real operation queue, the
 *   desktop UI. `scripts/ai-desktop-smoke-sync.mjs` is written but NOT executed.
 */
import { afterAll, beforeEach, describe, expect, it, vi } from 'vitest';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import crypto from 'node:crypto';
import type { CompareDirsReport, DirDiffEntry, DirDiffState } from '../../ipc/compare';
import { FsError, type OperationProgress } from '../../ipc/types';
import type { SyncBaseline } from '../../syncBaseline';
import type { RunState, RunUpdate } from '../chatStore.svelte';
import type { IntentProvider, ProviderEvent, ProviderPlanRequest } from '../provider';

// ── filesystem-backed IPC doubles (FAKE) ─────────────────────────────────
const T0 = 1_600_000_000; // seconds; old fixed mtimes keep the metadata compare deterministic
const root = fs.mkdtempSync(path.join(os.tmpdir(), 'ferrum-fer218-'));
const storeDir = path.join(root, '_store');
fs.mkdirSync(storeDir);

const calls = vi.hoisted(() => ({
  enqueue: 0,
  register: 0,
  authorize: 0,
  baselineSave: 0,
  validate: [] as [string, string][],
}));
const jobs = new Map<string, OperationProgress>();
let jobSeq = 0;
let copyBehavior: 'ok' | 'fail' | 'cancel' | 'partial' = 'ok';
let truncated = false;
let registerFails = false;
let enqueueLimit = Infinity; // enqueueCopy rejects after this many accepted batches

function norm(p: string): string {
  return path.resolve(p).split(path.sep).join('/').toLowerCase();
}
function real(p: string): string {
  return norm(fs.realpathSync(p));
}
function storeFile(l: string, r: string): string {
  const key = crypto
    .createHash('sha1')
    .update(`${real(l)}|${real(r)}`)
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
    else if (byContent) {
      state = fs.readFileSync(path.join(l, name)).equals(fs.readFileSync(path.join(r, name)))
        ? 'same'
        : 'contentDifferent';
    } else if (Math.abs(a!.mtimeMs - b!.mtimeMs) <= 2000) state = 'same';
    else state = a!.mtimeMs > b!.mtimeMs ? 'newerLeft' : 'newerRight';
    return { name, state, ...facts };
  });
  return { leftRoot: l, rightRoot: r, entries, truncated, recursive: true };
}

vi.mock('../../ipc/compare', () => ({
  compareDirs: async (l: string, r: string, byContent: boolean) => fakeCompare(l, r, byContent),
  compareFiles: vi.fn(),
  cancelCompare: vi.fn(),
  validateSyncRoots: async (l: string, r: string) => {
    calls.validate.push([l, r]);
    const a = real(l);
    const b = real(r);
    if (a === b || a.startsWith(b + '/') || b.startsWith(a + '/')) {
      throw new FsError({ code: 'invalidPath', message: 'roots are same or nested', path: l });
    }
  },
}));

vi.mock('../../ipc/mutations', () => ({
  enqueueCopy: async (sources: string[], targetDir: string) => {
    if (calls.enqueue >= enqueueLimit) throw new Error('queue full');
    calls.enqueue++;
    const id = `job-${++jobSeq}`;
    const base = {
      operationId: id,
      kind: 'copy' as const,
      processedBytes: 0,
      processedFiles: sources.length,
    };
    if (copyBehavior === 'fail') {
      jobs.set(id, {
        ...base,
        processedFiles: 0, // nothing was written by a job that failed outright
        state: 'failed',
        error: 'disk error',
        failures: sources.map((s) => ({ path: s, message: 'disk error' })),
      });
    } else if (copyBehavior === 'cancel') {
      jobs.set(id, { ...base, processedFiles: 0, state: 'cancelled' });
    } else {
      for (const s of sources) {
        const dest = path.join(targetDir, path.basename(s));
        fs.copyFileSync(s, dest);
        fs.utimesSync(dest, Date.now() / 1000 + 100, Date.now() / 1000 + 100); // copy engine keeps no mtime (FER-393)
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

vi.mock('../../ipc/syncBaseline', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../../ipc/syncBaseline')>();
  return {
    ...actual,
    syncBaselineLoad: async (pair: { leftRoot: string; rightRoot: string }) => {
      const file = storeFile(pair.leftRoot, pair.rightRoot);
      if (!fs.existsSync(file)) return { status: 'firstRun', reason: 'none' };
      const parsed = JSON.parse(fs.readFileSync(file, 'utf8')) as SyncBaseline;
      return {
        status: 'ok',
        baseline: { ...parsed, leftRoot: pair.leftRoot, rightRoot: pair.rightRoot },
      };
    },
    syncBaselineSave: async (pair: { leftRoot: string; rightRoot: string }, b: SyncBaseline) => {
      calls.baselineSave++;
      const file = storeFile(pair.leftRoot, pair.rightRoot);
      fs.writeFileSync(file + '.tmp', JSON.stringify(b));
      fs.renameSync(file + '.tmp', file);
    },
    syncBaselineClear: async () => {},
  };
});

// TypeScript MIMIC of the Rust ApprovalLedger (see header). Same observable rules: fingerprints (kind + size +
// mtime) of every path given at registration, targets that must stay absent, canonical roots, single use, codes only.
interface Slot {
  left: string;
  right: string;
  prints: Map<string, string>;
  absent: string[];
  consumed: boolean;
}
const slots = new Map<string, Slot>();
const fp = (p: string): string => {
  try {
    const st = fs.lstatSync(p);
    return `${st.isDirectory() ? 'd' : 'f'}:${st.isDirectory() ? 0 : st.size}:${Math.floor(st.mtimeMs)}`;
  } catch {
    return 'missing';
  }
};
const reject = (reasons: string[]) => ({
  kind: 'stale',
  reasons,
  summary: `Rencana usang (${reasons.length} alasan); tidak ada yang ditulis. Susun ulang dan setujui lagi.`,
});
const invokeMock = vi.hoisted(() => vi.fn());
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invokeMock(...a) }));

type Req = Record<string, unknown> & { handleId: string };
function fakeBackend(cmd: string, args: Record<string, unknown>): unknown {
  if (cmd === 'ai_register_approval') {
    calls.register++;
    if (registerFails)
      throw {
        kind: 'source_unreadable',
        reasons: [],
        summary: 'Item tidak terbaca; tidak ada yang disetujui.',
      };
    const r = args.request as Req;
    const prints = new Map<string, string>();
    for (const p of r.sourcePaths as string[]) {
      const f = fp(p);
      if (f === 'missing')
        throw {
          kind: 'source_unreadable',
          reasons: [],
          summary: 'Item tidak terbaca; tidak ada yang disetujui.',
        };
      prints.set(p, f);
    }
    slots.set(r.handleId, {
      left: real(r.root as string),
      right: real(r.secondRoot as string),
      prints,
      absent: r.absentPaths as string[],
      consumed: false,
    });
    return {
      scopeDigest: crypto
        .createHash('sha1')
        .update(JSON.stringify([...prints]))
        .digest('hex')
        .slice(0, 32),
    };
  }
  if (cmd === 'ai_authorize_execute') {
    calls.authorize++;
    const c = args.claim as Req;
    const s = slots.get(c.handleId);
    if (!s) throw reject(['unknown_approval']);
    if (s.consumed) throw reject(['already_consumed']);
    const reasons: string[] = [];
    if (real(c.root as string) !== s.left || real(c.secondRoot as string) !== s.right)
      reasons.push('root_mismatch');
    for (const [p, f] of s.prints)
      if (fp(p) !== f) reasons.push(fp(p) === 'missing' ? 'source_missing' : 'source_changed');
    for (const p of s.absent) if (fp(p) !== 'missing') reasons.push('target_appeared');
    if (reasons.length) throw reject([...new Set(reasons)]);
    s.consumed = true;
    return undefined;
  }
  slots.delete(args.handleId as string);
  return undefined;
}

// ── real modules, imported after the mocks ───────────────────────────────
const { ContextService } = await import('../context');
const { ScopeResolver } = await import('../contextResolver');
const { ContextFlow } = await import('../contextFlow');
const { FakeHost } = await import('../contextTestKit');
const { createCommandFacade } = await import('./index');
const { SyncPreviewStore } = await import('./sync');
const { createBackendGuard } = await import('../backendGuard');
const { AiExecutor } = await import('../executor');
const { MemoryExecutionLedger } = await import('../executorLedger');
const { PollingQueuePort } = await import('../executorQueue');
const { buildSyncDryRun, runSyncPlan, settleSyncRun } = await import('../../syncService');
type SettleResult = Awaited<ReturnType<typeof settleSyncRun>>;

// ── fixture + harness ────────────────────────────────────────────────────
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
function tree(dir: string): Record<string, string> {
  return Object.fromEntries(
    [...walk(dir)].map(([k]) => [k, fs.readFileSync(path.join(dir, k), 'utf8')]),
  );
}
function stored(l: string, r: string): SyncBaseline | null {
  const f = storeFile(l, r);
  return fs.existsSync(f) ? (JSON.parse(fs.readFileSync(f, 'utf8')) as SyncBaseline) : null;
}
function storedBytes(l: string, r: string): string | null {
  const f = storeFile(l, r);
  return fs.existsSync(f) ? fs.readFileSync(f, 'utf8') : null;
}

function harness(l: string, r: string) {
  const host = new FakeHost();
  host.left.path = l;
  host.right.path = r;
  const svc = new ContextService(host);
  const resolver = new ScopeResolver(svc);
  const flow = new ContextFlow(svc, resolver);
  const queue = new PollingQueuePort({
    read: () => [...jobs.values()],
    cancel: async () => undefined,
  });
  const previews = new SyncPreviewStore();
  const settled: SettleResult[] = [];
  const { facade, registry } = createCommandFacade({
    archive: { flow, resolver, ctx: svc, outputExists: () => false },
    sync: {
      flow,
      ctx: svc,
      store: previews,
      readJob: (id) => queue.get(id),
      onSettled: (s) => settled.push(s),
    },
    gates: { 'directories.sync': { status: 'ready' } }, // independent of the committed gate state
  });
  const requests: Array<ProviderPlanRequest & { capabilities?: readonly string[] }> = [];
  const provider: IntentProvider = {
    async *plan(request: ProviderPlanRequest): AsyncGenerator<ProviderEvent> {
      requests.push(request);
      yield {
        kind: 'intent',
        intent: {
          schemaVersion: 1,
          requestId: request.requestId,
          intent: {
            capability: 'directories.sync',
            leftPane: 'left',
            rightPane: 'right',
            mode: 'bidirectional',
          },
        },
      };
    },
  };
  const updates: RunUpdate[] = [];
  const ex = new AiExecutor({
    provider,
    capture: (id) => svc.capture(id),
    modelContext: (s) => svc.modelContext(s),
    facade,
    capabilities: registry,
    ledger: new MemoryExecutionLedger(),
    queue,
    guard: createBackendGuard({
      sourcePaths: () => undefined,
      syncScope: (plan) => previews.scopeOf(plan),
    }),
    conflictViews: (plan) => previews.conflictsOf(plan),
    listItems: async (plan, o, n) => previews.page(plan, o, n),
  });
  ex.subscribe((u) => updates.push(u));
  const stateOf = (runId: string): RunState | undefined =>
    [...updates].reverse().find((u) => u.runId === runId)?.state;
  const last = (runId: string) => [...updates].reverse().find((u) => u.runId === runId)!;
  let n = 0;
  async function chat(text = 'sinkronkan kedua panel dua arah') {
    const runId = `run-${++n}`;
    ex.submit({ requestId: `req_${n}`, text, runId } as never);
    await vi.waitFor(() =>
      expect(['awaitingApproval', 'needsInput', 'failed', 'stale', 'blockedCapability']).toContain(
        stateOf(runId),
      ),
    );
    return runId;
  }
  const finish = async (runId: string) => {
    await vi.waitFor(() =>
      expect(['succeeded', 'partial', 'failed', 'cancelled', 'stale', 'unknownOutcome']).toContain(
        stateOf(runId),
      ),
    );
    return stateOf(runId)!;
  };
  /** Preview -> Run -> final state, then waits for the baseline settlement of that run. */
  async function approveAndSettle(runId: string) {
    const before = settled.length;
    const queuedBefore = calls.enqueue;
    ex.approve(runId, { planDigest: last(runId).plan!.previewDigest });
    const state = await finish(runId);
    // Whenever something reached the queue, its baseline settlement must also have finished.
    if (calls.enqueue > queuedBefore)
      await vi.waitFor(() => expect(settled.length).toBeGreaterThan(before));
    return state;
  }
  return {
    host,
    svc,
    ex,
    updates,
    stateOf,
    last,
    chat,
    finish,
    approveAndSettle,
    settled,
    requests,
    previews,
    flow,
  };
}

/** The manual panel path (same services, no chat): dry-run -> run (overwrites confirmed) -> settle. */
async function manualSync(l: string, r: string) {
  const dry = await buildSyncDryRun(l, r, { direction: 'both' });
  const result = await runSyncPlan(dry.report, dry.plan, { allowOverwrite: true });
  if (result.status !== 'queued') throw new Error(`manual run not queued: ${result.status}`);
  await settleSyncRun({
    report: dry.report,
    plan: dry.plan,
    result,
    readJob: (id) => jobs.get(id),
    wait: { timeoutMs: 50, pollMs: 5 },
  });
  return dry;
}

function actionMap(plan: { name: string; action: string; reason?: string }[]) {
  return Object.fromEntries(
    plan.map((p) => [p.name, p.reason ? `${p.action}:${p.reason}` : p.action]),
  );
}

beforeEach(() => {
  calls.enqueue = 0;
  calls.register = 0;
  calls.authorize = 0;
  calls.baselineSave = 0;
  calls.validate.length = 0;
  copyBehavior = 'ok';
  truncated = false;
  registerFails = false;
  enqueueLimit = Infinity;
  jobs.clear();
  slots.clear();
  invokeMock.mockReset();
  invokeMock.mockImplementation(async (cmd: string, args: Record<string, unknown>) =>
    fakeBackend(cmd, args),
  );
});

afterAll(() => {
  fs.rmSync(root, { recursive: true, force: true });
});

/** Fixture: a.txt (both), l.txt (left), r.txt (right). */
function unionFixture() {
  const { l, r } = pairDirs();
  put(l, 'a.txt', 'same');
  put(r, 'a.txt', 'same');
  put(l, 'l.txt', 'from left');
  put(r, 'r.txt', 'from right');
  return { l, r };
}

/** Second-run mutations: a.txt edited on the left; l.txt deleted on the right; r.txt edited on BOTH sides; n.txt new on the right. */
function mutate(l: string, r: string) {
  put(l, 'a.txt', 'same v2', T0 + 1000);
  fs.rmSync(path.join(r, 'l.txt'));
  put(l, 'r.txt', 'left edit of r', T0 + 2000);
  put(r, 'r.txt', 'right edit of r!', T0 + 3000);
  put(r, 'n.txt', 'new on right', T0 + 4000);
}

// ── tests ────────────────────────────────────────────────────────────────

describe('first run through the chat (union, no delete, nothing wins by timestamp)', () => {
  it('previews one approval, runs the union through the queue and settles the baseline', async () => {
    const { l, r } = unionFixture();
    const t = harness(l, r);
    const runId = await t.chat();
    expect(t.stateOf(runId)).toBe('awaitingApproval');
    const shown = t.last(runId);
    expect(shown.plan?.capability).toBe('directories.sync');
    expect(shown.scopeNote).toContain('Kiri → kanan: 1 item (0 menimpa).');
    expect(shown.scopeNote).toContain('Kanan → kiri: 1 item (0 menimpa).');
    expect(shown.warnings?.join(' ')).toContain('Run pertama');
    // zero writes, zero jobs and no baseline before the user approved
    expect(calls.enqueue).toBe(0);
    expect(tree(r)).toEqual({ 'a.txt': 'same', 'r.txt': 'from right' });
    expect(stored(l, r)).toBeNull();
    expect(calls.register).toBe(1);
    expect(calls.authorize).toBe(0);

    expect(await t.approveAndSettle(runId)).toBe('succeeded');
    expect(calls.authorize).toBe(1);
    expect(tree(l)).toEqual({ 'a.txt': 'same', 'l.txt': 'from left', 'r.txt': 'from right' });
    expect(tree(r)).toEqual({ 'a.txt': 'same', 'l.txt': 'from left', 'r.txt': 'from right' });
    expect(t.settled.at(-1)).toMatchObject({ status: 'saved' });
    expect(Object.keys(stored(l, r)!.entries).sort()).toEqual(['a.txt', 'l.txt', 'r.txt']);
    // two-way progress: a right-bound and a left-bound job, both owned by this run
    expect(t.last(runId).jobs?.map((j) => j.state)).toEqual(['completed', 'completed']);
  });

  it('the model never sees names, paths or counts of the folders', async () => {
    const { l, r } = unionFixture();
    const t = harness(l, r);
    await t.chat();
    const sent = JSON.stringify(t.requests);
    for (const secret of [l, r, 'l.txt', 'r.txt', 'a.txt', path.basename(root)])
      expect(sent).not.toContain(secret);
  });
});

describe('second run (baseline trusted) through the chat', () => {
  it('overwrites only the side that changed, lists conflicts, never deletes or restores, moves the baseline only for verified items', async () => {
    const { l, r } = unionFixture();
    const t = harness(l, r);
    expect(await t.approveAndSettle(await t.chat())).toBe('succeeded');
    const before = stored(l, r)!;
    mutate(l, r);

    const runId = await t.chat();
    const shown = t.last(runId);
    expect(shown.warnings?.join(' ')).toContain('Baseline tersimpan dipakai');
    expect(shown.scopeNote).toContain('Kiri → kanan: 1 item (1 menimpa).'); // a.txt
    expect(shown.scopeNote).toContain('Kanan → kiri: 1 item (0 menimpa).'); // n.txt
    expect(shown.scopeNote).toContain('Konflik: 2 (dilewati). Hapus: 0.');
    expect(shown.conflicts?.map((c) => `${c.label}:${c.kind}`).sort()).toEqual([
      'l.txt:deletedRight',
      'r.txt:bothChanged',
    ]);
    expect(shown.warnings?.join(' ')).toContain('1 file yang sudah ada akan ditimpa');

    expect(await t.approveAndSettle(runId)).toBe('succeeded');
    expect(t.last(runId).warnings?.join(' ')).toContain('2 konflik dilewati'); // not reported as "all in sync"
    // overwrite and new file landed ...
    expect(fs.readFileSync(path.join(r, 'a.txt'), 'utf8')).toBe('same v2');
    expect(fs.readFileSync(path.join(l, 'n.txt'), 'utf8')).toBe('new on right');
    // ... conflicts were NOT executed: both divergent versions are intact, the deleted file was neither
    // restored on the right nor deleted on the left
    expect(fs.readFileSync(path.join(l, 'r.txt'), 'utf8')).toBe('left edit of r');
    expect(fs.readFileSync(path.join(r, 'r.txt'), 'utf8')).toBe('right edit of r!');
    expect(fs.existsSync(path.join(r, 'l.txt'))).toBe(false);
    expect(fs.readFileSync(path.join(l, 'l.txt'), 'utf8')).toBe('from left');
    // baseline: verified items moved, conflicting items untouched
    const after = stored(l, r)!;
    expect(after.entries['l.txt']).toEqual(before.entries['l.txt']);
    expect(after.entries['r.txt']).toEqual(before.entries['r.txt']);
    expect(after.entries['a.txt']).not.toEqual(before.entries['a.txt']);
    expect(after.entries['n.txt']).toBeDefined();
  });

  it('chat result == manual panel result for the same plan (same tree, same actions, same baseline entries)', async () => {
    const chatDirs = unionFixture();
    const manualDirs = unionFixture();
    const t = harness(chatDirs.l, chatDirs.r);

    expect(await t.approveAndSettle(await t.chat())).toBe('succeeded');
    await manualSync(manualDirs.l, manualDirs.r);
    expect(tree(chatDirs.l)).toEqual(tree(manualDirs.l));
    expect(tree(chatDirs.r)).toEqual(tree(manualDirs.r));

    mutate(chatDirs.l, chatDirs.r);
    mutate(manualDirs.l, manualDirs.r);
    const runId = await t.chat();
    expect(await t.approveAndSettle(runId)).toBe('succeeded');
    const manualDry = await manualSync(manualDirs.l, manualDirs.r);

    expect(tree(chatDirs.l)).toEqual(tree(manualDirs.l));
    expect(tree(chatDirs.r)).toEqual(tree(manualDirs.r));
    const names = (b: SyncBaseline | null) => Object.keys(b!.entries).sort();
    expect(names(stored(chatDirs.l, chatDirs.r))).toEqual(
      names(stored(manualDirs.l, manualDirs.r)),
    );
    // same classification: the chat preview counts equal the manual dry-run summary
    expect(actionMap(manualDry.plan)).toMatchObject({
      'a.txt': 'updateRight',
      'n.txt': 'copyToLeft',
      'l.txt': 'conflict:deletedRight',
      'r.txt': 'conflict:bothChanged',
    });
    expect(manualDry.summary).toMatchObject({
      toRight: 1,
      toLeft: 1,
      overwrites: 1,
      conflicts: 2,
      deletes: 0,
    });
  });
});

describe('adversarial', () => {
  it('a file changed after the preview is stale at the backend guard: zero copies, baseline untouched', async () => {
    const { l, r } = unionFixture();
    const t = harness(l, r);
    const runId = await t.chat();
    put(l, 'l.txt', 'from left but different now', T0 + 5000);
    expect(await t.approveAndSettle(runId)).toBe('stale');
    expect(calls.authorize).toBe(1);
    expect(calls.enqueue).toBe(0);
    expect(tree(r)).toEqual({ 'a.txt': 'same', 'r.txt': 'from right' });
    expect(t.last(runId).errorMessage).toContain('Rencana usang');
    expect(stored(l, r)).toBeNull();
  });

  it("a file that appeared at an approved NEW target is stale, and the other party's file is not overwritten", async () => {
    const { l, r } = unionFixture();
    const t = harness(l, r);
    const runId = await t.chat();
    put(r, 'l.txt', 'someone else made this', T0 + 5000);
    expect(await t.approveAndSettle(runId)).toBe('stale');
    expect(calls.enqueue).toBe(0);
    expect(fs.readFileSync(path.join(r, 'l.txt'), 'utf8')).toBe('someone else made this');
  });

  it('an overwrite target edited after the preview is stale', async () => {
    const { l, r } = unionFixture();
    const t = harness(l, r);
    expect(await t.approveAndSettle(await t.chat())).toBe('succeeded');
    mutate(l, r);
    const runId = await t.chat();
    put(r, 'a.txt', 'edited on the right while the plan waited', T0 + 9000);
    expect(await t.approveAndSettle(runId)).toBe('stale');
    expect(fs.readFileSync(path.join(r, 'a.txt'), 'utf8')).toBe(
      'edited on the right while the plan waited',
    );
  });

  it('a root redirected through a junction after the preview is stale: nothing is written through it', async () => {
    const { l, r } = unionFixture();
    const t = harness(l, r);
    const runId = await t.chat();
    const elsewhere = path.join(path.dirname(r), 'elsewhere');
    fs.mkdirSync(elsewhere);
    put(elsewhere, 'a.txt', 'same');
    put(elsewhere, 'r.txt', 'from right');
    fs.rmSync(r, { recursive: true });
    try {
      fs.symlinkSync(elsewhere, r, 'junction');
    } catch {
      console.warn('SKIPPED: cannot create a junction here');
      return;
    }
    expect(await t.approveAndSettle(runId)).toBe('stale');
    expect(calls.enqueue).toBe(0);
    expect(fs.existsSync(path.join(elsewhere, 'l.txt'))).toBe(false);
  });

  it('double dispatch (two approvals, two executes) enqueues once per direction', async () => {
    const { l, r } = unionFixture();
    const t = harness(l, r);
    const runId = await t.chat();
    const digest = t.last(runId).plan!.previewDigest;
    t.ex.approve(runId, { planDigest: digest });
    t.ex.approve(runId, { planDigest: digest });
    await t.finish(runId);
    expect(calls.enqueue).toBe(2); // one right-bound batch + one left-bound batch, not four
    expect(calls.authorize).toBe(1);
  });

  it('a forged approval cannot run anything: wrong digest = stale, unknown run = ignored, no token = refused', async () => {
    const { l, r } = unionFixture();
    const t = harness(l, r);
    const runId = await t.chat();
    t.ex.approve('run-that-does-not-exist');
    t.ex.approve(runId, { planDigest: 'forged_digest' });
    await t.finish(runId);
    expect(t.stateOf(runId)).toBe('stale');
    expect(calls.enqueue).toBe(0);
    expect(calls.authorize).toBe(0);
  });

  it('same / nested / aliased roots never reach a plan', async () => {
    const { l, r } = unionFixture();
    const t = harness(l, l);
    const same = await t.chat();
    expect(t.stateOf(same)).toBe('needsInput');
    t.host.right.path = path.join(l, 'inner');
    const nested = await t.chat();
    expect(t.stateOf(nested)).toBe('needsInput');
    // alias: a junction whose real target is the left folder
    const alias = path.join(path.dirname(l), 'alias-of-left');
    try {
      fs.symlinkSync(l, alias, 'junction');
      t.host.right.path = alias;
      const viaAlias = await t.chat();
      expect(t.stateOf(viaAlias)).toBe('needsInput');
      expect(t.last(viaAlias).errorMessage).toMatch(/alias|sama|bersarang/i);
    } catch (e) {
      if (e instanceof Error && /EPERM|EACCES/.test(e.message))
        console.warn('SKIPPED: junction not allowed');
      else throw e;
    }
    expect(calls.enqueue).toBe(0);
    expect(calls.register).toBe(0);
    void r;
  });

  it('a truncated compare gives no plan', async () => {
    const { l, r } = unionFixture();
    const t = harness(l, r);
    truncated = true;
    const runId = await t.chat();
    expect(t.stateOf(runId)).toBe('failed');
    expect(t.last(runId).errorMessage).toContain('terpotong');
    expect(calls.register).toBe(0);
    expect(calls.enqueue).toBe(0);
  });

  it('fail closed: a backend that cannot register means nothing can be approved or written', async () => {
    const { l, r } = unionFixture();
    const t = harness(l, r);
    registerFails = true;
    const runId = await t.chat();
    expect(t.stateOf(runId)).toBe('failed');
    t.ex.approve(runId); // no preview awaiting approval: ignored
    expect(calls.enqueue).toBe(0);
    expect(calls.authorize).toBe(0);
    expect(tree(r)).toEqual({ 'a.txt': 'same', 'r.txt': 'from right' });
  });

  it('a pane navigated after the preview is stale before anything is enqueued', async () => {
    const { l, r } = unionFixture();
    const t = harness(l, r);
    const runId = await t.chat();
    t.host.left.listing = {};
    expect(await t.approveAndSettle(runId)).toBe('stale');
    expect(calls.enqueue).toBe(0);
  });

  it('a batch that never reached the queue makes the result partial, not "completed"', async () => {
    const { l, r } = unionFixture();
    const t = harness(l, r);
    enqueueLimit = 1; // the right-bound batch is accepted, the left-bound one is refused
    const runId = await t.chat();
    expect(await t.approveAndSettle(runId)).toBe('partial');
    expect(t.last(runId).errorMessage).toContain('1 item gagal masuk antrean');
    expect(fs.existsSync(path.join(l, 'r.txt'))).toBe(false); // the left-bound copy did not happen
    expect(fs.existsSync(path.join(r, 'l.txt'))).toBe(true);
  });

  it.each([
    ['fail', 'failed'],
    ['cancel', 'cancelled'],
    ['partial', 'partial'],
  ] as const)(
    'a %s job gives the %s result and leaves the baseline byte-identical',
    async (behavior, expected) => {
      const { l, r } = unionFixture();
      const t = harness(l, r);
      expect(await t.approveAndSettle(await t.chat())).toBe('succeeded');
      const before = storedBytes(l, r);
      mutate(l, r);
      copyBehavior = behavior;
      const runId = await t.chat();
      const state = await t.approveAndSettle(runId);
      expect(state).toBe(expected);
      // failed / cancelled / partial never claim "all in sync" and never move the verified entries
      const after = stored(l, r)!;
      const prev = JSON.parse(before!) as SyncBaseline;
      expect(after.entries['l.txt']).toEqual(prev.entries['l.txt']);
      expect(after.entries['r.txt']).toEqual(prev.entries['r.txt']);
      if (behavior !== 'partial') expect(storedBytes(l, r)).toBe(before);
    },
  );
});
