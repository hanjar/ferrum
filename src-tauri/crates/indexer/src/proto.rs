//! Framed request/response protocol on the `FerrumIndexer` named pipe
//! (FER-474, IDX-S1).
//!
//! Frame = `len: u32 LE` + `kind: u8` + payload; `len` counts the kind byte
//! plus the payload. Requests are tiny and capped at [`MAX_REQUEST_FRAME`];
//! responses at [`MAX_RESPONSE_FRAME`]. Both decoders are trust boundaries
//! (the service decodes requests from any interactive user, Ferrum decodes
//! responses): every length, enum byte and string is checked, trailing bytes
//! are rejected, and nothing is allocated before its size has been bounded.
//!
//! Conversation:
//! 1. client → `Hello` (magic + protocol version, no data). The service needs
//!    one read before it can impersonate the client; it then authenticates
//!    the client TOKEN and answers `HelloOk` or `Error` + close.
//! 2. any number of request → response pairs, strictly one at a time.
//! 3. either side closes. The service closes on idle, on rate-limit abuse,
//!    on any malformed frame, and on stop.

use crate::index::{Kind, valid_name};
use crate::query::{
    ChangeStatus, ChangedDir, Changes, FolderSize, FolderSizes, JournalMark, KindFilter,
    MAX_CHANGED_DIRS, MAX_EXTENSION, MAX_EXTENSIONS, MAX_FOLDER_NAMES, MAX_PAGE, MAX_PATH_BYTES,
    MAX_PATTERN, MAX_SCOPE, MatchKind, QueryCursor, RescanReason, SearchHit, SearchPage,
    SearchQuery, SubtreeEntry, SubtreeInfo, parse_abs_dir,
};
use std::io::{self, Read, Write};

/// `FRIX` — Ferrum IndeX.
pub const MAGIC: [u8; 4] = *b"FRIX";
/// v2 (FER-475): `indexStatus` request, index state in `status`.
/// v3 (FER-476): `search` and `subtree` queries.
/// v4 (FER-477): `folderSizes` and `changes` queries.
pub const PROTO_VERSION: u16 = 4;
/// Hard cap on one request frame (kind byte + payload). The largest request
/// is `folderSizes`: parent (1024) + up to 200 names; the client splits a
/// batch so that it fits. Any frame above this is refused BEFORE it is
/// allocated (8 connections x 16 KiB at most).
pub const MAX_REQUEST_FRAME: u32 = 16 * 1024;
/// Hard cap on one response frame (a search page or subtree chunk is cut at
/// ≈256 KiB by the service; this is the decoder's hard limit).
pub const MAX_RESPONSE_FRAME: u32 = 1 << 20;
/// Most entries in one subtree chunk frame.
pub const MAX_CHUNK_ENTRIES: u32 = 16 * 1024;
/// Cap on any string carried in a frame (bytes of UTF-8).
pub const MAX_TEXT: usize = 200;

const REQ_HELLO: u8 = 0x01;
const REQ_PING: u8 = 0x02;
const REQ_STATUS: u8 = 0x03;
const REQ_VERSION: u8 = 0x04;
const REQ_GET_MODE: u8 = 0x05;
const REQ_SET_MODE: u8 = 0x06;
const REQ_INDEX_STATUS: u8 = 0x07;
const REQ_SEARCH: u8 = 0x08;
const REQ_SUBTREE: u8 = 0x09;
const REQ_FOLDER_SIZES: u8 = 0x0A;
const REQ_CHANGES: u8 = 0x0B;

const RES_HELLO_OK: u8 = 0x81;
const RES_PONG: u8 = 0x82;
const RES_STATUS: u8 = 0x83;
const RES_VERSION: u8 = 0x84;
const RES_MODE: u8 = 0x85;
const RES_MODE_SET: u8 = 0x86;
const RES_INDEX_STATUS: u8 = 0x87;
const RES_SEARCH: u8 = 0x88;
const RES_SUBTREE: u8 = 0x89;
const RES_FOLDER_SIZES: u8 = 0x8A;
const RES_CHANGES: u8 = 0x8B;
const RES_ERROR: u8 = 0xFF;

/// Privacy mode of the indexer (chosen at install time, changeable later).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mode {
    /// Single-user PC: index everything, no per-user result filtering.
    HighPerformance,
    /// Shared PC: results filtered by the caller's access rights (S2+), other
    /// users' profiles not indexed. The privacy-safe default.
    Shared,
}

impl Mode {
    pub const ALL: [Mode; 2] = [Mode::HighPerformance, Mode::Shared];

    pub fn as_str(self) -> &'static str {
        match self {
            Mode::HighPerformance => "high_performance",
            Mode::Shared => "shared",
        }
    }

    /// Exact, case-sensitive parse of the wire/config/CLI spelling.
    pub fn parse(s: &str) -> Option<Mode> {
        match s {
            "high_performance" => Some(Mode::HighPerformance),
            "shared" => Some(Mode::Shared),
            _ => None,
        }
    }

    fn to_byte(self) -> u8 {
        match self {
            Mode::HighPerformance => 1,
            Mode::Shared => 2,
        }
    }

    fn from_byte(b: u8) -> Option<Mode> {
        match b {
            1 => Some(Mode::HighPerformance),
            2 => Some(Mode::Shared),
            _ => None,
        }
    }

    /// Does switching from `self` to `to` reduce privacy? Such a change needs
    /// an elevated administrator (see `auth::may_set_mode`).
    pub fn lowers_privacy_to(self, to: Mode) -> bool {
        self == Mode::Shared && to == Mode::HighPerformance
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceState {
    Running,
    Paused,
}

impl ServiceState {
    fn to_byte(self) -> u8 {
        match self {
            ServiceState::Running => 1,
            ServiceState::Paused => 2,
        }
    }
    fn from_byte(b: u8) -> Option<Self> {
        match b {
            1 => Some(ServiceState::Running),
            2 => Some(ServiceState::Paused),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            ServiceState::Running => "running",
            ServiceState::Paused => "paused",
        }
    }
}

/// Overall index state in `status` (per-volume detail: `indexStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexState {
    /// No indexable volume (or the worker has not discovered any yet).
    None,
    /// At least one volume is being built.
    Building,
    /// No build running and at least one volume ready.
    Ready,
    /// Every volume failed.
    Error,
}

impl IndexState {
    fn to_byte(self) -> u8 {
        match self {
            IndexState::None => 0,
            IndexState::Building => 1,
            IndexState::Ready => 2,
            IndexState::Error => 3,
        }
    }
    fn from_byte(b: u8) -> Option<Self> {
        Some(match b {
            0 => IndexState::None,
            1 => IndexState::Building,
            2 => IndexState::Ready,
            3 => IndexState::Error,
            _ => return None,
        })
    }
    pub fn as_str(self) -> &'static str {
        match self {
            IndexState::None => "none",
            IndexState::Building => "building",
            IndexState::Ready => "ready",
            IndexState::Error => "error",
        }
    }

    /// Aggregate of the per-volume states.
    pub fn aggregate(volumes: &[VolumeStatus]) -> IndexState {
        if volumes.is_empty() {
            IndexState::None
        } else if volumes.iter().any(|v| v.state == VolumeState::Building) {
            IndexState::Building
        } else if volumes.iter().any(|v| v.state == VolumeState::Ready) {
            IndexState::Ready
        } else {
            IndexState::Error
        }
    }
}

/// State of one volume's index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeState {
    /// Pending or reading the MFT (see `progress_permille`).
    Building,
    /// Built (and live when `live`).
    Ready,
    /// Failed; retried later (see `error`).
    Error,
}

impl VolumeState {
    fn to_byte(self) -> u8 {
        match self {
            VolumeState::Building => 1,
            VolumeState::Ready => 2,
            VolumeState::Error => 3,
        }
    }
    fn from_byte(b: u8) -> Option<Self> {
        Some(match b {
            1 => VolumeState::Building,
            2 => VolumeState::Ready,
            3 => VolumeState::Error,
            _ => return None,
        })
    }
    pub fn as_str(self) -> &'static str {
        match self {
            VolumeState::Building => "building",
            VolumeState::Ready => "ready",
            VolumeState::Error => "error",
        }
    }
}

