import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render } from '@testing-library/svelte';
import { tick } from 'svelte';
import FileList from './FileList.svelte';
import { toRow, type Row } from '../stores/paneStore.svelte';
import type { FileEntry } from '../ipc/types';
import { FsError } from '../ipc/types';
import { registerPane, type PaneApi } from '../stores/panesRegistry.svelte';
import { dialogs } from '../stores/dialogStore.svelte';
import { contextMenu } from '../stores/contextMenuStore.svelte';
import {
  columns as columnsStore,
  loadColumnConfig,
  loadColumnWidths,
} from '../stores/columnsStore.svelte';
import { ui } from '../uiState.svelte';
import { prefs } from '../stores/prefsStore.svelte';

vi.mock('../ipc/mutations', () => ({
  createEntry: vi.fn(),
  renameEntry: vi.fn(),
  deleteEntries: vi.fn(),
  planCopy: vi.fn(),
  enqueueCopy: vi.fn(),
  enqueueMove: vi.fn(),
}));
const { renameEntry } = await import('../ipc/mutations');

const N = 10_000;

function synthetic(n: number): Row[] {
  const out: Row[] = [];
  for (let i = 0; i < n; i++) {
    const e: FileEntry = {
      name: `bench-${String(i).padStart(5, '0')}.txt`,
      path: `C:\\bench\\${i}`,
      isDir: false,
      isSymlink: false,
      size: i,
      modifiedMs: 1_790_512_000_000,
      hidden: false,
    };
    out.push(toRow(e));
  }
  return out;
}

function firstDataRow(container: HTMLElement): string {
  const rows = container.querySelectorAll('.row[data-path]');
  expect(rows.length).toBeGreaterThan(0);
  return rows[0].querySelector('.nameText')?.textContent ?? '';
}

describe('FileList virtualization (10k rows, viewport 520px)', () => {
  it('renders only a viewport-sized window of rows, not the dataset', () => {
    const { container } = render(FileList, {
      rows: synthetic(N),
      viewportOverride: 520,
    });
    const domRows = container.querySelectorAll('.row').length;
    // 20 visible + up to 16 overscan + 1 ".." row
    expect(domRows).toBeGreaterThan(10);
    expect(domRows).toBeLessThanOrEqual(37);
    expect(domRows).toBeLessThan(N / 100);
  });

  it('moves the window on scroll without losing rows around the viewport', async () => {
    const rows = synthetic(N);
    const { container } = render(FileList, { rows, viewportOverride: 520 });
    const body = container.querySelector('.body') as HTMLElement;
    expect(firstDataRow(container)).toBe('bench-00000.txt');

    body.scrollTop = 26 * 5000;
    body.dispatchEvent(new Event('scroll'));
    await tick();
    expect(firstDataRow(container)).toMatch(/bench-049(8|9)\d/);
    expect(container.querySelectorAll('.row').length).toBeLessThanOrEqual(37);

    body.scrollTop = 26 * (N - 2);
    body.dispatchEvent(new Event('scroll'));
    await tick();
    expect(firstDataRow(container)).toMatch(/bench-099(8|9)\d/);
  });

  function rowByPath(container: HTMLElement, path: string): Element | null {
    // backslashes in paths break CSS attribute selectors; match via DOM instead
    return (
      Array.from(container.querySelectorAll('.row[data-path]')).find(
        (el) => el.getAttribute('data-path') === path,
      ) ?? null
    );
  }

  it('scrolls a far-away cursor row into view', async () => {
    const rows = synthetic(N);
    const { container } = render(FileList, {
      rows,
      viewportOverride: 520,
      cursor: 'C:\\bench\\9000',
    });
    await tick();
    const body = container.querySelector('.body') as HTMLElement;
    expect(body.scrollTop).toBeGreaterThan(26 * 8900);
    // Cursor row is now inside the rendered window
    expect(rowByPath(container, 'C:\\bench\\9000')).not.toBeNull();
  });

  it('keeps cursor visible when it moves up after deep scrolling', async () => {
    const rows = synthetic(N);
    const { container } = render(FileList, { rows, viewportOverride: 520 });
    const body = container.querySelector('.body') as HTMLElement;
    body.scrollTop = 26 * 9000;
    body.dispatchEvent(new Event('scroll'));
    await tick();

    const withCursor = render(FileList, {
      rows,
      viewportOverride: 520,
      cursor: 'C:\\bench\\5',
    });
    await tick();
    const body2 = withCursor.container.querySelector('.body') as HTMLElement;
    expect(body2.scrollTop).toBeLessThan(26 * 10);
    expect(rowByPath(withCursor.container, 'C:\\bench\\5')).not.toBeNull();
  });
});

