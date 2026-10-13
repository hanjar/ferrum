import { beforeEach, describe, expect, it } from 'vitest';
import {
  detailLine,
  durationMsOf,
  failureCount,
  formatDuration,
  isRetryable,
  splitBtm,
} from './btm';
import { getAction, isEnabled, labelOf, matches, menuGroups, runAction } from './actions';
import type { OperationProgress } from './ipc/types';
import { ops } from './stores/opsStore.svelte';

function op(over: Partial<OperationProgress>): OperationProgress {
  return {
    operationId: 'op_x',
    kind: 'copy',
    state: 'running',
    processedBytes: 0,
    processedFiles: 0,
    ...over,
  };
}

beforeEach(() => {
  ops.panelOpen = false;
});

describe('BTM view model (F2-26)', () => {
  it('splits active from history and orders history newest-finished-first', () => {
    const a = op({ operationId: 'op_a', state: 'running' });
    const q = op({ operationId: 'op_b', state: 'queued' });
    const h1 = op({ operationId: 'op_c', state: 'failed', finishedAtMs: 100 });
    const h2 = op({ operationId: 'op_d', state: 'completed', finishedAtMs: 300 });
    const h3 = op({ operationId: 'op_e', state: 'cancelled', finishedAtMs: 200 });
    const { active, history } = splitBtm([h1, a, h2, q, h3]);
    expect(active.map((o) => o.operationId)).toEqual(['op_a', 'op_b']); // stable id order
    expect(history.map((o) => o.operationId)).toEqual(['op_d', 'op_e', 'op_c']);
  });

  it('offers retry only for failed operations', () => {
    for (const state of ['queued', 'running', 'completed', 'cancelled'] as const) {
      expect(isRetryable(op({ state })), state).toBe(false);
    }
    expect(isRetryable(op({ state: 'failed' }))).toBe(true);
  });

  it('computes durations and formats them for the history rows', () => {
    expect(durationMsOf(op({ state: 'running' }))).toBeNull();
    expect(durationMsOf(op({ queuedAtMs: 1000, finishedAtMs: 1500 }))).toBe(500);
    expect(durationMsOf(op({ finishedAtMs: 1500 }))).toBeNull(); // never queued → unknown
    expect(formatDuration(50)).toBe('50ms');
    expect(formatDuration(4300)).toBe('4.3s');
    expect(formatDuration(65_000)).toBe('1m 05s');
  });

  it('counts per-file failures for the details toggle', () => {
    expect(failureCount(op({ state: 'failed' }))).toBe(0);
    expect(
      failureCount(op({ state: 'failed', failures: [{ path: 'C:\\a', message: 'locked' }] })),
    ).toBe(1);
  });

  it('derives one detail line per job state', () => {
    expect(detailLine(op({ currentPath: 'C:\\t\\f.bin' }))).toBe('C:\\t\\f.bin');
    expect(detailLine(op({ state: 'queued' }))).toBe('Waiting in queue');
    expect(detailLine(op({ state: 'cancelled' }))).toBe('Stopped by user');
    expect(detailLine(op({ state: 'completed', processedFiles: 7, totalFiles: 7 }))).toBe(
      '7 file(s) processed',
    );
    expect(detailLine(op({ state: 'failed', error: 'disk full' }))).toBe('disk full');
    expect(detailLine(op({ state: 'failed' }))).toBe('Failed');
  });
});

describe('BTM registry command (F2-26)', () => {
  it('exposes Background Transfer Manager in the Commands menu with Alt+B', () => {
    const action = getAction('commands.btm');
    expect(action).toBeDefined();
    expect(action!.group).toBe('Commands');
    expect(labelOf(action!)).toBe('Background Transfer Manager');
    expect(action!.shortcut).toEqual({ key: 'b', alt: true });
    expect(isEnabled(action!)).toBe(true);
    // Lives in the Commands menu group (TC Commands → Background Transfer
    // Manager); the generic View alias stays out of the menu.
    const commands = menuGroups.find((g) => g.label === 'Commands')!;
    expect(commands.ids).toContain('commands.btm');
    expect(commands.ids).not.toContain('queue.panel');
  });

  it('Alt+B opens the manager instead of toggling it closed', () => {
    ops.panelOpen = false;
    const action = getAction('commands.btm')!;
    expect(
      matches(
        { key: 'b', alt: true },
        { key: 'b', ctrlKey: false, metaKey: false, shiftKey: false, altKey: true },
      ),
    ).toBe(true);
    expect(runAction(action)).toBe(true);
    expect(ops.panelOpen).toBe(true);
    // Opening again keeps the panel open (show semantics, not toggle).
    runAction(action);
    expect(ops.panelOpen).toBe(true);
    ops.panelOpen = false;
  });

  it('does not bind Alt+B to any other action', () => {
    const hits = getAction('commands.btm');
    expect(hits).toBeDefined();
    const event = { key: 'b', ctrlKey: false, metaKey: false, shiftKey: false, altKey: true };
    const fromEvent = [getAction('commands.btm')!].filter(
      (a) => a.shortcut && matches(a.shortcut, event),
    );
    expect(fromEvent.map((a) => a.id)).toEqual(['commands.btm']);
  });
});