/// Why a volume is in `Error` (codes only: no paths or names on the wire).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeErrorCode {
    None,
    /// The volume could not be opened for reading (console mode without
    /// admin rights, or denied).
    AccessDenied,
    OpenFailed,
    NotNtfs,
    MftLayout,
    ReadFailed,
    /// The index would exceed the memory budget.
    MemoryLimit,
    Internal,
    /// Shared mode: the profile list could not be read or resolved, so the
    /// volume is not indexed (fail closed).
    ProfilesUnavailable,
    /// Shared mode: the volume is mounted inside a user profile.
    PrivateVolume,
}

impl VolumeErrorCode {
    fn to_byte(self) -> u8 {
        match self {
            VolumeErrorCode::None => 0,
            VolumeErrorCode::AccessDenied => 1,
            VolumeErrorCode::OpenFailed => 2,
            VolumeErrorCode::NotNtfs => 3,
            VolumeErrorCode::MftLayout => 4,
            VolumeErrorCode::ReadFailed => 5,
            VolumeErrorCode::MemoryLimit => 6,
            VolumeErrorCode::Internal => 7,
            VolumeErrorCode::ProfilesUnavailable => 8,
            VolumeErrorCode::PrivateVolume => 9,
        }
    }
    fn from_byte(b: u8) -> Option<Self> {
        Some(match b {
            0 => VolumeErrorCode::None,
            1 => VolumeErrorCode::AccessDenied,
            2 => VolumeErrorCode::OpenFailed,
            3 => VolumeErrorCode::NotNtfs,
            4 => VolumeErrorCode::MftLayout,
            5 => VolumeErrorCode::ReadFailed,
            6 => VolumeErrorCode::MemoryLimit,
            7 => VolumeErrorCode::Internal,
            8 => VolumeErrorCode::ProfilesUnavailable,
            9 => VolumeErrorCode::PrivateVolume,
            _ => return None,
        })
    }
    pub fn as_str(self) -> &'static str {
        match self {
            VolumeErrorCode::None => "none",
            VolumeErrorCode::AccessDenied => "access_denied",
            VolumeErrorCode::OpenFailed => "open_failed",
            VolumeErrorCode::NotNtfs => "not_ntfs",
            VolumeErrorCode::MftLayout => "mft_layout",
            VolumeErrorCode::ReadFailed => "read_failed",
            VolumeErrorCode::MemoryLimit => "memory_limit",
            VolumeErrorCode::Internal => "internal",
            VolumeErrorCode::ProfilesUnavailable => "profiles_unavailable",
            VolumeErrorCode::PrivateVolume => "private_volume",
        }
    }
}

/// Most volumes in one `indexStatus` (drive letters A-Z).
pub const MAX_VOLUMES: usize = 26;

/// One volume in `indexStatus`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeStatus {
    /// ASCII drive letter `A`..=`Z`.
    pub letter: u8,
    pub state: VolumeState,
    pub error: VolumeErrorCode,
    pub entries: u64,
    pub memory_bytes: u64,
    /// USN journal ID (0 = none).
    pub journal_id: u64,
    /// Next USN the index will read (its position in the journal).
    pub next_usn: i64,
    /// Kept current from the USN journal (false = snapshot, no journal).
    pub live: bool,
    /// Build progress 0..=1000 (1000 = ready).
    pub progress_permille: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexStatusInfo {
    /// Index work is paused (service paused).
    pub paused: bool,
    /// FER-476 review H1: the service token is not restricted by the
    /// service SID, so search/subtree are switched off (`degraded`).
    pub degraded: bool,
    pub memory_bytes: u64,
    pub memory_limit: u64,
    pub volumes: Vec<VolumeStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Hello,
    Ping {
        token: u64,
    },
    Status,
    Version,
    GetMode,
    SetMode(Mode),
    IndexStatus,
    /// FER-476: one page of a name search.
    Search(SearchQuery),
    /// FER-476: stream every visible entry below an absolute directory
    /// (Disk Usage). Answered by `SubtreeChunk` frames until `done`.
    Subtree {
        path: String,
    },
    /// FER-477a: total size of folders `parent` + `name` (high_performance
    /// only; shared mode answers `forbidden`).
    FolderSizes {
        parent: String,
        names: Vec<String>,
    },
    /// FER-477b: directories below `path` changed since `since` (none =
    /// just the current journal position).
    Changes {
        path: String,
        since: Option<JournalMark>,
    },
}

/// One frame of a subtree answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubtreeChunk {
    pub info: SubtreeInfo,
    /// Last frame of this answer.
    pub done: bool,
    pub entries: Vec<SubtreeEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelloInfo {
    pub proto: u16,
    /// Whether THIS client may switch to `high_performance` (elevated admin).
    pub can_lower_privacy: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusInfo {
    pub state: ServiceState,
    pub mode: Mode,
    pub index: IndexState,
    pub version: String,
    pub uptime_secs: u64,
    /// True when the server runs as a console process (dev/test), not under
    /// the SCM.
    pub console: bool,
    /// FER-476 review H1: queries are off because the service token is not
    /// restricted (SID type changed, or the install probe failed).
    pub degraded: bool,
}

/// Error codes carried by `Response::Error`. Messages are short English
/// diagnostics; Ferrum shows its own text per code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    BadRequest,
    Unauthenticated,
    Forbidden,
    /// The change needs an elevated administrator (UAC).
    NeedsElevation,
    RateLimited,
    Busy,
    Paused,
    UnsupportedVersion,
    Internal,
    /// FER-476: the scope is not searchable from the index (not found, not
    /// listable by the client, excluded, or not indexed right now). The
    /// client falls back to its own scan.
    NotIndexed,
    Timeout,
    TooLarge,
    /// FER-476 review H1: queries are disabled (service token not restricted).
    Degraded,
}

impl ErrorCode {
    fn to_u16(self) -> u16 {
        match self {
            ErrorCode::BadRequest => 1,
            ErrorCode::Unauthenticated => 2,
            ErrorCode::Forbidden => 3,
            ErrorCode::NeedsElevation => 4,
            ErrorCode::RateLimited => 5,
            ErrorCode::Busy => 6,
            ErrorCode::Paused => 7,
            ErrorCode::UnsupportedVersion => 8,
            ErrorCode::Internal => 9,
            ErrorCode::NotIndexed => 10,
            ErrorCode::Timeout => 11,
            ErrorCode::TooLarge => 12,
            ErrorCode::Degraded => 13,
        }
    }
    fn from_u16(v: u16) -> Option<Self> {
        Some(match v {
            1 => ErrorCode::BadRequest,
            2 => ErrorCode::Unauthenticated,
            3 => ErrorCode::Forbidden,
            4 => ErrorCode::NeedsElevation,
            5 => ErrorCode::RateLimited,
            6 => ErrorCode::Busy,
            7 => ErrorCode::Paused,
            8 => ErrorCode::UnsupportedVersion,
            9 => ErrorCode::Internal,
            10 => ErrorCode::NotIndexed,
            11 => ErrorCode::Timeout,
            12 => ErrorCode::TooLarge,
            13 => ErrorCode::Degraded,
            _ => return None,
        })
    }
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::BadRequest => "bad_request",
            ErrorCode::Unauthenticated => "unauthenticated",
            ErrorCode::Forbidden => "forbidden",
            ErrorCode::NeedsElevation => "needs_elevation",
            ErrorCode::RateLimited => "rate_limited",
            ErrorCode::Busy => "busy",
            ErrorCode::Paused => "paused",
            ErrorCode::UnsupportedVersion => "unsupported_version",
            ErrorCode::Internal => "internal",
            ErrorCode::NotIndexed => "not_indexed",
            ErrorCode::Timeout => "timeout",
            ErrorCode::TooLarge => "too_large",
            ErrorCode::Degraded => "degraded",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Response {
    HelloOk(HelloInfo),
    Pong { token: u64 },
    Status(StatusInfo),
    Version { version: String, proto: u16 },
    Mode(Mode),
    ModeSet { mode: Mode, changed: bool },
    IndexStatus(IndexStatusInfo),
    SearchPage(SearchPage),
    SubtreeChunk(SubtreeChunk),
    FolderSizes(FolderSizes),
    Changes(Changes),
    Error { code: ErrorCode, message: String },
}

#[derive(Debug, PartialEq, Eq)]
pub enum ProtocolError {
    /// The peer closed the pipe at a frame boundary.
    Eof,
    Io(String),
    FrameTooLarge(u32),
    EmptyFrame,
    UnknownKind(u8),
    Malformed(&'static str),
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Eof => write!(f, "pipe closed"),
            Self::Io(e) => write!(f, "pipe I/O: {e}"),
            Self::FrameTooLarge(n) => write!(f, "frame too large ({n} bytes)"),
            Self::EmptyFrame => write!(f, "empty frame"),
            Self::UnknownKind(k) => write!(f, "unknown frame kind {k:#04x}"),
            Self::Malformed(why) => write!(f, "malformed frame: {why}"),
        }
    }
}

