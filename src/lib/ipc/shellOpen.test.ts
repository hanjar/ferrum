import { beforeEach, describe, expect, it, vi } from 'vitest';

const invokeMock = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invokeMock(...a) }));

const { openExternal, associateWith, openTerminal, printFile, openThirdPartyNotices } =
  await import('./shellOpen');
const { FsError } = await import('./types');

function fakeTauri() {
  (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {};
}
function unTauri() {
  delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__;
}

beforeEach(() => {
  invokeMock.mockReset().mockName('invoke');
  unTauri();
});

describe('third-party notices (FER-554)', () => {
  it('invokes the backend command without any path argument', async () => {
    fakeTauri();
    invokeMock.mockResolvedValue(undefined);
    await openThirdPartyNotices();
    expect(invokeMock).toHaveBeenCalledWith('open_third_party_notices', {});
  });

  it('rejects unsupported outside Tauri', async () => {
    await expect(openThirdPartyNotices()).rejects.toMatchObject({ code: 'unsupported' });
    expect(invokeMock).not.toHaveBeenCalled();
  });
});

describe('shell open backend wrapper (F2-25)', () => {
  it('rejects unsupported outside Tauri', async () => {
    await expect(openExternal('C:\\t\\a.txt')).rejects.toMatchObject({ code: 'unsupported' });
    await expect(associateWith('C:\\t\\a.txt')).rejects.toMatchObject({ code: 'unsupported' });
    await expect(openTerminal('C:\\t')).rejects.toMatchObject({ code: 'unsupported' });
    await expect(printFile('C:\\t\\a.txt')).rejects.toMatchObject({ code: 'unsupported' });
    expect(invokeMock).not.toHaveBeenCalled();
  });

  it('invokes the structured commands with verbatim paths', async () => {
    fakeTauri();
    invokeMock.mockResolvedValue(undefined);
    // Spaces, Unicode and metacharacters cross the boundary as data.
    const weird = String.raw`C:\Program Files (x86)\béhind thé scenes\a & "b".txt`;
    await expect(openExternal(weird)).resolves.toBeUndefined();
    expect(invokeMock).toHaveBeenCalledWith('open_external', { path: weird });
    await expect(associateWith(weird)).resolves.toBeUndefined();
    expect(invokeMock).toHaveBeenCalledWith('associate_with', { path: weird });
    await expect(openTerminal(weird, 'wt')).resolves.toBeUndefined();
    expect(invokeMock).toHaveBeenCalledWith('open_terminal', { cwd: weird, program: 'wt' });
    await expect(printFile(weird)).resolves.toBeUndefined();
    expect(invokeMock).toHaveBeenCalledWith('print_file', { path: weird });
  });

  it('defaults the terminal preference to auto', async () => {
    fakeTauri();
    invokeMock.mockResolvedValue(undefined);
    await openTerminal('C:\\t');
    expect(invokeMock).toHaveBeenCalledWith('open_terminal', { cwd: 'C:\\t', program: 'auto' });
  });

  it('maps structured IPC errors to FsError', async () => {
    fakeTauri();
    invokeMock.mockRejectedValue({
      code: 'notFound',
      message: 'No application is associated with this file for the requested action',
      path: 'C:\\t\\a.xyz',
    });
    const err = await openExternal('C:\\t\\a.xyz').catch((e: unknown) => e);
    expect(err).toBeInstanceOf(FsError);
    expect(err).toMatchObject({ code: 'notFound', path: 'C:\\t\\a.xyz' });
  });

  it('maps non-object rejections to a generic io FsError', async () => {
    fakeTauri();
    invokeMock.mockRejectedValue('boom');
    await expect(openExternal('C:\\t\\a.txt')).rejects.toMatchObject({ code: 'io' });
  });
});
