//! FER-576 (review R1, B1): the Recycle Bin gate of the AI delete.
//!
//! The AI chat promises "to the Recycle Bin, never permanent". `trash::delete` only asks the shell to
//! recycle IF POSSIBLE: on a fixed drive the shell still deletes for good when the volume is set to
//! "Don't move files to the Recycle Bin" (`NukeOnDelete`), when the bin size is 0 or the no-recycle policy
//! is set, when the item (or the whole batch) does not fit the volume's bin quota, or when the item lives
//! on another volume mounted into a folder or reached through a junction. This module closes those holes:
//!
//! 1. [`assess`] decides BEFORE anything is touched, per item, from the volume the item really lives on
//!    (not from its drive letter): fixed local volume, not a mount point or redirect, bin state `Usable`
//!    (the same registry gate the sync engine uses), the real recursive size within HALF of the bin's
//!    capacity. One failing item refuses the whole batch.
//! 2. [`delete_verified`] sends the items one at a time and, through a handle opened BEFORE the shell is
//!    called, checks that the same object now lives inside `$Recycle.Bin` (the pattern of
//!    `sync_engine::delete_policy`). Anything else stops the batch and is reported as a permanent delete.
//!
//! Everything that touches Windows is behind [`RecycleEnv`] / [`RecycleIo`], so each condition is tested
//! with a fake.

use crate::domain::ErrorCode;
use crate::mutations::FailedItem;
use crate::sync_engine::env::BinState;
use serde::Serialize;
use std::collections::HashMap;

/// `GetDriveTypeW` of a fixed local disk, the only kind of volume trusted to have a Recycle Bin.
pub const DRIVE_FIXED: u32 = 3;
/// Share of a volume the bin may take when the volume has no explicit limit (conservative).
pub const DEFAULT_BIN_PERCENT: u64 = 5;
/// Items must fit in this fraction of the bin capacity (the bin may already hold other things).
pub const CAPACITY_HEADROOM_DIVISOR: u64 = 2;
/// Entries one size walk may visit before it gives up (the answer is then "unknown", i.e. refused).
pub const MAX_CENSUS_ENTRIES: u64 = 200_000;
/// Longest one size walk may take.
pub const MAX_CENSUS_SECONDS: u64 = 5;

/// Why an item was refused (stable codes; the UI maps them to text).
pub mod reason {
    pub const NETWORK_OR_UNC: &str = "network_or_unc";
    pub const RESERVED_NAME: &str = "reserved_name";
    pub const NO_RECYCLE_BIN: &str = "no_recycle_bin";
    pub const RECYCLE_DISABLED: &str = "recycle_disabled";
    pub const RECYCLE_UNKNOWN: &str = "recycle_unknown";
    pub const MOUNT_POINT: &str = "mount_point";
    pub const REDIRECTED: &str = "redirected";
    pub const VOLUME_UNKNOWN: &str = "volume_unknown";
    pub const LINK_ITEM: &str = "link_item";
    pub const SIZE_UNKNOWN: &str = "size_unknown";
    pub const CAPACITY_UNKNOWN: &str = "capacity_unknown";
    pub const TOO_BIG: &str = "too_big";
}

/// What one tree holds, counted without following links.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Census {
    pub bytes: u64,
    pub files: u64,
    pub dirs: u64,
    /// Folders with nothing in them: archives list these as their own entry (a non-empty folder is implied).
    pub empty_dirs: u64,
    /// Symlinks / junctions inside the tree: not descended, and an archive skips them.
    pub links: u64,
}

impl Census {
    pub fn entries(&self) -> u64 {
        self.files + self.dirs
    }

    /// The members a 7z / ZIP of this tree lists: every file plus every empty folder.
    pub fn archive_members(&self) -> u64 {
        self.files + self.empty_dirs
    }
}

/// The volume an item lives on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeFacts {
    /// Drive-letter root such as `D:\`.
    pub root: String,
    pub drive_type: u32,
    pub total_bytes: Option<u64>,
}