impl std::error::Error for ProtocolError {}

// ── encoding ─────────────────────────────────────────────────────────────

fn put_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn put_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// A string as `u16 len + bytes`, cut to [`MAX_TEXT`] on a char boundary and
/// with control characters replaced, so the encoder never produces a frame
/// its own decoder would reject.
fn put_text(out: &mut Vec<u8>, s: &str) {
    let clean: String = s
        .chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect();
    let mut cut = clean.len().min(MAX_TEXT);
    while !clean.is_char_boundary(cut) {
        cut -= 1;
    }
    let b = &clean.as_bytes()[..cut];
    put_u16(out, b.len() as u16);
    out.extend_from_slice(b);
}

/// A long string (`u16 len + bytes`) whose size the caller already bounded
/// (patterns, paths, names): written as is.
fn put_ltext(out: &mut Vec<u8>, s: &str) {
    let b = &s.as_bytes()[..s.len().min(u16::MAX as usize)];
    put_u16(out, b.len() as u16);
    out.extend_from_slice(b);
}

fn kind_byte(k: Kind) -> u8 {
    k as u8
}

fn match_byte(m: MatchKind) -> u8 {
    match m {
        MatchKind::Substring => 1,
        MatchKind::Prefix => 2,
        MatchKind::Glob => 3,
    }
}

fn kinds_byte(k: KindFilter) -> u8 {
    match k {
        KindFilter::Files => 1,
        KindFilter::Dirs => 2,
        KindFilter::Both => 3,
    }
}

const ALLOC_NONE: u64 = u64::MAX;

fn rescan_byte(r: RescanReason) -> u8 {
    match r {
        RescanReason::JournalChanged => 1,
        RescanReason::Unavailable => 2,
        RescanReason::TooMany => 3,
        RescanReason::ExcludedInside => 4,
        RescanReason::RootChanged => 5,
        RescanReason::Unverifiable => 6,
    }
}

fn rescan_from(b: u8) -> Option<RescanReason> {
    Some(match b {
        1 => RescanReason::JournalChanged,
        2 => RescanReason::Unavailable,
        3 => RescanReason::TooMany,
        4 => RescanReason::ExcludedInside,
        5 => RescanReason::RootChanged,
        6 => RescanReason::Unverifiable,
        _ => return None,
    })
}

/// A changed directory's path relative to the requested one: empty, or
/// valid names joined by single backslashes.
fn valid_rel(rel: &str) -> bool {
    rel.is_empty() || rel.split('\\').all(valid_name)
}

/// A USN on the wire: never negative.
fn usn_from(v: u64) -> Result<i64, ProtocolError> {
    i64::try_from(v).map_err(|_| ProtocolError::Malformed("bad usn"))
}

fn frame(body: Vec<u8>) -> Vec<u8> {
    let mut f = Vec::with_capacity(4 + body.len());
    put_u32(&mut f, body.len() as u32);
    f.extend_from_slice(&body);
    f
}

pub fn encode_request(req: &Request) -> Vec<u8> {
    let mut b = Vec::with_capacity(16);
    match req {
        Request::Hello => {
            b.push(REQ_HELLO);
            b.extend_from_slice(&MAGIC);
            put_u16(&mut b, PROTO_VERSION);
        }
        Request::Ping { token } => {
            b.push(REQ_PING);
            put_u64(&mut b, *token);
        }
        Request::Status => b.push(REQ_STATUS),
        Request::Version => b.push(REQ_VERSION),
        Request::GetMode => b.push(REQ_GET_MODE),
        Request::SetMode(m) => {
            b.push(REQ_SET_MODE);
            b.push(m.to_byte());
        }
        Request::IndexStatus => b.push(REQ_INDEX_STATUS),
        Request::Search(q) => {
            b.push(REQ_SEARCH);
            b.push(match_byte(q.match_kind));
            b.push(kinds_byte(q.kinds));
            put_u16(&mut b, q.limit);
            b.push(q.cursor.letter);
            put_u32(&mut b, q.cursor.slot);
            put_ltext(&mut b, &q.pattern);
            put_ltext(&mut b, &q.scope);
            b.push(q.extensions.len().min(MAX_EXTENSIONS) as u8);
            for e in q.extensions.iter().take(MAX_EXTENSIONS) {
                put_ltext(&mut b, e);
            }
        }
        Request::Subtree { path } => {
            b.push(REQ_SUBTREE);
            put_ltext(&mut b, path);
        }
        Request::FolderSizes { parent, names } => {
            b.push(REQ_FOLDER_SIZES);
            put_ltext(&mut b, parent);
            let n = names.len().min(MAX_FOLDER_NAMES);
            b.push(n as u8);
            for name in &names[..n] {
                put_ltext(&mut b, name);
            }
        }
        Request::Changes { path, since } => {
            b.push(REQ_CHANGES);
            put_ltext(&mut b, path);
            match since {
                None => b.push(0),
                Some(m) => {
                    b.push(1);
                    put_u64(&mut b, m.journal_id);
                    put_u64(&mut b, m.usn as u64);
                }
            }
        }
    }
    frame(b)
}

pub fn encode_response(res: &Response) -> Vec<u8> {
    let mut b = Vec::with_capacity(64);
    match res {
        Response::HelloOk(h) => {
            b.push(RES_HELLO_OK);
            b.extend_from_slice(&MAGIC);
            put_u16(&mut b, h.proto);
            b.push(h.can_lower_privacy as u8);
        }
        Response::Pong { token } => {
            b.push(RES_PONG);
            put_u64(&mut b, *token);
        }
        Response::Status(s) => {
            b.push(RES_STATUS);
            b.push(s.state.to_byte());
            b.push(s.mode.to_byte());
            b.push(s.index.to_byte());
            put_u64(&mut b, s.uptime_secs);
            b.push(s.console as u8);
            b.push(s.degraded as u8);
            put_text(&mut b, &s.version);
        }
        Response::Version { version, proto } => {
            b.push(RES_VERSION);
            put_u16(&mut b, *proto);
            put_text(&mut b, version);
        }
        Response::Mode(m) => {
            b.push(RES_MODE);
            b.push(m.to_byte());
        }
        Response::ModeSet { mode, changed } => {
            b.push(RES_MODE_SET);
            b.push(mode.to_byte());
            b.push(*changed as u8);
        }
        Response::IndexStatus(s) => {
            b.push(RES_INDEX_STATUS);
            b.push(s.paused as u8 | (s.degraded as u8) << 1);
            put_u64(&mut b, s.memory_bytes);
            put_u64(&mut b, s.memory_limit);
            // The worker never tracks more than 26 volumes; cap anyway so
            // the encoder can never exceed the frame limit.
            let n = s.volumes.len().min(MAX_VOLUMES);
            b.push(n as u8);
            for v in &s.volumes[..n] {
                b.push(v.letter);
                b.push(v.state.to_byte());
                b.push(v.error.to_byte());
                put_u64(&mut b, v.entries);
                put_u64(&mut b, v.memory_bytes);
                put_u64(&mut b, v.journal_id);
                put_u64(&mut b, v.next_usn as u64);
                b.push(v.live as u8);
                put_u16(&mut b, v.progress_permille.min(1000));
            }
        }
        Response::SearchPage(p) => {
            b.push(RES_SEARCH);
            let flags =
                p.stale as u8 | (p.excluded_inside as u8) << 1 | (p.next.is_some() as u8) << 2;
            b.push(flags);
            put_u32(&mut b, p.unavailable & ((1 << 26) - 1));
            let next = p.next.unwrap_or_default();
            b.push(next.letter);
            put_u32(&mut b, next.slot);
            let n = p.hits.len().min(MAX_PAGE as usize);
            put_u16(&mut b, n as u16);
            for h in &p.hits[..n] {
                b.push(kind_byte(h.kind));
                put_u64(&mut b, h.size);
                put_u64(&mut b, h.allocated.unwrap_or(ALLOC_NONE));
                put_u64(&mut b, h.modified);
                put_u32(&mut b, h.attributes);
                put_ltext(&mut b, &h.path);
            }
        }
        Response::SubtreeChunk(c) => {
            b.push(RES_SUBTREE);
            let flags =
                c.info.stale as u8 | (c.info.excluded_inside as u8) << 1 | (c.done as u8) << 2;
            b.push(flags);
            put_u64(&mut b, c.info.root_frn);
            let n = c.entries.len().min(MAX_CHUNK_ENTRIES as usize);
            put_u32(&mut b, n as u32);
            for e in &c.entries[..n] {
                put_u64(&mut b, e.frn);
                put_u64(&mut b, e.parent);
                b.push(kind_byte(e.kind));
                put_u64(&mut b, e.size);
                put_u64(&mut b, e.allocated.unwrap_or(ALLOC_NONE));
                put_ltext(&mut b, &e.name);
            }
        }
        Response::FolderSizes(r) => {
            b.push(RES_FOLDER_SIZES);
            b.push(r.stale as u8);
            let n = r.sizes.len().min(MAX_FOLDER_NAMES);
            b.push(n as u8);
            for s in &r.sizes[..n] {
                match *s {
                    FolderSize::Ok { bytes, files } => {
                        b.push(1);
                        put_u64(&mut b, bytes);
                        put_u64(&mut b, files);
                    }
                    FolderSize::NotIndexed => {
                        b.push(2);
                        put_u64(&mut b, 0);
                        put_u64(&mut b, 0);
                    }
                }
            }
        }
        Response::Changes(c) => {
            b.push(RES_CHANGES);
            put_u64(&mut b, c.mark.journal_id);
            put_u64(&mut b, c.mark.usn as u64);
            put_u64(&mut b, c.root_frn);
            let (status, reason) = match c.status {
                ChangeStatus::Complete => (1u8, 0u8),
                ChangeStatus::MarkOnly => (2, 0),
                ChangeStatus::Rescan(r) => (3, rescan_byte(r)),
            };
            b.push(status);
            b.push(reason);
            // Only a complete answer carries directories; the query never
            // returns more than the cap (it answers rescan instead).
            debug_assert!(c.dirs.len() <= MAX_CHANGED_DIRS);
            let dirs: &[ChangedDir] = if c.status == ChangeStatus::Complete {
                &c.dirs[..c.dirs.len().min(MAX_CHANGED_DIRS)]
            } else {
                &[]
            };
            put_u32(&mut b, dirs.len() as u32);
            for d in dirs {
                b.push(d.recursive as u8);
                put_ltext(&mut b, &d.rel);
            }
        }
        Response::Error { code, message } => {
            b.push(RES_ERROR);
            put_u16(&mut b, code.to_u16());
            put_text(&mut b, message);
        }
    }
    debug_assert!(b.len() as u32 <= MAX_RESPONSE_FRAME);
    frame(b)
}

