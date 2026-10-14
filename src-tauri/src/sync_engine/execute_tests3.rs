//! Engine tests, part 3: handle-level checks, links, cap, systemic delete failures.

use super::*;
use crate::archive_extract::publish::MoveError;
use crate::automation::model::{DeletePolicy, Side, SyncMode};
use crate::sync_engine::env::Mover;
use crate::sync_engine::errors::Reason;
use crate::sync_engine::test_support::*;
use std::fs;
use std::path::Path;
use std::sync::Arc;

fn mirror() -> SyncMode {
    SyncMode::Mirror { from: Side::Left }
}
fn one_way() -> SyncMode {
    SyncMode::OneWay { from: Side::Left }
}

fn read(p: &Path) -> String {
    fs::read_to_string(p).unwrap()
}

struct Ran {
    report: EngineReport,
}

fn go(rig: &Rig, profile: &ProfilePolicy, env: &EngineEnv) -> Ran {
    go_with(rig, profile, env, |_| {})
}

fn go_with(
    rig: &Rig,
    profile: &ProfilePolicy,
    env: &EngineEnv,
    tweak: impl FnOnce(&mut EngineRequest<'_>),
) -> Ran {
    let lease = rig.lease(profile);
    let sink = Recorder::default();
    let report = {
        let mut req = EngineRequest::new(profile, &rig.baselines, &sink, env);
        req.run_id = 7;
        req.lease = Some(&lease);
        tweak(&mut req);
        run(&req)
    };
    Ran { report }
}

fn status_of(r: &Ran, name: &str) -> OpStatus {
    r.report
        .ops
        .iter()
        .find(|o| o.name == name)
        .unwrap_or_else(|| panic!("no op for {name}: {:?}", r.report.ops))
        .status
}

fn reason_of(r: &Ran, name: &str) -> Option<Reason> {
    r.report
        .ops
        .iter()
        .find(|o| o.name == name)
        .and_then(|o| o.reason)
}

struct CrossVolume;

impl Mover for CrossVolume {
    fn rename_into(
        &self,
        _: &crate::archive_extract::publish::DirHandle,
        _: &std::ffi::OsStr,
        _: &crate::archive_extract::publish::DirHandle,
        _: &std::ffi::OsStr,
        _: crate::archive_extract::publish::FactsCheck<'_>,
    ) -> Result<bool, MoveError> {
        Err(MoveError::CrossDevice)
    }
}

/// Edit `path` right after the just-in-time check of the operation named `name`.
fn edit_after_jit(env: &mut EngineEnv, name: &'static str, path: std::path::PathBuf) {
    let hooks = Arc::new(TestHooks::default());
    *hooks.after_jit.lock().unwrap() = Some(Box::new(move |_, n| {
        if n == name {
            fs::write(&path, b"edited by the user in the gap, longer than before").unwrap();
        }
    }));
    env.hooks = hooks;
}

// RQ-7.5, T10: the facts are checked again on the file's OWN handle at the moment of the delete,
// so a file edited between the just-in-time look and the delete survives - permanent delete.
#[test]
fn permanent_delete_checks_the_file_on_its_own_handle() {
    let rig = Rig::new();
    write(&rig.left.join("keep.txt"), b"k");
    write(&rig.right.join("extra.txt"), b"old");
    let profile = rig.profile(mirror(), DeletePolicy::Permanent);
    let mut env = test_env();
    edit_after_jit(&mut env, "extra.txt", rig.right.join("extra.txt"));
    let r = go(&rig, &profile, &env);
    assert_eq!(status_of(&r, "extra.txt"), OpStatus::Changed);
    assert_eq!(
        read(&rig.right.join("extra.txt")),
        "edited by the user in the gap, longer than before"
    );
}

// ... the same for the versioning rename (same volume) ...
#[test]
fn versioning_move_checks_the_file_on_its_own_handle() {
    let rig = Rig::new();
    write(&rig.left.join("keep.txt"), b"k");
    write(&rig.right.join("extra.txt"), b"old");
    let profile = rig.profile(mirror(), rig.versioning());
    let mut env = test_env();
    edit_after_jit(&mut env, "extra.txt", rig.right.join("extra.txt"));
    let r = go(&rig, &profile, &env);
    assert_eq!(status_of(&r, "extra.txt"), OpStatus::Changed);
    assert!(rig.right.join("extra.txt").exists());
    assert!(
        tree(&rig.versions)
            .iter()
            .all(|n| !n.ends_with("extra.txt")),
        "nothing was versioned: {:?}",
        tree(&rig.versions)
    );
}

// ... and for the cross-volume copy+delete path: the stale copy is dropped, the source stays.
#[test]
fn cross_volume_move_of_a_file_edited_in_the_gap_keeps_the_source() {
    let rig = Rig::new();
    write(&rig.left.join("keep.txt"), b"k");
    write(&rig.right.join("extra.txt"), b"old");
    let profile = rig.profile(mirror(), rig.versioning());
    let mut env = test_env();
    env.mover = Arc::new(CrossVolume);
    edit_after_jit(&mut env, "extra.txt", rig.right.join("extra.txt"));
    let r = go(&rig, &profile, &env);
    assert_eq!(status_of(&r, "extra.txt"), OpStatus::Changed);
    assert_eq!(
        read(&rig.right.join("extra.txt")),
        "edited by the user in the gap, longer than before"
    );
    assert!(
        tree(&rig.versions)
            .iter()
            .all(|n| !n.ends_with("extra.txt"))
    );
}

// An overwrite under versioning keeps the previous version only if the target is still what the
// plan saw; an edited target is left alone and the new file is not copied over it.
#[test]
fn overwrite_under_versioning_does_not_move_a_target_edited_in_the_gap() {
    let rig = Rig::new();
    write(&rig.left.join("c.txt"), b"left version, newer");
    write(&rig.right.join("c.txt"), b"old");
    set_mtime(&rig.left.join("c.txt"), 1_900_000_500);
    set_mtime(&rig.right.join("c.txt"), 1_900_000_000);
    let profile = rig.profile(one_way(), rig.versioning());
    let mut env = test_env();
    edit_after_jit(&mut env, "c.txt", rig.right.join("c.txt"));
    let r = go(&rig, &profile, &env);
    assert_eq!(status_of(&r, "c.txt"), OpStatus::Changed);
    assert_eq!(
        read(&rig.right.join("c.txt")),
        "edited by the user in the gap, longer than before"
    );
    assert!(tree(&rig.versions).iter().all(|n| !n.ends_with("c.txt")));
}

// SP-3.4: one-way under versioning keeps the previous version of an overwritten file.
#[test]
fn one_way_versioning_keeps_overwritten_files_and_deletes_nothing() {
    let rig = Rig::new();
    write(&rig.left.join("c.txt"), b"left version, newer");
    write(&rig.right.join("c.txt"), b"old");
    write(&rig.right.join("only-right.txt"), b"stays");
    set_mtime(&rig.left.join("c.txt"), 1_900_000_500);
    set_mtime(&rig.right.join("c.txt"), 1_900_000_000);
    let profile = rig.profile(one_way(), rig.versioning());
    let r = go(&rig, &profile, &test_env());
    assert!(matches!(r.report.outcome, EngineOutcome::Completed { .. }));
    assert_eq!(read(&rig.right.join("c.txt")), "left version, newer");
    assert_eq!(read(&rig.right.join("only-right.txt")), "stays");
    assert_eq!(
        read(
            &rig.versions
                .join("Right")
                .join("2026-01-02 030405 #7")
                .join("c.txt")
        ),
        "old"
    );
}

// SE-9: the versioning root itself replaced by a junction after the plan is refused.
#[test]
fn versioning_root_replaced_by_a_junction_after_the_plan_is_refused() {
    let rig = Rig::new();
    write(&rig.left.join("keep.txt"), b"k");
    write(&rig.right.join("extra.txt"), b"must stay");
    fs::create_dir_all(&rig.versions).unwrap();
    let outside = rig.base().join("outside");
    fs::create_dir_all(&outside).unwrap();
    let profile = rig.profile(mirror(), rig.versioning());
    let hooks = Arc::new(TestHooks::default());
    let (versions, out2) = (rig.versions.clone(), outside.clone());
    let made = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let m2 = made.clone();
    *hooks.after_plan.lock().unwrap() = Some(Box::new(move || {
        fs::remove_dir(&versions).unwrap();
        if !make_junction(&versions, &out2) {
            m2.store(false, std::sync::atomic::Ordering::SeqCst);
        }
    }));
    let mut env = test_env();
    env.hooks = hooks;
    let r = go(&rig, &profile, &env);
    if !made.load(std::sync::atomic::Ordering::SeqCst) {
        eprintln!("SKIPPED: junction creation is not permitted here");
        let _ = fs::create_dir_all(&rig.versions);
        return;
    }
    match &r.report.outcome {
        EngineOutcome::Failed { error } => assert_eq!(error.code, "versioning_root_invalid"),
        other => panic!("expected Failed, got {other:?}"),
    }
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
    assert_eq!(read(&rig.right.join("extra.txt")), "must stay");
    assert!(!rig.right.join("keep.txt").exists(), "no operation ran");
    let _ = fs::remove_dir(&rig.versions);
}

// Design §5.3: more entries than the cap is a blocker with the documented text.
#[test]
fn too_many_entries_is_a_blocker() {
    let rig = Rig::new();
    for i in 0..6 {
        write(&rig.left.join(format!("f{i}.txt")), b"x");
    }
    let profile = rig.profile(one_way(), DeletePolicy::Permanent);
    let r = go_with(&rig, &profile, &test_env(), |req| req.entry_cap = 3);
    match &r.report.outcome {
        EngineOutcome::Failed { error } => {
            assert_eq!(error.code, "cap_exceeded");
            assert_eq!(
                error.text,
                "Too many items (more than 200,000) — narrow the profile with filters."
            );
        }
        other => panic!("expected Failed, got {other:?}"),
    }
    assert!(r.report.blockers.contains(&"truncated"));
    assert!(tree(&rig.right).is_empty());
}

// Links are entries, never followed: a junction on the target side is not deleted through, and
// its target keeps every file.
#[test]
fn a_junction_entry_is_never_followed_or_deleted_through() {
    let rig = Rig::new();
    write(&rig.left.join("keep.txt"), b"k");
    let outside = rig.base().join("outside");
    write(&outside.join("precious.txt"), b"precious");
    if !make_junction(&rig.right.join("J"), &outside) {
        eprintln!("SKIPPED: junction creation is not permitted here");
        return;
    }
    let profile = rig.profile(mirror(), DeletePolicy::Permanent);
    let r = go(&rig, &profile, &test_env());
    assert_eq!(read(&outside.join("precious.txt")), "precious");
    assert!(
        rig.right.join("J").exists(),
        "the link itself is left in place"
    );
    // Either skipped by the planner as a folder-ish entry or failed as an unsupported link -
    // never done.
    let done = r
        .report
        .ops
        .iter()
        .any(|o| o.name == "J" && o.status == OpStatus::Done);
    assert!(!done);
    if let Some(op) = r.report.ops.iter().find(|o| o.name == "J") {
        assert_eq!(op.status, OpStatus::Skipped);
        assert_eq!(reason_of(&r, "J"), Some(Reason::LinkUnsupported));
    }
    let _ = fs::remove_dir(rig.right.join("J"));
}

// RQ-7.2: a systemic failure while deleting (the Recycle Bin refuses with access denied) stops
// the run; the files not yet deleted are untouched and not started.
#[test]
fn systemic_failure_while_deleting_stops_with_the_rest_untouched() {
    let rig = Rig::new();
    write(&rig.left.join("keep.txt"), b"k");
    for n in ["a.txt", "b.txt", "c.txt"] {
        write(&rig.right.join(n), b"x");
    }
    let profile = rig.profile(mirror(), DeletePolicy::RecycleBin);
    let bin = FakeTrash::new(rig.base().join("bin"));
    *bin.fail.lock().unwrap() = Some(crate::domain::FsError::new(
        crate::domain::ErrorCode::PermissionDenied,
        "Access is denied. (os error 5)",
    ));
    let mut env = test_env();
    env.trash = bin;
    let r = go(&rig, &profile, &env);
    assert_eq!(status_of(&r, "a.txt"), OpStatus::Failed);
    assert_eq!(reason_of(&r, "a.txt"), Some(Reason::AccessDenied));
    assert_eq!(status_of(&r, "b.txt"), OpStatus::NotStarted);
    assert_eq!(status_of(&r, "c.txt"), OpStatus::NotStarted);
    assert!(matches!(r.report.outcome, EngineOutcome::Partial { .. }));
    for n in ["a.txt", "b.txt", "c.txt"] {
        assert!(rig.right.join(n).exists(), "{n} stays");
    }
}

// Only a side that is written needs an opened root (a read-only source keeps working).
#[test]
fn only_written_sides_open_a_root_handle() {
    let rig = Rig::new();
    write(&rig.left.join("a.txt"), b"a");
    let profile = rig.profile(one_way(), DeletePolicy::Permanent);
    let sink = NoSink;
    let env = test_env();
    let req = EngineRequest::new(&profile, &rig.baselines, &sink, &env);
    let mut dry = EngineRequest::new(&profile, &rig.baselines, &sink, &env);
    dry.dry_run = true;
    let report = run(&dry);
    assert!(matches!(report.outcome, EngineOutcome::DryRun { .. }));
    let _ = req;
    let entry = DirDiffEntry {
        name: "a.txt".into(),
        is_dir: false,
        state: DirDiffState::LeftOnly,
        left_size: 1,
        left_modified_ms: 0,
        right_size: 0,
        right_modified_ms: 0,
    };
    let op = |target| Op {
        index: 0,
        name: "a.txt".into(),
        action: SyncAction::CopyToRight,
        kind: OpKind::CopyNew,
        target,
        entry: entry.clone(),
        held: false,
    };
    assert_eq!(written_sides(&[op(Side::Right)]), [false, true]);
    assert_eq!(written_sides(&[op(Side::Left)]), [true, false]);
    assert_eq!(
        written_sides(&[op(Side::Left), op(Side::Right)]),
        [true, true]
    );
    let mut held = op(Side::Left);
    held.held = true;
    assert_eq!(written_sides(&[held]), [false, false]);
}

// RQ-9: every outcome kind is reachable and reaches the wire under its documented name.
#[test]
fn rq_9_every_outcome_reaches_the_wire_under_its_kind() {
    use crate::automation::manual_run::result_dto;
    use crate::sync_engine::errors::{Classified, ErrorClass};
    let kind = |r: &Ran| result_dto(&r.report).kind;

    // completed
    let rig = Rig::new();
    write(&rig.left.join("a.txt"), b"a");
    let profile = rig.profile(one_way(), DeletePolicy::Permanent);
    assert_eq!(kind(&go(&rig, &profile, &test_env())), "completed");

    // nothingToDo (the same pair again)
    assert_eq!(kind(&go(&rig, &profile, &test_env())), "nothingToDo");

    // conflictHeld
    let rig = Rig::new();
    write(&rig.left.join("c.txt"), b"left");
    write(&rig.right.join("c.txt"), b"right!");
    let two_way = SyncMode::TwoWay {
        propagate_deletes: false,
    };
    let profile = rig.profile(two_way, DeletePolicy::Permanent);
    assert_eq!(kind(&go(&rig, &profile, &test_env())), "conflictHeld");

    // partial (one item fails, one succeeds)
    let rig = Rig::new();
    write(&rig.left.join("bad.txt"), b"1");
    write(&rig.left.join("good.txt"), b"2");
    let profile = rig.profile(one_way(), DeletePolicy::Permanent);
    let copier = ScriptedCopier::new();
    copier.fail(
        "bad.txt",
        &[Classified {
            class: ErrorClass::PerItem,
            reason: Reason::InvalidName,
        }],
    );
    let mut env = test_env();
    env.copier = copier;
    let r = go(&rig, &profile, &env);
    assert_eq!(kind(&r), "partial");
    assert_eq!(result_dto(&r.report).error_code, Some("invalid_name"));

    // failed (the delete guard holds the run)
    let rig = Rig::new();
    write(&rig.right.join("precious.txt"), b"p");
    let profile = rig.profile(mirror(), DeletePolicy::Permanent);
    let r = go(&rig, &profile, &test_env());
    assert_eq!(kind(&r), "failed");
    assert_eq!(result_dto(&r.report).error_code, Some("empty_source"));

    // cancelled
    let rig = Rig::new();
    write(&rig.left.join("a.txt"), b"a");
    let profile = rig.profile(one_way(), DeletePolicy::Permanent);
    let token = CancelToken::default();
    token.cancel();
    let r = go_with(&rig, &profile, &test_env(), move |req| req.cancel = token);
    assert_eq!(kind(&r), "cancelled");

    // interrupted (a root is gone before the first operation)
    let rig = Rig::new();
    write(&rig.left.join("a.txt"), b"a");
    let profile = rig.profile(one_way(), DeletePolicy::Permanent);
    let mut env = test_env();
    env.liveness = Arc::new(DiesAfter {
        root: profile.right.path.clone(),
        remaining: std::sync::atomic::AtomicUsize::new(0),
    });
    let r = go(&rig, &profile, &env);
    assert_eq!(kind(&r), "interrupted");
    assert_eq!(
        result_dto(&r.report).interrupted,
        Some(InterruptReason::RootOffline)
    );
}

// T8, SE-4: a folder swapped in at the same path right after the gate is refused by the identity
// proof on the handle: nothing is deleted anywhere (the original is intact under its new name).
#[test]
fn a_root_swapped_after_the_gate_is_refused_by_the_identity_on_the_handle() {
    let rig = Rig::new();
    write(&rig.left.join("keep.txt"), b"k");
    write(
        &rig.right.join("x.txt"),
        b"x belongs to the original folder",
    );
    let profile = rig.profile(mirror(), DeletePolicy::Permanent);
    let hooks = Arc::new(TestHooks::default());
    let (right, old) = (rig.right.clone(), rig.base().join("right_old"));
    *hooks.after_gate.lock().unwrap() = Some(Box::new(move || {
        fs::rename(&right, &old).unwrap();
        fs::create_dir_all(&right).unwrap();
        fs::write(right.join("x.txt"), b"x in the impostor folder").unwrap();
        fs::write(right.join("y.txt"), b"y in the impostor folder").unwrap();
    }));
    let mut env = test_env();
    env.hooks = hooks;
    let r = go(&rig, &profile, &env);
    match &r.report.outcome {
        EngineOutcome::Failed { error } => assert_eq!(error.code, "identity_mismatch"),
        other => panic!("expected Failed, got {other:?}"),
    }
    assert_eq!(read(&rig.right.join("x.txt")), "x in the impostor folder");
    assert_eq!(read(&rig.right.join("y.txt")), "y in the impostor folder");
    assert_eq!(
        read(&rig.base().join("right_old").join("x.txt")),
        "x belongs to the original folder"
    );
}

// The pin also holds for the rest of the run: a root cannot be renamed away mid-run.
#[test]
fn a_pinned_root_cannot_be_renamed_while_the_run_holds_it() {
    let rig = Rig::new();
    write(&rig.left.join("a.txt"), b"a");
    let profile = rig.profile(one_way(), DeletePolicy::Permanent);
    let hooks = Arc::new(TestHooks::default());
    let (right, moved) = (rig.right.clone(), rig.base().join("right_moved"));
    let refused = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let r2 = refused.clone();
    *hooks.after_plan.lock().unwrap() = Some(Box::new(move || {
        r2.store(
            fs::rename(&right, &moved).is_err(),
            std::sync::atomic::Ordering::SeqCst,
        );
    }));
    let mut env = test_env();
    env.hooks = hooks;
    let r = go(&rig, &profile, &env);
    assert!(
        refused.load(std::sync::atomic::Ordering::SeqCst),
        "rename was refused"
    );
    assert!(matches!(r.report.outcome, EngineOutcome::Completed { .. }));
    assert_eq!(read(&rig.right.join("a.txt")), "a");
}

// SP-3.1: a shell that deletes for good instead of recycling is detected on the same object's
// final path; the run stops (recycle_bypassed), at most one file is affected.
#[test]
fn a_recycle_that_deleted_for_good_is_detected_and_stops_the_run() {
    let rig = Rig::new();
    write(&rig.left.join("keep.txt"), b"k");
    for n in ["a.txt", "b.txt", "c.txt"] {
        write(&rig.right.join(n), b"x");
    }
    let profile = rig.profile(mirror(), DeletePolicy::RecycleBin);
    let bin = FakeTrash::new(rig.base().join("bin"));
    bin.nuke.store(true, std::sync::atomic::Ordering::SeqCst);
    let mut env = test_env();
    env.trash = bin.clone();
    let r = go(&rig, &profile, &env);
    assert_eq!(status_of(&r, "a.txt"), OpStatus::Failed);
    assert_eq!(reason_of(&r, "a.txt"), Some(Reason::RecycleBypassed));
    assert_eq!(
        bin.trashed.lock().unwrap().len(),
        1,
        "only one file was affected"
    );
    assert!(rig.right.join("b.txt").exists() && rig.right.join("c.txt").exists());
    match &r.report.outcome {
        EngineOutcome::Failed { error } | EngineOutcome::Partial { error, .. } => {
            assert_eq!(error.code, "recycle_bypassed")
        }
        other => panic!("expected a stopped run, got {other:?}"),
    }
}

// FAT/exFAT: the shell deletes classically, the name stays while the engine's probe is open.
// The pending delete is detected, the run stops as recycle_bypassed, one file is affected.
#[test]
fn a_classic_pending_delete_is_detected_as_bypassed() {
    let rig = Rig::new();
    write(&rig.left.join("keep.txt"), b"k");
    for n in ["a.txt", "b.txt", "c.txt"] {
        write(&rig.right.join(n), b"x");
    }
    let profile = rig.profile(mirror(), DeletePolicy::RecycleBin);
    let bin = FakeTrash::new(rig.base().join("bin"));
    bin.classic.store(true, std::sync::atomic::Ordering::SeqCst);
    let mut env = test_env();
    env.trash = bin.clone();
    let r = go(&rig, &profile, &env);
    assert_eq!(status_of(&r, "a.txt"), OpStatus::Failed);
    assert_eq!(reason_of(&r, "a.txt"), Some(Reason::RecycleBypassed));
    assert_eq!(
        bin.trashed.lock().unwrap().len(),
        1,
        "only one file was affected"
    );
    assert!(rig.right.join("b.txt").exists() && rig.right.join("c.txt").exists());
    match &r.report.outcome {
        EngineOutcome::Failed { error } | EngineOutcome::Partial { error, .. } => {
            assert_eq!(error.code, "recycle_bypassed")
        }
        other => panic!("expected a stopped run, got {other:?}"),
    }
}

/// Mover that reports every file as a cloud placeholder and refuses to rename.
struct PlaceholderMover;

impl Mover for PlaceholderMover {
    fn rename_into(
        &self,
        _: &crate::archive_extract::publish::DirHandle,
        _: &std::ffi::OsStr,
        _: &crate::archive_extract::publish::DirHandle,
        _: &std::ffi::OsStr,
        _: crate::archive_extract::publish::FactsCheck<'_>,
    ) -> Result<bool, MoveError> {
        panic!("a cloud placeholder must not be renamed into the versions tree");
    }

    fn needs_copy(
        &self,
        _: &crate::archive_extract::publish::DirHandle,
        _: &std::ffi::OsStr,
    ) -> bool {
        true
    }
}

// A not-yet-downloaded cloud placeholder is versioned by copy-then-delete (the bytes are read),
// never by renaming the stub.
#[test]
fn a_cloud_placeholder_is_versioned_by_copy_then_delete() {
    let rig = Rig::new();
    write(&rig.left.join("keep.txt"), b"k");
    write(&rig.right.join("extra.txt"), b"cloud content");
    let profile = rig.profile(mirror(), rig.versioning());
    let mut env = test_env();
    env.mover = Arc::new(PlaceholderMover);
    let r = go(&rig, &profile, &env);
    assert!(
        matches!(r.report.outcome, EngineOutcome::Completed { .. }),
        "{:?}",
        r.report.outcome
    );
    assert!(!rig.right.join("extra.txt").exists());
    assert_eq!(
        read(
            &rig.versions
                .join("Right")
                .join("2026-01-02 030405 #7")
                .join("extra.txt")
        ),
        "cloud content"
    );
}

// An ordinary local file is not a placeholder (no attribute set): the real check says so.
#[test]
fn a_local_file_is_not_an_offline_placeholder() {
    let rig = Rig::new();
    write(&rig.right.join("f.txt"), b"x");
    let root = crate::archive_extract::publish::open_root(&rig.right).unwrap();
    assert!(!crate::archive_extract::publish::is_offline_placeholder(
        &root,
        std::ffi::OsStr::new("f.txt")
    ));
    assert!(crate::archive_extract::publish::is_offline_placeholder(
        &root,
        std::ffi::OsStr::new("missing.txt")
    ));
}

// SP-3.1: a volume whose bin is off (delete immediately, size 0, policy) or unreadable is
// refused by the gate before any operation.
#[test]
fn a_disabled_or_unknown_recycle_bin_is_refused_by_the_gate() {
    use crate::sync_engine::env::BinState;
    for state in [BinState::Disabled, BinState::Unknown] {
        let rig = Rig::new();
        write(&rig.left.join("keep.txt"), b"k");
        write(&rig.right.join("extra.txt"), b"must stay");
        let profile = rig.profile(mirror(), DeletePolicy::RecycleBin);
        let mut env = test_env();
        env.bin = Arc::new(FixedBin(state));
        let r = go(&rig, &profile, &env);
        match &r.report.outcome {
            EngineOutcome::Failed { error } => assert_eq!(error.code, "recycle_bin_unavailable"),
            other => panic!("expected Failed, got {other:?}"),
        }
        assert_eq!(read(&rig.right.join("extra.txt")), "must stay");
        assert!(!rig.right.join("keep.txt").exists());
    }
}

// A name swapped for a FOLDER between the check and the shell call never reaches the shell.
#[test]
fn a_file_swapped_for_a_folder_never_reaches_the_shell() {
    let rig = Rig::new();
    write(&rig.left.join("keep.txt"), b"k");
    write(&rig.right.join("extra.txt"), b"old");
    let profile = rig.profile(mirror(), DeletePolicy::RecycleBin);
    let bin = FakeTrash::new(rig.base().join("bin"));
    let hooks = Arc::new(TestHooks::default());
    let target = rig.right.join("extra.txt");
    *hooks.after_jit.lock().unwrap() = Some(Box::new(move |_, n| {
        if n == "extra.txt" {
            fs::remove_file(&target).unwrap();
            fs::create_dir_all(target.join("inner")).unwrap();
            fs::write(target.join("inner").join("f.txt"), b"precious").unwrap();
        }
    }));
    let mut env = test_env();
    env.trash = bin.clone();
    env.hooks = hooks;
    let r = go(&rig, &profile, &env);
    assert_eq!(status_of(&r, "extra.txt"), OpStatus::Failed);
    assert!(bin.trashed.lock().unwrap().is_empty());
    assert_eq!(
        read(&rig.right.join("extra.txt").join("inner").join("f.txt")),
        "precious"
    );
}

// Cost per operation: one watcher per RUN, not per operation (a thread plus its sleep per
// delete cost ~26 ms each). 1500 permanent deletes must finish well within a loose bound.
#[test]
fn many_deletes_cost_well_under_a_millisecond_of_overhead_each() {
    let rig = Rig::new();
    write(&rig.left.join("keep.txt"), b"k");
    for i in 0..1500 {
        write(&rig.right.join(format!("x{i:04}.txt")), b"x");
    }
    let mut profile = rig.profile(mirror(), DeletePolicy::Permanent);
    profile.guard.enabled = false;
    let started = std::time::Instant::now();
    let r = go(&rig, &profile, &test_env());
    let secs = started.elapsed().as_secs_f64();
    assert!(matches!(r.report.outcome, EngineOutcome::Completed { .. }));
    assert_eq!(tree(&rig.right), ["keep.txt"]);
    // 1500 x 26 ms would be 39 s; the run must stay under 15 s even on a slow disk.
    assert!(secs < 15.0, "{secs:.1} s for 1500 deletes");
}

// A link in the SOURCE is skipped with a note and does not hold the other deletes (it used to
// fail as a copy and withhold every delete in every run).
#[test]
fn a_link_in_the_source_is_skipped_and_does_not_hold_the_deletes() {
    let rig = Rig::new();
    write(&rig.left.join("keep.txt"), b"k");
    let outside = rig.base().join("outside");
    write(&outside.join("o.txt"), b"o");
    if !make_junction(&rig.left.join("J"), &outside) {
        eprintln!("SKIPPED: junction creation is not permitted here");
        return;
    }
    write(&rig.right.join("extra.txt"), b"x");
    let profile = rig.profile(mirror(), DeletePolicy::Permanent);
    let r = go(&rig, &profile, &test_env());
    assert_eq!(status_of(&r, "J"), OpStatus::Skipped);
    assert_eq!(reason_of(&r, "J"), Some(Reason::LinkUnsupported));
    assert_eq!(status_of(&r, "extra.txt"), OpStatus::Done);
    assert!(matches!(r.report.outcome, EngineOutcome::Completed { .. }));
    assert!(!rig.right.join("J").exists());
    assert_eq!(read(&outside.join("o.txt")), "o");
    let _ = fs::remove_dir(rig.left.join("J"));
}

// Only name-surrogate reparse tags are links; cloud placeholders (OneDrive), deduplication and
// the like are ordinary files for the engine (their tags are asserted here because a real
// placeholder cannot be made in a test).
#[test]
fn only_name_surrogate_reparse_tags_are_links() {
    use crate::archive_extract::publish::tag_is_link;
    assert!(tag_is_link(0xA000_000C), "symlink");
    assert!(tag_is_link(0xA000_0003), "mount point / junction");
    assert!(!tag_is_link(0x9000_001A), "cloud files placeholder");
    assert!(
        !tag_is_link(0x9000_701A),
        "cloud files placeholder (variant)"
    );
    assert!(!tag_is_link(0x8000_0013), "deduplication");
    assert!(!tag_is_link(0x8000_0017), "WOF / compressed");
}

// Q4: only an acknowledgement for THIS plan and THIS delete count lifts a tripped guard; an
// empty source can never be acknowledged.
#[test]
fn a_guard_acknowledgement_is_bound_to_the_plan_and_never_lifts_an_empty_source() {
    let rig = Rig::new();
    write(&rig.left.join("keep.txt"), b"k");
    for i in 0..3 {
        write(&rig.right.join(format!("x{i}.txt")), b"x");
    }
    let mut profile = rig.profile(mirror(), DeletePolicy::Permanent);
    profile.guard.max_count = 1;
    profile.guard.max_percent = 1;
    // The fingerprint of the previewed plan.
    let sink = NoSink;
    let env = test_env();
    let mut dry = EngineRequest::new(&profile, &rig.baselines, &sink, &env);
    dry.dry_run = true;
    let fp = run(&dry).fingerprint.unwrap();

    for ack in [
        GuardAck::from_native_yes("0".repeat(64), 3),
        GuardAck::from_native_yes(fp.clone(), 2),
    ] {
        let r = go_with(&rig, &profile, &env, |req| req.guard_ack = Some(ack));
        match &r.report.outcome {
            EngineOutcome::Failed { error } => assert_eq!(error.code, "guard_tripped"),
            other => panic!("expected Failed, got {other:?}"),
        }
        assert!(rig.right.join("x0.txt").exists());
    }
    let ack = GuardAck::from_native_yes(fp, 3);
    let r = go_with(&rig, &profile, &env, |req| req.guard_ack = Some(ack));
    assert!(matches!(r.report.outcome, EngineOutcome::Completed { .. }));

    // An empty source stays a hard stop with any acknowledgement.
    let rig = Rig::new();
    write(&rig.right.join("precious.txt"), b"p");
    let profile = rig.profile(mirror(), DeletePolicy::Permanent);
    let ack = GuardAck::from_native_yes("0".repeat(64), 1);
    let r = go_with(&rig, &profile, &env, |req| req.guard_ack = Some(ack));
    match &r.report.outcome {
        EngineOutcome::Failed { error } => assert_eq!(error.code, "empty_source"),
        other => panic!("expected Failed, got {other:?}"),
    }
    assert!(rig.right.join("precious.txt").exists());
}

// Q4: the acknowledgement is created in one place only (the manual run, after the native
// dialog). The host of automations (later) must never be able to lift a guard.
#[test]
fn only_the_manual_run_creates_a_guard_acknowledgement() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut stack = vec![root];
    let mut users = Vec::new();
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                if path.to_string_lossy().contains("_tests") {
                    continue;
                }
                let text = fs::read_to_string(&path).unwrap();
                let cut = [
                    "
#[cfg(test)]
#[path",
                    "
#[cfg(test)]
mod tests",
                    "
#[cfg(all(test",
                ]
                .iter()
                .filter_map(|m| text.find(m))
                .min()
                .unwrap_or(text.len());
                if text[..cut].contains("from_native_yes(") {
                    users.push(path.file_name().unwrap().to_string_lossy().into_owned());
                }
            }
        }
    }
    users.sort();
    assert_eq!(users, ["execute.rs", "manual_run.rs"]);
}

