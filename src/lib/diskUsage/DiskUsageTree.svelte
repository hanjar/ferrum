<!--
  DSK-04.2 — Disk usage tree, largest-first (FER-322).

  Alternative access to the treemap: rows come from disk_usage_query_children
  per node (lazy expand, offset/limit + total pager), columns are name,
  metric bytes (allocated null = "unavailable", never logical §2.2),
  percent-of-parent ("—" when null §2.7), a proportional bar, file/folder
  counts and error/partial badges (from the snapshot's subtree aggregates).

  DSK-06.1: per-row context actions (right-click, or the ⋯ affordance) ride
  the EXISTING surfaces via diskUsageActions — Reveal (pane navigation),
  Properties (entry_properties dialog), Copy path, and Move/Delete only
  through the existing confirm dialogs + operation queue, each revalidated
  against the snapshot first. The scan root row offers the read-only set
  only; no auto-clean exists anywhere.

  A11y / keyboard (FER-499, ARIA tree pattern, aria-activedescendant roving):
  ↑/↓ move, Home/End, PageUp/PageDown; → expands a closed folder or moves to
  its first child; ← collapses an open folder or moves to the parent; Enter
  toggles a folder (on a "Muat N lagi" row it loads the next page and keeps
  the cursor at the same index); Shift+Enter opens the folder in the Files
  tab; Ctrl+Enter zooms the treemap into the folder (the old second-Enter
  zoom); `*` expands the folder's direct child folders; a printable key
  jumps to the next row whose name starts with the typed text (type-ahead).
  Tab is not consumed by the App pane switch while the tree has focus.

  Cursor ↔ treemap sync: moving the cursor (debounced 150 ms) zooms the
  treemap to the folder (or to the file's parent, highlighting the file) and
  updates the breadcrumb. A treemap zoom or selection moves the cursor back.
  The scan Root field is never touched by navigation.

  Double-click zooms a FOLDER (files do nothing at all).

  DSK-11 (FER-409): the tree fills the LEFT pane of the single-session
  workspace (no `side` anymore — one store drives both panes).
-->
<script lang="ts">
  import { tick, untrack } from 'svelte';
  import ContextMenu from '../components/ContextMenu.svelte';
  import { contextMenu, createMenuFocusReturn } from '../stores/contextMenuStore.svelte';
  import { TREE_PAGE_SIZE, type DiskUsageStore, type TreeNodeState } from './diskUsageStore.svelte';
  import { DU_SIZE_NOTE, duRowActionItems, type DuScanTarget } from './diskUsageActions.svelte';
  import { formatSize } from '../format';
  import { kindOf } from '../kind';
  import FileIcon from '../components/FileIcon.svelte';
  import { DU_TREE_ROW_COLUMNS, DU_TREE_ROW_MIN_WIDTH, metricValue } from './treemapLayout';
  import type { DiskUsageChildItem } from '../ipc/types';

  let { store, onreveal }: { store: DiskUsageStore; onreveal?: (filePath: string) => void } =
    $props();

  /** Track template without the first (chevron) track — the per-row first
   *  track is `20px + depth × 16px` so names indent with their level. */
  const DU_TREE_ROW_REST = DU_TREE_ROW_COLUMNS.replace(/^\S+\s+/, '');
  const INDENT_PX = 16;
  const ZOOM_DEBOUNCE_MS = 150;

  interface TreeRow {
    /** Stable per-render DOM key (row ids are paths — not safe as keys). */
    domKey: string;
    id: string;
    name: string;
    depth: number;
    kind: DiskUsageChildItem['kind'];
    bytes: number;
    /** Snapshot logical bytes of THIS entry (revalidation input, DSK-06.2). */
    logicalBytes: number;
    /** Allocated unknown under the allocated metric — "unavailable" text. */
    bytesUnknown: boolean;
    /** Backend percentOfParent as shipped — 0–100 scale, null = unknown. */
    percent: number | null;
    files: number;
    dirs: number;
    errors: number;
    expanded: boolean;
    expandable: boolean;
    page: TreeNodeState | null;
    /** A navigable "Muat N lagi" row (not a file/folder). */
    more?: boolean;
    /** Parent node whose next page this row loads. */
    moreParent?: string;
  }

  // ── row model (flattened visible tree) ───────────────────────────────────
  function childRow(item: DiskUsageChildItem, depth: number, domKey: string): TreeRow {
    const previewNode = store.previewNodes[item.id];
    const bytes = metricValue(item, store.metric);
    return {
      domKey,
      id: item.id,
      name: item.name,
      depth,
      kind: item.kind,
      bytes,
      logicalBytes: item.logicalBytes,
      bytesUnknown: store.metric === 'allocated' && item.allocatedBytes === null,
      percent: item.percentOfParent,
      files: item.subtreeFiles ?? previewNode?.subtreeFiles ?? (item.kind === 'file' ? 1 : 0),
      dirs: item.subtreeDirs ?? previewNode?.subtreeDirs ?? 0,
      errors: item.subtreeErrors ?? previewNode?.subtreeErrors ?? 0,
      expanded: store.expanded.has(item.id),
      expandable: item.kind === 'dir' && store.hasData,
      page: store.treeNodes[item.id] ?? null,
    };
  }

  function moreRow(parentId: string, depth: number, domKey: string, page: TreeNodeState): TreeRow {
    return {
      domKey,
      id: `more:${parentId}`,
      name: 'Muat lagi',
      depth,
      kind: 'dir',
      bytes: 0,
      logicalBytes: 0,
      bytesUnknown: false,
      percent: null,
      files: 0,
      dirs: 0,
      errors: 0,
      expanded: false,
      expandable: false,
      page,
      more: true,
      moreParent: parentId,
    };
  }

  function moreLabel(row: TreeRow): string {
    const p = row.page;
    if (!p) return 'Muat lagi';
    if (p.loadingMore) return 'Memuat…';
    const n = Math.min(TREE_PAGE_SIZE, p.total - p.items.length);
    return `Muat ${n} lagi (${p.items.length}/${p.total})`;
  }

  const rows = $derived.by<TreeRow[]>(() => {
    const out: TreeRow[] = [];
    // Root row: live totals while scanning, snapshot aggregates afterwards.
    if (store.phase === 'scanning') {
      out.push({
        domKey: 'r0',
        id: store.root || '·',
        name: store.root || 'Scan…',
        depth: 0,
        kind: 'dir',
        bytes:
          store.metric === 'allocated'
            ? (store.live.knownAllocatedBytes ?? 0)
            : store.live.knownLogicalBytes,
        logicalBytes: 0,
        bytesUnknown: store.metric === 'allocated' && store.live.knownAllocatedBytes === null,
        percent: 100,
        files: store.live.scannedFiles,
        dirs: store.live.scannedDirs,
        errors: store.live.totalErrors,
        expanded: true,
        expandable: false,
        page: null,
      });
      store.scanPreview.forEach((item, i) => out.push(childRow(item, 1, `p${i}`)));
      return out;
    }
    const node = store.snapshot?.rootNode;
    if (!node) return out;
    const bytes =
      store.metric === 'allocated' ? node.subtreeAllocatedBytes : node.subtreeLogicalBytes;
    out.push({
      domKey: 'r0',
      id: node.id,
      name: node.name || node.id,
      depth: 0,
      kind: 'dir',
      bytes: bytes ?? 0,
      logicalBytes: node.subtreeLogicalBytes,
      bytesUnknown: store.metric === 'allocated' && bytes === null,
      percent: 100,
      files: node.subtreeFiles,
      dirs: node.subtreeDirs,
      errors: node.subtreeErrors,
      expanded: true,
      expandable: false,
      // Root children page — always listed (its "Muat" row sits under the root).
      page: store.treeNodes[node.id] ?? null,
    });
    const walk = (nodeId: string, depth: number, prefix: string): void => {
      const page = store.treeNodes[nodeId];
      if (!page) return;
      page.items.forEach((item, i) => {
        const key = `${prefix}/${i}`;
        out.push(childRow(item, depth, key));
        if (item.kind === 'dir' && store.expanded.has(item.id)) walk(item.id, depth + 1, key);
      });
      if (!page.error && page.items.length < page.total) {
        out.push(moreRow(nodeId, depth, `${prefix}/more`, page));
      }
    };
    walk(node.id, 1, 'r');
    return out;
  });

  // ── keyboard roving (aria-activedescendant) ──────────────────────────────
  let activeKey = $state<string | null>(null);
  let treeEl: HTMLElement | undefined = $state();

  const domKeyOf = (key: string): string => `du-row-${key.replace(/[^A-Za-z0-9_-]/g, '_')}`;

  const activeRowDomId = $derived.by(() => {
    const match = activeKey ? rows.find((r) => r.id === activeKey) : undefined;
    if (match) return domKeyOf(match.domKey);
    return rows.length > 0 ? domKeyOf(rows[0].domKey) : undefined;
  });

  function currentRow(): TreeRow | undefined {
    return (activeKey ? rows.find((r) => r.id === activeKey) : undefined) ?? rows[0];
  }

  function activeIndex(): number {
    const i = activeKey ? rows.findIndex((r) => r.id === activeKey) : -1;
    return i === -1 ? 0 : i;
  }

  /** Move the cursor only (no treemap change). */
  function setActive(row: TreeRow): void {
    activeKey = row.id;
    void tick().then(() => {
      const cur = rows.find((r) => r.id === row.id);
      if (!cur) return;
      // jsdom has no scrollIntoView — optional call keeps tests honest.
      document.getElementById(domKeyOf(cur.domKey))?.scrollIntoView?.({ block: 'nearest' });
    });
  }

  // Treemap sync: a debounced user cursor move zooms the treemap to the row.
  let zoomTimer: ReturnType<typeof setTimeout> | undefined;
  /** Zoom / selection values last written by this tree; a different store
   *  value came from the treemap or Files and moves the cursor instead. */
  let syncedZoom: string | undefined;
  let syncedSel: string | null | undefined;

  /** The user moved the cursor onto `row`. */
  function cursorMoved(row: TreeRow): void {
    clearTimeout(zoomTimer);
    setActive(row);
    if (row.more) return;
    const id = row.id;
    zoomTimer = setTimeout(() => {
      const r = rows.find((x) => x.id === id && !x.more);
      if (r) syncTreemapTo(r);
    }, ZOOM_DEBOUNCE_MS);
  }

  /** Nearest shallower expandable row above (the flattened parent). */
  function parentRowOf(row: TreeRow): TreeRow | undefined {
    const i = rows.indexOf(row);
    for (let j = i - 1; j >= 0; j--) {
      if (rows[j].depth < row.depth && rows[j].expandable) return rows[j];
    }
    return undefined;
  }

  /** Zoom the treemap to the row's folder (a file: its parent folder, with the
   *  file highlighted). The scan root (depth 0) is the implicit zoom base. */
  function syncTreemapTo(row: TreeRow): void {
    if (!store.hasData || row.more) return;
    const ancestors: string[] = [];
    let cur: TreeRow = row;
    while (cur.depth > 0) {
      const p = parentRowOf(cur);
      if (!p || p.depth === 0) break;
      ancestors.unshift(p.id);
      cur = p;
    }
    const zoomPath = row.depth > 0 && row.kind === 'dir' ? [...ancestors, row.id] : ancestors;
    store.focusFromTree(zoomPath, row.id);
    syncedZoom = store.zoomNodeId;
    syncedSel = store.selectedNodeId;
  }

  // Treemap/Files → tree: a zoom or selection made elsewhere moves the cursor.
  let pendingTarget = $state<string | null>(null);

  $effect(() => {
    const z = store.zoomNodeId;
    const s = store.selectedNodeId;
    untrack(() => {
      if (syncedZoom === undefined) {
        // First run: the cursor starts where it always did (no selection yet).
        syncedZoom = z;
        syncedSel = s;
        return;
      }
      const zChanged = z !== syncedZoom;
      const sChanged = s !== syncedSel;
      syncedZoom = z;
      syncedSel = s;
      if (!zChanged && !sChanged) return;
      const target = s ?? z;
      if (!target) return;
      const r = rows.find((x) => x.id === target && !x.more);
      if (r) {
        pendingTarget = null;
        if (r.id !== activeKey) {
          clearTimeout(zoomTimer);
          setActive(r);
        }
      } else {
        pendingTarget = target;
      }
    });
  });

  $effect(() => {
    if (pendingTarget) {
      const r = rows.find((x) => x.id === pendingTarget && !x.more);
      if (r) {
        pendingTarget = null;
        if (r.id !== activeKey) {
          clearTimeout(zoomTimer);
          setActive(r);
        }
      }
    }
  });

  // ── folder helpers ───────────────────────────────────────────────────────
  /** `*` on a folder whose children are not loaded yet: expand it, then its
   *  child folders once the first page has arrived. */
  let pendingStar = $state<string | null>(null);

  function expandChildFolders(nodeId: string): void {
    const items = store.treeNodes[nodeId]?.items ?? [];
    for (const it of items) {
      if (it.kind === 'dir' && !store.expanded.has(it.id)) store.toggleExpand(it.id);
    }
  }

  $effect(() => {
    const id = pendingStar;
    if (!id) return;
    const node = store.treeNodes[id];
    if (node && !node.loading) {
      pendingStar = null;
      untrack(() => expandChildFolders(id));
    }
  });

  /** Next page of a folder's children; the cursor keeps its index. */
  async function loadMore(row: TreeRow): Promise<void> {
    if (!row.more || !row.moreParent || row.page?.loadingMore) return;
    const at = rows.findIndex((r) => r.id === row.id);
    await store.loadChildren(row.moreParent, true);
    await tick();
    const next = rows[Math.min(Math.max(at, 0), rows.length - 1)];
    if (next) setActive(next);
  }

  /** Cursor step by `delta` rows (clamped at both ends). */
  function moveBy(delta: number): void {
    if (rows.length === 0) return;
    const i = activeKey ? rows.findIndex((r) => r.id === activeKey) : -1;
    const next = i === -1 ? 0 : Math.min(rows.length - 1, Math.max(0, i + delta));
    cursorMoved(rows[next]);
  }

  function pageStep(): number {
    const h = document.getElementById(activeRowDomId ?? '')?.offsetHeight || 22;
    const visible = treeEl?.clientHeight || 220;
    return Math.max(1, Math.floor(visible / h) - 1);
  }

  // Type-ahead buffer: printable keys jump to the next row whose name starts
  // with the typed text; the buffer resets after a short pause.
  let typeBuf = '';
  let typeTimer: ReturnType<typeof setTimeout> | undefined;

  function typeAhead(ch: string): void {
    clearTimeout(typeTimer);
    typeBuf += ch.toLowerCase();
    typeTimer = setTimeout(() => {
      typeBuf = '';
    }, 700);
    const n = rows.length;
    if (n === 0) return;
    const start = activeIndex();
    // A longer prefix keeps the current row as the first candidate.
    const offset = typeBuf.length > 1 ? 0 : 1;
    for (let k = 0; k < n; k++) {
      const r = rows[(start + offset + k) % n];
      if (!r.more && r.name.toLowerCase().startsWith(typeBuf)) {
        cursorMoved(r);
        return;
      }
    }
  }

  $effect(() => () => {
    clearTimeout(zoomTimer);
    clearTimeout(typeTimer);
  });

  function onTreeKey(e: KeyboardEvent): void {
    if (e.altKey || e.metaKey) return;
    const row = currentRow();

    if (e.ctrlKey) {
      // Ctrl+Enter: zoom the treemap into the folder (the former second-Enter zoom).
      if (e.key === 'Enter' && row && row.expandable && !row.more) {
        e.preventDefault();
        store.zoomIn(row.id);
      }
      return;
    }

    // Type-ahead (printable keys; Space and * have their own meanings).
    if (e.key.length === 1 && e.key !== ' ' && e.key !== '*') {
      e.preventDefault();
      typeAhead(e.key);
      return;
    }

    switch (e.key) {
      case 'ArrowDown':
        e.preventDefault();
        moveBy(1);
        break;
      case 'ArrowUp':
        e.preventDefault();
        moveBy(-1);
        break;
      case 'PageDown':
        e.preventDefault();
        moveBy(pageStep());
        break;
      case 'PageUp':
        e.preventDefault();
        moveBy(-pageStep());
        break;
      case 'Home':
        e.preventDefault();
        if (rows[0]) cursorMoved(rows[0]);
        break;
      case 'End':
        e.preventDefault();
        if (rows.length > 0) cursorMoved(rows[rows.length - 1]);
        break;
      case 'ArrowRight': {
        e.preventDefault();
        if (!row || row.more) break;
        const isOpen = row.depth === 0 || row.expanded;
        if (!isOpen) {
          if (row.expandable) store.toggleExpand(row.id);
          break;
        }
        // Open folder: step to its first child (the next row one level deeper).
        const child = rows[rows.indexOf(row) + 1];
        if (child && child.depth === row.depth + 1) cursorMoved(child);
        break;
      }
      case 'ArrowLeft': {
        e.preventDefault();
        if (!row || row.more) break;
        if (row.expandable && row.expanded) {
          store.toggleExpand(row.id);
          break;
        }
        if (row.depth > 0) {
          const parent = parentRowOf(row);
          if (parent) cursorMoved(parent);
        }
        break;
      }
      case 'Enter':
      case ' ': {
        e.preventDefault();
        if (!row) break;
        if (row.more) {
          void loadMore(row);
          break;
        }
        if (e.shiftKey && row.kind === 'dir') {
          // Open the folder in the Files tab (Files follows the selection).
          store.selectNode(row.id);
          store.setRightTab('files');
          break;
        }
        if (row.expandable) store.toggleExpand(row.id);
        else store.selectNode(row.id);
        break;
      }
      case '*': {
        e.preventDefault();
        if (!row || row.more) break;
        if (row.depth === 0 || row.expanded) expandChildFolders(row.id);
        else if (row.expandable) {
          store.toggleExpand(row.id);
          pendingStar = row.id;
        }
        break;
      }
      case 'Backspace':
        // FER-577 (B4): Backspace = one level up (zoom out), like Backspace in the file list.
        e.preventDefault();
        if (store.zoomStack.length > 0 && store.phase !== 'scanning') store.zoomOut();
        break;
      case 'Tab':
        // Native Tab order: keep the App-level FER-489 pane switch from pulling
        // focus away from the tree. Browser default (focus leaves the tree) stays.
        e.stopPropagation();
        break;
    }
  }

  // ── interactions ─────────────────────────────────────────────────────────
  function rowClick(row: TreeRow): void {
    treeEl?.focus();
    if (row.more) {
      void loadMore(row);
      return;
    }
    store.selectNode(row.id);
    cursorMoved(row);
  }

  function rowDblClick(row: TreeRow): void {
    // ONLY folders zoom; files/symlinks do nothing at all (never open).
    if (row.expandable) void store.zoomIn(row.id);
  }

  // ── DSK-06.1 row context actions (existing surfaces, revalidated) ────────
  // One mounted ContextMenu per component; a private token keeps the hidden
  // FileList's instance from rendering this menu (FileList convention).
  const menuOwner = {};
  const rowMenu = createMenuFocusReturn<string>();

  function rowTarget(row: TreeRow): DuScanTarget {
    return { path: row.id, kind: row.kind, logicalBytes: row.logicalBytes };
  }

  function openRowMenu(row: TreeRow, x: number, y: number, opener?: HTMLElement): void {
    rowMenu.setOpener(opener, row.id);
    contextMenu.open(
      x,
      y,
      duRowActionItems(store, rowTarget(row), { root: row.depth === 0, onreveal }),
      menuOwner,
    );
  }

  function rowContextMenu(row: TreeRow, e: MouseEvent): void {
    e.preventDefault();
    const tr = (e.currentTarget as HTMLElement).closest('.row');
    const btn = tr?.querySelector<HTMLButtonElement>('.ctxBtn') ?? undefined;
    openRowMenu(row, e.clientX, e.clientY, btn);
  }

  /** ⋯ affordance (mouse/keyboard reachable alternative to right-click). */
  function rowMenuButton(row: TreeRow, e: MouseEvent): void {
    e.stopPropagation();
    const btn =
      (e.currentTarget as HTMLElement).closest('button') ?? (e.currentTarget as HTMLElement);
    const rect = btn.getBoundingClientRect();
    openRowMenu(row, rect.left, rect.bottom + 2, btn);
  }

  function bytesText(row: TreeRow): string {
    if (row.bytesUnknown) return 'unavailable';
    return formatSize(row.bytes);
  }

  function pctText(p: number | null): string {
    // Backend percents are 0–100 (contract §2.7, Rust percent_value) —
    // render as-is; ×100 here double-scaled real data ("7200.0%", DSK-07 B1).
    return p === null ? '—' : `${p.toFixed(1)}%`;
  }

  /** Vertical guide lines: one per ancestor level above the row. */
  function guideLevels(depth: number): number[] {
    return Array.from({ length: depth }, (_, i) => i);
  }
</script>

<div
  class="treeWrap"
  data-testid="du-tree"
  style={`--duRowCols: ${DU_TREE_ROW_COLUMNS}; --duRowMinW: ${DU_TREE_ROW_MIN_WIDTH}px`}
>
  <div class="treeHead">
    <span>Folder — largest first</span>
    <span class="hright"
      >{#if store.phase === 'scanning'}pratinjau berjalan{:else}{TREE_PAGE_SIZE}/halaman{/if}</span
    >
  </div>
  <div
    bind:this={treeEl}
    class="tree"
    role="tree"
    tabindex="0"
    aria-label="Tree folder terurut terbesar-dulu"
    aria-activedescendant={activeRowDomId}
    onkeydown={onTreeKey}
  >
    <div class="cols" aria-hidden="true">
      <span class="cchev"></span>
      <span class="cname">Nama</span>
      <span class="cbar"></span>
      <span class="cbytes">Ukuran</span>
      <span class="cpct">%</span>
      <span class="ccnt">File·Fld</span>
    </div>
    {#if rows.length === 0 && store.hasData}
      <div class="hint" role="status">Lokasi kosong (0 file, 0 folder).</div>
    {:else if rows.length === 0}
      <div class="hint" role="status">Memuat struktur…</div>
    {/if}
    {#each rows as row (row.domKey)}
      {#if row.more}
        <div
          id={domKeyOf(row.domKey)}
          class="row more"
          class:act={domKeyOf(row.domKey) === activeRowDomId}
          role="treeitem"
          tabindex="-1"
          aria-level={row.depth + 1}
          aria-label={moreLabel(row)}
          style={`padding-left: ${30 + row.depth * INDENT_PX}px`}
          onclick={() => rowClick(row)}
        >
          {moreLabel(row)}
        </div>
      {:else}
        <div
          id={domKeyOf(row.domKey)}
          class="row"
          class:sel={row.id === store.selectedNodeId}
          class:act={domKeyOf(row.domKey) === activeRowDomId}
          role="treeitem"
          tabindex="-1"
          aria-level={row.depth + 1}
          aria-selected={row.id === store.selectedNodeId}
          aria-expanded={row.expandable ? row.expanded : undefined}
          data-row-id={row.id}
          style={`grid-template-columns: calc(20px + ${row.depth * INDENT_PX}px) ${DU_TREE_ROW_REST}`}
          onclick={() => rowClick(row)}
          ondblclick={() => rowDblClick(row)}
          oncontextmenu={(e) => rowContextMenu(row, e)}
        >
          <span class="chev" style={`padding-left: ${row.depth * INDENT_PX}px`}>
            {#each guideLevels(row.depth) as lvl (lvl)}
              <span class="guide" style={`left: ${lvl * INDENT_PX + 10}px`} aria-hidden="true"
              ></span>
            {/each}
            {#if row.expandable}
              <button
                type="button"
                class="chevBtn"
                tabindex="-1"
                aria-label={row.expanded ? `Collapse ${row.name}` : `Expand ${row.name}`}
                onclick={(e) => {
                  e.stopPropagation();
                  store.toggleExpand(row.id);
                }}
              >
                <svg
                  class="chevIcon"
                  class:open={row.expanded}
                  viewBox="0 0 12 12"
                  width="12"
                  height="12"
                  aria-hidden="true"
                >
                  <path
                    d="M4 2.5 8 6 4 9.5"
                    fill="none"
                    stroke="currentColor"
                    stroke-width="1.6"
                    stroke-linecap="round"
                    stroke-linejoin="round"
                  />
                </svg>
              </button>
            {:else}
              <span class="leafDot" aria-hidden="true">{row.kind === 'symlink' ? '↝' : '·'}</span>
            {/if}
          </span>
          <span class="cname" title={row.id}>
            <span class="cico"
              ><FileIcon
                kind={kindOf({ name: row.name, isDir: row.kind === 'dir' })}
                name={row.name}
                size={14}
              /></span
            >
            <span class="nml">{row.name}</span>
            {#if row.errors > 0}
              <span class="badge" role="status" title={`${row.errors} entri gagal dibaca`}>
                ⚠ {row.errors > 999 ? '999+' : row.errors}
              </span>
            {/if}
            <button
              type="button"
              class="ctxBtn"
              tabindex="-1"
              aria-haspopup="menu"
              aria-expanded={contextMenu.isOpen && rowMenu.activeId === row.id}
              aria-label={`Aksi untuk ${row.name}`}
              title={`Aksi baris via dialog existing (revalidasi dulu) — ${DU_SIZE_NOTE}`}
              onclick={(e) => rowMenuButton(row, e)}
            >
              ⋯
            </button>
          </span>
          <span class="cbar" aria-hidden="true">
            <span
              class="bar"
              style={`width:${row.bytesUnknown ? 0 : Math.min(100, row.percent ?? 0)}%`}
            ></span>
          </span>
          <span class="cbytes mono" class:unknown={row.bytesUnknown}>{bytesText(row)}</span>
          <span class="cpct mono">{pctText(row.percent)}</span>
          <span class="ccnt mono" title={`${row.files} file · ${row.dirs} folder`}
            >{row.files}·{row.dirs}</span
          >
        </div>
        {#if row.page?.error}
          <div class="submsg err" role="status">{row.page.error}</div>
        {:else if row.page?.loading && row.page.items.length === 0}
          <div class="submsg" role="status">Memuat…</div>
        {:else if !row.page && row.expandable && row.expanded}
          <div class="submsg" role="status">Memuat…</div>
        {/if}
      {/if}
    {/each}
  </div>
  <ContextMenu owner={menuOwner} />
</div>

<style>
  .treeWrap {
    display: flex;
    flex-direction: column;
    min-height: 0;
    min-width: 0;
    /* DSK-11: the workspace grid's own gap draws the pane divider. */
  }
  .treeHead {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 8px;
    padding: 5px 10px;
    border-bottom: 1px solid var(--line);
    font-size: calc(11.5px + var(--uiStep, 0px));
    font-weight: 600;
    color: var(--text2);
  }
  .hright {
    font-weight: 400;
    color: var(--text3);
  }
  .tree {
    flex: 1;
    min-height: 0;
    overflow: auto;
    outline: none;
    font-size: calc(12px + var(--uiStep, 0px));
    padding-bottom: 4px;
  }
  .tree:focus-visible {
    box-shadow: inset 0 0 0 2px var(--accentLine);
  }
  /* DSK-10 responsive columns (FER-408): header and rows share ONE track
     template (incl. the chevron track, so labels align with values). Tracks
     shrink to their minimums in a narrow panel — the name never below 140px
     — and below the row minimum the scroller shows a horizontal scrollbar
     instead of silently clipping columns. */
  .cols {
    display: grid;
    grid-template-columns: var(--duRowCols);
    min-width: var(--duRowMinW);
    gap: 0 8px;
    position: sticky;
    top: 0;
    z-index: 1;
    padding: 3px 10px;
    background: var(--elev2);
    border-bottom: 1px solid var(--line);
    color: var(--text3);
    font-size: calc(10.5px + var(--uiStep, 0px));
    font-weight: 600;
  }
  .cchev {
    min-width: 20px;
  }
  .row {
    display: grid;
    grid-template-columns: var(--duRowCols);
    min-width: var(--duRowMinW);
    gap: 0 8px;
    align-items: center;
    padding: 2px 10px;
    cursor: default;
  }
  .row.act {
    background: var(--hover);
  }
  /* The cursor stays visible while the tree has focus (outline, not only bg). */
  .tree:focus-visible .row.act {
    outline: 2px solid var(--accentLine);
    outline-offset: -2px;
  }
  .row.sel {
    box-shadow: inset 0 0 0 1.5px var(--accentInk);
  }
  .chev {
    position: relative;
    display: flex;
    align-items: center;
    min-width: 0;
  }
  .guide {
    position: absolute;
    top: -2px;
    bottom: -2px;
    width: 1px;
    background: var(--line2);
    opacity: 0.8;
    pointer-events: none;
  }
  .chevBtn {
    position: relative;
    z-index: 1;
    display: inline-grid;
    place-items: center;
    width: 20px;
    height: 20px;
    margin: 0;
    padding: 0;
    border: 0;
    border-radius: var(--r);
    background: none;
    color: var(--text2);
    cursor: pointer;
  }
  .chevBtn:hover {
    background: var(--hover);
    color: var(--text);
  }
  .chevIcon {
    display: block;
    transition: transform 120ms ease;
  }
  .chevIcon.open {
    transform: rotate(90deg);
  }
  @media (prefers-reduced-motion: reduce) {
    .chevIcon {
      transition: none;
    }
  }
  .leafDot {
    width: 20px;
    text-align: center;
    color: var(--text3);
  }
  .cname {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
  }
  .cico {
    flex: none;
    display: grid;
    place-items: center;
    width: 14px;
    height: 16px;
  }
  .nml {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .badge {
    flex: none;
    padding: 0 5px;
    border-radius: 999px;
    font-size: calc(10px + var(--uiStep, 0px));
    font-weight: 600;
    background: color-mix(in oklch, var(--amber) 30%, var(--elev));
  }
  .ctxBtn {
    flex: none;
    margin-left: auto;
    border: 1px solid transparent;
    border-radius: var(--r);
    background: none;
    color: var(--text3);
    font: inherit;
    font-size: calc(11px + var(--uiStep, 0px));
    line-height: 1;
    padding: 0 4px;
    cursor: pointer;
  }
  .ctxBtn:hover {
    background: var(--hover);
    color: var(--text);
    border-color: var(--line2);
  }
  .cbar {
    position: relative;
    height: 7px;
    border-radius: 4px;
    background: color-mix(in oklch, var(--bar) 35%, transparent);
    overflow: hidden;
  }
  .bar {
    position: absolute;
    inset: 0 auto 0 0;
    border-radius: 4px;
    background: color-mix(in oklch, var(--folder) 55%, var(--elev));
  }
  .cbytes,
  .cpct,
  .ccnt {
    text-align: right;
    font-variant-numeric: tabular-nums;
    color: var(--text2);
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .cbytes.unknown {
    color: var(--text3);
    font-style: italic;
  }
  .ccnt {
    color: var(--text3);
  }
  .submsg {
    padding: 2px 34px;
    color: var(--text3);
    font-size: calc(11.5px + var(--uiStep, 0px));
  }
  .submsg.err {
    color: color-mix(in oklch, var(--redInk) 75%, var(--text));
  }
  /* "Muat N lagi" is a navigable tree row, indented to its level. */
  .row.more {
    display: block;
    min-width: var(--duRowMinW);
    padding: 3px 10px;
    color: var(--text2);
    font-size: calc(11px + var(--uiStep, 0px));
    cursor: pointer;
  }
  .row.more:hover {
    background: var(--hover);
    color: var(--text);
  }
  .hint {
    padding: 14px 12px;
    color: var(--text2);
  }
</style>
