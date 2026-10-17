//! Handle-relative, no-follow publication primitives for archive extraction.
//!
//! Every path component is resolved against an already-opened parent
//! HANDLE/dirfd, never re-resolved through the filesystem namespace. A
//! concurrently swapped component therefore cannot redirect writes: the open
//! itself either lands on the validated directory object or fails. Any
//! object that turns out to be a reparse point (Windows junction/symlink)
//! or symlink (Unix) is rejected — fail-closed.
//!
//! Windows notes:
//! - The destination root is opened via `CreateFileW` (DOS path) with
//!   GENERIC_READ|GENERIC_WRITE so children can be created under it.
//! - Children use `NtCreateFile` with `RootDirectory` set to the parent
//!   handle and `FILE_OPEN_REPARSE_POINT`, so junctions are never traversed;
//!   an attribute-tag query rejects reparse targets explicitly.
//! - Partial-file rollback uses POSIX-semantics delete through the file's
//!   own handle (requires the DELETE access bit on open).
//!
//! Unix notes: `openat`/`mkdirat` with `O_NOFOLLOW`, `fstatat`
//! `AT_SYMLINK_NOFOLLOW` checks, and `unlinkat` rollback.

use std::ffi::OsStr;
use std::io;
use std::path::Path;

#[derive(Debug)]
#[allow(dead_code)] // variants are constructed by platform-specific paths
pub enum PublishError {
    /// Final component does not exist.
    NotFound,
    /// Exclusive create hit an existing object.
    Exists,
    /// Existing object is a symlink/junction/reparse point.
    Reparse,
    /// Existing object kind conflicts with the request (file vs dir).
    WrongKind,
    Io(String),
}

impl PublishError {
    fn io(err: io::Error) -> Self {
        match err.kind() {
            io::ErrorKind::NotFound => Self::NotFound,
            io::ErrorKind::AlreadyExists => Self::Exists,
            _ => Self::Io(err.to_string()),
        }
    }
}

/// A directory opened by handle. Children resolve relative to this object,
/// never through a path.
pub struct DirHandle {
    #[cfg(windows)]
    handle: windows_sys::Win32::Foundation::HANDLE,
    #[cfg(unix)]
    fd: std::os::unix::io::RawFd,
    /// True when created by the current extraction (rollback candidate).
    /// Read by the extraction rollback pass on non-Windows hosts; reserved
    /// on Windows where rollback uses per-file POSIX disposition.
    #[allow(dead_code)]
    pub created_for_rollback: bool,
}

unsafe impl Send for DirHandle {}

impl Drop for DirHandle {
    fn drop(&mut self) {
        #[cfg(windows)]
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.handle);
        }
        #[cfg(unix)]
        unsafe {
            libc::close(self.fd);
        }
    }
}

/// Open the extraction root. Rejects reparse-point roots and anything that is not a directory:
/// the caller chose a real directory; a junction there means it no longer resolves as named.
/// Links in the ANCESTORS of `path` are resolved by the OS as usual; callers that must reject
/// those compare [`final_path`] with the path they recorded.
#[allow(dead_code)]
pub fn open_root(path: &Path) -> Result<DirHandle, PublishError> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::Storage::FileSystem::{
            CreateFileW, FILE_ATTRIBUTE_DIRECTORY, FILE_FLAG_BACKUP_SEMANTICS,
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
            FILE_SHARE_WRITE, OPEN_EXISTING,
        };

        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let close = |h| unsafe {
            windows_sys::Win32::Foundation::CloseHandle(h);
        };

        // Step 1: look at the object NAMED by `path` itself, without following it. A handle
        // opened with FILE_FLAG_OPEN_REPARSE_POINT is filtered and cannot parent NtCreateFile
        // children, so it is only used to inspect and to remember the object's identity.
        // (`std::fs::metadata` follows links and would report the junction's TARGET.)
        let probe = unsafe {
            CreateFileW(
                wide.as_ptr(),
                FILE_READ_ATTRIBUTES,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                std::ptr::null_mut(),
            )
        };
        if probe == INVALID_HANDLE_VALUE || probe.is_null() {
            return Err(PublishError::io(io::Error::last_os_error()));
        }
        let probed = (|| {
            if is_reparse(probe)? {
                return Err(PublishError::Reparse);
            }
            let attrs = handle_attributes(probe).map_err(PublishError::io)?;
            if attrs & FILE_ATTRIBUTE_DIRECTORY == 0 {
                return Err(PublishError::WrongKind);
            }
            object_id(probe).map_err(PublishError::io)
        })();
        close(probe);
        let wanted = probed?;

        // Step 2: open the usable handle (children are created through it), then prove ON THE
        // HANDLE that it is the very object inspected in step 1. A name swapped for a junction,
        // symlink or another directory between the two opens has another identity: refused.
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                // GENERIC_WRITE carries FILE_ADD_FILE/FILE_ADD_SUBDIRECTORY,
                // required to create children through this handle; DELETE is
                // needed to roll back empty directories created by the run.
                GENERIC_READ | GENERIC_WRITE | FILE_DELETE_ACCESS,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE || handle.is_null() {
            return Err(PublishError::io(io::Error::last_os_error()));
        }
        let verified = (|| {
            if is_reparse(handle)? {
                return Err(PublishError::Reparse);
            }
            let attrs = handle_attributes(handle).map_err(PublishError::io)?;
            if attrs & FILE_ATTRIBUTE_DIRECTORY == 0 {
                return Err(PublishError::WrongKind);
            }
            if object_id(handle).map_err(PublishError::io)? != wanted {
                // The name now resolves to something else than what was inspected.
                return Err(PublishError::Reparse);
            }
            Ok(())
        })();
        if let Err(e) = verified {
            close(handle);
            return Err(e);
        }
        Ok(DirHandle {
            handle,
            created_for_rollback: false,
        })
    }
    #[cfg(unix)]
    {
        let fd = open_unix(path.as_os_str(), OpenKind::ExistingDir, None)?;
        if is_reparse(fd)? {
            return Err(PublishError::Reparse);
        }
        Ok(DirHandle {
            fd,
            created_for_rollback: false,
        })
    }
}

/// Open (or create) the child directory `name` relative to `parent`.
/// Reparse children are rejected after a no-follow open.
#[allow(dead_code)]
pub fn child_dir(parent: &DirHandle, name: &OsStr) -> Result<DirHandle, PublishError> {
    #[cfg(windows)]
    {
        let handle = open_windows(name, OpenKind::ExistingDir, Some(parent.handle))?;
        if is_reparse(handle)? {
            return Err(PublishError::Reparse);
        }
        Ok(DirHandle {
            handle,
            created_for_rollback: false,
        })
    }
    #[cfg(unix)]
    {
        let fd = open_unix(name, OpenKind::ExistingDir, Some(parent.fd))?;
        if is_reparse(fd)? {
            return Err(PublishError::Reparse);
        }
        Ok(DirHandle {
            fd,
            created_for_rollback: false,
        })
    }
}

