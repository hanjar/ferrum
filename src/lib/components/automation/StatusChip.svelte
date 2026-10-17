<script lang="ts">
  /**
   * The one chip that renders every automation state (requirements §1): an icon AND a label AND a
   * tint, so colour is never the only signal. The label (and the optional sub-line) are built by
   * the backend; this component only chooses the icon and the tint from the kind. Tints use the
   * theme tokens only, so they follow light, dark and every palette.
   */
  import type { AutomationStatus } from '../../ipc/automation';

  import { STATUS_TONE } from './automationModel';

  let {
    status,
    showSub = false,
  }: {
    status: AutomationStatus;
    /** Draw the backend's sub-line under the chip (the list does; history rows do not). */
    showSub?: boolean;
  } = $props();

  const tone = $derived(STATUS_TONE[status.kind] ?? 'mute');
</script>

<span class="wrap">
  <span class="chip tone-{tone}" data-kind={status.kind} data-tone={tone}>
    <svg
      class="glyph"
      width="12"
      height="12"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      stroke-width="2"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
      data-icon={status.kind}
    >
      {#if status.kind === 'watching'}
        <path d="M2 12s3.5-7 10-7 10 7 10 7-3.5 7-10 7S2 12 2 12z" /><circle
          cx="12"
          cy="12"
          r="3"
        />
      {:else if status.kind === 'quiet'}
        <circle cx="12" cy="13" r="8" /><path d="M12 9v4l2.5 2M9 2h6" />
      {:else if status.kind === 'checking'}
        <path d="M21 12a9 9 0 1 1-3-6.7" /><path d="M21 3v6h-6" />
      {:else if status.kind === 'device'}
        <rect x="3" y="14" width="18" height="6" rx="2" /><path d="M7 17h.01M12 4v7M9 8l3 3 3-3" />
      {:else if status.kind === 'recheck'}
        <path d="M3 12a9 9 0 0 1 15-6.7L21 8" /><path d="M21 3v5h-5M12 8v5M12 16h.01" />
      {:else if status.kind === 'unsupported'}
        <circle cx="12" cy="12" r="9" /><path d="M5.6 5.6l12.8 12.8" />
      {:else if status.kind === 'scheduled'}
        <rect x="3" y="5" width="18" height="16" rx="2.5" /><path d="M3 10h18M8 3v4M16 3v4" />
      {:else if status.kind === 'queued'}
        <path d="M4 6h16M4 12h16M4 18h9" />
      {:else if status.kind === 'pending'}
        <path d="M17 2l4 4-4 4" /><path d="M3 11V9a3 3 0 0 1 3-3h15M7 22l-4-4 4-4" /><path
          d="M21 13v2a3 3 0 0 1-3 3H3"
        />
      {:else if status.kind === 'running'}
        <path d="M7 4l13 8-13 8z" />
      {:else if status.kind === 'paused'}
        <path d="M9 5v14M15 5v14" />
      {:else if status.kind === 'completed'}
        <circle cx="12" cy="12" r="9" /><path d="M8 12.5l3 3 5-6" />
      {:else if status.kind === 'partial'}
        <circle cx="12" cy="12" r="9" /><path d="M12 3v18M12 12h9" />
      {:else if status.kind === 'cancelled'}
        <circle cx="12" cy="12" r="9" /><path d="M9 9l6 6M15 9l-6 6" />
      {:else if status.kind === 'skipped'}
        <path d="M5 4l10 8-10 8zM19 5v14" />
      {:else if status.kind === 'attention'}
        <path d="M12 3l10 18H2z" /><path d="M12 10v5M12 18h.01" />
      {:else if status.kind === 'review'}
        <path d="M4 4h16v13H8l-4 4z" /><path d="M9 9h6M9 13h4" />
      {:else if status.kind === 'warn'}
        <circle cx="12" cy="12" r="9" /><path d="M12 7v6M12 16.5h.01" />
      {:else}
        <circle cx="12" cy="12" r="9" />
      {/if}
    </svg>
    <span class="label">{status.label}</span>
  </span>
  {#if showSub && status.sub}
    <span class="sub">{status.sub}</span>
  {/if}
</span>

<style>
  .wrap {
    display: inline-flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 2px;
    min-width: 0;
    max-width: 100%;
  }
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    max-width: 100%;
    padding: 1px 7px 1px 5px;
    border-radius: 9px;
    font-size: 11.5px;
    font-weight: 600;
    line-height: 16px;
    white-space: nowrap;
    background: var(--hover2);
    color: var(--text2);
  }
  .glyph {
    flex: none;
  }
  .label {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .tone-ok {
    background: var(--greenSoft);
    color: var(--greenInk);
  }
  .tone-info {
    background: var(--accentSoft);
    color: var(--accentInk);
  }
  .tone-warn {
    background: var(--amberSoft);
    color: var(--amberInk);
  }
  .tone-bad {
    background: var(--redSoft);
    color: var(--redInk);
  }
  .tone-mute {
    background: var(--hover2);
    color: var(--text2);
  }
  .sub {
    font-size: 11px;
    color: var(--text3);
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
