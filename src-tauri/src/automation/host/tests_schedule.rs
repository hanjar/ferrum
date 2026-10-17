//! Host tests, part 3 (FER-525 / P1.4): the schedule hook. A fake clock drives `Host::tick()`;
//! nothing sleeps. What is pinned here:
//!
//! - an occurrence becomes a queue entry and reaches the engine only through the gate,
//! - missed runs: one Skipped entry, and ONE catch-up with the policy "run once",
//! - a device that is offline when an occurrence fires waits and starts when it is back (SC-4),
//! - the clock moving back never fires the same occurrence twice (SC-3.4),
//! - nothing is queued for an automation that is held, paused or whose approval does not verify,
//! - a merged schedule trigger cannot keep a stale Run now alive (FER-525 condition 2),
//! - the follow-ups of FER-524 (the future-stamped request, the restamp of a merged Run now).

use std::sync::atomic::Ordering;

use super::runner::{FUTURE_SKEW_MS, USER_REQUEST_TTL_MS, eligible};
use super::test_support::*;
use super::{HostClass, Recompute};
use crate::automation::model::{
    AutomationPolicy, Freq, MissedPolicy, ScheduleTrigger, Trigger,
};
use crate::automation::status::StatusKind;
use crate::automation::store::queue::{self, NewEntry, TriggerKind};
use crate::automation::store::runs::Outcome;
use crate::automation::store::runtime::{self, Attention, AttentionKind};
use crate::automation::store::{StoreError, automations};
use crate::automation::test_support::{Fixture, T0};

const SEC: i64 = 1_000;
const MIN: i64 = 60 * SEC;
const HOUR: i64 = 60 * MIN;
const DAY: i64 = 24 * HOUR;
/// T0 is 2025-10-09T08:53:20Z; the first 10:00 UTC after it:
const OCC1: i64 = 1_760_004_000_000;

fn policy(missed: MissedPolicy) -> AutomationPolicy {
    let mut p = Fixture::daily();
    p.trigger = Trigger::Schedule(ScheduleTrigger {
        freq: Freq::Daily {},
        time: "10:00".into(),
        tz: "UTC".into(),
        missed,
    });
    p
}

fn scheduled(rig: &Rig, name: &str, missed: MissedPolicy) -> (String, String) {
    let (profile, _) = rig.fx.profile(&format!("Profile of {name}"));
    let id = rig.fx.new_automation(name, &profile, policy(missed));
    (id, profile)
}

fn set_clock(rig: &Rig, ms: i64) {
    rig.world.clock.store(ms, Ordering::SeqCst);
}

fn rt(rig: &Rig, a: &str) -> runtime::RuntimeRow {
    rig.fx
        .store
        .with_read(|c| runtime::get(c, a))
        .unwrap()
        .unwrap()
}

fn set_state(rig: &Rig, a: &str, cursor: Option<i64>, last: Option<i64>) {
    rig.fx
        .store
        .with_write(|c| -> Result<(), StoreError> {
            runtime::set_schedule(c, a, cursor, last, None, rig.now())?;
            Ok(())
        })
        .unwrap();
}

fn change_seq(rig: &Rig) -> i64 {
    rig.fx.store.with_read(automations::change_seq).unwrap()
}

fn entries(rig: &Rig) -> Vec<queue::QueueEntry> {
    rig.fx.store.with_read(queue::all).unwrap()
}

// ── An occurrence reaches the engine through the gate ──

