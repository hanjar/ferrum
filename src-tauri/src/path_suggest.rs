//! FER-456 address bar backend: subfolder suggestions and path resolution.
//!
//! Pure lexical path handling (no `canonicalize`, so no symlink/UNC round
//! trips). Suggestions list only directory names and supported archives,
//! match the last segment as a case-insensitive prefix, never follow links
//! and never open files (a cloud placeholder is therefore not hydrated; a
//! parent folder that is itself an offline placeholder is not enumerated).
//!
//! Hardening (security review of FER-456):
//! - Suggestions never touch an UNC host that is not already known (a host
//!   the user is in, or one passed in as known: Recent/Hotlist/panes, or one
//!   opened with Enter this session). Typing or pasting `\\host\` therefore
//!   cannot trigger an SMB/NTLM connection; only Enter (`resolve`) does.
//!   WebDAV forms (`host@SSL`, `host@80`, `DavWWWRoot`) are refused.
//! - At most [`MAX_INFLIGHT`] workers run at once per command; when full the
//!   call returns `timed_out` immediately. A folder/host that timed out is
//!   remembered as slow for [`SLOW_TTL`].
//! - FER-461: suggestion workers are counted per class. Local folders have
//!   their own [`MAX_INFLIGHT`]; network folders (UNC or a mapped network
//!   drive) share another [`MAX_INFLIGHT`] with at most [`MAX_PER_HOST`] per
//!   host. A slow SMB host can therefore never starve local suggestions, nor
//!   take every network slot.
//! - The FINAL path (after `%VAR%`, base join and normalisation) and `base`
//!   pass the same device-path / control-character checks as the raw input.
//! - Only an allowlist of path variables is expanded.

use crate::domain::{ErrorCode, FsError, validate_path};
use crate::fs_service;
use crate::media_protocol::is_device_path;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant};

pub const MAX_LIMIT: usize = 50;
pub const TIMEOUT: Duration = Duration::from_millis(300);
pub const MAX_INFLIGHT: usize = 4;
/// FER-461: suggestion workers one network host may hold at once.
pub const MAX_PER_HOST: usize = 2;
const SLOW_TTL: Duration = Duration::from_secs(30);
const ARCHIVE_EXTS: [&str; 3] = [".zip", ".rar", ".7z"];
/// Win32 path length limit.
const MAX_INPUT_LEN: usize = 32_767;
/// Hard cap on directory entries inspected per call (huge folders).
const MAX_SCANNED: usize = 50_000;
/// RECALL_ON_OPEN | RECALL_ON_DATA_ACCESS: enumerating would hydrate.
const ATTR_OFFLINE_MASK: u32 = 0x0004_0000 | 0x0040_0000;
const ATTR_REPARSE_POINT: u32 = 0x0400;
/// Variables that hold paths; anything else stays verbatim (no value leaks).
const ENV_ALLOWLIST: [&str; 19] = [
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    "APPDATA",
    "LOCALAPPDATA",
    "TEMP",
    "TMP",
    "PUBLIC",
    "PROGRAMDATA",
    "ProgramFiles",
    "ProgramFiles(x86)",
    "ProgramW6432",
    "CommonProgramFiles",
    "SystemRoot",
    "SystemDrive",
    "windir",
    "OneDrive",
    "OneDriveConsumer",
    "OneDriveCommercial",
];

