<script lang="ts">
  // Kartu run: semua label status berasal dari state executor (run.label). Teks model dan
  // nama file adalah data tak tepercaya, dirender sebagai teks (tanpa {@html}).
  import {
    describeSettlement,
    jobsProgress,
    PAGE_SIZE,
    type ChatStore,
    type RunView,
  } from '../../ai/chatStore.svelte';
  import { compareStore } from '../../stores/compareStore.svelte';
  import { aiChatHost } from '../../ai/aiChatHost.svelte';
  import { getAction, runAction } from '../../actions';
  import { duWorkspace } from '../../diskUsage/duWorkspace.svelte';
  import { describeConnection } from '../../ai/providerErrors';
  import { providerStatus, type ProviderStatus } from '../../ai/providerStatus.svelte';
  import MarkdownText from './MarkdownText.svelte';

  let {
    run,
    store,
    status = providerStatus,
  }: { run: RunView; store: ChatStore; status?: ProviderStatus } = $props();

  // FER-465: tombol aksi galat provider. [Coba lagi] hanya untuk run yang gagal (permintaan dikirim ulang).
  const providerActions = $derived(
    (run.providerError?.actions ?? []).filter((a) => a !== 'retry' || run.state === 'failed'),
  );
  // FER-573: fixed UI hint from the executor (never model text). The button only OPENS Disk Usage
  // (read-only); once it is open the hint is moot, so the button disappears.
  const showOpenDiskUsage = $derived(run.uiHint === 'openDiskUsage' && !duWorkspace.open);
  function openDiskUsage() {
    const action = getAction('commands.diskUsage');
    if (action && !duWorkspace.open) runAction(action);
  }
  const testNote = $derived(
    status.state === 'ok'
      ? 'Koneksi ke server AI berhasil.'
      : status.state === 'down' && status.kind
        ? describeConnection(status.kind, status.hostPort)
        : status.state === 'checking'
          ? 'Menguji koneksi…'
          : '',
  );

  const CAPABILITY: Record<string, string> = {
    'archive.create': 'Create archive (ZIP / 7z)',
    'directories.sync': 'Two-way directory sync',
    'items.delete': 'Move to Recycle Bin',
  };
  const CONFLICT_PAGE = 20;
  const JOB_CAP = 50;

  let conflictPage = $state(0);
  let showItems = $state(false);

  const plan = $derived(run.plan);
  // FER-576: a delete card is a WARNING card (never auto-approved; Recycle Bin only).
  const isDelete = $derived(plan?.operation.kind === 'items.delete');
  const deleteNames = $derived((run.items?.rows ?? []).slice(0, 10));
  // Review R1 S4: the confirm button of a delete stays off until the names it will delete are on screen.
  const namesShown = $derived(
    !!run.items && !run.items.loading && !run.items.error && run.items.rows.length > 0,
  );
  $effect(() => {
    // The names come from the local frozen manifest; they are never sent to the provider.
    if (isDelete && run.state === 'awaitingApproval' && !run.items)
      void store.loadItems(run.runId, 0);
  });
  const progress = $derived(jobsProgress(run.jobs));
  const active = $derived(run.state === 'queued' || run.state === 'running');
  const cancellable = $derived(
    ![
      'succeeded',
      'partial',
      'failed',
      'cancelled',
      'blockedCapability',
      'stale',
      'unknownOutcome',
    ].includes(run.state),
  );
  const editable = $derived(['preview', 'awaitingApproval', 'needsInput'].includes(run.state));
  const conflictSlice = $derived(
    run.conflicts.slice(conflictPage * CONFLICT_PAGE, (conflictPage + 1) * CONFLICT_PAGE),
  );
  const items = $derived(run.items);
  const tone = $derived(
    run.state === 'failed' || run.state === 'unknownOutcome'
      ? 'bad'
      : run.state === 'cancelled' ||
          run.state === 'blockedCapability' ||
          run.state === 'stale' ||
          run.state === 'partial'
        ? 'warn'
        : run.state === 'succeeded'
          ? 'good'
          : 'info',
  );
  // "Resolve in Sync panel": UI-only hand-off of this run's folder pair to the existing Synchronize
  // Dirs panel. The chat never picks a winner; roots stay local (never sent to the provider).
  const resolveRoots = $derived(
    plan?.operation.kind === 'directories.sync' && run.conflicts.length > 0
      ? { left: plan.operation.leftRoot, right: plan.operation.rightRoot }
      : null,
  );
  const settlement = $derived(run.settlement ? describeSettlement(run.settlement) : undefined);
  const bytes = (n: number) =>
    n < 1024
      ? `${n} B`
      : n < 1048576
        ? `${(n / 1024).toFixed(1)} KB`
        : `${(n / 1048576).toFixed(1)} MB`;

  function toggleItems() {
    showItems = !showItems;
    if (showItems && !run.items) void store.loadItems(run.runId, 0);
  }
