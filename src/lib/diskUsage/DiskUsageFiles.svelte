<!--
  DSK-04.3 — Largest files view (FER-322, FER-540).

  disk_usage_top_files over the SELECTED subtree (else the zoom node):
  recursive, files only, capped — the backend `label` ("Top N …") renders
  verbatim with an explicit `truncated` marker, never as a claim of all
  results (§3.8). Filters (min bytes / categories) re-QUERY the snapshot —
  the pager totals and cap live server-side.

  Row actions (DSK-06.1, FER-540): single dropdown button (⋯) and row
  context menu opening the existing ContextMenu: Reveal in pane, Copy path
  (shows "Tersalin" feedback), Properties, and Move/Delete ONLY through the
  existing flows (confirm dialogs + operation queue, revalidated first).
  Double-click does nothing — files are never opened/executed.

  Table layout (FER-540): resizable columns (Name, Jenis, Logical/Allocated,
  % root) via mouse drag and keyboard (←/→ 16 px, role="separator",
  aria-valuenow), double-click resets to default. Column widths persisted to
  localStorage. table-layout: fixed + colgroup for clean ellipsis.

  DSK-11 (FER-409): the Files tab fills the RIGHT pane of the single-session
  workspace (no `side` anymore — one store drives both panes).
-->
<script lang="ts">
  import { SvelteSet } from 'svelte/reactivity';
  import ContextMenu from '../components/ContextMenu.svelte';
  import ColumnResizeHandle from '../components/ColumnResizeHandle.svelte';
  import { contextMenu, createMenuFocusReturn } from '../stores/contextMenuStore.svelte';
  import type { DiskUsageStore } from './diskUsageStore.svelte';
  import { DU_SIZE_NOTE, duFileRowActionItems, type DuScanTarget } from './diskUsageActions.svelte';
  import {
    type DuFilesCol,
    type DuFilesColumnWidths,
    DEFAULT_DU_FILES_COLUMNS,
    DU_FILES_MIN_WIDTHS,
    DU_FILES_MAX_WIDTH,
    DU_FILES_DEFAULT_NAME_WIDTH,
    DU_FILES_RESIZE_STEP,
    loadDuFilesColumnWidths,
    persistDuFilesColumnWidths,
  } from './diskUsageFilesColumns';
  import { formatSize } from '../format';
  import { copyText } from '../clipboard';
  import type { DiskUsageFileCategory, DiskUsageFileItem } from '../ipc/types';

  let { store, onreveal }: { store: DiskUsageStore; onreveal?: (filePath: string) => void } =
    $props();

  const CATEGORY_LABEL: Record<DiskUsageFileCategory, string> = {
    video: 'Video',
    image: 'Image',
    audio: 'Audio',
    document: 'Document',
    archive: 'Archive',
    other: 'Other',
  };
  const CATEGORIES = Object.keys(CATEGORY_LABEL) as DiskUsageFileCategory[];

  let minBytesInput = $state('');
  let chosen = new SvelteSet<DiskUsageFileCategory>();
  let copiedId = $state<string | null>(null);
  let copyTimer: ReturnType<typeof setTimeout> | undefined;

  // ── Column resize state (FER-540) ──────────────────────────────────────────
  let colWidths = $state<DuFilesColumnWidths>(loadDuFilesColumnWidths());
  let thElements = $state<Partial<Record<DuFilesCol, HTMLElement>>>({});

  function getEffectiveColWidth(col: DuFilesCol): number {
    if (col === 'name') {
      if (colWidths.name !== null) return colWidths.name;
      const w = thElements.name?.getBoundingClientRect().width;
      return w && Number.isFinite(w) && w > 0 ? Math.round(w) : DU_FILES_DEFAULT_NAME_WIDTH;
    }
    return colWidths[col];
  }

  function handleColChange(col: DuFilesCol, width: number): void {
    colWidths = { ...colWidths, [col]: width };
  }

  function handleColCommit(): void {
    persistDuFilesColumnWidths(colWidths);
  }

  function handleColReset(col: DuFilesCol): void {
    colWidths = { ...colWidths, [col]: DEFAULT_DU_FILES_COLUMNS[col] };
    persistDuFilesColumnWidths(colWidths);
  }

  const duHeaderCols = $derived([
    {
      key: 'name' as const,
      label: 'Nama',
      resizeLabel: 'Ubah lebar kolom Nama',
      num: false,
      min: DU_FILES_MIN_WIDTHS.name,
    },
    {
      key: 'kind' as const,
      label: 'Jenis',
      resizeLabel: 'Ubah lebar kolom Jenis',
      num: false,
      min: DU_FILES_MIN_WIDTHS.kind,
    },
    {
      key: 'size' as const,
      label: store.metric === 'allocated' ? 'Allocated' : 'Logical',
      resizeLabel: 'Ubah lebar kolom Ukuran',
      num: true,
      min: DU_FILES_MIN_WIDTHS.size,
    },
    {
      key: 'percent' as const,
      label: '% root',
      resizeLabel: 'Ubah lebar kolom % root',
      num: true,
      min: DU_FILES_MIN_WIDTHS.percent,
    },
  ]);

  const minTableWidth = $derived(
    34 +
      (colWidths.name !== null ? colWidths.name : DU_FILES_MIN_WIDTHS.name) +
      colWidths.kind +
      colWidths.size +
      colWidths.percent +
      48,
  );

  // ── Row context actions & dropdown menu (FER-540) ──────────────────────────
  const menuOwner = {};
  const rowMenu = createMenuFocusReturn<string>();

  function applyMinBytes(raw: string): void {
    minBytesInput = raw;
    const n = raw.trim() === '' ? null : Number(raw);
    store.setFilesFilter(n !== null && Number.isFinite(n) && n >= 0 ? Math.floor(n) : null, [
      ...chosen,
    ]);
  }

  function toggleCategory(c: DiskUsageFileCategory): void {
    if (chosen.has(c)) chosen.delete(c);
    else chosen.add(c);
    // SvelteSet mutations are reactive on their own — no reassignment needed.
    store.setFilesFilter(parseMin(), [...chosen]);
  }

  function parseMin(): number | null {
    const raw = minBytesInput.trim();
    if (raw === '') return null;
    const n = Number(raw);
    return Number.isFinite(n) && n >= 0 ? Math.floor(n) : null;
  }

  async function copyPath(file: DiskUsageFileItem): Promise<void> {
    if (await copyText(file.id)) {
      copiedId = file.id;
      clearTimeout(copyTimer);
      copyTimer = setTimeout(() => (copiedId = null), 1500);
    }
  }

  function pctText(p: number | null): string {
    // Backend percents are 0–100 (contract §2.7, Rust percent_value) —
    // render as-is; ×100 here double-scaled real data ("7200.0%", DSK-07 B1).
    return p === null ? '—' : `${p.toFixed(1)}%`;
  }

  /** DSK-06.1: move/delete ride the existing flows; the module revalidates
   *  the snapshot target first and refuses stale ones with a rescan gate. */
  function target(file: DiskUsageFileItem): DuScanTarget {
    return { path: file.id, kind: 'file' as const, logicalBytes: file.logicalBytes };
  }

  function sizeText(file: DiskUsageFileItem): string {
    if (store.metric === 'allocated' && file.allocatedBytes === null) return 'unavailable';
    return formatSize(
      store.metric === 'allocated' ? (file.allocatedBytes ?? 0) : file.logicalBytes,
    );
  }

  function openFileRowMenu(file: DiskUsageFileItem, e: MouseEvent | KeyboardEvent) {
    e.stopPropagation();
    const btn =
      (e.currentTarget as HTMLElement).closest('button') ?? (e.currentTarget as HTMLElement);
    rowMenu.setOpener(btn, file.id);
    const rect = btn.getBoundingClientRect();
    const x = 'clientX' in e && e.clientX > 0 ? e.clientX : rect.left;
    const y = 'clientY' in e && e.clientY > 0 ? e.clientY : rect.bottom + 2;
    contextMenu.open(
      x,
      y,
      duFileRowActionItems(store, target(file), {
        onreveal,
        oncopy: () => void copyPath(file),
      }),
      menuOwner,
    );
  }

  function onRowContextMenu(file: DiskUsageFileItem, e: MouseEvent) {
    e.preventDefault();
    e.stopPropagation();
    const tr = (e.currentTarget as HTMLElement).closest('tr');
    const btn = tr?.querySelector<HTMLButtonElement>('.actBtn') ?? undefined;
    rowMenu.setOpener(btn, file.id);
    contextMenu.open(
      e.clientX,
      e.clientY,
      duFileRowActionItems(store, target(file), {
        onreveal,
        oncopy: () => void copyPath(file),
      }),
      menuOwner,
    );
  }

  const result = $derived(store.files?.result ?? null);
  const loading = $derived(store.files?.loading === true);
  const errorText = $derived(store.files?.error ?? null);
  const scopeName = $derived.by(() => {
    const id = store.filesScopeId;
    const i = Math.max(id.lastIndexOf('/'), id.lastIndexOf('\\'));
    return i >= 0 ? id.slice(i + 1) || id : id;
  });
