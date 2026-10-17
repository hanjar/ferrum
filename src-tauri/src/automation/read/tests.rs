//! The read commands (P1.2): list, detail, history, operations, events. Status is the pure
//! `status::compute`; these tests pin that the store facts reach it correctly, that nothing is
//! written by a read, and that every request is validated.

use super::*;
use crate::automation::approval::{self, TestRequest};
use crate::automation::dpapi::KeyLoad;
use crate::automation::dto::{PauseMode, StatusKind};
use crate::automation::store::StoreError;
use crate::automation::store::controls::{self, NewControl};
use crate::automation::store::events::EventKind;
use crate::automation::store::hosts::{self, HostRow};
use crate::automation::store::queue::{HostClass, NewEntry, TriggerKind};
use crate::automation::store::runs::{
    Finish, NewOp, NewRun, OpKind, OpSide, OpStatus, Phase, RunCounts, RunHost,
};
use crate::automation::store::runtime::AttentionKind;
use crate::automation::test_support::{Fixture, T0, passing_dry_run};

fn write<T>(fx: &Fixture, f: impl FnOnce(&Connection) -> Result<T, StoreError>) -> T {
    fx.store
        .with_write(|conn| -> Result<T, StoreError> { f(conn) })
        .unwrap()
}

fn list(fx: &Fixture, at: i64) -> Vec<AutomationRowDto> {
    list_in(&fx.store, fx.keys.as_ref(), at).unwrap()
}

fn row(fx: &Fixture, id: &str, at: i64) -> AutomationRowDto {
    list(fx, at).into_iter().find(|r| r.id == id).unwrap()
}

fn status(fx: &Fixture, id: &str, at: i64) -> (StatusKind, String, Option<String>) {
    let s = row(fx, id, at).status;
    (s.kind, s.label, s.sub)
}

fn one_watch(fx: &Fixture, name: &str) -> (String, String) {
    let (p, _) = fx.profile(&format!("{name} profile"));
    let a = fx.new_automation(name, &p, Fixture::watch());
    (p, a)
}

fn start(fx: &Fixture, auto: &str, name: &str, profile: &str, at: i64) -> i64 {
    write(fx, |c| {
        runs::start(
            c,
            &NewRun {
                automation_id: Some(auto),
                automation_name: Some(name),
                profile_id: profile,
                profile_revision: 1,
                policy_hash: None,
                host: RunHost::App,
                trigger_kind: "schedule",
                trigger_text: "Scheduled \u{b7} 02:00 WIB",
                merged_count: 0,
                retry_of: None,
                started_ms: at,
            },
        )
    })
}

fn end(fx: &Fixture, run: i64, outcome: Outcome, counts: RunCounts, total: i64, at: i64) {
    write(fx, |c| {
        runs::finish(
            c,
            run,
            &Finish {
                ended_ms: at,
                outcome,
                ops_total: total,
                counts,
                error_text: None,
                note: None,
            },
        )
        .map(|_| ())
    });
}

fn queue_up(fx: &Fixture, auto: &str, profile: &str, text: &str, at: i64) {
    write(fx, |c| {
        queue::enqueue(
            c,
            &NewEntry {
                automation_id: auto,
                profile_id: profile,
                host_class: HostClass::App,
                kind: TriggerKind::Watch,
                text,
                retry_of: None,
            },
            at,
        )
        .map(|_| ())
    });
}

fn history(fx: &Fixture, req: HistoryRequest) -> Result<HistoryDto, CommandError> {
    history_in(&fx.store, &req)
}

fn hist(limit: i64) -> HistoryRequest {
    HistoryRequest {
        filter: HistoryFilterWire::All,
        automation_id: None,
        before: None,
        limit,
    }
}

// ── List: facts reach the status ──

#[test]
fn the_list_shows_trigger_runtime_and_flags_of_a_fresh_automation() {
    let fx = Fixture::new();
    let (pa, _) = fx.profile("Photos to NAS");
    let sched = fx.new_automation("Nightly photos", &pa, Fixture::daily());
    let (_, watch) = one_watch(&fx, "Projects watch");
    let rows = list(&fx, T0);
    assert_eq!(rows.len(), 2);
    // Ordered by name.
    assert_eq!(
        rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
        vec!["Nightly photos", "Projects watch"]
    );
    let s = row(&fx, &sched, T0);
    assert_eq!(
        (
            s.status.kind,
            s.status.label.as_str(),
            s.status.sub.as_deref()
        ),
        (StatusKind::Scheduled, "Scheduled", Some("No upcoming run"))
    );
    assert_eq!(s.trigger, "Daily at 02:00 (Asia/Jakarta)");
    assert_eq!(
        (
            s.profile_name.as_str(),
            s.profile_revision,
            s.approved_revision
        ),
        ("Photos to NAS", 1, Some(1))
    );
    assert_eq!(
        (s.runtime.as_str(), s.runtime_note.as_str()),
        ("app", "Only while open")
    );
    assert!(s.can_run_now && s.run_now_reason.is_none() && !s.show_review);
    assert!(s.last_run.is_none() && s.review.is_none() && s.attention.is_none());
    let w = row(&fx, &watch, T0);
    assert_eq!(w.status.kind, StatusKind::Watching);
    assert_eq!(
        w.trigger,
        "Watches the left folder \u{b7} runs after 10 s without changes \u{b7} runs when a folder comes back"
    );
}

