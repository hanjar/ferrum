/** FER-593: zoom steps of the image viewer, in percent. */
export const ZOOM_LEVELS: readonly number[] = [25, 50, 75, 100, 150, 200, 300, 400];

/**
 * The next fixed level above (`dir` 1) or below (-1) `current` percent, which may sit between
 * levels (Fit). Null when there is none in that direction (already at the end).
 */
export function zoomStep(current: number, dir: 1 | -1): number | null {
  const eps = 0.5;
  const next =
    dir === 1
      ? ZOOM_LEVELS.find((z) => z > current + eps)
      : [...ZOOM_LEVELS].reverse().find((z) => z < current - eps);
  return next ?? null;
}
