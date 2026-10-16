/**
 * DSK-04.2 — DiskUsageTree component tests: lazy pagination UI, ARIA tree
 * semantics and the keyboard model. The store singleton is preloaded via
 * testData/direct fields; the IPC wrapper module is mocked so expand clicks
 * run against a controllable backend.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render } from '@testing-library/svelte';
import { tick } from 'svelte';

const duMocks = vi.hoisted(() => ({
  start: vi.fn(),
  cancel: vi.fn(),
  snapshot: vi.fn(),
  queryChildren: vi.fn(),
  topFiles: vi.fn(),
}));
vi.mock('../ipc/diskUsage', () => ({
  diskUsageStart: duMocks.start,
  diskUsageCancel: duMocks.cancel,
  diskUsageSnapshot: duMocks.snapshot,
  diskUsageQueryChildren: duMocks.queryChildren,
  diskUsageTopFiles: duMocks.topFiles,
  onDiskUsageEvent: vi.fn(async () => () => {}),
  diskUsageCacheLoad: vi.fn(async () => null),
  diskUsageCacheDelete: vi.fn(async () => 0),
  diskUsageCacheList: vi.fn(async () => []),
  diskUsageCacheNoteMutation: vi.fn(async () => undefined),
}));

// DSK-06: the row context actions revalidate through entry_properties; the
// light stat is mocked so Delete/Move from the menu run without a Tauri host.
const propsMocks = vi.hoisted(() => ({
  entryProperties: vi.fn(),
  occupiedSpace: vi.fn(),
  volumeInfo: vi.fn(),
  setAttributes: vi.fn(),
}));
vi.mock('../ipc/properties', () => propsMocks);

// DSK-10: the tree takes its store instance as a prop (DSK-11: the ONE
// workspace session store — tests build their own).
const { createDiskUsageStore } = await import('./diskUsageStore.svelte');
const diskUsage = createDiskUsageStore();
const DiskUsageTree = (await import('./DiskUsageTree.svelte')).default;

/** Render the tree against this file's store. */
function renderTree() {
  return render(DiskUsageTree, { store: diskUsage });
}
const { contextMenu } = await import('../stores/contextMenuStore.svelte');
const { dialogs } = await import('../stores/dialogStore.svelte');
import type { DiskUsageChildItem, DiskUsageNode } from '../ipc/types';

const ROOT = 'C:\\data';

function item(id: string, over: Partial<DiskUsageChildItem> = {}): DiskUsageChildItem {
  return {
    id,
    name: id,
    kind: 'dir',
    logicalBytes: 100,
    allocatedBytes: 128,
    allocatedSupported: true,
    category: 'other',
    percentOfParent: null,
    percentOfScanRoot: null,
    ...over,
  };
}

function node(id: string, over: Partial<DiskUsageNode> = {}): DiskUsageNode {
  return {
    id,
    path: id,
    name: id,
    childrenIds: [],
    directFiles: 0,
    directSymlinks: 0,
    directLogicalBytes: 0,
    directAllocatedBytes: 0,
    subtreeFiles: 0,
    subtreeDirs: 0,
    subtreeSymlinks: 0,
    subtreeLogicalBytes: 0,
    subtreeAllocatedBytes: 0,
    subtreeErrors: 0,
    skippedVolumeBoundaries: 0,
    subtreeSkippedVolumes: 0,
    ...over,
  };
}

function preload(): void {
  diskUsage.testData('s1', 'complete', ROOT);
  diskUsage.snapshot = {
    scanId: 's1',
    rootPath: ROOT,
    rootId: ROOT,
    version: 3,
    scannedAtMs: 1,
    rootNode: node(ROOT, { name: 'data', subtreeFiles: 5, subtreeDirs: 2, subtreeErrors: 2 }),
    stats: {
      scannedFiles: 5,
      scannedDirs: 2,
      scannedSymlinks: 0,
      skippedVolumeBoundaries: 0,
      logicalBytes: 500,
      allocatedAvailableFiles: 5,
      allocatedUnavailableFiles: 0,
      knownAllocatedBytes: 640,
      totalErrors: 2,
      coverage: 'partial',
      cancelled: false,
      depthCapped: false,
      durationMs: 5,
    },
  };
}

