//! ACT-09 QA suite for the activity store (FER-273 / FER-274 / FER-275 / FER-276).
//!
//! Scope: store-level behaviour only. The store receives receipts that the
//! caller has already decided are eligible; "refresh", "session restore",
//! "failed navigation" and "bulk sync" are *not* receipt kinds, so the
//! contract (docs/activity/decisions.md) says they must never reach `record`.
//! These tests prove that (a) nothing in the store inflates counts when such
//! traffic is replayed or misrouted within the rules the store can enforce, and
//! (b) where the store cannot enforce it (aliases it cannot resolve), the
//! behaviour is pinned down and documented rather than hidden.
//!
//! Benchmarks at the bottom are `#[ignore]`; run them with
//! `node scripts/bench-activity.mjs` (see docs/activity/verification.md).

use ferrum_activity_contract::{
    ActivityEntity, ActivityKind, ActivityReceipt, Availability, EntityKind, Provenance,
    ReceiptSource,
};
use ferrum_activity_store::{ActivityStore, RecordOutcome, SCHEMA_VERSION};
use rusqlite::Connection;

const DAY: u64 = 86_400_000;

fn folder_at(
    event: &str,
    entity_id: &str,
    locator: &str,
    name: &str,
    epoch: u64,
    at: u64,
) -> ActivityReceipt {
    ActivityReceipt {
        version: 1,
        event_id: event.into(),
        action_id: format!("act-{event}"),
        privacy_epoch: epoch,
        source: ReceiptSource::NavigationCommit,
        kind: ActivityKind::FolderVisit,
        provenance: Provenance::Manual,
        entity: ActivityEntity {
            id: entity_id.into(),
            kind: EntityKind::Folder,
            provider: "local".into(),
            volume_id: "volume-one".into(),
            locator: locator.into(),
            parent_path: "C:\\Work".into(),
            display_name: name.into(),
            availability: Availability::Available,
        },
        occurred_utc_ms: at,
    }
}

fn folder(event: &str, id: &str, epoch: u64, at: u64) -> ActivityReceipt {
    folder_at(event, id, &format!("C:\\Work\\{id}"), id, epoch, at)
}

fn file_open(event: &str, id: &str, epoch: u64, at: u64) -> ActivityReceipt {
    let mut value = folder_at(event, id, &format!("C:\\Work\\{id}.txt"), id, epoch, at);
    value.source = ReceiptSource::OpenAccepted;
    value.kind = ActivityKind::FileOpenRequested;
    value.entity.kind = EntityKind::File;
    value
}

fn recent_ids(store: &mut ActivityStore, until: u64) -> Vec<String> {
    let epoch = store.privacy_epoch().unwrap();
    store
        .recent(0, until, ActivityKind::FolderVisit, epoch, 100)
        .unwrap()
        .unwrap()
        .into_iter()
        .map(|row| row.entity.id)
        .collect()
}

// ---------------------------------------------------------------------------
// FER-274: exclusion aliases
// ---------------------------------------------------------------------------

#[test]
fn exclusion_matches_case_slash_and_trailing_separator_aliases_on_windows_paths() {
    let mut store = ActivityStore::in_memory().unwrap();
    store.exclude_root("C:\\Work\\Private").unwrap();
    let aliases = [
        "C:\\Work\\Private",
        "c:\\work\\private\\sub",
        "C:/WORK/private/sub/deeper",
        "c:/work/PRIVATE/",
        "C:\\Work\\Private\\",
    ];
    for (index, alias) in aliases.iter().enumerate() {
        let receipt = folder_at(
            &format!("e{index}"),
            &format!("p{index}"),
            alias,
            "x",
            1,
            10,
        );
        assert_eq!(
            store.record(&receipt).unwrap(),
            RecordOutcome::Ignored,
            "alias {alias} must be excluded"
        );
    }
    assert_eq!(store.episode_count().unwrap(), 0);
}

#[test]
fn exclusion_uses_subtree_boundary_not_string_prefix() {
    let mut store = ActivityStore::in_memory().unwrap();
    store.exclude_root("C:\\Work\\Private").unwrap();
    for (index, sibling) in [
        "C:\\Work\\PrivateX",
        "C:\\Work\\Private2\\a",
        "C:\\Work\\Priv",
    ]
    .iter()
    .enumerate()
    {
        let receipt = folder_at(
            &format!("s{index}"),
            &format!("s{index}"),
            sibling,
            "x",
            1,
            10,
        );
        assert_eq!(
            store.record(&receipt).unwrap(),
            RecordOutcome::NewEpisode,
            "{sibling} is outside the subtree"
        );
    }
    assert_eq!(store.episode_count().unwrap(), 3);
}

#[test]
fn exclusion_unc_is_case_insensitive_and_posix_is_case_sensitive() {
    let mut store = ActivityStore::in_memory().unwrap();
    store.exclude_root("\\\\Server\\Share\\Secret").unwrap();
    store.exclude_root("/home/me/Secret").unwrap();
    let unc = folder_at("u", "u", "\\\\SERVER\\share\\secret\\x", "x", 2, 10);
    assert_eq!(store.record(&unc).unwrap(), RecordOutcome::Ignored);
    let posix_same = folder_at("p1", "p1", "/home/me/Secret/x", "x", 2, 10);
    assert_eq!(store.record(&posix_same).unwrap(), RecordOutcome::Ignored);
    // POSIX file systems are case sensitive: a different-case sibling is a
    // different directory and must stay recordable.
    let posix_other = folder_at("p2", "p2", "/home/me/secret/x", "x", 2, 10);
    assert_eq!(
        store.record(&posix_other).unwrap(),
        RecordOutcome::NewEpisode
    );
}

