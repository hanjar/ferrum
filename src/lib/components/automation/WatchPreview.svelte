<script lang="ts">
  /**
   * "Simulate a change" (WA-8): a LOCAL preview of the quiet countdown. Each press counts as one
   * change and restarts the countdown at the chosen idle delay; when it runs out the preview ends
   * with "Preview only — N changes batched into one run". Nothing here touches the backend, the
   * queue or the file system: the component has no store, no IPC and no network access, so a
   * preview can never run or queue anything.
   */
  import { onDestroy } from 'svelte';
  import { previewDone, previewHint, quietLabel } from './automationModel';

  let { idleSecs }: { idleSecs: number } = $props();

  const TICK_MS = 200;

  let changes = $state(0);
  /** Whole seconds left, or null when no countdown is running. */
  let left = $state<number | null>(null);
  let finished = $state(false);
  let endsAt = 0;
  let timer: ReturnType<typeof setInterval> | null = null;

  function stopTimer() {
    if (timer) clearInterval(timer);
    timer = null;
  }

  function tick() {
    const rest = Math.ceil((endsAt - Date.now()) / 1000);
    if (rest <= 0) {
      stopTimer();
      left = null;
      finished = true;
      return;
    }
    left = rest;
  }

  function simulate() {
    // A press after a finished preview starts a new one; a press during a countdown adds a change.
    changes = finished ? 1 : changes + 1;
    finished = false;
    endsAt = Date.now() + idleSecs * 1000;
    left = idleSecs;
    stopTimer();
    timer = setInterval(tick, TICK_MS);
  }

  // The delay was edited: the running preview no longer matches it.
  let lastIdle: number | undefined;
  $effect(() => {
    const idle = idleSecs;
    const changed = lastIdle !== undefined && lastIdle !== idle;
    lastIdle = idle;
    if (!changed) return;
    stopTimer();
    left = null;
    finished = false;
    changes = 0;
  });

  onDestroy(stopTimer);
</script>

<div class="preview" data-testid="watch-preview">
  <button type="button" onclick={simulate}>Simulate a change</button>
  <div class="out" role="status" aria-live="polite" data-testid="watch-preview-text">
    {#if left !== null}
      <span class="count" data-testid="watch-preview-count">{quietLabel(left)}</span>
      <span class="hint">{previewHint(idleSecs)}</span>
    {:else if finished}
      <span class="done">{previewDone(changes)}</span>
    {:else}
      <span class="hint">Preview only. Nothing is run or queued.</span>
    {/if}
  </div>
</div>

<style>
  .preview {
    display: flex;
    align-items: center;
    gap: 10px;
    flex-wrap: wrap;
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
  }
  .out {
    display: flex;
    gap: 8px;
    align-items: baseline;
    font-size: 12px;
  }
  .count {
    font-weight: 600;
    color: var(--accentInk);
  }
  .hint {
    color: var(--text3);
    font-size: 11.5px;
  }
  .done {
    color: var(--text2);
  }
</style>
