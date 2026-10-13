import { describe, expect, it } from 'vitest';
import {
  BASELINE_VERSION,
  emptyBaseline,
  factsEqual,
  isBaselineTrusted,
  reconcileBaseline,
  sideChange,
  type SyncBaseline,
} from './syncBaseline';
import { buildSyncPlan } from './syncPlan';
import type { CompareDirsReport, DirDiffEntry, DirDiffState } from './ipc/compare';

const L = 'C:\\L';
const R = 'D:\\R';
const T0 = 1_700_000_000_000;

function e(
  name: string,
  state: DirDiffState,
  left?: [number, number],
  right?: [number, number],
): DirDiffEntry {
  return {
    name,
    state,
    leftSize: left?.[0] ?? 0,
    leftModifiedMs: left?.[1] ?? 0,
    rightSize: right?.[0] ?? 0,
    rightModifiedMs: right?.[1] ?? 0,
  };
}
const rep = (entries: DirDiffEntry[]): CompareDirsReport => ({
  leftRoot: L,
  rightRoot: R,
  entries,
  truncated: false,
});

describe('baseline trust', () => {
  it('trusts only a well-formed baseline bound to the same root pair', () => {
    const b = emptyBaseline(L, R);
    expect(b.version).toBe(BASELINE_VERSION);
    expect(isBaselineTrusted(b, L, R)).toBe(true);
    expect(isBaselineTrusted(b, L, 'D:\\Other')).toBe(false);
    expect(isBaselineTrusted(b, R, L)).toBe(false); // swapped panes = another pair identity
    expect(isBaselineTrusted(null, L, R)).toBe(false);
    expect(isBaselineTrusted({ ...b, version: 2 }, L, R)).toBe(false);
    expect(isBaselineTrusted({ ...b, entries: null }, L, R)).toBe(false);
    expect(isBaselineTrusted('garbage', L, R)).toBe(false);
  });

  it('separator and trailing slash differences do not change the identity', () => {
    const b = emptyBaseline('C:\\L\\', 'D:/R');
    expect(isBaselineTrusted(b, 'C:/L', 'D:\\R\\')).toBe(true);
  });
});

describe('facts and side changes', () => {
  it('compares size and mtime with the 2s FAT tolerance', () => {
    expect(factsEqual({ size: 1, modifiedMs: T0 }, { size: 1, modifiedMs: T0 + 2000 })).toBe(true);
    expect(factsEqual({ size: 1, modifiedMs: T0 }, { size: 1, modifiedMs: T0 + 2001 })).toBe(false);
    expect(factsEqual({ size: 1, modifiedMs: T0 }, { size: 2, modifiedMs: T0 })).toBe(false);
  });

  it('classifies a side as unchanged / changed / missing', () => {
    const f = { size: 1, modifiedMs: T0 };
    expect(sideChange(f, f)).toBe('unchanged');
    expect(sideChange(f, { size: 2, modifiedMs: T0 })).toBe('changed');
    expect(sideChange(f, null)).toBe('missing');
  });
});

