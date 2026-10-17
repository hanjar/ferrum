//! FER-564 (P0.2 review P3-2, P3-3): the commit-time re-check of an in-place overwrite and the
//! restore of a versioned file whose replacement did not land.

use super::*;
use crate::archive_extract::publish::{self, MoveError, PublishError};
use crate::automation::model::{DeletePolicy, Side, SyncMode};
use crate::sync_engine::env::{Copier, CopyReport, EngineCopier, Mover};
use crate::sync_engine::errors::{Classified, ErrorClass, Reason};
use crate::sync_engine::test_support::*;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

fn mirror() -> SyncMode {
    SyncMode::Mirror { from: Side::Left }
}
fn one_way() -> SyncMode {
    SyncMode::OneWay { from: Side::Left }
}
fn two_way() -> SyncMode {
    SyncMode::TwoWay {
        propagate_deletes: true,
    }
}

fn read(p: &Path) -> String {
    fs::read_to_string(p).unwrap()
}

fn go_with(
    rig: &Rig,
    profile: &ProfilePolicy,
    env: &EngineEnv,
    tweak: impl FnOnce(&mut EngineRequest<'_>),
) -> EngineReport {
    let lease = rig.lease(profile);
    let sink = NoSink;
    let mut req = EngineRequest::new(profile, &rig.baselines, &sink, env);
    req.run_id = 7;
    req.lease = Some(&lease);
    tweak(&mut req);
    run(&req)
}

fn go(rig: &Rig, profile: &ProfilePolicy, env: &EngineEnv) -> EngineReport {
    go_with(rig, profile, env, |_| {})
}

fn op<'a>(r: &'a EngineReport, name: &str) -> &'a OpRecord {
    r.ops
        .iter()
        .find(|o| o.name == name)
        .unwrap_or_else(|| panic!("no op for {name}: {:?}", r.ops))
}

/// Left `c.txt` is newer and longer than right `c.txt`: an overwrite to the right.
fn seed_overwrite(rig: &Rig) {
    write(&rig.left.join("c.txt"), b"left-c-new-content");
    write(&rig.right.join("c.txt"), b"right-c-old!");
    set_mtime(&rig.left.join("c.txt"), 1_900_000_500);
    set_mtime(&rig.right.join("c.txt"), 1_900_000_000);
}

fn hooked(before_commit: impl Fn(usize, &str) + Send + 'static) -> EngineEnv {
    let hooks = Arc::new(TestHooks::default());
    *hooks.before_commit.lock().unwrap() = Some(Box::new(before_commit));
    let mut env = test_env();
    env.hooks = hooks;
    env
}

fn leftovers(dir: &Path) -> Vec<String> {
    tree(dir)
        .into_iter()
        .filter(|n| n.contains(".ferrum-stage-") || n.contains(".ferrum-backup-"))
        .collect()
}

// ── P3-2: the target is checked again at the commit ──

// An in-place overwrite whose target is edited AFTER the just-in-time check (the long copy window)
// is not overwritten: the edit stays, the item is `changed` / `target_changed`, no staging file is
// left, and another comparison is due.
#[test]
fn a_target_edited_before_the_commit_is_not_overwritten_permanent() {
    let rig = Rig::new();
    seed_overwrite(&rig);
    let profile = rig.profile(one_way(), DeletePolicy::Permanent);
    let right = rig.right.clone();
    let env = hooked(move |_, _| {
        fs::write(right.join("c.txt"), b"the user's own edit, much longer").unwrap();
    });
    let r = go(&rig, &profile, &env);
    let rec = op(&r, "c.txt");
    assert_eq!(rec.status, OpStatus::Changed);
    assert_eq!(rec.reason, Some(Reason::TargetChanged));
    assert!(r.pending_rerun);
    assert_eq!(
        read(&rig.right.join("c.txt")),
        "the user's own edit, much longer"
    );
    assert_eq!(leftovers(&rig.right), Vec::<String>::new());
}

