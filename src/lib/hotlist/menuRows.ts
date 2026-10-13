/**
 * Rows of the Ctrl+D popup (FER-458): the hotlist tree plus the fixed footer actions, with the
 * accelerator rules and the keyboard-navigation helpers kept pure so they can be unit tested.
 */
import { titleParts, type HotItem } from './model';

export type ActionId = 'add' | 'remove' | 'configure';

export type Row =
  | { kind: 'separator' }
  | {
      kind: 'item';
      item: Exclude<HotItem, { type: 'separator' }>;
      /** Index path inside the hotlist tree. */
      path: number[];
      key: string | null;
      keys: string[];
      label: string;
    }
  | {
      kind: 'action';
      id: ActionId;
      label: string;
      glyph: string;
      key: string;
      disabled: boolean;
    };

/** Rows for the list at `path` (empty = top level, which also gets the footer actions). */
export function rowsFor(
  items: HotItem[],
  path: number[],
  opts: { canAdd: boolean; canRemove: boolean; readOnly: boolean },
): Row[] {
  const rows: Row[] = items.map((item, i) => {
    if (item.type === 'separator') return { kind: 'separator' as const };
    const { key, keys, label } = titleParts(item.title);
    return { kind: 'item' as const, item, path: [...path, i], key, keys, label };
  });
  if (path.length > 0) return rows;
  if (rows.length > 0) rows.push({ kind: 'separator' });
  rows.push(
    {
      kind: 'action',
      id: 'add',
      label: 'Add current folder',
      glyph: '+',
      key: '+',
      disabled: !opts.canAdd || opts.readOnly,
    },
    {
      kind: 'action',
      id: 'remove',
      label: 'Remove current folder',
      glyph: '−',
      key: '-',
      disabled: !opts.canRemove || opts.readOnly,
    },
    { kind: 'action', id: 'configure', label: 'Configure…', glyph: '⚙', key: ',', disabled: false },
  );
  return rows;
}

export const isSelectable = (row: Row | undefined): boolean =>
  row !== undefined && row.kind !== 'separator' && !(row.kind === 'action' && row.disabled);

/** Next selectable index from `from` in `dir`, wrapping; `-1` when nothing is selectable. */
export function step(rows: Row[], from: number, dir: 1 | -1): number {
  const n = rows.length;
  for (let k = 1; k <= n; k++) {
    const i = (((from + dir * k) % n) + n) % n;
    if (isSelectable(rows[i])) return i;
  }
  return -1;
}

export function firstSelectable(rows: Row[]): number {
  return rows.findIndex((r) => isSelectable(r));
}

export function lastSelectable(rows: Row[]): number {
  for (let i = rows.length - 1; i >= 0; i--) if (isSelectable(rows[i])) return i;
  return -1;
}

/**
 * Accelerator lookup for a typed character: indices of the selectable rows whose key matches,
 * in list order. The caller activates a lone match and cycles through several.
 */
export function matchKey(rows: Row[], char: string): number[] {
  const k = char.toLowerCase();
  const hits: number[] = [];
  rows.forEach((r, i) => {
    if (!isSelectable(r)) return;
    const keys = r.kind === 'item' ? r.keys : r.kind === 'action' ? [r.key] : [];
    if (keys.includes(k)) hits.push(i);
  });
  return hits;
}

/** After `current`, the next index in `hits` (wrapping); the first one when none is current. */
export function cycle(hits: number[], current: number): number {
  return hits.find((i) => i > current) ?? hits[0];
}
