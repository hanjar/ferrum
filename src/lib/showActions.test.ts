/**
 * F2-15 — Show menu actions through the shared registry: per-pane view
 * mode, sorting, filters, tree, vertical arrangement, reread.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { actionForEvent, getAction, isEnabled, labelOf, runAction, searchActions } from './actions';
import { registerPane, type PaneApi } from './stores/panesRegistry.svelte';
import { viewState } from './stores/viewStateStore.svelte';
import { columns } from './stores/columnsStore.svelte';
import { separateTree } from './stores/separateTreeStore.svelte';
import { dialogs } from './stores/dialogStore.svelte';
import { ui } from './uiState.svelte';

function fakePane(): PaneApi {
  return {
    path: 'C:\\dir',
    selectedPaths: () => [],
    cursorEntry: () => undefined,
    refresh: vi.fn(),
    newTab: () => {},
    closeCurrentTab: () => {},
    goUp: () => {},
    openCursorEntry: () => {},
  };
}

let left: PaneApi;
let right: PaneApi;

beforeEach(() => {
  left = fakePane();
  right = fakePane();
  registerPane(0, left);
  registerPane(1, right);
  ui.activePane = 0;
  ui.sidebarOpen = true;
  ui.sidebarMode = 'places';
  ui.verticalLayout = false;
  dialogs.close();
  for (const p of [0, 1] as const) {
    viewState.setViewMode(p, 'full');
    viewState.setSort(p, 'name', 1);
    viewState.setFilterMode(p, 'all');
    if (viewState.reversed(p)) viewState.toggleReversed(p);
  }
});

afterEach(() => {
  columns.resetAll(); // dialog flag + persisted config back to defaults
  separateTree.resetAll(); // per-pane toggles + persisted expansion back to OFF
});

function run(id: string): void {
  expect(runAction(getAction(id)!), id).toBe(true);
}

describe('show mode actions (per pane)', () => {
  it('Brief/Full switch only the active pane', () => {
    run('show.brief');
    expect(viewState.viewMode(0)).toBe('brief');
    expect(viewState.viewMode(1)).toBe('full');
    ui.activePane = 1;
    run('show.brief');
    expect(viewState.viewMode(1)).toBe('brief');
    ui.activePane = 0;
    run('show.full');
    expect(viewState.viewMode(0)).toBe('full');
  });

  it('sorting actions follow the TC shortcut slots and stay per pane', () => {
    expect(getAction('show.sortName')!.shortcut).toEqual({ key: 'F3', ctrlOrCmd: true });
    expect(getAction('show.sortExtension')!.shortcut).toEqual({ key: 'F4', ctrlOrCmd: true });
    expect(getAction('show.sortTime')!.shortcut).toEqual({ key: 'F5', ctrlOrCmd: true });
    expect(getAction('show.sortSize')!.shortcut).toEqual({ key: 'F6', ctrlOrCmd: true });
    expect(getAction('show.unsorted')!.shortcut).toEqual({ key: 'F7', ctrlOrCmd: true });

    run('show.sortSize');
    expect(viewState.sort(0)).toEqual({ key: 'size', dir: 1 });
    // same key again reverses direction (like clicking the column header)
    run('show.sortSize');
    expect(viewState.sort(0)).toEqual({ key: 'size', dir: -1 });
    expect(viewState.sort(1)).toEqual({ key: 'name', dir: 1 });

    // Ctrl+F6 dispatches to the sort action; plain F5 still copies when a
    // selection exists (exact-modifier matching keeps them apart).
    expect(actionForEvent({ key: 'F6', ctrlKey: true, metaKey: false, shiftKey: false })?.id).toBe(
      'show.sortSize',
    );
    registerPane(0, {
      ...fakePane(),
      selectedPaths: () => ['C:\\dir\\a.txt'],
    });
    expect(actionForEvent({ key: 'F5', ctrlKey: false, metaKey: false, shiftKey: false })?.id).toBe(
      'file.copy',
    );
  });

  it('unsorted keeps the listing order via the sort key', () => {
    run('show.unsorted');
    expect(viewState.sort(0).key).toBe('unsorted');
  });

  it('reverse order toggles and reports its state in the label', () => {
    expect(labelOf(getAction('show.reverse')!)).toBe('Reversed Order');
    run('show.reverse');
    expect(viewState.reversed(0)).toBe(true);
    expect(viewState.reversed(1)).toBe(false);
    expect(labelOf(getAction('show.reverse')!)).toBe('Normal Order');
    run('show.reverse');
    expect(viewState.reversed(0)).toBe(false);
  });
});

describe('show filter actions', () => {
  it('All Files / Programs presets apply to the active pane', () => {
    run('show.programs');
    expect(viewState.filterMode(0)).toBe('programs');
    run('show.allFiles');
    expect(viewState.filterMode(0)).toBe('all');
    expect(viewState.filterMode(1)).toBe('all');
  });

  it('Only Selected Files toggles back to all with an explanatory label', () => {
    expect(labelOf(getAction('show.selectedOnly')!)).toBe('Only Selected Files');
    run('show.selectedOnly');
    expect(viewState.filterMode(0)).toBe('selected');
    expect(labelOf(getAction('show.selectedOnly')!)).toBe('Show All Files (leave Only Selected)');
    run('show.selectedOnly');
    expect(viewState.filterMode(0)).toBe('all');
  });

  it('Custom Filter opens the mask dialog for the active pane', () => {
    run('show.filter');
    expect(dialogs.current).toMatchObject({ kind: 'showFilter', parent: 'C:\\dir' });
  });
});

describe('layout and reread', () => {
  it('Tree opens the sidebar in tree mode', () => {
    ui.sidebarOpen = false;
    run('show.tree');
    expect(ui.sidebarOpen).toBe(true);
    expect(ui.sidebarMode).toBe('tree');
  });

  it('Vertical Arrangement toggles the shell grid', () => {
    run('show.vertical');
    expect(ui.verticalLayout).toBe(true);
    run('show.vertical');
    expect(ui.verticalLayout).toBe(false);
  });

  it('Reread Source refreshes only the active pane via Ctrl+R', () => {
    expect(getAction('show.refresh')!.shortcut).toEqual({ key: 'r', ctrlOrCmd: true });
    run('show.refresh');
    expect(left.refresh).toHaveBeenCalledOnce();
    expect(right.refresh).not.toHaveBeenCalled();
  });

  it('Separate Tree toggles the ACTIVE pane only (FER-366, opens D33)', () => {
    const action = getAction('show.separateTree')!;
    // No longer a deferred stub: it runs, has no shortcut (the spec assigns
    // none), and its label mirrors the state of the ACTIVE pane.
    expect(action.disabledReason).toBeUndefined();
    expect(action.shortcut).toBeUndefined();
    expect(isEnabled(action)).toBe(true);
    expect(labelOf(action)).toBe('Separate Tree');

    run('show.separateTree');
    expect(separateTree.isEnabled(0)).toBe(true);
    expect(separateTree.isEnabled(1)).toBe(false);
    expect(labelOf(action)).toBe('Hide Separate Tree');

    // Pane isolation: the other pane keeps its own toggle state.
    ui.activePane = 1;
    expect(labelOf(action)).toBe('Separate Tree');
    run('show.separateTree');
    expect(separateTree.isEnabled(1)).toBe(true);
    expect(separateTree.isEnabled(0)).toBe(true);
    run('show.separateTree');
    expect(separateTree.isEnabled(1)).toBe(false);
    expect(separateTree.isEnabled(0)).toBe(true);
    ui.activePane = 0;
  });

  it('Custom Columns opens its own config dialog (FER-365, opens D33)', () => {
    const action = getAction('show.customColumns')!;
    // No longer a deferred stub: it runs, so it appears in the palette too.
    expect(action.disabledReason).toBeUndefined();
    expect(isEnabled(action)).toBe(true);
    expect(searchActions('custom columns').some((a) => a.id === 'show.customColumns')).toBe(true);
    expect(columns.open).toBe(false);
    run('show.customColumns');
    expect(columns.open).toBe(true);
    columns.close();
    expect(columns.open).toBe(false);
  });

  it('Comments toggles the shared column switch with an honest label (F2-27)', () => {
    const action = getAction('show.comments')!;
    expect(action.disabledReason).toBeUndefined();
    ui.showComments = false;
    expect(labelOf(action)).toBe('Comments');
    run('show.comments');
    expect(ui.showComments).toBe(true);
    expect(labelOf(action)).toBe('Comments (hide)');
    run('show.comments');
    expect(ui.showComments).toBe(false);
  });
});
