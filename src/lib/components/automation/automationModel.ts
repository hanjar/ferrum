/**
 * Pure helpers of the Automation editor and review (Automation P1.5): the form model of a scheduled
 * automation, local validation, the exact validation texts (SC-1.1, AP-2.4), time-zone display
 * (SC-1.2, SC-2.3) and the error texts of the IPC codes. Nothing here touches the backend and
 * nothing here can mark anything "approved".
 */
import type {
  AutomationEvent,
  AutomationRun,
  AutomationStatusKind,
  RunCounts,
  RunOp,
  AutomationConditions,
  AutomationDraft,
  AutomationFreq,
  AutomationPolicy,
  NextRun,
  ScheduleTrigger,
} from '../../ipc/automation';
import type { FieldError } from '../../ipc/syncProfiles';

export const NAME_MAX = 80;
export const MAX_RETRIES = 3;
export const ONCE_MAX_DAYS_AHEAD = 13;

/** Weekdays in display order (Monday first); `value` is the contract's number, 0 = Sunday. */
export const WEEKDAYS: { value: number; short: string; long: string }[] = [
  { value: 1, short: 'Mon', long: 'Monday' },
  { value: 2, short: 'Tue', long: 'Tuesday' },
  { value: 3, short: 'Wed', long: 'Wednesday' },
  { value: 4, short: 'Thu', long: 'Thursday' },
  { value: 5, short: 'Fri', long: 'Friday' },
  { value: 6, short: 'Sat', long: 'Saturday' },
  { value: 0, short: 'Sun', long: 'Sunday' },
];

export interface ScheduleForm {
  name: string;
  profileId: string;
  freq: 'once' | 'daily' | 'weekly';
  /** `YYYY-MM-DD` in the schedule's zone (Once). */
  date: string;
  days: number[];
  /** `HH:MM` */
  time: string;
  tz: string;
  missed: 'skip' | 'runOnce';
  retries: number;
  /** Carried through from a stored automation; the editor only offers 'app' (Background is Unavailable). */
  runtime: 'app' | 'background';
  /** Carried through unchanged: the editor has no controls for them (optional OP-1). */
  conditions: AutomationConditions;
}

export function systemZone(): string {
  try {
    return Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC';
  } catch {
    return 'UTC';
  }
}

function zoneParts(tz: string, at: Date): Record<string, string> {
  const fmt = new Intl.DateTimeFormat('en-CA', {
    timeZone: tz,
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    hourCycle: 'h23',
  });
  const out: Record<string, string> = {};
  for (const p of fmt.formatToParts(at)) out[p.type] = p.value;
  return out;
}

/** The wall clock of `at` in `tz` (`date` = `YYYY-MM-DD`, `time` = `HH:MM`). */
export function zoneClock(tz: string, at: Date): { date: string; time: string } {
  const p = zoneParts(tz, at);
  return { date: `${p.year}-${p.month}-${p.day}`, time: `${p.hour}:${p.minute}` };
}

/** `date` plus `days` calendar days (both `YYYY-MM-DD`, no time zone involved). */
export function addDays(date: string, days: number): string {
  const [y, m, d] = date.split('-').map(Number);
  const t = new Date(Date.UTC(y, m - 1, d + days));
  return t.toISOString().slice(0, 10);
}

/** `tz` when it is a valid zone id, else the system zone (a half-typed zone must not throw). */
export function safeZone(tz: string): string {
  return isValidZone(tz) ? tz : systemZone();
}

export function isValidZone(tz: string): boolean {
  if (!tz) return false;
  try {
    new Intl.DateTimeFormat('en-US', { timeZone: tz });
    return true;
  } catch {
    return false;
  }
}

/** Whether the zone changes its UTC offset during `year` (compares the offsets of 12 month starts). */
export function zoneHasDst(tz: string, year = new Date().getFullYear()): boolean {
  if (!isValidZone(tz)) return false;
  const offsets = new Set<number>();
  for (let month = 0; month < 12; month++) {
    const at = new Date(Date.UTC(year, month, 1, 12));
    const p = zoneParts(tz, at);
    const local = Date.UTC(+p.year, +p.month - 1, +p.day, +p.hour, +p.minute);
    offsets.add(local - at.getTime());
  }
  return offsets.size > 1;
}

