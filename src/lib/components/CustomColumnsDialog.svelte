<script lang="ts">
  import { columns } from '../stores/columnsStore.svelte';
  import { ALL_COLUMNS, COLUMN_LABELS, type ColumnId } from '../columns';
  import Icon from './Icon.svelte';
  import { dialog } from '../dialogKit';
  import Mnemonic from './Mnemonic.svelte';

  const rest = $derived(columns.config.filter((id) => id !== 'name'));

  function isVisible(id: ColumnId): boolean {
    return columns.config.includes(id);
  }

  // FER-577: Esc and Enter (from a checkbox) both close; every toggle already applies live.
  const dialogOpts = { onEscape: () => columns.close(), onEnter: () => columns.close() };
</script>

{#if columns.open}
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="scrim" onmousedown={() => columns.close()}></div>
  <div
    class="modal"
    role="dialog"
    aria-modal="true"
    aria-label="Custom Columns"
    tabindex="-1"
    use:dialog={dialogOpts}
  >
    <header>
      <h2>Custom Columns</h2>
      <button class="icon" title="Close" onclick={() => columns.close()}>
        <Icon name="close" size={11} />
      </button>
    </header>
    <p class="hint">
      Chooses which columns the Full view shows, and in what order. The Name column is always
      visible.
    </p>

    <ul class="list">
      {#each ALL_COLUMNS as id (id)}
        <li class:disabled={id === 'name'}>
          {#if id === 'name'}
            <span class="check pinned" title="The Name column is always visible"></span>
          {:else}
            <input
              type="checkbox"
              class="check"
              checked={isVisible(id)}
              aria-label="Show {COLUMN_LABELS[id]} column"
              onchange={(e) => columns.setVisible(id, e.currentTarget.checked)}
            />
          {/if}
          <span class="label">{COLUMN_LABELS[id]}</span>
          {#if id !== 'name' && isVisible(id)}
            <span class="order">
              <button
                class="mini"
                title="Move {COLUMN_LABELS[id]} up"
                aria-label="Move {COLUMN_LABELS[id]} up"
                disabled={rest.indexOf(id) === 0}
                onclick={() => columns.move(id, -1)}
              >
                ↑
              </button>
              <button
                class="mini"
                title="Move {COLUMN_LABELS[id]} down"
                aria-label="Move {COLUMN_LABELS[id]} down"
                disabled={rest.indexOf(id) === rest.length - 1}
                onclick={() => columns.move(id, 1)}
              >
                ↓
              </button>
            </span>
          {/if}
        </li>
      {/each}
    </ul>

    <footer>
      <button class="ghost" onclick={() => columns.reset()}
        ><Mnemonic label="&Reset to default" /></button
      >
      <button class="primary" onclick={() => columns.close()}><Mnemonic label="&Done" /></button>
    </footer>
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
    width: min(360px, 92vw);
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
  .icon {
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
  .icon:hover {
    background: var(--hover2);
    color: var(--text);
  }
  .hint {
    margin: 0;
    font-size: calc(12px + var(--uiStep, 0px));
    color: var(--text3);
  }
  .list {
    margin: 0;
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  li {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 5px 8px;
    border-radius: var(--r);
  }
  li:hover {
    background: var(--hover);
  }
  .check {
    width: 14px;
    height: 14px;
    flex: none;
    accent-color: var(--accent);
  }
  .check.pinned {
    display: inline-block;
  }
  .label {
    flex: 1;
    font-size: calc(12.5px + var(--uiStep, 0px));
    color: var(--text);
  }
  .order {
    display: flex;
    gap: 2px;
  }
  .mini {
    width: 20px;
    height: 20px;
    display: grid;
    place-items: center;
    border: 0;
    border-radius: var(--r);
    background: transparent;
    color: var(--text2);
    cursor: default;
    font-size: calc(12px + var(--uiStep, 0px));
  }
  .mini:hover:not(:disabled) {
    background: var(--hover2);
    color: var(--text);
  }
  .mini:disabled {
    opacity: 0.35;
  }
  footer {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }
  button {
    border: 0;
    border-radius: var(--r);
    font-size: calc(12.5px + var(--uiStep, 0px));
    cursor: default;
  }
  .ghost {
    height: 28px;
    padding: 0 12px;
    background: transparent;
    color: var(--text2);
  }
  .ghost:hover {
    background: var(--hover);
    color: var(--text);
  }
  .primary {
    height: 28px;
    padding: 0 14px;
    background: var(--accent);
    color: #fff;
    font-weight: 600;
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
