<script lang="ts">
  import { untrack } from 'svelte';
  import Icon from './Icon.svelte';
  import { ui } from '../uiState.svelte';
  import {
    actions,
    getAction,
    isEnabled,
    labelOf,
    runAction,
    searchActions,
    shortcutFor,
    shortcutLabel,
    type Action,
  } from '../actions';
  import {
    activitySearch,
    shellOpen,
    type ActivityEntityDto,
    type ActivityFavoriteDto,
    type ActivityMatchDto,
  } from '../ipc/activity';
  import type { ReasonCode } from '../activity/contract';
  import { isInTauri } from '../ipc/tauri';
  import { getPane } from '../stores/panesRegistry.svelte';
  import { activity } from '../stores/activityStore.svelte';
  import { parentOf } from '../format';
  import type { PaneId } from '../uiState.svelte';
  import { hotlist } from '../stores/hotlistStore.svelte';
  import { flattenHotlist, matchHotlist, type PaletteHotItem } from '../hotlist/paletteItems';
  import { dialog, roving } from '../dialogKit';
  import { recentCommands } from '../stores/recentCommands.svelte';

  type PaletteMode = 'all' | 'folder' | 'file' | 'command';

  const MODES: { id: PaletteMode; label: string }[] = [
    { id: 'all', label: 'Semua' },
    { id: 'folder', label: 'Folder' },
    { id: 'file', label: 'File' },
    { id: 'command', label: 'Perintah' },
  ];

  let query = $state('');
  let index = $state(0);
  let inputEl = $state<HTMLInputElement | undefined>();
  let mode = $state<PaletteMode>('all');
  // Pane target snapshot (spec §3): Enter acts on the pane that was active
  // when the palette opened, even if focus/active pane changes afterwards.
  let targetPaneId = $state<PaneId>(0);

  const results = $derived(searchActions(query, actions));
  // Phase-4 provider: locations previously accessed in Ferrum, labeled as
  // history — never presented as filesystem-wide search results.
  let locations = $state<ActivityMatchDto[]>([]);
  let locationsStale = $state(false);
  let searchSeq = 0;

  type FlatItem = {
    group: 'rcmd' | 'fav' | 'recent' | 'frequent' | 'hot' | 'loc' | 'cmd';
    id: string;
    label: string;
    sub: string;
    kbd?: string;
    /** Small tag before the sub text (hotlist entries). */
    badge?: string;
    /** FER-467 R8: opening it contacts another computer (UNC / WebDAV). */
    network?: boolean;
    /** Parent folder, used only to tell apart identically named results. */
    parent?: string;
    activate: () => void;
  };

  /** Last `n` path segments of a folder path, either separator style. */
  function tailSegments(path: string, n = 2): string {
    const parts = path.split(/[\\/]+/).filter(Boolean);
    return parts.slice(-n).join('\\');
  }

  // FER-481: the Ctrl+D hotlist, flattened (reactive to hotlist.doc). No new IPC.
  const hotEntries = $derived(flattenHotlist(hotlist.doc.items));
  const HOT_QUOTA = 8;

  function hotItem(e: PaletteHotItem): FlatItem {
    return {
      group: 'hot',
      id: `hot:${e.id}`,
      label: e.breadcrumb,
      sub: e.detail,
      badge: 'Hotlist',
      network: e.network,
      // Same paths as the Ctrl+D menu: folders open in the active pane (network hosts are asked
      // about once), commands go through hotlist_run_command by their place in the tree (the
      // backend validates and shows the native confirmation).
      activate: () => {
        ui.paletteOpen = false;
        hotlist.activate(e.item, e.position);
      },
    };
  }

  // Query mode: commands + history locations (mode-filtered).
  const searchItems = $derived.by((): FlatItem[] => {
    const items: FlatItem[] = [];
    if (mode !== 'folder' && mode !== 'file') {
      for (const a of results) {
        items.push({
          group: 'cmd',
          id: `cmd:${a.id}`,
          label: labelOf(a),
          sub: a.group,
          kbd: shortcutFor(a) ? shortcutLabel(shortcutFor(a)!) : undefined,
          activate: () => run(a),
        });
      }
    }
    if (mode !== 'file') {
      const pool =
        mode === 'all'
          ? hotEntries
          : hotEntries.filter((e) => e.kind === (mode === 'folder' ? 'dir' : 'command'));
      for (const e of matchHotlist(pool, query).slice(0, HOT_QUOTA)) items.push(hotItem(e));
    }
    if (mode !== 'command') {
      const shown = mode === 'all' ? locations : locations.filter((l) => l.entity.kind === mode);
      for (const loc of shown) {
        items.push({
          group: 'loc',
          id: `loc:${loc.entity.id}`,
          label: loc.entity.displayName,
          parent: loc.entity.parentPath,
          sub: `${loc.entity.kind === 'file' ? 'File · ' : ''}${matchReasons(loc)} · ${loc.entity.parentPath}`,
          activate: () => openLocation(loc),
        });
      }
    }
    return items;
  });

  /** FER-580: the stored recent ids that exist and can run right now (newest first, max 3). */
  function recentActions(): Action[] {
    const out: Action[] = [];
    for (const id of recentCommands.ids) {
      const a = getAction(id);
      if (a && isEnabled(a)) out.push(a);
    }
    return out;
  }

  // Empty query (spec §3): labeled groups with limited quotas, favorite-first.
  const EMPTY_QUOTA = { fav: 3, recent: 3, frequent: 3, cmd: 8 } as const;
  const emptyItems = $derived.by((): FlatItem[] => {
    const items: FlatItem[] = [];
    // FER-580: the commands run last, above everything else, only with a truly empty input.
    if (query.trim() === '') {
      for (const a of recentActions()) {
        items.push({
          group: 'rcmd',
          id: `rcmd:${a.id}`,
          label: labelOf(a),
          sub: a.group,
          kbd: shortcutFor(a) ? shortcutLabel(shortcutFor(a)!) : undefined,
          activate: () => run(a),
        });
      }
    }
    for (const fav of activity.favorites.slice(0, EMPTY_QUOTA.fav)) {
      items.push({
        group: 'fav',
        id: `fav:${fav.path}`,
        label: fav.label,
        parent: parentOf(fav.path),
        sub: fav.kind === 'file' ? 'File · Favorite' : 'Folder · Favorite',
        activate: () => activateFavorite(fav),
      });
    }
    for (const item of activity.recent.slice(0, EMPTY_QUOTA.recent)) {
      items.push({
        group: 'recent',
        id: `rec:${item.entity.id}`,
        label: item.entity.displayName,
        parent: item.entity.parentPath,
        sub: 'Recent · ' + item.entity.parentPath,
        activate: () => navigateEntity(item.entity),
      });
    }
    for (const item of activity.frequent.slice(0, EMPTY_QUOTA.frequent)) {
      items.push({
        group: 'frequent',
        id: `frq:${item.entity.id}`,
        label: item.entity.displayName,
        parent: item.entity.parentPath,
        sub: `Frequent · ${item.episodeCount} kunjungan / ${item.activeDays} hari`,
        activate: () => navigateEntity(item.entity),
      });
    }
    for (const e of hotEntries.slice(0, HOT_QUOTA)) items.push(hotItem(e));
    for (const a of results.slice(0, EMPTY_QUOTA.cmd)) {
      items.push({
        group: 'cmd',
        id: `cmd:${a.id}`,
        label: labelOf(a),
        sub: a.group,
        kbd: shortcutFor(a) ? shortcutLabel(shortcutFor(a)!) : undefined,
        activate: () => run(a),
      });
    }
    return items;
  });

  const baseItems = $derived(query.trim().length >= 2 ? searchItems : emptyItems);

  // Disambiguation (FER-258): results that share a name (case-insensitive)
  // get a distinguishing parent hint. Two segments (parent + grandparent) are
  // used so "Reports" under two different "Projects" folders still differ.
  const disambiguation = $derived.by((): Record<string, string> => {
    const byName: Record<string, FlatItem[]> = Object.create(null);
    for (const it of baseItems) {
      if (it.group === 'cmd' || !it.parent) continue;
      const key = it.label.toLowerCase();
      (byName[key] ??= []).push(it);
    }
    const hints: Record<string, string> = {};
    for (const list of Object.values(byName)) {
      if (list.length < 2) continue;
      for (const it of list) hints[it.id] = tailSegments(it.parent!, 2);
    }
    return hints;
  });
  const flatItems = $derived(baseItems);
  const total = $derived(flatItems.length);
  const showChips = $derived(query.trim().length >= 2);

  // FER-577: the element that had focus before the palette opened. Captured before the DOM
  // changes and handed back BEFORE an action runs, so a dialog the action opens remembers a live
  // element (not the palette input that is about to disappear) as its own return target.
  let opener: HTMLElement | null = null;
  $effect.pre(() => {
    if (!ui.paletteOpen) return;
    const a = untrack(() => document.activeElement);
    opener = a instanceof HTMLElement && a !== document.body ? a : null;
  });
  function restoreFocus(): void {
    if (opener?.isConnected) opener.focus();
    opener = null;
  }
  function closePalette(): void {
    ui.paletteOpen = false;
    restoreFocus();
  }

  $effect(() => {
    if (ui.paletteOpen) {
      query = '';
      index = 0;
      selectedId = null;
      // Snapshot ONCE at open: untracked so a later active-pane change does
      // not re-run this effect (which would reset the query and retarget).
      targetPaneId = untrack(() => ui.activePane);
      void activity.refresh().catch(() => undefined);
      queueMicrotask(() => inputEl?.focus());
    }
  });

  $effect(() => {
    const q = query.trim();
    if (!ui.paletteOpen || mode === 'command' || q.length < 2 || !isInTauri()) {
      locations = [];
      locationsStale = false;
      return;
    }
    const scope = mode === 'all' ? 'all' : mode;
    // Cancellation (FER-259): the backend has NO cancel — an `invoke` that
    // started always runs to completion. Cancellation is client-side only:
    // each keystroke/mode change bumps `searchSeq` and clears the debounce
    // timer, and a response whose seq is no longer current is dropped
    // (stale-drop) instead of painting.
    const seq = ++searchSeq;
    const timer = setTimeout(() => {
      void activitySearch(q, scope, 8)
        .then((found) => {
          if (seq === searchSeq && found !== null) {
            locations = found;
            locationsStale = false;
          } else if (seq === searchSeq) {
            locationsStale = true; // stale epoch: keep nothing rather than guess
          }
          // Selection is reconciled by the effect below once flatItems change.
        })
        .catch(() => undefined);
    }, 120);
    return () => clearTimeout(timer);
  });

  // Selection stability (spec §3, FER-259): selection is id-driven. The first
  // result is selected as soon as it exists (never "nothing selected"), and
  // when async history results land the highlighted entry keeps its id and
  // therefore its row — only its index may move. Typing or changing the
  // filter chip resets the id so the new first result is selected.
  let selectedId = $state<string | null>(null);

  function idAt(i: number): string | null {
    return flatItems[i]?.id ?? null;
  }

  function reconcileSelection() {
    if (total === 0) {
      index = 0;
      selectedId = null;
      return;
    }
    if (selectedId) {
      const at = flatItems.findIndex((it) => it.id === selectedId);
      if (at >= 0) {
        index = at;
        return;
      }
    }
    index = 0;
    selectedId = idAt(0);
  }

  $effect(() => {
    void flatItems;
    untrack(reconcileSelection);
  });

  // Short reason per contract ReasonCode (FER-254); at most two are shown so the
  // row stays one line. Old backends send no codes: fall back to the class label.
  const REASON_LABEL: Record<ReasonCode, string> = {
    exactName: 'Nama persis',
    similarName: 'Nama mirip',
    abbreviation: 'Singkatan',
    recent: 'Baru dibuka',
    frequent: 'Sering dibuka',
    favorite: 'Favorit',
    sameWorkspace: 'Workspace sama',
    visitedInFerrum: 'Pernah diakses di Ferrum',
  };
  const matchReasons = (loc: ActivityMatchDto): string => {
    const labels = (loc.reasonCodes ?? [])
      .map((code) => REASON_LABEL[code])
      .filter((label): label is string => Boolean(label))
      .slice(0, 2);
    return labels.length > 0 ? labels.join(' · ') : matchLabel(loc.matchClass);
  };

  const matchLabel = (matchClass: string): string =>
    matchClass === 'fuzzy' || matchClass === 'subsequence'
      ? 'Nama mirip'
      : matchClass === 'initialism'
        ? 'Singkatan'
        : 'Pernah diakses di Ferrum';

  function openLocation(item: ActivityMatchDto) {
    ui.paletteOpen = false;
    const pane = getPane(targetPaneId);
    // Folder → open in place; file → REVEAL: open the parent and select the
    // entry (spec default UX — executing files stays an explicit OS action).
    if (item.entity.kind === 'folder') {
      pane?.openPath?.(item.entity.locator);
    } else if (pane?.revealPath) {
      pane.revealPath(item.entity.locator);
    } else {
      pane?.openPath?.(item.entity.parentPath);
    }
  }

  /** Empty-query favorites reuse the sidebar semantics: folders navigate,
   * files open with the OS default app (receipt recorded backend-side only
   * while recording is active — private session / pause pass record=false). */
  function activateFavorite(fav: ActivityFavoriteDto): void {
    ui.paletteOpen = false;
    if (fav.kind === 'file') {
      void shellOpen(fav.path, activity.recordingActive).catch(() => undefined);
      return;
    }
    getPane(targetPaneId)?.openPath?.(fav.path);
  }

  function navigateEntity(entity: ActivityEntityDto): void {
    ui.paletteOpen = false;
    getPane(targetPaneId)?.openPath?.(entity.locator);
  }

  function runResult(i: number): void {
    const item = flatItems[i];
    if (!item) return;
    restoreFocus();
    item.activate();
  }

  function run(a: Action) {
    if (!isEnabled(a)) return;
    ui.paletteOpen = false;
    runAction(a);
  }

  const PAGE_STEP = 6;

  function moveTo(i: number): void {
    if (total === 0) return;
    index = Math.max(0, Math.min(i, total - 1));
    selectedId = idAt(index);
  }

  // Keep the highlighted row visible while the arrows / Page keys move it.
  $effect(() => {
    if (!ui.paletteOpen) return;
    const at = index;
    queueMicrotask(() =>
      document.getElementById(`palette-opt-${at}`)?.scrollIntoView?.({ block: 'nearest' }),
    );
  });

  function onKey(e: KeyboardEvent) {
    const onChip = e.target instanceof HTMLElement && e.target.closest('.modeChip') !== null;
    // Esc is handled by use:dialog (dialogOpts.onEscape) before this handler sees it.
    if (e.key === 'PageDown') {
      e.preventDefault();
      moveTo(index + PAGE_STEP);
    } else if (e.key === 'PageUp') {
      e.preventDefault();
      moveTo(index - PAGE_STEP);
    } else if ((e.key === 'Home' || e.key === 'End') && (e.ctrlKey || query === '')) {
      // In a filled input Home / End keep moving the caret; Ctrl+Home / Ctrl+End jump the list.
      e.preventDefault();
      moveTo(e.key === 'Home' ? 0 : total - 1);
    } else if (onChip && (e.key === 'Enter' || e.key === ' ')) {
      // Chips are buttons: Enter / Space activate the chip itself, not the highlighted result.
      return;
    } else if (e.key === 'ArrowDown') {
      e.preventDefault();
      if (total > 0) index = Math.min(index + 1, total - 1);
      selectedId = idAt(index);
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      index = Math.max(index - 1, 0);
      selectedId = idAt(index);
    } else if (e.key === 'Enter' && index < total) {
      e.preventDefault();
      runResult(index);
    }
  }

  // Consecutive same-group items, keeping each item's flat index for ARIA ids.
  const groups = $derived.by(() => {
    const out: { group: FlatItem['group']; items: { item: FlatItem; i: number }[] }[] = [];
    flatItems.forEach((item, i) => {
      const last = out[out.length - 1];
      if (last && last.group === item.group) last.items.push({ item, i });
      else out.push({ group: item.group, items: [{ item, i }] });
    });
    return out;
  });

  // Live region: partial/stale history results are announced, not silent.
  const statusText = $derived(
    locationsStale
      ? 'Riwayat berubah — hasil riwayat belum lengkap, coba lagi.'
      : total === 0
        ? 'Tidak ada hasil'
        : `${total} hasil`,
  );

  // FER-577: Tab trap and modalDepth (the palette is a modal too); focus goes to the input and
  // is handed back by closePalette(), so the dialog action's own return is off. Esc, arrows and
  // Enter stay in onKey above (Esc here only guarantees it never leaks past the palette).
  const dialogOpts = {
    initial: 'input',
    onEscape: () => closePalette(),
    mnemonics: false,
    returnFocus: false,
  };

  const GROUP_LABEL: Record<FlatItem['group'], string> = {
    rcmd: 'Recent commands',
    fav: 'Favorites',
    recent: 'Recent',
    frequent: 'Frequent',
    hot: 'Hotlist',
    loc: 'Pernah diakses di Ferrum',
    cmd: 'Perintah',
  };
