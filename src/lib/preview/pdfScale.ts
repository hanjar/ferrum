/**
 * Render limits for the sandboxed PDF viewer (FER-453, P3). Pure functions, so the
 * caps are unit-tested without a browser.
 */

/** Largest canvas the viewer will allocate for one page (pixels, 4 bytes each: about 64 MB). */
export const MAX_CANVAS_PIXELS = 16_000_000;

/** Pages drawn at the same time. Others wait in the queue. */
export const MAX_CONCURRENT_RENDERS = 3;

/**
 * Canvas scale for one page. `zoom * dpr` when the canvas stays within `maxPixels`;
 * otherwise the largest scale that does, so a huge page is drawn smaller instead of
 * allocating gigabytes.
 */
export function renderScale(
  baseW: number,
  baseH: number,
  zoom: number,
  dpr: number,
  maxPixels: number = MAX_CANVAS_PIXELS,
): number {
  const wanted = zoom * dpr;
  const area = baseW * baseH;
  if (!Number.isFinite(area) || area <= 0) return wanted;
  if (area * wanted * wanted <= maxPixels) return wanted;
  return Math.sqrt(maxPixels / area);
}

/**
 * Runs at most `limit` async jobs at once; the rest wait their turn in FIFO order.
 * Each job's failure is delivered to its own caller only.
 */
export function createLimiter(limit: number): <T>(job: () => Promise<T>) => Promise<T> {
  let active = 0;
  const queue: Array<() => void> = [];
  const next = (): void => {
    if (active >= limit) return;
    const start = queue.shift();
    if (start) start();
  };
  return <T>(job: () => Promise<T>): Promise<T> =>
    new Promise<T>((resolve, reject) => {
      queue.push(() => {
        active++;
        job()
          .then(resolve, reject)
          .finally(() => {
            active--;
            next();
          });
      });
      next();
    });
}
