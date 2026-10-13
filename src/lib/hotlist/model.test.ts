import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  appendItems,
  countItems,
  findAllDirs,
  findDir,
  getAt,
  insertAt,
  isDevicePath,
  LIMITS,
  migrateV1,
  moveTo,
  moveWithin,
  parseDoc,
  removeAt,
  removeMany,
  replaceAt,
  samePath,
  titleParts,
  type DirItem,
  type HotItem,
} from './model';

const here = dirname(fileURLToPath(import.meta.url));
const fixture = (name: string): string =>
  readFileSync(join(here, '../../../contract-fixtures/hotlist', name), 'utf-8');

const dir = (title: string, path = `C:\\${title}`): DirItem => ({ type: 'dir', title, path });
const sub = (title: string, items: HotItem[] = []): HotItem => ({ type: 'submenu', title, items });
const sep = (): HotItem => ({ type: 'separator' });
const titles = (items: HotItem[]) =>
  items.map((i) => (i.type === 'separator' ? '--' : i.title)).join(',');

describe('contract fixture (shared with the Rust parser tests)', () => {
  it('the converted real-world hotlist is a valid v2 document', () => {
    const doc = parseDoc(JSON.parse(fixture('tc-dirmenu.expected.json')));
    expect(doc).not.toBeNull();
    expect(doc!.items).toHaveLength(6);
    expect(findDir(doc!.items, 'c:\\users\\you\\drive\\personal\\charlie')).toEqual([0, 5, 3]);
    expect(findDir(doc!.items, 'C:\\Users\\you\\Downloads\\Telegram Desktop\\')).toEqual([1]);
  });
});

describe('titleParts', () => {
  it('uses the & marker and strips it from the label', () => {
    expect(titleParts('&Telegram')).toEqual({ key: 't', keys: ['t'], label: 'Telegram' });
    expect(titleParts('Fish && Chips')).toEqual({ key: null, keys: [], label: 'Fish & Chips' });
  });

  it('falls back to a single leading digit and drops the redundant number', () => {
    expect(titleParts('2. Project')).toEqual({ key: '2', keys: ['2'], label: 'Project' });
    expect(titleParts('&1. Drive')).toEqual({ key: '1', keys: ['1'], label: 'Drive' });
    expect(titleParts('10. Ten')).toEqual({ key: null, keys: [], label: '10. Ten' });
    expect(titleParts('kando')).toEqual({ key: null, keys: [], label: 'kando' });
  });

  it('keeps both the number and the & letter as accelerators, number shown', () => {
    expect(titleParts('4. &Charlie')).toEqual({ key: '4', keys: ['4', 'c'], label: 'Charlie' });
    expect(titleParts('4. Work &Program')).toEqual({
      key: '4',
      keys: ['4', 'p'],
      label: 'Work Program',
    });
  });

  it('never produces an empty label', () => {
    expect(titleParts('&').label).toBe('&');
    expect(titleParts('1.').label).toBe('1.');
  });
});

describe('parseDoc', () => {
  const ok = (items: unknown) => parseDoc({ version: 2, items });

  it('accepts every item type and rejects wrong versions and unknown types', () => {
    expect(
      ok([
        { type: 'dir', title: 'a', path: 'C:\\a', targetPath: 'D:\\b' },
        { type: 'separator' },
        { type: 'submenu', title: 's' },
        { type: 'command', title: 'c', command: 'C:\\x.exe', args: ['-a'] },
      ]),
    ).not.toBeNull();
    expect(parseDoc({ version: 1, items: [] })).toBeNull();
    expect(parseDoc(null)).toBeNull();
    expect(ok([{ type: 'weird' }])).toBeNull();
    expect(ok('nope')).toBeNull();
  });

  it('enforces title/path/args rules and the depth and count limits', () => {
    expect(ok([{ type: 'dir', title: '', path: 'C:\\a' }])).toBeNull();
    expect(ok([{ type: 'dir', title: 'a', path: '' }])).toBeNull();
    expect(ok([{ type: 'dir', title: 'a', path: '\\\\' }])).toBeNull();
    expect(ok([{ type: 'dir', title: 'a\nb', path: 'C:\\a' }])).toBeNull();
    expect(ok([{ type: 'command', title: 'c', command: 'x', args: ['a\0'] }])).toBeNull();
    expect(
      ok([{ type: 'command', title: 'c', command: 'x', args: Array(LIMITS.args + 1).fill('a') }]),
    ).toBeNull();
    let deep: HotItem = dir('leaf');
    for (let i = 0; i < LIMITS.depth; i++) deep = sub('s', [deep]);
    expect(ok([deep])).not.toBeNull();
    expect(ok([sub('s', [deep])])).toBeNull();
    expect(ok(Array(LIMITS.items + 1).fill({ type: 'separator' }))).toBeNull();
  });
});

