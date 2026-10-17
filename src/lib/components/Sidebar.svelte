<script lang="ts">
  import { onMount } from 'svelte';
  import ActivityDisclosure from './ActivityDisclosure.svelte';
  import { ui } from '../uiState.svelte';
  import Icon from './Icon.svelte';
  import TreeSidebar from './TreeSidebar.svelte';
  import { sidebarTree } from '../stores/treeStore.svelte';
  import { mockFavorites } from '../mockData';
  import { isInTauri } from '../ipc/tauri';
  import DrivesSection from './DrivesSection.svelte';
  import { activity } from '../stores/activityStore.svelte';
  import { activePane } from '../stores/panesRegistry.svelte';
  import { shellOpen, type ActivityFavoriteDto, type ActivityItemDto } from '../ipc/activity';
  import FileIcon from './FileIcon.svelte';
  import { kindOf } from '../kind';
  import { automationUiEnabled } from '../featureFlags';
  import { automation } from '../stores/automationStore.svelte';

  // Favorites collapse (ACT-05): show the first five pins; the rest hide
  // behind "Tampilkan semua" with an internal scroll when expanded.
  const FAVORITES_PREVIEW = 5;
  let favExpanded = $state(false);
  // The shared singleton: FilePane's tree-follow feeds this same instance
  // (expand-to-active-path), and the tree state survives flipping between
  // Places and Tree (F2-15 mode lives in the shell state).
  const tree = sidebarTree;
  // The mock Favorites fixture shows in browser dev only; on desktop the real
  // Favorites/Recent/Frequent come from the activity store. Drives are always
  // real (DrivesSection, FER-496).
  // Until the first activity refresh settles, desktop shows "Memuat…"; if the
  // backend is still unavailable afterwards, it shows "Favorites tidak tersedia".
  let favLoading = $state(true);

  onMount(() => {
    void activity
      .refresh()
      .catch(() => undefined)
      .finally(() => {
        favLoading = false;
      });
    // Privacy changes made in other windows invalidate these lists at once.
    void activity.startSync();
    // MH-9: the Automation badge needs the list; only when the (default-off) automation UI is on.
    if (automationUiEnabled && isInTauri()) void automation.load();
  });

  function openActivityItem(item: ActivityItemDto) {
    const pane = activePane();
    if (pane) pane.openPath?.(item.entity.locator);
  }

  function openFavorite(favorite: ActivityFavoriteDto) {
    if (favorite.kind === 'file') {
      // Pinned file: explicit OS open through the default association; the
      // backend records the OpenAccepted receipt only after acceptance.
      void shellOpen(favorite.path, activity.recordingActive).catch(() => undefined);
      return;
    }
    const pane = activePane();
    if (pane) pane.openPath?.(favorite.path);
  }

  function unpinFavorite(favorite: ActivityFavoriteDto) {
    void activity.toggleFavorite(favorite.path, favorite.kind).catch(() => undefined);
  }

  function hideItem(item: ActivityItemDto) {
    void activity.hideSuggestion(item.entity.id).catch(() => undefined);
  }

  const reasonOf = (item: ActivityItemDto): string =>
    item.episodeCount > 1 ? `${item.episodeCount} kunjungan` : '1 kunjungan';

  const visibleFavorites = $derived(
    favExpanded ? activity.favorites : activity.favorites.slice(0, FAVORITES_PREVIEW),
  );
</script>