// Same under the Recycle Bin policy: the user's edit is neither replaced nor lost, and nothing
// was sent to the bin.
#[test]
fn a_target_edited_before_the_commit_is_not_overwritten_recycle_bin() {
    let rig = Rig::new();
    seed_overwrite(&rig);
    let profile = rig.profile(one_way(), DeletePolicy::RecycleBin);
    let bin = FakeTrash::new(rig.base().join("bin"));
    let right = rig.right.clone();
    let mut env = hooked(move |_, _| {
        fs::write(right.join("c.txt"), b"edited").unwrap();
    });
    env.trash = bin.clone();
    let r = go(&rig, &profile, &env);
    assert_eq!(op(&r, "c.txt").status, OpStatus::Changed);
    assert_eq!(op(&r, "c.txt").reason, Some(Reason::TargetChanged));
    assert_eq!(read(&rig.right.join("c.txt")), "edited");
    assert!(bin.trashed.lock().unwrap().is_empty());
    assert_eq!(leftovers(&rig.right), Vec::<String>::new());
}

// An edit that keeps the size and moves the time by one second is inside the plan's 2 s
// tolerance; the exact facts seen at the just-in-time check still catch it.
#[test]
fn a_same_size_edit_with_a_nearby_time_is_still_caught() {
    let rig = Rig::new();
    seed_overwrite(&rig);
    let profile = rig.profile(one_way(), DeletePolicy::Permanent);
    let right = rig.right.clone();
    let env = hooked(move |_, _| {
        let p = right.join("c.txt");
        fs::write(&p, b"RIGHT-C-OLD!").unwrap();
        set_mtime(&p, 1_900_000_001);
    });
    let r = go(&rig, &profile, &env);
    assert_eq!(op(&r, "c.txt").status, OpStatus::Changed);
    assert_eq!(op(&r, "c.txt").reason, Some(Reason::TargetChanged));
    assert_eq!(read(&rig.right.join("c.txt")), "RIGHT-C-OLD!");
}

// The target removed in the window is not recreated by the overwrite.
#[test]
fn a_target_removed_before_the_commit_is_not_recreated() {
    let rig = Rig::new();
    seed_overwrite(&rig);
    let profile = rig.profile(one_way(), DeletePolicy::Permanent);
    let right = rig.right.clone();
    let env = hooked(move |_, _| {
        fs::remove_file(right.join("c.txt")).unwrap();
    });
    let r = go(&rig, &profile, &env);
    assert_eq!(op(&r, "c.txt").status, OpStatus::Changed);
    assert_eq!(op(&r, "c.txt").reason, Some(Reason::TargetChanged));
    assert!(!rig.right.join("c.txt").exists());
    assert_eq!(leftovers(&rig.right), Vec::<String>::new());
}

// A target swapped for a folder in the window is left alone.
#[test]
fn a_target_swapped_for_a_folder_before_the_commit_is_left_alone() {
    let rig = Rig::new();
    seed_overwrite(&rig);
    let profile = rig.profile(one_way(), DeletePolicy::Permanent);
    let right = rig.right.clone();
    let env = hooked(move |_, _| {
        fs::remove_file(right.join("c.txt")).unwrap();
        fs::create_dir(right.join("c.txt")).unwrap();
    });
    let r = go(&rig, &profile, &env);
    assert_eq!(op(&r, "c.txt").status, OpStatus::Changed);
    assert!(rig.right.join("c.txt").is_dir());
}

// A program that has the target open for writing at the commit blocks the swap: the item fails
// as a sharing violation and the file is untouched.
#[test]
fn a_target_open_for_writing_at_the_commit_is_not_replaced() {
    let rig = Rig::new();
    seed_overwrite(&rig);
    let profile = rig.profile(one_way(), DeletePolicy::Permanent);
    let right = rig.right.clone();
    let held = Arc::new(std::sync::Mutex::new(None));
    let h = held.clone();
    let env = hooked(move |_, _| {
        let f = fs::OpenOptions::new()
            .write(true)
            .open(right.join("c.txt"))
            .unwrap();
        *h.lock().unwrap() = Some(f);
    });
    let r = go_with(&rig, &profile, &env, |req| req.retries = 0);
    drop(held);
    let rec = op(&r, "c.txt");
    assert_eq!(rec.status, OpStatus::Failed);
    assert_eq!(rec.reason, Some(Reason::SharingViolation));
    assert_eq!(read(&rig.right.join("c.txt")), "right-c-old!");
    assert_eq!(leftovers(&rig.right), Vec::<String>::new());
}

