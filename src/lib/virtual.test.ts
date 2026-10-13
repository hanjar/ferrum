import { describe, expect, it } from 'vitest';
import { rangeToShow, ROW_H, visibleRange } from './virtual';

const VP = 520; // 20 rows of 26px

describe('visibleRange', () => {
  it('top of list shows first rows plus overscan', () => {
    const r = visibleRange(0, VP, 10_000, 8);
    expect(r.start).toBe(0);
    expect(r.end).toBe(20 + 8); // ceil(520/26)=20 visible + overscan below
  });

  it('overscans on both sides mid-list', () => {
    const r = visibleRange(26 * 1000, VP, 10_000, 8);
    expect(r.start).toBe(1000 - 8);
    expect(r.end).toBe(1000 + 20 + 8);
  });

  it('clamps at the bottom of the list', () => {
    // realistic max scroll: total height minus viewport
    const r = visibleRange(26 * 10_000 - VP, VP, 10_000, 8);
    expect(r.end).toBe(10_000);
    expect(r.start).toBe(10_000 - 20 - 8);
  });

  it('survives scroll positions past the end without an inverted range', () => {
    const r = visibleRange(26 * 99_999, VP, 10_000, 8);
    expect(r.end).toBe(10_000);
    expect(r.start).toBeLessThanOrEqual(r.end);
  });

  it('handles degenerate inputs', () => {
    expect(visibleRange(0, VP, 0)).toEqual({ start: 0, end: 0 });
    expect(visibleRange(-50, 0, 10)).toEqual({ start: 0, end: 0 });
    expect(visibleRange(0, VP, 10)).toEqual({ start: 0, end: 10 });
  });

  it('window size depends on viewport, not total', () => {
    const small = visibleRange(5000, VP, 10_000, 8);
    const huge = visibleRange(5000, VP, 1_000_000, 8);
    expect(small.end - small.start).toBe(huge.end - huge.start);
    expect(small.end - small.start).toBeLessThanOrEqual(20 + 2 * 8);
  });
});

describe('rangeToShow (keyboard cursor scroll)', () => {
  it('returns null when the row is already visible', () => {
    expect(rangeToShow(10, 26 * 5, VP)).toBeNull(); // rows 5..25 visible
  });

  it('scrolls up to reveal a row above the viewport with margin', () => {
    const t = rangeToShow(3, 26 * 5, VP)!;
    expect(t).toBeGreaterThanOrEqual(0);
    expect(t / ROW_H).toBeLessThanOrEqual(3);
  });

  it('scrolls down to reveal a row below the viewport', () => {
    const t = rangeToShow(40, 26 * 5, VP)!;
    expect(t + VP).toBeGreaterThanOrEqual(41 * ROW_H);
  });

  it('never returns negative scroll targets', () => {
    expect(rangeToShow(0, 26 * 10, VP, undefined, 100)).toBeGreaterThanOrEqual(0);
  });
});
