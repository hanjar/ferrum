import { beforeEach, describe, expect, it, vi } from 'vitest';

// FER-575: the open-from-archive flow. The IPC bridge is mocked; what matters is which command
// receives which archive/member split, the password retry loop, and what the user is told.
const bridge = vi.hoisted(() => ({ open: vi.fn() }));
vi.mock('../ipc/archive', () => ({
  archiveOpenMember: (...a: unknown[]) => bridge.open(...a),
  newArchiveRequestId: () => 'arc-test',
}));

const { archiveMemberOf, openArchiveMemberAt } = await import('./openArchiveMember');
const { FsError } = await import('../ipc/types');

const KNOWN: ReadonlySet<string> = new Set([
  'C:\\data\\arsip.rar',
  'D:\\a.ZIP',
  'C:\\a.zip',
  'C:\\a.7z',
  'C:\\a.rar',
]);

function deps(over: Partial<Parameters<typeof openArchiveMemberAt>[1]> = {}) {
  return {
    knownArchives: KNOWN,
    askPassword: vi.fn(async () => null as string | null),
    onError: vi.fn(),
    ...over,
  };
}

function archiveErr(archiveCode: string, message = 'x') {
  return new FsError({ code: 'io', message, archiveCode } as never);
}

beforeEach(() => {
  bridge.open.mockReset().mockResolvedValue(true);
});

describe('archiveMemberOf', () => {
  it('splits a file inside an archive into archive path and slash-separated member', () => {
    expect(archiveMemberOf('C:\\data\\arsip.rar\\docs\\Laporan 1.pdf', KNOWN)).toEqual({
      archivePath: 'C:\\data\\arsip.rar',
      memberPath: 'docs/Laporan 1.pdf',
    });
    expect(archiveMemberOf('D:\\a.ZIP\\x.txt', KNOWN)).toEqual({
      archivePath: 'D:\\a.ZIP',
      memberPath: 'x.txt',
    });
  });

  it('is null for plain disk files and for the archive file itself', () => {
    expect(archiveMemberOf('C:\\data\\notes.txt', KNOWN)).toBeNull();
    expect(archiveMemberOf('C:\\data\\arsip.rar', KNOWN)).toBeNull();
  });
});

