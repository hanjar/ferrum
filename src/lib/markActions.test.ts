/**
 * F2-14 — Mark menu actions through the shared registry: a fake pane feeds
 * plain-data entries and records the selection actions apply, mirroring how
 * FilePane registers itself.
 */
import { beforeEach, describe, expect, it, vi } from 'vitest';
import {
  actionForEvent,
  actions,
  getAction,
  isEnabled,
  labelOf,
  matches,
  menuGroups,
  runAction,
  searchActions,
} from './actions';
import { registerPane, type PaneApi, type PaneEntryInfo } from './stores/panesRegistry.svelte';
import { dialogs } from './stores/dialogStore.svelte';
import { clearSelectionMemory, restoreSelection } from './stores/selectionMemory.svelte';

function entry(name: string, isDir = false): PaneEntryInfo {
  return {
    name,
    path: `C:\\dir\\${name}`,
    isDir,
    size: isDir ? 0 : 10,
    modifiedMs: 1_790_000_000_000,
  };
}

const ENTRIES: PaneEntryInfo[] = [
  entry('report.txt'),
  entry('notes.txt'),
  entry('photo.JPG'),
  entry('Makefile'),
  entry('docs', true),
];

/** Pane fake whose selection is readable through the same PaneApi contract
 *  the real FilePane implements. */
function markPane(selected: string[] = [], path = 'C:\\dir'): PaneApi {
  const state = { path, selection: [...selected], cursor: ENTRIES[0] };
  return {
    get path() {
      return state.path;
    },
    selectedPaths: () => state.selection,
    cursorEntry: () =>
      state.cursor
        ? { name: state.cursor.name, path: state.cursor.path, isDir: state.cursor.isDir }
        : undefined,
    refresh: () => {},
    newTab: () => {},
    closeCurrentTab: () => {},
    goUp: () => {},
    openCursorEntry: () => {},
    visibleEntries: () => ENTRIES,
    selectPaths: (paths) => {
      state.selection = [...paths];
    },
  };
}

function clipboardStub(): { texts: string[] } {
  const texts: string[] = [];
  Object.defineProperty(globalThis.navigator, 'clipboard', {
    configurable: true,
    value: {
      writeText: vi.fn(async (t: string) => {
        texts.push(t);
      }),
    },
  });
  return { texts };
}

let pane: ReturnType<typeof markPane>;

beforeEach(() => {
  clearSelectionMemory();
  dialogs.close();
  pane = markPane(['C:\\dir\\report.txt']);
  registerPane(0, pane);
  registerPane(1, markPane());
  // Real async clipboard stand-in so copy actions never hit the fallback.
  Object.defineProperty(globalThis.navigator, 'clipboard', {
    configurable: true,
    value: { writeText: vi.fn(async () => undefined) },
  });
});

describe('mark action availability and ordering', () => {
  it('all F2-14 Mark actions are registry-enabled; FER-367 binds the two TC Num-key chords', () => {
    for (const id of [
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
      'mark.copyNames',
      'mark.copyPaths',
      'mark.copyDetails',
      'mark.copyPathDetails',
    ]) {
      const action = getAction(id)!;
      expect(action.disabledReason, id).toBeUndefined();
      expect(action.run, id).toBeTypeOf('function');
      expect(isEnabled(action), id).toBe(true);
      expect(runAction(action), id).toBe(true);
    }
    // FER-367: the two TC Num-key chords live on the canonical actions.
    expect(getAction('mark.invert')!.shortcut).toEqual({ key: '*', shift: 'any' });
    expect(getAction('mark.sameExtension')!.shortcut).toEqual({ key: '+', shift: 'any' });
    // Screenshot order inside the Mark menu.
    const ids = menuGroups.find((g) => g.label === 'Mark')!.ids;
    expect(ids.indexOf('mark.selectGroup')).toBeLessThan(ids.indexOf('mark.unselectGroup'));
    expect(ids.indexOf('mark.invert')).toBeLessThan(ids.indexOf('mark.sameExtension'));
    expect(ids.indexOf('mark.copyNames')).toBeLessThan(ids.indexOf('mark.copyPaths'));
  });

  it('appears in the palette; copy actions need a selection', () => {
    expect(searchActions('Invert Selection').map((a) => a.id)).toContain('mark.invert');
    registerPane(0, markPane([]));
    expect(isEnabled(getAction('mark.copyNames')!)).toBe(false);
    expect(runAction(getAction('mark.copyNames')!)).toBe(false);
  });
});

