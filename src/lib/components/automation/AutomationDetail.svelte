<script lang="ts">
  /**
   * The detail page of one automation (MH-5): status with its explanation, the lifecycle
   * pipeline, the quiet countdown, the current run with its progress (RQ-5), waiting entries,
   * trigger and folders, runtime, recent runs, events and the controls. Everything shown is
   * computed by the backend; the buttons only ask it (Run now, Pause/Resume, Stop, Edit).
   */
  import { onMount } from 'svelte';
  import type { AutomationStore, DetailState } from '../../stores/automationStore.svelte';
  import StatusChip from './StatusChip.svelte';
  import RunHistory from './RunHistory.svelte';
  import RunOps from './RunOps.svelte';
  import {
    DETAIL_EVENTS,
    DETAIL_RECENT_RUNS,
    EVENT_GLYPH,
    formatWhen,
    isStopping,
    phaseText,
    progressCounts,
    progressFraction,
  } from './automationModel';

  let { store, detail }: { store: AutomationStore; detail: DetailState } = $props();

  const data = $derived(detail.data);
  const row = $derived(data?.automation);
  const run = $derived(data?.currentRun);
  const stopping = $derived(isStopping(row?.status.sub, run?.result.sub));
  const recent = $derived((data?.recentRuns ?? []).slice(0, DETAIL_RECENT_RUNS));
  const events = $derived([
    ...(data?.events.events ?? []).slice(0, DETAIL_EVENTS),
    ...detail.moreEvents,
  ]);
  const hasMoreEvents = $derived(detail.eventsNext !== undefined);
  const runNowBlocked = $derived(!row || !row.canRunNow || !!store.pending[`run:${row.id}`]);

  // The quiet countdown (a watch automation waiting for changes to settle) ticks locally.
  let nowMs = $state(Date.now());
  onMount(() => {
    const t = setInterval(() => (nowMs = Date.now()), 1000);
    return () => clearInterval(t);
  });
  const quietLeft = $derived(
    data?.quiet ? Math.max(0, Math.ceil((data.quiet.untilMs - nowMs) / 1000)) : 0,
  );
  const quietFraction = $derived(
    data?.quiet && data.quiet.idleSecs > 0
      ? Math.min(1, Math.max(0, 1 - quietLeft / data.quiet.idleSecs))
      : 0,
  );

  let showOps = $state(false);

  function resolveHeld() {
    const hold = row?.attention;
    if (!hold) return;
    const held = (data?.recentRuns ?? []).find((r) => r.id === hold.runId);
    if (held) {
      if (!store.expanded[held.id]) store.toggleRun(held);
    } else {
      store.closeDetail();
      store.showHistory('attention');
    }
  }
</script>