describe('migrateV1', () => {
  it('turns the flat list into folder items and drops junk and duplicates', () => {
    const raw = JSON.stringify([
      { name: 'Code', path: 'C:\\code' },
      { name: 'dup', path: 'C:\\code' },
      { name: '', path: 'C:\\x' },
      { path: 'C:\\no-name' },
      'junk',
    ]);
    expect(migrateV1(raw)).toEqual([{ type: 'dir', title: 'Code', path: 'C:\\code' }]);
    expect(migrateV1('{bad')).toEqual([]);
    expect(migrateV1(null)).toEqual([]);
    expect(migrateV1('{"a":1}')).toEqual([]);
  });
});

describe('tree operations', () => {
  const base = (): HotItem[] => [dir('A'), sub('S', [dir('S1'), dir('S2')]), sep(), dir('B')];

  it('inserts, replaces and removes by index path without touching the original', () => {
    const items = base();
    const added = insertAt(items, [1], 1, dir('NEW'))!;
    expect(titles((getAt(added, [1]) as { items: HotItem[] }).items)).toBe('S1,NEW,S2');
    expect(titles(items)).toBe('A,S,--,B');
    expect((getAt(items, [1]) as { items: HotItem[] }).items).toHaveLength(2);
    expect(titles(removeAt(items, [3])!)).toBe('A,S,--');
    expect(getAt(replaceAt(items, [0], dir('Z'))!, [0])).toMatchObject({ title: 'Z' });
    expect(removeAt(items, [9])).toBeNull();
    expect(insertAt(items, [0], 0, dir('x'))).toBeNull(); // [0] is a dir, not a submenu
  });

  it('refuses inserts beyond the depth and count limits', () => {
    let deep: HotItem = dir('leaf');
    for (let i = 0; i < LIMITS.depth; i++) deep = sub('s', [deep]);
    const items = [deep];
    const path = Array(LIMITS.depth).fill(0);
    expect(insertAt(items, path, 0, dir('too deep?'))).not.toBeNull();
    expect(insertAt(items, path, 0, sub('one more level'))).toBeNull();
    const full: HotItem[] = Array(LIMITS.items).fill(sep());
    expect(insertAt(full, [], 0, dir('x'))).toBeNull();
  });

  it('moves within a list and reports the new path', () => {
    const up = moveWithin(base(), [3], -1)!;
    expect(titles(up.items)).toBe('A,S,B,--');
    expect(up.path).toEqual([2]);
    expect(moveWithin(base(), [0], -1)).toBeNull();
    expect(moveWithin(base(), [3], 1)).toBeNull();
    const nested = moveWithin(base(), [1, 0], 1)!;
    expect(nested.path).toEqual([1, 1]);
  });

  it('moves between lists and into submenus without index drift', () => {
    const intoSub = moveTo(base(), [0], [1], 2)!; // A → end of S
    expect(titles(intoSub.items)).toBe('S,--,B');
    expect(titles((getAt(intoSub.items, [0]) as { items: HotItem[] }).items)).toBe('S1,S2,A');
    expect(intoSub.path).toEqual([0, 2]);
    const out = moveTo(base(), [1, 0], [], 0)!; // S1 → top
    expect(titles(out.items)).toBe('S1,A,S,--,B');
    const later = moveTo(base(), [0], [], 4)!; // A → after B
    expect(titles(later.items)).toBe('S,--,B,A');
    const earlier = moveTo(base(), [3], [], 0)!;
    expect(titles(earlier.items)).toBe('B,A,S,--');
  });

  it('never moves a submenu into itself or its descendants', () => {
    const items = [sub('S', [sub('T', [dir('x')])])];
    expect(moveTo(items, [0], [0], 0)).toBeNull();
    expect(moveTo(items, [0], [0, 0], 0)).toBeNull();
    expect(moveTo(items, [0, 0], [0], 0)).not.toBeNull();
  });

  it('refuses a move that would exceed the depth limit', () => {
    let deep: HotItem = dir('leaf');
    for (let i = 0; i < LIMITS.depth; i++) deep = sub('s', [deep]);
    const items = [deep, sub('mover', [dir('m')])];
    expect(moveTo(items, [1], Array(LIMITS.depth).fill(0), 0)).toBeNull();
  });

  it('finds every folder entry for a path and removes them all safely', () => {
    const items = [
      dir('A', 'C:\\x'),
      sub('S', [dir('B', 'c:\\X\\'), dir('C', 'C:\\y')]),
      dir('D', 'C:\\x'),
    ];
    const hits = findAllDirs(items, 'C:\\x');
    expect(hits).toEqual([[0], [1, 0], [2]]);
    expect(titles(removeMany(items, hits))).toBe('S');
    expect(countItems(items)).toBe(5);
  });

  it('appends within the limits and counts what was dropped', () => {
    const full: HotItem[] = Array(LIMITS.items - 1).fill(sep());
    const { items, dropped } = appendItems(full, [dir('fits'), dir('dropped'), dir('dropped2')]);
    expect(items).toHaveLength(LIMITS.items);
    expect(dropped).toBe(2);
  });
});

