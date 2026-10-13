// FER-466: the local Disk Usage question runner. Pure: a fake port stands in for the workspace.
import { describe, expect, it, vi } from 'vitest';
import {
  describeDiskUsageAnswer,
  describeDiskUsageQuery,
  diskUsageWorkspacePort,
  relativeToRoot,
  visibleText,
  runDiskUsageQuery,
  type DiskUsageActiveScan,
  type DiskUsageQueryPort,
  type DuStoreLike,
} from './diskUsageQuery';
import type {
  DiskUsageChildItem,
  DiskUsageChildrenPage,
  DiskUsageFileItem,
  DiskUsageStats,
  DiskUsageTopFiles,
} from '../ipc/types';

const ROOT = 'D:\\Drive';

const stats: DiskUsageStats = {
  scannedFiles: 1200,
  scannedDirs: 80,
  scannedSymlinks: 2,
  skippedVolumeBoundaries: 0,
  logicalBytes: 5_000_000_000,
  allocatedAvailableFiles: 1200,
  allocatedUnavailableFiles: 0,
  knownAllocatedBytes: 5_100_000_000,
  totalErrors: 3,
  coverage: 'complete',
  cancelled: false,
  depthCapped: false,
  durationMs: 4200,
};

const scan = (over: Partial<DiskUsageActiveScan> = {}): DiskUsageActiveScan => ({
  scanId: 's1',
  root: ROOT,
  rootId: ROOT,
  viewId: `${ROOT}\\Projects`,
  viewLabel: 'Projects',
  metric: 'logical',
  phase: 'complete',
  engine: 'walk',
  scannedAtMs: 1,
  stats,
  ...over,
});

const file = (
  name: string,
  bytes: number,
  over: Partial<DiskUsageFileItem> = {},
): DiskUsageFileItem => ({
  id: `${ROOT}\\Projects\\${name}`,
  parentId: `${ROOT}\\Projects`,
  name,
  logicalBytes: bytes,
  allocatedBytes: bytes + 4096,
  allocatedSupported: true,
  category: 'video',
  percentOfParent: 50,
  percentOfScanRoot: 12.5,
  ...over,
});

const top = (
  items: DiskUsageFileItem[],
  over: Partial<DiskUsageTopFiles> = {},
): DiskUsageTopFiles => ({
  scanId: 's1',
  nodeId: `${ROOT}\\Projects`,
  version: 1,
  scannedAtMs: 1,
  metric: 'logical',
  coverage: 'complete',
  limit: 10,
  totalMatching: items.length,
  truncated: false,
  label: 'Top N',
  items,
  ...over,
});

const child = (name: string, kind: 'dir' | 'file', bytes: number): DiskUsageChildItem => ({
  id: `${ROOT}\\${name}`,
  name,
  kind,
  logicalBytes: bytes,
  allocatedBytes: null,
  allocatedSupported: false,
  category: 'other',
  percentOfParent: 10,
  percentOfScanRoot: 10,
});

const page = (
  items: DiskUsageChildItem[],
  over: Partial<DiskUsageChildrenPage> = {},
): DiskUsageChildrenPage => ({
  scanId: 's1',
  nodeId: ROOT,
  version: 1,
  scannedAtMs: 1,
  metric: 'logical',
  coverage: 'complete',
  offset: 0,
  limit: 200,
  total: items.length,
  items,
  ...over,
});

function port(over: Partial<DiskUsageQueryPort> = {}): DiskUsageQueryPort {
  return {
    available: () => true,
    current: () => scan(),
    topFiles: vi.fn(async () => top([file('big.mkv', 3e9), file('b.mkv', 2e9)])),
    children: vi.fn(async () => page([])),
    ...over,
  };
}

describe('relativeToRoot', () => {
  it('strips the scan root (case-insensitive) and falls back to the last segment', () => {
    expect(relativeToRoot('D:\\Drive\\a\\b.txt', 'd:\\drive')).toBe('a\\b.txt');
    expect(relativeToRoot('D:/Drive/a/b.txt', 'D:\\Drive\\')).toBe('a\\b.txt');
    expect(relativeToRoot('D:\\Drive', ROOT)).toBe('.');
    expect(relativeToRoot('E:\\other\\x.bin', ROOT)).toBe('x.bin');
  });
});

