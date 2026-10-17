/**
 * Automation IPC (Automation P1.1, design §7.2): thin typed wrappers over
 * `src-tauri/src/automation`. Errors arrive as `{ code, message }` with a closed-set code and
 * never carry paths. Outside the desktop app (plain browser dev, unit tests) an inert fallback
 * answers `unsupported`: nothing in the webview can create, approve or enable an automation.
 *
 * SE-1: no wrapper sends a "confirmed" or "approved" flag and none sends dialog text. Approving
 * and resuming show a native Windows confirmation from the backend, built from stored data; the
 * outcome comes back as the `status` of the result (`canceled`, `stale`, ...).
 */
import { invoke } from '@tauri-apps/api/core';
import { isInTauri } from './tauri';
import type { FieldError } from './syncProfiles';

// ── Draft (the policy part is exactly what is stored and hashed) ──

export type AutomationFreq =
  { kind: 'once'; date: string } | { kind: 'daily' } | { kind: 'weekly'; days: number[] };

export interface ScheduleTrigger {
  kind: 'schedule';
  freq: AutomationFreq;
  /** `HH:MM`, 24 hours. */
  time: string;
  /** IANA zone id. */
  tz: string;
  missed: 'skip' | 'runOnce';
}

export interface WatchTrigger {
  kind: 'watch';
  watchLeft: boolean;
  watchRight: boolean;
  onChanges: boolean;
  onAvailable: boolean;
  /** 2..=3600 */
  idleSecs: number;
  /** 60..=86400, default 900 */
  maxWaitSecs: number;
}

export type AutomationTrigger = ScheduleTrigger | WatchTrigger;

export interface AutomationConditions {
  skipOnBattery: boolean;
  skipOnMetered: boolean;
}

export interface AutomationPolicy {
  trigger: AutomationTrigger;
  runtime: 'app' | 'background';
  /** 0..=3, default 2 */
  retries: number;
  conditions: AutomationConditions;
}

export interface AutomationDraft {
  name: string;
  /** The sync profile to run; the backend looks its contents up. */
  profileId: string;
  policy: AutomationPolicy;
}

// ── Results ──

/** Why an automation needs a review (design §8.5, AP-6). */
export type NeedsReview =
  | { reason: 'profileChanged'; from: number; to: number }
  | { reason: 'policyChanged' }
  | { reason: 'notApproved' }
  | { reason: 'keyUnavailable' }
  | { reason: 'unsupportedVersion' };

export interface AutomationDto {
  id: string;
  name: string;
  kind: 'schedule' | 'watch';
  profileId: string;
  profileName: string;
  /** The profile's CURRENT revision. */
  profileRevision: number;
  policy: AutomationPolicy;
  enabled: boolean;
  configRev: number;
  createdMs: number;
  updatedMs: number;
  /** Present when the stored approval no longer verifies. */
  review?: NeedsReview;
}

export type StepStatus = 'ok' | 'warn' | 'fail';

/** One line of a test run or of the editor's readiness block (AP-2.2, AP-4.2). */
export interface Step {
  id: string;
  status: StepStatus;
  text: string;
}

export interface NextRun {
  atMs: number;
  label: string;
}

export interface AutomationValidateResult {
  errors: FieldError[];
  checks: Step[];
  /** Upcoming schedule occurrences; filled by the scheduler (empty until then). */
  next: NextRun[];
}

export interface AutomationTestResult {
  /** Required by `approve`; valid for 15 minutes, for this app session, for this exact policy. */
  testId: string;
  /** The id a NEW automation will get; `approve` takes it from the test, the webview sends none. */
  automationId: string;
  policyHash: string;
  steps: Step[];
  passed: boolean;
}

export type ApproveResult =
  { status: 'approved'; automation: AutomationDto } | { status: 'canceled' } | { status: 'stale' };

/** `needsChoice`: a run is in progress - show the pause dialog and call `pause`. */
export type SetEnabledResult =
  { status: 'enabled' | 'paused' | 'canceled' } | { status: 'needsChoice' };

