import { describe, expect, it } from 'vitest';
import { isMostlyText } from './packSuggest';

describe('FER-579: isMostlyText', () => {
  it('suggests for text-only selections, case-insensitively', () => {
    expect(isMostlyText(['C:\\d\\a.csv', 'C:\\d\\B.LOG', 'C:\\d\\c.json'])).toBe(true);
    expect(isMostlyText(['/d/a.sql'])).toBe(true);
  });

  it('does not suggest for binaries, folders, extensionless names or mixed selections', () => {
    expect(isMostlyText([])).toBe(false);
    expect(isMostlyText(['C:\\d\\photo.jpg', 'C:\\d\\movie.mp4'])).toBe(false);
    expect(isMostlyText(['C:\\d\\folder'])).toBe(false);
    expect(isMostlyText(['C:\\d\\.gitignore'])).toBe(false);
    expect(isMostlyText(['C:\\d\\a.csv', 'C:\\d\\b.png'])).toBe(false);
  });

  it('tolerates a small non-text share (>= 80% text)', () => {
    const text = ['a.csv', 'b.csv', 'c.txt', 'd.log'].map((n) => 'C:\\d\\' + n);
    expect(isMostlyText([...text, 'C:\\d\\readme.pdf'])).toBe(true);
    expect(isMostlyText([...text, 'C:\\d\\x.pdf', 'C:\\d\\y.pdf'])).toBe(false);
  });
});
