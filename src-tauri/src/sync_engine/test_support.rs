//! Shared fixtures of the profile engine tests: temp trees, a real profile over them, a lease,
//! and scripted versions of every injection point (design §13.1).

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::automation::lease::{GuardOutcome, LeaseEnv, LeaseGuard};
use crate::automation::model::{
    CompareMode, DeleteGuard, DeletePolicy, Filters, ProfilePolicy, RootSpec, SyncMode,
};
use crate::automation::store::Store;
use crate::automation::store::leases::HolderKind;
use crate::domain::FsError;
use crate::ops::CancelToken;
use crate::sync_baseline::SyncBaselineStore;
use crate::sync_engine::env::{
    Clock, Copier, CopyReport, EngineCopier, EngineEnv, Hooks, Liveness, Sleeper, Trash,
};
use crate::sync_engine::errors::Classified;
use crate::sync_engine::execute::{Counts, EngineSink, OpRecord, Phase};
use crate::sync_engine::roots;

/// Create a junction `link` -> `target` (needs no privilege). `false` when it is not possible.
pub fn make_junction(link: &Path, target: &Path) -> bool {
    #[cfg(windows)]
    {
        std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
    #[cfg(not(windows))]
    {
        let _ = (link, target);
        false
    }
}

pub fn write(path: &Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, bytes).unwrap();
}

/// Set the modified time of `path` (so two files compare "same" or "newer" deterministically).
pub fn set_mtime(path: &Path, secs_since_epoch: u64) {
    let f = fs::OpenOptions::new().write(true).open(path).unwrap();
    let t = std::time::UNIX_EPOCH + Duration::from_secs(secs_since_epoch);
    f.set_modified(t).unwrap();
}

/// Names (relative, '/'-separated, sorted) of every file and folder below `root`, folders with a
/// trailing '/'. Never follows links.
pub fn tree(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
        let Ok(rd) = fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            let rel = p
                .strip_prefix(base)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let meta = fs::symlink_metadata(&p).unwrap();
            if meta.is_dir() && !meta.file_type().is_symlink() {
                out.push(format!("{rel}/"));
                walk(base, &p, out);
            } else {
                out.push(rel);
            }
        }
    }
    walk(root, root, &mut out);
    out.sort();
    out
}

/// A temp area with two roots, a baseline store, an automation store (for leases) and a
/// versioning folder location.
pub struct Rig {
    pub dir: tempfile::TempDir,
    pub left: PathBuf,
    pub right: PathBuf,
    pub versions: PathBuf,
    pub baselines: SyncBaselineStore,
    pub auto: Arc<Store>,
    pub lease_env: LeaseEnv,
}

