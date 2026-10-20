/** Single action registry shared by menus, palette, shortcuts, and keybar. */
import { ui, fullPageOpen, type Theme } from './uiState.svelte';
import { modalDepth } from './dialogKit';
import { activePane, inactivePane } from './stores/panesRegistry.svelte';
import { dialogs } from './stores/dialogStore.svelte';
import { ops } from './stores/opsStore.svelte';
import { baseName } from './stores/paneStore.svelte';
import { restoreSelection, saveSelection } from './stores/selectionMemory.svelte';
import { viewState } from './stores/viewStateStore.svelte';
import { searchStore } from './stores/searchStore.svelte';
import { activity } from './stores/activityStore.svelte';
import { previewStore } from './stores/previewStore.svelte';
import { compareStore } from './stores/compareStore.svelte';
import { copyText } from './clipboard';
import { copySelectionToClipboard, pasteFromClipboard } from './stores/fileClipboard';
import { rootOf } from './format';
import { toast } from './stores/toastStore.svelte';
import { folderCopyText, namesText, pathsText, pickCopyEntries, type CopyEntry } from './copyPath';
import {
  applySavedSelection,
  formatDetails,
  formatNames,
  formatPathDetails,
  formatPaths,
  invertSelection,
  selectSameExtension,
  type EntryInfo,
} from './markSelection';
import { multiRename } from './stores/multiRenameStore.svelte';
import { hotlist } from './stores/hotlistStore.svelte';
import { historyMenu } from './stores/historyMenuStore.svelte';
import { branch } from './stores/branchStore.svelte';
import {
  archiveMemberSelection,
  isCombineCandidate,
  openArchiveChecksum,
  openChecksumCreate,
  openChecksumVerify,
  openCombineParts,
  openSplitFile,
} from './stores/fileToolsStore.svelte';
import {
  openCdTree,
  openDesktop,
  goBack,
  openInOtherPane,
  swapPanes,
  targetFromSource,
} from './navigation';
import {
  openAttributes,
  openOccupiedSpace,
  openProperties,
  openSystemInfo,
  openVolumeInfo,
} from './stores/propertiesStore.svelte';
import { shell } from './stores/shellStore.svelte';
import { isArchiveFile } from './stores/archiveFetcher';
import { comments } from './stores/commentStore.svelte';
import { codecs } from './stores/codecsStore.svelte';
import { viewPresets } from './stores/viewPresetsStore.svelte';
import { columns } from './stores/columnsStore.svelte';
import { separateTree } from './stores/separateTreeStore.svelte';
import { duWorkspace } from './diskUsage/duWorkspace.svelte';
import { openSearchSeparate } from './searchSeparate';
import { quitApp } from './quit';
import { recentCommands } from './stores/recentCommands.svelte';
import { aiChatHost } from './ai/aiChatHost.svelte';
import { addSelectionToChat, addToChatTargets, askAgentAboutSelection } from './ai/addToChat';
import { rankMatches, type MatchField } from './paletteMatch';
import { AGENT_IDS } from './ai/agentCli.svelte';
import { AGENT_LABELS, openAgentTerminal, terminalReady } from './ai/agentTerminal';

export interface Shortcut {
  /** KeyboardEvent.key value, e.g. 'Tab', 'F5', 'k', 'Enter', 'Backspace'. */
  key: string;
  ctrlOrCmd?: boolean;
  /**
   * `true`/`false` require the exact shift state (the default). `'any'`
   * accepts the key with OR without Shift — FER-367: the TC Num-key chords
   * whose `e.key` is the same character from the top row (Shift+Digit8 →
   * '*') and the numpad (NumpadMultiply → '*', no Shift). Only used where
   * no other binding can produce that bare key (checked by the uniqueness
   * test) and text inputs never dispatch shortcuts (App.onKeydown guard).
   */
  shift?: boolean | 'any';
  alt?: boolean;
}

export interface Action {
  id: string;
  label: string | (() => string);
  group: string;
  shortcut?: Shortcut;
  /** Context-dependent availability (for example, file selection). */
  available?: () => boolean;
  /** Unimplemented actions have a reason, but no handler or shortcut. */
  disabledReason?: string;
  run?: () => void;
  /** Extra palette search terms (aliases, Indonesian names). Falls back to `ACTION_KEYWORDS`. */
  keywords?: string[];
  /**
   * FER-489: when 'app-keydown', the key is handled by App.onKeydown itself (paneTab.ts); the
   * registry entry only serves the palette, menus and shortcut labels.
   */
  scope?: 'app-keydown';
}

const ctrl = (key: string): Shortcut => ({ key, ctrlOrCmd: true });

/** FER-572: Ctrl+C must still copy highlighted text (preview, status bar) instead of files. */
function textIsSelected(): boolean {
  try {
    return (globalThis.getSelection?.()?.toString() ?? '') !== '';
  } catch {
    return false;
  }
}

// ── F2-14 Mark helpers ────────────────────────────────────────────────────
// All selection math runs on pane-reported plain data (visibleEntries), so
// menus/palette/shortcuts never touch DOM rows and work on 100k listings.

function markEntries(): EntryInfo[] {
  return activePane()?.visibleEntries?.() ?? [];
}

function selectedMarkEntries(): EntryInfo[] {
  const selected = new Set(activePane()?.selectedPaths() ?? []);
  return markEntries().filter((e) => selected.has(e.path));
}

/** Only the explicitly marked rows (never the implicit cursor row). */
function markedPaths(): string[] {
  const pane = activePane();
  return pane ? (pane.markedPaths?.() ?? pane.selectedPaths()) : [];
}

function replacePaneSelection(next: ReadonlySet<string>): void {
  activePane()?.selectPaths?.(next);
}

async function copyToClipboard(text: string, title: string): Promise<void> {
  if (await copyText(text)) return;
  dialogs.open({ kind: 'error', title, message: 'The clipboard is not available right now.' });
}

// ── FER-483 Ctrl+1/2/3: copy folder path / names / full paths ─────────────
// Items: every selected row in visible order, else the cursor row.

function copyTargets(): CopyEntry[] {
  const pane = activePane();
  if (!pane) return [];
  const selected = pane.selectedPaths();
  const picked = pickCopyEntries(pane.visibleEntries?.() ?? [], selected, pane.cursorEntry());
  if (picked.length === 0 && selected.length > 0) {
    // No row data from the pane: fall back to plain path identity.
    return selected.map((path) => ({ name: baseName(path), path }));
  }
  return picked;
}

async function copyWithToast(text: string, done: string, title: string): Promise<void> {
  if (text === '') return;
  if (await copyText(text)) {
    toast.show(`Disalin: ${done}`);
    return;
  }
  dialogs.open({ kind: 'error', title, message: 'The clipboard is not available right now.' });
}

// ── F2-18 compare targets ─────────────────────────────────────────────────
// Priority: two files selected in the active pane → exactly one file
// selected on each pane (Total Commander's marked-in-both-panes flow).

function compareContentTargets(): [string, string] | undefined {
  const pane = activePane();
  const other = inactivePane();
  if (!pane) return undefined;
  const selected = pane.selectedPaths();
  if (selected.length === 2) return [selected[0], selected[1]];
  if (selected.length === 1 && other) {
    const otherSelected = other.selectedPaths();
    if (otherSelected.length === 1 && otherSelected[0] !== selected[0]) {
      return [selected[0], otherSelected[0]];
    }
  }
  return undefined;
}

