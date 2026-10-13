#[path = "benchmark.rs"]
mod benchmark;
#[allow(dead_code)]
#[path = "matcher.rs"]
mod matcher;
#[allow(dead_code)]
#[path = "ranking.rs"]
mod ranking;

use benchmark::{generate, DAY_MS, FIXED_NOW_UTC_MS};
use ranking::{frequent, recent, AccessKind, Episode};
use std::time::Instant;

fn replay(fixture: &benchmark::Fixture) -> Vec<Episode<'_>> {
    fixture
        .episodes
        .iter()
        .map(|episode| Episode {
            entity_id: &fixture.entities[episode.entity_index].id,
            kind: AccessKind::FolderVisit,
            last_utc_ms: episode.last_utc_ms,
        })
        .collect()
}

#[test]
fn fixture_replay_is_deterministic_and_uses_only_synthetic_paths() {
    let first = generate(16, 100, 42);
    let second = generate(16, 100, 42);
    assert_eq!(first, second);
    assert_ne!(first, generate(16, 100, 43));
    assert!(first
        .entities
        .iter()
        .all(|entity| entity.path.starts_with("/fixture/")));
    assert!(first.episodes.iter().all(|episode| {
        episode.entity_index < first.entities.len()
            && episode.last_utc_ms <= FIXED_NOW_UTC_MS
            && episode.last_utc_ms >= FIXED_NOW_UTC_MS - 90 * DAY_MS
    }));
}

#[test]
fn replay_ranking_and_typo_recall_do_not_depend_on_recent_candidate_subset() {
    let fixture = generate(32, 320, 7);
    let episodes = replay(&fixture);
    let recent = recent(&episodes, 0, FIXED_NOW_UTC_MS + 1, AccessKind::FolderVisit);
    let frequent = frequent(&episodes, 0, FIXED_NOW_UTC_MS, AccessKind::FolderVisit);
    assert_eq!(recent.len(), 32);
    assert!(!frequent.is_empty());
    let target = &fixture.entities[0];
    assert!(matcher::match_candidate("proejct", &target.name, &target.path, 0).is_some());
    assert!(matcher::match_candidate("inv2026", &fixture.entities[2].name, "", 0).is_some());
    let exact = matcher::match_candidate("project", &target.name, &target.path, 0).unwrap();
    let fuzzy = matcher::match_candidate("project", "proejct", "", u32::MAX).unwrap();
    assert_eq!(
        matcher::compare_matches(&exact, "a", &fuzzy, "b"),
        std::cmp::Ordering::Less
    );
}

#[test]
#[ignore = "manual in-memory helper timing; not SQLite or desktop acceptance"]
fn synthetic_100k_entities_1m_episodes_helper_timing() {
    let start = Instant::now();
    let fixture = generate(100_000, 1_000_000, 17);
    let generated = start.elapsed();
    let episodes = replay(&fixture);
    let start = Instant::now();
    let recent_rows = recent(&episodes, 0, FIXED_NOW_UTC_MS + 1, AccessKind::FolderVisit);
    let recent_elapsed = start.elapsed();
    let start = Instant::now();
    let frequent_rows = frequent(&episodes, 0, FIXED_NOW_UTC_MS, AccessKind::FolderVisit);
    let frequent_elapsed = start.elapsed();
    assert_eq!(recent_rows.len(), 100_000);
    assert!(!frequent_rows.is_empty());
    println!(
        "in-memory helper only: generate={generated:?}, recent={recent_elapsed:?}, frequent={frequent_elapsed:?}; rows={} / {}",
        recent_rows.len(),
        frequent_rows.len()
    );
}
