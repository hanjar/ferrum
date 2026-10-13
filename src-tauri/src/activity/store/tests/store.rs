use ferrum_activity_contract::{
    ActivityEntity, ActivityKind, ActivityReceipt, Availability, EntityKind, Provenance,
    ReceiptSource,
};
use ferrum_activity_store::{ActivityStore, RecentCursor, RecordOutcome, SCHEMA_VERSION};
use rusqlite::Connection;

fn receipt(
    event: &str,
    action: &str,
    entity_id: &str,
    path: &str,
    epoch: u64,
    timestamp: u64,
) -> ActivityReceipt {
    ActivityReceipt {
        version: 1,
        event_id: event.into(),
        action_id: action.into(),
        privacy_epoch: epoch,
        source: ReceiptSource::NavigationCommit,
        kind: ActivityKind::FolderVisit,
        provenance: Provenance::Manual,
        entity: ActivityEntity {
            id: entity_id.into(),
            kind: EntityKind::Folder,
            provider: "local".into(),
            volume_id: "volume-one".into(),
            locator: path.into(),
            parent_path: "C:\\Work".into(),
            display_name: "Reports".into(),
            availability: Availability::Available,
        },
        occurred_utc_ms: timestamp,
    }
}

fn named_receipt(id: &str, name: &str, timestamp: u64, epoch: u64) -> ActivityReceipt {
    let mut value = receipt(id, id, id, &format!("C:\\Work\\{id}"), epoch, timestamp);
    value.entity.display_name = name.into();
    value
}

fn candidates(store: &mut ActivityStore, query: &str, epoch: u64) -> Vec<String> {
    store
        .search_candidates(query, ActivityKind::FolderVisit, epoch, 100)
        .unwrap()
        .unwrap()
        .into_iter()
        .map(|entity| entity.id)
        .collect()
}

#[test]
fn candidate_lookup_finds_rare_old_name_and_typo_without_recent_or_frequent_gate() {
    let mut store = ActivityStore::in_memory().unwrap();
    store
        .record(&named_receipt("rare", "Quarry Notes", 10, 0))
        .unwrap();
    for index in 0..150 {
        store
            .record(&named_receipt(
                &format!("new-{index:03}"),
                "Invoices",
                100_000 + index,
                0,
            ))
            .unwrap();
    }
    assert_eq!(candidates(&mut store, "quarry", 0), vec!["rare"]);
    assert_eq!(candidates(&mut store, "quary", 0), vec!["rare"]);
    assert!(
        store
            .recent(100_000, 200_000, ActivityKind::FolderVisit, 0, 100)
            .unwrap()
            .unwrap()
            .iter()
            .all(|row| row.entity.id != "rare")
    );
    assert!(
        store
            .frequent(0, 200_000, ActivityKind::FolderVisit, 0, 100)
            .unwrap()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn candidate_index_follows_rename_privacy_and_retention() {
    let mut store = ActivityStore::in_memory().unwrap();
    store
        .record(&named_receipt("private", "Secret Plans", 100, 0))
        .unwrap();
    store
        .record(&named_receipt("forgot", "Secret Draft", 100, 0))
        .unwrap();
    store
        .record(&named_receipt("old", "Secret Archive", 100, 0))
        .unwrap();
    store
        .record(&named_receipt("safe", "Secret Public", 200, 0))
        .unwrap();
    let mut renamed = named_receipt("safe", "Public Notes", 201, 0);
    renamed.event_id = "safe-renamed".into();
    renamed.action_id = "safe-renamed".into();
    store.record(&renamed).unwrap();
    assert!(!candidates(&mut store, "secret", 0).contains(&"safe".to_string()));
    assert_eq!(candidates(&mut store, "public", 0), vec!["safe"]);
    store.forget_entity("forgot").unwrap();
    assert!(
        store
            .search_candidates("secret", ActivityKind::FolderVisit, 0, 10)
            .unwrap()
            .is_none()
    );
    assert!(!candidates(&mut store, "secret", 1).contains(&"forgot".to_string()));
    store.exclude_root("C:\\Work\\private").unwrap();
    assert!(!candidates(&mut store, "secret", 2).contains(&"private".to_string()));
    assert_eq!(store.purge_before(150).unwrap(), 1);
    assert!(candidates(&mut store, "secret", 2).is_empty());
    store.clear_all().unwrap();
    assert!(candidates(&mut store, "public", 3).is_empty());
}

#[test]
fn candidate_query_has_stable_bounds_kind_and_unicode_grams() {
    let mut store = ActivityStore::in_memory().unwrap();
    for index in 0..140 {
        store
            .record(&named_receipt(
                &format!("item-{index:03}"),
                "Archive",
                100,
                0,
            ))
            .unwrap();
    }
    let ids = candidates(&mut store, "archve", 0);
    assert_eq!(ids.len(), 100);
    assert_eq!(ids.first().unwrap(), "item-000");
    assert_eq!(ids.last().unwrap(), "item-099");
    assert_eq!(candidates(&mut store, "archve", 0), ids);
    assert_eq!(candidates(&mut store, "archive", 0), ids);
    store
        .record(&named_receipt("unicode", "Übergröße", 100, 0))
        .unwrap();
    assert_eq!(candidates(&mut store, "ÜBERGRÖSE", 0), vec!["unicode"]);
    let mut file = named_receipt("file", "Übergröße", 100, 0);
    file.kind = ActivityKind::FileOpenRequested;
    file.source = ReceiptSource::OpenAccepted;
    file.entity.kind = EntityKind::File;
    store.record(&file).unwrap();
    let files = store
        .search_candidates("übergröße", ActivityKind::FileOpenRequested, 0, 10)
        .unwrap()
        .unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].id, "file");
    assert!(
        store
            .search_candidates("archive", ActivityKind::FilePreview, 0, 10)
            .is_err()
    );
    assert!(
        store
            .search_candidates("archive", ActivityKind::FolderVisit, 0, 0)
            .is_err()
    );
    assert!(
        store
            .search_candidates("archive", ActivityKind::FolderVisit, 0, 101)
            .is_err()
    );
    assert!(
        store
            .search_candidates(" ", ActivityKind::FolderVisit, 0, 10)
            .is_err()
    );
}

#[test]
fn locator_candidates_filter_kind_epoch_and_escape_wildcards() {
    let mut store = ActivityStore::in_memory().unwrap();
    store
        .record(&named_receipt("inv", "Invoices", 100, 0))
        .unwrap();
    let mut file = named_receipt("file", "Invoices", 100, 0);
    file.kind = ActivityKind::FileOpenRequested;
    file.source = ReceiptSource::OpenAccepted;
    file.entity.kind = EntityKind::File;
    file.entity.locator = "C:\\Work\\file-invoices.txt".into();
    store.record(&file).unwrap();
    let mut ids = |needle: &str, kind: ActivityKind| -> Vec<String> {
        store
            .locator_candidates(needle, kind, 0, 10)
            .unwrap()
            .unwrap()
            .into_iter()
            .map(|entity| entity.id)
            .collect()
    };
    assert_eq!(ids("work", ActivityKind::FolderVisit), vec!["inv"]);
    assert_eq!(ids(r"c:\work", ActivityKind::FolderVisit), vec!["inv"]);
    assert_eq!(ids("work", ActivityKind::FileOpenRequested), vec!["file"]);
    // Wildcards in the needle are literals, not SQL patterns.
    assert!(ids("w!_rk", ActivityKind::FolderVisit).is_empty());
    assert!(
        store
            .locator_candidates("work", ActivityKind::FolderVisit, 9, 10)
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .locator_candidates("", ActivityKind::FolderVisit, 0, 10)
            .is_err()
    );
}

#[test]
fn exact_name_outside_bounded_postings_precedes_other_candidates() {
    let mut store = ActivityStore::in_memory().unwrap();
    for index in 0..120 {
        store
            .record(&named_receipt(
                &format!("item-{index:03}"),
                "Archives",
                100,
                0,
            ))
            .unwrap();
    }
    store
        .record(&named_receipt("z-exact", "Archive", 100, 0))
        .unwrap();
    let ids = candidates(&mut store, "archive", 0);
    assert_eq!(ids.len(), 100);
    assert_eq!(ids.first().unwrap(), "z-exact");
}

