import { describe, expect, it } from 'vitest';
import { crumbsOf, layoutCrumbs, normalizeDrivePath, type CrumbMetrics } from './crumbs';

describe('crumbsOf', () => {
  it('drive segment is the drive root with a trailing backslash', () => {
    expect(crumbsOf('C:\\Users\\me')).toEqual([
      { label: 'C:', path: 'C:\\' },
      { label: 'Users', path: 'C:\\Users' },
      { label: 'me', path: 'C:\\Users\\me' },
    ]);
  });

  it('a bare drive root yields a single crumb that stays C:\\ (no /C: cascade)', () => {
    expect(crumbsOf('C:\\')).toEqual([{ label: 'C:', path: 'C:\\' }]);
    expect(crumbsOf('C:')).toEqual([{ label: 'C:', path: 'C:\\' }]);
    expect(crumbsOf('/C:')).toEqual([{ label: 'C:', path: 'C:\\' }]);
  });

  it('forward-slash drive paths keep their separator', () => {
    expect(crumbsOf('d:/Backup/x')).toEqual([
      { label: 'D:', path: 'd:/' },
      { label: 'Backup', path: 'd:/Backup' },
      { label: 'x', path: 'd:/Backup/x' },
    ]);
  });

  it('POSIX paths are unchanged', () => {
    expect(crumbsOf('/home/me')).toEqual([
      { label: 'home', path: '/home' },
      { label: 'me', path: '/home/me' },
    ]);
    expect(crumbsOf('/')).toEqual([]);
  });

  it('UNC paths keep server+share as one root crumb', () => {
    expect(crumbsOf('\\\\srv\\share\\dir')).toEqual([
      { label: '\\\\srv\\share', path: '\\\\srv\\share\\' },
      { label: 'dir', path: '\\\\srv\\share\\dir' },
    ]);
  });

  it('empty path has no crumbs', () => {
    expect(crumbsOf('')).toEqual([]);
  });

  it('FER-53: archive paths generate correct segments with archive markers', () => {
    const crumbs = crumbsOf('C:\\data\\backup.zip\\docs\\2026');
    expect(crumbs).toEqual([
      { label: 'C:', path: 'C:\\' },
      { label: 'data', path: 'C:\\data' },
      { label: 'backup.zip', path: 'C:\\data\\backup.zip', isArchive: true, inArchive: true },
      { label: 'docs', path: 'C:\\data\\backup.zip\\docs', inArchive: true },
      { label: '2026', path: 'C:\\data\\backup.zip\\docs\\2026', inArchive: true },
    ]);
  });

  it('FER-53: archive root path marks archive file segment', () => {
    const crumbs = crumbsOf('D:\\archives\\pack.7z');
    expect(crumbs).toEqual([
      { label: 'D:', path: 'D:\\' },
      { label: 'archives', path: 'D:\\archives' },
      { label: 'pack.7z', path: 'D:\\archives\\pack.7z', isArchive: true, inArchive: true },
    ]);
  });

  it('FER-53: supports .rar archive segmentation and does not false-mark extensions like .zip.bak', () => {
    const rarCrumbs = crumbsOf('/home/user/files.rar/sub');
    expect(rarCrumbs).toEqual([
      { label: 'home', path: '/home' },
      { label: 'user', path: '/home/user' },
      { label: 'files.rar', path: '/home/user/files.rar', isArchive: true, inArchive: true },
      { label: 'sub', path: '/home/user/files.rar/sub', inArchive: true },
    ]);

    const bakCrumbs = crumbsOf('C:\\data\\backup.zip.bak\\sub');
    expect(bakCrumbs[1].isArchive).toBeUndefined();
    expect(bakCrumbs[1].inArchive).toBeUndefined();
  });
});

describe('normalizeDrivePath', () => {
  it('repairs legacy drive-relative and slash-prefixed values', () => {
    expect(normalizeDrivePath('C:')).toBe('C:\\');
    expect(normalizeDrivePath('/C:')).toBe('C:\\');
    expect(normalizeDrivePath('/c:')).toBe('c:\\');
    expect(normalizeDrivePath('/C:\\Users')).toBe('C:\\Users');
    expect(normalizeDrivePath('/D:/x')).toBe('D:/x');
  });

  it('leaves everything else untouched', () => {
    for (const p of ['C:\\', 'C:\\a', 'C:foo', '/home/me', '/', '\\\\srv\\share', '', 'relative']) {
      expect(normalizeDrivePath(p)).toBe(p);
    }
  });
});

