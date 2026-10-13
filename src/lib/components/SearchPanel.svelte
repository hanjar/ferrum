<script lang="ts">
  /**
   * Search panel (F2-16, Alt+F7): pattern/mode/size/time/content filters,
   * progressive results, cancel, and saved searches. Results navigate the
   * ACTIVE pane to the containing folder on click.
   */
  import { searchStore } from '../stores/searchStore.svelte';
  import { formatSize } from '../format';
  import { kindOf } from '../kind';
  import FileIcon from './FileIcon.svelte';

  const store = searchStore;

  function numberOrNull(v: string): number | undefined {
    const n = Number(v);
    return Number.isFinite(n) && v.trim() !== '' ? n : undefined;
  }

  function minSizeInput(e: Event) {
    store.options.minSize = numberOrNull((e.target as HTMLInputElement).value);
  }
  function onResultsKey(e: KeyboardEvent) {
    switch (e.key) {
      case 'ArrowDown':
        store.moveCursor(1);
        break;
      case 'ArrowUp':
        store.moveCursor(-1);
        break;
      case 'Home':
        store.moveCursor('home');
        break;
      case 'End':
        store.moveCursor('end');
        break;
      case 'Enter':
        if (!store.revealCursor()) return;
        break;
      case 'Escape':
        if (store.running) void store.cancel();
        else store.togglePanel();
        break;
      default:
        return;
    }
    e.preventDefault();
    queueMicrotask(() =>
      document.getElementById(`search-opt-${store.cursor}`)?.scrollIntoView?.({ block: 'nearest' }),
    );
  }

  function maxSizeInput(e: Event) {
    store.options.maxSize = numberOrNull((e.target as HTMLInputElement).value);
  }
</script>

