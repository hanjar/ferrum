/**
 * F2-18 — compare store and panel: report handling, Mark Newer selection
 * per side, Hide Same filtering, and compare-by-content verdicts. The IPC
 * layer is mocked; panes are fakes through the registry.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { compareStore, isNewerOnSide } from './compareStore.svelte';
import { registerPane, type PaneApi, type PaneEntryInfo } from './panesRegistry.svelte';
import type { CompareDirsReport, CompareResult, DirDiffEntry } from '../ipc/compare';

vi.mock('../ipc/compare', () => ({
  validateSyncRoots: vi.fn(async () => {}),
  compareDirs: (...args: unknown[]) => compareDirsMock(...args),
  compareFiles: (...args: unknown[]) => compareFilesMock(...args),
}));

const compareDirsMock = vi.fn();
const compareFilesMock = vi.fn();

function paneApi(path: string, names: string[]): PaneApi & { selection: string[] } {
  const state = { selection: [] as string[] };
  const entries: PaneEntryInfo[] = names.map((n) => ({
    name: n,
    path: `${path}\\${n}`,
    isDir: false,
    size: 1,
    modifiedMs: 1,
  }));
  return {
    path,
    selection: state.selection,
    selectedPaths: () => state.selection,
    cursorEntry: () => undefined,
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
}

function diffEntry(name: string, state: DirDiffEntry['state']): DirDiffEntry {
  return { name, state, leftSize: 1, leftModifiedMs: 1, rightSize: 1, rightModifiedMs: 1 };
}

function report(entries: DirDiffEntry[]): CompareDirsReport {
  return { leftRoot: 'C:\\L', rightRoot: 'D:\\R', entries, truncated: false };
}

let left: ReturnType<typeof paneApi>;
let right: ReturnType<typeof paneApi>;

beforeEach(() => {
  compareDirsMock.mockReset();
  compareFilesMock.mockReset();
  compareStore.reset();
  left = paneApi('C:\\L', ['same.txt', 'newer-left.txt', 'left-only.txt', 'size.txt']);
  right = paneApi('D:\\R', ['same.txt', 'newer-left.txt', 'right-only.txt', 'size.txt']);
  registerPane(0, left);
  registerPane(1, right);
});

afterEach(() => {
  compareStore.reset();
});

describe('isNewerOnSide', () => {
  it('treats newer and only-on-side as work for that side', () => {
    expect(isNewerOnSide('newerLeft', 'left')).toBe(true);
    expect(isNewerOnSide('leftOnly', 'left')).toBe(true);
    expect(isNewerOnSide('rightOnly', 'left')).toBe(false);
    expect(isNewerOnSide('same', 'left')).toBe(false);
    expect(isNewerOnSide('newerRight', 'right')).toBe(true);
    expect(isNewerOnSide('sizeDifferent', 'right')).toBe(false);
  });
});

describe('compareDirectories', () => {
  it('fetches the report from the two pane roots and opens the panel', async () => {
    compareDirsMock.mockResolvedValue(
      report([
        diffEntry('same.txt', 'same'),
        diffEntry('newer-left.txt', 'newerLeft'),
        diffEntry('left-only.txt', 'leftOnly'),
        diffEntry('right-only.txt', 'rightOnly'),
      ]),
    );
    await compareStore.compareDirectories(false);
    expect(compareDirsMock).toHaveBeenCalledWith('C:\\L', 'D:\\R', false, {
      recursive: false,
      opId: expect.any(String),
    });
    expect(compareStore.report?.entries).toHaveLength(4);
    expect(compareStore.panelOpen).toBe(true);
    expect(compareStore.busy).toBe(false);
  });

  it('surfaces errors honestly and keeps the panel for the message', async () => {
    compareDirsMock.mockRejectedValue(new Error('Access is denied.'));
    await compareStore.compareDirectories(true);
    expect(compareStore.errorText).toBe('Access is denied.');
    expect(compareStore.report).toBeNull();
    expect(compareStore.byContent).toBe(true);
  });

  it('explains when a pane is missing', async () => {
    registerPane(1, right)(); // unregister right
    await compareStore.compareDirectories();
    expect(compareDirsMock).not.toHaveBeenCalled();
    expect(compareStore.errorText).toMatch(/both panes/i);
  });
});

describe('markNewerHideSame', () => {
  it('selects per-side work and hides same names', async () => {
    compareDirsMock.mockResolvedValue(
      report([
        diffEntry('same.txt', 'same'),
        diffEntry('newer-left.txt', 'newerLeft'),
        diffEntry('left-only.txt', 'leftOnly'),
        diffEntry('right-only.txt', 'rightOnly'),
      ]),
    );
    await compareStore.compareDirectories(false);
    compareStore.markNewerHideSame();
    expect(left.selectedPaths().sort()).toEqual(
      ['C:\\L\\left-only.txt', 'C:\\L\\newer-left.txt'].sort(),
    );
    expect(right.selectedPaths()).toEqual(['D:\\R\\right-only.txt']);
    expect(compareStore.hiddenSameNames).toEqual(new Set(['same.txt']));
  });

  it('without a report explains to run Compare Directories first', () => {
    compareStore.markNewerHideSame();
    expect(compareStore.errorText).toMatch(/run compare directories first/i);
  });
});

describe('hide same and show all interplay', () => {
  it('toggleHideSame flips between hidden and visible', async () => {
    compareDirsMock.mockResolvedValue(
      report([diffEntry('a.txt', 'same'), diffEntry('b.txt', 'newerLeft')]),
    );
    await compareStore.compareDirectories(false);
    compareStore.toggleHideSame();
    expect(compareStore.hiddenSameNames).toEqual(new Set(['a.txt']));
    compareStore.toggleHideSame();
    expect(compareStore.hiddenSameNames).toBeNull();
  });
});

describe('compareContents', () => {
  it('stores the verdict with both paths', async () => {
    const result: CompareResult = {
      equal: false,
      firstMismatch: 150_000,
      leftSize: 200_000,
      rightSize: 200_000,
      leftLine: 42,
      rightLine: 42,
      bothText: true,
    };
    compareFilesMock.mockResolvedValue(result);
    await compareStore.compareContents('C:\\a.txt', 'D:\\b.txt');
    expect(compareFilesMock).toHaveBeenCalledWith('C:\\a.txt', 'D:\\b.txt', {
      opId: expect.any(String),
      encoding: 'auto',
    });
    expect(compareStore.fileResult?.equal).toBe(false);
    expect(compareStore.fileResult?.leftLine).toBe(42);
    expect(compareStore.panelOpen).toBe(true);
  });

  it('records compare errors without throwing', async () => {
    compareFilesMock.mockRejectedValue(new Error('not found'));
    await compareStore.compareContents('C:\\a.txt', 'D:\\b.txt');
    expect(compareStore.errorText).toBe('not found');
    expect(compareStore.fileResult).toBeNull();
  });
});
