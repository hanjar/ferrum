<!--
  DSK-05 — Disk usage treemap (squarified). FER-323.

  Frozen props contract (consumed by DSK-04):
    items      DiskUsageChildItem[] — one zoom level's children (DSK-03 shape).
    metric     'logical' | 'allocated' — drives areas AND numbers (§2.6).
    selectedId string | null — highlight sync with tree/Files.
    coverage   'complete' | 'partial' | 'stale' — partial shows the
               denied/partial badge, never a silent gap (§3.5).
    onselect(id | null)  — tile click selects; click on empty plot deselects.
    onzoom(id)           — double-click on a FOLDER only. Files and symlinks
                           do nothing at all (never open/execute).
  Breadcrumb/up navigation is DSK-04's job, not this component's.

  Rendering notes:
  - Tiles are <button>s: tab-focusable, Enter/Space = select (a11y brief).
  - Other group: small tiles fold into one "__other__" tile; its tooltip
    lists members. Zero-area Other (all-null allocated) is hidden from the
    plot but stays in the legend — nothing silently disappears (§2.2).
  - Layout re-measures via ResizeObserver without flicker; squarify is
    deterministic, so progressive scans only ever move tiles deterministically.
-->
<script lang="ts">
  import { onMount } from 'svelte';
  import type {
    DiskUsageChildItem,
    DiskUsageCoverage,
    DiskUsageFileCategory,
    DiskUsageSizeMetric,
  } from '../ipc/types';
  import { formatSize } from '../format';
  import { metricValue, squarify, type TreemapRect } from './treemapLayout';

  let {
    items,
    metric,
    selectedId,
    coverage = 'complete',
    scanRootTotal,
    onselect,
    onzoom,
  }: {
    items: DiskUsageChildItem[];
    metric: DiskUsageSizeMetric;
    selectedId: string | null;
    coverage?: DiskUsageCoverage;
    scanRootTotal?: number;
    onselect?: (id: string | null) => void;
    onzoom?: (id: string) => void;
  } = $props();

  // Plot size: 0 until measured (onMount + ResizeObserver callback). No
  // $effect writes here — the only writers are the observer and onMount,
  // both guarded by a change check so they cannot loop.
  let plotEl: HTMLDivElement | undefined = $state();
  let width = $state(0);
  let height = $state(0);
  let tooltip = $state<{ rect: TreemapRect; x: number; y: number } | null>(null);

  const CATEGORY_COLOR: Record<DiskUsageFileCategory, string> = {
    video: 'var(--ftMedia)',
    image: 'var(--ftImage)',
    audio: 'var(--ftCode)',
    document: 'var(--ftDocs)',
    archive: 'var(--ftArchive)',
    other: 'var(--bar)',
  };
  const CATEGORY_LABEL: Record<DiskUsageFileCategory, string> = {
    video: 'Video',
    image: 'Image',
    audio: 'Audio',
    document: 'Document',
    archive: 'Archive',
    other: 'Other file',
  };
  const CATEGORY_ORDER: DiskUsageFileCategory[] = [
    'video',
    'image',
    'audio',
    'document',
    'archive',
    'other',
  ];

  const layout = $derived(
    width > 0 && height > 0 && items.length > 0
      ? squarify(items, width, height, { metric })
      : { rects: [], totalValue: 0, otherCount: 0 },
  );
  const containers = $derived(layout.rects.filter((r) => r.isDirContainer && r.w >= 1 && r.h >= 1));
  const leaves = $derived(layout.rects.filter((r) => !r.isDirContainer && r.w >= 1 && r.h >= 1));
  const isEmpty = $derived(
    width > 0 && height > 0 && (items.length === 0 || layout.totalValue <= 0),
  );
  const allUnavailable = $derived(
    metric === 'allocated' && items.length > 0 && items.every((i) => i.allocatedBytes === null),
  );

  const legend = $derived.by(() => {
    const entries: { color: string; label: string }[] = [];
    if (layout.rects.some((r) => r.item?.kind === 'dir'))
      entries.push({ color: 'var(--folder)', label: 'Folder' });
    const cats = new Set(
      layout.rects.filter((r) => r.item && r.item.kind !== 'dir').map((r) => r.item!.category),
    );
    for (const c of CATEGORY_ORDER) {
      if (cats.has(c)) entries.push({ color: CATEGORY_COLOR[c], label: CATEGORY_LABEL[c] });
    }
    if (layout.otherCount > 0) {
      entries.push({ color: 'var(--bar)', label: `Other · ${layout.otherCount} item` });
    }
    return entries;
  });

  onMount(() => {
    const el = plotEl;
    if (!el) return;
    const measure = (w: number, h: number) => {
      if (w !== width || h !== height) {
        width = w;
        height = h;
      }
    };
    const box = el.getBoundingClientRect();
    measure(Math.round(box.width), Math.round(box.height));
    const ro = new ResizeObserver((entries) => {
      const r = entries[0]?.contentRect;
      if (r) measure(Math.round(r.width), Math.round(r.height));
    });
    ro.observe(el);
    return () => ro.disconnect();
  });

  function tileClick(r: TreemapRect): void {
    // Other has no tree node to sync with — a click there selects nothing
    // (prototype behaviour), it only informs via its tooltip.
    if (!r.item) return;
    if (r.item.kind === 'dir') {
      onzoom?.(r.id);
    } else {
      onselect?.(r.id);
    }
  }

  function tileDblClick(r: TreemapRect): void {
    // ONLY folders zoom. Files/symlinks never open or execute anything.
    if (r.item && r.item.kind === 'dir') onzoom?.(r.id);
  }

  function plotBackgroundClick(e: MouseEvent): void {
    // Click on empty plot (not a tile) = deselect.
    const t = e.target;
    if (t instanceof Element && plotEl && plotEl.contains(t) && !t.closest('button.tile')) {
      onselect?.(null);
    }
  }

  function showTip(r: TreemapRect, e: MouseEvent): void {
    tooltip = { rect: r, x: e.clientX, y: e.clientY };
  }

  function showTipAtTile(r: TreemapRect, el: Element): void {
    // Keyboard focus has no pointer — anchor the tooltip to the tile itself.
    const b = el.getBoundingClientRect();
    tooltip = { rect: r, x: b.left, y: b.bottom };
  }

  function hideTip(): void {
    tooltip = null;
  }

  function tileColor(r: TreemapRect): string {
    if (!r.item) return 'var(--bar)';
    if (r.item.kind === 'dir') return 'var(--folder)';
    return CATEGORY_COLOR[r.item.category];
  }

  function tileStyle(r: TreemapRect): string {
    const c = tileColor(r);
    const mix = r.item && r.item.kind === 'dir' ? 22 : 28;
    return `left:${r.x}px;top:${r.y}px;width:${r.w}px;height:${r.h}px;background:color-mix(in oklch, ${c} ${mix}%, var(--elev))`;
  }

  /** Renders a 0–100 percent as shipped by the backend (contract §2.7) —
   *  ×100 here double-scaled real data ("7200.0%", DSK-07 B1). Local
   *  fractions ("of zoom") convert to 0–100 at their call sites. */
  function pct(p: number | null): string {
    return p === null ? '—' : `${p.toFixed(1)}%`;
  }

  function focusPct(r: TreemapRect): number | null {
    if (layout.totalValue <= 0) return null;
    return (r.value / layout.totalValue) * 100;
  }

  const metricLabel = $derived(metric === 'allocated' ? 'allocated' : 'logical');

  function tileAria(r: TreemapRect): string {
    const avail =
      r.item && metric === 'allocated' && r.item.allocatedBytes === null
        ? ' (allocated unavailable)'
        : '';
    return `${r.label}: ${formatSize(r.value)} ${metricLabel}${avail}`;
  }

  const tipLines = $derived.by(() => {
    if (!tooltip) return [] as string[];
    const r = tooltip.rect;
    const lines: string[] = [];
    if (r.otherGroup) {
      const g = r.otherGroup;
      lines.push(`${formatSize(g.value)} ${metricLabel} · ${pct(focusPct(r))} of focus`);
      lines.push(
        metric === 'allocated' && g.allocatedBytes === null
          ? 'allocated partial — sebagian anggota unavailable (tidak diganti logical)'
          : `logical ${formatSize(g.logicalBytes)}`,
      );
      const shown = g.members.slice(0, 20);
      for (const m of shown) lines.push(`· ${m.name} — ${formatSize(metricValue(m, metric))}`);
      if (g.members.length > shown.length) lines.push(`… +${g.members.length - shown.length} lagi`);
      lines.push('double-click group: tidak ada aksi');
    } else if (r.item) {
      const it = r.item;
      lines.push(`${formatSize(r.value)} ${metricLabel} · ${pct(focusPct(r))} of focus`);
      const rootPct =
        it.percentOfScanRoot !== null
          ? it.percentOfScanRoot
          : scanRootTotal && scanRootTotal > 0
            ? (r.value / scanRootTotal) * 100
            : null;
      lines.push(`of parent: ${pct(it.percentOfParent)} · of scan root: ${pct(rootPct)}`);
      if (it.allocatedBytes === null) {
        // §2.2 — unknown allocated is "unavailable" in every metric view,
        // never silently 0 and never logical bytes in disguise.
        lines.push(
          metric === 'allocated'
            ? 'allocated: unavailable (logical TIDAK disubstitusi)'
            : 'allocated: unavailable',
        );
      }
      lines.push(
        it.kind === 'dir'
          ? 'klik: fokus/drill · double-click: zoom'
          : 'klik: pilih · double-click file: tidak ada aksi',
      );
    }
    return lines;
  });