#[test]
fn candidate_posting_limit_applies_after_kind_filter() {
    let mut store = ActivityStore::in_memory().unwrap();
    for index in 0..120 {
        store
            .record(&named_receipt(
                &format!("folder-{index:03}"),
                "Archive",
                100,
                0,
            ))
            .unwrap();
    }
    let mut file = named_receipt("file", "Archive", 100, 0);
    file.kind = ActivityKind::FileOpenRequested;
    file.source = ReceiptSource::OpenAccepted;
    file.entity.kind = EntityKind::File;
    store.record(&file).unwrap();
    let files = store
        .search_candidates("archive", ActivityKind::FileOpenRequested, 0, 10)
        .unwrap()
        .unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].id, "file");
}

#[test]
fn favorites_are_manual_stable_and_survive_history_clears() {
    let mut store = ActivityStore::in_memory().unwrap();
    store
        .record(&named_receipt("visited", "Reports", 100, 0))
        .unwrap();
    assert!(
        store
            .add_favorite(r"C:\Work\Reports", "Reports", "folder")
            .unwrap()
    );
    assert!(
        !store
            .add_favorite(r"C:\Work\Reports", "Reports", "folder")
            .unwrap()
    );
    assert!(
        store
            .add_favorite(r"C:\Backup", "Backup", "folder")
            .unwrap()
    );
    assert!(
        store
            .add_favorite(r"C:\book.xlsx", "book.xlsx", "file")
            .unwrap()
    );
    assert_eq!(
        store.favorites().unwrap(),
        vec![
            (
                r"C:\Work\Reports".to_string(),
                "Reports".to_string(),
                "folder".to_string()
            ),
            (
                r"C:\Backup".to_string(),
                "Backup".to_string(),
                "folder".to_string()
            ),
            (
                r"C:\book.xlsx".to_string(),
                "book.xlsx".to_string(),
                "file".to_string()
            ),
        ]
    );
    // History privacy ops must never remove manual pins (spec §7).
    store.forget_entity("visited").unwrap();
    store.purge_before(200_000).unwrap();
    store.exclude_root(r"C:\Work").unwrap();
    let _ = store.clear_all().unwrap();
    assert_eq!(store.episode_count().unwrap(), 0);
    assert_eq!(store.favorites().unwrap().len(), 3);
    assert!(store.remove_favorite(r"C:\Work\Reports").unwrap());
    assert!(!store.remove_favorite(r"C:\Work\Reports").unwrap());
    assert_eq!(store.favorites().unwrap().len(), 2);
}

#[test]
fn dedupes_retries_and_continues_episode_across_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("activity.sqlite3");
    let first = receipt(
        "event-1",
        "action-1",
        "entity-1",
        "C:\\Work\\Reports",
        0,
        100_000,
    );
    let second = receipt(
        "event-2",
        "action-2",
        "entity-1",
        "C:\\Work\\Reports",
        0,
        110_000,
    );
    {
        let mut store = ActivityStore::open(&path).unwrap();
        assert_eq!(store.record(&first).unwrap(), RecordOutcome::NewEpisode);
        assert_eq!(store.record(&first).unwrap(), RecordOutcome::Duplicate);
        assert_eq!(
            store.record(&second).unwrap(),
            RecordOutcome::ContinuedEpisode
        );
        assert_eq!(store.episode_count().unwrap(), 1);
    }
    let mut store = ActivityStore::open(&path).unwrap();
    assert_eq!(store.record(&second).unwrap(), RecordOutcome::Duplicate);
    assert_eq!(
        store
            .record(&receipt(
                "event-3",
                "action-3",
                "entity-1",
                "C:\\Work\\Reports",
                0,
                141_000
            ))
            .unwrap(),
        RecordOutcome::NewEpisode
    );
    assert_eq!(store.episode_count().unwrap(), 2);
    let connection = Connection::open(path).unwrap();
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
}

#[test]
fn clear_invalidates_pending_receipts_and_purges_derived_data() {
    let mut store = ActivityStore::in_memory().unwrap();
    let old = receipt(
        "old",
        "old-action",
        "entity-1",
        "C:\\Work\\Reports",
        0,
        100_000,
    );
    assert_eq!(store.record(&old).unwrap(), RecordOutcome::NewEpisode);
    assert_eq!(store.clear_all().unwrap(), 1);
    assert_eq!(store.episode_count().unwrap(), 0);
    assert_eq!(store.record(&old).unwrap(), RecordOutcome::StaleEpoch);
    assert_eq!(
        store
            .record(&receipt(
                "new",
                "new-action",
                "entity-1",
                "C:\\Work\\Reports",
                1,
                110_000
            ))
            .unwrap(),
        RecordOutcome::NewEpisode
    );
}

#[test]
fn exclusion_is_boundary_aware_and_removes_existing_history() {
    let mut store = ActivityStore::in_memory().unwrap();
    let inside = receipt(
        "inside",
        "inside-action",
        "inside",
        "C:\\Work\\Private\\a",
        0,
        100_000,
    );
    let sibling = receipt(
        "sibling",
        "sibling-action",
        "sibling",
        "C:\\Work\\Private2",
        0,
        100_000,
    );
    store.record(&inside).unwrap();
    store.record(&sibling).unwrap();
    assert_eq!(store.exclude_root("C:\\Work\\Private").unwrap(), 1);
    assert_eq!(store.episode_count().unwrap(), 1);
    assert_eq!(
        store
            .record(&receipt(
                "blocked",
                "blocked-action",
                "inside",
                "C:\\Work\\Private\\a",
                0,
                200_000
            ))
            .unwrap(),
        RecordOutcome::StaleEpoch
    );
    assert_eq!(
        store
            .record(&receipt(
                "new-blocked",
                "new-blocked-action",
                "inside",
                "C:\\Work\\Private\\a",
                1,
                200_000
            ))
            .unwrap(),
        RecordOutcome::Ignored
    );
    assert_eq!(store.episode_count().unwrap(), 1);
}

#[test]
fn exclusion_with_trailing_separator_purges_root_and_invalidates_pages() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("activity.sqlite3");
    let mut store = ActivityStore::open(&path).unwrap();
    for (id, locator) in [
        ("root", "C:\\Work\\Private"),
        ("child", "C:\\Work\\Private\\Child"),
        ("sibling", "C:\\Work\\Private2"),
    ] {
        store
            .record(&receipt(id, id, id, locator, 0, 100_000))
            .unwrap();
    }
    let page = store
        .recent_page(0, 200_000, ActivityKind::FolderVisit, 0, 1, None)
        .unwrap()
        .unwrap();
    let cursor = page.next_cursor.unwrap();
    assert_eq!(store.exclude_root("c:/work/private/").unwrap(), 1);
    assert_eq!(store.episode_count().unwrap(), 1);
    assert!(
        store
            .recent_page(0, 200_000, ActivityKind::FolderVisit, 0, 1, Some(&cursor))
            .unwrap()
            .is_none()
    );
    let rows = store
        .recent(0, 200_000, ActivityKind::FolderVisit, 1, 10)
        .unwrap()
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].entity.id, "sibling");
    assert_eq!(
        store
            .record(&receipt(
                "late",
                "late",
                "root",
                "C:\\Work\\Private",
                1,
                200_000
            ))
            .unwrap(),
        RecordOutcome::Ignored
    );
    drop(store);
    let connection = Connection::open(path).unwrap();
    for table in [
        "activity_entities",
        "activity_episodes",
        "activity_receipts",
        "activity_vocabulary",
    ] {
        let count: i64 = connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 1, "{table} retained excluded history");
    }
}

