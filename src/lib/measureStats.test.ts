import { describe, expect, it } from 'vitest';
// @ts-expect-error plain .mjs script without type declarations
import { median, percentile, cpuDeltaSeconds, sumMem } from '../../scripts/measure-app.mjs';

describe('scripts/measure-app.mjs helpers', () => {
  it('median handles odd, even and empty input', () => {
    expect(median([3, 1, 2])).toBe(2);
    expect(median([4, 1, 3, 2])).toBe(2.5);
    expect(median([])).toBeNaN();
  });

  it('percentile uses nearest rank', () => {
    const v = [10, 20, 30, 40, 50, 60, 70, 80, 90, 100];
    expect(percentile(v, 95)).toBe(100);
    expect(percentile(v, 50)).toBe(50);
    expect(percentile([5], 95)).toBe(5);
  });

  it('cpuDeltaSeconds only counts pids present in both samples', () => {
    const before = [
      { id: 1, cpu: 1 },
      { id: 2, cpu: 2 },
    ];
    const after = [
      { id: 1, cpu: 1.5 },
      { id: 3, cpu: 9 },
    ];
    expect(cpuDeltaSeconds(before, after)).toBeCloseTo(0.5);
  });

  it('sumMem converts bytes to MB', () => {
    const procs = [
      { ws: 1048576 * 10, pm: 1048576 * 4 },
      { ws: 1048576 * 5, pm: 1048576 * 1 },
    ];
    expect(sumMem(procs)).toEqual({ processes: 2, workingSetMB: 15, privateMB: 5 });
  });
});
