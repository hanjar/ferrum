//! Audit-gap tests: FER-248 (corrupt DB recovery), FER-250 (chat provenance),
//! FER-252 (out-of-order episodes), plus the batch API behind FER-246.

use ferrum_activity_contract::{
    ActivityEntity, ActivityKind, ActivityReceipt, Availability, EntityKind, Provenance,
    ReceiptSource,
};
use ferrum_activity_store::{ActivityStore, RecordOutcome, SCHEMA_VERSION};
use rusqlite::Connection;
use std::path::Path;

const DAY: u64 = 86_400_000;

fn folder(event: &str, id: &str, epoch: u64, at: u64) -> ActivityReceipt {
    ActivityReceipt {
        version: 1,
        event_id: event.into(),
        action_id: format!("act-{event}"),
        privacy_epoch: epoch,
        source: ReceiptSource::NavigationCommit,
        kind: ActivityKind::FolderVisit,
        provenance: Provenance::Manual,
        entity: ActivityEntity {
            id: id.into(),
            kind: EntityKind::Folder,
            provider: "local".into(),
            volume_id: "volume-one".into(),
            locator: format!("C:\\Work\\{id}"),
            parent_path: "C:\\Work".into(),
            display_name: id.into(),
            availability: Availability::Available,
        },
        occurred_utc_ms: at,
    }
}

fn frequent_count(store: &mut ActivityStore, until: u64) -> usize {
    let epoch = store.privacy_epoch().unwrap();
    store
        .frequent(0, until, ActivityKind::FolderVisit, epoch, 100)
        .unwrap()
        .map_or(0, |rows| rows.len())
}

fn recent_count(store: &mut ActivityStore, until: u64) -> usize {
    let epoch = store.privacy_epoch().unwrap();
    store
        .recent(0, until, ActivityKind::FolderVisit, epoch, 100)
        .unwrap()
        .map_or(0, |rows| rows.len())
}

fn quarantined(dir: &Path) -> Vec<std::path::PathBuf> {
    std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.to_string_lossy().contains(".corrupt-"))
        .collect()
}

// ---- FER-250 ---------------------------------------------------------------

#[test]
fn chat_provenance_never_creates_entity_or_episode() {
    let mut store = ActivityStore::in_memory().unwrap();
    let mut chat = folder("c1", "chatty", 0, 1_000);
    chat.provenance = Provenance::Chat;
    assert_eq!(store.record(&chat).unwrap(), RecordOutcome::Ignored);
    assert_eq!(store.episode_count().unwrap(), 0);
    assert_eq!(recent_count(&mut store, DAY), 0);
    assert_eq!(frequent_count(&mut store, DAY), 0);
    // The same receipt id is not burned: the manual one is still recorded.
    let manual = folder("c1", "chatty", 0, 1_000);
    assert_eq!(store.record(&manual).unwrap(), RecordOutcome::NewEpisode);
    // Chat on top of an existing episode does not extend it or add a count.
    let mut again = folder("c2", "chatty", 0, 5_000);
    again.provenance = Provenance::Chat;
    assert_eq!(store.record(&again).unwrap(), RecordOutcome::Ignored);
    assert_eq!(store.episode_count().unwrap(), 1);
}

#[test]
fn watcher_never_touches_activity() {
    let watch = include_str!("../../../watch.rs");
    // The word "activity" appears in a doc comment; check real call surfaces.
    for needle in [
        "activity_service",
        "ActivityService",
        "ActivityHandle",
        "record_visit",
        "note_fs_changes",
        "ferrum_activity",
    ] {
        assert!(
            !watch.contains(needle),
            "watch.rs must not reference {needle}"
        );
    }
}

// ---- FER-252 ---------------------------------------------------------------

