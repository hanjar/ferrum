import { beforeEach, describe, expect, it, vi } from 'vitest';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const invokeMock = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invokeMock(...a) }));
// eslint-disable-next-line @typescript-eslint/no-unused-vars -- pass-through mock, args are forwarded verbatim
const listenMock = vi.fn(async (..._a: unknown[]) => () => {});
vi.mock('@tauri-apps/api/event', () => ({
  listen: (...a: unknown[]) => listenMock(...a),
}));

const { searchStart, searchCancel, onSearchEvent } = await import('./search');
const { watchStart, watchStop, onFsChanged } = await import('./watch');
const { loadPrefs, savePrefs } = await import('./prefs');
const { readPreview, readArchiveMemberPreview, cancelArchiveRequest } = await import('./preview');
const { FsError } = await import('./types');

const here = dirname(fileURLToPath(import.meta.url));
const entryFixture = JSON.parse(
  readFileSync(join(here, '../../../contract-fixtures/file-entry.json'), 'utf-8'),
);

function fakeTauri() {
  (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {};
}
function unTauri() {
  delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__;
}

beforeEach(() => {
  invokeMock.mockReset().mockName('invoke');
  listenMock.mockClear();
  unTauri();
});

describe('search backend wrapper', () => {
  it('rejects unsupported outside Tauri', async () => {
    await expect(
      searchStart('id-1', 'C:\\t', {
        pattern: 'he',
        mode: 'glob',
        caseSensitive: false,
        recursive: false,
      }),
    ).rejects.toMatchObject({ code: 'unsupported' });
  });

  it('invokes search_start with id/root/options and maps errors', async () => {
    fakeTauri();
    const options = {
      pattern: 'he',
      mode: 'glob' as const,
      caseSensitive: false,
      recursive: false,
      maxResults: 5,
    };
    await expect(searchStart('id-1', 'C:\\t', options)).resolves.toBeUndefined();
    expect(invokeMock).toHaveBeenCalledWith('search_start', { id: 'id-1', root: 'C:\\t', options });
    invokeMock.mockRejectedValueOnce({ code: 'notFound', message: 'gone' });
    await expect(searchStart('id-2', 'C:\\x', options)).rejects.toBeInstanceOf(FsError);
  });

  it('cancel is false outside Tauri and invokes inside', async () => {
    await expect(searchCancel('id-1')).resolves.toBe(false);
    fakeTauri();
    invokeMock.mockResolvedValueOnce(true);
    await expect(searchCancel('id-1')).resolves.toBe(true);
    expect(invokeMock).toHaveBeenCalledWith('search_cancel', { id: 'id-1' });
    invokeMock.mockRejectedValueOnce('raw');
    await expect(searchCancel('id-1')).resolves.toBe(false);
  });

  it('onSearchEvent subscribes to search://progress', async () => {
    const cb = vi.fn();
    await onSearchEvent(cb);
    expect(listenMock).toHaveBeenCalledWith('search://progress', expect.any(Function));
  });

  it('entry fixture still passes the shared shape', () => {
    expect(entryFixture.isDir).toBe(true); // ties this suite to the contract fixture
  });
});

describe('watch backend wrapper', () => {
  it('rejects start outside Tauri; stop is silent', async () => {
    await expect(watchStart('C:\\t')).rejects.toMatchObject({ code: 'unsupported' });
    await expect(watchStop('C:\\t')).resolves.toBeUndefined();
  });

  it('invokes watch_start/stop inside Tauri and maps errors', async () => {
    fakeTauri();
    invokeMock.mockResolvedValue(undefined);
    await watchStart('C:\\t');
    await watchStop('C:\\t');
    expect(invokeMock).toHaveBeenNthCalledWith(1, 'watch_start', { path: 'C:\\t' });
    expect(invokeMock).toHaveBeenNthCalledWith(2, 'watch_stop', { path: 'C:\\t' });
    invokeMock.mockRejectedValueOnce({ code: 'permissionDenied', message: 'no' });
    await expect(watchStart('C:\\locked')).rejects.toMatchObject({ code: 'permissionDenied' });
  });

  it('onFsChanged subscribes to fs://changed', async () => {
    await onFsChanged(vi.fn());
    expect(listenMock).toHaveBeenCalledWith('fs://changed', expect.any(Function));
  });
});

describe('prefs backend wrapper', () => {
  it('load rejects unsupported; save is silent outside Tauri', async () => {
    await expect(loadPrefs()).rejects.toMatchObject({ code: 'unsupported' });
    await expect(
      savePrefs({
        theme: 'dark',
        palette: 'violet',
        showHidden: false,
        terminal: 'auto',
        paneSplit: 0.5,
        tabs: {
          openNextToCurrent: true,
          restoreTabs: true,
          tabWidth: 'fit',
          closeOnMiddleClick: true,
        },
        preview: {
          openOnLaunch: false,
          maxTextBytes: 2_000_000,
          syntaxHighlight: true,
          autoplayVideo: false,
          readMetadata: true,
        },
        density: 'compact',
        iconStyle: 'colored',
        iconTheme: 'Tile',
        uiFontLevel: 'm',
        showExtensions: true,
        foldersFirst: true,
      }),
    ).resolves.toBeUndefined();
  });

  it('loads and saves through invoke inside Tauri', async () => {
    fakeTauri();
    const prefs = {
      theme: 'light' as const,
      palette: 'aqua' as const,
      showHidden: true,
      terminal: 'wt' as const,
      paneSplit: 0.6,
      tabs: {
        openNextToCurrent: false,
        restoreTabs: true,
        tabWidth: 'fixed' as const,
        closeOnMiddleClick: false,
      },
      session: { panes: [{ tabs: ['C:\\a'], current: 0 }] },
      preview: {
        openOnLaunch: true,
        maxTextBytes: 500_000,
        syntaxHighlight: false,
        autoplayVideo: true,
        readMetadata: false,
      },
      density: 'comfortable' as const,
      iconStyle: 'monochrome' as const,
      iconTheme: 'Tile' as const,
      uiFontLevel: 'l' as const,
      showExtensions: false,
      foldersFirst: false,
    };
    const normalized = {
      ...prefs,
      confirmBeforeDeleting: true,
      verifyAfterCopy: false,
      copyBufferKiB: 1024,
      copyThreads: 0,
      copySafeWrite: false,
      copyPriority: 'normal',
      conflictPolicy: 'ask',
      preserveTimestamps: true,
      preservePermissions: false,
    };
    invokeMock.mockResolvedValueOnce(prefs);
    await expect(loadPrefs()).resolves.toEqual(normalized);
    await savePrefs(prefs);
    expect(invokeMock).toHaveBeenLastCalledWith('save_prefs', { prefs: normalized });
  });
});

describe('preview backend wrapper', () => {
  it('rejects unsupported outside Tauri', async () => {
    await expect(readPreview('C:\\x.txt', 1024)).rejects.toMatchObject({ code: 'unsupported' });
  });

  it('invokes read_preview and validates the PreviewData payload', async () => {
    fakeTauri();
    const data = {
      kind: 'text',
      data: 'aGVsbG8=',
      truncated: false,
      tooLarge: false,
      size: 5,
      modifiedMs: 1234,
    };
    invokeMock.mockResolvedValueOnce(data);
    await expect(readPreview('C:\\x.txt', 1024)).resolves.toEqual(data);
    expect(invokeMock).toHaveBeenCalledWith('read_preview', { path: 'C:\\x.txt', maxBytes: 1024 });
    // Malformed payloads never cross the boundary.
    invokeMock.mockResolvedValueOnce({ kind: 'text' });
    await expect(readPreview('C:\\x.txt', 1024)).rejects.toBeInstanceOf(FsError);
    invokeMock.mockRejectedValueOnce('raw');
    await expect(readPreview('C:\\x.txt', 1024)).rejects.toBeInstanceOf(FsError);
  });
});

describe('archive member preview wrapper (FER-104)', () => {
  it('invokes read_archive_member_preview with camelCase args and validates the payload', async () => {
    fakeTauri();
    const data = {
      kind: 'text',
      data: 'aGk=',
      truncated: false,
      tooLarge: false,
      size: 2,
      modifiedMs: 1,
    };
    invokeMock.mockResolvedValueOnce(data);
    await expect(readArchiveMemberPreview('C:\\a.zip', 'd/x.txt', 1024)).resolves.toEqual(data);
    expect(invokeMock).toHaveBeenCalledWith('read_archive_member_preview', {
      archivePath: 'C:\\a.zip',
      memberPath: 'd/x.txt',
      maxBytes: 1024,
      requestId: undefined,
    });
    invokeMock.mockResolvedValueOnce({ kind: 'text' });
    await expect(readArchiveMemberPreview('C:\\a.zip', 'x', 1)).rejects.toBeInstanceOf(FsError);
  });

  it('passes the request id and cancels by id with archive_cancel_request (FER-438)', async () => {
    fakeTauri();
    invokeMock.mockResolvedValueOnce({
      kind: 'text',
      data: '',
      truncated: false,
      tooLarge: false,
      size: 0,
      modifiedMs: 0,
    });
    await readArchiveMemberPreview('C:\\a.zip', 'x', 1, 'pv-7');
    expect(invokeMock).toHaveBeenCalledWith(
      'read_archive_member_preview',
      expect.objectContaining({ requestId: 'pv-7' }),
    );
    invokeMock.mockResolvedValueOnce(undefined);
    cancelArchiveRequest('pv-7');
    expect(invokeMock).toHaveBeenLastCalledWith('archive_cancel_request', { requestId: 'pv-7' });
    // A rejected cancel must never surface.
    invokeMock.mockRejectedValueOnce(new Error('gone'));
    expect(() => cancelArchiveRequest('pv-8')).not.toThrow();
  });
});