#[test]
fn forget_entity_invalidates_queued_writes() {
    let mut store = ActivityStore::in_memory().unwrap();
    store
        .record(&receipt(
            "first",
            "first-action",
            "forgotten",
            "C:\\Work\\Old",
            0,
            100_000,
        ))
        .unwrap();
    store.forget_entity("forgotten").unwrap();
    assert_eq!(store.privacy_epoch().unwrap(), 1);
    assert_eq!(store.episode_count().unwrap(), 0);
    assert_eq!(
        store
            .record(&receipt(
                "queued",
                "queued-action",
                "forgotten",
                "C:\\Work\\Old",
                0,
                110_000
            ))
            .unwrap(),
        RecordOutcome::StaleEpoch
    );
}

#[test]
fn retention_removes_old_receipts_and_vocabulary_with_entities() {
    let mut store = ActivityStore::in_memory().unwrap();
    store
        .record(&receipt(
            "old",
            "old-action",
            "old-item",
            "C:\\Work\\Old",
            0,
            100_000,
        ))
        .unwrap();
    store
        .record(&receipt(
            "recent",
            "recent-action",
            "recent-item",
            "C:\\Work\\Recent",
            0,
            200_000,
        ))
        .unwrap();
    assert_eq!(store.purge_before(150_000).unwrap(), 1);
    assert_eq!(store.episode_count().unwrap(), 1);
    assert_eq!(
        store
            .record(&receipt(
                "old",
                "old-action",
                "old-item",
                "C:\\Work\\Old",
                0,
                100_000
            ))
            .unwrap(),
        RecordOutcome::Ignored
    );
    assert_eq!(
        store
            .record(&receipt(
                "fresh",
                "fresh-action",
                "old-item",
                "C:\\Work\\Old",
                0,
                210_000
            ))
            .unwrap(),
        RecordOutcome::NewEpisode
    );
}

#[test]
fn exclusion_rejects_traversal_and_handles_windows_casing() {
    let mut store = ActivityStore::in_memory().unwrap();
    store
        .record(&receipt(
            "first",
            "first-action",
            "private",
            "C:\\Work\\Private\\a",
            0,
            100_000,
        ))
        .unwrap();
    assert!(store.exclude_root("C:\\Work\\Private\\..\\Public").is_err());
    assert_eq!(store.privacy_epoch().unwrap(), 0);
    assert_eq!(store.episode_count().unwrap(), 1);
    assert_eq!(store.exclude_root("c:/work/private").unwrap(), 1);
    assert_eq!(store.episode_count().unwrap(), 0);
    assert_eq!(
        store
            .record(&receipt(
                "next",
                "next-action",
                "private",
                "C:\\WORK\\PRIVATE\\b",
                1,
                200_000,
            ))
            .unwrap(),
        RecordOutcome::Ignored
    );
}

#[test]
fn migrates_version_four_and_backfills_search_grams() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("activity.sqlite3");
    {
        let mut store = ActivityStore::open(&path).unwrap();
        store
            .record(&named_receipt("older", "Quarry Notes", 100, 0))
            .unwrap();
    }
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch("DROP TABLE activity_search_grams; PRAGMA user_version = 4;")
        .unwrap();
    drop(connection);
    let mut store = ActivityStore::open(&path).unwrap();
    assert_eq!(candidates(&mut store, "quary", 0), vec!["older"]);
    assert_eq!(store.episode_count().unwrap(), 1);
    drop(store);
    let connection = Connection::open(path).unwrap();
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
}

#[test]
fn migrates_version_five_and_indexes_exact_names() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("activity.sqlite3");
    {
        let mut store = ActivityStore::open(&path).unwrap();
        store
            .record(&named_receipt("older", "Quarry Notes", 100, 0))
            .unwrap();
    }
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch("DROP INDEX activity_vocabulary_name; PRAGMA user_version = 5;")
        .unwrap();
    drop(connection);
    let mut store = ActivityStore::open(&path).unwrap();
    assert_eq!(candidates(&mut store, "quarry notes", 0), vec!["older"]);
    assert_eq!(store.episode_count().unwrap(), 1);
    drop(store);
    let connection = Connection::open(path).unwrap();
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    let index_exists: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'index' AND name = 'activity_vocabulary_name')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(index_exists);
}

#[test]
fn migrates_version_one_without_losing_history() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("activity.sqlite3");
    {
        let mut store = ActivityStore::open(&path).unwrap();
        store
            .record(&receipt(
                "first",
                "first-action",
                "stable",
                "C:\\Work\\Reports",
                0,
                100_000,
            ))
            .unwrap();
    }
    let connection = Connection::open(&path).unwrap();
    connection.execute_batch("DROP TABLE activity_search_grams; DROP INDEX activity_recent_kind_time; ALTER TABLE activity_state DROP COLUMN data_revision; ALTER TABLE activity_state DROP COLUMN retention_cutoff_utc_ms; PRAGMA user_version = 1;").unwrap();
    drop(connection);
    let mut store = ActivityStore::open(&path).unwrap();
    assert_eq!(store.episode_count().unwrap(), 1);
    assert_eq!(store.purge_before(150_000).unwrap(), 1);
    let connection = Connection::open(path).unwrap();
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
}

#[test]
fn migrates_version_two_and_keeps_recent_query_index() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("activity.sqlite3");
    {
        let mut store = ActivityStore::open(&path).unwrap();
        store
            .record(&receipt(
                "first",
                "first-action",
                "stable",
                "C:\\Work\\Reports",
                0,
                100_000,
            ))
            .unwrap();
    }
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch("DROP TABLE activity_search_grams; DROP INDEX activity_recent_kind_time; ALTER TABLE activity_state DROP COLUMN data_revision; PRAGMA user_version = 2;")
        .unwrap();
    drop(connection);
    let mut store = ActivityStore::open(&path).unwrap();
    let rows = store
        .recent(0, 200_000, ActivityKind::FolderVisit, 0, 100)
        .unwrap()
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].entity.id, "stable");
    let connection = Connection::open(path).unwrap();
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    let index: String = connection
        .query_row(
            "SELECT name FROM sqlite_master WHERE type = 'index' AND name = 'activity_recent_kind_time'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(index, "activity_recent_kind_time");
}

#[test]
fn migrates_version_three_and_preserves_history() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("activity.sqlite3");
    {
        let mut store = ActivityStore::open(&path).unwrap();
        store
            .record(&receipt(
                "first",
                "first",
                "stable",
                "C:\\Work\\Reports",
                0,
                100_000,
            ))
            .unwrap();
    }
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "DROP TABLE activity_search_grams; ALTER TABLE activity_state DROP COLUMN data_revision; PRAGMA user_version = 3;",
        )
        .unwrap();
    drop(connection);
    let mut store = ActivityStore::open(&path).unwrap();
    assert_eq!(store.episode_count().unwrap(), 1);
    assert_eq!(
        store
            .recent_page(0, 200_000, ActivityKind::FolderVisit, 0, 10, None)
            .unwrap()
            .unwrap()
            .rows[0]
            .entity
            .id,
        "stable"
    );
    let connection = Connection::open(path).unwrap();
    let revision: u64 = connection
        .query_row(
            "SELECT data_revision FROM activity_state WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(revision, 0);
}

#[test]
fn restart_preserves_privacy_epoch_after_clear() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("activity.sqlite3");
    let old = receipt(
        "old",
        "old-action",
        "private",
        "C:\\Work\\Private",
        0,
        100_000,
    );
    {
        let mut store = ActivityStore::open(&path).unwrap();
        store.record(&old).unwrap();
        assert_eq!(store.clear_all().unwrap(), 1);
    }
    let mut store = ActivityStore::open(&path).unwrap();
    assert_eq!(store.privacy_epoch().unwrap(), 1);
    assert_eq!(store.record(&old).unwrap(), RecordOutcome::StaleEpoch);
    assert_eq!(store.episode_count().unwrap(), 0);
}

