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

/** FER-593: Thumbnail View size levels, smallest first. */
export type ThumbSize = 'small' | 'medium' | 'large' | 'xlarge';
export const THUMB_SIZES: readonly ThumbSize[] = ['small', 'medium', 'large', 'xlarge'];
export const DEFAULT_THUMB_SIZE: ThumbSize = 'medium';
/** Preview box height and grid column width in px; Medium is the original look. */
export const THUMB_METRICS: Record<ThumbSize, { box: number; col: number; label: string }> = {
  small: { box: 48, col: 96, label: 'Small' },
  medium: { box: 72, col: 124, label: 'Medium' },
  large: { box: 128, col: 176, label: 'Large' },
  xlarge: { box: 224, col: 272, label: 'Extra large' },
};

/** Validate a stored value; anything unknown (or missing) is Medium. */
export function parseThumbSize(v: unknown): ThumbSize {
  return THUMB_SIZES.includes(v as ThumbSize) ? (v as ThumbSize) : DEFAULT_THUMB_SIZE;
}

export interface PaneViewState {
  viewMode: ViewMode;
  /** FER-593: size level of Thumbnail View (kept while another view mode is shown). */
  thumbSize: ThumbSize;
  sort: SortSpec;
  reversed: boolean;
  filterMode: FilterMode;
  /** Mask for the 'custom' filter mode ('*' and '?' wildcards). */
  filterPattern: string;
}

function initialPane(): PaneViewState {
  return {
    viewMode: 'full',
    thumbSize: DEFAULT_THUMB_SIZE,
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

  setThumbSize(pane: PaneId, size: ThumbSize) {
    this.view(pane).thumbSize = parseThumbSize(size);
  }

  thumbSize(pane: PaneId): ThumbSize {
    return this.view(pane).thumbSize;
  }

  /** One level larger (+1) or smaller (-1), clamped at Small / Extra large. */
  stepThumbSize(pane: PaneId, delta: 1 | -1): ThumbSize {
    const i = THUMB_SIZES.indexOf(this.view(pane).thumbSize) + delta;
    const next = THUMB_SIZES[Math.min(THUMB_SIZES.length - 1, Math.max(0, i))];
    this.view(pane).thumbSize = next;
    return next;
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
