<script lang="ts">
  import { getAction, isEnabled, runAction, type Action } from '../actions';

  const keys: { key: string; id: string; label: string }[] = [
    { key: 'F2', id: 'file.rename', label: 'Rename' },
    { key: 'F5', id: 'file.copy', label: 'Copy' },
    { key: 'F6', id: 'file.move', label: 'Move' },
    { key: 'F7', id: 'file.mkdir', label: 'New folder' },
    { key: 'F8', id: 'file.delete', label: 'Delete' },
  ];

  function choose(e: MouseEvent, id: string): Action | undefined {
    return getAction(id === 'file.delete' && e.shiftKey ? 'file.delete.permanent' : id);
  }
</script>

<footer class="keybar">
  {#each keys as item (item.id)}
    {@const action = getAction(item.id)}
    <button
      class="key"
      title={action
        ? isEnabled(action)
          ? item.label
          : (action.disabledReason ?? 'Unavailable')
        : 'Unavailable'}
      aria-label={`${item.key} ${item.label}`}
      disabled={!action || !isEnabled(action)}
      onclick={(e) => {
        const chosen = choose(e, item.id);
        if (chosen) runAction(chosen);
      }}
    >
      <span class="mono kbd">{item.key}</span>{item.label}
    </button>
  {/each}
  <div class="spacer"></div>
  <div class="hints">
    <span><span class="mono kbd">Ctrl</span>K Commands</span>
  </div>
</footer>

<style>
  .keybar {
    height: 30px;
    flex: none;
    display: flex;
    align-items: center;
    gap: 2px;
    padding: 0 6px;
    background: var(--win);
    border-top: 1px solid var(--line);
    overflow: hidden;
  }
  .key {
    height: 22px;
    flex: none;
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 0 8px 0 4px;
    border: 0;
    border-radius: var(--r);
    background: transparent;
    font-size: calc(12px + var(--uiStep, 0px));
    color: var(--text2);
    white-space: nowrap;
    cursor: default;
  }
  .key:hover {
    background: var(--hover2);
    color: var(--text);
  }
  .kbd {
    font-size: calc(10.5px + var(--uiStep, 0px));
    padding: 1px 5px;
    border-radius: 4px;
    background: var(--kbd);
    color: var(--text3);
  }
  .spacer {
    flex: 1;
  }
  .hints {
    display: flex;
    align-items: center;
    gap: 14px;
    padding-right: 6px;
    font-size: calc(11.5px + var(--uiStep, 0px));
    color: var(--text3);
    white-space: nowrap;
    min-width: 0;
    overflow: hidden;
  }
  .hints span {
    display: flex;
    align-items: center;
    gap: 6px;
  }
</style>
