/**
 * DSK-05 — squarified treemap layout. Pure, deterministic, DOM-free.
 *
 * Contract bindings (docs/disk-usage-contract.md):
 * - §2.2 allocated bytes `null` = unavailable: its area is 0 and logical
 *   bytes are NEVER substituted; the item stays visible as a member of the
 *   virtual Other group (nothing is dropped).
 * - §3.8 stable tie-break: equal values order by id ascending, so the same
 *   dataset always yields the same rects (progressive scans included).
 * - §2.6 plain numbers only; integer rect rounding overflows the canvas by
 *   at most 1px (see `rect()`).
 */

import type { DiskUsageChildItem, DiskUsageSizeMetric } from '../ipc/types';

/** Reserved id of the virtual "Other" group tile. */
export const OTHER_ID = '__other__';

/**
 * DSK-10 (FER-408) — responsive tree column template. The counts columns may
 * shrink in a narrow half-window panel, but the NAME column never goes below
 * 140px and a row never below its track minimum — below that the tree
 * container scrolls horizontally (a visible scrollbar), so no column is ever
 * silently clipped. Shared by the tree component and its tests.
 */
export const DU_TREE_ROW_COLUMNS =
  '20px minmax(140px, 1fr) minmax(48px, 64px) minmax(64px, 84px) minmax(40px, 52px) minmax(52px, 72px)';
/** Tracks 20+140+48+64+40+52 + 5×8 gaps + 2×10 row padding. */
export const DU_TREE_ROW_MIN_WIDTH = 424;

export interface TreemapOptions {
  /** Byte metric driving the areas — the SAME metric the caller displays. */
  metric: DiskUsageSizeMetric;
  /** Items outside the top N by value are grouped into Other (default 12). */
  maxTiles?: number;
  /** Items below this fraction of the total area are grouped (default 0.015). */
  smallFraction?: number;
  /** Maximum nesting depth for nested folders (default 3). */
  maxDepth?: number;
  /** Maximum total rectangles to render across all levels (default 2000). */
  maxRects?: number;
}

/** Aggregated small tiles — members stay addressable for tooltip/detail. */
export interface OtherGroup {
  id: string;
  /** "Other · N item" */
  label: string;
  count: number;
  /** Sum of member logicalBytes (complete for the logical metric). */
  logicalBytes: number;
  /** Sum of member allocatedBytes; null when ANY member is null (partial —
   *  never padded with logical bytes, contract §2.2). */
  allocatedBytes: number | null;
  /** Sum of the selected metric over members (null allocated → 0 area). */
  value: number;
  /** Original items, largest first — the tooltip/detail list. */
  members: DiskUsageChildItem[];
}

export interface TreemapRect {
  id: string;
  x: number;
  y: number;
  w: number;
  h: number;
  /** Item name, or "Other · N item". */
  label: string;
  /** Metric bytes driving the area (Other: sum of member metric values). */
  value: number;
  /** The original item — absent for the Other group tile. */
  item?: DiskUsageChildItem;
  /** Set exactly when id === OTHER_ID or nested other group. */
  otherGroup?: OtherGroup;
  /** Hierarchy depth in the treemap: 1 for direct focus children, 2 for grandchildren, 3 for great-grandchildren. */
  depth: number;
  /** True when this rect represents a folder container that nests its children inside. */
  isDirContainer?: boolean;
  /** Header height in pixels for the folder label if drawn (0 for compact/no header). */
  headerHeight?: number;
  /** Inner content area allocated for children inside this container. */
  contentRect?: { x: number; y: number; w: number; h: number };
}

export interface SquarifyResult {
  rects: TreemapRect[];
  /** Sum of the selected metric over ALL input items (0 for empty input). */
  totalValue: number;
  /** How many items were folded into the Other group (0 = no group). */
  otherCount: number;
}

/** Bytes of `item` under `metric`. Null allocated → 0, never logical (§2.2). */
export function metricValue(item: DiskUsageChildItem, metric: DiskUsageSizeMetric): number {
  return metric === 'allocated' ? (item.allocatedBytes ?? 0) : item.logicalBytes;
}

/** Largest-first with the §3.8 stable id tie-break. */
function byValueDesc(
  items: DiskUsageChildItem[],
  metric: DiskUsageSizeMetric,
): DiskUsageChildItem[] {
  return [...items].sort((a, b) => {
    const d = metricValue(b, metric) - metricValue(a, metric);
    if (d !== 0) return d;
    return a.id < b.id ? -1 : a.id > b.id ? 1 : 0;
  });
}