#[test]
fn excluding_a_root_purges_existing_alias_spelled_history_and_survives_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("act.sqlite3");
    {
        let mut store = ActivityStore::open(&path).unwrap();
        store
            .record(&folder_at("a", "a", "C:\\Work\\Private\\A", "A", 0, 100))
            .unwrap();
        store
            .record(&folder_at("b", "b", "C:\\Work\\Public", "B", 0, 200))
            .unwrap();
        // Different spelling than stored locators.
        let epoch = store.exclude_root("c:/work/private/").unwrap();
        assert_eq!(epoch, 1);
        assert_eq!(recent_ids(&mut store, 1_000), vec!["b"]);
    }
    let mut store = ActivityStore::open(&path).unwrap();
    assert_eq!(store.privacy_epoch().unwrap(), 1);
    // Persisted exclusion still blocks the alias after restart.
    let again = folder_at("a2", "a", "C:/WORK/PRIVATE/A", "A", 1, 300);
    assert_eq!(store.record(&again).unwrap(), RecordOutcome::Ignored);
    assert_eq!(recent_ids(&mut store, 1_000), vec!["b"]);
}

#[test]
fn exclusion_rejects_parent_traversal_and_relative_roots() {
    let mut store = ActivityStore::in_memory().unwrap();
    assert!(store.exclude_root("C:\\Work\\..\\Private").is_err());
    assert!(store.exclude_root("Private").is_err());
    assert!(store.exclude_root("").is_err());
    assert_eq!(
        store.privacy_epoch().unwrap(),
        0,
        "rejected exclusion must not bump the epoch"
    );
}

/// Junction / symlink / 8.3 short-name / verbatim (`\\?\`) aliases are NOT
/// resolvable by a string-only store: it cannot know that two spellings name
/// the same directory. The contract therefore requires the caller (ACT-06
/// identity) to canonicalize locators before `record`/`exclude_root`. This
/// test pins the current behaviour so a regression to *worse* is caught and the
/// gap stays documented. See docs/activity/verification.md "Gaps".
#[test]
fn junction_short_name_and_verbatim_aliases_are_not_resolved_by_store_documented_gap() {
    let mut store = ActivityStore::in_memory().unwrap();
    store.exclude_root("C:\\Secret").unwrap();
    // Junction `C:\Work\Link` -> `C:\Secret` (caller did not canonicalize).
    let via_junction = folder_at("j", "j", "C:\\Work\\Link\\Docs", "Docs", 1, 10);
    assert_eq!(
        store.record(&via_junction).unwrap(),
        RecordOutcome::NewEpisode
    );
    // 8.3 short name of a long exclusion root.
    let mut store = ActivityStore::in_memory().unwrap();
    store.exclude_root("C:\\Program Files\\Secret").unwrap();
    let short = folder_at("s", "s", "C:\\PROGRA~1\\Secret\\x", "x", 1, 10);
    assert_eq!(store.record(&short).unwrap(), RecordOutcome::NewEpisode);
    // Verbatim prefix as produced by std::fs::canonicalize on Windows.
    let mut store = ActivityStore::in_memory().unwrap();
    store.exclude_root("C:\\Work\\Private").unwrap();
    let verbatim = folder_at("v", "v", "\\\\?\\C:\\Work\\Private\\x", "x", 1, 10);
    assert_eq!(
        store.record(&verbatim).unwrap(),
        RecordOutcome::NewEpisode,
        "verbatim-prefix locators are not normalized by the store"
    );
    // When the caller canonicalizes to the excluded root's spelling, exclusion works.
    let canonical = folder_at("c", "c", "C:\\Work\\Private\\x", "x", 1, 10);
    assert_eq!(store.record(&canonical).unwrap(), RecordOutcome::Ignored);
}

// ---------------------------------------------------------------------------
// FER-274: offline devices
// ---------------------------------------------------------------------------

#[test]
fn offline_entities_are_kept_and_not_treated_as_deleted() {
    let mut store = ActivityStore::in_memory().unwrap();
    for (index, availability) in [
        Availability::Offline,
        Availability::Missing,
        Availability::Inaccessible,
        Availability::Unknown,
    ]
    .into_iter()
    .enumerate()
    {
        let mut receipt = folder(
            &format!("o{index}"),
            &format!("o{index}"),
            0,
            1_000 + index as u64,
        );
        receipt.entity.availability = availability.clone();
        assert_eq!(store.record(&receipt).unwrap(), RecordOutcome::NewEpisode);
    }
    let epoch = store.privacy_epoch().unwrap();
    let rows = store
        .recent(0, 10_000, ActivityKind::FolderVisit, epoch, 100)
        .unwrap()
        .unwrap();
    assert_eq!(rows.len(), 4);
    let state = |id: &str| {
        rows.iter()
            .find(|row| row.entity.id == id)
            .unwrap()
            .entity
            .availability
            .clone()
    };
    assert_eq!(state("o0"), Availability::Offline);
    assert_eq!(state("o1"), Availability::Missing);
    assert_eq!(state("o2"), Availability::Inaccessible);
    assert_eq!(state("o3"), Availability::Unknown);
}

#[test]
fn availability_change_updates_entity_without_new_episode_or_identity_loss() {
    let mut store = ActivityStore::in_memory().unwrap();
    let mut offline = folder("d1", "usb", 0, 1_000);
    offline.entity.availability = Availability::Offline;
    assert_eq!(store.record(&offline).unwrap(), RecordOutcome::NewEpisode);
    // Device comes back; same entity id, same volume, a few seconds later.
    let online = folder("d2", "usb", 0, 5_000);
    assert_eq!(
        store.record(&online).unwrap(),
        RecordOutcome::ContinuedEpisode
    );
    let epoch = store.privacy_epoch().unwrap();
    let rows = store
        .recent(0, 10_000, ActivityKind::FolderVisit, epoch, 10)
        .unwrap()
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].episode_count, 1);
    assert_eq!(rows[0].entity.availability, Availability::Available);
}

#[test]
fn drive_letter_reuse_with_another_volume_is_rejected_not_silently_retargeted() {
    let mut store = ActivityStore::in_memory().unwrap();
    store.record(&folder("v1", "removable", 0, 1_000)).unwrap();
    let mut reused = folder("v2", "removable", 0, 100_000);
    reused.entity.volume_id = "another-volume".into();
    assert!(
        store.record(&reused).is_err(),
        "volume change under the same entity id"
    );
    // Failed record must roll back completely.
    assert_eq!(store.episode_count().unwrap(), 1);
    // The event id of the failed attempt is not consumed.
    let mut retry = folder("v2", "removable-new", 0, 100_000);
    retry.entity.volume_id = "another-volume".into();
    assert_eq!(store.record(&retry).unwrap(), RecordOutcome::NewEpisode);
}

