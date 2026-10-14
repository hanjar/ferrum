/**
 * DSK-06.1/06.2 — diskUsageActions tests.
 *
 * Proven here: (a) revalidation refuses missing/changed/unverifiable snapshot
 * targets BEFORE any existing flow opens (mocked entry_properties IPC);
 * (b) Move/Delete are wired to the EXISTING surfaces only — the confirm
 * dialogs via dialogStore (planCopy/enqueueMove stay untouched until the
 * user confirms); (c) read-only actions ride the existing properties dialog
 * and clipboard; (d) the module exposes NO auto-clean/bulk-wipe surface.
 */
import { beforeEach, describe, expect, it, vi } from 'vitest';

const propsMocks = vi.hoisted(() => ({
  entryProperties: vi.fn(),
  occupiedSpace: vi.fn(),
  volumeInfo: vi.fn(),
  setAttributes: vi.fn(),
}));
vi.mock('../ipc/properties', () => propsMocks);

const mutMocks = vi.hoisted(() => ({
  planCopy: vi.fn(),
  createEntry: vi.fn(),
  deleteEntries: vi.fn(),
  enqueueCopy: vi.fn(),
  enqueueMove: vi.fn(),
  renameEntry: vi.fn(),
}));
vi.mock('../ipc/mutations', () => mutMocks);

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

import type { PaneApi } from '../stores/panesRegistry.svelte';

const { dialogs } = await import('../stores/dialogStore.svelte');
const { propertiesStore } = await import('../stores/propertiesStore.svelte');
const { registerPane } = await import('../stores/panesRegistry.svelte');
const { ui } = await import('../uiState.svelte');
const { FsError } = await import('../ipc/types');
// DSK-10: the gate is per store instance (no module-level duAction state).
const { createDiskUsageStore } = await import('./diskUsageStore.svelte');
const diskUsage = createDiskUsageStore();
const duActions = await import('./diskUsageActions.svelte');
const { duWorkspace } = await import('./duWorkspace.svelte');

const ROOT = 'C:\\data';
const FILE = `${ROOT}\\a.mkv`;

const target = { path: FILE, kind: 'file' as const, logicalBytes: 5000 };

