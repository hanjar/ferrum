<script lang="ts">
  import { onMount } from 'svelte';
  import Titlebar from './lib/components/Titlebar.svelte';
  import Sidebar from './lib/components/Sidebar.svelte';
  import FilePane from './lib/components/FilePane.svelte';
  import SplitDivider from './lib/components/SplitDivider.svelte';
  import Keybar from './lib/components/Keybar.svelte';
  import Bench from './lib/components/Bench.svelte';
  import OpsPanel from './lib/components/OpsPanel.svelte';
  import Dialogs from './lib/components/Dialogs.svelte';
  import CommandPalette from './lib/components/CommandPalette.svelte';
  import Toast from './lib/components/Toast.svelte';
  import SettingsDialog from './lib/components/SettingsDialog.svelte';
  import { initProviderSetup } from './lib/ai/providerSettings';
  import ActivityPage from './lib/components/ActivityPage.svelte';
  import SyncProfilesPage from './lib/components/sync/SyncProfilesPage.svelte';
  import ChatPanel from './lib/components/ai/ChatPanel.svelte';
  import { aiChatHost } from './lib/ai/aiChatHost.svelte';
  import { aiRuntime } from './lib/ai/aiRuntime';
  // Re-read after a provider change; the new runtime's chat carries the old history, the panel stays mounted.
  const chatRuntime = $derived.by(() => {
    void aiChatHost.runtimeVersion;
    return aiRuntime();
  });
  import { prefs } from './lib/stores/prefsStore.svelte';
  import { applyStartupPreview } from './lib/stores/previewStartup';
  import { actionForEvent, runAction } from './lib/actions';
  import { dialogs } from './lib/stores/dialogStore.svelte';
  import { ops } from './lib/stores/opsStore.svelte';
  import { watcher } from './lib/stores/watchStore.svelte';
  import { driveStatus } from './lib/stores/driveStatus.svelte';
  import { setupExternalDrop } from './lib/stores/externalDrop';
  import { handlePaneTab } from './lib/paneTab';
  import { activePane } from './lib/stores/panesRegistry.svelte';
  import SearchPanel from './lib/components/SearchPanel.svelte';
  import QuickView from './lib/components/QuickView.svelte';
  import ComparePanel from './lib/components/ComparePanel.svelte';
  import SyncPanel from './lib/components/SyncPanel.svelte';
  import MultiRenameDialog from './lib/components/MultiRenameDialog.svelte';
  import PropertiesDialog from './lib/components/PropertiesDialog.svelte';
  import HotlistDialog from './lib/components/HotlistDialog.svelte';
  import HotlistMenu from './lib/components/HotlistMenu.svelte';
  import HistoryMenu from './lib/components/HistoryMenu.svelte';
  import { hotlist } from './lib/stores/hotlistStore.svelte';
  import FileToolsDialog from './lib/components/FileToolsDialog.svelte';
  import ViewPresetsDialog from './lib/components/ViewPresetsDialog.svelte';
  import CustomColumnsDialog from './lib/components/CustomColumnsDialog.svelte';
  import SearchWindow from './lib/components/SearchWindow.svelte';
  import { isSearchWindow } from './lib/searchSeparate';
  import { searchStore } from './lib/stores/searchStore.svelte';
  import { compareStore } from './lib/stores/compareStore.svelte';
  import { ui } from './lib/uiState.svelte';
  import { duWorkspace } from './lib/diskUsage/duWorkspace.svelte';
  import DiskUsageWorkspace from './lib/diskUsage/DiskUsageWorkspace.svelte';
  import { listen } from '@tauri-apps/api/event';
  import { isInTauri } from './lib/ipc/tauri';
  import { GOTO_EVENT } from './lib/searchSeparate';

  $effect(() => {
    ui.applyToDocument();
    // Live OS theme changes while theme === 'system' (FER-370 APP-01); the
    // listener is removed whenever this effect re-runs or the app unmounts.
    return ui.watchSystemTheme();
  });

  // F2-27: the separate search window asks THIS window to jump to a
  // result's folder; the ACTIVE pane navigates.
  onMount(() => {
    if (isInTauri() && !isSearchWindow()) {
      void listen<string>(GOTO_EVENT, (e) => {
        const dir = e.payload as string;
        if (typeof dir === 'string' && dir) activePane()?.openPath?.(dir);
      });
    }
  });

  onMount(async () => {
    void ops.start();
    void watcher.start();
    void driveStatus.refresh();
    void setupExternalDrop(() => activePane()?.path ?? null);
    void hotlist.load(); // FER-458: hotlist from the backend (also migrates the old v1 list)
    void initProviderSetup(); // FER-430: AI provider setup from the backend (never blocks startup)
    await prefs.init(); // persisted prefs apply first…
    applyStartupPreview(); // …then the one-shot FER-396 preview-on-launch rule
    // …then the screenshot/testing hook can override: ?theme=light|dark|system.
    const forced = new URLSearchParams(location.search).get('theme');
    if (forced === 'light' || forced === 'dark' || forced === 'system') {
      ui.setTheme(forced);
    }
  });

  // Virtualization benchmark mode: ?bench=100000 (task 05; results element
  // is read via headless-browser --dump-dom, see docs/benchmarks.md).
  const benchN = Number(new URLSearchParams(location.search).get('bench') ?? 0) || null;

  // F2-27: the separate search window renders a standalone page instead of
  // the dual-pane shell (its own searchStore instance in this renderer).
  const searchWindow = isSearchWindow();

  // FER-406: three-track grid (left | divider | right); fr units keep the
  // ratio responsive at any window size — only the divider track is fixed.
  const splitTracks = $derived(
    `minmax(0, ${ui.paneSplit}fr) auto minmax(0, ${1 - ui.paneSplit}fr)`,
  );

  // Registry-driven shortcuts (task 13). Modal dialogs own the keyboard
  // while open; inside text inputs nothing fires (shortcuts never steal typing).
  function onKeydown(e: KeyboardEvent) {
    const t = e.target as HTMLElement | null;
    const inText =
      t !== null && (t.tagName === 'INPUT' || t.tagName === 'TEXTAREA' || t.isContentEditable);
    if (e.defaultPrevented || dialogs.current || ui.paletteOpen || benchN) return;
    // FER-489: Tab / Shift+Tab only switch between the two file lists (see paneTab.ts).
    if (e.key === 'Tab' && handlePaneTab(e)) {
      e.preventDefault();
      return;
    }
    if (inText) return;
    const action = actionForEvent(e);
    // The Tab shortcut is registered for the palette / menu label; its key is handled above.
    if (action?.scope === 'app-keydown') return;
    if (action) {
      e.preventDefault();
      runAction(action);
    }
  }
