//! Engine tests, part 2: versioning trees, gates, lease and settle, indexer, sources.

use super::*;
use crate::archive_extract::publish::MoveError;
use crate::automation::lease::LeaseEnv;
use crate::automation::model::{DeletePolicy, Side, SyncMode};
use crate::sync_baseline::{LoadResult, RootPair};
use crate::sync_engine::env::Mover;
use crate::sync_engine::errors::Reason;
use crate::sync_engine::roots;
use crate::sync_engine::test_support::*;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

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

const MIRRORED_RIGHT: [&str; 5] = ["a.txt", "c.txt", "same.txt", "sub/", "sub/b.txt"];

// ── Versioning: junctions, second volume (SE-9, T9, design §5.5) ──

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

// Design §5.5: on another volume the file is copied into the versions tree, verified, and only
// then deleted from the source. Simulated with a mover that always answers "not the same device".
#[test]
fn cross_volume_versioning_copies_verifies_then_deletes_the_source() {
    let rig = Rig::new();
    seed_basic(&rig);
    let profile = rig.profile(mirror(), rig.versioning());
    let mut env = test_env();
    env.mover = Arc::new(CrossVolume);
    let r = go(&rig, &profile, &env);
    completed(&r);
    assert_eq!(tree(&rig.right), MIRRORED_RIGHT);
    let run_dir = rig.versions.join("Right").join("2026-01-02 030405 #7");
    assert_eq!(read(&run_dir.join("c.txt")), "right-c-old!");
    assert_eq!(read(&run_dir.join("extra.txt")), "x");
    assert_eq!(read(&run_dir.join("sub2").join("extra2.txt")), "y");
    // No staging leftovers in the versions tree.
    assert!(
        tree(&rig.versions)
            .iter()
            .all(|n| !n.contains(".ferrum-stage-"))
    );
}

/// A copier whose result is empty, to prove the size check before the source delete.
struct TruncatingCopier;

impl crate::sync_engine::env::Copier for TruncatingCopier {
    fn copy(
        &self,
        sources: &[String],
        dest_dir: &str,
        overwrite: bool,
        durable: bool,
        cancel: &CancelToken,
    ) -> crate::sync_engine::env::CopyReport {
        let report = crate::sync_engine::env::EngineCopier
            .copy(sources, dest_dir, overwrite, durable, cancel);
        for s in sources {
            let name = Path::new(s).file_name().unwrap();
            let f = fs::OpenOptions::new()
                .write(true)
                .open(Path::new(dest_dir).join(name))
                .unwrap();
            f.set_len(0).unwrap();
        }
        report
    }
}

#[test]
fn a_short_cross_volume_copy_is_rolled_back_and_the_source_stays() {
    let rig = Rig::new();
    write(&rig.left.join("keep.txt"), b"k");
    write(&rig.right.join("extra.txt"), b"important data");
    let profile = rig.profile(mirror(), rig.versioning());
    let mut env = test_env();
    env.mover = Arc::new(CrossVolume);
    env.copier = Arc::new(TruncatingCopier);
    let r = go(&rig, &profile, &env);
    assert_eq!(status_of(&r, "extra.txt"), OpStatus::Failed);
    assert_eq!(reason_of(&r, "extra.txt"), Some(Reason::VerifyFailed));
    assert_eq!(read(&rig.right.join("extra.txt")), "important data");
    let in_versions: Vec<String> = tree(&rig.versions)
        .into_iter()
        .filter(|n| n.ends_with("extra.txt"))
        .collect();
    assert!(
        in_versions.is_empty(),
        "the bad copy was removed: {in_versions:?}"
    );
}