describe('mark selection operations', () => {
  it('Select All selects every visible entry; Unselect All clears', () => {
    runAction(getAction('mark.selectAll')!);
    expect(pane.selectedPaths()).toEqual(ENTRIES.map((e) => e.path));
    runAction(getAction('mark.unselectAll')!);
    expect(pane.selectedPaths()).toEqual([]);
  });

  it('Invert Selection keeps only unselected entries', () => {
    pane = markPane(['C:\\dir\\report.txt', 'C:\\dir\\notes.txt']);
    registerPane(0, pane);
    runAction(getAction('mark.invert')!);
    expect([...pane.selectedPaths()].sort()).toEqual([
      'C:\\dir\\Makefile',
      'C:\\dir\\docs',
      'C:\\dir\\photo.JPG',
    ]);
  });

  it('Select All With Same Extension matches files, case-insensitively', () => {
    runAction(getAction('mark.sameExtension')!); // cursor = report.txt
    expect([...pane.selectedPaths()].sort()).toEqual(['C:\\dir\\notes.txt', 'C:\\dir\\report.txt']);
  });

  it('Save Selection stores basenames; Restore re-applies by name', () => {
    pane = markPane(['C:\\dir\\report.txt', 'C:\\dir\\docs']);
    registerPane(0, pane);
    runAction(getAction('mark.save')!);
    expect(restoreSelection('C:\\dir')).toEqual(['report.txt', 'docs']);
    pane = markPane([]); // selection got cleared (e.g. refresh)
    registerPane(0, pane);
    runAction(getAction('mark.restore')!);
    expect([...pane.selectedPaths()].sort()).toEqual(['C:\\dir\\docs', 'C:\\dir\\report.txt']);
    expect(dialogs.current).toBeNull(); // full success stays silent
  });

  it('Restore in another folder explains that nothing was saved there', () => {
    registerPane(0, markPane(['C:\\dir\\report.txt']));
    runAction(getAction('mark.save')!);
    registerPane(0, markPane([], 'C:\\other'));
    runAction(getAction('mark.restore')!);
    expect(dialogs.current).toMatchObject({ kind: 'error', title: 'Restore Selection' });
  });
});

describe('mark clipboard actions', () => {
  it('copies selected names and paths as plain text', async () => {
    const clip = clipboardStub();
    registerPane(0, markPane(['C:\\dir\\report.txt', 'C:\\dir\\docs']));
    runAction(getAction('mark.copyNames')!);
    runAction(getAction('mark.copyPaths')!);
    await vi.waitFor(() => expect(clip.texts.length).toBe(2));
    expect(clip.texts[0]).toBe('report.txt\ndocs');
    expect(clip.texts[1]).toBe('C:\\dir\\report.txt\nC:\\dir\\docs');
  });

  it('copies details as TSV with size and modification time', async () => {
    const clip = clipboardStub();
    registerPane(0, markPane(['C:\\dir\\report.txt']));
    runAction(getAction('mark.copyDetails')!);
    runAction(getAction('mark.copyPathDetails')!);
    await vi.waitFor(() => expect(clip.texts.length).toBe(2));
    expect(clip.texts[0]).toMatch(/^report\.txt\t10\t\d{4}-\d{2}-\d{2} \d{2}:\d{2}$/);
    expect(clip.texts[1]).toMatch(/^C:\\dir\\report\.txt\t10\t\d{4}-\d{2}-\d{2} \d{2}:\d{2}$/);
  });

  it('clipboard actions are unavailable without a selection', () => {
    registerPane(0, markPane([]));
    for (const id of [
      'mark.copyNames',
      'mark.copyPaths',
      'mark.copyDetails',
      'mark.copyPathDetails',
    ]) {
      expect(isEnabled(getAction(id)!), id).toBe(false);
    }
  });
});

