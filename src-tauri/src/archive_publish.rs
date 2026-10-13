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

/// Open the extraction root. Rejects reparse-point roots: the caller chose a
/// real directory; a junction there means it no longer resolves as named.
#[allow(dead_code)]
pub fn open_root(path: &Path) -> Result<DirHandle, PublishError> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::Storage::FileSystem::{
            CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_READ, FILE_SHARE_WRITE,
            OPEN_EXISTING,
        };

        // Reparse check BEFORE open: opening with FILE_FLAG_OPEN_REPARSE_POINT
        // yields a filtered handle that cannot parent NtCreateFile children.
        use std::os::windows::fs::MetadataExt;
        let attrs = std::fs::metadata(path)
            .map(|m| m.file_attributes())
            .map_err(PublishError::io)?;
        if attrs & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(PublishError::Reparse);
        }

        let mut wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let handle = unsafe {
            CreateFileW(
                wide.as_mut_ptr(),
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
        let _ = &mut wide;
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

/// Delete an open file through its own descriptor (POSIX semantics on
/// Windows). The name disappears even if the path was swapped.
#[allow(dead_code)]
pub fn delete_open_file(file: std::fs::File) {
    #[cfg(windows)]
    {
        use windows_sys::Wdk::Storage::FileSystem::FILE_DISPOSITION_POSIX_SEMANTICS;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_DISPOSITION_INFO_EX, FileDispositionInfoEx, SetFileInformationByHandle,
        };
        let handle = std::os::windows::io::AsRawHandle::as_raw_handle(&file)
            as windows_sys::Win32::Foundation::HANDLE;
        let info = FILE_DISPOSITION_INFO_EX {
            Flags: FILE_DISPOSITION_POSIX_SEMANTICS,
        };
        let ok = unsafe {
            SetFileInformationByHandle(
                handle,
                FileDispositionInfoEx,
                &info as *const _ as *const core::ffi::c_void,
                std::mem::size_of::<FILE_DISPOSITION_INFO_EX>() as u32,
            )
        };
        if ok == 0 {
            // Pre-1809 fallback: classic disposition (delete on close).
            use windows_sys::Win32::Storage::FileSystem::{
                FILE_DISPOSITION_INFO, FileDispositionInfo,
            };
            let classic = FILE_DISPOSITION_INFO { DeleteFile: true };
            unsafe {
                SetFileInformationByHandle(
                    handle,
                    FileDispositionInfo,
                    &classic as *const _ as *const core::ffi::c_void,
                    std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
                );
            }
        }
        drop(file); // last handle closed; the name disappears
    }
    #[cfg(unix)]
    {
        // Unix rollback unlinks through the parent's fd (see unlink_entry);
        // closing alone leaves the name, so callers must call unlink_entry.
        drop(file);
    }
}

/// Unlink `name` via the parent's own descriptor (no path resolution).
#[allow(dead_code)]
pub fn unlink_entry(parent: &DirHandle, name: &OsStr, is_dir: bool) {
    #[cfg(windows)]
    {
        let kind = if is_dir {
            OpenKind::ExistingDirDelete
        } else {
            OpenKind::ExistingFileDelete
        };
        match open_windows(name, kind, Some(parent.handle)) {
            Ok(handle) => {
                if !matches!(is_reparse(handle), Ok(true)) {
                    dispose_via_handle(handle);
                } else {
                    unsafe {
                        windows_sys::Win32::Foundation::CloseHandle(handle);
                    }
                }
            }
            Err(e) => eprintln!(
                "unlink_entry open failed: {e:?} name={name:?} kind={kind:?} parent_null={}",
                parent.handle.is_null()
            ),
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let Ok(c) = std::ffi::CString::new(name.as_bytes()) else {
            return;
        };
        unsafe {
            libc::unlinkat(
                parent.fd,
                c.as_ptr(),
                if is_dir { libc::AT_REMOVEDIR } else { 0 },
            );
        }
    }
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
    /// Existing file opened with DELETE so POSIX disposition succeeds.
    ExistingFileDelete,
    /// Existing (empty) directory opened with DELETE.
    ExistingDirDelete,
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
        OpenKind::ExistingFileDelete => (
            // DELETE + READ_ATTRIBUTES (needed for the reparse check), but
            // WITHOUT SYNCHRONIZE/synchronous-options: that combination is
            // rejected as INVALID_PARAMETER by NtCreateFile.
            FILE_DELETE_ACCESS | windows_sys::Win32::Storage::FileSystem::FILE_READ_ATTRIBUTES,
            FILE_OPEN,
            FILE_NON_DIRECTORY_FILE,
            FILE_ATTRIBUTE_NORMAL,
        ),
        OpenKind::ExistingDirDelete => (
            FILE_DELETE_ACCESS | windows_sys::Win32::Storage::FileSystem::FILE_READ_ATTRIBUTES,
            FILE_OPEN,
            FILE_DIRECTORY_FILE,
            0u32,
        ),
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
            FILE_SHARE_READ | FILE_SHARE_WRITE,
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

/// POSIX-delete an open handle (file or empty directory).
#[cfg(windows)]
fn dispose_via_handle(handle: windows_sys::Win32::Foundation::HANDLE) {
    use windows_sys::Wdk::Storage::FileSystem::FILE_DISPOSITION_POSIX_SEMANTICS;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_DISPOSITION_INFO_EX, FileDispositionInfoEx, SetFileInformationByHandle,
    };
    let info = FILE_DISPOSITION_INFO_EX {
        Flags: FILE_DISPOSITION_POSIX_SEMANTICS,
    };
    let ok = unsafe {
        SetFileInformationByHandle(
            handle,
            FileDispositionInfoEx,
            &info as *const _ as *const core::ffi::c_void,
            std::mem::size_of::<FILE_DISPOSITION_INFO_EX>() as u32,
        )
    };
    if ok == 0 {
        eprintln!("dispose POSIX failed: {}", io::Error::last_os_error());
    }
    unsafe {
        windows_sys::Win32::Foundation::CloseHandle(handle);
    }
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