beforeEach(() => {
  diskUsage.reset();
  duMocks.queryChildren.mockReset();
  duMocks.topFiles.mockReset().mockResolvedValue({
    scanId: 's1',
    nodeId: ROOT,
    version: 3,
    scannedAtMs: 1,
    metric: 'logical',
    coverage: 'complete',
    limit: 20,
    totalMatching: 0,
    truncated: false,
    label: 'Top 20 files',
    items: [],
  });
});

afterEach(() => {
  cleanup();
  contextMenu.close();
  dialogs.close();
});

describe('rendering + ARIA', () => {
  it('renders a role=tree with root treeitem at level 1 and error badges', async () => {
    preload();
    duMocks.queryChildren.mockResolvedValue({
      scanId: 's1',
      nodeId: ROOT,
      version: 3,
      scannedAtMs: 1,
      metric: 'logical',
      coverage: 'partial',
      offset: 0,
      limit: 50,
      total: 1,
      items: [item('movies', { percentOfParent: 50 })],
    });
    const { container } = renderTree();
    await diskUsage.loadChildren(ROOT);
    await tick();
    const tree = container.querySelector('[role="tree"]');
    expect(tree).not.toBeNull();
    expect(tree?.getAttribute('aria-activedescendant')).toBeTruthy();
    const rows = container.querySelectorAll('[role="treeitem"]');
    expect(rows.length).toBe(2); // root + movies
    expect(rows[0].getAttribute('aria-level')).toBe('1');
    expect(rows[1].getAttribute('aria-level')).toBe('2');
    expect(rows[1].getAttribute('aria-expanded')).toBe('false');
    // Percent renders, never 0% for unknown; error badge from snapshot aggregates.
    // DSK-07 B1: the backend ships 0–100 — a 50 fixture renders 50.0%, never
    // the double-scaled 5000.0%.
    expect(rows[1].textContent).toContain('50.0%');
    expect(rows[1].textContent).not.toContain('5000.0%');
    expect(rows[0].textContent).toContain('⚠');
    expect(rows[0].textContent).toContain('5·2'); // files·folders counts
  });

  it('FER-479: child counts and error badges come from the page items (no node map)', async () => {
    preload();
    duMocks.queryChildren.mockResolvedValue({
      scanId: 's1',
      nodeId: ROOT,
      version: 3,
      scannedAtMs: 1,
      metric: 'logical',
      coverage: 'partial',
      offset: 0,
      limit: 50,
      total: 2,
      items: [
        item(`${ROOT}\\movies`, {
          name: 'movies',
          subtreeFiles: 7,
          subtreeDirs: 3,
          subtreeErrors: 4,
        }),
        item(`${ROOT}\\a.bin`, { name: 'a.bin', kind: 'file', logicalBytes: 10 }),
      ],
    });
    // The summary snapshot carries the ROOT only — no per-node map at all.
    expect(diskUsage.snapshot && 'nodes' in diskUsage.snapshot).toBe(false);
    const { container } = renderTree();
    await diskUsage.loadChildren(ROOT);
    await tick();
    const rows = container.querySelectorAll('[role="treeitem"]');
    expect(rows.length).toBe(3);
    expect(rows[1].querySelector('.ccnt')?.textContent).toBe('7·3');
    expect(rows[1].querySelector('.badge')?.textContent).toContain('4');
    expect(rows[2].querySelector('.ccnt')?.textContent).toBe('1·0');
    expect(rows[2].querySelector('.badge')).toBeNull();
  });

  it('DSK-10 responsive columns: shared track template with a name floor + row min-width', async () => {
    const { DU_TREE_ROW_COLUMNS, DU_TREE_ROW_MIN_WIDTH } = await import('./treemapLayout');
    // One source of truth: the name track never shrinks below 140px, counts
    // tracks have their own floors, and rows carry a min-width so a narrow
    // panel scrolls horizontally instead of silently clipping columns.
    expect(DU_TREE_ROW_COLUMNS).toBe(
      '20px minmax(140px, 1fr) minmax(48px, 64px) minmax(64px, 84px) minmax(40px, 52px) minmax(52px, 72px)',
    );
    expect(DU_TREE_ROW_MIN_WIDTH).toBe(424);

    preload();
    const { container } = renderTree();
    const wrap = container.querySelector('[data-testid="du-tree"]') as HTMLElement;
    // The template + minimum are bound to the DOM via CSS vars shared by
    // the header (.cols) and every row — nothing clipped without a trace.
    const style = wrap.getAttribute('style') ?? '';
    expect(style).toContain('minmax(140px, 1fr)');
    expect(style).toContain('424px');
    const head = wrap.querySelector('.cols') as HTMLElement;
    expect(head.querySelector('.cchev')).toBeTruthy(); // header uses the same grid
  });

  it('shows "—" for null percent and "unavailable" for null allocated bytes', () => {
    preload();
    diskUsage.treeNodes[ROOT] = {
      items: [
        item('x', { percentOfParent: null, logicalBytes: 10, allocatedBytes: null }),
        item('y', { kind: 'file', percentOfParent: 25 }),
      ],
      total: 2,
      loading: false,
      loadingMore: false,
      error: null,
      loadId: 1,
    };
    const first = renderTree();
    const rows = [...first.container.querySelectorAll('[role="treeitem"]')];
    expect(rows[1].textContent).toContain('—'); // null percent → "—", never 0%
    expect(rows[2].textContent).toContain('25.0%');
    cleanup();
    // Allocated metric + null allocated → "unavailable", never logical bytes.
    diskUsage.metric = 'allocated';
    const second = renderTree();
    const rows2 = [...second.container.querySelectorAll('[role="treeitem"]')];
    expect(rows2[1].textContent).toContain('unavailable');
    expect(rows2[1].textContent).not.toContain('10.0 B');
  });
});