export const actions: Action[] = [
  {
    id: 'pane.switch',
    // FER-489: TC behaviour. Tab / Shift+Tab inside a file list moves focus to the OTHER pane's
    // list (two panes, so both directions are the same). Elsewhere Tab stays native.
    label: 'Switch to other pane (Tab)',
    group: 'Navigate',
    shortcut: { key: 'Tab', shift: 'any' },
    scope: 'app-keydown',
    run: () => {
      if (inactivePane()?.focusList === undefined) return; // single-pane layout: nothing to switch to
      ui.togglePane();
      activePane()?.focusList?.();
    },
  },
  {
    id: 'nav.up',
    label: 'Go to parent folder',
    group: 'Navigate',
    shortcut: { key: 'Backspace' },
    available: () => activePane() !== undefined,
    run: () => activePane()?.goUp(),
  },
  {
    // FER-495: Ctrl+\ jumps to the root of the active pane's drive / UNC share (Total Commander).
    id: 'nav.root',
    label: 'Go to root',
    group: 'Navigate',
    shortcut: ctrl('\\'),
    available: () => {
      const path = activePane()?.path;
      return !!path && rootOf(path) !== '';
    },
    run: () => {
      const pane = activePane();
      const root = pane ? rootOf(pane.path) : '';
      if (root) pane?.openPath?.(root);
    },
  },
  {
    // FER-456: Alt+D is the Windows convention for the address bar (Disk
    // Usage moved to Alt+Shift+D).
    id: 'nav.editPath',
    label: 'Edit path (address bar)',
    group: 'Navigate',
    shortcut: { key: 'd', alt: true },
    available: () => activePane()?.editPath !== undefined,
    run: () => activePane()?.editPath?.(),
  },
  {
    id: 'tab.new',
    label: 'New tab',
    group: 'Tabs',
    shortcut: ctrl('t'),
    available: () => activePane() !== undefined,
    run: () => activePane()?.newTab(),
  },
  {
    id: 'tab.close',
    label: 'Close tab',
    group: 'Tabs',
    shortcut: ctrl('w'),
    available: () => activePane() !== undefined,
    run: () => activePane()?.closeCurrentTab(),
  },
  {
    // F2-25: Enter runs the entry the Total Commander way — a folder
    // navigates, a file is handed to its OS default association. There is
    // deliberately no second open path: Ferrum's only "internal
    // associations" are its clearly registered preview/editor surfaces
    // (Quick View via the kind registry), never a hook on Enter.
    id: 'file.open',
    label: () => {
      const cursor = activePane()?.cursorEntry();
      if (cursor?.isDir === false) {
        return isArchiveFile(cursor.name) ? 'Open archive' : 'Open with associated application';
      }
      return 'Open selected folder';
    },
    group: 'File',
    shortcut: { key: 'Enter' },
    available: () => activePane()?.cursorEntry() !== undefined,
    run: () => {
      const pane = activePane();
      const cursor = pane?.cursorEntry();
      if (!pane || !cursor) return;
      // Folders and supported archives (.zip/.7z/.rar) navigate, like a
      // double-click (FER-450, Total Commander behaviour); other files go to
      // the OS association, and the shell store records the Activity receipt
      // once the OS accepted (FER-249).
      if (cursor.isDir || isArchiveFile(cursor.name)) pane.openCursorEntry?.();
      else void shell.openFileExternal(cursor.path);
    },
  },
  // ── F2-25: Associate With opens the OS picker; the USER decides there —
  // Ferrum never writes a system association itself.
  {
    id: 'file.associate',
    label: 'Associate With…',
    group: 'File',
    available: () => {
      const pane = activePane();
      const cursor = pane?.cursorEntry();
      return cursor !== undefined && !cursor.isDir && pane?.selectedPaths().length === 1;
    },
    run: () => {
      const target = activePane()?.cursorEntry();
      if (target && !target.isDir) void shell.associateWith(target.path);
    },
  },
  {
    id: 'file.rename',
    label: 'Rename',
    group: 'File',
    shortcut: { key: 'F2' },
    available: () => {
      const pane = activePane();
      const selected = pane?.selectedPaths();
      return selected?.length === 1 && selected[0] === pane?.cursorEntry()?.path;
    },
    run: () => dialogs.beginRename(),
  },
  {
    id: 'file.copy',
    label: 'Copy to other pane',
    group: 'File',
    shortcut: { key: 'F5' },
    available: () =>
      (activePane()?.selectedPaths().length ?? 0) > 0 && inactivePane() !== undefined,
    run: () => void dialogs.beginCopyFlow(),
  },
  {
    id: 'file.move',
    label: 'Move to other pane',
    group: 'File',
    shortcut: { key: 'F6' },
    available: () =>
      (activePane()?.selectedPaths().length ?? 0) > 0 && inactivePane() !== undefined,
    run: () => void dialogs.beginMoveFlow(),
  },
  {
    // FER-572: Windows file clipboard, shared with Explorer. Ctrl+C / Ctrl+X / Ctrl+V.
    id: 'file.clipCopy',
    label: 'Copy files to clipboard',
    group: 'File',
    shortcut: ctrl('c'),
    available: () => (activePane()?.selectedPaths().length ?? 0) > 0 && !textIsSelected(),
    run: () => void copySelectionToClipboard(false),
    keywords: ['copy', 'salin', 'clipboard', 'explorer'],
  },
  {
    id: 'file.clipCut',
    label: 'Cut files to clipboard',
    group: 'File',
    shortcut: ctrl('x'),
    available: () => (activePane()?.selectedPaths().length ?? 0) > 0 && !textIsSelected(),
    run: () => void copySelectionToClipboard(true),
    keywords: ['cut', 'potong', 'clipboard', 'explorer'],
  },
  {
    id: 'file.clipPaste',
    label: 'Paste files from clipboard',
    group: 'File',
    shortcut: ctrl('v'),
    available: () => activePane() !== undefined,
    run: () => void pasteFromClipboard(),
    keywords: ['paste', 'tempel', 'clipboard', 'explorer'],
  },
  {
    id: 'file.mkdir',
    label: 'New folder',
    group: 'File',
    shortcut: { key: 'F7' },
    available: () => activePane() !== undefined,
    run: () => dialogs.beginMkdir(),
  },
  {
    id: 'file.delete',
    label: 'Delete (Recycle Bin)',
    group: 'File',
    shortcut: { key: 'F8' },
    available: () => (activePane()?.selectedPaths().length ?? 0) > 0,
    run: () => dialogs.beginDelete(false),
  },
  {
    // FER-364 (D05/D44): permanent delete is enabled on its intended
    // Shift+F8 slot — the plain F8 action keeps the Recycle Bin default.
    // The confirm dialog opens with the permanent checkbox pre-checked
    // (carried by beginDelete) and stays explicitly confirmable.
    id: 'file.delete.permanent',
    label: 'Delete permanently',
    group: 'File',
    shortcut: { key: 'F8', shift: true },
    available: () => (activePane()?.selectedPaths().length ?? 0) > 0,
    run: () => dialogs.beginDelete(true),
  },
  {
    id: 'file.multiRename',
    label: 'Multi-Rename Tool',
    group: 'File',
    shortcut: ctrl('m'),
    available: () => (activePane()?.selectedPaths().length ?? 0) > 0,
    run: () => multiRename.openForSelection(),
  },
  // ── F2-27: Edit Comment writes the per-directory sidecar (comments.rs);
  // Comments view shows the same map as a listing column.
  {
    id: 'file.comment',
    label: 'Edit Comment',
    group: 'File',
    available: () => {
      const pane = activePane();
      const cursor = pane?.cursorEntry();
      return cursor !== undefined && !cursor.isDir && pane?.selectedPaths().length === 1;
    },
    run: () =>
      dialogs.beginComment(comments.for(ui.activePane, activePane()?.cursorEntry()?.name ?? '')),
  },
  {
    // F2-27: print hands the file to its associated application's print
    // verb — no custom renderer. Unsupported platforms report it honestly.
    id: 'file.print',
    label: 'Print',
    group: 'File',
    available: () => {
      const pane = activePane();
      const cursor = pane?.cursorEntry();
      return cursor !== undefined && !cursor.isDir && pane?.selectedPaths().length === 1;
    },
    run: () => {
      const target = activePane()?.cursorEntry();
      if (target && !target.isDir) void shell.print(target.path);
    },
  },
  // ── F2-27: Encode/Decode — narrow, honest format support (codecs.rs):
  // MIME base64 encode; base64/UUE/XXE decode; BinHex refused. Work
  // enqueues onto the operation queue (progress in OpsPanel).
  {
    id: 'file.encode',
    label: 'Encode File',
    group: 'File',
    available: () => (activePane()?.selectedPaths().length ?? 0) > 0,
    run: () => void codecs.encodeSelection(),
  },
  {
    id: 'file.decode',
    label: 'Decode File',
    group: 'File',
    available: () => (activePane()?.selectedPaths().length ?? 0) > 0,
    run: () => void codecs.decodeSelection(),
  },
  // ── F2-27: Quit — respects in-flight queue operations with a confirm.
  {
    id: 'file.quit',
    label: 'Quit',
    group: 'File',
    run: () => {
      const active = ops.active.length;
      if (active > 0) {
        dialogs.open({ kind: 'quitConfirm', operations: active });
        return;
      }
      void quitApp();
    },
  },
  // ── F2-12 archive operations (TC parity) ───────────────────────────────
  {
    id: 'archive.pack',
    label: 'Pack to archive (ZIP)',
    group: 'File',
    shortcut: { key: 'F5', alt: true },
    available: () => (activePane()?.selectedPaths().length ?? 0) > 0,
    run: () => dialogs.beginPack('zip'),
  },
  {
    id: 'archive.pack7z',
    label: 'Pack to archive (7z)',
    group: 'File',
    available: () => (activePane()?.selectedPaths().length ?? 0) > 0,
    run: () => dialogs.beginPack('sevenz'),
  },
  {
    id: 'archive.unpack',
    label: 'Unpack to other pane',
    group: 'File',
    shortcut: { key: 'F9', alt: true },
    available: () =>
      (activePane()?.selectedPaths().length ?? 0) > 0 && inactivePane() !== undefined,
    run: () => dialogs.beginUnpack(),
  },
  {
    id: 'archive.test',
    label: 'Test archive(s)',
    group: 'File',
    shortcut: { key: 'F9', alt: true, shift: true },
    available: () => (activePane()?.selectedPaths().length ?? 0) > 0,
    run: () => dialogs.beginTestArchive(),
  },
  // ── F2-12 integration: the classic Files-menu entries now ride the same
  // archive engine as their archive.* counterparts (no shortcut of their
  // own — the Alt+F5 / Alt+F9 / Alt+Shift+F9 slots belong to archive.*).
  {
    id: 'file.pack',
    label: 'Pack',
    group: 'File',
    available: () => (activePane()?.selectedPaths().length ?? 0) > 0,
    run: () => dialogs.beginPack('zip'),
  },
  {
    id: 'file.unpack',
    label: 'Unpack Specific Files',
    group: 'File',
    available: () =>
      (activePane()?.selectedPaths().length ?? 0) > 0 && inactivePane() !== undefined,
    run: () => dialogs.beginUnpack(),
  },
  {
    id: 'file.testArchive',
    label: 'Test Archive(s)',
    group: 'File',
    available: () => (activePane()?.selectedPaths().length ?? 0) > 0,
    run: () => dialogs.beginTestArchive(),
  },
  {
    id: 'file.properties',
    label: 'Properties',
    group: 'File',
    shortcut: { key: 'Enter', alt: true },
    available: () => activePane()?.selectedPaths().length === 1,
    run: openProperties,
  },
  {
    id: 'file.attributes',
    label: 'Change Attributes',
    group: 'File',
    available: () => (activePane()?.selectedPaths().length ?? 0) > 0,
    run: openAttributes,
  },
  {
    id: 'file.occupiedSpace',
    label: 'Calculate Occupied Space',
    group: 'File',
    shortcut: ctrl('l'),
    available: () => (activePane()?.selectedPaths().length ?? 0) > 0,
    run: openOccupiedSpace,
  },
  {
    id: 'commands.volumeLabel',
    label: 'Volume Label',
    group: 'Commands',
    available: () => activePane() !== undefined,
    run: openVolumeInfo,
  },
  {
    id: 'commands.systemInfo',
    label: 'System Information',
    group: 'Commands',
    run: openSystemInfo,
  },
  // ── F2-25: Open Terminal Here — structured argv launch (wt fallback
  // cmd on Windows), cwd from the ACTIVE pane, never a shell string.
  {
    id: 'commands.prompt',
    label: 'Open Terminal Here',
    group: 'Commands',
    available: () => activePane() !== undefined,
    run: () => void shell.openTerminalHere(),
  },
  // FER-494: "Buka <Agent> di sini" — one per agent that can open interactively in a terminal
  // (found + ACL passed), at the ACTIVE pane's folder. The folder is the process cwd only.
  ...AGENT_IDS.map((id): Action => ({
    id: `ai.agentTerminal.${id}`,
    label: `Buka ${AGENT_LABELS[id]} di sini`,
    group: 'Commands',
    keywords: [id, AGENT_LABELS[id].toLowerCase(), 'agent', 'terminal ai', 'buka di terminal'],
    available: () => activePane() !== undefined && terminalReady(id),
    run: () => {
      const pane = activePane();
      if (pane) void openAgentTerminal(id, pane.path);
    },
  })),
  {
    id: 'commands.hotlist',
    label: 'Directory Hotlist',
    group: 'Commands',
    shortcut: ctrl('d'),
    available: () => activePane() !== undefined,
    run: () => (hotlist.menuOpen ? hotlist.closeMenu() : hotlist.openMenu(ui.activePane)),
  },
  {
    // FER-484: Total Commander's folder history dropdown. Only while focus is in a pane (or on the
    // page body), never over dialogs or text fields (the key dispatcher already skips those).
    id: 'nav.historyMenu',
    label: 'Folder history dropdown',
    group: 'Navigate',
    shortcut: { key: 'ArrowDown', alt: true },
    available: () =>
      activePane() !== undefined &&
      (document.activeElement === document.body ||
        document.activeElement?.closest('[data-pane]') != null),
    run: () => historyMenu.toggle(ui.activePane),
  },
  {
    id: 'commands.cdTree',
    label: 'CD Tree',
    group: 'Commands',
    shortcut: { key: 'F10', alt: true },
    run: openCdTree,
  },
  {
    id: 'commands.back',
    label: 'Go Back',
    group: 'Commands',
    shortcut: { key: 'ArrowLeft', alt: true },
    available: () => activePane()?.canGoBack === true,
    run: goBack,
  },
  {
    id: 'commands.desktop',
    label: 'Open Desktop Folder',
    group: 'Commands',
    run: () => void openDesktop(),
  },
  {
    id: 'commands.copyFolderPath',
    label: 'Copy Folder Path To Clipboard',
    group: 'Commands',
    shortcut: ctrl('1'),
    available: () => activePane() !== undefined,
    run: () => {
      const pane = activePane();
      void copyWithToast(
        folderCopyText(pane?.path ?? '', pane?.cursorEntry()),
        'path folder',
        'Copy Folder Path',
      );
    },
  },
  {
    id: 'commands.copyItemNames',
    label: 'Copy File Names To Clipboard',
    group: 'Commands',
    shortcut: ctrl('2'),
    available: () => activePane() !== undefined,
    run: () => {
      const items = copyTargets();
      void copyWithToast(namesText(items), `${items.length} nama`, 'Copy Names');
    },
  },
  {
    id: 'commands.copyItemPaths',
    label: 'Copy Full Paths To Clipboard',
    group: 'Commands',
    shortcut: ctrl('3'),
    available: () => activePane() !== undefined,
    run: () => {
      const items = copyTargets();
      void copyWithToast(pathsText(items), `${items.length} path`, 'Copy Paths');
    },
  },
  {
    id: 'commands.sourceTarget',
    label: 'Source↔Target',
    group: 'Commands',
    shortcut: ctrl('u'),
    run: () => {
      swapPanes();
    },
  },
  {
    id: 'commands.openInRightPane',
    label: 'Open in Right Pane',
    group: 'Commands',
    shortcut: ctrl('ArrowRight'),
    available: () => activePane() !== undefined && inactivePane() !== undefined,
    run: () => {
      openInOtherPane(ui.activePane === 0);
    },
  },
  {
    id: 'commands.openInLeftPane',
    label: 'Open in Left Pane',
    group: 'Commands',
    shortcut: ctrl('ArrowLeft'),
    available: () => activePane() !== undefined && inactivePane() !== undefined,
    run: () => {
      openInOtherPane(ui.activePane === 1);
    },
  },
  {
    id: 'commands.targetSource',
    label: 'Target = Source',
    group: 'Commands',
    run: () => {
      targetFromSource();
    },
  },
  {
    id: 'commands.branch',
    label: 'Branch View (With Subdirs)',
    group: 'Commands',
    shortcut: ctrl('b'),
    available: () => activePane() !== undefined,
    run: () => {
      const pane = activePane();
      if (pane) void branch.toggle(ui.activePane, pane.path);
    },
  },
  {
    id: 'file.createChecksum',
    label: 'Create Checksum File(s)',
    group: 'File',
    available: () => (activePane()?.selectedPaths().length ?? 0) > 0,
    run: openChecksumCreate,
  },
  {
    id: 'file.verifyChecksum',
    label: 'Verify Checksums',
    group: 'File',
    available: () => (activePane()?.selectedPaths().length ?? 0) > 0,
    run: openChecksumVerify,
  },
  {
    id: 'file.archiveChecksum',
    label: 'Checksum of Archive Member',
    group: 'File',
    available: () => archiveMemberSelection() !== null,
    run: openArchiveChecksum,
  },
  {
    id: 'file.split',
    label: 'Split File',
    group: 'File',
    available: () => activePane()?.selectedPaths().length === 1,
    run: openSplitFile,
  },
  {
    id: 'file.combine',
    label: 'Combine Files',
    group: 'File',
    available: () => {
      const selected = activePane()?.selectedPaths() ?? [];
      return selected.length === 1 && isCombineCandidate(selected[0]);
    },
    run: openCombineParts,
  },
  {
    id: 'show.refresh',
    label: 'Reread Source',
    group: 'Show',
    shortcut: ctrl('r'),
    available: () => activePane() !== undefined,
    run: () => activePane()?.refresh(),
  },
  {
    id: 'search.open',
    label: 'Search in current folder',
    group: 'Navigate',
    shortcut: { key: 'f', ctrlOrCmd: true },
    run: () => {
      const pane = activePane();
      if (pane) {
        searchStore.root = pane.path;
        searchStore.panelOpen = true;
      }
    },
  },
  {
    id: 'search.close',
    label: 'Close search',
    group: 'Navigate',
    shortcut: { key: 'Escape' },
    available: () => searchStore.panelOpen,
    run: () => {
      searchStore.panelOpen = false;
      void searchStore.cancel();
    },
  },
  {
    // F2-12 merge audit: the incoming lane's select.invert (Shift+*) and
    // select.extension (Shift++) duplicated main's canonical mark.invert /
    // mark.sameExtension and were discarded in favour of the mark.* set.
    // FER-367 put the TC Num-key chords onto those canonical mark.* actions.
    // select.none STAYS: it adds the TC NumDash-parity keyboard deselect the
    // mark.* set lacks, rebound to Shift+Minus because Ctrl+D is the hotlist
    // (1727ae1).
    id: 'select.none',
    label: 'Deselect all',
    group: 'Select',
    // Ctrl+D belongs to the directory hotlist (F2-22); deselect mirrors the
    // TC NumDash convention on the main keyboard instead.
    shortcut: { key: '-', shift: true },
    run: () => activePane()?.clearSelection?.(),
  },
  {
    id: 'view.hidden',
    label: () => (ui.showHidden ? 'Hide hidden files' : 'Show hidden files'),
    group: 'View',
    shortcut: ctrl('h'),
    run: () => ui.toggleHidden(),
  },
  {
    id: 'view.aiChat',
    label: () => (aiChatHost.open ? 'Hide AI assistant' : 'Show AI assistant'),
    group: 'View',
    shortcut: { key: 'a', ctrlOrCmd: true, shift: true },
    run: () => aiChatHost.toggle(),
  },
  {
    id: 'view.aiSettings',
    label: 'AI assistant settings…',
    group: 'View',
    run: () => aiChatHost.openSettings(),
  },
  // FER-486: attach the selection (or the cursor item) to the AI chat to ask about it.
  {
    id: 'ai.addToChat',
    label: 'Add to chat',
    group: 'File',
    shortcut: { key: 'l', ctrlOrCmd: true, shift: true },
    available: () => addToChatTargets().length > 0,
    run: () => void addSelectionToChat(),
  },
  // FER-490: ask a local agent CLI (read-only) about the selection.
  {
    id: 'ai.askAgent',
    label: 'Ask AI agent about selection',
    group: 'File',
    available: () => addToChatTargets().length > 0,
    run: () => void askAgentAboutSelection(),
  },
  {
    id: 'queue.panel',
    label: 'Operations queue',
    group: 'View',
    run: () => {
      void ops.start();
      ops.togglePanel();
    },
  },
  // ── F2-26 Commands: Background Transfer Manager (TC Commands menu) ──────
  // Same panel as queue.panel under its Total Commander name; opens instead
  // of toggling so the shortcut always lands on a visible manager.
  {
    id: 'commands.btm',
    label: 'Background Transfer Manager',
    group: 'Commands',
    shortcut: { key: 'b', alt: true },
    run: () => {
      void ops.start();
      ops.panelOpen = true;
    },
  },
  {
    id: 'theme.toggle',
    label: () =>
      ui.effectiveTheme() === 'dark' ? 'Switch to light theme' : 'Switch to dark theme',
    group: 'View',
    run: () => ui.setTheme(ui.effectiveTheme() === 'dark' ? 'light' : 'dark'),
  },
  {
    id: 'theme.dark',
    label: 'Use dark theme',
    group: 'View',
    available: () => ui.theme !== 'dark',
    run: () => ui.setTheme('dark' as Theme),
  },
  {
    id: 'theme.light',
    label: 'Use light theme',
    group: 'View',
    available: () => ui.theme !== 'light',
    run: () => ui.setTheme('light' as Theme),
  },
  {
    id: 'theme.system',
    label: 'Follow system theme',
    group: 'View',
    available: () => ui.theme !== 'system',
    run: () => ui.setTheme('system' as Theme),
  },
  {
    id: 'favorites.pin',
    label: () =>
      activePane()?.path && activity.isFavorite(activePane()!.path)
        ? 'Unpin folder aktif dari Favorites'
        : 'Pin folder aktif ke Favorites',
    group: 'Favorites',
    shortcut: { key: 'd', ctrlOrCmd: true, shift: true },
    available: () => !!activePane()?.path,
    run: () => {
      const path = activePane()?.path;
      if (path) void activity.toggleFavorite(path, 'folder');
    },
  } as unknown as Action,
  {
    id: 'activity.open',
    label: 'Buka halaman Activity',
    group: 'Favorites',
    run: () => (ui.activityOpen = true),
  },
  {
    id: 'sync.profiles.open',
    label: 'Sync & Backup: Sync profiles',
    group: 'Commands',
    run: () => (ui.syncProfilesOpen = true),
  },
  {
    id: 'palette.open',
    label: 'Command palette',
    group: 'Commands',
    shortcut: ctrl('k'),
    // FER-577 (S3): over any other modal (Properties, Multi-rename, Ops panel, a dialog of the
    // store, ...) the palette would run pane actions (Delete, Move, ...) on the panes behind it.
    available: () => modalDepth() === 0,
    run: () => (ui.paletteOpen = true),
  },
  // ── FER-395: the first opener for the Settings dialog (previously rendered
  // but unreachable); also appears in the Commands menu via menuGroups.
  {
    id: 'app.settings',
    label: 'Settings',
    group: 'Commands',
    shortcut: ctrl(','),
    run: () => (ui.settingsOpen = true),
  },
  // ── F2-18 compare actions ───────────────────────────────────────────────
  {
    id: 'file.compareContent',
    label: 'Compare By Content',
    group: 'Files',
    available: () => compareContentTargets() !== undefined,
    run: () => {
      const targets = compareContentTargets();
      if (targets) void compareStore.compareContents(targets[0], targets[1]);
    },
  },
  {
    id: 'mark.compareDirectories',
    label: 'Compare Directories',
    group: 'Mark',
    shortcut: { key: 'F2', shift: true },
    available: () => activePane() !== undefined && inactivePane() !== undefined,
    // Uses the panel's visible toggles (content / recursive; default off).
    run: () => void compareStore.compareDirectories(compareStore.byContent, compareStore.recursive),
  },
  // ── F2-19 Commands: Synchronize Dirs (mandatory dry-run) ────────────────
  {
    id: 'commands.synchronize',
    label: 'Synchronize Dirs',
    group: 'Commands',
    available: () => activePane() !== undefined && inactivePane() !== undefined,
    run: () => void compareStore.openSync(null),
  },
  // ── F2-14 Mark menu ─────────────────────────────────────────────────────
  {
    id: 'mark.selectGroup',
    label: 'Select Group',
    group: 'Mark',
    available: () => activePane()?.visibleEntries != null,
    run: () => dialogs.beginSelectGroup(false),
  },
  {
    id: 'mark.unselectGroup',
    label: 'Unselect Group',
    group: 'Mark',
    available: () => activePane()?.visibleEntries != null,
    run: () => dialogs.beginSelectGroup(true),
  },
  {
    id: 'mark.selectAll',
    label: 'Select All',
    group: 'Mark',
    available: () => markEntries().length > 0,
    run: () => replacePaneSelection(new Set(markEntries().map((e) => e.path))),
  },
  {
    id: 'mark.unselectAll',
    label: 'Unselect All',
    group: 'Mark',
    available: () => activePane() !== undefined,
    run: () => replacePaneSelection(new Set()),
  },
  {
    // FER-367 (TC Num-key): Shift+Numpad* / Shift+* — `e.key` is '*' from
    // both the top row (Shift+Digit8) and NumpadMultiply (with or without
    // Shift), so `shift: 'any'` covers both physical keys unambiguously.
    id: 'mark.invert',
    label: 'Invert Selection',
    group: 'Mark',
    shortcut: { key: '*', shift: 'any' },
    available: () => activePane()?.visibleEntries != null,
    run: () => replacePaneSelection(invertSelection(markEntries(), new Set(markedPaths()))),
  },
  {
    // FER-367 (TC Num-key): Shift+Numpad+ / Shift++ extends the selection to
    // same-extension entries, matching mark.sameExtension.
    id: 'mark.sameExtension',
    label: 'Select All With Same Extension',
    group: 'Mark',
    shortcut: { key: '+', shift: 'any' },
    available: () => activePane()?.cursorEntry() !== undefined,
    run: () =>
      replacePaneSelection(selectSameExtension(markEntries(), activePane()?.cursorEntry())),
  },
  {
    id: 'mark.save',
    label: 'Save Selection',
    group: 'Mark',
    available: () => markedPaths().length > 0,
    run: () => {
      const pane = activePane();
      if (!pane) return;
      saveSelection(
        pane.path,
        markedPaths().map((p) => baseName(p)),
      );
    },
  },
  {
    id: 'mark.restore',
    label: 'Restore Selection',
    group: 'Mark',
    available: () => activePane()?.visibleEntries != null,
    run: () => {
      const pane = activePane();
      if (!pane) return;
      const names = restoreSelection(pane.path);
      if (!names) {
        dialogs.open({
          kind: 'error',
          title: 'Restore Selection',
          message: 'No saved selection for this folder.',
        });
        return;
      }
      const applied = applySavedSelection(markEntries(), {
        version: 1,
        path: pane.path,
        names,
      });
      replacePaneSelection(new Set(applied.selected));
      if (applied.missing.length > 0)
        dialogs.open({
          kind: 'error',
          title: 'Restore Selection',
          message: `Restored ${applied.selected.length} of ${names.length} saved names; ${applied.missing.length} not found in this folder.`,
        });
    },
  },
  {
    id: 'mark.saveFile',
    label: 'Save Selection To File',
    group: 'Mark',
    available: () => (activePane()?.selectedPaths().length ?? 0) > 0,
    run: () => dialogs.beginSaveSelection(),
  },
  {
    id: 'mark.loadFile',
    label: 'Load Selection From File',
    group: 'Mark',
    available: () => activePane() !== undefined,
    run: () => dialogs.beginLoadSelection(),
  },
  {
    id: 'mark.copyNames',
    label: 'Copy Selected Names To Clipboard',
    group: 'Mark',
    available: () => (activePane()?.selectedPaths().length ?? 0) > 0,
    run: () => void copyToClipboard(formatNames(selectedMarkEntries()), 'Copy Names'),
  },
  {
    id: 'mark.copyPaths',
    label: 'Copy Names With Path To Clipboard',
    group: 'Mark',
    available: () => (activePane()?.selectedPaths().length ?? 0) > 0,
    run: () => void copyToClipboard(formatPaths(selectedMarkEntries()), 'Copy Paths'),
  },
  {
    id: 'mark.copyDetails',
    label: 'Copy To Clipboard With All Details',
    group: 'Mark',
    available: () => (activePane()?.selectedPaths().length ?? 0) > 0,
    run: () => void copyToClipboard(formatDetails(selectedMarkEntries()), 'Copy Details'),
  },
  {
    id: 'mark.copyPathDetails',
    label: 'Copy To Clipboard With Path+Details',
    group: 'Mark',
    available: () => (activePane()?.selectedPaths().length ?? 0) > 0,
    run: () => void copyToClipboard(formatPathDetails(selectedMarkEntries()), 'Copy Path+Details'),
  },
  {
    id: 'mark.newerFiles',
    label: 'Mark Newer, Hide Same Files',
    group: 'Mark',
    available: () => compareStore.report !== null,
    run: () => compareStore.markNewerHideSame(),
  },
  // ── F2-15 Show menu ─────────────────────────────────────────────────────
  // Mode/sort/filter state is per pane (viewStateStore); shortcuts follow
  // the Total Commander scheme on free Ctrl+F1…F8/Ctrl+R slots.
  {
    id: 'show.brief',
    label: 'Brief',
    group: 'Show',
    shortcut: ctrl('F1'),
    available: () => activePane() !== undefined,
    run: () => viewState.setViewMode(ui.activePane, 'brief'),
  },
  {
    id: 'show.full',
    label: 'Full',
    group: 'Show',
    shortcut: ctrl('F2'),
    available: () => activePane() !== undefined,
    run: () => viewState.setViewMode(ui.activePane, 'full'),
  },
  {
    id: 'show.tree',
    label: 'Tree',
    group: 'Show',
    shortcut: ctrl('F8'),
    run: () => {
      ui.sidebarOpen = true;
      ui.sidebarMode = 'tree';
    },
  },
  // ── F2-27: Thumbnail view — TC's Ctrl+Shift+F2 slot; image rows render
  // through the `ferrum-media` protocol, everything else keeps its icon.
  {
    id: 'show.thumbnails',
    label: 'Thumbnail View',
    group: 'Show',
    shortcut: { key: 'F2', ctrlOrCmd: true, shift: true },
    available: () => activePane() !== undefined,
    run: () => viewState.setViewMode(ui.activePane, 'thumbnail'),
  },
  // ── F2-27: Comments view — the listing gains a comment column fed by the
  // per-directory sidecar map (commentStore).
  {
    id: 'show.comments',
    label: () => (ui.showComments ? 'Comments (hide)' : 'Comments'),
    group: 'Show',
    available: () => activePane() !== undefined,
    run: () => {
      ui.showComments = !ui.showComments;
    },
  },
  // ── F2-27: Custom View Modes — named presets of the EXISTING per-pane
  // layout (view mode, sorting, filter preset). Plugin API stays deferred.
  {
    id: 'show.customModes',
    label: 'Custom View Modes',
    group: 'Show',
    available: () => activePane() !== undefined,
    run: () => viewPresets.openDialog(),
  },
  // ── FER-366 (opens the last D33 deferral): Separate Tree — each pane can
  // show its OWN drive-tree panel above its file list (TC per-pane separate
  // tree). Toggles the ACTIVE pane; the label mirrors the show.quickView
  // toggle pattern. Shortcut-free by design: the spec assigns this legacy
  // entry no key, and it stays that way.
  {
    id: 'show.separateTree',
    label: () => (separateTree.isEnabled(ui.activePane) ? 'Hide Separate Tree' : 'Separate Tree'),
    group: 'Show',
    available: () => activePane() !== undefined,
    run: () => separateTree.toggle(ui.activePane),
  },
  // ── FER-365 (opens D33): Custom Columns — configures which of the
  // EXISTING SortKey-able columns Full mode shows, and in what order
  // (columns.ts engine; persisted preference). No new column kinds.
  {
    id: 'show.customColumns',
    label: 'Custom Columns',
    group: 'Show',
    available: () => activePane() !== undefined,
    run: () => columns.openDialog(),
  },
  {
    id: 'show.vertical',
    label: 'Vertical Arrangement',
    group: 'Show',
    run: () => ui.toggleVerticalLayout(),
  },
  {
    id: 'show.allFiles',
    label: 'All Files',
    group: 'Show',
    available: () => activePane() !== undefined,
    run: () => {
      viewState.setFilterMode(ui.activePane, 'all');
      compareStore.clearHiddenSame(); // leaving presets also clears Hide Same (F2-18)
    },
  },
  {
    id: 'show.programs',
    label: 'Programs',
    group: 'Show',
    available: () => activePane() !== undefined,
    run: () => viewState.setFilterMode(ui.activePane, 'programs'),
  },
  {
    id: 'show.filter',
    label: 'Custom Filter',
    group: 'Show',
    available: () => activePane() !== undefined,
    run: () => dialogs.beginShowFilter(),
  },
  {
    id: 'show.selectedOnly',
    label: () =>
      viewState.filterMode(ui.activePane) === 'selected'
        ? 'Show All Files (leave Only Selected)'
        : 'Only Selected Files',
    group: 'Show',
    available: () => activePane() !== undefined,
    run: () =>
      viewState.filterMode(ui.activePane) === 'selected'
        ? viewState.setFilterMode(ui.activePane, 'all')
        : viewState.setFilterMode(ui.activePane, 'selected'),
  },
  {
    id: 'show.sortName',
    label: 'Sort by Name',
    group: 'Show',
    shortcut: ctrl('F3'),
    available: () => activePane() !== undefined,
    run: () => viewState.setSort(ui.activePane, 'name'),
  },
  {
    id: 'show.sortExtension',
    label: 'Sort by Extension',
    group: 'Show',
    shortcut: ctrl('F4'),
    available: () => activePane() !== undefined,
    run: () => viewState.setSort(ui.activePane, 'extension'),
  },
  {
    id: 'show.sortTime',
    label: 'Sort by Time',
    group: 'Show',
    shortcut: ctrl('F5'),
    available: () => activePane() !== undefined,
    run: () => viewState.setSort(ui.activePane, 'modified'),
  },
  {
    id: 'show.sortSize',
    label: 'Sort by Size',
    group: 'Show',
    shortcut: ctrl('F6'),
    available: () => activePane() !== undefined,
    run: () => viewState.setSort(ui.activePane, 'size'),
  },
  {
    id: 'show.unsorted',
    label: 'Unsorted',
    group: 'Show',
    shortcut: ctrl('F7'),
    available: () => activePane() !== undefined,
    run: () => viewState.setSort(ui.activePane, 'unsorted'),
  },
  {
    id: 'show.reverse',
    label: () => (viewState.reversed(ui.activePane) ? 'Normal Order' : 'Reversed Order'),
    group: 'Show',
    available: () => activePane() !== undefined,
    run: () => viewState.toggleReversed(ui.activePane),
  },
  // ── F2-16 Commands: Search (Alt+F7, TC slot) ────────────────────────────
  {
    id: 'commands.search',
    label: 'Search',
    group: 'Commands',
    shortcut: { key: 'F7', alt: true },
    run: () => searchStore.togglePanel(),
  },
  // ── DSK-04 (FER-322) / DSK-10 (FER-408): Disk usage — the full TWO-PANEL
  // workspace: left panel scans the left pane's folder, right the right's,
  // both at the same time; Exit (or Alt+Shift+D again) returns to the dual-pane
  // Files and keeps both sides' results. Alt+Shift+D (Alt+D is the
  // address bar, FER-456) sits beside Alt+F7 (Search) / Alt+B (BTM).
  {
    id: 'commands.diskUsage',
    label: () => (duWorkspace.open ? 'Exit Disk Usage (back to Files)' : 'Disk Usage'),
    group: 'Commands',
    shortcut: { key: 'd', alt: true, shift: true },
    available: () => activePane() !== undefined,
    run: () => duWorkspace.toggle(),
  },
  // ── F2-27: Search in a separate window (TC "Search separate") — its own
  // WebviewWindow running the same search store; the main window listens
  // for its goto events.
  {
    id: 'commands.searchSeparate',
    label: 'Search in Separate Window',
    group: 'Commands',
    available: () => activePane() !== undefined,
    run: () => {
      const pane = activePane();
      openSearchSeparate(pane?.path).catch((e: unknown) => {
        dialogs.open({
          kind: 'error',
          title: 'Search in Separate Window',
          message: e instanceof Error ? e.message : String(e),
        });
      });
    },
  },
  // ── F2-17 Show: Quick View (Ctrl+Q, TC slot) ────────────────────────────
  {
    id: 'show.quickView',
    label: () => (ui.quickViewOpen ? 'Close Quick View Panel' : 'Quick View Panel'),
    group: 'Show',
    shortcut: ctrl('q'),
    run: () => {
      ui.quickViewOpen = !ui.quickViewOpen;
      if (!ui.quickViewOpen) previewStore.setTarget(null);
    },
  },
];