static SUGGEST_INFLIGHT: AtomicUsize = AtomicUsize::new(0);
/// FER-461: network suggestion workers per host key (see [`Permit::network`]).
static NET_INFLIGHT: OnceLock<Mutex<HashMap<String, usize>>> = OnceLock::new();
static RESOLVE_INFLIGHT: AtomicUsize = AtomicUsize::new(0);
static SESSION_HOSTS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
static SLOW: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SuggestKind {
    Drive,
    Dir,
    Archive,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Suggestion {
    pub name: String,
    pub path: String,
    pub kind: SuggestKind,
    /// Symlink / junction: listed, but never resolved here.
    pub link: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SuggestResult {
    pub items: Vec<Suggestion>,
    /// More matches existed than `limit`.
    pub truncated: bool,
    /// The deadline hit, the worker cap was full or the folder is known slow;
    /// `items` is whatever was gathered in time.
    pub timed_out: bool,
    /// The folder is on an UNC host that is not known yet: no I/O was done,
    /// the UI says "press Enter to connect to \\host".
    pub needs_confirm: bool,
    pub host: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ResolveKind {
    Dir,
    Archive,
    File,
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Resolved {
    pub path: String,
    pub kind: ResolveKind,
}

fn invalid(msg: &str) -> FsError {
    FsError::new(ErrorCode::InvalidPath, msg)
}

pub fn is_archive_name(name: &str) -> bool {
    let l = name.to_lowercase();
    ARCHIVE_EXTS.iter().any(|e| l.ends_with(e))
}

/// Expand `%NAME%` references for allowlisted path variables; everything
/// else (unknown or not allowlisted) stays verbatim.
pub fn expand_env(input: &str) -> String {
    expand_env_with(input, |k| std::env::var(k).ok())
}

fn expand_env_with(input: &str, get: impl Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(i) = rest.find('%') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        match after.find('%') {
            Some(j)
                if j > 0
                    && after[..j]
                        .chars()
                        .all(|c| c.is_alphanumeric() || matches!(c, '_' | '(' | ')')) =>
            {
                let name = &after[..j];
                let canonical = ENV_ALLOWLIST.iter().find(|a| a.eq_ignore_ascii_case(name));
                match canonical.and_then(|c| get(c)) {
                    Some(v) => out.push_str(&v),
                    None => out.push_str(&rest[i..i + j + 2]),
                }
                rest = &after[j + 1..];
            }
            _ => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

fn is_absolute(p: &str) -> bool {
    let b = p.as_bytes();
    (b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':')
        || p.starts_with("\\\\")
        || p.starts_with("//")
        || p.starts_with('/')
}

/// Join `input` onto `base` when relative, then resolve `.` and `..`
/// lexically. Separators are normalised to `\` for drive/UNC paths.
pub fn normalize(input: &str, base: &str) -> String {
    let input = input.trim();
    let joined = if is_absolute(input) {
        input.to_string()
    } else if input.starts_with('\\') {
        // "\foo": root of the base's drive.
        let drive: String = base.chars().take(2).collect();
        format!("{drive}{input}")
    } else if base.is_empty() {
        input.to_string()
    } else {
        format!("{}\\{}", base.trim_end_matches(['\\', '/']), input)
    };
    let unc = joined.starts_with("\\\\") || joined.starts_with("//");
    let posix = !unc && joined.starts_with('/');
    let trailing = joined.ends_with(['\\', '/']);
    let mut parts: Vec<&str> = Vec::new();
    let mut segs = joined.split(['\\', '/']).filter(|s| !s.is_empty());
    let mut prefix = String::new();
    if unc {
        // \\server\share is the root.
        let server = segs.next().unwrap_or("");
        prefix = match segs.next() {
            Some(s) => format!("\\\\{server}\\{s}"),
            None => format!("\\\\{server}"),
        };
    } else if !posix
        && let Some(first) = segs.clone().next()
        && first.len() == 2
        && first.as_bytes()[1] == b':'
    {
        prefix = first.to_string();
        segs.next();
    }
    for s in segs {
        match s {
            "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    let sep = if posix { "/" } else { "\\" };
    let mut out = String::new();
    if posix {
        out.push('/');
    } else {
        out.push_str(&prefix);
    }
    if !parts.is_empty() {
        if !out.is_empty() && !out.ends_with(sep) {
            out.push_str(sep);
        }
        out.push_str(&parts.join(sep));
    }
    if (trailing || parts.is_empty()) && !out.ends_with(sep) && !out.is_empty() {
        out.push_str(sep);
    }
    out
}

fn attrs_of(meta: &fs::Metadata) -> u32 {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        meta.file_attributes()
    }
    #[cfg(not(windows))]
    {
        let _ = meta;
        0
    }
}

/// Length, control-character and device-namespace checks. Applied to the
/// raw input, to `base` AND to the final normalised path.
fn vet_text(s: &str) -> Result<(), FsError> {
    if s.len() > MAX_INPUT_LEN {
        return Err(invalid("Path is too long"));
    }
    if s.chars().any(char::is_control) {
        return Err(invalid("Path contains control characters"));
    }
    if is_device_path(s) {
        return Err(invalid("Device paths are not supported"));
    }
    Ok(())
}

/// Lowercased host of an UNC path (`\\host\share` or `//host/share`).
fn unc_host(path: &str) -> Option<String> {
    let rest = path
        .strip_prefix("\\\\")
        .or_else(|| path.strip_prefix("//"))?;
    let host = rest.split(['\\', '/']).next().unwrap_or("");
    Some(host.to_lowercase())
}

/// WebDAV spellings of an UNC path: `host@SSL`, `host@80`, `DavWWWRoot`.
fn is_webdav_form(path: &str) -> bool {
    let Some(rest) = path
        .strip_prefix("\\\\")
        .or_else(|| path.strip_prefix("//"))
    else {
        return false;
    };
    let mut segs = rest.split(['\\', '/']);
    segs.next().is_some_and(|h| h.contains('@'))
        || segs.any(|s| s.eq_ignore_ascii_case("DavWWWRoot"))
}

fn session_hosts() -> &'static Mutex<HashSet<String>> {
    SESSION_HOSTS.get_or_init(Default::default)
}

/// Remember an UNC host the user explicitly connected to (Enter).
fn note_host(path: &str) {
    if is_webdav_form(path) {
        return;
    }
    if let Some(h) = unc_host(path)
        && !h.is_empty()
        && let Ok(mut set) = session_hosts().lock()
    {
        set.insert(h);
    }
}

fn is_known_host(host: &str, base: &str, known: &[String]) -> bool {
    if unc_host(base).as_deref() == Some(host) {
        return true; // the pane is already on this host
    }
    if known.iter().any(|k| {
        let kh = unc_host(k).unwrap_or_else(|| k.to_lowercase());
        kh == host
    }) {
        return true;
    }
    session_hosts()
        .lock()
        .map(|s| s.contains(host))
        .unwrap_or(false)
}

fn slow_map() -> &'static Mutex<HashMap<String, Instant>> {
    SLOW.get_or_init(Default::default)
}

fn is_slow(key: &str) -> bool {
    let Ok(mut m) = slow_map().lock() else {
        return false;
    };
    match m.get(key) {
        Some(t) if t.elapsed() < SLOW_TTL => true,
        Some(_) => {
            m.remove(key);
            false
        }
        None => false,
    }
}

fn mark_slow(key: &str) {
    if let Ok(mut m) = slow_map().lock() {
        if m.len() > 256 {
            m.retain(|_, t| t.elapsed() < SLOW_TTL);
        }
        m.insert(key.to_string(), Instant::now());
    }
}

/// What a suggest call still has to do after the I/O-free checks.
#[derive(Debug)]
pub enum Plan {
    Done(SuggestResult),
    Scan {
        folder: String,
        prefix: String,
        /// Slow-cache key: the UNC host, else the lowercased folder.
        key: String,
        /// FER-461: worker class. `Some(host key)` for a network folder (UNC
        /// host, or `drive:z` for a mapped network drive), `None` for local.
        network: Option<String>,
    },
}

/// FER-461: true when `X:\...` is on a mapped network drive. `GetDriveTypeW`
/// answers from the local drive table and does not contact the server.
fn is_network_drive(folder: &str) -> bool {
    let b = folder.as_bytes();
    if b.len() < 2 || !b[0].is_ascii_alphabetic() || b[1] != b':' {
        return false;
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Storage::FileSystem::GetDriveTypeW;
        let root: Vec<u16> = format!("{}:\\", b[0] as char)
            .encode_utf16()
            .chain(Some(0))
            .collect();
        // SAFETY: NUL-terminated root path.
        unsafe { GetDriveTypeW(root.as_ptr()) == crate::path_guard::DRIVE_REMOTE }
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// All checks that need no filesystem access (except the local drive list).
pub fn plan_suggest(
    input: &str,
    base: &str,
    limit: usize,
    known_hosts: &[String],
) -> Result<Plan, FsError> {
    let limit = limit.clamp(1, MAX_LIMIT);
    vet_text(input)?;
    if !base.is_empty() {
        vet_text(base)?;
    }
    let expanded = expand_env(input);
    vet_text(&expanded)?;
    // Split into the folder part (kept verbatim for display) and prefix.
    let cut = expanded.rfind(['\\', '/']).map(|i| i + 1);
    let mut result = SuggestResult::default();

    // "", "C", "C:" -> drives.
    let is_drive_typing = cut.is_none()
        && (expanded.is_empty()
            || (expanded.len() <= 2
                && expanded.as_bytes()[0].is_ascii_alphabetic()
                && (expanded.len() == 1 || expanded.as_bytes()[1] == b':')));
    if is_drive_typing {
        let p = expanded.to_lowercase();
        for d in fs_service::list_drives() {
            if d.path.to_lowercase().starts_with(&p) {
                result.items.push(Suggestion {
                    name: d.path.clone(),
                    path: d.path,
                    kind: SuggestKind::Drive,
                    link: false,
                });
            }
        }
        result.items.truncate(limit);
        return Ok(Plan::Done(result));
    }

    let (folder_raw, prefix) = match cut {
        Some(c) => (&expanded[..c], &expanded[c..]),
        None => ("", expanded.as_str()),
    };
    let folder = normalize(folder_raw, base);
    vet_text(&folder)?;
    if folder.is_empty() || folder == "\\\\" {
        return Ok(Plan::Done(result));
    }
    let mut key = folder.to_lowercase();
    let mut network =
        is_network_drive(&folder).then(|| format!("drive:{}", folder[..1].to_ascii_lowercase()));
    if let Some(host) = unc_host(&folder) {
        if host.is_empty() || host == "?" || host == "." {
            return Err(invalid("Device paths are not supported"));
        }
        if is_webdav_form(&folder) {
            return Err(invalid(
                "WebDAV locations are not suggested; press Enter to open",
            ));
        }
        if !is_known_host(&host, base, known_hosts) {
            result.needs_confirm = true;
            result.host = Some(host);
            return Ok(Plan::Done(result));
        }
        network = Some(host.clone());
        key = host;
    }
    Ok(Plan::Scan {
        folder,
        prefix: prefix.to_string(),
        key,
        network,
    })
}

/// The directory scan itself (the only part that touches the filesystem).
pub fn scan(folder: &str, prefix: &str, limit: usize, timeout: Duration) -> SuggestResult {
    let deadline = Instant::now() + timeout;
    let limit = limit.clamp(1, MAX_LIMIT);
    let mut result = SuggestResult::default();
    let dir_path = Path::new(folder);
    let Ok(meta) = fs::symlink_metadata(dir_path) else {
        return result;
    };
    if attrs_of(&meta) & ATTR_OFFLINE_MASK != 0 {
        return result;
    }
    // FER-461 L4: `read_dir` follows a junction/symlink, so the TARGET must not
    // be an unpopulated cloud placeholder either (reading its attributes does
    // not hydrate it; enumerating it would).
    if attrs_of(&meta) & ATTR_REPARSE_POINT != 0 {
        match fs::metadata(dir_path) {
            Ok(target) if attrs_of(&target) & ATTR_OFFLINE_MASK == 0 => {}
            _ => return result,
        }
    }
    let Ok(rd) = fs::read_dir(dir_path) else {
        return result;
    };
    let needle = prefix.to_lowercase();
    let mut matches: Vec<Suggestion> = Vec::new();
    let mut scanned = 0usize;
    for ent in rd {
        scanned += 1;
        if scanned > MAX_SCANNED || (scanned.is_multiple_of(256) && Instant::now() >= deadline) {
            result.timed_out = true;
            break;
        }
        let Ok(ent) = ent else { continue };
        let name = ent.file_name().to_string_lossy().into_owned();
        if !name.to_lowercase().starts_with(&needle) {
            continue;
        }
        // DirEntry::metadata does not follow links and is served from the
        // directory enumeration on Windows (no per-file open).
        let Ok(m) = ent.metadata() else { continue };
        let link = m.file_type().is_symlink();
        let kind = if m.is_dir() {
            SuggestKind::Dir
        } else if m.is_file() && is_archive_name(&name) {
            SuggestKind::Archive
        } else {
            continue;
        };
        let sep = if folder.starts_with('/') { "/" } else { "\\" };
        let full = if folder.ends_with(sep) {
            format!("{folder}{name}")
        } else {
            format!("{folder}{sep}{name}")
        };
        matches.push(Suggestion {
            name,
            path: full,
            kind,
            link,
        });
    }
    matches.sort_by_key(|s| s.name.to_lowercase());
    if matches.len() > limit {
        result.truncated = true;
        matches.truncate(limit);
    }
    result.items = matches;
    result
}

/// Plan + scan on the calling thread (unit tests; the command uses
/// [`suggest_bounded`]).
pub fn suggest(
    input: &str,
    base: &str,
    limit: usize,
    timeout: Duration,
    known_hosts: &[String],
) -> Result<SuggestResult, FsError> {
    Ok(match plan_suggest(input, base, limit, known_hosts)? {
        Plan::Done(r) => r,
        Plan::Scan { folder, prefix, .. } => scan(&folder, &prefix, limit, timeout),
    })
}

/// The command's entry point: bounded worker count, per-call deadline and a
/// negative cache for folders/hosts that already timed out.
pub fn suggest_bounded(
    input: &str,
    base: &str,
    limit: usize,
    known_hosts: &[String],
) -> Result<SuggestResult, FsError> {
    let timed_out = || SuggestResult {
        timed_out: true,
        ..Default::default()
    };
    match plan_suggest(input, base, limit, known_hosts)? {
        Plan::Done(r) => Ok(r),
        Plan::Scan {
            folder,
            prefix,
            key,
            network,
        } => {
            if is_slow(&key) {
                return Ok(timed_out());
            }
            let permit = match &network {
                Some(host) => Permit::network(host),
                None => Permit::counted(&SUGGEST_INFLIGHT, MAX_INFLIGHT),
            };
            match run_with_permit(permit, TIMEOUT, move || {
                scan(&folder, &prefix, limit, TIMEOUT)
            }) {
                Ok(r) => Ok(r),
                Err(Bounded::Timeout) => {
                    mark_slow(&key);
                    Ok(timed_out())
                }
                Err(Bounded::Busy) => Ok(timed_out()),
            }
        }
    }
}

/// Resolve what the user typed to a navigable path. Paths inside a
/// supported archive (`a.zip\sub`) resolve as `Archive` without being
/// opened; the pane's normal archive route reports member errors. Pressing
/// Enter is an explicit act, so an UNC host is connected to here and then
/// remembered for suggestions.
pub fn resolve(input: &str, base: &str) -> Result<Resolved, FsError> {
    validate_path(input)?;
    vet_text(input)?;
    if !base.is_empty() {
        vet_text(base)?;
    }
    let expanded = expand_env(input);
    vet_text(&expanded)?;
    let path = normalize(&expanded, base);
    if path.is_empty() {
        return Err(invalid("Path must not be empty"));
    }
    vet_text(&path)?;
    if let Some(host) = unc_host(&path)
        && (host.is_empty() || host == "?" || host == ".")
    {
        return Err(invalid("Device paths are not supported"));
    }
    note_host(&path);
    let probe = if path.len() > 3 && path.ends_with(['\\', '/']) {
        path[..path.len() - 1].to_string()
    } else {
        path.clone()
    };
    match fs::metadata(&probe) {
        Ok(m) if m.is_dir() => {
            return Ok(Resolved {
                path,
                kind: ResolveKind::Dir,
            });
        }
        Ok(m) if is_archive_name(&probe) && m.is_file() => {
            return Ok(Resolved {
                path: probe,
                kind: ResolveKind::Archive,
            });
        }
        Ok(_) => {
            return Ok(Resolved {
                path: probe,
                kind: ResolveKind::File,
            });
        }
        Err(_) => {}
    }
    // Inside an archive? Find an existing archive file among the ancestors.
    let mut cur = probe.as_str();
    while let Some(i) = cur.rfind(['\\', '/']) {
        cur = &cur[..i];
        if cur.is_empty() {
            break;
        }
        if is_archive_name(cur) {
            if fs::metadata(cur).map(|m| m.is_file()).unwrap_or(false) {
                return Ok(Resolved {
                    path: probe,
                    kind: ResolveKind::Archive,
                });
            }
            break;
        }
    }
    Ok(Resolved {
        path,
        kind: ResolveKind::Missing,
    })
}

/// `resolve` on a bounded worker (separate cap from suggestions, so a hung
/// suggestion never blocks an explicit Enter).
pub fn resolve_bounded(input: &str, base: &str) -> Result<Resolved, FsError> {
    let (i, b) = (input.to_string(), base.to_string());
    match run_bounded(
        &RESOLVE_INFLIGHT,
        MAX_INFLIGHT,
        Duration::from_secs(3),
        move || resolve(&i, &b),
    ) {
        Ok(r) => r,
        Err(Bounded::Timeout) => Err(FsError::new(
            ErrorCode::Io,
            "The location did not answer in time",
        )),
        Err(Bounded::Busy) => Err(FsError::new(
            ErrorCode::Io,
            "Too many lookups are still waiting; try again in a moment",
        )),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bounded {
    /// The worker did not answer within the deadline (it keeps running).
    Timeout,
    /// `max` workers were already in flight; nothing was spawned.
    Busy,
}

/// One held worker slot; dropping it frees the slot. Moved into the worker
/// thread, so a worker that outlives its deadline keeps counting.
pub struct Permit(Option<Box<dyn FnOnce() + Send>>);

impl Drop for Permit {
    fn drop(&mut self) {
        if let Some(release) = self.0.take() {
            release();
        }
    }
}

fn net_inflight() -> &'static Mutex<HashMap<String, usize>> {
    NET_INFLIGHT.get_or_init(Default::default)
}

impl Permit {
    /// A slot of a plain counter capped at `max`.
    pub fn counted(counter: &'static AtomicUsize, max: usize) -> Option<Self> {
        if counter.fetch_add(1, Ordering::SeqCst) >= max {
            counter.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        Some(Self(Some(Box::new(move || {
            counter.fetch_sub(1, Ordering::SeqCst);
        }))))
    }

    /// FER-461: a network suggestion slot for `host`: at most
    /// [`MAX_PER_HOST`] for this host and [`MAX_INFLIGHT`] for all network
    /// hosts together. Local folders never use these slots.
    pub fn network(host: &str) -> Option<Self> {
        let mut map = net_inflight().lock().ok()?;
        let total: usize = map.values().sum();
        let mine = map.get(host).copied().unwrap_or(0);
        if total >= MAX_INFLIGHT || mine >= MAX_PER_HOST {
            return None;
        }
        map.insert(host.to_string(), mine + 1);
        let host = host.to_string();
        Some(Self(Some(Box::new(move || {
            if let Ok(mut map) = net_inflight().lock()
                && let Some(n) = map.get_mut(&host)
            {
                *n -= 1;
                if *n == 0 {
                    map.remove(&host);
                }
            }
        }))))
    }
}

/// Run `f` on its own thread and give up after `timeout` so a hung network
/// share can never hold the command. At most `max` such threads (counted in
/// `counter`, including ones that outlived their deadline) exist at once.
pub fn run_bounded<T: Send + 'static>(
    counter: &'static AtomicUsize,
    max: usize,
    timeout: Duration,
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, Bounded> {
    run_with_permit(Permit::counted(counter, max), timeout, f)
}

/// `run_bounded` with a slot the caller already acquired (`None` = no slot
/// free: refused at once, nothing spawned). The slot is held by the worker
/// thread until `f` returns, also past the deadline.
pub fn run_with_permit<T: Send + 'static>(
    permit: Option<Permit>,
    timeout: Duration,
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, Bounded> {
    let Some(permit) = permit else {
        return Err(Bounded::Busy);
    };
    let (tx, rx) = mpsc::channel();
    // On a spawn failure the closure (and the permit in it) is dropped, which
    // frees the slot.
    let spawned = std::thread::Builder::new()
        .name("path-suggest".into())
        .spawn(move || {
            let _permit = permit;
            let _ = tx.send(f());
        });
    if spawned.is_err() {
        return Err(Bounded::Busy);
    }
    rx.recv_timeout(timeout).map_err(|_| Bounded::Timeout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn names(r: &SuggestResult) -> Vec<&str> {
        r.items.iter().map(|s| s.name.as_str()).collect()
    }

    fn sg(input: &str, base: &str, limit: usize) -> Result<SuggestResult, FsError> {
        suggest(input, base, limit, TIMEOUT, &[])
    }

    #[test]
    fn expand_env_only_allowlisted_variables() {
        let get = |k: &str| match k {
            "USERPROFILE" => Some("C:\\u".to_string()),
            "ProgramFiles(x86)" => Some("C:\\pf86".to_string()),
            "SECRET_TOKEN" => Some("hunter2".to_string()),
            _ => None,
        };
        assert_eq!(expand_env_with("%USERPROFILE%\\a", get), "C:\\u\\a");
        assert_eq!(expand_env_with("%userprofile%\\a", get), "C:\\u\\a");
        assert_eq!(expand_env_with("%ProgramFiles(x86)%", get), "C:\\pf86");
        // Not allowlisted: verbatim, never expanded (no value leak).
        assert_eq!(expand_env_with("%SECRET_TOKEN%", get), "%SECRET_TOKEN%");
        assert_eq!(expand_env_with("%NOPE%\\a", |_| None), "%NOPE%\\a");
        assert_eq!(expand_env_with("50%", |_| None), "50%");
    }

    #[test]
    fn normalize_relative_dotdot_and_slashes() {
        assert_eq!(normalize("..", "C:\\a\\b"), "C:\\a");
        assert_eq!(normalize("c/d", "C:\\a"), "C:\\a\\c\\d");
        assert_eq!(normalize("C:/x/./y/../z", ""), "C:\\x\\z");
        assert_eq!(normalize("C:", ""), "C:\\");
        assert_eq!(normalize("..\\..\\..", "C:\\a"), "C:\\");
        assert_eq!(
            normalize("\\\\srv\\share\\a\\..\\b", ""),
            "\\\\srv\\share\\b"
        );
    }

    #[test]
    fn prefix_match_is_case_insensitive_dirs_and_archives_only() {
        let t = tempdir().unwrap();
        for d in ["Alpha", "alps", "beta"] {
            fs::create_dir(t.path().join(d)).unwrap();
        }
        fs::write(t.path().join("alpine.zip"), b"x").unwrap();
        fs::write(t.path().join("alpha.txt"), b"x").unwrap();
        let base = t.path().to_string_lossy().into_owned();
        let r = sg("AL", &base, 50).unwrap();
        assert_eq!(names(&r), ["Alpha", "alpine.zip", "alps"]);
        assert_eq!(r.items[1].kind, SuggestKind::Archive);
        let r = sg(&format!("{base}\\be"), "", 50).unwrap();
        assert_eq!(names(&r), ["beta"]);
    }

    #[test]
    fn limit_truncates_and_flags() {
        let t = tempdir().unwrap();
        for i in 0..10 {
            fs::create_dir(t.path().join(format!("d{i}"))).unwrap();
        }
        let base = format!("{}\\", t.path().to_string_lossy());
        let r = sg(&base, "", 3).unwrap();
        assert_eq!(r.items.len(), 3);
        assert!(r.truncated);
    }

    #[test]
    fn device_nul_control_and_long_paths_are_rejected() {
        assert!(sg("\\\\.\\PhysicalDrive0\\", "", 5).is_err());
        assert!(sg("\\\\?\\GLOBALROOT\\Device\\", "", 5).is_err());
        assert!(sg("a\0b", "", 5).is_err());
        assert!(sg("a\x01b", "", 5).is_err());
        assert!(sg(&"a".repeat(MAX_INPUT_LEN + 1), "", 5).is_err());
        assert!(resolve("\\\\.\\C:", "").is_err());
        assert!(resolve("a\x07b", "").is_err());
    }

    #[test]
    fn device_path_smuggled_through_base_is_refused() {
        // The base itself is a device path.
        assert!(sg("x\\", "\\\\.\\GLOBALROOT\\Device\\Shadow1", 5).is_err());
        assert!(resolve("x", "\\\\.\\GLOBALROOT\\Device\\Shadow1").is_err());
        // A harmless-looking base that JOINS into a device path.
        assert!(sg("x\\", "\\\\.", 5).is_err());
        assert!(resolve("PhysicalDrive0", "\\\\.").is_err());
        // Control characters in base.
        assert!(sg("x\\", "C:\\a\x02", 5).is_err());
    }

    #[test]
    fn unknown_unc_host_triggers_no_io_and_asks_for_confirmation() {
        // A host name that cannot exist: if this touched the network the
        // test would stall or fail; instead the plan is answered up front.
        let plan = plan_suggest("\\\\no-such-host-fer456\\share\\a", "C:\\", 50, &[]).unwrap();
        match plan {
            Plan::Done(r) => {
                assert!(r.needs_confirm);
                assert_eq!(r.host.as_deref(), Some("no-such-host-fer456"));
                assert!(r.items.is_empty());
            }
            Plan::Scan { .. } => panic!("an unknown UNC host must not be scanned"),
        }
        // Bare host form too.
        assert!(matches!(
            plan_suggest("//no-such-host-fer456/", "", 50, &[]).unwrap(),
            Plan::Done(r) if r.needs_confirm
        ));
    }

    #[test]
    fn known_unc_hosts_are_scanned_and_enter_makes_a_host_known() {
        let known = vec!["\\\\KnownHost\\share".to_string()];
        assert!(matches!(
            plan_suggest("\\\\knownhost\\share\\a", "", 50, &known).unwrap(),
            Plan::Scan { .. }
        ));
        // The pane being on a host makes it known.
        assert!(matches!(
            plan_suggest("\\\\panehost\\s\\a", "\\\\panehost\\s", 50, &[]).unwrap(),
            Plan::Scan { .. }
        ));
        // Enter (resolve) remembers the host for later suggestions.
        note_host("\\\\entered-host-fer456\\s");
        assert!(matches!(
            plan_suggest("\\\\entered-host-fer456\\s\\a", "", 50, &[]).unwrap(),
            Plan::Scan { .. }
        ));
    }

    #[test]
    fn webdav_forms_and_odd_hosts_are_refused_in_suggestions() {
        for p in [
            "\\\\host@SSL\\share\\a",
            "\\\\host@8080\\share\\a",
            "\\\\host\\DavWWWRoot\\a",
            "\\\\.\\x\\a",
            "\\\\?\\x\\a",
        ] {
            assert!(
                plan_suggest(p, "", 50, &["host".to_string()]).is_err(),
                "{p}"
            );
        }
    }

    #[test]
    fn missing_folder_gives_no_suggestions() {
        let r = sg("C:\\definitely\\not\\here\\x", "", 5).unwrap();
        assert!(r.items.is_empty());
    }

    #[test]
    fn resolve_kinds() {
        let t = tempdir().unwrap();
        let base = t.path().to_string_lossy().into_owned();
        fs::create_dir(t.path().join("sub")).unwrap();
        fs::write(t.path().join("a.zip"), b"x").unwrap();
        fs::write(t.path().join("f.txt"), b"x").unwrap();
        assert_eq!(resolve("sub", &base).unwrap().kind, ResolveKind::Dir);
        assert_eq!(resolve("a.zip", &base).unwrap().kind, ResolveKind::Archive);
        assert_eq!(
            resolve("a.zip\\inner\\d", &base).unwrap().kind,
            ResolveKind::Archive
        );
        assert_eq!(resolve("f.txt", &base).unwrap().kind, ResolveKind::File);
        assert_eq!(resolve("nope", &base).unwrap().kind, ResolveKind::Missing);
        assert_eq!(resolve("sub\\..", &base).unwrap().kind, ResolveKind::Dir);
    }

    #[test]
    fn run_bounded_times_out_and_caps_concurrent_workers() {
        static C: AtomicUsize = AtomicUsize::new(0);
        // A quick job completes.
        assert_eq!(run_bounded(&C, 2, Duration::from_millis(500), || 7), Ok(7));
        // Two stuck workers fill the cap (they outlive their deadline)...
        for _ in 0..2 {
            let r = run_bounded(&C, 2, Duration::from_millis(20), || {
                std::thread::sleep(Duration::from_millis(600));
            });
            assert_eq!(r, Err(Bounded::Timeout));
        }
        // ...so the next call is refused at once, without spawning.
        let t0 = Instant::now();
        assert_eq!(
            run_bounded(&C, 2, Duration::from_millis(500), || 1),
            Err(Bounded::Busy)
        );
        assert!(t0.elapsed() < Duration::from_millis(100));
        assert_eq!(C.load(Ordering::SeqCst), 2);
        // Once the stuck workers finish, capacity returns.
        std::thread::sleep(Duration::from_millis(800));
        assert_eq!(C.load(Ordering::SeqCst), 0);
        assert_eq!(run_bounded(&C, 2, Duration::from_millis(500), || 3), Ok(3));
    }

    /// FER-461: a slow SMB host holding its slots (and even every network
    /// slot) never makes a LOCAL suggestion time out, and one host cannot take
    /// all network slots.
    #[test]
    fn a_slow_network_host_never_starves_local_suggestions() {
        let slow = "slow-host-fer461";
        let held: Vec<Permit> = (0..MAX_PER_HOST)
            .map(|_| Permit::network(slow).expect("free slot"))
            .collect();
        assert!(Permit::network(slow).is_none(), "per-host cap");
        // Other hosts still get slots, up to the network-wide cap.
        let mut others = Vec::new();
        for i in 0.. {
            match Permit::network(&format!("other-host-fer461-{i}")) {
                Some(p) => others.push(p),
                None => break,
            }
        }
        assert_eq!(held.len() + others.len(), MAX_INFLIGHT);

        // Local suggestions are unaffected while every network slot is held.
        let dir = tempdir().unwrap();
        fs::create_dir(dir.path().join("Alpha")).unwrap();
        let base = dir.path().to_string_lossy().into_owned();
        let r = suggest_bounded("Al", &base, 10, &[]).unwrap();
        assert!(!r.timed_out, "local suggestion must not time out");
        assert_eq!(names(&r), vec!["Alpha"]);

        // A known UNC host whose slots are full is refused at once (no I/O).
        let t0 = Instant::now();
        let r = suggest_bounded(&format!("\\\\{slow}\\share\\x"), "", 10, &[slow.into()]).unwrap();
        assert!(r.timed_out && r.items.is_empty());
        assert!(t0.elapsed() < Duration::from_millis(100));

        drop(held);
        drop(others);
        assert!(net_inflight().lock().unwrap().get(slow).is_none());
        assert!(Permit::network(slow).is_some());
    }

    #[test]
    fn plan_classifies_unc_folders_as_network_and_local_folders_as_local() {
        let dir = tempdir().unwrap();
        let base = dir.path().to_string_lossy().into_owned();
        match plan_suggest(&format!(r"{base}\x"), "", 10, &[]).unwrap() {
            Plan::Scan { network, .. } => assert_eq!(network, None),
            p => panic!("{p:?}"),
        }
        match plan_suggest("\\\\Srv-Fer461\\share\\x", "", 10, &["srv-fer461".into()]).unwrap() {
            Plan::Scan { network, key, .. } => {
                assert_eq!(network.as_deref(), Some("srv-fer461"));
                assert_eq!(key, "srv-fer461");
            }
            p => panic!("{p:?}"),
        }
    }

    /// FER-461 L4: a junction whose target is an unpopulated cloud placeholder
    /// is not enumerated (the target's attributes are checked, not only the
    /// link's). Here: the junction target is readable, so it IS listed; the
    /// offline case cannot be created without a cloud provider, so the guard
    /// is pinned by reading the source.
    #[cfg(windows)]
    #[test]
    fn a_junction_target_is_checked_for_offline_attributes() {
        let src = include_str!("path_suggest.rs");
        let scan_fn = &src[src.find("pub fn scan(").unwrap()..];
        let scan_fn = &scan_fn[..scan_fn.find("fs::read_dir(dir_path)").unwrap()];
        assert!(scan_fn.contains("ATTR_REPARSE_POINT"));
        assert!(scan_fn.contains("fs::metadata(dir_path)"));

        let dir = tempdir().unwrap();
        let target = dir.path().join("target");
        fs::create_dir(&target).unwrap();
        fs::create_dir(target.join("Inner")).unwrap();
        let link = dir.path().join("link");
        let ok = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&target)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !ok {
            return; // no junction support here; the source pin above still holds
        }
        let r = scan(&link.to_string_lossy(), "In", 10, TIMEOUT);
        assert_eq!(names(&r), vec!["Inner"]);
    }

    #[test]
    fn slow_cache_marks_and_expires() {
        let key = "\\\\slow-host-fer456";
        assert!(!is_slow(key));
        mark_slow(key);
        assert!(is_slow(key));
        slow_map().lock().unwrap().insert(
            key.into(),
            Instant::now() - SLOW_TTL - Duration::from_secs(1),
        );
        assert!(!is_slow(key));
    }

    #[cfg(windows)]
    #[test]
    fn drive_typing_lists_drives() {
        let r = sg("", "", 50).unwrap();
        assert!(r.items.iter().all(|s| s.kind == SuggestKind::Drive));
        assert!(!r.items.is_empty());
    }
}