/**
 * Split into regular tiles and (optionally) one virtual Other group.
 * A tile is "small" — and joins Other — when it sits outside the top
 * `maxTiles` OR its value is below `smallFraction` of the total. Zero-value
 * items (null allocated) always end up in Other, so they stay listed without
 * stealing area. The regular set is never left empty while total > 0.
 */
function groupOther(
  items: DiskUsageChildItem[],
  opts: TreemapOptions,
  otherId = OTHER_ID,
): { tiles: DiskUsageChildItem[]; other: OtherGroup | null; totalValue: number } {
  const maxTiles = Math.max(1, opts.maxTiles ?? 12);
  const smallFraction = opts.smallFraction ?? 0.015;
  const sorted = byValueDesc(items, opts.metric);
  const totalValue = sorted.reduce((s, it) => s + metricValue(it, opts.metric), 0);

  const smallIdx = new Set<number>();
  for (let i = 0; i < sorted.length; i++) {
    if (
      i >= maxTiles ||
      (totalValue > 0 && metricValue(sorted[i], opts.metric) / totalValue < smallFraction)
    ) {
      smallIdx.add(i);
    }
  }
  if (smallIdx.size === sorted.length && totalValue > 0) {
    smallIdx.delete(0); // keep the largest tile out of Other
  }

  const tiles = sorted.filter((_, i) => !smallIdx.has(i));
  const members = sorted.filter((_, i) => smallIdx.has(i));
  let other: OtherGroup | null = null;
  if (members.length > 0) {
    let logicalBytes = 0;
    let allocatedBytes = 0;
    let allocatedNull = false;
    let value = 0;
    for (const m of members) {
      logicalBytes += m.logicalBytes;
      if (m.allocatedBytes === null) allocatedNull = true;
      else allocatedBytes += m.allocatedBytes;
      value += metricValue(m, opts.metric);
    }
    other = {
      id: otherId,
      label: `Other · ${members.length} item`,
      count: members.length,
      logicalBytes,
      allocatedBytes: allocatedNull ? null : allocatedBytes,
      value,
      members,
    };
  }
  return { tiles, other, totalValue };
}

