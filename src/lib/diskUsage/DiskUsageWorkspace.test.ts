/**
 * DSK-11 (FER-409) — the single-session Disk usage workspace. Proven here:
 * the split layout (ONE tree filling the left slot, the Treemap|Files tabs
 * filling the right slot, global header/Exit on top), mount/unmount + restore
 * (the scan state survives the global Exit), one store instance driving both
 * slots (select in the store → the tree row highlights; zoom crumbs), the
 * single global Exit (bar button + Escape, suppressed while a dialog owns
 * the keyboard), and the Reveal flow into the ENTRY pane (existing pane
 * navigation + selection). The DSK-10 two-independent-sides layout is gone.
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
  cacheLoad: vi.fn(),
  cacheDelete: vi.fn(),
  fastElig: vi.fn(),
  indexElig: vi.fn(),
}));
vi.mock('../ipc/diskUsage', () => ({
  diskUsageStart: duMocks.start,
  diskUsageCancel: duMocks.cancel,
  diskUsageSnapshot: duMocks.snapshot,
  diskUsageQueryChildren: duMocks.queryChildren,
  diskUsageTopFiles: duMocks.topFiles,
  onDiskUsageEvent: vi.fn(async () => () => {}),
  diskUsageCacheLoad: duMocks.cacheLoad,
  diskUsageCacheDelete: duMocks.cacheDelete,
  diskUsageFastEligibility: duMocks.fastElig,
  diskUsageIndexEligibility: duMocks.indexElig,
  diskUsageCacheList: vi.fn(async () => []),
  diskUsageCacheNoteMutation: vi.fn(async () => undefined),
}));

import DiskUsageWorkspace from './DiskUsageWorkspace.svelte';
import { duWorkspace } from './duWorkspace.svelte';
import { dialogs } from '../stores/dialogStore.svelte';
import { registerPane } from '../stores/panesRegistry.svelte';
import type { DiskUsageChildItem, DiskUsageSnapshot } from '../ipc/types';

/** Drain the microtask queue (store methods are 1–2 awaits deep). */
async function flush(): Promise<void> {
  for (let i = 0; i < 12; i++) await Promise.resolve();
}

function snapshotFixture(scanId: string, rootId: string): DiskUsageSnapshot {
  return {
    scanId,
    rootPath: rootId,
    rootId,
    version: 3,
    scannedAtMs: 1,
    rootNode: {
      id: rootId,
      path: rootId,
      name: rootId,
      childrenIds: [],
      directFiles: 0,
      directSymlinks: 0,
      directLogicalBytes: 0,
      directAllocatedBytes: 0,
      subtreeFiles: 2,
      subtreeDirs: 1,
      subtreeSymlinks: 0,
      subtreeLogicalBytes: 10,
      subtreeAllocatedBytes: 12,
      subtreeErrors: 0,
      skippedVolumeBoundaries: 0,
      subtreeSkippedVolumes: 0,
    },
    stats: {
      scannedFiles: 2,
      scannedDirs: 1,
      scannedSymlinks: 0,
      skippedVolumeBoundaries: 0,
      logicalBytes: 10,
      allocatedAvailableFiles: 2,
      allocatedUnavailableFiles: 0,
      knownAllocatedBytes: 12,
      totalErrors: 0,
      coverage: 'complete',
      cancelled: false,
      depthCapped: false,
      durationMs: 3,
    },
  };
}

/** Feed a finished scan through the real event path of the ONE store. */
function finishScan(scanId: string): void {
  duWorkspace.store.testEvent({
    scanId,
    scannedFiles: 2,
    scannedDirs: 1,
    scannedSymlinks: 0,
    knownLogicalBytes: 10,
    knownAllocatedBytes: 12,
    allocatedAvailableFiles: 2,
    allocatedUnavailableFiles: 0,
    coverage: 'complete',
    errors: [],
    totalErrors: 0,
    topChildren: [],
    done: true,
    cancelled: false,
    durationMs: 3,
  });
}

