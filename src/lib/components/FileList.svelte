<script lang="ts">
  import FileIcon from './FileIcon.svelte';
  import ContextMenu from './ContextMenu.svelte';
  import type { Row } from '../stores/paneStore.svelte';
  import { rangeToShow, visibleRange } from '../virtual';
  import { view } from '../stores/viewStore.svelte';
  import { ui } from '../uiState.svelte';
  import FileGrid from './FileGrid.svelte';
  import { contextMenu, type ContextMenuItem } from '../stores/contextMenuStore.svelte';
  import { agentSubmenuItem } from '../ai/agentTerminal';
  import { dialogs } from '../stores/dialogStore.svelte';
  import { activePane } from '../stores/panesRegistry.svelte';
  import { shell } from '../stores/shellStore.svelte';
  import { renameEntry } from '../ipc/mutations';
  import { FsError } from '../ipc/types';
  import { activity } from '../stores/activityStore.svelte';
  import { folderSizes } from '../stores/folderSizesStore.svelte';
  import { getAction, isEnabled, labelOf, runAction, shortcutLabel } from '../actions';
  import ColumnResizeHandle from './ColumnResizeHandle.svelte';
  import { columns as columnsStore } from '../stores/columnsStore.svelte';
  import {
    columnLayout,
    extensionText,
    displayName,
    DEFAULT_COLUMN_CONFIG,
    DEFAULT_FILE_LIST_COLUMN_WIDTHS,
    FILE_LIST_COLUMN_MINS,
    FILE_LIST_COLUMN_MAX,
    FILE_LIST_DEFAULT_COMMENT_WIDTH,
    type ColumnConfig,
    type FileListColumnWidths,
    type ResizableColumnId,
  } from '../columns';

  let {
    rows,
    selected = new Set<string>(),
    cursor,
    loading = false,
    errorText,
    notice = null,
    /** 'full' shows the configured columns; 'brief' is names-only (F2-15). */
    mode = 'full',
    /** F2-365 Custom Columns: visible columns + order for Full mode. */
    columns = DEFAULT_COLUMN_CONFIG,
    /** FER-541: Optional column widths override / external state. */
    columnWidths,
    /** F2-27 Comments view: extra comment column fed by the sidecar map. */
    showComments = false,
    comments,
    /** Test hook: jsdom has no layout, so real measurement yields 0. */
    viewportOverride,
    sortKey = 'name',
    sortDir = 1,
    onsort,
    oncolumnwidthchange,
    oncolumnwidthcommit,
    onlistkey,
    onlistfocus,
    onrowclick,
    onrowactivate,
    onup,
  }: {
    rows: Row[];
    selected?: Set<string>;
    cursor?: string;
    loading?: boolean;
    errorText?: string;
    /** FER-457: small note when the tab was moved to a nearer existing folder. */
    notice?: string | null;
    mode?: 'brief' | 'full' | 'thumbnail';
    columns?: ColumnConfig;
    columnWidths?: FileListColumnWidths;
    showComments?: boolean;
    comments?: Record<string, string>;
    viewportOverride?: number;
    sortKey?: 'name' | 'size' | 'modified' | 'extension' | 'unsorted';
    sortDir?: 1 | -1;
    onsort?: (key: 'name' | 'size' | 'modified' | 'extension') => void;
    oncolumnwidthchange?: (col: ResizableColumnId, width: number) => void;
    oncolumnwidthcommit?: (col: ResizableColumnId, width: number) => void;
    onlistkey?: (e: KeyboardEvent) => void;
    /** FER-489: the list container received focus (pane puts a cursor on a row). */
    onlistfocus?: () => void;
    onrowclick?: (row: Row, e: MouseEvent) => void;
    onrowactivate?: (row: Row) => void;
    onup?: () => void;
  } = $props();

  const brief = $derived(mode === 'brief');
  // Thumbnail view (F2-27) rides the grid renderer with image previews.
  const thumbnails = $derived(mode === 'thumbnail');
  // Comments column (F2-27) exists only in Full mode, like Size/Modified.
  const commentsView = $derived(showComments && !brief);

  let commentHeadEl = $state<HTMLElement | undefined>();
  let measuredCommentWidth = $state(FILE_LIST_DEFAULT_COMMENT_WIDTH);

  $effect(() => {
    const el = commentHeadEl;
    if (!el) return;
    const initial = el.getBoundingClientRect().width;
    if (Number.isFinite(initial) && initial > 0) {
      measuredCommentWidth = Math.round(initial);
    }
    if (typeof ResizeObserver === 'undefined') return;
    const observer = new ResizeObserver((entries) => {
      for (const entry of entries) {
        const w = entry.borderBoxSize?.[0]?.inlineSize ?? entry.contentRect.width;
        if (Number.isFinite(w) && w > 0) measuredCommentWidth = Math.round(w);
      }
    });
    observer.observe(el);
    return () => observer.disconnect();
  });

  let dragWidths = $state<Partial<FileListColumnWidths>>({});

  const committedWidths = $derived<FileListColumnWidths>({
    ...columnsStore.widths,
    ...columnWidths,
  });

  const liveWidths = $derived<FileListColumnWidths>({
    ...committedWidths,
    ...dragWidths,
  });

  function effectiveColWidth(col: ResizableColumnId): number {
    if (col === 'comment') {
      if (liveWidths.comment !== null) return liveWidths.comment;
      return measuredCommentWidth;
    }
    return liveWidths[col] ?? DEFAULT_FILE_LIST_COLUMN_WIDTHS[col] ?? 72;
  }

  let rafId: number | null = null;
  let pendingDrag: { col: ResizableColumnId; width: number } | null = null;

  function handleColChange(col: ResizableColumnId, width: number): void {
    // In production browser environments, throttle pointermove to screen refresh rate (60/120Hz)
    // via requestAnimationFrame so rapid pointer events don't trigger redundant updates.
    // In test environments (node/jsdom), update synchronously so tests can verify live updates.
    if (
      typeof window !== 'undefined' &&
      typeof window.requestAnimationFrame === 'function' &&
      typeof process === 'undefined'
    ) {
      pendingDrag = { col, width };
      if (rafId === null) {
        rafId = window.requestAnimationFrame(() => {
          rafId = null;
          if (pendingDrag) {
            dragWidths = { ...dragWidths, [pendingDrag.col]: pendingDrag.width };
            oncolumnwidthchange?.(pendingDrag.col, pendingDrag.width);
            pendingDrag = null;
          }
        });
      }
    } else {
      dragWidths = { ...dragWidths, [col]: width };
      oncolumnwidthchange?.(col, width);
    }
  }

  function handleColCommit(col: ResizableColumnId, width: number): void {
    if (rafId !== null && typeof cancelAnimationFrame === 'function') {
      cancelAnimationFrame(rafId);
      rafId = null;
    }
    pendingDrag = null;
    const next = { ...dragWidths };
    delete next[col];
    dragWidths = next;
    columnsStore.setWidth(col, width);
    oncolumnwidthcommit?.(col, width);
  }

  function handleColCancel(col: ResizableColumnId): void {
    if (rafId !== null && typeof cancelAnimationFrame === 'function') {
      cancelAnimationFrame(rafId);
      rafId = null;
    }
    pendingDrag = null;
    const next = { ...dragWidths };
    delete next[col];
    dragWidths = next;
  }

  function handleColReset(col: ResizableColumnId): void {
    if (rafId !== null && typeof cancelAnimationFrame === 'function') {
      cancelAnimationFrame(rafId);
      rafId = null;
    }
    pendingDrag = null;
    const next = { ...dragWidths };
    delete next[col];
    dragWidths = next;
    columnsStore.resetWidth(col);
  }

  // F2-365: Full mode renders the configured columns; the default config
  // reproduces the built-in Name/Size/Modified grid exactly.
  const layout = $derived(
    brief || thumbnails
      ? null
      : columnLayout(columns, { comments: commentsView, widths: liveWidths }),
  );

  let bodyEl = $state<HTMLElement | undefined>();
  let measuredH = $state(0);
  let scrollTop = $state(0);

  // jsdom (tests) has no layout: measurement stays 0. Fall back to a
  // typical viewport so the window math still renders rows; real browsers
  // always have a positive measurement before first paint matters.
  const viewportH = $derived(viewportOverride ?? (measuredH > 0 ? measuredH : 600));
  // +1 for the ".." row that always occupies index 0.
  const total = $derived(rows.length + 1);
  // FER-377 density + D50 coupling: density base (26/30) OR the minimum for
  // the current text level (13+step)*1.85, whichever is larger; the
  // virtualizer math gets the same number so offsets never drift.
  const UI_STEP_PX: Record<string, number> = { s: -1, m: 0, l: 2, xl: 4 };
  const rowPx = $derived.by(() => {
    const base = ui.density === 'comfortable' ? 30 : 26;
    const min = Math.ceil((13 + (UI_STEP_PX[ui.uiFontLevel] ?? 0)) * 1.85);
    return Math.max(base, min);
  });
  const range = $derived(visibleRange(scrollTop, viewportH, total, 8, rowPx));
  const start = $derived(Math.max(0, range.start - 1)); // real-row slice start
  const slice = $derived(rows.slice(start, Math.max(0, range.end - 1)));

  // FER-477a: folder sizes from the FerrumIndexer for the folders on screen (opt-in; the
  // service answers only in High performance mode — otherwise the cell stays "—").
  const showsSize = $derived(!!layout && layout.columns.some((c) => c.id === 'size'));
  $effect(() => {
    if (!ui.indexerFolderSizes || !showsSize) return;
    folderSizes.want(
      slice
        .filter((r) => r.entry.isDir && !r.entry.isSymlink)
        .map((r) => ({ path: r.entry.path, name: r.entry.name })),
    );
  });

  function sizeCell(r: Row): string {
    if (r.sizeText) return r.sizeText;
    if (r.entry.isDir && !r.entry.isSymlink) return folderSizes.text(r.entry.path) ?? '—';
    return '—';
  }

  // Keyboard/focus correctness: when the cursor moves outside the viewport,
  // scroll just enough to reveal it (task 08 drives cursor via arrow keys).
  $effect(() => {
    const path = cursor;
    if (!path || !bodyEl) return;
    const idx = rows.findIndex((r) => r.entry.path === path);
    if (idx < 0) return;
    const vp = viewportOverride ?? bodyEl.clientHeight;
    const target = rangeToShow(idx + 1, bodyEl.scrollTop, vp, rowPx);
    if (target !== null) {
      bodyEl.scrollTop = target;
      scrollTop = target; // keep the window in sync (jsdom fires no scroll event)
    }
  });

  // ── Context menu ────────────────────────────────────────────────────────
  // Private token so only THIS pane's mounted ContextMenu renders the
  // (position:fixed) overlay — FileList exists once per pane.
  const menuOwner = {};

  let menuOpener: HTMLElement | undefined;

  /** Menu key / Shift+F10: open the row menu at the cursor row (FER-577). */
  function onBodyKey(e: KeyboardEvent) {
    const isMenuKey =
      (e.key === 'ContextMenu' && !e.shiftKey) || (e.key === 'F10' && e.shiftKey && !e.altKey);
    if (!isMenuKey || e.ctrlKey || e.metaKey || !cursor) {
      onlistkey?.(e);
      return;
    }
    const row = rows.find((r) => r.entry.path === cursor);
    if (!row) return onlistkey?.(e);
    e.preventDefault();
    const el = Array.from(bodyEl?.querySelectorAll<HTMLElement>('.row[data-path]') ?? []).find(
      (n) => n.dataset.path === cursor,
    );
    const box = (el ?? bodyEl)?.getBoundingClientRect();
    rowContextmenu(row, e, el, box ? { x: box.left + 28, y: box.bottom } : undefined);
  }

  function rowContextmenu(r: Row, e: Event, opener?: HTMLElement, at?: { x: number; y: number }) {
    e.preventDefault();
    if (renamingPath) cancelRename(); // another action cancels inline rename
    // Right-clicking an unselected row behaves like a plain left-click first.
    if (!selected.has(r.entry.path)) onrowclick?.(r, new MouseEvent('click'));
    const items: ContextMenuItem[] = [];
    function add(id: string) {
      const action = getAction(id);
      if (!action) return;
      items.push({
        label: labelOf(action),
        kbd: action.shortcut ? shortcutLabel(action.shortcut) : undefined,
        disabled: !isEnabled(action),
        danger: id.startsWith('file.delete'),
        run: () => void runAction(action),
      });
    }
    if (activePane()?.cursorEntry()) add('file.open'); // folder navigates, file opens externally (F2-25)
    if (r.entry.isDir) {
      items.push({
        label: 'Open Terminal Here',
        run: () => void shell.openTerminalAt(r.entry.path),
      });
    }
    if (!activePane()?.cursorEntry()?.isDir) {
      add('file.associate');
      add('file.comment');
    }
    // Manual pin (spec §5/§7): acts on the RIGHT-CLICKED row — never the
    // active pane's cursor, which may belong to the other pane in dual-pane.
    // Files and folders both pin; favorites files open via the OS default.
    if (!r.entry.isSymlink && activity.available) {
      const kind = r.entry.isDir ? 'folder' : 'file';
      const pinned = activity.isFavorite(r.entry.path);
      items.push({
        label: pinned ? 'Unpin from Favorites' : 'Pin to Favorites',
        run: () => void activity.toggleFavorite(r.entry.path, kind),
      });
    }
    add('ai.addToChat'); // FER-486: ask the AI about the selection
    const agentItem = agentSubmenuItem(r.entry.path); // FER-494: file -> its parent folder
    if (agentItem) items.push(agentItem);
    items.push({ separator: true });
    add('file.copy');
    add('file.move');
    items.push({ separator: true });
    if (selected.size <= 1) {
      add('file.rename');
      items.push({ label: 'Rename inline', run: () => beginRename(r) });
    }
    add('file.delete');
    add('file.delete.permanent');
    items.push({ separator: true });
    add('file.mkdir');
    add('file.newfile');
    menuOpener = opener ?? (e.currentTarget instanceof HTMLElement ? e.currentTarget : undefined);
    // A keyboard-raised contextmenu event carries (0, 0): anchor it to the row instead.
    const me = e as MouseEvent;
    const x =
      at?.x ??
      (me.clientX === 0 && me.clientY === 0 && menuOpener
        ? menuOpener.getBoundingClientRect().left + 28
        : me.clientX);
    const y =
      at?.y ??
      (me.clientX === 0 && me.clientY === 0 && menuOpener
        ? menuOpener.getBoundingClientRect().bottom
        : me.clientY);
    contextMenu.open(x, y, items, menuOwner);
  }

  // Focus returns to the row that opened the menu when it closes — unless an
  // inline rename started from it (the input takes focus instead, and focus
  // comes back here once the rename settles).
  $effect(() => {
    if (contextMenu.isOpen || renamingPath) return;
    const el = menuOpener;
    menuOpener = undefined;
    if (el?.isConnected) el.focus();
  });

  // ── Inline rename (explicit row-only action; registry Rename opens dialog) ──
  let renamingPath = $state<string>();
  let renameDraft = $state('');
  let renameInputEl = $state<HTMLInputElement | undefined>();
  let renameOriginal = '';
  let renameIsDir = false;

  function beginRename(row: Row) {
    renamingPath = row.entry.path;
    renameOriginal = row.entry.name;
    renameIsDir = row.entry.isDir && !row.entry.isSymlink;
    renameDraft = row.entry.name;
  }

  function cancelRename() {
    renamingPath = undefined;
    renameDraft = '';
  }

  async function commitRename() {
    const path = renamingPath;
    if (!path) return;
    const next = renameDraft.trim();
    // Clear state FIRST: blur (element removal, click-away) must not re-commit.
    renamingPath = undefined;
    renameDraft = '';
    if (!next || next === renameOriginal) return; // empty/unchanged = cancel
    try {
      await renameEntry(path, next);
      activity.noteFsChange();
      activePane()?.refresh();
    } catch (err) {
      dialogs.open({
        kind: 'error',
        title: 'Rename',
        message: err instanceof FsError ? err.message : String(err),
      });
    }
  }

  function renameKeydown(e: KeyboardEvent) {
    // Keep list navigation and global shortcuts away from the input.
    e.stopPropagation();
    if (e.key === 'Enter') {
      e.preventDefault();
      void commitRename();
    } else if (e.key === 'Escape') {
      e.preventDefault();
      cancelRename();
    }
  }

  // Focus + basename selection on the freshly rendered input: files select
  // everything before the last dot (dotfiles keep the whole name), folders none.
  $effect(() => {
    const path = renamingPath;
    const input = renameInputEl;
    if (!path || !input) return;
    input.focus();
    if (renameIsDir) return;
    const dot = renameOriginal.lastIndexOf('.');
    input.setSelectionRange(0, dot > 0 ? dot : renameOriginal.length);
  });

  // A rename outliving its row (refresh, navigation) must not linger hidden.
  $effect(() => {
    const path = renamingPath;
    if (path && !rows.some((r) => r.entry.path === path)) cancelRename();
  });
