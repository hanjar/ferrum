//! FER-486 A1/B1: one guard for paths that come from the webview and are opened for READING
//! content (Quick View preview, image preview, archive member preview, AI attachments).
//!
//! String level (`check_str`), before anything touches the filesystem:
//! - `/` is normalised to `\` first, so every spelling is checked the same way;
//! - NT and device namespaces are refused in every spelling: `\\?\`, `\\.\`, `\??\`
//!   (and `//?/`, `//./`, `/\?\`, ... after normalisation);
//! - only a drive path (`X:\...`) or a UNC path (`\\host\share\...`) is accepted. UNC stays allowed
//!   because Ferrum browses network shares; the `pipe`, `mailslot` and `IPC$` shares are refused;
//! - alternate data streams are refused: no `:` anywhere except right after the drive letter.
//!
//! Handle level (`open_entry`, B1): the path is resolved ONCE and everything after that works on
//! handles, so nothing can be swapped between the check and the read:
//! - first open: no data access, the reparse point itself (an lstat; a cloud placeholder is never
//!   hydrated), `SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION` (a server at the other end can
//!   identify but never impersonate the user), `GetFileType` must be `FILE_TYPE_DISK`;
//! - a real folder: listed through that same handle (`GetFileInformationByHandleEx`);
//!   a folder junction/symlink is reported as a link and never followed;
//! - a file symlink: its target is opened once more with the same flags (following the link)
//!   and that handle is checked again; this checked handle is the one that is read;
//! - content is read from a `ReOpenFile` of the checked handle (same file object, no new path
//!   lookup), and only when neither the link nor the target carries the cloud placeholder
//!   attributes (OFFLINE / RECALL_ON_OPEN / RECALL_ON_DATA_ACCESS).
//!
//! Metadata always comes from the checked handle (`File::metadata`); for a symlink that is the
//! TARGET (so a symlink's size is the target size).

use crate::domain::{ErrorCode, FsError, validate_path};
use std::fs::{File, Metadata};

/// `SECURITY_IDENTIFICATION` (winbase.h). std adds `SECURITY_SQOS_PRESENT` when this is set.
#[cfg(windows)]
const SECURITY_IDENTIFICATION: u32 = 0x0001_0000;
#[cfg(windows)]
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
#[cfg(windows)]
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
#[cfg(windows)]
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
#[cfg(windows)]
const SHARE_ALL: u32 = 0x7; // read | write | delete: never block other users of the file

fn refused() -> FsError {
    FsError::new(ErrorCode::InvalidPath, "Path refused for safety")
}

fn valid_host(host: &str) -> bool {
    !host.is_empty()
        && !host.starts_with(['.', '-'])
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '$'))
}

/// String-level checks. Returns the normalised path (`/` replaced by `\`).
pub fn check_str(path: &str) -> Result<String, FsError> {
    validate_path(path)?;
    if path.chars().any(char::is_control) {
        return Err(refused());
    }
    let p = path.replace('/', "\\");
    if p.starts_with(r"\??\") {
        return Err(refused());
    }
    if let Some(rest) = p.strip_prefix(r"\\") {
        // UNC: \\host\share[\...]. `\\?`, `\\.` and any odd host are device namespaces.
        let mut parts = rest.split('\\');
        let host = parts.next().unwrap_or("");
        let share = parts.next().unwrap_or("");
        if !valid_host(host)
            || share.is_empty()
            || ["pipe", "mailslot", "ipc$"]
                .iter()
                .any(|s| share.eq_ignore_ascii_case(s))
            || p.contains(':')
        {
            return Err(refused());
        }
        return Ok(p);
    }
    let b = p.as_bytes();
    let drive = b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\';
    if !drive || p[2..].contains(':') {
        return Err(refused());
    }
    Ok(p)
}

/// FER-515 P3-2: true when any file-system component of a path already accepted by `check_str`
/// is a Windows reserved device name: `CON`, `PRN`, `AUX`, `NUL`, `COM1-9`, `LPT1-9` (also the
/// superscript digits), `CONIN$`, `CONOUT$` - with or without an extension, any case, ignoring the
/// trailing dots/spaces Windows strips. The drive (`X:`) and the UNC host/share are skipped.
pub fn has_reserved_device_component(normalised: &str) -> bool {
    let rest = match normalised.strip_prefix(r"\\") {
        Some(unc) => unc.splitn(3, '\\').nth(2).unwrap_or(""),
        None => normalised.get(2..).unwrap_or(""),
    };
    rest.split('\\')
        .filter(|c| !c.is_empty())
        .any(is_reserved_device_name)
}

