//! Host tests, part 2 (FER-524 / P1.3): the prototype flows (Run now toasts, partial → attention →
//! recheck → retry → resolved, conflicts, stop, pause), election and handover, crash recovery,
//! the background class, merged triggers, privacy of events and the recovery paths.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::request::{self, QueueAnswer, RequestError};
use super::test_support::*;
use super::{HostClass, HostEvent, RunMode};
use crate::automation::dto::PauseMode;
use crate::automation::status::StatusKind;
use crate::automation::store::leases::{self, AcquireRequest, Holder, HolderKind};
use crate::automation::store::queue::{self, TriggerKind};
use crate::automation::store::runs::{self, NewRun, Outcome, RecheckState, RunHost};
use crate::automation::store::{StoreError, controls, runtime};
use crate::sync_engine::InterruptReason;
use crate::sync_engine::execute::{Counts, EngineOutcome, Phase};

fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while !cond() {
        assert!(std::time::Instant::now() < deadline, "timed out: {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Tick until the host has nothing in flight (the clock does not move).
fn drive(host: &super::Host<FakeDeps>) {
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        host.tick();
        if host.is_idle() {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "host did not go idle");
        std::thread::sleep(Duration::from_millis(5));
    }
    settle(host);
}

/// An engine that runs until it is released or cancelled.
fn blocking(release: Arc<AtomicBool>, done: u32, total: u32) -> Script {
    script(move |_, call| {
        call.sink.phase(Phase::Comparing);
        call.sink.phase(Phase::Syncing);
        while !call.cancel.is_cancelled() && !release.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(2));
        }
        if call.cancel.is_cancelled() {
            cancelled(done, total)
        } else {
            completed(total)
        }
    })
}

fn set_attention(rig: &Rig, automation: &str, run_id: i64) {
    rig.sql(&format!(
        "UPDATE automation_runtime SET attention = '{{\"kind\":\"partial\",\"runId\":{run_id},\"text\":\"1 failed\"}}' WHERE automation_id = '{automation}'"
    ));
}

fn attention_of(rig: &Rig, automation: &str) -> Option<runtime::Attention> {
    rig.fx
        .store
        .with_read(|c| runtime::get(c, automation))
        .unwrap()
        .unwrap()
        .attention
}

fn run_row(rig: &Rig, id: i64) -> runs::RunRecord {
    rig.fx
        .store
        .with_read(|c| runs::get(c, id))
        .unwrap()
        .unwrap()
}

// ── Run now (RQ-4): the toasts ──

#[test]
fn run_now_answers_queued_then_merged() {
    let rig = Rig::new();
    let (a, _) = rig.automation("Toast");
    let first = request::run_now_command(&rig.fx.store, rig.fx.keys.as_ref(), &a, rig.now());
    assert_eq!(first, Ok(QueueAnswer::Queued));
    let second = request::run_now_command(&rig.fx.store, rig.fx.keys.as_ref(), &a, rig.now());
    assert_eq!(second, Ok(QueueAnswer::Merged));
    assert_eq!(rig.queue_len(), 1, "no duplicate job");
}

#[test]
fn run_now_says_waiting_while_another_run_uses_the_same_folders() {
    let rig = Rig::new();
    let (a, pa) = rig.automation("First");
    // A second profile on the SAME left folder.
    let left = rig.fx.profile_policy(&pa).left.path;
    let (b, _) = rig.overlapping_automation("Second", &left);
    let release = Arc::new(AtomicBool::new(false));
    rig.world.set_script(blocking(release.clone(), 0, 3));
    let host = rig.host(1, HostClass::App);
    request::run_now_command(&rig.fx.store, rig.fx.keys.as_ref(), &a, rig.now()).unwrap();
    host.tick();
    wait_until("first run active", || !host.active_runs().is_empty());
    let answer = request::run_now_command(&rig.fx.store, rig.fx.keys.as_ref(), &b, rig.now());
    assert_eq!(
        answer,
        Ok(QueueAnswer::Waiting {
            name: "First".to_string()
        })
    );
    // The status line says why it waits (RQ-3.2).
    let rows =
        crate::automation::read::list_in(&rig.fx.store, rig.fx.keys.as_ref(), rig.now()).unwrap();
    let row = rows.iter().find(|r| r.id == b).unwrap();
    assert_eq!(row.status.kind, StatusKind::Queued);
    assert!(
        row.status.sub.as_deref().unwrap_or("").contains("First"),
        "{:?}",
        row.status
    );
    // The first run ends; the waiting one starts on the next pass.
    release.store(true, Ordering::SeqCst);
    wait_until("first run ended", || host.is_idle());
    settle(&host);
    rig.world.set_script(default_script());
    drive(&host);
    let calls = rig.world.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[1].automation_id, b);
}

#[test]
fn run_now_says_device_while_a_folder_is_offline() {
    let rig = Rig::new();
    let (a, _) = rig.automation("Away");
    rig.sql(&format!(
        "UPDATE automation_runtime SET trigger_state = 'device' WHERE automation_id = '{a}'"
    ));
    let answer = request::run_now_command(&rig.fx.store, rig.fx.keys.as_ref(), &a, rig.now());
    assert_eq!(answer, Ok(QueueAnswer::Device));
}

