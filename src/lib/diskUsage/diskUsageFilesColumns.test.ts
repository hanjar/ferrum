import { describe, expect, it } from 'vitest';
import {
  DEFAULT_DU_FILES_COLUMNS,
  DU_FILES_COLUMNS_KEY,
  DU_FILES_DEFAULT_NAME_WIDTH,
  DU_FILES_MAX_WIDTH,
  DU_FILES_MIN_WIDTHS,
  DU_FILES_RESIZE_STEP,
  clampDuFilesColumnWidth,
  loadDuFilesColumnWidths,
  persistDuFilesColumnWidths,
  sanitizeDuFilesColumnWidths,
  type DuFilesColumnWidths,
} from './diskUsageFilesColumns';

describe('diskUsageFilesColumns', () => {
  it('defines the expected defaults, minimums, maximum, and resize step', () => {
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
    expect(DU_FILES_MAX_WIDTH).toBe(2000);
    expect(DU_FILES_DEFAULT_NAME_WIDTH).toBe(400);
    expect(DU_FILES_RESIZE_STEP).toBe(16);
  });

  it('clampDuFilesColumnWidth respects minimums, maximums, and rounds to integer', () => {
    expect(clampDuFilesColumnWidth('name', 50)).toBe(120);
    expect(clampDuFilesColumnWidth('name', 120.4)).toBe(120);
    expect(clampDuFilesColumnWidth('name', 200.7)).toBe(201);
    expect(clampDuFilesColumnWidth('name', 5000)).toBe(2000);
    expect(clampDuFilesColumnWidth('kind', 30)).toBe(56);
    expect(clampDuFilesColumnWidth('kind', 90)).toBe(90);
    expect(clampDuFilesColumnWidth('kind', 1e9)).toBe(2000);
    expect(clampDuFilesColumnWidth('size', 10)).toBe(56);
    expect(clampDuFilesColumnWidth('percent', 10)).toBe(56);
  });

  it('sanitizeDuFilesColumnWidths handles bad shapes, corrupt JSON, extreme values, and partial data', () => {
    expect(sanitizeDuFilesColumnWidths(null)).toEqual(DEFAULT_DU_FILES_COLUMNS);
    expect(sanitizeDuFilesColumnWidths('bad')).toEqual(DEFAULT_DU_FILES_COLUMNS);
    expect(sanitizeDuFilesColumnWidths({})).toEqual(DEFAULT_DU_FILES_COLUMNS);

    // Below minimum or above maximum falls back to default
    const extreme = {
      name: 50, // < 120 -> fallback to null
      kind: 1e9, // > 2000 -> fallback to 90
      size: -100, // < 56 -> fallback to 100
      percent: 'not-a-number', // string -> fallback to 70
    };
    expect(sanitizeDuFilesColumnWidths(extreme)).toEqual({
      name: null,
      kind: 90,
      size: 100,
      percent: 70,
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

    // Failing getItem storage is handled gracefully
    const failingGetStorage = {
      getItem: () => {
        throw new Error('SecurityError: Blocked storage');
      },
      setItem: () => {},
    } as unknown as Storage;
    expect(loadDuFilesColumnWidths(failingGetStorage)).toEqual(DEFAULT_DU_FILES_COLUMNS);

    // Failing setItem storage is handled gracefully
    const failingSetStorage = {
      getItem: () => null,
      setItem: () => {
        throw new Error('QuotaExceeded');
      },
    } as unknown as Storage;
    expect(() => persistDuFilesColumnWidths(custom, failingSetStorage)).not.toThrow();
  });
});