pub fn is_reserved_device_name(comp: &str) -> bool {
    let trimmed = comp.trim_end_matches(['.', ' ']);
    let stem = trimmed
        .split('.')
        .next()
        .unwrap_or(trimmed)
        .trim_end_matches(' ')
        .to_uppercase();
    if matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) {
        return true;
    }
    let mut chars = stem.chars();
    let prefix: String = chars.by_ref().take(3).collect();
    matches!(prefix.as_str(), "COM" | "LPT")
        && matches!(
            chars.next(),
            Some('1'..='9' | '\u{00B9}' | '\u{00B2}' | '\u{00B3}')
        )
        && chars.next().is_none()
}

/// A checked disk file. `meta` describes the checked handle (the symlink TARGET for a link).
pub struct GuardedFile {
    pub path: String,
    pub meta: Metadata,
    /// True when the path itself is a file symlink (the target was opened and checked).
    pub via_symlink: bool,
    /// Cloud placeholder (on the link or the target): content must not be read.
    pub offline: bool,
    handle: File,
}

/// A checked real folder (not a reparse point).
pub struct GuardedDir {
    pub path: String,
    pub meta: Metadata,
    handle: File,
}

pub enum Entry {
    File(GuardedFile),
    Dir(GuardedDir),
    /// A folder junction or symlink: reported, never followed.
    LinkDir {
        path: String,
        meta: Metadata,
    },
}

/// One row of a folder listing read through the checked handle.
pub struct DirRow {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
}

#[cfg(windows)]
fn attrs(meta: &Metadata) -> u32 {
    use std::os::windows::fs::MetadataExt;
    meta.file_attributes()
}

#[cfg(windows)]
fn open_flags(path: &str, follow: bool) -> Result<File, FsError> {
    use std::os::windows::fs::OpenOptionsExt;
    let mut flags = FILE_FLAG_BACKUP_SEMANTICS;
    if !follow {
        flags |= FILE_FLAG_OPEN_REPARSE_POINT;
    }
    std::fs::OpenOptions::new()
        .access_mode(0)
        .share_mode(SHARE_ALL)
        .custom_flags(flags)
        .security_qos_flags(SECURITY_IDENTIFICATION)
        .open(path)
        .map_err(|e| FsError::from(e).with_path(path))
}

/// Refuses every handle that is not a disk file or folder (pipe, character device, ...).
#[cfg(windows)]
pub(crate) fn require_disk(file: &File) -> Result<(), FsError> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{FILE_TYPE_DISK, GetFileType};
    // SAFETY: the handle is valid for the lifetime of `file`.
    if unsafe { GetFileType(file.as_raw_handle() as _) } != FILE_TYPE_DISK {
        return Err(refused());
    }
    Ok(())
}

/// Resolves the path once and returns handle-backed facts about it (see module docs).
#[cfg(windows)]
pub fn open_entry(path: &str) -> Result<Entry, FsError> {
    let path = check_str(path)?;
    let link = open_flags(&path, false)?;
    require_disk(&link)?;
    let link_meta = link
        .metadata()
        .map_err(|e| FsError::from(e).with_path(&path))?;
    let reparse = attrs(&link_meta) & FILE_ATTRIBUTE_REPARSE_POINT != 0;
    if link_meta.is_dir() {
        if reparse {
            return Ok(Entry::LinkDir {
                path,
                meta: link_meta,
            });
        }
        return Ok(Entry::Dir(GuardedDir {
            path,
            meta: link_meta,
            handle: link,
        }));
    }
    let link_offline = crate::media_meta::offline_attrs(attrs(&link_meta));
    if !link_meta.file_type().is_symlink() {
        // A regular file, or a cloud placeholder (a reparse point that is not a symlink).
        return Ok(Entry::File(GuardedFile {
            path,
            offline: link_offline,
            via_symlink: false,
            meta: link_meta,
            handle: link,
        }));
    }
    // File symlink: open the target once (same SQOS flags) and check THAT handle.
    let target = open_flags(&path, true)?;
    require_disk(&target)?;
    let meta = target
        .metadata()
        .map_err(|e| FsError::from(e).with_path(&path))?;
    if meta.is_dir() {
        return Ok(Entry::LinkDir {
            path,
            meta: link_meta,
        });
    }
    Ok(Entry::File(GuardedFile {
        path,
        offline: link_offline || crate::media_meta::offline_attrs(attrs(&meta)),
        via_symlink: true,
        meta,
        handle: target,
    }))
}

