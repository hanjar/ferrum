import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { PaneApi } from './panesRegistry.svelte';

const verifyMock = vi.fn();
const cancelMock = vi.fn();
const createMock = vi.fn();
const splitMock = vi.fn();
const combineMock = vi.fn();
vi.mock('../ipc/fileTools', async (importOriginal) => {
  const original = await importOriginal<typeof import('../ipc/fileTools')>();
  return {
    ...original,
    verifyChecksums: (...a: unknown[]) => verifyMock(...a),
    cancelChecksumRequest: (...a: unknown[]) => cancelMock(...a),
    enqueueChecksumCreate: (...a: unknown[]) => createMock(...a),
    enqueueSplitFile: (...a: unknown[]) => splitMock(...a),
    enqueueCombineParts: (...a: unknown[]) => combineMock(...a),
  };
});

const { fileTools, openChecksumCreate, openChecksumVerify, openSplitFile, openCombineParts } =
  await import('./fileToolsStore.svelte');
const { registerPane } = await import('./panesRegistry.svelte');

function register(selection: string[]): void {
  const pane: PaneApi = {
    path: 'C:\\dir',
    selectedPaths: () => selection,
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

beforeEach(() => {
  verifyMock.mockReset();
  cancelMock.mockReset();
  createMock.mockReset();
  splitMock.mockReset();
  combineMock.mockReset();
});

afterEach(() => {
  fileTools.close();
});

describe('fileToolsStore', () => {
  it('opens checksum creation with an algorithm choice and enqueues the batch', async () => {
    register(['C:\\dir\\a.txt']);
    openChecksumCreate();
    expect(fileTools.current).toEqual({ kind: 'checksumCreate', paths: ['C:\\dir\\a.txt'] });
    expect(fileTools.algorithm).toBe('sha256'); // the only default
    fileTools.algorithm = 'crc32';
    createMock.mockResolvedValue('op_1');
    await fileTools.submit();
    expect(createMock).toHaveBeenCalledWith(['C:\\dir\\a.txt'], 'crc32', 'refuse');
    expect(fileTools.enqueued).toBe(true);
    expect(fileTools.error).toBe('');
  });

  it('runs verification immediately and reports per-item outcomes', async () => {
    verifyMock.mockResolvedValue({
      requested: 2,
      ok: 1,
      mismatched: 1,
      outcomes: [
        { path: 'C:\\dir\\a.txt', status: 'ok' },
        { path: 'C:\\dir\\bad.txt', status: 'mismatch', expected: 'aa', actual: 'bb' },
      ],
    });
    register(['C:\\dir\\a.txt', 'C:\\dir\\bad.txt']);
    openChecksumVerify();
    await vi.waitFor(() => expect(fileTools.verify?.mismatched).toBe(1));
    expect(verifyMock).toHaveBeenCalledWith(
      ['C:\\dir\\a.txt', 'C:\\dir\\bad.txt'],
      expect.stringMatching(/^verify-/),
    );
  });

  it('splits next to the source with a megabyte part size', async () => {
    register(['C:\\dir\\movie.iso']);
    openSplitFile();
    expect(fileTools.current).toEqual({ kind: 'split', path: 'C:\\dir\\movie.iso' });
    fileTools.partSizeMb = 100;
    splitMock.mockResolvedValue('op_2');
    await fileTools.submit();
    expect(splitMock).toHaveBeenCalledWith(
      'C:\\dir\\movie.iso',
      100 * 1024 * 1024,
      'C:\\dir\\',
      'refuse',
    );
    expect(fileTools.enqueued).toBe(true);
  });

  it('combines the first part into the reconstructed file', async () => {
    register(['C:\\dir\\movie.iso.001']);
    openCombineParts();
    expect(fileTools.current).toEqual({ kind: 'combine', firstPart: 'C:\\dir\\movie.iso.001' });
    combineMock.mockResolvedValue('op_3');
    await fileTools.submit();
    expect(combineMock).toHaveBeenCalledWith(
      'C:\\dir\\movie.iso.001',
      'C:\\dir\\movie.iso',
      'refuse',
    );
  });

  it('surfaces queue errors without closing the dialog', async () => {
    register(['C:\\dir\\a.txt']);
    openChecksumCreate();
    createMock.mockRejectedValue(new Error('disk full'));
    await fileTools.submit();
    expect(fileTools.error).toContain('disk full');
    expect(fileTools.enqueued).toBe(false);
    // A coded FsError shows its bare message, matching the other dialogs.
    createMock.mockRejectedValue(
      new (await import('../ipc/types')).FsError({ code: 'io', message: 'no space' }),
    );
    await fileTools.submit();
    expect(fileTools.error).toBe('no space');
  });

  it('passes the chosen conflict policy to every producer and resets it per open', async () => {
    register(['C:\\dir\\a.txt']);
    openChecksumCreate();
    expect(fileTools.conflictPolicy).toBe('refuse');
    fileTools.conflictPolicy = 'skip';
    createMock.mockResolvedValue('op_1');
    await fileTools.submit();
    expect(createMock).toHaveBeenLastCalledWith(['C:\\dir\\a.txt'], 'sha256', 'skip');

    openSplitFile();
    expect(fileTools.conflictPolicy).toBe('refuse'); // never carried over
    fileTools.conflictPolicy = 'overwriteOlder';
    splitMock.mockResolvedValue('op_2');
    await fileTools.submit();
    expect(splitMock).toHaveBeenLastCalledWith(
      'C:\\dir\\a.txt',
      1024 * 1024,
      'C:\\dir\\',
      'overwriteOlder',
    );

    register(['C:\\dir\\a.txt.001']);
    openCombineParts();
    fileTools.conflictPolicy = 'rename';
    combineMock.mockResolvedValue('op_3');
    await fileTools.submit();
    expect(combineMock).toHaveBeenLastCalledWith('C:\\dir\\a.txt.001', 'C:\\dir\\a.txt', 'rename');
  });

  it('cancels a running verification through its request id and keeps the partial report', async () => {
    let finish: (v: unknown) => void = () => {};
    verifyMock.mockImplementation(() => new Promise((resolve) => (finish = resolve)));
    cancelMock.mockResolvedValue(true);
    register(['C:\\dir\\big.iso']);
    openChecksumVerify();
    await vi.waitFor(() => expect(fileTools.verifying).toBe(true));
    const requestId = verifyMock.mock.calls[0][1] as string;
    await fileTools.cancelVerify();
    expect(cancelMock).toHaveBeenCalledWith(requestId);
    finish({ requested: 1, ok: 0, mismatched: 0, outcomes: [], cancelled: true });
    await vi.waitFor(() => expect(fileTools.verifying).toBe(false));
    expect(fileTools.verify?.cancelled).toBe(true);
  });

  it('ignores a cancel when no verification is running', async () => {
    register(['C:\\dir\\a.txt']);
    openChecksumCreate();
    await fileTools.cancelVerify();
    expect(cancelMock).not.toHaveBeenCalled();
  });

  it('refuses to open without a selection', () => {
    register([]);
    openChecksumCreate();
    openChecksumVerify();
    openSplitFile();
    openCombineParts();
    expect(fileTools.current).toBeNull();
  });
});

describe('isCombineCandidate', () => {
  it('accepts .NNN parts and .parts.json manifests only', async () => {
    const { isCombineCandidate } = await import('./fileToolsStore.svelte');
    expect(isCombineCandidate('C:\\x\\f.iso.001')).toBe(true);
    expect(isCombineCandidate('C:\\x\\f.iso.parts.json')).toBe(true);
    for (const no of ['C:\\x\\f.iso', 'C:\\x\\f.iso.1', 'C:\\x\\f.iso.0001', 'C:\\x\\f.iso.abc']) {
      expect(isCombineCandidate(no), no).toBe(false);
    }
  });
});

describe('F2-24 action registry integration', () => {
  it('enables the four file tools without shortcuts and guards combine availability', async () => {
    const { getAction, isEnabled, runAction } = await import('../actions');
    for (const id of ['file.createChecksum', 'file.verifyChecksum', 'file.split', 'file.combine']) {
      const action = getAction(id)!;
      expect(action.disabledReason, id).toBeUndefined();
      expect(action.shortcut, id).toBeUndefined(); // TC has no default bindings
      expect(action.run, id).toBeDefined();
    }
    // Checksum tools want a selection.
    register([]);
    expect(isEnabled(getAction('file.createChecksum')!)).toBe(false);
    expect(isEnabled(getAction('file.verifyChecksum')!)).toBe(false);
    // Split/combine want exactly one entry; combine only a real part shape.
    register(['C:\\d\\a.txt']);
    expect(isEnabled(getAction('file.split')!)).toBe(true);
    expect(isEnabled(getAction('file.combine')!)).toBe(false);
    register(['C:\\d\\f.iso.001']);
    expect(isEnabled(getAction('file.combine')!)).toBe(true);
    expect(runAction(getAction('file.combine')!)).toBe(true);
    expect(fileTools.current).toEqual({ kind: 'combine', firstPart: 'C:\\d\\f.iso.001' });
  });
});
