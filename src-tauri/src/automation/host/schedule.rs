//! The schedule hook of the host (Automation design §9.3; FER-525 / P1.4).
//!
//! Once per pass the host asks the pure scheduler (`automation::scheduler`) what is due for each
//! enabled schedule automation of its class, and turns the answer into **queue entries and history
//! rows - nothing else**:
//!
//! - an occurrence that is on time becomes a `Schedule` entry,
//! - missed occurrences become ONE Skipped history entry (SC-3.1) and, with the policy "run once
//!   when available again", ONE `CatchUp` entry (SC-3.2),
//! - a DST gap becomes a Skipped history entry with the SC-2.1 text.
//!
//! **No shortcut to the engine.** This file never builds a `GateOk`, never calls the engine and
//! never starts a run: an entry it queues waits for `runner::start_cycle`, whose `gate()` runs
//! `policy::verify` again, probes the folders, takes the lease and applies the delete policy
//! prerequisites for THAT run (design §10.4). A source test keeps these names out of this file.
//! The entry is queued by `request::trigger_tx`, which refuses an automation whose approval does
//! not verify, that is held or paused - the same checks as Run now - so a schedule can neither
//! run nor even queue what the user has not approved.
//!
//! **A device that is offline (SC-4)** is not special here: the entry waits in the queue, the gate
//! answers "keep" (Waiting for device) and tries again every few seconds, and the run starts when
//! the folder is back. It never runs against a missing or empty folder.
//!
//! **Clock safety.** An occurrence is handled at most once (`last_fired_occurrence_ms` is written
//! in the SAME transaction as the queue entry and the history row), whatever the clock does; the
//! write only goes through when the automation, its policy and its schedule state are what the
//! pass read (compare and set), so a second writer cannot make the same occurrence fire twice.

use std::sync::Arc;

use super::{HostDeps, Inner};
use crate::automation::model::{AutomationPolicy, MissedPolicy, Trigger};
use crate::automation::scheduler::{Evaluation, SchedState, Schedule, missed_text};
use crate::automation::store::automations::{self, EventKind};
use crate::automation::store::queue::{HostClass, TriggerKind};
use crate::automation::store::runs::{self, Finish, NewRun, Outcome, RunCounts};
use crate::automation::store::{StoreError, events, profiles, runtime};

use super::request::{self, RequestError};

/// The persisted cursor is refreshed at most this often when nothing else happens.
const CURSOR_EVERY_MS: i64 = 60_000;
/// The pass looks again at least this often (the spec's `min(next - now, 60 s)`).
const MAX_WAIT_MS: i64 = 60_000;

/// Why the schedule is looked at now. The stronger reason is the one a missed-run entry names
/// (declaration order = strength).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Recompute {
    /// The store changed (an approval, an edit, a pause or a resume).
    Edited,
    /// `WM_TIMECHANGE`: the system clock was set.
    TimeChanged,
    /// `WM_SETTINGCHANGE("TimeZone")`: the system zone changed.
    ZoneChanged,
    /// `PBT_APMRESUMEAUTOMATIC`: the PC woke up.
    Resumed,
    /// This process just became the host.
    Started,
}

/// What the host remembers between passes.
#[derive(Debug, Default)]
pub(crate) struct Book {
    pub pending: Option<Recompute>,
    pub next_wake_ms: i64,
    pub last_pass_ms: Option<i64>,
}

impl Book {
    pub(crate) fn ask(&mut self, why: Recompute) {
        self.pending = Some(self.pending.map_or(why, |p| p.max(why)));
    }
}

/// Everything a pass read for one automation.
struct Job {
    id: String,
    name: String,
    profile_id: String,
    /// The stored policy text and revision the pass evaluated: compare and set.
    policy_json: String,
    config_rev: i64,
    schedule: Schedule,
    missed: MissedPolicy,
    state: SchedState,
    stored_next: Option<i64>,
    eval: Evaluation,
}

fn class_of(policy: &AutomationPolicy) -> HostClass {
    match policy.runtime {
        crate::automation::model::Runtime::App => HostClass::App,
        crate::automation::model::Runtime::Background => HostClass::Background,
    }
}

