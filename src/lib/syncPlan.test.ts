import { describe, expect, it } from 'vitest';
import {
  buildSyncPlan,
  classifyEntry,
  isPlanStale,
  planFacts,
  planSummary,
  type SyncOptions,
} from './syncPlan';
import type { CompareDirsReport, DirDiffEntry, DirDiffState } from './ipc/compare';

function entry(name: string, state: DirDiffState): DirDiffEntry {
  return { name, state, leftSize: 5, leftModifiedMs: 100, rightSize: 7, rightModifiedMs: 200 };
}

function report(entries: DirDiffEntry[]): CompareDirsReport {
  return { leftRoot: 'C:\\L', rightRoot: 'D:\\R', entries, truncated: false };
}

const DIRS: SyncOptions['direction'][] = ['leftToRight', 'rightToLeft', 'both'];

describe('classifyEntry (F2-19)', () => {
  it('same entries are always skipped', () => {
    for (const direction of DIRS) {
      expect(classifyEntry(entry('a', 'same'), 'C:\\L', 'D:\\R', { direction })).toBe('skip');
    }
  });

  it('leftOnly copies right unless direction is rightToLeft', () => {
    expect(classifyEntry(entry('a', 'leftOnly'), 'C', 'D', { direction: 'leftToRight' })).toBe(
      'copyToRight',
    );
    expect(classifyEntry(entry('a', 'leftOnly'), 'C', 'D', { direction: 'both' })).toBe(
      'copyToRight',
    );
    expect(classifyEntry(entry('a', 'leftOnly'), 'C', 'D', { direction: 'rightToLeft' })).toBe(
      'skip',
    );
  });

  it('rightOnly copies left; one-directional runs can delete extras on request', () => {
    expect(classifyEntry(entry('a', 'rightOnly'), 'C', 'D', { direction: 'rightToLeft' })).toBe(
      'copyToLeft',
    );
    expect(classifyEntry(entry('a', 'rightOnly'), 'C', 'D', { direction: 'leftToRight' })).toBe(
      'skip',
    );
    expect(
      classifyEntry(entry('a', 'rightOnly'), 'C', 'D', {
        direction: 'leftToRight',
        deleteExtras: true,
      }),
    ).toBe('deleteRight');
    // both-directions never deletes (ambiguous which side is the source)
    expect(
      classifyEntry(entry('a', 'rightOnly'), 'C', 'D', { direction: 'both', deleteExtras: true }),
    ).toBe('copyToLeft');
  });

  it('newer side wins in its direction', () => {
    expect(classifyEntry(entry('a', 'newerLeft'), 'C', 'D', { direction: 'leftToRight' })).toBe(
      'updateRight',
    );
    expect(classifyEntry(entry('a', 'newerLeft'), 'C', 'D', { direction: 'rightToLeft' })).toBe(
      'skip',
    );
    expect(classifyEntry(entry('a', 'newerRight'), 'C', 'D', { direction: 'rightToLeft' })).toBe(
      'updateLeft',
    );
    expect(classifyEntry(entry('a', 'newerRight'), 'C', 'D', { direction: 'leftToRight' })).toBe(
      'skip',
    );
  });

  it('conflicting content: direction decides, both-directions conflicts', () => {
    for (const state of ['sizeDifferent', 'contentDifferent'] as const) {
      expect(classifyEntry(entry('a', state), 'C', 'D', { direction: 'leftToRight' })).toBe(
        'updateRight',
      );
      expect(classifyEntry(entry('a', state), 'C', 'D', { direction: 'rightToLeft' })).toBe(
        'updateLeft',
      );
      expect(classifyEntry(entry('a', state), 'C', 'D', { direction: 'both' })).toBe('conflict');
    }
  });
});

describe('buildSyncPlan', () => {
  it('maps every entry and keeps correct side paths', () => {
    const plan = buildSyncPlan(
      report([entry('left-only.txt', 'leftOnly'), entry('right-only.txt', 'rightOnly')]),
      { direction: 'both' },
    );
    expect(plan).toHaveLength(2);
    expect(plan[0]).toMatchObject({
      name: 'left-only.txt',
      action: 'copyToRight',
      leftPath: 'C:\\L\\left-only.txt',
      rightPath: 'D:\\R\\left-only.txt',
    });
    expect(plan[1]).toMatchObject({
      name: 'right-only.txt',
      action: 'copyToLeft',
      leftPath: 'C:\\L\\right-only.txt',
      rightPath: 'D:\\R\\right-only.txt',
    });
  });

  it('forward-slash roots build slash paths', () => {
    const plan = buildSyncPlan(
      {
        leftRoot: '/tmp/l',
        rightRoot: '/tmp/r',
        truncated: false,
        entries: [entry('a', 'leftOnly')],
      },
      { direction: 'leftToRight' },
    );
    expect(plan[0].leftPath).toBe('/tmp/l/a');
    expect(plan[0].rightPath).toBe('/tmp/r/a');
  });
});

