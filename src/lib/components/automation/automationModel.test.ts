import { describe, expect, it } from 'vitest';
import {
  IDLE_DEFAULT,
  QUIET_SUBLINE,
  UNSUPPORTED_ROOT_NOTE,
  WATCH_NOTIFICATION_NOTE,
  automationErrorMessage,
  clampIdle,
  clampMaxWaitMin,
  isNetworkPath,
  maxWaitText,
  previewDone,
  previewHint,
  quietLabel,
  watchPolicyLines,
  watchSummary,
  STATUS_KINDS,
  STATUS_TONE,
  addDays,
  dstNote,
  errorMessage,
  formFromDraft,
  localErrors,
  mergeErrors,
  newScheduleForm,
  relativeLabel,
  toDraft,
  zoneClock,
  zoneHasDst,
} from './automationModel';
import { PROFILE_ID } from './automationTestKit';

const NOW = new Date('2026-10-17T10:00:00Z');

const form = (over: Record<string, unknown> = {}) => ({
  ...newScheduleForm(PROFILE_ID, NOW),
  name: 'Nightly',
  tz: 'UTC',
  date: '2026-10-17',
  ...over,
});

describe('automationModel', () => {
  it('has the 18 status kinds of requirements section 1, each with a tone', () => {
    expect(STATUS_KINDS).toHaveLength(18);
    expect(new Set(STATUS_KINDS).size).toBe(18);
    for (const k of STATUS_KINDS) expect(STATUS_TONE[k]).toBeTruthy();
  });

  it('uses the exact validation texts of SC-1.1', () => {
    expect(errorMessage({ field: 'trigger.days', code: 'days_required' })).toBe(
      'Pick at least one weekday',
    );
    expect(errorMessage({ field: 'trigger.time', code: 'time_invalid' })).toBe(
      'Enter a valid time',
    );
    expect(errorMessage({ field: 'trigger.time', code: 'time_passed' })).toBe(
      'That time has already passed — pick a later date or time',
    );
  });

  it('reports a weekly schedule without a weekday, a bad time and a passed Once time', () => {
    expect(localErrors(form({ freq: 'weekly', days: [] }), NOW)).toContainEqual({
      field: 'trigger.days',
      code: 'days_required',
    });
    expect(localErrors(form({ time: '24:00' }), NOW)).toContainEqual({
      field: 'trigger.time',
      code: 'time_invalid',
    });
    expect(localErrors(form({ time: '' }), NOW)).toContainEqual({
      field: 'trigger.time',
      code: 'time_invalid',
    });
    // Same day, 09:00 is before the 10:00 clock; 11:00 is later.
    expect(localErrors(form({ freq: 'once', time: '09:00' }), NOW)).toContainEqual({
      field: 'trigger.time',
      code: 'time_passed',
    });
    expect(localErrors(form({ freq: 'once', time: '11:00' }), NOW)).toEqual([]);
    // Tomorrow at 09:00 and 13 days ahead are fine; 14 days ahead and yesterday are out of range.
    expect(localErrors(form({ freq: 'once', date: '2026-10-18', time: '09:00' }), NOW)).toEqual([]);
    expect(localErrors(form({ freq: 'once', date: '2026-10-30', time: '09:00' }), NOW)).toEqual([]);
    expect(
      localErrors(form({ freq: 'once', date: '2026-10-31', time: '09:00' }), NOW)[0].code,
    ).toBe('date_out_of_range');
    expect(
      localErrors(form({ freq: 'once', date: '2026-10-16', time: '09:00' }), NOW)[0].code,
    ).toBe('date_out_of_range');
  });

  it('checks name, profile, zone and retries', () => {
    expect(localErrors(form({ name: '  ' }), NOW)[0].code).toBe('name_required');
    expect(localErrors(form({ name: 'x'.repeat(81) }), NOW)[0].code).toBe('name_too_long');
    expect(localErrors(form({ profileId: '' }), NOW)[0].code).toBe('profile_required');
    expect(localErrors(form({ tz: 'Nowhere/Land' }), NOW)[0].code).toBe('tz_invalid');
    expect(localErrors(form({ retries: 4 }), NOW)[0].code).toBe('retries_out_of_range');
    expect(localErrors(form(), NOW)).toEqual([]);
  });

  it('merges error lists without duplicates', () => {
    const a = { field: 'name', code: 'name_required' };
    expect(mergeErrors([a], [a, { field: 'x', code: 'y' }])).toHaveLength(2);
  });

  it('round-trips a schedule draft', () => {
    const f = form({ freq: 'weekly', days: [5, 1, 1], missed: 'runOnce' });
    const d = toDraft(f);
    expect(d.policy.trigger).toEqual({
      kind: 'schedule',
      freq: { kind: 'weekly', days: [1, 5] },
      time: '02:00',
      tz: 'UTC',
      missed: 'runOnce',
    });
    expect(d.policy.runtime).toBe('app');
    expect(formFromDraft(d, NOW)?.days).toEqual([1, 5]);
  });

  it('formats relative times', () => {
    const t = Date.parse('2026-10-17T10:00:00Z');
    expect(relativeLabel(t + 9 * 3600000 + 15 * 60000, t)).toBe('in 9 h 15 min');
    expect(relativeLabel(t + 5 * 60000, t)).toBe('in 5 min');
    expect(relativeLabel(t + 3 * 3600000, t)).toBe('in 3 h');
    expect(relativeLabel(t + 10000, t)).toBe('in less than a minute');
  });

  it('writes the DST note of SC-2.3 for both zone kinds', () => {
    expect(zoneHasDst('Europe/Amsterdam', 2026)).toBe(true);
    expect(zoneHasDst('Asia/Jakarta', 2026)).toBe(false);
    expect(dstNote('Europe/Amsterdam')).toBe(
      "Daylight saving: a local time that doesn't exist is skipped; a local time that happens twice runs once.",
    );
    expect(dstNote('Asia/Jakarta')).toBe(
      'Asia/Jakarta has no daylight-saving changes. For zones that do: a nonexistent local time is skipped, a repeated one runs once.',
    );
  });

  it('computes the wall clock of a zone and calendar arithmetic', () => {
    expect(zoneClock('Asia/Jakarta', NOW)).toEqual({ date: '2026-10-17', time: '17:00' });
    expect(addDays('2026-10-30', 2)).toBe('2026-11-01');
  });
});

