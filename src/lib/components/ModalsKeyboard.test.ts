/**
 * FER-577 (B1): keyboard behaviour shared by the modal tools (MultiRename, FileTools, Custom
 * Columns, View Presets, Properties, Background Transfer Manager) and the context menu.
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, render, screen } from '@testing-library/svelte';
import { tick } from 'svelte';
import MultiRenameDialog from './MultiRenameDialog.svelte';
import FileToolsDialog from './FileToolsDialog.svelte';
import CustomColumnsDialog from './CustomColumnsDialog.svelte';
import ViewPresetsDialog from './ViewPresetsDialog.svelte';
import PropertiesDialog from './PropertiesDialog.svelte';
import OpsPanel from './OpsPanel.svelte';
import ContextMenu from './ContextMenu.svelte';
import { duplicateMnemonics, modalDepth, resetModalDepth } from '../dialogKit';
import { multiRename } from '../stores/multiRenameStore.svelte';
import { fileTools } from '../stores/fileToolsStore.svelte';
import { columns } from '../stores/columnsStore.svelte';
import { viewPresets } from '../stores/viewPresetsStore.svelte';
import { propertiesStore } from '../stores/propertiesStore.svelte';
import { ops } from '../stores/opsStore.svelte';
import { contextMenu, type ContextMenuItem } from '../stores/contextMenuStore.svelte';
import { registerPane, type PaneApi } from '../stores/panesRegistry.svelte';

const renameBatchMock = vi.fn().mockResolvedValue({ renamed: 2, requested: 2, failures: [] });
vi.mock('../ipc/renameBatch', () => ({
  renameBatch: (...args: unknown[]) => renameBatchMock(...args),
}));

function key(el: Element, k: string, init: KeyboardEventInit = {}): KeyboardEvent {
  const ev = new KeyboardEvent('keydown', { key: k, bubbles: true, cancelable: true, ...init });
  el.dispatchEvent(ev);
  return ev;
}

const unregs: (() => void)[] = [];
afterEach(() => {
  cleanup();
  multiRename.close();
  fileTools.close();
  columns.close();
  viewPresets.close();
  propertiesStore.close();
  ops.panelOpen = false;
  ops.ops.clear();
  contextMenu.close();
  renameBatchMock.mockClear();
  for (const u of unregs.splice(0)) u();
  resetModalDepth();
});

const modal = () => document.querySelector<HTMLElement>('[role="dialog"]')!;

describe('every tool modal: depth, Esc, focus return, unique accelerators', () => {
  const cases: [string, () => void, () => unknown][] = [
    [
      'MultiRename',
      () => {
        const pane: PaneApi = {
          path: 'C:/d',
          selectedPaths: () => ['C:/d/a.txt', 'C:/d/b.txt'],
          cursorEntry: () => undefined,
          refresh: () => {},
          newTab: () => {},
          closeCurrentTab: () => {},
          goUp: () => {},
          openCursorEntry: () => {},
          listNames: () => ['a.txt', 'b.txt'],
        };
        unregs.push(registerPane(0, pane), registerPane(1, pane));
        multiRename.openForSelection();
      },
      () => render(MultiRenameDialog),
    ],
    [
      'FileTools create',
      () => fileTools.open({ kind: 'checksumCreate', paths: ['C:/d/a.txt'] }),
      () => render(FileToolsDialog),
    ],
    [
      'FileTools split',
      () => fileTools.open({ kind: 'split', path: 'C:/d/a.bin' }),
      () => render(FileToolsDialog),
    ],
    [
      'FileTools archive member',
      () =>
        fileTools.open({
          kind: 'archiveChecksum',
          archivePath: 'C:/d/a.zip',
          memberPath: 'x.txt',
        } as never),
      () => render(FileToolsDialog),
    ],
    ['CustomColumns', () => columns.openDialog(), () => render(CustomColumnsDialog)],
    ['ViewPresets', () => viewPresets.openDialog(), () => render(ViewPresetsDialog)],
    [
      'Properties attributes',
      () => propertiesStore.open({ kind: 'attributes', paths: ['C:/d/a.txt'] }),
      () => render(PropertiesDialog),
    ],
    [
      'Properties system',
      () => propertiesStore.open({ kind: 'system' }),
      () => render(PropertiesDialog),
    ],
  ];

  for (const [name, open, mount] of cases) {
    it(`${name}: counts as a modal, Esc closes, focus returns, no duplicate letters`, async () => {
      const opener = document.createElement('button');
      document.body.append(opener);
      opener.focus();
      mount();
      open();
      await tick();
      await vi.waitFor(() => expect(modal()).toBeTruthy());
      expect(modalDepth()).toBe(1);
      expect(modal().contains(document.activeElement)).toBe(true);
      expect(duplicateMnemonics(modal())).toEqual([]);
      key(document.activeElement ?? modal(), 'Escape');
      await tick();
      expect(document.querySelector('[role="dialog"]')).toBeNull();
      expect(modalDepth()).toBe(0);
      expect(document.activeElement).toBe(opener);
      opener.remove();
    });
  }
});

describe('MultiRename keyboard', () => {
  it('Enter in the pattern field runs the rename; Alt+U / Alt+R are wired', async () => {
    const pane: PaneApi = {
      path: 'C:/d',
      selectedPaths: () => ['C:/d/a.txt', 'C:/d/b.txt'],
      cursorEntry: () => undefined,
      refresh: () => {},
      newTab: () => {},
      closeCurrentTab: () => {},
      goUp: () => {},
      openCursorEntry: () => {},
      listNames: () => ['a.txt', 'b.txt'],
    };
    unregs.push(registerPane(0, pane), registerPane(1, pane));
    render(MultiRenameDialog);
    multiRename.openForSelection();
    await tick();
    multiRename.options.pattern = 'x-[C]';
    await tick();
    const input = screen.getByLabelText('Name pattern') as HTMLInputElement;
    input.focus();
    const ev = key(input, 'Enter');
    expect(ev.defaultPrevented).toBe(true);
    await vi.waitFor(() => expect(renameBatchMock).toHaveBeenCalledTimes(1));
    expect(document.querySelector('[data-mnemonic="r"]')?.textContent?.trim()).toBe('Rename');
  });

  it('Enter on the Close button does not rename', async () => {
    render(MultiRenameDialog);
    const pane: PaneApi = {
      path: 'C:/d',
      selectedPaths: () => ['C:/d/a.txt'],
      cursorEntry: () => undefined,
      refresh: () => {},
      newTab: () => {},
      closeCurrentTab: () => {},
      goUp: () => {},
      openCursorEntry: () => {},
      listNames: () => ['a.txt'],
    };
    unregs.push(registerPane(0, pane), registerPane(1, pane));
    multiRename.openForSelection();
    await tick();
    multiRename.options.pattern = 'y-[C]';
    await tick();
    const close = screen.getByRole('button', { name: 'Close' });
    close.focus();
    expect(key(close, 'Enter').defaultPrevented).toBe(false);
    expect(renameBatchMock).not.toHaveBeenCalled();
  });
});

describe('Background Transfer Manager keyboard', () => {
  function seed() {
    ops.ops.set('a', {
      operationId: 'a',
      kind: 'copy',
      state: 'running',
      processedBytes: 1,
      totalBytes: 10,
      processedFiles: 0,
      totalFiles: 1,
    });
    ops.ops.set('b', {
      operationId: 'b',
      kind: 'move',
      state: 'queued',
      processedBytes: 0,
      totalBytes: 10,
      processedFiles: 0,
      totalFiles: 1,
    });
    ops.panelOpen = true;
  }

  it('takes focus on open, arrows walk the jobs, Esc closes and gives focus back', async () => {
    const opener = document.createElement('button');
    document.body.append(opener);
    opener.focus();
    render(OpsPanel);
    seed();
    await tick();
    const jobs = Array.from(document.querySelectorAll<HTMLElement>('.panel .job'));
    expect(jobs.length).toBe(2);
    await vi.waitFor(() => expect(document.activeElement).toBe(jobs[0]));
    key(jobs[0], 'ArrowDown');
    expect(document.activeElement).toBe(jobs[1]);
    key(jobs[1], 'ArrowDown');
    expect(document.activeElement).toBe(jobs[1]);
    key(jobs[1], 'Home');
    expect(document.activeElement).toBe(jobs[0]);
    key(jobs[0], 'Escape');
    await tick();
    expect(ops.panelOpen).toBe(false);
    expect(document.activeElement).toBe(opener);
    opener.remove();
  });

  it('Alt+B (the opener, muted globally while a modal is open) closes it; Tab wraps', async () => {
    render(OpsPanel);
    seed();
    await tick();
    const buttons = Array.from(document.querySelectorAll<HTMLElement>('.panel button'));
    const last = buttons[buttons.length - 1];
    last.focus();
    expect(key(last, 'Tab').defaultPrevented).toBe(true);
    expect(document.querySelector('.panel')!.contains(document.activeElement)).toBe(true);
    key(last, 'b', { altKey: true });
    await tick();
    expect(ops.panelOpen).toBe(false);
  });
});

describe('ContextMenu keyboard', () => {
  const runs = { open: vi.fn(), del: vi.fn(), dup: vi.fn() };
  const items: ContextMenuItem[] = [
    { label: 'Open', run: runs.open },
    { separator: true },
    { label: 'Duplicate', run: runs.dup },
    { label: 'Delete', danger: true, run: runs.del },
  ];
  async function openMenu() {
    render(ContextMenu);
    contextMenu.open(10, 10, items);
    await tick();
    await vi.waitFor(() => expect(document.activeElement?.getAttribute('role')).toBe('menuitem'));
  }
  const menu = () => document.querySelector<HTMLElement>('[role="menu"]')!;

  it('Home/End jump, letters activate a unique accelerator, items show their underline', async () => {
    await openMenu();
    const labels = Array.from(menu().querySelectorAll('.label u')).map((u) => u.textContent);
    expect(labels.length).toBe(3);
    expect(new Set(labels.map((l) => l!.toLowerCase())).size).toBe(3);
    key(document.activeElement!, 'End');
    expect(document.activeElement?.textContent).toContain('Delete');
    key(document.activeElement!, 'Home');
    expect(document.activeElement?.textContent).toContain('Open');
    const marks = Array.from(menu().querySelectorAll<HTMLElement>('[data-mnemonic]'));
    // S2: the dangerous item is only focused by its letter, Enter (click) runs it.
    key(document.activeElement!, marks[2].dataset.mnemonic!);
    expect(document.activeElement).toBe(marks[2]);
    expect(runs.del).not.toHaveBeenCalled();
    expect(contextMenu.isOpen).toBe(true);
    // A safe item runs at once.
    key(document.activeElement!, marks[1].dataset.mnemonic!);
    expect(runs.dup).toHaveBeenCalledTimes(1);
    expect(contextMenu.isOpen).toBe(false);
  });

  it('Esc and Tab close the menu', async () => {
    await openMenu();
    expect(key(document.activeElement!, 'Escape').defaultPrevented).toBe(true);
    await tick();
    expect(contextMenu.isOpen).toBe(false);
    await openMenu();
    key(document.activeElement!, 'Tab');
    await tick();
    expect(contextMenu.isOpen).toBe(false);
  });
});