/** SC-2.3, exact texts. */
export function dstNote(tz: string): string {
  if (!isValidZone(tz)) return '';
  return zoneHasDst(tz)
    ? "Daylight saving: a local time that doesn't exist is skipped; a local time that happens twice runs once."
    : `${tz} has no daylight-saving changes. For zones that do: a nonexistent local time is skipped, a repeated one runs once.`;
}

export function newScheduleForm(profileId = '', now: Date | number = new Date()): ScheduleForm {
  const tz = systemZone();
  return {
    name: '',
    profileId,
    freq: 'daily',
    date: zoneClock(tz, new Date(now)).date,
    days: [],
    time: '02:00',
    tz,
    missed: 'skip',
    retries: 2,
    runtime: 'app',
    conditions: { skipOnBattery: false, skipOnMetered: false },
  };
}

/** The form of a stored scheduled automation; `null` for a watch automation (P2.2 edits those). */
export function formFromDraft(
  draft: AutomationDraft,
  now: Date | number = new Date(),
): ScheduleForm | null {
  const t = draft.policy.trigger;
  if (t.kind !== 'schedule') return null;
  return {
    name: draft.name,
    profileId: draft.profileId,
    freq: t.freq.kind,
    date: t.freq.kind === 'once' ? t.freq.date : zoneClock(t.tz, new Date(now)).date,
    days: t.freq.kind === 'weekly' ? [...t.freq.days] : [],
    time: t.time,
    tz: t.tz,
    missed: t.missed,
    retries: draft.policy.retries,
    runtime: draft.policy.runtime,
    conditions: { ...draft.policy.conditions },
  };
}

/** The draft the backend validates, tests and (after a passed test) approves. */
export function toDraft(form: ScheduleForm): AutomationDraft {
  let freq: AutomationFreq;
  if (form.freq === 'once') freq = { kind: 'once', date: form.date };
  else if (form.freq === 'weekly')
    freq = { kind: 'weekly', days: [...new Set(form.days)].sort((a, b) => a - b) };
  else freq = { kind: 'daily' };
  const trigger: ScheduleTrigger = {
    kind: 'schedule',
    freq,
    time: form.time,
    tz: form.tz,
    missed: form.missed,
  };
  const policy: AutomationPolicy = {
    trigger,
    runtime: form.runtime,
    retries: form.retries,
    conditions: { ...form.conditions },
  };
  return { name: form.name.trim(), profileId: form.profileId, policy };
}

const TIME_RE = /^([01]\d|2[0-3]):[0-5]\d$/;

/**
 * Checks the editor can do without the backend. The backend validates again (and owns what needs
 * the time-zone database); the two lists are merged by `mergeErrors`.
 */
export function localErrors(form: ScheduleForm, now: Date | number = new Date()): FieldError[] {
  const errors: FieldError[] = [];
  const name = form.name.trim();
  if (!name) errors.push({ field: 'name', code: 'name_required' });
  else if ([...name].length > NAME_MAX) errors.push({ field: 'name', code: 'name_too_long' });
  if (!form.profileId) errors.push({ field: 'profileId', code: 'profile_required' });
  const timeOk = TIME_RE.test(form.time);
  if (!timeOk) errors.push({ field: 'trigger.time', code: 'time_invalid' });
  const zoneOk = isValidZone(form.tz);
  if (!zoneOk) errors.push({ field: 'trigger.tz', code: 'tz_invalid' });
  if (form.freq === 'weekly' && form.days.length === 0)
    errors.push({ field: 'trigger.days', code: 'days_required' });
  if (form.freq === 'once') {
    if (!/^\d{4}-\d{2}-\d{2}$/.test(form.date)) {
      errors.push({ field: 'trigger.date', code: 'date_invalid' });
    } else if (zoneOk) {
      const clock = zoneClock(form.tz, new Date(now));
      if (form.date < clock.date || form.date > addDays(clock.date, ONCE_MAX_DAYS_AHEAD)) {
        errors.push({ field: 'trigger.date', code: 'date_out_of_range' });
      } else if (timeOk && form.date === clock.date && form.time <= clock.time) {
        errors.push({ field: 'trigger.time', code: 'time_passed' });
      }
    }
  }
  if (form.runtime === 'background') errors.push({ field: 'runtime', code: 'runtime_unavailable' });
  if (!Number.isInteger(form.retries) || form.retries < 0 || form.retries > MAX_RETRIES)
    errors.push({ field: 'retries', code: 'retries_out_of_range' });
  return errors;
}