/// Create a child directory relative to `parent`; fails if it exists.
#[allow(dead_code)]
pub fn create_dir_new(parent: &DirHandle, name: &OsStr) -> Result<DirHandle, PublishError> {
    #[cfg(windows)]
    {
        let handle = open_windows(name, OpenKind::NewDir, Some(parent.handle))?;
        if is_reparse(handle)? {
            return Err(PublishError::Reparse);
        }
        Ok(DirHandle {
            handle,
            created_for_rollback: true,
        })
    }
    #[cfg(unix)]
    {
        let fd = open_unix(name, OpenKind::NewDir, Some(parent.fd))?;
        if is_reparse(fd)? {
            return Err(PublishError::Reparse);
        }
        Ok(DirHandle {
            fd,
            created_for_rollback: true,
        })
    }
}

/// Exclusive file creation relative to `parent` (no-clobber).
#[allow(dead_code)]
pub fn create_file_new(parent: &DirHandle, name: &OsStr) -> Result<std::fs::File, PublishError> {
    #[cfg(windows)]
    {
        let handle = open_windows(name, OpenKind::NewFile, Some(parent.handle))?;
        if is_reparse(handle)? {
            return Err(PublishError::Reparse);
        }
        Ok(unsafe { std::os::windows::io::FromRawHandle::from_raw_handle(handle) })
    }
    #[cfg(unix)]
    {
        let fd = open_unix(name, OpenKind::NewFile, Some(parent.fd))?;
        if is_reparse(fd)? {
            return Err(PublishError::Reparse);
        }
        Ok(unsafe { std::os::unix::io::FromRawFd::from_raw_fd(fd) })
    }
}

/// Open an existing file for truncating write, relative to `parent`.
/// Rejects reparse/symlink targets.
#[allow(dead_code)]
pub fn open_existing_for_overwrite(
    parent: &DirHandle,
    name: &OsStr,
) -> Result<std::fs::File, PublishError> {
    #[cfg(windows)]
    {
        let handle = open_windows(name, OpenKind::ExistingFileTruncate, Some(parent.handle))?;
        if is_reparse(handle)? {
            return Err(PublishError::Reparse);
        }
        Ok(unsafe { std::os::windows::io::FromRawHandle::from_raw_handle(handle) })
    }
    #[cfg(unix)]
    {
        let fd = open_unix(name, OpenKind::ExistingFileTruncate, Some(parent.fd))?;
        if is_reparse(fd)? {
            return Err(PublishError::Reparse);
        }
        Ok(unsafe { std::os::unix::io::FromRawFd::from_raw_fd(fd) })
    }
}

/// Non-destructive existence check relative to `parent` (no-follow).
#[allow(dead_code)]
pub fn entry_exists(parent: &DirHandle, name: &OsStr) -> Result<bool, PublishError> {
    #[cfg(windows)]
    {
        match open_windows(name, OpenKind::ExistingFileRead, Some(parent.handle)) {
            Ok(handle) => {
                unsafe {
                    windows_sys::Win32::Foundation::CloseHandle(handle);
                }
                Ok(true)
            }
            Err(PublishError::NotFound) => Ok(false),
            Err(PublishError::WrongKind) => Ok(true), // exists but is a dir
            Err(e) => Err(e),
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let c = std::ffi::CString::new(name.as_bytes())
            .map_err(|_| PublishError::Io("NUL in name".into()))?;
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        let rc =
            unsafe { libc::fstatat(parent.fd, c.as_ptr(), &mut stat, libc::AT_SYMLINK_NOFOLLOW) };
        if rc == 0 {
            Ok(true)
        } else {
            match errno() {
                libc::ENOENT | libc::ENOTDIR => Ok(false),
                _ => Err(PublishError::io(io::Error::last_os_error())),
            }
        }
    }
}

/// Last-write time (unix ms) of an entry relative to `parent`, for
/// OverwriteOlder. None = unknown (caller treats destination as older).
#[allow(dead_code)]
pub fn modified_ms(parent: &DirHandle, name: &OsStr) -> Option<i64> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
        };
        let handle = open_windows(name, OpenKind::ExistingFileRead, Some(parent.handle)).ok()?;
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        let ok = unsafe { GetFileInformationByHandle(handle, &mut info) };
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(handle);
        }
        if ok == 0 {
            return None;
        }
        let ticks100 = ((info.ftLastWriteTime.dwHighDateTime as u64) << 32)
            | info.ftLastWriteTime.dwLowDateTime as u64;
        const EPOCH_DIFF: u64 = 11_644_473_600; // 1601→1970 seconds
        Some(
            ((ticks100 / 10_000_000).saturating_sub(EPOCH_DIFF)) as i64 * 1000
                + ((ticks100 % 10_000_000) / 10_000) as i64,
        )
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let c = std::ffi::CString::new(name.as_bytes()).ok()?;
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        let rc =
            unsafe { libc::fstatat(parent.fd, c.as_ptr(), &mut stat, libc::AT_SYMLINK_NOFOLLOW) };
        if rc != 0 {
            return None;
        }
        Some((stat.st_mtime as i64) * 1000 + (stat.st_mtime_nsec / 1_000_000) as i64)
    }
}

/// Unlink `name` via the parent's own descriptor (no path resolution). A directory is only
/// removed when empty. The entry is opened WITHOUT following a reparse point/symlink: when the
/// name turned into a link (swapped in after the caller created a real entry there), the link
/// is left untouched and `Err(Reparse)` is returned. Nothing behind a link is ever removed, and
/// a link that this run did not create is not this run's to delete.
#[allow(dead_code)]
pub fn unlink_entry(parent: &DirHandle, name: &OsStr, is_dir: bool) -> Result<(), PublishError> {
    #[cfg(windows)]
    {
        let kind = if is_dir {
            OpenKind::ExistingDirRemove
        } else {
            OpenKind::ExistingFileRemove
        };
        let handle = open_windows(name, kind, Some(parent.handle))?;
        let close = |h| unsafe {
            windows_sys::Win32::Foundation::CloseHandle(h);
        };
        match is_reparse(handle) {
            Ok(false) => {}
            Ok(true) => {
                close(handle);
                return Err(PublishError::Reparse);
            }
            Err(e) => {
                close(handle);
                return Err(e);
            }
        }
        let result = set_disposition(handle, false);
        close(handle);
        result.map_err(PublishError::io)
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let c = std::ffi::CString::new(name.as_bytes())
            .map_err(|_| PublishError::Io("NUL in name".into()))?;
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstatat(parent.fd, c.as_ptr(), &mut stat, libc::AT_SYMLINK_NOFOLLOW) }
            != 0
        {
            return Err(PublishError::io(io::Error::last_os_error()));
        }
        if libc::S_ISLNK(stat.st_mode) {
            return Err(PublishError::Reparse);
        }
        let rc = unsafe {
            libc::unlinkat(
                parent.fd,
                c.as_ptr(),
                if is_dir { libc::AT_REMOVEDIR } else { 0 },
            )
        };
        if rc != 0 {
            return Err(PublishError::io(io::Error::last_os_error()));
        }
        Ok(())
    }
}

// ── Sync engine primitives (Automation P0.2) ────────────────────────────
//
// Handle-relative and no-follow like everything above: a name is resolved against an opened
// parent object, a reparse point is never traversed, and nothing here ever removes a non-empty
// directory.