#[test]
fn retention_blocks_delayed_receipts_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("activity.sqlite3");
    {
        let mut store = ActivityStore::open(&path).unwrap();
        store.purge_before(150_000).unwrap();
    }
    let mut store = ActivityStore::open(&path).unwrap();
    assert_eq!(
        store
            .record(&receipt(
                "late",
                "late-action",
                "private",
                "C:\\Work\\Private",
                0,
                100_000
            ))
            .unwrap(),
        RecordOutcome::Ignored
    );
    assert_eq!(store.episode_count().unwrap(), 0);
}

#[test]
fn clear_purges_sqlite_rows_and_rejects_corrupt_or_future_schema() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("activity.sqlite3");
    {
        let mut store = ActivityStore::open(&path).unwrap();
        store
            .record(&receipt(
                "first",
                "first-action",
                "private",
                "C:\\Work\\Private",
                0,
                100_000,
            ))
            .unwrap();
        store.exclude_root("C:\\Work\\Excluded").unwrap();
        store.clear_all().unwrap();
    }
    let connection = Connection::open(&path).unwrap();
    for table in [
        "activity_entities",
        "activity_episodes",
        "activity_receipts",
        "activity_vocabulary",
        "activity_hidden",
        "activity_search_grams",
    ] {
        let count: i64 = connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0, "{table} retained private data");
    }
    let exclusions: i64 = connection
        .query_row("SELECT COUNT(*) FROM activity_exclusions", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(exclusions, 1);
    connection
        .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
        .unwrap();
    drop(connection);
    assert!(ActivityStore::open(&path).is_err());

    let broken = dir.path().join("corrupt.sqlite3");
    std::fs::write(&broken, b"not a SQLite database").unwrap();
    assert!(ActivityStore::open(&broken).is_err());
    assert_eq!(std::fs::read(&broken).unwrap(), b"not a SQLite database");
}

#[test]
fn failed_receipt_insert_rolls_back_entity_and_episode() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("activity.sqlite3");
    let mut store = ActivityStore::open(&path).unwrap();
    let connection = Connection::open(&path).unwrap();
    connection.execute_batch("CREATE TRIGGER reject_receipt BEFORE INSERT ON activity_receipts BEGIN SELECT RAISE(ABORT, 'test failure'); END;").unwrap();
    let first = receipt(
        "first",
        "first-action",
        "stable",
        "C:\\Work\\Reports",
        0,
        100_000,
    );
    assert!(store.record(&first).is_err());
    assert_eq!(store.episode_count().unwrap(), 0);
    let entities: i64 = connection
        .query_row("SELECT COUNT(*) FROM activity_entities", [], |row| {
            row.get(0)
        })
        .unwrap();
    let vocabulary: i64 = connection
        .query_row("SELECT COUNT(*) FROM activity_vocabulary", [], |row| {
            row.get(0)
        })
        .unwrap();
    let grams: i64 = connection
        .query_row("SELECT COUNT(*) FROM activity_search_grams", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!((entities, vocabulary, grams), (0, 0, 0));
    connection
        .execute_batch("DROP TRIGGER reject_receipt")
        .unwrap();
    assert_eq!(store.record(&first).unwrap(), RecordOutcome::NewEpisode);
}

