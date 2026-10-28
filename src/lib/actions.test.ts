import { beforeEach, describe, expect, it, vi } from 'vitest';
import {
  actionForEvent,
  actions,
  getAction,
  isEnabled,
  labelOf,
  matches,
  MENU_SEPARATOR,
  menuGroups,
  menuSubmenus,
  runAction,
  shortcutBlockedByFullPage,
  FULL_PAGE_SHORTCUT_IDS,
  searchActions,
  shortcutFor,
  shortcutLabel,
} from './actions';
import { ui } from './uiState.svelte';
import { registerPane, type PaneApi } from './stores/panesRegistry.svelte';
import { dialogs } from './stores/dialogStore.svelte';

/** F2-25: the registry wires shell launches through the store, never raw IPC. */
const shellSpies = vi.hoisted(() => ({
  openFileExternal: vi.fn(),
  associateWith: vi.fn(),
  openTerminalHere: vi.fn(),
  print: vi.fn(),
}));
vi.mock('./stores/shellStore.svelte', () => ({ shell: shellSpies }));

const fakePane: PaneApi = {
  path: 'C:\\t',
  selectedPaths: () => ['C:\\t\\a.txt'],
  cursorEntry: () => ({ name: 'a.txt', path: 'C:\\t\\a.txt', isDir: false }),
  refresh: () => {},
  newTab: () => {},
  closeCurrentTab: () => {},
  goUp: () => {},
  openCursorEntry: () => {},
};

beforeEach(() => {
  registerPane(0, fakePane);
  registerPane(1, fakePane);
  ui.activePane = 0;
  dialogs.close();
  ui.paletteOpen = false;
});

describe('shortcut matching', () => {
  it('matches plain keys without modifiers', () => {
    expect(
      matches({ key: 'F5' }, { key: 'F5', ctrlKey: false, metaKey: false, shiftKey: false }),
    ).toBe(true);
    expect(
      matches({ key: 'F5' }, { key: 'F6', ctrlKey: false, metaKey: false, shiftKey: false }),
    ).toBe(false);
  });

  it('uses Ctrl on Windows and Cmd on macOS', () => {
    const e = { key: 'k', ctrlKey: true, metaKey: false, shiftKey: false };
    const eMeta = { key: 'k', ctrlKey: false, metaKey: true, shiftKey: false };
    expect(matches({ key: 'k', ctrlOrCmd: true }, e, 'Win32')).toBe(true);
    expect(matches({ key: 'k', ctrlOrCmd: true }, eMeta, 'MacIntel')).toBe(true);
    expect(matches({ key: 'k' }, e, 'Win32')).toBe(false);
  });

  it('distinguishes shift variants (F8 vs Shift+F8)', () => {
    const f8 = { key: 'F8', ctrlKey: false, metaKey: false, shiftKey: false };
    const shiftF8 = { key: 'F8', ctrlKey: false, metaKey: false, shiftKey: true };
    expect(matches({ key: 'F8' }, f8)).toBe(true);
    expect(matches({ key: 'F8' }, shiftF8)).toBe(false); // plain F8 must not eat shift
    expect(matches({ key: 'F8', shift: true }, shiftF8)).toBe(true);
  });

  it('case-insensitive letter keys', () => {
    expect(
      matches(
        { key: 'w', ctrlOrCmd: true },
        { key: 'w', ctrlKey: true, metaKey: false, shiftKey: false },
      ),
    ).toBe(true);
  });
});