#[test]
fn run_now_is_refused_with_the_reason_when_it_cannot_run() {
    let rig = Rig::new();
    let (a, p) = rig.automation("Refused");
    set_attention(&rig, &a, 3);
    assert_eq!(
        request::run_now_command(&rig.fx.store, rig.fx.keys.as_ref(), &a, rig.now()),
        Err(RequestError::Held("1 failed".to_string()))
    );
    rig.sql(&format!(
        "UPDATE automation_runtime SET attention = NULL WHERE automation_id = '{a}'"
    ));
    approval_pause(&rig, &a);
    assert_eq!(
        request::run_now_command(&rig.fx.store, rig.fx.keys.as_ref(), &a, rig.now()),
        Err(RequestError::Paused)
    );
    let (b, pb) = rig.automation("Reviewed");
    let rev = rig
        .fx
        .store
        .with_read(|c| crate::automation::store::profiles::get(c, &pb))
        .unwrap()
        .unwrap()
        .revision;
    rig.fx.bump_profile(&pb, rev, "*.x");
    assert_eq!(
        request::run_now_command(&rig.fx.store, rig.fx.keys.as_ref(), &b, rig.now()),
        Err(RequestError::NeedsReview)
    );
    assert_eq!(
        request::run_now_command(&rig.fx.store, rig.fx.keys.as_ref(), "nope", rig.now()),
        Err(RequestError::NotFound)
    );
    assert_eq!(rig.queue_len(), 0, "nothing was queued");
    let _ = p;
}

fn approval_pause(rig: &Rig, a: &str) {
    crate::automation::approval::pause_in(
        &rig.fx.store,
        rig.fx.keys.as_ref(),
        a,
        PauseMode::Finish,
        rig.fx.clock(),
    )
    .unwrap();
}

/// T5: a flood of controls for one target is refused.
#[test]
fn controls_are_rate_limited_per_target() {
    let rig = Rig::new();
    let (a, p) = rig.automation("Flood");
    rig.world.set_script(script(|_, _| partial(3, 1, 2)));
    let host = rig.host(1, HostClass::App);
    rig.enqueue(&a, &p, TriggerKind::RunNow, HostClass::App);
    tick(&host);
    let run_id = rig.runs()[0].id;
    for _ in 0..request::CONTROLS_PER_SECOND {
        request::recheck_command(&rig.fx.store, run_id, rig.now()).unwrap();
    }
    assert_eq!(
        request::recheck_command(&rig.fx.store, run_id, rig.now()),
        Err(RequestError::TooMany)
    );
    // A second later it is allowed again.
    assert!(request::recheck_command(&rig.fx.store, run_id, rig.now() + 1_500).is_ok());
}

// ── Partial → attention → held → recheck → retry → resolved (RQ-9.1, RQ-10) ──

#[test]
fn partial_run_holds_new_triggers_until_recheck_and_retry_resolve_it() {
    let rig = Rig::new();
    let (a, p) = rig.automation("Photos");
    let host = rig.host(1, HostClass::App);
    rig.world.set_script(script(|_, call| {
        if call.dry_run {
            dry_run(2, 0)
        } else {
            partial(3, 1, 2)
        }
    }));
    rig.enqueue(&a, &p, TriggerKind::Schedule, HostClass::App);
    tick(&host);
    let first = rig.runs()[0].clone();
    assert_eq!(first.outcome, Some(Outcome::Partial));
    assert_eq!(
        (
            first.counts.ok,
            first.counts.failed,
            first.counts.not_started
        ),
        (3, 1, 2)
    );
    assert_eq!(rig.status_kind(&a), StatusKind::Attention);
    let att = attention_of(&rig, &a).unwrap();
    assert_eq!(att.run_id, first.id);
    assert_eq!(att.text, "1 failed \u{b7} 2 not started");

    // New triggers are held (RQ-9.1) and say so.
    rig.advance(11_000);
    rig.enqueue(&a, &p, TriggerKind::Watch, HostClass::App);
    tick(&host);
    assert_eq!(rig.world.calls().len(), 1, "the held trigger did not run");
    assert_eq!(rig.queue_len(), 0);
    assert!(
        rig.event_texts(&a)
            .iter()
            .any(|e| e.contains("Trigger held")),
        "{:?}",
        rig.event_texts(&a)
    );

    // Retry before a recheck is refused (RQ-10.2: only after a recheck that found work).
    assert_eq!(
        request::retry_command(&rig.fx.store, rig.fx.keys.as_ref(), first.id, rig.now()),
        Err(RequestError::NotReady("recheck_first"))
    );

    // Recheck (RQ-10.1): read-only, "4 earlier operations ... 2 remaining".
    assert_eq!(
        request::recheck_command(&rig.fx.store, first.id, rig.now()),
        Ok(QueueAnswer::Requested)
    );
    drive(&host);
    let rechecked = run_row(&rig, first.id);
    let rc = rechecked.recheck.expect("recheck recorded");
    assert_eq!(rc.state, RecheckState::Remaining);
    assert_eq!(rc.remaining, 2);
    assert_eq!(
        rc.text,
        "Fresh comparison: 4 earlier operations are in sync and won't be repeated \u{b7} 2 operations remaining."
    );
    assert!(
        attention_of(&rig, &a).is_some(),
        "a recheck with work left keeps the hold"
    );
    let dry: Vec<_> = rig
        .world
        .calls()
        .into_iter()
        .filter(|c| c.dry_run)
        .collect();
    assert_eq!(dry.len(), 1);
    assert!(
        !dry[0].holds_lease,
        "a recheck is read only and takes no lease"
    );

    // Retry (RQ-10.2): a fresh plan; success resolves the original run (RQ-10.4).
    rig.world.set_script(script(|_, _| completed(2)));
    assert_eq!(
        request::retry_command(&rig.fx.store, rig.fx.keys.as_ref(), first.id, rig.now()),
        Ok(QueueAnswer::Queued)
    );
    drive(&host);
    let calls = rig.world.calls();
    let last = calls.last().unwrap();
    assert!(
        matches!(last.mode, RunMode::Retry { of } if of == first.id),
        "{last:?}"
    );
    let retry_run = rig.runs().last().unwrap().clone();
    assert_eq!(retry_run.outcome, Some(Outcome::Completed));
    assert_eq!(retry_run.retry_of, Some(first.id));
    let resolved = run_row(&rig, first.id);
    assert_eq!(resolved.resolved_by, Some(retry_run.id));
    assert_eq!(
        resolved.resolved_text.as_deref(),
        Some(format!("Resolved by run #{}", retry_run.id).as_str())
    );
    assert!(attention_of(&rig, &a).is_none(), "Needs attention cleared");
    assert_ne!(rig.status_kind(&a), StatusKind::Attention);
    // Triggers run again.
    rig.advance(11_000);
    rig.enqueue(&a, &p, TriggerKind::Schedule, HostClass::App);
    tick(&host);
    assert_eq!(rig.world.calls().len(), calls.len() + 1);
}

