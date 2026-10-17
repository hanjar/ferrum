//! Queue, runtime, runs, run operations, events and controls (P1.2: RQ-3.1, RQ-3.3, RQ-3.4, RQ-9.1,
//! RQ-9.2, MH-7, MH-8, OP-2, design §3.5, §4.3).

use super::controls::{self, Action, NewControl, SKEW_MS, TTL_MS};
use super::events::{self, EVENT_MAX_CHARS, EVENTS_KEPT, EventKind};
use super::queue::{self, Enqueued, HostClass, MERGED_REASONS_KEPT, NewEntry, TriggerKind};
use super::runs::{
    self, Deleted, Finish, HistoryFilter, NewOp, NewRun, OPS_KEPT, OpKind, OpSide, OpStatus,
    Outcome, Phase, RecheckRecord, RecheckState, RunCounts, RunHost,
};
use super::runtime::{self, Attention, AttentionKind, TriggerState};
use super::{Store, StoreError};
use crate::automation::test_support::{Fixture, T0};

fn write<T>(fx: &Fixture, f: impl FnOnce(&rusqlite::Connection) -> Result<T, StoreError>) -> T {
    fx.store
        .with_write(|conn| -> Result<T, StoreError> { f(conn) })
        .unwrap()
}

fn read<T>(fx: &Fixture, f: impl FnOnce(&rusqlite::Connection) -> Result<T, StoreError>) -> T {
    fx.store
        .with_read(|conn| -> Result<T, StoreError> { f(conn) })
        .unwrap()
}

fn exec(fx: &Fixture, sql: &str) {
    write(fx, |c| {
        c.execute_batch(sql).map_err(|_| StoreError::Unavailable)
    });
}

/// Two automations on two profiles; returns `(profile_a, auto_a, profile_b, auto_b)`.
fn two(fx: &Fixture) -> (String, String, String, String) {
    let (pa, _) = fx.profile("Photos to NAS");
    let a = fx.new_automation("Nightly photos", &pa, Fixture::daily());
    let (pb, _) = fx.profile("Projects backup");
    let b = fx.new_automation("Projects watch", &pb, Fixture::watch());
    (pa, a, pb, b)
}

fn entry<'a>(auto: &'a str, profile: &'a str, kind: TriggerKind, text: &'a str) -> NewEntry<'a> {
    NewEntry {
        automation_id: auto,
        profile_id: profile,
        host_class: HostClass::App,
        kind,
        text,
        retry_of: None,
    }
}

fn new_run<'a>(auto: &'a str, name: &'a str, profile: &'a str, at: i64) -> NewRun<'a> {
    NewRun {
        automation_id: Some(auto),
        automation_name: Some(name),
        profile_id: profile,
        profile_revision: 1,
        policy_hash: Some("ab"),
        host: RunHost::App,
        trigger_kind: "schedule",
        trigger_text: "Scheduled \u{b7} 02:00 WIB",
        merged_count: 0,
        retry_of: None,
        started_ms: at,
    }
}

fn finish<'a>(outcome: Outcome, counts: RunCounts, total: i64, at: i64) -> Finish<'a> {
    Finish {
        ended_ms: at,
        outcome,
        ops_total: total,
        counts,
        error_text: None,
        note: None,
    }
}

// ── Queue ──

#[test]
fn a_second_trigger_for_the_same_profile_merges_count_and_reasons() {
    let fx = Fixture::new();
    let (pa, a, _, _) = two(&fx);
    let first = write(&fx, |c| {
        queue::enqueue(
            c,
            &entry(&a, &pa, TriggerKind::Schedule, "Scheduled \u{b7} 02:00 WIB"),
            T0,
        )
    });
    let Enqueued::Queued { id } = first else {
        panic!("queued, got {first:?}")
    };
    let second = write(&fx, |c| {
        queue::enqueue(
            c,
            &entry(&a, &pa, TriggerKind::Watch, "Changes detected"),
            T0 + 5,
        )
    });
    assert_eq!(
        second,
        Enqueued::Merged {
            id,
            merged_count: 1
        }
    );
    // The same reason again counts but is not listed twice; the entry's own text is not a reason.
    write(&fx, |c| {
        queue::enqueue(
            c,
            &entry(&a, &pa, TriggerKind::Watch, "Changes detected"),
            T0 + 6,
        )
    });
    let third = write(&fx, |c| {
        queue::enqueue(c, &entry(&a, &pa, TriggerKind::RunNow, "Run now"), T0 + 7)
    });
    assert_eq!(
        third,
        Enqueued::Merged {
            id,
            merged_count: 3
        }
    );
    let all = read(&fx, queue::all);
    assert_eq!(all.len(), 1, "no duplicate job");
    assert_eq!(all[0].merged_count, 3);
    assert_eq!(all[0].merged_reasons, vec!["Changes detected", "Run now"]);
    // The earlier entry keeps its place and its own trigger.
    assert_eq!(all[0].enqueued_ms, T0);
    assert_eq!(all[0].kind, TriggerKind::Schedule);
    assert_eq!(all[0].text, "Scheduled \u{b7} 02:00 WIB");
}

