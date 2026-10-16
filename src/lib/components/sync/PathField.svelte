<script lang="ts">
  /**
   * One folder field of the profile editor (SP-9.2): path input with suggestions from the existing
   * path suggester, a "Use active pane" shortcut and a caption the backend produced (volume kind).
   * "Browse…" is intentionally absent: the app has no native folder picker yet (no dialog plugin).
   */
  import { pathSuggest, type PathSuggestion } from '../../ipc/pathSuggest';

  let {
    id,
    label,
    value = $bindable(''),
    caption = '',
    errors = [],
    activePath,
    oninput,
  }: {
    id: string;
    label: string;
    value: string;
    caption?: string;
    errors?: string[];
    /** Path of the active pane, or undefined when there is none. */
    activePath?: string;
    oninput?: () => void;
  } = $props();

  let items = $state<PathSuggestion[]>([]);
  let timer: ReturnType<typeof setTimeout> | null = null;
  let seq = 0;

  function changed() {
    oninput?.();
    if (timer) clearTimeout(timer);
    timer = setTimeout(() => void fetchSuggestions(), 150);
  }

  async function fetchSuggestions() {
    timer = null;
    const mine = ++seq;
    const typed = value.trim();
    if (!typed) {
      items = [];
      return;
    }
    try {
      const res = await pathSuggest(typed, activePath ?? '');
      if (mine === seq) items = res.items.filter((i) => i.kind !== 'archive').slice(0, 8);
    } catch {
      if (mine === seq) items = [];
    }
  }

  function pick(item: PathSuggestion) {
    value = item.path;
    items = [];
    seq++;
    oninput?.();
  }

  function useActive() {
    if (!activePath) return;
    value = activePath;
    items = [];
    seq++;
    oninput?.();
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === 'Escape' && items.length > 0) {
      e.stopPropagation();
      items = [];
    }
  }
</script>

<div class="field">
  <label for={id}>{label}</label>
  <div class="inputRow">
    <input
      {id}
      type="text"
      spellcheck="false"
      autocomplete="off"
      placeholder="D:\Photos  or  \\nas\photos"
      aria-invalid={errors.length > 0}
      aria-describedby={`${id}-help`}
      bind:value
      oninput={changed}
      onkeydown={onKey}
    />
    <button type="button" class="use" disabled={!activePath} onclick={useActive}>
      Use active pane
    </button>
  </div>
  {#if items.length > 0}
    <ul class="suggest" role="listbox" aria-label={`${label} suggestions`}>
      {#each items as item (item.path)}
        <li role="option" aria-selected="false">
          <button type="button" onclick={() => pick(item)}>{item.path}</button>
        </li>
      {/each}
    </ul>
  {/if}
  <div id={`${id}-help`} class="help">
    {#if caption}<span class="caption">{caption}</span>{/if}
    {#each errors as msg (msg)}<span class="err">{msg}</span>{/each}
  </div>
</div>

<style>
  .field {
    display: flex;
    flex-direction: column;
    gap: 4px;
    min-width: 0;
  }
  label {
    font-size: 12px;
    color: var(--text2);
  }
  .inputRow {
    display: flex;
    gap: 6px;
  }
  input {
    flex: 1;
    min-width: 0;
    height: 26px;
    padding: 0 8px;
    border: 1px solid var(--line);
    border-radius: var(--r);
    background: var(--bg);
    color: var(--text);
    font-family: inherit;
    font-size: 12.5px;
  }
  input[aria-invalid='true'] {
    border-color: var(--red);
  }
  .use {
    height: 26px;
    padding: 0 10px;
    border: 1px solid var(--line);
    border-radius: var(--r);
    background: transparent;
    color: var(--text2);
    font-size: 12px;
    cursor: default;
  }
  .use:hover:not(:disabled) {
    background: var(--hover);
    color: var(--text);
  }
  .use:disabled {
    opacity: 0.5;
  }
  .suggest {
    list-style: none;
    margin: 0;
    padding: 2px;
    border: 1px solid var(--line);
    border-radius: var(--r);
    background: var(--elev);
    max-height: 160px;
    overflow: auto;
  }
  .suggest button {
    width: 100%;
    text-align: left;
    padding: 3px 6px;
    border: 0;
    border-radius: var(--r);
    background: transparent;
    color: var(--text2);
    font-size: 12px;
    cursor: default;
  }
  .suggest button:hover {
    background: var(--hover);
    color: var(--text);
  }
  .help {
    display: flex;
    flex-direction: column;
    gap: 2px;
    font-size: 11.5px;
    min-height: 0;
  }
  .caption {
    color: var(--text3);
  }
  .err {
    color: var(--redInk);
  }
</style>
