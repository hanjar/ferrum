<script lang="ts">
  import Icon from './Icon.svelte';
  import { roving } from '../dialogKit';
  import type { TabState } from '../stores/paneStore.svelte';
  import type { TabWidth } from '../uiState.svelte';

  let {
    tabs,
    current = 0,
    tabWidth = 'fit',
    middleClose = false,
    onselect,
    onclose,
    onnew,
  }: {
    tabs: Pick<TabState, 'label'>[];
    current?: number;
    /** FER-395: 'fit' shrinks to the label; 'fixed' gives uniform 160px tabs. */
    tabWidth?: TabWidth;
    /** FER-395: middle-click (auxclick button 1) closes the clicked tab. */
    middleClose?: boolean;
    onselect?: (i: number) => void;
    onclose?: (i: number) => void;
    onnew?: () => void;
  } = $props();
</script>

<!-- FER-577 (B4): arrows / Home / End move between tabs and select them (one tab stop); Ctrl+Tab,
     Ctrl+Shift+Tab and Ctrl+W are registry shortcuts (tab.next, tab.prev, tab.close). -->
<div class="tabs" role="tablist" class:fixed={tabWidth === 'fixed'} use:roving>
  {#each tabs as t, i (i)}
    <button
      role="tab"
      class="tab"
      class:active={i === current}
      aria-selected={i === current}
      onclick={() => onselect?.(i)}
      onmousedown={(e) => {
        // Suppress the button-1 autoscroll before auxclick closes the tab.
        if (middleClose && e.button === 1) e.preventDefault();
      }}
      onauxclick={(e) => {
        if (!middleClose || e.button !== 1) return;
        e.preventDefault();
        onclose?.(i);
      }}
    >
      <Icon name="folderOutline" size={13} />
      <span class="label">{t.label}</span>
      <span
        class="close"
        aria-hidden="true"
        title="Close tab"
        onclick={(e) => {
          e.stopPropagation();
          onclose?.(i);
        }}
      >
        <Icon name="close" size={10} />
      </span>
    </button>
  {/each}
  <button class="newTab" title="New tab" onclick={() => onnew?.()}>
    <Icon name="plus" size={13} />
  </button>
</div>

<style>
  .tabs {
    height: 36px;
    flex: none;
    display: flex;
    align-items: center;
    gap: 2px;
    padding: 0 6px;
    border-bottom: 1px solid var(--line);
    overflow: hidden;
  }
  /* FER-395 fixed width: uniform tabs can overflow the strip, so it scrolls
     instead of pushing the close/new buttons out of view. */
  .tabs.fixed {
    overflow-x: auto;
  }
  .tabs.fixed .tab {
    flex: none;
    width: 160px;
    min-width: 160px;
    max-width: 160px;
  }
  .tab {
    height: 26px;
    min-width: 0;
    max-width: 180px;
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 0 4px 0 9px;
    border: 0;
    border-radius: var(--r);
    background: transparent;
    color: var(--text3);
    font-size: calc(12.5px + var(--uiStep, 0px));
    cursor: default;
    transition:
      background-color 140ms,
      color 140ms;
  }
  .tab:hover {
    color: var(--text2);
  }
  .tab.active {
    background: var(--elev);
    box-shadow: var(--tabShadow);
    color: var(--text);
  }
  .tab :global(svg) {
    flex: none;
    opacity: 0.8;
  }
  .label {
    min-width: 0;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .close {
    width: 18px;
    height: 18px;
    flex: none;
    display: grid;
    place-items: center;
    border-radius: 4px;
    opacity: 0.55;
    color: var(--text3);
  }
  .close:hover {
    background: var(--hover2);
    color: var(--text);
    opacity: 1;
  }
  .newTab {
    width: 24px;
    height: 24px;
    flex: none;
    display: grid;
    place-items: center;
    border: 0;
    border-radius: var(--r);
    background: transparent;
    color: var(--text3);
    cursor: default;
  }
  .newTab:hover {
    background: var(--hover2);
    color: var(--text);
  }
</style>
