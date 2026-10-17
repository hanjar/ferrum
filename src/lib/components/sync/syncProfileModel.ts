/**
 * Pure helpers of the Sync profiles UI (Automation P0.5): the editor form model, the wording for
 * every backend code, and small derived facts (delete-policy availability, filter example).
 *
 * Nothing here decides anything the backend decides: validation, readiness and the delete
 * guard are computed in Rust (SP-8) and only DISPLAYED here. The form is turned into the wire
 * `draft` by `toDraft`, which also drops fields that do not apply to the chosen mode (the backend
 * rejects e.g. a `from` on a two-way profile).
 */
import type {
  FieldChange,
  FieldError,
  Readiness,
  RootIdentity,
  SyncGuardVerdict,
  SyncBlocker,
  SyncPlanSummary,
  SyncPolicyDraft,
  SyncProfileDraft,
  SyncRunResult,
} from '../../ipc/syncProfiles';

export type SyncMode = SyncPolicyDraft['mode'];
export type DeletePolicy = SyncPolicyDraft['deletePolicy'];
export type Side = 'left' | 'right';
export type RootKind = 'fixed' | 'removable' | 'network';

/** The editor's working copy: filters are plain text (one pattern per line). */
export interface ProfileForm {
  name: string;
  left: string;
  right: string;
  mode: SyncMode;
  /** oneWay / mirror only. */
  from: Side;
  /** twoWay only. */
  propagateDeletes: boolean;
  deletePolicy: DeletePolicy;
  versioningRoot: string;
  compare: SyncPolicyDraft['compare'];
  includeText: string;
  excludeText: string;
  guardEnabled: boolean;
  guardMaxCount: number;
  guardMaxPercent: number;
}

export const GUARD_DEFAULT_COUNT = 50;
export const GUARD_DEFAULT_PERCENT = 20;

/**
 * Wire limits of the guard numbers (u32 count, u8 percent). Out-of-range values are clamped to
 * the wire type so the backend answers with its range error (guard_*_range) and not with a
 * deserialization error. Anything below 1 is sent as 0 for the same reason.
 */
export const GUARD_COUNT_WIRE_MAX = 4_294_967_295;
export const GUARD_PERCENT_WIRE_MAX = 255;

export function clampGuard(n: number, max: number): number {
  if (!Number.isFinite(n)) return 0;
  return Math.min(max, Math.max(0, Math.trunc(n)));
}

export function emptyForm(): ProfileForm {
  return {
    name: '',
    left: '',
    right: '',
    mode: 'oneWay',
    from: 'left',
    propagateDeletes: false,
    deletePolicy: 'recycleBin',
    versioningRoot: '',
    compare: 'sizeAndDate',
    includeText: '',
    excludeText: '',
    guardEnabled: true,
    guardMaxCount: GUARD_DEFAULT_COUNT,
    guardMaxPercent: GUARD_DEFAULT_PERCENT,
  };
}

export function patternsOf(text: string): string[] {
  return text
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter((line) => line.length > 0);
}

/** Form -> wire draft. Fields that do not apply to the mode / policy are omitted. */
export function toDraft(form: ProfileForm): SyncProfileDraft {
  const draft: SyncProfileDraft = {
    name: form.name,
    left: form.left,
    right: form.right,
    mode: form.mode,
    deletePolicy: form.deletePolicy,
    compare: form.compare,
    include: patternsOf(form.includeText),
    exclude: patternsOf(form.excludeText),
    guard: {
      enabled: form.guardEnabled,
      maxCount: clampGuard(form.guardMaxCount, GUARD_COUNT_WIRE_MAX),
      maxPercent: clampGuard(form.guardMaxPercent, GUARD_PERCENT_WIRE_MAX),
    },
  };
  if (form.mode === 'oneWay' || form.mode === 'mirror') draft.from = form.from;
  if (form.mode === 'twoWay') draft.propagateDeletes = form.propagateDeletes;
  if (form.deletePolicy === 'versioning') draft.versioningRoot = form.versioningRoot;
  return draft;
}

