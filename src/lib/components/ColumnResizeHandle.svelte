<script lang="ts">
  /**
   * Generic draggable/keyboard column resizer handle (FER-540, FER-541).
   *
   * Following the ARIA splitter / separator pattern modeled in SplitDivider.svelte:
   * - role="separator", aria-orientation="vertical", aria-valuenow, aria-valuemin, aria-valuemax.
   * - Mouse drag with pointer capture (pointerdown -> pointermove -> pointerup/cancel/lostpointercapture).
   * - Keyboard ArrowLeft / ArrowRight adjusts by step; Home/End jumps to min/max.
   * - Double-click resets to default via onreset.
   * - Changes are committed on pointerup, keyboard, or reset (not on every pointermove).
   *
   * NOTE: The parent element of ColumnResizeHandle MUST have `position: relative`
   * (or sticky/absolute) because this handle is absolutely positioned (`top: 0;`).
   */
  let {
    label,
    value,
    min,
    max = 2000,
    step = 16,
    side = 'right',
    onchange,
    oncommit,
    oncancel,
    onreset,
  }: {
    label: string;
    value: number;
    min: number;
    max?: number;
    step?: number;
    side?: 'left' | 'right';
    onchange: (width: number) => void;
    oncommit?: (width: number) => void;
    oncancel?: () => void;
    onreset?: () => void;
  } = $props();

  let el = $state<HTMLElement | undefined>();
  let dragging = $state(false);
  let dragStartX = 0;
  let dragStartWidth = 0;

  function clamp(w: number): number {
    return Math.max(min, Math.min(max, Math.round(w)));
  }

  function onPointerDown(e: PointerEvent) {
    if (e.button !== 0) return;
    e.stopPropagation();
    dragStartX = e.clientX;
    dragStartWidth = value;
    dragging = true;
    el?.setPointerCapture?.(e.pointerId);
  }

  function onPointerMove(e: PointerEvent) {
    if (!dragging) return;
    const sign = side === 'left' ? -1 : 1;
    const delta = (e.clientX - dragStartX) * sign;
    const next = clamp(dragStartWidth + delta);
    onchange(next);
  }

  function endDrag(e: PointerEvent) {
    if (!dragging) return;
    dragging = false;
    if (el?.hasPointerCapture?.(e.pointerId)) {
      try {
        el.releasePointerCapture(e.pointerId);
      } catch {
        /* ignore */
      }
    }
    const sign = side === 'left' ? -1 : 1;
    const delta = (e.clientX - dragStartX) * sign;
    const finalWidth = clamp(dragStartWidth + delta);
    oncommit?.(finalWidth);
  }

  function cancelDrag(e?: PointerEvent) {
    if (!dragging) return;
    dragging = false;
    if (e && el?.hasPointerCapture?.(e.pointerId)) {
      try {
        el.releasePointerCapture(e.pointerId);
      } catch {
        /* ignore */
      }
    }
    onchange(dragStartWidth);
    oncancel?.();
  }

  function onKeydown(e: KeyboardEvent) {
    const sign = side === 'left' ? -1 : 1;
    let next: number | null = null;
    if (e.key === 'ArrowLeft') next = value - step * sign;
    else if (e.key === 'ArrowRight') next = value + step * sign;
    else if (e.key === 'Home') next = min;
    else if (e.key === 'End') next = max;

    if (next === null) return;
    e.preventDefault();
    const clamped = clamp(next);
    onchange(clamped);
    oncommit?.(clamped);
  }

  function onClick(e: MouseEvent) {
    e.stopPropagation();
  }

  function onDblClick(e: MouseEvent) {
    e.stopPropagation();
    onreset?.();
  }

  $effect(() => {
    if (!dragging) return;
    const prevCursor = document.body.style.cursor;
    const prevUserSelect = document.body.style.userSelect;
    document.body.style.cursor = 'col-resize';
    document.body.style.userSelect = 'none';
    return () => {
      document.body.style.cursor = prevCursor;
      document.body.style.userSelect = prevUserSelect;
    };
  });
</script>

<!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
<div
  bind:this={el}
  class="colResizer"
  class:side-left={side === 'left'}
  class:side-right={side === 'right'}
  class:dragging
  role="separator"
  tabindex="0"
  aria-orientation="vertical"
  aria-label={label}
  aria-valuemin={min}
  aria-valuemax={max}
  aria-valuenow={value}
  onpointerdown={onPointerDown}
  onpointermove={onPointerMove}
  onpointerup={endDrag}
  onpointercancel={cancelDrag}
  onlostpointercapture={cancelDrag}
  onclick={onClick}
  ondblclick={onDblClick}
  onkeydown={onKeydown}
></div>

<style>
  .colResizer {
    position: absolute;
    top: 0;
    width: 8px;
    height: 100%;
    cursor: col-resize;
    user-select: none;
    touch-action: none;
    z-index: 2;
  }
  .colResizer.side-right {
    right: 0;
  }
  .colResizer.side-left {
    left: 0;
  }
  .colResizer.side-right::after {
    content: '';
    position: absolute;
    top: 20%;
    bottom: 20%;
    right: 3px;
    width: 1px;
    background: var(--line2);
  }
  .colResizer.side-left::after {
    content: '';
    position: absolute;
    top: 20%;
    bottom: 20%;
    left: 3px;
    width: 1px;
    background: var(--line2);
  }
  .colResizer:hover::after,
  .colResizer:focus-visible::after,
  .colResizer.dragging::after {
    background: var(--accent);
    width: 2px;
  }
  .colResizer:focus-visible {
    outline: none;
  }
</style>
