//! Domain contract shared across the IPC boundary.
//!
//! The TS mirror lives in `src/lib/ipc/types.ts`; the fixtures in
//! `contract-fixtures/` are the single source of truth both sides test
//! against. Change one, change all three.
//!
//! Number safety (decision D18): sizes are `u64` bytes and timestamps are
//! `i64` unix epoch milliseconds. Both stay far below 2^53 for any real
//! filesystem object, so they cross into JS as plain numbers without
//! precision loss. Directory sizes are 0; aggregation is a later concern.

use serde::{Deserialize, Serialize};

/// One row of a directory listing. Intentionally minimal — anything the
/// file list cannot display does not cross the boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileEntry {
    /// Basename for display.
    pub name: String,
    /// Absolute path with platform separators; the identity used for
    /// navigation and operations.
    pub path: String,
    pub is_dir: bool,
    /// True for symlinks themselves (and junctions on Windows); recursion
    /// never follows them (decision D07).
    pub is_symlink: bool,
    /// Bytes for files, 0 for directories.
    pub size: u64,
    /// Unix epoch milliseconds.
    pub modified_ms: i64,
    /// Hidden per platform convention (dot-prefix or Windows attribute).
    pub hidden: bool,
}

/// Stable machine-readable error codes. Never leak panics or raw OS strings
/// as the code — `message` carries the human detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ErrorCode {
    NotFound,
    PermissionDenied,
    InvalidPath,
    NotADirectory,
    AlreadyExists,
    Cancelled,
    Unsupported,
    Io,
}

/// The only error shape that crosses IPC.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FsError {
    pub code: ErrorCode,
    pub message: String,
    /// Offending path when one is known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// FER-73: the structured archive error code behind an archive failure (password required,
    /// wrong password, corrupt, cancelled, multipart...). The UI branches on this, never on the
    /// message text. Absent for non-archive errors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive_code: Option<crate::archive_contract::ArchiveErrorCode>,
    /// FER-55: Offending member path inside an archive when one is known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member_path: Option<String>,
}

impl FsError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            path: None,
            archive_code: None,
            member_path: None,
        }
    }

    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }
}

impl std::fmt::Display for FsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.path {
            Some(p) => write!(f, "{:?} at {p}: {}", self.code, self.message),
            None => write!(f, "{:?}: {}", self.code, self.message),
        }
    }
}

impl std::error::Error for FsError {}

impl From<std::io::Error> for FsError {
    fn from(err: std::io::Error) -> Self {
        let code = match err.kind() {
            std::io::ErrorKind::NotFound => ErrorCode::NotFound,
            std::io::ErrorKind::PermissionDenied => ErrorCode::PermissionDenied,
            std::io::ErrorKind::InvalidInput | std::io::ErrorKind::InvalidFilename => {
                ErrorCode::InvalidPath
            }
            _ => ErrorCode::Io,
        };
        FsError::new(code, err.to_string())
    }
}

/// Boundary validation. Ferrum is a real file manager: users may open any
/// location the OS allows, so this rejects only structurally unusable input
/// (empty / whitespace-only / NUL-containing strings). It deliberately does
/// NOT restrict to any sandbox root.
pub fn validate_path(path: &str) -> Result<(), FsError> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err(FsError::new(
            ErrorCode::InvalidPath,
            "Path must not be empty",
        ));
    }
    if path.contains('\0') {
        return Err(FsError::new(
            ErrorCode::InvalidPath,
            "Path must not contain NUL",
        ));
    }
    Ok(())
}

// ── Operation queue contract (used from task 09; defined here so the
//    boundary stays in one file) ─────────────────────────────────────────

/// Opaque, stable operation id (format: `op_<26 chars>`).
pub type OperationId = String;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OperationKind {
    Copy,
    Move,
    Delete,
    Create,
    Rename,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OperationState {
    Queued,
    Running,
    Paused,
    Completed,
    Failed,
    Cancelled,
}

/// One per-file failure attached to a terminal operation (F2-26 BTM
/// per-file detail). Engines that continue after item errors report the
/// collected failures in their final progress.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationFailure {
    pub path: String,
    pub message: String,
}

