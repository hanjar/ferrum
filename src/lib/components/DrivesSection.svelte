<script lang="ts">
  /**
   * Sidebar Places > Drives (FER-496): real volumes from `list_drives`.
   * The fixture is used ONLY when there is no Tauri backend (browser dev).
   */
  import { onMount } from 'svelte';
  import Icon from './Icon.svelte';
  import { isInTauri } from '../ipc/tauri';
  import type { DriveInfo } from '../ipc/drives';
  import { mockDrives } from '../mockData';
  import { driveStatus, driveForPath } from '../stores/driveStatus.svelte';
  import { activePane } from '../stores/panesRegistry.svelte';
  import {
    driveIcon,
    driveLabel,
    driveStatusText,
    driveTooltip,
    usedPercent,
  } from '../driveDisplay';

  const inTauri = isInTauri();

  onMount(() => {
    if (!inTauri) return;
    void driveStatus.refresh();
    return driveStatus.watch();
  });

  const drives = $derived<DriveInfo[]>(inTauri ? driveStatus.drives : mockDrives);
  const loading = $derived(inTauri && !driveStatus.ready);
  const current = $derived.by(() => {
    const path = activePane()?.path;
    const d = path ? driveForPath(path, drives) : null;
    return d?.path ?? null;
  });

  function openDrive(d: DriveInfo) {
    if (d.status === 'noMedia') return;
    const pane = activePane();
    if (!pane) return;
    pane.openPath?.(d.path);
    pane.focusList?.();
  }
</script>

<div class="section" role="group" aria-label="Drives">
  <div class="sectionTitle">Drives</div>
  {#if loading}
    <div class="empty" role="status">Memuat drive…</div>
  {:else if drives.length === 0}
    <div class="empty" role="status">Tidak ada drive</div>
  {/if}
  {#each drives as d (d.path)}
    <button
      class="item"
      class:current={d.path === current}
      class:dim={d.status !== 'ok'}
      data-drive={d.path}
      data-status={d.status}
      title={driveTooltip(d)}
      aria-disabled={d.status === 'noMedia'}
      aria-current={d.path === current ? 'true' : undefined}
      onclick={() => openDrive(d)}
    >
      <span class="row">
        <span class="itemIcon"><Icon name={driveIcon(d)} size={15} /></span>
        <span class="label">{driveLabel(d)}</span>
      </span>
      <span class="barWrap">
        {#if d.status === 'ok'}
          <span class="bar"
            ><span class="barFill" class:full={usedPercent(d) >= 90} style:width="{usedPercent(d)}%"
            ></span></span
          >
        {/if}
        <span class="barText">{driveStatusText(d)}</span>
      </span>
    </button>
  {/each}
</div>

<style>
  .section {
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .sectionTitle {
    padding: 6px 8px 4px;
    font-size: calc(11px + var(--uiStep, 0px));
    font-weight: 500;
    color: var(--text3);
  }
  .item {
    display: flex;
    flex-direction: column;
    gap: 5px;
    padding: 5px 8px;
    border: 0;
    border-radius: var(--r);
    background: transparent;
    text-align: left;
    cursor: default;
  }
  .item:hover {
    background: var(--hover);
  }
  .item.current {
    background: var(--hover2);
  }
  .item.dim .label {
    color: var(--text3);
  }
  .item[aria-disabled='true'] {
    opacity: 0.7;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
    color: var(--text2);
  }
  .itemIcon {
    display: grid;
    place-items: center;
    flex: none;
  }
  .label {
    flex: 1;
    min-width: 0;
    font-size: calc(12.5px + var(--uiStep, 0px));
    color: var(--text);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .barWrap {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding-left: 23px;
  }
  .bar {
    height: 3px;
    border-radius: 2px;
    background: var(--line2);
    overflow: hidden;
  }
  .barFill {
    display: block;
    height: 100%;
    background: var(--bar);
    border-radius: 2px;
  }
  .barFill.full {
    background: var(--danger, #d4503f);
  }
  .barText {
    font-size: calc(11px + var(--uiStep, 0px));
    color: var(--text3);
  }
  .empty {
    padding: 5px 8px;
    font-size: 11.5px;
    color: var(--text3);
  }
</style>