export function mergeErrors(...lists: FieldError[][]): FieldError[] {
  const seen = new Set<string>();
  const out: FieldError[] = [];
  for (const list of lists)
    for (const e of list) {
      const key = `${e.field}\u0000${e.code}`;
      if (seen.has(key)) continue;
      seen.add(key);
      out.push(e);
    }
  return out;
}

/** Validation texts. SC-1.1 and the weekday/time texts are the spec's exact strings. */
export function errorMessage(e: FieldError): string {
  switch (e.code) {
    case 'name_required':
      return 'Enter a name';
    case 'name_too_long':
      return `Use ${NAME_MAX} characters or fewer for the name`;
    case 'name_control':
      return 'The name contains characters that are not allowed';
    case 'profile_required':
      return 'Pick a sync profile';
    case 'days_required':
      return 'Pick at least one weekday';
    case 'days_invalid':
      return 'Pick valid weekdays';
    case 'time_invalid':
      return 'Enter a valid time';
    case 'time_passed':
    case 'date_passed':
      return 'That time has already passed — pick a later date or time';
    case 'date_invalid':
      return 'Enter a valid date';
    case 'date_out_of_range':
      return `Pick a date from today up to ${ONCE_MAX_DAYS_AHEAD} days ahead`;
    case 'tz_invalid':
      return 'Pick a valid time zone';
    case 'runtime_unavailable':
      return RUNTIME_BACKGROUND_UNAVAILABLE;
    case 'retries_out_of_range':
      return `Retries must be between 0 and ${MAX_RETRIES}`;
    case 'watch_root_required':
      return 'Pick at least one folder to watch';
    case 'watch_event_required':
      return 'Pick when the automation should run';
    case 'idle_out_of_range':
      return 'The idle delay must be between 2 and 3600 seconds';
    case 'max_wait_out_of_range':
      return 'The longest wait must be between 60 and 86400 seconds';
    case 'watch_unsupported_root':
      return "This folder can't be watched: network folders have no change notifications";
    default:
      return 'Check this setting';
  }
}

/** "Daily at 02:00 (Asia/Jakarta)" for a schedule draft; the review screen shows it. */
export function scheduleSummary(t: ScheduleTrigger): string {
  const at = `${t.time} (${t.tz})`;
  switch (t.freq.kind) {
    case 'once':
      return `Once on ${t.freq.date} at ${at}`;
    case 'daily':
      return `Daily at ${at}`;
    case 'weekly': {
      const names = WEEKDAYS.filter((d) => (t.freq as { days: number[] }).days.includes(d.value))
        .map((d) => d.short)
        .join(', ');
      return `Weekly on ${names} at ${at}`;
    }
  }
}

export function missedText(missed: 'skip' | 'runOnce'): string {
  return missed === 'skip'
    ? 'If a run is missed: skipped; it waits for the next time.'
    : 'If a run is missed: one catch-up run when Ferrum is available again.';
}

// ── Time display (SC-1.2) ──

/** "in 9 h 15 min" */
export function relativeLabel(atMs: number, nowMs: number): string {
  const mins = Math.max(0, Math.round((atMs - nowMs) / 60000));
  if (mins < 1) return 'in less than a minute';
  if (mins < 60) return `in ${mins} min`;
  const hours = Math.floor(mins / 60);
  const rest = mins % 60;
  if (hours < 48) return rest ? `in ${hours} h ${rest} min` : `in ${hours} h`;
  const days = Math.floor(hours / 24);
  const h = hours % 24;
  return h ? `in ${days} d ${h} h` : `in ${days} d`;
}

/** "= Tue 20:00 WIB": the run in the system zone, only when the schedule zone is a different zone. */
export function systemEquivalent(atMs: number, scheduleTz: string): string | null {
  const sys = systemZone();
  if (sys === scheduleTz || !isValidZone(sys)) return null;
  const text = new Intl.DateTimeFormat('en-GB', {
    timeZone: sys,
    weekday: 'short',
    hour: '2-digit',
    minute: '2-digit',
    hourCycle: 'h23',
    timeZoneName: 'short',
  })
    .format(new Date(atMs))
    .replace(',', '');
  return `= ${text}`;
}

export interface NextRunLine {
  label: string;
  relative: string;
  equivalent: string | null;
}

export function nextRunLines(next: NextRun[], tz: string, nowMs: number): NextRunLine[] {
  return next.slice(0, 3).map((n) => ({
    label: n.label,
    relative: relativeLabel(n.atMs, nowMs),
    equivalent: systemEquivalent(n.atMs, tz),
  }));
}

