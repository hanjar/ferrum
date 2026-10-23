//! FER-586: permanent delete of a folder tree, relative to open directory handles.
//!
//! This is the walker behind a permanent delete of a folder (`delete_jobs`). It does what std's
//! Windows `remove_dir_all` does, plus a cancel check and a progress callback per entry:
//!
//! - The top folder is opened once by path with `FILE_FLAG_OPEN_REPARSE_POINT`. Everything below it
//!   is opened with `NtCreateFile` **relative to the parent's handle** (`RootDirectory`), one name
//!   component at a time, with `FILE_OPEN_REPARSE_POINT` (and `OBJ_DONT_REPARSE` where supported). No
//!   path below the top folder is ever resolved by the walker, so a folder swapped for a junction, or
//!   turned into one in place (`FSCTL_SET_REPARSE_POINT` on a held, emptied folder: the CVE-2022-21658
//!   class), cannot redirect an open or a delete to the junction's target. For the in-place case the
//!   kernel refuses a name relative to a handle whose object became a reparse point
//!   (STATUS_REPARSE_POINT_NOT_RESOLVED); the walker then deletes that folder as the link it now is,
//!   through the handle it holds, and never lists the target.
//! - What an entry IS comes from its open handle (`FileAttributeTagInfo`), never from the
//!   enumeration: a name-surrogate reparse point (junction, symlink) is deleted as the link itself and
//!   never entered; only a real directory is listed.
//! - Listing reads the open handle (`GetFileInformationByHandleEx(FileIdBothDirectoryInfo)`).
//! - Deleting goes through the open handle: `FileDispositionInfoEx` with POSIX semantics and
//!   `IGNORE_READONLY_ATTRIBUTE`; where a file system does not support that (FAT, older systems) the
//!   classic `FileDispositionInfo`, after clearing the read-only attribute through a re-opened handle
//!   of the same object.
//! - Iterative: an explicit stack of open directories, so depth is bounded by memory, not the
//!   thread's stack.
//! - Retries like std: a sharing violation on open is retried a few times with a short back-off (a
//!   virus scanner holding a file for a moment); a folder that is not empty when deleted (new entries
//!   appeared, or a non-POSIX delete is still pending) is listed again a bounded number of times.
//! - The first real error stops the walk (like `remove_dir_all`) and names the entry it failed on.
//!
//! Paths kept next to the handles are for messages and test seams only; no operation uses them.
//!
//! All `unsafe` of the permanent delete lives in this module.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows_sys::Wdk::Storage::FileSystem::{
    FILE_OPEN, FILE_OPEN_REPARSE_POINT, FILE_SYNCHRONOUS_IO_NONALERT, NtCreateFile,
};
use windows_sys::Win32::Foundation::{
    ERROR_CANT_RESOLVE_FILENAME, ERROR_DIR_NOT_EMPTY, ERROR_INVALID_FUNCTION,
    ERROR_INVALID_PARAMETER, ERROR_NO_MORE_FILES, ERROR_NOT_SUPPORTED, ERROR_SHARING_VIOLATION,
    NTSTATUS, OBJ_DONT_REPARSE, RtlNtStatusToDosError, STATUS_DELETE_PENDING,
    STATUS_INVALID_PARAMETER, STATUS_OBJECT_NAME_NOT_FOUND, STATUS_OBJECT_PATH_NOT_FOUND,
    STATUS_REPARSE_POINT_ENCOUNTERED, STATUS_REPARSE_POINT_NOT_RESOLVED, STATUS_SHARING_VIOLATION,
    UNICODE_STRING,
};
use windows_sys::Win32::Storage::FileSystem::{
    DELETE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_READONLY,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_TAG_INFO, FILE_BASIC_INFO,
    FILE_DISPOSITION_FLAG_DELETE, FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE,
    FILE_DISPOSITION_FLAG_POSIX_SEMANTICS, FILE_DISPOSITION_INFO, FILE_DISPOSITION_INFO_EX,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_BOTH_DIR_INFO,
    FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, FILE_WRITE_ATTRIBUTES, FileAttributeTagInfo, FileBasicInfo,
    FileDispositionInfo, FileDispositionInfoEx, FileIdBothDirectoryInfo,
    FileIdBothDirectoryRestartInfo, GetFileInformationByHandleEx, SYNCHRONIZE,
    SetFileInformationByHandle,
};
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

/// Why a walk stopped before the tree was gone.
#[derive(Debug)]
pub enum WalkError {
    /// The cancel flag was seen. Whatever was not deleted yet is still there.
    Cancelled,
    /// A real error on `path` (the entry the walk could not open, list or delete).
    Io { path: PathBuf, error: io::Error },
}

