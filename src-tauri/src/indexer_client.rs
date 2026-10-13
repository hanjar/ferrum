//! FER-474 (IDX-S1): Ferrum's side of the `FerrumIndexer` service — status
//! for Settings, and the privacy-mode change. Ferrum stays a normal,
//! non-elevated process; when the service is absent or stopped everything
//! here degrades to a plain "not installed / not running" status and the
//! rest of Ferrum keeps using its own scanning.
//!
//! Mode changes:
//! - `→ shared` (more privacy): sent over the pipe as this user.
//! - `→ high_performance` (less privacy): the service only accepts it from an
//!   elevated administrator, so Ferrum runs `ferrum-indexer.exe set-mode
//!   high_performance` through UAC (binary resolved next to ferrum.exe, and
//!   in release builds only from Program Files — FER-460 rule).
//! - Both require `confirmed: true`, set by the frontend only after the
//!   privacy dialog (`IndexerModeDialog`) was accepted.

use serde::Serialize;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexerStatus {
    /// Registered with the SCM.
    pub installed: bool,
    /// SCM state (`running`, `stopped`, `paused`, `start_pending`, …).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scm_state: Option<String>,
    /// The pipe answered and passed verification.
    pub reachable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// `high_performance` | `shared`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Service-reported state (`running` | `paused`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// Index state (`none` in S1).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index: Option<String>,
    /// This Ferrum process could lower privacy without UAC (it is elevated).
    pub can_lower_privacy: bool,
    /// Machine-readable problem: `unsupported`, `location` (Ferrum is not
    /// under Program Files, so the installers skipped the service), `scm`,
    /// `untrusted`, `squatted` (FER-480 review F1: another process holds
    /// the pipe name), `unreachable`, `busy`, `protocol`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
    /// FER-480 review F1: PID of the process holding the pipe name when
    /// `problem == "squatted"` (and readable).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub squatter_pid: Option<u32>,
    /// FER-476: index work paused (service paused).
    pub index_paused: bool,
    /// FER-476 review H1: the service token is not restricted, so the
    /// service refuses search/subtree; Ferrum uses its own scanning.
    pub degraded: bool,
    /// FER-476: per-volume index state (codes only, never paths).
    pub volumes: Vec<IndexerVolume>,
}

/// One volume's index state for Settings and the AI (`indexer.status`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexerVolume {
    pub letter: String,
    /// `building` | `ready` | `error`.
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub entries: u64,
    pub progress_permille: u16,
    pub live: bool,
}

impl IndexerVolume {
    pub fn from_status(v: &ferrum_indexer::proto::VolumeStatus) -> IndexerVolume {
        use ferrum_indexer::proto::VolumeState;
        IndexerVolume {
            letter: (v.letter as char).to_string(),
            state: v.state.as_str().into(),
            error: (v.state == VolumeState::Error).then(|| v.error.as_str().into()),
            entries: v.entries,
            progress_permille: v.progress_permille.min(1000),
            live: v.live,
        }
    }
}

/// FILETIME (100 ns since 1601) → Unix milliseconds; 0 stays 0 (unknown).
pub fn filetime_to_unix_ms(ft: u64) -> i64 {
    if ft == 0 {
        0
    } else {
        (ft / 10_000) as i64 - 11_644_473_600_000
    }
}

/// `root` as the service expects a scope: `X:\` or `X:\a\b` (uppercase
/// drive letter, no trailing separator below the root, `/` → `\`).
/// `None` for anything the index cannot answer (UNC, relative, `..`).
pub fn index_scope(root: &str) -> Option<String> {
    let r = root.replace('/', "\\");
    let mut s = r.trim_end_matches('\\').to_string();
    if s.len() == 2 {
        s.push('\\');
    }
    let (letter, comps) = ferrum_indexer::query::parse_abs_dir(&s)?;
    let mut out = format!("{}:\\", letter as char);
    out.push_str(&comps.join("\\"));
    Some(out)
}

/// Is `inner` the same directory as `outer` or below it (case-insensitive,
/// component-wise)?
pub fn path_within(inner: &str, outer: &str) -> bool {
    let norm = |p: &str| -> Vec<String> {
        p.replace('/', "\\")
            .split('\\')
            .filter(|c| !c.is_empty())
            .map(|c| c.to_lowercase())
            .collect()
    };
    let (i, o) = (norm(inner), norm(outer));
    !o.is_empty() && i.len() >= o.len() && i[..o.len()] == o[..]
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexerSetModeResult {
    pub mode: String,
    pub changed: bool,
    /// The change went through the elevated (UAC) path.
    pub elevated: bool,
}

/// Error for the frontend: `code` drives the UI text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IndexerError {
    /// `not_confirmed`, `invalid_mode`, `unsupported`, `not_running`,
    /// `declined` (UAC No), `refused`, `failed`.
    pub code: String,
    pub message: String,
}

impl IndexerError {
    fn new(code: &str, message: impl Into<String>) -> Self {
        IndexerError {
            code: code.into(),
            message: message.into(),
        }
    }
}

/// Validate the frontend's request before anything is contacted.
pub fn check_set_mode_request(
    mode: &str,
    confirmed: bool,
) -> Result<ferrum_indexer::proto::Mode, IndexerError> {
    let m = ferrum_indexer::proto::Mode::parse(mode)
        .ok_or_else(|| IndexerError::new("invalid_mode", format!("unknown mode {mode:?}")))?;
    if !confirmed {
        return Err(IndexerError::new(
            "not_confirmed",
            "the privacy confirmation was not accepted",
        ));
    }
    Ok(m)
}

#[tauri::command]
pub async fn indexer_status() -> IndexerStatus {
    tauri::async_runtime::spawn_blocking(imp::status)
        .await
        .unwrap_or_else(|_| IndexerStatus {
            problem: Some("protocol".into()),
            ..Default::default()
        })
}

