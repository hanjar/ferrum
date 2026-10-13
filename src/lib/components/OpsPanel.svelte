<script lang="ts">
  import Icon from './Icon.svelte';
  import CopyControls from './CopyControls.svelte';
  import { ops, pctOf } from '../stores/opsStore.svelte';
  import {
    detailLine,
    durationMsOf,
    failureCount,
    formatDuration,
    isRetryable,
    splitBtm,
  } from '../btm';
  import type { OperationId, OperationProgress } from '../ipc/types';
  import { SvelteSet } from 'svelte/reactivity';

  const stateLabel: Record<string, string> = {
    queued: 'Queued',
    running: 'Running',
    paused: 'Paused',
    completed: 'Completed',
    failed: 'Failed',
    cancelled: 'Cancelled',
  };

  // No pause/resume control: engines expose no safe checkpoints (D14) —
  // restart semantics are "retry from scratch", so a pause button would lie.
  const view = $derived(splitBtm(ops.all));
  const hasFinished = $derived(
    ops.all.some((o) => o.state === 'completed' || o.state === 'failed' || o.state === 'cancelled'),
  );
  const expanded = new SvelteSet<OperationId>();

  function toggleDetail(id: OperationId) {
    if (expanded.has(id)) expanded.delete(id);
    else expanded.add(id);
  }

  function retry(op: OperationProgress) {
    void ops.retry(op.operationId);
  }
</script>

