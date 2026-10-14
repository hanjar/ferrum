//! Injection points of the profile engine (design §13.1): everything that touches the outside
//! world is a trait with a production implementation here, so tests drive sharing violations,
//! a vanished root, a second volume, a Recycle Bin and the clock without real devices.

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::archive_extract::publish::{self, DirHandle, MoveError};
use crate::copy_engine::{self, ConflictPolicy, CopyOptions};
use crate::domain::FsError;
use crate::ops::CancelToken;
use crate::sync_engine::errors::{Classified, classify_fs};
use crate::sync_engine::roots::{FsProbe, RootProbe};
use crate::sync_incremental::{ChangesSource, ServiceChanges};

// ── Copying ──

/// What happened to the sources of one copy call.
#[derive(Debug, Default)]
pub struct CopyReport {
    /// Sources that were fully copied.
    pub done: HashSet<String>,
    /// Sources that failed, with their classification. The key is the source path, or the
    /// destination folder when the whole call failed before any source was looked at.
    pub failed: Vec<(String, Classified)>,
    /// Stopped by the cancel token.
    pub cancelled: bool,
}

/// Copies files into one destination folder under their own names, each published through a
/// staging file (a file in progress is complete or absent).
pub trait Copier: Send + Sync {
    /// `overwrite` replaces an existing destination file; otherwise an existing destination
    /// makes that source fail to copy (it is neither done nor failed: the caller treats it as
    /// "target appeared"). `durable` flushes each file to disk before it is published.
    fn copy(
        &self,
        sources: &[String],
        dest_dir: &str,
        overwrite: bool,
        durable: bool,
        cancel: &CancelToken,
    ) -> CopyReport;
}

/// Production copier: `copy_engine`.
pub struct EngineCopier;

impl Copier for EngineCopier {
    fn copy(
        &self,
        sources: &[String],
        dest_dir: &str,
        overwrite: bool,
        durable: bool,
        cancel: &CancelToken,
    ) -> CopyReport {
        let policy = if overwrite {
            ConflictPolicy::Overwrite
        } else {
            ConflictPolicy::Skip
        };
        let (threads, priority) = crate::copy_pool::defaults();
        let options = CopyOptions {
            threads,
            priority,
            preserve_timestamps: true,
            durable,
            ..CopyOptions::default()
        };
        let out = copy_engine::run_copy_with_options(
            sources,
            dest_dir,
            policy,
            options,
            cancel,
            &|_progress| {},
        );
        let mut report = CopyReport {
            cancelled: out.cancelled,
            ..CopyReport::default()
        };
        for (src, _dest) in out.completed {
            report.done.insert(src);
        }
        for failure in out.failures {
            let fs = FsError::new(failure.code, failure.message);
            report
                .failed
                .push((failure.source, classify_fs(&fs, false)));
        }
        report
    }
}

// ── Recycle Bin ──

/// Moves one file to the Recycle Bin. Only used after the drive gate passed; a failure never
/// turns into a permanent delete.
pub trait Trash: Send + Sync {
    fn trash_file(&self, path: &Path) -> Result<(), FsError>;
}

pub struct SystemTrash;

impl Trash for SystemTrash {
    fn trash_file(&self, path: &Path) -> Result<(), FsError> {
        let report = crate::mutations::delete_to_trash(&[path.to_string_lossy().into_owned()]);
        match report.failed.into_iter().next() {
            None if report.deleted == 1 => Ok(()),
            None => Err(FsError::new(crate::domain::ErrorCode::Io, "not trashed")),
            Some(f) => Err(FsError::new(f.code, f.message)),
        }
    }
}

// ── Moving into the versioning tree ──

/// Renames a file by handle into another folder (never replacing), after `check` accepted the
/// size and last-write time read from the handle itself (`Ok(false)` = not moved, the file is not
/// what was expected). Injectable so a second volume can be simulated (`MoveError::CrossDevice`)
/// when only one exists.
pub trait Mover: Send + Sync {
    fn rename_into(
        &self,
        src_parent: &DirHandle,
        src_name: &std::ffi::OsStr,
        dst_parent: &DirHandle,
        dst_name: &std::ffi::OsStr,
        check: publish::FactsCheck<'_>,
    ) -> Result<bool, MoveError>;
}

pub struct HandleMover;

impl Mover for HandleMover {
    fn rename_into(
        &self,
        src_parent: &DirHandle,
        src_name: &std::ffi::OsStr,
        dst_parent: &DirHandle,
        dst_name: &std::ffi::OsStr,
        check: publish::FactsCheck<'_>,
    ) -> Result<bool, MoveError> {
        publish::rename_file_into_if(src_parent, src_name, dst_parent, dst_name, check)
    }
}

