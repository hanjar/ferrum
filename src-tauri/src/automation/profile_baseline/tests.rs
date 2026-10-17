//! Separate, MAC'd baseline store of profile and automation runs (FER-522 design decision).
//! The point: a baseline the webview can write (the manual store) must never decide what an
//! unattended run deletes.

use std::collections::BTreeMap;
use std::sync::Arc;

use super::*;
use crate::automation::dpapi::{ApprovalKey, MemoryKeys};
use crate::automation::model::{DeletePolicy, SyncMode};
use crate::sync_baseline::{BASELINE_VERSION, BaselineDto, EntryDto, FactsDto};
use crate::sync_engine::execute::{EngineOutcome, EngineRequest, run};
use crate::sync_engine::test_support::{Recorder, Rig, set_mtime, test_env, write};

fn dirs() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let left = tmp.path().join("left");
    let right = tmp.path().join("right");
    std::fs::create_dir_all(&left).unwrap();
    std::fs::create_dir_all(&right).unwrap();
    (tmp, left, right)
}

fn pair(left: &Path, right: &Path) -> RootPair {
    RootPair {
        left_root: left.to_string_lossy().into_owned(),
        right_root: right.to_string_lossy().into_owned(),
    }
}

fn baseline_for(pair: &RootPair, names: &[&str]) -> BaselineDto {
    let facts = FactsDto {
        size: 3,
        modified_ms: 1_900_000_000_000,
    };
    BaselineDto {
        version: BASELINE_VERSION,
        left_root: pair.left_root.clone(),
        right_root: pair.right_root.clone(),
        entries: names
            .iter()
            .map(|n| {
                (
                    (*n).to_string(),
                    EntryDto {
                        left: facts.clone(),
                        right: facts.clone(),
                    },
                )
            })
            .collect::<BTreeMap<_, _>>(),
        journal: None,
    }
}

fn store_in(dir: &Path, keys: Arc<MemoryKeys>) -> ProfileBaselineStore {
    ProfileBaselineStore::open(dir, keys)
}

fn only_file(dir: &Path) -> std::path::PathBuf {
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    assert_eq!(files.len(), 1, "{files:?}");
    files.remove(0)
}

/// A saved baseline carries a MAC, loads as trusted, and `verify_profile_baseline` says so.
#[test]
fn a_saved_baseline_is_signed_and_trusted() {
    let (tmp, left, right) = dirs();
    let keys = Arc::new(MemoryKeys::with_key(3));
    let store = store_in(tmp.path(), keys);
    let p = pair(&left, &right);
    assert_eq!(
        verify_profile_baseline(&store, &p).unwrap(),
        BaselineTrust::Absent
    );
    store
        .engine_store()
        .save_unguarded_for_tests(&p, &baseline_for(&p, &["a.txt"]))
        .unwrap();
    let file = only_file(&tmp.path().join(BASELINES_DIR));
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("\"mac\":\""), "{text}");
    assert_eq!(
        verify_profile_baseline(&store, &p).unwrap(),
        BaselineTrust::Trusted
    );
    assert!(matches!(
        store.engine_store().load(&p).unwrap(),
        LoadResult::Ok { .. }
    ));
}

/// The profile store lives in its own folder; the manual store of the same pair cannot see it.
#[test]
fn the_manual_store_and_the_profile_store_are_separate_namespaces() {
    let (tmp, left, right) = dirs();
    let keys = Arc::new(MemoryKeys::with_key(3));
    let profile_store = store_in(tmp.path(), keys);
    let manual_dir = tmp.path().join("sync-baselines");
    let manual = SyncBaselineStore::open(&manual_dir);
    let p = pair(&left, &right);
    profile_store
        .engine_store()
        .save_unguarded_for_tests(&p, &baseline_for(&p, &["a.txt"]))
        .unwrap();
    assert!(
        matches!(
            manual.load(&p).unwrap(),
            LoadResult::FirstRun {
                reason: FirstRunReason::None
            }
        ),
        "a manual baseline load never sees profile baselines"
    );
    manual
        .save_unguarded_for_tests(&p, &baseline_for(&p, &["forged.txt"]))
        .unwrap();
    // And the other way round: the manual write did not touch the profile baseline.
    let LoadResult::Ok { baseline } = profile_store.engine_store().load(&p).unwrap() else {
        panic!("trusted baseline expected")
    };
    assert!(baseline.entries.contains_key("a.txt"));
    assert!(!baseline.entries.contains_key("forged.txt"));
    assert_ne!(manual_dir, tmp.path().join(BASELINES_DIR));
}

/// A baseline written by anything without the key - here the plain (manual-style) store pointed
/// straight at the profile folder, as a generic file write would - is UNTRUSTED.
#[test]
fn a_baseline_without_a_mac_is_untrusted() {
    let (tmp, left, right) = dirs();
    let keys = Arc::new(MemoryKeys::with_key(3));
    let store = store_in(tmp.path(), keys);
    let p = pair(&left, &right);
    let forger = SyncBaselineStore::open(&tmp.path().join(BASELINES_DIR));
    forger
        .save_unguarded_for_tests(&p, &baseline_for(&p, &["a.txt"]))
        .unwrap();
    assert_eq!(
        verify_profile_baseline(&store, &p).unwrap(),
        BaselineTrust::Untrusted
    );
    assert!(matches!(
        store.engine_store().load(&p).unwrap(),
        LoadResult::FirstRun {
            reason: FirstRunReason::Untrusted
        }
    ));
}