{#if ops.panelOpen}
  <div class="scrim" onmousedown={() => (ops.panelOpen = false)}></div>
  <aside class="panel" role="dialog" aria-label="Background Transfer Manager">
    <header>
      <Icon name="queue" size={15} />
      <span class="title">Background Transfer Manager</span>
      {#if hasFinished}
        <button class="ghost" onclick={() => void ops.clearFinished()}>Clear history</button>
      {/if}
      <button class="icon" title="Collapse" onclick={() => (ops.panelOpen = false)}>
        <Icon name="close" size={11} />
      </button>
    </header>

    {#if ops.all.length === 0}
      <div class="empty">
        <p>No transfers.</p>
        <button class="ghost" onclick={() => void ops.enqueueDummy(1500)}>
          Run 1.5s test job
        </button>
      </div>
    {:else}
      <div class="list">
        {#each view.active as op (op.operationId)}
          <div class="job">
            <div class="row1">
              <span class="kind">{op.kind}</span>
              <span class="name mono">{detailLine(op)}</span>
              <span class="pct mono">{Math.round(pctOf(op))}%</span>
            </div>
            <div class="bar"><div class="fill" style:width="{pctOf(op)}%"></div></div>
            {#if op.state === 'running' && ops.copyControlOf(op.operationId)}
              <CopyControls control={ops.copyControlOf(op.operationId)!} />
            {/if}
            <div class="row2">
              <span>{stateLabel[op.state]}</span>
              <button class="ghost danger" onclick={() => void ops.cancel(op.operationId)}>
                Cancel
              </button>
            </div>
          </div>
        {/each}
        {#if view.active.length > 0 && view.history.length > 0}
          <div class="sectionLabel">History</div>
        {/if}
        {#each view.history as op (op.operationId)}
          <div class="job done">
            <div class="row1">
              <span class="kind">{op.kind}</span>
              <span class="name mono">{detailLine(op)}</span>
              <span class="pct mono"
                >{op.state === 'failed'
                  ? 'failed'
                  : op.state === 'cancelled'
                    ? 'stopped'
                    : '100%'}</span
              >
            </div>
            <div class="bar"><div class="fill" style:width="100%"></div></div>
            <div class="row2">
              <span>
                {stateLabel[op.state]}{#if durationMsOf(op) !== null}
                  · {formatDuration(durationMsOf(op)!)}
                {/if}
              </span>
              <span class="rowActions">
                {#if failureCount(op) > 0}
                  <button class="ghost" onclick={() => toggleDetail(op.operationId)}>
                    {expanded.has(op.operationId) ? 'Hide' : 'Details'} ({failureCount(op)})
                  </button>
                {/if}
                {#if isRetryable(op)}
                  <button class="ghost" onclick={() => retry(op)}>Retry</button>
                {/if}
              </span>
            </div>
            {#if op.error}
              <div class="errorText" title={op.error}>{op.error}</div>
            {/if}
            {#if expanded.has(op.operationId) && failureCount(op) > 0}
              <ul class="failures">
                {#each op.failures ?? [] as f (f.path + f.message)}
                  <li>
                    <span class="mono path">{f.path}</span>
                    <span class="why">{f.message}</span>
                  </li>
                {/each}
              </ul>
            {/if}
          </div>
        {/each}
      </div>
    {/if}
  </aside>
{/if}

<style>
  .scrim {
    position: fixed;
    inset: 0;
    z-index: 30;
  }
  .panel {
    position: fixed;
    right: 8px;
    bottom: 40px;
    width: 372px;
    max-height: 60vh;
    display: flex;
    flex-direction: column;
    border-radius: var(--rl);
    background: var(--elev);
    box-shadow: var(--shadow);
    overflow: hidden;
    animation: fmUp 180ms cubic-bezier(0.2, 0.7, 0.2, 1);
    z-index: 31;
  }
  header {
    height: 38px;
    flex: none;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 0 6px 0 12px;
    color: var(--accentInk);
  }
  .title {
    flex: 1;
    font-size: calc(12.5px + var(--uiStep, 0px));
    font-weight: 600;
    color: var(--text);
  }
  .ghost {
    border: 0;
    background: transparent;
    color: var(--text2);
    font-size: calc(11.5px + var(--uiStep, 0px));
    padding: 3px 7px;
    border-radius: var(--r);
    cursor: default;
  }
  .ghost:hover {
    background: var(--hover2);
    color: var(--text);
  }
  .ghost.danger:hover {
    color: var(--red);
  }
  .icon {
    width: 24px;
    height: 24px;
    display: grid;
    place-items: center;
    border: 0;
    border-radius: var(--r);
    background: transparent;
    color: var(--text3);
    cursor: default;
  }
  .icon:hover {
    background: var(--hover2);
    color: var(--text);
  }
  .empty {
    padding: 20px;
    text-align: center;
    font-size: calc(12.5px + var(--uiStep, 0px));
    color: var(--text3);
    display: flex;
    flex-direction: column;
    gap: 10px;
    align-items: center;
  }
  .list {
    overflow: auto;
  }
  .sectionLabel {
    padding: 6px 12px 2px;
    font-size: calc(10.5px + var(--uiStep, 0px));
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--text3);
  }
  .job {
    display: flex;
    flex-direction: column;
    gap: 7px;
    padding: 10px 12px;
    border-top: 1px solid var(--line);
  }
  .job.done {
    opacity: 0.75;
  }
  .row1 {
    display: flex;
    align-items: center;
    gap: 10px;
    min-width: 0;
  }
  .kind {
    font-size: calc(11px + var(--uiStep, 0px));
    font-weight: 600;
    text-transform: capitalize;
    color: var(--accentInk);
    flex: none;
  }
  .name {
    flex: 1;
    min-width: 0;
    font-size: calc(11px + var(--uiStep, 0px));
    color: var(--text3);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .pct {
    font-size: calc(12px + var(--uiStep, 0px));
    color: var(--text2);
    flex: none;
  }
  .bar {
    height: 4px;
    border-radius: 2px;
    background: var(--line2);
    overflow: hidden;
  }
  .fill {
    height: 100%;
    background: var(--accent);
    border-radius: 2px;
    transition: width 200ms linear;
  }
  .job.done .fill {
    background: var(--bar);
  }
  .row2 {
    display: flex;
    align-items: center;
    justify-content: space-between;
    font-size: calc(11px + var(--uiStep, 0px));
    color: var(--text3);
  }
  .rowActions {
    display: flex;
    align-items: center;
    gap: 2px;
  }
  .errorText {
    font-size: calc(11px + var(--uiStep, 0px));
    color: var(--red);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .failures {
    margin: 0;
    padding: 4px 0 0;
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .failures li {
    display: flex;
    flex-direction: column;
    font-size: calc(11px + var(--uiStep, 0px));
    gap: 1px;
  }
  .failures .path {
    color: var(--text2);
    word-break: break-all;
  }
  .failures .why {
    color: var(--text3);
  }
  @keyframes fmUp {
    from {
      opacity: 0;
      transform: translateY(8px);
    }
    to {
      opacity: 1;
      transform: none;
    }
  }
</style>