describe('openArchiveMemberAt', () => {
  it('leaves a plain disk path to the normal open and calls nothing', async () => {
    const d = deps();
    expect(await openArchiveMemberAt('C:\\data\\notes.txt', d)).toBe('notMember');
    expect(bridge.open).not.toHaveBeenCalled();
  });

  it('sends the archive and member separately, never the virtual path', async () => {
    const d = deps();
    expect(await openArchiveMemberAt('C:\\data\\arsip.rar\\docs\\a.pdf', d)).toBe('opened');
    expect(bridge.open).toHaveBeenCalledWith(
      'C:\\data\\arsip.rar',
      'docs/a.pdf',
      undefined,
      'arc-test',
    );
    expect(d.onError).not.toHaveBeenCalled();
  });

  it('a declined launch is quiet', async () => {
    bridge.open.mockResolvedValue(false);
    const d = deps();
    expect(await openArchiveMemberAt('C:\\a.zip\\x.txt', d)).toBe('declined');
    expect(d.onError).not.toHaveBeenCalled();
  });

  it('reuses the password the pane already holds', async () => {
    const d = deps({ passwordFor: () => 'pw1' });
    await openArchiveMemberAt('C:\\a.zip\\x.txt', d);
    expect(bridge.open).toHaveBeenCalledWith('C:\\a.zip', 'x.txt', 'pw1', 'arc-test');
  });

  it('asks for the password once, retries with it and remembers it after success', async () => {
    bridge.open.mockRejectedValueOnce(archiveErr('passwordRequired')).mockResolvedValueOnce(true);
    const remember = vi.fn();
    const d = deps({ askPassword: vi.fn(async () => 'secret'), rememberPassword: remember });
    expect(await openArchiveMemberAt('C:\\a.7z\\x.txt', d)).toBe('opened');
    expect(d.askPassword).toHaveBeenCalledWith('C:\\a.7z', false);
    expect(bridge.open).toHaveBeenLastCalledWith('C:\\a.7z', 'x.txt', 'secret', 'arc-test');
    expect(remember).toHaveBeenCalledWith('C:\\a.7z', 'secret');
  });

  it('a wrong password asks again with the retry note; a wrong one is never remembered', async () => {
    bridge.open
      .mockRejectedValueOnce(archiveErr('passwordRequired'))
      .mockRejectedValueOnce(archiveErr('badPassword'))
      .mockResolvedValueOnce(true);
    const remember = vi.fn();
    const answers = ['bad', 'good'];
    const ask = vi.fn<(p: string, wrong: boolean) => Promise<string | null>>(
      async () => answers.shift() ?? null,
    );
    const d = deps({ askPassword: ask, rememberPassword: remember });
    expect(await openArchiveMemberAt('C:\\a.rar\\x.txt', d)).toBe('opened');
    expect(ask.mock.calls.map((c) => c[1])).toEqual([false, true]);
    expect(remember).toHaveBeenCalledTimes(1);
    expect(remember).toHaveBeenCalledWith('C:\\a.rar', 'good');
  });

  it('dismissing the password prompt opens nothing and shows no error', async () => {
    bridge.open.mockRejectedValue(archiveErr('passwordRequired'));
    const d = deps();
    expect(await openArchiveMemberAt('C:\\a.rar\\x.txt', d)).toBe('declined');
    expect(bridge.open).toHaveBeenCalledTimes(1);
    expect(d.onError).not.toHaveBeenCalled();
  });

  it('reports a clear error (size limit, missing member, ...) once', async () => {
    bridge.open.mockRejectedValue(archiveErr('limitExceeded', 'File terlalu besar'));
    const d = deps();
    expect(await openArchiveMemberAt('C:\\a.rar\\big.iso', d)).toBe('failed');
    expect(d.onError).toHaveBeenCalledTimes(1);
    expect(d.onError).toHaveBeenCalledWith('File terlalu besar');
  });

  it('a cancel (including the native "run this?" No) is not an error', async () => {
    bridge.open.mockRejectedValue(new FsError({ code: 'cancelled', message: 'dibatalkan' }));
    const d = deps();
    expect(await openArchiveMemberAt('C:\\a.rar\\setup.exe', d)).toBe('declined');
    expect(d.onError).not.toHaveBeenCalled();
  });

  it('shows the working notice only for slow extractions and always clears it', async () => {
    vi.useFakeTimers();
    try {
      const clear = vi.fn();
      const show = vi.fn(() => clear);
      let finish: (v: boolean) => void = () => {};
      bridge.open.mockReturnValue(new Promise<boolean>((r) => (finish = r)));
      const p = openArchiveMemberAt(
        'C:\\a.rar\\docs\\big.pdf',
        deps({ showWorking: show, slowMs: 300 }),
      );
      await vi.advanceTimersByTimeAsync(299);
      expect(show).not.toHaveBeenCalled();
      await vi.advanceTimersByTimeAsync(2);
      expect(show).toHaveBeenCalledWith('big.pdf');
      finish(true);
      await p;
      expect(clear).toHaveBeenCalledTimes(1);

      // A fast open never shows it.
      show.mockClear();
      bridge.open.mockResolvedValue(true);
      await openArchiveMemberAt('C:\\a.rar\\x.txt', deps({ showWorking: show, slowMs: 300 }));
      await vi.advanceTimersByTimeAsync(1000);
      expect(show).not.toHaveBeenCalled();
    } finally {
      vi.useRealTimers();
    }
  });
});

describe('only real opened archives are routed to the archive command (S3)', () => {
  const folderFile = 'C:\\data\\backup.zip\\notes.txt';
  it('a real disk folder named backup.zip is left to the normal open', async () => {
    expect(archiveMemberOf(folderFile, KNOWN)).toBeNull();
    const d = deps();
    expect(await openArchiveMemberAt(folderFile, d)).toBe('notMember');
    expect(bridge.open).not.toHaveBeenCalled();
  });

  it('the same path becomes an archive member once the pane opened that archive', async () => {
    const known = new Set(['C:\\data\\backup.zip']);
    expect(await openArchiveMemberAt(folderFile, deps({ knownArchives: known }))).toBe('opened');
    expect(bridge.open).toHaveBeenCalledWith(
      'C:\\data\\backup.zip',
      'notes.txt',
      undefined,
      'arc-test',
    );
  });

  it('a wrong password is forgotten, not kept', async () => {
    bridge.open.mockRejectedValueOnce(archiveErr('badPassword')).mockResolvedValueOnce(true);
    const forget = vi.fn();
    const d = deps({ askPassword: vi.fn(async () => 'good'), forgetPassword: forget });
    await openArchiveMemberAt('C:\\a.rar\\x.txt', d);
    expect(forget).toHaveBeenCalledWith('C:\\a.rar');
  });
});
