//! FER-586: permanent delete of a folder tree, relative to open directory handles.
//!
//! This is the walker behind a permanent delete of a folder (`delete_jobs`). It works like std's
//! Windows `remove_dir_all`, plus a cancel check and a progress callback per entry, plus an identity
//! check that std does not have (needed on network locations, see below).
//!
//! What it does:
//! - The top folder is opened once by path with `FILE_FLAG_OPEN_REPARSE_POINT`. Junctions in the
//!   top folder's ANCESTORS are resolved as for any path the user chooses (as std and Explorer do).
//! - Everything below it is opened with `NtCreateFile` relative to the parent's handle
//!   (`RootDirectory`), one validated name component at a time, with `FILE_OPEN_REPARSE_POINT` (a
//!   final reparse point is opened as itself) and `OBJ_DONT_REPARSE` where supported.
//! - What an entry IS comes from its open handle (`FileAttributeTagInfo`), never from the listing: a
//!   name-surrogate reparse point (junction, symlink) is deleted as the link itself and never
//!   entered. A directory carrying another kind of reparse point (cloud placeholder, dedup, ...) is
//!   entered like a folder; std instead deletes such a directory as an entry (and fails if it is not
//!   empty). Entering it is safe here because everything below it is handle-relative as well.
//! - Identity: every entry's 128-bit file id (and the volume serial) comes from the listing of the
//!   parent's handle (`FileIdExtdDirectoryInfo`). After a child is opened, its own id
//!   (`FileIdInfo`) must be the listed one; otherwise the handle is dropped unused, the parent is
//!   listed again (bounded), and finally the walk stops with an error.
//! - Listing reads the open handle; deleting goes through the open handle: `FileDispositionInfoEx`
//!   with POSIX semantics and `IGNORE_READONLY_ATTRIBUTE`; where a file system does not support that
//!   (FAT, older systems) the classic `FileDispositionInfo`, after clearing the read-only attribute
//!   through a second handle opened from the first (restored if the delete still fails). That second
//!   handle, like the working handle of the top folder, must prove it is the same object (file id)
//!   wherever ids are required; otherwise nothing is changed and the original error is returned.
//! - Iterative: an explicit stack of open directories, so depth is bounded by memory, not the
//!   thread's stack.
//! - Retries like std: a sharing violation on open is retried a few times with a short back-off; a
//!   folder that is not empty when deleted is listed again a bounded number of times.
//! - The first real error stops the walk (like `remove_dir_all`) and names the entry it failed on.
//!
//! What is guaranteed (the CVE-2022-21658 class: a folder swapped for a junction, or turned into a
//! junction in place, while the delete runs):
//! - Local volumes on a known file system (NTFS, ReFS, FAT, FAT32, exFAT, by the volume's
//!   `FileFsAttributeInformation`): a relative open is resolved by the kernel in the held object
//!   itself. A folder
//!   swapped for a link before it is opened is opened AS the link and deleted as such. A held folder
//!   turned into a link in place makes every later relative open fail with
//!   STATUS_REPARSE_POINT_NOT_RESOLVED; the walker then deletes it as the link it now is, through
//!   the handle it holds, and never lists the target. Tested with and without `OBJ_DONT_REPARSE`.
//!   The identity check is a second line of defence here.
//! - Network locations (UNC paths, mapped drives): SMB has no handle-relative open; the redirector
//!   sends the joined PATH and the server resolves junctions in it. There the identity check is what
//!   keeps an open that landed somewhere else from being used: the object must be the one the held
//!   parent listed. On a network location every entry must have a verifiable id, otherwise the walk
//!   is refused with an error; and held folders are opened without `FILE_SHARE_DELETE`, so they
//!   cannot be renamed away while held. Tested through `\\localhost\<drive>$` (scenarios: in-place
//!   junction, swap before open, rename of a held folder, the classic delete's second open).
//! - Any other local file system (e.g. a user-mode WinFsp/Dokan volume, which may resolve by path)
//!   is treated like a network location: ids required, held folders without delete sharing.
//! - Local volumes keep `FILE_SHARE_DELETE` (as std): a subtree moved elsewhere by another process
//!   while it is being deleted is still deleted at its new place (it is the same object the user
//!   chose).
//!
//! Paths kept next to the handles are for messages and test seams only; no operation uses them.
//!
//! All `unsafe` of the permanent delete lives in this module.

use std::cell::Cell;
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
    FILE_OPEN, FILE_OPEN_REPARSE_POINT, FILE_SYNCHRONOUS_IO_NONALERT, FileFsAttributeInformation,
    FileFsDeviceInformation, NtCreateFile, NtQueryVolumeInformationFile,
};
use windows_sys::Win32::Foundation::{
    ERROR_CANT_RESOLVE_FILENAME, ERROR_DIR_NOT_EMPTY, ERROR_INVALID_FUNCTION, ERROR_INVALID_LEVEL,
    ERROR_INVALID_PARAMETER, ERROR_NO_MORE_FILES, ERROR_NOT_SUPPORTED, ERROR_SHARING_VIOLATION,
    NTSTATUS, OBJ_DONT_REPARSE, RtlNtStatusToDosError, STATUS_DELETE_PENDING,
    STATUS_INVALID_PARAMETER, STATUS_NAME_TOO_LONG, STATUS_OBJECT_NAME_NOT_FOUND,
    STATUS_OBJECT_PATH_NOT_FOUND, STATUS_REPARSE_POINT_ENCOUNTERED,
    STATUS_REPARSE_POINT_NOT_RESOLVED, STATUS_SHARING_VIOLATION, UNICODE_STRING,
};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, DELETE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL,
    FILE_ATTRIBUTE_READONLY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_TAG_INFO,
    FILE_BASIC_INFO, FILE_DISPOSITION_FLAG_DELETE, FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE,
    FILE_DISPOSITION_FLAG_POSIX_SEMANTICS, FILE_DISPOSITION_INFO, FILE_DISPOSITION_INFO_EX,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_BOTH_DIR_INFO,
    FILE_ID_EXTD_DIR_INFO, FILE_ID_INFO, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_WRITE_ATTRIBUTES,
    FileAttributeTagInfo, FileBasicInfo, FileDispositionInfo, FileDispositionInfoEx,
    FileIdBothDirectoryInfo, FileIdBothDirectoryRestartInfo, FileIdExtdDirectoryInfo,
    FileIdExtdDirectoryRestartInfo, FileIdInfo, GetFileInformationByHandle,
    GetFileInformationByHandleEx, SYNCHRONIZE, SetFileInformationByHandle,
};
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

