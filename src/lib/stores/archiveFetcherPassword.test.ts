import { beforeEach, describe, expect, it, vi } from 'vitest';
import { FsError } from '../ipc/types';

// FER-504: opening a header-encrypted archive (listing answers
// PasswordRequired/BadPassword) prompts for the password and retries the
// listing with it. The password lives only in the fetcher's memory: reused
// while the pane stays inside the archive, forgotten once it leaves — so
// nothing of it can reach the persisted session or prefs.
const archiveList = vi.hoisted(() => vi.fn());
vi.mock('../ipc/archive', () => ({ archiveList }));
vi.mock('../ipc/tauri', () => ({ listDir: vi.fn() }));

const { createArchiveAwareFetcher } = await import('./archiveFetcher');

const pwRequired = () =>
  new FsError({ code: 'io', message: 'password required', archiveCode: 'passwordRequired' });
const badPassword = () =>
  new FsError({ code: 'io', message: 'wrong password', archiveCode: 'badPassword' });

function passwordsSeen(): (string | null | undefined)[] {
  return archiveList.mock.calls.map((c) => c[3]);
}

describe('createArchiveAwareFetcher password flow (FER-504)', () => {
  beforeEach(() => {
    archiveList.mockReset();
  });

  it('prompts on PasswordRequired and retries the listing with the typed password', async () => {
    archiveList.mockRejectedValueOnce(pwRequired()).mockResolvedValue({ entries: [] });
    const known = new Set<string>();
    const ask = vi.fn().mockResolvedValue('opensesame');
    const fetcher = createArchiveAwareFetcher(known, (p) => known.add(p), undefined, ask);

    await fetcher('C:\\a\\enc.rar');
    expect(ask).toHaveBeenCalledTimes(1);
    expect(ask).toHaveBeenCalledWith('C:\\a\\enc.rar', false);
    expect(passwordsSeen()).toEqual([undefined, 'opensesame']);
    expect(known.has('C:\\a\\enc.rar')).toBe(true); // opened only after success
  });

  it('a wrong password re-prompts with the wrong flag, then succeeds', async () => {
    archiveList
      .mockRejectedValueOnce(pwRequired())
      .mockRejectedValueOnce(badPassword())
      .mockResolvedValue({ entries: [] });
    const ask = vi.fn().mockResolvedValueOnce('nope').mockResolvedValueOnce('correct');
    const fetcher = createArchiveAwareFetcher(new Set<string>(), undefined, undefined, ask);

    await fetcher('C:\\a\\enc.rar');
    expect(ask).toHaveBeenCalledTimes(2);
    expect(ask).toHaveBeenNthCalledWith(2, 'C:\\a\\enc.rar', true);
    expect(passwordsSeen()).toEqual([undefined, 'nope', 'correct']);
  });

  it('a dismissed prompt rethrows the archive error and stores nothing', async () => {
    const err = pwRequired();
    archiveList.mockRejectedValue(err);
    const known = new Set<string>();
    const seen: [string, unknown][] = [];
    const ask = vi.fn().mockResolvedValue(null);
    const fetcher = createArchiveAwareFetcher(
      known,
      (p) => known.add(p),
      (p, e) => seen.push([p, e]),
      ask,
    );

    await expect(fetcher('C:\\a\\enc.rar')).rejects.toBe(err);
    expect(passwordsSeen()).toEqual([undefined]);
    expect(known.size).toBe(0);
    expect(seen).toEqual([['C:\\a\\enc.rar', err]]);
  });

  it('non-password errors never prompt', async () => {
    archiveList.mockRejectedValue(new Error('io'));
    const ask = vi.fn();
    const fetcher = createArchiveAwareFetcher(new Set<string>(), undefined, undefined, ask);
    await expect(fetcher('C:\\a\\plain.zip')).rejects.toThrow('io');
    expect(ask).not.toHaveBeenCalled();
  });

  it('the password is reused for listings inside the archive without re-prompting', async () => {
    archiveList.mockRejectedValueOnce(pwRequired()).mockResolvedValue({ entries: [] });
    const ask = vi.fn().mockResolvedValue('opensesame');
    const known = new Set<string>();
    const fetcher = createArchiveAwareFetcher(known, (p) => known.add(p), undefined, ask);

    await fetcher('C:\\a\\enc.rar'); // open: prompts once
    await fetcher('C:\\a\\enc.rar\\docs'); // browse: same password, no prompt
    expect(ask).toHaveBeenCalledTimes(1);
    expect(passwordsSeen()[2]).toBe('opensesame');
  });

  it('leaving the archive for a disk path forgets the password', async () => {
    // open → prompt+retry OK; disk nav; re-open fails again → fresh prompt.
    archiveList
      .mockRejectedValueOnce(pwRequired())
      .mockResolvedValueOnce({ entries: [] })
      .mockRejectedValueOnce(pwRequired())
      .mockResolvedValue({ entries: [] });
    const ask = vi.fn().mockResolvedValue('opensesame');
    const known = new Set<string>();
    const fetcher = createArchiveAwareFetcher(known, (p) => known.add(p), undefined, ask);

    await fetcher('C:\\a\\enc.rar');
    await fetcher('C:\\somewhere'); // disk navigation wipes the memory
    await fetcher('C:\\a\\enc.rar'); // re-open starts fresh
    expect(ask).toHaveBeenCalledTimes(2);
    expect(passwordsSeen()).toEqual([undefined, 'opensesame', undefined, 'opensesame']);
  });

  it('a wrong password that is then dismissed is not remembered', async () => {
    archiveList
      .mockRejectedValueOnce(pwRequired())
      .mockRejectedValueOnce(badPassword())
      .mockRejectedValueOnce(pwRequired())
      .mockResolvedValue({ entries: [] });
    const ask = vi
      .fn()
      .mockResolvedValueOnce('nope')
      .mockResolvedValueOnce(null)
      .mockResolvedValueOnce('correct');
    const fetcher = createArchiveAwareFetcher(new Set<string>(), undefined, undefined, ask);

    await expect(fetcher('C:\\a\\enc.rar')).rejects.toBeInstanceOf(FsError);
    await fetcher('C:\\a\\enc.rar'); // retry: the wrong 'nope' must not be reused
    expect(passwordsSeen()).toEqual([undefined, 'nope', undefined, 'correct']);
  });

  it('moving to a different archive forgets the previous archive password', async () => {
    archiveList
      .mockRejectedValueOnce(pwRequired()) // enc.rar
      .mockResolvedValueOnce({ entries: [] }) // enc.rar + 'opensesame'
      .mockResolvedValueOnce({ entries: [] }) // other.zip
      .mockRejectedValueOnce(pwRequired()) // back into enc.rar: no stored password
      .mockResolvedValue({ entries: [] });
    const ask = vi.fn().mockResolvedValue('opensesame');
    const known = new Set<string>();
    const fetcher = createArchiveAwareFetcher(known, (p) => known.add(p), undefined, ask);

    await fetcher('C:\\a\\enc.rar');
    await fetcher('C:\\b\\other.zip'); // archive → another archive
    await fetcher('C:\\a\\enc.rar\\docs');
    expect(ask).toHaveBeenCalledTimes(2);
    expect(passwordsSeen()).toEqual([undefined, 'opensesame', undefined, undefined, 'opensesame']);
  });

  it('without an askPassword callback the flow behaves exactly as before', async () => {
    archiveList.mockRejectedValue(pwRequired());
    const fetcher = createArchiveAwareFetcher(new Set<string>());
    await expect(fetcher('C:\\a\\enc.rar')).rejects.toBeInstanceOf(FsError);
    expect(passwordsSeen()).toEqual([undefined]);
  });
});
