/**
 * Custom Columns store (FER-365, opens decision D33): the visible-column
 * configuration for Full mode as ONE persisted preference (localStorage,
 * like the view presets / hotlist pattern). The config is global — both
 * panes' Full mode render it — and independent of the named view presets,
 * which keep snapshotting the existing per-pane view state only. Malformed
 * storage recovers to the built-in default via `parseColumnConfig`.
 */
import {
  ALL_COLUMNS,
  DEFAULT_COLUMN_CONFIG,
  DEFAULT_FILE_LIST_COLUMN_WIDTHS,
  clampFileListColumnWidth,
  fileListColumnResize,
  isDefaultFileListColumnWidths,
  parseColumnConfig,
  sanitizeFileListColumnWidths,
  type ColumnConfig,
  type ColumnId,
  type FileListColumnWidths,
  type ResizableColumnId,
} from '../columns';

const KEY = 'ferrum.customColumns.v1';

export interface PersistedColumnPayload {
  columns?: unknown;
  widths?: unknown;
}

function getStorage(storage?: Storage): Storage | undefined {
  if (storage !== undefined) return storage;
  try {
    return typeof globalThis !== 'undefined' ? globalThis.localStorage : undefined;
  } catch {
    return undefined;
  }
}

export function loadStoredColumnData(storage?: Storage): {
  config: ColumnConfig;
  widths: FileListColumnWidths;
} {
  let raw: string | null = null;
  try {
    const s = getStorage(storage);
    raw = s?.getItem(KEY) ?? null;
  } catch {
    return {
      config: [...DEFAULT_COLUMN_CONFIG],
      widths: { ...DEFAULT_FILE_LIST_COLUMN_WIDTHS },
    };
  }

  if (raw === null || raw === undefined) {
    return {
      config: [...DEFAULT_COLUMN_CONFIG],
      widths: { ...DEFAULT_FILE_LIST_COLUMN_WIDTHS },
    };
  }
  let data: unknown;
  try {
    data = JSON.parse(raw);
  } catch {
    return {
      config: [...DEFAULT_COLUMN_CONFIG],
      widths: { ...DEFAULT_FILE_LIST_COLUMN_WIDTHS },
    };
  }

  // Legacy format: raw array of ColumnIds
  if (Array.isArray(data)) {
    return {
      config: parseColumnConfig(data),
      widths: { ...DEFAULT_FILE_LIST_COLUMN_WIDTHS },
    };
  }

  // Object format: { columns?: ..., widths?: ... }
  if (typeof data === 'object' && data !== null) {
    const obj = data as PersistedColumnPayload;
    return {
      config: parseColumnConfig(obj.columns),
      widths: sanitizeFileListColumnWidths(obj.widths),
    };
  }

  return {
    config: [...DEFAULT_COLUMN_CONFIG],
    widths: { ...DEFAULT_FILE_LIST_COLUMN_WIDTHS },
  };
}

export function loadColumnConfig(storage?: Storage): ColumnConfig {
  return loadStoredColumnData(storage).config;
}

export function loadColumnWidths(storage?: Storage): FileListColumnWidths {
  return loadStoredColumnData(storage).widths;
}

export function persistCustomColumns(
  config: ColumnConfig,
  widths: FileListColumnWidths,
  storage?: Storage,
): void {
  try {
    const s = getStorage(storage);
    if (!s) return;
    if (isDefaultFileListColumnWidths(widths)) {
      // If widths are all default, write flat ColumnConfig array for 100% backward compatibility
      s.setItem(KEY, JSON.stringify(config));
    } else {
      s.setItem(KEY, JSON.stringify({ columns: config, widths }));
    }
  } catch {
    /* storage full/unavailable — saving is best-effort */
  }
}

export function persistColumnConfig(config: ColumnConfig, storage?: Storage): void {
  const widths = columns ? columns.widths : { ...DEFAULT_FILE_LIST_COLUMN_WIDTHS };
  persistCustomColumns(config, widths, storage);
}

class ColumnsStore {
  open = $state(false);
  config = $state<ColumnConfig>(loadStoredColumnData().config);
  widths = $state<FileListColumnWidths>(loadStoredColumnData().widths);

  openDialog(): void {
    this.open = true;
  }

  close(): void {
    this.open = false;
  }

  private commit(nextConfig: ColumnConfig, nextWidths: FileListColumnWidths = this.widths): void {
    this.config = nextConfig;
    this.widths = nextWidths;
    persistCustomColumns(nextConfig, nextWidths);
  }

  /** Set and persist one column width. */
  setWidth(id: ResizableColumnId, width: number): void {
    const clamped = clampFileListColumnWidth(id, width);
    const nextWidths = { ...this.widths, [id]: clamped };
    this.commit(this.config, nextWidths);
  }

  /** Reset one column width to default. */
  resetWidth(id: ResizableColumnId): void {
    const nextWidths = fileListColumnResize.resetCol(this.widths, id);
    this.commit(this.config, nextWidths);
  }

  /** Reset all column widths to default. */
  resetWidths(): void {
    const nextWidths = fileListColumnResize.reset();
    this.commit(this.config, nextWidths);
  }

  /** Show/hide one column; `name` is pinned visible (row identity). */
  setVisible(id: ColumnId, visible: boolean): void {
    if (id === 'name') return;
    const rest = this.config.filter((c) => c !== 'name');
    if (visible === rest.includes(id)) return;
    if (!visible) {
      this.commit(['name', ...rest.filter((c) => c !== id)]);
      return;
    }
    // Reappear at its canonical ALL_COLUMNS position among the visible rest.
    const order = ALL_COLUMNS.filter((c) => c !== 'name' && (c === id || rest.includes(c)));
    this.commit(['name', ...order]);
  }

  /** Move one non-name column one slot up/down in the visible order. */
  move(id: ColumnId, delta: 1 | -1): void {
    if (id === 'name') return;
    const rest = this.config.filter((c) => c !== 'name');
    const i = rest.indexOf(id);
    const j = i + delta;
    if (i < 0 || j < 0 || j >= rest.length) return;
    [rest[i], rest[j]] = [rest[j], rest[i]];
    this.commit(['name', ...rest]);
  }

  /** Back to the built-in layout (also the malformed-storage recovery). */
  reset(): void {
    this.widths = fileListColumnResize.reset();
    this.commit([...DEFAULT_COLUMN_CONFIG], this.widths);
  }

  /** Test hook. */
  resetAll(): void {
    this.open = false;
    try {
      getStorage()?.removeItem(KEY);
    } catch {
      /* best-effort */
    }
    this.config = [...DEFAULT_COLUMN_CONFIG];
    this.widths = fileListColumnResize.reset();
  }
}

export const columns = new ColumnsStore();