// ── Context menu + inline rename ──────────────────────────────────────────

const fileRow = toRow({
  name: 'notes.txt',
  path: 'C:\\bench\\notes.txt',
  isDir: false,
  isSymlink: false,
  size: 12,
  modifiedMs: 1_790_512_000_000,
  hidden: false,
});
const dirRow = toRow({
  name: 'reports',
  path: 'C:\\bench\\reports',
  isDir: true,
  isSymlink: false,
  size: 0,
  modifiedMs: 1_790_512_000_000,
  hidden: false,
});
const twoRows = [fileRow, dirRow];

let paneOff: (() => void) | undefined;

function registerFakePane(over: Partial<PaneApi> = {}): PaneApi {
  const api: PaneApi = {
    path: 'C:\\bench',
    selectedPaths: () => [],
    cursorEntry: () => undefined,
    refresh: () => {},
    newTab: () => {},
    closeCurrentTab: () => {},
    goUp: () => {},
    openCursorEntry: () => {},
    ...over,
  };
  paneOff = registerPane(0, api);
  ui.activePane = 0;
  return api;
}

afterEach(() => {
  cleanup();
  paneOff?.();
  paneOff = undefined;
  contextMenu.close();
  dialogs.close();
  vi.mocked(renameEntry).mockReset();
  ui.activePane = 0;
});

function rowByPath(container: HTMLElement, path: string): HTMLElement {
  const el = Array.from(container.querySelectorAll('.row[data-path]')).find(
    (x) => x.getAttribute('data-path') === path,
  );
  expect(el, `row "${path}"`).toBeTruthy();
  return el as HTMLElement;
}

function menuItemLabels(): string[] {
  return Array.from(document.querySelectorAll('[role="menuitem"]')).map(
    (b) => b.querySelector('.label')?.textContent ?? '',
  );
}

function menuItem(label: string): HTMLButtonElement {
  const el = Array.from(document.querySelectorAll('[role="menuitem"]')).find((b) =>
    b.textContent?.includes(label),
  );
  expect(el, `menu item "${label}"`).toBeTruthy();
  return el as HTMLButtonElement;
}

