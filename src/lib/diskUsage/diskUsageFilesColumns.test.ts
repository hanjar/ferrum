import { describe, expect, it } from 'vitest';
import {
  DEFAULT_DU_FILES_COLUMNS,
  DU_FILES_COLUMNS_KEY,
  DU_FILES_DEFAULT_NAME_WIDTH,
  DU_FILES_MIN_WIDTHS,
  DU_FILES_RESIZE_STEP,
  clampDuFilesColumnWidth,
  loadDuFilesColumnWidths,
  persistDuFilesColumnWidths,
  sanitizeDuFilesColumnWidths,
  type DuFilesColumnWidths,
} from './diskUsageFilesColumns';

describe('diskUsageFilesColumns', () => {
  it('defines the expected defaults, minimums, and resize step', () => {
    expect(DEFAULT_DU_FILES_COLUMNS).toEqual({
      name: null,
      kind: 90,
      size: 100,
      percent: 70,
    });
    expect(DU_FILES_MIN_WIDTHS).toEqual({
      name: 120,
      kind: 56,
      size: 56,
      percent: 56,
    });
    expect(DU_FILES_DEFAULT_NAME_WIDTH).toBe(400);
    expect(DU_FILES_RESIZE_STEP).toBe(16);
  });

  it('clampDuFilesColumnWidth respects minimums and rounds to integer', () => {
    expect(clampDuFilesColumnWidth('name', 50)).toBe(120);
    expect(clampDuFilesColumnWidth('name', 120.4)).toBe(120);
    expect(clampDuFilesColumnWidth('name', 200.7)).toBe(201);
    expect(clampDuFilesColumnWidth('kind', 30)).toBe(56);
    expect(clampDuFilesColumnWidth('kind', 90)).toBe(90);
    expect(clampDuFilesColumnWidth('size', 10)).toBe(56);
    expect(clampDuFilesColumnWidth('percent', 10)).toBe(56);
  });

  it('sanitizeDuFilesColumnWidths handles bad shapes, corrupt JSON, and partial data', () => {
    expect(sanitizeDuFilesColumnWidths(null)).toEqual(DEFAULT_DU_FILES_COLUMNS);
    expect(sanitizeDuFilesColumnWidths('bad')).toEqual(DEFAULT_DU_FILES_COLUMNS);
    expect(sanitizeDuFilesColumnWidths({})).toEqual(DEFAULT_DU_FILES_COLUMNS);

    // Below minimum falls back to default
    const partial = {
      name: 50, // < 120 -> fallback to null
      kind: 20, // < 56 -> fallback to 90
      size: 150,
      percent: 80,
    };
    expect(sanitizeDuFilesColumnWidths(partial)).toEqual({
      name: null,
      kind: 90,
      size: 150,
      percent: 80,
    });

    // Valid values preserved
    const valid: DuFilesColumnWidths = {
      name: 300,
      kind: 120,
      size: 110,
      percent: 75,
    };
    expect(sanitizeDuFilesColumnWidths(valid)).toEqual(valid);
  });

  it('loads and persists column widths to localStorage safely', () => {
    const memoryStore: Record<string, string> = {};
    const mockStorage: Storage = {
      getItem: (k: string) => memoryStore[k] ?? null,
      setItem: (k: string, v: string) => {
        memoryStore[k] = v;
      },
      removeItem: (k: string) => {
        delete memoryStore[k];
      },
      clear: () => {
        for (const k of Object.keys(memoryStore)) delete memoryStore[k];
      },
      key: () => null,
      length: 0,
    };

    // When empty -> default
    expect(loadDuFilesColumnWidths(mockStorage)).toEqual(DEFAULT_DU_FILES_COLUMNS);

    // When corrupted -> default
    mockStorage.setItem(DU_FILES_COLUMNS_KEY, '{invalid json');
    expect(loadDuFilesColumnWidths(mockStorage)).toEqual(DEFAULT_DU_FILES_COLUMNS);

    // Persist and reload
    const custom: DuFilesColumnWidths = {
      name: 450,
      kind: 110,
      size: 120,
      percent: 65,
    };
    persistDuFilesColumnWidths(custom, mockStorage);
    expect(loadDuFilesColumnWidths(mockStorage)).toEqual(custom);

    // Failing storage is handled gracefully
    const failingStorage = {
      getItem: () => null,
      setItem: () => {
        throw new Error('QuotaExceeded');
      },
    } as unknown as Storage;
    expect(() => persistDuFilesColumnWidths(custom, failingStorage)).not.toThrow();
  });
});
