/**
 * DSK-04 — diskUsageStore tests: state transitions, scanId guard, async
 * query guards (selection/zoom/metric/filter), tree pagination, files
 * filters and NotFound → rescan. The IPC wrapper module is mocked (same
 * style as src/lib/ipc/backends.test.ts); events are fed through the same
 * onEvent path the real listener uses.
 */
import { beforeEach, describe, expect, it, vi } from 'vitest';

const duMocks = vi.hoisted(() => ({
  start: vi.fn(),
  cancel: vi.fn(),
  snapshot: vi.fn(),
  queryChildren: vi.fn(),
  topFiles: vi.fn(),
  cacheLoad: vi.fn(),
  cacheDelete: vi.fn(),
}));
vi.mock('../ipc/diskUsage', () => ({
  diskUsageStart: duMocks.start,
  diskUsageCancel: duMocks.cancel,
  diskUsageSnapshot: duMocks.snapshot,
  diskUsageQueryChildren: duMocks.queryChildren,
  diskUsageTopFiles: duMocks.topFiles,
  diskUsageCacheLoad: duMocks.cacheLoad,
  diskUsageCacheDelete: duMocks.cacheDelete,
  onDiskUsageEvent: vi.fn(async () => () => {}),
}));

const { createDiskUsageStore } = await import('./diskUsageStore.svelte');
const { FsError } = await import('../ipc/types');
// DSK-10: the singleton is gone — every test file builds its own instance.
const diskUsage = createDiskUsageStore();
import type {
  DiskUsageChildItem,
  DiskUsageChildrenPage,
  DiskUsageNode,
  DiskUsageProgressEvent,
  DiskUsageSnapshot,
  DiskUsageTopFiles,
} from '../ipc/types';

/** Drain the microtask queue (store methods are 1–2 awaits deep). */
async function flush(): Promise<void> {
  for (let i = 0; i < 12; i++) await Promise.resolve();
}

