import { describe, expect, it } from 'vitest';
import { kindOf } from './kind';

describe('kindOf', () => {
  it('directories are always dir', () => {
    expect(kindOf({ isDir: true, name: 'archive.zip' })).toBe('dir');
  });

  it('maps by extension case-insensitively', () => {
    expect(kindOf({ isDir: false, name: 'main.rs' })).toBe('code');
    expect(kindOf({ isDir: false, name: 'APP.TSX' })).toBe('code');
    expect(kindOf({ isDir: false, name: 'hero.png' })).toBe('image');
    expect(kindOf({ isDir: false, name: 'turntable.MP4' })).toBe('media');
    expect(kindOf({ isDir: false, name: 'bundle.7z' })).toBe('archive');
    expect(kindOf({ isDir: false, name: 'dump.csv' })).toBe('data');
    expect(kindOf({ isDir: false, name: 'README.md' })).toBe('docs');
  });

  it('unknown and extensionless fall back to text', () => {
    expect(kindOf({ isDir: false, name: 'Makefile' })).toBe('text');
    expect(kindOf({ isDir: false, name: 'weird.zzzx' })).toBe('text');
    expect(kindOf({ isDir: false, name: '.gitignore' })).toBe('text');
  });
});
