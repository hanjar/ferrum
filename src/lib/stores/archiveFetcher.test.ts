import { describe, expect, it } from 'vitest';
import { isArchiveFile, splitArchiveLocation } from './archiveFetcher';

describe('splitArchiveLocation (FER-369)', () => {
  it('splits archive-root and member paths into disk file + member', () => {
    expect(splitArchiveLocation('C:\\dir\\bundle.zip')).toEqual({
      archivePath: 'C:\\dir\\bundle.zip',
      memberPath: '',
    });
    expect(splitArchiveLocation('C:\\dir\\bundle.zip\\members\\sub')).toEqual({
      archivePath: 'C:\\dir\\bundle.zip',
      memberPath: 'members\\sub',
    });
    // POSIX separators and case-insensitive extensions behave the same.
    expect(splitArchiveLocation('/tmp/BUNDLE.ZIP/docs')).toEqual({
      archivePath: '/tmp/BUNDLE.ZIP',
      memberPath: 'docs',
    });
  });

  it('supports every browsable archive extension at the boundary', () => {
    for (const ext of ['.zip', '.rar', '.7z']) {
      const loc = splitArchiveLocation(`C:\\a\\x${ext}\\m`);
      expect(loc?.archivePath).toBe(`C:\\a\\x${ext}`);
    }
  });

  it('returns null for plain disk locations', () => {
    expect(splitArchiveLocation('C:\\')).toBeNull();
    expect(splitArchiveLocation('C:\\dir\\notes.txt')).toBeNull();
  });

  it('does not mistake extension-lookalikes for the virtual boundary', () => {
    // The archive extension must be followed by a separator to count.
    expect(splitArchiveLocation('C:\\data.zip.bak\\x')).toBeNull();
    expect(splitArchiveLocation('C:\\my.zip files\\x')).toBeNull();
    expect(splitArchiveLocation('C:\\x.7zip\\inner')).toBeNull(); // .7z is followed by 'i'
    expect(splitArchiveLocation('C:\\notes.ziptxt')).toBeNull();
  });

  it('keeps isArchiveFile semantics for whole-path archive names', () => {
    expect(isArchiveFile('C:\\a.zip')).toBe(true);
    expect(isArchiveFile('C:\\a.ZIP')).toBe(true);
    expect(isArchiveFile('C:\\a.tar')).toBe(false);
  });
});
