import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { CompareDirsReport, DirDiffEntry, DirDiffState } from './ipc/compare';
import { ui } from './uiState.svelte';

const compareDirsMock = vi.hoisted(() => vi.fn());
const enqueueCopyMock = vi.hoisted(() => vi.fn());
const validateRootsMock = vi.hoisted(() =>
  vi.fn<(left: string, right: string) => Promise<void>>(async () => {}),
);
vi.mock('./ipc/compare', () => ({
  compareDirs: compareDirsMock,
  compareFiles: vi.fn(),
  validateSyncRoots: validateRootsMock,
}));
vi.mock('./ipc/mutations', () => ({ enqueueCopy: enqueueCopyMock }));

const { compareForSync, buildSyncDryRun, runSyncPlan, settleSyncRun } =
  await import('./syncService');

function diff(name: string, state: DirDiffState, ms = 1): DirDiffEntry {
  return { name, state, leftSize: 1, leftModifiedMs: ms, rightSize: 1, rightModifiedMs: ms };
}
function report(entries: DirDiffEntry[]): CompareDirsReport {
  return { leftRoot: 'C:\\L', rightRoot: 'D:\\R', entries, truncated: false };
}

beforeEach(() => {
  compareDirsMock.mockReset();
  enqueueCopyMock.mockReset();
  validateRootsMock.mockReset();
  validateRootsMock.mockResolvedValue(undefined);
});

describe('compareForSync / buildSyncDryRun (read-only)', () => {
  it('compares the two roots by metadata (not by content) and mutates nothing', async () => {
    compareDirsMock.mockResolvedValue(report([diff('a', 'leftOnly')]));
    await compareForSync('C:\\L', 'D:\\R', { recursive: false });
    expect(compareDirsMock).toHaveBeenCalledWith('C:\\L', 'D:\\R', false);
    await compareForSync('C:\\L', 'D:\\R', { byContent: true });
    // FER-477 review: skipping content reads is opt-in — off: exactly the old call.
    expect(compareDirsMock).toHaveBeenLastCalledWith('C:\\L', 'D:\\R', true, {
      recursive: true,
      noCache: true,
    });
    ui.syncJournalSkip = true;
    try {
      await compareForSync('C:\\L', 'D:\\R', { byContent: true });
      expect(compareDirsMock).toHaveBeenLastCalledWith('C:\\L', 'D:\\R', true, {
        recursive: true,
        noCache: true,
        incremental: true,
      });
    } finally {
      ui.syncJournalSkip = false;
    }
    expect(enqueueCopyMock).not.toHaveBeenCalled();
  });

  it('rejects same/nested/aliased roots before any compare runs', async () => {
    validateRootsMock.mockRejectedValue(new Error('nested roots'));
    await expect(compareForSync('C:\\L', 'C:\\L\\sub')).rejects.toThrow('nested roots');
    expect(validateRootsMock).toHaveBeenCalledWith('C:\\L', 'C:\\L\\sub');
    expect(compareDirsMock).not.toHaveBeenCalled();
  });

  it('a folder vs file of the same name is a skip-only type mismatch, never an overwrite', async () => {
    compareDirsMock.mockResolvedValue(report([{ ...diff('x', 'sizeDifferent'), isDir: true }]));
    const { plan } = await buildSyncDryRun('C:\\L', 'D:\\R', { direction: 'leftToRight' });
    const item = plan[0];
    expect(item.action).toBe('conflict');
    expect(item.reason).toBe('typeMismatch');
  });

  it('builds the classified plan and summary from one compare', async () => {
    compareDirsMock.mockResolvedValue(
      report([
        diff('l', 'leftOnly'),
        diff('r', 'rightOnly'),
        diff('s', 'same'),
        diff('c', 'contentDifferent'),
      ]),
    );
    const dry = await buildSyncDryRun('C:\\L', 'D:\\R', { direction: 'both' });
    expect(compareDirsMock).toHaveBeenCalledTimes(1);
    expect(dry.summary).toMatchObject({
      toRight: 1,
      toLeft: 1,
      conflicts: 1,
      skipped: 1,
      deletes: 0,
    });
    expect(dry.plan.map((p) => p.action)).toEqual([
      'copyToRight',
      'copyToLeft',
      'skip',
      'conflict',
    ]);
    expect(enqueueCopyMock).not.toHaveBeenCalled();
  });
});