/// Why a handle-relative move did not happen.
#[derive(Debug)]
#[allow(dead_code)]
pub enum MoveError {
    /// Source and destination are on different volumes: nothing was moved.
    CrossDevice,
    /// The destination name exists: nothing was moved or replaced.
    Exists,
    Publish(PublishError),
}

/// Remove the empty directory `name` below `parent`, never following a reparse point.
/// `Ok(true)` = removed, `Ok(false)` = not empty (nothing changed), `Err(NotFound)` = already
/// gone, `Err(Reparse)` = a junction or symlink (left alone). Never recursive.
#[allow(dead_code)]
pub fn remove_empty_dir(parent: &DirHandle, name: &OsStr) -> Result<bool, PublishError> {
    #[cfg(windows)]
    {
        let handle = open_windows(name, OpenKind::ExistingDirRemove, Some(parent.handle))?;
        if is_link(handle)? {
            unsafe {
                windows_sys::Win32::Foundation::CloseHandle(handle);
            }
            return Err(PublishError::Reparse);
        }
        let result = set_disposition(handle, false);
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(handle);
        }
        match result {
            Ok(()) => Ok(true),
            // ERROR_DIR_NOT_EMPTY
            Err(e) if e.raw_os_error() == Some(145) => Ok(false),
            Err(e) => Err(PublishError::io(e)),
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (parent, name);
        Err(PublishError::Io("unsupported platform".into()))
    }
}

/// The final path the OS resolves for the folder behind `dir` right now (`\\?\` prefix
/// included, links resolved). Used to prove that a root opened by path is the folder the caller
/// named, not something a junction or symlink on the way redirected it to.
#[allow(dead_code)]
pub fn final_path(dir: &DirHandle) -> Option<String> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Storage::FileSystem::GetFinalPathNameByHandleW;
        let mut buf = vec![0u16; 1024];
        loop {
            // SAFETY: valid handle; `buf` is writable for `buf.len()` u16s.
            let n = unsafe {
                GetFinalPathNameByHandleW(dir.handle, buf.as_mut_ptr(), buf.len() as u32, 0)
            } as usize;
            if n == 0 {
                return None;
            }
            if n < buf.len() {
                return Some(String::from_utf16_lossy(&buf[..n]));
            }
            buf.resize(n + 1, 0);
        }
    }
    #[cfg(not(windows))]
    {
        let _ = dir;
        None
    }
}

/// The raw OS handle of an opened folder (identity checks read ids from it).
#[cfg(windows)]
#[allow(dead_code)]
pub fn raw_handle(dir: &DirHandle) -> windows_sys::Win32::Foundation::HANDLE {
    dir.handle
}

/// Open a folder to create children in (no DELETE access, so it can sit next to a read-only pin
/// that shares no delete). Does not look at reparse points: callers prove the folder with
/// [`final_path`] and its ids.
#[allow(dead_code)]
pub fn open_root_write(path: &Path) -> Result<DirHandle, PublishError> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::Storage::FileSystem::{
            CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_READ, FILE_SHARE_WRITE,
            OPEN_EXISTING,
        };
        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE || handle.is_null() {
            return Err(PublishError::io(io::Error::last_os_error()));
        }
        Ok(DirHandle {
            handle,
            created_for_rollback: false,
        })
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Err(PublishError::Io("unsupported platform".into()))
    }
}

/// Open a folder READ-ONLY (list + attributes) so a read-only source still works. Children can
/// not be created through this handle. The folder cannot be renamed or replaced while it is
/// held (no delete sharing). Does not look at reparse points: callers prove the folder with
/// [`final_path`] and its ids.
#[allow(dead_code)]
pub fn open_root_read(path: &Path) -> Result<DirHandle, PublishError> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
        use windows_sys::Win32::Storage::FileSystem::{
            CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, SYNCHRONIZE,
        };
        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE || handle.is_null() {
            return Err(PublishError::io(io::Error::last_os_error()));
        }
        Ok(DirHandle {
            handle,
            created_for_rollback: false,
        })
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Err(PublishError::Io("unsupported platform".into()))
    }
}

/// Is the object behind `dir` a directory? ([`open_root`] opens a plain file just as well.)
#[allow(dead_code)]
pub fn is_directory(dir: &DirHandle) -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY, GetFileInformationByHandle,
        };
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: valid handle, writable out structure.
        let ok = unsafe { GetFileInformationByHandle(dir.handle, &mut info) };
        ok != 0 && info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0
    }
    #[cfg(not(windows))]
    {
        let _ = dir;
        false
    }
}

/// A file held open for observation (facts and final path), sharing delete, so the same
/// object can be asked where it ended up after something else moved or removed it.
#[cfg(windows)]
pub struct FileProbe {
    handle: windows_sys::Win32::Foundation::HANDLE,
}

#[cfg(windows)]
unsafe impl Send for FileProbe {}

#[cfg(windows)]
impl Drop for FileProbe {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.handle);
        }
    }
}