describe('actionForEvent', () => {
  it('FER-364: Shift+F8 routes to permanent delete; plain F8 keeps the trash delete', () => {
    const shifted = actionForEvent({ key: 'F8', ctrlKey: false, metaKey: false, shiftKey: true });
    expect(shifted?.id).toBe('file.delete.permanent');
    const plain = actionForEvent({ key: 'F8', ctrlKey: false, metaKey: false, shiftKey: false });
    expect(plain?.id).toBe('file.delete');
  });

  it('honors availability', () => {
    const del = actionForEvent({ key: 'F8', ctrlKey: false, metaKey: false, shiftKey: false });
    expect(del?.id).toBe('file.delete'); // fake pane has selection
    // Without any pane registered actions report unavailable.
    const none = actionForEvent(
      { key: 'F8', ctrlKey: false, metaKey: false, shiftKey: false },
      actions.map((a) => ({ ...a, available: () => false })),
    );
    expect(none).toBeUndefined();
  });

  it('FER-456: Alt+D is the address bar and Disk Usage moved to Alt+Shift+D', () => {
    const base = { ctrlKey: false, metaKey: false };
    const pick = (event: object) =>
      actions.find((a) => a.shortcut && matches(a.shortcut, { ...base, ...event } as never))?.id;
    expect(pick({ key: 'd', altKey: true, shiftKey: false })).toBe('nav.editPath');
    expect(pick({ key: 'D', altKey: true, shiftKey: true })).toBe('commands.diskUsage');
    expect(getAction('nav.editPath')?.shortcut).toEqual({ key: 'd', alt: true });
  });

  it('registry ids and assigned shortcuts resolve uniquely', () => {
    const ids = actions.map((a) => a.id);
    expect(new Set(ids).size).toBe(ids.length);
    for (const action of actions) {
      if (!action.shortcut) continue;
      const { key, ctrlOrCmd, shift, alt } = action.shortcut;
      const event = {
        key,
        ctrlKey: Boolean(ctrlOrCmd),
        metaKey: false,
        shiftKey: Boolean(shift),
        altKey: Boolean(alt),
      };
      const hits = actions.filter(
        (candidate) => candidate.shortcut && matches(candidate.shortcut, event),
      );
      expect(
        hits.map((hit) => hit.id),
        `shortcut for ${action.id}`,
      ).toEqual([action.id]);
    }
  });

  it('core shortcuts are all bound', () => {
    for (const key of ['Tab', 'F2', 'F5', 'F6', 'F7', 'F8', 'Enter', 'Backspace']) {
      const hit = actions.some((a) => a.shortcut?.key === key);
      expect(hit, `missing shortcut ${key}`).toBe(true);
    }
    for (const k of ['t', 'w', 'k', 'h']) {
      const hit = actions.some((a) => a.shortcut?.ctrlOrCmd && a.shortcut.key === k);
      expect(hit, `missing ctrl+${k}`).toBe(true);
    }
  });
});

describe('palette search', () => {
  it('empty query lists available actions', () => {
    expect(searchActions('').length).toBeGreaterThan(5);
  });

  it('substring match is case-insensitive on label', () => {
    const hits = searchActions('folder').map((a) => labelOf(a));
    expect(hits.some((l) => l.includes('folder'))).toBe(true);
  });

  it('dynamic labels reflect state', () => {
    const hidden = actions.find((a) => a.id === 'view.hidden')!;
    ui.showHidden = false;
    expect(labelOf(hidden)).toBe('Show hidden files');
    ui.showHidden = true;
    expect(labelOf(hidden)).toBe('Hide hidden files');
    ui.showHidden = false;
  });
});