describe('runSyncPlan', () => {
  async function dryRun(entries: DirDiffEntry[]) {
    compareDirsMock.mockResolvedValueOnce(report(entries));
    return buildSyncDryRun('C:\\L', 'D:\\R', { direction: 'both' });
  }

  it('re-compares first; a changed snapshot is stale and enqueues nothing', async () => {
    const dry = await dryRun([diff('a', 'leftOnly', 1)]);
    compareDirsMock.mockResolvedValueOnce(report([diff('a', 'leftOnly', 2)]));
    const result = await runSyncPlan(dry.report, dry.plan);
    expect(result.status).toBe('stale');
    if (result.status === 'stale') expect(result.fresh.entries[0]!.leftModifiedMs).toBe(2);
    expect(enqueueCopyMock).not.toHaveBeenCalled();
  });

  it('the pre-run re-validation never asks for the fast scan, even for a fast preview (FER-472)', async () => {
    const dry = await dryRun([diff('a', 'leftOnly')]);
    const fastPreview = { ...dry.report, recursive: true, fastScan: { left: true, right: true } };
    compareDirsMock.mockResolvedValueOnce(dry.report);
    await runSyncPlan(fastPreview, dry.plan);
    const opts = compareDirsMock.mock.calls.at(-1)![3] as Record<string, unknown>;
    expect(opts).not.toHaveProperty('fastScan');
  });

  it('a "same" entry that the fresh ordinary scan sees changed makes the plan stale', async () => {
    const dry = await dryRun([diff('a', 'same', 1), diff('b', 'leftOnly')]);
    // The snapshot (e.g. the MFT) said a == a; the live scan finds the right copy edited.
    const changed = { ...diff('a', 'sizeDifferent', 1), rightSize: 9 };
    compareDirsMock.mockResolvedValueOnce(report([changed, diff('b', 'leftOnly')]));
    const result = await runSyncPlan(dry.report, dry.plan);
    expect(result.status).toBe('stale');
    expect(enqueueCopyMock).not.toHaveBeenCalled();
  });

  it('the post-run settle compare never asks for the fast scan either', async () => {
    const dry = await dryRun([diff('a', 'same')]);
    compareDirsMock.mockResolvedValue(dry.report);
    await settleSyncRun({
      report: { ...dry.report, fastScan: { left: true, right: false } },
      plan: dry.plan,
      result: {
        status: 'queued',
        jobIds: [],
        toRight: 0,
        toLeft: 0,
        deletes: [],
        conflicts: 0,
        failed: 0,
        skipped: 0,
        cancelled: false,
        items: [],
      },
      readJob: () => undefined,
      wait: { timeoutMs: 10, pollMs: 5 },
    });
    const opts = compareDirsMock.mock.calls.at(-1)![3] as Record<string, unknown>;
    expect(opts).not.toHaveProperty('fastScan');
  });

  it('enqueues right-bound then left-bound copies with the overwrite policy and returns job ids', async () => {
    const dry = await dryRun([diff('l', 'leftOnly'), diff('r', 'rightOnly')]);
    compareDirsMock.mockResolvedValueOnce(dry.report);
    enqueueCopyMock.mockResolvedValueOnce('op-1').mockResolvedValueOnce('op-2');
    const result = await runSyncPlan(dry.report, dry.plan);
    expect(enqueueCopyMock.mock.calls).toEqual([
      [['C:\\L\\l'], 'D:\\R', 'overwrite'],
      [['D:\\R\\r'], 'C:\\L', 'overwrite'],
    ]);
    expect(result).toEqual({
      status: 'queued',
      jobIds: ['op-1', 'op-2'],
      toRight: 1,
      toLeft: 1,
      deletes: [],
      conflicts: 0,
      failed: 0,
      skipped: 0,
      cancelled: false,
      items: [
        { name: 'l', action: 'copyToRight', status: 'queued', jobId: 'op-1' },
        { name: 'r', action: 'copyToLeft', status: 'queued', jobId: 'op-2' },
      ],
    });
  });

  it('deletes are returned for the caller to confirm, never executed here', async () => {
    compareDirsMock.mockResolvedValueOnce(
      report([diff('x', 'rightOnly'), diff('c', 'contentDifferent')]),
    );
    const dry = await buildSyncDryRun('C:\\L', 'D:\\R', {
      direction: 'leftToRight',
      deleteExtras: true,
    });
    compareDirsMock.mockResolvedValueOnce(dry.report);
    const result = await runSyncPlan(dry.report, dry.plan, { allowOverwrite: true });
    expect(enqueueCopyMock.mock.calls.length).toBe(1); // only the contentDifferent -> updateRight
    expect(result).toMatchObject({
      status: 'queued',
      deletes: ['D:\\R\\x'],
      toRight: 1,
      conflicts: 0,
    });
  });

  it('conflicts are counted but never executed', async () => {
    const dry = await dryRun([diff('c', 'contentDifferent')]);
    compareDirsMock.mockResolvedValueOnce(dry.report);
    const result = await runSyncPlan(dry.report, dry.plan);
    expect(enqueueCopyMock).not.toHaveBeenCalled();
    expect(result).toMatchObject({ status: 'queued', jobIds: [], conflicts: 1 });
  });

  it('nothing to copy enqueues nothing and yields no job ids', async () => {
    const dry = await dryRun([diff('s', 'same')]);
    compareDirsMock.mockResolvedValueOnce(dry.report);
    const result = await runSyncPlan(dry.report, dry.plan);
    expect(enqueueCopyMock).not.toHaveBeenCalled();
    expect(result).toMatchObject({ status: 'queued', jobIds: [] });
  });
});