describe('row context menu', () => {
  beforeEach(() =>
    registerFakePane({
      selectedPaths: () => ['C:\\bench\\notes.txt'],
      cursorEntry: () => ({ name: 'notes.txt', path: 'C:\\bench\\notes.txt', isDir: false }),
    }),
  );
  it('FER-577 S2: with confirmBeforeDeleting off, Menu key + the Delete letter only focuses Delete', async () => {
    prefs.fileOperations.confirmBeforeDeleting = false;
    const { container } = render(FileList, {
      rows: twoRows,
      viewportOverride: 520,
      cursor: 'C:\\bench\\notes.txt',
    });
    const body = container.querySelector('.body') as HTMLElement;
    body.focus();
    fireEvent.keyDown(body, { key: 'ContextMenu' });
    await tick();
    const del = Array.from(document.querySelectorAll<HTMLElement>('[role="menuitem"]')).find(
      (b) => b.dataset.danger !== undefined && /^Delete/.test(b.textContent?.trim() ?? ''),
    )!;
    expect(del).toBeTruthy();
    fireEvent.keyDown(document.activeElement!, { key: del.dataset.mnemonic! });
    await tick();
    expect(document.activeElement).toBe(del);
    expect(contextMenu.isOpen).toBe(true);
    expect(dialogs.current).toBeNull();
    prefs.fileOperations.confirmBeforeDeleting = true;
  });

  it('FER-577: Menu key / Shift+F10 open the menu for the cursor row, anchored to that row', async () => {
    const onlistkey = vi.fn();
    const { container } = render(FileList, {
      rows: twoRows,
      viewportOverride: 520,
      cursor: 'C:\\bench\\notes.txt',
      onlistkey,
    });
    const body = container.querySelector('.body') as HTMLElement;
    body.focus();
    const ev = new KeyboardEvent('keydown', {
      key: 'F10',
      shiftKey: true,
      bubbles: true,
      cancelable: true,
    });
    body.dispatchEvent(ev);
    await tick();
    expect(ev.defaultPrevented).toBe(true);
    expect(contextMenu.isOpen).toBe(true);
    expect(onlistkey).not.toHaveBeenCalled(); // the pane never sees a menu key
    contextMenu.close();
    fireEvent.keyDown(body, { key: 'ContextMenu' });
    await tick();
    expect(contextMenu.isOpen).toBe(true);
    contextMenu.close();
    fireEvent.keyDown(body, { key: 'ArrowDown' });
    expect(onlistkey).toHaveBeenCalledTimes(1);
  });

  it('right-click on an unselected row selects it first, then opens the standard menu', async () => {
    const onrowclick = vi.fn();
    const { container } = render(FileList, {
      rows: twoRows,
      viewportOverride: 520,
      onrowclick,
    });
    fireEvent.contextMenu(rowByPath(container, 'C:\\bench\\notes.txt'));
    await tick();
    expect(onrowclick).toHaveBeenCalledTimes(1);
    // F2-25: a file cursor offers the OS open plus the Associate With picker.
    expect(menuItemLabels()).toEqual([
      'Open with associated application',
      'Associate With…',
      'Edit Comment',
      'Add to chat',
      'Copy to other pane',
      'Move to other pane',
      'Rename',
      'Rename inline',
      'Delete (Recycle Bin)',
      'Delete permanently',
      'New folder',
    ]);
    const menu = document.querySelector('[role="menu"]') as HTMLElement;
    for (const kbd of ['F5', 'F6', 'F2', 'F8', 'F7']) {
      expect(menu.textContent).toContain(kbd);
    }
  });

  it('shows Open for a directory cursor entry and runs it; no re-select for already-selected rows', async () => {
    const openCursorEntry = vi.fn();
    const onrowclick = vi.fn();
    registerFakePane({
      cursorEntry: () => ({ name: 'reports', path: 'C:\\bench\\reports', isDir: true }),
      openCursorEntry,
    });
    const { container } = render(FileList, {
      rows: twoRows,
      viewportOverride: 520,
      selected: new Set(['C:\\bench\\reports']),
      onrowclick,
    });
    fireEvent.contextMenu(rowByPath(container, 'C:\\bench\\reports'));
    await tick();
    expect(onrowclick).not.toHaveBeenCalled();
    expect(menuItemLabels()).toContain('Open selected folder');
    fireEvent.click(menuItem('Open'));
    await tick();
    expect(openCursorEntry).toHaveBeenCalledTimes(1);
    expect(contextMenu.isOpen).toBe(false);
  });

  it('hides Rename when the selection holds more than one entry', async () => {
    registerFakePane({ selectedPaths: () => ['C:\\bench\\notes.txt', 'C:\\bench\\reports'] });
    const { container } = render(FileList, {
      rows: twoRows,
      viewportOverride: 520,
      selected: new Set(['C:\\bench\\notes.txt', 'C:\\bench\\reports']),
    });
    fireEvent.contextMenu(rowByPath(container, 'C:\\bench\\notes.txt'));
    await tick();
    expect(menuItemLabels()).not.toContain('Rename');
    expect(menuItemLabels()).not.toContain('Rename inline');
    expect(menuItemLabels()).toContain('Delete (Recycle Bin)');
  });

  it('closes on Escape and on a mousedown outside the menu', async () => {
    const { container } = render(FileList, { rows: twoRows, viewportOverride: 520 });
    fireEvent.contextMenu(rowByPath(container, 'C:\\bench\\notes.txt'));
    await tick();
    expect(contextMenu.isOpen).toBe(true);
    fireEvent.keyDown(window, { key: 'Escape' });
    await tick();
    expect(contextMenu.isOpen).toBe(false);
    expect(document.querySelector('[role="menu"]')).toBeNull();

    fireEvent.contextMenu(rowByPath(container, 'C:\\bench\\notes.txt'));
    await tick();
    fireEvent.mouseDown(document.body);
    await tick();
    expect(contextMenu.isOpen).toBe(false);
  });

  it('closes when the list body scrolls', async () => {
    const { container } = render(FileList, { rows: twoRows, viewportOverride: 520 });
    fireEvent.contextMenu(rowByPath(container, 'C:\\bench\\notes.txt'));
    await tick();
    const body = container.querySelector('.body') as HTMLElement;
    body.scrollTop = 10;
    body.dispatchEvent(new Event('scroll'));
    await tick();
    expect(contextMenu.isOpen).toBe(false);
  });

  it('menu items drive the mutation dialogs (New folder, Delete)', async () => {
    registerFakePane({ selectedPaths: () => ['C:\\bench\\notes.txt'] });
    const { container } = render(FileList, {
      rows: twoRows,
      viewportOverride: 520,
      selected: new Set(['C:\\bench\\notes.txt']),
    });
    fireEvent.contextMenu(rowByPath(container, 'C:\\bench\\notes.txt'));
    await tick();
    fireEvent.click(menuItem('New folder'));
    await tick();
    expect(dialogs.current).toEqual({ kind: 'mkdir', parent: 'C:\\bench' });
    dialogs.close();

    fireEvent.contextMenu(rowByPath(container, 'C:\\bench\\notes.txt'));
    fireEvent.click(menuItem('Delete (Recycle Bin)'));
    await tick();
    expect(dialogs.current?.kind).toBe('delete');
  });
});