// ---------------------------------------------------------------------------
// FER-274: failed navigation
// ---------------------------------------------------------------------------

#[test]
fn failed_or_invalid_navigation_receipts_leave_no_trace_and_do_not_burn_ids() {
    let mut store = ActivityStore::in_memory().unwrap();
    // Relative / garbage locator: what a half-formed "failed load" might carry.
    let mut invalid = folder("f1", "f1", 0, 10);
    invalid.entity.locator = "not-absolute".into();
    assert!(store.record(&invalid).is_err());
    // A file entity cannot be a folder visit.
    let mut wrong_kind = folder("f2", "f2", 0, 10);
    wrong_kind.entity.kind = EntityKind::File;
    assert!(store.record(&wrong_kind).is_err());
    // Source/kind mismatch (e.g. wrongly labelled refresh as commit).
    let mut mismatch = folder("f3", "f3", 0, 10);
    mismatch.source = ReceiptSource::OpenAccepted;
    assert!(store.record(&mismatch).is_err());
    // Empty IDs.
    let mut empty = folder("f4", "f4", 0, 10);
    empty.event_id = "  ".into();
    assert!(store.record(&empty).is_err());
    assert_eq!(store.episode_count().unwrap(), 0);
    assert!(recent_ids(&mut store, 100).is_empty());
    // The IDs of the rejected receipts are still available for a valid retry.
    assert_eq!(
        store.record(&folder("f1", "f1", 0, 10)).unwrap(),
        RecordOutcome::NewEpisode
    );
}

#[test]
fn late_receipts_from_a_cancelled_or_pre_clear_navigation_are_dropped_by_epoch() {
    let mut store = ActivityStore::in_memory().unwrap();
    let in_flight = folder("late", "late", 0, 10);
    let epoch = store.clear_all().unwrap();
    assert_eq!(epoch, 1);
    assert_eq!(store.record(&in_flight).unwrap(), RecordOutcome::StaleEpoch);
    assert_eq!(store.episode_count().unwrap(), 0);
    // Pause also bumps the epoch; queued writes from before the pause are dropped.
    let queued = folder("queued", "queued", epoch, 20);
    let after_pause = store.set_recording_enabled(false).unwrap();
    assert_eq!(store.record(&queued).unwrap(), RecordOutcome::StaleEpoch);
    // Current-epoch receipt while paused is ignored, not stored.
    assert_eq!(
        store
            .record(&folder("paused", "paused", after_pause, 30))
            .unwrap(),
        RecordOutcome::Ignored
    );
    assert_eq!(store.episode_count().unwrap(), 0);
}

#[test]
fn non_eligible_activity_kinds_never_become_episodes() {
    let mut store = ActivityStore::in_memory().unwrap();
    let mut preview = file_open("pv", "doc", 0, 10);
    preview.source = ReceiptSource::PreviewCompleted;
    preview.kind = ActivityKind::FilePreview;
    assert_eq!(store.record(&preview).unwrap(), RecordOutcome::Ignored);
    let mut job = folder("job", "job", 0, 10);
    job.source = ReceiptSource::JobCompleted;
    job.kind = ActivityKind::OperationCompleted;
    assert_eq!(store.record(&job).unwrap(), RecordOutcome::Ignored);
    assert_eq!(store.episode_count().unwrap(), 0);
}

// ---------------------------------------------------------------------------
// FER-274: restore inflation
// ---------------------------------------------------------------------------

#[test]
fn restart_and_session_restore_replay_do_not_inflate_counts() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("restore.sqlite3");
    let receipts: Vec<ActivityReceipt> = (0..500)
        .map(|i| {
            folder(
                &format!("r{i}"),
                &format!("restore-{}", i % 50),
                0,
                1_000 + i * 60_000,
            )
        })
        .collect();
    let baseline;
    {
        let mut store = ActivityStore::open(&path).unwrap();
        for receipt in &receipts {
            assert_eq!(store.record(receipt).unwrap(), RecordOutcome::NewEpisode);
        }
        baseline = store.episode_count().unwrap();
        assert_eq!(baseline, 500);
    }
    // Ten restarts: opening the store alone must never change any count.
    for _ in 0..10 {
        let store = ActivityStore::open(&path).unwrap();
        assert_eq!(store.episode_count().unwrap(), baseline);
        assert_eq!(store.privacy_epoch().unwrap(), 0);
    }
    // A restore that re-fires the whole receipt log (same event ids) is a no-op.
    let mut store = ActivityStore::open(&path).unwrap();
    for receipt in &receipts {
        assert_eq!(store.record(receipt).unwrap(), RecordOutcome::Duplicate);
    }
    assert_eq!(store.episode_count().unwrap(), baseline);
    let epoch = store.privacy_epoch().unwrap();
    let rows = store
        .recent(0, 10 * DAY, ActivityKind::FolderVisit, epoch, 100)
        .unwrap()
        .unwrap();
    assert_eq!(rows.len(), 50);
    assert!(rows.iter().all(|row| row.episode_count == 10));
}

// ---------------------------------------------------------------------------
// FER-274: duplicate hooks
// ---------------------------------------------------------------------------

#[test]
fn duplicate_event_or_action_ids_are_idempotent_regardless_of_other_fields() {
    let mut store = ActivityStore::in_memory().unwrap();
    let first = folder("ev-1", "dup", 0, 1_000);
    assert_eq!(store.record(&first).unwrap(), RecordOutcome::NewEpisode);
    assert_eq!(store.record(&first).unwrap(), RecordOutcome::Duplicate);
    // Same event id, different action id, much later.
    let mut same_event = first.clone();
    same_event.action_id = "other-action".into();
    same_event.occurred_utc_ms = 10 * DAY;
    assert_eq!(store.record(&same_event).unwrap(), RecordOutcome::Duplicate);
    // Same action id, different event id.
    let mut same_action = first.clone();
    same_action.event_id = "other-event".into();
    same_action.occurred_utc_ms = 10 * DAY;
    assert_eq!(
        store.record(&same_action).unwrap(),
        RecordOutcome::Duplicate
    );
    // Same event id on a different entity must not leak onto it either.
    let other_entity = folder("ev-1", "someone-else", 0, 2_000);
    assert_eq!(
        store.record(&other_entity).unwrap(),
        RecordOutcome::Duplicate
    );
    assert_eq!(store.episode_count().unwrap(), 1);
    assert_eq!(recent_ids(&mut store, 11 * DAY), vec!["dup"]);
}

