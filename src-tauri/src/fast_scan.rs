//! FER-460: launch `ferrum-scan-helper.exe` elevated (UAC) and receive its
//! MFT entry stream over a private named pipe. Ferrum itself stays a normal,
//! non-elevated process.
//!
//! Security design (reviewed against FER-460; details in
//! docs/disk-usage-contract.md §7):
//! - The helper path is resolved from Ferrum's OWN exe directory (the
//!   protected install dir), never from PATH or the working directory.
//! - argv carries only `--volume X --pipe ferrum-scan-<128-bit hex>
//!   --nonce <256-bit hex>`, validated again by the helper.
//! - Ferrum creates the pipe FIRST (`FILE_FLAG_FIRST_PIPE_INSTANCE`, one
//!   instance, `PIPE_REJECT_REMOTE_CLIENTS`) with a protected DACL granting
//!   access only to the current user's SID.
//! - Same account only (review W1): the fast scan is offered only to an
//!   account that can elevate ITSELF (split admin token, or already
//!   elevated); the helper refuses a server of another user.
//! - Client authentication BEFORE anything is sent (review W3): after the
//!   helper's data-free hello, Ferrum impersonates the client at
//!   identification level and requires an ELEVATED token of its own user
//!   SID — a medium-IL process cannot fake that — plus the client PID of the
//!   process ShellExecuteEx started. Only then does Ferrum send the nonce.
//!   The helper checks the nonce before sending data and connects with
//!   SECURITY_IDENTIFICATION (its elevated token is never usable by us).
//! - Everything read is decoded by `ferrum_ntfs_scan::protocol` (bounded,
//!   strict) and additionally capped here (entries vs. the volume's record
//!   count, total name bytes, a global deadline).
//! - Cancel or any error closes the pipe; the helper exits on its next
//!   write. Connect, inactivity and global timeouts bound every wait.

use ferrum_ntfs_scan::protocol::{DoneInfo, WireEntry};
use std::path::PathBuf;

/// Helper binary name, next to ferrum.exe.
pub const HELPER_EXE: &str = "ferrum-scan-helper.exe";

/// Whether a root may be offered the fast scan.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FastScanEligibility {
    pub eligible: bool,
    /// Drive letter when the root is on a local NTFS volume.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub volume: Option<String>,
    /// Why not (shown verbatim in the UI when relevant).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl FastScanEligibility {
    fn no(reason: impl Into<String>) -> Self {
        Self {
            eligible: false,
            volume: None,
            reason: Some(reason.into()),
        }
    }

    /// Not eligible, with a reason for the UI (FER-476 index offer).
    pub fn not(reason: impl Into<String>) -> Self {
        Self::no(reason)
    }

    /// Eligible on drive `volume`.
    pub fn yes(volume: &str) -> Self {
        Self {
            eligible: true,
            volume: Some(volume.to_string()),
            reason: None,
        }
    }
}

/// Progress reported while the helper streams.
#[derive(Debug, Clone, Copy, Default)]
pub struct HelperProgress {
    pub records_done: u64,
    pub records_total: u64,
    pub entries: u64,
}

pub struct HelperResult {
    pub entries: Vec<WireEntry>,
    pub done: DoneInfo,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FastScanError {
    /// The user said No on the UAC prompt.
    Declined,
    Cancelled,
    /// Anything else; the message is user-facing (Indonesian).
    Failed(String),
}

impl std::fmt::Display for FastScanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Declined => write!(f, "izin admin ditolak"),
            Self::Cancelled => write!(f, "dibatalkan"),
            Self::Failed(m) => write!(f, "{m}"),
        }
    }
}

/// Absolute path of the helper next to the running exe, if it may be used.
pub fn helper_path() -> Option<PathBuf> {
    helper_location().ok()
}

