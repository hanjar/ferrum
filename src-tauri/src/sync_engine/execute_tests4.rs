//! FER-562: the engine applies the protected-folder / Startup policy to every write. A test
//! policy (`DenyList`) protects `right\protected`; the roots themselves are valid.

use super::*;
use crate::automation::model::{DeletePolicy, Side, SyncMode};
use crate::sync_engine::errors::Reason;
use crate::sync_engine::test_support::*;
use std::fs;
use std::path::Path;
use std::sync::Arc;

fn one_way() -> SyncMode {
    SyncMode::OneWay { from: Side::Left }
}
fn mirror() -> SyncMode {
    SyncMode::Mirror { from: Side::Left }
}

fn run_with(rig: &Rig, profile: &ProfilePolicy, env: &EngineEnv) -> EngineReport {
    let lease = rig.lease(profile);
    let sink = NoSink;
    let mut req = EngineRequest::new(profile, &rig.baselines, &sink, env);
    req.run_id = 7;
    req.lease = Some(&lease);
    run(&req)
}

/// The versions folder next to the (canonical, long-name) roots: the run gate compares the stored
/// path with its canonical form, so a temp path in 8.3 spelling (CI runners) must not be used.
fn versions_dir(rig: &Rig) -> std::path::PathBuf {
    rig.left.parent().unwrap().join("versions")
}

fn guarded_env(deny: &[&Path]) -> EngineEnv {
    let mut env = test_env();
    env.guard = Arc::new(DenyList(deny.iter().map(|p| p.to_path_buf()).collect()));
    env
}

fn op<'a>(r: &'a EngineReport, name: &str) -> &'a OpRecord {
    r.ops
        .iter()
        .find(|o| o.name == name)
        .unwrap_or_else(|| panic!("no op for {name}: {:?}", r.ops))
}

// A file that would be created in a protected folder fails with `protected_folder`; the rest of
// the run goes through and nothing appears there.
#[test]
fn copy_into_a_protected_subfolder_fails_per_item_and_writes_nothing() {
    let rig = Rig::new();
    write(&rig.left.join("ok.txt"), b"fine");
    write(&rig.left.join("protected").join("x.txt"), b"secret");
    let profile = rig.profile(one_way(), DeletePolicy::Permanent);
    let env = guarded_env(&[&rig.right.join("protected")]);
    let r = run_with(&rig, &profile, &env);
    assert!(
        matches!(r.outcome, EngineOutcome::Partial { .. }),
        "{:?}",
        r.outcome
    );
    assert_eq!(op(&r, "ok.txt").status, OpStatus::Done);
    assert_eq!(op(&r, "protected/x.txt").status, OpStatus::Failed);
    assert_eq!(
        op(&r, "protected/x.txt").reason,
        Some(Reason::ProtectedFolder)
    );
    assert_eq!(
        fs::read_to_string(rig.right.join("ok.txt")).unwrap(),
        "fine"
    );
    assert!(!rig.right.join("protected").exists());
}

// An overwrite of a file that already exists in a protected folder is refused; the old content
// stays.
#[test]
fn overwrite_inside_a_protected_folder_is_refused() {
    let rig = Rig::new();
    write(&rig.left.join("protected").join("x.txt"), b"replacement!");
    write(&rig.right.join("protected").join("x.txt"), b"original");
    set_mtime(&rig.right.join("protected").join("x.txt"), 1_900_000_000);
    set_mtime(&rig.left.join("protected").join("x.txt"), 1_900_000_500);
    let profile = rig.profile(one_way(), DeletePolicy::Permanent);
    let env = guarded_env(&[&rig.right.join("protected")]);
    let r = run_with(&rig, &profile, &env);
    assert_eq!(op(&r, "protected/x.txt").status, OpStatus::Failed);
    assert_eq!(
        op(&r, "protected/x.txt").reason,
        Some(Reason::ProtectedFolder)
    );
    assert_eq!(
        fs::read_to_string(rig.right.join("protected").join("x.txt")).unwrap(),
        "original"
    );
}