/// Test seams: called at the moments a racing process could act.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) enum Seam<'a> {
    /// A listed child is about to be opened (relative to its parent's handle).
    BeforeOpen(&'a Path),
    /// A child folder was opened and checked through its handle; it is about to be listed.
    OpenedDir(&'a Path),
    /// A directory's handle was listed (one batch); its entries are about to be opened.
    Listed(&'a Path),
}

/// Remove `root` (a folder) and everything below it. `cancelled` is polled before every entry;
/// `removed(bytes)` is called once per entry that is gone (a file with its size, a folder or link
/// with 0), the top folder included.
pub fn remove_tree(
    root: &Path,
    cancelled: &dyn Fn() -> bool,
    removed: &mut dyn FnMut(u64),
) -> Result<(), WalkError> {
    remove_tree_with(root, cancelled, removed, &|_| {})
}

const SHARE_ALL: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
/// What every opened entry needs: delete it, read what it is, wait on it (synchronous I/O).
const BASE_ACCESS: u32 = DELETE | FILE_READ_ATTRIBUTES | SYNCHRONIZE;
/// A sharing violation on open is retried this many times (1, 2, 4 ... 32 ms; about 63 ms in all).
const SHARING_RETRIES: u32 = 6;
/// A folder that is still not empty when deleted is listed again at most this many times.
const NOT_EMPTY_RESCANS: u32 = 10;
/// One listing batch. `u64` elements keep the buffer 8-byte aligned, as the API requires.
const DIR_BUF_WORDS: usize = 64 * 1024 / 8;
/// `IsReparseTagNameSurrogate`: junctions and symbolic links (they name another entry).
const REPARSE_TAG_NAME_SURROGATE: u32 = 0x2000_0000;

pub(crate) fn remove_tree_with(
    root: &Path,
    cancelled: &dyn Fn() -> bool,
    removed: &mut dyn FnMut(u64),
    seam: &dyn Fn(Seam<'_>),
) -> Result<(), WalkError> {
    let io_err = |path: &Path, error: io::Error| WalkError::Io {
        path: path.to_path_buf(),
        error,
    };
    if cancelled() {
        return Err(WalkError::Cancelled);
    }
    let top = with_sharing_retry(|| open_root(root)).map_err(|e| io_err(root, e))?;
    let tag = attribute_tag(&top).map_err(|e| io_err(root, e))?;
    if !is_real_directory(&tag) {
        // Not a folder (any more): a link is deleted as the link, a file as the file.
        delete_by_handle(&top).map_err(|e| io_err(root, e))?;
        removed(0);
        return Ok(());
    }

    let mut buf = vec![0u64; DIR_BUF_WORDS];
    let mut stack = vec![Frame::new(top, root.to_path_buf())];
    while let Some(frame) = stack.last_mut() {
        if cancelled() {
            return Err(WalkError::Cancelled);
        }
        if let Some(entry) = frame.pending.pop_front() {
            let child_path = frame.path.join(OsString::from_wide(&entry.name));
            seam(Seam::BeforeOpen(&child_path));
            let access = if entry.listed_as_dir {
                BASE_ACCESS | FILE_LIST_DIRECTORY
            } else {
                BASE_ACCESS
            };
            let child = match with_sharing_retry(|| {
                open_relative(&frame.handle, &entry.name, access)
            }) {
                Ok(Opened::Handle(h)) => h,
                // Gone since it was listed (or being deleted by someone else): nothing to do.
                Ok(Opened::Gone) => continue,
                Ok(Opened::ParentBecameLink) => {
                    // The folder we hold was turned into a junction/symlink in place (it had to
                    // be emptied for that). Its listed names now resolve only in the link's
                    // target, which is never entered: stop listing it and delete the link
                    // itself through the handle we hold.
                    // Anything else (a non-link reparse point that a filter does not resolve for a
                    // relative open) stops the walk with the error: never guessed at.
                    let tag = attribute_tag(&frame.handle).map_err(|e| io_err(&frame.path, e))?;
                    if !is_link(&tag) {
                        return Err(io_err(
                            &child_path,
                            io::Error::from_raw_os_error(ERROR_CANT_RESOLVE_FILENAME as i32),
                        ));
                    }
                    frame.pending.clear();
                    frame.listed_to_end = true;
                    continue;
                }
                Err(e) => return Err(io_err(&child_path, e)),
            };
            let tag = attribute_tag(&child).map_err(|e| io_err(&child_path, e))?;
            if is_real_directory(&tag) {
                let child = if entry.listed_as_dir {
                    child
                } else {
                    // Listed as a file, a folder now: open the same object again, able to list.
                    reopen(&child, BASE_ACCESS | FILE_LIST_DIRECTORY)
                        .map_err(|e| io_err(&child_path, e))?
                };
                seam(Seam::OpenedDir(&child_path));
                stack.push(Frame::new(child, child_path));
                continue;
            }
            // A file, or a link of any kind (deleted as the link, never entered).
            delete_by_handle(&child).map_err(|e| io_err(&child_path, e))?;
            drop(child);
            let is_plain_file =
                tag.FileAttributes & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT) == 0;
            removed(if is_plain_file { entry.size } else { 0 });
            continue;
        }
        if !frame.listed_to_end {
            let restart = std::mem::replace(&mut frame.restart, false);
            let more = list_batch(&frame.handle, &mut buf, restart, &mut frame.pending)
                .map_err(|e| io_err(&frame.path, e))?;
            frame.listed_to_end = !more;
            seam(Seam::Listed(&frame.path));
            continue;
        }
        // Listed to the end and every entry handled: delete the folder itself.
        match delete_by_handle(&frame.handle) {
            Ok(()) => {
                stack.pop();
                removed(0);
            }
            Err(e)
                if e.raw_os_error() == Some(ERROR_DIR_NOT_EMPTY as i32)
                    && frame.rescans < NOT_EMPTY_RESCANS =>
            {
                frame.rescans += 1;
                frame.listed_to_end = false;
                frame.restart = true;
                std::thread::sleep(Duration::from_millis(u64::from(frame.rescans)));
            }
            Err(e) => return Err(io_err(&frame.path, e)),
        }
    }
    Ok(())
}

struct Entry {
    name: Vec<u16>,
    listed_as_dir: bool,
    size: u64,
}

struct Frame {
    handle: OwnedHandle,
    /// For messages and test seams only.
    path: PathBuf,
    pending: VecDeque<Entry>,
    restart: bool,
    listed_to_end: bool,
    rescans: u32,
}

impl Frame {
    fn new(handle: OwnedHandle, path: PathBuf) -> Self {
        Self {
            handle,
            path,
            pending: VecDeque::new(),
            restart: true,
            listed_to_end: false,
            rescans: 0,
        }
    }
}

fn is_real_directory(tag: &FILE_ATTRIBUTE_TAG_INFO) -> bool {
    tag.FileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0 && !is_link(tag)
}

/// A name-surrogate reparse point (junction, directory or file symlink). Other reparse points
/// (cloud placeholders, dedup, ...) are ordinary entries, as for std.
fn is_link(tag: &FILE_ATTRIBUTE_TAG_INFO) -> bool {
    tag.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        && tag.ReparseTag & REPARSE_TAG_NAME_SURROGATE != 0
}

fn with_sharing_retry<T>(mut op: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    let mut attempt = 0;
    loop {
        match op() {
            Err(e)
                if e.raw_os_error() == Some(ERROR_SHARING_VIOLATION as i32)
                    && attempt < SHARING_RETRIES =>
            {
                std::thread::sleep(Duration::from_millis(1 << attempt));
                attempt += 1;
            }
            other => return other,
        }
    }
}

/// The top folder, by path (the path the user chose), without following it if it is a link.
fn open_root(path: &Path) -> io::Result<OwnedHandle> {
    let file = fs::OpenOptions::new()
        .access_mode(BASE_ACCESS | FILE_LIST_DIRECTORY)
        .share_mode(SHARE_ALL)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    Ok(OwnedHandle::from(file))
}

fn nt_error(status: NTSTATUS) -> io::Error {
    // SAFETY: a pure status-code translation with no pointers.
    let code = unsafe { RtlNtStatusToDosError(status) };
    io::Error::from_raw_os_error(code as i32)
}

/// Whether `OBJ_DONT_REPARSE` is accepted (cleared once if the system rejects it, as std does).
static DONT_REPARSE_SUPPORTED: AtomicBool = AtomicBool::new(true);

#[cfg(test)]
thread_local! {
    /// Tests: open as a system without `OBJ_DONT_REPARSE` would.
    static FORCE_NO_DONT_REPARSE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn dont_reparse_flag() -> u32 {
    #[cfg(test)]
    if FORCE_NO_DONT_REPARSE.with(|f| f.get()) {
        return 0;
    }
    if DONT_REPARSE_SUPPORTED.load(Ordering::Relaxed) {
        OBJ_DONT_REPARSE
    } else {
        0
    }
}

/// `NtCreateFile` of `name` relative to the open directory or file `root` (an empty `name` opens
/// `root`'s own object again).
///
/// `FILE_OPEN_REPARSE_POINT`: a final reparse point is opened as itself, never followed.
/// A name relative to a handle cannot be redirected either: when `root` itself has been turned into
/// a junction in place, the kernel answers STATUS_REPARSE_POINT_NOT_RESOLVED (a relative open has no
/// path to re-parse), with or without `OBJ_DONT_REPARSE`; both cases are tested
/// (`a_held_folder_turned_into_a_junction_in_place_...`). `OBJ_DONT_REPARSE` (refuse any reparse
/// point while resolving) is added on top where the system knows it; older systems reject it with
/// STATUS_INVALID_PARAMETER and it is dropped from then on, as std does.
fn nt_open(root: &OwnedHandle, name: &[u16], access: u32) -> Result<OwnedHandle, NTSTATUS> {
    loop {
        let flag = dont_reparse_flag();
        match nt_open_once(root, name, access, flag) {
            Err(STATUS_INVALID_PARAMETER) if flag != 0 => {
                DONT_REPARSE_SUPPORTED.store(false, Ordering::Relaxed);
            }
            other => return other,
        }
    }
}

fn nt_open_once(
    root: &OwnedHandle,
    name: &[u16],
    access: u32,
    attributes: u32,
) -> Result<OwnedHandle, NTSTATUS> {
    let byte_len = u16::try_from(name.len() * 2).map_err(|_| STATUS_INVALID_PARAMETER)?;
    let unicode = UNICODE_STRING {
        Length: byte_len,
        MaximumLength: byte_len,
        Buffer: name.as_ptr().cast_mut(),
    };
    let object = OBJECT_ATTRIBUTES {
        Length: std::mem::size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: root.as_raw_handle(),
        ObjectName: &unicode,
        Attributes: attributes,
        SecurityDescriptor: std::ptr::null(),
        SecurityQualityOfService: std::ptr::null(),
    };
    let mut handle = std::ptr::null_mut();
    let mut status_block = IO_STATUS_BLOCK::default();
    // SAFETY: every pointer is valid for the call: `handle` and `status_block` are writable locals,
    // `object` and the `unicode` it points to live until the call returns, and `name` (borrowed by
    // `unicode.Buffer`; NtCreateFile only reads the name) outlives them. `RootDirectory` is a live
    // handle we own. No allocation size, no EA buffer.
    let status = unsafe {
        NtCreateFile(
            &mut handle,
            access,
            &object,
            &mut status_block,
            std::ptr::null(),
            0,
            SHARE_ALL,
            FILE_OPEN,
            FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            std::ptr::null(),
            0,
        )
    };
    if status < 0 {
        return Err(status);
    }
    // SAFETY: NtCreateFile succeeded, so `handle` is a new open handle that nobody else owns.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

enum Opened {
    Handle(OwnedHandle),
    /// Gone since it was listed, or being deleted by someone else.
    Gone,
    /// Resolving the name would cross a reparse point: the PARENT turned into a link in place.
    ParentBecameLink,
}

/// Open the single name component `name` relative to the directory `parent`, never following a
/// reparse point.
fn open_relative(parent: &OwnedHandle, name: &[u16], access: u32) -> io::Result<Opened> {
    // A listed name is one component. Anything else would be a bug or a hostile file system: refuse
    // rather than resolve a multi-component path.
    if name.is_empty()
        || name
            .iter()
            .any(|&c| c == u16::from(b'\\') || c == u16::from(b'/') || c == 0)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the folder listing returned an invalid name",
        ));
    }
    match nt_open(parent, name, access) {
        Ok(h) => Ok(Opened::Handle(h)),
        Err(
            STATUS_OBJECT_NAME_NOT_FOUND | STATUS_OBJECT_PATH_NOT_FOUND | STATUS_DELETE_PENDING,
        ) => Ok(Opened::Gone),
        Err(STATUS_REPARSE_POINT_ENCOUNTERED | STATUS_REPARSE_POINT_NOT_RESOLVED) => {
            Ok(Opened::ParentBecameLink)
        }
        Err(STATUS_SHARING_VIOLATION) => {
            Err(io::Error::from_raw_os_error(ERROR_SHARING_VIOLATION as i32))
        }
        Err(other) => Err(nt_error(other)),
    }
}

/// Open the same object as `handle` again with `access` (no name is resolved; works for folders,
/// unlike `ReOpenFile`).
fn reopen(handle: &OwnedHandle, access: u32) -> io::Result<OwnedHandle> {
    nt_open(handle, &[], access).map_err(nt_error)
}

fn attribute_tag(handle: &OwnedHandle) -> io::Result<FILE_ATTRIBUTE_TAG_INFO> {
    let mut info = FILE_ATTRIBUTE_TAG_INFO::default();
    // SAFETY: `info` is a writable FILE_ATTRIBUTE_TAG_INFO and the size passed is its size.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            handle.as_raw_handle(),
            FileAttributeTagInfo,
            (&raw mut info).cast(),
            std::mem::size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(info)
}

/// Read the next batch of `dir`'s entries into `out` ("." and ".." skipped). False at the end.
fn list_batch(
    dir: &OwnedHandle,
    buf: &mut [u64],
    restart: bool,
    out: &mut VecDeque<Entry>,
) -> io::Result<bool> {
    let class = if restart {
        FileIdBothDirectoryRestartInfo
    } else {
        FileIdBothDirectoryInfo
    };
    let byte_len = std::mem::size_of_val(buf);
    // SAFETY: `buf` is writable for `byte_len` bytes and 8-byte aligned (u64 elements), as
    // FILE_ID_BOTH_DIR_INFO requires.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            dir.as_raw_handle(),
            class,
            buf.as_mut_ptr().cast(),
            byte_len as u32,
        )
    };
    if ok == 0 {
        let e = io::Error::last_os_error();
        if e.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
            return Ok(false);
        }
        return Err(e);
    }
    parse_batch(buf, out)?;
    Ok(true)
}