describe('shared menu registry', () => {
  it('keeps six ordered menu groups with stable, distinct IDs', () => {
    expect(menuGroups.map((group) => group.label)).toEqual([
      'Files',
      'Mark',
      'Commands',
      'View',
      'Sort',
      'Show',
    ]);
    const ids = menuGroups
      .flatMap((group) => group.ids)
      .flatMap((id) => menuSubmenus[id]?.ids ?? [id])
      .filter((id) => id !== MENU_SEPARATOR);
    expect(new Set(ids).size).toBe(ids.length);
    for (const id of ids) expect(getAction(id)?.id).toBe(id);
    expect(menuSubmenus['submenu:archive'].ids).toEqual([
      'file.pack',
      'file.unpack',
      'file.testArchive',
    ]);
    expect(menuGroups[1].ids).toContain('mark.selectAll');
    expect(menuGroups[2].ids).toContain('palette.open');
    expect(menuGroups[3].ids).toContain('view.hidden');
    expect(menuGroups[4].ids).toContain('show.sortName');
    expect(menuGroups[5].ids).toContain('view.aiChat');
    expect(getAction('toString')).toBeUndefined();
    expect(getAction('missing.action')).toBeUndefined();
  });

  it('FER-395: Settings opens the dialog via Ctrl+comma and rides the Commands menu', () => {
    const settings = getAction('app.settings')!;
    expect(settings.shortcut).toEqual({ key: ',', ctrlOrCmd: true });
    expect(menuGroups.find((g) => g.label === 'Commands')?.ids).toContain('app.settings');
    ui.settingsOpen = false;
    expect(runAction(settings)).toBe(true);
    expect(ui.settingsOpen).toBe(true);
    ui.settingsOpen = false;
    expect(actionForEvent({ key: ',', ctrlKey: true, metaKey: false, shiftKey: false })?.id).toBe(
      'app.settings',
    );
  });

  it('FER-366: no deferred actions remain — the registry is fully real', () => {
    // The consciously-deferred F2-27 entries graduated one by one:
    // show.customColumns (FER-365) and finally show.separateTree (FER-366).
    // Every registry action now has a handler and no disabledReason, so no
    // menu item can mislead by being visible-but-inert.
    expect(actions.length).toBeGreaterThan(0);
    for (const action of actions) {
      expect(action.disabledReason, action.id).toBeUndefined();
      expect(typeof action.run, action.id).toBe('function');
    }
    // The last graduate is enabled, shortcut-free by design, and searchable
    // in the palette like every other real action.
    const separate = getAction('show.separateTree')!;
    expect(separate.group).toBe('Show');
    expect(separate.shortcut).toBeUndefined();
    expect(separate.available?.() ?? true).toBe(true);
    expect(searchActions('separate tree').some((a) => a.id === 'show.separateTree')).toBe(true);
  });

  it('FER-590: New file is bound to Shift+F4 and opens the New-file dialog', () => {
    const action = getAction('file.newfile')!;
    expect(action.label).toBe('New file');
    expect(action.group).toBe('File');
    expect(action.shortcut).toEqual({ key: 'F4', shift: true });
    expect(isEnabled(action)).toBe(true);
    expect(searchActions('buat file').some((a) => a.id === 'file.newfile')).toBe(true);
  });

  it('FER-364: permanent delete is enabled on Shift+F8 and opens a pre-checked confirm', () => {
    const action = getAction('file.delete.permanent')!;
    expect(action.disabledReason).toBeUndefined();
    expect(action.shortcut).toEqual({ key: 'F8', shift: true });
    expect(isEnabled(action)).toBe(true);
    expect(runAction(action)).toBe(true);
    expect(dialogs.current).toMatchObject({ kind: 'delete', permanent: true });
    dialogs.close();
    // Trash stays the default delete path (D05).
    expect(runAction(getAction('file.delete')!)).toBe(true);
    expect(dialogs.current).toMatchObject({ kind: 'delete', permanent: false });
  });

  it('F2-12 integration: Files-menu pack/unpack/test run the archive engine', () => {
    // file.pack opens the pack dialog for the current selection.
    expect(runAction(getAction('file.pack')!)).toBe(true);
    expect(dialogs.current).toMatchObject({ kind: 'pack', format: 'zip' });
    dialogs.close();
    // file.unpack / file.testArchive are wired too — with a non-archive
    // selection they explain what to select instead of staying disabled.
    expect(isEnabled(getAction('file.unpack')!)).toBe(true);
    expect(runAction(getAction('file.unpack')!)).toBe(true);
    expect(dialogs.current).toMatchObject({
      kind: 'error',
      title: 'Unpack',
      message: expect.stringContaining('.zip, .7z or .rar'),
    });
    dialogs.close();
    expect(isEnabled(getAction('file.testArchive')!)).toBe(true);
    expect(runAction(getAction('file.testArchive')!)).toBe(true);
    expect(dialogs.current).toMatchObject({
      kind: 'error',
      title: 'Test archive',
      message: expect.stringContaining('.zip, .7z or .rar'),
    });
    dialogs.close();
    // The Alt-slot archive.* actions keep their shortcuts; the file.* menu
    // entries deliberately carry none (no shortcut collisions).
    expect(getAction('archive.pack')?.shortcut).toMatchObject({ key: 'F5', alt: true });
    expect(getAction('archive.unpack')?.shortcut).toMatchObject({ key: 'F9', alt: true });
    expect(getAction('archive.test')?.shortcut).toMatchObject({
      key: 'F9',
      alt: true,
      shift: true,
    });
    expect(getAction('file.pack')?.shortcut).toBeUndefined();
    expect(getAction('file.unpack')?.shortcut).toBeUndefined();
    expect(getAction('file.testArchive')?.shortcut).toBeUndefined();
  });

  it('guards context availability and invokes working action exactly once', () => {
    const unavailable = {
      id: 'test.unavailable',
      label: 'Unavailable',
      group: 'Test',
      available: () => false,
      run: vi.fn(),
    };
    expect(runAction(unavailable)).toBe(false);
    expect(unavailable.run).not.toHaveBeenCalled();
    const enabled = { id: 'test.enabled', label: 'Enabled', group: 'Test', run: vi.fn() };
    expect(runAction(enabled)).toBe(true);
    expect(enabled.run).toHaveBeenCalledOnce();
    // F2-25: Enter on a FILE now launches the OS association (folders navigate).
    expect(runAction(getAction('file.open')!)).toBe(true);
    expect(shellSpies.openFileExternal).toHaveBeenCalledWith('C:\\t\\a.txt');
    registerPane(0, { ...fakePane, selectedPaths: () => ['C:\\t\\a.txt', 'C:\\t\\b.txt'] });
    expect(isEnabled(getAction('file.rename')!)).toBe(false);
    expect(runAction(getAction('file.rename')!)).toBe(false);
    registerPane(0, { ...fakePane, selectedPaths: () => [] });
    expect(isEnabled(getAction('file.rename')!)).toBe(false);
    registerPane(0, fakePane);
    expect(isEnabled(getAction('file.rename')!)).toBe(true);
  });

  it('Enter on a supported archive enters it instead of launching the OS association (FER-450)', () => {
    for (const name of ['data.zip', 'DATA.7Z', 'pack.rar']) {
      const openCursorEntry = vi.fn();
      shellSpies.openFileExternal.mockClear();
      registerPane(0, {
        ...fakePane,
        cursorEntry: () => ({ name, path: `C:\\t\\${name}`, isDir: false }),
        openCursorEntry,
      });
      expect(labelOf(getAction('file.open')!)).toBe('Open archive');
      expect(runAction(getAction('file.open')!)).toBe(true);
      expect(openCursorEntry).toHaveBeenCalledOnce();
      expect(shellSpies.openFileExternal).not.toHaveBeenCalled();
    }
  });

  it('Enter on an unsupported archive type still uses the OS association', () => {
    const openCursorEntry = vi.fn();
    shellSpies.openFileExternal.mockClear();
    registerPane(0, {
      ...fakePane,
      cursorEntry: () => ({ name: 'data.tar', path: 'C:\\t\\data.tar', isDir: false }),
      openCursorEntry,
    });
    expect(runAction(getAction('file.open')!)).toBe(true);
    expect(openCursorEntry).not.toHaveBeenCalled();
    expect(shellSpies.openFileExternal).toHaveBeenCalledWith('C:\\t\\data.tar');
  });

  it('requires a pane and matching selected cursor before rename', () => {
    registerPane(0, { ...fakePane, selectedPaths: () => ['C:\\t\\b.txt'] });
    expect(isEnabled(getAction('file.rename')!)).toBe(false);
    expect(runAction(getAction('file.rename')!)).toBe(false);
    expect(dialogs.current).toBeNull();
    registerPane(0, fakePane);
    registerPane(1, fakePane)();
    ui.activePane = 1;
    for (const id of ['nav.up', 'tab.new', 'tab.close', 'file.mkdir']) {
      expect(runAction(getAction(id)!), id).toBe(false);
    }
    expect(dialogs.current).toBeNull();
  });

  it('does not start a transfer without a destination pane', () => {
    registerPane(1, fakePane)();
    for (const id of ['file.copy', 'file.move']) {
      expect(runAction(getAction(id)!), id).toBe(false);
    }
    expect(dialogs.current).toBeNull();
  });

  it('opens the multi-rename tool from Files with Ctrl+M on a selection (F2-20)', async () => {
    const action = getAction('file.multiRename')!;
    expect(action.disabledReason).toBeUndefined();
    expect(action.shortcut).toEqual({ key: 'm', ctrlOrCmd: true });
    expect(isEnabled(action)).toBe(true);
    expect(runAction(action)).toBe(true);
    const { multiRename } = await import('./stores/multiRenameStore.svelte');
    expect(multiRename.open).toBe(true);
    expect(multiRename.paths).toEqual(['C:\\t\\a.txt']);
    multiRename.close();
    expect(multiRename.open).toBe(false);
  });

  it('runs selected working actions through shared handlers', () => {
    expect(runAction(getAction('file.rename')!)).toBe(true);
    expect(dialogs.current).toEqual({ kind: 'rename', path: 'C:\\t\\a.txt', original: 'a.txt' });
    dialogs.close();
    expect(runAction(getAction('file.mkdir')!)).toBe(true);
    expect(dialogs.current).toEqual({ kind: 'mkdir', parent: 'C:\\t' });
    dialogs.close();
    expect(runAction(getAction('file.newfile')!)).toBe(true);
    expect(dialogs.current).toEqual({ kind: 'newfile', parent: 'C:\\t' });
    const wasHidden = ui.showHidden;
    expect(runAction(getAction('view.hidden')!)).toBe(true);
    expect(ui.showHidden).toBe(!wasHidden);
    ui.showHidden = wasHidden;
    expect(runAction(getAction('palette.open')!)).toBe(true);
    expect(ui.paletteOpen).toBe(true);
  });
  it('switches theme through menu actions and updates dynamic labels', () => {
    ui.setTheme('dark');
    const toggle = getAction('theme.toggle')!;
    expect(labelOf(toggle)).toBe('Switch to light theme');
    expect(runAction(toggle)).toBe(true);
    expect(ui.effectiveTheme()).toBe('light');
    expect(labelOf(toggle)).toBe('Switch to dark theme');
    expect(runAction(getAction('theme.dark')!)).toBe(true);
    expect(ui.effectiveTheme()).toBe('dark');
  });

  it('rereads active pane from Show without touching inactive pane', () => {
    const left = vi.fn();
    const right = vi.fn();
    registerPane(0, { ...fakePane, refresh: left });
    registerPane(1, { ...fakePane, refresh: right });
    const action = getAction('show.refresh')!;
    expect(runAction(action)).toBe(true);
    expect(left).toHaveBeenCalledOnce();
    expect(right).not.toHaveBeenCalled();
  });
  it('formats native Ctrl/Cmd labels and modifier combinations without binding placeholders', () => {
    expect(shortcutLabel({ key: 'k', ctrlOrCmd: true }, 'win32')).toBe('Ctrl+K');
    expect(shortcutLabel({ key: 'k', ctrlOrCmd: true }, 'darwin')).toBe('Cmd+K');
    expect(shortcutLabel({ key: 'F8', shift: true }, 'linux')).toBe('Shift+F8');
    expect(shortcutLabel({ key: 'F5', alt: true }, 'win32')).toBe('Alt+F5');
  });
  it('does not claim Cmd+H on macOS or dispatch native-hide shortcut', () => {
    const hidden = getAction('view.hidden')!;
    expect(shortcutFor(hidden, 'MacIntel')).toBeUndefined();
    expect(
      actionForEvent(
        { key: 'h', ctrlKey: false, metaKey: true, shiftKey: false },
        actions,
        'MacIntel',
      ),
    ).toBeUndefined();
    expect(shortcutFor(hidden, 'Win32')).toEqual({ key: 'h', ctrlOrCmd: true });
  });
  it('accepts only OS-native primary modifier', () => {
    expect(
      matches(
        { key: 'k', ctrlOrCmd: true },
        { key: 'k', ctrlKey: false, metaKey: true, shiftKey: false },
        'Win32',
      ),
    ).toBe(false);
    expect(
      matches(
        { key: 'k', ctrlOrCmd: true },
        { key: 'k', ctrlKey: true, metaKey: false, shiftKey: false },
        'MacIntel',
      ),
    ).toBe(false);
  });
});