impl Rig {
    pub fn new() -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(dir.path()).unwrap();
        let strip = |p: PathBuf| -> PathBuf {
            let s = p.to_string_lossy().into_owned();
            PathBuf::from(s.strip_prefix(r"\\?\").unwrap_or(&s).to_string())
        };
        let base = strip(base);
        let left = base.join("left");
        let right = base.join("right");
        fs::create_dir_all(&left).unwrap();
        fs::create_dir_all(&right).unwrap();
        let baselines = SyncBaselineStore::open(&base.join("baselines"));
        let auto = Arc::new(Store::open(&base.join("automation"), "0.2.0"));
        Rig {
            versions: base.join("versions"),
            left,
            right,
            baselines,
            auto,
            lease_env: LeaseEnv::system(),
            dir,
        }
    }

    pub fn base(&self) -> PathBuf {
        self.dir.path().to_path_buf()
    }

    /// A real profile over the two roots (identity taken from the real folders).
    pub fn profile(&self, mode: SyncMode, delete_policy: DeletePolicy) -> ProfilePolicy {
        let spec = |p: &Path| {
            let canonical = roots::canonical(&p.to_string_lossy()).unwrap();
            let identity = roots::identity(&canonical).unwrap();
            RootSpec {
                path: canonical,
                identity,
            }
        };
        ProfilePolicy {
            left: spec(&self.left),
            right: spec(&self.right),
            mode,
            delete_policy,
            compare: CompareMode::SizeAndDate,
            filters: Filters {
                include: vec![],
                exclude: vec![],
            },
            guard: DeleteGuard {
                enabled: true,
                max_count: 50,
                max_percent: 20,
            },
        }
    }

    pub fn versioning(&self) -> DeletePolicy {
        DeletePolicy::Versioning {
            root: roots::canonical(&self.versions.to_string_lossy()).unwrap(),
        }
    }

    /// Lease over both roots (and the versioning root for a versioning profile).
    pub fn lease(&self, profile: &ProfilePolicy) -> LeaseGuard {
        let mut list = vec![profile.left.path.clone(), profile.right.path.clone()];
        if let DeletePolicy::Versioning { root } = &profile.delete_policy {
            list.push(root.clone());
        }
        match LeaseGuard::acquire(
            &self.auto,
            &self.lease_env,
            HolderKind::Manual,
            Some(1),
            &list,
            None,
        )
        .unwrap()
        {
            GuardOutcome::Acquired(g) => g,
            GuardOutcome::Busy(_) => panic!("lease busy in a test rig"),
        }
    }
}

// ── Scripted injection points ──

/// Sink that records everything.
#[derive(Default)]
pub struct Recorder {
    pub ops: Mutex<Vec<OpRecord>>,
    pub phases: Mutex<Vec<Phase>>,
    pub counts: Mutex<Vec<(std::time::Instant, Counts)>>,
}

impl EngineSink for Recorder {
    fn phase(&self, phase: Phase) {
        self.phases.lock().unwrap().push(phase);
    }
    fn op(&self, record: &OpRecord) {
        self.ops.lock().unwrap().push(record.clone());
    }
    fn counts(&self, counts: &Counts) {
        self.counts
            .lock()
            .unwrap()
            .push((std::time::Instant::now(), *counts));
    }
}

/// Copier that fails given files a scripted number of times, then delegates to `copy_engine`.
pub struct ScriptedCopier {
    inner: EngineCopier,
    script: Mutex<HashMap<String, VecDeque<Classified>>>,
    pub calls: Mutex<Vec<Vec<String>>>,
    /// Cancel this token when a source with this file name is about to be copied.
    pub cancel_on: Mutex<Option<(String, CancelToken)>>,
}

impl ScriptedCopier {
    pub fn new() -> Arc<ScriptedCopier> {
        Arc::new(ScriptedCopier {
            inner: EngineCopier,
            script: Mutex::new(HashMap::new()),
            calls: Mutex::new(Vec::new()),
            cancel_on: Mutex::new(None),
        })
    }

