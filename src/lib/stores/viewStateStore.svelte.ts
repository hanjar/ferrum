/**
 * Per-pane Show/view state (F2-15): view mode (Brief/Full), sorting
 * (Name/Extension/Time/Size/Unsorted + reversed), and the filter preset
 * (All files / Programs / Custom mask / Only selected). Total Commander
 * keeps these per pane; so does Ferrum. Components read, actions write.
 */
import type { PaneId } from '../uiState.svelte';
import type { SortSpec } from '../selection';

export type ViewMode = 'brief' | 'full' | 'thumbnail';
export type FilterMode = 'all' | 'programs' | 'custom' | 'selected';

export interface PaneViewState {
  viewMode: ViewMode;
  sort: SortSpec;
  reversed: boolean;
  filterMode: FilterMode;
  /** Mask for the 'custom' filter mode ('*' and '?' wildcards). */
  filterPattern: string;
}

function initialPane(): PaneViewState {
  return {
    viewMode: 'full',
    sort: { key: 'name', dir: 1 },
    reversed: false,
    filterMode: 'all',
    filterPattern: '',
  };
}

class ViewStateStore {
  panes = $state<Record<PaneId, PaneViewState>>({
    0: initialPane(),
    1: initialPane(),
  });

  view(pane: PaneId): PaneViewState {
    return this.panes[pane] ?? (this.panes[pane] = initialPane());
  }

  setViewMode(pane: PaneId, mode: ViewMode) {
    this.view(pane).viewMode = mode;
  }

  viewMode(pane: PaneId): ViewMode {
    return this.view(pane).viewMode;
  }

  setSort(pane: PaneId, key: SortSpec['key'], dir: 1 | -1 = 1) {
    const v = this.view(pane);
    // Same key toggles direction (header behaviour); a new key starts asc.
    v.sort = v.sort.key === key ? { key, dir: v.sort.dir === 1 ? -1 : 1 } : { key, dir };
  }

  sort(pane: PaneId): SortSpec {
    return this.view(pane).sort;
  }

  toggleReversed(pane: PaneId) {
    this.view(pane).reversed = !this.view(pane).reversed;
  }

  reversed(pane: PaneId): boolean {
    return this.view(pane).reversed;
  }

  setFilterMode(pane: PaneId, mode: FilterMode, pattern?: string) {
    const v = this.view(pane);
    v.filterMode = mode;
    if (pattern !== undefined) v.filterPattern = pattern;
  }

  filterMode(pane: PaneId): FilterMode {
    return this.view(pane).filterMode;
  }

  filterPattern(pane: PaneId): string {
    return this.view(pane).filterPattern;
  }
}

export const viewState = new ViewStateStore();
