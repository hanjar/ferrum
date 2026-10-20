import { beforeEach, describe, expect, it, vi } from 'vitest';

const ipc = vi.hoisted(() => ({
  clipboardCopyFiles: vi.fn(),
  clipboardReadFiles: vi.fn(),
  clipboardFinishCut: vi.fn(),
}));
vi.mock('../ipc/clipboardFiles', () => ipc);

import { actionForEvent, runAction } from '../actions';
import { dialogs } from './dialogStore.svelte';
import { registerPane, type PaneApi } from './panesRegistry.svelte';
import { ui } from '../uiState.svelte';
import { toast } from './toastStore.svelte';
import { FsError } from '../ipc/types';

function pane(path: string, selected: string[]): PaneApi {
  return {
    path,
    selectedPaths: () => selected,
    cursorEntry: () => null,
    refresh: () => {},
    newTab: () => {},
    closeCurrentTab: () => {},
    goUp: () => {},
    openCursorEntry: () => {},
  } as unknown as PaneApi;
}

const key = (k: string) => ({ key: k, ctrlKey: true, metaKey: false, shiftKey: false });

beforeEach(() => {
  vi.clearAllMocks();
  registerPane(0, pane('C:\\src', ['C:\\src\\a.txt', 'C:\\src\\b']));
  registerPane(1, pane('D:\\other', ['D:\\other\\z']));
  ui.activePane = 0;
  dialogs.close();
});

describe('FER-572 file clipboard shortcuts', () => {
  it('Ctrl+C / Ctrl+X / Ctrl+V resolve to the clipboard actions', () => {
    expect(actionForEvent(key('c'))?.id).toBe('file.clipCopy');
    expect(actionForEvent(key('x'))?.id).toBe('file.clipCut');
    expect(actionForEvent(key('v'))?.id).toBe('file.clipPaste');
  });

  it('Ctrl+C sends the ACTIVE pane selection as a copy', async () => {
    ipc.clipboardCopyFiles.mockResolvedValue(7);
    runAction(actionForEvent(key('c'))!);
    await vi.waitFor(() => expect(ipc.clipboardCopyFiles).toHaveBeenCalled());
    expect(ipc.clipboardCopyFiles).toHaveBeenCalledWith(['C:\\src\\a.txt', 'C:\\src\\b'], false);
  });

  it('Ctrl+X sends the selection as a cut, from the other pane when it is active', async () => {
    ipc.clipboardCopyFiles.mockResolvedValue(7);
    ui.activePane = 1;
    runAction(actionForEvent(key('x'))!);
    await vi.waitFor(() => expect(ipc.clipboardCopyFiles).toHaveBeenCalled());
    expect(ipc.clipboardCopyFiles).toHaveBeenCalledWith(['D:\\other\\z'], true);
  });

  it('a backend refusal opens an error dialog', async () => {
    ipc.clipboardCopyFiles.mockRejectedValue(
      new FsError({ code: 'invalidPath', message: 'refused' }),
    );
    runAction(actionForEvent(key('c'))!);
    await vi.waitFor(() => expect(dialogs.current?.kind).toBe('error'));
  });

  it('Ctrl+C is not taken from highlighted text', () => {
    const sel = vi.spyOn(globalThis, 'getSelection').mockReturnValue({
      toString: () => 'some text',
    } as unknown as Selection);
    expect(actionForEvent(key('c'))).toBeUndefined();
    sel.mockRestore();
  });
});

describe('FER-572 paste', () => {
  it('a copy paste opens the copy flow for the active pane folder', async () => {
    ipc.clipboardReadFiles.mockResolvedValue({
      paths: ['E:\\x\\f.txt'],
      cut: false,
      sequence: 3,
    });
    const spy = vi.spyOn(dialogs, 'beginTransferTo').mockResolvedValue();
    ui.activePane = 1;
    runAction(actionForEvent(key('v'))!);
    await vi.waitFor(() => expect(spy).toHaveBeenCalled());
    expect(spy).toHaveBeenCalledWith('copy', ['E:\\x\\f.txt'], 'D:\\other', undefined);
    spy.mockRestore();
  });

  it('a cut paste becomes a move that carries the clipboard sequence', async () => {
    ipc.clipboardReadFiles.mockResolvedValue({
      paths: ['E:\\x\\f.txt'],
      cut: true,
      sequence: 9,
    });
    const spy = vi.spyOn(dialogs, 'beginTransferTo').mockResolvedValue();
    runAction(actionForEvent(key('v'))!);
    await vi.waitFor(() => expect(spy).toHaveBeenCalled());
    expect(spy).toHaveBeenCalledWith('move', ['E:\\x\\f.txt'], 'C:\\src', {
      clipboardSequence: 9,
    });
    spy.mockRestore();
  });

  it('an empty clipboard only shows a toast', async () => {
    ipc.clipboardReadFiles.mockResolvedValue(null);
    const spy = vi.spyOn(dialogs, 'beginTransferTo').mockResolvedValue();
    const show = vi.spyOn(toast, 'show');
    runAction(actionForEvent(key('v'))!);
    await vi.waitFor(() => expect(show).toHaveBeenCalled());
    expect(spy).not.toHaveBeenCalled();
    spy.mockRestore();
  });

  it('a rejected clipboard list opens an error dialog and starts nothing', async () => {
    ipc.clipboardReadFiles.mockRejectedValue(
      new FsError({ code: 'invalidPath', message: 'Path contains a reserved Windows device name' }),
    );
    runAction(actionForEvent(key('v'))!);
    await vi.waitFor(() => expect(dialogs.current?.kind).toBe('error'));
  });
});