#[test]
fn needs_review_comes_from_the_approval_not_from_the_enabled_flag() {
    let fx = Fixture::new();
    let (p, _) = fx.profile("Projects");
    let a = fx.new_automation("Projects watch", &p, Fixture::watch());
    fx.bump_profile(&p, 1, "*.tmp");
    let r = row(&fx, &a, T0);
    assert_eq!(
        (
            r.status.kind,
            r.status.label.as_str(),
            r.status.sub.as_deref()
        ),
        (
            StatusKind::Review,
            "Needs review",
            Some("Profile changed (rev 1 \u{2192} 2) \u{b7} held")
        )
    );
    assert!(!r.can_run_now && r.show_review);
    assert_eq!(
        r.review,
        Some(NeedsReviewDto::ProfileChanged { from: 1, to: 2 })
    );
    assert_eq!(r.profile_revision, 2);
    assert_eq!(r.approved_revision, Some(1));
    // Enabled or not, the verdict is the same.
    approval::pause_in(
        &fx.store,
        fx.keys.as_ref(),
        &a,
        PauseMode::Finish,
        fx.clock(),
    )
    .unwrap();
    assert_eq!(row(&fx, &a, T0).status.kind, StatusKind::Review);
    // Without a usable key nothing verifies.
    let (p2, _) = fx.profile("Second");
    let b = fx.new_automation("Second watch", &p2, Fixture::watch());
    fx.memory.as_ref().unwrap().set(KeyLoad::Missing);
    let r = row(&fx, &b, T0);
    assert_eq!(r.review, Some(NeedsReviewDto::KeyUnavailable));
    // Reading never creates a key.
    assert!(matches!(fx.keys.load(), KeyLoad::Missing));
}

#[test]
fn paused_and_a_stored_policy_that_cannot_be_read() {
    let fx = Fixture::new();
    let (_, a) = one_watch(&fx, "Projects watch");
    let (pb, _) = fx.profile("Photos");
    let b = fx.new_automation("Nightly photos", &pb, Fixture::daily());
    approval::pause_in(
        &fx.store,
        fx.keys.as_ref(),
        &a,
        PauseMode::Finish,
        fx.clock(),
    )
    .unwrap();
    approval::pause_in(
        &fx.store,
        fx.keys.as_ref(),
        &b,
        PauseMode::Finish,
        fx.clock(),
    )
    .unwrap();
    let w = row(&fx, &a, T0);
    assert_eq!(
        (w.status.kind, w.status.sub.as_deref()),
        (StatusKind::Paused, Some("Not monitoring"))
    );
    assert!(!w.can_run_now);
    assert_eq!(
        w.run_now_reason.as_deref(),
        Some("Paused \u{2014} resume it first")
    );
    assert_eq!(row(&fx, &b, T0).status.sub.as_deref(), Some("No new runs"));
    // A row whose stored policy is damaged stays in the list, held for review.
    write(&fx, |c| {
        c.execute(
            "UPDATE automations SET policy = '{broken' WHERE id = ?1",
            [&a],
        )
        .map(|_| ())
        .map_err(|_| StoreError::Unavailable)
    });
    let r = row(&fx, &a, T0);
    assert_eq!(r.status.kind, StatusKind::Review);
    assert_eq!(r.trigger, "");
    assert_eq!(
        list(&fx, T0).len(),
        2,
        "one damaged row does not hide the others"
    );
    let err = get_in(&fx.store, fx.keys.as_ref(), T0, &a).unwrap_err();
    assert_eq!(err.code, "needs_review");
}