describe('exact modifiers', () => {
  const event = { key: 'F5', ctrlKey: false, metaKey: false, shiftKey: false, altKey: false };
  it('rejects extra Alt, Shift, Ctrl, or Meta on plain shortcuts', () => {
    for (const modifier of ['altKey', 'shiftKey', 'ctrlKey', 'metaKey'] as const) {
      expect(matches({ key: 'F5' }, { ...event, [modifier]: true }), modifier).toBe(false);
    }
    expect(matches({ key: 'Tab' }, { ...event, key: 'Tab', shiftKey: true })).toBe(false);
  });

  it('requires Alt when declared and rejects additional modifiers', () => {
    expect(matches({ key: 'F5', alt: true }, { ...event, altKey: true })).toBe(true);
    expect(matches({ key: 'F5', alt: true }, event)).toBe(false);
    expect(matches({ key: 'F5', alt: true }, { ...event, altKey: true, ctrlKey: true })).toBe(
      false,
    );
    expect(matches({ key: 'F5', alt: true }, { ...event, altKey: true, shiftKey: true })).toBe(
      false,
    );
    expect(
      matches({ key: 'k', ctrlOrCmd: true }, { ...event, key: 'k', ctrlKey: true, metaKey: true }),
    ).toBe(false);
  });
});

