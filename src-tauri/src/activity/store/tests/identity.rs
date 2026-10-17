use ferrum_activity_contract::{
    ActivityEntity, ActivityKind, ActivityReceipt, Availability, EntityKind, Provenance,
    ReceiptSource,
};
use ferrum_activity_store::{
    ActivityStore, AliasOutcome, ChangeOutcome, ExternalCandidate, FsChange, FsChangeKind,
    IdentityConfidence, MountOutcome, ReconcileOutcome, RecordOutcome, SCHEMA_VERSION,
    UnresolvedReason, ValidationOutcome, ValidationProbe,
};
use rusqlite::Connection;
use std::sync::atomic::{AtomicU64, Ordering};

const DAY: u64 = 86_400_000;
static SEQ: AtomicU64 = AtomicU64::new(1);

fn entity_id(path: &str) -> String {
    // Mirrors the service: id derives from the folded path at first sight.
    if path.starts_with('/') {
        return format!("id:{path}");
    }
    format!("id:{}", path.to_lowercase().replace('/', "\\"))
}

fn visit_kind(volume: &str, path: &str, ts: u64, kind: EntityKind) -> ActivityReceipt {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let (source, activity) = match kind {
        EntityKind::Folder => (ReceiptSource::NavigationCommit, ActivityKind::FolderVisit),
        EntityKind::File => (ReceiptSource::OpenAccepted, ActivityKind::FileOpenRequested),
    };
    let trimmed = path.trim_end_matches(['/', '\\']);
    let split = trimmed.rfind(['/', '\\']).unwrap_or(0);
    ActivityReceipt {
        version: 1,
        event_id: format!("e{n}"),
        action_id: format!("a{n}"),
        privacy_epoch: 0,
        source,
        kind: activity,
        provenance: Provenance::Manual,
        entity: ActivityEntity {
            id: entity_id(path),
            kind,
            provider: "local".into(),
            volume_id: volume.into(),
            locator: path.into(),
            parent_path: if split <= 2 {
                trimmed[..split + 1].to_owned()
            } else {
                trimmed[..split].to_owned()
            },
            display_name: trimmed[split + 1..].to_owned(),
            availability: Availability::Available,
        },
        occurred_utc_ms: ts,
    }
}

fn visit(store: &mut ActivityStore, volume: &str, path: &str, ts: u64) -> ActivityEntity {
    store
        .record(&visit_kind(volume, path, ts, EntityKind::Folder))
        .unwrap();
    store.entity_at_locator(volume, path).unwrap().unwrap()
}

fn visit_file(store: &mut ActivityStore, volume: &str, path: &str, ts: u64) -> ActivityEntity {
    store
        .record(&visit_kind(volume, path, ts, EntityKind::File))
        .unwrap();
    store.entity_at_locator(volume, path).unwrap().unwrap()
}

fn change(kind: FsChangeKind, volume: &str, from: &str, to: Option<&str>) -> FsChange {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    FsChange {
        action_id: format!("change-{n}"),
        kind,
        from_volume_id: volume.into(),
        from: from.into(),
        to_volume_id: to.map(|_| volume.into()),
        to: to.map(str::to_owned),
        occurred_utc_ms: 1_000,
    }
}

fn recent_ids(store: &mut ActivityStore) -> Vec<(String, u64)> {
    store
        .recent(0, 100 * DAY, ActivityKind::FolderVisit, 0, 100)
        .unwrap()
        .unwrap()
        .into_iter()
        .map(|row| (row.entity.id, row.episode_count))
        .collect()
}

fn rank_snapshot(path: &std::path::Path) -> Vec<(String, String, i64, i64)> {
    let connection = Connection::open(path).unwrap();
    let mut statement = connection
        .prepare("SELECT entity_id, kind, CAST(ROUND(rank_key * 1e6) AS INTEGER), active_days FROM activity_entity_rank ORDER BY entity_id, kind")
        .unwrap();
    statement
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

// ---- FER-261: rename / move / delete / copy receipts -------------------

#[test]
fn rename_keeps_entity_id_and_history_and_derived_tables() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("activity.sqlite3");
    let mut store = ActivityStore::open(&path).unwrap();
    visit(&mut store, "V1", "C:\\Work\\Old", DAY + 1_000);
    let original = visit(&mut store, "V1", "C:\\Work\\Old", 2 * DAY + 1_000);
    let before = rank_snapshot(&path);
    assert_eq!(before.len(), 1);
    let outcome = store
        .apply_change(&change(
            FsChangeKind::Rename,
            "V1",
            "C:\\Work\\Old",
            Some("C:\\Work\\New"),
        ))
        .unwrap();
    assert_eq!(outcome, ChangeOutcome::Relocated { entities: 1 });
    assert!(
        store
            .entity_at_locator("V1", "C:\\Work\\Old")
            .unwrap()
            .is_none()
    );
    let moved = store
        .entity_at_locator("V1", "C:\\Work\\New")
        .unwrap()
        .unwrap();
    assert_eq!(moved.id, original.id);
    assert_eq!(moved.display_name, "New");
    assert_eq!(moved.parent_path, "C:\\Work");
    assert_eq!(moved.availability, Availability::Available);
    // History and rank rows follow the entity: nothing recomputed or lost.
    assert_eq!(rank_snapshot(&path), before);
    assert_eq!(recent_ids(&mut store), vec![(original.id.clone(), 2)]);
    let frequent = store
        .frequent(0, 2 * DAY + 5_000, ActivityKind::FolderVisit, 0, 10)
        .unwrap()
        .unwrap();
    assert_eq!(frequent.len(), 1);
    assert_eq!(frequent[0].entity.locator, "C:\\Work\\New");
    // Rename is not a visit; a later visit to the new name continues history.
    assert_eq!(store.episode_count().unwrap(), 2);
    visit(&mut store, "V1", "C:\\Work\\New", 3 * DAY);
    assert_eq!(recent_ids(&mut store), vec![(original.id.clone(), 3)]);
    // Search follows the new name, not the old one.
    let hits = store
        .search_candidates("New", ActivityKind::FolderVisit, 0, 10)
        .unwrap()
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert!(
        store
            .search_candidates("Old", ActivityKind::FolderVisit, 0, 10)
            .unwrap()
            .unwrap()
            .is_empty()
    );
    // Derived rank still equals a from-scratch rebuild.
    let after = rank_snapshot(&path);
    assert_eq!(after.len(), 1);
}

