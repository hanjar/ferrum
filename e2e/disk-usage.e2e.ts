import { test, expect, type Page } from '@playwright/test';

/**
 * FER-325 (DSK-07.2) E2E, reworked for DSK-11 (FER-409): Disk usage scan/view
 * flow through the webview as ONE session divided across the dual-pane grid
 * (largest-first tree fills the left pane, Treemap|Files tabs fill the right,
 * global header on top). Mocks Tauri IPC (same pattern as
 * archive-navigation.e2e.ts) to serve the FROZEN contract fixture — root D:\
 * 25 GiB = 18 + 3 + 2 + 1.5 + 0.5 (design/disk-usage/README.md §Fixture; the
 * 0.5 GiB tail is spelled out as Documents + Music + Photos + one loose file
 * so largest-first order is deterministic).
 *
 * SCOPE HONESTY: this suite proves the Svelte store/components flow and the
 * generation guards (a late event of an old scanId never paints). It does NOT
 * prove the real Rust scanner/queries — those are covered by the Rust suite
 * (src-tauri). The mock mirrors the Rust wire contract faithfully: camelCase
 * payloads, percent values on the 0..100 scale as percent_value() ships them
 * (src-tauri/src/disk_usage.rs), allocated-null stays null.
 */

const KB = 1024;
const MB = 1024 * KB;
const GB = 1024 * MB; // fixture unit: GiB (base 1024), displayed as "GB"

interface SpecEntry {
  name: string;
  /** Logical bytes (files); dirs aggregate from children. */
  size?: number;
  /** Allocated bytes: omit = same as size; null = OS cannot report (G2). */
  alloc?: number | null;
  children?: SpecEntry[];
}

/** D:\Projects\helix-engine\target\debug\kernel.pdb (5 GiB — largest file). */
const KERNEL = 'D:\\Projects\\helix-engine\\target\\debug\\kernel.pdb';

const SPEC: SpecEntry = {
  name: 'D:',
  children: [
    {
      name: 'Projects', // 18 GiB (72%)
      children: [
        {
          name: 'helix-engine', // 12 GiB
          children: [
            {
              name: 'target', // 9 GiB
              children: [
                {
                  name: 'debug', // 8 GiB
                  children: [
                    { name: 'kernel.pdb', size: 5 * GB },
                    { name: 'build-assets.obj', size: 3 * GB },
                  ],
                },
                { name: 'debug.log', size: 1 * GB },
              ],
            },
            {
              name: 'docs', // 2 GiB
              children: [
                { name: 'architecture.pdf', size: 1.2 * GB },
                { name: 'guide.epub', size: 0.8 * GB },
              ],
            },
            { name: 'README.md', size: 1 * GB },
          ],
        },
        {
          name: 'node-modules-cache', // 6 GiB
          children: [
            { name: 'lodash.js', size: 4 * GB },
            { name: 'react.js', size: 2 * GB },
          ],
        },
      ],
    },
    { name: 'Archive', children: [{ name: 'backup-2026.zip', size: 3 * GB }] },
    {
      name: 'Backups', // 2 GiB logical, sparse: allocated 0.5 GiB
      children: [{ name: 'image.img', size: 2 * GB, alloc: 0.5 * GB }],
    },
    { name: 'Videos', children: [{ name: 'movie.mkv', size: 1.5 * GB }] },
    // The 0.5 GiB tail (2%):
    {
      name: 'Documents', // 0.2 GiB — allocated null subtree (cloud placeholder)
      children: [{ name: 'report.docx', size: 0.2 * GB, alloc: null }],
    },
    { name: 'Music', children: [{ name: 'song.flac', size: 0.15 * GB }] },
    { name: 'Photos', children: [{ name: 'pic.png', size: 0.1 * GB }] },
    { name: 'notes.txt', size: 0.05 * GB },
  ],
};

// Derived expectations (kept in one place; 18+3+2+1.5+0.5 = 25 exactly).
const EXPECTED_FILES = 15;
const EXPECTED_DIRS = 13;

interface MockModel {
  scanId: string;
  rootPath: string;
  rootId: string;
  version: number;
  scannedAtMs: number;
  nodes: Record<string, SpecNode>;
  filesByParent: Record<string, { name: string; logical: number; alloc: number | null }[]>;
  stats: Record<string, unknown>;
}

interface SpecNode {
  id: string;
  path: string;
  name: string;
  parentId?: string;
  childrenIds: string[];
  directFiles: number;
  directSymlinks: number;
  directLogicalBytes: number;
  directAllocatedBytes: number | null;
  subtreeFiles: number;
  subtreeDirs: number;
  subtreeSymlinks: number;
  subtreeLogicalBytes: number;
  subtreeAllocatedBytes: number | null;
  subtreeErrors: number;
  skippedVolumeBoundaries: number;
  subtreeSkippedVolumes: number;
}

