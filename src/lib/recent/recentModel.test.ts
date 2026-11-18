import { describe, expect, it } from 'vitest';
import {
  fromActivity,
  fromRecentFiles,
  matchRecentByName,
  mergeRecent,
  pathKey,
  type RecentItem,
} from './recentModel';

const item = (
  path: string,
  ms: number,
  source: string,
  isDir: boolean | null = false,
): RecentItem => ({
  path,
  name: path.split('\\').pop()!,
  parent: path.slice(0, path.lastIndexOf('\\') + 1),
  isDir,
  lastUsedMs: ms,
  sources: [source],
});

describe('pathKey', () => {
  it('is case-insensitive and ignores separators style and a trailing separator', () => {
    expect(pathKey('C:/Work/A.txt')).toBe(pathKey('c:\\work\\a.TXT'));
    expect(pathKey('C:\\Work\\')).toBe(pathKey('C:\\work'));
    expect(pathKey('C:\\')).toBe('c:\\');
  });
});

describe('mergeRecent (FER-598)', () => {
  it('de-duplicates by path case-insensitively, keeps the newest time and every source', () => {
    const merged = mergeRecent([
      [item('C:\\Docs\\a.txt', 100, 'Ferrum')],
      [item('c:\\docs\\A.TXT', 300, 'Windows')],
      [item('C:\\Docs\\a.txt', 200, 'Downloads')],
    ]);
    expect(merged).toHaveLength(1);
    expect(merged[0].lastUsedMs).toBe(300);
    expect(merged[0].sources).toEqual(['Ferrum', 'Windows', 'Downloads']);
  });

  it('sorts newest first and caps', () => {
    const many = Array.from({ length: 300 }, (_, i) => item(`C:\\f${i}.txt`, i, 'Ferrum'));
    const merged = mergeRecent([many], 200);
    expect(merged).toHaveLength(200);
    expect(merged[0].lastUsedMs).toBe(299);
    expect(merged[199].lastUsedMs).toBe(100);
  });

  it('a known kind wins over an unknown one', () => {
    const merged = mergeRecent([
      [item('C:\\x', 10, 'Windows', null)],
      [item('C:\\x', 5, 'Ferrum', true)],
    ]);
    expect(merged[0].isDir).toBe(true);
  });

  it('does not mutate its inputs', () => {
    const a = item('C:\\a', 1, 'Ferrum');
    mergeRecent([[a], [item('C:\\a', 2, 'Windows')]]);
    expect(a.sources).toEqual(['Ferrum']);
  });
});

describe('conversions', () => {
  it('converts activity items and backend items', () => {
    const a = fromActivity([
      {
        entity: {
          id: '1',
          kind: 'file',
          provider: 'fs',
          volumeId: 'v',
          locator: 'C:\\a\\b.txt',
          parentPath: 'C:\\a',
          displayName: 'b.txt',
          availability: 'available',
        },
        lastUtcMs: 5,
        episodeCount: 1,
        activeDays: 1,
        score: null,
      },
    ]);
    expect(a[0]).toMatchObject({ path: 'C:\\a\\b.txt', isDir: false, sources: ['Ferrum'] });
    const w = fromRecentFiles([
      {
        path: 'C:\\Users\\a\\Downloads\\x.zip',
        name: 'x.zip',
        isDir: false,
        lastUsedMs: 9,
        source: 'new',
        label: 'Downloads',
      },
    ]);
    expect(w[0]).toMatchObject({ sources: ['Downloads'], parent: 'C:\\Users\\a\\Downloads\\' });
  });
});

describe('matchRecentByName', () => {
  const pool = [
    item('C:\\a\\report-2025.docx', 1, 'Windows'),
    item('C:\\a\\annual-report.docx', 3, 'Windows'),
    item('C:\\a\\other.txt', 5, 'Windows'),
  ];
  it('ranks prefix matches before substring matches, ignores case', () => {
    expect(matchRecentByName(pool, 'REPORT', 5).map((i) => i.name)).toEqual([
      'report-2025.docx',
      'annual-report.docx',
    ]);
  });
  it('limits and returns nothing for an empty query', () => {
    expect(matchRecentByName(pool, 'o', 1)).toHaveLength(1);
    expect(matchRecentByName(pool, '  ', 5)).toEqual([]);
  });
});
