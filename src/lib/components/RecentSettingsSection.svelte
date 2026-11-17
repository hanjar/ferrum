<script lang="ts">
  // FER-598: "Recent files" section of Settings. Windows Recent and newly added items of watched
  // folders (Downloads by default) feed the Recent view and the Ctrl+K "Recent files" group next to
  // Ferrum's own Activity history. Each source has its own switch; OFF means it is not read at all.
  import { ui } from '../uiState.svelte';
  import { recentDefaultFolders, recentValidateFolders } from '../ipc/recent';
  import { recentStore } from '../stores/recentStore.svelte';

  const DAY_CHOICES = [1, 3, 7, 14, 30];

  let defaults = $state<string[]>([]);
  let input = $state('');
  let message = $state('');
  const folders = $derived(ui.recentWatchFolders ?? defaults);

  $effect(() => {
    void recentDefaultFolders()
      .then((d) => (defaults = d))
      .catch(() => undefined);
  });

  function refreshLater(): void {
    void recentStore.refresh().catch(() => undefined);
  }

  async function add(): Promise<void> {
    const raw = input.trim();
    if (raw === '') return;
    message = '';
    const accepted = await recentValidateFolders([raw]).catch(() => []);
    if (accepted.length === 0) {
      message =
        'Use a full path such as D:\\Work or \\\\server\\share. Device paths (\\\\?\\), relative paths and Ferrum’s own data folders are not accepted.';
      return;
    }
    const next = [...folders];
    if (!next.some((f) => f.toLowerCase() === accepted[0].toLowerCase())) next.push(accepted[0]);
    ui.recentWatchFolders = next;
    input = '';
    refreshLater();
  }

  function remove(path: string): void {
    ui.recentWatchFolders = folders.filter((f) => f !== path);
    refreshLater();
  }
</script>

<div class="rec">
  <div class="name">Recent files</div>
  <div class="desc">
    Sources for the Recent view (Ctrl+E) and the Recent files group in Ctrl+K, next to Ferrum’s own
    history.
  </div>

  <label class="row">
    <span>Include Windows Recent</span>
    <input
      class="switch"
      type="checkbox"
      role="switch"
      checked={ui.includeWindowsRecent}
      aria-label="Include Windows Recent"
      onchange={(e) => {
        ui.includeWindowsRecent = (e.currentTarget as HTMLInputElement).checked;
        refreshLater();
      }}
    />
  </label>

  <label class="row">
    <span>Include newly added items from watched folders</span>
    <input
      class="switch"
      type="checkbox"
      role="switch"
      checked={ui.recentNewItems}
      aria-label="Include newly added items from watched folders"
      onchange={(e) => {
        ui.recentNewItems = (e.currentTarget as HTMLInputElement).checked;
        refreshLater();
      }}
    />
  </label>

  {#if ui.recentNewItems}
    <label class="row">
      <span>Count items added within</span>
      <select
        aria-label="New items window in days"
        value={String(ui.recentNewDays)}
        onchange={(e) => {
          ui.recentNewDays = Number((e.currentTarget as HTMLSelectElement).value);
          refreshLater();
        }}
      >
        {#each DAY_CHOICES as d (d)}
          <option value={String(d)}>{d === 1 ? '1 day' : `${d} days`}</option>
        {/each}
      </select>
    </label>

    <div class="desc">Watched folders (top level only, newest first):</div>
    <ul class="folders" aria-label="Watched folders">
      {#each folders as f (f)}
        <li>
          <span class="mono">{f}</span>
          <button type="button" aria-label="Remove {f}" onclick={() => remove(f)}>Remove</button>
        </li>
      {:else}
        <li class="desc">No folder is watched.</li>
      {/each}
    </ul>
    <div class="edit">
      <input
        type="text"
        bind:value={input}
        placeholder="Add a folder, e.g. D:\Work"
        spellcheck="false"
        aria-label="Folder to watch"
        onkeydown={(e) => {
          if (e.key === 'Enter') {
            e.preventDefault();
            void add();
          }
        }}
      />
      <button type="button" onclick={() => void add()}>Add</button>
    </div>
    {#if message}<p class="warn" role="alert">{message}</p>{/if}
  {/if}
</div>

<style>
  .rec {
    margin-top: 18px;
    padding-top: 14px;
    border-top: 1px solid var(--line);
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .name {
    font-size: calc(13px + var(--uiStep, 0px));
    color: var(--text);
  }
  .desc {
    font-size: calc(11.5px + var(--uiStep, 0px));
    color: var(--text3);
  }
  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    font-size: calc(12.5px + var(--uiStep, 0px));
    color: var(--text2);
  }
  .folders {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .folders li {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
  }
  .mono {
    font-family: 'Geist Mono', monospace;
    font-size: calc(11.5px + var(--uiStep, 0px));
    color: var(--text2);
    word-break: break-all;
  }
  .edit {
    display: flex;
    gap: 8px;
  }
  input[type='text'] {
    flex: 1;
    min-width: 200px;
    height: 26px;
    padding: 0 8px;
    border: 0;
    outline: 0;
    border-radius: var(--r);
    background: var(--bg);
    box-shadow: 0 0 0 1px var(--line);
    color: var(--text);
    font-size: calc(12.5px + var(--uiStep, 0px));
  }
  input[type='text']:focus {
    box-shadow: 0 0 0 1.5px var(--accentLine);
  }
  select {
    height: 26px;
    border: 0;
    border-radius: var(--r);
    background: var(--bg);
    box-shadow: 0 0 0 1px var(--line);
    color: var(--text);
  }
  p {
    margin: 0;
    font-size: calc(12px + var(--uiStep, 0px));
    color: var(--text);
  }
  button {
    height: 26px;
    padding: 0 12px;
    border: 0;
    border-radius: var(--r);
    background: var(--elev);
    box-shadow: 0 0 0 1px var(--line);
    color: var(--text);
    cursor: pointer;
  }
</style>