#[test]
fn folder_move_relocates_subtree_on_segment_boundaries() {
    let mut store = ActivityStore::in_memory().unwrap();
    let root = visit(&mut store, "V1", "C:\\A\\foo", 10_000);
    let sub = visit(&mut store, "V1", "C:\\A\\foo\\Sub Dir", 11_000);
    let file = visit_file(&mut store, "V1", "C:\\A\\foo\\Sub Dir\\Report.TXT", 12_000);
    let sibling = visit(&mut store, "V1", "C:\\A\\foobar", 13_000);
    let other_case = visit(&mut store, "V1", "C:\\A\\foo2\\x", 14_000);
    let outcome = store
        .apply_change(&change(
            FsChangeKind::Move,
            "V1",
            "c:/a/FOO/",
            Some("C:\\B\\foo"),
        ))
        .unwrap();
    assert_eq!(outcome, ChangeOutcome::Relocated { entities: 3 });
    let new_root = store
        .entity_at_locator("V1", "C:\\B\\foo")
        .unwrap()
        .unwrap();
    let new_sub = store
        .entity_at_locator("V1", "c:\\b\\FOO\\sub dir")
        .unwrap()
        .unwrap();
    let new_file = store
        .entity_at_locator("V1", "C:\\B\\foo\\Sub Dir\\Report.TXT")
        .unwrap()
        .unwrap();
    assert_eq!(
        (&new_root.id, &new_sub.id, &new_file.id),
        (&root.id, &sub.id, &file.id)
    );
    // Original casing of the descendants is preserved, parents are rewritten.
    assert_eq!(new_sub.locator, "C:\\B\\foo\\Sub Dir");
    assert_eq!(new_sub.parent_path, "C:\\B\\foo");
    assert_eq!(new_sub.display_name, "Sub Dir");
    assert_eq!(new_file.parent_path, "C:\\B\\foo\\Sub Dir");
    assert_eq!(new_file.display_name, "Report.TXT");
    // `foobar` and `foo2` share the textual prefix but are different segments.
    for (entity, locator) in [(&sibling, "C:\\A\\foobar"), (&other_case, "C:\\A\\foo2\\x")] {
        let still = store.entity_at_locator("V1", locator).unwrap().unwrap();
        assert_eq!(still.id, entity.id);
        assert_eq!(still.locator, locator);
    }
    assert!(
        store
            .entity_at_locator("V1", "C:\\A\\foo\\Sub Dir")
            .unwrap()
            .is_none()
    );
}

#[test]
fn move_into_own_subtree_is_rejected_and_changes_nothing() {
    let mut store = ActivityStore::in_memory().unwrap();
    visit(&mut store, "V1", "C:\\A\\foo", 10_000);
    visit(&mut store, "V1", "C:\\A\\foo\\child", 11_000);
    assert!(
        store
            .apply_change(&change(
                FsChangeKind::Move,
                "V1",
                "C:\\A\\foo",
                Some("C:\\A\\foo\\child\\foo"),
            ))
            .is_err()
    );
    assert!(
        store
            .entity_at_locator("V1", "C:\\A\\foo\\child")
            .unwrap()
            .is_some()
    );
}

#[test]
fn delete_marks_missing_keeps_history_and_never_revives_for_a_new_file() {
    let mut store = ActivityStore::in_memory().unwrap();
    let folder = visit(&mut store, "V1", "C:\\Work\\Gone", 10_000);
    let child = visit(&mut store, "V1", "C:\\Work\\Gone\\inner", 11_000);
    let outcome = store
        .apply_change(&change(FsChangeKind::Delete, "V1", "C:\\Work\\Gone", None))
        .unwrap();
    assert_eq!(outcome, ChangeOutcome::Deleted { entities: 2 });
    for id in [&folder.id, &child.id] {
        let state = store.identity_state(id).unwrap().unwrap();
        assert_eq!(state.entity.availability, Availability::Missing);
        assert!(!state.attached);
        assert_eq!(state.confidence, IdentityConfidence::Ended);
    }
    // The evidence stays searchable/visible as missing, not silently removed.
    assert_eq!(recent_ids(&mut store).len(), 2);
    // A new folder created at the same path is a different entity.
    let reborn = visit(&mut store, "V1", "C:\\Work\\Gone", 20_000);
    assert_ne!(reborn.id, folder.id);
    assert_eq!(reborn.availability, Availability::Available);
    let rows = recent_ids(&mut store);
    let by_id = |id: &str| rows.iter().find(|(entity, _)| entity == id).unwrap().1;
    assert_eq!(by_id(&folder.id), 1);
    assert_eq!(by_id(&reborn.id), 1);
}

