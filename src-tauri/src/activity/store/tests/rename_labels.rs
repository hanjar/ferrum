//! Rename/move must refresh every label the sidebar reads: recent()/frequent()
//! rows (display_name, parent_path, locator) and the search index.

use ferrum_activity_contract::{
    ActivityEntity, ActivityKind, ActivityReceipt, Availability, EntityKind, Provenance,
    ReceiptSource,
};
use ferrum_activity_store::{ActivityStore, ChangeOutcome, FsChange, FsChangeKind};

const DAY: u64 = 86_400_000;

fn receipt(n: u64, path: &str, parent: &str, name: &str, ts: u64) -> ActivityReceipt {
    ActivityReceipt {
        version: 1,
        event_id: format!("e{n}"),
        action_id: format!("a{n}"),
        privacy_epoch: 0,
        source: ReceiptSource::NavigationCommit,
        kind: ActivityKind::FolderVisit,
        provenance: Provenance::Manual,
        entity: ActivityEntity {
            id: format!("id:{}", path.to_lowercase()),
            kind: EntityKind::Folder,
            provider: "local".into(),
            volume_id: "V1".into(),
            locator: path.into(),
            parent_path: parent.into(),
            display_name: name.into(),
            availability: Availability::Available,
        },
        occurred_utc_ms: ts,
    }
}

fn rename(n: u64, from: &str, to: &str, kind: FsChangeKind) -> FsChange {
    FsChange {
        action_id: format!("c{n}"),
        kind,
        from_volume_id: "V1".into(),
        from: from.into(),
        to_volume_id: Some("V1".into()),
        to: Some(to.into()),
        occurred_utc_ms: 5_000,
    }
}

fn search(store: &mut ActivityStore, q: &str) -> Vec<ActivityEntity> {
    store
        .search_candidates(q, ActivityKind::FolderVisit, 0, 20)
        .unwrap()
        .unwrap()
}

#[test]
fn rename_refreshes_recent_frequent_labels_and_search_index() {
    let mut store = ActivityStore::in_memory().unwrap();
    store
        .record(&receipt(
            1,
            "C:\\Work\\Alphaproject",
            "C:\\Work",
            "Alphaproject",
            DAY + 1,
        ))
        .unwrap();
    store
        .record(&receipt(
            2,
            "C:\\Work\\Alphaproject",
            "C:\\Work",
            "Alphaproject",
            2 * DAY + 1,
        ))
        .unwrap();
    assert_eq!(search(&mut store, "alphaproject").len(), 1);

    let outcome = store
        .apply_change(&rename(
            1,
            "C:\\Work\\Alphaproject",
            "C:\\Archive\\Zulu",
            FsChangeKind::Move,
        ))
        .unwrap();
    assert_eq!(outcome, ChangeOutcome::Relocated { entities: 1 });

    let recent = store
        .recent(0, 100 * DAY, ActivityKind::FolderVisit, 0, 10)
        .unwrap()
        .unwrap();
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0].entity.display_name, "Zulu");
    assert_eq!(recent[0].entity.parent_path, "C:\\Archive");
    assert_eq!(recent[0].entity.locator, "C:\\Archive\\Zulu");
    assert_eq!(recent[0].episode_count, 2);

    let frequent = store
        .frequent(0, 3 * DAY, ActivityKind::FolderVisit, 0, 10)
        .unwrap()
        .unwrap();
    assert_eq!(frequent.len(), 1);
    assert_eq!(frequent[0].entity.display_name, "Zulu");
    assert_eq!(frequent[0].entity.parent_path, "C:\\Archive");

    let hits = search(&mut store, "zulu");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].display_name, "Zulu");
    assert!(search(&mut store, "alphaproject").is_empty());
    assert!(search(&mut store, "alpha").is_empty());
    // Candidates are gram-based: a surviving old name would still be returned.
    assert!(
        search(&mut store, "alphaproject")
            .iter()
            .all(|e| e.display_name != "Alphaproject")
    );
}

#[test]
fn renaming_a_parent_updates_descendant_parent_paths_and_keeps_their_names() {
    let mut store = ActivityStore::in_memory().unwrap();
    store
        .record(&receipt(1, "C:\\Work\\Old", "C:\\Work", "Old", DAY + 1))
        .unwrap();
    store
        .record(&receipt(
            2,
            "C:\\Work\\Old\\Sub",
            "C:\\Work\\Old",
            "Sub",
            DAY + 2,
        ))
        .unwrap();
    store
        .apply_change(&rename(
            1,
            "C:\\Work\\Old",
            "C:\\Work\\New",
            FsChangeKind::Rename,
        ))
        .unwrap();
    let recent = store
        .recent(0, 100 * DAY, ActivityKind::FolderVisit, 0, 10)
        .unwrap()
        .unwrap();
    let mut got: Vec<(String, String, String)> = recent
        .into_iter()
        .map(|r| {
            (
                r.entity.display_name,
                r.entity.parent_path,
                r.entity.locator,
            )
        })
        .collect();
    got.sort();
    assert_eq!(
        got,
        vec![
            ("New".into(), "C:\\Work".into(), "C:\\Work\\New".into()),
            (
                "Sub".into(),
                "C:\\Work\\New".into(),
                "C:\\Work\\New\\Sub".into()
            ),
        ]
    );
    assert_eq!(search(&mut store, "sub").len(), 1);
    assert_eq!(search(&mut store, "new").len(), 1);
    assert!(search(&mut store, "old").is_empty());
}