describe('custom columns rendering (FER-365)', () => {
  const configured = twoRows; // notes.txt (file) + reports (dir)

  it('default config reproduces the built-in Name/Size/Modified grid', () => {
    const { container } = render(FileList, { rows: configured, viewportOverride: 520 });
    expect(container.querySelector('.hName')).toBeTruthy();
    expect(container.querySelector('.hSize')).toBeTruthy();
    expect(container.querySelector('.hMod')).toBeTruthy();
    expect(container.querySelector('.hExt')).toBeNull();
    expect(container.querySelector('.ext')).toBeNull();
  });

  it('renders the configured columns, in order, with extension cell text', async () => {
    const onsort = vi.fn();
    const { container } = render(FileList, {
      rows: configured,
      viewportOverride: 520,
      columns: ['name', 'extension', 'modified'],
      onsort,
    });
    const labels = Array.from(container.querySelectorAll('.head > *')).map((el) =>
      el.textContent?.trim(),
    );
    expect(labels).toEqual(['Name ↑', 'Ext', 'Modified']);
    expect(container.querySelector('.hSize')).toBeNull(); // hidden column
    const fileRow = rowByPath(container, 'C:\\bench\\notes.txt');
    expect(fileRow.querySelector('.ext')?.textContent).toBe('txt');
    expect(fileRow.querySelector('.size')).toBeNull();
    // Directories show no extension.
    const dirRow = rowByPath(container, 'C:\\bench\\reports');
    expect(dirRow.querySelector('.ext')?.textContent).toBe('');
    // The extension header sorts by extension.
    fireEvent.click(container.querySelector('.hExt') as HTMLElement);
    expect(onsort).toHaveBeenCalledWith('extension');
  });

  it('reorders visible columns and keeps the up row cells aligned', () => {
    const { container } = render(FileList, {
      rows: configured,
      viewportOverride: 520,
      columns: ['name', 'modified', 'size'],
    });
    const headClasses = Array.from(container.querySelectorAll('.head > *')).map(
      (el) => Array.from(el.classList).find((c) => c.startsWith('h')) ?? '',
    );
    expect(headClasses).toEqual(['hName', 'hMod', 'hSize']);
    const upRow = container.querySelector('.upRow') as HTMLElement;
    expect(upRow.style.gridTemplateColumns).toBe('minmax(0, 1fr) 132px 72px');
    expect(upRow.querySelectorAll('.size')[0]?.textContent).toBe('—');
  });

  it('appends the Comments column after the configured columns', () => {
    const { container } = render(FileList, {
      rows: configured,
      viewportOverride: 520,
      columns: ['name', 'size'],
      showComments: true,
      comments: { 'notes.txt': 'hello' },
    });
    const headCells = Array.from(container.querySelectorAll('.head > *')).map((el) =>
      el.textContent?.trim(),
    );
    expect(headCells).toEqual(['Name ↑', 'Size', 'Comment']);
    const fileRow = rowByPath(container, 'C:\\bench\\notes.txt');
    expect(fileRow.querySelector('.comment')?.textContent).toBe('hello');
  });
});

