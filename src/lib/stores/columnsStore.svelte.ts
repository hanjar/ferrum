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
  parseColumnConfig,
  type ColumnConfig,
  type ColumnId,
} from '../columns';

const KEY = 'ferrum.customColumns.v1';

export function persistColumnConfig(
  config: ColumnConfig,
  storage: Storage | undefined = globalThis.localStorage,
): void {
  try {
    storage?.setItem(KEY, JSON.stringify(config));
  } catch {
    /* storage full/unavailable — saving is best-effort */
  }
}

function loadColumnConfig(storage: Storage | undefined = globalThis.localStorage): ColumnConfig {
  const raw = storage?.getItem(KEY);
  if (raw === null || raw === undefined) return [...DEFAULT_COLUMN_CONFIG];
  let data: unknown;
  try {
    data = JSON.parse(raw);
  } catch {
    return [...DEFAULT_COLUMN_CONFIG]; // not JSON → built-in layout
  }
  return parseColumnConfig(data);
}

export { loadColumnConfig };

class ColumnsStore {
  open = $state(false);
  config = $state<ColumnConfig>(loadColumnConfig());

  openDialog(): void {
    this.open = true;
  }

  close(): void {
    this.open = false;
  }

  private commit(next: ColumnConfig): void {
    this.config = next;
    persistColumnConfig(next);
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
    this.commit([...DEFAULT_COLUMN_CONFIG]);
  }

  /** Test hook. */
  resetAll(): void {
    this.open = false;
    try {
      globalThis.localStorage?.removeItem(KEY);
    } catch {
      /* best-effort */
    }
    this.config = [...DEFAULT_COLUMN_CONFIG];
  }
}

export const columns = new ColumnsStore();