function childItem(id: string, over: Partial<DiskUsageChildItem> = {}): DiskUsageChildItem {
  return {
    id,
    name: id,
    kind: 'dir',
    logicalBytes: 5,
    allocatedBytes: 8,
    allocatedSupported: true,
    category: 'other',
    percentOfParent: null,
    percentOfScanRoot: null,
    ...over,
  };
}

beforeEach(() => {
  vi.clearAllMocks();
  duWorkspace.exit();
  duWorkspace.entryPane = 0;
  duWorkspace.store.reset();
  dialogs.close();
  duMocks.cancel.mockResolvedValue(true);
  duMocks.cacheLoad.mockReset().mockResolvedValue(null);
  duMocks.cacheDelete.mockReset().mockResolvedValue(1);
  duMocks.fastElig.mockReset().mockResolvedValue({ eligible: false });
  duMocks.indexElig.mockReset().mockResolvedValue({ eligible: false });
  duMocks.start.mockReset();
  duMocks.queryChildren.mockResolvedValue({
    scanId: 's-x',
    nodeId: 'n',
    version: 1,
    scannedAtMs: 1,
    metric: 'logical',
    coverage: 'complete',
    offset: 0,
    limit: 50,
    total: 0,
    items: [],
  });
  duMocks.topFiles.mockResolvedValue({
    scanId: 's-x',
    nodeId: 'n',
    version: 1,
    scannedAtMs: 1,
    metric: 'logical',
    coverage: 'complete',
    limit: 20,
    totalMatching: 0,
    truncated: false,
    label: 'Top 20',
    items: [],
  });
});

afterEach(() => {
  cleanup();
  duWorkspace.exit();
  duWorkspace.store.reset();
});

describe('DiskUsageWorkspace — split layout (one session, two slots)', () => {
  it('renders ONE tree in the left slot and the tab view in the right slot', () => {
    duWorkspace.openFromPanes();
    const { getByTestId } = render(DiskUsageWorkspace);
    const ws = getByTestId('du-workspace');
    // Slot A: exactly ONE tree, inside the left slot.
    const left = ws.querySelectorAll('[data-testid="du-slot-left"]');
    expect(left).toHaveLength(1);
    expect(left[0].querySelectorAll('[data-testid="du-tree"]')).toHaveLength(1);
    // Slot B: the Treemap|Files tablist lives in the right slot.
    const right = ws.querySelectorAll('[data-testid="du-slot-right"]');
    expect(right).toHaveLength(1);
    expect(right[0].querySelectorAll('[role="tab"]')).toHaveLength(2);
    // The DSK-10 per-side panes are gone for good.
    expect(ws.querySelectorAll('[data-testid="du-pane"]')).toHaveLength(0);
    // Global header: root input + Exit on top of the grid.
    expect(ws.querySelector('[aria-label="Path root scan disk usage"]')).toBeTruthy();
  });

  it('openFromPanes suggests the ENTRY pane cwd as root — without touching results', () => {
    registerPane(0, paneOf('C:\\left'));
    registerPane(1, paneOf('D:\\right'));
    duWorkspace.store.testData('s-old', 'complete', 'C:\\left');
    duWorkspace.openFromPanes();
    expect(duWorkspace.open).toBe(true);
    expect(duWorkspace.entryPane).toBe(0);
    expect(duWorkspace.store.rootInput).toBe('C:\\left');
    // Scan state survives (states.md) — no auto-rescan anywhere.
    expect(duWorkspace.store.phase).toBe('complete');
    expect(duWorkspace.store.scanId).toBe('s-old');
    expect(duMocks.start).not.toHaveBeenCalled();
  });

  it('tree selection and zoom ride the ONE store — no second instance anywhere', async () => {
    registerPane(0, paneOf('C:\\left'));
    const store = duWorkspace.store;
    store.testData('s1', 'complete', 'C:\\left');
    store.snapshot = snapshotFixture('s1', 'C:\\left');
    store.treeNodes['C:\\left'] = {
      items: [childItem('C:\\left\\docs')],
      total: 1,
      loading: false,
      loadingMore: false,
      error: null,
      loadId: 1,
    };
    const { container } = render(DiskUsageWorkspace);
    // Selecting through the store highlights the row in the LEFT slot tree…
    store.selectNode('C:\\left\\docs');
    await tick();
    const row = [...container.querySelectorAll('.row')].find(
      (r) => r.getAttribute('data-row-id') === 'C:\\left\\docs',
    );
    expect(row?.classList.contains('sel')).toBe(true);
    // …and zooming is visible in the RIGHT slot's breadcrumb.
    store.zoomIn('C:\\left\\docs');
    await tick();
    const crumbs = [...container.querySelectorAll('button.crumb')].map((c) =>
      c.textContent?.trim(),
    );
    // Root crumb label = base name of the scan root; then the zoom level.
    expect(crumbs).toEqual(['left', 'docs']);
  });
});

