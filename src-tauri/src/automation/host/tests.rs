//! Host tests (FER-524 / P1.3): the gate, every run path, the prototype flows, election,
//! crash recovery and the security properties the review of P1.1 asked for. A fake clock and a
//! scripted engine drive `Host::tick()`; the store, the keys and the leases are real.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use super::HostClass;
use super::request;
use super::test_support::*;
use crate::automation::dto::PauseMode;
use crate::automation::lease::{GuardOutcome, LeaseEnv, LeaseGuard};
use crate::automation::status::StatusKind;
use crate::automation::store::leases::{self, HolderKind};
use crate::automation::store::queue::TriggerKind;
use crate::automation::store::runs::Outcome;
use crate::automation::store::{StoreError, runtime};
use crate::sync_engine::roots;

const ALL_TRIGGERS: [TriggerKind; 9] = [
    TriggerKind::Schedule,
    TriggerKind::Watch,
    TriggerKind::RunNow,
    TriggerKind::Retry,
    TriggerKind::CatchUp,
    TriggerKind::PendingRerun,
    TriggerKind::Available,
    TriggerKind::Recheck,
    TriggerKind::Startup,
];

fn count_event(rig: &Rig, automation: &str, needle: &str) -> usize {
    rig.event_texts(automation)
        .iter()
        .filter(|t| t.contains(needle))
        .count()
}

fn set_attention(rig: &Rig, automation: &str, run_id: i64) {
    rig.sql(&format!(
        "UPDATE automation_runtime SET attention = '{{\"kind\":\"partial\",\"runId\":{run_id},\"text\":\"1 failed\"}}' WHERE automation_id = '{automation}'"
    ));
}

// ── A run, start to end ──

/// RQ-1 / RQ-9: a queued run passes the gate, holds a lease that covers its folders, calls the
/// engine once and records the run, its operations, counts and events; the lease is gone after.
#[test]
fn a_queued_run_goes_through_the_gate_and_is_recorded() {
    let rig = Rig::new();
    let (a, p) = rig.automation("Nightly");
    let host = rig.host(1, HostClass::App);
    rig.enqueue(&a, &p, TriggerKind::RunNow, HostClass::App);
    let before = rig.world.keys.count();
    tick(&host);

    let calls = rig.world.calls();
    assert_eq!(calls.len(), 1);
    assert!(calls[0].holds_lease && calls[0].lease_covers, "{calls:?}");
    assert!(!calls[0].dry_run);
    assert_eq!(
        calls[0].run_id,
        rig.runs()[0].id,
        "the engine got the run id of the history row"
    );
    assert!(
        calls[0].verify_loads_before > before,
        "the approval was verified by the gate before the engine ran"
    );
    let runs = rig.runs();
    assert_eq!(runs.len(), 1);
    let run = &runs[0];
    assert_eq!(run.outcome, Some(Outcome::Completed));
    assert_eq!(run.counts.ok, 3);
    assert_eq!(run.ops_total, 3);
    assert_eq!(run.trigger_text, "Test trigger");
    assert!(run.ended_ms.is_some());
    let ops = rig
        .fx
        .store
        .with_read(|c| crate::automation::store::runs::stored_ops(c, run.id))
        .unwrap();
    assert_eq!(ops, 3);
    assert_eq!(rig.queue_len(), 0, "the entry left the queue");
    assert!(
        rig.fx.store.with_read(leases::all).unwrap().is_empty(),
        "the lease was released"
    );
    let events = rig.event_texts(&a);
    assert!(events.iter().any(|e| e.contains("started")), "{events:?}");
    assert!(events.iter().any(|e| e.contains("completed")), "{events:?}");
}

// ── The gate: each refusal reason of design §10.4 ──

/// One refusal: no engine call, the entry as expected, ONE event, and the status.
fn assert_refused(rig: &Rig, a: &str, entry_kept: bool, status: StatusKind, event: &str) {
    assert!(
        rig.world.calls().is_empty(),
        "the engine must not be called"
    );
    assert_eq!(rig.queue_len(), usize::from(entry_kept));
    assert_eq!(count_event(rig, a, event), 1, "{:?}", rig.event_texts(a));
    assert_eq!(rig.status_kind(a), status, "{}", rig.status_label(a));
}