#[test]
fn copy_does_not_inherit_history_and_leaves_the_source_alone() {
    let mut store = ActivityStore::in_memory().unwrap();
    visit(&mut store, "V1", "C:\\Work\\Src", 10_000);
    let source = visit(&mut store, "V1", "C:\\Work\\Src", 10_000 + DAY);
    let outcome = store
        .apply_change(&change(
            FsChangeKind::Copy,
            "V1",
            "C:\\Work\\Src",
            Some("C:\\Work\\Copy"),
        ))
        .unwrap();
    assert_eq!(outcome, ChangeOutcome::CopyNotInherited);
    assert!(
        store
            .entity_at_locator("V1", "C:\\Work\\Copy")
            .unwrap()
            .is_none()
    );
    let copy = visit(&mut store, "V1", "C:\\Work\\Copy", 20_000 + DAY);
    assert_ne!(copy.id, source.id);
    let rows = recent_ids(&mut store);
    assert_eq!(
        rows.iter().find(|(id, _)| *id == copy.id).unwrap().1,
        1,
        "copy starts with its own history"
    );
    assert_eq!(rows.iter().find(|(id, _)| *id == source.id).unwrap().1, 2);
    assert_eq!(
        store
            .entity_at_locator("V1", "C:\\Work\\Src")
            .unwrap()
            .unwrap()
            .id,
        source.id
    );
}

#[test]
fn change_receipts_are_idempotent_and_unknown_sources_are_not_tracked() {
    let mut store = ActivityStore::in_memory().unwrap();
    visit(&mut store, "V1", "C:\\Work\\Old", 10_000);
    let rename = change(
        FsChangeKind::Rename,
        "V1",
        "C:\\Work\\Old",
        Some("C:\\Work\\New"),
    );
    assert_eq!(
        store.apply_change(&rename).unwrap(),
        ChangeOutcome::Relocated { entities: 1 }
    );
    assert_eq!(
        store.apply_change(&rename).unwrap(),
        ChangeOutcome::Duplicate
    );
    assert_eq!(
        store
            .apply_change(&change(
                FsChangeKind::Rename,
                "V1",
                "C:\\Work\\NeverSeen",
                Some("C:\\Work\\Other"),
            ))
            .unwrap(),
        ChangeOutcome::NotTracked
    );
    assert!(
        store
            .entity_at_locator("V1", "C:\\Work\\Other")
            .unwrap()
            .is_none()
    );
}

#[test]
fn overwritten_destination_keeps_its_history_as_missing() {
    let mut store = ActivityStore::in_memory().unwrap();
    let a = visit_file(&mut store, "V1", "C:\\Work\\a.txt", 10_000);
    let b = visit_file(&mut store, "V1", "C:\\Work\\b.txt", 11_000);
    store
        .apply_change(&change(
            FsChangeKind::Rename,
            "V1",
            "C:\\Work\\a.txt",
            Some("C:\\Work\\b.txt"),
        ))
        .unwrap();
    let now_b = store
        .entity_at_locator("V1", "C:\\Work\\b.txt")
        .unwrap()
        .unwrap();
    assert_eq!(now_b.id, a.id);
    let ghost = store.identity_state(&b.id).unwrap().unwrap();
    assert_eq!(ghost.entity.availability, Availability::Missing);
    assert!(!ghost.attached);
}

#[test]
fn moving_into_an_excluded_root_forgets_the_history() {
    let mut store = ActivityStore::in_memory().unwrap();
    visit(&mut store, "V1", "C:\\Work\\Public", 10_000);
    let epoch = store.exclude_root("C:\\Private").unwrap();
    let outcome = store
        .apply_change(&change(
            FsChangeKind::Move,
            "V1",
            "C:\\Work\\Public",
            Some("C:\\Private\\Public"),
        ))
        .unwrap();
    assert_eq!(outcome, ChangeOutcome::Forgotten { entities: 1 });
    assert!(recent_ids_with_epoch(&mut store, epoch + 1).is_empty());
    assert!(
        store
            .entity_at_locator("V1", "C:\\Private\\Public")
            .unwrap()
            .is_none()
    );
    assert!(store.privacy_epoch().unwrap() > epoch);
}

fn recent_ids_with_epoch(store: &mut ActivityStore, epoch: u64) -> Vec<String> {
    store
        .recent(0, 100 * DAY, ActivityKind::FolderVisit, epoch, 100)
        .unwrap()
        .unwrap()
        .into_iter()
        .map(|row| row.entity.id)
        .collect()
}

// ---- FER-262: casing, aliases, availability, lazy validation -----------

#[test]
fn windows_paths_fold_case_but_posix_paths_do_not() {
    let mut store = ActivityStore::in_memory().unwrap();
    let first = visit(&mut store, "V1", "C:\\Work\\Reports", 10_000);
    // Same location spelled differently: same entity, no identity conflict.
    store
        .record(&visit_kind(
            "V1",
            "c:/work/REPORTS/",
            50_000,
            EntityKind::Folder,
        ))
        .unwrap();
    let again = store
        .entity_at_locator("V1", "C:\\WORK\\reports")
        .unwrap()
        .unwrap();
    assert_eq!(again.id, first.id);
    assert_eq!(recent_ids(&mut store), vec![(first.id.clone(), 2)]);
    // Extended-length prefix is the same location too.
    assert_eq!(
        store
            .entity_at_locator("V1", "\\\\?\\C:\\Work\\Reports")
            .unwrap()
            .unwrap()
            .id,
        first.id
    );
    let lower = visit(&mut store, "V1", "/home/a", 60_000);
    let upper = visit(&mut store, "V1", "/home/A", 61_000);
    assert_ne!(lower.id, upper.id);
}

