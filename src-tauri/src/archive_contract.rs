//! F2-02 archive boundary. Not registered with Tauri yet; format backends and
//! commands are integrated in F2-03–F2-12. Archive member paths are never OS paths.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::path::PathBuf;

/// Fixed, path-free messages for the read-only budgets (solid-block skip work
/// and codec memory). The preview maps these to safe UI text.
pub const SOLID_TOO_LARGE: &str = "solid block too large to preview";
pub const CODEC_TOO_LARGE: &str = "codec memory too large to preview";

/// Budget for decompressed bytes skipped to reach a member of a solid block.
pub const MAX_SOLID_SKIP_BYTES: u64 = 256 * 1024 * 1024;
/// Budget for wall time spent skipping.
pub const MAX_SOLID_SKIP_SECS: u64 = 5;

/// Cooperative cancellation (FER-438). A token is shared between the thread
/// that runs a blocking archive read and whoever may abandon it. The read
/// loops (7z skip/read per 64 KiB chunk, RAR between members, the preview
/// collector) poll [`is_cancelled`], so a read the frontend no longer wants
/// stops within one chunk. The token is installed per thread with
/// [`with_cancel`], so provider signatures stay unchanged.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(std::sync::Arc<std::sync::atomic::AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }
    /// True when both handles share the same flag (same job), not merely the same value.
    pub fn same_job(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0)
    }
}

thread_local! {
    static CURRENT_CANCEL: std::cell::RefCell<Option<CancelToken>> =
        const { std::cell::RefCell::new(None) };
}

/// Run `f` with `token` installed for the current thread (restored after).
pub fn with_cancel<T>(token: &CancelToken, f: impl FnOnce() -> T) -> T {
    struct Restore(Option<CancelToken>);
    impl Drop for Restore {
        fn drop(&mut self) {
            CURRENT_CANCEL.with(|c| *c.borrow_mut() = self.0.take());
        }
    }
    let previous = CURRENT_CANCEL.with(|c| c.borrow_mut().replace(token.clone()));
    let _restore = Restore(previous);
    f()
}

/// True when the token installed on this thread was cancelled.
pub fn is_cancelled() -> bool {
    CURRENT_CANCEL.with(|c| c.borrow().as_ref().is_some_and(CancelToken::is_cancelled))
}

/// `Err(Cancelled)` once the current thread's token is cancelled.
pub fn check_cancelled() -> Result<(), ArchiveError> {
    if is_cancelled() {
        Err(ArchiveError::new(
            ArchiveErrorCode::Cancelled,
            "archive read cancelled",
        ))
    } else {
        Ok(())
    }
}

/// Canonical slash-separated relative archive member. Empty means archive root.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MemberPath(String);

