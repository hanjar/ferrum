/**
 * DSK-04.3 — DiskUsageFiles component tests: verbatim Top-N label +
 * truncated marker, filter re-query (threshold/categories), allocated-null
 * honesty and Reveal/Copy actions. Double-click is asserted to be inert.
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

// DSK-06: row action wiring — revalidation stat + the existing transfer flows.
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

// DSK-10: the component takes its store instance as a prop (DSK-11: the ONE
// workspace session store — tests build their own).
const duStoreMod = await import('./diskUsageStore.svelte');
const diskUsage = duStoreMod.createDiskUsageStore();
type DiskUsageStore = ReturnType<typeof duStoreMod.createDiskUsageStore>;
const DiskUsageFiles = (await import('./DiskUsageFiles.svelte')).default;

/** Render with this file's store pre-wired; overrides allowed. */
function renderFiles(
  over: Partial<{ store: DiskUsageStore; onreveal: (path: string) => void }> = {},
) {
  return render(DiskUsageFiles, {
    store: diskUsage,
    onreveal: () => {},
    ...over,
  });
}
const { dialogs } = await import('../stores/dialogStore.svelte');
const { contextMenu } = await import('../stores/contextMenuStore.svelte');
const { loadDuFilesColumnWidths, DU_FILES_COLUMNS_KEY } = await import('./diskUsageFilesColumns');
import type { DiskUsageFileItem, DiskUsageTopFiles } from '../ipc/types';

const ROOT = 'C:\\data';

function fileItem(over: Partial<DiskUsageFileItem>): DiskUsageFileItem {
  return {
    id: `${ROOT}\\a.mkv`,
    parentId: ROOT,
    name: 'a.mkv',
    logicalBytes: 5000,
    allocatedBytes: 5120,
    allocatedSupported: true,
    category: 'video',
    percentOfParent: 50,
    percentOfScanRoot: 25,
    ...over,
  };
}

function resultFixture(over: Partial<DiskUsageTopFiles> = {}): DiskUsageTopFiles {
  return {
    scanId: 's1',
    nodeId: ROOT,
    version: 3,
    scannedAtMs: 1,
    metric: 'logical',
    coverage: 'complete',
    limit: 20,
    totalMatching: 2,
    truncated: false,
    label: 'Top 20 files',
    items: [
      fileItem({}),
      fileItem({
        id: `${ROOT}\\b.docx`,
        parentId: `${ROOT}\\docs`,
        name: 'b.docx',
        logicalBytes: 900,
        category: 'document',
        percentOfScanRoot: 5,
      }),
    ],
    ...over,
  };
}

function preloadFiles(over: Partial<DiskUsageTopFiles> = {}): void {
  diskUsage.testData('s1', 'complete', ROOT);
  diskUsage.files = { result: resultFixture(over), loading: false, error: null, seq: 1 };
}

beforeEach(() => {
  diskUsage.reset();
  duMocks.topFiles.mockReset().mockResolvedValue(resultFixture());
  propsMocks.entryProperties.mockReset();
  mutMocks.planCopy.mockReset();
  mutMocks.deleteEntries.mockReset();
  mutMocks.enqueueMove.mockReset();
});

afterEach(() => {
  cleanup();
  dialogs.close();
  contextMenu.close();
  globalThis.localStorage?.clear();
});

describe('rendering', () => {
  it('renders the label verbatim with truncated marker and full-path tooltips', () => {
    preloadFiles({ truncated: true, totalMatching: 90 });
    const { container, getByText } = renderFiles();
    expect(getByText('Top 20 files').textContent).toBe('Top 20 files'); // verbatim
    expect(getByText(/truncated/).textContent).toContain('truncated');
    const row = container.querySelector('tr[data-path$="a.mkv"]') as HTMLElement;
    expect(row?.getAttribute('title')).toBe(`${ROOT}\\a.mkv`); // FULL PATH tooltip
    expect(container.textContent).toContain('2/90 file');
  });

  it('shows unavailable (never logical) for null allocated under the allocated metric', () => {
    diskUsage.metric = 'allocated'; // no setMetric → no re-query side effects
    preloadFiles({
      metric: 'allocated',
      items: [fileItem({ allocatedBytes: null, allocatedSupported: false, logicalBytes: 9999 })],
    });
    const { container } = renderFiles();
    const cells = [...container.querySelectorAll('td.num')];
    expect(cells[0].textContent).toContain('unavailable');
    expect(cells[0].textContent).not.toContain('9999');
  });

  it('shows empty and loading states honestly', () => {
    preloadFiles({ items: [], totalMatching: 0 });
    const empty = renderFiles();
    expect(empty.container.textContent).toContain('Tidak ada file yang cocok');
    cleanup();
    diskUsage.files = { result: null, loading: true, error: null, seq: 2 };
    const loading = renderFiles();
    expect(loading.container.textContent).toContain('Memuat');
  });
});

