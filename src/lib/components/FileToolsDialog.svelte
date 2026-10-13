<script lang="ts">
  import { focusModal } from '../focusModal';
  import { fileTools, RAR_CHECKSUM_UNSUPPORTED } from '../stores/fileToolsStore.svelte';
  import { formatSize } from '../format';
  import type { Algorithm, OutputPolicy } from '../ipc/fileTools';

  const d = $derived(fileTools.current);

  const algorithms: { value: Algorithm; label: string }[] = [
    { value: 'sha256', label: 'SHA-256' },
    { value: 'sha1', label: 'SHA-1 (legacy verify)' },
    { value: 'md5', label: 'MD5 (legacy verify)' },
    { value: 'crc32', label: 'CRC32 (legacy verify)' },
  ];

  const policies: { value: OutputPolicy; label: string }[] = [
    { value: 'refuse', label: 'Refuse (keep existing)' },
    { value: 'skip', label: 'Skip existing' },
    { value: 'overwrite', label: 'Overwrite' },
    { value: 'overwriteOlder', label: 'Overwrite if older' },
    { value: 'rename', label: 'Keep both (rename)' },
  ];
  // Sidecar and part names are fixed, so only the combined file can be renamed.
  const policyChoices = $derived(
    d?.kind === 'combine' ? policies : policies.filter((p) => p.value !== 'rename'),
  );

  const statusLabel: Record<string, string> = {
    ok: 'OK',
    mismatch: 'MISMATCH',
    missing: 'missing',
    noManifest: 'no manifest',
    failed: 'failed',
    skipped: 'skipped',
  };

  function short(path: string): string {
    return path.replace(/^.*[\\/]/, '');
  }

  const isRar = $derived(d?.kind === 'archiveChecksum' && /\.rar$/i.test(d.archivePath));

  function onKey(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.preventDefault();
      fileTools.close();
    }
  }
</script>