function pane(path: string): PaneApi {
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

function props(over: Record<string, unknown> = {}) {
  return {
    path: FILE,
    name: 'a.mkv',
    kind: 'file',
    size: 5000,
    allocatedBytes: 5120,
    allocatedSupported: true,
    createdMs: 1,
    modifiedMs: 2,
    accessedMs: 3,
    readOnly: false,
    hidden: false,
    system: false,
    ...over,
  };
}

let unregisterOther: (() => void) | undefined;

beforeEach(() => {
  diskUsage.reset();
  duWorkspace.entryPane = 0;
  dialogs.close();
  propertiesStore.current = null;
  diskUsage.clearDuGate();
  ui.activePane = 0;
  registerPane(0, pane(ROOT));
  unregisterOther?.();
  unregisterOther = registerPane(1, pane('D:\\other'));
  propsMocks.entryProperties.mockReset();
  mutMocks.planCopy.mockReset();
  mutMocks.deleteEntries.mockReset();
  mutMocks.enqueueMove.mockReset();
});

describe('revalidation gate (DSK-06.2)', () => {
  it('a missing target is refused with the rescan gate and NOTHING opens', async () => {
    propsMocks.entryProperties.mockRejectedValue(
      new FsError({ code: 'notFound', message: 'gone' }),
    );
    await expect(duActions.deleteScanEntryWithConfirm(diskUsage, target)).resolves.toBe(false);
    expect(dialogs.current).toBeNull();
    expect(diskUsage.duGate).toMatchObject({ path: FILE });
    expect(diskUsage.duGate?.message).toContain('usang');
  });

  it('a changed file size is refused — the snapshot row is not the disk', async () => {
    propsMocks.entryProperties.mockResolvedValue(props({ size: 4 }));
    await expect(duActions.moveScanEntryToOtherPane(diskUsage, target)).resolves.toBe(false);
    expect(mutMocks.planCopy).not.toHaveBeenCalled();
    expect(dialogs.current).toBeNull();
    expect(diskUsage.duGate?.message).toContain('berubah');
  });

  it('a kind change (file → dir) is refused', async () => {
    propsMocks.entryProperties.mockResolvedValue(props({ kind: 'dir' }));
    await expect(duActions.deleteScanEntryWithConfirm(diskUsage, target)).resolves.toBe(false);
    expect(dialogs.current).toBeNull();
    expect(diskUsage.duGate?.message).toContain('Jenis');
  });

  it('an unverifiable target (stat error) is refused, never waved through', async () => {
    propsMocks.entryProperties.mockRejectedValue(
      new FsError({ code: 'permissionDenied', message: 'locked' }),
    );
    await expect(duActions.deleteScanEntryWithConfirm(diskUsage, target)).resolves.toBe(false);
    expect(dialogs.current).toBeNull();
    expect(diskUsage.duGate?.message).toContain('verifikasi');
  });

  it('a still-matching symlink passes revalidation (kind + size rules by kind)', async () => {
    propsMocks.entryProperties.mockResolvedValue(props({ kind: 'symlink', size: 0 }));
    await expect(
      duActions.deleteScanEntryWithConfirm(diskUsage, {
        path: FILE,
        kind: 'symlink',
        logicalBytes: 0,
      }),
    ).resolves.toBe(true);
    expect(dialogs.current).toMatchObject({ kind: 'delete', paths: [FILE] });
  });

  it('starting a new action clears the previous gate note', async () => {
    diskUsage.duGate = { path: FILE, message: 'old' };
    propsMocks.entryProperties.mockResolvedValue(props());
    await duActions.deleteScanEntryWithConfirm(diskUsage, target);
    expect(diskUsage.duGate).toBeNull();
  });

  it('DSK-10: a refusal on this store never appears on another store instance gate', async () => {
    const other = createDiskUsageStore();
    propsMocks.entryProperties.mockRejectedValue(
      new FsError({ code: 'notFound', message: 'gone' }),
    );
    await expect(duActions.deleteScanEntryWithConfirm(diskUsage, target)).resolves.toBe(false);
    expect(diskUsage.duGate).not.toBeNull();
    expect(other.duGate).toBeNull();
  });
});

describe('delete wiring (DSK-06.1) — the ONE existing delete path', () => {
  it('a valid target opens the existing delete confirm dialog; no direct IPC', async () => {
    propsMocks.entryProperties.mockResolvedValue(props());
    await expect(duActions.deleteScanEntryWithConfirm(diskUsage, target)).resolves.toBe(true);
    // Trash default carried by beginDelete — permanent stays a user choice.
    expect(dialogs.current).toMatchObject({ kind: 'delete', paths: [FILE], permanent: false });
    expect(mutMocks.deleteEntries).not.toHaveBeenCalled(); // only the dialog confirm may
  });

  it('a stale target is never handed to delete', async () => {
    propsMocks.entryProperties.mockRejectedValue(new FsError({ code: 'notFound', message: 'x' }));
    await expect(duActions.deleteScanEntryWithConfirm(diskUsage, target)).resolves.toBe(false);
    expect(mutMocks.deleteEntries).not.toHaveBeenCalled();
    expect(dialogs.current).toBeNull();
  });
});

describe('move wiring (DSK-06.1) — existing planCopy + confirm + BTM queue', () => {
  it('a valid target rides planCopy and opens the existing move confirm dialog', async () => {
    propsMocks.entryProperties.mockResolvedValue(props());
    mutMocks.planCopy.mockResolvedValue({ totalFiles: 1, totalBytes: 5000, conflicts: [] });
    // Entry pane 0 → destination is the OPPOSITE pane's folder (pane 1).
    await expect(duActions.moveScanEntryToOtherPane(diskUsage, target)).resolves.toBe(true);
    expect(mutMocks.planCopy).toHaveBeenCalledWith([FILE], 'D:\\other');
    expect(dialogs.current).toMatchObject({ kind: 'move', paths: [FILE], target: 'D:\\other' });
    expect(mutMocks.enqueueMove).not.toHaveBeenCalled(); // queued only on confirm
  });

  it('the destination is the pane OPPOSITE the entry pane, not ui.activePane', async () => {
    propsMocks.entryProperties.mockResolvedValue(props());
    mutMocks.planCopy.mockResolvedValue({ totalFiles: 1, totalBytes: 5000, conflicts: [] });
    ui.activePane = 1; // inactivePane() would now point at pane 0's folder
    duWorkspace.entryPane = 0; // entered from pane 0 → destination stays pane 1
    await duActions.moveScanEntryToOtherPane(diskUsage, target);
    expect(mutMocks.planCopy).toHaveBeenCalledWith([FILE], 'D:\\other');
    ui.activePane = 0;
    duWorkspace.entryPane = 0;
  });

  it('without a destination pane the move is refused and nothing opens', async () => {
    propsMocks.entryProperties.mockResolvedValue(props());
    unregisterOther?.();
    unregisterOther = undefined;
    await expect(duActions.moveScanEntryToOtherPane(diskUsage, target)).resolves.toBe(false);
    expect(dialogs.current).toBeNull();
    expect(diskUsage.duGate?.message).toContain('pane lain');
  });
});

describe('read-only row actions (existing surfaces)', () => {
  it('Properties opens the existing entry_properties dialog for the snapshot path', () => {
    duActions.openPropertiesFor(FILE);
    expect(propertiesStore.current).toMatchObject({ kind: 'properties', path: FILE });
  });

  it('Copy path uses the existing clipboard helper', async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, 'clipboard', {
      value: { writeText },
      configurable: true,
    });
    await expect(duActions.copyScanPath(FILE)).resolves.toBe(true);
    expect(writeText).toHaveBeenCalledWith(FILE);
  });

  it('Reveal delegates to the pane callback (existing navigation + selection)', () => {
    const onreveal = vi.fn();
    const items = duActions.duRowActionItems(diskUsage, target, {
      root: false,
      onreveal,
    });
    const first = items[0];
    if (!('separator' in first)) first.run();
    expect(onreveal).toHaveBeenCalledWith(FILE);
  });
});