#[cfg(windows)]
impl FileProbe {
    /// Is the object marked for deletion (a classic, non-POSIX delete on FAT/exFAT leaves the
    /// name in place while a handle like this one is open)?
    pub fn delete_pending(&self) -> bool {
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_STANDARD_INFO, FileStandardInfo, GetFileInformationByHandleEx,
        };
        let mut info: FILE_STANDARD_INFO = unsafe { std::mem::zeroed() };
        // SAFETY: valid handle; `info` is a writable out structure of the given size.
        let ok = unsafe {
            GetFileInformationByHandleEx(
                self.handle,
                FileStandardInfo,
                &mut info as *mut _ as *mut core::ffi::c_void,
                std::mem::size_of::<FILE_STANDARD_INFO>() as u32,
            )
        };
        // Unknown counts as pending: the caller treats it as "removed for good".
        ok == 0 || info.DeletePending
    }

    /// File attributes of the object behind the handle.
    pub fn attributes(&self) -> io::Result<u32> {
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
        };
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: valid handle, writable out structure.
        if unsafe { GetFileInformationByHandle(self.handle, &mut info) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(info.dwFileAttributes)
    }

    /// Size and last-write time (unix ms) of the object behind the handle.
    pub fn facts(&self) -> io::Result<(u64, i64)> {
        handle_facts(self.handle)
    }

    /// The path the OS reports for the object NOW (`None`: it has no path any more, for example
    /// because it was deleted).
    pub fn final_path(&self) -> Option<String> {
        use windows_sys::Win32::Storage::FileSystem::GetFinalPathNameByHandleW;
        let mut buf = vec![0u16; 1024];
        loop {
            // SAFETY: valid handle; `buf` is writable for `buf.len()` u16s.
            let n = unsafe {
                GetFinalPathNameByHandleW(self.handle, buf.as_mut_ptr(), buf.len() as u32, 0)
            } as usize;
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

/// Is the file a cloud placeholder whose content is not on this machine (OneDrive Files
/// On-Demand and the like: OFFLINE / RECALL_ON_OPEN / RECALL_ON_DATA_ACCESS)? Reads attributes
/// only; nothing is hydrated. Unknown answers `true` (fail safe: the caller then reads the bytes).
#[cfg(windows)]
#[allow(dead_code)]
pub fn is_offline_placeholder(parent: &DirHandle, name: &OsStr) -> bool {
    const OFFLINE: u32 = 0x1000;
    const RECALL_ON_OPEN: u32 = 0x4_0000;
    const RECALL_ON_DATA_ACCESS: u32 = 0x40_0000;
    match open_file_probe(parent, name).and_then(|p| p.attributes().map_err(PublishError::io)) {
        Ok(attrs) => attrs & (OFFLINE | RECALL_ON_OPEN | RECALL_ON_DATA_ACCESS) != 0,
        Err(_) => true,
    }
}

#[cfg(not(windows))]
#[allow(dead_code)]
pub fn is_offline_placeholder(_parent: &DirHandle, _name: &OsStr) -> bool {
    true
}

/// Open the file `name` below `parent` for observation (no-follow; a reparse point is
/// `Err(Reparse)`, a directory is an error).
#[cfg(windows)]
#[allow(dead_code)]
pub fn open_file_probe(parent: &DirHandle, name: &OsStr) -> Result<FileProbe, PublishError> {
    let handle = open_windows(name, OpenKind::ExistingFileProbe, Some(parent.handle))?;
    match is_link(handle) {
        Ok(false) => Ok(FileProbe { handle }),
        Ok(true) => {
            unsafe {
                windows_sys::Win32::Foundation::CloseHandle(handle);
            }
            Err(PublishError::Reparse)
        }
        Err(e) => {
            unsafe {
                windows_sys::Win32::Foundation::CloseHandle(handle);
            }
            Err(e)
        }
    }
}

/// Check on the facts of a file as read from its OWN handle: `(size, last write in unix ms)`.
pub type FactsCheck<'a> = &'a dyn Fn(u64, i64) -> bool;

/// Delete the file `name` below `parent` through its own handle (a read-only attribute does not
/// stop it). A reparse point is never followed or deleted: `Err(Reparse)`.
#[allow(dead_code)]
pub fn delete_file(parent: &DirHandle, name: &OsStr) -> Result<(), PublishError> {
    delete_file_if(parent, name, &|_, _| true).map(|_| ())
}

/// [`delete_file`] that first reads the size and last-write time from the very handle it is about
/// to dispose and asks `check`. `Ok(false)` = the file is not what the caller expected: nothing
/// was deleted. The check and the delete act on the same object, so a file replaced or edited
/// after the caller's own look is never deleted by mistake (just-in-time check without a gap).
#[allow(dead_code)]
pub fn delete_file_if(
    parent: &DirHandle,
    name: &OsStr,
    check: FactsCheck<'_>,
) -> Result<bool, PublishError> {
    #[cfg(windows)]
    {
        let handle = open_windows(name, OpenKind::ExistingFileRemove, Some(parent.handle))?;
        let close = |h| unsafe {
            windows_sys::Win32::Foundation::CloseHandle(h);
        };
        match is_link(handle) {
            Ok(false) => {}
            Ok(true) => {
                close(handle);
                return Err(PublishError::Reparse);
            }
            Err(e) => {
                close(handle);
                return Err(e);
            }
        }
        match handle_facts(handle) {
            Ok((size, ms)) if check(size, ms) => {}
            Ok(_) => {
                close(handle);
                return Ok(false);
            }
            Err(e) => {
                close(handle);
                return Err(PublishError::io(e));
            }
        }
        let result = set_disposition(handle, true);
        close(handle);
        result.map(|()| true).map_err(PublishError::io)
    }
    #[cfg(not(windows))]
    {
        let _ = (parent, name, check);
        Err(PublishError::Io("unsupported platform".into()))
    }
}

/// How [`replace_file_checked`] ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Replaced {
    /// The staged file is now at the destination; the old file is gone.
    Done,
    /// The destination is not what the caller expected (other size or time, a folder, a link, or
    /// gone): nothing was changed.
    Changed,
}

/// FER-564 (P0.2 review P3-2): publish `staged` over the existing file `dest` ONLY if `dest` still
/// is what `check` accepts, with the check made on a handle that is held until the swap is done.
///
/// 1. `dest` is opened (no-follow) for DELETE with a share mode that denies every other writer,
///    deleter and renamer. A program that has the file open for writing makes this fail with a
///    sharing violation (the user's file is left alone and the caller may retry later).
/// 2. Its size and last-write time are read from that same handle and given to `check`.
/// 3. The held handle renames the old file to `backup` (same folder, never replacing), then
///    `staged` is renamed to `dest` WITHOUT replacing. If that fails, the old file is renamed back
///    (if a newcomer took its name, the old file stays at `backup`: it is never overwritten or
///    deleted in that case).
/// 4. Only after the new file is in place is the old one deleted through the held handle.
///
/// Nothing in here can replace a file that was edited after the check: no writer can open the
/// file between the check and the move, and the destination name is never overwritten.
#[cfg(windows)]
pub fn replace_file_checked(
    staged: &Path,
    dest: &Path,
    backup: &Path,
    check: FactsCheck<'_>,
) -> io::Result<Replaced> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, DELETE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
        FILE_SHARE_READ, OPEN_EXISTING,
    };
    let wide = |p: &Path| -> Vec<u16> { p.as_os_str().encode_wide().chain(Some(0)).collect() };
    let dest_w = wide(dest);
    // SAFETY: NUL-terminated buffer that outlives the call; the handle is closed below.
    let handle = unsafe {
        CreateFileW(
            dest_w.as_ptr(),
            DELETE | FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        let e = io::Error::last_os_error();
        // The destination vanished: that is a change, not an error.
        if matches!(e.raw_os_error(), Some(2 | 3)) {
            return Ok(Replaced::Changed);
        }
        return Err(e);
    }
    let close = |h| unsafe {
        CloseHandle(h);
    };
    // Kind of object, from the held handle.
    {
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
        };
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: valid handle, writable out structure.
        if unsafe { GetFileInformationByHandle(handle, &mut info) } == 0 {
            let e = io::Error::last_os_error();
            close(handle);
            return Err(e);
        }
        if info.dwFileAttributes & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
            close(handle);
            return Ok(Replaced::Changed);
        }
    }
    match handle_facts(handle) {
        Ok((size, ms)) if check(size, ms) => {}
        Ok(_) => {
            close(handle);
            return Ok(Replaced::Changed);
        }
        Err(e) => {
            close(handle);
            return Err(e);
        }
    }
    // Old file aside (by handle), new file in (no replace).
    if let Err(e) = rename_by_handle(handle, backup) {
        close(handle);
        return Err(e);
    }
    match move_no_replace(staged, dest) {
        Ok(()) => {
            // The new file is published: drop the old one. A failure here must not turn a valid
            // result into a failed copy; the stale backup is ignored by sync (SP-4.4).
            let _ = set_disposition(handle, true);
            close(handle);
            Ok(Replaced::Done)
        }
        Err(e) => {
            // Put the old file back; if its name was taken meanwhile it stays at `backup`.
            let _ = rename_by_handle(handle, dest);
            close(handle);
            Err(e)
        }
    }
}