#[test]
fn running_queued_and_waiting_rows() {
    crate::automation::host::set_unattended_for_tests(true);
    let fx = Fixture::new();
    let (p, a) = one_watch(&fx, "Projects watch");
    let b = fx.new_automation("Same folders, other trigger", &p, Fixture::daily());
    // A runs: Comparing, then Running 2/6, then stopping.
    let run = start(&fx, &a, "Projects watch", &p, T0);
    assert_eq!(status(&fx, &a, T0).1, "Comparing");
    write(&fx, |c| runs::set_phase(c, run, Phase::Sync));
    write(&fx, |c| {
        runs::set_progress(
            c,
            run,
            6,
            &RunCounts {
                ok: 2,
                ..Default::default()
            },
        )
    });
    assert_eq!(
        status(&fx, &a, T0),
        (
            StatusKind::Running,
            "Running 2/6".into(),
            Some("Scheduled \u{b7} 02:00 WIB".into())
        )
    );
    write(&fx, |c| runtime::set_pending_rerun(c, &a, true, T0));
    assert_eq!(
        status(&fx, &a, T0).2.as_deref(),
        Some("1 pending rerun after this")
    );
    write(&fx, |c| {
        controls::post(
            c,
            &NewControl {
                action: controls::Action::Stop,
                automation_id: None,
                run_id: Some(run),
            },
            T0,
        )
        .map(|_| ())
    });
    assert_eq!(
        status(&fx, &a, T0).2.as_deref(),
        Some("Stopping after the current file")
    );
    // An expired stop request no longer says "stopping".
    assert_eq!(
        status(&fx, &a, T0 + controls::TTL_MS + 1).2.as_deref(),
        Some("1 pending rerun after this")
    );
    // B queued on the same folders waits for A.
    queue_up(&fx, &b, &p, "Scheduled", T0);
    assert_eq!(
        status(&fx, &b, T0),
        (
            StatusKind::Queued,
            "Queued".into(),
            Some("Waits for \"Projects watch\" \u{2014} same folders".into())
        )
    );
    // Once A ends, B is simply starting.
    end(
        &fx,
        run,
        Outcome::Completed,
        RunCounts {
            ok: 6,
            ..Default::default()
        },
        6,
        T0 + 50,
    );
    assert_eq!(
        status(&fx, &b, T0 + 60).2.as_deref(),
        Some("Starting\u{2026}")
    );
    // Root offline: waiting for the device.
    write(&fx, |c| {
        runtime::set_trigger_state(c, &b, TriggerState::Device, None, None, None, T0 + 70)
            .map(|_| ())
    });
    assert_eq!(
        status(&fx, &b, T0 + 80),
        (StatusKind::Device, "Waiting for device".into(), None)
    );
}

#[test]
fn quiet_countdown_and_pending_rerun_of_a_watch() {
    let fx = Fixture::new();
    let (p, a) = one_watch(&fx, "Projects watch");
    let other = fx.new_automation("Other on the same folders", &p, Fixture::daily());
    write(&fx, |c| {
        runtime::set_trigger_state(
            c,
            &a,
            TriggerState::Quiet,
            Some(T0 + 7_000),
            Some(T0),
            None,
            T0,
        )
        .map(|_| ())
    });
    assert_eq!(
        status(&fx, &a, T0),
        (
            StatusKind::Quiet,
            "Waiting for quiet \u{b7} 7 s".into(),
            Some("Runs after 10 s without changes".into())
        )
    );
    assert_eq!(
        status(&fx, &a, T0 + 6_500).1,
        "Waiting for quiet \u{b7} 1 s"
    );
    assert_eq!(
        status(&fx, &a, T0 + 9_000).1,
        "Waiting for quiet \u{b7} 0 s"
    );
    let detail = get_in(&fx.store, fx.keys.as_ref(), T0, &a).unwrap();
    assert_eq!(detail.pipeline.current, Some(2));
    assert_eq!(detail.pipeline.steps.len(), 5);
    let quiet = detail.quiet.unwrap();
    assert_eq!(
        (quiet.until_ms, quiet.remaining_secs, quiet.idle_secs),
        (T0 + 7_000, 7, 10)
    );
    // A pending rerun shows only while an overlapping run is active.
    write(&fx, |c| runtime::set_pending_rerun(c, &a, true, T0));
    assert_eq!(status(&fx, &a, T0).0, StatusKind::Quiet);
    let run = start(&fx, &other, "Other on the same folders", &p, T0);
    assert_eq!(status(&fx, &a, T0).0, StatusKind::Pending);
    end(
        &fx,
        run,
        Outcome::Completed,
        RunCounts::default(),
        0,
        T0 + 5,
    );
    assert_eq!(status(&fx, &a, T0 + 10).0, StatusKind::Quiet);
}

#[test]
fn attention_holds_the_automation_and_shows_in_the_history() {
    let fx = Fixture::new();
    let (p, a) = one_watch(&fx, "Projects watch");
    let run = start(&fx, &a, "Projects watch", &p, T0);
    let counts = RunCounts {
        ok: 4,
        failed: 1,
        not_started: 1,
        ..Default::default()
    };
    end(&fx, run, Outcome::Partial, counts, 6, T0 + 30);
    let r = row(&fx, &a, T0 + 40);
    assert_eq!(
        (
            r.status.kind,
            r.status.label.as_str(),
            r.status.sub.as_deref()
        ),
        (
            StatusKind::Attention,
            "Needs attention",
            Some("1 failed \u{b7} 1 not started \u{b7} new runs held")
        )
    );
    assert!(!r.can_run_now);
    assert_eq!(
        r.run_now_reason.as_deref(),
        Some("1 failed \u{b7} 1 not started \u{b7} new runs held")
    );
    let att = r.attention.unwrap();
    assert_eq!((att.kind, att.run_id), (AttentionKind::Partial, run));
    let last = r.last_run.unwrap();
    assert_eq!(
        (last.run_id, last.result.kind, last.result.label.as_str()),
        (run, StatusKind::Partial, "Partial")
    );
    assert_eq!(
        last.result.sub.as_deref(),
        Some("4 succeeded \u{b7} 1 failed \u{b7} 1 not started")
    );
    let held = history(
        &fx,
        HistoryRequest {
            filter: HistoryFilterWire::Attention,
            ..hist(10)
        },
    )
    .unwrap();
    assert_eq!(held.runs.len(), 1);
    assert!(held.runs[0].needs_attention);
    assert_eq!(held.runs[0].result.label, "Partial");
    // Resolved: the hold is gone, the run stays in the history without the flag.
    write(&fx, |c| {
        runs::resolve(c, run, None, "Resolved in Sync by you", T0 + 60).map(|_| ())
    });
    assert_eq!(row(&fx, &a, T0 + 70).status.kind, StatusKind::Watching);
    assert!(row(&fx, &a, T0 + 70).can_run_now);
    let after = history(&fx, hist(10)).unwrap();
    assert!(!after.runs[0].needs_attention);
    assert_eq!(
        after.runs[0].resolved_text.as_deref(),
        Some("Resolved in Sync by you")
    );
    assert!(
        history(
            &fx,
            HistoryRequest {
                filter: HistoryFilterWire::Attention,
                ..hist(10)
            }
        )
        .unwrap()
        .runs
        .is_empty()
    );
}