export interface ApproveInput {
  /** The automation being edited; absent for a new one. */
  id?: string;
  /** The `configRev` the editor loaded; required with `id`. */
  expectedConfigRev?: number;
  draft: AutomationDraft;
  testId: string;
}

export interface TestInput {
  /** The automation being edited (its id is part of the policy hash); absent for a new one. */
  id?: string;
  draft: AutomationDraft;
}

export type PauseMode = 'finish' | 'stop';

/** A failed call; `code` is from the closed set of design §7 (`stale`, `needs_review`, ...). */
export class AutomationError extends Error {
  constructor(
    public code: string,
    /** The backend's fixed, path-free message; only used to tell cases apart. */
    public detail?: string,
  ) {
    super(code);
    this.name = 'AutomationError';
  }
}

export interface AutomationBackend {
  /** Field errors, readiness checks and next runs; no side effects. */
  validate(draft: AutomationDraft): Promise<AutomationValidateResult>;
  /** Read-only test run (AP-4); files are never changed. */
  test(input: TestInput): Promise<AutomationTestResult>;
  /** Approve and enable after a passed test; the backend shows the native confirmation. */
  approve(input: ApproveInput): Promise<ApproveResult>;
  /** Enable = resume (native confirmation); disable = pause (no dialog). */
  setEnabled(id: string, enabled: boolean): Promise<SetEnabledResult>;
  /** Pause (configuration part); safe direction, no dialog. */
  pause(id: string, mode: PauseMode): Promise<{ status: 'paused' }>;
  /** Delete; the caller confirms in its own dialog (safe direction). */
  delete(id: string): Promise<{ status: 'deleted' }>;
  /** Every automation with status, last result and button flags (read-only). */
  list(): Promise<AutomationRow[]>;
  /** The detail page of one automation (read-only). */
  get(id: string): Promise<AutomationDetail>;
  /** Run history, newest first; page with `next` (read-only). */
  history(query: HistoryQuery): Promise<HistoryPage>;
  /** Stored operations of one run, paged (read-only). */
  runOps(runId: number, offset: number, limit: number): Promise<RunOpsPage>;
  /** The event log of one automation, newest first; page with `next` (read-only). */
  events(id: string, before: number | undefined, limit: number): Promise<EventsPage>;
}

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(cmd, args);
  } catch (e) {
    const code = typeof e === 'object' && e !== null && 'code' in e ? String(e.code) : 'unknown';
    const detail =
      typeof e === 'object' && e !== null && 'message' in e && typeof e.message === 'string'
        ? e.message
        : undefined;
    throw new AutomationError(code, detail);
  }
}

const tauriBackend: AutomationBackend = {
  async validate(draft) {
    return call<AutomationValidateResult>('automation_validate', { draft });
  },
  async test(input) {
    return call<AutomationTestResult>('automation_test', {
      id: input.id ?? null,
      draft: input.draft,
    });
  },
  async approve(input) {
    // Flat arguments, never a "confirmed" flag or dialog text: the confirmation is backend-only.
    return call<ApproveResult>('automation_approve', {
      id: input.id ?? null,
      expectedConfigRev: input.expectedConfigRev ?? null,
      draft: input.draft,
      testId: input.testId,
    });
  },
  async setEnabled(id, enabled) {
    return call<SetEnabledResult>('automation_set_enabled', { id, enabled });
  },
  async pause(id, mode) {
    return call<{ status: 'paused' }>('automation_pause', { id, mode });
  },
  async delete(id) {
    return call<{ status: 'deleted' }>('automation_delete', { id });
  },
  async list() {
    return call<AutomationRow[]>('automation_list');
  },
  async get(id) {
    return call<AutomationDetail>('automation_get', { id });
  },
  async history(query) {
    return call<HistoryPage>('automation_history', {
      filter: query.filter,
      automationId: query.automationId ?? null,
      before: query.before ?? null,
      limit: query.limit,
    });
  },
  async runOps(runId, offset, limit) {
    return call<RunOpsPage>('automation_run_ops', { runId, offset, limit });
  },
  async events(id, before, limit) {
    return call<EventsPage>('automation_events', { id, before: before ?? null, limit });
  },
};

