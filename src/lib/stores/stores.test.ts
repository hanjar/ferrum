import { afterEach, describe, expect, it } from 'vitest';
import { pctOf, ops } from './opsStore.svelte';
import { dialogs } from './dialogStore.svelte';
import { activePane, inactivePane, registerPane, refreshAllPanes } from './panesRegistry.svelte';
import { ui } from '../uiState.svelte';
import type { OperationProgress } from '../ipc/types';

const p = (over: Partial<OperationProgress>): OperationProgress => ({
  operationId: 'op_x',
  kind: 'copy',
  state: 'running',
  processedBytes: 0,
  processedFiles: 0,
  ...over,
});

afterEach(() => {
  dialogs.close();
  ui.activePane = 0;
});

describe('pctOf', () => {
  it('uses byte totals when present', () => {
    expect(pctOf(p({ processedBytes: 50, totalBytes: 100 }))).toBe(50);
    expect(pctOf(p({ processedBytes: 150, totalBytes: 100 }))).toBe(100); // clamped
  });

  it('falls back to file counts', () => {
    expect(pctOf(p({ processedFiles: 1, totalFiles: 4 }))).toBe(25);
  });

  it('queued = 0, completed = 100', () => {
    expect(pctOf(p({ state: 'queued' }))).toBe(0);
    expect(pctOf(p({ state: 'completed' }))).toBe(100);
  });

  it('unknown totals read as 0', () => {
    expect(pctOf(p({}))).toBe(0);
  });
});

describe('ops store state helpers', () => {
  it('active filters to queued/running', () => {
    ops.ops.set('a', p({ operationId: 'a', state: 'queued' }));
    ops.ops.set('b', p({ operationId: 'b', state: 'completed' }));
    ops.ops.set('c', p({ operationId: 'c', state: 'running' }));
    expect(ops.active.map((o) => o.operationId)).toEqual(['a', 'c']);
    expect(ops.all).toHaveLength(3);
    ops.ops.clear();
  });

  it('panel toggles', () => {
    const before = ops.panelOpen;
    ops.togglePanel();
    expect(ops.panelOpen).toBe(!before);
    ops.togglePanel();
  });
});

describe('pane registry', () => {
  it('active/inactive follow ui.activePane; register returns unregister', () => {
    const api = {
      path: 'C:\\x',
      selectedPaths: () => [],
      cursorEntry: () => undefined,
      refresh: () => {},
      newTab: () => {},
      closeCurrentTab: () => {},
      goUp: () => {},
      openCursorEntry: () => {},
    };
    const off = registerPane(1, api);
    ui.activePane = 1;
    expect(activePane()?.path).toBe('C:\\x');
    expect(inactivePane()).toBeUndefined();
    let refreshed = 0;
    const off0 = registerPane(0, { ...api, refresh: () => (refreshed += 1) });
    refreshAllPanes();
    expect(refreshed).toBe(1);
    off();
    off0();
    expect(activePane()).toBeUndefined();
  });
});

describe('dialog openers (browser mode)', () => {
  it('mkdir binds to the active pane path', () => {
    registerPane(0, {
      path: 'C:\\somewhere',
      selectedPaths: () => [],
      cursorEntry: () => undefined,
      refresh: () => {},
      newTab: () => {},
      closeCurrentTab: () => {},
      goUp: () => {},
      openCursorEntry: () => {},
    });
    dialogs.beginMkdir();
    expect(dialogs.current).toEqual({ kind: 'mkdir', parent: 'C:\\somewhere' });
    dialogs.close();
  });

  it('rename without a cursor explains the problem', () => {
    registerPane(0, {
      path: 'C:\\t',
      selectedPaths: () => [],
      cursorEntry: () => undefined,
      refresh: () => {},
      newTab: () => {},
      closeCurrentTab: () => {},
      goUp: () => {},
      openCursorEntry: () => {},
    });
    dialogs.beginRename();
    expect(dialogs.current?.kind).toBe('error');
    dialogs.close();
  });

  it('delete without selection explains the problem', () => {
    registerPane(0, {
      path: 'C:\\t',
      selectedPaths: () => [],
      cursorEntry: () => undefined,
      refresh: () => {},
      newTab: () => {},
      closeCurrentTab: () => {},
      goUp: () => {},
      openCursorEntry: () => {},
    });
    dialogs.beginDelete();
    expect(dialogs.current?.kind).toBe('error');
    dialogs.close();
  });
});