fn parse_batch(buf: &[u64], out: &mut VecDeque<Entry>) -> io::Result<()> {
    let base = buf.as_ptr().cast::<u8>();
    let byte_len = std::mem::size_of_val(buf);
    let name_offset = std::mem::offset_of!(FILE_ID_BOTH_DIR_INFO, FileName);
    let bad = || io::Error::new(io::ErrorKind::InvalidData, "malformed folder listing");
    let mut offset = 0usize;
    loop {
        if offset
            .checked_add(name_offset)
            .is_none_or(|end| end > byte_len)
            || !offset.is_multiple_of(8)
        {
            return Err(bad());
        }
        // SAFETY: `offset + name_offset <= byte_len` was checked, so the fixed header is inside
        // `buf`; `offset` is 8-aligned and `buf` is 8-aligned, matching FILE_ID_BOTH_DIR_INFO.
        let info = unsafe { &*base.add(offset).cast::<FILE_ID_BOTH_DIR_INFO>() };
        let name_bytes = info.FileNameLength as usize;
        if !name_bytes.is_multiple_of(2)
            || offset
                .checked_add(name_offset + name_bytes)
                .is_none_or(|end| end > byte_len)
        {
            return Err(bad());
        }
        // SAFETY: the name lies inside `buf` (checked just above) and is 2-aligned (8-aligned
        // record + even offset of FileName); `buf` is not written while the slice lives.
        let name = unsafe {
            std::slice::from_raw_parts(base.add(offset + name_offset).cast::<u16>(), name_bytes / 2)
        };
        let dot = u16::from(b'.');
        if name != [dot] && name != [dot, dot] {
            out.push_back(Entry {
                name: name.to_vec(),
                listed_as_dir: info.FileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0,
                size: u64::try_from(info.EndOfFile).unwrap_or(0),
            });
        }
        if info.NextEntryOffset == 0 {
            return Ok(());
        }
        offset = offset
            .checked_add(info.NextEntryOffset as usize)
            .ok_or_else(bad)?;
    }
}

