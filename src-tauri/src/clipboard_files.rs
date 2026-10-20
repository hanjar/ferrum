//! FER-572: Windows shell clipboard for FILES (copy / cut / paste with Explorer).
//!
//! Two layers:
//!
//! * a pure codec for the `CF_HDROP` payload (`DROPFILES` header + double-NUL terminated UTF-16
//!   path list) and for the `Preferred DropEffect` DWORD. The clipboard content comes from
//!   ANOTHER process and is untrusted, so the parser works on a byte slice with explicit bounds
//!   checks and hard limits and never indexes past the block it was given;
//! * a thin Win32 layer (`OpenClipboard` with a bounded retry, a drop guard that always closes
//!   the clipboard, `GlobalLock`/`GlobalUnlock` in pairs) that moves those bytes in and out.
//!
//! This module only transports the path list. The paths are validated by the IPC commands in
//! `ipc.rs` (string gate, device names, protected folders) and every later copy / move goes
//! through the normal operation queue with all of its guards. Nothing here opens or runs a file.
//!
//! Format reference: Microsoft, "Shell Clipboard Formats" (CF_HDROP, "Preferred DropEffect",
//! "Paste Succeeded") and the `DROPFILES` structure (shlobj_core.h).

use crate::domain::{ErrorCode, FsError};

/// `DROPFILES` header: pFiles(u32) pt.x(i32) pt.y(i32) fNC(u32) fWide(u32).
pub const DROPFILES_HEADER_LEN: usize = 20;
/// Most items accepted from / written to the clipboard.
pub const MAX_ITEMS: usize = 10_000;
/// Longest single path (UTF-16 units, without the terminator): the extended Windows limit.
pub const MAX_PATH_UNITS: usize = 32_767;
/// Total path text accepted (UTF-16 units, all items). Bounds memory and work.
pub const MAX_TOTAL_UNITS: usize = 2_000_000;
/// Largest clipboard block read at all (header + 2 bytes per unit + slack).
pub const MAX_BLOCK_BYTES: usize = DROPFILES_HEADER_LEN + MAX_TOTAL_UNITS * 2 + 4;

pub const DROPEFFECT_COPY: u32 = 1;
pub const DROPEFFECT_MOVE: u32 = 2;

/// Why a clipboard payload was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DropFilesError {
    Truncated,
    BadOffset,
    Ansi,
    Unaligned,
    NotTerminated,
    Empty,
    TooManyItems,
    PathTooLong,
    TooMuchText,
    BadText,
    NulInPath,
}

impl DropFilesError {
    pub fn message(&self) -> &'static str {
        match self {
            Self::Truncated => "The clipboard file list is truncated",
            Self::BadOffset => "The clipboard file list has an invalid header",
            Self::Ansi => "The clipboard holds an ANSI file list, which is not supported",
            Self::Unaligned => "The clipboard file list is not valid UTF-16",
            Self::NotTerminated => "The clipboard file list is not terminated",
            Self::Empty => "The clipboard file list is empty",
            Self::TooManyItems => "The clipboard holds too many files",
            Self::PathTooLong => "A path on the clipboard is too long",
            Self::TooMuchText => "The clipboard file list is too large",
            Self::BadText => "A path on the clipboard is not valid text",
            Self::NulInPath => "A path contains a NUL character",
        }
    }
}

impl From<DropFilesError> for FsError {
    fn from(e: DropFilesError) -> Self {
        FsError::new(ErrorCode::InvalidPath, e.message())
    }
}