#[cfg(not(windows))]
pub fn open_entry(path: &str) -> Result<Entry, FsError> {
    let path = check_str(path)?;
    let link_meta =
        std::fs::symlink_metadata(&path).map_err(|e| FsError::from(e).with_path(&path))?;
    if link_meta.file_type().is_symlink() && std::fs::metadata(&path).is_ok_and(|m| m.is_dir()) {
        return Ok(Entry::LinkDir {
            path,
            meta: link_meta,
        });
    }
    let handle = File::open(&path).map_err(|e| FsError::from(e).with_path(&path))?;
    let meta = handle
        .metadata()
        .map_err(|e| FsError::from(e).with_path(&path))?;
    if meta.is_dir() {
        return Ok(Entry::Dir(GuardedDir { path, meta, handle }));
    }
    Ok(Entry::File(GuardedFile {
        path,
        via_symlink: link_meta.file_type().is_symlink(),
        offline: false,
        meta,
        handle,
    }))
}

/// (volume serial, file index) of an open handle: identifies the file object across handles.
#[cfg(windows)]
fn file_identity(file: &File) -> Option<(u32, u64)> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };
    // SAFETY: zeroed POD out-parameter; valid handle for the lifetime of `file`.
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle() as _, &mut info) };
    (ok != 0).then(|| {
        (
            info.dwVolumeSerialNumber,
            (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
        )
    })
}

/// `ReOpenFile` of a checked handle with new access: same file object, no path lookup.
#[cfg(windows)]
fn reopen(handle: &File, access: u32, flags: u32, path: &str) -> Result<File, FsError> {
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::Storage::FileSystem::ReOpenFile;
    // SAFETY: valid source handle; the result is owned by the returned File.
    let raw = unsafe { ReOpenFile(handle.as_raw_handle() as _, access, SHARE_ALL, flags) };
    if raw == INVALID_HANDLE_VALUE || raw.is_null() {
        return Err(FsError::from(std::io::Error::last_os_error()).with_path(path));
    }
    // SAFETY: `raw` is a fresh handle we own.
    let file = unsafe { File::from_raw_handle(raw as _) };
    require_disk(&file)?;
    Ok(file)
}

impl GuardedFile {
    /// Read handle on the SAME file object that was checked. Refused for cloud placeholders.
    pub fn open_read(&self) -> Result<File, FsError> {
        if self.offline {
            return Err(crate::preview::offline_error().with_path(&self.path));
        }
        #[cfg(windows)]
        {
            use windows_sys::Win32::Storage::FileSystem::FILE_GENERIC_READ;
            reopen(&self.handle, FILE_GENERIC_READ, 0, &self.path)
        }
        #[cfg(not(windows))]
        {
            self.handle
                .try_clone()
                .map_err(|e| FsError::from(e).with_path(&self.path))
        }
    }
}

/// FER-488: a checked file held in place for a backend that can only open by PATH (unrar).
/// `path` is the final path of the pinned file object (`\\?\` form, no junction or symlink left
/// in it). While the pin lives, its handle (read access, `FILE_SHARE_READ` only) keeps anyone
/// from writing, renaming or deleting the file, and a folder that holds an open file cannot be
/// renamed either, so `path` keeps naming the file object that was checked.
pub struct PathPin {
    pub path: String,
    #[cfg(windows)]
    _pin: File,
}

impl GuardedFile {
    /// Pins the checked file for a path-only backend: opens the path again with the same SQOS
    /// identification flags, requires `FILE_TYPE_DISK`, and requires the SAME file object
    /// (volume serial + file index) as the checked handle. Refused for cloud placeholders.
    pub fn pin_for_path_api(&self) -> Result<PathPin, FsError> {
        if self.offline {
            return Err(crate::preview::offline_error().with_path(&self.path));
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;
            let pin = std::fs::OpenOptions::new()
                .read(true)
                .share_mode(FILE_SHARE_READ)
                .security_qos_flags(SECURITY_IDENTIFICATION)
                .open(&self.path)
                .map_err(|e| FsError::from(e).with_path(&self.path))?;
            require_disk(&pin)?;
            let checked = file_identity(&self.handle);
            if checked.is_none() || checked != file_identity(&pin) {
                return Err(refused());
            }
            let path = final_path_of(&pin).ok_or_else(refused)?;
            Ok(PathPin { path, _pin: pin })
        }
        #[cfg(not(windows))]
        {
            Ok(PathPin {
                path: self.path.clone(),
            })
        }
    }
}

