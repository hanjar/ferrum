/**
 * FER-180 characterization: the manual Pack dialog (F2-12) before/after the
 * service extraction. The archive IPC is mocked at the boundary, so these
 * tests pin the exact call the dialog makes and its UI reactions.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render } from '@testing-library/svelte';
import { tick } from 'svelte';
import Dialogs from './Dialogs.svelte';
import { dialogs } from '../stores/dialogStore.svelte';
import { registerPane, type PaneApi } from '../stores/panesRegistry.svelte';
import { ui } from '../uiState.svelte';
import { FsError } from '../ipc/types';

const archiveMocks = vi.hoisted(() => ({
  archiveCreate: vi.fn(),
  archiveCancelRequest: vi.fn(),
  newArchiveRequestId: () => 'req-pack-dialog-1',
  listenArchiveProgress: vi.fn(async () => () => {}) as (cb: unknown) => Promise<() => void>,
}));
vi.mock('../ipc/archive', () => ({
  archiveList: vi.fn(),
  archiveRead: vi.fn(),
  archiveExtract: vi.fn(),
  archiveExtractPreflight: vi.fn(),
  archiveTest: vi.fn(),
  archiveCreate: archiveMocks.archiveCreate,
  archiveCreateZip: vi.fn(),
  archiveCancelRequest: (id: string) => archiveMocks.archiveCancelRequest(id),
  archiveRarToolProbe: vi.fn(async () => ({ ok: false })),
  archiveRarToolDetect: vi.fn(async () => null),
  newArchiveRequestId: () => archiveMocks.newArchiveRequestId(),
  listenArchiveProgress: (cb: (e: unknown) => void) => archiveMocks.listenArchiveProgress(cb),
}));

const unregisters: (() => void)[] = [];
let refresh = vi.fn();

function register(id: 0 | 1, path: string, selected: string[]): void {
  const api: PaneApi = {
    path,
    selectedPaths: () => selected,
    cursorEntry: () => undefined,
    refresh: () => refresh(),
    newTab: () => {},
    closeCurrentTab: () => {},
    goUp: () => {},
    openCursorEntry: () => {},
  };
  unregisters.push(registerPane(id, api));
}

const DIR = 'C:\\Data\\Invoice';
const FILE_A = 'C:\\Data\\Invoice\\a.txt';

beforeEach(() => {
  archiveMocks.archiveCreate.mockReset();
  archiveMocks.archiveCancelRequest.mockReset();
  refresh = vi.fn();
  ui.activePane = 0;
});

afterEach(() => {
  dialogs.close();
  for (const off of unregisters.splice(0)) off();
});

describe('beginPack (manual dialog opener)', () => {
  it('opens the pack dialog for the active selection with <folder>.zip as target', () => {
    register(0, DIR, [FILE_A, 'C:\\Data\\Invoice\\b.txt']);
    dialogs.beginPack('zip');
    expect(dialogs.current).toEqual({
      kind: 'pack',
      paths: [FILE_A, 'C:\\Data\\Invoice\\b.txt'],
      target: 'C:\\Data\\Invoice.zip',
      format: 'zip',
    });
  });

  it('7z uses the .7z extension', () => {
    register(0, DIR, [FILE_A]);
    dialogs.beginPack('sevenz');
    expect(dialogs.current).toMatchObject({ target: 'C:\\Data\\Invoice.7z', format: 'sevenz' });
  });

  it('a pane path that already ends in the extension is not doubled', () => {
    register(0, 'C:\\Data\\x.zip', ['C:\\Data\\x.zip\\a']);
    dialogs.beginPack('zip');
    expect(dialogs.current).toMatchObject({ target: 'C:\\Data\\x.zip' });
  });

  it('empty selection opens the Nothing-selected error and never a pack dialog', () => {
    register(0, DIR, []);
    dialogs.beginPack('zip');
    expect(dialogs.current).toEqual({ kind: 'error', title: 'Pack', message: 'Nothing selected.' });
  });

  it('opening the dialog is not an archive operation', () => {
    register(0, DIR, [FILE_A]);
    dialogs.beginPack('zip');
    expect(archiveMocks.archiveCreate).not.toHaveBeenCalled();
  });
});

describe('Pack dialog submit', () => {
  function open() {
    register(0, DIR, [FILE_A]);
    dialogs.beginPack('zip');
  }

  it('Pack button calls archiveCreate(paths, target, format, conflict, requestId), closes and refreshes the pane', async () => {
    archiveMocks.archiveCreate.mockResolvedValue('C:\\Data\\Invoice.zip');
    open();
    const { getByText } = render(Dialogs);
    await tick();
    await fireEvent.click(getByText('Pack', { selector: 'button' }));
    await vi.waitFor(() => expect(dialogs.current).toBeNull());
    expect(archiveMocks.archiveCreate).toHaveBeenCalledTimes(1);
    expect(archiveMocks.archiveCreate).toHaveBeenCalledWith(
      [FILE_A],
      'C:\\Data\\Invoice.zip',
      'zip',
      undefined,
      expect.any(String),
      'normal',
    );
    expect(refresh).toHaveBeenCalled();
  });

  it('Enter submits the same call', async () => {
    archiveMocks.archiveCreate.mockResolvedValue('x');
    open();
    render(Dialogs);
    await tick();
    await fireEvent.keyDown(document.querySelector('[role="dialog"]')!, { key: 'Enter' });
    await vi.waitFor(() => expect(archiveMocks.archiveCreate).toHaveBeenCalledTimes(1));
  });

  it('shows progress while packing, and Batal cancels the job', async () => {
    let cancelCalled = false;
    archiveMocks.archiveCreate.mockImplementation(() => {
      return new Promise((_, reject) => {
        archiveMocks.archiveCancelRequest.mockImplementation(() => {
          cancelCalled = true;
          reject(
            new FsError({ code: 'cancelled', message: 'Cancelled', archiveCode: 'cancelled' }),
          );
        });
      });
    });

    open();
    const { getByText, getByRole } = render(Dialogs);
    await tick();
    await fireEvent.click(getByText('Pack', { selector: 'button' }));
    await tick();

    // Now progress dialog is active:
    expect(dialogs.current).toMatchObject({ kind: 'archiveProgress', title: 'Pack' });
    const batalBtn = getByRole('button', { name: 'Batal' });
    expect(batalBtn).toBeTruthy();

    await fireEvent.click(batalBtn);
    await tick();
    expect(cancelCalled).toBe(true);
    await vi.waitFor(() => expect(dialogs.current).toBeNull());
    expect(refresh).not.toHaveBeenCalled();
  });

  it('Esc during packing cancels the running job', async () => {
    let cancelCalled = false;
    archiveMocks.archiveCreate.mockImplementation(() => {
      return new Promise((_, reject) => {
        archiveMocks.archiveCancelRequest.mockImplementation(() => {
          cancelCalled = true;
          reject(
            new FsError({ code: 'cancelled', message: 'Cancelled', archiveCode: 'cancelled' }),
          );
        });
      });
    });

    open();
    const { getByText, getByRole } = render(Dialogs);
    await tick();
    await fireEvent.click(getByText('Pack', { selector: 'button' }));
    await tick();

    expect(dialogs.current).toMatchObject({ kind: 'archiveProgress' });
    await fireEvent.keyDown(getByRole('dialog'), { key: 'Escape' });
    await tick();
    expect(cancelCalled).toBe(true);
    await vi.waitFor(() => expect(dialogs.current).toBeNull());
  });

  it('failure keeps the dialog open, shows the FsError message and does not refresh', async () => {
    archiveMocks.archiveCreate.mockRejectedValue(
      new FsError({ code: 'io', message: 'disk is full' }),
    );
    open();
    const { getByText, findByText } = render(Dialogs);
    await tick();
    await fireEvent.click(getByText('Pack', { selector: 'button' }));
    await findByText(/disk is full/);
    expect(dialogs.current).toMatchObject({ kind: 'pack' });
    expect(refresh).not.toHaveBeenCalled();
  });
});