#[cfg(not(windows))]
pub fn replace_file_checked(
    _staged: &Path,
    _dest: &Path,
    _backup: &Path,
    _check: FactsCheck<'_>,
) -> io::Result<Replaced> {
    Err(io::Error::other("unsupported platform"))
}

/// `MoveFileExW` without `MOVEFILE_REPLACE_EXISTING`: an existing destination is an error.
#[cfg(windows)]
fn move_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::MoveFileExW;
    let wide = |p: &Path| -> Vec<u16> { p.as_os_str().encode_wide().chain(Some(0)).collect() };
    let (a, b) = (wide(from), wide(to));
    // SAFETY: NUL-terminated buffers that outlive the call; flags 0 = no REPLACE_EXISTING.
    if unsafe { MoveFileExW(a.as_ptr(), b.as_ptr(), 0) } != 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Rename the object behind `handle` to the full path `to`, never replacing an existing one.
#[cfg(windows)]
fn rename_by_handle(handle: windows_sys::Win32::Foundation::HANDLE, to: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_RENAME_INFO, FileRenameInfo, SetFileInformationByHandle,
    };
    let name: Vec<u16> = to.as_os_str().encode_wide().collect();
    let header = std::mem::size_of::<FILE_RENAME_INFO>();
    let bytes = header + name.len() * 2;
    // A u64 buffer keeps the variable-length structure aligned.
    let mut buffer = vec![0u64; bytes.div_ceil(8)];
    let info = buffer.as_mut_ptr() as *mut FILE_RENAME_INFO;
    // SAFETY: `buffer` is zeroed, aligned and large enough for the fixed part plus the name,
    // which is copied over `FileName` and the bytes behind it. ReplaceIfExists stays FALSE.
    unsafe {
        (*info).FileNameLength = (name.len() * 2) as u32;
        let at = std::ptr::addr_of_mut!((*info).FileName) as *mut u16;
        std::ptr::copy_nonoverlapping(name.as_ptr(), at, name.len());
    }
    // SAFETY: valid handle; `info` points at `bytes` initialised bytes.
    let ok = unsafe {
        SetFileInformationByHandle(
            handle,
            FileRenameInfo,
            info as *const core::ffi::c_void,
            bytes as u32,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Size and last-write time (unix ms) of an open handle.
#[cfg(windows)]
fn handle_facts(handle: windows_sys::Win32::Foundation::HANDLE) -> io::Result<(u64, i64)> {
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: valid handle, writable out structure.
    if unsafe { GetFileInformationByHandle(handle, &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let size = (u64::from(info.nFileSizeHigh) << 32) | u64::from(info.nFileSizeLow);
    let ticks100 = (u64::from(info.ftLastWriteTime.dwHighDateTime) << 32)
        | u64::from(info.ftLastWriteTime.dwLowDateTime);
    const EPOCH_DIFF: u64 = 11_644_473_600; // 1601 -> 1970 seconds
    let ms = ((ticks100 / 10_000_000).saturating_sub(EPOCH_DIFF)) as i64 * 1000
        + ((ticks100 % 10_000_000) / 10_000) as i64;
    Ok((size, ms))
}

/// Size and last-write time (unix ms) of the file `name` below `parent`, read from its own
/// handle (no-follow; a reparse point is an error).
#[allow(dead_code)]
pub fn file_facts(parent: &DirHandle, name: &OsStr) -> Result<(u64, i64), PublishError> {
    #[cfg(windows)]
    {
        let handle = open_windows(name, OpenKind::ExistingFileRead, Some(parent.handle))?;
        let reparse = is_link(handle);
        let facts = handle_facts(handle);
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(handle);
        }
        if reparse? {
            return Err(PublishError::Reparse);
        }
        facts.map_err(PublishError::io)
    }
    #[cfg(not(windows))]
    {
        let _ = (parent, name);
        Err(PublishError::Io("unsupported platform".into()))
    }
}

/// Size of the file `name` below `parent` (no-follow; a reparse point is an error).
#[allow(dead_code)]
pub fn file_size(parent: &DirHandle, name: &OsStr) -> Result<u64, PublishError> {
    file_facts(parent, name).map(|(size, _)| size)
}

/// Rename the file `src_name` below `src_parent` to `dst_name` below `dst_parent`, both resolved
/// by handle without following reparse points. The destination is never replaced
/// (`MoveError::Exists`). Different volumes: `MoveError::CrossDevice`, nothing changed.
#[allow(dead_code)]
pub fn rename_file_into(
    src_parent: &DirHandle,
    src_name: &OsStr,
    dst_parent: &DirHandle,
    dst_name: &OsStr,
) -> Result<(), MoveError> {
    rename_file_into_if(src_parent, src_name, dst_parent, dst_name, &|_, _| true).map(|_| ())
}

/// [`rename_file_into`] that checks the source's size and last-write time on the handle it
/// renames. `Ok(false)` = the file is not what the caller expected: nothing was moved.
#[allow(dead_code)]
pub fn rename_file_into_if(
    src_parent: &DirHandle,
    src_name: &OsStr,
    dst_parent: &DirHandle,
    dst_name: &OsStr,
    check: FactsCheck<'_>,
) -> Result<bool, MoveError> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Wdk::Storage::FileSystem::{
            FILE_RENAME_INFORMATION, FileRenameInformation, NtSetInformationFile,
        };
        use windows_sys::Win32::Foundation::STATUS_SUCCESS;
        use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

        let handle = open_windows(
            src_name,
            OpenKind::ExistingFileRemove,
            Some(src_parent.handle),
        )
        .map_err(MoveError::Publish)?;
        let close = |h| unsafe {
            windows_sys::Win32::Foundation::CloseHandle(h);
        };
        match is_link(handle) {
            Ok(false) => {}
            Ok(true) => {
                close(handle);
                return Err(MoveError::Publish(PublishError::Reparse));
            }
            Err(e) => {
                close(handle);
                return Err(MoveError::Publish(e));
            }
        }
        match handle_facts(handle) {
            Ok((size, ms)) if check(size, ms) => {}
            Ok(_) => {
                close(handle);
                return Ok(false);
            }
            Err(e) => {
                close(handle);
                return Err(MoveError::Publish(PublishError::io(e)));
            }
        }
        let wide: Vec<u16> = dst_name.encode_wide().collect();
        if wide.is_empty()
            || wide.len() > 255
            || wide
                .iter()
                .any(|&ch| ch == u16::from(b'\\') || ch == u16::from(b'/'))
        {
            close(handle);
            return Err(MoveError::Publish(PublishError::Io("bad file name".into())));
        }
        // FILE_RENAME_INFORMATION is a variable-length structure; a u64 buffer keeps it aligned.
        let header = std::mem::size_of::<FILE_RENAME_INFORMATION>();
        let bytes = header + wide.len() * 2;
        let mut buffer = vec![0u64; bytes.div_ceil(8)];
        let info = buffer.as_mut_ptr() as *mut FILE_RENAME_INFORMATION;
        // SAFETY: `buffer` is zeroed, 8-byte aligned and large enough for the fixed part plus
        // the name, which is copied over `FileName` and the bytes behind it.
        unsafe {
            // ReplaceIfExists = FALSE (the union stays zeroed): never replace.
            (*info).RootDirectory = dst_parent.handle;
            (*info).FileNameLength = (wide.len() * 2) as u32;
            let name_at = std::ptr::addr_of_mut!((*info).FileName) as *mut u16;
            std::ptr::copy_nonoverlapping(wide.as_ptr(), name_at, wide.len());
        }
        let mut ios: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };
        let status = unsafe {
            NtSetInformationFile(
                handle,
                &mut ios,
                info as *const core::ffi::c_void,
                bytes as u32,
                FileRenameInformation,
            )
        };
        close(handle);
        if status == STATUS_SUCCESS {
            return Ok(true);
        }
        Err(match status as u32 {
            0xC000_00D4 => MoveError::CrossDevice,
            0xC000_0035 => MoveError::Exists,
            0xC000_0034 | 0xC000_003A => MoveError::Publish(PublishError::NotFound),
            other => MoveError::Publish(PublishError::Io(format!(
                "NtCreateFile failed: {other:#010x}"
            ))),
        })
    }
    #[cfg(not(windows))]
    {
        let _ = (src_parent, src_name, dst_parent, dst_name, check);
        Err(MoveError::Publish(PublishError::Io(
            "unsupported platform".into(),
        )))
    }
}