#[test]
fn cross_pane_hooks_within_cooldown_form_one_episode_with_window_boundary() {
    let mut store = ActivityStore::in_memory().unwrap();
    assert_eq!(
        store.record(&folder("p1", "x", 0, 100_000)).unwrap(),
        RecordOutcome::NewEpisode
    );
    // Second pane/window, different event/action IDs, 29.999 s later: same episode.
    assert_eq!(
        store.record(&folder("p2", "x", 0, 129_999)).unwrap(),
        RecordOutcome::ContinuedEpisode
    );
    // Exactly 30 s after the episode's *last* timestamp starts a new episode.
    assert_eq!(
        store.record(&folder("p3", "x", 0, 159_999)).unwrap(),
        RecordOutcome::NewEpisode
    );
    assert_eq!(store.episode_count().unwrap(), 2);
}

#[test]
fn two_store_handles_on_one_file_share_cooldown_like_two_windows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("multi.sqlite3");
    let mut window_a = ActivityStore::open(&path).unwrap();
    let mut window_b = ActivityStore::open(&path).unwrap();
    assert_eq!(
        window_a.record(&folder("a1", "shared", 0, 10_000)).unwrap(),
        RecordOutcome::NewEpisode
    );
    assert_eq!(
        window_b.record(&folder("b1", "shared", 0, 12_000)).unwrap(),
        RecordOutcome::ContinuedEpisode
    );
    assert_eq!(
        window_a.record(&folder("a2", "shared", 0, 14_000)).unwrap(),
        RecordOutcome::ContinuedEpisode
    );
    assert_eq!(
        window_b.record(&folder("b1", "shared", 0, 12_000)).unwrap(),
        RecordOutcome::Duplicate
    );
    assert_eq!(window_a.episode_count().unwrap(), 1);
    // A privacy change in one window invalidates the other's queued write.
    let epoch = window_a.set_recording_enabled(false).unwrap();
    assert_eq!(
        window_b.record(&folder("b2", "shared", 0, 16_000)).unwrap(),
        RecordOutcome::StaleEpoch
    );
    assert_eq!(window_b.privacy_epoch().unwrap(), epoch);
}

/// Cross-pane receipts can arrive out of timestamp order. A receipt whose
/// timestamp is *older* than the episode's last timestamp used to open a
/// second episode even inside the 30 s cooldown. FER-252 closed that gap, so
/// this test now pins the correct behaviour (one continued episode); rollup
/// consistency is covered in `tests/gaps.rs`.
#[test]
fn out_of_order_cross_pane_receipt_inside_cooldown_continues_the_episode() {
    let mut store = ActivityStore::in_memory().unwrap();
    assert_eq!(
        store.record(&folder("n1", "ooo", 0, 20_000)).unwrap(),
        RecordOutcome::NewEpisode
    );
    // Older timestamp (pane B's commit was stamped earlier but delivered later).
    let outcome = store.record(&folder("n2", "ooo", 0, 15_000)).unwrap();
    assert_eq!(outcome, RecordOutcome::ContinuedEpisode);
    assert_eq!(store.episode_count().unwrap(), 1);
}

// ---------------------------------------------------------------------------
// FER-274: refresh / bulk inflation
// ---------------------------------------------------------------------------