// The normal case still works: an untouched target is replaced, with no backup left behind and
// the new file's time carried over.
#[test]
fn an_unchanged_target_is_replaced_and_leaves_nothing_behind() {
    let rig = Rig::new();
    seed_overwrite(&rig);
    let profile = rig.profile(one_way(), DeletePolicy::Permanent);
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let env = hooked(move |_, _| {
        c.fetch_add(1, Ordering::SeqCst);
    });
    let r = go(&rig, &profile, &env);
    assert!(
        matches!(r.outcome, EngineOutcome::Completed { .. }),
        "{:?}",
        r.outcome
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1, "the commit hook ran once");
    assert_eq!(read(&rig.right.join("c.txt")), "left-c-new-content");
    assert_eq!(leftovers(&rig.right), Vec::<String>::new());
    let (l, rt) = (
        fs::metadata(rig.left.join("c.txt")).unwrap(),
        fs::metadata(rig.right.join("c.txt")).unwrap(),
    );
    assert_eq!(l.modified().unwrap(), rt.modified().unwrap());
}

// A new file (no target) never goes through the swap.
#[test]
fn a_new_file_does_not_use_the_commit_check() {
    let rig = Rig::new();
    write(&rig.left.join("n.txt"), b"new");
    let profile = rig.profile(one_way(), DeletePolicy::Permanent);
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let env = hooked(move |_, _| {
        c.fetch_add(1, Ordering::SeqCst);
    });
    let r = go(&rig, &profile, &env);
    assert!(matches!(r.outcome, EngineOutcome::Completed { .. }));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(read(&rig.right.join("n.txt")), "new");
}

// ── P3-3: a versioned file whose replacement did not land goes back ──

fn per_item() -> Classified {
    Classified {
        class: ErrorClass::PerItem,
        reason: Reason::VerifyFailed,
    }
}

fn same_dir(a: &str, b: &Path) -> bool {
    a.trim_end_matches('\\')
        .eq_ignore_ascii_case(b.to_string_lossy().trim_end_matches('\\'))
}

/// Copier whose first copy into `dir` fails (or is cancelled); everything else is real.
struct FailFirstInto {
    dir: std::path::PathBuf,
    cancel: Option<CancelToken>,
    hits: AtomicUsize,
}

impl FailFirstInto {
    fn failing(dir: &Path) -> Arc<Self> {
        Arc::new(FailFirstInto {
            dir: dir.to_path_buf(),
            cancel: None,
            hits: AtomicUsize::new(0),
        })
    }
    fn cancelling(dir: &Path, token: CancelToken) -> Arc<Self> {
        Arc::new(FailFirstInto {
            dir: dir.to_path_buf(),
            cancel: Some(token),
            hits: AtomicUsize::new(0),
        })
    }
}

impl Copier for FailFirstInto {
    fn copy(
        &self,
        sources: &[String],
        dest_dir: &str,
        overwrite: bool,
        durable: bool,
        cancel: &CancelToken,
        guard: Option<&dyn crate::copy_engine::ReplaceGuard>,
    ) -> CopyReport {
        if same_dir(dest_dir, &self.dir) && self.hits.fetch_add(1, Ordering::SeqCst) == 0 {
            if let Some(t) = &self.cancel {
                t.cancel();
                return CopyReport {
                    cancelled: true,
                    ..CopyReport::default()
                };
            }
            return CopyReport {
                failed: vec![(sources[0].clone(), per_item())],
                ..CopyReport::default()
            };
        }
        EngineCopier.copy(sources, dest_dir, overwrite, durable, cancel, guard)
    }
}