</script>

{#if ui.paletteOpen}
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="scrim" onmousedown={closePalette}></div>
  <div
    class="palette"
    role="dialog"
    aria-modal="true"
    aria-label="Command palette"
    tabindex="-1"
    onkeydown={onKey}
    use:dialog={dialogOpts}
  >
    <div class="head">
      <Icon name="search" size={17} />
      <input
        bind:this={inputEl}
        bind:value={query}
        oninput={() => (selectedId = null)}
        role="combobox"
        aria-expanded="true"
        aria-controls="palette-results"
        aria-activedescendant={total > 0 ? `palette-opt-${index}` : undefined}
        placeholder="Type a command…"
        spellcheck="false"
        aria-label="Search commands"
      />
      <span class="kbd mono">esc</span>
    </div>
    {#if showChips}
      <div
        class="modes"
        role="toolbar"
        aria-label="Filter hasil"
        use:roving={{ items: '.modeChip', orientation: 'horizontal', select: false }}
      >
        {#each MODES as m (m.id)}
          <button
            class="modeChip"
            class:sel={mode === m.id}
            aria-pressed={mode === m.id}
            onclick={() => {
              mode = m.id;
              index = 0;
              selectedId = null;
            }}
          >
            {m.label}
          </button>
        {/each}
      </div>
    {/if}
    <div class="sr" role="status" aria-live="polite">{statusText}</div>
    <div class="list" id="palette-results" role="listbox" aria-label="Results">
      {#each groups as g (g.group)}
        <div role="group" aria-label={GROUP_LABEL[g.group]}>
          <div class="groupTitle" aria-hidden="true">{GROUP_LABEL[g.group]}</div>
          {#each g.items as { item, i } (item.id)}
            <button
              class="item"
              id={`palette-opt-${i}`}
              class:active={i === index}
              role="option"
              aria-selected={i === index}
              tabindex="-1"
              onmousemove={() => {
                index = i;
                selectedId = item.id;
              }}
              onclick={() => {
                restoreFocus();
                ui.paletteOpen = false;
                item.activate();
              }}
            >
              <span class="label">{item.label}</span>
              {#if disambiguation[item.id]}
                <span class="disamb" title={item.parent}>{disambiguation[item.id]}</span>
              {/if}
              {#if item.badge}<span class="badge">{item.badge}</span>{/if}
              {#if item.network}<span
                  class="badge net"
                  title="Network folder: opening it contacts another computer">net</span
                >{/if}
              <span class="sub" title={item.sub}>{item.sub}</span>
              {#if item.kbd}<span class="kbd mono">{item.kbd}</span>{/if}
            </button>
          {/each}
        </div>
      {/each}
      {#if locationsStale && total > 0}
        <div class="empty" role="note">Riwayat berubah — hasil riwayat belum lengkap.</div>
      {/if}
      {#if total === 0}
        <div class="empty">
          {locationsStale ? 'Riwayat berubah — coba lagi.' : 'No commands match.'}
        </div>
      {/if}
    </div>
    <footer class="foot">
      <span><span class="kbd mono">↑↓</span> Navigate</span>
      <span><span class="kbd mono">↵</span> Run</span>
    </footer>
  </div>
{/if}

<style>
  .modes {
    display: flex;
    gap: 4px;
    padding: 8px 12px 0;
  }
  .modeChip {
    height: 22px;
    padding: 0 10px;
    border: 0;
    border-radius: 999px;
    background: var(--hover);
    color: var(--text3);
    font-size: 11.5px;
    cursor: default;
  }
  .modeChip.sel {
    background: var(--sel);
    color: var(--text);
  }
  .scrim {
    position: fixed;
    inset: 0;
    background: var(--scrim);
    z-index: 70;
    animation: fmFade 140ms ease-out;
  }
  .palette {
    position: fixed;
    left: 50%;
    top: 12vh;
    transform: translateX(-50%);
    width: min(620px, 92vw);
    border-radius: calc(var(--rl) + 2px);
    background: var(--elev);
    box-shadow: var(--shadow);
    overflow: hidden;
    z-index: 71;
    animation: fmPop 150ms cubic-bezier(0.2, 0.7, 0.2, 1);
    display: flex;
    flex-direction: column;
  }
  .head {
    height: 50px;
    flex: none;
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 0 14px;
    border-bottom: 1px solid var(--line);
    color: var(--text3);
  }
  .head input {
    flex: 1;
    min-width: 0;
    border: 0;
    outline: 0;
    background: transparent;
    font-size: calc(15px + var(--uiStep, 0px));
    color: var(--text);
  }
  .kbd {
    font-size: calc(10.5px + var(--uiStep, 0px));
    padding: 2px 6px;
    border-radius: 4px;
    background: var(--kbd);
    color: var(--text2);
  }
  .list {
    max-height: min(400px, 56vh);
    overflow: auto;
    padding: 4px 6px 6px;
  }
  .item {
    position: relative;
    width: 100%;
    height: 34px;
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 0 10px;
    border: 0;
    border-radius: var(--r);
    background: transparent;
    color: var(--text);
    font-size: calc(13px + var(--uiStep, 0px));
    text-align: left;
    cursor: default;
  }
  .item.active {
    background: var(--sel);
  }
  .groupTitle {
    padding: 8px 10px 4px;
    font-size: 11px;
    font-weight: 500;
    color: var(--text3);
  }
  .item.active::before {
    content: '';
    position: absolute;
    left: 0;
    top: 8px;
    bottom: 8px;
    width: 2px;
    border-radius: 2px;
    background: var(--accent);
  }
  .label {
    white-space: nowrap;
  }
  .badge {
    flex: none;
    padding: 1px 6px;
    border-radius: 999px;
    background: var(--hover);
    color: var(--text2);
    font-size: 10.5px;
  }
  .badge.net {
    font-family: 'Geist Mono', monospace;
    background: transparent;
    border: 1px solid var(--line);
  }
  .disamb {
    flex: none;
    max-width: 40%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 11px;
    color: var(--text2);
  }
  .sr {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }
  .sub {
    flex: 1;
    min-width: 0;
    font-family: 'Geist Mono', monospace;
    font-size: calc(11px + var(--uiStep, 0px));
    color: var(--text3);
    text-align: left;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .item .kbd {
    margin-left: auto;
  }
  .empty {
    padding: 28px;
    text-align: center;
    font-size: calc(12.5px + var(--uiStep, 0px));
    color: var(--text3);
  }
  .foot {
    height: 34px;
    flex: none;
    display: flex;
    align-items: center;
    gap: 14px;
    padding: 0 14px;
    border-top: 1px solid var(--line);
    font-size: calc(11.5px + var(--uiStep, 0px));
    color: var(--text3);
    background: var(--hover);
  }
  .foot span {
    display: flex;
    gap: 5px;
    align-items: center;
  }
  @keyframes fmPop {
    from {
      opacity: 0;
      transform: translateX(-50%) translateY(-6px) scale(0.985);
    }
    to {
      opacity: 1;
      transform: translateX(-50%) translateY(0) scale(1);
    }
  }
</style>
