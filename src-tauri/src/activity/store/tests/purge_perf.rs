//! `purge_before` correctness (small data, every derived table consistent) and
//! a 100k entity / 1M episode performance test (`#[ignore]`).
//! Perf run: cargo test --release --offline -j 2 --test purge_perf -- --ignored --nocapture
//! Size can be scaled down with FERRUM_PURGE_ENTITIES (default 100000).

use ferrum_activity_contract::{
    ActivityEntity, ActivityKind, ActivityReceipt, Availability, EntityKind, Provenance,
    ReceiptSource,
};
use ferrum_activity_store::{ActivityStore, rebuild_rank};
use rusqlite::{Connection, params};

const DAY: u64 = 86_400_000;

fn receipt(id: &str, name: &str, timestamp: u64, epoch: u64) -> ActivityReceipt {
    ActivityReceipt {
        version: 1,
        event_id: format!("{id}-{timestamp}"),
        action_id: format!("{id}-{timestamp}"),
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
            display_name: name.into(),
            availability: Availability::Available,
        },
        occurred_utc_ms: timestamp,
    }
}

fn rows(connection: &Connection, sql: &str) -> Vec<String> {
    let mut statement = connection.prepare(sql).unwrap();
    let count = statement.column_count();
    statement
        .query_map([], |row| {
            let mut parts = Vec::new();
            for index in 0..count {
                let value: rusqlite::types::Value = row.get(index)?;
                parts.push(format!("{value:?}"));
            }
            Ok(parts.join("|"))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn count(connection: &Connection, sql: &str) -> i64 {
    connection.query_row(sql, [], |row| row.get(0)).unwrap()
}

/// Every derived table must be explained by the surviving episodes.
fn assert_consistent(connection: &Connection) {
    assert_eq!(
        rows(
            connection,
            "SELECT entity_id, kind, MAX(last_utc_ms), COUNT(*) FROM activity_episodes GROUP BY entity_id, kind ORDER BY 1, 2"
        ),
        rows(
            connection,
            "SELECT entity_id, kind, last_utc_ms, episodes FROM activity_entity_summary ORDER BY 1, 2"
        ),
        "summary matches episodes"
    );
    assert_eq!(
        count(
            connection,
            "SELECT COUNT(*) FROM activity_entities WHERE id NOT IN (SELECT entity_id FROM activity_episodes)"
        ),
        0,
        "no episode-less entity survives"
    );
    for table in [
        "activity_vocabulary",
        "activity_search_grams",
        "activity_entity_summary",
        "activity_daily",
        "activity_entity_rank",
    ] {
        assert_eq!(
            count(
                connection,
                &format!(
                    "SELECT COUNT(*) FROM {table} WHERE entity_id NOT IN (SELECT id FROM activity_entities)"
                )
            ),
            0,
            "{table} has no orphan"
        );
    }
    assert_eq!(
        count(
            connection,
            "SELECT COUNT(*) FROM activity_receipts WHERE episode_id NOT IN (SELECT id FROM activity_episodes)"
        ),
        0,
        "no orphan receipts"
    );
    // The rank table equals a rebuild from the daily buckets.
    let rank = rows(
        connection,
        "SELECT entity_id, kind, rank_key, active_days, last_utc_ms FROM activity_entity_rank ORDER BY 1, 2",
    );
    let mut copy = Connection::open_in_memory().unwrap();
    copy.execute_batch(
        "CREATE TABLE activity_daily (entity_id TEXT, kind TEXT, utc_day INTEGER, episodes INTEGER, last_utc_ms INTEGER);
         CREATE TABLE activity_entity_rank (entity_id TEXT, kind TEXT, rank_key REAL, active_days INTEGER, last_utc_ms INTEGER);",
    )
    .unwrap();
    for (e, k, d, n, l) in {
        let mut s = connection
            .prepare("SELECT entity_id, kind, utc_day, episodes, last_utc_ms FROM activity_daily")
            .unwrap();
        s.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
            ))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect::<Vec<_>>()
    } {
        copy.execute(
            "INSERT INTO activity_daily VALUES (?1, ?2, ?3, ?4, ?5)",
            params![e, k, d, n, l],
        )
        .unwrap();
    }
    let tx = copy.transaction().unwrap();
    rebuild_rank(&tx).unwrap();
    tx.commit().unwrap();
    assert_eq!(
        rank,
        rows(
            &copy,
            "SELECT entity_id, kind, rank_key, active_days, last_utc_ms FROM activity_entity_rank ORDER BY 1, 2"
        ),
        "rank matches rebuild from daily"
    );
}

#[test]
fn purge_keeps_rank_summary_daily_hidden_vocabulary_and_grams_consistent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("purge.sqlite3");
    let mut store = ActivityStore::open(&path).unwrap();
    let epoch = store.privacy_epoch().unwrap();
    // old: only before cutoff (vanishes). mixed: spans cutoff across days.
    // fresh: only after cutoff. hiddenold: hidden and old. hiddenmixed: hidden, survives.
    let cutoff = 10 * DAY;
    let plan: &[(&str, &str, &[u64])] = &[
        ("old", "Old Archive", &[DAY, 2 * DAY + 5, 3 * DAY]),
        (
            "mixed",
            "Mixed Reports",
            &[DAY + 1, 2 * DAY + 1, 11 * DAY + 7, 12 * DAY + 9, 12 * DAY + 20],
        ),
        ("fresh", "Fresh Notes", &[10 * DAY, 11 * DAY, 20 * DAY]),
        ("hiddenold", "Hidden Past", &[DAY]),
        ("hiddenmixed", "Hidden Mixed", &[DAY + 3, 15 * DAY, 16 * DAY]),
    ];
    for (id, name, times) in plan {
        for at in *times {
            store.record(&receipt(id, name, *at, epoch)).unwrap();
        }
    }
    {
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "INSERT INTO activity_hidden(entity_id) VALUES ('hiddenold'), ('hiddenmixed');",
            )
            .unwrap();
        assert_consistent(&connection);
    }
    let total = store.episode_count().unwrap();
    let removed = store.purge_before(cutoff).unwrap();
    // old: 3, mixed: 2, hiddenold: 1, hiddenmixed: 1
    assert_eq!(removed, 7);
    assert_eq!(store.episode_count().unwrap(), total - 7);
    assert_eq!(store.privacy_epoch().unwrap(), epoch, "purge does not bump epoch");

    let connection = Connection::open(&path).unwrap();
    assert_consistent(&connection);
    assert_eq!(
        rows(&connection, "SELECT id FROM activity_entities ORDER BY id"),
        vec!["Text(\"fresh\")", "Text(\"hiddenmixed\")", "Text(\"mixed\")"]
    );
    assert_eq!(
        rows(
            &connection,
            "SELECT entity_id, episodes FROM activity_entity_summary ORDER BY 1"
        ),
        vec![
            "Text(\"fresh\")|Integer(3)",
            "Text(\"hiddenmixed\")|Integer(2)",
            "Text(\"mixed\")|Integer(2)",
        ]
    );
    // vocabulary and grams follow the surviving entities only
    assert_eq!(
        count(&connection, "SELECT COUNT(*) FROM activity_vocabulary"),
        3
    );
    assert_eq!(
        count(
            &connection,
            "SELECT COUNT(DISTINCT entity_id) FROM activity_search_grams"
        ),
        3
    );
    // old daily buckets are gone, survivors keep theirs
    assert_eq!(
        count(
            &connection,
            "SELECT COUNT(*) FROM activity_daily WHERE last_utc_ms < 10 * 86400000"
        ),
        0
    );
    assert_eq!(count(&connection, "SELECT COUNT(*) FROM activity_daily"), 3 + 2 + 2);
    // hidden flag of a surviving entity is preserved; query surface still works
    assert_eq!(
        count(
            &connection,
            "SELECT COUNT(*) FROM activity_hidden WHERE entity_id = 'hiddenmixed'"
        ),
        1
    );
    let recent = store
        .recent(0, 30 * DAY, ActivityKind::FolderVisit, epoch, 10)
        .unwrap()
        .unwrap();
    let ids: Vec<_> = recent.iter().map(|r| r.entity.id.clone()).collect();
    assert!(ids.contains(&"fresh".to_string()) && ids.contains(&"mixed".to_string()));
    assert!(!ids.contains(&"old".to_string()));
    // purge again is a no-op
    assert_eq!(store.purge_before(cutoff).unwrap(), 0);
    // purge everything leaves nothing behind
    assert_eq!(store.purge_before(100 * DAY).unwrap() as u64, total - 7);
    let connection = Connection::open(&path).unwrap();
    for table in [
        "activity_entities",
        "activity_episodes",
        "activity_entity_summary",
        "activity_daily",
        "activity_entity_rank",
        "activity_vocabulary",
        "activity_search_grams",
    ] {
        assert_eq!(count(&connection, &format!("SELECT COUNT(*) FROM {table}")), 0, "{table}");
    }
}