describe('DiskUsageWorkspace — mount/unmount + restore', () => {
  it('global Exit keeps the finished result; re-entering shows it without rescanning', async () => {
    duMocks.start.mockResolvedValueOnce('s-l');
    duMocks.snapshot.mockResolvedValueOnce(snapshotFixture('s-l', 'C:\\left'));
    duWorkspace.store.rootInput = 'C:\\left';
    await duWorkspace.store.start();
    finishScan('s-l');
    await flush();
    expect(duWorkspace.store.phase).toBe('complete');

    const { getByRole } = render(DiskUsageWorkspace);
    getByRole('button', { name: /Exit ke Files/ }).click();
    expect(duWorkspace.open).toBe(false);
    // Nothing is reset: the snapshot is kept…
    expect(duWorkspace.store.scanId).toBe('s-l');
    expect(duWorkspace.store.phase).toBe('complete');
    // …and re-entering runs NO new scan (results still there).
    const startsBefore = duMocks.start.mock.calls.length;
    duWorkspace.openFromPanes();
    expect(duMocks.start.mock.calls.length).toBe(startsBefore);
  });

  it('Escape exits the workspace and keeps the finished scan state', async () => {
    duMocks.start.mockResolvedValueOnce('s-l');
    duMocks.snapshot.mockResolvedValueOnce(snapshotFixture('s-l', 'C:\\left'));
    duWorkspace.store.rootInput = 'C:\\left';
    await duWorkspace.store.start();
    finishScan('s-l');
    await flush();
    duWorkspace.openFromPanes();
    render(DiskUsageWorkspace);
    fireEvent.keyDown(window, { key: 'Escape' });
    expect(duWorkspace.open).toBe(false);
    expect(duWorkspace.store.phase).toBe('complete');
    expect(duWorkspace.store.scanId).toBe('s-l');
  });

  it('Escape with an open dialog does NOT exit (the dialog owns the keyboard)', () => {
    duWorkspace.openFromPanes();
    render(DiskUsageWorkspace);
    dialogs.open({ kind: 'mkdir', parent: 'C:\\' });
    fireEvent.keyDown(window, { key: 'Escape' });
    expect(duWorkspace.open).toBe(true);
    expect(dialogs.current).not.toBeNull();
  });
});