describe('filters re-query the snapshot', () => {
  it('category checkboxes re-query with categories filter', async () => {
    preloadFiles();
    const { container } = renderFiles();
    const checkboxes = [...container.querySelectorAll('input[type="checkbox"]')];
    fireEvent.click(checkboxes[0]); // video
    await Promise.resolve();
    expect(duMocks.topFiles).toHaveBeenLastCalledWith('s1', ROOT, expect.any(Number), {
      metric: 'logical',
      minBytes: null,
      categories: ['video'],
    });
    fireEvent.click(checkboxes[0]); // untoggle
    await Promise.resolve();
    await Promise.resolve();
    expect(duMocks.topFiles).toHaveBeenLastCalledWith('s1', ROOT, expect.any(Number), {
      metric: 'logical',
      minBytes: null,
      categories: null, // empty category set is sent as null (no filter)
    });
  });

  it('the min-bytes threshold re-queries on change', async () => {
    preloadFiles();
    const { container } = renderFiles();
    const min = container.querySelector('input[type="number"]') as HTMLInputElement;
    // bind:value listens on `input`; the filter commits on `change`.
    fireEvent.input(min, { target: { value: '1024' } });
    fireEvent.change(min);
    await Promise.resolve();
    expect(duMocks.topFiles).toHaveBeenLastCalledWith('s1', ROOT, expect.any(Number), {
      metric: 'logical',
      minBytes: 1024,
      categories: null,
    });
  });
});