/// The outside world of [`assess`].
pub trait RecycleEnv {
    /// Is the item itself a symlink or junction? `Err` = could not look.
    fn is_link(&self, path: &str) -> Option<bool>;
    /// The volume the item REALLY lives on (its parent resolved through junctions and mount points).
    /// `Err(code)` is one of [`reason`].
    fn volume_of(&self, path: &str) -> Result<VolumeFacts, &'static str>;
    fn bin_state(&self, root: &str) -> BinState;
    /// The explicit bin limit of the volume in MB; `None` = no explicit limit.
    fn bin_capacity_mb(&self, root: &str) -> Option<u64>;
    /// Size and counts of the tree; `Err(code)` when it could not be measured completely.
    fn census(&self, path: &str) -> Result<Census, &'static str>;
}

/// The verdict for one item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assessment {
    pub path: String,
    pub ok: bool,
    pub reason: Option<&'static str>,
    pub census: Census,
}

fn lexical_blocker(path: &str) -> Option<&'static str> {
    let plain = crate::path_guard::strip_verbatim(path);
    let bytes = plain.as_bytes();
    let drive_letter_path = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/');
    if !drive_letter_path {
        return Some(reason::NETWORK_OR_UNC);
    }
    if crate::path_guard::has_reserved_device_component(&plain) {
        return Some(reason::RESERVED_NAME);
    }
    None
}

/// Decide, for every path, whether the shell is guaranteed to move it into the Recycle Bin.
/// All-or-nothing is the caller's job: ONE `ok == false` refuses the whole batch.
pub fn assess(paths: &[String], env: &dyn RecycleEnv) -> Vec<Assessment> {
    struct Volume {
        bytes: u64,
        total: Option<u64>,
    }
    let mut volumes: HashMap<String, Volume> = HashMap::new();
    let mut verdicts: Vec<(Assessment, Option<String>)> = Vec::with_capacity(paths.len());
    for path in paths {
        let mut verdict = Assessment {
            path: path.clone(),
            ok: false,
            reason: None,
            census: Census::default(),
        };
        let mut root: Option<String> = None;
        verdict.reason = (|| {
            if let Some(code) = lexical_blocker(path) {
                return Some(code);
            }
            let volume = match env.volume_of(path) {
                Ok(volume) => volume,
                Err(code) => return Some(code),
            };
            if volume.drive_type != DRIVE_FIXED {
                return Some(reason::NO_RECYCLE_BIN);
            }
            match env.bin_state(&volume.root) {
                BinState::Usable => {}
                BinState::Disabled => return Some(reason::RECYCLE_DISABLED),
                BinState::Unknown => return Some(reason::RECYCLE_UNKNOWN),
            }
            match env.is_link(path) {
                Some(false) => {}
                Some(true) => return Some(reason::LINK_ITEM),
                None => return Some(reason::SIZE_UNKNOWN),
            }
            match env.census(path) {
                Ok(census) => verdict.census = census,
                Err(code) => return Some(code),
            }
            let entry = volumes.entry(volume.root.clone()).or_insert(Volume {
                bytes: 0,
                total: volume.total_bytes,
            });
            entry.bytes = entry.bytes.saturating_add(verdict.census.bytes);
            root = Some(volume.root);
            None
        })();
        verdict.ok = verdict.reason.is_none();
        verdicts.push((verdict, root));
    }
    // The quota is per volume and shared by the whole batch.
    for (verdict, root) in &mut verdicts {
        let Some(root) = root.as_deref().filter(|_| verdict.ok) else {
            continue;
        };
        let volume = &volumes[root];
        let capacity = env
            .bin_capacity_mb(root)
            .map(|mb| mb.saturating_mul(1024 * 1024))
            .or_else(|| volume.total.map(|total| total / 100 * DEFAULT_BIN_PERCENT));
        match capacity {
            None => {
                verdict.ok = false;
                verdict.reason = Some(reason::CAPACITY_UNKNOWN);
            }
            Some(capacity) if volume.bytes.saturating_mul(CAPACITY_HEADROOM_DIVISOR) > capacity => {
                verdict.ok = false;
                verdict.reason = Some(reason::TOO_BIG);
            }
            Some(_) => {}
        }
    }
    verdicts.into_iter().map(|(verdict, _)| verdict).collect()
}