describe('runSyncPlan safety (FER-110/111/112)', () => {
  it('refuses a plan from a truncated/errored report without comparing or enqueuing', async () => {
    const rep = { ...report([diff('a', 'leftOnly')]), truncated: true };
    const result = await runSyncPlan(rep, []);
    expect(result).toMatchObject({ status: 'blocked', reasons: ['truncated'] });
    expect(compareDirsMock).not.toHaveBeenCalled();
    expect(enqueueCopyMock).not.toHaveBeenCalled();
  });

  it('overwrites are skipped (and reported) unless allowOverwrite is set', async () => {
    compareDirsMock.mockResolvedValue(report([diff('n', 'newerLeft')]));
    const dry = await buildSyncDryRun('C:\\L', 'D:\\R', { direction: 'leftToRight' });
    const result = await runSyncPlan(dry.report, dry.plan);
    expect(enqueueCopyMock).not.toHaveBeenCalled();
    expect(result).toMatchObject({
      status: 'queued',
      skipped: 1,
      items: [{ name: 'n', status: 'skipped', reason: 'overwrite not confirmed' }],
    });
  });

  it('content mode re-validates by content', async () => {
    compareDirsMock.mockResolvedValue(report([diff('a', 'contentDifferent')]));
    const dry = await buildSyncDryRun(
      'C:\\L',
      'D:\\R',
      { direction: 'leftToRight' },
      { byContent: true },
    );
    await runSyncPlan(dry.report, dry.plan, { byContent: true, allowOverwrite: true });
    expect(compareDirsMock).toHaveBeenLastCalledWith('C:\\L', 'D:\\R', true, {
      recursive: false,
      noCache: true,
    });
  });
});
