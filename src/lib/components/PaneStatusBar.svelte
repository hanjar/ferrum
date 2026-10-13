<script lang="ts">
  import { formatSize } from '../format';

  let {
    status,
    driveName,
    driveFree,
    driveW,
    /** Real drive info from list_drives; takes precedence over mock props. */
    drive,
    /** True when browsing inside an archive (read-only indicator). */
    readonly,
  }: {
    status: string;
    driveName?: string;
    driveFree?: string;
    driveW?: string;
    drive?: { label: string; freeBytes: number; totalBytes: number } | null;
    readonly?: boolean;
  } = $props();

  const usedPct = $derived(
    drive && drive.totalBytes > 0
      ? Math.min(100, (1 - drive.freeBytes / drive.totalBytes) * 100)
      : 0,
  );
</script>

<footer class="status">
  <span class="info">{status}{readonly ? ' · read-only' : ''}</span>
  {#if drive}
    <span class="drive">{drive.label}</span>
    <span class="bar"><span class="fill" style:width="{usedPct}%"></span></span>
    <span class="free">{formatSize(drive.freeBytes)} free of {formatSize(drive.totalBytes)}</span>
  {:else if driveName}
    <span class="drive">{driveName}</span>
    <span class="bar"><span class="fill" style:width={driveW ?? '0%'}></span></span>
    <span class="free">{driveFree}</span>
  {/if}
</footer>

<style>
  .status {
    height: 28px;
    flex: none;
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 0 12px;
    border-top: 1px solid var(--line);
    font-size: calc(11.5px + var(--uiStep, 0px));
  }
  .info {
    flex: 1;
    min-width: 0;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    color: var(--text2);
  }
  .drive {
    color: var(--text2);
    white-space: nowrap;
  }
  .bar {
    width: 56px;
    height: 3px;
    border-radius: 2px;
    background: var(--line2);
    overflow: hidden;
    flex: none;
  }
  .fill {
    display: block;
    height: 100%;
    background: var(--bar);
  }
  .free {
    color: var(--text3);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    min-width: 0;
    flex: 0 1 auto;
  }
</style>
