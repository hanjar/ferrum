//! ZIP create (F2-07) and integrity test (F2-08) on the `zip` crate, plus
//! RAR integrity test via the `unrar` crate's per-member test mode.
//!
//! Creation: sources validated (no traversal/symlink — D07), files collected
//! recursively, archive written to a `.ferrum-tmp` sibling then renamed — a
//! failure never leaves a partial archive at the final path.
//! Testing: every member is decoded to EOF (the zip crate verifies CRC
//! during decode; unrar's per-member `test()` validates decode + CRC).

use crate::archive_contract::{
    ArchiveConflictPolicy, ArchiveError, ArchiveErrorCode, ArchiveLimits, ArchiveProgress,
    ArchiveTestEntry, ArchiveTestReport, ArchiveTestStatus, MemberPath,
};

use std::fs::{File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

// unrar_sys's Registry references need advapi32 at link time (see F2-06).
#[cfg(windows)]
#[link(name = "advapi32")]
unsafe extern "C" {}

fn err(code: ArchiveErrorCode, msg: impl Into<String>) -> ArchiveError {
    ArchiveError::new(code, msg)
}

fn io_err(e: std::io::Error, path: &Path) -> ArchiveError {
    let mut e = ArchiveError::new(ArchiveErrorCode::Io, e.to_string());
    e.archive_path = Some(path.to_string_lossy().into_owned());
    e
}

// ── Destination conflict policy (FER-411) ─────────────────────────────────
//
// `rename` over an existing path REPLACES it on Windows and Unix, so the policy has to be applied
// before publishing. A name is claimed with an exclusive create (`create_new`), which is atomic on
// every filesystem and cannot be raced into an overwrite; the finished archive is then renamed over
// our own empty placeholder only.
//
// - `Overwrite`: replace an existing destination (the manual Pack dialog's explicit choice).
// - `Skip`: never replace; an existing destination (file or directory) fails with `Conflict`.
// - `Rename`: take the first free `name (n).ext`; returns the path actually written.
// - `OverwriteOlder`: has no meaning when creating an archive; rejected instead of guessed.

const NOT_SUPPORTED: &str = "overwriteOlder is not supported when creating an archive";
const EXISTS: &str = "The destination already exists; nothing was overwritten";

fn conflict_error(dest: &Path, message: &str) -> ArchiveError {
    let mut e = ArchiveError::new(ArchiveErrorCode::Conflict, message);
    e.archive_path = Some(dest.to_string_lossy().into_owned());
    e
}

/// Fail fast, before the (possibly long) archive is written.
pub(crate) fn precheck_conflict(
    dest: &Path,
    policy: ArchiveConflictPolicy,
) -> Result<(), ArchiveError> {
    match policy {
        ArchiveConflictPolicy::OverwriteOlder => Err(conflict_error(dest, NOT_SUPPORTED)),
        ArchiveConflictPolicy::Skip if dest.symlink_metadata().is_ok() => {
            Err(conflict_error(dest, EXISTS))
        }
        _ => Ok(()),
    }
}

/// Sibling temp file claimed exclusively: never truncates a user's file, never collides with a
/// concurrent create of the same destination.
fn exclusive_tmp(dest: &Path) -> Result<PathBuf, ArchiveError> {
    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "archive".to_owned());
    for attempt in 0..100u32 {
        let candidate = dest.with_file_name(format!(
            ".{name}.{}-{attempt}.ferrum-tmp",
            std::process::id()
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(_) => return Ok(candidate),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(io_err(e, &candidate)),
        }
    }
    Err(err(
        ArchiveErrorCode::Io,
        "Could not allocate a temporary file",
    ))
}

/// Claim `path` with an exclusive create. `Ok(false)` = something is already there.
fn claim(path: &Path) -> Result<bool, ArchiveError> {
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        // A directory in the way reports "access denied" on Windows.
        Err(_) if path.symlink_metadata().is_ok() => Ok(false),
        Err(e) => Err(io_err(e, path)),
    }
}

fn numbered(dest: &Path, n: u32) -> PathBuf {
    let stem = dest
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = match dest.extension() {
        Some(ext) => format!("{stem} ({n}).{}", ext.to_string_lossy()),
        None => format!("{stem} ({n})"),
    };
    dest.with_file_name(name)
}

/// Publish the finished `tmp` as the archive under `policy`; returns the path actually written.
pub(crate) fn publish_created(
    tmp: &Path,
    dest: &Path,
    policy: ArchiveConflictPolicy,
) -> Result<PathBuf, ArchiveError> {
    let (target, claimed) = match policy {
        ArchiveConflictPolicy::Overwrite => (dest.to_path_buf(), false),
        ArchiveConflictPolicy::OverwriteOlder => {
            return Err(conflict_error(dest, NOT_SUPPORTED));
        }
        ArchiveConflictPolicy::Skip => {
            if !claim(dest)? {
                return Err(conflict_error(dest, EXISTS));
            }
            (dest.to_path_buf(), true)
        }
        ArchiveConflictPolicy::Rename => {
            let mut chosen = None;
            for n in 1..=9999u32 {
                let candidate = if n == 1 {
                    dest.to_path_buf()
                } else {
                    numbered(dest, n)
                };
                if claim(&candidate)? {
                    chosen = Some(candidate);
                    break;
                }
            }
            match chosen {
                Some(path) => (path, true),
                None => return Err(conflict_error(dest, "No free name for the archive")),
            }
        }
    };
    match std::fs::rename(tmp, &target) {
        Ok(()) => Ok(target),
        Err(e) => {
            // Only ever remove what THIS call claimed; an overwritten destination is not ours to delete.
            if claimed {
                let _ = std::fs::remove_file(&target);
            }
            Err(io_err(e, &target))
        }
    }
}

// ── Edge cases while packing (FER-205) ────────────────────────────────────
//
// The message prefixes below are part of the engine's contract: the chat adapter maps them to a
// user-facing cause (`src/lib/ai/adapters/archive.ts`). Change both sides together.

/// A source file could not be opened or read (locked, access denied, vanished mid-run).
pub const SOURCE_UNREADABLE: &str = "Source could not be read";
/// The destination drive ran out of space while the archive was written.
pub const NO_SPACE: &str = "Not enough disk space on the destination drive";

/// `ERROR_HANDLE_DISK_FULL` (39) and `ERROR_DISK_FULL` (112) on Windows; `ENOSPC` elsewhere.
fn is_out_of_space(e: &std::io::Error) -> bool {
    if e.kind() == std::io::ErrorKind::StorageFull {
        return true;
    }
    matches!(e.raw_os_error(), Some(39) | Some(112)) && cfg!(windows)
}

fn read_err(e: std::io::Error, path: &Path) -> ArchiveError {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut out = ArchiveError::new(
        ArchiveErrorCode::Io,
        format!("{SOURCE_UNREADABLE}: {name}: {e}"),
    );
    out.archive_path = Some(path.to_string_lossy().into_owned());
    out
}

fn write_err(e: std::io::Error, what: &str) -> ArchiveError {
    if is_out_of_space(&e) {
        return err(
            ArchiveErrorCode::Io,
            format!("{NO_SPACE}; nothing was written"),
        );
    }
    err(ArchiveErrorCode::Io, format!("{what}: {e}"))
}

fn zip_err(e: zip::result::ZipError, what: &str) -> ArchiveError {
    match e {
        zip::result::ZipError::Io(io) => write_err(io, what),
        other => err(ArchiveErrorCode::Io, format!("{what}: {other}")),
    }
}

/// `.<name>.<pid>-<n>.ferrum-tmp` / `.<name>.<pid>-<n>.ferrum-tmp.rar`: an archive Ferrum is writing right now.
fn is_ferrum_staging(name: &str) -> bool {
    parse_staging_pid(name).is_some()
}

/// A leftover only qualifies once it is at least this old: a just-created staging entry of a
/// concurrent create (whose PID could be reused or unknown) is never swept. (FER-510)
pub const MIN_STAGING_AGE: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// FER-510: parses a strict Ferrum staging name `.<anything>.<pid>-<n>.ferrum-tmp` (also the
/// `.ferrum-tmp.rar` variant) and returns its `<pid>`. Every part is checked: leading dot,
/// exact suffix, digits-only `<pid>-<n>` tag behind the last dot. Lookalikes
/// (`out.ferrum-tmp`, `.out.zip.ferrum-tmp`, `.out.zip.12-x.ferrum-tmp`) parse to `None`.
fn parse_staging_pid(name: &str) -> Option<u32> {
    let inner = name.strip_prefix('.').and_then(|n| {
        n.strip_suffix(".ferrum-tmp")
            .or_else(|| n.strip_suffix(".ferrum-tmp.rar"))
    })?;
    let (_, tag) = inner.rsplit_once('.')?;
    let (pid, n) = tag.split_once('-')?;
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(pid) || !digits(n) {
        return None;
    }
    pid.parse().ok()
}

/// FER-510: fail-closed PID liveness (Windows): opening or querying the process counts as
/// alive on any error, so nothing is deleted on doubt. A PID reused by another process also
/// reports alive.
#[cfg(windows)]
fn pid_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    // SAFETY: OpenProcess with query-only access on a numeric PID; the handle is closed on
    // every path below.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        // Refuse to delete on anything but the definite "invalid parameter" = the PID does
        // not exist.
        return std::io::Error::last_os_error().raw_os_error() != Some(87); // ERROR_INVALID_PARAMETER
    }
    let mut code: u32 = 0;
    // SAFETY: `handle` is a valid process handle and `code` a valid u32 out-slot.
    let ok = unsafe { GetExitCodeProcess(handle, &mut code) };
    // SAFETY: `handle` was returned by OpenProcess above.
    unsafe { CloseHandle(handle) };
    if ok == 0 {
        return true; // cannot tell → treat as alive
    }
    code == STILL_ACTIVE as u32
}

/// FER-510: fail-closed — without the Windows API there is no safe answer, so never delete.
#[cfg(not(windows))]
fn pid_alive(_pid: u32) -> bool {
    true
}