function squarifySingleLevel(
  items: DiskUsageChildItem[],
  x: number,
  y: number,
  width: number,
  height: number,
  opts: TreemapOptions,
  depth: number,
  maxDepth: number,
  maxRects: number,
  outRects: TreemapRect[],
  idPrefix: string,
): { totalValue: number; otherCount: number } {
  const otherId = idPrefix ? `${idPrefix}${OTHER_ID}` : OTHER_ID;
  const { tiles, other, totalValue } = groupOther(items, opts, otherId);
  const otherCount = other ? other.count : 0;

  if (
    items.length === 0 ||
    totalValue <= 0 ||
    !Number.isFinite(width) ||
    !Number.isFinite(height) ||
    width <= 0 ||
    height <= 0 ||
    outRects.length >= maxRects
  ) {
    return { totalValue, otherCount };
  }

  type Entry = {
    item: DiskUsageChildItem | null;
    other: OtherGroup | null;
    value: number;
    area: number;
  };
  const canvas = width * height;
  const entries: Entry[] = tiles.map((item) => {
    const value = metricValue(item, opts.metric);
    return { item, other: null, value, area: (value / totalValue) * canvas };
  });
  if (other) {
    // The group keeps a rect even when its value is 0 (all-null allocated):
    // zero-area, but the members remain attached and visible in the legend.
    entries.push({
      item: null,
      other,
      value: other.value,
      area: (other.value / totalValue) * canvas,
    });
  }

  const levelRects: TreemapRect[] = [];

  function makeRect(e: Entry, rx: number, ry: number, rw: number, rh: number): TreemapRect {
    const roundX = Math.max(0, Math.round(rx));
    const roundY = Math.max(0, Math.round(ry));
    const roundW = Math.max(
      0,
      Math.min(Math.round(rx + rw) - roundX, Math.round(x + width) - roundX),
    );
    const roundH = Math.max(
      0,
      Math.min(Math.round(ry + rh) - roundY, Math.round(y + height) - roundY),
    );
    if (e.other) {
      return {
        id: e.other.id,
        x: roundX,
        y: roundY,
        w: roundW,
        h: roundH,
        label: e.other.label,
        value: e.other.value,
        otherGroup: e.other,
        depth,
        isDirContainer: false,
        headerHeight: 0,
      };
    }
    const item = e.item as DiskUsageChildItem;
    return {
      id: item.id,
      x: roundX,
      y: roundY,
      w: roundW,
      h: roundH,
      label: item.name,
      value: e.value,
      item,
      depth,
      isDirContainer: false,
      headerHeight: 0,
    };
  }

  let cx = x;
  let cy = y;
  let cw = width;
  let ch = height;
  let row: Entry[] = [];

  const worst = (r: Entry[], side: number): number => {
    const s = r.reduce((a, q) => a + q.area, 0);
    let worstRatio = 0;
    for (const q of r) {
      const ratio = Math.max((side * side * q.area) / (s * s), (s * s) / (side * side * q.area));
      if (ratio > worstRatio) worstRatio = ratio;
    }
    return worstRatio;
  };

  const layRow = (r: Entry[]): void => {
    const s = r.reduce((a, q) => a + q.area, 0);
    if (s <= 0 || cw <= 0 || ch <= 0) return;
    if (cw >= ch) {
      const stripW = s / ch;
      let yy = cy;
      for (const q of r) {
        const qh = q.area / stripW;
        levelRects.push(makeRect(q, cx, yy, stripW, qh));
        yy += qh;
      }
      cx += stripW;
      cw -= stripW;
    } else {
      const stripH = s / cw;
      let xx = cx;
      for (const q of r) {
        const qw = q.area / stripH;
        levelRects.push(makeRect(q, xx, cy, qw, stripH));
        xx += qw;
      }
      cy += stripH;
      ch -= stripH;
    }
  };

  for (const e of entries) {
    if (e.area <= 0) {
      levelRects.push(makeRect(e, x, y, 0, 0));
      continue;
    }
    if (row.length === 0 || worst([...row, e], Math.min(cw, ch)) <= worst(row, Math.min(cw, ch))) {
      row.push(e);
    } else {
      layRow(row);
      row = [e];
    }
  }
  if (row.length > 0) layRow(row);

  for (const rect of levelRects) {
    if (outRects.length >= maxRects) break;
    const it = rect.item;
    const canRecurse =
      depth < maxDepth &&
      it !== undefined &&
      it.kind === 'dir' &&
      Array.isArray(it.children) &&
      it.children.length > 0;

    const pad = 2;
    const minW = 28;
    const minH = 24;

    if (canRecurse && rect.w >= minW && rect.h >= minH) {
      const headerH = rect.w >= 48 && rect.h >= 36 ? 18 : rect.w >= 36 && rect.h >= 24 ? 14 : 0;
      const innerX = rect.x + pad;
      const innerY = rect.y + headerH + pad;
      const innerW = rect.w - 2 * pad;
      const innerH = rect.h - headerH - 2 * pad;

      rect.isDirContainer = true;
      rect.headerHeight = headerH;
      rect.contentRect = { x: innerX, y: innerY, w: innerW, h: innerH };
      outRects.push(rect);

      if (innerW >= 4 && innerH >= 4 && outRects.length < maxRects) {
        squarifySingleLevel(
          it.children!,
          innerX,
          innerY,
          innerW,
          innerH,
          opts,
          depth + 1,
          maxDepth,
          maxRects,
          outRects,
          rect.id + '/',
        );
      }
    } else {
      rect.isDirContainer = false;
      rect.headerHeight = 0;
      outRects.push(rect);
    }
  }

  return { totalValue, otherCount };
}

/**
 * Squarified layout (Bruls et al.), extended for nested multi-level display.
 * Displays focus folder children and nested children up to 3 levels deep with folder
 * headers and padding.
 *
 * Percentage basis conventions (FER-550):
 * - Tile label: percent of the currently focused folder (r.value / totalValue * 100),
 *   consistent across all nesting depths (depth 1, 2, 3).
 * - Tooltip: comprehensive breakdown:
 *   1) % of focus: portion against the current focused folder root
 *   2) of parent: portion against the node's immediate parent folder (percentOfParent)
 *   3) of scan root: portion against the global scan root (percentOfScanRoot / scanRootTotal)
 *
 * Input order does not matter — items are sorted internally, so the same dataset always
 * produces the same rects deterministically.
 */
export function squarify(
  items: DiskUsageChildItem[],
  width: number,
  height: number,
  opts: TreemapOptions,
): SquarifyResult {
  const maxDepth = opts.maxDepth ?? 3;
  const maxRects = opts.maxRects ?? 2000;
  const allRects: TreemapRect[] = [];

  const topLevel = squarifySingleLevel(
    items,
    0,
    0,
    width,
    height,
    opts,
    1,
    maxDepth,
    maxRects,
    allRects,
    '',
  );

  return {
    rects: allRects,
    totalValue: topLevel.totalValue,
    otherCount: topLevel.otherCount,
  };
}
