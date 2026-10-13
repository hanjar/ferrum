<script lang="ts">
  /**
   * Compact EXIF / media metadata list (PRV-06). Embed under the basic file
   * info in Quick View: <MediaMetadata path={entry?.path ?? null} />.
   * Renders nothing when `ui.preview.readMetadata` is OFF or there is no
   * data. GPS is plain coordinate text only: no map, link or external tile.
   */
  import { untrack } from 'svelte';
  import { ui } from '../uiState.svelte';
  import {
    formatDimensions,
    formatDuration,
    formatGps,
    mediaMetaStore as store,
  } from '../stores/mediaMetaStore.svelte';

  let { path = null }: { path?: string | null } = $props();

  // Re-run on target change AND on the readMetadata toggle; the store does
  // the privacy gate and invalidates in-flight requests.
  $effect(() => {
    const p = path;
    void ui.preview.readMetadata;
    untrack(() => store.setTarget(p));
  });

  const rows = $derived.by((): [string, string][] => {
    const m = store.data;
    if (!ui.preview.readMetadata || !m) return [];
    const out: [string, string][] = [];
    const dim = formatDimensions(m);
    if (dim) out.push(['Dimensions', dim]);
    if (m.durationMs !== undefined) out.push(['Duration', formatDuration(m.durationMs)]);
    const cam = [m.camera?.make, m.camera?.model].filter(Boolean).join(' ');
    if (cam) out.push(['Camera', cam]);
    if (m.takenAt) out.push(['Taken', m.takenAt]);
    if (m.orientation !== undefined) out.push(['Orientation', String(m.orientation)]);
    if (m.gps) out.push(['GPS', formatGps(m.gps.lat, m.gps.lon)]);
    return [...out, ...m.other];
  });
</script>

{#if rows.length > 0}
  <dl class="media-meta" aria-label="Media metadata" data-testid="media-metadata">
    {#each rows as [label, value], i (i)}
      <dt>{label}</dt>
      <dd>{value}</dd>
    {/each}
  </dl>
{/if}

<style>
  .media-meta {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 2px 10px;
    margin: 6px 0 0;
    font-size: 12px;
  }
  dt {
    opacity: 0.75;
  }
  dd {
    margin: 0;
    overflow-wrap: anywhere;
  }
</style>
