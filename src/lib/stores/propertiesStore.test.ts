import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { PaneApi } from './panesRegistry.svelte';

const entryPropertiesMock = vi.fn();
const occupiedSpaceMock = vi.fn();
const volumeInfoMock = vi.fn();
const setAttributesMock = vi.fn();
const cancelOccupiedMock = vi.fn();
const hostInfoMock = vi.fn();
let progressHandler: ((p: unknown) => void) | null = null;
vi.mock('../ipc/properties', () => ({
  entryProperties: (...a: unknown[]) => entryPropertiesMock(...a),
  occupiedSpace: (...a: unknown[]) => occupiedSpaceMock(...a),
  cancelOccupiedSpace: (...a: unknown[]) => cancelOccupiedMock(...a),
  onOccupiedProgress: async (h: (p: unknown) => void) => {
    progressHandler = h;
    return () => {
      progressHandler = null;
    };
  },
  volumeInfo: (...a: unknown[]) => volumeInfoMock(...a),
  setAttributes: (...a: unknown[]) => setAttributesMock(...a),
  hostInfo: (...a: unknown[]) => hostInfoMock(...a),
}));

const {
  propertiesStore,
  planAttributeChanges,
  openProperties,
  openAttributes,
  openOccupiedSpace,
  openVolumeInfo,
  openSystemInfo,
} = await import('./propertiesStore.svelte');
const { registerPane } = await import('./panesRegistry.svelte');
const { FsError } = await import('../ipc/types');
// Load the real registry and its transitive dependencies during collection so
// cold module transforms do not consume an individual test's timeout budget.
const { getAction, isEnabled, runAction, actionForEvent } = await import('../actions');
const { branch } = await import('./branchStore.svelte');

function register(selection: string[], path = 'C:\\dir'): void {
  const pane: PaneApi = {
    path,
    selectedPaths: () => selection,
    cursorEntry: () => undefined,
    refresh: () => {},
    newTab: () => {},
    closeCurrentTab: () => {},
    goUp: () => {},
    openCursorEntry: () => {},
  };
  registerPane(0, pane);
  registerPane(1, pane);
}

beforeEach(() => {
  entryPropertiesMock.mockReset();
  occupiedSpaceMock.mockReset();
  volumeInfoMock.mockReset();
  setAttributesMock.mockReset();
  cancelOccupiedMock.mockReset();
  hostInfoMock.mockReset();
  hostInfoMock.mockRejectedValue(new Error('no host'));
  progressHandler = null;
});

const entry = (over: Record<string, unknown> = {}) => ({
  path: 'C:\\dir\\a.txt',
  name: 'a.txt',
  kind: 'file',
  size: 5,
  allocatedBytes: 0,
  allocatedSupported: false,
  createdMs: 0,
  modifiedMs: 0,
  accessedMs: 0,
  readOnly: false,
  hidden: false,
  system: false,
  attributesEditable: true,
  ...over,
});

const report = (over: Record<string, unknown> = {}) => ({
  files: 2,
  dirs: 1,
  symlinks: 0,
  logicalBytes: 10,
  allocatedBytes: 8192,
  allocatedSupported: true,
  failed: [],
  cancelled: false,
  ...over,
});

afterEach(() => {
  propertiesStore.close();
});