/// Editing a signed file (an entry, the MAC, the key) or signing with another key all fail.
#[test]
fn tampering_is_untrusted() {
    let (tmp, left, right) = dirs();
    let keys = Arc::new(MemoryKeys::with_key(3));
    let store = store_in(tmp.path(), keys.clone());
    let p = pair(&left, &right);
    store
        .engine_store()
        .save_unguarded_for_tests(&p, &baseline_for(&p, &["a.txt"]))
        .unwrap();
    let file = only_file(&tmp.path().join(BASELINES_DIR));
    let original = std::fs::read_to_string(&file).unwrap();

    // One more "in sync" entry slipped in.
    let edited = original.replace("\"a.txt\"", "\"a.txt\":{\"left\":{\"size\":3,\"modifiedMs\":1900000000000},\"right\":{\"size\":3,\"modifiedMs\":1900000000000}},\"extra.txt\"");
    std::fs::write(&file, &edited).unwrap();
    assert_eq!(
        verify_profile_baseline(&store, &p).unwrap(),
        BaselineTrust::Untrusted,
        "a changed body"
    );

    // A changed size in the existing entry.
    std::fs::write(&file, original.replacen("\"size\":3", "\"size\":4", 1)).unwrap();
    assert_eq!(
        verify_profile_baseline(&store, &p).unwrap(),
        BaselineTrust::Untrusted,
        "a changed fact"
    );

    // The MAC stripped.
    let value: serde_json::Value = serde_json::from_str(&original).unwrap();
    let mut stripped = value.clone();
    stripped.as_object_mut().unwrap().remove("mac");
    std::fs::write(&file, stripped.to_string()).unwrap();
    assert_eq!(
        verify_profile_baseline(&store, &p).unwrap(),
        BaselineTrust::Untrusted,
        "no mac"
    );

    // The MAC swapped for garbage.
    let mut garbage = value.clone();
    garbage["mac"] = serde_json::Value::String("00".repeat(32));
    std::fs::write(&file, garbage.to_string()).unwrap();
    assert_eq!(
        verify_profile_baseline(&store, &p).unwrap(),
        BaselineTrust::Untrusted,
        "wrong mac"
    );

    // Untouched again: trusted.
    std::fs::write(&file, &original).unwrap();
    assert_eq!(
        verify_profile_baseline(&store, &p).unwrap(),
        BaselineTrust::Trusted
    );

    // Another key (store copied to another user/machine, or the key lost and recreated).
    keys.set(KeyLoad::Key(ApprovalKey::for_tests(99)));
    assert_eq!(
        verify_profile_baseline(&store, &p).unwrap(),
        BaselineTrust::Untrusted,
        "another key"
    );
    keys.set(KeyLoad::Missing);
    assert_eq!(
        verify_profile_baseline(&store, &p).unwrap(),
        BaselineTrust::Untrusted,
        "no key"
    );
}

#[test]
fn verify_reports_unavailable_roots_and_refuses_nested_ones() {
    let (tmp, left, right) = dirs();
    let keys = Arc::new(MemoryKeys::with_key(3));
    let store = store_in(tmp.path(), keys);
    let gone = pair(&left, &tmp.path().join("not-there"));
    assert_eq!(
        verify_profile_baseline(&store, &gone).unwrap(),
        BaselineTrust::RootsUnavailable
    );
    let nested = pair(&left, &left.join("inside"));
    std::fs::create_dir_all(left.join("inside")).unwrap();
    assert!(verify_profile_baseline(&store, &nested).is_err());
    let _ = right;
}

/// The store directory as a reparse point is not used at all.
#[cfg(windows)]
#[test]
fn a_junction_as_the_baselines_folder_is_unavailable() {
    let (tmp, left, right) = dirs();
    let elsewhere = tmp.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let store_dir = tmp.path().join("store");
    std::fs::create_dir_all(&store_dir).unwrap();
    let linked = std::process::Command::new("cmd")
        .args(["/c", "mklink", "/J"])
        .arg(store_dir.join(BASELINES_DIR))
        .arg(&elsewhere)
        .output()
        .is_ok_and(|o| o.status.success());
    assert!(linked);
    let store = ProfileBaselineStore::open(&store_dir, Arc::new(MemoryKeys::with_key(3)));
    let p = pair(&left, &right);
    assert!(
        store
            .engine_store()
            .save_unguarded_for_tests(&p, &baseline_for(&p, &["a.txt"]))
            .is_err()
    );
    assert!(std::fs::read_dir(&elsewhere).unwrap().next().is_none());
}

// ── The attack, end to end, through the real engine ──

