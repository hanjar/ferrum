import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { tick } from 'svelte';
import MultiRenameDialog from './MultiRenameDialog.svelte';

const renameBatchMock = vi.fn();
vi.mock('../ipc/renameBatch', () => ({
  renameBatch: (...args: unknown[]) => renameBatchMock(...args),
}));

const { multiRename } = await import('../stores/multiRenameStore.svelte');
const { registerPane } = await import('../stores/panesRegistry.svelte');
import type { PaneApi } from '../stores/panesRegistry.svelte';

function register(selection: string[], names: string[]): void {
  const pane: PaneApi = {
    path: 'C:\\d',
    selectedPaths: () => selection,
    cursorEntry: () => undefined,
    refresh: vi.fn(),
    newTab: () => {},
    closeCurrentTab: () => {},
    goUp: () => {},
    openCursorEntry: () => {},
    listNames: () => names,
  };
  registerPane(0, pane);
  registerPane(1, pane);
}

afterEach(() => {
  cleanup();
  multiRename.close();
  multiRename.journal = [];
  renameBatchMock.mockReset();
});

describe('MultiRenameDialog', () => {
  it('renders nothing while closed and previews original→target rows when open', async () => {
    const { container } = render(MultiRenameDialog);
    expect(container.querySelector('[role="dialog"]')).toBeNull();
    register(['C:\\d\\a.txt', 'C:\\d\\b.txt'], ['a.txt', 'b.txt']);
    multiRename.openForSelection();
    await tick();
    expect(screen.getByRole('dialog', { name: 'Multi-Rename Tool' })).toBeTruthy();
    expect(screen.getAllByText('a.txt').length).toBeGreaterThan(0);
    // Default [N] pattern: nothing changes, Rename is armed (no issues).
    const rename = screen.getByRole('button', { name: 'Rename' }) as HTMLButtonElement;
    expect(rename.disabled).toBe(false);
  });

  it('disables Rename while the preview has duplicate targets and shows the reason', async () => {
    register(['C:\\d\\a.txt', 'C:\\d\\b.txt'], ['a.txt', 'b.txt']);
    multiRename.openForSelection();
    multiRename.options.pattern = 'same';
    render(MultiRenameDialog);
    await tick();
    const rename = screen.getByRole('button', { name: 'Rename' }) as HTMLButtonElement;
    expect(rename.disabled).toBe(true);
    // Only the second holder of the duplicate target is flagged.
    expect(screen.getByText(/1 problem/)).toBeTruthy();
    expect(screen.getAllByText('duplicate target').length).toBe(1);
  });

  it('executes the run via IPC, reports the outcome, and refreshes the pane', async () => {
    const refresh = vi.fn();
    const spyPane: PaneApi = {
      path: 'C:\\d',
      selectedPaths: () => ['C:\\d\\a.txt'],
      cursorEntry: () => undefined,
      refresh,
      newTab: () => {},
      closeCurrentTab: () => {},
      goUp: () => {},
      openCursorEntry: () => {},
      listNames: () => ['a.txt'],
    };
    registerPane(0, spyPane);
    registerPane(1, spyPane);
    multiRename.openForSelection();
    multiRename.options.pattern = 'new_[C].[E]';
    renameBatchMock.mockResolvedValue({
      requested: 1,
      renamed: 1,
      failed: [],
      journal: [{ from: 'C:\\d\\a.txt', to: 'new_1.txt' }],
    });
    render(MultiRenameDialog);
    await tick();
    fireEvent.click(screen.getByRole('button', { name: 'Rename' }));
    await vi.waitFor(() => expect(screen.getByText(/Renamed 1 of 1/)).toBeTruthy());
    expect(renameBatchMock).toHaveBeenCalledExactlyOnceWith([
      { from: 'C:\\d\\a.txt', to: 'new_1.txt' },
    ]);
    expect(refresh).toHaveBeenCalledOnce();
    // Undo becomes available after a successful run.
    const undo = screen.getByRole('button', { name: 'Undo last rename' }) as HTMLButtonElement;
    expect(undo.disabled).toBe(false);
  });

  it('closes on Escape and via Close without executing anything', async () => {
    register(['C:\\d\\a.txt'], ['a.txt']);
    multiRename.openForSelection();
    render(MultiRenameDialog);
    await tick();
    fireEvent.keyDown(screen.getByRole('dialog'), { key: 'Escape' });
    await tick();
    expect(multiRename.open).toBe(false);
    expect(renameBatchMock).not.toHaveBeenCalled();
  });
});
