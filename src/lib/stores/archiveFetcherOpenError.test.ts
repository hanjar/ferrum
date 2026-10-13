import { describe, expect, it, vi } from 'vitest';

// FER-75: a failed archive open is reported to the UI (so it can explain "part 2 of a multipart
// archive") and still rejects, and a failed open is not remembered as an opened archive.
const archiveList = vi.hoisted(() => vi.fn());
vi.mock('../ipc/archive', () => ({ archiveList }));
vi.mock('../ipc/tauri', () => ({ listDir: vi.fn() }));

const { createArchiveAwareFetcher } = await import('./archiveFetcher');

describe('createArchiveAwareFetcher open errors (FER-75)', () => {
  it('reports the failure to the callback and still throws', async () => {
    const err = new Error('part 2');
    archiveList.mockRejectedValue(err);
    const known = new Set<string>();
    const seen: [string, unknown][] = [];
    const fetcher = createArchiveAwareFetcher(
      known,
      (p) => known.add(p),
      (p, e) => seen.push([p, e]),
    );
    await expect(fetcher('C:\\a\\x.part2.rar')).rejects.toBe(err);
    expect(seen).toEqual([['C:\\a\\x.part2.rar', err]]);
    expect(known.size).toBe(0);
  });

  it('a successful open is remembered and does not call the error callback', async () => {
    archiveList.mockResolvedValue({ entries: [] });
    const known = new Set<string>();
    const onError = vi.fn();
    const fetcher = createArchiveAwareFetcher(known, (p) => known.add(p), onError);
    await fetcher('C:\\a\\x.zip');
    expect(known.has('C:\\a\\x.zip')).toBe(true);
    expect(onError).not.toHaveBeenCalled();
  });

  it('browsing inside an already opened archive does not use the open-error callback', async () => {
    archiveList.mockRejectedValue(new Error('io'));
    const known = new Set<string>(['C:\\a\\x.zip']);
    const onError = vi.fn();
    const fetcher = createArchiveAwareFetcher(known, undefined, onError);
    await expect(fetcher('C:\\a\\x.zip\\docs')).rejects.toThrow('io');
    expect(onError).not.toHaveBeenCalled();
  });
});
