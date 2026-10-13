<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import Icon from './Icon.svelte';
  import ActivityPrivacyControls from './ActivityPrivacyControls.svelte';
  import ActivityDisclosure from './ActivityDisclosure.svelte';
  import { activity } from '../stores/activityStore.svelte';
  import { activityPage } from '../stores/activityPageStore.svelte';
  import { activePane } from '../stores/panesRegistry.svelte';
  import { chartMax, coverageLabel, fillDays } from '../activityChart';
  import { formatDate, parentOf } from '../format';
  import { shellOpen, type ActivityWindowDays } from '../ipc/activity';

  let { onclose }: { onclose?: () => void } = $props();

  const windows: ActivityWindowDays[] = [7, 30, 90];
  let showPrivacy = $state(false);
  let modalEl: HTMLDivElement | undefined = $state();

  const overview = $derived(activityPage.overview);
  const selected = $derived(activityPage.selected);
  const bars = $derived(overview ? fillDays(overview.daily, activityPage.days, Date.now()) : []);
  const maxBar = $derived(Math.max(1, chartMax(bars)));
  const coverage = $derived(
    overview
      ? coverageLabel(overview.oldestUtcMs, overview.retentionDays, overview.truncated, formatDate)
      : '',
  );

  onMount(() => {
    modalEl?.focus(); // Escape/arrow keys work immediately
    void activity.refresh().catch(() => undefined);
    void activityPage.load();
  });
  // Another window changing privacy state must refresh this view at once.
  let lastEpoch = activity.privacyEpoch;
  $effect(() => {
    const epoch = activity.privacyEpoch;
    if (epoch !== lastEpoch) {
      lastEpoch = epoch;
      void activityPage.load();
    }
  });
  onDestroy(() => activityPage.reset());

  function open(entityKind: 'file' | 'folder', path: string) {
    if (entityKind === 'file') {
      void shellOpen(path, activity.recordingActive).catch(() => undefined);
      return;
    }
    activePane()?.openPath?.(path);
    onclose?.();
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.preventDefault();
      if (activityPage.pending) activityPage.cancelScope();
      else onclose?.();
    }
  }

  function listKey(e: KeyboardEvent) {
    if (!overview || overview.items.length === 0) return;
    const items = overview.items;
    const idx = items.findIndex((i) => i.entity.id === activityPage.selectedId);
    let next = idx;
    if (e.key === 'ArrowDown') next = Math.min(items.length - 1, idx + 1);
    else if (e.key === 'ArrowUp') next = Math.max(0, idx < 0 ? 0 : idx - 1);
    else return;
    e.preventDefault();
    activityPage.select(items[next].entity.id);
    document.getElementById(`act-${items[next].entity.id}`)?.focus();
  }

  const reason = (episodes: number) => (episodes === 1 ? '1 kunjungan' : `${episodes} kunjungan`);

  function excludeTarget(): string | null {
    if (!selected) return null;
    return selected.entity.kind === 'folder'
      ? selected.entity.locator
      : parentOf(selected.entity.locator) || null;
  }
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="scrim" onmousedown={() => onclose?.()}></div>
<div
  class="modal"
  role="dialog"
  aria-modal="true"
  aria-label="Activity"
  tabindex="-1"
  bind:this={modalEl}
  onkeydown={onKey}