describe('lazy pagination', () => {
  it('offers "load more" while items < total and requests the next offset', async () => {
    preload();
    duMocks.queryChildren
      .mockResolvedValueOnce({
        scanId: 's1',
        nodeId: ROOT,
        version: 3,
        scannedAtMs: 1,
        metric: 'logical',
        coverage: 'complete',
        offset: 0,
        limit: 50,
        total: 3,
        items: [item('a'), item('b')],
      })
      .mockResolvedValueOnce({
        scanId: 's1',
        nodeId: ROOT,
        version: 3,
        scannedAtMs: 1,
        metric: 'logical',
        coverage: 'complete',
        offset: 2,
        limit: 50,
        total: 3,
        items: [item('c')],
      });
    const { container } = renderTree();
    await diskUsage.loadChildren(ROOT);
    await tick(); // let the derived rows re-render
    const more = [...container.querySelectorAll('[role="treeitem"]')].find((b) =>
      b.textContent?.includes('Muat'),
    );
    expect(more?.textContent).toContain('(2/3)');
    more!.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    await tick();
    await tick();
    expect(duMocks.queryChildren).toHaveBeenLastCalledWith(
      's1',
      ROOT,
      2,
      expect.any(Number),
      'largestFirst',
      { metric: 'logical' },
    );
    expect(diskUsage.treeNodes[ROOT]?.items).toHaveLength(3);
  });
});