#[test]
fn merge_reasons_are_capped_and_bounded() {
    let fx = Fixture::new();
    let (pa, a, _, _) = two(&fx);
    write(&fx, |c| {
        queue::enqueue(c, &entry(&a, &pa, TriggerKind::Watch, "first"), T0)
    });
    for n in 0..(MERGED_REASONS_KEPT + 5) {
        let text = format!("reason {n}");
        write(&fx, |c| {
            queue::enqueue(c, &entry(&a, &pa, TriggerKind::Watch, &text), T0 + 1)
        });
    }
    let e = read(&fx, |c| queue::by_profile(c, &pa)).unwrap();
    assert_eq!(e.merged_count, (MERGED_REASONS_KEPT + 5) as i64);
    assert_eq!(e.merged_reasons.len(), MERGED_REASONS_KEPT);
    // A long text with control characters is stored flat and bounded.
    let long = format!("x\n{}", "y".repeat(500));
    let (pb, b) = {
        let (pb, _) = fx.profile("Other");
        let b = fx.new_automation("Other watch", &pb, Fixture::watch());
        (pb, b)
    };
    write(&fx, |c| {
        queue::enqueue(c, &entry(&b, &pb, TriggerKind::Watch, &long), T0)
    });
    let e = read(&fx, |c| queue::by_profile(c, &pb)).unwrap();
    assert_eq!(e.text.chars().count(), queue::TEXT_MAX);
    assert!(!e.text.contains('\n'));
    // An empty text and a retry without a run are refused.
    let err = fx
        .store
        .with_write(|c| queue::enqueue(c, &entry(&b, &pb, TriggerKind::Watch, ""), T0));
    assert_eq!(err, Err(StoreError::Invalid));
    let err = fx
        .store
        .with_write(|c| queue::enqueue(c, &entry(&b, &pb, TriggerKind::Retry, "Retry"), T0));
    assert_eq!(err, Err(StoreError::Invalid));
}

#[test]
fn different_profiles_with_overlapping_roots_queue_separately() {
    let fx = Fixture::new();
    let (pa, a, pb, b) = two(&fx);
    // The folders overlap (one inside the other), the profiles differ: two entries (RQ-3.1 merges
    // per profile; RQ-2 keeps them from RUNNING together).
    let outer = vec!["C:\\Data".to_string(), "D:\\Backup".to_string()];
    let inner = vec!["c:\\data\\Photos".to_string()];
    let apart = vec!["E:\\Other".to_string()];
    assert!(queue::roots_overlap(&outer, &inner));
    assert!(queue::roots_overlap(&inner, &outer));
    assert!(queue::roots_overlap(&outer, &outer));
    assert!(!queue::roots_overlap(&outer, &apart));
    // Component-wise: "C:\Data2" is not inside "C:\Data".
    assert!(!queue::roots_overlap(&outer, &["C:\\Data2".to_string()]));
    write(&fx, |c| {
        queue::enqueue(c, &entry(&a, &pa, TriggerKind::Schedule, "Scheduled"), T0)
    });
    write(&fx, |c| {
        queue::enqueue(
            c,
            &entry(&b, &pb, TriggerKind::Watch, "Changes detected"),
            T0 + 1,
        )
    });
    let all = read(&fx, queue::all);
    assert_eq!(all.len(), 2);
    assert!(all.iter().all(|e| e.merged_count == 0));
}

#[test]
fn entries_start_fifo_per_host_class_and_a_blocked_one_does_not_hold_up_the_next() {
    let fx = Fixture::new();
    let mut profiles = Vec::new();
    for n in 0..4 {
        let (p, _) = fx.profile(&format!("Profile {n}"));
        let a = fx.new_automation(&format!("Auto {n}"), &p, Fixture::watch());
        profiles.push((p, a));
    }
    let hosts = [
        HostClass::App,
        HostClass::Background,
        HostClass::App,
        HostClass::Background,
    ];
    for (i, ((p, a), host)) in profiles.iter().zip(hosts).enumerate() {
        let mut e = entry(a, p, TriggerKind::Watch, "Changes detected");
        e.host_class = host;
        write(&fx, |c| queue::enqueue(c, &e, T0 + i as i64 * 10));
    }
    let app = read(&fx, |c| queue::for_host(c, HostClass::App));
    let bg = read(&fx, |c| queue::for_host(c, HostClass::Background));
    assert_eq!(
        app.iter().map(|e| e.profile_id.clone()).collect::<Vec<_>>(),
        vec![profiles[0].0.clone(), profiles[2].0.clone()]
    );
    assert_eq!(
        bg.iter().map(|e| e.profile_id.clone()).collect::<Vec<_>>(),
        vec![profiles[1].0.clone(), profiles[3].0.clone()]
    );
    // Nothing blocked: the head starts. Head blocked: the next one that can.
    assert_eq!(
        queue::first_startable(&app, |_| false).unwrap().id,
        app[0].id
    );
    let blocked_head = queue::first_startable(&app, |e| e.id == app[0].id).unwrap();
    assert_eq!(blocked_head.id, app[1].id);
    assert!(queue::first_startable(&app, |_| true).is_none());
    // Removing the started entry leaves the others in order.
    write(&fx, |c| queue::remove(c, app[0].id));
    assert_eq!(read(&fx, |c| queue::for_host(c, HostClass::App)).len(), 1);
    assert!(
        !write(&fx, |c| queue::remove(c, app[0].id)),
        "second remove finds nothing"
    );
}

