import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import { tick } from 'svelte';
import HotlistDialog from './HotlistDialog.svelte';
import { hotlist } from '../stores/hotlistStore.svelte';
import { registerPane, type PaneApi } from '../stores/panesRegistry.svelte';
import { HotlistError, resetMemoryBackend } from '../ipc/hotlist';
import type { HotItem } from '../hotlist/model';

const dir = (title: string, path: string): HotItem => ({ type: 'dir', title, path });

const START: HotItem[] = [
  dir('&Alpha', 'C:\\alpha'),
  { type: 'submenu', title: 'Group', items: [dir('Inner', 'C:\\inner')] },
  { type: 'command', title: 'Tool', command: 'C:\\t.exe', args: ['-x', 'a b'] },
];

let current = 'C:\\here\\folder';

function pane(): PaneApi {
  return {
    get path() {
      return current;
    },
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

async function openDialog(items: HotItem[] = START) {
  await hotlist.replace(items);
  hotlist.openConfigure();
  await tick();
  await screen.findByRole('dialog', { name: 'Configure Directory Hotlist' });
  await tick();
}

const treeTitles = () => screen.queryAllByRole('treeitem').map((t) => t.textContent?.trim() ?? '');
const item = (name: RegExp | string) => screen.getByRole('treeitem', { name });

beforeEach(() => {
  current = 'C:\\here\\folder';
  registerPane(0, pane());
});

afterEach(async () => {
  cleanup();
  hotlist.closeConfigure();
  hotlist.closeMenu();
  hotlist.say('');
  resetMemoryBackend();
  await hotlist.load();
  vi.restoreAllMocks();
});

describe('HotlistDialog (Configure)', () => {
  it('renders nothing while closed and the tree with indentation when open', async () => {
    const { container } = render(HotlistDialog);
    expect(container.querySelector('[role="dialog"]')).toBeNull();
    await openDialog();
    expect(screen.getAllByRole('treeitem')).toHaveLength(4); // Alpha, Group, Inner, Tool
    expect(item(/Inner/).getAttribute('aria-level')).toBe('2');
    expect(item(/Tool/).textContent).toContain('▶');
    expect(item(/Alpha/).getAttribute('aria-selected')).toBe('true');
  });

  it('edits title, path and target path of a folder in the draft only until OK', async () => {
    render(HotlistDialog);
    await openDialog();
    await fireEvent.input(screen.getByLabelText('Title'), { target: { value: 'Renamed' } });
    await fireEvent.input(screen.getByLabelText('Path'), { target: { value: 'D:\\new' } });
    await fireEvent.input(screen.getByLabelText('Target path'), { target: { value: 'E:\\other' } });
    expect(treeTitles()[0]).toContain('Renamed');
    expect(hotlist.entries()[0].path).toBe('C:\\alpha'); // nothing saved yet
    await fireEvent.click(screen.getByRole('button', { name: 'OK' }));
    await waitFor(() => expect(hotlist.configureOpen).toBe(false));
    expect(hotlist.doc.items[0]).toEqual({
      type: 'dir',
      title: 'Renamed',
      path: 'D:\\new',
      targetPath: 'E:\\other',
    });
  });

  it('Cancel discards the draft', async () => {
    render(HotlistDialog);
    await openDialog();
    await fireEvent.click(screen.getByRole('button', { name: 'Delete item' }));
    expect(screen.getAllByRole('treeitem')).toHaveLength(3);
    await fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(hotlist.configureOpen).toBe(false);
    expect(hotlist.doc.items).toHaveLength(3);
  });

  it('adds the active folder, a submenu, a separator and a command after the selection', async () => {
    render(HotlistDialog);
    await openDialog();
    await fireEvent.click(screen.getByRole('button', { name: 'Add item' }));
    expect(treeTitles()[1]).toContain('folder');
    expect(treeTitles()[1]).toContain('C:\\here\\folder');
    await fireEvent.click(screen.getByRole('button', { name: 'Add submenu' }));
    expect(treeTitles()[2]).toContain('New submenu');
    await fireEvent.click(screen.getByRole('button', { name: 'Add separator' }));
    expect(treeTitles()[3]).toContain('separator');
    await fireEvent.click(screen.getByRole('button', { name: 'Add command' }));
    expect(treeTitles()[4]).toContain('New command');
    // The new command has no program yet: OK is blocked with an explanation.
    expect(screen.getByRole('button', { name: 'OK' }).hasAttribute('disabled')).toBe(true);
    expect(screen.getByRole('alert').textContent).toMatch(/needs a local program/);
    await fireEvent.input(screen.getByLabelText('Command'), { target: { value: 'C:\\x.exe' } });
    await fireEvent.input(screen.getByLabelText('Arguments'), {
      target: { value: '-a "two words"' },
    });
    expect(screen.getByRole('button', { name: 'OK' }).hasAttribute('disabled')).toBe(false);
    await fireEvent.click(screen.getByRole('button', { name: 'OK' }));
    await waitFor(() => expect(hotlist.configureOpen).toBe(false));
    expect(hotlist.doc.items[4]).toEqual({
      type: 'command',
      title: 'New command',
      command: 'C:\\x.exe',
      args: ['-a', 'two words'],
    });
  });

  it('moves items up and down and deletes the selection, keeping a sensible selection', async () => {
    render(HotlistDialog);
    await openDialog();
    await fireEvent.click(screen.getByRole('button', { name: 'Down' }));
    expect(treeTitles()[0]).toContain('Group');
    expect(item(/Alpha/).getAttribute('aria-selected')).toBe('true');
    await fireEvent.click(screen.getByRole('button', { name: 'Delete item' }));
    expect(treeTitles().some((t) => t.includes('Alpha'))).toBe(false);
    expect(
      screen.getAllByRole('treeitem').some((t) => t.getAttribute('aria-selected') === 'true'),
    ).toBe(true);
  });

  it('keyboard: arrows move the selection and Delete removes the item', async () => {
    render(HotlistDialog);
    await openDialog();
    const tree = screen.getByRole('tree');
    await fireEvent.keyDown(tree, { key: 'ArrowDown' });
    expect(item(/Group/).getAttribute('aria-selected')).toBe('true');
    await fireEvent.keyDown(tree, { key: 'Delete' });
    expect(treeTitles().some((t) => t.includes('Group'))).toBe(false);
  });

  it('disables OK for an empty path and while the file is unavailable', async () => {
    render(HotlistDialog);
    await openDialog();
    await fireEvent.input(screen.getByLabelText('Path'), { target: { value: '' } });
    expect(screen.getByRole('button', { name: 'OK' }).hasAttribute('disabled')).toBe(true);
    expect(screen.getByRole('alert').textContent).toMatch(/title and a path/);
    await fireEvent.input(screen.getByLabelText('Path'), { target: { value: 'C:\\ok' } });
    expect(screen.getByRole('button', { name: 'OK' }).hasAttribute('disabled')).toBe(false);
    hotlist.applyView({
      doc: { version: 2, items: START },
      status: 'cloud',
      readOnly: true,
      customLocation: 'D:\\h.json',
    });
    await tick();
    expect(screen.getByRole('button', { name: 'OK' }).hasAttribute('disabled')).toBe(true);
    expect(screen.getByText(/shown read-only/)).toBeTruthy();
  });

  it('Escape closes the dialog', async () => {
    render(HotlistDialog);
    await openDialog();
    await fireEvent.keyDown(screen.getByRole('dialog'), { key: 'Escape' });
    expect(hotlist.configureOpen).toBe(false);
  });
});

describe('HotlistDialog import from Total Commander', () => {
  const preview = {
    doc: {
      version: 2 as const,
      source: 'Total Commander [DirMenu]',
      items: [
        dir('&Downloads', 'C:\\dl'),
        { type: 'command' as const, title: 'kando', command: 'C:\\k.exe', args: [] },
        { type: 'submenu' as const, title: 'Dummy', items: [] },
      ],
    },
    warnings: [{ index: 7, kind: 'no_action' }],
  };

  it('previews first, explains commands and warnings, and merges into the draft on Import', async () => {
    render(HotlistDialog);
    await openDialog();
    const spy = vi.spyOn(hotlist.api, 'importPreview').mockResolvedValue(preview);
    await fireEvent.click(screen.getByRole('button', { name: /Import from Total Commander/ }));
    await fireEvent.click(screen.getByRole('button', { name: 'Preview' }));
    const list = await screen.findByRole('list', { name: 'Import preview' });
    expect(spy).toHaveBeenCalledExactlyOnceWith(null);
    expect(within(list).getByText('Downloads')).toBeTruthy();
    expect(screen.getByText(/never run automatically/)).toBeTruthy();
    expect(screen.getByRole('list', { name: 'Import warnings' }).textContent).toContain('Entry 7');
    // Nothing is merged until Import is pressed.
    expect(hotlist.doc.items).toHaveLength(3);
    await fireEvent.click(screen.getByRole('button', { name: 'Import' }));
    expect(treeTitles().some((t) => t.includes('Downloads'))).toBe(true);
    expect(treeTitles().some((t) => t.includes('Alpha'))).toBe(true); // appended, not replaced
    // …and still not saved until OK.
    expect(hotlist.doc.items).toHaveLength(3);
  });

  it('can replace the whole hotlist, and passes a typed path to the backend', async () => {
    render(HotlistDialog);
    await openDialog();
    const spy = vi.spyOn(hotlist.api, 'importPreview').mockResolvedValue(preview);
    await fireEvent.click(screen.getByRole('button', { name: /Import from Total Commander/ }));
    await fireEvent.input(screen.getByLabelText('wincmd.ini path'), {
      target: { value: 'C:\\tmp\\wincmd.ini' },
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Preview' }));
    await screen.findByRole('list', { name: 'Import preview' });
    expect(spy).toHaveBeenCalledWith('C:\\tmp\\wincmd.ini');
    await fireEvent.click(screen.getByRole('radio', { name: /Replace my hotlist/ }));
    await fireEvent.click(screen.getByRole('button', { name: 'Import' }));
    expect(treeTitles().some((t) => t.includes('Alpha'))).toBe(false);
    expect(treeTitles().some((t) => t.includes('Downloads'))).toBe(true);
  });

  it('shows a calm message for a missing file or a file without [DirMenu]', async () => {
    render(HotlistDialog);
    await openDialog();
    const spy = vi.spyOn(hotlist.api, 'importPreview');
    spy.mockRejectedValueOnce(new HotlistError('not_found'));
    await fireEvent.click(screen.getByRole('button', { name: /Import from Total Commander/ }));
    await fireEvent.click(screen.getByRole('button', { name: 'Preview' }));
    expect((await screen.findByRole('alert')).textContent).toMatch(/not found/i);
    spy.mockRejectedValueOnce(new HotlistError('no_dirmenu'));
    await fireEvent.click(screen.getByRole('button', { name: 'Preview' }));
    await waitFor(() => expect(screen.getByRole('alert').textContent).toMatch(/\[DirMenu\]/));
    expect(screen.getByRole('button', { name: 'Import' }).hasAttribute('disabled')).toBe(true);
  });
});