/// Why a walk stopped before the tree was gone.
#[derive(Debug)]
pub enum WalkError {
    /// The cancel flag was seen. Whatever was not deleted yet is still there.
    Cancelled,
    /// A real error on `path` (the entry the walk could not open, list, verify or delete).
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
    /// The classic delete is about to open a read-only entry again to clear its attribute.
    BeforeReopen(&'a Path),
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
/// Held folders on a network location: nobody may rename or delete them while we hold them.
const SHARE_NO_DELETE: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE;
/// What every opened entry needs: delete it, read what it is, wait on it (synchronous I/O).
const BASE_ACCESS: u32 = DELETE | FILE_READ_ATTRIBUTES | SYNCHRONIZE;
/// A sharing violation on open is retried this many times (1, 2, 4 ... 32 ms; about 63 ms in all).
const SHARING_RETRIES: u32 = 6;
/// A folder is listed again at most this many times (not empty when deleted, or an entry changed).
const RESCANS: u32 = 10;
/// One listing batch. `u64` elements keep the buffer 8-byte aligned, as the API requires.
const DIR_BUF_WORDS: usize = 64 * 1024 / 8;
/// `IsReparseTagNameSurrogate`: junctions and symbolic links (they name another entry).
const REPARSE_TAG_NAME_SURROGATE: u32 = 0x2000_0000;

/// A 128-bit file id (an id of 0 means "not available").
type FileId = [u8; 16];

/// Facts about the volume the walk runs on, fixed when the top folder is opened.
struct Volume {
    /// Every opened object must prove its identity (file id): a network location (opens are
    /// resolved by path on the server) or a file system not known to resolve relative opens in the
    /// held object itself (see [`ids_required`]).
    strict: bool,
    /// Share mode for held folders.
    dir_share: u32,
    /// The listing class with 128-bit ids works here (cleared once if the file system rejects it).
    extended_listing: Cell<bool>,
}

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
    // A probe handle (no delete access, so it does not conflict with the share mode chosen below)
    // says what the top entry is and where it lives. The working handle is opened again from it: on
    // a local volume that is the same object without resolving the path again; on a network
    // location the server resolves the path again, so the two ids must match.
    let probe = with_sharing_retry(|| open_root(root)).map_err(|e| io_err(root, e))?;
    let tag = attribute_tag(&probe).map_err(|e| io_err(root, e))?;
    let remote = is_remote(&probe).map_err(|e| io_err(root, e))?;
    let strict = ids_required(remote, file_system_name(&probe).as_deref());
    let volume = Volume {
        strict,
        dir_share: if strict { SHARE_NO_DELETE } else { SHARE_ALL },
        extended_listing: Cell::new(true),
    };
    let reopen_top = |access: u32, share: u32| -> io::Result<OwnedHandle> {
        let h = with_sharing_retry(|| reopen(&probe, access, share))?;
        if !same_object(&probe, &h, strict) {
            return Err(io::Error::other(
                "the folder changed while it was being opened for deleting",
            ));
        }
        Ok(h)
    };
    if !is_real_directory(&tag) {
        // Not a folder (any more): a link is deleted as the link, a file as the file.
        let top = reopen_top(BASE_ACCESS, SHARE_ALL).map_err(|e| io_err(root, e))?;
        drop(probe);
        delete_by_handle(&top, strict, &|| seam(Seam::BeforeReopen(root)))
            .map_err(|e| io_err(root, e))?;
        removed(0);
        return Ok(());
    }
    // Held without delete sharing too when ids are required.
    let top = reopen_top(BASE_ACCESS | FILE_LIST_DIRECTORY, volume.dir_share)
        .map_err(|e| io_err(root, e))?;
    drop(probe);
    let top_volume = object_id(&top).and_then(|(serial, _)| serial);

    let mut buf = vec![0u64; DIR_BUF_WORDS];
    let mut stack = vec![Frame::new(top, root.to_path_buf(), top_volume)];
    while let Some(frame) = stack.last_mut() {
        if cancelled() {
            return Err(WalkError::Cancelled);
        }
        if let Some(entry) = frame.pending.pop_front() {
            let child_path = frame.path.join(OsString::from_wide(&entry.name));
            seam(Seam::BeforeOpen(&child_path));
            let (access, share) = if entry.listed_as_dir {
                (BASE_ACCESS | FILE_LIST_DIRECTORY, volume.dir_share)
            } else {
                (BASE_ACCESS, SHARE_ALL)
            };
            let child = match with_sharing_retry(|| {
                open_relative(&frame.handle, &entry.name, access, share)
            }) {
                Ok(Opened::Handle(h)) => h,
                // Gone since it was listed (or being deleted by someone else): nothing to do.
                Ok(Opened::Gone) => continue,
                Ok(Opened::ParentBecameLink) => {
                    // The folder we hold was turned into a junction/symlink in place (it had to
                    // be emptied for that). Its listed names now resolve only in the link's
                    // target, which is never entered: stop listing it and delete the link
                    // itself through the handle we hold. Anything else (a non-link reparse point
                    // that a filter does not resolve for a relative open) stops the walk.
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
            match verify_identity(&entry, &child, frame.volume, &volume) {
                Identity::Same => {}
                Identity::Unverifiable => {
                    // On a network location an unverified handle may be any object the server
                    // resolved the joined path to: never use it.
                    drop(child);
                    return Err(io_err(
                        &child_path,
                        io::Error::new(
                            io::ErrorKind::Unsupported,
                            "this network location does not report file ids, so the delete cannot \
                             make sure it stays inside the folder; use the Recycle Bin or delete it \
                             on the server",
                        ),
                    ));
                }
                Identity::Changed => {
                    // Not the object the held parent listed (swapped, or a path resolved through
                    // a junction on a server): drop it unused and list the parent again.
                    drop(child);
                    if frame.rescans >= RESCANS {
                        return Err(io_err(
                            &child_path,
                            io::Error::other(
                                "the entry kept changing while the folder was being deleted",
                            ),
                        ));
                    }
                    frame.rescan();
                    continue;
                }
            }
            let tag = attribute_tag(&child).map_err(|e| io_err(&child_path, e))?;
            if is_real_directory(&tag) {
                let child = if entry.listed_as_dir {
                    child
                } else if volume.strict {
                    // Listed as a file, a folder now: another object. (A held folder on a network
                    // location must be opened without delete sharing, which this handle was not.)
                    drop(child);
                    if frame.rescans >= RESCANS {
                        return Err(io_err(
                            &child_path,
                            io::Error::other(
                                "the entry kept changing while the folder was being deleted",
                            ),
                        ));
                    }
                    frame.rescan();
                    continue;
                } else {
                    // Listed as a file, a folder now: open the same object again, able to list.
                    reopen(&child, BASE_ACCESS | FILE_LIST_DIRECTORY, SHARE_ALL)
                        .map_err(|e| io_err(&child_path, e))?
                };
                let child_volume = object_id(&child).and_then(|(serial, _)| serial);
                seam(Seam::OpenedDir(&child_path));
                stack.push(Frame::new(child, child_path, child_volume));
                continue;
            }
            // A file, or a link of any kind (deleted as the link, never entered).
            delete_by_handle(&child, volume.strict, &|| {
                seam(Seam::BeforeReopen(&child_path))
            })
            .map_err(|e| io_err(&child_path, e))?;
            drop(child);
            let is_plain_file =
                tag.FileAttributes & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT) == 0;
            // The size is the listed one (a hard-linked file counts once per name).
            removed(if is_plain_file { entry.size } else { 0 });
            continue;
        }
        if !frame.listed_to_end {
            let restart = std::mem::replace(&mut frame.restart, false);
            let more = list_batch(
                &frame.handle,
                &mut buf,
                restart,
                &volume,
                &mut frame.pending,
            )
            .map_err(|e| io_err(&frame.path, e))?;
            frame.listed_to_end = !more;
            seam(Seam::Listed(&frame.path));
            continue;
        }
        // Listed to the end and every entry handled: delete the folder itself.
        let frame_path = frame.path.clone();
        match delete_by_handle(&frame.handle, volume.strict, &|| {
            seam(Seam::BeforeReopen(&frame_path))
        }) {
            Ok(()) => {
                stack.pop();
                removed(0);
            }
            Err(e)
                if e.raw_os_error() == Some(ERROR_DIR_NOT_EMPTY as i32)
                    && frame.rescans < RESCANS =>
            {
                frame.rescan();
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
    /// From the parent's listing; `None` when the file system reports none.
    id: Option<FileId>,
}

struct Frame {
    handle: OwnedHandle,
    /// For messages and test seams only.
    path: PathBuf,
    /// Serial number of the volume the held folder is on, when known.
    volume: Option<u64>,
    pending: VecDeque<Entry>,
    restart: bool,
    listed_to_end: bool,
    rescans: u32,
}

impl Frame {
    fn new(handle: OwnedHandle, path: PathBuf, volume: Option<u64>) -> Self {
        Self {
            handle,
            path,
            volume,
            pending: VecDeque::new(),
            restart: true,
            listed_to_end: false,
            rescans: 0,
        }
    }

    /// Forget what was listed and list the folder again from the start (bounded by the caller).
    fn rescan(&mut self) {
        self.rescans += 1;
        self.pending.clear();
        self.listed_to_end = false;
        self.restart = true;
        std::thread::sleep(Duration::from_millis(u64::from(self.rescans)));
    }
}

enum Identity {
    /// The opened object is the listed one.
    Same,
    /// It is another object.
    Changed,
    /// Ids are not available (accepted on a local volume only).
    Unverifiable,
}

#[cfg(test)]
thread_local! {
    /// Tests: entries with this name are reported as changed after their open.
    static FORCE_ID_MISMATCH: std::cell::RefCell<Option<OsString>> =
        const { std::cell::RefCell::new(None) };
}

fn verify_identity(
    entry: &Entry,
    child: &OwnedHandle,
    parent_volume: Option<u64>,
    volume: &Volume,
) -> Identity {
    #[cfg(test)]
    if FORCE_ID_MISMATCH.with(|f| {
        f.borrow()
            .as_ref()
            .is_some_and(|n| *n == OsString::from_wide(&entry.name))
    }) {
        return Identity::Changed;
    }
    let opened = object_id(child);
    let (Some(listed), Some((serial, id))) = (entry.id, opened) else {
        return if volume.strict {
            Identity::Unverifiable
        } else {
            // A known local file system: the relative open was resolved in the held object itself.
            Identity::Same
        };
    };
    if id != listed {
        return Identity::Changed;
    }
    match (parent_volume, serial) {
        (Some(p), Some(c)) if p != c => Identity::Changed,
        _ => Identity::Same,
    }
}

/// The volume serial (when known) and the 128-bit id of the object behind `handle`; `None` when the
/// file system reports no usable id.
fn object_id(handle: &OwnedHandle) -> Option<(Option<u64>, FileId)> {
    let mut info = FILE_ID_INFO::default();
    // SAFETY: `info` is a writable FILE_ID_INFO and the size passed is its size.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            handle.as_raw_handle(),
            FileIdInfo,
            (&raw mut info).cast(),
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        )
    };
    if ok != 0 {
        let id = info.FileId.Identifier;
        return (id != [0; 16]).then_some((Some(info.VolumeSerialNumber), id));
    }
    // Older file systems: the 64-bit index, widened the way FileIdInfo widens it on NTFS.
    let mut bh = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: `bh` is a writable BY_HANDLE_FILE_INFORMATION.
    let ok = unsafe { GetFileInformationByHandle(handle.as_raw_handle(), &mut bh) };
    if ok == 0 {
        return None;
    }
    let index = (u64::from(bh.nFileIndexHigh) << 32) | u64::from(bh.nFileIndexLow);
    (index != 0).then_some((Some(u64::from(bh.dwVolumeSerialNumber)), widen(index)))
}

#[cfg(test)]
thread_local! {
    /// Tests: a re-opened handle is reported as another object.
    static FORCE_REOPEN_MISMATCH: Cell<bool> = const { Cell::new(false) };
}

/// Whether `b` (opened again from `a`) is provably the same object as `a`. Ids that cannot be read
/// count as "same" only when ids are not required on this volume.
fn same_object(a: &OwnedHandle, b: &OwnedHandle, strict: bool) -> bool {
    #[cfg(test)]
    if FORCE_REOPEN_MISMATCH.with(|f| f.get()) {
        return false;
    }
    match (object_id(a), object_id(b)) {
        (Some((serial_a, id_a)), Some((serial_b, id_b))) => {
            id_a == id_b && (serial_a.is_none() || serial_b.is_none() || serial_a == serial_b)
        }
        _ => !strict,
    }
}

/// Local file systems known to resolve a handle-relative open in the held object itself (the
/// kernel's own drivers). Anything else (a network location, or a third-party file system that may
/// resolve by path in user mode, e.g. WinFsp/Dokan) must prove every entry's identity.
const KNOWN_LOCAL_FILE_SYSTEMS: [&str; 5] = ["NTFS", "ReFS", "FAT", "FAT32", "exFAT"];

fn ids_required(remote: bool, file_system: Option<&str>) -> bool {
    remote
        || !file_system.is_some_and(|name| {
            KNOWN_LOCAL_FILE_SYSTEMS
                .iter()
                .any(|known| known.eq_ignore_ascii_case(name))
        })
}

/// The file system name of the volume of `handle` (`FileFsAttributeInformation`), if it answers.
fn file_system_name(handle: &OwnedHandle) -> Option<String> {
    // FILE_FS_ATTRIBUTE_INFORMATION: attributes (u32), max component length (i32), name length in
    // bytes (u32), then the name. 8-byte aligned buffer, far larger than any file system name.
    let mut buf = [0u64; 64];
    let mut status_block = IO_STATUS_BLOCK::default();
    // SAFETY: `buf` is writable for the size passed, `status_block` is writable, and `handle` is a
    // live handle we own.
    let status = unsafe {
        NtQueryVolumeInformationFile(
            handle.as_raw_handle(),
            &mut status_block,
            buf.as_mut_ptr().cast(),
            std::mem::size_of_val(&buf) as u32,
            FileFsAttributeInformation,
        )
    };
    if status < 0 {
        return None;
    }
    let bytes: Vec<u8> = buf.iter().flat_map(|w| w.to_le_bytes()).collect();
    let name_len = u32::from_le_bytes(bytes[8..12].try_into().ok()?) as usize;
    let name_bytes = bytes.get(12..12usize.checked_add(name_len)?)?;
    if !name_len.is_multiple_of(2) {
        return None;
    }
    let units: Vec<u16> = name_bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .collect();
    String::from_utf16(&units).ok()
}

fn widen(index: u64) -> FileId {
    let mut id = [0u8; 16];
    id[..8].copy_from_slice(&index.to_le_bytes());
    id
}

/// `FILE_FS_DEVICE_INFORMATION` (ntifs.h), declared here to avoid one more windows-sys feature.
#[repr(C)]
#[derive(Default)]
struct FsDeviceInformation {
    device_type: u32,
    characteristics: u32,
}
/// `FILE_REMOTE_DEVICE` in `FILE_FS_DEVICE_INFORMATION::Characteristics`.
const FILE_REMOTE_DEVICE: u32 = 0x10;

/// UNC path or mapped network drive, as the volume of `handle` reports it. An error (the question
/// cannot be answered) stops the walk rather than guessing "local".
fn is_remote(handle: &OwnedHandle) -> io::Result<bool> {
    #[cfg(test)]
    if FORCE_REMOTE.with(|f| f.get()) {
        return Ok(true);
    }
    let mut info = FsDeviceInformation::default();
    let mut status_block = IO_STATUS_BLOCK::default();
    // SAFETY: `info` is a writable FILE_FS_DEVICE_INFORMATION-shaped struct of the size passed,
    // `status_block` is writable, and `handle` is a live handle we own.
    let status = unsafe {
        NtQueryVolumeInformationFile(
            handle.as_raw_handle(),
            &mut status_block,
            (&raw mut info).cast(),
            std::mem::size_of::<FsDeviceInformation>() as u32,
            FileFsDeviceInformation,
        )
    };
    if status < 0 {
        return Err(nt_error(status));
    }
    Ok(info.characteristics & FILE_REMOTE_DEVICE != 0)
}

#[cfg(test)]
thread_local! {
    /// Tests: treat a local walk as a network one (ids required, folders held without delete share).
    static FORCE_REMOTE: Cell<bool> = const { Cell::new(false) };
}

fn is_real_directory(tag: &FILE_ATTRIBUTE_TAG_INFO) -> bool {
    tag.FileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0 && !is_link(tag)
}

/// A name-surrogate reparse point (junction, directory or file symlink). A directory with another
/// kind of reparse point is entered like a folder (see the module docs).
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

/// The top folder, by path (the path the user chose), without following it if it is a link. Only a
/// probe: no delete access (the working handle is opened from it).
fn open_root(path: &Path) -> io::Result<OwnedHandle> {
    let file = fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES | SYNCHRONIZE)
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

/// Whether `OBJ_DONT_REPARSE` is accepted (cleared once if the kernel rejects it, as std does).
static DONT_REPARSE_SUPPORTED: AtomicBool = AtomicBool::new(true);

#[cfg(test)]
thread_local! {
    /// Tests: open as a system without `OBJ_DONT_REPARSE` would.
    static FORCE_NO_DONT_REPARSE: Cell<bool> = const { Cell::new(false) };
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
/// `root`'s own object again, no name resolved).
///
/// `FILE_OPEN_REPARSE_POINT`: a final reparse point is opened as itself, never followed. On a local
/// volume a name relative to a handle cannot be redirected either: when `root` itself has been
/// turned into a junction in place, the kernel answers STATUS_REPARSE_POINT_NOT_RESOLVED, with or
/// without `OBJ_DONT_REPARSE` (both tested). `OBJ_DONT_REPARSE` is added on top where the kernel
/// knows it; when NtCreateFile itself rejects it with STATUS_INVALID_PARAMETER it is dropped from
/// then on, as std does. (On a network location none of this holds: see the identity check.)
fn nt_open(
    root: &OwnedHandle,
    name: &[u16],
    access: u32,
    share: u32,
) -> Result<OwnedHandle, NTSTATUS> {
    // Not a kernel answer: never mistaken for one below (it does not touch OBJ_DONT_REPARSE).
    let byte_len = u16::try_from(name.len() * 2).map_err(|_| STATUS_NAME_TOO_LONG)?;
    loop {
        let flag = dont_reparse_flag();
        match nt_open_once(root, name, byte_len, access, share, flag) {
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
    byte_len: u16,
    access: u32,
    share: u32,
    attributes: u32,
) -> Result<OwnedHandle, NTSTATUS> {
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
    // `unicode.Buffer` for exactly `byte_len` bytes; NtCreateFile only reads the name) outlives
    // them. `RootDirectory` is a live handle we own. No allocation size, no EA buffer.
    let status = unsafe {
        NtCreateFile(
            &mut handle,
            access,
            &object,
            &mut status_block,
            std::ptr::null(),
            0,
            share,
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
/// final reparse point.
fn open_relative(
    parent: &OwnedHandle,
    name: &[u16],
    access: u32,
    share: u32,
) -> io::Result<Opened> {
    // A listed name is one component of a plain file or folder. A separator, NUL or a ':' (a
    // stream) would be a bug or a hostile file system: refuse rather than resolve it.
    let refused = |c: u16| c == 0 || c == u16::from(b'/') || c == u16::from(b':') || c == 0x5C;
    if name.is_empty() || name.iter().any(|&c| refused(c)) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the folder listing returned an invalid name",
        ));
    }
    match nt_open(parent, name, access, share) {
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

/// Open the same object as `handle` again with `access` and `share` (no name is resolved; works for
/// folders, unlike `ReOpenFile`).
fn reopen(handle: &OwnedHandle, access: u32, share: u32) -> io::Result<OwnedHandle> {
    nt_open(handle, &[], access, share).map_err(nt_error)
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
/// Uses the listing with 128-bit ids; a file system that does not know it gets the 64-bit one.
fn list_batch(
    dir: &OwnedHandle,
    buf: &mut [u64],
    restart: bool,
    volume: &Volume,
    out: &mut VecDeque<Entry>,
) -> io::Result<bool> {
    loop {
        let extended = volume.extended_listing.get();
        let class = match (extended, restart) {
            (true, true) => FileIdExtdDirectoryRestartInfo,
            (true, false) => FileIdExtdDirectoryInfo,
            (false, true) => FileIdBothDirectoryRestartInfo,
            (false, false) => FileIdBothDirectoryInfo,
        };
        let byte_len = std::mem::size_of_val(buf);
        // SAFETY: `buf` is writable for `byte_len` bytes and 8-byte aligned (u64 elements), as both
        // listing structures require.
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
            let code = e.raw_os_error();
            if code == Some(ERROR_NO_MORE_FILES as i32) {
                return Ok(false);
            }
            let unsupported = [
                ERROR_INVALID_PARAMETER,
                ERROR_NOT_SUPPORTED,
                ERROR_INVALID_LEVEL,
            ]
            .iter()
            .any(|&c| code == Some(c as i32));
            // Switch only at the start of a listing (a continuation must keep its class).
            if extended && restart && unsupported {
                volume.extended_listing.set(false);
                continue;
            }
            return Err(e);
        }
        if extended {
            parse_batch_extd(buf, out)?;
        } else {
            parse_batch_both(buf, out)?;
        }
        return Ok(true);
    }
}

/// The fields of one listing record that the walker uses.
struct Record<'a> {
    next: u32,
    attributes: u32,
    end_of_file: i64,
    id: Option<FileId>,
    name: &'a [u16],
}

/// Walk the records of one listing batch. `read` copies the fixed part of the record at an offset
/// that `walk_records` has checked to hold a whole `T`.
fn walk_records<'a, T: Copy>(
    buf: &'a [u64],
    name_offset: usize,
    fields: impl Fn(&T) -> (u32, u32, u32, i64, Option<FileId>),
    out: &mut VecDeque<Entry>,
) -> io::Result<()> {
    let base = buf.as_ptr().cast::<u8>();
    let byte_len = std::mem::size_of_val(buf);
    let bad = || io::Error::new(io::ErrorKind::InvalidData, "malformed folder listing");
    let mut offset = 0usize;
    loop {
        // The whole fixed structure must be inside the buffer before it is read (a real record is
        // never shorter: its name is at least one character and records are 8-aligned).
        if offset
            .checked_add(std::mem::size_of::<T>())
            .is_none_or(|end| end > byte_len)
            || !offset.is_multiple_of(8)
        {
            return Err(bad());
        }
        // SAFETY: `offset + size_of::<T>() <= byte_len` was checked, so the read stays inside
        // `buf`; `read_unaligned` needs no alignment and `T` is a plain-old-data listing struct.
        let fixed: T = unsafe { std::ptr::read_unaligned(base.add(offset).cast::<T>()) };
        let (next, name_len, attributes, end_of_file, id) = fields(&fixed);
        let name_bytes = name_len as usize;
        if !name_bytes.is_multiple_of(2)
            || offset
                .checked_add(name_offset + name_bytes)
                .is_none_or(|end| end > byte_len)
        {
            return Err(bad());
        }
        // SAFETY: the name lies inside `buf` (checked just above) and is 2-aligned (8-aligned
        // record + even offset of FileName); `buf` is not written while the slice lives.
        let name: &'a [u16] = unsafe {
            std::slice::from_raw_parts(base.add(offset + name_offset).cast::<u16>(), name_bytes / 2)
        };
        let record = Record {
            next,
            attributes,
            end_of_file,
            id,
            name,
        };
        let dot = u16::from(b'.');
        if record.name != [dot] && record.name != [dot, dot] {
            out.push_back(Entry {
                name: record.name.to_vec(),
                listed_as_dir: record.attributes & FILE_ATTRIBUTE_DIRECTORY != 0,
                size: u64::try_from(record.end_of_file).unwrap_or(0),
                id: record.id,
            });
        }
        if record.next == 0 {
            return Ok(());
        }
        offset = offset.checked_add(record.next as usize).ok_or_else(bad)?;
    }
}

fn parse_batch_extd(buf: &[u64], out: &mut VecDeque<Entry>) -> io::Result<()> {
    walk_records::<FILE_ID_EXTD_DIR_INFO>(
        buf,
        std::mem::offset_of!(FILE_ID_EXTD_DIR_INFO, FileName),
        |r| {
            let id = r.FileId.Identifier;
            (
                r.NextEntryOffset,
                r.FileNameLength,
                r.FileAttributes,
                r.EndOfFile,
                (id != [0; 16]).then_some(id),
            )
        },
        out,
    )
}

fn parse_batch_both(buf: &[u64], out: &mut VecDeque<Entry>) -> io::Result<()> {
    walk_records::<FILE_ID_BOTH_DIR_INFO>(
        buf,
        std::mem::offset_of!(FILE_ID_BOTH_DIR_INFO, FileName),
        |r| {
            let index = r.FileId as u64;
            (
                r.NextEntryOffset,
                r.FileNameLength,
                r.FileAttributes,
                r.EndOfFile,
                (index != 0).then(|| widen(index)),
            )
        },
        out,
    )
}

#[cfg(test)]
thread_local! {
    /// Tests: take the fallback path (classic disposition) as on a file system without POSIX delete.
    static FORCE_LEGACY_DELETE: Cell<bool> = const { Cell::new(false) };
    /// Tests: the classic disposition fails even after read-only was cleared.
    static FORCE_LEGACY_FAILURE: Cell<bool> = const { Cell::new(false) };
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
/// `strict`: ids are required on this volume (see [`legacy_delete`]); `before_reopen` is a test seam.
fn delete_by_handle(
    handle: &OwnedHandle,
    strict: bool,
    before_reopen: &dyn Fn(),
) -> io::Result<()> {
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
    legacy_delete(handle, strict, before_reopen)
}

fn set_legacy_disposition(handle: &OwnedHandle) -> io::Result<()> {
    #[cfg(test)]
    if FORCE_LEGACY_FAILURE.with(|f| f.get()) {
        // Only the retry after clearing read-only is made to fail.
        let tag = attribute_tag(handle)?;
        if tag.FileAttributes & FILE_ATTRIBUTE_READONLY == 0 {
            return Err(io::Error::from_raw_os_error(ERROR_SHARING_VIOLATION as i32));
        }
    }
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

fn set_basic_attributes(handle: &OwnedHandle, attributes: u32) -> io::Result<()> {
    // Times 0 = "leave unchanged".
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
            handle.as_raw_handle(),
            FileBasicInfo,
            (&raw const update).cast(),
            std::mem::size_of::<FILE_BASIC_INFO>() as u32,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// The classic delete: the name goes when the last handle closes. A read-only entry is made
/// writable first, through a second handle opened from the first one. On a local volume that is the
/// same object; on a network location the server resolves the path again, so the second handle
/// must prove it is the same object (file id) before any attribute is changed; otherwise the
/// original error is returned and nothing is touched. If the delete still fails, the read-only
/// attribute is put back.
fn legacy_delete(handle: &OwnedHandle, strict: bool, before_reopen: &dyn Fn()) -> io::Result<()> {
    match set_legacy_disposition(handle) {
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
            let tag = attribute_tag(handle)?;
            if tag.FileAttributes & FILE_ATTRIBUTE_READONLY == 0 {
                return Err(e);
            }
            before_reopen();
            let writer = match reopen(
                handle,
                FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES | SYNCHRONIZE,
                SHARE_ALL,
            ) {
                Ok(w) if same_object(handle, &w, strict) => w,
                _ => return Err(e),
            };
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
            set_basic_attributes(&writer, basic.FileAttributes & !FILE_ATTRIBUTE_READONLY)?;
            set_legacy_disposition(handle).inspect_err(|_| {
                // Best effort: the entry stays, so it keeps its read-only attribute.
                let _ = set_basic_attributes(&writer, basic.FileAttributes);
            })
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
        convert_in_place(dir, target, false).expect("FSCTL_SET_REPARSE_POINT (mount point)");
    }

    /// The same with a directory SYMLINK reparse point (needs the symlink privilege).
    fn convert_in_place(dir: &Path, target: &Path, symlink: bool) -> io::Result<()> {
        use windows_sys::Win32::System::IO::DeviceIoControl;
        const FSCTL_SET_REPARSE_POINT: u32 = 0x0009_00A4;
        const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;
        const IO_REPARSE_TAG_SYMLINK: u32 = 0xA000_000C;
        let h = fs::OpenOptions::new()
            .access_mode(FILE_WRITE_ATTRIBUTES)
            .share_mode(SHARE_ALL)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(dir)?;
        let sub: Vec<u16> = format!(r"\??\{}", target.display())
            .encode_utf16()
            .collect();
        let sub_bytes = (sub.len() * 2) as u16;
        let print: Vec<u16> = target
            .as_os_str()
            .to_string_lossy()
            .encode_utf16()
            .collect();
        let print_bytes = (print.len() * 2) as u16;
        let mut path_buffer = sub.clone();
        path_buffer.push(0);
        path_buffer.extend(&print);
        path_buffer.push(0);
        let mut buf: Vec<u8> = Vec::new();
        let (tag, header) = if symlink {
            (IO_REPARSE_TAG_SYMLINK, 12)
        } else {
            (IO_REPARSE_TAG_MOUNT_POINT, 8)
        };
        buf.extend(tag.to_le_bytes());
        buf.extend(((header + path_buffer.len() * 2) as u16).to_le_bytes());
        buf.extend(0u16.to_le_bytes());
        buf.extend(0u16.to_le_bytes()); // substitute offset
        buf.extend(sub_bytes.to_le_bytes());
        buf.extend((sub_bytes + 2).to_le_bytes()); // print name offset
        buf.extend(print_bytes.to_le_bytes()); // print name length
        if symlink {
            buf.extend(0u32.to_le_bytes()); // flags: absolute
        }
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
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    static SKIPPED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    /// A test that cannot run here says so loudly (and fails when FERRUM_REQUIRE_ALL_TESTS is set,
    /// e.g. on a machine with the symlink privilege and the admin share).
    fn skip(test: &str, reason: &str) {
        let n = SKIPPED.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        let msg = format!("SKIPPED ({n} so far in this run): {test}: {reason}");
        if std::env::var_os("FERRUM_REQUIRE_ALL_TESTS").is_some() {
            panic!("{msg}");
        }
        // Straight to the process's stderr: the test harness captures `eprintln!` of a passing
        // test, which would hide the skip (in CI logs too).
        use std::io::Write;
        let _ = writeln!(std::io::stderr().lock(), "{msg}");
    }

    /// `\\localhost\<drive>$\...` for a local path, if the admin share answers here.
    fn admin_share_path(local: &Path) -> Option<PathBuf> {
        let s = local.to_string_lossy();
        let bytes = s.as_bytes();
        if bytes.len() < 3 || bytes[1] != b':' {
            return None;
        }
        let unc = PathBuf::from(format!(
            r"\\localhost\{}${}",
            s[..1].to_lowercase(),
            &s[2..]
        ));
        fs::metadata(&unc).ok().map(|_| unc)
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
            // Creating a symlink needs Developer Mode or the privilege.
            skip(
                "a_file_symlink_inside_the_tree",
                &format!("cannot create a symlink ({e})"),
            );
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
    fn parse_batch_rejects_records_running_past_the_buffer() {
        fn words(bytes: &[u8; 128]) -> Vec<u64> {
            bytes
                .as_chunks::<8>()
                .0
                .iter()
                .map(|c| u64::from_le_bytes(*c))
                .collect()
        }
        type Parser = fn(&[u64], &mut VecDeque<Entry>) -> io::Result<()>;
        let cases: [(Parser, usize, usize, usize); 2] = [
            (
                parse_batch_both,
                std::mem::offset_of!(FILE_ID_BOTH_DIR_INFO, FileName),
                std::mem::offset_of!(FILE_ID_BOTH_DIR_INFO, FileNameLength),
                std::mem::size_of::<FILE_ID_BOTH_DIR_INFO>(),
            ),
            (
                parse_batch_extd,
                std::mem::offset_of!(FILE_ID_EXTD_DIR_INFO, FileName),
                std::mem::offset_of!(FILE_ID_EXTD_DIR_INFO, FileNameLength),
                std::mem::size_of::<FILE_ID_EXTD_DIR_INFO>(),
            ),
        ];
        for (parse, name_offset, fnl, size) in cases {
            let mut bytes = [0u8; 128];
            // A name running past the end.
            bytes[fnl..fnl + 4].copy_from_slice(&(((128 - name_offset) + 2) as u32).to_le_bytes());
            let mut out = VecDeque::new();
            assert!(parse(&words(&bytes), &mut out).is_err());
            bytes[fnl..fnl + 4].copy_from_slice(&2u32.to_le_bytes());
            bytes[name_offset..name_offset + 2].copy_from_slice(&u16::from(b'x').to_le_bytes());
            // NextEntryOffset pointing outside.
            bytes[0..4].copy_from_slice(&200u32.to_le_bytes());
            out.clear();
            assert!(parse(&words(&bytes), &mut out).is_err());
            // Review N1: a next record whose fixed part would end past the buffer, although its
            // name offset alone would still fit.
            let next = (128 - size + 8) as u32;
            assert!(next as usize + name_offset <= 128 && next as usize + size > 128);
            bytes[0..4].copy_from_slice(&next.to_le_bytes());
            out.clear();
            assert!(parse(&words(&bytes), &mut out).is_err());
            bytes[0..4].copy_from_slice(&0u32.to_le_bytes());
            out.clear();
            parse(&words(&bytes), &mut out).unwrap();
            assert_eq!(out.len(), 1);
            assert_eq!(out[0].name, vec![u16::from(b'x')]);
        }
    }

    #[test]
    fn a_name_with_a_separator_or_a_stream_colon_is_never_opened() {
        let fx = fixture();
        let dir = open_root(&fx.root).unwrap();
        for bad in ["a:b", "x::$DATA", "a/b", r"a\b", ""] {
            let name: Vec<u16> = bad.encode_utf16().collect();
            let r = open_relative(&dir, &name, BASE_ACCESS, SHARE_ALL);
            assert!(
                matches!(&r, Err(e) if e.kind() == io::ErrorKind::InvalidData),
                "{bad:?} must be refused"
            );
        }
    }

    #[test]
    fn an_entry_whose_opened_id_differs_from_the_listing_is_never_deleted() {
        let fx = fixture();
        let top = fx.root.join("ids");
        fs::create_dir_all(&top).unwrap();
        fs::write(top.join("a.txt"), b"x").unwrap();
        fs::write(top.join("f.txt"), b"x").unwrap();
        FORCE_ID_MISMATCH.with(|f| *f.borrow_mut() = Some(OsString::from("f.txt")));
        let (r, _, _) = run(&top);
        FORCE_ID_MISMATCH.with(|f| *f.borrow_mut() = None);
        match r {
            Err(WalkError::Io { path, error }) => {
                assert_eq!(path, top.join("f.txt"));
                assert!(error.to_string().contains("kept changing"), "{error}");
            }
            other => panic!("expected the changed entry to stop the walk, got {other:?}"),
        }
        assert!(
            top.join("f.txt").exists(),
            "a mismatching handle is never used"
        );
    }

    #[test]
    fn a_network_style_walk_holds_folders_without_delete_sharing_and_verifies_ids() {
        let fx = fixture();
        let (v, _) = victim(&fx.root);
        let top = fx.root.join("tree");
        let d = top.join("d");
        fs::create_dir_all(d.join("nested")).unwrap();
        fs::write(d.join("secret.txt"), b"decoy").unwrap();
        let rename_refused = Cell::new(false);
        FORCE_REMOTE.with(|f| f.set(true));
        let r = remove_tree_with(&top, &never, &mut |_| {}, &|s| {
            if let Seam::Listed(p) = s
                && p == d
                && !rename_refused.get()
            {
                // Scenario D: rename the held folder away to put a junction in its place.
                match fs::rename(&d, fx.root.join("d-moved")) {
                    Err(e) => {
                        assert_eq!(e.raw_os_error(), Some(ERROR_SHARING_VIOLATION as i32));
                        rename_refused.set(true);
                    }
                    Ok(()) => panic!("a held folder could be renamed away"),
                }
            }
        });
        FORCE_REMOTE.with(|f| f.set(false));
        r.unwrap();
        assert!(rename_refused.get());
        assert!(!top.exists());
        assert_victim_intact(&v);
    }

    #[test]
    fn the_classic_delete_puts_read_only_back_when_it_still_fails() {
        let fx = fixture();
        let top = fx.root.join("keep-ro");
        fs::create_dir_all(&top).unwrap();
        let f = top.join("f.txt");
        fs::write(&f, b"x").unwrap();
        set_readonly(&f);
        FORCE_LEGACY_DELETE.with(|x| x.set(true));
        FORCE_LEGACY_FAILURE.with(|x| x.set(true));
        let (r, _, _) = run(&top);
        FORCE_LEGACY_FAILURE.with(|x| x.set(false));
        FORCE_LEGACY_DELETE.with(|x| x.set(false));
        assert!(r.is_err());
        assert!(f.exists());
        assert!(
            fs::metadata(&f).unwrap().permissions().readonly(),
            "the read-only attribute is restored"
        );
    }

    #[test]
    fn a_held_folder_turned_into_a_directory_symlink_in_place_never_reaches_the_target() {
        let fx = fixture();
        let (v, _) = victim(&fx.root);
        let top = fx.root.join("tree");
        let d = top.join("d");
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("secret.txt"), b"decoy").unwrap();
        let outcome = std::cell::RefCell::new(None::<io::Result<()>>);
        let r = remove_tree_with(&top, &never, &mut |_| {}, &|s| {
            if let Seam::Listed(p) = s
                && p == d
                && outcome.borrow().is_none()
            {
                fs::remove_file(d.join("secret.txt")).unwrap();
                *outcome.borrow_mut() = Some(convert_in_place(&d, &v, true));
            }
        });
        match outcome.into_inner() {
            Some(Ok(())) => {
                assert_victim_intact(&v);
                r.unwrap();
                assert!(!top.exists());
            }
            Some(Err(e)) => {
                skip(
                    "a_held_folder_turned_into_a_directory_symlink_in_place",
                    &format!("cannot set a symlink reparse point ({e})"),
                );
                assert_victim_intact(&v);
            }
            None => panic!("the seam never ran"),
        }
    }

    /// Review B1: the same races through the SMB redirector (`\\localhost\<drive>$`), where a
    /// "relative" open is sent to the server as a joined path.
    #[test]
    fn over_smb_a_held_folder_turned_into_a_junction_in_place_never_reaches_the_target() {
        let fx = fixture();
        let Some(unc_root) = admin_share_path(&fx.root) else {
            skip(
                "over_smb_a_held_folder_turned_into_a_junction_in_place",
                r"the admin share \\localhost\<drive>$ is not reachable here",
            );
            return;
        };
        let (v, _) = victim(&fx.root);
        let top = fx.root.join("tree");
        let d = top.join("d");
        fs::create_dir_all(d.join("nested")).unwrap();
        fs::write(d.join("secret.txt"), b"decoy").unwrap();
        fs::write(d.join("nested").join("deep.txt"), b"decoy").unwrap();
        let unc_top = unc_root.join("tree");
        let unc_d = unc_top.join("d");
        let converted = Cell::new(false);
        let r = remove_tree_with(&unc_top, &never, &mut |_| {}, &|s| {
            if let Seam::Listed(p) = s
                && p == unc_d
                && !converted.get()
            {
                converted.set(true);
                fs::remove_dir_all(d.join("nested")).unwrap();
                fs::remove_file(d.join("secret.txt")).unwrap();
                convert_in_place_to_junction(&d, &v);
            }
        });
        assert!(converted.get());
        assert_victim_intact(&v);
        r.unwrap();
        assert!(!top.exists());
    }

    #[test]
    fn over_smb_a_held_folder_cannot_be_renamed_away_and_replaced_by_a_junction() {
        let fx = fixture();
        let Some(unc_root) = admin_share_path(&fx.root) else {
            skip(
                "over_smb_a_held_folder_cannot_be_renamed_away",
                r"the admin share \\localhost\<drive>$ is not reachable here",
            );
            return;
        };
        let (v, _) = victim(&fx.root);
        let top = fx.root.join("tree");
        let d = top.join("d");
        fs::create_dir_all(d.join("nested")).unwrap();
        fs::write(d.join("secret.txt"), b"decoy").unwrap();
        fs::write(d.join("nested").join("deep.txt"), b"decoy").unwrap();
        let unc_top = unc_root.join("tree");
        let unc_d = unc_top.join("d");
        let attempted = Cell::new(false);
        let r = remove_tree_with(&unc_top, &never, &mut |_| {}, &|s| {
            if let Seam::Listed(p) = s
                && p == unc_d
                && !attempted.replace(true)
                && fs::rename(&d, fx.root.join("d-moved")).is_ok()
            {
                // Should not happen (held without delete sharing); if it does, the id check
                // must still keep the victim safe.
                make_junction(&d, &v);
            }
        });
        assert!(attempted.get());
        assert_victim_intact(&v);
        assert!(
            !fx.root.join("d-moved").exists(),
            "the held folder could be renamed away"
        );
        r.unwrap();
        assert!(!top.exists());
    }

    #[test]
    fn ids_are_required_off_the_known_local_file_systems() {
        for fs_name in ["NTFS", "ReFS", "FAT", "FAT32", "exFAT", "ntfs"] {
            assert!(!ids_required(false, Some(fs_name)), "{fs_name}");
            assert!(ids_required(true, Some(fs_name)), "remote {fs_name}");
        }
        for fs_name in ["FUSE-rclone", "Dokan", "CDFS", "UDF", ""] {
            assert!(ids_required(false, Some(fs_name)), "{fs_name}");
        }
        assert!(ids_required(false, None), "unknown file system");
    }

    #[test]
    fn the_test_volume_is_a_known_local_file_system() {
        let fx = fixture();
        let h = open_root(&fx.root).unwrap();
        let name = file_system_name(&h);
        assert!(!is_remote(&h).unwrap());
        assert!(
            !ids_required(false, name.as_deref()),
            "temp folder on {name:?}"
        );
    }

    #[test]
    fn a_top_folder_that_changes_between_probe_and_open_is_refused() {
        let fx = fixture();
        let top = fx.root.join("t");
        fs::create_dir_all(&top).unwrap();
        fs::write(top.join("f"), b"x").unwrap();
        FORCE_REOPEN_MISMATCH.with(|f| f.set(true));
        let (r, entries, _) = run(&top);
        FORCE_REOPEN_MISMATCH.with(|f| f.set(false));
        match r {
            Err(WalkError::Io { error, .. }) => {
                assert!(error.to_string().contains("changed"), "{error}")
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
        assert_eq!(entries, 0);
        assert!(top.join("f").exists());
    }

    #[test]
    fn the_classic_delete_never_clears_read_only_on_an_object_it_cannot_prove_is_the_entry() {
        // Review R2-S1, local form: ids required (network-style), classic delete, and the second
        // handle reported as another object. The original error comes back; no attribute changes.
        let fx = fixture();
        let top = fx.root.join("t");
        fs::create_dir_all(&top).unwrap();
        let f = top.join("f.txt");
        fs::write(&f, b"x").unwrap();
        set_readonly(&f);
        let reopened = Cell::new(false);
        FORCE_REMOTE.with(|x| x.set(true));
        FORCE_LEGACY_DELETE.with(|x| x.set(true));
        let r = remove_tree_with(&top, &never, &mut |_| {}, &|s| {
            if let Seam::BeforeReopen(p) = s
                && p == f
            {
                reopened.set(true);
                FORCE_REOPEN_MISMATCH.with(|x| x.set(true));
            }
        });
        FORCE_REOPEN_MISMATCH.with(|x| x.set(false));
        FORCE_LEGACY_DELETE.with(|x| x.set(false));
        FORCE_REMOTE.with(|x| x.set(false));
        assert!(reopened.get());
        match r {
            Err(WalkError::Io { path, error }) => {
                assert_eq!(path, f);
                assert_eq!(error.kind(), io::ErrorKind::PermissionDenied, "{error}");
            }
            other => panic!("expected the original access error, got {other:?}"),
        }
        assert!(f.exists());
        assert!(fs::metadata(&f).unwrap().permissions().readonly());
    }

    /// Review R2-S1 through SMB: the classic delete's second open is sent to the server as a path.
    /// The read-only entry is moved away and its folder turned into a junction to a victim folder
    /// holding a read-only file of the same name; the victim's attribute must not change.
    #[test]
    fn over_smb_the_classic_delete_never_touches_the_attributes_of_an_object_outside_the_tree() {
        let fx = fixture();
        let Some(unc_root) = admin_share_path(&fx.root) else {
            skip(
                "over_smb_the_classic_delete_never_touches_the_attributes",
                r"the admin share \\localhost\<drive>$ is not reachable here",
            );
            return;
        };
        let v = fx.root.join("victim");
        fs::create_dir_all(&v).unwrap();
        fs::write(v.join("f.txt"), b"precious").unwrap();
        set_readonly(&v.join("f.txt"));
        let top = fx.root.join("tree");
        let d = top.join("d");
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("f.txt"), b"decoy").unwrap();
        set_readonly(&d.join("f.txt"));
        let unc_f = unc_root.join("tree").join("d").join("f.txt");
        let swapped = Cell::new(false);
        FORCE_LEGACY_DELETE.with(|x| x.set(true));
        let r = remove_tree_with(&unc_root.join("tree"), &never, &mut |_| {}, &|s| {
            if let Seam::BeforeReopen(p) = s
                && p == unc_f
                && !swapped.replace(true)
            {
                let moved = fx.root.join("f-moved.txt");
                fs::rename(d.join("f.txt"), &moved).unwrap();
                // The attacker's own file: made writable, so that its delete would succeed and
                // nothing would put the victim's attribute back.
                let mut perms = fs::metadata(&moved).unwrap().permissions();
                #[allow(clippy::permissions_set_readonly_false)]
                perms.set_readonly(false);
                fs::set_permissions(&moved, perms).unwrap();
                convert_in_place_to_junction(&d, &v);
            }
        });
        FORCE_LEGACY_DELETE.with(|x| x.set(false));
        assert!(swapped.get(), "the classic delete path was not taken");
        assert!(r.is_err(), "the unproven entry is not deleted: {r:?}");
        assert_eq!(fs::read(v.join("f.txt")).unwrap(), b"precious");
        assert!(
            fs::metadata(v.join("f.txt"))
                .unwrap()
                .permissions()
                .readonly(),
            "the victim's read-only attribute is untouched"
        );
        // Clean-up for the temp dir: the moved decoy and the junction.
        let _ = fs::remove_dir(&d);
    }

    #[test]
    fn over_smb_a_folder_swapped_for_a_junction_before_it_is_opened_is_removed_as_a_link() {
        let fx = fixture();
        let Some(unc_root) = admin_share_path(&fx.root) else {
            skip(
                "over_smb_a_folder_swapped_for_a_junction_before_it_is_opened",
                r"the admin share \\localhost\<drive>$ is not reachable here",
            );
            return;
        };
        let (v, _) = victim(&fx.root);
        let top = fx.root.join("tree");
        let sub = top.join("sub");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("secret.txt"), b"decoy").unwrap();
        let unc_sub = unc_root.join("tree").join("sub");
        let swapped = Cell::new(false);
        let r = remove_tree_with(&unc_root.join("tree"), &never, &mut |_| {}, &|s| {
            if let Seam::BeforeOpen(p) = s
                && p == unc_sub
                && !swapped.replace(true)
            {
                fs::rename(&sub, top.join("moved")).unwrap();
                make_junction(&sub, &v);
            }
        });
        assert!(swapped.get());
        assert_victim_intact(&v);
        r.unwrap();
        assert!(!top.exists());
    }
}
