import { beforeEach, describe, expect, it } from 'vitest';
import { columns, loadColumnConfig, persistColumnConfig } from './columnsStore.svelte';
import { DEFAULT_COLUMN_CONFIG } from '../columns';

const KEY = 'ferrum.customColumns.v1';

beforeEach(() => {
  columns.resetAll();
});

describe('custom columns store (FER-365)', () => {
  it('starts at the built-in layout with no stored preference', () => {
    expect(globalThis.localStorage?.getItem(KEY)).toBeNull();
    expect(columns.config).toEqual(DEFAULT_COLUMN_CONFIG);
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
    }
    globalThis.localStorage?.removeItem(KEY);
    expect(loadColumnConfig()).toEqual(DEFAULT_COLUMN_CONFIG); // absent key
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
  });
});
