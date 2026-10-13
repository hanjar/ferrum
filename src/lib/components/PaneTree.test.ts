/**
 * FER-366 Separate Tree — component behaviour through FilePane: per-pane
 * placement above the list, lazy expansion via the shared TreeSidebar,
 * cwd two-way sync, pane isolation, and persistence. Runs in browser mode,
 * so listings come from the prototype mock fetcher.
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render } from '@testing-library/svelte';
import { tick } from 'svelte';
import FilePane from './FilePane.svelte';
import { getAction, runAction } from '../actions';
import { separateTree } from '../stores/separateTreeStore.svelte';
import { sidebarTree } from '../stores/treeStore.svelte';
import { ui } from '../uiState.svelte';

const KEY = 'ferrum.separateTree.v1';

interface Mounted {
  container: HTMLElement;
}

let left: Mounted;
let right: Mounted;

async function boot() {
  left = render(FilePane, { paneId: 0 });
  right = render(FilePane, { paneId: 1 });
  await vi.waitFor(() =>
    expect(left.container.querySelectorAll('.row[data-path]').length).toBeGreaterThan(5),
  );
  await vi.waitFor(() =>
    expect(right.container.querySelectorAll('.row[data-path]').length).toBeGreaterThan(3),
  );
}

/** The pane's separate tree element (null when the pane's tree is off). */
function treeOf(m: Mounted): HTMLElement | null {
  return m.container.querySelector('[role="tree"]');
}

function hasTreeRow(m: Mounted, name: string): boolean {
  const tree = treeOf(m);
  return (
    tree !== null &&
    Array.from(tree.querySelectorAll('[role="treeitem"]')).some((el) =>
      el.textContent?.includes(name),
    )
  );
}

function treeRow(m: Mounted, name: string): HTMLElement {
  const tree = treeOf(m);
  if (!tree) throw new Error('separate tree not rendered');
  const row = Array.from(tree.querySelectorAll('[role="treeitem"]')).find((el) =>
    el.textContent?.includes(name),
  );
  if (!row) throw new Error(`tree row not found: ${name}`);
  return row as HTMLElement;
}

function chevOf(m: Mounted, name: string): HTMLButtonElement {
  const chev = treeRow(m, name).querySelector<HTMLButtonElement>('button.chev');
  if (!chev) throw new Error(`chevron not found: ${name}`);
  return chev;
}

/** Toggle the ACTIVE pane's separate tree through the registry action and
 *  let Svelte flush the conditional PaneTree block. */
async function toggleSeparateTree(): Promise<void> {
  expect(runAction(getAction('show.separateTree')!)).toBe(true);
  await tick();
}

afterEach(() => {
  cleanup();
  ui.activePane = 0;
  separateTree.resetAll();
});

describe('separate tree placement and toggle', () => {
  it('renders no tree by default — zero-config panes are unchanged', async () => {
    await boot();
    expect(treeOf(left)).toBeNull();
    expect(treeOf(right)).toBeNull();
  });

  it('the registry action shows the ACTIVE pane tree only, above its list', async () => {
    await boot();
    await toggleSeparateTree();

    // Placement (TC): the tree sits inside the pane, between the pane's
    // toolbar and its file list. (Since FER-322 the pane content lives in a
    // .filesMode wrapper — Disk usage swaps it as a pane mode — so the tree
    // is matched as a descendant; the toolbar/tree/list order still holds.)
    const paneTree = left.container.querySelector('section[data-pane="0"] .paneTree');
    expect(paneTree).toBeTruthy();
    expect(paneTree!.previousElementSibling?.classList.contains('toolbar')).toBe(true);
    expect(paneTree!.nextElementSibling?.classList.contains('head')).toBe(true);

    // Pane 1 is unaffected.
    expect(treeOf(right)).toBeNull();
    expect(separateTree.isEnabled(1)).toBe(false);
  });

  it('toggling off removes the panel', async () => {
    await boot();
    await toggleSeparateTree();
    expect(treeOf(left)).toBeTruthy();
    await toggleSeparateTree();
    expect(treeOf(left)).toBeNull();
    expect(separateTree.isEnabled(0)).toBe(false);
  });
});