</script>

<svelte:window onkeydown={onKeydown} />

{#if benchN}
  <Bench n={benchN} />
{:else if searchWindow}
  <SearchWindow />
{:else}
  <div class="shell">
    <Titlebar />
    <div class="body">
      {#if ui.sidebarOpen}
        <Sidebar />
      {/if}
      <div class="main">
        <!-- DSK-11 (FER-409): the Disk usage workspace is a full-screen mode:
             ONE scan session across the dual-pane grid (tree left, tabs
             right). The Files grid stays MOUNTED but hidden while the mode
             is open, so both panes' tabs/scroll/selection survive Exit. -->
        <div
          class="paneGrid"
          data-pane-grid
          hidden={duWorkspace.open}
          style:grid-template-columns={ui.verticalLayout ? undefined : splitTracks}
          style:grid-template-rows={ui.verticalLayout ? splitTracks : undefined}
        >
          <FilePane paneId={0} />
          <SplitDivider
            orientation={ui.verticalLayout ? 'rows' : 'columns'}
            bind:ratio={ui.paneSplit}
          />
          <FilePane paneId={1} />
        </div>
        {#if duWorkspace.open}
          <DiskUsageWorkspace />
        {/if}
      </div>
      {#if searchStore.panelOpen}
        <SearchPanel />
      {/if}
      {#if ui.quickViewOpen}
        <QuickView />
      {/if}
      {#if compareStore.panelOpen}
        <ComparePanel />
      {/if}
      {#if compareStore.syncOpen}
        <SyncPanel />
      {/if}
      {#if aiChatHost.open}
        <ChatPanel
          store={chatRuntime.chat}
          configured={chatRuntime.configured}
          onopensettings={() => aiChatHost.openSettings()}
          onclose={() => aiChatHost.close()}
          returnFocus={aiChatHost.returnFocus}
        />
      {/if}
    </div>
    <Keybar />
    <OpsPanel />
    <Dialogs />
    <MultiRenameDialog />
    <PropertiesDialog />
    <HotlistMenu />
    <HistoryMenu />
    <HotlistDialog />
    <FileToolsDialog />
    <ViewPresetsDialog />
    <CustomColumnsDialog />
    <CommandPalette />
    <Toast />
    {#if ui.activityOpen}
      <ActivityPage onclose={() => (ui.activityOpen = false)} />
    {/if}
    {#if ui.syncProfilesOpen}
      <SyncProfilesPage onclose={() => (ui.syncProfilesOpen = false)} />
    {/if}
    {#if ui.settingsOpen}
      <SettingsDialog onclose={() => (ui.settingsOpen = false)} />
    {/if}
  </div>
{/if}

<style>
  .shell {
    height: 100%;
    display: flex;
    flex-direction: column;
    background: var(--bg);
    color: var(--text);
    font-size: calc(13px + var(--uiStep, 0px));
    overflow: hidden;
  }
  .body {
    flex: 1;
    min-height: 0;
    display: flex;
  }
  .main {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  /* FER-406: tracks come inline (left | divider | right); the SplitDivider
     paints the separating line, so no gap/background here anymore. */
  .paneGrid {
    flex: 1;
    min-height: 0;
    display: grid;
  }
  /* DSK-10: hidden (not unmounted) while the Disk usage workspace is open —
     the explicit display rule is needed because .paneGrid sets flex/grid. */
  .paneGrid[hidden] {
    display: none;
  }
</style>
