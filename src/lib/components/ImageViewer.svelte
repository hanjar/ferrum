<script lang="ts">
  /**
   * F3 image viewer (FER-593): a full-window overlay with Fit and fixed zoom steps. The image
   * always comes from `read_image_preview`: SVG as inline bytes, big rasters downsampled to 2048 px.
   * It is shown only as `<img src="data:...">` (an <img> never runs SVG script nor loads external
   * resources); SVG must never go through `{@html}`, `<object>`, `<embed>`, `<iframe>`, a blob URL
   * or a CSS background. Modal (`use:dialog`): the panes behind get no key presses.
   */
  import { tick, untrack } from 'svelte';
  import { dialog } from '../dialogKit';
  import { imageViewer } from '../stores/imageViewerStore.svelte';
  import { mediaRevoke, newMediaScope } from '../ipc/media';
  import { readImagePreview, imagePreviewSrc, type ImagePreview } from '../ipc/previewImage';
  import { SVG_EXT } from '../ipc/svgThumbs';
  import { zoomStep } from '../imageZoom';

  const path = $derived(imageViewer.isOpen ? imageViewer.path : undefined);
  const name = $derived(path ? (path.split(/[\\/]/).pop() ?? path) : '');
  const isSvg = $derived(path ? SVG_EXT.test(path) : false);

  let info = $state<ImagePreview | null>(null);
  let src = $state('');
  let error = $state('');
  let loading = $state(false);
  /** null = Fit, else a percentage from ZOOM_LEVELS. */
  let zoom = $state<number | null>(null);
  let natW = $state(0);
  let natH = $state(0);
  let vw = $state(0);
  let vh = $state(0);
  let stage = $state<HTMLElement | undefined>();

  // One load per shown image; the grants of its scope are revoked when the viewer closes or
  // moves on. State is reset untracked so the effect depends on the path only.
  $effect(() => {
    const p = path;
    if (!p) return;
    const scope = newMediaScope('viewer');
    let stale = false;
    untrack(() => {
      info = null;
      src = '';
      error = '';
      loading = true;
      zoom = null;
      natW = 0;
      natH = 0;
    });
    readImagePreview(p)
      .then(async (r) => {
        const s = await imagePreviewSrc(p, r, scope);
        if (stale) return;
        if (!s) throw new Error('This image cannot be shown');
        src = s;
        info = r;
        loading = false;
      })
      .catch((e: unknown) => {
        if (stale) return;
        error = e instanceof Error ? e.message : String(e);
        loading = false;
      });
    return () => {
      stale = true;
      void mediaRevoke(scope);
    };
  });

  const known = $derived(natW > 0 && natH > 0 && vw > 0 && vh > 0);
  /** Scale that shows the whole image: never above 100% for rasters, SVG may grow to the window. */
  const fitScale = $derived(known ? Math.min(vw / natW, vh / natH, isSvg ? Infinity : 1) : 1);
  const fitPercent = $derived(fitScale * 100);
  const scale = $derived(zoom === null ? fitScale : zoom / 100);
  /** Image box in px; without an intrinsic size the fitted window box counts as 100%. */
  const boxStyle = $derived.by(() => {
    if (natW > 0 && natH > 0) {
      return `width:${Math.max(1, natW * scale)}px;height:${Math.max(1, natH * scale)}px`;
    }
    const f = zoom === null ? 1 : zoom / 100;
    return vw > 0 && vh > 0
      ? `width:${vw * f}px;height:${vh * f}px;object-fit:contain`
      : 'max-width:100%;max-height:100%';
  });
  const overflowing = $derived(
    zoom !== null && natW > 0 && natH > 0 && (natW * scale > vw + 1 || natH * scale > vh + 1),
  );

  const zoomLabel = $derived(zoom === null ? 'Fit' : `${zoom}%`);

  async function applyZoom(next: number | null) {
    const el = stage;
    const cx = el ? (el.scrollLeft + el.clientWidth / 2) / Math.max(1, el.scrollWidth) : 0.5;
    const cy = el ? (el.scrollTop + el.clientHeight / 2) / Math.max(1, el.scrollHeight) : 0.5;
    zoom = next;
    if (!el) return;
    await tick();
    el.scrollLeft = cx * el.scrollWidth - el.clientWidth / 2;
    el.scrollTop = cy * el.scrollHeight - el.clientHeight / 2;
  }

  function step(dir: 1 | -1) {
    const next = zoomStep(zoom ?? fitPercent, dir);
    if (next !== null) void applyZoom(next);
  }

  function onload(e: Event) {
    const img = e.currentTarget as HTMLImageElement;
    natW = img.naturalWidth;
    natH = img.naturalHeight;
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.defaultPrevented || e.isComposing || e.ctrlKey || e.metaKey || e.altKey) return;
    const handled = (fn: () => void) => {
      e.preventDefault();
      e.stopPropagation();
      fn();
    };
    switch (e.key) {
      case 'ArrowRight':
      case 'PageDown':
        return handled(() => imageViewer.move(1));
      case 'ArrowLeft':
      case 'PageUp':
        return handled(() => imageViewer.move(-1));
      case 'Home':
        return handled(() => imageViewer.first());
      case 'End':
        return handled(() => imageViewer.last());
      case '+':
      case '=':
        return handled(() => step(1));
      case '-':
        return handled(() => step(-1));
      case '0':
      case 'f':
      case 'F':
        return handled(() => void applyZoom(null));
      case '1':
        return handled(() => void applyZoom(100));
      case 'F3':
        return handled(() => imageViewer.close());
    }
  }

  // Ctrl+wheel zooms (non-passive so the webview itself does not); a plain wheel scrolls the stage.
  function wheelZoom(node: HTMLElement) {
    let acc = 0;
    function onwheel(e: WheelEvent) {
      if (!(e.ctrlKey || e.metaKey)) return;
      e.preventDefault();
      acc += e.deltaY;
      if (Math.abs(acc) < 50) return;
      const up = acc < 0;
      acc = 0;
      step(up ? 1 : -1);
    }
    node.addEventListener('wheel', onwheel, { passive: false });
    return { destroy: () => node.removeEventListener('wheel', onwheel) };
  }

  // Drag-to-pan when the image is larger than the stage.
  let drag: { x: number; y: number; left: number; top: number } | null = null;
  function onpointerdown(e: PointerEvent) {
    if (e.button !== 0 || !stage || !overflowing) return;
    drag = { x: e.clientX, y: e.clientY, left: stage.scrollLeft, top: stage.scrollTop };
    stage.setPointerCapture?.(e.pointerId);
    e.preventDefault();
  }
  function onpointermove(e: PointerEvent) {
    if (!drag || !stage) return;
    stage.scrollLeft = drag.left - (e.clientX - drag.x);
    stage.scrollTop = drag.top - (e.clientY - drag.y);
  }
  function onpointerup(e: PointerEvent) {
    drag = null;
    stage?.releasePointerCapture?.(e.pointerId);
  }