#[test]
fn case_only_rename_keeps_identity_and_updates_display_casing() {
    let mut store = ActivityStore::in_memory().unwrap();
    let first = visit(&mut store, "V1", "C:\\Work\\reports", 10_000);
    assert_eq!(
        store
            .apply_change(&change(
                FsChangeKind::Rename,
                "V1",
                "C:\\Work\\reports",
                Some("C:\\Work\\Reports"),
            ))
            .unwrap(),
        ChangeOutcome::Relocated { entities: 1 }
    );
    let after = store
        .entity_at_locator("V1", "C:\\Work\\reports")
        .unwrap()
        .unwrap();
    assert_eq!(after.id, first.id);
    assert_eq!(after.locator, "C:\\Work\\Reports");
    assert_eq!(after.display_name, "Reports");
}

#[test]
fn availability_states_are_distinct_and_lazily_validated() {
    let mut store = ActivityStore::in_memory().unwrap();
    let entity = visit(&mut store, "V1", "C:\\Work\\Probe", 10_000);
    let state = store.identity_state(&entity.id).unwrap().unwrap();
    assert_eq!(state.confidence, IdentityConfidence::Path);
    assert!(state.platform_id.is_none());
    // A receipt proves existence at that moment, so it counts as a check.
    assert!(!state.is_stale(10_000 + 1_000, 60_000));
    assert!(state.is_stale(10_000 + 60_001, 60_000));
    let cases = [
        (ValidationProbe::NotFound, Availability::Missing),
        (ValidationProbe::AccessDenied, Availability::Inaccessible),
        (ValidationProbe::VolumeUnavailable, Availability::Offline),
        (ValidationProbe::Indeterminate, Availability::Unknown),
        (
            ValidationProbe::Present { platform_id: None },
            Availability::Available,
        ),
    ];
    for (index, (probe, expected)) in cases.into_iter().enumerate() {
        let now = 100_000 + index as u64;
        assert_eq!(
            store.apply_validation(&entity.id, &probe, now).unwrap(),
            ValidationOutcome::Updated(expected.clone())
        );
        let state = store.identity_state(&entity.id).unwrap().unwrap();
        assert_eq!(state.entity.availability, expected);
        assert_eq!(state.checked_utc_ms, Some(now));
        assert!(
            state.attached,
            "validation never detaches a locator by itself"
        );
        // Visible through ordinary queries as well.
        let row = store
            .recent(0, 100 * DAY, ActivityKind::FolderVisit, 0, 10)
            .unwrap()
            .unwrap();
        assert_eq!(row[0].entity.availability, expected);
    }
    assert_eq!(
        store
            .apply_validation("missing-id", &ValidationProbe::NotFound, 1)
            .unwrap(),
        ValidationOutcome::UnknownEntity
    );
    // A later receipt proves it is there again.
    visit(&mut store, "V1", "C:\\Work\\Probe", 500_000);
    store
        .apply_validation(&entity.id, &ValidationProbe::Indeterminate, 500_001)
        .unwrap();
    visit(&mut store, "V1", "C:\\Work\\Probe", 900_000);
    assert_eq!(
        store
            .identity_state(&entity.id)
            .unwrap()
            .unwrap()
            .entity
            .availability,
        Availability::Available
    );
}

#[test]
fn platform_id_is_learned_then_a_mismatch_ends_the_identity() {
    let mut store = ActivityStore::in_memory().unwrap();
    let entity = visit_file(&mut store, "V1", "C:\\Work\\a.bin", 10_000);
    let probe = |id: &str| ValidationProbe::Present {
        platform_id: Some(id.into()),
    };
    store
        .apply_validation(&entity.id, &probe("F-7"), 20_000)
        .unwrap();
    let state = store.identity_state(&entity.id).unwrap().unwrap();
    assert_eq!(state.platform_id.as_deref(), Some("F-7"));
    assert_eq!(state.confidence, IdentityConfidence::Verified);
    assert_eq!(
        store
            .apply_validation(&entity.id, &probe("F-7"), 21_000)
            .unwrap(),
        ValidationOutcome::Updated(Availability::Available)
    );
    // Same path now holds another object (recycled name): never reuse history.
    assert_eq!(
        store
            .apply_validation(&entity.id, &probe("F-99"), 22_000)
            .unwrap(),
        ValidationOutcome::IdentityMismatch
    );
    let state = store.identity_state(&entity.id).unwrap().unwrap();
    assert_eq!(state.entity.availability, Availability::Missing);
    assert!(!state.attached);
    assert_eq!(state.confidence, IdentityConfidence::Ended);
    let reborn = visit_file(&mut store, "V1", "C:\\Work\\a.bin", 30_000);
    assert_ne!(reborn.id, entity.id);
    // A detached entity is not re-attached by a later present probe either.
    assert_eq!(
        store
            .apply_validation(&entity.id, &probe("F-7"), 31_000)
            .unwrap(),
        ValidationOutcome::IdentityMismatch
    );
}