describe('row actions (FER-540)', () => {
  it('action button opens dropdown menu with five items and no auto-clean', async () => {
    preloadFiles();
    const { container } = renderFiles({ onreveal: vi.fn() });
    const rows = [...container.querySelectorAll<HTMLElement>('tbody tr')];
    // Each row has ONE action button
    const actBtns = rows[0].querySelectorAll('button');
    expect(actBtns).toHaveLength(1);
    const actBtn = actBtns[0];
    expect(actBtn.getAttribute('aria-haspopup')).toBe('menu');
    expect(actBtn.getAttribute('aria-expanded')).toBe('false');

    // Click opens context menu
    fireEvent.click(actBtn);
    expect(contextMenu.isOpen).toBe(true);
    expect(actBtn.getAttribute('aria-expanded')).toBe('true');

    const labels = contextMenu.items.map((i) => ('label' in i ? i.label : '—'));
    expect(labels).toEqual(['Reveal', 'Copy path', 'Properties', 'Move…', 'Delete…']);

    // Zero auto-clean affordance
    for (const it of contextMenu.items) {
      if ('label' in it) expect(it.label).not.toMatch(/clean|wipe|auto/i);
    }
  });

  it('Reveal hands the file path to the pane callback; Copy uses the clipboard and shows Tersalin feedback', async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, 'clipboard', {
      value: { writeText },
      configurable: true,
    });
    preloadFiles();
    const onreveal = vi.fn();
    const { container } = renderFiles({ onreveal });
    const rows = [...container.querySelectorAll('tbody tr')];
    const actBtn = rows[0].querySelector('button')!;

    // Reveal via dropdown menu
    fireEvent.click(actBtn);
    const revealItem = Array.from(
      document.querySelectorAll<HTMLButtonElement>('[role="menuitem"]'),
    ).find((b) => b.textContent?.trim() === 'Reveal')!;
    revealItem.click();
    expect(onreveal).toHaveBeenCalledWith(`${ROOT}\\a.mkv`);

    // Double-click on a row is inert — no reveal, no navigation.
    fireEvent.dblClick(rows[1]);
    expect(onreveal).toHaveBeenCalledTimes(1);

    // Copy path via dropdown menu
    fireEvent.click(actBtn);
    const copyItem = Array.from(
      document.querySelectorAll<HTMLButtonElement>('[role="menuitem"]'),
    ).find((b) => b.textContent?.trim() === 'Copy path')!;
    copyItem.click();
    await vi.waitFor(() => {
      expect(writeText).toHaveBeenCalledWith(`${ROOT}\\a.mkv`);
      expect(actBtn.textContent?.trim()).toBe('Tersalin');
    });
  });

  it('percent column uses the scan root denominator with parent in the title', () => {
    preloadFiles();
    const { container } = renderFiles();
    const row = container.querySelector('tr[data-path$="a.mkv"]') as HTMLElement;
    const cells = row.querySelectorAll('td');
    // DSK-07 B1: fixtures are 0–100 as the backend ships them — 25 renders
    // "25.0%", never the double-scaled "2500.0%".
    expect(cells[4].textContent).toBe('25.0%');
    expect(cells[4].textContent).not.toContain('2500.0%');
    expect(cells[4].getAttribute('title')).toContain('50.0%');
  });

  it('DSK-06: Properties/Move/Delete menu items ride the existing surfaces only', async () => {
    // Move needs a destination pane (the registry's pane 1).
    const { registerPane } = await import('../stores/panesRegistry.svelte');
    const paneApi = {
      path: 'D:\\other',
      selectedPaths: () => [],
      cursorEntry: () => undefined,
      refresh: () => {},
      newTab: () => {},
      closeCurrentTab: () => {},
      goUp: () => {},
      openCursorEntry: () => {},
    };
    const unreg = registerPane(1, paneApi);
    propsMocks.entryProperties.mockResolvedValue({
      path: `${ROOT}\\a.mkv`,
      name: 'a.mkv',
      kind: 'file',
      size: 5000, // matches the snapshot logicalBytes → revalidation passes
      allocatedBytes: 5120,
      allocatedSupported: true,
      createdMs: 1,
      modifiedMs: 2,
      accessedMs: 3,
      readOnly: false,
      hidden: false,
      system: false,
    });
    mutMocks.planCopy.mockResolvedValue({ totalFiles: 1, totalBytes: 5000, conflicts: [] });
    preloadFiles();
    const { container } = renderFiles({ onreveal: vi.fn() });
    const rows = [...container.querySelectorAll<HTMLElement>('tbody tr')];
    const actBtn = rows[0].querySelector('button')!;

    // Properties → the existing entry_properties dialog.
    fireEvent.click(actBtn);
    const propItem = Array.from(
      document.querySelectorAll<HTMLButtonElement>('[role="menuitem"]'),
    ).find((b) => b.textContent?.trim() === 'Properties')!;
    propItem.click();
    const { propertiesStore } = await import('../stores/propertiesStore.svelte');
    expect(propertiesStore.current).toMatchObject({ kind: 'properties', path: `${ROOT}\\a.mkv` });

    // Delete → revalidate → the ONE existing confirm dialog; no direct IPC.
    fireEvent.click(actBtn);
    const delItem = Array.from(
      document.querySelectorAll<HTMLButtonElement>('[role="menuitem"]'),
    ).find((b) => b.textContent?.trim() === 'Delete…')!;
    delItem.click();
    await vi.waitFor(() =>
      expect(dialogs.current).toMatchObject({
        kind: 'delete',
        paths: [`${ROOT}\\a.mkv`],
        permanent: false,
      }),
    );
    expect(mutMocks.deleteEntries).not.toHaveBeenCalled();
    dialogs.close();

    // Move → the existing planCopy + move confirm flow (queue untouched).
    fireEvent.click(actBtn);
    const moveItem = Array.from(
      document.querySelectorAll<HTMLButtonElement>('[role="menuitem"]'),
    ).find((b) => b.textContent?.trim() === 'Move…')!;
    moveItem.click();
    await vi.waitFor(() =>
      expect(dialogs.current).toMatchObject({ kind: 'move', paths: [`${ROOT}\\a.mkv`] }),
    );
    expect(mutMocks.planCopy).toHaveBeenCalled();
    expect(mutMocks.enqueueMove).not.toHaveBeenCalled();
    unreg();
  });

  it('DSK-06: a stale row refuses Move/Delete from menu with the rescan gate', async () => {
    const { FsError } = await import('../ipc/types');
    propsMocks.entryProperties.mockRejectedValue(new FsError({ code: 'notFound', message: 'x' }));
    preloadFiles();
    const { container } = renderFiles({ onreveal: vi.fn() });
    const rows = [...container.querySelectorAll('tbody tr')];
    const actBtn = rows[0].querySelector('button')!;

    fireEvent.click(actBtn);
    const delItem = Array.from(
      document.querySelectorAll<HTMLButtonElement>('[role="menuitem"]'),
    ).find((b) => b.textContent?.trim() === 'Delete…')!;
    delItem.click();

    // The refusal lands on THIS store's gate (per-instance since DSK-10).
    await vi.waitFor(() => expect(diskUsage.duGate?.path).toBe(`${ROOT}\\a.mkv`));
    expect(dialogs.current).toBeNull();
    expect(mutMocks.deleteEntries).not.toHaveBeenCalled();
  });

  it('Escape closes menu and focus returns to the action button', async () => {
    preloadFiles();
    const { container } = renderFiles({ onreveal: vi.fn() });
    const rows = [...container.querySelectorAll('tbody tr')];
    const actBtn = rows[0].querySelector('button')!;

    actBtn.focus();
    fireEvent.click(actBtn);
    expect(contextMenu.isOpen).toBe(true);

    fireEvent.keyDown(window, { key: 'Escape' });
    expect(contextMenu.isOpen).toBe(false);

    await vi.waitFor(() => expect(document.activeElement).toBe(actBtn));
  });

  it('right-click on a row opens the context menu', () => {
    preloadFiles();
    const { container } = renderFiles({ onreveal: vi.fn() });
    const rows = [...container.querySelectorAll('tbody tr')];

    fireEvent.contextMenu(rows[0]);
    expect(contextMenu.isOpen).toBe(true);
    const labels = contextMenu.items.map((i) => ('label' in i ? i.label : '—'));
    expect(labels).toEqual(['Reveal', 'Copy path', 'Properties', 'Move…', 'Delete…']);
  });
});

