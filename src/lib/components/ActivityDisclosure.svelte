<script lang="ts">
  /** First-run notice shown while recording is on until acknowledged once. */
  import { activity } from '../stores/activityStore.svelte';

  let { onsettings }: { onsettings?: () => void } = $props();
</script>

{#if activity.needsDisclosure}
  <div class="disclosure" role="region" aria-label="Pemberitahuan pencatatan activity">
    <p>
      Ferrum mencatat folder yang Anda buka dan file yang Anda minta dibuka <strong
        >di Ferrum saja</strong
      >, disimpan lokal di perangkat ini selama {activity.retentionDays} hari dan tidak dikirim ke mana
      pun. Jika Anda bertanya tentang riwayat ini ke asisten AI, pertanyaan yang Anda ketik dikirim ke
      provider AI, sedangkan hasil riwayatnya hanya dibaca dan ditampilkan di perangkat ini.
    </p>
    <div class="actions">
      <button onclick={() => void activity.ackDisclosure().catch(() => undefined)}>Mengerti</button>
      {#if onsettings}
        <button class="ghost" onclick={() => onsettings?.()}>Atur privasi</button>
      {/if}
    </div>
  </div>
{/if}

<style>
  .disclosure {
    margin: 6px 8px;
    padding: 8px;
    border: 1px solid var(--line);
    border-radius: var(--r);
    background: var(--hover);
    font-size: 11.5px;
    color: var(--text2);
  }
  p {
    margin: 0 0 6px;
  }
  .actions {
    display: flex;
    gap: 6px;
  }
  button {
    height: 22px;
    padding: 0 8px;
    border: 1px solid var(--line);
    border-radius: var(--r);
    background: var(--elev);
    color: var(--text);
    font-size: 11.5px;
    cursor: default;
  }
  button.ghost {
    background: transparent;
    color: var(--text2);
  }
</style>
