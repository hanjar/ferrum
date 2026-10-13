<script lang="ts">
  // SVG glyphs extracted from the prototype. Stroke icons inherit currentColor;
  // filled glyphs use CSS vars so palettes keep working.
  let {
    name,
    size = 16,
    class: cls = '',
  }: {
    name: string;
    size?: number;
    class?: string;
  } = $props();
</script>

<svg
  class={cls}
  width={size}
  height={size}
  viewBox="0 0 24 24"
  fill="none"
  stroke="currentColor"
  stroke-width="1.7"
  stroke-linecap="round"
  stroke-linejoin="round"
  aria-hidden="true"
>
  {#if name === 'sidebar'}
    <rect x="3" y="4.5" width="18" height="15" rx="2.5" /><path d="M9 4.5v15" />
  {:else if name === 'search'}
    <circle cx="11" cy="11" r="7" /><path d="M20 20l-3.5-3.5" />
  {:else if name === 'queue'}
    <path d="M3 12h4l3-7 4 14 3-7h4" />
  {:else if name === 'moon'}
    <path d="M19.5 14.5A7.5 7.5 0 1 1 9.5 4.5a6 6 0 0 0 10 10z" />
  {:else if name === 'sun'}
    <circle cx="12" cy="12" r="4" />
    <path
      d="M12 2.5v2M12 19.5v2M4.6 4.6l1.4 1.4M18 18l1.4 1.4M2.5 12h2M19.5 12h2M4.6 19.4L6 18M18 6l1.4-1.4"
    />
  {:else if name === 'sparkle'}
    <path d="M12 3.5l1.9 5.1 5.1 1.9-5.1 1.9-1.9 5.1-1.9-5.1-5.1-1.9 5.1-1.9z" />
    <path d="M18.5 15.5l.8 2.2 2.2.8-2.2.8-.8 2.2-.8-2.2-2.2-.8 2.2-.8z" />
  {:else if name === 'settings'}
    <path d="M4 7h9M17 7h3M4 12h3M11 12h9M4 17h11M19 17h1" />
    <circle cx="15" cy="7" r="2" /><circle cx="9" cy="12" r="2" /><circle cx="17" cy="17" r="2" />
  {:else if name === 'chevLeft'}
    <path d="M15 18l-6-6 6-6" />
  {:else if name === 'chevRight'}
    <path d="M9 18l6-6-6-6" />
  {:else if name === 'up'}
    <path d="M12 19V6M6 11l6-6 6 6" />
  {:else if name === 'plus'}
    <path d="M12 5v14M5 12h14" />
  {:else if name === 'close'}
    <path d="M6 6l12 12M18 6L6 18" />
  {:else if name === 'filter'}
    <path d="M4 6h16M7 12h10M10 18h4" />
  {:else if name === 'viewDetails'}
    <path d="M4 6h16M4 12h16M4 18h16" />
  {:else if name === 'viewList'}
    <path d="M9 6h11M9 12h11M9 18h11M4.5 6h.01M4.5 12h.01M4.5 18h.01" />
  {:else if name === 'viewGrid'}
    <g stroke-width="2">
      <rect x="4" y="4" width="6.5" height="6.5" rx="1.2" /><rect
        x="13.5"
        y="4"
        width="6.5"
        height="6.5"
        rx="1.2"
      /><rect x="4" y="13.5" width="6.5" height="6.5" rx="1.2" /><rect
        x="13.5"
        y="13.5"
        width="6.5"
        height="6.5"
        rx="1.2"
      />
    </g>
  {:else if name === 'home'}
    <path d="M4 11l8-6.5 8 6.5v8.5a1 1 0 0 1-1 1h-4.5v-6h-5v6H5a1 1 0 0 1-1-1z" />
  {:else if name === 'folderOutline'}
    <path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" />
  {:else if name === 'clock'}
    <circle cx="12" cy="12" r="8.5" /><path d="M12 7.5V12l3 2" />
  {:else if name === 'drive'}
    <rect x="3" y="6" width="18" height="12" rx="2.5" /><path d="M7 14h.01M11 14h6" />
  {:else if name === 'net'}
    <rect x="3" y="4" width="18" height="6.5" rx="1.8" /><rect
      x="3"
      y="13.5"
      width="18"
      height="6.5"
      rx="1.8"
    /><path d="M7 7.2h.01M7 16.8h.01" />
  {:else if name === 'usb'}
    <rect x="6" y="3.5" width="12" height="17" rx="2.5" /><path
      d="M9.5 3.5v4.5M14.5 3.5v4.5M12 15h.01"
    />
  {:else if name === 'disk'}
    <circle cx="12" cy="12" r="8.5" /><circle cx="12" cy="12" r="2.5" />
  {:else if name === 'star'}
    <path d="M12 3.5l2.6 5.3 5.9.9-4.3 4.1 1 5.8L12 16.9l-5.2 2.7 1-5.8-4.3-4.1 5.9-.9z" />
  {:else if name === 'tree'}
    <path d="M5 4v14a2 2 0 0 0 2 2h3M5 10h5" />
    <rect x="12" y="7" width="8" height="6" rx="1.5" /><rect
      x="12"
      y="16"
      width="8"
      height="6"
      rx="1.5"
    />
  {:else if name === 'check'}
    <path d="M5 12.5l4.5 4.5L19 7.5" />
  {/if}
</svg>