/// RQ-10.3: a recheck that finds nothing left clears the hold.
#[test]
fn a_clean_recheck_clears_needs_attention() {
    let rig = Rig::new();
    let (a, p) = rig.automation("Clean");
    let host = rig.host(1, HostClass::App);
    rig.world.set_script(script(|_, call| {
        if call.dry_run {
            dry_run(0, 0)
        } else {
            partial(1, 1, 0)
        }
    }));
    rig.enqueue(&a, &p, TriggerKind::RunNow, HostClass::App);
    tick(&host);
    let id = rig.runs()[0].id;
    request::recheck_command(&rig.fx.store, id, rig.now()).unwrap();
    drive(&host);
    let row = run_row(&rig, id);
    assert_eq!(row.recheck.unwrap().state, RecheckState::InSync);
    assert_eq!(
        row.resolved_text.as_deref(),
        Some("Resolved \u{2014} folders are in sync")
    );
    assert!(attention_of(&rig, &a).is_none());
}

// ── Conflict (RQ-6, RQ-10.5) ──

#[test]
fn a_conflict_is_held_and_a_recheck_waits_for_the_user_to_decide_in_sync() {
    let rig = Rig::new();
    let (a, p) = rig.automation("Docs");
    let host = rig.host(1, HostClass::App);
    let remaining = Arc::new(AtomicUsize::new(1));
    {
        let remaining = remaining.clone();
        rig.world.set_script(script(move |_, call| {
            if call.dry_run {
                dry_run(0, remaining.load(Ordering::SeqCst) as u32)
            } else {
                conflict_held(1)
            }
        }));
    }
    rig.enqueue(&a, &p, TriggerKind::Watch, HostClass::App);
    tick(&host);
    let run = rig.runs()[0].clone();
    assert_eq!(run.outcome, Some(Outcome::Conflict));
    assert_eq!(
        attention_of(&rig, &a).unwrap().text,
        "1 conflict held for review"
    );
    // Still conflicting: the answer says Ferrum will not choose a version.
    request::recheck_command(&rig.fx.store, run.id, rig.now()).unwrap();
    drive(&host);
    let rc = run_row(&rig, run.id).recheck.unwrap();
    assert_eq!(rc.state, RecheckState::Conflict);
    assert!(rc.text.contains("Decide in Sync"), "{}", rc.text);
    assert!(attention_of(&rig, &a).is_some());
    // A conflict never offers Retry.
    assert!(matches!(
        request::retry_command(&rig.fx.store, rig.fx.keys.as_ref(), run.id, rig.now()),
        Err(RequestError::NotReady(_))
    ));
    // The user decided in Sync: the next recheck clears the hold.
    remaining.store(0, Ordering::SeqCst);
    rig.advance(2_000);
    request::recheck_command(&rig.fx.store, run.id, rig.now()).unwrap();
    drive(&host);
    let row = run_row(&rig, run.id);
    assert_eq!(row.recheck.unwrap().state, RecheckState::InSync);
    assert_eq!(
        row.resolved_text.as_deref(),
        Some("Resolved in Sync by you")
    );
    assert!(attention_of(&rig, &a).is_none());
}

// ── Stop and pause (RQ-8) ──

#[test]
fn stop_mid_run_ends_cancelled_with_the_no_rollback_note_and_no_attention() {
    let rig = Rig::new();
    let (a, p) = rig.automation("Stoppable");
    let host = rig.host(1, HostClass::App);
    rig.world
        .set_script(blocking(Arc::new(AtomicBool::new(false)), 3, 6));
    rig.enqueue(&a, &p, TriggerKind::RunNow, HostClass::App);
    host.tick();
    wait_until("run active", || !host.active_runs().is_empty());
    let run_id = host.active_runs()[0];
    assert_eq!(
        request::stop_command(&rig.fx.store, run_id, rig.now()),
        Ok(QueueAnswer::Requested)
    );
    // While the host has not answered yet the status reads "Stopping ...".
    let rows =
        crate::automation::read::list_in(&rig.fx.store, rig.fx.keys.as_ref(), rig.now()).unwrap();
    assert!(
        rows[0]
            .status
            .sub
            .as_deref()
            .unwrap_or("")
            .contains("Stopping"),
        "{:?}",
        rows[0].status
    );
    drive(&host);
    let row = run_row(&rig, run_id);
    assert_eq!(row.outcome, Some(Outcome::Cancelled));
    assert_eq!(
        row.note.as_deref(),
        Some("Stopped by you. 3 completed files stay as synced \u{2014} no automatic rollback.")
    );
    assert!(attention_of(&rig, &a).is_none(), "RQ-9.2");
    // The control was answered when the run ended.
    let pending = rig
        .fx
        .store
        .with_read(|c| controls::pending(c, rig.now()))
        .unwrap();
    assert!(pending.is_empty(), "{pending:?}");
}

