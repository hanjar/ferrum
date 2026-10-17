<script lang="ts">
  /**
   * "Sync & Backup" > Automation (MH-1): a full page like Sync profiles. Tabs "Automations (n)" and
   * "Run history", the list, the schedule editor and the review screen. Shown only when the
   * automation UI feature flag is on (see featureFlags.ts).
   */
  import { onDestroy, onMount } from 'svelte';
  import Icon from '../Icon.svelte';
  import StatusChip from './StatusChip.svelte';
  import AutomationList from './AutomationList.svelte';
  import AutomationEditor from './AutomationEditor.svelte';
  import AutomationReview from './AutomationReview.svelte';
  import { automation, type AutomationStore } from '../../stores/automationStore.svelte';
  import { listenAutomationChanged } from '../../ipc/automation';
  import { ui } from '../../uiState.svelte';

  let {
    onclose,
    store = automation,
    openSyncProfiles = () => (ui.syncProfilesOpen = true),
  }: {
    onclose?: () => void;
    store?: AutomationStore;
    openSyncProfiles?: () => void;
  } = $props();

  let modalEl: HTMLDivElement | undefined = $state();
  let unlisten: (() => void) | null = null;
  let destroyed = false;

  onMount(() => {
    modalEl?.focus();
    void store.load();
    // MH-10: another process changed the store; re-read what is shown.
    void listenAutomationChanged(() => {
      void store.load();
      if (store.tab === 'history') void store.loadHistory();
    }).then((fn) => {
      if (destroyed) fn();
      else unlisten = fn;
    });
  });
  onDestroy(() => {
    destroyed = true;
    unlisten?.();
    store.dispose();
    store.closeEditor();
  });

  const attention = $derived(store.attentionCount);

  function onKey(e: KeyboardEvent) {
    if (e.key !== 'Escape') return;
    e.preventDefault();
    if (store.review?.approving) return;
    if (store.discardPending) store.discardPending = false;
    else if (store.view === 'review') store.backFromReview();
    else if (store.view === 'editor') store.leaveEditor();
    else onclose?.();
  }

  function closePage() {
    if (store.review?.approving) return;
    if (store.view === 'list') {
      onclose?.();
    } else if (store.leaveEditor()) {
      onclose?.();
    }
  }

  function when(ms: number): string {
    return new Date(ms).toLocaleString(undefined, {
      month: 'short',
      day: 'numeric',
      hour: '2-digit',
      minute: '2-digit',
    });
  }

  const title = $derived(
    store.view === 'editor'
      ? store.editor?.id
        ? 'Edit automation'
        : 'New automation'
      : store.view === 'review'
        ? 'Review & approve'
        : 'Automation',
  );
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="scrim" onmousedown={() => store.view === 'list' && onclose?.()}></div>
<div
  class="modal"
  role="dialog"
  aria-modal="true"
  aria-label="Automation"
  tabindex="-1"
  bind:this={modalEl}
  onkeydown={onKey}
