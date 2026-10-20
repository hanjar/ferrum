import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { PaneApi } from './panesRegistry.svelte';

const planCopyMock = vi.fn();
vi.mock('../ipc/mutations', () => ({
  planCopy: (...args: unknown[]) => planCopyMock(...args),
}));

const { dialogs, summarizeArchiveTest, sanitizeMemberPath } = await import('./dialogStore.svelte');
const { registerPane } = await import('./panesRegistry.svelte');
const { ui } = await import('../uiState.svelte');
const { FsError } = await import('../ipc/types');

const unregisters: (() => void)[] = [];

function register(
  id: 0 | 1,
  path: string,
  selected: string[] = [],
  onRefresh: () => void = () => {},
): void {
  const api: PaneApi = {
    path,
    selectedPaths: () => selected,
    cursorEntry: () => undefined,
    refresh: onRefresh,
    newTab: () => {},
    closeCurrentTab: () => {},
    goUp: () => {},
    openCursorEntry: () => {},
  };
  unregisters.push(registerPane(id, api));
}

afterEach(() => {
  dialogs.close();
  ui.activePane = 0;
  for (const off of unregisters.splice(0)) off();
});

beforeEach(() => {
  planCopyMock.mockReset();
});

describe('FER-572 S1: archive detection only for real archive segments', () => {
  const sources = ['C:\\Downloads\\foo.zip_extracted\\a.txt'];
  const plan = { totalFiles: 1, totalBytes: 1, conflicts: [] };

  it('a folder merely named like foo.zip_extracted is a normal source and target', async () => {
    planCopyMock.mockResolvedValue(plan);
    await dialogs.beginTransferTo('move', sources, 'D:\\backup.zip.old');
    expect(planCopyMock).toHaveBeenCalledWith(sources, 'D:\\backup.zip.old');
    expect(dialogs.current?.kind).toBe('move');
  });

  it('a clipboard paste (fromDisk) never routes disk sources to the archive flow', async () => {
    planCopyMock.mockResolvedValue(plan);
    await dialogs.beginTransferTo('copy', ['C:\\x\\real.zip\\odd.txt'], 'D:\\Backup', {
      fromDisk: true,
    });
    expect(planCopyMock).toHaveBeenCalled();
    expect(dialogs.current?.kind).toBe('copy');
  });

  it('a real member path inside an archive is still refused as a move source', async () => {
    await dialogs.beginTransferTo('move', ['C:\\x\\real.zip\\inner.txt'], 'D:\\Backup');
    expect(planCopyMock).not.toHaveBeenCalled();
    expect(dialogs.current?.kind).toBe('error');
  });
});

describe('beginTransferTo (drop flow, explicit target)', () => {
  it('plans the copy for the explicit target without touching the pane registry', async () => {
    planCopyMock.mockResolvedValue({ totalFiles: 2, totalBytes: 4096, conflicts: [] });
    // No panes registered: the explicit target/paths must be enough.
    await dialogs.beginTransferTo('copy', ['C:\\a\\x.txt', 'C:\\a\\y.txt'], 'D:\\Backup');
    expect(planCopyMock).toHaveBeenCalledWith(['C:\\a\\x.txt', 'C:\\a\\y.txt'], 'D:\\Backup');
    expect(dialogs.current).toEqual({
      kind: 'copy',
      paths: ['C:\\a\\x.txt', 'C:\\a\\y.txt'],
      target: 'D:\\Backup',
      totalFiles: 2,
      totalBytes: 4096,
      conflicts: [],
    });
  });

  it('planCopy still runs to surface conflicts; kind follows the request (move)', async () => {
    planCopyMock.mockResolvedValue({
      totalFiles: 1,
      totalBytes: 8,
      conflicts: [{ source: 'C:\\a\\x.txt', dest: 'D:\\Backup\\x.txt' }],
    });
    await dialogs.beginTransferTo('move', ['C:\\a\\x.txt'], 'D:\\Backup');
    expect(planCopyMock).toHaveBeenCalledWith(['C:\\a\\x.txt'], 'D:\\Backup');
    expect(dialogs.current).toMatchObject({
      kind: 'move',
      target: 'D:\\Backup',
      conflicts: [{ source: 'C:\\a\\x.txt', dest: 'D:\\Backup\\x.txt' }],
    });
  });

  it('a failed plan opens the error dialog with the flow title', async () => {
    planCopyMock.mockRejectedValue(new FsError({ code: 'unsupported', message: 'nope' }));
    await dialogs.beginTransferTo('move', ['C:\\a\\x.txt'], 'D:\\Backup');
    expect(dialogs.current).toEqual({ kind: 'error', title: 'Move', message: 'nope' });
  });

  it('empty paths refuse to plan', async () => {
    await dialogs.beginTransferTo('copy', [], 'D:\\Backup');
    expect(dialogs.current).toEqual({ kind: 'error', title: 'Copy', message: 'Nothing selected.' });
    expect(planCopyMock).not.toHaveBeenCalled();
  });
});

describe('selection flows keep their exact current behavior', () => {
  it('beginCopyFlow plans the active selection into the inactive pane', async () => {
    register(0, 'C:\\Projects', ['C:\\Projects\\README.md']);
    register(1, 'D:\\Backup');
    planCopyMock.mockResolvedValue({ totalFiles: 1, totalBytes: 2970, conflicts: [] });
    await dialogs.beginCopyFlow();
    expect(planCopyMock).toHaveBeenCalledWith(['C:\\Projects\\README.md'], 'D:\\Backup');
    expect(dialogs.current).toMatchObject({ kind: 'copy', target: 'D:\\Backup' });
  });

  it('beginMoveFlow without a selection opens the current Nothing-selected error', async () => {
    register(0, 'C:\\Projects');
    register(1, 'D:\\Backup');
    await dialogs.beginMoveFlow();
    expect(dialogs.current).toEqual({ kind: 'error', title: 'Copy', message: 'Nothing selected.' });
    expect(planCopyMock).not.toHaveBeenCalled();
  });
});

