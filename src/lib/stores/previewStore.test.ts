/**
 * F2-17 — preview store: debounced target loads, stale-load supersession,
 * and safe asset-src/text decoding helpers. readPreview is mocked so no
 * Tauri is touched.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { tick } from 'svelte';
import { ui, PREVIEW_DEFAULTS } from '../uiState.svelte';
import {
  formatPreviewLimit,
  archiveMemberOf,
  PREVIEW_DEBOUNCE_MS,
  previewStore,
  PreviewStore,
  previewImageSrc,
  previewText,
} from './previewStore.svelte';
import type { PreviewData } from '../ipc/preview';

vi.mock('../ipc/preview', () => ({
  readPreview: (...args: unknown[]) => readPreviewMock(...args),
  readArchiveMemberPreview: (...args: unknown[]) => readArchiveMock(...args),
  cancelArchiveRequest: (...args: unknown[]) => cancelMock(...args),
}));

const readPreviewMock = vi.fn();
const readArchiveMock = vi.fn();
const cancelMock = vi.fn();

function data(kind: string, base64 = '', truncated = false): PreviewData {
  return {
    kind: kind as PreviewData['kind'],
    data: base64,
    truncated,
    tooLarge: false,
    size: 1,
    modifiedMs: 1,
  };
}

beforeEach(() => {
  ui.preview.maxTextBytes = PREVIEW_DEFAULTS.maxTextBytes;
  readPreviewMock.mockReset();
  readArchiveMock.mockReset();
  cancelMock.mockReset();
  previewStore.reset();
});

afterEach(() => {
  vi.useRealTimers();
});

describe('preview store', () => {
  it('is idle without a target and clears on null', async () => {
    previewStore.setTarget(null);
    expect(previewStore.state).toBe('idle');
    expect(previewStore.data).toBeNull();
    await previewStore.flush();
    expect(readPreviewMock).not.toHaveBeenCalled();
  });

  it('loads a text file after the debounce window', async () => {
    vi.useFakeTimers();
    readPreviewMock.mockResolvedValue(data('text', 'aGk='));
    previewStore.setTarget({
      name: 'a.txt',
      path: 'C:\\d\\a.txt',
      isDir: false,
      size: 2,
      modifiedMs: 1,
    });
    expect(readPreviewMock).not.toHaveBeenCalled(); // debounced
    await vi.advanceTimersByTimeAsync(PREVIEW_DEBOUNCE_MS + 1);
    expect(previewStore.state).toBe('ready');
    expect(readPreviewMock).toHaveBeenCalledWith('C:\\d\\a.txt', 2_000_000);
    expect(previewText(previewStore.data!.data)).toBe('hi');
  });

  it('drops the previous result as soon as the target changes (no stale data during debounce)', async () => {
    vi.useFakeTimers();
    readPreviewMock.mockResolvedValue(data('video'));
    previewStore.setTarget({
      name: 'a.mp4',
      path: 'C:/d/a.mp4',
      isDir: false,
      size: 9,
      modifiedMs: 1,
    });
    await vi.advanceTimersByTimeAsync(PREVIEW_DEBOUNCE_MS + 1);
    expect(previewStore.data?.kind).toBe('video');
    previewStore.setTarget({
      name: 'b.iso',
      path: 'C:/d/b.iso',
      isDir: false,
      size: 9,
      modifiedMs: 1,
    });
    expect(previewStore.data).toBeNull();
    expect(previewStore.state).toBe('loading');
  });

  it('supersedes a stale load when the cursor moves quickly', async () => {
    let resolveFirst: (d: PreviewData) => void = () => {};
    readPreviewMock.mockImplementationOnce(
      () => new Promise<PreviewData>((resolve) => (resolveFirst = resolve)),
    );
    readPreviewMock.mockResolvedValue(data('text', 'bmV3'));
    previewStore.setTarget({
      name: 'old',
      path: 'C:\\d\\old',
      isDir: false,
      size: 1,
      modifiedMs: 1,
    });
    void previewStore.flush(); // first load hangs by design
    expect(previewStore.state).toBe('loading');
    previewStore.setTarget({
      name: 'new',
      path: 'C:\\d\\new',
      isDir: false,
      size: 1,
      modifiedMs: 1,
    });
    await previewStore.flush();
    expect(previewStore.state).toBe('ready');
    expect(previewText(previewStore.data!.data)).toBe('new');
    // the hung first load resolving late must not overwrite anything
    resolveFirst(data('text', 'T0xE'));
    await new Promise((r) => setTimeout(r, 0));
    expect(previewText(previewStore.data!.data)).toBe('new');
  });

  it('does not reload when the target path is unchanged', async () => {
    vi.useFakeTimers();
    readPreviewMock.mockResolvedValue(data('text', 'aGk='));
    const target = { name: 'a.txt', path: 'C:\\d\\a.txt', isDir: false, size: 2, modifiedMs: 1 };
    previewStore.setTarget(target);
    await vi.advanceTimersByTimeAsync(PREVIEW_DEBOUNCE_MS + 1);
    expect(readPreviewMock).toHaveBeenCalledTimes(1);
    previewStore.setTarget({ ...target }); // same path: no new load scheduled
    await vi.advanceTimersByTimeAsync(PREVIEW_DEBOUNCE_MS + 1);
    expect(readPreviewMock).toHaveBeenCalledTimes(1);
  });

  it('directories skip the file read and show metadata only', async () => {
    previewStore.setTarget({
      name: 'sub',
      path: 'C:\\d\\sub',
      isDir: true,
      size: 0,
      modifiedMs: 1,
    });
    await previewStore.flush();
    expect(readPreviewMock).not.toHaveBeenCalled();
    expect(previewStore.state).toBe('ready');
    expect(previewStore.data).toBeNull();
  });

  it('surfaces load errors honestly and recovers on a new target', async () => {
    readPreviewMock.mockRejectedValueOnce(new Error('Access is denied'));
    previewStore.setTarget({ name: 'x', path: 'C:\\d\\x', isDir: false, size: 1, modifiedMs: 1 });
    await previewStore.flush();
    expect(previewStore.state).toBe('error');
    expect(previewStore.errorText).toBe('Access is denied');
    readPreviewMock.mockResolvedValue(data('text', 'aGk='));
    previewStore.setTarget({ name: 'y', path: 'C:\\d\\y', isDir: false, size: 1, modifiedMs: 1 });
    await previewStore.flush();
    expect(previewStore.state).toBe('ready');
    expect(previewStore.errorText).toBe('');
  });

  it('reloads after an error even when the path is unchanged', async () => {
    readPreviewMock.mockRejectedValueOnce(new Error('disk error'));
    const target = { name: 'x', path: 'C:\\d\\x', isDir: false, size: 1, modifiedMs: 1 };
    previewStore.setTarget(target);
    await previewStore.flush();
    expect(previewStore.state).toBe('error');
    readPreviewMock.mockResolvedValue(data('text', 'aGk='));
    previewStore.setTarget({ ...target });
    await previewStore.flush();
    expect(previewStore.state).toBe('ready');
  });
});

describe('text limit (PRV-03)', () => {
  const big = (name: string, size: number) => ({
    name,
    path: `C:\\d\\${name}`,
    isDir: false,
    size,
    modifiedMs: 5,
  });

  it('uses ui.preview.maxTextBytes as the read limit', async () => {
    ui.preview.maxTextBytes = 500_000;
    readPreviewMock.mockResolvedValue(data('text', 'aGk='));
    previewStore.setTarget(big('a.txt', 10));
    await previewStore.flush();
    expect(readPreviewMock).toHaveBeenCalledWith('C:\\d\\a.txt', 500_000);
  });

  it('files over the limit are metadata-only and never read', async () => {
    ui.preview.maxTextBytes = 100_000;
    previewStore.setTarget(big('huge.json', 100_001));
    await previewStore.flush();
    expect(readPreviewMock).not.toHaveBeenCalled();
    expect(previewStore.state).toBe('ready');
    expect(previewStore.data).toMatchObject({
      tooLarge: true,
      data: '',
      kind: 'json',
      size: 100_001,
    });
  });

  it('a file exactly at the limit is still read; media ignores the limit', async () => {
    ui.preview.maxTextBytes = 100_000;
    readPreviewMock.mockResolvedValue(data('text', 'aGk='));
    previewStore.setTarget(big('edge.txt', 100_000));
    await previewStore.flush();
    expect(readPreviewMock).toHaveBeenCalledTimes(1);
    readPreviewMock.mockResolvedValue(data('video'));
    previewStore.setTarget(big('clip.mp4', 900_000_000));
    await previewStore.flush();
    expect(readPreviewMock).toHaveBeenCalledTimes(2);
    expect(previewStore.data?.kind).toBe('video');
  });

  it('changing the limit reloads the current target', async () => {
    vi.useFakeTimers();
    readPreviewMock.mockResolvedValue(data('text', 'aGk='));
    previewStore.setTarget(big('a.txt', 300_000));
    await vi.advanceTimersByTimeAsync(PREVIEW_DEBOUNCE_MS + 1);
    expect(readPreviewMock).toHaveBeenCalledTimes(1);
    ui.preview.maxTextBytes = 100_000; // now smaller than the file
    await tick();
    await vi.advanceTimersByTimeAsync(PREVIEW_DEBOUNCE_MS + 1);
    expect(readPreviewMock).toHaveBeenCalledTimes(1); // metadata-only, no read
    expect(previewStore.data?.tooLarge).toBe(true);
    ui.preview.maxTextBytes = 500_000;
    await tick();
    await vi.advanceTimersByTimeAsync(PREVIEW_DEBOUNCE_MS + 1);
    expect(readPreviewMock).toHaveBeenLastCalledWith('C:\\d\\a.txt', 500_000);
    expect(previewStore.data?.tooLarge).toBe(false);
  });

  it('an in-flight result read under the old limit is discarded', async () => {
    let resolveOld: (d: PreviewData) => void = () => {};
    readPreviewMock.mockImplementationOnce(
      () => new Promise<PreviewData>((resolve) => (resolveOld = resolve)),
    );
    readPreviewMock.mockResolvedValue(data('text', 'bmV3'));
    previewStore.setTarget(big('a.txt', 10));
    void previewStore.flush();
    expect(previewStore.state).toBe('loading');
    ui.preview.maxTextBytes = 1_000_000;
    await tick();
    await previewStore.flush();
    expect(previewText(previewStore.data!.data)).toBe('new');
    resolveOld(data('text', 'T0xE'));
    await new Promise((r) => setTimeout(r, 0));
    expect(previewText(previewStore.data!.data)).toBe('new');
  });

  it('rapid selection ends on the last target only', async () => {
    vi.useFakeTimers();
    readPreviewMock.mockResolvedValue(data('text', 'aGk='));
    for (const n of ['a', 'b', 'c']) previewStore.setTarget(big(`${n}.txt`, 10));
    await vi.advanceTimersByTimeAsync(PREVIEW_DEBOUNCE_MS + 1);
    expect(readPreviewMock).toHaveBeenCalledTimes(1);
    expect(readPreviewMock).toHaveBeenCalledWith('C:\\d\\c.txt', 2_000_000);
  });

  it('formats decimal limits', () => {
    expect(formatPreviewLimit(100_000)).toBe('100 KB');
    expect(formatPreviewLimit(2_000_000)).toBe('2 MB');
    expect(formatPreviewLimit(1_500_000)).toBe('1.5 MB');
  });
});

describe('safe rendering helpers', () => {
  it('decodes base64 to lossy UTF-8 text', () => {
    expect(previewText('aMOpbGxv')).toBe('héllo'); // "héllo" in UTF-8
    // invalid UTF-8 bytes (0xFF) decode with replacement chars, no throw
    expect(previewText('/w==')).toBe('\uFFFD');
    expect(previewText('')).toBe('');
  });
});

describe('archive member preview branch (FER-104)', () => {
  const member = (name: string, path: string, size = 12) => ({
    name,
    path,
    isDir: false,
    size,
    modifiedMs: 1,
  });

  beforeEach(() => {
    readArchiveMock.mockReset();
    readPreviewMock.mockReset();
    previewStore.reset();
    ui.preview.maxTextBytes = 2_000_000;
  });

  it('splits archive member paths and ignores disk files', () => {
    expect(archiveMemberOf('C:\\d\\a.zip\\docs/readme.txt')).toEqual({
      archivePath: 'C:\\d\\a.zip',
      memberPath: 'docs/readme.txt',
    });
    expect(archiveMemberOf('C:\\d\\a.7z\\docs\\x.txt')).toEqual({
      archivePath: 'C:\\d\\a.7z',
      memberPath: 'docs/x.txt',
    });
    expect(archiveMemberOf('C:\\d\\a.zip')).toBeNull(); // the archive file itself
    expect(archiveMemberOf('C:\\d\\notes.txt')).toBeNull();
  });

  it('routes a selected member to readArchiveMemberPreview, never readPreview', async () => {
    readArchiveMock.mockResolvedValue(data('text', 'aGk='));
    previewStore.setTarget(member('readme.txt', 'C:\\d\\a.zip\\docs/readme.txt'));
    await previewStore.flush();
    expect(readArchiveMock).toHaveBeenCalledWith(
      'C:\\d\\a.zip',
      'docs/readme.txt',
      2_000_000,
      expect.stringMatching(/^pv-[0-9a-f-]{36}-\d+$/),
    );
    expect(cancelMock).not.toHaveBeenCalled(); // finished normally: nothing to cancel
    expect(readPreviewMock).not.toHaveBeenCalled();
    expect(previewStore.state).toBe('ready');
    expect(previewText(previewStore.data!.data)).toBe('hi');
  });

  it('cancels the backend read of the old archive member when the selection changes (FER-438)', async () => {
    let resolveFirst!: (d: PreviewData) => void;
    readArchiveMock.mockImplementationOnce(
      () => new Promise<PreviewData>((resolve) => (resolveFirst = resolve)),
    );
    previewStore.setTarget(member('a.txt', 'C:\\d\\a.zip\\a.txt'));
    const firstLoad = previewStore.flush();
    await tick();
    const firstId = readArchiveMock.mock.calls[0][3] as string;
    expect(firstId).toMatch(/^pv-[0-9a-f-]{36}-\d+$/);

    readArchiveMock.mockResolvedValueOnce(data('text', 'aGk='));
    previewStore.setTarget(member('b.txt', 'C:\\d\\a.zip\\b.txt'));
    expect(cancelMock).toHaveBeenCalledTimes(1);
    expect(cancelMock).toHaveBeenCalledWith(firstId);
    await previewStore.flush();
    const secondId = readArchiveMock.mock.calls[1][3] as string;
    expect(secondId).not.toBe(firstId);

    // The stale result is dropped and the old id is not cancelled twice.
    resolveFirst(data('text', 'b2xk'));
    await firstLoad;
    expect(previewText(previewStore.data!.data)).toBe('hi');
    expect(cancelMock).toHaveBeenCalledTimes(1);
  });

  it('uses request ids that cannot collide between two store instances', async () => {
    readArchiveMock.mockResolvedValue(data('text', 'aGk='));
    const other = new PreviewStore();
    previewStore.setTarget(member('a.txt', 'C:\\d\\a.zip\\a.txt'));
    await previewStore.flush();
    other.setTarget(member('a.txt', 'C:\\d\\a.zip\\a.txt'));
    await other.flush();
    const [first, second] = readArchiveMock.mock.calls.map((c) => c[3] as string);
    expect(first).not.toBe(second);
    // The random prefix (uuid) differs, whatever the counters are.
    expect(first.split('-').slice(0, -1).join('-')).not.toBe(
      second.split('-').slice(0, -1).join('-'),
    );
    other.reset();
  });

  it('cancels an in-flight archive read when the target is cleared', async () => {
    readArchiveMock.mockImplementationOnce(() => new Promise<PreviewData>(() => {}));
    previewStore.setTarget(member('a.txt', 'C:\\d\\a.zip\\a.txt'));
    void previewStore.flush();
    await tick();
    previewStore.setTarget(null);
    expect(cancelMock).toHaveBeenCalledTimes(1);
  });

  it('never cancels plain disk reads', async () => {
    readPreviewMock.mockResolvedValue(data('text', 'aGk='));
    previewStore.setTarget(member('n.txt', 'C:\\d\\n.txt'));
    await previewStore.flush();
    previewStore.setTarget(member('m.txt', 'C:\\d\\m.txt'));
    expect(cancelMock).not.toHaveBeenCalled();
  });

  it('does not read an oversize text member (metadata only)', async () => {
    previewStore.setTarget(member('big.txt', 'C:\\d\\a.zip\\big.txt', 5_000_000));
    await previewStore.flush();
    expect(readArchiveMock).not.toHaveBeenCalled();
    expect(previewStore.data?.tooLarge).toBe(true);
  });

  it('surfaces a safe backend error for an encrypted member', async () => {
    readArchiveMock.mockRejectedValue(new Error('Encrypted archive member cannot be previewed'));
    previewStore.setTarget(member('s.txt', 'C:\\d\\a.zip\\s.txt'));
    await previewStore.flush();
    expect(previewStore.state).toBe('error');
    expect(previewStore.errorText).toContain('Encrypted');
  });

  it('builds an inert data URL for image members and never a file URL for disk images', () => {
    const img = data('image', 'AAAA');
    expect(previewImageSrc('C:\\d\\a.zip\\pic.png', img)).toBe('data:image/png;base64,AAAA');
    expect(previewImageSrc('C:\\d\\a.zip\\v.svg', img)).toBe('data:image/svg+xml;base64,AAAA');
    expect(previewImageSrc('C:\\d\\a.zip\\x.xyz', img)).toBe('');
    expect(previewImageSrc('C:\\d\\p.png', img)).toBe('');
    expect(previewImageSrc('C:\\d\\a.zip\\n.txt', data('text'))).toBe('');
  });
});
