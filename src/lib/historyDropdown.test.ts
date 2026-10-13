import { describe, expect, it } from 'vitest';
import {
  buildHistoryList,
  displayPath,
  frequentRows,
  historyRows,
  pathKey,
} from './historyDropdown';

describe('displayPath / pathKey', () => {
  it('strips the extended-length prefix', () => {
    expect(displayPath('\\\\?\\C:\\Users\\a')).toBe('C:\\Users\\a');
    expect(displayPath('\\\\?\\UNC\\srv\\share\\x')).toBe('\\\\srv\\share\\x');
    expect(displayPath('D:\\x')).toBe('D:\\x');
  });
  it('keys ignore case, slash style and trailing separator', () => {
    expect(pathKey('C:/Users/A/')).toBe(pathKey('\\\\?\\c:\\users\\a'));
    expect(pathKey('C:')).toBe('c:\\');
    expect(pathKey('C:\\')).toBe('c:\\');
  });
});

describe('frequentRows', () => {
  it('sorts by count desc, dedupes spellings, caps and flags archives', () => {
    const rows = frequentRows(
      [
        { path: 'C:\\a', count: 2, lastUtcMs: 1 },
        { path: 'c:\\A\\', count: 9, lastUtcMs: 2 },
        { path: 'D:\\x\\file.zip', count: 5, lastUtcMs: 3 },
        { path: 'D:\\y', count: 5, lastUtcMs: 9 },
        { path: '', count: 99, lastUtcMs: 0 },
      ],
      3,
    );
    expect(rows.map((r) => [r.path, r.count, r.kind])).toEqual([
      ['c:\\A\\', 9, 'dir'],
      ['D:\\y', 5, 'dir'],
      ['D:\\x\\file.zip', 5, 'archive'],
    ]);
  });
  it('limits to 20 by default', () => {
    const many = Array.from({ length: 40 }, (_, i) => ({
      path: `C:\\f${i}`,
      count: i,
      lastUtcMs: i,
    }));
    expect(frequentRows(many)).toHaveLength(20);
  });
});

describe('historyRows', () => {
  it('puts the current folder last and highlights it', () => {
    const rows = historyRows(['C:\\a', 'C:\\b'], 'C:\\c');
    expect(rows.map((r) => r.path)).toEqual(['C:\\a', 'C:\\b', 'C:\\c']);
    expect(rows.map((r) => r.current ?? false)).toEqual([false, false, true]);
  });
  it('collapses consecutive duplicates only', () => {
    const rows = historyRows(['C:\\a', 'c:\\A\\', 'C:\\b', 'C:\\a'], 'C:\\a');
    expect(rows.map((r) => r.path)).toEqual(['c:\\A\\', 'C:\\b', 'C:\\a']);
  });
  it('keeps the newest 30', () => {
    const back = Array.from({ length: 50 }, (_, i) => `C:\\f${i}`);
    const rows = historyRows(back, 'C:\\now');
    expect(rows).toHaveLength(30);
    expect(rows[29]).toMatchObject({ path: 'C:\\now', current: true });
    expect(rows[0].path).toBe('C:\\f21');
  });
  it('falls back to activity_recent (newest first) when the tab has no history', () => {
    const rows = historyRows([], 'C:\\now', ['C:\\new', 'C:\\old']);
    expect(rows.map((r) => r.path)).toEqual(['C:\\old', 'C:\\new', 'C:\\now']);
  });
  it('does not repeat the current folder when it is also the newest recent', () => {
    const rows = historyRows([], 'C:\\now', ['c:\\now\\', 'C:\\old']);
    expect(rows.map((r) => r.path)).toEqual(['C:\\old', 'C:\\now']);
    expect(rows[1].current).toBe(true);
  });
  it('shows archive paths with the archive kind and no prefix', () => {
    const rows = historyRows(['\\\\?\\D:\\t\\file.zip'], 'D:\\t\\file.zip\\Takeout');
    expect(rows[0]).toMatchObject({ path: 'D:\\t\\file.zip', kind: 'archive' });
    expect(rows[1].kind).toBe('dir');
  });
});

describe('buildHistoryList', () => {
  it('rows = top then bottom', () => {
    const l = buildHistoryList({
      frequent: [{ path: 'C:\\a', count: 3, lastUtcMs: 1 }],
      tabBack: ['C:\\b'],
      current: 'C:\\c',
    });
    expect(l.rows.map((r) => r.path)).toEqual(['C:\\a', 'C:\\b', 'C:\\c']);
    expect(l.top).toHaveLength(1);
  });
});

describe('network marker (FER-467 R8)', () => {
  it('marks UNC and WebDAV folders, including the long-path UNC spelling', () => {
    const rows = historyRows(['\\\\?\\UNC\\srv\\share', 'C:\\local', '//dav@SSL/x'], '\\\\srv\\b');
    expect(rows.map((r) => [r.path, r.network])).toEqual([
      ['\\\\srv\\share', true],
      ['C:\\local', false],
      ['//dav@SSL/x', true],
      ['\\\\srv\\b', true],
    ]);
    const top = frequentRows([{ path: '\\\\srv\\t', count: 2, lastUtcMs: 1 }]);
    expect(top[0].network).toBe(true);
  });
});
