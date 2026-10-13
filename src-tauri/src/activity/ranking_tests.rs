#[path = "ranking.rs"]
mod ranking;

use ranking::{distinct_entities, frequent, page, recent, AccessKind, Episode, RANKING_VERSION};

const DAY: i64 = 86_400_000;

fn visit(entity_id: &str, day: i64, offset: i64) -> Episode<'_> {
    Episode {
        entity_id,
        kind: AccessKind::FolderVisit,
        last_utc_ms: day * DAY + offset,
    }
}

#[test]
fn recent_uses_kind_and_half_open_utc_bounds() {
    let mut episodes = vec![visit("b", -1, DAY - 1), visit("b", 0, 0), visit("a", 0, 0)];
    episodes.push(Episode {
        entity_id: "ignored-file",
        kind: AccessKind::FileOpenRequested,
        last_utc_ms: 10,
    });
    episodes.push(visit("outside", 1, 0));
    let rows = recent(&episodes, 0, DAY, AccessKind::FolderVisit);
    assert_eq!(
        rows.iter()
            .map(|row| row.entity_id.as_str())
            .collect::<Vec<_>>(),
        ["a", "b"]
    );
    assert_eq!(rows[0].episode_count, 1);
    assert_eq!(rows[1].episode_count, 1);
    assert_eq!(rows[0].ranking_version, RANKING_VERSION);
    assert!(recent(&episodes, DAY, 0, AccessKind::FolderVisit).is_empty());
    let files = recent(&episodes, 0, DAY, AccessKind::FileOpenRequested);
    assert_eq!(files[0].entity_id, "ignored-file");
}

#[test]
fn recent_filters_events_before_choosing_last_access() {
    let episodes = [
        visit("older", 1, 1),
        visit("newer", 2, 1),
        visit("older", 3, 1),
    ];
    let rows = recent(&episodes, DAY, 3 * DAY, AccessKind::FolderVisit);
    assert_eq!(rows[0].entity_id, "newer");
    assert_eq!(rows[1].last_utc_ms, DAY + 1);
}

#[test]
fn frequent_caps_five_per_day_but_preserves_raw_counts() {
    let mut episodes: Vec<_> = (0..20).map(|i| visit("busy", 0, i * 31_000)).collect();
    episodes.push(visit("busy", 1, 2));
    episodes.extend([visit("steady", 0, 1), visit("steady", 1, 2)]);
    episodes.push(visit("single", 1, 4));
    let rows = frequent(&episodes, 0, DAY + 20, AccessKind::FolderVisit);
    assert_eq!(
        rows.iter()
            .map(|row| row.entity_id.as_str())
            .collect::<Vec<_>>(),
        ["busy", "steady"]
    );
    assert_eq!(rows[0].episode_count, 21);
    assert_eq!(rows[0].active_days, 2);
    let expected = 5.0 * 2.0_f64.powf(-1.0 / 14.0) + 1.0;
    assert!((rows[0].score - expected).abs() < 1e-10);
}

#[test]
fn frequent_half_life_favors_regular_new_activity() {
    let mut episodes = vec![visit("old", -42, 1), visit("old", -41, 1)];
    for i in 1..5 {
        episodes.push(visit("old", -42, 1 + i * 31_000));
        episodes.push(visit("old", -41, 1 + i * 31_000));
    }
    episodes.extend([visit("regular", -1, 1), visit("regular", 0, 1)]);
    let rows = frequent(&episodes, -45 * DAY, 10, AccessKind::FolderVisit);
    assert_eq!(rows[0].entity_id, "regular");
    assert_eq!(rows[1].entity_id, "old");
}

#[test]
fn utc_bucket_boundaries_and_future_episodes() {
    let episodes = [
        visit("boundary", -1, DAY - 1),
        visit("boundary", 0, 0),
        visit("future", 0, 1),
        visit("future", 1, 0),
    ];
    let rows = frequent(&episodes, -DAY, 1, AccessKind::FolderVisit);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].entity_id, "boundary");
    assert_eq!(rows[0].active_days, 2);
}

#[test]
fn frequent_ties_and_paging_stay_stable_for_same_snapshot() {
    let episodes = [
        visit("z", 0, 0),
        visit("z", 1, 0),
        visit("a", 0, 0),
        visit("a", 1, 0),
    ];
    let rows = frequent(&episodes, 0, DAY + 1, AccessKind::FolderVisit);
    assert!(distinct_entities(&rows));
    assert_eq!(page(&rows, None, 1)[0].entity_id, "a");
    assert_eq!(page(&rows, Some("a"), 1)[0].entity_id, "z");
    assert!(page(&rows, Some("gone"), 1).is_empty());
    assert!(page(&rows, None, 0).is_empty());
    assert!(page(&rows, Some("z"), 1).is_empty());
}
