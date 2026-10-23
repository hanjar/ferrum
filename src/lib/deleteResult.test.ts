import { describe, expect, it } from 'vitest';
import type { DeleteJobResult } from './ipc/deleteJobs';
import {
  deleteChangedDisk,
  deleteHasProblems,
  deleteOutcome,
  deleteProblemText,
  deleteProgressDetail,
  deleteRemaining,
  deleteSummary,
} from './deleteResult';

const make = (over: Partial<DeleteJobResult> = {}, report = {}): DeleteJobResult => ({
  report: { requested: 3, deleted: 3, failed: [], ...report },
  cancelled: false,
  notProcessed: [],
  partial: [],
  ...over,
});

describe('deleteResult', () => {
  it('a clean job has no problem text and a "finished" summary', () => {
    const r = make();
    expect(deleteHasProblems(r)).toBe(false);
    expect(deleteProblemText(r)).toBe('');
    expect(deleteSummary(r)).toBe('Delete finished: 3 items');
    expect(deleteOutcome(r)).toBe('done');
  });

  it('failures keep the "N of M could not be deleted: ..." wording', () => {
    const r = make(
      {},
      {
        deleted: 1,
        failed: [
          { path: 'C:\\a', code: 'permissionDenied', message: 'locked' },
          { path: 'C:\\b', code: 'notFound', message: 'gone' },
        ],
      },
    );
    expect(deleteProblemText(r)).toBe('2 of 3 could not be deleted: locked; gone');
    expect(deleteOutcome(r)).toBe('failed');
    expect(deleteSummary(r)).toBe('Delete failed: 2 of 3 items');
  });

  it('a cancel reports deleted, partly deleted and untouched items and what to retry', () => {
    const r = make(
      { cancelled: true, partial: ['C:\\data\\big'], notProcessed: ['C:\\data\\x', 'C:\\data\\y'] },
      { deleted: 1 },
    );
    const text = deleteProblemText(r);
    expect(text).toContain('Cancelled: 1 of 3 deleted.');
    expect(text).toContain('1 folder was only partly deleted and still exists: big.');
    expect(text).toContain('2 items were not touched.');
    expect(deleteOutcome(r)).toBe('canceled');
    expect(deleteSummary(r)).toBe('Delete cancelled: 1/3 deleted, 1 folder partly deleted');
    expect(deleteRemaining(r)).toEqual(['C:\\data\\big', 'C:\\data\\x', 'C:\\data\\y']);
  });

  it('changed-disk is true for a deleted item or a partly deleted folder only', () => {
    expect(deleteChangedDisk(make())).toBe(true);
    expect(deleteChangedDisk(make({ partial: ['C:\\d'] }, { deleted: 0 }))).toBe(true);
    expect(
      deleteChangedDisk(make({ cancelled: true, notProcessed: ['C:\\d'] }, { deleted: 0 })),
    ).toBe(false);
  });

  it('a folder that failed in the middle is not listed twice', () => {
    const r = make(
      { partial: ['C:/d/mixed'] },
      {
        deleted: 0,
        failed: [{ path: 'C:/d/mixed', code: 'io', message: 'locked (partly deleted)' }],
      },
    );
    const text = deleteProblemText(r);
    expect(text).not.toContain('only partly deleted');
    expect(text).toContain('locked');
    expect(deleteRemaining(make({ partial: ['a'], notProcessed: ['a', 'b'] }))).toEqual(['a', 'b']);
  });

  it('long lists are shortened', () => {
    const r = make({ cancelled: true, partial: ['a\\1', 'a\\2', 'a\\3', 'a\\4'] }, { deleted: 0 });
    expect(deleteProblemText(r)).toContain('1, 2, 3 …');
  });
});

describe('deleteProgressDetail (FER-586)', () => {
  it('is empty before anything was removed', () => {
    expect(deleteProgressDetail({ done: 0, total: 1, entries: 0, bytes: 0 })).toBe('');
  });
  it('counts entries and bytes inside folders', () => {
    expect(deleteProgressDetail({ done: 0, total: 1, entries: 1204, bytes: 3 * 1024 * 1024 })).toBe(
      '1,204 entries removed, 3.00 MB',
    );
    expect(deleteProgressDetail({ done: 0, total: 1, entries: 1, bytes: 0 })).toBe(
      '1 entry removed',
    );
  });
  it('accepts a progress without bytes (older backend)', () => {
    expect(deleteProgressDetail({ done: 1, total: 2, entries: 40 })).toBe('40 entries removed');
  });
});
