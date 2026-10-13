//! Hotlist persistence (FER-458). One file `hotlist.json` (schema v2) at a location the user may
//! move (e.g. into a cloud drive). Safety rules:
//! - never overwrite silently: a changed-from-outside file makes the write fail with `Conflict`;
//! - a file that is unreadable / invalid / a cloud placeholder / lost puts the store in read-only
//!   mode and the last valid copy (`hotlist.cache.json` in the config dir) is shown instead;
//! - cloud placeholders (OneDrive/cloud sync VFS) are never read, so they are never hydrated;
//! - writes go to a temp file that is flushed (`sync_all`) before it is renamed into place; the
//!   previous on-disk version becomes `.bak` (FER-467 R6);
//! - the hash check and the replacement happen on ONE locked handle: the current file is opened
//!   with read sharing only, its bytes are hashed through that handle, and that same handle renames
//!   it to `.bak`, so nobody can change it between the check and the write (FER-467 R6);
//! - a custom location must be really local: no mapped network drive, junction/symlink, reserved
//!   device name, alternate data stream or Ferrum's own config files (FER-467 R7, `path_guard`);
//! - no raw path ever reaches a log line or an error kind.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::model::{HotlistDoc, MAX_FILE_BYTES};

pub const FILE_NAME: &str = "hotlist.json";
pub const CACHE_NAME: &str = "hotlist.cache.json";
pub const LOCATION_NAME: &str = "hotlist-location.json";
pub const APPROVALS_NAME: &str = "hotlist-approvals.json";
const MAX_APPROVALS: usize = 500;

/// Content fingerprint used for external-change detection (mtime is unreliable under sync).
pub type Stamp = String;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreError {
    /// File changed outside Ferrum since it was read; the caller must reload.
    Conflict,
    /// Store is read-only (file unavailable); nothing was written.
    ReadOnly,
    InvalidDoc,
    InvalidPath,
    TargetExists,
    TargetMissing,
    TargetUnusable,
    ParentMissing,
    Storage,
    /// FER-467 R7: custom location on a network drive / UNC / remote folder.
    NetworkLocation,
    /// FER-467 R7: custom location goes through a junction or symlink.
    LinkedLocation,
    /// FER-467 R7: reserved device name, trailing dot/space, or one of Ferrum's own config files.
    ReservedLocation,
    /// FER-467 R7: alternate data stream.
    StreamLocation,
}

impl StoreError {
    pub fn kind(self) -> &'static str {
        match self {
            Self::Conflict => "conflict",
            Self::ReadOnly => "read_only",
            Self::InvalidDoc => "invalid_doc",
            Self::InvalidPath => "invalid_path",
            Self::TargetExists => "target_exists",
            Self::TargetMissing => "target_missing",
            Self::TargetUnusable => "target_unusable",
            Self::ParentMissing => "parent_missing",
            Self::Storage => "storage",
            Self::NetworkLocation => "location_network",
            Self::LinkedLocation => "location_link",
            Self::ReservedLocation => "location_reserved",
            Self::StreamLocation => "location_stream",
        }
    }
}

/// What reading the hotlist file found.
#[derive(Debug, PartialEq)]
pub enum Read {
    Ok(HotlistDoc, Stamp),
    Absent,
    /// Cloud placeholder: reading would download it, so it is not read.
    Cloud,
    Unreadable,
    Invalid,
}

fn stamp_of(bytes: &[u8]) -> Stamp {
    hex::encode(Sha256::digest(bytes))
}

#[cfg(windows)]
fn is_cloud_placeholder(meta: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    // OFFLINE | RECALL_ON_OPEN | RECALL_ON_DATA_ACCESS
    meta.file_attributes() & (0x0000_1000 | 0x0004_0000 | 0x0040_0000) != 0
}

#[cfg(not(windows))]
fn is_cloud_placeholder(_meta: &fs::Metadata) -> bool {
    false
}

pub fn read_file(path: &Path) -> Read {
    let meta = match fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // A missing parent means the drive/folder is not there (yet): not "absent".
            return match path.parent() {
                Some(p) if !p.as_os_str().is_empty() && !p.exists() => Read::Unreadable,
                _ => Read::Absent,
            };
        }
        Err(_) => return Read::Unreadable,
    };
    if !meta.is_file() {
        return Read::Unreadable;
    }
    if is_cloud_placeholder(&meta) {
        return Read::Cloud;
    }
    if meta.len() > MAX_FILE_BYTES {
        return Read::Invalid;
    }
    match fs::read(path) {
        Ok(bytes) => match HotlistDoc::parse(&bytes) {
            Some(doc) => Read::Ok(doc, stamp_of(&bytes)),
            None => Read::Invalid,
        },
        Err(_) => Read::Unreadable,
    }
}

/// Temp file next to `path`, named so sync clients skip it (`~*.tmp` is in cloud sync default
/// exclude list) and so it never looks like a hotlist.
fn temp_for(path: &Path) -> Result<PathBuf, StoreError> {
    let dir = path.parent().ok_or(StoreError::InvalidPath)?;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or(StoreError::InvalidPath)?;
    Ok(dir.join(format!("~{name}.{}.tmp", std::process::id())))
}

/// Write + flush to disk (`sync_all`) before anything is renamed onto it (FER-467 R6): a power cut
/// after the rename can no longer leave a 0-byte hotlist behind.
fn write_synced(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = fs::File::create(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    let dir = path.parent().ok_or(StoreError::InvalidPath)?;
    if !dir.as_os_str().is_empty() {
        fs::create_dir_all(dir).map_err(|_| StoreError::Storage)?;
    }
    let tmp = temp_for(path)?;
    if write_synced(&tmp, bytes).is_err() {
        let _ = fs::remove_file(&tmp);
        return Err(StoreError::Storage);
    }
    fs::rename(&tmp, path).map_err(|_| {
        let _ = fs::remove_file(&tmp);
        StoreError::Storage
    })
}

/// Why a checked replacement did not happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Replace {
    /// The file is not the version we read (or appeared/vanished, or is being written right now).
    Changed,
    /// The file cannot be read safely (placeholder, not a file, I/O error): read-only.
    Unavailable,
    Storage,
}

