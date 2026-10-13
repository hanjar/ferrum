import { describe, expect, it } from 'vitest';
import { formatDate, formatSize, parentOf, rootOf } from './format';

describe('rootOf (FER-495)', () => {
  it('drive paths go to the drive root', () => {
    expect(rootOf('C:\\Users\\you\\x')).toBe('C:\\');
    expect(rootOf('d:/data/a')).toBe('d:\\');
    expect(rootOf('C:\\')).toBe('C:\\');
  });

  it('paths inside an archive resolve to the drive that holds it', () => {
    expect(rootOf('E:\\backup\\a.zip\\inner\\dir')).toBe('E:\\');
  });

  it('UNC paths go to the share root', () => {
    expect(rootOf('\\\\server\\share\\a\\b')).toBe('\\\\server\\share\\');
    expect(rootOf('\\\\server\\share')).toBe('\\\\server\\share\\');
  });

  it('long-path prefix is kept; empty input has no root', () => {
    expect(rootOf('\\\\?\\C:\\a\\b')).toBe('\\\\?\\C:\\');
    expect(rootOf('')).toBe('');
  });
});

describe('formatSize', () => {
  it('formats bytes through units with 3 significant digits', () => {
    expect(formatSize(0)).toBe('0 B');
    expect(formatSize(612)).toBe('612 B');
    expect(formatSize(88_264)).toBe('86.2 KB');
    expect(formatSize(8_400_000)).toBe('8.01 MB');
    expect(formatSize(184 * 1024 ** 2)).toBe('184 MB');
    expect(formatSize(1.6 * 1024 ** 3)).toBe('1.60 GB');
    expect(formatSize(2.4 * 1024 ** 4)).toBe('2.40 TB');
  });

  it('never renders NaN or negatives', () => {
    expect(formatSize(-5)).toBe('—');
    expect(formatSize(Number.NaN)).toBe('—');
  });
});

describe('formatDate', () => {
  it('formats local time as YYYY-MM-DD HH:mm', () => {
    const d = new Date(2026, 8, 25, 16, 20); // local 2026-09-25 16:20
    expect(formatDate(d.getTime())).toBe('2026-09-25 16:20');
  });

  it('empty for unknown timestamps', () => {
    expect(formatDate(0)).toBe('');
    expect(formatDate(Number.NaN)).toBe('');
  });
});

describe('parentOf', () => {
  it('windows paths keep drive root separator', () => {
    expect(parentOf('C:\\Users\\you')).toBe('C:\\Users\\');
    expect(parentOf('C:\\Users\\')).toBe('C:\\');
  });

  it('unix paths', () => {
    expect(parentOf('/home/arif/docs')).toBe('/home/arif/');
    expect(parentOf('/home')).toBe('/');
  });

  it('returns empty at/below root', () => {
    expect(parentOf('C:\\')).toBe('');
    expect(parentOf('/')).toBe('');
  });
});