/// POSIX-style delete of an open file or EMPTY directory through its handle. A non-empty
/// directory fails with ERROR_DIR_NOT_EMPTY and is left untouched.
#[cfg(windows)]
fn set_disposition(
    handle: windows_sys::Win32::Foundation::HANDLE,
    ignore_readonly: bool,
) -> io::Result<()> {
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_DISPOSITION_FLAG_DELETE, FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE,
        FILE_DISPOSITION_FLAG_POSIX_SEMANTICS, FILE_DISPOSITION_INFO, FILE_DISPOSITION_INFO_EX,
        FileDispositionInfo, FileDispositionInfoEx, SetFileInformationByHandle,
    };
    let mut flags = FILE_DISPOSITION_FLAG_DELETE | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS;
    if ignore_readonly {
        flags |= FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE;
    }
    let info = FILE_DISPOSITION_INFO_EX { Flags: flags };
    let ok = unsafe {
        SetFileInformationByHandle(
            handle,
            FileDispositionInfoEx,
            &info as *const _ as *const core::ffi::c_void,
            std::mem::size_of::<FILE_DISPOSITION_INFO_EX>() as u32,
        )
    };
    if ok != 0 {
        return Ok(());
    }
    let err = io::Error::last_os_error();
    // Windows before 1809 does not know the extended disposition: classic delete-on-close
    // (no read-only override there, so a read-only file fails closed with access denied).
    if !matches!(err.raw_os_error(), Some(1 | 50 | 87)) {
        return Err(err);
    }
    let classic = FILE_DISPOSITION_INFO { DeleteFile: true };
    let ok = unsafe {
        SetFileInformationByHandle(
            handle,
            FileDispositionInfo,
            &classic as *const _ as *const core::ffi::c_void,
            std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

// ── Windows implementation ──────────────────────────────────────────────

#[cfg(windows)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum OpenKind {
    ExistingDir,
    NewDir,
    NewFile,
    ExistingFileTruncate,
    ExistingFileRead,
    /// Existing directory opened with DELETE, WITHOUT following a reparse point (sync engine
    /// removal and extraction rollback).
    ExistingDirRemove,
    /// Existing file opened with DELETE (delete or rename through the handle), WITHOUT
    /// following a reparse point (sync engine and extraction rollback).
    ExistingFileRemove,
    /// Sync engine: existing file held open only to read its facts and its final path, sharing
    /// DELETE so the shell can move it meanwhile. No reparse point is followed.
    ExistingFileProbe,
}

#[cfg(windows)]
const FILE_DELETE_ACCESS: u32 = 0x0001_0000; // DELETE, not exported by windows-sys

/// Builds the `UNICODE_STRING` for a relative NtCreateFile name.
///
/// The returned `Vec<u16>` owns the buffer that `UNICODE_STRING::Buffer` points into, so the
/// caller must keep it alive until the NT call returns. Previously only the `UNICODE_STRING`
/// was returned and the `Vec` was dropped when this function returned, leaving `Buffer`
/// dangling (use-after-free, fixed in FER-505).
#[cfg(windows)]
fn nt(
    name: &OsStr,
) -> Result<(Vec<u16>, windows_sys::Win32::Foundation::UNICODE_STRING), PublishError> {
    use std::os::windows::ffi::OsStrExt;
    let mut wide: Vec<u16> = name.encode_wide().collect();
    if wide.len() > 32_000 {
        return Err(PublishError::Io("name too long".into()));
    }
    // Relative NtCreateFile names must not start with a separator.
    let sep = u16::from(b'\\');
    while wide.first() == Some(&sep) {
        wide.remove(0);
    }
    let len = wide.len() * 2;
    let uni = windows_sys::Win32::Foundation::UNICODE_STRING {
        Length: len as u16,
        MaximumLength: len as u16,
        Buffer: wide.as_mut_ptr(),
    };
    Ok((wide, uni))
}

#[cfg(windows)]
fn open_windows(
    name: &OsStr,
    kind: OpenKind,
    parent: Option<windows_sys::Win32::Foundation::HANDLE>,
) -> Result<windows_sys::Win32::Foundation::HANDLE, PublishError> {
    use windows_sys::Wdk::Storage::FileSystem::{
        FILE_CREATE, FILE_NON_DIRECTORY_FILE, FILE_OPEN, FILE_OPEN_FOR_BACKUP_INTENT,
        FILE_OPEN_REPARSE_POINT, FILE_OVERWRITE, FILE_SYNCHRONOUS_IO_NONALERT, NtCreateFile,
    };
    use windows_sys::Win32::Foundation::{OBJ_CASE_INSENSITIVE, STATUS_SUCCESS};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_NORMAL, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_SHARE_READ,
        FILE_SHARE_WRITE, FILE_WRITE_DATA, SYNCHRONIZE,
    };
    use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

    // `_wide_buf` must outlive the NtCreateFile call below: `uni.Buffer` points into it.
    let (_wide_buf, uni) = nt(name)?;
    let oa = windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES {
        Length: std::mem::size_of::<windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: parent.unwrap_or(std::ptr::null_mut()),
        ObjectName: &uni as *const _,
        Attributes: OBJ_CASE_INSENSITIVE,
        ..Default::default()
    };

    const FILE_DIRECTORY_FILE: u32 = windows_sys::Wdk::Storage::FileSystem::FILE_DIRECTORY_FILE;
    let (access, disposition, options, attrs) = match kind {
        OpenKind::ExistingDir | OpenKind::NewDir => {
            let disposition = if kind == OpenKind::NewDir {
                FILE_CREATE
            } else {
                FILE_OPEN
            };
            (
                FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
                disposition,
                FILE_DIRECTORY_FILE
                    | FILE_SYNCHRONOUS_IO_NONALERT
                    | FILE_OPEN_REPARSE_POINT
                    | FILE_OPEN_FOR_BACKUP_INTENT,
                0u32,
            )
        }
        OpenKind::NewFile => (
            FILE_WRITE_DATA
                | windows_sys::Win32::Storage::FileSystem::FILE_READ_ATTRIBUTES
                | SYNCHRONIZE
                | FILE_DELETE_ACCESS,
            FILE_CREATE,
            FILE_NON_DIRECTORY_FILE
                | FILE_SYNCHRONOUS_IO_NONALERT
                | FILE_OPEN_REPARSE_POINT
                | FILE_OPEN_FOR_BACKUP_INTENT,
            FILE_ATTRIBUTE_NORMAL,
        ),
        OpenKind::ExistingFileTruncate => (
            FILE_WRITE_DATA
                | windows_sys::Win32::Storage::FileSystem::FILE_READ_ATTRIBUTES
                | SYNCHRONIZE
                | FILE_DELETE_ACCESS,
            FILE_OVERWRITE,
            FILE_NON_DIRECTORY_FILE
                | FILE_SYNCHRONOUS_IO_NONALERT
                | FILE_OPEN_REPARSE_POINT
                | FILE_OPEN_FOR_BACKUP_INTENT,
            FILE_ATTRIBUTE_NORMAL,
        ),
        OpenKind::ExistingFileRead => (
            // Synchronous options require SYNCHRONIZE in the access mask.
            windows_sys::Win32::Storage::FileSystem::FILE_READ_ATTRIBUTES | SYNCHRONIZE,
            FILE_OPEN,
            FILE_NON_DIRECTORY_FILE
                | FILE_SYNCHRONOUS_IO_NONALERT
                | FILE_OPEN_REPARSE_POINT
                | FILE_OPEN_FOR_BACKUP_INTENT,
            FILE_ATTRIBUTE_NORMAL,
        ),
        OpenKind::ExistingDirRemove => (
            FILE_DELETE_ACCESS | windows_sys::Win32::Storage::FileSystem::FILE_READ_ATTRIBUTES,
            FILE_OPEN,
            FILE_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_OPEN_FOR_BACKUP_INTENT,
            0u32,
        ),
        OpenKind::ExistingFileRemove => (
            FILE_DELETE_ACCESS | windows_sys::Win32::Storage::FileSystem::FILE_READ_ATTRIBUTES,
            FILE_OPEN,
            FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_OPEN_FOR_BACKUP_INTENT,
            FILE_ATTRIBUTE_NORMAL,
        ),
        OpenKind::ExistingFileProbe => (
            windows_sys::Win32::Storage::FileSystem::FILE_READ_ATTRIBUTES,
            FILE_OPEN,
            FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_OPEN_FOR_BACKUP_INTENT,
            FILE_ATTRIBUTE_NORMAL,
        ),
    };
    let share = if kind == OpenKind::ExistingFileProbe {
        FILE_SHARE_READ
            | FILE_SHARE_WRITE
            | windows_sys::Win32::Storage::FileSystem::FILE_SHARE_DELETE
    } else {
        FILE_SHARE_READ | FILE_SHARE_WRITE
    };

    let mut handle: windows_sys::Win32::Foundation::HANDLE = std::ptr::null_mut();
    let mut ios: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };
    let status = unsafe {
        NtCreateFile(
            &mut handle,
            access,
            &oa,
            &mut ios,
            std::ptr::null_mut(),
            attrs,
            share,
            disposition,
            options,
            std::ptr::null(),
            0,
        )
    };
    if status != STATUS_SUCCESS {
        if std::env::var_os("FERRUM_PUBLISH_DEBUG").is_some() {
            eprintln!("NtCreateFile fail kind={kind:?} name={name:?} status={status:#010x}");
        }
        return Err(match status as u32 {
            0xC000_0034 | 0xC000_003A => PublishError::NotFound,
            0xC000_0035 => PublishError::Exists,
            0xC000_00A5 | 0xC000_00AD => PublishError::WrongKind,
            0xC000_00BA => PublishError::Io("file is mapped".into()),
            other => PublishError::Io(format!("NtCreateFile failed: {other:#010x}")),
        });
    }
    Ok(handle)
}