/// FER-510: sweep Ferrum staging leftovers (dead PID and older than [`MIN_STAGING_AGE`]) from
/// the folder that would hold `dest`, before a create starts there. A crash can leave the
/// ZIP/7z staging file `.<dest>.<pid>-<n>.ferrum-tmp` or the RAR job folder
/// `.<dest>.<pid>-<rand>.ferrum-tmp` behind. Best effort: returns how many entries were
/// removed; any unreadable or ambiguous entry is skipped untouched.
///
/// Safety rails: only exact Ferrum staging names; only the destination folder of THIS job;
/// symlink/junction entries are neither followed nor removed (the no-follow stance of
/// `archive_publish`); a future mtime, unreadable metadata or a live/unknown PID all mean
/// "do not delete".
pub fn clean_stale_staging(dest: &Path) -> usize {
    let Some(parent) = dest.parent().filter(|p| !p.as_os_str().is_empty()) else {
        return 0;
    };
    let Ok(children) = std::fs::read_dir(parent) else {
        return 0;
    };
    let mut removed = 0;
    for child in children.flatten() {
        let Some(pid) = parse_staging_pid(&child.file_name().to_string_lossy()) else {
            continue;
        };
        // No-follow: DirEntry::file_type/metadata report the link itself, never its target.
        let Ok(kind) = child.file_type() else {
            continue;
        };
        if kind.is_symlink() {
            continue; // a reparse point is never followed and never removed
        }
        let Ok(meta) = child.metadata() else {
            continue;
        };
        let Ok(modified) = meta.modified() else {
            continue;
        };
        let Ok(age) = modified.elapsed() else {
            continue; // mtime in the future: never delete
        };
        if age < MIN_STAGING_AGE {
            continue;
        }
        if pid_alive(pid) {
            continue;
        }
        let path = child.path();
        let done = if kind.is_dir() {
            // std's remove_dir_all does not follow symlinks/junctions (the guarantee JobDir's
            // Drop relies on); the entry itself was verified to be no reparse point above.
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        if done.is_ok() {
            removed += 1;
        }
    }
    removed
}

fn same_name(a: &std::ffi::OsStr, b: &std::ffi::OsStr) -> bool {
    if cfg!(windows) {
        a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
    } else {
        a == b
    }
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// True when `path` is the archive being created (`dest`) itself.
fn is_destination(path: &Path, dest: &Path) -> bool {
    match (path.file_name(), dest.file_name()) {
        (Some(p), Some(d)) if same_name(p, d) => match (path.parent(), dest.parent()) {
            (Some(pp), Some(dp)) => same_dir(pp, dp),
            _ => false,
        },
        _ => false,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceEntry {
    pub disk_path: PathBuf,
    pub archive_name: String,
    pub is_dir: bool,
}

/// FER-510: two sources from different base folders can produce the same relative member name
/// (selecting `docs\a.txt` together with `other\a.txt`). Rar.exe only fails on such listfiles
/// AFTER running (cryptic "exit code 10"), and ZIP/7z would silently write duplicate members
/// that extraction later refuses as conflicts — so every create refuses them up front,
/// case-insensitively like the Windows filesystem, before any process starts or a byte is
/// written.
fn check_duplicate_members(entries: &[SourceEntry]) -> Result<(), ArchiveError> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for e in entries {
        if !seen.insert(e.archive_name.to_lowercase()) {
            let mut err = ArchiveError::new(
                ArchiveErrorCode::Conflict,
                format!(
                    "Duplicate archive member name '{}' from different source folders; pack from one folder or separately",
                    e.archive_name
                ),
            );
            err.member_path = Some(e.archive_name.clone());
            return Err(err);
        }
    }
    Ok(())
}

/// Collect every source and drop what must never become input: the destination itself (a file
/// that `Overwrite` is about to replace, possibly inside a selected folder) and the staging files of
/// archives being written. The output can therefore never contain itself. Under `Rename` an existing
/// file with the requested name is not the output (a free numbered name is), so it stays input.
pub(crate) fn collect_sources(
    sources: &[String],
    destination_path: &Path,
    policy: ArchiveConflictPolicy,
    limits: &ArchiveLimits,
) -> Result<Vec<SourceEntry>, ArchiveError> {
    let output =
        |p: &Path| policy != ArchiveConflictPolicy::Rename && is_destination(p, destination_path);
    let mut entries: Vec<SourceEntry> = Vec::new();
    for source in sources {
        let root = validate_source(source)?;
        if output(&root) {
            return Err(err(
                ArchiveErrorCode::InvalidMemberPath,
                "The archive cannot contain itself: the destination is one of the sources",
            ));
        }
        let base_name = root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .ok_or_else(|| {
                err(
                    ArchiveErrorCode::InvalidMemberPath,
                    "Source has no file name",
                )
            })?;
        let meta = std::fs::symlink_metadata(&root).map_err(|e| io_err(e, &root))?;
        if meta.file_type().is_symlink() {
            continue; // D07
        }
        if meta.is_dir() {
            let start_idx = entries.len();
            let has_files = collect_dir_children(&root, &base_name, limits, &output, &mut entries)?;
            if !has_files {
                entries.insert(
                    start_idx,
                    SourceEntry {
                        disk_path: root,
                        archive_name: base_name,
                        is_dir: true,
                    },
                );
            }
        } else {
            if base_name.split('/').count() > limits.max_depth as usize {
                return Err(err(
                    ArchiveErrorCode::LimitExceeded,
                    "Archive depth limit exceeded",
                ));
            }
            entries.push(SourceEntry {
                disk_path: root,
                archive_name: base_name,
                is_dir: false,
            });
        }
    }
    entries.retain(|e| {
        let staging = e
            .disk_path
            .file_name()
            .is_some_and(|n| is_ferrum_staging(&n.to_string_lossy()));
        !staging && !output(&e.disk_path)
    });
    if entries.len() > limits.max_entries as usize {
        return Err(err(ArchiveErrorCode::LimitExceeded, "Too many files"));
    }
    check_duplicate_members(&entries)?;
    Ok(entries)
}

fn collect_dir_children(
    dir: &Path,
    dir_archive_name: &str,
    limits: &ArchiveLimits,
    output: &dyn Fn(&Path) -> bool,
    entries: &mut Vec<SourceEntry>,
) -> Result<bool, ArchiveError> {
    if dir_archive_name.split('/').count() > limits.max_depth as usize {
        return Err(err(
            ArchiveErrorCode::LimitExceeded,
            "Archive depth limit exceeded",
        ));
    }
    let children = std::fs::read_dir(dir).map_err(|e| io_err(e, dir))?;
    let mut children: Vec<_> = children.filter_map(|e| e.ok()).collect();
    children.sort_by_key(|e| e.file_name());

    let mut dir_has_files = false;
    for child in children {
        let child_path = child.path();
        let child_meta =
            std::fs::symlink_metadata(&child_path).map_err(|e| io_err(e, &child_path))?;
        if child_meta.file_type().is_symlink() {
            continue; // D07
        }
        let child_name_str = child.file_name().to_string_lossy().into_owned();
        if is_ferrum_staging(&child_name_str) || output(&child_path) {
            continue;
        }
        let child_name = format!("{dir_archive_name}/{child_name_str}");
        if child_name.split('/').count() > limits.max_depth as usize {
            return Err(err(
                ArchiveErrorCode::LimitExceeded,
                "Archive depth limit exceeded",
            ));
        }
        if child_meta.is_dir() {
            let start_idx = entries.len();
            let child_has_files =
                collect_dir_children(&child_path, &child_name, limits, output, entries)?;
            if child_has_files {
                dir_has_files = true;
            } else {
                entries.insert(
                    start_idx,
                    SourceEntry {
                        disk_path: child_path,
                        archive_name: child_name,
                        is_dir: true,
                    },
                );
            }
        } else {
            dir_has_files = true;
            entries.push(SourceEntry {
                disk_path: child_path,
                archive_name: child_name,
                is_dir: false,
            });
        }
    }
    Ok(dir_has_files)
}

/// Validate a source path for packing: exists, no `.`/`..` traversal, not a
/// symlink (D07: we never dereference links into archives).
pub(crate) fn validate_source(source: &str) -> Result<PathBuf, ArchiveError> {
    if source.is_empty() || source.contains('\0') {
        return Err(err(
            ArchiveErrorCode::InvalidMemberPath,
            "Empty or NUL source path",
        ));
    }
    let p = Path::new(source);
    if p.components().any(|c| {
        matches!(
            c,
            std::path::Component::ParentDir | std::path::Component::CurDir
        )
    }) {
        return Err(err(
            ArchiveErrorCode::InvalidMemberPath,
            "Source path must not contain '.' or '..' components",
        ));
    }
    if !p.exists() {
        return Err(err(ArchiveErrorCode::InvalidMemberPath, "Source not found"));
    }
    let meta = std::fs::symlink_metadata(p).map_err(|e| io_err(e, p))?;
    if meta.file_type().is_symlink() {
        return Err(err(
            ArchiveErrorCode::UnsafeEntry,
            "Refusing to pack a symlink",
        ));
    }
    Ok(p.to_path_buf())
}

/// Recursively collect regular files under `root` (skipping symlinks),
/// returning `(disk_path, archive_name)` pairs.
pub fn collect_files(
    root: &Path,
    archive_name: &str,
    files: &mut Vec<(PathBuf, String)>,
) -> Result<(), ArchiveError> {
    let meta = std::fs::symlink_metadata(root).map_err(|e| io_err(e, root))?;
    if meta.file_type().is_symlink() {
        return Ok(());
    }
    if meta.is_dir() {
        let children = std::fs::read_dir(root).map_err(|e| io_err(e, root))?;
        let mut children: Vec<_> = children.filter_map(|e| e.ok()).collect();
        children.sort_by_key(|e| e.file_name());
        for child in children {
            let child_path = child.path();
            let child_meta =
                std::fs::symlink_metadata(&child_path).map_err(|e| io_err(e, &child_path))?;
            let child_name = format!("{archive_name}/{}", child.file_name().to_string_lossy());
            if child_meta.file_type().is_symlink() {
                continue; // D07
            }
            if child_meta.is_dir() {
                collect_files(&child_path, &child_name, files)?;
            } else {
                files.push((child_path, child_name));
            }
        }
    } else {
        files.push((root.to_path_buf(), archive_name.to_owned()));
    }
    Ok(())
}

/// Create a ZIP archive from `sources` at `destination_path`. The archive is
/// written to a `.ferrum-tmp` sibling then renamed — a failure never leaves
/// a partial file at the final path.
pub fn create_zip(
    sources: &[String],
    destination_path: &Path,
    limits: &ArchiveLimits,
) -> Result<(), ArchiveError> {
    create_zip_with_policy(
        sources,
        destination_path,
        limits,
        ArchiveConflictPolicy::Overwrite,
    )
    .map(|_| ())
}

/// Progress tracker for archive creation (FER-60/FER-67).
pub(crate) struct CreateTrack {
    pub(crate) bytes: u64,
    pub(crate) entries: u64,
    pub(crate) total_bytes: Option<u64>,
    pub(crate) total_entries: Option<u64>,
}

impl CreateTrack {
    pub(crate) fn emit(
        &self,
        progress: &mut dyn FnMut(ArchiveProgress) -> bool,
        member: Option<&MemberPath>,
    ) -> bool {
        progress(ArchiveProgress {
            operation_id: "create".to_owned(),
            kind: crate::archive_contract::ArchiveOperationKind::Create,
            processed_bytes: self.bytes,
            total_bytes: self.total_bytes,
            processed_entries: self.entries,
            total_entries: self.total_entries,
            current_member_path: member.cloned(),
            written_entries: None,
            skipped_entries: None,
        })
    }
}

/// Like [`create_zip`], with an explicit destination conflict policy; returns the path written.
pub fn create_zip_with_policy(
    sources: &[String],
    destination_path: &Path,
    limits: &ArchiveLimits,
    policy: ArchiveConflictPolicy,
) -> Result<PathBuf, ArchiveError> {
    create_zip_progress(sources, destination_path, limits, policy, &mut |_| true)
}

#[cfg(windows)]
unsafe extern "system" {
    fn FileTimeToSystemTime(
        lpFileTime: *const windows_sys::Win32::Foundation::FILETIME,
        lpSystemTime: *mut windows_sys::Win32::Foundation::SYSTEMTIME,
    ) -> i32;
}

#[cfg(windows)]
fn mtime_for_path(disk_path: &Path) -> (Option<zip::DateTime>, Option<u32>) {
    use std::os::windows::fs::MetadataExt;
    let Ok(meta) = disk_path.metadata() else {
        return (None, None);
    };
    let Ok(mtime_sys) = meta.modified() else {
        return (None, None);
    };
    let unix_secs = mtime_sys
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs() as u32);
    let ft_u64 = meta.last_write_time();
    let ft = windows_sys::Win32::Foundation::FILETIME {
        dwLowDateTime: (ft_u64 & 0xFFFF_FFFF) as u32,
        dwHighDateTime: (ft_u64 >> 32) as u32,
    };
    let mut local_ft = windows_sys::Win32::Foundation::FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let mut st = windows_sys::Win32::Foundation::SYSTEMTIME {
        wYear: 0,
        wMonth: 0,
        wDayOfWeek: 0,
        wDay: 0,
        wHour: 0,
        wMinute: 0,
        wSecond: 0,
        wMilliseconds: 0,
    };
    let dt = unsafe {
        if windows_sys::Win32::Storage::FileSystem::FileTimeToLocalFileTime(&ft, &mut local_ft) != 0
            && FileTimeToSystemTime(&local_ft, &mut st) != 0
        {
            zip::DateTime::from_date_and_time(
                st.wYear,
                st.wMonth as u8,
                st.wDay as u8,
                st.wHour as u8,
                st.wMinute as u8,
                st.wSecond as u8,
            )
            .ok()
        } else {
            None
        }
    };
    (dt, unix_secs)
}

#[cfg(not(windows))]
fn mtime_for_path(disk_path: &Path) -> (Option<zip::DateTime>, Option<u32>) {
    let Ok(meta) = disk_path.metadata() else {
        return (None, None);
    };
    let Ok(mtime_sys) = meta.modified() else {
        return (None, None);
    };
    let Ok(dur) = mtime_sys.duration_since(std::time::UNIX_EPOCH) else {
        return (None, None);
    };
    let unix_secs = dur.as_secs() as u32;
    let days = (unix_secs / 86400) as i64;
    let rem = (unix_secs % 86400) as u32;
    let hour = (rem / 3600) as u8;
    let minute = ((rem % 3600) / 60) as u8;
    let second = (rem % 60) as u8;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1020 + doe / 1460 - doe / 36524) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u8;
    let y = if m <= 2 { y + 1 } else { y };
    let dt = (1980..=2107)
        .contains(&y)
        .then(|| zip::DateTime::from_date_and_time(y as u16, m, d, hour, minute, second).ok())
        .flatten();
    (dt, Some(unix_secs))
}

fn options_for_entry(
    base: zip::write::FullFileOptions<'static>,
    disk_path: &Path,
) -> zip::write::FullFileOptions<'static> {
    let (dt, unix_secs) = mtime_for_path(disk_path);
    let mut options = base;
    if let Some(dt) = dt {
        options = options.last_modified_time(dt);
    }
    if let Some(secs) = unix_secs {
        let mut extra = vec![0x01]; // flags: mod_time present
        extra.extend_from_slice(&secs.to_le_bytes());
        let _ = options.add_extra_data(0x5455, extra.into_boxed_slice(), false);
    }
    options
}