#[test]
fn background_automations_need_a_live_runner() {
    let fx = Fixture::new();
    let (p, _) = fx.profile("Background profile");
    let draft = Fixture::auto_draft(
        "Background watch",
        &p,
        crate::automation::model::AutomationPolicy {
            runtime: crate::automation::model::Runtime::Background,
            ..Fixture::watch()
        },
    );
    let probe = Fixture::probe();
    let dry = passing_dry_run();
    let access = |_: &str, _: bool| true;
    let deps = crate::automation::test_run::TestDeps {
        probe: &probe,
        budget: std::time::Duration::from_secs(2),
        dry_run: &dry,
        access: &access,
        background_ready: true,
    };
    let test = approval::test_in(
        &fx.store,
        &fx.ledger,
        TestRequest {
            id: None,
            draft: draft.clone(),
        },
        &deps,
        fx.clock(),
    )
    .unwrap();
    assert!(test.passed);
    let crate::automation::dto::ApproveResultDto::Approved { automation } =
        fx.approve(None, None, &draft, &test.test_id, true)
    else {
        panic!("approved")
    };
    let id = automation.id;
    let r = row(&fx, &id, T0);
    assert_eq!(
        (r.status.kind, r.status.label.as_str()),
        (StatusKind::Warn, "Runner not running")
    );
    assert_eq!(
        (r.runtime.as_str(), r.runtime_note.as_str()),
        ("background", "Background runner")
    );
    // Run now is dimmed with the reason while the runner is down.
    assert!(!r.can_run_now);
    assert_eq!(r.run_now_reason.as_deref(), Some("Runner not running"));
    // A live runner heartbeat ends it; a stale one does not count.
    let beat = |at: i64| HostRow {
        kind: "runner".into(),
        pid: 77,
        created: 1,
        version: "0.2.0".into(),
        schema_version: 2,
        session_id: 1,
        last_beat_ms: at,
    };
    write(&fx, |c| hosts::beat(c, &beat(T0 - 16_000)));
    assert_eq!(row(&fx, &id, T0).status.kind, StatusKind::Warn);
    write(&fx, |c| hosts::beat(c, &beat(T0 - 5_000)));
    assert_eq!(row(&fx, &id, T0).status.kind, StatusKind::Watching);
    assert!(row(&fx, &id, T0).can_run_now);
    // The app host's heartbeat is not the runner's.
    write(&fx, |c| {
        hosts::beat(
            c,
            &HostRow {
                kind: "app".into(),
                ..beat(T0)
            },
        )?;
        hosts::beat(c, &beat(T0 - 99_000))
    });
    assert_eq!(row(&fx, &id, T0).status.kind, StatusKind::Warn);
}

// ── Detail ──