// ── Time ──

pub trait Sleeper: Send + Sync {
    /// Wait `d`. `false` = the cancel token fired while waiting.
    fn sleep(&self, d: Duration, cancel: &CancelToken) -> bool;
}

pub struct RealSleeper;

impl Sleeper for RealSleeper {
    fn sleep(&self, d: Duration, cancel: &CancelToken) -> bool {
        let end = std::time::Instant::now() + d;
        while std::time::Instant::now() < end {
            if cancel.is_cancelled() {
                return false;
            }
            std::thread::sleep(Duration::from_millis(50).min(d));
        }
        !cancel.is_cancelled()
    }
}

pub trait Clock: Send + Sync {
    /// `yyyy-MM-dd HHmmss` in the SYSTEM time zone (design §5.5); never contains a character
    /// that is invalid on FAT/exFAT.
    fn local_stamp(&self) -> String;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn local_stamp(&self) -> String {
        crate::sync_engine::versioning::local_stamp_now()
    }
}

// ── Roots ──

/// Is a sync root still there? (RQ-7.4: a root that disappears mid-run stops the run.)
pub trait Liveness: Send + Sync {
    fn alive(&self, root: &str) -> bool;
}

pub struct FsLiveness;

impl Liveness for FsLiveness {
    fn alive(&self, root: &str) -> bool {
        std::fs::metadata(root).map(|m| m.is_dir()).unwrap_or(false)
    }
}

// ── Test hooks ──

/// Observation points between the steps of a run. All no-ops in production; tests use them to
/// change the world "between plan and move".
pub trait Hooks: Send + Sync {
    /// The final plan, before it is validated (tests tamper with it to prove that a plan that
    /// breaks the profile contract is refused in every build).
    fn on_final_plan(&self, _plan: &mut Vec<crate::sync_engine::plan::SyncPlanItem>) {}
    /// The plan is final and validated, no operation has started.
    fn after_plan(&self) {}
    /// Operation `index` passed its just-in-time check and is about to act (the gap the
    /// handle-level checks of delete and move close).
    fn after_jit(&self, _index: usize, _name: &str) {}
    /// Called right before operation `index` (after the stop checks, before its just-in-time check).
    fn before_op(&self, _index: usize, _name: &str) {}
}

pub struct NoHooks;

impl Hooks for NoHooks {}

// ── Bundle ──

/// Every injectable dependency of one run.
pub struct EngineEnv {
    pub probe: Arc<dyn RootProbe>,
    pub copier: Arc<dyn Copier>,
    pub trash: Arc<dyn Trash>,
    pub mover: Arc<dyn Mover>,
    pub sleeper: Arc<dyn Sleeper>,
    pub clock: Arc<dyn Clock>,
    pub liveness: Arc<dyn Liveness>,
    pub hooks: Arc<dyn Hooks>,
    /// The FerrumIndexer journal (IX-1); tests answer `complete` / `rescan` here.
    pub changes: Mutex<Box<dyn ChangesSource + Send>>,
}

impl EngineEnv {
    pub fn production() -> Self {
        EngineEnv {
            probe: Arc::new(FsProbe),
            copier: Arc::new(EngineCopier),
            trash: Arc::new(SystemTrash),
            mover: Arc::new(HandleMover),
            sleeper: Arc::new(RealSleeper),
            clock: Arc::new(SystemClock),
            liveness: Arc::new(FsLiveness),
            hooks: Arc::new(NoHooks),
            changes: Mutex::new(Box::new(ServiceChanges)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real Recycle Bin, on a temp file owned by this test. Ignored by default because it
    /// leaves one entry (`ferrum-p02-trash-test-<pid>.txt`) in the current user's Recycle Bin;
    /// run it by hand with `cargo test system_trash -- --ignored` to verify the shell path.
    #[test]
    #[ignore = "puts one temp file into the real Recycle Bin"]
    fn system_trash_moves_one_file_to_the_recycle_bin() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir
            .path()
            .join(format!("ferrum-p02-trash-test-{}.txt", std::process::id()));
        std::fs::write(&file, b"temp").unwrap();
        SystemTrash.trash_file(&file).unwrap();
        assert!(!file.exists());
    }

    #[test]
    fn production_env_wires_the_real_implementations() {
        let env = EngineEnv::production();
        assert_eq!(env.clock.local_stamp().len(), "2026-01-02 030405".len());
        assert!(env.liveness.alive(&std::env::temp_dir().to_string_lossy()));
        assert!(!env.liveness.alive(r"Z:\definitely\not\here\ferrum"));
    }
}
