//! Properties / attributes / occupied space / volume info services (F2-21).
//! Kept Tauri-free for unit testing; `ipc.rs` exposes thin commands.
//!
//! Honesty rules: `allocated_bytes` is the real on-disk size on Windows
//! (GetCompressedFileSizeW) and equals the logical size elsewhere (flagged
//! by `allocated_supported = false`). Recursion NEVER follows symlinks or
//! reparse points (decision D07) — they count as entries but are not
//! descended into, which also makes symlink loops impossible.

use crate::domain::{ErrorCode, FsError};
use crate::ops::CancelToken;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

/// Detail sheet for one entry (Alt+Enter).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryProperties {
    pub path: String,
    pub name: String,
    /// `file` | `dir` | `symlink` | `other`
    pub kind: String,
    /// Logical bytes (0 for directories, link targets never read).
    pub size: u64,
    /// On-disk bytes when the OS can report them, else the logical size.
    pub allocated_bytes: u64,
    /// False when `allocated_bytes` is just the logical size (no OS support).
    pub allocated_supported: bool,
    pub created_ms: i64,
    pub modified_ms: i64,
    pub accessed_ms: i64,
    pub read_only: bool,
    pub hidden: bool,
    pub system: bool,
    /// POSIX permission bits (`0o7777`), reported on Unix only. `None` means
    /// "not reported by this platform", never "no permissions".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<u32>,
    /// True when Change Attributes can really write read-only/hidden here
    /// (Windows). Lets the UI disable the toggles up front instead of
    /// failing on Apply.
    pub attributes_editable: bool,
    /// Link target for symlinks/junctions, read without following.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symlink_target: Option<String>,
}

/// Recursive size report (Ctrl+L).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OccupiedReport {
    pub files: u64,
    pub dirs: u64,
    pub symlinks: u64,
    /// Logical bytes.
    pub logical_bytes: u64,
    /// On-disk bytes on Windows; logical elsewhere.
    pub allocated_bytes: u64,
    pub allocated_supported: bool,
    /// Entries that could not be stat'ed/read (e.g. permission denied).
    pub failed: Vec<FailedPath>,
    /// True when the walk was cancelled; the totals above are then partial.
    pub cancelled: bool,
}

/// Running totals emitted while the walk is in progress. Counters only grow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OccupiedProgress {
    pub files: u64,
    pub dirs: u64,
    pub symlinks: u64,
    pub logical_bytes: u64,
}

/// Entries processed between two progress callbacks.
const PROGRESS_EVERY: u64 = 256;

static SCANS: LazyLock<Mutex<HashMap<String, CancelToken>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Registers a cancellable Occupied Space walk under `scan_id`.
pub fn register_scan(scan_id: &str) -> CancelToken {
    let token = CancelToken::default();
    SCANS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(scan_id.to_string(), token.clone());
    token
}

/// Drops the registration once the walk has ended.
pub fn finish_scan(scan_id: &str) {
    SCANS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(scan_id);
}

/// Cancels a running walk; false when `scan_id` is unknown or finished.
pub fn cancel_scan(scan_id: &str) -> bool {
    match SCANS.lock().unwrap_or_else(|e| e.into_inner()).get(scan_id) {
        Some(token) => {
            token.cancel();
            true
        }
        None => false,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FailedPath {
    pub path: String,
    pub code: ErrorCode,
    pub message: String,
}

/// Read-only volume/system facts for the drive holding `path`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VolumeInfo {
    /// Drive root the info was read from, e.g. `C:\`.
    pub root: String,
    /// Volume label, empty when the drive has none.
    pub label: String,
    /// Filesystem name, e.g. `NTFS`.
    pub fs_name: String,
    pub total_bytes: u64,
    pub free_bytes: u64,
}

/// Host facts available from Rust std alone (no extra dependency). The OS
/// *version* is deliberately absent: it needs a platform API we do not link.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostInfo {
    /// `windows` | `linux` | `macos` | …
    pub os: String,
    pub arch: String,
    /// `windows` | `unix`
    pub family: String,
    /// From the environment (`COMPUTERNAME`/`HOSTNAME`); `None` when unset.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_name: Option<String>,
}

