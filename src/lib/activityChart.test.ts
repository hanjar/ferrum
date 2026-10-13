import { describe, expect, it } from 'vitest';
import { chartMax, coverageLabel, fillDays } from './activityChart';

const DAY = 86_400_000;
const NOW = 100 * DAY + 5_000;

describe('fillDays', () => {
  it('returns one zero-filled UTC bar per day, oldest first, ending today', () => {
    const bars = fillDays([{ dayUtcMs: 99 * DAY, items: 4 }], 7, NOW);
    expect(bars).toHaveLength(7);
    expect(bars[0].dayUtcMs).toBe(94 * DAY);
    expect(bars[6].dayUtcMs).toBe(100 * DAY);
    expect(bars[5]).toEqual({ dayUtcMs: 99 * DAY, items: 4 });
    expect(bars.filter((b) => b.items === 0)).toHaveLength(6);
    expect(chartMax(bars)).toBe(4);
  });

  it('ignores buckets outside the window', () => {
    const bars = fillDays([{ dayUtcMs: 10 * DAY, items: 9 }], 30, NOW);
    expect(chartMax(bars)).toBe(0);
  });
});

describe('coverageLabel', () => {
  const fmt = () => '2026-09-01 10:00';
  it('never claims all-time or OS-wide coverage', () => {
    const text = coverageLabel(1, 90, false, fmt);
    expect(text).toContain('di dalam Ferrum');
    expect(text).toContain('Bukan aktivitas OS');
    expect(text).toContain('2026-09-01');
    expect(text.toLowerCase()).not.toContain('seumur hidup');
  });
  it('states empty and truncated cases', () => {
    expect(coverageLabel(null, 30, false, fmt)).toContain('Belum ada data');
    expect(coverageLabel(1, 30, true, fmt)).toContain('dibatasi');
  });
});
