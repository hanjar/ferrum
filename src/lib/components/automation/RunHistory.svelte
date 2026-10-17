<script lang="ts">
  /**
   * Run table (MH-6): Run, Automation, Trigger, Started → ended, Result, Succeeded, Failed, Not
   * started. A row expands to show the operations, the error text, the note and the actions
   * Recheck / Retry remaining N / Review in Sync ↗ / Open automation.
   *
   * Every button only asks the backend; what is offered comes from the run's own facts
   * (`needsAttention`, `recheck`, `outcome`) - Retry exists only after a Recheck that found
   * something remaining, and never for a conflict (RQ-10.2).
   */
  import type { AutomationStore } from '../../stores/automationStore.svelte';
  import type { AutomationRun } from '../../ipc/automation';
  import StatusChip from './StatusChip.svelte';
  import RunOps from './RunOps.svelte';
  import {
    canRecheck,
    canReviewInSync,
    retryRemaining,
    triggerWithMerge,
    whenRange,
  } from './automationModel';

  let {
    store,
    runs,
    compact = false,
  }: {
    store: AutomationStore;
    runs: AutomationRun[];
    /** The detail page's "recent runs": no Automation column and no Open automation button. */
    compact?: boolean;
  } = $props();

  const cols = $derived(compact ? 7 : 8);
</script>

<table class="history" data-testid={compact ? 'recent-runs' : 'run-history'}>
  <thead>
    <tr>
      <th>Run</th>
      {#if !compact}<th>Automation</th>{/if}
      <th>Trigger</th><th>Started → ended</th><th>Result</th>
      <th>Succeeded</th><th>Failed</th><th>Not started</th>
    </tr>
  </thead>
  <tbody>
    {#each runs as run (run.id)}
      {@const open = !!store.expanded[run.id]}
      {@const retryN = retryRemaining(run)}
      <tr data-run={run.id} class:open>
        <td>
          <button
            type="button"
            class="toggle"
            aria-expanded={open}
            aria-controls="run-detail-{run.id}"
            aria-label="Run #{run.id}, {open ? 'collapse' : 'expand'}"
            onclick={() => store.toggleRun(run)}
          >
            <span class="caret" aria-hidden="true">{open ? '▾' : '▸'}</span>#{run.id}
          </button>
        </td>
        {#if !compact}<td>{run.automationName ?? 'Manual run'}</td>{/if}
        <td>{triggerWithMerge(run)}</td>
        <td>{whenRange(run.startedMs, run.endedMs)}</td>
        <td><StatusChip status={run.result} /></td>
        <td>{run.counts.ok}</td>
        <td>{run.counts.failed}</td>
        <td>{run.counts.notStarted}</td>
      </tr>
      {#if open}
        <tr class="detail" id="run-detail-{run.id}" data-run-detail={run.id}>
          <td colspan={cols}>
            <div class="panel">
              {#if run.errorText}
                <p class="err" data-testid="run-error">Error: {run.errorText}</p>
              {/if}
              {#if run.note}
                <p class="note" data-testid="run-note">{run.note}</p>
              {/if}
              {#if run.resolvedText}
                <p class="ok" data-testid="run-resolved">{run.resolvedText}</p>
              {:else if run.resolvedBy !== undefined}
                <p class="ok" data-testid="run-resolved">Resolved by run #{run.resolvedBy}</p>
              {/if}
              {#if run.recheck}
                <p class="recheck" data-testid="run-recheck" data-state={run.recheck.state}>
                  {run.recheck.text}
                </p>
              {/if}

              <div class="actions">
                {#if canRecheck(run)}
                  <button
                    type="button"
                    disabled={!!store.pending[`recheck:${run.id}`]}
                    onclick={() => void store.recheck(run)}
                  >
                    Recheck
                  </button>
                {/if}
                {#if retryN > 0}
                  <button
                    type="button"
                    class="primary"
                    disabled={!!store.pending[`retry:${run.id}`]}
                    onclick={() => void store.retry(run)}
                  >
                    Retry remaining {retryN}
                  </button>
                {/if}
                {#if canReviewInSync(run)}
                  <button
                    type="button"
                    disabled={!!store.pending[`sync:${run.id}`]}
                    onclick={() => void store.reviewInSync(run)}
                  >
                    Review in Sync ↗
                  </button>
                {/if}
                {#if !compact && run.automationId}
                  <button type="button" onclick={() => void store.openDetail(run.automationId!)}>
                    Open automation
                  </button>
                {/if}
              </div>

              {#if run.opsTotal > 0}
                <RunOps {store} runId={run.id} />
              {:else}
                <p class="muted">No operations in this run.</p>
              {/if}
            </div>
          </td>
        </tr>
      {/if}
    {/each}
  </tbody>
</table>

<style>
  .history {
    width: 100%;
    border-collapse: collapse;
    font-size: 12px;
  }
  .history th {
    text-align: left;
    font-weight: 600;
    color: var(--text2);
    padding: 4px 6px;
  }
  .history td {
    padding: 6px;
    border-top: 1px solid var(--line);
  }
  tr.open > td {
    background: var(--hover);
  }
  .toggle {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    height: 22px;
    padding: 0 4px;
    border: 0;
    border-radius: var(--r);
    background: transparent;
    color: var(--text);
    font-size: 12px;
    cursor: default;
  }
  .toggle:hover {
    background: var(--hover2);
  }
  .caret {
    width: 10px;
    color: var(--text3);
  }
  .panel {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 4px 2px 6px;
  }
  .panel p {
    margin: 0;
    font-size: 12px;
  }
  .err {
    color: var(--redInk);
  }
  .note,
  .recheck {
    color: var(--text2);
  }
  .ok {
    color: var(--greenInk);
  }
  .muted {
    color: var(--text3);
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .actions button {
    height: 26px;
    padding: 0 10px;
    border: 0;
    border-radius: var(--r);
    background: var(--hover2);
    color: var(--text2);
    font-size: 12px;
    cursor: default;
  }
  .actions button:hover:not(:disabled) {
    color: var(--text);
  }
  .actions button:disabled {
    opacity: 0.45;
  }
  .actions button.primary {
    background: var(--accent);
    color: #fff;
  }
  .toggle:focus-visible,
  .actions button:focus-visible {
    outline: 2px solid var(--accentInk);
    outline-offset: 2px;
  }
</style>