    /// `file_name` fails with each of `failures` in turn (one per attempt).
    pub fn fail(&self, file_name: &str, failures: &[Classified]) {
        self.script
            .lock()
            .unwrap()
            .insert(file_name.to_string(), failures.iter().copied().collect());
    }
}

impl Copier for ScriptedCopier {
    fn copy(
        &self,
        sources: &[String],
        dest_dir: &str,
        overwrite: bool,
        durable: bool,
        cancel: &CancelToken,
    ) -> CopyReport {
        self.calls.lock().unwrap().push(sources.to_vec());
        let mut failed: Vec<(String, Classified)> = Vec::new();
        let mut pass: Vec<String> = Vec::new();
        for s in sources {
            let name = Path::new(s)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if let Some((target, token)) = self.cancel_on.lock().unwrap().as_ref()
                && *target == name
            {
                token.cancel();
            }
            let next = self
                .script
                .lock()
                .unwrap()
                .get_mut(&name)
                .and_then(VecDeque::pop_front);
            match next {
                Some(c) => failed.push((s.clone(), c)),
                None => pass.push(s.clone()),
            }
        }
        let mut report = if pass.is_empty() {
            CopyReport::default()
        } else {
            self.inner.copy(&pass, dest_dir, overwrite, durable, cancel)
        };
        report.failed.extend(failed);
        report
    }
}

/// Trash that records the files it was given and moves them to `bin` (a temp folder) instead of
/// the real Recycle Bin.
pub struct FakeTrash {
    pub bin: PathBuf,
    pub trashed: Mutex<Vec<PathBuf>>,
    pub fail: Mutex<Option<FsError>>,
}

impl FakeTrash {
    pub fn new(bin: PathBuf) -> Arc<FakeTrash> {
        fs::create_dir_all(&bin).unwrap();
        Arc::new(FakeTrash {
            bin,
            trashed: Mutex::new(Vec::new()),
            fail: Mutex::new(None),
        })
    }
}

impl Trash for FakeTrash {
    fn trash_file(&self, path: &Path) -> Result<(), FsError> {
        if let Some(e) = self.fail.lock().unwrap().clone() {
            return Err(e);
        }
        let n = self.trashed.lock().unwrap().len();
        let dest = self.bin.join(format!(
            "{n}-{}",
            path.file_name().unwrap().to_string_lossy()
        ));
        fs::rename(path, dest).map_err(FsError::from)?;
        self.trashed.lock().unwrap().push(path.to_path_buf());
        Ok(())
    }
}

pub struct FixedClock(pub &'static str);

impl Clock for FixedClock {
    fn local_stamp(&self) -> String {
        self.0.to_string()
    }
}

/// Sleeper that records and returns at once.
#[derive(Default)]
pub struct InstantSleeper {
    pub slept: Mutex<Vec<Duration>>,
}

impl Sleeper for InstantSleeper {
    fn sleep(&self, d: Duration, cancel: &CancelToken) -> bool {
        self.slept.lock().unwrap().push(d);
        !cancel.is_cancelled()
    }
}

/// Liveness that reports a root as gone after a number of questions.
pub struct DiesAfter {
    pub root: String,
    pub remaining: AtomicUsize,
}

impl Liveness for DiesAfter {
    fn alive(&self, root: &str) -> bool {
        if !root.eq_ignore_ascii_case(&self.root) {
            return std::fs::metadata(root).map(|m| m.is_dir()).unwrap_or(false);
        }
        let left = self.remaining.load(Ordering::SeqCst);
        if left == 0 {
            return false;
        }
        self.remaining.store(left - 1, Ordering::SeqCst);
        true
    }
}

type PlanHook = Box<dyn Fn(&mut Vec<crate::sync_engine::plan::SyncPlanItem>) + Send>;
type OpHook = Box<dyn Fn(usize, &str) + Send>;

/// Hooks driven by closures.
#[derive(Default)]
pub struct TestHooks {
    pub final_plan: Mutex<Option<PlanHook>>,
    pub after_plan: Mutex<Option<Box<dyn Fn() + Send>>>,
    pub before_op: Mutex<Option<OpHook>>,
    pub after_jit: Mutex<Option<OpHook>>,
}

impl Hooks for TestHooks {
    fn on_final_plan(&self, plan: &mut Vec<crate::sync_engine::plan::SyncPlanItem>) {
        if let Some(f) = self.final_plan.lock().unwrap().as_ref() {
            f(plan);
        }
    }
    fn after_plan(&self) {
        if let Some(f) = self.after_plan.lock().unwrap().as_ref() {
            f();
        }
    }
    fn after_jit(&self, index: usize, name: &str) {
        if let Some(f) = self.after_jit.lock().unwrap().as_ref() {
            f(index, name);
        }
    }
    fn before_op(&self, index: usize, name: &str) {
        if let Some(f) = self.before_op.lock().unwrap().as_ref() {
            f(index, name);
        }
    }
}

/// An environment that behaves like production except for the clock and the sleeper.
pub fn test_env() -> EngineEnv {
    let mut env = EngineEnv::production();
    env.clock = Arc::new(FixedClock("2026-01-02 030405"));
    env.sleeper = Arc::new(InstantSleeper::default());
    env
}
