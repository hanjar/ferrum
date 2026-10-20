// FER-576 (review R1, S2): "Test Archive passed" must also mean "the archive holds what will be deleted".
import { beforeEach, describe, expect, it, vi } from 'vitest';

const archiveIpc = vi.hoisted(() => ({ archiveTest: vi.fn() }));
vi.mock('../ipc/archive', () => ({ archiveTest: archiveIpc.archiveTest }));

const { verifyArchiveOutput } = await import('./aiRuntime');

const entries = (...statuses: string[]) => ({
  archivePath: 'x.7z',
  entries: statuses.map((status, i) => ({ memberPath: `m${i}`, status })),
});

beforeEach(() => archiveIpc.archiveTest.mockReset());

describe('verifyArchiveOutput', () => {
  it('passes when every entry passed and the count is the expected one', async () => {
    archiveIpc.archiveTest.mockResolvedValue(entries('passed', 'passed', 'passed'));
    expect(await verifyArchiveOutput('x.7z', { members: 3 })).toEqual({
      ok: true,
      message: '3 entri terverifikasi',
    });
  });

  it('fails when the archive holds fewer members than the sources have (something was skipped)', async () => {
    archiveIpc.archiveTest.mockResolvedValue(entries('passed', 'passed'));
    const verdict = await verifyArchiveOutput('x.7z', { members: 3 });
    expect(verdict.ok).toBe(false);
    expect(verdict.message).toContain('seharusnya 3');
  });

  it('fails when the archive holds more members than expected', async () => {
    archiveIpc.archiveTest.mockResolvedValue(entries('passed', 'passed', 'passed', 'passed'));
    expect((await verifyArchiveOutput('x.7z', { members: 3 })).ok).toBe(false);
  });

  it('fails on any entry that did not pass, whatever the count', async () => {
    for (const bad of ['failed', 'passwordRequired', 'unsupported']) {
      archiveIpc.archiveTest.mockResolvedValue(entries('passed', bad));
      expect((await verifyArchiveOutput('x.7z', { members: 2 })).ok, bad).toBe(false);
    }
  });

  it('fails on an empty archive', async () => {
    archiveIpc.archiveTest.mockResolvedValue(entries());
    expect((await verifyArchiveOutput('x.7z', { members: 0 })).ok).toBe(false);
  });

  it('without an expected count it still needs every entry to pass', async () => {
    archiveIpc.archiveTest.mockResolvedValue(entries('passed'));
    expect((await verifyArchiveOutput('x.7z')).ok).toBe(true);
  });
});
