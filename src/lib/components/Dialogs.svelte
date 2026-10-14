<script lang="ts">
  import {
    createEntry,
    deleteEntries,
    enqueueCopy,
    enqueueMove,
    renameEntry,
  } from '../ipc/mutations';
  import { runPack, type PackFormat } from '../packService';
  import { loadSelectionFile, saveSelectionFile } from '../ipc/selectionFile';
  import { FsError } from '../ipc/types';
  import { isCancelledError } from '../ipc/archiveErrors';
  import { archiveRarToolProbe, type ArchiveCompressionLevel } from '../ipc/archive';
  import {
    dialogs,
    sanitizeMemberPath,
    type ArchiveTestOutcome,
  } from '../stores/dialogStore.svelte';
  import { activePane } from '../stores/panesRegistry.svelte';
  import {
    applySavedSelection,
    matchMasks,
    parseSelectionFile,
    selectMatching,
  } from '../markSelection';
  import { ui } from '../uiState.svelte';
  import { prefs } from '../stores/prefsStore.svelte';
  import { requiresDeleteConfirmation } from '../ipc/prefs';
  import { viewState } from '../stores/viewStateStore.svelte';
  import { comments } from '../stores/commentStore.svelte';
  import { quitApp } from '../quit';
  // DSK-06.3: completed mutations mark a touching Disk usage scan stale —
  // observation only; the flows themselves are unchanged.
  import { noteScopeMutation, recordQueuedMove } from '../diskUsage/invalidation';
  import { activity } from '../stores/activityStore.svelte';

  let name = $state('');
  let permanent = $state(false);
  let password = $state('');
  let policy = $state<'ask' | 'skip' | 'overwrite' | 'overwriteOlder' | 'rename'>('ask');
  // FER-368: whole-batch decision for the extractConflicts dialog. 'skip' is
  // the safe default re-applied on every dialog open (D21 one-ask).
  let extractPolicy = $state<'skip' | 'overwrite' | 'rename'>('skip');
  let packLevel = $state<ArchiveCompressionLevel>('normal');
  let packFormat = $state<PackFormat>('zip');
  let packTarget = $state<string>('');
  let rarAvailable = $state(false);
  // FER-510: true while the WinRAR probe is still running — the RAR option must never show
  // before the probe finished, even when the dialog opens directly with format 'rar'.
  let rarProbing = $state(false);

  /** The target path for format `fmt`: swap (or append) the archive extension. */
  function withArchiveExt(current: string, fmt: PackFormat): string {
    const newExt = fmt === 'sevenz' ? '.7z' : `.${fmt}`;
    for (const ext of ['.zip', '.7z', '.rar']) {
      if (current.toLowerCase().endsWith(ext)) {
        return current.slice(0, -ext.length) + newExt;
      }
    }
    return current + newExt;
  }

  function changePackFormat(fmt: PackFormat) {
    packFormat = fmt;
    if (d && d.kind === 'pack') {
      d.format = fmt;
      packTarget = withArchiveExt(packTarget || d.target, fmt);
      d.target = packTarget;
    }
  }
  /** FER-510: probe failed or threw — RAR stays hidden; a dialog opened as 'rar' falls back
   *  to ZIP (target extension included) with a message. LOCAL state only: mutating the
   *  store's dialog here would re-run the open effect (which reads d.format) and wipe the
   *  message again. */
  function rarUnavailable(reason?: string) {
    rarAvailable = false;
    if (packFormat === 'rar') {
      packFormat = 'zip';
      packTarget = withArchiveExt(packTarget, 'zip');
      error = reason
        ? `WinRAR tidak tersedia (${reason}) — format dikembalikan ke ZIP.`
        : 'WinRAR tidak tersedia — format dikembalikan ke ZIP.';
    }
  }

  /** FER-510: 'rar' was requested but the probe has not OK'd it yet — nothing to submit. */
  const rarProbePending = $derived(rarProbing && packFormat === 'rar');

  /** FER-460 scan offer: "jangan tanya lagi" (offer preference only). */
  let fastDontAsk = $state(false);
  /** FER-460: initial focus of the scan offer = the safe "Scan biasa". */
  let safeScanBtn = $state<HTMLButtonElement | undefined>();
  /** FER-494 T1: initial focus of the agent confirmation = "Batal". */
  let agentCancelBtn = $state<HTMLButtonElement | undefined>();
  let busy = $state(false);
  let error = $state('');
  let inputEl = $state<HTMLInputElement | undefined>();
  let commentDraft = $state('');
  let modalEl = $state<HTMLDivElement | undefined>();
  let deleteConfirmationBypassed = $state(false);

  // Reset per-dialog transient state whenever a dialog opens. FER-364: the
  // delete checkbox initializes FROM the dialog's carried `permanent` flag
  // (beginDelete) instead of a hardcoded false, so Shift+F8 opens with the
  // permanent confirm pre-checked (D05) — it stays user-editable while the
  // dialog is open and re-initializes on every new open.
  $effect(() => {
    const d = dialogs.current;
    if (d) {
      error = '';
      busy = false;
      permanent = d.kind === 'delete' ? d.permanent : false;
      const preferred = prefs.fileOperations.conflictPolicy;
      policy =
        d.kind === 'copy' || d.kind === 'move'
          ? preferred === 'keepBoth'
            ? 'rename'
            : preferred
          : 'ask';
      extractPolicy = 'skip';
      packLevel = 'normal';
      password = '';
      fastDontAsk = false;
      rarProbing = false;
      rarAvailable = false;
      name = d.kind === 'rename' ? d.original : '';
      commentDraft = d.kind === 'comment' ? d.initial : '';
      if (d.kind === 'pack') {
        packFormat = d.format;
        packTarget = d.target;
        // FER-510: the RAR option only ever appears after the probe succeeded. A dialog
        // opened directly with format 'rar' (e.g. reopened after a failed RAR pack) shows a
        // checking note instead, cannot be submitted while checking, and falls back to ZIP
        // with a message when the probe fails.
        rarProbing = true;
        archiveRarToolProbe()
          .then((res) => {
            if (dialogs.current !== d) return;
            rarProbing = false;
            if (res && res.ok) {
              rarAvailable = true;
            } else {
              rarUnavailable(res?.reason);
            }
          })
          .catch((e) => {
            if (dialogs.current !== d) return;
            rarProbing = false;
            rarUnavailable(e instanceof FsError ? e.message : undefined);
          });
        if (d.initialError) {
          error = d.initialError;
        }
      }
      if (d.kind === 'delete') {
        deleteConfirmationBypassed = !requiresDeleteConfirmation(
          d.permanent,
          prefs.fileOperations.confirmBeforeDeleting,
        );
        if (deleteConfirmationBypassed) queueMicrotask(() => void submitDelete(d.paths));
      } else {
        deleteConfirmationBypassed = false;
      }
      queueMicrotask(() => {
        if (inputEl) {
          inputEl.focus();
          inputEl.select();
          return;
        }
        if (dialogs.current?.kind === 'fastScanOffer' && safeScanBtn) {
          safeScanBtn.focus();
          return;
        }
        if (dialogs.current?.kind === 'agentTerminalConfirm' && agentCancelBtn) {
          agentCancelBtn.focus();
          return;
        }
        // Dialogs without an input (pack, test reports) opened by KEYBOARD
        // still need Enter/Escape to work: focus the modal itself. Never
        // steal focus when something inside the modal is already focused —
        // the MenuBar moves focus into menu-opened dialogs itself.
        const modal = modalEl;
        if (modal && !modal.contains(document.activeElement)) modal.focus();
      });
    }
  });

  const d = $derived(dialogs.current);

  // F2-14: live match count for the Select/Unselect Group preview.
  const groupMatchCount = $derived.by(() => {
    if (d?.kind !== 'selectGroup') return 0;
    const pane = activePane();
    if (!pane?.visibleEntries) return 0;
    const ci = !/linux/i.test(globalThis.navigator?.platform ?? '');
    return pane.visibleEntries().filter((e) => matchMasks(e.name, name, ci)).length;
  });

  function testOutcomeLabel(st: ArchiveTestOutcome): string {
    switch (st) {
      case 'passed':
        return 'Lulus';
      case 'failed':
        return 'Gagal (korup)';
      case 'passwordRequired':
        return 'Butuh password';
      case 'unsupported':
        return 'Tidak didukung';
      case 'cancelled':
        return 'Dibatalkan';
    }
  }

  function formatMiB(bytes: number): string {
    return bytes >= 1024 * 1024
      ? `${(bytes / (1024 * 1024)).toFixed(1)} MiB`
      : `${Math.round(bytes / 1024)} KiB`;
  }

  /** 0..100 for the progress bar; null (indeterminate) when no total is known. */
  function progressPercent(p: {
    processedBytes: number;
    totalBytes?: number;
    processedEntries: number;
    totalEntries?: number;
  }): number | null {
    if (p.totalBytes && p.totalBytes > 0) {
      return Math.min(100, Math.round((p.processedBytes / p.totalBytes) * 100));
    }
    if (p.totalEntries && p.totalEntries > 0) {
      return Math.min(100, Math.round((p.processedEntries / p.totalEntries) * 100));
    }
    return null;
  }

  /** Submit the typed password once and clear the field (FER-73). */
  function submitPassword(retry: (password: string) => Promise<void>) {
    const typed = password;
    password = '';
    void retry(typed);
  }

  function fileNameOf(path: string): string {
    const cut = Math.max(path.lastIndexOf(String.fromCharCode(92)), path.lastIndexOf('/'));
    return path.slice(cut + 1);
  }

  function fail(e: unknown) {
    error = e instanceof FsError ? e.message : String(e);
    busy = false;
  }

  function submitSelectGroup(unselect: boolean) {
    const pane = activePane();
    if (!pane?.visibleEntries) return;
    const ci = !/linux/i.test(globalThis.navigator?.platform ?? '');
    const next = selectMatching(
      pane.visibleEntries(),
      name,
      { select: !unselect, caseInsensitive: ci },
      new Set(pane.markedPaths?.() ?? pane.selectedPaths()),
    );
    pane.selectPaths?.(next);
    dialogs.close();
  }

  async function submitSaveSelection(parent: string, names: string[]) {
    busy = true;
    try {
      // FER-512: only a NAME inside `parent` is sent; the backend refuses
      // anything with separators or traversal.
      const file = name.trim() || 'selection.json';
      await saveSelectionFile(parent, file, names);
      dialogs.close();
    } catch (e) {
      fail(e);
    }
  }

  async function submitLoadSelection(d: { kind: 'loadSelection'; parent: string }) {
    const pane = activePane();
    if (!pane?.visibleEntries) return;
    busy = true;
    try {
      // FER-512: only a NAME inside `parent` is sent; same default as Save.
      const file = name.trim() || 'selection.json';
      const doc = await loadSelectionFile(d.parent, file);
      const saved = parseSelectionFile(JSON.stringify(doc));
      const applied = applySavedSelection(pane.visibleEntries(), saved);
      pane.selectPaths?.(applied.selected);
      dialogs.close();
      if (applied.missing.length > 0) {
        dialogs.open({
          kind: 'error',
          title: 'Load Selection',
          message:
            `Restored ${applied.selected.length} of ${saved.names.length} saved names; ` +
            `${applied.missing.length} not found in this folder.`,
        });
      }
    } catch (e) {
      fail(e);
    }
  }

  /** F2-15: apply the custom mask as the active pane's Show filter preset. */
  function submitShowFilter() {
    const pattern = name.trim();
    viewState.setFilterMode(ui.activePane, pattern ? 'custom' : 'all', pattern);
    dialogs.close();
  }

  /** F2-27: write the edited comment (empty clears it) and refresh the map. */
  async function submitComment(path: string) {
    busy = true;
    try {
      await comments.set(ui.activePane, path, commentDraft);
      dialogs.close();
    } catch (e) {
      fail(e);
    }
  }

  /** F2-27: confirmed quit — closes the (last) window, ending the app. */
  function confirmQuit() {
    void quitApp();
    dialogs.close();
  }

  async function submitMkdir(parent: string) {
    busy = true;
    try {
      await createEntry('dir', parent, name);
      dialogs.close();
      activePane()?.refresh();
    } catch (e) {
      fail(e);
    }
  }

  async function submitRename(path: string) {
    busy = true;
    try {
      const updated = await renameEntry(path, name);
      noteScopeMutation([path, updated.path]); // DSK-06.3: old and new location
      activity.noteFsChange(); // sidebar Recent/Frequent labels follow the rename
      dialogs.close();
      activePane()?.refresh();
    } catch (e) {
      fail(e);
    }
  }

  async function submitCopy(kind: 'copy' | 'move', paths: string[], target: string) {
    busy = true;
    try {
      if (kind === 'copy') await enqueueCopy(paths, target, policy);
      else {
        const opId = await enqueueMove(paths, target, policy);
        // DSK-06.3: the queued move is observed to its terminal state — the
        // scan goes stale only once the operation actually did work.
        recordQueuedMove(opId, paths, target);
      }
      dialogs.close();
    } catch (e) {
      fail(e);
    }
  }

  function submitPack(paths: string[], target: string, fmt: PackFormat) {
    busy = true;
    const level = packLevel;
    dialogs
      .withProgress({ title: 'Pack', archivePath: target }, (requestId) =>
        runPack({ paths, target, format: fmt, requestId, level }),
      )
      .then(() => {
        busy = false;
        dialogs.close();
        activePane()?.refresh();
      })
      .catch((e) => {
        busy = false;
        if (isCancelledError(e)) {
          dialogs.close();
          return;
        }
        const msg = e instanceof FsError ? e.message : String(e);
        dialogs.open({ kind: 'pack', paths, target, format: fmt, initialError: msg });
      });
  }

  async function submitDelete(paths: string[]) {
    busy = true;
    try {
      const report = await deleteEntries(paths, permanent);
      if (report.failed.length < report.requested) {
        // DSK-06.3: at least one path was deleted — a touching scan is stale.
        noteScopeMutation(paths);
        activity.noteFsChange();
      }
      if (report.failed.length > 0) {
        error =
          `${report.failed.length} of ${report.requested} could not be deleted: ` +
          report.failed.map((f) => `${f.message}`).join('; ');
        busy = false;
        activePane()?.refresh();
        return;
      }
      dialogs.close();
      activePane()?.refresh();
    } catch (e) {
      fail(e);
    }
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.preventDefault();
      dialogs.close();
    } else if (e.key === 'Enter' && !busy) {
      // FER-460 (review L7): in the scan offer, Enter on a focused button
      // activates THAT button; a bare Enter is the safe default (regular
      // scan). The elevated fast scan always needs an explicit choice.
      if (
        (d?.kind === 'fastScanOffer' || d?.kind === 'agentTerminalConfirm') &&
        e.target instanceof HTMLButtonElement
      )
        return;
      e.preventDefault();
      if (d?.kind === 'mkdir' && name.trim()) void submitMkdir(d.parent);
      else if (d?.kind === 'rename' && name.trim()) void submitRename(d.path);
      else if (d?.kind === 'delete') void submitDelete(d.paths);
      else if (d?.kind === 'selectGroup' && name.trim()) submitSelectGroup(d.unselect);
      else if (d?.kind === 'saveSelection') void submitSaveSelection(d.parent, d.names);
      else if (d?.kind === 'loadSelection') void submitLoadSelection(d);
      else if (d?.kind === 'showFilter') submitShowFilter();
      else if (d?.kind === 'comment') void submitComment(d.path);
      else if (d?.kind === 'pack' && !rarProbePending)
        submitPack(d.paths, packTarget || d.target, packFormat);
      else if (d?.kind === 'extractConflicts' && !busy)
        dialogs.confirmExtractConflicts(extractPolicy);
      else if (d?.kind === 'password' && password.trim()) submitPassword(d.retry);
      else if (d?.kind === 'fastScanOffer') d.choose('normal', fastDontAsk);
    }
  }
