<script lang="ts">
  // FER-466: kartu jawaban `diskUsage.query` (run `answered`). Jawaban dihitung lokal dari hasil Disk
  // Usage yang sedang tampil dan tidak pernah dikirim ke provider. Nama/path adalah data tak
  // tepercaya: dirender sebagai teks (tanpa {@html}). Aksi hanya Reveal (navigasi pane), Copy path
  // (clipboard) dan Buka Disk Usage; tidak ada hapus/pindah dari kartu ini.
  import type { RunView } from '../../ai/chatStore.svelte';
  import {
    CATEGORY_LABEL,
    describeDiskUsageAnswer,
    formatAnswerBytes,
    visibleText,
    type DiskUsageAnswerItem,
  } from '../../ai/diskUsageQuery';
  import { formatSize } from '../../format';

  let {
    run,
    reveal,
    copyPath,
    openDiskUsage,
  }: {
    run: RunView;
    /** Existing pane navigation (leave Disk Usage, open the parent, select the entry). */
    reveal: (path: string) => void;
    /** Existing clipboard helper; resolves false when the copy failed. */
    copyPath: (path: string) => Promise<boolean>;
    /** Enter the Disk Usage workspace (so the user can run a scan). */
    openDiskUsage: () => void;
  } = $props();

  const answer = $derived(run.diskUsage);
  const described = $derived(answer ? describeDiskUsageAnswer(answer) : undefined);
  const tone = $derived(described?.tone ?? 'info');
  let note = $state('');

  async function copy(item: DiskUsageAnswerItem) {
    note = (await copyPath(item.path)) ? 'Path disalin.' : 'Path tidak dapat disalin.';
  }
</script>

<section
  class="card {tone}"
  aria-label="Disk Usage: {run.userText.slice(0, 60)}"
  data-testid="du-answer"
  data-status={answer?.status}
>
  <div class="status" role="status" aria-live="polite">
    <span class="dot" aria-hidden="true"></span>
    <strong>Hasil Disk Usage</strong>
  </div>

  {#if run.diskUsageQuery}
    <p class="muted" data-testid="du-query">
      Permintaan (dari model), dibaca di hasil scan lokal: {run.diskUsageQuery}
    </p>
  {/if}

  {#if described}
    <p
      class="message {tone}"
      role={tone === 'warn' || tone === 'bad' ? 'alert' : undefined}
      data-testid="du-message"
    >
      {described.text}
    </p>
  {/if}

  {#if answer?.status === 'noScan'}
    <div class="actions">
      <button type="button" class="ghost" data-testid="du-open" onclick={openDiskUsage}>
        Buka Disk Usage
      </button>
    </div>
  {:else if answer?.status === 'unavailable'}
    <div class="actions">
      <button type="button" class="ghost" data-testid="du-open" onclick={openDiskUsage}>
        Buka Disk Usage
      </button>
    </div>
  {:else if answer?.status === 'summary' && answer.summary}
    <dl class="summary" data-testid="du-summary">
      <dt>Root scan</dt>
      <dd class="mono"><bdi>{visibleText(answer.scanRoot)}</bdi></dd>
      <dt>File</dt>
      <dd>{answer.summary.files.toLocaleString('en-US')}</dd>
      <dt>Folder</dt>
      <dd>{answer.summary.folders.toLocaleString('en-US')}</dd>
      <dt>Ukuran logis</dt>
      <dd>{formatSize(answer.summary.logicalBytes)}</dd>
      <dt>Ukuran di disk</dt>
      <dd>
        {answer.summary.allocatedBytes === null
          ? 'tidak tersedia'
          : formatSize(answer.summary.allocatedBytes)}
      </dd>
      {#if answer.summary.errors > 0}
        <dt>Tidak terbaca</dt>
        <dd>{answer.summary.errors.toLocaleString('en-US')} entri</dd>
      {/if}
    </dl>
  {:else if answer?.status === 'files' || answer?.status === 'folders'}
    <p class="muted small" data-testid="du-scope">
      Folder: <bdi>{visibleText(answer.scopeLabel)}</bdi> · ukuran {answer.metric === 'allocated'
        ? 'di disk'
        : 'logis'}{answer.category ? ` · hanya ${CATEGORY_LABEL[answer.category]}` : ''}
    </p>
    {#if answer.items.length}
      <ol class="rows" aria-label="Hasil Disk Usage" data-testid="du-items">
        {#each answer.items as item (item.path)}
          <li data-testid="du-item">
            <span class="name mono"><bdi>{visibleText(item.name)}</bdi></span>
            <span class="size" data-testid="du-size">{formatAnswerBytes(item.bytes)}</span>
            {#if item.percent !== null}
              <span class="muted small">{item.percent.toFixed(1)}%</span>
            {/if}
            <span class="hint mono" title={visibleText(item.rel)}
              ><bdi>{visibleText(item.rel)}</bdi></span
            >
            <span class="rowactions">
              <button
                type="button"
                class="ghost"
                data-testid="du-reveal"
                aria-label="Reveal {visibleText(item.name)} di panel"
                onclick={() => reveal(item.path)}
              >
                Reveal
              </button>
              <button
                type="button"
                class="ghost"
                data-testid="du-copy"
                aria-label="Salin path {visibleText(item.name)}"
                onclick={() => void copy(item)}
              >
                Copy path
              </button>
            </span>
          </li>
        {/each}
      </ol>
    {/if}
  {/if}

  {#if answer && 'notes' in answer && answer.notes.length}
    <ul class="notes" data-testid="du-notes">
      {#each answer.notes as n (n)}<li class="muted small">{n}</li>{/each}
    </ul>
  {/if}
  {#if note}
    <p class="muted small" role="status" data-testid="du-note">{note}</p>
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
  .status {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--accentInk);
  }
  .good .dot {
    background: var(--green);
  }
  .warn .dot {
    background: var(--amber);
  }
  .bad .dot {
    background: var(--red);
  }
  p {
    margin: 0;
  }
  .message.warn {
    color: var(--amber);
  }
  .message.bad {
    color: var(--red);
  }
  .muted {
    color: var(--text3);
  }
  .small {
    font-size: 11px;
  }
  .rows {
    list-style: none;
    margin: 0;
    padding: 0;
    max-height: 300px;
    overflow: auto;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .rows li {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 2px 8px;
    overflow-wrap: anywhere;
    padding: 2px 0;
    border-bottom: 1px solid var(--line);
  }
  .name {
    font-weight: 600;
  }
  .size {
    font-variant-numeric: tabular-nums;
  }
  .hint {
    color: var(--text3);
    min-width: 0;
    flex-basis: 100%;
  }
  .rowactions {
    margin-left: auto;
    display: inline-flex;
    gap: 4px;
  }
  .summary {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: 2px 12px;
    margin: 0;
  }
  .summary dt {
    color: var(--text3);
  }
  .summary dd {
    margin: 0;
    overflow-wrap: anywhere;
  }
  .notes {
    margin: 0;
    padding-left: 16px;
  }
  .actions {
    display: flex;
  }
  button {
    cursor: pointer;
    border-radius: var(--r);
    padding: 2px 8px;
    color: var(--text);
  }
  button:focus-visible {
    outline: 2px solid var(--accentInk);
    outline-offset: 1px;
  }
  .ghost {
    background: transparent;
    border: 1px solid var(--line2);
  }
  .ghost:hover:not(:disabled) {
    background: var(--hover2);
  }
</style>
