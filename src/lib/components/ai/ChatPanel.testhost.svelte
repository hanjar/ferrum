<script lang="ts">
  // Host uji: tombol pemicu membuka/menutup panel (meniru mounting integrator) +
  // listener window yang mensimulasikan shortcut file manager.
  import type { ChatStore } from '../../ai/chatStore.svelte';
  import ChatPanel from './ChatPanel.svelte';

  let { store, onshortcut }: { store: ChatStore; onshortcut: (key: string) => void } = $props();
  let open = $state(false);
</script>

<svelte:window onkeydown={(e) => onshortcut(e.key)} />
<button type="button" data-testid="trigger" onclick={() => (open = true)}>Open chat</button>
{#if open}
  <ChatPanel {store} onclose={() => (open = false)} />
{/if}
