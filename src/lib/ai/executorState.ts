// AI-06 / FER-195: state machine run + penjadwal dependency steps + ringkasan status dari queue.
// Semua transisi dikendalikan executor; model tidak punya jalur ke sini.
import type { RunState } from './chatStore.svelte';
import type { PlanStep } from './contracts';
import type { OperationProgress } from '../ipc/types';

const TRANSITIONS: Record<RunState, readonly RunState[]> = {
  // ACT-08: a read-only history question ends in `answered` straight from planning (no plan/approval).
  planning: ['resolving', 'needsInput', 'blockedCapability', 'failed', 'cancelled', 'answered'],
  resolving: ['preview', 'needsInput', 'blockedCapability', 'stale', 'failed', 'cancelled'],
  // AI-09: follow-up membuat revision baru sebelum queue (preview/awaitingApproval/needsInput -> resolving/planning).
  preview: [
    'awaitingApproval',
    'queued',
    'blockedCapability',
    'stale',
    'failed',
    'cancelled',
    'resolving',
    'planning',
  ],
  awaitingApproval: [
    'queued',
    'needsInput',
    'stale',
    'failed',
    'cancelled',
    'resolving',
    'planning',
  ],
  queued: [
    'running',
    'succeeded',
    'partial',
    'failed',
    'cancelled',
    'stale',
    'blockedCapability',
    'unknownOutcome',
  ],
  running: ['succeeded', 'partial', 'failed', 'cancelled', 'unknownOutcome'],
  needsInput: ['cancelled', 'resolving', 'planning'],
  // Hanya rekonsiliasi eksplisit yang boleh keluar dari unknownOutcome; tidak pernah ke queued (replay).
  unknownOutcome: ['running', 'succeeded', 'partial', 'failed', 'cancelled'],
  succeeded: [],
  partial: [],
  failed: [],
  cancelled: [],
  blockedCapability: [],
  stale: [],
  answered: [],
};

export function canTransition(from: RunState, to: RunState): boolean {
  return TRANSITIONS[from].includes(to);
}

/**
 * FER-471: extra edges for a `copy.adjust` run only (no plan, no queue job): the request waits for the
 * card's confirm button and then ends. Plan runs never get these edges.
 */
const CONTROL_TRANSITIONS: Partial<Record<RunState, readonly RunState[]>> = {
  planning: ['awaitingApproval', 'succeeded'],
  awaitingApproval: ['succeeded'],
};
export function canControlTransition(from: RunState, to: RunState): boolean {
  return CONTROL_TRANSITIONS[from]?.includes(to) ?? false;
}
/** State akhir: tidak ada transisi keluar. */
export function isFinal(state: RunState): boolean {
  return TRANSITIONS[state].length === 0;
}

export type StepStatus = 'pending' | 'running' | 'completed' | 'failed' | 'skipped';
export type StepOutcome = 'completed' | 'partial' | 'failed' | 'cancelled';

/** Menjadwalkan langkah plan sesuai `dependsOn`; langkah turunan tidak jalan setelah gagal/cancel. */
export class DependencyScheduler {
  private readonly status_ = new Map<string, StepStatus>();
  private readonly byId = new Map<string, PlanStep>();
  private cancelled = false;

  constructor(private readonly steps: readonly PlanStep[]) {
    for (const s of steps) {
      this.byId.set(s.id, s);
      this.status_.set(s.id, 'pending');
    }
  }

  status(id: string): StepStatus {
    const s = this.status_.get(id);
    if (!s) throw new Error(`Unknown step ${id}`);
    return s;
  }

  /** Langkah pending yang seluruh dependensinya completed. */
  ready(): PlanStep[] {
    if (this.cancelled) return [];
    return this.steps.filter(
      (s) =>
        this.status_.get(s.id) === 'pending' &&
        s.dependsOn.every((d) => this.status_.get(d) === 'completed'),
    );
  }