/// Progress event payload. Emitted at most 5–10 times per second per
/// operation (decision D14 / task 09). The trailing optional fields are the
/// F2-26 BTM traceability additions: failure reason, per-file failures,
/// and enqueue/finish wall-clock stamps (epoch ms, D18) for the history.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationProgress {
    pub operation_id: OperationId,
    pub kind: OperationKind,
    pub state: OperationState,
    pub processed_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_bytes: Option<u64>,
    pub processed_files: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_files: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_bytes_per_sec: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<OperationFailure>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queued_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at_ms: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../contract-fixtures");

    fn fixture(name: &str) -> String {
        // Checkouts may carry CRLF; compare JSON content, not line endings.
        std::fs::read_to_string(format!("{FIXTURE_DIR}/{name}"))
            .expect("fixture readable")
            .replace("\r\n", "\n")
    }

    /// Git may check fixtures out with CRLF (`core.autocrlf`), while
    /// `serde_json::to_string_pretty` always emits `\n`. Compare JSON
    /// newline-agnostically so the round-trip stays byte-exact modulo line
    /// endings while still catching shape, ordering, and indent drift.
    fn normalize_newlines(s: &str) -> String {
        s.replace("\r\n", "\n")
    }

    #[test]
    fn file_entry_fixture_round_trips() {
        let raw = fixture("file-entry.json");
        let entry: FileEntry = serde_json::from_str(&raw).expect("deserializes");
        assert_eq!(entry.name, "hélix-eñgine");
        assert!(entry.is_dir);
        assert_eq!(entry.modified_ms, 1_790_512_000_000);
        // Re-serialization must match the fixture (modulo line endings) so
        // the TS side sees exactly this shape.
        let back = serde_json::to_string_pretty(&entry).expect("serializes");
        assert_eq!(
            normalize_newlines(back.trim()),
            normalize_newlines(raw.trim())
        );
    }

    #[test]
    fn fs_error_fixture_round_trips() {
        let raw = fixture("fs-error.json");
        let err: FsError = serde_json::from_str(&raw).expect("deserializes");
        assert_eq!(err.code, ErrorCode::PermissionDenied);
        assert_eq!(err.path.as_deref(), Some(r"C:\Windows\CSC"));
        let back = serde_json::to_string_pretty(&err).expect("serializes");
        assert_eq!(
            normalize_newlines(back.trim()),
            normalize_newlines(raw.trim())
        );
    }

    #[test]
    fn operation_progress_fixture_round_trips() {
        let raw = fixture("operation-progress.json");
        let p: OperationProgress = serde_json::from_str(&raw).expect("deserializes");
        assert_eq!(p.state, OperationState::Running);
        assert_eq!(p.kind, OperationKind::Copy);
        assert_eq!(p.total_files, Some(3482));
        let back = serde_json::to_string_pretty(&p).expect("serializes");
        assert_eq!(
            normalize_newlines(back.trim()),
            normalize_newlines(raw.trim())
        );
    }

    #[test]
    fn optional_fields_are_omitted_not_null() {
        let p = OperationProgress {
            operation_id: "op_x".into(),
            kind: OperationKind::Delete,
            state: OperationState::Queued,
            processed_bytes: 0,
            total_bytes: None,
            processed_files: 0,
            total_files: None,
            current_path: None,
            rate_bytes_per_sec: None,
            error: None,
            failures: Vec::new(),
            queued_at_ms: None,
            finished_at_ms: None,
        };
        let json = serde_json::to_value(&p).unwrap();
        assert!(json.get("totalBytes").is_none());
        assert!(json.get("currentPath").is_none());
        // F2-26 traceability fields stay omitted until populated.
        assert!(json.get("error").is_none());
        assert!(json.get("failures").is_none());
        assert!(json.get("queuedAtMs").is_none());
        assert!(json.get("finishedAtMs").is_none());
    }

    #[test]
    fn failures_traceability_round_trips() {
        let p = OperationProgress {
            operation_id: "op_x".into(),
            kind: OperationKind::Copy,
            state: OperationState::Failed,
            processed_bytes: 0,
            total_bytes: None,
            processed_files: 1,
            total_files: Some(2),
            current_path: None,
            rate_bytes_per_sec: None,
            error: Some("1 of 2 item(s) failed: locked".into()),
            failures: vec![OperationFailure {
                path: r"C:\t\locked.bin".into(),
                message: "locked by another process".into(),
            }],
            queued_at_ms: Some(1_790_512_000_000),
            finished_at_ms: Some(1_790_512_000_500),
        };
        let json = serde_json::to_value(&p).unwrap();
        assert_eq!(
            json.get("error").and_then(|v| v.as_str()),
            Some("1 of 2 item(s) failed: locked")
        );
        assert_eq!(
            json.get("failures")
                .and_then(|v| v.as_array())
                .map(|a| a.len()),
            Some(1)
        );
        assert_eq!(
            json.get("queuedAtMs").and_then(|v| v.as_i64()),
            Some(1_790_512_000_000)
        );
        let back: OperationProgress = serde_json::from_value(json).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn io_errors_map_to_codes() {
        assert_eq!(
            FsError::from(std::io::Error::from(std::io::ErrorKind::NotFound)).code,
            ErrorCode::NotFound
        );
        assert_eq!(
            FsError::from(std::io::Error::from(std::io::ErrorKind::PermissionDenied)).code,
            ErrorCode::PermissionDenied
        );
        assert_eq!(
            FsError::from(std::io::Error::from(std::io::ErrorKind::InvalidInput)).code,
            ErrorCode::InvalidPath
        );
    }

    #[test]
    fn validate_path_rejects_structural_garbage_only() {
        assert!(validate_path("").is_err());
        assert!(validate_path("   ").is_err());
        assert!(validate_path("a\0b").is_err());
        for ok in ["C:\\", "/home/arif", "\\\\server\\share", "D:/békér fólder"] {
            assert!(validate_path(ok).is_ok(), "should accept {ok:?}");
        }
    }
}
