/**
 * F2-18 — registry actions and panel component: Compare Directories
 * (Shift+F2), Compare By Content target selection, Mark Newer/Hide Same,
 * and Hide Same filtering inside FilePane.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, render } from '@testing-library/svelte';
import { tick } from 'svelte';
import ComparePanel from './ComparePanel.svelte';
import FilePane from './FilePane.svelte';
import { actionForEvent, getAction, isEnabled, labelOf, runAction } from '../actions';
import { compareStore } from '../stores/compareStore.svelte';
import { registerPane, type PaneApi, type PaneEntryInfo } from '../stores/panesRegistry.svelte';
import type { CompareDirsReport } from '../ipc/compare';

vi.mock('../ipc/compare', () => ({
  validateSyncRoots: vi.fn(async () => {}),
  compareDirs: (...args: unknown[]) => compareDirsMock(...args),
  compareFiles: (...args: unknown[]) => compareFilesMock(...args),
}));

const compareDirsMock = vi.fn();
const compareFilesMock = vi.fn();

function makePane(path: string, names: string[], selected: string[] = [], cursor?: string) {
  const state = { selection: [...selected], cursorName: cursor };
  const entries: PaneEntryInfo[] = names.map((n) => ({
    name: n,
    path: `${path}\\${n}`,
    isDir: false,
    size: 1,
    modifiedMs: 1,
  }));
  const api: PaneApi & { selection: string[] } = {
    path,
    selection: state.selection,
    selectedPaths: () => state.selection,
    cursorEntry: () => {
      const name = state.cursorName;
      const e = entries.find((x) => x.name === name);
      return e ? { name: e.name, path: e.path, isDir: false } : undefined;
    },
    refresh: () => {},
    newTab: () => {},
    closeCurrentTab: () => {},
    goUp: () => {},
    openCursorEntry: () => {},
    visibleEntries: () => entries,
    selectPaths: (paths) => {
      state.selection = [...paths];
    },
  };
  return api;
}

function report(): CompareDirsReport {
  return {
    leftRoot: 'C:\\L',
    rightRoot: 'D:\\R',
    truncated: false,
    entries: [
      {
        name: 'same.txt',
        state: 'same',
        leftSize: 1,
        leftModifiedMs: 1,
        rightSize: 1,
        rightModifiedMs: 1,
      },
      {
        name: 'newer-left.txt',
        state: 'newerLeft',
        leftSize: 2,
        leftModifiedMs: 2,
        rightSize: 1,
        rightModifiedMs: 1,
      },
      {
        name: 'left-only.txt',
        state: 'leftOnly',
        leftSize: 3,
        leftModifiedMs: 1,
        rightSize: 0,
        rightModifiedMs: 0,
      },
      {
        name: 'right-only.txt',
        state: 'rightOnly',
        leftSize: 0,
        leftModifiedMs: 0,
        rightSize: 3,
        rightModifiedMs: 1,
      },
    ],
  };
}

beforeEach(() => {
  compareDirsMock.mockReset();
  compareFilesMock.mockReset();
  compareStore.reset();
  registerPane(0, makePane('C:\\L', ['same.txt', 'newer-left.txt', 'left-only.txt']));
  registerPane(1, makePane('D:\\R', ['same.txt', 'right-only.txt']));
});

afterEach(() => {
  cleanup();
  compareStore.reset();
});

describe('compare registry actions', () => {
  it('Compare Directories runs on Shift+F2 against the two pane roots', async () => {
    const action = getAction('mark.compareDirectories')!;
    expect(action.shortcut).toEqual({ key: 'F2', shift: true });
    expect(labelOf(action)).toBe('Compare Directories');
    compareDirsMock.mockResolvedValue(report());
    expect(runAction(action)).toBe(true);
    await vi.waitFor(() => expect(compareStore.report).not.toBeNull());
    expect(compareDirsMock).toHaveBeenCalledWith('C:\\L', 'D:\\R', false, {
      recursive: false,
      opId: expect.any(String),
    });
    // exact modifiers: plain F2 stays rename (with a rename-eligible pane)
    registerPane(0, makePane('C:\\L', ['same.txt'], ['C:\\L\\same.txt'], 'same.txt'));
    expect(actionForEvent({ key: 'F2', ctrlKey: false, metaKey: false, shiftKey: false })?.id).toBe(
      'file.rename',
    );
  });

  it('Mark Newer selects per-side work and Hide Same hides equal names from the panes', async () => {
    compareDirsMock.mockResolvedValue(report());
    await compareStore.compareDirectories(false);
    const newer = getAction('mark.newerFiles')!;
    expect(isEnabled(newer)).toBe(true);
    expect(runAction(newer)).toBe(true);
    expect(compareStore.hiddenSameNames).toEqual(new Set(['same.txt']));

    // FilePane honors the Hide Same filter: the mock listing contains
    // README.md on the left pane; hiding it by name must remove the row.
    const left = render(FilePane, { paneId: 0 });
    await vi.waitFor(() =>
      expect(left.container.querySelectorAll('.row[data-path]').length).toBeGreaterThan(0),
    );
    compareStore.hiddenSameNames = new Set(['README.md']);
    await tick();
    let paths = [...left.container.querySelectorAll('.row[data-path]')].map((el) =>
      el.getAttribute('data-path'),
    );
    expect(paths).not.toContain('C:\\Projects\\README.md');
    compareStore.clearHiddenSame();
    await tick();
    paths = [...left.container.querySelectorAll('.row[data-path]')].map((el) =>
      el.getAttribute('data-path'),
    );
    expect(paths).toContain('C:\\Projects\\README.md');
  });

  it('Compare By Content picks two selected files first', async () => {
    registerPane(0, makePane('C:\\L', ['a.txt', 'b.txt'], ['C:\\L\\a.txt', 'C:\\L\\b.txt']));
    compareFilesMock.mockResolvedValue({
      equal: true,
      leftSize: 5,
      rightSize: 5,
      bothText: true,
    });
    const action = getAction('file.compareContent')!;
    expect(isEnabled(action)).toBe(true);
    expect(runAction(action)).toBe(true);
    await vi.waitFor(() => expect(compareStore.fileResult).not.toBeNull());
    expect(compareFilesMock).toHaveBeenCalledWith('C:\\L\\a.txt', 'C:\\L\\b.txt', {
      opId: expect.any(String),
      encoding: 'auto',
    });
  });

  it('Compare By Content falls back to one file per pane', async () => {
    registerPane(0, makePane('C:\\L', ['a.txt'], ['C:\\L\\a.txt']));
    registerPane(1, makePane('D:\\R', ['b.txt'], ['D:\\R\\b.txt']));
    compareFilesMock.mockResolvedValue({
      equal: false,
      leftSize: 1,
      rightSize: 2,
      bothText: false,
    });
    runAction(getAction('file.compareContent')!);
    await vi.waitFor(() => expect(compareStore.fileResult).not.toBeNull());
    expect(compareFilesMock).toHaveBeenCalledWith('C:\\L\\a.txt', 'D:\\R\\b.txt', {
      opId: expect.any(String),
      encoding: 'auto',
    });
  });

  it('Compare By Content is unavailable without a comparable pair', () => {
    registerPane(0, makePane('C:\\L', ['a.txt'], ['C:\\L\\a.txt']));
    registerPane(1, makePane('D:\\R', ['b.txt'], []));
    expect(isEnabled(getAction('file.compareContent')!)).toBe(false);
    expect(runAction(getAction('file.compareContent')!)).toBe(false);
  });

  it('show.allFiles clears the Hide Same filter too', async () => {
    compareDirsMock.mockResolvedValue(report());
    await compareStore.compareDirectories(false);
    compareStore.markNewerHideSame();
    expect(compareStore.hiddenSameNames).not.toBeNull();
    runAction(getAction('show.allFiles')!);
    expect(compareStore.hiddenSameNames).toBeNull();
  });
});

describe('ComparePanel component', () => {
  it('renders the report with state labels and the content verdict', async () => {
    compareDirsMock.mockResolvedValue(report());
    await compareStore.compareDirectories(false);
    compareStore.fileResult = {
      equal: false,
      firstMismatch: 150_000,
      leftSize: 200_000,
      rightSize: 200_000,
      leftLine: 42,
      rightLine: 42,
      bothText: true,
      leftPath: 'C:\\L\\a.txt',
      rightPath: 'D:\\R\\b.txt',
    };
    const r = render(ComparePanel);
    await tick();
    const text = r.container.textContent ?? '';
    expect(text).toContain('Compare By Content');
    expect(text).toContain('Files are different at byte 150000 (line 42 vs line 42)');
    expect(text).toContain('newer left');
    expect(text).toContain('left only');
    expect(text).toContain('same.txt');
    expect(text).toContain('Hide Same Files');
  });

  it('shows busy and error states instead of pretending', async () => {
    compareStore.panelOpen = true;
    compareStore.busy = true;
    const r = render(ComparePanel);
    await tick();
    expect(r.container.textContent).toContain('Comparing');
    compareStore.busy = false;
    compareStore.errorText = 'Access is denied.';
    await tick();
    expect(r.container.textContent).toContain('Access is denied.');
  });

  it('closes via its close button', async () => {
    compareStore.panelOpen = true;
    const r = render(ComparePanel);
    await tick();
    (r.container.querySelector('button.close') as HTMLElement).click();
    expect(compareStore.panelOpen).toBe(false);
  });
});
