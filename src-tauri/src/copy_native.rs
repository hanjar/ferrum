//! FER-471 Windows primitives for the copy engine: `CopyFile2` with progress and
//! cancel, storage media detection (seek penalty IOCTL, no admin needed) and the
//! per-thread background mode (`THREAD_MODE_BACKGROUND_BEGIN`: CPU and I/O).
//!
//! Everything here is a thin, testable wrapper. Non-Windows builds get inert
//! fallbacks so the engine's portable stream copy stays the only path there.

use std::io;
use std::path::Path;

/// Storage class of a path, used to pick the starting worker count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Media {
    Ssd,
    Hdd,
    Network,
    Unknown,
}

impl Media {
    /// Worst-case of two endpoints: network > hdd > unknown > ssd.
    pub fn slower(self, other: Media) -> Media {
        fn rank(m: Media) -> u8 {
            match m {
                Media::Ssd => 0,
                Media::Unknown => 1,
                Media::Hdd => 2,
                Media::Network => 3,
            }
        }
        if rank(other) > rank(self) {
            other
        } else {
            self
        }
    }
}

/// Outcome of a native copy that did not simply succeed.
#[derive(Debug)]
pub enum NativeCopyError {
    /// The cancel/abort probe asked to stop; the destination is partial.
    Cancelled,
    /// The source is locked in a way CopyFile2 cannot read (sharing violation).
    /// The caller falls back to the portable stream copy, which shares all.
    SharingViolation,
    Io(io::Error),
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::GENERIC_WRITE;
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_LOCK_VIOLATION, ERROR_SHARING_VIOLATION, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Globalization::{
        LCMAP_UPPERCASE, LCMapStringEx, LOCALE_NAME_INVARIANT,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        COPY_FILE_NO_BUFFERING, COPYFILE2_CALLBACK_CHUNK_FINISHED, COPYFILE2_EXTENDED_PARAMETERS,
        COPYFILE2_MESSAGE, COPYFILE2_MESSAGE_ACTION, COPYFILE2_PROGRESS_CANCEL,
        COPYFILE2_PROGRESS_CONTINUE, CopyFile2, CreateFileW, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FlushFileBuffers, GetDriveTypeW,
        MoveFileExW, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::{
        DEVICE_SEEK_PENALTY_DESCRIPTOR, IOCTL_STORAGE_QUERY_PROPERTY, PropertyStandardQuery,
        STORAGE_PROPERTY_QUERY, StorageDeviceSeekPenaltyProperty,
    };
    use windows_sys::Win32::System::Threading::{
        GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN,
        THREAD_MODE_BACKGROUND_END,
    };
    use windows_sys::Win32::System::WindowsProgramming::{
        DRIVE_FIXED, DRIVE_REMOTE, DRIVE_REMOVABLE,
    };

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }

    struct Ctx<'a> {
        /// Return true to abort the copy.
        abort: &'a dyn Fn() -> bool,
        /// Called with the byte delta of each finished chunk.
        on_bytes: &'a dyn Fn(u64),
        last_total: std::cell::Cell<u64>,
    }

    unsafe extern "system" fn progress(
        message: *const COPYFILE2_MESSAGE,
        context: *const c_void,
    ) -> COPYFILE2_MESSAGE_ACTION {
        // SAFETY: CopyFile2 passes back the pointers handed in below; both live
        // for the whole synchronous call.
        let (message, ctx) = unsafe { (&*message, &*(context as *const Ctx)) };
        // A panic must never unwind across the FFI boundary (that aborts the process):
        // treat it as "cancel this copy"; the caller then removes the staged file.
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if message.Type == COPYFILE2_CALLBACK_CHUNK_FINISHED {
                // SAFETY: the union arm is selected by `Type`.
                let total = unsafe { message.Info.ChunkFinished.uliTotalBytesTransferred };
                let delta = total.saturating_sub(ctx.last_total.get());
                ctx.last_total.set(total);
                if delta > 0 {
                    (ctx.on_bytes)(delta);
                }
            }
            if (ctx.abort)() {
                COPYFILE2_PROGRESS_CANCEL
            } else {
                COPYFILE2_PROGRESS_CONTINUE
            }
        }))
        .unwrap_or(COPYFILE2_PROGRESS_CANCEL)
    }

    /// Upper-case with the OS's invariant-locale mapping (`LCMapStringEx`), the closest user-mode
    /// match to NTFS's case-insensitive name comparison: ss stays ss, no "SS" expansion.
    pub fn upper_invariant(text: &str) -> String {
        let src: Vec<u16> = text.encode_utf16().collect();
        if src.is_empty() {
            return String::new();
        }
        let map = |dest: *mut u16, len: i32| {
            // SAFETY: `src` is valid for `src.len()` units; `dest` is null (size query) or `len` units.
            unsafe {
                LCMapStringEx(
                    LOCALE_NAME_INVARIANT,
                    LCMAP_UPPERCASE,
                    src.as_ptr(),
                    src.len() as i32,
                    dest,
                    len,
                    std::ptr::null(),
                    std::ptr::null(),
                    0,
                )
            }
        };
        let needed = map(std::ptr::null_mut(), 0);
        if needed <= 0 {
            return text.to_uppercase();
        }
        let mut out = vec![0u16; needed as usize];
        let written = map(out.as_mut_ptr(), needed);
        if written <= 0 {
            return text.to_uppercase();
        }
        out.truncate(written as usize);
        String::from_utf16_lossy(&out)
    }

    /// Best-effort FlushFileBuffers on a directory, so a rename into it is recorded on disk.
    /// Errors are ignored (some file systems and shares do not support it).
    pub fn flush_dir(dir: &Path) {
        let wide_dir = wide(dir);
        // SAFETY: NUL-terminated path; BACKUP_SEMANTICS is required to open a directory.
        let handle = unsafe {
            CreateFileW(
                wide_dir.as_ptr(),
                GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return;
        }
        // SAFETY: valid handle from CreateFileW.
        unsafe {
            FlushFileBuffers(handle);
            CloseHandle(handle);
        }
    }

    /// Rename WITHOUT replacing: an existing destination is an error (AlreadyExists).
    pub fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
        let (a, b) = (wide(from), wide(to));
        // SAFETY: NUL-terminated buffers that outlive the call; flags 0 = no REPLACE_EXISTING.
        if unsafe { MoveFileExW(a.as_ptr(), b.as_ptr(), 0) } != 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    pub fn copy_file(
        src: &Path,
        dest: &Path,
        unbuffered: bool,
        abort: &dyn Fn() -> bool,
        on_bytes: &dyn Fn(u64),
    ) -> Result<(), NativeCopyError> {
        let ctx = Ctx {
            abort,
            on_bytes,
            last_total: std::cell::Cell::new(0),
        };
        let params = COPYFILE2_EXTENDED_PARAMETERS {
            dwSize: std::mem::size_of::<COPYFILE2_EXTENDED_PARAMETERS>() as u32,
            dwCopyFlags: if unbuffered {
                COPY_FILE_NO_BUFFERING
            } else {
                0
            },
            pfCancel: std::ptr::null_mut(),
            pProgressRoutine: Some(progress),
            pvCallbackContext: &ctx as *const Ctx as *mut c_void,
        };
        let (from, to) = (wide(src), wide(dest));
        // SAFETY: both buffers are NUL-terminated and outlive the call; `params`
        // and `ctx` are valid for its duration.
        let hr = unsafe { CopyFile2(from.as_ptr(), to.as_ptr(), &params) };
        if hr >= 0 {
            return Ok(());
        }
        // HRESULT_FROM_WIN32: 0x8007xxxx carries the Win32 code in the low word.
        let code = (hr as u32) & 0xFFFF;
        if (hr as u32) >> 16 == 0x8007 {
            if code == 1235 {
                // ERROR_REQUEST_ABORTED: we cancelled from the callback.
                return Err(NativeCopyError::Cancelled);
            }
            if code == ERROR_SHARING_VIOLATION || code == ERROR_LOCK_VIOLATION {
                return Err(NativeCopyError::SharingViolation);
            }
            return Err(NativeCopyError::Io(io::Error::from_raw_os_error(
                code as i32,
            )));
        }
        Err(NativeCopyError::Io(io::Error::other(format!(
            "CopyFile2 failed: 0x{:08X}",
            hr as u32
        ))))
    }

    /// Enter/leave background mode for the CURRENT thread. Failure is harmless
    /// (the thread simply keeps its normal priority).
    pub fn set_thread_background(on: bool) -> bool {
        let mode = if on {
            THREAD_MODE_BACKGROUND_BEGIN
        } else {
            THREAD_MODE_BACKGROUND_END
        };
        // SAFETY: pseudo-handle of the calling thread.
        unsafe { SetThreadPriority(GetCurrentThread(), mode) != 0 }
    }

    pub fn media_of(path: &Path) -> Media {
        let text = path.to_string_lossy();
        let bytes = text.as_bytes();
        // UNC share (\\server\share) is network; \\?\ and \\.\ prefixes are local device paths.
        if text.starts_with(r"\\") && !text.starts_with(r"\\?\") && !text.starts_with(r"\\.\") {
            return Media::Network;
        }
        let stripped = text
            .strip_prefix(r"\\?\")
            .or_else(|| text.strip_prefix(r"\\.\"))
            .unwrap_or(&text);
        if stripped.to_ascii_uppercase().starts_with("UNC\\") {
            return Media::Network;
        }
        let letter = match stripped.as_bytes() {
            [l, b':', ..] if l.is_ascii_alphabetic() => l.to_ascii_uppercase() as char,
            _ => {
                let _ = bytes;
                return Media::Unknown;
            }
        };
        let root: Vec<u16> = format!("{letter}:\\")
            .encode_utf16()
            .chain(Some(0))
            .collect();
        // SAFETY: NUL-terminated root path.
        match unsafe { GetDriveTypeW(root.as_ptr()) } {
            DRIVE_REMOTE => return Media::Network,
            DRIVE_REMOVABLE => return Media::Hdd, // USB sticks behave badly under parallel IO
            DRIVE_FIXED => {}
            _ => return Media::Unknown,
        }
        let device: Vec<u16> = format!("\\\\.\\{letter}:")
            .encode_utf16()
            .chain(Some(0))
            .collect();
        // Access 0: enough for a property query, no elevation needed.
        // SAFETY: NUL-terminated device path.
        let handle = unsafe {
            CreateFileW(
                device.as_ptr(),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Media::Unknown;
        }
        let query = STORAGE_PROPERTY_QUERY {
            PropertyId: StorageDeviceSeekPenaltyProperty,
            QueryType: PropertyStandardQuery,
            AdditionalParameters: [0],
        };
        let mut descriptor = DEVICE_SEEK_PENALTY_DESCRIPTOR::default();
        let mut returned = 0u32;
        // SAFETY: buffers are sized by the structs passed; handle is valid.
        let ok = unsafe {
            DeviceIoControl(
                handle,
                IOCTL_STORAGE_QUERY_PROPERTY,
                &query as *const _ as *const c_void,
                std::mem::size_of::<STORAGE_PROPERTY_QUERY>() as u32,
                &mut descriptor as *mut _ as *mut c_void,
                std::mem::size_of::<DEVICE_SEEK_PENALTY_DESCRIPTOR>() as u32,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        // SAFETY: handle came from CreateFileW above.
        unsafe { CloseHandle(handle) };
        if ok == 0 {
            Media::Unknown
        } else if descriptor.IncursSeekPenalty {
            Media::Hdd
        } else {
            Media::Ssd
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;

    pub fn copy_file(
        _src: &Path,
        _dest: &Path,
        _unbuffered: bool,
        _abort: &dyn Fn() -> bool,
        _on_bytes: &dyn Fn(u64),
    ) -> Result<(), NativeCopyError> {
        Err(NativeCopyError::Io(io::Error::from(
            io::ErrorKind::Unsupported,
        )))
    }

    pub fn set_thread_background(_on: bool) -> bool {
        false
    }

    pub fn upper_invariant(text: &str) -> String {
        text.to_uppercase()
    }

    pub fn flush_dir(_dir: &Path) {}

    pub fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
        if to.exists() {
            return Err(io::Error::from(io::ErrorKind::AlreadyExists));
        }
        std::fs::rename(from, to)
    }

    pub fn media_of(_path: &Path) -> Media {
        Media::Unknown
    }
}

pub use imp::{
    copy_file, flush_dir, media_of, rename_no_replace, set_thread_background, upper_invariant,
};

/// True where `copy_file` is a real implementation.
pub const NATIVE_COPY: bool = cfg!(windows);

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn copy_file2_copies_and_reports_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.bin");
        let dst = dir.path().join("b.bin");
        let data = vec![9u8; 3 * 1024 * 1024 + 17];
        std::fs::write(&src, &data).unwrap();
        let seen = AtomicU64::new(0);
        copy_file(&src, &dst, false, &|| false, &|n| {
            seen.fetch_add(n, Ordering::Relaxed);
        })
        .unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), data);
        assert_eq!(seen.load(Ordering::Relaxed), data.len() as u64);
    }

    #[test]
    fn copy_file2_aborts_from_callback() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.bin");
        let dst = dir.path().join("b.bin");
        std::fs::write(&src, vec![1u8; 8 * 1024 * 1024]).unwrap();
        let r = copy_file(&src, &dst, false, &|| true, &|_| {});
        assert!(matches!(r, Err(NativeCopyError::Cancelled)), "{r:?}");
    }

    #[test]
    fn a_panicking_progress_closure_cancels_instead_of_aborting_the_process() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.bin");
        let dst = dir.path().join("b.bin");
        std::fs::write(&src, vec![1u8; 2 * 1024 * 1024]).unwrap();
        let r = copy_file(&src, &dst, false, &|| false, &|_| panic!("boom"));
        assert!(matches!(r, Err(NativeCopyError::Cancelled)), "{r:?}");
    }

    #[test]
    fn rename_no_replace_refuses_an_existing_destination() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b, c) = (
            dir.path().join("a"),
            dir.path().join("b"),
            dir.path().join("c"),
        );
        std::fs::write(&a, b"new").unwrap();
        std::fs::write(&b, b"old").unwrap();
        let e = rename_no_replace(&a, &b).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&b).unwrap(), b"old");
        assert!(a.exists(), "source of the rename untouched");
        rename_no_replace(&a, &c).unwrap();
        assert_eq!(std::fs::read(&c).unwrap(), b"new");
    }

    #[test]
    fn upper_invariant_follows_the_os_mapping_not_rust_unicode_expansion() {
        assert_eq!(upper_invariant("Report.TXT"), "REPORT.TXT");
        assert_eq!(upper_invariant("abc"), upper_invariant("ABC"));
        assert_eq!(upper_invariant(""), "");
        // Rust expands to "SS"; the OS keeps one code unit, so "ß" and "SS" are different names.
        assert_eq!("ß".to_uppercase(), "SS");
        assert_ne!(upper_invariant("ß"), "SS");
        assert_eq!(upper_invariant("ß").chars().count(), 1);
        // Same length in, same length out for plain letters (no accidental expansion).
        assert_eq!(
            upper_invariant("straße.txt").chars().count(),
            "straße.txt".chars().count()
        );
        // Dotless i maps like the OS says; never panics and stays one character.
        assert_eq!(upper_invariant("ı").chars().count(), 1);
    }

    #[test]
    fn flush_dir_is_harmless_on_real_and_missing_directories() {
        let dir = tempfile::tempdir().unwrap();
        flush_dir(dir.path());
        flush_dir(&dir.path().join("missing"));
    }

    #[test]
    fn media_of_classifies_unc_and_system_drive() {
        assert_eq!(media_of(Path::new(r"\\server\share\x")), Media::Network);
        let m = media_of(&std::env::temp_dir());
        assert!(
            matches!(m, Media::Ssd | Media::Hdd | Media::Unknown),
            "{m:?}"
        );
        assert_eq!(media_of(Path::new("relative")), Media::Unknown);
    }

    #[test]
    fn background_mode_toggles() {
        std::thread::spawn(|| {
            assert!(set_thread_background(true));
            assert!(set_thread_background(false));
        })
        .join()
        .unwrap();
    }
}
