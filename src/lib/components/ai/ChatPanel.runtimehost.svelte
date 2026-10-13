<script lang="ts">
  // Host uji: meniru wiring App.svelte (panel tetap ter-mount, runtime dibaca ulang saat runtimeVersion naik).
  import { aiChatHost } from '../../ai/aiChatHost.svelte';
  import { aiRuntime } from '../../ai/aiRuntime';
  import ChatPanel from './ChatPanel.svelte';

  const rt = $derived.by(() => {
    void aiChatHost.runtimeVersion;
    return aiRuntime();
  });
</script>

<ChatPanel
  store={rt.chat}
  configured={rt.configured}
  onopensettings={() => aiChatHost.openSettings()}
  onclose={() => {}}
/>