#[test]
fn gate_refuses_a_paused_automation() {
    let rig = Rig::new();
    let (a, p) = rig.automation("Paused one");
    let host = rig.host(1, HostClass::App);
    approval_pause(&rig, &a);
    rig.enqueue(&a, &p, TriggerKind::Schedule, HostClass::App);
    tick(&host);
    assert_refused(
        &rig,
        &a,
        false,
        StatusKind::Paused,
        "the automation is paused",
    );
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

#[test]
fn gate_refuses_an_automation_of_the_other_runtime() {
    let rig = Rig::new();
    let (a, p) = rig.automation("App one");
    // A background host never runs an in-app automation, even if a row says so.
    let host = rig.host(1, HostClass::Background);
    rig.enqueue(&a, &p, TriggerKind::Schedule, HostClass::Background);
    tick(&host);
    assert_refused(
        &rig,
        &a,
        false,
        StatusKind::Scheduled,
        "belongs to another runtime",
    );
}

#[test]
fn gate_refuses_a_changed_profile_and_the_enabled_flag_does_not_help() {
    let rig = Rig::new();
    let (a, p) = rig.automation("Reviewed");
    let host = rig.host(1, HostClass::App);
    let rev = rig.fx.profile_policy(&p); // keep the compiler honest about the helper
    let _ = rev;
    let current = rig
        .fx
        .store
        .with_read(|c| crate::automation::store::profiles::get(c, &p))
        .unwrap()
        .unwrap();
    rig.fx.bump_profile(&p, current.revision, "*.bak");
    // The automation is still ENABLED (the flag was never touched) ...
    assert!(
        rig.fx
            .store
            .with_read(|c| crate::automation::store::automations::get(c, &a))
            .unwrap()
            .unwrap()
            .enabled
    );
    rig.enqueue(&a, &p, TriggerKind::Watch, HostClass::App);
    tick(&host);
    // ... and still nothing runs: the approval no longer verifies.
    assert_refused(&rig, &a, false, StatusKind::Review, "needs review");
    assert!(
        rig.event_texts(&a)
            .iter()
            .any(|e| e.contains("Profile changed (rev 1")),
        "{:?}",
        rig.event_texts(&a)
    );
}

#[test]
fn gate_refuses_a_run_while_attention_is_set() {
    let rig = Rig::new();
    let (a, p) = rig.automation("Held one");
    let host = rig.host(1, HostClass::App);
    set_attention(&rig, &a, 9);
    for kind in [
        TriggerKind::Schedule,
        TriggerKind::RunNow,
        TriggerKind::Watch,
    ] {
        rig.enqueue(&a, &p, kind, HostClass::App);
        tick(&host);
    }
    assert!(rig.world.calls().is_empty());
    assert_eq!(rig.queue_len(), 0);
    assert_eq!(rig.status_kind(&a), StatusKind::Attention);
    // The same text is not logged again and again (three triggers, one event).
    assert_eq!(count_event(&rig, &a, "needs attention"), 1);
}

#[test]
fn a_condition_keeps_the_entry_queued_and_logs_once() {
    let rig = Rig::new();
    let (a, p) = rig.automation("Needs AC");
    let host = rig.host(1, HostClass::App);
    *rig.world.unmet.lock().unwrap() = Some("Waits for AC power");
    rig.enqueue(&a, &p, TriggerKind::Schedule, HostClass::App);
    tick(&host);
    rig.advance(6_000);
    tick(&host);
    rig.advance(6_000);
    tick(&host);
    assert_refused(&rig, &a, true, StatusKind::Queued, "Waits for AC power");
    // The condition clears: the same entry runs.
    *rig.world.unmet.lock().unwrap() = None;
    rig.advance(6_000);
    tick(&host);
    assert_eq!(rig.world.calls().len(), 1);
    assert_eq!(rig.queue_len(), 0);
}

#[test]
fn an_offline_folder_waits_for_the_device_and_runs_when_it_is_back() {
    let rig = Rig::new();
    let (a, p) = rig.automation("Offline");
    let host = rig.host(1, HostClass::App);
    let right = rig.fx.profile_policy(&p).right.path;
    let away = format!("{right}-away");
    std::fs::rename(&right, &away).unwrap();
    rig.enqueue(&a, &p, TriggerKind::Schedule, HostClass::App);
    tick(&host);
    rig.advance(6_000);
    tick(&host);
    assert_refused(&rig, &a, true, StatusKind::Device, "Waiting for device");
    // The folder comes back (the very same folder).
    std::fs::rename(&away, &right).unwrap();
    rig.advance(6_000);
    tick(&host);
    assert_eq!(rig.world.calls().len(), 1);
    assert_eq!(rig.queue_len(), 0);
    let state = rig
        .fx
        .store
        .with_read(|c| runtime::get(c, &a))
        .unwrap()
        .unwrap()
        .trigger_state;
    assert_eq!(
        state,
        Some(runtime::TriggerState::Scheduled),
        "left the device state"
    );
}

#[test]
fn a_folder_that_is_another_folder_now_is_not_run() {
    let rig = Rig::new();
    let (a, p) = rig.automation("Swapped");
    let host = rig.host(1, HostClass::App);
    let right = rig.fx.profile_policy(&p).right.path;
    // Delete and recreate: same path, another folder (T8).
    std::fs::remove_dir_all(&right).unwrap();
    std::fs::create_dir_all(&right).unwrap();
    rig.enqueue(&a, &p, TriggerKind::Schedule, HostClass::App);
    tick(&host);
    assert_refused(&rig, &a, true, StatusKind::Device, "Waiting for device");
}

#[test]
fn a_delete_policy_prerequisite_that_fails_is_a_failed_run_with_attention() {
    let rig = Rig::new();
    // A mirror deletes on the receiving side, so the Recycle Bin must be usable there.
    let (a, p) = rig.mirror_automation("Bin off");
    let host = rig.host(1, HostClass::App);
    *rig.world.bin.lock().unwrap() = crate::sync_engine::env::BinState::Disabled;
    rig.enqueue(&a, &p, TriggerKind::Schedule, HostClass::App);
    tick(&host);
    assert!(rig.world.calls().is_empty());
    assert_eq!(rig.queue_len(), 0);
    let runs = rig.runs();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].outcome, Some(Outcome::Failed));
    assert!(
        runs[0]
            .error_text
            .as_deref()
            .is_some_and(|t| t.contains("Recycle Bin")),
        "{:?}",
        runs[0].error_text
    );
    assert_eq!(rig.status_kind(&a), StatusKind::Attention);
    assert_eq!(
        count_event(&rig, &a, "failed before any file was touched"),
        1
    );
}