/// Builds a `CF_HDROP` payload (`fWide = TRUE`, `pFiles = 20`, double-NUL terminated).
pub fn encode_dropfiles(paths: &[String]) -> Result<Vec<u8>, DropFilesError> {
    if paths.is_empty() {
        return Err(DropFilesError::Empty);
    }
    if paths.len() > MAX_ITEMS {
        return Err(DropFilesError::TooManyItems);
    }
    let mut units: Vec<u16> = Vec::new();
    for p in paths {
        if p.contains('\0') {
            return Err(DropFilesError::NulInPath);
        }
        let start = units.len();
        units.extend(p.encode_utf16());
        let len = units.len() - start;
        if len == 0 {
            return Err(DropFilesError::Empty);
        }
        if len > MAX_PATH_UNITS {
            return Err(DropFilesError::PathTooLong);
        }
        if units.len() > MAX_TOTAL_UNITS {
            return Err(DropFilesError::TooMuchText);
        }
        units.push(0);
    }
    units.push(0);
    let mut out = Vec::with_capacity(DROPFILES_HEADER_LEN + units.len() * 2);
    out.extend_from_slice(&(DROPFILES_HEADER_LEN as u32).to_le_bytes()); // pFiles
    out.extend_from_slice(&0i32.to_le_bytes()); // pt.x
    out.extend_from_slice(&0i32.to_le_bytes()); // pt.y
    out.extend_from_slice(&0u32.to_le_bytes()); // fNC
    out.extend_from_slice(&1u32.to_le_bytes()); // fWide
    for u in units {
        out.extend_from_slice(&u.to_le_bytes());
    }
    Ok(out)
}

fn read_u32(bytes: &[u8], at: usize) -> Option<u32> {
    let chunk = bytes.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes(chunk.try_into().ok()?))
}

/// Parses an untrusted `CF_HDROP` block. `bytes` is exactly the clipboard block (GlobalSize).
pub fn parse_dropfiles(bytes: &[u8]) -> Result<Vec<String>, DropFilesError> {
    if bytes.len() > MAX_BLOCK_BYTES {
        return Err(DropFilesError::TooMuchText);
    }
    if bytes.len() < DROPFILES_HEADER_LEN {
        return Err(DropFilesError::Truncated);
    }
    let p_files = read_u32(bytes, 0).ok_or(DropFilesError::Truncated)? as usize;
    let f_wide = read_u32(bytes, 16).ok_or(DropFilesError::Truncated)?;
    // The list starts after the header and inside the block.
    if p_files < DROPFILES_HEADER_LEN || p_files > bytes.len() {
        return Err(DropFilesError::BadOffset);
    }
    if f_wide == 0 {
        return Err(DropFilesError::Ansi);
    }
    let body = &bytes[p_files..];
    if !body.len().is_multiple_of(2) {
        return Err(DropFilesError::Unaligned);
    }
    let units: Vec<u16> = body
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .collect();
    let mut items: Vec<String> = Vec::new();
    let mut total = 0usize;
    let mut pos = 0usize;
    loop {
        // Running off the block before the closing empty string means a missing double NUL.
        let first = *units.get(pos).ok_or(DropFilesError::NotTerminated)?;
        if first == 0 {
            break; // the list terminator
        }
        let rel = units[pos..]
            .iter()
            .position(|&u| u == 0)
            .ok_or(DropFilesError::NotTerminated)?;
        if rel > MAX_PATH_UNITS {
            return Err(DropFilesError::PathTooLong);
        }
        total += rel;
        if total > MAX_TOTAL_UNITS {
            return Err(DropFilesError::TooMuchText);
        }
        if items.len() >= MAX_ITEMS {
            return Err(DropFilesError::TooManyItems);
        }
        let text =
            String::from_utf16(&units[pos..pos + rel]).map_err(|_| DropFilesError::BadText)?;
        items.push(text);
        pos += rel + 1;
    }
    if items.is_empty() {
        return Err(DropFilesError::Empty);
    }
    Ok(items)
}

/// Encodes the `Preferred DropEffect` DWORD.
pub fn encode_drop_effect(cut: bool) -> [u8; 4] {
    (if cut {
        DROPEFFECT_MOVE
    } else {
        DROPEFFECT_COPY
    })
    .to_le_bytes()
}

/// True when the (optional) `Preferred DropEffect` block asks for a MOVE. Absent, short or any
/// other value (copy, link, copy|link, none) means copy: the safe default that deletes nothing.
pub fn effect_is_move(bytes: Option<&[u8]>) -> bool {
    match bytes.and_then(|b| read_u32(b, 0)) {
        Some(v) => v & DROPEFFECT_MOVE != 0 && v & DROPEFFECT_COPY == 0,
        None => false,
    }
}