/// The first refusal of a batch, if any.
pub fn first_refusal(verdicts: &[Assessment]) -> Option<&Assessment> {
    verdicts.iter().find(|verdict| !verdict.ok)
}

// ── The verified delete ──

/// An open handle on an item, held across the shell call.
pub trait ItemProbe {
    /// Marked for deletion while still named (a classic delete): gone for good once closed.
    fn delete_pending(&self) -> bool;
    /// Where the OS says the SAME object is now (`None`: it has no path any more).
    fn final_path(&self) -> Option<String>;
}

pub trait RecycleIo {
    /// Open the item for observation (no-follow, shared with the shell).
    fn open_probe(&self, path: &str) -> Result<Box<dyn ItemProbe>, String>;
    /// The shell call for ONE item.
    fn trash(&self, path: &str) -> Result<(), String>;
    /// Is the volume's bin still usable right now (settings can change between the gate and the call)?
    fn still_usable(&self, path: &str) -> bool;
}

/// What a verified delete did.
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecycleReport {
    pub requested: usize,
    /// Items verified to be inside `$Recycle.Bin`.
    pub deleted: usize,
    pub failed: Vec<FailedItem>,
    /// Items the shell deleted for good (or that left no trace in the bin). The batch stopped there.
    pub bypassed: Vec<String>,
    /// Items not attempted because a permanent delete was detected before them.
    pub skipped: usize,
}

fn in_bin(final_path: &str) -> bool {
    final_path.to_lowercase().contains(r"\$recycle.bin\")
}

fn failed(path: &str, code: ErrorCode, message: &str) -> FailedItem {
    FailedItem {
        path: path.to_owned(),
        code,
        message: message.to_owned(),
    }
}

/// Send the items to the Recycle Bin one at a time and verify each landed there. Stops the batch at the
/// first item that did not (it is reported in `bypassed`, never as a success).
pub fn delete_verified(paths: &[String], io: &dyn RecycleIo) -> RecycleReport {
    let mut report = RecycleReport {
        requested: paths.len(),
        ..RecycleReport::default()
    };
    for (index, path) in paths.iter().enumerate() {
        if !io.still_usable(path) {
            // The settings changed since the gate: stop BEFORE the shell can delete for good.
            report.failed.push(failed(
                path,
                ErrorCode::Unsupported,
                "The Recycle Bin of this drive is no longer available; nothing more was deleted",
            ));
            report.skipped = paths.len() - index - 1;
            return report;
        }
        let probe = match io.open_probe(path) {
            Ok(probe) => probe,
            Err(message) => {
                report.failed.push(failed(path, ErrorCode::Io, &message));
                continue;
            }
        };
        if let Err(message) = io.trash(path) {
            report.failed.push(failed(path, ErrorCode::Io, &message));
            continue;
        }
        let original = crate::path_guard::strip_verbatim(path).to_lowercase();
        let landed = if probe.delete_pending() {
            None
        } else {
            match probe.final_path() {
                Some(final_path) if in_bin(&final_path) => Some(true),
                // Still where it was: the shell did nothing.
                Some(final_path)
                    if crate::path_guard::strip_verbatim(&final_path).to_lowercase()
                        == original =>
                {
                    Some(false)
                }
                _ => None,
            }
        };
        match landed {
            Some(true) => report.deleted += 1,
            Some(false) => {
                report
                    .failed
                    .push(failed(path, ErrorCode::Io, "The item was not moved"))
            }
            None => {
                report.bypassed.push(path.clone());
                report.skipped = paths.len() - index - 1;
                return report;
            }
        }
    }
    report
}

// ── Production implementation (Windows) ──

/// Walk a tree without following links, counting bytes, files, folders and links. Gives up with
/// `size_unknown` on any error, after [`MAX_CENSUS_ENTRIES`] entries or after [`MAX_CENSUS_SECONDS`].
pub fn census_tree(path: &str) -> Result<Census, &'static str> {
    use std::time::{Duration, Instant};
    let deadline = Instant::now() + Duration::from_secs(MAX_CENSUS_SECONDS);
    let mut census = Census::default();
    let mut visited: u64 = 0;
    let mut stack = vec![std::path::PathBuf::from(path)];
    while let Some(current) = stack.pop() {
        visited += 1;
        if visited > MAX_CENSUS_ENTRIES || Instant::now() > deadline {
            return Err(reason::SIZE_UNKNOWN);
        }
        let meta = std::fs::symlink_metadata(&current).map_err(|_| reason::SIZE_UNKNOWN)?;
        if is_reparse(&meta) {
            census.links += 1;
            continue;
        }
        if meta.is_dir() {
            census.dirs += 1;
            let mut children = 0u64;
            for entry in std::fs::read_dir(&current).map_err(|_| reason::SIZE_UNKNOWN)? {
                stack.push(entry.map_err(|_| reason::SIZE_UNKNOWN)?.path());
                children += 1;
            }
            if children == 0 {
                census.empty_dirs += 1;
            }
        } else {
            census.files += 1;
            census.bytes = census.bytes.saturating_add(meta.len());
        }
    }
    Ok(census)
}