/** Stored profile policy -> form. */
export function fromPolicy(name: string, p: SyncPolicyDraft): ProfileForm {
  return {
    name,
    left: p.left,
    right: p.right,
    mode: p.mode,
    from: p.from ?? 'left',
    propagateDeletes: p.propagateDeletes ?? false,
    deletePolicy: p.deletePolicy,
    versioningRoot: p.versioningRoot ?? '',
    compare: p.compare,
    includeText: p.include.join('\n'),
    excludeText: p.exclude.join('\n'),
    guardEnabled: p.guard.enabled,
    guardMaxCount: p.guard.maxCount,
    guardMaxPercent: p.guard.maxPercent,
  };
}

// ── Editor wording ──

/** SP-2.4: one sentence per mode (and side) about overwrites and deletions. */
export function modeSentence(mode: SyncMode, from: Side): string {
  const [src, dst] = from === 'left' ? ['Left', 'right'] : ['Right', 'left'];
  if (mode === 'oneWay') {
    return `${src} always wins; new and changed files are copied to the ${dst}. Extra files on the ${dst} are kept.`;
  }
  if (mode === 'mirror') {
    return `${src} always wins; the ${dst} becomes an exact copy. Extra files on the ${dst} are deleted using the delete policy.`;
  }
  return 'Only the side proven changed overwrites the other. Conflicts are held, never guessed, and deletions are propagated only if you turn that on.';
}

/** Sides on which a run of this form could delete (SP-3.1); mirrors the backend rule. */
export function receivingSides(
  form: Pick<ProfileForm, 'mode' | 'from' | 'propagateDeletes'>,
): Side[] {
  if (form.mode === 'mirror') return [form.from === 'left' ? 'right' : 'left'];
  if (form.mode === 'twoWay' && form.propagateDeletes) return ['left', 'right'];
  return [];
}

/**
 * SP-3.1 display: why the Recycle Bin card is disabled, or null when it can be chosen. Uses the
 * drive kinds the BACKEND reported for the current folders (`sync_profile_validate`); the backend
 * still refuses the save, this only explains it up front.
 */
export function recycleBinBlockReason(
  form: Pick<ProfileForm, 'mode' | 'from' | 'propagateDeletes' | 'left' | 'right'>,
  kinds: { left?: string; right?: string },
): string | null {
  for (const side of receivingSides(form)) {
    const kind = kinds[side];
    if (kind !== undefined && kind !== 'fixed') {
      const root = (side === 'left' ? form.left : form.right).trim() || `the ${side} folder`;
      return `Recycle Bin isn't available on ${root} — choose a versioning folder or permanent delete`;
    }
  }
  return null;
}

/** SP-9.2 caption under a folder: "Local disk · NTFS", "Removable drive · FAT32", "Network share". */
export function rootCaption(identity: RootIdentity | undefined, kind: string | undefined): string {
  if (!identity) return '';
  if (identity.kind === 'network') return 'Network share';
  const base =
    kind === 'removable' || identity.drive === 'removable' ? 'Removable drive' : 'Local disk';
  return identity.fsName ? `${base} · ${identity.fsName}` : base;
}

/** SP-4.5: "Excluded: 3 patterns · e.g. `node_modules\`, `*.tmp`". */
export function excludeExample(patterns: string[]): string {
  if (patterns.length === 0) return 'Excluded: no patterns';
  const noun = patterns.length === 1 ? 'pattern' : 'patterns';
  const shown = patterns
    .slice(0, 2)
    .map((p) => `\`${p}\``)
    .join(', ');
  return `Excluded: ${patterns.length} ${noun} · e.g. ${shown}`;
}