#[test]
fn recent_reads_eligible_episodes_in_range_without_recording_query() {
    let mut store = ActivityStore::in_memory().unwrap();
    for (event, entity, path, at) in [
        ("one", "z", "C:\\Work\\Z", 100_000),
        ("two", "z", "C:\\Work\\Z", 110_000),
        ("three", "a", "C:\\Work\\A", 110_000),
        ("four", "z", "C:\\Work\\Z", 150_000),
    ] {
        store
            .record(&receipt(event, event, entity, path, 0, at))
            .unwrap();
    }
    let mut file = receipt("file", "file", "file", "C:\\Work\\File", 0, 160_000);
    file.kind = ActivityKind::FileOpenRequested;
    file.source = ReceiptSource::OpenAccepted;
    file.entity.kind = EntityKind::File;
    store.record(&file).unwrap();

    let rows = store
        .recent(100_000, 160_000, ActivityKind::FolderVisit, 0, 100)
        .unwrap()
        .unwrap();
    assert_eq!(
        rows.iter()
            .map(|row| row.entity.id.as_str())
            .collect::<Vec<_>>(),
        vec!["z", "a"]
    );
    assert_eq!(rows[0].episode_count, 2);
    assert_eq!(rows[1].episode_count, 1);
    assert_eq!(rows[0].last_utc_ms, 150_000);
    assert_eq!(store.episode_count().unwrap(), 4);
    assert_eq!(
        store
            .recent(100_000, 160_000, ActivityKind::FolderVisit, 0, 1)
            .unwrap()
            .unwrap()
            .len(),
        1
    );
    assert!(
        store
            .recent(0, 200_000, ActivityKind::FolderVisit, 0, 101)
            .is_err()
    );
    assert_eq!(
        store
            .recent(160_000, 160_000, ActivityKind::FolderVisit, 0, 100)
            .unwrap()
            .unwrap()
            .len(),
        0
    );
    assert_eq!(
        store
            .recent(0, 200_000, ActivityKind::FileOpenRequested, 0, 100)
            .unwrap()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn recent_rejects_stale_epochs_and_purged_rows_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("activity.sqlite3");
    {
        let mut store = ActivityStore::open(&path).unwrap();
        store
            .record(&receipt(
                "first",
                "first",
                "private",
                "C:\\Work\\Private",
                0,
                100_000,
            ))
            .unwrap();
        assert_eq!(
            store
                .recent(0, 200_000, ActivityKind::FolderVisit, 0, 100)
                .unwrap()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(store.exclude_root("C:\\Work\\Private").unwrap(), 1);
        assert!(
            store
                .recent(0, 200_000, ActivityKind::FolderVisit, 0, 100)
                .unwrap()
                .is_none()
        );
    }
    let mut store = ActivityStore::open(&path).unwrap();
    assert!(
        store
            .recent(0, 200_000, ActivityKind::FolderVisit, 1, 100)
            .unwrap()
            .unwrap()
            .is_empty()
    );
    assert_eq!(store.clear_all().unwrap(), 2);
    assert!(
        store
            .recent(0, 200_000, ActivityKind::FolderVisit, 1, 100)
            .unwrap()
            .is_none()
    );
}

#[test]
fn recent_paging_has_stable_ties_and_rejects_changed_data() {
    let mut store = ActivityStore::in_memory().unwrap();
    for (id, at) in [("z", 100_000), ("a", 100_000), ("m", 90_000)] {
        store
            .record(&receipt(id, id, id, &format!("C:\\Work\\{id}"), 0, at))
            .unwrap();
    }
    let first = store
        .recent_page(0, 200_000, ActivityKind::FolderVisit, 0, 1, None)
        .unwrap()
        .unwrap();
    assert_eq!(first.rows[0].entity.id, "a");
    let cursor = first.next_cursor.unwrap();
    assert_eq!(
        store
            .record(&receipt("z", "z", "z", "C:\\Work\\z", 0, 100_000))
            .unwrap(),
        RecordOutcome::Duplicate
    );
    let second = store
        .recent_page(0, 200_000, ActivityKind::FolderVisit, 0, 1, Some(&cursor))
        .unwrap()
        .unwrap();
    assert_eq!(second.rows[0].entity.id, "z");
    let third = store
        .recent_page(
            0,
            200_000,
            ActivityKind::FolderVisit,
            0,
            1,
            second.next_cursor.as_ref(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(third.rows[0].entity.id, "m");
    assert!(third.next_cursor.is_none());
    assert!(
        store
            .recent_page(0, 300_000, ActivityKind::FolderVisit, 0, 1, Some(&cursor))
            .is_err()
    );
    store
        .record(&receipt("new", "new", "new", "C:\\Work\\New", 0, 120_000))
        .unwrap();
    assert!(
        store
            .recent_page(0, 200_000, ActivityKind::FolderVisit, 0, 1, Some(&cursor))
            .is_err()
    );
    assert_eq!(store.episode_count().unwrap(), 4);
}

#[test]
fn recent_paging_cursor_survives_restart_and_rejects_retention_purge() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("activity.sqlite3");
    let cursor = {
        let mut store = ActivityStore::open(&path).unwrap();
        for (id, at) in [("older", 100_000), ("newer", 200_000)] {
            store
                .record(&receipt(id, id, id, &format!("C:\\Work\\{id}"), 0, at))
                .unwrap();
        }
        store
            .recent_page(0, 300_000, ActivityKind::FolderVisit, 0, 1, None)
            .unwrap()
            .unwrap()
            .next_cursor
            .unwrap()
    };
    let mut store = ActivityStore::open(&path).unwrap();
    let second = store
        .recent_page(0, 300_000, ActivityKind::FolderVisit, 0, 1, Some(&cursor))
        .unwrap()
        .unwrap();
    assert_eq!(second.rows[0].entity.id, "older");
    assert!(second.next_cursor.is_none());
    assert_eq!(store.purge_before(150_000).unwrap(), 1);
    assert!(
        store
            .recent_page(0, 300_000, ActivityKind::FolderVisit, 0, 1, Some(&cursor))
            .is_err()
    );
}

#[test]
fn recent_cursor_rejects_forgery_and_privacy_invalidation() {
    let mut store = ActivityStore::in_memory().unwrap();
    for id in ["a", "b"] {
        store
            .record(&receipt(id, id, id, &format!("C:\\Work\\{id}"), 0, 100_000))
            .unwrap();
    }
    let cursor = store
        .recent_page(0, 200_000, ActivityKind::FolderVisit, 0, 1, None)
        .unwrap()
        .unwrap()
        .next_cursor
        .unwrap();
    let mut forged = cursor.clone();
    forged.last_entity_id = "other".into();
    assert!(
        store
            .recent_page(0, 200_000, ActivityKind::FolderVisit, 0, 1, Some(&forged))
            .is_err()
    );
    let mut wrong_revision = cursor.clone();
    wrong_revision.revision += 1;
    assert!(
        store
            .recent_page(
                0,
                200_000,
                ActivityKind::FolderVisit,
                0,
                1,
                Some(&wrong_revision)
            )
            .is_err()
    );
    let forged_empty = RecentCursor {
        last_entity_id: String::new(),
        ..cursor.clone()
    };
    assert!(
        store
            .recent_page(
                0,
                200_000,
                ActivityKind::FolderVisit,
                0,
                1,
                Some(&forged_empty)
            )
            .is_err()
    );
    assert_eq!(store.clear_all().unwrap(), 1);
    assert!(
        store
            .recent_page(0, 200_000, ActivityKind::FolderVisit, 0, 1, Some(&cursor))
            .unwrap()
            .is_none()
    );
}

#[test]
fn frequent_uses_utc_day_cap_and_excludes_one_day_items() {
    const DAY: u64 = 86_400_000;
    let mut store = ActivityStore::in_memory().unwrap();
    for index in 0..7 {
        let at = DAY + 1_000 + index * 31_000;
        let event = format!("older-{index}");
        store
            .record(&receipt(&event, &event, "older", "C:\\Work\\Older", 0, at))
            .unwrap();
    }
    for (event, entity, path, at) in [
        ("older-new", "older", "C:\\Work\\Older", 2 * DAY + 1_000),
        ("regular-one", "regular", "C:\\Work\\Regular", DAY + 1_000),
        (
            "regular-two",
            "regular",
            "C:\\Work\\Regular",
            2 * DAY + 1_000,
        ),
        ("one-day", "one-day", "C:\\Work\\OneDay", 2 * DAY + 2_000),
    ] {
        store
            .record(&receipt(event, event, entity, path, 0, at))
            .unwrap();
    }
    let rows = store
        .frequent(0, 2 * DAY + 3_000, ActivityKind::FolderVisit, 0, 10)
        .unwrap()
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].entity.id, "older");
    assert_eq!(rows[0].episode_count, 8);
    assert_eq!(rows[0].active_days, 2);
    let expected = 5.0 * 2.0_f64.powf(-1.0 / 14.0) + 1.0;
    assert!((rows[0].score - expected).abs() < 1e-10);
    assert_eq!(rows[1].entity.id, "regular");
    assert_eq!(rows[1].episode_count, 2);
    assert_eq!(store.episode_count().unwrap(), 11);
    assert!(
        store
            .frequent(0, 2 * DAY + 3_000, ActivityKind::FolderVisit, 0, 0)
            .is_err()
    );
}

#[test]
fn frequent_obeys_kind_time_privacy_and_purge() {
    const DAY: u64 = 86_400_000;
    let mut store = ActivityStore::in_memory().unwrap();
    for (event, at) in [("first", DAY), ("second", 2 * DAY)] {
        store
            .record(&receipt(event, event, "folder", "C:\\Work\\Folder", 0, at))
            .unwrap();
    }
    let mut file = receipt("file", "file", "file", "C:\\Work\\File", 0, DAY);
    file.kind = ActivityKind::FileOpenRequested;
    file.source = ReceiptSource::OpenAccepted;
    file.entity.kind = EntityKind::File;
    store.record(&file).unwrap();
    assert_eq!(
        store
            .frequent(0, 2 * DAY, ActivityKind::FolderVisit, 0, 10)
            .unwrap()
            .unwrap()
            .len(),
        1
    );
    assert!(
        store
            .frequent(2 * DAY, 2 * DAY, ActivityKind::FolderVisit, 0, 10)
            .unwrap()
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .frequent(0, 2 * DAY, ActivityKind::FileOpenRequested, 0, 10)
            .unwrap()
            .unwrap()
            .is_empty()
    );
    assert_eq!(store.purge_before(2 * DAY).unwrap(), 2);
    assert!(
        store
            .frequent(0, 2 * DAY, ActivityKind::FolderVisit, 0, 10)
            .unwrap()
            .unwrap()
            .is_empty()
    );
    assert_eq!(store.clear_all().unwrap(), 1);
    assert!(
        store
            .frequent(0, 2 * DAY, ActivityKind::FolderVisit, 0, 10)
            .unwrap()
            .is_none()
    );
}

#[test]
fn pause_and_identity_conflict_do_not_corrupt_history() {
    let mut store = ActivityStore::in_memory().unwrap();
    let original = receipt(
        "first",
        "first-action",
        "stable",
        "C:\\Work\\Reports",
        0,
        100_000,
    );
    store.record(&original).unwrap();
    let conflict = receipt(
        "second",
        "second-action",
        "stable",
        "C:\\Work\\Other",
        0,
        200_000,
    );
    assert!(store.record(&conflict).is_err());
    assert_eq!(store.episode_count().unwrap(), 1);
    assert_eq!(store.set_recording_enabled(false).unwrap(), 1);
    assert_eq!(
        store
            .record(&receipt(
                "third",
                "third-action",
                "another",
                "C:\\Work\\Other",
                0,
                200_000
            ))
            .unwrap(),
        RecordOutcome::StaleEpoch
    );
    assert_eq!(
        store
            .record(&receipt(
                "fourth",
                "fourth-action",
                "another",
                "C:\\Work\\Other",
                1,
                200_000
            ))
            .unwrap(),
        RecordOutcome::Ignored
    );
    assert_eq!(store.episode_count().unwrap(), 1);
}

/// ACT-09 benchmark fixture: 100k entities / 1M episodes seeded through plain
/// SQL (record() per receipt would measure writes, not query latency), then
/// warm p95 over recent / frequent / candidate queries on the real SQLite
/// store. Run with: cargo test --offline -j 1 --test store -- --ignored --nocapture
#[test]
#[ignore = "benchmark dataset; run explicitly"]
fn benchmark_100k_entities_1m_episodes_query_p95() {
    use rusqlite::params;
    use std::time::Instant;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bench.sqlite3");
    let mut store = ActivityStore::open(&path).unwrap();
    let epoch = store.privacy_epoch().unwrap();

    const ENTITIES: u64 = 100_000;
    const EPISODES_PER_ENTITY: u64 = 10;
    let seed_start = Instant::now();
    {
        let mut connection = Connection::open(&path).unwrap();
        let tx = connection.transaction().unwrap();
        {
            let mut entity = tx
                .prepare(
                    "INSERT INTO activity_entities(id, kind, volume_id, locator, parent_path, display_name, availability, metadata)
                     VALUES (?1, 'Folder', 'C:', ?2, 'C:\\Bench', ?3, 'Available', ?4)",
                )
                .unwrap();
            let mut episode = tx
                .prepare(
                    "INSERT INTO activity_episodes(entity_id, kind, start_utc_ms, last_utc_ms)
                     VALUES (?1, 'folderVisit', ?2, ?2)",
                )
                .unwrap();
            let mut vocab = tx
                .prepare("INSERT INTO activity_vocabulary(entity_id, search_key) VALUES (?1, ?2)")
                .unwrap();
            let mut gram = tx
                .prepare("INSERT OR IGNORE INTO activity_search_grams(kind, gram, entity_id) VALUES ('Folder', ?1, ?2)")
                .unwrap();
            for index in 0..ENTITIES {
                let id = format!("bench-{index:06}");
                let name = if index % 10 == 0 {
                    format!("Project Reports {index:06}")
                } else if index % 10 == 5 {
                    format!("Invoices Archive {index:06}")
                } else {
                    format!("Folder Series {index:06}")
                };
                let locator = format!("C:\\Bench\\{index:06}");
                let metadata = serde_json::to_string(&ActivityEntity {
                    id: id.clone(),
                    kind: EntityKind::Folder,
                    provider: "local".into(),
                    volume_id: "C:".into(),
                    locator: locator.clone(),
                    parent_path: "C:\\Bench".into(),
                    display_name: name.clone(),
                    availability: Availability::Available,
                })
                .unwrap();
                entity
                    .execute(params![id, locator, name, metadata])
                    .unwrap();
                vocab.execute(params![id, name.to_lowercase()]).unwrap();
                let lower = name.to_lowercase();
                let chars: Vec<char> = lower.chars().collect();
                for window in chars.windows(3) {
                    let piece: String = window.iter().collect();
                    gram.execute(params![piece, id]).unwrap();
                }
                // Word-token prefix entries, as index_name() writes them (schema v13).
                for word in lower.split(' ') {
                    gram.execute(params![format!("{}{word}", char::from(1u8)), id]).unwrap();
                }
                let base = 90 * 86_400_000u64;
                for slot in 0..EPISODES_PER_ENTITY {
                    // Offset keeps every episode inside the queried retention window, as the
                    // app purges older buckets at the same cutoff.
                    let at = (base / EPISODES_PER_ENTITY) * slot + 1_000_000 + index % 3_600_000;
                    episode.execute(params![id, at as i64]).unwrap();
                }
            }
        }
        // A real store builds its rollups through record(); mirror that state
        // so the benchmark measures production-shaped tables.
        tx.execute(
            "INSERT OR REPLACE INTO activity_entity_summary(entity_id, kind, last_utc_ms, episodes)
             SELECT entity_id, kind, MAX(last_utc_ms), COUNT(*)
             FROM activity_episodes GROUP BY entity_id, kind",
            [],
        )
        .unwrap();
        tx.execute(
            "INSERT OR REPLACE INTO activity_daily(entity_id, kind, utc_day, episodes, last_utc_ms)
             SELECT entity_id, kind, last_utc_ms / 86400000, COUNT(*), MAX(last_utc_ms)
             FROM activity_episodes GROUP BY entity_id, kind, last_utc_ms / 86400000",
            [],
        )
        .unwrap();
        ferrum_activity_store::rebuild_rank(&tx).unwrap();
        tx.commit().unwrap();
    }
    let seed = seed_start.elapsed();
    {
        let connection = Connection::open(&path).unwrap();
        let grams: i64 = connection
            .query_row("SELECT COUNT(*) FROM activity_search_grams", [], |r| {
                r.get(0)
            })
            .unwrap();
        let entities: i64 = connection
            .query_row("SELECT COUNT(*) FROM activity_entities", [], |r| r.get(0))
            .unwrap();
        println!("probe: entities={entities} grams={grams}");
    }

    fn p95(samples: &mut [u128]) -> u128 {
        samples.sort_unstable();
        samples[(samples.len() as f64 * 0.95) as usize % samples.len()]
    }
    let mut recent_times = Vec::new();
    let mut frequent_times = Vec::new();
    let mut exact_times = Vec::new();
    let mut fuzzy_times = Vec::new();
    let mut prefix_times = Vec::new();
    let mut short_times = Vec::new();
    let now: u64 = 90 * 86_400_000 + 1_000_000;
    for _ in 0..50 {
        let start = Instant::now();
        let rows = store
            .recent(0, now, ActivityKind::FolderVisit, epoch, 100)
            .unwrap()
            .unwrap();
        recent_times.push(start.elapsed().as_millis());
        assert_eq!(rows.len(), 100);

        let start = Instant::now();
        let rows = store
            .frequent(
                now - 90 * 86_400_000,
                now,
                ActivityKind::FolderVisit,
                epoch,
                100,
            )
            .unwrap()
            .unwrap();
        frequent_times.push(start.elapsed().as_millis());
        assert_eq!(rows.len(), 100);

        let start = Instant::now();
        let hits = store
            .search_candidates("project reports", ActivityKind::FolderVisit, epoch, 100)
            .unwrap()
            .unwrap();
        exact_times.push(start.elapsed().as_millis());
        assert!(!hits.is_empty());

        let start = Instant::now();
        let hits = store
            .search_candidates("reoprts", ActivityKind::FolderVisit, epoch, 100)
            .unwrap()
            .unwrap();
        fuzzy_times.push(start.elapsed().as_millis());
        assert!(!hits.is_empty());

        let start = Instant::now();
        let hits = store
            .search_candidates("proj rep", ActivityKind::FolderVisit, epoch, 100)
            .unwrap()
            .unwrap();
        prefix_times.push(start.elapsed().as_millis());
        assert!(!hits.is_empty());

        let start = Instant::now();
        let hits = store
            .search_candidates("in", ActivityKind::FolderVisit, epoch, 100)
            .unwrap()
            .unwrap();
        short_times.push(start.elapsed().as_millis());
        assert!(!hits.is_empty());
    }
    println!(
        "prefix-search p95 {} ms, short-query p95 {} ms",
        p95(&mut prefix_times),
        p95(&mut short_times)
    );
    let (recent_p95, frequent_p95, exact_p95, fuzzy_p95) = (
        p95(&mut recent_times),
        p95(&mut frequent_times),
        p95(&mut exact_times),
        p95(&mut fuzzy_times),
    );
    println!(
        "seed 100k entities / {} episodes: {seed:?}; warm p50/p95 ms — recent {}/{} frequent {}/{} exact-search {}/{} typo-search {}/{}",
        ENTITIES * EPISODES_PER_ENTITY,
        recent_times[recent_times.len() / 2],
        recent_p95,
        frequent_times[frequent_times.len() / 2],
        frequent_p95,
        exact_times[exact_times.len() / 2],
        exact_p95,
        fuzzy_times[fuzzy_times.len() / 2],
        fuzzy_p95,
    );
}

/// The ranked fast path must agree with a brute-force score over the same
/// episodes, and stay correct after purge and forget.
#[test]
fn frequent_rank_table_matches_bruteforce_and_follows_purge_and_forget() {
    const DAY: u64 = 86_400_000;
    let mut store = ActivityStore::in_memory().unwrap();
    // (id, days since epoch of visits, visits that day)
    let plan: [(&str, &[(u64, u32)]); 5] = [
        ("a", &[(1, 9), (5, 1), (9, 2)]),
        ("b", &[(8, 1), (9, 1)]),
        ("c", &[(2, 3), (3, 3), (4, 3), (9, 1)]),
        ("d", &[(9, 7)]),
        ("e", &[(6, 1), (7, 1), (8, 1), (9, 1)]),
    ];
    let mut serial = 0u64;
    for (id, visits) in plan {
        for (day, count) in visits {
            for repeat in 0..*count {
                // Episodes >30s apart so each visit is a new episode.
                let at = day * DAY + u64::from(repeat) * 60_000 + serial;
                serial += 1;
                store
                    .record(&receipt(
                        &format!("{id}-{day}-{repeat}"),
                        &format!("{id}-{day}-{repeat}"),
                        id,
                        &format!("C:\\Work\\{id}"),
                        0,
                        at,
                    ))
                    .unwrap();
            }
        }
    }
    let now = 9 * DAY + 3_600_000;
    let expected = |skip: &[&str]| {
        let today = (now / DAY) as i64;
        let mut scored: Vec<(String, f64)> = plan
            .iter()
            .filter(|(id, visits)| visits.len() >= 2 && !skip.contains(id))
            .map(|(id, visits)| {
                let score: f64 = visits
                    .iter()
                    .map(|(day, count)| {
                        f64::from((*count).min(5))
                            * 2.0_f64.powf(-((today - *day as i64) as f64) / 14.0)
                    })
                    .sum();
                ((*id).to_string(), score)
            })
            .collect();
        scored.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        scored
    };
    let check = |store: &mut ActivityStore, skip: &[&str]| {
        let epoch = store.privacy_epoch().unwrap();
        let rows = store
            .frequent(0, now, ActivityKind::FolderVisit, epoch, 10)
            .unwrap()
            .unwrap();
        let want = expected(skip);
        assert_eq!(rows.len(), want.len());
        for (row, (id, score)) in rows.iter().zip(want) {
            assert_eq!(row.entity.id, id);
            assert!((row.score - score).abs() < 1e-9, "{id}: {} vs {score}", row.score);
        }
    };
    check(&mut store, &[]);
    // A smaller limit takes the same prefix from the ranked table.
    let top2 = store
        .frequent(0, now, ActivityKind::FolderVisit, 0, 2)
        .unwrap()
        .unwrap();
    let want = expected(&[]);
    assert_eq!(top2[0].entity.id, want[0].0);
    assert_eq!(top2[1].entity.id, want[1].0);
    store.forget_entity("a").unwrap();
    check(&mut store, &["a"]);
}

// ---------------------------------------------------------------------------
// FER-254 / FER-255: workspace root filter, token-prefix retrieval, popularity
// ---------------------------------------------------------------------------

fn visit_at(store: &mut ActivityStore, id: &str, path: &str, name: &str, at: u64) {
    let mut value = receipt(
        &format!("{id}-{at}"),
        &format!("act-{id}-{at}"),
        id,
        path,
        0,
        at,
    );
    value.entity.display_name = name.into();
    store.record(&value).unwrap();
}

fn ids_of(rows: Vec<ActivityEntity>) -> Vec<String> {
    rows.into_iter().map(|e| e.id).collect()
}

#[test]
fn short_and_prefix_queries_are_served_by_token_index_not_gram_sampling() {
    let mut store = ActivityStore::in_memory().unwrap();
    store
        .record(&named_receipt("zz-target", "Quarry Notes", 10, 0))
        .unwrap();
    store
        .record(&named_receipt("zz-inv", "Zed Invoice", 11, 0))
        .unwrap();
    for index in 0..150 {
        store
            .record(&named_receipt(
                &format!("aa-{index:03}"),
                "Invoices",
                100_000 + index,
                0,
            ))
            .unwrap();
    }
    // A two-letter prefix has no 3-gram; only the token index can find it.
    assert_eq!(candidates(&mut store, "qu", 0), vec!["zz-target"]);
    assert!(candidates(&mut store, "no", 0).contains(&"zz-target".to_string()));
    // 150 "Invoices" saturate the "inv" gram posting window (100, by id order);
    // the later-id "Invoice" must still be retrieved through the prefix index.
    assert!(candidates(&mut store, "invoi", 0).contains(&"zz-inv".to_string()));
    assert!(candidates(&mut store, "inv", 0).contains(&"zz-inv".to_string()));
    // Multi-word prefix: every word is a prefix seek.
    assert!(candidates(&mut store, "quar no", 0).contains(&"zz-target".to_string()));
}

#[test]
fn recall_fixture_typos_and_abbreviations_still_resolve() {
    let mut store = ActivityStore::in_memory().unwrap();
    store.record(&named_receipt("inv", "Invoice", 1, 0)).unwrap();
    store.record(&named_receipt("prj", "project", 2, 0)).unwrap();
    store.record(&named_receipt("doc", "documents", 3, 0)).unwrap();
    store.record(&named_receipt("i26", "Invoice 2026", 4, 0)).unwrap();
    assert!(candidates(&mut store, "invoce", 0).contains(&"inv".to_string()));
    assert!(candidates(&mut store, "proejct", 0).contains(&"prj".to_string()));
    assert!(candidates(&mut store, "documemts", 0).contains(&"doc".to_string()));
    assert!(candidates(&mut store, "inv2026", 0).contains(&"i26".to_string()));
}

#[test]
fn migrates_version_twelve_and_backfills_token_index() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("activity.sqlite3");
    {
        let mut store = ActivityStore::open(&path).unwrap();
        store
            .record(&named_receipt("older", "Quarry Notes", 100, 0))
            .unwrap();
    }
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "DELETE FROM activity_search_grams WHERE substr(gram, 1, 1) IN (char(1), char(2));
             PRAGMA user_version = 12;",
        )
        .unwrap();
    drop(connection);
    let mut store = ActivityStore::open(&path).unwrap();
    assert_eq!(candidates(&mut store, "qu", 0), vec!["older"]);
    assert_eq!(candidates(&mut store, "qn", 0), vec!["older"], "initials");
    drop(store);
    let connection = Connection::open(path).unwrap();
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    const { assert!(SCHEMA_VERSION >= 13) };
}

