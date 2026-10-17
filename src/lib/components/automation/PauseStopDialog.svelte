<script lang="ts">
  /**
   * The Pause and Stop dialogs (RQ-8). Both are plain buttons that call the store; the dialog
   * holds no state of its own beyond focus. The safe choice is the default: "Pause & let it
   * finish", and for Stop the button that keeps the run going.
   *
   * Pause (RQ-8.2): "Pause & let it finish" (default), "Pause & stop current run", Cancel.
   * Stop (RQ-8.1): "Stop the current run?" with the no-rollback note.
   */
  import { onMount } from 'svelte';
  import type { AutomationStore, DialogState } from '../../stores/automationStore.svelte';

  let { store, dialog }: { store: AutomationStore; dialog: DialogState } = $props();

  let defaultBtn: HTMLButtonElement | undefined = $state();
  let box: HTMLDivElement | undefined = $state();

  const busy = $derived(
    dialog.kind === 'pause'
      ? !!store.pending[`pause:${dialog.id}`]
      : !!store.pending[`stop:${dialog.runId}`],
  );

  onMount(() => defaultBtn?.focus());

  function onKey(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      store.closeDialog();
      return;
    }
    if (e.key !== 'Tab' || !box) return;
    // Keep focus inside the dialog.
    const items = [...box.querySelectorAll<HTMLButtonElement>('button:not(:disabled)')];
    if (items.length === 0) return;
    const first = items[0];
    const last = items[items.length - 1];
    if (e.shiftKey && document.activeElement === first) {
      e.preventDefault();
      last.focus();
    } else if (!e.shiftKey && document.activeElement === last) {
      e.preventDefault();
      first.focus();
    }
  }
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="overlay" onmousedown={(e) => e.target === e.currentTarget && store.closeDialog()}>
  <div
    class="dlg"
    role="alertdialog"
    aria-modal="true"
    aria-labelledby="ps-title"
    aria-describedby="ps-body"
    tabindex="-1"
    bind:this={box}
    onkeydown={onKey}
    data-testid="{dialog.kind}-dialog"
  >
    {#if dialog.kind === 'pause'}
      <h3 id="ps-title">Pause “{dialog.name}”?</h3>
      <p id="ps-body">
        A run is in progress. Pausing also removes this automation's queued runs and any pending
        rerun.
      </p>
      <div class="buttons">
        <button type="button" disabled={busy} onclick={() => store.closeDialog()}>Cancel</button>
        <button
          type="button"
          class="danger"
          disabled={busy}
          onclick={() => void store.pauseWith('stop')}
        >
          Pause &amp; stop current run
        </button>
        <button
          type="button"
          class="primary"
          bind:this={defaultBtn}
          disabled={busy}
          onclick={() => void store.pauseWith('finish')}
        >
          Pause &amp; let it finish
        </button>
      </div>
    {:else}
      <h3 id="ps-title">Stop the current run?</h3>
      <p id="ps-body">
        The file in progress finishes or is rolled back, and the remaining operations are not
        started. Files that were already synced stay as they are &mdash; there is no automatic
        rollback. Monitoring stays on.
      </p>
      <div class="buttons">
        <button
          type="button"
          class="primary"
          bind:this={defaultBtn}
          disabled={busy}
          onclick={() => store.closeDialog()}
        >
          Keep running
        </button>
        <button
          type="button"
          class="danger"
          disabled={busy}
          onclick={() => void store.confirmStop()}
        >
          Stop run
        </button>
      </div>
    {/if}
  </div>
</div>

<style>
  .overlay {
    position: absolute;
    inset: 0;
    z-index: 5;
    display: grid;
    place-items: center;
    background: var(--scrim);
    border-radius: inherit;
  }
  .dlg {
    width: min(460px, 92%);
    padding: 16px;
    border-radius: var(--rl);
    background: var(--elev);
    box-shadow: var(--shadow);
    border: 1px solid var(--line);
  }
  h3 {
    margin: 0 0 8px;
    font-size: 14px;
    font-weight: 600;
  }
  p {
    margin: 0 0 14px;
    font-size: 12.5px;
    color: var(--text2);
    line-height: 1.45;
  }
  .buttons {
    display: flex;
    justify-content: flex-end;
    flex-wrap: wrap;
    gap: 8px;
  }
  button {
    height: 28px;
    padding: 0 12px;
    border: 0;
    border-radius: var(--r);
    background: var(--hover2);
    color: var(--text2);
    font-size: 12px;
    cursor: default;
  }
  button:hover:not(:disabled) {
    color: var(--text);
  }
  button:disabled {
    opacity: 0.5;
  }
  button.primary {
    background: var(--accent);
    color: #fff;
  }
  button.danger {
    color: var(--redInk);
  }
  button:focus-visible {
    outline: 2px solid var(--accentInk);
    outline-offset: 2px;
  }
</style>
