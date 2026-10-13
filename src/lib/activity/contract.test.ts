import { describe, expect, it } from 'vitest';
import queryFixture from '../../../contract-fixtures/activity/query-v1.json';
import resultFixture from '../../../contract-fixtures/activity/result-v1.json';
import receiptFixture from '../../../contract-fixtures/activity/receipt-v1.json';
import preferencesFixture from '../../../contract-fixtures/activity/preferences-v1.json';
import statsFixture from '../../../contract-fixtures/activity/stats-v1.json';
import {
  parseActivityPreferences,
  parseActivityQuery,
  parseActivityResult,
  parseActivityReceipt,
  parseActivityStats,
} from './contract';

describe('activity IPC contract v1', () => {
  it('round-trips shared fixtures without losing identity or counts', () => {
    expect(parseActivityQuery(queryFixture)).toEqual(queryFixture);
    expect(parseActivityResult(resultFixture)).toEqual(resultFixture);
    expect(parseActivityReceipt(receiptFixture)).toEqual(receiptFixture);
    expect(parseActivityResult(resultFixture).entries[0].entity.locator).toBe('C:\\Work\\invoice');
  });

  it('rejects unknown fields, invalid limits and incomplete time bounds', () => {
    expect(() => parseActivityQuery({ ...queryFixture, limit: 0 })).toThrow();
    expect(() => parseActivityQuery({ ...queryFixture, limit: 501 })).toThrow();
    expect(() => parseActivityQuery({ ...queryFixture, surprise: 'secret' })).toThrow();
    expect(() =>
      parseActivityQuery({ ...queryFixture, bounds: { fromUtcMs: 10, untilUtcMs: 10 } }),
    ).toThrow();
  });

  it('rejects invalid locators, mismatched receipts and unknown discriminants', () => {
    expect(() => parseActivityReceipt({ ...receiptFixture, source: 'refresh' })).toThrow();
    expect(() => parseActivityReceipt({ ...receiptFixture, kind: 'fileOpenRequested' })).toThrow();
    expect(() =>
      parseActivityReceipt({
        ...receiptFixture,
        entity: { ...receiptFixture.entity, locator: '../relative' },
      }),
    ).toThrow();
    expect(() =>
      parseActivityResult({
        ...resultFixture,
        entries: [{ ...resultFixture.entries[0], matchKind: 'magic' }],
      }),
    ).toThrow();
  });

  it('round-trips privacy preferences and raw statistics fixtures', () => {
    expect(parseActivityPreferences(preferencesFixture)).toEqual(preferencesFixture);
    expect(parseActivityStats(statsFixture)).toEqual(statsFixture);
  });

  it('rejects invalid privacy settings and stats whose counts or coverage disagree', () => {
    expect(() => parseActivityPreferences({ ...preferencesFixture, retentionDays: 365 })).toThrow();
    expect(() =>
      parseActivityPreferences({ ...preferencesFixture, excludedRoots: ['relative'] }),
    ).toThrow();
    expect(() =>
      parseActivityPreferences({ ...preferencesFixture, hiddenEntityIds: [''] }),
    ).toThrow();
    expect(() => parseActivityPreferences({ ...preferencesFixture, extra: true })).toThrow();
    expect(() => parseActivityStats({ ...statsFixture, episodeCount: 7 })).toThrow();
    expect(() => parseActivityStats({ ...statsFixture, uniqueEntities: 9 })).toThrow();
    expect(() =>
      parseActivityStats({ ...statsFixture, daily: [{ utcDay: 'not-a-day', episodeCount: 8 }] }),
    ).toThrow();
    expect(() =>
      parseActivityStats({
        ...statsFixture,
        coverage: { ...statsFixture.coverage, untilUtcMs: 2000000000 },
      }),
    ).toThrow();
  });
});