/** Separator entry inside a menu group's `ids` (FER-429). */
export const MENU_SEPARATOR = '-';

// FER-429 (owner chose option 2): View / Sort / Show split the old 29-item Show menu so no
// menu needs scrolling at 1440x900. Theme actions live on the titlebar icon and AI provider
// settings in Settings > AI; both stay in the registry, so Ctrl+K still finds them.
export const menuGroups: {
  label: 'Files' | 'Mark' | 'Commands' | 'View' | 'Sort' | 'Show';
  ids: string[];
}[] = [
  {
    label: 'Files',
    // FER-431: 27 flat items needed a scroll at 768px. Rarely used actions moved into
    // submenus (see menuSubmenus); everything stays in the registry and in Ctrl+K.
    ids: [
      'file.open',
      'file.rename',
      'file.copy',
      'file.move',
      'file.mkdir',
      'file.delete',
      'file.delete.permanent',
      MENU_SEPARATOR,
      'submenu:archive',
      'submenu:checksum',
      'submenu:splitEncode',
      MENU_SEPARATOR,
      'file.compareContent',
      'file.multiRename',
      'file.print',
      'submenu:attributes',
      MENU_SEPARATOR,
      'tab.close',
      'file.quit',
    ],
  },
  {
    label: 'Mark',
    ids: [
      'mark.selectGroup',
      'mark.unselectGroup',
      'mark.selectAll',
      'mark.unselectAll',
      'mark.invert',
      'mark.sameExtension',
      'mark.save',
      'mark.restore',
      'mark.saveFile',
      'mark.loadFile',
      // FER-572: the Files menu is full (768px rule), so the file clipboard sits beside Copy Names.
      'file.clipCopy',
      'file.clipCut',
      'file.clipPaste',
      'mark.copyNames',
      'mark.copyPaths',
      'mark.copyDetails',
      'mark.copyPathDetails',
      'mark.compareDirectories',
      'mark.newerFiles',
    ],
  },
  {
    label: 'Commands',
    ids: [
      'commands.cdTree',
      'commands.search',
      'commands.searchSeparate',
      'commands.diskUsage',
      'commands.volumeLabel',
      'commands.systemInfo',
      'commands.synchronize',
      'commands.hotlist',
      'commands.back',
      'nav.historyMenu',
      'commands.prompt',
      'commands.branch',
      'commands.desktop',
      'commands.btm',
      'commands.copyFolderPath',
      'commands.copyItemNames',
      'commands.copyItemPaths',
      'commands.sourceTarget',
      'commands.targetSource',
      'nav.up',
      'nav.root',
      'pane.switch',
      'palette.open',
      'app.settings',
    ],
  },
  {
    label: 'View',
    ids: [
      'show.brief',
      'show.full',
      'show.thumbnails',
      'show.comments',
      'show.customColumns',
      'show.customModes',
      MENU_SEPARATOR,
      'show.tree',
      'show.separateTree',
      'show.quickView',
      'show.vertical',
      MENU_SEPARATOR,
      'view.hidden',
      'show.refresh',
    ],
  },
  {
    label: 'Sort',
    ids: [
      'show.sortName',
      'show.sortExtension',
      'show.sortTime',
      'show.sortSize',
      'show.unsorted',
      MENU_SEPARATOR,
      'show.reverse',
    ],
  },
  {
    label: 'Show',
    ids: [
      'show.allFiles',
      'show.programs',
      'show.filter',
      'show.selectedOnly',
      MENU_SEPARATOR,
      'tab.new',
      MENU_SEPARATOR,
      'view.aiChat',
    ],
  },
];

