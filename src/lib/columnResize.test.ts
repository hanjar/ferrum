import { describe, expect, it } from 'vitest';
import { createColumnResize } from './columnResize';

describe('columnResize helper', () => {
  const helper = createColumnResize<'colA' | 'colB' | 'colC'>({
    keys: ['colA', 'colB', 'colC'],
    defaults: { colA: null, colB: 100, colC: 200 },
    mins: { colA: 120, colB: 60 },
    defaultMin: 50,
    max: 2000,
    step: 16,
    storageKey: 'test.columns.key',
  });

  it('exposes configuration properties and limits', () => {
    expect(helper.keys).toEqual(['colA', 'colB', 'colC']);
    expect(helper.defaults).toEqual({ colA: null, colB: 100, colC: 200 });
    expect(helper.step).toBe(16);
    expect(helper.storageKey).toBe('test.columns.key');
    expect(helper.getMin('colA')).toBe(120);
    expect(helper.getMin('colB')).toBe(60);
    expect(helper.getMin('colC')).toBe(50); // fallback to defaultMin
    expect(helper.getMax('colA')).toBe(2000);
    expect(helper.getMax('colB')).toBe(2000);
  });

  describe('clamp', () => {
    it('clamps values below min to min', () => {
      expect(helper.clamp('colA', 40)).toBe(120);
      expect(helper.clamp('colB', 10)).toBe(60);
      expect(helper.clamp('colC', 20)).toBe(50);
    });

    it('clamps values above max to max', () => {
      expect(helper.clamp('colA', 5000)).toBe(2000);
      expect(helper.clamp('colB', 1e9)).toBe(2000);
    });

    it('rounds floating point values to nearest integer', () => {
      expect(helper.clamp('colA', 150.4)).toBe(150);
      expect(helper.clamp('colA', 150.6)).toBe(151);
    });

    it('handles non-finite values safely', () => {
      expect(helper.clamp('colA', NaN)).toBe(120);
      expect(helper.clamp('colB', Infinity)).toBe(60);
    });
  });

  describe('sanitize', () => {
    it('returns defaults for non-objects or null', () => {
      expect(helper.sanitize(null)).toEqual(helper.defaults);
      expect(helper.sanitize('invalid')).toEqual(helper.defaults);
      expect(helper.sanitize(42)).toEqual(helper.defaults);
    });

    it('falls back to default for values < min or > max or invalid types', () => {
      const corrupted = {
        colA: 50, // < 120 -> null
        colB: 1e9, // > 2000 -> 100
        colC: 'not-a-number', // string -> 200
      };
      expect(helper.sanitize(corrupted)).toEqual({
        colA: null,
        colB: 100,
        colC: 200,
      });

      const nanValues = {
        colA: NaN,
        colB: -20,
        colC: {},
      };
      expect(helper.sanitize(nanValues)).toEqual({
        colA: null,
        colB: 100,
        colC: 200,
      });
    });

    it('preserves valid in-range values and rounds them', () => {
      const valid = {
        colA: 350.2,
        colB: 150,
        colC: 500,
      };
      expect(helper.sanitize(valid)).toEqual({
        colA: 350,
        colB: 150,
        colC: 500,
      });
    });

    it('handles null appropriately', () => {
      // colA default is null, so null is preserved
      // colB default is 100, so null falls back to 100
      expect(helper.sanitize({ colA: null, colB: null, colC: 300 })).toEqual({
        colA: null,
        colB: 100,
        colC: 300,
      });
    });
  });

  describe('load and persist', () => {
    it('returns defaults when storage is empty or invalid', () => {
      const mem: Record<string, string> = {};
      const storage: Storage = {
        getItem: (k: string) => mem[k] ?? null,
        setItem: (k: string, v: string) => {
          mem[k] = v;
        },
        removeItem: (k: string) => {
          delete mem[k];
        },
        clear: () => {},
        key: () => null,
        length: 0,
      };

      expect(helper.load(storage)).toEqual(helper.defaults);

      mem['test.columns.key'] = '{corrupt json';
      expect(helper.load(storage)).toEqual(helper.defaults);
    });

    it('persists and loads valid widths', () => {
      const mem: Record<string, string> = {};
      const storage: Storage = {
        getItem: (k: string) => mem[k] ?? null,
        setItem: (k: string, v: string) => {
          mem[k] = v;
        },
        removeItem: () => {},
        clear: () => {},
        key: () => null,
        length: 0,
      };

      const custom = { colA: 400, colB: 180, colC: 220 };
      helper.persist(custom, storage);
      expect(helper.load(storage)).toEqual(custom);
    });

    it('catches errors when storage.getItem throws (e.g. SecurityError)', () => {
      const throwingStorage = {
        getItem: () => {
          throw new Error('Access denied');
        },
        setItem: () => {},
      } as unknown as Storage;

      expect(helper.load(throwingStorage)).toEqual(helper.defaults);
    });

    it('catches errors when storage.setItem throws (e.g. QuotaExceeded)', () => {
      const failingStorage = {
        getItem: () => null,
        setItem: () => {
          throw new Error('QuotaExceeded');
        },
      } as unknown as Storage;

      expect(() =>
        helper.persist({ colA: 300, colB: 100, colC: 200 }, failingStorage),
      ).not.toThrow();
    });
  });

  describe('reset helpers', () => {
    it('reset returns defaults', () => {
      expect(helper.reset()).toEqual(helper.defaults);
    });

    it('resetKey returns the specific column default', () => {
      expect(helper.resetKey('colA')).toBeNull();
      expect(helper.resetKey('colB')).toBe(100);
      expect(helper.resetKey('colC')).toBe(200);
    });

    it('resetCol resets a single column in a width record', () => {
      const current = { colA: 300, colB: 150, colC: 400 };
      expect(helper.resetCol(current, 'colB')).toEqual({
        colA: 300,
        colB: 100,
        colC: 400,
      });
      expect(helper.resetCol(current, 'colA')).toEqual({
        colA: null,
        colB: 150,
        colC: 400,
      });
    });
  });
});
