<script lang="ts">
  import { tick, untrack } from 'svelte';
  import Icon from './Icon.svelte';
  import { activity } from '../stores/activityStore.svelte';
  import { hotlist } from '../stores/hotlistStore.svelte';
  import { activePane, inactivePane } from '../stores/panesRegistry.svelte';
  import { pathResolve, pathSuggest, type PathSuggestion } from '../ipc/pathSuggest';
  import {
    completeText,
    highlightPrefix,
    refineFromCache,
    splitInput,
    stepIndex,
    uncPathsOf,
    type SuggestCache,
  } from '../addressPath';
  import { FsError } from '../ipc/types';
  import { layoutCrumbs, type Crumb, type CrumbLayout } from '../crumbs';

  let {
    crumbs,
    path,
    editing = $bindable(false),
    oncrumb,
    onnavigate,
    onhotlist,
  }: {
    crumbs: Crumb[];
    /** The pane's current folder: initial text and base for relative input. */
    path: string;
    editing?: boolean;
    oncrumb?: (path: string) => void;
    onnavigate?: (path: string) => void;
    /** The ☆▾ button: opens the hotlist popup (FER-458). */
    onhotlist?: () => void;
  } = $props();

  const DEBOUNCE_MS = 120;
  const RECENT_MAX = 5;
  const uid = Math.random().toString(36).slice(2, 8);

  interface Row {
    id: string;
    name: string;
    path: string;
    kind: 'drive' | 'dir' | 'archive' | 'recent';
    link: boolean;
  }

  let text = $state('');
  let items = $state<PathSuggestion[]>([]);
  let active = $state(-1);
  let message = $state('');
  let busy = $state(false);
  /** Unknown UNC host the user typed: no I/O until Enter. */
  let confirmHost = $state('');
  let cache: SuggestCache<PathSuggestion> | undefined;
  let inputEl = $state<HTMLInputElement | undefined>();
  let rootEl = $state<HTMLDivElement | undefined>();
  let measureEl = $state<HTMLSpanElement | undefined>();
  let dropLeft = $state(0);
  let timer: ReturnType<typeof setTimeout> | undefined;
  // Newest request wins (same pattern as previewStore); late replies are dropped.
  let reqId = 0;

  const split = $derived(splitInput(text));

  const recent = $derived.by((): Row[] => {
    const q = text.trim().toLowerCase();
    // Private session and paused recording show no Recent (activity contract).
    if (!q || !activity.recordingActive) return [];
    const have = new Set(items.map((i) => i.path.toLowerCase()));
    const out: Row[] = [];
    for (const it of activity.recent) {
      const loc = it.entity.locator;
      if (it.entity.kind !== 'folder' || have.has(loc.toLowerCase())) continue;
      const hay = `${it.entity.displayName} ${loc}`.toLowerCase();
      if (!hay.includes(q)) continue;
      out.push({
        id: '',
        name: loc,
        path: loc,
        kind: 'recent',
        link: false,
      });
      if (out.length >= RECENT_MAX) break;
    }
    return out;
  });

  const rows = $derived.by((): Row[] => {
    const base: Row[] = items.map((s) => ({ ...s, id: '' }));
    return [...base, ...recent].map((r, i) => ({ ...r, id: `ab-${uid}-${i}` }));
  });
  const firstRecent = $derived(items.length);

  function measureLeft() {
    if (!measureEl || !inputEl) return;
    const w = measureEl.getBoundingClientRect().width;
    const max = Math.max(0, inputEl.clientWidth - 160);
    dropLeft = Math.min(w, max);
  }

  async function open() {
    if (editing) return;
    editing = true;
  }

  function close() {
    editing = false;
  }

  // Reset + focus whenever the bar enters edit mode.
  $effect(() => {
    if (!editing) {
      untrack(() => {
        clearTimeout(timer);
        reqId++;
        items = [];
        active = -1;
        message = '';
        busy = false;
        confirmHost = '';
        cache = undefined;
      });
      return;
    }
    untrack(() => {
      text = path;
      void tick().then(() => {
        inputEl?.focus();
        inputEl?.select();
      });
    });
  });

  function schedule() {
    clearTimeout(timer);
    message = '';
    // Same folder, longer prefix, complete list cached: filter here, no IPC.
    const local = refineFromCache(cache, path, text);
    if (local) {
      reqId++;
      items = local;
      active = -1;
      confirmHost = '';
      void tick().then(measureLeft);
      return;
    }
    timer = setTimeout(() => void fetchSuggestions(), DEBOUNCE_MS);
  }

  /** UNC hosts the backend may already treat as known (it never contacts others). */
  function knownUnc(): string[] {
    const paths = [path, activePane()?.path ?? '', inactivePane()?.path ?? ''];
    for (const h of hotlist.entries()) paths.push(h.path);
    if (activity.recordingActive) for (const r of activity.recent) paths.push(r.entity.locator);
    return uncPathsOf(paths);
  }

  async function fetchSuggestions() {
    const id = ++reqId;
    const typed = text;
    try {
      const res = await pathSuggest(typed, path, undefined, knownUnc());
      if (id !== reqId || !editing) return;
      items = res.items;
      confirmHost = res.needsConfirm ? (res.host ?? '') : '';
      const { folder, prefix } = splitInput(typed);
      cache = {
        base: path,
        folder,
        prefix,
        items: res.items,
        complete: !res.truncated && !res.timedOut && !res.needsConfirm,
      };
      active = -1;
      await tick();
      measureLeft();
    } catch (e) {
      if (id !== reqId) return;
      items = [];
      message = e instanceof FsError ? e.message : 'Could not read suggestions';
    }
  }

  function onInput() {
    schedule();
  }

  function complete(row: Row | undefined) {
    if (!row) return;
    if (row.kind === 'recent') {
      text = row.path.endsWith('\\') ? row.path : `${row.path}\\`;
    } else {
      text = completeText(text, row.name, row.kind);
    }
    void tick().then(() => {
      inputEl?.setSelectionRange(text.length, text.length);
    });
    schedule();
  }

  async function submit(target?: string) {
    const typed = (target ?? text).trim();
    if (!typed || busy) return;
    busy = true;
    const id = ++reqId;
    try {
      const res = await pathResolve(typed, path);
      if (id !== reqId || !editing) return;
      if (res.kind === 'dir' || res.kind === 'archive') {
        close();
        onnavigate?.(res.path);
      } else if (res.kind === 'file') {
        message = `Not a folder: ${typed}`;
      } else {
        message = `Path not found: ${typed}`;
      }
    } catch (e) {
      if (id !== reqId) return;
      message = e instanceof FsError ? e.message : 'Cannot open that location';
    } finally {
      busy = false;
    }
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      close();
    } else if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      active = stepIndex(active, e.key === 'ArrowDown' ? 1 : -1, rows.length);
    } else if (e.key === 'Tab') {
      if (rows.length === 0) return;
      e.preventDefault();
      complete(rows[active >= 0 ? active : 0]);
    } else if (e.key === 'Enter') {
      e.preventDefault();
      const row = active >= 0 ? rows[active] : undefined;
      // A picked suggestion opens directly; plain Enter opens what was typed.
      void submit(row ? row.path : undefined);
    }
  }

  function onFocusOut(e: FocusEvent) {
    const next = e.relatedTarget as Node | null;
    if (next && rootEl?.contains(next)) return;
    close();
  }

  // --- FER-491 smart breadcrumb: fit once per frame, fold the middle into `…` ---
  let navEl = $state<HTMLElement | undefined>();
  let measEl = $state<HTMLElement | undefined>();
  let moreBtn = $state<HTMLButtonElement | undefined>();
  let menuEl = $state<HTMLDivElement | undefined>();
  let fittedState = $state<CrumbLayout>({ shown: [], hidden: [] });
  /** Path the stored layout was computed for; a different path shows everything until re-fit. */
  let fittedKey = $state('');
  let moreOpen = $state(false);
  let menuLeft = $state(0);
  let raf = 0;

  /** Space kept free at the end of the row so the click-to-edit gutter stays reachable. */
  const EDIT_GUTTER = 16;

  type Seg = { t: 'crumb'; i: number } | { t: 'more' } | { t: 'sep' };
  const crumbKey = $derived(crumbs.map((c) => c.path).join('|'));
  const fitted = $derived.by((): CrumbLayout => {
    if (fittedKey === crumbKey && fittedState.shown.length > 0) return fittedState;
    return { shown: crumbs.map((_, i) => i), hidden: [] };
  });
  const segs = $derived.by((): Seg[] => {
    const layout = fitted;
    const moreAt = layout.hidden.length
      ? layout.shown.filter((i) => i < layout.hidden[0]).length
      : -1;
    const out: Seg[] = [];
    layout.shown.forEach((ci, k) => {
      if (k === moreAt) {
        if (out.length) out.push({ t: 'sep' });
        out.push({ t: 'more' });
      }
      if (out.length) out.push({ t: 'sep' });
      out.push({ t: 'crumb', i: ci });
    });
    return out;
  });

  function measureFit() {
    raf = 0;
    if (!navEl || !measEl) return;
    // Not laid out (hidden pane, jsdom): show everything, fit when it gets a size.
    if (navEl.clientWidth === 0) {
      fittedKey = '';
      fittedState = { shown: [], hidden: [] };
      return;
    }
    // One read phase, then one write: no interleaved layout reads.
    const kids = Array.from(measEl.children) as HTMLElement[];
    const n = crumbs.length;
    const widths = kids.slice(0, n).map((k) => k.getBoundingClientRect().width);
    const sep = kids[n]?.getBoundingClientRect().width ?? 14;
    const more = kids[n + 1]?.getBoundingClientRect().width ?? 24;
    const available = navEl.clientWidth - EDIT_GUTTER;
    const next = layoutCrumbs({ widths, sep: sep + 2, more, available });
    if (crumbKey !== fittedKey || JSON.stringify(next) !== JSON.stringify(fittedState)) {
      fittedKey = crumbKey;
      fittedState = next;
    }
  }

  function scheduleFit() {
    if (raf || typeof requestAnimationFrame === 'undefined') return;
    raf = requestAnimationFrame(measureFit);
  }

  $effect(() => {
    if (!navEl || !measEl || typeof ResizeObserver === 'undefined') return;
    const ro = new ResizeObserver(scheduleFit);
    ro.observe(navEl);
    ro.observe(measEl);
    scheduleFit();
    return () => {
      ro.disconnect();
      if (raf && typeof cancelAnimationFrame !== 'undefined') cancelAnimationFrame(raf);
      raf = 0;
    };
  });

  // A new path (or relabel) re-measures; the menu never outlives its segments.
  $effect(() => {
    void crumbs;
    untrack(() => {
      moreOpen = false;
      scheduleFit();
    });
  });

  const menuButtons = (): HTMLButtonElement[] =>
    Array.from(menuEl?.querySelectorAll<HTMLButtonElement>('.mi') ?? []);

  async function openMore() {
    if (!moreBtn || !rootEl) return;
    menuLeft = moreBtn.getBoundingClientRect().left - rootEl.getBoundingClientRect().left;
    moreOpen = true;
    await tick();
    menuButtons()[0]?.focus();
  }
  function closeMore(refocus: boolean) {
    moreOpen = false;
    if (refocus) moreBtn?.focus();
  }
  function toggleMore() {
    if (moreOpen) closeMore(false);
    else void openMore();
  }
  function onMoreBtnKey(e: KeyboardEvent) {
    if (e.key === 'ArrowDown' && !moreOpen) {
      e.preventDefault();
      void openMore();
    }
  }
  function pickHidden(i: number) {
    closeMore(false);
    oncrumb?.(crumbs[i].path);
  }
  function onMenuKey(e: KeyboardEvent) {
    // Menu keys must not reach the pane's list keymap (Enter/arrows would act on rows).
    e.stopPropagation();
    const items = menuButtons();
    const at = items.indexOf(document.activeElement as HTMLButtonElement);
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      const d = e.key === 'ArrowDown' ? 1 : -1;
      items[(at + d + items.length) % items.length]?.focus();
    } else if (e.key === 'Home' || e.key === 'End') {
      e.preventDefault();
      items[e.key === 'Home' ? 0 : items.length - 1]?.focus();
    } else if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      closeMore(true);
    } else if (e.key === 'Tab') {
      // FER-489: Tab leaves the menu and returns to this pane's file list.
      e.preventDefault();
      closeMore(false);
      (e.currentTarget as HTMLElement)
        .closest('section.pane')
        ?.querySelector<HTMLElement>('[data-pane-list]')
        ?.focus();
    }
  }
  function onMenuFocusOut(e: FocusEvent) {
    const next = e.relatedTarget as Node | null;
    if (next && (menuEl?.contains(next) || next === moreBtn)) return;
    moreOpen = false;
  }

  const showList = $derived(editing && rows.length > 0);
  const hintIndex = $derived(active >= 0 ? active : 0);