/** Browser-dev / test fallback: nothing can be approved or enabled outside the desktop app. */
const inertBackend: AutomationBackend = {
  async validate() {
    throw new AutomationError('unsupported');
  },
  async test() {
    throw new AutomationError('unsupported');
  },
  async approve() {
    throw new AutomationError('unsupported');
  },
  async setEnabled() {
    throw new AutomationError('unsupported');
  },
  async pause() {
    throw new AutomationError('unsupported');
  },
  async delete() {
    throw new AutomationError('unsupported');
  },
  async list() {
    throw new AutomationError('unsupported');
  },
  async get() {
    throw new AutomationError('unsupported');
  },
  async history() {
    throw new AutomationError('unsupported');
  },
  async runOps() {
    throw new AutomationError('unsupported');
  },
  async events() {
    throw new AutomationError('unsupported');
  },
};

function pick(): AutomationBackend {
  return isInTauri() ? tauriBackend : inertBackend;
}

/** Resolved per call so tests can flip the environment. */
export function defaultBackend(): AutomationBackend {
  return {
    validate: (d) => pick().validate(d),
    test: (i) => pick().test(i),
    approve: (i) => pick().approve(i),
    setEnabled: (id, e) => pick().setEnabled(id, e),
    pause: (id, m) => pick().pause(id, m),
    delete: (id) => pick().delete(id),
    list: () => pick().list(),
    get: (id) => pick().get(id),
    history: (q) => pick().history(q),
    runOps: (r, o, l) => pick().runOps(r, o, l),
    events: (id, b, l) => pick().events(id, b, l),
  };
}

/** Human summary of a needs-review verdict ("Profile changed (rev 3 → 4) · held", AP-6). */
export function reviewLabel(review: NeedsReview): string {
  switch (review.reason) {
    case 'profileChanged':
      return `Profile changed (rev ${review.from} → ${review.to}) · held`;
    case 'policyChanged':
      return 'Settings changed since approval · held';
    case 'notApproved':
      return 'Not approved on this device · held';
    case 'keyUnavailable':
      return 'Approval key unavailable · held';
    case 'unsupportedVersion':
      return 'Approved by a different Ferrum version · held';
  }
}

// ── Read side (P1.2, design §7.2): list, detail, history, operations, events ──

/** Chip kinds of requirements §1; the StatusChip draws an icon and a label for each, never colour alone. */
export type AutomationStatusKind =
  | 'watching'
  | 'quiet'
  | 'checking'
  | 'device'
  | 'recheck'
  | 'unsupported'
  | 'scheduled'
  | 'queued'
  | 'pending'
  | 'running'
  | 'paused'
  | 'completed'
  | 'partial'
  | 'cancelled'
  | 'skipped'
  | 'attention'
  | 'review'
  | 'warn';

/** A chip: kind, label and an optional sub-line, all built by the backend (`status::compute`). */
export interface AutomationStatus {
  kind: AutomationStatusKind;
  label: string;
  sub?: string;
}

/** File operations of one run (never filesystem events). */
export interface RunCounts {
  ok: number;
  failed: number;
  notStarted: number;
  held: number;
  skippedChanged: number;
}

/** The hold that keeps new runs of an automation back until the run is resolved (RQ-9.1). */
export interface AttentionHold {
  kind: 'partial' | 'conflict' | 'failed';
  runId: number;
  text: string;
}

export interface LastRun {
  runId: number;
  startedMs: number;
  endedMs: number;
  result: AutomationStatus;
}

/** One row of the automation list (MH-2). `canRunNow` is a hint; the backend checks again on every run. */
export interface AutomationRow {
  id: string;
  name: string;
  kind: 'schedule' | 'watch';
  profileId: string;
  profileName: string;
  /** The profile's CURRENT revision. */
  profileRevision: number;
  /** The revision the stored approval covers. */
  approvedRevision?: number;
  enabled: boolean;
  configRev: number;
  status: AutomationStatus;
  trigger: string;
  runtime: 'app' | 'background';
  runtimeNote: string;
  lastRun?: LastRun;
  canRunNow: boolean;
  /** Why Run now is dimmed. */
  runNowReason?: string;
  /** The row offers "Review changes" instead of Resolve. */
  showReview: boolean;
  review?: NeedsReview;
  attention?: AttentionHold;
}