describe('FER-364: beginDelete carries the permanent flag', () => {
  it('default (trash) opens the delete dialog with permanent: false', () => {
    register(0, 'C:\\Projects', ['C:\\Projects\\README.md']);
    dialogs.beginDelete();
    expect(dialogs.current).toEqual({
      kind: 'delete',
      paths: ['C:\\Projects\\README.md'],
      permanent: false,
    });
  });

  it('beginDelete(true) opens the delete dialog pre-checked for permanent deletion (D05)', () => {
    register(0, 'C:\\Projects', ['C:\\Projects\\README.md']);
    dialogs.beginDelete(true);
    expect(dialogs.current).toEqual({
      kind: 'delete',
      paths: ['C:\\Projects\\README.md'],
      permanent: true,
    });
  });

  it('an empty selection still opens the Nothing-selected error for both variants', () => {
    register(0, 'C:\\Projects');
    dialogs.beginDelete(true);
    expect(dialogs.current).toEqual({
      kind: 'error',
      title: 'Delete',
      message: 'Nothing selected.',
    });
    dialogs.close();
    dialogs.beginDelete(false);
    expect(dialogs.current).toEqual({
      kind: 'error',
      title: 'Delete',
      message: 'Nothing selected.',
    });
  });
});

const archiveExtractMock = vi.fn();
const archivePreflightMock = vi.fn();
const archiveTestMock = vi.fn();
const archiveCancelMock = vi.fn();
const multipartCheckMock = vi.fn();
let progressHandler: ((e: Record<string, unknown>) => void) | null = null;
let requestSeq = 0;
vi.mock('../ipc/archive', () => ({
  archiveExtract: (...args: unknown[]) => archiveExtractMock(...args),
  archiveExtractPreflight: (...args: unknown[]) => archivePreflightMock(...args),
  archiveTest: (...args: unknown[]) => archiveTestMock(...args),
  archiveCreate: vi.fn(),
  archiveCreateZip: vi.fn(),
  archiveCancelRequest: (...args: unknown[]) => archiveCancelMock(...args),
  archiveMultipartCheck: (...args: unknown[]) => multipartCheckMock(...args),
  newArchiveRequestId: () => `req-${++requestSeq}`,
  listenArchiveProgress: async (handler: (e: Record<string, unknown>) => void) => {
    progressHandler = handler;
    return () => {
      progressHandler = null;
    };
  },
}));