describe('inline rename', () => {
  interface Harness {
    container: HTMLElement;
    refresh: ReturnType<typeof vi.fn>;
  }

  /** Opens the context menu on notes.txt and runs its Rename item. */
  async function startRename(): Promise<Harness> {
    const refresh = vi.fn();
    registerFakePane({ refresh });
    const { container } = render(FileList, {
      rows: twoRows,
      viewportOverride: 520,
      selected: new Set(['C:\\bench\\notes.txt']),
    });
    fireEvent.contextMenu(rowByPath(container, 'C:\\bench\\notes.txt'));
    await tick();
    fireEvent.click(menuItem('Rename inline'));
    await tick();
    return { container, refresh };
  }

  it('starts inline editing with the basename selected and focused', async () => {
    await startRename();
    const input = document.querySelector('.renameInput') as HTMLInputElement;
    expect(input).toBeTruthy();
    expect(input.value).toBe('notes.txt');
    expect(document.activeElement).toBe(input);
    // Basename only: everything before the last dot.
    expect(input.selectionStart).toBe(0);
    expect(input.selectionEnd).toBe(5);
  });

  it('folders rename without a text selection', async () => {
    registerFakePane();
    const { container } = render(FileList, {
      rows: twoRows,
      viewportOverride: 520,
      selected: new Set(['C:\\bench\\reports']),
    });
    fireEvent.contextMenu(rowByPath(container, 'C:\\bench\\reports'));
    await tick();
    fireEvent.click(menuItem('Rename inline'));
    await tick();
    const input = container.querySelector('.renameInput') as HTMLInputElement;
    expect(input.value).toBe('reports');
    expect(input.selectionStart).toBe(input.selectionEnd);
  });

  it('Enter commits via renameEntry and refreshes the pane', async () => {
    const { refresh } = await startRename();
    const input = document.querySelector('.renameInput') as HTMLInputElement;
    fireEvent.input(input, { target: { value: 'notes-2026.txt' } });
    fireEvent.keyDown(input, { key: 'Enter' });
    await vi.waitFor(() =>
      expect(renameEntry).toHaveBeenCalledWith('C:\\bench\\notes.txt', 'notes-2026.txt'),
    );
    await vi.waitFor(() => expect(refresh).toHaveBeenCalledTimes(1));
    expect(document.querySelector('.renameInput')).toBeNull();
  });

  it('blur commits as well', async () => {
    await startRename();
    const input = document.querySelector('.renameInput') as HTMLInputElement;
    fireEvent.input(input, { target: { value: 'renamed-by-blur.txt' } });
    fireEvent.blur(input);
    await vi.waitFor(() =>
      expect(renameEntry).toHaveBeenCalledWith('C:\\bench\\notes.txt', 'renamed-by-blur.txt'),
    );
  });

  it('Escape cancels without calling IPC', async () => {
    await startRename();
    const input = document.querySelector('.renameInput') as HTMLInputElement;
    fireEvent.input(input, { target: { value: 'discarded.txt' } });
    fireEvent.keyDown(input, { key: 'Escape' });
    await tick();
    expect(renameEntry).not.toHaveBeenCalled();
    expect(document.querySelector('.renameInput')).toBeNull();
  });

  it('empty or unchanged names cancel silently', async () => {
    await startRename();
    const input = document.querySelector('.renameInput') as HTMLInputElement;
    fireEvent.keyDown(input, { key: 'Enter' }); // unchanged
    await tick();
    expect(renameEntry).not.toHaveBeenCalled();
    expect(document.querySelector('.renameInput')).toBeNull();
  });

  it('a failed rename opens the non-destructive error dialog', async () => {
    vi.mocked(renameEntry).mockRejectedValueOnce(
      new FsError({ code: 'alreadyExists', message: 'Target name already exists' }),
    );
    const { container } = await startRename();
    const input = document.querySelector('.renameInput') as HTMLInputElement;
    fireEvent.input(input, { target: { value: 'reports' } });
    fireEvent.keyDown(input, { key: 'Enter' });
    await vi.waitFor(() => expect(dialogs.current?.kind).toBe('error'));
    expect(dialogs.current).toMatchObject({
      kind: 'error',
      title: 'Rename',
      message: 'Target name already exists',
    });
    await tick();
    // Non-destructive: the editor closed, the row keeps its old name.
    expect(document.querySelector('.renameInput')).toBeNull();
    expect(rowByPath(container, 'C:\\bench\\notes.txt').textContent).toContain('notes.txt');
  });

  it('right-clicking another row cancels an in-progress rename', async () => {
    const { container } = await startRename();
    expect(document.querySelector('.renameInput')).toBeTruthy();
    fireEvent.contextMenu(rowByPath(container, 'C:\\bench\\reports'));
    await tick();
    expect(document.querySelector('.renameInput')).toBeNull();
    expect(renameEntry).not.toHaveBeenCalled();
    expect(contextMenu.isOpen).toBe(true);
  });
});

