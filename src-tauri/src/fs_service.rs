//! Filesystem read services. Kept free of Tauri types so they stay
//! unit-testable; `ipc.rs` is the only layer that touches Tauri.

use crate::domain::{FileEntry, FsError};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// One logical drive, shown as a tree-sidebar root. Windows-first (D01);
/// other platforms enumerate nothing yet rather than guessing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DriveInfo {
    /// Volume label, empty when the drive has none.
    pub label: String,
    /// Root path with trailing separator, e.g. `C:\`.
    pub path: String,
    pub free_bytes: u64,
    pub total_bytes: u64,
    /// `fixed` | `removable` | `network` | `optical` | `ramdisk` | `unknown`.
    #[serde(default = "default_drive_type")]
    pub drive_type: String,
    /// File system name (`NTFS`, `exFAT`, ...); empty when unknown.
    #[serde(default)]
    pub fs_name: String,
    /// Volume serial as `XXXX-XXXX`; empty when unknown.
    #[serde(default)]
    pub serial: String,
    /// `ok` | `noMedia` (empty card reader / optical slot) | `unavailable`
    /// (disconnected share, locked volume, or the query timed out).
    #[serde(default = "default_drive_status")]
    pub status: String,
}

fn default_drive_type() -> String {
    "unknown".into()
}

fn default_drive_status() -> String {
    "ok".into()
}