describe('column resizing (FER-540)', () => {
  it('drag handle changes column width and persists to localStorage', () => {
    preloadFiles();
    const { container } = renderFiles();

    const kindResizer = container.querySelector(
      '[aria-label="Ubah lebar kolom Jenis"]',
    ) as HTMLElement;
    expect(kindResizer).not.toBeNull();
    expect(kindResizer.getAttribute('role')).toBe('separator');
    expect(kindResizer.getAttribute('aria-orientation')).toBe('vertical');
    expect(kindResizer.getAttribute('aria-valuenow')).toBe('90');

    // Pointer drag
    fireEvent.pointerDown(kindResizer, { clientX: 100, button: 0, pointerId: 1 });
    fireEvent.pointerMove(kindResizer, { clientX: 140, pointerId: 1 });
    fireEvent.pointerUp(kindResizer, { pointerId: 1 });

    expect(kindResizer.getAttribute('aria-valuenow')).toBe('130');
    expect(loadDuFilesColumnWidths().kind).toBe(130);
  });

  it('keyboard ArrowLeft / ArrowRight on handle changes width by 16px', () => {
    preloadFiles();
    const { container } = renderFiles();

    const kindResizer = container.querySelector(
      '[aria-label="Ubah lebar kolom Jenis"]',
    ) as HTMLElement;
    kindResizer.focus();

    fireEvent.keyDown(kindResizer, { key: 'ArrowRight' });
    expect(kindResizer.getAttribute('aria-valuenow')).toBe('106'); // 90 + 16

    fireEvent.keyDown(kindResizer, { key: 'ArrowLeft' });
    expect(kindResizer.getAttribute('aria-valuenow')).toBe('90'); // 106 - 16

    fireEvent.keyDown(kindResizer, { key: 'ArrowLeft' });
    expect(kindResizer.getAttribute('aria-valuenow')).toBe('74'); // 90 - 16
    expect(loadDuFilesColumnWidths().kind).toBe(74);
  });

  it('minimum width is respected (120px for Name, 56px for other columns)', () => {
    preloadFiles();
    const { container } = renderFiles();

    const kindResizer = container.querySelector(
      '[aria-label="Ubah lebar kolom Jenis"]',
    ) as HTMLElement;
    for (let i = 0; i < 10; i++) {
      fireEvent.keyDown(kindResizer, { key: 'ArrowLeft' });
    }
    expect(kindResizer.getAttribute('aria-valuenow')).toBe('56');

    const nameResizer = container.querySelector(
      '[aria-label="Ubah lebar kolom Nama"]',
    ) as HTMLElement;
    for (let i = 0; i < 30; i++) {
      fireEvent.keyDown(nameResizer, { key: 'ArrowLeft' });
    }
    expect(nameResizer.getAttribute('aria-valuenow')).toBe('120');
  });

  it('double-click on handle resets column width to default', () => {
    preloadFiles();
    const { container } = renderFiles();

    const kindResizer = container.querySelector(
      '[aria-label="Ubah lebar kolom Jenis"]',
    ) as HTMLElement;
    fireEvent.keyDown(kindResizer, { key: 'ArrowRight' });
    expect(kindResizer.getAttribute('aria-valuenow')).toBe('106');

    fireEvent.dblClick(kindResizer);
    expect(kindResizer.getAttribute('aria-valuenow')).toBe('90');
    expect(loadDuFilesColumnWidths().kind).toBe(90);

    const nameResizer = container.querySelector(
      '[aria-label="Ubah lebar kolom Nama"]',
    ) as HTMLElement;
    fireEvent.keyDown(nameResizer, { key: 'ArrowRight' });
    fireEvent.dblClick(nameResizer);
    expect(loadDuFilesColumnWidths().name).toBeNull();
  });

  it('handles corrupted localStorage gracefully by falling back to defaults', () => {
    globalThis.localStorage?.setItem(DU_FILES_COLUMNS_KEY, '{corrupt json');
    preloadFiles();
    const { container } = renderFiles();

    const kindResizer = container.querySelector(
      '[aria-label="Ubah lebar kolom Jenis"]',
    ) as HTMLElement;
    expect(kindResizer.getAttribute('aria-valuenow')).toBe('90');
    const sizeResizer = container.querySelector(
      '[aria-label="Ubah lebar kolom Ukuran"]',
    ) as HTMLElement;
    expect(sizeResizer.getAttribute('aria-valuenow')).toBe('100');
  });

  it('handles extreme and nonsensical values (1e9, negative, string, NaN) by falling back to default widths', () => {
    globalThis.localStorage?.setItem(
      DU_FILES_COLUMNS_KEY,
      JSON.stringify({ name: 1e9, kind: 1e9, size: -50, percent: 'huge' }),
    );
    preloadFiles();
    const { container } = renderFiles();

    const kindResizer = container.querySelector(
      '[aria-label="Ubah lebar kolom Jenis"]',
    ) as HTMLElement;
    expect(kindResizer.getAttribute('aria-valuenow')).toBe('90');

    const sizeResizer = container.querySelector(
      '[aria-label="Ubah lebar kolom Ukuran"]',
    ) as HTMLElement;
    expect(sizeResizer.getAttribute('aria-valuenow')).toBe('100');

    const percentResizer = container.querySelector(
      '[aria-label="Ubah lebar kolom % root"]',
    ) as HTMLElement;
    expect(percentResizer.getAttribute('aria-valuenow')).toBe('70');
  });

  it('handles storage throwing errors on getItem and setItem without crashing', () => {
    const getItemSpy = vi.spyOn(Storage.prototype, 'getItem').mockImplementation(() => {
      throw new Error('SecurityError: Blocked storage');
    });
    const setItemSpy = vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => {
      throw new Error('QuotaExceeded');
    });

    preloadFiles();
    const { container } = renderFiles();

    const kindResizer = container.querySelector(
      '[aria-label="Ubah lebar kolom Jenis"]',
    ) as HTMLElement;
    expect(kindResizer.getAttribute('aria-valuenow')).toBe('90');

    // Resize via keyboard or drag does not crash when setItem throws
    expect(() => {
      fireEvent.keyDown(kindResizer, { key: 'ArrowRight' });
    }).not.toThrow();
    expect(kindResizer.getAttribute('aria-valuenow')).toBe('106');

    getItemSpy.mockRestore();
    setItemSpy.mockRestore();
  });

  it('reactive auto-width: resize observer changes aria-valuenow and first drag does not jump', async () => {
    let observerCallback: ResizeObserverCallback | undefined;
    class TestResizeObserver implements ResizeObserver {
      constructor(cb: ResizeObserverCallback) {
        observerCallback = cb;
      }
      observe() {}
      unobserve() {}
      disconnect() {}
    }
    const origRO = globalThis.ResizeObserver;
    globalThis.ResizeObserver = TestResizeObserver;

    try {
      preloadFiles();
      const { container } = renderFiles();
      const nameResizer = container.querySelector(
        '[aria-label="Ubah lebar kolom Nama"]',
      ) as HTMLElement;
      expect(nameResizer.getAttribute('aria-valuenow')).toBe('400');

      observerCallback?.(
        [
          {
            contentRect: { width: 340, height: 100 },
            borderBoxSize: [{ inlineSize: 340, blockSize: 100 }],
          } as unknown as ResizeObserverEntry,
        ],
        {} as ResizeObserver,
      );
      await tick();

      expect(nameResizer.getAttribute('aria-valuenow')).toBe('340');

      nameResizer.setPointerCapture = vi.fn();
      nameResizer.hasPointerCapture = vi.fn(() => true);
      nameResizer.releasePointerCapture = vi.fn();

      fireEvent.pointerDown(nameResizer, { button: 0, clientX: 500, pointerId: 1 });
      fireEvent.pointerMove(nameResizer, { clientX: 520, pointerId: 1 });
      fireEvent.pointerUp(nameResizer, { clientX: 520, pointerId: 1 });
      await tick();

      expect(nameResizer.getAttribute('aria-valuenow')).toBe('360');
      expect(loadDuFilesColumnWidths().name).toBe(360);
    } finally {
      globalThis.ResizeObserver = origRO;
    }
  });
});