pub fn host_info() -> HostInfo {
    let host_name = ["COMPUTERNAME", "HOSTNAME"]
        .iter()
        .find_map(|k| std::env::var(k).ok())
        .filter(|v| !v.is_empty());
    HostInfo {
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        family: std::env::consts::FAMILY.to_string(),
        host_name,
    }
}

fn epoch_ms(t: std::io::Result<std::time::SystemTime>) -> i64 {
    t.ok()
        .and_then(|v| v.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// On-disk size of one file; Windows reports the real allocated size.
/// Shared with the DSK-02 disk-usage scanner so both engines report the
/// same per-file allocation availability.
#[cfg(windows)]
pub(crate) fn allocated_size(path: &Path, logical: u64) -> (u64, bool) {
    use std::os::windows::ffi::OsStrExt;
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut high: u32 = 0;
    let low = unsafe {
        windows_sys::Win32::Storage::FileSystem::GetCompressedFileSizeW(wide.as_ptr(), &mut high)
    };
    if low != u32::MAX {
        return (((high as u64) << 32) | low as u64, true);
    }
    (logical, false)
}

#[cfg(not(windows))]
pub(crate) fn allocated_size(path: &Path, logical: u64) -> (u64, bool) {
    let _ = path;
    (logical, false)
}

/// Properties of one entry, described via lstat (links are never followed).
pub fn entry_properties(path: &str) -> Result<EntryProperties, FsError> {
    let p = Path::new(path);
    let lm = fs::symlink_metadata(p).map_err(|e| FsError::from(e).with_path(path))?;
    let is_symlink = lm.is_symlink();
    let is_dir = lm.is_dir();
    let kind = if is_symlink {
        "symlink"
    } else if is_dir {
        "dir"
    } else if lm.is_file() {
        "file"
    } else {
        "other"
    };
    let logical = if is_dir { 0 } else { lm.len() };
    let (allocated, supported) = if is_dir || is_symlink {
        (0, false)
    } else {
        allocated_size(p, logical)
    };
    let mut attrs = AttrFlags::default();
    read_attrs(p, &lm, &mut attrs);
    let symlink_target = if is_symlink {
        fs::read_link(p)
            .ok()
            .map(|t| t.to_string_lossy().into_owned())
    } else {
        None
    };
    Ok(EntryProperties {
        path: path.to_string(),
        name: p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string()),
        kind: kind.to_string(),
        size: logical,
        allocated_bytes: allocated,
        allocated_supported: supported,
        created_ms: epoch_ms(lm.created()),
        modified_ms: epoch_ms(lm.modified()),
        accessed_ms: epoch_ms(lm.accessed()),
        read_only: attrs.read_only,
        hidden: attrs.hidden,
        system: attrs.system,
        mode: read_mode(&lm),
        attributes_editable: cfg!(windows),
        symlink_target,
    })
}

#[derive(Default)]
struct AttrFlags {
    read_only: bool,
    hidden: bool,
    system: bool,
}

#[cfg(windows)]
fn read_attrs(path: &Path, lm: &fs::Metadata, out: &mut AttrFlags) {
    use std::os::windows::fs::MetadataExt;
    let a = lm.file_attributes();
    out.read_only = a & 0x1 != 0; // FILE_ATTRIBUTE_READONLY
    out.hidden = a & 0x2 != 0; // FILE_ATTRIBUTE_HIDDEN
    out.system = a & 0x4 != 0; // FILE_ATTRIBUTE_SYSTEM
    let _ = path;
}

/// Non-Windows: read-only from the write bits, hidden by the dot-prefix
/// convention; there is no system attribute.
#[cfg(not(windows))]
fn read_attrs(path: &Path, lm: &fs::Metadata, out: &mut AttrFlags) {
    out.read_only = lm.permissions().readonly();
    out.hidden = path
        .file_name()
        .is_some_and(|n| n.to_string_lossy().starts_with('.'));
}

#[cfg(unix)]
fn read_mode(lm: &fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    Some(lm.permissions().mode() & 0o7777)
}

#[cfg(not(unix))]
fn read_mode(lm: &fs::Metadata) -> Option<u32> {
    let _ = lm;
    None
}

/// Recursive logical + allocated size without progress or cancel.
pub fn occupied_space(paths: &[String]) -> OccupiedReport {
    occupied_space_with(paths, &CancelToken::default(), &mut |_| {})
}

/// Recursive logical + allocated size. Iterative (explicit stack) so deep
/// trees cannot overflow the thread; per-item failures are reported and the
/// walk continues. Symlinks are counted but never descended (D07).
/// `cancel` is polled between entries; a cancelled walk returns the partial
/// totals with `cancelled = true`. `on_progress` fires every
/// [`PROGRESS_EVERY`] entries with monotonically growing counters.
pub fn occupied_space_with(
    paths: &[String],
    cancel: &CancelToken,
    on_progress: &mut dyn FnMut(&OccupiedProgress),
) -> OccupiedReport {
    let mut report = OccupiedReport {
        files: 0,
        dirs: 0,
        symlinks: 0,
        logical_bytes: 0,
        allocated_bytes: 0,
        allocated_supported: true,
        failed: Vec::new(),
        cancelled: false,
    };
    let mut seen: u64 = 0;
    let mut stack: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
    while let Some(current) = stack.pop() {
        if cancel.is_cancelled() {
            report.cancelled = true;
            break;
        }
        seen += 1;
        if seen.is_multiple_of(PROGRESS_EVERY) {
            on_progress(&OccupiedProgress {
                files: report.files,
                dirs: report.dirs,
                symlinks: report.symlinks,
                logical_bytes: report.logical_bytes,
            });
        }
        let Ok(lm) = fs::symlink_metadata(&current) else {
            report.failed.push(FailedPath {
                path: current.to_string_lossy().into_owned(),
                code: ErrorCode::NotFound,
                message: "Entry not found".into(),
            });
            continue;
        };
        if lm.is_symlink() {
            report.symlinks += 1;
            continue; // never follow (D07); loops become impossible
        }
        if lm.is_dir() {
            report.dirs += 1;
            match fs::read_dir(&current) {
                Ok(read) => {
                    for item in read {
                        match item {
                            Ok(entry) => stack.push(entry.path()),
                            Err(e) => report.failed.push(FailedPath {
                                path: current.to_string_lossy().into_owned(),
                                code: FsError::from(e).code,
                                message: "Directory entry unreadable".into(),
                            }),
                        }
                    }
                }
                Err(e) => {
                    let fe = FsError::from(e);
                    report.failed.push(FailedPath {
                        path: current.to_string_lossy().into_owned(),
                        code: fe.code,
                        message: fe.message,
                    });
                }
            }
            continue;
        }
        report.files += 1;
        let logical = lm.len();
        report.logical_bytes += logical;
        let (allocated, supported) = allocated_size(&current, logical);
        report.allocated_bytes += allocated;
        report.allocated_supported &= supported;
    }
    report
}

/// Volume label of the drive holding `path`.
pub fn volume_label_of(path: &str) -> Result<String, FsError> {
    volume_info(path).map(|v| v.label)
}

/// Read-only volume facts (Windows). Other platforms report Unsupported
/// rather than fabricating values.
pub fn volume_info(path: &str) -> Result<VolumeInfo, FsError> {
    let root = drive_root_of(path).ok_or_else(|| {
        FsError::new(
            ErrorCode::Unsupported,
            "Volume information is not supported for this path on this platform \
             (only local drive-letter volumes are inspected; UNC and POSIX roots are not)",
        )
        .with_path(path)
    })?;
    #[cfg(windows)]
    {
        volume_info_windows(&root)
    }
    #[cfg(not(windows))]
    {
        let _ = root;
        Err(FsError::new(
            ErrorCode::Unsupported,
            "Volume information is not supported on this platform",
        ))
    }
}

/// `C:\dir\file.txt` → `C:\`; UNC paths and POSIX roots report nothing.
fn drive_root_of(path: &str) -> Option<String> {
    let bytes = path.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        return Some(format!("{}:\\", bytes[0] as char));
    }
    None
}

