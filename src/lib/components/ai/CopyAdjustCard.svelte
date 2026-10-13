<script lang="ts">
  // FER-471: kartu konfirmasi `copy.adjust`. Semua teks dibuat aplikasi (bukan model): apa yang akan
  // diubah pada copy yang sedang berjalan. Tidak ada file/path; efeknya hanya kinerja. Tombol
  // "Terapkan" adalah persetujuan (pola approval executor); tanpa klik, tidak ada yang berubah.
  import type { ChatStore, RunView } from '../../ai/chatStore.svelte';
  import { describeSettings } from '../../ai/copyAdjust';

  let { run, store }: { run: RunView; store: ChatStore } = $props();

  const view = $derived(run.copyAdjust);
  const waiting = $derived(run.state === 'awaitingApproval');
  const tone = $derived(
    run.state === 'succeeded' ? 'good' : run.state === 'failed' ? 'bad' : waiting ? 'info' : 'warn',
  );
</script>

<section
  class="card {tone}"
  aria-label="Penyesuaian copy: {run.userText.slice(0, 60)}"
  data-testid="copy-adjust"
  data-state={run.state}
>
  <div class="status" role="status" aria-live="polite">
    <strong>{waiting ? 'Menunggu konfirmasi Anda' : run.label}</strong>
  </div>

  {#if view}
    <p class="summary" data-testid="copy-adjust-summary">{view.summary}</p>
    <p class="muted small" data-testid="copy-adjust-job">{view.job}</p>
    <p class="muted small" data-testid="copy-adjust-change">
      {describeSettings(view.before)} → {describeSettings(view.after)}
    </p>
    {#if view.result}
      <p
        class="result {view.result.ok ? 'good' : 'bad'}"
        role={view.result.ok ? 'status' : 'alert'}
        data-testid="copy-adjust-result"
      >
        {view.result.message}
        {#if view.result.applied}
          Sekarang: {describeSettings(view.result.applied)}.
        {/if}
      </p>
    {/if}
  {:else if run.blockedReason}
    <p class="result bad" role="alert" data-testid="copy-adjust-blocked">{run.blockedReason}</p>
  {:else if run.errorMessage}
    <p class="result bad" role="alert">{run.errorMessage}</p>
  {/if}

  {#if waiting}
    <p class="muted small">Hanya mengubah kinerja copy; tidak ada file yang disentuh.</p>
    <div class="actions">
      <button
        type="button"
        class="primary"
        data-testid="copy-adjust-apply"
        disabled={!run.canApprove}
        onclick={() => store.approve(run.runId)}
      >
        Terapkan
      </button>
      <button type="button" class="ghost" onclick={() => store.cancel(run.runId)}>Batal</button>
    </div>
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