impl GuardedDir {
    /// Direct entries of the checked folder, read through its handle: at most `keep` rows, and
    /// at most `max_scan` entries counted. Returns (rows, counted, stopped_early).
    #[cfg(windows)]
    pub fn list(
        &self,
        keep: usize,
        max_scan: usize,
    ) -> Result<(Vec<DirRow>, usize, bool), FsError> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Foundation::{ERROR_NO_MORE_FILES, GetLastError};
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_ID_BOTH_DIR_INFO, FILE_LIST_DIRECTORY, FileIdBothDirectoryInfo,
            FileIdBothDirectoryRestartInfo, GetFileInformationByHandleEx, SYNCHRONIZE,
        };
        // ReOpenFile does not grant list access on this directory handle, so the listing handle is
        // opened once more (same SQOS flags, reparse point not followed) and must be the very same
        // folder as the checked handle (volume serial + file index), a real folder, on disk.
        let dir = {
            use std::os::windows::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .access_mode(FILE_LIST_DIRECTORY | SYNCHRONIZE)
                .share_mode(SHARE_ALL)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
                .security_qos_flags(SECURITY_IDENTIFICATION)
                .open(&self.path)
                .map_err(|e| FsError::from(e).with_path(&self.path))?
        };
        require_disk(&dir)?;
        let same = file_identity(&dir).is_some_and(|id| Some(id) == file_identity(&self.handle));
        let meta = dir
            .metadata()
            .map_err(|e| FsError::from(e).with_path(&self.path))?;
        if !same || !meta.is_dir() || attrs(&meta) & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(
                FsError::new(ErrorCode::Io, "Folder changed while it was being read")
                    .with_path(&self.path),
            );
        }
        let mut rows = Vec::new();
        let mut counted = 0usize;
        let mut buf = vec![0u64; 8 * 1024]; // 64 KiB, 8-byte aligned
        let mut class = FileIdBothDirectoryRestartInfo;
        loop {
            // SAFETY: buffer valid and writable for its byte length.
            let ok = unsafe {
                GetFileInformationByHandleEx(
                    dir.as_raw_handle() as _,
                    class,
                    buf.as_mut_ptr().cast(),
                    (buf.len() * 8) as u32,
                )
            };
            if ok == 0 {
                // SAFETY: plain thread-local error read.
                if unsafe { GetLastError() } == ERROR_NO_MORE_FILES {
                    return Ok((rows, counted, false));
                }
                return Err(FsError::from(std::io::Error::last_os_error()).with_path(&self.path));
            }
            class = FileIdBothDirectoryInfo;
            let base = buf.as_ptr().cast::<u8>();
            let mut offset = 0usize;
            loop {
                // SAFETY: the API wrote a chain of FILE_ID_BOTH_DIR_INFO records into `buf`;
                // offsets come from NextEntryOffset and stay inside the buffer.
                let info = unsafe { &*base.add(offset).cast::<FILE_ID_BOTH_DIR_INFO>() };
                let name_len = info.FileNameLength as usize / 2;
                // SAFETY: FileNameLength bytes of UTF-16 follow FileName inside the record.
                let name = unsafe {
                    std::slice::from_raw_parts(
                        std::ptr::addr_of!(info.FileName).cast::<u16>(),
                        name_len,
                    )
                };
                let name = String::from_utf16_lossy(name);
                if name != "." && name != ".." {
                    if counted >= max_scan {
                        return Ok((rows, counted, true));
                    }
                    counted += 1;
                    if rows.len() < keep {
                        let is_dir = info.FileAttributes & 0x10 != 0;
                        rows.push(DirRow {
                            name,
                            is_dir,
                            size: if is_dir {
                                0
                            } else {
                                info.EndOfFile.max(0) as u64
                            },
                        });
                    }
                }
                if info.NextEntryOffset == 0 {
                    break;
                }
                offset += info.NextEntryOffset as usize;
            }
        }
    }

    #[cfg(not(windows))]
    pub fn list(
        &self,
        keep: usize,
        max_scan: usize,
    ) -> Result<(Vec<DirRow>, usize, bool), FsError> {
        let mut rows = Vec::new();
        let mut counted = 0;
        for entry in std::fs::read_dir(&self.path).map_err(FsError::from)? {
            let Ok(entry) = entry else { continue };
            if counted >= max_scan {
                return Ok((rows, counted, true));
            }
            counted += 1;
            if rows.len() < keep {
                let meta = entry.metadata().ok();
                let is_dir = meta.as_ref().is_some_and(|m| m.is_dir());
                rows.push(DirRow {
                    name: entry.file_name().to_string_lossy().into_owned(),
                    is_dir,
                    size: meta.map_or(0, |m| if is_dir { 0 } else { m.len() }),
                });
            }
        }
        Ok((rows, counted, false))
    }
}