// SE-9, T9: a junction planted at `<versions>\Right` between plan and move is rejected; nothing
// is written through it and the file that should have been versioned is still where it was.
#[test]
fn junction_planted_in_the_versioning_tree_after_the_plan_is_rejected() {
    let rig = Rig::new();
    write(&rig.left.join("keep.txt"), b"k");
    write(&rig.right.join("keep.txt"), b"k");
    set_mtime(&rig.left.join("keep.txt"), 1_900_000_000);
    set_mtime(&rig.right.join("keep.txt"), 1_900_000_000);
    write(&rig.right.join("extra.txt"), b"to be versioned");
    fs::create_dir_all(&rig.versions).unwrap();
    let outside = rig.base().join("outside");
    fs::create_dir_all(&outside).unwrap();
    let profile = rig.profile(mirror(), rig.versioning());
    let hooks = Arc::new(TestHooks::default());
    let (versions, out2) = (rig.versions.clone(), outside.clone());
    let made = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let m2 = made.clone();
    *hooks.after_plan.lock().unwrap() = Some(Box::new(move || {
        if !make_junction(&versions.join("Right"), &out2) {
            m2.store(false, std::sync::atomic::Ordering::SeqCst);
        }
    }));
    let mut env = test_env();
    env.hooks = hooks;
    let r = go(&rig, &profile, &env);
    if !made.load(std::sync::atomic::Ordering::SeqCst) {
        eprintln!("SKIPPED: junction creation is not permitted here");
        return;
    }
    assert_eq!(status_of(&r, "extra.txt"), OpStatus::Failed);
    assert_eq!(reason_of(&r, "extra.txt"), Some(Reason::LinkInPath));
    assert_eq!(
        fs::read_dir(&outside).unwrap().count(),
        0,
        "nothing written through the junction"
    );
    assert_eq!(read(&rig.right.join("extra.txt")), "to be versioned");
    let _ = fs::remove_dir(rig.versions.join("Right"));
}

// SE-9: the same for a junction deeper in the tree (the `sub2` folder of this run's versions).
#[test]
fn junction_deeper_in_the_versioning_tree_is_rejected_too() {
    let rig = Rig::new();
    write(&rig.left.join("keep.txt"), b"k");
    write(&rig.right.join("keep.txt"), b"k");
    set_mtime(&rig.left.join("keep.txt"), 1_900_000_000);
    set_mtime(&rig.right.join("keep.txt"), 1_900_000_000);
    write(&rig.right.join("sub2").join("extra2.txt"), b"deep");
    let run_dir = rig.versions.join("Right").join("2026-01-02 030405 #7");
    fs::create_dir_all(&run_dir).unwrap();
    let outside = rig.base().join("outside");
    fs::create_dir_all(&outside).unwrap();
    if !make_junction(&run_dir.join("sub2"), &outside) {
        eprintln!("SKIPPED: junction creation is not permitted here");
        return;
    }
    let profile = rig.profile(mirror(), rig.versioning());
    let r = go(&rig, &profile, &test_env());
    assert_eq!(status_of(&r, "sub2/extra2.txt"), OpStatus::Failed);
    assert_eq!(reason_of(&r, "sub2/extra2.txt"), Some(Reason::LinkInPath));
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
    assert_eq!(read(&rig.right.join("sub2").join("extra2.txt")), "deep");
    let _ = fs::remove_dir(run_dir.join("sub2"));
}

// SP-3.2: the versioning root must be a folder, not a file, and must not sit
// inside a sync root; checked before any operation.
#[test]
fn versioning_root_that_is_a_file_or_nested_is_refused_before_any_operation() {
    let rig = Rig::new();
    write(&rig.left.join("a.txt"), b"a");
    write(&rig.versions, b"i am a file, not a folder");
    let profile = rig.profile(mirror(), rig.versioning());
    let r = go(&rig, &profile, &test_env());
    match &r.report.outcome {
        EngineOutcome::Failed { error } => assert_eq!(error.code, "versioning_root_invalid"),
        other => panic!("expected Failed, got {other:?}"),
    }
    assert!(tree(&rig.right).is_empty());
    assert_eq!(read(&rig.versions), "i am a file, not a folder");

    // Inside the left root.
    let rig = Rig::new();
    write(&rig.left.join("a.txt"), b"a");
    let nested = DeletePolicy::Versioning {
        root: roots::canonical(&rig.left.join("versions").to_string_lossy()).unwrap(),
    };
    let profile = rig.profile(mirror(), nested);
    let r = go(&rig, &profile, &test_env());
    match &r.report.outcome {
        EngineOutcome::Failed { error } => assert_eq!(error.code, "versioning_root_invalid"),
        other => panic!("expected Failed, got {other:?}"),
    }
    assert!(tree(&rig.right).is_empty());
}