>
  <header>
    <h2>{title}</h2>
    <button class="icon" title="Close" aria-label="Close" onclick={closePage}>
      <Icon name="close" size={11} />
    </button>
  </header>

  {#if store.discardPending}
    <div class="panel confirm" role="alertdialog" aria-label="Discard changes">
      <p>Discard your unsaved changes to this automation?</p>
      <div class="buttons">
        <button type="button" onclick={() => (store.discardPending = false)}>Keep editing</button>
        <button type="button" class="danger" onclick={() => store.closeEditor()}>
          Discard changes
        </button>
      </div>
    </div>
  {/if}

  {#if store.view === 'list'}
    <div class="tabs" role="tablist">
      <button
        type="button"
        role="tab"
        aria-selected={store.tab === 'automations'}
        class:on={store.tab === 'automations'}
        onclick={() => (store.tab = 'automations')}
      >
        Automations ({store.rows.length})
      </button>
      <button
        type="button"
        role="tab"
        aria-selected={store.tab === 'history'}
        class:on={store.tab === 'history'}
        onclick={() => store.showHistory(store.history.filter)}
      >
        Run history
        {#if attention > 0}<span class="badge" data-testid="attention-badge"
            >{attention} need attention</span
          >{/if}
      </button>
      <span class="spacer"></span>
      {#if store.tab === 'automations'}
        <button type="button" class="primary" onclick={() => store.openNew()}>
          New automation
        </button>
      {/if}
      <button type="button" onclick={() => void store.load()}>Refresh</button>
    </div>

    {#if store.notice}
      <p
        class="banner {store.notice.kind}"
        role={store.notice.kind === 'error' ? 'alert' : 'status'}
      >
        {store.notice.text}
      </p>
    {/if}
    {#if store.listError}
      <p class="banner error" role="alert">{store.listError}</p>
    {/if}

    {#if store.tab === 'automations'}
      <AutomationList {store} />
    {:else}
      <div class="filters" role="group" aria-label="Run history filter">
        <button
          type="button"
          class:on={store.history.filter === 'all'}
          onclick={() => void store.loadHistory('all')}>All runs</button
        >
        <button
          type="button"
          class:on={store.history.filter === 'attention'}
          onclick={() => void store.loadHistory('attention')}>Needs attention</button
        >
      </div>
      {#if store.history.error}
        <p class="banner error" role="alert">{store.history.error}</p>
      {/if}
      {#if store.history.runs.length === 0 && !store.history.loading && !store.history.error}
        <p class="muted" data-testid="history-empty">No runs to show.</p>
      {/if}
      <table class="history">
        <thead>
          <tr>
            <th>Run</th><th>Automation</th><th>Trigger</th><th>Started</th><th>Result</th>
            <th>Succeeded</th><th>Failed</th><th>Not started</th>
          </tr>
        </thead>
        <tbody>
          {#each store.history.runs as run (run.id)}
            <tr data-run={run.id}>
              <td>#{run.id}</td>
              <td>{run.automationName ?? 'Manual run'}</td>
              <td>{run.triggerText}</td>
              <td>{when(run.startedMs)}</td>
              <td><StatusChip status={run.result} /></td>
              <td>{run.counts.ok}</td>
              <td>{run.counts.failed}</td>
              <td>{run.counts.notStarted}</td>
            </tr>
          {/each}
        </tbody>
      </table>
      <p class="muted">
        Counts are file operations from the engine's comparison — not filesystem events.
      </p>
    {/if}
  {:else if store.view === 'editor' && store.editor}
    <AutomationEditor
      {store}
      editor={store.editor}
      oncreateprofile={openSyncProfiles}
      oneditprofile={() => openSyncProfiles()}
    />
  {:else if store.view === 'review' && store.review}
    <AutomationReview {store} review={store.review} oneditprofile={() => openSyncProfiles()} />
  {/if}
</div>

<style>
  .scrim {
    position: fixed;
    inset: 0;
    background: var(--scrim);
    z-index: 60;
    animation: fmFade 140ms ease-out;
  }
  .modal {
    position: fixed;
    left: 50%;
    top: 50%;
    transform: translate(-50%, -50%);
    width: min(940px, 94vw);
    height: min(720px, 88vh);
    padding: 16px;
    border-radius: calc(var(--rl) + 2px);
    background: var(--elev);
    box-shadow: var(--shadow);
    z-index: 61;
    display: flex;
    flex-direction: column;
    gap: 10px;
    overflow: auto;
    animation: fmUp 160ms cubic-bezier(0.2, 0.7, 0.2, 1);
  }
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }
  h2 {
    margin: 0;
    font-size: 14px;
    font-weight: 600;
  }
  .icon {
    width: 24px;
    height: 24px;
    padding: 0;
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
  .tabs,
  .filters {
    display: flex;
    gap: 6px;
    align-items: center;
  }
  .spacer {
    flex: 1;
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
  button:hover:not(:disabled) {
    color: var(--text);
  }
  button.on {
    background: var(--accentSoft);
    color: var(--accentInk);
  }
  button.primary {
    background: var(--accent);
    color: #fff;
  }
  button.danger {
    color: var(--redInk);
  }
  .badge {
    margin-left: 6px;
    padding: 0 6px;
    border-radius: 8px;
    background: var(--redSoft);
    color: var(--redInk);
    font-size: 11px;
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
  .panel.confirm {
    padding: 10px;
    border: 1px solid var(--line);
    border-radius: var(--r);
    font-size: 12.5px;
  }
  .panel.confirm p {
    margin: 0 0 8px;
  }
  .buttons {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
  }
  .muted {
    margin: 0;
    font-size: 11.5px;
    color: var(--text3);
  }
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
</style>