const FIELD_MESSAGES: Record<string, string> = {
  name_required: 'Enter a name for this profile.',
  name_too_long: 'The name can have at most 80 characters.',
  name_taken: 'Another profile already uses this name.',
  name_control: "The name can't contain control characters.",
  roots_identical: 'The two folders are the same folder.',
  roots_nested:
    'One folder is inside the other; synchronizing them would copy a folder into itself.',
  root_required: 'Choose a folder.',
  root_too_long: 'This path is too long.',
  root_invalid:
    'Use a full drive path such as D:\\Photos, or a network path such as \\\\nas\\photos.',
  root_unavailable: "This folder can't be reached right now.",
  root_missing: "This folder doesn't exist.",
  root_protected_folder: "This folder holds Ferrum's own settings and can't be synchronized.",
  root_contains_protected_folder:
    "This folder contains Ferrum's own settings, and the profile writes to it. Choose a narrower folder.",
  root_startup_folder: "This is a Windows Startup folder; Ferrum doesn't write there.",
  root_contains_startup_folder:
    'This folder contains the Windows Startup folder, and the profile writes to it. Choose a narrower folder.',
  root_local_share: 'Use the local path instead of a share pointing at this computer.',
  side_required: 'Choose a direction.',
  side_not_allowed: 'Two-way profiles have no direction.',
  propagate_not_allowed: 'Deletion propagation applies to two-way profiles only.',
  filters_too_many: 'Use at most 64 patterns.',
  filter_too_long: 'Each pattern can have at most 260 characters.',
  filter_control: "Patterns can't contain control characters.",
  guard_count_range: 'The item limit must be between 1 and 100000.',
  guard_percent_range: 'The percent limit must be between 1 and 100.',
  versioning_root_required: 'Choose the versioning folder.',
  versioning_root_invalid: "The versioning folder isn't usable. Use a full local or network path.",
  versioning_root_overlap: 'The versioning folder must not be inside a sync folder or contain one.',
  versioning_root_not_allowed: 'A versioning folder only applies to the versioning delete policy.',
};

/** Message of one backend validation error; `recycle_bin_unavailable` names the folder (SP-3.1). */
export function fieldMessage(err: FieldError, form?: Pick<ProfileForm, 'left' | 'right'>): string {
  if (err.code === 'recycle_bin_unavailable') {
    const root =
      err.field === 'left' || err.field === 'right'
        ? (form?.[err.field] ?? '').trim() || `the ${err.field} folder`
        : 'this folder';
    return `Recycle Bin isn't available on ${root} — choose a versioning folder or permanent delete`;
  }
  return FIELD_MESSAGES[err.code] ?? `This value was refused (${err.code}).`;
}

/** Which form control an error belongs to (the guard error covers two inputs). */
export function errorsFor(errors: FieldError[], ...fields: string[]): FieldError[] {
  return errors.filter((e) => fields.includes(e.field));
}

// ── Readiness (SP-8): display only ──

export function readinessLabel(r: Readiness): string {
  if (r.status === 'ready') return 'Ready';
  if (r.status === 'checking') return 'Checking…';
  return 'Not ready';
}

const READINESS_REASONS: Record<string, string> = {
  rootMissing: 'A sync folder is missing.',
  rootOffline: 'A sync folder is offline or cannot be reached.',
  identityUnknown:
    'A network folder was saved while offline. Open the profile and save it once the folder is reachable.',
  identityMismatch:
    'A folder is on a different drive than when the profile was saved. Open the profile and save it to accept the new drive.',
  recycleBinUnavailable:
    "Recycle Bin isn't available on a folder — choose a versioning folder or permanent delete.",
  versioningRootInvalid: "The versioning folder isn't usable.",
  rootLocalShare:
    'A folder is a share pointing at this computer. Open the profile and use the local path.',
  rootProtected:
    "A folder is, or contains, Ferrum's own settings or the Windows Startup folder. Open the profile and choose another folder.",
  profileInvalid: 'The saved profile is invalid. Open it to review.',
};

export function readinessReason(r: Readiness): string {
  if (r.status !== 'notReady') return '';
  return READINESS_REASONS[r.reason] ?? 'The profile is not ready to run.';
}

// ── IPC errors, per operation ──

export type IpcOp = 'load' | 'save' | 'delete' | 'preview' | 'run';