#[test]
fn stopping_a_run_that_is_not_running_is_refused() {
    let rig = Rig::new();
    let (a, p) = rig.automation("Done already");
    let host = rig.host(1, HostClass::App);
    rig.enqueue(&a, &p, TriggerKind::RunNow, HostClass::App);
    tick(&host);
    let id = rig.runs()[0].id;
    assert_eq!(
        request::stop_command(&rig.fx.store, id, rig.now()),
        Err(RequestError::NotReady("not_running"))
    );
    assert_eq!(
        request::stop_command(&rig.fx.store, 9_999, rig.now()),
        Err(RequestError::NotFound)
    );
    assert_eq!(
        request::stop_command(&rig.fx.store, 0, rig.now()),
        Err(RequestError::Invalid)
    );
}

#[test]
fn pause_and_stop_cancels_the_run_and_drops_the_queue() {
    let rig = Rig::new();
    let (a, p) = rig.automation("Paused mid-run");
    let host = rig.host(1, HostClass::App);
    rig.world
        .set_script(blocking(Arc::new(AtomicBool::new(false)), 1, 4));
    rig.enqueue(&a, &p, TriggerKind::RunNow, HostClass::App);
    host.tick();
    wait_until("run active", || !host.active_runs().is_empty());
    let run_id = host.active_runs()[0];
    // A pending rerun and a waiting entry are dropped by the pause (RQ-8.2).
    rig.sql(&format!(
        "UPDATE automation_runtime SET pending_rerun = 1 WHERE automation_id = '{a}'"
    ));
    rig.enqueue(&a, &p, TriggerKind::Watch, HostClass::App);
    approval_pause_mode(&rig, &a, PauseMode::Stop);
    request::pause_run_part(&rig.fx.store, &a, true, rig.now()).unwrap();
    drive(&host);
    let row = run_row(&rig, run_id);
    assert_eq!(row.outcome, Some(Outcome::Cancelled));
    assert!(
        row.note
            .as_deref()
            .unwrap_or("")
            .starts_with("Stopped because you paused the automation"),
        "{:?}",
        row.note
    );
    assert_eq!(rig.queue_len(), 0);
    assert_eq!(rig.status_kind(&a), StatusKind::Paused);
    assert_eq!(rig.world.calls().len(), 1);
}

#[test]
fn pause_and_let_it_finish_lets_the_run_end_without_follow_ups() {
    let rig = Rig::new();
    let (a, p) = rig.automation("Finish first");
    let host = rig.host(1, HostClass::App);
    let release = Arc::new(AtomicBool::new(false));
    rig.world.set_script(blocking(release.clone(), 0, 2));
    rig.enqueue(&a, &p, TriggerKind::RunNow, HostClass::App);
    host.tick();
    wait_until("run active", || !host.active_runs().is_empty());
    let run_id = host.active_runs()[0];
    approval_pause_mode(&rig, &a, PauseMode::Finish);
    request::pause_run_part(&rig.fx.store, &a, false, rig.now()).unwrap();
    host.tick();
    assert_eq!(host.active_runs(), vec![run_id], "the run keeps going");
    release.store(true, Ordering::SeqCst);
    drive(&host);
    assert_eq!(run_row(&rig, run_id).outcome, Some(Outcome::Completed));
    assert_eq!(
        rig.queue_len(),
        0,
        "a paused automation does not queue a rerun"
    );
    assert_eq!(rig.status_kind(&a), StatusKind::Paused);
}

fn approval_pause_mode(rig: &Rig, a: &str, mode: PauseMode) {
    crate::automation::approval::pause_in(
        &rig.fx.store,
        rig.fx.keys.as_ref(),
        a,
        mode,
        rig.fx.clock(),
    )
    .unwrap();
}

// ── Outcomes of RQ-7 / RQ-9 ──

#[test]
fn a_tripped_guard_fails_the_run_and_asks_for_attention() {
    let rig = Rig::new();
    let (a, p) = rig.automation("Guarded");
    let host = rig.host(1, HostClass::App);
    rig.world.set_script(script(|_, _| {
        failed(
            "guard_tripped",
            "Would delete 60 items (25 % of the folder) \u{2014} over the safety limit. Review in Sync.",
        )
    }));
    rig.enqueue(&a, &p, TriggerKind::Schedule, HostClass::App);
    tick(&host);
    let run = rig.runs()[0].clone();
    assert_eq!(run.outcome, Some(Outcome::Failed));
    assert!(run.error_text.unwrap().contains("over the safety limit"));
    assert_eq!(rig.status_kind(&a), StatusKind::Attention);
}