/// Mover that behaves like the real one, except that every call from the `from`-th on fails.
struct FailRenamesFrom {
    from: usize,
    calls: AtomicUsize,
}

impl Mover for FailRenamesFrom {
    fn rename_into(
        &self,
        src_parent: &publish::DirHandle,
        src_name: &std::ffi::OsStr,
        dst_parent: &publish::DirHandle,
        dst_name: &std::ffi::OsStr,
        check: publish::FactsCheck<'_>,
    ) -> Result<bool, MoveError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if n >= self.from {
            return Err(MoveError::Publish(PublishError::Io("injected".into())));
        }
        publish::rename_file_into_if(src_parent, src_name, dst_parent, dst_name, check)
    }
}

struct CrossVolume;

impl Mover for CrossVolume {
    fn rename_into(
        &self,
        _: &publish::DirHandle,
        _: &std::ffi::OsStr,
        _: &publish::DirHandle,
        _: &std::ffi::OsStr,
        _: publish::FactsCheck<'_>,
    ) -> Result<bool, MoveError> {
        Err(MoveError::CrossDevice)
    }
}

fn versions_listing(rig: &Rig) -> Vec<String> {
    tree(&rig.versions)
        .into_iter()
        .filter(|n| !n.ends_with('/'))
        .collect()
}

fn old_file_is_intact(rig: &Rig) {
    let p = rig.right.join("c.txt");
    assert_eq!(read(&p), "right-c-old!");
    let secs = fs::metadata(&p)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert_eq!(secs, 1_900_000_000, "the old file keeps its time");
    assert_eq!(leftovers(&rig.right), Vec::<String>::new());
}

// A copy that fails after the old file went to the versions tree puts the old file back, whole.
#[test]
fn a_failed_versioning_copy_puts_the_old_file_back() {
    let rig = Rig::new();
    seed_overwrite(&rig);
    let profile = rig.profile(mirror(), rig.versioning());
    let mut env = test_env();
    env.copier = FailFirstInto::failing(&rig.right);
    let r = go(&rig, &profile, &env);
    let rec = op(&r, "c.txt");
    assert_eq!(rec.status, OpStatus::Failed);
    assert_eq!(rec.reason, Some(Reason::VerifyFailed));
    old_file_is_intact(&rig);
    assert_eq!(
        versions_listing(&rig),
        Vec::<String>::new(),
        "nothing stays aside"
    );
}

// A cancelled copy does the same, and the run is reported cancelled.
#[test]
fn a_cancelled_versioning_copy_puts_the_old_file_back() {
    let rig = Rig::new();
    seed_overwrite(&rig);
    let profile = rig.profile(mirror(), rig.versioning());
    let token = CancelToken::default();
    let mut env = test_env();
    env.copier = FailFirstInto::cancelling(&rig.right, token.clone());
    let r = go_with(&rig, &profile, &env, move |req| req.cancel = token);
    assert!(
        matches!(r.outcome, EngineOutcome::Cancelled { .. }),
        "{:?}",
        r.outcome
    );
    old_file_is_intact(&rig);
    assert_eq!(versions_listing(&rig), Vec::<String>::new());
}

// On another volume the old file is copied back (size verified) and the version is dropped.
#[test]
fn cross_volume_restore_copies_the_old_file_back() {
    let rig = Rig::new();
    seed_overwrite(&rig);
    let profile = rig.profile(mirror(), rig.versioning());
    let mut env = test_env();
    env.mover = Arc::new(CrossVolume);
    // The copy into the versions folder is a copy into another folder, so only the overwrite
    // itself (the first copy into the right root) fails.
    env.copier = FailFirstInto::failing(&rig.right);
    let r = go(&rig, &profile, &env);
    assert_eq!(op(&r, "c.txt").status, OpStatus::Failed);
    assert_eq!(read(&rig.right.join("c.txt")), "right-c-old!");
    assert_eq!(leftovers(&rig.right), Vec::<String>::new());
    assert_eq!(versions_listing(&rig), Vec::<String>::new());
}

