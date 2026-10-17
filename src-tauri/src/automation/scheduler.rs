//! The schedule engine (Automation design §9.3; FER-525 / P1.4): which local times a schedule
//! means, what a DST change does to them, which occurrences fired on time and which were missed.
//!
//! **Pure.** Nothing here reads a clock, the store or a folder: every function takes `now_ms`
//! (Unix epoch milliseconds) from its caller, so the tests drive a fake clock and the host decides
//! what to do with the answer. In particular nothing here can start a run - the host turns an
//! [`Evaluation`] into queue entries, and every queue entry still goes through the gate
//! (`policy::verify` per run, lease, guard).
//!
//! # Rules
//!
//! - **Zones** are IANA ids from the BUNDLED database (`jiff`, feature `tzdb-bundle-always`), never
//!   from the machine, so a schedule means the same thing everywhere (SC-2).
//! - **DST gap** (a local time that does not exist): the slot is a [`Slot::Gap`] - skipped for that
//!   day, reported once with the SC-2.1 text. **DST fold** (a local time that happens twice): ONE
//!   slot, at the first (earlier) instant (SC-2.2).
//! - **On time** = within [`ON_TIME_MS`] (120 s) after the occurrence, inclusive: 119 s late and
//!   120 s late fire, 121 s late is missed (SC-3).
//! - **Never twice (SC-3.4).** An occurrence at or before `last_fired_ms` is never handled again,
//!   whatever the clock says; `last_fired_ms` also covers occurrences that were handled as missed
//!   or as a DST gap. A clock that jumps back therefore cannot fire the same occurrence again.
//!   A clock that jumped far FORWARD and came back leaves later occurrences suppressed until real
//!   time catches up with the stored mark: the failure direction is "no run", never "extra run".
//!   The host bounds this: a mark more than 26 hours ahead of the clock (more than a zone offset or
//!   a DST shift can explain) is clamped to `now` and a Warn event is written. Trade-off: if the
//!   clock goes back by more than 26 hours after an occurrence and stays there until the false
//!   clock passes the same local time again, that occurrence can run twice. The Warn event shows it.
//! - **The cursor** (`cursor_ms`) is the last instant the scheduler looked. It only marks where
//!   missed-run detection starts after the host was away; it is clamped to `now`, so a clock that
//!   moved back cannot hide anything behind it. A schedule without a cursor (just approved or
//!   resumed) starts looking at `now`: time before the approval is not "missed".

use jiff::Timestamp;
use jiff::civil::{Date, DateTime};
use jiff::tz::{AmbiguousOffset, TimeZone};

use crate::automation::model::{Freq, MissedPolicy, ScheduleTrigger};

/// An occurrence is on time up to this long after its time (SC-3).
pub const ON_TIME_MS: i64 = 120_000;
/// Longest Once horizon the editor accepts, in days from today (SC-1).
pub const ONCE_MAX_DAYS_AHEAD: i64 = 13;
/// Days scanned when looking for the next slot (a weekly schedule finds one within 7).
const SCAN_DAYS: i64 = 400;
/// Most slots one pass looks at after being away (a daily schedule away for a year).
const MAX_PASS_SLOTS: usize = 800;

/// Why a trigger cannot be scheduled. The codes are the field codes of `FieldError`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleError {
    Time,
    /// Not an IANA zone of the bundled database.
    Zone,
    Date,
    Days,
}

impl ScheduleError {
    /// `(field, code)` as the editor shows them.
    pub fn field_code(self) -> (&'static str, &'static str) {
        match self {
            ScheduleError::Time => ("trigger.time", "time_invalid"),
            ScheduleError::Zone => ("trigger.tz", "tz_invalid"),
            ScheduleError::Date => ("trigger.date", "date_invalid"),
            ScheduleError::Days => ("trigger.days", "days_invalid"),
        }
    }
}

/// Is `name` a zone of the bundled database?
pub fn zone_known(name: &str) -> bool {
    TimeZone::get(name).is_ok()
}

#[derive(Debug, Clone)]
enum Pattern {
    Once(Date),
    Daily,
    /// Sunday = 0.
    Weekly(Vec<i8>),
}

/// A schedule ready to be asked questions.
#[derive(Debug, Clone)]
pub struct Schedule {
    pattern: Pattern,
    hour: i8,
    minute: i8,
    tz: TimeZone,
    tz_name: String,
}

/// One scheduled local time, resolved in the zone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    /// A real instant to run at (a fold resolves to its first instant).
    Run { at_ms: i64 },
    /// The local time does not exist on `date` (DST gap). `at_ms` is only an ordering instant.
    Gap { at_ms: i64, date: Date },
}

impl Slot {
    pub fn at_ms(&self) -> i64 {
        match self {
            Slot::Run { at_ms } | Slot::Gap { at_ms, .. } => *at_ms,
        }
    }
}

fn parse_time(text: &str) -> Option<(i8, i8)> {
    let b = text.as_bytes();
    if b.len() != 5 || b[2] != b':' || ![0, 1, 3, 4].iter().all(|i| b[*i].is_ascii_digit()) {
        return None;
    }
    let h = (b[0] - b'0') * 10 + (b[1] - b'0');
    let m = (b[3] - b'0') * 10 + (b[4] - b'0');
    (h < 24 && m < 60).then_some((h as i8, m as i8))
}

