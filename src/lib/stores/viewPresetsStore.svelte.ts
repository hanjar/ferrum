/**
 * Custom View Modes (F2-27.4): named presets of the per-pane view state
 * that already exists — view mode (Brief/Full/Thumbnail), sorting,
 * reversed, and the filter preset. Persisted to localStorage like the
 * hotlist and saved searches; malformed entries are dropped on load. A
 * plugin API for NEW column kinds stays explicitly out of scope (D33).
 */
import { viewState, type FilterMode, type ViewMode } from './viewStateStore.svelte';
import { ui, type PaneId } from '../uiState.svelte';
import type { SortSpec } from '../selection';

export interface ViewPreset {
  name: string;
  viewMode: ViewMode;
  sort: SortSpec;
  reversed: boolean;
  filterMode: FilterMode;
  filterPattern: string;
}

const KEY = 'ferrum.viewPresets.v1';
export const VIEW_PRESET_LIMIT = 30;

const VIEW_MODES: ViewMode[] = ['brief', 'full', 'thumbnail'];
const FILTER_MODES: FilterMode[] = ['all', 'programs', 'custom', 'selected'];

/** Parse and validate a persisted payload; suspect entries are dropped. */
export function parseViewPresets(raw: string | null): ViewPreset[] {
  if (!raw) return [];
  let data: unknown;
  try {
    data = JSON.parse(raw);
  } catch {
    return [];
  }
  if (!Array.isArray(data)) return [];
  const seen: string[] = []; // plain dedupe list — nothing reactive here
  const out: ViewPreset[] = [];
  for (const item of data) {
    if (typeof item !== 'object' || item === null) continue;
    const p = item as Record<string, unknown>;
    if (typeof p.name !== 'string' || p.name.trim() === '') continue;
    if (!VIEW_MODES.includes(p.viewMode as ViewMode)) continue;
    if (!FILTER_MODES.includes(p.filterMode as FilterMode)) continue;
    const sort = p.sort as SortSpec | undefined;
    if (
      !sort ||
      !['name', 'size', 'modified', 'extension', 'unsorted'].includes(sort.key) ||
      (sort.dir !== 1 && sort.dir !== -1)
    ) {
      continue;
    }
    if (seen.includes(p.name)) continue;
    seen.push(p.name);
    out.push({
      name: p.name,
      viewMode: p.viewMode as ViewMode,
      sort: { key: sort.key, dir: sort.dir },
      reversed: p.reversed === true,
      filterMode: p.filterMode as FilterMode,
      filterPattern: typeof p.filterPattern === 'string' ? p.filterPattern : '',
    });
  }
  return out;
}

export function persistViewPresets(
  presets: ViewPreset[],
  storage: Storage | undefined = globalThis.localStorage,
): void {
  try {
    storage?.setItem(KEY, JSON.stringify(presets));
  } catch {
    /* storage full/unavailable — saving is best-effort */
  }
}

class ViewPresetsStore {
  open = $state(false);
  presets = $state<ViewPreset[]>(parseViewPresets(globalThis.localStorage?.getItem(KEY) ?? null));
  /** Name input bound to the save field. */
  draftName = $state('');

  openDialog(): void {
    this.open = true;
    this.draftName = '';
  }

  close(): void {
    this.open = false;
  }

  entries(): ViewPreset[] {
    return this.presets;
  }

  /** Snapshot the ACTIVE pane's view state under a name. */
  save(pane: PaneId, rawName: string): void {
    const name = rawName.trim();
    if (!name) return;
    const preset: ViewPreset = {
      name,
      viewMode: viewState.viewMode(pane),
      sort: { ...viewState.sort(pane) },
      reversed: viewState.reversed(pane),
      filterMode: viewState.filterMode(pane),
      filterPattern: viewState.filterPattern(pane),
    };
    this.presets = [preset, ...this.presets.filter((p) => p.name !== name)].slice(
      0,
      VIEW_PRESET_LIMIT,
    );
    persistViewPresets(this.presets);
  }

  /** Apply a preset to a pane (the dialog applies to the active pane). */
  apply(preset: ViewPreset, pane: PaneId): void {
    viewState.setViewMode(pane, preset.viewMode);
    viewState.setSort(pane, preset.sort.key, preset.sort.dir);
    if (viewState.reversed(pane) !== preset.reversed) viewState.toggleReversed(pane);
    viewState.setFilterMode(pane, preset.filterMode, preset.filterPattern);
  }

  remove(name: string): void {
    this.presets = this.presets.filter((p) => p.name !== name);
    persistViewPresets(this.presets);
  }

  /** Test hook. */
  reset(): void {
    this.presets = [];
    this.draftName = '';
    this.open = false;
    persistViewPresets(this.presets);
  }
}

export const viewPresets = new ViewPresetsStore();

/** Convenience opener bound to the active pane. */
export function saveActivePreset(name: string): void {
  viewPresets.save(ui.activePane, name);
}

/** Apply to the active pane. */
export function applyPresetActive(preset: ViewPreset): void {
  viewPresets.apply(preset, ui.activePane);
}