#[test]
fn one_thousand_refresh_style_receipts_collapse_into_one_episode() {
    // Refresh never emits a receipt by contract. If a hook mistakenly fired on
    // every refresh (distinct event IDs, 100 ms apart) the cooldown chain must
    // still keep the statistic at one episode instead of 1000.
    let mut store = ActivityStore::in_memory().unwrap();
    let start = 1_000_000_u64;
    let mut outcomes = [0usize; 2];
    for i in 0..1_000_u64 {
        match store
            .record(&folder(&format!("rf{i}"), "refreshed", 0, start + i * 100))
            .unwrap()
        {
            RecordOutcome::NewEpisode => outcomes[0] += 1,
            RecordOutcome::ContinuedEpisode => outcomes[1] += 1,
            other => panic!("unexpected outcome {other:?}"),
        }
    }
    assert_eq!(outcomes, [1, 999]);
    assert_eq!(store.episode_count().unwrap(), 1);
    let epoch = store.privacy_epoch().unwrap();
    let rows = store
        .recent(0, start + DAY, ActivityKind::FolderVisit, epoch, 10)
        .unwrap()
        .unwrap();
    assert_eq!(rows[0].episode_count, 1);
    // Last access follows the chain, count does not.
    assert_eq!(rows[0].last_utc_ms, start + 999 * 100);
    // One active day -> never a "Frequent" habit.
    assert!(
        store
            .frequent(0, start + DAY, ActivityKind::FolderVisit, epoch, 10)
            .unwrap()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn bulk_sync_of_ten_thousand_files_does_not_touch_access_counts() {
    // Bulk ZIP / sync / copy members are never personal opens: they arrive (if
    // at all) as operation receipts, which the store must ignore wholesale.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sync.sqlite3");
    let mut store = ActivityStore::open(&path).unwrap();
    store
        .record(&file_open("real-open", "report", 0, 5_000))
        .unwrap();
    let started = std::time::Instant::now();
    for i in 0..10_000_u64 {
        let mut receipt = file_open(
            &format!("sync-{i}"),
            &format!("synced-{i:05}"),
            0,
            10_000 + i,
        );
        receipt.source = ReceiptSource::JobCompleted;
        receipt.kind = ActivityKind::OperationCompleted;
        assert_eq!(store.record(&receipt).unwrap(), RecordOutcome::Ignored);
    }
    let elapsed = started.elapsed();
    assert_eq!(
        store.episode_count().unwrap(),
        1,
        "only the explicit open counts"
    );
    drop(store);
    let raw = Connection::open(&path).unwrap();
    let entities: i64 = raw
        .query_row("SELECT COUNT(*) FROM activity_entities", [], |r| r.get(0))
        .unwrap();
    let vocab: i64 = raw
        .query_row("SELECT COUNT(*) FROM activity_vocabulary", [], |r| r.get(0))
        .unwrap();
    let receipts: i64 = raw
        .query_row("SELECT COUNT(*) FROM activity_receipts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        (entities, vocab, receipts),
        (1, 1, 1),
        "ignored receipts must not create entities/vocabulary/receipt rows"
    );
    assert!(
        elapsed.as_secs() < 60,
        "10k ignored receipts took {elapsed:?}"
    );
}

// ---------------------------------------------------------------------------
// FER-273: E2E store flow on a persistent fresh profile
// ---------------------------------------------------------------------------

#[test]
fn e2e_fresh_profile_visits_restart_recent_frequent_search_forget() {
    let profile = tempfile::tempdir().unwrap();
    // Fresh profile: directory does not exist yet; open() must create it.
    let db_path = profile
        .path()
        .join("appdata")
        .join("activity")
        .join("activity.sqlite3");
    assert!(!db_path.exists());
    let now = 20 * DAY + 3_600_000;

    // --- session 1: first run, no history, then visits across days -----------
    {
        let mut store = ActivityStore::open(&db_path).unwrap();
        let version: i64 = Connection::open(&db_path)
            .unwrap()
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(
            version, SCHEMA_VERSION,
            "fresh profile starts at the current schema"
        );
        assert!(store.recording_enabled().unwrap());
        let epoch = store.privacy_epoch().unwrap();
        assert_eq!(epoch, 0);
        assert!(
            store
                .recent(0, now, ActivityKind::FolderVisit, epoch, 20)
                .unwrap()
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .frequent(0, now, ActivityKind::FolderVisit, epoch, 20)
                .unwrap()
                .unwrap()
                .is_empty()
        );
        let mut serial = 0;
        let mut visit = |store: &mut ActivityStore, id: &str, name: &str, day: u64, offset: u64| {
            serial += 1;
            let receipt = folder_at(
                &format!("e2e-{serial}"),
                id,
                &format!("C:\\Work\\{name}"),
                name,
                0,
                day * DAY + offset,
            );
            assert_eq!(store.record(&receipt).unwrap(), RecordOutcome::NewEpisode);
        };
        // "Reports" visited on 4 different days (a habit), "Invoices" on 2, "Quarry Notes" once.
        for day in [10, 14, 17, 19] {
            visit(&mut store, "reports", "Reports", day, 1_000);
        }
        for day in [18, 19] {
            visit(&mut store, "invoices", "Invoices", day, 2_000);
        }
        visit(&mut store, "quarry", "Quarry Notes", 12, 3_000);
        // A file open (separate kind, separate Recent list).
        let open = file_open("e2e-open", "budget", 0, 19 * DAY + 5_000);
        assert_eq!(store.record(&open).unwrap(), RecordOutcome::NewEpisode);
        assert_eq!(store.episode_count().unwrap(), 8);
    }

    assert!(
        db_path.exists(),
        "session 1 must have created the database file"
    );

    // --- session 2: restart ---------------------------------------------------
    let mut store = ActivityStore::open(&db_path).unwrap();
    let epoch = store.privacy_epoch().unwrap();
    assert_eq!(epoch, 0);
    assert_eq!(store.episode_count().unwrap(), 8);

    let recent = store
        .recent(0, now, ActivityKind::FolderVisit, epoch, 20)
        .unwrap()
        .unwrap();
    let ids: Vec<&str> = recent.iter().map(|row| row.entity.id.as_str()).collect();
    // Last-access descending: invoices d19+2000 < reports d19+1000? invoices is later in-day.
    assert_eq!(ids, vec!["invoices", "reports", "quarry"]);
    assert_eq!(recent[1].episode_count, 4);
    let files = store
        .recent(0, now, ActivityKind::FileOpenRequested, epoch, 20)
        .unwrap()
        .unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].entity.id, "budget");

    // Frequent needs >= 2 distinct active UTC days: reports first, then invoices.
    let frequent = store
        .frequent(0, now, ActivityKind::FolderVisit, epoch, 20)
        .unwrap()
        .unwrap();
    let ids: Vec<&str> = frequent.iter().map(|row| row.entity.id.as_str()).collect();
    assert_eq!(ids, vec!["reports", "invoices"]);
    assert_eq!(frequent[0].active_days, 4);
    assert!(frequent[0].score > frequent[1].score);

    // Search: exact, prefix-ish, typo, and a never-visited name.
    let search = |store: &mut ActivityStore, query: &str| -> Vec<String> {
        store
            .search_candidates(query, ActivityKind::FolderVisit, epoch, 20)
            .unwrap()
            .unwrap()
            .into_iter()
            .map(|e| e.id)
            .collect()
    };
    assert_eq!(search(&mut store, "reports"), vec!["reports"]);
    assert_eq!(
        search(&mut store, "reoprts"),
        vec!["reports"],
        "transposition typo"
    );
    assert_eq!(
        search(&mut store, "quarry"),
        vec!["quarry"],
        "visited once, old, not in Frequent"
    );
    assert!(search(&mut store, "never visited folder").is_empty());
    let located = store
        .locator_candidates("work!\\reports", ActivityKind::FolderVisit, epoch, 5)
        .unwrap()
        .unwrap();
    assert!(located.len() <= 1);

    // --- forget one entity ----------------------------------------------------
    store.forget_entity("reports").unwrap();
    let new_epoch = store.privacy_epoch().unwrap();
    assert_eq!(new_epoch, epoch + 1);
    // Queries from the pre-forget epoch are late and must return nothing.
    assert!(
        store
            .recent(0, now, ActivityKind::FolderVisit, epoch, 20)
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .frequent(0, now, ActivityKind::FolderVisit, epoch, 20)
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .search_candidates("reports", ActivityKind::FolderVisit, epoch, 20)
            .unwrap()
            .is_none()
    );
    // Current-epoch queries: gone from every surface, others intact, files untouched.
    let recent_after = store
        .recent(0, now, ActivityKind::FolderVisit, new_epoch, 20)
        .unwrap()
        .unwrap();
    assert_eq!(
        recent_after
            .iter()
            .map(|r| r.entity.id.as_str())
            .collect::<Vec<_>>(),
        vec!["invoices", "quarry"]
    );
    assert!(
        store
            .search_candidates("reports", ActivityKind::FolderVisit, new_epoch, 20)
            .unwrap()
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .search_candidates("reoprts", ActivityKind::FolderVisit, new_epoch, 20)
            .unwrap()
            .unwrap()
            .is_empty()
    );
    let frequent_after = store
        .frequent(0, now, ActivityKind::FolderVisit, new_epoch, 20)
        .unwrap()
        .unwrap();
    assert_eq!(
        frequent_after
            .iter()
            .map(|r| r.entity.id.as_str())
            .collect::<Vec<_>>(),
        vec!["invoices"]
    );
    // A queued/late receipt from before the forget cannot resurrect it.
    assert_eq!(
        store
            .record(&folder_at(
                "late",
                "reports",
                "C:\\Work\\Reports",
                "Reports",
                epoch,
                20 * DAY
            ))
            .unwrap(),
        RecordOutcome::StaleEpoch
    );
    drop(store);

    // --- session 3: forget survived restart ----------------------------------
    let mut store = ActivityStore::open(&db_path).unwrap();
    assert_eq!(store.privacy_epoch().unwrap(), new_epoch);
    assert_eq!(recent_ids(&mut store, now), vec!["invoices", "quarry"]);
    assert_eq!(store.episode_count().unwrap(), 4);
    // A deliberate new visit after forgetting starts from scratch (count 1).
    let again = folder_at(
        "again",
        "reports",
        "C:\\Work\\Reports",
        "Reports",
        new_epoch,
        20 * DAY + 1_000,
    );
    assert_eq!(store.record(&again).unwrap(), RecordOutcome::NewEpisode);
    let rows = store
        .recent(0, now, ActivityKind::FolderVisit, new_epoch, 20)
        .unwrap()
        .unwrap();
    assert_eq!(rows[0].entity.id, "reports");
    assert_eq!(rows[0].episode_count, 1);
    assert!(store.recording_enabled().unwrap());
}