#[cfg(windows)]
fn is_reparse(meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_reparse(meta: &std::fs::Metadata) -> bool {
    meta.file_type().is_symlink()
}

/// Production environment: the real volumes, the shell's registry settings and the disk.
pub struct SystemRecycleEnv;

#[cfg(windows)]
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

impl RecycleEnv for SystemRecycleEnv {
    fn is_link(&self, path: &str) -> Option<bool> {
        std::fs::symlink_metadata(path)
            .ok()
            .map(|meta| is_reparse(&meta))
    }

    #[cfg(windows)]
    fn volume_of(&self, path: &str) -> Result<VolumeFacts, &'static str> {
        use windows_sys::Win32::Storage::FileSystem::{
            GetDiskFreeSpaceExW, GetDriveTypeW, GetVolumePathNameW,
        };
        let plain = crate::path_guard::strip_verbatim(path);
        let parent = std::path::Path::new(&plain)
            .parent()
            .ok_or(reason::VOLUME_UNKNOWN)?;
        // The real place of the folder the item sits in: junctions and symlinks in its ancestors are resolved.
        let canonical = std::fs::canonicalize(parent).map_err(|_| reason::VOLUME_UNKNOWN)?;
        let canonical = crate::path_guard::strip_verbatim(&canonical.to_string_lossy());
        if canonical.starts_with(r"\\") {
            return Err(reason::NETWORK_OR_UNC);
        }
        let same_letter = |a: &str, b: &str| {
            matches!(
                (a.chars().next(), b.chars().next()),
                (Some(x), Some(y)) if x.eq_ignore_ascii_case(&y)
            ) && a.as_bytes().get(1) == Some(&b':')
                && b.as_bytes().get(1) == Some(&b':')
        };
        if !same_letter(&plain, &canonical) {
            return Err(reason::REDIRECTED);
        }
        let mut buf = vec![0u16; 1024];
        // SAFETY: NUL-terminated input; `buf` is writable for its length.
        let ok = unsafe {
            GetVolumePathNameW(
                wide(&canonical).as_ptr(),
                buf.as_mut_ptr(),
                buf.len() as u32,
            )
        };
        if ok == 0 {
            return Err(reason::VOLUME_UNKNOWN);
        }
        let len = buf.iter().position(|c| *c == 0).unwrap_or(0);
        let root = String::from_utf16_lossy(&buf[..len]);
        // A volume mounted into a folder has its own bin settings and may be anything: refuse.
        if root.len() != 3 || !root.ends_with(":\\") || !same_letter(&root, &plain) {
            return Err(reason::MOUNT_POINT);
        }
        let root = root.to_ascii_uppercase();
        // SAFETY: NUL-terminated root.
        let drive_type = unsafe { GetDriveTypeW(wide(&root).as_ptr()) };
        let mut total: u64 = 0;
        // SAFETY: NUL-terminated root; `total` is a writable out parameter; the others may be null.
        let have_total = unsafe {
            GetDiskFreeSpaceExW(
                wide(&root).as_ptr(),
                std::ptr::null_mut(),
                &mut total,
                std::ptr::null_mut(),
            )
        } != 0;
        Ok(VolumeFacts {
            root,
            drive_type,
            total_bytes: have_total.then_some(total),
        })
    }

    #[cfg(not(windows))]
    fn volume_of(&self, _path: &str) -> Result<VolumeFacts, &'static str> {
        Err(reason::VOLUME_UNKNOWN)
    }

    fn bin_state(&self, root: &str) -> BinState {
        use crate::sync_engine::env::{BinConfig, RegistryBinConfig};
        RegistryBinConfig.state(root)
    }

    fn bin_capacity_mb(&self, root: &str) -> Option<u64> {
        crate::sync_engine::env::bin_max_capacity_mb(root).map(u64::from)
    }

    fn census(&self, path: &str) -> Result<Census, &'static str> {
        census_tree(path)
    }
}