describe('DiskUsageWorkspace — Reveal into the ENTRY pane', () => {
  it('Reveal exits the workspace and drives ONLY the entry pane', async () => {
    const open0 = vi.fn();
    const select0 = vi.fn();
    const open1 = vi.fn();
    const select1 = vi.fn();
    registerPane(0, { ...paneOf('C:\\left'), openPath: open0, selectPaths: select0 });
    registerPane(1, { ...paneOf('D:\\right'), openPath: open1, selectPaths: select1 });
    duWorkspace.openFromPanes();
    // Prepared Files result so a real Reveal button renders.
    const store = duWorkspace.store;
    store.testData('s-l', 'complete', 'C:\\left');
    store.setRightTab('files');
    store.files = {
      result: {
        scanId: 's-l',
        nodeId: 'C:\\left',
        version: 1,
        scannedAtMs: 1,
        metric: 'logical',
        coverage: 'complete',
        limit: 20,
        totalMatching: 1,
        truncated: false,
        label: 'Top 20',
        items: [
          {
            id: 'C:\\left\\docs\\a.mkv',
            parentId: 'C:\\left\\docs',
            name: 'a.mkv',
            logicalBytes: 5,
            allocatedBytes: 8,
            allocatedSupported: true,
            category: 'video',
            percentOfParent: 50,
            percentOfScanRoot: 50,
          },
        ],
      },
      loading: false,
      error: null,
      seq: 1,
    };
    const { container } = render(DiskUsageWorkspace);
    const actBtn = container.querySelector<HTMLButtonElement>('.actBtn')!;
    expect(actBtn).toBeTruthy();
    actBtn.click();
    await tick();
    const reveal = Array.from(
      document.querySelectorAll<HTMLButtonElement>('[role="menuitem"]'),
    ).find((b) => b.textContent?.trim() === 'Reveal');
    expect(reveal).toBeTruthy();
    reveal!.click();
    await tick();
    expect(duWorkspace.open).toBe(false); // the workspace closes…
    // parentOf keeps the trailing separator (roots like "C:\" round-trip).
    expect(open0).toHaveBeenCalledWith('C:\\left\\docs\\'); // …entry pane navigates…
    expect(select0).toHaveBeenCalledWith(['C:\\left\\docs\\a.mkv'], 'C:\\left\\docs\\a.mkv');
    expect(open1).not.toHaveBeenCalled(); // …the other pane is untouched.
    expect(select1).not.toHaveBeenCalled();
  });
});

describe('DiskUsageWorkspace — breadcrumb zoom (DSK-07 B2)', () => {
  it('clicking a crumb zooms to THAT crumb — crumb i passes i, not one level deeper', async () => {
    const store = duWorkspace.store;
    store.testData('s1', 'complete', 'D:\\');
    store.zoomIn('D:\\a');
    store.zoomIn('D:\\a\\b');
    const { container } = render(DiskUsageWorkspace);
    const crumbEls = (): Element[] => [...container.querySelectorAll('button.crumb')];
    expect(crumbEls().map((c) => c.textContent?.trim())).toEqual(['D:', 'a', 'b']);
    // Regression: the old store slice(0, index+1) made crumb 0 ("D:") land
    // on D:\a — one level too deep.
    fireEvent.click(crumbEls()[0]);
    expect(store.zoomStack).toEqual([]);
    await tick(); // DOM catches up with the store before the next click
    store.zoomIn('D:\\a');
    store.zoomIn('D:\\a\\b');
    await tick();
    expect(crumbEls().map((c) => c.textContent?.trim())).toEqual(['D:', 'a', 'b']);
    fireEvent.click(crumbEls()[1]);
    expect(store.zoomStack).toEqual(['D:\\a']);
    await tick();
    fireEvent.click(crumbEls()[1]); // current level ('a') → no jump
    expect(store.zoomStack).toEqual(['D:\\a']);
  });
});

