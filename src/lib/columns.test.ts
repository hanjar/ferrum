import { describe, expect, it } from 'vitest';
import {
  ALL_COLUMNS,
  COLUMN_LABELS,
  DEFAULT_COLUMN_CONFIG,
  columnLayout,
  effectiveSortSpec,
  extensionText,
  isDefaultColumnConfig,
  parseColumnConfig,
} from './columns';

describe('custom columns engine (FER-365)', () => {
  it('defaults to the built-in Full mode layout', () => {
    expect(DEFAULT_COLUMN_CONFIG).toEqual(['name', 'size', 'modified']);
    // Zero-config behaviour is unchanged: parse of nothing = built-in columns.
    expect(parseColumnConfig(null)).toEqual(DEFAULT_COLUMN_CONFIG);
    expect(parseColumnConfig(undefined)).toEqual(DEFAULT_COLUMN_CONFIG);
    expect(isDefaultColumnConfig(parseColumnConfig(undefined))).toBe(true);
  });

  it('validates persisted configs and recovers from malformed storage', () => {
    expect(parseColumnConfig('nope')).toEqual(DEFAULT_COLUMN_CONFIG); // not an array
    expect(parseColumnConfig({ columns: [] })).toEqual(DEFAULT_COLUMN_CONFIG);
    expect(parseColumnConfig([42])).toEqual(DEFAULT_COLUMN_CONFIG); // unknown id
    expect(parseColumnConfig(['name', 'nope'])).toEqual(DEFAULT_COLUMN_CONFIG);
    expect(parseColumnConfig(['size', 'size', 'name'])).toEqual(DEFAULT_COLUMN_CONFIG); // duplicates
    expect(parseColumnConfig(['size', 'modified'])).toEqual(DEFAULT_COLUMN_CONFIG); // name missing
    expect(parseColumnConfig([])).toEqual(DEFAULT_COLUMN_CONFIG); // empty = no identity
  });

  it('normalises a valid config so Name leads and preserves the chosen order', () => {
    expect(parseColumnConfig(['name', 'extension', 'size', 'modified'])).toEqual([
      'name',
      'extension',
      'size',
      'modified',
    ]);
    // name re-entered later still leads, relative order of the rest kept.
    expect(parseColumnConfig(['size', 'modified', 'name', 'extension'])).toEqual([
      'name',
      'size',
      'modified',
      'extension',
    ]);
    // name-only is a legal minimal config.
    expect(parseColumnConfig(['name'])).toEqual(['name']);
  });

  it('builds the render model: labels, tracks, and the grid template', () => {
    const layout = columnLayout(['name', 'extension', 'size']);
    expect(layout.columns.map((c) => c.id)).toEqual(['name', 'extension', 'size']);
    expect(layout.columns.map((c) => c.label)).toEqual([
      COLUMN_LABELS.name,
      COLUMN_LABELS.extension,
      COLUMN_LABELS.size,
    ]);
    expect(layout.gridTemplate).toBe('minmax(0, 1fr) 72px 72px');
  });

  it('reproduces the built-in grid for the default config, with Comments appended on request', () => {
    expect(columnLayout().gridTemplate).toBe('minmax(0, 1fr) 72px 132px');
    expect(columnLayout(DEFAULT_COLUMN_CONFIG, { comments: true }).gridTemplate).toBe(
      'minmax(0, 1fr) 72px 132px minmax(0, 1.6fr)',
    );
    // The synthetic Comment column is distinguishable from the Name column.
    const withComments = columnLayout(['name'], { comments: true });
    expect(withComments.columns.map((c) => c.id)).toEqual(['name', 'comment']);
    expect(withComments.columns[1].label).toBe('Comment');
  });

  it('falls back to Name sorting while the sorted column is hidden', () => {
    const config = ['name', 'size'] as const;
    // Visible key passes through untouched (direction preserved).
    expect(effectiveSortSpec([...config], { key: 'size', dir: -1 })).toEqual({
      key: 'size',
      dir: -1,
    });
    // Hidden key (modified not configured) falls back to Name, same direction.
    expect(effectiveSortSpec([...config], { key: 'modified', dir: -1 })).toEqual({
      key: 'name',
      dir: -1,
    });
    // Unsorted is not a column and always passes through.
    expect(effectiveSortSpec([...config], { key: 'unsorted', dir: 1 })).toEqual({
      key: 'unsorted',
      dir: 1,
    });
  });

  it('renders extension cell text without the dot; directories show none', () => {
    expect(extensionText('notes.txt', false)).toBe('txt');
    expect(extensionText('archive.tar.gz', false)).toBe('gz');
    expect(extensionText('Makefile', false)).toBe('');
    expect(extensionText('.gitignore', false)).toBe(''); // dotfile: no extension
    expect(extensionText('reports', true)).toBe('');
  });

  it('keeps every SortKey-able field in the allowed set and nothing more', () => {
    // F2-27 scope: exactly the existing SortKey-able columns, no plugin kinds.
    expect(ALL_COLUMNS).toEqual(['name', 'extension', 'size', 'modified']);
  });
});