// ── Error texts of the IPC codes ──

export const STALE_TEXT = 'The profile or automation changed — review again';

export function automationErrorMessage(code: string): string {
  switch (code) {
    case 'unsupported':
      return 'Automation is available only in the Ferrum desktop app.';
    case 'stale':
      return STALE_TEXT;
    case 'busy':
      return 'Another confirmation dialog is open. Close it and try again. Nothing was changed.';
    case 'not_found':
      return 'This automation or its sync profile no longer exists.';
    case 'invalid_input':
      return 'Some settings are not valid. Go back and check them.';
    case 'needs_review':
      return 'Review the profile change before running';
    case 'paused':
      return 'This automation is paused. Resume it first.';
    case 'needs_attention':
    case 'held':
      return 'A held run needs your attention first.';
    case 'lease_busy':
      return 'These folders are in use by another sync. Wait for it to finish, then try again.';
    case 'rate_limited':
      return 'Too many requests. Wait a moment and try again.';
    case 'runner_down':
      return 'The background runner is not running.';
    case 'runtime_unavailable':
      return RUNTIME_BACKGROUND_UNAVAILABLE;
    case 'canceled':
      return 'Canceled in the confirmation dialog. Nothing was changed.';
    case 'not_ready':
      return "The sync profile isn't ready. Check its folders and delete policy.";
    case 'in_use':
      return 'This automation is in use right now. Try again in a moment.';
    case 'store_unavailable':
    case 'store_untrusted':
    case 'store_read_only':
      return "The automation store isn't available. Nothing was changed.";
    default:
      return 'Something went wrong. Nothing was changed.';
  }
}

/** Runtime chooser texts (RT-1, RT-1.1). */
export const RUNTIME_APP_DESC =
  "Runs at the scheduled time if Ferrum is open. Won't run while Ferrum is closed, the PC sleeps or you're signed out.";
export const RUNTIME_BACKGROUND_DESC =
  "Runs even when Ferrum is closed, as long as you're signed in to Windows. Doesn't wake a sleeping PC or run while you're signed out.";
export const RUNTIME_BACKGROUND_UNAVAILABLE =
  "The background runner isn't included in this Ferrum build.";

// ── Status chips (requirements §1) ──

export type StatusTone = 'ok' | 'info' | 'warn' | 'bad' | 'mute';

/** Every kind of requirements §1, in the table's order. */
export const STATUS_KINDS: AutomationStatusKind[] = [
  'watching',
  'quiet',
  'checking',
  'device',
  'recheck',
  'unsupported',
  'scheduled',
  'queued',
  'pending',
  'running',
  'paused',
  'completed',
  'partial',
  'cancelled',
  'skipped',
  'attention',
  'review',
  'warn',
];

export const STATUS_TONE: Record<AutomationStatusKind, StatusTone> = {
  watching: 'ok',
  quiet: 'info',
  checking: 'info',
  device: 'warn',
  recheck: 'warn',
  unsupported: 'mute',
  scheduled: 'info',
  queued: 'info',
  pending: 'info',
  running: 'info',
  paused: 'mute',
  completed: 'ok',
  partial: 'warn',
  cancelled: 'mute',
  skipped: 'mute',
  attention: 'bad',
  review: 'warn',
  warn: 'warn',
};

// ── Detail page, run progress and history (P1.6) ──

/** Operations per page of a run's operation list (RQ-5: lists longer than 200 rows are paged). */
export const OPS_PAGE = 200;

/** Operations of a run that have not been done yet: total minus succeeded, failed, held and changed. */
export function remainingOps(opsTotal: number, c: RunCounts): number {
  return Math.max(0, opsTotal - c.ok - c.failed - c.held - c.skippedChanged);
}

/** "3 succeeded · 0 failed · 3 remaining" (RQ-5). */
export function progressCounts(run: Pick<AutomationRun, 'opsTotal' | 'counts'>): string {
  const c = run.counts;
  return `${c.ok} succeeded · ${c.failed} failed · ${remainingOps(run.opsTotal, c)} remaining`;
}