/// Mover that does the real move, then has "the user" write a new file at the old place before
/// the restore runs (the restore's own rename must then refuse to replace it).
struct Newcomer(std::path::PathBuf);

impl Mover for Newcomer {
    fn rename_into(
        &self,
        a: &publish::DirHandle,
        b: &std::ffi::OsStr,
        c: &publish::DirHandle,
        d: &std::ffi::OsStr,
        check: publish::FactsCheck<'_>,
    ) -> Result<bool, MoveError> {
        let moved = publish::rename_file_into_if(a, b, c, d, check);
        if moved.as_ref().is_ok_and(|m| *m) && !self.0.join("c.txt").exists() {
            fs::write(self.0.join("c.txt"), "user's new file").unwrap();
        }
        moved
    }
}

// A name taken meanwhile is never replaced by the restore; the old version stays in the tree.
#[test]
fn a_restore_never_replaces_a_file_that_took_the_place() {
    let rig = Rig::new();
    seed_overwrite(&rig);
    let profile = rig.profile(mirror(), rig.versioning());
    let mut env = test_env();
    env.copier = FailFirstInto::failing(&rig.right);
    env.mover = Arc::new(Newcomer(rig.right.clone()));
    let r = go(&rig, &profile, &env);
    assert_eq!(op(&r, "c.txt").status, OpStatus::Failed);
    assert_eq!(read(&rig.right.join("c.txt")), "user's new file");
    assert_eq!(versions_listing(&rig).len(), 1, "the old version is kept");
}

// A restore that fails is loud: the run stops with a distinct reason (attention), the old file is
// safe in the versions tree, and a later run neither deletes nor loses anything.
#[test]
fn a_failed_restore_stops_the_run_and_keeps_the_old_file_in_the_versions_tree() {
    let rig = Rig::new();
    seed_overwrite(&rig);
    write(&rig.left.join("z.txt"), b"later");
    let profile = rig.profile(mirror(), rig.versioning());
    let mut env = test_env();
    env.copier = FailFirstInto::failing(&rig.right);
    env.mover = Arc::new(FailRenamesFrom {
        from: 2,
        calls: AtomicUsize::new(0),
    });
    let r = go(&rig, &profile, &env);
    let rec = op(&r, "c.txt");
    assert_eq!(rec.status, OpStatus::Failed);
    assert_eq!(rec.reason, Some(Reason::RestoreFailed));
    assert_eq!(op(&r, "z.txt").status, OpStatus::NotStarted);
    match &r.outcome {
        EngineOutcome::Failed { error } | EngineOutcome::Partial { error, .. } => {
            assert_eq!(error.code, "restore_failed");
        }
        other => panic!("expected an attention outcome, got {other:?}"),
    }
    assert!(!rig.right.join("c.txt").exists());
    let kept = versions_listing(&rig);
    assert_eq!(kept.len(), 1);
    assert_eq!(read(&rig.versions.join(&kept[0])), "right-c-old!");
    // The next mirror run copies the file again and deletes nothing.
    let again = go(&rig, &profile, &test_env());
    assert!(
        matches!(again.outcome, EngineOutcome::Completed { .. }),
        "{:?}",
        again.outcome
    );
    assert_eq!(read(&rig.right.join("c.txt")), "left-c-new-content");
    assert_eq!(read(&rig.left.join("c.txt")), "left-c-new-content");
    assert_eq!(read(&rig.left.join("z.txt")), "later");
}

// Same failure after a cancel: still attention, not a quiet "cancelled".
#[test]
fn a_failed_restore_after_a_cancel_is_still_reported() {
    let rig = Rig::new();
    seed_overwrite(&rig);
    let profile = rig.profile(mirror(), rig.versioning());
    let token = CancelToken::default();
    let mut env = test_env();
    env.copier = FailFirstInto::cancelling(&rig.right, token.clone());
    env.mover = Arc::new(FailRenamesFrom {
        from: 2,
        calls: AtomicUsize::new(0),
    });
    let r = go_with(&rig, &profile, &env, move |req| req.cancel = token);
    assert_eq!(op(&r, "c.txt").reason, Some(Reason::RestoreFailed));
    match &r.outcome {
        EngineOutcome::Failed { error } | EngineOutcome::Partial { error, .. } => {
            assert_eq!(error.code, "restore_failed");
        }
        other => panic!("expected an attention outcome, got {other:?}"),
    }
}