describe('samePath', () => {
  it('ignores case, slashes and trailing separators', () => {
    expect(samePath('C:\\Users\\Me\\', 'c:/users/me')).toBe(true);
    expect(samePath('C:\\', 'C:')).toBe(true);
    expect(samePath('C:\\a', 'C:\\b')).toBe(false);
  });
});

describe('device paths in folder items (FER-467 H1, mirror of the Rust rule)', () => {
  const bad = [
    '\\\\.\\UNC\\evil\\share',
    '\\\\?\\UNC\\evil\\share',
    '\\\\?\\GLOBALROOT\\Device\\Mup\\evil\\share',
    '\\\\.\\GLOBALROOT\\Device\\Mup\\evil\\share',
    '//?/globalroot/device/mup/evil/share',
    '\\??\\UNC\\evil\\share',
    '\\Device\\Mup\\evil\\share',
    'C:\\x\\..\\Device\\Mup\\evil',
    '\\\\.\\pipe\\x',
    '\\\\.',
    '\\\\?',
    '\\\\?\\C:\\x:stream',
    '\\\\?\\Volume{00000000-0000-0000-0000-000000000000}\\x',
  ];
  const good = [
    'C:\\Users\\me',
    '\\\\server\\share\\x',
    '//server/share',
    '\\\\host@SSL\\DavWWWRoot\\x',
    '\\\\?\\C:\\a very\\long\\path',
    'C:\\Device\\x',
    'C:\\globalroot\\x',
  ];

  it('classifies every spelling like the backend', () => {
    for (const p of bad) expect(isDevicePath(p), p).toBe(true);
    for (const p of good) expect(isDevicePath(p), p).toBe(false);
  });

  it('a document with a device folder (path or target path) is refused', () => {
    for (const p of bad) {
      expect(parseDoc({ version: 2, items: [dir('d', p)] }), p).toBeNull();
      expect(
        parseDoc({
          version: 2,
          items: [{ type: 'dir', title: 'd', path: 'C:\\ok', targetPath: p }],
        }),
        p,
      ).toBeNull();
    }
    for (const p of good) expect(parseDoc({ version: 2, items: [dir('d', p)] }), p).not.toBeNull();
  });
});