#[test]
fn the_queue_persists_across_a_reopen() {
    let fx = Fixture::new();
    let (pa, a, _, _) = two(&fx);
    write(&fx, |c| {
        queue::enqueue(
            c,
            &entry(&a, &pa, TriggerKind::Schedule, "Scheduled \u{b7} 02:00 WIB"),
            T0,
        )
    });
    write(&fx, |c| {
        queue::enqueue(
            c,
            &entry(&a, &pa, TriggerKind::Watch, "Changes detected"),
            T0 + 1,
        )
    });
    let before = read(&fx, queue::all);
    let dir = fx.store_dir();
    // A second handle on the same files sees the stored queue (a restarted host, RQ-3.4).
    let reopened = Store::open(&dir, "0.2.0");
    let after = reopened.with_read(queue::all).unwrap();
    assert_eq!(before, after);
    assert_eq!(after[0].merged_count, 1);
    assert_eq!(after[0].merged_reasons, vec!["Changes detected"]);
}

#[test]
fn a_retry_upgrades_its_own_entry_and_never_merges_into_another_automations() {
    let fx = Fixture::new();
    let (pa, a, _, _) = two(&fx);
    let other = fx.new_automation("Second on the same profile", &pa, Fixture::watch());
    let run = write(&fx, |c| {
        runs::start(c, &new_run(&a, "Nightly photos", &pa, T0))
    });
    write(&fx, |c| {
        queue::enqueue(
            c,
            &entry(&a, &pa, TriggerKind::Watch, "Changes detected"),
            T0,
        )
    });
    // The same automation's retry becomes the entry's trigger.
    let mut retry = entry(&a, &pa, TriggerKind::Retry, "Retry remaining 2");
    retry.retry_of = Some(run);
    let out = write(&fx, |c| queue::enqueue(c, &retry, T0 + 1));
    assert!(matches!(
        out,
        Enqueued::Merged {
            merged_count: 1,
            ..
        }
    ));
    let e = read(&fx, |c| queue::by_profile(c, &pa)).unwrap();
    assert_eq!((e.kind, e.retry_of), (TriggerKind::Retry, Some(run)));
    // Another automation's retry cannot ride on an entry that is not its own.
    write(&fx, |c| queue::remove(c, e.id));
    write(&fx, |c| {
        queue::enqueue(
            c,
            &entry(&other, &pa, TriggerKind::Watch, "Changes detected"),
            T0 + 2,
        )
    });
    let out = write(&fx, |c| queue::enqueue(c, &retry, T0 + 3));
    assert_eq!(out, Enqueued::Refused);
    // Another automation's ordinary trigger does merge (one run covers both).
    let out = write(&fx, |c| {
        queue::enqueue(
            c,
            &entry(&a, &pa, TriggerKind::Schedule, "Scheduled"),
            T0 + 4,
        )
    });
    assert!(matches!(out, Enqueued::Merged { .. }));
}

#[test]
fn pause_and_delete_drop_the_queue_and_the_pending_rerun() {
    let fx = Fixture::new();
    let (pa, a, pb, b) = two(&fx);
    write(&fx, |c| {
        queue::enqueue(c, &entry(&a, &pa, TriggerKind::Schedule, "Scheduled"), T0)
    });
    write(&fx, |c| {
        queue::enqueue(
            c,
            &entry(&b, &pb, TriggerKind::Watch, "Changes detected"),
            T0,
        )
    });
    write(&fx, |c| runtime::set_pending_rerun(c, &b, true, T0));
    crate::automation::approval::pause_in(
        &fx.store,
        &b,
        crate::automation::dto::PauseMode::Finish,
        fx.clock(),
    )
    .unwrap();
    assert!(
        read(&fx, |c| queue::by_profile(c, &pb)).is_none(),
        "RQ-8.2: queued entry removed"
    );
    assert!(
        read(&fx, |c| queue::by_profile(c, &pa)).is_some(),
        "other automations keep theirs"
    );
    let rt = read(&fx, |c| runtime::get(c, &b)).unwrap();
    assert!(!rt.pending_rerun, "RQ-8.2: pending rerun removed");
    // Deleting an automation removes its entries by cascade.
    crate::automation::approval::delete_in(&fx.store, &a).unwrap();
    assert!(read(&fx, queue::all).is_empty());
}

// ── Runtime ──

#[test]
fn the_trigger_state_owns_its_countdown_fields() {
    let fx = Fixture::new();
    let (_, _, _, b) = two(&fx);
    write(&fx, |c| {
        runtime::set_trigger_state(
            c,
            &b,
            TriggerState::Quiet,
            Some(T0 + 7_000),
            Some(T0),
            None,
            T0,
        )
    });
    let rt = read(&fx, |c| runtime::get(c, &b)).unwrap();
    assert_eq!(rt.trigger_state, Some(TriggerState::Quiet));
    assert_eq!(
        (rt.quiet_until_ms, rt.burst_started_ms),
        (Some(T0 + 7_000), Some(T0))
    );
    write(&fx, |c| {
        runtime::set_trigger_state(
            c,
            &b,
            TriggerState::Checking,
            Some(1),
            Some(2),
            Some("Resumed"),
            T0 + 1,
        )
    });
    let rt = read(&fx, |c| runtime::get(c, &b)).unwrap();
    assert_eq!(rt.trigger_state, Some(TriggerState::Checking));
    assert_eq!((rt.quiet_until_ms, rt.burst_started_ms), (None, None));
    assert_eq!(rt.check_reason.as_deref(), Some("Resumed"));
    write(&fx, |c| {
        runtime::set_trigger_state(
            c,
            &b,
            TriggerState::Watching,
            None,
            None,
            Some("stale"),
            T0 + 2,
        )
    });
    assert_eq!(
        read(&fx, |c| runtime::get(c, &b)).unwrap().check_reason,
        None
    );
    // A state this version does not know reads as `None`, never a panic.
    exec(
        &fx,
        &format!(
            "UPDATE automation_runtime SET trigger_state = 'mystery' WHERE automation_id = '{b}'"
        ),
    );
    assert_eq!(
        read(&fx, |c| runtime::get(c, &b)).unwrap().trigger_state,
        None
    );
    // A missing row updates nothing.
    assert!(!write(&fx, |c| runtime::set_pending_rerun(
        c, "no-such", true, T0
    )));
}