/// FER-490: final path of an open handle (`GetFinalPathNameByHandleW`, DOS volume name, `\\?\`
/// prefix kept). Tells where the checked file object REALLY is, whatever path was used to open it
/// (a junction swapped into a parent folder shows up here).
#[cfg(windows)]
fn final_path_of(file: &File) -> Option<String> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::GetFinalPathNameByHandleW;
    let mut buf = vec![0u16; 1024];
    loop {
        // SAFETY: valid handle; `buf` is writable for `buf.len()` u16s.
        let n = unsafe {
            GetFinalPathNameByHandleW(
                file.as_raw_handle() as _,
                buf.as_mut_ptr(),
                buf.len() as u32,
                0,
            )
        } as usize;
        if n == 0 {
            return None;
        }
        if n < buf.len() {
            return Some(String::from_utf16_lossy(&buf[..n]));
        }
        if n > 32 * 1024 {
            return None;
        }
        buf.resize(n + 1, 0);
    }
}

#[cfg(not(windows))]
fn final_path_of(_file: &File) -> Option<String> {
    None
}

impl GuardedFile {
    /// FER-490: where the checked file object really is (see `final_path_of`).
    pub fn final_path(&self) -> Option<String> {
        final_path_of(&self.handle)
    }
}

impl GuardedDir {
    /// FER-490: where the checked folder really is (see `final_path_of`).
    pub fn final_path(&self) -> Option<String> {
        final_path_of(&self.handle)
    }
}

// ── FER-467 R7: a local place Ferrum may WRITE its own settings file to ──

/// Why a path is not an acceptable local settings location.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalRefusal {
    /// Not an absolute drive path, device/NT namespace, or other malformed spelling.
    Invalid,
    /// An alternate data stream (`h.json:x`, `h.json::$DATA`).
    Stream,
    /// A reserved DOS device name in some component (`CON`, `NUL.json`, `COM1`, ...) or a
    /// component ending in a dot/space (Windows silently strips those, so two spellings alias).
    Reserved,
    /// UNC, a drive letter mapped to a network share, or a folder whose real location is remote.
    Network,
    /// A junction, symlink or other name-surrogate reparse point somewhere in the path.
    Link,
}

/// `CON`, `PRN`, `AUX`, `NUL`, `COM0-9`, `LPT0-9` (plus the superscript digits ¹²³ Windows also
/// treats as ports), `CONIN$`, `CONOUT$`, `CLOCK$`. Matched on the part before the first dot, with
/// trailing spaces ignored, case-insensitively (`nul.json`, `Com1 .txt` are devices too).
pub fn is_reserved_name(component: &str) -> bool {
    let stem = component
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end_matches(' ');
    let up = stem.to_uppercase();
    if matches!(
        up.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$" | "CLOCK$"
    ) {
        return true;
    }
    let mut chars = up.chars();
    let prefix: String = chars.by_ref().take(3).collect();
    let rest: Vec<char> = chars.collect();
    (prefix == "COM" || prefix == "LPT")
        && rest.len() == 1
        && (rest[0].is_ascii_digit() || matches!(rest[0], '¹' | '²' | '³'))
}

/// Pure string checks for a local settings location (before the filesystem is consulted).
/// Returns the normalised path (`/` replaced by `\`).
pub fn check_local_str(path: &str) -> Result<String, LocalRefusal> {
    let p = path.replace('/', "\\");
    if let Some(rest) = p.strip_prefix(r"\\") {
        // Device namespaces are malformed; anything else starting with two separators is UNC.
        let device = rest.starts_with(['?', '.']) || rest.trim().is_empty();
        return Err(if device {
            LocalRefusal::Invalid
        } else {
            LocalRefusal::Network
        });
    }
    let b = p.as_bytes();
    let drive = b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\';
    if drive && p[2..].contains(':') {
        return Err(LocalRefusal::Stream);
    }
    let p = check_str(path).map_err(|_| LocalRefusal::Invalid)?;
    if p.contains(['*', '?', '"', '<', '>', '|']) {
        return Err(LocalRefusal::Invalid);
    }
    for comp in p[3..].split('\\').filter(|c| !c.is_empty()) {
        if is_reserved_name(comp) || comp.ends_with('.') || comp.ends_with(' ') {
            return Err(LocalRefusal::Reserved);
        }
    }
    Ok(p)
}