/// Like [`create_zip_with_policy`], but with progress and cooperative cancellation (FER-60).
pub fn create_zip_progress(
    sources: &[String],
    destination_path: &Path,
    limits: &ArchiveLimits,
    policy: ArchiveConflictPolicy,
    progress: &mut dyn FnMut(ArchiveProgress) -> bool,
) -> Result<PathBuf, ArchiveError> {
    create_zip_progress_with_level(
        sources,
        destination_path,
        limits,
        policy,
        crate::archive_contract::ArchiveCompressionLevel::Normal,
        progress,
    )
}

/// Like [`create_zip_progress`], with an explicit compression level (FER-59).
pub fn create_zip_progress_with_level(
    sources: &[String],
    destination_path: &Path,
    limits: &ArchiveLimits,
    policy: ArchiveConflictPolicy,
    level: crate::archive_contract::ArchiveCompressionLevel,
    progress: &mut dyn FnMut(ArchiveProgress) -> bool,
) -> Result<PathBuf, ArchiveError> {
    create_zip_via_progress_with_level(
        sources,
        destination_path,
        limits,
        policy,
        level,
        &|file| Box::new(BufWriter::new(file)),
        progress,
    )
}

/// What the ZIP is written through; tests substitute a writer that fails like a full drive.
trait WriteSeek: Write + std::io::Seek {}
impl<T: Write + std::io::Seek> WriteSeek for T {}
type WrapWriter<'a> = &'a dyn Fn(File) -> Box<dyn WriteSeek>;

#[allow(dead_code)]
fn create_zip_via(
    sources: &[String],
    destination_path: &Path,
    limits: &ArchiveLimits,
    policy: ArchiveConflictPolicy,
    wrap: WrapWriter<'_>,
) -> Result<PathBuf, ArchiveError> {
    create_zip_via_progress_with_level(
        sources,
        destination_path,
        limits,
        policy,
        crate::archive_contract::ArchiveCompressionLevel::Normal,
        wrap,
        &mut |_| true,
    )
}

fn create_zip_via_progress_with_level(
    sources: &[String],
    destination_path: &Path,
    limits: &ArchiveLimits,
    policy: ArchiveConflictPolicy,
    level: crate::archive_contract::ArchiveCompressionLevel,
    wrap: WrapWriter<'_>,
    progress: &mut dyn FnMut(ArchiveProgress) -> bool,
) -> Result<PathBuf, ArchiveError> {
    level.check_for(crate::archive_contract::ArchiveCreateFormat::Zip)?;
    precheck_conflict(destination_path, policy)?;
    crate::archive_contract::check_cancelled()?;
    clean_stale_staging(destination_path);
    let files = collect_sources(sources, destination_path, policy, limits)?;
    if files.len() > limits.max_entries as usize {
        return Err(err(ArchiveErrorCode::LimitExceeded, "Too many files"));
    }

    crate::archive_contract::check_cancelled()?;
    let tmp_path = exclusive_tmp(destination_path)?;
    let written = File::create(&tmp_path)
        .map_err(|e| io_err(e, &tmp_path))
        .and_then(|file| write_zip(wrap(file), &files, level, progress));
    match written {
        Ok(()) => publish_created(&tmp_path, destination_path, policy).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp_path);
        }),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp_path);
            Err(e)
        }
    }
}

fn write_zip<W: Write + std::io::Seek>(
    out: W,
    files: &[SourceEntry],
    level: crate::archive_contract::ArchiveCompressionLevel,
    progress: &mut dyn FnMut(ArchiveProgress) -> bool,
) -> Result<(), ArchiveError> {
    crate::archive_contract::check_cancelled()?;
    let mut writer = zip::ZipWriter::new(out);
    let (method, comp_level) = match level {
        crate::archive_contract::ArchiveCompressionLevel::Store => {
            (zip::CompressionMethod::Stored, None)
        }
        crate::archive_contract::ArchiveCompressionLevel::Fast => {
            (zip::CompressionMethod::Deflated, Some(1))
        }
        crate::archive_contract::ArchiveCompressionLevel::Normal => {
            (zip::CompressionMethod::Deflated, Some(6))
        }
        crate::archive_contract::ArchiveCompressionLevel::Max => {
            (zip::CompressionMethod::Deflated, Some(9))
        }
        // FER-579: refused up front by `check_for`; never silently use another level.
        crate::archive_contract::ArchiveCompressionLevel::UltraText => {
            return Err(err(
                ArchiveErrorCode::UnsupportedFormat,
                "Compression level ultra-text is only available for 7z",
            ));
        }
    };
    let base_options = zip::write::FullFileOptions::default()
        .compression_method(method)
        .compression_level(comp_level);
    let mut buf = vec![0u8; 64 * 1024];
    let total_bytes: u64 = files
        .iter()
        .filter(|e| !e.is_dir)
        .filter_map(|e| e.disk_path.metadata().ok().map(|m| m.len()))
        .sum();
    let mut track = CreateTrack {
        bytes: 0,
        entries: 0,
        total_bytes: Some(total_bytes),
        total_entries: Some(files.len() as u64),
    };

    for entry in files {
        let member_path = MemberPath::parse(&entry.archive_name).ok();
        if !track.emit(progress, member_path.as_ref()) || crate::archive_contract::is_cancelled() {
            return Err(ArchiveError::new(
                ArchiveErrorCode::Cancelled,
                "archive create cancelled",
            ));
        }

        if entry.is_dir {
            let dir_name = format!("{}/", entry.archive_name.trim_end_matches('/'));
            let dir_options = options_for_entry(base_options.clone(), &entry.disk_path);
            writer
                .add_directory(&dir_name, dir_options)
                .map_err(|e| zip_err(e, "ZIP entry start"))?;
            track.entries += 1;
            if !track.emit(progress, member_path.as_ref())
                || crate::archive_contract::is_cancelled()
            {
                return Err(ArchiveError::new(
                    ArchiveErrorCode::Cancelled,
                    "archive create cancelled",
                ));
            }
            continue;
        }

        // Open before the entry starts: an unreadable source never leaves a half-written member.
        let mut src = File::open(&entry.disk_path).map_err(|e| read_err(e, &entry.disk_path))?;
        let file_options = options_for_entry(base_options.clone(), &entry.disk_path);
        writer
            .start_file(entry.archive_name.as_str(), file_options)
            .map_err(|e| zip_err(e, "ZIP entry start"))?;
        loop {
            if !track.emit(progress, member_path.as_ref())
                || crate::archive_contract::is_cancelled()
            {
                return Err(ArchiveError::new(
                    ArchiveErrorCode::Cancelled,
                    "archive create cancelled",
                ));
            }
            let n = src
                .read(&mut buf)
                .map_err(|e| read_err(e, &entry.disk_path))?;
            if n == 0 {
                break;
            }
            writer
                .write_all(&buf[..n])
                .map_err(|e| write_err(e, "ZIP write"))?;
            track.bytes += n as u64;
            if !track.emit(progress, member_path.as_ref())
                || crate::archive_contract::is_cancelled()
            {
                return Err(ArchiveError::new(
                    ArchiveErrorCode::Cancelled,
                    "archive create cancelled",
                ));
            }
        }
        track.entries += 1;
        if !track.emit(progress, member_path.as_ref()) || crate::archive_contract::is_cancelled() {
            return Err(ArchiveError::new(
                ArchiveErrorCode::Cancelled,
                "archive create cancelled",
            ));
        }
    }
    if !track.emit(progress, None) || crate::archive_contract::is_cancelled() {
        return Err(ArchiveError::new(
            ArchiveErrorCode::Cancelled,
            "archive create cancelled",
        ));
    }
    let mut inner = writer.finish().map_err(|e| zip_err(e, "ZIP finalize"))?;
    inner.flush().map_err(|e| write_err(e, "ZIP flush"))
}

