<script lang="ts">
  import { toast } from '../stores/toastStore.svelte';
</script>

<div class="toastHost" role="status" aria-live="polite">
  {#if toast.message}
    <div class="toast" data-testid="toast">{toast.message}</div>
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
    padding: 6px 14px;
    border-radius: 6px;
    background: var(--elev);
    color: var(--text);
    border: 1px solid var(--line);
    font-size: calc(12px + var(--uiStep, 0px));
    box-shadow: var(--shadow);
  }
</style>
