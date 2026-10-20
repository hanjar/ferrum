<script lang="ts">
  import { onMount } from 'svelte';
  import { getCurrentWindow } from '@tauri-apps/api/window';
  import Icon from './Icon.svelte';
  import MenuBar from './MenuBar.svelte';
  import { ui } from '../uiState.svelte';
  import { modalDepth } from '../dialogKit';
  import { BUILD_LABEL_COMPACT, BUILD_DETAIL } from '../version';
  import { ops } from '../stores/opsStore.svelte';
  import { jobs } from '../stores/jobsStore.svelte';
  import { aiChatHost } from '../ai/aiChatHost.svelte';

  const inTauri = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
  const appWindow = inTauri ? getCurrentWindow() : null;

  let maximized = $state(false);

  // FER-495: the menu lives in the titlebar row. When the full menu plus a usable search box no
  // longer fit, it collapses to a hamburger. The full width is cached while expanded, so the
  // collapsed state never feeds back into its own measurement (no flip-flop / layout thrash).
  const MIN_PALETTE = 200;
  const GAPS = 12 + 12 + 12; // left padding + two grid gaps
  let bar: HTMLElement;
  let leftEl: HTMLElement;
  let menuHost: HTMLElement;
  let rightEl: HTMLElement;
  let compact = $state(false);
  let fullMenuWidth = 0;

  function measure() {
    if (!bar || !leftEl || !menuHost || !rightEl) return;
    const hostW = menuHost.offsetWidth;
    if (!compact) fullMenuWidth = hostW;
    const fixedLeft = leftEl.offsetWidth - hostW;
    const free = bar.clientWidth - GAPS - rightEl.offsetWidth - fixedLeft - MIN_PALETTE;
    compact = fullMenuWidth > free;
  }

  async function syncMaximized() {
    try {
      maximized = (await appWindow?.isMaximized()) ?? false;
    } catch {
      // Not running in a real window (browser dev, e2e mock): keep the default icon.
    }
  }

  onMount(() => {
    let ro: ResizeObserver | undefined;
    if (typeof ResizeObserver !== 'undefined') {
      ro = new ResizeObserver(() => measure());
      ro.observe(bar);
      ro.observe(rightEl);
      ro.observe(menuHost);
    }
    measure();
    void syncMaximized();
    let unlisten: (() => void) | undefined;
    let disposed = false;
    try {
      void appWindow
        ?.onResized(() => void syncMaximized())
        .then((fn) => {
          if (disposed) fn();
          else unlisten = fn;
        })
        .catch(() => {});
    } catch {
      // see syncMaximized
    }
    return () => {
      ro?.disconnect();
      disposed = true;
      unlisten?.();
    };
  });

  async function minimize() {
    await appWindow?.minimize();
  }
  async function toggleMaximize() {
    await appWindow?.toggleMaximize();
  }
  async function close() {
    await appWindow?.close();
  }
</script>