// ── Recycle Bin gate (SP-3.1) ──

/// Probe that reports every local root as a removable drive.
struct DriveProbe {
    removable: bool,
}

impl roots::RootProbe for DriveProbe {
    fn canonical(&self, path: &str) -> Result<String, roots::RootError> {
        roots::canonical(path)
    }
    fn identity(
        &self,
        canonical: &str,
    ) -> Result<crate::automation::model::RootIdentity, roots::RootError> {
        let mut id = roots::identity(canonical)?;
        if let crate::automation::model::RootIdentity::Local { drive, .. } = &mut id
            && self.removable
        {
            *drive = crate::automation::model::DriveKind::Removable;
        }
        Ok(id)
    }
}

// SP-3.1, design §5.5: the Recycle Bin is refused at RUN time on a non-fixed drive, before any
// operation, and never turns into a permanent delete.
#[test]
fn recycle_bin_is_refused_at_run_time_on_a_removable_drive() {
    use roots::RootProbe;
    let rig = Rig::new();
    write(&rig.left.join("keep.txt"), b"k");
    write(&rig.right.join("extra.txt"), b"must survive");
    let probe = DriveProbe { removable: true };
    let mut profile = rig.profile(mirror(), DeletePolicy::RecycleBin);
    // As if saved while the drive was removable (the editor refuses that; this is the run side).
    for spec in [&mut profile.left, &mut profile.right] {
        spec.identity = probe.identity(&spec.path).unwrap();
    }
    let bin = FakeTrash::new(rig.base().join("bin"));
    let mut env = test_env();
    env.probe = Arc::new(probe);
    env.trash = bin.clone();
    let r = go(&rig, &profile, &env);
    match &r.report.outcome {
        EngineOutcome::Failed { error } => {
            assert_eq!(error.code, "recycle_bin_unavailable");
            assert!(error.text.starts_with("Recycle Bin isn't available on "));
            assert!(
                error
                    .text
                    .ends_with("choose a versioning folder or permanent delete.")
            );
        }
        other => panic!("expected Failed, got {other:?}"),
    }
    assert_eq!(read(&rig.right.join("extra.txt")), "must survive");
    assert!(bin.trashed.lock().unwrap().is_empty());
    assert!(
        !rig.right.join("keep.txt").exists(),
        "not even the copy ran"
    );

    // The drive was fixed when saved but is removable now: a different volume, fail closed.
    let rig = Rig::new();
    write(&rig.right.join("extra.txt"), b"must survive");
    let profile = rig.profile(mirror(), DeletePolicy::RecycleBin);
    let mut env = test_env();
    env.probe = Arc::new(DriveProbe { removable: true });
    let r = go(&rig, &profile, &env);
    assert!(matches!(r.report.outcome, EngineOutcome::Failed { .. }));
    assert_eq!(read(&rig.right.join("extra.txt")), "must survive");
}

// SP-3.1: at SAVE time the same non-fixed drive is refused (injected drive type).
#[test]
fn recycle_bin_is_refused_at_save_time_on_a_removable_drive() {
    use crate::automation::model::{
        Draft, DraftGuard, PolicyDraft, WireCompare, WireDeletePolicy, WireMode, WireSide,
    };
    let rig = Rig::new();
    let draft = Draft {
        name: "Backup".into(),
        policy: PolicyDraft {
            left: rig.left.to_string_lossy().into_owned(),
            right: rig.right.to_string_lossy().into_owned(),
            mode: WireMode::Mirror,
            from: Some(WireSide::Left),
            propagate_deletes: None,
            delete_policy: WireDeletePolicy::RecycleBin,
            versioning_root: None,
            compare: WireCompare::SizeAndDate,
            include: vec![],
            exclude: vec![],
            guard: DraftGuard {
                enabled: true,
                max_count: 50,
                max_percent: 20,
            },
        },
    };
    let errors = draft.validate(&DriveProbe { removable: true }).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|e| e.field == "right" && e.code == "recycle_bin_unavailable")
    );
    assert!(draft.validate(&DriveProbe { removable: false }).is_ok());
}