// ── decoding (untrusted input on both sides) ─────────────────────────────

struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], ProtocolError> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or(ProtocolError::Malformed("length overflow"))?;
        if end > self.buf.len() {
            return Err(ProtocolError::Malformed("truncated"));
        }
        let s = &self.buf[self.pos..end];
        self.pos = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, ProtocolError> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, ProtocolError> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Result<u32, ProtocolError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    /// `u16 len + UTF-8` of at most `max` bytes, no control characters.
    fn ltext(&mut self, max: usize) -> Result<String, ProtocolError> {
        let n = self.u16()? as usize;
        if n > max {
            return Err(ProtocolError::Malformed("text too long"));
        }
        let raw = self.take(n)?;
        let s = std::str::from_utf8(raw).map_err(|_| ProtocolError::Malformed("text not UTF-8"))?;
        if s.chars().any(char::is_control) {
            return Err(ProtocolError::Malformed("control character in text"));
        }
        Ok(s.to_owned())
    }
    fn kind(&mut self) -> Result<Kind, ProtocolError> {
        Kind::from_byte(self.u8()?).ok_or(ProtocolError::Malformed("bad entry kind"))
    }
    fn alloc(&mut self) -> Result<Option<u64>, ProtocolError> {
        let v = self.u64()?;
        Ok((v != ALLOC_NONE).then_some(v))
    }
    fn u64(&mut self) -> Result<u64, ProtocolError> {
        let b = self.take(8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(b);
        Ok(u64::from_le_bytes(a))
    }
    fn bool(&mut self) -> Result<bool, ProtocolError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(ProtocolError::Malformed("bad bool")),
        }
    }
    fn text(&mut self) -> Result<String, ProtocolError> {
        let n = self.u16()? as usize;
        if n > MAX_TEXT {
            return Err(ProtocolError::Malformed("text too long"));
        }
        let raw = self.take(n)?;
        let s = std::str::from_utf8(raw).map_err(|_| ProtocolError::Malformed("text not UTF-8"))?;
        if s.chars().any(char::is_control) {
            return Err(ProtocolError::Malformed("control character in text"));
        }
        Ok(s.to_owned())
    }
    fn finish(&self) -> Result<(), ProtocolError> {
        if self.pos == self.buf.len() {
            Ok(())
        } else {
            Err(ProtocolError::Malformed("trailing bytes"))
        }
    }
}

fn check_magic(c: &mut Cursor<'_>) -> Result<u16, ProtocolError> {
    if c.take(4)? != MAGIC {
        return Err(ProtocolError::Malformed("bad magic"));
    }
    c.u16()
}

fn mode(c: &mut Cursor<'_>) -> Result<Mode, ProtocolError> {
    Mode::from_byte(c.u8()?).ok_or(ProtocolError::Malformed("bad mode"))
}

/// Decode one request body (kind byte + payload). Service side.
pub fn decode_request(body: &[u8]) -> Result<Request, ProtocolError> {
    let mut c = Cursor { buf: body, pos: 0 };
    let kind = c.u8().map_err(|_| ProtocolError::EmptyFrame)?;
    let req = match kind {
        REQ_HELLO => {
            if check_magic(&mut c)? != PROTO_VERSION {
                return Err(ProtocolError::Malformed("unsupported version"));
            }
            Request::Hello
        }
        REQ_PING => Request::Ping { token: c.u64()? },
        REQ_STATUS => Request::Status,
        REQ_VERSION => Request::Version,
        REQ_GET_MODE => Request::GetMode,
        REQ_SET_MODE => Request::SetMode(mode(&mut c)?),
        REQ_INDEX_STATUS => Request::IndexStatus,
        REQ_SEARCH => {
            let match_kind = match c.u8()? {
                1 => MatchKind::Substring,
                2 => MatchKind::Prefix,
                3 => MatchKind::Glob,
                _ => return Err(ProtocolError::Malformed("bad match kind")),
            };
            let kinds = match c.u8()? {
                1 => KindFilter::Files,
                2 => KindFilter::Dirs,
                3 => KindFilter::Both,
                _ => return Err(ProtocolError::Malformed("bad kind filter")),
            };
            let limit = c.u16()?;
            let cursor = QueryCursor {
                letter: c.u8()?,
                slot: c.u32()?,
            };
            let pattern = c.ltext(MAX_PATTERN)?;
            let scope = c.ltext(MAX_SCOPE)?;
            let n = c.u8()? as usize;
            if n > MAX_EXTENSIONS {
                return Err(ProtocolError::Malformed("too many extensions"));
            }
            let mut extensions = Vec::with_capacity(n);
            for _ in 0..n {
                extensions.push(c.ltext(MAX_EXTENSION)?);
            }
            let q = SearchQuery {
                pattern,
                match_kind,
                scope,
                extensions,
                kinds,
                limit,
                cursor,
            };
            // The query rules are part of the frame contract: anything the
            // search would refuse is refused here, before it reaches it.
            q.validate()
                .map_err(|_| ProtocolError::Malformed("invalid search"))?;
            Request::Search(q)
        }
        REQ_SUBTREE => {
            let path = c.ltext(MAX_SCOPE)?;
            if parse_abs_dir(&path).is_none() {
                return Err(ProtocolError::Malformed("invalid subtree path"));
            }
            Request::Subtree { path }
        }
        REQ_FOLDER_SIZES => {
            let parent = c.ltext(MAX_SCOPE)?;
            if parse_abs_dir(&parent).is_none() {
                return Err(ProtocolError::Malformed("invalid folder sizes parent"));
            }
            let n = c.u8()? as usize;
            if n == 0 || n > MAX_FOLDER_NAMES {
                return Err(ProtocolError::Malformed("bad folder count"));
            }
            let mut names = Vec::with_capacity(n);
            for _ in 0..n {
                // valid_name: no `..`, `.`, separators, `:` (alternate data
                // streams) or control characters.
                let name = c.ltext(crate::index::MAX_NAME_BYTES)?;
                if !valid_name(&name) {
                    return Err(ProtocolError::Malformed("bad folder name"));
                }
                names.push(name);
            }
            Request::FolderSizes { parent, names }
        }
        REQ_CHANGES => {
            let path = c.ltext(MAX_SCOPE)?;
            if parse_abs_dir(&path).is_none() {
                return Err(ProtocolError::Malformed("invalid changes path"));
            }
            let since = match c.u8()? {
                0 => None,
                1 => {
                    let journal_id = c.u64()?;
                    let usn = usn_from(c.u64()?)?;
                    Some(JournalMark { journal_id, usn })
                }
                _ => return Err(ProtocolError::Malformed("bad mark flag")),
            };
            Request::Changes { path, since }
        }
        other => return Err(ProtocolError::UnknownKind(other)),
    };
    c.finish()?;
    Ok(req)
}