#[test]
fn a_damaged_attention_column_still_holds() {
    let fx = Fixture::new();
    let (_, a, _, _) = two(&fx);
    exec(
        &fx,
        &format!(
            "UPDATE automation_runtime SET attention = '{{not json' WHERE automation_id = '{a}'"
        ),
    );
    let rt = read(&fx, |c| runtime::get(c, &a)).unwrap();
    let att = rt.attention.expect("a damaged hold is still a hold");
    assert_eq!(att.kind, AttentionKind::Failed);
    // An unknown field is damage too (deny_unknown_fields).
    exec(
        &fx,
        &format!(
            "UPDATE automation_runtime SET attention = '{{\"kind\":\"partial\",\"runId\":1,\"text\":\"x\",\"extra\":1}}' WHERE automation_id = '{a}'"
        ),
    );
    assert!(
        read(&fx, |c| runtime::get(c, &a))
            .unwrap()
            .attention
            .is_some()
    );
}

// ── Runs ──

#[test]
fn run_ids_are_never_reused_after_a_delete() {
    let fx = Fixture::new();
    let (pa, a, _, _) = two(&fx);
    let mut ids = Vec::new();
    for n in 0..3 {
        let id = write(&fx, |c| {
            runs::start(c, &new_run(&a, "Nightly photos", &pa, T0 + n))
        });
        write(&fx, |c| {
            runs::finish(
                c,
                id,
                &finish(Outcome::Completed, RunCounts::default(), 0, T0 + n + 1),
            )
        });
        ids.push(id);
    }
    assert_eq!(ids[1], ids[0] + 1);
    // Delete the NEWEST run: a plain rowid table would hand its number out again.
    assert_eq!(write(&fx, |c| runs::delete(c, ids[2])), Deleted::Deleted);
    let next = write(&fx, |c| {
        runs::start(c, &new_run(&a, "Nightly photos", &pa, T0 + 10))
    });
    assert!(next > ids[2], "{next} must not reuse {}", ids[2]);
    // Delete everything: numbering still goes on.
    for id in [ids[0], ids[1]] {
        write(&fx, |c| runs::delete(c, id));
    }
    write(&fx, |c| {
        runs::finish(
            c,
            next,
            &finish(Outcome::Completed, RunCounts::default(), 0, T0 + 11),
        )
    });
    write(&fx, |c| runs::delete(c, next));
    let again = write(&fx, |c| {
        runs::start(c, &new_run(&a, "Nightly photos", &pa, T0 + 20))
    });
    assert!(again > next);
    assert_eq!(write(&fx, |c| runs::delete(c, 9_999)), Deleted::Missing);
}

