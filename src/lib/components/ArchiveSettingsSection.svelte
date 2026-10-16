<script lang="ts">
  /**
   * "Archive" section of Settings (FER-505): configuration of the user's Rar.exe
   * for optional RAR archive creation. Ferrum does not bundle WinRAR.
   *
   * FER-511: the input below is a DRAFT only — nothing here writes prefs. The path
   * changes solely through `archive_rar_tool_set_path`, which validates the file
   * WITHOUT running it, then pops a NATIVE confirmation dialog (the webview can
   * neither draw it nor answer it) and stores a fingerprint of the confirmed exe.
   * A stored path without (or with a stale) fingerprint never runs: re-confirm.
   */
  import {
    archiveRarToolDetect,
    archiveRarToolProbe,
    archiveRarToolSetPath,
    archiveRarToolStatus,
    type RarToolStatusState,
  } from '../ipc/archive';
  import { ui } from '../uiState.svelte';

  let draftPath = $state('');
  let probeStatus = $state<{ ok: boolean; message: string } | null>(null);
  let confirmState = $state<RarToolStatusState>('unset');
  /** FER-514: the path the backend has stored (shown in the status line, prefills the draft). */
  let storedPath = $state<string | null>(null);
  let draftTouched = false;
  let busy = $state(false);

  const savedPath = $derived(ui.rarExePath);

  $effect(() => {
    void refreshStatus();
  });

  async function refreshStatus(): Promise<void> {
    try {
      const status = await archiveRarToolStatus();
      confirmState = status.state;
      storedPath = status.path;
      // FER-514: show the saved path in the input unless the user is already typing another one.
      if (status.path && !draftTouched && draftPath.trim().length === 0) draftPath = status.path;
    } catch {
      confirmState = 'unset';
      storedPath = null;
    }
  }

  async function detect() {
    busy = true;
    probeStatus = null;
    try {
      // Detection only fills the draft: storing still needs the native confirmation.
      const detected = await archiveRarToolDetect();
      if (detected) {
        draftPath = detected;
        probeStatus = { ok: true, message: `Ditemukan: ${detected}` };
      } else {
        probeStatus = {
          ok: false,
          message: 'Rar.exe tidak ditemukan di lokasi standar Program Files.',
        };
      }
    } catch (e) {
      probeStatus = { ok: false, message: e instanceof Error ? e.message : String(e) };
    } finally {
      busy = false;
    }
  }

  /** "Pakai path ini": the only write path. Empty draft = remove the setting (no dialog). */
  async function applyDraft() {
    busy = true;
    probeStatus = null;
    const value = draftPath.trim();
    try {
      await archiveRarToolSetPath(value.length > 0 ? value : null);
      ui.rarExePath = value.length > 0 ? value : null;
      draftPath = ui.rarExePath ?? '';
      draftTouched = false;
      await refreshStatus();
      probeStatus = {
        ok: true,
        message:
          value.length > 0
            ? 'Path tersimpan. Jalankan "Tes" untuk memverifikasi WinRAR.'
            : 'Pengaturan WinRAR dihapus.',
      };
    } catch (e) {
      // Includes the native dialog's No: nothing was stored.
      probeStatus = { ok: false, message: e instanceof Error ? e.message : String(e) };
    } finally {
      busy = false;
    }
  }

  async function clearSaved() {
    draftPath = '';
    await applyDraft();
  }

  async function probe() {
    busy = true;
    probeStatus = null;
    try {
      // The probe runs the SAVED, CONFIRMED exe from prefs on disk — never the draft.
      const res = await archiveRarToolProbe();
      if (res.ok) {
        probeStatus = {
          ok: true,
          message: res.version ?? 'Rar.exe terverifikasi dan siap digunakan.',
        };
      } else {
        probeStatus = {
          ok: false,
          message: res.reason ?? 'Gagal memverifikasi Rar.exe.',
        };
      }
    } catch (e) {
      probeStatus = { ok: false, message: e instanceof Error ? e.message : String(e) };
    } finally {
      busy = false;
    }
  }
</script>

