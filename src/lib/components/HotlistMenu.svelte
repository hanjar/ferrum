<script lang="ts">
  // Ctrl+D popup (FER-458): nested hotlist with accelerators, submenus, ✓ on the active folder,
  // ▶︎ on command items, a "net" tag on folders on a network share (FER-467 R8) and the Add /
  // Remove / Configure footer. Also the once-per-host question before a network folder is opened and
  // the one-line notice (conflict, read-only, launch failure). Command items are confirmed by the
  // backend in a native Windows dialog (FER-467 R5), not here.
  import { tick } from 'svelte';
  import { hotlist } from '../stores/hotlistStore.svelte';
  import { activePane } from '../stores/panesRegistry.svelte';
  import { samePath } from '../hotlist/model';
  import { isNetworkPath } from '../networkPath';
  import {
    cycle,
    firstSelectable,
    isSelectable,
    lastSelectable,
    matchKey,
    rowsFor,
    step,
    type Row,
  } from '../hotlist/menuRows';

  const MENU_W = 272;

  let root = $state<HTMLElement>();
  let cursor = $state<number[]>([]);
  /** The submenu under the LAST cursor entry is shown (hover/click), not just highlighted. */
  let peek = $state(false);
  let pos = $state({ left: 8, top: 40 });
  let previousFocus: HTMLElement | null = null;

  const currentPath = $derived(hotlist.menuOpen ? (activePane()?.path ?? '') : '');
  const listed = $derived(currentPath !== '' && hotlist.isListed(currentPath));
  const opts = $derived({
    canAdd: currentPath !== '' && !listed,
    canRemove: listed,
    readOnly: hotlist.readOnly,
  });

  /** One row list per open level; the last cursor entry may itself be a submenu (shown open). */
  const levels = $derived.by<Row[][]>(() => {
    const out: Row[][] = [rowsFor(hotlist.doc.items, [], opts)];
    for (let k = 0; k < cursor.length; k++) {
      const row = out[k]?.[cursor[k]];
      const shown = k < cursor.length - 1 || peek;
      if (shown && row?.kind === 'item' && row.item.type === 'submenu') {
        out.push(rowsFor(row.item.items, row.path, opts));
      } else break;
    }
    return out;
  });

  function place() {
    const pane = activePane() ? hotlist.menuPane : 0;
    const anchor =
      document.querySelector<HTMLElement>(`[data-pane="${pane}"] button.hot`) ??
      document.querySelector<HTMLElement>(`[data-pane="${pane}"]`);
    const r = anchor?.getBoundingClientRect();
    const vw = window.innerWidth;
    const left = r ? (anchor?.matches('button.hot') ? r.right - MENU_W : r.left + 12) : 12;
    pos = {
      left: Math.max(8, Math.min(left, vw - MENU_W - 8)),
      top: r ? (anchor?.matches('button.hot') ? r.bottom + 4 : r.top + 40) : 48,
    };
  }

  $effect(() => {
    if (!hotlist.menuOpen) return;
    previousFocus = document.activeElement as HTMLElement | null;
    place();
    peek = false;
    cursor = [firstSelectable(rowsFor(hotlist.doc.items, [], opts))].filter((i) => i >= 0);
    void tick().then(() => root?.focus());
    return () => {
      const el = previousFocus;
      previousFocus = null;
      queueMicrotask(() => el?.focus?.());
    };
  });

  function rowAt(depth: number): Row | undefined {
    return levels[depth]?.[cursor[depth]];
  }

  function close() {
    hotlist.closeMenu();
  }

  function enter(depth: number) {
    const row = levels[depth]?.[cursor[depth]];
    if (row?.kind !== 'item' || row.item.type !== 'submenu') return;
    const first = firstSelectable(rowsFor(row.item.items, row.path, opts));
    peek = true;
    if (first >= 0) cursor = [...cursor.slice(0, depth + 1), first];
  }

  function activate(depth: number, index: number) {
    const row = levels[depth]?.[index];
    if (!isSelectable(row)) return;
    cursor = [...cursor.slice(0, depth), index];
    if (row?.kind === 'item') {
      if (row.item.type === 'submenu') {
        enter(depth);
        return;
      }
      hotlist.activate(row.item, row.path);
      close();
    } else if (row?.kind === 'action') {
      if (row.id === 'add') void hotlist.addFolder(currentPath);
      else if (row.id === 'remove') void hotlist.removeFolder(currentPath);
      else hotlist.openConfigure();
      if (row.id !== 'configure') close();
    }
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.ctrlKey || e.metaKey || e.altKey) {
      if (e.key.toLowerCase() === 'd' && (e.ctrlKey || e.metaKey)) {
        e.preventDefault();
        e.stopPropagation();
        close();
      }
      return;
    }
    const depth = Math.max(0, cursor.length - 1);
    const rows = levels[depth] ?? [];
    const at = cursor[depth] ?? -1;
    const move = (i: number) => {
      if (i < 0) return;
      peek = false;
      cursor = [...cursor.slice(0, depth), i];
    };
    switch (e.key) {
      case 'ArrowDown':
        move(step(rows, at, 1));
        break;
      case 'ArrowUp':
        move(step(rows, at, -1));
        break;
      case 'Home':
        move(firstSelectable(rows));
        break;
      case 'End':
        move(lastSelectable(rows));
        break;
      case 'ArrowRight': {
        const row = rowAt(depth);
        if (row?.kind === 'item' && row.item.type === 'submenu') enter(depth);
        break;
      }
      case 'ArrowLeft':
        if (cursor.length > 1) {
          cursor = cursor.slice(0, -1);
          peek = false;
        }
        break;
      case 'Escape':
        if (cursor.length > 1) {
          cursor = cursor.slice(0, -1);
          peek = false;
        } else if (peek) peek = false;
        else close();
        break;
      case 'Enter':
      case ' ':
        if (at >= 0) activate(depth, at);
        break;
      case 'Tab':
        break;
      default: {
        if (e.key.length !== 1) return;
        const hits = matchKey(rows, e.key);
        if (hits.length === 1) activate(depth, hits[0]);
        else if (hits.length > 1) move(cycle(hits, at));
        else return;
      }
    }
    e.preventDefault();
    e.stopPropagation();
  }

  function hover(depth: number, index: number) {
    if (!isSelectable(levels[depth]?.[index])) return;
    const row = levels[depth][index];
    const isSub = row.kind === 'item' && row.item.type === 'submenu';
    if (cursor[depth] === index && cursor.length === depth + 1 && peek === isSub) return;
    cursor = [...cursor.slice(0, depth), index];
    peek = isSub;
  }

  function flip(depth: number): boolean {
    return pos.left + MENU_W * (depth + 2) + 8 > window.innerWidth;
  }

  // ── network folder confirmation (FER-467 R8) ──
  let confirmEl = $state<HTMLElement>();
  $effect(() => {
    if (hotlist.pendingNetwork) void tick().then(() => confirmEl?.querySelector('button')?.focus());
  });
  function onConfirmKey(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      hotlist.cancelNetwork();
    }
  }
