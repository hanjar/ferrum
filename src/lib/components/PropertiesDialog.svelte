<script lang="ts">
  import { focusModal } from '../focusModal';
  import { propertiesStore as store, systemInfo } from '../stores/propertiesStore.svelte';
  import { formatSize } from '../format';

  const d = $derived(store.current);
  const p = $derived(store.properties);
  const sys = $derived(d?.kind === 'system' ? systemInfo() : null);

  const notSupported = $derived(store.errorCode === 'unsupported');
  const choice = $derived(store.attributeChoice);

  /** `rw-r-----` style string plus the octal bits, e.g. `rw-r----- (0640)`. */
  function fmtMode(mode: number): string {
    const bits = 'rwxrwxrwx';
    let out = '';
    for (let i = 0; i < 9; i++) out += mode & (1 << (8 - i)) ? bits[i] : '-';
    return `${out} (${(mode & 0o7777).toString(8).padStart(4, '0')})`;
  }

  function fmtFlags(e: { readOnly: boolean; hidden: boolean; system: boolean }): string {
    const flags = [e.readOnly && 'Read-only', e.hidden && 'Hidden', e.system && 'System'].filter(
      Boolean,
    );
    return flags.length > 0 ? flags.join(', ') : 'None';
  }

  const kindLabel: Record<string, string> = {
    file: 'File',
    dir: 'Folder',
    symlink: 'Symlink',
    other: 'Special',
  };

  function fmtDate(ms: number): string {
    if (!ms) return '—';
    const date = new Date(ms);
    const pad = (n: number) => String(n).padStart(2, '0');
    return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`;
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.preventDefault();
      store.close();
    }
  }
</script>

{#if d}
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="scrim" onmousedown={() => store.close()}></div>
  <div
    class="modal"
    role="dialog"
    aria-modal="true"
    aria-label={d.kind === 'properties'
      ? 'Properties'
      : d.kind === 'attributes'
        ? 'Change Attributes'
        : d.kind === 'occupied'
          ? 'Calculate Occupied Space'
          : d.kind === 'volume'
            ? 'Volume Label'
            : 'System Information'}
    tabindex="-1"
    use:focusModal
    onkeydown={onKey}
  >
    {#if d.kind === 'properties'}
      <h2>Properties</h2>
      {#if store.loading}
        <p class="dim">Reading…</p>
      {:else if p}
        <p class="mono path">{p.path}</p>
        <dl class="grid">
          <dt>Type</dt>
          <dd>{kindLabel[p.kind] ?? p.kind}{p.symlinkTarget ? ` → ${p.symlinkTarget}` : ''}</dd>
          <dt>Size</dt>
          <dd>
            {p.kind === 'dir' ? '—' : `${formatSize(p.size)} (${p.size.toLocaleString()} bytes)`}
          </dd>
          <dt>On disk</dt>
          <dd>
            {#if p.kind !== 'dir' && p.allocatedSupported}
              {formatSize(p.allocatedBytes)} ({p.allocatedBytes.toLocaleString()} bytes)
            {:else}
              {formatSize(p.size)}<span class="dim">
                (logical — allocated size not reported by this OS)</span
              >
            {/if}
          </dd>
          <dt>Created</dt>
          <dd>{fmtDate(p.createdMs)}</dd>
          <dt>Modified</dt>
          <dd>{fmtDate(p.modifiedMs)}</dd>
          <dt>Accessed</dt>
          <dd>{fmtDate(p.accessedMs)}</dd>
          <dt>Attributes</dt>
          <dd>{fmtFlags(p)}</dd>
          <dt>Permissions</dt>
          <dd>
            {#if p.mode !== undefined}
              <span class="mono">{fmtMode(p.mode)}</span>
            {:else}
              <span class="dim">Not reported on this platform</span>
            {/if}
          </dd>
        </dl>
      {/if}
      <div class="actions">
        <button class="ghost" onclick={() => store.close()}>Close</button>
      </div>
    {:else if d.kind === 'attributes'}
      <h2>Change Attributes</h2>
      {#if store.loading}
        <p class="dim">Reading…</p>
      {:else if store.attributeItems.length > 0}
        {#if !store.attributesSupported}
          <div class="notice" role="status">
            <strong>Not supported on this platform</strong>
            <p>
              Ferrum can only write the read-only and hidden flags, and only on Windows. POSIX
              permission bits and timestamps cannot be changed from this dialog.
            </p>
          </div>
        {/if}
        {#if store.attributeStage === 'edit'}
          <p class="dim">
            {store.attributeItems.length === 1 ? '1 item' : `${store.attributeItems.length} items`} selected.
            A mixed checkbox leaves each item as it is.
          </p>
          <div class="attrs">
            <label class="check">
              <input
                type="checkbox"
                checked={choice.readOnly === true}
                indeterminate={choice.readOnly === null}
                disabled={store.busy || !store.attributesSupported}
                onchange={(e) => (store.attributeChoice.readOnly = e.currentTarget.checked)}
              />
              Read-only
            </label>
            <label class="check">
              <input
                type="checkbox"
                checked={choice.hidden === true}
                indeterminate={choice.hidden === null}
                disabled={store.busy || !store.attributesSupported}
                onchange={(e) => (store.attributeChoice.hidden = e.currentTarget.checked)}
              />
              Hidden
            </label>
          </div>
          {#if store.attributesApplied > 0 && store.attributeErrors.length === 0}
            <p class="dim" role="status">
              Applied to {store.attributesApplied}
              {store.attributesApplied === 1 ? 'item' : 'items'}.
            </p>
          {/if}
        {:else}
          <p class="dim">Review what will change:</p>
          <ul class="plan">
            {#each store.attributePlan as row (row.path)}
              <li>
                <span class="mono path">{row.path}</span>
                {#if row.error}
                  <span class="error">{row.error} — skipped</span>
                {:else if row.changes.length === 0}
                  <span class="dim">No change</span>
                {:else}
                  <span>{row.changes.join('; ')}</span>
                {/if}
              </li>
            {/each}
          </ul>
        {/if}
      {/if}
      {#if store.attributeErrors.length > 0}
        <ul class="plan" role="alert">
          {#each store.attributeErrors as failure (failure)}
            <li class="error">{failure}</li>
          {/each}
        </ul>
      {/if}
      <div class="actions">
        {#if store.attributeStage === 'preview'}
          <button class="ghost" onclick={() => store.backToEdit()} disabled={store.busy}
            >Back</button
          >
          <button
            class="primary"
            onclick={() => void store.applyAttributes()}
            disabled={store.busy || !store.attributesSupported || !store.hasAttributeChanges}
          >
            Apply
          </button>
        {:else}
          <button class="ghost" onclick={() => store.close()} disabled={store.busy}>Close</button>
          <button
            class="primary"
            onclick={() => store.reviewAttributes()}
            disabled={store.busy || !store.attributesSupported || !store.hasAttributeChanges}
          >
            Review changes
          </button>
        {/if}
      </div>
    {:else if d.kind === 'occupied'}
      <h2>Calculate Occupied Space</h2>
      {#if store.loading}
        <p class="dim" role="status">
          {store.cancelling ? 'Cancelling' : 'Calculating'}
          {d.paths.length === 1 ? '1 item' : `${d.paths.length} items`}…
        </p>
        {#if store.progress}
          <p class="mono dim">
            {store.progress.files.toLocaleString()} files, {store.progress.dirs.toLocaleString()} folders,
            {formatSize(store.progress.logicalBytes)} so far
          </p>
        {/if}
      {:else if store.occupied}
        {#if store.occupied.cancelled}
          <div class="notice" role="status">
            <strong>Cancelled</strong>
            <p>The totals below are partial: the calculation was stopped before it finished.</p>
          </div>
        {/if}
        <dl class="grid">
          <dt>Files</dt>
          <dd class="mono">{store.occupied.files.toLocaleString()}</dd>
          <dt>Folders</dt>
          <dd class="mono">{store.occupied.dirs.toLocaleString()}</dd>
          <dt>Symlinks (not followed)</dt>
          <dd class="mono">{store.occupied.symlinks.toLocaleString()}</dd>
          <dt>Size</dt>
          <dd>
            {formatSize(store.occupied.logicalBytes)} ({store.occupied.logicalBytes.toLocaleString()}
            bytes)
          </dd>
          <dt>Occupied on disk</dt>
          <dd>
            {#if store.occupied.allocatedSupported}
              {formatSize(store.occupied.allocatedBytes)} ({store.occupied.allocatedBytes.toLocaleString()}
              bytes)
            {:else}
              {formatSize(store.occupied.logicalBytes)}<span class="dim">
                (logical — allocated size not reported by this OS)</span
              >
            {/if}
          </dd>
        </dl>
        {#if store.occupied.failed.length > 0}
          <p class="error">
            {store.occupied.failed.length} entr{store.occupied.failed.length === 1 ? 'y' : 'ies'} could
            not be read:
            {store.occupied.failed[0].path}
            {#if store.occupied.failed.length > 1}<span class="dim">
                and {store.occupied.failed.length - 1} more</span
              >{/if}
          </p>
        {/if}
      {/if}
      <div class="actions">
        {#if store.loading}
          <button class="ghost" onclick={() => store.cancelOccupied()} disabled={store.cancelling}>
            Cancel
          </button>
        {:else}
          <button class="ghost" onclick={() => store.close()}>Close</button>
        {/if}
      </div>
    {:else if d.kind === 'volume'}
      <h2>Volume Label</h2>
      {#if store.loading}
        <p class="dim">Reading volume…</p>
      {:else if store.volume}
        <dl class="grid">
          <dt>Drive</dt>
          <dd class="mono">{store.volume.root}</dd>
          <dt>Label</dt>
          <dd>{store.volume.label === '' ? '(no label)' : store.volume.label}</dd>
          <dt>Filesystem</dt>
          <dd>{store.volume.fsName || '—'}</dd>
          <dt>Capacity</dt>
          <dd>{formatSize(store.volume.totalBytes)}</dd>
          <dt>Free</dt>
          <dd>{formatSize(store.volume.freeBytes)}</dd>
        </dl>
        <p class="dim">Renaming the volume label is not supported yet.</p>
      {/if}
      <div class="actions">
        <button class="ghost" onclick={() => store.close()}>Close</button>
      </div>
    {:else if d.kind === 'system' && sys}
      <h2>System Information</h2>
      <dl class="grid">
        {#if store.host}
          <dt>Operating system</dt>
          <dd>{store.host.os} ({store.host.family})</dd>
          <dt>Architecture</dt>
          <dd>{store.host.arch}</dd>
          {#if store.host.hostName}
            <dt>Computer name</dt>
            <dd>{store.host.hostName}</dd>
          {/if}
        {/if}
        <dt>Platform</dt>
        <dd>{sys.platform || '—'}</dd>
        <dt>Language</dt>
        <dd>{sys.language}</dd>
        <dt>CPU threads</dt>
        <dd class="mono">{sys.cpuThreads}</dd>
        {#if sys.memoryGB !== null}
          <dt>Device memory</dt>
          <dd class="mono">{sys.memoryGB} GB</dd>
        {/if}
        <dt>Screen</dt>
        <dd class="mono">{sys.screen || '—'}</dd>
        <dt>User agent</dt>
        <dd class="mono ua">{sys.userAgent}</dd>
      </dl>
      <p class="dim">The operating-system version is not reported (no dependency-free API).</p>
      <div class="actions">
        <button class="ghost" onclick={() => store.close()}>Close</button>
      </div>
    {/if}
    {#if store.error && notSupported}
      <div class="notice" role="status">
        <strong>Not supported on this platform</strong>
        <p>{store.error}</p>
      </div>
    {:else if store.error}
      <p class="error" role="alert">{store.error}</p>
    {/if}
  </div>
{/if}

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
    top: 20%;
    transform: translateX(-50%);
    width: min(520px, 94vw);
    padding: 18px;
    border-radius: calc(var(--rl) + 2px);
    background: var(--elev);
    box-shadow: var(--shadow);
    z-index: 61;
    display: flex;
    flex-direction: column;
    gap: 10px;
    animation: fmUp 160ms cubic-bezier(0.2, 0.7, 0.2, 1);
  }
  h2 {
    margin: 0;
    font-size: calc(14px + var(--uiStep, 0px));
    font-weight: 600;
  }
  p {
    margin: 0;
    font-size: calc(12.5px + var(--uiStep, 0px));
    color: var(--text2);
  }
  .path {
    font-size: calc(11px + var(--uiStep, 0px));
    color: var(--text3);
    word-break: break-all;
  }
  .mono {
    font-family: 'Geist Mono', monospace;
    font-size: calc(11.5px + var(--uiStep, 0px));
  }
  .grid {
    margin: 0;
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 4px 16px;
    font-size: calc(12.5px + var(--uiStep, 0px));
  }
  dt {
    color: var(--text3);
  }
  dd {
    margin: 0;
    color: var(--text);
    word-break: break-word;
  }
  .ua {
    font-size: calc(10.5px + var(--uiStep, 0px));
    color: var(--text3);
  }
  .attrs {
    display: flex;
    gap: 16px;
  }
  .check {
    display: flex;
    gap: 6px;
    align-items: center;
    font-size: calc(12px + var(--uiStep, 0px));
    color: var(--text2);
  }
  .dim {
    color: var(--text3);
  }
  .notice {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 8px 10px;
    border-radius: var(--r);
    background: var(--hover2);
    font-size: calc(12.5px + var(--uiStep, 0px));
    color: var(--text2);
  }
  .plan {
    margin: 0;
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: 6px;
    max-height: 220px;
    overflow: auto;
    font-size: calc(12px + var(--uiStep, 0px));
  }
  .plan li {
    display: flex;
    flex-direction: column;
  }
  .error {
    color: var(--red);
    font-size: calc(12px + var(--uiStep, 0px));
    word-break: break-word;
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 4px;
  }
  button {
    height: 28px;
    padding: 0 14px;
    border: 0;
    border-radius: var(--r);
    font-size: calc(12.5px + var(--uiStep, 0px));
    cursor: default;
  }
  .ghost {
    background: transparent;
    color: var(--text2);
  }
  .ghost:hover {
    background: var(--hover2);
    color: var(--text);
  }
  .primary {
    background: var(--accent);
    color: #fff;
    font-weight: 600;
  }
  button:disabled {
    opacity: 0.5;
  }
</style>
