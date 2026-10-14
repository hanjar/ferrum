<script lang="ts">
  /**
   * Synchronize Directories (F2-19): a MANDATORY dry-run preview built from
   * the live (recursive) directory compare, then an explicit execute step
   * that re-validates the snapshot first. Nothing runs until the user has
   * seen and approved this preview; overwrites and deletions each need their
   * own confirmation, and undecided conflicts are never executed.
   */
  import { compareStore } from '../stores/compareStore.svelte';
  import { isResolvable, type SyncAction, type SyncDirection } from '../syncPlan';

  const store = compareStore;

  const directions: { value: SyncDirection; label: string }[] = [
    { value: 'leftToRight', label: 'Left → Right' },
    { value: 'rightToLeft', label: 'Right → Left' },
    { value: 'both', label: 'Both directions' },
  ];

  const actionLabel: Record<SyncAction, string> = {
    copyToRight: 'copy →',
    copyToLeft: '← copy',
    updateRight: 'overwrite →',
    updateLeft: '← overwrite',
    deleteRight: 'delete (right)',
    deleteLeft: 'delete (left)',
    conflict: 'conflict — not executed until decided',
    skip: 'same',
  };

  const PAGE = 200;
  let planLimit = $state(PAGE);
  let logLimit = $state(PAGE);

  const conflictCount = $derived(store.syncSummary?.conflicts ?? 0);

  function summaryText(): string {
    const s = store.syncSummary;
    if (!s) return '';
    return (
      `${s.total} entries · ${s.toRight} to right · ${s.toLeft} to left` +
      (s.overwrites ? ` · ${s.overwrites} overwrites` : '') +
      (s.deletes ? ` · ${s.deletes} deletions` : '') +
      (s.conflicts ? ` · ${s.conflicts} undecided conflicts` : '') +
      (s.userSkipped ? ` · ${s.userSkipped} skipped by you` : '') +
      ` · ${s.skipped - s.userSkipped} already equal/ignored`
    );
  }
</script>