describe('FER-429 menu restructure (owner option 2)', () => {
  // Every item the old 29-entry Show menu carried, with its shortcut before the restructure.
  const OLD_SHOW: [string, string | undefined][] = [
    ['show.brief', 'Ctrl+F1'],
    ['show.full', 'Ctrl+F2'],
    ['show.comments', undefined],
    ['show.customColumns', undefined],
    ['show.customModes', undefined],
    ['show.tree', 'Ctrl+F8'],
    ['show.separateTree', undefined],
    ['show.thumbnails', 'Ctrl+Shift+F2'],
    ['show.quickView', 'Ctrl+Q'],
    ['show.vertical', undefined],
    ['tab.new', 'Ctrl+T'],
    ['show.allFiles', undefined],
    ['show.programs', undefined],
    ['show.filter', undefined],
    ['show.selectedOnly', undefined],
    ['show.sortName', 'Ctrl+F3'],
    ['show.sortExtension', 'Ctrl+F4'],
    ['show.sortTime', 'Ctrl+F5'],
    ['show.sortSize', 'Ctrl+F6'],
    ['show.unsorted', 'Ctrl+F7'],
    ['show.reverse', undefined],
    ['show.refresh', 'Ctrl+R'],
    ['view.hidden', 'Ctrl+H'],
    ['view.aiChat', 'Ctrl+Shift+A'],
    ['view.aiSettings', undefined],
    ['theme.toggle', undefined],
    ['theme.dark', undefined],
    ['theme.light', undefined],
    ['theme.system', undefined],
  ];
  const inMenu = (id: string) => menuGroups.some((group) => group.ids.includes(id));
  const group = (label: string) => menuGroups.find((g) => g.label === label)!.ids;

  it('keeps every old Show item reachable and its shortcut unchanged', () => {
    for (const [id, shortcut] of OLD_SHOW) {
      const action = getAction(id);
      expect(action, id).toBeDefined();
      const label = action!.shortcut ? shortcutLabel(action!.shortcut, 'Win32') : undefined;
      expect(label, id).toBe(shortcut);
      // Theme and AI settings left the menus by design; they remain in the registry the palette searches.
      expect(inMenu(id) || actions.includes(action!), id).toBe(true);
    }
  });

  it('moves only theme actions and AI settings out of the menus', () => {
    const outside = OLD_SHOW.map(([id]) => id).filter((id) => !inMenu(id));
    expect(outside.sort()).toEqual([
      'theme.dark',
      'theme.light',
      'theme.system',
      'theme.toggle',
      'view.aiSettings',
    ]);
    // AI settings stays one click away through Commands > Settings.
    expect(group('Commands')).toContain('app.settings');
  });

  it('finds the actions that left the menus through the command palette', () => {
    ui.theme = 'dark';
    for (const query of [
      'Switch to light theme',
      'Use light theme',
      'Follow system theme',
      'AI assistant settings',
    ]) {
      expect(searchActions(query).length, query).toBeGreaterThan(0);
    }
  });

  it('splits View / Sort / Show and keeps each menu short enough not to scroll at 1440x900', () => {
    expect(group('Sort').filter((id) => id !== MENU_SEPARATOR)).toEqual([
      'show.sortName',
      'show.sortExtension',
      'show.sortTime',
      'show.sortSize',
      'show.unsorted',
      'show.reverse',
    ]);
    expect(group('Show')).toContain('view.aiChat');
    expect(group('View')).toContain('view.hidden');
    // Menu max-height is min(70vh, 600px) = 600px at 900px tall; rows are 28px, separators 9px, padding 8px.
    for (const label of ['View', 'Sort', 'Show']) {
      const ids = group(label);
      const seps = ids.filter((id) => id === MENU_SEPARATOR).length;
      expect((ids.length - seps) * 28 + seps * 9 + 8, label).toBeLessThanOrEqual(600);
      expect(ids[0]).not.toBe(MENU_SEPARATOR);
      expect(ids.at(-1)).not.toBe(MENU_SEPARATOR);
    }
  });
});