#[test]
fn busy_folders_keep_the_entry_queued_without_noise() {
    let rig = Rig::new();
    let (a, p) = rig.automation("Busy");
    let host = rig.host(1, HostClass::App);
    let policy = rig.fx.profile_policy(&p);
    let lease_roots = vec![
        roots::canonical(&policy.left.path).unwrap(),
        roots::canonical(&policy.right.path).unwrap(),
    ];
    let other: LeaseEnv = rig.lease_env(999);
    let manual = match LeaseGuard::acquire(
        &rig.fx.store,
        &other,
        HolderKind::Manual,
        None,
        &lease_roots,
        None,
    )
    .unwrap()
    {
        GuardOutcome::Acquired(g) => g,
        GuardOutcome::Busy(_) => panic!("free"),
    };
    rig.enqueue(&a, &p, TriggerKind::Schedule, HostClass::App);
    tick(&host);
    rig.advance(6_000);
    tick(&host);
    assert!(rig.world.calls().is_empty());
    assert_eq!(rig.queue_len(), 1);
    assert!(rig.event_texts(&a).iter().all(|e| !e.contains("busy")));
    drop(manual);
    rig.advance(6_000);
    tick(&host);
    assert_eq!(rig.world.calls().len(), 1);
}

/// The configuration may change while the folders are probed (here: between the first and the
/// second verification of one gate). The run must not start on the strength of the first look.
#[test]
fn a_change_between_the_two_verifications_stops_the_run() {
    let rig = Rig::new();
    let (a, p) = rig.automation("Raced");
    let host = rig.host(1, HostClass::App);
    let rev = rig
        .fx
        .store
        .with_read(|c| crate::automation::store::profiles::get(c, &p))
        .unwrap()
        .unwrap()
        .revision;
    let fired = Arc::new(AtomicBool::new(false));
    {
        // Edit the profile exactly once, while the first gate is between its two looks (a
        // policy-changing profile save, the same path as the user's save).
        let (store, fired, profile) = (rig.fx.store.clone(), fired.clone(), p.clone());
        *rig.world.between.lock().unwrap() = Some(Box::new(move || {
            if fired.swap(true, Ordering::SeqCst) {
                return;
            }
            let probe: Arc<dyn crate::sync_engine::roots::RootProbe> =
                Arc::new(crate::sync_engine::roots::FsProbe);
            let current = crate::automation::commands::get_in(&store, &profile, &probe).unwrap();
            let mut policy = current.policy.clone();
            policy.exclude.push("*.zzz".into());
            crate::automation::commands::save_in(
                &store,
                crate::automation::commands::SaveRequest {
                    id: Some(profile.clone()),
                    expected_revision: Some(rev),
                    draft: crate::automation::model::Draft {
                        name: current.name,
                        policy,
                    },
                },
                &probe,
                |_| true,
                1,
            )
            .unwrap();
        }));
    }
    rig.enqueue(&a, &p, TriggerKind::Schedule, HostClass::App);
    tick(&host);
    assert!(fired.load(Ordering::SeqCst));
    assert!(rig.world.calls().is_empty(), "the second look refused it");
    // The next try judges the new state: needs review, entry removed.
    *rig.world.between.lock().unwrap() = None;
    rig.advance(6_000);
    rig.enqueue(&a, &p, TriggerKind::Schedule, HostClass::App);
    tick(&host);
    assert!(rig.world.calls().is_empty());
    assert_eq!(rig.status_kind(&a), StatusKind::Review);
}