#[test]
fn the_detail_collects_current_run_waiting_entries_recent_runs_and_events() {
    let fx = Fixture::new();
    let (p, a) = one_watch(&fx, "Projects watch");
    let b = fx.new_automation("Second", &p, Fixture::daily());
    for n in 0..6 {
        let run = start(&fx, &a, "Projects watch", &p, T0 + n * 10);
        end(
            &fx,
            run,
            Outcome::Completed,
            RunCounts {
                ok: 1,
                ..Default::default()
            },
            1,
            T0 + n * 10 + 1,
        );
    }
    let current = start(&fx, &a, "Projects watch", &p, T0 + 100);
    for n in 0..20 {
        write(&fx, |c| {
            events::add_event(c, &a, T0 + n, EventKind::Mute, &format!("ignored {n}"))
        });
    }
    queue_up(&fx, &b, &p, "Changes detected", T0);
    queue_up(&fx, &b, &p, "Another change", T0 + 1);
    let d = get_in(&fx.store, fx.keys.as_ref(), T0 + 200, &a).unwrap();
    assert_eq!(d.automation.status.kind, StatusKind::Running);
    assert_eq!(d.pipeline.current, Some(4));
    assert_eq!(d.current_run.as_ref().map(|r| r.id), Some(current));
    assert_eq!(d.recent_runs.len(), RECENT_RUNS as usize);
    assert!(d.recent_runs.windows(2).all(|w| w[0].id > w[1].id));
    assert_eq!(
        d.recent_runs[0].id, current,
        "the running run is the newest of the recent ones"
    );
    assert_eq!(d.waiting.len(), 1);
    assert_eq!(d.waiting[0].merged_count, 1);
    assert_eq!(d.waiting[0].merged_reasons, vec!["Another change"]);
    assert_eq!(d.waiting[0].automation_name.as_deref(), Some("Second"));
    assert_eq!(d.events.events.len(), DETAIL_EVENTS as usize);
    assert!(d.events.next.is_some());
    assert_eq!(d.events.events[0].text, "ignored 19");
    assert_eq!(d.profile.name, "Projects watch profile");
    assert!(!d.profile.left.is_empty() && !d.profile.right.is_empty());
    assert_eq!(d.policy, Fixture::watch());
    // B's detail knows what it waits for.
    let db = get_in(&fx.store, fx.keys.as_ref(), T0 + 200, &b).unwrap();
    assert_eq!(db.waiting[0].waits_for.as_deref(), Some("Projects watch"));
    assert_eq!(
        db.pipeline.steps,
        vec!["Scheduled", "Queued", "Running", "Finished"]
    );
    assert_eq!(db.pipeline.current, Some(1));
    // Unknown and malformed ids.
    assert_eq!(
        get_in(
            &fx.store,
            fx.keys.as_ref(),
            T0,
            "7d1c2b3a-4e5f-4a6b-8c7d-9e0f1a2b3c4d"
        )
        .unwrap_err()
        .code,
        "not_found"
    );
    assert_eq!(
        get_in(&fx.store, fx.keys.as_ref(), T0, "nope")
            .unwrap_err()
            .code,
        "invalid_input"
    );
}

#[test]
fn a_held_automation_has_no_current_pipeline_step() {
    let fx = Fixture::new();
    let (p, a) = one_watch(&fx, "Projects watch");
    assert_eq!(
        get_in(&fx.store, fx.keys.as_ref(), T0, &a)
            .unwrap()
            .pipeline
            .current,
        Some(0)
    );
    fx.bump_profile(&p, 1, "*.tmp");
    let d = get_in(&fx.store, fx.keys.as_ref(), T0, &a).unwrap();
    assert_eq!(d.automation.status.kind, StatusKind::Review);
    assert_eq!(d.pipeline.current, None);
}

// ── History, operations and events: validation and paging ──

#[test]
fn history_requests_are_validated_and_paged() {
    let fx = Fixture::new();
    let (p, a) = one_watch(&fx, "Projects watch");
    let mut ids = Vec::new();
    for n in 0..5 {
        let run = start(&fx, &a, "Projects watch", &p, T0 + n);
        end(
            &fx,
            run,
            Outcome::Completed,
            RunCounts::default(),
            0,
            T0 + n + 1,
        );
        ids.push(run);
    }
    let p1 = history(&fx, hist(2)).unwrap();
    assert_eq!(
        p1.runs.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![ids[4], ids[3]]
    );
    assert_eq!(p1.next, Some(ids[3]));
    let p2 = history(
        &fx,
        HistoryRequest {
            before: p1.next,
            ..hist(2)
        },
    )
    .unwrap();
    assert_eq!(
        p2.runs.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![ids[2], ids[1]]
    );
    let p3 = history(
        &fx,
        HistoryRequest {
            before: p2.next,
            ..hist(2)
        },
    )
    .unwrap();
    assert_eq!((p3.runs.len(), p3.next), (1, None));
    let exact = history(&fx, hist(5)).unwrap();
    assert_eq!(
        (exact.runs.len(), exact.next),
        (5, None),
        "an exactly full last page has no cursor"
    );
    assert_eq!(history(&fx, hist(100)).unwrap().runs.len(), 5);
    assert_eq!(
        history(
            &fx,
            HistoryRequest {
                automation_id: Some(a.clone()),
                ..hist(3)
            }
        )
        .unwrap()
        .runs
        .len(),
        3
    );
    assert!(
        history(
            &fx,
            HistoryRequest {
                automation_id: Some("7d1c2b3a-4e5f-4a6b-8c7d-9e0f1a2b3c4d".into()),
                ..hist(3)
            }
        )
        .unwrap()
        .runs
        .is_empty()
    );
    for bad in [
        hist(0),
        hist(-1),
        hist(101),
        HistoryRequest {
            before: Some(0),
            ..hist(5)
        },
        HistoryRequest {
            before: Some(-4),
            ..hist(5)
        },
        HistoryRequest {
            automation_id: Some("../etc".into()),
            ..hist(5)
        },
    ] {
        assert_eq!(
            history(&fx, bad.clone()).unwrap_err().code,
            "invalid_input",
            "{bad:?}"
        );
    }
    // A run row carries what the history table shows.
    let r = &p1.runs[0];
    assert_eq!(
        (
            r.automation_name.as_deref(),
            r.host.as_str(),
            r.phase.as_str()
        ),
        (Some("Projects watch"), "app", "done")
    );
    assert_eq!(r.outcome.as_deref(), Some("completed"));
    assert_eq!(
        r.result.sub.as_deref(),
        Some("Already in sync \u{b7} 0 operations")
    );
}

