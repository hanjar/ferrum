/**
 * Sync Profile IPC (Automation P0.3, design §7.1): thin typed wrappers over
 * `src-tauri/src/automation`. Errors arrive as `{ code, message }` with a closed-set code and
 * never carry paths. Outside the desktop app (plain browser dev, unit tests) an in-memory
 * backend keeps the UI usable without touching any file.
 *
 * SE-1: no wrapper ever sends a `confirmed` flag — the native confirmation lives entirely in the
 * backend, and its answer comes back as the `status` of the save result.
 */
import { invoke } from '@tauri-apps/api/core';
import { isInTauri } from './tauri';

export interface SyncProfileGuard {
  enabled: boolean;
  maxCount: number;
  maxPercent: number;
}

/** The policy exactly as the editor sends it and the backend echoes it (design §7.1 `draft`). */
export interface SyncPolicyDraft {
  left: string;
  right: string;
  mode: 'oneWay' | 'mirror' | 'twoWay';
  from?: 'left' | 'right';
  propagateDeletes?: boolean;
  deletePolicy: 'recycleBin' | 'versioning' | 'permanent';
  versioningRoot?: string;
  compare: 'sizeAndDate' | 'content';
  include: string[];
  exclude: string[];
  guard: SyncProfileGuard;
}

export interface SyncProfileDraft extends SyncPolicyDraft {
  name: string;
}

/** How the backend identified a root when the profile was saved (SP-7.1). */
export type RootIdentity =
  | {
      kind: 'local';
      volumeSerial: string;
      fileId: string;
      fsName: string;
      drive: 'fixed' | 'removable';
    }
  | {
      kind: 'network';
      uncPrefix: string;
      /** Present only when the share could be opened at save time; absent = never verified. */
      volumeSerial?: string;
      fileId?: string;
    };

export type Readiness =
  { status: 'ready' } | { status: 'checking' } | { status: 'notReady'; reason: string };

export interface ProfileSummary {
  id: string;
  name: string;
  revision: number;
  left: string;
  right: string;
  modeLabel: string;
  deleteLabel: string;
  readiness: Readiness;
  usedBy: number;
}

export interface FieldChange {
  field: string;
  from: string;
  to: string;
}

/** SP-6.3: which policy fields changed in one revision, with display values, newest first. */
export interface RevisionChanges {
  revision: number;
  changedMs: number;
  changes: FieldChange[];
}

export interface ProfileDto {
  id: string;
  name: string;
  revision: number;
  policy: SyncPolicyDraft;
  identities: { left?: RootIdentity; right?: RootIdentity };
  changes: RevisionChanges[];
  createdMs: number;
  updatedMs: number;
  readiness: Readiness;
}

export interface FieldError {
  field: string;
  code: string;
}

export interface ValidateResult {
  errors: FieldError[];
  identities: { left?: RootIdentity; right?: RootIdentity };
  kinds: { left?: string; right?: string };
}

export type SaveResult =
  { status: 'saved'; profile: ProfileDto } | { status: 'canceled' } | { status: 'stale' };

export interface SaveInput {
  id?: string;
  /** The revision the editor loaded (SP-6.2); a mismatch answers `stale`. */
  expectedRevision?: number;
  draft: SyncProfileDraft;
}

// ── Manual preview and run (Automation P0.2, design §7.1) ──

/** Plan counts of a preview (the engine's `planSummary`). */
export interface SyncPlanSummary {
  total: number;
  toRight: number;
  toLeft: number;
  overwrites: number;
  deletes: number;
  conflicts: number;
  skipped: number;
  userSkipped: number;
}

/** Verdict of the delete safety guard (SP-5), decided before any operation. */
export type SyncGuardVerdict =
  | { verdict: 'off' | 'ok' }
  | {
      verdict: 'tripped';
      side: 'left' | 'right';
      deletes: number;
      entries: number;
      percent: number;
    }
  | { verdict: 'emptySource'; empty: 'left' | 'right' };

/** Why a comparison cannot be trusted (closed codes; a run is then impossible). */
export type SyncBlocker = 'truncated' | 'cancelled' | 'errors';

export interface SyncPreview {
  /** Required by `run`; empty when `blockers` is present. */
  previewId: string;
  summary: SyncPlanSummary;
  conflicts: number;
  guard: SyncGuardVerdict;
  blockers?: SyncBlocker[];
}

export interface SyncRunInput {
  id: string;
  previewId: string;
  /** The user confirmed the overwrites listed in the preview (webview dialog). */
  confirmOverwrite: boolean;
}

export interface SyncRunStarted {
  runId: number;
}

export interface SyncRunCounts {
  total: number;
  done: number;
  failed: number;
  skipped: number;
  changed: number;
  held: number;
  notStarted: number;
}

export type SyncOpStatus =
  'notStarted' | 'running' | 'done' | 'skipped' | 'changed' | 'failed' | 'held';

export interface SyncRunProblem {
  index: number;
  name: string;
  action: string;
  status: SyncOpStatus;
  /** Closed code such as `sharing_violation`, `access_denied`, `disk_full`, `link_in_path`. */
  reason?: string;
}

export type SyncSettleState =
  | { state: 'notAttempted' }
  | { state: 'saved' | 'unchanged'; entries: number }
  | { state: 'skipped'; reason: string }
  | { state: 'saveFailed'; code: string };

/** The final result of a run. `errorText` is the documented run error text (may name a root). */
export interface SyncRunResult {
  kind:
    | 'completed'
    | 'partial'
    | 'conflictHeld'
    | 'failed'
    | 'cancelled'
    | 'interrupted'
    | 'nothingToDo';
  counts?: SyncRunCounts;
  errorCode?: string;
  errorText?: string;
  interrupted?: 'rootOffline' | 'leaseLost';
  pendingRerun: boolean;
  settle: SyncSettleState;
  problems: SyncRunProblem[];
}