// ── Lease and settle (design §3.4, §5.7, SP-7) ──

// Design §3.4: no lease, no run; a lease over other folders does not count.
#[test]
fn a_run_needs_a_lease_that_covers_its_folders() {
    let rig = Rig::new();
    write(&rig.left.join("a.txt"), b"a");
    let profile = rig.profile(one_way(), DeletePolicy::Permanent);
    let sink = Recorder::default();
    let env = test_env();
    let req = EngineRequest::new(&profile, &rig.baselines, &sink, &env);
    let report = run(&req);
    match report.outcome {
        EngineOutcome::Failed { error } => assert_eq!(error.code, "lease_required"),
        other => panic!("expected Failed, got {other:?}"),
    }
    assert!(tree(&rig.right).is_empty());

    // A lease on a different pair of folders.
    let other = Rig::new();
    let other_profile = other.profile(one_way(), DeletePolicy::Permanent);
    let wrong = other.lease(&other_profile);
    let mut req = EngineRequest::new(&profile, &rig.baselines, &sink, &env);
    req.lease = Some(&wrong);
    match run(&req).outcome {
        EngineOutcome::Failed { error } => assert_eq!(error.code, "lease_required"),
        o => panic!("expected Failed, got {o:?}"),
    }
    assert!(tree(&rig.right).is_empty());

    // A dry run needs none.
    let mut req = EngineRequest::new(&profile, &rig.baselines, &sink, &env);
    req.dry_run = true;
    assert!(matches!(run(&req).outcome, EngineOutcome::DryRun { .. }));
}

fn pair_of(profile: &ProfilePolicy) -> RootPair {
    RootPair {
        left_root: profile.left.path.clone(),
        right_root: profile.right.path.clone(),
    }
}

// SP-7 / design §5.4 step 9: a run that changed something saves the baseline while its lease is
// alive; the next run sees it.
#[test]
fn settle_saves_the_baseline_while_the_lease_is_alive() {
    let rig = Rig::new();
    write(&rig.left.join("a.txt"), b"a");
    let profile = rig.profile(one_way(), DeletePolicy::Permanent);
    let r = go(&rig, &profile, &test_env());
    completed(&r);
    assert!(
        matches!(r.report.settle, SettleState::Saved { entries: 1 }),
        "{:?}",
        r.report.settle
    );
    match rig.baselines.load(&pair_of(&profile)).unwrap() {
        LoadResult::Ok { baseline } => assert!(baseline.entries.contains_key("a.txt")),
        other => panic!("baseline expected: {other:?}"),
    }
    // Phases reached the sink in order.
    let phases = r.sink.phases.lock().unwrap().clone();
    assert_eq!(
        phases,
        [
            Phase::Checking,
            Phase::Comparing,
            Phase::Syncing,
            Phase::Settling
        ]
    );
    // Nothing left to do: the next run is a no-op and the baseline stays.
    let r2 = go(&rig, &profile, &test_env());
    assert!(matches!(r2.report.outcome, EngineOutcome::NothingToDo));
    assert!(matches!(r2.report.settle, SettleState::Unchanged { .. }));
}

