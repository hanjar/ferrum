/**
 * DSK-04.3 — DiskUsageFiles component tests: verbatim Top-N label +
 * truncated marker, filter re-query (threshold/categories), allocated-null
 * honesty and Reveal/Copy actions. Double-click is asserted to be inert.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render } from '@testing-library/svelte';

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

describe('row actions', () => {
  it('Reveal hands the file path to the pane callback; Copy uses the clipboard', async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, 'clipboard', {
      value: { writeText },
      configurable: true,
    });
    preloadFiles();
    const onreveal = vi.fn();
    const { container, getAllByText } = renderFiles({ onreveal });
    const rows = [...container.querySelectorAll('tbody tr')];
    const revealBtn = rows[0].querySelector('button')!;
    revealBtn.click();
    expect(onreveal).toHaveBeenCalledWith(`${ROOT}\\a.mkv`);
    // Double-click on a row is inert — no reveal, no navigation.
    fireEvent.dblClick(rows[1]);
    expect(onreveal).toHaveBeenCalledTimes(1);
    getAllByText('Copy path')[0].click();
    await Promise.resolve();
    await Promise.resolve();
    expect(writeText).toHaveBeenCalledWith(`${ROOT}\\a.mkv`);
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

  it('DSK-06: Properties/Move/Delete row buttons ride the existing surfaces only', async () => {
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
    const rowButtons = (r: HTMLElement) =>
      [...r.querySelectorAll('button')].map((b) => b.textContent?.trim());
    // Reveal stays FIRST (existing contract), then Copy path, then DSK-06.
    expect(rowButtons(rows[0])).toEqual(['Reveal', 'Copy path', 'Properties', 'Move…', 'Delete…']);
    // Zero auto-clean affordance anywhere in the view.
    for (const b of [...container.querySelectorAll('button')]) {
      expect(b.textContent ?? '').not.toMatch(/clean|wipe|auto/i);
    }
    const btn = (r: HTMLElement, label: string) =>
      [...r.querySelectorAll('button')].find((b) => b.textContent?.trim() === label)!;
    // Properties → the existing entry_properties dialog.
    btn(rows[0], 'Properties').click();
    const { propertiesStore } = await import('../stores/propertiesStore.svelte');
    expect(propertiesStore.current).toMatchObject({ kind: 'properties', path: `${ROOT}\\a.mkv` });
    // Delete → revalidate → the ONE existing confirm dialog; no direct IPC.
    btn(rows[0], 'Delete…').click();
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
    btn(rows[0], 'Move…').click();
    await vi.waitFor(() =>
      expect(dialogs.current).toMatchObject({ kind: 'move', paths: [`${ROOT}\\a.mkv`] }),
    );
    expect(mutMocks.planCopy).toHaveBeenCalled();
    expect(mutMocks.enqueueMove).not.toHaveBeenCalled();
    unreg();
  });

  it('DSK-06: a stale row refuses Move/Delete with the rescan gate', async () => {
    const { FsError } = await import('../ipc/types');
    propsMocks.entryProperties.mockRejectedValue(new FsError({ code: 'notFound', message: 'x' }));
    preloadFiles();
    const { container } = renderFiles({ onreveal: vi.fn() });
    const rows = [...container.querySelectorAll('tbody tr')];
    const del = [...rows[0].querySelectorAll('button')].find(
      (b) => b.textContent?.trim() === 'Delete…',
    )!;
    del.click();
    // The refusal lands on THIS store's gate (per-instance since DSK-10).
    await vi.waitFor(() => expect(diskUsage.duGate?.path).toBe(`${ROOT}\\a.mkv`));
    expect(dialogs.current).toBeNull();
    expect(mutMocks.deleteEntries).not.toHaveBeenCalled();
  });
});
