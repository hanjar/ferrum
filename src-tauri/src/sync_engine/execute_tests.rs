//! Engine tests on temp trees (Automation P0.2). Every test names the requirement it proves.
//! Nothing here touches a folder of the user; the Recycle Bin is a temp folder (`FakeTrash`).

use super::*;
use crate::automation::model::{DeletePolicy, Side, SyncMode};
use crate::sync_engine::env::{Copier, CopyReport, EngineCopier};
use crate::sync_engine::errors::{ErrorClass, Reason};
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

/// Left: a.txt, sub/b.txt (new), c.txt (newer, other size), same.txt.
/// Right: c.txt (older), extra.txt, sub2/extra2.txt, emptyR/, same.txt.
fn seed_basic(rig: &Rig) {
    write(&rig.left.join("a.txt"), b"new-a");
    write(&rig.left.join("sub").join("b.txt"), b"new-b");
    write(&rig.left.join("c.txt"), b"left-c-new");
    write(&rig.left.join("same.txt"), b"same");
    write(&rig.right.join("c.txt"), b"right-c-old!");
    write(&rig.right.join("extra.txt"), b"x");
    write(&rig.right.join("sub2").join("extra2.txt"), b"y");
    fs::create_dir_all(rig.right.join("emptyR")).unwrap();
    write(&rig.right.join("same.txt"), b"same");
    for p in [
        rig.left.join("c.txt"),
        rig.left.join("same.txt"),
        rig.right.join("same.txt"),
    ] {
        set_mtime(&p, 1_900_000_000);
    }
    set_mtime(&rig.left.join("c.txt"), 1_900_000_500);
    set_mtime(&rig.right.join("c.txt"), 1_900_000_000);
}

struct Ran {
    report: EngineReport,
    sink: Recorder,
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
    Ran { report, sink }
}