/** Payload of the `sync-profile://run` event: a phase, throttled counts, or the final result. */
export interface SyncRunEvent {
  runId: number;
  phase?: 'checking' | 'comparing' | 'syncing' | 'stopping' | 'settling';
  counts?: SyncRunCounts;
  result?: SyncRunResult;
}

export const SYNC_PROFILE_RUN_EVENT = 'sync-profile://run';

/** A failed call; `code` is from the closed set of design §7 (`stale`, `not_found`, ...). */
export class SyncProfilesError extends Error {
  constructor(public code: string) {
    super(code);
    this.name = 'SyncProfilesError';
  }
}

export interface SyncProfilesBackend {
  list(): Promise<ProfileSummary[]>;
  get(id: string): Promise<ProfileDto>;
  validate(draft: SyncProfileDraft): Promise<ValidateResult>;
  save(input: SaveInput): Promise<SaveResult>;
  delete(id: string, expectedRevision: number): Promise<{ status: string }>;
  /** Read-only engine dry run (no lease, nothing touched). */
  preview(id: string): Promise<SyncPreview>;
  /** Start a previewed run; progress and the result arrive as `sync-profile://run` events. */
  run(input: SyncRunInput): Promise<SyncRunStarted>;
}

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(cmd, args);
  } catch (e) {
    const code = typeof e === 'object' && e !== null && 'code' in e ? String(e.code) : 'unknown';
    throw new SyncProfilesError(code);
  }
}

const tauriBackend: SyncProfilesBackend = {
  async list() {
    return call<ProfileSummary[]>('sync_profile_list');
  },
  async get(id) {
    return call<ProfileDto>('sync_profile_get', { id });
  },
  async validate(draft) {
    return call<ValidateResult>('sync_profile_validate', { draft });
  },
  async save(input) {
    return call<SaveResult>('sync_profile_save', {
      id: input.id ?? null,
      expectedRevision: input.expectedRevision ?? null,
      draft: input.draft,
    });
  },
  async delete(id, expectedRevision) {
    return call<{ status: string }>('sync_profile_delete', { id, expectedRevision });
  },
  async preview(id) {
    return call<SyncPreview>('sync_profile_preview', { id });
  },
  async run(input) {
    // Flat arguments, never a "confirmed" flag: the permanent-delete dialog is backend-only (SE-1).
    return call<SyncRunStarted>('sync_profile_run', {
      id: input.id,
      previewId: input.previewId,
      confirmOverwrite: input.confirmOverwrite,
    });
  },
};

/** Browser-dev / test fallback: profiles live in memory, nothing touches a store. */
export function createMemoryBackend(initial: SyncProfileDraft[] = []): SyncProfilesBackend {
  const profiles = new Map<string, ProfileDto>();
  for (const draft of initial) {
    const id = crypto.randomUUID();
    profiles.set(id, {
      id,
      name: draft.name,
      revision: 1,
      policy: { ...draft },
      identities: {},
      changes: [],
      createdMs: 0,
      updatedMs: 0,
      readiness: { status: 'checking' },
    });
  }
  return {
    async list() {
      return [...profiles.values()].map((p) => ({
        id: p.id,
        name: p.name,
        revision: p.revision,
        left: p.policy.left,
        right: p.policy.right,
        modeLabel: '',
        deleteLabel: '',
        readiness: p.readiness,
        usedBy: 0,
      }));
    },
    async get(id) {
      const p = profiles.get(id);
      if (!p) throw new SyncProfilesError('not_found');
      return p;
    },
    async validate() {
      throw new SyncProfilesError('unsupported');
    },
    async save(input) {
      const existing = input.id ? profiles.get(input.id) : undefined;
      if (input.id && input.expectedRevision !== undefined && existing) {
        if (existing.revision !== input.expectedRevision) throw new SyncProfilesError('stale');
      }
      const id = existing?.id ?? crypto.randomUUID();
      const profile: ProfileDto = {
        id,
        name: input.draft.name,
        revision: (existing?.revision ?? 0) + 1,
        policy: { ...input.draft },
        identities: existing?.identities ?? {},
        changes: existing?.changes ?? [],
        createdMs: existing?.createdMs ?? 0,
        updatedMs: 0,
        readiness: existing?.readiness ?? { status: 'checking' },
      };
      profiles.set(id, profile);
      return { status: 'saved', profile };
    },
    async preview() {
      throw new SyncProfilesError('unsupported');
    },
    async run() {
      throw new SyncProfilesError('unsupported');
    },
    async delete(id, expectedRevision) {
      const p = profiles.get(id);
      if (!p) throw new SyncProfilesError('not_found');
      if (p.revision !== expectedRevision) throw new SyncProfilesError('stale');
      profiles.delete(id);
      return { status: 'deleted' };
    },
  };
}

let memory: SyncProfilesBackend | null = null;

/** Resolved per call so tests can flip the environment. */
export function defaultBackend(): SyncProfilesBackend {
  return {
    list: () => pick().list(),
    get: (id) => pick().get(id),
    validate: (d) => pick().validate(d),
    save: (i) => pick().save(i),
    delete: (id, r) => pick().delete(id, r),
    preview: (id) => pick().preview(id),
    run: (i) => pick().run(i),
  };
}

function pick(): SyncProfilesBackend {
  if (isInTauri()) return tauriBackend;
  memory ??= createMemoryBackend();
  return memory;
}

/** Test hook: forget the in-memory fallback state. */
export function resetMemoryBackend(): void {
  memory = null;
}