describe('propertiesStore', () => {
  it('loads entry properties for the single selected path', async () => {
    entryPropertiesMock.mockResolvedValue({
      path: 'C:\\dir\\a.txt',
      name: 'a.txt',
      kind: 'file',
      size: 5,
      allocatedBytes: 4096,
      allocatedSupported: true,
      createdMs: 1,
      modifiedMs: 2,
      accessedMs: 3,
      readOnly: false,
      hidden: false,
      system: false,
    });
    register(['C:\\dir\\a.txt']);
    openProperties();
    expect(propertiesStore.current).toEqual({ kind: 'properties', path: 'C:\\dir\\a.txt' });
    await vi.waitFor(() => expect(propertiesStore.properties?.name).toBe('a.txt'));
    expect(entryPropertiesMock).toHaveBeenCalledWith('C:\\dir\\a.txt');
    expect(propertiesStore.loading).toBe(false);
  });

  it('refuses Properties without exactly one selection', () => {
    register(['C:\\a', 'C:\\b']);
    openProperties();
    expect(propertiesStore.current).toBeNull();
    register([]);
    openAttributes();
    expect(propertiesStore.current).toBeNull();
  });

  it('calculates occupied space for the whole selection', async () => {
    occupiedSpaceMock.mockResolvedValue({
      files: 2,
      dirs: 1,
      symlinks: 0,
      logicalBytes: 10,
      allocatedBytes: 8192,
      allocatedSupported: true,
      failed: [],
    });
    register(['C:\\dir\\a', 'C:\\dir\\b']);
    openOccupiedSpace();
    await vi.waitFor(() =>
      expect(occupiedSpaceMock).toHaveBeenCalledWith(expect.any(String), [
        'C:\\dir\\a',
        'C:\\dir\\b',
      ]),
    );
    await vi.waitFor(() => expect(propertiesStore.occupied?.files).toBe(2));
  });

  it('reads volume info from the active pane path', async () => {
    volumeInfoMock.mockResolvedValue({
      root: 'C:\\',
      label: 'OS',
      fsName: 'NTFS',
      totalBytes: 1,
      freeBytes: 0,
    });
    register([], 'C:\\Windows');
    openVolumeInfo();
    await vi.waitFor(() => expect(volumeInfoMock).toHaveBeenCalledWith('C:\\Windows'));
    await vi.waitFor(() => expect(propertiesStore.volume?.label).toBe('OS'));
    expect(propertiesStore.loading).toBe(false);
  });

  it('opens system info without IPC', () => {
    openSystemInfo();
    expect(propertiesStore.current).toEqual({ kind: 'system' });
    expect(entryPropertiesMock).not.toHaveBeenCalled();
  });

  it('opens Change Attributes for the whole selection and seeds shared/mixed choices', async () => {
    entryPropertiesMock.mockImplementation(async (p: string) =>
      p.endsWith('a.txt')
        ? entry({ readOnly: true, hidden: true })
        : entry({ path: p, readOnly: true }),
    );
    register(['C:\\dir\\a.txt', 'C:\\dir\\b.txt']);
    openAttributes();
    expect(propertiesStore.current).toEqual({
      kind: 'attributes',
      paths: ['C:\\dir\\a.txt', 'C:\\dir\\b.txt'],
    });
    await vi.waitFor(() => expect(propertiesStore.attributeItems).toHaveLength(2));
    expect(propertiesStore.attributeChoice).toEqual({ readOnly: true, hidden: null });
    expect(propertiesStore.attributesSupported).toBe(true);
  });

  it('previews per-item changes, then applies only changed items', async () => {
    entryPropertiesMock.mockImplementation(async (p: string) =>
      entry({ path: p, readOnly: p.endsWith('a.txt') }),
    );
    register(['C:\\dir\\a.txt', 'C:\\dir\\b.txt']);
    openAttributes();
    await vi.waitFor(() => expect(propertiesStore.attributeItems).toHaveLength(2));
    propertiesStore.attributeChoice.readOnly = true;
    propertiesStore.reviewAttributes();
    expect(propertiesStore.attributeStage).toBe('preview');
    expect(propertiesStore.attributePlan).toEqual([
      { path: 'C:\\dir\\a.txt', changes: [], readOnly: null, hidden: null },
      { path: 'C:\\dir\\b.txt', changes: ['Read-only: no → yes'], readOnly: true, hidden: null },
    ]);
    expect(setAttributesMock).not.toHaveBeenCalled(); // preview alone changes nothing
    setAttributesMock.mockResolvedValue(undefined);
    await propertiesStore.applyAttributes();
    expect(setAttributesMock).toHaveBeenCalledTimes(1);
    expect(setAttributesMock).toHaveBeenCalledWith('C:\\dir\\b.txt', true, null);
    expect(propertiesStore.attributeErrors).toEqual([]);
    expect(propertiesStore.attributesApplied).toBe(1);
    expect(propertiesStore.attributeStage).toBe('edit');
  });

  it('reports a failed attribute apply per item and keeps going', async () => {
    entryPropertiesMock.mockImplementation(async (p: string) => entry({ path: p }));
    register(['C:\\dir\\a.txt', 'C:\\dir\\b.txt']);
    openAttributes();
    await vi.waitFor(() => expect(propertiesStore.attributeItems).toHaveLength(2));
    propertiesStore.attributeChoice.hidden = true;
    propertiesStore.reviewAttributes();
    setAttributesMock.mockImplementation(async (p: string) => {
      if (p.endsWith('a.txt')) throw new FsError({ code: 'permissionDenied', message: 'locked' });
    });
    await propertiesStore.applyAttributes();
    expect(setAttributesMock).toHaveBeenCalledTimes(2); // b.txt still attempted
    expect(propertiesStore.attributeErrors).toEqual(['C:\\dir\\a.txt: locked']);
    expect(propertiesStore.attributesApplied).toBe(1);
    expect(propertiesStore.attributeStage).toBe('preview'); // stays reviewable
  });

  it('never applies or even previews when the platform cannot edit attributes (Unsupported)', async () => {
    entryPropertiesMock.mockResolvedValue(entry({ attributesEditable: false }));
    register(['C:\\dir\\a.txt']);
    openAttributes();
    await vi.waitFor(() => expect(propertiesStore.attributeItems).toHaveLength(1));
    expect(propertiesStore.attributesSupported).toBe(false);
    propertiesStore.attributeChoice.readOnly = true;
    propertiesStore.reviewAttributes();
    expect(propertiesStore.attributeStage).toBe('edit');
    await propertiesStore.applyAttributes();
    expect(setAttributesMock).not.toHaveBeenCalled();
  });

  it('skips unreadable items in the plan with their error', () => {
    const plan = planAttributeChanges(
      [
        { path: 'C:\\ok', props: entry() as never },
        { path: 'C:\\bad', props: null, error: 'gone' },
      ],
      { readOnly: true, hidden: null },
    );
    expect(plan[0].changes).toEqual(['Read-only: no → yes']);
    expect(plan[1]).toEqual({
      path: 'C:\\bad',
      changes: [],
      readOnly: null,
      hidden: null,
      error: 'gone',
    });
  });

  it('tracks occupied-space progress, then clears it when the report arrives', async () => {
    let finish: (r: unknown) => void = () => {};
    occupiedSpaceMock.mockImplementation(() => new Promise((res) => (finish = res)));
    register(['C:\\dir']);
    openOccupiedSpace();
    await vi.waitFor(() => expect(occupiedSpaceMock).toHaveBeenCalled());
    const scanId = occupiedSpaceMock.mock.calls[0][0] as string;
    expect(propertiesStore.occupiedRunning).toBe(true);
    progressHandler?.({ scanId: 'someone-else', files: 99, dirs: 0, symlinks: 0, logicalBytes: 1 });
    expect(propertiesStore.progress).toBeNull(); // other scans are ignored
    progressHandler?.({ scanId, files: 10, dirs: 2, symlinks: 0, logicalBytes: 100 });
    progressHandler?.({ scanId, files: 20, dirs: 3, symlinks: 0, logicalBytes: 250 });
    expect(propertiesStore.progress?.files).toBe(20);
    finish(report({ files: 30 }));
    await vi.waitFor(() => expect(propertiesStore.occupied?.files).toBe(30));
    expect(propertiesStore.progress).toBeNull();
    expect(propertiesStore.occupiedRunning).toBe(false);
    expect(propertiesStore.loading).toBe(false);
  });

  it('Cancel asks Rust to stop the walk and shows the partial, cancelled report', async () => {
    let finish: (r: unknown) => void = () => {};
    occupiedSpaceMock.mockImplementation(() => new Promise((res) => (finish = res)));
    register(['C:\\dir']);
    openOccupiedSpace();
    await vi.waitFor(() => expect(occupiedSpaceMock).toHaveBeenCalled());
    const scanId = occupiedSpaceMock.mock.calls[0][0] as string;
    propertiesStore.cancelOccupied();
    expect(cancelOccupiedMock).toHaveBeenCalledWith(scanId);
    expect(propertiesStore.cancelling).toBe(true);
    propertiesStore.cancelOccupied(); // idempotent
    expect(cancelOccupiedMock).toHaveBeenCalledTimes(1);
    finish(report({ cancelled: true, files: 3 }));
    await vi.waitFor(() => expect(propertiesStore.occupied?.cancelled).toBe(true));
    expect(propertiesStore.cancelling).toBe(false);
  });

  it('closing the dialog while busy cancels the walk and drops the late result', async () => {
    let finish: (r: unknown) => void = () => {};
    occupiedSpaceMock.mockImplementation(() => new Promise((res) => (finish = res)));
    register(['C:\\dir']);
    openOccupiedSpace();
    await vi.waitFor(() => expect(occupiedSpaceMock).toHaveBeenCalled());
    const scanId = occupiedSpaceMock.mock.calls[0][0] as string;
    propertiesStore.close();
    expect(cancelOccupiedMock).toHaveBeenCalledWith(scanId);
    expect(propertiesStore.current).toBeNull();
    expect(propertiesStore.occupiedRunning).toBe(false);
    finish(report({ cancelled: true }));
    await Promise.resolve();
    expect(propertiesStore.occupied).toBeNull(); // result of the closed dialog is ignored
  });

  it('opening another dialog cancels a running walk too', async () => {
    occupiedSpaceMock.mockImplementation(() => new Promise(() => {}));
    register(['C:\\dir']);
    openOccupiedSpace();
    await vi.waitFor(() => expect(occupiedSpaceMock).toHaveBeenCalled());
    openSystemInfo();
    expect(cancelOccupiedMock).toHaveBeenCalledTimes(1);
  });

  it('keeps the error code so the UI can explain Unsupported platforms', async () => {
    volumeInfoMock.mockRejectedValue(
      new FsError({ code: 'unsupported', message: 'Volume information is not supported' }),
    );
    register([], '\\\\srv\\share');
    openVolumeInfo();
    await vi.waitFor(() => expect(propertiesStore.errorCode).toBe('unsupported'));
    expect(propertiesStore.volume).toBeNull();
  });

  it('adds host info to System Information when available, silently without it', async () => {
    hostInfoMock.mockResolvedValue({ os: 'windows', arch: 'x86_64', family: 'windows' });
    openSystemInfo();
    await vi.waitFor(() => expect(propertiesStore.host?.os).toBe('windows'));
    hostInfoMock.mockRejectedValue(new Error('x'));
    openSystemInfo();
    await Promise.resolve();
    expect(propertiesStore.host).toBeNull();
  });

  it('surfaces load errors and never opens a broken dialog silently', async () => {
    entryPropertiesMock.mockRejectedValue(new FsError({ code: 'notFound', message: 'gone' }));
    register(['C:\\gone.txt']);
    openProperties();
    await vi.waitFor(() => expect(propertiesStore.error).toBe('gone'));
    expect(propertiesStore.properties).toBeNull();
  });
});