#[cfg(windows)]
mod locked {
    //! FER-467 R6: replace a file only if it still holds the bytes we expect, with no window in
    //! between. The current file is opened with read sharing only (nobody else can write, delete or
    //! rename it while we hold it), hashed through that handle, and renamed to `.bak` THROUGH THAT
    //! HANDLE; then the flushed temp file is moved into the free name without replacing anything.
    use std::fs::{File, OpenOptions};
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use std::path::Path;

    const GENERIC_READ: u32 = 0x8000_0000;
    const DELETE: u32 = 0x0001_0000;
    const FILE_SHARE_READ: u32 = 0x1;

    pub fn open(path: &Path) -> std::io::Result<File> {
        OpenOptions::new()
            .access_mode(GENERIC_READ | DELETE)
            .share_mode(FILE_SHARE_READ)
            .open(path)
    }

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }

    /// Rename the open file itself (replacing `to` if it exists).
    pub fn rename_handle(file: &File, to: &Path) -> std::io::Result<()> {
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_RENAME_INFO, FileRenameInfo, SetFileInformationByHandle,
        };
        let name: Vec<u16> = to.as_os_str().encode_wide().collect();
        let header = std::mem::offset_of!(FILE_RENAME_INFO, FileName);
        let size = header + (name.len() + 1) * 2;
        let mut buf = vec![
            0u64;
            size.max(std::mem::size_of::<FILE_RENAME_INFO>())
                .div_ceil(8)
        ];
        let info = buf.as_mut_ptr().cast::<FILE_RENAME_INFO>();
        // SAFETY: `buf` is zeroed, 8-byte aligned and large enough for the header plus the name
        // and its NUL; the handle is valid for the lifetime of `file`.
        let ok = unsafe {
            (*info).Anonymous.ReplaceIfExists = true;
            (*info).FileNameLength = (name.len() * 2) as u32;
            std::ptr::copy_nonoverlapping(
                name.as_ptr(),
                std::ptr::addr_of_mut!((*info).FileName).cast::<u16>(),
                name.len(),
            );
            SetFileInformationByHandle(
                file.as_raw_handle() as _,
                FileRenameInfo,
                info.cast(),
                size as u32,
            )
        };
        if ok == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    /// Move without replacing: fails if something took the name in the meantime.
    pub fn move_no_replace(from: &Path, to: &Path) -> std::io::Result<()> {
        use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_WRITE_THROUGH, MoveFileExW};
        let (f, t) = (wide(from), wide(to));
        // SAFETY: both buffers are NUL-terminated UTF-16.
        if unsafe { MoveFileExW(f.as_ptr(), t.as_ptr(), MOVEFILE_WRITE_THROUGH) } == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

#[cfg(not(windows))]
mod locked {
    use std::fs::File;
    use std::path::Path;

    pub fn open(path: &Path) -> std::io::Result<File> {
        File::open(path)
    }

    pub fn rename_handle(_file: &File, _to: &Path) -> std::io::Result<()> {
        Err(std::io::Error::other("rename by handle needs Windows"))
    }

    pub fn move_no_replace(from: &Path, to: &Path) -> std::io::Result<()> {
        if to.exists() {
            return Err(std::io::ErrorKind::AlreadyExists.into());
        }
        std::fs::rename(from, to)
    }
}

fn bak_of(path: &Path) -> PathBuf {
    let mut bak = path.as_os_str().to_owned();
    bak.push(".bak");
    PathBuf::from(bak)
}

/// FER-467 R6: write `bytes` to `path` only if the file still is the version whose stamp is
/// `expected` (`None` = there must be no file). See `locked` for how the window is closed.
fn replace_checked(path: &Path, expected: Option<&str>, bytes: &[u8]) -> Result<(), Replace> {
    // Never open (let alone read) a cloud placeholder: that could download it.
    match fs::metadata(path) {
        Ok(meta) if !meta.is_file() || is_cloud_placeholder(&meta) => {
            return Err(Replace::Unavailable);
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if expected.is_some() {
                return Err(Replace::Changed);
            }
            if path.parent().is_some_and(|p| !p.exists()) {
                return Err(Replace::Unavailable);
            }
            let tmp = temp_for(path).map_err(|_| Replace::Storage)?;
            if write_synced(&tmp, bytes).is_err() {
                let _ = fs::remove_file(&tmp);
                return Err(Replace::Storage);
            }
            return locked::move_no_replace(&tmp, path).map_err(|e| {
                let _ = fs::remove_file(&tmp);
                if path.exists() || e.kind() == std::io::ErrorKind::AlreadyExists {
                    Replace::Changed
                } else {
                    Replace::Storage
                }
            });
        }
        Err(_) => return Err(Replace::Unavailable),
    }
    let file = match locked::open(path) {
        Ok(f) => f,
        // ERROR_SHARING_VIOLATION: someone is writing it right now.
        Err(e) if e.raw_os_error() == Some(32) => return Err(Replace::Changed),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(Replace::Changed),
        Err(_) => return Err(Replace::Unavailable),
    };
    let meta = file.metadata().map_err(|_| Replace::Unavailable)?;
    if !meta.is_file() || is_cloud_placeholder(&meta) {
        return Err(Replace::Unavailable);
    }
    let mut current = Vec::new();
    {
        use std::io::Read as _;
        (&file)
            .take(MAX_FILE_BYTES + 1)
            .read_to_end(&mut current)
            .map_err(|_| Replace::Unavailable)?;
    }
    if expected != Some(stamp_of(&current).as_str()) {
        return Err(Replace::Changed);
    }
    let tmp = temp_for(path).map_err(|_| Replace::Storage)?;
    if write_synced(&tmp, bytes).is_err() {
        let _ = fs::remove_file(&tmp);
        return Err(Replace::Storage);
    }
    if cfg!(windows) {
        if locked::rename_handle(&file, &bak_of(path)).is_err() {
            let _ = fs::remove_file(&tmp);
            return Err(Replace::Storage);
        }
        drop(file);
        locked::move_no_replace(&tmp, path).map_err(|_| {
            // Someone created a new file in the instant the name was free: theirs stays, ours goes;
            // the verified previous version is in `.bak`.
            let _ = fs::remove_file(&tmp);
            Replace::Changed
        })
    } else {
        drop(file);
        let _ = fs::copy(path, bak_of(path));
        fs::rename(&tmp, path).map_err(|_| {
            let _ = fs::remove_file(&tmp);
            Replace::Storage
        })
    }
}

