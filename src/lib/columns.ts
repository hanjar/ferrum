/**
 * Custom Columns engine (FER-365, opens decision D33): pure config →
 * render-model functions for the Full (details) mode listing. Scope follows
 * the F2-27.4 subtask: named configuration of the EXISTING SortKey-able
 * columns only — name, extension, size, modified — never new column kinds
 * and never arbitrary HTML (the plugin API stays deferred).
 *
 * `name` is the row identity (icon, inline rename, cursor): it is always
 * visible and always first, exactly like the built-in layout. The user
 * configures visibility and order of the remaining columns.
 */
import type { SortSpec } from './selection';
import { createColumnResize } from './columnResize';

export type ColumnId = 'name' | 'extension' | 'size' | 'modified';

export type ResizableColumnId = 'extension' | 'size' | 'modified' | 'comment';

export const RESIZABLE_COLUMNS: readonly ResizableColumnId[] = [
  'extension',
  'size',
  'modified',
  'comment',
] as const;

export type FileListColumnWidths = Record<ResizableColumnId, number | null>;

export const DEFAULT_FILE_LIST_COLUMN_WIDTHS: FileListColumnWidths = {
  extension: 72,
  size: 72,
  modified: 132,
  comment: null,
};

export const FILE_LIST_COLUMN_MINS: Record<ResizableColumnId, number> = {
  extension: 40,
  size: 56,
  modified: 90,
  comment: 80,
};

export const FILE_LIST_COLUMN_MAX = 2000;
export const FILE_LIST_DEFAULT_COMMENT_WIDTH = 200;

export const fileListColumnResize = createColumnResize<ResizableColumnId>({
  keys: RESIZABLE_COLUMNS,
  defaults: DEFAULT_FILE_LIST_COLUMN_WIDTHS,
  mins: FILE_LIST_COLUMN_MINS,
  max: FILE_LIST_COLUMN_MAX,
});

export function clampFileListColumnWidth(col: ResizableColumnId, width: number): number {
  return fileListColumnResize.clamp(col, width);
}

export function sanitizeFileListColumnWidths(raw: unknown): FileListColumnWidths {
  return fileListColumnResize.sanitize(raw) as FileListColumnWidths;
}

export function isDefaultFileListColumnWidths(widths: FileListColumnWidths): boolean {
  for (const k of RESIZABLE_COLUMNS) {
    if (widths[k] !== DEFAULT_FILE_LIST_COLUMN_WIDTHS[k]) return false;
  }
  return true;
}

/** Ordered visible-column list; `name` first (validated invariant). */
export type ColumnConfig = ColumnId[];

/** Every column the engine knows about, in canonical order. */
export const ALL_COLUMNS: ColumnId[] = ['name', 'extension', 'size', 'modified'];

/** Zero-config default = the built-in Full mode columns, so behaviour with
 *  no stored preference is byte-identical to before. */
export const DEFAULT_COLUMN_CONFIG: ColumnConfig = ['name', 'size', 'modified'];

/** Header label of one column (English UI). */
export const COLUMN_LABELS: Record<ColumnId, string> = {
  name: 'Name',
  extension: 'Ext',
  size: 'Size',
  modified: 'Modified',
};

const isColumnId = (v: unknown): v is ColumnId =>
  typeof v === 'string' && (ALL_COLUMNS as string[]).includes(v);

/**
 * Validate arbitrary persisted data into a usable config. Suspect input
 * (wrong shape, unknown ids, duplicates, missing name) falls back to the
 * built-in default; a valid list is normalised so `name` leads.
 */
export function parseColumnConfig(raw: unknown): ColumnConfig {
  if (typeof raw === 'object' && raw !== null && 'columns' in raw) {
    return parseColumnConfig((raw as { columns: unknown }).columns);
  }
  if (!Array.isArray(raw)) return [...DEFAULT_COLUMN_CONFIG];
  if (!raw.every(isColumnId)) return [...DEFAULT_COLUMN_CONFIG];
  const ids = raw as ColumnId[];
  if (new Set(ids).size !== ids.length) return [...DEFAULT_COLUMN_CONFIG]; // duplicates = ambiguous intent
  if (!ids.includes('name')) return [...DEFAULT_COLUMN_CONFIG];
  const rest = ids.filter((id) => id !== 'name');
  return ['name', ...rest];
}