/** One clear sentence per closed-set backend code (design §7); `usedBy` feeds SP-9.5. */
export function ipcErrorMessage(code: string, op: IpcOp, usedBy = 0): string {
  switch (code) {
    case 'stale':
      return op === 'run'
        ? 'The preview is out of date. Preview the profile again.'
        : 'This profile was changed elsewhere. Review the current version.';
    case 'canceled':
      return 'Canceled in the confirmation dialog. Nothing was changed.';
    case 'busy':
      return 'Another confirmation dialog is open. Close it and try again. Nothing was changed.';
    case 'lease_busy':
      return 'These folders are in use by another sync. Wait for it to finish, then try again.';
    case 'lease_required':
      return 'The folders are not reserved for this sync. Try again.';
    case 'not_ready':
      return "This profile isn't ready to run. Check its folders and delete policy.";
    case 'held':
      return 'Held: the source folder is empty, so nothing would be kept. Nothing was changed.';
    case 'in_use':
      if (op === 'delete') return usedByMessage(usedBy);
      if (op === 'preview') return 'Another preview is running. Try again in a moment.';
      return 'This profile is already running.';
    case 'needs_review':
    case 'needs_attention':
      return 'This profile needs review. Open it and save it again.';
    case 'not_found':
      return 'This profile no longer exists. The list was refreshed.';
    case 'invalid_input':
      return op === 'save'
        ? 'The profile data was refused. Check the fields and try again.'
        : 'The request was refused.';
    case 'store_unavailable':
      return 'The sync profile store is unavailable. Nothing was changed.';
    case 'store_untrusted':
      return 'The sync profile store location is not trusted. Nothing was changed.';
    case 'store_read_only':
      return 'Ferrum was updated — restart Ferrum to change sync profiles.';
    case 'runtime_unavailable':
    case 'register_failed':
      return 'The background runner is unavailable.';
    case 'unsupported':
      return 'Sync profiles need the Ferrum desktop app.';
    default:
      return 'Something went wrong. Nothing was changed.';
  }
}

/** SP-9.5. */
export function usedByMessage(count: number): string {
  const n = count > 0 ? count : 1;
  return `Used by ${n} ${n === 1 ? 'automation' : 'automations'} — delete or change ${n === 1 ? 'it' : 'them'} first.`;
}

// ── Preview / run wording ──

export function summaryLine(s: SyncPlanSummary): string {
  const parts = [`${s.total} ${s.total === 1 ? 'change' : 'changes'}`];
  if (s.toRight) parts.push(`${s.toRight} to right`);
  if (s.toLeft) parts.push(`${s.toLeft} to left`);
  if (s.overwrites)
    parts.push(`${s.overwrites} ${s.overwrites === 1 ? 'overwrite' : 'overwrites'}`);
  if (s.deletes) parts.push(`${s.deletes} ${s.deletes === 1 ? 'deletion' : 'deletions'}`);
  if (s.conflicts)
    parts.push(`${s.conflicts} ${s.conflicts === 1 ? 'conflict' : 'conflicts'} held`);
  if (s.skipped) parts.push(`${s.skipped} skipped`);
  return parts.join(' · ');
}

export function blockerMessage(b: SyncBlocker): string {
  switch (b) {
    case 'truncated':
      return 'The comparison was cut short (too many entries), so it cannot be trusted.';
    case 'cancelled':
      return 'The comparison was canceled.';
    case 'errors':
      return 'Some entries could not be read, so the comparison cannot be trusted.';
  }
}

/** Guard verdict wording; confirmation itself is a native dialog the backend shows (Q4). */
export function guardMessage(g: SyncGuardVerdict): string | null {
  if (g.verdict === 'tripped') {
    return `Would delete ${g.deletes} of ${g.entries} items (${g.percent}%) on the ${g.side} — over the safety limit. Ferrum will ask you to confirm in a separate Windows dialog.`;
  }
  if (g.verdict === 'emptySource') {
    return `The ${g.empty} folder is empty. Nothing would be kept, so this profile cannot run until the folder has files again.`;
  }
  return null;
}

export function runResultMessage(r: SyncRunResult): string {
  const c = r.counts;
  const detail = c
    ? ` ${c.done} done${c.failed ? `, ${c.failed} failed` : ''}${c.skipped ? `, ${c.skipped} skipped` : ''}.`
    : '';
  switch (r.kind) {
    case 'completed':
      return `Completed.${detail}`;
    case 'nothingToDo':
      return 'Nothing to do. Both folders already match.';
    case 'partial':
      return `Finished with problems.${detail}`;
    case 'conflictHeld':
      return `Stopped: conflicts were held and nothing was resolved for you.${detail}`;
    case 'failed':
      return r.errorText ? `Failed: ${r.errorText}` : 'Failed. Nothing more was changed.';
    case 'cancelled':
      return `Canceled.${detail}`;
    case 'interrupted':
      return r.interrupted === 'rootOffline'
        ? 'Interrupted: a folder went offline.'
        : r.interrupted === 'leaseLost'
          ? 'Interrupted: the folders were taken over by another sync.'
          : 'Interrupted.';
  }
}

/** SP-6.3 row: "Right root: E:\Backups → F:\Backups". */
export function changeLine(c: FieldChange): string {
  return `${c.field}: ${c.from} → ${c.to}`;
}