#[test]
fn token_index_follows_rename_and_forget() {
    let mut store = ActivityStore::in_memory().unwrap();
    visit_at(&mut store, "e1", "C:\\Work\\e1", "Quarry Notes", 1_000_000);
    assert_eq!(candidates(&mut store, "qu", 0), vec!["e1"]);
    // Same locator, new display name: the old tokens must go.
    visit_at(&mut store, "e1", "C:\\Work\\e1", "Harbor Plans", 2_000_000);
    assert!(candidates(&mut store, "qu", 0).is_empty());
    assert_eq!(candidates(&mut store, "ha", 0), vec!["e1"]);
    store.forget_entity("e1").unwrap();
    let epoch = store.privacy_epoch().unwrap();
    assert!(candidates(&mut store, "ha", epoch).is_empty());
}

fn scoped_store() -> ActivityStore {
    const D: u64 = 86_400_000;
    let mut store = ActivityStore::in_memory().unwrap();
    for (id, path, name) in [
        ("in-a", "C:\\Work\\A\\Reports", "Reports"),
        ("in-a-deep", "C:\\Work\\A\\Deep\\Reports Q2", "Reports Q2"),
        ("sibling", "C:\\Work\\AB\\Reports", "Reports"),
        ("outside", "C:\\Other\\Reports", "Reports"),
        ("root-itself", "C:\\Work\\A", "A"),
    ] {
        // Two distinct UTC days so frequent() admits every entity.
        visit_at(&mut store, id, path, name, 10 * D + 1_000);
        visit_at(&mut store, id, path, name, 11 * D + 1_000);
    }
    store
}