<header class="titlebar" data-tauri-drag-region bind:this={bar}>
  <div class="left" data-tauri-drag-region bind:this={leftEl}>
    <div class="logo" aria-hidden="true" data-tauri-drag-region>
      <div class="logoMark"><span></span><span></span></div>
      <span class="logoName">Ferrum</span>
      <span
        class="buildLabel"
        title={BUILD_DETAIL}
        style="margin-left:6px;font-size:10px;color:var(--text3);opacity:.85;font-family:'Geist Mono',monospace;"
        >{BUILD_LABEL_COMPACT}</span
      >
    </div>
    <button
      class="iconBtn"
      class:active={ui.sidebarOpen}
      title="Sidebar"
      onclick={() => (ui.sidebarOpen = !ui.sidebarOpen)}
    >
      <Icon name="sidebar" />
    </button>
    <div class="menuHost" data-tauri-drag-region bind:this={menuHost}>
      <MenuBar {compact} />
    </div>
  </div>

  <button
    class="paletteBox"
    title="Search files or run a command (Ctrl+K)"
    onclick={() => modalDepth() === 0 && (ui.paletteOpen = true)}
  >
    <Icon name="search" size={14} />
    <span class="paletteText">Search files or run a command</span>
    <span class="mono kbd">Ctrl K</span>
  </button>

  <div class="right" data-tauri-drag-region bind:this={rightEl}>
    <button
      class="iconBtn"
      class:active={ops.panelOpen}
      title="Operations"
      onclick={() => {
        void ops.start();
        ops.togglePanel();
      }}
    >
      <Icon name="queue" />
      {#if ops.active.length + jobs.runningCount > 0}
        <span class="badge" data-testid="ops-badge">{ops.active.length + jobs.runningCount}</span>
      {/if}
    </button>
    <button
      class="iconBtn"
      class:active={aiChatHost.open}
      title="AI assistant (Ctrl+Shift+A)"
      aria-label="AI assistant"
      aria-pressed={aiChatHost.open}
      onclick={() => aiChatHost.toggle()}
    >
      <Icon name="sparkle" />
    </button>
    <button
      class="iconBtn"
      title="Activity"
      aria-label="Activity"
      onclick={() => (ui.activityOpen = true)}
    >
      <Icon name="clock" />
    </button>
    <button class="iconBtn" title="Settings (Ctrl+,)" onclick={() => (ui.settingsOpen = true)}>
      <svg
        width="16"
        height="16"
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        stroke-width="1.7"
        stroke-linecap="round"
        stroke-linejoin="round"
        aria-hidden="true"
      >
        <circle cx="12" cy="12" r="3.2" />
        <path
          d="M19.4 15a1.7 1.7 0 0 0 .34 1.87l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.7 1.7 0 0 0-1.87-.34 1.7 1.7 0 0 0-1 1.55V21a2 2 0 1 1-4 0v-.09a1.7 1.7 0 0 0-1.1-1.55 1.7 1.7 0 0 0-1.88.34l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.7 1.7 0 0 0 .34-1.87 1.7 1.7 0 0 0-1.55-1H3a2 2 0 1 1 0-4h.09a1.7 1.7 0 0 0 1.55-1.1 1.7 1.7 0 0 0-.34-1.88l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.7 1.7 0 0 0 1.87.34h.09a1.7 1.7 0 0 0 1-1.55V3a2 2 0 1 1 4 0v.09a1.7 1.7 0 0 0 1 1.55 1.7 1.7 0 0 0 1.88-.34l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.7 1.7 0 0 0-.34 1.87v.09a1.7 1.7 0 0 0 1.55 1H21a2 2 0 1 1 0 4h-.09a1.7 1.7 0 0 0-1.55 1z"
        />
      </svg>
    </button>
    <button
      class="iconBtn"
      title={ui.theme === 'dark' ? 'Switch to light' : 'Switch to dark'}
      aria-label={ui.theme === 'dark' ? 'Switch to light theme' : 'Switch to dark theme'}
      onclick={() => {
        ui.toggleTheme();
        ui.applyToDocument();
      }}
    >
      <Icon name={ui.theme === 'dark' ? 'moon' : 'sun'} />
    </button>
    <div class="winControls">
      <button title="Minimize" onclick={minimize}
        ><svg width="10" height="10" viewBox="0 0 10 10"
          ><path d="M0 5h10" stroke="currentColor" /></svg
        ></button
      >
      <button
        title={maximized ? 'Restore' : 'Maximize'}
        aria-label={maximized ? 'Restore' : 'Maximize'}
        onclick={toggleMaximize}
      >
        {#if maximized}
          <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor">
            <rect x=".5" y="2.5" width="7" height="7" />
            <path d="M2.5 2.5v-2h7v7h-2" />
          </svg>
        {:else}
          <svg width="10" height="10" viewBox="0 0 10 10"
            ><rect x=".5" y=".5" width="9" height="9" fill="none" stroke="currentColor" /></svg
          >
        {/if}
      </button>
      <button class="closeBtn" title="Close" onclick={close}
        ><svg width="10" height="10" viewBox="0 0 10 10"
          ><path d="M0 0l10 10M10 0L0 10" stroke="currentColor" /></svg
        ></button
      >
    </div>
  </div>
</header>

<style>
  .titlebar {
    height: 40px;
    flex: none;
    display: grid;
    grid-template-columns: auto minmax(120px, 1fr) auto;
    align-items: center;
    gap: 12px;
    padding-left: 12px;
    background: var(--titlebar);
    border-bottom: 1px solid var(--line);
  }
  .left {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
  }
  .logo {
    display: flex;
    align-items: center;
    gap: 8px;
    padding-right: 6px;
  }
  .logoMark {
    width: 18px;
    height: 18px;
    border-radius: 5px;
    background: var(--accent);
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 2px;
    padding: 4px;
  }
  .logoMark span {
    background: rgba(255, 255, 255, 0.92);
    border-radius: 1px;
  }
  .logoMark span:last-child {
    background: rgba(255, 255, 255, 0.55);
  }
  .logoName {
    font-weight: 600;
    font-size: calc(12.5px + var(--uiStep, 0px));
  }
  .iconBtn {
    width: 28px;
    height: 28px;
    display: grid;
    place-items: center;
    border: 0;
    border-radius: var(--r);
    background: transparent;
    color: var(--text2);
    cursor: default;
  }
  .iconBtn:hover {
    background: var(--hover2);
    color: var(--text);
  }
  .iconBtn.active {
    background: var(--hover2);
    color: var(--text);
  }
  .iconBtn {
    position: relative;
  }
  .badge {
    position: absolute;
    top: 2px;
    right: 1px;
    min-width: 14px;
    height: 14px;
    padding: 0 3px;
    border-radius: 7px;
    background: var(--accent);
    color: #fff;
    font-size: calc(9.5px + var(--uiStep, 0px));
    font-weight: 600;
    display: grid;
    place-items: center;
  }
  .menuHost {
    flex: none;
    display: flex;
    align-items: center;
  }
  .paletteBox {
    width: 100%;
    max-width: 440px;
    justify-self: center;
    height: 28px;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 0 5px 0 10px;
    border: 0;
    border-radius: var(--r);
    background: var(--hover);
    box-shadow: inset 0 0 0 1px var(--line);
    color: var(--text3);
    font-size: calc(12.5px + var(--uiStep, 0px));
    cursor: default;
    min-width: 0;
  }
  .paletteBox:hover {
    box-shadow: inset 0 0 0 1px var(--line2);
    color: var(--text2);
  }
  .paletteText {
    flex: 1;
    min-width: 0;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    text-align: left;
  }
  .kbd {
    font-size: calc(10.5px + var(--uiStep, 0px));
    padding: 2px 6px;
    border-radius: 4px;
    background: var(--kbd);
    color: var(--text2);
  }
  .right {
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: 2px;
    height: 100%;
  }
  .winControls {
    display: flex;
    height: 100%;
    margin-left: 8px;
  }
  .winControls button {
    width: 46px;
    display: grid;
    place-items: center;
    border: 0;
    background: transparent;
    color: var(--text2);
    cursor: default;
  }
  .winControls button:hover {
    background: var(--hover2);
  }
  .winControls .closeBtn:hover {
    background: #c42b1c;
    color: #fff;
  }
</style>