#[tauri::command]
pub async fn indexer_set_mode(
    window: tauri::Window,
    mode: String,
    confirmed: bool,
) -> Result<IndexerSetModeResult, IndexerError> {
    let m = check_set_mode_request(&mode, confirmed)?;
    let hwnd = owner_hwnd(&window);
    tauri::async_runtime::spawn_blocking(move || imp::set_mode(m, hwnd))
        .await
        .map_err(|e| IndexerError::new("failed", e.to_string()))?
}

fn owner_hwnd(window: &tauri::Window) -> isize {
    #[cfg(windows)]
    {
        window.hwnd().map(|h| h.0 as isize).unwrap_or(0)
    }
    #[cfg(not(windows))]
    {
        let _ = window;
        0
    }
}

/// FER-476: answer a search from the FerrumIndexer when it can (glob name
/// search, recursive, no content filter, a drive-letter root that the index
/// covers). `None` = not used: nothing was emitted, the caller runs the
/// walker. Size/time/depth/case filters are applied here on the hits, with
/// the walker's exact name rule (`compile_name_matcher`) re-checked.
pub fn index_search(
    root: &str,
    options: &crate::search::SearchOptions,
    cancel: &std::sync::atomic::AtomicBool,
    deadline: std::time::Instant,
    sink: &mut dyn FnMut(&[crate::domain::FileEntry]) -> bool,
) -> Option<Result<crate::search::SearchStats, crate::domain::FsError>> {
    imp::index_search(root, options, cancel, deadline, sink)
}

/// FER-476 Disk Usage: every visible entry below `root` from the index, as
/// the MFT helper's entry stream (`Err` = not available; the caller falls
/// back). `progress(n)` after each chunk.
pub fn index_subtree(
    root: &str,
    cancel: &std::sync::atomic::AtomicBool,
    progress: &mut dyn FnMut(u64),
) -> Result<IndexSubtree, String> {
    imp::index_subtree(root, cancel, progress)
}

/// Result of [`index_subtree`].
pub struct IndexSubtree {
    pub root_frn: u64,
    pub entries: Vec<ferrum_ntfs_scan::protocol::WireEntry>,
    /// Answered from the previous index while it is rebuilt.
    pub stale: bool,
    /// Shared mode: profile folders (and the recycle bin) inside `root` are
    /// not indexed, so their size is missing.
    pub excluded_inside: bool,
}

/// Most entries Ferrum accepts from one subtree answer (same order as the
/// MFT helper's cap).
pub const MAX_SUBTREE_ENTRIES: u64 = 30_000_000;

/// May Disk Usage offer "Instan (indeks)" for `root`? Cheap: one status
/// round-trip, no elevation.
#[tauri::command]
pub async fn disk_usage_index_eligibility(root: String) -> crate::fast_scan::FastScanEligibility {
    tauri::async_runtime::spawn_blocking(move || imp::index_eligibility(&root))
        .await
        .unwrap_or_else(|_| crate::fast_scan::FastScanEligibility::not("indeks tidak tersedia"))
}

/// FER-464: does the FerrumIndexer currently cover `root` (service up, not
/// degraded, drive index ready)? Roots it covers use "Instan (indeks)" and
/// are never written to the Disk usage scan cache.
pub fn index_covers(root: &str) -> bool {
    imp::index_eligibility(root).eligible
}

/// FER-477a: folder sizes for the file list (Size column of folder rows).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderSizesResult {
    /// The service answered (High performance mode, parent indexed).
    pub available: bool,
    /// Why not: `not_local`, `unavailable`, `shared_mode`, `degraded`,
    /// `not_indexed`, `busy`, `timeout`, `too_many`, `protocol`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// One per requested name: total bytes, or `null` when unknown (not an
    /// indexed folder, invalid name, or not available).
    pub sizes: Vec<Option<u64>>,
    /// Answered from the previous index while it is rebuilt.
    pub stale: bool,
}

/// Most names one `indexer_folder_sizes` call accepts (the service takes
/// at most 200 per request; Ferrum splits).
pub const MAX_FOLDER_SIZE_NAMES: usize = 1000;

impl FolderSizesResult {
    fn unavailable(n: usize, reason: &str) -> Self {
        FolderSizesResult {
            available: false,
            reason: Some(reason.into()),
            sizes: vec![None; n],
            stale: false,
        }
    }
}