#[cfg(windows)]
fn volume_info_windows(root: &str) -> Result<VolumeInfo, FsError> {
    use windows_sys::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetVolumeInformationW};
    let wide: Vec<u16> = root.encode_utf16().chain(std::iter::once(0)).collect();
    let mut label_buf = [0u16; 261];
    let mut fs_buf = [0u16; 261];
    let label_ok = unsafe {
        GetVolumeInformationW(
            wide.as_ptr(),
            label_buf.as_mut_ptr(),
            label_buf.len() as u32,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            fs_buf.as_mut_ptr(),
            fs_buf.len() as u32,
        )
    };
    let from_buf = |buf: &[u16]| -> String {
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..end])
    };
    let mut free: u64 = 0;
    let mut total: u64 = 0;
    let space_ok = unsafe {
        GetDiskFreeSpaceExW(wide.as_ptr(), std::ptr::null_mut(), &mut total, &mut free) != 0
    };
    if label_ok == 0 && !space_ok {
        return Err(FsError::new(ErrorCode::NotFound, "Volume cannot be queried").with_path(root));
    }
    Ok(VolumeInfo {
        root: root.to_string(),
        label: if label_ok != 0 {
            from_buf(&label_buf)
        } else {
            String::new()
        },
        fs_name: if label_ok != 0 {
            from_buf(&fs_buf)
        } else {
            String::new()
        },
        total_bytes: if space_ok { total } else { 0 },
        free_bytes: if space_ok { free } else { 0 },
    })
}

