<!--
  DSK-11 (FER-409) — the full-screen Disk usage workspace: ONE scan session
  divided across the dual-pane grid. The largest-first tree fills the LEFT
  pane (its header "Folder — largest first" and the 50/page pager ride in
  that slot), the Treemap|Files tabs fill the RIGHT pane (scope breadcrumb +
  ↑ zoom path + the verbatim "Top N" label), and the global header spans the
  top: root input, metric Logical|Allocated, arrived counts, status chip,
  Scan/Rescan/Cancel, Exit. ONE `duWorkspace.store` instance drives both
  slots — selecting in the tree highlights the treemap tile, double-click
  zooms (breadcrumb in the right slot), with no duplicated state. The DSK-10
  two-independent-sides layout is fully removed.

  The workspace replaces the dual-pane Files grid while `duWorkspace.open`
  (the FilePanes stay mounted-hidden, so their tabs/scroll/selection survive
  Exit). The single global Exit (bar button or Escape, never while a dialog
  owns the keyboard) returns to the normal dual-pane Files and keeps the
  scan results for the next open (states.md).

  State rules (states.md): partial/cancelled never labelled complete; NO
  percent-done and NO ETA while scanning; stale carries the scan-time
  marker; offline/empty/error are honest cards, never 0 bytes.
-->
<script lang="ts">
  import Treemap from './Treemap.svelte';
  import DiskUsageTree from './DiskUsageTree.svelte';
  import DiskUsageFiles from './DiskUsageFiles.svelte';
  import { duWorkspace } from './duWorkspace.svelte';
  import { type DiskUsageTab } from './diskUsageStore.svelte';
  // DSK-06: the standing size note (the revalidation gate lives on the store).
  import { DU_SIZE_NOTE } from './diskUsageActions.svelte';
  import { dialogs } from '../stores/dialogStore.svelte';
  import { paneById } from '../stores/panesRegistry.svelte';
  import { formatDate, parentOf } from '../format';
  import type { DiskUsageCoverage } from '../ipc/types';
  import { diskUsageFastEligibility, diskUsageIndexEligibility } from '../ipc/diskUsage';
  import { cacheWarning, ageLabel } from './cacheAge';
  import { ui } from '../uiState.svelte';
  import { chooseScanMode, type OfferAnswer } from './fastScanOffer';

  const store = duWorkspace.store;

  // Escape = the ONE global Exit (never while a dialog owns the keyboard).
  // One handler at the top — the slots never handle it themselves.
  function onWindowKey(e: KeyboardEvent): void {
    if (e.key !== 'Escape' || e.defaultPrevented) return;
    if (dialogs.current) return;
    e.preventDefault();
    duWorkspace.exit();
  }

  /** DSK-04.3 Reveal: leave the workspace, send the ENTRY pane's Files view
   *  to the entry's parent folder and select it there (the existing pane
   *  navigation — never a new window or an execution). */
  function reveal(path: string): void {
    duWorkspace.exit();
    const api = paneById(duWorkspace.entryPane);
    if (!api) return;
    api.openPath?.(parentOf(path));
    api.selectPaths?.([path], path);
  }

  /** DSK-06.2 CTA: refused stale target → rescan the scope (same start the
   *  header button drives; the gate note is dismissed either way). */
  function rescanFromGate(): void {
    store.clearDuGate();
    void requestScan();
  }

  /** FER-460 + FER-464: Scan/Rescan. A root the FerrumIndexer covers goes
   *  straight to "Instan (indeks)" (no question). Other roots keep the
   *  fast-scan offer (local NTFS only, preference-gated). The manual "Pilihan
   *  scan…" button (`choose`) always shows the offer: regular, fast (admin)
   *  or instant. The offer dialog settles exactly once: a choice, or
   *  dismissal (= no scan). */
  let offering = $state(false);
  async function requestScan(choose = false): Promise<void> {
    if (offering || store.scanRunning) return;
    const root = store.rootInput.trim();
    if (!root) return void store.start();
    offering = true;
    try {
      const mode = await chooseScanMode(root, {
        offerEnabled: () => ui.diskUsageFastOffer,
        eligibility: diskUsageFastEligibility,
        indexEligibility: diskUsageIndexEligibility,
        instantDefault: true,
        forceOffer: choose,
        ask: (elig, index) =>
          new Promise<OfferAnswer>((resolve) => {
            let settled = false;
            dialogs.open({
              kind: 'fastScanOffer',
              root,
              volume: elig.volume ?? index?.volume ?? '',
              fastAvailable: elig.eligible,
              indexAvailable: index?.eligible === true,
              choose: (m, dontAskAgain) => {
                if (settled) return;
                settled = true;
                resolve({ mode: m, dontAskAgain });
                if (dialogs.current?.kind === 'fastScanOffer') dialogs.close();
              },
            });
          }),
        disableOffer: () => {
          ui.diskUsageFastOffer = false;
        },
      });
      if (mode) await store.start(mode === 'normal' ? false : mode === 'fast' ? true : 'index');
    } finally {
      offering = false;
    }
  }

  function setTab(tab: DiskUsageTab): void {
    store.setRightTab(tab);
  }

  function coverage(): DiskUsageCoverage {
    if (store.phase === 'partial' || store.phase === 'cancelled') return 'partial';
    if (store.phase === 'stale') return 'stale';
    return 'complete';
  }

  const scanning = $derived(store.phase === 'scanning');
  const hasData = $derived(store.hasData);

  let isEditingAddress = $state(false);
  let addressText = $state('');

  // Keep addressText synced with the focused folder unless user is actively editing
  const currentFocusPath = $derived(
    store.hasData ? store.zoomNodeId || store.root : store.rootInput,
  );

  $effect(() => {
    const p = currentFocusPath;
    if (!isEditingAddress) {
      addressText = p;
    }
  });

  function onAddressFocus(): void {
    isEditingAddress = true;
    addressText = currentFocusPath;
  }

  function onAddressBlur(): void {
    isEditingAddress = false;
    if (!store.hasData) {
      store.rootInput = addressText;
    } else {
      addressText = currentFocusPath;
    }
  }

  function onAddressInput(e: Event): void {
    const val = (e.target as HTMLInputElement).value;
    addressText = val;
    isEditingAddress = true;
    store.rootInput = val;
  }

  async function onAddressKeyDown(e: KeyboardEvent): Promise<void> {
    if (e.key === 'Enter') {
      e.preventDefault();
      const target = addressText.trim();
      if (!target) return;
      if (store.hasData && store.root) {
        const ok = await store.focusPathIfScanned(target);
        if (ok) {
          isEditingAddress = false;
          (e.target as HTMLElement)?.blur();
          return;
        }
      }
      // Outside scan results or no data: trigger scan
      store.rootInput = target;
      isEditingAddress = false;
      (e.target as HTMLElement)?.blur();
      void requestScan();
    } else if (e.key === 'Escape') {
      isEditingAddress = false;
      addressText = currentFocusPath;
      (e.target as HTMLElement)?.blur();
    }
  }

  function handleScanClick(choose = false): void {
    if (isEditingAddress && addressText.trim()) {
      store.rootInput = addressText.trim();
    } else if (store.hasData && store.root) {
      store.rootInput = store.root;
    }
    void requestScan(choose);
  }

  const canScan = $derived(
    !scanning &&
      !offering &&
      (isEditingAddress
        ? addressText.trim().length > 0
        : store.rootInput.trim().length > 0 || store.root.length > 0),
  );

  // FER-464: a result opened from the on-disk cache is always Stale; it also
  // warns when old (Settings) or after an in-app change.
  const cacheNote = $derived(
    store.fromCache
      ? cacheWarning(store.scannedAtMs, Date.now(), ui.diskUsageCacheWarnHours, store.cacheMutated)
      : null,
  );