describe('separate tree lazy expansion and navigation', () => {
  it('expands lazily (mock-served children), collapses, and keeps the cache', async () => {
    await boot();
    await toggleSeparateTree();
    await vi.waitFor(() => expect(hasTreeRow(left, 'Projects')).toBe(true));

    const tree = separateTree.trees[0];
    fireEvent.click(chevOf(left, 'Projects'));
    await vi.waitFor(() => expect(hasTreeRow(left, 'datasets')).toBe(true));
    expect(tree.childrenOf('C:\\Projects').map((e) => e.name)).toEqual([
      'archive-2025',
      'atlas-design-system',
      'datasets',
      'helix-engine',
      'orbit-web',
      'renders',
    ]);
    const fetched = tree.childrenOf('C:\\Projects').length;

    // Collapse hides; the cache stays, so re-expanding refetches nothing.
    fireEvent.click(chevOf(left, 'Projects'));
    await vi.waitFor(() => expect(hasTreeRow(left, 'datasets')).toBe(false));
    fireEvent.click(chevOf(left, 'Projects'));
    await vi.waitFor(() => expect(hasTreeRow(left, 'datasets')).toBe(true));
    expect(tree.childrenOf('C:\\Projects')).toHaveLength(fetched);
  });

  it('clicking a tree folder navigates only its own pane and highlights the node', async () => {
    await boot();
    await toggleSeparateTree();
    await vi.waitFor(() => expect(hasTreeRow(left, 'Projects')).toBe(true));
    fireEvent.click(chevOf(left, 'Projects'));
    await vi.waitFor(() => expect(hasTreeRow(left, 'datasets')).toBe(true));
    fireEvent.click(treeRow(left, 'datasets'));

    // The pane's own tree selected + expanded the ancestors of the cwd.
    await vi.waitFor(() => expect(separateTree.trees[0].selected).toBe('C:\\Projects\\datasets'));
    expect(separateTree.trees[0].expanded.has('C:\\Projects')).toBe(true);
    // The pane navigated (crumb reflects the new directory).
    await vi.waitFor(() =>
      expect(left.container.querySelector('.crumbs')?.textContent).toContain('datasets'),
    );

    // The other pane never moved and has no tree of its own.
    expect(treeOf(right)).toBeNull();
    expect(separateTree.trees[1].selected).toBeNull();
    expect(separateTree.trees[1].expanded.size).toBe(0);
    expect(right.container.querySelector('.crumbs')?.textContent).toContain('Backup');
    // The pane trees are private stores, not the global sidebar tree.
    expect(separateTree.trees[0]).not.toBe(sidebarTree);
  });

  it('the pane tree follows the pane cwd as it navigates (two-way)', async () => {
    await boot();
    await toggleSeparateTree();

    // Navigate by double-clicking the FILE LIST row (not the tree).
    const row = Array.from(left.container.querySelectorAll('.row[data-path]')).find(
      (el) => el.getAttribute('data-path') === 'C:\\Projects\\datasets',
    );
    expect(row).toBeTruthy();
    fireEvent.dblClick(row as HTMLElement);
    await vi.waitFor(() => expect(separateTree.trees[0].selected).toBe('C:\\Projects\\datasets'));
    // Ancestors expanded in the pane's own tree; 'C:\Projects' is a visible
    // root (the mock drive root above it has no rendered row).
    expect(separateTree.trees[0].expanded.has('C:\\Projects')).toBe(true);
  });
});

describe('separate tree persistence', () => {
  it('persists the per-pane toggle and expansion to localStorage', async () => {
    await boot();
    await toggleSeparateTree();
    await vi.waitFor(() => expect(hasTreeRow(left, 'Projects')).toBe(true));
    fireEvent.click(chevOf(left, 'Projects'));
    await vi.waitFor(() => expect(hasTreeRow(left, 'datasets')).toBeTruthy());

    await vi.waitFor(() => {
      const saved = JSON.parse(globalThis.localStorage!.getItem(KEY) ?? 'null');
      expect(saved?.enabled).toEqual([true, false]);
      expect(saved?.expanded?.['0']).toContain('C:\\Projects');
    });
  });
});