</script>

<section class="card {tone}" aria-label="Request: {run.userText.slice(0, 60)}">
  <div class="status" role="status" aria-live="polite">
    <span class="dot" aria-hidden="true"></span>
    <strong>{run.cancelRequested && active ? 'Cancelling…' : run.label}</strong>
    {#if run.chain}
      <span class="rev" data-testid="chain-step"
        >Langkah {run.chain.index + 1} dari {run.chain.total}</span
      >
    {/if}
  </div>
  {#if run.chain?.note}
    <p class="muted" data-testid="chain-note">{run.chain.note}</p>
  {/if}
  {#if run.navigate}
    <p class={run.navigate.result?.ok === false ? 'error' : 'muted'} data-testid="navigate-result">
      {run.navigate.summary}
      {#if run.navigate.result}{run.navigate.result.message}{/if}
    </p>
  {/if}

  {#if run.modelText}
    <div class="note">
      <span class="tag">Assistant note</span>
      <MarkdownText text={run.modelText} />
    </div>
  {/if}

  {#if run.blockedReason}
    <p class="blocked" role="alert">{run.blockedReason} Nothing was run.</p>
  {/if}
  {#if run.errorMessage}
    <p class="error" role="alert" data-testid="provider-error-message">{run.errorMessage}</p>
  {/if}
  {#if run.providerError?.detail}
    <p class="muted" data-testid="provider-error-detail">{run.providerError.detail}</p>
  {/if}
  {#if showOpenDiskUsage}
    <div class="actions" data-testid="disk-usage-hint-actions">
      <button type="button" onclick={openDiskUsage}>Buka Disk Usage (Alt+Shift+D)</button>
    </div>
  {/if}
  {#if providerActions.length}
    <div class="actions" data-testid="provider-error-actions">
      {#if providerActions.includes('retry')}
        <button type="button" onclick={() => store.retry(run.runId)}>Coba lagi</button>
      {/if}
      {#if providerActions.includes('settings')}
        <button type="button" class="ghost" onclick={() => aiChatHost.openSettings()}
          >Buka Settings ▸ AI</button
        >
      {/if}
      {#if providerActions.includes('test')}
        <button
          type="button"
          class="ghost"
          disabled={status.state === 'checking'}
          onclick={() => void status.check(true)}>Test connection</button
        >
      {/if}
    </div>
    {#if testNote}<p class="muted" role="status" data-testid="provider-test-note">
        {testNote}
      </p>{/if}
  {/if}

  {#if plan}
    <h3>
      {CAPABILITY[plan.capability] ?? plan.capability}
      {#if run.revision > 1}<span class="rev" data-testid="revision">Revision {run.revision}</span
        >{/if}
    </h3>
    {#if run.revisionNotes.length}
      <ul class="rows" aria-label="Plan changes" data-testid="revision-notes">
        {#each run.revisionNotes as note, i (i)}
          <li class="muted">{note}</li>
        {/each}
      </ul>
    {/if}
    {#if active}
      <p class="muted" data-testid="running-hint">
        Changes typed now do not alter this job. Type "cancel" to stop it, then start a new request.
      </p>
    {/if}
    {#if isDelete && plan.operation.kind === 'items.delete'}
      <div class="deleteWarn" role="alert" data-testid="delete-warning">
        <strong>⚠ Peringatan: {plan.preview.itemCount} item akan dipindahkan ke Recycle Bin</strong>
        <p class="muted">
          Dari {plan.operation.sourceDirectory}. Item bisa dikembalikan dari Recycle Bin; ini bukan
          hapus permanen. Pastikan daftar di bawah benar sebelum menekan tombol konfirmasi.
        </p>
        {#if run.items && run.items.rows.length}
          <ul class="rows" aria-label="Items to delete" data-testid="delete-items">
            {#each deleteNames as row (row.ref)}
              <li>
                <span class="mono">{row.label}</span>
                {#if row.detail}<span class="muted">{row.detail}</span>{/if}
              </li>
            {/each}
          </ul>
          {#if run.items.total > deleteNames.length}
            <p class="muted">dan {run.items.total - deleteNames.length} item lainnya</p>
          {/if}
        {:else if run.items?.error}
          <p class="error" role="alert">
            Daftar item tidak dapat dimuat, jadi penghapusan tidak bisa disetujui. Batalkan dan coba
            lagi.
          </p>
        {:else if run.state === 'awaitingApproval'}
          <p class="muted">Memuat daftar item…</p>
        {/if}
      </div>
    {/if}
    <dl class="kv">
      {#if plan.operation.kind === 'archive.create'}
        <dt>Output</dt>
        <dd class="mono">{plan.operation.targetDirectory}\{plan.operation.outputName}</dd>
      {:else if plan.operation.kind === 'items.delete'}
        <dt>Destination</dt>
        <dd>Recycle Bin</dd>
      {:else}
        <dt>Left root</dt>
        <dd class="mono">{plan.operation.leftRoot}</dd>
        <dt>Right root</dt>
        <dd class="mono">{plan.operation.rightRoot}</dd>
      {/if}
      {#each plan.sourceSnapshot.panes as pane (pane.paneId)}
        <dt>
          {pane.paneId === 'left' ? 'Left' : 'Right'} panel{plan.sourceSnapshot
            .activePaneAtSubmit === pane.paneId
            ? ' (active)'
            : ''}
        </dt>
        <dd>
          <span class="mono">{pane.location.path}</span>
          <span class="muted">
            · {pane.location.kind} · filter {pane.filter ? `"${pane.filter}"` : 'none'} · hidden {pane.showHidden
              ? 'shown'
              : 'not included'}
          </span>
        </dd>
      {/each}
      <dt>Scope</dt>
      <dd>
        {plan.preview.itemCount} items · {plan.preview.totalBytes > 0
          ? bytes(plan.preview.totalBytes)
          : 'size unknown'} · {plan.preview.conflictCount} conflicts · {plan.stepCount} step{plan.stepCount ===
        1
          ? ''
          : 's'}
      </dd>
      {#if run.scopeNote}
        <dt>Includes</dt>
        <dd data-testid="scope-note">{run.scopeNote}</dd>
      {/if}
      {#each run.warnings ?? [] as warning (warning)}
        <dt>Note</dt>
        <dd class="warning">{warning}</dd>
      {/each}
    </dl>

    {#if run.conflicts.length}
      <div class="sub">
        <h4>Conflicts ({run.conflicts.length})</h4>
        {#if resolveRoots}
          <div class="actions">
            <button
              type="button"
              class="ghost"
              data-testid="resolve-in-sync"
              disabled={compareStore.syncBusy}
              title="Opens Synchronize Dirs for these two folders. You decide each conflict there."
              onclick={() => void compareStore.openSync(resolveRoots)}
            >
              Resolve in Sync panel
            </button>
          </div>
        {/if}
        <ul class="rows">
          {#each conflictSlice as c (c.itemRef)}
            <li><span class="mono">{c.label}</span> <span class="muted">{c.kind}</span></li>
          {/each}
        </ul>
        {#if run.conflicts.length > CONFLICT_PAGE}
          <div class="pager">
            <button
              type="button"
              class="ghost"
              disabled={conflictPage === 0}
              onclick={() => conflictPage--}
            >
              Previous
            </button>
            <span class="muted">
              {conflictPage * CONFLICT_PAGE + 1}–{Math.min(
                run.conflicts.length,
                (conflictPage + 1) * CONFLICT_PAGE,
              )} of
              {run.conflicts.length}
            </span>
            <button
              type="button"
              class="ghost"
              disabled={(conflictPage + 1) * CONFLICT_PAGE >= run.conflicts.length}
              onclick={() => conflictPage++}
            >
              Next
            </button>
          </div>
        {/if}
      </div>
    {/if}

    <div class="sub">
      <button type="button" class="ghost" aria-expanded={showItems} onclick={toggleItems}>
        {showItems ? 'Hide items' : 'Show items'}
      </button>
      {#if showItems}
        {#if !items || (items.loading && !items.rows.length)}
          <p class="muted">Loading…</p>
        {:else}
          <ul class="rows" aria-label="Plan items">
            {#each items.rows as row (row.ref)}
              <li>
                <span class="mono">{row.label}</span>
                {#if row.detail}<span class="muted">{row.detail}</span>{/if}
              </li>
            {/each}
          </ul>
          {#if items.error}<p class="error" role="alert">{items.error}</p>{/if}
          <div class="pager">
            <button
              type="button"
              class="ghost"
              disabled={items.offset === 0 || items.loading}
              onclick={() => store.loadItems(run.runId, Math.max(0, items.offset - PAGE_SIZE))}
            >
              Previous
            </button>
            <span class="muted">
              {items.total ? items.offset + 1 : 0}–{Math.min(items.total, items.offset + PAGE_SIZE)} of
              {items.total}
            </span>
            <button
              type="button"
              class="ghost"
              disabled={items.offset + PAGE_SIZE >= items.total || items.loading}
              onclick={() => store.loadItems(run.runId, items.offset + PAGE_SIZE)}
            >
              Next
            </button>
          </div>
        {/if}
      {/if}
    </div>
  {/if}

  {#if run.jobs.length}
    <div class="sub">
      <h4>Jobs</h4>
      {#if progress !== null}
        <progress max="100" value={progress} aria-label="Overall progress">{progress}%</progress>
        <span class="muted">{progress}%</span>
      {/if}
      <ul class="rows">
        {#each run.jobs.slice(0, JOB_CAP) as j (j.jobId)}
          <li>
            <span class="mono">{j.jobId}</span>
            <span class="job {j.state}">{j.state}</span>
            {#if j.total}<span class="muted">{j.done}/{j.total}</span>{/if}
            {#if j.message}<span class="muted">{j.message}</span>{/if}
          </li>
        {/each}
      </ul>
      {#if run.jobs.length > JOB_CAP}<p class="muted">
          +{run.jobs.length - JOB_CAP} more jobs
        </p>{/if}
    </div>
  {/if}

  {#if run.receipt}
    <div class="sub">
      <h4>Result</h4>
      <p>{run.receipt.completed} completed · {run.receipt.failed} failed</p>
      {#if run.receipt.errors.length}
        <ul class="rows" aria-label="Errors">
          {#each run.receipt.errors as e, i (i)}
            <li>
              {#if e.itemRef}<span class="mono">{e.itemRef}</span>{/if}
              {e.message}
            </li>
          {/each}
        </ul>
      {/if}
    </div>
  {/if}

  {#if settlement}
    <p class="settle {settlement.tone}" data-testid="settlement">{settlement.text}</p>
  {/if}

  {#if run.state === 'unknownOutcome' && store.canCheckResult}
    <div class="actions">
      <button
        type="button"
        class="primary"
        data-testid="check-result"
        disabled={run.checking}
        aria-busy={run.checking}
        onclick={() => void store.checkResult(run.runId)}
      >
        {run.checking ? 'Checking…' : 'Check result'}
      </button>
    </div>
  {/if}
  {#if run.checkNote}
    <p class="muted" role="status" data-testid="check-note">{run.checkNote}</p>
  {/if}

  {#if cancellable}
    <div class="actions">
      {#if isDelete && !active}
        <!-- Cancel comes first so keyboard focus never lands on the destructive button by default. -->
        <button
          type="button"
          class="ghost"
          disabled={run.cancelRequested}
          onclick={() => store.cancel(run.runId)}
        >
          Cancel
        </button>
      {/if}
      {#if run.state === 'awaitingApproval'}
        <button
          type="button"
          class={isDelete ? 'danger' : 'primary'}
          data-testid={isDelete ? 'confirm-delete' : undefined}
          disabled={!run.canApprove || (isDelete && !namesShown)}
          onclick={() => store.approve(run.runId)}
        >
          {isDelete ? 'Pindahkan ke Recycle Bin' : 'Run'}
        </button>
      {/if}
      {#if editable && !isDelete}
        <button type="button" class="ghost" onclick={() => store.edit(run.runId)}>Edit</button>
      {/if}
      {#if !isDelete || active}
        <button
          type="button"
          class="ghost"
          disabled={run.cancelRequested}
          onclick={() => store.cancel(run.runId)}
        >
          {active ? 'Stop' : 'Cancel'}
        </button>
      {/if}
    </div>
  {/if}
</section>

<style>
  .rev {
    margin-left: 6px;
    font-size: 11px;
    font-weight: normal;
    color: var(--text3);
    border: 1px solid var(--line2);
    border-radius: var(--r);
    padding: 0 6px;
  }
  .card {
    border: 1px solid var(--line2);
    border-left-width: 3px;
    border-radius: var(--rl);
    background: var(--elev);
    padding: 8px 10px;
    display: flex;
    flex-direction: column;
    gap: 6px;
    min-width: 0;
  }
  .card.good {
    border-left-color: var(--green);
  }
  .card.warn {
    border-left-color: var(--amber);
  }
  .card.bad {
    border-left-color: var(--red);
  }
  .card.info {
    border-left-color: var(--accent);
  }
  .status {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--accentInk);
  }
  .good .dot {
    background: var(--green);
  }
  .warn .dot {
    background: var(--amber);
  }
  .bad .dot {
    background: var(--red);
  }
  h3,
  h4 {
    margin: 0;
    font-size: 12px;
  }
  .kv {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: 2px 10px;
    margin: 0;
  }
  .kv dt {
    color: var(--text3);
  }
  .kv dd {
    margin: 0;
    overflow-wrap: anywhere;
    min-width: 0;
  }
  .muted {
    color: var(--text3);
  }
  .tag {
    color: var(--text3);
    font-size: 11px;
    text-transform: uppercase;
  }
  .note {
    background: var(--kbd);
    border-radius: var(--r);
    padding: 6px 8px;
  }
  .blocked {
    color: var(--amber);
    margin: 0;
  }
  .error {
    color: var(--red);
    margin: 0;
    overflow-wrap: anywhere;
  }
  .sub {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .settle {
    margin: 0;
    color: var(--text3);
  }
  .settle.warn {
    color: var(--amber);
  }
  .settle.bad {
    color: var(--red);
  }
  .rows {
    list-style: none;
    margin: 0;
    padding: 0;
    max-height: 220px;
    overflow: auto;
  }
  .rows li {
    overflow-wrap: anywhere;
    padding: 1px 0;
  }
  .pager {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .job.failed {
    color: var(--red);
  }
  .job.completed {
    color: var(--green);
  }
  .job.cancelled {
    color: var(--amber);
  }
  progress {
    width: 100%;
    accent-color: var(--accent);
  }
  .actions {
    display: flex;
    gap: 6px;
    flex-wrap: wrap;
  }
  button {
    cursor: pointer;
    border-radius: var(--r);
    padding: 4px 10px;
    color: var(--text);
  }
  button:focus-visible {
    outline: 2px solid var(--accentInk);
    outline-offset: 1px;
  }
  button:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .ghost {
    background: transparent;
    border: 1px solid var(--line2);
  }
  .ghost:hover:not(:disabled) {
    background: var(--hover2);
  }
  .deleteWarn {
    border: 1px solid var(--red);
    border-radius: var(--r);
    background: color-mix(in srgb, var(--red) 10%, transparent);
    padding: 6px 8px;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .deleteWarn p {
    margin: 0;
  }
  .danger {
    background: var(--red);
    border: 1px solid var(--red);
    color: #fff;
  }
  .primary {
    background: var(--accent);
    border: 1px solid var(--accent);
    color: #fff;
  }
</style>
