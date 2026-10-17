//! Versioning delete policy (Automation design §5.5, SP-3.2, SE-9).
//!
//! A file that a run deletes, or overwrites, is first moved to
//! `<versioning root>\<Left|Right>\<yyyy-MM-dd HHmmss> #<run id>\<relative path>`.
//!
//! - The destination folders are created component by component with the handle-relative,
//!   no-follow primitives of `archive_publish`: a junction or symlink planted anywhere in the
//!   versioning tree is rejected and nothing is written through it (SE-9, T9).
//! - Same volume: the file is renamed by handle (`FileRenameInformation` with the destination
//!   folder handle as root directory), source and destination both resolved without following
//!   reparse points. The destination is never replaced.
//! - Another volume (the rename answers "not the same device"): the file is copied with
//!   `copy_engine` into the already-open destination folder, the copy's size is verified by
//!   handle, and only then the source is deleted by handle.
//! - The versioning root must be a folder (a file there is refused), is created on first use,
//!   and the run's lease covers it.

use std::ffi::OsStr;
use std::path::Path;

use crate::archive_extract::publish::{self, DirHandle, MoveError, PublishError};
use crate::automation::model::Side;
use crate::ops::CancelToken;
use crate::sync_engine::env::EngineEnv;
use crate::sync_engine::errors::{Classified, ErrorClass, Reason};
use crate::sync_engine::nofollow::{self, ChainError, classify_publish};

/// `<yyyy-MM-dd HHmmss> #<run id>`: the folder of one run.
pub fn stamp_dir_name(stamp: &str, run_id: i64) -> String {
    format!("{stamp} #{run_id}")
}

/// Format a civil time as `yyyy-MM-dd HHmmss`.
pub fn format_stamp(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> String {
    format!("{y:04}-{mo:02}-{d:02} {h:02}{mi:02}{s:02}")
}

/// The current time in the system time zone, as the stamp.
#[cfg(windows)]
pub fn local_stamp_now() -> String {
    use windows_sys::Win32::Foundation::SYSTEMTIME;
    use windows_sys::Win32::System::SystemInformation::GetLocalTime;
    let mut t: SYSTEMTIME = unsafe { std::mem::zeroed() };
    // SAFETY: `t` is a valid out pointer.
    unsafe { GetLocalTime(&mut t) };
    format_stamp(
        i32::from(t.wYear),
        u32::from(t.wMonth),
        u32::from(t.wDay),
        u32::from(t.wHour),
        u32::from(t.wMinute),
        u32::from(t.wSecond),
    )
}

#[cfg(not(windows))]
pub fn local_stamp_now() -> String {
    // Automation ships on Windows only; UTC keeps other hosts (tests) deterministic.
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400) as u32;
    // Civil-from-days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y } as i32;
    format_stamp(y, m, d, rem / 3600, rem % 3600 / 60, rem % 60)
}

/// How a move ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Moved {
    /// Same volume: renamed by handle.
    Renamed,
    /// Another volume: copied, verified, source deleted.
    Copied,
    /// The file is not what the plan saw (size or time differ on its own handle): not moved.
    Changed,
}

/// The facts the plan saw of a file; the move acts only on a file that still matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Expect {
    pub size: u64,
    pub mtime_ms: i64,
}

impl Expect {
    pub fn matches(&self, size: u64, mtime_ms: i64) -> bool {
        size == self.size
            && (mtime_ms - self.mtime_ms).abs() <= crate::sync_engine::MTIME_TOLERANCE_MS
    }
}

/// An opened versioning root plus the folder name of this run.
pub struct VersionStore {
    root: DirHandle,
    root_path: String,
    stamp_dir: String,
}

fn per_item(reason: Reason) -> Classified {
    Classified {
        class: ErrorClass::PerItem,
        reason,
    }
}

impl VersionStore {
    /// Open (creating on first use) the versioning root. A reparse point or a file as the root
    /// is refused.
    pub fn open(root_path: &str, stamp: &str, run_id: i64) -> Result<VersionStore, Classified> {
        let root = nofollow::open_root_folder(root_path, true).map_err(|e| e.classify())?;
        Ok(VersionStore {
            root,
            root_path: root_path.to_string(),
            stamp_dir: stamp_dir_name(stamp, run_id),
        })
    }