const FORGED_NAMES: &[&str] = &[
    "new.txt",
    "same0.txt",
    "same1.txt",
    "same2.txt",
    "same3.txt",
    "same4.txt",
    "same5.txt",
];

/// Two-way sync with deletion propagation under a per-test rig: `new.txt` exists ONLY on the left;
/// `same0..5.txt` exist on both sides, equal.
fn forged_rig() -> (Rig, crate::automation::model::ProfilePolicy, RootPair) {
    let rig = Rig::new();
    write(&rig.left.join("new.txt"), b"new");
    set_mtime(&rig.left.join("new.txt"), 1_900_000_000);
    // Plenty of files that really are in sync, so one deletion stays under the delete guard.
    for n in 0..6 {
        for side in [&rig.left, &rig.right] {
            let file = side.join(format!("same{n}.txt"));
            write(&file, b"new");
            set_mtime(&file, 1_900_000_000);
        }
    }
    let profile = rig.profile(
        SyncMode::TwoWay {
            propagate_deletes: true,
        },
        DeletePolicy::Permanent,
    );
    let p = pair(
        Path::new(&profile.left.path),
        Path::new(&profile.right.path),
    );
    (rig, profile, p)
}

fn run_engine(
    rig: &Rig,
    profile: &crate::automation::model::ProfilePolicy,
    baselines: &SyncBaselineStore,
) -> EngineOutcome {
    let lease = rig.lease(profile);
    let sink = Recorder::default();
    let env = test_env();
    let mut req = EngineRequest::new(profile, baselines, &sink, &env);
    req.run_id = 7;
    req.lease = Some(&lease);
    run(&req).outcome
}

/// THE property: a baseline forged to claim `new.txt` was already in sync makes an engine that
/// trusts it DELETE the user's only copy (the control, using the plain store); the profile store
/// refuses the forged file, treats the run as a first run and holds the file for review instead.
#[test]
fn a_forged_baseline_deletes_through_the_plain_store_but_not_through_the_profile_store() {
    // Control: the plain (unauthenticated) store trusts the forged baseline -> data loss.
    let (rig, profile, p) = forged_rig();
    let plain = SyncBaselineStore::open(&rig.base().join("plain"));
    plain
        .save_unguarded_for_tests(&p, &baseline_for(&p, FORGED_NAMES))
        .unwrap();
    run_engine(&rig, &profile, &plain);
    assert!(
        !rig.left.join("new.txt").exists(),
        "the control shows the attack is real: the only copy was deleted"
    );

    // Profile store: the same forged file placed where a generic file write could put it.
    let (rig, profile, p) = forged_rig();
    let keys = Arc::new(MemoryKeys::with_key(3));
    let store_dir = rig.base().join("store");
    std::fs::create_dir_all(&store_dir).unwrap();
    let store = ProfileBaselineStore::open(&store_dir, keys);
    let forger = SyncBaselineStore::open(&store_dir.join(BASELINES_DIR));
    forger
        .save_unguarded_for_tests(&p, &baseline_for(&p, FORGED_NAMES))
        .unwrap();
    let outcome = run_engine(&rig, &profile, store.engine_store());
    assert!(rig.left.join("new.txt").exists(), "nothing was deleted");
    // Without a trusted baseline a left-only file is "new or deleted? unknown": it is held for
    // review (never deleted, never guessed).
    assert!(
        matches!(outcome, EngineOutcome::ConflictHeld { .. }),
        "{outcome:?}"
    );
    assert!(!rig.right.join("new.txt").exists());
    assert_eq!(std::fs::read(rig.left.join("new.txt")).unwrap(), b"new");
}

/// The authenticated store works end to end: run 1 settles a signed baseline, a later deletion
/// on one side propagates to the other under that trusted baseline (design §5.5).
#[test]
fn a_run_settles_a_signed_baseline_and_later_deletes_propagate() {
    let rig = Rig::new();
    for side in [&rig.left, &rig.right] {
        write(&side.join("keep.txt"), b"keep");
        write(&side.join("gone.txt"), b"gone");
        set_mtime(&side.join("keep.txt"), 1_900_000_000);
        set_mtime(&side.join("gone.txt"), 1_900_000_000);
    }
    let profile = rig.profile(
        SyncMode::TwoWay {
            propagate_deletes: true,
        },
        DeletePolicy::Permanent,
    );
    let p = pair(
        Path::new(&profile.left.path),
        Path::new(&profile.right.path),
    );
    let store_dir = rig.base().join("store");
    std::fs::create_dir_all(&store_dir).unwrap();
    let store = ProfileBaselineStore::open(&store_dir, Arc::new(MemoryKeys::with_key(3)));
    run_engine(&rig, &profile, store.engine_store());
    assert_eq!(
        verify_profile_baseline(&store, &p).unwrap(),
        BaselineTrust::Trusted
    );
    std::fs::remove_file(rig.right.join("gone.txt")).unwrap();
    run_engine(&rig, &profile, store.engine_store());
    assert!(!rig.left.join("gone.txt").exists(), "propagated");
    assert!(rig.left.join("keep.txt").exists());
}
