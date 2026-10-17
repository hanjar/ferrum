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