</script>

{#if imageViewer.isOpen}
  <div
    class="viewer"
    role="dialog"
    aria-modal="true"
    aria-label="Image viewer"
    tabindex="-1"
    use:dialog={{ onEscape: () => imageViewer.close(), onEnter: () => imageViewer.close() }}
    onkeydown={onKeydown}
  >
    <div class="bar">
      <span class="name" title={path}>{name}</span>
      <span class="pos mono">{imageViewer.index + 1} / {imageViewer.paths.length}</span>
      <span class="grow"></span>
      {#if info?.downsampled}
        <span class="note" title="Shown from a smaller copy">
          downsampled from {info.originalWidth}×{info.originalHeight}
        </span>
      {/if}
      <button type="button" aria-label="Zoom out" title="Zoom out (-)" onclick={() => step(-1)}
        >−</button
      >
      <span class="zoom mono" aria-live="polite" data-testid="zoom-label">{zoomLabel}</span>
      <button type="button" aria-label="Zoom in" title="Zoom in (+)" onclick={() => step(1)}
        >+</button
      >
      <button type="button" title="Fit to window (0)" onclick={() => void applyZoom(null)}
        >Fit</button
      >
      <button type="button" title="Actual size (1)" onclick={() => void applyZoom(100)}>100%</button
      >
      <button
        type="button"
        aria-label="Close image viewer"
        title="Close (Esc)"
        onclick={() => imageViewer.close()}>✕</button
      >
    </div>
    <div
      class="stage"
      role="presentation"
      class:pan={overflowing}
      bind:this={stage}
      bind:clientWidth={vw}
      bind:clientHeight={vh}
      use:wheelZoom
      {onpointerdown}
      {onpointermove}
      {onpointerup}
      onpointercancel={onpointerup}
    >
      {#if error}
        <p class="msg error" role="alert">{error}</p>
      {:else if loading}
        <p class="msg">Loading…</p>
      {:else if src}
        <img class="image" {src} alt="Image {name}" style={boxStyle} {onload} draggable="false" />
      {/if}
    </div>
  </div>
{/if}

<style>
  .viewer {
    position: fixed;
    inset: 0;
    z-index: 62;
    display: flex;
    flex-direction: column;
    background: var(--elev);
    color: var(--text);
    outline: none;
  }
  .bar {
    flex: none;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 10px;
    border-bottom: 1px solid var(--line);
    font-size: calc(12.5px + var(--uiStep, 0px));
  }
  .name {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-weight: 600;
  }
  .grow {
    flex: 1;
  }
  .pos,
  .zoom,
  .note {
    color: var(--text3);
    white-space: nowrap;
  }
  .zoom {
    min-width: 42px;
    text-align: center;
    color: var(--text2);
  }
  .mono {
    font-family: 'Geist Mono', monospace;
    font-size: calc(11.5px + var(--uiStep, 0px));
  }
  button {
    min-width: 26px;
    height: 24px;
    padding: 0 8px;
    border: 1px solid var(--line2);
    border-radius: var(--r);
    background: transparent;
    color: var(--text);
    font: inherit;
    cursor: default;
  }
  button:hover {
    background: var(--hover2);
  }
  button:focus-visible {
    outline: 2px solid var(--accentLine);
    outline-offset: 1px;
  }
  .stage {
    flex: 1;
    min-height: 0;
    display: flex;
    overflow: auto;
  }
  .stage.pan {
    cursor: grab;
  }
  .image {
    display: block;
    flex: none;
    margin: auto; /* centred when smaller than the stage, scrollable when larger */
    user-select: none;
  }
  .msg {
    margin: auto;
    font-size: calc(12.5px + var(--uiStep, 0px));
    color: var(--text3);
    word-break: break-word;
  }
  .error {
    color: var(--red);
  }
</style>