// Design §3.4 / §5.7: a lease that is lost mid-run stops the run safely (Interrupted) and the
// baseline is NOT saved - `save` is refused without a live lease.
#[test]
fn a_lost_lease_stops_the_run_and_the_baseline_is_not_saved() {
    let mut rig = Rig::new();
    rig.lease_env = LeaseEnv {
        beat_every: Duration::from_millis(10),
        ..LeaseEnv::system()
    };
    // One folder per file: the folder operations run one at a time, so the lease check between
    // them is visible.
    for i in 0..4 {
        write(&rig.left.join(format!("d{i}")).join("f.txt"), b"data");
    }
    let profile = rig.profile(one_way(), DeletePolicy::Permanent);
    let lease = rig.lease(&profile);
    let (store, id, holder) = (
        rig.auto.clone(),
        lease.id().to_string(),
        rig.lease_env.holder,
    );
    let hooks = Arc::new(TestHooks::default());
    *hooks.before_op.lock().unwrap() = Some(Box::new(move |index, _| {
        if index == 2 {
            // Another process reclaims the lease; the next heartbeat notices.
            store
                .with_write(|c| crate::automation::store::leases::release(c, &id, holder))
                .unwrap();
            std::thread::sleep(Duration::from_millis(400));
        }
    }));
    let mut env = test_env();
    env.hooks = hooks;
    let sink = Recorder::default();
    let mut req = EngineRequest::new(&profile, &rig.baselines, &sink, &env);
    req.lease = Some(&lease);
    let report = run(&req);
    match report.outcome {
        EngineOutcome::Interrupted { why, counts } => {
            assert_eq!(why, InterruptReason::LeaseLost);
            assert!(counts.done >= 1 && counts.not_started >= 1, "{counts:?}");
        }
        other => panic!("expected Interrupted, got {other:?}"),
    }
    assert_eq!(
        report.settle,
        SettleState::SaveFailed {
            code: "lease_required"
        }
    );
    assert!(matches!(
        rig.baselines.load(&pair_of(&profile)).unwrap(),
        LoadResult::FirstRun { .. }
    ));
}

// FER-524 (P0.2 review P3-12): `is_lost` only flips when the holder's own heartbeat notices. A
// lease whose heartbeat stalled (here: its clock moved on and nothing beat) no longer COVERS the
// folders, and the run must stop before the next operation instead of working on folders another
// holder may have taken.
#[test]
fn a_lease_that_stopped_covering_the_folders_stops_the_run_even_if_it_never_noticed() {
    use std::sync::atomic::{AtomicI64, Ordering};
    let mut rig = Rig::new();
    let clock = Arc::new(AtomicI64::new(5_000_000));
    rig.lease_env = LeaseEnv {
        clock: {
            let c = clock.clone();
            Arc::new(move || c.load(Ordering::SeqCst))
        },
        probe: Arc::new(crate::automation::store::leases::OsProcessProbe),
        holder: crate::automation::store::leases::Holder::current(),
        // The heartbeat never gets to run in this test: `is_lost` stays false.
        beat_every: Duration::from_secs(3600),
        max_manual_hold_ms: crate::automation::lease::MAX_MANUAL_HOLD_MS,
    };
    for i in 0..4 {
        write(&rig.left.join(format!("d{i}")).join("f.txt"), b"data");
    }
    let profile = rig.profile(one_way(), DeletePolicy::Permanent);
    let lease = rig.lease(&profile);
    let hooks = Arc::new(TestHooks::default());
    let clock_for_hook = clock.clone();
    *hooks.before_op.lock().unwrap() = Some(Box::new(move |index, _| {
        if index == 2 {
            // 40 s pass without a beat: the row is stale, but nobody told the guard.
            clock_for_hook.fetch_add(40_000, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(2_300));
        }
    }));
    let mut env = test_env();
    env.hooks = hooks;
    let sink = Recorder::default();
    let mut req = EngineRequest::new(&profile, &rig.baselines, &sink, &env);
    req.lease = Some(&lease);
    let report = run(&req);
    assert!(!lease.is_lost(), "the guard never noticed anything");
    match report.outcome {
        EngineOutcome::Interrupted { why, counts } => {
            assert_eq!(why, InterruptReason::LeaseLost);
            assert!(counts.done >= 1 && counts.not_started >= 1, "{counts:?}");
        }
        other => panic!("expected Interrupted, got {other:?}"),
    }
    assert!(
        matches!(
            rig.baselines.load(&pair_of(&profile)).unwrap(),
            LoadResult::FirstRun { .. }
        ),
        "no baseline was saved on a lease that does not cover the folders"
    );
}

// ── Indexer path (IX-1) ──

mod indexer {
    use super::*;
    use crate::automation::model::CompareMode;
    use crate::sync_incremental::ChangesSource;
    use ferrum_indexer::query::{ChangeStatus, Changes, JournalMark, RescanReason};
    use std::collections::VecDeque;

    struct FakeChanges(VecDeque<Result<[Changes; 2], String>>);

