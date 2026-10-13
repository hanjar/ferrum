<script lang="ts">
  /**
   * "Archive" section of Settings (FER-505): configuration of the user's Rar.exe
   * for optional RAR archive creation. Ferrum does not bundle WinRAR.
   */
  import { archiveRarToolDetect, archiveRarToolProbe } from '../ipc/archive';
  import { ui } from '../uiState.svelte';
  import { prefs } from '../stores/prefsStore.svelte';

  let probeStatus = $state<{ ok: boolean; message: string } | null>(null);
  let busy = $state(false);

  async function detect() {
    busy = true;
    probeStatus = null;
    try {
      const detected = await archiveRarToolDetect();
      if (detected) {
        ui.rarExePath = detected;
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

  async function probe() {
    busy = true;
    probeStatus = null;
    try {
      // The probe reads rarExePath from prefs on disk (never from the UI), so
      // persist the path being edited first — without the save debounce.
      const saved = await prefs.flush();
      if (!saved) {
        probeStatus = {
          ok: false,
          message: 'Gagal menyimpan pengaturan; path belum bisa dites.',
        };
        return;
      }
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
        Lokasi file Rar.exe (CLI) dari instalasi WinRAR Anda. Biarkan kosong jika tidak ingin
        membuat arsip RAR.
      </div>
    </div>
  </div>

  <div class="fieldRow">
    <input
      id="rar-exe-path"
      class="textInput mono"
      type="text"
      placeholder="C:\Program Files\WinRAR\Rar.exe"
      value={ui.rarExePath ?? ''}
      oninput={(e) => {
        const val = e.currentTarget.value.trim();
        ui.rarExePath = val.length > 0 ? val : null;
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
        onclick={() => void probe()}
        disabled={busy || !ui.rarExePath}
        data-testid="rar-probe-btn"
      >
        Tes
      </button>
    </div>
  </div>

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