function flush(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

describe('FER-368 unpack flow (Alt+F9): D21 one-ask conflict dialog', () => {
  beforeEach(() => {
    archiveExtractMock.mockReset();
    archivePreflightMock.mockReset();
    archiveTestMock.mockReset();
  });

  it('without an archive in the selection it explains what to select', () => {
    register(0, 'C:\\Projects', ['C:\\Projects\\README.md']);
    register(1, 'D:\\Backup');
    dialogs.beginUnpack();
    expect(dialogs.current).toMatchObject({
      kind: 'error',
      title: 'Unpack',
      message: expect.stringContaining('.zip, .7z or .rar'),
    });
    expect(archivePreflightMock).not.toHaveBeenCalled();
    expect(archiveExtractMock).not.toHaveBeenCalled();
  });

  it('no conflicts → extracts the FULL archive (all members) with skip, no dialog', async () => {
    register(0, 'C:\\Projects', ['C:\\Projects\\sample.zip']);
    register(1, 'D:\\Backup');
    archivePreflightMock.mockResolvedValue({ totalFiles: 2, totalBytes: 100, conflicts: [] });
    archiveExtractMock.mockResolvedValue(undefined);
    dialogs.beginUnpack();
    await flush();
    // FER-504: the trailing undefined is the password — none exists in this flow.
    expect(archivePreflightMock).toHaveBeenCalledWith(
      'C:\\Projects\\sample.zip',
      [],
      'D:\\Backup',
      undefined,
    );
    expect(archiveExtractMock).toHaveBeenCalledWith(
      'C:\\Projects\\sample.zip',
      [],
      'D:\\Backup',
      'skip',
      undefined,
      expect.any(String),
    );
    expect(dialogs.current).toBeNull();
  });

  it('conflicts → ONE apply-to-all dialog; the decision extracts with that policy', async () => {
    register(0, 'C:\\Projects', ['C:\\Projects\\sample.zip']);
    register(1, 'D:\\Backup');
    archivePreflightMock.mockResolvedValue({
      totalFiles: 2,
      totalBytes: 100,
      conflicts: [{ memberPath: 'a.txt', destinationPath: 'D:\\Backup\\a.txt' }],
    });
    archiveExtractMock.mockResolvedValue(undefined);
    dialogs.beginUnpack();
    await flush();
    expect(dialogs.current).toMatchObject({
      kind: 'extractConflicts',
      target: 'D:\\Backup',
      conflicts: [{ memberPath: 'a.txt', destinationPath: 'D:\\Backup\\a.txt' }],
    });
    expect(archiveExtractMock).not.toHaveBeenCalled();
    dialogs.confirmExtractConflicts('overwrite');
    await flush();
    expect(archiveExtractMock).toHaveBeenCalledWith(
      'C:\\Projects\\sample.zip',
      [],
      'D:\\Backup',
      'overwrite',
      undefined,
      expect.any(String),
    );
    expect(dialogs.current).toBeNull();
  });

  it('cancel/Escape (plain close) aborts cleanly: no extract ever runs', async () => {
    register(0, 'C:\\Projects', ['C:\\Projects\\sample.zip']);
    register(1, 'D:\\Backup');
    archivePreflightMock.mockResolvedValue({
      totalFiles: 1,
      totalBytes: 10,
      conflicts: [{ memberPath: 'a.txt', destinationPath: 'D:\\Backup\\a.txt' }],
    });
    dialogs.beginUnpack();
    await flush();
    dialogs.close();
    await flush();
    expect(archiveExtractMock).not.toHaveBeenCalled();
    expect(dialogs.current).toBeNull();
  });

  it('password flow: preflight unlocks first, the conflict ask follows, extract runs WITH the password', async () => {
    register(0, 'C:\\Projects', ['C:\\Projects\\enc.zip']);
    register(1, 'D:\\Backup');
    archivePreflightMock
      .mockRejectedValueOnce(
        new FsError({
          code: 'io',
          message: 'Encrypted member requires a password',
          archiveCode: 'passwordRequired',
        }),
      )
      .mockResolvedValueOnce({ totalFiles: 1, totalBytes: 10, conflicts: [] });
    archiveExtractMock.mockResolvedValue(undefined);
    dialogs.beginUnpack();
    await flush();
    expect(dialogs.current).toMatchObject({
      kind: 'password',
      archivePath: 'C:\\Projects\\enc.zip',
    });
    const retry = (dialogs.current as { retry: (pw: string) => Promise<void> }).retry;
    await retry('pw1');
    await flush();
    expect(archivePreflightMock).toHaveBeenCalledTimes(2);
    // FER-504: the retry preflights WITH the typed password (header-encrypted
    // archives now enumerate too), then the extract runs with it.
    expect(archivePreflightMock).toHaveBeenNthCalledWith(
      2,
      'C:\\Projects\\enc.zip',
      [],
      'D:\\Backup',
      'pw1',
    );
    expect(archiveExtractMock).toHaveBeenCalledTimes(1);
    expect(archiveExtractMock).toHaveBeenCalledWith(
      'C:\\Projects\\enc.zip',
      [],
      'D:\\Backup',
      'skip',
      'pw1',
      expect.any(String),
    );
    expect(dialogs.current).toBeNull();
  });

  it('header-encrypted corner: preflight still locked after unlock → direct extract fallback keeps the flow working', async () => {
    register(0, 'C:\\Projects', ['C:\\Projects\\enc.zip']);
    register(1, 'D:\\Backup');
    archivePreflightMock.mockRejectedValue(
      new FsError({
        code: 'io',
        message: 'Encrypted member requires a password',
        archiveCode: 'passwordRequired',
      }),
    );
    archiveExtractMock.mockResolvedValue(undefined);
    dialogs.beginUnpack();
    await flush();
    const retry = (dialogs.current as { retry: (pw: string) => Promise<void> }).retry;
    await retry('pw1');
    await flush();
    // FER-504 note: since preflight carries the password this only happens
    // when the unlock STILL failed (e.g. the typed password was wrong) — the
    // legacy direct extract keeps the batch moving; its own password-retry
    // applies from there.
    expect(archiveExtractMock).toHaveBeenCalledWith(
      'C:\\Projects\\enc.zip',
      [],
      'D:\\Backup',
      'skip',
      'pw1',
      expect.any(String),
    );
    expect(dialogs.current).toBeNull();
  });

  it('non-password preflight failure surfaces an Unpack error and stops the queue', async () => {
    register(0, 'C:\\Projects', ['C:\\Projects\\bad.zip', 'C:\\Projects\\next.zip']);
    register(1, 'D:\\Backup');
    archivePreflightMock.mockRejectedValue(new FsError({ code: 'io', message: 'disk full' }));
    dialogs.beginUnpack();
    await flush();
    expect(archivePreflightMock).toHaveBeenCalledTimes(1);
    expect(archiveExtractMock).not.toHaveBeenCalled();
    expect(dialogs.current).toMatchObject({ kind: 'error', title: 'Unpack', message: 'disk full' });
  });

  it('multi-archive batch: conflicts from BOTH archives land in ONE ask; one decision drives both extracts', async () => {
    register(0, 'C:\\Projects', ['C:\\Projects\\one.zip', 'C:\\Projects\\two.zip']);
    register(1, 'D:\\Backup');
    archivePreflightMock
      .mockResolvedValueOnce({
        totalFiles: 1,
        totalBytes: 1,
        conflicts: [{ memberPath: 'x.txt', destinationPath: 'D:\\Backup\\x.txt' }],
      })
      .mockResolvedValueOnce({
        totalFiles: 1,
        totalBytes: 1,
        conflicts: [{ memberPath: 'y.txt', destinationPath: 'D:\\Backup\\y.txt' }],
      });
    archiveExtractMock.mockResolvedValue(undefined);
    dialogs.beginUnpack();
    await flush();
    expect(dialogs.current).toMatchObject({
      kind: 'extractConflicts',
      target: 'D:\\Backup',
      conflicts: [
        { memberPath: 'x.txt', destinationPath: 'D:\\Backup\\x.txt' },
        { memberPath: 'y.txt', destinationPath: 'D:\\Backup\\y.txt' },
      ],
    });
    dialogs.confirmExtractConflicts('rename');
    await flush();
    expect(archiveExtractMock).toHaveBeenCalledTimes(2);
    expect(archiveExtractMock.mock.calls[0]).toEqual([
      'C:\\Projects\\one.zip',
      [],
      'D:\\Backup',
      'rename',
      undefined,
      expect.stringMatching(/^req-/),
    ]);
    expect(archiveExtractMock.mock.calls[1]).toEqual([
      'C:\\Projects\\two.zip',
      [],
      'D:\\Backup',
      'rename',
      undefined,
      expect.stringMatching(/^req-/),
    ]);
  });

  it('copy FROM an archive (F5) runs the same one-ask flow with slash member paths', async () => {
    register(0, 'C:\\arc', ['C:\\arc\\sample.zip\\docs\\readme.md']);
    register(1, 'D:\\Backup');
    archivePreflightMock.mockResolvedValue({
      totalFiles: 1,
      totalBytes: 100,
      conflicts: [{ memberPath: 'docs/readme.md', destinationPath: 'D:\\Backup\\docs\\readme.md' }],
    });
    archiveExtractMock.mockResolvedValue(undefined);
    await dialogs.beginCopyFlow();
    await flush();
    expect(planCopyMock).not.toHaveBeenCalled(); // archive sources skip the disk planner
    // FER-504: the trailing undefined is the password — none exists in this flow.
    expect(archivePreflightMock).toHaveBeenCalledWith(
      'C:\\arc\\sample.zip',
      ['docs/readme.md'],
      'D:\\Backup',
      undefined,
    );
    dialogs.confirmExtractConflicts('skip');
    await flush();
    expect(archiveExtractMock).toHaveBeenCalledWith(
      'C:\\arc\\sample.zip',
      ['docs/readme.md'],
      'D:\\Backup',
      'skip',
      undefined,
      expect.any(String),
    );
  });
});

describe('F2-12 test-archive flow (Alt+Shift+F9)', () => {
  beforeEach(() => {
    archiveExtractMock.mockReset();
    archiveTestMock.mockReset();
  });

  it('without an archive in the selection it explains what to select', () => {
    register(0, 'C:\\Projects');
    dialogs.beginTestArchive();
    expect(dialogs.current).toMatchObject({
      kind: 'error',
      title: 'Test archive',
    });
    expect(archiveTestMock).not.toHaveBeenCalled();
  });

  it('all members pass → passed dialog, no problem count', async () => {
    register(0, 'C:\\Projects', ['C:\\Projects\\sample.zip']);
    archiveTestMock.mockResolvedValue({
      archivePath: 'C:\\Projects\\sample.zip',
      entries: [{ memberPath: 'a.txt', status: 'passed' }],
    });
    dialogs.beginTestArchive();
    await flush();
    expect(dialogs.current).toMatchObject({ kind: 'testReport', title: 'Archive test passed' });
    const reports = (
      dialogs.current as { reports: { archivePath: string; status: string; durationMs?: number }[] }
    ).reports;
    expect(reports[0].archivePath).toContain('sample.zip');
    expect(reports[0].status).toBe('passed');
    expect(typeof reports[0].durationMs).toBe('number');
  });

  it('failed members are listed with their member path and error message', async () => {
    register(0, 'C:\\Projects', ['C:\\Projects\\broken.zip']);
    archiveTestMock.mockResolvedValue({
      archivePath: 'C:\\Projects\\broken.zip',
      entries: [
        { memberPath: 'good.txt', status: 'passed' },
        {
          memberPath: 'damaged.txt',
          status: 'failed',
          error: { code: 'corrupt', message: 'CRC mismatch' },
        },
      ],
    });
    dialogs.beginTestArchive();
    await flush();
    expect(dialogs.current).toMatchObject({
      kind: 'testReport',
      title: 'Archive test: 1 problem(s)',
    });
    const reports = (
      dialogs.current as {
        reports: { status: string; failures: { memberPath: string; message: string }[] }[];
      }
    ).reports;
    expect(reports[0].status).toBe('failed');
    expect(reports[0].failures).toEqual([
      { memberPath: 'damaged.txt', status: 'failed', message: 'CRC mismatch' },
    ]);
  });

  it('lists every failure, not only the first five', () => {
    const entries = Array.from({ length: 9 }, (_, i) => ({
      memberPath: `f${i}.txt`,
      status: 'failed',
      error: { message: 'bad' },
    }));
    const item = summarizeArchiveTest('x.zip', entries);
    expect(item.failures).toHaveLength(9);
    expect(item.total).toBe(9);
    expect(item.passed).toBe(0);
  });

  it('password, unsupported and corrupt are kept apart; corrupt wins', () => {
    expect(
      summarizeArchiveTest('a.zip', [{ memberPath: 'a', status: 'passwordRequired' }]).status,
    ).toBe('passwordRequired');
    expect(summarizeArchiveTest('b.zip', [{ memberPath: 'b', status: 'unsupported' }]).status).toBe(
      'unsupported',
    );
    expect(
      summarizeArchiveTest('c.zip', [
        { memberPath: 'c', status: 'passwordRequired' },
        { memberPath: 'd', status: 'failed' },
      ]).status,
    ).toBe('failed');
  });
});

// ── FER-79 / FER-73 / FER-75 ──────────────────────────────────────────

type Deferred<T> = { promise: Promise<T>; resolve: (v: T) => void; reject: (e: unknown) => void };
function deferred<T>(): Deferred<T> {
  let resolve!: (v: T) => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

const cancelledError = () =>
  new FsError({ code: 'cancelled', message: 'Extraction cancelled', archiveCode: 'cancelled' });

type PasswordDialog = {
  kind: string;
  message?: string;
  retry: (pw: string) => Promise<void>;
};

describe('FER-79 cancel and progress for extract', () => {
  beforeEach(() => {
    archiveExtractMock.mockReset();
    archivePreflightMock.mockReset();
    archiveCancelMock.mockReset();
    archivePreflightMock.mockResolvedValue({ totalFiles: 1, totalBytes: 1, conflicts: [] });
  });

  it('extract gets a request id and shows a progress dialog that Esc/close cancels with that id', async () => {
    register(0, 'C:\\Projects', ['C:\\Projects\\big.zip']);
    register(1, 'D:\\Backup');
    const job = deferred<void>();
    archiveExtractMock.mockReturnValue(job.promise);
    dialogs.beginUnpack();
    await flush();
    const id = archiveExtractMock.mock.calls[0]![5] as string;
    expect(id).toMatch(/^req-/);
    expect(dialogs.current).toMatchObject({
      kind: 'archiveProgress',
      requestId: id,
      archivePath: 'C:\\Projects\\big.zip',
      cancelling: false,
    });

    dialogs.close(); // Esc
    expect(archiveCancelMock).toHaveBeenCalledWith(id);
    // The dialog stays up, marked cancelling, until the job really stops.
    expect(dialogs.current).toMatchObject({ kind: 'archiveProgress', cancelling: true });

    job.reject(cancelledError());
    await flush();
    expect(dialogs.current).toMatchObject({ kind: 'error', title: 'Unpack' });
    expect((dialogs.current as { message: string }).message).toContain('Dibatalkan');
    // Only one cancel request even if cancel is asked for again.
    dialogs.cancelArchiveJob();
    expect(archiveCancelMock).toHaveBeenCalledTimes(1);
  });

  it('Batal (cancelArchiveJob) sends the same request id; a cancelled batch does not start the next archive', async () => {
    register(0, 'C:\\Projects', ['C:\\Projects\\one.zip', 'C:\\Projects\\two.zip']);
    register(1, 'D:\\Backup');
    const job = deferred<void>();
    archiveExtractMock.mockReturnValue(job.promise);
    dialogs.beginUnpack();
    await flush();
    const id = archiveExtractMock.mock.calls[0]![5] as string;
    expect(dialogs.current).toMatchObject({ kind: 'archiveProgress', position: '1/2' });
    dialogs.cancelArchiveJob();
    expect(archiveCancelMock).toHaveBeenCalledWith(id);
    job.reject(cancelledError());
    await flush();
    expect(archiveExtractMock).toHaveBeenCalledTimes(1);
  });

  it('progress events update the dialog, and only for its own request id', async () => {
    register(0, 'C:\\Projects', ['C:\\Projects\\big.zip']);
    register(1, 'D:\\Backup');
    const job = deferred<void>();
    archiveExtractMock.mockReturnValue(job.promise);
    dialogs.beginUnpack();
    await flush();
    const id = archiveExtractMock.mock.calls[0]![5] as string;
    progressHandler?.({
      requestId: 'someone-else',
      processedBytes: 999,
      processedEntries: 9,
    });
    expect(dialogs.current).toMatchObject({ processedBytes: 0 });
    progressHandler?.({
      requestId: id,
      processedBytes: 2048,
      totalBytes: 4096,
      processedEntries: 1,
      totalEntries: 2,
      currentMember: 'docs/readme.md',
    });
    expect(dialogs.current).toMatchObject({
      processedBytes: 2048,
      totalBytes: 4096,
      processedEntries: 1,
      totalEntries: 2,
      currentMember: 'docs/readme.md',
    });
    job.resolve();
    await flush();
    expect(dialogs.current).toBeNull();
    expect(progressHandler).toBeNull(); // listener released
  });

  it('F5 from inside an archive is cancellable too (same queue)', async () => {
    register(0, 'C:\\arc', ['C:\\arc\\sample.zip\\docs\\readme.md']);
    register(1, 'D:\\Backup');
    const job = deferred<void>();
    archiveExtractMock.mockReturnValue(job.promise);
    await dialogs.beginCopyFlow();
    await flush();
    const id = archiveExtractMock.mock.calls[0]![5] as string;
    expect(dialogs.current).toMatchObject({ kind: 'archiveProgress', requestId: id });
    dialogs.close();
    expect(archiveCancelMock).toHaveBeenCalledWith(id);
    job.reject(cancelledError());
    await flush();
  });
});

describe('FER-73 password in Test Archive and unpack', () => {
  beforeEach(() => {
    archiveExtractMock.mockReset();
    archivePreflightMock.mockReset();
    archiveTestMock.mockReset();
    archiveCancelMock.mockReset();
  });

  it('Test Archive on an encrypted archive asks for a password, retries with it, and keeps it out of the report', async () => {
    register(0, 'C:\\Projects', ['C:\\Projects\\enc.zip']);
    archiveTestMock
      .mockResolvedValueOnce({
        archivePath: 'C:\\Projects\\enc.zip',
        entries: [
          {
            memberPath: 'a.txt',
            status: 'failed',
            error: { code: 'passwordRequired', message: 'encrypted member requires a password' },
          },
        ],
      })
      .mockResolvedValueOnce({
        archivePath: 'C:\\Projects\\enc.zip',
        entries: [{ memberPath: 'a.txt', status: 'passed' }],
      });
    dialogs.beginTestArchive();
    await flush();
    expect(dialogs.current).toMatchObject({
      kind: 'password',
      archivePath: 'C:\\Projects\\enc.zip',
    });
    expect((dialogs.current as PasswordDialog).message).toBeUndefined();
    await (dialogs.current as PasswordDialog).retry('s3cret');
    await flush();
    expect(archiveTestMock).toHaveBeenCalledTimes(2);
    expect(archiveTestMock.mock.calls[0]![1]).toBeUndefined();
    expect(archiveTestMock.mock.calls[1]![1]).toBe('s3cret');
    expect(archiveTestMock.mock.calls[1]![2]).toMatch(/^req-/);
    expect(dialogs.current).toMatchObject({ kind: 'testReport', title: 'Archive test passed' });
    expect(JSON.stringify(dialogs.current)).not.toContain('s3cret');
  });

  it('a wrong password is told apart from a missing one: the prompt says so and asks again', async () => {
    register(0, 'C:\\Projects', ['C:\\Projects\\enc.7z']);
    // 7z with an encrypted header fails the whole call with a structured code.
    archiveTestMock
      .mockRejectedValueOnce(
        new FsError({
          code: 'io',
          message: '7z: PasswordRequired',
          archiveCode: 'passwordRequired',
        }),
      )
      .mockRejectedValueOnce(
        new FsError({ code: 'io', message: '7z: MaybeBadPassword', archiveCode: 'badPassword' }),
      )
      .mockResolvedValueOnce({
        archivePath: 'C:\\Projects\\enc.7z',
        entries: [{ memberPath: 'a.txt', status: 'passed' }],
      });
    dialogs.beginTestArchive();
    await flush();
    let d = dialogs.current as PasswordDialog;
    expect(d.kind).toBe('password');
    expect(d.message).toBeUndefined();
    await d.retry('wrong');
    await flush();
    d = dialogs.current as PasswordDialog;
    expect(d.kind).toBe('password');
    expect(d.message).toMatch(/salah atau data terenkripsi rusak/i);
    await d.retry('right');
    await flush();
    expect(archiveTestMock.mock.calls.map((c) => c[1])).toEqual([undefined, 'wrong', 'right']);
    expect(dialogs.current).toMatchObject({ kind: 'testReport', title: 'Archive test passed' });
  });

  it('dismissing the prompt leaves the archive as "password required" without asking again', async () => {
    register(0, 'C:\\Projects', ['C:\\Projects\\enc.zip']);
    archiveTestMock.mockRejectedValue(
      new FsError({ code: 'io', message: 'needs one', archiveCode: 'passwordRequired' }),
    );
    dialogs.beginTestArchive();
    await flush();
    expect(dialogs.current).toMatchObject({ kind: 'password' });
    dialogs.close(); // Esc
    await flush();
    expect(archiveTestMock).toHaveBeenCalledTimes(1);
    const report = dialogs.current as { kind: string; reports: { status: string }[] };
    expect(report.kind).toBe('testReport');
    expect(report.reports[0]).toMatchObject({ status: 'passwordRequired' });
  });

  it('a damaged archive is not a password problem even when its message mentions the word', async () => {
    register(0, 'C:\\Projects', ['C:\\Projects\\bad.zip']);
    archiveTestMock.mockResolvedValue({
      archivePath: 'C:\\Projects\\bad.zip',
      entries: [
        {
          memberPath: 'a.txt',
          status: 'failed',
          error: { code: 'corruptArchive', message: 'password field is garbage' },
        },
      ],
    });
    dialogs.beginTestArchive();
    await flush();
    expect(dialogs.current).toMatchObject({ kind: 'testReport' });
    expect((dialogs.current as { reports: { status: string }[] }).reports[0]!.status).toBe(
      'failed',
    );
  });

  it('summarize: wrong and missing password are password outcomes, corruption still wins', () => {
    const wrong = summarizeArchiveTest('a.zip', [
      { memberPath: 'a', status: 'failed', error: { code: 'badPassword', message: 'x' } },
    ]);
    expect(wrong).toMatchObject({ status: 'passwordRequired', wrongPassword: true });
    const missing = summarizeArchiveTest('a.zip', [
      { memberPath: 'a', status: 'failed', error: { code: 'passwordRequired', message: 'x' } },
    ]);
    expect(missing.status).toBe('passwordRequired');
    expect(missing.wrongPassword).toBeUndefined();
    const mixed = summarizeArchiveTest('a.zip', [
      { memberPath: 'a', status: 'failed', error: { code: 'badPassword', message: 'x' } },
      { memberPath: 'b', status: 'failed', error: { code: 'corruptArchive', message: 'y' } },
    ]);
    expect(mixed.status).toBe('failed');
  });

  it('extract no longer treats the word "password" in a message as a password prompt', async () => {
    register(0, 'C:\\Projects', ['C:\\Projects\\x.zip']);
    register(1, 'D:\\Backup');
    archivePreflightMock.mockRejectedValue(
      new FsError({ code: 'io', message: 'cannot read password.txt' }),
    );
    dialogs.beginUnpack();
    await flush();
    expect(dialogs.current).toMatchObject({ kind: 'error', title: 'Unpack' });
  });

  it('extract retries on a wrong password with a visible message', async () => {
    register(0, 'C:\\Projects', ['C:\\Projects\\enc.zip']);
    register(1, 'D:\\Backup');
    archivePreflightMock.mockResolvedValue({ totalFiles: 1, totalBytes: 1, conflicts: [] });
    archiveExtractMock
      .mockRejectedValueOnce(
        new FsError({ code: 'io', message: 'Wrong archive password', archiveCode: 'badPassword' }),
      )
      .mockResolvedValueOnce(undefined);
    dialogs.beginUnpack();
    await flush();
    const d = dialogs.current as PasswordDialog;
    expect(d.kind).toBe('password');
    expect(d.message).toMatch(/salah/i);
    // ZIP reports a wrong password exactly, so no "or damaged data" hedge.
    expect(d.message).not.toMatch(/rusak/i);
    await d.retry('fixed');
    await flush();
    expect(archiveExtractMock.mock.calls[1]![4]).toBe('fixed');
    expect(dialogs.current).toBeNull();
  });
});

describe('FER-79 cancel in Test Archive', () => {
  beforeEach(() => {
    archiveTestMock.mockReset();
    archiveCancelMock.mockReset();
  });

  it('Batal calls cancel with the job id; the report marks the interrupted and unstarted archives as cancelled', async () => {
    register(0, 'C:\\Projects', [
      'C:\\Projects\\a.zip',
      'C:\\Projects\\b.7z',
      'C:\\Projects\\c.rar',
    ]);
    const job = deferred<unknown>();
    archiveTestMock.mockReturnValue(job.promise);
    dialogs.beginTestArchive();
    await flush();
    const id = archiveTestMock.mock.calls[0]![2] as string;
    expect(dialogs.current).toMatchObject({
      kind: 'archiveProgress',
      requestId: id,
      title: 'Test archive',
      position: '1/3',
    });
    dialogs.cancelArchiveJob();
    expect(archiveCancelMock).toHaveBeenCalledWith(id);
    job.reject(cancelledError());
    await flush();
    expect(archiveTestMock).toHaveBeenCalledTimes(1); // b and c never started
    const d = dialogs.current as {
      kind: string;
      title: string;
      reports: { status: string; archivePath: string }[];
    };
    expect(d.kind).toBe('testReport');
    expect(d.title).toMatch(/dibatalkan/i);
    expect(d.reports.map((r) => r.status)).toEqual(['cancelled', 'cancelled', 'cancelled']);
  });

  it('Esc on the progress dialog cancels the test too', async () => {
    register(0, 'C:\\Projects', ['C:\\Projects\\a.zip']);
    const job = deferred<unknown>();
    archiveTestMock.mockReturnValue(job.promise);
    dialogs.beginTestArchive();
    await flush();
    const id = archiveTestMock.mock.calls[0]![2] as string;
    dialogs.close();
    expect(archiveCancelMock).toHaveBeenCalledWith(id);
    job.reject(cancelledError());
    await flush();
  });
});

describe('FER-75 multipart messages', () => {
  beforeEach(() => {
    archiveExtractMock.mockReset();
    archivePreflightMock.mockReset();
    archiveTestMock.mockReset();
    multipartCheckMock.mockReset();
  });

  const notFirstError = () =>
    new FsError({
      code: 'unsupported',
      message: 'Ini bagian 2 dari arsip multipart. Buka bagian pertama: a.part1.rar',
      archiveCode: 'multipartNotFirst',
    });

  it('unpacking part 2 explains it and offers to open the first part; nothing is extracted', async () => {
    const opened: string[] = [];
    unregisters.push(
      registerPane(0, {
        path: 'C:\\Projects',
        selectedPaths: () => ['C:\\Projects\\a.part2.rar'],
        cursorEntry: () => undefined,
        refresh: () => {},
        newTab: () => {},
        closeCurrentTab: () => {},
        goUp: () => {},
        openCursorEntry: () => {},
        openPath: (p: string) => void opened.push(p),
      } as PaneApi),
    );
    register(1, 'D:\\Backup');
    archivePreflightMock.mockRejectedValue(notFirstError());
    multipartCheckMock.mockResolvedValue({
      kind: 'notFirst',
      part: 2,
      firstName: 'a.part1.rar',
      firstPath: 'C:\\Projects\\a.part1.rar',
      firstExists: true,
    });
    dialogs.beginUnpack();
    await flush();
    expect(archiveExtractMock).not.toHaveBeenCalled();
    expect(dialogs.current).toMatchObject({
      kind: 'multipart',
      archivePath: 'C:\\Projects\\a.part2.rar',
      info: { kind: 'notFirst', part: 2, firstName: 'a.part1.rar' },
    });
    dialogs.openFirstPart();
    expect(opened).toEqual(['C:\\Projects\\a.part1.rar']);
    expect(dialogs.current).toBeNull();
  });

  it('a first part with missing volumes is refused and names the missing ones', async () => {
    register(0, 'C:\\Projects', ['C:\\Projects\\a.part1.rar']);
    register(1, 'D:\\Backup');
    archivePreflightMock.mockRejectedValue(
      new FsError({
        code: 'unsupported',
        message: 'Bagian arsip multipart hilang: a.part2.rar.',
        archiveCode: 'multipartMissing',
      }),
    );
    multipartCheckMock.mockResolvedValue({ kind: 'missing', missing: ['a.part2.rar'] });
    dialogs.beginUnpack();
    await flush();
    expect(archiveExtractMock).not.toHaveBeenCalled();
    expect(dialogs.current).toMatchObject({
      kind: 'multipart',
      info: { kind: 'missing', missing: ['a.part2.rar'] },
    });
    // No "open first part" action for a missing-volumes dialog.
    dialogs.openFirstPart();
    expect(dialogs.current).toMatchObject({ kind: 'multipart' });
  });

  it('Test Archive on a non-first part reports it as unsupported with the explanation', async () => {
    register(0, 'C:\\Projects', ['C:\\Projects\\a.part2.rar']);
    archiveTestMock.mockRejectedValue(notFirstError());
    dialogs.beginTestArchive();
    await flush();
    const d = dialogs.current as { reports: { status: string; note?: string }[] };
    expect(d.reports[0]!.status).toBe('unsupported');
    expect(d.reports[0]!.note).toContain('bagian 2');
  });

  it('falls back to the plain message when the folder check fails', async () => {
    register(0, 'C:\\Projects', ['C:\\Projects\\a.part2.rar']);
    register(1, 'D:\\Backup');
    archivePreflightMock.mockRejectedValue(notFirstError());
    multipartCheckMock.mockRejectedValue(new Error('boom'));
    dialogs.beginUnpack();
    await flush();
    expect(dialogs.current).toMatchObject({ kind: 'error', title: 'Unpack' });
  });
});

describe('FER-55: error per item and destination refresh', () => {
  beforeEach(() => {
    archiveExtractMock.mockReset();
    archivePreflightMock.mockReset();
    archivePreflightMock.mockResolvedValue({ totalFiles: 1, totalBytes: 1, conflicts: [] });
  });

  it('error dialog specifies the failed member path from FsError', async () => {
    register(0, 'C:\\Projects', ['C:\\Projects\\pkg.zip']);
    register(1, 'D:\\Backup');
    const err = new FsError({
      code: 'io',
      message: 'Write failed: disk full',
      memberPath: 'data/assets/icon.png',
    });
    archiveExtractMock.mockRejectedValue(err);

    dialogs.beginUnpack();
    await flush();

    expect(dialogs.current).toMatchObject({ kind: 'error', title: 'Unpack' });
    const msg = (dialogs.current as { message: string }).message;
    expect(msg).toContain('Gagal di: data/assets/icon.png');
    expect(msg).toContain('Write failed: disk full');
  });

  it('destination pane is refreshed on extract completion', async () => {
    const dstRefresh = vi.fn();
    register(0, 'C:\\Projects', ['C:\\Projects\\pkg.zip']);
    register(1, 'D:\\Backup', [], dstRefresh);
    archiveExtractMock.mockResolvedValue(undefined);

    dialogs.beginUnpack();
    await flush();

    expect(dstRefresh).toHaveBeenCalled();
  });

  it('destination pane is refreshed on extract failure (partial files visible)', async () => {
    const dstRefresh = vi.fn();
    register(0, 'C:\\Projects', ['C:\\Projects\\pkg.zip']);
    register(1, 'D:\\Backup', [], dstRefresh);
    const err = new FsError({
      code: 'io',
      message: 'Corrupt archive member',
      memberPath: 'corrupt.bin',
    });
    archiveExtractMock.mockRejectedValue(err);

    dialogs.beginUnpack();
    await flush();

    expect(dstRefresh).toHaveBeenCalled();
    expect(dialogs.current).toMatchObject({ kind: 'error' });
  });

  it('destination pane is refreshed on extract cancellation', async () => {
    const dstRefresh = vi.fn();
    register(0, 'C:\\Projects', ['C:\\Projects\\pkg.zip']);
    register(1, 'D:\\Backup', [], dstRefresh);
    const job = deferred<void>();
    archiveExtractMock.mockReturnValue(job.promise);

    dialogs.beginUnpack();
    await flush();

    dialogs.cancelArchiveJob();
    job.reject(cancelledError());
    await flush();

    expect(dstRefresh).toHaveBeenCalled();
  });

  it('FER-508: preflight conflict error specifies the failed member path from FsError', async () => {
    register(0, 'C:\\Projects', ['C:\\Projects\\pkg.zip']);
    register(1, 'D:\\Backup');
    const err = new FsError({
      code: 'alreadyExists',
      archiveCode: 'conflict',
      message: 'Duplicate archive member',
      memberPath: 'data/assets/icon.png',
    });
    archivePreflightMock.mockRejectedValue(err);

    dialogs.beginUnpack();
    await flush();

    expect(dialogs.current).toMatchObject({ kind: 'error', title: 'Unpack' });
    const msg = (dialogs.current as { message: string }).message;
    expect(msg).toContain('Gagal di: data/assets/icon.png');
    expect(msg).toContain('Duplicate archive member');
  });

  it('FER-508 (P3-2): sanitizes control characters and bidi overrides in member_path error display', async () => {
    register(0, 'C:\\Projects', ['C:\\Projects\\pkg.zip']);
    register(1, 'D:\\Backup');
    const evilMember = 'data/evil\u202Ename\n.png';
    const err = new FsError({
      code: 'alreadyExists',
      archiveCode: 'conflict',
      message: 'Duplicate archive member',
      memberPath: evilMember,
    });
    archivePreflightMock.mockRejectedValue(err);

    dialogs.beginUnpack();
    await flush();

    expect(dialogs.current).toMatchObject({ kind: 'error', title: 'Unpack' });
    const msg = (dialogs.current as { message: string }).message;
    expect(msg).toContain('Gagal di: data/evil\\u{202E}name\\n.png');
    expect(msg).not.toContain('\u202E');
    const lines = msg.split('\n');
    expect(lines[0]).toBe('Gagal di: data/evil\\u{202E}name\\n.png');
    expect(lines[1]).toBe('Duplicate archive member');
  });

  it('FER-508 (P3-2): sanitizeMemberPath helper unit tests', () => {
    expect(sanitizeMemberPath('docs/readme.txt')).toBe('docs/readme.txt');
    expect(sanitizeMemberPath('a\u202Eb\nc\rd\te\0f')).toBe('a\\u{202E}b\\nc\\rd\\te\\0f');
    expect(sanitizeMemberPath('file\u061C\u200E\u200F\u2066\u2069.ext')).toBe(
      'file\\u{061C}\\u{200E}\\u{200F}\\u{2066}\\u{2069}.ext',
    );
    expect(sanitizeMemberPath('line\u2028sep\u2029para')).toBe('line\\u{2028}sep\\u{2029}para');
  });
});

describe('FER-508: dialog queue for concurrent prompts', () => {
  beforeEach(() => {
    dialogs.current = null;
  });

  it('two concurrent password prompts both complete sequentially without hanging', async () => {
    const p1 = dialogs.askArchivePassword('C:\\arc1.rar', false);
    const p2 = dialogs.askArchivePassword('C:\\arc2.7z', false);

    expect(dialogs.current).toMatchObject({
      kind: 'password',
      archivePath: 'C:\\arc1.rar',
    });

    // Submit first password prompt
    const firstPrompt = dialogs.current as { retry: (pw: string) => Promise<void> };
    await firstPrompt.retry('pass1');

    // First prompt resolved, second prompt is now active
    expect(dialogs.current).toMatchObject({
      kind: 'password',
      archivePath: 'C:\\arc2.7z',
    });

    // Submit second password prompt
    const secondPrompt = dialogs.current as { retry: (pw: string) => Promise<void> };
    await secondPrompt.retry('pass2');

    const [res1, res2] = await Promise.all([p1, p2]);
    expect(res1).toBe('pass1');
    expect(res2).toBe('pass2');
    expect(dialogs.current).toBeNull();
  });

  it('first prompt cancelled still allows second queued prompt to be answered', async () => {
    const p1 = dialogs.askArchivePassword('C:\\arc1.rar', false);
    const p2 = dialogs.askArchivePassword('C:\\arc2.7z', false);

    expect(dialogs.current).toMatchObject({
      kind: 'password',
      archivePath: 'C:\\arc1.rar',
    });

    // User cancels the first prompt (Esc / Cancel)
    dialogs.close();

    // First prompt resolves to null, second prompt appears
    expect(await p1).toBeNull();
    expect(dialogs.current).toMatchObject({
      kind: 'password',
      archivePath: 'C:\\arc2.7z',
    });

    // User completes the second prompt
    const secondPrompt = dialogs.current as { retry: (pw: string) => Promise<void> };
    await secondPrompt.retry('pass2');

    expect(await p2).toBe('pass2');
    expect(dialogs.current).toBeNull();
  });

  it('both concurrent prompts can be cancelled without hanging', async () => {
    const p1 = dialogs.askArchivePassword('C:\\arc1.rar', false);
    const p2 = dialogs.askArchivePassword('C:\\arc2.7z', false);

    dialogs.close();
    expect(await p1).toBeNull();

    expect(dialogs.current).toMatchObject({
      kind: 'password',
      archivePath: 'C:\\arc2.7z',
    });

    dialogs.close();
    expect(await p2).toBeNull();
    expect(dialogs.current).toBeNull();
  });

  it('FER-508 (P3-1): queued prompt is unshifted (not cancelled) when non-prompt progress dialog opens, and reappears after progress closes', async () => {
    const p1 = dialogs.askArchivePassword('C:\\arc1.rar', false);
    const p2 = dialogs.askArchivePassword('C:\\arc2.7z', false);

    expect(dialogs.current).toMatchObject({
      kind: 'password',
      archivePath: 'C:\\arc1.rar',
    });

    let answered2: string | null = 'not_settled';
    p2.then((res) => {
      answered2 = res;
    });

    // User submits prompt 1
    const firstPrompt = dialogs.current as { retry: (pw: string) => Promise<void> };
    await firstPrompt.retry('pass1');
    expect(await p1).toBe('pass1');

    // Prompt 2 is now active
    expect(dialogs.current).toMatchObject({
      kind: 'password',
      archivePath: 'C:\\arc2.7z',
    });
    expect(answered2).toBe('not_settled');

    // Job 1 opens progress dialog: prompt 2 must be unshifted to queue, NOT cancelled
    await dialogs.withProgress(
      { title: 'Extract job 1', archivePath: 'C:\\arc1.rar' },
      async () => {
        expect(dialogs.current?.kind).toBe('archiveProgress');
        expect(answered2).toBe('not_settled');
      },
    );

    // After job 1 progress closes, prompt 2 is restored as current!
    expect(dialogs.current).toMatchObject({
      kind: 'password',
      archivePath: 'C:\\arc2.7z',
    });
    expect(answered2).toBe('not_settled');

    // User can now answer prompt 2
    const secondPrompt = dialogs.current as { retry: (pw: string) => Promise<void> };
    await secondPrompt.retry('pass2');

    expect(await p2).toBe('pass2');
    expect(dialogs.current).toBeNull();
  });

  it('FER-508 (P3-1): non-prompt dialog (error/summary) unshifts waiting prompt and restores it after close', async () => {
    const p1 = dialogs.askArchivePassword('C:\\arc1.rar', false);
    let answered: string | null = 'not_settled';
    p1.then((res) => {
      answered = res;
    });

    expect(dialogs.current).toMatchObject({
      kind: 'password',
      archivePath: 'C:\\arc1.rar',
    });

    // A non-prompt error dialog opens while prompt is waiting
    dialogs.open({ kind: 'error', title: 'Error', message: 'Something broke' });
    expect(dialogs.current).toMatchObject({ kind: 'error', message: 'Something broke' });
    expect(answered).toBe('not_settled');

    // User closes error dialog
    dialogs.close();

    // Prompt is restored
    expect(dialogs.current).toMatchObject({
      kind: 'password',
      archivePath: 'C:\\arc1.rar',
    });
    expect(answered).toBe('not_settled');

    // Now test extractSummary dialog
    dialogs.showExtractSummary({ written: 5 });
    expect(dialogs.current).toMatchObject({ kind: 'extractSummary', written: 5 });
    expect(answered).toBe('not_settled');

    // User closes summary dialog
    dialogs.close();

    // Prompt is restored again
    expect(dialogs.current).toMatchObject({
      kind: 'password',
      archivePath: 'C:\\arc1.rar',
    });
    expect(answered).toBe('not_settled');

    // User answers prompt
    const prompt = dialogs.current as { retry: (pw: string) => Promise<void> };
    await prompt.retry('secret');
    expect(await p1).toBe('secret');
    expect(dialogs.current).toBeNull();
  });

  it('FER-508 (P3-1): closeAll and reset cancel all active and queued prompts', async () => {
    const p1 = dialogs.askArchivePassword('C:\\arc1.rar', false);
    const p2 = dialogs.askArchivePassword('C:\\arc2.7z', false);
    const p3 = dialogs.askArchivePassword('C:\\arc3.zip', false);

    expect(dialogs.current).toMatchObject({ kind: 'password', archivePath: 'C:\\arc1.rar' });
    expect(dialogs.queue).toHaveLength(2);

    dialogs.closeAll();

    const [res1, res2, res3] = await Promise.all([p1, p2, p3]);
    expect(res1).toBeNull();
    expect(res2).toBeNull();
    expect(res3).toBeNull();
    expect(dialogs.current).toBeNull();
    expect(dialogs.queue).toEqual([]);

    // Same with reset()
    const p4 = dialogs.askArchivePassword('C:\\arc4.rar', false);
    const p5 = dialogs.askArchivePassword('C:\\arc5.7z', false);
    dialogs.reset();
    const [res4, res5] = await Promise.all([p4, p5]);
    expect(res4).toBeNull();
    expect(res5).toBeNull();
    expect(dialogs.current).toBeNull();
    expect(dialogs.queue).toEqual([]);
  });
});
