/**
 * Background Transfer Manager (F2-26) view model — pure logic over the
 * operation snapshot so menus/panel tests never touch IPC or the DOM.
 * The BTM is the existing operation queue wearing its Total Commander name:
 * transfers continue in the Rust worker independent of any dialog or panel.
 */
import type { OperationProgress } from './ipc/types';

export type BtmPhase = 'active' | 'history';

const ACTIVE_STATES = new Set(['queued', 'running']);

/** Split the snapshot: active ops keep queue order (stable id sort), the
 *  history is newest-finished-first (enqueue order as tiebreaker). */
export function splitBtm(all: OperationProgress[]): {
  active: OperationProgress[];
  history: OperationProgress[];
} {
  const active: OperationProgress[] = [];
  const history: OperationProgress[] = [];
  for (const op of all) (ACTIVE_STATES.has(op.state) ? active : history).push(op);
  history.sort((a, b) => finishedKey(b) - finishedKey(a));
  return { active, history };
}

function finishedKey(op: OperationProgress): number {
  return op.finishedAtMs ?? 0;
}

/** Retry is offered ONLY for failed operations — the engines expose no safe
 *  checkpoints (D14), so there is deliberately no pause/resume, and retried
 *  work always starts from scratch under a new operation id. */
export function isRetryable(op: OperationProgress): boolean {
  return op.state === 'failed';
}

/** Wall-clock duration for finished ops; null while active. */
export function durationMsOf(op: OperationProgress): number | null {
  if (op.queuedAtMs === undefined || op.finishedAtMs === undefined) return null;
  return Math.max(0, op.finishedAtMs - op.queuedAtMs);
}

/** Compact duration label for the history rows. */
export function formatDuration(ms: number): string {
  if (ms < 1000) return `${ms}ms`;
  const s = ms / 1000;
  if (s < 60) return `${s.toFixed(s < 10 ? 1 : 0)}s`;
  const m = Math.floor(s / 60);
  const rest = Math.round(s - m * 60);
  return `${m}m ${String(rest).padStart(2, '0')}s`;
}

/** Terminal timestamp for the history rows (epoch ms or null while active). */
export function finishedMsOf(op: OperationProgress): number | null {
  return op.finishedAtMs ?? null;
}

/** Number of per-file failures carried by the final progress. */
export function failureCount(op: OperationProgress): number {
  return op.failures?.length ?? 0;
}

/**
 * One-line detail for a job row: what it is doing right now, or a compact
 * summary of why it ended. Keeps the panel scannable while the per-file
 * failures stay one expansion away.
 */
export function detailLine(op: OperationProgress): string {
  if (op.currentPath) return op.currentPath;
  switch (op.state) {
    case 'queued':
      return 'Waiting in queue';
    case 'completed':
      return `${op.processedFiles} file(s) processed`;
    case 'cancelled':
      return 'Stopped by user';
    case 'failed':
      return op.error ?? 'Failed';
    default:
      return '';
  }
}