#[test]
fn e2e_pause_exclude_and_clear_persist_across_restart() {
    let profile = tempfile::tempdir().unwrap();
    let db_path = profile.path().join("activity").join("activity.sqlite3");
    {
        let mut store = ActivityStore::open(&db_path).unwrap();
        store.record(&folder("k1", "keep", 0, 1_000)).unwrap();
        let paused = store.set_recording_enabled(false).unwrap();
        assert_eq!(paused, 1);
        assert_eq!(
            store
                .record(&folder("k2", "paused-visit", paused, 2_000))
                .unwrap(),
            RecordOutcome::Ignored
        );
    }
    {
        let mut store = ActivityStore::open(&db_path).unwrap();
        assert!(
            !store.recording_enabled().unwrap(),
            "pause persists across restart"
        );
        assert_eq!(store.privacy_epoch().unwrap(), 1);
        let resumed = store.set_recording_enabled(true).unwrap();
        assert_eq!(resumed, 2);
        assert_eq!(
            store
                .record(&folder("k3", "resumed", resumed, 3_000))
                .unwrap(),
            RecordOutcome::NewEpisode
        );
        let epoch = store.clear_all().unwrap();
        assert_eq!(epoch, 3);
    }
    let mut store = ActivityStore::open(&db_path).unwrap();
    assert_eq!(store.privacy_epoch().unwrap(), 3);
    assert_eq!(store.episode_count().unwrap(), 0);
    assert!(recent_ids(&mut store, 10_000).is_empty());
    assert!(
        store
            .search_candidates("keep", ActivityKind::FolderVisit, 3, 10)
            .unwrap()
            .unwrap()
            .is_empty()
    );
    // Receipt IDs of cleared history can be reused (no stale dedup row survives).
    assert_eq!(
        store.record(&folder("k1", "keep", 3, 20_000)).unwrap(),
        RecordOutcome::NewEpisode
    );
}

// ---------------------------------------------------------------------------
// FER-275: performance regression found by the record() benchmark
// ---------------------------------------------------------------------------

/// BUG (found by bench-activity, ACT-09.4): `refresh_rank` in store/src/lib.rs
/// reads `SELECT utc_day, episodes, last_utc_ms FROM activity_daily WHERE
/// entity_id = ?1 AND kind = ?2`. Since schema v10 added the covering index
/// `activity_daily_frequent(kind, last_utc_ms, entity_id, utc_day, episodes)`,
/// SQLite plans this as a scan of every bucket of that kind (`SEARCH ... USING
/// COVERING INDEX activity_daily_frequent (kind=?)`) instead of the primary key
/// (entity_id, kind, utc_day). Every record() therefore costs O(daily buckets):
/// p50 about 135 ms at 100k entities / 1M episodes versus about 5 ms on a tiny DB.
/// Fix: `FROM activity_daily INDEXED BY sqlite_autoindex_activity_daily_1` (verified:
/// 105 ms -> 0.035 ms for the query, 120 ms -> 8 ms per record() on the 100k DB).
#[test]
fn refresh_rank_bucket_read_uses_primary_key_not_kind_wide_scan() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("plan.sqlite3");
    let mut store = ActivityStore::open(&path).unwrap();
    store.record(&folder("plan", "plan", 0, 1_000)).unwrap();
    drop(store);
    let raw = Connection::open(&path).unwrap();
    let mut statement = raw
        .prepare(
            "EXPLAIN QUERY PLAN SELECT utc_day, episodes, last_utc_ms FROM activity_daily WHERE entity_id = ?1 AND +kind = ?2",
        )
        .unwrap();
    let plan: Vec<String> = statement
        .query_map(["plan", "folderVisit"], |row| row.get::<_, String>(3))
        .unwrap()
        .map(|row| row.unwrap())
        .collect();
    let text = plan.join(" | ");
    assert!(
        text.contains("entity_id=?") && !text.contains("activity_daily_frequent"),
        "bucket read must seek by entity: {text}"
    );
}

// ---------------------------------------------------------------------------
// FER-275 / FER-276: benchmarks (ignored; run with scripts/bench-activity.mjs)
// ---------------------------------------------------------------------------

mod bench {
    use super::*;
    use rusqlite::params;
    use std::path::Path;
    use std::time::{Duration, Instant};