impl MemberPath {
    pub fn parse(value: &str) -> Result<Self, ArchiveError> {
        if !value.is_empty()
            && (value.starts_with('/')
                || value.ends_with('/')
                || value.split('/').any(|part| {
                    part.is_empty()
                        || part == "."
                        || part == ".."
                        || part.ends_with('.')
                        || part.ends_with(' ')
                        || part.chars().any(|c| {
                            c == '\\' || c == ':' || c.is_control() || "<>\"|?*".contains(c)
                        })
                        || is_reserved_name(part)
                }))
        {
            return Err(ArchiveError::new(
                ArchiveErrorCode::InvalidMemberPath,
                "Invalid archive member path",
            ));
        }
        Ok(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn parent(&self) -> Option<Self> {
        if self.0.is_empty() {
            return None;
        }
        let prefix = self.0.rsplit_once('/').map_or("", |(parent, _)| parent);
        Some(Self(prefix.to_owned()))
    }
    pub fn child(&self, name: &str) -> Result<Self, ArchiveError> {
        let child = Self::parse(name)?;
        if name.is_empty() || name.contains('/') {
            return Err(ArchiveError::new(
                ArchiveErrorCode::InvalidMemberPath,
                "Child must be one member name",
            ));
        }
        Ok(Self(if self.0.is_empty() {
            child.0
        } else {
            format!("{}/{name}", self.0)
        }))
    }
}

fn is_reserved_name(part: &str) -> bool {
    let stem = part.split('.').next().unwrap_or_default();
    let upper = stem.to_ascii_uppercase();
    matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (["COM", "LPT"]
            .iter()
            .any(|prefix| upper.starts_with(prefix))
            && stem.chars().count() == 4
            && stem
                .chars()
                .last()
                .is_some_and(|digit| matches!(digit, '1'..='9' | '¹' | '²' | '³')))
}

impl Serialize for MemberPath {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}
impl<'de> Deserialize<'de> for MemberPath {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

fn deserialize_disk_path<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let path = String::deserialize(deserializer)?;
    validate_disk_path(&path).map_err(serde::de::Error::custom)?;
    Ok(path)
}
fn validate_disk_path(path: &str) -> Result<(), ArchiveError> {
    if path.trim().is_empty() || path.contains('\0') {
        return Err(ArchiveError::new(
            ArchiveErrorCode::InvalidMemberPath,
            "Invalid disk path",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum Location {
    Disk {
        #[serde(deserialize_with = "deserialize_disk_path")]
        path: String,
    },
    Archive {
        #[serde(rename = "archivePath", deserialize_with = "deserialize_disk_path")]
        archive_path: String,
        #[serde(rename = "memberPath")]
        member_path: MemberPath,
    },
}

impl Location {
    pub fn member_path(&self) -> Option<&MemberPath> {
        match self {
            Self::Archive { member_path, .. } => Some(member_path),
            Self::Disk { .. } => None,
        }
    }
    pub fn archive_root(&self) -> Option<Self> {
        match self {
            Self::Archive { archive_path, .. } => Some(Self::Archive {
                archive_path: archive_path.clone(),
                member_path: MemberPath(String::new()),
            }),
            Self::Disk { .. } => None,
        }
    }
    pub fn archive_child(&self, name: &str) -> Result<Self, ArchiveError> {
        match self {
            Self::Archive {
                archive_path,
                member_path,
            } => Ok(Self::Archive {
                archive_path: archive_path.clone(),
                member_path: member_path.child(name)?,
            }),
            Self::Disk { .. } => Err(ArchiveError::new(
                ArchiveErrorCode::InvalidMemberPath,
                "Not inside archive",
            )),
        }
    }
    pub fn parent(&self) -> Option<Self> {
        match self {
            Self::Disk { .. } => None, // Disk navigation remains owned by existing disk APIs.
            Self::Archive {
                archive_path,
                member_path,
            } => match member_path.parent() {
                Some(parent) => Some(Self::Archive {
                    archive_path: archive_path.clone(),
                    member_path: parent,
                }),
                None => {
                    let slash = archive_path.rfind(['/', '\\'])?;
                    let parent = if slash == 0
                        || (slash == 2 && archive_path.as_bytes().get(1) == Some(&b':'))
                    {
                        &archive_path[..=slash]
                    } else {
                        &archive_path[..slash]
                    };
                    Some(Self::Disk {
                        path: parent.to_owned(),
                    })
                }
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveEntry {
    pub member_path: MemberPath,
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified_ms: Option<i64>,
}
impl ArchiveEntry {
    pub fn validate(&self) -> Result<(), ArchiveError> {
        if self.member_path.as_str().is_empty()
            || self.member_path.as_str().rsplit('/').next() != Some(self.name.as_str())
        {
            return Err(ArchiveError::new(
                ArchiveErrorCode::InvalidMemberPath,
                "Entry name differs from member path",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ArchiveFormat {
    Zip,
    Sevenz,
    Rar,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveCapabilities {
    pub format: ArchiveFormat,
    pub list: bool,
    pub read: bool,
    pub extract: bool,
    pub create: bool,
    pub test: bool,
    pub encrypted_read: bool,
    pub encrypted_create: bool,
    pub multi_volume: bool,
}
impl ArchiveCapabilities {
    pub fn validate(&self) -> Result<(), ArchiveError> {
        if self.format == ArchiveFormat::Rar && (self.create || self.encrypted_create) {
            return Err(ArchiveError::new(
                ArchiveErrorCode::UnsupportedFormat,
                "RAR creation is unsupported",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ArchiveErrorCode {
    UnsupportedFormat,
    UnsupportedCodec,
    PasswordRequired,
    BadPassword,
    CorruptArchive,
    UnsafeEntry,
    LimitExceeded,
    InvalidMemberPath,
    Conflict,
    Cancelled,
    /// FER-75: the file is volume N (>1) of a multipart archive; open the first volume.
    MultipartNotFirst,
    /// FER-75: the first volume was opened but later volumes are missing from its folder.
    MultipartMissing,
    Io,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveError {
    pub code: ArchiveErrorCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archive_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub member_path: Option<String>,
}
impl ArchiveError {
    pub fn new(code: ArchiveErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            archive_path: None,
            member_path: None,
        }
    }
}
impl std::fmt::Display for ArchiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}
impl std::error::Error for ArchiveError {}

/// IPC names planned for registration: `archive_list`, `archive_read`,
/// `archive_extract`, `archive_create`, `archive_test`. This module declares
/// payloads, not callable command handlers: no backend exists at F2-02.
/// FER-504: `Debug` view of an optional password that never prints its value:
/// `Some("***")` when present, `None` otherwise. Request types carrying a
/// password implement `Debug` by hand with this so a `{:?}` in a log line or
/// panic message cannot leak it.
pub(crate) struct RedactedPassword<'a>(pub(crate) &'a Option<String>);
impl std::fmt::Debug for RedactedPassword<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            Some(_) => f.debug_tuple("Some").field(&"***").finish(),
            None => f.write_str("None"),
        }
    }
}
impl std::fmt::Debug for ArchiveListRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArchiveListRequest")
            .field("location", &self.location)
            .field("request_token", &self.request_token)
            .field("password", &RedactedPassword(&self.password))
            .finish()
    }
}
impl std::fmt::Debug for ArchiveReadRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArchiveReadRequest")
            .field("location", &self.location)
            .field("max_bytes", &self.max_bytes)
            .field("password", &RedactedPassword(&self.password))
            .finish()
    }
}
impl std::fmt::Debug for ArchiveExtractRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArchiveExtractRequest")
            .field("archive_path", &self.archive_path)
            .field("member_paths", &self.member_paths)
            .field("destination_path", &self.destination_path)
            .field("conflict", &self.conflict)
            .field("limits", &self.limits)
            .field("password", &RedactedPassword(&self.password))
            .finish()
    }
}
impl std::fmt::Debug for ArchiveTestRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArchiveTestRequest")
            .field("archive_path", &self.archive_path)
            .field("password", &RedactedPassword(&self.password))
            .finish()
    }
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveListRequest {
    pub location: Location,
    pub request_token: u64,
    /// FER-504: optional archive password. Header-encrypted RAR (`-hp`) and
    /// 7z need it even to LIST. Absent → providers behave exactly as before;
    /// a header-encrypted archive then answers `PasswordRequired`. Memory
    /// only: callers hold it while the archive is open and never persist,
    /// log, or emit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveListResponse {
    pub location: Location,
    pub request_token: u64,
    pub entries: Vec<ArchiveEntry>,
}
impl ArchiveListResponse {
    pub fn validate(&self) -> Result<(), ArchiveError> {
        let Location::Archive { member_path, .. } = &self.location else {
            return Err(ArchiveError::new(
                ArchiveErrorCode::InvalidMemberPath,
                "Listing requires archive location",
            ));
        };
        for entry in &self.entries {
            entry.validate()?;
            let remainder = if member_path.as_str().is_empty() {
                entry.member_path.as_str()
            } else {
                entry
                    .member_path
                    .as_str()
                    .strip_prefix(member_path.as_str())
                    .and_then(|p| p.strip_prefix('/'))
                    .unwrap_or("")
            };
            if remainder.is_empty() || remainder.contains('/') {
                return Err(ArchiveError::new(
                    ArchiveErrorCode::InvalidMemberPath,
                    "Entry outside requested directory",
                ));
            }
        }
        Ok(())
    }
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveReadRequest {
    pub location: Location,
    pub max_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveReadResponse {
    pub bytes: Vec<u8>,
    pub truncated: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveOperationResponse {
    pub operation_id: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveLimits {
    pub max_entries: u64,
    pub max_total_bytes: u64,
    pub max_entry_bytes: u64,
    pub max_depth: u32,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ArchiveConflictPolicy {
    Skip,
    Overwrite,
    Rename,
    OverwriteOlder,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveExtractRequest {
    pub archive_path: String,
    /// Empty selects every member. Nonempty selects exact paths/subtrees.
    pub member_paths: Vec<MemberPath>,
    pub destination_path: String,
    pub conflict: ArchiveConflictPolicy,
    pub limits: ArchiveLimits,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ArchiveCreateFormat {
    Zip,
    Sevenz,
    Rar,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ArchiveCompressionLevel {
    Store,
    Fast,
    #[default]
    Normal,
    Max,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveCreateRequest {
    pub format: ArchiveCreateFormat,
    pub source_paths: Vec<String>,
    pub destination_path: String,
    pub conflict: ArchiveConflictPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<ArchiveCompressionLevel>,
}

impl ArchiveCreateRequest {
    pub fn level(&self) -> ArchiveCompressionLevel {
        self.level.unwrap_or_default()
    }
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveTestRequest {
    pub archive_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ArchiveOperationKind {
    Extract,
    Create,
    Test,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveProgress {
    pub operation_id: String,
    pub kind: ArchiveOperationKind,
    pub processed_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_bytes: Option<u64>,
    pub processed_entries: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_entries: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_member_path: Option<MemberPath>,
    /// FER-55: Number of files successfully written during extraction
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub written_entries: Option<u64>,
    /// FER-55: Number of files skipped during extraction
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped_entries: Option<u64>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ArchiveTestStatus {
    Passed,
    Failed,
    Unsupported,
    PasswordRequired,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveTestEntry {
    pub member_path: MemberPath,
    pub status: ArchiveTestStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ArchiveError>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveTestReport {
    pub archive_path: String,
    pub entries: Vec<ArchiveTestEntry>,
}

/// Format-independent backend interface. Implementations must list from archive
/// metadata without extracting; extraction/create stream to safe staging paths,
/// honor conflict/limits/cancellation, and test reads content without disk output.
/// `read` invokes `chunk` only up to request.max_bytes while retaining decoder
/// ownership, so codec, checksum, and cancellation failures stay typed.
/// Progress callback returns false to request cancellation.
pub trait ArchiveProvider {
    fn capabilities(&self, format: ArchiveFormat) -> ArchiveCapabilities;
    fn list(&self, request: &ArchiveListRequest) -> Result<ArchiveListResponse, ArchiveError>;
    fn read(
        &self,
        request: &ArchiveReadRequest,
        chunk: &mut dyn FnMut(&[u8]) -> Result<(), ArchiveError>,
    ) -> Result<bool, ArchiveError>;
    fn extract(
        &self,
        request: &ArchiveExtractRequest,
        progress: &mut dyn FnMut(ArchiveProgress) -> bool,
    ) -> Result<(), ArchiveError>;
    fn create(
        &self,
        request: &ArchiveCreateRequest,
        progress: &mut dyn FnMut(ArchiveProgress) -> bool,
    ) -> Result<PathBuf, ArchiveError>;
    fn test(
        &self,
        request: &ArchiveTestRequest,
        progress: &mut dyn FnMut(ArchiveProgress) -> bool,
    ) -> Result<ArchiveTestReport, ArchiveError>;

    /// FER-488: true when the provider can work from an archive handle the caller already
    /// opened and checked (`list_opened` / `read_opened`). False means the backend API only
    /// takes a path (unrar); the caller must then pin the checked file before passing a path.
    fn reads_opened(&self) -> bool {
        false
    }

    /// FER-488: `list`, but from `file` (opened and checked by the caller) instead of opening
    /// `archive_path` again; `archive_path` is then only a label. Fails closed for providers
    /// that cannot do this.
    fn list_opened(
        &self,
        file: std::fs::File,
        request: &ArchiveListRequest,
    ) -> Result<ArchiveListResponse, ArchiveError> {
        let _ = (file, request);
        Err(ArchiveError::new(
            ArchiveErrorCode::UnsupportedFormat,
            "Provider cannot read from an opened archive handle",
        ))
    }

    /// FER-488: `read`, but from `file` (see `list_opened`).
    fn read_opened(
        &self,
        file: std::fs::File,
        request: &ArchiveReadRequest,
        chunk: &mut dyn FnMut(&[u8]) -> Result<(), ArchiveError>,
    ) -> Result<bool, ArchiveError> {
        let _ = (file, request, chunk);
        Err(ArchiveError::new(
            ArchiveErrorCode::UnsupportedFormat,
            "Provider cannot read from an opened archive handle",
        ))
    }
}

/// (name, is_dir, size, modified_ms)
pub type RawLevelRow = (String, bool, u64, Option<i64>);

/// One raw member path → the row it contributes to the level under `prefix`
/// (`""` for the root, otherwise `"<dir>/"`), or `None` when the member lies
/// outside that level. A deeper member implies its first path segment as a
/// folder row.
pub fn level_row(
    prefix: &str,
    raw: &str,
    is_dir: bool,
    size: u64,
    modified_ms: Option<i64>,
) -> Option<RawLevelRow> {
    let rest = raw.strip_prefix(prefix)?;
    if rest.is_empty() {
        return None;
    }
    match rest.find('/') {
        Some(slash) => {
            let child = &rest[..slash];
            if child.is_empty() {
                None
            } else {
                let is_direct_dir = slash == rest.len() - 1;
                Some((
                    child.to_owned(),
                    true,
                    0,
                    if is_direct_dir { modified_ms } else { None },
                ))
            }
        }
        None => Some((
            rest.to_owned(),
            is_dir,
            if is_dir { 0 } else { size },
            modified_ms,
        )),
    }
}

/// FER-504, FER-508: type-aware dedup of one directory level's raw listing rows.
///
/// A folder is listed once whether it came from an explicit directory entry
/// or was implied by a deeper member path — but only when the names match
/// **exactly** (case-sensitively). Folders differing only in case (`docs` / `Docs`)
/// stay two rows, because the subfolder listing matches its prefix case-sensitively:
/// merging them would hide `Docs/*` members that extraction still writes.
///
/// A name shared by two files, or by a file and a folder, is a `Conflict` —
/// the same verdict the extract preflight reaches (`archive_extract::preflight`
/// rejects any repeated normalized name) — carrying the clashing member path.
/// That comparison folds to lowercase like the pipeline's `normalize()`, so
/// `X` and `x` clash on the case-insensitive Windows target. First occurrence
/// wins for merged folder rows (updating `modified_ms` if previously absent).
pub fn dedup_level(
    directory: &str,
    rows: Vec<RawLevelRow>,
) -> Result<Vec<RawLevelRow>, ArchiveError> {
    let mut seen_lower: std::collections::HashMap<String, bool /* is_dir */> =
        std::collections::HashMap::with_capacity(rows.len());
    let mut seen_dirs: std::collections::HashMap<String /* exact */, usize> =
        std::collections::HashMap::with_capacity(rows.len());
    let mut merged: Vec<RawLevelRow> = Vec::with_capacity(rows.len());

    for (name, is_dir, size, modified_ms) in rows {
        let lower = name.to_lowercase();
        match seen_lower.entry(lower) {
            std::collections::hash_map::Entry::Vacant(v) => {
                let idx = merged.len();
                merged.push((name.clone(), is_dir, size, modified_ms));
                v.insert(is_dir);
                if is_dir {
                    seen_dirs.insert(name, idx);
                }
            }
            std::collections::hash_map::Entry::Occupied(o) => {
                if is_dir && *o.get() {
                    // Folder + folder: merge only on an exact (case-sensitive) match;
                    // a case variant is its own row so its members stay reachable.
                    if let Some(&idx) = seen_dirs.get(&name) {
                        let existing = &mut merged[idx];
                        if existing.3.is_none() && modified_ms.is_some() {
                            existing.3 = modified_ms;
                        }
                    } else {
                        let idx = merged.len();
                        seen_dirs.insert(name.clone(), idx);
                        merged.push((name, true, size, modified_ms));
                    }
                    continue;
                }
                // File + file, or file + folder (case-insensitive): extraction would
                // refuse this archive, so the listing refuses to show a quiet row.
                let mut err =
                    ArchiveError::new(ArchiveErrorCode::Conflict, "Duplicate archive member");
                err.member_path = Some(if directory.is_empty() {
                    name
                } else {
                    format!("{directory}/{name}")
                });
                return Err(err);
            }
        }
    }
    Ok(merged)
}

#[cfg(test)]
mod debug_redaction_tests {
    use super::*;

    const SECRET: &str = "hunter2-secret";

    fn archive_location() -> Location {
        Location::Archive {
            archive_path: r"C:\a\enc.rar".into(),
            member_path: MemberPath::parse("docs").unwrap(),
        }
    }

    fn assert_masked(shown: String) {
        assert!(!shown.contains(SECRET), "{shown}");
        assert!(shown.contains(r#"password: Some("***")"#), "{shown}");
    }

    #[test]
    fn request_debug_never_prints_the_password() {
        let list = ArchiveListRequest {
            location: archive_location(),
            request_token: 7,
            password: Some(SECRET.into()),
        };
        assert_masked(format!("{list:?}"));
        assert!(!format!("{list:#?}").contains(SECRET));
        let read = ArchiveReadRequest {
            location: archive_location(),
            max_bytes: 1,
            password: Some(SECRET.into()),
        };
        assert_masked(format!("{read:?}"));
        let extract = ArchiveExtractRequest {
            archive_path: r"C:\a\enc.rar".into(),
            member_paths: Vec::new(),
            destination_path: r"C:\out".into(),
            conflict: ArchiveConflictPolicy::Skip,
            limits: ArchiveLimits {
                max_entries: 1,
                max_total_bytes: 1,
                max_entry_bytes: 1,
                max_depth: 1,
            },
            password: Some(SECRET.into()),
        };
        assert_masked(format!("{extract:?}"));
        let test = ArchiveTestRequest {
            archive_path: r"C:\a\enc.rar".into(),
            password: Some(SECRET.into()),
        };
        assert_masked(format!("{test:?}"));
    }

    #[test]
    fn request_debug_shows_absent_password_as_none() {
        let list = ArchiveListRequest {
            location: archive_location(),
            request_token: 7,
            password: None,
        };
        let shown = format!("{list:?}");
        assert!(shown.contains("password: None"), "{shown}");
        assert!(shown.contains("request_token: 7"), "{shown}");
    }
}

#[cfg(test)]
mod dedup_scale_tests {
    use super::*;

    #[test]
    fn dedup_level_linear_scale_test() {
        let mut rows = Vec::with_capacity(40_000);
        // 20k unique folder entries
        for i in 0..20_000 {
            rows.push((format!("unique_dir_{i}"), true, 0, None));
        }
        // 20k duplicate folder entries (1,000 distinct names repeated 20 times)
        for _ in 0..20 {
            for i in 0..1_000 {
                rows.push((format!("dup_dir_{i}"), true, 0, None));
            }
        }
        let start = std::time::Instant::now();
        let res = dedup_level("", rows).expect("dedup should succeed");
        let elapsed = start.elapsed();
        assert_eq!(res.len(), 21_000);
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "dedup_level took {:?}, expected < 2s (quadratic regression)",
            elapsed
        );
    }

    #[test]
    fn dedup_level_case_variant_scale_test() {
        const BASE: &str = "abcdefghijklmnopqrst"; // 20 letters
        assert_eq!(BASE.len(), 20);
        let mut variants = Vec::with_capacity(30_000);
        for i in 0..30_000u32 {
            let mut s = String::with_capacity(20);
            for (bit, ch) in BASE.chars().enumerate() {
                if (i & (1 << bit)) != 0 {
                    s.push(ch.to_ascii_uppercase());
                } else {
                    s.push(ch.to_ascii_lowercase());
                }
            }
            variants.push(s);
        }

        // 30k distinct case variants, 2 rows each -> 60k total rows
        let mut rows = Vec::with_capacity(60_000);
        for s in &variants {
            rows.push((s.clone(), true, 0, None));
        }
        for s in variants {
            rows.push((s, true, 0, Some(12345)));
        }

        let start = std::time::Instant::now();
        let res = dedup_level("", rows).expect("dedup should succeed");
        let elapsed = start.elapsed();

        assert_eq!(res.len(), 30_000);
        assert_eq!(res[0].3, Some(12345));
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "dedup_level case variants took {:?}, expected < 2s (quadratic regression)",
            elapsed
        );
    }
}