#[test]
fn run_ops_keep_the_first_5000_and_the_totals_stay_intact() {
    let fx = Fixture::new();
    let (pa, a, _, _) = two(&fx);
    let run = write(&fx, |c| {
        runs::start(c, &new_run(&a, "Nightly photos", &pa, T0))
    });
    let paths: Vec<String> = (0..5_200)
        .map(|n| format!("dir\\file-{n:05}.txt"))
        .collect();
    let ops: Vec<NewOp<'_>> = paths
        .iter()
        .map(|p| NewOp {
            kind: OpKind::CopyNew,
            side: OpSide::Right,
            rel_path: p,
            size: Some(10),
            status: OpStatus::Pending,
        })
        .collect();
    let first = write(&fx, |c| runs::add_ops(c, run, &ops[..4_000]));
    assert_eq!((first.stored, first.dropped), (4_000, 0));
    let second = write(&fx, |c| runs::add_ops(c, run, &ops[4_000..]));
    assert_eq!((second.stored, second.dropped), (1_000, 200));
    let more = write(&fx, |c| runs::add_ops(c, run, &ops[..10]));
    assert_eq!(
        (more.stored, more.dropped),
        (0, 10),
        "full: nothing more is stored"
    );
    assert_eq!(read(&fx, |c| runs::stored_ops(c, run)), OPS_KEPT);

    let counts = RunCounts {
        ok: 5_100,
        failed: 3,
        not_started: 97,
        ..Default::default()
    };
    write(&fx, |c| runs::set_progress(c, run, 5_200, &counts));
    write(&fx, |c| {
        runs::finish(c, run, &finish(Outcome::Partial, counts, 5_200, T0 + 9))
    });
    let r = read(&fx, |c| runs::get(c, run)).unwrap();
    assert_eq!(r.ops_total, 5_200, "the true total survives the cap");
    assert_eq!(r.counts, counts, "counts survive the cap");
    // The page past the cap is empty; the page across the end is short.
    let tail = read(&fx, |c| runs::ops_page(c, run, 4_990, 50));
    assert_eq!(tail.len(), 10);
    assert_eq!(tail.last().unwrap().seq, 4_999);
    assert!(read(&fx, |c| runs::ops_page(c, run, 5_000, 50)).is_empty());
    // Order and content of a page.
    let head = read(&fx, |c| runs::ops_page(c, run, 0, 3));
    assert_eq!(
        head.iter().map(|o| o.seq).collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    assert_eq!(head[0].rel_path, "dir\\file-00000.txt");
    // Deleting the run takes its operations along.
    assert_eq!(
        write(&fx, |c| runs::delete(c, run)),
        Deleted::HeldByAttention
    );
    write(&fx, |c| {
        runs::resolve(c, run, None, "Resolved in Sync by you", T0 + 20)
    });
    assert_eq!(write(&fx, |c| runs::delete(c, run)), Deleted::Deleted);
    assert_eq!(read(&fx, |c| runs::stored_ops(c, run)), 0);
}

#[test]
fn op_status_updates_retarget_and_reject_bad_sizes() {
    let fx = Fixture::new();
    let (pa, a, _, _) = two(&fx);
    let run = write(&fx, |c| {
        runs::start(c, &new_run(&a, "Nightly photos", &pa, T0))
    });
    let ops: Vec<NewOp<'_>> = ["a", "b", "c"]
        .iter()
        .map(|p| NewOp {
            kind: OpKind::Update,
            side: OpSide::Left,
            rel_path: p,
            size: None,
            status: OpStatus::Pending,
        })
        .collect();
    write(&fx, |c| runs::add_ops(c, run, &ops));
    write(&fx, |c| {
        runs::set_op_status(c, run, 0, OpStatus::Done, None, None)
    });
    write(&fx, |c| {
        runs::set_op_status(
            c,
            run,
            1,
            OpStatus::Failed,
            Some("access_denied"),
            Some("Access is denied"),
        )
    });
    assert_eq!(
        write(&fx, |c| runs::retarget_ops(
            c,
            run,
            OpStatus::Pending,
            OpStatus::NotStarted
        )),
        1
    );
    let page = read(&fx, |c| runs::ops_page(c, run, 0, 10));
    let statuses: Vec<_> = page.iter().map(|o| o.status).collect();
    assert_eq!(
        statuses,
        vec![OpStatus::Done, OpStatus::Failed, OpStatus::NotStarted]
    );
    assert_eq!(page[1].error_code.as_deref(), Some("access_denied"));
    let bad = [NewOp {
        kind: OpKind::Delete,
        side: OpSide::Right,
        rel_path: "x",
        size: Some(-1),
        status: OpStatus::Pending,
    }];
    assert_eq!(
        fx.store.with_write(|c| runs::add_ops(c, run, &bad)),
        Err(StoreError::Invalid)
    );
}

#[test]
fn partial_conflict_and_failed_set_attention_the_others_do_not() {
    let fx = Fixture::new();
    let (pa, a, _, _) = two(&fx);
    let cases = [
        (
            Outcome::Partial,
            RunCounts {
                ok: 4,
                failed: 1,
                not_started: 1,
                ..Default::default()
            },
            None,
            Some(("1 failed \u{b7} 1 not started", AttentionKind::Partial)),
        ),
        (
            Outcome::Conflict,
            RunCounts {
                held: 1,
                ..Default::default()
            },
            None,
            Some(("1 conflict held for review", AttentionKind::Conflict)),
        ),
        (
            Outcome::Failed,
            RunCounts::default(),
            Some("Access denied"),
            Some(("Access denied", AttentionKind::Failed)),
        ),
        (
            Outcome::Failed,
            RunCounts {
                not_started: 2,
                ..Default::default()
            },
            None,
            Some(("2 not started", AttentionKind::Failed)),
        ),
        (
            Outcome::Cancelled,
            RunCounts {
                ok: 3,
                not_started: 3,
                ..Default::default()
            },
            None,
            None,
        ),
        (
            Outcome::Interrupted,
            RunCounts {
                ok: 3,
                not_started: 3,
                ..Default::default()
            },
            None,
            None,
        ),
        (Outcome::Skipped, RunCounts::default(), None, None),
        (
            Outcome::Completed,
            RunCounts {
                ok: 6,
                ..Default::default()
            },
            None,
            None,
        ),
    ];
    for (n, (outcome, counts, error, expected)) in cases.into_iter().enumerate() {
        write(&fx, |c| runtime::set_attention(c, &a, None, T0));
        let run = write(&fx, |c| {
            runs::start(c, &new_run(&a, "Nightly photos", &pa, T0 + n as i64))
        });
        let mut end = finish(outcome, counts, 6, T0 + 100);
        end.error_text = error;
        let set = write(&fx, |c| runs::finish(c, run, &end));
        let held = read(&fx, |c| runtime::get(c, &a)).unwrap().attention;
        match expected {
            Some((text, kind)) => {
                assert_eq!(
                    set,
                    Some(Attention {
                        kind,
                        run_id: run,
                        text: text.into()
                    }),
                    "{outcome:?}"
                );
                assert_eq!(held, set, "{outcome:?}");
            }
            None => {
                assert_eq!(set, None, "{outcome:?}");
                assert_eq!(held, None, "RQ-9.2 / no hold for {outcome:?}");
            }
        }
        assert_eq!(outcome.sets_attention(), expected.is_some());
    }
}

#[test]
fn a_manual_run_and_an_ended_run_never_touch_the_hold() {
    let fx = Fixture::new();
    let (pa, a, _, _) = two(&fx);
    let mut manual = new_run(&a, "x", &pa, T0);
    manual.automation_id = None;
    manual.automation_name = None;
    manual.host = RunHost::Manual;
    let run = write(&fx, |c| runs::start(c, &manual));
    let counts = RunCounts {
        failed: 1,
        ..Default::default()
    };
    assert_eq!(
        write(&fx, |c| runs::finish(
            c,
            run,
            &finish(Outcome::Failed, counts, 1, T0 + 1)
        )),
        None
    );
    assert!(
        read(&fx, |c| runtime::get(c, &a))
            .unwrap()
            .attention
            .is_none()
    );
    // A second finish of an ended run changes nothing.
    let again = write(&fx, |c| {
        runs::finish(
            c,
            run,
            &finish(Outcome::Completed, RunCounts::default(), 0, T0 + 9),
        )
    });
    assert_eq!(again, None);
    assert_eq!(
        read(&fx, |c| runs::get(c, run)).unwrap().outcome,
        Some(Outcome::Failed)
    );
    assert_eq!(
        fx.store.with_write(|c| runs::finish(
            c,
            4_242,
            &finish(Outcome::Completed, RunCounts::default(), 0, T0)
        )),
        Err(StoreError::NotFound)
    );
}

#[test]
fn a_run_that_needs_attention_is_never_pruned_op_2() {
    let fx = Fixture::new();
    let (pa, a, _, _) = two(&fx);
    // Running: not deletable.
    let running = write(&fx, |c| {
        runs::start(c, &new_run(&a, "Nightly photos", &pa, T0))
    });
    assert_eq!(write(&fx, |c| runs::delete(c, running)), Deleted::Running);
    let counts = RunCounts {
        ok: 1,
        failed: 1,
        ..Default::default()
    };
    write(&fx, |c| {
        runs::finish(c, running, &finish(Outcome::Partial, counts, 2, T0 + 1))
    });
    // The hold names it: not deletable.
    assert_eq!(
        write(&fx, |c| runs::delete(c, running)),
        Deleted::HeldByAttention
    );
    assert!(read(&fx, |c| runs::get(c, running)).is_some());
    // A later, clean run does not release it, and can itself be deleted.
    let later = write(&fx, |c| {
        runs::start(c, &new_run(&a, "Nightly photos", &pa, T0 + 2))
    });
    write(&fx, |c| {
        runs::finish(
            c,
            later,
            &finish(Outcome::Completed, RunCounts::default(), 0, T0 + 3),
        )
    });
    assert_eq!(write(&fx, |c| runs::delete(c, later)), Deleted::Deleted);
    assert_eq!(
        write(&fx, |c| runs::delete(c, running)),
        Deleted::HeldByAttention
    );
    // Resolved by a later run: the hold goes, the run can be deleted.
    let retry = write(&fx, |c| {
        runs::start(c, &new_run(&a, "Nightly photos", &pa, T0 + 4))
    });
    write(&fx, |c| {
        runs::finish(
            c,
            retry,
            &finish(
                Outcome::Completed,
                RunCounts {
                    ok: 1,
                    ..Default::default()
                },
                1,
                T0 + 5,
            ),
        )
    });
    assert!(write(&fx, |c| runs::resolve(
        c,
        running,
        Some(retry),
        "Resolved by run",
        T0 + 6
    )));
    let r = read(&fx, |c| runs::get(c, running)).unwrap();
    assert_eq!(
        (r.resolved_by, r.resolved_text.as_deref()),
        (Some(retry), Some("Resolved by run"))
    );
    assert!(
        read(&fx, |c| runtime::get(c, &a))
            .unwrap()
            .attention
            .is_none()
    );
    assert_eq!(write(&fx, |c| runs::delete(c, running)), Deleted::Deleted);
    // A damaged hold protects every run of its automation.
    let other = write(&fx, |c| {
        runs::start(c, &new_run(&a, "Nightly photos", &pa, T0 + 7))
    });
    write(&fx, |c| {
        runs::finish(
            c,
            other,
            &finish(Outcome::Completed, RunCounts::default(), 0, T0 + 8),
        )
    });
    exec(
        &fx,
        &format!("UPDATE automation_runtime SET attention = 'garbage' WHERE automation_id = '{a}'"),
    );
    assert_eq!(
        write(&fx, |c| runs::delete(c, other)),
        Deleted::HeldByAttention
    );
    // Resolving against a run that does not exist is refused.
    assert_eq!(
        fx.store
            .with_write(|c| runs::resolve(c, other, Some(9_999), "x", T0)),
        Err(StoreError::NotFound)
    );
}

#[test]
fn history_pages_newest_first_and_filters_attention() {
    let fx = Fixture::new();
    let (pa, a, pb, b) = two(&fx);
    let mut ids = Vec::new();
    for n in 0..7 {
        let (auto, name, profile) = if n % 2 == 0 {
            (&a, "Nightly photos", &pa)
        } else {
            (&b, "Projects watch", &pb)
        };
        let run = write(&fx, |c| {
            runs::start(c, &new_run(auto, name, profile, T0 + n))
        });
        let outcome = if n == 4 {
            Outcome::Partial
        } else {
            Outcome::Completed
        };
        let counts = RunCounts {
            ok: 1,
            failed: i64::from(n == 4),
            ..Default::default()
        };
        write(&fx, |c| {
            runs::finish(c, run, &finish(outcome, counts, 2, T0 + n + 1))
        });
        ids.push(run);
    }
    let page1 = read(&fx, |c| runs::history(c, HistoryFilter::All, None, None, 3));
    assert_eq!(
        page1.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![ids[6], ids[5], ids[4]]
    );
    let page2 = read(&fx, |c| {
        runs::history(c, HistoryFilter::All, None, Some(ids[4]), 3)
    });
    assert_eq!(
        page2.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![ids[3], ids[2], ids[1]]
    );
    let mine = read(&fx, |c| {
        runs::history(c, HistoryFilter::All, Some(&b), None, 10)
    });
    assert_eq!(
        mine.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![ids[5], ids[3], ids[1]]
    );
    let attention = read(&fx, |c| {
        runs::history(c, HistoryFilter::Attention, None, None, 10)
    });
    assert_eq!(
        attention.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![ids[4]]
    );
    assert!(
        read(&fx, |c| runs::history(
            c,
            HistoryFilter::Attention,
            Some(&b),
            None,
            10
        ))
        .is_empty()
    );
    // Last finished run per automation.
    let last = read(&fx, runs::last_finished_by_automation);
    assert_eq!(last[&a].id, ids[6]);
    assert_eq!(last[&b].id, ids[5]);
    // The recheck record round-trips and is bounded.
    let rec = RecheckRecord {
        state: RecheckState::Remaining,
        remaining: 2,
        text: "Fresh comparison: 2 operations remaining".into(),
        at_ms: T0 + 50,
    };
    write(&fx, |c| runs::set_recheck(c, ids[4], Some(&rec)));
    assert_eq!(
        read(&fx, |c| runs::get(c, ids[4])).unwrap().recheck,
        Some(rec)
    );
}

// ── Events ──

#[test]
fn events_keep_the_newest_200_per_automation() {
    let fx = Fixture::new();
    let (_, a, _, b) = two(&fx);
    let own_before = read(&fx, |c| events::count(c, &b));
    for n in 0..250 {
        write(&fx, |c| {
            events::add_event(c, &a, T0 + n, EventKind::Info, &format!("event {n}"))
        });
    }
    assert_eq!(read(&fx, |c| events::count(c, &a)), EVENTS_KEPT);
    assert_eq!(
        read(&fx, |c| events::count(c, &b)),
        own_before,
        "other automations are untouched"
    );
    let newest = read(&fx, |c| events::events(c, &a, 3));
    assert_eq!(
        newest.iter().map(|e| e.text.clone()).collect::<Vec<_>>(),
        vec!["event 249", "event 248", "event 247"]
    );
    let all = read(&fx, |c| events::events(c, &a, 500));
    assert_eq!(all.len() as i64, EVENTS_KEPT);
    assert_eq!(all.last().unwrap().text, "event 50");
}

#[test]
fn event_pages_follow_the_cursor_and_text_is_bounded() {
    let fx = Fixture::new();
    let (pa, a, _, _) = two(&fx);
    let run = write(&fx, |c| {
        runs::start(c, &new_run(&a, "Nightly photos", &pa, T0))
    });
    for n in 0..10 {
        write(&fx, |c| {
            events::add_event_for_run(
                c,
                &a,
                T0 + n,
                EventKind::Mute,
                &format!("ignored {n}"),
                Some(run),
            )
        });
    }
    let p1 = read(&fx, |c| events::page(c, &a, None, 4));
    let p2 = read(&fx, |c| events::page(c, &a, Some(p1.last().unwrap().id), 4));
    assert_eq!(p1[0].text, "ignored 9");
    assert_eq!(p2[0].text, "ignored 5");
    assert!(
        p1.iter()
            .chain(&p2)
            .all(|e| e.kind == "mute" && e.run_id == Some(run))
    );
    // A clock that steps backwards does not reorder the log (insertion order is the order).
    write(&fx, |c| {
        events::add_event(c, &a, T0 - 99_000, EventKind::Warn, "after a clock step")
    });
    assert_eq!(
        read(&fx, |c| events::events(c, &a, 1))[0].text,
        "after a clock step"
    );
    let long = format!("a\nb{}", "z".repeat(900));
    write(&fx, |c| events::add_event(c, &a, T0, EventKind::Bad, &long));
    let text = read(&fx, |c| events::events(c, &a, 1))[0].text.clone();
    assert_eq!(text.chars().count(), EVENT_MAX_CHARS);
    assert!(text.ends_with('\u{2026}') && !text.contains('\n'));
}

// ── Controls ──

fn ctl(action: Action, auto: Option<&str>, run: Option<i64>) -> NewControl<'_> {
    NewControl {
        action,
        automation_id: auto,
        run_id: run,
    }
}