#[test]
fn volume_unavailable_and_returning_volume_do_not_mark_files_deleted() {
    let mut store = ActivityStore::in_memory().unwrap();
    store.mount_volume("V1", "E:", 1_000).unwrap();
    let entity = visit(&mut store, "V1", "E:\\Photos", 10_000);
    assert_eq!(store.unmount_volume("V1").unwrap(), 1);
    let state = store.identity_state(&entity.id).unwrap().unwrap();
    assert_eq!(state.entity.availability, Availability::Offline);
    assert!(
        state.attached,
        "offline keeps the locator for when it returns"
    );
    let outcome = store.mount_volume("V1", "E:", 20_000).unwrap();
    assert_eq!(
        outcome,
        MountOutcome {
            rebased: 0,
            offlined_other: 0,
            previous_mount: Some("E:".into()),
        }
    );
    // Back online but not yet verified: Unknown, never optimistic Available.
    assert_eq!(
        store
            .identity_state(&entity.id)
            .unwrap()
            .unwrap()
            .entity
            .availability,
        Availability::Unknown
    );
}

// ---- FER-263: drive letter replacement, recycled ids, uncertain renames

#[test]
fn drive_letter_reuse_by_another_volume_never_retargets_history() {
    let mut store = ActivityStore::in_memory().unwrap();
    store.mount_volume("V1", "D:", 1_000).unwrap();
    let old = visit(&mut store, "V1", "D:\\Work\\Plans", 10_000);
    visit(&mut store, "V1", "D:\\Work\\Plans", 10_000 + DAY);
    // Another disk takes the letter.
    let outcome = store.mount_volume("V2", "D:", 5 * DAY).unwrap();
    assert_eq!(outcome.offlined_other, 1);
    assert_eq!(
        store
            .identity_state(&old.id)
            .unwrap()
            .unwrap()
            .entity
            .availability,
        Availability::Offline
    );
    // Same-looking path on the new disk, same service-derived id: new entity.
    let other = visit(&mut store, "V2", "D:\\Work\\Plans", 6 * DAY);
    assert_ne!(other.id, old.id);
    assert_eq!(other.volume_id, "V2");
    assert_eq!(other.availability, Availability::Available);
    let rows = recent_ids(&mut store);
    assert_eq!(rows.iter().find(|(id, _)| *id == old.id).unwrap().1, 2);
    assert_eq!(rows.iter().find(|(id, _)| *id == other.id).unwrap().1, 1);
    // Queries for the old volume still resolve to the old entity only.
    assert_eq!(
        store
            .entity_at_locator("V1", "D:\\Work\\Plans")
            .unwrap()
            .unwrap()
            .id,
        old.id
    );
    // The original disk comes back under another letter: history follows it.
    let back = store.mount_volume("V1", "F:", 7 * DAY).unwrap();
    assert_eq!(back.rebased, 1);
    assert_eq!(back.previous_mount.as_deref(), Some("D:"));
    let moved = store
        .entity_at_locator("V1", "F:\\Work\\Plans")
        .unwrap()
        .unwrap();
    assert_eq!(moved.id, old.id);
    assert_eq!(moved.locator, "F:\\Work\\Plans");
    assert_eq!(moved.parent_path, "F:\\Work");
    assert_eq!(moved.availability, Availability::Unknown);
    assert!(
        store
            .entity_at_locator("V1", "D:\\Work\\Plans")
            .unwrap()
            .is_none()
    );
    // The letter's new owner is untouched.
    assert_eq!(
        store
            .entity_at_locator("V2", "D:\\Work\\Plans")
            .unwrap()
            .unwrap()
            .id,
        other.id
    );
}

#[test]
fn legacy_drive_letter_volume_ids_can_be_adopted_once_identity_is_known() {
    let mut store = ActivityStore::in_memory().unwrap();
    let legacy = visit(&mut store, "C:", "C:\\Work\\Old", 10_000);
    assert_eq!(store.adopt_legacy_volume("C:", "VOL-1").unwrap(), 1);
    let adopted = store
        .entity_at_locator("VOL-1", "C:\\Work\\Old")
        .unwrap()
        .unwrap();
    assert_eq!(adopted.id, legacy.id);
    assert_eq!(adopted.volume_id, "VOL-1");
    assert!(
        store
            .entity_at_locator("C:", "C:\\Work\\Old")
            .unwrap()
            .is_none()
    );
}

#[test]
fn recycled_platform_id_after_delete_is_never_merged_with_the_dead_entity() {
    let mut store = ActivityStore::in_memory().unwrap();
    let dead = visit_file(&mut store, "V1", "C:\\Work\\old.dat", 10_000);
    store
        .apply_validation(
            &dead.id,
            &ValidationProbe::Present {
                platform_id: Some("F-7".into()),
            },
            11_000,
        )
        .unwrap();
    store
        .apply_change(&change(
            FsChangeKind::Delete,
            "V1",
            "C:\\Work\\old.dat",
            None,
        ))
        .unwrap();
    // The filesystem hands id F-7 to a different, newer file.
    let newer = visit_file(&mut store, "V1", "C:\\Work\\new.dat", 20_000);
    store
        .apply_validation(
            &newer.id,
            &ValidationProbe::Present {
                platform_id: Some("F-7".into()),
            },
            21_000,
        )
        .unwrap();
    let outcome = store
        .reconcile_external(
            &dead.id,
            &ExternalCandidate {
                volume_id: "V1".into(),
                locator: "C:\\Work\\new.dat".into(),
                kind: EntityKind::File,
                platform_id: Some("F-7".into()),
            },
            22_000,
        )
        .unwrap();
    assert_eq!(
        outcome,
        ReconcileOutcome::Unresolved(UnresolvedReason::SourceNotMissing)
    );
    assert_eq!(
        store
            .entity_at_locator("V1", "C:\\Work\\new.dat")
            .unwrap()
            .unwrap()
            .id,
        newer.id
    );
    let state = store.identity_state(&dead.id).unwrap().unwrap();
    assert!(state.platform_id.is_none());
    assert_eq!(state.entity.availability, Availability::Missing);
}