function ev(partial: Partial<DiskUsageProgressEvent>): DiskUsageProgressEvent {
  return {
    scanId: 's1',
    scannedFiles: 0,
    scannedDirs: 0,
    scannedSymlinks: 0,
    knownLogicalBytes: 0,
    knownAllocatedBytes: null,
    allocatedAvailableFiles: 0,
    allocatedUnavailableFiles: 0,
    coverage: 'complete',
    errors: [],
    totalErrors: 0,
    topChildren: [],
    done: false,
    cancelled: false,
    durationMs: 0,
    ...partial,
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

const ROOT = 'C:\\data';

function snapshotFixture(over: Partial<DiskUsageSnapshot> = {}): DiskUsageSnapshot {
  return {
    scanId: 's1',
    rootPath: ROOT,
    rootId: ROOT,
    version: 7,
    scannedAtMs: 1_759_000_000_000,
    rootNode: node(ROOT, {
      name: 'data',
      subtreeFiles: 3,
      subtreeDirs: 2,
      subtreeLogicalBytes: 3000,
      subtreeAllocatedBytes: 4096,
    }),
    stats: {
      scannedFiles: 3,
      scannedDirs: 2,
      scannedSymlinks: 0,
      skippedVolumeBoundaries: 0,
      logicalBytes: 3000,
      allocatedAvailableFiles: 3,
      allocatedUnavailableFiles: 0,
      knownAllocatedBytes: 4096,
      totalErrors: 0,
      coverage: 'complete',
      cancelled: false,
      depthCapped: false,
      durationMs: 12,
    },
    ...over,
  };
}

function pageFixture(over: Partial<DiskUsageChildrenPage> = {}): DiskUsageChildrenPage {
  return {
    scanId: 's1',
    nodeId: ROOT,
    version: 7,
    scannedAtMs: 1,
    metric: 'logical',
    coverage: 'complete',
    offset: 0,
    limit: 50,
    total: 0,
    items: [],
    ...over,
  };
}

function filesFixture(over: Partial<DiskUsageTopFiles> = {}): DiskUsageTopFiles {
  return {
    scanId: 's1',
    nodeId: ROOT,
    version: 7,
    scannedAtMs: 1,
    metric: 'logical',
    coverage: 'complete',
    limit: 20,
    totalMatching: 0,
    truncated: false,
    label: 'Top 20 files',
    items: [],
    ...over,
  };
}

beforeEach(() => {
  diskUsage.reset();
  duMocks.start.mockReset();
  duMocks.cancel.mockReset().mockResolvedValue(true);
  duMocks.snapshot.mockReset();
  duMocks.queryChildren.mockReset().mockResolvedValue(pageFixture());
  duMocks.topFiles.mockReset().mockResolvedValue(filesFixture());
  duMocks.cacheLoad.mockReset().mockResolvedValue(null);
  duMocks.cacheDelete.mockReset().mockResolvedValue(1);
});

describe('FER-464 cached scan (roots outside the index)', () => {
  const E = 'E:/usb';
  const cached = (over: Record<string, unknown> = {}) => ({
    summary: snapshotFixture({
      scanId: 'c1',
      rootPath: E,
      rootId: E,
      scannedAtMs: Date.now() - 2 * 86_400_000,
    }),
    engine: 'ntfsMft' as const,
    mutated: false,
    ...over,
  });

  it('opens as Stale with its age, keeps the engine, and queries the registered scan', async () => {
    duMocks.cacheLoad.mockResolvedValue(cached());
    await expect(diskUsage.loadCached(E)).resolves.toBe(true);
    await flush();
    expect(diskUsage.phase).toBe('stale');
    expect(diskUsage.fromCache).toBe(true);
    expect(diskUsage.engine).toBe('ntfsMft');
    expect(diskUsage.scanId).toBe('c1');
    expect(diskUsage.root).toBe(E);
    expect(diskUsage.chip.text).toBe('Stale · hasil 2 hari lalu');
    expect(diskUsage.hasData).toBe(true);
    expect(duMocks.queryChildren).toHaveBeenCalled();
    expect(diskUsage.stats?.coverage).toBe('complete');
  });

  it('no cache / damaged cache → false, store untouched (the caller scans)', async () => {
    await expect(diskUsage.loadCached(E)).resolves.toBe(false);
    expect(diskUsage.phase).toBe('idle');
    expect(diskUsage.fromCache).toBe(false);
  });

  it('never replaces a running scan', async () => {
    duMocks.start.mockResolvedValue('s1');
    diskUsage.rootInput = ROOT;
    await diskUsage.start();
    duMocks.cacheLoad.mockResolvedValue(cached());
    await expect(diskUsage.loadCached(E)).resolves.toBe(false);
    expect(diskUsage.phase).toBe('scanning');
  });

  it('a scan started while the cache was loading wins', async () => {
    let release!: (v: unknown) => void;
    duMocks.cacheLoad.mockReturnValue(new Promise((r) => (release = r)));
    const p = diskUsage.loadCached(E);
    duMocks.start.mockResolvedValue('s9');
    diskUsage.rootInput = ROOT;
    await diskUsage.start();
    release(cached());
    await expect(p).resolves.toBe(false);
    expect(diskUsage.phase).toBe('scanning');
    expect(diskUsage.fromCache).toBe(false);
  });

  it('mutation flag arrives from the backend and in-app mutations set it', async () => {
    duMocks.cacheLoad.mockResolvedValue(cached({ mutated: true }));
    await diskUsage.loadCached(E);
    expect(diskUsage.cacheMutated).toBe(true);
    diskUsage.reset();
    duMocks.cacheLoad.mockResolvedValue(cached());
    await diskUsage.loadCached(E);
    expect(diskUsage.cacheMutated).toBe(false);
    diskUsage.markStale();
    expect(diskUsage.cacheMutated).toBe(true);
    expect(diskUsage.phase).toBe('stale');
  });

  it('a fresh scan clears the cache labelling', async () => {
    duMocks.cacheLoad.mockResolvedValue(cached());
    await diskUsage.loadCached(E);
    duMocks.start.mockResolvedValue('n1');
    await diskUsage.start();
    expect(diskUsage.fromCache).toBe(false);
    expect(diskUsage.cacheMutated).toBe(false);
    expect(diskUsage.phase).toBe('scanning');
  });

  it('Hapus cache deletes by root and drops the cached label, keeping the numbers', async () => {
    duMocks.cacheLoad.mockResolvedValue(cached());
    await diskUsage.loadCached(E);
    await diskUsage.deleteCache();
    expect(duMocks.cacheDelete).toHaveBeenCalledWith(E);
    expect(diskUsage.fromCache).toBe(false);
    expect(diskUsage.hasData).toBe(true);
  });
});

describe('FER-460 fast scan engine + fallback notice', () => {
  it('fast start forwards the flag; events set engine and keep the fallback notice', async () => {
    duMocks.start.mockResolvedValue('f1');
    duMocks.snapshot.mockResolvedValue(snapshotFixture());
    diskUsage.rootInput = ROOT;
    await diskUsage.start(true);
    expect(duMocks.start).toHaveBeenCalledWith(ROOT, { maxDepth: null }, true);
    expect(diskUsage.engine).toBe('ntfsMft');
    expect(diskUsage.notice).toBe('');
    // UAC declined → the backend falls back to the walk with a notice.
    diskUsage.testEvent(
      ev({ scanId: 'f1', engine: 'walk', notice: 'Scan cepat tidak dipakai (izin admin ditolak)' }),
    );
    expect(diskUsage.engine).toBe('walk');
    expect(diskUsage.notice).toContain('izin admin ditolak');
    diskUsage.testEvent(ev({ scanId: 'f1', engine: 'walk', done: true }));
    await flush();
    expect(diskUsage.phase).toBe('complete');
    expect(diskUsage.notice).toContain('izin admin ditolak');
    // A new regular scan clears both.
    duMocks.start.mockResolvedValue('w2');
    await diskUsage.start();
    expect(diskUsage.engine).toBe('walk');
    expect(diskUsage.notice).toBe('');
  });
});

describe('FER-460 MFT read progress (percent is honest: total known)', () => {
  it('shows a percent while reading, "Menyusun hasil" at done===total, nothing for the walk', async () => {
    duMocks.start.mockResolvedValue('m1');
    duMocks.snapshot.mockResolvedValue(snapshotFixture());
    diskUsage.rootInput = ROOT;
    await diskUsage.start(true);
    expect(diskUsage.mftPercent).toBeNull();
    expect(diskUsage.fastScanStage()).toContain('Membaca MFT');
    diskUsage.testEvent(
      ev({ scanId: 'm1', engine: 'ntfsMft', mftRecordsDone: 1250, mftRecordsTotal: 5000 }),
    );
    expect(diskUsage.mftPercent).toBe(25);
    expect(diskUsage.fastScanStage()).toBe('Membaca MFT… 25%');
    expect(diskUsage.chip.text).toBe('Scanning… 25%');
    // A step backwards never lowers the percent.
    diskUsage.testEvent(
      ev({ scanId: 'm1', engine: 'ntfsMft', mftRecordsDone: 100, mftRecordsTotal: 5000 }),
    );
    expect(diskUsage.mftPercent).toBe(25);
    // Garbage above the total is clamped, never > 100 %.
    diskUsage.testEvent(
      ev({ scanId: 'm1', engine: 'ntfsMft', mftRecordsDone: 9999, mftRecordsTotal: 5000 }),
    );
    expect(diskUsage.mftPercent).toBeNull();
    expect(diskUsage.fastScanStage()).toBe('Menyusun hasil…');
    diskUsage.testEvent(ev({ scanId: 'm1', engine: 'ntfsMft', done: true }));
    await flush();
    expect(diskUsage.mftProgress).toBeNull();
    // Fallback walk events carry no MFT fields: no percent.
    duMocks.start.mockResolvedValue('w1');
    await diskUsage.start(true);
    diskUsage.testEvent(ev({ scanId: 'w1', engine: 'walk', scannedFiles: 5 }));
    expect(diskUsage.mftPercent).toBeNull();
    expect(diskUsage.chip.text).toBe('Scanning…');
  });
});

describe('state transitions', () => {
  it('start → scanning → complete with snapshot root/stats and initial queries', async () => {
    duMocks.start.mockResolvedValue('s1');
    duMocks.snapshot.mockResolvedValue(snapshotFixture());
    diskUsage.rootInput = ROOT;
    await diskUsage.start();
    expect(diskUsage.phase).toBe('scanning');
    expect(diskUsage.root).toBe(ROOT);
    expect(duMocks.start).toHaveBeenCalledWith(ROOT, { maxDepth: null });

    diskUsage.testEvent(ev({ scanId: 's1', scannedFiles: 10, scannedDirs: 2 }));
    expect(diskUsage.live.scannedFiles).toBe(10);
    expect(diskUsage.chip.text).toContain('Scanning');

    diskUsage.testEvent(
      ev({ scanId: 's1', done: true, scannedFiles: 30, scannedDirs: 12, knownLogicalBytes: 3000 }),
    );
    await flush();
    expect(diskUsage.phase).toBe('complete');
    expect(diskUsage.rootId).toBe(ROOT);
    expect(diskUsage.version).toBe(7);
    expect(diskUsage.stats?.scannedFiles).toBe(3);
    // Tree root children + treemap + files all queried from the snapshot.
    expect(duMocks.queryChildren).toHaveBeenCalled();
    expect(duMocks.topFiles).toHaveBeenCalled();
    expect(diskUsage.chip.text).toBe('Complete');
  });

  it('cancel ends as cancelled with coverage partial — never complete', async () => {
    duMocks.start.mockResolvedValue('s1');
    diskUsage.rootInput = ROOT;
    await diskUsage.start();
    diskUsage.testEvent(
      ev({
        scanId: 's1',
        done: true,
        cancelled: true,
        coverage: 'partial',
        scannedFiles: 4,
      }),
    );
    await flush();
    expect(diskUsage.phase).toBe('cancelled');
    expect(diskUsage.phase).not.toBe('complete');
    expect(diskUsage.chip.text).toContain('Cancelled');
  });

  it('partial coverage ends as partial (amber chip)', async () => {
    duMocks.start.mockResolvedValue('s1');
    duMocks.snapshot.mockResolvedValue(
      snapshotFixture({ stats: { ...snapshotFixture().stats, coverage: 'partial' } }),
    );
    diskUsage.rootInput = ROOT;
    await diskUsage.start();
    diskUsage.testEvent(ev({ scanId: 's1', done: true, coverage: 'partial', totalErrors: 3 }));
    await flush();
    expect(diskUsage.phase).toBe('partial');
    expect(diskUsage.chip.tone).toBe('warn');
  });

  it('a complete scan with zero entries is the empty state', async () => {
    duMocks.start.mockResolvedValue('s1');
    duMocks.snapshot.mockResolvedValue(
      snapshotFixture({
        stats: {
          ...snapshotFixture().stats,
          scannedFiles: 0,
          scannedDirs: 0,
          scannedSymlinks: 0,
        },
      }),
    );
    diskUsage.rootInput = ROOT;
    await diskUsage.start();
    diskUsage.testEvent(ev({ scanId: 's1', done: true }));
    await flush();
    expect(diskUsage.phase).toBe('empty');
    expect(diskUsage.chip.text).toContain('lokasi kosong');
  });

  it('a final event with an error lands in the error state without snapshot queries', async () => {
    duMocks.start.mockResolvedValue('s1');
    diskUsage.rootInput = ROOT;
    await diskUsage.start();
    diskUsage.testEvent(ev({ scanId: 's1', done: true, error: 'volume unreachable' }));
    await flush();
    expect(diskUsage.phase).toBe('error');
    expect(diskUsage.errorText).toBe('volume unreachable');
    expect(duMocks.snapshot).not.toHaveBeenCalled();
  });

  it('start refuses an empty root and never leaves idle', async () => {
    diskUsage.rootInput = '   ';
    await diskUsage.start();
    expect(diskUsage.phase).toBe('idle');
    expect(duMocks.start).not.toHaveBeenCalled();
  });

  it('markStale moves finished phases to stale with a time marker', async () => {
    duMocks.start.mockResolvedValue('s1');
    duMocks.snapshot.mockResolvedValue(snapshotFixture());
    diskUsage.rootInput = ROOT;
    await diskUsage.start();
    diskUsage.testEvent(ev({ scanId: 's1', done: true }));
    await flush();
    diskUsage.markStale();
    expect(diskUsage.phase).toBe('stale');
    expect(diskUsage.chip.tone).toBe('warn');
    expect(diskUsage.chip.text).toMatch(/Stale · \d\d:\d\d/);
  });
});

describe('scanId generation guard', () => {
  it('drops events of a foreign scanId', async () => {
    duMocks.start.mockResolvedValue('s1');
    diskUsage.rootInput = ROOT;
    await diskUsage.start();
    diskUsage.testEvent(ev({ scanId: 'other-scan', scannedFiles: 99, done: true }));
    await flush();
    expect(diskUsage.live.scannedFiles).toBe(0);
    expect(diskUsage.phase).toBe('scanning');
  });

  it('buffers events arriving before start resolves and replays the matching id', async () => {
    let resolveStart: (id: string) => void = () => {};
    duMocks.start.mockReturnValue(
      new Promise<string>((r) => {
        resolveStart = r;
      }),
    );
    diskUsage.rootInput = ROOT;
    const running = diskUsage.start();
    await flush();
    // Early event for the scan the backend is already emitting.
    diskUsage.testEvent(ev({ scanId: 'early-1', scannedFiles: 5 }));
    resolveStart('early-1');
    await running;
    await flush();
    expect(diskUsage.scanId).toBe('early-1');
    expect(diskUsage.live.scannedFiles).toBe(5);
    expect(diskUsage.phase).toBe('scanning');
  });

  it('drops buffered events when the resolved scanId does not match', async () => {
    let resolveStart: (id: string) => void = () => {};
    duMocks.start.mockReturnValue(
      new Promise<string>((r) => {
        resolveStart = r;
      }),
    );
    diskUsage.rootInput = ROOT;
    const running = diskUsage.start();
    await flush();
    diskUsage.testEvent(ev({ scanId: 'stale-scan', scannedFiles: 5 }));
    resolveStart('fresh-scan');
    await running;
    await flush();
    expect(diskUsage.scanId).toBe('fresh-scan');
    expect(diskUsage.live.scannedFiles).toBe(0);
  });
});

describe('async selection/zoom guard', () => {
  it('discards a files response that lands after the selection changed', async () => {
    diskUsage.testData('s1', 'complete', ROOT);
    let resolveStale: (r: DiskUsageTopFiles) => void = () => {};
    duMocks.topFiles
      .mockImplementationOnce(
        () =>
          new Promise<DiskUsageTopFiles>((r) => {
            resolveStale = r;
          }),
      )
      .mockResolvedValueOnce(filesFixture({ label: 'fresh scope', totalMatching: 2 }));

    void diskUsage.loadFiles(); // scope = root (seq A)
    await flush();
    diskUsage.selectNode(`${ROOT}\\sub`); // bumps the files seq, issues a new query
    await flush();
    expect(diskUsage.files?.result?.label).toBe('fresh scope');

    resolveStale(filesFixture({ label: 'STALE root scope' })); // old scope answers late
    await flush();
    expect(diskUsage.files?.result?.label).toBe('fresh scope');
    expect(diskUsage.selectedNodeId).toBe(`${ROOT}\\sub`);
  });

  it('discards tree pages of a superseded load and never resurrects a collapsed node', async () => {
    diskUsage.testData('s1', 'complete', ROOT);
    let resolveFirst: (p: DiskUsageChildrenPage) => void = () => {};
    duMocks.queryChildren
      .mockImplementationOnce(
        () =>
          new Promise<DiskUsageChildrenPage>((r) => {
            resolveFirst = r;
          }),
      )
      .mockResolvedValue(pageFixture({ nodeId: `${ROOT}\\sub`, items: [item('a')], total: 1 }));

    void diskUsage.loadChildren(ROOT); // load 1 (slow)
    await flush();
    diskUsage.toggleExpand(ROOT); // collapse → state deleted
    await diskUsage.loadChildren(ROOT); // load 2 for the fresh expand — wait, expand re-issues
    await flush();
    expect(diskUsage.treeNodes[ROOT]?.items).toHaveLength(1);

    resolveFirst(pageFixture({ items: [item('stale'), item('rows')], total: 2 })); // load 1 lands late
    await flush();
    expect(diskUsage.treeNodes[ROOT]?.items).toHaveLength(1); // stale page discarded
  });

  it('treemap zoom re-queries and the breadcrumb follows the zoom stack', async () => {
    diskUsage.testData('s1', 'complete', ROOT);
    duMocks.queryChildren.mockResolvedValue(
      pageFixture({ nodeId: `${ROOT}\\big`, items: [item('child')], total: 1 }),
    );
    diskUsage.zoomIn(`${ROOT}\\big`);
    await flush();
    expect(diskUsage.zoomStack).toEqual([`${ROOT}\\big`]);
    expect(duMocks.queryChildren).toHaveBeenCalledWith(
      's1',
      `${ROOT}\\big`,
      0,
      expect.any(Number),
      'largestFirst',
      { metric: 'logical' },
    );
    expect(diskUsage.crumbs().map((c) => c.label)).toEqual(['data', 'big']);
    diskUsage.zoomOut();
    expect(diskUsage.zoomStack).toEqual([]);
    diskUsage.zoomIn(`${ROOT}\\a`);
    diskUsage.zoomIn(`${ROOT}\\a\\b`);
    // DSK-07 B2: zoomTo takes the CRUMB index (0 = scan root). crumbs() is
    // [data, a, b] here — crumb 1 ("a") must land ON a, not one level deeper
    // (the old slice(0, index+1) off-by-one).
    diskUsage.zoomTo(1);
    expect(diskUsage.zoomStack).toEqual([`${ROOT}\\a`]);
    diskUsage.zoomTo(0);
    expect(diskUsage.zoomStack).toEqual([]);
  });

  it('DSK-07 B2 regression: breadcrumb jump lands on the clicked crumb, not one level deeper', () => {
    diskUsage.testData('s1', 'complete', ROOT);
    diskUsage.zoomIn(`${ROOT}\\a`);
    diskUsage.zoomIn(`${ROOT}\\a\\b`);
    diskUsage.zoomIn(`${ROOT}\\a\\b\\c`);
    expect(diskUsage.crumbs().map((c) => c.label)).toEqual(['data', 'a', 'b', 'c']);
    // crumbs()[i] IS zoomStack[i-1] for i ≥ 1 — jumping to crumb i keeps
    // zoomStack[0..i-1] (i = 0 empties the stack back to the scan root).
    diskUsage.zoomTo(2); // crumb "b"
    expect(diskUsage.zoomStack).toEqual([`${ROOT}\\a`, `${ROOT}\\a\\b`]);
    diskUsage.zoomTo(3); // last crumb = current level → unchanged
    expect(diskUsage.zoomStack).toEqual([`${ROOT}\\a`, `${ROOT}\\a\\b`]);
    diskUsage.zoomTo(0); // scan root → empty stack
    expect(diskUsage.zoomStack).toEqual([]);
    diskUsage.zoomIn(`${ROOT}\\a`);
    diskUsage.zoomTo(5); // out of range → ignored
    diskUsage.zoomTo(-1); // negative → ignored
    expect(diskUsage.zoomStack).toEqual([`${ROOT}\\a`]);
  });
});

describe('metric + files filters re-query the snapshot', () => {
  it('metric switch clears tree pages and re-queries under the new metric', async () => {
    diskUsage.testData('s1', 'complete', ROOT);
    await diskUsage.loadChildren(ROOT);
    const oldPage = diskUsage.treeNodes[ROOT];
    expect(oldPage).toBeDefined();
    diskUsage.setMetric('allocated');
    // The root's level-1 page is refetched right away (the root row is never
    // in `expanded`) — a fresh loading node replaces the stale page.
    expect(diskUsage.treeNodes[ROOT]).not.toBe(oldPage);
    expect(diskUsage.treeNodes[ROOT]?.loading).toBe(true);
    await flush();
    expect(diskUsage.treeNodes[ROOT]?.loading).toBe(false);
    expect(duMocks.queryChildren).toHaveBeenLastCalledWith(
      's1',
      ROOT,
      0,
      expect.any(Number),
      'largestFirst',
      { metric: 'allocated' },
    );
    expect(duMocks.topFiles).toHaveBeenLastCalledWith('s1', ROOT, expect.any(Number), {
      metric: 'allocated',
      minBytes: null,
      categories: null,
    });
  });

  it('DSK-07 B3 regression: metric switch refetches the ROOT page even though the root is never expanded', async () => {
    diskUsage.testData('s1', 'complete', ROOT);
    duMocks.queryChildren.mockResolvedValue(
      pageFixture({ metric: 'allocated', items: [item('kid')], total: 1 }),
    );
    // The root page arrives via refreshSnapshot's eager load — `expanded`
    // stays empty, so the old expanded-only refetch loop skipped the root
    // and the top of the tree went blank on every metric switch.
    await diskUsage.loadChildren(ROOT);
    expect(diskUsage.expanded.size).toBe(0);
    diskUsage.setMetric('allocated');
    await flush();
    expect(diskUsage.treeNodes[ROOT]?.items).toHaveLength(1);
    expect(diskUsage.treeNodes[ROOT]?.items[0].id).toBe('kid');
  });

  it('files filters are sent verbatim and identical filters do not re-query', async () => {
    diskUsage.testData('s1', 'complete', ROOT);
    const before = duMocks.topFiles.mock.calls.length;
    diskUsage.setFilesFilter(1024, ['video', 'archive']);
    await flush();
    expect(duMocks.topFiles).toHaveBeenLastCalledWith('s1', ROOT, expect.any(Number), {
      metric: 'logical',
      minBytes: 1024,
      categories: ['video', 'archive'],
    });
    const after = duMocks.topFiles.mock.calls.length;
    diskUsage.setFilesFilter(1024, ['video', 'archive']);
    await flush();
    expect(duMocks.topFiles.mock.calls.length).toBe(after);
    expect(after).toBeGreaterThan(before - 1);
  });
});

describe('tree pagination', () => {
  it('pages children with offset+limit until total is reached', async () => {
    diskUsage.testData('s1', 'complete', ROOT);
    const first = Array.from({ length: 50 }, (_, i) => item(`n${i}`));
    const second = Array.from({ length: 50 }, (_, i) => item(`m${i}`));
    const third = Array.from({ length: 20 }, (_, i) => item(`k${i}`));
    duMocks.queryChildren
      .mockResolvedValueOnce(pageFixture({ items: first, total: 120, offset: 0 }))
      .mockResolvedValueOnce(pageFixture({ items: second, total: 120, offset: 50 }))
      .mockResolvedValueOnce(pageFixture({ items: third, total: 120, offset: 100 }));

    await diskUsage.loadChildren(ROOT);
    expect(diskUsage.treeNodes[ROOT]?.items).toHaveLength(50);
    expect(diskUsage.treeNodes[ROOT]?.total).toBe(120);

    await diskUsage.loadChildren(ROOT, true);
    expect(duMocks.queryChildren).toHaveBeenLastCalledWith(
      's1',
      ROOT,
      50,
      expect.any(Number),
      'largestFirst',
      { metric: 'logical' },
    );
    expect(diskUsage.treeNodes[ROOT]?.items).toHaveLength(100);

    await diskUsage.loadChildren(ROOT, true);
    expect(diskUsage.treeNodes[ROOT]?.items).toHaveLength(120);
  });

  it('NotFound answers with needsRescan (snapshot evicted, gap G8)', async () => {
    diskUsage.testData('s1', 'complete', ROOT);
    duMocks.queryChildren.mockRejectedValueOnce(new FsError({ code: 'notFound', message: 'gone' }));
    await diskUsage.loadChildren(ROOT);
    expect(diskUsage.needsRescan).toBe(true);
    diskUsage.needsRescan = false;
    duMocks.snapshot.mockResolvedValueOnce(null);
    await diskUsage.refreshSnapshot();
    expect(diskUsage.needsRescan).toBe(true);
  });

  it('an evicted scanId in a page response (foreign scanId) is discarded', async () => {
    diskUsage.testData('s1', 'complete', ROOT);
    duMocks.queryChildren.mockResolvedValueOnce(
      pageFixture({ scanId: 'OLD-SCAN', items: [item('ghost')], total: 1 }),
    );
    await diskUsage.loadChildren(ROOT);
    expect(diskUsage.treeNodes[ROOT]?.items).toHaveLength(0);
    expect(diskUsage.needsRescan).toBe(false);
  });
});

describe('multi-instance factory (DSK-10, FER-408)', () => {
  it('two instances scan independently — events never leak between them', async () => {
    const left = createDiskUsageStore();
    const right = createDiskUsageStore();
    duMocks.start.mockResolvedValue('s-left');
    duMocks.snapshot.mockResolvedValue(snapshotFixture({ scanId: 's-left' }));
    left.rootInput = ROOT;
    await left.start();
    expect(left.phase).toBe('scanning');
    expect(right.phase).toBe('idle');

    // A progress event of the LEFT scan is dropped by the RIGHT instance's
    // own scanId guard and applied only by its owner.
    const progress = ev({ scanId: 's-left', scannedFiles: 11, scannedDirs: 1 });
    right.testEvent(progress);
    expect(right.live.scannedFiles).toBe(0);
    left.testEvent(progress);
    expect(left.live.scannedFiles).toBe(11);

    // Left finishing leaves right untouched, and right can scan WHILE left
    // is complete (two concurrent snapshots — the old singleton could not).
    left.testEvent(ev({ scanId: 's-left', done: true }));
    await flush();
    expect(left.phase).toBe('complete');
    expect(right.phase).toBe('idle');
    duMocks.start.mockResolvedValue('s-right');
    duMocks.snapshot.mockResolvedValue(snapshotFixture({ scanId: 's-right', rootId: 'D:\\other' }));
    right.rootInput = 'D:\\other';
    await right.start();
    expect(left.phase).toBe('complete');
    expect(right.phase).toBe('scanning');
    right.testEvent(ev({ scanId: 's-right', done: true }));
    await flush();
    expect(right.phase).toBe('complete');
    expect(left.scanId).toBe('s-left');
    expect(right.scanId).toBe('s-right');
  });

  it('a foreign done event for the other side never completes this side', async () => {
    const left = createDiskUsageStore();
    const right = createDiskUsageStore();
    duMocks.start.mockResolvedValueOnce('s1').mockResolvedValueOnce('s2');
    left.rootInput = ROOT;
    right.rootInput = 'D:\\x';
    await left.start();
    await right.start();
    left.testEvent(ev({ scanId: 's2', done: true, scannedFiles: 5 }));
    await flush();
    expect(left.phase).toBe('scanning'); // s2 belongs to the other instance
    expect(duMocks.snapshot).not.toHaveBeenCalled();
  });

  it('instances share nothing: the action gate and view state are per store', () => {
    const a = createDiskUsageStore();
    const b = createDiskUsageStore();
    a.duGate = { path: 'C:\\x', message: 'usang' };
    a.zoomIn('C:\\x\\sub');
    expect(b.duGate).toBeNull();
    expect(b.zoomStack).toEqual([]);
    a.reset();
    expect(a.duGate).toBeNull();
    expect(b.zoomStack).toEqual([]); // resetting one never touches the other
  });
});

describe('mode + reveal context', () => {
  it('rescan while scanning is refused; cancel routes to the backend', async () => {
    duMocks.start.mockResolvedValue('s1');
    diskUsage.rootInput = ROOT;
    await diskUsage.start();
    duMocks.start.mockClear();
    diskUsage.rootInput = 'C:\\elsewhere';
    await diskUsage.start();
    expect(duMocks.start).not.toHaveBeenCalled();
    expect(diskUsage.root).toBe(ROOT);
    await diskUsage.cancel();
    expect(duMocks.cancel).toHaveBeenCalledWith('s1');
  });

  // ── FER-407 regressions: results stay available after scan completion ──
  it('FER-407 user flow: rescan twice — the second scan replaces results, needsRescan stays false', async () => {
    duMocks.start.mockResolvedValueOnce('s1');
    duMocks.snapshot.mockResolvedValue(snapshotFixture({ scanId: 's1' }));
    diskUsage.rootInput = ROOT;
    await diskUsage.start();
    diskUsage.testEvent(ev({ scanId: 's1', done: true, scannedFiles: 30 }));
    await flush();
    expect(diskUsage.phase).toBe('complete');
    expect(diskUsage.scanId).toBe('s1');
    expect(diskUsage.needsRescan).toBe(false);

    // Second scan: new scanId, new snapshot, children queried under s2.
    duMocks.start.mockResolvedValueOnce('s2');
    duMocks.snapshot.mockResolvedValue(
      snapshotFixture({
        scanId: 's2',
        rootId: 'C:\\elsewhere',
        rootNode: node('C:\\elsewhere', { name: 'elsewhere' }),
      }),
    );
    diskUsage.rootInput = 'C:\\elsewhere';
    await diskUsage.start();
    diskUsage.testEvent(ev({ scanId: 's2', done: true, scannedFiles: 5, knownLogicalBytes: 500 }));
    await flush();
    expect(diskUsage.scanId).toBe('s2');
    expect(diskUsage.rootId).toBe('C:\\elsewhere');
    expect(diskUsage.snapshot?.scanId).toBe('s2');
    expect(diskUsage.needsRescan).toBe(false);
    expect(duMocks.queryChildren).toHaveBeenCalledWith(
      's2',
      'C:\\elsewhere',
      0,
      expect.any(Number),
      'largestFirst',
      { metric: 'logical' },
    );
  });

  it('FER-407 eviction: a finished scan whose snapshot was evicted goes needsRescan honestly, and a rescan clears it', async () => {
    diskUsage.testData('s1', 'complete', ROOT);
    duMocks.snapshot.mockResolvedValue(snapshotFixture());
    await diskUsage.refreshSnapshot();
    expect(diskUsage.needsRescan).toBe(false);
    // Eight newer scans evicted s1 on the backend: queries now answer NotFound.
    duMocks.snapshot.mockResolvedValue(null);
    duMocks.queryChildren.mockRejectedValue(
      new FsError({ code: 'notFound', message: 'snapshot evicted' }),
    );
    await diskUsage.refreshSnapshot();
    expect(diskUsage.needsRescan).toBe(true);
    // The honest recovery is a rescan — which resets the flag at start.
    duMocks.start.mockResolvedValue('s2');
    diskUsage.rootInput = ROOT;
    await diskUsage.start();
    expect(diskUsage.needsRescan).toBe(false);
    expect(diskUsage.phase).toBe('scanning');
  });

  it('FER-407 restart: a fresh store is idle — no restored dead scan, no fake partial chip', async () => {
    const { DiskUsageStore } = await import('./diskUsageStore.svelte');
    const fresh = new DiskUsageStore();
    expect(fresh.phase).toBe('idle');
    expect(fresh.scanId).toBeNull();
    expect(fresh.snapshot).toBeNull();
    expect(fresh.needsRescan).toBe(false);
  });
});
