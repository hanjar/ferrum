//! File identity (FER-563): decide whether two paths are the SAME file or folder on disk,
//! whatever their spelling (8.3 short names, letter case, hard links, junction aliases).
//! Lexical `PathBuf` comparison cannot see those aliases, and a move/copy that treats an alias as
//! a different destination ends with `remove(source)` deleting the only copy.

use std::path::Path;

/// Volume serial + file index of an existing entry (symlinks/junctions are followed).
/// `None` when the entry cannot be opened or identified; callers then fall back to the
/// canonical path comparison.
#[cfg(windows)]
pub fn file_key(path: &Path) -> Option<(u64, u64)> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, GetFileInformationByHandle, OPEN_EXISTING,
    };
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // Access 0: query attributes only, works for locked files and directories.
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return None;
    }
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    let ok = unsafe { GetFileInformationByHandle(handle, &mut info) };
    unsafe {
        CloseHandle(handle);
    }
    if ok == 0 {
        return None;
    }
    let index = ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64;
    Some((info.dwVolumeSerialNumber as u64, index))
}

#[cfg(unix)]
pub fn file_key(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::metadata(path).ok()?;
    Some((m.dev(), m.ino()))
}

/// True when both paths exist and are the same file or folder.
pub fn same_entry(a: &Path, b: &Path) -> bool {
    match (file_key(a), file_key(b)) {
        (Some(x), Some(y)) => x == y,
        _ => match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
            (Ok(x), Ok(y)) => x == y,
            _ => false,
        },
    }
}

/// True when `child` is `ancestor` itself or lies anywhere beneath it, by identity.
pub fn is_same_or_inside(child: &Path, ancestor: &Path) -> bool {
    let mut cur = Some(child);
    while let Some(p) = cur {
        if !p.as_os_str().is_empty() && same_entry(p, ancestor) {
            return true;
        }
        cur = p.parent();
    }
    false
}