describe('F2-21 action registry integration', () => {
  it('binds Properties to Alt+Enter, Occupied Space to Ctrl+L, and the read-only info actions', () => {
    register(['C:\\dir\\a.txt']);
    const props = getAction('file.properties')!;
    expect(props.disabledReason).toBeUndefined();
    expect(props.shortcut).toEqual({ key: 'Enter', alt: true });
    expect(isEnabled(props)).toBe(true);
    expect(runAction(props)).toBe(true);
    expect(propertiesStore.current?.kind).toBe('properties');
    propertiesStore.close();

    const attrs = getAction('file.attributes')!;
    expect(attrs.run).toBeDefined();
    expect(runAction(attrs)).toBe(true);
    expect(propertiesStore.current?.kind).toBe('attributes');
    propertiesStore.close();

    const occ = getAction('file.occupiedSpace')!;
    expect(occ.shortcut).toEqual({ key: 'l', ctrlOrCmd: true });
    expect(runAction(occ)).toBe(true);
    expect(propertiesStore.current).toEqual({ kind: 'occupied', paths: ['C:\\dir\\a.txt'] });
    propertiesStore.close();

    expect(runAction(getAction('commands.volumeLabel')!)).toBe(true);
    expect(propertiesStore.current?.kind).toBe('volume');
    propertiesStore.close();
    expect(runAction(getAction('commands.systemInfo')!)).toBe(true);
    expect(propertiesStore.current?.kind).toBe('system');
    propertiesStore.close();

    // Alt+Enter routes through the registry shortcut machinery…
    expect(
      actionForEvent({
        key: 'Enter',
        ctrlKey: false,
        metaKey: false,
        shiftKey: false,
        altKey: true,
      })?.id,
    ).toBe('file.properties');
    // …and Properties stays unavailable without exactly one selection.
    register([]);
    expect(isEnabled(getAction('file.properties')!)).toBe(false);
    expect(runAction(getAction('file.properties')!)).toBe(false);
    expect(propertiesStore.current).toBeNull();
  });
});

describe('F2-23 branch view registry integration', () => {
  it('toggles the branch view on the active pane with Ctrl+B', async () => {
    register([], 'C:\\pane-root');
    const action = getAction('commands.branch')!;
    expect(action.disabledReason).toBeUndefined();
    expect(action.shortcut).toEqual({ key: 'b', ctrlOrCmd: true });
    expect(isEnabled(action)).toBe(true);
    expect(runAction(action)).toBe(true);
    await vi.waitFor(() => expect(branch.activeFor(0)).toBe(true));
    // Toggling again exits back to the directory view.
    expect(runAction(action)).toBe(true);
    expect(branch.activeFor(0)).toBe(false);
    expect(actionForEvent({ key: 'b', ctrlKey: true, metaKey: false, shiftKey: false })?.id).toBe(
      'commands.branch',
    );
  });
});
