<!--
  DSK-04.3 — Largest files view (FER-322).

  disk_usage_top_files over the SELECTED subtree (else the zoom node):
  recursive, files only, capped — the backend `label` ("Top N …") renders
  verbatim with an explicit `truncated` marker, never as a claim of all
  results (§3.8). Filters (min bytes / categories) re-QUERY the snapshot —
  the pager totals and cap live server-side.

  Row actions (DSK-06.1): Reveal in the existing pane (parent navigation +
  selection by the pane itself), Copy path, Properties (existing
  entry_properties dialog), and Move/Delete ONLY through the existing flows
  (confirm dialogs + operation queue) — each revalidated against the snapshot
  first (diskUsageActions). No auto-clean exists anywhere. Double-click does
  nothing — files are never opened/executed.

  DSK-11 (FER-409): the Files tab fills the RIGHT pane of the single-session
  workspace (no `side` anymore — one store drives both panes).
-->
<script lang="ts">
  import { SvelteSet } from 'svelte/reactivity';
  import type { DiskUsageStore } from './diskUsageStore.svelte';
  import {
    DU_SIZE_NOTE,
    deleteScanEntryWithConfirm,
    moveScanEntryToOtherPane,
    openPropertiesFor,
  } from './diskUsageActions.svelte';
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
  function target(file: DiskUsageFileItem) {
    return { path: file.id, kind: 'file' as const, logicalBytes: file.logicalBytes };
  }

  function sizeText(file: DiskUsageFileItem): string {
    if (store.metric === 'allocated' && file.allocatedBytes === null) return 'unavailable';
    return formatSize(
      store.metric === 'allocated' ? (file.allocatedBytes ?? 0) : file.logicalBytes,
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
      <table>
        <thead>
          <tr>
            <th class="rank">#</th>
            <th scope="col">Nama</th>
            <th scope="col">Jenis</th>
            <th scope="col" class="num">{store.metric === 'allocated' ? 'Allocated' : 'Logical'}</th
            >
            <th scope="col" class="num">% root</th>
            <th scope="col" class="act"><span class="srOnly">Aksi</span></th>
          </tr>
        </thead>
        <tbody>
          {#each result.items as file, i (file.id)}
            <tr data-path={file.id} title={file.id}>
              <td class="rank mono">{i + 1}</td>
              <td class="name">{file.name}</td>
              <td class="kind">{CATEGORY_LABEL[file.category]}</td>
              <td
                class="num mono"
                class:unknown={store.metric === 'allocated' && file.allocatedBytes === null}
                >{sizeText(file)}</td
              >
              <td class="num mono" title={`of parent: ${pctText(file.percentOfParent)}`}
                >{pctText(file.percentOfScanRoot)}</td
              >
              <td class="act">
                <button
                  type="button"
                  class="rowBtn"
                  disabled={!onreveal}
                  title="Tampilkan di pane Files (buka folder induk + pilih)"
                  onclick={() => onreveal?.(file.id)}
                >
                  Reveal
                </button>
                <button
                  type="button"
                  class="rowBtn"
                  title="Salin path lengkap"
                  onclick={() => void copyPath(file)}
                >
                  {copiedId === file.id ? 'Tersalin' : 'Copy path'}
                </button>
                <button
                  type="button"
                  class="rowBtn"
                  title={`Properties (dialog existing) — ${DU_SIZE_NOTE}`}
                  onclick={() => openPropertiesFor(file.id)}
                >
                  Properties
                </button>
                <button
                  type="button"
                  class="rowBtn"
                  title="Move ke folder pane lain — dialog konfirmasi + antrean existing (revalidasi dulu)"
                  onclick={() => void moveScanEntryToOtherPane(store, target(file))}
                >
                  Move…
                </button>
                <button
                  type="button"
                  class="rowBtn"
                  title="Delete ke Recycle Bin — dialog konfirmasi existing (revalidasi dulu)"
                  onclick={() => void deleteScanEntryWithConfirm(store, target(file))}
                >
                  Delete…
                </button>
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
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
    width: 100%;
    border-collapse: collapse;
    font-size: calc(12px + var(--uiStep, 0px));
  }
  th,
  td {
    padding: 3px 8px;
    text-align: left;
    border-bottom: 1px solid var(--line);
    white-space: nowrap;
  }
  td.name {
    overflow: hidden;
    max-width: 0;
    min-width: 30%;
    text-overflow: ellipsis;
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
    width: 1%;
    text-align: right;
  }
  .rowBtn {
    border: 1px solid var(--line2);
    border-radius: var(--r);
    background: var(--elev);
    color: var(--text2);
    font: inherit;
    font-size: calc(10.5px + var(--uiStep, 0px));
    padding: 1px 7px;
    margin-left: 4px;
    cursor: pointer;
  }
  .rowBtn:hover {
    background: var(--hover);
  }
  .rowBtn:disabled {
    opacity: 0.5;
    cursor: default;
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
