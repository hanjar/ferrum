<script lang="ts">
  import { roving } from '../../dialogKit';
  /**
   * Profile picker (Automation P0.5, reused by P1.5): one card per sync profile. A Ready profile
   * can be chosen; a Not-ready or still-checking one is shown disabled with the backend's reason.
   * Readiness is computed by the backend (SP-8) - this component only displays it.
   */
  import type { ProfileSummary } from '../../ipc/syncProfiles';
  import { readinessLabel, readinessReason } from './syncProfileModel';

  let {
    profiles,
    selectedId = null,
    onselect,
    label = 'Sync profiles',
  }: {
    profiles: ProfileSummary[];
    selectedId?: string | null;
    onselect?: (id: string) => void;
    label?: string;
  } = $props();
</script>

<div class="picker" role="radiogroup" aria-label={label} use:roving>
  {#each profiles as p (p.id)}
    {@const ready = p.readiness.status === 'ready'}
    <button
      type="button"
      role="radio"
      class="card"
      class:sel={selectedId === p.id}
      class:off={!ready}
      aria-checked={selectedId === p.id}
      aria-disabled={!ready}
      disabled={!ready}
      data-profile={p.id}
      onclick={() => ready && onselect?.(p.id)}
    >
      <span class="top">
        <span class="name">{p.name}</span>
        <span class="chip" class:ok={ready} class:warn={p.readiness.status === 'notReady'}>
          {readinessLabel(p.readiness)}
        </span>
      </span>
      <span class="paths">{p.left} ⇄ {p.right}</span>
      <span class="meta">{p.modeLabel}{p.deleteLabel ? ` · ${p.deleteLabel}` : ''}</span>
      {#if p.readiness.status === 'notReady'}
        <span class="reason">{readinessReason(p.readiness)}</span>
      {:else if p.readiness.status === 'checking'}
        <span class="reason">Checking the folders…</span>
      {/if}
    </button>
  {/each}
</div>

<style>
  .picker {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .card {
    display: flex;
    flex-direction: column;
    gap: 3px;
    align-items: stretch;
    text-align: left;
    padding: 8px 10px;
    border: 1px solid var(--line);
    border-radius: var(--r);
    background: transparent;
    color: var(--text);
    cursor: default;
  }
  .card:hover:not(:disabled) {
    background: var(--hover);
  }
  .card.sel {
    border-color: var(--accent);
    background: var(--hover);
  }
  .card.off {
    opacity: 0.65;
  }
  .top {
    display: flex;
    justify-content: space-between;
    gap: 8px;
  }
  .name {
    font-size: 13px;
    font-weight: 600;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .chip {
    flex: none;
    padding: 0 6px;
    border-radius: 8px;
    background: var(--hover2);
    color: var(--text2);
    font-size: 11px;
  }
  .chip.ok {
    background: var(--greenSoft);
    color: var(--greenInk);
  }
  .chip.warn {
    background: var(--redSoft);
    color: var(--redInk);
  }
  .paths,
  .meta,
  .reason {
    font-size: 11.5px;
    color: var(--text3);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .reason {
    white-space: normal;
    color: var(--text2);
  }
</style>
