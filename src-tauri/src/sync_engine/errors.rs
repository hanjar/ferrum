//! Error classification for profile runs (Automation design §5.6, RQ-7).
//!
//! Every failure of one operation is mapped to an [`ErrorClass`] (what the run does next) and a
//! [`Reason`] (a closed vocabulary shown to the user). Raw OS error text never leaves this module:
//! `io::Error` / `FsError` messages can carry paths, so only the numeric Win32 code is read from
//! them and the user-facing text comes from fixed templates.

use std::io;

use serde::Serialize;

use crate::domain::{ErrorCode, FsError};

/// What the run does after a failed operation (design §5.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ErrorClass {
    /// Retry after a re-check, up to the configured count (RQ-7.1).
    Transient,
    /// Stop the run at once (RQ-7.2).
    Systemic,
    /// The device or share is gone: stop after the current file, Interrupted (RQ-7.4).
    DeviceLost,
    /// Fail this operation, continue (RQ-7.3).
    PerItem,
    /// Anything else: per item, but three in a row become systemic.
    Unknown,
}

/// Closed vocabulary of failure reasons. `code()` is what crosses IPC; the text never contains a
/// path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    SharingViolation,
    LockViolation,
    NetworkBusy,
    NetworkError,
    AccessDenied,
    WriteProtected,
    DiskFull,
    LoginFailure,
    AccountRestriction,
    DeviceNotReady,
    DeviceLost,
    NotFound,
    InvalidName,
    PathTooLong,
    FileTooLarge,
    CantAccess,
    AlreadyExists,
    NotEmpty,
    NotADirectory,
    ParentIsFile,
    LinkInPath,
    LinkUnsupported,
    VerifyFailed,
    TargetAppeared,
    /// A file sent to the Recycle Bin did not end up there (the shell deleted it for good).
    RecycleBypassed,
    /// FER-562: the path is inside one of Ferrum's own state folders or a Windows Startup folder.
    ProtectedFolder,
    Unknown,
}

impl Serialize for Reason {
    /// On the wire a reason is its closed snake_case code.
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.code())
    }
}

impl Reason {
    pub fn code(self) -> &'static str {
        match self {
            Reason::SharingViolation => "sharing_violation",
            Reason::LockViolation => "lock_violation",
            Reason::NetworkBusy => "network_busy",
            Reason::NetworkError => "network_error",
            Reason::AccessDenied => "access_denied",
            Reason::WriteProtected => "write_protected",
            Reason::DiskFull => "disk_full",
            Reason::LoginFailure => "login_failure",
            Reason::AccountRestriction => "account_restriction",
            Reason::DeviceNotReady => "device_not_ready",
            Reason::DeviceLost => "device_lost",
            Reason::NotFound => "not_found",
            Reason::InvalidName => "invalid_name",
            Reason::PathTooLong => "path_too_long",
            Reason::FileTooLarge => "file_too_large",
            Reason::CantAccess => "cant_access",
            Reason::AlreadyExists => "already_exists",
            Reason::NotEmpty => "not_empty",
            Reason::NotADirectory => "not_a_directory",
            Reason::ParentIsFile => "parent_is_file",
            Reason::LinkInPath => "link_in_path",
            Reason::LinkUnsupported => "link_unsupported",
            Reason::VerifyFailed => "verify_failed",
            Reason::TargetAppeared => "target_appeared",
            Reason::RecycleBypassed => "recycle_bypassed",
            Reason::ProtectedFolder => "protected_folder",
            Reason::Unknown => "unknown",
        }
    }

    /// Fixed, path-free sentence for the run error text (stored in history).
    pub fn text(self) -> &'static str {
        match self {
            Reason::SharingViolation | Reason::LockViolation => {
                "a file was in use by another program"
            }
            Reason::NetworkBusy | Reason::NetworkError => "the network did not answer",
            Reason::AccessDenied => "access was denied",
            Reason::WriteProtected => "the destination is write protected",
            Reason::DiskFull => "the destination is full",
            Reason::LoginFailure | Reason::AccountRestriction => "sign-in to the share failed",
            Reason::DeviceNotReady | Reason::DeviceLost => "a drive was disconnected",
            Reason::NotFound => "a file disappeared",
            Reason::InvalidName => "a name is not valid on the destination",
            Reason::PathTooLong => "a path is too long",
            Reason::FileTooLarge => "a file is too large for the destination",
            Reason::CantAccess => "a file could not be accessed",
            Reason::AlreadyExists => "a file already exists",
            Reason::NotEmpty => "a folder is not empty",
            Reason::NotADirectory | Reason::ParentIsFile => "a folder name is taken by a file",
            Reason::LinkInPath => "a folder link was found where a real folder is required",
            Reason::LinkUnsupported => "links are not synchronized",
            Reason::VerifyFailed => "the copy could not be verified",
            Reason::TargetAppeared => "the destination appeared during the run",
            Reason::RecycleBypassed => "a file was removed without going to the Recycle Bin",
            Reason::ProtectedFolder => {
                "the destination is a folder Ferrum protects (its own settings or Windows Startup)"
            }
            Reason::Unknown => "an unexpected error occurred",
        }
    }
}