export function isDefaultColumnConfig(config: ColumnConfig): boolean {
  return (
    config.length === DEFAULT_COLUMN_CONFIG.length &&
    config.every((id, i) => id === DEFAULT_COLUMN_CONFIG[i])
  );
}

/** One rendered column in a row/header grid. `id` is a configured column,
 *  or the synthetic `'comment'` column appended by Comments view (F2-27). */
export interface ColumnLayoutColumn {
  id: ColumnId | 'comment';
  label: string;
  /** CSS grid track for this column (`minmax(0, 1fr)` for the name). */
  track: string;
}

/** Render model for the Full-mode header and row grids. */
export interface ColumnLayout {
  columns: ColumnLayoutColumn[];
  /** Complete `grid-template-columns`, including the trailing Comments
   *  column when requested (mirrors the built-in `.withComments` layout). */
  gridTemplate: string;
}

export type ColumnWidths = Partial<Record<ColumnId | 'comment', number | null>>;

const TRACKS: Record<ColumnId, string> = {
  name: 'minmax(0, 1fr)',
  extension: '72px',
  size: '72px',
  modified: '132px',
};

/** Built-in Full mode layout, with the optional Comments column appended and optional width overrides. */
export function columnLayout(
  config: ColumnConfig = DEFAULT_COLUMN_CONFIG,
  opts: {
    comments?: boolean;
    widths?: ColumnWidths;
    useCssVars?: boolean;
  } = {},
): ColumnLayout {
  const columns: ColumnLayoutColumn[] = config.map((id) => {
    let track = TRACKS[id];
    if (id !== 'name') {
      const fallback =
        opts.widths &&
        typeof opts.widths[id] === 'number' &&
        Number.isFinite(opts.widths[id]) &&
        (opts.widths[id] as number) > 0
          ? `${Math.round(opts.widths[id] as number)}px`
          : TRACKS[id];
      track = opts.useCssVars ? `var(--col-${id}, ${fallback})` : fallback;
    }
    return {
      id,
      label: COLUMN_LABELS[id],
      track,
    };
  });
  if (opts.comments) {
    let commentFallback = 'minmax(0, 1.6fr)';
    if (opts.widths) {
      const w = opts.widths.comment;
      if (typeof w === 'number' && Number.isFinite(w) && w > 0) {
        commentFallback = `${Math.round(w)}px`;
      }
    }
    const commentTrack = opts.useCssVars
      ? `var(--col-comment, ${commentFallback})`
      : commentFallback;
    columns.push({ id: 'comment', label: 'Comment', track: commentTrack });
  }
  return { columns, gridTemplate: columns.map((c) => c.track).join(' ') };
}

/**
 * Sorting by a hidden column falls back sanely: the effective sort key
 * becomes Name (same direction) so the visible order is always explained
 * by a visible header. `unsorted` is not a column and passes through.
 */
export function effectiveSortSpec(config: ColumnConfig, sort: SortSpec): SortSpec {
  if (sort.key === 'unsorted' || config.includes(sort.key as ColumnId)) return sort;
  return { key: 'name', dir: sort.dir };
}

/** Cell text for the extension column (directories show none). */
export function extensionText(name: string, isDir: boolean): string {
  if (isDir) return '';
  const dot = name.lastIndexOf('.');
  return dot > 0 ? name.slice(dot + 1) : '';
}

/** Listing display name (FER-379 "Show file extensions" off): files drop the
 *  last suffix only — directories, dotfiles and extensionless names stay
 *  whole. Display only; rename/tooltip/operations keep the real name. */
export function displayName(name: string, isDir: boolean, showExtensions: boolean): string {
  if (showExtensions || isDir || name.startsWith('.')) return name;
  const dot = name.lastIndexOf('.');
  return dot > 0 ? name.slice(0, dot) : name;
}
