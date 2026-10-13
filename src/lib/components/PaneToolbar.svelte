<script lang="ts">
  import { tick } from 'svelte';
  import Icon from './Icon.svelte';
  import AddressBar from './AddressBar.svelte';
  import { view } from '../stores/viewStore.svelte';
  import { ui } from '../uiState.svelte';

  import type { Crumb } from '../crumbs';

  let {
    crumbs,
    path = '',
    editing = $bindable(false),
    onnavigate,
    onhotlist,
    canBack = false,
    canFwd = false,
    dim = false,
    filterValue = '',
    onfilter,
    onback,
    onforward,
    onup,
    oncrumb,
    onDiskUsage,
  }: {
    crumbs: Crumb[];
    /** FER-456: current folder, the editable address bar's base. */
    path?: string;
    editing?: boolean;
    onnavigate?: (path: string) => void;
    onhotlist?: () => void;
    canBack?: boolean;
    canFwd?: boolean;
    dim?: boolean;
    filterValue?: string;
    onfilter?: (q: string) => void;
    onback?: () => void;
    onforward?: () => void;
    onup?: () => void;
    oncrumb?: (path: string) => void;
    /** DSK-04: opens the Disk usage pane MODE for this pane (Alt+Shift+D parity). */
    onDiskUsage?: () => void;
  } = $props();

  // --- Filter: an icon until it is opened or holds a query (FER-491) ---
  let filterOpen = $state(false);
  let filterInput = $state<HTMLInputElement | undefined>();
  const filterExpanded = $derived(filterOpen || filterValue !== '');

  async function openFilter() {
    filterOpen = true;
    await tick();
    filterInput?.focus();
  }
  function onFilterKey(e: KeyboardEvent) {
    if (e.key !== 'Escape') return;
    e.preventDefault();
    e.stopPropagation();
    onfilter?.('');
    filterOpen = false;
  }
  function onFilterBlur() {
    // An empty box folds back to the icon; a live query keeps it open.
    if (filterValue === '') filterOpen = false;
  }

  // --- View: one button that opens a List / Details / Grid menu ---
  type ViewItem = { label: string; mode: 'details' | 'grid'; icon: string; disabled?: boolean };
  const VIEW_ITEMS: ViewItem[] = [
    { label: 'List', mode: 'details', icon: 'viewList', disabled: true },
    { label: 'Details', mode: 'details', icon: 'viewDetails' },
    { label: 'Grid', mode: 'grid', icon: 'viewGrid' },
  ];
  const viewLabel = $derived(view.mode === 'grid' ? 'Grid' : 'Details');
  let viewOpen = $state(false);
  let viewWrap = $state<HTMLDivElement | undefined>();

  const menuItems = (): HTMLButtonElement[] =>
    Array.from(viewWrap?.querySelectorAll<HTMLButtonElement>('.mi:not(:disabled)') ?? []);

  async function openView() {
    viewOpen = true;
    await tick();
    const items = menuItems();
    (items.find((b) => b.getAttribute('aria-checked') === 'true') ?? items[0])?.focus();
  }
  function closeView(refocus: boolean) {
    viewOpen = false;
    if (refocus) viewWrap?.querySelector<HTMLButtonElement>('.viewBtn')?.focus();
  }
  function pickView(mode: 'details' | 'grid') {
    view.set(mode);
    closeView(true);
  }
  function onViewBtnKey(e: KeyboardEvent) {
    if (e.key === 'ArrowDown' && !viewOpen) {
      e.preventDefault();
      void openView();
    }
  }
  function onViewMenuKey(e: KeyboardEvent) {
    // Menu keys must not reach the pane's list keymap (Enter/arrows would act on rows).
    e.stopPropagation();
    const items = menuItems();
    const i = items.indexOf(document.activeElement as HTMLButtonElement);
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      const d = e.key === 'ArrowDown' ? 1 : -1;
      items[(i + d + items.length) % items.length]?.focus();
    } else if (e.key === 'Home' || e.key === 'End') {
      e.preventDefault();
      items[e.key === 'Home' ? 0 : items.length - 1]?.focus();
    } else if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      closeView(true);
    } else if (e.key === 'Tab') {
      // FER-489: Tab leaves the menu and returns to this pane's file list.
      e.preventDefault();
      closeView(false);
      (e.currentTarget as HTMLElement)
        .closest('section.pane')
        ?.querySelector<HTMLElement>('[data-pane-list]')
        ?.focus();
    }
  }
  function onViewFocusOut(e: FocusEvent) {
    const next = e.relatedTarget as Node | null;
    if (next && viewWrap?.contains(next)) return;
    viewOpen = false;
  }
</script>