#[test]
fn an_occurrence_is_queued_and_run_through_the_gate() {
    let rig = Rig::new();
    let (a, _) = scheduled(&rig, "Daily", MissedPolicy::Skip);
    let host = rig.host(1, HostClass::App);
    tick(&host);
    // The first pass only starts the clock of the schedule: nothing fires, nothing is missed.
    let row = rt(&rig, &a);
    assert_eq!(row.schedule_cursor_ms, Some(T0));
    assert_eq!(row.next_occurrence_ms, Some(OCC1));
    assert!(rig.world.calls().is_empty() && rig.runs().is_empty());

    set_clock(&rig, OCC1 + 5 * SEC);
    tick(&host);
    let calls = rig.world.calls();
    assert_eq!(calls.len(), 1, "exactly one run");
    assert!(calls[0].holds_lease && calls[0].lease_covers, "lease held");
    assert!(calls[0].verify_loads_before > 0, "policy::verify ran in the gate");
    let runs = rig.runs();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].trigger_kind, "schedule");
    assert_eq!(runs[0].trigger_text, "Scheduled \u{b7} 10:00 UTC");
    assert_eq!(runs[0].outcome, Some(Outcome::Completed));
    let row = rt(&rig, &a);
    assert_eq!(row.last_fired_occurrence_ms, Some(OCC1));
    assert_eq!(row.next_occurrence_ms, Some(OCC1 + DAY));

    // Later passes do not run it again.
    for _ in 0..4 {
        rig.advance(30 * SEC);
        tick(&host);
    }
    assert_eq!(rig.world.calls().len(), 1);
    assert_eq!(rig.queue_len(), 0);
}

#[test]
fn a_quiet_pass_writes_nothing() {
    let rig = Rig::new();
    let (_a, _) = scheduled(&rig, "Quiet", MissedPolicy::Skip);
    let host = rig.host(1, HostClass::App);
    tick(&host);
    rig.advance(SEC);
    tick(&host);
    let before = change_seq(&rig);
    for _ in 0..5 {
        rig.advance(SEC);
        tick(&host);
    }
    // Only the periodic host beat may write; the schedule hook adds nothing in 5 s.
    assert!(change_seq(&rig) - before <= 1, "schedule pass wrote while idle");
}

#[test]
fn with_automatic_runs_off_a_schedule_trigger_is_parked_not_run() {
    let rig = Rig::new();
    crate::automation::host::set_unattended_for_tests(false);
    let (a, _) = scheduled(&rig, "Parked", MissedPolicy::Skip);
    let host = rig.host(1, HostClass::App);
    tick(&host);
    set_clock(&rig, OCC1 + 5 * SEC);
    tick(&host);
    assert!(rig.world.calls().is_empty(), "the engine was not called");
    let queued = entries(&rig);
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].kind, TriggerKind::Schedule);
    assert!(
        rig.event_texts(&a)
            .iter()
            .any(|t| t.contains("run it with Run now"))
    );
}

// ── Missed runs ──

fn host_down_for_three(rig: &Rig, a: &str) -> i64 {
    // Last fired and last looked: Oct 8 10:00 and 11:00. Back on Oct 11 at 10:30.
    set_state(rig, a, Some(OCC1 - 23 * HOUR), Some(OCC1 - DAY));
    let now = OCC1 + 2 * DAY + 30 * MIN;
    set_clock(rig, now);
    now
}

#[test]
fn missed_with_skip_writes_one_aggregated_entry_and_runs_nothing() {
    let rig = Rig::new();
    let (a, _) = scheduled(&rig, "Skip", MissedPolicy::Skip);
    let now = host_down_for_three(&rig, &a);
    let host = rig.host(1, HostClass::App);
    tick(&host);
    let runs = rig.runs();
    assert_eq!(runs.len(), 1, "one entry for all three");
    assert_eq!(runs[0].outcome, Some(Outcome::Skipped));
    assert_eq!(
        runs[0].note.as_deref(),
        Some(
            "Missed 3 scheduled runs \u{2014} Ferrum was closed. Policy: skip; waits for the next time."
        )
    );
    assert!(rig.world.calls().is_empty());
    assert_eq!(rig.queue_len(), 0, "nothing piles up");
    assert_eq!(rt(&rig, &a).next_occurrence_ms, Some(OCC1 + 3 * DAY));
    // Looking again changes nothing.
    rig.advance(10 * SEC);
    tick(&host);
    assert_eq!(rig.runs().len(), 1);
    assert_eq!(rig.now(), now + 10 * SEC);
}

