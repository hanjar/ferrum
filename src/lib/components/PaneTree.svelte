<!--
  FER-366 Separate Tree: one pane's OWN tree panel (Total Commander's
  per-pane separate tree). Rendered by FilePane above its file list when
  the pane's separate tree is enabled; navigation targets only this pane
  via the pane's own navigate() (history, branch exit, selection reset).
-->
<script lang="ts">
  import TreeSidebar from './TreeSidebar.svelte';
  import { separateTree } from '../stores/separateTreeStore.svelte';
  import type { PaneId } from '../uiState.svelte';

  let { paneId, onnavigate }: { paneId: PaneId; onnavigate: (path: string) => void } = $props();

  const tree = $derived(separateTree.trees[paneId]);

  // Persist this pane's expansion set. Every mutation lands here: chevron
  // clicks go through the shared TreeSidebar toggle, cwd-follow through
  // separateTree.follow — both mutate the same reactive SvelteSet, so this
  // effect fires and the store's echo guard dedupes identical snapshots.
  $effect(() => {
    void [...tree.expanded].length;
    separateTree.persist();
  });
</script>

<div class="paneTree" data-pane-tree={paneId}>
  <TreeSidebar
    {tree}
    onNavigate={onnavigate}
    label={paneId === 0 ? 'Separate tree, left pane' : 'Separate tree, right pane'}
  />
</div>

<style>
  .paneTree {
    /* TC placement: the separate tree sits above the pane's file list and
       yields space to it (shrinks first, never pushes the list out). */
    flex: 0 1 auto;
    min-height: 36px;
    max-height: 40%;
    display: flex;
    overflow: hidden;
    border-bottom: 1px solid var(--line);
  }
</style>
