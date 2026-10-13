// FER-165 / FER-206: Pack availability, the read-only size estimate for the preview, and the engine failure
// contract (message prefixes shared with `src-tauri/src/archive_create_test.rs`).
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const props = vi.hoisted(() => ({
  occupiedSpace: vi.fn(),
  cancelOccupiedSpace: vi.fn(async () => true),
  volumeInfo: vi.fn(),
}));
vi.mock('./ipc/properties', () => props);
vi.mock('./ipc/archive', () => ({ archiveCreate: vi.fn() }));

const {
  measurePack,
  packFailureCause,
  packServiceAvailable,
  PACK_NO_SPACE,
  PACK_SOURCE_UNREADABLE,
} = await import('./packService');

const report = (over: Record<string, unknown> = {}) => ({
  files: 3,
  dirs: 1,
  symlinks: 0,
  logicalBytes: 4096,
  allocatedBytes: 8192,
  allocatedSupported: true,
  failed: [],
  cancelled: false,
  ...over,
});

function enterTauri(): void {
  (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {};
}

beforeEach(() => {
  props.occupiedSpace.mockReset();
  props.cancelOccupiedSpace.mockClear();
  props.volumeInfo.mockReset();
});
afterEach(() => {
  delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__;
  vi.useRealTimers();
});

describe('packServiceAvailable', () => {
  it('is false outside the desktop app and true inside it', () => {
    expect(packServiceAvailable()).toBe(false);
    enterTauri();
    expect(packServiceAvailable()).toBe(true);
  });
});

describe('measurePack', () => {
  it('outside the desktop app: no estimate, no IPC', async () => {
    expect(await measurePack(['C:\\a'], 'C:\\')).toBeUndefined();
    expect(props.occupiedSpace).not.toHaveBeenCalled();
  });

  it('sums the same paths the archive will read and adds the free space of the destination volume', async () => {
    enterTauri();
    props.occupiedSpace.mockResolvedValue(report());
    props.volumeInfo.mockResolvedValue({
      root: 'D:\\',
      label: '',
      fsName: 'NTFS',
      totalBytes: 10,
      freeBytes: 999,
    });
    const m = await measurePack(['D:\\x\\a.txt', 'D:\\x\\sub'], 'D:\\x');
    expect(props.occupiedSpace).toHaveBeenCalledWith(expect.stringMatching(/^pack-measure-/), [
      'D:\\x\\a.txt',
      'D:\\x\\sub',
    ]);
    expect(props.volumeInfo).toHaveBeenCalledWith('D:\\x');
    expect(m).toEqual({ bytes: 4096, files: 3, complete: true, unreadable: [], freeBytes: 999 });
  });

  it('unreadable parts make the size incomplete and are listed', async () => {
    enterTauri();
    props.occupiedSpace.mockResolvedValue(
      report({
        failed: [{ path: 'D:\\x\\sub\\secret', code: 'permissionDenied', message: 'denied' }],
      }),
    );
    props.volumeInfo.mockRejectedValue(new Error('no volume'));
    const m = await measurePack(['D:\\x\\sub'], 'D:\\x');
    expect(m).toEqual({
      bytes: 4096,
      files: 3,
      complete: false,
      unreadable: ['D:\\x\\sub\\secret'],
    });
  });

  it('stops a slow walk after the time budget and reports a partial (incomplete) size', async () => {
    enterTauri();
    vi.useFakeTimers();
    let finish!: (r: unknown) => void;
    props.occupiedSpace.mockImplementation(() => new Promise((resolve) => (finish = resolve)));
    props.volumeInfo.mockResolvedValue(undefined);
    props.cancelOccupiedSpace.mockImplementation(async () => {
      finish(report({ cancelled: true, logicalBytes: 10 }));
      return true;
    });
    const pending = measurePack(['D:\\huge'], 'D:\\', { timeoutMs: 50 });
    await vi.advanceTimersByTimeAsync(60);
    const m = await pending;
    expect(props.cancelOccupiedSpace).toHaveBeenCalledWith(props.occupiedSpace.mock.calls[0][0]);
    expect(m?.complete).toBe(false);
  });

  it('a failing walk never throws: the preview just has no size', async () => {
    enterTauri();
    props.occupiedSpace.mockRejectedValue(new Error('boom'));
    props.volumeInfo.mockResolvedValue(undefined);
    expect(await measurePack(['D:\\a'], 'D:\\')).toBeUndefined();
  });
});

describe('packFailureCause', () => {
  it('classifies by the engine contract prefixes, then by code', () => {
    expect(
      packFailureCause({ code: 'io', message: `${PACK_SOURCE_UNREADABLE}: a.txt: denied` }),
    ).toBe('sourceUnreadable');
    expect(packFailureCause({ code: 'io', message: `${PACK_NO_SPACE}; nothing was written` })).toBe(
      'noSpace',
    );
    expect(packFailureCause({ code: 'permissionDenied', message: 'x' })).toBe('permissionDenied');
    expect(packFailureCause({ code: 'io', message: 'ZIP write: something' })).toBe('other');
  });

  it('the prefixes match the Rust engine constants', async () => {
    // Read the Rust source so a rename on either side fails here, not silently in the UI.
    const { readFileSync } = await import('node:fs');
    const rust = readFileSync('src-tauri/src/archive_create_test.rs', 'utf8');
    expect(rust).toContain(`pub const SOURCE_UNREADABLE: &str = "${PACK_SOURCE_UNREADABLE}";`);
    expect(rust).toContain(`pub const NO_SPACE: &str = "${PACK_NO_SPACE}";`);
  });
});
