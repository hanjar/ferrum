/**
 * Windowed rendering math — pure and unit-tested. The component layer only
 * feeds scrollTop/viewport height and renders [start,end).
 */

export const ROW_H = 26; // px, fixed — matches --row token in app.css

export interface Range {
  start: number; // inclusive
  end: number; // exclusive
}

/**
 * Visible row range with overscan on both sides, clamped to [0,total].
 * Row height is constant (ROW_H) so the math never needs per-row measurement.
 */
export function visibleRange(
  scrollTop: number,
  viewportH: number,
  total: number,
  overscan = 8,
  rowH: number = ROW_H,
): Range {
  const safeTop = Math.max(0, scrollTop);
  const rows = Math.max(0, total);
  if (rows === 0 || viewportH <= 0 || rowH <= 0) return { start: 0, end: 0 };

  const first = Math.min(rows - 1, Math.floor(safeTop / rowH));
  const visible = Math.ceil(viewportH / rowH);
  const start = Math.max(0, first - overscan);
  const end = Math.min(rows, first + visible + overscan);
  return { start, end: Math.max(start, end) };
}

/** Range that fully reveals row `index` (for keyboard cursor movement). */
export function rangeToShow(
  index: number,
  scrollTop: number,
  viewportH: number,
  rowH: number = ROW_H,
  margin = 3,
): number | null {
  if (rowH <= 0 || viewportH <= 0) return null;
  const top = index * rowH;
  const bottom = top + rowH;
  const viewTop = scrollTop;
  const viewBottom = scrollTop + viewportH;
  if (top >= viewTop + margin * 0 && bottom <= viewBottom) return null; // visible
  if (top < viewTop) return Math.max(0, top - margin * rowH); // scroll up
  return bottom - viewportH + margin * rowH; // scroll down
}