/// Enumerate logical drives. Never fails and never blocks longer than the
/// per-call deadline: every drive is queried on its own worker thread, so a
/// disconnected network share cannot freeze the caller (it is reported as
/// `unavailable`). Empty removable/optical slots are reported as `noMedia`.
pub fn list_drives() -> Vec<DriveInfo> {
    #[cfg(windows)]
    {
        drives::list_drives_windows()
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

/// Bitmask of present drive letters (bit 0 = A:). Instant (no media I/O), so
/// the frontend can poll it cheaply to notice drives added or removed.
pub fn drive_letters_mask() -> u32 {
    #[cfg(windows)]
    {
        drives::letters_mask()
    }
    #[cfg(not(windows))]
    {
        0
    }
}

#[cfg(windows)]
mod drives {
    use super::DriveInfo;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};
    use windows_sys::Win32::Storage::FileSystem::{
        GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
    };
    use windows_sys::Win32::System::Diagnostics::Debug::{
        SEM_FAILCRITICALERRORS, SetThreadErrorMode,
    };
    use windows_sys::Win32::System::WindowsProgramming::{
        DRIVE_CDROM, DRIVE_FIXED, DRIVE_RAMDISK, DRIVE_REMOTE, DRIVE_REMOVABLE,
    };

    /// Total wall-clock budget for one enumeration.
    const DEADLINE: Duration = Duration::from_millis(1500);

    pub(super) fn letters_mask() -> u32 {
        unsafe { GetLogicalDrives() }
    }

    pub(super) fn list_drives_windows() -> Vec<DriveInfo> {
        let mask = letters_mask();
        if mask == 0 {
            return Vec::new();
        }
        let (tx, rx) = mpsc::channel::<(usize, DriveInfo)>();
        let mut pending: Vec<(u8, &'static str)> = Vec::new();
        for letter in letters_from_mask(mask) {
            let root_w = root_wide(letter);
            // GetDriveTypeW does no media I/O, safe on the caller thread.
            let kind = type_name(unsafe { GetDriveTypeW(root_w.as_ptr()) });
            if kind == "skip" {
                continue;
            }
            let idx = pending.len();
            pending.push((letter, kind));
            let tx = tx.clone();
            std::thread::spawn(move || {
                let _ = tx.send((idx, query_drive(letter, kind)));
            });
        }
        drop(tx);
        let mut results: Vec<Option<DriveInfo>> = vec![None; pending.len()];
        let start = Instant::now();
        let mut got = 0;
        while got < pending.len() {
            let left = DEADLINE.saturating_sub(start.elapsed());
            match rx.recv_timeout(left) {
                Ok((i, info)) => {
                    results[i] = Some(info);
                    got += 1;
                }
                Err(_) => break,
            }
        }
        pending
            .into_iter()
            .zip(results)
            .map(|((letter, kind), info)| {
                // A worker still stuck in the kernel is abandoned; the drive
                // is shown as unavailable instead of blocking the caller.
                info.unwrap_or_else(|| unavailable(letter, kind))
            })
            .collect()
    }

    /// Bit i of `mask` = drive letter (b'A' + i) is present.
    pub(super) fn letters_from_mask(mask: u32) -> Vec<u8> {
        (0..26)
            .filter(|i| mask & (1 << i) != 0)
            .map(|i| b'A' + i as u8)
            .collect()
    }

    pub(super) fn drive_root(letter: u8) -> String {
        format!("{}:\\", letter as char)
    }

    fn root_wide(letter: u8) -> Vec<u16> {
        drive_root(letter)
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect()
    }

    fn type_name(t: u32) -> &'static str {
        match t {
            DRIVE_FIXED => "fixed",
            DRIVE_REMOVABLE => "removable",
            DRIVE_REMOTE => "network",
            DRIVE_CDROM => "optical",
            DRIVE_RAMDISK => "ramdisk",
            0 | 1 => "skip", // unknown / no root dir
            _ => "unknown",
        }
    }

    fn unavailable(letter: u8, kind: &str) -> DriveInfo {
        DriveInfo {
            label: String::new(),
            path: drive_root(letter),
            free_bytes: 0,
            total_bytes: 0,
            drive_type: kind.into(),
            fs_name: String::new(),
            serial: String::new(),
            status: "unavailable".into(),
        }
    }

    fn query_drive(letter: u8, kind: &'static str) -> DriveInfo {
        // No "insert a disk" system dialogs for empty slots on this thread.
        unsafe {
            SetThreadErrorMode(SEM_FAILCRITICALERRORS, std::ptr::null_mut());
        }
        let root_w = root_wide(letter);
        let mut free: u64 = 0;
        let mut total: u64 = 0;
        let stats_ok = unsafe {
            GetDiskFreeSpaceExW(root_w.as_ptr(), std::ptr::null_mut(), &mut total, &mut free) != 0
        };
        let mut info = unavailable(letter, kind);
        if let Some((label, fs_name, serial)) = volume_details(&root_w) {
            info.label = label;
            info.fs_name = fs_name;
            info.serial = serial;
        }
        if stats_ok {
            info.free_bytes = free;
            info.total_bytes = total;
            info.status = "ok".into();
        } else if kind == "removable" || kind == "optical" {
            info.status = "noMedia".into();
        }
        info
    }

    fn volume_details(root_w: &[u16]) -> Option<(String, String, String)> {
        let mut label = [0u16; 261];
        let mut fs = [0u16; 261];
        let mut serial: u32 = 0;
        let ok = unsafe {
            GetVolumeInformationW(
                root_w.as_ptr(),
                label.as_mut_ptr(),
                label.len() as u32,
                &mut serial,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                fs.as_mut_ptr(),
                fs.len() as u32,
            )
        };
        if ok == 0 {
            return None;
        }
        let s = |b: &[u16]| {
            let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
            String::from_utf16_lossy(&b[..end])
        };
        Some((
            s(&label),
            s(&fs),
            format!("{:04X}-{:04X}", serial >> 16, serial & 0xFFFF),
        ))
    }
}

/// List one directory level (never recursive; symlinks are reported as
/// entries but never followed — decision D07).
///
/// Default order: folders first, then case-insensitive name ascending
/// (decision D13). Entries that cannot be stat'ed are skipped rather than
/// failing the whole listing — a file manager must show what it can read.
pub fn list_dir(path: &str) -> Result<Vec<FileEntry>, FsError> {
    let dir = Path::new(path);
    // symlink_metadata so the listing of a symlinked dir errors rather than
    // silently following into the target's parent-level metadata.
    let meta = fs::metadata(dir).map_err(|e| FsError::from(e).with_path(path))?;
    if !meta.is_dir() {
        return Err(FsError::new(
            crate::domain::ErrorCode::NotADirectory,
            "Path is a file, not a directory",
        )
        .with_path(path));
    }

    let mut entries: Vec<FileEntry> = Vec::new();
    let read = fs::read_dir(dir).map_err(|e| FsError::from(e).with_path(path))?;
    for item in read {
        let Ok(item) = item else {
            continue; // iterator-level failure: skip, keep the listing alive
        };
        let Ok(file_type) = item.file_type() else {
            continue;
        };
        // lstat semantics: describe the entry itself, not the link target.
        let Ok(lm) = fs::symlink_metadata(item.path()) else {
            continue;
        };
        let is_symlink = file_type.is_symlink();
        let is_dir = lm.is_dir(); // for symlinks this is false: link ≠ target
        let name = item.file_name().to_string_lossy().into_owned();
        entries.push(FileEntry {
            hidden: is_hidden(&name, &lm),
            name,
            path: path_buf_to_string(item.path()),
            is_dir,
            is_symlink,
            size: if is_dir { 0 } else { lm.len() },
            modified_ms: modified_ms(&lm),
        });
    }
    sort_default(&mut entries);
    Ok(entries)
}

/// Default presentation order: folders, then files; within groups, name
/// ascending case-insensitively (numeric-aware ordering arrives with task 08).
fn sort_default(entries: &mut [FileEntry]) {
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
}

fn path_buf_to_string(p: PathBuf) -> String {
    p.to_string_lossy().into_owned()
}

/// Crate-visible so `search.rs` builds identical FileEntry metadata.
pub(crate) fn modified_ms(meta: &fs::Metadata) -> i64 {
    use std::time::UNIX_EPOCH;
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Hidden = dot-prefix (Unix convention) or the Windows hidden attribute.
/// Uses already-fetched lstat metadata; no extra syscall.
/// Crate-visible so `search.rs` builds identical FileEntry metadata.
pub(crate) fn is_hidden(name: &str, lm: &fs::Metadata) -> bool {
    if name.starts_with('.') {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        lm.file_attributes() & 0x2 != 0 // FILE_ATTRIBUTE_HIDDEN
    }
    #[cfg(not(windows))]
    {
        let _ = lm;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQ: AtomicU64 = AtomicU64::new(0);

    /// Owned temp dir under %TEMP%, removed on Drop (also on assertion failure or early return).
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "ferrum-t04-{tag}-{}-{}",
                std::process::id(),
                SEQ.fetch_add(1, Ordering::SeqCst)
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).expect("temp root");
            TempDir(dir)
        }

        #[allow(dead_code)]
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl std::ops::Deref for TempDir {
        type Target = Path;
        fn deref(&self) -> &Path {
            &self.0
        }
    }

    impl AsRef<Path> for TempDir {
        fn as_ref(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn temp_root(tag: &str) -> TempDir {
        TempDir::new(tag)
    }

    fn names(listing: &[FileEntry]) -> Vec<&str> {
        listing.iter().map(|e| e.name.as_str()).collect()
    }

    #[test]
    fn lists_real_directory_with_unicode_names() {
        let root = temp_root("unicode");
        fs::write(root.join("békér.txt"), b"data").unwrap();
        fs::create_dir(root.join("dossier ünicode")).unwrap();
        let listing = list_dir(root.to_str().unwrap()).unwrap();
        let n = names(&listing);
        assert!(n.contains(&"békér.txt"));
        assert!(n.contains(&"dossier ünicode"));
        let dir_entry = listing
            .iter()
            .find(|e| e.name == "dossier ünicode")
            .unwrap();
        assert!(dir_entry.is_dir);
        assert_eq!(dir_entry.size, 0);
        let file_entry = listing.iter().find(|e| e.name == "békér.txt").unwrap();
        assert_eq!(file_entry.size, 4);
        assert!(!file_entry.is_dir);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn folders_sort_first_then_name_case_insensitive() {
        let root = temp_root("sort");
        fs::create_dir(root.join("Zebra")).unwrap();
        fs::write(root.join("apple.txt"), b"").unwrap();
        fs::create_dir(root.join("banana")).unwrap();
        fs::write(root.join("Cherry.txt"), b"").unwrap();
        let listing = list_dir(root.to_str().unwrap()).unwrap();
        let n = names(&listing);
        assert_eq!(n, vec!["banana", "Zebra", "apple.txt", "Cherry.txt"]);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn dot_prefix_is_hidden() {
        let root = temp_root("hidden");
        fs::write(root.join(".secret"), b"").unwrap();
        fs::write(root.join("open.txt"), b"").unwrap();
        let listing = list_dir(root.to_str().unwrap()).unwrap();
        let secret = listing.iter().find(|e| e.name == ".secret").unwrap();
        let open = listing.iter().find(|e| e.name == "open.txt").unwrap();
        assert!(secret.hidden);
        assert!(!open.hidden);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn symlink_reported_not_followed() {
        let root = temp_root("symlink");
        let target = root.join("target-dir");
        fs::create_dir(&target).unwrap();
        let link = root.join("link-to-target");
        #[cfg(windows)]
        {
            // Create via std (developer mode allows unprivileged symlinks);
            // if the environment refuses, skip the test rather than fail.
            let made = std::os::windows::fs::symlink_dir(&target, &link);
            if made.is_err() {
                return;
            }
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let listing = list_dir(root.to_str().unwrap()).unwrap();
        let l = listing.iter().find(|e| e.name == "link-to-target").unwrap();
        assert!(l.is_symlink);
        assert!(
            !l.is_dir,
            "symlink must not be reported as a real directory"
        );
        // Real sibling target still lists as a directory.
        let t = listing.iter().find(|e| e.name == "target-dir").unwrap();
        assert!(t.is_dir && !t.is_symlink);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn large_directory_fixture() {
        let root = temp_root("large1k");
        for i in 0..1000 {
            fs::write(root.join(format!("file-{i:04}.txt")), b"").unwrap();
        }
        let listing = list_dir(root.to_str().unwrap()).unwrap();
        assert_eq!(listing.len(), 1000);
        assert!(listing.windows(2).all(|w| w[0].name <= w[1].name));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_path_maps_to_not_found() {
        let err = list_dir(r"Z:\definitely\not\here").unwrap_err();
        assert_eq!(err.code, crate::domain::ErrorCode::NotFound);
    }

    #[test]
    fn file_path_maps_to_not_a_directory() {
        let root = temp_root("filecase");
        let f = root.join("plain.txt");
        fs::write(&f, b"x").unwrap();
        let err = list_dir(f.to_str().unwrap()).unwrap_err();
        assert_eq!(err.code, crate::domain::ErrorCode::NotADirectory);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn drive_info_serializes_camel_case() {
        let drive = DriveInfo {
            label: "System".into(),
            path: r"C:\".into(),
            free_bytes: 82 * 1024 * 1024 * 1024,
            total_bytes: 512 * 1024 * 1024 * 1024,
            drive_type: "fixed".into(),
            fs_name: "NTFS".into(),
            serial: "1A2B-3C4D".into(),
            status: "ok".into(),
        };
        let json = serde_json::to_value(&drive).unwrap();
        assert_eq!(json["path"], r"C:\");
        assert_eq!(json["label"], "System");
        assert_eq!(json["freeBytes"], drive.free_bytes);
        assert_eq!(json["totalBytes"], drive.total_bytes);
        assert_eq!(json["driveType"], "fixed");
        assert_eq!(json["fsName"], "NTFS");
        assert_eq!(json["status"], "ok");
        // Round-trip must be lossless across the boundary.
        let back: DriveInfo = serde_json::from_value(json).unwrap();
        assert_eq!(back, drive);
    }

    #[cfg(windows)]
    #[test]
    fn bitmask_decodes_present_drive_letters() {
        use drives::{drive_root, letters_from_mask};
        // Bits 2 and 3 set → C: and D:.
        assert_eq!(letters_from_mask(0b1100), vec![b'C', b'D']);
        assert_eq!(letters_from_mask(0), Vec::<u8>::new());
        assert_eq!(drive_root(b'Z'), r"Z:\");
    }

    #[test]
    fn legacy_drive_json_gets_defaults() {
        let d: DriveInfo = serde_json::from_value(serde_json::json!({
            "label": "", "path": "E:\\", "freeBytes": 0, "totalBytes": 0
        }))
        .unwrap();
        assert_eq!(d.drive_type, "unknown");
        assert_eq!(d.status, "ok");
    }

    #[cfg(windows)]
    #[test]
    fn system_drive_is_enumerated_on_host() {
        let drives = list_drives();
        let c = drives.iter().find(|d| d.path == r"C:\");
        let c = c.expect("C:\\ must exist on a Windows dev host");
        if c.total_bytes > 0 {
            assert!(c.free_bytes <= c.total_bytes, "free must not exceed total");
        }
    }
}
