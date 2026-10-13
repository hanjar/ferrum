#[path = "../../../../src-tauri/src/ai/executor.rs"]
mod executor;

use executor::*;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

static N: AtomicU32 = AtomicU32::new(0);

fn journal_path() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "ferrum-ai-exec-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    fs::create_dir_all(&dir).unwrap();
    dir.join("ledger.log")
}

fn fresh(r: Reservation) -> String {
    match r {
        Reservation::Fresh(k) => k,
        other => panic!("expected Fresh, got {other:?}"),
    }
}

#[test]
fn key_is_bound_to_execution_and_digest() {
    let a = idempotency_key("exe_1", "d1");
    assert_eq!(a, idempotency_key("exe_1", "d1"));
    assert_ne!(a, idempotency_key("exe_2", "d1"));
    assert_ne!(a, idempotency_key("exe_1", "d2"));
    assert!(a.starts_with("idem_"));
}

#[test]
fn double_submit_reserves_once_and_returns_existing() {
    let l = ExecutionLedger::in_memory();
    let key = fresh(l.reserve("exe_1", "plan_1", "d1").unwrap());
    l.record_enqueued(&key, &["job_1".into()]).unwrap();
    match l.reserve("exe_1", "plan_1", "d1").unwrap() {
        Reservation::Existing(e) => {
            assert_eq!(e.job_ids, vec!["job_1".to_string()]);
            assert_eq!(e.state, ExecState::Enqueued);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn provider_retry_with_new_execution_id_for_same_plan_gets_existing() {
    let l = ExecutionLedger::in_memory();
    let key = fresh(l.reserve("exe_1", "plan_1", "d1").unwrap());
    l.record_enqueued(&key, &["job_1".into()]).unwrap();
    assert!(matches!(
        l.reserve("exe_2", "plan_1", "d1").unwrap(),
        Reservation::Existing(_)
    ));
}

#[test]
fn concurrent_reserves_yield_exactly_one_fresh() {
    let l = Arc::new(ExecutionLedger::in_memory());
    let handles: Vec<_> = (0..16)
        .map(|_| {
            let l = Arc::clone(&l);
            std::thread::spawn(move || matches!(l.reserve("exe_1", "plan_1", "d1").unwrap(), Reservation::Fresh(_)))
        })
        .collect();
    let fresh_count = handles.into_iter().map(|h| h.join().unwrap()).filter(|b| *b).count();
    assert_eq!(fresh_count, 1);
}

#[test]
fn crash_between_enqueue_and_receipt_becomes_unknown_outcome_never_replayed() {
    let path = journal_path();
    {
        let l = ExecutionLedger::open(&path).unwrap();
        let _key = fresh(l.reserve("exe_1", "plan_1", "d1").unwrap());
        // process dies here: enqueue may or may not have happened, no record_enqueued
    }
    let l = ExecutionLedger::open(&path).unwrap();
    let unresolved = l.unresolved();
    assert_eq!(unresolved.len(), 1);
    assert_eq!(unresolved[0].state, ExecState::UnknownOutcome);
    // a retry (same or new execution id) is NOT allowed to enqueue again
    assert!(matches!(l.reserve("exe_1", "plan_1", "d1").unwrap(), Reservation::Existing(_)));
    assert!(matches!(l.reserve("exe_9", "plan_1", "d1").unwrap(), Reservation::Existing(_)));
    let summary = summarize(&unresolved[0], &|_| None);
    assert_eq!(summary.outcome, RunOutcome::UnknownOutcome);
}

#[test]
fn reconcile_found_adopts_jobs_and_not_found_allows_a_new_plan_attempt() {
    let path = journal_path();
    {
        let l = ExecutionLedger::open(&path).unwrap();
        fresh(l.reserve("exe_1", "plan_1", "d1").unwrap());
        fresh(l.reserve("exe_2", "plan_2", "d2").unwrap());
    }
    let l = ExecutionLedger::open(&path).unwrap();
    let k1 = idempotency_key("exe_1", "d1");
    let k2 = idempotency_key("exe_2", "d2");
    let e1 = l.resolve_unknown(&k1, Resolution::Found(vec!["job_7".into()])).unwrap();
    assert_eq!(e1.state, ExecState::Enqueued);
    assert!(l.owns_job("exe_1", "job_7"));
    let e2 = l.resolve_unknown(&k2, Resolution::NotFound).unwrap();
    assert_eq!(e2.state, ExecState::NotStarted);
    let k3 = fresh(l.reserve("exe_3", "plan_2", "d2").unwrap());
    // (a reserve left unrecorded would itself be unknown after a restart)
    l.record_enqueued(&k3, &["job_8".into()]).unwrap();
    // resolution survives another restart
    drop(l);
    let l = ExecutionLedger::open(&path).unwrap();
    assert!(l.owns_job("exe_1", "job_7"));
    assert!(l.unresolved().is_empty());
    // resolving a non-unknown entry is refused
    assert_eq!(l.resolve_unknown(&k1, Resolution::NotFound), Err(LedgerError::BadState));
}

#[test]
fn receipt_mapping_survives_restart_after_enqueue() {
    let path = journal_path();
    {
        let l = ExecutionLedger::open(&path).unwrap();
        let k = fresh(l.reserve("exe_1", "plan_1", "d1").unwrap());
        l.record_enqueued(&k, &["job_1".into(), "job_2".into()]).unwrap();
    }
    let l = ExecutionLedger::open(&path).unwrap();
    let e = l.by_execution("exe_1").unwrap();
    assert_eq!(e.job_ids, vec!["job_1".to_string(), "job_2".to_string()]);
    assert!(matches!(l.reserve("exe_1", "plan_1", "d1").unwrap(), Reservation::Existing(_)));
}

#[test]
fn torn_journal_line_is_ignored() {
    let path = journal_path();
    {
        let l = ExecutionLedger::open(&path).unwrap();
        let k = fresh(l.reserve("exe_1", "plan_1", "d1").unwrap());
        l.record_enqueued(&k, &["job_1".into()]).unwrap();
    }
    let mut text = fs::read_to_string(&path).unwrap();
    text.push_str("R\tidem_torn\texe_");
    fs::write(&path, text).unwrap();
    let l = ExecutionLedger::open(&path).unwrap();
    assert!(l.owns_job("exe_1", "job_1"));
    assert!(l.unresolved().is_empty());
}

#[test]
fn not_started_failure_releases_the_plan_for_a_new_reserve() {
    let l = ExecutionLedger::in_memory();
    let k = fresh(l.reserve("exe_1", "plan_1", "d1").unwrap());
    l.record_not_started(&k).unwrap();
    assert!(matches!(l.reserve("exe_1", "plan_1", "d1").unwrap(), Reservation::Fresh(_)));
}

#[test]
fn mark_unknown_after_enqueue_error_blocks_replay() {
    let l = ExecutionLedger::in_memory();
    let k = fresh(l.reserve("exe_1", "plan_1", "d1").unwrap());
    l.mark_unknown(&k).unwrap();
    assert!(matches!(l.reserve("exe_1", "plan_1", "d1").unwrap(), Reservation::Existing(e) if e.state == ExecState::UnknownOutcome));
    // a late record_enqueued (receipt arrives after all) is accepted
    l.record_enqueued(&k, &["job_1".into()]).unwrap();
    assert_eq!(l.lookup(&k).unwrap().state, ExecState::Enqueued);
}

#[test]
fn invalid_references_and_injection_are_rejected() {
    let l = ExecutionLedger::in_memory();
    for bad in ["", "a b", "a\tb", "a\nb", "../x", &"a".repeat(129)] {
        assert_eq!(l.reserve(bad, "plan_1", "d1"), Err(LedgerError::InvalidReference), "{bad:?}");
        assert_eq!(l.reserve("exe_1", bad, "d1"), Err(LedgerError::InvalidReference));
    }
    let k = fresh(l.reserve("exe_1", "plan_1", "d1").unwrap());
    assert_eq!(l.record_enqueued(&k, &["job\n1".into()]), Err(LedgerError::InvalidReference));
    assert_eq!(l.record_enqueued("nope", &[]), Err(LedgerError::UnknownKey));
}

#[test]
fn cancel_and_progress_route_only_to_owned_jobs() {
    let l = ExecutionLedger::in_memory();
    let k1 = fresh(l.reserve("exe_1", "plan_1", "d1").unwrap());
    l.record_enqueued(&k1, &["job_1".into()]).unwrap();
    let k2 = fresh(l.reserve("exe_2", "plan_2", "d2").unwrap());
    l.record_enqueued(&k2, &["job_2".into()]).unwrap();
    assert!(l.owns_job("exe_1", "job_1"));
    assert!(!l.owns_job("exe_1", "job_2"));
    assert!(!l.owns_job("exe_unknown", "job_1"));
    assert_eq!(l.cancel_targets("exe_1"), vec!["job_1".to_string()]);
    assert!(l.cancel_targets("exe_unknown").is_empty());
    // an unknown-outcome execution owns nothing until reconciled
    let k3 = fresh(l.reserve("exe_3", "plan_3", "d3").unwrap());
    l.mark_unknown(&k3).unwrap();
    assert!(l.cancel_targets("exe_3").is_empty());
}

fn status(state: JobState, files: u64, failures: u64) -> JobStatus {
    JobStatus { state, processed_files: files, failure_count: failures }
}

fn entry(jobs: &[&str]) -> Entry {
    Entry {
        key: "idem_x".into(),
        execution_id: "exe_1".into(),
        plan_id: "plan_1".into(),
        plan_digest: "d1".into(),
        state: ExecState::Enqueued,
        job_ids: jobs.iter().map(|s| s.to_string()).collect(),
    }
}

#[test]
fn summary_comes_from_engine_job_status() {
    let mut map: HashMap<&str, JobStatus> = HashMap::new();
    let e = entry(&["job_1"]);
    let s = |m: &HashMap<&str, JobStatus>| summarize(&e, &|id| m.get(id).copied());
    assert_eq!(s(&map).outcome, RunOutcome::UnknownOutcome); // missing job is never success
    map.insert("job_1", status(JobState::Queued, 0, 0));
    assert_eq!(s(&map).outcome, RunOutcome::Queued);
    map.insert("job_1", status(JobState::Running, 3, 0));
    assert_eq!(s(&map).outcome, RunOutcome::Running);
    map.insert("job_1", status(JobState::Completed, 12, 0));
    assert_eq!(s(&map), Summary { outcome: RunOutcome::Completed, completed_files: 12, failed: 0 });
    // cancel after a partial copy: copied files are reported, not rolled back
    map.insert("job_1", status(JobState::Cancelled, 5, 0));
    assert_eq!(s(&map), Summary { outcome: RunOutcome::Partial, completed_files: 5, failed: 0 });
    map.insert("job_1", status(JobState::Cancelled, 0, 0));
    assert_eq!(s(&map).outcome, RunOutcome::Cancelled);
    map.insert("job_1", status(JobState::Failed, 0, 1));
    assert_eq!(s(&map).outcome, RunOutcome::Failed);
    map.insert("job_1", status(JobState::Failed, 4, 2));
    assert_eq!(s(&map), Summary { outcome: RunOutcome::Partial, completed_files: 4, failed: 2 });
}

#[test]
fn summary_ignores_other_jobs_and_handles_mixed_states() {
    let e = entry(&["job_1", "job_2"]);
    let mut map: HashMap<&str, JobStatus> = HashMap::new();
    map.insert("job_9", status(JobState::Failed, 0, 9)); // someone else's job
    map.insert("job_1", status(JobState::Completed, 2, 0));
    assert_eq!(summarize(&e, &|id| map.get(id).copied()).outcome, RunOutcome::UnknownOutcome);
    map.insert("job_2", status(JobState::Queued, 0, 0));
    assert_eq!(summarize(&e, &|id| map.get(id).copied()).outcome, RunOutcome::Running);
    map.insert("job_2", status(JobState::Failed, 0, 1));
    let s = summarize(&e, &|id| map.get(id).copied());
    assert_eq!(s, Summary { outcome: RunOutcome::Partial, completed_files: 2, failed: 1 });
}

#[test]
fn unresolved_entry_summarizes_as_unknown_even_if_queue_has_jobs() {
    let mut e = entry(&["job_1"]);
    e.state = ExecState::UnknownOutcome;
    let s = summarize(&e, &|_| Some(status(JobState::Completed, 3, 0)));
    assert_eq!(s.outcome, RunOutcome::UnknownOutcome);
}