/**
 * FER-431: submenus referenced from `menuGroups` by an entry key (`submenu:<name>`).
 * Entries are action ids; submenus do not nest.
 */
export const menuSubmenus: Record<string, { label: string; ids: string[] }> = {
  'submenu:archive': {
    label: 'Archive',
    ids: ['file.pack', 'file.unpack', 'file.testArchive'],
  },
  'submenu:checksum': {
    label: 'Checksums',
    ids: ['file.createChecksum', 'file.verifyChecksum', 'file.archiveChecksum'],
  },
  'submenu:splitEncode': {
    label: 'Split & encode',
    ids: ['file.split', 'file.combine', 'file.encode', 'file.decode'],
  },
  'submenu:attributes': {
    label: 'Attributes & properties',
    ids: [
      'file.attributes',
      'file.properties',
      'file.occupiedSpace',
      'file.associate',
      'file.comment',
    ],
  },
};

const actionsById: Record<string, Action> = Object.fromEntries(
  actions.map((action) => [action.id, action]),
);

export function getAction(id: string): Action | undefined {
  return Object.hasOwn(actionsById, id) ? actionsById[id] : undefined;
}

export function isEnabled(action: Action): boolean {
  return !!action.run && !action.disabledReason && (action.available?.() ?? true);
}

/**
 * FER-561: the only actions whose keyboard shortcut still runs while a full page (Activity) covers
 * the panes. Everything else touches files or the hidden active pane and is blocked.
 */
