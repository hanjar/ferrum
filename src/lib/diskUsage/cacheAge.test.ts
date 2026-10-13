import { describe, expect, it } from 'vitest';
import { ageLabel, cacheWarning } from './cacheAge';

const NOW = 1_800_000_000_000;
const H = 3_600_000;

describe('ageLabel', () => {
  it('speaks in minutes, hours and days', () => {
    expect(ageLabel(NOW - 5_000, NOW)).toBe('baru saja');
    expect(ageLabel(NOW - 5 * 60_000, NOW)).toBe('5 menit lalu');
    expect(ageLabel(NOW - 3 * H, NOW)).toBe('3 jam lalu');
    expect(ageLabel(NOW - 49 * H, NOW)).toBe('2 hari lalu');
  });
  it('does not invent an age for an unknown or future time', () => {
    expect(ageLabel(0, NOW)).toBe('waktu tidak diketahui');
    expect(ageLabel(NOW + H, NOW)).toBe('baru saja');
  });
});

describe('cacheWarning', () => {
  it('is silent for a recent, untouched cache', () => {
    expect(cacheWarning(NOW - 2 * H, NOW, 24, false)).toBeNull();
    expect(cacheWarning(NOW - 24 * H, NOW, 24, false)).toBeNull();
  });
  it('warns past the Settings threshold', () => {
    expect(cacheWarning(NOW - 25 * H, NOW, 24, false)).toContain('1 hari');
    expect(cacheWarning(NOW - 7 * H, NOW, 6, false)).toContain('6 jam');
  });
  it('always warns after an in-app mutation, however recent', () => {
    expect(cacheWarning(NOW - 60_000, NOW, 24, true)).toContain('perubahan');
  });
  it('warns when the scan time is unknown', () => {
    expect(cacheWarning(0, NOW, 24, false)).toContain('tidak diketahui');
  });
});
