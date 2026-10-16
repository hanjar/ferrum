/**
 * FER-512: the selection save/load dialogs send `dir` (the pane folder) plus a bare file NAME —
 * never a path. The backend refuses anything with separators or traversal; here we pin the exact
 * arguments the dialogs hand to the IPC wrappers and what the UI shows.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render } from '@testing-library/svelte';
import { tick } from 'svelte';
import Dialogs from './Dialogs.svelte';
import { dialogs } from '../stores/dialogStore.svelte';
import { registerPane, type PaneApi } from '../stores/panesRegistry.svelte';
import { ui } from '../uiState.svelte';

const selMocks = vi.hoisted(() => ({
  saveSelectionFile: vi.fn(async () => undefined),
  loadSelectionFile: vi.fn(async () => ({ version: 1, path: 'C:\\Data', names: ['a.txt'] })),
}));
vi.mock('../ipc/selectionFile', () => selMocks);

vi.mock('../ipc/archive', () => ({
  archiveList: vi.fn(),
  archiveRead: vi.fn(),
  archiveExtract: vi.fn(),
  archiveExtractPreflight: vi.fn(),
  archiveTest: vi.fn(),
  archiveCreate: vi.fn(),
  archiveCreateZip: vi.fn(),
  archiveRarToolProbe: vi.fn(async () => ({ ok: false })),
  archiveRarToolDetect: vi.fn(async () => null),
}));

const unregisters: (() => void)[] = [];
const selected: string[] = [];

beforeEach(() => {
  selMocks.saveSelectionFile.mockClear();
  selMocks.loadSelectionFile.mockClear();
  selected.length = 0;
  ui.activePane = 0;
  const api: PaneApi = {
    path: 'C:\\Data',
    cursorEntry: () => undefined,
    refresh: () => {},
    newTab: () => {},
    closeCurrentTab: () => {},
    goUp: () => {},
    openCursorEntry: () => {},
    selectedPaths: () => [],
    visibleEntries: () => [
      { name: 'a.txt', path: 'C:\\Data\\a.txt', isDir: false, size: 1, modifiedMs: 0 },
      { name: 'b.txt', path: 'C:\\Data\\b.txt', isDir: false, size: 2, modifiedMs: 0 },
    ],
    selectPaths: (paths) => {
      selected.length = 0;
      selected.push(...paths);
    },
  };
  unregisters.push(registerPane(0, api));
});

afterEach(() => {
  dialogs.close();
  for (const off of unregisters.splice(0)) off();
});

describe('selection save dialog (FER-512 dir + name)', () => {
  it('sends the pane folder and the typed NAME, defaulting to selection.json', async () => {
    render(Dialogs);
    dialogs.open({ kind: 'saveSelection', parent: 'C:\\Data', names: ['a.txt', 'b.txt'] });
    await tick();
    // An empty field means the default NAME — the folder no longer becomes part of it.
    await fireEvent.click(document.querySelector('[role="dialog"] button.primary')!);
    await tick();
    expect(selMocks.saveSelectionFile).toHaveBeenCalledTimes(1);
    expect(selMocks.saveSelectionFile).toHaveBeenCalledWith('C:\\Data', 'selection.json', [
      'a.txt',
      'b.txt',
    ]);
    expect(dialogs.current).toBeNull();
  });

  it('sends a typed name verbatim; the backend rejects separators, not the dialog', async () => {
    render(Dialogs);
    dialogs.open({ kind: 'saveSelection', parent: 'C:\\Data', names: ['a.txt'] });
    await tick();
    const input = document.querySelector('[role="dialog"] input') as HTMLInputElement;
    await fireEvent.input(input, { target: { value: 'monday.json' } });
    await fireEvent.click(document.querySelector('[role="dialog"] button.primary')!);
    await tick();
    expect(selMocks.saveSelectionFile).toHaveBeenCalledWith('C:\\Data', 'monday.json', ['a.txt']);
  });
});

describe('selection load dialog (FER-512 dir + name)', () => {
  it('loads the named file from the pane folder and applies it to the pane', async () => {
    render(Dialogs);
    dialogs.open({ kind: 'loadSelection', parent: 'C:\\Data' });
    await tick();
    await fireEvent.click(document.querySelector('[role="dialog"] button.primary')!);
    await tick();
    expect(selMocks.loadSelectionFile).toHaveBeenCalledTimes(1);
    expect(selMocks.loadSelectionFile).toHaveBeenCalledWith('C:\\Data', 'selection.json');
    // The pane gets PATHS (the saved names resolved against the visible entries).
    expect(selected).toEqual(['C:\\Data\\a.txt']);
    expect(dialogs.current).toBeNull();
  });

  it('requires a name and shows where it would look', async () => {
    render(Dialogs);
    dialogs.open({ kind: 'loadSelection', parent: 'C:\\Data' });
    await tick();
    const modal = document.querySelector('[role="dialog"]')!;
    expect(modal.textContent).toContain('C:\\Data');
    await fireEvent.click(modal.querySelector('button.primary')!);
    await tick();
    // Empty input falls back to the same default NAME as Save (placeholder shows it).
    expect(selMocks.loadSelectionFile).toHaveBeenCalledWith('C:\\Data', 'selection.json');
    expect(dialogs.current).toBeNull();
  });
});