    impl ChangesSource for FakeChanges {
        fn changes(
            &mut self,
            _: [&str; 2],
            _: [Option<JournalMark>; 2],
        ) -> Result<[Changes; 2], String> {
            self.0.pop_front().unwrap_or(Err("no more answers".into()))
        }
    }

    fn ch(status: ChangeStatus, usn: i64) -> Changes {
        Changes {
            mark: JournalMark { journal_id: 7, usn },
            root_frn: 55,
            status,
            dirs: vec![],
        }
    }

    fn answers(list: Vec<[Changes; 2]>) -> EngineEnv {
        let mut env = test_env();
        env.changes =
            std::sync::Mutex::new(Box::new(FakeChanges(list.into_iter().map(Ok).collect())));
        env
    }

    // IX-1: with a fake journal that answers `complete`, content reads are skipped exactly as the
    // sync_incremental tests specify (a same-size same-mtime pair that differs in content is NOT
    // noticed); with `rescan`, every content is read and the difference is found and synced.
    #[test]
    fn complete_answer_skips_content_reads_and_rescan_reads_everything() {
        let rig = Rig::new();
        write(&rig.left.join("x.txt"), b"AAAA");
        write(&rig.right.join("x.txt"), b"AAAA");
        set_mtime(&rig.left.join("x.txt"), 1_900_000_000);
        set_mtime(&rig.right.join("x.txt"), 1_900_000_000);
        let mut profile = rig.profile(one_way(), DeletePolicy::Permanent);
        profile.compare = CompareMode::Content;

        // Run 1: the pair is identical; the journal only hands out marks. They are settled into
        // the baseline (the pair is proven fully in sync by content).
        let env = answers(vec![[
            ch(ChangeStatus::MarkOnly, 100),
            ch(ChangeStatus::MarkOnly, 200),
        ]]);
        let r1 = go_with(&rig, &profile, &env, |req| req.incremental = true);
        assert!(matches!(r1.report.outcome, EngineOutcome::NothingToDo));
        assert!(
            matches!(r1.report.settle, SettleState::Saved { .. }),
            "{:?}",
            r1.report.settle
        );
        assert_eq!(r1.report.incremental.as_ref().map(|i| i.used), Some(false));

        // The right file now differs in CONTENT only (same size, mtime restored).
        write(&rig.right.join("x.txt"), b"BBBB");
        set_mtime(&rig.right.join("x.txt"), 1_900_000_000);

        // Run 2: `complete`, nothing changed in any directory: the content read is skipped.
        let env = answers(vec![[
            ch(ChangeStatus::Complete, 150),
            ch(ChangeStatus::Complete, 250),
        ]]);
        let r2 = go_with(&rig, &profile, &env, |req| req.incremental = true);
        let info = r2.report.incremental.clone().expect("incremental info");
        assert!(info.used, "{info:?}");
        assert_eq!(info.content_skipped, 1);
        assert!(matches!(r2.report.outcome, EngineOutcome::NothingToDo));
        assert_eq!(read(&rig.right.join("x.txt")), "BBBB");

        // Run 3: the journal says rescan: every content is read, the difference is synced.
        let rescan = || {
            [
                ch(ChangeStatus::Rescan(RescanReason::JournalChanged), 300),
                ch(ChangeStatus::Rescan(RescanReason::JournalChanged), 400),
            ]
        };
        let env = answers(vec![rescan(), rescan()]);
        let r3 = go_with(&rig, &profile, &env, |req| req.incremental = true);
        let info = r3.report.incremental.clone().expect("incremental info");
        assert!(!info.used);
        assert_eq!(info.content_skipped, 0);
        completed(&r3);
        assert_eq!(read(&rig.right.join("x.txt")), "AAAA");
    }

    // IX-1: correctness never depends on the indexer - an unreachable service is the ordinary
    // compare.
    #[test]
    fn an_unreachable_indexer_falls_back_to_the_ordinary_compare() {
        let rig = Rig::new();
        write(&rig.left.join("x.txt"), b"AAAA");
        write(&rig.right.join("x.txt"), b"BBBB");
        set_mtime(&rig.left.join("x.txt"), 1_900_000_000);
        set_mtime(&rig.right.join("x.txt"), 1_900_000_000);
        let mut profile = rig.profile(one_way(), DeletePolicy::Permanent);
        profile.compare = CompareMode::Content;
        let env = answers(vec![]);
        let r = go_with(&rig, &profile, &env, |req| req.incremental = true);
        completed(&r);
        assert_eq!(read(&rig.right.join("x.txt")), "AAAA");
    }
}