describe('keyboard + interaction', () => {
  async function bootTwoRows() {
    preload();
    diskUsage.treeNodes[ROOT] = {
      items: [
        item(`${ROOT}\\bigdir`, { name: 'bigdir', percentOfParent: 70 }),
        item(`${ROOT}\\note.txt`, { name: 'note.txt', kind: 'file' }),
      ],
      total: 2,
      loading: false,
      loadingMore: false,
      error: null,
      loadId: 1,
    };
    const result = renderTree();
    const tree = result.container.querySelector('[role="tree"]') as HTMLElement;
    return { ...result, tree };
  }

  it('ArrowDown moves the roving focus; Enter toggles a folder; Enter on a file selects it; Ctrl+Enter zooms', async () => {
    const { tree } = await bootTwoRows();
    tree.focus();
    fireEvent.keyDown(tree, { key: 'ArrowDown' }); // active → root row
    fireEvent.keyDown(tree, { key: 'ArrowDown' }); // → bigdir
    await tick();
    const rows = tree.querySelectorAll('[role="treeitem"]');
    expect(tree.getAttribute('aria-activedescendant')).toBe(rows[1].id);

    fireEvent.keyDown(tree, { key: 'Enter' }); // folder → toggles expand
    expect(diskUsage.expanded.has(`${ROOT}\\bigdir`)).toBe(true);
    fireEvent.keyDown(tree, { key: 'Enter' });
    expect(diskUsage.expanded.has(`${ROOT}\\bigdir`)).toBe(false);

    fireEvent.keyDown(tree, { key: 'ArrowDown' }); // → note.txt (file)
    fireEvent.keyDown(tree, { key: 'Enter' });
    expect(diskUsage.selectedNodeId).toBe(`${ROOT}\\note.txt`);

    fireEvent.keyDown(tree, { key: 'ArrowUp' }); // back to bigdir
    fireEvent.keyDown(tree, { key: 'Enter', ctrlKey: true }); // zoom action kept
    expect(diskUsage.zoomStack).toEqual([`${ROOT}\\bigdir`]);
  });

  it('arrow keys expand/collapse and rise to the parent row', async () => {
    duMocks.queryChildren.mockResolvedValue({
      scanId: 's1',
      nodeId: `${ROOT}\\bigdir`,
      version: 3,
      scannedAtMs: 1,
      metric: 'logical',
      coverage: 'complete',
      offset: 0,
      limit: 50,
      total: 1,
      items: [item('inner', { kind: 'file' })],
    });
    const { tree } = await bootTwoRows();
    tree.focus();
    await tick();
    // Root row is a fixed container: ArrowRight on it does nothing. Move to
    // bigdir first, then expand it.
    fireEvent.keyDown(tree, { key: 'ArrowDown' }); // → root
    fireEvent.keyDown(tree, { key: 'ArrowDown' }); // → bigdir
    fireEvent.keyDown(tree, { key: 'ArrowRight' }); // expand bigdir
    await tick();
    await tick();
    expect(diskUsage.expanded.has(`${ROOT}\\bigdir`)).toBe(true);
    const rows = tree.querySelectorAll('[role="treeitem"]');
    expect(rows.length).toBe(4); // root, bigdir, inner (DFS insert), note.txt
    fireEvent.keyDown(tree, { key: 'ArrowDown' }); // → inner
    await tick();
    expect(tree.getAttribute('aria-activedescendant')).toBe(rows[2].id);
    fireEvent.keyDown(tree, { key: 'ArrowLeft' }); // inner is a file → rise to bigdir
    await tick();
    expect(tree.getAttribute('aria-activedescendant')).toBe(rows[1].id);
    fireEvent.keyDown(tree, { key: 'ArrowLeft' }); // bigdir expanded → collapse
    expect(diskUsage.expanded.has(`${ROOT}\\bigdir`)).toBe(false);
  });

  it('double-click zooms a FOLDER and does NOTHING on a file', async () => {
    const { container } = await bootTwoRows();
    const rows = [...container.querySelectorAll('[role="treeitem"]')];
    const bigdir = rows.find((r) => r.getAttribute('data-row-id')?.endsWith('bigdir'))!;
    const file = rows.find((r) => r.getAttribute('data-row-id')?.endsWith('note.txt'))!;
    fireEvent.dblClick(file);
    expect(diskUsage.zoomStack).toEqual([]); // files never act
    fireEvent.dblClick(bigdir);
    expect(diskUsage.zoomStack).toEqual([`${ROOT}\\bigdir`]);
  });

  it('a plain click selects (files included) — sync with treemap scope', async () => {
    const { container } = await bootTwoRows();
    const rows = [...container.querySelectorAll('[role="treeitem"]')];
    fireEvent.click(rows[2]); // note.txt
    expect(diskUsage.selectedNodeId).toBe(`${ROOT}\\note.txt`);
  });
});