// ── Review P1.1 #1: every run path verifies the approval ──

/// The approval is tampered with (a wrong MAC): whatever queued the run - every trigger kind the
/// queue knows - the engine is never called, and the entry is removed with a review event.
#[test]
fn no_trigger_kind_reaches_the_engine_without_a_valid_approval() {
    let rig = Rig::new();
    let (a, p) = rig.automation("Tampered");
    let host = rig.host(1, HostClass::App);
    let mac: String = rig
        .fx
        .store
        .with_read(|c| -> Result<String, StoreError> {
            Ok(crate::automation::store::approvals::get(c, &a)?
                .unwrap()
                .mac)
        })
        .unwrap();
    rig.sql(&format!(
        "UPDATE approvals SET mac = '{}' WHERE automation_id = '{a}'",
        "0".repeat(64)
    ));
    for kind in ALL_TRIGGERS {
        if kind == TriggerKind::Retry {
            set_attention(&rig, &a, 1);
        }
        rig.enqueue(&a, &p, kind, HostClass::App);
        rig.advance(11_000);
        tick(&host);
        assert!(
            rig.world.calls().is_empty(),
            "{kind:?} reached the engine with a bad MAC"
        );
        assert_eq!(rig.queue_len(), 0, "{kind:?}");
    }
    assert_eq!(rig.status_kind(&a), StatusKind::Review);
    // With the genuine MAC back, each of them runs - and every engine call is preceded by a
    // verification of the approval key.
    rig.sql(&format!(
        "UPDATE approvals SET mac = '{mac}' WHERE automation_id = '{a}'"
    ));
    rig.sql(&format!(
        "UPDATE automation_runtime SET attention = NULL WHERE automation_id = '{a}'"
    ));
    for kind in ALL_TRIGGERS {
        if kind == TriggerKind::Retry {
            set_attention(&rig, &a, 1);
        }
        let before = rig.world.calls().len();
        let loads = rig.world.keys.count();
        rig.enqueue(&a, &p, kind, HostClass::App);
        rig.advance(11_000);
        tick(&host);
        let calls = rig.world.calls();
        assert_eq!(calls.len(), before + 1, "{kind:?} should run");
        let call = calls.last().unwrap();
        assert!(
            call.verify_loads_before >= loads + 2,
            "{kind:?}: verified twice"
        );
        assert!(call.holds_lease && call.lease_covers, "{kind:?}");
        if kind == TriggerKind::Retry {
            assert!(matches!(call.mode, super::RunMode::Retry { of: 1 }));
            rig.sql(&format!(
                "UPDATE automation_runtime SET attention = NULL WHERE automation_id = '{a}'"
            ));
        }
    }
}