</script>

{#snippet list(depth: number)}
  {@const rows = levels[depth] ?? []}
  <ul
    class="list"
    class:sub={depth > 0}
    class:flip={depth > 0 && flip(depth - 1)}
    role="menu"
    aria-label={depth === 0 ? 'Directory Hotlist' : 'Submenu'}
  >
    {#each rows as row, i (i)}
      {#if row.kind === 'separator'}
        <li class="sep" role="separator"></li>
      {:else}
        {@const label = row.label}
        {@const isCur =
          row.kind === 'item' && row.item.type === 'dir' && samePath(row.item.path, currentPath)}
        {@const isSub = row.kind === 'item' && row.item.type === 'submenu'}
        {@const isNet =
          row.kind === 'item' &&
          row.item.type === 'dir' &&
          (isNetworkPath(row.item.path) || isNetworkPath(row.item.targetPath))}
        {@const open = isSub && cursor[depth] === i && levels[depth + 1] !== undefined}
        <li role="none" class="host">
          <!-- svelte-ignore a11y_click_events_have_key_events -->
          <div
            class="row"
            class:focus={cursor[depth] === i && cursor.length === depth + 1}
            class:path={cursor[depth] === i && cursor.length > depth + 1}
            class:disabled={row.kind === 'action' && row.disabled}
            role="menuitem"
            tabindex="-1"
            aria-label={label}
            aria-haspopup={isSub ? 'menu' : undefined}
            aria-expanded={isSub ? open : undefined}
            aria-disabled={row.kind === 'action' && row.disabled ? 'true' : undefined}
            aria-keyshortcuts={(row.kind === 'item' ? row.key : row.key) ?? undefined}
            data-depth={depth}
            onmouseenter={() => hover(depth, i)}
            onclick={() => activate(depth, i)}
          >
            <span class="acc" aria-hidden="true">
              {#if isCur}
                ✓
              {:else if row.kind === 'action'}
                {row.glyph}
              {:else if row.key}
                <u>{row.key.toUpperCase()}</u>
              {/if}
            </span>
            <span class="lbl">{label}</span>
            {#if isNet}
              <span
                class="net"
                title="Network folder: opening it contacts another computer"
                aria-label="network">net</span
              >
            {/if}
            {#if isSub}
              <span class="tail" aria-hidden="true">▸</span>
            {:else if row.kind === 'item' && row.item.type === 'command'}
              <span class="tail cmd" aria-hidden="true" title="Runs a program">▶︎</span>
            {/if}
          </div>
          {#if open}
            {@render list(depth + 1)}
          {/if}
        </li>
      {/if}
    {/each}
    {#if depth > 0 && rows.length === 0}
      <li class="empty" role="none">(empty)</li>
    {/if}
  </ul>
{/snippet}

{#if hotlist.menuOpen}
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="scrim" onmousedown={close}></div>
  <div
    class="menu"
    role="presentation"
    tabindex="-1"
    bind:this={root}
    onkeydown={onKeydown}
    style:left="{pos.left}px"
    style:top="{pos.top}px"
    style:width="{MENU_W}px"
  >
    {@render list(0)}
    {#if hotlist.readOnly}
      <p class="warn" role="status">
        Hotlist file unavailable — showing the last saved copy (read-only).
      </p>
    {/if}
  </div>
{/if}

{#if hotlist.pendingNetwork}
  {@const pending = hotlist.pendingNetwork}
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="scrim dim" onmousedown={() => hotlist.cancelNetwork()}></div>
  <div
    class="confirm"
    role="dialog"
    aria-modal="true"
    aria-label="Open network folder"
    tabindex="-1"
    bind:this={confirmEl}
    onkeydown={onConfirmKey}
  >
    <h2>Open a folder on another computer?</h2>
    <p>
      The hotlist item <strong>{pending.item.title.replace(/&(?!&)/g, '')}</strong> is on a network
      share. Opening it makes Windows connect to
      {#each pending.hosts as host, i (host)}{i > 0 ? ', ' : ''}<strong class="mono host" dir="ltr"
          >{host}</strong
        >{/each}
      and may send your Windows sign-in (NTLM) to it.
    </p>
    <p class="hint">You are asked once for each computer you have not opened before.</p>
    <div class="actions">
      <button class="ghost" onclick={() => hotlist.cancelNetwork()}>Cancel</button>
      <button class="primary" onclick={() => hotlist.confirmNetwork()}>Open</button>
    </div>
  </div>
{/if}

{#if hotlist.notice}
  <div class="notice" role="status">{hotlist.notice}</div>
{/if}

<style>
  .scrim {
    position: fixed;
    inset: 0;
    z-index: 70;
  }
  .scrim.dim {
    background: var(--scrim);
    z-index: 80;
  }
  .menu {
    position: fixed;
    z-index: 71;
    outline: none;
  }
  .list {
    margin: 0;
    padding: 4px;
    list-style: none;
    background: var(--elev);
    border: 1px solid var(--line);
    border-radius: 8px;
    box-shadow: var(--shadow);
    animation: fmPop 120ms ease-out;
    transform-origin: top left;
  }
  .list.sub {
    position: absolute;
    top: -5px;
    left: calc(100% - 2px);
    width: 272px;
  }
  .list.sub.flip {
    left: auto;
    right: calc(100% - 2px);
  }
  .host {
    position: relative;
  }
  .sep {
    height: 1px;
    margin: 4px 6px;
    background: var(--line);
  }
  .row {
    height: 26px;
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 0 8px 0 4px;
    border-radius: 6px;
    font-size: calc(12.5px + var(--uiStep, 0px));
    color: var(--text);
    cursor: default;
  }
  .row.path {
    background: var(--hover2);
  }
  .row.focus {
    background: var(--accent);
    color: #fff;
  }
  .row.disabled {
    opacity: 0.45;
  }
  .acc {
    width: 20px;
    flex: none;
    text-align: center;
    font-family: 'Geist Mono', monospace;
    font-size: calc(11.5px + var(--uiStep, 0px));
    color: var(--text3);
  }
  .row.focus .acc {
    color: inherit;
    opacity: 0.85;
  }
  .lbl {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .tail {
    flex: none;
    font-size: 10px;
    opacity: 0.7;
  }
  .tail.cmd {
    color: var(--green, currentColor);
  }
  .row.focus .tail.cmd {
    color: inherit;
  }
  .empty {
    padding: 4px 10px;
    font-size: calc(12px + var(--uiStep, 0px));
    color: var(--text3);
  }
  .warn {
    margin: 4px 0 0;
    padding: 4px 8px;
    border-radius: 6px;
    background: var(--elev);
    border: 1px solid var(--line);
    font-size: calc(11px + var(--uiStep, 0px));
    color: var(--text2);
  }
  .confirm {
    position: fixed;
    left: 50%;
    top: 24%;
    transform: translateX(-50%);
    width: min(440px, 94vw);
    padding: 16px 18px;
    border-radius: calc(var(--rl) + 2px);
    background: var(--elev);
    box-shadow: var(--shadow);
    z-index: 81;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .confirm h2 {
    margin: 0;
    font-size: calc(14px + var(--uiStep, 0px));
    font-weight: 600;
  }
  .confirm p {
    margin: 0;
    font-size: calc(12px + var(--uiStep, 0px));
    color: var(--text2);
  }
  .mono {
    font-family: 'Geist Mono', monospace;
    color: var(--text);
    word-break: break-all;
  }
  /* Whatever the file contains, it cannot reorder the surrounding text. */
  .host {
    direction: ltr;
    unicode-bidi: isolate;
  }
  .net {
    flex: none;
    padding: 0 4px;
    border: 1px solid var(--line);
    border-radius: 4px;
    font-family: 'Geist Mono', monospace;
    font-size: calc(10px + var(--uiStep, 0px));
    line-height: 14px;
    color: var(--text3);
  }
  .row.focus .net {
    color: inherit;
    border-color: currentColor;
  }
  .hint {
    font-size: calc(11px + var(--uiStep, 0px));
    color: var(--text3);
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
  }
  .actions button {
    height: 26px;
    padding: 0 14px;
    border: 0;
    border-radius: var(--r);
    background: var(--hover2);
    color: var(--text);
    font-size: calc(12px + var(--uiStep, 0px));
    cursor: default;
  }
  .actions .primary {
    background: var(--accent);
    color: #fff;
  }
  .actions .ghost {
    background: transparent;
    color: var(--text2);
  }
  .notice {
    position: fixed;
    left: 50%;
    bottom: 56px;
    transform: translateX(-50%);
    z-index: 82;
    max-width: min(560px, 92vw);
    padding: 8px 14px;
    border-radius: var(--r);
    background: var(--elev);
    box-shadow: var(--shadow);
    font-size: calc(12px + var(--uiStep, 0px));
    color: var(--text);
  }
  @keyframes fmPop {
    from {
      opacity: 0;
      transform: translateY(-4px) scale(0.985);
    }
    to {
      opacity: 1;
      transform: translateY(0) scale(1);
    }
  }
</style>