/// True when the handle points at a reparse point (symlink/junction).
#[cfg(windows)]
fn is_reparse(handle: windows_sys::Win32::Foundation::HANDLE) -> Result<bool, PublishError> {
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_TAG_INFO, FileAttributeTagInfo, GetFileInformationByHandleEx,
    };
    let mut tag: FILE_ATTRIBUTE_TAG_INFO = unsafe { std::mem::zeroed() };
    let ok = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileAttributeTagInfo,
            &mut tag as *const _ as *mut core::ffi::c_void,
            std::mem::size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
        )
    };
    if ok == 0 {
        return Err(PublishError::io(io::Error::last_os_error()));
    }
    Ok(
        tag.FileAttributes & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
            != 0,
    )
}

/// A reparse tag with the name-surrogate bit (symlink, junction/mount point) is a LINK; any other
/// reparse data (cloud placeholders such as OneDrive, deduplication, ...) is an ordinary file or
/// folder as far as the sync engine is concerned.
pub fn tag_is_link(tag: u32) -> bool {
    tag & 0x2000_0000 != 0
}

/// True when the handle points at a link (name-surrogate reparse point). Unlike `is_reparse` a
/// cloud placeholder or deduplicated file is NOT a link; asking costs no hydration.
#[cfg(windows)]
fn is_link(handle: windows_sys::Win32::Foundation::HANDLE) -> Result<bool, PublishError> {
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_TAG_INFO, FileAttributeTagInfo,
        GetFileInformationByHandleEx,
    };
    let mut tag: FILE_ATTRIBUTE_TAG_INFO = unsafe { std::mem::zeroed() };
    let ok = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileAttributeTagInfo,
            &mut tag as *const _ as *mut core::ffi::c_void,
            std::mem::size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
        )
    };
    if ok == 0 {
        return Err(PublishError::io(io::Error::last_os_error()));
    }
    Ok(tag.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 && tag_is_link(tag.ReparseTag))
}

/// [`child_dir`] that rejects only LINKS (see [`tag_is_link`]), not other reparse data.
#[cfg(windows)]
#[allow(dead_code)]
pub fn child_dir_link_aware(parent: &DirHandle, name: &OsStr) -> Result<DirHandle, PublishError> {
    let handle = open_windows(name, OpenKind::ExistingDir, Some(parent.handle))?;
    if is_link(handle)? {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(handle);
        }
        return Err(PublishError::Reparse);
    }
    Ok(DirHandle {
        handle,
        created_for_rollback: false,
    })
}

#[cfg(not(windows))]
#[allow(dead_code)]
pub fn child_dir_link_aware(parent: &DirHandle, name: &OsStr) -> Result<DirHandle, PublishError> {
    child_dir(parent, name)
}

