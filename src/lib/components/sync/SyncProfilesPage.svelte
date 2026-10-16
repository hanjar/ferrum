<script lang="ts">
  /**
   * "Sync & Backup" > Sync profiles (SP-9.1): a full page like Activity. Lists the stored profiles,
   * opens the editor, and offers Preview sync / Run now. Everything decided about a profile (its
   * readiness, validation, the delete guard, native confirmations) comes from the backend.
   */
  import { onDestroy, onMount } from 'svelte';
  import Icon from '../Icon.svelte';
  import SyncProfileEditor from './SyncProfileEditor.svelte';
  import { syncProfiles, type SyncProfilesStore } from '../../stores/syncProfilesStore.svelte';
  import { activePane, inactivePane } from '../../stores/panesRegistry.svelte';
  import {
    blockerMessage,
    guardMessage,
    readinessLabel,
    readinessReason,
    runResultMessage,
    summaryLine,
  } from './syncProfileModel';

  let { onclose, store = syncProfiles }: { onclose?: () => void; store?: SyncProfilesStore } =
    $props();

  let modalEl: HTMLDivElement | undefined = $state();
  let polls = 0;
  let pollTimer: ReturnType<typeof setTimeout> | null = null;

  onMount(() => {
    modalEl?.focus();
    void store.load();
  });
  onDestroy(() => {
    if (pollTimer) clearTimeout(pollTimer);
    // However the page went away, no half-edited editor stays behind in the shared store.
    store.closeEditor();
  });

  // The backend answers `checking` for a slow root (SP-8 budget); ask again a few times.
  $effect(() => {
    if (store.anyChecking && polls < 6 && !pollTimer) {
      pollTimer = setTimeout(() => {
        pollTimer = null;
        polls++;
        void store.load();
      }, 2500);
    }
    if (!store.anyChecking) polls = 0;
  });

  const editor = $derived(store.editor);
  const confirmDelete = $derived(
    store.confirmDeleteId ? store.summary(store.confirmDeleteId) : undefined,
  );

  function useCurrentPanes() {
    store.useCurrentPanes(activePane()?.path, inactivePane()?.path);
  }

  function onKey(e: KeyboardEvent) {
    if (e.key !== 'Escape') return;
    e.preventDefault();
    if (store.discardTarget) store.discardTarget = null;
    else if (store.runPrompt) store.cancelRunPrompt();
    else if (store.confirmDeleteId) store.cancelDelete();
    else if (editor) store.leaveEditor('list');
    else onclose?.();
  }

  /** The X button: with unsaved editor changes it asks first, like Escape and Cancel do. */
  function closePage() {
    if (editor) {
      if (store.leaveEditor('page')) onclose?.();
    } else onclose?.();
  }

  function discard() {
    const target = store.discardTarget;
    store.closeEditor();
    if (target === 'page') onclose?.();
  }

  const runningNow = (id: string) =>
    store.run?.profileId === id &&
    (store.run.status === 'starting' || store.run.status === 'running');
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="scrim" onmousedown={() => !editor && onclose?.()}></div>
<div
  class="modal"
  role="dialog"
  aria-modal="true"
  aria-label="Sync profiles"
  tabindex="-1"
  bind:this={modalEl}
  onkeydown={onKey}
