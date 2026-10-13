import { describe, expect, it } from 'vitest';
import {
  completeText,
  highlightPrefix,
  refineFromCache,
  splitInput,
  stepIndex,
  uncPathsOf,
} from './addressPath';

describe('addressPath', () => {
  it('splits folder and prefix on either separator', () => {
    expect(splitInput('C:\\Users\\ha')).toEqual({ folder: 'C:\\Users\\', prefix: 'ha' });
    expect(splitInput('C:/Users/')).toEqual({ folder: 'C:/Users/', prefix: '' });
    expect(splitInput('abc')).toEqual({ folder: '', prefix: 'abc' });
  });

  it('completes dirs with a separator, archives and drives correctly', () => {
    expect(completeText('C:\\Us', 'Users', 'dir')).toBe('C:\\Users\\');
    expect(completeText('C:\\x\\a', 'a.zip', 'archive')).toBe('C:\\x\\a.zip');
    expect(completeText('C', 'C:\\', 'drive')).toBe('C:\\');
    expect(completeText('C:/Us', 'Users', 'dir')).toBe('C:/Users/');
  });

  it('bolds the matched prefix keeping the real casing', () => {
    expect(highlightPrefix('Users', 'us')).toEqual([
      { text: 'Us', bold: true },
      { text: 'ers', bold: false },
    ]);
    expect(highlightPrefix('Users', '')).toEqual([{ text: 'Users', bold: false }]);
    expect(highlightPrefix('Users', 'Users')).toEqual([{ text: 'Users', bold: true }]);
  });

  it('steps with wrap-around and handles an empty list', () => {
    expect(stepIndex(-1, 1, 3)).toBe(0);
    expect(stepIndex(-1, -1, 3)).toBe(2);
    expect(stepIndex(2, 1, 3)).toBe(0);
    expect(stepIndex(0, -1, 3)).toBe(2);
    expect(stepIndex(0, 1, 0)).toBe(-1);
  });
});

describe('addressPath hardening helpers', () => {
  it('keeps only UNC paths, de-duplicated', () => {
    expect(uncPathsOf(['C:\\a', '\\\\h1\\s', '//h2/x', '\\\\h1\\s'])).toEqual([
      '\\\\h1\\s',
      '//h2/x',
    ]);
  });

  it('refines a complete cache locally and refuses when it cannot answer', () => {
    const cache = {
      base: 'C:\\',
      folder: 'C:\\Users\\',
      prefix: 'a',
      complete: true,
      items: [{ name: 'Alpha' }, { name: 'alps' }, { name: 'Azure' }],
    };
    expect(refineFromCache(cache, 'C:\\', 'C:\\Users\\al')?.map((i) => i.name)).toEqual([
      'Alpha',
      'alps',
    ]);
    // Shorter prefix, other folder, other base, incomplete: ask the backend.
    expect(refineFromCache(cache, 'C:\\', 'C:\\Users\\')).toBeUndefined();
    expect(refineFromCache(cache, 'C:\\', 'C:\\Other\\al')).toBeUndefined();
    expect(refineFromCache(cache, 'D:\\', 'C:\\Users\\al')).toBeUndefined();
    expect(refineFromCache({ ...cache, complete: false }, 'C:\\', 'C:\\Users\\al')).toBeUndefined();
    expect(refineFromCache(undefined, 'C:\\', 'C:\\Users\\al')).toBeUndefined();
  });
});