export const FULL_PAGE_SHORTCUT_IDS: ReadonlySet<string> = new Set([
  'palette.open',
  'app.settings',
  'activity.open',
  'sync.profiles.open',
]);

/** True when a full page is open and this action's shortcut must not run. */
export function shortcutBlockedByFullPage(action: Action): boolean {
  return fullPageOpen() && !FULL_PAGE_SHORTCUT_IDS.has(action.id);
}

/**
 * FER-577 (X1): while a modal is open (`use:dialog` counts them) the panes behind it are
 * unreachable, so only these shortcuts may still run; F5, F8, Delete, F7, Alt+... must not.
 */
export const MODAL_SHORTCUT_IDS: ReadonlySet<string> = new Set(['app.settings']);

/** True when a modal is open and this action's shortcut must not run. */
export function shortcutBlockedByModal(action: Action): boolean {
  return modalDepth() > 0 && !MODAL_SHORTCUT_IDS.has(action.id);
}

export function runAction(action: Action): boolean {
  if (!isEnabled(action)) return false;
  action.run!();
  recentCommands.record(action.id); // FER-580
  return true;
}
/** Cmd+H belongs to macOS Hide Application, not Ferrum. */
export function shortcutFor(
  action: Action,
  platform = globalThis.navigator?.platform ?? '',
): Shortcut | undefined {
  if (action.id === 'view.hidden' && /mac|darwin|iphone|ipad/i.test(platform)) return undefined;
  return action.shortcut;
}

