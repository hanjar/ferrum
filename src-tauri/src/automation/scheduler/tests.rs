//! Scheduler core tests (FER-525 / P1.4). A fake clock: every pass is handed its `now`, nothing
//! sleeps and nothing reads the machine's time or zone.

use super::*;
use crate::automation::model::{Freq, MissedPolicy, ScheduleTrigger};

fn trigger(freq: Freq, time: &str, tz: &str, missed: MissedPolicy) -> ScheduleTrigger {
    ScheduleTrigger {
        freq,
        time: time.into(),
        tz: tz.into(),
        missed,
    }
}

fn sched(freq: Freq, time: &str, tz: &str) -> Schedule {
    Schedule::from_trigger(&trigger(freq, time, tz, MissedPolicy::Skip)).expect("schedule")
}

/// Instant of a local wall time in `tz` ("2026-10-16 12:00"); a gap or fold resolves "compatibly".
fn local(tz: &str, text: &str) -> i64 {
    let dt: jiff::civil::DateTime = text.replace(' ', "T").parse().expect("civil time");
    dt.in_tz(tz).expect("zone").timestamp().as_millisecond()
}

fn utc(text: &str) -> i64 {
    text.parse::<Timestamp>().expect("utc").as_millisecond()
}

const SEC: i64 = 1_000;
const HOUR: i64 = 3_600_000;

#[test]
fn next_three_for_once_daily_and_weekly_with_week_wrap() {
    let now = local("Asia/Jakarta", "2026-10-16 12:00"); // a Friday
    let once = sched(
        Freq::Once {
            date: "2026-10-20".into(),
        },
        "09:00",
        "Asia/Jakarta",
    );
    let runs = once.next_runs(now, 3);
    assert_eq!(runs, vec![local("Asia/Jakarta", "2026-10-20 09:00")]);
    assert_eq!(once.label(runs[0]), "Tue 20 Oct, 09:00 WIB");

    let daily = sched(Freq::Daily {}, "09:00", "Asia/Jakarta");
    let runs = daily.next_runs(now, 3);
    assert_eq!(
        runs,
        vec![
            local("Asia/Jakarta", "2026-10-17 09:00"),
            local("Asia/Jakarta", "2026-10-18 09:00"),
            local("Asia/Jakarta", "2026-10-19 09:00"),
        ]
    );
    // Today's time has not come yet: today counts.
    assert_eq!(
        daily.next_runs(local("Asia/Jakarta", "2026-10-16 08:00"), 1),
        vec![local("Asia/Jakarta", "2026-10-16 09:00")]
    );

    // Monday (1) and Friday (5): Fri 12:00 -> Mon 19, Fri 23, Mon 26 (the week wraps).
    let weekly = sched(Freq::Weekly { days: vec![1, 5] }, "09:00", "Asia/Jakarta");
    let labels: Vec<String> = weekly
        .next_runs(now, 3)
        .into_iter()
        .map(|t| weekly.label(t))
        .collect();
    assert_eq!(
        labels,
        vec![
            "Mon 19 Oct, 09:00 WIB",
            "Fri 23 Oct, 09:00 WIB",
            "Mon 26 Oct, 09:00 WIB"
        ]
    );
    // Sunday (0) is a valid day and wraps to the next week.
    let sunday = sched(Freq::Weekly { days: vec![0] }, "23:59", "UTC");
    let runs = sunday.next_runs(utc("2026-10-18T23:59:30Z"), 2);
    assert_eq!(
        runs,
        vec![utc("2026-10-25T23:59:00Z"), utc("2026-11-01T23:59:00Z")]
    );
}

