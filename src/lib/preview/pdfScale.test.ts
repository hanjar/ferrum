/**
 * FER-453 P3: the canvas pixel cap and the render limiter.
 */
import { describe, expect, it } from 'vitest';
import { MAX_CANVAS_PIXELS, createLimiter, renderScale } from './pdfScale';

describe('renderScale (16 MP cap)', () => {
  it('keeps zoom * dpr when the canvas fits', () => {
    // US Letter at 2x zoom and dpr 2 is about 3.2 MP
    expect(renderScale(612, 792, 2, 2)).toBe(4);
  });

  it('caps a huge page so the canvas never exceeds MAX_CANVAS_PIXELS', () => {
    const w = 10_900;
    const h = 10_900;
    const s = renderScale(w, h, 1, 1.5);
    expect(s).toBeLessThan(1.5);
    expect(Math.floor(w * s) * Math.floor(h * s)).toBeLessThanOrEqual(MAX_CANVAS_PIXELS);
    // and not absurdly smaller than the cap allows
    expect(Math.floor(w * s) * Math.floor(h * s)).toBeGreaterThan(MAX_CANVAS_PIXELS * 0.99);
  });

  it('caps at the maximum zoom and dpr too', () => {
    const s = renderScale(14_400, 14_400, 3, 2);
    expect(Math.floor(14_400 * s) ** 2).toBeLessThanOrEqual(MAX_CANVAS_PIXELS);
  });

  it('a custom cap is honoured', () => {
    expect(renderScale(1000, 1000, 1, 1, 250_000)).toBeCloseTo(0.5);
  });

  it('degenerate sizes fall back to the wanted scale', () => {
    expect(renderScale(0, 0, 2, 1)).toBe(2);
    expect(renderScale(Number.NaN, 10, 1, 1)).toBe(1);
  });
});

describe('createLimiter', () => {
  it('never runs more than the limit at once, and finishes all jobs', async () => {
    const limit = createLimiter(3);
    let running = 0;
    let peak = 0;
    const job = (n: number) =>
      limit(async () => {
        running++;
        peak = Math.max(peak, running);
        await new Promise((r) => setTimeout(r, 5));
        running--;
        return n;
      });
    const results = await Promise.all(Array.from({ length: 12 }, (_, i) => job(i)));
    expect(peak).toBe(3);
    expect(results).toEqual(Array.from({ length: 12 }, (_, i) => i));
  });

  it('a failing job rejects only its own caller', async () => {
    const limit = createLimiter(2);
    const ok = limit(async () => 'fine');
    const bad = limit(async () => {
      throw new Error('boom');
    });
    await expect(ok).resolves.toBe('fine');
    await expect(bad).rejects.toThrow('boom');
  });
});