</script>

<svelte:window onclick={plotBackgroundClick} />

<section class="treemap" aria-label="Disk usage treemap">
  <div class="plot" bind:this={plotEl}>
    {#if isEmpty}
      <div class="empty" role="status">
        {#if allUnavailable}
          Allocated tidak tersedia untuk semua item pada level ini — bukan 0 B (kontrak §2.2).
        {:else}
          Tidak ada data untuk ditampilkan.
        {/if}
      </div>
    {/if}
    {#if coverage === 'partial'}
      <div class="badge" role="status">
        Permission denied — sebagian isi tidak terbaca (bukan 0)
      </div>
    {:else if coverage === 'stale'}
      <div class="badge" role="status">Data usang — hasil scan sebelumnya</div>
    {/if}

    {#each containers as c (c.id + ':frame')}
      <div
        class="dirFrame"
        class:sel={c.id === selectedId}
        style="left:{c.x}px;top:{c.y}px;width:{c.w}px;height:{c.h}px;"
      >
        {#if c.headerHeight && c.headerHeight > 0}
          <button
            type="button"
            class="tile dir dirHeader"
            class:sel={c.id === selectedId}
            style="left:0px;top:0px;width:{c.w}px;height:{c.headerHeight}px;"
            data-id={c.id}
            aria-pressed={c.id === selectedId}
            aria-label={tileAria(c)}
            onclick={(e) => {
              e.stopPropagation();
              tileClick(c);
            }}
            ondblclick={(e) => {
              e.stopPropagation();
              tileDblClick(c);
            }}
            onmouseenter={(e) => showTip(c, e)}
            onmousemove={(e) => showTip(c, e)}
            onmouseleave={hideTip}
            onfocus={(e) => showTipAtTile(c, e.currentTarget)}
            onblur={hideTip}
          >
            <span class="tin headerTin">
              <span class="tn">{c.label}</span>
              <span class="tv mono">{formatSize(c.value)} · {pct(focusPct(c))}</span>
            </span>
          </button>
        {/if}
      </div>
    {/each}

    {#each leaves as r (r.id)}
      <button
        type="button"
        class="tile"
        class:sel={r.id === selectedId}
        class:dir={r.item?.kind === 'dir'}
        class:group={!r.item}
        class:small={r.w < 64 || r.h < 30}
        class:tiny={r.w < 36 || r.h < 20}
        style={tileStyle(r)}
        aria-pressed={r.id === selectedId}
        aria-label={tileAria(r)}
        data-id={r.id}
        onclick={() => tileClick(r)}
        ondblclick={() => tileDblClick(r)}
        onmouseenter={(e) => showTip(r, e)}
        onmousemove={(e) => showTip(r, e)}
        onmouseleave={hideTip}
        onfocus={(e) => showTipAtTile(r, e.currentTarget)}
        onblur={hideTip}
      >
        <span class="tin">
          <span class="tn">{r.label}</span>
          <span class="tv mono">{formatSize(r.value)} · {pct(focusPct(r))}</span>
        </span>
      </button>
    {/each}
  </div>
  <ul class="legend" aria-label="Legenda kategori warna">
    {#each legend as l (l.label)}
      <li>
        <span class="swatch" style={`background:${l.color}`} aria-hidden="true"></span>{l.label}
      </li>
    {/each}
  </ul>
  {#if tooltip}
    <div class="tip" role="tooltip" style={`left:${tooltip.x + 14}px;top:${tooltip.y + 14}px`}>
      <b>{tooltip.rect.label}</b>
      {#each tipLines as line, i (i)}
        <div class="tl">{line}</div>
      {/each}
    </div>
  {/if}
</section>

<style>
  .treemap {
    position: relative;
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
    gap: 6px;
  }
  .plot {
    position: relative;
    flex: 1;
    min-height: 0;
    overflow: hidden;
    border-radius: var(--rl);
    background: var(--pane);
  }
  .dirFrame {
    position: absolute;
    border: 1px solid color-mix(in oklch, var(--folder) 40%, var(--line2));
    border-radius: var(--r);
    background: color-mix(in oklch, var(--folder) 6%, var(--pane));
    box-sizing: border-box;
    overflow: hidden;
    z-index: 0;
  }
  .dirFrame.sel {
    border-color: var(--accent);
    outline: 2px solid var(--accent);
  }
  .dirHeader {
    border-radius: 0;
    border-bottom: 1px solid color-mix(in oklch, var(--folder) 30%, var(--line2));
    background: color-mix(in oklch, var(--folder) 25%, var(--elev)) !important;
    padding: 1px 4px;
    z-index: 1;
  }
  .headerTin {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: calc(11px + var(--uiStep, 0px));
  }
  .headerTin .tn {
    font-weight: 600;
  }
  .tile {
    position: absolute;
    display: block;
    overflow: hidden;
    padding: 3px 5px;
    border: 1px solid var(--line2);
    border-radius: var(--r);
    color: var(--text);
    font: inherit;
    text-align: left;
    cursor: default;
    box-sizing: border-box;
    z-index: 1;
  }
  .tile:focus-visible {
    outline: 2px solid var(--text);
    outline-offset: -2px;
  }
  .tile.sel {
    outline: 2px solid var(--text);
    outline-offset: -2px;
    z-index: 3;
  }
  .tile.group {
    border-style: dashed;
  }
  /* Folder vs category must not rely on hue alone: the folder token is the
     palette accent family and can sit close to a category hue (e.g. Sunshine
     light). A solid leading bar is the non-color cue (FER-375). */
  .tile.dir {
    box-shadow: inset 3px 0 0 var(--folder);
  }
  .tin {
    display: block;
    min-width: 0;
  }
  .tn {
    display: block;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .tv {
    display: block;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    font-size: calc(11px + var(--uiStep, 0px));
    color: var(--text);
  }
  .tile.small .tv {
    display: none;
  }
  .tile.tiny .tin {
    display: none;
  }
  .empty {
    position: absolute;
    inset: 0;
    display: grid;
    place-content: center;
    color: var(--text2);
  }
  .badge {
    position: absolute;
    top: 6px;
    left: 6px;
    z-index: 4;
    max-width: calc(100% - 12px);
    overflow: hidden;
    padding: 2px 8px;
    border-radius: var(--r);
    background: color-mix(in oklch, var(--amber) 22%, var(--elev));
    color: var(--text);
    font-size: calc(11px + var(--uiStep, 0px));
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .legend {
    display: flex;
    flex-wrap: wrap;
    gap: 4px 14px;
    margin: 0;
    padding: 0;
    list-style: none;
    color: var(--text2);
    font-size: calc(11px + var(--uiStep, 0px));
  }
  .legend .swatch {
    display: inline-block;
    width: 9px;
    height: 9px;
    margin-right: 5px;
    border-radius: 2px;
    vertical-align: -1px;
  }
  .tip {
    position: fixed;
    z-index: 40;
    max-width: 340px;
    padding: 6px 9px;
    border-radius: var(--r);
    background: var(--elev2);
    box-shadow: var(--shadow);
    color: var(--text);
    font-size: calc(12px + var(--uiStep, 0px));
    pointer-events: none;
  }
  .tip .tl {
    color: var(--text2);
  }
</style>