#[test]
fn out_of_order_receipts_keep_one_episode_and_consistent_rollups() {
    let mut store = ActivityStore::in_memory().unwrap();
    let base = 3 * DAY + 100_000;
    // Frequent needs two active days: one earlier episode on day 1.
    assert_eq!(
        store.record(&folder("z", "x", 0, DAY + 1_000)).unwrap(),
        RecordOutcome::NewEpisode
    );
    assert_eq!(
        store.record(&folder("a", "x", 0, base + 20_000)).unwrap(),
        RecordOutcome::NewEpisode
    );
    assert_eq!(
        store.record(&folder("b", "x", 0, base + 5_000)).unwrap(),
        RecordOutcome::ContinuedEpisode
    );
    // Older than the start but inside the window: still the same episode.
    assert_eq!(
        store.record(&folder("c", "x", 0, base - 5_000)).unwrap(),
        RecordOutcome::ContinuedEpisode
    );
    // Outside the window relative to the (now extended) start: new episode.
    assert_eq!(
        store.record(&folder("d", "x", 0, base - 40_000)).unwrap(),
        RecordOutcome::NewEpisode
    );
    assert_eq!(store.episode_count().unwrap(), 3);
    let epoch = store.privacy_epoch().unwrap();
    let rows = store
        .recent(0, base + DAY, ActivityKind::FolderVisit, epoch, 10)
        .unwrap()
        .unwrap();
    assert_eq!(rows.len(), 1);
    let freq = store
        .frequent(0, base + DAY, ActivityKind::FolderVisit, epoch, 10)
        .unwrap()
        .unwrap();
    assert_eq!(freq.len(), 1);
    assert_eq!(freq[0].episode_count, 3);
    assert_eq!(freq[0].active_days, 2);
    assert_eq!(frequent_count(&mut store, base + DAY), 1);
}

#[test]
fn out_of_order_across_midnight_keeps_the_bucket_on_the_newest_day() {
    let mut store = ActivityStore::in_memory().unwrap();
    let midnight = 5 * DAY;
    assert_eq!(
        store
            .record(&folder("m0", "y", 0, 2 * DAY + 1_000))
            .unwrap(),
        RecordOutcome::NewEpisode
    );
    assert_eq!(
        store
            .record(&folder("m1", "y", 0, midnight + 5_000))
            .unwrap(),
        RecordOutcome::NewEpisode
    );
    assert_eq!(
        store
            .record(&folder("m2", "y", 0, midnight - 5_000))
            .unwrap(),
        RecordOutcome::ContinuedEpisode
    );
    let epoch = store.privacy_epoch().unwrap();
    let all = store
        .frequent(0, midnight + DAY, ActivityKind::FolderVisit, epoch, 10)
        .unwrap()
        .unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].episode_count, 2);
    assert_eq!(all[0].active_days, 2, "no phantom bucket on the older day");
    assert_eq!(all[0].last_utc_ms, midnight + 5_000);
}

// ---- FER-248 ---------------------------------------------------------------

#[test]
fn corrupt_file_is_quarantined_and_a_fresh_database_works() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("activity.sqlite3");
    std::fs::write(
        &path,
        b"this is definitely not a sqlite database, just junk bytes ".repeat(40),
    )
    .unwrap();
    let (mut store, recovered) = ActivityStore::open_recovering(&path).unwrap();
    assert!(recovered);
    assert_eq!(
        store.record(&folder("r1", "fresh", 0, 1_000)).unwrap(),
        RecordOutcome::NewEpisode
    );
    drop(store);
    let moved = quarantined(dir.path());
    assert_eq!(moved.len(), 1, "exactly one quarantine file");
    assert!(
        std::fs::read(&moved[0])
            .unwrap()
            .starts_with(b"this is definitely")
    );
    // Reopening the healthy database is not a recovery.
    let (mut again, recovered) = ActivityStore::open_recovering(&path).unwrap();
    assert!(!recovered);
    assert_eq!(recent_count(&mut again, DAY), 1);
}