<div class="archive-section" data-cat="Archive" data-testid="archive-settings-section">
  <div class="subHead">
    <h3>Arsip</h3>
    <p>Pengaturan alat pihak ketiga untuk pembuatan arsip RAR.</p>
  </div>

  <div class="rowSplit">
    <div class="lbl">
      <label for="rar-exe-path" class="name">Path WinRAR (Rar.exe)</label>
      <div class="desc">
        Lokasi file Rar.exe (CLI) dari instalasi WinRAR Anda. Mengubah path memunculkan dialog
        konfirmasi sistem (bukan jendela web), lalu Ferrum memverifikasi file itu sebelum setiap
        eksekusi.
      </div>
    </div>
  </div>

  <div class="fieldRow">
    <input
      id="rar-exe-path"
      class="textInput mono"
      type="text"
      placeholder="C:\Program Files\WinRAR\Rar.exe"
      bind:value={draftPath}
      oninput={() => {
        draftTouched = true;
        probeStatus = null;
      }}
      disabled={busy}
      data-testid="rar-exe-path-input"
    />
    <div class="actions">
      <button
        type="button"
        class="btn"
        onclick={() => void detect()}
        disabled={busy}
        data-testid="rar-detect-btn"
      >
        Cari otomatis
      </button>
      <button
        type="button"
        class="btn primary"
        onclick={() => void applyDraft()}
        disabled={busy || draftPath.trim().length === 0}
        data-testid="rar-apply-btn"
      >
        Pakai path ini
      </button>
      {#if savedPath}
        <button
          type="button"
          class="btn"
          onclick={() => void clearSaved()}
          disabled={busy}
          data-testid="rar-clear-btn"
        >
          Hapus
        </button>
      {/if}
      <button
        type="button"
        class="btn"
        onclick={() => void probe()}
        disabled={busy || confirmState !== 'ready'}
        data-testid="rar-probe-btn"
        title={confirmState === 'ready'
          ? 'Menjalankan Rar.exe tersimpan untuk membaca versinya'
          : 'Tersedia setelah path dikonfirmasi lewat "Pakai path ini"'}
      >
        Tes
      </button>
    </div>
  </div>

  <p
    class="statusMsg"
    role="status"
    data-testid="rar-status"
    class:success={confirmState === 'ready'}
    class:error={confirmState === 'unconfirmed' || confirmState === 'changed'}
  >
    {#if confirmState === 'ready'}
      ✓ Path terkonfirmasi dan siap digunakan: <span class="mono" data-testid="rar-status-path"
        >{storedPath}</span
      >
    {:else if confirmState === 'unconfirmed'}
      ✗ Path tersimpan tetapi BELUM dikonfirmasi (pengaturan lama) — Ferrum tidak akan
      menjalankannya. Klik "Pakai path ini" untuk mengonfirmasi: <span
        class="mono"
        data-testid="rar-status-path">{storedPath}</span
      >
    {:else if confirmState === 'changed'}
      ✗ WinRAR berubah sejak dikonfirmasi; atur ulang di Settings: <span
        class="mono"
        data-testid="rar-status-path">{storedPath}</span
      >
    {:else}
      Belum ada path WinRAR yang diatur; pembuatan arsip RAR nonaktif.
    {/if}
  </p>

  {#if probeStatus}
    <p
      class="statusMsg {probeStatus.ok ? 'success' : 'error'}"
      role="status"
      data-testid="rar-probe-status"
    >
      {probeStatus.ok ? '✓ ' : '✗ '}{probeStatus.message}
    </p>
  {/if}

  <p class="disclaimer">
    Ferrum tidak menyertakan WinRAR. Membuat arsip RAR memakai WinRAR milik Anda; lisensi WinRAR
    menjadi tanggung jawab Anda.
  </p>
</div>

<style>
  .archive-section {
    margin-top: 18px;
    padding-top: 14px;
    border-top: 1px solid var(--line);
    display: flex;
    flex-direction: column;
    gap: 8px;
    scroll-margin-top: 8px;
  }
  .subHead {
    margin: 4px 0 6px;
  }
  .subHead h3 {
    margin: 0;
    font-size: calc(14px + var(--uiStep, 0px));
    font-weight: 600;
    color: var(--text);
  }
  .subHead p {
    margin: 2px 0 0;
    font-size: calc(12px + var(--uiStep, 0px));
    color: var(--text3);
  }
  .rowSplit {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    padding: 6px 0 2px;
  }
  .lbl {
    min-width: 0;
    flex: 1 1 auto;
  }
  .name {
    display: block;
    font-size: calc(13px + var(--uiStep, 0px));
    font-weight: 500;
    color: var(--text);
  }
  .desc {
    font-size: calc(11.5px + var(--uiStep, 0px));
    color: var(--text3);
    margin-top: 2px;
  }
  .fieldRow {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
    align-items: center;
  }
  .textInput {
    flex: 1 1 280px;
    padding: 6px 10px;
    font-size: calc(12px + var(--uiStep, 0px));
    border: 1px solid var(--line);
    border-radius: var(--r);
    background: var(--bg);
    color: var(--text);
    outline: none;
  }
  .textInput:focus {
    border-color: var(--accent);
  }
  .mono {
    font-family: 'Geist Mono', monospace;
  }
  .actions {
    display: flex;
    gap: 6px;
  }
  .btn {
    padding: 6px 12px;
    font-size: calc(12px + var(--uiStep, 0px));
    border: 1px solid var(--line);
    border-radius: var(--r);
    background: var(--hover);
    color: var(--text2);
    cursor: default;
    white-space: nowrap;
  }
  .btn:hover:not(:disabled) {
    background: var(--hover2);
    color: var(--text);
  }
  .btn.primary {
    background: var(--accent);
    color: #fff;
    border-color: transparent;
  }
  .btn.primary:hover:not(:disabled) {
    filter: brightness(1.1);
  }
  .btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
  .statusMsg {
    margin: 2px 0 0;
    font-size: calc(12px + var(--uiStep, 0px));
    border-radius: var(--r);
    padding: 4px 8px;
  }
  .statusMsg.success {
    background: rgba(0, 184, 107, 0.12);
    color: #00b86b;
  }
  .statusMsg.error {
    background: rgba(239, 68, 68, 0.12);
    color: #ef4444;
  }
  .disclaimer {
    margin: 4px 0 0;
    font-size: calc(11px + var(--uiStep, 0px));
    color: var(--text3);
    line-height: 1.4;
  }
</style>
