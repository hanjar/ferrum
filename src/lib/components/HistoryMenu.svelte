<script lang="ts">
  // Alt+Down popup (FER-484): Total Commander's folder history. Top: most visited folders with their
  // visit count; below the line: this tab's history, the current folder last. Enter / click navigates
  // the pane it was opened for through the pane's normal navigation (its error flow included).
  import { tick } from 'svelte';
  import { historyMenu } from '../stores/historyMenuStore.svelte';
  import FileIcon from './FileIcon.svelte';
  import { baseName } from '../stores/paneStore.svelte';

  let root = $state<HTMLElement>();
  let pos = $state({ left: 8, top: 40, width: 360 });
  let previousFocus: HTMLElement | null = null;

  const list = $derived(historyMenu.list);

  function place() {
    const section = document.querySelector<HTMLElement>(`section[data-pane="${historyMenu.pane}"]`);
    const bar = section?.querySelector<HTMLElement>('.address');
    const s = section?.getBoundingClientRect();
    const b = bar?.getBoundingClientRect();
    if (!s || !b) return;
    pos = { left: s.left, top: b.bottom + 2, width: s.width };
  }

  $effect(() => {
    if (!historyMenu.isOpen) return;
    previousFocus = document.activeElement as HTMLElement | null;
    place();
    void tick().then(() => {
      root?.focus();
      root?.querySelector('.row.focus')?.scrollIntoView({ block: 'nearest' });
    });
    return () => {
      const el = previousFocus;
      previousFocus = null;
      queueMicrotask(() => el?.focus?.());
    };
  });

  $effect(() => {
    void historyMenu.cursor;
    if (historyMenu.isOpen)
      void tick().then(() =>
        root?.querySelector('.row.focus')?.scrollIntoView({ block: 'nearest' }),
      );
  });

  function onKeydown(e: KeyboardEvent) {
    if (e.altKey && e.key === 'ArrowDown' && !e.ctrlKey && !e.metaKey) {
      historyMenu.close(); // second Alt+Down closes
    } else if (e.ctrlKey || e.metaKey || e.altKey) {
      return;
    } else if (e.key === 'ArrowDown') historyMenu.move(1);
    else if (e.key === 'ArrowUp') historyMenu.move(-1);
    else if (e.key === 'Home') historyMenu.move(-1e6);
    else if (e.key === 'End') historyMenu.move(1e6);
    else if (e.key === 'PageDown') historyMenu.move(8);
    else if (e.key === 'PageUp') historyMenu.move(-8);
    else if (e.key === 'Enter') historyMenu.select(list.rows[historyMenu.cursor]);
    else if (e.key === 'Escape') historyMenu.close();
    else if (e.key === 'Tab') {
      /* focus stays in the popup */
    } else return;
    e.preventDefault();
    e.stopPropagation();
  }
</script>

{#snippet item(row: (typeof list.rows)[number], index: number)}
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <div
    class="row"
    class:focus={historyMenu.cursor === index}
    class:cur={row.current}
    role="option"
    tabindex="-1"
    aria-selected={historyMenu.cursor === index}
    aria-current={row.current ? 'location' : undefined}
    title={row.path}
    data-path={row.path}
    onmouseenter={() => (historyMenu.cursor = index)}
    onclick={() => historyMenu.select(row)}
  >
    <span class="ico"
      ><FileIcon
        kind={row.kind === 'archive' ? 'archive' : 'dir'}
        name={baseName(row.path)}
      /></span
    >
    <span class="lbl">{row.path}</span>
    {#if row.network}
      <span class="net" title="Network folder: opening it contacts another computer">net</span>
    {/if}
    {#if row.count !== undefined}
      <span class="cnt" aria-label="{row.count} visits">{row.count}</span>
    {/if}
  </div>
{/snippet}

{#if historyMenu.isOpen}
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="scrim" onmousedown={() => historyMenu.close()}></div>
  <div
    class="menu"
    bind:this={root}
    role="listbox"
    aria-label="Folder history"
    tabindex="-1"
    onkeydown={onKeydown}
    style:left="{pos.left}px"
    style:top="{pos.top}px"
    style:width="{pos.width}px"
  >
    {#if historyMenu.topWithheld}
      <p class="info" role="status">
        Frequent folders are hidden while activity recording is off or not yet confirmed.
      </p>
    {/if}
    {#each list.top as row, i (i)}
      {@render item(row, i)}
    {/each}
    {#if list.top.length > 0 && list.bottom.length > 0}
      <div class="sep" role="separator"></div>
    {/if}
    {#each list.bottom as row, j (j)}
      {@render item(row, list.top.length + j)}
    {/each}
  </div>
{/if}

<style>
  .scrim {
    position: fixed;
    inset: 0;
    z-index: 70;
  }
  .menu {
    position: fixed;
    z-index: 71;
    box-sizing: border-box;
    max-height: min(60vh, 440px);
    overflow-y: auto;
    padding: 4px;
    outline: none;
    background: var(--elev);
    border: 1px solid var(--line);
    border-radius: 8px;
    box-shadow: var(--shadow);
  }
  .sep {
    height: 1px;
    margin: 4px 6px;
    background: var(--line);
  }
  .info {
    margin: 0 0 4px;
    padding: 4px 8px;
    font-size: calc(11px + var(--uiStep, 0px));
    color: var(--text3);
  }
  .row {
    height: 24px;
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 0 8px 0 6px;
    border-radius: 6px;
    font-size: calc(12.5px + var(--uiStep, 0px));
    color: var(--text);
    cursor: default;
  }
  .row.cur {
    background: var(--hover2);
    font-weight: 600;
  }
  .row.focus {
    background: var(--accent);
    color: #fff;
  }
  .ico {
    flex: none;
    display: inline-flex;
    width: 18px;
    justify-content: center;
  }
  .lbl {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .cnt {
    flex: none;
    min-width: 2ch;
    text-align: right;
    font-family: 'Geist Mono', monospace;
    font-size: calc(11.5px + var(--uiStep, 0px));
    color: var(--text3);
  }
  .net {
    flex: none;
    padding: 0 4px;
    border: 1px solid var(--line);
    border-radius: 4px;
    font-family: 'Geist Mono', monospace;
    font-size: calc(10px + var(--uiStep, 0px));
    line-height: 14px;
    color: var(--text3);
  }
  .row.focus .net {
    color: inherit;
    border-color: currentColor;
  }
  .row.focus .cnt {
    color: inherit;
    opacity: 0.85;
  }
</style>
