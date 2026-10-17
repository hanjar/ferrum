//! The commit-time re-check of an in-place overwrite (FER-564, P0.2 review P3-2).
//!
//! `recycleBin` and `permanent` overwrite the target in place (SP-3.4): the new bytes are copied
//! to a staging file next to the target and then swapped in. The just-in-time check of the
//! operation is made before that copy, which can take minutes for a big file; a target edited
//! meanwhile must not be replaced (and, since an in-place overwrite never goes to the Recycle
//! Bin, the edit would be lost for good).
//!
//! [`CommitCheck`] is handed to the copy engine as its [`ReplaceGuard`]. At the moment of the
//! swap it asks `archive_publish::replace_file_checked`, which holds the target open without
//! write sharing, compares size and time on that handle and only then moves the old file aside and
//! publishes the staged one (never replacing a name, deleting the old file last). A target that
//! changed, vanished or turned into a folder or link is left exactly as it is: the item becomes
//! `changed` with the reason `target_changed`, and the staged copy is discarded by the copy engine.
//!
//! Why not `ReplaceFileW`: it works on paths (nothing can be compared on the object it replaces),
//! merges attributes and security from the old file, and its failure modes leave the old file in a
//! half-moved state that cannot be told apart from an edit. The held-handle swap keeps the old file
//! intact until the new one is in place and never overwrites a name.
//!
//! If the swap fails AND the old file cannot be put back (a newcomer took its name in the few
//! microseconds in between), the old file stays, intact, under its `.<name>.ferrum-backup-*` name;
//! that is reported as `backup_left` (systemic stop) instead of an ordinary copy error.
//!
//! Residual window: from the facts being read to the swap being done the target is held open
//! without write sharing, so nothing can write, rename or delete it in between; a program that
//! tries fails its own open for those few microseconds, never ours. Before the facts are read,
//! the check against the plan and the exact facts of the just-in-time check covers every edit.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::archive_extract::publish::{self, FileId, Replaced};
use crate::copy_engine::{ReplaceGuard, create_backup_path};
use crate::domain::{ErrorCode, FsError};
use crate::sync_engine::env::Hooks;
use crate::sync_engine::versioning::Expect;

/// How close the target's time must be to what was seen at the just-in-time check.
const SEEN_TOLERANCE_MS: i64 = 1;

pub struct CommitCheck<'a> {
    hooks: &'a dyn Hooks,
    index: usize,
    name: &'a str,
    /// What the plan saw (size, time within the usual tolerance).
    plan: Expect,
    /// What the just-in-time check itself saw, exactly (a tighter bound than the plan's).
    seen: Option<Expect>,
    /// Identity (volume serial + file index) of the target at the just-in-time check: a file
    /// replaced by another one with the same size and time is still caught.
    seen_id: Option<FileId>,
    changed: AtomicBool,
    /// Set when the old file could not be put back and stays at this backup name.
    backup_left: Mutex<Option<PathBuf>>,
}

/// The verbatim form of an absolute path (see `path_guard::verbatim_path`), so that a target
/// above MAX_PATH works as it did with `std::fs::rename`. Relative paths and paths that already
/// are verbatim or device paths are left alone.
pub(crate) fn extended(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if !path.is_absolute()
        || crate::path_guard::is_verbatim_spelling(path)
        || text.starts_with(r"\\.\")
    {
        return path.to_path_buf();
    }
    PathBuf::from(crate::path_guard::verbatim_path(&text.replace('/', "\\")))
}

impl<'a> CommitCheck<'a> {
    pub fn new(
        hooks: &'a dyn Hooks,
        index: usize,
        name: &'a str,
        plan: Expect,
        seen: Option<Expect>,
        seen_id: Option<FileId>,
    ) -> Self {
        CommitCheck {
            hooks,
            index,
            name,
            plan,
            seen,
            seen_id,
            changed: AtomicBool::new(false),
            backup_left: Mutex::new(None),
        }
    }

    /// The backup name the old file was left at, when the swap failed and the old file could not
    /// be put back (see `Replaced::BackupLeft`).
    pub fn backup_left(&self) -> Option<PathBuf> {
        self.backup_left.lock().ok().and_then(|b| b.clone())
    }

    /// Did the swap refuse because the target is not what was checked?
    pub fn target_changed(&self) -> bool {
        self.changed.load(Ordering::SeqCst)
    }

    fn accepts(&self, size: u64, mtime_ms: i64, id: FileId) -> bool {
        self.plan.matches(size, mtime_ms)
            && self.seen.is_none_or(|s| {
                s.size == size && (s.mtime_ms - mtime_ms).abs() <= SEEN_TOLERANCE_MS
            })
            && self.seen_id.is_none_or(|seen| seen == id)
    }
}

impl ReplaceGuard for CommitCheck<'_> {
    fn replace(&self, staged: &Path, dest: &Path) -> Result<(), FsError> {
        self.hooks.before_commit(self.index, self.name);
        let backup = create_backup_path(dest)?;
        let check = |size, ms, id| self.accepts(size, ms, id);
        match publish::replace_file_checked(
            &extended(staged),
            &extended(dest),
            &extended(&backup),
            &check,
        ) {
            Ok(Replaced::Done) => Ok(()),
            Ok(Replaced::BackupLeft) => {
                if let Ok(mut b) = self.backup_left.lock() {
                    *b = Some(backup.clone());
                }
                Err(FsError::new(
                    ErrorCode::Io,
                    "The new file could not be put in place and the previous file could not be put back; the previous file is kept under its backup name",
                )
                .with_path(backup.to_string_lossy()))
            }
            Ok(Replaced::Changed) => {
                self.changed.store(true, Ordering::SeqCst);
                Err(FsError::new(
                    ErrorCode::Busy,
                    "Destination changed during the copy; nothing was overwritten",
                )
                .with_path(dest.to_string_lossy()))
            }
            Err(e) => Err(FsError::from(e).with_path(dest.to_string_lossy())),
        }
    }
}