fn why_text(class: HostClass, why: Option<Recompute>) -> &'static str {
    match (why, class) {
        (Some(Recompute::Started), HostClass::App) => "Ferrum was closed",
        (Some(Recompute::Started), HostClass::Background) => "the runner was not running",
        (Some(Recompute::Resumed), _) => "the PC was asleep",
        (Some(Recompute::TimeChanged), _) => "the system clock changed",
        (Some(Recompute::ZoneChanged), _) => "the system time zone changed",
        _ => "it was not started in time",
    }
}

/// One pass. Cheap when nothing is due: it only looks at the clock and a flag.
pub(super) fn pass<D: HostDeps>(inner: &Arc<Inner<D>>, now: i64) {
    let why = {
        let mut state = inner.lock();
        if !state.hosting || state.shutdown {
            return;
        }
        let clock_back = state.sched.last_pass_ms.is_some_and(|t| now < t);
        if state.sched.pending.is_none() && !clock_back && now < state.sched.next_wake_ms {
            return;
        }
        let mut why = state.sched.pending.take();
        if clock_back && why.is_none() {
            why = Some(Recompute::TimeChanged);
        }
        state.sched.last_pass_ms = Some(now);
        // Until this pass has read the store, look again soon.
        state.sched.next_wake_ms = now.saturating_add(MAX_WAIT_MS);
        why
    };
    let Ok(jobs) = inner
        .store
        .with_read(|conn| -> Result<Vec<Job>, StoreError> { read_jobs(conn, inner.class, now) })
    else {
        // The store cannot say: try again at the next tick.
        inner.lock().sched.ask(Recompute::Edited);
        return;
    };
    let mut wake = now.saturating_add(MAX_WAIT_MS);
    for job in &jobs {
        if let Some(next) = job.eval.next_ms {
            wake = wake.min(next.max(now));
        }
    }
    let work: Vec<&Job> = jobs.iter().filter(|j| needs_write(j, now)).collect();
    if !work.is_empty() {
        let text = why_text(inner.class, why);
        let result = inner.write(|conn| -> Result<Vec<bool>, StoreError> {
            let mut applied = Vec::with_capacity(work.len());
            for job in &work {
                applied.push(apply(conn, inner, job, text, now)?);
            }
            Ok(applied)
        });
        match result {
            // A job whose rows moved under the pass is read again next tick.
            Ok(applied) if applied.iter().any(|ok| !ok) => {
                inner.lock().sched.ask(Recompute::Edited)
            }
            Ok(_) => {}
            Err(_) => inner.lock().sched.ask(Recompute::Edited),
        }
    }
    let mut state = inner.lock();
    state.sched.next_wake_ms = wake;
}

fn read_jobs(
    conn: &rusqlite::Connection,
    class: HostClass,
    now: i64,
) -> Result<Vec<Job>, StoreError> {
    let mut out = Vec::new();
    for rec in automations::list(conn)? {
        if rec.kind != "schedule" || !rec.enabled {
            continue;
        }
        let Ok(policy) = serde_json::from_str::<AutomationPolicy>(&rec.policy) else {
            continue;
        };
        let Trigger::Schedule(trigger) = &policy.trigger else {
            continue;
        };
        if class_of(&policy) != class {
            continue;
        }
        let Some(rt) = runtime::get(conn, &rec.id)? else {
            continue;
        };
        let Ok(schedule) = Schedule::from_trigger(trigger) else {
            // An unknown zone never fires; the editor refuses it before it is approved.
            continue;
        };
        let state = SchedState {
            cursor_ms: rt.schedule_cursor_ms,
            last_fired_ms: rt.last_fired_occurrence_ms,
        };
        let eval = schedule.evaluate(state, now, trigger.missed);
        out.push(Job {
            id: rec.id,
            name: rec.name,
            profile_id: rec.profile_id,
            policy_json: rec.policy,
            config_rev: rec.config_rev,
            schedule,
            missed: trigger.missed,
            state,
            stored_next: rt.next_occurrence_ms,
            eval,
        });
    }
    Ok(out)
}