{#snippet address()}
  <AddressBar {crumbs} {path} bind:editing {oncrumb} {onnavigate} {onhotlist} />
{/snippet}

{#snippet filterBox()}
  {#if filterExpanded}
    <label class="filter" title="Filter">
      <Icon name="filter" size={12} />
      <input
        bind:this={filterInput}
        value={filterValue}
        aria-label="Filter"
        oninput={(e) => onfilter?.((e.currentTarget as HTMLInputElement).value)}
        onkeydown={onFilterKey}
        onblur={onFilterBlur}
        placeholder="Filter"
        spellcheck="false"
      />
    </label>
  {:else}
    <button
      class="ib filterBtn"
      type="button"
      title="Filter"
      aria-label="Filter"
      aria-expanded="false"
      onclick={openFilter}
    >
      <Icon name="filter" size={14} />
    </button>
  {/if}
{/snippet}

{#snippet viewMenu()}
  <div class="viewWrap" bind:this={viewWrap}>
    <button
      class="ib viewBtn"
      type="button"
      title="View: {viewLabel}"
      aria-label="View: {viewLabel}"
      aria-haspopup="menu"
      aria-expanded={viewOpen}
      onclick={() => (viewOpen ? closeView(false) : void openView())}
      onkeydown={onViewBtnKey}
    >
      <Icon name={view.mode === 'grid' ? 'viewGrid' : 'viewDetails'} size={14} />
      <svg class="chev" viewBox="0 0 10 10" width="8" height="8" aria-hidden="true">
        <path
          d="M2 3.5 5 6.5 8 3.5"
          fill="none"
          stroke="currentColor"
          stroke-width="1.4"
          stroke-linecap="round"
          stroke-linejoin="round"
        />
      </svg>
    </button>
    {#if viewOpen}
      <div
        class="menu"
        role="menu"
        aria-label="View"
        tabindex="-1"
        onkeydown={onViewMenuKey}
        onfocusout={onViewFocusOut}
      >
        {#each VIEW_ITEMS as it (it.label)}
          <button
            class="mi"
            type="button"
            role="menuitemradio"
            aria-checked={!it.disabled && view.mode === it.mode}
            disabled={it.disabled}
            title={it.disabled ? 'List view is not available yet' : undefined}
            onclick={() => pickView(it.mode)}
          >
            <Icon name={it.icon} size={13} />
            <span>{it.label}</span>
          </button>
        {/each}
      </div>
    {/if}
  </div>
{/snippet}

{#snippet tools()}
  {@render filterBox()}
  {@render viewMenu()}
  {#if onDiskUsage}
    <button
      class="ib duBtn"
      type="button"
      title="Disk usage (Alt+Shift+D)"
      aria-label="Disk usage"
      onclick={onDiskUsage}
    >
      <Icon name="disk" size={14} />
    </button>
  {/if}
{/snippet}

{#snippet nav()}
  <div class="nav">
    <button class="ib" title="Back" aria-label="Back" disabled={!canBack} onclick={onback}
      ><Icon name="chevLeft" size={15} /></button
    >
    <button class="ib" title="Forward" aria-label="Forward" disabled={!canFwd} onclick={onforward}
      ><Icon name="chevRight" size={15} /></button
    >
    <button class="ib" title="Parent folder" aria-label="Parent folder" onclick={onup}
      ><Icon name="up" size={15} /></button
    >
  </div>
{/snippet}

<div class="toolbar" class:dim class:own={ui.addressOwnRow}>
  {#if ui.addressOwnRow}
    <!-- FER-491: the address bar owns a full-width row; actions sit on the row below. -->
    <div class="line addrLine">{@render address()}</div>
    <div class="line toolLine">
      {@render nav()}
      <span class="grow"></span>
      {@render tools()}
    </div>
  {:else}
    <div class="line">
      {@render nav()}
      {@render address()}
      {@render tools()}
    </div>
  {/if}
</div>

<style>
  .toolbar {
    flex: none;
    display: flex;
    flex-direction: column;
    border-bottom: 1px solid var(--line);
    transition: opacity 160ms;
  }
  .toolbar.dim {
    opacity: 0.55;
  }
  .line {
    height: 36px;
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 0 8px 0 6px;
  }
  .toolbar.own .line {
    height: 32px;
  }
  .toolbar.own .toolLine {
    border-top: 1px solid var(--line);
  }
  .grow {
    flex: 1 1 auto;
  }
  .nav {
    display: flex;
    gap: 1px;
    flex: none;
  }
  .ib {
    flex: none;
    width: 24px;
    height: 24px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: 2px;
    padding: 0;
    border: 0;
    border-radius: var(--r);
    background: transparent;
    color: var(--text2);
    cursor: default;
  }
  .ib:hover:not(:disabled),
  .ib[aria-expanded='true'] {
    background: var(--hover2);
    color: var(--text);
  }
  .ib:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: -2px;
  }
  .ib:disabled {
    opacity: 0.35;
  }
  .viewBtn {
    width: auto;
    padding: 0 4px 0 6px;
  }
  .filter {
    height: 24px;
    flex: 0 1 140px;
    min-width: 72px;
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 0 8px;
    border-radius: var(--r);
    background: var(--hover);
    box-shadow: inset 0 0 0 1px var(--line);
    color: var(--text3);
  }
  .filter:focus-within {
    box-shadow: inset 0 0 0 1px var(--accent);
  }
  .filter input {
    flex: 1;
    min-width: 0;
    border: 0;
    outline: 0;
    background: transparent;
    font-size: calc(12px + var(--uiStep, 0px));
    color: var(--text);
  }
  .viewWrap {
    position: relative;
    flex: none;
  }
  .menu {
    position: absolute;
    top: 100%;
    right: 0;
    margin-top: 3px;
    min-width: 128px;
    padding: 3px;
    display: flex;
    flex-direction: column;
    background: var(--elev);
    border: 1px solid var(--line);
    border-radius: var(--r);
    box-shadow: var(--tabShadow, 0 6px 20px rgba(0, 0, 0, 0.25));
    z-index: 40;
  }
  .mi {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 4px 8px;
    border: 0;
    border-radius: 4px;
    background: transparent;
    color: var(--text);
    font-size: calc(12px + var(--uiStep, 0px));
    text-align: left;
    cursor: default;
  }
  .mi:hover:not(:disabled),
  .mi:focus-visible {
    background: var(--hover2);
    outline: 0;
  }
  .mi[aria-checked='true'] {
    font-weight: 600;
    color: var(--accent);
  }
  .mi:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }
</style>