describe('FER-431 Files menu grouping', () => {
  const OLD_FILES = [
    'file.attributes',
    'file.pack',
    'file.unpack',
    'file.testArchive',
    'file.compareContent',
    'file.associate',
    'file.properties',
    'file.occupiedSpace',
    'file.multiRename',
    'file.comment',
    'file.print',
    'file.split',
    'file.combine',
    'file.encode',
    'file.decode',
    'file.createChecksum',
    'file.verifyChecksum',
    'file.archiveChecksum',
    'file.open',
    'file.rename',
    'file.copy',
    'file.move',
    'file.mkdir',
    'file.delete',
    'file.delete.permanent',
    'tab.close',
    'file.quit',
  ];
  const files = menuGroups.find((g) => g.label === 'Files')!.ids;
  const reachable = files.flatMap((id) => menuSubmenus[id]?.ids ?? [id]);

  it('keeps all 27 former Files actions reachable exactly once', () => {
    expect(OLD_FILES).toHaveLength(27);
    expect([...reachable.filter((id) => id !== MENU_SEPARATOR)].sort()).toEqual(
      [...OLD_FILES].sort(),
    );
    for (const id of OLD_FILES) expect(getAction(id), id).toBeDefined();
  });

  it('has few enough top-level rows to fit the 768px window without scrolling', () => {
    // Menu max-height is min(70vh, 600px) = 537px at 768px tall; rows 28px, separators 9px, padding 8px.
    const seps = files.filter((id) => id === MENU_SEPARATOR).length;
    const rows = files.length - seps;
    expect(rows).toBeLessThanOrEqual(16);
    expect(rows * 28 + seps * 9 + 8).toBeLessThanOrEqual(537);
    expect(files[0]).not.toBe(MENU_SEPARATOR);
    expect(files.at(-1)).not.toBe(MENU_SEPARATOR);
    expect(files.some((id, i) => id === MENU_SEPARATOR && files[i + 1] === MENU_SEPARATOR)).toBe(
      false,
    );
  });

  it('only references defined submenus, which hold registered actions and never nest', () => {
    for (const id of files.filter((x) => x.startsWith('submenu:'))) {
      expect(menuSubmenus[id], id).toBeDefined();
      for (const sub of menuSubmenus[id].ids) {
        expect(getAction(sub), sub).toBeDefined();
        expect(menuSubmenus[sub]).toBeUndefined();
      }
    }
  });

  it('keeps submenu actions findable in the command palette', () => {
    for (const query of ['Pack', 'Test archive', 'Properties', 'Create checksum']) {
      expect(searchActions(query).length, query).toBeGreaterThan(0);
    }
  });
});