describe('context menu items — no auto-clean, root stays read-only', () => {
  it('a non-root row offers Reveal/Properties/Copy path/Move/Delete — nothing else', () => {
    const items = duActions.duRowActionItems(diskUsage, target, {
      root: false,
      onreveal: vi.fn(),
    });
    const labels = items.map((i) => ('separator' in i ? '—' : i.label));
    expect(labels).toEqual([
      'Reveal in pane',
      'Properties',
      'Copy path',
      '—',
      'Move to other pane…',
      'Delete…',
    ]);
    for (const it of items) {
      if ('label' in it) expect(it.label).not.toMatch(/clean|wipe|auto/i);
    }
  });

  it('the scan root row never offers Move/Delete (read-only set only)', () => {
    const items = duActions.duRowActionItems(
      diskUsage,
      { path: ROOT, kind: 'dir', logicalBytes: 0 },
      { root: true, onreveal: vi.fn() },
    );
    const labels = items.map((i) => ('separator' in i ? '—' : i.label));
    expect(labels).toEqual(['Reveal in pane', 'Properties', 'Copy path']);
  });
});

describe('file row context menu items (FER-540) — no auto-clean', () => {
  it('offers Reveal, Copy path, Properties, Move, Delete', () => {
    const onreveal = vi.fn();
    const oncopy = vi.fn();
    const items = duActions.duFileRowActionItems(diskUsage, target, {
      onreveal,
      oncopy,
      copied: false,
    });
    const labels = items.map((i) => ('separator' in i ? '—' : i.label));
    expect(labels).toEqual(['Reveal', 'Copy path', 'Properties', 'Move…', 'Delete…']);
    for (const it of items) {
      if ('label' in it) expect(it.label).not.toMatch(/clean|wipe|auto/i);
    }

    // Copied feedback label
    const copiedItems = duActions.duFileRowActionItems(diskUsage, target, {
      onreveal,
      oncopy,
      copied: true,
    });
    expect(copiedItems[1]).toMatchObject({ label: 'Tersalin' });
  });

  it('Reveal is disabled when onreveal is not provided', () => {
    const items = duActions.duFileRowActionItems(diskUsage, target);
    expect(items[0]).toMatchObject({ label: 'Reveal', disabled: true });
  });
});

describe('no auto-clean surface (contract §2.3/§2.5)', () => {
  it('the module exports exactly the audited action surface', async () => {
    const mod = await import('./diskUsageActions.svelte');
    expect(Object.keys(mod).sort()).toEqual([
      'DU_SIZE_NOTE',
      'copyScanPath',
      'deleteScanEntryWithConfirm',
      'duFileRowActionItems',
      'duRowActionItems',
      'moveScanEntryToOtherPane',
      'openPropertiesFor',
      'revalidateScanTarget',
    ]);
    for (const key of Object.keys(mod)) {
      expect(key).not.toMatch(/clean|cleanup|wipe|autodelete/i);
    }
  });

  it('the standing note never promises recoverable space (DSK-06.4)', () => {
    expect(duActions.DU_SIZE_NOTE).toContain('≠');
    expect(duActions.DU_SIZE_NOTE).toContain('free space adalah metrik drive');
    // No claim that deleting adds free space — the anti-claim the contract
    // binds (§2.5): the note only warns why results differ.
    expect(duActions.DU_SIZE_NOTE.toLowerCase()).not.toContain('bertambah');
    expect(duActions.DU_SIZE_NOTE.toLowerCase()).not.toContain('meningkat');
  });
});