>
  <header>
    <h2>{editor ? (editor.id ? 'Edit sync profile' : 'New sync profile') : 'Sync profiles'}</h2>
    <button class="icon" title="Close" aria-label="Close" onclick={closePage}>
      <Icon name="close" size={11} />
    </button>
  </header>

  {#if editor && store.discardTarget}
    <div class="panel confirm" role="alertdialog" aria-label="Discard changes">
      <p>Discard your unsaved changes to this profile?</p>
      <div class="buttons">
        <button type="button" onclick={() => (store.discardTarget = null)}>Keep editing</button>
        <button type="button" class="danger" onclick={discard}>Discard changes</button>
      </div>
    </div>
  {/if}

  {#if editor}
    <SyncProfileEditor
      {store}
      {editor}
      activePath={activePane()?.path}
      inactivePath={inactivePane()?.path}
      oncancel={() => void store.leaveEditor('list')}
    />
  {:else}
    <div class="toolbar">
      <button type="button" class="primary" onclick={() => store.openNew()}>
        New sync profile
      </button>
      <button type="button" onclick={useCurrentPanes}>Use current panes</button>
      <button type="button" class="right" onclick={() => void store.load()}>Refresh</button>
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

    {#if store.loaded && store.profiles.length === 0 && !store.listError}
      <div class="empty" data-testid="sync-empty">
        <h3>No sync profiles yet</h3>
        <p>
          A sync profile saves two folders, a direction, how deletions are handled and which files
          to skip. You can preview it, run it by hand, and later reuse it in an automation.
        </p>
      </div>
    {/if}

    <ul class="profiles">
      {#each store.profiles as p (p.id)}
        {@const preview = store.preview?.profileId === p.id ? store.preview : null}
        {@const run = store.run?.profileId === p.id ? store.run : null}
        <li class="profile" data-profile={p.id}>
          <div class="head">
            <span class="name">{p.name}</span>
            <span class="rev">Revision {p.revision}</span>
            <span
              class="chip"
              class:ok={p.readiness.status === 'ready'}
              class:warn={p.readiness.status === 'notReady'}
            >
              {readinessLabel(p.readiness)}
            </span>
          </div>
          <div class="paths">{p.left} ⇄ {p.right}</div>
          <div class="meta">
            {p.modeLabel}{p.deleteLabel ? ` · ${p.deleteLabel}` : ''}{p.usedBy > 0
              ? ` · Used by ${p.usedBy} ${p.usedBy === 1 ? 'automation' : 'automations'}`
              : ''}
          </div>
          {#if p.readiness.status === 'notReady'}
            <div class="reason">{readinessReason(p.readiness)}</div>
          {/if}

          <div class="actions">
            <button
              type="button"
              disabled={p.readiness.status !== 'ready' ||
                store.previewBusyId !== null ||
                runningNow(p.id)}
              onclick={() => void store.previewProfile(p.id)}
            >
              {store.previewBusyId === p.id ? 'Comparing…' : 'Preview sync'}
            </button>
            <button
              type="button"
              disabled={!store.canRun(p.id)}
              title={preview ? undefined : 'Preview the profile first'}
              onclick={() => void store.requestRun(p.id)}
            >
              Run now
            </button>
            <button type="button" onclick={() => void store.openEdit(p.id)}>Edit</button>
            <button type="button" class="danger" onclick={() => store.askDelete(p.id)}
              >Delete</button
            >
          </div>

          {#if preview}
            <div class="panel" role="region" aria-label="Preview">
              <div class="line" data-testid="preview-summary">
                {summaryLine(preview.data.summary)}
              </div>
              {#each preview.data.blockers ?? [] as b (b)}
                <div class="err">{blockerMessage(b)}</div>
              {/each}
              {#if guardMessage(preview.data.guard)}
                <div class="warn">{guardMessage(preview.data.guard)}</div>
              {/if}
              {#if preview.data.conflicts > 0}
                <div class="hint">
                  {preview.data.conflicts}
                  {preview.data.conflicts === 1 ? 'conflict is' : 'conflicts are'}
                  held and will not be changed.
                </div>
              {/if}
            </div>
          {/if}

          {#if store.runPrompt?.profileId === p.id}
            {@const rp = store.runPrompt}
            <div class="panel confirm" role="alertdialog" aria-label="Confirm run">
              {#if rp.overwrites > 0}
                <p>
                  {rp.overwrites}
                  {rp.overwrites === 1 ? 'file' : 'files'} will be overwritten on the target side. Overwritten
                  files are replaced in place unless this profile keeps old versions in a versioning folder.
                </p>
              {/if}
              {#if rp.deletes > 0}
                <p>
                  {rp.deletes}
                  {rp.deletes === 1 ? 'item' : 'items'} will be deleted ({p.deleteLabel}).
                </p>
              {/if}
              <div class="buttons">
                <button type="button" onclick={() => store.cancelRunPrompt()}>Cancel</button>
                <button type="button" class="danger" onclick={() => void store.confirmRunPrompt()}>
                  Confirm and run
                </button>
              </div>
            </div>
          {/if}

          {#if confirmDelete && confirmDelete.id === p.id}
            <div class="panel confirm" role="alertdialog" aria-label="Confirm delete">
              <p>
                Delete the profile "{p.name}"? This removes the profile only; your files are not
                touched.
              </p>
              <div class="buttons">
                <button type="button" onclick={() => store.cancelDelete()}>Cancel</button>
                <button type="button" class="danger" onclick={() => void store.confirmDelete()}>
                  Delete profile
                </button>
              </div>
            </div>
          {/if}

          {#if run}
            <div class="panel" role="status" aria-label="Run status">
              {#if run.status === 'starting'}
                Starting…
              {:else if run.status === 'running'}
                Running{run.phase ? ` (${run.phase})` : ''}…
                {#if run.counts}{run.counts.done} of {run.counts.total} done{/if}
                {#if run.progressUnavailable}
                  <div class="hint">
                    Progress isn't available, so the result will not show here. Hide this status and
                    refresh to check.
                  </div>
                {/if}
                <button type="button" onclick={() => store.dismissRun()}>Hide status</button>
              {:else if run.status === 'finished' && run.result}
                <div class:err={run.result.kind === 'failed' || run.result.kind === 'interrupted'}>
                  {runResultMessage(run.result)}
                </div>
                {#each run.result.problems.slice(0, 5) as pr (pr.index)}
                  <div class="hint">{pr.name}: {pr.status}{pr.reason ? ` (${pr.reason})` : ''}</div>
                {/each}
                <button type="button" onclick={() => store.dismissRun()}>Dismiss</button>
              {:else if run.status === 'failed' && run.error}
                <div class={run.error.kind === 'error' ? 'err' : 'hint'}>{run.error.text}</div>
                <button type="button" onclick={() => store.dismissRun()}>Dismiss</button>
              {/if}
            </div>
          {/if}
        </li>
      {/each}
    </ul>
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
    width: min(860px, 94vw);
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
  .toolbar {
    display: flex;
    gap: 8px;
    align-items: center;
  }
  .toolbar .right {
    margin-left: auto;
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
  .banner.success {
    border: 1px solid var(--green);
  }
  .empty {
    padding: 18px;
    border: 1px dashed var(--line);
    border-radius: var(--r);
    color: var(--text2);
  }
  .empty h3 {
    margin: 0 0 6px;
    font-size: 13px;
  }
  .empty p {
    margin: 0;
    font-size: 12.5px;
  }
  .profiles {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .profile {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 10px 12px;
    border: 1px solid var(--line);
    border-radius: var(--r);
  }
  .head {
    display: flex;
    align-items: baseline;
    gap: 8px;
  }
  .name {
    font-size: 13.5px;
    font-weight: 600;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .rev {
    font-size: 11.5px;
    color: var(--text3);
  }
  .chip {
    margin-left: auto;
    padding: 0 7px;
    border-radius: 8px;
    background: var(--hover2);
    color: var(--text2);
    font-size: 11px;
  }
  .chip.ok {
    background: var(--greenSoft);
    color: var(--greenInk);
  }
  .chip.warn {
    background: var(--redSoft);
    color: var(--redInk);
  }
  .paths,
  .meta {
    font-size: 12px;
    color: var(--text3);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .reason {
    font-size: 12px;
    color: var(--text2);
  }
  .actions {
    display: flex;
    gap: 6px;
    margin-top: 4px;
  }
  .panel {
    margin-top: 4px;
    padding: 6px 8px;
    border-radius: var(--r);
    background: var(--hover);
    font-size: 12px;
    color: var(--text2);
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .panel.confirm {
    border: 1px solid var(--red);
    background: transparent;
  }
  .panel p {
    margin: 0;
  }
  .buttons {
    display: flex;
    justify-content: flex-end;
    gap: 6px;
  }
  .err {
    color: var(--redInk);
  }
  .warn {
    color: var(--text);
  }
  .hint {
    color: var(--text3);
  }
</style>