// Two-way with deletion propagation: after a failed restore the target is missing and the source
// changed since the baseline, so the next run HOLDS the item as a conflict - it never deletes the
// source, never invents a deletion.
#[test]
fn two_way_after_a_failed_restore_holds_a_conflict_and_deletes_nothing() {
    let rig = Rig::new();
    for side in [&rig.left, &rig.right] {
        write(&side.join("c.txt"), b"right-c-old!");
        set_mtime(&side.join("c.txt"), 1_900_000_000);
    }
    let profile = rig.profile(two_way(), rig.versioning());
    // Run 1 records the baseline.
    let r1 = go(&rig, &profile, &test_env());
    assert!(
        matches!(r1.outcome, EngineOutcome::NothingToDo),
        "{:?}",
        r1.outcome
    );
    assert!(matches!(r1.settle, SettleState::Saved { .. }));
    // The user edits the left file; run 2 overwrites to the right, the copy fails, the restore
    // fails.
    write(&rig.left.join("c.txt"), b"left-c-new-content");
    set_mtime(&rig.left.join("c.txt"), 1_900_000_500);
    let mut env = test_env();
    env.copier = FailFirstInto::failing(&rig.right);
    env.mover = Arc::new(FailRenamesFrom {
        from: 2,
        calls: AtomicUsize::new(0),
    });
    let r2 = go(&rig, &profile, &env);
    assert_eq!(op(&r2, "c.txt").reason, Some(Reason::RestoreFailed));
    assert!(!rig.right.join("c.txt").exists());
    // Run 3, a healthy environment.
    let r3 = go(&rig, &profile, &test_env());
    assert_eq!(op(&r3, "c.txt").status, OpStatus::Held);
    assert!(
        matches!(r3.outcome, EngineOutcome::ConflictHeld { .. }),
        "{:?}",
        r3.outcome
    );
    assert_eq!(read(&rig.left.join("c.txt")), "left-c-new-content");
    assert!(!rig.right.join("c.txt").exists());
    assert_eq!(
        versions_listing(&rig).len(),
        1,
        "the old version is still kept"
    );
    // No delete was versioned from the left folder.
    assert!(!rig.versions.join("Left").exists());
}

// ── rework of the Opus review (round 1) ──

// S1: only folders and LINKS (name-surrogate reparse tags) are refused as the target of the commit.
// A OneDrive/cloud placeholder or a deduplicated file is an ordinary file, as in the plan and the
// just-in-time check (their tags are asserted because a real placeholder cannot be made here).
#[test]
fn the_commit_refuses_folders_and_links_but_not_other_reparse_data() {
    use crate::archive_extract::publish::overwrite_target_refused as refused;
    const DIR: u32 = 0x10;
    const REPARSE: u32 = 0x400;
    const FILE: u32 = 0x20;
    assert!(refused(DIR, 0), "folder");
    assert!(refused(DIR | REPARSE, 0xA000_0003), "folder junction");
    assert!(
        refused(FILE | REPARSE, 0xA000_000C),
        "IO_REPARSE_TAG_SYMLINK"
    );
    assert!(
        refused(FILE | REPARSE, 0xA000_0003),
        "IO_REPARSE_TAG_MOUNT_POINT"
    );
    assert!(!refused(FILE, 0), "plain file");
    assert!(
        !refused(FILE | REPARSE, 0x9000_001A),
        "IO_REPARSE_TAG_CLOUD"
    );
    assert!(
        !refused(FILE | REPARSE, 0x9000_701A),
        "IO_REPARSE_TAG_CLOUD_7000"
    );
    assert!(
        !refused(FILE | REPARSE, 0x8000_0013),
        "IO_REPARSE_TAG_DEDUP"
    );
    assert!(!refused(FILE | REPARSE, 0x8000_0017), "IO_REPARSE_TAG_WOF");
    assert!(
        !refused(FILE, 0xA000_000C),
        "a stale tag value without the reparse attribute means nothing"
    );
}