/// Test all members of a ZIP archive (F2-08): decode each member fully —
/// the zip crate verifies CRC during decode. `password` decrypts AES
/// members; an encrypted member without one reports `PasswordRequired` for
/// that entry, a wrong one `BadPassword`.
pub fn test_zip(
    archive_path: &Path,
    password: Option<&str>,
) -> Result<ArchiveTestReport, ArchiveError> {
    test_zip_progress(archive_path, password, &mut |_| {})
}

/// Progress for one Test Archive run (FER-79): cumulative decoded bytes and members finished.
struct TestTrack {
    bytes: u64,
    entries: u64,
    total_bytes: Option<u64>,
    total_entries: Option<u64>,
}
impl TestTrack {
    fn emit(&self, progress: &mut dyn FnMut(ArchiveProgress), member: Option<&MemberPath>) {
        progress(ArchiveProgress {
            operation_id: "test".to_owned(),
            kind: crate::archive_contract::ArchiveOperationKind::Test,
            processed_bytes: self.bytes,
            total_bytes: self.total_bytes,
            processed_entries: self.entries,
            total_entries: self.total_entries,
            current_member_path: member.cloned(),
            written_entries: None,
            skipped_entries: None,
        });
    }
}

/// [`test_zip`] with a progress callback (called per decoded 64 KiB chunk and per member) and
/// cooperative cancel: the thread's cancel token (see `archive_contract::with_cancel`) is checked
/// before every chunk, so a cancel returns `Cancelled` within one chunk.
pub fn test_zip_progress(
    archive_path: &Path,
    password: Option<&str>,
    progress: &mut dyn FnMut(ArchiveProgress),
) -> Result<ArchiveTestReport, ArchiveError> {
    let file = File::open(archive_path).map_err(|e| io_err(e, archive_path))?;
    let mut archive = zip::ZipArchive::new(BufReader::new(file)).map_err(|e| {
        let code = match &e {
            zip::result::ZipError::UnsupportedArchive(_) => ArchiveErrorCode::UnsupportedCodec,
            zip::result::ZipError::InvalidArchive(_) => ArchiveErrorCode::CorruptArchive,
            _ => ArchiveErrorCode::Io,
        };
        ArchiveError::new(code, format!("ZIP open failed: {e}"))
    })?;

    let mut test_entries: Vec<ArchiveTestEntry> = Vec::new();
    let mut track = TestTrack {
        bytes: 0,
        entries: 0,
        total_bytes: (0..archive.len()).try_fold(0u64, |acc, i| {
            acc.checked_add(archive.by_index_raw(i).ok()?.size())
        }),
        total_entries: Some(archive.len() as u64),
    };
    for i in 0..archive.len() {
        crate::archive_contract::check_cancelled()?;
        let member_path = match archive.name_for_index(i) {
            Some(name) => MemberPath::parse(name)
                .unwrap_or_else(|_| MemberPath::parse(&format!("entry-{i}")).unwrap()),
            None => MemberPath::parse(&format!("entry-{i}")).unwrap(),
        };
        let report = |status, error| ArchiveTestEntry {
            member_path: member_path.clone(),
            status,
            error,
        };
        let opened = match password {
            Some(pw) => archive.by_index_decrypt(i, pw.as_bytes()),
            None => archive.by_index(i),
        };
        let mut zf = match opened {
            Ok(zf) => zf,
            Err(e) => {
                let (code, msg) = match &e {
                    zip::result::ZipError::UnsupportedArchive(m)
                        if *m == zip::result::ZipError::PASSWORD_REQUIRED =>
                    {
                        (
                            ArchiveErrorCode::PasswordRequired,
                            "encrypted member requires a password",
                        )
                    }
                    zip::result::ZipError::InvalidPassword => {
                        (ArchiveErrorCode::BadPassword, "wrong archive password")
                    }
                    _ => (ArchiveErrorCode::CorruptArchive, "unreadable entry"),
                };
                test_entries.push(report(ArchiveTestStatus::Failed, Some(err(code, msg))));
                continue;
            }
        };
        if zf.is_dir() {
            test_entries.push(report(ArchiveTestStatus::Passed, None));
            continue;
        }
        // Decode fully — the zip crate verifies CRC on read-to-EOF.
        let mut buf = [0u8; 64 * 1024];
        let mut decode_err: Option<String> = None;
        loop {
            crate::archive_contract::check_cancelled()?;
            match zf.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    track.bytes += n as u64;
                    track.emit(progress, Some(&member_path));
                }
                Err(e) => {
                    decode_err = Some(format!("decode failed: {e}"));
                    break;
                }
            }
        }
        track.entries += 1;
        track.emit(progress, Some(&member_path));
        test_entries.push(match decode_err {
            None => report(ArchiveTestStatus::Passed, None),
            Some(msg) => report(
                ArchiveTestStatus::Failed,
                Some(err(ArchiveErrorCode::CorruptArchive, msg)),
            ),
        });
    }

    Ok(ArchiveTestReport {
        archive_path: archive_path.to_string_lossy().into_owned(),
        entries: test_entries,
    })
}

/// Map an unrar failure onto a contract code by its structured status, never by message text
/// (FER-73): a missing password and a wrong password are different from damaged data.
fn unrar_code(e: &unrar::error::UnrarError) -> ArchiveErrorCode {
    use unrar::error::Code as C;
    match e.code {
        C::MissingPassword => ArchiveErrorCode::PasswordRequired,
        C::BadPassword => ArchiveErrorCode::BadPassword,
        _ => ArchiveErrorCode::CorruptArchive,
    }
}

/// Test all members of a RAR archive via the unrar crate's per-member test
/// mode (validates decode + CRC without extracting to disk). `password`
/// unlocks encrypted members via `Archive::with_password`.
pub fn test_rar(
    archive_path: &Path,
    password: Option<&str>,
) -> Result<ArchiveTestReport, ArchiveError> {
    test_rar_progress(archive_path, password, &mut |_| {})
}

/// [`test_rar`] with progress and cooperative cancel. unrar decodes one member per call, so a
/// cancel is honoured between members (not inside a single very large member).
pub fn test_rar_progress(
    archive_path: &Path,
    password: Option<&str>,
    progress: &mut dyn FnMut(ArchiveProgress),
) -> Result<ArchiveTestReport, ArchiveError> {
    use unrar::Archive;

    crate::archive_contract::check_cancelled()?;
    let open = match password {
        Some(pw) => Archive::with_password(archive_path, pw),
        None => Archive::new(archive_path),
    };
    let mut processing = open
        .open_for_processing()
        .map_err(|e| ArchiveError::new(unrar_code(&e), format!("unrar: {e}")))?;

    let mut test_entries: Vec<ArchiveTestEntry> = Vec::new();
    let mut track = TestTrack {
        bytes: 0,
        entries: 0,
        total_bytes: None,
        total_entries: None,
    };
    loop {
        crate::archive_contract::check_cancelled()?;
        let Some(header) = processing
            .read_header()
            .map_err(|e| ArchiveError::new(unrar_code(&e), format!("unrar: {e}")))?
        else {
            break;
        };
        let name = header.entry().filename.to_string_lossy().replace('\\', "/");
        let size = header.entry().unpacked_size;
        let member_path = MemberPath::parse(&name).unwrap_or_else(|_| {
            MemberPath::parse(&format!("entry-{}", test_entries.len())).unwrap()
        });
        match header.test() {
            Ok(rest) => {
                processing = rest;
                track.bytes = track.bytes.saturating_add(size);
                track.entries += 1;
                track.emit(progress, Some(&member_path));
                test_entries.push(ArchiveTestEntry {
                    member_path,
                    status: ArchiveTestStatus::Passed,
                    error: None,
                });
            }
            Err(e) => {
                // unrar's iterator is damaged after a test failure; report
                // and stop.
                test_entries.push(ArchiveTestEntry {
                    member_path,
                    status: ArchiveTestStatus::Failed,
                    error: Some(err(unrar_code(&e), format!("unrar: {e}"))),
                });
                break;
            }
        }
    }

    Ok(ArchiveTestReport {
        archive_path: archive_path.to_string_lossy().into_owned(),
        entries: test_entries,
    })
}

// ── 7z operations (F2-10/F2-11) ─────────────────────────────────────────

use sevenz_rust2::{ArchiveReader, ArchiveWriter, Password};

fn password_of(password: Option<&str>) -> Password {
    password.map(Password::from).unwrap_or_else(Password::empty)
}

/// Map sevenz-rust2 errors onto contract codes (mirrors archive_sevenz).
fn sevenz_err(e: sevenz_rust2::Error) -> ArchiveError {
    use sevenz_rust2::Error as E;
    let code = match &e {
        E::PasswordRequired => ArchiveErrorCode::PasswordRequired,
        E::MaybeBadPassword(_) => ArchiveErrorCode::BadPassword,
        E::BadSignature(_) => ArchiveErrorCode::UnsupportedFormat,
        E::Unsupported(_) | E::UnsupportedCompressionMethod(_) | E::ExternalUnsupported => {
            ArchiveErrorCode::UnsupportedCodec
        }
        E::MaxMemLimited { .. } => ArchiveErrorCode::LimitExceeded,
        _ => ArchiveErrorCode::CorruptArchive,
    };
    ArchiveError::new(code, format!("7z: {e}"))
}

/// Test all members of a 7z archive: decode each member fully, verifying
/// CRC via the sevenz-rust2 crate's decoder. `password` unlocks encrypted
/// content; without it an encrypted member surfaces `PasswordRequired`.
pub fn test_7z(
    archive_path: &Path,
    password: Option<&str>,
) -> Result<ArchiveTestReport, ArchiveError> {
    test_7z_progress(archive_path, password, &mut |_| {})
}

/// AES-256 + SHA-256 coder id of a 7z block.
const SEVENZ_AES_ID: [u8; 4] = [0x06, 0xF1, 0x07, 0x01];

