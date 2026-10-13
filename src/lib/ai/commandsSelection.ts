// AI-02 / FER-182: local selection handle and job-id outputs.
//
// A SelectionHandle is a frozen manifest of the items a prepared command will act
// on. It is NOT the pane's global selection: it keeps its own copy, so later
// selection changes, pane navigation or the resolver releasing its copy cannot
// change what an approved command touches (spec 5), and running a command never
// writes selection back to the panes. Paths stay local: `toJSON` exposes only an
// opaque id, counts and a digest.
import { selectionDigest, type PaneName } from './context';
import type { ManifestEntry, ScopeOk } from './contextResolver';
import type { CommandReceipt } from './commands';
import type { OperationProgress } from '../ipc/types';

export interface SelectionHandle {
  readonly handleId: string;
  readonly pane: PaneName;
  readonly tabId: string;
  readonly sourceDirectory: string;
  readonly contextRevision: number;
  readonly selectionRevision: number;
  readonly count: number;
  readonly digest: string;
  /** Frozen item paths. Local executors only; never send to the model. */
  paths(): readonly string[];
  toJSON(): { handleId: string; pane: PaneName; count: number; digest: string };
}

export class SelectionHandleStore {
  private readonly handles = new Map<string, SelectionHandle>();
  private seq = 0;

  create(scope: ScopeOk, entries: readonly ManifestEntry[]): SelectionHandle {
    const paths = Object.freeze(entries.map((e) => e.path));
    const handleId = `msel_${(++this.seq).toString(36)}_${Math.floor(Math.random() * 36 ** 4).toString(36)}`;
    const digest = selectionDigest(paths);
    const handle: SelectionHandle = Object.freeze({
      handleId,
      pane: scope.pane,
      tabId: scope.tabId,
      sourceDirectory: scope.sourceDirectory,
      contextRevision: scope.contextRevision,
      selectionRevision: scope.selectionRevision,
      count: paths.length,
      digest,
      paths: () => paths,
      toJSON: () => ({ handleId, pane: scope.pane, count: paths.length, digest }),
    });
    this.handles.set(handleId, handle);
    return handle;
  }

  get(handleId: string): SelectionHandle | undefined {
    return this.handles.get(handleId);
  }

  release(handleId: string): void {
    this.handles.delete(handleId);
  }

  get size(): number {
    return this.handles.size;
  }
}

export interface JobSummary {
  total: number;
  queued: number;
  running: number;
  completed: number;
  failed: number;
  cancelled: number;
  /** Job ids the queue snapshot does not know (never counted as success). */
  unknown: number;
  terminal: boolean;
  outcome: 'running' | 'completed' | 'partial' | 'failed' | 'cancelled' | 'unknown';
}

/**
 * Status of a run, derived ONLY from the queue's view of the receipt's own job
 * ids. Other operations in the queue are ignored; a missing job is `unknown`.
 */
export function summarizeJobs(
  receipt: Pick<CommandReceipt, 'jobIds' | 'state'>,
  lookup: (jobId: string) => Pick<OperationProgress, 'state'> | undefined,
): JobSummary {
  const s: JobSummary = {
    total: receipt.jobIds.length,
    queued: 0,
    running: 0,
    completed: 0,
    failed: 0,
    cancelled: 0,
    unknown: 0,
    terminal: false,
    outcome: 'unknown',
  };
  for (const id of receipt.jobIds) {
    const state = lookup(id)?.state;
    if (state === 'queued') s.queued++;
    else if (state === 'running' || state === 'paused') s.running++;
    else if (state === 'completed') s.completed++;
    else if (state === 'failed') s.failed++;
    else if (state === 'cancelled') s.cancelled++;
    else s.unknown++;
  }
  if (s.total === 0) {
    // No queue jobs: only an in-process service result can be terminal.
    if (receipt.state === 'completed') {
      s.terminal = true;
      s.outcome = 'completed';
    }
    return s;
  }
  if (s.unknown > 0) return s;
  if (s.queued + s.running > 0) {
    s.outcome = 'running';
    return s;
  }
  s.terminal = true;
  if (s.failed === 0 && s.cancelled === 0) s.outcome = 'completed';
  else if (s.completed === 0) s.outcome = s.failed > 0 ? 'failed' : 'cancelled';
  else s.outcome = 'partial';
  return s;
}
