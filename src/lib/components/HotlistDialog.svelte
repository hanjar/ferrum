<script lang="ts">
  // "Configure…" of the Directory Hotlist (FER-458), modelled on Total Commander's "Change
  // directory menu": tree on the left, Add item / Add submenu / Add separator / Delete / Up / Down
  // on the right, Title / Path / Target path / Command fields below. Edits go into a draft that
  // is only written (atomically, by the backend) on OK. Also hosts the wincmd.ini import preview.
  import { tick, untrack } from 'svelte';
  import { hotlist } from '../stores/hotlistStore.svelte';
  import { activePane } from '../stores/panesRegistry.svelte';
  import {
    appendItems,
    commandProblem,
    commandRisk,
    basename,
    countItems,
    getAt,
    insertAt,
    isDevicePath,
    moveTo,
    moveWithin,
    parseDoc,
    removeAt,
    replaceAt,
    titleParts,
    type HotItem,
    type TreePath,
  } from '../hotlist/model';
  import { HotlistError, type ImportResult } from '../ipc/hotlist';

  interface FlatRow {
    path: TreePath;
    item: HotItem;
    depth: number;
  }

  let draft = $state<HotItem[]>([]);
  let dirty = $state(false);
  let inited = false;
  let sel = $state<TreePath | null>(null);
  let treeEl = $state<HTMLElement>();
  let dragFrom = $state<TreePath | null>(null);
  let dropOn = $state<string | null>(null);
  let saving = $state(false);

  // Import panel
  let importOpen = $state(false);
  let importPath = $state('');
  let importBusy = $state(false);
  let importError = $state('');
  let preview = $state<ImportResult | null>(null);
  let importMode = $state<'append' | 'replace'>('append');

  const key = (p: TreePath) => p.join('.');

  const flat = $derived.by<FlatRow[]>(() => {
    const out: FlatRow[] = [];
    const walk = (items: HotItem[], prefix: TreePath, depth: number) => {
      items.forEach((item, i) => {
        const path = [...prefix, i];
        out.push({ path, item, depth });
        if (item.type === 'submenu') walk(item.items, path, depth + 1);
      });
    };
    walk(draft, [], 0);
    return out;
  });

  const selected = $derived(sel ? getAt(draft, sel) : null);
  const PROBLEMS = {
    program:
      'This is not a usable program path (no quotes or < > | * ?, and a colon only after the drive letter).',
    uri: 'Web addresses and protocol links (https:, ms-msdt:, …) cannot be used as a command.',
    network: 'Network paths (UNC, WebDAV) cannot be used as a command; use a local program.',
    args_unsafe:
      'Batch files (.bat/.cmd) cannot take arguments containing " % & | < > ^ because cmd.exe would interpret them.',
  } as const;
  const cmdProblem = $derived.by(() => {
    if (selected?.type !== 'command' || selected.command.trim() === '') return '';
    const problem = commandProblem(selected.command, selected.args);
    return problem ? PROBLEMS[problem] : '';
  });
  const cmdRisk = $derived.by(() => {
    if (selected?.type !== 'command') return '';
    const risk = commandRisk(selected.command);
    return risk === 'script'
      ? 'This runs a script (cmd.exe or an interpreter). It will ask before the first run.'
      : risk === 'shortcut'
        ? 'This is a shortcut; its real target is hidden inside the file. It will ask before the first run.'
        : '';
  });
  const valid = $derived(parseDoc({ version: 2, items: draft }) !== null);
  const total = $derived(countItems(draft));

  $effect(() => {
    const open = hotlist.configureOpen;
    const items = hotlist.doc.items;
    if (!open) {
      inited = false;
      dirty = false;
      importOpen = false;
      preview = null;
      return;
    }
    untrack(() => {
      if (!inited || !dirty) {
        draft = structuredClone($state.snapshot(items)) as HotItem[];
        if (!inited) sel = draft.length > 0 ? [0] : null;
        inited = true;
      }
    });
  });

  $effect(() => {
    if (hotlist.configureOpen) void tick().then(() => treeEl?.focus());
  });

  function edit(next: HotItem[] | null, nextSel?: TreePath | null) {
    if (!next) return;
    draft = next;
    dirty = true;
    if (nextSel !== undefined) sel = nextSel;
  }

  /** Insert after the selection (same list), or at the end when nothing is selected. */
  function add(item: HotItem) {
    const parent = sel ? sel.slice(0, -1) : [];
    const index = sel ? sel[sel.length - 1] + 1 : draft.length;
    const next = insertAt(draft, parent, index, item);
    if (!next) {
      hotlist.say('The hotlist is full or nested too deeply.');
      return;
    }
    edit(next, [...parent, index]);
  }

  function addDir() {
    const here = activePane()?.path ?? '';
    add({ type: 'dir', title: here ? basename(here) : 'New folder', path: here });
  }
  const addSubmenu = () => add({ type: 'submenu', title: 'New submenu', items: [] });
  const addSeparator = () => add({ type: 'separator' });
  const addCommand = () => add({ type: 'command', title: 'New command', command: '', args: [] });

  function remove() {
    if (!sel) return;
    const next = removeAt(draft, sel);
    if (!next) return;
    const parent = sel.slice(0, -1);
    const idx = sel[sel.length - 1];
    const siblings = parent.length
      ? (getAt(next, parent) as { items: HotItem[] } | null)?.items
      : next;
    const keep = Math.min(idx, (siblings?.length ?? 0) - 1);
    edit(next, keep >= 0 ? [...parent, keep] : parent.length ? parent : null);
  }

  function move(delta: -1 | 1) {
    if (!sel) return;
    const r = moveWithin(draft, sel, delta);
    if (r) edit(r.items, r.path);
  }

  function setField(patch: Record<string, unknown>) {
    if (!sel || !selected || selected.type === 'separator') return;
    edit(replaceAt(draft, sel, { ...selected, ...patch } as HotItem));
  }

  function onTreeKey(e: KeyboardEvent) {
    const i = sel ? flat.findIndex((r) => key(r.path) === key(sel!)) : -1;
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      if (i < flat.length - 1) sel = flat[i + 1].path;
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      if (i > 0) sel = flat[i - 1].path;
    } else if (e.key === 'Delete') {
      e.preventDefault();
      remove();
    } else if ((e.altKey || e.ctrlKey) && e.key === 'PageUp') {
      e.preventDefault();
      move(-1);
    } else if ((e.altKey || e.ctrlKey) && e.key === 'PageDown') {
      e.preventDefault();
      move(1);
    }
  }

  function onDialogKey(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      if (importOpen) importOpen = false;
      else hotlist.closeConfigure();
    } else if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) {
      e.preventDefault();
      void ok();
    }
  }

  async function ok() {
    if (!valid || hotlist.readOnly || saving) return;
    saving = true;
    const done = await hotlist.replace($state.snapshot(draft) as HotItem[]);
    saving = false;
    if (done) hotlist.closeConfigure();
  }

  // ── drag & drop (drop on a submenu = move into it; elsewhere = place before the row) ──
  function onDrop(target: FlatRow, e: DragEvent) {
    e.preventDefault();
    const from = dragFrom;
    dragFrom = null;
    dropOn = null;
    if (!from) return;
    const intoSub = target.item.type === 'submenu';
    const parent = intoSub ? target.path : target.path.slice(0, -1);
    const index = intoSub
      ? (getAt(draft, target.path) as { items: HotItem[] }).items.length
      : target.path[target.path.length - 1];
    const r = moveTo(draft, from, parent, index);
    if (r) edit(r.items, r.path);
    else hotlist.say("That item can't be moved there.");
  }

  // ── Total Commander import ──
  const WARNINGS: Record<string, string> = {
    no_action: 'has no command and was skipped',
    internal_command: 'is a Total Commander internal command and was skipped',
    unbalanced_end: 'has a stray end-of-submenu marker',
    unclosed_submenu: 'a submenu was not closed; it was closed at the end',
    ignored_after_gap: 'numbering has a gap; the rest was ignored',
    too_deep: 'submenus nest too deeply; the rest was ignored',
    truncated: 'too many entries; the rest was ignored',
    invalid_entry: 'is not valid and was skipped',
    device_path:
      'points at a device path (for example GLOBALROOT or a dot-device), not a folder; skipped',
  };
  const IMPORT_ERRORS: Record<string, string> = {
    not_found: 'File not found.',
    no_dirmenu: 'That file has no [DirMenu] section.',
    unreadable: 'The file could not be read.',
    invalid_path: 'That is not a usable path.',
    unsupported: 'Import needs the desktop app.',
  };

  async function runPreview() {
    importBusy = true;
    importError = '';
    preview = null;
    try {
      preview = await hotlist.api.importPreview(importPath.trim() || null);
    } catch (e) {
      importError = IMPORT_ERRORS[e instanceof HotlistError ? e.kind : ''] ?? 'Import failed.';
    } finally {
      importBusy = false;
    }
  }

  const previewCommands = $derived.by(() => {
    let n = 0;
    const walk = (items: HotItem[]) =>
      items.forEach((i) => {
        if (i.type === 'command') n++;
        else if (i.type === 'submenu') walk(i.items);
      });
    if (preview) walk(preview.doc.items);
    return n;
  });

  function applyImport() {
    if (!preview) return;
    const base = importMode === 'replace' ? [] : draft;
    const { items, dropped } = appendItems(base, preview.doc.items);
    edit(items, items.length > 0 ? [0] : null);
    importOpen = false;
    preview = null;
    if (dropped > 0) hotlist.say(`${dropped} imported item(s) did not fit and were left out.`);
  }

  function label(item: HotItem): string {
    return item.type === 'separator' ? '' : titleParts(item.title).label;
  }
