/**
 * DSK-04.3 / FER-540 — Resizable column widths and storage for the Largest files table.
 *
 * Contract:
 * - Columns: 'name', 'kind', 'size', 'percent'. (Rank '#' and 'Aksi' stay fixed).
 * - Minimum widths: Name = 120 px, kind/size/percent = 56 px.
 * - Defaults: Name receives remaining space (null in storage / auto in CSS),
 *   kind = 90 px, size = 100 px, percent = 70 px.
 * - ArrowLeft / ArrowRight change width by 16 px.
 * - Persisted to localStorage under KEY. Malformed or partial storage recovers
 *   safely field-wise to defaults.
 */

export type DuFilesCol = 'name' | 'kind' | 'size' | 'percent';

export interface DuFilesColumnWidths {
  name: number | null; // null = auto / receives remaining space
  kind: number;
  size: number;
  percent: number;
}

export const DEFAULT_DU_FILES_COLUMNS: DuFilesColumnWidths = {
  name: null,
  kind: 90,
  size: 100,
  percent: 70,
};

export const DU_FILES_MIN_WIDTHS: Record<DuFilesCol, number> = {
  name: 120,
  kind: 56,
  size: 56,
  percent: 56,
};

/** Default name width used as baseline in test/measurement fallbacks. */
export const DU_FILES_DEFAULT_NAME_WIDTH = 400;

/** Keyboard ←/→ step in pixels. */
export const DU_FILES_RESIZE_STEP = 16;

export const DU_FILES_COLUMNS_KEY = 'ferrum.diskUsageFiles.columns.v1';

export function clampDuFilesColumnWidth(col: DuFilesCol, width: number): number {
  return Math.max(DU_FILES_MIN_WIDTHS[col], Math.round(width));
}

export function sanitizeDuFilesColumnWidths(raw: unknown): DuFilesColumnWidths {
  if (typeof raw !== 'object' || raw === null) return { ...DEFAULT_DU_FILES_COLUMNS };
  const r = raw as Record<string, unknown>;
  const sanitize = (val: unknown, min: number, def: number | null): number | null => {
    if (val === null) return null;
    if (typeof val === 'number' && Number.isFinite(val) && val >= min) return Math.round(val);
    return def;
  };
  return {
    name: sanitize(r.name, DU_FILES_MIN_WIDTHS.name, DEFAULT_DU_FILES_COLUMNS.name),
    kind:
      sanitize(r.kind, DU_FILES_MIN_WIDTHS.kind, DEFAULT_DU_FILES_COLUMNS.kind) ??
      DEFAULT_DU_FILES_COLUMNS.kind,
    size:
      sanitize(r.size, DU_FILES_MIN_WIDTHS.size, DEFAULT_DU_FILES_COLUMNS.size) ??
      DEFAULT_DU_FILES_COLUMNS.size,
    percent:
      sanitize(r.percent, DU_FILES_MIN_WIDTHS.percent, DEFAULT_DU_FILES_COLUMNS.percent) ??
      DEFAULT_DU_FILES_COLUMNS.percent,
  };
}

export function loadDuFilesColumnWidths(
  storage: Storage | undefined = globalThis.localStorage,
): DuFilesColumnWidths {
  const raw = storage?.getItem(DU_FILES_COLUMNS_KEY);
  if (raw === null || raw === undefined) return { ...DEFAULT_DU_FILES_COLUMNS };
  try {
    return sanitizeDuFilesColumnWidths(JSON.parse(raw));
  } catch {
    return { ...DEFAULT_DU_FILES_COLUMNS };
  }
}

export function persistDuFilesColumnWidths(
  widths: DuFilesColumnWidths,
  storage: Storage | undefined = globalThis.localStorage,
): void {
  try {
    storage?.setItem(DU_FILES_COLUMNS_KEY, JSON.stringify(widths));
  } catch {
    /* storage full/unavailable — saving is best-effort */
  }
}