/// Decode one response body (kind byte + payload). Ferrum side.
pub fn decode_response(body: &[u8]) -> Result<Response, ProtocolError> {
    let mut c = Cursor { buf: body, pos: 0 };
    let kind = c.u8().map_err(|_| ProtocolError::EmptyFrame)?;
    let res = match kind {
        RES_HELLO_OK => {
            let proto = check_magic(&mut c)?;
            if proto != PROTO_VERSION {
                return Err(ProtocolError::Malformed("unsupported version"));
            }
            Response::HelloOk(HelloInfo {
                proto,
                can_lower_privacy: c.bool()?,
            })
        }
        RES_PONG => Response::Pong { token: c.u64()? },
        RES_STATUS => {
            let state = ServiceState::from_byte(c.u8()?)
                .ok_or(ProtocolError::Malformed("bad service state"))?;
            let mode = mode(&mut c)?;
            let index = IndexState::from_byte(c.u8()?)
                .ok_or(ProtocolError::Malformed("bad index state"))?;
            let uptime_secs = c.u64()?;
            let console = c.bool()?;
            let degraded = c.bool()?;
            let version = c.text()?;
            Response::Status(StatusInfo {
                state,
                mode,
                index,
                version,
                uptime_secs,
                console,
                degraded,
            })
        }
        RES_VERSION => {
            let proto = c.u16()?;
            let version = c.text()?;
            Response::Version { version, proto }
        }
        RES_MODE => Response::Mode(mode(&mut c)?),
        RES_MODE_SET => {
            let mode = mode(&mut c)?;
            let changed = c.bool()?;
            Response::ModeSet { mode, changed }
        }
        RES_INDEX_STATUS => {
            let flags = c.u8()?;
            if flags & !0b11 != 0 {
                return Err(ProtocolError::Malformed("bad index flags"));
            }
            let paused = flags & 1 != 0;
            let degraded = flags & 2 != 0;
            let memory_bytes = c.u64()?;
            let memory_limit = c.u64()?;
            let n = c.u8()? as usize;
            if n > MAX_VOLUMES {
                return Err(ProtocolError::Malformed("too many volumes"));
            }
            let mut volumes = Vec::with_capacity(n);
            for _ in 0..n {
                let letter = c.u8()?;
                if !letter.is_ascii_uppercase() {
                    return Err(ProtocolError::Malformed("bad drive letter"));
                }
                let state = VolumeState::from_byte(c.u8()?)
                    .ok_or(ProtocolError::Malformed("bad volume state"))?;
                let error = VolumeErrorCode::from_byte(c.u8()?)
                    .ok_or(ProtocolError::Malformed("bad volume error"))?;
                let entries = c.u64()?;
                let memory_bytes = c.u64()?;
                let journal_id = c.u64()?;
                let next_usn = c.u64()? as i64;
                let live = c.bool()?;
                let progress_permille = c.u16()?;
                if progress_permille > 1000 {
                    return Err(ProtocolError::Malformed("bad progress"));
                }
                volumes.push(VolumeStatus {
                    letter,
                    state,
                    error,
                    entries,
                    memory_bytes,
                    journal_id,
                    next_usn,
                    live,
                    progress_permille,
                });
            }
            Response::IndexStatus(IndexStatusInfo {
                paused,
                degraded,
                memory_bytes,
                memory_limit,
                volumes,
            })
        }
        RES_SEARCH => {
            let flags = c.u8()?;
            if flags & !0b111 != 0 {
                return Err(ProtocolError::Malformed("bad flags"));
            }
            let unavailable = c.u32()?;
            if unavailable >> 26 != 0 {
                return Err(ProtocolError::Malformed("bad volume mask"));
            }
            let cur = QueryCursor {
                letter: c.u8()?,
                slot: c.u32()?,
            };
            let next = if flags & 0b100 != 0 {
                if !cur.letter.is_ascii_uppercase() {
                    return Err(ProtocolError::Malformed("bad cursor"));
                }
                Some(cur)
            } else if cur != QueryCursor::default() {
                return Err(ProtocolError::Malformed("bad cursor"));
            } else {
                None
            };
            let n = c.u16()?;
            if n > MAX_PAGE {
                return Err(ProtocolError::Malformed("too many hits"));
            }
            let mut hits = Vec::with_capacity(n as usize);
            for _ in 0..n {
                let kind = c.kind()?;
                let size = c.u64()?;
                let allocated = c.alloc()?;
                let modified = c.u64()?;
                let attributes = c.u32()?;
                let path = c.ltext(MAX_PATH_BYTES)?;
                // An absolute path below a drive root, valid names only.
                match parse_abs_dir(&path) {
                    Some((_, comps)) if !comps.is_empty() && !path.ends_with('\\') => {}
                    _ => return Err(ProtocolError::Malformed("bad hit path")),
                }
                hits.push(SearchHit {
                    kind,
                    size,
                    allocated,
                    modified,
                    attributes,
                    path,
                });
            }
            Response::SearchPage(SearchPage {
                hits,
                next,
                stale: flags & 1 != 0,
                unavailable,
                excluded_inside: flags & 2 != 0,
            })
        }
        RES_SUBTREE => {
            let flags = c.u8()?;
            if flags & !0b111 != 0 {
                return Err(ProtocolError::Malformed("bad flags"));
            }
            let root_frn = c.u64()?;
            let n = c.u32()?;
            if n > MAX_CHUNK_ENTRIES {
                return Err(ProtocolError::Malformed("too many entries"));
            }
            let mut entries = Vec::with_capacity(n as usize);
            for _ in 0..n {
                let frn = c.u64()?;
                let parent = c.u64()?;
                let kind = c.kind()?;
                let size = c.u64()?;
                let allocated = c.alloc()?;
                let name = c.ltext(crate::index::MAX_NAME_BYTES)?;
                if !valid_name(&name) {
                    return Err(ProtocolError::Malformed("bad entry name"));
                }
                entries.push(SubtreeEntry {
                    frn,
                    parent,
                    kind,
                    size,
                    allocated,
                    name,
                });
            }
            Response::SubtreeChunk(SubtreeChunk {
                info: SubtreeInfo {
                    root_frn,
                    stale: flags & 1 != 0,
                    excluded_inside: flags & 2 != 0,
                },
                done: flags & 4 != 0,
                entries,
            })
        }
        RES_FOLDER_SIZES => {
            let stale = c.bool()?;
            let n = c.u8()? as usize;
            if n > MAX_FOLDER_NAMES {
                return Err(ProtocolError::Malformed("too many sizes"));
            }
            let mut sizes = Vec::with_capacity(n);
            for _ in 0..n {
                let status = c.u8()?;
                let bytes = c.u64()?;
                let files = c.u64()?;
                sizes.push(match status {
                    1 => FolderSize::Ok { bytes, files },
                    2 if bytes == 0 && files == 0 => FolderSize::NotIndexed,
                    _ => return Err(ProtocolError::Malformed("bad folder size")),
                });
            }
            Response::FolderSizes(FolderSizes { stale, sizes })
        }
        RES_CHANGES => {
            let journal_id = c.u64()?;
            let usn = usn_from(c.u64()?)?;
            let root_frn = c.u64()?;
            let status = match (c.u8()?, c.u8()?) {
                (1, 0) => ChangeStatus::Complete,
                (2, 0) => ChangeStatus::MarkOnly,
                (3, r) => ChangeStatus::Rescan(
                    rescan_from(r).ok_or(ProtocolError::Malformed("bad rescan reason"))?,
                ),
                _ => return Err(ProtocolError::Malformed("bad change status")),
            };
            let n = c.u32()? as usize;
            if n > MAX_CHANGED_DIRS || (n > 0 && status != ChangeStatus::Complete) {
                return Err(ProtocolError::Malformed("bad changed directory count"));
            }
            let mut dirs = Vec::with_capacity(n);
            for _ in 0..n {
                let recursive = c.bool()?;
                let rel = c.ltext(MAX_PATH_BYTES)?;
                if !valid_rel(&rel) {
                    return Err(ProtocolError::Malformed("bad changed directory"));
                }
                dirs.push(ChangedDir { rel, recursive });
            }
            Response::Changes(Changes {
                mark: JournalMark { journal_id, usn },
                root_frn,
                status,
                dirs,
            })
        }
        RES_ERROR => {
            let code =
                ErrorCode::from_u16(c.u16()?).ok_or(ProtocolError::Malformed("bad error code"))?;
            let message = c.text()?;
            Response::Error { code, message }
        }
        other => return Err(ProtocolError::UnknownKind(other)),
    };
    c.finish()?;
    Ok(res)
}

