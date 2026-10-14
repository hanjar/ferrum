import { beforeEach, describe, expect, it } from 'vitest';
import {
  columns,
  loadColumnConfig,
  loadColumnWidths,
  loadStoredColumnData,
  persistColumnConfig,
  persistCustomColumns,
} from './columnsStore.svelte';
import {
  DEFAULT_COLUMN_CONFIG,
  DEFAULT_FILE_LIST_COLUMN_WIDTHS,
  FILE_LIST_COLUMN_MINS,
  FILE_LIST_COLUMN_MAX,
} from '../columns';

const KEY = 'ferrum.customColumns.v1';

beforeEach(() => {
  columns.resetAll();
});

describe('custom columns store (FER-365, FER-541)', () => {
  it('starts at the built-in layout with no stored preference', () => {
    expect(globalThis.localStorage?.getItem(KEY)).toBeNull();
    expect(columns.config).toEqual(DEFAULT_COLUMN_CONFIG);
    expect(columns.widths).toEqual(DEFAULT_FILE_LIST_COLUMN_WIDTHS);
    expect(columns.open).toBe(false);
  });

  it('round-trips a config through localStorage', () => {
    columns.setVisible('extension', true);
    columns.setVisible('modified', false);
    columns.move('size', -1); // size before extension
    expect(columns.config).toEqual(['name', 'size', 'extension']);

    // A fresh read of the persisted payload round-trips to the same config.
    expect(loadColumnConfig()).toEqual(['name', 'size', 'extension']);
  });

  it('recovers from malformed storage with the built-in layout', () => {
    for (const bad of ['not json', '{"a":1}', '["size","size","name"]', '42', '[]']) {
      globalThis.localStorage?.setItem(KEY, bad);
      expect(loadColumnConfig()).toEqual(DEFAULT_COLUMN_CONFIG);
      expect(loadColumnWidths()).toEqual(DEFAULT_FILE_LIST_COLUMN_WIDTHS);
    }
    globalThis.localStorage?.removeItem(KEY);
    expect(loadColumnConfig()).toEqual(DEFAULT_COLUMN_CONFIG); // absent key
    expect(loadColumnWidths()).toEqual(DEFAULT_FILE_LIST_COLUMN_WIDTHS);
  });

  it('legacy preferences without width fields load default column widths', () => {
    globalThis.localStorage?.setItem(KEY, JSON.stringify(['name', 'extension', 'size']));
    expect(loadColumnConfig()).toEqual(['name', 'extension', 'size']);
    expect(loadColumnWidths()).toEqual(DEFAULT_FILE_LIST_COLUMN_WIDTHS);
  });

  it('round-trips custom widths through localStorage', () => {
    columns.setWidth('size', 110);
    columns.setWidth('modified', 180);
    columns.setWidth('comment', 250);

    expect(columns.widths.size).toBe(110);
    expect(columns.widths.modified).toBe(180);
    expect(columns.widths.comment).toBe(250);

    const reloaded = loadColumnWidths();
    expect(reloaded.size).toBe(110);
    expect(reloaded.modified).toBe(180);
    expect(reloaded.comment).toBe(250);
    expect(reloaded.extension).toBe(DEFAULT_FILE_LIST_COLUMN_WIDTHS.extension);
  });

  it('recovers corrupted width fields to defaults', () => {
    globalThis.localStorage?.setItem(
      KEY,
      JSON.stringify({
        columns: ['name', 'size', 'modified'],
        widths: {
          size: 'not a number',
          extension: 10, // below min 40
          modified: 999999, // above max 2000
          comment: null,
        },
      }),
    );
    const widths = loadColumnWidths();
    expect(widths.size).toBe(DEFAULT_FILE_LIST_COLUMN_WIDTHS.size); // 72
    expect(widths.extension).toBe(DEFAULT_FILE_LIST_COLUMN_WIDTHS.extension); // 72
    expect(widths.modified).toBe(DEFAULT_FILE_LIST_COLUMN_WIDTHS.modified); // 132
    expect(widths.comment).toBeNull();
  });

  it('clamps width values to defined minimum and maximum', () => {
    columns.setWidth('extension', 10);
    expect(columns.widths.extension).toBe(FILE_LIST_COLUMN_MINS.extension); // 40

    columns.setWidth('size', 3000);
    expect(columns.widths.size).toBe(FILE_LIST_COLUMN_MAX); // 2000

    columns.setWidth('modified', 50);
    expect(columns.widths.modified).toBe(FILE_LIST_COLUMN_MINS.modified); // 90

    columns.setWidth('comment', 30);
    expect(columns.widths.comment).toBe(FILE_LIST_COLUMN_MINS.comment); // 80
  });

  it('resetWidth resets individual column width and reset() resets all widths', () => {
    columns.setWidth('size', 150);
    columns.setWidth('modified', 200);
    expect(columns.widths.size).toBe(150);

    columns.resetWidth('size');
    expect(columns.widths.size).toBe(DEFAULT_FILE_LIST_COLUMN_WIDTHS.size);
    expect(columns.widths.modified).toBe(200);

    columns.reset();
    expect(columns.widths).toEqual(DEFAULT_FILE_LIST_COLUMN_WIDTHS);
    expect(columns.config).toEqual(DEFAULT_COLUMN_CONFIG);
  });

  it('toggles visibility, pins Name, and reappears hidden columns at canonical position', () => {
    columns.setVisible('extension', true);
    // Canonical ALL_COLUMNS slot among the visible rest: ext before size.
    expect(columns.config).toEqual(['name', 'extension', 'size', 'modified']);

    columns.setVisible('extension', false);
    expect(columns.config).toEqual(['name', 'size', 'modified']);

    columns.setVisible('extension', true);
    expect(columns.config).toEqual(['name', 'extension', 'size', 'modified']);

    // Enabling is idempotent, and Name is pinned (hiding it is a no-op).
    columns.setVisible('extension', true);
    columns.setVisible('name', false);
    expect(columns.config).toEqual(['name', 'extension', 'size', 'modified']);
  });

  it('reorders only visible non-name columns and clamps at the edges', () => {
    columns.setVisible('extension', true);
    // [name, extension, size, modified]
    columns.move('size', 1);
    expect(columns.config).toEqual(['name', 'extension', 'modified', 'size']);
    columns.move('size', 1); // already last → clamped
    expect(columns.config).toEqual(['name', 'extension', 'modified', 'size']);
    columns.move('extension', -1); // already first among non-name → clamped
    expect(columns.config).toEqual(['name', 'extension', 'modified', 'size']);
    columns.move('extension', 1);
    expect(columns.config).toEqual(['name', 'modified', 'extension', 'size']);
    // Moving a hidden column is a no-op.
    columns.setVisible('modified', false);
    columns.move('modified', -1);
    expect(columns.config).toEqual(['name', 'extension', 'size']);
  });

  it('reset() restores the built-in layout and persists it', () => {
    columns.setVisible('extension', true);
    columns.reset();
    expect(columns.config).toEqual(DEFAULT_COLUMN_CONFIG);
    expect(JSON.parse(globalThis.localStorage?.getItem(KEY) ?? 'null')).toEqual(
      DEFAULT_COLUMN_CONFIG,
    );
  });

  it('survives unavailable storage (persist is best-effort)', () => {
    expect(() =>
      persistColumnConfig(['name'], {
        setItem: () => {
          throw new Error('quota');
        },
        getItem: () => null,
        removeItem: () => {},
        clear: () => {},
        key: () => null,
        length: 0,
      } as Storage),
    ).not.toThrow();

    expect(() =>
      persistCustomColumns(['name'], DEFAULT_FILE_LIST_COLUMN_WIDTHS, {
        setItem: () => {
          throw new Error('quota');
        },
        getItem: () => null,
        removeItem: () => {},
        clear: () => {},
        key: () => null,
        length: 0,
      } as Storage),
    ).not.toThrow();

    const throwingStorage = {
      getItem: () => {
        throw new Error('SecurityError: Access denied');
      },
      setItem: () => {
        throw new Error('SecurityError: Access denied');
      },
      removeItem: () => {},
      clear: () => {},
      key: () => null,
      length: 0,
    } as Storage;

    expect(() => loadStoredColumnData(throwingStorage)).not.toThrow();
    const loaded = loadStoredColumnData(throwingStorage);
    expect(loaded.config).toEqual(DEFAULT_COLUMN_CONFIG);
    expect(loaded.widths).toEqual(DEFAULT_FILE_LIST_COLUMN_WIDTHS);
  });
});