<div class="detail" data-testid="automation-detail">
  <div class="top">
    <button type="button" onclick={() => store.closeDetail()}>← Automations</button>
  </div>

  {#if detail.error}
    <p class="banner error" role="alert">{detail.error}</p>
  {/if}
  {#if store.notice}
    <p class="banner {store.notice.kind}" role={store.notice.kind === 'error' ? 'alert' : 'status'}>
      {store.notice.text}
    </p>
  {/if}

  {#if detail.loading && !data}
    <p class="muted">Loading…</p>
  {:else if data && row}
    <section class="head">
      <div class="title">
        <h3>{row.name}</h3>
        <StatusChip status={row.status} showSub />
      </div>
      <div class="controls">
        <label class="switch" title={row.enabled ? 'Enabled' : 'Disabled'}>
          <input
            type="checkbox"
            role="switch"
            checked={row.enabled}
            disabled={!!store.pending[`pause:${row.id}`]}
            aria-label="Enabled: {row.name}"
            onchange={() => void store.toggleEnabled(row)}
          />
          <span class="track"></span>
        </label>
        <button
          type="button"
          disabled={!!store.pending[`pause:${row.id}`]}
          onclick={() => void store.toggleEnabled(row)}
        >
          {row.enabled ? 'Pause' : 'Resume'}
        </button>
        <button
          type="button"
          disabled={runNowBlocked}
          title={row.runNowReason}
          onclick={() => void store.runNow(row)}
        >
          Run now
        </button>
        {#if run}
          <button
            type="button"
            class="danger"
            disabled={stopping}
            onclick={() => store.askStop(run)}
          >
            Stop current run
          </button>
        {/if}
        <button
          type="button"
          disabled={row.kind !== 'schedule'}
          title={row.kind !== 'schedule'
            ? 'Editing watch-folder automations is not available yet'
            : undefined}
          onclick={() => void store.openEdit(row.id)}
        >
          Edit
        </button>
        {#if row.showReview}
          <button type="button" class="primary" onclick={() => void store.openReview(row)}>
            Review changes
          </button>
        {:else if row.attention}
          <button type="button" class="primary" onclick={resolveHeld}>Resolve held run</button>
        {/if}
      </div>
      {#if !row.canRunNow && row.runNowReason}
        <p class="muted" data-testid="run-now-reason">{row.runNowReason}</p>
      {/if}
      {#if row.attention}
        <p class="hold" role="status" data-testid="attention-hold">
          {row.attention.text} &mdash; new runs are held until it is resolved.
        </p>
      {/if}
    </section>

    <section aria-label="Pipeline">
      <ol class="pipeline" data-testid="pipeline">
        {#each data.pipeline.steps as step, i (i)}
          <li
            class:cur={data.pipeline.current === i}
            class:done={data.pipeline.current !== undefined && i < data.pipeline.current}
            aria-current={data.pipeline.current === i ? 'step' : undefined}
          >
            {step}
          </li>
        {/each}
      </ol>
      {#if data.quiet}
        <div class="quiet" data-testid="quiet-bar">
          <div
            class="bar"
            role="progressbar"
            aria-label="Waiting for quiet"
            aria-valuemin="0"
            aria-valuemax={data.quiet.idleSecs}
            aria-valuenow={Math.round(quietFraction * data.quiet.idleSecs)}
          >
            <span style="width:{quietFraction * 100}%"></span>
          </div>
          <span class="muted">Runs in {quietLeft} s if nothing changes</span>
        </div>
      {/if}
      {#if data.pendingRerun}
        <p class="muted" data-testid="pending-rerun">1 pending rerun after this</p>
      {/if}
    </section>

    {#if run}
      <section class="run" aria-label="Current run" data-testid="current-run">
        <h4>Run #{run.id} <span class="muted">{run.triggerText}</span></h4>
        <p class="phase" data-testid="run-phase" aria-live="polite">{phaseText(run, stopping)}</p>
        <div
          class="bar"
          role="progressbar"
          aria-label="Run progress"
          aria-valuemin="0"
          aria-valuemax={run.opsTotal}
          aria-valuenow={Math.round(progressFraction(run) * run.opsTotal)}
        >
          <span style="width:{progressFraction(run) * 100}%"></span>
        </div>
        <p class="counts" data-testid="run-counts">{progressCounts(run)}</p>
        {#if run.opsTotal > 0}
          <button
            type="button"
            class="link"
            aria-expanded={showOps}
            onclick={() => {
              showOps = !showOps;
              if (showOps) void store.loadOps(run.id, 0);
            }}
          >
            {showOps ? 'Hide operations' : 'Show operations'}
          </button>
          {#if showOps}<RunOps {store} runId={run.id} />{/if}
        {/if}
      </section>
    {/if}

    {#if data.waiting.length > 0}
      <section aria-label="Waiting" data-testid="waiting">
        <h4>Waiting</h4>
        <ul class="plain">
          {#each data.waiting as w (w.id)}
            <li>
              <span>{w.triggerText}</span>
              {#if w.mergedCount > 0}
                <span class="muted">
                  &middot; {w.mergedCount} merged{w.mergedReasons.length
                    ? ` (${w.mergedReasons.join(', ')})`
                    : ''}
                </span>
              {/if}
              {#if w.waitsFor}
                <span class="muted">&middot; Waits for “{w.waitsFor}” — same folders</span>
              {/if}
            </li>
          {/each}
        </ul>
      </section>
    {/if}

    <section aria-label="Settings" class="facts">
      <dl>
        <dt>Trigger</dt>
        <dd data-testid="fact-trigger">{row.trigger}</dd>
        <dt>Folders</dt>
        <dd data-testid="fact-roots">
          <span class="path" title={data.profile.left}>{data.profile.left}</span>
          <span class="arrow" aria-hidden="true">⇄</span>
          <span class="path" title={data.profile.right}>{data.profile.right}</span>
        </dd>
        <dt>Runtime</dt>
        <dd>{row.runtimeNote}</dd>
        <dt>Profile</dt>
        <dd>
          {data.profile.name} &middot; rev {data.profile.revision}{row.approvedRevision !==
          undefined
            ? ` (approved rev ${row.approvedRevision})`
            : ''}
        </dd>
      </dl>
    </section>

    <section aria-label="Recent runs">
      <h4>Recent runs</h4>
      {#if recent.length === 0}
        <p class="muted">No runs yet.</p>
      {:else}
        <RunHistory {store} runs={recent} compact />
      {/if}
    </section>

    <section aria-label="Events">
      <h4>Events</h4>
      {#if events.length === 0}
        <p class="muted">No events yet.</p>
      {:else}
        <ul class="events" data-testid="events">
          {#each events as ev (ev.id)}
            <li data-kind={ev.kind}>
              <span
                class="glyph k-{ev.kind}"
                title={EVENT_GLYPH[ev.kind].label}
                aria-label={EVENT_GLYPH[ev.kind].label}
              >
                {EVENT_GLYPH[ev.kind].glyph}
              </span>
              <span class="when">{formatWhen(ev.atMs)}</span>
              <span class="text">{ev.text}</span>
            </li>
          {/each}
        </ul>
        {#if hasMoreEvents}
          <button
            type="button"
            class="link"
            disabled={detail.eventsLoading}
            onclick={() => void store.moreEvents()}
          >
            Show earlier events
          </button>
        {/if}
      {/if}
    </section>
  {/if}
</div>

<style>
  .detail {
    display: flex;
    flex-direction: column;
    gap: 12px;
    min-height: 0;
  }
  section {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  h3 {
    margin: 0;
    font-size: 15px;
    font-weight: 600;
  }
  h4 {
    margin: 0;
    font-size: 12.5px;
    font-weight: 600;
    color: var(--text2);
  }
  .head .title {
    display: flex;
    align-items: center;
    gap: 10px;
    flex-wrap: wrap;
  }
  .controls {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 6px;
  }
  button {
    height: 26px;
    padding: 0 12px;
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
  button.danger {
    color: var(--redInk);
  }
  button.link {
    align-self: flex-start;
    height: 22px;
    padding: 0 6px;
    background: transparent;
    color: var(--accentInk);
    text-decoration: underline;
  }
  button:focus-visible {
    outline: 2px solid var(--accentInk);
    outline-offset: 2px;
  }
  .banner {
    margin: 0;
    padding: 6px 8px;
    border-radius: var(--r);
    background: var(--hover);
    font-size: 12px;
    color: var(--text2);
  }
  .banner.error {
    border: 1px solid var(--red);
  }
  .hold {
    margin: 0;
    padding: 6px 10px;
    border: 1px solid var(--red);
    border-radius: var(--r);
    background: var(--redSoft);
    font-size: 12px;
  }
  .muted {
    margin: 0;
    font-size: 11.5px;
    color: var(--text3);
  }
  .pipeline {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .pipeline li {
    padding: 2px 10px;
    border-radius: 10px;
    background: var(--hover);
    color: var(--text3);
    font-size: 11.5px;
  }
  .pipeline li.done {
    color: var(--text2);
  }
  .pipeline li.cur {
    background: var(--accentSoft);
    color: var(--accentInk);
    font-weight: 600;
  }
  .bar {
    height: 6px;
    border-radius: 3px;
    background: var(--hover2);
    overflow: hidden;
  }
  .bar span {
    display: block;
    height: 100%;
    background: var(--accent);
    transition: width 160ms linear;
  }
  .quiet {
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .quiet .bar {
    flex: 1;
  }
  .run {
    padding: 10px;
    border: 1px solid var(--line);
    border-radius: var(--r);
  }
  .phase {
    margin: 0;
    font-size: 13px;
    font-weight: 600;
  }
  .counts {
    margin: 0;
    font-size: 12px;
    color: var(--text2);
  }
  .plain {
    margin: 0;
    padding: 0;
    list-style: none;
    font-size: 12px;
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .facts dl {
    display: grid;
    grid-template-columns: 90px 1fr;
    gap: 4px 10px;
    margin: 0;
    font-size: 12px;
  }
  .facts dt {
    color: var(--text3);
  }
  .facts dd {
    margin: 0;
    min-width: 0;
  }
  .path {
    font-family: var(--mono, monospace);
    font-size: 11.5px;
    overflow-wrap: anywhere;
  }
  .arrow {
    margin: 0 6px;
    color: var(--text3);
  }
  .events {
    margin: 0;
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: 3px;
    font-size: 12px;
  }
  .events li {
    display: flex;
    gap: 8px;
    align-items: baseline;
  }
  .events .when {
    flex: none;
    width: 110px;
    color: var(--text3);
    font-size: 11.5px;
  }
  .glyph {
    flex: none;
    width: 14px;
    text-align: center;
    font-weight: 700;
  }
  .k-ok {
    color: var(--greenInk);
  }
  .k-warn {
    color: var(--amberInk);
  }
  .k-bad {
    color: var(--redInk);
  }
  .k-mute {
    color: var(--text3);
  }
  .k-info {
    color: var(--accentInk);
  }
  .events li[data-kind='mute'] .text {
    color: var(--text3);
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