#[test]
fn a_folder_that_disappears_mid_run_waits_for_the_device_without_attention() {
    let rig = Rig::new();
    let (a, p) = rig.automation("Unplugged");
    let host = rig.host(1, HostClass::App);
    rig.world.set_script(script(|_, _| {
        super::test_support::interrupted(2, 6, InterruptReason::RootOffline)
    }));
    rig.enqueue(&a, &p, TriggerKind::RunNow, HostClass::App);
    tick(&host);
    let run = rig.runs()[0].clone();
    assert_eq!(run.outcome, Some(Outcome::Interrupted));
    assert!(attention_of(&rig, &a).is_none(), "RQ-7.4 / RQ-9.2");
    assert_eq!(rig.status_kind(&a), StatusKind::Device);
    let entry = rig.fx.store.with_read(|c| queue::all(c)).unwrap();
    assert_eq!(entry.len(), 1);
    assert_eq!(
        entry[0].kind,
        TriggerKind::Available,
        "a fresh comparison decides"
    );
}

#[test]
fn a_file_changed_during_the_run_queues_one_pending_rerun_after_a_pause() {
    let rig = Rig::new();
    let (a, p) = rig.automation("Busy files");
    let host = rig.host(1, HostClass::App);
    let first = Arc::new(AtomicBool::new(true));
    {
        let first = first.clone();
        rig.world.set_script(script(move |_, _| {
            let mut r = completed(2);
            r.pending_rerun = first.swap(false, Ordering::SeqCst);
            r
        }));
    }
    rig.enqueue(&a, &p, TriggerKind::RunNow, HostClass::App);
    tick(&host);
    assert_eq!(rig.queue_len(), 1);
    // Not at once: the folder is given a moment to settle.
    tick(&host);
    assert_eq!(rig.world.calls().len(), 1);
    rig.advance(11_000);
    tick(&host);
    assert_eq!(rig.world.calls().len(), 2);
    let runs = rig.runs();
    assert_eq!(runs[1].trigger_kind, "pendingRerun");
    assert_eq!(rig.queue_len(), 0);
}

#[test]
fn reruns_do_not_chain_for_ever() {
    let rig = Rig::new();
    let (a, p) = rig.automation("Never quiet");
    let host = rig.host(1, HostClass::App);
    rig.world.set_script(script(|_, _| {
        let mut r = completed(1);
        r.pending_rerun = true;
        r
    }));
    rig.enqueue(&a, &p, TriggerKind::RunNow, HostClass::App);
    for _ in 0..12 {
        tick(&host);
        rig.advance(11_000);
    }
    assert_eq!(rig.runs().len(), 1 + super::MAX_RERUN_STREAK as usize);
    assert_eq!(rig.queue_len(), 0);
    assert!(
        rig.event_texts(&a)
            .iter()
            .any(|e| e.contains("Files keep changing")),
        "{:?}",
        rig.event_texts(&a)
    );
}

#[test]
fn a_nothing_to_do_retry_is_skipped_but_an_ordinary_run_is_completed() {
    let rig = Rig::new();
    let (a, p) = rig.automation("Quiet");
    let host = rig.host(1, HostClass::App);
    rig.world.set_script(script(|_, _| nothing_to_do()));
    rig.enqueue(&a, &p, TriggerKind::Schedule, HostClass::App);
    tick(&host);
    rig.enqueue(&a, &p, TriggerKind::CatchUp, HostClass::App);
    rig.advance(1_000);
    tick(&host);
    let runs = rig.runs();
    assert_eq!(runs[0].outcome, Some(Outcome::Completed));
    assert_eq!(
        runs[0].note.as_deref(),
        Some("Already in sync \u{b7} 0 operations")
    );
    assert_eq!(runs[1].outcome, Some(Outcome::Skipped));
    assert_eq!(
        runs[1].note.as_deref(),
        Some("Fresh comparison found nothing to do")
    );
}

#[test]
fn a_panic_in_the_engine_fails_the_run_and_frees_the_lease() {
    let rig = Rig::new();
    let (a, p) = rig.automation("Boom");
    let host = rig.host(1, HostClass::App);
    rig.world
        .set_script(script(|_, _| panic!("scripted engine panic")));
    rig.enqueue(&a, &p, TriggerKind::RunNow, HostClass::App);
    tick(&host);
    let run = rig.runs()[0].clone();
    assert_eq!(run.outcome, Some(Outcome::Failed));
    assert!(run.error_text.unwrap().contains("stopped unexpectedly"));
    assert_eq!(rig.status_kind(&a), StatusKind::Attention);
    assert!(
        rig.fx
            .store
            .with_read(|c| leases::all(c))
            .unwrap()
            .is_empty()
    );
    assert!(host.is_idle());
}

// ── Election (RT-4) and handover ──

#[test]
fn two_app_hosts_on_one_store_only_one_executes() {
    let rig = Rig::new();
    let (a, p) = rig.automation("Shared");
    let h1 = rig.host(1, HostClass::App);
    let h2 = rig.host(2, HostClass::App);
    h1.tick();
    h2.tick();
    assert!(h1.is_hosting() && !h2.is_hosting());
    assert_eq!(rig.host_rows().len(), 1);
    rig.enqueue(&a, &p, TriggerKind::RunNow, HostClass::App);
    h2.tick();
    settle(&h2);
    assert!(
        rig.world.calls().is_empty(),
        "the second instance does not execute"
    );
    tick(&h1);
    assert_eq!(rig.world.calls().len(), 1);
    assert_eq!(rig.runs()[0].host, RunHost::App);
}