#[test]
fn once_validation_uses_the_zone_calendar() {
    let now = local("Asia/Jakarta", "2026-10-16 12:00");
    let once =
        |date: &str, time: &str| sched(Freq::Once { date: date.into() }, time, "Asia/Jakarta");
    assert_eq!(once("2026-10-16", "12:01").once_error(now), None);
    assert_eq!(
        once("2026-10-16", "12:00").once_error(now),
        Some(("trigger.time", "time_passed"))
    );
    assert_eq!(
        once("2026-10-16", "11:59").once_error(now),
        Some(("trigger.time", "time_passed"))
    );
    assert_eq!(
        once("2026-10-15", "23:00").once_error(now),
        Some(("trigger.date", "date_out_of_range"))
    );
    // 13 days ahead is the last day; 14 is too far.
    assert_eq!(once("2026-10-29", "00:00").once_error(now), None);
    assert_eq!(
        once("2026-10-30", "00:00").once_error(now),
        Some(("trigger.date", "date_out_of_range"))
    );
    // "Today" is the schedule's zone's today, not the machine's: 20:00 UTC on the 16th is already
    // the 17th in Jakarta.
    let late = utc("2026-10-16T20:00:00Z");
    assert_eq!(
        once("2026-10-16", "23:00").once_error(late),
        Some(("trigger.date", "date_out_of_range"))
    );
    // Daily and weekly have no once-rule.
    assert_eq!(
        sched(Freq::Daily {}, "00:00", "Asia/Jakarta").once_error(now),
        None
    );
}

#[test]
fn unknown_zone_and_bad_fields_are_refused() {
    let t = |tz: &str, time: &str| {
        Schedule::from_trigger(&trigger(Freq::Daily {}, time, tz, MissedPolicy::Skip)).err()
    };
    assert_eq!(t("Mars/Olympus", "10:00"), Some(ScheduleError::Zone));
    assert_eq!(t("Asia/Jakarta", "25:00"), Some(ScheduleError::Time));
    assert_eq!(t("Asia/Jakarta", "9:00"), Some(ScheduleError::Time));
    assert!(t("Asia/Jakarta", "23:59").is_none());
    assert!(zone_known("Europe/Amsterdam"));
    assert!(zone_known("UTC"));
    assert!(!zone_known("Not/AZone"));
    assert!(
        Schedule::from_trigger(&trigger(
            Freq::Weekly { days: vec![] },
            "10:00",
            "UTC",
            MissedPolicy::Skip
        ))
        .is_err()
    );
}

#[test]
fn amsterdam_spring_forward_skips_a_nonexistent_time_with_the_sc21_entry() {
    let s = sched(Freq::Daily {}, "02:30", "Europe/Amsterdam");
    // Sat 28 Mar 02:30 fired; the host keeps running through the night of the change.
    let mut state = SchedState {
        cursor_ms: Some(utc("2026-03-28T01:31:00Z")),
        last_fired_ms: Some(utc("2026-03-28T01:30:00Z")),
    };
    // 29 Mar: 02:00 CET jumps to 03:00 CEST, so 02:30 does not exist.
    let before = s.evaluate(state, utc("2026-03-29T00:59:00Z"), MissedPolicy::Skip);
    assert!(before.fire.is_none() && before.gaps.is_empty() && before.missed.is_empty());
    state = before.state;
    let after = s.evaluate(state, utc("2026-03-29T01:40:00Z"), MissedPolicy::Skip);
    assert!(after.fire.is_none(), "nothing runs on the gap day");
    assert!(after.missed.is_empty(), "a gap is not a missed run");
    assert_eq!(after.gaps.len(), 1);
    assert_eq!(
        after.gaps[0].text,
        "Skipped \u{2014} 02:30 doesn't exist on Sun 29 Mar in Europe/Amsterdam (daylight saving)"
    );
    // Reported once: the same state does not report it again.
    let again = s.evaluate(after.state, utc("2026-03-29T02:00:00Z"), MissedPolicy::Skip);
    assert!(again.gaps.is_empty());
    // The next real run is Monday 30 Mar 02:30 CEST.
    assert_eq!(
        after.next_ms,
        Some(local("Europe/Amsterdam", "2026-03-30 02:30"))
    );
    // The editor's list skips the gap day too.
    let runs = s.next_runs(utc("2026-03-28T12:00:00Z"), 2);
    assert_eq!(
        runs,
        vec![
            local("Europe/Amsterdam", "2026-03-30 02:30"),
            local("Europe/Amsterdam", "2026-03-31 02:30")
        ]
    );
}

