/**
 * DSK-04.3 / FER-540 — Resizable column widths and storage for the Largest files table.
 *
 * Thin configuration wrapping the generic createColumnResize helper:
 * - Columns: 'name', 'kind', 'size', 'percent'. (Rank '#' and 'Aksi' stay fixed).
 * - Minimum widths: Name = 120 px, kind/size/percent = 56 px.
 * - Maximum width: 2000 px.
 * - Defaults: Name receives remaining space (null in storage / auto in CSS),
 *   kind = 90 px, size = 100 px, percent = 70 px.
 * - ArrowLeft / ArrowRight change width by 16 px.
 * - Persisted to localStorage under KEY. Malformed or partial storage recovers
 *   safely field-wise to defaults.
 */

import { createColumnResize } from '../columnResize';

export type DuFilesCol = 'name' | 'kind' | 'size' | 'percent';

export interface DuFilesColumnWidths {
  name: number | null; // null = auto / receives remaining space
  kind: number;
  size: number;
  percent: number;
}

export const DU_FILES_COLUMNS: readonly DuFilesCol[] = ['name', 'kind', 'size', 'percent'] as const;

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

export const DU_FILES_MAX_WIDTH = 2000;

/** Default name width used as baseline in test/measurement fallbacks. */
export const DU_FILES_DEFAULT_NAME_WIDTH = 400;

/** Keyboard ←/→ step in pixels. */
export const DU_FILES_RESIZE_STEP = 16;

export const DU_FILES_COLUMNS_KEY = 'ferrum.diskUsageFiles.columns.v1';

export const duFilesColumnResize = createColumnResize<DuFilesCol>({
  keys: DU_FILES_COLUMNS,
  defaults: DEFAULT_DU_FILES_COLUMNS,
  mins: DU_FILES_MIN_WIDTHS,
  max: DU_FILES_MAX_WIDTH,
  step: DU_FILES_RESIZE_STEP,
  storageKey: DU_FILES_COLUMNS_KEY,
});

export function clampDuFilesColumnWidth(col: DuFilesCol, width: number): number {
  return duFilesColumnResize.clamp(col, width);
}

export function sanitizeDuFilesColumnWidths(raw: unknown): DuFilesColumnWidths {
  return duFilesColumnResize.sanitize(raw) as DuFilesColumnWidths;
}

export function loadDuFilesColumnWidths(storage?: Storage | null): DuFilesColumnWidths {
  return duFilesColumnResize.load(storage) as DuFilesColumnWidths;
}

export function persistDuFilesColumnWidths(
  widths: DuFilesColumnWidths,
  storage?: Storage | null,
): void {
  duFilesColumnResize.persist(widths, storage);
}