/// Toggle READONLY / HIDDEN on Windows. `None` leaves the attribute as-is;
/// the OTHER known attributes are always preserved. Unsupported elsewhere.
pub fn set_attributes(
    path: &str,
    read_only: Option<bool>,
    hidden: Option<bool>,
) -> Result<(), FsError> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_ATTRIBUTE_DIRECTORY, GetFileAttributesW, SetFileAttributesW,
        };
        let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
        let current = unsafe { GetFileAttributesW(wide.as_ptr()) };
        if current == u32::MAX {
            return Err(FsError::new(ErrorCode::NotFound, "Entry not found").with_path(path));
        }
        let mut attrs = current;
        if let Some(v) = read_only {
            attrs = if v { attrs | 0x1 } else { attrs & !0x1 };
        }
        if let Some(v) = hidden {
            attrs = if v { attrs | 0x2 } else { attrs & !0x2 };
        }
        // Keep directory-ness explicit so the entry never loses its type.
        if Path::new(path).is_dir() {
            attrs |= FILE_ATTRIBUTE_DIRECTORY;
        }
        let ok = unsafe { SetFileAttributesW(wide.as_ptr(), attrs) };
        if ok == 0 {
            return Err(FsError::new(
                ErrorCode::PermissionDenied,
                "Attributes could not be changed",
            )
            .with_path(path));
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = (path, read_only, hidden);
        Err(FsError::new(
            ErrorCode::Unsupported,
            "Attribute changes are not supported on this platform",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQ: AtomicU64 = AtomicU64::new(0);

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ferrum-f221-{tag}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn s(p: &Path) -> String {
        p.to_string_lossy().into_owned()
    }

    /// Creates a directory symlink, or reports an explicit skip. Never lets a
    /// test pass silently when the environment cannot create links; set
    /// `FERRUM_REQUIRE_SYMLINKS=1` (CI) to turn the skip into a failure.
    fn try_symlink_dir(target: &Path, link: &Path) -> bool {
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_dir(target, link);
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(target, link);
        match made {
            Ok(()) => true,
            Err(e) => {
                assert!(
                    std::env::var_os("FERRUM_REQUIRE_SYMLINKS").is_none(),
                    "symlink creation required but unavailable: {e}"
                );
                eprintln!("SKIPPED: symlink creation unavailable here ({e})");
                false
            }
        }
    }

    #[test]
    fn file_properties_report_size_kind_and_times() {
        let root = temp("file");
        fs::write(root.join("notes.txt"), b"hello ferrum").unwrap();
        let p = entry_properties(&s(&root.join("notes.txt"))).unwrap();
        assert_eq!(p.kind, "file");
        assert_eq!(p.size, 12);
        assert_eq!(p.name, "notes.txt");
        assert!(p.modified_ms > 0);
        assert!(!p.read_only);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn dir_properties_have_zero_logical_size() {
        let root = temp("dir");
        let p = entry_properties(&s(&root)).unwrap();
        assert_eq!(p.kind, "dir");
        assert_eq!(p.size, 0);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn symlink_properties_show_target_without_following() {
        let root = temp("link");
        fs::create_dir(root.join("real")).unwrap();
        let link = root.join("link");
        if !try_symlink_dir(&root.join("real"), &link) {
            let _ = fs::remove_dir_all(&root);
            return;
        }
        let p = entry_properties(&s(&link)).unwrap();
        assert_eq!(p.kind, "symlink");
        assert!(p.symlink_target.is_some());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_path_maps_to_not_found() {
        let err = entry_properties(r"Z:\nope\gone.txt").unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);
    }

    #[test]
    fn occupied_space_walks_trees_and_reports_logical_and_allocated() {
        let root = temp("occ");
        fs::write(root.join("a.bin"), vec![0u8; 1000]).unwrap();
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("sub").join("b.bin"), vec![0u8; 500]).unwrap();
        let report = occupied_space(&[s(&root)]);
        assert_eq!(report.files, 2);
        assert_eq!(report.dirs, 2); // root + sub
        assert_eq!(report.symlinks, 0);
        assert_eq!(report.logical_bytes, 1500);
        assert!(report.allocated_bytes >= report.logical_bytes);
        assert!(report.failed.is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn occupied_space_never_follows_symlinks_so_loops_cannot_explode() {
        let root = temp("loop");
        fs::write(root.join("f.txt"), b"x").unwrap();
        let inner = root.join("inner");
        fs::create_dir(&inner).unwrap();
        if !try_symlink_dir(&root, &inner.join("up")) {
            let _ = fs::remove_dir_all(&root);
            return;
        }
        let report = occupied_space(&[s(&root)]);
        // The link is counted as an entry; the walk does NOT re-enter root.
        assert_eq!(report.symlinks, 1);
        assert_eq!(report.files, 1);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn occupied_space_reports_failures_and_keeps_going() {
        let root = temp("fail");
        fs::write(root.join("good.txt"), b"1").unwrap();
        let report = occupied_space(&[s(&root.join("missing.txt")), s(&root)]);
        assert_eq!(report.files, 1);
        assert_eq!(report.failed.len(), 1);
        assert_eq!(report.failed[0].code, ErrorCode::NotFound);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn multiple_roots_aggregate_into_one_report() {
        let a = temp("agg-a");
        let b = temp("agg-b");
        fs::write(a.join("x"), b"12").unwrap();
        fs::write(b.join("y"), b"345").unwrap();
        let report = occupied_space(&[s(&a), s(&b)]);
        assert_eq!(report.files, 2);
        assert_eq!(report.logical_bytes, 5);
        let _ = fs::remove_dir_all(&a);
        let _ = fs::remove_dir_all(&b);
    }

    #[cfg(windows)]
    #[test]
    fn volume_info_reads_the_system_drive_on_host() {
        let info = volume_info(r"C:\Windows").expect("C: volume info");
        assert_eq!(info.root, r"C:\");
        assert!(info.total_bytes > 0);
        assert!(info.free_bytes <= info.total_bytes);
        assert!(!info.fs_name.is_empty());
    }

    #[test]
    fn volume_info_reports_unsupported_for_paths_without_a_drive_root() {
        for path in ["/home/arif", r"\\server\share\dir"] {
            let err = volume_info(path).unwrap_err();
            assert_eq!(err.code, ErrorCode::Unsupported, "{path}");
            assert!(err.message.contains("not supported"), "{}", err.message);
        }
    }

    #[cfg(windows)]
    #[test]
    fn volume_info_reads_another_local_drive_when_one_exists() {
        let other = ('D'..='Z')
            .map(|c| format!("{c}:\\"))
            .find(|root| Path::new(root).exists());
        let Some(root) = other else {
            eprintln!("SKIPPED: no second drive letter on this host (UNC case covered above)");
            return;
        };
        let info = volume_info(&root).expect("second drive volume info");
        assert_eq!(info.root, root);
        assert!(info.free_bytes <= info.total_bytes);
    }

    #[test]
    fn host_info_reports_std_constants() {
        let h = host_info();
        assert_eq!(h.os, std::env::consts::OS);
        assert_eq!(h.family, if cfg!(windows) { "windows" } else { "unix" });
        assert!(!h.arch.is_empty());
    }

    #[test]
    fn attributes_editable_matches_platform_capability() {
        let root = temp("editable");
        let p = entry_properties(&s(&root)).unwrap();
        assert_eq!(p.attributes_editable, cfg!(windows));
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(not(windows))]
    #[test]
    fn set_attributes_is_unsupported_off_windows() {
        let err = set_attributes("/tmp/x", Some(true), None).unwrap_err();
        assert_eq!(err.code, ErrorCode::Unsupported);
    }

    #[cfg(unix)]
    #[test]
    fn posix_mode_bits_are_reported_on_unix() {
        use std::os::unix::fs::PermissionsExt;
        let root = temp("mode");
        let file = root.join("script.sh");
        fs::write(&file, b"#!/bin/sh").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o640)).unwrap();
        let p = entry_properties(&s(&file)).unwrap();
        assert_eq!(p.mode, Some(0o640));
        assert!(!p.read_only);
        fs::set_permissions(&file, fs::Permissions::from_mode(0o444)).unwrap();
        assert!(entry_properties(&s(&file)).unwrap().read_only);
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn dot_files_report_hidden_on_unix() {
        let root = temp("dot");
        fs::write(root.join(".secret"), b"x").unwrap();
        assert!(entry_properties(&s(&root.join(".secret"))).unwrap().hidden);
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(not(unix))]
    #[test]
    fn posix_mode_is_honestly_not_reported_off_unix() {
        let root = temp("nomode");
        fs::write(root.join("a.txt"), b"x").unwrap();
        assert_eq!(
            entry_properties(&s(&root.join("a.txt"))).unwrap().mode,
            None
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(windows)]
    #[test]
    fn hidden_and_system_flags_are_read_from_file_attributes() {
        use windows_sys::Win32::Storage::FileSystem::SetFileAttributesW;
        let root = temp("hs");
        let file = root.join("pagefile.sys");
        fs::write(&file, b"x").unwrap();
        let wide: Vec<u16> = file
            .as_os_str()
            .to_string_lossy()
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        // HIDDEN | SYSTEM | ARCHIVE
        assert_ne!(
            unsafe { SetFileAttributesW(wide.as_ptr(), 0x2 | 0x4 | 0x20) },
            0
        );
        let p = entry_properties(&s(&file)).unwrap();
        assert!(p.hidden && p.system && !p.read_only);
        // set_attributes must preserve the SYSTEM bit it does not manage.
        set_attributes(&s(&file), Some(true), Some(false)).unwrap();
        let p = entry_properties(&s(&file)).unwrap();
        assert!(p.read_only && !p.hidden && p.system);
        set_attributes(&s(&file), Some(false), None).unwrap();
        unsafe { SetFileAttributesW(wide.as_ptr(), 0x20) };
        let _ = fs::remove_dir_all(&root);
    }

    fn many_files(root: &Path, count: usize) {
        for i in 0..count {
            let dir = root
                .join(format!("d{}", i % 10))
                .join(format!("e{}", i % 7));
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join(format!("f{i}.bin")), [0u8; 10]).unwrap();
        }
    }

    #[test]
    fn large_nested_tree_totals_are_exact_and_bounded() {
        let root = temp("large");
        many_files(&root, 6000);
        let started = std::time::Instant::now();
        let report = occupied_space(&[s(&root)]);
        let elapsed = started.elapsed();
        assert_eq!(report.files, 6000);
        assert_eq!(report.dirs, 1 + 10 + 70); // root + d0..d9 + 70 e*
        assert_eq!(report.logical_bytes, 60_000);
        assert!(!report.cancelled && report.failed.is_empty());
        assert!(
            elapsed < std::time::Duration::from_secs(20),
            "walk took {elapsed:?}"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn progress_is_monotonic_and_never_exceeds_the_final_report() {
        let root = temp("progress");
        many_files(&root, 1500);
        let mut seen: Vec<OccupiedProgress> = Vec::new();
        let report =
            occupied_space_with(&[s(&root)], &CancelToken::default(), &mut |p| seen.push(*p));
        assert!(
            seen.len() >= 5,
            "expected several progress ticks, got {}",
            seen.len()
        );
        for pair in seen.windows(2) {
            assert!(pair[1].files >= pair[0].files);
            assert!(pair[1].dirs >= pair[0].dirs);
            assert!(pair[1].logical_bytes >= pair[0].logical_bytes);
        }
        let last = seen.last().unwrap();
        assert!(last.files <= report.files && last.logical_bytes <= report.logical_bytes);
        assert_eq!(report.files, 1500);
        assert!(!report.cancelled);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn cancel_mid_walk_ends_cleanly_with_a_partial_report() {
        let root = temp("cancel");
        many_files(&root, 3000);
        let token = CancelToken::default();
        let mut ticks = 0;
        let report = occupied_space_with(&[s(&root)], &token, &mut |_| {
            ticks += 1;
            if ticks == 2 {
                token.cancel();
            }
        });
        assert!(report.cancelled);
        assert_eq!(ticks, 2, "no progress after cancellation");
        assert!(
            report.files < 3000,
            "walk must stop early, saw {}",
            report.files
        );
        assert!(report.files > 0);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn already_cancelled_token_does_no_work() {
        let root = temp("precancel");
        fs::write(root.join("a"), b"x").unwrap();
        let token = CancelToken::default();
        token.cancel();
        let report = occupied_space_with(&[s(&root)], &token, &mut |_| {});
        assert!(report.cancelled);
        assert_eq!((report.files, report.dirs), (0, 0));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn scan_registry_cancels_by_id_and_forgets_finished_scans() {
        let token = register_scan("scan-reg-test");
        assert!(!token.is_cancelled());
        assert!(cancel_scan("scan-reg-test"));
        assert!(token.is_cancelled());
        finish_scan("scan-reg-test");
        assert!(!cancel_scan("scan-reg-test"));
        assert!(!cancel_scan("never-registered"));
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_directory_is_reported_and_the_walk_continues() {
        use std::os::unix::fs::PermissionsExt;
        let root = temp("denied");
        fs::write(root.join("ok.txt"), b"1").unwrap();
        let locked = root.join("locked");
        fs::create_dir(&locked).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::read_dir(&locked).is_ok() {
            eprintln!("SKIPPED: running with privileges that bypass directory modes");
        } else {
            let report = occupied_space(&[s(&root)]);
            assert_eq!(report.files, 1);
            assert_eq!(report.failed.len(), 1);
            assert_eq!(report.failed[0].code, ErrorCode::PermissionDenied);
        }
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        let _ = fs::remove_dir_all(&root);
    }

    /// Windows: deny read on a directory via an ACL (icacls) so listing it
    /// yields access-denied. Skips explicitly if icacls cannot be applied.
    #[cfg(windows)]
    #[test]
    fn acl_denied_directory_is_reported_and_the_walk_continues() {
        use std::process::Command;
        let root = temp("denied");
        fs::write(root.join("ok.txt"), b"1").unwrap();
        let locked = root.join("locked");
        fs::create_dir(&locked).unwrap();
        let deny = Command::new("icacls")
            .arg(&locked)
            .args(["/deny", "*S-1-1-0:(RD)"])
            .output();
        let applied = deny.map(|o| o.status.success()).unwrap_or(false);
        if !applied || fs::read_dir(&locked).is_ok() {
            eprintln!("SKIPPED: could not deny read via icacls on this host");
        } else {
            let report = occupied_space(&[s(&root)]);
            assert_eq!(report.files, 1);
            assert_eq!(report.failed.len(), 1);
            assert_eq!(report.failed[0].code, ErrorCode::PermissionDenied);
        }
        let _ = Command::new("icacls")
            .arg(&locked)
            .args(["/remove:d", "*S-1-1-0"])
            .output();
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(windows)]
    #[test]
    fn set_attributes_round_trips_read_only_and_hidden() {
        let root = temp("attrs");
        let file = root.join("doc.txt");
        fs::write(&file, b"x").unwrap();
        let sfile = s(&file);
        set_attributes(&sfile, Some(true), Some(true)).unwrap();
        let p = entry_properties(&sfile).unwrap();
        assert!(p.read_only && p.hidden);
        set_attributes(&sfile, Some(false), Some(false)).unwrap();
        let p = entry_properties(&sfile).unwrap();
        assert!(!p.read_only && !p.hidden);
        // Partial toggle leaves the other attribute alone.
        set_attributes(&sfile, Some(true), None).unwrap();
        let p = entry_properties(&sfile).unwrap();
        assert!(p.read_only && !p.hidden);
        set_attributes(&sfile, Some(false), None).unwrap();
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(windows)]
    #[test]
    fn set_attributes_on_missing_entry_is_not_found() {
        let err = set_attributes(r"Z:\gone.txt", Some(true), None).unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);
    }
}
