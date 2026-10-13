import { describe, expect, it } from 'vitest';
import {
  DEFAULT_OPTIONS,
  applyRule,
  buildPreview,
  dateToken,
  executablePairs,
  hasCrossRenames,
  isValidTargetName,
  splitName,
  timeToken,
  type RenameOptions,
} from './multiRename';

const opts = (over: Partial<RenameOptions> = {}): RenameOptions => ({
  ...DEFAULT_OPTIONS,
  ...over,
});

describe('splitName', () => {
  it('splits on the last dot and treats dotfiles as extension-less', () => {
    expect(splitName('archive.tar.gz')).toEqual({ base: 'archive.tar', ext: 'gz' });
    expect(splitName('report.v2.docx')).toEqual({ base: 'report.v2', ext: 'docx' });
    expect(splitName('.gitignore')).toEqual({ base: '.gitignore', ext: '' });
    expect(splitName('README')).toEqual({ base: 'README', ext: '' });
  });
});

describe('applyRule tokens', () => {
  it('keeps the original name with an empty pattern or plain [N]', () => {
    expect(applyRule('a.txt', opts({ pattern: '' }), 1)).toBe('a.txt');
    expect(applyRule('a.txt', opts(), 1)).toBe('a.txt');
  });

  it('substitutes [N] and [E] independently of the original extension', () => {
    expect(applyRule('photo.jpg', opts({ pattern: '[N]_backup' }), 1)).toBe('photo_backup.jpg');
    expect(applyRule('photo.jpg', opts({ pattern: '[N].[E].bak' }), 1)).toBe('photo.jpg.bak');
    expect(applyRule('photo.jpg', opts({ pattern: '[N]-new.[E]' }), 1)).toBe('photo-new.jpg');
  });

  it('counts with start, step, and zero padding', () => {
    const o = opts({ pattern: 'file_[C].[E]', counterStart: 10, counterStep: 5 });
    expect(applyRule('a.txt', o, 1)).toBe('file_10.txt');
    expect(applyRule('b.txt', o, 2)).toBe('file_15.txt');
    const padded = opts({ pattern: '[C]_[N]', counterStart: 3, counterPad: 4 });
    expect(applyRule('x', padded, 1)).toBe('0003_x');
    expect(applyRule('y', padded, 2)).toBe('0004_y');
    expect(applyRule('z', padded, 999)).toBe('1001_z');
  });

  it('uses filename-safe date and time tokens', () => {
    const now = new Date(2026, 8, 28, 14, 5); // local 2026-09-28 14:05
    expect(dateToken(now)).toBe('2026-09-28');
    expect(timeToken(now)).toBe('14-05');
    expect(applyRule('log', opts({ pattern: '[N]-[DATE]' }), 1, now)).toBe('log-2026-09-28');
    expect(applyRule('log', opts({ pattern: '[TIME]-[N]' }), 1, now)).toBe('14-05-log');
  });

  it('applies literal search/replace and rejects a broken regex literally', () => {
    expect(applyRule('Season 01', opts({ pattern: '[N]', find: 'Season', replace: 'S' }), 1)).toBe(
      'S 01',
    );
    expect(
      applyRule('a(b', opts({ pattern: '[N]', find: '(', replace: '-', regex: true }), 1),
    ).toBe('a-b'); // '(' alone is an invalid regex → literal fallback
    expect(
      applyRule('aaa', opts({ pattern: '[N]', find: 'a+', replace: 'X', regex: true }), 1),
    ).toBe('X'); // global regex replaces the whole run
  });

  it('transforms case over the whole resulting name', () => {
    expect(applyRule('Track One.mp3', opts({ caseMode: 'lower' }), 1)).toBe('track one.mp3');
    expect(applyRule('Track One.mp3', opts({ caseMode: 'upper' }), 1)).toBe('TRACK ONE.MP3');
    expect(applyRule('track one', opts({ caseMode: 'firstUpper' }), 1)).toBe('Track one');
  });
});

describe('isValidTargetName', () => {
  it('accepts normal and unicode names', () => {
    for (const ok of ['a.txt', 'ünïcode file (2).md', 'data-2026.backup.tar', '.hidden']) {
      expect(isValidTargetName(ok), ok).toBe(true);
    }
  });

  it('rejects structural garbage, trailing dot/space, and reserved device names', () => {
    for (const bad of [
      '',
      '   ',
      'a/b',
      'a\\b',
      'a<b',
      'a:"b',
      'a|b',
      'a?b',
      'a*b',
      'a\0b',
      'name.',
      'name ',
      '.',
      '..',
      'CON',
      'con.txt',
      'LPT1',
      'com4.txt',
      'NUL.md',
    ]) {
      expect(isValidTargetName(bad), bad).toBe(false);
    }
  });
});