  start(id: string): void {
    const step = this.byId.get(id);
    if (!step) throw new Error(`Unknown step ${id}`);
    if (this.status_.get(id) !== 'pending') throw new Error(`Step ${id} already started`);
    if (this.cancelled || !step.dependsOn.every((d) => this.status_.get(d) === 'completed')) {
      throw new Error(`Step ${id} dependencies are not completed`);
    }
    this.status_.set(id, 'running');
  }

  complete(id: string): void {
    if (this.status(id) === 'running') this.status_.set(id, 'completed');
  }

  fail(id: string): void {
    if (this.status(id) === 'running') this.status_.set(id, 'failed');
    this.skipDependents();
  }

  /** Hentikan penjadwalan langkah turunan; langkah yang sedang berjalan boleh tetap selesai. */
  cancel(): void {
    this.cancelled = true;
    this.skipDependents();
  }

  private skipDependents(): void {
    for (const s of this.steps) {
      if (this.status_.get(s.id) !== 'pending') continue;
      const blocked =
        this.cancelled ||
        s.dependsOn.some((d) => ['failed', 'skipped'].includes(this.status_.get(d) ?? ''));
      if (blocked) this.status_.set(s.id, 'skipped');
    }
    // skip berantai
    let changed = true;
    while (changed) {
      changed = false;
      for (const s of this.steps) {
        if (
          this.status_.get(s.id) === 'pending' &&
          s.dependsOn.some((d) => this.status_.get(d) === 'skipped')
        ) {
          this.status_.set(s.id, 'skipped');
          changed = true;
        }
      }
    }
  }

  get finished(): boolean {
    return [...this.status_.values()].every(
      (s) => s === 'completed' || s === 'failed' || s === 'skipped',
    );
  }

  outcome(): StepOutcome {
    const all = [...this.status_.values()];
    const completed = all.filter((s) => s === 'completed').length;
    if (completed === all.length) return 'completed';
    if (completed > 0) return 'partial';
    return all.includes('failed') ? 'failed' : 'cancelled';
  }
}

export type RunOutcome =
  'queued' | 'running' | 'completed' | 'partial' | 'failed' | 'cancelled' | 'unknownOutcome';

export interface RunSummary {
  outcome: RunOutcome;
  completedFiles: number;
  failed: number;
}

type JobLookup = (
  jobId: string,
) => Pick<OperationProgress, 'state' | 'processedFiles' | 'failures'> | undefined;

/**
 * Status run dari status engine HANYA untuk job ID milik run. Job yang tak dikenal queue
 * tidak pernah dihitung sukses. Dibatalkan setelah sebagian tersalin = partial (tanpa rollback).
 */
export function summarizeRun(jobIds: readonly string[], lookup: JobLookup): RunSummary {
  let files = 0;
  let failedItems = 0;
  let queued = 0;
  let running = 0;
  let completed = 0;
  let failedJobs = 0;
  let cancelled = 0;
  let unknown = 0;
  for (const id of jobIds) {
    const j = lookup(id);
    if (!j) {
      unknown++;
      continue;
    }
    files += j.processedFiles ?? 0;
    failedItems += j.failures?.length ?? 0;
    if (j.state === 'queued') queued++;
    else if (j.state === 'running' || j.state === 'paused') running++;
    else if (j.state === 'completed') completed++;
    else if (j.state === 'failed') {
      failedJobs++;
      if (!j.failures?.length) failedItems++;
    } else cancelled++;
  }
  const make = (outcome: RunOutcome): RunSummary => ({
    outcome,
    completedFiles: files,
    failed: failedItems,
  });
  if (jobIds.length === 0 || unknown > 0) return make('unknownOutcome');
  if (running > 0 || (queued > 0 && queued < jobIds.length)) return make('running');
  if (queued > 0) return make('queued');
  if (failedJobs === 0 && cancelled === 0) return make('completed');
  if (files > 0 || completed > 0) return make('partial');
  return make(failedJobs > 0 ? 'failed' : 'cancelled');
}