#[test]
fn workspace_root_filters_recent_frequent_and_search() {
    const D: u64 = 86_400_000;
    let mut store = scoped_store();
    let kind = ActivityKind::FolderVisit;
    let sorted = |mut v: Vec<String>| {
        v.sort();
        v
    };
    let want = vec!["in-a", "in-a-deep", "root-itself"];
    // Case, separator spelling and a trailing separator do not matter on Windows paths.
    for root in ["C:\\Work\\A", "c:/work/a/", "C:\\WORK\\A\\"] {
        let recent = store
            .recent_in(0, 20 * D, kind.clone(), 0, 100, Some(root))
            .unwrap()
            .unwrap();
        assert_eq!(
            sorted(recent.into_iter().map(|r| r.entity.id).collect()),
            want,
            "recent {root}"
        );
        let frequent = store
            .frequent_in(0, 12 * D, kind.clone(), 0, 100, Some(root))
            .unwrap()
            .unwrap();
        assert_eq!(
            sorted(frequent.into_iter().map(|r| r.entity.id).collect()),
            want,
            "frequent {root}"
        );
        let found = ids_of(
            store
                .search_candidates_in("reports", kind.clone(), 0, 100, Some(root))
                .unwrap()
                .unwrap(),
        );
        assert_eq!(sorted(found), vec!["in-a", "in-a-deep"], "search {root}");
        let by_path = ids_of(
            store
                .locator_candidates_in("reports", kind.clone(), 0, 100, Some(root))
                .unwrap()
                .unwrap(),
        );
        assert_eq!(sorted(by_path), vec!["in-a", "in-a-deep"], "locator {root}");
    }
    // Unfiltered behaviour is unchanged and includes the sibling and outside folders.
    let all = ids_of(
        store
            .search_candidates("reports", kind.clone(), 0, 100)
            .unwrap()
            .unwrap(),
    );
    assert_eq!(all.len(), 4);
    let unfiltered = store
        .recent(0, 20 * D, kind.clone(), 0, 100)
        .unwrap()
        .unwrap();
    assert_eq!(unfiltered.len(), 5);
    // A root with no history is an empty, valid result; a relative root is rejected.
    assert!(
        store
            .recent_in(0, 20 * D, kind.clone(), 0, 100, Some("C:\\Nowhere"))
            .unwrap()
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .recent_in(0, 20 * D, kind, 0, 100, Some("relative/path"))
            .is_err()
    );
}