function injectTauriMock(page: Page) {
  page.addInitScript((spec: SpecEntry) => {
    const GIB = 1024 * 1024 * 1024;
    const ROOT_ID = spec.name + '\\';
    // Module-scope consts are NOT visible inside the serialized init script —
    // the reveal target path is restated here on purpose.
    const KERNEL_PATH = 'D:\\Projects\\helix-engine\\target\\debug\\kernel.pdb';

    interface DuChildItem {
      id: string;
      name: string;
      kind: string;
      logicalBytes: number;
      allocatedBytes: number | null;
      allocatedSupported: boolean;
      category: string;
      percentOfParent: number | null;
      percentOfScanRoot: number | null;
      subtreeFiles?: number;
      subtreeDirs?: number;
      subtreeErrors?: number;
    }

    interface RunningScan {
      scanId: string;
      cancelled: boolean;
      completed: boolean;
      model: MockModel;
    }

    interface ListenArgs {
      event?: string;
      handler?: number;
    }
    interface ScanIdArgs {
      scanId?: string;
    }
    interface QueryArgs {
      scanId?: string;
      nodeId?: string;
      offset?: number;
      limit?: number;
      filter?: { metric?: string };
    }

    const state: {
      // Real Tauri delivers an event to every `listen` registration — the
      // map stays multi-listener even though DSK-11 subscribes exactly once
      // (one store); a single-slot map would silently drop extra listeners.
      handlers: Record<string, ((e: { event: string; id: number; payload: unknown }) => void)[]>;
      starts: string[];
      cancels: string[];
      scans: Map<string, RunningScan>;
      activeScanId: string | null;
      holdScan: boolean;
      scanStartWaiters: ((scan: RunningScan) => void)[];
      nextId: number;
    } = {
      handlers: {},
      starts: [],
      cancels: [],
      scans: new Map(),
      activeScanId: null,
      holdScan: false,
      scanStartWaiters: [],
      nextId: 1,
    };

    /** Build the full snapshot model of the frozen fixture. */
    function buildModel(scanId: string): MockModel {
      const nodes: Record<string, SpecNode> = {};
      const filesByParent: MockModel['filesByParent'] = {};
      let files = 0;
      let dirs = 0;
      let logical = 0;
      let knownAlloc: number | null = 0;
      let allocAvailable = 0;
      let allocUnavailable = 0;

      function walk(
        entry: SpecEntry,
        parentId: string | null,
      ): { logical: number; alloc: number | null; files: number; dirs: number } {
        if (!Array.isArray(entry.children)) {
          files++;
          const logicalBytes = entry.size ?? 0;
          const alloc = entry.alloc === undefined ? (entry.size ?? 0) : entry.alloc;
          if (alloc === null) {
            allocUnavailable++;
            knownAlloc = null; // one unreportable file poisons the aggregate
          } else {
            allocAvailable++;
            if (knownAlloc !== null) knownAlloc += alloc;
          }
          logical += logicalBytes;
          if (parentId !== null) {
            (filesByParent[parentId] ??= []).push({
              name: entry.name,
              logical: logicalBytes,
              alloc,
            });
          }
          return { logical: logicalBytes, alloc: alloc === null ? null : alloc, files: 1, dirs: 0 };
        }
        const id = parentId === null ? ROOT_ID : parentId + entry.name + '\\';
        dirs++;
        const node: SpecNode = {
          id,
          path: id,
          name: entry.name,
          parentId: parentId ?? undefined,
          childrenIds: [],
          directFiles: 0,
          directSymlinks: 0,
          directLogicalBytes: 0,
          directAllocatedBytes: 0,
          subtreeFiles: 0,
          subtreeDirs: 1,
          subtreeSymlinks: 0,
          subtreeLogicalBytes: 0,
          subtreeAllocatedBytes: 0,
          subtreeErrors: 0,
          skippedVolumeBoundaries: 0,
          subtreeSkippedVolumes: 0,
        };
        nodes[id] = node;
        if (parentId !== null) {
          node.parentId = parentId;
          nodes[parentId].childrenIds.push(id);
        }
        let sumLogical = 0;
        let sumAlloc: number | null = 0;
        let sumFiles = 0;
        let sumDirs = 1; // the directory itself (subtree roll-up)
        for (const child of entry.children) {
          const r = walk(child, id);
          sumLogical += r.logical;
          if (r.alloc === null || sumAlloc === null) sumAlloc = null;
          else sumAlloc += r.alloc;
          sumFiles += r.files;
          sumDirs += r.dirs;
          if (!Array.isArray(child.children)) {
            node.directFiles++;
            node.directLogicalBytes += r.logical;
          }
        }
        node.subtreeLogicalBytes = sumLogical;
        node.subtreeAllocatedBytes = sumAlloc;
        node.subtreeFiles = sumFiles;
        node.subtreeDirs = sumDirs;
        return { logical: sumLogical, alloc: sumAlloc, files: sumFiles, dirs: sumDirs };
      }

      walk(spec, null);
      return {
        scanId,
        rootPath: ROOT_ID,
        rootId: ROOT_ID,
        version: state.nextId,
        scannedAtMs: Date.now(),
        nodes,
        filesByParent,
        stats: {
          scannedFiles: files,
          scannedDirs: dirs,
          scannedSymlinks: 0,
          skippedVolumeBoundaries: 0,
          logicalBytes: logical,
          allocatedAvailableFiles: allocAvailable,
          allocatedUnavailableFiles: allocUnavailable,
          knownAllocatedBytes: knownAlloc,
          totalErrors: 0,
          coverage: 'complete',
          cancelled: false,
          depthCapped: false,
          durationMs: 90,
        },
      };
    }

    /** Rust percent_value mirror: null unless complete + Some/Some(>0). */
    function percent(num: number | null, den: number | null): number | null {
      if (num === null || den === null || den <= 0) return null;
      return (num / den) * 100.0; // 0..100 scale, exactly as the engine ships
    }

    function childItem(
      model: MockModel,
      parentId: string,
      dirId: string,
      metric: string,
    ): DuChildItem {
      const child = model.nodes[dirId];
      const parent = model.nodes[parentId];
      const root = model.nodes[model.rootId];
      const num = metric === 'allocated' ? child.subtreeAllocatedBytes : child.subtreeLogicalBytes;
      return {
        id: child.id,
        name: child.name,
        kind: 'dir',
        logicalBytes: child.subtreeLogicalBytes,
        allocatedBytes: child.subtreeAllocatedBytes,
        allocatedSupported: child.subtreeAllocatedBytes !== null,
        category: 'other',
        percentOfParent: percent(
          num,
          metric === 'allocated' ? parent.subtreeAllocatedBytes : parent.subtreeLogicalBytes,
        ),
        percentOfScanRoot: percent(
          num,
          metric === 'allocated' ? root.subtreeAllocatedBytes : root.subtreeLogicalBytes,
        ),
        subtreeFiles: child.subtreeFiles,
        subtreeDirs: child.subtreeDirs,
        subtreeErrors: child.subtreeErrors,
      };
    }

    function fileItem(
      model: MockModel,
      parentDirId: string,
      f: { name: string; logical: number; alloc: number | null },
      metric: string,
    ): DuChildItem & { parentId: string } {
      const root = model.nodes[model.rootId];
      const parent = model.nodes[parentDirId];
      const num = metric === 'allocated' ? f.alloc : f.logical;
      return {
        id: parentDirId + f.name,
        parentId: parentDirId,
        name: f.name,
        kind: 'file',
        logicalBytes: f.logical,
        allocatedBytes: f.alloc,
        allocatedSupported: f.alloc !== null,
        category: 'other',
        percentOfParent: percent(
          num,
          metric === 'allocated' ? parent.subtreeAllocatedBytes : parent.subtreeLogicalBytes,
        ),
        percentOfScanRoot: percent(
          num,
          metric === 'allocated' ? root.subtreeAllocatedBytes : root.subtreeLogicalBytes,
        ),
      };
    }

    function sortedChildren(model: MockModel, nodeId: string, metric: string): DuChildItem[] {
      const all: DuChildItem[] = [
        ...model.nodes[nodeId].childrenIds.map((c) => childItem(model, nodeId, c, metric)),
        ...(model.filesByParent[nodeId] ?? []).map((f) => fileItem(model, nodeId, f, metric)),
      ];
      all.sort((a, b) => {
        const va = metric === 'allocated' ? a.allocatedBytes : a.logicalBytes;
        const vb = metric === 'allocated' ? b.allocatedBytes : b.logicalBytes;
        if (va === null && vb === null) return a.id < b.id ? -1 : 1;
        if (va === null) return 1;
        if (vb === null) return -1;
        return vb - va || (a.id < b.id ? -1 : 1);
      });
      return all;
    }

    function emit(scanId: string, payload: Record<string, unknown>) {
      for (const handler of state.handlers['disk_usage://progress'] ?? []) {
        handler({
          event: 'disk_usage://progress',
          id: 1,
          payload: { scanId, ...payload },
        });
      }
    }

    function emitProgress(scan: RunningScan, scanId: string, frac: number): void {
      const s = scan.model.stats as {
        scannedFiles: number;
        scannedDirs: number;
        logicalBytes: number;
        knownAllocatedBytes: number | null;
      };
      const top = Object.values(scan.model.nodes)
        .filter((n) => n.parentId === scan.model.rootId)
        .sort((a, b) => b.subtreeLogicalBytes - a.subtreeLogicalBytes)
        .slice(0, 2);
      emit(scanId, {
        scannedFiles: Math.round(s.scannedFiles * frac),
        scannedDirs: Math.round(s.scannedDirs * frac),
        scannedSymlinks: 0,
        knownLogicalBytes: Math.round(s.logicalBytes * frac),
        knownAllocatedBytes:
          s.knownAllocatedBytes === null ? null : Math.round(s.knownAllocatedBytes * frac),
        allocatedAvailableFiles: 0,
        allocatedUnavailableFiles: 0,
        coverage: 'partial',
        errors: [],
        totalErrors: 0,
        topChildren: top,
        done: false,
        cancelled: false,
        durationMs: Math.round(90 * frac),
      });
    }

    function emitCancelled(scan: RunningScan, scanId: string): void {
      const s = scan.model.stats as {
        scannedFiles: number;
        scannedDirs: number;
        logicalBytes: number;
        knownAllocatedBytes: number | null;
      };
      emit(scanId, {
        scannedFiles: s.scannedFiles,
        scannedDirs: s.scannedDirs,
        scannedSymlinks: 0,
        knownLogicalBytes: s.logicalBytes,
        knownAllocatedBytes: s.knownAllocatedBytes,
        allocatedAvailableFiles: 0,
        allocatedUnavailableFiles: 0,
        coverage: 'partial', // cancelled is NEVER complete (contract §3.5)
        errors: [],
        totalErrors: 0,
        topChildren: [],
        done: true,
        cancelled: true,
        durationMs: 40,
      });
    }

    function emitComplete(scan: RunningScan, scanId: string): void {
      if (scan.cancelled || scan.completed) return;
      scan.completed = true;
      const s = scan.model.stats as Record<string, unknown>;
      emit(scanId, {
        ...s,
        coverage: 'complete',
        errors: [],
        topChildren: [],
        done: true,
        cancelled: false,
      });
    }

    function waitForActiveScan(): Promise<RunningScan> {
      const current = state.activeScanId ? state.scans.get(state.activeScanId) : undefined;
      if (current && !current.cancelled && !current.completed) {
        return Promise.resolve(current);
      }
      return new Promise<RunningScan>((resolve) => {
        state.scanStartWaiters.push(resolve);
      });
    }

    function startScan(): string {
      const scanId = `du-scan-${state.nextId}`;
      state.nextId++;
      state.starts.push(scanId);
      const scan: RunningScan = {
        scanId,
        cancelled: false,
        completed: false,
        model: buildModel(scanId),
      };
      state.scans.set(scanId, scan);
      state.activeScanId = scanId;
      const waiters = state.scanStartWaiters.splice(0);
      for (const w of waiters) w(scan);
      if (!state.holdScan) {
        emitProgress(scan, scanId, 0.3);
        emitProgress(scan, scanId, 0.6);
        emitComplete(scan, scanId);
      }
      return scanId;
    }

    (window as unknown as Record<string, unknown>).__duLog = {
      get starts(): string[] {
        return state.starts;
      },
    };
    (window as unknown as Record<string, unknown>).__duScan = {
      hold(): void {
        state.holdScan = true;
      },
      isActive(): boolean {
        const current = state.activeScanId ? state.scans.get(state.activeScanId) : undefined;
        return !!current && !current.cancelled && !current.completed;
      },
      async progress(frac: number): Promise<void> {
        const scan = await waitForActiveScan();
        if (!scan.cancelled && !scan.completed) {
          emitProgress(scan, scan.scanId, frac);
        }
      },
      async complete(): Promise<void> {
        const scan = await waitForActiveScan();
        state.holdScan = false;
        emitComplete(scan, scan.scanId);
      },
    };
    (window as unknown as Record<string, unknown>).__duEmit = (event: string, payload: unknown) => {
      for (const handler of state.handlers[event] ?? []) {
        handler({ event, id: 1, payload });
      }
    };

    window.__TAURI_INTERNALS__ = {
      metadata: {
        currentWindow: { label: 'main' },
        currentWebview: { windowLabel: 'main', label: 'main' },
      },
      plugins: {},
      transformCallback: (cb: unknown) => {
        const id = Math.floor(Math.random() * 1e9);
        (window as unknown as Record<string, unknown>)[`_${id}`] = cb;
        return id;
      },
      invoke: (cmd, args) => {
        if (cmd === 'plugin:path|resolve_directory') return Promise.resolve('C:\\Users\\demo');
        if (cmd === 'plugin:event|listen') {
          const a = (args ?? {}) as ListenArgs;
          if (typeof a.handler === 'number' && a.event) {
            const handler = (window as unknown as Record<string, (e: unknown) => void>)[
              `_${a.handler}`
            ];
            (state.handlers[a.event] ??= []).push(handler); // multi-listener
          }
          return Promise.resolve(Math.floor(Math.random() * 1e9));
        }
        if (cmd === 'watch_start' || cmd === 'watch_stop') return Promise.resolve(1);
        if (cmd === 'list_dir') {
          // Generic listing for every folder; includes the reveal target so
          // the Reveal flow can assert a real selected row in the FileList.
          return Promise.resolve([
            {
              name: 'kernel.pdb',
              path: KERNEL_PATH,
              isDir: false,
              isSymlink: false,
              size: 5 * GIB,
              modifiedMs: 1700000000000,
              hidden: false,
            },
            {
              name: 'sample.zip',
              path: 'C:\\fixtures\\sample.zip',
              isDir: false,
              isSymlink: false,
              size: 1024,
              modifiedMs: 1700000000000,
              hidden: false,
            },
            {
              name: 'other.txt',
              path: 'C:\\fixtures\\other.txt',
              isDir: false,
              isSymlink: false,
              size: 5,
              modifiedMs: 1700000000000,
              hidden: false,
            },
          ]);
        }
        if (cmd === 'list_drives') {
          return Promise.resolve([
            { label: 'Data', path: 'D:\\', freeBytes: 1e9, totalBytes: 5e11 },
          ]);
        }
        if (cmd === 'disk_usage_start') {
          return Promise.resolve(startScan()); // fixture ignores the typed root
        }
        if (cmd === 'disk_usage_cancel') {
          const a = (args ?? {}) as ScanIdArgs;
          const scanId = String(a.scanId ?? '');
          state.cancels.push(scanId);
          const scan = state.scans.get(scanId);
          if (!scan) return Promise.resolve(false);
          scan.cancelled = true;
          state.holdScan = false;
          emitCancelled(scan, scanId);
          return Promise.resolve(true);
        }
        if (cmd === 'disk_usage_snapshot') {
          const a = (args ?? {}) as ScanIdArgs;
          const scan = state.scans.get(String(a.scanId ?? ''));
          if (!scan) return Promise.resolve(null);
          // FER-479: the real backend answers with a bounded SUMMARY — the
          // scan root's aggregates (childrenIds empty), never the node map.
          const m = scan.model;
          return Promise.resolve({
            scanId: m.scanId,
            rootPath: m.rootPath,
            rootId: m.rootId,
            version: m.version,
            scannedAtMs: m.scannedAtMs,
            rootNode: { ...m.nodes[m.rootId], childrenIds: [] },
            stats: m.stats,
          });
        }
        if (cmd === 'disk_usage_query_children') {
          const a = (args ?? {}) as QueryArgs;
          const scan = state.scans.get(String(a.scanId ?? ''));
          const nodeId = String(a.nodeId ?? '');
          if (!scan || !scan.model.nodes[nodeId]) {
            return Promise.reject({ code: 'notFound', message: 'scan/node unknown (mock)' });
          }
          const metric = String(a.filter?.metric ?? 'logical');
          const all = sortedChildren(scan.model, nodeId, metric);
          const offset = Number(a.offset ?? 0);
          const limit = Number(a.limit ?? 50);
          return Promise.resolve({
            scanId: scan.model.scanId,
            nodeId,
            version: scan.model.version,
            scannedAtMs: scan.model.scannedAtMs,
            metric,
            coverage: 'complete',
            offset,
            limit,
            total: all.length,
            items: all.slice(offset, offset + limit),
          });
        }
        if (cmd === 'disk_usage_top_files') {
          const a = (args ?? {}) as QueryArgs;
          const scan = state.scans.get(String(a.scanId ?? ''));
          const nodeId = String(a.nodeId ?? '');
          if (!scan || !scan.model.nodes[nodeId]) {
            return Promise.reject({ code: 'notFound', message: 'scan/node unknown (mock)' });
          }
          const metric = String(a.filter?.metric ?? 'logical');
          // Iterative subtree walk — files only, like top_files_snapshot.
          const out: (DuChildItem & { parentId: string })[] = [];
          const stack = [nodeId];
          while (stack.length > 0) {
            const dir = stack.pop() as string;
            stack.push(...scan.model.nodes[dir].childrenIds);
            for (const f of scan.model.filesByParent[dir] ?? []) {
              out.push(fileItem(scan.model, dir, f, metric));
            }
          }
          out.sort((x, y) => {
            const vx = metric === 'allocated' ? x.allocatedBytes : x.logicalBytes;
            const vy = metric === 'allocated' ? y.allocatedBytes : y.logicalBytes;
            return (vy ?? -1) - (vx ?? -1) || (x.id < y.id ? -1 : 1);
          });
          const limit = Number(a.limit ?? 20);
          return Promise.resolve({
            scanId: scan.model.scanId,
            nodeId,
            version: scan.model.version,
            scannedAtMs: scan.model.scannedAtMs,
            metric,
            coverage: 'complete',
            limit,
            totalMatching: out.length,
            truncated: out.length > limit,
            label: `Top ${limit}`, // verbatim "Top N" label (contract §3.8)
            items: out.slice(0, limit),
          });
        }
        return Promise.reject(new Error(`Unknown command: ${cmd}`));
      },
    };
  }, SPEC);
}