#[test]
fn amsterdam_autumn_fold_runs_once_at_the_first_instant() {
    let s = sched(Freq::Daily {}, "02:30", "Europe/Amsterdam");
    // 25 Oct: 03:00 CEST falls back to 02:00 CET, so 02:30 happens twice:
    // 00:30Z (CEST) and 01:30Z (CET). The run is at the first.
    let first = utc("2026-10-25T00:30:00Z");
    let state = SchedState {
        cursor_ms: Some(utc("2026-10-24T20:00:00Z")),
        last_fired_ms: Some(utc("2026-10-24T00:30:00Z")),
    };
    let pass = s.evaluate(state, first + 5 * SEC, MissedPolicy::Skip);
    assert_eq!(pass.fire, Some(first));
    // An hour later the clock shows 02:30 again: no second run, nothing missed.
    let later = s.evaluate(
        pass.state,
        utc("2026-10-25T01:30:10Z"),
        MissedPolicy::RunOnce,
    );
    assert!(later.fire.is_none() && later.missed.is_empty() && !later.catch_up);
    assert_eq!(
        later.next_ms,
        Some(local("Europe/Amsterdam", "2026-10-26 02:30"))
    );
}

#[test]
fn new_york_gap_and_fold() {
    let s = sched(Freq::Daily {}, "02:30", "America/New_York");
    // 8 Mar 2026: 02:00 EST -> 03:00 EDT.
    let gap = s.evaluate(
        SchedState {
            cursor_ms: Some(utc("2026-03-08T04:00:00Z")),
            last_fired_ms: Some(utc("2026-03-07T07:30:00Z")),
        },
        utc("2026-03-08T08:00:00Z"),
        MissedPolicy::Skip,
    );
    assert!(gap.fire.is_none());
    assert_eq!(gap.gaps.len(), 1);
    assert_eq!(
        gap.gaps[0].text,
        "Skipped \u{2014} 02:30 doesn't exist on Sun 8 Mar in America/New_York (daylight saving)"
    );
    // 1 Nov 2026: 01:30 happens twice (05:30Z EDT, 06:30Z EST) - a 01:30 schedule runs once.
    let f = sched(Freq::Daily {}, "01:30", "America/New_York");
    let first = utc("2026-11-01T05:30:00Z");
    let pass = f.evaluate(
        SchedState {
            cursor_ms: Some(utc("2026-11-01T03:00:00Z")),
            last_fired_ms: Some(utc("2026-10-31T05:30:00Z")),
        },
        first + SEC,
        MissedPolicy::Skip,
    );
    assert_eq!(pass.fire, Some(first));
    let second = f.evaluate(pass.state, utc("2026-11-01T06:30:05Z"), MissedPolicy::Skip);
    assert!(second.fire.is_none() && second.missed.is_empty());
}

#[test]
fn jakarta_has_no_dst_at_all() {
    let s = sched(Freq::Daily {}, "02:30", "Asia/Jakarta");
    let start = local("Asia/Jakarta", "2026-01-01 00:00");
    let slots = s.slots_after(start, 366);
    assert_eq!(slots.len(), 366);
    assert!(slots.iter().all(|x| matches!(x, Slot::Run { .. })));
    // Every slot is exactly 24 h after the previous one.
    for w in slots.windows(2) {
        assert_eq!(w[1].at_ms() - w[0].at_ms(), 24 * HOUR);
    }
    assert_eq!(s.label(slots[0].at_ms()), "Thu 01 Jan, 02:30 WIB");
}

#[test]
fn host_down_for_three_occurrences_skip_writes_one_entry_runonce_adds_one_catch_up() {
    let s = sched(Freq::Daily {}, "10:00", "Asia/Jakarta");
    let state = SchedState {
        cursor_ms: Some(local("Asia/Jakarta", "2026-10-10 11:00")),
        last_fired_ms: Some(local("Asia/Jakarta", "2026-10-10 10:00")),
    };
    // Back on the 13th at 10:30: the 11th, 12th and 13th (30 min late) are all missed.
    let now = local("Asia/Jakarta", "2026-10-13 10:30");
    let skip = s.evaluate(state, now, MissedPolicy::Skip);
    assert_eq!(skip.missed.len(), 3);
    assert!(skip.fire.is_none() && !skip.catch_up);
    assert_eq!(
        missed_text(skip.missed.len(), MissedPolicy::Skip, "Ferrum was closed"),
        "Missed 3 scheduled runs \u{2014} Ferrum was closed. Policy: skip; waits for the next time."
    );
    let once = s.evaluate(state, now, MissedPolicy::RunOnce);
    assert_eq!(once.missed.len(), 3, "the same Skipped entry");
    assert!(once.catch_up, "plus exactly one catch-up");
    assert!(once.fire.is_none());
    // Handled: the same state does not report them again, and nothing piles up.
    let next = s.evaluate(once.state, now + 60 * SEC, MissedPolicy::RunOnce);
    assert!(next.missed.is_empty() && !next.catch_up && next.fire.is_none());
    assert_eq!(
        next.next_ms,
        Some(local("Asia/Jakarta", "2026-10-14 10:00"))
    );
    assert_eq!(
        missed_text(1, MissedPolicy::RunOnce, "the PC was asleep"),
        "Missed 1 scheduled run \u{2014} the PC was asleep. Policy: run once when available again."
    );
}