/// "Killing the host" (no release, no more ticks): after 15 s of fake time the other instance
/// takes over and runs what is queued; the old host, waking up, does nothing.
#[test]
fn a_silent_host_is_replaced_after_fifteen_seconds() {
    let rig = Rig::new();
    let (a, p) = rig.automation("Handover");
    let h1 = rig.host(1, HostClass::App);
    let h2 = rig.host(2, HostClass::App);
    h1.tick();
    h2.tick();
    assert!(h1.is_hosting() && !h2.is_hosting());
    rig.advance(14_000);
    h2.tick();
    assert!(
        !h2.is_hosting(),
        "14 s after the last beat the host is still live"
    );
    rig.advance(1_001);
    rig.enqueue(&a, &p, TriggerKind::RunNow, HostClass::App);
    tick(&h2);
    assert!(h2.is_hosting());
    assert_eq!(rig.world.calls().len(), 1);
    // The old host wakes up: it learns it lost the role and starts nothing.
    rig.enqueue(&a, &p, TriggerKind::RunNow, HostClass::App);
    rig.advance(6_000);
    h1.tick();
    assert!(!h1.is_hosting());
    assert_eq!(rig.world.calls().len(), 1);
}

#[test]
fn a_host_whose_process_died_is_replaced_at_once() {
    let rig = Rig::new();
    let h1 = rig.host(1, HostClass::App);
    let h2 = rig.host(2, HostClass::App);
    h1.tick();
    rig.world.probe.kill(Holder {
        pid: 1,
        created: 10,
    });
    rig.advance(5_000);
    h2.tick();
    assert!(h2.is_hosting());
}

#[test]
fn a_clean_shutdown_hands_the_role_over_at_once() {
    let rig = Rig::new();
    let h1 = rig.host(1, HostClass::App);
    let h2 = rig.host(2, HostClass::App);
    h1.tick();
    h1.shutdown(Duration::from_millis(200));
    assert!(rig.host_rows().is_empty());
    h2.tick();
    assert!(h2.is_hosting());
    // A host that shut down starts nothing any more.
    let (a, p) = rig.automation("After");
    rig.enqueue(&a, &p, TriggerKind::RunNow, HostClass::App);
    h1.tick();
    assert!(!h1.is_hosting() || rig.world.calls().is_empty());
}

#[test]
fn shutdown_interrupts_the_run_in_progress() {
    let rig = Rig::new();
    let (a, p) = rig.automation("Closing");
    let host = rig.host(1, HostClass::App);
    rig.world
        .set_script(blocking(Arc::new(AtomicBool::new(false)), 2, 5));
    rig.enqueue(&a, &p, TriggerKind::RunNow, HostClass::App);
    host.tick();
    wait_until("run active", || !host.active_runs().is_empty());
    let run_id = host.active_runs()[0];
    host.shutdown(Duration::from_secs(10));
    let row = run_row(&rig, run_id);
    assert_eq!(row.outcome, Some(Outcome::Interrupted), "LC-2");
    assert!(
        row.note
            .as_deref()
            .unwrap_or("")
            .contains("updated or closed"),
        "{:?}",
        row.note
    );
    assert!(attention_of(&rig, &a).is_none());
}

// ── Crash recovery (LC-6) ──

fn orphan_run(rig: &Rig, automation: &str, profile: &str, dead: Holder, host: RunHost) -> i64 {
    let probe = rig.world.probe.clone();
    let now = rig.now();
    let rec = rig
        .fx
        .store
        .with_read(|c| crate::automation::store::profiles::get(c, profile))
        .unwrap()
        .unwrap();
    let id = rig
        .fx
        .store
        .with_write(|c| -> Result<i64, StoreError> {
            runs::start(
                c,
                &NewRun {
                    automation_id: Some(automation),
                    automation_name: Some("Orphan"),
                    profile_id: profile,
                    profile_revision: rec.revision,
                    policy_hash: None,
                    host,
                    trigger_kind: "schedule",
                    trigger_text: "Scheduled",
                    merged_count: 0,
                    retry_of: None,
                    started_ms: now,
                },
            )
        })
        .unwrap();
    let roots = vec!["C:\\orphan-a".to_string(), "C:\\orphan-b".to_string()];
    rig.fx
        .store
        .with_write(|c| {
            leases::acquire(
                c,
                &AcquireRequest {
                    kind: HolderKind::App,
                    holder: dead,
                    run_id: Some(id),
                    roots: &roots,
                },
                now,
                probe.as_ref(),
            )
        })
        .unwrap();
    id
}

#[test]
fn a_run_left_running_by_a_dead_host_becomes_interrupted_and_its_lease_is_freed() {
    let rig = Rig::new();
    let (a, p) = rig.automation("Crashed");
    let dead = Holder {
        pid: 5,
        created: 50,
    };
    let id = orphan_run(&rig, &a, &p, dead, RunHost::App);
    rig.world.probe.kill(dead);
    let host = rig.host(1, HostClass::App);
    host.tick();
    settle(&host);
    let row = run_row(&rig, id);
    assert_eq!(row.outcome, Some(Outcome::Interrupted));
    assert!(row.ended_ms.is_some());
    assert!(
        attention_of(&rig, &a).is_none(),
        "an interrupted run holds nothing (RQ-9.2)"
    );
    assert!(
        rig.fx
            .store
            .with_read(|c| leases::all(c))
            .unwrap()
            .is_empty(),
        "the dead host's lease is gone"
    );
    assert!(
        rig.event_texts(&a)
            .iter()
            .any(|e| e.contains("was interrupted")),
        "{:?}",
        rig.event_texts(&a)
    );
    // And the folders can be used again.
    request::run_now_command(&rig.fx.store, rig.fx.keys.as_ref(), &a, rig.now()).unwrap();
    tick(&host);
    assert_eq!(rig.world.calls().len(), 1);
}