test.describe('DSK-11 disk usage E2E — ONE session across the dual-pane grid (mock IPC)', () => {
  let page: Page;

  test.beforeEach(async ({ browser }) => {
    page = await browser.newPage();
    injectTauriMock(page);
    await page.goto('/');
    await page.waitForSelector('[data-path]', { state: 'attached', timeout: 10_000 });
  });

  test.afterEach(async () => {
    await page.close();
  });

  // DSK-11: ONE session split across the grid — the tree fills the LEFT
  // slot, the Treemap|Files tabs fill the RIGHT slot, the header is global.
  const workspace = () => page.getByTestId('du-workspace');
  const leftSlot = () => workspace().getByTestId('du-slot-left');
  const rightSlot = () => workspace().getByTestId('du-slot-right');
  const chip = () => workspace().locator('.chip');
  const tree = () => leftSlot().getByTestId('du-tree');
  const rowByName = (name: string) => tree().locator('.row').filter({ hasText: name });
  const counts = () => workspace().locator('.counts');
  const rootInput = () => workspace().getByLabel('Path root scan disk usage');
  const tablist = () => rightSlot().getByRole('tablist', { name: 'Tampilan kanan' });
  const tabpanel = () =>
    rightSlot().getByRole('tabpanel', {
      name: /Treemap|Files terbesar/,
    });
  const filesGrid = () => page.locator('.paneGrid');
  const startsLog = () =>
    page.evaluate(() => (window as unknown as { __duLog: { starts: string[] } }).__duLog.starts);
  const holdScan = () =>
    page.evaluate(() => (window as unknown as { __duScan: { hold: () => void } }).__duScan.hold());
  /** Fail fast (5 s, clear message) instead of hanging in the page-side waiter. */
  const expectScanActive = () =>
    expect
      .poll(
        () =>
          page.evaluate(() =>
            (window as unknown as { __duScan: { isActive: () => boolean } }).__duScan.isActive(),
          ),
        { timeout: 5_000, message: 'scan never started (held scan is not active)' },
      )
      .toBe(true);
  const emitScanProgress = async (frac: number) => {
    await expectScanActive();
    await page.evaluate(
      (f) =>
        (
          window as unknown as { __duScan: { progress: (frac: number) => Promise<void> } }
        ).__duScan.progress(f),
      frac,
    );
  };
  const completeScan = async () => {
    await expectScanActive();
    await page.evaluate(() =>
      (window as unknown as { __duScan: { complete: () => Promise<void> } }).__duScan.complete(),
    );
  };

  /** Enter the workspace via a pane toolbar button (Alt+Shift+D's same entry). */
  async function openWorkspace(button = 'Scan') {
    await page.locator('button[title="Disk usage (Alt+Shift+D)"]').first().click();
    await expect(workspace()).toBeVisible();
    await expect(filesGrid()).toBeHidden(); // the Files grid is mounted-hidden
    if (button !== null) {
      await rootInput().fill('D:\\');
      await workspace().getByRole('button', { name: button, exact: true }).click();
    }
  }

  async function scanToComplete(): Promise<string[]> {
    await openWorkspace();
    await expect(chip()).toHaveText('Complete', { timeout: 5_000 });
    await expect(rowByName('Projects')).toBeVisible();
    return startsLog();
  }

  test('entering from a pane opens the split workspace with the session idle', async () => {
    await openWorkspace(null);
    await expect(chip()).toHaveText('Idle — belum ada scan');
    // Slot A: the largest-first tree fills the left pane.
    await expect(tree()).toBeVisible();
    // Slot B: the Treemap|Files tabs fill the right pane.
    await expect(tablist().getByRole('tab', { name: 'Treemap' })).toBeVisible();
    await expect(tablist().getByRole('tab', { name: 'Files' })).toBeVisible();
    // The root is suggested from the entry pane's current folder (mock home).
    await expect(rootInput()).not.toHaveValue('');
  });

  test('scan runs progressive without percent/ETA, then completes with fixture totals', async () => {
    await holdScan();
    await openWorkspace();
    // Progressive rules (states.md rule 2): counts arrive, NO percent/ETA.
    await expect(workspace().getByText('tanpa persen-selesai dan tanpa ETA')).toBeVisible();
    await expect(chip()).toHaveText('Scanning…');
    await emitScanProgress(0.3);
    await emitScanProgress(0.6);
    // Preview rows stream from event topChildren before the final event.
    await expect(rowByName('Projects')).toBeVisible();
    await completeScan();
    // Final: complete chip + fixture counts (15 files / 13 folders).
    await expect(chip()).toHaveText('Complete', { timeout: 5_000 });
    await expect(counts()).toHaveText(`${EXPECTED_FILES} files · ${EXPECTED_DIRS} folders`);
    // Largest-first tree (LEFT slot): 18 > 3 > 2 > 1.5 > 0.2 > 0.15 > 0.1 > 0.05 GiB.
    const rows = tree().locator('.row');
    await expect(rows.nth(1)).toContainText('Projects');
    await expect(rows.nth(2)).toContainText('Archive');
    await expect(rows.nth(3)).toContainText('Backups');
    await expect(rows.nth(4)).toContainText('Videos');
    await expect(rows.nth(5)).toContainText('Documents');
    await expect(rows.nth(6)).toContainText('Music');
    await expect(rows.nth(7)).toContainText('Photos');
    await expect(rows.nth(8)).toContainText('notes.txt');
    // Root totals from the fixture (25 GiB → "25.0 GB" display, base 1024).
    await expect(rows.nth(0)).toContainText('25.0 GB');
    await expect(rows.nth(1)).toContainText('18.0 GB');
    // The RIGHT slot's treemap covers the same session scope (root children).
    await expect(tabpanel().locator('button.tile').filter({ hasText: 'Projects' })).toBeVisible();
  });

  test('selecting a tree row highlights its treemap tile and scopes the Files tab (ONE store)', async () => {
    await scanToComplete();
    await rowByName('Archive').click(); // a root child → a tile in the root treemap
    const tm = () => tabpanel().locator('button.tile');
    await expect(tm().filter({ hasText: 'Archive' })).toHaveClass(/sel/);
    // The Files tab follows the SAME selection through the ONE store.
    await rightSlot().getByRole('tab', { name: 'Files' }).click();
    const files = rightSlot().getByTestId('du-files');
    await expect(files.locator('.count')).toContainText('1/1 file');
    // The scope label is the raw node id (dirs keep their trailing separator).
    await expect(files.locator('.count')).toContainText('scope: D:\\Archive\\');
    // Selecting the scan root row instead → the same store re-scopes Files
    // to the whole root (15 files), still with no second instance involved.
    await tree().locator('.row').first().click();
    await expect(files.locator('.count')).toContainText('15/15 file');
  });

  test('zoom into a folder re-scopes the treemap; Up returns to the scan root', async () => {
    await scanToComplete();
    // Double-click a FOLDER row zooms (files never open — store test covers).
    await rowByName('Projects').dblclick();
    const crumb = workspace().locator('.crumbs .crumb').filter({ hasText: 'Projects' });
    await expect(crumb).toBeVisible();
    // Right view now queries the Projects subtree: helix-engine (12 GiB) leads.
    const tm = () => tabpanel().locator('button.tile');
    await expect(tm().filter({ hasText: 'helix-engine' })).toBeVisible();
    // Up walks one level back out; the root treemap shows Projects again.
    await rightSlot().getByRole('button', { name: 'Naik satu level (zoom out)' }).click();
    await expect(tm().filter({ hasText: 'Projects' })).toBeVisible();
  });

  test('Files tab lists Top-20 recursive files of the selected subtree', async () => {
    await scanToComplete();
    await rowByName('Projects').click(); // selection scopes the Files view
    await rightSlot().getByRole('tab', { name: 'Files' }).click();
    const files = rightSlot().getByTestId('du-files');
    await expect(files.locator('.label')).toHaveText('Top 20');
    // 8 files live under Projects — not truncated, explicit count shown.
    await expect(files.locator('.count')).toContainText('8/8 file');
    await expect(files.locator('tr[data-path]')).toHaveCount(8);
    const rows = files.locator('tr[data-path]');
    // Largest first with the stable id tie-break for the 1 GiB pair
    // (D:\Projects\helix-engine\README.md sorts before ...\debug\debug.log):
    // 5 > 4 > 3 > 2 > 1.2 > 1 > 1 > 0.8 GiB.
    await expect(rows.nth(0)).toHaveAttribute('data-path', KERNEL);
    await expect(rows.nth(1)).toHaveAttribute(
      'data-path',
      'D:\\Projects\\node-modules-cache\\lodash.js',
    );
    await expect(rows.nth(2)).toHaveAttribute(
      'data-path',
      'D:\\Projects\\helix-engine\\target\\debug\\build-assets.obj',
    );
    await expect(rows.nth(3)).toContainText('react.js');
    await expect(rows.nth(4)).toContainText('architecture.pdf');
    await expect(rows.nth(5)).toContainText('README.md');
    await expect(rows.nth(6)).toContainText('debug.log');
    await expect(rows.nth(7)).toContainText('guide.epub');
    // Null semantics survive real rendering (contract §2.2): under the
    // allocated metric the cloud placeholder shows "unavailable", never a
    // fabricated number.
    await rowByName('Documents').click();
    await workspace().getByRole('button', { name: 'Allocated' }).click();
    await expect(files.locator('tr[data-path]').filter({ hasText: 'report.docx' })).toContainText(
      'unavailable',
    );
  });

  test('Exit restores BOTH Files panes; re-entering keeps the result without a rescan', async () => {
    const starts = await scanToComplete();

    await workspace()
      .getByRole('button', { name: /Exit ke Files/ })
      .click();
    await expect(workspace()).toBeHidden();
    await expect(filesGrid()).toBeVisible(); // dual-pane Files is back
    await expect(page.locator('[data-pane="0"] .filesMode')).toBeVisible();
    await expect(page.locator('[data-pane="1"] .filesMode')).toBeVisible();

    // Re-enter (same entry path): the session shows its KEPT result and no
    // new scan starts (states.md — scan state survives Exit).
    await page.locator('button[title="Disk usage (Alt+Shift+D)"]').first().click();
    await expect(workspace()).toBeVisible();
    await expect(chip()).toHaveText('Complete');
    await expect(counts()).toHaveText(`${EXPECTED_FILES} files · ${EXPECTED_DIRS} folders`);
    const startsAfter = await startsLog();
    expect(startsAfter.length).toBe(starts.length); // no new scan anywhere
  });

  test('Reveal leaves the workspace and selects the file in the entry pane', async () => {
    await scanToComplete();
    await rightSlot().getByRole('tab', { name: 'Files' }).click();
    const files = rightSlot().getByTestId('du-files');
    const targetRow = files.locator(`tr[data-path="${KERNEL.replace(/\\/g, '\\\\')}"]`);
    await targetRow.locator('.actBtn').click();
    await page.getByRole('menuitem', { name: 'Reveal' }).click();
    // The workspace closes; the LEFT Files pane shows the parent folder with
    // the revealed file selected (existing navigation, never an execution).
    await expect(workspace()).toBeHidden();
    await expect(filesGrid()).toBeVisible();
    await expect(
      page.locator(`[data-pane="0"] [data-path="${KERNEL.replace(/\\/g, '\\\\')}"].selected`),
    ).toBeVisible();
  });

  test('cancel mid-scan lands on cancelled partial (never complete), totals kept', async () => {
    await holdScan();
    await openWorkspace();
    await emitScanProgress(0.3);
    await expect(chip()).toHaveText('Scanning…');
    await workspace().getByRole('button', { name: 'Cancel' }).first().click();
    await expect(chip()).toHaveText('Cancelled', { timeout: 5_000 });
    await expect(
      workspace().getByText('Scan dibatalkan — data parsial, BUKAN complete.'),
    ).toBeVisible();
    // Arrived totals stay visible after cancel (states.md).
    await expect(counts()).not.toHaveText('—');
  });

  test('rescan after cancel starts a NEW scan generation and completes', async () => {
    const first = await scanToComplete();
    // Already inside the workspace: the header button reads Rescan.
    await workspace().getByRole('button', { name: 'Rescan', exact: true }).click();
    await expect(chip()).toHaveText('Complete', { timeout: 5_000 });
    const starts = await startsLog();
    expect(starts.length).toBe(2);
    expect(new Set(starts).size).toBe(2); // fresh scanId, never a replay
    expect(starts[1]).not.toBe(first[0]);
    await expect(counts()).toHaveText(`${EXPECTED_FILES} files · ${EXPECTED_DIRS} folders`);
  });

  test('late progress event of an OLD scan never changes the current UI', async () => {
    const starts = await scanToComplete();
    const oldScanId = starts[0];
    const countsBefore = await counts().innerText();
    // A straggler event from the old scan (ghost numbers + a ghost row).
    await page.evaluate((scanId) => {
      const payload = {
        scanId,
        scannedFiles: 999_999,
        scannedDirs: 99_999,
        scannedSymlinks: 0,
        knownLogicalBytes: 99 * 1024 ** 3,
        knownAllocatedBytes: null,
        allocatedAvailableFiles: 0,
        allocatedUnavailableFiles: 0,
        coverage: 'partial',
        errors: [],
        totalErrors: 1,
        topChildren: [
          {
            id: 'D:\\GHOST',
            path: 'D:\\GHOST',
            name: 'GHOST',
            childrenIds: [],
            directFiles: 0,
            directSymlinks: 0,
            directLogicalBytes: 42,
            directAllocatedBytes: null,
            subtreeFiles: 0,
            subtreeDirs: 0,
            subtreeSymlinks: 0,
            subtreeLogicalBytes: 42,
            subtreeAllocatedBytes: null,
            subtreeErrors: 0,
            skippedVolumeBoundaries: 0,
            subtreeSkippedVolumes: 0,
          },
        ],
        done: false,
        cancelled: false,
        durationMs: 5,
      };
      (window as unknown as { __duEmit: (event: string, payload: unknown) => void }).__duEmit(
        'disk_usage://progress',
        payload,
      );
    }, oldScanId);
    // Nothing moved: chip, counts and tree stay on the CURRENT result.
    await expect(chip()).toHaveText('Complete');
    await expect(counts()).toHaveText(countsBefore);
    await expect(rowByName('GHOST')).toHaveCount(0);
  });

  test('narrow window: tree columns shrink to their floors and scroll — never clipped silently', async () => {
    await page.setViewportSize({ width: 720, height: 700 }); // left slot ≈ 360px
    // Enter via the REAL Alt+Shift+D shortcut (at this width the pane toolbar
    // buttons overlap between panes — the shortcut is the width-proof path).
    await page.keyboard.press('Alt+Shift+d');
    await expect(workspace()).toBeVisible();
    await rootInput().fill('D:\\');
    await workspace().getByRole('button', { name: 'Scan', exact: true }).click();
    await expect(chip()).toHaveText('Complete', { timeout: 5_000 });
    const treeEl = tree().locator('.tree');
    // The row respects its min-width (name floor + counts floors)…
    const rowWidth = await tree()
      .locator('.row')
      .first()
      .evaluate((el) => el.getBoundingClientRect().width);
    expect(rowWidth).toBeGreaterThanOrEqual(423); // DU_TREE_ROW_MIN_WIDTH = 424
    // …and the tree scrolls horizontally (a visible trace) instead of
    // clipping the counts columns without a scrollbar.
    const scroll = await treeEl.evaluate((el) => ({
      client: el.clientWidth,
      scrollW: el.scrollWidth,
    }));
    expect(scroll.scrollW).toBeGreaterThanOrEqual(scroll.client);
    expect(scroll.scrollW - scroll.client).toBeGreaterThan(4); // actually scrollable
  });

  // FER-499: ARIA tree keyboard model + two-way sync with the treemap.
  test('FER-499 keyboard: ↓ moves the cursor, → expands then steps into the folder, ← rises; breadcrumb follows the cursor', async () => {
    await scanToComplete();
    const treeEl = tree().locator('.tree');
    await treeEl.focus();
    await page.keyboard.press('Home'); // scan root row (the cursor starts on it after a scan)
    await page.keyboard.press('ArrowDown'); // Projects
    await page.keyboard.press('ArrowRight'); // closed → expand
    await expect(rowByName('helix-engine')).toBeVisible();
    await page.keyboard.press('ArrowRight'); // open → first child (helix-engine, 12 GiB)
    const crumbs = workspace().locator('.crumbs');
    await expect(crumbs).toContainText('helix-engine'); // treemap zoomed to the cursor folder
    await page.keyboard.press('ArrowLeft'); // closed folder: rise to its parent, Projects
    await expect(crumbs).toContainText('Projects');
    await expect(crumbs).not.toContainText('helix-engine');
    // helix-engine is depth 2: its chevron sits 32px in (16px per level).
    const indent = await rowByName('helix-engine')
      .locator('.chev')
      .evaluate((el) => el.style.paddingLeft);
    expect(indent).toBe('32px');
  });

  test('FER-499 keyboard: Enter toggles a folder; Home/End keep the cursor in range', async () => {
    await scanToComplete();
    const treeEl = tree().locator('.tree');
    await treeEl.focus();
    await page.keyboard.press('End');
    const last = await treeEl.getAttribute('aria-activedescendant');
    expect(last).not.toBeNull();
    await page.keyboard.press('Home');
    await page.keyboard.press('ArrowDown');
    await page.keyboard.press('Enter'); // root's first child is Projects (folder)
    await expect(rowByName('helix-engine')).toBeVisible();
    await page.keyboard.press('Enter');
    await expect(rowByName('helix-engine')).toHaveCount(0);
  });

  test('FER-499 treemap click moves the tree cursor to the same node', async () => {
    await scanToComplete();
    const tm = tabpanel().locator('button.tile').filter({ hasText: 'Archive' });
    await tm.click();
    const activeId = await tree().locator('.tree').getAttribute('aria-activedescendant');
    expect(activeId).not.toBeNull();
    const activeRow = tree().locator(`[id="${activeId}"]`);
    await expect(activeRow).toContainText('Archive');
  });

  // Selecting a folder in the tree focuses treemap and updates address
  test('focus: selecting a folder in tree updates toolbar address and focuses treemap', async () => {
    await scanToComplete();
    const projectsRow = rowByName('Projects');
    await projectsRow.click();

    // Address in workspace toolbar updates to D:\Projects
    const addr = rootInput();
    await expect(addr).toHaveValue(/^D:\\Projects\\?$/);

    // Breadcrumb updates to include Projects
    const crumbs = workspace().locator('.crumbs');
    await expect(crumbs).toContainText('Projects');

    // Treemap renders contents of Projects (helix-engine)
    const tm = tabpanel();
    await expect(tm.locator('.tile').filter({ hasText: 'helix-engine' })).toBeVisible();
  });

  // Visual evidence for the FER-499 review: set DU_SHOT_DIR to a folder to capture.
  test('FER-499 screenshots (env DU_SHOT_DIR) dark + light', async ({ browser }) => {
    const dir = process.env.DU_SHOT_DIR;
    test.skip(!dir, 'screenshot capture only when DU_SHOT_DIR is set');
    const { join } = await import('node:path');
    for (const scheme of ['dark', 'light'] as const) {
      const ctx = await browser.newContext({ colorScheme: scheme });
      const p = await ctx.newPage();
      injectTauriMock(p);
      await p.goto('/');
      await p.waitForSelector('[data-path]', { state: 'attached', timeout: 10_000 });
      await p.evaluate((t) => {
        document.documentElement.dataset.theme = t;
      }, scheme);
      await p.locator('button[title="Disk usage (Alt+Shift+D)"]').first().click();
      await p.getByTestId('du-workspace').getByLabel('Path root scan disk usage').fill('D:\\');
      await p
        .getByTestId('du-workspace')
        .getByRole('button', { name: 'Scan', exact: true })
        .click();
      await expect(p.getByTestId('du-workspace').locator('.chip')).toHaveText('Complete', {
        timeout: 5_000,
      });
      const treeEl = p.getByTestId('du-tree').locator('.tree');
      await treeEl.focus();
      await p.keyboard.press('Home');
      await p.keyboard.press('ArrowDown'); // Projects
      await p.keyboard.press('ArrowRight'); // expand
      await p.keyboard.press('ArrowRight'); // first child: helix-engine
      await p.waitForTimeout(400);
      await p.screenshot({ path: join(dir!, `after-${scheme}.png`) });
      await ctx.close();
    }
  });
});