describe('runDiskUsageQuery', () => {
  it('largestFiles: runs on the view scope with the store metric and reports the root-relative path', async () => {
    const p = port();
    const a = await runDiskUsageQuery(p, { kind: 'largestFiles', limit: 2 });
    expect(p.topFiles).toHaveBeenCalledWith('s1', `${ROOT}\\Projects`, 2, {
      metric: 'logical',
      categories: null,
    });
    expect(a.status).toBe('files');
    if (a.status !== 'files') return;
    expect(a.items.map((i) => [i.name, i.rel, i.bytes, i.kind])).toEqual([
      ['big.mkv', 'Projects\\big.mkv', 3e9, 'file'],
      ['b.mkv', 'Projects\\b.mkv', 2e9, 'file'],
    ]);
    expect(a.items[0].path).toBe(`${ROOT}\\Projects\\big.mkv`);
    expect(a.scopeLabel).toBe('Projects');
    expect(a.truncated).toBe(false);
  });

  it('scope root + category use the root node and the category filter; limit defaults to 10', async () => {
    const p = port();
    await runDiskUsageQuery(p, { kind: 'largestFiles', scope: 'root', category: 'video' });
    expect(p.topFiles).toHaveBeenCalledWith('s1', ROOT, 10, {
      metric: 'logical',
      categories: ['video'],
    });
  });

  it('a Top-N is labelled as a cut list, never as all files', async () => {
    const p = port({
      topFiles: vi.fn(async () =>
        top([file('a', 9), file('b', 8)], { totalMatching: 5000, truncated: true }),
      ),
    });
    const a = await runDiskUsageQuery(p, { kind: 'largestFiles', limit: 2 });
    if (a.status !== 'files') throw new Error('expected files');
    expect(a.truncated).toBe(true);
    expect(a.totalMatching).toBe(5000);
    const d = describeDiskUsageAnswer(a);
    expect(d.text).toContain('dari 5,000');
    expect(d.text).toContain('bukan semua file');
  });

  it('allocated metric: unknown allocated bytes stay null, never logical in disguise', async () => {
    const p = port({
      current: () => scan({ metric: 'allocated' }),
      topFiles: vi.fn(async () => top([file('x', 7, { allocatedBytes: null })])),
    });
    const a = await runDiskUsageQuery(p, { kind: 'largestFiles' });
    if (a.status !== 'files') throw new Error('expected files');
    expect(a.items[0].bytes).toBeNull();
    expect(a.metric).toBe('allocated');
  });

  it('largestFolders keeps only folders, cut to the limit, with the direct-children caveat', async () => {
    const p = port({
      children: vi.fn(async () =>
        page([
          child('Big', 'dir', 900),
          child('movie.mkv', 'file', 800),
          child('Mid', 'dir', 500),
          child('Small', 'dir', 100),
        ]),
      ),
    });
    const a = await runDiskUsageQuery(p, { kind: 'largestFolders', scope: 'root', limit: 2 });
    expect(p.children).toHaveBeenCalledWith('s1', ROOT, 200, { metric: 'logical' });
    if (a.status !== 'folders') throw new Error('expected folders');
    expect(a.items.map((i) => i.name)).toEqual(['Big', 'Mid']);
    expect(a.truncated).toBe(true);
    expect(a.notes.join(' ')).toContain('tepat di bawah');
  });

  it('summary reads the stored stats; no stats means no numbers', async () => {
    const a = await runDiskUsageQuery(port(), { kind: 'summary' });
    if (a.status !== 'summary') throw new Error('expected summary');
    expect(a.summary).toMatchObject({ files: 1200, folders: 80, logicalBytes: 5_000_000_000 });
    const none = await runDiskUsageQuery(port({ current: () => scan({ stats: null }) }), {
      kind: 'summary',
    });
    expect(none).toMatchObject({ status: 'summary', summary: null });
  });

  it('partial, cancelled and stale scans say so', async () => {
    for (const [phase, text] of [
      ['partial', 'sebagian'],
      ['cancelled', 'dibatalkan'],
      ['stale', 'usang'],
    ] as const) {
      const a = await runDiskUsageQuery(port({ current: () => scan({ phase }) }), {
        kind: 'largestFiles',
      });
      if (a.status !== 'files') throw new Error('expected files');
      expect(a.notes.join(' '), phase).toContain(text);
      expect(describeDiskUsageAnswer(a).tone).toBe('warn');
    }
  });

  it('no scan on screen: an honest local answer that suggests a scan, no query is made', async () => {
    for (const reason of ['none', 'scanning', 'unusable'] as const) {
      const p = port({ current: () => ({ none: reason }) });
      const a = await runDiskUsageQuery(p, { kind: 'largestFiles' });
      expect(a).toEqual({ status: 'noScan', reason });
      expect(p.topFiles).not.toHaveBeenCalled();
    }
    expect(describeDiskUsageAnswer({ status: 'noScan', reason: 'none' }).text).toBe(
      'Belum ada hasil Disk Usage. Jalankan scan dulu.',
    );
  });

  it('a replaced snapshot (scanId differs) or an evicted one is reported, not painted', async () => {
    const swapped = port({ topFiles: vi.fn(async () => top([file('a', 1)], { scanId: 'other' })) });
    expect(await runDiskUsageQuery(swapped, { kind: 'largestFiles' })).toEqual({
      status: 'unavailable',
      reason: 'evicted',
    });
    const gone = port({
      topFiles: vi.fn(async () => {
        throw Object.assign(new Error('x'), { code: 'notFound' });
      }),
    });
    expect(await runDiskUsageQuery(gone, { kind: 'largestFiles' })).toEqual({
      status: 'unavailable',
      reason: 'evicted',
    });
    const broken = port({
      children: vi.fn(async () => {
        throw new Error('C:\\secret path leaked in error');
      }),
    });
    const a = await runDiskUsageQuery(broken, { kind: 'largestFolders' });
    expect(a).toEqual({ status: 'unavailable', reason: 'failed' });
    expect(JSON.stringify(a)).not.toContain('secret');
  });

  it('entries with control characters in a name or path are dropped', async () => {
    const p = port({
      topFiles: vi.fn(async () => top([file('ok.bin', 5), file('bad\u0007.bin', 9)])),
    });
    const a = await runDiskUsageQuery(p, { kind: 'largestFiles' });
    if (a.status !== 'files') throw new Error('expected files');
    expect(a.items.map((i) => i.name)).toEqual(['ok.bin']);
  });

  it('describes the question from the parsed intent only', () => {
    expect(describeDiskUsageQuery({ kind: 'largestFiles', category: 'video', scope: 'root' })).toBe(
      'file terbesar (video), seluruh scan',
    );
    expect(describeDiskUsageQuery({ kind: 'largestFolders' })).toBe(
      'folder terbesar, tampilan sekarang',
    );
    expect(describeDiskUsageQuery({ kind: 'summary' })).toBe('ringkasan hasil scan');
  });
});