describe('DiskUsageWorkspace — FER-464 cached scan + Instan default', () => {
  const E = 'E:\\usb';
  const cachedLoad = (ageMs: number, mutated = false) => ({
    summary: { ...snapshotFixture('c1', E), scannedAtMs: Date.now() - ageMs },
    engine: 'walk' as const,
    mutated,
  });

  it('opening on a root with a cache shows it as Stale with age, warning and Rescan / Hapus cache', async () => {
    registerPane(0, paneOf(E));
    registerPane(1, paneOf('D:\\other'));
    duMocks.cacheLoad.mockResolvedValue(cachedLoad(3 * 86_400_000));
    duWorkspace.openFromPanes();
    await flush();
    const { container, getByTestId } = render(DiskUsageWorkspace);
    await tick();
    expect(duWorkspace.store.fromCache).toBe(true);
    expect(duWorkspace.store.phase).toBe('stale');
    expect(container.querySelector('.chip')?.textContent).toContain('Stale · hasil 3 hari lalu');
    expect(getByTestId('du-cache-note').textContent).toContain('cache di disk');
    expect(getByTestId('du-cache-warning').textContent).toContain('lebih tua dari 1 hari');
    const labels = [...container.querySelectorAll('.noteBtn')].map((b) => b.textContent?.trim());
    expect(labels).toEqual(['Rescan', 'Hapus cache']);
  });

  it('a recent cache has no age warning; an in-app change brings it back', async () => {
    registerPane(0, paneOf(E));
    registerPane(1, paneOf('D:\\other'));
    duMocks.cacheLoad.mockResolvedValue(cachedLoad(60_000));
    duWorkspace.openFromPanes();
    await flush();
    const { queryByTestId } = render(DiskUsageWorkspace);
    await tick();
    expect(queryByTestId('du-cache-warning')).toBeNull();
    duWorkspace.store.markStale(); // what noteScopeMutation does
    await tick();
    expect(queryByTestId('du-cache-warning')?.textContent).toContain('perubahan');
  });

  it('Hapus cache deletes this root only', async () => {
    registerPane(0, paneOf(E));
    registerPane(1, paneOf('D:\\other'));
    duMocks.cacheLoad.mockResolvedValue(cachedLoad(3 * 86_400_000));
    duWorkspace.openFromPanes();
    await flush();
    const { container } = render(DiskUsageWorkspace);
    await tick();
    const del = [...container.querySelectorAll('.noteBtn')].find(
      (b) => b.textContent?.trim() === 'Hapus cache',
    ) as HTMLElement;
    del.click();
    await flush();
    expect(duMocks.cacheDelete).toHaveBeenCalledWith(E);
    expect(duWorkspace.store.fromCache).toBe(false);
  });

  it('no cache → nothing changes (idle, no banner)', async () => {
    registerPane(0, paneOf(E));
    registerPane(1, paneOf('D:\\other'));
    duWorkspace.openFromPanes();
    await flush();
    expect(duWorkspace.store.phase).toBe('idle');
    expect(duWorkspace.store.fromCache).toBe(false);
  });

  it('Scan on an index-covered root starts "Instan (indeks)" at once — no offer dialog', async () => {
    duMocks.indexElig.mockResolvedValue({ eligible: true, volume: 'C' });
    duMocks.start.mockResolvedValue('i1');
    registerPane(0, paneOf('C:\\data'));
    registerPane(1, paneOf('D:\\other'));
    duWorkspace.openFromPanes();
    const { container } = render(DiskUsageWorkspace);
    await tick();
    const scan = [...container.querySelectorAll('button')].find(
      (b) => b.textContent?.trim() === 'Scan',
    ) as HTMLElement;
    scan.click();
    await flush();
    expect(dialogs.current).toBeNull();
    expect(duMocks.start).toHaveBeenCalledWith('C:\\data', { maxDepth: null }, false, 'index');
  });

  it('"Pilihan scan…" always opens the offer so regular / fast stay available by hand', async () => {
    duMocks.indexElig.mockResolvedValue({ eligible: true, volume: 'C' });
    duMocks.fastElig.mockResolvedValue({ eligible: true, volume: 'C' });
    registerPane(0, paneOf('C:\\data'));
    registerPane(1, paneOf('D:\\other'));
    duWorkspace.openFromPanes();
    const { container } = render(DiskUsageWorkspace);
    await tick();
    const pick = [...container.querySelectorAll('button')].find(
      (b) => b.textContent?.trim() === 'Pilihan scan…',
    ) as HTMLElement;
    pick.click();
    await flush();
    expect(dialogs.current?.kind).toBe('fastScanOffer');
    expect(duMocks.start).not.toHaveBeenCalled();
  });
});