#[test]
fn request_types_refuse_unknown_fields_and_values() {
    use serde_json::json;
    assert!(serde_json::from_value::<HistoryRequest>(json!({"filter":"all","limit":5})).is_ok());
    for bad in [
        json!({"filter":"all","limit":5,"extra":1}),
        json!({"filter":"everything","limit":5}),
        json!({"limit":5}),
        json!({"filter":"all"}),
        json!({"filter":"all","limit":"5"}),
    ] {
        assert!(
            serde_json::from_value::<HistoryRequest>(bad.clone()).is_err(),
            "{bad}"
        );
    }
    assert!(
        serde_json::from_value::<RunOpsRequest>(json!({"runId":1,"offset":0,"limit":5})).is_ok()
    );
    assert!(
        serde_json::from_value::<RunOpsRequest>(json!({"runId":1,"offset":0,"limit":5,"x":0}))
            .is_err()
    );
    let id = "7d1c2b3a-4e5f-4a6b-8c7d-9e0f1a2b3c4d";
    assert!(serde_json::from_value::<EventsRequest>(json!({"id":id,"limit":5})).is_ok());
    assert!(
        serde_json::from_value::<EventsRequest>(json!({"id":id,"limit":5,"force":true})).is_err()
    );
}

fn ask_missing(fx: &Fixture) -> CommandError {
    run_ops_in(
        &fx.store,
        &RunOpsRequest {
            run_id: 987_654,
            offset: 0,
            limit: 10,
        },
    )
    .unwrap_err()
}

#[test]
fn run_ops_are_paged_with_the_true_total() {
    let fx = Fixture::new();
    let (p, a) = one_watch(&fx, "Projects watch");
    let run = start(&fx, &a, "Projects watch", &p, T0);
    let paths: Vec<String> = (0..30).map(|n| format!("f{n:02}.txt")).collect();
    let ops: Vec<NewOp<'_>> = paths
        .iter()
        .map(|p| NewOp {
            kind: OpKind::CopyNew,
            side: OpSide::Right,
            rel_path: p,
            size: Some(5),
            status: OpStatus::Pending,
        })
        .collect();
    write(&fx, |c| runs::add_ops(c, run, &ops).map(|_| ()));
    // The run planned 12 000 operations; only 30 have rows here.
    write(&fx, |c| {
        runs::set_progress(c, run, 12_000, &RunCounts::default()).map(|_| ())
    });
    let ask = |offset, limit| {
        run_ops_in(
            &fx.store,
            &RunOpsRequest {
                run_id: run,
                offset,
                limit,
            },
        )
    };
    let first = ask(0, 10).unwrap();
    assert_eq!(
        (first.ops.len(), first.total, first.stored),
        (10, 12_000, 30)
    );
    assert_eq!(first.ops[0].rel_path, "f00.txt");
    assert_eq!(
        (
            first.ops[0].kind.as_str(),
            first.ops[0].side.as_str(),
            first.ops[0].status.as_str()
        ),
        ("copyNew", "right", "pending")
    );
    let last = ask(25, 200).unwrap();
    assert_eq!(last.ops.len(), 5);
    assert_eq!(last.ops[4].rel_path, "f29.txt");
    assert!(ask(30, 10).unwrap().ops.is_empty());
    assert_eq!(ask(5_000, 10).unwrap().ops.len(), 0);
    // The total never reads lower than what is stored.
    let small = start(&fx, &a, "Projects watch", &p, T0 + 1);
    write(&fx, |c| runs::add_ops(c, small, &ops[..3]).map(|_| ()));
    let got = run_ops_in(
        &fx.store,
        &RunOpsRequest {
            run_id: small,
            offset: 0,
            limit: 10,
        },
    )
    .unwrap();
    assert_eq!((got.total, got.stored), (3, 3));
    for bad in [
        (0, 0, 10),
        (-1, 0, 10),
        (run, -1, 10),
        (run, 5_001, 10),
        (run, 0, 0),
        (run, 0, 201),
        (run, 0, -3),
    ] {
        let err = run_ops_in(
            &fx.store,
            &RunOpsRequest {
                run_id: bad.0,
                offset: bad.1,
                limit: bad.2,
            },
        )
        .unwrap_err();
        assert_eq!(err.code, "invalid_input", "{bad:?}");
    }
    assert_eq!(ask_missing(&fx).code, "not_found");
}

