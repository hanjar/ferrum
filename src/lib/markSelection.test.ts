import { describe, expect, it } from 'vitest';
import {
  applySavedSelection,
  extensionOf,
  formatDetails,
  formatNames,
  formatPathDetails,
  formatPaths,
  formatTimestamp,
  globToRegExp,
  invertSelection,
  matchName,
  parseSelectionFile,
  selectMatching,
  selectSameExtension,
  serializeSelectionFile,
  type EntryInfo,
} from './markSelection';

function entry(name: string, isDir = false): EntryInfo {
  return { name, path: `C:\\dir\\${name}`, isDir, size: isDir ? 0 : 10, modifiedMs: 0 };
}

const FIXTURE: EntryInfo[] = [
  entry('report.txt'),
  entry('notes.txt'),
  entry('photo.JPG'),
  entry('archive.zip'),
  entry('Makefile'),
  entry('.gitignore'),
  entry('docs', true),
];

describe('glob masks', () => {
  it('compiles * and ? and keeps everything else literal', () => {
    expect(globToRegExp('*.txt').test('report.txt')).toBe(true);
    expect(globToRegExp('*.txt').test('archive.zip')).toBe(false);
    expect(globToRegExp('note?.txt').test('notes.txt')).toBe(true);
    expect(globToRegExp('note?.txt').test('note.txt')).toBe(false);
    // dots are literal, not regex "any char"
    expect(globToRegExp('photo.JPG').test('photoxJPG')).toBe(false);
  });

  it('matches full names only and supports multi-star masks', () => {
    expect(matchName('a/b.txt', '*.txt')).toBe(false); // never crosses separators
    expect(matchName('deeply.nested.name.txt', '*.txt')).toBe(true);
    expect(matchName('2026-09-28.log', '2026-*.log')).toBe(true);
  });

  it('is case-insensitive by default and case-sensitive on request', () => {
    expect(matchName('photo.JPG', '*.jpg')).toBe(true);
    expect(matchName('photo.JPG', '*.jpg', false)).toBe(false);
    expect(matchName('photo.JPG', '*.JPG', false)).toBe(true);
  });

  it('never matches with an empty or blank pattern', () => {
    expect(matchName('anything', '')).toBe(false);
    expect(matchName('anything', '   ')).toBe(false);
  });
});

describe('group select / unselect', () => {
  it('adds matches on top of the current selection', () => {
    const current = new Set([FIXTURE[4].path]); // Makefile
    const next = selectMatching(FIXTURE, '*.txt', { select: true }, current);
    expect(next.size).toBe(3); // Makefile + 2 txt
    expect(next.has('C:\\dir\\Makefile')).toBe(true);
    expect(current.size).toBe(1); // input untouched
  });

  it('removes matches for Unselect Group', () => {
    const all = new Set(FIXTURE.map((e) => e.path));
    const next = selectMatching(FIXTURE, '*.txt', { select: false }, all);
    expect(next.size).toBe(FIXTURE.length - 2);
    expect(next.has('C:\\dir\\report.txt')).toBe(false);
  });

  it('is a no-op when nothing matches', () => {
    const current = new Set([FIXTURE[0].path]);
    expect(selectMatching(FIXTURE, '*.nomatch', { select: true }, current)).toEqual(current);
  });
});

describe('invert and same extension', () => {
  it('inverts over the visible list', () => {
    const current = new Set([FIXTURE[0].path, FIXTURE[1].path]);
    const next = invertSelection(FIXTURE, current);
    expect(next.size).toBe(FIXTURE.length - 2);
    expect(next.has('C:\\dir\\report.txt')).toBe(false);
    expect(next.has('C:\\dir\\docs')).toBe(true);
  });

  it('selects files sharing the cursor extension', () => {
    const next = selectSameExtension(FIXTURE, FIXTURE[0]); // report.txt
    expect([...next]).toEqual(['C:\\dir\\report.txt', 'C:\\dir\\notes.txt']);
  });

  it('extension compare is case-insensitive', () => {
    const next = selectSameExtension(FIXTURE, FIXTURE[2]); // photo.JPG
    expect(next.size).toBe(1);
  });

  it('directories select directories; no cursor selects nothing', () => {
    expect(selectSameExtension(FIXTURE, FIXTURE[6]).size).toBe(1); // docs
    expect(selectSameExtension(FIXTURE, undefined).size).toBe(0);
  });

  it('extensionless files group together and dotfiles form their own group', () => {
    expect(extensionOf('Makefile')).toBe('');
    expect(extensionOf('.gitignore')).toBe('');
    expect(extensionOf('a.b.c')).toBe('c');
    const rows = [entry('Makefile'), entry('README'), entry('.gitignore'), entry('x.txt')];
    expect(selectSameExtension(rows, rows[0]).size).toBe(2); // Makefile + README
    expect(selectSameExtension(rows, rows[2]).size).toBe(1); // dotfile cursor → dotfiles only
  });
});

