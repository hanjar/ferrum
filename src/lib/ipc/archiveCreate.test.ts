// FER-411: the IPC bridge for archive_create forwards the destination conflict policy.
import { beforeEach, describe, expect, it, vi } from 'vitest';

const invokeMock = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invokeMock(...a) }));
vi.mock('./tauri', () => ({ isInTauri: () => true }));

import { archiveCreate } from './archive';
import { FsError } from './types';

const SRC = 'C:\\a';
const OUT = 'C:\\o.zip';

beforeEach(() => {
  invokeMock.mockReset();
});

describe('archiveCreate conflict policy', () => {
  it('defaults to overwrite so existing callers (the manual Pack dialog) keep their behavior', async () => {
    invokeMock.mockResolvedValue(OUT);
    await archiveCreate([SRC], OUT, 'zip');
    expect(invokeMock).toHaveBeenCalledWith('archive_create', {
      request: { format: 'zip', sourcePaths: [SRC], destinationPath: OUT, conflict: 'overwrite' },
    });
  });

  it('forwards skip and rename and resolves with the path the backend actually wrote', async () => {
    invokeMock.mockResolvedValue('C:\\o (2).zip');
    const written = await archiveCreate([SRC], OUT, 'zip', 'rename');
    expect(written).toBe('C:\\o (2).zip');
    expect(invokeMock.mock.calls[0]![1]).toMatchObject({ request: { conflict: 'rename' } });
    await archiveCreate([SRC], OUT, 'sevenz', 'skip');
    expect(invokeMock.mock.calls[1]![1]).toMatchObject({
      request: { format: 'sevenz', conflict: 'skip' },
    });
  });

  it('maps the backend "already exists" rejection to FsError alreadyExists', async () => {
    invokeMock.mockRejectedValue({
      code: 'alreadyExists',
      message: 'The destination already exists',
      path: OUT,
    });
    const error = await archiveCreate([SRC], OUT, 'zip', 'skip').catch((e: unknown) => e);
    expect(error).toBeInstanceOf(FsError);
    expect((error as FsError).code).toBe('alreadyExists');
  });
});