#[test]
fn controls_older_than_ten_minutes_are_ignored() {
    let fx = Fixture::new();
    let (_, a, _, _) = two(&fx);
    let now = T0 + 3 * TTL_MS;
    let fresh = write(&fx, |c| {
        controls::post(c, &ctl(Action::RunNow, Some(&a), None), now - 1_000)
    });
    let edge = write(&fx, |c| {
        controls::post(c, &ctl(Action::PauseFinish, Some(&a), None), now - TTL_MS)
    });
    let old = write(&fx, |c| {
        controls::post(c, &ctl(Action::PauseStop, Some(&a), None), now - TTL_MS - 1)
    });
    let ancient = write(&fx, |c| {
        controls::post(c, &ctl(Action::Reload, None, None), now - 3 * TTL_MS)
    });
    let ids: Vec<i64> = read(&fx, |c| controls::pending(c, now))
        .iter()
        .map(|c| c.id)
        .collect();
    assert_eq!(
        ids,
        vec![fresh, edge],
        "exactly ten minutes still counts; ten minutes and a millisecond does not"
    );
    assert!(!ids.contains(&old) && !ids.contains(&ancient));
    assert!(controls::is_live(now - TTL_MS, now));
    assert!(!controls::is_live(now - TTL_MS - 1, now));
    // An expired control cannot be answered either.
    assert!(!write(&fx, |c| controls::mark_handled(
        c, old, "queued", now
    )));
    // A creation time far in the future does not live for ever.
    let forged = write(&fx, |c| {
        controls::post(c, &ctl(Action::Reload, None, None), now + SKEW_MS + 1)
    });
    assert!(
        !read(&fx, |c| controls::pending(c, now))
            .iter()
            .any(|c| c.id == forged)
    );
    assert!(
        controls::is_live(now + SKEW_MS, now),
        "small clock skew between processes is fine"
    );
    // The sweep removes what is out of its window, handled or not.
    assert!(write(&fx, |c| controls::mark_handled(
        c, fresh, "queued", now
    )));
    let swept = write(&fx, |c| controls::sweep(c, now + TTL_MS + 2_000));
    assert!(swept >= 3, "expired and forged rows go: {swept}");
    assert!(read(&fx, |c| controls::get(c, old)).is_none());
}