#[cfg(test)]
thread_local! {
    /// Tests: take the fallback path (classic disposition) as on a file system without POSIX delete.
    static FORCE_LEGACY_DELETE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn legacy_forced() -> bool {
    #[cfg(test)]
    {
        FORCE_LEGACY_DELETE.with(|f| f.get())
    }
    #[cfg(not(test))]
    {
        false
    }
}

/// Delete the object behind `handle` (a file, an empty folder or a link: the link itself).
fn delete_by_handle(handle: &OwnedHandle) -> io::Result<()> {
    if !legacy_forced() {
        let info = FILE_DISPOSITION_INFO_EX {
            Flags: FILE_DISPOSITION_FLAG_DELETE
                | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS
                | FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE,
        };
        // SAFETY: `info` is a valid FILE_DISPOSITION_INFO_EX and the size passed is its size.
        let ok = unsafe {
            SetFileInformationByHandle(
                handle.as_raw_handle(),
                FileDispositionInfoEx,
                (&raw const info).cast(),
                std::mem::size_of::<FILE_DISPOSITION_INFO_EX>() as u32,
            )
        };
        if ok != 0 {
            return Ok(());
        }
        let e = io::Error::last_os_error();
        let unsupported = [
            ERROR_INVALID_PARAMETER,
            ERROR_NOT_SUPPORTED,
            ERROR_INVALID_FUNCTION,
        ]
        .iter()
        .any(|&c| e.raw_os_error() == Some(c as i32));
        if !unsupported {
            return Err(e);
        }
    }
    legacy_delete(handle)
}

fn set_legacy_disposition(handle: &OwnedHandle) -> io::Result<()> {
    let info = FILE_DISPOSITION_INFO { DeleteFile: true };
    // SAFETY: `info` is a valid FILE_DISPOSITION_INFO and the size passed is its size.
    let ok = unsafe {
        SetFileInformationByHandle(
            handle.as_raw_handle(),
            FileDispositionInfo,
            (&raw const info).cast(),
            std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// The classic delete: the name goes when the last handle closes. A read-only entry is made
/// writable first, through a second handle of the SAME object (never by path).
fn legacy_delete(handle: &OwnedHandle) -> io::Result<()> {
    match set_legacy_disposition(handle) {
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
            let tag = attribute_tag(handle)?;
            if tag.FileAttributes & FILE_ATTRIBUTE_READONLY == 0 {
                return Err(e);
            }
            let writer = reopen(
                handle,
                FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES | SYNCHRONIZE,
            )
            .map_err(|_| e)?;
            let mut basic = FILE_BASIC_INFO::default();
            // SAFETY: `basic` is a writable FILE_BASIC_INFO and the size passed is its size.
            let ok = unsafe {
                GetFileInformationByHandleEx(
                    writer.as_raw_handle(),
                    FileBasicInfo,
                    (&raw mut basic).cast(),
                    std::mem::size_of::<FILE_BASIC_INFO>() as u32,
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            // Times 0 = "leave unchanged".
            let attributes = basic.FileAttributes & !FILE_ATTRIBUTE_READONLY;
            let update = FILE_BASIC_INFO {
                FileAttributes: if attributes == 0 {
                    FILE_ATTRIBUTE_NORMAL
                } else {
                    attributes
                },
                ..FILE_BASIC_INFO::default()
            };
            // SAFETY: `update` is a valid FILE_BASIC_INFO and the size passed is its size.
            let ok = unsafe {
                SetFileInformationByHandle(
                    writer.as_raw_handle(),
                    FileBasicInfo,
                    (&raw const update).cast(),
                    std::mem::size_of::<FILE_BASIC_INFO>() as u32,
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            set_legacy_disposition(handle)
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    struct Fixture {
        root: PathBuf,
        _dir: tempfile::TempDir,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let root = root
            .to_string_lossy()
            .strip_prefix(r"\\?\")
            .map(PathBuf::from)
            .unwrap_or(root);
        Fixture { root, _dir: dir }
    }

    fn never() -> bool {
        false
    }

    /// `mklink /J` needs no privilege (same helper as delete_jobs' tests).
    pub(crate) fn make_junction(link: &Path, target: &Path) {
        let out = std::process::Command::new("cmd")
            .arg("/C")
            .arg("mklink")
            .arg("/J")
            .arg(link)
            .arg(target)
            .output()
            .expect("cmd runs");
        assert!(
            out.status.success(),
            "mklink /J failed: {}",
            String::from_utf8_lossy(&out.stdout)
        );
    }

    /// Turn the EXISTING, empty folder `dir` into a mount point (junction) to `target` in place,
    /// with `FSCTL_SET_REPARSE_POINT`, the way an attacker would while a handle to it is held.
    fn convert_in_place_to_junction(dir: &Path, target: &Path) {
        use windows_sys::Win32::System::IO::DeviceIoControl;
        const FSCTL_SET_REPARSE_POINT: u32 = 0x0009_00A4;
        const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;
        let h = fs::OpenOptions::new()
            .access_mode(FILE_WRITE_ATTRIBUTES)
            .share_mode(SHARE_ALL)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(dir)
            .expect("open the folder to convert");
        let sub: Vec<u16> = format!(r"\??\{}", target.display())
            .encode_utf16()
            .collect();
        let sub_bytes = (sub.len() * 2) as u16;
        let mut path_buffer = sub.clone();
        path_buffer.extend([0, 0]); // substitute NUL + empty print name NUL
        let mut buf: Vec<u8> = Vec::new();
        buf.extend(IO_REPARSE_TAG_MOUNT_POINT.to_le_bytes());
        buf.extend(((8 + path_buffer.len() * 2) as u16).to_le_bytes());
        buf.extend(0u16.to_le_bytes());
        buf.extend(0u16.to_le_bytes()); // substitute offset
        buf.extend(sub_bytes.to_le_bytes());
        buf.extend((sub_bytes + 2).to_le_bytes()); // print name offset
        buf.extend(0u16.to_le_bytes()); // print name length
        for w in path_buffer {
            buf.extend(w.to_le_bytes());
        }
        let mut returned = 0u32;
        // SAFETY: `buf` is a readable REPARSE_DATA_BUFFER of the given length, `returned` is
        // writable, no output buffer, synchronous (no OVERLAPPED).
        let ok = unsafe {
            DeviceIoControl(
                h.as_raw_handle(),
                FSCTL_SET_REPARSE_POINT,
                buf.as_ptr().cast(),
                buf.len() as u32,
                std::ptr::null_mut(),
                0,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        assert!(
            ok != 0,
            "FSCTL_SET_REPARSE_POINT: {}",
            io::Error::last_os_error()
        );
    }

    fn victim(root: &Path) -> (PathBuf, PathBuf) {
        let v = root.join("victim");
        fs::create_dir_all(v.join("nested")).unwrap();
        fs::write(v.join("secret.txt"), b"precious").unwrap();
        fs::write(v.join("nested").join("deep.txt"), b"precious").unwrap();
        let secret = v.join("secret.txt");
        (v, secret)
    }

    fn assert_victim_intact(v: &Path) {
        assert_eq!(fs::read(v.join("secret.txt")).unwrap(), b"precious");
        assert_eq!(
            fs::read(v.join("nested").join("deep.txt")).unwrap(),
            b"precious"
        );
    }

    fn set_readonly(p: &Path) {
        let mut perms = fs::metadata(p).unwrap().permissions();
        perms.set_readonly(true);
        fs::set_permissions(p, perms).unwrap();
    }

    fn run(root: &Path) -> (Result<(), WalkError>, u64, u64) {
        let mut entries = 0u64;
        let mut bytes = 0u64;
        let r = remove_tree(root, &never, &mut |b| {
            entries += 1;
            bytes += b;
        });
        (r, entries, bytes)
    }

    #[test]
    fn deletes_a_tree_and_counts_every_entry_and_byte() {
        let fx = fixture();
        let top = fx.root.join("top");
        fs::create_dir_all(top.join("a").join("b")).unwrap();
        fs::write(top.join("one.bin"), vec![0u8; 1000]).unwrap();
        fs::write(top.join("a").join("two.bin"), vec![0u8; 24]).unwrap();
        fs::write(top.join("a").join("b").join("three.bin"), b"").unwrap();
        let (r, entries, bytes) = run(&top);
        r.unwrap();
        assert!(!top.exists());
        // top, a, b + three files
        assert_eq!(entries, 6);
        assert_eq!(bytes, 1024);
    }

    #[test]
    fn a_large_flat_folder_spanning_several_listing_batches_is_deleted() {
        let fx = fixture();
        let top = fx.root.join("flat");
        fs::create_dir_all(&top).unwrap();
        for i in 0..3000 {
            fs::write(
                top.join(format!("a-rather-long-file-name-number-{i:05}.txt")),
                b"x",
            )
            .unwrap();
        }
        fs::create_dir(top.join("zz-sub")).unwrap();
        fs::write(top.join("zz-sub").join("f"), b"x").unwrap();
        let (r, entries, _) = run(&top);
        r.unwrap();
        assert!(!top.exists());
        assert_eq!(entries, 3000 + 3);
    }

    #[test]
    fn read_only_files_and_folders_are_deleted() {
        let fx = fixture();
        let top = fx.root.join("ro");
        let inner = top.join("inner");
        fs::create_dir_all(&inner).unwrap();
        fs::write(inner.join("f.txt"), b"x").unwrap();
        set_readonly(&inner.join("f.txt"));
        set_readonly(&inner);
        set_readonly(&top);
        let (r, entries, _) = run(&top);
        r.unwrap();
        assert!(!top.exists());
        assert_eq!(entries, 3);
    }

    #[test]
    fn the_legacy_delete_path_clears_read_only_through_the_handle() {
        let fx = fixture();
        let top = fx.root.join("legacy");
        let inner = top.join("inner");
        fs::create_dir_all(&inner).unwrap();
        fs::write(inner.join("f.txt"), b"x").unwrap();
        fs::write(top.join("g.txt"), b"x").unwrap();
        set_readonly(&inner.join("f.txt"));
        set_readonly(&inner);
        FORCE_LEGACY_DELETE.with(|f| f.set(true));
        let (r, entries, _) = run(&top);
        FORCE_LEGACY_DELETE.with(|f| f.set(false));
        r.unwrap();
        assert!(!top.exists());
        assert_eq!(entries, 4);
    }

    #[test]
    fn a_junction_inside_the_tree_is_removed_as_a_link_and_its_target_survives() {
        let fx = fixture();
        let (v, _) = victim(&fx.root);
        let top = fx.root.join("tree");
        fs::create_dir_all(top.join("sub")).unwrap();
        make_junction(&top.join("sub").join("link"), &v);
        set_readonly(&v.join("secret.txt"));
        let (r, entries, _) = run(&top);
        r.unwrap();
        assert!(!top.exists());
        assert_eq!(entries, 3, "tree, sub and the link itself");
        assert_victim_intact(&v);
        assert!(
            fs::metadata(v.join("secret.txt"))
                .unwrap()
                .permissions()
                .readonly(),
            "the target's attributes are untouched"
        );
    }

    #[test]
    fn a_top_folder_that_is_a_junction_is_removed_as_the_link() {
        let fx = fixture();
        let (v, _) = victim(&fx.root);
        let link = fx.root.join("link");
        make_junction(&link, &v);
        let (r, entries, _) = run(&link);
        r.unwrap();
        assert_eq!(entries, 1);
        assert!(fs::symlink_metadata(&link).is_err());
        assert_victim_intact(&v);
    }

    #[test]
    fn a_file_symlink_inside_the_tree_is_removed_as_the_link() {
        let fx = fixture();
        let (v, secret) = victim(&fx.root);
        let top = fx.root.join("tree");
        fs::create_dir_all(&top).unwrap();
        let link = top.join("secret-link.txt");
        if let Err(e) = std::os::windows::fs::symlink_file(&secret, &link) {
            // Creating a symlink needs Developer Mode or the privilege; the walker is not the subject.
            eprintln!("skipped: cannot create a file symlink here ({e})");
            return;
        }
        // With the same privilege a directory symlink works too.
        std::os::windows::fs::symlink_dir(&v, top.join("dir-link")).unwrap();
        let (r, entries, bytes) = run(&top);
        r.unwrap();
        assert!(!top.exists());
        assert_eq!(entries, 3, "tree and the two links");
        assert_eq!(bytes, 0, "a link's target size is not counted");
        assert_victim_intact(&v);
    }

    #[test]
    fn a_very_deep_tree_with_long_paths_is_deleted_on_a_small_stack() {
        let fx = fixture();
        let top = fx.root.join("deep");
        let mut p = PathBuf::from(format!(r"\\?\{}", top.display()));
        fs::create_dir_all(&p).unwrap();
        let segment = "d".repeat(50);
        for _ in 0..400 {
            p.push(&segment);
            fs::create_dir(&p).unwrap();
        }
        fs::write(p.join("leaf.txt"), b"x").unwrap();
        assert!(p.as_os_str().len() > 260 * 50, "far beyond MAX_PATH");
        let verbatim = PathBuf::from(format!(r"\\?\{}", top.display()));
        let handle = std::thread::Builder::new()
            .stack_size(128 * 1024)
            .spawn(move || {
                let mut n = 0u64;
                let r = remove_tree(&verbatim, &never, &mut |_| n += 1);
                (r.is_ok(), n)
            })
            .unwrap();
        let (ok, n) = handle.join().expect("no stack overflow");
        assert!(ok);
        assert_eq!(n, 1 + 400 + 1);
        assert!(!top.exists());
    }

    #[test]
    fn cancel_mid_tree_stops_and_leaves_the_rest() {
        let fx = fixture();
        let top = fx.root.join("big");
        fs::create_dir_all(top.join("sub")).unwrap();
        for i in 0..50 {
            fs::write(top.join(format!("f{i:02}.txt")), b"x").unwrap();
            fs::write(top.join("sub").join(format!("g{i:02}.txt")), b"x").unwrap();
        }
        let removed = Cell::new(0u64);
        let r = remove_tree(&top, &|| removed.get() >= 10, &mut |_| {
            removed.set(removed.get() + 1)
        });
        assert!(matches!(r, Err(WalkError::Cancelled)));
        assert_eq!(removed.get(), 10, "stops at the very next entry");
        assert!(top.exists());
        let left = walkdir_count(&top);
        assert_eq!(left, 100 + 1 - 10, "exactly the reported entries are gone");
    }

    fn walkdir_count(p: &Path) -> u64 {
        let mut n = 0;
        for e in fs::read_dir(p).unwrap() {
            let e = e.unwrap();
            n += 1;
            if e.file_type().unwrap().is_dir() {
                n += walkdir_count(&e.path());
            }
        }
        n
    }

    #[test]
    fn a_folder_swapped_for_a_junction_between_listing_and_open_is_removed_as_a_link() {
        let fx = fixture();
        let (v, _) = victim(&fx.root);
        let top = fx.root.join("tree");
        let sub = top.join("sub");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("secret.txt"), b"attacker's decoy").unwrap();
        let swapped = Cell::new(false);
        let r = remove_tree_with(&top, &never, &mut |_| {}, &|s| {
            if let Seam::BeforeOpen(p) = s
                && p == sub
                && !swapped.replace(true)
            {
                fs::rename(&sub, top.join("moved")).unwrap();
                make_junction(&sub, &v);
            }
        });
        assert!(swapped.get());
        r.unwrap();
        assert!(!top.exists(), "the moved folder is found on the re-listing");
        assert_victim_intact(&v);
    }

    #[test]
    fn a_held_folder_turned_into_a_junction_in_place_never_reaches_the_target() {
        held_folder_turned_into_a_junction();
        empty_folder_turned_into_a_junction_before_it_is_listed();
    }

    #[test]
    fn a_held_folder_turned_into_a_junction_in_place_without_obj_dont_reparse_too() {
        FORCE_NO_DONT_REPARSE.with(|f| f.set(true));
        held_folder_turned_into_a_junction();
        empty_folder_turned_into_a_junction_before_it_is_listed();
        FORCE_NO_DONT_REPARSE.with(|f| f.set(false));
    }

    /// Review R2-S1, second variant: `d` is empty when opened; it becomes a mount point after the
    /// handle check and before the listing. The listing reads the held object, never the target.
    fn empty_folder_turned_into_a_junction_before_it_is_listed() {
        let fx = fixture();
        let (v, _) = victim(&fx.root);
        let top = fx.root.join("tree");
        let d = top.join("d");
        fs::create_dir_all(&d).unwrap();
        fs::write(top.join("other.txt"), b"x").unwrap();
        let converted = Cell::new(false);
        let r = remove_tree_with(&top, &never, &mut |_| {}, &|s| {
            if let Seam::OpenedDir(p) = s
                && p == d
            {
                convert_in_place_to_junction(&d, &v);
                converted.set(true);
            }
        });
        assert!(converted.get());
        assert_victim_intact(&v);
        r.unwrap();
        assert!(!top.exists());
    }

    fn held_folder_turned_into_a_junction() {
        // Review R2-S1: the walker holds `d` open and has listed it; the attacker empties it and
        // makes it a mount point to the victim (allowed while our handle is open). The listed names
        // now exist only in the victim folder; opens relative to our handle must not find them.
        let fx = fixture();
        let (v, _) = victim(&fx.root);
        let top = fx.root.join("tree");
        let d = top.join("d");
        fs::create_dir_all(d.join("nested")).unwrap();
        fs::write(d.join("secret.txt"), b"decoy").unwrap();
        fs::write(d.join("nested").join("deep.txt"), b"decoy").unwrap();
        let converted = Cell::new(false);
        let opened_after = RefCell::new(Vec::<PathBuf>::new());
        let r = remove_tree_with(&top, &never, &mut |_| {}, &|s| match s {
            Seam::Listed(p) if p == d && !converted.get() => {
                converted.set(true);
                fs::remove_dir_all(d.join("nested")).unwrap();
                fs::remove_file(d.join("secret.txt")).unwrap();
                convert_in_place_to_junction(&d, &v);
                assert!(
                    d.join("secret.txt").exists(),
                    "by path, d now leads to the victim"
                );
            }
            Seam::BeforeOpen(p) if converted.get() => {
                opened_after.borrow_mut().push(p.to_path_buf())
            }
            _ => {}
        });
        assert!(converted.get());
        assert_victim_intact(&v);
        assert!(
            !opened_after.borrow().is_empty(),
            "the walker did try the listed names after the conversion"
        );
        // The walk itself either removed the (now link) folder or reported why not; the victim is
        // what matters, and the tree must not be left with victim content deleted.
        if let Err(e) = r {
            panic!("walk failed: {e:?}");
        }
        assert!(!top.exists());
    }

    #[test]
    fn a_held_file_stops_the_walk_with_its_path() {
        let fx = fixture();
        let top = fx.root.join("held");
        fs::create_dir_all(&top).unwrap();
        fs::write(top.join("a.txt"), b"x").unwrap();
        fs::write(top.join("z.txt"), b"x").unwrap();
        let _held = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(top.join("z.txt"))
            .unwrap();
        let (r, _, _) = run(&top);
        match r {
            Err(WalkError::Io { path, error }) => {
                assert_eq!(path, top.join("z.txt"));
                assert_eq!(error.raw_os_error(), Some(ERROR_SHARING_VIOLATION as i32));
            }
            other => panic!("expected a sharing violation, got {other:?}"),
        }
        assert!(top.join("z.txt").exists());
    }

    #[test]
    fn parse_batch_rejects_a_record_running_past_the_buffer() {
        // 128 bytes: one header (104) + a little. Built as bytes, then copied into aligned words.
        let name_offset = std::mem::offset_of!(FILE_ID_BOTH_DIR_INFO, FileName);
        let fnl = std::mem::offset_of!(FILE_ID_BOTH_DIR_INFO, FileNameLength);
        let words = |bytes: &[u8; 128]| -> Vec<u64> {
            bytes
                .as_chunks::<8>()
                .0
                .iter()
                .map(|c| u64::from_le_bytes(*c))
                .collect()
        };
        let mut bytes = [0u8; 128];
        bytes[fnl..fnl + 4].copy_from_slice(&(((128 - name_offset) + 2) as u32).to_le_bytes());
        let mut out = VecDeque::new();
        assert!(parse_batch(&words(&bytes), &mut out).is_err());
        bytes[fnl..fnl + 4].copy_from_slice(&2u32.to_le_bytes());
        bytes[name_offset..name_offset + 2].copy_from_slice(&u16::from(b'x').to_le_bytes());
        // NextEntryOffset pointing outside.
        bytes[0..4].copy_from_slice(&200u32.to_le_bytes());
        out.clear();
        assert!(parse_batch(&words(&bytes), &mut out).is_err());
        bytes[0..4].copy_from_slice(&0u32.to_le_bytes());
        out.clear();
        parse_batch(&words(&bytes), &mut out).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, vec![u16::from(b'x')]);
    }
}