describe('clipboard formats', () => {
  it('formats names and paths one per line', () => {
    expect(formatNames([FIXTURE[0], FIXTURE[1]])).toBe('report.txt\nnotes.txt');
    expect(formatPaths([FIXTURE[0]])).toBe('C:\\dir\\report.txt');
  });

  it('formats details as TSV name/size/time', () => {
    const ms = Date.UTC(2026, 8, 28, 3, 5);
    const rows: EntryInfo[] = [
      { name: 'a.txt', path: 'C:\\a.txt', isDir: false, size: 2048, modifiedMs: ms },
    ];
    // Expected time is derived from the same Date so the test is TZ-independent.
    const d = new Date(ms);
    const when = `${d.getFullYear()}-09-28 ${String(d.getHours()).padStart(2, '0')}:${String(
      d.getMinutes(),
    ).padStart(2, '0')}`;
    expect(formatDetails(rows)).toBe(`a.txt\t2048\t${when}`);
    expect(formatPathDetails(rows)).toBe(`C:\\a.txt\t2048\t${when}`);
  });

  it('renders empty timestamps for zero/invalid times', () => {
    expect(formatTimestamp(0)).toBe('');
    expect(formatTimestamp(-5)).toBe('');
  });
});

describe('selection file format', () => {
  it('round-trips serialize → parse', () => {
    const json = serializeSelectionFile('C:\\dir', ['a.txt', 'b.txt']);
    expect(parseSelectionFile(json)).toEqual({
      version: 1,
      path: 'C:\\dir',
      names: ['a.txt', 'b.txt'],
    });
  });

  it('rejects wrong version, malformed JSON, and non-basenames', () => {
    expect(() => parseSelectionFile('not json')).toThrow();
    expect(() => parseSelectionFile('{"version":2,"path":"C:","names":[]}')).toThrow(/version/);
    expect(() => parseSelectionFile('{"version":1,"names":[]}')).toThrow();
    expect(() => parseSelectionFile('{"version":1,"path":"C:","names":[42]}')).toThrow();
    for (const bad of ['..', 'a\\b', 'a/b', '.', 'a\0b']) {
      const doc = JSON.stringify({ version: 1, path: 'C:\\dir', names: [bad] });
      expect(() => parseSelectionFile(doc), bad).toThrow(/basename/);
    }
  });

  it('applies saved names by identity and reports missing ones', () => {
    const saved = { version: 1, path: 'C:\\dir', names: ['report.txt', 'GONE.txt'] };
    const applied = applySavedSelection(FIXTURE, saved);
    expect(applied.selected).toEqual(['C:\\dir\\report.txt']);
    expect(applied.missing).toEqual(['GONE.txt']);
  });

  it('name matching on restore is case-insensitive but returns real paths', () => {
    const saved = { version: 1, path: 'C:\\dir', names: ['PHOTO.jpg'] };
    expect(applySavedSelection(FIXTURE, saved).selected).toEqual(['C:\\dir\\photo.JPG']);
  });
});

describe('100k virtualized selection (F2-14 acceptance)', () => {
  const entries: EntryInfo[] = Array.from({ length: 100_000 }, (_, i) => entry(`file-${i}.txt`));

  it('group select + invert + unselect over 100k entries stays interactive', () => {
    const t0 = performance.now();
    // file-10.txt … file-19.txt — exactly ten one-digit-after-1 names
    let sel = selectMatching(entries, 'file-1?.txt', { select: true });
    expect(sel.size).toBe(10);
    sel = selectMatching(entries, 'file-1?*.txt', { select: true });
    expect(sel.size).toBeGreaterThan(0);
    const inv = invertSelection(entries, sel);
    expect(inv.size).toBe(100_000 - sel.size);
    sel = selectMatching(entries, 'file-1*', { select: false }, sel);
    expect(sel.size).toBe(0);
    const elapsed = performance.now() - t0;
    // FER-459: one pass takes ~0.7s alone (debug glob matching over 100k names). The old 2s bound
    // and vitest's default 5s timeout both tripped under a parallel full-suite run. An O(n^2)
    // regression would take minutes, so a wide bound still catches what this test is for.
    expect(elapsed).toBeLessThan(10_000);
  }, 30_000);
});