#[test]
fn missed_with_run_once_adds_exactly_one_catch_up_through_the_gate() {
    let rig = Rig::new();
    let (a, _) = scheduled(&rig, "Once", MissedPolicy::RunOnce);
    host_down_for_three(&rig, &a);
    let host = rig.host(1, HostClass::App);
    tick(&host);
    let runs = rig.runs();
    assert_eq!(runs.len(), 2, "the Skipped entry and one catch-up run");
    assert_eq!(runs[0].outcome, Some(Outcome::Skipped));
    assert_eq!(
        runs[0].note.as_deref(),
        Some(
            "Missed 3 scheduled runs \u{2014} Ferrum was closed. Policy: run once when available again."
        )
    );
    assert_eq!(runs[1].trigger_kind, "catchUp");
    assert_eq!(runs[1].trigger_text, "Missed run \u{b7} catch-up");
    let calls = rig.world.calls();
    assert_eq!(calls.len(), 1);
    assert!(calls[0].holds_lease && calls[0].verify_loads_before > 0);
    // No second catch-up later.
    rig.advance(5 * MIN);
    tick(&host);
    assert_eq!(rig.world.calls().len(), 1);
}

#[test]
fn a_resume_from_sleep_names_the_sleep() {
    let rig = Rig::new();
    let (a, _) = scheduled(&rig, "Sleeper", MissedPolicy::Skip);
    let host = rig.host(1, HostClass::App);
    tick(&host);
    // The PC sleeps through the occurrence (the host does not tick) and wakes 3 h later.
    set_clock(&rig, OCC1 + 3 * HOUR);
    host.recompute_schedules(Recompute::Resumed);
    tick(&host);
    let runs = rig.runs();
    assert_eq!(runs.len(), 1);
    assert_eq!(
        runs[0].note.as_deref(),
        Some(
            "Missed 1 scheduled run \u{2014} the PC was asleep. Policy: skip; waits for the next time."
        )
    );
    assert!(rig.world.calls().is_empty());
    let _ = a;
}

// ── DST in the host path ──

#[test]
fn a_dst_gap_is_a_skipped_entry_and_no_run() {
    let rig = Rig::new();
    let (profile, _) = rig.fx.profile("Amsterdam");
    let mut p = Fixture::daily();
    p.trigger = Trigger::Schedule(ScheduleTrigger {
        freq: Freq::Daily {},
        time: "02:30".into(),
        tz: "Europe/Amsterdam".into(),
        missed: MissedPolicy::RunOnce,
    });
    let a = rig.fx.new_automation("Amsterdam", &profile, p);
    // 2026-03-29: the clocks go from 02:00 to 03:00; 02:30 does not exist.
    let before = 1_774_742_400_000; // 2026-03-29T00:00:00Z
    set_clock(&rig, before);
    let host = rig.host(1, HostClass::App);
    tick(&host);
    set_clock(&rig, before + 2 * HOUR);
    tick(&host);
    let runs = rig.runs();
    assert_eq!(runs.len(), 1);
    assert_eq!(
        runs[0].note.as_deref(),
        Some(
            "Skipped \u{2014} 02:30 doesn't exist on Sun 29 Mar in Europe/Amsterdam (daylight saving)"
        )
    );
    assert!(rig.world.calls().is_empty(), "a gap is neither run nor caught up");
    assert!(
        rig.event_texts(&a)
            .iter()
            .any(|t| t.contains("daylight saving"))
    );
}

// ── SC-4: the device is offline when the occurrence fires ──

#[test]
fn an_offline_device_holds_the_run_until_it_is_back() {
    let rig = Rig::new();
    let (a, p) = scheduled(&rig, "Away", MissedPolicy::Skip);
    let host = rig.host(1, HostClass::App);
    tick(&host);
    let right = rig.fx.profile_policy(&p).right.path;
    let away = format!("{right}-away");
    std::fs::rename(&right, &away).unwrap();

    set_clock(&rig, OCC1 + 5 * SEC);
    tick(&host);
    assert!(rig.world.calls().is_empty(), "never against a missing folder");
    assert_eq!(rig.queue_len(), 1, "queued and held");
    assert_eq!(rig.status_kind(&a), StatusKind::Device);
    // It stays held, however long, and the occurrence is not fired again.
    for _ in 0..5 {
        rig.advance(10 * SEC);
        tick(&host);
    }
    assert!(rig.world.calls().is_empty());
    assert_eq!(rig.queue_len(), 1);

    std::fs::rename(&away, &right).unwrap();
    rig.advance(6 * SEC);
    tick(&host);
    let calls = rig.world.calls();
    assert_eq!(calls.len(), 1, "starts on reconnect");
    assert!(calls[0].holds_lease);
    assert_eq!(rig.queue_len(), 0);
    assert_eq!(rig.runs().len(), 1);
    assert_eq!(rig.runs()[0].trigger_kind, "schedule");
}