#[test]
fn controls_validate_their_target_and_record_the_answer() {
    let fx = Fixture::new();
    let (pa, a, _, _) = two(&fx);
    let run = write(&fx, |c| {
        runs::start(c, &new_run(&a, "Nightly photos", &pa, T0))
    });
    // The target must match the action.
    for bad in [
        ctl(Action::RunNow, None, None),
        ctl(Action::RunNow, Some(&a), Some(run)),
        ctl(Action::Stop, Some(&a), None),
        ctl(Action::Stop, None, None),
        ctl(Action::Stop, None, Some(0)),
        ctl(Action::Reload, Some(&a), None),
        ctl(Action::RestartRunner, None, Some(run)),
    ] {
        assert_eq!(
            fx.store.with_write(|c| controls::post(c, &bad, T0)),
            Err(StoreError::Invalid)
        );
    }
    let stop = write(&fx, |c| {
        controls::post(c, &ctl(Action::Stop, None, Some(run)), T0)
    });
    let rerun = write(&fx, |c| {
        controls::post(c, &ctl(Action::Recheck, None, Some(run)), T0)
    });
    write(&fx, |c| {
        controls::post(c, &ctl(Action::RestartRunner, None, None), T0)
    });
    let for_run = read(&fx, |c| controls::pending_for(c, &a, &[run], T0 + 1));
    assert_eq!(
        for_run.iter().map(|c| c.id).collect::<Vec<_>>(),
        vec![stop, rerun]
    );
    // Answering: once, with a bounded single-line text.
    assert!(write(&fx, |c| controls::mark_handled(
        c,
        stop,
        &format!("held\n{}", "x".repeat(200)),
        T0 + 2
    )));
    assert!(
        !write(&fx, |c| controls::mark_handled(c, stop, "again", T0 + 3)),
        "answered once"
    );
    let got = read(&fx, |c| controls::get(c, stop)).unwrap();
    assert_eq!(got.handled_ms, Some(T0 + 2));
    let result = got.result.unwrap();
    assert!(result.chars().count() <= controls::RESULT_MAX && !result.contains('\n'));
    assert!(
        !read(&fx, |c| controls::pending(c, T0 + 4))
            .iter()
            .any(|c| c.id == stop)
    );
    // Every action round-trips its name.
    for action in [
        Action::RunNow,
        Action::Stop,
        Action::PauseFinish,
        Action::PauseStop,
        Action::Recheck,
        Action::Retry,
        Action::Reload,
        Action::RestartRunner,
    ] {
        assert_eq!(Action::parse(action.as_str()), Some(action));
    }
    assert_eq!(Action::parse("rm -rf"), None);
}