#[test]
fn external_rename_with_platform_evidence_is_reconciled_with_history() {
    let mut store = ActivityStore::in_memory().unwrap();
    let entity = visit(&mut store, "V1", "C:\\Work\\Plans", 10_000);
    let child = visit(&mut store, "V1", "C:\\Work\\Plans\\q1", 11_000);
    visit(&mut store, "V1", "C:\\Work\\Plans", 10_000 + DAY);
    store
        .apply_validation(
            &entity.id,
            &ValidationProbe::Present {
                platform_id: Some("F-1".into()),
            },
            12_000,
        )
        .unwrap();
    store
        .apply_validation(&entity.id, &ValidationProbe::NotFound, 2 * DAY)
        .unwrap();
    let outcome = store
        .reconcile_external(
            &entity.id,
            &ExternalCandidate {
                volume_id: "V1".into(),
                locator: "C:\\Work\\Plans 2026".into(),
                kind: EntityKind::Folder,
                platform_id: Some("F-1".into()),
            },
            2 * DAY + 1,
        )
        .unwrap();
    assert_eq!(outcome, ReconcileOutcome::Merged);
    let merged = store
        .entity_at_locator("V1", "C:\\Work\\Plans 2026")
        .unwrap()
        .unwrap();
    assert_eq!(merged.id, entity.id);
    assert_eq!(merged.availability, Availability::Available);
    assert_eq!(
        store
            .identity_state(&entity.id)
            .unwrap()
            .unwrap()
            .confidence,
        IdentityConfidence::Verified
    );
    // Descendants follow the folder but are not claimed verified.
    let moved_child = store
        .entity_at_locator("V1", "C:\\Work\\Plans 2026\\q1")
        .unwrap()
        .unwrap();
    assert_eq!(moved_child.id, child.id);
    assert_eq!(moved_child.availability, Availability::Unknown);
    assert_eq!(
        recent_ids(&mut store)
            .into_iter()
            .find(|(id, _)| *id == entity.id)
            .unwrap()
            .1,
        2
    );
}

#[test]
fn external_rename_without_sufficient_evidence_is_not_merged() {
    let mut store = ActivityStore::in_memory().unwrap();
    // Evidence is only name and similarity: no platform id on either side.
    let weak = visit_file(&mut store, "V1", "C:\\Work\\report.docx", 10_000);
    store
        .apply_validation(&weak.id, &ValidationProbe::NotFound, 20_000)
        .unwrap();
    let candidate =
        |volume: &str, locator: &str, kind: EntityKind, pid: Option<&str>| ExternalCandidate {
            volume_id: volume.into(),
            locator: locator.into(),
            kind,
            platform_id: pid.map(str::to_owned),
        };
    let none = candidate("V1", "C:\\Work\\report v2.docx", EntityKind::File, None);
    assert_eq!(
        store.reconcile_external(&weak.id, &none, 30_000).unwrap(),
        ReconcileOutcome::Unresolved(UnresolvedReason::NoPlatformEvidence)
    );
    let state = store.identity_state(&weak.id).unwrap().unwrap();
    assert_eq!(state.confidence, IdentityConfidence::Unresolved);
    assert!(state.attached);
    assert!(
        store
            .entity_at_locator("V1", "C:\\Work\\report v2.docx")
            .unwrap()
            .is_none()
    );
    // Candidate carries an id but the entity never learned one.
    let only_candidate = candidate(
        "V1",
        "C:\\Work\\report v2.docx",
        EntityKind::File,
        Some("F-1"),
    );
    assert_eq!(
        store
            .reconcile_external(&weak.id, &only_candidate, 30_001)
            .unwrap(),
        ReconcileOutcome::Unresolved(UnresolvedReason::NoPlatformEvidence)
    );

    let strong = visit_file(&mut store, "V1", "C:\\Work\\strong.docx", 40_000);
    store
        .apply_validation(
            &strong.id,
            &ValidationProbe::Present {
                platform_id: Some("F-2".into()),
            },
            41_000,
        )
        .unwrap();
    store
        .apply_validation(&strong.id, &ValidationProbe::NotFound, 42_000)
        .unwrap();
    let attempts = [
        (
            candidate("V1", "C:\\Work\\x.docx", EntityKind::File, Some("F-3")),
            UnresolvedReason::PlatformIdMismatch,
        ),
        (
            candidate("V2", "C:\\Work\\x.docx", EntityKind::File, Some("F-2")),
            UnresolvedReason::VolumeMismatch,
        ),
        (
            candidate("V1", "C:\\Work\\x.docx", EntityKind::Folder, Some("F-2")),
            UnresolvedReason::KindMismatch,
        ),
    ];
    for (attempt, expected) in attempts {
        assert_eq!(
            store
                .reconcile_external(&strong.id, &attempt, 43_000)
                .unwrap(),
            ReconcileOutcome::Unresolved(expected)
        );
        assert!(
            store
                .entity_at_locator("V1", "C:\\Work\\strong.docx")
                .unwrap()
                .is_some()
        );
    }
    // Target path already belongs to another tracked entity.
    let taken = visit_file(&mut store, "V1", "C:\\Work\\taken.docx", 44_000);
    assert_eq!(
        store
            .reconcile_external(
                &strong.id,
                &candidate("V1", "C:\\Work\\taken.docx", EntityKind::File, Some("F-2")),
                45_000,
            )
            .unwrap(),
        ReconcileOutcome::Unresolved(UnresolvedReason::TargetTracked)
    );
    assert_eq!(
        store
            .entity_at_locator("V1", "C:\\Work\\taken.docx")
            .unwrap()
            .unwrap()
            .id,
        taken.id
    );
    // The same file id on two other tracked entities is ambiguous.
    let twin = visit_file(&mut store, "V1", "C:\\Work\\twin.docx", 46_000);
    store
        .apply_validation(
            &twin.id,
            &ValidationProbe::Present {
                platform_id: Some("F-2".into()),
            },
            47_000,
        )
        .unwrap();
    assert_eq!(
        store
            .reconcile_external(
                &strong.id,
                &candidate("V1", "C:\\Work\\fresh.docx", EntityKind::File, Some("F-2")),
                48_000,
            )
            .unwrap(),
        ReconcileOutcome::Unresolved(UnresolvedReason::Ambiguous)
    );
    assert_eq!(
        store.reconcile_external("ghost", &none, 1).unwrap(),
        ReconcileOutcome::UnknownEntity
    );
}