fn ts(ms: i64) -> Option<Timestamp> {
    Timestamp::from_millisecond(ms).ok()
}

impl Schedule {
    /// Build a schedule from the stored trigger. Everything the model checks only by syntax
    /// (zone existence) is checked here.
    pub fn from_trigger(t: &ScheduleTrigger) -> Result<Self, ScheduleError> {
        let (hour, minute) = parse_time(&t.time).ok_or(ScheduleError::Time)?;
        let tz = TimeZone::get(&t.tz).map_err(|_| ScheduleError::Zone)?;
        let pattern = match &t.freq {
            Freq::Once { date } => Pattern::Once(date.parse().map_err(|_| ScheduleError::Date)?),
            Freq::Daily {} => Pattern::Daily,
            Freq::Weekly { days } => {
                if days.is_empty() || days.iter().any(|d| *d > 6) {
                    return Err(ScheduleError::Days);
                }
                Pattern::Weekly(days.iter().map(|d| *d as i8).collect())
            }
        };
        Ok(Schedule {
            pattern,
            hour,
            minute,
            tz,
            tz_name: t.tz.clone(),
        })
    }

    pub fn zone_name(&self) -> &str {
        &self.tz_name
    }

    pub fn is_once(&self) -> bool {
        matches!(self.pattern, Pattern::Once(_))
    }

    fn matches(&self, date: Date) -> bool {
        match &self.pattern {
            Pattern::Once(d) => *d == date,
            Pattern::Daily => true,
            Pattern::Weekly(days) => days.contains(&date.weekday().to_sunday_zero_offset()),
        }
    }

    /// The zone's calendar date at `ms`.
    fn date_at(&self, ms: i64) -> Option<Date> {
        Some(ts(ms)?.to_zoned(self.tz.clone()).date())
    }

    fn resolve(&self, date: Date) -> Option<Slot> {
        let dt: DateTime = date.at(self.hour, self.minute, 0, 0);
        let amb = self.tz.to_ambiguous_timestamp(dt);
        match amb.offset() {
            AmbiguousOffset::Gap { .. } => {
                let at = amb.compatible().ok()?;
                Some(Slot::Gap {
                    at_ms: at.as_millisecond(),
                    date,
                })
            }
            AmbiguousOffset::Fold { .. } => {
                let at = amb.earlier().ok()?;
                Some(Slot::Run {
                    at_ms: at.as_millisecond(),
                })
            }
            AmbiguousOffset::Unambiguous { .. } => {
                let at = amb.unambiguous().ok()?;
                Some(Slot::Run {
                    at_ms: at.as_millisecond(),
                })
            }
        }
    }

    /// Slots strictly after `after_ms`, oldest first, at most `limit`.
    pub fn slots_after(&self, after_ms: i64, limit: usize) -> Vec<Slot> {
        let mut out = Vec::new();
        // A day back covers every zone offset and a gap's shift; the filter below is exact.
        let Some(start) = self
            .date_at(after_ms)
            .and_then(|d| d.checked_add(jiff::Span::new().days(-1)).ok())
        else {
            return out;
        };
        let mut date = start;
        for _ in 0..SCAN_DAYS {
            if out.len() >= limit {
                break;
            }
            if self.matches(date)
                && let Some(slot) = self.resolve(date)
                && slot.at_ms() > after_ms
            {
                out.push(slot);
            }
            match date.checked_add(jiff::Span::new().days(1)) {
                Ok(next) => date = next,
                Err(_) => break,
            }
        }
        out
    }

    /// The next `count` real runs after `now_ms` (SC-1.2). A DST gap day has no run.
    pub fn next_runs(&self, now_ms: i64, count: usize) -> Vec<i64> {
        self.slots_after(now_ms, count.saturating_mul(2).saturating_add(4))
            .into_iter()
            .filter_map(|s| match s {
                Slot::Run { at_ms } => Some(at_ms),
                Slot::Gap { .. } => None,
            })
            .take(count)
            .collect()
    }

    /// "Tue 20 Oct, 10:00 WIB": date, local time and zone abbreviation (SC-1.2).
    pub fn label(&self, at_ms: i64) -> String {
        match ts(at_ms) {
            Some(t) => t
                .to_zoned(self.tz.clone())
                .strftime("%a %d %b, %H:%M %Z")
                .to_string(),
            None => String::new(),
        }
    }

    /// "10:00 WIB" for a trigger text ("Scheduled · 10:00 WIB").
    pub fn short_label(&self, at_ms: i64) -> String {
        match ts(at_ms) {
            Some(t) => t.to_zoned(self.tz.clone()).strftime("%H:%M %Z").to_string(),
            None => String::new(),
        }
    }

    /// SC-2.1 text of a skipped DST gap.
    pub fn gap_text(&self, date: Date) -> String {
        format!(
            "Skipped \u{2014} {:02}:{:02} doesn't exist on {} in {} (daylight saving)",
            self.hour,
            self.minute,
            date.strftime("%a %-d %b"),
            self.tz_name
        )
    }