/// [`test_7z`] with progress and cooperative cancel (checked before every 64 KiB chunk).
pub fn test_7z_progress(
    archive_path: &Path,
    password: Option<&str>,
    progress: &mut dyn FnMut(ArchiveProgress),
) -> Result<ArchiveTestReport, ArchiveError> {
    crate::archive_contract::check_cancelled()?;
    let mut reader = ArchiveReader::open(archive_path, password_of(password)).map_err(|e| {
        let mut err = sevenz_err(e);
        err.archive_path = Some(archive_path.to_string_lossy().into_owned());
        err
    })?;
    // FER-104 N1: single-threaded decode (see archive_sevenz::open_reader).
    reader.set_thread_count(1);
    // FER-579: PPMd allocates its declared model up front; refuse absurd sizes before decoding.
    crate::archive_contract::check_ppmd_memory(
        reader.archive(),
        crate::archive_contract::PPMD_DECODE_MAX_MEMORY,
    )
    .map_err(|mut e| {
        e.archive_path = Some(archive_path.to_string_lossy().into_owned());
        e
    })?;

    // A decode failure inside AES content while a password WAS supplied cannot be told apart from
    // a wrong password (the decoder only sees garbage), so it is reported as `BadPassword`; the
    // same failure in plain content is real damage (`CorruptArchive`).
    let encrypted = reader.archive().blocks.iter().any(|b| {
        b.coders
            .iter()
            .any(|c| c.encoder_method_id() == SEVENZ_AES_ID)
    });
    let wrong_password_suspected = encrypted && password.is_some_and(|p| !p.is_empty());
    let mut track = TestTrack {
        bytes: 0,
        entries: 0,
        total_bytes: Some(
            reader
                .archive()
                .files
                .iter()
                .filter(|f| !f.is_directory())
                .map(|f| f.size())
                .sum(),
        ),
        total_entries: Some(reader.archive().files.len() as u64),
    };

    let mut test_entries: Vec<ArchiveTestEntry> = Vec::new();
    let mut cancelled = false;
    reader
        .for_each_entries(|entry, reader| {
            let name = entry.name().to_owned();
            let member_path = MemberPath::parse(&name).unwrap_or_else(|_| {
                MemberPath::parse(&format!("entry-{}", test_entries.len())).unwrap()
            });
            let mut buf = [0u8; 64 * 1024];
            loop {
                if crate::archive_contract::is_cancelled() {
                    cancelled = true;
                    return Ok(false);
                }
                match reader.read(&mut buf) {
                    Ok(0) => {
                        track.entries += 1;
                        track.emit(progress, Some(&member_path));
                        test_entries.push(ArchiveTestEntry {
                            member_path,
                            status: ArchiveTestStatus::Passed,
                            error: None,
                        });
                        return Ok(true);
                    }
                    Ok(n) => {
                        track.bytes += n as u64;
                        track.emit(progress, Some(&member_path));
                    }
                    Err(e) => {
                        // The callback's stream is a plain Read: decode-level
                        // failures surface through for_each_entries' own
                        // Result and are mapped at the outer call below.
                        let (code, text) = if wrong_password_suspected {
                            (
                                ArchiveErrorCode::BadPassword,
                                format!("wrong password or damaged encrypted data: {e}"),
                            )
                        } else {
                            (ArchiveErrorCode::CorruptArchive, format!("decode: {e}"))
                        };
                        test_entries.push(ArchiveTestEntry {
                            member_path,
                            status: ArchiveTestStatus::Failed,
                            error: Some(err(code, text)),
                        });
                        return Ok(true);
                    }
                }
            }
        })
        .map_err(sevenz_err)?;
    if cancelled {
        return Err(ArchiveError::new(
            ArchiveErrorCode::Cancelled,
            "archive test cancelled",
        ));
    }

    Ok(ArchiveTestReport {
        archive_path: archive_path.to_string_lossy().into_owned(),
        entries: test_entries,
    })
}

struct SevenZProgressReader<'a, R: Read> {
    inner: R,
    track: &'a mut CreateTrack,
    progress: &'a mut dyn FnMut(ArchiveProgress) -> bool,
    member_path: Option<&'a MemberPath>,
    cancelled: &'a mut bool,
}

impl<'a, R: Read> Read for SevenZProgressReader<'a, R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if *self.cancelled || crate::archive_contract::is_cancelled() {
            *self.cancelled = true;
            return Err(std::io::Error::other("archive create cancelled"));
        }
        let n = self.inner.read(buf)?;
        if n > 0 {
            self.track.bytes += n as u64;
            if !self.track.emit(self.progress, self.member_path)
                || crate::archive_contract::is_cancelled()
            {
                *self.cancelled = true;
                return Err(std::io::Error::other("archive create cancelled"));
            }
        }
        Ok(n)
    }
}

fn mtime_for_7z(disk_path: &Path) -> Option<sevenz_rust2::NtTime> {
    let meta = disk_path.metadata().ok()?;
    let mtime = meta.modified().ok()?;
    sevenz_rust2::NtTime::try_from(mtime).ok()
}

/// FER-579: PPMd settings of the `ultra-text` 7z level. These are the values 7-Zip itself uses
/// for `-mx=9 -m0=PPMd` (order 32, 192 MiB model memory). PPMd is symmetric, so the reader needs
/// the same memory: 192 MiB stays under the reader's 256 MiB codec cap (`archive_sevenz`).
pub const ULTRA_TEXT_PPMD_ORDER: u32 = 32;
pub const ULTRA_TEXT_PPMD_MEMORY: u32 = 192 * 1024 * 1024;

/// 7-Zip `CEncProps::Normalize` for PPMd: the smallest 2^i (i in 16..=31) with `size <= 2^i / 16`,
/// capped at the 192 MiB maximum. The model of a smaller file never fills, so the ratio is the
/// same; order stays 32 (7-Zip only changes memory).
pub fn ultra_text_mem_for(size: u64) -> u32 {
    for i in 16..=31u32 {
        let m = 1u64 << i;
        if size <= m / 16 {
            return m.min(u64::from(ULTRA_TEXT_PPMD_MEMORY)) as u32;
        }
    }
    ULTRA_TEXT_PPMD_MEMORY
}

/// Create a 7z archive from `sources` at `destination_path`.
pub fn create_7z(sources: &[String], destination_path: &Path) -> Result<(), ArchiveError> {
    create_7z_with_policy(sources, destination_path, ArchiveConflictPolicy::Overwrite).map(|_| ())
}

/// Like [`create_7z`], with an explicit destination conflict policy; returns the path written.
pub fn create_7z_with_policy(
    sources: &[String],
    destination_path: &Path,
    policy: ArchiveConflictPolicy,
) -> Result<PathBuf, ArchiveError> {
    create_7z_progress(sources, destination_path, policy, &mut |_| true)
}

/// Like [`create_7z_with_policy`], but with progress and cooperative cancellation (FER-67).
pub fn create_7z_progress(
    sources: &[String],
    destination_path: &Path,
    policy: ArchiveConflictPolicy,
    progress: &mut dyn FnMut(ArchiveProgress) -> bool,
) -> Result<PathBuf, ArchiveError> {
    create_7z_progress_with_level(
        sources,
        destination_path,
        policy,
        crate::archive_contract::ArchiveCompressionLevel::Normal,
        progress,
    )
}

/// Like [`create_7z_progress`], with an explicit compression level (FER-66).
pub fn create_7z_progress_with_level(
    sources: &[String],
    destination_path: &Path,
    policy: ArchiveConflictPolicy,
    level: crate::archive_contract::ArchiveCompressionLevel,
    progress: &mut dyn FnMut(ArchiveProgress) -> bool,
) -> Result<PathBuf, ArchiveError> {
    level.check_for(crate::archive_contract::ArchiveCreateFormat::Sevenz)?;
    precheck_conflict(destination_path, policy)?;
    crate::archive_contract::check_cancelled()?;
    clean_stale_staging(destination_path);
    let default_limits = ArchiveLimits {
        max_entries: 100_000,
        max_total_bytes: u64::MAX,
        max_entry_bytes: 512 * 1024 * 1024,
        max_depth: 32,
    };
    let files = collect_sources(sources, destination_path, policy, &default_limits)?;

    let tmp_path = exclusive_tmp(destination_path)?;
    let total_bytes: u64 = files
        .iter()
        .filter(|e| !e.is_dir)
        .filter_map(|e| e.disk_path.metadata().ok().map(|m| m.len()))
        .sum();
    let mut track = CreateTrack {
        bytes: 0,
        entries: 0,
        total_bytes: Some(total_bytes),
        total_entries: Some(files.len() as u64),
    };

    let mut cancelled = false;
    let written = (|| -> Result<(), ArchiveError> {
        crate::archive_contract::check_cancelled()?;
        let mut writer = ArchiveWriter::create(&tmp_path)
            .map_err(|e| ArchiveError::new(ArchiveErrorCode::Io, format!("7z create: {e}")))?;
        match level {
            crate::archive_contract::ArchiveCompressionLevel::Store => {
                let config =
                    sevenz_rust2::EncoderConfiguration::new(sevenz_rust2::EncoderMethod::COPY);
                writer.set_content_methods(vec![config]);
            }
            crate::archive_contract::ArchiveCompressionLevel::Fast => {
                let opts = sevenz_rust2::encoder_options::Lzma2Options::from_level(1);
                let config = sevenz_rust2::EncoderConfiguration::from(opts);
                writer.set_content_methods(vec![config]);
            }
            crate::archive_contract::ArchiveCompressionLevel::Normal => {
                let opts = sevenz_rust2::encoder_options::Lzma2Options::from_level(6);
                let config = sevenz_rust2::EncoderConfiguration::from(opts);
                writer.set_content_methods(vec![config]);
            }
            crate::archive_contract::ArchiveCompressionLevel::Max => {
                let mut opts = sevenz_rust2::encoder_options::Lzma2Options::from_level(9);
                // FER-66: Limit dictionary size to 16 MiB so encoder memory remains under 256 MiB (from_level is single-threaded).
                opts.set_dictionary_size(16 * 1024 * 1024);
                let config = sevenz_rust2::EncoderConfiguration::from(opts);
                writer.set_content_methods(vec![config]);
            }
            // Sized per file inside the loop below.
            crate::archive_contract::ArchiveCompressionLevel::UltraText => {}
        }
        for entry in &files {
            let member_path = MemberPath::parse(&entry.archive_name).ok();
            if !track.emit(progress, member_path.as_ref())
                || crate::archive_contract::is_cancelled()
            {
                cancelled = true;
                return Err(ArchiveError::new(
                    ArchiveErrorCode::Cancelled,
                    "archive create cancelled",
                ));
            }
            if entry.is_dir {
                let mut dir_entry = sevenz_rust2::ArchiveEntry::new_directory(&entry.archive_name);
                if let Some(nt) = mtime_for_7z(&entry.disk_path) {
                    dir_entry.last_modified_date = nt;
                    dir_entry.has_last_modified_date = true;
                }
                let none: Option<&[u8]> = None;
                writer.push_archive_entry(dir_entry, none).map_err(|e| {
                    if cancelled || crate::archive_contract::is_cancelled() {
                        ArchiveError::new(ArchiveErrorCode::Cancelled, "archive create cancelled")
                    } else {
                        ArchiveError::new(ArchiveErrorCode::Io, format!("7z push: {e}"))
                    }
                })?;
                track.entries += 1;
                if !track.emit(progress, member_path.as_ref())
                    || crate::archive_contract::is_cancelled()
                {
                    cancelled = true;
                    return Err(ArchiveError::new(
                        ArchiveErrorCode::Cancelled,
                        "archive create cancelled",
                    ));
                }
                continue;
            }
            let src = File::open(&entry.disk_path).map_err(|e| read_err(e, &entry.disk_path))?;
            if level == crate::archive_contract::ArchiveCompressionLevel::UltraText {
                // FER-579: every file is its own block and PPMd allocates + zero-fills its whole
                // model per block, so size it to the file like 7-Zip does.
                let size = entry
                    .disk_path
                    .metadata()
                    .map(|m| m.len())
                    .unwrap_or(u64::MAX);
                let opts = sevenz_rust2::encoder_options::PpmdOptions::from_order_memory_size(
                    ULTRA_TEXT_PPMD_ORDER,
                    ultra_text_mem_for(size),
                );
                writer.set_content_methods(vec![sevenz_rust2::EncoderConfiguration::from(opts)]);
            }
            let mut file_entry = sevenz_rust2::ArchiveEntry::new_file(&entry.archive_name);
            if let Some(nt) = mtime_for_7z(&entry.disk_path) {
                file_entry.last_modified_date = nt;
                file_entry.has_last_modified_date = true;
            }
            let progress_reader = SevenZProgressReader {
                inner: BufReader::new(src),
                track: &mut track,
                progress,
                member_path: member_path.as_ref(),
                cancelled: &mut cancelled,
            };
            writer
                .push_archive_entry(file_entry, Some(progress_reader))
                .map_err(|e| {
                    if cancelled || crate::archive_contract::is_cancelled() {
                        ArchiveError::new(ArchiveErrorCode::Cancelled, "archive create cancelled")
                    } else {
                        ArchiveError::new(ArchiveErrorCode::Io, format!("7z push: {e}"))
                    }
                })?;
            track.entries += 1;
            if !track.emit(progress, member_path.as_ref())
                || crate::archive_contract::is_cancelled()
            {
                cancelled = true;
                return Err(ArchiveError::new(
                    ArchiveErrorCode::Cancelled,
                    "archive create cancelled",
                ));
            }
        }
        if !track.emit(progress, None) || crate::archive_contract::is_cancelled() {
            cancelled = true;
            return Err(ArchiveError::new(
                ArchiveErrorCode::Cancelled,
                "archive create cancelled",
            ));
        }
        writer
            .finish()
            .map_err(|e| ArchiveError::new(ArchiveErrorCode::Io, format!("7z finish: {e}")))?;
        Ok(())
    })();
    // A failure never leaves a partial archive or a temp file behind.
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(e);
    }
    publish_created(&tmp_path, destination_path, policy).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp_path);
    })
}