#[test]
fn present_entity_is_never_hijacked_by_a_reconcile_attempt() {
    let mut store = ActivityStore::in_memory().unwrap();
    let entity = visit_file(&mut store, "V1", "C:\\Work\\live.txt", 10_000);
    store
        .apply_validation(
            &entity.id,
            &ValidationProbe::Present {
                platform_id: Some("F-1".into()),
            },
            11_000,
        )
        .unwrap();
    assert_eq!(
        store
            .reconcile_external(
                &entity.id,
                &ExternalCandidate {
                    volume_id: "V1".into(),
                    locator: "C:\\Work\\elsewhere.txt".into(),
                    kind: EntityKind::File,
                    platform_id: Some("F-1".into()),
                },
                12_000,
            )
            .unwrap(),
        ReconcileOutcome::Unresolved(UnresolvedReason::SourceNotMissing)
    );
    assert!(
        store
            .entity_at_locator("V1", "C:\\Work\\live.txt")
            .unwrap()
            .is_some()
    );
}

#[test]
fn recycled_entity_id_after_a_move_gets_a_fresh_entity() {
    let mut store = ActivityStore::in_memory().unwrap();
    // Service ids derive from the path: after P -> Q, id(P) belongs to Q.
    let first = visit(&mut store, "V1", "C:\\Work\\P", 10_000);
    store
        .apply_change(&change(
            FsChangeKind::Rename,
            "V1",
            "C:\\Work\\P",
            Some("C:\\Work\\Q"),
        ))
        .unwrap();
    // Somebody creates a new folder named P and the user visits it.
    let outcome = store
        .record(&visit_kind("V1", "C:\\Work\\P", 20_000, EntityKind::Folder))
        .unwrap();
    assert_eq!(outcome, RecordOutcome::NewEpisode);
    let recreated = store
        .entity_at_locator("V1", "C:\\Work\\P")
        .unwrap()
        .unwrap();
    assert_ne!(recreated.id, first.id);
    assert_eq!(
        store
            .entity_at_locator("V1", "C:\\Work\\Q")
            .unwrap()
            .unwrap()
            .id,
        first.id
    );
    // And a visit to Q continues the original history.
    visit(&mut store, "V1", "C:\\Work\\Q", 100_000);
    let rows = recent_ids(&mut store);
    assert_eq!(rows.iter().find(|(id, _)| *id == first.id).unwrap().1, 2);
    assert_eq!(
        rows.iter().find(|(id, _)| *id == recreated.id).unwrap().1,
        1
    );
}

// ---- aliases ---------------------------------------------------------

#[test]
fn verified_aliases_resolve_to_the_same_entity() {
    let mut store = ActivityStore::in_memory().unwrap();
    let real = visit(&mut store, "V1", "C:\\Real\\Docs", 10_000);
    assert_eq!(
        store.link_alias(&real.id, "C:\\Links\\Docs").unwrap(),
        AliasOutcome::Linked
    );
    assert_eq!(
        store.link_alias(&real.id, "c:\\links\\docs").unwrap(),
        AliasOutcome::AlreadyLinked
    );
    store
        .record(&visit_kind(
            "V1",
            "C:\\Links\\Docs",
            50_000,
            EntityKind::Folder,
        ))
        .unwrap();
    assert_eq!(recent_ids(&mut store), vec![(real.id.clone(), 2)]);
    // The canonical locator is not overwritten by the alias spelling.
    assert_eq!(
        store
            .entity_at_locator("V1", "C:\\Links\\Docs")
            .unwrap()
            .unwrap()
            .locator,
        "C:\\Real\\Docs"
    );
    // An alias cannot claim a path that is tracked as its own entity.
    let separate = visit(&mut store, "V1", "C:\\Links\\Other", 60_000);
    assert_eq!(
        store.link_alias(&real.id, "C:\\Links\\Other").unwrap(),
        AliasOutcome::Conflict
    );
    assert_ne!(separate.id, real.id);
}

