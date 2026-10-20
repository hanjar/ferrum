/**
 * FER-577 (B3): HotlistDialog moved from its hand-written handler to the shared dialog
 * conventions: staged Esc, Enter = OK from fields (never from a button or the tree), Ctrl+Enter
 * from anywhere, one access letter per control, focus return.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { tick } from 'svelte';
import HotlistDialog from './HotlistDialog.svelte';
import { hotlist } from '../stores/hotlistStore.svelte';
import { registerPane, type PaneApi } from '../stores/panesRegistry.svelte';
import { resetMemoryBackend } from '../ipc/hotlist';
import { duplicateMnemonics, modalDepth, resetModalDepth } from '../dialogKit';
import type { HotItem } from '../hotlist/model';

const dir = (title: string, path: string): HotItem => ({ type: 'dir', title, path });
const START: HotItem[] = [
  dir('Alpha', 'C:\\alpha'),
  { type: 'command', title: 'Tool', command: 'C:\\t.exe', args: [] },
];

function pane(): PaneApi {
  return {
    path: 'C:\\here',
    selectedPaths: () => [],
    cursorEntry: () => undefined,
    refresh: () => {},
    newTab: () => {},
    closeCurrentTab: () => {},
    goUp: () => {},
    openCursorEntry: () => {},
    openPath: vi.fn(),
  };
}

let opener: HTMLButtonElement;

async function openDialog() {
  await hotlist.replace(START);
  opener.focus();
  hotlist.openConfigure();
  await tick();
  const modal = await screen.findByRole('dialog', { name: 'Configure Directory Hotlist' });
  await tick();
  return modal;
}

beforeEach(() => {
  resetModalDepth();
  registerPane(0, pane());
  opener = document.createElement('button');
  document.body.append(opener);
  render(HotlistDialog);
});
afterEach(async () => {
  cleanup();
  opener.remove();
  hotlist.closeConfigure();
  resetMemoryBackend();
  await hotlist.load();
  resetModalDepth();
  vi.restoreAllMocks();
});

describe('HotlistDialog keyboard (B3)', () => {
  it('starts on the tree, counts as a modal, Esc closes and focus goes back', async () => {
    const modal = await openDialog();
    expect(document.activeElement).toBe(screen.getByRole('tree'));
    expect(modalDepth()).toBe(1);
    await fireEvent.keyDown(modal, { key: 'Escape' });
    expect(hotlist.configureOpen).toBe(false);
    await tick();
    expect(document.activeElement).toBe(opener);
  });

  it('has no access letter used twice (tree view, dir item, command item)', async () => {
    const modal = await openDialog();
    expect(duplicateMnemonics(modal)).toEqual([]); // Alpha is selected: dir fields
    await fireEvent.click(screen.getByRole('treeitem', { name: /Tool/ }));
    await tick();
    expect(duplicateMnemonics(modal)).toEqual([]); // command fields
    expect(modal.querySelectorAll('[data-mnemonic]').length).toBeGreaterThanOrEqual(12);
  });

  it('Alt+letter presses the side buttons', async () => {
    const modal = await openDialog();
    const before = screen.getAllByRole('treeitem').length;
    await fireEvent.keyDown(modal, { key: 'i', altKey: true }); // Add item
    await tick();
    expect(screen.getAllByRole('treeitem').length).toBe(before + 1);
    await fireEvent.keyDown(modal, { key: 'd', altKey: true }); // Delete item
    await tick();
    expect(screen.getAllByRole('treeitem').length).toBe(before);
  });

  it('Esc is staged: import view first, then the dialog', async () => {
    const modal = await openDialog();
    await fireEvent.click(screen.getByRole('button', { name: /Import from Total Commander/ }));
    await tick();
    expect(screen.getByRole('region', { name: 'Import from Total Commander' })).toBeTruthy();
    // The Import button vanished with the tree view: focus must have moved inside the dialog.
    await vi.waitFor(() =>
      expect(document.activeElement).toBe(screen.getByLabelText('wincmd.ini path')),
    );
    await fireEvent.keyDown(modal, { key: 'Escape' });
    await tick();
    expect(screen.queryByRole('region', { name: 'Import from Total Commander' })).toBeNull();
    await vi.waitFor(() => expect(document.activeElement).toBe(screen.getByRole('tree')));
    expect(hotlist.configureOpen).toBe(true);
    await fireEvent.keyDown(modal, { key: 'Escape' });
    expect(hotlist.configureOpen).toBe(false);
  });

  it('Enter in a field saves (OK); Enter on a button or in the tree does not', async () => {
    const modal = await openDialog();
    const replace = vi.spyOn(hotlist, 'replace');
    // Tree: Enter is not OK.
    await fireEvent.keyDown(screen.getByRole('tree'), { key: 'Enter' });
    expect(replace).not.toHaveBeenCalled();
    // Button: native activation, no handler runs OK.
    const up = screen.getByRole('button', { name: 'Up' });
    const ev = new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true });
    up.dispatchEvent(ev);
    expect(ev.defaultPrevented).toBe(false);
    expect(replace).not.toHaveBeenCalled();
    // Field: OK.
    await fireEvent.keyDown(screen.getByLabelText('Title'), { key: 'Enter' });
    await vi.waitFor(() => expect(replace).toHaveBeenCalledTimes(1));
    await vi.waitFor(() => expect(hotlist.configureOpen).toBe(false));
    expect(modal.isConnected).toBe(false);
  });

  it('Ctrl+Enter is OK from a button too', async () => {
    await openDialog();
    const replace = vi.spyOn(hotlist, 'replace');
    await fireEvent.keyDown(screen.getByRole('button', { name: 'Up' }), {
      key: 'Enter',
      ctrlKey: true,
    });
    await vi.waitFor(() => expect(replace).toHaveBeenCalledTimes(1));
  });

  it('Enter in the wincmd.ini path field previews; it never applies an old preview (B1)', async () => {
    await openDialog();
    const fileA = {
      doc: { version: 2, items: [{ type: 'dir', title: 'FromA', path: 'C:\\a' }] },
      warnings: [],
    };
    const fileB = {
      doc: { version: 2, items: [{ type: 'dir', title: 'FromB', path: 'C:\\b' }] },
      warnings: [],
    };
    const prev = vi
      .spyOn(hotlist.api, 'importPreview')
      .mockResolvedValueOnce(fileA as never)
      .mockResolvedValueOnce(fileB as never);
    await fireEvent.click(screen.getByRole('button', { name: /Import from Total Commander/ }));
    await tick();
    const path = (await vi.waitFor(() =>
      screen.getByLabelText('wincmd.ini path'),
    )) as HTMLInputElement;
    await fireEvent.input(path, { target: { value: 'C:\\a.ini' } });
    await fireEvent.keyDown(path, { key: 'Enter' });
    await vi.waitFor(() => expect(prev).toHaveBeenCalledTimes(1));
    await screen.findByText('FromA');
    // Replace mode, then the path changes: the old preview is gone, Enter previews B and does not
    // replace the draft with A.
    await fireEvent.click(screen.getByLabelText(/Replace my hotlist/));
    await fireEvent.input(path, { target: { value: 'C:\\b.ini' } });
    expect(screen.queryByText('FromA')).toBeNull();
    await fireEvent.keyDown(path, { key: 'Enter' });
    await vi.waitFor(() => expect(prev).toHaveBeenCalledTimes(2));
    await screen.findByText('FromB');
    expect(screen.queryByRole('region', { name: 'Import from Total Commander' })).not.toBeNull();
    // The draft still holds the original tree (nothing was replaced yet).
    await fireEvent.keyDown(screen.getByRole('dialog'), { key: 'Escape' });
    await tick();
    expect(screen.queryAllByRole('treeitem').length).toBe(START.length);
  });
});