#[test]
fn a_run_whose_lease_is_still_alive_is_not_taken_away_from_a_stalled_host() {
    let rig = Rig::new();
    let (a, p) = rig.automation("Stalled");
    let stalled = Holder {
        pid: 5,
        created: 50,
    };
    let id = orphan_run(&rig, &a, &p, stalled, RunHost::App);
    // Alive (the probe knows it) and its beat is fresh: only the host row went stale.
    let host = rig.host(1, HostClass::App);
    host.tick();
    settle(&host);
    assert_eq!(run_row(&rig, id).outcome, None, "still running");
}

#[test]
fn the_runs_of_the_other_host_class_are_left_alone_by_recovery() {
    let rig = Rig::new();
    let (a, p) = rig.automation("Background run");
    let dead = Holder {
        pid: 5,
        created: 50,
    };
    let id = orphan_run(&rig, &a, &p, dead, RunHost::Background);
    rig.world.probe.kill(dead);
    let host = rig.host(1, HostClass::App);
    host.tick();
    settle(&host);
    assert_eq!(
        run_row(&rig, id).outcome,
        None,
        "the runner's run is the runner's to recover"
    );
}

// ── HostClass::Background ──

#[test]
fn a_background_host_runs_only_background_automations() {
    let rig = Rig::new();
    let (app_auto, app_profile) = rig.automation("In-app");
    let (bg_auto, bg_profile) = rig.background_automation("Runner");
    rig.enqueue(
        &app_auto,
        &app_profile,
        TriggerKind::Schedule,
        HostClass::App,
    );
    rig.enqueue(
        &bg_auto,
        &bg_profile,
        TriggerKind::Schedule,
        HostClass::Background,
    );

    let runner = rig.host(2, HostClass::Background);
    tick(&runner);
    let calls = rig.world.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].automation_id, bg_auto);
    assert_eq!(rig.runs()[0].host, RunHost::Background);
    // The in-app entry is untouched by the runner ...
    assert_eq!(rig.queue_len(), 1);
    // ... and the app host does not touch the runner's.
    rig.enqueue(
        &bg_auto,
        &bg_profile,
        TriggerKind::Schedule,
        HostClass::Background,
    );
    let app = rig.host(1, HostClass::App);
    rig.advance(1_000);
    tick(&app);
    let calls = rig.world.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[1].automation_id, app_auto);
    assert_eq!(
        rig.queue_len(),
        1,
        "the background entry waits for the runner"
    );
    assert_eq!(rig.host_rows().len(), 2, "one row per host kind");
}

#[test]
fn a_host_leaves_the_controls_of_the_other_class_for_that_host() {
    let rig = Rig::new();
    let (bg_auto, _) = rig.background_automation("Runner only");
    rig.sql(&format!(
        "INSERT INTO controls(automation_id, action, created_ms) VALUES ('{bg_auto}', 'runNow', {})",
        rig.now()
    ));
    let app = rig.host(1, HostClass::App);
    tick(&app);
    let pending = rig
        .fx
        .store
        .with_read(|c| controls::pending(c, rig.now()))
        .unwrap();
    assert_eq!(pending.len(), 1, "not answered by the app host");
    assert_eq!(rig.queue_len(), 0);
    let runner = rig.host(2, HostClass::Background);
    tick(&runner);
    assert_eq!(rig.world.calls().len(), 1);
    assert!(
        rig.fx
            .store
            .with_read(|c| controls::pending(c, rig.now()))
            .unwrap()
            .is_empty()
    );
}

// ── Triggers merged into another automation's entry (design §9.5) ──

#[test]
fn merged_triggers_move_to_a_healthy_sibling_when_the_owner_is_paused() {
    let rig = Rig::new();
    let (owner, p) = rig.automation("Owner");
    let sibling = rig.automation_on("Sibling", &p);
    rig.enqueue(&owner, &p, TriggerKind::Schedule, HostClass::App);
    // The sibling's trigger merges into the owner's entry (one entry per profile).
    rig.fx
        .store
        .with_write(|c| {
            queue::enqueue(
                c,
                &queue::NewEntry {
                    automation_id: &sibling,
                    profile_id: &p,
                    host_class: HostClass::App,
                    kind: TriggerKind::Watch,
                    text: "Changes detected",
                    retry_of: None,
                },
                rig.now(),
            )
            .map(|_| ())
        })
        .unwrap();
    let before = rig.fx.store.with_read(|c| queue::all(c)).unwrap();
    assert_eq!((before.len(), before[0].merged_count), (1, 1));
    approval_pause(&rig, &owner);
    let after = rig.fx.store.with_read(|c| queue::all(c)).unwrap();
    assert_eq!(after.len(), 1, "the merged trigger survived");
    assert_eq!(after[0].automation_id, sibling);
    assert_eq!(after[0].kind, TriggerKind::Watch);
    assert_eq!(after[0].text, "Changes detected");
    // It runs, as the sibling.
    let host = rig.host(1, HostClass::App);
    tick(&host);
    assert_eq!(rig.world.calls()[0].automation_id, sibling);
}

#[test]
fn merged_triggers_move_when_the_owner_is_held_or_in_review_at_the_gate() {
    let rig = Rig::new();
    let (owner, p) = rig.automation("Held owner");
    let sibling = rig.automation_on("Healthy sibling", &p);
    let host = rig.host(1, HostClass::App);
    rig.enqueue(&owner, &p, TriggerKind::Schedule, HostClass::App);
    rig.fx
        .store
        .with_write(|c| {
            queue::enqueue(
                c,
                &queue::NewEntry {
                    automation_id: &sibling,
                    profile_id: &p,
                    host_class: HostClass::App,
                    kind: TriggerKind::Watch,
                    text: "Changes detected",
                    retry_of: None,
                },
                rig.now(),
            )
            .map(|_| ())
        })
        .unwrap();
    set_attention(&rig, &owner, 4);
    tick(&host);
    assert!(rig.world.calls().is_empty(), "the held owner did not run");
    let entries = rig.fx.store.with_read(|c| queue::all(c)).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].automation_id, sibling);
    rig.advance(1_000);
    tick(&host);
    assert_eq!(rig.world.calls().len(), 1);
    assert_eq!(rig.world.calls()[0].automation_id, sibling);
}

