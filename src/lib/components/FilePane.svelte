<script lang="ts">
  import TabStrip from './TabStrip.svelte';
  import PaneToolbar from './PaneToolbar.svelte';
  import { crumbsOf } from '../crumbs';
  import { hotlist } from '../stores/hotlistStore.svelte';
  import FileList from './FileList.svelte';
  import PaneStatusBar from './PaneStatusBar.svelte';
  import { mockFetcher, mockHome } from '../mockData';
  import { ui, fullPageOpen, type PaneId } from '../uiState.svelte';
  import { modalDepth } from '../dialogKit';
  import { SvelteMap, SvelteSet } from 'svelte/reactivity';
  import { onMount, tick, untrack } from 'svelte';
  import { PaneStore, type Row } from '../stores/paneStore.svelte';
  import {
    filterRows,
    moveCursor,
    selectAll,
    selectRange,
    sortRows,
    toggleSelect,
    type SortKey,
  } from '../selection';
  import { filterByView } from '../viewFilter';
  import { viewState } from '../stores/viewStateStore.svelte';
  import { columns } from '../stores/columnsStore.svelte';
  import { effectiveSortSpec } from '../columns';
  import { isInTauri } from '../ipc/tauri';
  import { activity } from '../stores/activityStore.svelte';
  import { shellOpen } from '../ipc/activity';
  import { handleShellError } from '../ipc/shellOpen';
  import { archiveMemberOf, openArchiveMemberAt } from '../stores/openArchiveMember';
  import { toast } from '../stores/toastStore.svelte';
  import { formatSize, parentOf } from '../format';
  import { homeDir } from '@tauri-apps/api/path';
  import { registerPane } from '../stores/panesRegistry.svelte';
  import { buildPaneProbe, registerPaneContext } from '../ai/contextPanes';
  import { prefs } from '../stores/prefsStore.svelte';
  import { watcher } from '../stores/watchStore.svelte';
  import { driveStatus } from '../stores/driveStatus.svelte';
  import { sidebarTree } from '../stores/treeStore.svelte';
  import { separateTree } from '../stores/separateTreeStore.svelte';
  import PaneTree from './PaneTree.svelte';
  import { dialogs } from '../stores/dialogStore.svelte';
  import { previewStore } from '../stores/previewStore.svelte';
  import { compareStore } from '../stores/compareStore.svelte';
  import { branch } from '../stores/branchStore.svelte';
  import { comments } from '../stores/commentStore.svelte';
  import {
    beginInternalDrag,
    dragItemCount,
    dragOriginPane,
    endInternalDrag,
    isInternalDrag,
    readDragPaths,
    setDragPaths,
  } from '../stores/dragPayload';
  import {
    createArchiveAwareFetcher,
    isArchiveFile,
    splitArchiveLocation,
  } from '../stores/archiveFetcher';
  import { isMultipartError } from '../ipc/archiveErrors';
  import { duWorkspace } from '../diskUsage/duWorkspace.svelte';

  let { paneId }: { paneId: PaneId } = $props();

  // Browser dev keeps the shell previewable without Rust via mock listings.
  // In Tauri: archive-aware fetcher dispatches to disk or archive listing.
  const knownArchives = new SvelteSet<string>();
  const archiveFetcher = isInTauri()
    ? createArchiveAwareFetcher(
        knownArchives,
        (p) => knownArchives.add(p),
        // FER-75: a non-first volume or a set with missing volumes gets an explanation, with a
        // button to open the first volume, instead of a bare error.
        (p, e) => {
          if (isMultipartError(e)) void dialogs.showMultipart(p, (e as Error).message, 'Open');
        },
        // FER-504: a header-encrypted archive asks for its password when
        // the pane opens it; the listing retries with what the user typed.
        (p, wrong) => dialogs.askArchivePassword(p, wrong),
      )
    : null;
  const store = new PaneStore(
    archiveFetcher ?? mockFetcher,
    // Desktop: homePath starts at the real drive root and is replaced by the real home dir in bootTabs.
    // Mock home is browser dev only.
    isInTauri() ? 'C:\\' : mockHome[paneId],
  );
  // Deliberate successful navigation → activity receipt (best-effort, quiet).
  store.onVisit = (path) => {
    // Gated by pause + this window's private session.
    activity.recordVisit(path);
  };

  /** True when the current path is inside an archive (read-only). */
  const inArchive = $derived(splitArchiveLocation(store.path) !== null);

  let selected = new SvelteSet<string>();
  let paneEl = $state<HTMLElement | undefined>();
  let cursor = $state<string | undefined>();
  let anchor = $state<string | undefined>();
  let filterQ = $state('');
  let lastTypeKey = '';

  const active = $derived(ui.activePane === paneId);
  const crumbs = $derived(crumbsOf(store.path));
  let addressEditing = $state(false);
  // Per-pane Show state (F2-15): mode, sort, reversed, filter preset.
  const view = $derived(viewState.panes[paneId]);
  // F2-365 Custom Columns: the visible-column preference is global; the
  // pane's sort falls back to Name while its column is hidden so the visible
  // order is always explained by a visible header.
  const colCfg = $derived(columns.config);
  const effSort = $derived(effectiveSortSpec(colCfg, view.sort));
  // Branch View (F2-23): an optional per-pane row SOURCE swap — the subtree
  // listing replaces the directory listing as the pipeline's input while
  // sort/filter/selection and the pane's tab/history stay exactly the same.
  const branchActive = $derived(branch.activeFor(paneId));
  const branchRows = $derived(branchActive ? branch.rows(paneId) : []);
  // View pipeline (F2-15): sort the full listing, apply the Show filter
  // preset, then the query/hidden filter; reversed flips the final display
  // order. F2-18's Hide Same names filter last. Selection and cursor
  // address rows by path, so all of it survives. Branch rows (F2-23) enter
  // at the FRONT of this same pipeline, so they are sorted and filtered
  // exactly like directory rows.
  const visible = $derived.by(() => {
    let rows = sortRows(branchActive ? branchRows : store.rows, effSort, ui.foldersFirst);
    rows = filterByView(rows, {
      mode: view.filterMode,
      pattern: view.filterPattern,
      selectedPaths: selected,
    });
    rows = filterRows(rows, { query: filterQ, showHidden: ui.showHidden });
    const hideSame = compareStore.hiddenSameNames;
    if (hideSame) rows = rows.filter((r) => !hideSame.has(r.entry.name));
    if (view.reversed) rows.reverse();
    return rows;
  });
  // Total size of the marked FILES (folders have no size until calculated).
  const markedBytes = $derived(
    visible.reduce(
      (n, r) => (selected.has(r.entry.path) && !r.entry.isDir ? n + r.entry.size : n),
      0,
    ),
  );
  const statusText = $derived(
    branchActive
      ? (branch.stateFor(paneId)?.label ??
          `Branch view · ${store.path} · ${branchRows.length} files${branch.noteFor(paneId)}`)
      : selected.size > 0
        ? `${selected.size} of ${store.rows.length} selected · ${formatSize(markedBytes)}`
        : `${store.rows.length} items`,
  );

  function setSort(key: SortKey) {
    viewState.setSort(paneId, key);
  }

  function replaceSelection(next: Iterable<string>) {
    selected.clear();
    for (const p of next) selected.add(p);
  }

  /** Marks when there are any, else the cursor row: what F5/F6/F8/Properties/... act on. */
  function effectiveSelection(): string[] {
    if (selected.size > 0) return [...selected];
    return cursor !== undefined && visible.some((r) => r.entry.path === cursor) ? [cursor] : [];
  }

  /** Keyboard model over the VISIBLE list (task 08). */

  /**
   * FER-492 (Total Commander model): the cursor and the marks are separate. Plain navigation
   * only moves the cursor and never touches the marks; Shift+navigation marks the range from
   * the anchor; Space/Insert toggle the mark of the cursor row and step down.
   */
  function moveCursorTo(next: string | undefined, e: KeyboardEvent) {
    if (!next) return;
    if (e.shiftKey) {
      const from = anchor ?? cursor ?? next;
      replaceSelection(selectRange(visible, from, next));
      anchor = from;
    } else {
      anchor = next;
    }
    cursor = next;
  }

  function onListKey(e: KeyboardEvent) {
    ui.activate(paneId);
    // FER-484: Alt+Down belongs to the folder history dropdown, not to cursor movement.
    if (e.altKey && (e.key === 'ArrowDown' || e.key === 'ArrowUp')) return;
    const delta = e.key === 'ArrowDown' ? 1 : e.key === 'ArrowUp' ? -1 : 0;
    if (delta !== 0) {
      e.preventDefault();
      moveCursorTo(moveCursor(visible, cursor, delta), e);
      return;
    }
    if (e.key === 'Home' || e.key === 'End') {
      e.preventDefault();
      moveCursorTo(visible[e.key === 'Home' ? 0 : visible.length - 1]?.entry.path, e);
      return;
    }
    if (e.key === 'PageDown' || e.key === 'PageUp') {
      // The focusable list would scroll natively and leave the cursor behind.
      e.preventDefault();
      const body = e.currentTarget instanceof HTMLElement ? e.currentTarget : null;
      const rowH = body?.querySelector('[data-path]')?.getBoundingClientRect().height || 26;
      const page = Math.max(1, Math.floor((body?.clientHeight || 0) / rowH) - 1);
      moveCursorTo(moveCursor(visible, cursor, e.key === 'PageDown' ? page : -page), e);
      return;
    }
    if (
      (e.key === ' ' || e.key === 'Insert') &&
      !e.ctrlKey &&
      !e.metaKey &&
      !e.altKey &&
      !e.shiftKey
    ) {
      const target = e.target;
      if (
        target instanceof HTMLElement &&
        (target.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(target.tagName))
      )
        return;
      e.preventDefault();
      if (!cursor || !visible.some((r) => r.entry.path === cursor)) return;
      replaceSelection(toggleSelect(selected, cursor));
      anchor = cursor;
      const next = moveCursor(visible, cursor, 1);
      if (next) cursor = next;
      return;
    }
    // FER-128: Esc stops a branch walk that is still loading (rows so far stay).
    if (e.key === 'Escape' && branchActive && branch.isLoading(paneId)) {
      e.preventDefault();
      branch.cancel(paneId);
      return;
    }
    if (e.key === 'Escape' && selected.size > 0 && !e.ctrlKey && !e.metaKey && !e.altKey) {
      e.preventDefault();
      replaceSelection([]);
      return;
    }
    if ((e.ctrlKey || e.metaKey) && (e.key === 'a' || e.key === 'A')) {
      e.preventDefault();
      replaceSelection(selectAll(visible));
      return;
    }
    onTypeToSelect(e);
  }

  function onTypeToSelect(e: KeyboardEvent) {
    if (ui.activePane !== paneId || e.defaultPrevented) return;
    // FER-577 (N4): a dialog, the palette or a full page owns the keyboard; the list behind
    // it must not move its cursor while the user types an accelerator or a name.
    if (modalDepth() > 0 || dialogs.current || ui.paletteOpen || fullPageOpen() || duWorkspace.open)
      return;
    const target = e.target;
    if (
      e.ctrlKey ||
      e.metaKey ||
      e.altKey ||
      e.key.length !== 1 ||
      !/^[\p{L}\p{N}]$/u.test(e.key) ||
      (target instanceof HTMLElement &&
        (target.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(target.tagName)))
    )
      return;

    const key = e.key.toLocaleLowerCase();
    const repeated = key === lastTypeKey;
    lastTypeKey = key;
    const currentIndex = repeated ? visible.findIndex((row) => row.entry.path === cursor) : -1;
    for (let offset = 1; offset <= visible.length; offset += 1) {
      const index = (currentIndex + offset + visible.length) % visible.length;
      const row = visible[index];
      if (!row.entry.name.toLocaleLowerCase().startsWith(key)) continue;
      e.preventDefault();
      cursor = row.entry.path;
      anchor = cursor;
      return;
    }
  }

  // Initial tabs (FER-395): boot waits for prefs so "Restore tabs on launch"
  // can swap the default single home tab for the saved session. Restore only
  // navigates — an unreachable location surfaces its error inside its own
  // tab. onMount (not $effect): tab boot reads reactive state, so an effect
  // here would re-trigger itself endlessly.
  async function bootTabs(): Promise<void> {
    await prefs.init(); // same promise App startup awaits — no double load
    const saved = prefs.session?.panes[paneId];
    // Real home is the fallback when a restored folder is gone (FER-457); the prototype mock home stays in browser dev.
    if (isInTauri()) {
      // Real home also becomes the default for new tabs and last-tab close (store.homePath).
      // If the lookup fails, keep the drive root; never overwrite with an empty value.
      try {
        const home = await homeDir();
        if (home) {
          store.recoveryHome = home;
          store.homePath = home;
        }
      } catch {
        // keep store.homePath at the drive root
      }
    }
    if (ui.tabs.restoreTabs && saved && saved.tabs.length > 0) {
      await store.restore(saved.tabs, saved.current);
      return;
    }
    // Default boot: home directory under Tauri, prototype mock in browser.
    if (isInTauri()) {
      void store.newTab(store.homePath);
    } else {
      void store.newTab(mockHome[paneId]);
    }
  }

  onMount(() => {
    void bootTabs();
    // Read-only snapshot source for the AI assistant (FER-160): getters are evaluated per call.
    const side = paneId === 0 ? 'left' : 'right';
    const offContext = registerPaneContext(
      side,
      buildPaneProbe(() => ({
        paneId: side,
        tab: store.tab,
        rows: store.rows,
        visible: () => visible,
        selected: () => effectiveSelection(),
        path: store.path,
        branchActive,
        inArchive,
        filterMode: view.filterMode,
        filterPattern: view.filterPattern,
        filterQuery: filterQ,
        showHidden: ui.showHidden,
        sort: { key: effSort.key, dir: effSort.dir },
      })),
    );
    // Expose the pane to global actions (dialogs, shortcuts, palette).
    const offPane = registerPane(paneId, {
      get path() {
        return store.path;
      },
      get canGoBack() {
        return store.canBack;
      },
      // Operation targets: the marks, else the cursor row (FER-492).
      selectedPaths: () => effectiveSelection(),
      markedPaths: () => [...selected],
      cursorEntry: () => {
        const row = visible.find((r) => r.entry.path === cursor);
        return row
          ? {
              name: row.entry.name,
              path: row.entry.path,
              isDir: row.entry.isDir && !row.entry.isSymlink,
            }
          : undefined;
      },
      refresh: () => {
        selected.clear();
        anchor = undefined; // FER-489: the cursor survives a refresh (reconcileCursor)
        void store.refresh();
      },
      // F2-26 watcher refresh: re-list while keeping selection/cursor whose
      // entries still exist — external changes must not clobber the user's
      // selection. Removed entries are pruned (incremental-if-safe).
      refreshExternal: () => {
        void store.refresh().then(() => {
          const alive = new Set(store.rows.map((r) => r.entry.path));
          for (const p of [...selected]) {
            if (!alive.has(p)) selected.delete(p);
          }
          if (cursor !== undefined && !alive.has(cursor)) {
            anchor = undefined; // cursor moves to a neighbour in reconcileCursor (FER-489)
          }
        });
      },
      newTab: () => void store.newTab(),
      cycleTab: (delta) => {
        const n = store.tabs.length;
        if (n < 2) return;
        selected.clear();
        cursor = undefined;
        const next = (store.current + delta + n) % n;
        store.selectTab(next);
        // Focus sat on a tab button: follow the selection instead of staying on the old tab.
        if (document.activeElement?.closest('[role="tablist"]')) {
          void tick().then(() =>
            paneEl?.querySelectorAll<HTMLElement>('[role="tab"]')[next]?.focus(),
          );
        }
      },
      closeCurrentTab: () => void store.closeTab(store.current),
      goUp: () => void store.goUp(),
      openCursorEntry: () => {
        const target = visible.find((r) => r.entry.path === cursor);
        if (!target) return;
        if (target.entry.isDir && !target.entry.isSymlink) {
          navigate(target.entry.path);
        } else if (isArchiveFile(target.entry.name)) {
          navigate(target.entry.path);
        } else if (target && !target.entry.isDir) {
          void openFile(target.entry.path);
        }
      },
      openPath: (path: string) => navigate(path),
      // FER-482 Ctrl+U: swap the whole pane content with the other pane.
      takeContents: () => ({
        ...store.takeTabs(),
        selected: [...selected],
        cursor,
        anchor,
      }),
      putContents: (c) => {
        branch.exit(paneId);
        store.adoptTabs(c.tabs, c.current);
        replaceSelection(c.selected);
        cursor = c.cursor;
        anchor = c.anchor;
      },
      editPath: () => (addressEditing = true),
      // FER-489: Tab from the other pane. Cursor keeps its last row; first row when none yet.
      focusList: () => {
        ensureCursor();
        const list = paneEl?.querySelector<HTMLElement>('[data-pane-list]');
        list?.focus();
      },
      clearSelection: () => {
        replaceSelection(new Set());
      },
      // F2-14: Mark operations work on plain data + path identity, so the
      // registry can select and format without touching component internals.
      visibleEntries: () =>
        visible.map((r) => ({
          name: r.entry.name,
          path: r.entry.path,
          isDir: r.entry.isDir && !r.entry.isSymlink,
          size: r.entry.size,
          modifiedMs: r.entry.modifiedMs,
        })),
      selectPaths: (paths: Iterable<string>, cursorPath?: string) => {
        replaceSelection(new Set(paths));
        if (cursorPath !== undefined) cursor = cursorPath;
      },
      back: () => {
        selected.clear();
        cursor = undefined;
        anchor = undefined;
        void store.back();
      },
      historyBack: () => [...(store.tab?.back ?? [])],
      listNames: () => store.rows.map((r) => r.entry.name),
      // FER-395 session provider: plain tab paths + active index.
      getSession: () => ({
        tabs: store.tabs.map((t) => t.path),
        current: store.current,
      }),
      // Phase-4 palette default UX: reveal a FILE — open its parent folder,
      // then select + cursor the entry so the user sees it in place.
      revealPath: (path: string) => {
        const parent = parentOf(path);
        if (!parent) return;
        selected.clear();
        cursor = undefined;
        anchor = undefined;
        void store.open(parent).then(() => {
          if (store.error || path === store.path) return;
          if (store.rows.some((row) => row.entry.path === path)) {
            selected.add(path);
            cursor = path;
          }
        });
      },
    });
    return () => {
      offContext();
      offPane();
    };
  });

  function rowClick(row: Row, e: MouseEvent) {
    // Explorer rules: a plain click moves the cursor and clears the marks; Ctrl toggles a
    // mark; Shift marks the range from the anchor.
    if (e.shiftKey && anchor) {
      replaceSelection(selectRange(visible, anchor, row.entry.path));
    } else if (e.ctrlKey || e.metaKey) {
      replaceSelection(toggleSelect(selected, row.entry.path));
      anchor = row.entry.path;
    } else {
      replaceSelection([]);
      anchor = row.entry.path;
    }
    cursor = row.entry.path;
    ui.activate(paneId);
  }

  /**
   * Deliberate open of a FILE (double-click / Enter): hand it to the OS and
   * record the receipt only after the OS accepted it (backend decides; private
   * or paused windows pass record=false). Never called from preview/QuickView.
   */
  async function openFile(path: string) {
    // Non-local / virtual locations (URI-style paths) are not ours to open.
    if (/^[A-Za-z][A-Za-z0-9+.-]+:\/\//.test(path)) return;
    try {
      // FER-575: a file inside an archive is not a disk path. The backend extracts that member
      // to its temp folder and opens it through the confirmed shell path; no receipt (temp file).
      if (archiveMemberOf(path, knownArchives)) {
        await openArchiveMemberAt(path, {
          knownArchives,
          passwordFor: archiveFetcher?.passwordFor,
          forgetPassword: archiveFetcher?.forgetPassword,
          rememberPassword: archiveFetcher?.rememberPassword,
          askPassword: (p, wrong) => dialogs.askArchivePassword(p, wrong),
          showWorking: (name) => {
            const message = `Membuka ${name} dari arsip…`;
            toast.show(message, 600_000);
            // Clear only our own notice, never a newer unrelated toast.
            return () => {
              if (toast.message === message) toast.show('', 0);
            };
          },
          onError: (message) =>
            dialogs.open({ kind: 'error', title: 'Open from archive', message }),
        });
        return;
      }
      await shellOpen(path, activity.recordingActive);
    } catch (err) {
      handleShellError('Open', err);
    }
  }

  function rowActivate(row: Row) {
    if ((row.entry.isDir && !row.entry.isSymlink) || isArchiveFile(row.entry.name)) {
      pendingFocus = true;
      selected.clear();
      cursor = undefined;
      void store.open(row.entry.path);
    } else if (!row.entry.isDir) {
      void openFile(row.entry.path);
    }
  }

  // Keep the OS watcher pointed at both panes' current directories, and
  // make the sidebar tree follow the ACTIVE pane's path (two-way sync).
  $effect(() => {
    const path = store.path;
    if (!path) return;
    void watcher.registerPanePath(paneId, path);
    if (ui.activePane === paneId) void sidebarTree.followActivePath(path);
  });

  // FER-366 Separate Tree: this pane's OWN tree follows the pane's cwd
  // (ancestors expand lazily, the path is highlighted) — expansion and
  // selection live in the pane's private TreeStore, so neither the other
  // pane's tree nor the global sidebar tree is touched by this effect.
  $effect(() => {
    if (!separateTree.isEnabled(paneId)) return;
    const path = store.path;
    if (path) void separateTree.follow(paneId, path);
  });

  // F2-27 Comments view: follow this pane's directory (and listing
  // refreshes, so a rename migration shows up) while comments are shown.
  $effect(() => {
    if (!ui.showComments) return;
    const dir = store.path;
    void store.rows; // re-read the sidecar after listing refreshes as well
    if (dir) void comments.load(paneId, dir);
  });

  // ── FER-489 cursor model (Total Commander) ─────────────────────────────
  // After a location change the cursor lands on: the folder just left (going up), the last
  // cursor remembered there (back/forward/tab switch), else the first row. Refreshes keep the
  // cursor, or move it to the nearest surviving neighbour. The cursor is NOT a mark.
  const cursorMemory = new SvelteMap<string, string>();
  let settledKey: string | undefined;
  let settledTab = -1;
  let prevBack = 0;
  let prevFwd = 0;
  let prevPaths: string[] = [];
  let pendingFocus = false;
  const normPath = (p: string) => {
    const q = p.toLowerCase().split('/').join(String.fromCharCode(92));
    return q.length > 1 && q.endsWith(String.fromCharCode(92)) ? q.slice(0, -1) : q;
  };

  /** A focused list always has a cursor row: keep the last one, else the first row. */
  function ensureCursor() {
    if (cursor !== undefined && visible.some((r) => r.entry.path === cursor)) return;
    const first = visible[0];
    if (first) {
      cursor = first.entry.path;
      anchor = cursor;
    }
  }

  function focusOwnList(el: Element | null) {
    const list = paneEl?.querySelector<HTMLElement>('[data-pane-list]');
    if (!list) return;
    const idle = !el || el === document.body || list.contains(el);
    if (ui.activePane === paneId && (pendingFocus || idle)) list.focus();
    pendingFocus = false;
  }

  function reconcileCursor(path: string, rows: typeof visible, tab: number) {
    const paths = rows.map((r) => r.entry.path);
    const present = new Set(paths);
    const key = (branchActive ? 'branch:' : '') + path;
    const t = store.tab;
    const back = t?.back.length ?? 0;
    const fwd = t?.fwd.length ?? 0;
    if (key !== settledKey || tab !== settledTab) {
      const prevKey = settledKey;
      const sameTab = tab === settledTab;
      const wentBack = sameTab && back < prevBack;
      const wentFwd = sameTab && fwd < prevFwd;
      settledKey = key;
      settledTab = tab;
      prevBack = back;
      prevFwd = fwd;
      prevPaths = paths;
      if (cursor === undefined || !present.has(cursor)) {
        let target: string | undefined;
        if (prevKey && sameTab && !wentBack && !wentFwd) {
          const left = normPath(prevKey);
          target = paths.find((p) => normPath(p) === left);
        }
        if (!target && (wentBack || wentFwd || !sameTab)) {
          const mem = cursorMemory.get(key);
          if (mem && present.has(mem)) target = mem;
        }
        cursor = target ?? paths[0];
        anchor = undefined;
      }
      focusOwnList(document.activeElement);
      return;
    }
    prevBack = back;
    prevFwd = fwd;
    if (cursor !== undefined && !present.has(cursor)) {
      const old = prevPaths.indexOf(cursor);
      let next: string | undefined;
      if (old >= 0) {
        for (let i = old + 1; i < prevPaths.length && !next; i++) {
          if (present.has(prevPaths[i])) next = prevPaths[i];
        }
        for (let i = old - 1; i >= 0 && !next; i--) {
          if (present.has(prevPaths[i])) next = prevPaths[i];
        }
      }
      cursor = next ?? paths[0];
      anchor = undefined;
    }
    prevPaths = paths;
    // A refresh re-renders the rows; keep keyboard focus in the list if it was there.
    focusOwnList(document.activeElement);
  }

  $effect(() => {
    const path = store.path;
    const rows = visible;
    const tab = store.current;
    // FER-128: a streaming branch walk with no rows yet is still "loading".
    if (store.loading || store.error || !path || branch.isSettling(paneId)) return;
    untrack(() => reconcileCursor(path, rows, tab));
  });

  $effect(() => {
    const c = cursor;
    const path = store.path;
    if (c === undefined || store.loading) return;
    if (untrack(() => visible.some((r) => r.entry.path === c))) {
      cursorMemory.set((branchActive ? 'branch:' : '') + path, c);
    }
  });

  function navigate(path: string) {
    pendingFocus = true;
    selected.clear();
    cursor = undefined;
    anchor = undefined;
    // Any explicit navigation leaves the branch listing (spec: the
    // breadcrumb returns to the directory view without losing history).
    branch.exit(paneId);
    void store.open(path);
  }

  // ── Cross-pane drag & drop (prototype dropOn overlay) ─────────────────
  // Drag sources are handled here via DELEGATION: FileList rows carry
  // data-path and stay untouched; dragstart bubbles up to this pane root.
  // MVP target rule: drops are accepted only from the OTHER pane.
  let dropHover = $state(false);
  let dropMove = $state(false);
  let dropItems = $state(0);

  function onDragStart(e: DragEvent) {
    const target = e.target;
    if (!(target instanceof Element)) return;
    const path = target.closest('[data-path]')?.getAttribute('data-path');
    if (!path) return;
    const paths = selected.has(path) ? [...selected] : [path];
    beginInternalDrag(paneId, paths.length);
    setDragPaths(e, paths);
  }

  function onDragEnd() {
    endInternalDrag();
  }

  function onDragOver(e: DragEvent) {
    if (!isInternalDrag(e)) return;
    const origin = dragOriginPane();
    if (origin === null || origin === paneId) return;
    e.preventDefault();
    if (e.dataTransfer) e.dataTransfer.dropEffect = e.shiftKey ? 'move' : 'copy';
    dropMove = e.shiftKey;
    dropItems = dragItemCount();
    dropHover = true;
  }

  function onDragLeave(e: DragEvent) {
    const el = e.currentTarget;
    const to = e.relatedTarget;
    if (el instanceof Element && (!(to instanceof Node) || !el.contains(to))) {
      dropHover = false;
    }
  }

  function onDrop(e: DragEvent) {
    if (!isInternalDrag(e)) return;
    const origin = dragOriginPane();
    if (origin === null || origin === paneId) return;
    const paths = readDragPaths(e);
    const move = e.shiftKey;
    e.preventDefault();
    dropHover = false;
    endInternalDrag();
    if (!paths || paths.length === 0 || !store.path) return;
    void dialogs.beginTransferTo(move ? 'move' : 'copy', paths, store.path);
  }

  // F2-17: the ACTIVE pane's cursor drives the Quick View panel (the store
  // debounces and supersedes stale loads by itself).
  $effect(() => {
    if (!ui.quickViewOpen || ui.activePane !== paneId) return;
    const path = cursor;
    const row = visible.find((r) => r.entry.path === path);
    previewStore.setTarget(
      row
        ? {
            name: row.entry.name,
            path: row.entry.path,
            isDir: row.entry.isDir && !row.entry.isSymlink,
            size: row.entry.size,
            modifiedMs: row.entry.modifiedMs,
          }
        : null,
    );
  });
</script>

<svelte:window onkeydown={(e) => onTypeToSelect(e)} />

<section
  class="pane"
  bind:this={paneEl}
  class:active
  data-pane={paneId}
  onmousedown={() => ui.activate(paneId)}
  ondragstart={onDragStart}
  ondragend={onDragEnd}
  ondragover={onDragOver}
  ondragleave={onDragLeave}
  ondrop={onDrop}
  aria-label={paneId === 0 ? 'Left pane' : 'Right pane'}
>
  <div class="topLine"></div>
  <div class="filesMode">
    <TabStrip
      tabs={store.tabs}
      current={store.current}
      tabWidth={ui.tabs.tabWidth}
      middleClose={ui.tabs.closeOnMiddleClick}
      onselect={(i) => {
        selected.clear();
        cursor = undefined;
        store.selectTab(i);
      }}
      onclose={(i) => void store.closeTab(i)}
      onnew={() => void store.newTab()}
    />
    <PaneToolbar
      {crumbs}
      path={store.path}
      bind:editing={addressEditing}
      onnavigate={navigate}
      onhotlist={() => hotlist.openMenu(paneId)}
      dim={!active}
      filterValue={filterQ}
      onfilter={(q) => (filterQ = q)}
      canBack={store.canBack}
      canFwd={store.canFwd}
      onback={() => void store.back()}
      onforward={() => void store.forward()}
      onup={() => void store.goUp()}
      oncrumb={navigate}
      onDiskUsage={() => duWorkspace.openFromPanes()}
    />
    {#if separateTree.isEnabled(paneId)}
      <!-- FER-366: TC's per-pane separate tree — this pane's OWN drive tree
           above its list; expanding/clicking navigates only this pane. -->
      <PaneTree {paneId} onnavigate={navigate} />
    {/if}
    <FileList
      rows={visible}
      {selected}
      {cursor}
      mode={view.viewMode}
      columns={colCfg}
      showComments={ui.showComments}
      comments={comments.map(paneId)}
      notice={store.notice}
      loading={branchActive ? branch.isSettling(paneId) : store.loading}
      errorText={branchActive
        ? (branch.stateFor(paneId)?.error?.message ?? undefined)
        : store.error?.message}
      sortKey={effSort.key}
      sortDir={effSort.dir}
      onsort={setSort}
      onlistkey={onListKey}
      onlistfocus={ensureCursor}
      onrowclick={rowClick}
      onrowactivate={rowActivate}
      onup={() => void store.goUp()}
    />
    {#if branchActive && branch.isLoading(paneId)}
      <button type="button" class="branchCancel" onclick={() => branch.cancel(paneId)}>
        Cancel
      </button>
    {/if}
    <PaneStatusBar
      status={statusText}
      readonly={inArchive}
      drive={driveStatus.driveFor(store.path)}
    />
  </div>

  {#if dropHover}
    <!-- Drop overlay: visual copy of prototype v2's pane.dropOn block. -->
    <div class="dropOverlay" aria-hidden="true">
      <div class="dropPill">
        <svg
          width="14"
          height="14"
          viewBox="0 0 24 24"
          fill="none"
          stroke="var(--accentInk)"
          stroke-width="2"
          stroke-linecap="round"
          stroke-linejoin="round"
          aria-hidden="true"
        >
          <rect x="8" y="8" width="12" height="12" rx="2" />
          <path d="M16 8V5a1 1 0 0 0-1-1H5a1 1 0 0 0-1 1v10a1 1 0 0 0 1 1h3" />
        </svg>
        <span>{dropMove ? 'Move' : 'Copy'} {dropItems} items</span>
        {#if !dropMove}<span class="dropHint">· hold ⇧ to move</span>{/if}
      </div>
    </div>
  {/if}
</section>

<style>
  .pane {
    position: relative;
    min-width: 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
    background: var(--pane);
    transition: background-color 160ms;
  }
  .pane.active {
    background: var(--paneActive);
  }
  .topLine {
    position: absolute;
    left: 0;
    right: 0;
    top: 0;
    height: 2px;
    background: var(--accent);
    opacity: 0;
    transition: opacity 160ms;
    pointer-events: none;
    z-index: 2;
  }
  .pane.active .topLine {
    opacity: 0.85;
  }
  /* Files chrome keeps its flex column. DSK-10: Disk usage is no longer a
     pane mode — the app-level workspace replaces the whole grid while open,
     so this subtree is always visible in the Files mode. */
  .filesMode {
    flex: 1;
    min-height: 0;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  /* Drop overlay (prototype v2: inset dashed accent line + centered pill). */
  .dropOverlay {
    position: absolute;
    inset: 6px;
    border-radius: var(--rl);
    border: 1.5px dashed var(--accentLine);
    background: var(--accentSoft);
    pointer-events: none;
    display: grid;
    place-items: center;
    animation: fmFade 120ms ease-out;
    z-index: 3;
  }
  .dropPill {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 14px;
    border-radius: 999px;
    background: var(--elev);
    box-shadow: var(--shadow);
    font-size: calc(12.5px + var(--uiStep, 0px));
  }
  .dropHint {
    color: var(--text3);
  }
  /* FER-128: Cancel for a streaming branch walk, floating above the status bar. */
  .branchCancel {
    position: absolute;
    right: 14px;
    bottom: 34px;
    z-index: 2;
    padding: 4px 12px;
    border-radius: 999px;
    border: 1px solid var(--accentLine);
    background: var(--elev);
    color: inherit;
    box-shadow: var(--shadow);
    font-size: calc(12px + var(--uiStep, 0px));
    cursor: pointer;
  }
</style>
