//! FER-432 #2 / #6: retention floor on every read path, periodic purge on the
//! identity worker, and hidden-id hygiene in `activity-settings.json`.

use super::*;

fn id_of(path: &str) -> String {
    build_receipt(path, VisitKind::Folder).unwrap().entity.id
}

fn record_old(service: &ActivityService, path: &str, at: u64) {
    service
        .record_at_for_test(path, VisitKind::Folder, at)
        .unwrap();
}

/// 40 days ago (inside the default 90-day store cutoff) and 1 day ago.
fn seed_old_and_fresh(service: &ActivityService) -> (u64, u64) {
    let now = now_ms();
    let old = now - 40 * DAY_MS;
    let fresh = now - DAY_MS;
    record_old(service, r"C:\Work\Oldfolder", old);
    record_old(service, r"C:\Work\Freshfolder", fresh);
    (old, fresh)
}

/// Shortens retention WITHOUT purging (what a long-running session looks like
/// between purges: the setting moved, the rows are still on disk).
fn shorten_retention_without_purge(service: &ActivityService, days: u32) {
    service
        .update_settings(|s| s.retention_days = Some(days))
        .unwrap();
}

#[test]
fn recent_applies_the_retention_floor_before_any_purge() {
    let dir = tempfile::tempdir().unwrap();
    let service = ActivityService::open(dir.path()).unwrap();
    seed_old_and_fresh(&service);
    assert_eq!(
        service.recent(ActivityKind::FolderVisit, 10).unwrap().len(),
        2
    );
    shorten_retention_without_purge(&service, 30);
    assert_eq!(
        service.episode_count_for_test().unwrap(),
        2,
        "not purged yet"
    );
    let names: Vec<_> = service
        .recent(ActivityKind::FolderVisit, 10)
        .unwrap()
        .into_iter()
        .map(|i| i.entity.display_name)
        .collect();
    assert_eq!(names, vec!["Freshfolder".to_owned()]);
}

#[test]
fn search_and_chat_find_by_name_apply_the_retention_floor() {
    let dir = tempfile::tempdir().unwrap();
    let service = ActivityService::open(dir.path()).unwrap();
    seed_old_and_fresh(&service);
    let epoch = service.store.lock().unwrap().privacy_epoch().unwrap();
    let names = |found: Vec<ActivityEntity>| -> Vec<String> {
        let mut names: Vec<String> = found.into_iter().map(|e| e.display_name).collect();
        names.sort();
        names
    };
    {
        let mut reader = service.chat_reader(None);
        let found = reader
            .name_candidates("folder", ActivityKind::FolderVisit, epoch, 10)
            .unwrap()
            .unwrap();
        assert_eq!(
            names(found),
            vec!["Freshfolder", "Oldfolder"],
            "both inside retention"
        );
    }
    let search_names = |q: &str| -> Vec<String> {
        service
            .search(q, SearchScope::All, 10)
            .unwrap()
            .unwrap()
            .into_iter()
            .map(|m| m.entity.display_name)
            .collect()
    };
    assert!(search_names("oldfolder").contains(&"Oldfolder".to_owned()));
    shorten_retention_without_purge(&service, 30);
    let mut reader = service.chat_reader(None);
    let found = reader
        .name_candidates("folder", ActivityKind::FolderVisit, epoch, 10)
        .unwrap()
        .unwrap();
    assert_eq!(
        names(found),
        vec!["Freshfolder"],
        "older than retention: never offered"
    );
    assert!(
        !search_names("oldfolder").contains(&"Oldfolder".to_owned()),
        "palette search uses the same floor"
    );
}