#[test]
fn on_time_is_within_120_seconds_inclusive() {
    let s = sched(Freq::Daily {}, "10:00", "UTC");
    let at = utc("2026-10-16T10:00:00Z");
    let base = SchedState {
        cursor_ms: Some(at - 60 * SEC),
        last_fired_ms: Some(at - 24 * HOUR),
    };
    let late = |secs: i64| s.evaluate(base, at + secs * SEC, MissedPolicy::RunOnce);
    let on_time = late(119);
    assert_eq!(on_time.fire, Some(at));
    assert!(on_time.missed.is_empty() && !on_time.catch_up);
    assert_eq!(late(120).fire, Some(at), "exactly 120 s is still on time");
    let missed = late(121);
    assert!(missed.fire.is_none());
    assert_eq!(missed.missed, vec![at]);
    assert!(missed.catch_up);
    // An on-time occurrence next to older missed ones fires; the catch-up would be redundant.
    let mixed = s.evaluate(
        SchedState {
            cursor_ms: Some(at - 3 * 24 * HOUR),
            last_fired_ms: Some(at - 3 * 24 * HOUR),
        },
        at + 30 * SEC,
        MissedPolicy::RunOnce,
    );
    assert_eq!(mixed.fire, Some(at));
    assert_eq!(mixed.missed.len(), 2);
    assert!(!mixed.catch_up);
}

#[test]
fn a_clock_moved_back_two_hours_never_fires_again() {
    let s = sched(Freq::Daily {}, "10:00", "UTC");
    let at = utc("2026-10-16T10:00:00Z");
    let mut state = SchedState {
        cursor_ms: Some(at - 60 * SEC),
        last_fired_ms: Some(at - 24 * HOUR),
    };
    let fired = s.evaluate(state, at + 10 * SEC, MissedPolicy::RunOnce);
    assert_eq!(fired.fire, Some(at));
    state = fired.state;
    // The clock is set back two hours (10:00:10 -> 08:00:10) and walks forward through 10:00 again.
    for (now, what) in [
        (at - 2 * HOUR + 10 * SEC, "right after the change"),
        (at - 10 * SEC, "just before the occurrence"),
        (at, "at the occurrence"),
        (at + 30 * SEC, "after it"),
        (at + 3 * HOUR, "much later"),
    ] {
        let pass = s.evaluate(state, now, MissedPolicy::RunOnce);
        assert!(pass.fire.is_none(), "fired twice {what}");
        assert!(pass.missed.is_empty() && !pass.catch_up, "{what}");
        state = pass.state;
    }
    // Tomorrow still fires (the mark does not freeze the schedule).
    let tomorrow = s.evaluate(state, at + 24 * HOUR + 5 * SEC, MissedPolicy::Skip);
    assert_eq!(tomorrow.fire, Some(at + 24 * HOUR));
}

#[test]
fn a_cursor_ahead_of_the_clock_hides_nothing_unhandled() {
    let s = sched(Freq::Daily {}, "10:00", "UTC");
    let at = utc("2026-10-16T10:00:00Z");
    // The stored cursor is two hours AHEAD (the clock moved back), the occurrence was handled.
    let state = SchedState {
        cursor_ms: Some(at + 2 * HOUR),
        last_fired_ms: Some(at),
    };
    let pass = s.evaluate(state, at + 30 * SEC, MissedPolicy::RunOnce);
    assert!(pass.fire.is_none() && pass.missed.is_empty());
    // Next occurrence is tomorrow's.
    assert_eq!(pass.next_ms, Some(at + 24 * HOUR));
    // The new cursor follows the clock, not the old future value.
    assert_eq!(pass.state.cursor_ms, Some(at + 30 * SEC));
}