describe('buildPreview', () => {
  it('passes through unchanged names without an issue', () => {
    const items = buildPreview(['same.txt', 'other.bin'], opts());
    expect(items).toEqual([
      { original: 'same.txt', target: 'same.txt' },
      { original: 'other.bin', target: 'other.bin' },
    ]);
  });

  it('flags duplicate targets inside the selection', () => {
    const items = buildPreview(['a.txt', 'b.txt'], opts({ pattern: 'same.[E]' }));
    expect(items[0].issue).toBeUndefined();
    expect(items[1].issue).toBe('duplicate');
  });

  it('allows a rename onto another member’s original name (swap/cycle)', () => {
    const items = buildPreview(['a.txt', 'b.txt'], opts({ pattern: 'b.[E]' }), []);
    // only renaming a.txt→b.txt: b.txt keeps its name, so not yet a swap
    expect(items[0].issue).toBeUndefined();
  });

  it('supports a full two-file swap without flagging duplicates', () => {
    // a→b then b→a via counter-style patterns is executed as a batch in one run
    const names = ['a.txt', 'b.txt'];
    const first = buildPreview(names, opts({ pattern: '[N]__tmp' }));
    expect(first.map((i) => i.target)).toEqual(['a__tmp.txt', 'b__tmp.txt']);
    const swap = buildPreview(['a__tmp.txt', 'b__tmp.txt'], opts({ pattern: '[N]' }));
    expect(hasCrossRenames(swap)).toBe(false);
    expect(hasCrossRenames(buildPreview(names, opts({ pattern: 'swapped_[N]' })))).toBe(false);
  });

  it('flags collisions with entries outside the selection, case-insensitively', () => {
    const others = ['KEEP.MP3', 'taken.txt'];
    expect(buildPreview(['x'], opts({ pattern: 'keep.mp3' }), others)[0].issue).toBe('collision');
    expect(buildPreview(['x'], opts({ pattern: 'TAKEN.txt' }), others)[0].issue).toBe('collision');
    // Renaming a selection member never collides with a fellow member's original.
    expect(
      buildPreview(['a', 'KEEP.mp3'], opts({ pattern: 'new_[N]' }), ['taken.txt']).map(
        (i) => i.issue,
      ),
    ).toEqual([undefined, undefined]);
  });

  it('flags invalid targets from reserved or malformed results', () => {
    const items = buildPreview(['x.txt'], opts({ pattern: 'CON.[E]' }));
    expect(items[0].issue).toBe('invalid');
    expect(buildPreview(['x'], opts({ pattern: '' }), []).length).toBe(1);
  });

  it('handles 1000 files fast and keeps counters aligned with item index', () => {
    const names = Array.from({ length: 1000 }, (_, i) => `old-${i}.txt`);
    const t0 = performance.now();
    const items = buildPreview(names, opts({ pattern: 'new_[C].[E]', counterPad: 4 }));
    expect(performance.now() - t0).toBeLessThan(2000);
    expect(items).toHaveLength(1000);
    expect(items[0].target).toBe('new_0001.txt');
    expect(items[999].target).toBe('new_1000.txt');
    expect(items.every((i) => i.issue === undefined)).toBe(true);
  });
});

describe('executablePairs', () => {
  it('drops unchanged and flagged items, pairing targets with caller paths', () => {
    const paths = ['C:\\d\\a.txt', 'C:\\d\\same.txt', 'C:\\d\\reserved.txt'];
    const preview = buildPreview(
      ['a.txt', 'same.txt', 'reserved.txt'],
      opts({ pattern: 'renamed_[C].[E]' }),
    );
    const pairs = executablePairs(paths, preview);
    expect(pairs).toEqual([
      { from: 'C:\\d\\a.txt', to: 'renamed_1.txt' },
      { from: 'C:\\d\\same.txt', to: 'renamed_2.txt' },
      { from: 'C:\\d\\reserved.txt', to: 'renamed_3.txt' },
    ]);
  });

  it('skips flagged items and keeps clean ones (the dialog gates the run)', () => {
    const preview = buildPreview(['a.txt', 'b.txt'], opts({ pattern: 'same.[E]' }));
    // b.txt holds the duplicate target and is flagged; a.txt stays executable.
    expect(executablePairs(['C:\\a.txt', 'C:\\b.txt'], preview)).toEqual([
      { from: 'C:\\a.txt', to: 'same.txt' },
    ]);
  });
});