#[test]
fn periodic_purge_runs_on_the_worker_and_prunes_hidden_ids() {
    let dir = tempfile::tempdir().unwrap();
    let service =
        ActivityService::open_with_purge_interval(dir.path(), Duration::from_millis(40)).unwrap();
    service.flush();
    seed_old_and_fresh(&service);
    service
        .set_hidden(&id_of(r"C:\Work\Oldfolder"), true)
        .unwrap();
    service
        .set_hidden(&id_of(r"C:\Work\Freshfolder"), true)
        .unwrap();
    shorten_retention_without_purge(&service, 30);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while service.episode_count_for_test().unwrap() != 1 {
        assert!(
            std::time::Instant::now() < deadline,
            "periodic purge never ran"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    // The purge removed the old entity; its hide flag must not linger.
    while service.state().unwrap().hidden_count != 1 {
        assert!(
            std::time::Instant::now() < deadline,
            "hidden ids not pruned"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        service.settings_snapshot().hidden,
        vec![id_of(r"C:\Work\Freshfolder")]
    );
    // Persisted too.
    let bytes = std::fs::read(dir.path().join(SETTINGS_FILE)).unwrap();
    let saved: Settings = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(saved.hidden, vec![id_of(r"C:\Work\Freshfolder")]);
}

#[test]
fn periodic_purge_still_runs_under_a_steady_job_stream() {
    let dir = tempfile::tempdir().unwrap();
    let service =
        ActivityService::open_with_purge_interval(dir.path(), Duration::from_millis(40)).unwrap();
    service.flush();
    seed_old_and_fresh(&service);
    shorten_retention_without_purge(&service, 30);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    // Jobs keep arriving faster than the interval: the purge must not starve.
    while service.episode_count_for_test().unwrap() > 1 {
        assert!(
            std::time::Instant::now() < deadline,
            "purge starved by jobs"
        );
        let (ack, _wait) = sync_channel(1);
        if let Some(jobs) = &service.jobs {
            let _ = jobs.try_send(Job::Flush(ack));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn worker_stops_when_the_service_is_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let service =
        ActivityService::open_with_purge_interval(dir.path(), Duration::from_millis(10)).unwrap();
    let store = Arc::clone(&service.store);
    drop(service);
    // Only the worker can still hold the store; it must let go promptly.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while Arc::strong_count(&store) > 1 {
        assert!(std::time::Instant::now() < deadline, "worker kept running");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn exclude_prunes_hidden_ids_of_removed_entities() {
    let dir = tempfile::tempdir().unwrap();
    let service = ActivityService::open(dir.path()).unwrap();
    let now = now_ms();
    record_old(&service, r"C:\Work\Inside", now - DAY_MS);
    record_old(&service, r"C:\Other\Outside", now - DAY_MS);
    service.set_hidden(&id_of(r"C:\Work\Inside"), true).unwrap();
    service
        .set_hidden(&id_of(r"C:\Other\Outside"), true)
        .unwrap();
    assert_eq!(service.state().unwrap().hidden_count, 2);
    service.exclude(r"C:\Work").unwrap();
    assert_eq!(
        service.settings_snapshot().hidden,
        vec![id_of(r"C:\Other\Outside")]
    );
}

#[test]
fn set_retention_purge_prunes_hidden_ids() {
    let dir = tempfile::tempdir().unwrap();
    let service = ActivityService::open(dir.path()).unwrap();
    seed_old_and_fresh(&service);
    service
        .set_hidden(&id_of(r"C:\Work\Oldfolder"), true)
        .unwrap();
    assert_eq!(service.set_retention(30).unwrap().hidden_count, 0);
}

#[test]
fn hidden_list_is_capped_dropping_the_oldest() {
    let dir = tempfile::tempdir().unwrap();
    let service = ActivityService::open(dir.path()).unwrap();
    let seeded: Vec<String> = (0..MAX_HIDDEN).map(|i| format!("l{i:016x}")).collect();
    service
        .update_settings(|s| s.hidden = seeded.clone())
        .unwrap();
    let state = service.set_hidden("lnewest", true).unwrap();
    assert_eq!(state.hidden_count, MAX_HIDDEN as u64);
    let hidden = service.settings_snapshot().hidden;
    assert_eq!(hidden.len(), MAX_HIDDEN);
    assert!(!hidden.contains(&seeded[0]), "oldest dropped");
    assert_eq!(hidden.last().map(String::as_str), Some("lnewest"));
    // Re-hiding an id already present never grows the list.
    service.set_hidden("lnewest", true).unwrap();
    assert_eq!(service.settings_snapshot().hidden.len(), MAX_HIDDEN);
}

#[test]
fn chat_offer_needs_service_and_acknowledged_disclosure() {
    // FER-432 #5: the backend re-gate of `activity.query` (gate_read_capabilities).
    assert!(!chat_offer_ready(None));
    let dir = tempfile::tempdir().unwrap();
    let service = ActivityService::open(dir.path()).unwrap();
    assert!(
        !chat_offer_ready(Some(&service)),
        "disclosure not acknowledged yet"
    );
    service.ack_disclosure().unwrap();
    assert!(chat_offer_ready(Some(&service)));
}

#[test]
fn stale_hidden_ids_on_disk_are_pruned_on_open() {
    let dir = tempfile::tempdir().unwrap();
    let settings = Settings {
        hidden: vec!["l00000000deadbeef".to_owned()],
        ..Settings::default()
    };
    std::fs::write(
        dir.path().join(SETTINGS_FILE),
        serde_json::to_vec(&settings).unwrap(),
    )
    .unwrap();
    let service = ActivityService::open(dir.path()).unwrap();
    assert_eq!(service.state().unwrap().hidden_count, 0);
}

#[test]
fn forget_and_clear_leave_no_stale_hidden_ids() {
    let dir = tempfile::tempdir().unwrap();
    let service = ActivityService::open(dir.path()).unwrap();
    let now = now_ms();
    record_old(&service, r"C:\Work\One", now - DAY_MS);
    record_old(&service, r"C:\Work\Two", now - DAY_MS);
    service.set_hidden(&id_of(r"C:\Work\One"), true).unwrap();
    service.set_hidden(&id_of(r"C:\Work\Two"), true).unwrap();
    service.forget(&id_of(r"C:\Work\One")).unwrap();
    assert_eq!(
        service.settings_snapshot().hidden,
        vec![id_of(r"C:\Work\Two")]
    );
    service.clear().unwrap();
    assert!(service.settings_snapshot().hidden.is_empty());
}