</script>

<svelte:window onkeydown={onWindowKey} />

<section
  class="duws"
  data-testid="du-workspace"
  aria-label="Disk usage — tree kiri, Treemap/Files kanan"
>
  <div class="wsbar">
    <span class="wstitle">Disk usage</span>
    {#if store.root && hasData}
      <button
        type="button"
        class="scanRootChip"
        title="Root scan: {store.root} — klik untuk kembali ke root scan"
        onclick={() => store.zoomTo(0)}
      >
        <span class="srLab">Scan:</span>
        {store.root}
      </button>
    {/if}
    <label class="root">
      <span class="rlab">{hasData ? 'Path' : 'Root'}</span>
      <input
        class="mono"
        value={addressText}
        onfocus={onAddressFocus}
        onblur={onAddressBlur}
        oninput={onAddressInput}
        onkeydown={onAddressKeyDown}
        placeholder="C:\folder"
        spellcheck="false"
        aria-label="Path root scan disk usage"
        disabled={scanning}
      />
    </label>
    <button
      type="button"
      class="btn upBtn"
      disabled={scanning || !hasData || store.zoomStack.length === 0}
      title="Naik satu level (zoom out)"
      aria-label="Naik satu level (zoom out)"
      onclick={() => store.zoomOut()}
    >
      ↑
    </button>
    <button class="btn" disabled={!canScan} onclick={() => handleScanClick()}>
      {hasData ? 'Rescan' : 'Scan'}
    </button>
    <button
      class="btn"
      disabled={!canScan}
      title="Pilih sendiri: scan biasa, scan cepat (admin), atau Instan (indeks)"
      onclick={() => handleScanClick(true)}
    >
      Pilihan scan…
    </button>
    <button class="btn danger" disabled={!scanning} onclick={() => void store.cancel()}>
      Cancel
    </button>
    <span class="sep" aria-hidden="true"></span>
    <div class="seg" role="group" aria-label="Metric ukuran">
      <button
        class:sel={store.metric === 'logical'}
        disabled={scanning}
        aria-pressed={store.metric === 'logical'}
        title="Logical bytes — panjang konten menurut metadata (hardlink dihitung per path)"
        onclick={() => store.setMetric('logical')}
      >
        Logical
      </button>
      <button
        class:sel={store.metric === 'allocated'}
        disabled={scanning}
        aria-pressed={store.metric === 'allocated'}
        title="Allocated bytes — alokasi on-disk; tak tersedia = null, tidak diganti logical"
        onclick={() => store.setMetric('allocated')}
      >
        Allocated
      </button>
    </div>
    <!-- DSK-06.4: logical size is NOT recoverable space — standing note. -->
    <span class="metricInfo" role="note" aria-label={DU_SIZE_NOTE} title={DU_SIZE_NOTE}>i</span>
    <span class="counts mono">{store.countsLine()}</span>
    <span class="chip {store.chip.tone}" role="status">{store.chip.text}</span>
    {#if store.engine === 'ntfsMft' && store.phase !== 'idle'}
      <span class="engine" title="Dibaca dari NTFS MFT lewat helper admin (FER-460)">MFT</span>
    {:else if store.engine === 'index' && store.phase !== 'idle'}
      <span class="engine" title="Dibaca dari indeks FerrumIndexer, tanpa admin (FER-476)"
        >Indeks</span
      >
    {/if}
    {#if store.notice}
      <span class="notice" role="note" title={store.notice}>{store.notice}</span>
    {/if}
    <span class="wshint">satu sesi scan · Escape = Exit</span>
    <button
      class="btn exit"
      onclick={() => duWorkspace.exit()}
      title="Kembali ke dual-pane Files (hasil scan dipertahankan)"
    >
      Exit ke Files
    </button>
  </div>

  {#if scanning}
    <div class="note info" role="status">
      Scan berjalan — tanpa persen-selesai dan tanpa ETA (total akhir belum diketahui).
    </div>
  {:else if store.needsRescan}
    <div class="note warn" role="status">
      Hasil scan tidak tersedia lagi (snapshot dibuang atau tidak ditemukan) — Rescan diperlukan.
    </div>
  {:else if store.phase === 'partial'}
    <div class="note warn" role="status">
      Partial — sebagian isi tidak terbaca (bukan 0){store.live.totalErrors > 0
        ? ` · ${store.live.totalErrors} entri gagal`
        : ''}.
    </div>
  {:else if store.phase === 'stale' && store.fromCache}
    <div class="note warn" role="status" data-testid="du-cache-note">
      Hasil dari cache di disk — scan {formatDate(store.scannedAtMs) || '(waktu tidak diketahui)'}
      ({ageLabel(store.scannedAtMs, Date.now())}), bukan kondisi sekarang.
      {#if store.stats?.coverage === 'partial'}Scan aslinya Partial.{/if}
      {#if cacheNote}<strong data-testid="du-cache-warning">{cacheNote}</strong>{/if}
      <button class="noteBtn" onclick={() => void requestScan()}>Rescan</button>
      <button class="noteBtn" onclick={() => void store.deleteCache()}>Hapus cache</button>
    </div>
  {:else if store.phase === 'stale'}
    <div class="note warn" role="status">
      Hasil mungkin usang — scan terakhir {formatDate(store.scannedAtMs) ||
        '(waktu tidak diketahui)'}. Rescan disarankan. Angka di bawah adalah hasil scan LAMA.
      <button class="noteBtn" onclick={() => void requestScan()}>Rescan</button>
    </div>
  {:else if store.phase === 'cancelled'}
    <div class="note info" role="status">
      Scan dibatalkan — data parsial, BUKAN complete. Totals yang sudah diterima tetap ditampilkan.
    </div>
  {:else if store.phase === 'error'}
    <div class="note err" role="status">Scan gagal: {store.errorText}</div>
  {:else if store.phase === 'empty'}
    <div class="note" role="status">Lokasi kosong — scan sukses atas 0 file, 0 folder.</div>
  {/if}

  {#if store.duGate}
    <!-- DSK-06.2: a snapshot target that no longer matches the disk refuses
         its action and demands a rescan — it is never handed to Move/Delete. -->
    <div class="note err" role="alert">
      Aksi dibatalkan — {store.duGate.message}
      <span class="mono gatePath">{store.duGate.path}</span>
      <button class="noteBtn" onclick={rescanFromGate}>Rescan</button>
      <button class="noteBtn" onclick={() => store.clearDuGate()}>Tutup</button>
    </div>
  {/if}

  <div class="cols">
    <!-- Slot A (left pane): the largest-first tree, full size. -->
    <div class="slotLeft" data-testid="du-slot-left">
      <DiskUsageTree {store} onreveal={reveal} />
    </div>
    <!-- Slot B (right pane): the Treemap|Files tabs, full size. -->
    <div class="slotRight" data-testid="du-slot-right">
      <div class="tabs" role="tablist" aria-label="Tampilan kanan">
        <button
          role="tab"
          aria-selected={store.rightTab === 'treemap'}
          class:sel={store.rightTab === 'treemap'}
          onclick={() => setTab('treemap')}
        >
          Treemap
        </button>
        <button
          role="tab"
          aria-selected={store.rightTab === 'files'}
          class:sel={store.rightTab === 'files'}
          onclick={() => setTab('files')}
        >
          Files
        </button>
        <div class="crumbs" aria-label="Scope zoom">
          {#each store.crumbs() as crumb, i (crumb.id)}
            {#if i > 0}<span class="csep" aria-hidden="true">›</span>{/if}
            <button
              class="crumb"
              class:last={i === store.crumbs().length - 1}
              disabled={scanning}
              title={crumb.id}
              onclick={() => store.zoomTo(i)}
            >
              {crumb.label}
            </button>
          {/each}
        </div>
        <span class="zpath mono" title={store.zoomNodeId}>
          <button
            class="up"
            disabled={store.zoomStack.length === 0}
            aria-label="Naik satu level (zoom out)"
            onclick={() => store.zoomOut()}
          >
            ↑
          </button>
          {store.zoomNodeId || '—'}
        </span>
      </div>
      <div
        class="view"
        role="tabpanel"
        aria-label={store.rightTab === 'treemap' ? 'Treemap' : 'Files terbesar'}
      >
        {#if store.phase === 'idle'}
          <div class="card" role="status">
            <b>Belum ada scan.</b>
            <span>Root diisi dari folder pane — ubah bila perlu, lalu mulai.</span>
            <button class="btn primary" disabled={!canScan} onclick={() => void requestScan()}>
              Mulai scan
            </button>
          </div>
        {:else if store.phase === 'error'}
          <div class="card" role="status">
            <b>Scan gagal.</b>
            <span>{store.errorText}</span>
            <button class="btn" disabled={!canScan} onclick={() => void requestScan()}>
              Coba lagi
            </button>
          </div>
        {:else if store.phase === 'empty'}
          <div class="card" role="status">
            <b>Lokasi kosong.</b>
            <span>Scan sukses atas 0 file, 0 folder — coverage tetap complete.</span>
            <button class="btn" onclick={() => duWorkspace.exit()}>Kembali ke pane Files</button>
          </div>
        {:else if scanning}
          <div class="card" role="status">
            <b>
              {store.engine === 'ntfsMft' ? store.fastScanStage() : 'Scan berjalan…'}
            </b>
            {#if store.engine === 'ntfsMft' && store.mftProgress}
              <!-- FER-460: the MFT size is known, so this bar is honest; the
                   regular walk keeps showing counts only (contract §3.5). -->
              <progress
                class="mftBar"
                max={store.mftProgress.total}
                value={store.mftProgress.done}
                aria-label="Progres membaca MFT"
              ></progress>
            {/if}
            <span>{store.countsLine()}</span>
            <button class="btn danger" onclick={() => void store.cancel()}>Cancel</button>
          </div>
        {:else if store.needsRescan}
          <div class="card" role="status">
            <b>Snapshot tidak tersedia.</b>
            <span>Rescan untuk menghitung ulang scope ini.</span>
            <button class="btn" disabled={!canScan} onclick={() => void requestScan()}>
              Rescan
            </button>
          </div>
        {:else if store.rightTab === 'treemap'}
          <div class="tmHost">
            <Treemap
              items={store.treemap?.items ?? []}
              metric={store.metric}
              selectedId={store.selectedNodeId}
              coverage={coverage()}
              scanRootTotal={store.rootNode
                ? store.metric === 'allocated'
                  ? (store.rootNode.subtreeAllocatedBytes ?? undefined)
                  : store.rootNode.subtreeLogicalBytes
                : undefined}
              onselect={(id) => store.selectNode(id)}
              onzoom={(id) => store.zoomIn(id)}
            />
            {#if store.treemap?.loading}
              <div class="loadingNote" role="status">Memuat treemap…</div>
            {/if}
          </div>
        {:else}
          <DiskUsageFiles {store} onreveal={reveal} />
        {/if}
      </div>
    </div>
  </div>
</section>

<style>
  .duws {
    flex: 1;
    min-height: 0;
    min-width: 0;
    display: flex;
    flex-direction: column;
    background: var(--pane);
  }
  /* ── global header ─────────────────────────────────────────────────────── */
  .wsbar {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
    padding: 4px 12px;
    border-bottom: 1px solid var(--line);
    background: color-mix(in oklch, var(--accent) 10%, var(--elev2));
    font-size: calc(12px + var(--uiStep, 0px));
  }
  .wstitle {
    font-weight: 700;
    color: var(--text);
    margin-right: 2px;
  }
  .wshint {
    color: var(--text3);
    font-size: calc(11px + var(--uiStep, 0px));
    margin-left: auto;
  }
  .root {
    display: inline-flex;
    align-items: center;
    gap: 5px;
  }
  .rlab {
    color: var(--text3);
    font-size: calc(11px + var(--uiStep, 0px));
    font-weight: 600;
  }
  .scanRootChip {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 2px 8px;
    border-radius: var(--rl);
    border: 1px solid var(--line2);
    background: var(--elev);
    color: var(--text2);
    font: inherit;
    font-size: calc(11px + var(--uiStep, 0px));
    max-width: 200px;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    cursor: pointer;
  }
  .scanRootChip:hover {
    background: var(--hover);
    color: var(--text);
  }
  .srLab {
    font-weight: 700;
    color: var(--text3);
    text-transform: uppercase;
    font-size: calc(9.5px + var(--uiStep, 0px));
    letter-spacing: 0.05em;
  }
  .upBtn {
    padding: 3px 8px;
    font-weight: 700;
    line-height: 1;
  }
  .root input {
    width: 210px;
  }
  input,
  .btn,
  .seg button,
  .tabs button,
  .crumb,
  .up {
    font: inherit;
    font-size: calc(12px + var(--uiStep, 0px));
  }
  input {
    border: 1px solid var(--line2);
    border-radius: var(--r);
    padding: 2px 7px;
    background: var(--pane);
    color: var(--text);
  }
  .btn {
    border: 1px solid var(--line2);
    border-radius: var(--rl);
    background: var(--pane);
    color: var(--text);
    padding: 3px 10px;
    font-weight: 600;
    cursor: pointer;
  }
  .btn:hover:not(:disabled) {
    background: var(--hover);
  }
  .btn:disabled {
    opacity: 0.45;
    cursor: default;
  }
  .btn.danger:not(:disabled) {
    color: color-mix(in oklch, var(--redInk) 75%, var(--text));
  }
  .btn.primary {
    background: color-mix(in oklch, var(--accent) 18%, var(--pane));
  }
  .sep {
    width: 1px;
    align-self: stretch;
    background: var(--line);
  }
  .seg {
    display: inline-flex;
    border: 1px solid var(--line2);
    border-radius: var(--rl);
    overflow: hidden;
  }
  .seg button {
    border: 0;
    background: var(--pane);
    color: var(--text2);
    padding: 3px 9px;
    cursor: pointer;
    font-weight: 600;
  }
  .seg button.sel {
    background: var(--elev);
    color: var(--accentInk);
  }
  .counts {
    color: var(--text2);
    font-variant-numeric: tabular-nums;
  }
  .chip {
    padding: 1px 9px;
    border-radius: 999px;
    font-size: calc(11px + var(--uiStep, 0px));
    font-weight: 700;
    background: var(--elev);
    color: var(--text2);
    box-shadow: inset 0 0 0 1px var(--line2);
  }
  .chip.ok {
    background: color-mix(in oklch, var(--green) 24%, var(--elev));
    color: var(--text);
  }
  .chip.scan {
    background: color-mix(in oklch, var(--accent) 22%, var(--elev));
    color: var(--text);
  }
  .chip.warn {
    background: color-mix(in oklch, var(--amber) 26%, var(--elev));
    color: var(--text);
  }
  .chip.err {
    background: color-mix(in oklch, var(--red) 28%, var(--elev));
    color: var(--text);
  }
  .engine {
    padding: 1px 7px;
    border-radius: 4px;
    font-size: calc(10px + var(--uiStep, 0px));
    font-weight: 700;
    letter-spacing: 0.04em;
    background: color-mix(in oklch, var(--accent) 22%, var(--elev));
    color: var(--text);
  }
  .notice {
    max-width: 32ch;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: calc(11px + var(--uiStep, 0px));
    color: var(--text2);
  }
  .metricInfo {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 16px;
    height: 16px;
    border-radius: 999px;
    border: 1px solid var(--line2);
    background: var(--elev);
    color: var(--text2);
    font-size: calc(10px + var(--uiStep, 0px));
    font-style: italic;
    font-weight: 700;
    cursor: help;
    user-select: none;
  }
  /* ── session notes (global, full width) ────────────────────────────────── */
  .note {
    padding: 5px 12px;
    font-size: calc(11.5px + var(--uiStep, 0px));
    border-bottom: 1px solid var(--line);
    background: var(--elev);
  }
  .note.warn {
    background: color-mix(in oklch, var(--amber) 16%, var(--pane));
  }
  .note.info {
    background: color-mix(in oklch, var(--accent) 12%, var(--pane));
  }
  .note.err {
    background: color-mix(in oklch, var(--red) 18%, var(--pane));
  }
  .noteBtn {
    border: 1px solid var(--line2);
    border-radius: var(--r);
    background: var(--elev);
    color: var(--text);
    font: inherit;
    font-size: calc(11px + var(--uiStep, 0px));
    font-weight: 600;
    padding: 0 8px;
    margin-left: 8px;
    cursor: pointer;
  }
  .noteBtn:hover {
    background: var(--hover);
  }
  .gatePath {
    color: var(--text3);
  }
  /* ── the dual-pane grid: tree slot A + tabs slot B ─────────────────────── */
  .cols {
    flex: 1;
    min-height: 0;
    display: grid;
    grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
    gap: 1px;
    background: var(--line);
  }
  .slotLeft {
    min-width: 0;
    min-height: 0;
    display: flex;
    background: var(--pane);
  }
  .slotLeft > :global(*) {
    flex: 1;
  }
  .slotRight {
    min-width: 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
    background: var(--pane);
  }
  .tabs {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 4px 10px;
    border-bottom: 1px solid var(--line);
    background: var(--elev2);
  }
  .tabs button {
    border: 0;
    background: none;
    color: var(--text2);
    font-weight: 600;
    padding: 3px 10px;
    border-radius: var(--r);
    cursor: pointer;
  }
  .tabs button.sel {
    background: var(--pane);
    color: var(--text);
    box-shadow: inset 0 0 0 1px var(--line2);
  }
  .crumbs {
    display: flex;
    align-items: center;
    gap: 2px;
    min-width: 0;
    overflow: hidden;
    margin-left: 6px;
  }
  .crumb {
    border: 0;
    background: none;
    color: var(--text2);
    padding: 2px 4px;
    border-radius: var(--r);
    cursor: pointer;
    max-width: 180px;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .crumb.last {
    color: var(--text);
    font-weight: 600;
  }
  .crumb:hover:not(:disabled) {
    background: var(--hover);
  }
  .csep {
    color: var(--text3);
  }
  .zpath {
    margin-left: auto;
    display: inline-flex;
    align-items: center;
    gap: 6px;
    color: var(--text2);
    min-width: 0;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    font-size: calc(11.5px + var(--uiStep, 0px));
  }
  .up {
    border: 1px solid var(--line2);
    border-radius: var(--r);
    background: var(--pane);
    color: var(--text2);
    cursor: pointer;
    padding: 0 7px;
  }
  .up:disabled {
    opacity: 0.45;
    cursor: default;
  }
  .view {
    flex: 1;
    min-height: 0;
    position: relative;
    display: flex;
    flex-direction: column;
  }
  .view > :global(*) {
    flex: 1;
    min-height: 0;
  }
  .tmHost {
    position: relative;
    display: flex;
    flex-direction: column;
  }
  .loadingNote {
    position: absolute;
    right: 8px;
    top: 8px;
    padding: 1px 8px;
    border-radius: 999px;
    background: var(--elev);
    box-shadow: inset 0 0 0 1px var(--line2);
    font-size: calc(10.5px + var(--uiStep, 0px));
    color: var(--text2);
  }
  .mftBar {
    width: min(320px, 70%);
    height: 6px;
    accent-color: var(--accent);
  }
  .card {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 8px;
    text-align: center;
    background: var(--pane);
    color: var(--text2);
    font-size: calc(12.5px + var(--uiStep, 0px));
  }
  .card b {
    color: var(--text);
    font-size: calc(13.5px + var(--uiStep, 0px));
  }
  .card span {
    max-width: 46ch;
  }
</style>