/// A classified failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Classified {
    pub class: ErrorClass,
    pub reason: Reason,
}

const fn c(class: ErrorClass, reason: Reason) -> Classified {
    Classified { class, reason }
}

/// Map a Win32 error code (design §5.6). `on_root` says the failing path is a sync root itself,
/// where "path not found" means the device is gone rather than a vanished file.
pub fn classify_win32(code: u32, on_root: bool) -> Classified {
    use ErrorClass::*;
    match code {
        32 => c(Transient, Reason::SharingViolation),
        33 => c(Transient, Reason::LockViolation),
        54 => c(Transient, Reason::NetworkBusy),
        59 | 64 | 121 | 1231 => c(Transient, Reason::NetworkError),
        5 => c(Systemic, Reason::AccessDenied),
        19 => c(Systemic, Reason::WriteProtected),
        39 | 112 => c(Systemic, Reason::DiskFull),
        86 | 1326 => c(Systemic, Reason::LoginFailure),
        1327..=1331 => c(Systemic, Reason::AccountRestriction),
        21 => c(DeviceLost, Reason::DeviceNotReady),
        55 | 1167 => c(DeviceLost, Reason::DeviceLost),
        53 | 3 if on_root => c(DeviceLost, Reason::DeviceLost),
        2 | 3 => c(PerItem, Reason::NotFound),
        53 => c(PerItem, Reason::NetworkError),
        123 | 267 | 161 => c(PerItem, Reason::InvalidName),
        206 => c(PerItem, Reason::PathTooLong),
        223 => c(PerItem, Reason::FileTooLarge),
        1920 => c(PerItem, Reason::CantAccess),
        80 | 183 => c(PerItem, Reason::AlreadyExists),
        145 => c(PerItem, Reason::NotEmpty),
        _ => c(Unknown, Reason::Unknown),
    }
}

/// Classify a plain `io::Error` (uses the raw Win32 code when there is one).
pub fn classify_io(e: &io::Error, on_root: bool) -> Classified {
    if let Some(code) = e.raw_os_error() {
        let r = classify_win32(code as u32, on_root);
        if r.class != ErrorClass::Unknown {
            return r;
        }
    }
    match e.kind() {
        io::ErrorKind::NotFound => c(ErrorClass::PerItem, Reason::NotFound),
        io::ErrorKind::PermissionDenied => c(ErrorClass::Systemic, Reason::AccessDenied),
        io::ErrorKind::AlreadyExists => c(ErrorClass::PerItem, Reason::AlreadyExists),
        io::ErrorKind::InvalidInput | io::ErrorKind::InvalidFilename => {
            c(ErrorClass::PerItem, Reason::InvalidName)
        }
        _ => c(ErrorClass::Unknown, Reason::Unknown),
    }
}

/// The numeric code of a `(os error N)` suffix in an error text, if any. `FsError` flattens an
/// `io::Error` into text, so this is how the Win32 code is recovered.
pub fn os_error_in(text: &str) -> Option<u32> {
    let at = text.rfind("(os error ")?;
    let rest = &text[at + "(os error ".len()..];
    let end = rest.find(')')?;
    rest[..end].trim().parse().ok()
}

/// The NTSTATUS in a `NtCreateFile failed: 0x........` text produced by the no-follow primitives,
/// converted to its Win32 code.
fn nt_status_in(text: &str) -> Option<u32> {
    let at = text.find("failed: 0x")?;
    let hex: String = text[at + "failed: 0x".len()..]
        .chars()
        .take_while(|ch| ch.is_ascii_hexdigit())
        .collect();
    let status = u32::from_str_radix(&hex, 16).ok()?;
    // STATUS_DELETE_PENDING: another program is deleting this very file. That is a per-item
    // event ("it is already going away"), not an access problem that should stop the run.
    if status == 0xC000_0056 {
        return Some(2);
    }
    nt_status_to_win32(status)
}

#[cfg(windows)]
fn nt_status_to_win32(status: u32) -> Option<u32> {
    use windows_sys::Win32::Foundation::RtlNtStatusToDosError;
    // SAFETY: a pure lookup of a status value; no pointers.
    let code = unsafe { RtlNtStatusToDosError(status as i32) };
    // 317 (ERROR_MR_MID_NOT_FOUND) is what the call answers for an unmapped status.
    (code != 0 && code != 317).then_some(code)
}

#[cfg(not(windows))]
fn nt_status_to_win32(_status: u32) -> Option<u32> {
    None
}