describe('layoutCrumbs (FER-491)', () => {
  const P = (s: string) => s.split('/').join(String.fromCharCode(92));
  const W = (label: string) => label.length * 7 + 10;
  const metrics = (path: string, available: number): CrumbMetrics => ({
    widths: crumbsOf(path).map((c) => W(c.label)),
    sep: 14,
    more: 24,
    available,
  });

  it('short path: everything is shown, no ellipsis button', () => {
    const l = layoutCrumbs(metrics(P('C:/Users/me'), 400));
    expect(l).toEqual({ shown: [0, 1, 2], hidden: [] });
  });

  it('long path: root + tail + last stay, the middle folds into one ellipsis', () => {
    const path = P('C:/Users/demo/Projects/clients/acme-corp/2026/Q3-reports/final-deliverables');
    const m = metrics(path, 300);
    const l = layoutCrumbs(m);
    const n = m.widths.length;
    expect(l.shown[0]).toBe(0);
    expect(l.shown[l.shown.length - 1]).toBe(n - 1);
    expect(l.hidden.length).toBeGreaterThan(0);
    // hidden is a contiguous middle run, shown + hidden covers every crumb once
    expect([...l.shown, ...l.hidden].sort((a, b) => a - b)).toEqual(
      Array.from({ length: n }, (_, i) => i),
    );
    expect(l.hidden).toEqual(l.hidden.map((_, i) => l.hidden[0] + i));
    const used =
      l.shown.reduce((a, i) => a + m.widths[i], 0) + (l.shown.length - 1 + 1) * m.sep + m.more;
    expect(used).toBeLessThanOrEqual(300);
    expect(l.lastMax).toBeUndefined();
  });

  it('keeps as many right-hand segments as fit (more room, fewer hidden)', () => {
    const path = P('C:/a/bb/ccc/dddd/eeeee/ffffff/ggggggg');
    const narrow = layoutCrumbs(metrics(path, 220));
    const wide = layoutCrumbs(metrics(path, 330));
    expect(wide.hidden.length).toBeLessThan(narrow.hidden.length);
  });

  it('very long active folder: cut with an ellipsis, everything else in the dropdown', () => {
    const path = P('C:/Users/') + 'x'.repeat(80);
    const m = metrics(path, 200);
    const l = layoutCrumbs(m);
    expect(l.shown).toEqual([2]);
    expect(l.hidden).toEqual([0, 1]);
    expect(l.lastMax).toBe(200 - m.more - m.sep);
  });

  it('root does not fit beside the last segment: root joins the dropdown', () => {
    const m: CrumbMetrics = { widths: [40, 30, 100], sep: 10, more: 20, available: 140 };
    expect(layoutCrumbs(m)).toEqual({ shown: [2], hidden: [0, 1] });
  });

  it('UNC root counts as one crumb', () => {
    const path = P('//srv/share/a/b/c/d/e');
    const l = layoutCrumbs(metrics(path, 200));
    expect(l.shown[0]).toBe(0);
    expect(l.shown[l.shown.length - 1]).toBe(crumbsOf(path).length - 1);
    expect(l.hidden.length).toBeGreaterThan(0);
  });

  it('path inside an archive: the archive segment is an ordinary crumb', () => {
    const path = P('C:/Backups/2026/october/file.zip/docs/reports/final');
    const c = crumbsOf(path);
    expect(c.map((x) => x.label)).toContain('file.zip');
    const l = layoutCrumbs(metrics(path, 260));
    expect(l.shown[l.shown.length - 1]).toBe(c.length - 1);
    expect([...l.shown, ...l.hidden]).toHaveLength(c.length);
  });

  it('root only / single crumb / empty', () => {
    expect(layoutCrumbs(metrics(P('C:/'), 200))).toEqual({ shown: [0], hidden: [] });
    expect(layoutCrumbs({ widths: [500], sep: 14, more: 24, available: 120 })).toEqual({
      shown: [0],
      hidden: [],
      lastMax: 120,
    });
    expect(layoutCrumbs({ widths: [], sep: 14, more: 24, available: 120 })).toEqual({
      shown: [],
      hidden: [],
    });
  });
});