{#if store.panelOpen}
  <aside class="panel" aria-label="Search">
    <div class="headRow">
      <h2>Search</h2>
      <span class="root mono">{store.root || '—'}</span>
      <button class="close" aria-label="Close search" onclick={() => store.togglePanel()}>✕</button>
    </div>

    <form
      class="filters"
      role="search"
      onsubmit={(e) => {
        e.preventDefault();
        void store.start();
      }}
    >
      <input
        class="pattern"
        bind:value={store.options.pattern}
        placeholder="File name — e.g. report-*.txt"
        aria-label="Search pattern"
        spellcheck="false"
      />
      <div class="row">
        <label>
          <input type="radio" value="glob" bind:group={store.options.mode} /> Mask
        </label>
        <label>
          <input type="radio" value="regex" bind:group={store.options.mode} /> Regex
        </label>
        <label>
          <input type="checkbox" bind:checked={store.options.caseSensitive} /> Match case
        </label>
        <label>
          <input type="checkbox" bind:checked={store.options.recursive} /> Subfolders
        </label>
      </div>
      <div class="row">
        <input
          type="number"
          min="0"
          placeholder="min bytes"
          value={store.options.minSize ?? ''}
          oninput={minSizeInput}
          aria-label="Minimum size in bytes"
        />
        <input
          type="number"
          min="0"
          placeholder="max bytes"
          value={store.options.maxSize ?? ''}
          oninput={maxSizeInput}
          aria-label="Maximum size in bytes"
        />
      </div>
      <input
        class="content"
        bind:value={store.options.content}
        placeholder="Text inside files (small files only)"
        aria-label="Content text"
        spellcheck="false"
      />
      <div class="row buttons">
        <button type="submit" class="primary" disabled={store.running}>Search</button>
        {#if store.running}
          <button type="button" onclick={() => void store.cancel()}>Stop</button>
        {/if}
      </div>
    </form>

    <div class="summary" role="status">
      {#if store.running}
        Searching… {store.results.length} found
      {:else if store.stats}
        {store.results.length} found · {store.stats.visited} scanned
        {#if store.stats.errors > 0}· {store.stats.errors} unreadable{/if}
        {#if store.stats.truncated}· result limit reached{/if}
        {#if store.stats.timedOut}· time limit reached — results may be incomplete{/if}
        {#if store.stats.source === 'index' || store.stats.source === 'indexAndWalker'}<span
            data-testid="search-source"
            title="Dijawab dari indeks FerrumIndexer (FER-476)"
            >· via indeks{store.stats.stale ? ' (sedang dibangun ulang)' : ''}</span
          >{/if}
      {/if}
    </div>
    {#if store.errorText}
      <p class="error" role="alert">{store.errorText}</p>
    {/if}

    <div class="row actions">
      <button
        type="button"
        onclick={() => store.feedToPane()}
        disabled={store.results.length === 0}
        title="Show the results as a list in the active pane">Feed to pane</button
      >
      <button
        type="button"
        onclick={() => void store.copyResults()}
        disabled={store.results.length === 0}
        title="Copy all result paths, one per line">Copy results</button
      >
      {#if store.notice}<span class="notice" role="status">{store.notice}</span>{/if}
    </div>

    <ul
      class="results"
      role="listbox"
      tabindex="0"
      aria-label="Search results"
      aria-activedescendant={store.cursor >= 0 ? `search-opt-${store.cursor}` : undefined}
      onkeydown={onResultsKey}
    >
      {#each store.results as r, i (r.path)}
        <li id={`search-opt-${i}`} role="option" aria-selected={i === store.cursor}>
          <button
            class="result"
            class:cursor={i === store.cursor}
            tabindex="-1"
            onclick={() => {
              store.cursor = i;
              store.openResultFolder(r.path);
            }}
            title={r.path}
          >
            <span class="name"
              ><FileIcon kind={kindOf(r)} name={r.name} size={14} /><span class="nt">{r.name}</span
              ></span
            >
            <span class="dir">{r.path}</span>
            <span class="size">{r.isDir ? 'folder' : formatSize(r.size)}</span>
          </button>
        </li>
      {:else}
        <li class="empty">No results yet.</li>
      {/each}
    </ul>

    <div class="saved">
      <div class="row">
        <input
          bind:value={store.saveName}
          placeholder="Save current search as…"
          aria-label="Saved search name"
          spellcheck="false"
        />
        <button type="button" onclick={() => store.saveCurrent()} disabled={!store.saveName.trim()}>
          Save
        </button>
      </div>
      {#if store.saved.length > 0}
        <ul>
          {#each store.saved as s (s.name)}
            <li class="savedRow">
              <button class="apply" onclick={() => store.applySaved(s)}>{s.name}</button>
              <button
                class="remove"
                aria-label={`Delete saved search ${s.name}`}
                onclick={() => store.removeSaved(s.name)}>✕</button
              >
            </li>
          {/each}
        </ul>
      {/if}
    </div>
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
    gap: 8px;
  }
  h2 {
    margin: 0;
    font-size: calc(13px + var(--uiStep, 0px));
    font-weight: 600;
  }
  .root {
    flex: 1;
    min-width: 0;
    font-size: calc(10.5px + var(--uiStep, 0px));
    color: var(--text3);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .close,
  .remove {
    border: 0;
    background: transparent;
    color: var(--text3);
    cursor: default;
  }
  .close:hover,
  .remove:hover {
    color: var(--text);
  }
  .filters {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  input:not([type]),
  input[type='number'] {
    height: 26px;
    padding: 0 8px;
    border: 0;
    outline: 0;
    border-radius: var(--r);
    background: var(--bg);
    box-shadow: 0 0 0 1px var(--line2);
    color: var(--text);
    font-size: calc(12px + var(--uiStep, 0px));
    min-width: 0;
  }
  input:focus-visible {
    box-shadow: 0 0 0 1.5px var(--accentLine);
  }
  .row {
    display: flex;
    align-items: center;
    gap: 10px;
    font-size: calc(11.5px + var(--uiStep, 0px));
    color: var(--text2);
    flex-wrap: wrap;
  }
  .row label {
    display: flex;
    align-items: center;
    gap: 4px;
    white-space: nowrap;
  }
  .row input[type='number'] {
    width: 90px;
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
  button:disabled {
    opacity: 0.5;
  }
  .summary {
    min-height: 16px;
    font-size: calc(11.5px + var(--uiStep, 0px));
    color: var(--text3);
  }
  .error {
    margin: 0;
    font-size: calc(11.5px + var(--uiStep, 0px));
    color: var(--red);
    word-break: break-word;
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
  .result {
    width: 100%;
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto;
    gap: 0 8px;
    height: auto;
    padding: 4px 8px;
    text-align: left;
    background: transparent;
    border-radius: var(--r);
    font-size: calc(12px + var(--uiStep, 0px));
    color: var(--text);
  }
  .result.cursor {
    background: var(--hover2);
    box-shadow: inset 0 0 0 1px var(--accentLine);
  }
  .actions {
    gap: 6px;
  }
  .notice {
    color: var(--text3);
    font-size: calc(11px + var(--uiStep, 0px));
  }
  .result:hover {
    background: var(--hover2);
  }
  .result .name {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
    font-weight: 500;
  }
  .result .nt {
    min-width: 0;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .result .dir {
    grid-column: 1 / 2;
    font-size: calc(10.5px + var(--uiStep, 0px));
    color: var(--text3);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .result .size {
    grid-column: 2 / 3;
    grid-row: 1 / 3;
    align-self: center;
    font-family: 'Geist Mono', monospace;
    font-size: calc(10.5px + var(--uiStep, 0px));
    color: var(--text3);
  }
  .empty {
    padding: 12px 8px;
    font-size: calc(12px + var(--uiStep, 0px));
    color: var(--text3);
  }
  .saved {
    border-top: 1px solid var(--line);
    padding-top: 8px;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .saved .row {
    gap: 6px;
  }
  .saved .row input {
    flex: 1;
  }
  .saved ul {
    margin: 0;
    padding: 0;
    list-style: none;
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    max-height: 72px;
    overflow: auto;
  }
  .savedRow {
    display: flex;
    align-items: center;
    gap: 2px;
  }
  .apply {
    height: 20px;
    padding: 0 8px;
    font-size: calc(11px + var(--uiStep, 0px));
  }
</style>