describe('select group dialog flow', () => {
  it('Select and Unselect Group open the pattern dialog for the pane folder', () => {
    runAction(getAction('mark.selectGroup')!);
    expect(dialogs.current).toMatchObject({
      kind: 'selectGroup',
      unselect: false,
      parent: 'C:\\dir',
    });
    runAction(getAction('mark.unselectGroup')!);
    expect(dialogs.current).toMatchObject({ kind: 'selectGroup', unselect: true });
  });

  it('same-extension action is unavailable without a cursor', () => {
    registerPane(0, {
      ...markPane(),
      cursorEntry: () => undefined,
    } as PaneApi);
    expect(isEnabled(getAction('mark.sameExtension')!)).toBe(false);
    expect(labelOf(getAction('mark.sameExtension')!)).toBe('Select All With Same Extension');
  });
});

// FER-367: TC Num-key chords route through the shared registry matcher.
// Browser key facts the bindings rely on: the top row with Shift produces
// key '*' (Shift+Digit8) and '+' (Shift+Equal); the numpad produces the SAME
// key values with or without Shift (NumpadMultiply/NumpadAdd), hence
// `shift: 'any'`.
describe('FER-367: TC Num-key shortcuts on canonical mark actions', () => {
  const base = { ctrlKey: false, metaKey: false, altKey: false };

  it('Shift+* (top row, Shift+Digit8) inverts the selection', () => {
    pane = markPane(['C:\\dir\\report.txt', 'C:\\dir\\notes.txt']);
    registerPane(0, pane);
    const hit = actionForEvent({ key: '*', ...base, shiftKey: true });
    expect(hit?.id).toBe('mark.invert');
    runAction(hit!);
    expect([...pane.selectedPaths()].sort()).toEqual([
      'C:\\dir\\Makefile',
      'C:\\dir\\docs',
      'C:\\dir\\photo.JPG',
    ]);
  });

  it('numpad * fires invert with or without Shift (NumpadMultiply → key "*")', () => {
    pane = markPane(['C:\\dir\\report.txt']);
    registerPane(0, pane);
    expect(actionForEvent({ key: '*', ...base, shiftKey: false })?.id).toBe('mark.invert');
    expect(actionForEvent({ key: '*', ...base, shiftKey: true })?.id).toBe('mark.invert');
  });

  it('Shift++ (top row Shift+Equal) and numpad + select the same extension', () => {
    for (const shiftKey of [true, false]) {
      const hit = actionForEvent({ key: '+', ...base, shiftKey });
      expect(hit?.id, `shiftKey=${shiftKey}`).toBe('mark.sameExtension');
    }
    const hit = actionForEvent({ key: '+', ...base, shiftKey: true });
    runAction(hit!); // cursor = report.txt
    expect([...pane.selectedPaths()].sort()).toEqual(['C:\\dir\\notes.txt', 'C:\\dir\\report.txt']);
  });

  it('no extra modifier hijacks the chords: Ctrl/Alt+* and Ctrl/Alt++ never match', () => {
    expect(actionForEvent({ key: '*', ...base, shiftKey: true, ctrlKey: true })).toBeUndefined();
    expect(actionForEvent({ key: '*', ...base, shiftKey: true, altKey: true })).toBeUndefined();
    expect(actionForEvent({ key: '+', ...base, shiftKey: true, ctrlKey: true })).toBeUndefined();
    expect(actionForEvent({ key: '+', ...base, shiftKey: true, altKey: true })).toBeUndefined();
  });

  it('the chords stay unique against every other registry shortcut', () => {
    for (const [key, shiftKey] of [
      ['*', true],
      ['*', false],
      ['+', true],
      ['+', false],
    ] as const) {
      const hits = actions
        .filter((a) => a.shortcut && matches(a.shortcut, { key, ...base, shiftKey }))
        .map((a) => a.id);
      expect(new Set(hits).size, `${key} shift=${shiftKey}`).toBe(hits.length);
    }
  });
});
