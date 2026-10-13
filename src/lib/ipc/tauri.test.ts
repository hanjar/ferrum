import { afterEach, describe, expect, it, vi } from 'vitest';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const invokeMock = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invokeMock(...a) }));

const { isInTauri, listDir } = await import('./tauri');
const { FsError } = await import('./types');

const here = dirname(fileURLToPath(import.meta.url));
const entryFixture = JSON.parse(
  readFileSync(join(here, '../../../contract-fixtures/file-entry.json'), 'utf-8'),
);

function fakeTauri() {
  (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {};
}

afterEach(() => {
  invokeMock.mockReset();
  delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__;
});

describe('listDir service', () => {
  it('rejects with code unsupported in browser mode', async () => {
    expect(isInTauri()).toBe(false);
    await expect(listDir('C:\\')).rejects.toMatchObject({ code: 'unsupported' });
    expect(invokeMock).not.toHaveBeenCalled();
  });

  it('maps invoke rejections through FsError.fromIPC', async () => {
    fakeTauri();
    invokeMock.mockRejectedValue({ code: 'notFound', message: 'gone', path: 'C:\\x' });
    const err = await listDir('C:\\x').catch((e) => e);
    expect(err).toBeInstanceOf(FsError);
    expect(err.code).toBe('notFound');
    expect(err.path).toBe('C:\\x');
  });

  it('guards non-array results and malformed entries', async () => {
    fakeTauri();
    invokeMock.mockResolvedValue({ nope: true });
    await expect(listDir('C:\\')).rejects.toBeInstanceOf(FsError);
    invokeMock.mockResolvedValue([{ name: 'only-name' }]);
    await expect(listDir('C:\\')).rejects.toMatchObject({ code: 'io' });
  });

  it('returns validated entries for a well-formed listing', async () => {
    fakeTauri();
    invokeMock.mockResolvedValue([entryFixture]);
    const out = await listDir('C:\\Users');
    expect(out).toHaveLength(1);
    expect(out[0].name).toBe('hélix-eñgine');
    expect(out[0].isDir).toBe(true);
  });
});