// ── Filters, staging files, throttle, sources ──

// SP-4.3, SP-4.4: excluded entries and the engine's own staging files are invisible: never
// copied, never overwritten, never deleted - not even by a mirror.
#[test]
fn excluded_entries_and_staging_files_are_never_touched() {
    let rig = Rig::new();
    write(&rig.left.join("a.txt"), b"a");
    write(&rig.left.join("skip.tmp"), b"left tmp");
    write(&rig.right.join("keep.tmp"), b"right tmp");
    write(
        &rig.right.join(".a.txt.ferrum-stage-1-1"),
        b"staging of someone else",
    );
    write(&rig.right.join("extra.txt"), b"extra");
    let mut profile = rig.profile(mirror(), DeletePolicy::Permanent);
    profile.filters.exclude = vec!["*.tmp".into()];
    let r = go(&rig, &profile, &test_env());
    completed(&r);
    assert_eq!(
        tree(&rig.right),
        [".a.txt.ferrum-stage-1-1", "a.txt", "keep.tmp"]
    );
    assert!(!rig.right.join("skip.tmp").exists());
    assert_eq!(read(&rig.left.join("skip.tmp")), "left tmp");
}

// Permanent delete goes through the file's own handle: a read-only file is deleted.
#[test]
fn permanent_delete_handles_read_only_files() {
    let rig = Rig::new();
    write(&rig.left.join("keep.txt"), b"k");
    write(&rig.right.join("ro.txt"), b"read only");
    let mut perm = fs::metadata(rig.right.join("ro.txt"))
        .unwrap()
        .permissions();
    perm.set_readonly(true);
    fs::set_permissions(rig.right.join("ro.txt"), perm).unwrap();
    let profile = rig.profile(mirror(), DeletePolicy::Permanent);
    let r = go(&rig, &profile, &test_env());
    completed(&r);
    assert_eq!(tree(&rig.right), ["keep.txt"]);
}

// Design §5.1: counts reach the sink at most ten times per second.
#[test]
fn counts_are_throttled_to_ten_per_second() {
    let rig = Rig::new();
    for i in 0..600 {
        write(&rig.left.join(format!("f{i:04}.txt")), b"x");
    }
    let profile = rig.profile(one_way(), DeletePolicy::Permanent);
    let started = std::time::Instant::now();
    let r = go(&rig, &profile, &test_env());
    let elapsed = started.elapsed().as_secs_f64();
    assert_eq!(completed(&r).done, 600);
    let n = r.sink.counts.lock().unwrap().len();
    assert!(
        (n as f64) <= elapsed * 10.0 + 4.0,
        "{n} count events in {elapsed:.2} s"
    );
}

// Nothing in the engine may call a recursive delete or the generic delete helpers; folders only
// go through `remove_empty_dir`.
#[test]
fn engine_sources_contain_no_recursive_or_generic_delete() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let files = [
        "sync_engine/execute.rs",
        "sync_engine/delete_policy.rs",
        "sync_engine/versioning.rs",
        "sync_engine/nofollow.rs",
        "sync_engine/opset.rs",
        "sync_engine/env.rs",
        "automation/manual_run.rs",
    ];
    for file in files {
        let text = fs::read_to_string(root.join(file)).unwrap();
        // Test code may use them to set up fixtures; production code is above the test module.
        let cut = ["#[cfg(test)]", "#[cfg(all(test"]
            .iter()
            .filter_map(|m| text.find(m))
            .min()
            .unwrap_or(text.len());
        let production = &text[..cut];
        for forbidden in [
            "remove_dir_all",
            "delete_permanent",
            "fs::remove_dir(",
            "fs::remove_file(",
        ] {
            assert!(
                !production.contains(forbidden),
                "{file} must not use {forbidden}"
            );
        }
    }
}