    const BASE_MS: u64 = 90 * DAY;

    fn micros(d: Duration) -> u128 {
        d.as_micros()
    }

    fn percentile(sorted: &[u128], q: f64) -> u128 {
        let idx = ((sorted.len() as f64 * q).ceil() as usize).clamp(1, sorted.len()) - 1;
        sorted[idx]
    }

    fn fmt_ms(us: u128) -> String {
        format!("{:.3}", us as f64 / 1000.0)
    }

    /// Simple text histogram over fixed microsecond buckets.
    fn report(label: &str, samples: &mut [u128]) {
        samples.sort_unstable();
        let n = samples.len();
        println!(
            "  {label:<34} n={n:<6} p50={:>9} ms  p95={:>9} ms  p99={:>9} ms  max={:>9} ms",
            fmt_ms(percentile(samples, 0.50)),
            fmt_ms(percentile(samples, 0.95)),
            fmt_ms(percentile(samples, 0.99)),
            fmt_ms(*samples.last().unwrap()),
        );
        let edges: [(u128, &str); 9] = [
            (100, "<0.1 ms"),
            (250, "<0.25"),
            (500, "<0.5"),
            (1_000, "<1"),
            (2_500, "<2.5"),
            (5_000, "<5"),
            (10_000, "<10"),
            (50_000, "<50"),
            (u128::MAX, ">=50"),
        ];
        let mut lower = 0u128;
        for (upper, name) in edges {
            let count = samples
                .iter()
                .filter(|s| **s >= lower && **s < upper)
                .count();
            lower = upper;
            if count == 0 {
                continue;
            }
            let bar = "#".repeat((count * 40).div_ceil(n).max(1));
            println!("      {name:>8} ms | {bar} {count}");
        }
    }

    /// Seeds through raw SQL (as the ACT-09.3 fixture does) so the seed is fast;
    /// rollups and rank table are rebuilt the way `record()` maintains them.
    fn seed(path: &Path, entities: u64, episodes_per_entity: u64) -> Duration {
        drop(ActivityStore::open(path).unwrap()); // create schema
        let started = Instant::now();
        let mut connection = Connection::open(path).unwrap();
        connection
            .execute_batch("PRAGMA synchronous = OFF; PRAGMA journal_mode = MEMORY;")
            .unwrap();
        let tx = connection.transaction().unwrap();
        {
            let mut entity = tx
                .prepare("INSERT INTO activity_entities(id, kind, volume_id, locator, parent_path, display_name, availability, metadata) VALUES (?1, 'Folder', 'C:', ?2, 'C:\\Bench', ?3, 'Available', ?4)")
                .unwrap();
            let mut episode = tx
                .prepare("INSERT INTO activity_episodes(entity_id, kind, start_utc_ms, last_utc_ms) VALUES (?1, 'folderVisit', ?2, ?2)")
                .unwrap();
            let mut vocab = tx
                .prepare("INSERT INTO activity_vocabulary(entity_id, search_key) VALUES (?1, ?2)")
                .unwrap();
            let mut gram = tx
                .prepare("INSERT OR IGNORE INTO activity_search_grams(kind, gram, entity_id) VALUES ('Folder', ?1, ?2)")
                .unwrap();
            for index in 0..entities {
                let id = format!("bench-{index:07}");
                let name = match index % 10 {
                    0 => format!("Project Reports {index:07}"),
                    5 => format!("Invoices Archive {index:07}"),
                    _ => format!("Folder Series {index:07}"),
                };
                let locator = format!("C:\\Bench\\{index:07}");
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
                let lower = name.to_lowercase();
                vocab.execute(params![id, lower]).unwrap();
                let chars: Vec<char> = lower.chars().collect();
                for window in chars.windows(3) {
                    let piece: String = window.iter().collect();
                    gram.execute(params![piece, id]).unwrap();
                }
                for slot in 0..episodes_per_entity {
                    let at = (BASE_MS / episodes_per_entity) * slot + 1_000_000 + index % 3_600_000;
                    episode.execute(params![id, at as i64]).unwrap();
                }
            }
        }
        tx.execute(
            "INSERT OR REPLACE INTO activity_entity_summary(entity_id, kind, last_utc_ms, episodes) SELECT entity_id, kind, MAX(last_utc_ms), COUNT(*) FROM activity_episodes GROUP BY entity_id, kind",
            [],
        )
        .unwrap();
        tx.execute(
            "INSERT OR REPLACE INTO activity_daily(entity_id, kind, utc_day, episodes, last_utc_ms) SELECT entity_id, kind, last_utc_ms / 86400000, COUNT(*), MAX(last_utc_ms) FROM activity_episodes GROUP BY entity_id, kind, last_utc_ms / 86400000",
            [],
        )
        .unwrap();
        ferrum_activity_store::rebuild_rank(&tx).unwrap();
        tx.commit().unwrap();
        started.elapsed()
    }

    /// Raw SQLite commit latency on the same directory (default journal, one
    /// row per transaction): the fixed floor any write-through record() pays.
    fn disk_floor(dir: &Path) {
        let connection = Connection::open(dir.join("floor.sqlite3")).unwrap();
        connection
            .execute_batch("CREATE TABLE t(a INTEGER);")
            .unwrap();
        let mut samples = Vec::new();
        for i in 0..400 {
            let started = Instant::now();
            connection
                .execute("INSERT INTO t VALUES (?1)", [i])
                .unwrap();
            samples.push(micros(started.elapsed()));
        }
        println!("disk floor (raw rusqlite 1-row autocommit insert, same dir):");
        report("raw commit", &mut samples);
    }

