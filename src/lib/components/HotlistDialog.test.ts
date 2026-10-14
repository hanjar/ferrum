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

  it('adds the active folder, a separator, a command and a submenu after the selection', async () => {
    render(HotlistDialog);
    await openDialog();
    await fireEvent.click(screen.getByRole('button', { name: 'Add item' }));
    expect(treeTitles()[1]).toContain('folder');
    expect(treeTitles()[1]).toContain('C:\\here\\folder');
    await fireEvent.click(screen.getByRole('button', { name: 'Add separator' }));
    expect(treeTitles()[2]).toContain('separator');
    await fireEvent.click(screen.getByRole('button', { name: 'Add command' }));
    expect(treeTitles()[3]).toContain('New command');
    // The new command has no program yet: OK is blocked with an explanation.
    expect(screen.getByRole('button', { name: 'OK' }).hasAttribute('disabled')).toBe(true);
    expect(screen.getByRole('alert').textContent).toMatch(/needs a local program/);
    await fireEvent.input(screen.getByLabelText('Command'), { target: { value: 'C:\\x.exe' } });
    await fireEvent.input(screen.getByLabelText('Arguments'), {
      target: { value: '-a "two words"' },
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Add submenu' }));
    expect(treeTitles()[4]).toContain('New submenu');
    expect(screen.getByRole('button', { name: 'OK' }).hasAttribute('disabled')).toBe(false);
    await fireEvent.click(screen.getByRole('button', { name: 'OK' }));
    await waitFor(() => expect(hotlist.configureOpen).toBe(false));
    expect(hotlist.doc.items[3]).toEqual({
      type: 'command',
      title: 'New command',
      command: 'C:\\x.exe',
      args: ['-a', 'two words'],
    });
    expect(hotlist.doc.items[4]).toEqual({ type: 'submenu', title: 'New submenu', items: [] });
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

describe('HotlistDialog moving items (pointer drag, indent/outdent, add into submenu)', () => {
  const ROW = 30;
  const TREE: HotItem[] = [
    { type: 'submenu', title: 'Zoom', items: [] },
    dir('C', 'C:\\c'),
    dir('D', 'C:\\d'),
    { type: 'submenu', title: 'Full', items: [dir('F1', 'C:\\f1')] },
  ];

  /** jsdom has no layout: every tree row is ROW px high, stacked from y = 0. */
  function stubRows() {
    vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockImplementation(function (
      this: HTMLElement,
    ) {
      const rows = this.parentElement
        ? [...this.parentElement.children].filter((c) => c.getAttribute('role') === 'treeitem')
        : [];
      const i = rows.indexOf(this);
      const top = i < 0 ? 0 : i * ROW;
      const h = i < 0 ? 0 : ROW;
      return {
        top,
        bottom: top + h,
        left: 0,
        right: 200,
        width: 200,
        height: h,
        x: 0,
        y: top,
      } as DOMRect;
    });
  }
  /** Y inside row `row` at fraction `frac` of its height. */
  const at = (row: number, frac = 0.5) => (row + frac) * ROW;
  const firstLetters = () => treeTitles().map((t) => t.replace(/^\W+/, '')[0]);

  async function drag(from: RegExp, y: number, opts: { release?: boolean } = {}) {
    const el = item(from);
    await fireEvent.pointerDown(el, { button: 0, clientX: 10, clientY: 5, pointerId: 1 });
    await fireEvent.pointerMove(el, { clientX: 12, clientY: y, pointerId: 1 });
    if (opts.release !== false) {
      await fireEvent.pointerUp(el, { clientX: 12, clientY: y, pointerId: 1 });
    }
  }

  const titleOf = (i: HotItem) => (i as { title: string }).title;
  const children = (i: number) => (hotlist.doc.items[i] as { items: HotItem[] }).items;
  async function ok() {
    await fireEvent.click(screen.getByRole('button', { name: 'OK' }));
    await waitFor(() => expect(hotlist.configureOpen).toBe(false));
  }
  // Rows of TREE (all unfolded): 0 Zoom, 1 C, 2 D, 3 Full, 4 F1.
  const C = /^C\b/;
  const D = /^D\b/;

  it('tree rows are no longer HTML5 draggable', async () => {
    render(HotlistDialog);
    await openDialog(TREE);
    for (const row of screen.getAllByRole('treeitem')) {
      expect(row.getAttribute('draggable')).toBeNull();
    }
  });

  it('dropping on the middle of an EMPTY submenu row puts the item inside it', async () => {
    render(HotlistDialog);
    await openDialog(TREE);
    stubRows();
    await drag(C, at(0));
    expect(treeTitles()[0]).toContain('Zoom');
    expect(item(C).getAttribute('aria-level')).toBe('2');
    await ok();
    expect(children(0).map(titleOf)).toEqual(['C']);
    expect(hotlist.doc.items.map(titleOf)).toEqual(['Zoom', 'D', 'Full']);
  });

  it('dropping on a non-empty submenu appends at the end of its contents', async () => {
    render(HotlistDialog);
    await openDialog(TREE);
    stubRows();
    await drag(C, at(3));
    await ok();
    expect(children(2).map(titleOf)).toEqual(['F1', 'C']);
  });

  it('dropping on the upper or lower part of a row inserts before or after it', async () => {
    render(HotlistDialog);
    await openDialog(TREE);
    stubRows();
    await drag(D, at(1, 0.1)); // upper part of C: before C
    expect(firstLetters()).toEqual(['Z', 'D', 'C', 'F', 'F']);
    await drag(D, at(2, 0.9)); // lower part of C (now row 2): after C
    expect(firstLetters()).toEqual(['Z', 'C', 'D', 'F', 'F']);
    // The lower third of a submenu row is "after it", not "inside it".
    await drag(C, at(0, 0.9));
    expect(treeTitles()[0]).toContain('Zoom');
    expect(item(C).getAttribute('aria-level')).toBe('1');
  });

  it('a submenu cannot be dragged into itself or its child: message, tree unchanged', async () => {
    render(HotlistDialog);
    await openDialog(TREE);
    stubRows();
    await drag(/Full/, at(4)); // before/after its own child F1 lies inside Full
    expect(hotlist.notice).toMatch(/can't be moved there/);
    expect(treeTitles()).toHaveLength(5);
    expect(item(/F1/).getAttribute('aria-level')).toBe('2');
    expect(item(/Full/).getAttribute('aria-level')).toBe('1');
  });

  it('a move under 4 px is a click that selects, not a drag', async () => {
    render(HotlistDialog);
    await openDialog(TREE);
    stubRows();
    const el = item(D);
    await fireEvent.pointerDown(el, { button: 0, clientX: 10, clientY: at(2), pointerId: 1 });
    await fireEvent.pointerMove(el, { clientX: 12, clientY: at(2) + 2, pointerId: 1 });
    await fireEvent.pointerUp(el, { clientX: 12, clientY: at(0), pointerId: 1 });
    await fireEvent.click(el);
    expect(el.getAttribute('aria-selected')).toBe('true');
    expect(firstLetters()).toEqual(['Z', 'C', 'D', 'F', 'F']);
  });

  it('Escape cancels a drag without changing the tree or closing the dialog', async () => {
    render(HotlistDialog);
    await openDialog(TREE);
    stubRows();
    const el = item(C);
    await drag(C, at(0), { release: false });
    expect(el.className).toMatch(/dragging/);
    expect(item(/Zoom/).className).toMatch(/drop-into/);
    await fireEvent.keyDown(screen.getByRole('tree'), { key: 'Escape' });
    expect(hotlist.configureOpen).toBe(true);
    expect(el.className).not.toMatch(/dragging/);
    await fireEvent.pointerUp(el, { clientX: 12, clientY: at(0), pointerId: 1 });
    expect(item(C).getAttribute('aria-level')).toBe('1');
    expect(treeTitles()).toHaveLength(5);
  });

  it('Into submenu / Out of submenu buttons and Ctrl+Right / Ctrl+Left', async () => {
    render(HotlistDialog);
    await openDialog(TREE);
    const tree = screen.getByRole('tree');
    const into = screen.getByRole('button', { name: 'Into submenu' });
    const out = screen.getByRole('button', { name: 'Out of submenu' });
    expect(into.hasAttribute('disabled')).toBe(true); // Zoom is first
    expect(out.hasAttribute('disabled')).toBe(true); // already at the top level
    await fireEvent.click(item(C));
    expect(into.hasAttribute('disabled')).toBe(false); // Zoom is right above C
    expect(out.hasAttribute('disabled')).toBe(true);
    await fireEvent.keyDown(tree, { key: 'ArrowRight', ctrlKey: true });
    expect(item(C).getAttribute('aria-level')).toBe('2');
    expect(item(C).getAttribute('aria-selected')).toBe('true');
    expect(into.hasAttribute('disabled')).toBe(true);
    expect(out.hasAttribute('disabled')).toBe(false);
    await fireEvent.keyDown(tree, { key: 'ArrowLeft', ctrlKey: true });
    expect(item(C).getAttribute('aria-level')).toBe('1');
    await fireEvent.keyDown(tree, { key: 'ArrowLeft', ctrlKey: true }); // top level: no-op
    expect(item(C).getAttribute('aria-level')).toBe('1');
    await fireEvent.click(into);
    expect(item(C).getAttribute('aria-level')).toBe('2');
    await fireEvent.click(out);
    expect(item(C).getAttribute('aria-level')).toBe('1');
    // D sits below C, which is a folder, not a submenu: indent is refused.
    await fireEvent.click(item(D));
    await fireEvent.keyDown(tree, { key: 'ArrowRight', ctrlKey: true });
    expect(item(D).getAttribute('aria-level')).toBe('1');
  });

  it('Ctrl+Up / Ctrl+Down move the item like the Up / Down buttons', async () => {
    render(HotlistDialog);
    await openDialog(TREE);
    const tree = screen.getByRole('tree');
    await fireEvent.click(item(C));
    await fireEvent.keyDown(tree, { key: 'ArrowUp', ctrlKey: true });
    expect(treeTitles()[0]).toContain('C');
    await fireEvent.keyDown(tree, { key: 'ArrowDown', ctrlKey: true });
    expect(treeTitles()[1]).toContain('C');
  });

  it('Add with a submenu selected adds inside it, at the end', async () => {
    render(HotlistDialog);
    await openDialog(TREE);
    await fireEvent.click(item(/Full/));
    await fireEvent.click(screen.getByRole('button', { name: 'Add separator' }));
    await fireEvent.click(item(/Full/));
    await fireEvent.click(screen.getByRole('button', { name: 'Add item' }));
    expect(screen.getAllByRole('treeitem')).toHaveLength(7);
    await ok();
    const full = children(3);
    expect(full).toHaveLength(3);
    expect(full[0]).toMatchObject({ title: 'F1' });
    expect(full[1]).toEqual({ type: 'separator' });
    expect(full[2]).toMatchObject({ type: 'dir', path: 'C:\\here\\folder' });
    expect(hotlist.doc.items).toHaveLength(4);
  });

  it('Add into an EMPTY selected submenu works too', async () => {
    render(HotlistDialog);
    await openDialog(TREE);
    await fireEvent.click(item(/Zoom/));
    await fireEvent.click(screen.getByRole('button', { name: 'Add command' }));
    expect(item(/New command/).getAttribute('aria-level')).toBe('2');
  });

  it('Add with a non-submenu selected still adds after it in the same list', async () => {
    render(HotlistDialog);
    await openDialog(TREE);
    await fireEvent.click(item(/F1/));
    await fireEvent.click(screen.getByRole('button', { name: 'Add separator' }));
    await ok();
    expect(children(3)[1]).toEqual({ type: 'separator' });
  });

  it('a submenu folds and unfolds with the glyph and the Left / Right keys', async () => {
    render(HotlistDialog);
    await openDialog(TREE);
    const tree = screen.getByRole('tree');
    expect(item(/Full/).textContent).toContain('▾');
    await fireEvent.click(item(/F1/));
    await fireEvent.click(item(/Full/));
    await fireEvent.keyDown(tree, { key: 'ArrowLeft' });
    expect(screen.queryAllByRole('treeitem')).toHaveLength(4);
    expect(item(/Full/).textContent).toContain('▸');
    await fireEvent.keyDown(tree, { key: 'ArrowRight' });
    expect(screen.queryAllByRole('treeitem')).toHaveLength(5);
    await fireEvent.click(item(/Full/).querySelector('.glyph')!);
    expect(screen.queryAllByRole('treeitem')).toHaveLength(4);
  });

  it('explains the item kinds next to the buttons', async () => {
    render(HotlistDialog);
    await openDialog(TREE);
    const help = screen.getByText(/Separator = a divider line/);
    expect(help.textContent).toMatch(/Item = shortcut to a folder/);
    expect(help.textContent).toMatch(/Command =\s+runs a program/);
  });
});