/// `DRIVE_REMOTE` from `GetDriveTypeW`.
pub const DRIVE_REMOTE: u32 = 4;

/// Drive types a local settings file may live on: anything but a network drive.
pub fn drive_type_is_local(drive_type: u32) -> bool {
    drive_type != DRIVE_REMOTE
}

/// Name-surrogate reparse tags (junctions, symlinks, ...) redirect a path elsewhere; cloud-file
/// placeholder tags (OneDrive/cloud sync) and the like do not and stay allowed.
pub fn reparse_tag_redirects(tag: u32) -> bool {
    tag & 0x2000_0000 != 0
}

#[cfg(windows)]
fn reparse_tag_of(path: &str) -> Option<u32> {
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::Storage::FileSystem::{FindClose, FindFirstFileW, WIN32_FIND_DATAW};
    let wide: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
    // SAFETY: `wide` is NUL-terminated; `data` is a valid out pointer; the find handle is closed.
    unsafe {
        let mut data: WIN32_FIND_DATAW = std::mem::zeroed();
        let h = FindFirstFileW(wide.as_ptr(), &mut data);
        if h == INVALID_HANDLE_VALUE {
            return None;
        }
        FindClose(h);
        (data.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0).then_some(data.dwReserved0)
    }
}

/// Real (filesystem) checks for a place Ferrum will write a settings file to (FER-467 R7): the
/// string checks above, then the drive must not be a mapped network drive, no existing component
/// may be a junction/symlink, and the deepest existing folder must really be local (its final path
/// is not UNC). The drive type is checked before any component is touched, so a mapped drive is
/// refused without contacting the server; no file content is read.
#[cfg(windows)]
pub fn check_local_location(path: &str) -> Result<String, LocalRefusal> {
    use windows_sys::Win32::Storage::FileSystem::GetDriveTypeW;
    let p = check_local_str(path)?;
    let root: Vec<u16> = p[..3].encode_utf16().chain(Some(0)).collect();
    // SAFETY: NUL-terminated root path.
    if !drive_type_is_local(unsafe { GetDriveTypeW(root.as_ptr()) }) {
        return Err(LocalRefusal::Network);
    }
    // Every existing component below the root, deepest first.
    let mut deepest_dir: Option<String> = None;
    let mut cur = p.trim_end_matches('\\').to_string();
    while cur.len() > 3 {
        if let Ok(meta) = std::fs::symlink_metadata(&cur) {
            if attrs(&meta) & FILE_ATTRIBUTE_REPARSE_POINT != 0
                && reparse_tag_of(&cur).is_none_or(reparse_tag_redirects)
            {
                return Err(LocalRefusal::Link);
            }
            if deepest_dir.is_none() && meta.is_dir() {
                deepest_dir = Some(cur.clone());
            }
        }
        match cur.rfind('\\') {
            Some(2) => cur.truncate(3),
            Some(i) if i > 2 => cur.truncate(i),
            _ => break,
        }
    }
    if let Some(dir) = deepest_dir
        && let Ok(handle) = open_flags(&dir, true)
        && final_path_of(&handle).is_some_and(|f| {
            let f = f.to_ascii_uppercase();
            f.starts_with(r"\\?\UNC\") || (f.starts_with(r"\\") && !f.starts_with(r"\\?\"))
        })
    {
        return Err(LocalRefusal::Network);
    }
    Ok(p)
}

#[cfg(not(windows))]
pub fn check_local_location(path: &str) -> Result<String, LocalRefusal> {
    check_local_str(path)
}

/// `check_str` + handle checks, for callers that need a checked disk FILE (not a folder or link).
/// Archive member previews open the archive through `preview_archive::CheckedArchive` (FER-488).
pub fn checked_file(path: &str) -> Result<GuardedFile, FsError> {
    match open_entry(path)? {
        Entry::File(file) => Ok(file),
        Entry::Dir(GuardedDir { path, .. }) | Entry::LinkDir { path, .. } => {
            Err(FsError::new(ErrorCode::NotADirectory, "Not a regular file").with_path(&path))
        }
    }
}

#[cfg(test)]
#[path = "path_guard_tests.rs"]
mod tests;