    fn bench_record(path: &Path, entities: u64) {
        let mut store = ActivityStore::open(path).unwrap();
        let epoch = store.privacy_epoch().unwrap();
        const N: u64 = 400;
        // All benchmark timestamps sit after the seeded data so no receipt is stale or older.
        let t0 = BASE_MS + 10 * DAY;
        let mut new_entity = Vec::new();
        let mut existing_new_episode = Vec::new();
        let mut continued = Vec::new();
        let mut duplicate = Vec::new();
        let mut ignored = Vec::new();
        let mut first_batch = Vec::new();
        for i in 0..N {
            let id = format!("live-{i:05}");
            let mut receipt = folder_at(
                &format!("n{i}"),
                &id,
                &format!("C:\\Live\\{id}"),
                &format!("Live Folder {i}"),
                epoch,
                t0 + i * 40_000,
            );
            receipt.entity.volume_id = "C:".into();
            let started = Instant::now();
            assert_eq!(store.record(&receipt).unwrap(), RecordOutcome::NewEpisode);
            new_entity.push(micros(started.elapsed()));
            first_batch.push(receipt);
        }
        for i in 0..N {
            // Existing seeded entity: new episode (>30 s after its last access).
            let index = (i * 7919) % entities;
            let id = format!("bench-{index:07}");
            let mut receipt = folder_at(
                &format!("x{i}"),
                &id,
                &format!("C:\\Bench\\{index:07}"),
                "Folder Series",
                epoch,
                t0 + DAY + i * 40_000,
            );
            receipt.entity.volume_id = "C:".into();
            // Keep display name consistent with the seeded entity kind of name.
            receipt.entity.display_name = match index % 10 {
                0 => format!("Project Reports {index:07}"),
                5 => format!("Invoices Archive {index:07}"),
                _ => format!("Folder Series {index:07}"),
            };
            let started = Instant::now();
            assert_eq!(store.record(&receipt).unwrap(), RecordOutcome::NewEpisode);
            existing_new_episode.push(micros(started.elapsed()));
            // Continued episode: same entity 2 s later.
            let mut again = receipt.clone();
            again.event_id = format!("c{i}");
            again.action_id = format!("act-c{i}");
            again.occurred_utc_ms += 2_000;
            let started = Instant::now();
            assert_eq!(
                store.record(&again).unwrap(),
                RecordOutcome::ContinuedEpisode
            );
            continued.push(micros(started.elapsed()));
        }
        for receipt in &first_batch {
            let started = Instant::now();
            assert_eq!(store.record(receipt).unwrap(), RecordOutcome::Duplicate);
            duplicate.push(micros(started.elapsed()));
        }
        for i in 0..N {
            let mut receipt = folder(&format!("ig{i}"), &format!("ig{i}"), epoch, t0);
            receipt.source = ReceiptSource::JobCompleted;
            receipt.kind = ActivityKind::OperationCompleted;
            let started = Instant::now();
            assert_eq!(store.record(&receipt).unwrap(), RecordOutcome::Ignored);
            ignored.push(micros(started.elapsed()));
        }
        println!(
            "record() overhead per call (navigation hook cost on this DB; on-disk, default journal, fsync per commit):"
        );
        report("new entity + new episode", &mut new_entity);
        report("existing entity + new episode", &mut existing_new_episode);
        report("continued episode (<30 s)", &mut continued);
        report("duplicate event id", &mut duplicate);
        report("ignored (operation receipt)", &mut ignored);
    }

    fn run(label: &str, entities: u64, episodes_per_entity: u64) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bench.sqlite3");
        println!(
            "=== {label}: {entities} entities x {episodes_per_entity} episodes = {} episodes ===",
            entities * episodes_per_entity
        );
        let seed_time = seed(&path, entities, episodes_per_entity);
        let size = std::fs::metadata(&path).unwrap().len();
        println!(
            "seed time {seed_time:.1?}; db file {:.1} MiB",
            size as f64 / 1_048_576.0
        );

        // Startup: open() on an existing, current-schema DB. Page cache is warm
        // (just written) so this is a warm-start number; first-ever open after
        // boot would add cold-read cost not measured here.
        let mut opens = Vec::new();
        let mut first_query = Vec::new();
        for _ in 0..25 {
            let started = Instant::now();
            let mut store = ActivityStore::open(&path).unwrap();
            opens.push(micros(started.elapsed()));
            let epoch = store.privacy_epoch().unwrap();
            let started = Instant::now();
            let rows = store
                .recent(0, BASE_MS + 2_000_000, ActivityKind::FolderVisit, epoch, 20)
                .unwrap()
                .unwrap();
            first_query.push(micros(started.elapsed()));
            assert!(!rows.is_empty());
        }
        println!("startup (warm cache, 25 open+drop cycles):");
        report("open()", &mut opens);
        report("first recent(limit 20) after open", &mut first_query);

        disk_floor(dir.path());
        bench_record(&path, entities);

        // Upgrade path: pretend the DB was last written by schema v9, so open()
        // must create + rebuild the rank table (what a v9 user pays once).
        if SCHEMA_VERSION != 10 {
            println!(
                "migration simulation skipped: written for v9 -> v10, current schema is v{SCHEMA_VERSION}"
            );
        } else {
            {
                let connection = Connection::open(&path).unwrap();
                connection
                    .execute_batch("DROP TABLE activity_entity_rank; PRAGMA user_version = 9;")
                    .unwrap();
            }
            let started = Instant::now();
            let store = ActivityStore::open(&path);
            let took = started.elapsed();
            match store {
                Ok(_) => println!(
                    "one-time migration v9 -> v{SCHEMA_VERSION} (rank rebuild) on this DB: {took:.2?}"
                ),
                Err(error) => {
                    println!("migration v9 -> v{SCHEMA_VERSION} could not be simulated: {error}")
                }
            }
        }
        println!();
    }

    /// Baseline: same on-disk setup with a tiny DB, to separate the fixed
    /// per-commit cost (journal + fsync) from cost that scales with DB size.
    #[test]
    #[ignore = "benchmark baseline: 1k entities / 2k episodes; run via scripts/bench-activity.mjs"]
    fn bench_startup_and_record_1k_entities_baseline() {
        run("DB-1k (baseline)", 1_000, 2);
    }

    #[test]
    #[ignore = "benchmark: 100k entities / 1M episodes; run via scripts/bench-activity.mjs"]
    fn bench_startup_and_record_100k_entities_1m_episodes() {
        run("DB-100k", 100_000, 10);
    }

    #[test]
    #[ignore = "benchmark: 1M entities / 2M episodes (slow seed); run via scripts/bench-activity.mjs"]
    fn bench_startup_and_record_1m_entities_2m_episodes() {
        run("DB-1M", 1_000_000, 2);
    }
}
