/**
 * Tabel semantik sync dua arah (spec AI-10 bagian 6) — perilaku, bukan
 * implementasi. Setiap `it` memetakan satu baris tabel/matriks acceptance.
 */
import { describe, expect, it } from 'vitest';
import { buildSyncPlan, syncPlanBlockers } from './syncPlan';
import type { SyncBaseline } from './syncBaseline';
import type { CompareDirsReport, DirDiffEntry, DirDiffState } from './ipc/compare';

const L = 'C:\\L';
const R = 'D:\\R';

function e(
  name: string,
  state: DirDiffState,
  left?: { size: number; ms: number },
  right?: { size: number; ms: number },
): DirDiffEntry {
  return {
    name,
    state,
    leftSize: left?.size ?? 0,
    leftModifiedMs: left?.ms ?? 0,
    rightSize: right?.size ?? 0,
    rightModifiedMs: right?.ms ?? 0,
  };
}

function rep(entries: DirDiffEntry[], truncated = false): CompareDirsReport {
  return { leftRoot: L, rightRoot: R, entries, truncated };
}

function base(entries: SyncBaseline['entries'], roots: [string, string] = [L, R]): SyncBaseline {
  return { version: 1, leftRoot: roots[0], rightRoot: roots[1], entries };
}

const T0 = 1_700_000_000_000;
const facts = (size: number, ms = T0) => ({ size, modifiedMs: ms });
const actionOf = (plan: ReturnType<typeof buildSyncPlan>, name: string) =>
  plan.find((p) => p.name === name)!;

describe('first run (no baseline), both directions', () => {
  it('left-only copies right, right-only copies left, never deletes', () => {
    const plan = buildSyncPlan(
      rep([
        e('a', 'leftOnly', { size: 1, ms: T0 }),
        e('b', 'rightOnly', undefined, { size: 1, ms: T0 }),
      ]),
      { direction: 'both', deleteExtras: true },
    );
    expect(actionOf(plan, 'a').action).toBe('copyToRight');
    expect(actionOf(plan, 'b').action).toBe('copyToLeft');
    expect(plan.some((p) => p.action.startsWith('delete'))).toBe(false);
  });

  it('same path with different content is a conflict even if one side is newer', () => {
    const plan = buildSyncPlan(
      rep([
        e('n1', 'newerLeft', { size: 5, ms: T0 + 10_000 }, { size: 5, ms: T0 }),
        e('n2', 'newerRight', { size: 5, ms: T0 }, { size: 5, ms: T0 + 10_000 }),
        e('s', 'sizeDifferent', { size: 5, ms: T0 }, { size: 9, ms: T0 }),
        e('c', 'contentDifferent', { size: 5, ms: T0 }, { size: 5, ms: T0 }),
      ]),
      { direction: 'both' },
    );
    for (const name of ['n1', 'n2', 's', 'c']) {
      expect(actionOf(plan, name)).toMatchObject({ action: 'conflict', reason: 'noBaseline' });
    }
  });

  it('an untrusted baseline (other roots, wrong version) behaves as first run', () => {
    const wrongRoots = base({}, [L, 'E:\\Else']);
    const plan = buildSyncPlan(
      rep([e('n1', 'newerLeft', { size: 5, ms: T0 + 10_000 }, { size: 5, ms: T0 })]),
      { direction: 'both', baseline: wrongRoots },
    );
    expect(actionOf(plan, 'n1')).toMatchObject({ action: 'conflict', reason: 'noBaseline' });

    const corrupt = {
      version: 99,
      leftRoot: L,
      rightRoot: R,
      entries: {},
    } as unknown as SyncBaseline;
    const plan2 = buildSyncPlan(
      rep([e('n1', 'newerLeft', { size: 5, ms: T0 + 10_000 }, { size: 5, ms: T0 })]),
      { direction: 'both', baseline: corrupt },
    );
    expect(actionOf(plan2, 'n1').action).toBe('conflict');
  });
});

