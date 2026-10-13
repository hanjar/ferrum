import { afterEach, describe, expect, it, vi } from 'vitest';

const invokeMock = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invokeMock(...a) }));

const { listDrives, driveLettersMask } = await import('./drives');
const { isInTauri } = await import('./tauri');
const { FsError } = await import('./types');

function fakeTauri() {
  (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {};
}

const drive = {
  label: 'System',
  path: 'C:\\',
  freeBytes: 82 * 1024 ** 3,
  totalBytes: 512 * 1024 ** 3,
};

afterEach(() => {
  invokeMock.mockReset();
  delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__;
});

describe('listDrives service', () => {
  it('rejects with code unsupported in browser mode', async () => {
    expect(isInTauri()).toBe(false);
    await expect(listDrives()).rejects.toMatchObject({ code: 'unsupported' });
    expect(invokeMock).not.toHaveBeenCalled();
  });

  it('maps invoke rejections through FsError.fromIPC', async () => {
    fakeTauri();
    invokeMock.mockRejectedValue({ code: 'permissionDenied', message: 'no', path: 'C:\\' });
    const err = await listDrives().catch((e) => e);
    expect(err).toBeInstanceOf(FsError);
    expect(err.code).toBe('permissionDenied');
  });

  it('guards non-array results and malformed drives', async () => {
    fakeTauri();
    invokeMock.mockResolvedValue({ nope: true });
    await expect(listDrives()).rejects.toMatchObject({ code: 'io' });
    invokeMock.mockResolvedValue([{ label: 'X', path: 'X:\\', freeBytes: 'many' }]);
    await expect(listDrives()).rejects.toMatchObject({ code: 'io' });
  });

  it('returns validated drives for a well-formed payload', async () => {
    fakeTauri();
    invokeMock.mockResolvedValue([drive, { label: '', path: 'Q:\\', freeBytes: 0, totalBytes: 0 }]);
    const out = await listDrives();
    expect(out).toHaveLength(2);
    expect(out[0].path).toBe('C:\\');
    expect(out[0].totalBytes).toBe(512 * 1024 ** 3);
    expect(out[1].label).toBe('');
  });

  it('defaults new fields for legacy payloads and keeps provided ones', async () => {
    fakeTauri();
    invokeMock.mockResolvedValue([
      drive,
      {
        ...drive,
        path: 'E:\\',
        driveType: 'removable',
        fsName: 'exFAT',
        serial: 'AAAA-BBBB',
        status: 'noMedia',
      },
    ]);
    const out = await listDrives();
    expect(out[0]).toMatchObject({ driveType: 'unknown', status: 'ok', fsName: '', serial: '' });
    expect(out[1]).toMatchObject({ driveType: 'removable', status: 'noMedia', fsName: 'exFAT' });
  });
});

describe('driveLettersMask', () => {
  it('is 0 outside Tauri without invoking', async () => {
    expect(await driveLettersMask()).toBe(0);
    expect(invokeMock).not.toHaveBeenCalled();
  });

  it('returns the mask, 0 on failure', async () => {
    fakeTauri();
    invokeMock.mockResolvedValueOnce(0b1100);
    expect(await driveLettersMask()).toBe(12);
    invokeMock.mockRejectedValueOnce(new Error('x'));
    expect(await driveLettersMask()).toBe(0);
  });
});