{#if store.syncOpen}
  <aside class="panel" aria-label="Synchronize Directories">
    <div class="headRow">
      <h2>Synchronize Dirs</h2>
      <button class="close" aria-label="Close synchronize panel" onclick={() => store.closeSync()}>
        ✕
      </button>
    </div>

    <div class="row" role="radiogroup" aria-label="Sync direction">
      {#each directions as d (d.value)}
        <label class="check">
          <input
            type="radio"
            name="sync-direction"
            value={d.value}
            checked={store.syncDirection === d.value}
            onchange={() => store.setSyncDirection(d.value)}
            disabled={store.syncBusy}
          />
          {d.label}
        </label>
      {/each}
    </div>
    <label class="check">
      <input
        type="checkbox"
        checked={store.syncByContent}
        onchange={(e) => void store.setSyncByContent((e.target as HTMLInputElement).checked)}
        disabled={store.syncBusy}
      />
      Compare contents (slower, byte-exact)
    </label>
    {#if store.syncDirection !== 'both'}
      <label class="check danger">
        <input
          type="checkbox"
          checked={store.syncDeleteExtras}
          onchange={(e) => store.setSyncDeleteExtras((e.target as HTMLInputElement).checked)}
          disabled={store.syncBusy}
        />
        Delete extra files on the target side (asks separately)
      </label>
    {/if}

    <p class="preview" role="status">
      {#if store.syncBusy}
        Working…
      {:else if store.syncPlan}
        Dry run — nothing executed yet. {summaryText()}
      {:else if store.syncBlocked}
        No runnable plan.
      {:else}
        No plan yet.
      {/if}
    </p>

    {#if store.syncReport?.fastScan && !store.syncBusy}
      <p class="hint" role="status">
        {#if store.syncReport.fastScan.left || store.syncReport.fastScan.right}
          Scan cepat (MFT) dipakai untuk
          {store.syncReport.fastScan.left && store.syncReport.fastScan.right
            ? 'kedua folder'
            : store.syncReport.fastScan.left
              ? 'folder kiri'
              : 'folder kanan'}. Sebelum menjalankan, sync memeriksa ulang dengan scan biasa.
        {/if}
        {store.syncReport.fastScan.note ?? ''}
      </p>
    {/if}

    {#if store.syncReport?.incremental && !store.syncBusy}
      <p class="hint" role="status" data-testid="sync-journal-skip">
        {store.syncReport.incremental.used
          ? `Sync cepat (journal indexer): isi ${store.syncReport.incremental.contentSkipped} file dilewati karena terbukti tidak berubah sejak sync terakhir.`
          : `Sync cepat tidak dipakai (${store.syncReport.incremental.reason ?? 'tidak tersedia'}): semua isi file dibaca.`}
      </p>
    {/if}
    {#if store.syncSettle && 'contentSkipped' in store.syncSettle && store.syncSettle.contentSkipped}
      <p class="hint" role="status" data-testid="sync-settle-skip">
        Verifikasi setelah sync: isi {store.syncSettle.contentSkipped} file dilewati (journal indexer
        membuktikan tidak berubah).
      </p>
    {/if}

    {#if store.syncLeaseBusy}
      <p class="error" role="alert" data-testid="sync-lease-busy">{store.syncLeaseBusy}</p>
    {:else if store.syncBlocked}
      <p class="error" role="alert">{store.syncBlocked}</p>
    {:else if store.syncStale}
      <p class="error" role="alert">
        Sources changed since this preview was generated — review the refreshed plan, then run
        again.
      </p>
    {:else if store.errorText}
      <p class="error" role="alert">{store.errorText}</p>
    {/if}
    {#if store.syncDone}
      <p class="done" role="status">
        Queued {store.syncDone.toRight} copy/update(s) → right, {store.syncDone.toLeft} ← left
        {#if store.syncDone.deletes}, {store.syncDone.deletes} deletion(s) awaiting confirmation{/if}.
        {#if store.syncDone.failed}{store.syncDone.failed} failed.{/if}
        {#if store.syncDone.skipped}{store.syncDone.skipped} not executed.{/if}
        {#if store.syncDone.cancelled}Cancelled before completion.{/if}
      </p>
    {/if}

    {#if store.syncConfirm}
      <div class="confirm" role="alertdialog" aria-label="Confirm overwrite">
        <p>
          {store.syncConfirm.overwrites} file(s) will be overwritten on the target side. Overwritten files
          are replaced, not backed up.
        </p>
        <div class="row buttons">
          <button type="button" onclick={() => (store.syncConfirm = null)}>Cancel</button>
          <button
            type="button"
            class="primary danger"
            onclick={() => void store.executeSync({ confirmOverwrite: true })}
          >
            Overwrite {store.syncConfirm.overwrites} file(s)
          </button>
        </div>
      </div>
    {/if}

    {#if conflictCount > 0}
      <div class="row" aria-label="Apply to all conflicts">
        <span>{conflictCount} conflict(s) — apply to all:</span>
        <button type="button" onclick={() => store.decideAllConflicts('keepLeft')}>
          Keep left
        </button>
        <button type="button" onclick={() => store.decideAllConflicts('keepRight')}>
          Keep right
        </button>
        <button type="button" onclick={() => store.decideAllConflicts('skip')}>Skip</button>
      </div>
    {/if}

    <ul class="results" aria-label="Synchronization plan">
      {#if store.syncPlan}
        {#each store.syncPlan.slice(0, planLimit) as item (item.name + item.action)}
          <li class="entry {item.action === 'skip' ? 'same' : ''}">
            <span class="name">{item.name}{item.isDir ? '/' : ''}</span>
            <span class="action">
              {actionLabel[item.action]}{item.decision ? ` (you: ${item.decision})` : ''}
            </span>
            {#if item.action === 'conflict'}
              <span class="decide">
                {#if isResolvable(item)}
                  <button type="button" onclick={() => store.decideConflict(item.name, 'keepLeft')}>
                    Keep left
                  </button>
                  <button
                    type="button"
                    onclick={() => store.decideConflict(item.name, 'keepRight')}
                  >
                    Keep right
                  </button>
                {/if}
                <button type="button" onclick={() => store.decideConflict(item.name, 'skip')}>
                  Skip
                </button>
              </span>
            {:else if item.decision}
              <span class="decide">
                <button type="button" onclick={() => store.decideConflict(item.name, null)}>
                  Undo
                </button>
              </span>
            {/if}
          </li>
        {:else}
          <li class="dim">The folders are already in sync.</li>
        {/each}
        {#if store.syncPlan.length > planLimit}
          <li class="dim">
            … and {store.syncPlan.length - planLimit} more entries (all included in the run).
            <button type="button" onclick={() => (planLimit = store.syncPlan?.length ?? PAGE)}>
              Show all {store.syncPlan.length}
            </button>
          </li>
        {/if}
      {:else if !store.syncBlocked}
        <li class="dim">Open with both panes on folders to build a plan.</li>
      {/if}
    </ul>

    {#if store.syncLog.length > 0}
      <p class="preview">Last run — per item</p>
      <ul class="results log" aria-label="Synchronization result log">
        {#each store.syncLog.slice(0, logLimit) as r (r.name + r.action)}
          <li class="entry {r.status}">
            <span class="name">{r.name}</span>
            <span class="action">{r.status}{r.reason ? ` — ${r.reason}` : ''}</span>
          </li>
        {/each}
        {#if store.syncLog.length > logLimit}
          <li class="dim">
            <button type="button" onclick={() => (logLimit = store.syncLog.length)}>
              Show all {store.syncLog.length}
            </button>
          </li>
        {/if}
      </ul>
    {/if}

    <div class="row buttons">
      {#if store.syncBusy}
        <button type="button" onclick={() => store.cancelSync()} title="Stop before the next step">
          Cancel
        </button>
      {/if}
      <button
        type="button"
        onclick={() => void store.openSync()}
        disabled={store.syncBusy}
        title="Regenerate the dry-run preview from the current folders"
      >
        Refresh Preview
      </button>
      <button
        type="button"
        class="primary"
        onclick={() => void store.executeSync()}
        disabled={store.syncBusy || !store.syncPlan}
        title="Executes exactly the previewed plan after re-checking the folders"
      >
        Run Synchronization
      </button>
    </div>
  </aside>
{/if}

<style>
  .panel {
    width: 360px;
    flex: none;
    min-height: 0;
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 10px;
    background: var(--pane);
    border-left: 1px solid var(--line);
    overflow: hidden;
  }
  .headRow {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }
  h2 {
    margin: 0;
    font-size: calc(13px + var(--uiStep, 0px));
    font-weight: 600;
  }
  .close {
    border: 0;
    background: transparent;
    color: var(--text3);
    cursor: default;
  }
  .close:hover {
    color: var(--text);
  }
  .row {
    display: flex;
    align-items: center;
    gap: 10px;
    font-size: calc(11.5px + var(--uiStep, 0px));
    color: var(--text2);
    flex-wrap: wrap;
  }
  .check {
    display: flex;
    align-items: center;
    gap: 4px;
    white-space: nowrap;
    font-size: 11.5px;
    color: var(--text2);
  }
  .check.danger {
    color: var(--red);
  }
  .preview {
    margin: 0;
    font-size: calc(12px + var(--uiStep, 0px));
    color: var(--text2);
  }
  .error {
    margin: 0;
    font-size: calc(11.5px + var(--uiStep, 0px));
    color: var(--red);
    word-break: break-word;
  }
  .hint {
    margin: 0;
    font-size: calc(11.5px + var(--uiStep, 0px));
    color: var(--text3);
    word-break: break-word;
  }
  .done {
    margin: 0;
    font-size: calc(11.5px + var(--uiStep, 0px));
    color: var(--green, var(--text2));
  }
  .dim {
    font-size: calc(11.5px + var(--uiStep, 0px));
    color: var(--text3);
  }
  .results {
    flex: 1;
    min-height: 0;
    overflow: auto;
    margin: 0;
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .results.log {
    max-height: 120px;
    flex: none;
  }
  .entry {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto;
    gap: 8px;
    padding: 3px 8px;
    border-radius: var(--r);
    font-size: calc(12px + var(--uiStep, 0px));
  }
  .entry:nth-child(odd) {
    background: var(--hover);
  }
  .entry.same {
    color: var(--text3);
  }
  .entry.failed .action {
    color: var(--red);
  }
  .decide {
    grid-column: 1 / -1;
    display: flex;
    gap: 4px;
  }
  .decide button {
    height: 20px;
    padding: 0 8px;
    font-size: 11px;
  }
  .confirm {
    border: 1px solid var(--red);
    border-radius: var(--r);
    padding: 6px 8px;
    font-size: 11.5px;
  }
  .confirm p {
    margin: 0 0 6px;
  }
  .name {
    min-width: 0;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .action {
    font-size: calc(10.5px + var(--uiStep, 0px));
    color: var(--text2);
    white-space: nowrap;
  }
  .buttons {
    justify-content: flex-end;
  }
  button {
    height: 24px;
    padding: 0 12px;
    border: 0;
    border-radius: var(--r);
    font-size: calc(12px + var(--uiStep, 0px));
    cursor: default;
    background: var(--hover2);
    color: var(--text2);
  }
  button:hover:not(:disabled) {
    color: var(--text);
  }
  button.primary {
    background: var(--accent);
    color: #fff;
    font-weight: 600;
  }
  button.primary.danger {
    background: var(--red);
  }
  button:disabled {
    opacity: 0.5;
  }
</style>