describe('with a trusted baseline (both directions)', () => {
  const baseline = base({
    f: { left: facts(5), right: facts(5) },
  });

  it('only left changed updates right', () => {
    const plan = buildSyncPlan(
      rep([e('f', 'newerLeft', { size: 6, ms: T0 + 60_000 }, { size: 5, ms: T0 })]),
      { direction: 'both', baseline },
    );
    expect(actionOf(plan, 'f').action).toBe('updateRight');
  });

  it('only right changed updates left', () => {
    const plan = buildSyncPlan(
      rep([e('f', 'newerRight', { size: 5, ms: T0 }, { size: 8, ms: T0 + 60_000 })]),
      { direction: 'both', baseline },
    );
    expect(actionOf(plan, 'f').action).toBe('updateLeft');
  });

  it('both sides changed differently is a conflict, regardless of which is newer', () => {
    const plan = buildSyncPlan(
      rep([e('f', 'newerRight', { size: 7, ms: T0 + 1_000_000 }, { size: 8, ms: T0 + 2_000_000 })]),
      { direction: 'both', baseline },
    );
    expect(actionOf(plan, 'f')).toMatchObject({ action: 'conflict', reason: 'bothChanged' });
  });

  it('one side missing and the other untouched: no delete propagation, decision needed', () => {
    const plan = buildSyncPlan(rep([e('f', 'leftOnly', { size: 5, ms: T0 })]), {
      direction: 'both',
      baseline,
      deleteExtras: true,
    });
    expect(actionOf(plan, 'f')).toMatchObject({ action: 'conflict', reason: 'deletedRight' });
    const plan2 = buildSyncPlan(rep([e('f', 'rightOnly', undefined, { size: 5, ms: T0 })]), {
      direction: 'both',
      baseline,
    });
    expect(actionOf(plan2, 'f')).toMatchObject({ action: 'conflict', reason: 'deletedLeft' });
  });

  it('one side deleted and the other modified is a conflict, never auto delete or overwrite', () => {
    const plan = buildSyncPlan(rep([e('f', 'leftOnly', { size: 99, ms: T0 + 99_000 })]), {
      direction: 'both',
      baseline,
    });
    expect(actionOf(plan, 'f')).toMatchObject({ action: 'conflict', reason: 'modifyDelete' });
    const plan2 = buildSyncPlan(
      rep([e('f', 'rightOnly', undefined, { size: 99, ms: T0 + 99_000 })]),
      { direction: 'both', baseline },
    );
    expect(actionOf(plan2, 'f')).toMatchObject({ action: 'conflict', reason: 'modifyDelete' });
  });

  it('a new file not in the baseline still unions to the other side', () => {
    const plan = buildSyncPlan(rep([e('new', 'leftOnly', { size: 1, ms: T0 })]), {
      direction: 'both',
      baseline,
    });
    expect(actionOf(plan, 'new').action).toBe('copyToRight');
  });

  it('mtime jitter within tolerance does not count as a change', () => {
    const plan = buildSyncPlan(
      rep([e('f', 'newerLeft', { size: 5, ms: T0 + 2_000 }, { size: 5, ms: T0 })]),
      { direction: 'both', baseline },
    );
    // FER-414: neither side changed beyond tolerance since the verified-identical baseline,
    // so the leftover metadata difference (e.g. a copy's fresh mtime) needs no action. It is
    // never a silent winner: nothing is written.
    expect(actionOf(plan, 'f').action).toBe('skip');
  });
});

describe('one-directional runs keep explicit semantics', () => {
  it('leftToRight still overwrites an explicit direction and never touches left', () => {
    const plan = buildSyncPlan(
      rep([e('f', 'sizeDifferent', { size: 1, ms: T0 }, { size: 2, ms: T0 })]),
      { direction: 'leftToRight' },
    );
    expect(actionOf(plan, 'f').action).toBe('updateRight');
  });
});

describe('case collisions and blockers', () => {
  it('names differing only by case never become silent copies (any direction)', () => {
    for (const direction of ['both', 'leftToRight', 'rightToLeft'] as const) {
      const plan = buildSyncPlan(
        rep([
          e('Report.TXT', 'leftOnly', { size: 1, ms: T0 }),
          e('report.txt', 'rightOnly', undefined, { size: 2, ms: T0 }),
        ]),
        { direction },
      );
      // nothing may write: every item is a conflict or an (untouched) skip
      for (const item of plan) {
        expect(['conflict', 'skip']).toContain(item.action);
      }
      expect(plan.some((p) => p.action === 'conflict' && p.reason === 'caseCollision')).toBe(true);
    }
  });

  it('equal entries in a case group stay skipped', () => {
    const plan = buildSyncPlan(
      rep([
        e('A', 'same', { size: 1, ms: T0 }, { size: 1, ms: T0 }),
        e('a', 'same', { size: 1, ms: T0 }, { size: 1, ms: T0 }),
      ]),
      { direction: 'both' },
    );
    expect(plan.every((p) => p.action === 'skip')).toBe(true);
  });

  it('a truncated compare report blocks execution', () => {
    expect(syncPlanBlockers(rep([], true))).toContain('truncated');
    expect(syncPlanBlockers(rep([], false))).toEqual([]);
  });
});