/// Read one frame body, refusing anything above `max` BEFORE allocating.
/// `Eof` only when the peer closed cleanly at a frame boundary.
pub fn read_frame(r: &mut impl Read, max: u32) -> Result<Vec<u8>, ProtocolError> {
    let mut len = [0u8; 4];
    let mut got = 0usize;
    while got < 4 {
        match r.read(&mut len[got..]) {
            Ok(0) if got == 0 => return Err(ProtocolError::Eof),
            Ok(0) => return Err(ProtocolError::Malformed("truncated length")),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(ProtocolError::Io(e.to_string())),
        }
    }
    let n = u32::from_le_bytes(len);
    if n == 0 {
        return Err(ProtocolError::EmptyFrame);
    }
    if n > max {
        return Err(ProtocolError::FrameTooLarge(n));
    }
    let mut body = vec![0u8; n as usize];
    let mut got = 0usize;
    while got < body.len() {
        match r.read(&mut body[got..]) {
            Ok(0) => return Err(ProtocolError::Malformed("truncated frame")),
            Ok(k) => got += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(ProtocolError::Io(e.to_string())),
        }
    }
    Ok(body)
}

pub fn read_request(r: &mut impl Read) -> Result<Request, ProtocolError> {
    decode_request(&read_frame(r, MAX_REQUEST_FRAME)?)
}

pub fn read_response(r: &mut impl Read) -> Result<Response, ProtocolError> {
    decode_response(&read_frame(r, MAX_RESPONSE_FRAME)?)
}

