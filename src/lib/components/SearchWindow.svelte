<script lang="ts">
  /**
   * F2-27: standalone search window (TC "Search separate"). Shares the
   * search store — each renderer owns its instance, so this search runs
   * independently of the main window's panel. "Show in main window"
   * emits `search://goto`, which the main window handles by navigating
   * the ACTIVE pane to the result's folder.
   */
  import { onMount } from 'svelte';
  import { emit } from '@tauri-apps/api/event';
  import { searchStore } from '../stores/searchStore.svelte';
  import { isInTauri } from '../ipc/tauri';
  import { formatSize } from '../format';

  const results = $derived(searchStore.results);

  function containingDir(path: string): string {
    const i = Math.max(path.lastIndexOf('\\'), path.lastIndexOf('/'));
    let dir = i > 0 ? path.slice(0, i) : path;
    if (/^[A-Za-z]:$/.test(dir)) dir += '\\';
    return dir;
  }

  async function showInMain(path: string) {
    try {
      await emit('search://goto', containingDir(path));
    } catch {
      /* window-to-window bridge unavailable; the row stays where it is */
    }
  }

  onMount(() => {
    const asked = new URLSearchParams(window.location.search).get('root');
    if (asked && !searchStore.root) searchStore.root = asked;
  });

  function onKey(e: KeyboardEvent) {
    if (e.key === 'Enter' && !searchStore.running) void searchStore.start();
  }
</script>

<div class="page" data-testid="search-window">
  <header>
    <h1>Find Files</h1>
    <span class="hint">{isInTauri() ? '' : 'desktop app required'}</span>
  </header>

  <div class="form">
    <label class="field">
      <span>Search in</span>
      <input bind:value={searchStore.root} placeholder="Folder" spellcheck="false" />
    </label>
    <label class="field">
      <span>File name</span>
      <input
        bind:value={searchStore.options.pattern}
        placeholder="*.txt; report-??.csv"
        spellcheck="false"
        onkeydown={onKey}
      />
    </label>
    <label class="field grow">
      <span>Containing text</span>
      <input
        bind:value={searchStore.options.content}
        placeholder="(optional)"
        spellcheck="false"
        onkeydown={onKey}
      />
    </label>
    <div class="buttons">
      <button
        class="primary"
        disabled={searchStore.running}
        onclick={() => void searchStore.start()}
      >
        Start
      </button>
      <button disabled={!searchStore.running} onclick={() => void searchStore.cancel()}>
        Stop
      </button>
    </div>
  </div>

  {#if searchStore.errorText}
    <p class="error" role="alert">{searchStore.errorText}</p>
  {/if}

  <div class="results" role="list" aria-label="Search results">
    {#each results as r (r.path)}
      <div class="row" role="listitem">
        <span class="name">{r.name}</span>
        <span class="dir">{r.path}</span>
        <span class="size">{r.isDir ? '—' : formatSize(r.size)}</span>
        <button class="goto" title="Show in main window" onclick={() => void showInMain(r.path)}>
          Show
        </button>
      </div>
    {:else}
      {#if !searchStore.running}
        <p class="empty">No results yet — set a folder and a name pattern, then Start.</p>
      {/if}
    {/each}
  </div>

  <footer>
    {#if searchStore.running}
      Searching…
    {:else if searchStore.stats}
      {results.length}
      {results.length === 1 ? 'file' : 'files'}
      {#if searchStore.stats.truncated}(result cap reached){/if}
    {/if}
  </footer>
</div>

<style>
  .page {
    height: 100vh;
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding: 14px;
    background: var(--bg);
    color: var(--text);
    font-size: calc(13px + var(--uiStep, 0px));
  }
  header {
    display: flex;
    align-items: baseline;
    gap: 10px;
  }
  h1 {
    margin: 0;
    font-size: calc(15px + var(--uiStep, 0px));
    font-weight: 600;
  }
  .hint {
    font-size: calc(11px + var(--uiStep, 0px));
    color: var(--text3);
  }
  .form {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    align-items: flex-end;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 3px;
    font-size: calc(11px + var(--uiStep, 0px));
    color: var(--text3);
  }
  .field.grow {
    flex: 1;
    min-width: 140px;
  }
  input {
    height: 28px;
    min-width: 180px;
    padding: 0 8px;
    border: 0;
    outline: 0;
    border-radius: var(--r);
    background: var(--pane);
    box-shadow: 0 0 0 1px var(--line2);
    color: var(--text);
    font-size: calc(12.5px + var(--uiStep, 0px));
  }
  input:focus-visible {
    box-shadow: 0 0 0 1.5px var(--accentLine);
  }
  .buttons {
    display: flex;
    gap: 6px;
  }
  button {
    height: 28px;
    padding: 0 12px;
    border: 0;
    border-radius: var(--r);
    background: var(--hover2);
    color: var(--text);
    font-size: calc(12.5px + var(--uiStep, 0px));
    cursor: default;
  }
  button:disabled {
    opacity: 0.5;
  }
  .primary {
    background: var(--accent);
    color: #fff;
    font-weight: 600;
  }
  .error {
    margin: 0;
    font-size: calc(12px + var(--uiStep, 0px));
    color: var(--red);
  }
  .results {
    flex: 1;
    min-height: 0;
    overflow: auto;
    border-radius: var(--rl);
    background: var(--pane);
  }
  .row {
    display: grid;
    grid-template-columns: minmax(0, 0.6fr) minmax(0, 1fr) 72px 52px;
    gap: 10px;
    align-items: center;
    height: 26px;
    padding: 0 10px;
  }
  .row:hover {
    background-image: linear-gradient(var(--hover), var(--hover));
  }
  .name {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .dir {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    color: var(--text3);
    font-size: calc(11.5px + var(--uiStep, 0px));
  }
  .size {
    text-align: right;
    font-family: 'Geist Mono', monospace;
    font-size: calc(11px + var(--uiStep, 0px));
    color: var(--text2);
  }
  .goto {
    height: 20px;
    padding: 0 8px;
    font-size: calc(11px + var(--uiStep, 0px));
  }
  .empty {
    padding: 24px;
    text-align: center;
    color: var(--text3);
    font-size: calc(12px + var(--uiStep, 0px));
  }
  footer {
    min-height: 16px;
    font-size: calc(11px + var(--uiStep, 0px));
    color: var(--text3);
  }
</style>