// ── SC-3.4: the clock ──

#[test]
fn a_clock_moved_back_two_hours_does_not_fire_again() {
    let rig = Rig::new();
    let (_a, _) = scheduled(&rig, "Clock", MissedPolicy::RunOnce);
    let host = rig.host(1, HostClass::App);
    tick(&host);
    set_clock(&rig, OCC1 + 10 * SEC);
    tick(&host);
    assert_eq!(rig.world.calls().len(), 1);

    // The user sets the clock back 2 h; the system tells the host.
    set_clock(&rig, OCC1 - 2 * HOUR + 10 * SEC);
    host.recompute_schedules(Recompute::TimeChanged);
    tick(&host);
    // ...and the clock walks through 10:00 again.
    for t in [-30 * SEC, 0, 30 * SEC, 5 * MIN, 3 * HOUR] {
        set_clock(&rig, OCC1 + t);
        tick(&host);
    }
    assert_eq!(rig.world.calls().len(), 1, "never twice");
    assert_eq!(rig.runs().len(), 1, "nothing missed either");
    assert_eq!(rig.queue_len(), 0);
}

// ── Nothing runs, or even queues, without a valid approval ──

#[test]
fn a_held_automation_gets_no_scheduled_entry() {
    let rig = Rig::new();
    let (a, _) = scheduled(&rig, "Held", MissedPolicy::RunOnce);
    let host = rig.host(1, HostClass::App);
    tick(&host);
    rig.fx
        .store
        .with_write(|c| -> Result<(), StoreError> {
            runtime::set_attention(
                c,
                &a,
                Some(&Attention {
                    kind: AttentionKind::Failed,
                    run_id: 5,
                    text: "1 failed".into(),
                }),
                rig.now(),
            )?;
            Ok(())
        })
        .unwrap();
    set_clock(&rig, OCC1 + 5 * SEC);
    tick(&host);
    assert_eq!(rig.queue_len(), 0);
    assert!(rig.world.calls().is_empty());
    assert!(
        rig.event_texts(&a)
            .iter()
            .any(|t| t.contains("new runs are held"))
    );
    // The occurrence is consumed: releasing the hold later does not replay it.
    assert_eq!(rt(&rig, &a).last_fired_occurrence_ms, Some(OCC1));
}

#[test]
fn an_automation_whose_approval_does_not_verify_is_never_queued_or_run() {
    let rig = Rig::new();
    let (a, _) = scheduled(&rig, "Tampered", MissedPolicy::RunOnce);
    let host = rig.host(1, HostClass::App);
    tick(&host);
    // The row says enabled; the MAC is wrong. `enabled` is not an authorization.
    rig.sql("UPDATE approvals SET mac = 'not-a-mac'");
    set_clock(&rig, OCC1 + 5 * SEC);
    tick(&host);
    assert_eq!(rig.queue_len(), 0, "not even queued");
    assert!(rig.world.calls().is_empty());
    assert!(
        rig.event_texts(&a)
            .iter()
            .any(|t| t.contains("needs review"))
    );
}

#[test]
fn a_schedule_entry_queued_with_a_bad_approval_is_refused_by_the_gate() {
    let rig = Rig::new();
    let (a, p) = scheduled(&rig, "Gate", MissedPolicy::Skip);
    rig.sql("UPDATE approvals SET mac = 'not-a-mac'");
    rig.enqueue(&a, &p, TriggerKind::CatchUp, HostClass::App);
    let host = rig.host(1, HostClass::App);
    tick(&host);
    assert!(rig.world.calls().is_empty());
    assert_eq!(rig.queue_len(), 0, "refused and removed");
}