describe('DSK-06 row context actions (existing surfaces, revalidated)', () => {
  async function bootTwoRows() {
    preload();
    diskUsage.treeNodes[ROOT] = {
      items: [
        item(`${ROOT}\\bigdir`, { name: 'bigdir', percentOfParent: 70 }),
        item(`${ROOT}\\note.txt`, { name: 'note.txt', kind: 'file' }),
      ],
      total: 2,
      loading: false,
      loadingMore: false,
      error: null,
      loadId: 1,
    };
    const result = renderTree();
    await tick();
    return result;
  }

  const menuLabels = (): string[] =>
    contextMenu.items.map((i) => ('separator' in i ? '—' : i.label));

  it('right-click opens the context menu with the existing-surface items and no clean entry', async () => {
    const { container } = await bootTwoRows();
    const rows = [...container.querySelectorAll('[role="treeitem"]')];
    fireEvent.contextMenu(rows[1]); // bigdir
    await tick();
    expect(contextMenu.isOpen).toBe(true);
    expect(menuLabels()).toEqual([
      'Reveal in pane',
      'Properties',
      'Copy path',
      '—',
      'Move to other pane…',
      'Delete…',
    ]);
  });

  it('the scan root row offers the read-only set only — never Move/Delete', async () => {
    const { container } = await bootTwoRows();
    const rows = [...container.querySelectorAll('[role="treeitem"]')];
    fireEvent.contextMenu(rows[0]); // scan root
    await tick();
    expect(menuLabels()).toEqual(['Reveal in pane', 'Properties', 'Copy path']);
  });

  it('the ⋯ affordance opens the same menu', async () => {
    const { container } = await bootTwoRows();
    const rows = [...container.querySelectorAll('[role="treeitem"]')];
    const ctx = rows[2].querySelector('.ctxBtn') as HTMLButtonElement;
    expect(ctx).not.toBeNull();
    fireEvent.click(ctx);
    await tick();
    expect(contextMenu.isOpen).toBe(true);
  });

  it('choosing Delete revalidates then opens the ONE existing delete confirm', async () => {
    propsMocks.entryProperties.mockResolvedValue({
      path: `${ROOT}\\bigdir`,
      name: 'bigdir',
      kind: 'dir',
      size: 0,
      allocatedBytes: 0,
      allocatedSupported: true,
      createdMs: 1,
      modifiedMs: 2,
      accessedMs: 3,
      readOnly: false,
      hidden: false,
      system: false,
    });
    const { container } = await bootTwoRows();
    const rows = [...container.querySelectorAll('[role="treeitem"]')];
    fireEvent.contextMenu(rows[1]);
    const del = await vi.waitFor(() => {
      const btn = Array.from(document.querySelectorAll('[role="menuitem"]')).find((b) =>
        b.textContent?.includes('Delete…'),
      );
      expect(btn, 'menu item "Delete…"').toBeTruthy();
      return btn as HTMLElement;
    });
    del.click();
    await vi.waitFor(() =>
      expect(dialogs.current).toMatchObject({
        kind: 'delete',
        paths: [`${ROOT}\\bigdir`],
        permanent: false,
      }),
    );
  });

  it('a stale row refuses Delete from the menu and leaves no dialog', async () => {
    propsMocks.entryProperties.mockRejectedValue(
      new (await import('../ipc/types')).FsError({ code: 'notFound', message: 'gone' }),
    );
    const { container } = await bootTwoRows();
    const rows = [...container.querySelectorAll('[role="treeitem"]')];
    fireEvent.contextMenu(rows[2]); // note.txt
    const del = await vi.waitFor(() => {
      const btn = Array.from(document.querySelectorAll('[role="menuitem"]')).find((b) =>
        b.textContent?.includes('Delete…'),
      );
      expect(btn, 'menu item "Delete…"').toBeTruthy();
      return btn as HTMLElement;
    });
    del.click();
    await vi.waitFor(() => expect(dialogs.current).toBeNull());
    expect(contextMenu.isOpen).toBe(false); // menu already closed by its own click
  });
});