<aside class="sidebar">
  <div class="modeSwitch">
    <button class:sel={ui.sidebarMode === 'places'} onclick={() => (ui.sidebarMode = 'places')}>
      <Icon name="star" size={13} />
      Places
    </button>
    <button class:sel={ui.sidebarMode === 'tree'} onclick={() => (ui.sidebarMode = 'tree')}>
      <Icon name="tree" size={13} />
      Tree
    </button>
  </div>

  {#if ui.sidebarMode === 'tree'}
    <TreeSidebar {tree} />
  {/if}

  {#if ui.sidebarMode === 'places'}
    <nav class="places" aria-label="Places">
      {#if !activity.available}
        <div class="section">
          <div class="sectionTitle">Favorites</div>
          {#if isInTauri()}
            <div class="favEmpty" role="status">
              {favLoading ? 'Memuat…' : 'Favorites tidak tersedia'}
            </div>
          {:else}
            {#each mockFavorites as it (it.label)}
              <button class="item">
                <span class="row">
                  <span class="itemIcon"><Icon name={it.icon} size={15} /></span>
                  <span class="label">{it.label}</span>
                </span>
              </button>
            {/each}
          {/if}
        </div>
      {/if}
      <DrivesSection />
      {#if activity.available}
        <ActivityDisclosure onsettings={() => (ui.settingsOpen = true)} />
        {#if activity.privateSession}
          <div class="favEmpty" role="status">Sesi privat: saran riwayat disembunyikan</div>
        {/if}
        <div class="section">
          <div class="sectionTitle">Favorites</div>
          {#if activity.favorites.length === 0}
            <div class="favEmpty">Ctrl+Shift+D untuk pin folder aktif</div>
          {:else}
            <div class="favList" class:expanded={favExpanded}>
              {#each visibleFavorites as fav (fav.path)}
                <div class="favRow">
                  <button class="item" title={fav.path} onclick={() => openFavorite(fav)}>
                    <span class="row">
                      <span class="itemIcon">
                        {#if fav.kind === 'file'}
                          <FileIcon
                            kind={kindOf({ name: fav.label, isDir: false })}
                            name={fav.label}
                            size={15}
                          />
                        {:else}
                          <Icon name="star" size={15} />
                        {/if}
                      </span>
                      <span class="label">{fav.label}</span>
                    </span>
                  </button>
                  <button
                    class="unpin"
                    title={`Lepas pin ${fav.label}`}
                    aria-label={`Lepas pin ${fav.label}`}
                    onclick={() => unpinFavorite(fav)}
                  >
                    <Icon name="close" size={10} />
                  </button>
                </div>
              {/each}
            </div>
            {#if activity.favorites.length > FAVORITES_PREVIEW}
              <button class="favToggle" onclick={() => (favExpanded = !favExpanded)}>
                {favExpanded
                  ? 'Tampilkan lebih sedikit'
                  : `Tampilkan semua (${activity.favorites.length})`}
              </button>
            {/if}
          {/if}
        </div>
        {#if !activity.privateSession}
          <div class="section" role="group" aria-label="Recent">
            <div class="sectionTitle">Recent</div>
            {#if activity.recent.length === 0}
              <div class="favEmpty" role="status">Belum ada folder yang baru dibuka</div>
            {/if}
            {#each activity.recent as item (item.entity.id)}
              <div class="favRow">
                <button
                  class="item"
                  title={item.entity.locator}
                  onclick={() => openActivityItem(item)}
                >
                  <span class="row">
                    <span class="itemIcon"><Icon name="clock" size={15} /></span>
                    <span class="label">{item.entity.displayName}</span>
                    <span class="meta">{reasonOf(item)}</span>
                  </span>
                </button>
                <button
                  class="unpin"
                  title={`Sembunyikan ${item.entity.displayName} dari saran`}
                  aria-label={`Sembunyikan ${item.entity.displayName} dari saran`}
                  onclick={() => hideItem(item)}
                >
                  <Icon name="close" size={10} />
                </button>
              </div>
            {/each}
          </div>
        {/if}
        {#if !activity.privateSession}
          <div class="section" role="group" aria-label="Frequent">
            <div class="sectionTitle">Frequent</div>
            {#if activity.frequent.length === 0}
              <div class="favEmpty" role="status">Belum ada folder yang sering dibuka</div>
            {/if}
            {#each activity.frequent as item (item.entity.id)}
              <div class="favRow">
                <button
                  class="item"
                  title={`${item.entity.locator} · ${item.episodeCount} kunjungan / ${item.activeDays} hari`}
                  onclick={() => openActivityItem(item)}
                >
                  <span class="row">
                    <span class="itemIcon"><Icon name="star" size={15} /></span>
                    <span class="label">{item.entity.displayName}</span>
                    <span class="meta">{reasonOf(item)}</span>
                  </span>
                </button>
                <button
                  class="unpin"
                  title={`Sembunyikan ${item.entity.displayName} dari saran`}
                  aria-label={`Sembunyikan ${item.entity.displayName} dari saran`}
                  onclick={() => hideItem(item)}
                >
                  <Icon name="close" size={10} />
                </button>
              </div>
            {/each}
          </div>
        {/if}
        <button class="favToggle" onclick={() => (ui.activityOpen = true)}>
          Lihat semua Activity…
        </button>
      {:else}
        <div class="section" role="group" aria-label="Activity">
          <div class="sectionTitle">Activity</div>
          <div class="favEmpty" role="status">
            Riwayat Activity tidak tersedia (offline). Recent, Frequent, dan Favorites
            disembunyikan.
          </div>
        </div>
      {/if}
      <div class="section" role="group" aria-label="Sync & Backup">
        <div class="sectionTitle">Sync &amp; Backup</div>
        <button class="item" onclick={() => (ui.syncProfilesOpen = true)}>
          <span class="row">
            <span class="itemIcon"><Icon name="queue" size={15} /></span>
            <span class="label">Sync profiles</span>
          </span>
        </button>
        {#if automationUiEnabled}
          <button class="item" onclick={() => (ui.automationOpen = true)}>
            <span class="row">
              <span class="itemIcon"><Icon name="clock" size={15} /></span>
              <span class="label">Automation</span>
              {#if automation.attentionCount > 0}
                <span
                  class="autoBadge"
                  data-testid="automation-badge"
                  aria-label="{automation.attentionCount} need attention"
                >
                  {automation.attentionCount}
                </span>
              {/if}
            </span>
          </button>
        {:else}
          <button
            class="item"
            disabled
            aria-disabled="true"
            title="Automation arrives in a later release"
          >
            <span class="row">
              <span class="itemIcon"><Icon name="clock" size={15} /></span>
              <span class="label">Automation</span>
            </span>
          </button>
        {/if}
      </div>
    </nav>
  {/if}
</aside>

<style>
  .sidebar {
    width: 208px;
    flex: none;
    background: var(--titlebar);
    border-right: 1px solid var(--line);
    display: flex;
    flex-direction: column;
    min-height: 0;
  }
  .modeSwitch {
    display: flex;
    gap: 2px;
    padding: 2px;
    margin: 8px 8px 4px;
    border-radius: var(--r);
    background: var(--hover2);
  }
  .modeSwitch button {
    flex: 1;
    height: 22px;
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 6px;
    border: 0;
    border-radius: calc(var(--r) - 2px);
    font-size: calc(12px + var(--uiStep, 0px));
    background: transparent;
    color: var(--text3);
    cursor: default;
    transition:
      background-color 140ms,
      color 140ms;
  }
  .modeSwitch button.sel {
    background: var(--elev);
    box-shadow: var(--tabShadow);
    color: var(--text);
  }
  .places {
    flex: 1;
    min-height: 0;
    overflow: auto;
    display: flex;
    flex-direction: column;
    gap: 12px;
    padding: 0 8px 14px;
    animation: fmFade 140ms ease-out;
  }
  .section {
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .sectionTitle {
    padding: 6px 8px 4px;
    font-size: calc(11px + var(--uiStep, 0px));
    font-weight: 500;
    color: var(--text3);
  }
  .item {
    display: flex;
    flex-direction: column;
    gap: 5px;
    padding: 5px 8px;
    border: 0;
    border-radius: var(--r);
    background: transparent;
    text-align: left;
    cursor: default;
  }
  .item:hover {
    background: var(--hover);
  }
  .item:disabled {
    opacity: 0.5;
  }
  .item:disabled:hover {
    background: transparent;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
    color: var(--text2);
  }
  .itemIcon {
    display: grid;
    place-items: center;
    flex: none;
  }
  .label {
    flex: 1;
    min-width: 0;
    font-size: calc(12.5px + var(--uiStep, 0px));
    color: var(--text);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .meta {
    font-family: 'Geist Mono', monospace;
    font-size: calc(10.5px + var(--uiStep, 0px));
    color: var(--text3);
  }
  .favRow {
    display: flex;
    align-items: center;
    border-radius: var(--r);
  }
  .favList {
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .favList.expanded {
    max-height: 232px;
    overflow-y: auto;
  }
  .favToggle {
    border: 0;
    background: transparent;
    padding: 4px 8px;
    font-size: 11.5px;
    color: var(--text3);
    text-align: left;
    cursor: default;
    border-radius: var(--r);
  }
  .favToggle:hover {
    background: var(--hover);
    color: var(--text);
  }
  .favRow .item {
    flex: 1;
    min-width: 0;
  }
  .favRow:hover {
    background: var(--hover);
  }
  .unpin {
    flex: none;
    width: 20px;
    height: 20px;
    margin-right: 6px;
    display: grid;
    /* Never display:none — a hidden button cannot receive keyboard focus.
       Visually revealed on row hover/focus-within, but always in tab order. */
    opacity: 0;
    place-items: center;
    border: 0;
    border-radius: var(--r);
    background: transparent;
    color: var(--text3);
    cursor: default;
  }
  .favRow:hover .unpin,
  .favRow:focus-within .unpin,
  .unpin:focus-visible {
    opacity: 1;
  }
  .unpin:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: -2px;
  }
  .unpin:hover {
    background: var(--hover2);
    color: var(--text);
  }
  .favEmpty {
    padding: 5px 8px;
    font-size: 11.5px;
    color: var(--text3);
  }
  .autoBadge {
    margin-left: auto;
    min-width: 16px;
    padding: 0 5px;
    border-radius: 8px;
    background: var(--redSoft);
    color: var(--redInk);
    font-size: 11px;
    font-weight: 600;
    text-align: center;
  }
</style>
