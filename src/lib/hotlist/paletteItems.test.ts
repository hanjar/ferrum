import { describe, expect, it } from 'vitest';
import type { HotItem } from './model';
import { flattenHotlist, matchHotlist } from './paletteItems';

const tree: HotItem[] = [
  {
    type: 'submenu',
    title: '&1. Drive',
    items: [
      { type: 'dir', title: '&1. Drive', path: 'C:/n/' },
      {
        type: 'submenu',
        title: '&6. personal',
        items: [{ type: 'dir', title: '&Alice', path: 'C:/n/personal/alice' }],
      },
    ],
  },
  { type: 'separator' },
  { type: 'dir', title: '&Downloads', path: 'C:/dl' },
  { type: 'submenu', title: 'Dummy', items: [] },
  { type: 'command', title: 'ka&ndo', command: 'C:/t/kando.exe', args: ['-x'] },
];

describe('flattenHotlist', () => {
  const flat = flattenHotlist(tree);

  it('flattens in hotlist order, skipping separators and empty submenus', () => {
    expect(flat.map((e) => e.breadcrumb)).toEqual([
      'Drive',
      'Drive › personal › Alice',
      'Downloads',
      'kando',
    ]);
    expect(flat.some((e) => e.breadcrumb.includes('Dummy'))).toBe(false);
  });

  it('removes & mnemonics and carries path / command line as detail', () => {
    expect(flat.some((e) => e.breadcrumb.includes('&'))).toBe(false);
    expect(flat[1].detail).toBe('C:/n/personal/alice');
    expect(flat[3].kind).toBe('command');
    expect(flat[3].detail).toContain('kando.exe');
  });

  it('gives unique ids', () => {
    expect(new Set(flat.map((e) => e.id)).size).toBe(flat.length);
  });

  it('carries the index path that runs a command item', () => {
    expect(flat.map((e) => e.position)).toEqual([[0, 0], [0, 1, 0], [2], [4]]);
  });

  it('marks folders on a network share (FER-467 R8)', () => {
    const net = flattenHotlist([
      { type: 'dir', title: 'Share', path: '\\\\filesrv\\team' },
      { type: 'dir', title: 'Dav', path: '//dav@SSL/DavWWWRoot' },
      { type: 'dir', title: 'Pair', path: 'C:\\a', targetPath: '\\\\other\\b' },
      { type: 'dir', title: 'Local', path: 'C:\\a' },
      { type: 'command', title: 'cmd', command: 'C:\\x.exe', args: [] },
    ]);
    expect(net.map((e) => e.network)).toEqual([true, true, true, false, false]);
    expect(flat.every((e) => !e.network)).toBe(true);
  });
});

describe('matchHotlist', () => {
  const flat = flattenHotlist(tree);
  it('matches breadcrumb and path, case-insensitive, all words', () => {
    expect(matchHotlist(flat, 'ALICE').map((e) => e.breadcrumb)).toEqual([
      'Drive › personal › Alice',
    ]);
    expect(matchHotlist(flat, 'personal alice')).toHaveLength(1);
    expect(matchHotlist(flat, 'c:/dl')).toHaveLength(1);
    expect(matchHotlist(flat, 'zzz')).toHaveLength(0);
  });
  it('never matches the & marker', () => {
    expect(matchHotlist(flat, '&')).toHaveLength(0);
  });
});

describe('matchHotlist typo tolerance (FER-485)', () => {
  const inv: HotItem[] = [
    { type: 'dir', title: '&Downloads', path: 'C:/dl' },
    { type: 'dir', title: 'Invoice 2024', path: 'C:/work/invoice-2024' },
    {
      type: 'submenu',
      title: 'personal',
      items: [{ type: 'dir', title: '&Alice', path: 'C:/p/alice' }],
    },
  ];
  const flat = flattenHotlist(inv);
  it('finds "invocie" -> invoice item', () => {
    expect(matchHotlist(flat, 'invocie').map((e) => e.breadcrumb)).toEqual(['Invoice 2024']);
  });
  it('keeps "alice" first', () => {
    expect(matchHotlist(flat, 'alice')[0].breadcrumb).toBe('personal › Alice');
  });
  it('gives no junk for "zz"', () => {
    expect(matchHotlist(flat, 'zz')).toHaveLength(0);
  });
});
