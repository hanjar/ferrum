import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { PaneApi } from './panesRegistry.svelte';

const verifyMock = vi.fn();
const cancelMock = vi.fn();
const memberMock = vi.fn();
let progressHandler: ((p: unknown) => void) | null = null;
const unlistenMock = vi.fn();
vi.mock('../ipc/fileTools', async (importOriginal) => {
  const original = await importOriginal<typeof import('../ipc/fileTools')>();
  return {
    ...original,
    verifyChecksums: (...a: unknown[]) => verifyMock(...a),
    cancelChecksumRequest: (...a: unknown[]) => cancelMock(...a),
    checksumArchiveMember: (...a: unknown[]) => memberMock(...a),
    onChecksumProgress: async (h: (p: unknown) => void) => {
      progressHandler = h;
      return unlistenMock;
    },
  };
});

const { fileTools, openArchiveChecksum, openChecksumVerify, RAR_CHECKSUM_UNSUPPORTED } =
  await import('./fileToolsStore.svelte');
const { registerPane } = await import('./panesRegistry.svelte');
const { FsError } = await import('../ipc/types');

function register(selection: string[], dirs: string[] = []): void {
  const pane: PaneApi = {
    path: 'C:\\d',
    selectedPaths: () => selection,
    visibleEntries: () =>
      selection.map((path) => ({
        name: path,
        path,
        isDir: dirs.includes(path),
        size: 0,
        modifiedMs: 0,
      })),
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

const member = 'C:\\d\\pack.zip\\docs\\a.txt';

beforeEach(() => {
  verifyMock.mockReset();
  cancelMock.mockReset();
  memberMock.mockReset();
  unlistenMock.mockReset();
  progressHandler = null;
});

afterEach(() => {
  fileTools.close();
});

describe('archive member checksum (FER-133)', () => {
  it('opens only for one non-directory member inside an archive and normalises the path', () => {
    register([member]);
    openArchiveChecksum();
    expect(fileTools.current).toEqual({
      kind: 'archiveChecksum',
      archivePath: 'C:\\d\\pack.zip',
      memberPath: 'docs/a.txt',
    });
    fileTools.close();
    const none: [string[], string[]][] = [
      [['C:\\d\\a.txt'], []],
      [['C:\\d\\pack.zip'], []],
      [[member, member + '2'], []],
      [[], []],
      [[member], [member]],
    ];
    for (const [sel, dirs] of none) {
      register(sel, dirs);
      openArchiveChecksum();
      expect(fileTools.current, JSON.stringify(sel)).toBeNull();
    }
  });

  it('hashes with the chosen algorithm, tracks progress by request id and unlistens', async () => {
    register([member]);
    openArchiveChecksum();
    fileTools.algorithm = 'sha1';
    fileTools.password = 'pw';
    let finish: (v: unknown) => void = () => {};
    memberMock.mockImplementation(() => new Promise((resolve) => (finish = resolve)));
    const run = fileTools.runArchiveHash();
    await vi.waitFor(() => expect(memberMock).toHaveBeenCalled());
    const [archive, path, algorithm, options] = memberMock.mock.calls[0];
    expect([archive, path, algorithm, options.password]).toEqual([
      'C:\\d\\pack.zip',
      'docs/a.txt',
      'sha1',
      'pw',
    ]);
    expect(fileTools.hashing).toBe(true);
    // A foreign request id is ignored; ours updates the progress.
    progressHandler?.({
      requestId: 'other',
      processedBytes: 1,
      filesDone: 0,
      filesTotal: 1,
      current: '',
    });
    expect(fileTools.progress).toBeNull();
    progressHandler?.({
      requestId: options.requestId,
      processedBytes: 2048,
      filesDone: 0,
      filesTotal: 1,
      current: 'docs/a.txt',
    });
    expect(fileTools.progress?.processedBytes).toBe(2048);
    finish({ archivePath: archive, memberPath: path, algorithm: 'sha1', hash: 'abc' });
    await run;
    expect(fileTools.memberHash?.hash).toBe('abc');
    expect(fileTools.hashing).toBe(false);
    expect(fileTools.progress).toBeNull();
    expect(unlistenMock).toHaveBeenCalled();
  });

  it('cancels through the request id and reports a cancel, not an error', async () => {
    register([member]);
    openArchiveChecksum();
    memberMock.mockImplementation(
      () =>
        new Promise((_, reject) =>
          setTimeout(() => reject(new FsError({ code: 'cancelled', message: 'cancelled' })), 20),
        ),
    );
    cancelMock.mockResolvedValue(true);
    const run = fileTools.runArchiveHash();
    await vi.waitFor(() => expect(memberMock).toHaveBeenCalled());
    await fileTools.cancelVerify();
    expect(cancelMock).toHaveBeenCalledWith(memberMock.mock.calls[0][3].requestId);
    await run;
    expect(fileTools.memberCancelled).toBe(true);
    expect(fileTools.error).toBe('');
  });

  it('refuses RAR with a clear message and never calls the backend', async () => {
    register(['C:\\d\\pack.rar\\a.txt']);
    openArchiveChecksum();
    await fileTools.runArchiveHash();
    expect(memberMock).not.toHaveBeenCalled();
    expect(fileTools.error).toBe(RAR_CHECKSUM_UNSUPPORTED);
    expect(fileTools.memberHash).toBeNull();
  });

  it('surfaces backend errors and clears the password on close', async () => {
    register([member]);
    openArchiveChecksum();
    fileTools.password = 'x';
    memberMock.mockRejectedValue(new FsError({ code: 'io', message: 'password required' }));
    await fileTools.runArchiveHash();
    expect(fileTools.error).toBe('password required');
    fileTools.close();
    expect(fileTools.password).toBe('');
  });

  it('shows live progress during a verify run too', async () => {
    register(['C:\\dir\\a.txt']);
    let finish: (v: unknown) => void = () => {};
    verifyMock.mockImplementation(() => new Promise((resolve) => (finish = resolve)));
    openChecksumVerify();
    await vi.waitFor(() => expect(verifyMock).toHaveBeenCalled());
    const id = verifyMock.mock.calls[0][1];
    progressHandler?.({
      requestId: id,
      processedBytes: 10,
      filesDone: 0,
      filesTotal: 1,
      current: 'a.txt',
    });
    expect(fileTools.progress?.current).toBe('a.txt');
    finish({ requested: 1, ok: 1, mismatched: 0, outcomes: [] });
    await vi.waitFor(() => expect(fileTools.verifying).toBe(false));
    expect(fileTools.progress).toBeNull();
  });
});

describe('file.archiveChecksum action', () => {
  it('is only enabled for a single archive member', async () => {
    const { getAction, isEnabled } = await import('../actions');
    const action = getAction('file.archiveChecksum')!;
    expect(action.run).toBeDefined();
    register(['C:\\d\\a.txt']);
    expect(isEnabled(action)).toBe(false);
    register(['C:\\d\\pack.7z\\x.bin']);
    expect(isEnabled(action)).toBe(true);
  });
});