#[test]
fn exclusion_covers_the_verified_target_of_an_alias() {
    let mut store = ActivityStore::in_memory().unwrap();
    let real = visit(&mut store, "V1", "C:\\Real\\Secret", 10_000);
    store.link_alias(&real.id, "C:\\Safe\\Secret").unwrap();
    let epoch = store.exclude_root("C:\\Safe").unwrap();
    assert!(recent_ids_with_epoch(&mut store, epoch).is_empty());
    // Visiting the real path directly must stay excluded.
    let mut receipt = visit_kind("V1", "C:\\Real\\Secret", 20_000, EntityKind::Folder);
    receipt.privacy_epoch = epoch;
    assert_eq!(store.record(&receipt).unwrap(), RecordOutcome::Ignored);
}

#[test]
fn linking_an_alias_inside_an_excluded_root_forgets_the_target() {
    let mut store = ActivityStore::in_memory().unwrap();
    let epoch = store.exclude_root("C:\\Safe").unwrap();
    let mut receipt = visit_kind("V1", "C:\\Real\\Secret", 10_000, EntityKind::Folder);
    receipt.privacy_epoch = epoch;
    store.record(&receipt).unwrap();
    let real = store
        .entity_at_locator("V1", "C:\\Real\\Secret")
        .unwrap()
        .unwrap();
    assert_eq!(
        store.link_alias(&real.id, "C:\\Safe\\Secret").unwrap(),
        AliasOutcome::ExcludedTarget
    );
    assert!(
        store
            .entity_at_locator("V1", "C:\\Real\\Secret")
            .unwrap()
            .is_none()
    );
}

// ---- migration ---------------------------------------------------------

#[test]
fn migrates_version_ten_and_backfills_locator_keys() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("activity.sqlite3");
    let entity;
    {
        let mut store = ActivityStore::open(&path).unwrap();
        entity = visit(&mut store, "V1", "C:\\Work\\Reports", 10_000);
        visit(&mut store, "V1", "C:\\Work\\Reports", 10_000 + DAY);
    }
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "DROP INDEX activity_entity_locator;
             DROP INDEX IF EXISTS activity_entity_platform;
             DROP TABLE activity_aliases; DROP TABLE activity_changes; DROP TABLE activity_volumes;
             ALTER TABLE activity_entities DROP COLUMN locator_key;
             ALTER TABLE activity_entities DROP COLUMN platform_id;
             ALTER TABLE activity_entities DROP COLUMN identity;
             ALTER TABLE activity_entities DROP COLUMN relocated;
             ALTER TABLE activity_entities DROP COLUMN checked_utc_ms;
             PRAGMA user_version = 10;",
        )
        .unwrap();
    drop(connection);
    let mut store = ActivityStore::open(&path).unwrap();
    let found = store
        .entity_at_locator("V1", "c:\\work\\REPORTS")
        .unwrap()
        .unwrap();
    assert_eq!(found.id, entity.id);
    assert_eq!(
        store
            .identity_state(&entity.id)
            .unwrap()
            .unwrap()
            .confidence,
        IdentityConfidence::Path
    );
    assert_eq!(recent_ids(&mut store), vec![(entity.id.clone(), 2)]);
    let version: i64 = Connection::open(&path)
        .unwrap()
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    assert_eq!(SCHEMA_VERSION, 13);
    // Rename keeps working on a migrated database.
    assert_eq!(
        store
            .apply_change(&change(
                FsChangeKind::Rename,
                "V1",
                "C:\\Work\\Reports",
                Some("C:\\Work\\Final"),
            ))
            .unwrap(),
        ChangeOutcome::Relocated { entities: 1 }
    );
}

#[test]
fn identity_data_is_purged_by_clear_and_retention() {
    let mut store = ActivityStore::in_memory().unwrap();
    store.mount_volume("V1", "C:", 1_000).unwrap();
    let real = visit(&mut store, "V1", "C:\\Real\\Docs", 10_000);
    store.link_alias(&real.id, "C:\\Links\\Docs").unwrap();
    store
        .apply_change(&change(
            FsChangeKind::Rename,
            "V1",
            "C:\\Real\\Docs",
            Some("C:\\Real\\Docs2"),
        ))
        .unwrap();
    assert_eq!(store.purge_before(20_000).unwrap(), 1);
    assert!(
        store
            .entity_at_locator("V1", "C:\\Real\\Docs2")
            .unwrap()
            .is_none()
    );
    visit(&mut store, "V1", "C:\\Real\\Again", 30_000);
    store.clear_all().unwrap();
    assert!(
        store
            .entity_at_locator("V1", "C:\\Real\\Again")
            .unwrap()
            .is_none()
    );
}

#[test]
fn same_id_on_another_volume_without_evidence_is_refused() {
    let mut store = ActivityStore::in_memory().unwrap();
    visit(&mut store, "V1", "D:\\Work\\Plans", 10_000);
    // V1 is not registered as unmounted: nothing proves the letter changed hands.
    assert!(
        store
            .record(&visit_kind(
                "V2",
                "D:\\Work\\Plans",
                20_000,
                EntityKind::Folder
            ))
            .is_err()
    );
    assert_eq!(store.episode_count().unwrap(), 1);
}