fn serialize(doc: &HotlistDoc) -> Result<Vec<u8>, StoreError> {
    doc.validate().map_err(|_| StoreError::InvalidDoc)?;
    serde_json::to_vec_pretty(doc).map_err(|_| StoreError::Storage)
}

#[derive(Serialize, Deserialize)]
struct LocationFile {
    path: String,
}

/// Where the hotlist lives right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Loc {
    Default,
    Custom(PathBuf),
    /// A custom location is configured but fails the safety checks (FER-467 R7): nothing is read
    /// from or written to it; the store is read-only with the cached copy.
    Refused(String, StoreError),
}

impl Loc {
    fn file(&self, dir: &Path) -> Option<PathBuf> {
        match self {
            Loc::Default => Some(dir.join(FILE_NAME)),
            Loc::Custom(p) => Some(p.clone()),
            Loc::Refused(..) => None,
        }
    }

    fn is_custom(&self) -> bool {
        !matches!(self, Loc::Default)
    }

    fn shown(&self) -> Option<String> {
        match self {
            Loc::Default => None,
            Loc::Custom(p) => Some(p.to_string_lossy().into_owned()),
            Loc::Refused(raw, _) => Some(raw.clone()),
        }
    }
}

/// The configured hotlist location, re-checked (string AND filesystem checks) on every use.
pub fn read_location(dir: &Path) -> Loc {
    let Some(parsed) = fs::read(dir.join(LOCATION_NAME))
        .ok()
        .and_then(|raw| serde_json::from_slice::<LocationFile>(&raw).ok())
    else {
        return Loc::Default;
    };
    let path = PathBuf::from(&parsed.path);
    match validate_location_in(&path, dir) {
        Ok(()) => Loc::Custom(path),
        Err(e) => Loc::Refused(parsed.path, e),
    }
}

/// Path of the hotlist file in use (`None` while a refused custom location is configured).
pub fn location_file(dir: &Path) -> Option<PathBuf> {
    read_location(dir).file(dir)
}

fn write_location(dir: &Path, location: Option<&Path>) -> Result<(), StoreError> {
    match location {
        Some(p) => {
            let text = serde_json::to_vec(&LocationFile {
                path: p.to_string_lossy().into_owned(),
            })
            .map_err(|_| StoreError::Storage)?;
            write_atomic(&dir.join(LOCATION_NAME), &text)
        }
        None => match fs::remove_file(dir.join(LOCATION_NAME)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(StoreError::Storage),
        },
    }
}

/// A custom location must be an absolute local drive path to a `.json` file: no UNC/device
/// paths (they could contact the network), no `..`. String checks only; see
/// `validate_location_in` for the full check.
pub fn validate_location(path: &Path) -> Result<(), StoreError> {
    let text = path.to_str().ok_or(StoreError::InvalidPath)?;
    if !crate::is_plausible_path(text)
        || text
            .chars()
            .any(|c| c.is_control() || super::model::is_format_char(c))
    {
        return Err(StoreError::InvalidPath);
    }
    let b = text.as_bytes();
    let drive = b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\';
    let unix_abs = !cfg!(windows) && text.starts_with('/');
    if !(drive || unix_abs) || text.starts_with("\\\\") {
        return Err(StoreError::InvalidPath);
    }
    if path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(StoreError::InvalidPath);
    }
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or(StoreError::InvalidPath)?;
    if !name.to_ascii_lowercase().ends_with(".json") {
        return Err(StoreError::InvalidPath);
    }
    Ok(())
}

/// Ferrum's own files in the config dir that a custom hotlist location must never point at.
const RESERVED_IN_CONFIG: [&str; 3] = [CACHE_NAME, LOCATION_NAME, APPROVALS_NAME];

fn same_dir(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| {
        p.to_string_lossy()
            .replace('/', "\\")
            .trim_end_matches('\\')
            .to_lowercase()
    };
    norm(a) == norm(b)
}