#[test]
fn a_control_row_with_an_unknown_action_is_rejected_by_the_schema() {
    let fx = Fixture::new();
    let r = fx.store.with_write(|c| -> Result<(), StoreError> {
        c.execute(
            "INSERT INTO controls(action, created_ms) VALUES ('format', 1)",
            [],
        )
        .map(|_| ())
        .map_err(|_| StoreError::Unavailable)
    });
    assert!(r.is_err());
}

#[test]
fn tables_survive_a_reopen_with_their_contents() {
    let fx = Fixture::new();
    let (pa, a, _, _) = two(&fx);
    let run = write(&fx, |c| {
        runs::start(c, &new_run(&a, "Nightly photos", &pa, T0))
    });
    write(&fx, |c| runs::set_phase(c, run, Phase::Sync));
    write(&fx, |c| events::add_event(c, &a, T0, EventKind::Ok, "kept"));
    let reopened = Store::open(&fx.store_dir(), "0.2.0");
    let (r, e) = reopened
        .with_read(|c| -> Result<_, StoreError> {
            Ok((runs::get(c, run)?, events::events(c, &a, 5)?))
        })
        .unwrap();
    assert_eq!(r.unwrap().phase, Phase::Sync);
    assert!(e.iter().any(|e| e.text == "kept"));
}
