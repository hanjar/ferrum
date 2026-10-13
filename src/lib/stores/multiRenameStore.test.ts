import { afterEach, describe, expect, it, vi } from 'vitest';
import type { PaneApi } from './panesRegistry.svelte';

const renameBatchMock = vi.fn();
vi.mock('../ipc/renameBatch', () => ({
  renameBatch: (...args: unknown[]) => renameBatchMock(...args),
}));

const { multiRename } = await import('./multiRenameStore.svelte');
const { registerPane } = await import('./panesRegistry.svelte');
const { FsError } = await import('../ipc/types');

function paneWith(selection: string[], names: string[]): PaneApi {
  return {
    path: 'C:\\dir',
    selectedPaths: () => selection,
    cursorEntry: () => undefined,
    refresh: vi.fn(),
    newTab: () => {},
    closeCurrentTab: () => {},
    goUp: () => {},
    openCursorEntry: () => {},
    listNames: () => names,
  } as PaneApi;
}

afterEach(() => {
  multiRename.close();
  multiRename.journal = [];
  renameBatchMock.mockReset();
});

describe('multiRenameStore', () => {
  it('opens from the active pane selection with an outside-selection collision pool', () => {
    registerPane(
      0,
      paneWith(['C:\\dir\\a.txt', 'C:\\dir\\b.txt'], ['a.txt', 'b.txt', 'other.txt']),
    );
    multiRename.openForSelection();
    expect(multiRename.open).toBe(true);
    expect(multiRename.paths).toEqual(['C:\\dir\\a.txt', 'C:\\dir\\b.txt']);
    expect(multiRename.directoryNames).toEqual(['other.txt']);
    expect(multiRename.canRun()).toBe(true); // [N] keeps every name: no issues
  });

  it('does not open without a selection and never offers run then', () => {
    registerPane(0, paneWith([], []));
    multiRename.openForSelection();
    expect(multiRename.open).toBe(false);
    expect(multiRename.canRun()).toBe(false);
  });

  it('locks the run while duplicate targets exist and unlocks after the fix', () => {
    registerPane(0, paneWith(['C:\\d\\a.txt', 'C:\\d\\b.txt'], ['a.txt', 'b.txt']));
    multiRename.openForSelection();
    multiRename.options.pattern = 'same';
    expect(multiRename.preview().map((i) => i.issue)).toEqual([undefined, 'duplicate']);
    expect(multiRename.canRun()).toBe(false);
    multiRename.options.pattern = 'item_[C]';
    expect(multiRename.canRun()).toBe(true);
  });

  it('runs the executable pairs, journals them, and refreshes the pane', async () => {
    const pane = paneWith(['C:\\d\\a.txt', 'C:\\d\\b.txt'], ['a.txt', 'b.txt']);
    registerPane(0, pane);
    multiRename.openForSelection();
    multiRename.paths = ['C:\\d\\a.txt', 'C:\\d\\gone.txt'];
    multiRename.options.pattern = 'renamed_[C].[E]';
    renameBatchMock.mockResolvedValue({
      requested: 2,
      renamed: 1,
      failed: [{ from: 'C:\\d\\gone.txt', to: 'renamed_2.txt', code: 'notFound', message: 'gone' }],
      journal: [{ from: 'C:\\d\\a.txt', to: 'renamed_1.txt' }],
    });
    await multiRename.run();
    expect(renameBatchMock).toHaveBeenCalledWith([
      { from: 'C:\\d\\a.txt', to: 'renamed_1.txt' },
      { from: 'C:\\d\\gone.txt', to: 'renamed_2.txt' },
    ]);
    expect(multiRename.journal).toEqual([{ from: 'C:\\d\\a.txt', to: 'renamed_1.txt' }]);
    expect(multiRename.lastReport).toEqual({
      requested: 2,
      renamed: 1,
      failures: [{ from: 'C:\\d\\gone.txt', to: 'renamed_2.txt', message: 'gone' }],
    });
    expect(pane.refresh).toHaveBeenCalledOnce();
  });

  it('undo replays the journal reversed and drops only restored entries', async () => {
    registerPane(0, paneWith(['C:\\d\\a.txt'], ['a.txt']));
    multiRename.openForSelection();
    multiRename.journal = [
      { from: 'C:\\d\\a.txt', to: 'x.txt' },
      { from: 'C:\\d\\b.txt', to: 'y.txt' },
    ];
    renameBatchMock.mockResolvedValue({
      requested: 2,
      renamed: 1,
      failed: [
        { from: 'C:\\d\\x.txt', to: 'a.txt', code: 'notFound', message: 'x moved elsewhere' },
      ],
      journal: [{ from: 'C:\\d\\y.txt', to: 'b.txt' }],
    });
    await multiRename.undo();
    expect(renameBatchMock).toHaveBeenCalledWith([
      { from: 'C:\\d\\y.txt', to: 'b.txt' },
      { from: 'C:\\d\\x.txt', to: 'a.txt' },
    ]);
    expect(multiRename.journal).toEqual([{ from: 'C:\\d\\a.txt', to: 'x.txt' }]);
  });

  it('surfaces IPC failures as dialog error text without touching the journal', async () => {
    registerPane(0, paneWith(['C:\\d\\a.txt'], ['a.txt']));
    multiRename.openForSelection();
    multiRename.options.pattern = 'new_[C]';
    renameBatchMock.mockRejectedValue(new FsError({ code: 'unsupported', message: 'nope' }));
    await multiRename.run();
    expect(multiRename.error).toBe('nope');
    expect(multiRename.journal).toEqual([]);
  });
});