describe('DiskUsageWorkspace — focus and address synchronization', () => {
  it('selecting a folder in tree updates toolbar address, breadcrumb, and treemap', async () => {
    registerPane(0, paneOf('C:\\data'));
    const store = duWorkspace.store;
    store.testData('s-sync', 'complete', 'C:\\data');
    store.snapshot = snapshotFixture('s-sync', 'C:\\data');
    store.treeNodes['C:\\data'] = {
      items: [childItem('C:\\data\\sub')],
      total: 1,
      loading: false,
      loadingMore: false,
      error: null,
      loadId: 1,
    };
    const { container } = render(DiskUsageWorkspace);
    await tick();

    // Initially address is C:\data
    const input = container.querySelector(
      'input[aria-label="Path root scan disk usage"]',
    ) as HTMLInputElement;
    expect(input.value).toBe('C:\\data');

    // Click row C:\data\sub in tree
    const row = [...container.querySelectorAll('.row')].find(
      (r) => r.getAttribute('data-row-id') === 'C:\\data\\sub',
    ) as HTMLElement;
    expect(row).toBeTruthy();
    row.click();
    await tick();
    await new Promise((r) => setTimeout(r, 200));
    await flush();

    // Toolbar address updates to C:\data\sub
    expect(input.value).toBe('C:\\data\\sub');

    // Scan chip shows Scan: C:\data
    const chip = container.querySelector('.scanRootChip');
    expect(chip?.textContent).toContain('Scan:');
    expect(chip?.textContent).toContain('C:\\data');

    // Breadcrumbs updated
    const crumbs = [...container.querySelectorAll('button.crumb')].map((c) =>
      c.textContent?.trim(),
    );
    expect(crumbs).toEqual(['data', 'sub']);
  });

  it('drilling in treemap updates address and Up button returns to parent', async () => {
    registerPane(0, paneOf('C:\\data'));
    const store = duWorkspace.store;
    store.testData('s-sync', 'complete', 'C:\\data');
    store.snapshot = snapshotFixture('s-sync', 'C:\\data');
    const { container } = render(DiskUsageWorkspace);
    await tick();

    const input = container.querySelector(
      'input[aria-label="Path root scan disk usage"]',
    ) as HTMLInputElement;
    const upBtn = container.querySelector('button.upBtn') as HTMLButtonElement;
    expect(upBtn.disabled).toBe(true);

    // Zoom into C:\data\sub
    store.zoomIn('C:\\data\\sub');
    await tick();

    expect(input.value).toBe('C:\\data\\sub');
    expect(upBtn.disabled).toBe(false);

    // Click Up button
    upBtn.click();
    await tick();

    expect(input.value).toBe('C:\\data');
    expect(store.zoomStack).toEqual([]);
    expect(upBtn.disabled).toBe(true);
  });

  it('address bar Enter navigates within scan scope without rescanning', async () => {
    registerPane(0, paneOf('C:\\data'));
    const store = duWorkspace.store;
    store.testData('s-sync', 'complete', 'C:\\data');
    store.snapshot = snapshotFixture('s-sync', 'C:\\data');
    const { container } = render(DiskUsageWorkspace);
    await tick();

    const input = container.querySelector(
      'input[aria-label="Path root scan disk usage"]',
    ) as HTMLInputElement;
    fireEvent.focus(input);
    fireEvent.input(input, { target: { value: 'C:\\data\\nested' } });
    fireEvent.keyDown(input, { key: 'Enter' });
    await tick();
    await flush();

    // Should focus within scan scope, NOT start a new scan
    expect(duMocks.start).not.toHaveBeenCalled();
    expect(store.zoomNodeId).toBe('C:\\data\\nested');
  });

  it('address bar Enter triggers a new scan if path is outside scanned scope', async () => {
    duMocks.start.mockResolvedValueOnce('s-new');
    registerPane(0, paneOf('C:\\data'));
    const store = duWorkspace.store;
    store.testData('s-sync', 'complete', 'C:\\data');
    store.snapshot = snapshotFixture('s-sync', 'C:\\data');
    const { container } = render(DiskUsageWorkspace);
    await tick();

    const input = container.querySelector(
      'input[aria-label="Path root scan disk usage"]',
    ) as HTMLInputElement;
    fireEvent.focus(input);
    fireEvent.input(input, { target: { value: 'D:\\other' } });
    fireEvent.keyDown(input, { key: 'Enter' });
    await tick();
    await flush();

    // Should trigger a new scan for D:\other
    expect(duMocks.start).toHaveBeenCalledWith('D:\\other', { maxDepth: null });
  });
});

// ── helpers ────────────────────────────────────────────────────────────────
function paneOf(path: string) {
  return {
    path,
    selectedPaths: () => [],
    cursorEntry: () => undefined,
    refresh: () => {},
    newTab: () => {},
    closeCurrentTab: () => {},
    goUp: () => {},
    openCursorEntry: () => {},
  };
}
