<script lang="ts">
  /**
   * Compare panel (F2-18): directory-compare report between the pane roots
   * and compare-by-content verdicts. Strictly read-only — the actions that
   * consume results live in the Mark menu (Mark Newer / Hide Same).
   */
  import { compareStore } from '../stores/compareStore.svelte';
  import { formatSize } from '../format';
  import { compareRun } from '../stores/compareRun.svelte';
  import type { CompareEncoding, DirDiffState } from '../ipc/compare';
  import { dialog } from '../dialogKit';

  const store = compareStore;

  const encodings: CompareEncoding[] = ['auto', 'utf-8', 'utf-16le', 'utf-16be', 'latin1'];

  function rerunFile(enc: CompareEncoding) {
    const f = store.fileResult;
    if (f) void store.compareContents(f.leftPath, f.rightPath, enc);
    else store.encoding = enc;
  }

  // FER-577 (B3): docked panel. The cursor starts on "Compare Directories"; Esc cancels a running
  // compare (read-only, harmless), otherwise closes the panel; focus returns to the opener.
  const panelOpts = {
    modal: false,
    mnemonics: false,
    initial: '[data-autofocus]',
    onEscape: () => {
      if (store.busy) void store.cancelRun();
      else store.closePanel();
    },
  };

  const progressPct = $derived.by(() => {
    const p = compareRun.progress;
    return p && p.total ? Math.min(100, Math.round((p.done / p.total) * 100)) : null;
  });

  const stateLabel: Record<DirDiffState, string> = {
    leftOnly: 'left only',
    rightOnly: 'right only',
    same: 'same',
    newerLeft: 'newer left',
    newerRight: 'newer right',
    sizeDifferent: 'size differs',
    contentDifferent: 'content differs',
  };
</script>

