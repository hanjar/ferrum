<script lang="ts">
  /**
   * The automation list (MH-2, MH-3, MH-4): attention banner, queue strip and one row per
   * automation. Everything shown (status, last result, runtime note, button flags) is computed by
   * the backend; this component only displays it. "Run now" only asks the backend to queue a run
   * (the answer is a toast); it is dimmed, with the reason as its tooltip, when the backend says
   * the automation cannot run now.
   */
  import type { AutomationStore } from '../../stores/automationStore.svelte';
  import type { AutomationRow } from '../../ipc/automation';
  import StatusChip from './StatusChip.svelte';

  let { store }: { store: AutomationStore } = $props();

  const attentionRows = $derived(
    store.rows.filter((r) => r.showReview || r.attention !== undefined),
  );

  function when(ms: number): string {
    return new Date(ms).toLocaleString(undefined, {
      month: 'short',
      day: 'numeric',
      hour: '2-digit',
      minute: '2-digit',
    });
  }

  const warnNote = (r: AutomationRow) => r.runtime === 'app' || /not running/i.test(r.runtimeNote);
</script>

{#if attentionRows.length > 0}
  <div class="attn" role="status" data-testid="attention-banner">
    <span>
      {#if attentionRows.length === 1}
        1 automation needs you &mdash; {attentionRows[0].name} &mdash; new runs are held until reviewed.
      {:else}
        {attentionRows.length} automations need you &mdash; new runs are held until they are reviewed
        or resolved.
      {/if}
    </span>
    <button type="button" class="link" onclick={() => store.showHistory('attention')}>
      Show in run history
    </button>
  </div>
{/if}

{#if store.queue.length > 0}
  <div class="queue" role="group" aria-label="Queue" data-testid="queue-strip">
    <span class="qtitle">Queue</span>
    {#each store.queue as r (r.id)}
      <span class="qitem">
        <span class="qname">{r.name}</span>
        <StatusChip status={r.status} />
      </span>
    {/each}
  </div>
{/if}

{#if store.loaded && store.rows.length === 0 && !store.listError}
  <div class="empty" data-testid="automation-empty">
    <h3>No automations yet</h3>
    <p>
      An automation runs a saved sync profile for you at a scheduled time. Create a sync profile
      first, then add an automation for it.
    </p>
  </div>
{/if}

<ul class="rows">
  {#each store.rows as r (r.id)}
    <li class="row" class:hl={store.highlightId === r.id} data-automation={r.id}>
      <label class="switch" title={r.enabled ? 'Enabled' : 'Disabled'}>
        <input
          type="checkbox"
          role="switch"
          checked={r.enabled}
          aria-label="Enabled: {r.name}"
          onchange={() => void store.toggleEnabled(r)}
        />
        <span class="track"></span>
      </label>

      <div class="main">
        <div class="name">
          <button
            type="button"
            class="namelink"
            title="Open details"
            onclick={() => void store.openDetail(r.id)}>{r.name}</button
          >
        </div>
        <div class="meta">
          {r.profileName}{r.approvedRevision !== undefined ? ` · rev ${r.approvedRevision}` : ''}
        </div>
        <div class="meta">{r.trigger}</div>
        <div class="note" class:amber={warnNote(r)}>{r.runtimeNote}</div>
      </div>

      <div class="status">
        <StatusChip status={r.status} showSub />
      </div>

      <div class="last">
        {#if r.lastRun}
          <StatusChip status={r.lastRun.result} showSub />
          <span class="when">{when(r.lastRun.endedMs)}</span>
        {:else}
          <span class="when">No runs yet</span>
        {/if}
      </div>

      <div class="actions">
        <button
          type="button"
          disabled={!r.canRunNow || !!store.pending[`run:${r.id}`]}
          title={r.canRunNow ? undefined : r.runNowReason}
          aria-label="Run now: {r.name}"
          onclick={() => void store.runNow(r)}
        >
          Run now
        </button>
        <button type="button" onclick={() => void store.openEdit(r.id)}>Edit</button>
        {#if r.showReview}
          <button type="button" class="primary" onclick={() => void store.openReview(r)}>
            Review changes
          </button>
        {:else if r.attention}
          <button type="button" class="primary" onclick={() => void store.openDetail(r.id)}>
            Resolve
          </button>
        {/if}
      </div>
    </li>
  {/each}
</ul>

<style>
  .attn {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 7px 10px;
    border: 1px solid var(--red);
    border-radius: var(--r);
    background: var(--redSoft);
    color: var(--text);
    font-size: 12px;
  }
  .link {
    margin-left: auto;
    flex: none;
    height: 22px;
    padding: 0 8px;
    border: 0;
    border-radius: var(--r);
    background: transparent;
    color: var(--accentInk);
    font-size: 12px;
    text-decoration: underline;
    cursor: default;
  }
  .queue {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
    padding: 6px 10px;
    border-radius: var(--r);
    background: var(--hover);
    font-size: 12px;
  }
  .qtitle {
    font-weight: 600;
    color: var(--text2);
  }
  .qitem {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }
  .empty {
    padding: 18px;
    border: 1px dashed var(--line);
    border-radius: var(--r);
    color: var(--text2);
  }
  .empty h3 {
    margin: 0 0 4px;
    font-size: 13px;
  }
  .empty p {
    margin: 0;
    font-size: 12px;
  }
  .rows {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .row {
    display: grid;
    grid-template-columns: 40px minmax(0, 2.2fr) minmax(0, 1.3fr) minmax(0, 1.3fr) auto;
    align-items: center;
    gap: 10px;
    min-height: 42px;
    padding: 8px 10px;
    border: 1px solid var(--line);
    border-radius: var(--r);
  }
  .row.hl {
    background: var(--flash);
    border-color: var(--green);
  }
  .name {
    font-size: 13px;
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .namelink {
    height: auto;
    max-width: 100%;
    padding: 0;
    background: transparent;
    color: var(--text);
    font: inherit;
    overflow: hidden;
    text-overflow: ellipsis;
    text-align: left;
  }
  .namelink:hover:not(:disabled) {
    text-decoration: underline;
  }
  .meta,
  .note,
  .when {
    font-size: 11.5px;
    color: var(--text3);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .note.amber {
    color: var(--amberInk);
  }
  .last {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }
  .actions {
    display: flex;
    gap: 6px;
    justify-content: flex-end;
  }
  button {
    height: 26px;
    padding: 0 10px;
    border: 0;
    border-radius: var(--r);
    background: var(--hover2);
    color: var(--text2);
    font-size: 12px;
    cursor: default;
    white-space: nowrap;
  }
  button:hover:not(:disabled) {
    color: var(--text);
  }
  button:disabled {
    opacity: 0.45;
  }
  button.primary {
    background: var(--accent);
    color: #fff;
  }
  .switch {
    position: relative;
    display: inline-block;
    width: 34px;
    height: 20px;
  }
  .switch input {
    position: absolute;
    inset: 0;
    opacity: 0;
    margin: 0;
    cursor: default;
  }
  .track {
    position: absolute;
    inset: 0;
    border-radius: 10px;
    background: var(--hover2);
    pointer-events: none;
    transition: background 120ms;
  }
  .track::after {
    content: '';
    position: absolute;
    top: 2px;
    left: 2px;
    width: 16px;
    height: 16px;
    border-radius: 50%;
    background: var(--text3);
    transition:
      transform 120ms,
      background 120ms;
  }
  .switch input:checked + .track {
    background: var(--accent);
  }
  .switch input:checked + .track::after {
    transform: translateX(14px);
    background: #fff;
  }
  .switch input:focus-visible + .track {
    outline: 2px solid var(--accentInk);
    outline-offset: 2px;
  }
</style>
