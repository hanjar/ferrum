<script lang="ts">
  import { toast } from '../stores/toastStore.svelte';
</script>

<div class="toastHost" role="status" aria-live="polite">
  {#if toast.message}
    <div class="toast" data-testid="toast">
      <span>{toast.message}</span>
      {#if toast.action}
        <!-- Never takes the focus: reachable by mouse; the keyboard way is the dock (Alt+B). -->
        <button
          type="button"
          class="act"
          tabindex="-1"
          data-testid="toast-action"
          onmousedown={(e) => e.preventDefault()}
          onclick={() => toast.runAction()}>{toast.action.label}</button
        >
      {/if}
    </div>
  {/if}
</div>

<style>
  .toastHost {
    position: fixed;
    left: 50%;
    bottom: 44px;
    transform: translateX(-50%);
    z-index: 900;
    pointer-events: none;
  }
  .toast {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 6px 14px;
    border-radius: 6px;
    background: var(--elev);
    color: var(--text);
    border: 1px solid var(--line);
    font-size: calc(12px + var(--uiStep, 0px));
    box-shadow: var(--shadow);
  }
  .act {
    pointer-events: auto;
    border: 0;
    border-radius: var(--r);
    background: transparent;
    color: var(--accentInk);
    font: inherit;
    font-weight: 600;
    padding: 2px 6px;
    cursor: default;
  }
  .act:hover {
    background: var(--hover2);
  }
</style>