// A mirror delete (permanent, and the move into a versions tree) is refused for a file inside a
// protected folder; the file stays, the rest is mirrored.
#[test]
fn deletes_and_versioning_moves_inside_a_protected_folder_are_refused() {
    for versioning in [false, true] {
        let rig = Rig::new();
        write(&rig.left.join("keep.txt"), b"k");
        write(&rig.right.join("protected").join("extra.txt"), b"e");
        write(&rig.right.join("gone.txt"), b"g");
        let policy = if versioning {
            DeletePolicy::Versioning {
                root: versions_dir(&rig).to_string_lossy().into_owned(),
            }
        } else {
            DeletePolicy::Permanent
        };
        let profile = rig.profile(mirror(), policy);
        let env = guarded_env(&[&rig.right.join("protected")]);
        let r = run_with(&rig, &profile, &env);
        let extra = op(&r, "protected/extra.txt");
        assert_eq!(extra.status, OpStatus::Failed, "{versioning}");
        assert_eq!(extra.reason, Some(Reason::ProtectedFolder), "{versioning}");
        assert!(
            rig.right.join("protected").join("extra.txt").exists(),
            "{versioning}"
        );
        assert!(!rig.right.join("gone.txt").exists(), "{versioning}");
        assert!(rig.right.join("keep.txt").exists(), "{versioning}");
    }
}

// With the versions tree itself protected, the move aside is refused and the target file is
// untouched.
#[test]
fn a_protected_versions_folder_receives_nothing() {
    let rig = Rig::new();
    write(&rig.left.join("keep.txt"), b"k");
    write(&rig.right.join("gone.txt"), b"g");
    let versions = versions_dir(&rig);
    let root = versions.to_string_lossy().into_owned();
    let profile = rig.profile(mirror(), DeletePolicy::Versioning { root });
    let env = guarded_env(&[&versions]);
    let r = run_with(&rig, &profile, &env);
    assert_eq!(op(&r, "gone.txt").status, OpStatus::Failed);
    assert_eq!(op(&r, "gone.txt").reason, Some(Reason::ProtectedFolder));
    assert!(rig.right.join("gone.txt").exists());
}

// An unrelated protected folder changes nothing for a normal run.
#[test]
fn a_normal_run_is_unaffected_by_a_policy_that_protects_something_else() {
    let rig = Rig::new();
    write(&rig.left.join("a.txt"), b"a");
    write(&rig.left.join("sub").join("b.txt"), b"b");
    let profile = rig.profile(one_way(), DeletePolicy::Permanent);
    let env = guarded_env(&[&rig.base().join("elsewhere")]);
    let r = run_with(&rig, &profile, &env);
    assert!(
        matches!(r.outcome, EngineOutcome::Completed { .. }),
        "{:?}",
        r.outcome
    );
    assert_eq!(tree(&rig.right), ["a.txt", "sub/", "sub/b.txt"]);
}

// The run gate (production policy) refuses a root inside a protected folder, and a written root
// that contains one; a source root that only holds one is allowed.
#[test]
fn the_run_gate_refuses_protected_roots() {
    let _g = crate::protected_dirs::TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let rig = Rig::new();
    write(&rig.left.join("a.txt"), b"a");
    let profile = rig.profile(one_way(), DeletePolicy::Permanent);

    // Right IS the protected folder.
    crate::protected_dirs::register([rig.right.clone()]);
    let r = run_with(&rig, &profile, &test_env());
    match &r.outcome {
        EngineOutcome::Failed { error } => assert_eq!(error.code, "root_protected"),
        other => panic!("expected Failed, got {other:?}"),
    }
    assert!(tree(&rig.right).is_empty());

    // Right (written) CONTAINS a protected folder.
    fs::create_dir_all(rig.right.join("state")).unwrap();
    crate::protected_dirs::register([rig.right.join("state")]);
    let r = run_with(&rig, &profile, &test_env());
    match &r.outcome {
        EngineOutcome::Failed { error } => assert_eq!(error.code, "root_protected"),
        other => panic!("expected Failed, got {other:?}"),
    }

    // Left (only read) contains a protected folder: allowed.
    crate::protected_dirs::register(Vec::<std::path::PathBuf>::new());
    fs::remove_dir_all(rig.right.join("state")).unwrap();
    fs::create_dir_all(rig.left.join("state")).unwrap();
    crate::protected_dirs::register([rig.left.join("state")]);
    let r = run_with(&rig, &profile, &test_env());
    crate::protected_dirs::register(Vec::<std::path::PathBuf>::new());
    assert!(
        matches!(r.outcome, EngineOutcome::Completed { .. }),
        "{:?}",
        r.outcome
    );
}