describe('full-page shortcut guard (FER-561)', () => {
  const key = (k: string, extra: Record<string, boolean> = {}) => ({
    key: k,
    ctrlKey: false,
    metaKey: false,
    shiftKey: false,
    altKey: false,
    ...extra,
  });
  const blocked = (e: ReturnType<typeof key>) => {
    const action = actionForEvent(e);
    return action ? shortcutBlockedByFullPage(action) : undefined;
  };

  beforeEach(() => {
    ui.activityOpen = false;
    ui.syncProfilesOpen = false;
  });

  it('blocks nothing while no full page is open', () => {
    for (const a of actions) expect(shortcutBlockedByFullPage(a)).toBe(false);
  });

  it('blocks file shortcuts (F2, F5, F6, F7, F8, Shift+F8, Enter) while Activity is open', () => {
    ui.activityOpen = true;
    for (const e of [
      key('F2'),
      key('F5'),
      key('F6'),
      key('F7'),
      key('F8'),
      key('F8', { shiftKey: true }),
      key('Enter'),
    ]) {
      expect(blocked(e), e.key).toBe(true);
    }
  });

  it('keeps the palette, settings and Activity shortcuts working', () => {
    ui.activityOpen = true;
    expect(blocked(key('k', { ctrlKey: true }))).toBe(false);
    expect(blocked(key(',', { ctrlKey: true }))).toBe(false);
    expect(shortcutBlockedByFullPage(getAction('activity.open')!)).toBe(false);
    expect([...FULL_PAGE_SHORTCUT_IDS].sort()).toEqual([
      'activity.open',
      'app.settings',
      'palette.open',
      'sync.profiles.open',
    ]);
  });

  it('blocks file shortcuts while the Sync profiles page is open, allows its opener', () => {
    ui.syncProfilesOpen = true;
    for (const e of [key('F2'), key('F5'), key('F6'), key('F7'), key('F8'), key('Enter')]) {
      expect(blocked(e), e.key).toBe(true);
    }
    expect(blocked(key('k', { ctrlKey: true }))).toBe(false);
    expect(shortcutBlockedByFullPage(getAction('sync.profiles.open')!)).toBe(false);
  });

  it('every allowed id exists in the registry', () => {
    for (const id of FULL_PAGE_SHORTCUT_IDS) expect(getAction(id)).toBeDefined();
  });
});

describe('FER-577 B4 tab shortcuts', () => {
  it('Ctrl+Tab and Ctrl+Shift+Tab cycle tabs; plain Tab stays the pane switch', () => {
    const cycleTab = vi.fn();
    const off = registerPane(0, {
      path: 'C:/',
      selectedPaths: () => [],
      cursorEntry: () => undefined,
      refresh: () => {},
      newTab: () => {},
      closeCurrentTab: () => {},
      goUp: () => {},
      openCursorEntry: () => {},
      cycleTab,
    });
    const ev = (shift: boolean, ctrl = true) => ({
      key: 'Tab',
      ctrlKey: ctrl,
      metaKey: false,
      shiftKey: shift,
      altKey: false,
    });
    const next = actionForEvent(ev(false), actions, 'Win32');
    const prev = actionForEvent(ev(true), actions, 'Win32');
    expect(next?.id).toBe('tab.next');
    expect(prev?.id).toBe('tab.prev');
    next?.run?.();
    prev?.run?.();
    expect(cycleTab.mock.calls).toEqual([[1], [-1]]);
    expect(actionForEvent(ev(false, false), actions, 'Win32')?.id).toBe('pane.switch');
    off();
  });
});
