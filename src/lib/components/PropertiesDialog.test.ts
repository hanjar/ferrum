import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import PropertiesDialog from './PropertiesDialog.svelte';

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

const { propertiesStore, openSystemInfo } = await import('../stores/propertiesStore.svelte');
const { FsError } = await import('../ipc/types');

afterEach(() => {
  cleanup();
  propertiesStore.close();
  entryPropertiesMock.mockReset();
  occupiedSpaceMock.mockReset();
  volumeInfoMock.mockReset();
  setAttributesMock.mockReset();
  cancelOccupiedMock.mockReset();
  hostInfoMock.mockReset();
  hostInfoMock.mockRejectedValue(new Error('no host'));
  progressHandler = null;
});

const fileEntry = {
  path: 'C:\\d\\notes.txt',
  name: 'notes.txt',
  kind: 'file',
  size: 2048,
  allocatedBytes: 4096,
  allocatedSupported: true,
  createdMs: new Date(2026, 0, 2, 3, 4, 5).getTime(),
  modifiedMs: new Date(2026, 8, 28, 14, 0, 0).getTime(),
  accessedMs: 0,
  readOnly: true,
  hidden: false,
  system: false,
  attributesEditable: true,
};

describe('PropertiesDialog', () => {
  it('renders nothing while closed and shows the property sheet when open', async () => {
    const { container } = render(PropertiesDialog);
    expect(container.querySelector('[role="dialog"]')).toBeNull();
    entryPropertiesMock.mockResolvedValue(fileEntry);
    propertiesStore.open({ kind: 'properties', path: fileEntry.path });
    await vi.waitFor(() => expect(screen.getByRole('dialog', { name: 'Properties' })).toBeTruthy());
    expect(screen.getByText('C:\\d\\notes.txt')).toBeTruthy();
    expect(screen.getByText(/^2.00 KB /)).toBeTruthy(); // logical size
    expect(screen.getByText(/^4.00 KB /)).toBeTruthy(); // on-disk size
    // Attributes are shown read-only here; editing lives in Change Attributes.
    expect(screen.getByText('Read-only')).toBeTruthy();
    expect(screen.queryByRole('checkbox')).toBeNull();
  });

  it('flags a logical-only allocated size honestly when the OS cannot report it', async () => {
    entryPropertiesMock.mockResolvedValue({
      ...fileEntry,
      allocatedSupported: false,
      allocatedBytes: fileEntry.size,
    });
    render(PropertiesDialog);
    propertiesStore.open({ kind: 'properties', path: fileEntry.path });
    await vi.waitFor(() =>
      expect(screen.getByText(/allocated size not reported by this OS/)).toBeTruthy(),
    );
  });

  it('shows the read-only volume sheet with a rename-not-supported note', async () => {
    volumeInfoMock.mockResolvedValue({
      root: 'C:\\',
      label: 'Windows',
      fsName: 'NTFS',
      totalBytes: 512 * 1024 ** 3,
      freeBytes: 82 * 1024 ** 3,
    });
    propertiesStore.open({ kind: 'volume', path: 'C:\\' });
    render(PropertiesDialog);
    await vi.waitFor(() => expect(screen.getByText('NTFS')).toBeTruthy());
    expect(screen.getByText('Windows')).toBeTruthy();
    expect(screen.getByText('Renaming the volume label is not supported yet.')).toBeTruthy();
  });

  it('renders read-only system information without any IPC', async () => {
    openSystemInfo();
    render(PropertiesDialog);
    await vi.waitFor(() =>
      expect(screen.getByRole('dialog', { name: 'System Information' })).toBeTruthy(),
    );
    expect(screen.getByText('Platform')).toBeTruthy();
    expect(entryPropertiesMock).not.toHaveBeenCalled();
  });

  it('shows the occupied-space calculation state and per-path failures', async () => {
    occupiedSpaceMock.mockResolvedValue({
      files: 2,
      dirs: 1,
      symlinks: 1,
      logicalBytes: 3000,
      allocatedBytes: 8192,
      allocatedSupported: true,
      failed: [{ path: 'C:\\d\\locked', code: 'permissionDenied', message: 'denied' }],
    });
    propertiesStore.open({ kind: 'occupied', paths: ['C:\\d'] });
    render(PropertiesDialog);
    expect(screen.getByRole('dialog', { name: 'Calculate Occupied Space' })).toBeTruthy();
    await vi.waitFor(() => expect(screen.getByText(/^2.93 KB /)).toBeTruthy());
    expect(screen.getByText(/could not be read/)).toBeTruthy();
    expect(screen.getByText(/C:\\d\\locked/)).toBeTruthy();
  });

  it('shows the system flag next to read-only and hidden', async () => {
    entryPropertiesMock.mockResolvedValue({
      ...fileEntry,
      readOnly: true,
      hidden: true,
      system: true,
    });
    render(PropertiesDialog);
    propertiesStore.open({ kind: 'properties', path: fileEntry.path });
    await vi.waitFor(() => expect(screen.getByText('Read-only, Hidden, System')).toBeTruthy());
  });

  it('shows the symlink target without following it', async () => {
    entryPropertiesMock.mockResolvedValue({
      ...fileEntry,
      kind: 'symlink',
      symlinkTarget: 'D:\\real\\file.txt',
    });
    render(PropertiesDialog);
    propertiesStore.open({ kind: 'properties', path: fileEntry.path });
    await vi.waitFor(() => expect(screen.getByText(/Symlink → D:\\real\\file.txt/)).toBeTruthy());
  });

  it('formats POSIX permissions when reported and says so honestly when not', async () => {
    entryPropertiesMock.mockResolvedValue({ ...fileEntry, mode: 0o640 });
    render(PropertiesDialog);
    propertiesStore.open({ kind: 'properties', path: fileEntry.path });
    await vi.waitFor(() => expect(screen.getByText('rw-r----- (0640)')).toBeTruthy());
    entryPropertiesMock.mockResolvedValue(fileEntry);
    propertiesStore.open({ kind: 'properties', path: fileEntry.path });
    await vi.waitFor(() => expect(screen.getByText('Not reported on this platform')).toBeTruthy());
  });

  it('Change Attributes previews the change before anything is written', async () => {
    entryPropertiesMock.mockImplementation(async (path: string) => ({ ...fileEntry, path }));
    setAttributesMock.mockResolvedValue(undefined);
    render(PropertiesDialog);
    propertiesStore.open({ kind: 'attributes', paths: ['C:\\a.txt', 'C:\\b.txt'] });
    await vi.waitFor(() => expect(screen.getByText(/2 items selected/)).toBeTruthy());
    const hidden = screen.getByRole('checkbox', { name: 'Hidden' }) as HTMLInputElement;
    await fireEvent.click(hidden);
    await fireEvent.click(screen.getByRole('button', { name: 'Review changes' }));
    expect(screen.getByText('Review what will change:')).toBeTruthy();
    expect(screen.getAllByText('Hidden: no → yes')).toHaveLength(2);
    expect(setAttributesMock).not.toHaveBeenCalled();
    await fireEvent.click(screen.getByRole('button', { name: 'Apply' }));
    await vi.waitFor(() => expect(setAttributesMock).toHaveBeenCalledTimes(2));
  });

  it('Change Attributes reports a failing item by path in the dialog', async () => {
    entryPropertiesMock.mockImplementation(async (path: string) => ({ ...fileEntry, path }));
    setAttributesMock.mockImplementation(async (path: string) => {
      if (path.endsWith('a.txt'))
        throw new FsError({ code: 'permissionDenied', message: 'locked' });
    });
    render(PropertiesDialog);
    propertiesStore.open({ kind: 'attributes', paths: ['C:\\a.txt', 'C:\\b.txt'] });
    await vi.waitFor(() => expect(screen.getByText(/2 items selected/)).toBeTruthy());
    await fireEvent.click(screen.getByRole('checkbox', { name: 'Hidden' }));
    await fireEvent.click(screen.getByRole('button', { name: 'Review changes' }));
    await fireEvent.click(screen.getByRole('button', { name: 'Apply' }));
    await vi.waitFor(() => expect(screen.getByText('C:\\a.txt: locked')).toBeTruthy());
    expect(setAttributesMock).toHaveBeenCalledTimes(2);
  });

  it('disables the attribute toggles and explains why when the platform is Unsupported', async () => {
    entryPropertiesMock.mockResolvedValue({ ...fileEntry, attributesEditable: false });
    render(PropertiesDialog);
    propertiesStore.open({ kind: 'attributes', paths: [fileEntry.path] });
    await vi.waitFor(() => expect(screen.getByText('Not supported on this platform')).toBeTruthy());
    expect(screen.getByText(/only on Windows/)).toBeTruthy();
    expect((screen.getByRole('checkbox', { name: 'Read-only' }) as HTMLInputElement).disabled).toBe(
      true,
    );
    expect((screen.getByRole('checkbox', { name: 'Hidden' }) as HTMLInputElement).disabled).toBe(
      true,
    );
    expect(
      (screen.getByRole('button', { name: 'Review changes' }) as HTMLButtonElement).disabled,
    ).toBe(true);
  });

  it('explains an Unsupported volume path instead of showing a raw error', async () => {
    volumeInfoMock.mockRejectedValue(
      new FsError({
        code: 'unsupported',
        message: 'Volume information is not supported for this path on this platform',
      }),
    );
    render(PropertiesDialog);
    propertiesStore.open({ kind: 'volume', path: '\\\\srv\\share' });
    await vi.waitFor(() => expect(screen.getByText('Not supported on this platform')).toBeTruthy());
    expect(screen.getByText(/Volume information is not supported/)).toBeTruthy();
    expect(screen.queryByRole('alert')).toBeNull();
  });

  it('still shows genuine errors as alerts', async () => {
    volumeInfoMock.mockRejectedValue(new FsError({ code: 'notFound', message: 'gone' }));
    render(PropertiesDialog);
    propertiesStore.open({ kind: 'volume', path: 'C:\\' });
    await vi.waitFor(() => expect(screen.getByRole('alert').textContent).toBe('gone'));
  });

  it('occupied space shows running progress and a Cancel button that stops the walk', async () => {
    let finish: (r: unknown) => void = () => {};
    occupiedSpaceMock.mockImplementation(() => new Promise((res) => (finish = res)));
    render(PropertiesDialog);
    propertiesStore.open({ kind: 'occupied', paths: ['C:\\d'] });
    await vi.waitFor(() => expect(screen.getByText(/Calculating 1 item/)).toBeTruthy());
    const scanId = occupiedSpaceMock.mock.calls[0][0] as string;
    progressHandler?.({ scanId, files: 1200, dirs: 40, symlinks: 0, logicalBytes: 2048 });
    await vi.waitFor(() => expect(screen.getByText(/1,200 files, 40 folders/)).toBeTruthy());
    expect(screen.queryByRole('button', { name: 'Close' })).toBeNull();
    await fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(cancelOccupiedMock).toHaveBeenCalledWith(scanId);
    await vi.waitFor(() => expect(screen.getByText(/Cancelling 1 item/)).toBeTruthy());
    expect((screen.getByRole('button', { name: 'Cancel' }) as HTMLButtonElement).disabled).toBe(
      true,
    );
    finish({
      files: 1300,
      dirs: 45,
      symlinks: 0,
      logicalBytes: 3000,
      allocatedBytes: 4096,
      allocatedSupported: true,
      failed: [],
      cancelled: true,
    });
    await vi.waitFor(() => expect(screen.getByText('Cancelled')).toBeTruthy());
    expect(screen.getByText(/totals below are partial/)).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Close' })).toBeTruthy();
  });

  it('takes keyboard focus on open so Escape reaches the dialog (not the row behind it)', async () => {
    const row = document.createElement('button');
    document.body.append(row);
    row.focus();
    render(PropertiesDialog);
    openSystemInfo();
    await screen.findByRole('dialog');
    expect(document.activeElement).toBe(screen.getByRole('dialog'));
    await fireEvent.keyDown(document.activeElement as Element, { key: 'Escape' });
    expect(propertiesStore.current).toBeNull();
    row.remove();
  });

  it('Escape while the walk runs cancels it and closes the dialog', async () => {
    occupiedSpaceMock.mockImplementation(() => new Promise(() => {}));
    render(PropertiesDialog);
    propertiesStore.open({ kind: 'occupied', paths: ['C:\\d'] });
    await vi.waitFor(() => expect(occupiedSpaceMock).toHaveBeenCalled());
    const scanId = occupiedSpaceMock.mock.calls[0][0] as string;
    await fireEvent.keyDown(screen.getByRole('dialog'), { key: 'Escape' });
    expect(cancelOccupiedMock).toHaveBeenCalledWith(scanId);
    expect(screen.queryByRole('dialog')).toBeNull();
  });

  it('adds host OS details to System Information when Rust provides them', async () => {
    hostInfoMock.mockResolvedValue({
      os: 'windows',
      arch: 'x86_64',
      family: 'windows',
      hostName: 'DESK-01',
    });
    openSystemInfo();
    render(PropertiesDialog);
    await vi.waitFor(() => expect(screen.getByText('DESK-01')).toBeTruthy());
    expect(screen.getByText('windows (windows)')).toBeTruthy();
    expect(screen.getByText(/operating-system version is not reported/)).toBeTruthy();
  });
});