/// A recheck is read-only but is gated as well: a bad approval means no comparison at all.
#[test]
fn a_recheck_is_gated_too() {
    let rig = Rig::new();
    let (a, p) = rig.automation("Recheck gate");
    let host = rig.host(1, HostClass::App);
    rig.world.set_script(script(|_, _| partial(3, 1, 2)));
    rig.enqueue(&a, &p, TriggerKind::RunNow, HostClass::App);
    tick(&host);
    let run_id = rig.runs()[0].id;
    assert_eq!(rig.status_kind(&a), StatusKind::Attention);

    let mac: String = rig
        .fx
        .store
        .with_read(|c| -> Result<String, StoreError> {
            Ok(crate::automation::store::approvals::get(c, &a)?
                .unwrap()
                .mac)
        })
        .unwrap();
    rig.sql(&format!(
        "UPDATE approvals SET mac = '{}' WHERE automation_id = '{a}'",
        "f".repeat(64)
    ));
    request::recheck_command(&rig.fx.store, run_id, rig.now()).unwrap();
    rig.advance(1_000);
    tick(&host);
    assert_eq!(
        rig.world.calls().len(),
        1,
        "only the original run called the engine"
    );
    assert!(
        rig.event_texts(&a)
            .iter()
            .any(|e| e.contains("Recheck not done: needs review")),
        "{:?}",
        rig.event_texts(&a)
    );

    rig.sql(&format!(
        "UPDATE approvals SET mac = '{mac}' WHERE automation_id = '{a}'"
    ));
    rig.world.set_script(script(|_, call| {
        if call.dry_run {
            dry_run(2, 0)
        } else {
            partial(3, 1, 2)
        }
    }));
    request::recheck_command(&rig.fx.store, run_id, rig.now()).unwrap();
    rig.advance(1_000);
    tick(&host);
    let calls = rig.world.calls();
    assert_eq!(calls.len(), 2);
    assert!(calls[1].dry_run && !calls[1].holds_lease);
    assert!(matches!(calls[1].mode, super::RunMode::Recheck { .. }));
}

// ── Review P1.1 #2: no engine from inside a store closure ──

#[test]
fn the_engine_is_refused_from_inside_a_store_closure() {
    let rig = Rig::new();
    let (a, p) = rig.automation("Closure");
    let host = rig.host(1, HostClass::App);
    let store = rig.fx.store.clone();
    let seen = Arc::new(Mutex::new(None));
    {
        let seen = seen.clone();
        rig.world.set_script(script(move |proof, call| {
            // What a buggy caller would do: call the engine while holding the store.
            let baselines =
                crate::automation::profile_baseline::ProfileBaselineStore::unavailable();
            let env = crate::sync_engine::test_support::test_env();
            let out = store
                .with_read(|_| -> Result<_, StoreError> {
                    Ok(super::engine::run_gated(proof, call, &baselines, &env))
                })
                .unwrap();
            *seen.lock().unwrap() = Some(out.outcome.clone());
            completed(1)
        }));
    }
    rig.enqueue(&a, &p, TriggerKind::RunNow, HostClass::App);
    tick(&host);
    let outcome = seen.lock().unwrap().clone().expect("script ran");
    assert!(
        matches!(&outcome, crate::sync_engine::EngineOutcome::Failed { error } if error.code == "internal"),
        "{outcome:?}"
    );
}