describe('reconcileBaseline (only verified results move the baseline)', () => {
  const before = rep([
    e('ok.txt', 'leftOnly', [5, T0]),
    e('fail.txt', 'leftOnly', [6, T0]),
    e('cancel.txt', 'rightOnly', undefined, [7, T0]),
    e('conf.txt', 'sizeDifferent', [1, T0], [2, T0]),
    e('eq.txt', 'same', [3, T0], [3, T0]),
  ]);
  const plan = buildSyncPlan(before, { direction: 'both' });

  // fresh compare after the run: ok.txt now present on both sides and equal;
  // fail/cancel did not land.
  const after = rep([
    e('ok.txt', 'same', [5, T0], [5, T0 + 500]),
    e('fail.txt', 'leftOnly', [6, T0]),
    e('cancel.txt', 'rightOnly', undefined, [7, T0]),
    e('conf.txt', 'sizeDifferent', [1, T0], [2, T0]),
    e('eq.txt', 'same', [3, T0], [3, T0]),
  ]);

  it('records succeeded and already-equal items, nothing else', () => {
    const next = reconcileBaseline(null, after, plan, {
      'ok.txt': 'ok',
      'fail.txt': 'failed',
      'cancel.txt': 'cancelled',
    });
    expect(Object.keys(next.entries).sort()).toEqual(['eq.txt', 'ok.txt']);
    expect(next.entries['ok.txt']).toEqual({
      left: { size: 5, modifiedMs: T0 },
      right: { size: 5, modifiedMs: T0 + 500 },
    });
    expect(next.leftRoot).toBe(L);
    expect(next.rightRoot).toBe(R);
  });

  it('an "ok" outcome that the fresh compare does not confirm does NOT move the baseline', () => {
    const lying = rep([e('ok.txt', 'leftOnly', [5, T0])]);
    const next = reconcileBaseline(null, lying, plan, { 'ok.txt': 'ok' });
    expect(next.entries['ok.txt']).toBeUndefined();
  });

  it('failed, cancelled and conflicted items keep their previous baseline entry untouched', () => {
    const prev: SyncBaseline = {
      version: 1,
      leftRoot: L,
      rightRoot: R,
      entries: {
        'conf.txt': {
          left: { size: 1, modifiedMs: T0 - 5000 },
          right: { size: 1, modifiedMs: T0 - 5000 },
        },
        'fail.txt': {
          left: { size: 6, modifiedMs: T0 - 9000 },
          right: { size: 6, modifiedMs: T0 - 9000 },
        },
      },
    };
    const next = reconcileBaseline(prev, after, plan, {
      'fail.txt': 'failed',
      'cancel.txt': 'cancelled',
    });
    expect(next.entries['conf.txt']).toEqual(prev.entries['conf.txt']);
    expect(next.entries['fail.txt']).toEqual(prev.entries['fail.txt']);
    // input is not mutated
    expect(prev.entries['ok.txt']).toBeUndefined();
  });

  it('does not inherit entries from a baseline of another root pair', () => {
    const other: SyncBaseline = {
      version: 1,
      leftRoot: 'X:\\',
      rightRoot: 'Y:\\',
      entries: {
        'stale.txt': { left: { size: 1, modifiedMs: 1 }, right: { size: 1, modifiedMs: 1 } },
      },
    };
    const next = reconcileBaseline(other, after, plan, {});
    expect(next.entries['stale.txt']).toBeUndefined();
    expect(next.leftRoot).toBe(L);
  });

  it('a verified delete (gone from both sides) drops the baseline entry', () => {
    const prev: SyncBaseline = {
      version: 1,
      leftRoot: L,
      rightRoot: R,
      entries: {
        'gone.txt': { left: { size: 1, modifiedMs: T0 }, right: { size: 1, modifiedMs: T0 } },
      },
    };
    const delPlan = buildSyncPlan(rep([e('gone.txt', 'rightOnly', undefined, [1, T0])]), {
      direction: 'leftToRight',
      deleteExtras: true,
    });
    expect(delPlan[0].action).toBe('deleteRight');
    const next = reconcileBaseline(prev, rep([]), delPlan, { 'gone.txt': 'ok' });
    expect(next.entries['gone.txt']).toBeUndefined();
  });
});

describe('journal marks (FER-477b)', () => {
  const marks = {
    left: { journalId: '133000000000000001', usn: '4096', rootFrn: '5', path: 'C:/L' },
    right: { journalId: '133000000000000002', usn: '8192', rootFrn: '6', path: 'D:/R' },
    takenAtMs: 1_800_000_000_000,
  };
  const clean = (): CompareDirsReport => ({
    ...rep([
      { ...e('dir', 'same'), isDir: true },
      e('dir/a.txt', 'same', [3, T0], [3, T0 + 900]),
      e('b.txt', 'same', [4, T0], [4, T0]),
    ]),
    recursive: true,
    journal: marks,
  });
  const planOf = (r: CompareDirsReport) => buildSyncPlan(r, { direction: 'both' });

  it('keeps the marks only when the settle compare proves the pair fully in sync', () => {
    const after = clean();
    const next = reconcileBaseline(null, after, planOf(after), {});
    expect(next.journal).toEqual(marks);
  });

  it('drops the marks after anything that is not a complete, clean, recorded result', () => {
    const variants: CompareDirsReport[] = [
      { ...clean(), entries: [...clean().entries, e('new.txt', 'leftOnly', [1, T0])] },
      { ...clean(), truncated: true },
      { ...clean(), cancelled: true },
      { ...clean(), errors: [{ name: 'x', message: 'denied' }] },
      { ...clean(), recursive: undefined },
      { ...clean(), journal: undefined },
    ];
    for (const after of variants) {
      const next = reconcileBaseline(null, after, planOf(after), {});
      expect(next.journal).toBeUndefined();
    }
  });

  it('never carries marks over from the previous baseline', () => {
    const prev: SyncBaseline = { ...emptyBaseline(L, R), journal: marks };
    const after = { ...clean(), journal: undefined };
    expect(reconcileBaseline(prev, after, planOf(after), {}).journal).toBeUndefined();
  });

  it('a stale entry kept from an older run (failed update) blocks the marks', () => {
    // The fresh compare says `same`, but the plan item was a failed update: the baseline keeps
    // the OLD facts, so it no longer equals what was verified → no marks.
    const before = rep([e('b.txt', 'sizeDifferent', [4, T0], [5, T0])]);
    const plan = buildSyncPlan(before, { direction: 'both' });
    const prev: SyncBaseline = {
      ...emptyBaseline(L, R),
      entries: { 'b.txt': { left: { size: 9, modifiedMs: 1 }, right: { size: 9, modifiedMs: 1 } } },
    };
    const after: CompareDirsReport = {
      ...rep([e('b.txt', 'same', [4, T0], [4, T0])]),
      recursive: true,
      journal: marks,
    };
    const next = reconcileBaseline(prev, after, plan, { 'b.txt': 'failed' });
    expect(next.entries['b.txt']).toEqual(prev.entries['b.txt']);
    expect(next.journal).toBeUndefined();
  });
});