</script>

<div class="address" class:editing bind:this={rootEl}>
  {#if editing}
    <div class="field">
      <input
        bind:this={inputEl}
        bind:value={text}
        class="addrInput"
        role="combobox"
        aria-label="Address"
        aria-expanded={showList}
        aria-controls={showList ? `ab-list-${uid}` : undefined}
        aria-autocomplete="list"
        aria-activedescendant={active >= 0 && rows[active] ? rows[active].id : undefined}
        aria-invalid={message ? 'true' : undefined}
        aria-describedby={message ? `ab-msg-${uid}` : undefined}
        spellcheck="false"
        autocomplete="off"
        oninput={onInput}
        onkeydown={onKeydown}
        onfocusout={onFocusOut}
      />
      <span class="measure" bind:this={measureEl} aria-hidden="true">{split.folder}</span>
      {#if showList}
        <ul
          class="list"
          id="ab-list-{uid}"
          role="listbox"
          aria-label="Folder suggestions"
          style:left="{dropLeft}px"
        >
          {#each rows as row, i (row.id)}
            {#if i === firstRecent && i > 0}
              <li class="divider" role="presentation"></li>
            {/if}
            <!-- svelte-ignore a11y_click_events_have_key_events -->
            <li
              id={row.id}
              role="option"
              aria-selected={i === active}
              class="opt"
              class:on={i === active}
              class:recent={row.kind === 'recent'}
              onmousedown={(e) => e.preventDefault()}
              onclick={() => complete(row)}
              ondblclick={() => void submit(row.path)}
              onmousemove={() => (active = i)}
            >
              <span class="ico">
                <Icon
                  name={row.kind === 'recent'
                    ? 'clock'
                    : row.kind === 'drive'
                      ? 'drive'
                      : 'folderOutline'}
                  size={13}
                />
              </span>
              <span class="nm">
                {#if row.kind === 'recent'}
                  {row.name}
                {:else}
                  {#each highlightPrefix(row.name, split.prefix) as p, k (k)}
                    {#if p.bold}<b>{p.text}</b>{:else}{p.text}{/if}
                  {/each}
                {/if}
              </span>
              {#if row.link}<span class="tag" aria-hidden="true">link</span>{/if}
              {#if row.kind === 'archive'}<span class="tag" aria-hidden="true">arsip</span>{/if}
              {#if row.kind === 'recent'}<span class="tag" aria-hidden="true">recent</span>{/if}
              {#if i === hintIndex}<span class="hint" aria-hidden="true">Tab lengkapi</span>{/if}
            </li>
          {/each}
        </ul>
      {/if}
      {#if confirmHost && !message}
        <div class="note" role="status">Tekan Enter untuk menghubungi \\{confirmHost}</div>
      {/if}
      <div class="sr" role="status">{showList ? `${rows.length} saran` : ''}</div>
      {#if message}
        <div class="msg" id="ab-msg-{uid}" role="alert">{message}</div>
      {/if}
    </div>
  {:else}
    <nav class="crumbs" aria-label="Path" title={path} bind:this={navEl}>
      {#each segs as s, k (k)}
        {#if s.t === 'sep'}
          <Icon name="chevRight" size={12} class="sep" />
        {:else if s.t === 'more'}
          <button
            class="more cb"
            type="button"
            title="Hidden folders"
            aria-label="Hidden folders"
            aria-haspopup="menu"
            aria-expanded={moreOpen}
            bind:this={moreBtn}
            onclick={toggleMore}
            onkeydown={onMoreBtnKey}>…</button
          >
        {:else}
          <button
            class="crumb cb"
            class:last={s.i === crumbs.length - 1}
            class:archive={Boolean(crumbs[s.i].isArchive)}
            class:in-archive={Boolean(crumbs[s.i].inArchive)}
            data-archive={crumbs[s.i].isArchive ? 'true' : undefined}
            data-in-archive={crumbs[s.i].inArchive ? 'true' : undefined}
            style:max-width={s.i === crumbs.length - 1 && fitted.lastMax
              ? `${fitted.lastMax}px`
              : null}
            onclick={() => oncrumb?.(crumbs[s.i].path)}
          >
            {crumbs[s.i].label}
          </button>
        {/if}
      {/each}
      <!-- Hidden measuring row: natural widths, read once per frame (no layout thrash). -->
      <span class="meas" aria-hidden="true" bind:this={measEl}>
        {#each crumbs as c, i (i)}
          <span class="m cb" class:last={i === crumbs.length - 1}>{c.label}</span>
        {/each}
        <span class="mSep"><Icon name="chevRight" size={12} /></span>
        <span class="mMore more cb">…</span>
      </span>
      <button
        class="empty"
        type="button"
        title="Edit path (Alt+D)"
        aria-label="Edit path"
        onclick={open}
      ></button>
    </nav>
  {/if}
  {#if moreOpen && !editing}
    <div
      class="menu"
      role="menu"
      aria-label="Hidden folders"
      tabindex="-1"
      style:left="{menuLeft}px"
      bind:this={menuEl}
      onkeydown={onMenuKey}
      onfocusout={onMenuFocusOut}
    >
      {#each fitted.hidden as hi (hi)}
        <button
          class="mi"
          type="button"
          role="menuitem"
          title={crumbs[hi].path}
          onclick={() => pickHidden(hi)}
        >
          <Icon name={hi === 0 ? 'drive' : 'folderOutline'} size={13} />
          <span class="mlabel">{crumbs[hi].label}</span>
        </button>
      {/each}
    </div>
  {/if}
  {#if onhotlist}
    <button
      class="hot"
      type="button"
      title="Directory Hotlist (Ctrl+D)"
      aria-haspopup="menu"
      onclick={onhotlist}
    >
      <Icon name="star" size={13} />
      <svg class="chev" viewBox="0 0 10 10" width="8" height="8" aria-hidden="true">
        <path
          d="M2 3.5 5 6.5 8 3.5"
          fill="none"
          stroke="currentColor"
          stroke-width="1.4"
          stroke-linecap="round"
          stroke-linejoin="round"
        />
      </svg>
    </button>
  {/if}
</div>

<style>
  .address {
    flex: 1 1 auto;
    min-width: 72px;
    display: flex;
    align-items: center;
    gap: 4px;
    position: relative;
  }
  .address.editing {
    /* Claim room from the filter box while typing a path. */
    flex: 1 1 520px;
  }
  .crumbs {
    flex: 1 1 auto;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: 1px;
    overflow: hidden;
    white-space: nowrap;
    align-self: stretch;
    position: relative;
  }
  .cb {
    padding: 3px 5px;
    border: 0;
    border-radius: 4px;
    font-size: calc(12.5px + var(--uiStep, 0px));
    background: transparent;
    color: var(--text2);
    white-space: nowrap;
    cursor: default;
  }
  .crumbs .crumb {
    flex: 0 0 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  /* The active folder may shrink (ellipsis at its end) when it alone overflows. */
  .crumbs .crumb.last {
    flex: 0 1 auto;
  }
  .more {
    flex: none;
    font-weight: 600;
    letter-spacing: 1px;
    color: var(--text2);
  }
  .more:hover,
  .more[aria-expanded='true'] {
    background: var(--hover2);
    color: var(--text);
  }
  .more:focus-visible,
  .crumb:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: -2px;
  }
  .meas {
    position: absolute;
    left: 0;
    top: 0;
    visibility: hidden;
    pointer-events: none;
    display: flex;
    align-items: center;
    width: max-content;
  }
  .meas .m,
  .meas .mMore,
  .meas .mSep {
    flex: none;
  }
  .meas .m.last {
    font-weight: 500;
  }
  .menu {
    position: absolute;
    top: 100%;
    margin-top: 2px;
    padding: 3px;
    min-width: 160px;
    max-width: 360px;
    max-height: 280px;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    background: var(--elev);
    border: 1px solid var(--line);
    border-radius: var(--r);
    box-shadow: var(--tabShadow, 0 6px 20px rgba(0, 0, 0, 0.25));
    z-index: 40;
  }
  .mi {
    display: flex;
    align-items: center;
    gap: 7px;
    padding: 4px 7px;
    border: 0;
    border-radius: 4px;
    background: transparent;
    color: var(--text);
    font-size: calc(12px + var(--uiStep, 0px));
    text-align: left;
    cursor: default;
  }
  .mi:hover,
  .mi:focus-visible {
    background: var(--hover2);
    outline: 0;
  }
  .mi :global(svg) {
    flex: none;
    color: var(--text3);
  }
  .mlabel {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .crumbs :global(.sep path) {
    stroke: var(--text3);
  }
  .crumb.last {
    color: var(--text);
    font-weight: 500;
  }
  .crumb:hover {
    background: var(--hover2);
    color: var(--text);
  }
  .empty {
    flex: 1 1 auto;
    min-width: 20px;
    align-self: stretch;
    margin: 4px 0 4px 2px;
    border: 1px dashed transparent;
    border-radius: 4px;
    background: transparent;
    cursor: text;
  }
  .empty:hover,
  .empty:focus-visible {
    border-color: var(--line);
  }
  .field {
    flex: 1 1 auto;
    min-width: 0;
    position: relative;
  }
  .addrInput {
    width: 100%;
    height: 26px;
    padding: 0 8px;
    font-family: 'Geist Mono', ui-monospace, monospace;
    font-size: calc(12px + var(--uiStep, 0px));
    color: var(--text);
    background: var(--elev);
    border: 1px solid var(--accent);
    border-radius: var(--r);
    outline: 0;
    box-sizing: border-box;
  }
  .measure {
    position: absolute;
    visibility: hidden;
    white-space: pre;
    left: 8px;
    top: 0;
    pointer-events: none;
    font-family: 'Geist Mono', ui-monospace, monospace;
    font-size: calc(12px + var(--uiStep, 0px));
  }
  .list {
    position: absolute;
    top: 100%;
    margin: 2px 0 0 8px;
    padding: 3px;
    min-width: 240px;
    max-width: 480px;
    max-height: 280px;
    overflow-y: auto;
    list-style: none;
    background: var(--elev);
    border: 1px solid var(--line);
    border-radius: var(--r);
    box-shadow: var(--tabShadow, 0 6px 20px rgba(0, 0, 0, 0.25));
    z-index: 30;
  }
  .opt {
    display: flex;
    align-items: center;
    gap: 7px;
    padding: 4px 7px;
    border-radius: 4px;
    font-size: calc(12px + var(--uiStep, 0px));
    color: var(--text);
    cursor: default;
  }
  .opt.on {
    background: var(--hover2);
  }
  .opt.recent .nm {
    color: var(--text2);
  }
  .ico {
    display: grid;
    color: var(--text3);
    flex: none;
  }
  .nm {
    flex: 1 1 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: 'Geist Mono', ui-monospace, monospace;
  }
  .nm b {
    font-weight: 700;
  }
  .tag,
  .hint {
    flex: none;
    font-size: 10.5px;
    color: var(--text3);
  }
  .hint {
    opacity: 0.85;
  }
  .divider {
    height: 1px;
    margin: 3px 4px;
    background: var(--line);
  }
  .note {
    position: absolute;
    top: 100%;
    left: 8px;
    margin-top: 2px;
    padding: 3px 8px;
    font-size: 11.5px;
    color: var(--text2);
    background: var(--elev);
    border: 1px solid var(--line);
    border-radius: var(--r);
    z-index: 29;
  }
  .sr {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }
  .msg {
    position: absolute;
    top: 100%;
    left: 8px;
    margin-top: 2px;
    padding: 3px 8px;
    font-size: 11.5px;
    color: var(--danger, #e5484d);
    background: var(--elev);
    border: 1px solid var(--line);
    border-radius: var(--r);
    z-index: 29;
    max-width: 100%;
  }
  .hot {
    flex: none;
    height: 24px;
    display: inline-flex;
    align-items: center;
    gap: 3px;
    padding: 0 6px;
    border: 0;
    border-radius: var(--r);
    background: transparent;
    color: var(--text2);
    cursor: default;
  }
  .hot:hover {
    background: var(--hover2);
    color: var(--text);
  }
</style>
