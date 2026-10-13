import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
  FolderSizes,
  FOLDER_SIZE_BACKOFF_MS,
  FOLDER_SIZE_TTL_MS,
  parentFor,
} from './folderSizesStore.svelte';
import { ui } from '../uiState.svelte';
import type { FolderSizesResult } from '../ipc/indexer';

function answer(
  sizes: (number | null)[],
  over: Partial<FolderSizesResult> = {},
): FolderSizesResult {
  return { available: true, reason: null, sizes, stale: false, ...over };
}

describe('folder sizes store (FER-477a)', () => {
  let now = 1_000_000;
  beforeEach(() => {
    ui.indexerFolderSizes = true;
    now = 1_000_000;
  });
  afterEach(() => {
    ui.indexerFolderSizes = false;
  });

  it('derives the parent of a folder row', () => {
    expect(parentFor('C:\\Data\\Projects', 'Projects')).toBe('C:\\Data');
    expect(parentFor('C:\\Projects', 'Projects')).toBe('C:\\');
    expect(parentFor('D:/a/b', 'b')).toBe('D:\\a');
    expect(parentFor('C:\\x', 'y')).toBeNull();
    expect(parentFor('C:\\x', '')).toBeNull();
  });

  it('asks once per parent for the folders on screen and shows formatted sizes', async () => {
    const fetcher = vi.fn(async (_p: string, names: string[]) =>
      answer(names.map((n) => (n === 'big' ? 3 * 1024 * 1024 : null))),
    );
    const s = new FolderSizes(fetcher, () => now);
    s.want([
      { path: 'C:\\Data\\big', name: 'big' },
      { path: 'C:\\Data\\gone', name: 'gone' },
      { path: '\\\\srv\\share\\x', name: 'x' }, // UNC: never asked
    ]);
    await s.flush();
    expect(fetcher).toHaveBeenCalledTimes(1);
    expect(fetcher).toHaveBeenCalledWith('C:\\Data', ['big', 'gone']);
    expect(s.text('c:\\data\\BIG')).toBe('3.00 MB');
    expect(s.text('C:\\Data\\gone')).toBeNull();
    // Within the TTL nothing is asked again.
    s.want([{ path: 'C:\\Data\\big', name: 'big' }]);
    await s.flush();
    expect(fetcher).toHaveBeenCalledTimes(1);
    now += FOLDER_SIZE_TTL_MS + 1;
    s.want([{ path: 'C:\\Data\\big', name: 'big' }]);
    await s.flush();
    expect(fetcher).toHaveBeenCalledTimes(2);
  });

  it('a shared-mode refusal shows "—" and pauses asking for a while', async () => {
    const fetcher = vi.fn(async (_p: string, names: string[]) =>
      answer(
        names.map(() => null),
        { available: false, reason: 'shared_mode' },
      ),
    );
    const s = new FolderSizes(fetcher, () => now);
    s.want([{ path: 'C:\\A\\x', name: 'x' }]);
    await s.flush();
    expect(s.text('C:\\A\\x')).toBeNull();
    expect(s.reason).toBe('shared_mode');
    s.want([{ path: 'C:\\B\\y', name: 'y' }]);
    await s.flush();
    expect(fetcher).toHaveBeenCalledTimes(1);
    now += FOLDER_SIZE_BACKOFF_MS + 1;
    s.want([{ path: 'C:\\B\\y', name: 'y' }]);
    await s.flush();
    expect(fetcher).toHaveBeenCalledTimes(2);
  });

  it('does nothing while the setting is off, and clear() drops late answers', async () => {
    let release: (r: FolderSizesResult) => void = () => {};
    const fetcher = vi.fn(
      () =>
        new Promise<FolderSizesResult>((res) => {
          release = res;
        }),
    );
    const s = new FolderSizes(fetcher, () => now);
    ui.indexerFolderSizes = false;
    s.want([{ path: 'C:\\A\\x', name: 'x' }]);
    await s.flush();
    expect(fetcher).not.toHaveBeenCalled();
    ui.indexerFolderSizes = true;
    s.want([{ path: 'C:\\A\\x', name: 'x' }]);
    const pending = s.flush();
    s.clear();
    release(answer([42]));
    await pending;
    expect(s.bytes('C:\\A\\x')).toBeNull();
    // Turned off: the text is "—" even for a cached size.
    s.want([{ path: 'C:\\A\\x', name: 'x' }]);
    const p2 = s.flush();
    release(answer([42]));
    await p2;
    expect(s.text('C:\\A\\x')).toBe('42.0 B');
    ui.indexerFolderSizes = false;
    expect(s.text('C:\\A\\x')).toBeNull();
  });
});
