<script lang="ts">
  import { contextMenu } from '../stores/contextMenuStore.svelte';

  /**
   * Owner token: FileList mounts one instance per pane, so only the instance
   * whose token was passed to `contextMenu.open()` renders the (fixed)
   * overlay. Standalone usage (no owner anywhere) renders as usual.
   */
  let { owner }: { owner?: unknown } = $props();

  let menuEl = $state<HTMLElement | undefined>();

  const mine = $derived(contextMenu.isOpen && contextMenu.owner === owner);

  // Initial focus goes to the first enabled item; the row that opened the
  // menu regains focus when it closes (handled by FileList).
  $effect(() => {
    if (!mine) return;
    queueMicrotask(() => firstItem()?.focus());
  });

  function enabledItems(): HTMLButtonElement[] {
    if (!menuEl) return [];
    return Array.from(
      menuEl.querySelectorAll<HTMLButtonElement>('[role="menuitem"]:not([disabled])'),
    );
  }

  function firstItem(): HTMLButtonElement | undefined {
    return enabledItems()[0];
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp') return;
    e.preventDefault();
    const list = enabledItems();
    if (list.length === 0) return;
    const idx = list.indexOf(document.activeElement as HTMLButtonElement);
    const next =
      e.key === 'ArrowDown' ? (idx + 1) % list.length : idx <= 0 ? list.length - 1 : idx - 1;
    list[next].focus();
  }

  function onWindowKeydown(e: KeyboardEvent) {
    if (!contextMenu.isOpen || e.key !== 'Escape') return;
    e.preventDefault();
    contextMenu.close();
  }

  function onWindowMousedown(e: MouseEvent) {
    // Every pane mounts an instance; a non-owner has no menuEl and would
    // otherwise treat a press on the owner's menu as "outside" and close it
    // before the click lands (run() never fires).
    if (!mine) return;
    if (menuEl && e.composedPath().includes(menuEl)) return;
    contextMenu.close();
  }

  /** Resize/scroll changes geometry the clamped position was computed against. */
  function onWindowMoveOrResize() {
    if (contextMenu.isOpen) contextMenu.close();
  }
</script>

<svelte:window
  onkeydown={onWindowKeydown}
  onmousedown={onWindowMousedown}
  onresize={onWindowMoveOrResize}
  onscroll={onWindowMoveOrResize}
/>

{#if mine}
  <div
    class="menu"
    role="menu"
    aria-label="Context menu"
    tabindex="-1"
    bind:this={menuEl}
    onkeydown={onKeydown}
    style:left="{contextMenu.x}px"
    style:top="{contextMenu.y}px"
  >
    {#each contextMenu.items as it (it)}
      {#if 'separator' in it}
        <div class="sep" role="separator"></div>
      {:else}
        <button
          class="item"
          class:danger={it.danger}
          role="menuitem"
          tabindex="-1"
          disabled={it.disabled}
          onmousedown={it.disabled
            ? undefined
            : (e) => {
                e.preventDefault();
                if (!contextMenu.isOpen) return;
                contextMenu.close();
                it.run();
              }}
          onclick={it.disabled
            ? undefined
            : () => {
                if (!contextMenu.isOpen) return; // already ran on mousedown
                contextMenu.close();
                it.run();
              }}
        >
          <span class="label">{it.label}</span>
          {#if it.kbd}<span class="kbd">{it.kbd}</span>{/if}
        </button>
      {/if}
    {/each}
  </div>
{/if}

<style>
  .menu {
    position: fixed;
    width: 236px;
    padding: 4px;
    border-radius: calc(var(--r) + 2px);
    background: var(--elev);
    box-shadow: var(--shadow);
    z-index: 51;
    animation: fmPop 120ms ease-out;
    transform-origin: top left;
  }
  .sep {
    height: 1px;
    margin: 4px 6px;
    background: var(--line);
  }
  .item {
    width: 100%;
    height: 26px;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 0 8px;
    border: 0;
    border-radius: var(--r);
    background: transparent;
    font: inherit;
    font-size: calc(12.5px + var(--uiStep, 0px));
    color: var(--text);
    text-align: left;
    cursor: default;
  }
  .item:hover,
  .item:focus-visible {
    outline: none;
    background: var(--accent);
    color: #fff;
  }
  .item:disabled {
    opacity: 0.5;
  }
  .item.danger {
    color: var(--red);
  }
  .label {
    flex: 1;
    white-space: nowrap;
  }
  .kbd {
    font-family: 'Geist Mono', monospace;
    font-size: calc(10.5px + var(--uiStep, 0px));
    opacity: 0.6;
  }
  @keyframes fmPop {
    from {
      opacity: 0;
      transform: translateY(-4px) scale(0.985);
    }
    to {
      opacity: 1;
      transform: translateY(0) scale(1);
    }
  }
</style>