describe('planSummary', () => {
  it('counts each action class', () => {
    const items = buildSyncPlan(
      report([
        entry('a', 'same'),
        entry('b', 'leftOnly'),
        entry('c', 'rightOnly'),
        entry('d', 'newerLeft'),
        entry('e', 'newerRight'),
        entry('f', 'sizeDifferent'),
      ]),
      { direction: 'both' },
    );
    expect(planSummary(items)).toEqual({
      total: 6,
      toRight: 1,
      toLeft: 1,
      overwrites: 0,
      deletes: 0,
      // first run, both directions: newer-wins is gone, differences conflict
      conflicts: 3,
      skipped: 1,
      userSkipped: 0,
    });
  });
});

describe('stale plan detection', () => {
  it('detects changed sizes, times, and added/removed entries', () => {
    const before = report([entry('a.txt', 'same'), entry('b.txt', 'newerLeft')]);
    const same = report([entry('a.txt', 'same'), entry('b.txt', 'newerLeft')]);
    expect(isPlanStale(before, same)).toBe(false);

    // state derivation changed but raw facts identical → NOT stale
    const reDerived = report([entry('a.txt', 'same'), entry('b.txt', 'same')]);
    expect(isPlanStale(before, reDerived)).toBe(false);

    const touched = report([
      entry('a.txt', 'same'),
      { ...entry('b.txt', 'newerLeft'), leftSize: 6 },
    ]);
    expect(isPlanStale(before, touched)).toBe(true);

    const added = report([entry('a.txt', 'same'), entry('b.txt', 'newerLeft'), entry('c', 'same')]);
    expect(isPlanStale(before, added)).toBe(true);

    const removed = report([entry('a.txt', 'same')]);
    expect(isPlanStale(before, removed)).toBe(true);

    const movedRoots = report([entry('a.txt', 'same'), entry('b.txt', 'newerLeft')]);
    movedRoots.leftRoot = 'E:\\other';
    expect(isPlanStale(before, movedRoots)).toBe(true);
  });

  it('a folder own mtime never makes an unchanged plan look stale (FER-472)', () => {
    const dir = (ms: number) => ({
      ...entry('d', 'same'),
      isDir: true,
      leftModifiedMs: ms,
      rightModifiedMs: ms,
    });
    expect(isPlanStale(report([dir(1000)]), report([dir(2000)]))).toBe(false);
    // ...while a file mtime still does.
    const file = (ms: number) => ({ ...entry('f', 'same'), leftModifiedMs: ms });
    expect(isPlanStale(report([file(1000)]), report([file(2000)]))).toBe(true);
  });

  it('planFacts are independent of state derivation', () => {
    const a = planFacts(report([entry('x', 'newerLeft')]));
    const b = planFacts(report([entry('x', 'same')]));
    expect(a).toEqual(b);
  });
});

describe('FER-110/111/112 plan rules', () => {
  it('rightToLeft + deleteExtras deletes left-only extras; without it they are skipped', () => {
    const r = report([entry('x', 'leftOnly')]);
    expect(buildSyncPlan(r, { direction: 'rightToLeft' })[0].action).toBe('skip');
    expect(buildSyncPlan(r, { direction: 'rightToLeft', deleteExtras: true })[0].action).toBe(
      'deleteLeft',
    );
    // 'both' never deletes
    expect(buildSyncPlan(r, { direction: 'both', deleteExtras: true })[0].action).toBe(
      'copyToRight',
    );
  });

  it('recursive names become native paths and folders stay one entry', () => {
    const plan = buildSyncPlan(
      report([
        { ...entry('a/b/c.txt', 'leftOnly') },
        { ...entry('dir', 'rightOnly'), isDir: true },
      ]),
      { direction: 'both' },
    );
    expect(plan[0].leftPath).toBe('C:\\L\\a\\b\\c.txt');
    expect(plan[0].rightPath).toBe('D:\\R\\a\\b\\c.txt');
    expect(plan[1]).toMatchObject({ action: 'copyToLeft', isDir: true });
  });

  it('syncPlanBlockers flags truncated, cancelled and errors', async () => {
    const { syncPlanBlockers, describeBlockers } = await import('./syncPlan');
    const r = {
      ...report([]),
      truncated: true,
      cancelled: true,
      errors: [{ name: 'p', message: 'denied' }],
    };
    expect(syncPlanBlockers(r)).toEqual(['truncated', 'cancelled', 'errors']);
    expect(describeBlockers(r)).toMatch(/truncated.*cancelled.*p: denied/);
    expect(describeBlockers(report([]))).toBe('');
  });

  it('applyDecisions resolves conflicts and leaves undecided ones as conflicts', async () => {
    const { applyDecisions } = await import('./syncPlan');
    const plan = buildSyncPlan(
      report([entry('a', 'contentDifferent'), entry('b', 'contentDifferent')]),
      { direction: 'both' },
    );
    const out = applyDecisions(plan, { a: 'keepLeft' });
    expect(out.map((p) => p.action)).toEqual(['updateRight', 'conflict']);
    expect(planSummary(out)).toMatchObject({ overwrites: 1, conflicts: 1 });
  });

  it('content mode compares the verdict too (size+mtime alone is not enough)', () => {
    const a = report([entry('a', 'contentDifferent')]);
    const b = report([entry('a', 'same')]);
    expect(isPlanStale(a, b)).toBe(false);
    expect(isPlanStale(a, b, { byContent: true })).toBe(true);
  });
});