describe('FER-499 tree navigation: indentation, keyboard model, treemap sync', () => {
  const BIG = `${ROOT}\\bigdir`;
  const NOTE = `${ROOT}\\note.txt`;
  const INNER = `${BIG}\\inner.txt`;

  /** Children by node id; offset/limit honoured like the real IPC. */
  function mockChildren(map: Record<string, DiskUsageChildItem[]>): void {
    duMocks.queryChildren.mockImplementation(
      async (_scan: string, nodeId: string, offset = 0, limit = 50) => {
        const all = map[nodeId] ?? [];
        return {
          scanId: 's1',
          nodeId,
          version: 3,
          scannedAtMs: 1,
          metric: 'logical',
          coverage: 'complete',
          offset,
          limit,
          total: all.length,
          items: all.slice(offset, offset + limit),
        };
      },
    );
  }

  async function boot(rootItems: DiskUsageChildItem[], total = rootItems.length) {
    preload();
    diskUsage.treeNodes[ROOT] = {
      items: rootItems,
      total,
      loading: false,
      loadingMore: false,
      error: null,
      loadId: 1,
    };
    const r = renderTree();
    const tree = r.container.querySelector('[role="tree"]') as HTMLElement;
    tree.focus();
    const rows = (): HTMLElement[] => [...tree.querySelectorAll<HTMLElement>('[role="treeitem"]')];
    return { ...r, tree, rows };
  }

  const press = async (tree: HTMLElement, key: string, init: KeyboardEventInit = {}) => {
    fireEvent.keyDown(tree, { key, ...init });
    await tick();
  };
  /** Longer than the 150 ms cursor-to-treemap debounce. */
  const settle = () => new Promise((resolve) => setTimeout(resolve, 180));
  const activeOf = (tree: HTMLElement) => tree.getAttribute('aria-activedescendant');

  const bigAndNote = () => [
    item(BIG, { name: 'bigdir' }),
    item(NOTE, { name: 'note.txt', kind: 'file' }),
  ];

  beforeEach(() => {
    duMocks.queryChildren.mockReset();
    mockChildren({ [ROOT]: bigAndNote() });
  });

  it('indents children per level, marks folders with aria-expanded and levels with aria-level', async () => {
    mockChildren({
      [ROOT]: bigAndNote(),
      [BIG]: [item(INNER, { name: 'inner.txt', kind: 'file' })],
    });
    const { tree, rows } = await boot(bigAndNote());
    await press(tree, 'ArrowDown');
    await press(tree, 'ArrowDown');
    await press(tree, 'ArrowRight'); // expand bigdir
    await tick();
    await tick();
    const r = rows();
    expect(r.map((x) => x.getAttribute('aria-level'))).toEqual(['1', '2', '3', '2']);
    expect(r[1].getAttribute('aria-expanded')).toBe('true');
    expect(r[3].getAttribute('aria-expanded')).toBeNull(); // file: no expanded state
    // Indent grows 16px per depth; the chevron sits at that offset.
    expect(r[1].querySelector<HTMLElement>('.chev')!.style.paddingLeft).toBe('16px');
    expect(r[2].querySelector<HTMLElement>('.chev')!.style.paddingLeft).toBe('32px');
    expect(r[2].querySelectorAll('.guide')).toHaveLength(2);
    // Files have no chevron button but still occupy the same column.
    expect(r[3].querySelector('button.chevBtn')).toBeNull();
    expect(r[1].querySelector('button.chevBtn')).not.toBeNull();
  });

  it('right arrow expands a closed folder then steps to its first child; left on a file rises; left on an open folder collapses', async () => {
    mockChildren({
      [ROOT]: bigAndNote(),
      [BIG]: [item(INNER, { name: 'inner.txt', kind: 'file' })],
    });
    const { tree, rows } = await boot(bigAndNote());
    await press(tree, 'ArrowDown');
    await press(tree, 'ArrowDown'); // bigdir
    await press(tree, 'ArrowRight'); // closed: expand
    await tick();
    expect(diskUsage.expanded.has(BIG)).toBe(true);
    expect(activeOf(tree)).toBe(rows()[1].id); // cursor stays on bigdir
    await press(tree, 'ArrowRight'); // open: first child
    expect(activeOf(tree)).toBe(rows()[2].id);
    await press(tree, 'ArrowLeft'); // file: parent
    expect(activeOf(tree)).toBe(rows()[1].id);
    await press(tree, 'ArrowLeft'); // open folder: collapse
    expect(diskUsage.expanded.has(BIG)).toBe(false);
    expect(rows()).toHaveLength(3);
  });

  it('Enter toggles a folder; Shift+Enter opens it in the Files tab; Ctrl+Enter zooms (kept action)', async () => {
    const { tree } = await boot(bigAndNote());
    await press(tree, 'ArrowDown');
    await press(tree, 'ArrowDown'); // bigdir
    await press(tree, 'Enter');
    expect(diskUsage.expanded.has(BIG)).toBe(true);
    await press(tree, 'Enter');
    expect(diskUsage.expanded.has(BIG)).toBe(false);
    await press(tree, 'Enter', { shiftKey: true });
    expect(diskUsage.selectedNodeId).toBe(BIG);
    expect(diskUsage.rightTab).toBe('files');
    await press(tree, 'Enter', { ctrlKey: true });
    expect(diskUsage.zoomStack).toEqual([BIG]);
  });

  it('Enter on "Muat N lagi" loads the next page and keeps the cursor at the same index', async () => {
    const a = item('a', { kind: 'file' });
    const b = item('b', { kind: 'file' });
    const c = item('c', { kind: 'file' });
    mockChildren({ [ROOT]: [a, b, c] });
    const { tree, rows } = await boot([a, b], 3);
    await press(tree, 'ArrowDown');
    await press(tree, 'ArrowDown');
    await press(tree, 'ArrowDown');
    await press(tree, 'ArrowDown'); // the "Muat 1 lagi" row
    expect(rows()[3].getAttribute('aria-label')).toContain('Muat 1 lagi (2/3)');
    expect(activeOf(tree)).toBe(rows()[3].id);
    await press(tree, 'Enter');
    await new Promise((resolve) => setTimeout(resolve, 0)); // load + cursor update
    await tick();
    expect(duMocks.queryChildren).toHaveBeenLastCalledWith(
      's1',
      ROOT,
      2,
      expect.any(Number),
      'largestFirst',
      { metric: 'logical' },
    );
    const after = rows();
    expect(after.map((x) => x.getAttribute('data-row-id'))).toEqual([ROOT, 'a', 'b', 'c']);
    expect(activeOf(tree)).toBe(after[3].id); // same index: the first newly loaded row
  });

  it('Home / End / PageDown / PageUp move the cursor and stay in range', async () => {
    const { tree, rows } = await boot(bigAndNote());
    await press(tree, 'End');
    expect(activeOf(tree)).toBe(rows()[2].id);
    await press(tree, 'Home');
    expect(activeOf(tree)).toBe(rows()[0].id);
    await press(tree, 'PageDown'); // jsdom has no layout: one page covers the list
    expect(activeOf(tree)).toBe(rows()[2].id);
    await press(tree, 'PageUp');
    expect(activeOf(tree)).toBe(rows()[0].id);
  });

  it('type-ahead jumps to the next row whose name starts with the typed text', async () => {
    const { tree, rows } = await boot(bigAndNote());
    await press(tree, 'n');
    expect(activeOf(tree)).toBe(rows()[2].id);
    await press(tree, 'b');
    expect(activeOf(tree)).toBe(rows()[2].id); // the buffer "nb" matches nothing: no move
  });

  it('* expands the child folders one level', async () => {
    const { tree } = await boot(bigAndNote());
    await press(tree, '*'); // on the root row
    expect(diskUsage.expanded.has(BIG)).toBe(true);
  });

  it('Tab does not reach the window (App FER-489 pane switch) while the tree has focus', async () => {
    const { tree } = await boot(bigAndNote());
    const onWindow = vi.fn();
    window.addEventListener('keydown', onWindow);
    try {
      fireEvent.keyDown(tree, { key: 'Tab' });
      expect(onWindow).not.toHaveBeenCalled();
    } finally {
      window.removeEventListener('keydown', onWindow);
    }
  });

  it('tree cursor on a folder zooms the treemap to it (breadcrumb follows); root field untouched', async () => {
    const rootBefore = diskUsage.rootInput;
    const { tree } = await boot(bigAndNote());
    await press(tree, 'ArrowDown');
    await press(tree, 'ArrowDown'); // bigdir
    await settle();
    expect(diskUsage.zoomStack).toEqual([BIG]);
    expect(diskUsage.crumbs().map((c) => c.id)).toEqual([ROOT, BIG]);
    expect(diskUsage.crumbs().map((c) => c.label)).toEqual(['data', 'bigdir']);
    expect(diskUsage.rootInput).toBe(rootBefore);
  });

  it('tree cursor on a file zooms to its parent folder and highlights the file', async () => {
    mockChildren({
      [ROOT]: bigAndNote(),
      [BIG]: [item(INNER, { name: 'inner.txt', kind: 'file' })],
    });
    const { tree } = await boot(bigAndNote());
    await press(tree, 'ArrowDown');
    await press(tree, 'ArrowDown'); // bigdir
    await press(tree, 'ArrowRight'); // expand bigdir
    await press(tree, 'ArrowDown'); // to inner.txt
    await settle();
    expect(diskUsage.zoomStack).toEqual([BIG]);
    expect(diskUsage.selectedNodeId).toBe(INNER);
  });

  it('a treemap zoom moves the tree cursor to that folder without a loop back', async () => {
    const { tree, rows } = await boot(bigAndNote());
    diskUsage.zoomIn(BIG); // what a treemap double-click does
    await tick();
    await tick();
    const big = rows().find((r) => r.getAttribute('data-row-id') === BIG)!;
    expect(activeOf(tree)).toBe(big.id);
    await settle();
    expect(diskUsage.zoomStack).toEqual([BIG]); // not re-zoomed by the tree
  });

  it('a treemap selection moves the tree cursor onto that node', async () => {
    const { tree, rows } = await boot(bigAndNote());
    diskUsage.selectNode(NOTE);
    await tick();
    await tick();
    const note = rows().find((r) => r.getAttribute('data-row-id') === NOTE)!;
    expect(activeOf(tree)).toBe(note.id);
    expect(note.getAttribute('aria-selected')).toBe('true');
  });

  it('clicking a row and switching metric within the 150 ms debounce keeps selection on that node even when rows reorder', async () => {
    const DOCS = `${ROOT}\\Documents`;
    const MUSIC = `${ROOT}\\Music`;
    const logicalItems = [
      item(DOCS, { name: 'Documents', logicalBytes: 200, allocatedBytes: null }),
      item(MUSIC, { name: 'Music', logicalBytes: 150, allocatedBytes: 150 }),
    ];
    const allocatedItems = [
      item(MUSIC, { name: 'Music', logicalBytes: 150, allocatedBytes: 150 }),
      item(DOCS, { name: 'Documents', logicalBytes: 200, allocatedBytes: null }),
    ];
    duMocks.queryChildren.mockImplementation(
      async (_scan: string, nodeId: string, offset = 0, limit = 50, _sort, filter) => {
        const all =
          nodeId === ROOT ? (filter?.metric === 'allocated' ? allocatedItems : logicalItems) : [];
        return {
          scanId: 's1',
          nodeId,
          version: 3,
          scannedAtMs: 1,
          metric: filter?.metric ?? 'logical',
          coverage: 'complete',
          offset,
          limit,
          total: all.length,
          items: all.slice(offset, offset + limit),
        };
      },
    );
    const { tree, rows } = await boot(logicalItems);
    const docsBefore = rows().find((r) => r.getAttribute('data-row-id') === DOCS)!;
    fireEvent.click(docsBefore);
    expect(diskUsage.selectedNodeId).toBe(DOCS);

    // Switch metric before the 150 ms zoomTimer fires; Documents moves from index 1 to index 2.
    diskUsage.setMetric('allocated');
    await settle();
    await tick();

    expect(diskUsage.selectedNodeId).toBe(DOCS);
    expect(diskUsage.zoomStack).toEqual([DOCS]);
    const docsAfter = rows().find((r) => r.getAttribute('data-row-id') === DOCS)!;
    expect(activeOf(tree)).toBe(docsAfter.id);
  });
});
