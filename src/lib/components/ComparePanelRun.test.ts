/** FER-106..109 UI: options, diff, progress/cancel, partial results, recursive mark/hide. */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, render } from '@testing-library/svelte';
import { tick } from 'svelte';
import ComparePanel from './ComparePanel.svelte';
import { getAction, runAction } from '../actions';
import { compareStore } from '../stores/compareStore.svelte';
import { compareRun } from '../stores/compareRun.svelte';
import { registerPane, type PaneApi, type PaneEntryInfo } from '../stores/panesRegistry.svelte';
import type { CompareDirsReport, DirDiffEntry } from '../ipc/compare';

const compareDirsMock = vi.fn();
const compareFilesMock = vi.fn();
const cancelMock = vi.fn<(...args: unknown[]) => Promise<void>>(async () => {});
const clearMock = vi.fn(async () => {});
vi.mock('../ipc/compare', () => ({
  COMPARE_PROGRESS_EVENT: 'compare://progress',
  validateSyncRoots: vi.fn(async () => {}),
  compareDirs: (...a: unknown[]) => compareDirsMock(...a),
  compareFiles: (...a: unknown[]) => compareFilesMock(...a),
  cancelCompare: (...a: unknown[]) => cancelMock(...a),
  clearCompareCache: () => clearMock(),
}));
let progressHandler: ((e: { payload: unknown }) => void) | null = null;
vi.mock('@tauri-apps/api/event', () => ({
  listen: async (_n: string, h: (e: { payload: unknown }) => void) => {
    progressHandler = h;
    return () => {
      progressHandler = null;
    };
  },
}));

function pane(path: string, rels: string[]) {
  const st = { selection: [] as string[] };
  const entries: PaneEntryInfo[] = rels.map((r) => ({
    name: r.split('/').pop()!,
    path: `${path}\\${r.replace(/\//g, '\\')}`,
    isDir: false,
    size: 1,
    modifiedMs: 1,
  }));
  const api = {
    path,
    getSelection: () => st.selection,
    selectedPaths: () => st.selection,
    cursorEntry: () => undefined,
    refresh: () => {},
    newTab: () => {},
    closeCurrentTab: () => {},
    goUp: () => {},
    openCursorEntry: () => {},
    visibleEntries: () => entries,
    selectPaths: (p: Iterable<string>) => {
      st.selection = [...p];
    },
  };
  return api as unknown as PaneApi & { getSelection: () => string[] };
}
const E = (name: string, state: DirDiffEntry['state']): DirDiffEntry => ({
  name,
  state,
  leftSize: 1,
  leftModifiedMs: 1,
  rightSize: 1,
  rightModifiedMs: 1,
});
const rep = (
  entries: DirDiffEntry[],
  extra: Partial<CompareDirsReport> = {},
): CompareDirsReport => ({
  leftRoot: 'C:\\L',
  rightRoot: 'D:\\R',
  entries,
  truncated: false,
  ...extra,
});

let left: ReturnType<typeof pane>;
beforeEach(() => {
  compareDirsMock.mockReset();
  compareFilesMock.mockReset();
  cancelMock.mockClear();
  clearMock.mockClear();
  compareStore.reset();
  left = pane('C:\\L', ['sub/new.txt', 'sub/same.txt', 'top.txt']);
  registerPane(0, left);
  registerPane(1, pane('D:\\R', ['sub/new.txt', 'sub/same.txt', 'top.txt']));
});
afterEach(() => {
  cleanup();
  compareStore.reset();
});

describe('options', () => {
  it('forwards content + recursive toggles and a unique opId to compareDirs', async () => {
    compareDirsMock.mockResolvedValue(rep([]));
    compareStore.panelOpen = true;
    const r = render(ComparePanel);
    await tick();
    const boxes = r.container.querySelectorAll<HTMLInputElement>('input[type=checkbox]');
    boxes[0].click();
    boxes[1].click();
    await tick();
    expect(r.container.textContent).toContain('Bandingkan isi (content)');
    expect(r.container.textContent).toContain('Rekursif');
    expect(r.container.textContent).toContain('2 detik');
    runAction(getAction('mark.compareDirectories')!);
    await vi.waitFor(() => expect(compareDirsMock).toHaveBeenCalledTimes(1));
    const [, , byContent, opts] = compareDirsMock.mock.calls[0];
    expect(byContent).toBe(true);
    expect(opts.recursive).toBe(true);
    await vi.waitFor(() => expect(compareStore.busy).toBe(false));
    await compareStore.compareDirectories();
    expect(compareDirsMock.mock.calls[1][3].opId).not.toBe(opts.opId);
  });

  it('passes the chosen encoding to compareFiles and shows diff, encoding, cache, truncation', async () => {
    compareFilesMock.mockResolvedValue({
      equal: false,
      firstMismatch: 3,
      leftSize: 5,
      rightSize: 6,
      bothText: true,
      fromCache: true,
      textDiff: {
        encoding: 'utf-16le',
        truncated: true,
        hunks: [
          {
            leftStart: 2,
            rightStart: 2,
            lines: [
              { kind: 'ctx', text: 'same' },
              { kind: 'del', text: 'old' },
              { kind: 'add', text: 'new' },
            ],
          },
        ],
      },
    });
    await compareStore.compareContents('C:\\L\\a', 'D:\\R\\b', 'latin1');
    expect(compareFilesMock).toHaveBeenCalledWith('C:\\L\\a', 'D:\\R\\b', {
      opId: expect.any(String),
      encoding: 'latin1',
    });
    const r = render(ComparePanel);
    await tick();
    expect(r.container.querySelectorAll('.dl.add')).toHaveLength(1);
    expect(r.container.querySelectorAll('.dl.del')).toHaveLength(1);
    const t = r.container.textContent ?? '';
    expect(t).toContain('@@ -2 +2 @@');
    expect(t).toContain('Diff truncated');
    expect(t).toContain('encoding utf-16le');
    expect(t).toContain('from cache');
  });
});