fn completed(r: &Ran) -> Counts {
    match &r.report.outcome {
        EngineOutcome::Completed { counts } => *counts,
        other => panic!("expected Completed, got {other:?}"),
    }
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

const MIRRORED_RIGHT: [&str; 5] = ["a.txt", "c.txt", "same.txt", "sub/", "sub/b.txt"];

// ── Modes x delete policies ──

// SP-3.1, SP-3.4, RQ-1, design §5.5: Mirror with the Recycle Bin. Overwrites replace in place,
// deletes go to the bin one file at a time, emptied folders are removed non-recursively.
#[test]
fn mirror_with_recycle_bin_trees_file_by_file() {
    let rig = Rig::new();
    seed_basic(&rig);
    let profile = rig.profile(mirror(), DeletePolicy::RecycleBin);
    let bin = FakeTrash::new(rig.base().join("bin"));
    let mut env = test_env();
    env.trash = bin.clone();
    let r = go(&rig, &profile, &env);
    let counts = completed(&r);
    assert_eq!(counts.failed, 0);
    assert_eq!(tree(&rig.right), MIRRORED_RIGHT);
    assert_eq!(read(&rig.right.join("c.txt")), "left-c-new");
    assert_eq!(read(&rig.right.join("sub").join("b.txt")), "new-b");
    let mut trashed: Vec<String> = bin
        .trashed
        .lock()
        .unwrap()
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    trashed.sort();
    assert_eq!(trashed, ["extra.txt", "extra2.txt"]);
    // The left side is never touched.
    assert_eq!(
        tree(&rig.left),
        ["a.txt", "c.txt", "same.txt", "sub/", "sub/b.txt"]
    );
    // No versions folder was made for a Recycle Bin profile.
    assert!(!rig.versions.exists());
}

// SP-3.4, RQ-1: Mirror with permanent deletes.
#[test]
fn mirror_with_permanent_delete_trees_file_by_file() {
    let rig = Rig::new();
    seed_basic(&rig);
    let profile = rig.profile(mirror(), DeletePolicy::Permanent);
    let r = go(&rig, &profile, &test_env());
    completed(&r);
    assert_eq!(tree(&rig.right), MIRRORED_RIGHT);
    assert_eq!(read(&rig.right.join("c.txt")), "left-c-new");
}

// SP-3.2, SP-3.4, SE-9, design §5.5: versioning keeps deleted files AND overwritten files under
// `<root>\Right\<stamp> #<run>\<rel>`; same volume = rename by handle.
#[test]
fn mirror_with_versioning_keeps_deleted_and_overwritten_files() {
    let rig = Rig::new();
    seed_basic(&rig);
    let profile = rig.profile(mirror(), rig.versioning());
    let r = go(&rig, &profile, &test_env());
    completed(&r);
    assert_eq!(tree(&rig.right), MIRRORED_RIGHT);
    let run_dir = rig.versions.join("Right").join("2026-01-02 030405 #7");
    assert_eq!(
        tree(&rig.versions),
        [
            "Right/",
            "Right/2026-01-02 030405 #7/",
            "Right/2026-01-02 030405 #7/c.txt",
            "Right/2026-01-02 030405 #7/extra.txt",
            "Right/2026-01-02 030405 #7/sub2/",
            "Right/2026-01-02 030405 #7/sub2/extra2.txt",
        ]
    );
    // The old version of the overwritten file and the deleted ones, with their content.
    assert_eq!(read(&run_dir.join("c.txt")), "right-c-old!");
    assert_eq!(read(&run_dir.join("extra.txt")), "x");
    assert_eq!(read(&run_dir.join("sub2").join("extra2.txt")), "y");
    assert_eq!(read(&rig.right.join("c.txt")), "left-c-new");
}

// SP-2 / RQ-1: One-way never deletes, whatever the policy.
#[test]
fn one_way_copies_and_updates_but_never_deletes() {
    for policy in [DeletePolicy::Permanent, DeletePolicy::RecycleBin] {
        let rig = Rig::new();
        seed_basic(&rig);
        let profile = rig.profile(one_way(), policy);
        let bin = FakeTrash::new(rig.base().join("bin"));
        let mut env = test_env();
        env.trash = bin.clone();
        let r = go(&rig, &profile, &env);
        completed(&r);
        assert_eq!(
            tree(&rig.right),
            [
                "a.txt",
                "c.txt",
                "emptyR/",
                "extra.txt",
                "same.txt",
                "sub/",
                "sub/b.txt",
                "sub2/",
                "sub2/extra2.txt",
            ]
        );
        assert_eq!(read(&rig.right.join("c.txt")), "left-c-new");
        assert!(bin.trashed.lock().unwrap().is_empty());
    }
}

// SP-2.3 / RQ-1: two-way without a baseline copies the union and holds the real conflict.
#[test]
fn two_way_first_run_copies_the_union_and_holds_conflicts() {
    let rig = Rig::new();
    seed_basic(&rig);
    let profile = rig.profile(
        SyncMode::TwoWay {
            propagate_deletes: false,
        },
        DeletePolicy::Permanent,
    );
    let r = go(&rig, &profile, &test_env());
    match &r.report.outcome {
        EngineOutcome::ConflictHeld { counts } => assert_eq!(counts.failed, 0),
        other => panic!("expected ConflictHeld, got {other:?}"),
    }
    assert!(rig.left.join("extra.txt").exists());
    assert!(rig.left.join("sub2").join("extra2.txt").exists());
    assert!(rig.right.join("a.txt").exists());
    assert!(rig.right.join("sub").join("b.txt").exists());
    // c.txt differs on both sides with no baseline: held, neither side overwritten (RQ-6).
    assert_eq!(read(&rig.left.join("c.txt")), "left-c-new");
    assert_eq!(read(&rig.right.join("c.txt")), "right-c-old!");
    assert_eq!(status_of(&r, "c.txt"), OpStatus::Held);
    // The verified part was settled into the baseline.
    assert!(matches!(r.report.settle, SettleState::Saved { .. }));
}

// SP-2.3, design §5.5: two-way with deletion propagation deletes on EITHER side; the versions of
// a left-side delete live under `Left\`.
#[test]
fn two_way_propagation_versions_left_deletes_under_the_left_folder() {
    let rig = Rig::new();
    write(&rig.left.join("keep.txt"), b"keep");
    write(&rig.left.join("gone.txt"), b"gone");
    write(&rig.right.join("keep.txt"), b"keep");
    write(&rig.right.join("gone.txt"), b"gone");
    for n in ["keep.txt", "gone.txt"] {
        set_mtime(&rig.left.join(n), 1_900_000_000);
        set_mtime(&rig.right.join(n), 1_900_000_000);
    }
    let profile = rig.profile(
        SyncMode::TwoWay {
            propagate_deletes: true,
        },
        rig.versioning(),
    );
    // Run 1 records the baseline (everything in sync).
    let r1 = go(&rig, &profile, &test_env());
    assert!(matches!(r1.report.outcome, EngineOutcome::NothingToDo));
    assert!(matches!(r1.report.settle, SettleState::Saved { .. }));
    // The user deletes gone.txt on the RIGHT; run 2 propagates the delete to the left.
    fs::remove_file(rig.right.join("gone.txt")).unwrap();
    let r2 = go(&rig, &profile, &test_env());
    completed(&r2);
    assert!(!rig.left.join("gone.txt").exists());
    assert!(rig.left.join("keep.txt").exists());
    assert_eq!(
        read(
            &rig.versions
                .join("Left")
                .join("2026-01-02 030405 #7")
                .join("gone.txt")
        ),
        "gone"
    );
}

// ── Folders (design §5.4 execution contract) ──

// Contract 3: CreateDir is non-recursive mkdir, "already exists" is success; RemoveEmptyDir of a
// folder that is not empty (only excluded files inside) is skipped, never an error.
#[test]
fn empty_folder_actions_create_remove_and_skip_a_folder_with_hidden_content() {
    let rig = Rig::new();
    fs::create_dir_all(rig.left.join("deep").join("er")).unwrap();
    fs::create_dir_all(rig.right.join("goneR")).unwrap();
    fs::create_dir_all(rig.right.join("hasLog")).unwrap();
    write(&rig.right.join("hasLog").join("keep.log"), b"log");
    let mut profile = rig.profile(mirror(), DeletePolicy::Permanent);
    profile.filters.exclude = vec!["*.log".into()];
    let r = go(&rig, &profile, &test_env());
    completed(&r);
    assert_eq!(status_of(&r, "deep"), OpStatus::Done);
    assert_eq!(status_of(&r, "deep/er"), OpStatus::Done);
    assert_eq!(status_of(&r, "goneR"), OpStatus::Done);
    assert_eq!(status_of(&r, "hasLog"), OpStatus::Skipped);
    assert_eq!(reason_of(&r, "hasLog"), Some(Reason::NotEmpty));
    assert_eq!(
        tree(&rig.right),
        ["deep/", "deep/er/", "hasLog/", "hasLog/keep.log"]
    );
    assert_eq!(read(&rig.right.join("hasLog").join("keep.log")), "log");
}

// Contract 1/2: a FILE where a parent folder is needed fails the item and overwrites nothing; a
// junction in the parent path fails the item and nothing is written through it.
#[test]
fn a_file_or_junction_in_the_parent_path_fails_the_item_only() {
    let rig = Rig::new();
    write(&rig.left.join("sub").join("b.txt"), b"b");
    write(&rig.left.join("ok.txt"), b"ok");
    let outside = rig.base().join("outside");
    fs::create_dir_all(&outside).unwrap();
    let profile = rig.profile(one_way(), DeletePolicy::Permanent);
    let hooks = Arc::new(TestHooks::default());
    let (right, out2) = (rig.right.clone(), outside.clone());
    *hooks.after_plan.lock().unwrap() = Some(Box::new(move || {
        // Between plan and op: `sub` appears on the right as a FILE.
        fs::write(right.join("sub"), b"i am a file").unwrap();
        let _ = &out2;
    }));
    let mut env = test_env();
    env.hooks = hooks;
    let r = go(&rig, &profile, &env);
    // `sub` (CreateDir) saw the file: changed. `sub/b.txt` fails on the parent. ok.txt copies.
    assert_eq!(status_of(&r, "sub"), OpStatus::Changed);
    assert_eq!(status_of(&r, "sub/b.txt"), OpStatus::Failed);
    assert_eq!(reason_of(&r, "sub/b.txt"), Some(Reason::ParentIsFile));
    assert_eq!(status_of(&r, "ok.txt"), OpStatus::Done);
    assert_eq!(read(&rig.right.join("sub")), "i am a file");
    assert!(r.report.pending_rerun);
    assert!(matches!(r.report.outcome, EngineOutcome::Partial { .. }));

    // Junction variant.
    let rig = Rig::new();
    write(&rig.left.join("sub").join("b.txt"), b"b");
    let outside = rig.base().join("outside");
    fs::create_dir_all(&outside).unwrap();
    let profile = rig.profile(one_way(), DeletePolicy::Permanent);
    let hooks = Arc::new(TestHooks::default());
    let (right, out2) = (rig.right.clone(), outside.clone());
    let skipped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let sk = skipped.clone();
    *hooks.after_plan.lock().unwrap() = Some(Box::new(move || {
        if !make_junction(&right.join("sub"), &out2) {
            sk.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }));
    let mut env = test_env();
    env.hooks = hooks;
    let r = go(&rig, &profile, &env);
    if skipped.load(std::sync::atomic::Ordering::SeqCst) {
        eprintln!("SKIPPED: junction creation is not permitted here");
        return;
    }
    assert_eq!(status_of(&r, "sub/b.txt"), OpStatus::Failed);
    assert_eq!(reason_of(&r, "sub/b.txt"), Some(Reason::LinkInPath));
    assert_eq!(
        fs::read_dir(&outside).unwrap().count(),
        0,
        "nothing written through the junction"
    );
    let _ = fs::remove_dir(rig.right.join("sub"));
}

// Contract 3: RemoveEmptyDir is no-follow - a folder replaced by a junction between plan and op
// is skipped, never removed or followed.
#[test]
fn remove_empty_dir_skips_a_junction_planted_after_the_plan() {
    let rig = Rig::new();
    fs::create_dir_all(rig.right.join("D")).unwrap();
    write(&rig.left.join("x.txt"), b"x");
    let outside = rig.base().join("outside");
    write(&outside.join("precious.txt"), b"precious");
    let profile = rig.profile(mirror(), DeletePolicy::Permanent);
    let hooks = Arc::new(TestHooks::default());
    let (right, out2) = (rig.right.clone(), outside.clone());
    let planted = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let pl = planted.clone();
    *hooks.after_plan.lock().unwrap() = Some(Box::new(move || {
        fs::remove_dir(right.join("D")).unwrap();
        if !make_junction(&right.join("D"), &out2) {
            pl.store(false, std::sync::atomic::Ordering::SeqCst);
        }
    }));
    let mut env = test_env();
    env.hooks = hooks;
    let r = go(&rig, &profile, &env);
    if !planted.load(std::sync::atomic::Ordering::SeqCst) {
        eprintln!("SKIPPED: junction creation is not permitted here");
        return;
    }
    assert_eq!(status_of(&r, "D"), OpStatus::Skipped);
    assert_eq!(read(&outside.join("precious.txt")), "precious");
    assert!(
        rig.right.join("D").exists(),
        "the junction itself is left alone"
    );
    let _ = fs::remove_dir(rig.right.join("D"));
}

// Design §5.4 contract 6: the executor refuses a plan that breaks the folder rules,
// in release builds too (the hook tampers with the FINAL plan).
#[test]
fn a_plan_with_a_recursive_folder_action_is_refused_before_any_operation() {
    let rig = Rig::new();
    write(&rig.left.join("a.txt"), b"a");
    fs::create_dir_all(rig.left.join("folder")).unwrap();
    write(&rig.right.join("extra.txt"), b"extra");
    let profile = rig.profile(mirror(), DeletePolicy::Permanent);
    let hooks = Arc::new(TestHooks::default());
    *hooks.final_plan.lock().unwrap() = Some(Box::new(|plan| {
        for item in plan.iter_mut() {
            if item.is_dir {
                item.action = SyncAction::CopyToRight;
            }
        }
    }));
    let mut env = test_env();
    env.hooks = hooks;
    let r = go(&rig, &profile, &env);
    match &r.report.outcome {
        EngineOutcome::Failed { error } => assert_eq!(error.code, "plan_invalid"),
        other => panic!("expected Failed(plan_invalid), got {other:?}"),
    }
    // Nothing ran: no copy, no delete, no folder.
    assert_eq!(tree(&rig.right), ["extra.txt"]);
    assert!(r.sink.ops.lock().unwrap().is_empty());
}

// ── Guards (SP-5) ──

fn seed_deletes(rig: &Rig, kept: usize, extras: usize) {
    for i in 0..kept {
        write(&rig.left.join(format!("k{i}.txt")), b"k");
        write(&rig.right.join(format!("k{i}.txt")), b"k");
        set_mtime(&rig.left.join(format!("k{i}.txt")), 1_900_000_000);
        set_mtime(&rig.right.join(format!("k{i}.txt")), 1_900_000_000);
    }
    for i in 0..extras {
        write(&rig.right.join(format!("x{i}.txt")), b"x");
    }
}

// SP-5.1: 51 deletes of 100 entries trips the guard before ANY operation.
#[test]
fn guard_trips_before_any_operation() {
    let rig = Rig::new();
    seed_deletes(&rig, 49, 51);
    write(&rig.left.join("new.txt"), b"would be copied");
    let profile = rig.profile(mirror(), DeletePolicy::Permanent);
    let before = tree(&rig.right);
    let r = go(&rig, &profile, &test_env());
    match &r.report.outcome {
        EngineOutcome::Failed { error } => {
            assert_eq!(error.code, "guard_tripped");
            assert!(
                error.text.starts_with("Would delete 51 items (51 % of "),
                "{}",
                error.text
            );
            assert!(
                error
                    .text
                    .ends_with("over the safety limit. Review in Sync.")
            );
        }
        other => panic!("expected Failed, got {other:?}"),
    }
    assert_eq!(tree(&rig.right), before, "no copy happened either");
    assert!(
        r.sink.ops.lock().unwrap().is_empty(),
        "no operation started"
    );
}

// SP-5.1: exactly 50 deletes is within the limit and runs.
#[test]
fn guard_allows_exactly_the_limit() {
    let rig = Rig::new();
    seed_deletes(&rig, 50, 50);
    let profile = rig.profile(mirror(), DeletePolicy::Permanent);
    let r = go(&rig, &profile, &test_env());
    assert_eq!(completed(&r).done, 50);
    assert_eq!(tree(&rig.right).len(), 50);
}

// SP-5.2: an empty source with a populated target holds the run - guard on or off.
#[test]
fn empty_source_holds_the_run_even_with_the_guard_off() {
    for enabled in [true, false] {
        let rig = Rig::new();
        write(&rig.right.join("precious.txt"), b"precious");
        let mut profile = rig.profile(mirror(), DeletePolicy::Permanent);
        profile.guard.enabled = enabled;
        let r = go(&rig, &profile, &test_env());
        match &r.report.outcome {
            EngineOutcome::Failed { error } => {
                assert_eq!(error.code, "empty_source");
                assert_eq!(error.text, "Source folder is empty — nothing deleted");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        assert_eq!(read(&rig.right.join("precious.txt")), "precious");
    }
}

// SP-5.1: a guard that is off lets a big delete run (the profile owner turned it off natively).
#[test]
fn guard_off_lets_a_large_delete_run() {
    let rig = Rig::new();
    seed_deletes(&rig, 5, 60);
    let mut profile = rig.profile(mirror(), DeletePolicy::Permanent);
    profile.guard.enabled = false;
    let r = go(&rig, &profile, &test_env());
    assert_eq!(completed(&r).done, 60);
}

// ── Just-in-time check (RQ-7.5, T10) ──

// RQ-7.5: a source modified between plan and op is `changed`, the run continues, the pending
// flag is reported - and the changed file is NOT copied (stale facts).
#[test]
fn source_modified_between_plan_and_op_is_changed_and_the_run_continues() {
    let rig = Rig::new();
    write(&rig.left.join("a.txt"), b"first");
    write(&rig.left.join("b.txt"), b"second");
    let profile = rig.profile(one_way(), DeletePolicy::Permanent);
    let hooks = Arc::new(TestHooks::default());
    let left = rig.left.clone();
    *hooks.after_plan.lock().unwrap() = Some(Box::new(move || {
        fs::write(left.join("a.txt"), b"first, edited after the plan").unwrap();
    }));
    let mut env = test_env();
    env.hooks = hooks;
    let r = go(&rig, &profile, &env);
    assert_eq!(status_of(&r, "a.txt"), OpStatus::Changed);
    assert_eq!(status_of(&r, "b.txt"), OpStatus::Done);
    assert!(r.report.pending_rerun);
    assert!(!rig.right.join("a.txt").exists());
    assert_eq!(read(&rig.right.join("b.txt")), "second");
    assert!(matches!(r.report.outcome, EngineOutcome::Completed { .. }));
}

// RQ-7.5: a target that appeared after the plan is never overwritten by a "new" copy.
#[test]
fn target_appearing_after_the_plan_is_not_overwritten() {
    let rig = Rig::new();
    write(&rig.left.join("a.txt"), b"from left");
    let profile = rig.profile(one_way(), DeletePolicy::Permanent);
    let hooks = Arc::new(TestHooks::default());
    let right = rig.right.clone();
    *hooks.after_plan.lock().unwrap() = Some(Box::new(move || {
        fs::write(right.join("a.txt"), b"user wrote this meanwhile").unwrap();
    }));
    let mut env = test_env();
    env.hooks = hooks;
    let r = go(&rig, &profile, &env);
    assert_eq!(status_of(&r, "a.txt"), OpStatus::Changed);
    assert_eq!(read(&rig.right.join("a.txt")), "user wrote this meanwhile");
}

// RQ-7.5: a delete whose file changed after the plan is not executed.
#[test]
fn delete_of_a_file_modified_after_the_plan_is_not_executed() {
    let rig = Rig::new();
    write(&rig.left.join("keep.txt"), b"k");
    write(&rig.right.join("extra.txt"), b"old");
    let profile = rig.profile(mirror(), DeletePolicy::Permanent);
    let hooks = Arc::new(TestHooks::default());
    let right = rig.right.clone();
    *hooks.after_plan.lock().unwrap() = Some(Box::new(move || {
        fs::write(right.join("extra.txt"), b"the user just edited this file").unwrap();
    }));
    let mut env = test_env();
    env.hooks = hooks;
    let r = go(&rig, &profile, &env);
    assert_eq!(status_of(&r, "extra.txt"), OpStatus::Changed);
    assert_eq!(
        read(&rig.right.join("extra.txt")),
        "the user just edited this file"
    );
}

// ── Errors (RQ-7.1 - RQ-7.4, design §5.6) ──

fn transient() -> Classified {
    Classified {
        class: ErrorClass::Transient,
        reason: Reason::SharingViolation,
    }
}
fn denied() -> Classified {
    Classified {
        class: ErrorClass::Systemic,
        reason: Reason::AccessDenied,
    }
}
use crate::sync_engine::errors::Classified;

// RQ-7.1: a scripted sharing violation is retried twice (2 s, 10 s backoff) and then succeeds.
#[test]
fn sharing_violation_is_retried_twice_then_succeeds() {
    let rig = Rig::new();
    write(&rig.left.join("a.txt"), b"data");
    let profile = rig.profile(one_way(), DeletePolicy::Permanent);
    let copier = ScriptedCopier::new();
    copier.fail("a.txt", &[transient(), transient()]);
    let sleeper = Arc::new(InstantSleeper::default());
    let mut env = test_env();
    env.copier = copier.clone();
    env.sleeper = sleeper.clone();
    let r = go(&rig, &profile, &env);
    completed(&r);
    assert_eq!(status_of(&r, "a.txt"), OpStatus::Done);
    assert_eq!(read(&rig.right.join("a.txt")), "data");
    assert_eq!(
        *sleeper.slept.lock().unwrap(),
        [Duration::from_secs(2), Duration::from_secs(10)]
    );
}

// RQ-7.1: with the retries used up the op fails (per item) and the run goes on.
#[test]
fn retries_run_out_and_the_op_fails_while_the_run_continues() {
    let rig = Rig::new();
    write(&rig.left.join("a.txt"), b"data");
    write(&rig.left.join("b.txt"), b"data2");
    let profile = rig.profile(one_way(), DeletePolicy::Permanent);
    let copier = ScriptedCopier::new();
    copier.fail(
        "a.txt",
        &[transient(), transient(), transient(), transient()],
    );
    let mut env = test_env();
    env.copier = copier;
    let r = go(&rig, &profile, &env);
    assert_eq!(status_of(&r, "a.txt"), OpStatus::Failed);
    assert_eq!(reason_of(&r, "a.txt"), Some(Reason::SharingViolation));
    assert_eq!(status_of(&r, "b.txt"), OpStatus::Done);
    assert!(matches!(r.report.outcome, EngineOutcome::Partial { .. }));
}

// RQ-7.2: access denied stops the run at once - the remaining operations are not started and the
// outcome is Partial (Failed when nothing succeeded).
#[test]
fn access_denied_stops_the_run_with_the_rest_not_started() {
    let rig = Rig::new();
    write(&rig.left.join("a.txt"), b"1");
    write(&rig.left.join("b.txt"), b"2");
    write(&rig.left.join("z").join("c.txt"), b"3");
    let profile = rig.profile(one_way(), DeletePolicy::Permanent);
    let copier = ScriptedCopier::new();
    copier.fail("b.txt", &[denied()]);
    let mut env = test_env();
    env.copier = copier;
    let r = go(&rig, &profile, &env);
    assert_eq!(status_of(&r, "a.txt"), OpStatus::Done);
    assert_eq!(status_of(&r, "b.txt"), OpStatus::Failed);
    assert_eq!(status_of(&r, "z/c.txt"), OpStatus::NotStarted);
    match &r.report.outcome {
        EngineOutcome::Partial { counts, error } => {
            // The folder `z` and a.txt are done; b.txt failed.
            assert_eq!(counts.done, 2);
            assert_eq!(counts.failed, 1);
            assert!(counts.not_started >= 1);
            assert!(
                error
                    .text
                    .starts_with("Not retried automatically — permission errors need you.")
            );
        }
        other => panic!("expected Partial, got {other:?}"),
    }
    assert!(!rig.right.join("z").join("c.txt").exists());

    // Nothing succeeded before it: Failed.
    let rig = Rig::new();
    write(&rig.left.join("a.txt"), b"1");
    let profile = rig.profile(one_way(), DeletePolicy::Permanent);
    let copier = ScriptedCopier::new();
    copier.fail("a.txt", &[denied()]);
    let mut env = test_env();
    env.copier = copier;
    let r = go(&rig, &profile, &env);
    assert!(matches!(r.report.outcome, EngineOutcome::Failed { .. }));
}

// RQ-7.2 + design §5.4 step 4: when a copy failed, deletes do not run (same rule as runSyncPlan).
#[test]
fn deletes_wait_when_a_copy_failed() {
    let rig = Rig::new();
    write(&rig.left.join("a.txt"), b"1");
    write(&rig.left.join("k.txt"), b"k");
    write(&rig.right.join("k.txt"), b"k");
    set_mtime(&rig.left.join("k.txt"), 1_900_000_000);
    set_mtime(&rig.right.join("k.txt"), 1_900_000_000);
    write(&rig.right.join("extra.txt"), b"keep me");
    let profile = rig.profile(mirror(), DeletePolicy::Permanent);
    let copier = ScriptedCopier::new();
    copier.fail(
        "a.txt",
        &[Classified {
            class: ErrorClass::PerItem,
            reason: Reason::InvalidName,
        }],
    );
    let mut env = test_env();
    env.copier = copier;
    let r = go(&rig, &profile, &env);
    assert_eq!(status_of(&r, "a.txt"), OpStatus::Failed);
    assert_eq!(status_of(&r, "extra.txt"), OpStatus::NotStarted);
    assert_eq!(read(&rig.right.join("extra.txt")), "keep me");
}

// RQ-7.3: a per-item invalid name fails that op and the run continues.
#[test]
fn per_item_invalid_name_fails_the_op_and_the_run_continues() {
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
    assert_eq!(status_of(&r, "bad.txt"), OpStatus::Failed);
    assert_eq!(reason_of(&r, "bad.txt"), Some(Reason::InvalidName));
    assert_eq!(status_of(&r, "good.txt"), OpStatus::Done);
    assert!(matches!(r.report.outcome, EngineOutcome::Partial { .. }));
}

// RQ-7.3 / SE: a name that is not an ordinary file name (`..`, separators, device names) never
// reaches a handle open - the op fails and nothing outside the root is touched.
#[test]
fn hostile_names_fail_the_op_without_touching_anything() {
    let rig = Rig::new();
    write(&rig.base().join("victim.txt"), b"outside the roots");
    let profile = rig.profile(mirror(), DeletePolicy::Permanent);
    let roots = check_gate(&profile, &test_env()).unwrap();
    for name in [
        "../victim.txt",
        "a/../../victim.txt",
        "x\\y.txt",
        "CON",
        "a//b",
        "",
    ] {
        let entry = DirDiffEntry {
            name: name.into(),
            is_dir: false,
            state: DirDiffState::RightOnly,
            left_size: 0,
            left_modified_ms: 0,
            right_size: 1,
            right_modified_ms: 0,
        };
        let op = Op {
            index: 0,
            name: name.into(),
            action: SyncAction::DeleteRight,
            kind: OpKind::DeleteFile,
            target: Side::Right,
            entry,
            held: false,
        };
        let env = test_env();
        let sink = Recorder::default();
        let lease = rig.lease(&profile);
        let mut req = EngineRequest::new(&profile, &rig.baselines, &sink, &env);
        req.lease = Some(&lease);
        let handles = open_handles(&profile, &roots, [true, true]).unwrap();
        let records = initial_records(std::slice::from_ref(&op));
        let mut runner = Runner::new(&req, &roots, handles, None, vec![op], records);
        runner.execute();
        assert_eq!(runner.records[0].status, OpStatus::Failed, "{name:?}");
        assert_eq!(
            runner.records[0].reason,
            Some(Reason::InvalidName),
            "{name:?}"
        );
    }
    assert_eq!(read(&rig.base().join("victim.txt")), "outside the roots");
}

// A delete op that points at a FOLDER is never carried out (no recursive delete anywhere).
#[test]
fn a_delete_op_over_a_folder_never_removes_it() {
    let rig = Rig::new();
    write(&rig.right.join("dir").join("inner.txt"), b"inner");
    let profile = rig.profile(mirror(), DeletePolicy::Permanent);
    let roots = check_gate(&profile, &test_env()).unwrap();
    let entry = DirDiffEntry {
        name: "dir".into(),
        is_dir: false, // forged: the plan claims a file
        state: DirDiffState::RightOnly,
        left_size: 0,
        left_modified_ms: 0,
        right_size: 0,
        right_modified_ms: 0,
    };
    let op = Op {
        index: 0,
        name: "dir".into(),
        action: SyncAction::DeleteRight,
        kind: OpKind::DeleteFile,
        target: Side::Right,
        entry,
        held: false,
    };
    let env = test_env();
    let sink = Recorder::default();
    let lease = rig.lease(&profile);
    let mut req = EngineRequest::new(&profile, &rig.baselines, &sink, &env);
    req.lease = Some(&lease);
    let handles = open_handles(&profile, &roots, [true, true]).unwrap();
    let records = initial_records(std::slice::from_ref(&op));
    let mut runner = Runner::new(&req, &roots, handles, None, vec![op], records);
    runner.execute();
    assert_ne!(runner.records[0].status, OpStatus::Done);
    assert_eq!(read(&rig.right.join("dir").join("inner.txt")), "inner");
}

// RQ-7.4: a root that vanishes mid-run stops the run after the current file: Interrupted, the
// completed files stay, the automation is not flagged as failed.
#[test]
fn root_vanishing_mid_run_is_interrupted() {
    let rig = Rig::new();
    for n in ["a.txt", "b.txt", "c.txt"] {
        write(&rig.left.join("d").join(n), n.as_bytes());
    }
    for i in 0..3 {
        write(&rig.left.join(format!("z{i}")).join("f.txt"), b"z");
    }
    let profile = rig.profile(one_way(), DeletePolicy::Permanent);
    let mut env = test_env();
    // The right root answers "alive" for the first checks, then is gone.
    env.liveness = Arc::new(DiesAfter {
        root: profile.right.path.clone(),
        remaining: std::sync::atomic::AtomicUsize::new(4),
    });
    let r = go(&rig, &profile, &env);
    match &r.report.outcome {
        EngineOutcome::Interrupted { counts, why } => {
            assert_eq!(*why, InterruptReason::RootOffline);
            assert!(counts.done >= 1, "completed files stay: {counts:?}");
            assert!(counts.not_started >= 1);
        }
        other => panic!("expected Interrupted, got {other:?}"),
    }
}

// RQ-7.4 / T8: a root that is a DIFFERENT folder now (deleted and recreated) never runs.
#[test]
fn recreated_root_is_refused_by_the_identity_gate() {
    let rig = Rig::new();
    write(&rig.left.join("a.txt"), b"a");
    write(&rig.right.join("keep.txt"), b"keep");
    let profile = rig.profile(mirror(), DeletePolicy::Permanent);
    fs::remove_dir_all(&rig.right).unwrap();
    fs::create_dir_all(&rig.right).unwrap();
    let r = go(&rig, &profile, &test_env());
    match &r.report.outcome {
        EngineOutcome::Failed { error } => assert_eq!(error.code, "identity_mismatch"),
        other => panic!("expected Failed, got {other:?}"),
    }
    assert!(tree(&rig.right).is_empty());

    // A root that is gone.
    let rig = Rig::new();
    let profile = rig.profile(mirror(), DeletePolicy::Permanent);
    fs::remove_dir_all(&rig.right).unwrap();
    let r = go(&rig, &profile, &test_env());
    match &r.report.outcome {
        EngineOutcome::Failed { error } => assert!(
            matches!(error.code, "root_unavailable" | "identity_mismatch"),
            "{}",
            error.code
        ),
        other => panic!("expected Failed, got {other:?}"),
    }
}

// Design §5.6: three unknown failures in a row become systemic. Overwrites run one at a time,
// so the stop is visible between them.
#[test]
fn three_unknown_failures_in_a_row_stop_the_run() {
    let rig = Rig::new();
    for n in ["a", "b", "c", "d", "e"] {
        write(&rig.left.join(format!("{n}.txt")), b"new content");
        write(&rig.right.join(format!("{n}.txt")), b"old");
    }
    let profile = rig.profile(one_way(), DeletePolicy::Permanent);
    let unknown = Classified {
        class: ErrorClass::Unknown,
        reason: Reason::Unknown,
    };
    let copier = ScriptedCopier::new();
    for n in ["a", "b", "c"] {
        copier.fail(&format!("{n}.txt"), &[unknown]);
    }
    let mut env = test_env();
    env.copier = copier;
    let r = go(&rig, &profile, &env);
    assert!(
        matches!(r.report.outcome, EngineOutcome::Failed { .. }),
        "{:?}",
        r.report.outcome
    );
    assert_eq!(status_of(&r, "c.txt"), OpStatus::Failed);
    assert_eq!(status_of(&r, "d.txt"), OpStatus::NotStarted);
    assert_eq!(read(&rig.right.join("d.txt")), "old");

    // A success in between resets the streak.
    let rig = Rig::new();
    for n in ["a", "b", "c", "d", "e"] {
        write(&rig.left.join(format!("{n}.txt")), b"new content");
        write(&rig.right.join(format!("{n}.txt")), b"old");
    }
    let profile = rig.profile(one_way(), DeletePolicy::Permanent);
    let copier = ScriptedCopier::new();
    for n in ["a", "b", "d", "e"] {
        copier.fail(&format!("{n}.txt"), &[unknown]);
    }
    let mut env = test_env();
    env.copier = copier;
    let r = go(&rig, &profile, &env);
    assert_eq!(
        status_of(&r, "e.txt"),
        OpStatus::Failed,
        "no stop: e.txt was attempted"
    );
    assert!(matches!(r.report.outcome, EngineOutcome::Partial { .. }));
}

// ── Cancel (RQ-8.1) ──

// RQ-8.1: cancelling in the middle of a big copy leaves the file complete or absent, no staging
// leftover, outcome Cancelled; the files after it are not started.
/// The production copier, cancelled (user token and the copy's own token) by its first chunk.
struct CancelInFirstChunk {
    user: CancelToken,
    seen: Arc<std::sync::atomic::AtomicBool>,
}

impl Copier for CancelInFirstChunk {
    fn copy(
        &self,
        sources: &[String],
        dest_dir: &str,
        overwrite: bool,
        durable: bool,
        cancel: &CancelToken,
        guard: Option<&dyn crate::copy_engine::ReplaceGuard>,
    ) -> CopyReport {
        let (user, own, seen) = (self.user.clone(), cancel.clone(), self.seen.clone());
        let dir = Path::new(dest_dir);
        crate::copy_engine::hook_chunks(dir, move |_| {
            seen.store(true, std::sync::atomic::Ordering::SeqCst);
            user.cancel();
            own.cancel();
        });
        let report = EngineCopier.copy(sources, dest_dir, overwrite, durable, cancel, guard);
        crate::copy_engine::unhook_chunks(dir);
        report
    }
}

#[test]
fn cancel_mid_file_leaves_no_partial_file_and_no_staging_leftover() {
    let rig = Rig::new();
    // Sixteen chunks are plenty to be "in the middle of a file" when the cancel comes from the
    // copy's own first chunk (FER-566: this used to be 192 MB and a polling thread).
    const BIG: usize = 16 * crate::copy_engine::CHUNK;
    write(&rig.left.join("a-big.bin"), &vec![7u8; BIG]);
    write(&rig.left.join("z-after.txt"), b"after");
    let profile = rig.profile(one_way(), DeletePolicy::Permanent);
    let token = CancelToken::default();
    // The real copy engine cancels itself from inside its first chunk (user token and the copy's
    // own token together), so the cancel always hits a file in progress: deterministic, no polling
    // thread racing a fast disk.
    let saw_chunk = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut env = test_env();
    env.copier = Arc::new(CancelInFirstChunk {
        user: token.clone(),
        seen: saw_chunk.clone(),
    });
    let tk = token.clone();
    let r = go_with(&rig, &profile, &env, move |req| req.cancel = tk);
    assert!(
        saw_chunk.load(std::sync::atomic::Ordering::SeqCst),
        "a chunk was written, so the cancel hit a file in progress"
    );
    assert!(
        matches!(r.report.outcome, EngineOutcome::Cancelled { .. }),
        "{:?}",
        r.report.outcome
    );
    let leftovers: Vec<String> = tree(&rig.right)
        .into_iter()
        .filter(|n| n.contains(".ferrum-stage-"))
        .collect();
    assert!(leftovers.is_empty(), "staging leftovers: {leftovers:?}");
    if rig.right.join("a-big.bin").exists() {
        assert_eq!(
            fs::metadata(rig.right.join("a-big.bin")).unwrap().len(),
            BIG as u64
        );
    }
    assert!(!rig.right.join("z-after.txt").exists());
    assert_eq!(status_of(&r, "z-after.txt"), OpStatus::NotStarted);
}

// RQ-8.1: a cancel before the run starts changes nothing.
#[test]
fn cancel_before_the_first_operation_changes_nothing() {
    let rig = Rig::new();
    write(&rig.left.join("a.txt"), b"a");
    let profile = rig.profile(one_way(), DeletePolicy::Permanent);
    let token = CancelToken::default();
    token.cancel();
    let r = go_with(&rig, &profile, &test_env(), move |req| req.cancel = token);
    assert!(matches!(r.report.outcome, EngineOutcome::Cancelled { .. }));
    assert!(tree(&rig.right).is_empty());
}
