// FER-471: `copy.adjust` — the AI may tune the copy job that is running right now.
//
// What this is NOT: a plan. No prepare, no ResolvedPlan, no ledger write, no queue job, no path and
// no operation id from the model. The model only picks an action; the app resolves WHICH running
// copy job it applies to, shows what will change on a card, and runs it only after the user presses
// the confirm button (approval pattern of the executor). The effect is performance only: threads,
// auto mode and OS priority of the copy workers. Files are never touched.
import {
  COPY_ADJUST_CAPABILITY,
  MAX_COPY_THREADS,
  type CopyAdjustRequest,
  type ControlCapabilityId,
} from './contracts';
import type { EngineGate, IntentSchema } from './commandsRegistry';
import type { CopyAdjust, CopyControlState, CopyPriority } from '../ipc/copyControl';

/** Executor port: running copy jobs and the one write the capability may do. */
export interface CopyControlPort {
  /** True while a copy job is running (the offer rule; folded with the backend gate in Rust). */
  available(): boolean;
  running(): Promise<CopyControlState[]>;
  /** The backend's confirmed state, or null when the job is no longer running. */
  adjust(operationId: string, change: CopyAdjust): Promise<CopyControlState | null>;
}

export const CONTROL_CAPABILITY_GATES: Record<ControlCapabilityId, EngineGate> = {
  'copy.adjust': { status: 'ready' },
};

export interface ControlOfferInput {
  /** A copy job is running now (checked again by the backend before the provider call). */
  copyRunning: boolean;
  gates?: Partial<Record<ControlCapabilityId, EngineGate>>;
}

export function offeredControlCapabilityIds(input: ControlOfferInput): ControlCapabilityId[] {
  const gates = { ...CONTROL_CAPABILITY_GATES, ...input.gates };
  return input.copyRunning && gates['copy.adjust'].status === 'ready' ? ['copy.adjust'] : [];
}

/** Model-facing schema; mirrors `parseOfferedIntent` (examples are checked by tests). */
export const COPY_ADJUST_SCHEMA: IntentSchema = {
  type: 'object',
  additionalProperties: false,
  required: ['capability', 'action'],
  properties: {
    capability: { const: COPY_ADJUST_CAPABILITY },
    action: { enum: ['increase', 'decrease', 'set', 'auto', 'priority'] },
    by: { type: 'integer', minimum: 1, maximum: 8 },
    threads: { type: 'integer', minimum: 1, maximum: MAX_COPY_THREADS },
    priority: { enum: ['normal', 'background'] },
  },
  examples: [
    { capability: 'copy.adjust', action: 'increase' },
    { capability: 'copy.adjust', action: 'decrease', by: 2 },
    { capability: 'copy.adjust', action: 'set', threads: 8 },
    { capability: 'copy.adjust', action: 'auto' },
    { capability: 'copy.adjust', action: 'priority', priority: 'background' },
  ],
};

/** The settings a card shows before/after. */
export interface CopySettingsView {
  threads: number;
  auto: boolean;
  priority: CopyPriority;
}

export interface CopyAdjustView {
  /** App-built sentence: what will change (never model text). */
  summary: string;
  /** Which running copy job (app-resolved). */
  job: string;
  before: CopySettingsView;
  after: CopySettingsView;
  /** Set once the user confirmed and the backend answered. */
  result?: { ok: boolean; message: string; applied?: CopySettingsView };
}

export function settingsOf(state: CopyControlState): CopySettingsView {
  return { threads: state.threads, auto: state.auto, priority: state.priority };
}

const clampThreads = (n: number) => Math.min(MAX_COPY_THREADS, Math.max(1, Math.round(n)));

/** Wire change for the backend (same vocabulary as the progress dialog buttons). */
export function toCopyAdjust(request: CopyAdjustRequest): CopyAdjust {
  switch (request.action) {
    case 'increase':
      return { kind: 'up', by: request.by ?? 1 };
    case 'decrease':
      return { kind: 'down', by: request.by ?? 1 };
    case 'set':
      return { kind: 'set', threads: request.threads };
    case 'auto':
      return { kind: 'auto', enabled: true };
    case 'priority':
      return { kind: 'priority', priority: request.priority };
  }
}

/**
 * What is sent at confirm time: the ABSOLUTE result shown on the card. A relative +1 would land on
 * whatever the thread count is by then (auto mode or the [+] button may have moved it), so the user
 * would get something other than what they approved.
 */
export function toConfirmedChange(request: CopyAdjustRequest, after: CopySettingsView): CopyAdjust {
  switch (request.action) {
    case 'increase':
    case 'decrease':
    case 'set':
      return { kind: 'set', threads: after.threads };
    default:
      return toCopyAdjust(request);
  }
}

/** What the backend will do with the request (mirror of `CopyControl::adjust`). */
export function previewCopyAdjust(
  before: CopySettingsView,
  request: CopyAdjustRequest,
): CopySettingsView {
  switch (request.action) {
    case 'increase':
      return { ...before, auto: false, threads: clampThreads(before.threads + (request.by ?? 1)) };
    case 'decrease':
      return { ...before, auto: false, threads: clampThreads(before.threads - (request.by ?? 1)) };
    case 'set':
      return { ...before, auto: false, threads: clampThreads(request.threads) };
    case 'auto':
      return { ...before, auto: true };
    case 'priority':
      return { ...before, priority: request.priority };
  }
}

export const PRIORITY_LABEL: Record<CopyPriority, string> = {
  normal: 'Normal',
  background: 'Latar belakang',
};

export function describeSettings(s: CopySettingsView): string {
  return `${s.auto ? 'Otomatis' : `${s.threads} thread`} · prioritas ${PRIORITY_LABEL[s.priority]}`;
}

export function sameSettings(a: CopySettingsView, b: CopySettingsView): boolean {
  return a.threads === b.threads && a.auto === b.auto && a.priority === b.priority;
}

/** One app-written sentence for the confirm card. */
export function describeCopyAdjust(
  request: CopyAdjustRequest,
  before: CopySettingsView,
  after: CopySettingsView,
): string {
  switch (request.action) {
    case 'increase':
      return `Naikkan thread copy: ${before.threads} → ${after.threads} (mode otomatis dimatikan).`;
    case 'decrease':
      return `Turunkan thread copy: ${before.threads} → ${after.threads} (mode otomatis dimatikan).`;
    case 'set':
      return `Atur thread copy ke ${after.threads} (mode otomatis dimatikan).`;
    case 'auto':
      return 'Aktifkan mode otomatis: Ferrum menyesuaikan jumlah thread dengan kecepatan disk.';
    case 'priority':
      return `Ubah prioritas copy ke ${PRIORITY_LABEL[after.priority]}.`;
  }
}

export function describeJob(state: CopyControlState): string {
  return `Copy yang sedang berjalan (${state.operationId}): ${describeSettings(settingsOf(state))}, ${state.activeWorkers} thread aktif`;
}