</script>

{#if hotlist.configureOpen}
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="scrim" onmousedown={() => hotlist.closeConfigure()}></div>
  <div
    class="modal"
    role="dialog"
    aria-modal="true"
    aria-label="Configure Directory Hotlist"
    tabindex="-1"
    onkeydown={onDialogKey}
  >
    <h2>Change directory menu</h2>
    {#if hotlist.readOnly}
      <p class="warn" role="status">
        The hotlist file is unavailable, so it is shown read-only. Fix the location or reload in
        Settings › Hotlist.
      </p>
    {/if}

    {#if importOpen}
      <section class="import" aria-label="Import from Total Commander">
        <h3>Import from Total Commander</h3>
        <p class="dim">
          Reads the [DirMenu] of a wincmd.ini (the file is only read, never changed). Leave the path
          empty to use <span class="mono">%APPDATA%\GHISLER\wincmd.ini</span>.
        </p>
        <div class="row">
          <input
            aria-label="wincmd.ini path"
            bind:value={importPath}
            spellcheck="false"
            placeholder="%APPDATA%\GHISLER\wincmd.ini"
            onkeydown={(e) => {
              if (e.key === 'Enter') {
                e.preventDefault();
                void runPreview();
              }
            }}
          />
          <button onclick={() => void runPreview()} disabled={importBusy}>Preview</button>
        </div>
        {#if importError}<p class="err" role="alert">{importError}</p>{/if}
        {#if preview}
          <ul class="prev" aria-label="Import preview">
            {#snippet node(items: HotItem[], depth: number)}
              {#each items as it, i (i)}
                <li style:padding-left="{depth * 14 + 6}px">
                  {#if it.type === 'separator'}
                    <span class="dim">────────</span>
                  {:else if it.type === 'submenu'}
                    <span>▸ {label(it)}</span>
                  {:else if it.type === 'command'}
                    <span>▶︎ {label(it)}</span> <span class="dim mono">{it.command}</span>
                  {:else}
                    <span>{label(it)}</span> <span class="dim mono">{it.path}</span>
                  {/if}
                </li>
                {#if it.type === 'submenu'}{@render node(it.items, depth + 1)}{/if}
              {/each}
            {/snippet}
            {@render node(preview.doc.items, 0)}
          </ul>
          <p class="dim">
            {countItems(preview.doc.items)} items.
            {#if previewCommands > 0}
              {previewCommands} program item(s) are imported but never run automatically; each asks for
              confirmation the first time.
            {/if}
          </p>
          {#if preview.warnings.length > 0}
            <ul class="warns" aria-label="Import warnings">
              {#each preview.warnings as w, i (i)}
                <li>
                  {w.index > 0 ? `Entry ${w.index} ` : ''}{WARNINGS[w.kind] ?? w.kind}
                </li>
              {/each}
            </ul>
          {/if}
          <div class="row">
            <label
              ><input type="radio" name="imode" value="append" bind:group={importMode} /> Add to my hotlist</label
            >
            <label
              ><input type="radio" name="imode" value="replace" bind:group={importMode} /> Replace my
              hotlist</label
            >
          </div>
        {/if}
        <div class="actions">
          <button class="ghost" onclick={() => (importOpen = false)}>Back</button>
          <button class="primary" onclick={applyImport} disabled={!preview}>Import</button>
        </div>
      </section>
    {:else}
      <div class="main">
        <ul
          class="tree"
          role="tree"
          aria-label="Hotlist items"
          tabindex="0"
          bind:this={treeEl}
          onkeydown={onTreeKey}
        >
          {#each flat as row (key(row.path))}
            <!-- svelte-ignore a11y_click_events_have_key_events -->
            <li
              role="treeitem"
              aria-level={row.depth + 1}
              aria-selected={sel !== null && key(sel) === key(row.path)}
              class:sel={sel !== null && key(sel) === key(row.path)}
              class:drop={dropOn === key(row.path)}
              style:padding-left="{row.depth * 16 + 8}px"
              draggable="true"
              ondragstart={() => (dragFrom = row.path)}
              ondragover={(e) => {
                e.preventDefault();
                dropOn = key(row.path);
              }}
              ondragleave={() => (dropOn = null)}
              ondrop={(e) => onDrop(row, e)}
              ondragend={() => {
                dragFrom = null;
                dropOn = null;
              }}
              onclick={() => (sel = row.path)}
            >
              {#if row.item.type === 'separator'}
                <span class="dim">— separator —</span>
              {:else}
                <span class="glyph" aria-hidden="true"
                  >{row.item.type === 'submenu'
                    ? '▸'
                    : row.item.type === 'command'
                      ? '▶︎'
                      : '·'}</span
                >
                <span class="name">{row.item.title}</span>
                {#if row.item.type === 'dir'}
                  <span class="path mono">{row.item.path}</span>
                {/if}
              {/if}
            </li>
          {/each}
          {#if flat.length === 0}
            <li class="dim empty">Empty — add an item.</li>
          {/if}
        </ul>
        <div class="side">
          <button onclick={addDir}>Add item</button>
          <button onclick={addSubmenu}>Add submenu</button>
          <button onclick={addSeparator}>Add separator</button>
          <button onclick={addCommand}>Add command</button>
          <button onclick={remove} disabled={!sel}>Delete item</button>
          <button onclick={() => move(-1)} disabled={!sel}>Up</button>
          <button onclick={() => move(1)} disabled={!sel}>Down</button>
          <button onclick={() => (importOpen = true)}>Import from Total Commander…</button>
        </div>
      </div>

      <div class="fields">
        {#if selected && selected.type !== 'separator'}
          <label>
            <span>Title</span>
            <input
              aria-label="Title"
              value={selected.title}
              spellcheck="false"
              oninput={(e) => setField({ title: e.currentTarget.value })}
            />
          </label>
          {#if selected.type === 'dir'}
            <label>
              <span>Path</span>
              <input
                aria-label="Path"
                value={selected.path}
                spellcheck="false"
                oninput={(e) => setField({ path: e.currentTarget.value })}
              />
            </label>
            <label>
              <span>Target path</span>
              <input
                aria-label="Target path"
                value={selected.targetPath ?? ''}
                spellcheck="false"
                placeholder="optional: folder for the other pane"
                oninput={(e) => setField({ targetPath: e.currentTarget.value || undefined })}
              />
            </label>
            {#if isDevicePath(selected.path) || (selected.targetPath !== undefined && isDevicePath(selected.targetPath))}
              <p class="err hint" role="alert" data-testid="path-problem">
                Device paths (\\.\…, \\?\GLOBALROOT, \??\, \Device\…) cannot be used. Use a drive
                path or \\server\share.
              </p>
            {/if}
          {:else if selected.type === 'command'}
            <label>
              <span>Command</span>
              <input
                aria-label="Command"
                value={selected.command}
                spellcheck="false"
                oninput={(e) => setField({ command: e.currentTarget.value })}
              />
            </label>
            <label>
              <span>Arguments</span>
              <input
                aria-label="Arguments"
                value={selected.args.join(' ')}
                spellcheck="false"
                placeholder="separated by spaces; use quotes for spaces"
                oninput={(e) =>
                  setField({
                    args: (e.currentTarget.value.match(/"[^"]*"|\S+/g) ?? []).map((a) =>
                      a.replace(/^"(.*)"$/, '$1'),
                    ),
                  })}
              />
            </label>
            {#if cmdProblem}
              <p class="err hint" role="alert" data-testid="command-problem">{cmdProblem}</p>
            {/if}
            {#if cmdRisk}
              <p class="warn hint" role="status" data-testid="command-risk">{cmdRisk}</p>
            {/if}
          {/if}
          <p class="dim hint">
            Put <span class="mono">&amp;</span> before a letter to make it the accelerator (<span
              class="mono">&amp;Downloads</span
            >); a leading number works too.
          </p>
        {:else}
          <p class="dim hint">Select an item to edit it.</p>
        {/if}
      </div>
    {/if}

    {#if !valid}
      <p class="err" role="alert">
        Every folder needs a title and a path; every command needs a local program. Invisible or
        direction-changing characters are not allowed.
      </p>
    {/if}
    <div class="actions">
      <span class="dim count">{total} items</span>
      <button class="ghost" onclick={() => hotlist.closeConfigure()}>Cancel</button>
      <button
        class="primary"
        onclick={() => void ok()}
        disabled={!valid || hotlist.readOnly || saving || importOpen}
      >
        OK
      </button>
    </div>
  </div>
{/if}

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
    top: 12%;
    transform: translateX(-50%);
    width: min(680px, 94vw);
    max-height: 80vh;
    overflow: auto;
    padding: 18px;
    border-radius: calc(var(--rl) + 2px);
    background: var(--elev);
    box-shadow: var(--shadow);
    z-index: 61;
    display: flex;
    flex-direction: column;
    gap: 10px;
    animation: fmUp 160ms cubic-bezier(0.2, 0.7, 0.2, 1);
  }
  h2 {
    margin: 0;
    font-size: calc(14px + var(--uiStep, 0px));
    font-weight: 600;
  }
  h3 {
    margin: 0;
    font-size: calc(13px + var(--uiStep, 0px));
    font-weight: 600;
  }
  p {
    margin: 0;
    font-size: calc(12px + var(--uiStep, 0px));
  }
  .dim {
    color: var(--text3);
  }
  .err {
    color: var(--red);
  }
  .warn {
    padding: 6px 10px;
    border-radius: var(--r);
    background: var(--hover2);
    color: var(--text2);
  }
  .mono {
    font-family: 'Geist Mono', monospace;
  }
  .main {
    display: flex;
    gap: 10px;
    min-height: 0;
  }
  .tree {
    flex: 1;
    margin: 0;
    padding: 2px;
    list-style: none;
    height: 260px;
    overflow: auto;
    border-radius: var(--r);
    background: var(--bg);
    outline: none;
  }
  .tree:focus-visible {
    box-shadow: 0 0 0 1.5px var(--accentLine);
  }
  .tree li {
    display: flex;
    align-items: baseline;
    gap: 6px;
    padding: 3px 8px;
    border-radius: var(--r);
    cursor: default;
    font-size: calc(12.5px + var(--uiStep, 0px));
  }
  .tree li:hover {
    background: var(--hover2);
  }
  .tree li.sel {
    background: var(--accentSoft);
    outline: 1px solid var(--accentLine);
  }
  .tree li.drop {
    box-shadow: inset 0 2px 0 var(--accent);
  }
  .glyph {
    width: 10px;
    flex: none;
    color: var(--text3);
  }
  .name {
    color: var(--text);
    white-space: nowrap;
  }
  .path {
    color: var(--text3);
    font-size: calc(10.5px + var(--uiStep, 0px));
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .side {
    width: 190px;
    flex: none;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .fields {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .fields label {
    display: grid;
    grid-template-columns: 90px 1fr;
    align-items: center;
    gap: 8px;
    font-size: calc(12px + var(--uiStep, 0px));
    color: var(--text2);
  }
  .fields input,
  .import .row input:not([type]) {
    height: 26px;
    padding: 0 8px;
    border: 0;
    outline: 0;
    border-radius: var(--r);
    background: var(--bg);
    box-shadow: 0 0 0 1px var(--line);
    color: var(--text);
    font-size: calc(12.5px + var(--uiStep, 0px));
    min-width: 0;
  }
  .fields input:focus,
  .import .row input:focus {
    box-shadow: 0 0 0 1.5px var(--accentLine);
  }
  .hint {
    font-size: calc(11px + var(--uiStep, 0px));
  }
  .import {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .import .row {
    display: flex;
    gap: 8px;
    align-items: center;
  }
  .import .row input:not([type]) {
    flex: 1;
  }
  .prev {
    margin: 0;
    padding: 4px;
    list-style: none;
    max-height: 200px;
    overflow: auto;
    border-radius: var(--r);
    background: var(--bg);
    font-size: calc(12px + var(--uiStep, 0px));
  }
  .prev li {
    padding: 1px 0;
  }
  .warns {
    margin: 0;
    padding-left: 18px;
    font-size: calc(11.5px + var(--uiStep, 0px));
    color: var(--text2);
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    align-items: center;
    gap: 8px;
  }
  .count {
    margin-right: auto;
    font-size: calc(11px + var(--uiStep, 0px));
  }
  button {
    height: 26px;
    padding: 0 12px;
    border: 0;
    border-radius: var(--r);
    background: var(--hover2);
    color: var(--text);
    font-size: calc(12px + var(--uiStep, 0px));
    cursor: default;
  }
  button:hover:not(:disabled) {
    filter: brightness(1.06);
  }
  button:disabled {
    opacity: 0.5;
  }
  .primary {
    background: var(--accent);
    color: #fff;
  }
  .ghost {
    background: transparent;
    color: var(--text2);
  }
</style>
