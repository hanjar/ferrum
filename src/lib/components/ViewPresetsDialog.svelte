<script lang="ts">
  import {
    applyPresetActive,
    saveActivePreset,
    viewPresets,
  } from '../stores/viewPresetsStore.svelte';
  import Icon from './Icon.svelte';

  let inputEl = $state<HTMLInputElement | undefined>();

  const presets = $derived(viewPresets.open ? viewPresets.entries() : []);

  const SORT_LABEL: Record<string, string> = {
    name: 'Name',
    size: 'Size',
    modified: 'Time',
    extension: 'Extension',
    unsorted: 'Unsorted',
  };

  function sortLabel(key: string): string {
    return SORT_LABEL[key] ?? key;
  }

  $effect(() => {
    if (!viewPresets.open) return;
    queueMicrotask(() => inputEl?.focus());
  });

  function onKey(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.preventDefault();
      viewPresets.close();
    } else if (e.key === 'Enter' && viewPresets.draftName.trim()) {
      e.preventDefault();
      saveActivePreset(viewPresets.draftName);
      viewPresets.draftName = '';
    }
  }
</script>

{#if viewPresets.open}
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="scrim" onmousedown={() => viewPresets.close()}></div>
  <div
    class="modal"
    role="dialog"
    aria-modal="true"
    aria-label="Custom View Modes"
    tabindex="-1"
    onkeydown={onKey}
  >
    <header>
      <h2>Custom View Modes</h2>
      <button class="icon" title="Close" onclick={() => viewPresets.close()}>
        <Icon name="close" size={11} />
      </button>
    </header>
    <p class="hint">
      Saves the active pane's current layout — view mode, sorting and filter — as a named preset.
    </p>

    <div class="saveRow">
      <input
        bind:this={inputEl}
        bind:value={viewPresets.draftName}
        placeholder="Preset name (e.g. Photos)"
        spellcheck="false"
        aria-label="Preset name"
      />
      <button
        class="primary"
        disabled={!viewPresets.draftName.trim()}
        onclick={() => {
          saveActivePreset(viewPresets.draftName);
          viewPresets.draftName = '';
        }}
      >
        Save current
      </button>
    </div>

    {#if presets.length === 0}
      <p class="empty">No presets saved yet.</p>
    {:else}
      <ul class="list">
        {#each presets as preset (preset.name)}
          <li>
            <button
              class="apply"
              title="Apply to the active pane"
              onclick={() => applyPresetActive(preset)}
            >
              <span class="name">{preset.name}</span>
              <span class="desc"
                >{preset.viewMode} · {sortLabel(preset.sort.key)}{preset.reversed ? ' ↓' : ''}
                · {preset.filterMode === 'custom' ? preset.filterPattern : preset.filterMode}</span
              >
            </button>
            <button
              class="del"
              title="Delete preset"
              onclick={() => viewPresets.remove(preset.name)}
            >
              <Icon name="close" size={10} />
            </button>
          </li>
        {/each}
      </ul>
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
    top: 50%;
    transform: translate(-50%, -50%);
    width: min(420px, 92vw);
    padding: 16px;
    border-radius: calc(var(--rl) + 2px);
    background: var(--elev);
    box-shadow: var(--shadow);
    z-index: 61;
    display: flex;
    flex-direction: column;
    gap: 12px;
    animation: fmUp 160ms cubic-bezier(0.2, 0.7, 0.2, 1);
  }
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }
  h2 {
    margin: 0;
    font-size: calc(14px + var(--uiStep, 0px));
    font-weight: 600;
  }
  .icon,
  .del {
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
  .icon:hover,
  .del:hover {
    background: var(--hover2);
    color: var(--text);
  }
  .hint,
  .empty {
    margin: 0;
    font-size: calc(12px + var(--uiStep, 0px));
    color: var(--text3);
  }
  .saveRow {
    display: flex;
    gap: 8px;
  }
  input:not([type]) {
    flex: 1;
    height: 30px;
    padding: 0 10px;
    border: 0;
    outline: 0;
    border-radius: var(--r);
    background: var(--bg);
    box-shadow: 0 0 0 1px var(--line2);
    color: var(--text);
    font-size: calc(13px + var(--uiStep, 0px));
  }
  input:focus-visible {
    box-shadow: 0 0 0 1.5px var(--accentLine);
  }
  button {
    border: 0;
    border-radius: var(--r);
    font-size: calc(12.5px + var(--uiStep, 0px));
    cursor: default;
  }
  .primary {
    height: 30px;
    padding: 0 14px;
    background: var(--accent);
    color: #fff;
    font-weight: 600;
  }
  .primary:disabled {
    opacity: 0.5;
  }
  .list {
    margin: 0;
    padding: 0;
    list-style: none;
    max-height: 240px;
    overflow: auto;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  li {
    display: flex;
    align-items: center;
    gap: 4px;
  }
  .apply {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 1px;
    padding: 6px 10px;
    background: transparent;
    text-align: left;
  }
  .apply:hover {
    background: var(--hover);
  }
  .name {
    font-size: calc(12.5px + var(--uiStep, 0px));
    color: var(--text);
  }
  .desc {
    font-size: calc(11px + var(--uiStep, 0px));
    color: var(--text3);
  }
  @keyframes fmFade {
    from {
      opacity: 0;
    }
    to {
      opacity: 1;
    }
  }
  @keyframes fmUp {
    from {
      opacity: 0;
      transform: translate(-50%, -48%);
    }
    to {
      opacity: 1;
      transform: translate(-50%, -50%);
    }
  }
</style>