describe('automationModel rework R1', () => {
  it('N2: the name length counts code points like the backend', () => {
    expect(localErrors(form({ name: '\u{1F600}'.repeat(80) }), NOW)).toEqual([]);
    expect(localErrors(form({ name: '\u{1F600}'.repeat(81) }), NOW)[0].code).toBe('name_too_long');
  });

  it('N3/N8: backend error codes have texts, and an invalid zone gets no DST claim', () => {
    for (const code of ['held', 'lease_busy', 'rate_limited', 'runner_down', 'canceled']) {
      expect(automationErrorMessage(code)).not.toMatch(/Something went wrong/);
    }
    expect(dstNote('Asia/Ja')).toBe('');
  });
  describe('watch triggers (WA-1, WA-2, WA-8)', () => {
    const watchForm = (over: Record<string, unknown> = {}) => form({ kind: 'watch', ...over });

    it('clamps the idle delay to an integer of 2 to 3600 (WA-1.3)', () => {
      expect(clampIdle(1)).toBe(2);
      expect(clampIdle(0)).toBe(2);
      expect(clampIdle(-40)).toBe(2);
      expect(clampIdle(2)).toBe(2);
      expect(clampIdle(10.4)).toBe(10);
      expect(clampIdle(10.6)).toBe(11);
      expect(clampIdle(3600)).toBe(3600);
      expect(clampIdle(3601)).toBe(3600);
      expect(clampIdle(1e9)).toBe(3600);
      expect(clampIdle(Number.NaN)).toBe(IDLE_DEFAULT);
      expect(clampIdle(Number.POSITIVE_INFINITY)).toBe(IDLE_DEFAULT);
    });

    it('clamps the longest wait to 1 to 1440 minutes (WA-1.4)', () => {
      expect(clampMaxWaitMin(0)).toBe(1);
      expect(clampMaxWaitMin(1441)).toBe(1440);
      expect(clampMaxWaitMin(15)).toBe(15);
      expect(clampMaxWaitMin(Number.NaN)).toBe(15);
      expect(maxWaitText(900)).toBe('15 min');
      expect(maxWaitText(5400)).toBe('1 h 30 min');
      expect(maxWaitText(7200)).toBe('2 h');
    });

    it('round-trips a watch draft in both directions', () => {
      const f = watchForm({ watchLeft: false, watchRight: true, idleSecs: 45, maxWaitSecs: 1800 });
      const d = toDraft(f);
      expect(d.policy.trigger).toEqual({
        kind: 'watch',
        watchLeft: false,
        watchRight: true,
        onChanges: true,
        onAvailable: true,
        idleSecs: 45,
        maxWaitSecs: 1800,
      });
      const back = formFromDraft(d, NOW);
      expect(back).toMatchObject({
        kind: 'watch',
        watchLeft: false,
        watchRight: true,
        idleSecs: 45,
        maxWaitSecs: 1800,
        retries: 2,
        name: 'Nightly',
      });
      expect(toDraft(back!)).toEqual(d);
    });

    it('validates the watch fields with the backend codes and ignores schedule fields', () => {
      const codes = (f: Record<string, unknown>, un?: { left: boolean; right: boolean }) =>
        localErrors(watchForm(f), NOW, un).map((e) => e.code);
      // Schedule problems do not matter for a watch.
      expect(codes({ time: 'xx', tz: 'Nowhere/Land', freq: 'weekly', days: [] })).toEqual([]);
      expect(codes({ watchLeft: false, watchRight: false })).toEqual(['watch_root_required']);
      expect(codes({ onChanges: false, onAvailable: false })).toEqual(['watch_event_required']);
      expect(codes({ idleSecs: 1 })).toEqual(['idle_out_of_range']);
      expect(codes({ idleSecs: 3601 })).toEqual(['idle_out_of_range']);
      expect(codes({ idleSecs: 2.5 })).toEqual(['idle_out_of_range']);
      expect(codes({ idleSecs: 2 })).toEqual([]);
      expect(codes({ idleSecs: 3600 })).toEqual([]);
      expect(codes({ maxWaitSecs: 59 })).toEqual(['max_wait_out_of_range']);
      expect(codes({ maxWaitSecs: 86401 })).toEqual(['max_wait_out_of_range']);
      expect(codes({ maxWaitSecs: 60 })).toEqual([]);
      expect(codes({ maxWaitSecs: 86400 })).toEqual([]);
    });

    it('an unwatchable root is refused, and counts as no root when it is the only one', () => {
      const un = { left: true, right: false };
      expect(localErrors(watchForm(), NOW, un).map((e) => e.code)).toEqual([
        'watch_unsupported_root',
        'watch_root_required',
      ]);
      expect(
        localErrors(watchForm({ watchLeft: true, watchRight: true }), NOW, un).map((e) => e.code),
      ).toEqual(['watch_unsupported_root']);
      expect(localErrors(watchForm({ watchLeft: false, watchRight: true }), NOW, un)).toEqual([]);
    });

    it('recognizes network paths and keeps long local paths local (WA-2.3)', () => {
      expect(isNetworkPath('\\\\nas\\photos')).toBe(true);
      expect(isNetworkPath('//nas/photos')).toBe(true);
      expect(isNetworkPath('\\\\?\\UNC\\nas\\photos')).toBe(true);
      expect(isNetworkPath('\\\\?\\C:\\Photos')).toBe(false);
      expect(isNetworkPath('D:\\Photos')).toBe(false);
      expect(isNetworkPath('')).toBe(false);
    });

    it('uses the exact spec texts', () => {
      expect(quietLabel(7)).toBe('Waiting for quiet · 7 s');
      expect(QUIET_SUBLINE).toBe('Changes detected — new changes reset the countdown');
      expect(UNSUPPORTED_ROOT_NOTE).toBe(
        "No change notifications on this provider — can't be watched",
      );
      expect(WATCH_NOTIFICATION_NOTE).toBe(
        'File notifications only trigger a fresh comparison by the sync engine; they are never the list of what gets copied.',
      );
      expect(previewHint(10)).toBe('Any new change resets the countdown to 10 s');
      expect(previewDone(3)).toBe('Preview only — 3 changes batched into one run');
      expect(previewDone(1)).toBe('Preview only — 1 change batched into one run');
    });

    it('summarizes a watch like the backend row and lists the AP-3 policies', () => {
      const t = toDraft(watchForm({ watchRight: true, idleSecs: 30 })).policy.trigger;
      if (t.kind !== 'watch') throw new Error('watch expected');
      expect(watchSummary(t)).toBe(
        'Watches both folders · runs after 30 s without changes · runs when a folder comes back',
      );
      expect(watchSummary({ ...t, onAvailable: false, watchRight: false })).toBe(
        'Watches the left folder · runs after 30 s without changes',
      );
      const lines = watchPolicyLines(t).join('\n');
      expect(lines).toContain('restarts the 30 s countdown');
      expect(lines).toContain('15 min at the latest');
      expect(lines).toContain('own runs are ignored');
      expect(lines).toContain('pending rerun');
    });
  });
});