// RQ-1 matrix: two-way with deletion propagation x Recycle Bin / permanent. The user deletes a
// file on the right; the next run deletes it on the left under the profile's policy.
#[test]
fn two_way_propagation_with_recycle_bin_and_permanent() {
    for recycle in [true, false] {
        let rig = Rig::new();
        for side in [&rig.left, &rig.right] {
            write(&side.join("keep.txt"), b"keep");
            write(&side.join("gone.txt"), b"gone");
            set_mtime(&side.join("keep.txt"), 1_900_000_000);
            set_mtime(&side.join("gone.txt"), 1_900_000_000);
        }
        let policy = if recycle {
            DeletePolicy::RecycleBin
        } else {
            DeletePolicy::Permanent
        };
        let profile = rig.profile(
            SyncMode::TwoWay {
                propagate_deletes: true,
            },
            policy,
        );
        let bin = FakeTrash::new(rig.base().join("bin"));
        let mut env = test_env();
        env.trash = bin.clone();
        let r1 = go(&rig, &profile, &env);
        assert!(matches!(r1.report.outcome, EngineOutcome::NothingToDo));
        fs::remove_file(rig.right.join("gone.txt")).unwrap();
        let r2 = go(&rig, &profile, &env);
        assert!(
            matches!(r2.report.outcome, EngineOutcome::Completed { .. }),
            "{:?}",
            r2.report.outcome
        );
        assert!(!rig.left.join("gone.txt").exists());
        assert!(rig.left.join("keep.txt").exists() && rig.right.join("keep.txt").exists());
        assert_eq!(bin.trashed.lock().unwrap().len(), usize::from(recycle));
    }
}

