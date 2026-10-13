<script lang="ts">
  import type { Kind } from '../kind';
  import { ui } from '../uiState.svelte';
  import { glyphKindOf, iconAttrs, labelOf } from '../fileIcon';

  // FER-478: `variant` picks the production icon box (README "Render sites"):
  // list 14x16 (rows, search, disk usage), grid 30x36, preview 44x52.
  // Folders keep the folder glyph in every theme.
  let {
    kind,
    name,
    size = 15,
    variant = 'list',
  }: {
    kind: Kind;
    name?: string;
    size?: number;
    variant?: 'list' | 'grid' | 'preview';
  } = $props();

  // FER-378 File icons: monochrome neutralizes only the file/folder glyph
  // color (contract: statuses, overlays and selection are untouched).
  const mono = $derived(ui.iconStyle === 'monochrome');
  const a = $derived(iconAttrs(ui.iconTheme, glyphKindOf(name, kind), mono));
  const box = $derived(
    variant === 'preview'
      ? { w: 44, h: 52 }
      : variant === 'grid'
        ? { w: 30, h: 36 }
        : { w: 14, h: 16 },
  );
  const text = $derived(a.badge && variant !== 'list' ? labelOf(name) : '');
</script>

{#if kind === 'dir'}
  <svg width={size} height={size} viewBox="0 0 16 16" aria-hidden="true">
    <path
      d="M1.5 4.3c0-.9.7-1.6 1.6-1.6h3.1l1.5 1.5h5.2c.9 0 1.6.7 1.6 1.6v5.9c0 .9-.7 1.6-1.6 1.6H3.1c-.9 0-1.6-.7-1.6-1.6z"
      fill={mono ? 'var(--text3)' : 'var(--folder)'}
    />
  </svg>
{:else}
  <span
    class="fi"
    class:grid={variant === 'grid'}
    class:preview={variant === 'preview'}
    data-icon-theme={ui.iconTheme}
    style:width="{box.w}px"
    style:height="{box.h}px"
  >
    <svg
      width={box.w}
      height={box.h}
      viewBox="0 0 14 16"
      fill="none"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
    >
      <path d={a.body} fill={a.bf} fill-opacity={a.bo} stroke={a.bs} stroke-width="1" />
      {#if a.fold}
        <path d={a.fold} fill={a.ff} stroke={a.fs} stroke-width="1" />
      {/if}
      {#if a.badge}
        <rect x=".6" y="8" width={a.bw} height="5" rx="1.2" fill={a.bc} />
      {/if}
      {#if a.glyph}
        <path d={a.glyph} transform={a.gt} stroke={a.gs} stroke-width="1.15" />
      {/if}
    </svg>
    {#if text}<span class="lbl">{text}</span>{/if}
  </span>
{/if}

<style>
  .fi {
    position: relative;
    display: inline-block;
    flex: none;
    line-height: 0;
  }
  .lbl {
    position: absolute;
    display: flex;
    align-items: center;
    justify-content: center;
    color: #fff;
    font-family: 'Geist Mono', var(--mono, ui-monospace, monospace);
    font-weight: 600;
    line-height: 1;
    overflow: hidden;
    white-space: nowrap;
    pointer-events: none;
  }
  .grid .lbl {
    left: 1.3px;
    top: 18px;
    width: 21.8px;
    height: 10.7px;
    font-size: 7px;
  }
  .preview .lbl {
    left: 1.9px;
    top: 26px;
    width: 32px;
    height: 15.7px;
    font-size: 10px;
  }
</style>
