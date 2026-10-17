<script lang="ts">
  /**
   * The per-operation list of one run (RQ-5): Done / In progress / Failed / Not started / Held /
   * Pending, in pages of 200. Paths and error texts come from the backend and are drawn as text
   * only (never as HTML). Only the first 5 000 operations of a run are stored (MH-8); the rest is
   * counted, and the note says so.
   */
  import type { AutomationStore } from '../../stores/automationStore.svelte';
  import { OP_KIND_LABEL, OP_STATUS_LABEL, OPS_PAGE, opTone } from './automationModel';

  let { store, runId }: { store: AutomationStore; runId: number } = $props();

  const st = $derived(store.opsByRun[runId]);
  const page = $derived(st?.page ?? null);
  const offset = $derived(st?.offset ?? 0);
  const from = $derived(page && page.stored > 0 ? offset + 1 : 0);
  const to = $derived(page ? offset + page.ops.length : 0);
</script>

<div class="ops" data-testid="run-ops-{runId}">
  {#if st?.error}
    <p class="err" role="alert">{st.error}</p>
  {/if}
  {#if !page && st?.loading}
    <p class="muted">Loading operations…</p>
  {:else if page && page.ops.length === 0}
    <p class="muted">No operations.</p>
  {:else if page}
    <table>
      <thead>
        <tr><th>Operation</th><th>Path</th><th>Status</th></tr>
      </thead>
      <tbody>
        {#each page.ops as op (op.seq)}
          <tr data-seq={op.seq}>
            <td>{OP_KIND_LABEL[op.kind]} <span class="side">({op.side})</span></td>
            <td class="path" title={op.relPath}>{op.relPath}</td>
            <td>
              <span class="st tone-{opTone(op.status)}" data-status={op.status}>
                {OP_STATUS_LABEL[op.status]}
              </span>
              {#if op.errorText}<span class="why">{op.errorText}</span>{/if}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
    {#if page.stored > OPS_PAGE}
      <div class="pager" role="group" aria-label="Operation pages">
        <button
          type="button"
          disabled={offset <= 0 || st?.loading}
          onclick={() => void store.loadOps(runId, Math.max(0, offset - OPS_PAGE))}
        >
          Previous {OPS_PAGE}
        </button>
        <span class="muted" data-testid="ops-range">Showing {from}–{to} of {page.stored}</span>
        <button
          type="button"
          disabled={offset + OPS_PAGE >= page.stored || st?.loading}
          onclick={() => void store.loadOps(runId, offset + OPS_PAGE)}
        >
          Next {OPS_PAGE}
        </button>
      </div>
    {/if}
    {#if page.total > page.stored}
      <p class="muted">
        Showing the first {page.stored} of {page.total} operations; the rest are counted only.
      </p>
    {/if}
  {/if}
</div>

<style>
  .ops {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  table {
    width: 100%;
    border-collapse: collapse;
    font-size: 11.5px;
  }
  th {
    text-align: left;
    font-weight: 600;
    color: var(--text2);
    padding: 2px 6px;
  }
  td {
    padding: 3px 6px;
    border-top: 1px solid var(--line);
    vertical-align: top;
  }
  .path {
    max-width: 320px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .side {
    color: var(--text3);
  }
  .st {
    display: inline-block;
    padding: 0 6px;
    border-radius: 8px;
    background: var(--hover2);
    color: var(--text2);
    font-weight: 600;
  }
  .tone-ok {
    background: var(--greenSoft);
    color: var(--greenInk);
  }
  .tone-info {
    background: var(--accentSoft);
    color: var(--accentInk);
  }
  .tone-warn {
    background: var(--amberSoft);
    color: var(--amberInk);
  }
  .tone-bad {
    background: var(--redSoft);
    color: var(--redInk);
  }
  .why {
    margin-left: 6px;
    color: var(--text3);
  }
  .pager {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .pager button {
    height: 24px;
    padding: 0 10px;
    border: 0;
    border-radius: var(--r);
    background: var(--hover2);
    color: var(--text2);
    font-size: 12px;
    cursor: default;
  }
  .pager button:disabled {
    opacity: 0.45;
  }
  .muted {
    margin: 0;
    font-size: 11.5px;
    color: var(--text3);
  }
  .err {
    margin: 0;
    font-size: 12px;
    color: var(--redInk);
  }
</style>
