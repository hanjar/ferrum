<script lang="ts">
  import FileIcon from './FileIcon.svelte';
  import type { Row } from '../stores/paneStore.svelte';
  import ThumbImage from './ThumbImage.svelte';
  import { createGrantBatcher, mediaRevoke, newMediaScope } from '../ipc/media';
  import { isInTauri } from '../ipc/tauri';
  import { onDestroy } from 'svelte';
  import { ui } from '../uiState.svelte';
  import { displayName } from '../columns';

  let root = $state<HTMLElement | undefined>();

  // FER-489: keep the cursor cell visible after navigation / refresh.
  $effect(() => {
    void cursor;
    root?.querySelector<HTMLElement>('.cell.cursor')?.scrollIntoView?.({ block: 'nearest' });
  });

  let {
    rows,
    selected = new Set<string>(),
    cursor,
    onrowclick,
    onrowactivate,
    onup,
    /** F2-27 Thumbnail View: image rows preview through the `ferrum-media` protocol (FER-452). */
    thumbnails = false,
  }: {
    rows: Row[];
    selected?: Set<string>;
    cursor?: string;
    onrowclick?: (row: Row, e: MouseEvent) => void;
    onrowactivate?: (row: Row) => void;
    onup?: () => void;
    thumbnails?: boolean;
  } = $props();

  // Only clear image extensions preview; lazy + async decoding keeps large
  // folders responsive (no dedicated Rust cache — the webview's image
  // cache is the cache; honest scope, see docs/verification-matrix.md).
  const IMG_EXT = /\.(png|jpe?g|gif|webp|bmp|ico|tiff?)$/i;

  function isThumbable(path: string): boolean {
    return IMG_EXT.test(path) && isInTauri();
  }

  // Thumbnail tokens live in one scope per grid: dropped when the listing
  // changes (navigation / refresh) and when the grid goes away. A thumbnail
  // still on screen re-grants itself if its token was revoked before it loaded.
  const batcher = createGrantBatcher(newMediaScope('grid'));
  let lastRows: Row[] | null = null;
  $effect(() => {
    const current = rows;
    if (lastRows && lastRows !== current) void mediaRevoke(batcher.scope);
    lastRows = current;
  });
  onDestroy(() => void mediaRevoke(batcher.scope));
</script>

<div class="body" role="grid" aria-label="File grid" bind:this={root}>
  <div class="grid" class:thumbs={thumbnails}>
    <div class="cell up" role="gridcell" onclick={() => onup?.()} title="Parent folder">
      <span class="ico">
        <svg
          width="26"
          height="26"
          viewBox="0 0 24 24"
          fill="none"
          stroke="var(--text3)"
          stroke-width="1.6"
          stroke-linecap="round"
          stroke-linejoin="round"
          aria-hidden="true"
        >
          <path d="M9 14L4 9l5-5" /><path d="M4 9h10.5a5.5 5.5 0 0 1 0 11H11" />
        </svg>
      </span>
      <span class="name">..</span>
    </div>
    {#each rows as r (r.entry.path)}
      <div
        class="cell"
        class:selected={selected.has(r.entry.path)}
        class:cursor={cursor === r.entry.path}
        role="gridcell"
        aria-selected={selected.has(r.entry.path)}
        data-path={r.entry.path}
        onclick={(e) => onrowclick?.(r, e)}
        ondblclick={() => onrowactivate?.(r)}
      >
        <span class="thumbBox">
          {#if thumbnails && isThumbable(r.entry.path)}
            <ThumbImage path={r.entry.path} {batcher} />
          {:else}
            <FileIcon kind={r.kind} name={r.entry.name} size={38} variant="grid" />
          {/if}
        </span>
        <span class="name">{displayName(r.entry.name, r.entry.isDir, ui.showExtensions)}</span>
      </div>
    {/each}
  </div>
</div>

<style>
  .body {
    flex: 1;
    min-height: 0;
    overflow: auto;
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(96px, 1fr));
    gap: 4px;
    padding: 10px;
  }
  /* Thumbnails: bigger tiles for the preview boxes (F2-27). */
  .grid.thumbs {
    grid-template-columns: repeat(auto-fill, minmax(124px, 1fr));
  }
  .cell {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 7px;
    padding: 12px 6px 9px;
    border-radius: var(--rl);
    background-color: transparent;
    cursor: default;
  }
  .cell:hover {
    background-image: linear-gradient(var(--hover), var(--hover));
  }
  .cell.selected {
    background-color: var(--sel);
  }
  :global([data-pane-list]:focus-within) .cell.cursor {
    outline: 2px solid var(--accentLine);
    outline-offset: -2px;
  }
  .cell.cursor {
    box-shadow: inset 0 0 0 1px var(--accentLine);
  }
  .ico {
    height: 38px;
    display: grid;
    place-items: center;
  }
  .thumbBox {
    height: 38px;
    display: grid;
    place-items: center;
  }
  .thumbs .thumbBox {
    height: 72px;
  }
  .name {
    max-width: 100%;
    font-size: calc(11.5px + var(--uiStep, 0px));
    text-align: center;
    line-height: 1.3;
    overflow: hidden;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    -webkit-box-orient: vertical;
    word-break: break-all;
    color: var(--text);
  }
</style>