/// The helper next to the running exe — and, in release builds, ONLY when
/// that directory is inside Program Files. A per-user install (e.g. NSIS
/// `currentUser` under %LOCALAPPDATA%) is writable by any process of the
/// user, so non-admin malware could swap the binary and ride the user's
/// next UAC "Yes"; such installs never offer the fast scan. A debug build
/// (developer `target\debug`, never shipped) may skip the location rule, but
/// only when the developer opts in with `FERRUM_DEV_HELPER=1` (FER-463 L3):
/// otherwise malware of the developer could swap the user-writable debug
/// helper and ride an unrelated UAC "Yes".
pub fn helper_location() -> Result<PathBuf, &'static str> {
    let exe = std::env::current_exe().map_err(|_| "Lokasi Ferrum tidak diketahui")?;
    let dir = exe.parent().ok_or("Lokasi Ferrum tidak diketahui")?;
    let p = dir.join(HELPER_EXE);
    if !p.is_file() {
        return Err("Helper scan cepat tidak terpasang");
    }
    let bypass = location_rule_bypassed(
        cfg!(debug_assertions),
        std::env::var(DEV_HELPER_ENV).ok().as_deref(),
    );
    if !bypass && !in_protected_dir(dir, &protected_roots()) {
        return Err("Scan cepat hanya untuk instalasi di Program Files");
    }
    Ok(p)
}

/// FER-463 L3: developer opt-in for a debug-build helper outside Program Files.
pub const DEV_HELPER_ENV: &str = "FERRUM_DEV_HELPER";

/// The Program Files rule is skipped only in a debug build AND with the
/// explicit opt-in `FERRUM_DEV_HELPER=1`. A release build never skips it.
pub fn location_rule_bypassed(debug_build: bool, dev_helper: Option<&str>) -> bool {
    debug_build && dev_helper == Some("1")
}

/// True when `dir` is `root` or below it, for any root (component-wise,
/// case-insensitive; `C:\Program Files Evil` does not match `C:\Program Files`).
pub fn in_protected_dir(dir: &std::path::Path, roots: &[PathBuf]) -> bool {
    let norm = |p: &std::path::Path| -> Vec<String> {
        p.components()
            .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
            .filter(|c| c != "\\")
            .collect()
    };
    let d = norm(dir);
    roots.iter().any(|r| {
        let r = norm(r);
        !r.is_empty() && d.len() >= r.len() && d[..r.len()] == r[..]
    })
}

/// Program Files folders from the Known-Folder API (not environment
/// variables, which the launching process controls).
#[cfg(windows)]
fn protected_roots() -> Vec<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::{
        FOLDERID_ProgramFiles, FOLDERID_ProgramFilesX86, SHGetKnownFolderPath,
    };
    let mut out = Vec::new();
    for id in [FOLDERID_ProgramFiles, FOLDERID_ProgramFilesX86] {
        let mut p: *mut u16 = std::ptr::null_mut();
        let hr = unsafe { SHGetKnownFolderPath(&id, 0, std::ptr::null_mut(), &mut p) };
        if hr >= 0 && !p.is_null() {
            let mut n = 0usize;
            while unsafe { *p.add(n) } != 0 {
                n += 1;
            }
            let s = std::ffi::OsString::from_wide(unsafe { std::slice::from_raw_parts(p, n) });
            out.push(PathBuf::from(s));
        }
        if !p.is_null() {
            unsafe { CoTaskMemFree(p.cast()) };
        }
    }
    out
}

#[cfg(not(windows))]
fn protected_roots() -> Vec<PathBuf> {
    Vec::new()
}

/// Drive letter of a plain `X:\…` root. UNC, `\\?\`, and relative paths are
/// refused (they are never local-NTFS candidates for the MFT helper).
pub fn drive_letter_of(root: &str) -> Option<char> {
    let b = root.as_bytes();
    if b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/')
    {
        Some(b[0].to_ascii_uppercase() as char)
    } else {
        None
    }
}

#[cfg(not(windows))]
pub fn eligibility(_root: &str) -> FastScanEligibility {
    FastScanEligibility::no("Scan cepat hanya tersedia di Windows")
}

#[cfg(not(windows))]
pub fn run_helper(
    _volume: char,
    _hwnd: isize,
    _cancel: &std::sync::atomic::AtomicBool,
    _on_progress: impl FnMut(HelperProgress),
) -> Result<HelperResult, FastScanError> {
    Err(FastScanError::Failed(
        "Scan cepat hanya tersedia di Windows".into(),
    ))
}

#[cfg(windows)]
pub use win::{eligibility, run_helper};