export interface RecheckInfo {
  state: 'running' | 'inSync' | 'remaining' | 'conflict';
  remaining: number;
  text: string;
  atMs: number;
}

export type RunPhase = 'gate' | 'compare' | 'plan' | 'sync' | 'settle' | 'done';
export type RunOutcome =
  'completed' | 'partial' | 'conflict' | 'failed' | 'cancelled' | 'interrupted' | 'skipped';

/** One run: a history row, the current run or a recent run. */
export interface AutomationRun {
  id: number;
  /** Absent for a manual profile run. */
  automationId?: string;
  /** The name when the run started. */
  automationName?: string;
  profileId: string;
  profileRevision: number;
  host: 'app' | 'background' | 'manual';
  triggerKind: string;
  triggerText: string;
  mergedCount: number;
  retryOf?: number;
  startedMs: number;
  endedMs?: number;
  phase: RunPhase;
  /** Absent while the run is going. */
  outcome?: RunOutcome;
  opsTotal: number;
  counts: RunCounts;
  result: AutomationStatus;
  errorText?: string;
  note?: string;
  resolvedBy?: number;
  resolvedText?: string;
  recheck?: RecheckInfo;
  /** The automation's hold names this run right now. */
  needsAttention: boolean;
}

export type HistoryFilter = 'all' | 'attention';

export interface HistoryQuery {
  filter: HistoryFilter;
  automationId?: string;
  /** Cursor: the `next` of the previous page. */
  before?: number;
  /** 1..=100 */
  limit: number;
}

export interface HistoryPage {
  runs: AutomationRun[];
  next?: number;
}

export interface RunOp {
  seq: number;
  kind:
    | 'copyNew'
    | 'update'
    | 'createFolder'
    | 'copyFolder'
    | 'delete'
    | 'deleteToVersions'
    | 'conflict';
  /** The target side. */
  side: 'left' | 'right';
  relPath: string;
  size?: number;
  status: 'pending' | 'inProgress' | 'done' | 'failed' | 'notStarted' | 'held' | 'changed';
  errorCode?: string;
  errorText?: string;
}

export interface RunOpsPage {
  ops: RunOp[];
  /** The run's true number of operations. */
  total: number;
  /** How many have rows (the first 5 000); paging runs over this. */
  stored: number;
}

export interface AutomationEvent {
  id: number;
  atMs: number;
  kind: 'ok' | 'warn' | 'bad' | 'mute' | 'info';
  text: string;
  runId?: number;
}

export interface EventsPage {
  events: AutomationEvent[];
  next?: number;
}

export interface QueueEntry {
  id: number;
  automationId: string;
  automationName?: string;
  triggerKind: string;
  triggerText: string;
  mergedCount: number;
  mergedReasons: string[];
  enqueuedMs: number;
  retryOf?: number;
  hostClass: 'app' | 'background';
  /** The run that uses the same folders, when that is what it waits for. */
  waitsFor?: string;
}

export interface AutomationDetail {
  automation: AutomationRow;
  policy: AutomationPolicy;
  profile: { id: string; name: string; revision: number; left: string; right: string };
  pipeline: { steps: string[]; current?: number };
  quiet?: { untilMs: number; remainingSecs: number; idleSecs: number };
  pendingRerun: boolean;
  currentRun?: AutomationRun;
  waiting: QueueEntry[];
  recentRuns: AutomationRun[];
  events: EventsPage;
}

/** MH-9: automations that need a review or hold new runs - the sidebar badge. */
export function attentionBadge(rows: readonly AutomationRow[]): number {
  return rows.filter((r) => r.showReview || r.attention !== undefined).length;
}