#[test]
fn a_paused_schedule_does_not_fire_and_resuming_does_not_replay() {
    let rig = Rig::new();
    let (a, _) = scheduled(&rig, "Paused", MissedPolicy::RunOnce);
    let host = rig.host(1, HostClass::App);
    tick(&host);
    rig.sql(&format!("UPDATE automations SET enabled = 0 WHERE id = '{a}'"));
    set_clock(&rig, OCC1 + 2 * DAY);
    tick(&host);
    assert!(rig.runs().is_empty() && rig.world.calls().is_empty() && rig.queue_len() == 0);
    // Resume (reset_runtime): the paused days are not "missed".
    rig.fx
        .store
        .with_write(|c| -> Result<(), StoreError> {
            automations::set_enabled(c, &a, true, rig.now())?;
            automations::reset_runtime(c, &a, "app", "scheduled", rig.now())?;
            Ok(())
        })
        .unwrap();
    rig.advance(10 * SEC);
    tick(&host);
    assert!(rig.runs().is_empty(), "no Skipped entry for the paused time");
    assert!(rig.world.calls().is_empty());
}

#[test]
fn a_host_runs_only_schedules_of_its_own_class() {
    let rig = Rig::new();
    let (a, _) = scheduled(&rig, "App only", MissedPolicy::RunOnce);
    let host = rig.host(1, HostClass::Background);
    tick(&host);
    set_clock(&rig, OCC1 + 5 * SEC);
    tick(&host);
    assert!(rig.world.calls().is_empty() && rig.queue_len() == 0);
    assert_eq!(rt(&rig, &a).schedule_cursor_ms, None, "untouched");
}

// ── Condition 2: a merged trigger cannot make a stale Run now eligible again ──

fn enqueue_at(rig: &Rig, a: &str, p: &str, kind: TriggerKind, text: &str, at: i64) {
    rig.fx
        .store
        .with_write(|conn| -> Result<(), StoreError> {
            queue::enqueue(
                conn,
                &NewEntry {
                    automation_id: a,
                    profile_id: p,
                    host_class: HostClass::App,
                    kind,
                    text,
                    retry_of: None,
                },
                at,
            )?;
            Ok(())
        })
        .unwrap();
}

fn only_entry(rig: &Rig) -> queue::QueueEntry {
    let mut all = entries(rig);
    assert_eq!(all.len(), 1);
    all.remove(0)
}

fn eligible_off(rig: &Rig, host: &super::Host<FakeDeps>, e: &queue::QueueEntry, now: i64) -> bool {
    let _ = rig;
    eligible(&host.inner.lock(), e, now, false)
}

#[test]
fn a_schedule_trigger_merged_into_a_run_now_does_not_keep_it_alive() {
    let rig = Rig::new();
    let (a, p) = scheduled(&rig, "Merge", MissedPolicy::Skip);
    let host = rig.host(1, HostClass::App);
    let t = T0;
    enqueue_at(&rig, &a, &p, TriggerKind::RunNow, "Run now", t);
    // 9 minutes later a schedule trigger merges into the user's request.
    enqueue_at(&rig, &a, &p, TriggerKind::Schedule, "Scheduled \u{b7} 10:00 UTC", t + 9 * MIN);
    let e = only_entry(&rig);
    assert_eq!(e.kind, TriggerKind::RunNow, "still the user's request");
    assert_eq!(e.enqueued_ms, t, "the user's epoch is untouched");
    assert_eq!(e.merged_count, 1);
    assert!(eligible_off(&rig, &host, &e, t + 9 * MIN));
    // Past the TTL it is stale, and the merged trigger did not renew it.
    assert!(!eligible_off(&rig, &host, &e, t + USER_REQUEST_TTL_MS + SEC));
    enqueue_at(&rig, &a, &p, TriggerKind::Schedule, "Scheduled \u{b7} 10:00 UTC", t + 12 * MIN);
    enqueue_at(&rig, &a, &p, TriggerKind::CatchUp, "Missed run \u{b7} catch-up", t + 13 * MIN);
    let e = only_entry(&rig);
    assert_eq!(e.enqueued_ms, t);
    assert!(!eligible_off(&rig, &host, &e, t + 13 * MIN));
    // Only the user's next request is a new epoch.
    enqueue_at(&rig, &a, &p, TriggerKind::RunNow, "Run now", t + 14 * MIN);
    let e = only_entry(&rig);
    assert_eq!(e.enqueued_ms, t + 14 * MIN, "restamped by the user's request");
    assert_eq!(e.kind, TriggerKind::RunNow);
    assert!(eligible_off(&rig, &host, &e, t + 14 * MIN));
}