/// Split `names` (indices into the caller's list) into requests that fit
/// the service's limits: at most 200 names and a frame of at most
/// `MAX_REQUEST_FRAME` bytes.
pub fn folder_size_batches(parent: &str, names: &[(usize, String)]) -> Vec<Vec<(usize, String)>> {
    use ferrum_indexer::proto::MAX_REQUEST_FRAME;
    use ferrum_indexer::query::MAX_FOLDER_NAMES;
    let fixed = 1 + 2 + parent.len() + 1;
    let budget = MAX_REQUEST_FRAME as usize;
    let mut out: Vec<Vec<(usize, String)>> = Vec::new();
    let mut cur: Vec<(usize, String)> = Vec::new();
    let mut size = fixed;
    for (i, n) in names {
        let cost = 2 + n.len();
        if !cur.is_empty() && (cur.len() >= MAX_FOLDER_NAMES || size + cost > budget) {
            out.push(std::mem::take(&mut cur));
            size = fixed;
        }
        size += cost;
        cur.push((*i, n.clone()));
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// FER-477a: total size of each folder `parent\name` from the FerrumIndexer.
/// Only answered in High performance mode — the service refuses it in the
/// shared mode (privacy), and every other problem is a `reason`; the file
/// list then shows "–" as before. Read-only, no elevation, nothing is
/// opened on disk by Ferrum.
#[tauri::command]
pub async fn indexer_folder_sizes(parent: String, names: Vec<String>) -> FolderSizesResult {
    let n = names.len();
    tauri::async_runtime::spawn_blocking(move || folder_sizes(&parent, &names))
        .await
        .unwrap_or_else(|_| FolderSizesResult::unavailable(n, "protocol"))
}

pub fn folder_sizes(parent: &str, names: &[String]) -> FolderSizesResult {
    if names.len() > MAX_FOLDER_SIZE_NAMES {
        return FolderSizesResult::unavailable(names.len(), "too_many");
    }
    let Some(scope) = index_scope(parent) else {
        return FolderSizesResult::unavailable(names.len(), "not_local");
    };
    imp::folder_sizes(&scope, names)
}

/// FER-477b: changed directories below both compare roots since the two
/// marks, over one verified connection (see `sync_incremental`).
pub fn journal_changes(
    roots: [&str; 2],
    since: [Option<ferrum_indexer::query::JournalMark>; 2],
) -> Result<[ferrum_indexer::query::Changes; 2], String> {
    imp::journal_changes(roots, since)
}

#[cfg(not(windows))]
mod imp {
    use super::*;
    pub fn index_search(
        _root: &str,
        _o: &crate::search::SearchOptions,
        _c: &std::sync::atomic::AtomicBool,
        _d: std::time::Instant,
        _s: &mut dyn FnMut(&[crate::domain::FileEntry]) -> bool,
    ) -> Option<Result<crate::search::SearchStats, crate::domain::FsError>> {
        None
    }
    pub fn index_subtree(
        _root: &str,
        _c: &std::sync::atomic::AtomicBool,
        _p: &mut dyn FnMut(u64),
    ) -> Result<IndexSubtree, String> {
        Err("hanya Windows".into())
    }
    pub fn index_eligibility(_root: &str) -> crate::fast_scan::FastScanEligibility {
        crate::fast_scan::FastScanEligibility::not("hanya Windows")
    }
    pub fn folder_sizes(_parent: &str, names: &[String]) -> FolderSizesResult {
        FolderSizesResult::unavailable(names.len(), "unavailable")
    }
    pub fn journal_changes(
        _roots: [&str; 2],
        _since: [Option<ferrum_indexer::query::JournalMark>; 2],
    ) -> Result<[ferrum_indexer::query::Changes; 2], String> {
        Err("hanya Windows".into())
    }
    pub fn status() -> IndexerStatus {
        IndexerStatus {
            problem: Some("unsupported".into()),
            ..Default::default()
        }
    }
    pub fn set_mode(
        _m: ferrum_indexer::proto::Mode,
        _hwnd: isize,
    ) -> Result<IndexerSetModeResult, IndexerError> {
        Err(IndexerError::new(
            "unsupported",
            "FerrumIndexer is Windows-only",
        ))
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use ferrum_indexer::client::{Client, ClientError};
    use ferrum_indexer::pipe::Endpoint;
    use ferrum_indexer::proto::{ErrorCode, Mode};
    use ferrum_indexer::service::ScmState as ServiceState;
    use std::path::PathBuf;
    use std::time::Duration;

    const TIMEOUT: Duration = Duration::from_secs(3);
    /// The elevated `set-mode` process must finish within this (after UAC).
    const ELEVATED_TIMEOUT: Duration = Duration::from_secs(30);

    pub fn scm_state_str(s: ServiceState) -> &'static str {
        match s {
            ServiceState::Stopped => "stopped",
            ServiceState::StartPending => "start_pending",
            ServiceState::StopPending => "stop_pending",
            ServiceState::Running => "running",
            ServiceState::ContinuePending => "continue_pending",
            ServiceState::PausePending => "pause_pending",
            ServiceState::Paused => "paused",
        }
    }

    fn installed_in_program_files() -> bool {
        std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(|d| d.to_path_buf()))
            .is_some_and(|d| {
                ferrum_indexer::win::in_protected_dir(&d, &ferrum_indexer::win::protected_roots())
            })
    }

    fn problem(e: &ClientError) -> &'static str {
        match e {
            ClientError::NotRunning => "unreachable",
            ClientError::Busy => "busy",
            ClientError::Squatted { .. } => "squatted",
            ClientError::Untrusted(_) => "untrusted",
            ClientError::Refused { .. } | ClientError::Protocol(_) => "protocol",
        }
    }

    pub fn status() -> IndexerStatus {
        let mut st = IndexerStatus::default();
        match ferrum_indexer::service::query_state() {
            Ok(None) => {
                // Review M1: the installers only register the service when
                // Ferrum lives under Program Files; say so instead of a bare
                // "not installed".
                if !installed_in_program_files() {
                    st.problem = Some("location".into());
                }
                return st;
            }
            Ok(Some(s)) => {
                st.installed = true;
                st.scm_state = Some(scm_state_str(s).into());
                if !matches!(s, ServiceState::Running | ServiceState::Paused) {
                    return st;
                }
            }
            Err(e) => {
                log::warn!("indexer: SCM query failed: {e}");
                st.problem = Some("scm".into());
                return st;
            }
        }
        let r = Client::connect(&Endpoint::Service, TIMEOUT).and_then(|mut c| {
            let s = c.status()?;
            // Older services (no indexStatus) still report the rest.
            let ix = c.index_status().ok();
            Ok((s, ix, c.hello.can_lower_privacy))
        });
        match r {
            Ok((s, ix, can)) => {
                st.reachable = true;
                st.version = Some(s.version);
                st.mode = Some(s.mode.as_str().into());
                st.state = Some(s.state.as_str().into());
                st.index = Some(s.index.as_str().into());
                st.can_lower_privacy = can;
                st.degraded = s.degraded;
                if let Some(ix) = ix {
                    st.index_paused = ix.paused;
                    st.volumes = ix.volumes.iter().map(IndexerVolume::from_status).collect();
                }
            }
            Err(e) => {
                log::warn!("indexer: status failed: {e}");
                st.problem = Some(problem(&e).into());
                if let ClientError::Squatted { pid, .. } = &e {
                    st.squatter_pid = *pid;
                }
            }
        }
        st
    }

    pub fn set_mode(m: Mode, hwnd: isize) -> Result<IndexerSetModeResult, IndexerError> {
        let mut c = Client::connect(&Endpoint::Service, TIMEOUT).map_err(|e| match e {
            ClientError::NotRunning => {
                IndexerError::new("not_running", "FerrumIndexer is not running")
            }
            other => IndexerError::new("failed", other.to_string()),
        })?;
        match c.set_mode(m) {
            Ok(changed) => Ok(IndexerSetModeResult {
                mode: m.as_str().into(),
                changed,
                elevated: false,
            }),
            Err(ClientError::Refused {
                code: ErrorCode::NeedsElevation,
                ..
            }) => {
                drop(c);
                run_elevated_set_mode(m, hwnd)?;
                Ok(IndexerSetModeResult {
                    mode: m.as_str().into(),
                    changed: true,
                    elevated: true,
                })
            }
            Err(ClientError::Refused { code, message }) => Err(IndexerError::new(
                "refused",
                format!("{}: {message}", code.as_str()),
            )),
            Err(e) => Err(IndexerError::new("failed", e.to_string())),
        }
    }

    /// Connect quickly: a missing service must not delay the fallback.
    fn quick_client(ep: &Endpoint) -> Option<Client> {
        match Client::connect(ep, Duration::from_millis(700)) {
            Ok(c) => Some(c),
            // FER-480: a pipe whose server fails verification (possible
            // squatter) is never used; worth a line in the log.
            Err(e @ (ClientError::Untrusted(_) | ClientError::Squatted { .. })) => {
                log::warn!("indexer: {e}");
                None
            }
            Err(_) => None,
        }
    }

    pub fn index_eligibility(root: &str) -> crate::fast_scan::FastScanEligibility {
        use crate::fast_scan::FastScanEligibility as E;
        use ferrum_indexer::proto::VolumeState;
        let Some(scope) = index_scope(root) else {
            return E::not("bukan drive lokal");
        };
        let letter = scope.as_bytes()[0];
        let Some(mut c) = quick_client(&Endpoint::Service) else {
            return E::not("FerrumIndexer tidak berjalan atau tidak tepercaya");
        };
        match c.index_status() {
            Ok(st) if st.degraded => E::not("FerrumIndexer dalam mode terbatas (degraded)"),
            Ok(st) => match st.volumes.iter().find(|v| v.letter == letter) {
                Some(v) if v.state == VolumeState::Ready => E::yes(&(letter as char).to_string()),
                Some(v) if v.state == VolumeState::Building => {
                    E::not("indeks drive ini sedang dibangun")
                }
                Some(_) => E::not("indeks drive ini tidak tersedia"),
                None => E::not("drive ini tidak diindeks"),
            },
            Err(_) => E::not("FerrumIndexer tidak menjawab"),
        }
    }

    pub fn index_search(
        root: &str,
        o: &crate::search::SearchOptions,
        cancel: &std::sync::atomic::AtomicBool,
        deadline: std::time::Instant,
        sink: &mut dyn FnMut(&[crate::domain::FileEntry]) -> bool,
    ) -> Option<Result<crate::search::SearchStats, crate::domain::FsError>> {
        index_search_at(&Endpoint::Service, root, o, cancel, deadline, sink)
    }

    pub fn index_search_at(
        ep: &Endpoint,
        root: &str,
        o: &crate::search::SearchOptions,
        cancel: &std::sync::atomic::AtomicBool,
        deadline: std::time::Instant,
        sink: &mut dyn FnMut(&[crate::domain::FileEntry]) -> bool,
    ) -> Option<Result<crate::search::SearchStats, crate::domain::FsError>> {
        use crate::search::{SearchSource, SearchStats};
        use ferrum_indexer::index::Kind;
        use ferrum_indexer::query::{KindFilter, MatchKind, QueryCursor, SearchQuery};
        use std::sync::atomic::Ordering;
        let pattern = o.pattern.trim();
        if o.mode != "glob"
            || !o.recursive
            || pattern.is_empty()
            || o.content.as_deref().is_some_and(|c| !c.trim().is_empty())
        {
            return None;
        }
        let scope = index_scope(root)?;
        let matcher =
            crate::search::compile_name_matcher(pattern, "glob", o.case_sensitive).ok()?;
        let mut q = SearchQuery {
            pattern: pattern.to_string(),
            match_kind: MatchKind::Glob,
            scope: scope.clone(),
            extensions: Vec::new(),
            kinds: KindFilter::Both,
            limit: ferrum_indexer::query::MAX_PAGE,
            cursor: QueryCursor::default(),
        };
        q.validate().ok()?;
        let mut c = quick_client(ep)?;
        let start = std::time::Instant::now();
        let max = if o.max_results == 0 {
            100_000
        } else {
            o.max_results.min(100_000)
        };
        let root_depth = scope.trim_end_matches('\\').split('\\').count();
        let mut stats = SearchStats {
            matched: 0,
            visited: 0,
            errors: 0,
            truncated: false,
            cancelled: false,
            timed_out: false,
            duration_ms: 0,
            source: SearchSource::Index,
            stale: false,
        };
        let mut first = true;
        let mut excluded_inside = false;
        let mut stopped = false;
        loop {
            if cancel.load(Ordering::Relaxed) {
                stats.cancelled = true;
                stopped = true;
                break;
            }
            if std::time::Instant::now() >= deadline {
                stats.timed_out = true;
                stopped = true;
                break;
            }
            let page = match c.search(&q) {
                Ok(p) => p,
                Err(e) if first => {
                    log::info!("indexer search not used: {e}");
                    return None;
                }
                Err(e) => {
                    log::warn!("indexer search stopped: {e}");
                    stats.truncated = true;
                    stopped = true;
                    break;
                }
            };
            first = false;
            excluded_inside |= page.excluded_inside;
            stats.stale |= page.stale;
            stats.visited += page.hits.len();
            let mut batch = Vec::new();
            for h in page.hits {
                let name = h.path.rsplit('\\').next().unwrap_or_default().to_string();
                let depth = h.path.split('\\').count().saturating_sub(root_depth + 1);
                if depth > o.max_depth || !matcher.is_match(&name) {
                    continue;
                }
                let is_dir = h.kind == Kind::Dir;
                let size = if is_dir { 0 } else { h.size };
                let modified_ms = filetime_to_unix_ms(h.modified);
                if !crate::search::passes_size_time_values(size, modified_ms, o, is_dir) {
                    continue;
                }
                batch.push(crate::domain::FileEntry {
                    hidden: name.starts_with('.') || h.attributes & 0x2 != 0,
                    name,
                    path: h.path,
                    is_dir,
                    is_symlink: h.kind == Kind::Symlink,
                    size,
                    modified_ms,
                });
                stats.matched += 1;
                if stats.matched >= max {
                    stats.truncated = true;
                    break;
                }
            }
            if !batch.is_empty() && !sink(&batch) {
                stats.cancelled = true;
                stopped = true;
                break;
            }
            if stats.truncated {
                stopped = true;
                break;
            }
            match page.next {
                Some(n) => q.cursor = n,
                None => break,
            }
        }
        drop(c);
        // Shared mode: profile folders are not indexed. This user's own
        // profile (if inside the searched root) is walked, so their own files
        // are still found; other profiles are not readable by them anyway.
        if excluded_inside
            && !stopped
            && let Ok(profile) = std::env::var("USERPROFILE")
            && path_within(&profile, &scope)
            && std::path::Path::new(&profile).is_dir()
        {
            let mut rest = o.clone();
            rest.max_results = max - stats.matched;
            match crate::search::run_search_with_deadline(
                &profile,
                &rest,
                cancel,
                Some(deadline),
                |b| sink(b),
            ) {
                Ok(w) => {
                    stats.matched += w.matched;
                    stats.visited += w.visited;
                    stats.errors += w.errors;
                    stats.truncated |= w.truncated;
                    stats.cancelled |= w.cancelled;
                    stats.timed_out |= w.timed_out;
                    stats.source = SearchSource::IndexAndWalker;
                }
                Err(e) => log::warn!("profile walk after index search failed: {e:?}"),
            }
        }
        stats.duration_ms = start.elapsed().as_millis() as u64;
        Some(Ok(stats))
    }

    pub fn index_subtree(
        root: &str,
        cancel: &std::sync::atomic::AtomicBool,
        progress: &mut dyn FnMut(u64),
    ) -> Result<IndexSubtree, String> {
        index_subtree_at(&Endpoint::Service, root, cancel, progress)
    }

    pub fn index_subtree_at(
        ep: &Endpoint,
        root: &str,
        cancel: &std::sync::atomic::AtomicBool,
        progress: &mut dyn FnMut(u64),
    ) -> Result<IndexSubtree, String> {
        use ferrum_indexer::index::Kind;
        use ferrum_ntfs_scan::protocol::{EntryKind, WireEntry};
        use std::sync::atomic::Ordering;
        let scope = index_scope(root).ok_or("bukan drive lokal")?;
        let mut c = quick_client(ep).ok_or("FerrumIndexer tidak berjalan atau tidak tepercaya")?;
        let mut entries: Vec<WireEntry> = Vec::new();
        let info = c
            .subtree(&scope, MAX_SUBTREE_ENTRIES, &mut |_, chunk| {
                if cancel.load(Ordering::Relaxed) {
                    return false;
                }
                entries.extend(chunk.into_iter().map(|e| WireEntry {
                    frn: e.frn,
                    parent: e.parent,
                    kind: match e.kind {
                        Kind::File => EntryKind::File,
                        Kind::Dir => EntryKind::Dir,
                        Kind::Symlink => EntryKind::Symlink,
                    },
                    logical: e.size,
                    allocated: e.allocated,
                    modified: 0,
                    name: e.name,
                }));
                progress(entries.len() as u64);
                true
            })
            .map_err(|e| match e {
                ClientError::Refused {
                    code: ErrorCode::NotIndexed,
                    ..
                } => "folder ini tidak ada di indeks".to_string(),
                ClientError::Refused {
                    code: ErrorCode::Busy,
                    ..
                } => "FerrumIndexer sibuk".to_string(),
                ClientError::Refused {
                    code: ErrorCode::Timeout,
                    ..
                } => "indeks terlalu lama menjawab".to_string(),
                other => other.to_string(),
            })?;
        Ok(IndexSubtree {
            root_frn: info.root_frn,
            entries,
            stale: info.stale,
            excluded_inside: info.excluded_inside,
        })
    }

    pub fn folder_sizes(parent: &str, names: &[String]) -> FolderSizesResult {
        folder_sizes_at(&Endpoint::Service, parent, names)
    }

    fn refused_reason(e: &ClientError) -> &'static str {
        match e {
            ClientError::Refused { code, .. } => match code {
                ErrorCode::Forbidden => "shared_mode",
                ErrorCode::Degraded => "degraded",
                ErrorCode::NotIndexed => "not_indexed",
                ErrorCode::Busy | ErrorCode::RateLimited => "busy",
                ErrorCode::Timeout => "timeout",
                ErrorCode::Paused => "unavailable",
                _ => "protocol",
            },
            ClientError::Busy => "busy",
            ClientError::Protocol(_) => "protocol",
            _ => "unavailable",
        }
    }

    pub fn folder_sizes_at(ep: &Endpoint, parent: &str, names: &[String]) -> FolderSizesResult {
        use ferrum_indexer::query::FolderSize;
        let mut out = FolderSizesResult {
            available: true,
            reason: None,
            sizes: vec![None; names.len()],
            stale: false,
        };
        // Names the service would refuse stay `None` here (never sent).
        let valid: Vec<(usize, String)> = names
            .iter()
            .enumerate()
            .filter(|(_, n)| ferrum_indexer::index::valid_name(n))
            .map(|(i, n)| (i, n.clone()))
            .collect();
        if valid.is_empty() {
            return out;
        }
        let Some(mut c) = quick_client(ep) else {
            return FolderSizesResult::unavailable(names.len(), "unavailable");
        };
        c.set_timeout(FOLDER_SIZES_TIMEOUT);
        for batch in folder_size_batches(parent, &valid) {
            let req: Vec<String> = batch.iter().map(|(_, n)| n.clone()).collect();
            match c.folder_sizes(parent, &req) {
                Ok(r) => {
                    out.stale |= r.stale;
                    for ((i, _), s) in batch.iter().zip(r.sizes) {
                        if let FolderSize::Ok { bytes, .. } = s {
                            out.sizes[*i] = Some(bytes);
                        }
                    }
                }
                Err(e) => {
                    log::debug!("indexer folder sizes not used: {e}");
                    return FolderSizesResult::unavailable(names.len(), refused_reason(&e));
                }
            }
        }
        out
    }

    /// The service waits up to `CHANGES_BARRIER_TIME` (5 s) for its index.
    const CHANGES_TIMEOUT: Duration = Duration::from_secs(10);
    /// The service answers folder sizes within 5 s (then `timeout`).
    const FOLDER_SIZES_TIMEOUT: Duration = Duration::from_secs(8);

    pub fn journal_changes(
        roots: [&str; 2],
        since: [Option<ferrum_indexer::query::JournalMark>; 2],
    ) -> Result<[ferrum_indexer::query::Changes; 2], String> {
        journal_changes_at(&Endpoint::Service, roots, since)
    }

    pub fn journal_changes_at(
        ep: &Endpoint,
        roots: [&str; 2],
        since: [Option<ferrum_indexer::query::JournalMark>; 2],
    ) -> Result<[ferrum_indexer::query::Changes; 2], String> {
        let mut c = quick_client(ep).ok_or("FerrumIndexer not available")?;
        c.set_timeout(CHANGES_TIMEOUT);
        let l = c.changes(roots[0], since[0]).map_err(|e| e.to_string())?;
        let r = c.changes(roots[1], since[1]).map_err(|e| e.to_string())?;
        Ok([l, r])
    }

    /// `ferrum-indexer.exe` next to ferrum.exe; in release builds only from
    /// Program Files (a user-writable copy must never get a UAC "Yes").
    fn indexer_binary() -> Result<PathBuf, IndexerError> {
        let exe = std::env::current_exe()
            .map_err(|e| IndexerError::new("failed", format!("own path: {e}")))?;
        let p = ferrum_indexer::service::binary_next_to(&exe)
            .filter(|p| p.is_file())
            .ok_or_else(|| IndexerError::new("failed", "ferrum-indexer.exe is not installed"))?;
        if !cfg!(debug_assertions) {
            let dir = p.parent().unwrap_or(&p);
            if !ferrum_indexer::win::in_protected_dir(dir, &ferrum_indexer::win::protected_roots())
            {
                return Err(IndexerError::new(
                    "failed",
                    "the mode can only be raised from an installation under Program Files",
                ));
            }
        }
        Ok(p)
    }

    /// Run `ferrum-indexer.exe set-mode <m>` elevated (UAC) and wait for it.
    fn run_elevated_set_mode(m: Mode, hwnd: isize) -> Result<(), IndexerError> {
        use ferrum_indexer::win::{OwnedHandle, wide};
        use windows_sys::Win32::Foundation::{ERROR_CANCELLED, WAIT_OBJECT_0};
        use windows_sys::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
        use windows_sys::Win32::UI::Shell::{
            SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
            ShellExecuteExW,
        };
        use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;

        let exe = indexer_binary()?;
        let verb = wide("runas");
        let file = wide(&exe);
        // Fixed, validated argument: the mode string comes from `Mode`.
        let params = wide(format!("set-mode {}", m.as_str()));
        let dir = wide(exe.parent().map(|p| p.as_os_str()).unwrap_or_default());
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
            let err = std::io::Error::last_os_error();
            return Err(if err.raw_os_error() == Some(ERROR_CANCELLED as i32) {
                IndexerError::new("declined", "administrator permission was declined")
            } else {
                IndexerError::new("failed", format!("could not start ferrum-indexer: {err}"))
            });
        }
        if sei.hProcess.is_null() {
            return Err(IndexerError::new("failed", "no process handle"));
        }
        let process = OwnedHandle(sei.hProcess);
        if unsafe { WaitForSingleObject(process.raw(), ELEVATED_TIMEOUT.as_millis() as u32) }
            != WAIT_OBJECT_0
        {
            return Err(IndexerError::new("failed", "ferrum-indexer did not finish"));
        }
        let mut code = 1u32;
        unsafe { GetExitCodeProcess(process.raw(), &mut code) };
        match code {
            0 => Ok(()),
            4 => Err(IndexerError::new(
                "not_running",
                "FerrumIndexer is not running",
            )),
            c => Err(IndexerError::new(
                "failed",
                format!("ferrum-indexer set-mode exited with {c}"),
            )),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn scm_states_have_stable_names() {
            assert_eq!(scm_state_str(ServiceState::Running), "running");
            assert_eq!(scm_state_str(ServiceState::Stopped), "stopped");
            assert_eq!(scm_state_str(ServiceState::Paused), "paused");
        }

        /// FER-476: Ferrum's index search + Disk Usage subtree against a
        /// real (console) FerrumIndexer on a dev pipe serving a synthetic
        /// index: filters applied on hits, entries converted for the
        /// snapshot builder, and a missing scope answers "not used".
        #[test]
        fn index_search_and_subtree_against_a_console_service() {
            use ferrum_indexer::engine::{IndexShared, LiveIndex, ServedVolume};
            use ferrum_indexer::index::{Kind, Meta, VolumeIndex};
            use ferrum_indexer::proto::Mode;
            use ferrum_indexer::server::{self, ServerState};
            use std::sync::atomic::{AtomicBool, Ordering};
            use std::sync::{Arc, RwLock};
            const ROOT: u64 = 5 | (5 << 48);
            let f = |r: u64| r | (1 << 48);
            let m = |size| Meta {
                size,
                allocated: Some(size),
                modified: 116_444_736_000_000_000 + 10_000 * 1000,
                attributes: 0x20,
            };
            let mut ix = VolumeIndex::new(ROOT);
            ix.add_name(f(20), ROOT, Kind::Dir, Meta::default(), "Data")
                .unwrap();
            ix.add_name(f(21), f(20), Kind::File, m(10), "a.txt")
                .unwrap();
            ix.add_name(f(22), f(20), Kind::File, m(99), "B.TXT")
                .unwrap();
            ix.add_name(f(23), f(20), Kind::Dir, Meta::default(), "sub")
                .unwrap();
            ix.add_name(f(24), f(23), Kind::File, m(1), "deep.txt")
                .unwrap();
            ix.add_name(f(25), ROOT, Kind::File, m(1), "out.txt")
                .unwrap();
            let live = LiveIndex {
                index: ix,
                excluded: Default::default(),
                scope: ferrum_indexer::scope::Scope::everything(),
                journal_id: Some(1),
                next_usn: 0,
                dirty: false,
                stats: Default::default(),
                failed_rounds: 0,
                security_changes: 0,
                changes: ferrum_indexer::changelog::ChangeLog::new(0),
                caught_pass: 0,
            };
            let shared = IndexShared::new(Mode::HighPerformance);
            shared.serve_for_test(vec![ServedVolume {
                letter: b'Q',
                index: Some(Arc::new(RwLock::new(live))),
                stale: false,
            }]);
            let ep = Endpoint::dev(&format!("ferrum-{}", std::process::id())).unwrap();
            let state = Arc::new(
                ServerState::new("t", true, Mode::HighPerformance, None)
                    .with_index(shared.clone())
                    .with_queries(true),
            );
            let stop = Arc::new(AtomicBool::new(false));
            let (e2, s2, st2) = (ep.clone(), stop.clone(), state.clone());
            let th = std::thread::spawn(move || server::run(&e2, st2, s2));
            for _ in 0..100 {
                if Client::connect(&ep, Duration::from_millis(50)).is_ok() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            let run = |root: &str, o: crate::search::SearchOptions| {
                let mut got = Vec::new();
                let r = index_search_at(
                    &ep,
                    root,
                    &o,
                    &AtomicBool::new(false),
                    std::time::Instant::now() + Duration::from_secs(10),
                    &mut |b| {
                        got.extend(b.iter().cloned());
                        true
                    },
                );
                (r, got)
            };
            let o = crate::search::SearchOptions {
                pattern: "*.txt".into(),
                ..Default::default()
            };
            let (r, got) = run(r"Q:\Data", o.clone());
            let stats = r.expect("index used").unwrap();
            assert_eq!(stats.source, crate::search::SearchSource::Index);
            let mut names: Vec<&str> = got.iter().map(|e| e.name.as_str()).collect();
            names.sort();
            assert_eq!(names, vec!["B.TXT", "a.txt", "deep.txt"]);
            let a = got.iter().find(|e| e.name == "a.txt").unwrap();
            assert_eq!(a.path, r"Q:\Data\a.txt");
            assert_eq!(a.modified_ms, 1000);
            // Walker semantics on hits: case sensitivity, size, depth.
            let (_, got) = run(
                r"Q:\Data",
                crate::search::SearchOptions {
                    case_sensitive: true,
                    min_size: Some(5),
                    max_depth: 0,
                    ..o.clone()
                },
            );
            let names: Vec<&str> = got.iter().map(|e| e.name.as_str()).collect();
            assert_eq!(names, vec!["a.txt"]);
            // Not indexed scope / unsupported query -> not used (walker).
            assert!(run(r"Q:\Nope", o.clone()).0.is_none());
            assert!(
                run(
                    r"Q:\Data",
                    crate::search::SearchOptions {
                        content: Some("x".into()),
                        ..o.clone()
                    }
                )
                .0
                .is_none()
            );
            let sub =
                index_subtree_at(&ep, r"Q:\Data", &AtomicBool::new(false), &mut |_| {}).unwrap();
            assert_eq!(sub.root_frn, f(20));
            assert_eq!(sub.entries.len(), 4);
            assert!(
                index_subtree_at(&ep, r"Q:\Nope", &AtomicBool::new(false), &mut |_| {}).is_err()
            );
            // FER-477a: folder sizes (high_performance), invalid names never
            // sent, unknown ones null.
            let names: Vec<String> = ["Data", "out.txt", "..", "Nope", "data"]
                .iter()
                .map(|s| s.to_string())
                .collect();
            let fs = folder_sizes_at(&ep, r"Q:\", &names);
            assert!(fs.available, "{fs:?}");
            assert_eq!(fs.sizes, vec![Some(110), None, None, None, Some(110)]);
            let fs = folder_sizes_at(&ep, r"Q:\Nope", &names);
            assert_eq!(
                (fs.available, fs.reason.as_deref()),
                (false, Some("not_indexed"))
            );
            // FER-477b: marks need the freshness barrier (a worker pass).
            let ix = shared.served().0[0].index.clone().unwrap();
            let passes_stop = Arc::new(AtomicBool::new(false));
            let passes = {
                let (sh, st) = (shared.clone(), passes_stop.clone());
                std::thread::spawn(move || {
                    while !st.load(Ordering::SeqCst) {
                        sh.complete_pass_for_test(std::slice::from_ref(&ix));
                        std::thread::sleep(Duration::from_millis(20));
                    }
                })
            };
            let [a, b] = journal_changes_at(&ep, [r"Q:\Data", r"Q:\"], [None, None]).unwrap();
            assert_eq!(a.status, ferrum_indexer::query::ChangeStatus::MarkOnly);
            assert_eq!(b.mark.journal_id, 1);
            passes_stop.store(true, Ordering::SeqCst);
            let _ = passes.join();
            stop.store(true, Ordering::SeqCst);
            let _ = th.join();
        }

        /// On a dev machine the service is normally not installed; either
        /// way `status` must not fail or hang.
        #[test]
        fn status_never_fails() {
            let st = status();
            if !st.installed {
                assert!(!st.reachable && st.mode.is_none());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_mode_needs_a_valid_mode_and_confirmation() {
        assert_eq!(
            check_set_mode_request("shared", false).unwrap_err().code,
            "not_confirmed"
        );
        assert_eq!(
            check_set_mode_request("fast", true).unwrap_err().code,
            "invalid_mode"
        );
        assert_eq!(
            check_set_mode_request("Shared", true).unwrap_err().code,
            "invalid_mode"
        );
        assert!(check_set_mode_request("high_performance", true).is_ok());
        assert!(check_set_mode_request("shared", true).is_ok());
    }

    #[test]
    fn folder_size_batches_fit_the_service_limits() {
        use ferrum_indexer::proto::{MAX_REQUEST_FRAME, Request, encode_request};
        let parent = format!(r"C:\{}", "p".repeat(900));
        let names: Vec<(usize, String)> = (0..700)
            .map(|i| (i, format!("{i:0>3}{}", "n".repeat(i % 120))))
            .collect();
        let batches = folder_size_batches(&parent, &names);
        let mut seen = Vec::new();
        for b in &batches {
            assert!(!b.is_empty() && b.len() <= ferrum_indexer::query::MAX_FOLDER_NAMES);
            let frame = encode_request(&Request::FolderSizes {
                parent: parent.clone(),
                names: b.iter().map(|(_, n)| n.clone()).collect(),
            });
            assert!(
                frame.len() <= MAX_REQUEST_FRAME as usize + 4,
                "{}",
                frame.len()
            );
            seen.extend(b.iter().map(|(i, _)| *i));
        }
        assert_eq!(
            seen,
            (0..700).collect::<Vec<_>>(),
            "every name once, in order"
        );
        assert!(batches.len() >= 4);
        assert!(folder_size_batches(&parent, &[]).is_empty());
        // Ferrum-side refusals before any connection.
        let many: Vec<String> = (0..=MAX_FOLDER_SIZE_NAMES).map(|i| i.to_string()).collect();
        let r = folder_sizes(r"C:\", &many);
        assert_eq!(r.reason.as_deref(), Some("too_many"));
        assert_eq!(r.sizes.len(), many.len());
        let r = folder_sizes(r"\\srv\share", &["a".to_string()]);
        assert_eq!(
            (r.available, r.reason.as_deref(), r.sizes.clone()),
            (false, Some("not_local"), vec![None])
        );
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["reason"], "not_local");
        assert!(v["sizes"][0].is_null());
    }

    #[test]
    fn index_scope_normalises_roots() {
        assert_eq!(index_scope(r"c:\").as_deref(), Some(r"C:\"));
        assert_eq!(index_scope("C:").as_deref(), Some(r"C:\"));
        assert_eq!(
            index_scope(r"C:\Users\Bob\").as_deref(),
            Some(r"C:\Users\Bob")
        );
        assert_eq!(index_scope("D:/Data/x").as_deref(), Some(r"D:\Data\x"));
        for bad in [r"\\srv\share", "relative", r"C:\a\..\b", ""] {
            assert_eq!(index_scope(bad), None, "{bad}");
        }
        assert!(path_within(r"C:\Users\Bob", r"c:\users"));
        assert!(path_within(r"C:\Users\Bob", r"C:\Users\Bob\"));
        assert!(!path_within(r"C:\Users\Bobby", r"C:\Users\Bob"));
        assert!(path_within(r"C:\Users\Bob", r"C:\"));
        assert_eq!(filetime_to_unix_ms(0), 0);
        assert_eq!(filetime_to_unix_ms(116_444_736_000_000_000), 0);
    }

    #[test]
    fn status_serializes_camel_case() {
        let s = IndexerStatus {
            installed: true,
            scm_state: Some("running".into()),
            reachable: true,
            mode: Some("shared".into()),
            can_lower_privacy: false,
            ..Default::default()
        };
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["installed"], true);
        assert_eq!(v["scmState"], "running");
        assert_eq!(v["canLowerPrivacy"], false);
        assert!(v.get("problem").is_none());
        assert_eq!(v["indexPaused"], false);
        assert_eq!(v["degraded"], false);
        assert!(v["volumes"].as_array().unwrap().is_empty());
        let vol = IndexerVolume::from_status(&ferrum_indexer::proto::VolumeStatus {
            letter: b'C',
            state: ferrum_indexer::proto::VolumeState::Error,
            error: ferrum_indexer::proto::VolumeErrorCode::MemoryLimit,
            entries: 5,
            memory_bytes: 1,
            journal_id: 1,
            next_usn: 1,
            live: false,
            progress_permille: 0,
        });
        let v = serde_json::to_value(&vol).unwrap();
        assert_eq!(v["letter"], "C");
        assert_eq!(v["error"], "memory_limit");
        assert_eq!(v["progressPermille"], 0);
    }
}