#[test]
fn workspace_filter_applies_before_the_result_limit() {
    const D: u64 = 86_400_000;
    let mut store = ActivityStore::in_memory().unwrap();
    // 120 newer entities outside the workspace would fill the top of an
    // unfiltered query; the single older one inside must still be returned.
    visit_at(&mut store, "mine", "C:\\Work\\A\\Mine", "Mine", 5 * D);
    visit_at(&mut store, "mine", "C:\\Work\\A\\Mine", "Mine", 6 * D);
    for index in 0..120 {
        let id = format!("far-{index:03}");
        let path = format!("C:\\Elsewhere\\{id}");
        visit_at(&mut store, &id, &path, "Mine Far", 9 * D + index * 60_000);
        visit_at(&mut store, &id, &path, "Mine Far", 10 * D + index * 60_000);
    }
    let kind = ActivityKind::FolderVisit;
    let recent = store
        .recent_in(0, 20 * D, kind.clone(), 0, 5, Some("C:\\Work\\A"))
        .unwrap()
        .unwrap();
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0].entity.id, "mine");
    let frequent = store
        .frequent_in(0, 12 * D, kind.clone(), 0, 5, Some("C:\\Work\\A"))
        .unwrap()
        .unwrap();
    assert_eq!(frequent.len(), 1);
    assert_eq!(frequent[0].entity.id, "mine");
    let found = store
        .search_candidates_in("mine", kind, 0, 5, Some("C:\\Work\\A"))
        .unwrap()
        .unwrap();
    assert_eq!(ids_of(found), vec!["mine"]);
}

#[test]
fn entity_popularity_counts_episodes_inside_the_window_only() {
    const D: u64 = 86_400_000;
    let mut store = ActivityStore::in_memory().unwrap();
    visit_at(&mut store, "p", "C:\\Work\\p", "P", 2 * D);
    visit_at(&mut store, "p", "C:\\Work\\p", "P", 5 * D);
    visit_at(&mut store, "p", "C:\\Work\\p", "P", 5 * D + 120_000);
    visit_at(&mut store, "q", "C:\\Work\\q", "Q", 5 * D);
    let ids = vec!["p".to_string(), "q".to_string(), "missing".to_string()];
    let kind = ActivityKind::FolderVisit;
    let all = store.entity_popularity(&ids, kind.clone(), 0, 6 * D).unwrap();
    assert_eq!(all["p"].episodes, 3);
    assert_eq!(all["p"].active_days, 2);
    assert_eq!(all["p"].last_utc_ms, 5 * D + 120_000);
    assert_eq!(all["q"].episodes, 1);
    assert!(!all.contains_key("missing"));
    // from bound excludes the oldest bucket; now bound excludes the future.
    let window = store
        .entity_popularity(&ids, kind.clone(), 3 * D, 6 * D)
        .unwrap();
    assert_eq!(window["p"].episodes, 2);
    assert_eq!(window["p"].active_days, 1);
    let before = store.entity_popularity(&ids, kind, 0, 4 * D).unwrap();
    assert_eq!(before["p"].episodes, 1);
    assert!(!before.contains_key("q"));
}

/// FER-255: day buckets and decay use the UTC calendar only, at midnight and across days.
#[test]
fn utc_day_boundary_controls_frequent_score_and_window() {
    const D: u64 = 86_400_000;
    let day = 40 * D;
    let mut store = ActivityStore::in_memory().unwrap();
    // 45 s apart (a new episode) but on different UTC days.
    visit_at(&mut store, "mid", "C:\\Work\\mid", "Mid", day - 40_000);
    visit_at(&mut store, "mid", "C:\\Work\\mid", "Mid", day + 5_000);
    let kind = ActivityKind::FolderVisit;
    let score_at = |store: &mut ActivityStore, now: u64| -> Option<(f64, u64)> {
        store
            .frequent(0, now, kind.clone(), 0, 10)
            .unwrap()
            .unwrap()
            .into_iter()
            .next()
            .map(|r| (r.score, r.active_days))
    };
    // 00:00:10 UTC: yesterday's visit is age 1, today's age 0.
    let (score, days) = score_at(&mut store, day + 10_000).unwrap();
    assert_eq!(days, 2);
    assert!((score - (1.0 + 2.0_f64.powf(-1.0 / 14.0))).abs() < 1e-9);
    // 23:59:59.999 UTC the day before: today's visit is in the future and one
    // active day remains, so the entity is not frequent.
    assert!(score_at(&mut store, day - 1).is_none());
    // Next UTC day: ages 2 and 1.
    let (score, days) = score_at(&mut store, day + D + 1).unwrap();
    assert_eq!(days, 2);
    let expected = 2.0_f64.powf(-2.0 / 14.0) + 2.0_f64.powf(-1.0 / 14.0);
    assert!((score - expected).abs() < 1e-9);
    // Window start at the first visit keeps it; one ms later drops its bucket.
    let included = store
        .frequent(day - 40_000, day + 10_000, kind.clone(), 0, 10)
        .unwrap()
        .unwrap();
    assert_eq!(included.len(), 1);
    let excluded = store
        .frequent(day - 39_999, day + 10_000, kind, 0, 10)
        .unwrap()
        .unwrap();
    assert!(excluded.is_empty(), "one bucket left, below two active days");
}

#[test]
fn abbreviation_queries_are_retrieved_through_the_initials_index() {
    let mut store = ActivityStore::in_memory().unwrap();
    store
        .record(&named_receipt("qbr", "Quarterly Business Review", 1, 0))
        .unwrap();
    store
        .record(&named_receipt("i26", "Invoice 2026", 2, 0))
        .unwrap();
    store.record(&named_receipt("one", "Solo", 3, 0)).unwrap();
    // No 3-gram or word prefix of "qbr" occurs in the name: initials only.
    assert_eq!(candidates(&mut store, "qbr", 0), vec!["qbr"]);
    assert_eq!(candidates(&mut store, "QBR", 0), vec!["qbr"]);
    // Initials of the words before a trailing number, plus the number.
    assert!(candidates(&mut store, "i2026", 0).contains(&"i26".to_string()));
    // Single-word names have no initials entry.
    assert!(!candidates(&mut store, "s", 0).is_empty());
}