#[test]
fn events_are_paged_and_validated() {
    let fx = Fixture::new();
    let (_, a) = one_watch(&fx, "Projects watch");
    for n in 0..30 {
        write(&fx, |c| {
            events::add_event(c, &a, T0 + n, EventKind::Info, &format!("event {n}"))
        });
    }
    let ask = |before, limit| {
        events_in(
            &fx.store,
            &EventsRequest {
                id: a.clone(),
                before,
                limit,
            },
        )
    };
    let p1 = ask(None, 10).unwrap();
    assert_eq!(p1.events.len(), 10);
    assert_eq!(p1.events[0].text, "event 29");
    let p2 = ask(p1.next, 10).unwrap();
    assert_eq!(p2.events[0].text, "event 19");
    let mut next = p2.next;
    let mut seen = 20;
    while let Some(cursor) = next {
        let page = ask(Some(cursor), 50).unwrap();
        seen += page.events.len();
        next = page.next;
    }
    assert!(
        seen >= 31,
        "30 events plus the approval line, every one once: {seen}"
    );
    for bad in [
        (None, 0),
        (None, 51),
        (None, -1),
        (Some(0), 10),
        (Some(-5), 10),
    ] {
        assert_eq!(
            ask(bad.0, bad.1).unwrap_err().code,
            "invalid_input",
            "{bad:?}"
        );
    }
    let missing = events_in(
        &fx.store,
        &EventsRequest {
            id: "7d1c2b3a-4e5f-4a6b-8c7d-9e0f1a2b3c4d".into(),
            before: None,
            limit: 5,
        },
    );
    assert_eq!(missing.unwrap_err().code, "not_found");
    let bad_id = events_in(
        &fx.store,
        &EventsRequest {
            id: "x".into(),
            before: None,
            limit: 5,
        },
    );
    assert_eq!(bad_id.unwrap_err().code, "invalid_input");
}

// ── Reads change nothing and leak nothing ──

fn counts(fx: &Fixture) -> (String, i64, i64, i64) {
    fx.store
        .with_read(|c| -> Result<_, StoreError> {
            let q = |sql: &str| -> i64 { c.query_row(sql, [], |r| r.get(0)).unwrap() };
            let seq: String = c
                .query_row("SELECT value FROM meta WHERE key = 'change_seq'", [], |r| {
                    r.get(0)
                })
                .unwrap();
            Ok((
                seq,
                q("SELECT COUNT(*) FROM events"),
                q("SELECT COUNT(*) FROM runs"),
                q("SELECT COUNT(*) FROM queue") + q("SELECT COUNT(*) FROM controls"),
            ))
        })
        .unwrap()
}

#[test]
fn reads_write_nothing_and_expose_no_secret() {
    let fx = Fixture::new();
    let (p, a) = one_watch(&fx, "Projects watch");
    let run = start(&fx, &a, "Projects watch", &p, T0);
    end(
        &fx,
        run,
        Outcome::Partial,
        RunCounts {
            ok: 1,
            failed: 1,
            ..Default::default()
        },
        2,
        T0 + 1,
    );
    let before = counts(&fx);
    let list_json = serde_json::to_string(&list(&fx, T0 + 5)).unwrap();
    let detail_json =
        serde_json::to_string(&get_in(&fx.store, fx.keys.as_ref(), T0 + 5, &a).unwrap()).unwrap();
    let hist_json = serde_json::to_string(&history(&fx, hist(10)).unwrap()).unwrap();
    assert_eq!(counts(&fx), before, "a read leaves the store as it was");
    for json in [&list_json, &detail_json, &hist_json] {
        for forbidden in [
            "leaseId",
            "lease_id",
            "\"mac\"",
            "policyHash",
            "policy_hash",
            "approvalKey",
        ] {
            assert!(!json.contains(forbidden), "{forbidden} leaked into {json}");
        }
    }
    // Read-only store (LC-3): reads still work.
    let dir = fx.store_dir();
    let ro = Store::open(&dir, "0.2.0");
    assert!(list_in(&ro, fx.keys.as_ref(), T0).is_ok());
}

#[test]
fn listing_with_the_real_dpapi_key_file_does_not_deadlock_or_touch_the_key() {
    let fx = Fixture::with_dpapi();
    let (p, _) = fx.profile("Projects");
    let a = {
        let draft = Fixture::auto_draft("Projects watch", &p, Fixture::watch());
        let test = fx.test(None, &draft);
        match fx.approve(None, None, &draft, &test.test_id, true) {
            crate::automation::dto::ApproveResultDto::Approved { automation } => automation.id,
            other => panic!("{other:?}"),
        }
    };
    assert_eq!(row(&fx, &a, T0).status.kind, StatusKind::Watching);
    let key_file = fx.store_dir().join("approval.key");
    let before = std::fs::metadata(&key_file).unwrap().modified().unwrap();
    for _ in 0..3 {
        list(&fx, T0);
    }
    assert_eq!(
        std::fs::metadata(&key_file).unwrap().modified().unwrap(),
        before
    );
}

// ── Pure helpers ──

