<script lang="ts">
  // Tree mode rows, 1:1 with the prototype's sideTree block: 24px rows,
  // 6 + depth*14 indent, 16px chevron rotating 90° over 140ms.
  import { onMount } from 'svelte';
  import Icon from './Icon.svelte';
  import FileIcon from './FileIcon.svelte';
  import { activePane } from '../stores/panesRegistry.svelte';
  import { TreeStore } from '../stores/treeStore.svelte';

  let {
    tree = new TreeStore(),
    // FER-366: a per-pane tree supplies its own navigation target (its OWN
    // pane). Absent (the global sidebar) keeps the historic behaviour:
    // navigate the ACTIVE pane.
    onNavigate,
    label = 'Folder tree',
  }: { tree?: TreeStore; onNavigate?: (path: string) => void; label?: string } = $props();

  onMount(() => {
    void tree.init();
  });

  /** A tree click both highlights the node and drives the target pane. */
  function navigate(path: string) {
    tree.select(path);
    if (onNavigate) onNavigate(path);
    else activePane()?.openPath?.(path);
  }
</script>

{#snippet node(path: string, name: string, depth: number, isRoot: boolean, meta: string)}
  {@const open = tree.expanded.has(path)}
  {@const kids = tree.childrenOf(path)}
  {@const failed = tree.errorOf(path) !== undefined}
  {@const hasKids = isRoot || tree.isLoading(path) || !tree.hasFetched(path) || kids.length > 0}
  <div
    class="row"
    class:root={isRoot}
    class:sel={tree.selected === path}
    role="treeitem"
    aria-selected={tree.selected === path}
    aria-expanded={hasKids ? open : undefined}
    tabindex="0"
    style:padding-left={`${6 + depth * 14}px`}
    onclick={() => navigate(path)}
    onkeydown={(e) => {
      if (e.key === 'Enter' || e.key === ' ') {
        e.preventDefault();
        navigate(path);
      }
    }}
  >
    {#if hasKids}
      <button
        class="chev"
        aria-label={open ? `Collapse ${name}` : `Expand ${name}`}
        tabindex="-1"
        onclick={(e) => {
          e.stopPropagation();
          void tree.toggle(path);
        }}
      >
        <svg
          class:open
          width="10"
          height="10"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          stroke-width="2.6"
          stroke-linecap="round"
          stroke-linejoin="round"
          aria-hidden="true"
        >
          <path d="M9 6l6 6-6 6" />
        </svg>
      </button>
    {:else}
      <span class="chevSpacer"></span>
    {/if}
    <span class="nodeIcon">
      {#if isRoot}
        <Icon name="drive" size={14} />
      {:else}
        <FileIcon kind="dir" size={14} />
      {/if}
    </span>
    <span class="name">{name}</span>
    {#if meta}<span class="meta">{meta}</span>{/if}
  </div>
  {#if open}
    {#if tree.isLoading(path)}
      <div class="state" style:padding-left={`${6 + (depth + 1) * 14}px`}>Loading…</div>
    {:else if failed}
      <div class="state" style:padding-left={`${6 + (depth + 1) * 14}px`}>Unavailable</div>
    {:else}
      {#each kids as kid (kid.path)}
        {@render node(kid.path, kid.name, depth + 1, false, '')}
      {/each}
    {/if}
  {/if}
{/snippet}

<div class="tree" role="tree" aria-label={label}>
  {#if tree.rootsLoading}
    <div class="state" style:padding-left="6px">Loading…</div>
  {:else if tree.rootsError}
    <div class="state" style:padding-left="6px">{tree.rootsError.message}</div>
  {:else}
    {#each tree.roots as root (root.path)}
      {@render node(root.path, root.label, 0, true, root.meta)}
    {/each}
  {/if}
</div>

<style>
  .tree {
    flex: 1;
    min-height: 0;
    overflow: auto;
    padding: 4px 6px 14px;
    animation: fmFade 140ms ease-out;
  }
  .row {
    height: 24px;
    display: flex;
    align-items: center;
    gap: 4px;
    padding-right: 6px;
    border-radius: var(--r);
    cursor: default;
    min-width: 0;
  }
  .tree > .row:first-child {
    margin-top: 0;
  }
  .row.root {
    margin-top: 6px;
  }
  .row:hover {
    background: var(--hover);
  }
  .row.sel {
    background: var(--sel);
  }
  .chev {
    width: 16px;
    height: 16px;
    flex: none;
    display: grid;
    place-items: center;
    padding: 0;
    border: 0;
    border-radius: 3px;
    background: transparent;
    color: var(--text3);
    cursor: default;
  }
  .chev:hover {
    background: var(--hover2);
    color: var(--text);
  }
  .chev svg {
    transition: transform 140ms cubic-bezier(0.2, 0.7, 0.2, 1);
  }
  .chev svg.open {
    transform: rotate(90deg);
  }
  .chevSpacer {
    width: 16px;
    height: 16px;
    flex: none;
  }
  .nodeIcon {
    width: 15px;
    flex: none;
    display: grid;
    place-items: center;
    color: var(--folder);
  }
  .row.root .nodeIcon {
    color: var(--text3);
  }
  .name {
    flex: 1;
    min-width: 0;
    margin-left: 3px;
    font-size: calc(12.5px + var(--uiStep, 0px));
    font-weight: 400;
    color: var(--text);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .row.root .name {
    font-weight: 500;
  }
  .row.sel .name {
    color: var(--accentInk);
    font-weight: 500;
  }
  .meta {
    font-family: 'Geist Mono', monospace;
    font-size: calc(10.5px + var(--uiStep, 0px));
    color: var(--text3);
  }
  .state {
    height: 24px;
    display: flex;
    align-items: center;
    font-size: calc(11px + var(--uiStep, 0px));
    color: var(--text3);
  }
</style>