export function shortcutLabel(
  shortcut: Shortcut,
  platform = globalThis.navigator?.platform ?? '',
): string {
  const parts: string[] = [];
  if (shortcut.ctrlOrCmd) parts.push(/mac|darwin|iphone|ipad/i.test(platform) ? 'Cmd' : 'Ctrl');
  if (shortcut.alt) parts.push('Alt');
  if (shortcut.shift) parts.push('Shift');
  parts.push(shortcut.key.length === 1 ? shortcut.key.toUpperCase() : shortcut.key);
  return parts.join('+');
}

/** Resolve label (static or dynamic). */
export function labelOf(a: Action): string {
  return typeof a.label === 'function' ? a.label() : a.label;
}

/** Match complete modifier state, never allowing an extra modifier to hijack
 *  an action. `shift: 'any'` is the one deliberate exception (FER-367): the
 *  key itself already disambiguates the TC Num-key chords. */
export function matches(
  shortcut: Shortcut,
  e: { key: string; ctrlKey: boolean; metaKey: boolean; shiftKey: boolean; altKey?: boolean },
  platform = globalThis.navigator?.platform ?? '',
): boolean {
  if (e.key.toLowerCase() !== shortcut.key.toLowerCase()) return false;
  const mac = /mac|darwin|iphone|ipad/i.test(platform);
  if (shortcut.ctrlOrCmd) {
    if (mac ? !e.metaKey || e.ctrlKey : !e.ctrlKey || e.metaKey) return false;
  } else if (e.ctrlKey || e.metaKey) return false;
  return (
    (shortcut.shift === 'any' || e.shiftKey === (shortcut.shift ?? false)) &&
    (e.altKey ?? false) === (shortcut.alt ?? false)
  );
}

