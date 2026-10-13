/** Breadcrumb segmentation + legacy drive-path repair. */

export interface Crumb {
  label: string;
  path: string;
  /** True for the archive file itself (.zip, .rar, .7z) */
  isArchive?: boolean;
  /** True for segments inside or at an archive boundary */
  inArchive?: boolean;
}

/**
 * Repair legacy persisted values: a drive-relative `C:` (saved when the old
 * breadcrumb navigated to the bare drive) or its `/C:` echo become `C:\`;
 * `/C:\x` loses the stray leading slash. Anything else is returned as-is.
 */
export function normalizeDrivePath(path: string): string {
  const m = /^\/?([A-Za-z]:)([\\/].*)?$/.exec(path);
  if (!m) return path;
  if (m[2] === undefined) return `${m[1]}\\`;
  // "C:\x" without a leading slash is already canonical.
  return path.startsWith('/') ? path.slice(1) : path;
}

const ARCHIVE_EXT_RE = /\.(zip|rar|7z)$/i;

export function crumbsOf(rawPath: string): Crumb[] {
  const path = normalizeDrivePath(rawPath);
  if (!path) return [];
  const sep = path.includes('\\') ? '\\' : '/';
  const out: Crumb[] = [];
  if (/^[A-Za-z]:[\\/]/.test(path)) {
    const parts = path.split(/[\\/]+/).filter(Boolean);
    const root = `${parts[0]}${path[2]}`; // "C:\" (keeps the original separator)
    out.push({ label: parts[0].toUpperCase(), path: root });
    let acc = root;
    for (const p of parts.slice(1)) {
      acc = `${acc}${acc.endsWith(path[2]) ? '' : path[2]}${p}`;
      out.push({ label: p, path: acc });
    }
  } else if (path.startsWith('\\\\')) {
    const parts = path.split(/[\\/]+/).filter(Boolean);
    if (parts.length === 0) return [];
    const root = `\\\\${parts[0]}${parts[1] ? `\\${parts[1]}` : ''}`;
    out.push({ label: root, path: parts[1] ? `${root}\\` : root });
    let acc = root;
    for (const p of parts.slice(2)) {
      acc = `${acc}\\${p}`;
      out.push({ label: p, path: acc });
    }
  } else {
    let acc = '/';
    for (const p of path.split(/[\\/]+/).filter(Boolean)) {
      acc = acc.endsWith(sep) ? `${acc}${p}` : `${acc}${sep}${p}`;
      out.push({ label: p, path: acc });
    }
  }

  // FER-53: mark archive segments
  const archiveIdx = out.findIndex((c) => ARCHIVE_EXT_RE.test(c.label));
  if (archiveIdx !== -1) {
    out[archiveIdx].isArchive = true;
    out[archiveIdx].inArchive = true;
    for (let i = archiveIdx + 1; i < out.length; i++) {
      out[i].inArchive = true;
    }
  }

  return out;
}

/** Natural (unclipped) pixel widths the layout needs from the DOM. */
export interface CrumbMetrics {
  /** One entry per crumb: its button's natural width. */
  widths: number[];
  /** Width of the chevron separator between two crumbs. */
  sep: number;
  /** Width of the `…` button. */
  more: number;
  /** Pixels the row may use (container width minus the click-to-edit gutter). */
  available: number;
}

export interface CrumbLayout {
  /** Indexes of the crumbs shown, in path order (the `…` button sits before `firstShown`). */
  shown: number[];
  /** Indexes folded into the `…` dropdown, in path order. Empty = no `…` button. */
  hidden: number[];
  /** Pixel cap for the last crumb when it must be cut with an ellipsis. */
  lastMax?: number;
}

/** The folder you are in never shrinks below this before the ellipsis kicks in. */
const MIN_LAST = 56;

/**
 * Decide which crumbs fit (FER-491). Priority: the last segment is always
 * shown whole (ellipsis at its end only when it alone overflows), then the
 * root/drive, then as many segments as fit counting from the right; whatever
 * is left in the middle collapses into one `…` button. Pure and DOM-free so
 * it can be measured once per frame and unit-tested.
 */
export function layoutCrumbs(m: CrumbMetrics): CrumbLayout {
  const n = m.widths.length;
  const all = Array.from({ length: n }, (_, i) => i);
  if (n === 0) return { shown: [], hidden: [] };
  const total = m.widths.reduce((a, w) => a + w, 0) + m.sep * (n - 1);
  if (total <= m.available) return { shown: all, hidden: [] };
  const last = n - 1;
  const wLast = m.widths[last];
  if (n === 1) return { shown: [0], hidden: [], lastMax: Math.max(MIN_LAST, m.available) };

  const moreCost = m.more + m.sep;
  // Not even the last segment + `…` fits: cut the last one, the rest live in `…`.
  if (wLast + moreCost > m.available) {
    return {
      shown: [last],
      hidden: all.slice(0, last),
      lastMax: Math.max(MIN_LAST, m.available - moreCost),
    };
  }
  let room = m.available - wLast - moreCost;
  // Root/drive next; if it does not fit beside the last segment it joins `…`
  // (nothing after it in priority may show either).
  const rootCost = m.widths[0] + m.sep;
  if (rootCost > room) return { shown: [last], hidden: all.slice(0, last) };
  room -= rootCost;
  // Then as many as fit from the right, contiguous with the last segment.
  const tail: number[] = [];
  let from = last;
  for (let i = last - 1; i >= 1; i--) {
    const cost = m.widths[i] + m.sep;
    if (cost > room) break;
    room -= cost;
    tail.unshift(i);
    from = i;
  }
  return { shown: [0, ...tail, last], hidden: all.slice(1, from) };
}