#[cfg(test)]
mod conflict_tests {
    //! FER-411: destination conflict policy when creating an archive. Temp dirs only.
    use super::*;
    use std::fs;

    struct Dir(PathBuf);
    impl Dir {
        fn new(tag: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let dir = std::env::temp_dir().join(format!(
                "ferrum-create-{tag}-{}-{nanos}",
                std::process::id()
            ));
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn source(&self) -> String {
            let p = self.0.join("source.txt");
            fs::write(&p, "payload").unwrap();
            p.to_string_lossy().into_owned()
        }
        fn dest(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
        fn leftovers(&self) -> Vec<String> {
            fs::read_dir(&self.0)
                .unwrap()
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.ends_with(".ferrum-tmp"))
                .collect()
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn limits() -> ArchiveLimits {
        ArchiveLimits {
            max_entries: 100,
            max_total_bytes: u64::MAX,
            max_entry_bytes: 1024 * 1024,
            max_depth: 8,
        }
    }
    fn make(dir: &Dir, name: &str, policy: ArchiveConflictPolicy) -> Result<PathBuf, ArchiveError> {
        create_zip_with_policy(&[dir.source()], &dir.dest(name), &limits(), policy)
    }
    fn is_valid_zip(path: &Path) -> bool {
        test_zip(path, None)
            .map(|r| !r.entries.is_empty())
            .unwrap_or(false)
    }

    #[test]
    fn skip_never_overwrites_an_existing_file_and_reports_a_conflict() {
        let dir = Dir::new("skip");
        let existing = dir.dest("out.zip");
        fs::write(&existing, "someone else's archive").unwrap();
        let e = make(&dir, "out.zip", ArchiveConflictPolicy::Skip).unwrap_err();
        assert_eq!(e.code, ArchiveErrorCode::Conflict);
        assert_eq!(
            fs::read_to_string(&existing).unwrap(),
            "someone else's archive"
        );
        assert!(
            dir.leftovers().is_empty(),
            "no temp file left: {:?}",
            dir.leftovers()
        );
    }

    #[test]
    fn skip_creates_the_archive_when_the_name_is_free() {
        let dir = Dir::new("skip-free");
        let written = make(&dir, "out.zip", ArchiveConflictPolicy::Skip).unwrap();
        assert_eq!(written, dir.dest("out.zip"));
        assert!(is_valid_zip(&written));
        assert!(dir.leftovers().is_empty());
    }

    #[test]
    fn skip_refuses_when_a_directory_has_that_name() {
        let dir = Dir::new("skip-dir");
        fs::create_dir(dir.dest("out.zip")).unwrap();
        let e = make(&dir, "out.zip", ArchiveConflictPolicy::Skip).unwrap_err();
        assert_eq!(e.code, ArchiveErrorCode::Conflict);
        assert!(dir.dest("out.zip").is_dir());
    }

    #[test]
    fn rename_picks_the_first_free_numbered_name_and_leaves_the_old_file_alone() {
        let dir = Dir::new("rename");
        fs::write(dir.dest("out.zip"), "old one").unwrap();
        let second = make(&dir, "out.zip", ArchiveConflictPolicy::Rename).unwrap();
        assert_eq!(second, dir.dest("out (2).zip"));
        let third = make(&dir, "out.zip", ArchiveConflictPolicy::Rename).unwrap();
        assert_eq!(third, dir.dest("out (3).zip"));
        assert_eq!(fs::read_to_string(dir.dest("out.zip")).unwrap(), "old one");
        assert!(is_valid_zip(&second) && is_valid_zip(&third));
        assert!(dir.leftovers().is_empty());
    }

    #[test]
    fn rename_uses_the_requested_name_when_it_is_free() {
        let dir = Dir::new("rename-free");
        let written = make(&dir, "fresh.zip", ArchiveConflictPolicy::Rename).unwrap();
        assert_eq!(written, dir.dest("fresh.zip"));
    }

    #[test]
    fn overwrite_still_replaces_an_existing_destination_when_asked_explicitly() {
        let dir = Dir::new("overwrite");
        fs::write(dir.dest("out.zip"), "stale").unwrap();
        let written = make(&dir, "out.zip", ArchiveConflictPolicy::Overwrite).unwrap();
        assert!(is_valid_zip(&written));
        assert!(dir.leftovers().is_empty());
    }

    #[test]
    fn overwrite_older_is_rejected_for_creation_and_changes_nothing() {
        let dir = Dir::new("older");
        fs::write(dir.dest("out.zip"), "keep me").unwrap();
        let e = make(&dir, "out.zip", ArchiveConflictPolicy::OverwriteOlder).unwrap_err();
        assert_eq!(e.code, ArchiveErrorCode::Conflict);
        assert_eq!(fs::read_to_string(dir.dest("out.zip")).unwrap(), "keep me");
    }

    #[test]
    fn a_users_file_next_to_the_destination_is_never_truncated_by_the_temp_file() {
        // The old temp name was `<stem>.ferrum-tmp`: a user's file with that name was truncated.
        let dir = Dir::new("tmpname");
        let theirs = dir.dest("out.ferrum-tmp");
        fs::write(&theirs, "user data").unwrap();
        let written = make(&dir, "out.zip", ArchiveConflictPolicy::Skip).unwrap();
        assert!(is_valid_zip(&written));
        assert_eq!(fs::read_to_string(&theirs).unwrap(), "user data");
    }

    #[test]
    fn a_failed_create_leaves_no_partial_archive_and_no_temp_file() {
        let dir = Dir::new("failure");
        let missing = dir
            .dest("does-not-exist.txt")
            .to_string_lossy()
            .into_owned();
        let e = create_zip_with_policy(
            &[missing],
            &dir.dest("out.zip"),
            &limits(),
            ArchiveConflictPolicy::Skip,
        )
        .unwrap_err();
        assert_ne!(e.code, ArchiveErrorCode::Conflict);
        assert!(
            !dir.dest("out.zip").exists(),
            "no placeholder or partial archive"
        );
        assert!(dir.leftovers().is_empty());
    }

    #[test]
    fn the_legacy_wrapper_keeps_overwrite_semantics() {
        let dir = Dir::new("legacy");
        fs::write(dir.dest("out.zip"), "stale").unwrap();
        create_zip(&[dir.source()], &dir.dest("out.zip"), &limits()).unwrap();
        assert!(is_valid_zip(&dir.dest("out.zip")));
    }

    #[test]
    fn sevenz_follows_the_same_policy() {
        let dir = Dir::new("sevenz");
        fs::write(dir.dest("out.7z"), "theirs").unwrap();
        let e = create_7z_with_policy(
            &[dir.source()],
            &dir.dest("out.7z"),
            ArchiveConflictPolicy::Skip,
        )
        .unwrap_err();
        assert_eq!(e.code, ArchiveErrorCode::Conflict);
        assert_eq!(fs::read_to_string(dir.dest("out.7z")).unwrap(), "theirs");
        let renamed = create_7z_with_policy(
            &[dir.source()],
            &dir.dest("out.7z"),
            ArchiveConflictPolicy::Rename,
        )
        .unwrap();
        assert_eq!(renamed, dir.dest("out (2).7z"));
        assert!(dir.leftovers().is_empty());
    }
}

#[cfg(test)]
mod edge_tests {
    //! FER-205: output inside the source subtree, unreadable input, a full drive. Real files in temp dirs;
    //! only the full drive is simulated (a writer that fails like Windows does with ERROR_DISK_FULL).
    use super::*;
    use crate::archive_contract::ArchiveTestStatus;
    use std::fs;

    struct Dir(PathBuf);
    impl Dir {
        fn new(tag: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let dir = std::env::temp_dir()
                .join(format!("ferrum-edge-{tag}-{}-{nanos}", std::process::id()));
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn path(&self, rel: &str) -> PathBuf {
            rel.split('/').fold(self.0.clone(), |p, part| p.join(part))
        }
        fn write(&self, rel: &str, body: &[u8]) -> PathBuf {
            let p = self.path(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, body).unwrap();
            p
        }
        fn staging_files(&self) -> Vec<PathBuf> {
            let mut out = Vec::new();
            let mut stack = vec![self.0.clone()];
            while let Some(d) = stack.pop() {
                for e in fs::read_dir(&d).unwrap().flatten() {
                    let p = e.path();
                    if p.is_dir() {
                        stack.push(p);
                    } else if p.to_string_lossy().ends_with(".ferrum-tmp") {
                        out.push(p);
                    }
                }
            }
            out
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn s(p: &Path) -> String {
        p.to_string_lossy().into_owned()
    }
    fn limits() -> ArchiveLimits {
        ArchiveLimits {
            max_entries: 1000,
            max_total_bytes: u64::MAX,
            max_entry_bytes: 64 * 1024 * 1024,
            max_depth: 16,
        }
    }
    fn members(zip_path: &Path) -> Vec<String> {
        let mut a = zip::ZipArchive::new(File::open(zip_path).unwrap()).unwrap();
        let mut names: Vec<String> = (0..a.len())
            .map(|i| a.by_index(i).unwrap().name().to_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn staging_names_are_recognised_exactly() {
        assert!(is_ferrum_staging(".out.zip.1234-0.ferrum-tmp"));
        assert!(is_ferrum_staging(".a b (2).zip.7-99.ferrum-tmp"));
        assert!(!is_ferrum_staging("out.ferrum-tmp"));
        assert!(!is_ferrum_staging(".out.zip.ferrum-tmp"));
        assert!(!is_ferrum_staging(".out.zip.12-x.ferrum-tmp"));
        assert!(!is_ferrum_staging(".out.zip.1234-0.ferrum-tmp.txt"));
    }

    #[test]
    fn output_inside_a_selected_folder_never_becomes_input() {
        let dir = Dir::new("subtree");
        dir.write("proj/a.txt", b"alpha");
        dir.write("proj/sub/b.txt", b"beta");
        // Another Ferrum archive is being written into the same folder right now.
        dir.write("proj/sub/.other.zip.4242-0.ferrum-tmp", b"half written");
        let dest = dir.path("proj/sub/proj.zip");
        let source = [s(&dir.path("proj"))];

        let first =
            create_zip_with_policy(&source, &dest, &limits(), ArchiveConflictPolicy::Skip).unwrap();
        assert_eq!(first, dest);
        assert_eq!(members(&dest), vec!["proj/a.txt", "proj/sub/b.txt"]);

        // Re-packing over the existing archive (Overwrite): the old archive is not packed into the new one.
        create_zip_with_policy(&source, &dest, &limits(), ArchiveConflictPolicy::Overwrite)
            .unwrap();
        assert_eq!(members(&dest), vec!["proj/a.txt", "proj/sub/b.txt"]);
        let report = test_zip(&dest, None).unwrap();
        assert!(
            report
                .entries
                .iter()
                .all(|e| e.status == ArchiveTestStatus::Passed)
        );

        // Rename: the new numbered archive does not contain itself; the earlier archive is ordinary input.
        let renamed =
            create_zip_with_policy(&source, &dest, &limits(), ArchiveConflictPolicy::Rename)
                .unwrap();
        assert_eq!(renamed, dir.path("proj/sub/proj (2).zip"));
        assert_eq!(
            members(&renamed),
            vec!["proj/a.txt", "proj/sub/b.txt", "proj/sub/proj.zip"]
        );
        // Our own staging is gone; the other writer's staging file was neither packed nor touched.
        assert_eq!(
            dir.staging_files(),
            vec![dir.path("proj/sub/.other.zip.4242-0.ferrum-tmp")]
        );
    }

    #[test]
    fn the_destination_is_matched_like_the_filesystem_matches_names() {
        let dir = Dir::new("case");
        dir.write("proj/a.txt", b"alpha");
        dir.write("proj/OUT.ZIP", b"previous");
        let dest = dir.path("proj/out.zip");
        create_zip_with_policy(
            &[s(&dir.path("proj"))],
            &dest,
            &limits(),
            ArchiveConflictPolicy::Overwrite,
        )
        .unwrap();
        let names = members(&dest);
        if cfg!(windows) {
            // NTFS: OUT.ZIP and out.zip are the same file, i.e. the output itself.
            assert_eq!(names, vec!["proj/a.txt"]);
        } else {
            assert!(names.contains(&"proj/a.txt".to_owned()));
        }
    }

    #[test]
    fn a_source_that_is_the_destination_is_refused_and_left_untouched() {
        let dir = Dir::new("self");
        let me = dir.write("me.zip", b"original");
        let e = create_zip_with_policy(&[s(&me)], &me, &limits(), ArchiveConflictPolicy::Overwrite)
            .unwrap_err();
        assert_eq!(e.code, ArchiveErrorCode::InvalidMemberPath);
        assert_eq!(fs::read(&me).unwrap(), b"original");
        assert!(dir.staging_files().is_empty());
    }

    #[test]
    fn sevenz_never_packs_its_own_output_either() {
        let dir = Dir::new("sevenz-subtree");
        dir.write("proj/a.txt", b"alpha");
        dir.write("proj/proj.7z", b"previous");
        let dest = dir.path("proj/proj.7z");
        create_7z_with_policy(
            &[s(&dir.path("proj"))],
            &dest,
            ArchiveConflictPolicy::Overwrite,
        )
        .unwrap();
        let names: Vec<String> = test_7z(&dest, None)
            .unwrap()
            .entries
            .into_iter()
            .map(|e| e.member_path.as_str().to_owned())
            .collect();
        assert_eq!(names, vec!["proj/a.txt"]);
    }

    /// FER-510: two sources from different base folders with the same relative member name are
    /// refused up front with `Conflict` + `member_path` — before any process runs or a byte is
    /// written. ZIP and 7z share the same pipeline, so all three formats refuse identically.
    #[test]
    fn duplicate_member_names_from_different_folders_are_refused_up_front() {
        let dir = Dir::new("dup");
        dir.write("left/same.txt", b"one");
        dir.write("right/Same.txt", b"two");
        // Two FILES from different folders: members are their base names only.
        let sources = [
            s(&dir.path("left/same.txt")),
            s(&dir.path("right/Same.txt")),
        ];

        // ZIP: refused, case-insensitively (same.txt vs Same.txt), with the member named.
        let dest = dir.path("out.zip");
        let e = create_zip_with_policy(&sources, &dest, &limits(), ArchiveConflictPolicy::Skip)
            .unwrap_err();
        assert_eq!(e.code, ArchiveErrorCode::Conflict);
        assert_eq!(e.member_path.as_deref(), Some("Same.txt"));
        assert!(
            e.message.contains("Duplicate archive member name"),
            "{}",
            e.message
        );
        assert!(!dest.exists(), "nothing written");
        assert!(dir.staging_files().is_empty(), "no temp file left");

        // 7z: identical refusal through the shared pipeline.
        let e = create_7z_with_policy(&sources, &dir.path("out.7z"), ArchiveConflictPolicy::Skip)
            .unwrap_err();
        assert_eq!(e.code, ArchiveErrorCode::Conflict);
        assert_eq!(e.member_path.as_deref(), Some("Same.txt"));

        // A nested duplicate: two FOLDERS with the same name under different parents both
        // contribute `docs/x.txt`.
        dir.write("left2/docs/x.txt", b"x1");
        dir.write("right2/docs/x.txt", b"x2");
        let e = create_zip_with_policy(
            &[s(&dir.path("left2/docs")), s(&dir.path("right2/docs"))],
            &dest,
            &limits(),
            ArchiveConflictPolicy::Skip,
        )
        .unwrap_err();
        assert_eq!(e.code, ArchiveErrorCode::Conflict);
        assert_eq!(e.member_path.as_deref(), Some("docs/x.txt"));

        // Distinct names still pack from multiple folders (folder + single file).
        dir.write("right/z.txt", b"zee");
        let written = create_zip_with_policy(
            &[s(&dir.path("left")), s(&dir.path("right/z.txt"))],
            &dest,
            &limits(),
            ArchiveConflictPolicy::Skip,
        )
        .unwrap();
        assert_eq!(members(&written), vec!["left/same.txt", "z.txt"]);
    }

    #[cfg(windows)]
    fn lock_exclusively(path: &Path) -> File {
        use std::os::windows::fs::OpenOptionsExt;
        OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(path)
            .unwrap()
    }

    #[cfg(windows)]
    #[test]
    fn an_unreadable_source_fails_with_its_name_and_leaves_nothing_behind() {
        let dir = Dir::new("locked");
        dir.write("proj/a.txt", b"alpha");
        let locked = dir.write("proj/sub/locked.txt", b"in use");
        dir.write("proj/z.txt", b"zulu");
        let _guard = lock_exclusively(&locked);
        let dest = dir.path("out.zip");
        let e = create_zip_with_policy(
            &[s(&dir.path("proj"))],
            &dest,
            &limits(),
            ArchiveConflictPolicy::Skip,
        )
        .unwrap_err();
        assert_eq!(e.code, ArchiveErrorCode::Io);
        assert!(e.message.starts_with(SOURCE_UNREADABLE), "{}", e.message);
        assert!(e.message.contains("locked.txt"), "{}", e.message);
        assert_eq!(e.archive_path.as_deref(), Some(s(&locked).as_str()));
        assert!(
            !dest.exists(),
            "no partial archive and no placeholder at the destination"
        );
        assert!(dir.staging_files().is_empty(), "{:?}", dir.staging_files());
    }

    #[cfg(windows)]
    #[test]
    fn sevenz_reports_an_unreadable_source_the_same_way() {
        let dir = Dir::new("locked7z");
        let locked = dir.write("proj/locked.txt", b"in use");
        let _guard = lock_exclusively(&locked);
        let dest = dir.path("out.7z");
        let e = create_7z_with_policy(&[s(&dir.path("proj"))], &dest, ArchiveConflictPolicy::Skip)
            .unwrap_err();
        assert!(e.message.starts_with(SOURCE_UNREADABLE), "{}", e.message);
        assert!(!dest.exists());
        assert!(dir.staging_files().is_empty());
    }

    /// Accepts `capacity` bytes, then fails every write the way a full NTFS volume does.
    struct FullDrive {
        inner: File,
        capacity: usize,
        written: usize,
    }
    impl Write for FullDrive {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if self.written + buf.len() > self.capacity {
                return Err(if cfg!(windows) {
                    std::io::Error::from_raw_os_error(112) // ERROR_DISK_FULL
                } else {
                    std::io::Error::from(std::io::ErrorKind::StorageFull)
                });
            }
            self.written += buf.len();
            self.inner.write(buf)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.inner.flush()
        }
    }
    impl std::io::Seek for FullDrive {
        fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
            self.inner.seek(pos)
        }
    }

    #[test]
    fn a_full_drive_fails_cleanly_with_a_space_message() {
        let dir = Dir::new("full");
        // Incompressible payload larger than the simulated free space.
        let mut payload = vec![0u8; 256 * 1024];
        let mut x: u32 = 0x9e37_79b9;
        for b in payload.iter_mut() {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            *b = x as u8;
        }
        dir.write("proj/big.bin", &payload);
        let dest = dir.path("out.zip");
        let source = [s(&dir.path("proj"))];
        for capacity in [0usize, 100, 64 * 1024] {
            let e = create_zip_via(
                &source,
                &dest,
                &limits(),
                ArchiveConflictPolicy::Skip,
                &|file| {
                    Box::new(FullDrive {
                        inner: file,
                        capacity,
                        written: 0,
                    })
                },
            )
            .unwrap_err();
            assert_eq!(e.code, ArchiveErrorCode::Io);
            assert!(
                e.message.starts_with(NO_SPACE),
                "capacity {capacity}: {}",
                e.message
            );
            assert!(!dest.exists(), "capacity {capacity}: nothing published");
            assert!(
                dir.staging_files().is_empty(),
                "capacity {capacity}: {:?}",
                dir.staging_files()
            );
        }
        // The same request with room to spare succeeds: the failure came from the drive, not the input.
        create_zip_with_policy(&source, &dest, &limits(), ArchiveConflictPolicy::Skip).unwrap();
        assert_eq!(members(&dest), vec!["proj/big.bin"]);
    }

    #[test]
    fn out_of_space_is_recognised_from_the_os_error() {
        assert!(is_out_of_space(&std::io::Error::from(
            std::io::ErrorKind::StorageFull
        )));
        if cfg!(windows) {
            assert!(is_out_of_space(&std::io::Error::from_raw_os_error(112)));
            assert!(is_out_of_space(&std::io::Error::from_raw_os_error(39)));
        }
        assert!(!is_out_of_space(&std::io::Error::from(
            std::io::ErrorKind::PermissionDenied
        )));
    }

    #[test]
    fn nested_empty_folders_produce_correct_entries() {
        let dir = Dir::new("nested-empty");
        let empty_leaf = dir.path("proj/a/b/c");
        fs::create_dir_all(&empty_leaf).unwrap();

        let dest_zip = dir.path("out.zip");
        create_zip(&[s(&dir.path("proj"))], &dest_zip, &limits()).unwrap();
        assert_eq!(
            members(&dest_zip),
            vec!["proj/", "proj/a/", "proj/a/b/", "proj/a/b/c/"]
        );
        let report_zip = test_zip(&dest_zip, None).unwrap();
        assert_eq!(report_zip.entries.len(), 4);
        assert!(
            report_zip
                .entries
                .iter()
                .all(|e| e.status == ArchiveTestStatus::Passed)
        );

        let dest_7z = dir.path("out.7z");
        create_7z(&[s(&dir.path("proj"))], &dest_7z).unwrap();
        let report_7z = test_7z(&dest_7z, None).unwrap();
        assert_eq!(report_7z.entries.len(), 4);
        assert!(
            report_7z
                .entries
                .iter()
                .all(|e| e.status == ArchiveTestStatus::Passed)
        );
    }

    #[test]
    fn populated_folders_do_not_produce_duplicate_listing_entries() {
        let dir = Dir::new("no-duplicate");
        dir.write("proj/populated/file.txt", b"data");
        let empty_leaf = dir.path("proj/empty_dir");
        fs::create_dir_all(&empty_leaf).unwrap();

        let dest_zip = dir.path("out.zip");
        create_zip(&[s(&dir.path("proj"))], &dest_zip, &limits()).unwrap();
        // Populated folder must NOT have an explicit directory entry,
        // while the empty folder must.
        assert_eq!(
            members(&dest_zip),
            vec!["proj/empty_dir/", "proj/populated/file.txt"]
        );

        let dest_7z = dir.path("out.7z");
        create_7z(&[s(&dir.path("proj"))], &dest_7z).unwrap();
        let report = test_7z(&dest_7z, None).unwrap();
        assert_eq!(report.entries.len(), 2);
    }
}

#[cfg(test)]
mod staging_cleanup_tests {
    //! FER-510: the sweep of Ferrum's own stale staging entries (dead PID, older than
    //! [`MIN_STAGING_AGE`]) — pattern strictness, age/PID rails, and the no-follow stance.
    use super::*;
    use std::time::SystemTime;

    struct Dir(PathBuf);
    impl Dir {
        fn new(tag: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let dir = std::env::temp_dir().join(format!(
                "ferrum-staging-clean-{tag}-{}-{nanos}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Sets the mtime to twice [`MIN_STAGING_AGE`] ago. Directories need
    /// FILE_FLAG_BACKUP_SEMANTICS to be opened on Windows.
    #[cfg(windows)]
    fn age_old(path: &Path) {
        use std::os::windows::fs::OpenOptionsExt;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true);
        if path.is_dir() {
            opts.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS);
        }
        let f = opts.open(path).unwrap();
        f.set_modified(SystemTime::now() - MIN_STAGING_AGE * 2)
            .unwrap();
    }

    /// A PID that is provably dead right now: a short-lived child that already exited.
    #[cfg(windows)]
    fn a_dead_pid() -> Option<u32> {
        use std::os::windows::process::CommandExt;
        for _ in 0..16 {
            let mut child = std::process::Command::new("cmd")
                .args(["/C", "exit", "0"])
                .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .ok()?;
            let pid = child.id();
            let _ = child.wait();
            if !pid_alive(pid) {
                return Some(pid);
            }
        }
        None
    }

    #[test]
    fn parses_only_exact_ferrum_staging_names() {
        assert_eq!(parse_staging_pid(".out.zip.123-0.ferrum-tmp"), Some(123));
        assert_eq!(
            parse_staging_pid(".a b (2).zip.7-99.ferrum-tmp.rar"),
            Some(7)
        );
        assert_eq!(
            parse_staging_pid(".out.rar.999-123456789012345.ferrum-tmp"),
            Some(999)
        );
        // Lookalikes never parse.
        assert_eq!(parse_staging_pid("out.ferrum-tmp"), None);
        assert_eq!(parse_staging_pid(".out.zip.ferrum-tmp"), None);
        assert_eq!(parse_staging_pid(".out.zip.12-x.ferrum-tmp"), None);
        assert_eq!(parse_staging_pid(".out.zip.12x-0.ferrum-tmp"), None);
        assert_eq!(parse_staging_pid(".out.zip.123-0.ferrum-tmp.txt"), None);
        assert_eq!(parse_staging_pid("ferrum-tmp"), None);
        assert_eq!(parse_staging_pid(""), None);
    }

    #[cfg(windows)]
    #[test]
    fn stale_staging_of_dead_pids_is_removed_files_and_job_folders() {
        let Some(dead) = a_dead_pid() else {
            eprintln!("SKIP: no provably dead PID available");
            return;
        };
        let d = Dir::new("dead");
        let stale_file = d.0.join(format!(".out.zip.{dead}-3.ferrum-tmp"));
        std::fs::write(&stale_file, b"half written").unwrap();
        age_old(&stale_file);
        let stale_job = d.0.join(format!(".out.rar.{dead}-77.ferrum-tmp"));
        std::fs::create_dir_all(&stale_job).unwrap();
        std::fs::write(stale_job.join("archive.rar"), b"partial").unwrap();
        age_old(&stale_job);

        assert_eq!(clean_stale_staging(&d.0.join("out.zip")), 2);
        assert!(!stale_file.exists(), "stale staging file removed");
        assert!(!stale_job.exists(), "stale job folder removed");
    }

    #[cfg(windows)]
    #[test]
    fn staging_of_live_pids_and_young_entries_is_never_touched() {
        let Some(dead) = a_dead_pid() else {
            eprintln!("SKIP: no provably dead PID available");
            return;
        };
        let d = Dir::new("live");
        // Our own (alive) PID, even when old.
        let live =
            d.0.join(format!(".out.zip.{}-1.ferrum-tmp", std::process::id()));
        std::fs::write(&live, b"another create in progress").unwrap();
        age_old(&live);
        // Dead PID, but too young (< MIN_STAGING_AGE): could still belong to a start-up race.
        let young = d.0.join(format!(".out.zip.{dead}-2.ferrum-tmp"));
        std::fs::write(&young, b"fresh leftover").unwrap();

        assert_eq!(clean_stale_staging(&d.0.join("out.zip")), 0);
        assert!(live.exists(), "a live PID's staging is kept");
        assert!(young.exists(), "a young leftover is kept");
    }

    #[cfg(windows)]
    #[test]
    fn lookalike_names_are_never_removed_even_when_old_and_the_pid_is_dead() {
        let Some(dead) = a_dead_pid() else {
            eprintln!("SKIP: no provably dead PID available");
            return;
        };
        let d = Dir::new("lookalike");
        let lookalikes: Vec<String> = vec![
            "out.ferrum-tmp".into(),                   // no leading dot
            ".out.zip.ferrum-tmp".into(),              // no pid-tag
            format!(".out.zip.{dead}-x.ferrum-tmp"),   // non-digit tag
            format!(".notes.{dead}-5.ferrum-tmp.txt"), // wrong suffix
            format!("folder{dead}-5.ferrum-tmp"),      // no leading dot
        ];
        for name in &lookalikes {
            let p = d.0.join(name);
            std::fs::write(&p, b"not ferrum's").unwrap();
            age_old(&p);
        }
        assert_eq!(clean_stale_staging(&d.0.join("out.zip")), 0);
        for name in &lookalikes {
            assert!(d.0.join(name).exists(), "{name} must survive");
        }
    }

    /// A junction named exactly like a staging folder is neither followed nor removed, and its
    /// target is untouched (no-follow, fail-closed).
    #[cfg(windows)]
    #[test]
    fn a_junction_looking_like_staging_is_never_followed_or_removed() {
        let Some(dead) = a_dead_pid() else {
            eprintln!("SKIP: no provably dead PID available");
            return;
        };
        let d = Dir::new("junction");
        let target = d.0.join("precious-target");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("user data.txt"), b"keep me").unwrap();
        let link = d.0.join(format!(".out.zip.{dead}-9.ferrum-tmp"));
        let ok = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&target)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|st| st.success())
            .unwrap_or(false);
        assert!(ok, "mklink /J must succeed without privileges");

        // The junction is young, so the age rail alone would keep it; the point is that an
        // entry matching the staging pattern which IS a reparse point is never followed into
        // its target and never removed.
        assert_eq!(clean_stale_staging(&d.0.join("out.zip")), 0);
        assert!(link.exists(), "the junction itself is not removed");
        assert_eq!(
            std::fs::read_to_string(target.join("user data.txt")).unwrap(),
            "keep me",
            "the junction target is untouched"
        );
    }

    /// Wiring: a ZIP create into the folder sweeps an old dead-PID leftover next to it.
    #[cfg(windows)]
    #[test]
    fn a_zip_create_sweeps_stale_staging_in_the_destination_folder() {
        let Some(dead) = a_dead_pid() else {
            eprintln!("SKIP: no provably dead PID available");
            return;
        };
        let d = Dir::new("wiring");
        let stale = d.0.join(format!(".out.zip.{dead}-42.ferrum-tmp"));
        std::fs::write(&stale, b"from a crashed run").unwrap();
        age_old(&stale);
        let source = d.0.join("doc.txt");
        std::fs::write(&source, b"payload").unwrap();

        let dest = d.0.join("out.zip");
        create_zip_with_policy(
            &[source.to_string_lossy().into_owned()],
            &dest,
            &ArchiveLimits {
                max_entries: 100,
                max_total_bytes: u64::MAX,
                max_entry_bytes: u64::MAX,
                max_depth: 8,
            },
            ArchiveConflictPolicy::Skip,
        )
        .unwrap();
        assert!(dest.is_file(), "the archive was created");
        assert!(
            !stale.exists(),
            "the stale leftover was swept by the create"
        );
    }

    /// The sweep only ever looks at the destination's own folder: leftovers elsewhere stay.
    #[cfg(windows)]
    #[test]
    fn the_sweep_never_touches_any_folder_but_the_destination_folder() {
        let Some(dead) = a_dead_pid() else {
            eprintln!("SKIP: no provably dead PID available");
            return;
        };
        let d = Dir::new("scope");
        let elsewhere = d.0.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        let stranger = elsewhere.join(format!(".out.zip.{dead}-1.ferrum-tmp"));
        std::fs::write(&stranger, b"not this job's folder").unwrap();
        age_old(&stranger);

        // Destination is d.0/out.zip → its folder is d.0; "elsewhere" is never looked at.
        assert_eq!(clean_stale_staging(&d.0.join("out.zip")), 0);
        assert!(stranger.exists(), "another folder is never swept");

        // A destination without a parent folder is simply not swept.
        assert_eq!(clean_stale_staging(Path::new("out.zip")), 0);
        assert!(stranger.exists());
    }
}
