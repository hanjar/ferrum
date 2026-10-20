<script lang="ts">
  /**
   * Privacy controls shared by Settings and the Activity page. Each control
   * has a distinct, stated meaning; every destructive scope previews what it
   * would remove and only runs on explicit confirm.
   */
  import { activity } from '../stores/activityStore.svelte';
  import { activityPage } from '../stores/activityPageStore.svelte';
  import { activePane } from '../stores/panesRegistry.svelte';
  import type { RetentionDays } from '../ipc/activity';
  import { dialog, roving } from '../dialogKit';

  const choices: RetentionDays[] = [7, 30, 90];
  let busy = $state(false);

  const pending = $derived(activityPage.pending);

  async function guard(run: () => Promise<unknown>) {
    busy = true;
    try {
      await run();
    } catch (e) {
      console.error('[activity] privacy control failed', e);
    } finally {
      busy = false;
    }
  }

  function chooseRetention(days: RetentionDays) {
    if (days === activity.retentionDays) return;
    if (days > activity.retentionDays) {
      void guard(() => activity.setRetention(days)); // keeping more deletes nothing
      return;
    }
    void guard(() => activityPage.requestScope('retention', `retensi ${days} hari`, String(days)));
  }

  function excludeActive() {
    const path = activePane()?.path;
    if (!path) return;
    void guard(() => activityPage.requestScope('exclude', path, path));
  }

  function scopeTitle(scope: string): string {
    switch (scope) {
      case 'all':
        return 'Hapus semua riwayat activity';
      case 'forget':
        return 'Lupakan item ini';
      case 'exclude':
        return 'Kecualikan folder dari pencatatan';
      default:
        return 'Perpendek retensi';
    }
  }

  function scopeEffect(scope: string): string {
    switch (scope) {
      case 'exclude':
        return 'Folder dan subfoldernya tidak dicatat lagi; riwayat yang sudah ada di dalamnya dihapus.';
      case 'retention':
        return 'Riwayat lebih lama dari batas baru dihapus permanen dari database lokal.';
      case 'forget':
        return 'Riwayat dan statistik item ini dihapus. Tanda sembunyi ikut hilang.';
      default:
        return 'Semua riwayat dan statistik dihapus. Pengaturan tetap.';
    }
  }

  // FER-577 (B3): the confirmation is a real modal step. Focus goes to the safe "Batal", Tab stays
  // inside, Esc cancels ONLY this confirmation (it must not close Settings behind it) and focus
  // returns to the button that asked.
  const confirmOpts = {
    initial: '[data-autofocus]',
    onEscape: () => activityPage.cancelScope(),
    mnemonics: false,
    isolate: true,
  };

  async function confirm() {
    await guard(() => activityPage.confirmScope());
  }
</script>