describe('diskUsageWorkspacePort', () => {
  const store = (over: Partial<DuStoreLike> = {}): DuStoreLike => ({
    phase: 'complete',
    hasData: true,
    scanId: 's1',
    root: ROOT,
    rootId: ROOT,
    metric: 'logical',
    engine: 'walk',
    scannedAtMs: 5,
    stats,
    zoomNodeId: `${ROOT}\\Projects`,
    crumbs: () => [
      { id: ROOT, label: 'Drive' },
      { id: `${ROOT}\\Projects`, label: 'Projects' },
    ],
    ...over,
  });
  const io = { topFiles: vi.fn(), children: vi.fn() };
  const make = (open: boolean, s: DuStoreLike) =>
    diskUsageWorkspacePort({ open, store: s }, io as never);
  const idle = { hasData: false, phase: 'idle', scanId: null } as const;

  it('is offered while the workspace is open, or while a result is still held', () => {
    expect(make(true, store(idle)).available()).toBe(true);
    expect(make(false, store()).available()).toBe(true);
    expect(make(false, store(idle)).available()).toBe(false);
  });

  it('current(): the zoom folder is the view, the scan root is the root', () => {
    const c = make(true, store()).current();
    expect(c).toMatchObject({
      scanId: 's1',
      root: ROOT,
      rootId: ROOT,
      viewId: `${ROOT}\\Projects`,
      viewLabel: 'Projects',
    });
  });

  it('current(): running, idle, error and empty scans are not queryable', () => {
    expect(make(true, store({ phase: 'scanning', hasData: false })).current()).toEqual({
      none: 'scanning',
    });
    expect(make(true, store(idle)).current()).toEqual({ none: 'none' });
    expect(make(true, store({ phase: 'error', hasData: false })).current()).toEqual({
      none: 'unusable',
    });
    expect(make(true, store({ phase: 'empty' })).current()).toEqual({ none: 'unusable' });
  });
});

describe('visibleText', () => {
  it('marks format characters and leaves plain text alone', () => {
    expect(visibleText('a\u202Eb\u200Bc')).toBe('a\u27E8U+202E\u27E9b\u27E8U+200B\u27E9c');
    expect(visibleText('plain é.txt')).toBe('plain é.txt');
  });
});