/// FER-467 R7: the full check for a custom location. String checks (`validate_location`), then
/// the real ones through `path_guard::check_local_location`: mapped network drive, junction or
/// symlink in the path, a folder that really lives on the network, reserved device names (`CON`,
/// `NUL.json`, ...), trailing dots/spaces, alternate data streams; and never one of Ferrum's own
/// config files.
pub fn validate_location_in(path: &Path, dir: &Path) -> Result<(), StoreError> {
    #[cfg(windows)]
    {
        use crate::path_guard::{LocalRefusal, check_local_location};
        let text = path.to_str().ok_or(StoreError::InvalidPath)?;
        check_local_location(text).map_err(|r| match r {
            LocalRefusal::Invalid => StoreError::InvalidPath,
            LocalRefusal::Stream => StoreError::StreamLocation,
            LocalRefusal::Reserved => StoreError::ReservedLocation,
            LocalRefusal::Network => StoreError::NetworkLocation,
            LocalRefusal::Link => StoreError::LinkedLocation,
        })?;
    }
    validate_location(path)?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if path.parent().is_some_and(|p| same_dir(p, dir))
        && RESERVED_IN_CONFIG
            .iter()
            .any(|r| r.eq_ignore_ascii_case(name))
    {
        return Err(StoreError::ReservedLocation);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Status {
    /// File read fine.
    Ok,
    /// No file yet; the first save creates it.
    New,
    /// Custom file vanished while a cached copy exists; read-only.
    Missing,
    Cloud,
    Unreadable,
    Invalid,
    /// FER-467 R7: the configured custom location failed the safety checks; read-only.
    Unsafe,
}

impl Status {
    pub fn read_only(self) -> bool {
        !matches!(self, Self::Ok | Self::New)
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct View {
    pub doc: HotlistDoc,
    pub status: Status,
    pub read_only: bool,
    /// Custom file path (the user's own setting); `None` = default location.
    pub custom_location: Option<String>,
    /// FER-467 R7: why the custom location is refused (`location_network`, ...), if it is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location_problem: Option<&'static str>,
}

#[derive(Default)]
struct Inner {
    doc: Option<HotlistDoc>,
    stamp: Option<Stamp>,
    status: Option<Status>,
    cached_stamp: Option<Stamp>,
}

#[derive(Default)]
pub struct Store {
    inner: Mutex<Inner>,
}

impl Store {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn disk_cache(dir: &Path) -> Option<HotlistDoc> {
        HotlistDoc::parse(&fs::read(dir.join(CACHE_NAME)).ok()?)
    }

    fn view(inner: &Inner, loc: &Loc) -> View {
        let status = inner.status.unwrap_or(Status::New);
        View {
            doc: inner.doc.clone().unwrap_or_default(),
            status,
            read_only: status.read_only(),
            custom_location: loc.shown(),
            location_problem: match loc {
                Loc::Refused(_, e) => Some(e.kind()),
                _ => None,
            },
        }
    }

    /// Re-read the file and refresh the remembered doc/stamp/status.
    fn refresh(inner: &mut Inner, dir: &Path, loc: &Loc) {
        let Some(path) = loc.file(dir) else {
            inner.stamp = None;
            inner.doc = inner.doc.take().or_else(|| Self::disk_cache(dir));
            inner.status = Some(Status::Unsafe);
            return;
        };
        match read_file(&path) {
            Read::Ok(doc, stamp) => {
                if inner.cached_stamp.as_ref() != Some(&stamp) {
                    // Best effort: the cache is only a fallback.
                    let _ = write_atomic(
                        &dir.join(CACHE_NAME),
                        &serde_json::to_vec(&doc).unwrap_or_default(),
                    );
                    inner.cached_stamp = Some(stamp.clone());
                }
                inner.doc = Some(doc);
                inner.stamp = Some(stamp);
                inner.status = Some(Status::Ok);
            }
            Read::Absent => {
                let cache = inner.doc.clone().or_else(|| Self::disk_cache(dir));
                inner.stamp = None;
                if loc.is_custom() && cache.is_some() {
                    inner.doc = cache;
                    inner.status = Some(Status::Missing);
                } else {
                    inner.doc = Some(HotlistDoc::default());
                    inner.status = Some(Status::New);
                }
            }
            other => {
                inner.stamp = None;
                inner.doc = inner.doc.take().or_else(|| Self::disk_cache(dir));
                inner.status = Some(match other {
                    Read::Cloud => Status::Cloud,
                    Read::Invalid => Status::Invalid,
                    _ => Status::Unreadable,
                });
            }
        }
    }

    /// Load (or manually reload) the hotlist.
    pub fn load(&self, dir: &Path) -> View {
        let loc = read_location(dir);
        let mut inner = self.lock();
        Self::refresh(&mut inner, dir, &loc);
        Self::view(&inner, &loc)
    }

    /// Validate and write; refuses when read-only or when the file changed outside. The check and
    /// the replacement are one locked operation (`replace_checked`).
    pub fn save(&self, dir: &Path, doc: &HotlistDoc) -> Result<View, StoreError> {
        let bytes = serialize(doc)?;
        let loc = read_location(dir);
        let mut inner = self.lock();
        let Some(path) = loc.file(dir) else {
            Self::refresh(&mut inner, dir, &loc);
            return Err(StoreError::ReadOnly);
        };
        if inner.status == Some(Status::Missing) && !path.exists() {
            return Err(StoreError::ReadOnly);
        }
        let before = inner.stamp.clone();
        match replace_checked(&path, before.as_deref(), &bytes) {
            Ok(()) => {}
            Err(Replace::Storage) => return Err(StoreError::Storage),
            Err(_) => {
                Self::refresh(&mut inner, dir, &loc);
                let read_only = inner.status.is_some_and(Status::read_only);
                return Err(if read_only {
                    StoreError::ReadOnly
                } else {
                    StoreError::Conflict
                });
            }
        }
        let stamp = stamp_of(&bytes);
        let _ = write_atomic(&dir.join(CACHE_NAME), &bytes);
        inner.cached_stamp = Some(stamp.clone());
        inner.doc = Some(doc.clone());
        inner.stamp = Some(stamp);
        inner.status = Some(Status::Ok);
        Ok(Self::view(&inner, &loc))
    }

    /// What exists at a candidate location: `Missing`, `Valid` (a usable hotlist) or an error.
    pub fn check_location(location: Option<&Path>, dir: &Path) -> Result<Target, StoreError> {
        let path = match location {
            Some(p) => {
                validate_location_in(p, dir)?;
                p.to_path_buf()
            }
            None => dir.join(FILE_NAME),
        };
        match read_file(&path) {
            Read::Ok(..) => Ok(Target::Valid),
            Read::Absent => Ok(Target::Missing),
            _ => Err(StoreError::TargetUnusable),
        }
    }

    /// Change the hotlist file location. `Move` copies the current hotlist to a location with no
    /// file yet; `Use` adopts the valid file already there. Never overwrites an existing file.
    pub fn set_location(
        &self,
        dir: &Path,
        location: Option<&Path>,
        mode: Mode,
    ) -> Result<View, StoreError> {
        let target = Self::check_location(location, dir)?;
        let target_path = match location {
            Some(p) => p.to_path_buf(),
            None => dir.join(FILE_NAME),
        };
        let old = read_location(dir);
        let mut inner = self.lock();
        match (mode, target) {
            (Mode::Move, Target::Valid) => return Err(StoreError::TargetExists),
            (Mode::Use, Target::Missing) => return Err(StoreError::TargetMissing),
            (Mode::Move, Target::Missing) => {
                if target_path.parent().is_some_and(|p| !p.exists()) {
                    return Err(StoreError::ParentMissing);
                }
                if inner.doc.is_none() {
                    Self::refresh(&mut inner, dir, &old);
                }
                let doc = inner.doc.clone().unwrap_or_default();
                match replace_checked(&target_path, None, &serialize(&doc)?) {
                    Ok(()) => {}
                    Err(Replace::Changed) => return Err(StoreError::TargetExists),
                    Err(Replace::Unavailable) => return Err(StoreError::TargetUnusable),
                    Err(Replace::Storage) => return Err(StoreError::Storage),
                }
            }
            (Mode::Use, Target::Valid) => {}
        }
        write_location(dir, location)?;
        inner.doc = None;
        inner.stamp = None;
        inner.status = None;
        let loc = read_location(dir);
        Self::refresh(&mut inner, dir, &loc);
        Ok(Self::view(&inner, &loc))
    }

    /// The doc currently in memory (for command-item lookups).
    pub fn current_doc(&self, dir: &Path) -> HotlistDoc {
        let mut inner = self.lock();
        if inner.doc.is_none() {
            let loc = read_location(dir);
            Self::refresh(&mut inner, dir, &loc);
        }
        inner.doc.clone().unwrap_or_default()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Missing,
    Valid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Mode {
    Move,
    Use,
}

// ── approvals for `command` items ──

/// Everything an approval is bound to (FER-467 R4). Change any of it and the approval no longer
/// applies, so the user is asked again.
pub struct CommandIdentity<'a> {
    /// The hotlist file the item lives in (an approval in a local file does not carry over to a
    /// synced one).
    pub location: &'a str,
    /// Index path in the tree, e.g. `[0, 5, 3]`.
    pub position: &'a [usize],
    /// Titles of the submenus on the way, outermost first, and the item's own title: together
    /// they decide which accelerator keys reach the item.
    pub trail: &'a [&'a str],
    pub title: &'a str,
    pub command: &'a str,
    pub args: &'a [String],
    /// Working directory. Hotlist items have none today (ShellExecute gets no directory); the
    /// field is in the key so adding one later cannot silently reuse old approvals.
    pub cwd: Option<&'a str>,
}

/// SHA-256 over every identity field, each length-prefixed so no two different identities can
/// produce the same byte stream. The location is compared case-insensitively (Windows paths).
pub fn command_key(id: &CommandIdentity<'_>) -> String {
    fn field(h: &mut Sha256, bytes: &[u8]) {
        h.update((bytes.len() as u64).to_le_bytes());
        h.update(bytes);
    }
    let mut h = Sha256::new();
    field(&mut h, b"ferrum.hotlist.approval.v2");
    field(&mut h, id.location.to_lowercase().as_bytes());
    h.update((id.position.len() as u64).to_le_bytes());
    for p in id.position {
        h.update((*p as u64).to_le_bytes());
    }
    h.update((id.trail.len() as u64).to_le_bytes());
    for t in id.trail {
        field(&mut h, t.as_bytes());
    }
    field(&mut h, id.title.as_bytes());
    field(&mut h, id.command.as_bytes());
    h.update((id.args.len() as u64).to_le_bytes());
    for a in id.args {
        field(&mut h, a.as_bytes());
    }
    match id.cwd {
        Some(c) => field(&mut h, c.as_bytes()),
        None => h.update([0xff]),
    }
    hex::encode(h.finalize())
}

#[derive(Default, Serialize, Deserialize)]
struct Approvals {
    approved: Vec<String>,
}

pub fn is_approved(dir: &Path, key: &str) -> bool {
    fs::read(dir.join(APPROVALS_NAME))
        .ok()
        .and_then(|b| serde_json::from_slice::<Approvals>(&b).ok())
        .is_some_and(|a| a.approved.iter().any(|k| k == key))
}

pub fn approve(dir: &Path, key: &str) -> Result<(), StoreError> {
    let mut approvals = fs::read(dir.join(APPROVALS_NAME))
        .ok()
        .and_then(|b| serde_json::from_slice::<Approvals>(&b).ok())
        .unwrap_or_default();
    if !approvals.approved.iter().any(|k| k == key) {
        approvals.approved.push(key.to_string());
        if approvals.approved.len() > MAX_APPROVALS {
            approvals.approved.remove(0);
        }
    }
    let bytes = serde_json::to_vec(&approvals).map_err(|_| StoreError::Storage)?;
    write_atomic(&dir.join(APPROVALS_NAME), &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hotlist::model::Item;

    fn doc(title: &str) -> HotlistDoc {
        HotlistDoc {
            items: vec![Item::Dir {
                title: title.into(),
                path: "C:\\a".into(),
                target_path: None,
            }],
            ..Default::default()
        }
    }

    fn setup() -> (tempfile::TempDir, tempfile::TempDir) {
        (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap())
    }

    #[test]
    fn first_load_is_new_and_first_save_creates_file_and_cache() {
        let (cfg, _) = setup();
        let store = Store::new();
        let v = store.load(cfg.path());
        assert_eq!(v.status, Status::New);
        assert!(!v.read_only && v.doc.items.is_empty());
        let saved = store.save(cfg.path(), &doc("A")).unwrap();
        assert_eq!(saved.status, Status::Ok);
        assert!(cfg.path().join(FILE_NAME).exists());
        assert!(cfg.path().join(CACHE_NAME).exists());
        assert_eq!(Store::new().load(cfg.path()).doc, doc("A"));
    }

    #[test]
    fn second_save_keeps_a_bak_of_the_previous_version_and_no_temp_files() {
        let (cfg, _) = setup();
        let store = Store::new();
        store.load(cfg.path());
        store.save(cfg.path(), &doc("A")).unwrap();
        store.save(cfg.path(), &doc("B")).unwrap();
        let bak = fs::read(cfg.path().join("hotlist.json.bak")).unwrap();
        assert_eq!(HotlistDoc::parse(&bak), Some(doc("A")));
        assert!(
            !fs::read_dir(cfg.path()).unwrap().any(|e| e
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp"))
        );
    }

    #[test]
    fn external_change_blocks_the_write_and_reloads_instead_of_overwriting() {
        let (cfg, _) = setup();
        let store = Store::new();
        store.load(cfg.path());
        store.save(cfg.path(), &doc("A")).unwrap();
        // Another PC (sync) rewrites the file.
        fs::write(
            cfg.path().join(FILE_NAME),
            serde_json::to_vec(&doc("FROM-OTHER-PC")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            store.save(cfg.path(), &doc("MINE")).unwrap_err(),
            StoreError::Conflict
        );
        let on_disk = HotlistDoc::parse(&fs::read(cfg.path().join(FILE_NAME)).unwrap()).unwrap();
        assert_eq!(on_disk, doc("FROM-OTHER-PC"));
        // After the reload the caller's next save goes through.
        assert!(store.save(cfg.path(), &doc("MINE")).is_ok());
    }

    #[test]
    fn a_file_appearing_or_vanishing_behind_our_back_is_a_conflict() {
        let (cfg, _) = setup();
        let store = Store::new();
        store.load(cfg.path()); // New, stamp None
        fs::write(
            cfg.path().join(FILE_NAME),
            serde_json::to_vec(&doc("EXISTING")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            store.save(cfg.path(), &doc("X")).unwrap_err(),
            StoreError::Conflict
        );
        fs::remove_file(cfg.path().join(FILE_NAME)).unwrap();
        assert_eq!(
            store.save(cfg.path(), &doc("X")).unwrap_err(),
            StoreError::Conflict
        );
    }

    #[test]
    fn corrupt_file_is_read_only_shows_the_cache_and_is_never_overwritten() {
        let (cfg, _) = setup();
        let store = Store::new();
        store.load(cfg.path());
        store.save(cfg.path(), &doc("GOOD")).unwrap();
        fs::write(cfg.path().join(FILE_NAME), b"").unwrap();
        let fresh = Store::new(); // new process: only the disk cache is left
        let v = fresh.load(cfg.path());
        assert_eq!(v.status, Status::Invalid);
        assert!(v.read_only);
        assert_eq!(v.doc, doc("GOOD"));
        assert_eq!(
            fresh.save(cfg.path(), &doc("NEW")).unwrap_err(),
            StoreError::ReadOnly
        );
        assert_eq!(fs::read(cfg.path().join(FILE_NAME)).unwrap(), b"");
    }

    #[test]
    fn invalid_documents_are_rejected_before_touching_disk() {
        let (cfg, _) = setup();
        let store = Store::new();
        store.load(cfg.path());
        let mut bad = doc("A");
        bad.version = 1;
        assert_eq!(
            store.save(cfg.path(), &bad).unwrap_err(),
            StoreError::InvalidDoc
        );
        assert!(!cfg.path().join(FILE_NAME).exists());
    }

    #[test]
    fn location_validation_rejects_unc_device_relative_dotdot_and_non_json() {
        for bad in [
            "\\\\server\\share\\h.json",
            "\\\\?\\C:\\h.json",
            "\\\\.\\C:\\h.json",
            "relative\\h.json",
            "C:\\a\\..\\h.json",
            "C:\\a\\h.txt",
            "C:\\a\\",
            "",
            "C:\\a\0\\h.json",
        ] {
            assert_eq!(
                validate_location(Path::new(bad)),
                Err(StoreError::InvalidPath),
                "{bad:?}"
            );
        }
        #[cfg(windows)]
        assert!(validate_location(Path::new("D:\\Drive\\hotlist.json")).is_ok());
    }

    #[cfg(windows)]
    #[test]
    fn move_copies_into_an_empty_location_and_refuses_an_existing_file() {
        let (cfg, other) = setup();
        let store = Store::new();
        store.load(cfg.path());
        store.save(cfg.path(), &doc("A")).unwrap();
        let target = other.path().join("hotlist.json");
        let v = store
            .set_location(cfg.path(), Some(&target), Mode::Move)
            .unwrap();
        assert_eq!(v.doc, doc("A"));
        assert!(v.custom_location.is_some());
        assert!(target.exists());
        // Saves now go to the new file.
        store.save(cfg.path(), &doc("B")).unwrap();
        assert_eq!(
            HotlistDoc::parse(&fs::read(&target).unwrap()),
            Some(doc("B"))
        );
        // Moving onto a file that exists is refused and leaves it untouched.
        let again = tempfile::tempdir().unwrap();
        let busy = again.path().join("h.json");
        fs::write(&busy, serde_json::to_vec(&doc("THEIRS")).unwrap()).unwrap();
        assert_eq!(
            store
                .set_location(cfg.path(), Some(&busy), Mode::Move)
                .unwrap_err(),
            StoreError::TargetExists
        );
        assert_eq!(
            HotlistDoc::parse(&fs::read(&busy).unwrap()),
            Some(doc("THEIRS"))
        );
        // "Use existing" adopts it.
        let v = store
            .set_location(cfg.path(), Some(&busy), Mode::Use)
            .unwrap();
        assert_eq!(v.doc, doc("THEIRS"));
    }

    #[cfg(windows)]
    #[test]
    fn use_requires_a_valid_existing_file_and_move_needs_an_existing_parent() {
        let (cfg, other) = setup();
        let store = Store::new();
        store.load(cfg.path());
        let missing = other.path().join("none.json");
        assert_eq!(
            store
                .set_location(cfg.path(), Some(&missing), Mode::Use)
                .unwrap_err(),
            StoreError::TargetMissing
        );
        let no_parent = other.path().join("nodir").join("h.json");
        assert_eq!(
            Store::check_location(Some(&no_parent), cfg.path()),
            Err(StoreError::TargetUnusable)
        );
        assert_eq!(
            store
                .set_location(cfg.path(), Some(&no_parent), Mode::Move)
                .unwrap_err(),
            StoreError::TargetUnusable
        );
        let junk = other.path().join("junk.json");
        fs::write(&junk, b"not a hotlist").unwrap();
        assert_eq!(
            store
                .set_location(cfg.path(), Some(&junk), Mode::Use)
                .unwrap_err(),
            StoreError::TargetUnusable
        );
        assert_eq!(fs::read(&junk).unwrap(), b"not a hotlist");
    }

    #[cfg(windows)]
    #[test]
    fn a_vanished_custom_file_is_read_only_with_the_cached_copy() {
        let (cfg, other) = setup();
        let store = Store::new();
        store.load(cfg.path());
        store.save(cfg.path(), &doc("A")).unwrap();
        let target = other.path().join("h.json");
        store
            .set_location(cfg.path(), Some(&target), Mode::Move)
            .unwrap();
        fs::remove_file(&target).unwrap();
        let v = store.load(cfg.path());
        assert_eq!(v.status, Status::Missing);
        assert!(v.read_only);
        assert_eq!(v.doc, doc("A"));
        assert_eq!(
            store.save(cfg.path(), &doc("B")).unwrap_err(),
            StoreError::ReadOnly
        );
        assert!(!target.exists());
    }

    #[cfg(windows)]
    #[test]
    fn cloud_placeholders_are_not_read_so_they_are_never_hydrated() {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_OFFLINE, SetFileAttributesW};
        let (cfg, _) = setup();
        let store = Store::new();
        store.load(cfg.path());
        store.save(cfg.path(), &doc("GOOD")).unwrap();
        let file = cfg.path().join(FILE_NAME);
        let wide: Vec<u16> = file.as_os_str().encode_wide().chain(Some(0)).collect();
        assert_ne!(
            unsafe { SetFileAttributesW(wide.as_ptr(), FILE_ATTRIBUTE_OFFLINE) },
            0
        );
        assert_eq!(read_file(&file), Read::Cloud);
        let v = Store::new().load(cfg.path());
        assert_eq!(v.status, Status::Cloud);
        assert!(v.read_only);
        assert_eq!(v.doc, doc("GOOD")); // from the cache, not from the placeholder
        assert_eq!(
            Store::new().save(cfg.path(), &doc("X")).unwrap_err(),
            StoreError::ReadOnly
        );
    }

    fn ident<'a>(
        position: &'a [usize],
        trail: &'a [&'a str],
        title: &'a str,
        args: &'a [String],
    ) -> CommandIdentity<'a> {
        CommandIdentity {
            location: "C:\\cfg\\hotlist.json",
            position,
            trail,
            title,
            command: "C:\\k.exe",
            args,
            cwd: None,
        }
    }

    #[test]
    fn command_keys_bind_location_position_titles_program_args_and_cwd() {
        let args = vec!["-a".to_string()];
        let base = command_key(&ident(&[0, 1], &["&Tools"], "&k", &args));
        // FER-467 R4: every part of the identity matters.
        let other_args = vec!["-a".to_string(), "-b".to_string()];
        for changed in [
            command_key(&ident(&[0, 1], &["&Tools"], "&k", &other_args)),
            command_key(&ident(&[0, 2], &["&Tools"], "&k", &args)), // moved down
            command_key(&ident(&[1], &[], "&k", &args)),            // moved to the top level
            command_key(&ident(&[0, 1], &["&1 Tools"], "&k", &args)), // parent accelerator
            command_key(&ident(&[0, 1], &["&Tools"], "&1 k", &args)), // own accelerator
            command_key(&CommandIdentity {
                location: "D:\\sync\\hotlist.json",
                ..ident(&[0, 1], &["&Tools"], "&k", &args)
            }),
            command_key(&CommandIdentity {
                command: "C:\\k2.exe",
                ..ident(&[0, 1], &["&Tools"], "&k", &args)
            }),
            command_key(&CommandIdentity {
                cwd: Some("C:\\"),
                ..ident(&[0, 1], &["&Tools"], "&k", &args)
            }),
        ] {
            assert_ne!(base, changed);
        }
        let joined = vec!["-a-b".to_string()];
        let split = vec!["-a".to_string(), "-b".to_string()];
        assert_ne!(
            command_key(&ident(&[0], &[], "t", &joined)),
            command_key(&ident(&[0], &[], "t", &split))
        );
        // Location spelling case does not matter on Windows.
        assert_eq!(
            base,
            command_key(&CommandIdentity {
                location: "c:\\CFG\\Hotlist.json",
                ..ident(&[0, 1], &["&Tools"], "&k", &args)
            })
        );
        let cfg = tempfile::tempdir().unwrap();
        assert!(!is_approved(cfg.path(), &base));
        approve(cfg.path(), &base).unwrap();
        approve(cfg.path(), &base).unwrap();
        assert!(is_approved(cfg.path(), &base));
        let moved = command_key(&ident(&[0, 2], &["&Tools"], "&k", &args));
        assert!(!is_approved(cfg.path(), &moved));
    }

    // ── FER-467 R6 ──

    #[test]
    fn a_changed_file_is_never_replaced_and_no_temp_file_is_left() {
        let (cfg, _) = setup();
        let path = cfg.path().join(FILE_NAME);
        fs::write(&path, b"theirs").unwrap();
        let wrong = stamp_of(b"mine-was-different");
        assert_eq!(
            replace_checked(&path, Some(&wrong), b"new"),
            Err(Replace::Changed)
        );
        assert_eq!(replace_checked(&path, None, b"new"), Err(Replace::Changed));
        assert_eq!(fs::read(&path).unwrap(), b"theirs");
        assert!(!cfg.path().join("hotlist.json.bak").exists());
        let right = stamp_of(b"theirs");
        replace_checked(&path, Some(&right), b"new").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new");
        assert_eq!(
            fs::read(cfg.path().join("hotlist.json.bak")).unwrap(),
            b"theirs"
        );
        // A file that vanished is a change too.
        assert_eq!(
            replace_checked(&cfg.path().join("gone.json"), Some(&right), b"x"),
            Err(Replace::Changed)
        );
        let names: Vec<String> = fs::read_dir(cfg.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(names.iter().all(|n| !n.ends_with(".tmp")), "{names:?}");
    }

    #[cfg(windows)]
    #[test]
    fn while_the_hash_is_checked_nobody_else_can_write_the_file() {
        let (cfg, _) = setup();
        let path = cfg.path().join(FILE_NAME);
        fs::write(&path, b"v1").unwrap();
        let held = locked::open(&path).unwrap();
        // Another writer (a sync client) is shut out while we hold the checked handle...
        assert!(fs::OpenOptions::new().write(true).open(&path).is_err());
        assert!(fs::remove_file(&path).is_err());
        drop(held);
        // ...and a file someone else holds open for writing is not replaced underneath them.
        let writer = fs::OpenOptions::new().write(true).open(&path).unwrap();
        assert_eq!(
            replace_checked(&path, Some(&stamp_of(b"v1")), b"v2"),
            Err(Replace::Changed)
        );
        drop(writer);
        assert_eq!(fs::read(&path).unwrap(), b"v1");
        replace_checked(&path, Some(&stamp_of(b"v1")), b"v2").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"v2");
    }

    #[test]
    fn a_corrupted_file_makes_the_save_read_only_and_is_left_alone() {
        let (cfg, _) = setup();
        let store = Store::new();
        store.load(cfg.path());
        store.save(cfg.path(), &doc("A")).unwrap();
        fs::write(cfg.path().join(FILE_NAME), b"{garbage").unwrap();
        assert_eq!(
            store.save(cfg.path(), &doc("B")).unwrap_err(),
            StoreError::ReadOnly
        );
        assert_eq!(fs::read(cfg.path().join(FILE_NAME)).unwrap(), b"{garbage");
    }

    // ── FER-467 R7 ──

    #[cfg(windows)]
    fn junction(link: &Path, target: &Path) -> bool {
        std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .is_ok_and(|o| o.status.success())
    }

    #[cfg(windows)]
    #[test]
    fn a_location_behind_a_junction_is_refused() {
        let (cfg, other) = setup();
        let real = other.path().join("real");
        fs::create_dir(&real).unwrap();
        let link = other.path().join("via-junction");
        assert!(junction(&link, &real), "junction created");
        let store = Store::new();
        store.load(cfg.path());
        for candidate in [
            link.join("hotlist.json"),
            link.join("deeper").join("h.json"),
        ] {
            assert_eq!(
                Store::check_location(Some(&candidate), cfg.path()),
                Err(StoreError::LinkedLocation),
                "{candidate:?}"
            );
            assert_eq!(
                store
                    .set_location(cfg.path(), Some(&candidate), Mode::Move)
                    .unwrap_err(),
                StoreError::LinkedLocation
            );
        }
        assert!(!real.join("hotlist.json").exists());
        // The real folder itself is fine.
        assert!(Store::check_location(Some(&real.join("hotlist.json")), cfg.path()).is_ok());
    }

    #[cfg(windows)]
    #[test]
    fn a_location_that_becomes_a_junction_later_is_read_only_and_never_written() {
        let (cfg, other) = setup();
        let store = Store::new();
        store.load(cfg.path());
        store.save(cfg.path(), &doc("A")).unwrap();
        let folder = other.path().join("sync");
        fs::create_dir(&folder).unwrap();
        let target = folder.join("hotlist.json");
        store
            .set_location(cfg.path(), Some(&target), Mode::Move)
            .unwrap();
        // The folder is swapped for a junction to somewhere else.
        let elsewhere = other.path().join("elsewhere");
        fs::create_dir(&elsewhere).unwrap();
        fs::remove_file(&target).unwrap();
        fs::remove_dir(&folder).unwrap();
        assert!(junction(&folder, &elsewhere));
        let v = Store::new().load(cfg.path());
        assert_eq!(v.status, Status::Unsafe);
        assert!(v.read_only);
        assert_eq!(v.location_problem, Some("location_link"));
        assert_eq!(v.doc, doc("A")); // the cached copy
        assert_eq!(
            store.save(cfg.path(), &doc("B")).unwrap_err(),
            StoreError::ReadOnly
        );
        assert!(!elsewhere.join("hotlist.json").exists());
    }

    #[cfg(windows)]
    #[test]
    fn reserved_names_streams_unc_and_ferrums_own_files_are_refused() {
        let (cfg, other) = setup();
        let o = other.path().to_string_lossy().into_owned();
        for (bad, kind) in [
            (format!("{o}\\CON.json"), StoreError::ReservedLocation),
            (format!("{o}\\nul.json"), StoreError::ReservedLocation),
            (format!("{o}\\com1\\h.json"), StoreError::ReservedLocation),
            (format!("{o}\\Lpt9 .json"), StoreError::ReservedLocation),
            (format!("{o}\\conin$.json"), StoreError::ReservedLocation),
            (format!("{o}\\dir.\\h.json"), StoreError::ReservedLocation),
            (format!("{o}\\h.json:alt.json"), StoreError::StreamLocation),
            (format!("{o}\\h.json::$DATA"), StoreError::StreamLocation),
            (
                "\\\\server\\share\\h.json".into(),
                StoreError::NetworkLocation,
            ),
            ("//server/share/h.json".into(), StoreError::NetworkLocation),
            ("\\\\?\\C:\\h.json".into(), StoreError::InvalidPath),
        ] {
            assert_eq!(
                Store::check_location(Some(Path::new(&bad)), cfg.path()),
                Err(kind),
                "{bad}"
            );
        }
        for own in [
            CACHE_NAME,
            LOCATION_NAME,
            APPROVALS_NAME,
            "HOTLIST.Cache.JSON",
        ] {
            assert_eq!(
                validate_location_in(&cfg.path().join(own), cfg.path()),
                Err(StoreError::ReservedLocation),
                "{own}"
            );
        }
        // Normal names that merely contain a device word are fine.
        for ok in ["console.json", "nul-list.json", "com10.json", "my con.json"] {
            assert!(
                validate_location_in(&other.path().join(ok), cfg.path()).is_ok(),
                "{ok}"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn a_mapped_network_drive_is_refused() {
        use crate::path_guard::{DRIVE_REMOTE, drive_type_is_local};
        use windows_sys::Win32::Storage::FileSystem::GetDriveTypeW;
        assert!(!drive_type_is_local(DRIVE_REMOTE));
        for local in [2u32, 3, 5, 6] {
            assert!(drive_type_is_local(local)); // removable, fixed, cd-rom, ram disk
        }
        // Every mapped drive this machine has must be refused by the real check.
        let cfg = tempfile::tempdir().unwrap();
        for letter in b'A'..=b'Z' {
            let root: Vec<u16> = format!("{}:\\", letter as char)
                .encode_utf16()
                .chain(Some(0))
                .collect();
            // SAFETY: NUL-terminated root path.
            if unsafe { GetDriveTypeW(root.as_ptr()) } == DRIVE_REMOTE {
                let p = format!("{}:\\ferrum-test\\hotlist.json", letter as char);
                assert_eq!(
                    validate_location_in(Path::new(&p), cfg.path()),
                    Err(StoreError::NetworkLocation)
                );
            }
        }
    }
}