{#if d}
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="scrim" onmousedown={() => fileTools.close()}></div>
  <div
    class="modal"
    role="dialog"
    aria-modal="true"
    aria-label={d.kind === 'checksumCreate'
      ? 'Create Checksum File(s)'
      : d.kind === 'checksumVerify'
        ? 'Verify Checksums'
        : d.kind === 'archiveChecksum'
          ? 'Checksum of Archive Member'
          : d.kind === 'split'
            ? 'Split File'
            : 'Combine Files'}
    tabindex="-1"
    use:focusModal
    onkeydown={onKey}
  >
    {#if d.kind === 'checksumCreate'}
      <h2>Create Checksum File(s)</h2>
      <p class="dim">
        One sidecar manifest per file, next to it. An existing manifest is kept unless you choose
        otherwise below.
      </p>
      <div class="algorithms" role="radiogroup" aria-label="Checksum algorithm">
        {#each algorithms as a (a.value)}
          <label class="check">
            <input
              type="radio"
              name="algorithm"
              value={a.value}
              bind:group={fileTools.algorithm}
              disabled={fileTools.busy}
            />
            {a.label}
          </label>
        {/each}
      </div>
      <ul class="list">
        {#each d.paths.slice(0, 5) as p (p)}
          <li class="mono">{short(p)}</li>
        {/each}
        {#if d.paths.length > 5}<li class="dim">… and {d.paths.length - 5} more</li>{/if}
      </ul>
    {:else if d.kind === 'checksumVerify'}
      <h2>Verify Checksums</h2>
      {#if fileTools.verifying}
        <p class="dim">Hashing {d.paths.length} item{d.paths.length === 1 ? '' : 's'}…</p>
        {#if fileTools.progress}
          <p class="dim mono" data-testid="checksum-progress">
            {fileTools.progress.filesDone}/{fileTools.progress.filesTotal} · {formatSize(
              fileTools.progress.processedBytes,
            )} · {short(fileTools.progress.current)}
          </p>
        {/if}
      {:else if fileTools.verify}
        {#if fileTools.verify.cancelled}
          <p class="dim">Cancelled — only the items finished before the cancel are listed.</p>
        {/if}
        <p>
          <span class="mono ok">{fileTools.verify.ok}</span> OK ·
          <span class="mono bad">{fileTools.verify.mismatched}</span> mismatched ·
          <span class="mono"
            >{fileTools.verify.outcomes.length -
              fileTools.verify.ok -
              fileTools.verify.mismatched}</span
          >
          other
        </p>
        <ul class="list">
          {#each fileTools.verify.outcomes as o (o.path)}
            <li>
              <span
                class="mono {o.status === 'ok' ? 'ok' : o.status === 'mismatch' ? 'bad' : 'dim'}"
                >{statusLabel[o.status] ?? o.status}</span
              >
              <span class="mono">{short(o.path)}</span>
              {#if o.message}<span class="dim">{o.message}</span>{/if}
            </li>
          {/each}
        </ul>
      {/if}
      {#if fileTools.error}<p class="error" role="alert">{fileTools.error}</p>{/if}
      <div class="actions">
        {#if fileTools.verifying}
          <button class="ghost" onclick={() => void fileTools.cancelVerify()}>Cancel</button>
        {/if}
        <button class="ghost" onclick={() => fileTools.close()} disabled={fileTools.verifying}>
          Close
        </button>
      </div>
    {:else if d.kind === 'archiveChecksum'}
      <h2>Checksum of Archive Member</h2>
      <p class="mono path">{d.archivePath}</p>
      <p class="mono path">{d.memberPath}</p>
      {#if isRar}
        <p class="error" role="alert">{RAR_CHECKSUM_UNSUPPORTED}</p>
      {:else}
        <p class="dim">The member is streamed through the archive reader; nothing is extracted.</p>
        <div class="algorithms" role="radiogroup" aria-label="Checksum algorithm">
          {#each algorithms as a (a.value)}
            <label class="check">
              <input
                type="radio"
                name="member-algorithm"
                value={a.value}
                bind:group={fileTools.algorithm}
                disabled={fileTools.hashing}
              />
              {a.label.replace(' (legacy verify)', '')}
            </label>
          {/each}
        </div>
        <label class="field wide">
          <span>Password (if the archive is encrypted)</span>
          <input
            type="password"
            autocomplete="off"
            bind:value={fileTools.password}
            disabled={fileTools.hashing}
          />
        </label>
        {#if fileTools.hashing}
          <p class="dim mono" data-testid="checksum-progress">
            Hashing… {formatSize(fileTools.progress?.processedBytes ?? 0)}
          </p>
        {/if}
        {#if fileTools.memberHash}
          <p>
            <span class="mono">{fileTools.memberHash.algorithm.toUpperCase()}</span>
            <span class="mono ok path" data-testid="member-hash">{fileTools.memberHash.hash}</span>
          </p>
        {/if}
        {#if fileTools.memberCancelled}<p class="dim">Cancelled.</p>{/if}
      {/if}
      {#if fileTools.error && !isRar}<p class="error" role="alert">{fileTools.error}</p>{/if}
      <div class="actions">
        {#if fileTools.hashing}
          <button class="ghost" onclick={() => void fileTools.cancelVerify()}>Cancel</button>
        {/if}
        <button class="ghost" onclick={() => fileTools.close()} disabled={fileTools.hashing}>
          Close
        </button>
        <button
          class="primary"
          onclick={() => void fileTools.runArchiveHash()}
          disabled={fileTools.hashing || isRar}
        >
          Compute
        </button>
      </div>
    {:else if d.kind === 'split'}
      <h2>Split File</h2>
      <p class="mono path">{d.path}</p>
      <label class="field">
        <span>Part size (MB)</span>
        <input type="number" min="1" bind:value={fileTools.partSizeMb} disabled={fileTools.busy} />
      </label>
      <p class="dim">Parts land next to the source as .001, .002, … plus a .parts.json manifest.</p>
    {:else if d.kind === 'combine'}
      <h2>Combine Files</h2>
      <p class="mono path">{d.firstPart}</p>
      <p class="dim">
        All .NNN parts are validated (order, gaps, manifest sizes and SHA-256) before anything is
        written; the output appears only after a clean recombine.
      </p>
    {/if}
    {#if d.kind === 'checksumCreate' || d.kind === 'split' || d.kind === 'combine'}
      <label class="field wide">
        <span>If the output already exists</span>
        <select bind:value={fileTools.conflictPolicy} disabled={fileTools.busy}>
          {#each policyChoices as p (p.value)}
            <option value={p.value}>{p.label}</option>
          {/each}
        </select>
      </label>
    {/if}
    {#if d.kind !== 'checksumVerify' && d.kind !== 'archiveChecksum' && fileTools.error}
      <p class="error" role="alert">{fileTools.error}</p>
    {/if}
    {#if d.kind !== 'checksumVerify' && d.kind !== 'archiveChecksum' && fileTools.enqueued}
      <p class="dim">Queued — progress and cancel live in the operations panel.</p>
    {/if}
    {#if d.kind !== 'checksumVerify' && d.kind !== 'archiveChecksum'}
      <div class="actions">
        <button class="ghost" onclick={() => fileTools.close()} disabled={fileTools.busy}>
          Close
        </button>
        <button class="primary" onclick={() => void fileTools.submit()} disabled={fileTools.busy}>
          {d.kind === 'checksumCreate' ? 'Create' : d.kind === 'split' ? 'Split' : 'Combine'}
        </button>
      </div>
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
    top: 22%;
    transform: translateX(-50%);
    width: min(500px, 94vw);
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
  .dim {
    color: var(--text3);
    font-size: calc(11.5px + var(--uiStep, 0px));
  }
  .mono {
    font-family: 'Geist Mono', monospace;
    font-size: calc(11.5px + var(--uiStep, 0px));
  }
  .path {
    font-size: calc(11px + var(--uiStep, 0px));
    color: var(--text3);
    word-break: break-all;
  }
  .ok {
    color: var(--green, #3fae6a);
  }
  .bad {
    color: var(--red);
  }
  .list {
    margin: 0;
    padding: 0;
    list-style: none;
    max-height: 150px;
    overflow: auto;
    font-size: calc(11.5px + var(--uiStep, 0px));
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .list li {
    display: flex;
    gap: 10px;
    align-items: baseline;
  }
  .algorithms {
    display: flex;
    flex-wrap: wrap;
    gap: 6px 16px;
  }
  .check {
    display: flex;
    gap: 6px;
    align-items: center;
    font-size: calc(12px + var(--uiStep, 0px));
    color: var(--text2);
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 3px;
    font-size: calc(11px + var(--uiStep, 0px));
    color: var(--text3);
    max-width: 160px;
  }
  .field.wide {
    max-width: none;
  }
  select,
  input[type='number'],
  input[type='password'] {
    height: 28px;
    padding: 0 8px;
    border: 0;
    outline: 0;
    border-radius: var(--r);
    background: var(--bg);
    box-shadow: 0 0 0 1px var(--line2);
    color: var(--text);
    font-size: calc(12.5px + var(--uiStep, 0px));
  }
  select {
    padding: 0 6px;
  }
  .error {
    color: var(--red);
    font-size: calc(12px + var(--uiStep, 0px));
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