#[test]
fn trigger_summaries_read_like_the_prototype() {
    use crate::automation::model::{MissedPolicy, ScheduleTrigger, WatchTrigger};
    let sched = |freq| {
        trigger_summary(&Trigger::Schedule(ScheduleTrigger {
            freq,
            time: "18:00".into(),
            tz: "Asia/Jakarta".into(),
            missed: MissedPolicy::Skip,
        }))
    };
    assert_eq!(sched(Freq::Daily {}), "Daily at 18:00 (Asia/Jakarta)");
    assert_eq!(
        sched(Freq::Weekly {
            days: vec![1, 3, 5]
        }),
        "Weekly on Mon, Wed, Fri at 18:00 (Asia/Jakarta)"
    );
    assert_eq!(
        sched(Freq::Once {
            date: "2026-10-20".into()
        }),
        "Once on 2026-10-20 at 18:00 (Asia/Jakarta)"
    );
    let watch = |l, r, c, a| {
        trigger_summary(&Trigger::Watch(WatchTrigger {
            watch_left: l,
            watch_right: r,
            on_changes: c,
            on_available: a,
            idle_secs: 30,
            max_wait_secs: 900,
        }))
    };
    assert_eq!(
        watch(true, true, true, false),
        "Watches both folders \u{b7} runs after 30 s without changes"
    );
    assert_eq!(
        watch(false, true, false, true),
        "Watches the right folder \u{b7} runs when a folder comes back"
    );
}

// ── Rework 1 ──

/// B1: the Queued chip follows the AUTOMATION's own entry (design 9.1), not the profile's.
#[test]
fn the_queued_chip_belongs_to_the_automation_that_owns_the_entry() {
    let fx = Fixture::new();
    let (p, a) = one_watch(&fx, "Projects watch");
    let b = fx.new_automation("Same folders, other trigger", &p, Fixture::daily());
    queue_up(&fx, &a, &p, "Changes detected", T0);
    assert_eq!(status(&fx, &a, T0).0, StatusKind::Queued);
    // B shares the profile but has no entry of its own: it shows its trigger state ...
    assert_eq!(status(&fx, &b, T0).0, StatusKind::Scheduled);
    // ... a paused B stays Paused ...
    approval::pause_in(
        &fx.store,
        fx.keys.as_ref(),
        &b,
        PauseMode::Finish,
        fx.clock(),
    )
    .unwrap();
    assert_eq!(status(&fx, &b, T0).0, StatusKind::Paused);
    assert_eq!(status(&fx, &a, T0).0, StatusKind::Queued);
    // ... and a held B says Needs attention.
    let run = start(&fx, &b, "Same folders, other trigger", &p, T0 + 1);
    let counts = RunCounts {
        failed: 1,
        ..Default::default()
    };
    end(&fx, run, Outcome::Failed, counts, 1, T0 + 2);
    assert_eq!(status(&fx, &b, T0 + 3).0, StatusKind::Attention);
    // The detail of B still lists the profile's one waiting entry.
    let d = get_in(&fx.store, fx.keys.as_ref(), T0 + 3, &b).unwrap();
    assert_eq!(d.waiting.len(), 1);
    assert_eq!(d.waiting[0].automation_id, a);
}

/// N7: approving again or resuming never releases an attention hold; only a recheck or retry does.
#[test]
fn approve_and_resume_keep_the_attention_hold() {
    let fx = Fixture::new();
    let (p, a) = one_watch(&fx, "Projects watch");
    let run = start(&fx, &a, "Projects watch", &p, T0);
    let counts = RunCounts {
        ok: 1,
        failed: 1,
        ..Default::default()
    };
    end(&fx, run, Outcome::Partial, counts, 2, T0 + 1);
    let held = |fx: &Fixture| row(fx, &a, T0 + 10).attention.map(|h| h.run_id);
    assert_eq!(held(&fx), Some(run));
    // Pause and resume (native confirmation answered yes).
    approval::pause_in(
        &fx.store,
        fx.keys.as_ref(),
        &a,
        PauseMode::Finish,
        fx.clock(),
    )
    .unwrap();
    approval::set_enabled_in(&fx.store, &a, true, &fx.deps(), |_| true).unwrap();
    assert_eq!(held(&fx), Some(run), "resume keeps the hold");
    // Re-approval of an edited automation.
    let mut policy = Fixture::watch();
    policy.retries = 1;
    let draft = Fixture::auto_draft("Projects watch", &p, policy);
    let test = fx.test(Some(&a), &draft);
    fx.approve(Some(&a), Some(1), &draft, &test.test_id, true);
    assert_eq!(held(&fx), Some(run), "re-approval keeps the hold");
    assert_eq!(row(&fx, &a, T0 + 10).status.kind, StatusKind::Attention);
    // A recheck/retry (resolve) releases it.
    write(&fx, |c| {
        runs::resolve(c, run, None, "Resolved in Sync by you", T0 + 20).map(|_| ())
    });
    assert_eq!(held(&fx), None);
}
