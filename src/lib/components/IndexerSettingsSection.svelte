<script lang="ts">
  // "Indexer" section of Settings (FER-474, IDX-S1): status of the optional FerrumIndexer Windows
  // service and its privacy mode. Without the service Ferrum keeps working with its own scanning, so
  // every "not available" state is informational only. Changing the mode always goes through the
  // confirmation dialog below, which spells out the privacy consequence; lowering privacy also needs
  // UAC (the service only accepts it from an elevated administrator).
  import { onMount } from 'svelte';
  import {
    indexerSetMode,
    indexerStatus,
    IndexerError,
    UNAVAILABLE,
    readiness,
    squattedNote,
    type IndexerMode,
    type IndexerStatus,
  } from '../ipc/indexer';
  import { ui } from '../uiState.svelte';
  import { folderSizes } from '../stores/folderSizesStore.svelte';

  let status = $state<IndexerStatus>({ ...UNAVAILABLE, problem: null });
  let loading = $state(true);
  let busy = $state(false);
  let message = $state('');
  let pendingMode = $state<IndexerMode | null>(null);
  let understood = $state(false);
  let confirmEl: HTMLElement | undefined = $state();

  const MODE_TEXT: Record<IndexerMode, string> = {
    high_performance: 'High performance (PC dipakai sendiri)',
    shared: 'Bersama (privasi per pengguna)',
  };

  const SCM_TEXT: Record<string, string> = {
    running: 'Berjalan',
    stopped: 'Berhenti',
    paused: 'Dijeda',
    start_pending: 'Sedang dimulai',
    stop_pending: 'Sedang dihentikan',
    pause_pending: 'Sedang dijeda',
    continue_pending: 'Sedang dilanjutkan',
  };

  const ERRORS: Record<string, string> = {
    declined: 'Izin administrator ditolak. Mode tidak berubah.',
    not_running: 'Service FerrumIndexer tidak berjalan.',
    unsupported: 'Mengganti mode butuh aplikasi desktop Ferrum di Windows.',
    not_confirmed: 'Konfirmasi belum diberikan. Mode tidak berubah.',
  };

  /** FER-476: honest index state — not installed, stopped, building (with progress), ready. */
  const indexText = $derived.by(() => {
    if (loading) return '';
    const r = readiness(status);
    if (r === 'not_installed') return 'Tidak terpasang — search dan Disk Usage memakai scan biasa';
    if (r === 'stopped') return 'Service mati — search dan Disk Usage memakai scan biasa';
    if (r === 'degraded')
      return 'Mode terbatas (degraded): token service tidak dibatasi (SID Restricted tidak aktif), jadi search dan Disk Usage dari indeks dimatikan demi keamanan. Ferrum memakai scan biasa. Pasang ulang Ferrum, atau jalankan sc.exe sidtype FerrumIndexer restricted lalu restart service.';
    if (status.indexPaused) return 'Dijeda';
    if (r === 'building') {
      const b = status.volumes.filter((v) => v.state === 'building');
      const pct = b.map((v) => `${v.letter}: ${Math.floor(v.progressPermille / 10)}%`).join(', ');
      return pct ? `Sedang membangun indeks (${pct})` : 'Sedang membangun indeks';
    }
    if (r === 'ready') return 'Siap — search dan Disk Usage memakai indeks';
    return 'Gagal — search dan Disk Usage memakai scan biasa';
  });

  /** Short explanation of the current state when the service can't be used. */
  const note = $derived.by(() => {
    if (loading) return 'Memeriksa service…';
    if (status.problem === 'unsupported')
      return 'Service indexer hanya tersedia di aplikasi desktop Ferrum untuk Windows.';
    const squatted = squattedNote(status);
    if (squatted) return squatted;
    if (status.problem === 'untrusted')
      return 'Tidak tepercaya: pipe FerrumIndexer ada, tetapi server di baliknya bukan service FerrumIndexer yang terpasang (PID, file program, atau pemilik pipe tidak cocok). Ferrum tidak mengirim apa pun ke sana dan memakai scan biasa.';
    if (!status.installed && status.problem === 'location')
      return 'FerrumIndexer tidak dipasang karena Ferrum terpasang di luar Program Files. Demi keamanan, service yang berjalan sebagai SYSTEM hanya dipasang dari folder yang tidak bisa ditulis pengguna biasa. Pasang ulang Ferrum ke Program Files untuk memakainya.';
    if (!status.installed)
      return 'FerrumIndexer belum terpasang. Ferrum tetap bekerja dengan scan biasa. Pasang dengan installer Ferrum (MSI/NSIS) untuk menambahkannya.';
    if (!status.reachable)
      return 'Service terpasang tetapi tidak menjawab. Ferrum memakai scan biasa sampai service berjalan lagi.';
    return '';
  });

  const target = $derived<IndexerMode | null>(
    status.mode === 'shared'
      ? 'high_performance'
      : status.mode === 'high_performance'
        ? 'shared'
        : null,
  );

  async function refresh() {
    loading = true;
    try {
      status = await indexerStatus();
    } finally {
      loading = false;
    }
  }

  onMount(() => {
    void refresh();
  });

  function ask(mode: IndexerMode) {
    message = '';
    understood = false;
    pendingMode = mode;
    queueMicrotask(() => confirmEl?.focus());
  }

  function cancel() {
    pendingMode = null;
  }

  async function apply() {
    const mode = pendingMode;
    if (!mode) return;
    if (mode === 'high_performance' && !understood) return;
    busy = true;
    pendingMode = null;
    try {
      const r = await indexerSetMode(mode, true);
      message = r.changed ? `Mode diganti ke ${MODE_TEXT[r.mode]}.` : 'Mode sudah seperti itu.';
      // FER-477a: sizes (or refusals) of the previous mode must not linger.
      if (r.changed) folderSizes.clear();
    } catch (e) {
      const code = e instanceof IndexerError ? e.code : 'failed';
      message =
        ERRORS[code] ?? `Gagal mengganti mode: ${e instanceof Error ? e.message : String(e)}`;
    } finally {
      busy = false;
      await refresh();
    }
  }

  /** FER-477a: the folder-size column needs the High performance mode. */
  const folderSizeNote = $derived(
    !ui.indexerFolderSizes
      ? ''
      : status.reachable && status.mode === 'shared'
        ? 'Mode Bersama: service tidak memberikan ukuran folder (bisa membocorkan ukuran isi folder yang tidak boleh Anda lihat), jadi kolomnya tetap "—".'
        : status.reachable
          ? ''
          : 'Service tidak berjalan: kolom ukuran folder tetap "—".',
  );

  function toggleFolderSizes(on: boolean) {
    ui.indexerFolderSizes = on;
    folderSizes.clear();
  }

  // Native listener (not Svelte's delegated `onkeydown`), so stopPropagation keeps Escape from
  // also reaching the Settings modal, which would close all of Settings.
  $effect(() => {
    const el = confirmEl;
    if (!el) return;
    el.addEventListener('keydown', onConfirmKey);
    return () => el.removeEventListener('keydown', onConfirmKey);
  });

  function onConfirmKey(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      // Close only this dialog, not the whole Settings modal.
      e.preventDefault();
      e.stopPropagation();
      cancel();
    }
  }