/// Classify a flattened text (`FsError.message`, `PublishError::Io`).
pub fn classify_text(text: &str, code: Option<ErrorCode>, on_root: bool) -> Classified {
    if let Some(win32) = os_error_in(text).or_else(|| nt_status_in(text)) {
        let r = classify_win32(win32, on_root);
        if r.class != ErrorClass::Unknown {
            return r;
        }
    }
    match code {
        Some(ErrorCode::NotFound) => c(ErrorClass::PerItem, Reason::NotFound),
        Some(ErrorCode::PermissionDenied) => c(ErrorClass::Systemic, Reason::AccessDenied),
        Some(ErrorCode::InvalidPath) => c(ErrorClass::PerItem, Reason::InvalidName),
        Some(ErrorCode::AlreadyExists) => c(ErrorClass::PerItem, Reason::AlreadyExists),
        Some(ErrorCode::NotADirectory) => c(ErrorClass::PerItem, Reason::ParentIsFile),
        Some(ErrorCode::Unsupported) => c(ErrorClass::PerItem, Reason::LinkUnsupported),
        Some(ErrorCode::Busy) => c(ErrorClass::Transient, Reason::SharingViolation),
        _ => c(ErrorClass::Unknown, Reason::Unknown),
    }
}

/// Classify a copy-engine failure.
pub fn classify_fs(e: &FsError, on_root: bool) -> Classified {
    classify_text(&e.message, Some(e.code), on_root)
}

/// Retry backoff before attempt `n` (1-based): 2 s, 10 s, 30 s (RQ-7.1).
pub fn backoff(attempt: u8) -> std::time::Duration {
    std::time::Duration::from_secs(match attempt {
        0 | 1 => 2,
        2 => 10,
        _ => 30,
    })
}

/// Three unknown failures in a row become systemic (design §5.6).
pub const UNKNOWN_STREAK_LIMIT: u32 = 3;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn win32_table_matches_design_5_6() {
        for code in [32, 33, 54, 59, 64, 121, 1231] {
            assert_eq!(classify_win32(code, false).class, ErrorClass::Transient);
        }
        for code in [5, 19, 39, 112, 1326, 1327, 1328, 1329, 1330, 1331, 86] {
            assert_eq!(classify_win32(code, false).class, ErrorClass::Systemic);
        }
        for code in [21, 55, 1167] {
            assert_eq!(classify_win32(code, false).class, ErrorClass::DeviceLost);
        }
        // 53 / 3 are device loss only on a root.
        assert_eq!(classify_win32(53, true).class, ErrorClass::DeviceLost);
        assert_eq!(classify_win32(3, true).class, ErrorClass::DeviceLost);
        assert_eq!(classify_win32(3, false).class, ErrorClass::PerItem);
        for code in [2, 123, 206, 223, 1920] {
            assert_eq!(classify_win32(code, false).class, ErrorClass::PerItem);
        }
        assert_eq!(classify_win32(99_999, false).class, ErrorClass::Unknown);
    }

    #[test]
    fn os_error_suffix_is_read_from_text() {
        assert_eq!(os_error_in("The file is in use. (os error 32)"), Some(32));
        assert_eq!(os_error_in("no code here"), None);
        let fs = FsError::new(ErrorCode::Io, "being used by another process (os error 32)");
        assert_eq!(
            classify_fs(&fs, false),
            c(ErrorClass::Transient, Reason::SharingViolation)
        );
        let denied = FsError::new(
            ErrorCode::PermissionDenied,
            "Access is denied. (os error 5)",
        );
        assert_eq!(classify_fs(&denied, false).class, ErrorClass::Systemic);
    }

    #[test]
    fn io_errors_classify_by_code_then_kind() {
        let e = io::Error::from_raw_os_error(32);
        assert_eq!(classify_io(&e, false).class, ErrorClass::Transient);
        let nf = io::Error::from(io::ErrorKind::NotFound);
        assert_eq!(classify_io(&nf, false).reason, Reason::NotFound);
        let weird = io::Error::other("x");
        assert_eq!(classify_io(&weird, false).class, ErrorClass::Unknown);
    }

    #[test]
    fn reason_codes_are_path_free_and_closed() {
        for r in [
            Reason::SharingViolation,
            Reason::AccessDenied,
            Reason::LinkInPath,
            Reason::ProtectedFolder,
            Reason::Unknown,
        ] {
            assert!(!r.code().contains('\\') && !r.text().contains('\\'));
            assert!(
                r.code()
                    .chars()
                    .all(|ch| ch.is_ascii_lowercase() || ch == '_')
            );
        }
    }

    #[test]
    fn a_delete_pending_file_is_a_per_item_event_not_a_systemic_stop() {
        let c = classify_text("NtCreateFile failed: 0xc0000056", None, false);
        assert_eq!(c.class, ErrorClass::PerItem);
        assert_eq!(c.reason, Reason::NotFound);
    }

    #[test]
    fn backoff_is_2_10_30_seconds() {
        let secs = |n| backoff(n).as_secs();
        assert_eq!((secs(1), secs(2), secs(3), secs(9)), (2, 10, 30, 30));
    }
}