/// Is there anything to write for this job? Nothing is written for a quiet pass, so an idle
/// host does not touch the store (or bump `change_seq`) every second.
fn needs_write(job: &Job, now: i64) -> bool {
    let e = &job.eval;
    e.fire.is_some()
        || e.catch_up
        || !e.missed.is_empty()
        || !e.gaps.is_empty()
        || e.next_ms != job.stored_next
        || job
            .state
            .cursor_ms
            .is_none_or(|c| now < c || now - c >= CURSOR_EVERY_MS)
}

/// Write one job inside the pass's transaction. `Ok(false)` = the rows are not what the pass read
/// (nothing was written for this job).
fn apply<D: HostDeps>(
    conn: &rusqlite::Connection,
    inner: &Arc<Inner<D>>,
    job: &Job,
    why: &str,
    now: i64,
) -> Result<bool, StoreError> {
    // Compare and set: the automation, its policy and its schedule state are as they were read.
    let Some(rec) = automations::get(conn, &job.id)? else {
        return Ok(false);
    };
    let Some(rt) = runtime::get(conn, &job.id)? else {
        return Ok(false);
    };
    if !rec.enabled
        || rec.policy != job.policy_json
        || rec.config_rev != job.config_rev
        || rt.schedule_cursor_ms != job.state.cursor_ms
        || rt.last_fired_occurrence_ms != job.state.last_fired_ms
    {
        return Ok(false);
    }
    let e = &job.eval;
    let keys = inner.deps.keys();

    for gap in &e.gaps {
        skipped_entry(conn, inner, job, &gap.text, now)?;
    }
    if !e.missed.is_empty() {
        let text = missed_text(e.missed.len(), job.missed, why);
        skipped_entry(conn, inner, job, &text, now)?;
    }
    let queued = if let Some(at) = e.fire {
        Some((
            TriggerKind::Schedule,
            format!("Scheduled \u{b7} {}", job.schedule.short_label(at)),
        ))
    } else if e.catch_up {
        Some((
            TriggerKind::CatchUp,
            "Missed run \u{b7} catch-up".to_string(),
        ))
    } else {
        None
    };
    if let Some((kind, text)) = queued {
        match request::trigger_tx(conn, keys, &job.id, kind, &text, now) {
            Ok(_) => {}
            // Not queued, and said so: the occurrence is consumed (a later approval starts
            // from its own cursor, it does not replay old occurrences).
            Err(refusal) => {
                let line = match refusal {
                    RequestError::Held(_) => {
                        "Scheduled run skipped \u{2014} new runs are held (needs attention)"
                    }
                    RequestError::NeedsReview => {
                        "Scheduled run skipped \u{2014} the automation needs review"
                    }
                    RequestError::Store(err) => return Err(err),
                    _ => "Scheduled run skipped \u{2014} it cannot be queued right now",
                };
                events::add_event(conn, &job.id, now, EventKind::Warn, line)?;
            }
        }
    }
    runtime::set_schedule(
        conn,
        &job.id,
        e.state.cursor_ms,
        e.state.last_fired_ms,
        e.next_ms,
        now,
    )?;
    Ok(true)
}

/// A Skipped history entry (no run happened, no file was touched) plus its event line.
fn skipped_entry<D: HostDeps>(
    conn: &rusqlite::Connection,
    inner: &Arc<Inner<D>>,
    job: &Job,
    note: &str,
    now: i64,
) -> Result<(), StoreError> {
    let revision = profiles::get(conn, &job.profile_id)?
        .map(|p| p.revision)
        .unwrap_or(1)
        .max(1);
    let run_id = runs::start(
        conn,
        &NewRun {
            automation_id: Some(&job.id),
            automation_name: Some(&job.name),
            profile_id: &job.profile_id,
            profile_revision: revision,
            policy_hash: None,
            host: inner.run_host(),
            trigger_kind: TriggerKind::Schedule.as_str(),
            trigger_text: "Scheduled run skipped",
            merged_count: 0,
            retry_of: None,
            started_ms: now,
        },
    )?;
    runs::finish(
        conn,
        run_id,
        &Finish {
            ended_ms: now,
            outcome: Outcome::Skipped,
            ops_total: 0,
            counts: RunCounts::default(),
            error_text: None,
            note: Some(note),
        },
    )?;
    events::add_event_for_run(conn, &job.id, now, EventKind::Mute, note, Some(run_id))?;
    Ok(())
}