>
  <header>
    <h2>Activity</h2>
    <button class="icon" title="Close" aria-label="Close" onclick={() => onclose?.()}>
      <Icon name="close" size={11} />
    </button>
  </header>

  <ActivityDisclosure />

  <div class="toolbar">
    <div class="segmented" role="radiogroup" aria-label="Rentang waktu">
      {#each windows as d (d)}
        <button
          role="radio"
          aria-checked={activityPage.days === d}
          class:sel={activityPage.days === d}
          onclick={() => void activityPage.setDays(d)}>{d} hari</button
        >
      {/each}
    </div>
    <div class="segmented" role="radiogroup" aria-label="Jenis item">
      <button
        role="radio"
        aria-checked={activityPage.kind === 'folder'}
        class:sel={activityPage.kind === 'folder'}
        onclick={() => void activityPage.setKind('folder')}>Folder</button
      >
      <button
        role="radio"
        aria-checked={activityPage.kind === 'file'}
        class:sel={activityPage.kind === 'file'}
        onclick={() => void activityPage.setKind('file')}>File dibuka</button
      >
    </div>
    <div class="segmented" role="radiogroup" aria-label="Urutan">
      <button
        role="radio"
        aria-checked={activityPage.sort === 'recent'}
        class:sel={activityPage.sort === 'recent'}
        onclick={() => void activityPage.setSort('recent')}>Terbaru</button
      >
      <button
        role="radio"
        aria-checked={activityPage.sort === 'frequent'}
        class:sel={activityPage.sort === 'frequent'}
        onclick={() => void activityPage.setSort('frequent')}>Sering</button
      >
    </div>
    <button
      class="privacyToggle"
      aria-expanded={showPrivacy}
      onclick={() => (showPrivacy = !showPrivacy)}>Privasi & retensi</button
    >
  </div>

  {#if activity.privateSession}
    <p class="banner" role="status">
      Sesi privat aktif di jendela ini: tidak ada pencatatan baru, saran riwayat disembunyikan.
      Halaman ini tetap menampilkan riwayat yang tersimpan.
    </p>
  {:else if !activity.recording}
    <p class="banner" role="status">Pencatatan dijeda. Riwayat yang ada tetap tersimpan.</p>
  {/if}

  {#if showPrivacy}
    <section class="privacy"><ActivityPrivacyControls /></section>
  {/if}

  {#if activityPage.error}
    <p class="banner err" role="alert">{activityPage.error}</p>
  {/if}

  <p class="coverage" data-testid="coverage">
    {coverage || 'Aktivitas hanya dari dalam Ferrum, disimpan lokal.'}
  </p>

  {#if overview}
    <div class="chartWrap">
      <div class="chartTitle">
        Item terakhir diakses per hari (UTC) · {overview.totalItems}{overview.truncated ? '+' : ''}
        item · {overview.totalEpisodes} kunjungan
      </div>
      <svg
        class="chart"
        viewBox={`0 0 ${bars.length * 8} 40`}
        preserveAspectRatio="none"
        role="img"
        aria-label={`Grafik jumlah item per hari, ${activityPage.days} hari terakhir`}
      >
        {#each bars as b, i (b.dayUtcMs)}
          {@const h = (b.items / maxBar) * 38}
          <rect x={i * 8 + 1} y={40 - h} width="6" height={h} rx="1">
            <title>{new Date(b.dayUtcMs).toISOString().slice(0, 10)}: {b.items} item</title>
          </rect>
        {/each}
      </svg>
    </div>
  {/if}

  <div class="body">
    <ul
      class="list"
      role="listbox"
      aria-label="Riwayat activity"
      aria-busy={activityPage.loading}
      tabindex="-1"
      onkeydown={listKey}
    >
      {#if overview && overview.items.length === 0}
        <li class="empty">Belum ada aktivitas tercatat di rentang ini.</li>
      {/if}
      {#each overview?.items ?? [] as item (item.entity.id)}
        <li role="presentation">
          <button
            id={`act-${item.entity.id}`}
            role="option"
            aria-selected={activityPage.selectedId === item.entity.id}
            class="row"
            class:sel={activityPage.selectedId === item.entity.id}
            title={item.entity.locator}
            onclick={() => activityPage.select(item.entity.id)}
            ondblclick={() => open(item.entity.kind, item.entity.locator)}
          >
            <span class="name">{item.entity.displayName}</span>
            {#if item.hidden}<span class="tag">disembunyikan</span>{/if}
            <span class="meta">{reason(item.episodeCount)} · {formatDate(item.lastUtcMs)}</span>
          </button>
        </li>
      {/each}
    </ul>

    <aside class="detail" aria-label="Detail item">
      {#if selected}
        <div class="dName">{selected.entity.displayName}</div>
        <dl>
          <dt>Lokasi</dt>
          <dd class="mono">{selected.entity.locator}</dd>
          <dt>Terakhir diakses</dt>
          <dd>{formatDate(selected.lastUtcMs)}</dd>
          <dt>Kunjungan</dt>
          <dd>{selected.episodeCount}</dd>
          {#if selected.score !== null}
            <dt>Hari aktif</dt>
            <dd>{selected.activeDays}</dd>
          {/if}
          <dt>Status</dt>
          <dd>
            Ketersediaan tidak dicek saat ini; belum pernah diakses di Ferrum bukan berarti file
            tidak ada.
          </dd>
        </dl>
        <div class="dActions">
          <button onclick={() => open(selected.entity.kind, selected.entity.locator)}>
            {selected.entity.kind === 'file' ? 'Buka file' : 'Buka folder'}
          </button>
          <button onclick={() => void activityPage.setHidden(selected.entity.id, !selected.hidden)}>
            {selected.hidden ? 'Tampilkan lagi di saran' : 'Sembunyikan dari saran'}
          </button>
          <button
            onclick={() =>
              void activityPage.requestScope(
                'forget',
                selected.entity.displayName,
                selected.entity.id,
              )}>Lupakan item</button
          >
          {#if excludeTarget()}
            <button
              onclick={() =>
                void activityPage.requestScope(
                  'exclude',
                  excludeTarget() ?? '',
                  excludeTarget() ?? '',
                )}>Kecualikan folder</button
            >
          {/if}
        </div>
        <p class="note">
          Sembunyikan: hanya keluar dari saran, statistik tetap. Lupakan: hapus riwayat item ini.
          Kecualikan: folder tidak dicatat lagi. File asli tidak pernah dihapus.
        </p>
        {#if !showPrivacy && activityPage.pending}
          <section class="privacy"><ActivityPrivacyControls /></section>
        {/if}
      {:else}
        <p class="note">Pilih item untuk melihat detail. Panah atas/bawah memindah pilihan.</p>
      {/if}
    </aside>
  </div>
</div>

<style>
  .scrim {
    position: fixed;
    inset: 0;
    background: var(--scrim);
    z-index: 60;
    animation: fmFade 140ms ease-out;
  }
  .modal {
    position: fixed;
    left: 50%;
    top: 50%;
    transform: translate(-50%, -50%);
    width: min(860px, 94vw);
    max-height: 88vh;
    padding: 16px;
    border-radius: calc(var(--rl) + 2px);
    background: var(--elev);
    box-shadow: var(--shadow);
    z-index: 61;
    display: flex;
    flex-direction: column;
    gap: 10px;
    overflow: auto;
    animation: fmUp 160ms cubic-bezier(0.2, 0.7, 0.2, 1);
  }
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }
  h2 {
    margin: 0;
    font-size: 14px;
    font-weight: 600;
  }
  .icon {
    width: 24px;
    height: 24px;
    display: grid;
    place-items: center;
    border: 0;
    border-radius: var(--r);
    background: transparent;
    color: var(--text3);
    cursor: default;
  }
  .icon:hover {
    background: var(--hover2);
    color: var(--text);
  }
  .toolbar {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
    align-items: center;
  }
  .segmented {
    display: flex;
    gap: 2px;
    padding: 2px;
    border-radius: var(--r);
    background: var(--hover);
  }
  .segmented button {
    height: 24px;
    padding: 0 10px;
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
  .privacyToggle {
    margin-left: auto;
    height: 26px;
    padding: 0 10px;
    border: 1px solid var(--line);
    border-radius: var(--r);
    background: transparent;
    color: var(--text2);
    font-size: 12px;
    cursor: default;
  }
  .privacyToggle:hover {
    background: var(--hover);
  }
  .banner {
    margin: 0;
    padding: 6px 8px;
    border-radius: var(--r);
    background: var(--hover);
    font-size: 11.5px;
    color: var(--text2);
  }
  .banner.err {
    border: 1px solid var(--accent);
  }
  .privacy {
    padding: 10px;
    border: 1px solid var(--line);
    border-radius: var(--r);
  }
  .coverage {
    margin: 0;
    font-size: 11.5px;
    color: var(--text3);
  }
  .chartWrap {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .chartTitle {
    font-size: 11px;
    color: var(--text3);
  }
  .chart {
    width: 100%;
    height: 56px;
    fill: var(--accent);
    background: var(--hover);
    border-radius: var(--r);
  }
  .body {
    display: grid;
    grid-template-columns: minmax(0, 1.2fr) minmax(0, 1fr);
    gap: 12px;
    min-height: 180px;
  }
  .list {
    list-style: none;
    margin: 0;
    padding: 0;
    overflow: auto;
    max-height: 320px;
    border: 1px solid var(--line);
    border-radius: var(--r);
  }
  .row {
    width: 100%;
    display: flex;
    align-items: baseline;
    gap: 8px;
    padding: 5px 8px;
    border: 0;
    background: transparent;
    color: var(--text);
    font-size: 12.5px;
    text-align: left;
    cursor: default;
  }
  .row:hover {
    background: var(--hover);
  }
  .row.sel {
    background: var(--hover2);
  }
  .name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .meta {
    flex: none;
    font-size: 11px;
    color: var(--text3);
  }
  .tag {
    flex: none;
    font-size: 10.5px;
    padding: 0 5px;
    border-radius: 8px;
    background: var(--hover);
    color: var(--text3);
  }
  .empty {
    padding: 14px;
    font-size: 12px;
    color: var(--text3);
  }
  .detail {
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 6px;
    font-size: 12px;
    color: var(--text2);
  }
  .dName {
    font-size: 13px;
    font-weight: 600;
    color: var(--text);
  }
  dl {
    margin: 0;
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 2px 10px;
  }
  dt {
    color: var(--text3);
  }
  dd {
    margin: 0;
    min-width: 0;
  }
  .mono {
    font-family: var(--mono, monospace);
    word-break: break-all;
  }
  .dActions {
    display: flex;
    gap: 6px;
    flex-wrap: wrap;
  }
  .dActions button {
    height: 26px;
    padding: 0 10px;
    border: 1px solid var(--line);
    border-radius: var(--r);
    background: transparent;
    color: var(--text2);
    font-size: 12px;
    cursor: default;
  }
  .dActions button:hover {
    background: var(--hover);
  }
  .note {
    margin: 0;
    font-size: 11.5px;
    color: var(--text3);
  }
  @media (max-width: 640px) {
    .body {
      grid-template-columns: minmax(0, 1fr);
    }
  }
</style>
