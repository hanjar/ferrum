<script lang="ts">
  // ACT-08 R6: kartu jawaban riwayat aktivitas (run `answered`). Jawaban dibaca lokal dan tidak pernah
  // dikirim ke provider. Nama/hint adalah data tak tepercaya: dirender sebagai teks (tanpa {@html}).
  // "Buka di panel" (R7 v1) hanya menempatkan item di panel aktif lewat API pane; tidak ada mutasi.
  // "Buat ZIP" (R7 v2, FER-433) hanya MENYUSUN rencana arsip biasa (preview + persetujuan); belum menulis apa pun.
  import { tick } from 'svelte';
  import type { ChatStore, RunView } from '../../ai/chatStore.svelte';
  import {
    AVAILABILITY_LABEL,
    REASON_LABEL,
    answerItems,
    describeAnswer,
    isOpenable,
    type ActivityAnswerItem,
    type ActivityLocation,
  } from '../../ai/activityQuery';

  let {
    run,
    store,
    place,
  }: {
    run: RunView;
    store: ChatStore;
    /** Pane API hand-off (default in ChatPanel: reveal/open in the active pane). */
    place: (location: ActivityLocation) => boolean;
  } = $props();

  let root = $state<HTMLElement | undefined>();

  const answer = $derived(run.activity);
  const described = $derived(answer ? describeAnswer(answer) : undefined);
  const tone = $derived(described?.tone ?? 'info');
  const choosing = $derived(answer?.status === 'ambiguous' && !run.chosenId);
  const shown = $derived.by((): ActivityAnswerItem[] => {
    if (!answer) return [];
    if (answer.status === 'ambiguous')
      return answer.candidates.filter((c) => c.id === run.chosenId);
    return answerItems(answer);
  });
  const kindLabel = (kind: 'file' | 'folder') => (kind === 'folder' ? 'Folder' : 'File');

  async function choose(id: string) {
    store.chooseCandidate(run.runId, id);
    await tick();
    root?.querySelector<HTMLButtonElement>('[data-testid="open-item"]')?.focus();
  }
  function open(item: ActivityAnswerItem) {
    void store.openActivityItem(run.runId, item.id, place);
  }
  // FER-433 (R7 v2): rencana ZIP biasa untuk item ini; tujuan = folder panel aktif, tetap perlu persetujuan.
  function zip(item: ActivityAnswerItem) {
    if (!store.zipActivityItem(run.runId, item.id))
      run.openNote =
        'ZIP tidak dapat disusun sekarang. Selesaikan atau batalkan rencana yang aktif.';
  }
</script>

<section
  class="card {tone}"
  aria-label="Riwayat aktivitas: {run.userText.slice(0, 60)}"
  data-testid="activity-answer"
  data-status={answer?.status}
  bind:this={root}
>
  <div class="status" role="status" aria-live="polite">
    <span class="dot" aria-hidden="true"></span>
    <strong>{run.label}</strong>
  </div>

  {#if run.activityQuery}
    <p class="muted" data-testid="answer-query">
      Permintaan (dari model), dicari di riwayat lokal: {run.activityQuery}
    </p>
  {/if}

  {#if described}
    <p
      class="message {tone}"
      role={tone === 'warn' || tone === 'bad' ? 'alert' : undefined}
      data-testid="answer-message"
    >
      {described.text}
    </p>
  {/if}

  {#if choosing && answer?.status === 'ambiguous'}
    <ul class="rows" aria-label="Pilih item yang dimaksud" data-testid="candidates">
      {#each answer.candidates as c (c.id)}
        <li>
          <span class="name mono">{c.displayName}</span>
          <span class="hint mono" title={c.parentHint}>{c.parentHint}</span>
          <span class="muted">{kindLabel(c.kind)} · {AVAILABILITY_LABEL[c.availability]}</span>
          <button
            type="button"
            class="ghost"
            data-testid="choose-candidate"
            aria-label="Pilih {c.displayName} di {c.parentHint}"
            onclick={() => void choose(c.id)}
          >
            Pilih
          </button>
        </li>
      {/each}
    </ul>
  {:else if shown.length}
    <ul class="rows" aria-label="Hasil riwayat aktivitas" data-testid="answer-items">
      {#each shown as item (item.id)}
        <li data-testid="answer-item">
          <span class="name mono">{item.displayName}</span>
          {#if item.parentHint}<span class="hint mono" title={item.parentHint}
              >{item.parentHint}</span
            >{/if}
          <span class="muted">
            {kindLabel(item.kind)} ·
            <span class="avail {item.availability}">{AVAILABILITY_LABEL[item.availability]}</span>
          </span>
          {#if item.reasons.length}
            <span class="reasons" aria-label="Alasan">
              {#each item.reasons as reason (reason)}<span class="reason"
                  >{REASON_LABEL[reason]}</span
                >{/each}
            </span>
          {/if}
          {#if isOpenable(item) && store.canOpenActivityItems}
            <button
              type="button"
              class="ghost"
              data-testid="open-item"
              disabled={run.opening}
              aria-busy={run.opening}
              aria-label="Buka {item.displayName} di panel aktif"
              onclick={() => open(item)}
            >
              Buka di panel
            </button>
          {:else if !isOpenable(item)}
            <span class="muted" data-testid="not-openable">Tidak dapat dibuka</span>
          {/if}
          {#if isOpenable(item) && store.canZipActivityItems}
            <button
              type="button"
              class="ghost zip"
              data-testid="zip-item"
              disabled={store.busy}
              title="Menyusun rencana ZIP di folder panel aktif; belum menulis apa pun sebelum Anda setujui."
              aria-label="Buat ZIP dari {item.displayName} di folder panel aktif"
              onclick={() => zip(item)}
            >
              Buat ZIP
            </button>
          {/if}
        </li>
      {/each}
    </ul>
  {/if}

  {#if run.openNote}
    <p class="muted" role="status" data-testid="open-note">{run.openNote}</p>
  {/if}
  <p class="muted small" data-testid="answer-privacy">
    Dibaca dari riwayat lokal di perangkat ini; hasilnya tidak dikirim ke provider AI (pertanyaan
    Anda tetap dikirim).
  </p>
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
    max-height: 260px;
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
  .hint {
    color: var(--text3);
    min-width: 0;
  }
  .avail.inaccessible,
  .avail.offline,
  .avail.missing {
    color: var(--amber);
  }
  .reasons {
    display: inline-flex;
    gap: 4px;
    flex-wrap: wrap;
  }
  .reason {
    font-size: 11px;
    border: 1px solid var(--line2);
    border-radius: 999px;
    padding: 0 6px;
    color: var(--text3);
  }
  button {
    cursor: pointer;
    border-radius: var(--r);
    padding: 2px 8px;
    color: var(--text);
    margin-left: auto;
  }
  button:focus-visible {
    outline: 2px solid var(--accentInk);
    outline-offset: 1px;
  }
  button:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .ghost {
    background: transparent;
    border: 1px solid var(--line2);
  }
  .ghost.zip {
    margin-left: 0;
  }
  .ghost:hover:not(:disabled) {
    background: var(--hover2);
  }
</style>