#[test]
fn a_run_proof_cannot_be_used_for_a_dry_run_and_the_other_way_round() {
    let rig = Rig::new();
    let (a, p) = rig.automation("Modes");
    let host = rig.host(1, HostClass::App);
    let store = rig.fx.store.clone();
    let seen = Arc::new(Mutex::new(None));
    {
        let seen = seen.clone();
        let _ = &store;
        rig.world.set_script(script(move |proof, call| {
            let baselines =
                crate::automation::profile_baseline::ProfileBaselineStore::unavailable();
            let env = crate::sync_engine::test_support::test_env();
            let flipped = super::EngineCall {
                run_id: call.run_id,
                retries: 0,
                dry_run: true, // a RUN proof asked to compare read-only
                incremental: false,
                cancel: call.cancel.clone(),
                sink: call.sink,
            };
            let out = super::engine::run_gated(proof, &flipped, &baselines, &env);
            *seen.lock().unwrap() = Some(out.outcome.clone());
            completed(1)
        }));
    }
    rig.enqueue(&a, &p, TriggerKind::RunNow, HostClass::App);
    tick(&host);
    let outcome = seen.lock().unwrap().clone().expect("script ran");
    assert!(
        matches!(&outcome, crate::sync_engine::EngineOutcome::Failed { error } if error.code == "internal"),
        "{outcome:?}"
    );
}

// ── Source tests: the engine has one door, and the proof has one maker ──

fn host_sources() -> Vec<(String, String)> {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("automation");
    let mut files = vec![root.join("host.rs")];
    for entry in std::fs::read_dir(root.join("host")).unwrap().flatten() {
        let path = entry.path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if path.extension().is_some_and(|e| e == "rs") && !name.starts_with("test") {
            files.push(path);
        }
    }
    files
        .into_iter()
        .map(|p| {
            (
                p.file_name().unwrap().to_string_lossy().into_owned(),
                std::fs::read_to_string(&p).unwrap(),
            )
        })
        .collect()
}

/// Review P1.1 #1: the engine's request type (and with it the `run` call) appears in
/// `engine.rs` only, and that file's only entry point takes the gate's proof.
#[test]
fn only_engine_rs_calls_the_engine() {
    let sources = host_sources();
    assert!(sources.len() >= 6);
    for (name, text) in &sources {
        let code: String = text
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        if name == "engine.rs" {
            assert!(code.contains("pub fn run_gated(\n    proof: &GateOk,"));
            continue;
        }
        for needle in ["EngineRequest", "execute::run", "sync_engine::run"] {
            assert!(!code.contains(needle), "{name} mentions {needle}");
        }
    }
}

/// Nothing but the gate builds the proof.
#[test]
fn only_the_gate_builds_the_proof() {
    for (name, text) in host_sources() {
        if name == "gate.rs" {
            continue;
        }
        let code: String = text
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!code.contains("GateOk {"), "{name} builds a GateOk");
    }
}

/// The host and its parts stay free of the app runtime (the runner links them).
#[test]
fn the_host_has_no_app_runtime_dependency() {
    let needle = concat!("tauri", "::");
    for (name, text) in host_sources() {
        assert!(!text.contains(needle), "{name}");
    }
}

/// No panic can come out of the host loop or the request functions (they run on the IPC path
/// and on the loop thread): the code has no `unwrap`, `expect`, `panic!` or `unreachable!`.
#[test]
fn the_host_code_cannot_panic_on_its_own() {
    for (name, text) in host_sources() {
        for (n, line) in text.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            for needle in [".unwrap()", ".expect(", "panic!(", "unreachable!("] {
                assert!(!code.contains(needle), "{name}:{} uses {needle}", n + 1);
            }
        }
    }
}

/// The host never builds a `GuardAck` (the native "yes" to a tripped delete guard exists only for
/// a manual run): a guard that trips in an unattended run fails the run (SP-5.1).
#[test]
fn the_host_never_acknowledges_a_tripped_guard() {
    for (name, text) in host_sources() {
        assert!(!text.contains("GuardAck"), "{name}");
        assert!(!text.contains("from_native_yes"), "{name}");
    }
}