describe('FileList column resize (FER-541)', () => {
  beforeEach(() => {
    columnsStore.resetAll();
  });

  it('drag handle Size changes grid-template-columns for header and rows', async () => {
    const { container } = render(FileList, {
      rows: twoRows,
      viewportOverride: 520,
    });
    const head = container.querySelector('.head') as HTMLElement;
    expect(head.style.gridTemplateColumns).toBe('minmax(0, 1fr) 72px 132px');

    const sizeBtn = container.querySelector('.hSize') as HTMLElement;
    const handle = sizeBtn.querySelector('[role="separator"]') as HTMLElement;
    expect(handle).toBeTruthy();

    handle.setPointerCapture = vi.fn();
    handle.hasPointerCapture = vi.fn(() => true);
    handle.releasePointerCapture = vi.fn();

    // Start drag at x = 200
    fireEvent.pointerDown(handle, { button: 0, clientX: 200, pointerId: 1 });
    // Move left to x = 172 (delta = -28). Because handle is on the left edge (side="left"),
    // moving left increases width: 72 - (-28) = 100px.
    fireEvent.pointerMove(handle, { clientX: 172, pointerId: 1 });
    await tick();

    // Live update changes grid-template-columns of header
    expect(head.style.gridTemplateColumns).toBe('minmax(0, 1fr) 100px 132px');

    // And rows also reflect the new grid-template-columns
    const row = container.querySelector('.row') as HTMLElement;
    expect(row.style.gridTemplateColumns).toBe('minmax(0, 1fr) 100px 132px');

    const upRow = container.querySelector('.upRow') as HTMLElement;
    expect(upRow.style.gridTemplateColumns).toBe('minmax(0, 1fr) 100px 132px');
  });

  it('commit saves width through the existing persistence store', async () => {
    const oncommit = vi.fn();
    const { container } = render(FileList, {
      rows: twoRows,
      viewportOverride: 520,
      oncolumnwidthcommit: oncommit,
    });

    const sizeBtn = container.querySelector('.hSize') as HTMLElement;
    const handle = sizeBtn.querySelector('[role="separator"]') as HTMLElement;
    handle.setPointerCapture = vi.fn();
    handle.hasPointerCapture = vi.fn(() => true);
    handle.releasePointerCapture = vi.fn();

    fireEvent.pointerDown(handle, { button: 0, clientX: 200, pointerId: 1 });
    fireEvent.pointerMove(handle, { clientX: 172, pointerId: 1 });
    fireEvent.pointerUp(handle, { clientX: 172, pointerId: 1 });
    await tick();

    expect(oncommit).toHaveBeenCalledWith('size', 100);
    expect(columnsStore.widths.size).toBe(100);
    // Persisted to localStorage under the existing customColumns key
    const raw = globalThis.localStorage?.getItem('ferrum.customColumns.v1');
    expect(raw).toBeTruthy();
    const parsed = JSON.parse(raw!);
    expect(parsed.widths.size).toBe(100);
  });

  it('double click on handle resets to default width', async () => {
    columnsStore.setWidth('size', 120);
    const { container } = render(FileList, {
      rows: twoRows,
      viewportOverride: 520,
    });
    const head = container.querySelector('.head') as HTMLElement;
    expect(head.style.gridTemplateColumns).toBe('minmax(0, 1fr) 120px 132px');

    const sizeBtn = container.querySelector('.hSize') as HTMLElement;
    const handle = sizeBtn.querySelector('[role="separator"]') as HTMLElement;

    fireEvent.dblClick(handle);
    await tick();

    expect(columnsStore.widths.size).toBe(72);
    expect(head.style.gridTemplateColumns).toBe('minmax(0, 1fr) 72px 132px');
  });

  it('respects column minimum and maximum bounds', async () => {
    const { container } = render(FileList, {
      rows: twoRows,
      viewportOverride: 520,
    });
    const head = container.querySelector('.head') as HTMLElement;
    const sizeBtn = container.querySelector('.hSize') as HTMLElement;
    const handle = sizeBtn.querySelector('[role="separator"]') as HTMLElement;
    handle.setPointerCapture = vi.fn();
    handle.hasPointerCapture = vi.fn(() => true);
    handle.releasePointerCapture = vi.fn();

    // Shrink size past minimum 56 (dragging right by 100px: 72 - 100 = -28 -> clamps to 56)
    fireEvent.pointerDown(handle, { button: 0, clientX: 100, pointerId: 1 });
    fireEvent.pointerMove(handle, { clientX: 200, pointerId: 1 });
    fireEvent.pointerUp(handle, { clientX: 200, pointerId: 1 });
    await tick();

    expect(head.style.gridTemplateColumns).toBe('minmax(0, 1fr) 56px 132px');
    expect(columnsStore.widths.size).toBe(56);

    // Expand size past maximum 2000 (dragging left by 3000px)
    fireEvent.pointerDown(handle, { button: 0, clientX: 3000, pointerId: 1 });
    fireEvent.pointerMove(handle, { clientX: 0, pointerId: 1 });
    fireEvent.pointerUp(handle, { clientX: 0, pointerId: 1 });
    await tick();

    expect(head.style.gridTemplateColumns).toBe('minmax(0, 1fr) 2000px 132px');
    expect(columnsStore.widths.size).toBe(2000);
  });

  it('clicking and double clicking the resize handle does not trigger header sort', async () => {
    const onsort = vi.fn();
    const { container } = render(FileList, {
      rows: twoRows,
      viewportOverride: 520,
      onsort,
    });

    const sizeBtn = container.querySelector('.hSize') as HTMLElement;
    const handle = sizeBtn.querySelector('[role="separator"]') as HTMLElement;

    // Click handle: sort must NOT trigger
    fireEvent.click(handle);
    expect(onsort).not.toHaveBeenCalled();

    // Double-click handle: sort must NOT trigger
    fireEvent.dblClick(handle);
    expect(onsort).not.toHaveBeenCalled();

    // Click header button outside handle: sort DOES trigger
    fireEvent.click(sizeBtn);
    expect(onsort).toHaveBeenCalledWith('size');
  });

  it('loads default widths when legacy preference has no width field', () => {
    globalThis.localStorage?.setItem(
      'ferrum.customColumns.v1',
      JSON.stringify(['name', 'extension', 'size', 'modified']),
    );
    columnsStore.config = loadColumnConfig();
    columnsStore.widths = loadColumnWidths();
    const { container } = render(FileList, {
      rows: twoRows,
      viewportOverride: 520,
      columns: columnsStore.config,
    });
    const head = container.querySelector('.head') as HTMLElement;
    expect(head.style.gridTemplateColumns).toBe('minmax(0, 1fr) 72px 72px 132px');
  });

  it('corrupted width values fall back to defaults', () => {
    globalThis.localStorage?.setItem(
      'ferrum.customColumns.v1',
      JSON.stringify({
        columns: ['name', 'size', 'modified'],
        widths: { size: 'bogus', modified: -99 },
      }),
    );
    columnsStore.config = loadColumnConfig();
    columnsStore.widths = loadColumnWidths();
    const { container } = render(FileList, {
      rows: twoRows,
      viewportOverride: 520,
      columns: columnsStore.config,
    });
    const head = container.querySelector('.head') as HTMLElement;
    expect(head.style.gridTemplateColumns).toBe('minmax(0, 1fr) 72px 132px');
  });

  it('handles resizable Comment column and resets to fractional unit', async () => {
    const { container } = render(FileList, {
      rows: twoRows,
      viewportOverride: 520,
      showComments: true,
    });
    const head = container.querySelector('.head') as HTMLElement;
    // Default Comment is fractional: minmax(0, 1.6fr)
    expect(head.style.gridTemplateColumns).toBe('minmax(0, 1fr) 72px 132px minmax(0, 1.6fr)');

    const commentDiv = container.querySelector('.hComment') as HTMLElement;
    const handle = commentDiv.querySelector('[role="separator"]') as HTMLElement;
    expect(handle).toBeTruthy();

    handle.setPointerCapture = vi.fn();
    handle.hasPointerCapture = vi.fn(() => true);
    handle.releasePointerCapture = vi.fn();

    // Drag comment handle left (expand by 50px from measured/default 200 -> 250px)
    fireEvent.pointerDown(handle, { button: 0, clientX: 200, pointerId: 1 });
    fireEvent.pointerMove(handle, { clientX: 150, pointerId: 1 });
    fireEvent.pointerUp(handle, { clientX: 150, pointerId: 1 });
    await tick();

    // Now comment is a fixed px width (250px)
    expect(head.style.gridTemplateColumns).toBe('minmax(0, 1fr) 72px 132px 250px');
    expect(columnsStore.widths.comment).toBe(250);

    // Double-click resets back to null / minmax(0, 1.6fr)
    fireEvent.dblClick(handle);
    await tick();

    expect(head.style.gridTemplateColumns).toBe('minmax(0, 1fr) 72px 132px minmax(0, 1.6fr)');
    expect(columnsStore.widths.comment).toBeNull();
  });

  it('does not render column resize handles in brief or thumbnail mode', () => {
    const { container: briefContainer } = render(FileList, {
      rows: twoRows,
      viewportOverride: 520,
      mode: 'brief',
    });
    expect(briefContainer.querySelectorAll('[role="separator"]').length).toBe(0);

    const { container: thumbContainer } = render(FileList, {
      rows: twoRows,
      viewportOverride: 520,
      mode: 'thumbnail',
    });
    expect(thumbContainer.querySelectorAll('[role="separator"]').length).toBe(0);
  });

  it('reactive auto-width: resize observer changes comment aria-valuenow and first drag does not jump', async () => {
    let observerCallback: ResizeObserverCallback | undefined;
    class TestResizeObserver implements ResizeObserver {
      constructor(cb: ResizeObserverCallback) {
        observerCallback = cb;
      }
      observe() {}
      unobserve() {}
      disconnect() {}
    }
    const origRO = globalThis.ResizeObserver;
    globalThis.ResizeObserver = TestResizeObserver;

    try {
      const { container } = render(FileList, {
        rows: twoRows,
        viewportOverride: 520,
        showComments: true,
      });

      const commentDiv = container.querySelector('.hComment') as HTMLElement;
      const handle = commentDiv.querySelector('[role="separator"]') as HTMLElement;
      expect(handle).toBeTruthy();
      expect(handle.getAttribute('aria-valuenow')).toBe('200');

      // Simulate container resize:
      observerCallback?.(
        [
          {
            contentRect: { width: 280, height: 26 },
            borderBoxSize: [{ inlineSize: 280, blockSize: 26 }],
          } as unknown as ResizeObserverEntry,
        ],
        {} as ResizeObserver,
      );
      await tick();

      expect(handle.getAttribute('aria-valuenow')).toBe('280');

      handle.setPointerCapture = vi.fn();
      handle.hasPointerCapture = vi.fn(() => true);
      handle.releasePointerCapture = vi.fn();

      // First drag: side="left", drag from x=300 to x=260 (delta = -40, so width increases by 40 from 280 -> 320)
      fireEvent.pointerDown(handle, { button: 0, clientX: 300, pointerId: 1 });
      fireEvent.pointerMove(handle, { clientX: 260, pointerId: 1 });
      fireEvent.pointerUp(handle, { clientX: 260, pointerId: 1 });
      await tick();

      expect(handle.getAttribute('aria-valuenow')).toBe('320');
      expect(columnsStore.widths.comment).toBe(320);
    } finally {
      globalThis.ResizeObserver = origRO;
    }
  });
});
