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
        assert_eq!(op.status, OpStatus::Failed);
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