#[test]
fn a_zone_change_recomputes_the_next_occurrence() {
    let now = utc("2026-10-16T01:00:00Z");
    let jakarta = sched(Freq::Daily {}, "10:00", "Asia/Jakarta");
    let ams = sched(Freq::Daily {}, "10:00", "Europe/Amsterdam");
    let a = jakarta.next_runs(now, 1)[0];
    let b = ams.next_runs(now, 1)[0];
    assert_eq!(a, utc("2026-10-16T03:00:00Z"));
    assert_eq!(b, utc("2026-10-16T08:00:00Z"));
    assert_ne!(a, b);
    // Moving the same state to the other zone neither fires nor misses anything by itself.
    let state = SchedState {
        cursor_ms: Some(now),
        last_fired_ms: Some(utc("2026-10-15T03:00:00Z")),
    };
    let pass = ams.evaluate(state, now + 60 * SEC, MissedPolicy::RunOnce);
    assert!(pass.fire.is_none() && pass.missed.is_empty());
    assert_eq!(pass.next_ms, Some(b));
}

#[test]
fn once_fires_once_and_then_has_no_upcoming_run() {
    let s = sched(
        Freq::Once {
            date: "2026-10-20".into(),
        },
        "09:00",
        "Asia/Jakarta",
    );
    let at = local("Asia/Jakarta", "2026-10-20 09:00");
    let state = SchedState {
        cursor_ms: Some(at - HOUR),
        last_fired_ms: None,
    };
    let early = s.evaluate(state, at - 30 * SEC, MissedPolicy::Skip);
    assert!(early.fire.is_none());
    assert_eq!(early.next_ms, Some(at));
    let pass = s.evaluate(early.state, at + 5 * SEC, MissedPolicy::Skip);
    assert_eq!(pass.fire, Some(at));
    assert_eq!(pass.next_ms, None, "SC-1.3: No upcoming run");
    let after = s.evaluate(pass.state, at + 25 * HOUR, MissedPolicy::RunOnce);
    assert!(after.fire.is_none() && after.missed.is_empty() && after.next_ms.is_none());
    // A Once whose time passed while nothing ran is handled as missed, once.
    let missed = s.evaluate(
        SchedState {
            cursor_ms: Some(at - HOUR),
            last_fired_ms: None,
        },
        at + 5 * HOUR,
        MissedPolicy::RunOnce,
    );
    assert_eq!(missed.missed, vec![at]);
    assert!(missed.catch_up);
    assert!(
        s.evaluate(missed.state, at + 6 * HOUR, MissedPolicy::RunOnce)
            .missed
            .is_empty()
    );
}

#[test]
fn a_schedule_without_a_cursor_starts_looking_at_now() {
    let s = sched(Freq::Daily {}, "10:00", "UTC");
    // Approved at 10:01: the 10:00 occurrence before the approval is neither fired nor missed.
    let now = utc("2026-10-16T10:01:00Z");
    let pass = s.evaluate(SchedState::default(), now, MissedPolicy::RunOnce);
    assert!(pass.fire.is_none() && pass.missed.is_empty() && !pass.catch_up);
    assert_eq!(pass.state.cursor_ms, Some(now));
    assert_eq!(pass.next_ms, Some(utc("2026-10-17T10:00:00Z")));
}

#[test]
fn evaluation_is_idempotent() {
    let s = sched(
        Freq::Weekly {
            days: vec![1, 3, 5],
        },
        "08:15",
        "Europe/Amsterdam",
    );
    let mut state = SchedState {
        cursor_ms: Some(utc("2026-10-01T00:00:00Z")),
        last_fired_ms: None,
    };
    let mut now = utc("2026-10-20T12:00:00Z");
    let first = s.evaluate(state, now, MissedPolicy::RunOnce);
    assert!(!first.missed.is_empty());
    state = first.state;
    for _ in 0..5 {
        now += 7 * SEC;
        let again = s.evaluate(state, now, MissedPolicy::RunOnce);
        assert!(again.fire.is_none() && again.missed.is_empty() && !again.catch_up);
        assert!(again.gaps.is_empty());
        state = again.state;
    }
}