</script>

<div class="files" data-testid="du-files">
  <div class="meta">
    {#if result}
      <span class="label mono">{result.label}</span>
      <span class="count"
        >{result.items.length}/{result.totalMatching} file · scope: {scopeName}</span
      >
      {#if result.truncated}
        <span class="truncated" role="status">truncated — bukan semua hasil</span>
      {/if}
    {:else if loading}
      <span role="status">Memuat…</span>
    {:else if errorText}
      <span class="err" role="status">{errorText}</span>
    {/if}
  </div>
  <div class="filters">
    <label class="fmin">
      Min bytes
      <input
        type="number"
        min="0"
        step="any"
        placeholder="0"
        bind:value={minBytesInput}
        onchange={(e) => applyMinBytes((e.currentTarget as HTMLInputElement).value)}
      />
    </label>
    <fieldset class="fcats">
      <legend>Kategori</legend>
      {#each CATEGORIES as c (c)}
        <label class="cat">
          <input type="checkbox" checked={chosen.has(c)} onchange={() => toggleCategory(c)} />
          {CATEGORY_LABEL[c]}
        </label>
      {/each}
    </fieldset>
  </div>
  {#if result && result.items.length === 0}
    <div class="empty" role="status">Tidak ada file yang cocok filter di scope ini.</div>
  {:else if result && result.items.length > 0}
    <div class="tableWrap" aria-label="Daftar file terbesar">
      <table
        style:min-width="100%"
        style:width={colWidths.name !== null ? `max(100%, ${minTableWidth}px)` : '100%'}
      >
        <colgroup>
          <col class="colRank" style="width: 34px;" />
          <col
            class="colName"
            style={colWidths.name !== null ? `width: ${colWidths.name}px;` : undefined}
          />
          <col class="colKind" style="width: {colWidths.kind}px;" />
          <col class="colSize" style="width: {colWidths.size}px;" />
          <col class="colPercent" style="width: {colWidths.percent}px;" />
          <col class="colAct" style="width: 48px;" />
        </colgroup>
        <thead>
          <tr>
            <th class="rank">#</th>
            {#each duHeaderCols as col (col.key)}
              <th
                scope="col"
                class="thCol"
                class:num={col.num}
                bind:this={thElements[col.key]}
                style={colWidths[col.key] !== null ? `width: ${colWidths[col.key]}px;` : undefined}
              >
                <span class="thText">{col.label}</span>
                <ColumnResizeHandle
                  label={col.resizeLabel}
                  value={getEffectiveColWidth(col.key)}
                  min={col.min}
                  max={DU_FILES_MAX_WIDTH}
                  step={DU_FILES_RESIZE_STEP}
                  onchange={(w) => handleColChange(col.key, w)}
                  oncommit={handleColCommit}
                  onreset={() => handleColReset(col.key)}
                />
              </th>
            {/each}
            <th scope="col" class="act"><span class="srOnly">Aksi</span></th>
          </tr>
        </thead>
        <tbody>
          {#each result.items as file, i (file.id)}
            <tr
              data-path={file.id}
              title={file.id}
              oncontextmenu={(e) => onRowContextMenu(file, e)}
            >
              <td class="rank mono">{i + 1}</td>
              <td class="name" title={file.name}>{file.name}</td>
              <td class="kind" title={CATEGORY_LABEL[file.category]}
                >{CATEGORY_LABEL[file.category]}</td
              >
              <td
                class="num mono"
                class:unknown={store.metric === 'allocated' && file.allocatedBytes === null}
                title={sizeText(file)}>{sizeText(file)}</td
              >
              <td class="num mono" title={`of parent: ${pctText(file.percentOfParent)}`}
                >{pctText(file.percentOfScanRoot)}</td
              >
              <td class="act">
                <button
                  type="button"
                  class="rowBtn actBtn"
                  aria-haspopup="menu"
                  aria-expanded={contextMenu.isOpen && rowMenu.activeId === file.id}
                  aria-label={`Aksi untuk ${file.name}`}
                  title={copiedId === file.id
                    ? 'Tersalin'
                    : `Aksi untuk ${file.name} — ${DU_SIZE_NOTE}`}
                  onclick={(e) => openFileRowMenu(file, e)}
                >
                  {copiedId === file.id ? 'Tersalin' : '⋯'}
                </button>
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
    <ContextMenu owner={menuOwner} />
  {:else if !loading && !errorText}
    <div class="empty" role="status">Belum ada data — jalankan scan terlebih dahulu.</div>
  {/if}
</div>

<style>
  .files {
    display: flex;
    flex-direction: column;
    min-height: 0;
    min-width: 0;
  }
  .meta {
    display: flex;
    align-items: center;
    gap: 10px;
    flex-wrap: wrap;
    padding: 5px 10px;
    border-bottom: 1px solid var(--line);
    font-size: calc(11.5px + var(--uiStep, 0px));
    color: var(--text2);
  }
  .label {
    font-weight: 600;
    color: var(--text);
  }
  .truncated {
    padding: 0 6px;
    border-radius: 999px;
    background: color-mix(in oklch, var(--amber) 26%, var(--elev));
    font-size: calc(10.5px + var(--uiStep, 0px));
    font-weight: 600;
  }
  .err {
    color: color-mix(in oklch, var(--redInk) 75%, var(--text));
  }
  .filters {
    display: flex;
    align-items: center;
    gap: 14px;
    flex-wrap: wrap;
    padding: 5px 10px;
    border-bottom: 1px solid var(--line);
    font-size: calc(11.5px + var(--uiStep, 0px));
    color: var(--text2);
  }
  .fmin {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }
  .fmin input {
    width: 120px;
  }
  .fcats {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    border: 0;
    padding: 0;
    margin: 0;
  }
  .fcats legend {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip: rect(0 0 0 0);
  }
  .cat {
    display: inline-flex;
    align-items: center;
    gap: 3px;
  }
  .tableWrap {
    flex: 1;
    min-height: 0;
    overflow: auto;
    outline: none;
  }
  table {
    table-layout: fixed;
    border-collapse: collapse;
    font-size: calc(12px + var(--uiStep, 0px));
  }
  th,
  td {
    padding: 3px 8px;
    text-align: left;
    border-bottom: 1px solid var(--line);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  th.thCol {
    position: relative;
    user-select: none;
  }
  .thText {
    display: inline-block;
    max-width: calc(100% - 10px);
    overflow: hidden;
    text-overflow: ellipsis;
    vertical-align: middle;
  }
  th.num .thText {
    display: block;
    text-align: right;
  }
  th {
    position: sticky;
    top: 0;
    background: var(--elev2);
    color: var(--text3);
    font-size: calc(10.5px + var(--uiStep, 0px));
    z-index: 1;
  }
  .rank {
    width: 34px;
    color: var(--text3);
    text-align: right;
  }
  .num {
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
  td.num {
    color: var(--text2);
  }
  td.num.unknown {
    color: var(--text3);
    font-style: italic;
  }
  .act {
    width: 48px;
    text-align: right;
    padding-right: 6px;
  }
  .actBtn {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    min-width: 26px;
    height: 20px;
    border: 1px solid var(--line2);
    border-radius: var(--r);
    background: var(--elev);
    color: var(--text2);
    font: inherit;
    font-size: calc(11px + var(--uiStep, 0px));
    line-height: 1;
    padding: 0 5px;
    margin: 0;
    cursor: pointer;
  }
  .actBtn:hover,
  .actBtn[aria-expanded='true'] {
    background: var(--hover);
    color: var(--text);
  }
  .empty {
    padding: 14px 12px;
    color: var(--text2);
  }
  .srOnly {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip: rect(0 0 0 0);
  }
</style>