    /// The folder components `[<Left|Right>, <stamp> #<run>, <rel parent...>]`.
    fn components<'a>(&'a self, side: Side, rel_parent: &[&'a str]) -> Vec<&'a str> {
        let mut comps = vec![
            match side {
                Side::Left => "Left",
                Side::Right => "Right",
            },
            self.stamp_dir.as_str(),
        ];
        comps.extend_from_slice(rel_parent);
        comps
    }

    /// Display path of the destination folder (only used to hand a path to the copy engine).
    fn dest_dir_path(&self, side: Side, rel_parent: &[&str]) -> String {
        let mut path = std::path::PathBuf::from(&self.root_path);
        for comp in self.components(side, rel_parent) {
            path.push(comp);
        }
        path.to_string_lossy().into_owned()
    }

    /// Move the file `name` (below the opened `src_parent`, living at `src_path`) into the
    /// versioning tree, provided it still matches `expect` on its own handle. A copy that does
    /// not end with the source's size is rolled back (the source stays).
    #[allow(clippy::too_many_arguments)]
    pub fn move_in(
        &self,
        side: Side,
        src_parent: &DirHandle,
        src_path: &Path,
        rel_parent: &[&str],
        name: &str,
        expect: Expect,
        env: &EngineEnv,
        cancel: &CancelToken,
    ) -> Result<Moved, Classified> {
        let comps = self.components(side, rel_parent);
        // FER-562: before the versions folders are created, and again on the opened handle.
        let mut planned = comps.clone();
        planned.push(name);
        crate::sync_engine::write_guard::check_planned(&*env.guard, &self.root, &planned)?;
        let dest = nofollow::open_chain_owned(&self.root, &comps, true)
            .map_err(|e: ChainError| e.classify())?;
        // FER-562: the final path of the versions folder being written to.
        crate::sync_engine::write_guard::check_entry(&*env.guard, &dest, Some(name))?;
        let os = OsStr::new(name);
        let check = |size, ms| expect.matches(size, ms);
        // A cloud placeholder is copied (which reads and so hydrates its bytes) and then deleted;
        // a rename would carry only the stub into the versions tree.
        if !env.mover.needs_copy(src_parent, os) {
            match env.mover.rename_into(src_parent, os, &dest, os, &check) {
                Ok(true) => return Ok(Moved::Renamed),
                Ok(false) => return Ok(Moved::Changed),
                Err(MoveError::CrossDevice) => {}
                Err(MoveError::Exists) => return Err(per_item(Reason::AlreadyExists)),
                Err(MoveError::Publish(e)) => return Err(classify_publish(&e)),
            }
        }
        self.copy_then_delete(
            side, src_parent, src_path, rel_parent, name, expect, &dest, env, cancel,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn copy_then_delete(
        &self,
        side: Side,
        src_parent: &DirHandle,
        src_path: &Path,
        rel_parent: &[&str],
        name: &str,
        expect: Expect,
        dest: &DirHandle,
        env: &EngineEnv,
        cancel: &CancelToken,
    ) -> Result<Moved, Classified> {
        let os = OsStr::new(name);
        // The source as it is NOW, from its own handle: it must still be what the plan saw, and
        // it must be unchanged when the copy is done (exactly) before it is deleted.
        let (expected, expected_ms) =
            publish::file_facts(src_parent, os).map_err(|e| classify_publish(&e))?;
        if !expect.matches(expected, expected_ms) {
            return Ok(Moved::Changed);
        }
        let dest_dir = self.dest_dir_path(side, rel_parent);
        let source = src_path.to_string_lossy().into_owned();
        // The destination folder handle stays open for the whole copy: it cannot be swapped.
        let report = env.copier.copy(
            std::slice::from_ref(&source),
            &dest_dir,
            false,
            true,
            cancel,
        );
        if report.cancelled {
            return Err(Classified {
                class: ErrorClass::PerItem,
                reason: Reason::VerifyFailed,
            });
        }
        if let Some((_, class)) = report.failed.first() {
            return Err(*class);
        }
        if !report.done.contains(&source) {
            // Skipped by the copy engine: the destination name appeared meanwhile.
            return Err(per_item(Reason::AlreadyExists));
        }
        let copied = publish::file_size(dest, os);
        if !matches!(copied, Ok(n) if n == expected) {
            // A short or wrong copy: remove it, keep the source.
            let _ = publish::delete_file(dest, os);
            return Err(per_item(Reason::VerifyFailed));
        }
        let unchanged = |size, ms| size == expected && ms == expected_ms;
        match publish::delete_file_if(src_parent, os, &unchanged) {
            Ok(true) => Ok(Moved::Copied),
            // The source changed while it was copied: keep it, drop the stale version.
            Ok(false) => {
                let _ = publish::delete_file(dest, os);
                Ok(Moved::Changed)
            }
            // The source stays and the copy stays: nothing is lost, the op is reported failed.
            Err(PublishError::NotFound) => Ok(Moved::Copied),
            Err(e) => Err(classify_publish(&e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamp_folder_name_has_only_fat_safe_characters() {
        let stamp = format_stamp(2026, 3, 9, 7, 5, 1);
        assert_eq!(stamp, "2026-03-09 070501");
        let name = stamp_dir_name(&stamp, 42);
        assert_eq!(name, "2026-03-09 070501 #42");
        assert!(!name.contains([':', '\\', '/', '*', '?', '"', '<', '>', '|']));
        let now = local_stamp_now();
        assert_eq!(now.len(), "2026-03-09 070501".len());
        assert!(!now.contains(':'));
    }
}