/// File attributes of the object behind an open handle.
#[cfg(windows)]
fn handle_attributes(handle: windows_sys::Win32::Foundation::HANDLE) -> io::Result<u32> {
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: valid handle, writable out structure.
    if unsafe { GetFileInformationByHandle(handle, &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(info.dwFileAttributes)
}

/// Identity of the object behind an open handle: volume serial plus the 128-bit file id
/// (`FileIdInfo`; file systems without it fall back to the 64-bit index of the handle info).
#[cfg(windows)]
fn object_id(handle: windows_sys::Win32::Foundation::HANDLE) -> io::Result<(u64, [u8; 16])> {
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_ID_INFO, FileIdInfo, GetFileInformationByHandle,
        GetFileInformationByHandleEx,
    };
    let mut id: FILE_ID_INFO = unsafe { std::mem::zeroed() };
    // SAFETY: valid handle; `id` is a writable out structure of the given size.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileIdInfo,
            &mut id as *mut _ as *mut core::ffi::c_void,
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        )
    };
    if ok != 0 {
        return Ok((id.VolumeSerialNumber, id.FileId.Identifier));
    }
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: valid handle, writable out structure.
    if unsafe { GetFileInformationByHandle(handle, &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut file_id = [0u8; 16];
    let index = (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow);
    file_id[..8].copy_from_slice(&index.to_le_bytes());
    Ok((u64::from(info.dwVolumeSerialNumber), file_id))
}

// ── Unix implementation ─────────────────────────────────────────────────

#[cfg(unix)]
#[derive(Clone, Copy)]
enum OpenKind {
    ExistingDir,
    NewDir,
    NewFile,
    ExistingFileTruncate,
    ExistingFileRead,
}

#[cfg(unix)]
fn open_unix(
    name: &OsStr,
    kind: OpenKind,
    parent: Option<std::os::unix::io::RawFd>,
) -> Result<std::os::unix::io::RawFd, PublishError> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(name.as_bytes())
        .map_err(|_| PublishError::Io("NUL in name".into()))?;
    let dirfd = parent.unwrap_or(libc::AT_FDCWD);
    let (flags, mkdir) = match kind {
        OpenKind::ExistingDir => (libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW, false),
        OpenKind::NewDir => (libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW, true),
        OpenKind::NewFile => (
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            false,
        ),
        OpenKind::ExistingFileTruncate => {
            (libc::O_WRONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC, false)
        }
        OpenKind::ExistingFileRead => (libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC, false),
    };
    if mkdir {
        let rc = unsafe { libc::mkdirat(dirfd, c.as_ptr(), 0o700) };
        if rc != 0 && errno() != libc::EEXIST {
            return Err(PublishError::io(io::Error::last_os_error()));
        }
    }
    let mut flags = flags;
    if kind == OpenKind::ExistingFileTruncate {
        flags |= libc::O_TRUNC;
    }
    let fd = unsafe { libc::openat(dirfd, c.as_ptr(), flags, 0o600) };
    if fd < 0 {
        return Err(PublishError::io(io::Error::last_os_error()));
    }
    if is_reparse(fd)? {
        unsafe { libc::close(fd) };
        return Err(PublishError::Reparse);
    }
    Ok(fd)
}

#[cfg(unix)]
fn errno() -> i32 {
    io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

#[cfg(unix)]
fn is_reparse(fd: std::os::unix::io::RawFd) -> Result<bool, PublishError> {
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd, &mut stat) } != 0 {
        return Err(PublishError::io(io::Error::last_os_error()));
    }
    Ok(libc::S_ISLNK(stat.st_mode))
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::fs;

    fn make_junction(link: &Path, target: &Path) {
        let ok = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        assert!(ok, "mklink /J failed (junctions need no privilege)");
    }

    fn name(s: &str) -> &OsStr {
        OsStr::new(s)
    }

    #[test]
    fn open_root_accepts_a_real_directory_and_creates_children() {
        let tmp = tempfile::tempdir().unwrap();
        let root = open_root(tmp.path()).unwrap();
        let child = create_dir_new(&root, name("kid")).unwrap();
        drop(child);
        assert!(tmp.path().join("kid").is_dir());
    }

    #[test]
    fn open_root_rejects_a_junction_root() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        fs::create_dir(&real).unwrap();
        let link = tmp.path().join("link");
        make_junction(&link, &real);
        assert!(matches!(open_root(&link), Err(PublishError::Reparse)));
        // Nothing was created through the link.
        assert_eq!(fs::read_dir(&real).unwrap().count(), 0);
    }

    #[test]
    fn open_root_rejects_a_plain_file_root() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("f.txt");
        fs::write(&file, b"x").unwrap();
        assert!(matches!(open_root(&file), Err(PublishError::WrongKind)));
    }

    #[test]
    fn open_root_reports_a_missing_root_as_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(matches!(
            open_root(&tmp.path().join("nope")),
            Err(PublishError::NotFound)
        ));
    }

    #[test]
    fn unlink_entry_really_removes_an_empty_directory_and_a_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = open_root(tmp.path()).unwrap();
        drop(create_dir_new(&root, name("d")).unwrap());
        drop(create_file_new(&root, name("f.bin")).unwrap());
        assert!(tmp.path().join("d").is_dir());
        assert!(tmp.path().join("f.bin").is_file());
        unlink_entry(&root, name("d"), true).unwrap();
        unlink_entry(&root, name("f.bin"), false).unwrap();
        assert!(!tmp.path().join("d").exists());
        assert!(!tmp.path().join("f.bin").exists());
    }

    #[test]
    fn unlink_entry_leaves_a_non_empty_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let root = open_root(tmp.path()).unwrap();
        fs::create_dir(tmp.path().join("d")).unwrap();
        fs::write(tmp.path().join("d").join("keep.txt"), b"keep").unwrap();
        assert!(unlink_entry(&root, name("d"), true).is_err());
        assert_eq!(
            fs::read(tmp.path().join("d").join("keep.txt")).unwrap(),
            b"keep"
        );
    }

    #[test]
    fn unlink_entry_does_not_follow_a_junction_planted_in_the_staging_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tmp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        let staging = tmp.path().join("staging");
        fs::create_dir(&staging).unwrap();
        // The run "created" `d`; someone swapped it for a junction to an empty folder outside.
        make_junction(&staging.join("d"), &outside);
        let root = open_root(&staging).unwrap();
        assert!(matches!(
            unlink_entry(&root, name("d"), true),
            Err(PublishError::Reparse)
        ));
        // Neither the target nor the link was removed.
        assert!(outside.is_dir(), "junction target must survive");
        assert!(staging.join("d").exists(), "foreign link is left alone");
    }

    #[test]
    fn unlink_entry_keeps_the_contents_behind_a_junction() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tmp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("precious.txt"), b"data").unwrap();
        let staging = tmp.path().join("staging");
        fs::create_dir(&staging).unwrap();
        make_junction(&staging.join("d"), &outside);
        let root = open_root(&staging).unwrap();
        let _ = unlink_entry(&root, name("d"), true);
        let _ = unlink_entry(&root, name("d"), false);
        assert_eq!(fs::read(outside.join("precious.txt")).unwrap(), b"data");
    }
}
