<script lang="ts">
  import {
    PANE_SPLIT_DEFAULT,
    PANE_SPLIT_MAX,
    PANE_SPLIT_MIN,
    clampPaneSplit,
  } from '../uiState.svelte';

  /**
   * Draggable divider between the dual panes (FER-406). `orientation` says
   * how the PANES are arranged: 'columns' = side by side (vertical divider,
   * ArrowLeft/Right), 'rows' = stacked (horizontal divider, ArrowUp/Down).
   * The drag measures the surrounding `[data-pane-grid]` so this component
   * stays decoupled from App's layout internals.
   */
  let {
    orientation,
    ratio = $bindable(PANE_SPLIT_DEFAULT),
  }: {
    orientation: 'columns' | 'rows';
    ratio: number;
  } = $props();

  let el = $state<HTMLElement | undefined>();

  const pct = $derived(Math.round(clampPaneSplit(ratio) * 100));

  let dragging = $state(false);
  /** Grid rect captured once at pointerdown; fr units absorb window resizes. */
  let dragRect: DOMRect | null = null;

  function onPointerDown(e: PointerEvent) {
    if (e.button !== 0) return;
    const grid = el?.closest<HTMLElement>('[data-pane-grid]');
    if (!grid) return;
    const rect = grid.getBoundingClientRect();
    if (rect.width <= 0 && rect.height <= 0) return;
    dragRect = rect;
    dragging = true;
    el?.setPointerCapture(e.pointerId);
  }

  function onPointerMove(e: PointerEvent) {
    if (!dragging || !dragRect) return;
    ratio = clampPaneSplit(
      orientation === 'columns'
        ? (e.clientX - dragRect.left) / dragRect.width
        : (e.clientY - dragRect.top) / dragRect.height,
    );
  }

  function endDrag(e: PointerEvent) {
    if (!dragging) return;
    dragging = false;
    dragRect = null;
    if (el?.hasPointerCapture(e.pointerId)) el.releasePointerCapture(e.pointerId);
  }

  function onKeydown(e: KeyboardEvent) {
    // FER-482: Ctrl+Left/Right belong to the global "open in other pane" shortcuts.
    if (e.ctrlKey || e.metaKey) return;
    const step = 0.02;
    let next: number | null = null;
    if (orientation === 'columns') {
      if (e.key === 'ArrowLeft') next = ratio - step;
      else if (e.key === 'ArrowRight') next = ratio + step;
    } else {
      if (e.key === 'ArrowUp') next = ratio - step;
      else if (e.key === 'ArrowDown') next = ratio + step;
    }
    if (e.key === 'Home') next = PANE_SPLIT_MIN;
    else if (e.key === 'End') next = PANE_SPLIT_MAX;
    if (next === null) return;
    e.preventDefault();
    ratio = clampPaneSplit(next);
  }

  // Pointer capture keeps move events flowing while over the panes; the body
  // cursor/user-select keeps the OS out of text selection mid-drag.
  $effect(() => {
    if (!dragging) return;
    const prevCursor = document.body.style.cursor;
    const prevUserSelect = document.body.style.userSelect;
    document.body.style.cursor = orientation === 'columns' ? 'col-resize' : 'row-resize';
    document.body.style.userSelect = 'none';
    return () => {
      document.body.style.cursor = prevCursor;
      document.body.style.userSelect = prevUserSelect;
    };
  });
</script>

<!-- A focusable role="separator" is a standard interactive widget (ARIA
     splitter pattern); the a11y rules just don't model it. -->
<!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
<div
  bind:this={el}
  class="divider"
  data-testid="pane-divider"
  class:rows={orientation === 'rows'}
  class:dragging
  role="separator"
  tabindex="0"
  aria-orientation={orientation === 'columns' ? 'vertical' : 'horizontal'}
  aria-valuemin={PANE_SPLIT_MIN * 100}
  aria-valuemax={PANE_SPLIT_MAX * 100}
  aria-valuenow={pct}
  aria-valuetext={`${pct}% / ${100 - pct}%`}
  aria-label="Ukuran panel"
  onpointerdown={onPointerDown}
  onpointermove={onPointerMove}
  onpointerup={endDrag}
  onpointercancel={endDrag}
  onlostpointercapture={endDrag}
  ondblclick={() => (ratio = PANE_SPLIT_DEFAULT)}
  onkeydown={onKeydown}
></div>

<style>
  .divider {
    width: 5px;
    background: var(--line);
    touch-action: none;
    cursor: col-resize;
    transition: background 0.12s;
  }
  .divider.rows {
    width: auto;
    height: 5px;
    cursor: row-resize;
  }
  .divider:hover,
  .divider.dragging {
    background: var(--accentSoft);
  }
  .divider.dragging {
    background: var(--accentLine);
  }
</style>