</script>

{#if d}
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div
    class="scrim"
    onmousedown={() => !busy && d.kind !== 'archiveProgress' && dialogs.close()}
  ></div>
  <div
    class="modal"
    role="dialog"
    aria-modal="true"
    aria-label={d.kind}
    tabindex="-1"
    bind:this={modalEl}
    onkeydown={onKey}
  >
    {#if d.kind === 'mkdir'}
      <h2>New folder</h2>
      <p class="path mono">{d.parent}</p>
      <input
        bind:this={inputEl}
        bind:value={name}
        placeholder="Folder name"
        spellcheck="false"
        disabled={busy}
      />
    {:else if d.kind === 'rename'}
      <h2>Rename</h2>
      <p class="path mono">{d.original}</p>
      <input
        bind:this={inputEl}
        bind:value={name}
        placeholder="New name"
        spellcheck="false"
        disabled={busy}
      />
    {:else if d.kind === 'copy' || d.kind === 'move'}
      <h2>
        {d.kind === 'copy' ? 'Copy' : 'Move'}
        {d.paths.length === 1 ? ' 1 item' : ` ${d.paths.length} items`}
      </h2>
      <p>
        <span class="mono">{d.totalFiles}</span> files ·
        <span class="mono">{(d.totalBytes / 1024 / 1024).toFixed(1)}</span> MB →
        <span class="mono">{d.target}</span>
      </p>
      {#if d.conflicts.length > 0}
        <p class="error">
          {d.conflicts.length} name conflict{d.conflicts.length === 1 ? '' : 's'} in the target.
        </p>
        <ul class="list">
          {#each d.conflicts.slice(0, 4) as c (c.dest)}
            <li class="mono">{c.dest}</li>
          {/each}
        </ul>
        <div class="policies" role="radiogroup" aria-label="Conflict policy">
          {#each [['skip', 'Skip'], ['overwrite', 'Overwrite'], ['overwriteOlder', 'Overwrite older'], ['rename', 'Keep both']] as [val, label] (val)}
            <label class="check">
              <input type="radio" name="policy" bind:group={policy} value={val} disabled={busy} />
              {label}
            </label>
          {/each}
        </div>
      {/if}
    {:else if d.kind === 'delete'}
      <h2>{permanent ? 'Delete permanently' : 'Delete'}</h2>
      <p>
        {d.paths.length === 1 ? 'Delete 1 item?' : `Delete ${d.paths.length} items?`}
        {#if !permanent}<span class="dim">to Recycle Bin</span>{/if}
      </p>
      <ul class="list">
        {#each d.paths.slice(0, 5) as p (p)}
          <li class="mono">{p}</li>
        {/each}
        {#if d.paths.length > 5}<li class="dim">… and {d.paths.length - 5} more</li>{/if}
      </ul>
      <label class="check">
        <input
          type="checkbox"
          bind:checked={permanent}
          disabled={busy || deleteConfirmationBypassed}
        />
        Delete permanently (skip Recycle Bin)
      </label>
    {:else if d.kind === 'password'}
      <h2>Password required</h2>
      <p class="path mono">{d.archivePath}</p>
      {#if d.message}<p class="error" role="alert">{d.message}</p>{/if}
      <input
        bind:this={inputEl}
        bind:value={password}
        type="password"
        autocomplete="off"
        placeholder="Archive password"
        disabled={busy}
      />
    {:else if d.kind === 'extractConflicts'}
      <!-- FER-368 (D21): ONE ask for the whole extraction batch. -->
      <h2>Extract conflicts</h2>
      <p>
        {d.conflicts.length}
        name conflict{d.conflicts.length === 1 ? '' : 's'} in
        <span class="mono">{d.target}</span>.
      </p>
      <ul class="list">
        {#each d.conflicts.slice(0, 20) as c (c.destinationPath)}
          <li class="mono">{c.destinationPath}</li>
        {/each}
        {#if d.conflicts.length > 20}
          <li class="dim">… and {d.conflicts.length - 20} more</li>
        {/if}
      </ul>
      <div class="policies" role="radiogroup" aria-label="Conflict policy">
        {#each [['skip', 'Skip'], ['overwrite', 'Overwrite'], ['rename', 'Rename automatically']] as [val, label] (val)}
          <label class="check">
            <input
              type="radio"
              name="extractPolicy"
              bind:group={extractPolicy}
              value={val}
              disabled={busy}
            />
            {label}
          </label>
        {/each}
      </div>
    {:else if d.kind === 'pack'}
      <h2>Pack to archive</h2>
      <p>
        {d.paths.length === 1 ? 'Pack 1 item' : `Pack ${d.paths.length} items`} →
        <span class="mono">{packTarget || d.target}</span>
      </p>
      <p class="dim">Format: {packFormat === 'sevenz' ? 'SEVENZ' : packFormat.toUpperCase()}</p>
      <div class="policies" role="radiogroup" aria-label="Format">
        {#each rarAvailable ? [['zip', 'ZIP'], ['sevenz', '7Z'], ['rar', 'RAR']] : [['zip', 'ZIP'], ['sevenz', '7Z']] as [fmtVal, fmtLabel] (fmtVal)}
          <label class="check">
            <input
              type="radio"
              name="packFormat"
              bind:group={packFormat}
              value={fmtVal}
              onchange={() => changePackFormat(fmtVal as PackFormat)}
              disabled={busy}
            />
            {fmtLabel}
          </label>
        {/each}
      </div>
      {#if rarProbing}
        <p class="dim" data-testid="rar-probing">Memeriksa WinRAR…</p>
      {/if}
      <p class="dim">Kompresi:</p>
      <div class="policies" role="radiogroup" aria-label="Kompresi">
        {#each [['store', 'Simpan'], ['fast', 'Cepat'], ['normal', 'Normal'], ['max', 'Maksimal']] as [val, label] (val)}
          <label class="check">
            <input
              type="radio"
              name="packLevel"
              bind:group={packLevel}
              value={val}
              onchange={() => {
                packLevel = val as ArchiveCompressionLevel;
              }}
              disabled={busy}
            />
            {label}
          </label>
        {/each}
      </div>
    {:else if d.kind === 'selectGroup'}
      <h2>{d.unselect ? 'Unselect Group' : 'Select Group'}</h2>
      <p class="path mono">{d.parent}</p>
      <input
        bind:this={inputEl}
        bind:value={name}
        placeholder="*.txt; report-??.csv"
        spellcheck="false"
        disabled={busy}
      />
      <p class="dim">
        {groupMatchCount}
        {groupMatchCount === 1 ? 'match' : 'matches'} in this folder
      </p>
    {:else if d.kind === 'saveSelection'}
      <h2>Save Selection To File</h2>
      <p class="path mono">{d.parent}</p>
      <p>
        Save {d.names.length}
        selected {d.names.length === 1 ? 'name' : 'names'} (versioned JSON) into this folder.
      </p>
      <input
        bind:this={inputEl}
        bind:value={name}
        placeholder="selection.json"
        spellcheck="false"
        disabled={busy}
      />
    {:else if d.kind === 'loadSelection'}
      <h2>Load Selection From File</h2>
      <p class="path mono">{d.parent}</p>
      <p>Name of the selection file in this folder.</p>
      <input
        bind:this={inputEl}
        bind:value={name}
        placeholder="selection.json"
        spellcheck="false"
        disabled={busy}
      />
    {:else if d.kind === 'showFilter'}
      <h2>Custom Filter</h2>
      <p class="path mono">{d.parent}</p>
      <input
        bind:this={inputEl}
        bind:value={name}
        placeholder="*.txt; report-??.csv — empty clears the filter"
        spellcheck="false"
        disabled={busy}
      />
    {:else if d.kind === 'comment'}
      <h2>Edit Comment</h2>
      <p class="path mono">{d.name}</p>
      <input
        bind:this={inputEl}
        bind:value={commentDraft}
        placeholder="Comment — empty clears it (stored in this folder's .ferrum sidecar)"
        spellcheck="false"
        disabled={busy}
      />
    {:else if d.kind === 'agentTerminalConfirm'}
      <h2>{d.title}</h2>
      <p>{d.intro}</p>
      <ul class="agentWarnings" data-testid="agent-terminal-warnings">
        {#each d.warnings as w, i (i)}<li>{w}</li>{/each}
      </ul>
      <p>Buka hanya kalau Anda memercayai folder ini dan isinya.</p>
    {:else if d.kind === 'quitConfirm'}
      <h2>Quit Ferrum?</h2>
      <p>
        {d.operations}
        {d.operations === 1 ? 'operation is' : 'operations are'} still running. Quitting now cancels the
        pending work.
      </p>
    {:else if d.kind === 'fastScanOffer'}
      <h2>Scan lebih cepat?</h2>
      {#if d.purpose === 'sync'}
        <p>
          Drive {d.volume}: memakai NTFS. Scan cepat membaca tabel file (MFT) langsung lewat helper
          terpisah yang butuh izin admin (UAC), jadi pembacaan kedua folder jauh lebih singkat.
          Ferrum sendiri tetap berjalan tanpa hak admin. Hasil yang akan dijalankan tetap diperiksa
          ulang dengan scan biasa sebelum sync menulis apa pun.
        </p>
        <p class="path mono">{d.root}</p>
        <p class="path mono">{d.rightRoot}</p>
      {:else}
        {#if d.indexAvailable}
          <p>
            <strong>Instan (indeks):</strong> FerrumIndexer sudah mengindeks drive ini, jadi ukuran folder
            langsung tersedia tanpa izin admin. Di mode Bersama, isi folder profil pengguna tidak ikut
            dihitung.
          </p>
        {/if}
        {#if d.fastAvailable !== false}
          <p>
            Drive {d.volume}: memakai NTFS. Scan cepat membaca tabel file (MFT) langsung lewat
            helper terpisah yang butuh izin admin (UAC), jadi biasanya selesai dalam hitungan detik.
            Ferrum sendiri tetap berjalan tanpa hak admin.
          </p>
        {/if}
        <p class="path mono">{d.root}</p>
      {/if}
      <label class="check">
        <input type="checkbox" bind:checked={fastDontAsk} />
        Jangan tanya lagi (selalu scan biasa; aktifkan lagi di Settings)
      </label>
    {:else if d.kind === 'archiveProgress'}
      <!-- FER-79: running extract / Test Archive job. Batal and Esc cancel it. -->
      <h2>{d.title}{d.position ? ` (${d.position})` : ''}</h2>
      <p class="path mono">{d.archivePath}</p>
      {@const pct = progressPercent(d)}
      <div
        class="job-bar"
        role="progressbar"
        aria-label="Progres arsip"
        aria-valuemin="0"
        aria-valuemax="100"
        aria-valuenow={pct ?? undefined}
      >
        <div
          class="job-fill"
          class:indeterminate={pct === null}
          style:width={pct === null ? undefined : `${pct}%`}
        ></div>
      </div>
      <p class="job-meta">
        {formatMiB(d.processedBytes)}{d.totalBytes ? ` / ${formatMiB(d.totalBytes)}` : ''}
        · {d.processedEntries}{d.totalEntries ? ` / ${d.totalEntries}` : ''} anggota
        {#if d.writtenEntries !== undefined}
          · {d.writtenEntries} ditulis
        {/if}
        {#if d.skippedEntries !== undefined && d.skippedEntries > 0}
          · {d.skippedEntries} dilewati
        {/if}
      </p>
      {#if d.currentMember}<p class="path mono">{d.currentMember}</p>{/if}
      {#if d.cancelling}<p class="job-meta" role="status">Membatalkan...</p>{/if}
    {:else if d.kind === 'multipart'}
      <!-- FER-75: a volume that cannot be used alone. -->
      <h2>Arsip multipart</h2>
      <p class="path mono">{d.archivePath}</p>
      {#if d.info.kind === 'notFirst'}
        <p>Ini bagian {d.info.part} dari arsip multipart.</p>
        {#if d.info.firstExists}
          <p>Buka bagian pertama: <span class="mono">{d.info.firstName}</span></p>
        {:else}
          <p>
            Bagian pertama (<span class="mono">{d.info.firstName}</span>) tidak ditemukan di folder
            ini.
          </p>
        {/if}
      {:else if d.info.kind === 'missing'}
        <p>
          Bagian arsip multipart hilang. Ekstrak dan tes ditolak supaya tidak ada hasil sebagian.
        </p>
        <ul class="tr-fail">
          {#each d.info.missing as m (m)}<li class="mono">{m}</li>{/each}
        </ul>
      {/if}
    {:else if d.kind === 'error'}
      <h2>{d.title}</h2>
      <p style="white-space: pre-wrap;">{d.message}</p>
    {:else if d.kind === 'testReport'}
      <h2>{d.title}</h2>
      <ul class="test-report" aria-label="Hasil tes arsip">
        {#each d.reports as r (r.archivePath)}
          <li class={`tr tr-${r.status}`}>
            <div class="tr-head">
              <span class="tr-badge">{testOutcomeLabel(r.status)}</span>
              <span class="tr-name mono">{fileNameOf(r.archivePath)}</span>
              <span class="tr-meta"
                >{r.passed}/{r.total} anggota{r.durationMs !== undefined
                  ? ` · ${(r.durationMs / 1000).toFixed(2)} dtk`
                  : ''}</span
              >
            </div>
            {#if r.note}<p class="tr-note">{r.note}</p>{/if}
            {#if r.failures.length > 0}
              <ul class="tr-fail">
                {#each r.failures as f, i (`${i}:${f.memberPath}`)}
                  <li>
                    <span class="mono">{sanitizeMemberPath(f.memberPath)}</span> · {f.message}
                  </li>
                {/each}
              </ul>
            {/if}
          </li>
        {/each}
      </ul>
    {:else if d.kind === 'extractSummary'}
      <h2>{d.title ?? 'Ekstrak selesai'}</h2>
      {#if d.target}<p class="path mono">{d.target}</p>{/if}
      <div class="extract-summary">
        <p>Ditulis: {d.written} berkas</p>
        <p>Dilewati: {d.skipped} berkas</p>
        {#if d.renamed !== undefined}
          <p>Dinamai ulang: {d.renamed} berkas</p>
        {/if}
      </div>
    {/if}

    {#if error}<p class="error" role="alert">{error}</p>{/if}

    {#if d.kind === 'agentTerminalConfirm'}
      <div class="actions">
        <button class="ghost" bind:this={agentCancelBtn} onclick={() => dialogs.close()}
          >Batal</button
        >
        <button class="primary danger" onclick={d.onConfirm}>Buka tetap</button>
      </div>
    {:else if d.kind === 'archiveProgress'}
      <div class="actions">
        <button class="ghost" onclick={() => dialogs.cancelArchiveJob()} disabled={d.cancelling}>
          Batal
        </button>
      </div>
    {:else if d.kind === 'multipart'}
      <div class="actions">
        <button class="ghost" onclick={() => dialogs.close()}>Tutup</button>
        {#if d.info.kind === 'notFirst' && d.info.firstExists}
          <button class="primary" onclick={() => dialogs.openFirstPart()}>
            Buka bagian pertama
          </button>
        {/if}
      </div>
    {:else if d.kind === 'extractSummary'}
      <div class="actions">
        <button class="primary" onclick={() => dialogs.close()}>Tutup</button>
      </div>
    {:else if d.kind !== 'error' && d.kind !== 'testReport'}
      <div class="actions">
        <button class="ghost" onclick={() => dialogs.close()} disabled={busy}>Cancel</button>
        {#if d.kind === 'mkdir'}
          <button
            class="primary"
            onclick={() => void submitMkdir(d.parent)}
            disabled={busy || !name.trim()}
          >
            Create
          </button>
        {:else if d.kind === 'rename'}
          <button
            class="primary"
            onclick={() => void submitRename(d.path)}
            disabled={busy || !name.trim()}
          >
            Rename
          </button>
        {:else if d.kind === 'password'}
          <button
            class="primary"
            onclick={() => submitPassword(d.retry)}
            disabled={busy || !password}
          >
            Unlock
          </button>
        {:else if d.kind === 'pack'}
          <button
            class="primary"
            onclick={() => void submitPack(d.paths, packTarget || d.target, packFormat)}
            disabled={busy || rarProbePending}
          >
            Pack
          </button>
        {:else if d.kind === 'extractConflicts'}
          <!-- FER-368: the single whole-batch decision (apply-all inherent). -->
          <button
            class="primary"
            onclick={() => dialogs.confirmExtractConflicts(extractPolicy)}
            disabled={busy}
          >
            Extract
          </button>
        {:else if d.kind === 'copy' || d.kind === 'move'}
          <button
            class="primary"
            onclick={() => void submitCopy(d.kind, d.paths, d.target)}
            disabled={busy || (d.conflicts.length > 0 && policy === 'ask')}
          >
            {d.kind === 'copy' ? 'Copy' : 'Move'}
          </button>
        {:else if d.kind === 'selectGroup'}
          <button
            class="primary"
            onclick={() => submitSelectGroup(d.unselect)}
            disabled={busy || !name.trim()}
          >
            {d.unselect ? 'Unselect' : 'Select'}
          </button>
        {:else if d.kind === 'saveSelection'}
          <button
            class="primary"
            onclick={() => void submitSaveSelection(d.parent, d.names)}
            disabled={busy}
          >
            Save
          </button>
        {:else if d.kind === 'loadSelection'}
          <button class="primary" onclick={() => void submitLoadSelection(d)} disabled={busy}>
            Load
          </button>
        {:else if d.kind === 'showFilter'}
          <button class="primary" onclick={submitShowFilter} disabled={busy}>Filter</button>
        {:else if d.kind === 'comment'}
          <button class="primary" onclick={() => void submitComment(d.path)} disabled={busy}>
            Save
          </button>
        {:else if d.kind === 'quitConfirm'}
          <button class="primary danger" onclick={confirmQuit}>Quit anyway</button>
        {:else if d.kind === 'fastScanOffer'}
          <button
            class="ghost"
            bind:this={safeScanBtn}
            onclick={() => d.choose('normal', fastDontAsk)}>Scan biasa</button
          >
          {#if d.fastAvailable !== false}
            <button
              class={d.indexAvailable ? 'ghost' : 'primary'}
              onclick={() => d.choose('fast', fastDontAsk)}
            >
              Scan cepat (admin)
            </button>
          {/if}
          {#if d.indexAvailable}
            <button class="primary" onclick={() => d.choose('index', fastDontAsk)}>
              Instan (indeks)
            </button>
          {/if}
        {:else}
          <button class="primary danger" onclick={() => void submitDelete(d.paths)} disabled={busy}>
            {permanent ? 'Delete permanently' : 'Delete'}
          </button>
        {/if}
      </div>
    {:else}
      <div class="actions">
        <button class="primary" onclick={() => dialogs.close()}>OK</button>
      </div>
    {/if}
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
    top: 30%;
    transform: translateX(-50%);
    width: min(460px, 92vw);
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
  .agentWarnings {
    margin: 0 0 8px;
    padding-left: 18px;
    font-size: 12px;
    overflow-wrap: anywhere;
  }
  .path {
    margin: 0;
    font-size: calc(11px + var(--uiStep, 0px));
    color: var(--text3);
    word-break: break-all;
  }
  p {
    margin: 0;
    font-size: calc(12.5px + var(--uiStep, 0px));
    color: var(--text2);
  }
  input[type='text'],
  input:not([type]) {
    height: 30px;
    padding: 0 10px;
    border: 0;
    outline: 0;
    border-radius: var(--r);
    background: var(--bg);
    box-shadow: 0 0 0 1px var(--line2);
    color: var(--text);
    font-size: calc(13px + var(--uiStep, 0px));
  }
  input:focus-visible {
    box-shadow: 0 0 0 1.5px var(--accentLine);
  }
  .list {
    margin: 0;
    padding-left: 18px;
    font-size: calc(11.5px + var(--uiStep, 0px));
    color: var(--text2);
    max-height: 110px;
    overflow: auto;
  }
  .check {
    display: flex;
    gap: 8px;
    align-items: center;
    font-size: calc(12px + var(--uiStep, 0px));
    color: var(--text2);
  }
  .dim {
    color: var(--text3);
  }
  .error {
    color: var(--red);
    font-size: calc(12px + var(--uiStep, 0px));
    word-break: break-word;
  }
  .policies {
    display: flex;
    flex-wrap: wrap;
    gap: 6px 16px;
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 4px;
  }
  button {
    height: 28px;
    padding: 0 14px;
    border: 0;
    border-radius: var(--r);
    font-size: calc(12.5px + var(--uiStep, 0px));
    cursor: default;
  }
  .ghost {
    background: transparent;
    color: var(--text2);
  }
  .ghost:hover {
    background: var(--hover2);
    color: var(--text);
  }
  .primary {
    background: var(--accent);
    color: #fff;
    font-weight: 600;
  }
  .primary:hover {
    filter: brightness(1.08);
  }
  .primary.danger {
    background: var(--red);
  }
  button:disabled {
    opacity: 0.5;
  }
  .test-report {
    list-style: none;
    margin: 8px 0 0;
    padding: 0 4px 0 0;
    max-height: min(60vh, 480px);
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .test-report .tr {
    border: 1px solid var(--border, #d0d7de);
    border-radius: 6px;
    padding: 6px 8px;
  }
  .tr-head {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    align-items: baseline;
  }
  .tr-badge {
    font-weight: 600;
    white-space: nowrap;
  }
  .tr-meta {
    opacity: 0.7;
    font-size: 0.9em;
  }
  .tr-note,
  .tr-fail {
    margin: 4px 0 0;
    font-size: 0.9em;
  }
  .tr-fail {
    padding-left: 18px;
  }
  .job-bar {
    height: 8px;
    border-radius: 4px;
    background: var(--border, #d0d7de);
    overflow: hidden;
  }
  .job-fill {
    height: 100%;
    background: var(--accent, #0e7490);
    transition: width 120ms linear;
  }
  .job-fill.indeterminate {
    width: 35%;
    animation: jobSlide 1.1s ease-in-out infinite alternate;
  }
  @keyframes jobSlide {
    from {
      margin-left: 0;
    }
    to {
      margin-left: 65%;
    }
  }
  .job-meta {
    margin: 0;
    font-size: 0.9em;
    opacity: 0.75;
  }
</style>