#[test]
fn two_corruptions_never_overwrite_an_earlier_quarantine() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("activity.sqlite3");
    for round in 0..2 {
        std::fs::write(&path, format!("garbage-{round}").repeat(500)).unwrap();
        let (store, recovered) = ActivityStore::open_recovering(&path).unwrap();
        assert!(recovered);
        drop(store);
        let _ = std::fs::remove_file(&path);
    }
    assert_eq!(quarantined(dir.path()).len(), 2);
}

#[test]
fn future_schema_is_rejected_and_the_file_is_left_intact() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("activity.sqlite3");
    drop(ActivityStore::open(&path).unwrap());
    {
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .unwrap();
    }
    let before = std::fs::read(&path).unwrap();
    assert!(ActivityStore::open_recovering(&path).is_err());
    assert!(ActivityStore::open(&path).is_err());
    assert_eq!(
        std::fs::read(&path).unwrap(),
        before,
        "future-schema file untouched"
    );
    assert!(quarantined(dir.path()).is_empty());
}

// ---- FER-246 (store side) --------------------------------------------------

#[test]
fn batch_matches_sequential_semantics_and_checks_epoch_per_receipt() {
    let mut store = ActivityStore::in_memory().unwrap();
    let mut chat = folder("b4", "r", 0, 4_000);
    chat.provenance = Provenance::Chat;
    let batch = vec![
        folder("b1", "p", 0, 1_000),
        folder("b1", "p", 0, 1_100), // duplicate event id
        folder("b2", "p", 0, 2_000), // continues
        folder("b3", "q", 9, 3_000), // stale epoch
        chat,
        folder("b5", "p", 0, 1_500), // out of order, continues
    ];
    let out: Vec<_> = store
        .record_batch(&batch)
        .unwrap()
        .into_iter()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(
        out,
        vec![
            RecordOutcome::NewEpisode,
            RecordOutcome::Duplicate,
            RecordOutcome::ContinuedEpisode,
            RecordOutcome::StaleEpoch,
            RecordOutcome::Ignored,
            RecordOutcome::ContinuedEpisode,
        ]
    );
    assert_eq!(store.episode_count().unwrap(), 1);
}

#[test]
fn batch_failure_of_one_receipt_does_not_lose_the_others() {
    let mut store = ActivityStore::in_memory().unwrap();
    let mut bad = folder("bad", "z", 0, 1_000);
    bad.entity.locator = "relative".into(); // fails contract validation
    let out = store
        .record_batch(&[
            folder("g1", "ok1", 0, 1_000),
            bad,
            folder("g2", "ok2", 0, 1_000),
        ])
        .unwrap();
    assert!(out[0].is_ok() && out[1].is_err() && out[2].is_ok());
    assert_eq!(store.episode_count().unwrap(), 2);
}

/// Measurement for decisions.md (FER-246): latency of one synchronous record()
/// on a file database. Run with `-- --ignored --nocapture`.
#[test]
#[ignore]
fn measure_record_latency() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = ActivityStore::open(&dir.path().join("a.sqlite3")).unwrap();
    let mut samples = Vec::new();
    for i in 0..300_u64 {
        let r = folder(
            &format!("l{i}"),
            &format!("e{}", i % 50),
            0,
            1_000_000 + i * 60_000,
        );
        let t = std::time::Instant::now();
        store.record(&r).unwrap();
        samples.push(t.elapsed().as_micros());
    }
    samples.sort_unstable();
    println!(
        "single record(): p50={}us p95={}us max={}us",
        samples[150], samples[285], samples[299]
    );
    let batch: Vec<_> = (0..64_u64)
        .map(|i| {
            folder(
                &format!("m{i}"),
                &format!("e{}", i % 50),
                0,
                900_000_000 + i * 60_000,
            )
        })
        .collect();
    let t = std::time::Instant::now();
    store.record_batch(&batch).unwrap();
    println!("record_batch(64): {}us", t.elapsed().as_micros());
}