#[cfg(windows)]
mod win {
    use super::*;
    use ferrum_ntfs_scan::args::{HelperArgs, PIPE_PREFIX, hex_lower};
    use ferrum_ntfs_scan::protocol::{self, Message, NONCE_LEN, ProtocolError};
    use std::io::{self, Read};
    use std::os::windows::ffi::OsStrExt;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_BROKEN_PIPE, ERROR_CANCELLED, ERROR_IO_PENDING, ERROR_PIPE_CONNECTED,
        HANDLE, INVALID_HANDLE_VALUE, LocalFree, WAIT_OBJECT_0,
    };
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{
        GetTokenInformation, PSECURITY_DESCRIPTOR, RevertToSelf, SECURITY_ATTRIBUTES, TOKEN_QUERY,
        TOKEN_USER, TokenUser,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, GetDriveTypeW, GetVolumeInformationW,
        PIPE_ACCESS_DUPLEX, ReadFile, WriteFile,
    };
    use windows_sys::Win32::System::Com::{
        COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
    };
    use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId,
        ImpersonateNamedPipeClient, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
        PIPE_WAIT,
    };
    use windows_sys::Win32::System::Threading::{
        CreateEventW, GetCurrentProcess, GetCurrentThread, GetExitCodeProcess, GetProcessId,
        OpenProcessToken, OpenThreadToken, WaitForSingleObject,
    };
    use windows_sys::Win32::UI::Shell::{
        SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
        ShellExecuteExW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;

    /// After UAC, the helper must connect within this window.
    const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
    /// No frame for this long = the helper is stuck; give up.
    const INACTIVITY_TIMEOUT: Duration = Duration::from_secs(60);
    const POLL: u32 = 100;
    /// Whole fast scan, UAC prompt excluded (review L5).
    const GLOBAL_DEADLINE: Duration = Duration::from_secs(15 * 60);
    /// DoS guards on what one scan may deliver (a 4.5M-file C: is ~5M
    /// entries and ~150 MB of names). Entries are also capped at
    /// [`ENTRIES_PER_RECORD`] × the volume's record count.
    pub const MAX_TOTAL_ENTRIES: u64 = 40_000_000;
    pub const MAX_TOTAL_NAME_BYTES: u64 = 1024 * 1024 * 1024;
    /// A record yields one entry per non-DOS name; real hardlink counts are
    /// tiny, so 4 per record is a generous ceiling.
    pub const ENTRIES_PER_RECORD: u64 = 4;

    /// Entry ceiling for a volume with `total_records` MFT records.
    pub fn entry_cap(total_records: u64) -> u64 {
        MAX_TOTAL_ENTRIES.min(total_records.saturating_mul(ENTRIES_PER_RECORD))
    }

    /// Helper progress is untrusted (design decision, percent exception §3.5):
    /// accept only `0 < total`, `done <= total`, a total that never changes
    /// once known, and a `done` that never goes backwards.
    pub fn accept_progress(prev: &HelperProgress, done: u64, total: u64) -> bool {
        if total == 0 || done > total {
            return false;
        }
        if prev.records_total != 0 && total != prev.records_total {
            return false;
        }
        done >= prev.records_done
    }
    const DRIVE_REMOVABLE: u32 = 2;
    const DRIVE_FIXED: u32 = 3;

    fn wide(s: &str) -> Vec<u16> {
        std::ffi::OsStr::new(s)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    struct Handle(HANDLE);
    impl Drop for Handle {
        fn drop(&mut self) {
            if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
                unsafe { CloseHandle(self.0) };
            }
        }
    }

    fn fail(msg: impl Into<String>) -> FastScanError {
        FastScanError::Failed(msg.into())
    }

    pub fn eligibility(root: &str) -> FastScanEligibility {
        let Some(letter) = drive_letter_of(root) else {
            return FastScanEligibility::no("Scan cepat hanya untuk drive lokal (X:\\)");
        };
        let drive = wide(&format!("{letter}:\\"));
        let ty = unsafe { GetDriveTypeW(drive.as_ptr()) };
        if ty != DRIVE_FIXED && ty != DRIVE_REMOVABLE {
            return FastScanEligibility::no("Drive ini bukan disk lokal");
        }
        let mut fs = [0u16; 64];
        let ok = unsafe {
            GetVolumeInformationW(
                drive.as_ptr(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                fs.as_mut_ptr(),
                fs.len() as u32,
            )
        };
        if ok == 0 {
            return FastScanEligibility::no("Informasi volume tidak tersedia");
        }
        let end = fs.iter().position(|&c| c == 0).unwrap_or(fs.len());
        let name = String::from_utf16_lossy(&fs[..end]);
        if name != "NTFS" {
            return FastScanEligibility::no(format!("Volume {name}, bukan NTFS"));
        }
        if let Err(why) = helper_location() {
            return FastScanEligibility::no(why);
        }
        if !ferrum_ntfs_scan::token::may_elevate_same_account() {
            // Standard user: UAC would need another account's credentials,
            // and that account's volume-wide listing must not reach us (W1).
            return FastScanEligibility::no(
                "Scan cepat butuh akun administrator ini sendiri (UAC dengan akun lain tidak didukung)",
            );
        }
        FastScanEligibility {
            eligible: true,
            volume: Some(letter.to_string()),
            reason: None,
        }
    }

    /// String SID of the current process user (`S-1-5-21-…`).
    fn current_user_sid() -> io::Result<String> {
        let mut token: HANDLE = std::ptr::null_mut();
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let token = Handle(token);
        let mut len = 0u32;
        unsafe { GetTokenInformation(token.0, TokenUser, std::ptr::null_mut(), 0, &mut len) };
        if len == 0 || len > 64 * 1024 {
            return Err(io::Error::last_os_error());
        }
        // u64 backing keeps TOKEN_USER suitably aligned.
        let mut buf = vec![0u64; (len as usize).div_ceil(8)];
        if unsafe {
            GetTokenInformation(token.0, TokenUser, buf.as_mut_ptr().cast(), len, &mut len)
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let user = unsafe { &*(buf.as_ptr() as *const TOKEN_USER) };
        let mut s: *mut u16 = std::ptr::null_mut();
        if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut s) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut n = 0usize;
        while unsafe { *s.add(n) } != 0 {
            n += 1;
        }
        let out = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(s, n) });
        unsafe { LocalFree(s.cast()) };
        // Defensive: the SID is spliced into SDDL below.
        if !out.starts_with("S-1-")
            || !out
                .bytes()
                .all(|b| b.is_ascii_digit() || b == b'-' || b == b'S')
        {
            return Err(io::Error::other("unexpected SID format"));
        }
        Ok(out)
    }

    /// The pipe DACL: protected (no inheritance), GENERIC_ALL for the current
    /// user only. Pure for testability.
    pub fn pipe_sddl(user_sid: &str) -> String {
        format!("D:P(A;;GA;;;{user_sid})")
    }

    struct SecurityDescriptor(PSECURITY_DESCRIPTOR);
    impl Drop for SecurityDescriptor {
        fn drop(&mut self) {
            unsafe { LocalFree(self.0) };
        }
    }

    fn random_bytes<const N: usize>() -> Result<[u8; N], FastScanError> {
        let mut b = [0u8; N];
        getrandom::fill(&mut b).map_err(|_| fail("Sumber acak OS tidak tersedia"))?;
        Ok(b)
    }

    /// Overlapped pipe I/O with cancel + timeout polling.
    struct Pipe<'a> {
        handle: Handle,
        event: Handle,
        cancel: &'a AtomicBool,
        helper: HANDLE,
        buf: Vec<u8>,
        pos: usize,
        len: usize,
    }

    enum Wait {
        Done(u32),
        Eof,
    }

    impl Pipe<'_> {
        fn overlapped(&self) -> OVERLAPPED {
            let mut ov: OVERLAPPED = unsafe { std::mem::zeroed() };
            ov.hEvent = self.event.0;
            ov
        }

        /// Wait for a pending overlapped op; aborts it on cancel/timeout.
        fn wait(&self, ov: &mut OVERLAPPED, timeout: Duration) -> io::Result<Wait> {
            let start = Instant::now();
            loop {
                if unsafe { WaitForSingleObject(self.event.0, POLL) } == WAIT_OBJECT_0 {
                    let mut n = 0u32;
                    if unsafe { GetOverlappedResult(self.handle.0, ov, &mut n, 0) } == 0 {
                        let err = io::Error::last_os_error();
                        if err.raw_os_error() == Some(ERROR_BROKEN_PIPE as i32) {
                            return Ok(Wait::Eof);
                        }
                        return Err(err);
                    }
                    return Ok(Wait::Done(n));
                }
                let reason = if self.cancel.load(Ordering::Relaxed) {
                    // Not `Interrupted`: readers retry that kind forever.
                    Some(io::Error::new(
                        io::ErrorKind::ConnectionAborted,
                        "cancelled",
                    ))
                } else if start.elapsed() >= timeout {
                    Some(io::Error::new(io::ErrorKind::TimedOut, "helper timeout"))
                } else {
                    None
                };
                if let Some(e) = reason {
                    unsafe { CancelIoEx(self.handle.0, ov) };
                    let mut n = 0u32;
                    // Wait for the cancellation to land before `ov` dies.
                    unsafe { GetOverlappedResult(self.handle.0, ov, &mut n, 1) };
                    return Err(e);
                }
            }
        }

        fn connect(&self) -> io::Result<()> {
            let mut ov = self.overlapped();
            if unsafe { ConnectNamedPipe(self.handle.0, &mut ov) } != 0 {
                return Ok(());
            }
            let err = io::Error::last_os_error();
            match err.raw_os_error().map(|c| c as u32) {
                Some(ERROR_PIPE_CONNECTED) => Ok(()),
                Some(ERROR_IO_PENDING) => {
                    // Also stop early when the helper died before connecting.
                    let start = Instant::now();
                    loop {
                        match self.wait(&mut ov, Duration::from_millis(500)) {
                            Ok(_) => return Ok(()),
                            Err(e) if e.kind() == io::ErrorKind::TimedOut => {
                                if helper_exited(self.helper).is_some()
                                    || start.elapsed() >= CONNECT_TIMEOUT
                                {
                                    return Err(io::Error::new(
                                        io::ErrorKind::TimedOut,
                                        "helper did not connect",
                                    ));
                                }
                                // `wait` cancelled the connect; re-arm it.
                                ov = self.overlapped();
                                if unsafe { ConnectNamedPipe(self.handle.0, &mut ov) } != 0 {
                                    return Ok(());
                                }
                                let e2 = io::Error::last_os_error();
                                match e2.raw_os_error().map(|c| c as u32) {
                                    Some(ERROR_PIPE_CONNECTED) => return Ok(()),
                                    Some(ERROR_IO_PENDING) => continue,
                                    _ => return Err(e2),
                                }
                            }
                            Err(e) => return Err(e),
                        }
                    }
                }
                _ => Err(err),
            }
        }

        fn write_all(&self, data: &[u8]) -> io::Result<()> {
            let mut done = 0usize;
            while done < data.len() {
                let mut ov = self.overlapped();
                let mut n = 0u32;
                let chunk = (data.len() - done).min(64 * 1024) as u32;
                let ok = unsafe {
                    WriteFile(self.handle.0, data[done..].as_ptr(), chunk, &mut n, &mut ov)
                };
                if ok == 0 {
                    let err = io::Error::last_os_error();
                    if err.raw_os_error() != Some(ERROR_IO_PENDING as i32) {
                        return Err(err);
                    }
                }
                match self.wait(&mut ov, INACTIVITY_TIMEOUT)? {
                    Wait::Done(n) => done += n as usize,
                    Wait::Eof => return Err(io::ErrorKind::BrokenPipe.into()),
                }
            }
            Ok(())
        }
    }

    impl Read for Pipe<'_> {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            if self.pos == self.len {
                let mut ov = self.overlapped();
                let mut n = 0u32;
                let cap = self.buf.len() as u32;
                let ok =
                    unsafe { ReadFile(self.handle.0, self.buf.as_mut_ptr(), cap, &mut n, &mut ov) };
                if ok == 0 {
                    let err = io::Error::last_os_error();
                    match err.raw_os_error().map(|c| c as u32) {
                        Some(ERROR_IO_PENDING) => {}
                        Some(ERROR_BROKEN_PIPE) => return Ok(0),
                        _ => return Err(err),
                    }
                }
                match self.wait(&mut ov, INACTIVITY_TIMEOUT)? {
                    Wait::Done(n) => {
                        self.pos = 0;
                        self.len = n as usize;
                    }
                    Wait::Eof => return Ok(0),
                }
                if self.len == 0 {
                    return Ok(0);
                }
            }
            let n = out.len().min(self.len - self.pos);
            out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
            self.pos += n;
            Ok(n)
        }
    }

    fn helper_exited(process: HANDLE) -> Option<u32> {
        if unsafe { WaitForSingleObject(process, 0) } == WAIT_OBJECT_0 {
            let mut code = 0u32;
            unsafe { GetExitCodeProcess(process, &mut code) };
            Some(code)
        } else {
            None
        }
    }

    struct ComGuard(bool);
    impl Drop for ComGuard {
        fn drop(&mut self) {
            if self.0 {
                unsafe { CoUninitialize() };
            }
        }
    }

    /// Elevate the helper (UAC) and collect its whole entry stream.
    /// `hwnd` = owner window for the UAC prompt (0 = none).
    pub fn run_helper(
        volume: char,
        hwnd: isize,
        cancel: &AtomicBool,
        mut on_progress: impl FnMut(HelperProgress),
    ) -> Result<HelperResult, FastScanError> {
        let exe = helper_location().map_err(fail)?;
        let nonce: [u8; NONCE_LEN] = random_bytes()?;
        let pipe_id: [u8; 16] = random_bytes()?;
        let args = HelperArgs {
            volume,
            pipe_name: format!("{PIPE_PREFIX}{}", hex_lower(&pipe_id)),
            nonce,
        };

        // ── pipe with a current-user DACL, created before the helper runs ──
        let sid = current_user_sid().map_err(|e| fail(format!("SID user: {e}")))?;
        let sddl = wide(&pipe_sddl(&sid));
        let mut psd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut psd,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(fail(format!(
                "Security descriptor pipe: {}",
                io::Error::last_os_error()
            )));
        }
        let sd = SecurityDescriptor(psd);
        let sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd.0,
            bInheritHandle: 0,
        };
        let path = wide(&args.pipe_path());
        let raw = unsafe {
            CreateNamedPipeW(
                path.as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE | FILE_FLAG_OVERLAPPED,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                64 * 1024,
                1024 * 1024,
                0,
                &sa,
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            return Err(fail(format!("Pipe: {}", io::Error::last_os_error())));
        }
        let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        if event.is_null() {
            return Err(fail(format!("Event: {}", io::Error::last_os_error())));
        }

        // ── UAC ──────────────────────────────────────────────────────────
        let com = ComGuard(
            unsafe {
                CoInitializeEx(
                    std::ptr::null(),
                    (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
                )
            } >= 0,
        );
        let verb = wide("runas");
        let file = wide(&exe.to_string_lossy());
        let params = wide(&args.to_command_line());
        let dir = wide(
            &exe.parent()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
        );
        let mut sei: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
        sei.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
        sei.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI;
        sei.hwnd = hwnd as _;
        sei.lpVerb = verb.as_ptr();
        sei.lpFile = file.as_ptr();
        sei.lpParameters = params.as_ptr();
        sei.lpDirectory = dir.as_ptr();
        sei.nShow = SW_HIDE;
        if unsafe { ShellExecuteExW(&mut sei) } == 0 {
            let err = io::Error::last_os_error();
            return Err(if err.raw_os_error() == Some(ERROR_CANCELLED as i32) {
                FastScanError::Declined
            } else {
                fail(format!("Gagal menjalankan helper: {err}"))
            });
        }
        drop(com);
        if sei.hProcess.is_null() {
            return Err(fail("Helper berjalan tanpa handle proses"));
        }
        let process = Handle(sei.hProcess);
        let mut pipe = Pipe {
            handle: Handle(raw),
            event: Handle(event),
            cancel,
            helper: process.0,
            buf: vec![0u8; 256 * 1024],
            pos: 0,
            len: 0,
        };
        let map_io = |e: io::Error| {
            if e.kind() == io::ErrorKind::ConnectionAborted {
                FastScanError::Cancelled
            } else {
                fail(format!("Helper: {e}"))
            }
        };
        pipe.connect().map_err(map_io)?;
        let deadline = Instant::now() + GLOBAL_DEADLINE;
        let map_proto = |e: ProtocolError| match e {
            ProtocolError::Io(ref m) if m.contains("cancelled") => FastScanError::Cancelled,
            ProtocolError::Eof => fail("Helper berhenti sebelum selesai"),
            other => fail(format!("Data helper ditolak: {other}")),
        };

        // ── authenticate the client BEFORE sending anything (W3) ──────────
        // One read is needed before a pipe client can be impersonated; the
        // hello carries no data, so nothing untrusted is used before this.
        match protocol::read_message(&mut pipe).map_err(map_proto)? {
            Message::ClientHello => {}
            _ => return Err(fail("Data helper ditolak: hello tidak valid")),
        }
        let mut client_pid = 0u32;
        let expected = unsafe { GetProcessId(process.0) };
        if unsafe { GetNamedPipeClientProcessId(pipe.handle.0, &mut client_pid) } == 0
            || expected == 0
            || client_pid != expected
        {
            return Err(fail("Klien pipe bukan helper yang dijalankan Ferrum"));
        }
        authenticate_client(pipe.handle.0)?;
        pipe.write_all(&protocol::encode(&Message::ServerHello { nonce }))
            .map_err(map_io)?;

        // ── receive ───────────────────────────────────────────────────────
        let mut progress = HelperProgress::default();
        let cap = match protocol::read_message(&mut pipe).map_err(map_proto)? {
            Message::Volume(v) => entry_cap(v.total_records),
            Message::Error { message, .. } => return Err(fail(message)),
            _ => return Err(fail("Data helper ditolak: info volume tidak valid")),
        };
        let mut entries: Vec<WireEntry> = Vec::new();
        let mut name_bytes = 0u64;
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(FastScanError::Cancelled);
            }
            if Instant::now() >= deadline {
                return Err(fail("Scan cepat melebihi batas waktu"));
            }
            match protocol::read_message(&mut pipe).map_err(map_proto)? {
                Message::Progress { done, total } => {
                    // Untrusted: only a consistent, monotonic step reaches
                    // the UI; anything else is ignored (never a percent jump).
                    if accept_progress(&progress, done, total) {
                        progress.records_done = done;
                        progress.records_total = total;
                        on_progress(progress);
                    }
                }
                Message::Batch(batch) => {
                    let n = batch.len() as u64;
                    name_bytes += batch.iter().map(|e| e.name.len() as u64).sum::<u64>();
                    if entries.len() as u64 + n > cap {
                        return Err(fail(format!(
                            "Data helper ditolak: lebih dari {cap} entri untuk volume ini"
                        )));
                    }
                    if name_bytes > MAX_TOTAL_NAME_BYTES {
                        return Err(fail("Data helper ditolak: total nama melebihi 1 GiB"));
                    }
                    entries.extend(batch);
                    progress.entries = entries.len() as u64;
                }
                Message::Done(done) => {
                    return Ok(HelperResult { entries, done });
                }
                Message::Error { message, .. } => return Err(fail(message)),
                Message::ServerHello { .. } | Message::ClientHello | Message::Volume(_) => {
                    return Err(fail("Data helper ditolak: urutan pesan salah"));
                }
            }
        }
    }

    /// Identify the pipe client by its TOKEN (review W3): impersonate at the
    /// identification level the helper allows, read the thread token, revert
    /// immediately, then require an elevated token of this process's user.
    /// A medium-IL process of the same user cannot present such a token.
    fn authenticate_client(pipe: HANDLE) -> Result<(), FastScanError> {
        use ferrum_ntfs_scan::token;
        let own = token::current_process_token()
            .and_then(|t| token::user_sid(t.0))
            .map_err(|e| fail(format!("Token Ferrum: {e}")))?;
        if unsafe { ImpersonateNamedPipeClient(pipe) } == 0 {
            return Err(fail(format!(
                "Identitas klien pipe: {}",
                io::Error::last_os_error()
            )));
        }
        let mut tok: HANDLE = std::ptr::null_mut();
        let opened = unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut tok) };
        let open_err = io::Error::last_os_error();
        if unsafe { RevertToSelf() } == 0 {
            // Never keep running on a pool thread that carries another
            // security context.
            std::process::abort();
        }
        if opened == 0 {
            return Err(fail(format!("Token klien pipe: {open_err}")));
        }
        let tok = token::Handle(tok);
        let elevated = token::is_elevated(tok.0).map_err(|e| fail(format!("Token klien: {e}")))?;
        let sid = token::user_sid(tok.0).map_err(|e| fail(format!("Token klien: {e}")))?;
        client_token_ok(elevated, &sid, &own)
    }

    /// Pure decision behind [`authenticate_client`] (testable).
    pub fn client_token_ok(elevated: bool, sid: &[u8], own: &[u8]) -> Result<(), FastScanError> {
        if sid != own {
            return Err(fail("Klien pipe berjalan sebagai akun lain"));
        }
        if !elevated {
            return Err(fail("Klien pipe bukan proses elevated"));
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn sddl_grants_only_the_current_user() {
            let s = pipe_sddl("S-1-5-21-1-2-3-1001");
            assert_eq!(s, "D:P(A;;GA;;;S-1-5-21-1-2-3-1001)");
            for other in ["BA", "WD", "AU", "AN", "SY"] {
                assert!(!s.contains(&format!(";;;{other})")), "{other}");
            }
        }

        #[test]
        fn client_token_must_be_same_user_and_elevated() {
            let me = [1u8, 5, 0, 0, 0, 0, 0, 5, 21, 0, 0, 0];
            let other = [1u8, 5, 0, 0, 0, 0, 0, 5, 21, 0, 0, 1];
            assert!(client_token_ok(true, &me, &me).is_ok());
            assert!(client_token_ok(false, &me, &me).is_err(), "medium IL");
            assert!(client_token_ok(true, &other, &me).is_err(), "other account");
            assert!(client_token_ok(false, &other, &me).is_err());
        }

        #[test]
        fn progress_must_be_consistent_and_monotonic() {
            let mut p = HelperProgress::default();
            assert!(!accept_progress(&p, 0, 0), "no total");
            assert!(!accept_progress(&p, 11, 10), "done above total");
            assert!(accept_progress(&p, 4, 10));
            p.records_done = 4;
            p.records_total = 10;
            assert!(!accept_progress(&p, 3, 10), "backwards");
            assert!(!accept_progress(&p, 5, 20), "total changed");
            assert!(accept_progress(&p, 4, 10), "repeat is fine");
            assert!(accept_progress(&p, 10, 10));
        }

        #[test]
        fn entry_cap_follows_the_volume() {
            assert_eq!(entry_cap(1000), 4000);
            assert_eq!(entry_cap(u64::MAX), MAX_TOTAL_ENTRIES);
            assert_eq!(entry_cap(0), 0);
        }

        #[test]
        fn current_user_sid_is_well_formed() {
            let sid = current_user_sid().unwrap();
            assert!(sid.starts_with("S-1-5-"), "{sid}");
        }

        #[test]
        fn eligibility_rejects_non_local_roots() {
            assert!(!eligibility(r"\\server\share").eligible);
            assert!(!eligibility(r"\\?\C:\x").eligible);
            assert!(!eligibility("relative").eligible);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FER-463 L3: the debug bypass needs the explicit opt-in; release never bypasses.
    #[test]
    fn the_debug_location_bypass_is_opt_in() {
        assert!(location_rule_bypassed(true, Some("1")));
        for value in [
            None,
            Some(""),
            Some("0"),
            Some("true"),
            Some("1 "),
            Some("yes"),
        ] {
            assert!(!location_rule_bypassed(true, value), "{value:?}");
        }
        for value in [None, Some("1")] {
            assert!(!location_rule_bypassed(false, value), "{value:?}");
        }
    }

    #[test]
    fn protected_dir_is_component_wise() {
        use std::path::Path;
        let roots = vec![
            PathBuf::from(r"C:\Program Files"),
            PathBuf::from(r"C:\Program Files (x86)"),
        ];
        assert!(in_protected_dir(
            Path::new(r"C:\Program Files\Ferrum"),
            &roots
        ));
        assert!(in_protected_dir(
            Path::new(r"c:\program files\ferrum"),
            &roots
        ));
        assert!(in_protected_dir(
            Path::new(r"C:\Program Files (x86)\F"),
            &roots
        ));
        assert!(!in_protected_dir(
            Path::new(r"C:\Program Files Evil\F"),
            &roots
        ));
        assert!(!in_protected_dir(
            Path::new(r"C:\Users\x\AppData\Local\Ferrum"),
            &roots
        ));
        assert!(!in_protected_dir(Path::new(r"D:\Program Files\F"), &roots));
        assert!(!in_protected_dir(Path::new(r"C:\Program Files"), &[]));
    }

    #[cfg(windows)]
    #[test]
    fn known_program_files_roots_resolve() {
        let roots = protected_roots();
        assert!(!roots.is_empty());
        assert!(roots.iter().all(|r| r.is_absolute()));
    }

    #[test]
    fn drive_letters() {
        assert_eq!(drive_letter_of(r"C:\"), Some('C'));
        assert_eq!(drive_letter_of(r"d:\Data"), Some('D'));
        assert_eq!(drive_letter_of("C:"), None);
        assert_eq!(drive_letter_of(r"C:relative"), None);
        assert_eq!(drive_letter_of(r"\\srv\share"), None);
        assert_eq!(drive_letter_of(r"\\?\C:\"), None);
    }
}