#[test]
fn a_run_now_merged_into_a_parked_entry_starts_a_fresh_epoch() {
    let rig = Rig::new();
    let (a, p) = scheduled(&rig, "Upgrade", MissedPolicy::Skip);
    let host = rig.host(1, HostClass::App);
    let t = T0;
    // A schedule entry waits (parked) for 20 minutes; then the user clicks Run now.
    enqueue_at(&rig, &a, &p, TriggerKind::Schedule, "Scheduled \u{b7} 10:00 UTC", t);
    enqueue_at(&rig, &a, &p, TriggerKind::RunNow, "Run now", t + 20 * MIN);
    let e = only_entry(&rig);
    assert_eq!(e.kind, TriggerKind::RunNow);
    assert_eq!(e.enqueued_ms, t + 20 * MIN);
    // Without the restamp the request would have been born 20 minutes old and expired at once.
    assert!(eligible_off(&rig, &host, &e, t + 20 * MIN));
}

#[test]
fn a_user_request_stamped_in_the_future_is_stale() {
    let rig = Rig::new();
    let (a, p) = scheduled(&rig, "Future", MissedPolicy::Skip);
    let host = rig.host(1, HostClass::App);
    let t = T0;
    // The request was made at t; the clock was then set back 2 h.
    enqueue_at(&rig, &a, &p, TriggerKind::RunNow, "Run now", t);
    let e = only_entry(&rig);
    let now = t - 2 * HOUR;
    assert!(!eligible_off(&rig, &host, &e, now), "stamped 2 h ahead");
    // A small step back (inside the skew) is tolerated.
    assert!(eligible_off(&rig, &host, &e, t - FUTURE_SKEW_MS + SEC));
    assert!(!eligible_off(&rig, &host, &e, t - FUTURE_SKEW_MS - SEC));
}

#[test]
fn an_expired_user_request_is_dropped_with_an_event_after_the_clock_moved_back() {
    let rig = Rig::new();
    crate::automation::host::set_unattended_for_tests(false);
    let (a, p) = scheduled(&rig, "Dropped", MissedPolicy::Skip);
    let host = rig.host(1, HostClass::App);
    enqueue_at(&rig, &a, &p, TriggerKind::RunNow, "Run now", T0 + 3 * HOUR);
    // The clock is at T0: the stamp is 3 h ahead.
    tick(&host);
    assert!(rig.world.calls().is_empty());
    assert_eq!(rig.queue_len(), 0);
    assert!(
        rig.event_texts(&a)
            .iter()
            .any(|t| t.contains("Run now was not started in time"))
    );
}

// ── Source guards ──

/// The hook only queues. The names of the gate, the proof and the engine do not appear in it.
#[test]
fn the_schedule_hook_has_no_way_to_the_engine() {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src/automation/host/schedule.rs");
    let text = std::fs::read_to_string(path).unwrap();
    let code: String = text
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    for needle in [
        "run_gated",
        "GateKind",
        "gate(",
        "GateCtx",
        "run_engine",
        "engine::",
        "GateOk",
        "dry_run",
    ] {
        assert!(!code.contains(needle), "schedule.rs mentions {needle}");
    }
    // It queues only the two kinds a trigger source may queue, never a user kind.
    assert!(!code.contains("TriggerKind::RunNow") && !code.contains("TriggerKind::Retry"));
}

/// The scheduler core is pure: no store, no clock, no folder.
#[test]
fn the_scheduler_core_is_pure() {
    let path =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/automation/scheduler.rs");
    let text = std::fs::read_to_string(path).unwrap();
    let code: String = text
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    for needle in [
        "Store",
        "rusqlite",
        "SystemTime",
        "Timestamp::now",
        "Zoned::now",
        "std::fs",
        "std::thread",
        "sleep",
    ] {
        assert!(!code.contains(needle), "scheduler.rs mentions {needle}");
    }
}