{#if store.panelOpen}
  <aside class="panel" aria-label="Compare" tabindex="-1" use:dialog={panelOpts}>
    <div class="headRow">
      <h2>Compare</h2>
      <button class="close" aria-label="Close compare panel" onclick={() => store.closePanel()}>
        ✕
      </button>
    </div>

    {#if store.fileResult}
      <section class="card">
        <h3>Compare By Content</h3>
        <p class="mono paths">{store.fileResult.leftPath}</p>
        <p class="mono paths">{store.fileResult.rightPath}</p>
        {#if store.fileResult.cancelled}
          <p class="verdict warn" role="status">Cancelled — partial, no verdict.</p>
        {:else if store.fileResult.equal}
          <p class="verdict ok">Files are equal.</p>
        {:else}
          <p class="verdict diff">
            Files are different at byte {store.fileResult.firstMismatch ?? '?'}
            {#if store.fileResult.bothText && store.fileResult.leftLine}
              (line {store.fileResult.leftLine} vs line {store.fileResult.rightLine})
            {/if}.
          </p>
        {/if}
        <p class="dim mono">
          {store.fileResult.leftSize} vs {store.fileResult.rightSize} bytes
          {#if store.fileResult.fromCache}· <span class="cache">from cache</span>{/if}
          {#if store.fileResult.textDiff}· encoding {store.fileResult.textDiff.encoding}{/if}
        </p>
        {#if store.fileResult.textDiff}
          <div class="diff" aria-label="Text diff">
            {#each store.fileResult.textDiff.hunks as h, i (i)}
              <div class="hunk mono">@@ -{h.leftStart} +{h.rightStart} @@</div>
              {#each h.lines as l, j (j)}
                <div class="dl {l.kind}">
                  <span class="mark">{l.kind === 'add' ? '+' : l.kind === 'del' ? '-' : ' '}</span
                  >{l.text}
                </div>
              {/each}
            {/each}
            {#if store.fileResult.textDiff.truncated}
              <p class="warn" role="status">Diff truncated at the engine limit.</p>
            {/if}
          </div>
        {/if}
      </section>
    {/if}

    <div class="row">
      <button
        type="button"
        data-autofocus
        onclick={() => void store.compareDirectories(store.byContent)}
        disabled={store.busy}
      >
        Compare Directories
      </button>
      <label class="check">
        <input type="checkbox" bind:checked={store.byContent} disabled={store.busy} />
        Bandingkan isi (content)
      </label>
      <label class="check">
        <input type="checkbox" bind:checked={store.recursive} disabled={store.busy} />
        Rekursif
      </label>
      <label class="check">
        Encoding
        <select
          aria-label="Encoding"
          value={store.encoding}
          disabled={store.busy}
          onchange={(e) =>
            rerunFile((e.currentTarget as HTMLSelectElement).value as CompareEncoding)}
        >
          {#each encodings as enc (enc)}
            <option value={enc}>{enc}</option>
          {/each}
        </select>
      </label>
      <button type="button" onclick={() => void store.clearCache()} disabled={store.busy}>
        Clear cache
      </button>
      {#if store.report}
        <button type="button" onclick={() => store.toggleHideSame()}>
          {store.hiddenSameNames ? 'Show Same Files' : 'Hide Same Files'}
        </button>
      {/if}
    </div>

    <p class="dim policy">
      Waktu dianggap sama bila selisih ≤ 2 detik (toleransi sistem file); ukuran dibandingkan lebih
      dulu.{#if store.cacheCleared}
        Cache dibersihkan.{/if}
    </p>

    {#if store.busy}
      <p class="dim" role="status">
        Comparing…
        {#if compareRun.progress}
          {compareRun.progress.phase}: {compareRun.progress
            .done}{#if compareRun.progress.total}/{compareRun.progress.total}{/if}
          {#if compareRun.progress.current}<span class="mono">{compareRun.progress.current}</span
            >{/if}
        {/if}
      </p>
      <progress aria-label="Compare progress" max="100" value={progressPct ?? undefined}></progress>
      <button type="button" class="cancel" onclick={() => void store.cancelRun()}>Cancel</button>
    {:else if store.errorText}
      <p class="error" role="alert">{store.errorText}</p>
    {:else if store.report}
      <p class="dim mono">
        {store.report.leftRoot} ⇄ {store.report.rightRoot} · {store.report.entries.length} entries
        {#if store.report.recursive}· recursive{/if}
        {#if store.report.truncated}· truncated{/if}
      </p>
      {#if store.report.cancelled}
        <p class="warn" role="status">
          Cancelled — partial result (not used by Mark Newer / Hide Same / Sync).
        </p>
      {/if}
      {#if store.report.truncated}
        <p class="warn" role="status">Truncated — the list is incomplete (engine limit).</p>
      {/if}
      {#if store.report.errors?.length}
        <div class="warn" role="alert">
          <p>{store.report.errors.length} entries could not be read:</p>
          <ul class="errs">
            {#each store.report.errors as er (er.name)}
              <li class="mono">{er.name} — {er.message}</li>
            {/each}
          </ul>
        </div>
      {/if}
      <ul class="results" aria-label="Directory comparison">
        {#each store.report.entries as e (e.name)}
          <li class="entry {e.state}">
            <span class="name" title={e.name}>{e.name}{e.isDir ? '/' : ''}</span>
            <span class="state">{stateLabel[e.state]}</span>
            <span class="size mono">
              {#if e.leftSize || e.rightSize}
                {formatSize(e.leftSize)} / {formatSize(e.rightSize)}
              {/if}
            </span>
          </li>
        {:else}
          <li class="dim">The two folders have identical contents.</li>
        {/each}
      </ul>
    {:else}
      <p class="dim">
        Compare the two pane roots, or use Files → Compare By Content with two files selected.
      </p>
    {/if}
  </aside>
{/if}

<style>
  .panel {
    width: 340px;
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
  h2,
  h3 {
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
  .card {
    border: 1px solid var(--line);
    border-radius: var(--r);
    padding: 8px;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .paths {
    margin: 0;
    font-size: calc(10.5px + var(--uiStep, 0px));
    color: var(--text3);
    word-break: break-all;
  }
  .mono {
    font-family: 'Geist Mono', monospace;
  }
  .verdict {
    margin: 0;
    font-size: calc(12.5px + var(--uiStep, 0px));
  }
  .verdict.ok {
    color: var(--green, var(--text2));
  }
  .verdict.diff {
    color: var(--red);
  }
  .dim {
    margin: 0;
    font-size: calc(11.5px + var(--uiStep, 0px));
    color: var(--text3);
  }
  .error {
    margin: 0;
    font-size: calc(11.5px + var(--uiStep, 0px));
    color: var(--red);
    word-break: break-word;
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
  button:disabled {
    opacity: 0.5;
  }
  .warn {
    margin: 0;
    font-size: 11.5px;
    color: var(--accentInk, var(--red));
  }
  .errs {
    margin: 2px 0 0;
    padding-left: 14px;
    font-size: 10.5px;
    max-height: 70px;
    overflow: auto;
  }
  .diff {
    max-height: 200px;
    overflow: auto;
    font-family: 'Geist Mono', monospace;
    font-size: 10.5px;
  }
  .hunk {
    color: var(--text3);
    margin-top: 4px;
  }
  .dl {
    white-space: pre-wrap;
    word-break: break-all;
  }
  .dl.add {
    color: var(--green, var(--text));
    background: var(--hover);
  }
  .dl.del {
    color: var(--red);
    background: var(--hover);
  }
  .mark {
    display: inline-block;
    width: 1.2em;
  }
  progress {
    width: 100%;
    height: 6px;
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
  .entry {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto auto;
    gap: 8px;
    align-items: baseline;
    padding: 3px 8px;
    border-radius: var(--r);
    font-size: calc(12px + var(--uiStep, 0px));
  }
  .entry:nth-child(odd) {
    background: var(--hover);
  }
  .entry.same .name {
    color: var(--text3);
  }
  .name {
    min-width: 0;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .state {
    font-size: calc(10.5px + var(--uiStep, 0px));
    color: var(--text2);
    white-space: nowrap;
  }
  .entry.newerLeft .state,
  .entry.newerRight .state,
  .entry.contentDifferent .state {
    color: var(--accentInk);
    font-weight: 600;
  }
  .size {
    font-size: calc(10.5px + var(--uiStep, 0px));
    color: var(--text3);
    white-space: nowrap;
  }
</style>