<div class="controls">
  <label class="check">
    <input
      type="checkbox"
      checked={activity.recording}
      disabled={busy}
      onchange={(e) =>
        void guard(() => activity.setRecording((e.currentTarget as HTMLInputElement).checked))}
    />
    Catat folder & file yang diakses di Ferrum
  </label>
  <p class="note">
    Jeda: mematikan pencatatan baru di semua jendela. Riwayat yang ada tetap tersimpan.
  </p>

  <label class="check">
    <input
      type="checkbox"
      checked={activity.privateSession}
      onchange={(e) => activity.setPrivateSession((e.currentTarget as HTMLInputElement).checked)}
    />
    Sesi privat (hanya jendela ini)
  </label>
  <p class="note">
    Jendela ini tidak mencatat apa pun dan tidak menampilkan saran dari riwayat. Jendela lain tidak
    terpengaruh; hilang saat jendela ditutup.
  </p>

  <div class="rowTitle" id="retention-label">Retensi riwayat</div>
  <!-- select: false - arrowing onto a shorter retention must not open the delete confirmation. -->
  <div
    class="segmented"
    role="radiogroup"
    aria-labelledby="retention-label"
    use:roving={{ select: false }}
  >
    {#each choices as d (d)}
      <button
        role="radio"
        aria-checked={activity.retentionDays === d}
        class:sel={activity.retentionDays === d}
        disabled={busy}
        onclick={() => chooseRetention(d)}
      >
        {d} hari
      </button>
    {/each}
  </div>
  <p class="note">
    Disimpan lokal di perangkat ini, tidak pernah dikirim ke mana pun. Memperpendek retensi
    menghapus riwayat lama setelah Anda konfirmasi.
  </p>

  <div class="rowTitle">Hapus</div>
  <div class="privacyRow">
    <button class="privacyBtn" disabled={busy || !activePane()?.path} onclick={excludeActive}>
      Kecualikan folder aktif
    </button>
    <button
      class="privacyBtn danger"
      disabled={busy}
      onclick={() => void guard(() => activityPage.requestScope('all', 'semua riwayat'))}
    >
      Hapus semua riwayat activity
    </button>
  </div>
  <p class="note">
    Sembunyikan dan Lupakan per item ada di halaman Activity. Hapus/kecualikan tidak menghapus file
    asli maupun Favorites.
  </p>

  {#if pending}
    <div
      class="scope"
      role="alertdialog"
      aria-labelledby="scope-title"
      aria-describedby="scope-desc"
      aria-modal="true"
      tabindex="-1"
      use:dialog={confirmOpts}
    >
      <div class="scopeTitle" id="scope-title">{scopeTitle(pending.scope)}</div>
      <div id="scope-desc" aria-live="polite">
        {#if pending.busy || pending.preview === null}
          {pending.busy ? 'Menghitung cakupan…' : 'Pratinjau tidak tersedia.'}
        {:else}
          {#if pending.refreshed}
            <strong>Data berubah di jendela lain; cakupan dihitung ulang.</strong>
          {/if}
          <p class="scopeCount">
            Target: <span class="mono">{pending.label}</span><br />
            {pending.preview.items}{pending.preview.truncated ? '+' : ''} item ·
            {pending.preview.episodes} kunjungan akan dihapus dari riwayat.
          </p>
          {#if pending.preview.sample.length > 0}
            <ul class="sample">
              {#each pending.preview.sample as s (s)}<li class="mono">{s}</li>{/each}
            </ul>
          {/if}
          <p class="note">
            {scopeEffect(pending.scope)} Favorites ({pending.preview.favoritesKept}) dan file asli
            tidak tersentuh.
          </p>
        {/if}
      </div>
      <div class="privacyRow">
        <button class="privacyBtn" data-autofocus onclick={() => activityPage.cancelScope()}>
          Batal
        </button>
        <button
          class="privacyBtn danger armed"
          disabled={busy || pending.busy || pending.preview === null}
          onclick={() => void confirm()}
        >
          Konfirmasi
        </button>
      </div>
    </div>
  {/if}
</div>

<style>
  .controls {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .check {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 12.5px;
    color: var(--text2);
  }
  .note {
    margin: 2px 0 8px;
    font-size: 11.5px;
    color: var(--text3);
  }
  .rowTitle {
    font-size: 11px;
    font-weight: 500;
    color: var(--text3);
    text-transform: uppercase;
    letter-spacing: 0.06em;
    margin: 4px 0 4px;
  }
  .segmented {
    display: flex;
    gap: 2px;
    padding: 2px;
    border-radius: var(--r);
    background: var(--hover);
  }
  .segmented button {
    flex: 1;
    height: 26px;
    border: 0;
    border-radius: calc(var(--r) - 2px);
    background: transparent;
    color: var(--text3);
    font-size: 12px;
    cursor: default;
  }
  .segmented button.sel {
    background: var(--elev);
    box-shadow: var(--tabShadow);
    color: var(--text);
  }
  .privacyRow {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
  }
  .privacyBtn {
    height: 26px;
    padding: 0 10px;
    border: 1px solid var(--line);
    border-radius: var(--r);
    background: transparent;
    color: var(--text2);
    font-size: 12px;
    cursor: default;
  }
  .privacyBtn:hover:not(:disabled) {
    background: var(--hover);
  }
  .privacyBtn:disabled {
    opacity: 0.5;
  }
  .privacyBtn.danger.armed {
    border-color: var(--accent);
    color: var(--text);
  }
  .scope {
    margin-top: 6px;
    padding: 10px;
    border: 1px solid var(--accent);
    border-radius: var(--r);
    background: var(--hover);
    font-size: 12px;
    color: var(--text2);
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .scopeTitle {
    font-weight: 600;
    color: var(--text);
  }
  .scopeCount {
    margin: 4px 0;
  }
  .sample {
    margin: 0 0 4px;
    padding-left: 16px;
    font-size: 11px;
    color: var(--text3);
    max-height: 70px;
    overflow: auto;
  }
  .mono {
    font-family: var(--mono, monospace);
    word-break: break-all;
  }
</style>