</script>

<div class="idx" id="settings-indexer" data-cat="Indexer">
  <div class="head">
    <div class="name">FerrumIndexer</div>
    <div class="desc">
      Service Windows yang menyiapkan indeks NTFS sehingga search dan Disk Usage bisa menjawab
      seketika. Tanpa service, Ferrum memakai scan biasa.
    </div>
  </div>

  <dl class="facts" data-testid="indexer-status">
    <dt>Service</dt>
    <dd>{status.installed ? 'Terpasang' : 'Tidak terpasang'}</dd>
    {#if status.installed}
      <dt>Status</dt>
      <dd>{SCM_TEXT[status.scmState ?? ''] ?? status.scmState ?? '—'}</dd>
    {/if}
    {#if status.reachable}
      <dt>Versi</dt>
      <dd class="mono">{status.version ?? '—'}</dd>
      <dt>Mode</dt>
      <dd data-testid="indexer-mode">{status.mode ? MODE_TEXT[status.mode] : '—'}</dd>
    {/if}
    {#if indexText}
      <dt>Indeks</dt>
      <dd data-testid="indexer-index">{indexText}</dd>
      {#each status.volumes.filter((v) => v.state === 'error') as v (v.letter)}
        <dt>{v.letter}:</dt>
        <dd class="mono">{v.error}</dd>
      {/each}
    {/if}
  </dl>

  {#if note}<p class="msg" role="status">{note}</p>{/if}

  <div class="row">
    <button onclick={() => void refresh()} disabled={loading || busy}>Periksa lagi</button>
    {#if status.reachable && target}
      <button onclick={() => ask(target)} disabled={busy}>
        {target === 'high_performance' ? 'Ganti ke High performance…' : 'Ganti ke Bersama…'}
      </button>
    {/if}
  </div>

  {#if message}<p class="msg" role="status" data-testid="indexer-message">{message}</p>{/if}

  <label class="ack" data-testid="indexer-folder-sizes">
    <input
      type="checkbox"
      checked={ui.indexerFolderSizes}
      onchange={(e) => toggleFolderSizes((e.currentTarget as HTMLInputElement).checked)}
    />
    Tampilkan ukuran folder di kolom Size (dari indeks, hanya mode High performance)
  </label>
  {#if folderSizeNote}<p class="msg" role="status" data-testid="indexer-folder-sizes-note">
      {folderSizeNote}
    </p>{/if}

  <label class="ack" data-testid="indexer-sync-journal-skip">
    <input
      type="checkbox"
      checked={ui.syncJournalSkip}
      onchange={(e) => (ui.syncJournalSkip = (e.currentTarget as HTMLInputElement).checked)}
    />
    Sync cepat dengan journal indexer (lewati baca isi file yang terbukti tidak berubah)
  </label>
  <p class="msg" data-testid="indexer-sync-journal-skip-risk">
    Hanya untuk sync dua arah "by content". Isi file dilewati kalau USN journal NTFS membuktikan
    foldernya tidak berubah sejak sync bersih terakhir. Perubahan yang tidak tercatat journal (disk
    diubah dari OS lain/dual-boot, alat yang menulis langsung ke disk, kerusakan bit) tidak
    terdeteksi sampai verifikasi penuh berikutnya, paling lama 7 hari. Jumlah file yang dilewati
    ditampilkan di panel Sync. Mati = sync membaca semua isi seperti biasa.
  </p>

  {#if pendingMode}
    <div
      class="confirm"
      role="alertdialog"
      aria-modal="true"
      aria-labelledby="indexer-confirm-title"
      tabindex="-1"
      bind:this={confirmEl}
    >
      {#if pendingMode === 'high_performance'}
        <h4 id="indexer-confirm-title">Aktifkan High performance?</h4>
        <ul>
          <li>
            Service akan mengindeks <strong>semua</strong> file di PC ini, termasuk file di profil akun
            pengguna lain.
          </li>
          <li>
            Hasil search dan Disk Usage tidak lagi difilter per pengguna: siapa pun yang memakai
            Ferrum di PC ini bisa melihat nama file milik akun lain.
          </li>
          <li>
            Ukuran folder dari indeks (kolom Size) ikut aktif dan menghitung semua isi folder,
            termasuk file yang tidak bisa Anda buka.
          </li>
          <li>Pilih ini hanya kalau PC ini dipakai sendiri.</li>
          {#if !status.canLowerPrivacy}
            <li>Windows akan meminta izin administrator (UAC).</li>
          {/if}
        </ul>
        <label class="ack">
          <input type="checkbox" bind:checked={understood} />
          Saya mengerti, PC ini hanya dipakai oleh saya.
        </label>
      {:else}
        <h4 id="indexer-confirm-title">Ganti ke mode Bersama?</h4>
        <ul>
          <li>Hasil akan difilter sesuai hak akses setiap pengguna.</li>
          <li>Profil pengguna lain tidak diindeks.</li>
          <li>Search bisa sedikit lebih lambat dibanding High performance.</li>
        </ul>
      {/if}
      <div class="row end">
        <button class="ghost" onclick={cancel}>Batal</button>
        <button
          class="primary"
          onclick={() => void apply()}
          disabled={busy || (pendingMode === 'high_performance' && !understood)}>Ganti mode</button
        >
      </div>
    </div>
  {/if}
</div>

<style>
  .idx {
    margin-top: 18px;
    padding-top: 14px;
    border-top: 1px solid var(--line);
    display: flex;
    flex-direction: column;
    gap: 8px;
    scroll-margin-top: 8px;
  }
  .name {
    font-size: calc(13px + var(--uiStep, 0px));
    color: var(--text);
  }
  .desc {
    font-size: calc(11.5px + var(--uiStep, 0px));
    color: var(--text3);
  }
  .facts {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: 4px 14px;
    margin: 0;
    font-size: calc(12px + var(--uiStep, 0px));
  }
  dt {
    color: var(--text3);
  }
  dd {
    margin: 0;
    color: var(--text);
  }
  .mono {
    font-family: 'Geist Mono', monospace;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
  }
  .end {
    justify-content: flex-end;
  }
  .msg {
    margin: 0;
    font-size: calc(12px + var(--uiStep, 0px));
    color: var(--text2);
  }
  .confirm {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 12px 14px;
    border-radius: var(--r);
    background: var(--bg);
    box-shadow: 0 0 0 1px var(--accentLine);
    outline: 0;
  }
  h4 {
    margin: 0;
    font-size: calc(13px + var(--uiStep, 0px));
    color: var(--text);
  }
  ul {
    margin: 0;
    padding-left: 18px;
    font-size: calc(12px + var(--uiStep, 0px));
    color: var(--text2);
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .ack {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: calc(12px + var(--uiStep, 0px));
    color: var(--text);
  }
  button {
    height: 26px;
    padding: 0 12px;
    border: 0;
    border-radius: var(--r);
    background: var(--hover2);
    color: var(--text);
    font-size: calc(12px + var(--uiStep, 0px));
    cursor: default;
  }
  button:disabled {
    opacity: 0.5;
  }
  .primary {
    background: var(--accent);
    color: #fff;
  }
  .ghost {
    background: transparent;
    color: var(--text2);
  }
</style>