/** Phase line of a running run (RQ-5). `stopping` is the backend's "Stopping after the current file". */
export function phaseText(
  run: Pick<AutomationRun, 'phase' | 'opsTotal' | 'counts'>,
  stopping = false,
): string {
  if (stopping) return 'Stopping after the current file…';
  switch (run.phase) {
    case 'gate':
    case 'compare':
    case 'plan':
      return 'Comparing folders…';
    case 'sync': {
      const done = run.counts.ok + run.counts.failed + run.counts.skippedChanged;
      return `Syncing ${Math.min(done + 1, run.opsTotal)} of ${run.opsTotal}`;
    }
    case 'settle':
      return 'Finishing up…';
    case 'done':
      return 'Finished';
  }
}

/** Share of operations that are decided, 0..1, for the progress bar (by operations). */
export function progressFraction(run: Pick<AutomationRun, 'opsTotal' | 'counts'>): number {
  if (run.opsTotal <= 0) return 0;
  const c = run.counts;
  return Math.min(1, (c.ok + c.failed + c.held + c.skippedChanged) / run.opsTotal);
}

/** True when the run is being stopped (status sub-line of the backend). */
export function isStopping(...subs: (string | undefined)[]): boolean {
  return subs.some((s) => s?.startsWith('Stopping after the current file') === true);
}

export const OP_STATUS_LABEL: Record<RunOp['status'], string> = {
  done: 'Done',
  inProgress: 'In progress',
  failed: 'Failed',
  notStarted: 'Not started',
  held: 'Held',
  pending: 'Pending',
  changed: 'Changed during the run',
};

export const OP_KIND_LABEL: Record<RunOp['kind'], string> = {
  copyNew: 'Copy new',
  update: 'Update',
  createFolder: 'Create folder',
  copyFolder: 'Copy folder',
  delete: 'Delete',
  deleteToVersions: 'Delete (keep version)',
  conflict: 'Conflict',
};

/** Tone of an operation status, for the icon and tint beside its label. */
export function opTone(status: RunOp['status']): StatusTone {
  switch (status) {
    case 'done':
      return 'ok';
    case 'failed':
      return 'bad';
    case 'held':
    case 'changed':
      return 'warn';
    case 'inProgress':
      return 'info';
    default:
      return 'mute';
  }
}

/**
 * Recheck is offered for the run the automation's hold names (RQ-10: a fresh read-only comparison
 * of the held run). Never for a run that is still going.
 */
export function canRecheck(run: AutomationRun): boolean {
  return run.needsAttention && run.endedMs !== undefined && run.recheck?.state !== 'running';
}

/**
 * "Retry remaining N" (RQ-10.2): only after a Recheck found N > 0 and the run was not a conflict.
 * A conflict is never retried; the user decides it in Sync.
 */
export function retryRemaining(run: AutomationRun): number {
  const r = run.recheck;
  if (!run.needsAttention || !r || r.state !== 'remaining' || r.remaining <= 0) return 0;
  if (run.outcome === 'conflict') return 0;
  return r.remaining;
}

/** "Review in Sync" belongs to a run that held conflicts (RQ-6). */
export function canReviewInSync(run: AutomationRun): boolean {
  return run.outcome === 'conflict' || run.counts.held > 0 || run.recheck?.state === 'conflict';
}

export const EVENT_GLYPH: Record<AutomationEvent['kind'], { glyph: string; label: string }> = {
  ok: { glyph: '✓', label: 'OK' },
  warn: { glyph: '!', label: 'Warning' },
  bad: { glyph: '✕', label: 'Problem' },
  mute: { glyph: '·', label: 'Ignored' },
  info: { glyph: 'i', label: 'Info' },
};

/** Events shown on the detail page without paging (MH-5). */
export const DETAIL_EVENTS = 14;
/** Recent runs shown on the detail page (MH-5). */
export const DETAIL_RECENT_RUNS = 4;

export function formatWhen(ms: number): string {
  return new Date(ms).toLocaleString(undefined, {
    month: 'short',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
  });
}

/** "Oct 12, 02:00 → 02:01", or "Oct 12, 02:00 → running". */
export function whenRange(startedMs: number, endedMs?: number): string {
  const end =
    endedMs === undefined
      ? 'running'
      : new Date(endedMs).toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' });
  return `${formatWhen(startedMs)} → ${end}`;
}

/** One-line summary of a run's trigger for lists. */
export function triggerWithMerge(run: Pick<AutomationRun, 'triggerText' | 'mergedCount'>): string {
  return run.mergedCount > 0 ? `${run.triggerText} (+${run.mergedCount} merged)` : run.triggerText;
}
