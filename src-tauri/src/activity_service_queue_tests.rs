//! FER-246 / FER-248: queued visit receipts and corrupt-database recovery.

use super::*;

fn episodes(service: &ActivityService) -> u64 {
    service.store.lock().unwrap().episode_count().unwrap()
}

#[test]
fn record_visit_returns_before_the_worker_applies_it() {
    let dir = tempfile::tempdir().unwrap();
    let service = ActivityService::open(dir.path()).unwrap();
    // Holding the store lock stalls the worker; the caller must not notice.
    let guard = service.store.lock().unwrap();
    let started = std::time::Instant::now();
    assert_eq!(
        service
            .record_visit(r"C:\Work\Queued", VisitKind::Folder)
            .unwrap(),
        "queued"
    );
    // The store lock is held by this very thread, so a record_visit that waited for the worker
    // would hang forever; the bound only guards against slow-but-returning, so keep it generous.
    assert!(started.elapsed() < Duration::from_secs(10));
    drop(guard);
    service.flush();
    assert_eq!(episodes(&service), 1);
}

#[test]
fn backlog_is_applied_in_order_and_batched() {
    let dir = tempfile::tempdir().unwrap();
    let service = ActivityService::open(dir.path()).unwrap();
    let guard = service.store.lock().unwrap();
    for i in 0..150 {
        let label = service
            .record_visit(&format!(r"C:\Work\Folder{i}"), VisitKind::Folder)
            .unwrap();
        assert_eq!(label, "queued", "within capacity nothing is dropped");
    }
    drop(guard);
    service.flush();
    assert_eq!(episodes(&service), 150);
    assert_eq!(
        service
            .recent(ActivityKind::FolderVisit, 100)
            .unwrap()
            .len(),
        100
    );
}

#[test]
fn overflow_is_dropped_and_counted_never_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let service = ActivityService::open(dir.path()).unwrap();
    let guard = service.store.lock().unwrap();
    let mut queued = 0_u64;
    let mut dropped = 0_u64;
    for i in 0..(JOB_QUEUE as u64 * 3) {
        match service
            .record_visit(&format!(r"C:\Work\Burst{i}"), VisitKind::Folder)
            .expect("a full queue must not fail navigation")
            .as_str()
        {
            "queued" => queued += 1,
            "dropped" => dropped += 1,
            other => panic!("unexpected label {other}"),
        }
    }
    assert!(dropped > 0, "queue is bounded");
    assert_eq!(service.dropped_visits.load(Ordering::Relaxed), dropped);
    drop(guard);
    service.flush();
    assert_eq!(
        episodes(&service),
        queued,
        "every accepted receipt lands exactly once"
    );
}

#[test]
fn queued_receipt_is_cancelled_by_a_privacy_change() {
    let dir = tempfile::tempdir().unwrap();
    let service = ActivityService::open(dir.path()).unwrap();
    // Stale generation: the same bump that clear/forget/exclude/pause perform
    // before taking the store lock, observed while the visit is still queued.
    let guard = service.store.lock().unwrap();
    service
        .record_visit(r"C:\Work\Secret", VisitKind::Folder)
        .unwrap();
    service.bump_generation();
    drop(guard);
    service.flush();
    assert_eq!(
        episodes(&service),
        0,
        "receipt queued before the change is discarded"
    );
    // A receipt queued after the change is recorded normally.
    service
        .record_visit(r"C:\Work\Later", VisitKind::Folder)
        .unwrap();
    service.flush();
    assert_eq!(episodes(&service), 1);
}

#[test]
fn clear_and_pause_cancel_already_queued_visits() {
    let dir = tempfile::tempdir().unwrap();
    let service = ActivityService::open(dir.path()).unwrap();
    service
        .record_visit(r"C:\Work\One", VisitKind::Folder)
        .unwrap();
    service.clear().unwrap();
    service.flush();
    assert_eq!(episodes(&service), 0);
    service.set_recording(false).unwrap();
    service
        .record_visit(r"C:\Work\Two", VisitKind::Folder)
        .unwrap();
    service.flush();
    assert_eq!(episodes(&service), 0, "paused: nothing recorded");
}

#[test]
fn database_failure_does_not_fail_record_visit() {
    let dir = tempfile::tempdir().unwrap();
    let service = ActivityService::open(dir.path()).unwrap();
    // Poison the store mutex: the worker can no longer apply anything.
    let store = Arc::clone(&service.store);
    let _ = std::thread::spawn(move || {
        let _guard = store.lock().unwrap();
        panic!("poison");
    })
    .join();
    assert!(
        service
            .record_visit(r"C:\Work\Doomed", VisitKind::Folder)
            .is_ok()
    );
    service.flush();
    assert!(
        service
            .record_visit(r"C:\Work\Doomed2", VisitKind::Folder)
            .is_ok()
    );
}

#[test]
fn invalid_path_is_still_an_error_and_queues_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let service = ActivityService::open(dir.path()).unwrap();
    assert!(service.record_visit("relative", VisitKind::Folder).is_err());
    service.flush();
    assert_eq!(episodes(&service), 0);
}

#[test]
fn corrupt_database_is_recovered_and_flagged_in_state() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("activity.sqlite3"),
        b"not sqlite at all ".repeat(100),
    )
    .unwrap();
    let service = ActivityService::open(dir.path()).unwrap();
    assert!(service.state().unwrap().recovered);
    service
        .record_visit(r"C:\Work\AfterRecovery", VisitKind::Folder)
        .unwrap();
    service.flush();
    assert_eq!(episodes(&service), 1);
    let quarantined = std::fs::read_dir(dir.path())
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".corrupt-")
        })
        .count();
    assert_eq!(quarantined, 1);
    drop(service);
    let healthy = ActivityService::open(dir.path()).unwrap();
    assert!(!healthy.state().unwrap().recovered);
}

#[test]
fn newer_schema_database_is_refused_and_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("activity.sqlite3");
    drop(ferrum_activity_store::ActivityStore::open(&path).unwrap());
    {
        // PRAGMA user_version lives at byte offset 60 of the file header (big endian).
        let mut bytes = std::fs::read(&path).unwrap();
        let future = u32::try_from(ferrum_activity_store::SCHEMA_VERSION + 1).unwrap();
        bytes[60..64].copy_from_slice(&future.to_be_bytes());
        std::fs::write(&path, bytes).unwrap();
    }
    let before = std::fs::read(&path).unwrap();
    assert!(ActivityService::open(dir.path()).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
}