#[test]
fn a_sibling_that_is_itself_unhealthy_does_not_receive_the_trigger() {
    let rig = Rig::new();
    let (owner, p) = rig.automation("Owner 2");
    let sibling = rig.automation_on("Sibling 2", &p);
    set_attention(&rig, &sibling, 8);
    rig.enqueue(&owner, &p, TriggerKind::Schedule, HostClass::App);
    rig.fx
        .store
        .with_write(|c| {
            queue::enqueue(
                c,
                &queue::NewEntry {
                    automation_id: &sibling,
                    profile_id: &p,
                    host_class: HostClass::App,
                    kind: TriggerKind::Watch,
                    text: "Changes detected",
                    retry_of: None,
                },
                rig.now(),
            )
            .map(|_| ())
        })
        .unwrap();
    approval_pause(&rig, &owner);
    assert_eq!(rig.queue_len(), 0, "nobody healthy to give it to");
}

#[test]
fn deleting_the_owner_moves_merged_triggers_too() {
    let rig = Rig::new();
    let (owner, p) = rig.automation("Doomed owner");
    let sibling = rig.automation_on("Survivor", &p);
    rig.enqueue(&owner, &p, TriggerKind::Schedule, HostClass::App);
    rig.fx
        .store
        .with_write(|c| {
            queue::enqueue(
                c,
                &queue::NewEntry {
                    automation_id: &sibling,
                    profile_id: &p,
                    host_class: HostClass::App,
                    kind: TriggerKind::Watch,
                    text: "Changes detected",
                    retry_of: None,
                },
                rig.now(),
            )
            .map(|_| ())
        })
        .unwrap();
    crate::automation::approval::delete_in(
        &rig.fx.store,
        rig.fx.keys.as_ref(),
        &owner,
        rig.fx.clock(),
    )
    .unwrap();
    let entries = rig.fx.store.with_read(|c| queue::all(c)).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].automation_id, sibling);
}

// ── Events ──

#[test]
fn events_and_progress_leak_no_lease_id_mac_or_key() {
    let rig = Rig::new();
    let (a, p) = rig.automation("Private");
    let host = rig.host(1, HostClass::App);
    let lease_id = Arc::new(Mutex::new(String::new()));
    {
        let lease_id = lease_id.clone();
        rig.world.set_script(script(move |proof, call| {
            *lease_id.lock().unwrap() = proof.lease().unwrap().id().to_string();
            call.sink.phase(Phase::Comparing);
            call.sink.phase(Phase::Syncing);
            call.sink.counts(&Counts {
                total: 3,
                done: 1,
                ..Counts::default()
            });
            completed(3)
        }));
    }
    rig.enqueue(&a, &p, TriggerKind::RunNow, HostClass::App);
    tick(&host);
    let lease_id = lease_id.lock().unwrap().clone();
    assert_eq!(lease_id.len(), 32);
    let mac = rig
        .fx
        .store
        .with_read(|c| crate::automation::store::approvals::get(c, &a))
        .unwrap()
        .unwrap()
        .mac;
    let events = rig.world.emitted();
    assert!(!events.is_empty());
    let json = serde_json::to_string(&events).unwrap();
    assert!(!json.contains(&lease_id), "no lease id");
    assert!(!json.contains(&mac), "no MAC");
    assert!(!json.to_lowercase().contains("key"), "{json}");
    assert!(
        events
            .iter()
            .any(|e| matches!(e, HostEvent::Progress(p) if p.phase == "sync"))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, HostEvent::Progress(p) if p.phase == "done" && p.ops_total == 3))
    );
    // Nor do the DTOs the views read.
    let list = serde_json::to_string(
        &crate::automation::read::list_in(&rig.fx.store, rig.fx.keys.as_ref(), rig.now()).unwrap(),
    )
    .unwrap();
    assert!(!list.contains(&lease_id) && !list.contains(&mac));
}

/// MH-10: a change made by another process shows up as a `Changed` event within the poll gap,
/// on the instance that is not the host as well.
#[test]
fn a_change_made_elsewhere_is_announced_within_two_seconds() {
    let rig = Rig::new();
    let observer = rig.host(2, HostClass::App);
    let host = rig.host(1, HostClass::App);
    host.tick();
    observer.tick();
    let seq = |rig: &Rig| {
        rig.world
            .emitted()
            .iter()
            .filter_map(|e| match e {
                HostEvent::Changed { seq } => Some(*seq),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let first = seq(&rig);
    assert!(!first.is_empty());
    // Another process writes.
    let (_a, _p) = rig.automation("Elsewhere");
    rig.advance(1_000);
    observer.tick();
    assert_eq!(seq(&rig).len(), first.len(), "not before the poll gap");
    rig.advance(1_100);
    observer.tick();
    let all = seq(&rig);
    assert_eq!(all.len(), first.len() + 1);
    assert!(all.last().unwrap() > first.last().unwrap());
}

#[allow(dead_code)]
fn _keep(_: EngineOutcome) {}
