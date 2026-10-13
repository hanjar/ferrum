import { describe, expect, it } from 'vitest';
import { cycle, firstSelectable, lastSelectable, matchKey, rowsFor, step } from './menuRows';
import type { HotItem } from './model';

const items: HotItem[] = [
  { type: 'submenu', title: '&1. Drive', items: [{ type: 'dir', title: 'x', path: 'C:\\x' }] },
  { type: 'separator' },
  { type: 'dir', title: '&Telegram', path: 'C:\\t' },
  { type: 'dir', title: '&Tools', path: 'C:\\tools' },
  { type: 'command', title: 'kando', command: 'C:\\k.exe', args: [] },
];
const on = { canAdd: true, canRemove: false, readOnly: false };

describe('rowsFor', () => {
  it('adds the footer actions only at the top level, after a separator', () => {
    const rows = rowsFor(items, [], on);
    expect(rows.map((r) => r.kind)).toEqual([
      'item',
      'separator',
      'item',
      'item',
      'item',
      'separator',
      'action',
      'action',
      'action',
    ]);
    expect(rowsFor(items, [0], on).every((r) => r.kind !== 'action')).toBe(true);
    const actions = rows.filter((r) => r.kind === 'action');
    expect(actions.map((a) => a.kind === 'action' && a.disabled)).toEqual([false, true, false]);
  });

  it('read-only disables add and remove but keeps Configure', () => {
    const rows = rowsFor(items, [], { canAdd: true, canRemove: true, readOnly: true });
    const disabled = rows.flatMap((r) => (r.kind === 'action' ? [r.disabled] : []));
    expect(disabled).toEqual([true, true, false]);
  });

  it('an empty hotlist shows only the actions (no leading separator)', () => {
    expect(rowsFor([], [], on)[0].kind).toBe('action');
  });
});

describe('navigation helpers', () => {
  const rows = rowsFor(items, [], on);

  it('skips separators and disabled rows, wrapping around', () => {
    expect(firstSelectable(rows)).toBe(0);
    expect(step(rows, 0, 1)).toBe(2);
    expect(step(rows, 0, -1)).toBe(8); // wraps to Configure
    expect(step(rows, 4, 1)).toBe(6); // skips the separator
    expect(step(rows, 6, 1)).toBe(8); // skips the disabled Remove
    expect(lastSelectable(rows)).toBe(8);
  });

  it('matches accelerators case-insensitively and cycles through duplicates', () => {
    expect(matchKey(rows, '1')).toEqual([0]);
    expect(matchKey(rows, 'T')).toEqual([2, 3]);
    expect(matchKey(rows, 'z')).toEqual([]);
    expect(matchKey(rows, '+')).toEqual([6]);
    expect(matchKey(rows, '-')).toEqual([]); // Remove disabled
    expect(cycle([2, 3], 2)).toBe(3);
    expect(cycle([2, 3], 3)).toBe(2);
    expect(cycle([2, 3], -1)).toBe(2);
  });
});
