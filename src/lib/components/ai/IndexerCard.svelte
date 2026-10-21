<script lang="ts">
  // FER-476: kartu FerrumIndexer di chat. Dua bentuk, semua teks dibuat aplikasi (bukan model):
  //  - status: jawaban baca-saja "indeksnya sudah siap?" (tanpa tombol, tanpa path);
  //  - setMode: kartu konfirmasi perubahan mode privasi. Selalu menunggu klik "Ganti mode"; naik ke
  //    High performance juga butuh centang "PC ini hanya dipakai oleh saya" (aturan yang sama dengan
  //    dialog di Pengaturan). Windows meminta UAC sendiri saat mode dinaikkan.
  import type { ChatStore, RunView } from '../../ai/chatStore.svelte';
  import { MODE_LABEL } from '../../ai/indexerControl';

  import Mnemonic from '../Mnemonic.svelte';

  let { run, store }: { run: RunView; store: ChatStore } = $props();
  // FER-577 (B4): access letters only while this card is the live (waiting) run.
  const live = $derived(store.activeRun?.runId === run.runId);
  const mn = (label: string) => (live ? label : label.replace('&', ''));

  const view = $derived(run.indexer);
  const waiting = $derived(run.state === 'awaitingApproval' && !run.approvalSent);
  const applying = $derived(run.state === 'awaitingApproval' && run.approvalSent);
  let understood = $state(false);
  const canConfirm = $derived(
    run.canApprove && (view?.kind !== 'setMode' || !view.needsAck || understood),
  );
  const tone = $derived(
    run.state === 'succeeded'
      ? view?.kind === 'status' && view.readiness !== 'ready'
        ? 'warn'
        : 'good'
      : run.state === 'failed'
        ? 'bad'
        : run.state === 'awaitingApproval'
          ? 'info'
          : 'warn',
  );
  const READINESS_LABEL = {
    not_installed: 'Belum terpasang',
    stopped: 'Tidak berjalan',
    degraded: 'Mode terbatas (degraded)',
    building: 'Sedang disusun',
    ready: 'Siap',
    error: 'Bermasalah',
  } as const;
  const VOLUME_LABEL = { building: 'disusun', ready: 'siap', error: 'gagal' } as const;

  function confirm() {
    if (!canConfirm) return;
    store.approve(run.runId, { acknowledged: understood });
  }
</script>

<section
  class="card {tone}"
  aria-label="FerrumIndexer: {run.userText.slice(0, 60)}"
  data-testid="indexer-card"
  data-state={run.state}
>
  {#if view?.kind === 'status'}
    <div class="status" role="status" aria-live="polite">
      <strong data-testid="indexer-readiness">{READINESS_LABEL[view.readiness]}</strong>
      {#if view.paused}<span class="muted small"> · dijeda</span>{/if}
    </div>
    <p class="summary" data-testid="indexer-summary">{view.summary}</p>
    {#if view.mode}
      <p class="muted small" data-testid="indexer-mode-line">Mode: {MODE_LABEL[view.mode]}</p>
    {/if}
    {#if view.volumes.length}
      <ul class="volumes" data-testid="indexer-volumes">
        {#each view.volumes as v (v.letter)}
          <li>
            <span class="mono">{v.letter}:</span>
            {VOLUME_LABEL[v.state]}{v.state === 'building' ? ` ${v.percent}%` : ''}{v.error
              ? ` (${v.error})`
              : ''}
          </li>
        {/each}
      </ul>
    {/if}
  {:else if view?.kind === 'setMode'}
    <div class="status" role="status" aria-live="polite">
      <strong>
        {#if waiting}Menunggu konfirmasi Anda{:else if applying}Menerapkan perubahan…{:else}Mode
          FerrumIndexer{/if}
      </strong>
    </div>
    <p class="summary" data-testid="indexer-summary">{view.summary}</p>
    {#if view.current}
      <p class="muted small" data-testid="indexer-current">
        Sekarang: {MODE_LABEL[view.current]} → {MODE_LABEL[view.target]}
      </p>
    {/if}
    {#if !view.result}
      <ul class="consequences" data-testid="indexer-consequences">
        {#each view.consequences as line (line)}<li>{line}</li>{/each}
      </ul>
    {/if}
    {#if view.result}
      <p
        class="result {view.result.ok ? 'good' : 'bad'}"
        role={view.result.ok ? 'status' : 'alert'}
        data-testid="indexer-result"
      >
        {view.result.message}
      </p>
    {/if}
    {#if waiting && view.needsAck}
      <label class="ack">
        <input type="checkbox" data-testid="indexer-ack" bind:checked={understood} />
        {view.ackLabel}
      </label>
    {/if}
    {#if applying && view.target === 'high_performance'}
      <p class="muted small" data-testid="indexer-applying">
        Jika Windows meminta izin administrator (UAC), setujui di jendela itu.
      </p>
    {/if}
    {#if waiting}
      <div class="actions">
        <button
          type="button"
          class="primary"
          data-testid="indexer-apply"
          disabled={!canConfirm}
          onclick={confirm}
        >
          <Mnemonic label={mn('&Ganti mode')} />
        </button>
        <button type="button" class="ghost" onclick={() => store.cancel(run.runId)}
          ><Mnemonic label={mn('Bata&l')} /></button
        >
      </div>
    {/if}
  {:else if run.blockedReason}
    <p class="result bad" role="alert" data-testid="indexer-blocked">{run.blockedReason}</p>
  {:else if run.errorMessage}
    <p class="result bad" role="alert">{run.errorMessage}</p>
  {/if}
</section>

<style>
  .card {
    border: 1px solid var(--line2);
    border-left-width: 3px;
    border-radius: var(--rl);
    background: var(--elev);
    padding: 8px 10px;
    display: flex;
    flex-direction: column;
    gap: 6px;
    min-width: 0;
  }
  .card.good {
    border-left-color: var(--green);
  }
  .card.warn {
    border-left-color: var(--amber);
  }
  .card.bad {
    border-left-color: var(--red);
  }
  .card.info {
    border-left-color: var(--accent);
  }
  p {
    margin: 0;
    overflow-wrap: anywhere;
  }
  ul {
    margin: 0;
    padding-left: 18px;
    display: flex;
    flex-direction: column;
    gap: 3px;
    overflow-wrap: anywhere;
  }
  .volumes {
    list-style: none;
    padding-left: 0;
    font-size: 11px;
    color: var(--text2);
  }
  .mono {
    font-family: 'Geist Mono', monospace;
  }
  .muted {
    color: var(--text3);
  }
  .small {
    font-size: 11px;
  }
  .result.good {
    color: var(--green);
  }
  .result.bad {
    color: var(--red);
  }
  .ack {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .actions {
    display: flex;
    gap: 6px;
    justify-content: flex-end;
  }
  button {
    cursor: pointer;
    border-radius: var(--r);
    padding: 2px 10px;
    color: var(--text);
  }
  button:focus-visible {
    outline: 2px solid var(--accentInk);
    outline-offset: 1px;
  }
  button:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .primary {
    background: var(--accent);
    border: 1px solid var(--accent);
    color: #fff;
  }
  .ghost {
    background: transparent;
    border: 1px solid var(--line2);
  }
  .ghost:hover:not(:disabled) {
    background: var(--hover2);
  }
</style>
