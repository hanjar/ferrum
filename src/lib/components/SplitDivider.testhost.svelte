<script lang="ts">
  /**
   * Test-only host for SplitDivider.test.ts: exposes the bound ratio so
   * two-way binding can be asserted through the DOM.
   */
  import SplitDivider from './SplitDivider.svelte';

  let {
    orientation = 'columns',
    ratio = $bindable(0.5),
  }: {
    orientation?: 'columns' | 'rows';
    ratio?: number;
  } = $props();
</script>

<div data-pane-grid style="display: grid; width: 200px; height: 200px;">
  <span data-testid="ratio">{ratio}</span>
  <SplitDivider {orientation} bind:ratio />
</div>
