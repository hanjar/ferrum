/** PRV-06: media metadata store, privacy gate and stale-result invalidation. */
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { mediaMetaStore, formatDuration, formatGps } from './mediaMetaStore.svelte';
import { ui } from '../uiState.svelte';
import { FsError } from '../ipc/types';
import type { MediaMetadata } from '../ipc/mediaMeta';

const readMock = vi.fn();
vi.mock('../ipc/mediaMeta', () => ({
  readMediaMetadata: (...a: unknown[]) => readMock(...a),
}));

const meta = (w: number): MediaMetadata => ({ kind: 'image', width: w, height: 1, other: [] });

function deferred<T>() {
  let resolve!: (v: T) => void;
  const promise = new Promise<T>((r) => (resolve = r));
  return { promise, resolve };
}
const flush = () => new Promise((r) => setTimeout(r, 0));

beforeEach(() => {
  readMock.mockReset();
  mediaMetaStore.reset();
  ui.preview.readMetadata = true;
});

describe('mediaMetaStore', () => {
  it('loads for a target', async () => {
    readMock.mockResolvedValue(meta(5));
    mediaMetaStore.setTarget('/a.jpg');
    expect(mediaMetaStore.state).toBe('loading');
    await flush();
    expect(mediaMetaStore.state).toBe('ready');
    expect(mediaMetaStore.data?.width).toBe(5);
  });

  it('readMetadata OFF: never invokes and data is null', async () => {
    ui.preview.readMetadata = false;
    mediaMetaStore.setTarget('/a.jpg');
    await flush();
    expect(readMock).not.toHaveBeenCalled();
    expect(mediaMetaStore.data).toBeNull();
    expect(mediaMetaStore.state).toBe('idle');
  });

  it('rapid selection: old result is discarded', async () => {
    const a = deferred<MediaMetadata>();
    readMock.mockReturnValueOnce(a.promise).mockResolvedValueOnce(meta(2));
    mediaMetaStore.setTarget('/a.jpg');
    mediaMetaStore.setTarget('/b.jpg');
    await flush();
    a.resolve(meta(1));
    await flush();
    expect(mediaMetaStore.data?.width).toBe(2);
  });

  it('toggle ON to OFF mid-request: result dropped', async () => {
    const a = deferred<MediaMetadata>();
    readMock.mockReturnValueOnce(a.promise);
    mediaMetaStore.setTarget('/a.jpg');
    ui.preview.readMetadata = false;
    mediaMetaStore.refresh();
    a.resolve(meta(1));
    await flush();
    expect(mediaMetaStore.data).toBeNull();
    expect(mediaMetaStore.state).toBe('idle');
  });

  it('OFF without refresh still drops a late result', async () => {
    const a = deferred<MediaMetadata>();
    readMock.mockReturnValueOnce(a.promise);
    mediaMetaStore.setTarget('/a.jpg');
    ui.preview.readMetadata = false;
    a.resolve(meta(1));
    await flush();
    expect(mediaMetaStore.data).toBeNull();
  });

  it('null target resets and drops in-flight', async () => {
    const a = deferred<MediaMetadata>();
    readMock.mockReturnValueOnce(a.promise);
    mediaMetaStore.setTarget('/a.jpg');
    mediaMetaStore.setTarget(null);
    a.resolve(meta(1));
    await flush();
    expect(mediaMetaStore.data).toBeNull();
    expect(mediaMetaStore.state).toBe('idle');
  });

  it('unsupported becomes unavailable, other errors become error', async () => {
    readMock.mockRejectedValueOnce(new FsError({ code: 'unsupported', message: 'x' }));
    mediaMetaStore.setTarget('/a.txt');
    await flush();
    expect(mediaMetaStore.state).toBe('unavailable');
    readMock.mockRejectedValueOnce(new FsError({ code: 'io', message: 'x' }));
    mediaMetaStore.setTarget('/b.jpg');
    await flush();
    expect(mediaMetaStore.state).toBe('error');
  });

  it('formatters', () => {
    expect(formatDuration(65_000)).toBe('1:05');
    expect(formatDuration(3_725_000)).toBe('1:02:05');
    expect(formatGps(-6.5, 106.75)).toBe('6.50000° S, 106.75000° E');
  });
});