describe('progress, cancel, partial', () => {
  it('shows progress, Cancel calls cancelCompare(opId), cancelled result is partial and unused', async () => {
    let resolve!: (v: CompareDirsReport) => void;
    compareDirsMock.mockReturnValue(new Promise((r) => (resolve = r)));
    compareStore.panelOpen = true;
    const r = render(ComparePanel);
    const run = compareStore.compareDirectories(false, true);
    await vi.waitFor(() => expect(progressHandler).not.toBeNull());
    const opId = compareRun.opId!;
    progressHandler!({ payload: { opId: 'other', phase: 'scan', done: 9 } });
    progressHandler!({ payload: { opId, phase: 'content', done: 5, total: 10, current: 'x.bin' } });
    await tick();
    expect(r.container.textContent).toContain('content: 5/10');
    expect(r.container.querySelector('progress')!.getAttribute('value')).toBe('50');
    (r.container.querySelector('button.cancel') as HTMLElement).click();
    expect(cancelMock).toHaveBeenCalledWith(opId);
    resolve(rep([E('top.txt', 'newerLeft')], { cancelled: true }));
    await run;
    await tick();
    expect(r.container.textContent).toContain('Cancelled — partial result');
    compareStore.markNewerHideSame();
    expect(compareStore.hiddenSameNames).toBeNull();
    expect(left.getSelection()).toEqual([]);
    expect(compareStore.errorText).toContain('cancelled');
  });

  it('a newer run discards the old result', async () => {
    let first!: (v: CompareDirsReport) => void;
    compareDirsMock.mockReturnValueOnce(new Promise((r) => (first = r)));
    compareDirsMock.mockResolvedValueOnce(rep([E('b.txt', 'same')]));
    const p1 = compareStore.compareDirectories(false, false);
    await vi.waitFor(() => expect(compareDirsMock).toHaveBeenCalledTimes(1));
    await compareStore.compareDirectories(false, false);
    first(rep([E('OLD.txt', 'same')]));
    await p1;
    expect(compareStore.report!.entries[0].name).toBe('b.txt');
  });

  it('shows errors[] and truncated explicitly; clear cache calls the IPC', async () => {
    compareDirsMock.mockResolvedValue(
      rep([E('sub/a.txt', 'sizeDifferent')], {
        truncated: true,
        recursive: true,
        errors: [{ name: 'sub/locked.txt', message: 'Access denied' }],
      }),
    );
    await compareStore.compareDirectories(false, true);
    const r = render(ComparePanel);
    await tick();
    const t = r.container.textContent ?? '';
    expect(t).toContain('sub/a.txt');
    expect(t).toContain('1 entries could not be read');
    expect(t).toContain('sub/locked.txt — Access denied');
    expect(t).toContain('Truncated');
    await compareStore.clearCache();
    expect(clearMock).toHaveBeenCalled();
    expect(compareStore.cacheCleared).toBe(true);
  });
});

describe('recursive entries with Mark Newer / Hide Same', () => {
  it('matches relative paths and hides folders that are entirely same', async () => {
    compareDirsMock.mockResolvedValue(
      rep(
        [
          E('sub/new.txt', 'newerLeft'),
          E('sub/same.txt', 'same'),
          E('top.txt', 'same'),
          E('eq/x.txt', 'same'),
        ],
        { recursive: true },
      ),
    );
    await compareStore.compareDirectories(false, true);
    expect(runAction(getAction('mark.newerFiles')!)).toBe(true);
    expect(left.getSelection()).toEqual(['C:\\L\\sub\\new.txt']);
    expect(compareStore.hiddenSameNames).toEqual(
      new Set(['sub/same.txt', 'top.txt', 'eq/x.txt', 'eq']),
    );
  });
});
