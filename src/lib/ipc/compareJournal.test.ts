import { describe, expect, it } from 'vitest';
import { parseJournalMarks } from './compare';

describe('journal marks from the compare report (FER-477b)', () => {
  const ok = {
    left: {
      journalId: '18446744073709551615',
      usn: '9223372036854775807',
      rootFrn: '281474976710661',
      path: 'C:/Data',
    },
    right: { journalId: '1', usn: '0', rootFrn: '5', path: 'D:/' },
    takenAtMs: 1_800_000_000_000,
  };

  it('keeps well-formed decimal marks for both sides (u64 as strings)', () => {
    expect(parseJournalMarks(ok)).toEqual(ok);
  });

  it('drops anything malformed: one side, numbers, signs, garbage', () => {
    for (const bad of [
      null,
      'x',
      { left: ok.left },
      { left: ok.left, right: { journalId: 1, usn: '1' } },
      { left: ok.left, right: { journalId: '-1', usn: '1' } },
      { left: ok.left, right: { journalId: '1', usn: '1e3' } },
      { left: ok.left, right: { journalId: '1', usn: '123456789012345678901' } },
      { left: ok.left, right: { journalId: '', usn: '1' } },
      { ...ok, takenAtMs: undefined },
      { ...ok, takenAtMs: -1 },
      { ...ok, right: { ...ok.right, rootFrn: 'x' } },
      { ...ok, right: { ...ok.right, path: '' } },
    ]) {
      expect(parseJournalMarks(bad)).toBeUndefined();
    }
  });
});