// S2: a target replaced by another file with the same size and time (save-via-rename that keeps the
// time) is a different object: it is not overwritten.
#[test]
fn a_target_replaced_by_another_file_with_the_same_size_and_time_is_caught() {
    let rig = Rig::new();
    seed_overwrite(&rig);
    let profile = rig.profile(one_way(), DeletePolicy::Permanent);
    let right = rig.right.clone();
    let env = hooked(move |_, _| {
        let tmp = right.join("swap.tmp");
        fs::write(&tmp, b"OTHER-C-OLD!").unwrap();
        set_mtime(&tmp, 1_900_000_000);
        fs::rename(&tmp, right.join("c.txt")).unwrap();
    });
    let r = go(&rig, &profile, &env);
    let rec = op(&r, "c.txt");
    assert_eq!(rec.status, OpStatus::Changed);
    assert_eq!(rec.reason, Some(Reason::TargetChanged));
    assert!(r.pending_rerun);
    assert_eq!(read(&rig.right.join("c.txt")), "OTHER-C-OLD!");
    assert_eq!(leftovers(&rig.right), Vec::<String>::new());
}

// S3: the old file was moved aside, the new one could not be published and the old one could not
// be put back because a newcomer took its name. Nothing is overwritten or deleted, the old file is
// kept under its backup name, and the run stops with its own reason (not an ordinary copy error).
#[test]
fn an_old_file_left_under_its_backup_name_stops_the_run_with_its_own_reason() {
    let rig = Rig::new();
    seed_overwrite(&rig);
    write(&rig.left.join("z.txt"), b"later");
    let profile = rig.profile(mirror(), DeletePolicy::Permanent);
    let dest = rig.right.join("c.txt");
    let d = dest.clone();
    crate::archive_extract::publish::fault::after_aside(&dest, move || {
        fs::write(&d, b"the newcomer").unwrap();
    });
    let r = go(&rig, &profile, &test_env());
    let rec = op(&r, "c.txt");
    assert_eq!(rec.status, OpStatus::Failed);
    assert_eq!(rec.reason, Some(Reason::BackupLeft));
    assert_eq!(op(&r, "z.txt").status, OpStatus::NotStarted);
    match &r.outcome {
        EngineOutcome::Failed { error } | EngineOutcome::Partial { error, .. } => {
            assert_eq!(error.code, "backup_left");
            assert!(!error.text.contains('\\'), "{}", error.text);
        }
        other => panic!("expected an attention outcome, got {other:?}"),
    }
    // The newcomer is untouched; the old file is intact under its backup name; no staging file.
    assert_eq!(read(&dest), "the newcomer");
    let names = tree(&rig.right);
    let backups: Vec<_> = names
        .iter()
        .filter(|n| n.contains(".ferrum-backup-"))
        .collect();
    assert_eq!(backups.len(), 1, "{names:?}");
    assert_eq!(read(&rig.right.join(backups[0])), "right-c-old!");
    assert!(
        !names.iter().any(|n| n.contains(".ferrum-stage-")),
        "{names:?}"
    );
}

