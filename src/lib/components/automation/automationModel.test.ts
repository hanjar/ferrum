import { describe, expect, it } from 'vitest';
import {
  automationErrorMessage,
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
    expect(
      formFromDraft(
        {
          ...d,
          policy: {
            ...d.policy,
            trigger: {
              kind: 'watch',
              watchLeft: true,
              watchRight: false,
              onChanges: true,
              onAvailable: true,
              idleSecs: 10,
              maxWaitSecs: 900,
            },
          },
        },
        NOW,
      ),
    ).toBeNull();
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
});