/// What a paste found on the clipboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardFiles {
    pub paths: Vec<String>,
    pub cut: bool,
    /// `GetClipboardSequenceNumber` at read time (used to clear exactly this content later).
    pub sequence: u32,
}

#[cfg(windows)]
mod win {
    use super::*;
    use std::time::Duration;
    use windows_sys::Win32::Foundation::{GlobalFree, HANDLE, HWND};
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardSequenceNumber,
        IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
    };
    use windows_sys::Win32::System::Memory::{
        GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
    };

    const CF_HDROP: u32 = 15;
    /// Another process may hold the clipboard for a moment: a few short tries, never a long wait.
    const OPEN_ATTEMPTS: u32 = 8;
    const OPEN_PAUSE: Duration = Duration::from_millis(15);

    fn err(code: ErrorCode, msg: &str) -> FsError {
        FsError::new(code, msg)
    }

    /// Open clipboard; closed on drop on every path.
    struct Open;
    impl Drop for Open {
        fn drop(&mut self) {
            // SAFETY: only constructed after a successful OpenClipboard on this thread.
            unsafe {
                CloseClipboard();
            }
        }
    }

    fn open(hwnd: isize) -> Result<Open, FsError> {
        for attempt in 0..OPEN_ATTEMPTS {
            // SAFETY: plain Win32 call; `hwnd` is a window handle (or 0) owned by this process.
            if unsafe { OpenClipboard(hwnd as HWND) } != 0 {
                return Ok(Open);
            }
            if attempt + 1 < OPEN_ATTEMPTS {
                std::thread::sleep(OPEN_PAUSE);
            }
        }
        Err(err(
            ErrorCode::Busy,
            "The clipboard is in use by another program. Try again.",
        ))
    }

    fn format_id(name: &str) -> u32 {
        let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: `wide` is NUL terminated and outlives the call.
        unsafe { RegisterClipboardFormatW(wide.as_ptr()) }
    }

    /// GlobalAlloc + copy. Ownership passes to the clipboard on a successful SetClipboardData.
    fn alloc_global(data: &[u8]) -> Result<HANDLE, FsError> {
        // SAFETY: GlobalAlloc has no preconditions; a null result is handled.
        let h = unsafe { GlobalAlloc(GMEM_MOVEABLE, data.len()) };
        if h.is_null() {
            return Err(err(ErrorCode::Io, "Out of memory for the clipboard"));
        }
        // SAFETY: `h` is a live moveable block of `data.len()` bytes; locked, written within its
        // size, then unlocked in the same scope. On failure the block is freed (not yet owned).
        unsafe {
            let p = GlobalLock(h);
            if p.is_null() {
                GlobalFree(h);
                return Err(err(ErrorCode::Io, "Could not lock clipboard memory"));
            }
            std::ptr::copy_nonoverlapping(data.as_ptr(), p as *mut u8, data.len());
            GlobalUnlock(h);
        }
        Ok(h)
    }

    fn set_format(format: u32, data: &[u8]) -> Result<(), FsError> {
        let h = alloc_global(data)?;
        // SAFETY: the clipboard is open (caller holds `Open`); on success the system owns `h`
        // and it must not be freed; on failure we still own it and free it exactly once.
        unsafe {
            if SetClipboardData(format, h).is_null() {
                GlobalFree(h);
                return Err(err(ErrorCode::Io, "Could not write to the clipboard"));
            }
        }
        Ok(())
    }

    /// Copies a clipboard format's block out (bounded by `max`). `None` when not present.
    fn get_format(format: u32, max: usize) -> Result<Option<Vec<u8>>, FsError> {
        // SAFETY: plain query; the clipboard is open (caller holds `Open`).
        if unsafe { IsClipboardFormatAvailable(format) } == 0 {
            return Ok(None);
        }
        // SAFETY: the handle belongs to the clipboard: never freed or kept past CloseClipboard.
        let h = unsafe { GetClipboardData(format) };
        if h.is_null() {
            return Ok(None);
        }
        // SAFETY: `h` is a valid HGLOBAL from the clipboard.
        let size = unsafe { GlobalSize(h) };
        if size == 0 || size > max {
            return Err(err(
                ErrorCode::InvalidPath,
                "The clipboard data is too large",
            ));
        }
        // SAFETY: lock/unlock are paired; reads stay within GlobalSize bytes of the locked block
        // and are copied out before unlocking.
        unsafe {
            let p = GlobalLock(h);
            if p.is_null() {
                return Err(err(ErrorCode::Io, "Could not read the clipboard"));
            }
            let out = std::slice::from_raw_parts(p as *const u8, size).to_vec();
            GlobalUnlock(h);
            Ok(Some(out))
        }
    }

    /// Replaces the clipboard with a file list (`CF_HDROP` + `Preferred DropEffect`).
    pub fn write_files(hwnd: isize, paths: &[String], cut: bool) -> Result<u32, FsError> {
        let drop = encode_dropfiles(paths)?;
        let effect = encode_drop_effect(cut);
        let effect_fmt = format_id("Preferred DropEffect");
        let _open = open(hwnd)?;
        // SAFETY: clipboard open; EmptyClipboard makes this process the owner of what follows.
        if unsafe { EmptyClipboard() } == 0 {
            return Err(err(ErrorCode::Io, "Could not clear the clipboard"));
        }
        set_format(CF_HDROP, &drop)?;
        if let Err(e) = set_format(effect_fmt, &effect) {
            // Never leave a half-written clipboard (a "cut" list that would paste as copy).
            // SAFETY: clipboard still open.
            unsafe {
                EmptyClipboard();
            }
            return Err(e);
        }
        // SAFETY: plain query.
        Ok(unsafe { GetClipboardSequenceNumber() })
    }

    /// Reads the file list and the drop effect. `Ok(None)` = the clipboard holds no files.
    pub fn read_files(hwnd: isize) -> Result<Option<ClipboardFiles>, FsError> {
        let effect_fmt = format_id("Preferred DropEffect");
        let _open = open(hwnd)?;
        let Some(block) = get_format(CF_HDROP, MAX_BLOCK_BYTES)? else {
            return Ok(None);
        };
        let effect = get_format(effect_fmt, 64)?;
        // SAFETY: plain query.
        let sequence = unsafe { GetClipboardSequenceNumber() };
        let paths = parse_dropfiles(&block)?;
        Ok(Some(ClipboardFiles {
            paths,
            cut: effect_is_move(effect.as_deref()),
            sequence,
        }))
    }

    /// Empties the clipboard only if it still holds the content with this sequence number
    /// (Explorer does the same after a cut + paste it performed itself).
    pub fn clear_if_unchanged(hwnd: isize, sequence: u32) -> Result<bool, FsError> {
        let _open = open(hwnd)?;
        // SAFETY: plain query.
        if unsafe { GetClipboardSequenceNumber() } != sequence {
            return Ok(false);
        }
        // SAFETY: clipboard open.
        Ok(unsafe { EmptyClipboard() } != 0)
    }
}

#[cfg(windows)]
pub use win::{clear_if_unchanged, read_files, write_files};

#[cfg(not(windows))]
mod other {
    use super::*;
    fn unsupported() -> FsError {
        FsError::new(ErrorCode::Unsupported, "File clipboard needs Windows")
    }
    pub fn write_files(_: isize, _: &[String], _: bool) -> Result<u32, FsError> {
        Err(unsupported())
    }
    pub fn read_files(_: isize) -> Result<Option<ClipboardFiles>, FsError> {
        Err(unsupported())
    }
    pub fn clear_if_unchanged(_: isize, _: u32) -> Result<bool, FsError> {
        Err(unsupported())
    }
}
#[cfg(not(windows))]
pub use other::{clear_if_unchanged, read_files, write_files};

#[cfg(test)]
mod tests;