// N4: the swap of a target above MAX_PATH works (the old rename path handled it). The rest of the
// engine does not walk such paths, so the commit check is driven directly.
#[test]
fn the_commit_swap_works_above_max_path() {
    use crate::copy_engine::ReplaceGuard;
    use crate::sync_engine::commit_check::CommitCheck;
    use crate::sync_engine::env::NoHooks;
    use crate::sync_engine::versioning::Expect;
    let rig = Rig::new();
    let deep: String = (0..6).map(|i| format!("{}{i}/", "d".repeat(50))).collect();
    let dir = rig.right.join(&deep);
    let (dest, staged) = (dir.join("c.txt"), dir.join(".c.txt.ferrum-stage-1"));
    assert!(dest.to_string_lossy().len() > 260, "path is long enough");
    write(&dest, b"right-c-old!");
    write(&staged, b"left-c-new-content");
    set_mtime(&dest, 1_900_000_000);
    let m = fs::metadata(&dest).unwrap();
    let plan = Expect {
        size: m.len(),
        mtime_ms: crate::fs_service::modified_ms(&m),
    };
    let check = CommitCheck::new(&NoHooks, 0, "c.txt", plan, None, None);
    assert!(
        publish::file_id_of_path(&crate::sync_engine::commit_check::extended(&dest)).is_some(),
        "the identity of a long path is read in the verbatim form"
    );
    check.replace(&staged, &dest).unwrap();
    assert_eq!(read(&dest), "left-c-new-content");
    assert!(!staged.exists());
    assert_eq!(leftovers(&rig.right), Vec::<String>::new());
}

// N1: the first copy (the overwrite) fails; the restore copies the old version back across
// volumes, and that copy comes out short.
struct FailThenShort {
    dir: std::path::PathBuf,
    keep_time: bool,
    hits: AtomicUsize,
}

impl Copier for FailThenShort {
    fn copy(
        &self,
        sources: &[String],
        dest_dir: &str,
        overwrite: bool,
        durable: bool,
        cancel: &CancelToken,
        guard: Option<&dyn crate::copy_engine::ReplaceGuard>,
    ) -> CopyReport {
        if !same_dir(dest_dir, &self.dir) {
            return EngineCopier.copy(sources, dest_dir, overwrite, durable, cancel, guard);
        }
        if self.hits.fetch_add(1, Ordering::SeqCst) == 0 {
            return CopyReport {
                failed: vec![(sources[0].clone(), per_item())],
                ..CopyReport::default()
            };
        }
        let report = EngineCopier.copy(sources, dest_dir, overwrite, durable, cancel, guard);
        let name = Path::new(&sources[0]).file_name().unwrap();
        let f = fs::OpenOptions::new()
            .write(true)
            .open(Path::new(dest_dir).join(name))
            .unwrap();
        let before = f.metadata().unwrap().modified().unwrap();
        f.set_len(0).unwrap();
        if self.keep_time {
            f.set_modified(before).unwrap();
        }
        report
    }
}

fn short_restore(keep_time: bool) -> (Rig, EngineReport) {
    let rig = Rig::new();
    seed_overwrite(&rig);
    let profile = rig.profile(mirror(), rig.versioning());
    let mut env = test_env();
    env.mover = Arc::new(CrossVolume);
    env.copier = Arc::new(FailThenShort {
        dir: rig.right.clone(),
        keep_time,
        hits: AtomicUsize::new(0),
    });
    let r = go(&rig, &profile, &env);
    (rig, r)
}

// The short copy is still the file the restore wrote (its time was carried over): it is removed,
// the run stops, and the old version stays in the versions tree.
#[test]
fn a_short_restore_copy_that_is_still_ours_is_removed() {
    let (rig, r) = short_restore(true);
    assert_eq!(op(&r, "c.txt").reason, Some(Reason::RestoreFailed));
    assert!(!rig.right.join("c.txt").exists());
    let kept = versions_listing(&rig);
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert_eq!(read(&rig.versions.join(&kept[0])), "right-c-old!");
}

// A short file whose time is not the restored one is not ours to delete (somebody wrote to it):
// it is left alone, and the old version stays in the tree.
#[test]
fn a_short_file_that_is_not_ours_is_not_deleted_by_the_restore() {
    let (rig, r) = short_restore(false);
    assert_eq!(op(&r, "c.txt").reason, Some(Reason::RestoreFailed));
    assert!(rig.right.join("c.txt").exists());
    assert_eq!(fs::metadata(rig.right.join("c.txt")).unwrap().len(), 0);
    assert_eq!(versions_listing(&rig).len(), 1);
}