/** Return one available registry action for a key event. */
export function actionForEvent(
  e: { key: string; ctrlKey: boolean; metaKey: boolean; shiftKey: boolean; altKey?: boolean },
  list: Action[] = actions,
  platform = globalThis.navigator?.platform ?? '',
): Action | undefined {
  return list.find((action) => {
    const shortcut = shortcutFor(action, platform);
    return shortcut && matches(shortcut, e, platform) && isEnabled(action);
  });
}

/**
 * Palette search aliases by action id (FER-485), kept apart from the registry so the list can grow
 * without touching every action. An action's own `keywords` field wins when present.
 */
const ACTION_KEYWORDS: Record<string, string[]> = {
  'file.copy': ['salin', 'duplikat', 'copy'],
  'file.move': ['pindah', 'pindahkan', 'move'],
  'file.delete': ['hapus', 'buang', 'delete', 'recycle bin'],
  'file.delete.permanent': ['hapus permanen', 'delete'],
  'file.rename': ['ganti nama', 'rename'],
  'file.mkdir': ['folder baru', 'buat folder', 'new folder', 'mkdir'],
  'search.open': ['cari', 'search', 'find'],
  'nav.root': ['root', 'akar', 'drive'],
  'commands.search': ['cari', 'search', 'find'],
  'commands.searchSeparate': ['cari', 'search', 'find'],
  'file.compareContent': ['bandingkan', 'compare', 'diff'],
  'mark.compareDirectories': ['bandingkan', 'compare'],
  'commands.synchronize': ['sinkron', 'sinkronisasi', 'sync'],
  'commands.diskUsage': ['ukuran', 'disk usage', 'ruang disk'],
  'file.occupiedSpace': ['ukuran', 'disk usage'],
  'archive.pack': ['arsip', 'zip', 'kompres', 'compress'],
  'archive.pack7z': ['arsip', '7zip', 'kompres', 'compress'],
  'archive.unpack': ['ekstrak', 'extract', 'unzip', 'buka arsip'],
  'file.unpack': ['ekstrak', 'extract', 'unzip'],
  'file.properties': ['properti', 'info'],
  'commands.prompt': ['terminal', 'cmd', 'powershell'],
  'app.settings': ['pengaturan', 'opsi', 'options', 'preferences'],
  'theme.dark': ['gelap', 'dark mode'],
  'theme.light': ['terang', 'light mode'],
  'view.hidden': ['sembunyi', 'tersembunyi'],
  'select.none': ['batal pilih', 'unselect'],
  'mark.selectAll': ['pilih semua'],
  // FER-486
  'ai.addToChat': ['tanya ai', 'lampirkan', 'lampiran', 'kirim ke chat', 'ask ai', 'attach'],
  'ai.askAgent': [
    'tanya agent',
    'agent cli',
    'claude',
    'claude code',
    'codex',
    'tanya lampiran',
    'baca pdf',
  ],
};

/**
 * Palette search: typo-tolerant and ranked (exact, prefix, word start, substring, initialism,
 * subsequence, typo). Matches the label first and `keywords` as aliases. Available actions only.
 */
export function searchActions(query: string, list: Action[] = actions): Action[] {
  const pool = list.filter(isEnabled);
  return rankMatches(pool, query, (a) => {
    const fields: MatchField[] = [{ text: labelOf(a) }];
    for (const k of a.keywords ?? ACTION_KEYWORDS[a.id] ?? []) {
      fields.push({ text: k, weight: 0.9 });
    }
    return fields;
  });
}