/// Production I/O of the verified delete.
pub struct SystemRecycleIo;

#[cfg(windows)]
struct HandleProbe(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl Drop for HandleProbe {
    fn drop(&mut self) {
        // SAFETY: the handle was opened by `open_probe` and is closed exactly once.
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

#[cfg(windows)]
impl ItemProbe for HandleProbe {
    fn delete_pending(&self) -> bool {
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_STANDARD_INFO, FileStandardInfo, GetFileInformationByHandleEx,
        };
        let mut info: FILE_STANDARD_INFO = unsafe { std::mem::zeroed() };
        // SAFETY: valid handle; `info` is a writable out structure of the given size.
        let ok = unsafe {
            GetFileInformationByHandleEx(
                self.0,
                FileStandardInfo,
                &mut info as *mut _ as *mut core::ffi::c_void,
                std::mem::size_of::<FILE_STANDARD_INFO>() as u32,
            )
        };
        // Unknown counts as pending: the caller then treats it as "removed for good".
        ok == 0 || info.DeletePending
    }

    fn final_path(&self) -> Option<String> {
        use windows_sys::Win32::Storage::FileSystem::GetFinalPathNameByHandleW;
        let mut buf = vec![0u16; 1024];
        loop {
            // SAFETY: valid handle; `buf` is writable for `buf.len()` u16s.
            let n =
                unsafe { GetFinalPathNameByHandleW(self.0, buf.as_mut_ptr(), buf.len() as u32, 0) }
                    as usize;
            if n == 0 {
                return None;
            }
            if n < buf.len() {
                return Some(String::from_utf16_lossy(&buf[..n]));
            }
            buf.resize(n + 1, 0);
        }
    }
}

impl RecycleIo for SystemRecycleIo {
    #[cfg(windows)]
    fn open_probe(&self, path: &str) -> Result<Box<dyn ItemProbe>, String> {
        use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
        use windows_sys::Win32::Storage::FileSystem::{
            CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
            FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
            OPEN_EXISTING,
        };
        // SAFETY: NUL-terminated path; the handle is owned by the returned probe. Shared with the shell
        // (including DELETE) so the move into the bin is not blocked; opened without following a link.
        let handle = unsafe {
            CreateFileW(
                wide(path).as_ptr(),
                FILE_READ_ATTRIBUTES,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE || handle.is_null() {
            return Err("The item cannot be opened".to_owned());
        }
        Ok(Box::new(HandleProbe(handle)))
    }

    #[cfg(not(windows))]
    fn open_probe(&self, _path: &str) -> Result<Box<dyn ItemProbe>, String> {
        Err("unsupported platform".to_owned())
    }

    fn trash(&self, path: &str) -> Result<(), String> {
        let report = crate::mutations::delete_to_trash(&[path.to_owned()]);
        match report.failed.into_iter().next() {
            None if report.deleted == 1 => Ok(()),
            None => Err("The item was not moved".to_owned()),
            Some(item) => Err(item.message),
        }
    }

    fn still_usable(&self, path: &str) -> bool {
        let env = SystemRecycleEnv;
        match env.volume_of(path) {
            Ok(volume) => {
                volume.drive_type == DRIVE_FIXED && env.bin_state(&volume.root) == BinState::Usable
            }
            Err(_) => false,
        }
    }
}

#[cfg(test)]
#[path = "recycle_gate_tests.rs"]
mod tests;