#[test]
fn purge_with_receipts_and_cascades_leaves_no_orphans() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cascade.sqlite3");
    let mut store = ActivityStore::open(&path).unwrap();
    let epoch = store.privacy_epoch().unwrap();
    for index in 0..40u64 {
        let id = format!("e{index:02}");
        for slot in 0..4u64 {
            store
                .record(&receipt(&id, &format!("Entity {index:02}"), (index % 5 + slot * 3) * DAY + 1, epoch))
                .unwrap();
        }
    }
    store.purge_before(7 * DAY).unwrap();
    let connection = Connection::open(&path).unwrap();
    assert_consistent(&connection);
    assert!(count(&connection, "SELECT COUNT(*) FROM activity_entities") > 0);
}

/// Seeds `entities` x 10 episodes through plain SQL. `receipts` adds one
/// activity_receipts row per episode (what record() produces); `whole` makes
/// even entities entirely older than the 45 day cutoff (entity removal path),
/// otherwise every entity straddles it (half of each entity's episodes go).
fn seed(path: &std::path::Path, entities: u64, receipts: bool, whole: bool) {
    const EPISODES_PER_ENTITY: u64 = 10;
    let mut connection = Connection::open(path).unwrap();
    let tx = connection.transaction().unwrap();
    {
        let mut entity = tx
            .prepare(
                "INSERT INTO activity_entities(id, kind, volume_id, locator, parent_path, display_name, availability, metadata)
                 VALUES (?1, 'Folder', 'C:', ?2, 'C:\\Bench', ?3, 'Available', '{}')",
            )
            .unwrap();
        let mut episode = tx
            .prepare(
                "INSERT INTO activity_episodes(entity_id, kind, start_utc_ms, last_utc_ms)
                 VALUES (?1, 'folderVisit', ?2, ?2)",
            )
            .unwrap();
        let mut receipt_insert = tx
            .prepare("INSERT INTO activity_receipts(event_id, action_id, episode_id) VALUES (?1, ?1, ?2)")
            .unwrap();
        let mut vocab = tx
            .prepare("INSERT INTO activity_vocabulary(entity_id, search_key) VALUES (?1, ?2)")
            .unwrap();
        let mut gram = tx
            .prepare("INSERT OR IGNORE INTO activity_search_grams(kind, gram, entity_id) VALUES ('Folder', ?1, ?2)")
            .unwrap();
        let mut hidden = tx
            .prepare("INSERT INTO activity_hidden(entity_id) VALUES (?1)")
            .unwrap();
        for index in 0..entities {
            let id = format!("bench-{index:06}");
            let name = format!("Folder Series {index:06}");
            entity
                .execute(params![id, format!("C:\\Bench\\{index:06}"), name])
                .unwrap();
            vocab.execute(params![id, name.to_lowercase()]).unwrap();
            let chars: Vec<char> = name.to_lowercase().chars().collect();
            for window in chars.windows(3) {
                let piece: String = window.iter().collect();
                gram.execute(params![piece, id]).unwrap();
            }
            if index % 50 == 0 {
                hidden.execute([&id]).unwrap();
            }
            for slot in 0..EPISODES_PER_ENTITY {
                let at = if whole {
                    if index % 2 == 0 {
                        slot * 4 * DAY + 1_000_000 + index % 3_600_000
                    } else {
                        50 * DAY + slot * 4 * DAY + index % 3_600_000
                    }
                } else {
                    (90 * DAY / EPISODES_PER_ENTITY) * slot + 1_000_000 + index % 3_600_000
                };
                episode.execute(params![id, at as i64]).unwrap();
                if receipts {
                    let row = tx.last_insert_rowid();
                    receipt_insert
                        .execute(params![format!("r-{index}-{slot}"), row])
                        .unwrap();
                }
            }
        }
    }
    tx.execute(
        "INSERT OR REPLACE INTO activity_entity_summary(entity_id, kind, last_utc_ms, episodes)
         SELECT entity_id, kind, MAX(last_utc_ms), COUNT(*) FROM activity_episodes GROUP BY entity_id, kind",
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
    rebuild_rank(&tx).unwrap();
    tx.commit().unwrap();
}

fn run_purge_perf(receipts: bool, whole: bool, limit_secs: u64) {
    use std::time::Instant;
    let entities: u64 = std::env::var("FERRUM_PURGE_ENTITIES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(100_000);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("purge-bench.sqlite3");
    let mut store = ActivityStore::open(&path).unwrap();
    let seed_start = Instant::now();
    seed(&path, entities, receipts, whole);
    println!(
        "seed {entities} entities (receipts={receipts} whole={whole}): {:?}",
        seed_start.elapsed()
    );
    let cutoff = 45 * DAY;
    let start = Instant::now();
    let removed = store.purge_before(cutoff).unwrap();
    let elapsed = start.elapsed();
    println!("purge_before removed {removed} of {} episodes in {elapsed:?}", entities * 10);
    assert_eq!(removed as u64, entities * 10 / 2);
    let connection = Connection::open(&path).unwrap();
    let survivors = if whole { entities / 2 } else { entities } as i64;
    assert_eq!(count(&connection, "SELECT COUNT(*) FROM activity_entities"), survivors);
    assert_eq!(count(&connection, "SELECT COUNT(*) FROM activity_entity_summary"), survivors);
    assert_eq!(count(&connection, "SELECT COUNT(*) FROM activity_entity_rank"), survivors);
    assert_eq!(count(&connection, "SELECT COUNT(*) FROM activity_vocabulary"), survivors);
    assert_eq!(
        count(&connection, "SELECT COUNT(*) FROM activity_daily WHERE last_utc_ms < 45 * 86400000"),
        0
    );
    if receipts {
        assert_eq!(
            count(&connection, "SELECT COUNT(*) FROM activity_receipts"),
            entities as i64 * 5
        );
    }
    if entities <= 20_000 {
        assert_consistent(&connection);
    }
    if entities >= 100_000 {
        assert!(elapsed.as_secs() < limit_secs, "purge took {elapsed:?}");
    }
}

#[test]
#[ignore = "perf: 100k entities / 1M episodes; run explicitly"]
fn purge_before_100k_entities_1m_episodes_under_10_seconds() {
    run_purge_perf(false, false, 10);
}

#[test]
#[ignore = "perf: 100k entities / 1M episodes with receipts; run explicitly"]
fn purge_before_100k_entities_1m_episodes_with_receipts_under_30_seconds() {
    // Receipt rows double the delete work and the commit fsync is noisy on a
    // shared host, so the receipts variants use a looser bound.
    run_purge_perf(true, false, 30);
}

#[test]
#[ignore = "perf: 100k entities, half removed whole, with receipts; run explicitly"]
fn purge_before_100k_entities_half_removed_whole_with_receipts_under_30_seconds() {
    run_purge_perf(true, true, 30);
}