pub fn write_response(w: &mut impl Write, res: &Response) -> io::Result<()> {
    w.write_all(&encode_response(res))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_requests() -> Vec<Request> {
        vec![
            Request::Hello,
            Request::Ping { token: 0xDEAD_BEEF },
            Request::Status,
            Request::Version,
            Request::GetMode,
            Request::SetMode(Mode::Shared),
            Request::SetMode(Mode::HighPerformance),
            Request::IndexStatus,
            Request::Search(sample_query()),
            Request::Subtree {
                path: r"C:\Data".into(),
            },
            Request::FolderSizes {
                parent: r"C:\Data".into(),
                names: vec!["a".into(), "Projekt é".into()],
            },
            Request::Changes {
                path: r"D:\".into(),
                since: None,
            },
            Request::Changes {
                path: r"C:\Sync\left".into(),
                since: Some(JournalMark {
                    journal_id: 0x01D9_0000_0000_0001,
                    usn: 123_456_789,
                }),
            },
        ]
    }

    fn sample_query() -> SearchQuery {
        SearchQuery {
            pattern: "*.txt".into(),
            match_kind: MatchKind::Glob,
            scope: r"C:\Users\Public".into(),
            extensions: vec!["txt".into(), "md".into()],
            kinds: KindFilter::Files,
            limit: 200,
            cursor: QueryCursor {
                letter: b'C',
                slot: 77,
            },
        }
    }

    fn sample_page() -> SearchPage {
        SearchPage {
            hits: vec![
                SearchHit {
                    kind: Kind::File,
                    size: 5,
                    allocated: None,
                    modified: 1,
                    attributes: 0x20,
                    path: r"C:\a\b.txt".into(),
                },
                SearchHit {
                    kind: Kind::Dir,
                    size: 0,
                    allocated: Some(4096),
                    modified: 2,
                    attributes: 0x10,
                    path: r"D:\é".into(),
                },
            ],
            next: Some(QueryCursor {
                letter: b'D',
                slot: 9,
            }),
            stale: true,
            unavailable: 1 << 4,
            excluded_inside: true,
        }
    }

    fn sample_chunk() -> SubtreeChunk {
        SubtreeChunk {
            info: SubtreeInfo {
                root_frn: 30 | (1 << 48),
                stale: false,
                excluded_inside: true,
            },
            done: true,
            entries: vec![SubtreeEntry {
                frn: 31 | (1 << 48),
                parent: 30 | (1 << 48),
                kind: Kind::File,
                size: 10,
                allocated: Some(4096),
                name: "a.txt".into(),
            }],
        }
    }

    fn all_responses() -> Vec<Response> {
        vec![
            Response::HelloOk(HelloInfo {
                proto: PROTO_VERSION,
                can_lower_privacy: true,
            }),
            Response::Pong { token: 7 },
            Response::Status(StatusInfo {
                state: ServiceState::Paused,
                mode: Mode::HighPerformance,
                index: IndexState::Building,
                version: "0.1.0".into(),
                uptime_secs: 12345,
                console: true,
                degraded: true,
            }),
            Response::Version {
                version: "0.1.0".into(),
                proto: PROTO_VERSION,
            },
            Response::Mode(Mode::Shared),
            Response::ModeSet {
                mode: Mode::Shared,
                changed: false,
            },
            Response::Error {
                code: ErrorCode::NeedsElevation,
                message: "needs an elevated administrator".into(),
            },
            Response::Error {
                code: ErrorCode::NotIndexed,
                message: "not indexed".into(),
            },
            Response::IndexStatus(IndexStatusInfo {
                paused: false,
                degraded: true,
                memory_bytes: 0,
                memory_limit: 1 << 30,
                volumes: vec![],
            }),
            Response::IndexStatus(sample_index_status(3)),
            Response::SearchPage(sample_page()),
            Response::SearchPage(SearchPage::default()),
            Response::SubtreeChunk(sample_chunk()),
            Response::FolderSizes(FolderSizes {
                stale: true,
                sizes: vec![
                    FolderSize::Ok {
                        bytes: 1 << 40,
                        files: 12,
                    },
                    FolderSize::NotIndexed,
                ],
            }),
            Response::Changes(sample_changes()),
            Response::Changes(Changes {
                mark: JournalMark {
                    journal_id: 9,
                    usn: 0,
                },
                root_frn: 30 | (1 << 48),
                status: ChangeStatus::Rescan(RescanReason::ExcludedInside),
                dirs: vec![],
            }),
            Response::Changes(Changes {
                mark: JournalMark {
                    journal_id: 9,
                    usn: 77,
                },
                root_frn: 30 | (1 << 48),
                status: ChangeStatus::MarkOnly,
                dirs: vec![],
            }),
        ]
    }

    fn sample_changes() -> Changes {
        Changes {
            mark: JournalMark {
                journal_id: 0x01D9_0000_0000_0001,
                usn: 987_654_321,
            },
            root_frn: 30 | (1 << 48),
            status: ChangeStatus::Complete,
            dirs: vec![
                ChangedDir {
                    rel: String::new(),
                    recursive: false,
                },
                ChangedDir {
                    rel: r"a\b é".into(),
                    recursive: true,
                },
            ],
        }
    }

    #[test]
    fn folder_sizes_and_changes_frames_are_strict() {
        // Requests: names are single valid names (no `..`, separators, ADS
        // colon, controls), 1..=200 of them; the parent an absolute dir.
        for bad in ["..", ".", "a\\b", "a/b", "a:b", "a\u{1}", ""] {
            let f = encode_request(&Request::FolderSizes {
                parent: r"C:\".into(),
                names: vec![bad.into()],
            });
            assert!(read_request(&mut &f[..]).is_err(), "{bad:?}");
        }
        for parent in ["rel", r"C:\a\..\b", r"\\srv\share", r"C:\a:b"] {
            let f = encode_request(&Request::FolderSizes {
                parent: parent.into(),
                names: vec!["x".into()],
            });
            assert!(read_request(&mut &f[..]).is_err(), "{parent:?}");
        }
        let f = encode_request(&Request::FolderSizes {
            parent: r"C:\".into(),
            names: vec![],
        });
        assert!(read_request(&mut &f[..]).is_err(), "no names");
        let mut b = encode_request(&Request::FolderSizes {
            parent: r"C:\".into(),
            names: vec!["x".into()],
        })[4..]
            .to_vec();
        b[1 + 2 + 3] = 201; // count byte after parent `C:\`
        assert!(decode_request(&b).is_err(), "201 names");
        // The largest folderSizes the client sends fits the frame cap: a
        // 1024-byte parent and names that fill the rest.
        let parent = format!(r"C:\{}", vec!["p".repeat(200); 5].join("\\"));
        let names: Vec<String> = (0..200).map(|i| format!("{i:0>60}")).collect();
        let f = encode_request(&Request::FolderSizes {
            parent: parent.clone(),
            names: names.clone(),
        });
        assert!(f.len() as u32 <= MAX_REQUEST_FRAME + 4, "{}", f.len());
        assert_eq!(
            read_request(&mut &f[..]).unwrap(),
            Request::FolderSizes { parent, names }
        );
        // Changes request: bad path, bad flag, negative USN, trailing bytes.
        let f = encode_request(&Request::Changes {
            path: r"C:\a\..".into(),
            since: None,
        });
        assert!(read_request(&mut &f[..]).is_err());
        let ok = encode_request(&Request::Changes {
            path: r"C:\".into(),
            since: None,
        })[4..]
            .to_vec();
        let mut b = ok.clone();
        *b.last_mut().unwrap() = 2;
        assert!(decode_request(&b).is_err(), "mark flag 2");
        let mut b = ok.clone();
        b.push(0);
        assert!(decode_request(&b).is_err(), "trailing byte");
        let mut b = encode_request(&Request::Changes {
            path: r"C:\".into(),
            since: Some(JournalMark {
                journal_id: 1,
                usn: 5,
            }),
        })[4..]
            .to_vec();
        let n = b.len();
        b[n - 1] = 0x80; // usn high bit: negative
        assert!(decode_request(&b).is_err(), "negative usn");
        // Responses.
        let fs = |status: u8, bytes: u64| {
            let mut b = vec![RES_FOLDER_SIZES, 0, 1, status];
            b.extend_from_slice(&bytes.to_le_bytes());
            b.extend_from_slice(&0u64.to_le_bytes());
            b
        };
        assert!(decode_response(&fs(1, 5)).is_ok());
        assert!(decode_response(&fs(2, 0)).is_ok());
        assert!(
            decode_response(&fs(2, 5)).is_err(),
            "not indexed with bytes"
        );
        assert!(decode_response(&fs(3, 0)).is_err(), "bad status");
        let mut b = fs(1, 5);
        b[1] = 2;
        assert!(decode_response(&b).is_err(), "bad stale bool");
        let mut b = fs(1, 5);
        b[2] = 201;
        assert!(decode_response(&b).is_err(), "too many sizes");
        let body = encode_response(&Response::Changes(sample_changes()))[4..].to_vec();
        // kind 0, jid 1..9, usn 9..17, root 17..25, status 25, reason 26,
        // count 27..31.
        for (at, val) in [(25usize, 4u8), (25, 0), (26, 1), (16, 0x80)] {
            let mut b = body.clone();
            b[at] = val;
            assert!(decode_response(&b).is_err(), "offset {at} = {val}");
        }
        // Directories only in a complete answer.
        let mut b = body.clone();
        b[25] = 2;
        assert!(decode_response(&b).is_err(), "mark-only with dirs");
        let mut c = sample_changes();
        c.dirs[1].rel = r"a\..\b".into();
        assert!(decode_response(&encode_response(&Response::Changes(c))[4..]).is_err());
        let mut c = sample_changes();
        c.dirs[1].rel = r"\a".into();
        assert!(decode_response(&encode_response(&Response::Changes(c))[4..]).is_err());
        // The encoder never sends directories with a rescan.
        let mut c = sample_changes();
        c.status = ChangeStatus::Rescan(RescanReason::TooMany);
        match decode_response(&encode_response(&Response::Changes(c))[4..]).unwrap() {
            Response::Changes(c) => assert!(c.dirs.is_empty()),
            other => panic!("{other:?}"),
        }
        let mut b = body.clone();
        b[27..31].copy_from_slice(&((MAX_CHANGED_DIRS + 1) as u32).to_le_bytes());
        assert!(decode_response(&b).is_err(), "count over the cap");
    }

    #[test]
    fn search_and_subtree_frames_are_strict() {
        // Requests: an invalid query never decodes.
        for bad in [
            SearchQuery {
                pattern: String::new(),
                ..sample_query()
            },
            SearchQuery {
                scope: "rel".into(),
                ..sample_query()
            },
            SearchQuery {
                limit: 0,
                ..sample_query()
            },
            SearchQuery {
                extensions: vec!["TXT".into()],
                ..sample_query()
            },
        ] {
            let f = encode_request(&Request::Search(bad));
            assert!(read_request(&mut &f[..]).is_err());
        }
        let f = encode_request(&Request::Subtree {
            path: r"C:\a\..\b".into(),
        });
        assert!(read_request(&mut &f[..]).is_err());
        // The largest legal search request fits the frame.
        let big = SearchQuery {
            pattern: "p".repeat(MAX_PATTERN),
            // 5 components of 200 + separators = 1008 bytes.
            scope: format!(r"C:\{}", vec!["s".repeat(200); 5].join("\\")),
            extensions: vec!["e".repeat(MAX_EXTENSION); MAX_EXTENSIONS],
            ..sample_query()
        };
        let f = encode_request(&Request::Search(big.clone()));
        assert!(f.len() as u32 <= MAX_REQUEST_FRAME + 4, "{}", f.len());
        assert_eq!(read_request(&mut &f[..]).unwrap(), Request::Search(big));
        // Responses: bad hit path / name, bad flags, bad cursor.
        let mut p = sample_page();
        p.hits[0].path = "relative".into();
        assert!(decode_response(&encode_response(&Response::SearchPage(p))[4..]).is_err());
        let mut p = sample_page();
        p.hits[0].path = r"C:\".into();
        assert!(decode_response(&encode_response(&Response::SearchPage(p))[4..]).is_err());
        let mut c = sample_chunk();
        c.entries[0].name = "a\\b".into();
        assert!(decode_response(&encode_response(&Response::SubtreeChunk(c))[4..]).is_err());
        let mut b = encode_response(&Response::SearchPage(sample_page()))[4..].to_vec();
        b[1] |= 0b1000;
        assert!(decode_response(&b).is_err());
        let mut b = encode_response(&Response::SearchPage(SearchPage::default()))[4..].to_vec();
        b[6] = b'C'; // a cursor without the has-next flag
        assert!(decode_response(&b).is_err());
    }

    fn sample_index_status(n: usize) -> IndexStatusInfo {
        let states = [
            VolumeState::Building,
            VolumeState::Ready,
            VolumeState::Error,
        ];
        IndexStatusInfo {
            paused: true,
            degraded: false,
            memory_bytes: 123_456_789,
            memory_limit: 1 << 30,
            volumes: (0..n)
                .map(|i| VolumeStatus {
                    letter: b'C' + (i % 24) as u8,
                    state: states[i % 3],
                    error: if i % 3 == 2 {
                        VolumeErrorCode::AccessDenied
                    } else {
                        VolumeErrorCode::None
                    },
                    entries: 4_500_000 + i as u64,
                    memory_bytes: 300 << 20,
                    journal_id: 0x01D9_0000_0000_0000 + i as u64,
                    next_usn: 98_765_432_100 + i as i64,
                    live: i % 2 == 0,
                    progress_permille: [500, 1000, 0][i % 3],
                })
                .collect(),
        }
    }

    #[test]
    fn index_status_fits_the_frame_and_is_strict() {
        let full = Response::IndexStatus(sample_index_status(MAX_VOLUMES));
        let f = encode_response(&full);
        assert!(f.len() as u32 <= MAX_RESPONSE_FRAME + 4);
        assert_eq!(read_response(&mut &f[..]).unwrap(), full);
        // The encoder caps the list at 26.
        let over = Response::IndexStatus(sample_index_status(MAX_VOLUMES + 5));
        match read_response(&mut &encode_response(&over)[..]).unwrap() {
            Response::IndexStatus(s) => assert_eq!(s.volumes.len(), MAX_VOLUMES),
            other => panic!("{other:?}"),
        }
        let one = encode_response(&Response::IndexStatus(sample_index_status(1)))[4..].to_vec();
        // Body offsets: kind 0, paused 1, memory 2..18, count 18, letter 19,
        // state 20, error 21, 4 x u64 22..54, live 54, progress 55..57.
        for (at, bad) in [
            (1usize, 4u8),
            (18, 27),
            (19, b'c'),
            (19, b'1'),
            (20, 0),
            (20, 4),
            (21, 10),
            (54, 2),
        ] {
            let mut b = one.clone();
            b[at] = bad;
            assert!(decode_response(&b).is_err(), "offset {at} = {bad}");
        }
        let mut b = one.clone();
        b[55..57].copy_from_slice(&1001u16.to_le_bytes());
        assert!(decode_response(&b).is_err(), "progress > 1000");
        assert_eq!(one.len(), 57);
    }

    #[test]
    fn aggregate_index_state() {
        let mk = |s: VolumeState| {
            let mut v = sample_index_status(1).volumes.remove(0);
            v.state = s;
            v
        };
        assert_eq!(IndexState::aggregate(&[]), IndexState::None);
        assert_eq!(
            IndexState::aggregate(&[mk(VolumeState::Ready), mk(VolumeState::Building)]),
            IndexState::Building
        );
        assert_eq!(
            IndexState::aggregate(&[mk(VolumeState::Ready), mk(VolumeState::Error)]),
            IndexState::Ready
        );
        assert_eq!(
            IndexState::aggregate(&[mk(VolumeState::Error)]),
            IndexState::Error
        );
    }

    #[test]
    fn requests_round_trip() {
        for r in all_requests() {
            let f = encode_request(&r);
            assert_eq!(read_request(&mut &f[..]).unwrap(), r);
        }
    }

    #[test]
    fn responses_round_trip() {
        for r in all_responses() {
            let f = encode_response(&r);
            assert_eq!(read_response(&mut &f[..]).unwrap(), r);
        }
    }

    #[test]
    fn mode_strings_are_exact() {
        for m in Mode::ALL {
            assert_eq!(Mode::parse(m.as_str()), Some(m));
        }
        for bad in [
            "",
            "Shared",
            "SHARED",
            "high-performance",
            " shared",
            "shared\n",
        ] {
            assert_eq!(Mode::parse(bad), None, "{bad:?}");
        }
        assert!(Mode::Shared.lowers_privacy_to(Mode::HighPerformance));
        assert!(!Mode::HighPerformance.lowers_privacy_to(Mode::Shared));
        assert!(!Mode::Shared.lowers_privacy_to(Mode::Shared));
    }

    #[test]
    fn oversized_frames_are_refused_before_allocation() {
        let mut f = Vec::new();
        f.extend_from_slice(&(MAX_REQUEST_FRAME + 1).to_le_bytes());
        assert_eq!(
            read_request(&mut &f[..]),
            Err(ProtocolError::FrameTooLarge(MAX_REQUEST_FRAME + 1))
        );
        let mut f = Vec::new();
        f.extend_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(
            read_response(&mut &f[..]),
            Err(ProtocolError::FrameTooLarge(u32::MAX))
        );
    }

    #[test]
    fn empty_truncated_and_eof() {
        assert_eq!(read_request(&mut &[][..]), Err(ProtocolError::Eof));
        assert_eq!(
            read_request(&mut &[0u8, 0, 0, 0][..]),
            Err(ProtocolError::EmptyFrame)
        );
        assert!(matches!(
            read_request(&mut &[1u8, 0][..]),
            Err(ProtocolError::Malformed(_))
        ));
        // Length says 9, only 3 bytes follow.
        assert!(matches!(
            read_request(&mut &[9u8, 0, 0, 0, REQ_PING, 1, 2][..]),
            Err(ProtocolError::Malformed(_))
        ));
    }

    #[test]
    fn strict_request_decoding() {
        // Trailing byte after a payload-free request.
        assert_eq!(
            decode_request(&[REQ_STATUS, 0]),
            Err(ProtocolError::Malformed("trailing bytes"))
        );
        // Unknown kind, response kind sent as request.
        assert_eq!(
            decode_request(&[0x42]),
            Err(ProtocolError::UnknownKind(0x42))
        );
        assert_eq!(
            decode_request(&[RES_PONG, 0, 0, 0, 0, 0, 0, 0, 0]),
            Err(ProtocolError::UnknownKind(RES_PONG))
        );
        // Bad mode byte.
        for b in [0u8, 3, 0xFF] {
            assert!(decode_request(&[REQ_SET_MODE, b]).is_err(), "{b}");
        }
        // Bad magic / version.
        let mut h = vec![REQ_HELLO];
        h.extend_from_slice(b"XXXX");
        h.extend_from_slice(&PROTO_VERSION.to_le_bytes());
        assert!(decode_request(&h).is_err());
        let mut h = vec![REQ_HELLO];
        h.extend_from_slice(&MAGIC);
        h.extend_from_slice(&(PROTO_VERSION + 1).to_le_bytes());
        assert!(decode_request(&h).is_err());
    }

    #[test]
    fn strict_response_decoding() {
        // Text longer than the cap.
        let mut b = vec![RES_VERSION, 1, 0];
        b.extend_from_slice(&((MAX_TEXT + 1) as u16).to_le_bytes());
        b.extend(std::iter::repeat_n(b'a', MAX_TEXT + 1));
        assert!(decode_response(&b).is_err());
        // Control character and invalid UTF-8 in text.
        for bad in [&b"a\x07"[..], &[0xC3, 0x28][..]] {
            let mut b = vec![RES_ERROR, 1, 0];
            b.extend_from_slice(&(bad.len() as u16).to_le_bytes());
            b.extend_from_slice(bad);
            assert!(decode_response(&b).is_err());
        }
        // Unknown error code, bad bool.
        assert!(decode_response(&[RES_ERROR, 99, 0, 0, 0]).is_err());
        assert!(decode_response(&[RES_MODE_SET, 2, 2]).is_err());
        // Request kind sent as response.
        assert_eq!(
            decode_response(&[REQ_STATUS]),
            Err(ProtocolError::UnknownKind(REQ_STATUS))
        );
    }

    #[test]
    fn encoder_sanitises_text() {
        let long = "é".repeat(MAX_TEXT); // 2 bytes per char
        let f = encode_response(&Response::Error {
            code: ErrorCode::Internal,
            message: format!("x\ny{long}"),
        });
        match read_response(&mut &f[..]).unwrap() {
            Response::Error { message, .. } => {
                assert!(message.len() <= MAX_TEXT);
                assert!(message.starts_with("x?y"));
            }
            other => panic!("{other:?}"),
        }
    }

    /// Deterministic fuzz: random and mutated bodies never panic, and
    /// anything accepted re-encodes to the same bytes.
    #[test]
    fn decoders_survive_garbage() {
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let mut seeds: Vec<Vec<u8>> = all_requests()
            .iter()
            .map(|r| encode_request(r)[4..].to_vec())
            .collect();
        seeds.extend(
            all_responses()
                .iter()
                .map(|r| encode_response(r)[4..].to_vec()),
        );
        for i in 0..20_000 {
            let mut body = if i % 2 == 0 {
                seeds[(next() as usize) % seeds.len()].clone()
            } else {
                let n = (next() % 48) as usize;
                (0..n).map(|_| next() as u8).collect()
            };
            if !body.is_empty() {
                for _ in 0..(next() % 3) {
                    let at = (next() as usize) % body.len();
                    body[at] = next() as u8;
                }
            }
            if let Ok(r) = decode_request(&body) {
                assert_eq!(encode_request(&r)[4..], body[..]);
            }
            if let Ok(r) = decode_response(&body) {
                assert_eq!(encode_response(&r)[4..], body[..]);
            }
        }
    }
}
