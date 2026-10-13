// FER-375 (APP-04) acceptance: switching theme/palette while a Disk Usage
// result is on screen must not rescan and must not drop results, selection
// or zoom. Runs the real workspace + store against all 12 combinations.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, render } from '@testing-library/svelte';
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

import DiskUsageWorkspace from './DiskUsageWorkspace.svelte';
import { duWorkspace } from './duWorkspace.svelte';
import { ui, PALETTES } from '../uiState.svelte';
import type { DiskUsageChildItem, DiskUsageSnapshot } from '../ipc/types';

const ROOT = 'C:\\scan';
const DOCS = 'C:\\scan\\docs';

function snapshotFixture(): DiskUsageSnapshot {
  return {
    scanId: 's1',
    rootPath: ROOT,
    rootId: ROOT,
    version: 3,
    scannedAtMs: 1,
    rootNode: {
      id: ROOT,
      path: ROOT,
      name: ROOT,
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

function child(id: string): DiskUsageChildItem {
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
  };
}

beforeEach(() => {
  vi.clearAllMocks();
  duWorkspace.exit();
  duWorkspace.store.reset();
  duMocks.queryChildren.mockResolvedValue({
    scanId: 's1',
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
    scanId: 's1',
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
  ui.setTheme('dark');
  ui.palette = 'violet';
});

describe('Disk Usage survives live theme/palette switches (12 combinations)', () => {
  it('keeps results, selection and zoom and never rescans', async () => {
    const store = duWorkspace.store;
    store.testData('s1', 'complete', ROOT);
    store.snapshot = snapshotFixture();
    store.treeNodes[ROOT] = {
      items: [child(DOCS)],
      total: 1,
      loading: false,
      loadingMore: false,
      error: null,
      loadId: 1,
    };
    duWorkspace.openFromPanes();
    const { container } = render(DiskUsageWorkspace);
    store.zoomIn(DOCS);
    store.selectNode(DOCS);
    await tick();

    const crumbs = () =>
      [...container.querySelectorAll('button.crumb')].map((c) => c.textContent?.trim());
    const before = {
      snapshot: store.snapshot,
      scanId: store.scanId,
      phase: store.phase,
      selected: store.selectedNodeId,
      zoom: [...store.zoomStack],
      crumbs: crumbs(),
    };
    expect(before.selected).toBe(DOCS);
    expect(before.zoom).toEqual([DOCS]);
    const startCalls = duMocks.start.mock.calls.length;
    const snapshotCalls = duMocks.snapshot.mock.calls.length;

    let combos = 0;
    for (const p of PALETTES) {
      for (const mode of ['dark', 'light'] as const) {
        ui.palette = p.value;
        ui.setTheme(mode);
        await tick();
        combos++;
        const root = document.documentElement;
        expect(root.dataset.palette).toBe(p.value);
        expect(root.dataset.theme).toBe(mode);
        expect(store.snapshot).toBe(before.snapshot);
        expect(store.scanId).toBe(before.scanId);
        expect(store.phase).toBe(before.phase);
        expect(store.selectedNodeId).toBe(before.selected);
        expect(store.zoomStack).toEqual(before.zoom);
        expect(crumbs()).toEqual(before.crumbs);
      }
    }
    expect(combos).toBe(12);
    expect(duMocks.start.mock.calls.length).toBe(startCalls);
    expect(duMocks.snapshot.mock.calls.length).toBe(snapshotCalls);
  });
});