// Files are deleted only on the receiving side of the mode: a (tampered) plan that deletes on the
// source side of a mirror is refused before any operation.
#[test]
fn a_delete_on_the_non_receiving_side_is_refused() {
    let rig = Rig::new();
    write(&rig.left.join("only-left.txt"), b"left file");
    write(&rig.right.join("x.txt"), b"x");
    let profile = rig.profile(mirror(), DeletePolicy::Permanent);
    let hooks = Arc::new(TestHooks::default());
    *hooks.final_plan.lock().unwrap() = Some(Box::new(|plan| {
        for item in plan.iter_mut() {
            if item.name == "only-left.txt" {
                item.action = SyncAction::DeleteLeft;
            }
        }
    }));
    let mut env = test_env();
    env.hooks = hooks;
    let r = go(&rig, &profile, &env);
    match &r.report.outcome {
        EngineOutcome::Failed { error } => assert_eq!(error.code, "plan_invalid"),
        other => panic!("expected Failed, got {other:?}"),
    }
    assert_eq!(read(&rig.left.join("only-left.txt")), "left file");
    assert_eq!(read(&rig.right.join("x.txt")), "x");
}

// The guard fraction counts files only (numerator and denominator agree): 3 deletes of 4 files
// trip it even though the target also holds many folders.
#[test]
fn guard_denominator_counts_files_only() {
    let rig = Rig::new();
    write(&rig.left.join("keep.txt"), b"k");
    set_mtime(&rig.left.join("keep.txt"), 1_900_000_000);
    write(&rig.right.join("keep.txt"), b"k");
    set_mtime(&rig.right.join("keep.txt"), 1_900_000_000);
    for i in 0..3 {
        write(&rig.right.join(format!("d{i}")).join("x.txt"), b"x");
    }
    for i in 0..30 {
        fs::create_dir_all(rig.right.join(format!("empty{i}"))).unwrap();
    }
    let mut profile = rig.profile(mirror(), DeletePolicy::Permanent);
    profile.guard.max_count = 1;
    profile.guard.max_percent = 50;
    let r = go(&rig, &profile, &test_env());
    match &r.report.outcome {
        EngineOutcome::Failed { error } => assert_eq!(error.code, "guard_tripped"),
        other => panic!("expected Failed, got {other:?}"),
    }
}