</script>

<div
  class="head"
  class:brief
  class:withComments={commentsView}
  role="row"
  style:grid-template-columns={layout ? layout.gridTemplate : undefined}
>
  {#if layout}
    {#each layout.columns as c (c.id)}
      {#if c.id === 'name'}
        <button class="hName" onclick={() => onsort?.('name')}>
          Name <span class="arrow">{sortKey === 'name' ? (sortDir === 1 ? '↑' : '↓') : ''}</span>
        </button>
      {:else if c.id === 'comment'}
        <div class="hComment" bind:this={commentHeadEl}>
          <span>Comment</span>
          <ColumnResizeHandle
            label="Resize Comment column"
            value={effectiveColWidth('comment')}
            min={FILE_LIST_COLUMN_MINS.comment}
            max={FILE_LIST_COLUMN_MAX}
            step={16}
            side="left"
            onchange={(w) => handleColChange('comment', w)}
            oncommit={(w) => handleColCommit('comment', w)}
            oncancel={() => handleColCancel('comment')}
            onreset={() => handleColReset('comment')}
          />
        </div>
      {:else}
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
        <div
          class="{c.id === 'size' ? 'hSize' : c.id === 'extension' ? 'hExt' : 'hMod'} hCol"
          onclick={(e) => {
            if (
              e.target !== e.currentTarget &&
              (e.target as HTMLElement).closest('[role="separator"]')
            )
              return;
            onsort?.(c.id as 'size' | 'extension' | 'modified');
          }}
        >
          <button
            type="button"
            class="sortBtn"
            class:hSizeBtn={c.id === 'size'}
            onclick={(e) => {
              e.stopPropagation();
              onsort?.(c.id as 'size' | 'extension' | 'modified');
            }}
          >
            {#if c.id === 'size'}
              <span class="arrow">{sortKey === 'size' ? (sortDir === 1 ? '↑' : '↓') : ''}</span>
            {/if}
            {c.label}
            {#if c.id !== 'size'}
              <span class="arrow">{sortKey === c.id ? (sortDir === 1 ? '↑' : '↓') : ''}</span>
            {/if}
          </button>
          <ColumnResizeHandle
            label={`Resize ${c.label} column`}
            value={effectiveColWidth(c.id as ResizableColumnId)}
            min={FILE_LIST_COLUMN_MINS[c.id as ResizableColumnId]}
            max={FILE_LIST_COLUMN_MAX}
            step={16}
            side="left"
            onchange={(w) => handleColChange(c.id as ResizableColumnId, w)}
            oncommit={(w) => handleColCommit(c.id as ResizableColumnId, w)}
            oncancel={() => handleColCancel(c.id as ResizableColumnId)}
            onreset={() => handleColReset(c.id as ResizableColumnId)}
          />
        </div>
      {/if}
    {/each}
  {:else}
    <button class="hName" onclick={() => onsort?.('name')}>
      Name <span class="arrow">{sortKey === 'name' ? (sortDir === 1 ? '↑' : '↓') : ''}</span>
    </button>
  {/if}
</div>

{#if notice}
  <div class="notice" role="status">{notice}</div>
{/if}

<div
  class="body"
  bind:this={bodyEl}
  bind:clientHeight={measuredH}
  data-pane-list
  tabindex="0"
  onkeydown={onBodyKey}
  onfocus={() => onlistfocus?.()}
  onfocusin={(e) => {
    // FER-492: rows are virtualized, so a focused row element is unmounted once keys scroll it
    // away and focus would fall to <body>, killing every later key. Keep focus on the list.
    const t = e.target;
    if (t instanceof HTMLElement && t !== bodyEl && t.classList.contains('row')) {
      bodyEl?.focus({ preventScroll: true });
    }
  }}
  role="grid"
  aria-label="File list"
  aria-rowcount={total}
  aria-busy={loading}
  onscroll={() => {
    scrollTop = bodyEl?.scrollTop ?? 0;
    contextMenu.close(); // scrolled away from where the menu was anchored
  }}
>
  {#if errorText}
    <div class="state" role="alert">
      <div class="stateTitle">Can't open this folder</div>
      <div class="stateText">{errorText}</div>
    </div>
  {:else if loading}
    <div class="state"><div class="stateText">Loading…</div></div>
  {:else if view.mode === 'grid'}
    <FileGrid {rows} {selected} {cursor} {onrowclick} {onrowactivate} {onup} />
  {:else if thumbnails}
    <FileGrid {rows} {selected} {cursor} {onrowclick} {onrowactivate} {onup} thumbnails />
  {:else}
    <!-- total-height sizer: scrollbars behave as if all rows existed -->
    <div class="sizer" style:height="{total * rowPx}px">
      <div class="window" style:transform="translateY({start * rowPx}px)">
        {#if range.start === 0}
          <div
            class="row upRow"
            class:brief
            role="row"
            aria-rowindex={1}
            onclick={() => onup?.()}
            title="Parent folder"
            style:grid-template-columns={layout ? layout.gridTemplate : undefined}
          >
            <div class="name">
              <span class="ico">
                <svg
                  width="14"
                  height="14"
                  viewBox="0 0 24 24"
                  fill="none"
                  stroke="var(--text3)"
                  stroke-width="2"
                  stroke-linecap="round"
                  stroke-linejoin="round"
                  aria-hidden="true"
                >
                  <path d="M9 14L4 9l5-5" /><path d="M4 9h10.5a5.5 5.5 0 0 1 0 11H11" />
                </svg>
              </span>
              <span class="nameText">..</span>
            </div>
            {#if layout}
              {#each layout.columns as c (c.id)}
                {#if c.id === 'size'}
                  <div class="size">—</div>
                {:else if c.id !== 'name'}
                  <div
                    class={c.id === 'extension' ? 'ext' : c.id === 'comment' ? 'comment' : 'mod'}
                  ></div>
                {/if}
              {/each}
            {/if}
          </div>
        {/if}
        {#each slice as r, i (r.entry.path)}
          <div
            class="row"
            class:brief
            class:withComments={commentsView}
            class:selected={selected.has(r.entry.path)}
            class:cursor={cursor === r.entry.path}
            role="row"
            aria-rowindex={start + i + 2}
            aria-selected={selected.has(r.entry.path)}
            data-path={r.entry.path}
            tabindex="-1"
            title={commentsView && comments?.[r.entry.name] ? comments[r.entry.name] : undefined}
            onclick={(e) => onrowclick?.(r, e)}
            ondblclick={() => onrowactivate?.(r)}
            oncontextmenu={(e) => rowContextmenu(r, e)}
            style:grid-template-columns={layout ? layout.gridTemplate : undefined}
          >
            {#if layout}
              {#each layout.columns as c (c.id)}
                {#if c.id === 'name'}
                  <div class="name">
                    <span class="ico"><FileIcon kind={r.kind} name={r.entry.name} /></span>
                    {#if renamingPath === r.entry.path}
                      <input
                        class="renameInput"
                        bind:this={renameInputEl}
                        bind:value={renameDraft}
                        onkeydown={renameKeydown}
                        onblur={() => void commitRename()}
                        spellcheck="false"
                        aria-label="Rename {r.entry.name}"
                      />
                    {:else}
                      <span class="nameText"
                        >{displayName(r.entry.name, r.entry.isDir, ui.showExtensions)}</span
                      >
                    {/if}
                  </div>
                {:else if c.id === 'size'}
                  <div class="size">{sizeCell(r)}</div>
                {:else if c.id === 'modified'}
                  <div class="mod">{r.modText}</div>
                {:else if c.id === 'extension'}
                  <div class="ext">{extensionText(r.entry.name, r.entry.isDir)}</div>
                {:else if c.id === 'comment'}
                  <div class="comment">{comments?.[r.entry.name] ?? ''}</div>
                {/if}
              {/each}
            {:else}
              <div class="name">
                <span class="ico"><FileIcon kind={r.kind} name={r.entry.name} /></span>
                {#if renamingPath === r.entry.path}
                  <input
                    class="renameInput"
                    bind:this={renameInputEl}
                    bind:value={renameDraft}
                    onkeydown={renameKeydown}
                    onblur={() => void commitRename()}
                    spellcheck="false"
                    aria-label="Rename {r.entry.name}"
                  />
                {:else}
                  <span class="nameText"
                    >{displayName(r.entry.name, r.entry.isDir, ui.showExtensions)}</span
                  >
                {/if}
              </div>
            {/if}
          </div>
        {/each}
      </div>
    </div>
  {/if}
</div>

<ContextMenu owner={menuOwner} />

<style>
  .head {
    height: 26px;
    flex: none;
    display: grid;
    grid-template-columns: minmax(0, 1fr) 72px 132px;
    gap: 12px;
    align-items: center;
    padding: 0 12px 0 32px;
    border-bottom: 1px solid var(--line);
    font-size: calc(11.5px + var(--uiStep, 0px));
    color: var(--text3);
    font-weight: 500;
  }
  /* Brief mode (F2-15): names only, one column. */
  .head.brief,
  .row.brief {
    grid-template-columns: minmax(0, 1fr);
  }
  /* Comments view (F2-27): the comment text takes a flexible extra column. */
  .head.withComments,
  .row.withComments {
    grid-template-columns: minmax(0, 1fr) 72px 132px minmax(0, 1.6fr);
  }
  .head > button,
  .sortBtn {
    position: relative;
    border: 0;
    padding: 0;
    background: transparent;
    font: inherit;
    font-weight: 500;
    color: var(--text3);
    white-space: nowrap;
    overflow: hidden;
    text-align: left;
    cursor: pointer;
  }
  .head > button:hover,
  .sortBtn:hover {
    color: var(--text2);
  }
  .hCol {
    position: relative;
    height: 100%;
    display: flex;
    align-items: center;
    min-width: 0;
  }
  .sortBtn {
    width: 100%;
    height: 100%;
    display: flex;
    align-items: center;
    gap: 2px;
  }
  .hSize .sortBtn {
    justify-content: flex-end;
    text-align: right;
  }
  .arrow {
    color: var(--accentInk);
  }
  .hSize {
    text-align: right;
  }
  .body:focus {
    outline: none;
  }
  /* FER-489: the visible focus outline lives on the cursor row, not on the list container. */
  .body:focus-within .row.cursor {
    box-shadow: inset 0 0 0 2px var(--accentLine);
  }
  .body {
    flex: 1;
    min-height: 0;
    overflow: auto;
    position: relative;
  }
  .sizer {
    position: relative;
    width: 100%;
  }
  .window {
    position: absolute;
    left: 0;
    right: 0;
    top: 0;
    will-change: transform;
  }
  .row {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 72px 132px;
    gap: 12px;
    align-items: center;
    height: var(--row); /* = rowPx (density-driven); virtualization depends on it */
    padding: 0 12px 0 10px;
    margin: 0 4px;
    border-radius: calc(var(--r) - 1px);
    background-color: transparent;
    cursor: default;
    transition: background-color 120ms;
  }
  .row:hover {
    background-image: linear-gradient(var(--hover), var(--hover));
  }
  .row.selected {
    background-color: var(--sel);
  }
  .row.cursor {
    box-shadow: inset 0 0 0 1px var(--accentLine);
  }
  .name {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
  }
  .ico {
    width: 14px;
    height: 16px;
    flex: none;
    display: grid;
    place-items: center;
  }
  .nameText {
    min-width: 0;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    font-size: calc(13px + var(--uiStep, 0px));
  }
  .row.cursor .nameText {
    font-weight: 500;
  }
  .renameInput {
    flex: 1;
    min-width: 0;
    height: 20px;
    padding: 0 5px;
    margin-left: -5px;
    border: 0;
    outline: 0;
    border-radius: 4px;
    background: var(--elev);
    box-shadow: 0 0 0 1.5px var(--accent);
    font-family: inherit;
    font-size: calc(13px + var(--uiStep, 0px));
    color: var(--text);
  }
  .upRow .nameText {
    color: var(--text2);
  }
  .size,
  .mod {
    font-family: 'Geist Mono', monospace;
    font-size: calc(11.5px + var(--uiStep, 0px));
    color: var(--text2);
    white-space: nowrap;
    overflow: hidden;
  }
  .size {
    text-align: right;
  }
  /* Custom Columns (F2-365): the extension cell matches the size/mod tone. */
  .ext {
    font-family: 'Geist Mono', monospace;
    font-size: calc(11.5px + var(--uiStep, 0px));
    color: var(--text2);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .hComment {
    position: relative;
    white-space: nowrap;
    overflow: hidden;
  }
  .comment {
    font-size: calc(12px + var(--uiStep, 0px));
    color: var(--text2);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .state {
    padding: 32px 16px;
    text-align: center;
  }
  .notice {
    padding: 4px 12px;
    font-size: calc(12px + var(--uiStep, 0px));
    color: var(--text2);
    background: var(--hover, transparent);
    border-bottom: 1px solid var(--border, transparent);
    word-break: break-all;
  }
  .stateTitle {
    font-size: calc(13px + var(--uiStep, 0px));
    font-weight: 600;
    color: var(--text2);
    margin-bottom: 4px;
  }
  .stateText {
    font-size: calc(12.5px + var(--uiStep, 0px));
    color: var(--text3);
    word-break: break-all;
  }
</style>
