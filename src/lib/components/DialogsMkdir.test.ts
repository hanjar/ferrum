/**
 * FER-596: F7 "New folder" puts the cursor on the created folder (after the pane refresh), so
 * Enter opens it straight away. A failed create keeps the dialog open and moves no cursor.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render } from '@testing-library/svelte';
import { tick } from 'svelte';
import Dialogs from './Dialogs.svelte';
import { dialogs } from '../stores/dialogStore.svelte';
import { registerPane, type PaneApi } from '../stores/panesRegistry.svelte';
import { ui } from '../uiState.svelte';

const mutMocks = vi.hoisted(() => ({
  createEntry: vi.fn(),
}));
vi.mock('../ipc/mutations', () => mutMocks);

const calls: string[] = [];
const selectPaths = vi.fn((paths: Iterable<string>, cursorPath?: string) => {
  calls.push(`select:${[...paths].join(',')}|${cursorPath ?? ''}`);
});
const unregisters: (() => void)[] = [];

beforeEach(() => {
  calls.length = 0;
  selectPaths.mockClear();
  ui.activePane = 0;
  const api: PaneApi = {
    path: 'C:\\t',
    cursorEntry: () => undefined,
    refresh: () => {
      calls.push('refresh');
    },
    newTab: () => {},
    closeCurrentTab: () => {},
    goUp: () => {},
    openCursorEntry: () => {},
    selectedPaths: () => [],
    selectPaths,
  };
  unregisters.push(registerPane(0, api));
});

afterEach(() => {
  dialogs.close();
  mutMocks.createEntry.mockReset();
  for (const off of unregisters.splice(0)) off();
});

function typeName(value: string) {
  const input = document.querySelector('[role="dialog"] input') as HTMLInputElement;
  fireEvent.input(input, { target: { value } });
}

function submit() {
  fireEvent.click(document.querySelector('[role="dialog"] button.primary') as HTMLButtonElement);
}

describe('FER-596: F7 New folder puts the cursor on the new folder', () => {
  it('creates the folder, refreshes, then sets the cursor on it with no marks', async () => {
    mutMocks.createEntry.mockResolvedValue({ name: 'baru', path: 'C:\\t\\baru', isDir: true });
    render(Dialogs);
    dialogs.open({ kind: 'mkdir', parent: 'C:\\t' });
    await tick();
    typeName('baru');
    await tick();
    submit();
    await vi.waitFor(() => expect(dialogs.current).toBeNull());
    expect(mutMocks.createEntry).toHaveBeenCalledWith('dir', 'C:\\t', 'baru');
    expect(calls).toEqual(['refresh', 'select:|C:\\t\\baru']);
  });

  it('a failed create keeps the dialog open and moves no cursor', async () => {
    mutMocks.createEntry.mockRejectedValue(new Error('AlreadyExists'));
    render(Dialogs);
    dialogs.open({ kind: 'mkdir', parent: 'C:\\t' });
    await tick();
    typeName('ada');
    await tick();
    submit();
    await vi.waitFor(() => {
      expect(document.querySelector('[role="dialog"] .error')?.textContent).toContain(
        'AlreadyExists',
      );
    });
    expect(dialogs.current).toMatchObject({ kind: 'mkdir', parent: 'C:\\t' });
    expect(selectPaths).not.toHaveBeenCalled();
  });
});