    /// Validation of a Once schedule against the clock (SC-1, SC-1.1): the date must lie from
    /// today up to [`ONCE_MAX_DAYS_AHEAD`] days ahead in the schedule's zone, and today's time
    /// must still be ahead. The codes are the ones the editor already maps
    /// (`date_out_of_range`, `time_passed`). Other frequencies have nothing to check.
    pub fn once_error(&self, now_ms: i64) -> Option<(&'static str, &'static str)> {
        let Pattern::Once(date) = &self.pattern else {
            return None;
        };
        let now = ts(now_ms)?.to_zoned(self.tz.clone());
        let today = now.date();
        let last = today
            .checked_add(jiff::Span::new().days(ONCE_MAX_DAYS_AHEAD))
            .ok()?;
        if *date < today || *date > last {
            return Some(("trigger.date", "date_out_of_range"));
        }
        // The time itself: a Once slot at or before `now` has passed (a DST gap day has no slot
        // at all; it is not "passed" but it will be skipped - the editor warns separately).
        if let Some(Slot::Run { at_ms }) = self.resolve(*date)
            && at_ms <= now_ms
        {
            return Some(("trigger.time", "time_passed"));
        }
        None
    }
}

/// What the scheduler remembers per automation (the `automation_runtime` columns).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SchedState {
    /// The last instant the scheduler looked; `None` for a schedule that was just approved.
    pub cursor_ms: Option<i64>,
    /// The newest occurrence handled (fired, missed or DST-skipped).
    pub last_fired_ms: Option<i64>,
}

/// A DST gap to report once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GapSkip {
    pub at_ms: i64,
    pub text: String,
}

/// The answer of one pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evaluation {
    /// An occurrence that is due and on time: queue a Schedule trigger for it.
    pub fire: Option<i64>,
    /// Occurrences that passed more than [`ON_TIME_MS`] ago without being fired, oldest first
    /// (SC-3). One Skipped history entry covers all of them.
    pub missed: Vec<i64>,
    /// Policy "run once when available again" with something missed and nothing else firing:
    /// queue ONE catch-up (SC-3.2), never one per missed time.
    pub catch_up: bool,
    /// DST gaps that passed (SC-2.1), oldest first.
    pub gaps: Vec<GapSkip>,
    /// The state to store.
    pub state: SchedState,
    /// The next real run after `now` (None: "No upcoming run").
    pub next_ms: Option<i64>,
}

impl Schedule {
    /// One pass at `now_ms`. Idempotent: evaluating again with the returned `state` and the same
    /// or a later clock handles nothing twice.
    pub fn evaluate(&self, state: SchedState, now_ms: i64, missed: MissedPolicy) -> Evaluation {
        // The cursor never lies ahead of the clock (a clock that moved back).
        let cursor = state.cursor_ms.map_or(now_ms, |c| c.min(now_ms));
        let floor = state.last_fired_ms.map_or(cursor, |l| l.max(cursor));
        let mut fire: Option<i64> = None;
        let mut late: Vec<i64> = Vec::new();
        let mut gaps: Vec<GapSkip> = Vec::new();
        let mut newest = state.last_fired_ms;
        let mut note = |at: i64| newest = Some(newest.map_or(at, |n| n.max(at)));
        for slot in self.slots_after(floor, MAX_PASS_SLOTS) {
            if slot.at_ms() > now_ms {
                break;
            }
            match slot {
                Slot::Gap { at_ms, date } => {
                    gaps.push(GapSkip {
                        at_ms,
                        text: self.gap_text(date),
                    });
                    note(at_ms);
                }
                Slot::Run { at_ms } => {
                    note(at_ms);
                    if now_ms - at_ms <= ON_TIME_MS {
                        fire = Some(at_ms);
                    } else {
                        late.push(at_ms);
                    }
                }
            }
        }
        let catch_up = missed == MissedPolicy::RunOnce && !late.is_empty() && fire.is_none();
        let new_state = SchedState {
            cursor_ms: Some(now_ms),
            last_fired_ms: newest,
        };
        let next_ms = self
            .slots_after(now_ms.max(newest.unwrap_or(i64::MIN)), 4)
            .into_iter()
            .find_map(|s| match s {
                Slot::Run { at_ms } => Some(at_ms),
                Slot::Gap { .. } => None,
            });
        Evaluation {
            fire,
            missed: late,
            catch_up,
            gaps,
            state: new_state,
            next_ms,
        }
    }
}

/// The text of the Skipped history entry for missed occurrences (SC-3.1).
pub fn missed_text(count: usize, policy: MissedPolicy, why: &str) -> String {
    let times = if count == 1 {
        "1 scheduled run".to_string()
    } else {
        format!("{count} scheduled runs")
    };
    match policy {
        MissedPolicy::Skip => {
            format!("Missed {times} \u{2014} {why}. Policy: skip; waits for the next time.")
        }
        MissedPolicy::RunOnce => {
            format!("Missed {times} \u{2014} {why}. Policy: run once when available again.")
        }
    }
}

#[cfg(test)]
mod tests;
