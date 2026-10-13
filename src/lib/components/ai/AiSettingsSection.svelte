<script lang="ts">
  // "AI" section of the Settings dialog (FER-429; replaces the separate AI settings modal).
  // Opened through aiChatHost.openSettings() (menu, palette, chat banner), the section scrolls
  // into view once and focuses Endpoint. After any change the cached AI runtime is dropped so
  // the next request uses the new provider.
  import { onMount } from 'svelte';
  import { aiChatHost } from '../../ai/aiChatHost.svelte';
  import { reloadAiRuntime } from '../../ai/aiRuntime';
  import AiSettings from './AiSettings.svelte';
  import AgentCliSettings from './AgentCliSettings.svelte';
  import { isInTauri } from '../../ipc/tauri';

  let root = $state<HTMLElement>();

  onMount(() => {
    if (!aiChatHost.revealSettings) return;
    aiChatHost.revealSettings = false;
    // After the dialog's own initial focus has run.
    queueMicrotask(() => {
      root?.scrollIntoView?.({ block: 'start' });
      root?.querySelector<HTMLInputElement>('input')?.focus();
    });
  });

  function onChange() {
    reloadAiRuntime();
    aiChatHost.runtimeVersion++;
  }
</script>

<div class="aiSection" id="settings-ai" data-cat="AI" bind:this={root}>
  <AiSettings onchange={onChange} />
  <!-- FER-490: agent CLI lokal (desktop app saja). -->
  {#if isInTauri()}<AgentCliSettings />{/if}
</div>

<style>
  .aiSection {
    margin-top: 18px;
    padding-top: 14px;
    border-top: 1px solid var(--line);
    scroll-margin-top: 8px;
  }
</style>
