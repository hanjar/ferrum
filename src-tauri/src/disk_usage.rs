//! Progressive disk-usage scanner and per-subtree aggregation (DSK-02,
//! FER-320). Kept free of Tauri types so it stays unit-testable; `ipc.rs`
//! spawns the worker and forwards throttled events as
//! `disk_usage://progress` (≤10/s per scan), mirroring the
//! `search://progress` pattern.
//!
//! Contract (`docs/disk-usage-contract.md` §2/§3 — binding):
//! - Logical bytes = sum of file lengths per pathname; hardlink content MAY
//!   be counted once per pathname (footprint semantics, never presented as
//!   unique disk usage). Unique-allocation dedup is DEFERRED (gap G6).
//! - Allocated bytes are per-node `Option<u64>`: `None` means "at least one
//!   file under this node had no OS allocation data" — per-node null
//!   semantics (gap G2), never all-or-nothing across the scan and NEVER
//!   filled with logical bytes.
//! - Symlinks/junctions are counted as entries and NEVER followed (D07), so
//!   cycles are impossible by construction. Cross-volume child directories
//!   (drive-letter prefix differs from the scan root) are counted but never
//!   crossed (gap G3). Limitations: only `X:\` prefixes are recognized —
//!   UNC roots and NTFS mounted folders are NOT detected as boundaries, and
//!   on non-Windows the guard is off entirely (v1, Windows-first D44/G9).
//! - Metadata-only and read-only: no payload reads, no archive extraction,
//!   no cloud hydration, no mutation.
//! - Permission denied / deleted-during-scan are recorded as bounded,
//!   per-path-aggregated errors; the walk continues and coverage becomes
//!   `partial` — a failed entry is never silently counted as 0 bytes.
//! - Every scan gets a unique `scanId` (timestamp + counter). Progress
//!   events carry running totals with NO percent-done and NO ETA (the total
//!   is unknown until the walk ends). Only the final event sets `done`;
//!   cancel yields `done` with coverage `partial` + `cancelled: true`,
//!   never `complete`.
//! - `Coverage::Stale` is reserved for cached-scan freshness marking
//!   (gap G8, DSK-03/04); a live scan never emits it.
//! - Subtree totals are built from direct files + child subtrees; a parent
//!   is never re-added at the root, so root total = sum of leaves (gap G5).
//!
//! DSK-03 (FER-321) adds the read side — pure snapshot queries, still
//! Tauri-free so they stay unit-testable (`query_children_by_scan`,
//! `top_files_by_scan`; `ipc.rs` only forwards):
//! - Queries read ONLY the stored snapshot of a finished scan. An unknown or
//!   evicted `scanId` (bounded store) is a structured `NotFound` — the UI
//!   answers with a rescan (gap G8). An id outside the scanned tree is
//!   `NotFound` too; queries never re-walk the filesystem, so totals can
//!   never inflate.
//! - Every stored snapshot carries a monotonic `version` and `scanned_at_ms`;
//!   query responses repeat both so the UI can preserve selection across
//!   progress and detect a changed generation (gap G8). `Coverage::Stale`
//!   stays reserved for cached-scan freshness — never emitted by a live scan.
//! - `disk_usage_query_children` lists the DIRECT children of one node —
//!   child directories (subtree aggregates) + direct files + direct
//!   symlinks (0 bytes, counted but never followed, D07) — paginated,
//!   largest-first by the selected metric with a stable id-asc tie-break so
//!   pages are reproducible across calls (§3.8). Filtering never changes
//!   stored data; unknown allocated bytes stay `null` (gap G2) and simply
//!   sort as the smallest side — ordering, never substitution.
//! - `disk_usage_top_files` returns the largest FILES of a whole subtree,
//!   recursive, top-N with `truncated` + a "Top N" label — never a claim of
//!   all results (§3.8). Symlinks are excluded (their payload is not theirs).
//! - Percentages (`percent_of_parent`, `percent_of_scan_root`) are computed
//!   separately per item against explicit denominators (§2.7); partial
//!   coverage, a zero denominator, or an unknown value yield `null` — the
//!   UI renders "—", never 0%.
//! - Classification (DSK-03.2) is a simple data-driven extension map
//!   ([`category_of`]): video/image/audio/document/archive/other.
//!
//! DSK-09 (FER-405) replaces the single-threaded walk engine with a bounded
//! worker pool while keeping the public API and the event stream unchanged:
//! N scoped threads ([`default_worker_count()`], clamped `1..=16`) list
//! directories pulled from one shared queue while the CALLING thread stays
//! the aggregator — it owns the `LiveState` and the sink, so no lock ever
//! touches aggregation and `disk_usage://progress` semantics are byte-for-
//! byte the old ones (throttled running totals, exactly one final `done`).
//! Workers stream per-directory chunks over an mpsc channel; every
//! aggregate is commutative (saturating sums + `Option` poisoning), so
//! scheduling cannot change final totals, and
//! [`run_scan_serial_with_throttle`] keeps the previous walker verbatim as
//! the parity reference (`workers <= 1`) and the honest benchmark baseline.

use crate::domain::{ErrorCode, FsError, validate_path};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

/// Throttle for progressive events: at most one `disk_usage://progress`
/// batch per interval per scan (≤10/s, decision D14).
pub const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
/// Hard cap on distinct error paths carried per event/snapshot. The true
/// error count lives in `total_errors`; overflow past the cap is dropped
/// from the payload but never from the count.
pub const MAX_REPORTED_ERRORS: usize = 200;
/// Bounded largest-children list attached to every progress event.
pub const TOP_CHILDREN_LIMIT: usize = 20;
/// Finished scans kept for `disk_usage_snapshot` lookups; oldest evicted.
pub const MAX_STORED_SCANS: usize = 8;
/// FER-479: total entries (dirs + files + symlinks) the snapshot store may
/// hold across scans before older scans are evicted (the newest is always
/// kept, whatever its size). Roughly one whole system drive: a 4 M-entry
/// snapshot holds about 1.4 GB.
pub const MAX_STORED_ENTRIES: u64 = 5_000_000;
/// Upper bound for one query page / top list (DSK-03): requests above this
/// are clamped so a single IPC response stays bounded.
pub const MAX_QUERY_LIMIT: u64 = 1000;

/// Coverage of the data reported so far (contract §3.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Coverage {
    /// Walk finished, no errors, no caps — root totals are the real scope.
    Complete,
    /// Errors, a depth cap, or a cancellation made the data incomplete.
    /// Never labelled complete.
    Partial,
    /// Reserved for cached-scan freshness (gap G8, DSK-03/04). A live scan
    /// never emits it.
    Stale,
}

/// Scan parameters. v1 scans exactly ONE root per scan (the root is a
/// single argument — multi-root is out of scope by contract §3.1).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ScanOptions {
    /// Bound recursion depth (`None` = unlimited; D07 no-follow already
    /// makes cycles impossible). Hitting the cap stops descending, keeps
    /// what was counted, and reports coverage `partial` — never silently.
    pub max_depth: Option<usize>,
}

/// One failure, aggregated per path (a path failing repeatedly appears
/// once). The list is capped at [`MAX_REPORTED_ERRORS`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanError {
    pub path: String,
    pub code: ErrorCode,
    pub message: String,
}

/// Aggregates for one directory. Subtree totals = direct entries + all
/// child subtrees; the parent is never re-added at the root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskUsageNode {
    /// Stable path-derived identity (the lossy absolute path itself).
    pub id: String,
    pub path: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    pub children_ids: Vec<String>,
    /// File entries listed directly in this directory.
    pub direct_files: u64,
    /// Symlink/junction entries listed directly in this directory (counted,
    /// never followed — D07).
    pub direct_symlinks: u64,
    pub direct_logical_bytes: u64,
    /// On-disk bytes over this node's DIRECT files. `None` when any direct
    /// file lacked OS allocation data (per-node null semantics, gap G2).
    pub direct_allocated_bytes: Option<u64>,
    pub subtree_files: u64,
    pub subtree_dirs: u64,
    pub subtree_symlinks: u64,
    pub subtree_logical_bytes: u64,
    /// `None` when any file in the subtree lacked allocation data.
    pub subtree_allocated_bytes: Option<u64>,
    pub subtree_errors: u64,
    /// Child directories on other volumes — counted, never crossed (G3).
    pub skipped_volume_boundaries: u64,
    pub subtree_skipped_volumes: u64,
}

/// Final totals for a finished (or cancelled) scan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskUsageStats {
    pub scanned_files: u64,
    /// Directories actually visited (root included).
    pub scanned_dirs: u64,
    pub scanned_symlinks: u64,
    pub skipped_volume_boundaries: u64,
    pub logical_bytes: u64,
    /// Files whose on-disk size the OS could report (Windows).
    pub allocated_available_files: u64,
    /// Files without OS allocation data — the aggregate stays `null`-flagged
    /// instead of pretending (gap G2).
    pub allocated_unavailable_files: u64,
    /// `Some` only while allocation data was available for EVERY file so
    /// far; `None` marks the aggregate unavailable, never fabricated.
    pub known_allocated_bytes: Option<u64>,
    pub total_errors: u64,
    pub coverage: Coverage,
    pub cancelled: bool,
    /// True when `max_depth` stopped the descent before the tree ended.
    pub depth_capped: bool,
    pub duration_ms: u64,
}

/// Which engine produced a scan's events (FER-460). `walk` = the regular
/// directory walker; `ntfsMft` = the elevated helper reading the MFT.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ScanEngine {
    #[default]
    Walk,
    NtfsMft,
    /// FER-476: the FerrumIndexer's index (no elevation).
    Index,
}

/// Progressive payload for `disk_usage://progress`. Batches arrive while the
/// walk runs; exactly ONE final event carries `done: true`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskUsageEvent {
    pub scan_id: String,
    pub scanned_files: u64,
    pub scanned_dirs: u64,
    pub scanned_symlinks: u64,
    pub known_logical_bytes: u64,
    pub known_allocated_bytes: Option<u64>,
    pub allocated_available_files: u64,
    pub allocated_unavailable_files: u64,
    pub coverage: Coverage,
    /// Bounded, per-path-aggregated failures seen so far.
    pub errors: Vec<ScanError>,
    pub total_errors: u64,
    /// Largest top-level children (subtree logical desc, id asc tie-break),
    /// capped at [`TOP_CHILDREN_LIMIT`]. Full-tree querying is DSK-03.
    pub top_children: Vec<DiskUsageNode>,
    /// Only the final event of a scan ever sets this.
    pub done: bool,
    /// True when the walk stopped because of `disk_usage_cancel`; the event
    /// is `done` with coverage `partial`, never `complete`.
    pub cancelled: bool,
    /// Set only on the final event when the scan could not run at all
    /// (e.g. invalid/missing root).
    pub error: Option<String>,
    pub duration_ms: u64,
    /// FER-460: engine behind this event (additive field).
    pub engine: ScanEngine,
    /// FER-460: short user-facing note, e.g. why a requested fast scan fell
    /// back to the regular walk. Never an error by itself.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
    /// FER-460: MFT records read so far / in total while the helper streams
    /// (`ntfsMft` only). Unlike the walk, the MFT's size IS known up front,
    /// so a percent-done of the READ phase is honest (contract §3.5 forbids
    /// it only while the total is unknown). `done == total` = the tree is
    /// being built. Absent on walk events.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mft_records_done: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mft_records_total: Option<u64>,
}

/// Stored result of a finished scan, addressable by `disk_usage_snapshot`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskUsageSnapshot {
    pub scan_id: String,
    pub root_path: String,
    pub root_id: String,
    /// Monotonic snapshot generation (gap G8, DSK-03): query responses carry
    /// it so the UI can preserve selection across progress and flag a changed
    /// generation later. Never reused within a process.
    pub version: u64,
    /// Wall-clock ms (UNIX epoch) when the scan finished.
    pub scanned_at_ms: u64,
    /// Every directory node keyed by its path-derived id.
    pub nodes: HashMap<String, DiskUsageNode>,
    /// Direct files/symlinks per directory id — the query index behind
    /// `disk_usage_query_children` / `disk_usage_top_files` (DSK-03). Not
    /// serialized in snapshot responses; files are exposed only through the
    /// query commands so totals stay aggregation-based.
    #[serde(skip_serializing)]
    pub files_by_parent: HashMap<String, Vec<DiskUsageFileEntry>>,
    pub stats: DiskUsageStats,
}

/// What `disk_usage_snapshot` sends to the frontend (FER-479): the identity
/// of a finished scan, its final stats and the scan ROOT's aggregates — a
/// response of bounded size whatever the tree size. The full node map used
/// to travel here: for a whole system drive (C:\ ≈ 3.8 M entries) that is
/// hundreds of MB of JSON, serialized by a sync command on the main thread
/// and parsed in one go by the WebView — the window went white for minutes.
/// Everything below the root is read level by level through
/// `disk_usage_query_children`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskUsageSnapshotSummary {
    pub scan_id: String,
    pub root_path: String,
    pub root_id: String,
    pub version: u64,
    pub scanned_at_ms: u64,
    /// Scan root aggregates; `children_ids` is always EMPTY on the wire.
    pub root_node: DiskUsageNode,
    pub stats: DiskUsageStats,
}

impl DiskUsageSnapshot {
    /// Bounded projection for the frontend (see [`DiskUsageSnapshotSummary`]).
    pub fn summary(&self) -> DiskUsageSnapshotSummary {
        let mut root_node = self.nodes.get(&self.root_id).cloned().unwrap_or_else(|| {
            LiveState::blank_node(&self.root_id, &self.root_path, &self.root_path, None)
        });
        root_node.children_ids = Vec::new();
        DiskUsageSnapshotSummary {
            scan_id: self.scan_id.clone(),
            root_path: self.root_path.clone(),
            root_id: self.root_id.clone(),
            version: self.version,
            scanned_at_ms: self.scanned_at_ms,
            root_node,
            stats: self.stats.clone(),
        }
    }

    /// Directory nodes + file/symlink entries held: the store's budget unit.
    fn entry_count(&self) -> u64 {
        self.stats.scanned_dirs + self.stats.scanned_files + self.stats.scanned_symlinks
    }
}

/// What `run_scan*` returns to its caller (worker or test).
#[derive(Debug, Clone)]
pub struct ScanOutcome {
    pub scan_id: String,
    pub stats: DiskUsageStats,
    pub snapshot: Arc<DiskUsageSnapshot>,
}

/// Merges allocation totals under per-node null semantics: any `None` side
/// poisons the sum to `None` — never silently swapped for logical bytes.
fn merge_allocated(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.saturating_add(y)),
        _ => None,
    }
}

/// Bounded, per-path-aggregated error log.
#[derive(Default)]
struct ErrorLog {
    ordered: Vec<ScanError>,
    index: HashMap<String, usize>,
    total: u64,
}

impl ErrorLog {
    fn record(&mut self, path: &Path, err: &FsError) {
        self.record_parts(&path.to_string_lossy(), err.code, &err.message);
    }

    /// Same aggregation, from already-flat parts — the parallel workers
    /// stream errors as [`ScanError`] records (no `FsError` round-trip).
    fn record_parts(&mut self, path: &str, code: ErrorCode, message: &str) {
        self.total += 1;
        if self.index.contains_key(path) || self.ordered.len() >= MAX_REPORTED_ERRORS {
            return; // aggregated per path; overflow stays in `total` only
        }
        let key = path.to_string();
        self.index.insert(key.clone(), self.ordered.len());
        self.ordered.push(ScanError {
            path: key,
            code,
            message: message.to_string(),
        });
    }
}

/// Mutable state of one running scan.
struct LiveState {
    root_id: String,
    nodes: HashMap<String, DiskUsageNode>,
    /// Direct file/symlink records per directory id (DSK-03 query index).
    files_by_parent: HashMap<String, Vec<DiskUsageFileEntry>>,
    scanned_files: u64,
    scanned_dirs: u64,
    scanned_symlinks: u64,
    skipped_volume_boundaries: u64,
    logical_bytes: u64,
    allocated_available_files: u64,
    allocated_unavailable_files: u64,
    known_allocated_bytes: Option<u64>,
    errors: ErrorLog,
    depth_capped: bool,
}

impl LiveState {
    fn new(root_id: &str) -> Self {
        Self {
            root_id: root_id.to_string(),
            nodes: HashMap::new(),
            files_by_parent: HashMap::new(),
            scanned_files: 0,
            scanned_dirs: 0,
            scanned_symlinks: 0,
            skipped_volume_boundaries: 0,
            logical_bytes: 0,
            allocated_available_files: 0,
            allocated_unavailable_files: 0,
            known_allocated_bytes: Some(0),
            errors: ErrorLog::default(),
            depth_capped: false,
        }
    }

    fn blank_node(id: &str, path: &str, name: &str, parent_id: Option<String>) -> DiskUsageNode {
        DiskUsageNode {
            id: id.to_string(),
            path: path.to_string(),
            name: name.to_string(),
            parent_id,
            children_ids: Vec::new(),
            direct_files: 0,
            direct_symlinks: 0,
            direct_logical_bytes: 0,
            direct_allocated_bytes: Some(0),
            subtree_files: 0,
            subtree_dirs: 0,
            subtree_symlinks: 0,
            subtree_logical_bytes: 0,
            subtree_allocated_bytes: Some(0),
            subtree_errors: 0,
            skipped_volume_boundaries: 0,
            subtree_skipped_volumes: 0,
        }
    }

    fn create_root(&mut self, id: &str, path: &str, name: &str) {
        self.nodes
            .insert(id.to_string(), Self::blank_node(id, path, name, None));
    }

    fn create_dir_node(&mut self, id: &str, path: &str, name: &str, parent_id: &str) {
        self.nodes.insert(
            id.to_string(),
            Self::blank_node(id, path, name, Some(parent_id.to_string())),
        );
        if let Some(p) = self.nodes.get_mut(parent_id) {
            p.children_ids.push(id.to_string());
        }
        // Descendant dirs roll up into every ancestor's subtree counter.
        self.for_self_and_ancestors(parent_id, |n| n.subtree_dirs += 1);
    }

    /// Applies `f` to the node and every ancestor (walk-up aggregation —
    /// O(depth) per entry, keeps progressive totals always current).
    fn for_self_and_ancestors(&mut self, start: &str, mut f: impl FnMut(&mut DiskUsageNode)) {
        let mut cur = start.to_string();
        while let Some(node) = self.nodes.get_mut(&cur) {
            f(node);
            match node.parent_id.clone() {
                Some(p) => cur = p,
                None => break,
            }
        }
    }

    fn add_file(&mut self, parent_id: &str, name: &str, logical: u64, allocated: Option<u64>) {
        self.scanned_files += 1;
        self.logical_bytes = self.logical_bytes.saturating_add(logical);
        if allocated.is_some() {
            self.allocated_available_files += 1;
        } else {
            self.allocated_unavailable_files += 1;
        }
        self.known_allocated_bytes = merge_allocated(self.known_allocated_bytes, allocated);
        // Direct fields land on the immediate parent only.
        if let Some(node) = self.nodes.get_mut(parent_id) {
            node.direct_files += 1;
            node.direct_logical_bytes = node.direct_logical_bytes.saturating_add(logical);
            node.direct_allocated_bytes = merge_allocated(node.direct_allocated_bytes, allocated);
        }
        // DSK-03 query index: one record per direct file (path-derived ids
        // are rebuilt at query time; only the name is stored per entry).
        self.files_by_parent
            .entry(parent_id.to_string())
            .or_default()
            .push(DiskUsageFileEntry {
                name: name.to_string(),
                logical_bytes: logical,
                allocated_bytes: allocated,
                is_symlink: false,
            });
        // Subtree fields roll up through the whole ancestor chain.
        self.for_self_and_ancestors(parent_id, |n| {
            n.subtree_files += 1;
            n.subtree_logical_bytes = n.subtree_logical_bytes.saturating_add(logical);
            n.subtree_allocated_bytes = merge_allocated(n.subtree_allocated_bytes, allocated);
        });
    }

    fn add_symlink(&mut self, parent_id: &str, name: &str) {
        self.scanned_symlinks += 1;
        if let Some(node) = self.nodes.get_mut(parent_id) {
            node.direct_symlinks += 1;
        }
        // Counted as an entry, never followed (D07): 0 bytes, no allocation
        // data — the query index keeps it listable without following it.
        self.files_by_parent
            .entry(parent_id.to_string())
            .or_default()
            .push(DiskUsageFileEntry {
                name: name.to_string(),
                logical_bytes: 0,
                allocated_bytes: None,
                is_symlink: true,
            });
        self.for_self_and_ancestors(parent_id, |n| n.subtree_symlinks += 1);
    }

    fn skip_volume(&mut self, parent_id: &str) {
        self.skipped_volume_boundaries += 1;
        if let Some(node) = self.nodes.get_mut(parent_id) {
            node.skipped_volume_boundaries += 1;
        }
        self.for_self_and_ancestors(parent_id, |n| n.subtree_skipped_volumes += 1);
    }

    fn add_error(&mut self, chain_start: &str, path: &Path, err: &FsError) {
        self.errors.record(path, err);
        self.for_self_and_ancestors(chain_start, |n| n.subtree_errors += 1);
    }

    fn event(
        &self,
        scan_id: &str,
        done: bool,
        cancelled: bool,
        coverage: Coverage,
        error: Option<String>,
        duration_ms: u64,
    ) -> DiskUsageEvent {
        let mut top_children: Vec<DiskUsageNode> = self
            .nodes
            .get(&self.root_id)
            .map(|r| {
                r.children_ids
                    .iter()
                    .filter_map(|id| self.nodes.get(id).cloned())
                    .collect()
            })
            .unwrap_or_default();
        top_children.sort_by(|a, b| {
            b.subtree_logical_bytes
                .cmp(&a.subtree_logical_bytes)
                .then_with(|| a.id.cmp(&b.id))
        });
        top_children.truncate(TOP_CHILDREN_LIMIT);
        // FER-479: events carry aggregates only; child id lists stay in Rust.
        for c in &mut top_children {
            c.children_ids = Vec::new();
        }
        DiskUsageEvent {
            scan_id: scan_id.to_string(),
            scanned_files: self.scanned_files,
            scanned_dirs: self.scanned_dirs,
            scanned_symlinks: self.scanned_symlinks,
            known_logical_bytes: self.logical_bytes,
            known_allocated_bytes: self.known_allocated_bytes,
            allocated_available_files: self.allocated_available_files,
            allocated_unavailable_files: self.allocated_unavailable_files,
            coverage,
            errors: self.errors.ordered.clone(),
            total_errors: self.errors.total,
            top_children,
            done,
            cancelled,
            error,
            duration_ms,
            engine: ScanEngine::Walk,
            notice: None,
            mft_records_done: None,
            mft_records_total: None,
        }
    }
}

/// Final path segment for display/query rows (falls back to the whole path
/// for drive roots, matching the DSK-02 root name).
fn entry_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// Volume prefix of a path for the cross-volume guard: `C:\dir` → `C:\`
/// (case-insensitive compare). UNC paths and POSIX roots are NOT recognized
/// — documented v1 limitation (gap G9): such scans run without the boundary
/// guard rather than with a wrong one.
pub fn volume_prefix_of(path: &str) -> Option<String> {
    let b = path.as_bytes();
    if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
        Some(format!("{}:\\", b[0] as char))
    } else {
        None
    }
}

/// True when `child` stays on the scan root's volume. `None` root volume
/// (unrecognized prefix / non-Windows) disables the guard — documented.
fn same_volume(root_volume: &Option<String>, child: &Path) -> bool {
    match root_volume {
        None => true,
        Some(v) => {
            volume_prefix_of(&child.to_string_lossy()).is_some_and(|cv| cv.eq_ignore_ascii_case(v))
        }
    }
}

static SCAN_SEQ: AtomicU64 = AtomicU64::new(0);

/// Snapshot generation counter (gap G8): every finished scan gets the next
/// version so query responses can tell generations apart. Starts at 1.
static SNAPSHOT_VERSION_SEQ: AtomicU64 = AtomicU64::new(1);

pub fn next_snapshot_version() -> u64 {
    SNAPSHOT_VERSION_SEQ.fetch_add(1, Ordering::Relaxed)
}

pub fn unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Unique id per start (timestamp + process-wide counter): a second start
/// can never be confused with a first scan's late events (generation guard;
/// the frontend still compares `scanId` on every event).
pub fn new_scan_id() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!(
        "du-{millis:x}-{:04}",
        SCAN_SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

/// Live scans by id so `disk_usage_cancel` can stop the worker's I/O.
pub fn active_scans() -> &'static Mutex<HashMap<String, Arc<AtomicBool>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, Arc<AtomicBool>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Flag a running scan cancelled; the worker stops at the next entry and
/// deregisters itself. Returns false for unknown/already-finished ids.
pub fn cancel_scan(scan_id: &str) -> bool {
    match active_scans()
        .lock()
        .expect("disk usage registry")
        .remove(scan_id)
    {
        Some(flag) => {
            flag.store(true, Ordering::Relaxed);
            true
        }
        None => false,
    }
}

fn snapshot_store() -> &'static Mutex<HashMap<String, Arc<DiskUsageSnapshot>>> {
    static STORE: OnceLock<Mutex<HashMap<String, Arc<DiskUsageSnapshot>>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Oldest scan ids first, for bounded eviction from the snapshot store.
fn stored_order() -> &'static Mutex<VecDeque<String>> {
    static ORDER: OnceLock<Mutex<VecDeque<String>>> = OnceLock::new();
    ORDER.get_or_init(|| Mutex::new(VecDeque::new()))
}

/// Keep a finished scan addressable by `disk_usage_snapshot(scan_id)`
/// (bounded: oldest evicted past [`MAX_STORED_SCANS`]).
pub fn store_snapshot(snap: DiskUsageSnapshot) {
    store_snapshot_arc(Arc::new(snap));
}

/// Arc handover variant — lets the scan worker store its outcome WITHOUT a
/// deep clone of every node/map (FER-407: that clone ran after the `done`
/// event was already visible to the frontend and widened the NotFound window
/// on large trees by hundreds of milliseconds). Same bounded eviction.
pub fn store_snapshot_arc(snap: Arc<DiskUsageSnapshot>) {
    let mut order = stored_order().lock().expect("snapshot order");
    if !order.contains(&snap.scan_id) {
        order.push_back(snap.scan_id.clone());
    }
    snapshot_store()
        .lock()
        .expect("snapshot store")
        .insert(snap.scan_id.clone(), snap);
    // FER-479: evict oldest-first past the count cap OR the entry budget
    // (eight whole-drive snapshots would exhaust RAM); the newest scan
    // always stays, whatever its size.
    let mut store = snapshot_store().lock().expect("snapshot store");
    let held = |store: &HashMap<String, Arc<DiskUsageSnapshot>>| -> u64 {
        store.values().map(|s| s.entry_count()).sum()
    };
    while order.len() > MAX_STORED_SCANS || (order.len() > 1 && held(&store) > MAX_STORED_ENTRIES) {
        let oldest = order.pop_front().expect("non-empty order");
        store.remove(&oldest);
    }
}

/// Bounded summary of a stored scan for `disk_usage_snapshot` (FER-479).
pub fn snapshot_summary(scan_id: &str) -> Option<DiskUsageSnapshotSummary> {
    snapshot(scan_id).map(|s| s.summary())
}

/// The stored snapshot of a finished scan, if still retained.
pub fn snapshot(scan_id: &str) -> Option<Arc<DiskUsageSnapshot>> {
    snapshot_store()
        .lock()
        .expect("snapshot store")
        .get(scan_id)
        .cloned()
}

// ── DSK-03 (FER-321): largest-first query, top files, classification ─────

/// Byte metric a query sorts / thresholds / percentages by (DSK-03.2). The
/// selector never changes what is stored: `allocated` stays `null` per node
/// when the OS had no on-disk size — never substituted with logical (§2.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SizeMetric {
    #[default]
    Logical,
    Allocated,
}

/// Coarse file classification by extension (DSK-03.2) — simple and
/// data-driven ([`CATEGORY_EXTENSIONS`]); unknown or missing extension is
/// `other`. Directories and symlinks are never classified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FileCategory {
    Video,
    Image,
    Audio,
    Document,
    Archive,
    Other,
}

/// (extension, category) pairs — lowercase, no leading dot; the single
/// source of truth behind [`category_of`]. Keep alphabetical per category.
const CATEGORY_EXTENSIONS: &[(&str, FileCategory)] = &[
    ("3gp", FileCategory::Video),
    ("avi", FileCategory::Video),
    ("flv", FileCategory::Video),
    ("m4v", FileCategory::Video),
    ("mkv", FileCategory::Video),
    ("mov", FileCategory::Video),
    ("mp4", FileCategory::Video),
    ("mpeg", FileCategory::Video),
    ("mpg", FileCategory::Video),
    ("ts", FileCategory::Video),
    ("webm", FileCategory::Video),
    ("wmv", FileCategory::Video),
    ("bmp", FileCategory::Image),
    ("gif", FileCategory::Image),
    ("heic", FileCategory::Image),
    ("ico", FileCategory::Image),
    ("jpeg", FileCategory::Image),
    ("jpg", FileCategory::Image),
    ("png", FileCategory::Image),
    ("svg", FileCategory::Image),
    ("tif", FileCategory::Image),
    ("tiff", FileCategory::Image),
    ("webp", FileCategory::Image),
    ("aac", FileCategory::Audio),
    ("flac", FileCategory::Audio),
    ("m4a", FileCategory::Audio),
    ("mid", FileCategory::Audio),
    ("midi", FileCategory::Audio),
    ("mp3", FileCategory::Audio),
    ("ogg", FileCategory::Audio),
    ("opus", FileCategory::Audio),
    ("wav", FileCategory::Audio),
    ("wma", FileCategory::Audio),
    ("csv", FileCategory::Document),
    ("doc", FileCategory::Document),
    ("docx", FileCategory::Document),
    ("epub", FileCategory::Document),
    ("md", FileCategory::Document),
    ("ods", FileCategory::Document),
    ("odt", FileCategory::Document),
    ("pdf", FileCategory::Document),
    ("ppt", FileCategory::Document),
    ("pptx", FileCategory::Document),
    ("rtf", FileCategory::Document),
    ("txt", FileCategory::Document),
    ("xls", FileCategory::Document),
    ("xlsx", FileCategory::Document),
    ("7z", FileCategory::Archive),
    ("bz2", FileCategory::Archive),
    ("cab", FileCategory::Archive),
    ("gz", FileCategory::Archive),
    ("iso", FileCategory::Archive),
    ("rar", FileCategory::Archive),
    ("tar", FileCategory::Archive),
    ("xz", FileCategory::Archive),
    ("zip", FileCategory::Archive),
    ("zst", FileCategory::Archive),
];

fn category_map() -> &'static HashMap<&'static str, FileCategory> {
    static MAP: OnceLock<HashMap<&'static str, FileCategory>> = OnceLock::new();
    MAP.get_or_init(|| CATEGORY_EXTENSIONS.iter().copied().collect())
}

/// Classify a file NAME by its extension, case-insensitively (DSK-03.2).
/// No extension / unknown extension → [`FileCategory::Other`].
pub fn category_of(name: &str) -> FileCategory {
    Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .and_then(|e| category_map().get(e.as_str()).copied())
        .unwrap_or(FileCategory::Other)
}

/// Sort order for `disk_usage_query_children`. Metric sorts tie-break by the
/// stable path-derived id ascending so every page is reproducible (§3.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DiskUsageSort {
    #[default]
    LargestFirst,
    SmallestFirst,
    NameAsc,
}

/// Optional query filters (DSK-03.2) — all present fields are ANDed.
/// `metric` selects the byte metric for sort/threshold/percent;
/// `extensions`/`categories` match files only; `nameSubstring`/`minBytes`
/// apply to every entry kind.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DiskUsageFilter {
    pub metric: SizeMetric,
    /// Minimum size under the selected metric. Entries with unknown
    /// allocated bytes FAIL the threshold — unknown is never guessed.
    pub min_bytes: Option<u64>,
    /// Case-insensitive substring match on the entry name.
    pub name_substring: Option<String>,
    /// File extensions, with or without the leading dot, case-insensitive.
    pub extensions: Option<Vec<String>>,
    pub categories: Option<Vec<FileCategory>>,
}

/// Kind of one child row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DiskUsageEntryKind {
    Dir,
    File,
    Symlink,
}

/// Stored record of one direct file/symlink of a directory — the per-scan
/// query index. Symlinks are kept with 0 bytes and no allocation data:
/// counted, never followed (D07).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskUsageFileEntry {
    pub name: String,
    pub logical_bytes: u64,
    /// `None` when the OS had no on-disk size for this file (gap G2).
    pub allocated_bytes: Option<u64>,
    pub is_symlink: bool,
}

/// One row of `disk_usage_query_children`: a direct child directory (its
/// subtree aggregates), direct file, or direct symlink of the queried node.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskUsageChildItem {
    pub id: String,
    pub name: String,
    pub kind: DiskUsageEntryKind,
    /// Logical bytes: subtree total for dirs, file length for files, 0 for
    /// symlinks (never followed).
    pub logical_bytes: u64,
    /// On-disk bytes; `null` = unavailable (per-node null semantics, gap
    /// G2) — never logical bytes in disguise.
    pub allocated_bytes: Option<u64>,
    /// False exactly when `allocated_bytes` is `null`.
    pub allocated_supported: bool,
    /// Extension classification; `other` for dirs/symlinks.
    pub category: FileCategory,
    /// Percent of the QUERIED node's total under the selected metric.
    /// `null` on partial coverage, zero denominator, or unknown value —
    /// the UI renders "—", never 0% (§2.7).
    pub percent_of_parent: Option<f64>,
    /// Percent of the scan ROOT's total — computed independently of
    /// `percent_of_parent` (§2.7); same null rules.
    pub percent_of_scan_root: Option<f64>,
    /// FER-479: subtree counts of a child DIRECTORY (absent for files and
    /// symlinks), so the tree renders counts and error badges from the page
    /// itself instead of a client-side copy of the whole node map.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subtree_files: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subtree_dirs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subtree_errors: Option<u64>,
}

/// Page of `disk_usage_query_children` (largest-first by default, stable
/// id-asc tie-break, §3.8).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskUsageChildrenPage {
    pub scan_id: String,
    pub node_id: String,
    /// Snapshot generation — keep selections while it is unchanged (G8).
    pub version: u64,
    /// Wall-clock ms when the scan finished.
    pub scanned_at_ms: u64,
    pub metric: SizeMetric,
    pub coverage: Coverage,
    pub offset: u64,
    /// Applied page size (clamped to [`MAX_QUERY_LIMIT`]).
    pub limit: u64,
    /// Children matching the filter — the paging denominator.
    pub total: u64,
    pub items: Vec<DiskUsageChildItem>,
}

/// One row of `disk_usage_top_files` — files only, whole subtree of the
/// queried node.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskUsageFileItem {
    pub id: String,
    pub parent_id: String,
    pub name: String,
    pub logical_bytes: u64,
    pub allocated_bytes: Option<u64>,
    /// False exactly when `allocated_bytes` is `null`.
    pub allocated_supported: bool,
    pub category: FileCategory,
    /// Percent of the file's DIRECT parent directory total (selected
    /// metric); same null rules as child items.
    pub percent_of_parent: Option<f64>,
    /// Percent of the scan ROOT's total (independent denominator, §2.7).
    pub percent_of_scan_root: Option<f64>,
}

/// Result of `disk_usage_top_files`: the N largest files of the whole
/// subtree, largest-first with id-asc tie-break. `truncated` + `label` make
/// the cap explicit — a "Top N" list is never a claim of all results (§3.8).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskUsageTopFiles {
    pub scan_id: String,
    pub node_id: String,
    pub version: u64,
    pub scanned_at_ms: u64,
    pub metric: SizeMetric,
    pub coverage: Coverage,
    /// Applied limit (clamped to [`MAX_QUERY_LIMIT`]).
    pub limit: u64,
    /// Files matching the filter in the whole subtree, before the cap.
    pub total_matching: u64,
    pub truncated: bool,
    /// Display label, e.g. "Top 20" — render verbatim; NOT an exhaustive
    /// result list.
    pub label: String,
    pub items: Vec<DiskUsageFileItem>,
}

/// Value of an entry under the selected metric. `None` only for allocated
/// with unavailable data — the payload keeps `null`, no substitution (G2).
fn metric_value(logical: u64, allocated: Option<u64>, metric: SizeMetric) -> Option<u64> {
    match metric {
        SizeMetric::Logical => Some(logical),
        SizeMetric::Allocated => allocated,
    }
}

/// Percentage under contract §2.7: `None` (UI renders "—") whenever the
/// scan coverage is not complete, the denominator is 0, or the value is
/// unknown. NEVER a fabricated 0%.
fn percent_value(
    numerator: Option<u64>,
    denominator: Option<u64>,
    coverage: Coverage,
) -> Option<f64> {
    if coverage != Coverage::Complete {
        return None;
    }
    match (numerator, denominator) {
        (Some(n), Some(d)) if d > 0 => Some((n as f64 / d as f64) * 100.0),
        _ => None,
    }
}

fn normalize_extension(ext: &str) -> String {
    ext.trim().trim_start_matches('.').to_ascii_lowercase()
}

fn file_extension_of(name: &str) -> Option<String> {
    Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
}

/// Uniform candidate row shared by both queries before response mapping.
struct Candidate {
    id: String,
    parent_id: String,
    name: String,
    kind: DiskUsageEntryKind,
    logical_bytes: u64,
    allocated_bytes: Option<u64>,
    category: FileCategory,
}

impl Candidate {
    fn metric(&self, metric: SizeMetric) -> Option<u64> {
        metric_value(self.logical_bytes, self.allocated_bytes, metric)
    }
}

/// Pre-normalized [`DiskUsageFilter`] (lowercased once, not per entry).
struct FilterMatcher {
    metric: SizeMetric,
    min_bytes: Option<u64>,
    name_substring: Option<String>,
    extensions: Option<Vec<String>>,
    categories: Option<Vec<FileCategory>>,
}

impl FilterMatcher {
    fn new(filter: &DiskUsageFilter) -> Self {
        Self {
            metric: filter.metric,
            min_bytes: filter.min_bytes,
            name_substring: filter.name_substring.as_ref().map(|s| s.to_lowercase()),
            extensions: filter
                .extensions
                .as_ref()
                .map(|v| v.iter().map(|e| normalize_extension(e)).collect()),
            categories: filter.categories.clone(),
        }
    }

    fn matches(&self, c: &Candidate) -> bool {
        self.matches_parts(c.kind, &c.name, c.metric(self.metric), || c.category)
    }

    /// [`Self::matches`] without a materialized [`Candidate`] (FER-479: the
    /// recursive top-files scan checks millions of entries and must not
    /// allocate id strings for the ones that never make the top N).
    fn matches_parts(
        &self,
        kind: DiskUsageEntryKind,
        name: &str,
        metric: Option<u64>,
        category: impl FnOnce() -> FileCategory,
    ) -> bool {
        if let Some(min) = self.min_bytes {
            match metric {
                Some(v) if v >= min => {}
                _ => return false, // includes unknown allocated: never guessed
            }
        }
        if let Some(needle) = &self.name_substring
            && !name.to_lowercase().contains(needle)
        {
            return false;
        }
        if let Some(exts) = &self.extensions {
            // Extension/category filters describe files; dirs and symlinks
            // are dropped when either is set.
            let hit = kind == DiskUsageEntryKind::File
                && file_extension_of(name).is_some_and(|e| exts.contains(&e));
            if !hit {
                return false;
            }
        }
        if let Some(cats) = &self.categories
            && (kind != DiskUsageEntryKind::File || !cats.contains(&category()))
        {
            return false;
        }
        true
    }
}

/// Deterministic ordering: metric sorts tie-break by stable id asc (§3.8);
/// unknown allocated bytes sort as the smallest side — ordering only, the
/// payload keeps `null` (gap G2).
fn sort_candidates(items: &mut [Candidate], sort: DiskUsageSort, metric: SizeMetric) {
    items.sort_by(|a, b| match sort {
        DiskUsageSort::LargestFirst => b
            .metric(metric)
            .unwrap_or(0)
            .cmp(&a.metric(metric).unwrap_or(0))
            .then_with(|| a.id.cmp(&b.id)),
        DiskUsageSort::SmallestFirst => a
            .metric(metric)
            .unwrap_or(0)
            .cmp(&b.metric(metric).unwrap_or(0))
            .then_with(|| a.id.cmp(&b.id)),
        DiskUsageSort::NameAsc => a
            .name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.id.cmp(&b.id)),
    });
}

fn node_not_in_snapshot(node_id: &str) -> FsError {
    FsError::new(
        ErrorCode::NotFound,
        "Node is not part of this scan snapshot — select within the scanned tree",
    )
    .with_path(node_id)
}

fn scan_not_found(scan_id: &str) -> FsError {
    FsError::new(
        ErrorCode::NotFound,
        "Disk usage scan is unknown or evicted from the bounded store — rescan required",
    )
    .with_path(scan_id)
}

/// Direct children of ONE node from a stored snapshot: child directories
/// (subtree aggregates) + direct files + direct symlinks. Paginated,
/// largest-first by the selected metric with stable id tie-break (§3.8).
/// Pure snapshot read — the filesystem is never re-walked, so totals cannot
/// inflate.
pub fn query_children_snapshot(
    snap: &DiskUsageSnapshot,
    node_id: &str,
    offset: u64,
    limit: u64,
    sort: DiskUsageSort,
    filter: &DiskUsageFilter,
) -> Result<DiskUsageChildrenPage, FsError> {
    let node = snap
        .nodes
        .get(node_id)
        .ok_or_else(|| node_not_in_snapshot(node_id))?;
    let limit = limit.min(MAX_QUERY_LIMIT);
    let matcher = FilterMatcher::new(filter);

    let mut candidates: Vec<Candidate> = Vec::new();
    for id in &node.children_ids {
        if let Some(child) = snap.nodes.get(id) {
            candidates.push(Candidate {
                id: child.id.clone(),
                parent_id: node_id.to_string(),
                name: child.name.clone(),
                kind: DiskUsageEntryKind::Dir,
                logical_bytes: child.subtree_logical_bytes,
                allocated_bytes: child.subtree_allocated_bytes,
                category: FileCategory::Other,
            });
        }
    }
    if let Some(entries) = snap.files_by_parent.get(node_id) {
        for entry in entries {
            let kind = if entry.is_symlink {
                DiskUsageEntryKind::Symlink
            } else {
                DiskUsageEntryKind::File
            };
            candidates.push(Candidate {
                id: Path::new(node_id)
                    .join(&entry.name)
                    .to_string_lossy()
                    .into_owned(),
                parent_id: node_id.to_string(),
                name: entry.name.clone(),
                kind,
                logical_bytes: entry.logical_bytes,
                allocated_bytes: entry.allocated_bytes,
                category: if entry.is_symlink {
                    FileCategory::Other
                } else {
                    category_of(&entry.name)
                },
            });
        }
    }

    candidates.retain(|c| matcher.matches(c));
    let total = candidates.len() as u64;
    sort_candidates(&mut candidates, sort, filter.metric);

    let start = usize::try_from(offset.min(total)).unwrap_or(usize::MAX);
    let end = start
        .saturating_add(usize::try_from(limit).unwrap_or(0))
        .min(candidates.len());

    let coverage = snap.stats.coverage;
    let parent_denominator = metric_value(
        node.subtree_logical_bytes,
        node.subtree_allocated_bytes,
        filter.metric,
    );
    let root_denominator = snap.nodes.get(&snap.root_id).and_then(|root| {
        metric_value(
            root.subtree_logical_bytes,
            root.subtree_allocated_bytes,
            filter.metric,
        )
    });
    let items = candidates[start..end]
        .iter()
        .map(|c| {
            let dir = if c.kind == DiskUsageEntryKind::Dir {
                snap.nodes.get(&c.id)
            } else {
                None
            };
            DiskUsageChildItem {
                id: c.id.clone(),
                name: c.name.clone(),
                kind: c.kind,
                logical_bytes: c.logical_bytes,
                allocated_bytes: c.allocated_bytes,
                allocated_supported: c.allocated_bytes.is_some(),
                category: c.category,
                percent_of_parent: percent_value(
                    c.metric(filter.metric),
                    parent_denominator,
                    coverage,
                ),
                percent_of_scan_root: percent_value(
                    c.metric(filter.metric),
                    root_denominator,
                    coverage,
                ),
                subtree_files: dir.map(|d| d.subtree_files),
                subtree_dirs: dir.map(|d| d.subtree_dirs),
                subtree_errors: dir.map(|d| d.subtree_errors),
            }
        })
        .collect();
    Ok(DiskUsageChildrenPage {
        scan_id: snap.scan_id.clone(),
        node_id: node_id.to_string(),
        version: snap.version,
        scanned_at_ms: snap.scanned_at_ms,
        metric: filter.metric,
        coverage,
        offset,
        limit,
        total,
        items,
    })
}

/// The N largest FILES of a whole subtree, recursive, read purely from the
/// snapshot. Top-N with `truncated` + "Top N" label — never a claim of all
/// results (§3.8). Symlinks are excluded (never followed, D07).
pub fn top_files_snapshot(
    snap: &DiskUsageSnapshot,
    node_id: &str,
    limit: u64,
    filter: &DiskUsageFilter,
) -> Result<DiskUsageTopFiles, FsError> {
    if !snap.nodes.contains_key(node_id) {
        return Err(node_not_in_snapshot(node_id));
    }
    let limit = limit.min(MAX_QUERY_LIMIT);
    let matcher = FilterMatcher::new(filter);

    // Iterative subtree walk (same discipline as the scanner — explicit
    // stack, no recursion, ids are tree edges so no cycle is possible).
    //
    // FER-479: bounded top-N selection. The root of a whole-drive scan has
    // millions of files; materializing a Candidate (two path strings) per
    // file just to keep N took seconds and hundreds of MB. Only entries that
    // can still enter the current top N are materialized, and the buffer is
    // pruned back to N whenever it doubles: memory stays O(N) and the result
    // is exactly the full sort's (metric desc, id asc — §3.8).
    let keep = usize::try_from(limit).unwrap_or(usize::MAX);
    let prune_at = keep.saturating_mul(2).max(64);
    let mut candidates: Vec<Candidate> = Vec::new();
    let mut total_matching = 0u64;
    // Smallest metric inside a FULL top N (None until it first fills).
    let mut floor: Option<u64> = None;
    let mut stack: Vec<&str> = vec![node_id];
    while let Some(dir_id) = stack.pop() {
        if let Some(dir) = snap.nodes.get(dir_id) {
            stack.extend(dir.children_ids.iter().map(String::as_str));
        }
        if let Some(entries) = snap.files_by_parent.get(dir_id) {
            for entry in entries {
                if entry.is_symlink {
                    continue; // files only: a link's payload is not its own
                }
                let value = metric_value(entry.logical_bytes, entry.allocated_bytes, filter.metric);
                if !matcher.matches_parts(DiskUsageEntryKind::File, &entry.name, value, || {
                    category_of(&entry.name)
                }) {
                    continue;
                }
                total_matching += 1;
                if keep == 0 || floor.is_some_and(|f| value.unwrap_or(0) < f) {
                    continue; // can never make the top N
                }
                candidates.push(Candidate {
                    id: Path::new(dir_id)
                        .join(&entry.name)
                        .to_string_lossy()
                        .into_owned(),
                    parent_id: dir_id.to_string(),
                    name: entry.name.clone(),
                    kind: DiskUsageEntryKind::File,
                    logical_bytes: entry.logical_bytes,
                    allocated_bytes: entry.allocated_bytes,
                    category: category_of(&entry.name),
                });
                if candidates.len() >= prune_at {
                    sort_candidates(&mut candidates, DiskUsageSort::LargestFirst, filter.metric);
                    candidates.truncate(keep);
                    floor = candidates
                        .last()
                        .map(|c| c.metric(filter.metric).unwrap_or(0));
                }
            }
        }
    }

    sort_candidates(&mut candidates, DiskUsageSort::LargestFirst, filter.metric);
    let truncated = total_matching > limit;
    candidates.truncate(keep);

    let coverage = snap.stats.coverage;
    let root_denominator = snap.nodes.get(&snap.root_id).and_then(|root| {
        metric_value(
            root.subtree_logical_bytes,
            root.subtree_allocated_bytes,
            filter.metric,
        )
    });
    let items = candidates
        .iter()
        .map(|c| {
            // percent-of-parent uses the file's DIRECT parent directory
            // total; percent-of-scan-root the root total — independent
            // denominators (§2.7).
            let parent_denominator = snap.nodes.get(&c.parent_id).and_then(|p| {
                metric_value(
                    p.subtree_logical_bytes,
                    p.subtree_allocated_bytes,
                    filter.metric,
                )
            });
            DiskUsageFileItem {
                id: c.id.clone(),
                parent_id: c.parent_id.clone(),
                name: c.name.clone(),
                logical_bytes: c.logical_bytes,
                allocated_bytes: c.allocated_bytes,
                allocated_supported: c.allocated_bytes.is_some(),
                category: c.category,
                percent_of_parent: percent_value(
                    c.metric(filter.metric),
                    parent_denominator,
                    coverage,
                ),
                percent_of_scan_root: percent_value(
                    c.metric(filter.metric),
                    root_denominator,
                    coverage,
                ),
            }
        })
        .collect();
    Ok(DiskUsageTopFiles {
        scan_id: snap.scan_id.clone(),
        node_id: node_id.to_string(),
        version: snap.version,
        scanned_at_ms: snap.scanned_at_ms,
        metric: filter.metric,
        coverage,
        limit,
        total_matching,
        truncated,
        label: format!("Top {limit}"),
        items,
    })
}

/// [`query_children_snapshot`] against the stored snapshot of `scan_id`.
/// Unknown or evicted ids are a structured `NotFound` (bounded store) — the
/// UI answers with a rescan (gap G8).
pub fn query_children_by_scan(
    scan_id: &str,
    node_id: &str,
    offset: u64,
    limit: u64,
    sort: DiskUsageSort,
    filter: &DiskUsageFilter,
) -> Result<DiskUsageChildrenPage, FsError> {
    let snap = snapshot(scan_id).ok_or_else(|| scan_not_found(scan_id))?;
    query_children_snapshot(&snap, node_id, offset, limit, sort, filter)
}

/// [`top_files_snapshot`] against the stored snapshot of `scan_id`, with the
/// same structured `NotFound` on unknown/evicted ids (gap G8).
pub fn top_files_by_scan(
    scan_id: &str,
    node_id: &str,
    limit: u64,
    filter: &DiskUsageFilter,
) -> Result<DiskUsageTopFiles, FsError> {
    let snap = snapshot(scan_id).ok_or_else(|| scan_not_found(scan_id))?;
    top_files_snapshot(&snap, node_id, limit, filter)
}

/// One streaming scan with the default event throttle and a fresh scan id.
pub fn run_scan(
    root: &str,
    options: &ScanOptions,
    cancel: &AtomicBool,
    sink: impl FnMut(&DiskUsageEvent),
) -> Result<ScanOutcome, FsError> {
    run_scan_with_throttle(
        root,
        options,
        cancel,
        &new_scan_id(),
        Some(PROGRESS_INTERVAL),
        sink,
    )
}

/// Core walk with injectable scan id and throttle (`None` disables
/// intermediate events entirely — only the final `done` event is emitted),
/// so tests can observe the stream deterministically.
///
/// DSK-09: this shipping entry point dispatches to the parallel pool with
/// [`default_worker_count()`] scanning workers; the calling thread stays the
/// aggregator (it owns the live state and the sink). Semantics are
/// unchanged: iterative, metadata-only, continues past per-entry failures,
/// cancellation checked per entry, exactly one final `done` event, and
/// totals identical to the serial reference for any worker count.
#[allow(clippy::too_many_arguments)]
pub fn run_scan_with_throttle(
    root: &str,
    options: &ScanOptions,
    cancel: &AtomicBool,
    scan_id: &str,
    throttle: Option<Duration>,
    sink: impl FnMut(&DiskUsageEvent),
) -> Result<ScanOutcome, FsError> {
    run_scan_with_workers(
        root,
        options,
        cancel,
        scan_id,
        throttle,
        default_worker_count(),
        sink,
    )
}

/// DSK-09 knob: run the walk with an explicit worker count (tests /
/// benchmarks). `workers <= 1` runs [`run_scan_serial_with_throttle`] —
/// the previous single-threaded walker verbatim, whose output every other
/// count must match. Values above [`MAX_SCAN_WORKERS`] are clamped to it.
#[allow(clippy::too_many_arguments)]
pub fn run_scan_with_workers(
    root: &str,
    options: &ScanOptions,
    cancel: &AtomicBool,
    scan_id: &str,
    throttle: Option<Duration>,
    workers: usize,
    sink: impl FnMut(&DiskUsageEvent),
) -> Result<ScanOutcome, FsError> {
    if workers <= 1 {
        run_scan_serial_with_throttle(root, options, cancel, scan_id, throttle, sink)
    } else {
        run_scan_pool(
            root,
            options,
            cancel,
            scan_id,
            throttle,
            workers.min(MAX_SCAN_WORKERS),
            sink,
        )
    }
}

/// Upper bound on scanning workers (DSK-09): more threads stop helping long
/// before this on any rotational or AV-filtered drive, and the cap bounds
/// per-pool thread/stack overhead.
pub const MAX_SCAN_WORKERS: usize = 16;

/// DSK-09 default parallelism: every visible hardware thread, clamped to
/// `1..=[MAX_SCAN_WORKERS]` (never zero, even if the OS refuses to tell).
pub fn default_worker_count() -> usize {
    thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .clamp(1, MAX_SCAN_WORKERS)
}

/// FER-460: the walker's root rules, for the fast path to apply BEFORE it
/// asks for elevation.
pub fn validate_root_for_scan(root: &str) -> Result<(), FsError> {
    validate_scan_root(root).map(|_| ())
}

/// Shared root validation for both walkers — identical rejections and error
/// codes as the pre-DSK-09 walker: valid path, exists, NOT a symlink (D07:
/// a link root is rejected, never followed), IS a directory.
fn validate_scan_root(root: &str) -> Result<PathBuf, FsError> {
    validate_path(root)?;
    let root_path = PathBuf::from(root);
    let lm = fs::symlink_metadata(&root_path).map_err(|e| FsError::from(e).with_path(root))?;
    if lm.is_symlink() {
        return Err(FsError::new(
            ErrorCode::InvalidPath,
            "Scan root is a symlink/junction and is never followed (D07)",
        )
        .with_path(root));
    }
    if !lm.is_dir() {
        return Err(FsError::new(
            ErrorCode::NotADirectory,
            "Scan root is a file, not a directory",
        )
        .with_path(root));
    }
    Ok(root_path)
}

/// Shared tail for both walkers: final stats, exactly one final `done`
/// event, snapshot assembly (identical to the pre-DSK-09 walk tail). The
/// caller still hands the outcome to `ipc::finish_scan`, which stores the
/// snapshot (by `Arc`) BEFORE the done event reaches the frontend (FER-407).
fn finalize_scan(
    st: LiveState,
    root: &str,
    scan_id: &str,
    cancelled: bool,
    start: Instant,
    mut sink: impl FnMut(&DiskUsageEvent),
) -> ScanOutcome {
    let duration_ms = start.elapsed().as_millis() as u64;
    let coverage = if cancelled || st.errors.total > 0 || st.depth_capped {
        Coverage::Partial
    } else {
        Coverage::Complete
    };
    let stats = DiskUsageStats {
        scanned_files: st.scanned_files,
        scanned_dirs: st.scanned_dirs,
        scanned_symlinks: st.scanned_symlinks,
        skipped_volume_boundaries: st.skipped_volume_boundaries,
        logical_bytes: st.logical_bytes,
        allocated_available_files: st.allocated_available_files,
        allocated_unavailable_files: st.allocated_unavailable_files,
        known_allocated_bytes: st.known_allocated_bytes,
        total_errors: st.errors.total,
        coverage,
        cancelled,
        depth_capped: st.depth_capped,
        duration_ms,
    };
    // Exactly one final event per scan; only this one carries done: true.
    sink(&st.event(scan_id, true, cancelled, stats.coverage, None, duration_ms));
    let snapshot = DiskUsageSnapshot {
        scan_id: scan_id.to_string(),
        root_path: root.to_string(),
        root_id: st.root_id,
        version: next_snapshot_version(),
        scanned_at_ms: unix_millis(),
        nodes: st.nodes,
        files_by_parent: st.files_by_parent,
        stats: stats.clone(),
    };
    ScanOutcome {
        scan_id: scan_id.to_string(),
        stats,
        snapshot: Arc::new(snapshot),
    }
}

/// The pre-DSK-09 single-threaded walker, VERBATIM (explicit LIFO stack —
/// no stack overflow on deep trees; metadata-only; continues past
/// per-entry failures; cancellation checked before every entry so
/// `cancel_scan` stops I/O promptly). `workers = 1` runs exactly this so
/// the pool has a byte-identical reference, and the benchmark harness
/// drives it directly for honest before/after numbers in one run.
#[allow(clippy::too_many_arguments)]
pub fn run_scan_serial_with_throttle(
    root: &str,
    options: &ScanOptions,
    cancel: &AtomicBool,
    scan_id: &str,
    throttle: Option<Duration>,
    mut sink: impl FnMut(&DiskUsageEvent),
) -> Result<ScanOutcome, FsError> {
    let root_path = validate_scan_root(root)?;
    let root_volume = volume_prefix_of(root);
    let root_id = root.to_string();
    let root_name = entry_name(Path::new(root));
    let mut st = LiveState::new(&root_id);
    st.create_root(&root_id, root, &root_name);

    let start = Instant::now();
    let mut last_emit: Option<Instant> = None;
    let mut cancelled = false;
    // (dir, depth, parent id): no recursion, and symlinks never enter the
    // stack, so no loop is possible (D07).
    let mut stack: Vec<(PathBuf, usize, Option<String>)> = vec![(root_path, 0, None)];

    'outer: while let Some((dir, depth, parent_id)) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            cancelled = true;
            break;
        }
        st.scanned_dirs += 1;
        let node_id = dir.to_string_lossy().into_owned();
        if let Some(parent) = &parent_id {
            let name = dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| node_id.clone());
            st.create_dir_node(&node_id, &node_id, &name, parent);
        }
        let read = match fs::read_dir(&dir) {
            Ok(read) => read,
            Err(e) => {
                // Permission denied on the directory itself: recorded, the
                // walk continues, coverage goes partial (never 0 bytes).
                st.add_error(&node_id, &dir, &FsError::from(e));
                continue;
            }
        };
        for item in read {
            if cancel.load(Ordering::Relaxed) {
                cancelled = true;
                break 'outer;
            }
            let child = match item {
                Ok(c) => c,
                Err(e) => {
                    st.add_error(&node_id, &dir, &FsError::from(e));
                    continue;
                }
            };
            let child_path = child.path();
            match fs::symlink_metadata(&child_path) {
                // Deleted-during-scan and permission denied share this path:
                // recorded per path, the walk continues (contract §3.6).
                Err(e) => st.add_error(&node_id, &child_path, &FsError::from(e)),
                Ok(clm) => {
                    let is_link = clm.is_symlink();
                    if is_link {
                        st.add_symlink(&node_id, &entry_name(&child_path));
                    } else if clm.is_dir() {
                        let within_volume = same_volume(&root_volume, &child_path);
                        let depth_ok = options.max_depth.is_none_or(|md| depth < md);
                        if within_volume && depth_ok {
                            stack.push((child_path, depth + 1, Some(node_id.clone())));
                        } else if !within_volume {
                            // Other volumes are counted as entries but never
                            // crossed (gap G3).
                            st.skip_volume(&node_id);
                        } else {
                            st.depth_capped = true;
                        }
                    } else {
                        let logical = clm.len();
                        let (alloc, supported) =
                            crate::properties::allocated_size(&child_path, logical);
                        st.add_file(
                            &node_id,
                            &entry_name(&child_path),
                            logical,
                            if supported { Some(alloc) } else { None },
                        );
                    }
                }
            }
            if let Some(interval) = throttle {
                let due = last_emit.is_none_or(|t| t.elapsed() >= interval);
                if due {
                    last_emit = Some(Instant::now());
                    sink(&st.event(
                        scan_id,
                        false,
                        false,
                        Coverage::Partial,
                        None,
                        start.elapsed().as_millis() as u64,
                    ));
                }
            }
        }
    }

    Ok(finalize_scan(st, root, scan_id, cancelled, start, sink))
}

// ── DSK-09 (FER-405): parallel worker pool ────────────────────────────────

/// Entries per streamed chunk — simple backpressure: a worker holds at most
/// one in-flight chunk per directory plus its current accumulation, so the
/// worker→aggregator backlog stays bounded even on a directory with a huge
/// single listing.
const BATCH_CHUNK: usize = 4096;

/// Cap on the shared directory queue. Beyond this, newly discovered child
/// directories wait in the aggregator's deferred frontier and are released
/// as the queue drains — the queued-path memory stays bounded instead of
/// mirroring the whole tree up front. (`MAX_QUEUED_DIRS` tasks ≈ a few MB.)
const MAX_QUEUED_DIRS: usize = 8192;

/// How often the aggregator wakes to re-check the cancel flag while waiting
/// for chunks (the workers' per-entry checks stop the I/O regardless; this
/// only bounds how soon the aggregator notices — keeps cancel latency in
/// the millisecond range, matching the old per-entry checks).
const POOL_POLL: Duration = Duration::from_millis(1);

/// One directory to scan: where, how deep, and which node owns the entries.
struct DirTask {
    dir: PathBuf,
    depth: usize,
    parent_id: Option<String>,
}

/// Identity of the directory a chunk belongs to — fixed for the directory's
/// whole chunk sequence; the worker computes it once per task.
struct ChunkHead {
    node_id: String,
    name: String,
    parent_id: Option<String>,
    depth: usize,
}

/// Closing-chunk-only payload: children to enqueue, volume skips, depth cap.
struct DirFinish {
    child_dirs: Vec<PathBuf>,
    volume_skips: u64,
    depth_capped: bool,
}

/// One chunk of one directory's scan results, streamed worker → aggregator
/// over an mpsc channel. `open` marks the first chunk (bumps `scanned_dirs`
/// and creates the node so later chunks can land on it), `close` the last
/// (enqueues children, applies skips/caps, retires the directory from
/// `pending`). Per-sender FIFO ordering guarantees open → … → close.
struct DirChunk {
    head: ChunkHead,
    open: bool,
    close: bool,
    /// Direct entries in listing order (files + symlinks, DSK-03 index).
    entries: Vec<DiskUsageFileEntry>,
    /// Every failure occurrence; global dedup/cap happens in `ErrorLog`.
    errors: Vec<ScanError>,
    files: u64,
    symlinks: u64,
    logical: u64,
    /// `Some(sum)` over this chunk's files, `None` when any file lacked
    /// allocation data (poisons exactly like the old per-file merges).
    allocated: Option<u64>,
    allocated_unavailable: u64,
    finish: Option<DirFinish>,
}

/// Worker-side accumulation for the chunk currently being built. `allocated`
/// starts `Some(0)` so merging an empty chunk is a no-op and only a FILE
/// without allocation data can poison it (symlinks never touch it — same as
/// the old per-entry `add_file`/`add_symlink` split).
struct ChunkAcc {
    entries: Vec<DiskUsageFileEntry>,
    errors: Vec<ScanError>,
    files: u64,
    symlinks: u64,
    logical: u64,
    allocated: Option<u64>,
    allocated_unavailable: u64,
}

impl ChunkAcc {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
            errors: Vec::new(),
            files: 0,
            symlinks: 0,
            logical: 0,
            allocated: Some(0),
            allocated_unavailable: 0,
        }
    }

    fn push_error(&mut self, path: &Path, err: FsError) {
        self.errors.push(ScanError {
            path: path.to_string_lossy().into_owned(),
            code: err.code,
            message: err.message,
        });
    }
}

/// Streams one chunk to the aggregator, moving the accumulation out. The
/// channel is unbounded (a send never blocks a worker); boundedness comes
/// from the chunk size and directories (never file paths) being the queued
/// unit. A failed send means the aggregator is gone (cancel/pool shutdown):
/// the worker stops instead of scanning into the void.
fn send_chunk(
    tx: &mpsc::Sender<DirChunk>,
    head: &ChunkHead,
    open: bool,
    close: bool,
    acc: &mut ChunkAcc,
    finish: Option<DirFinish>,
) -> bool {
    let chunk = DirChunk {
        head: ChunkHead {
            node_id: head.node_id.clone(),
            name: head.name.clone(),
            parent_id: head.parent_id.clone(),
            depth: head.depth,
        },
        open,
        close,
        entries: std::mem::take(&mut acc.entries),
        errors: std::mem::take(&mut acc.errors),
        files: acc.files,
        symlinks: acc.symlinks,
        logical: acc.logical,
        allocated: acc.allocated,
        allocated_unavailable: acc.allocated_unavailable,
        finish,
    };
    let sent = tx.send(chunk).is_ok();
    acc.files = 0;
    acc.symlinks = 0;
    acc.logical = 0;
    acc.allocated = Some(0);
    acc.allocated_unavailable = 0;
    sent
}

/// Shared work queue: directory tasks only (never file paths) plus the
/// termination bookkeeping. `pending` counts discovered-but-not-yet-closed
/// directories — queued, deferred, and in-flight together; it hits zero
/// exactly when the tree is exhausted.
struct PoolQueue {
    dirs: VecDeque<DirTask>,
    pending: u64,
    done: bool,
}

struct PoolSharedInner {
    lock: Mutex<PoolQueue>,
    cv: Condvar,
}

type PoolShared = Arc<PoolSharedInner>;

/// Worker loop: pull a directory task, scan it fully, stream chunks. Exits
/// when the pool is done (every discovered directory applied) or cancelled;
/// `std::thread::scope` joins every worker on either path — no zombies.
fn pool_worker(
    queue: &PoolShared,
    tx: &mpsc::Sender<DirChunk>,
    cancel: &AtomicBool,
    options: &ScanOptions,
    root_volume: &Option<String>,
) {
    loop {
        let task = {
            let mut q = queue.lock.lock().expect("disk usage pool queue");
            loop {
                if q.done || cancel.load(Ordering::Relaxed) {
                    return;
                }
                match q.dirs.pop_front() {
                    Some(task) => break task,
                    None => {
                        if q.pending == 0 {
                            // Tree exhausted: retire the pool so waiting
                            // siblings wake and exit too.
                            q.done = true;
                            queue.cv.notify_all();
                            return;
                        }
                        q = queue.cv.wait(q).expect("disk usage pool queue");
                    }
                }
            }
        };
        if !scan_dir_task(&task, tx, cancel, options, root_volume) {
            return; // aggregator gone — stop scanning into the void
        }
    }
}

/// Scans ONE directory completely — same entry discipline as the serial
/// walker (metadata-only, per-entry cancel checks, D07 no-follow,
/// cross-volume guard, depth cap) — and streams the results in chunks.
/// Returns false when the aggregator is gone (send failed).
fn scan_dir_task(
    task: &DirTask,
    tx: &mpsc::Sender<DirChunk>,
    cancel: &AtomicBool,
    options: &ScanOptions,
    root_volume: &Option<String>,
) -> bool {
    let DirTask {
        dir,
        depth,
        parent_id,
    } = task;
    let head = ChunkHead {
        node_id: dir.to_string_lossy().into_owned(),
        name: entry_name(dir),
        parent_id: parent_id.clone(),
        depth: *depth,
    };
    let mut acc = ChunkAcc::new();
    let mut open_sent = false;
    let mut child_dirs: Vec<PathBuf> = Vec::new();
    let mut volume_skips = 0u64;
    let mut depth_capped = false;

    let read = match fs::read_dir(dir) {
        Ok(read) => read,
        Err(e) => {
            // Permission denied on the directory itself: it still counts as
            // visited and gets its node — coverage goes partial, never
            // silently 0 (contract §3.6).
            acc.push_error(dir, FsError::from(e));
            return send_chunk(
                tx,
                &head,
                true,
                true,
                &mut acc,
                Some(DirFinish {
                    child_dirs: Vec::new(),
                    volume_skips: 0,
                    depth_capped: false,
                }),
            );
        }
    };
    for item in read {
        if cancel.load(Ordering::Relaxed) {
            // Mid-listing cancel: no closing chunk is needed — the
            // aggregator has already stopped applying, the pool is marked
            // done, and the scope join tears everything down.
            return true;
        }
        let child = match item {
            Ok(child) => child,
            Err(e) => {
                acc.push_error(dir, FsError::from(e));
                continue;
            }
        };
        let child_path = child.path();
        match fs::symlink_metadata(&child_path) {
            // Deleted-during-scan and permission denied share this path:
            // recorded per path, the walk continues (contract §3.6).
            Err(e) => acc.push_error(&child_path, FsError::from(e)),
            Ok(clm) => {
                if clm.is_symlink() {
                    acc.entries.push(DiskUsageFileEntry {
                        name: entry_name(&child_path),
                        logical_bytes: 0,
                        allocated_bytes: None,
                        is_symlink: true,
                    });
                    acc.symlinks += 1;
                } else if clm.is_dir() {
                    let within_volume = same_volume(root_volume, &child_path);
                    let depth_ok = options.max_depth.is_none_or(|md| *depth < md);
                    if within_volume && depth_ok {
                        child_dirs.push(child_path);
                    } else if !within_volume {
                        // Other volumes are counted as entries but never
                        // crossed (gap G3).
                        volume_skips += 1;
                    } else {
                        depth_capped = true;
                    }
                } else {
                    let logical = clm.len();
                    let (alloc, supported) =
                        crate::properties::allocated_size(&child_path, logical);
                    let allocated = if supported { Some(alloc) } else { None };
                    if allocated.is_none() {
                        acc.allocated_unavailable += 1;
                    }
                    acc.entries.push(DiskUsageFileEntry {
                        name: entry_name(&child_path),
                        logical_bytes: logical,
                        allocated_bytes: allocated,
                        is_symlink: false,
                    });
                    acc.files += 1;
                    acc.logical = acc.logical.saturating_add(logical);
                    acc.allocated = merge_allocated(acc.allocated, allocated);
                }
            }
        }
        if acc.entries.len() >= BATCH_CHUNK {
            if !send_chunk(tx, &head, !open_sent, false, &mut acc, None) {
                return false;
            }
            open_sent = true;
        }
    }
    send_chunk(
        tx,
        &head,
        !open_sent,
        true,
        &mut acc,
        Some(DirFinish {
            child_dirs,
            volume_skips,
            depth_capped,
        }),
    )
}

/// Aggregator side: applies one chunk to the live state and, on the closing
/// chunk, releases newly discovered child directories into the (bounded)
/// shared queue. Every aggregate here is commutative (saturating sums +
/// `Option` poisoning), so application order across workers cannot change
/// the final totals. Ordering invariant: children are enqueued only while
/// applying the parent's close chunk, so a parent node always exists before
/// any worker can pop its child — `create_dir_node` never misses.
fn apply_chunk(
    mut chunk: DirChunk,
    st: &mut LiveState,
    queue: &PoolShared,
    deferred: &mut VecDeque<DirTask>,
) {
    if chunk.open {
        st.scanned_dirs += 1;
        if let Some(parent) = &chunk.head.parent_id {
            st.create_dir_node(
                &chunk.head.node_id,
                &chunk.head.node_id,
                &chunk.head.name,
                parent,
            );
        }
    }
    // File/symlink counters, per-node direct fields, query index — applied
    // once per chunk (≤[`BATCH_CHUNK`] entries) instead of once per entry.
    // Symlinks contribute counts but never allocation data (D07, §2.2).
    st.scanned_files += chunk.files;
    st.scanned_symlinks += chunk.symlinks;
    st.logical_bytes = st.logical_bytes.saturating_add(chunk.logical);
    st.allocated_available_files += chunk.files - chunk.allocated_unavailable;
    st.allocated_unavailable_files += chunk.allocated_unavailable;
    st.known_allocated_bytes = merge_allocated(st.known_allocated_bytes, chunk.allocated);
    if let Some(node) = st.nodes.get_mut(&chunk.head.node_id) {
        node.direct_files += chunk.files;
        node.direct_symlinks += chunk.symlinks;
        node.direct_logical_bytes = node.direct_logical_bytes.saturating_add(chunk.logical);
        node.direct_allocated_bytes = merge_allocated(node.direct_allocated_bytes, chunk.allocated);
    }
    if !chunk.entries.is_empty() {
        st.files_by_parent
            .entry(chunk.head.node_id.clone())
            .or_default()
            .append(&mut chunk.entries);
    }
    let (files, symlinks, logical, allocated) =
        (chunk.files, chunk.symlinks, chunk.logical, chunk.allocated);
    st.for_self_and_ancestors(&chunk.head.node_id, |n| {
        n.subtree_files += files;
        n.subtree_symlinks += symlinks;
        n.subtree_logical_bytes = n.subtree_logical_bytes.saturating_add(logical);
        n.subtree_allocated_bytes = merge_allocated(n.subtree_allocated_bytes, allocated);
    });

    if !chunk.errors.is_empty() {
        let count = chunk.errors.len() as u64;
        for err in &chunk.errors {
            st.errors.record_parts(&err.path, err.code, &err.message);
        }
        st.for_self_and_ancestors(&chunk.head.node_id, |n| n.subtree_errors += count);
    }

    if chunk.close
        && let Some(finish) = chunk.finish
    {
        if finish.volume_skips > 0 {
            st.skipped_volume_boundaries += finish.volume_skips;
            if let Some(node) = st.nodes.get_mut(&chunk.head.node_id) {
                node.skipped_volume_boundaries += finish.volume_skips;
            }
            st.for_self_and_ancestors(&chunk.head.node_id, |n| {
                n.subtree_skipped_volumes += finish.volume_skips;
            });
        }
        if finish.depth_capped {
            st.depth_capped = true;
        }
        // Enqueue this directory's children (bounded queue, deferred
        // frontier) and retire the directory from `pending`.
        let mut q = queue.lock.lock().expect("disk usage pool queue");
        q.pending += finish.child_dirs.len() as u64;
        for child in finish.child_dirs {
            deferred.push_back(DirTask {
                dir: child,
                depth: chunk.head.depth + 1,
                parent_id: Some(chunk.head.node_id.clone()),
            });
        }
        while q.dirs.len() < MAX_QUEUED_DIRS
            && let Some(task) = deferred.pop_front()
        {
            q.dirs.push_back(task);
        }
        q.pending -= 1;
        // Wake starved workers (new work) and idle ones (possible end).
        queue.cv.notify_all();
    }
}

/// DSK-09 parallel walk: `workers` scoped threads scan directories pulled
/// from a shared FIFO queue; the CALLING thread aggregates every chunk into
/// its single-owned `LiveState` (no per-entry locks, sink untouched) and
/// emits the same throttled progress events as the serial walk. Cancel is
/// checked per entry in workers and per [`POOL_POLL`] cycle here; on exit
/// the queue is marked done and `std::thread::scope` joins every worker
/// before returning. Totals are identical to the serial walk for any worker
/// count (commutative aggregation; verified by test).
#[allow(clippy::too_many_arguments)]
fn run_scan_pool(
    root: &str,
    options: &ScanOptions,
    cancel: &AtomicBool,
    scan_id: &str,
    throttle: Option<Duration>,
    workers: usize,
    mut sink: impl FnMut(&DiskUsageEvent),
) -> Result<ScanOutcome, FsError> {
    let root_path = validate_scan_root(root)?;
    let root_volume = volume_prefix_of(root);
    let root_id = root.to_string();
    let root_name = entry_name(Path::new(root));
    let mut st = LiveState::new(&root_id);
    st.create_root(&root_id, root, &root_name);

    let start = Instant::now();
    let mut last_emit: Option<Instant> = None;
    let queue: PoolShared = Arc::new(PoolSharedInner {
        lock: Mutex::new(PoolQueue {
            dirs: VecDeque::new(),
            // The root: discovered, not yet closed.
            pending: 1,
            done: false,
        }),
        cv: Condvar::new(),
    });
    queue
        .lock
        .lock()
        .expect("disk usage pool queue")
        .dirs
        .push_back(DirTask {
            dir: root_path,
            depth: 0,
            parent_id: None,
        });
    // Discovered children beyond [`MAX_QUEUED_DIRS`] wait here until the
    // queue drains below the cap — aggregator-owned, so it needs no lock.
    let mut deferred: VecDeque<DirTask> = VecDeque::new();
    let mut cancelled = false;
    let (tx, rx) = mpsc::channel::<DirChunk>();
    // Shared by reference into every worker closure (references are Copy).
    let root_volume = &root_volume;

    thread::scope(|scope| {
        for _ in 0..workers {
            let queue = Arc::clone(&queue);
            let tx = tx.clone();
            scope.spawn(move || pool_worker(&queue, &tx, cancel, options, root_volume));
        }
        drop(tx); // the aggregator holds the only receiving end

        'aggregate: loop {
            if cancel.load(Ordering::Relaxed) {
                cancelled = true;
                break 'aggregate;
            }
            // Drain everything currently queued. Per-sender FIFO preserves
            // open → entries → close for each directory. The throttle is
            // checked between messages too, so progress events stay timely
            // even while workers stream chunks without a gap.
            let mut disconnected = false;
            loop {
                match rx.recv_timeout(POOL_POLL) {
                    Ok(chunk) => apply_chunk(chunk, &mut st, &queue, &mut deferred),
                    Err(mpsc::RecvTimeoutError::Timeout) => break,
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
                if let Some(interval) = throttle {
                    let due = last_emit.is_none_or(|t| t.elapsed() >= interval);
                    if due {
                        last_emit = Some(Instant::now());
                        sink(&st.event(
                            scan_id,
                            false,
                            false,
                            Coverage::Partial,
                            None,
                            start.elapsed().as_millis() as u64,
                        ));
                    }
                }
            }
            if queue.lock.lock().expect("disk usage pool queue").pending == 0 {
                // Every discovered directory fully applied — the tree is done.
                break 'aggregate;
            }
            if cancel.load(Ordering::Relaxed) {
                // Workers exit on their per-entry cancel checks and drop the
                // senders, so a cancelled pool surfaces here FIRST as a
                // disconnected channel — the cancel decision must precede
                // the disconnect handling or a cancelled scan would be
                // mislabelled as a dead pool.
                cancelled = true;
                break 'aggregate;
            }
            if disconnected {
                // Unreachable by protocol (a popped directory always closes
                // unless cancelled) — a pool worker died mid-walk. Fail
                // HONESTLY: record why so coverage is partial, never a
                // silently complete-looking scan.
                st.add_error(
                    &st.root_id.clone(),
                    Path::new(root),
                    &FsError::new(
                        ErrorCode::Io,
                        "scan worker pool ended early; subtree totals are incomplete",
                    ),
                );
                break 'aggregate;
            }
            // Release deferred children into the drained queue (backpressure
            // low-water refill).
            if !deferred.is_empty() {
                let mut q = queue.lock.lock().expect("disk usage pool queue");
                while q.dirs.len() < MAX_QUEUED_DIRS
                    && let Some(task) = deferred.pop_front()
                {
                    q.dirs.push_back(task);
                }
                queue.cv.notify_all();
            }
        }

        // Shut the pool down: waiting workers see `done`, mid-scan workers
        // see the cancel flag per entry, and `scope` joins them all below.
        let mut q = queue.lock.lock().expect("disk usage pool queue");
        q.done = true;
        queue.cv.notify_all();
    });

    Ok(finalize_scan(st, root, scan_id, cancelled, start, sink))
}

/// FER-460: snapshot assembly from the elevated helper's MFT entries.
#[path = "disk_usage_fast.rs"]
pub mod fast;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    static SEQ: AtomicU64 = AtomicU64::new(0);

    /// Owned temp dir under %TEMP%, removed on Drop (also on assertion failure or early return).
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "ferrum-dsk02-{tag}-{}-{}",
                std::process::id(),
                SEQ.fetch_add(1, Ordering::SeqCst)
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).expect("temp root");
            TempDir(dir)
        }

        #[allow(dead_code)]
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl std::ops::Deref for TempDir {
        type Target = Path;
        fn deref(&self) -> &Path {
            &self.0
        }
    }

    impl AsRef<Path> for TempDir {
        fn as_ref(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn temp_root(tag: &str) -> TempDir {
        TempDir::new(tag)
    }

    fn no_cancel() -> AtomicBool {
        AtomicBool::new(false)
    }

    /// 1000 B at root, 500+250 B in `sub`, empty `sub2` — known fixture.
    fn fixture() -> TempDir {
        let root = temp_root("tree");
        fs::write(root.join("a.bin"), vec![0u8; 1000]).unwrap();
        fs::create_dir(root.join("sub")).unwrap();
        fs::write(root.join("sub").join("b.bin"), vec![0u8; 500]).unwrap();
        fs::write(root.join("sub").join("c.bin"), vec![0u8; 250]).unwrap();
        fs::create_dir(root.join("sub2")).unwrap();
        root
    }

    fn scan(
        root: &Path,
        options: &ScanOptions,
        cancel: &AtomicBool,
        throttle: Option<Duration>,
    ) -> (Vec<DiskUsageEvent>, ScanOutcome) {
        let events = std::sync::Mutex::new(Vec::new());
        let outcome = run_scan_with_throttle(
            root.to_str().unwrap(),
            options,
            cancel,
            "du-test",
            throttle,
            |ev| events.lock().unwrap().push(ev.clone()),
        )
        .expect("scan runs");
        (events.into_inner().unwrap(), outcome)
    }

    #[test]
    fn fixture_aggregation_is_exact_and_never_double_counts() {
        let root = fixture();
        let (_, outcome) = scan(&root, &ScanOptions::default(), &no_cancel(), None);
        let snap = &outcome.snapshot;
        let s = &snap.stats;
        assert_eq!(s.scanned_files, 3);
        assert_eq!(s.scanned_dirs, 3); // root + sub + sub2
        assert_eq!(s.logical_bytes, 1750);
        assert_eq!(s.coverage, Coverage::Complete);
        assert!(!s.cancelled);
        assert_eq!(s.total_errors, 0);

        let root_id = root.to_string_lossy().into_owned();
        let sub_id = root.join("sub").to_string_lossy().into_owned();
        let root_node = &snap.nodes[&root_id];
        // Root total = sum of leaves; its own direct file is NOT re-added.
        assert_eq!(root_node.direct_files, 1);
        assert_eq!(root_node.direct_logical_bytes, 1000);
        assert_eq!(root_node.subtree_files, 3);
        assert_eq!(root_node.subtree_dirs, 2);
        assert_eq!(root_node.subtree_logical_bytes, 1750);
        assert_eq!(root_node.children_ids.len(), 2);
        let sub_node = &snap.nodes[&sub_id];
        assert_eq!(sub_node.direct_files, 2);
        assert_eq!(sub_node.direct_logical_bytes, 750);
        assert_eq!(sub_node.subtree_logical_bytes, 750);
        assert_eq!(sub_node.parent_id.as_deref(), Some(root_id.as_str()));
        let sub2_id = root.join("sub2").to_string_lossy().into_owned();
        assert_eq!(snap.nodes[&sub2_id].subtree_logical_bytes, 0);
        assert_eq!(snap.nodes[&sub2_id].subtree_files, 0);
        // Allocated bytes: per-node availability, never fabricated.
        if cfg!(windows) {
            assert_eq!(s.allocated_unavailable_files, 0);
            assert!(s.known_allocated_bytes.unwrap() >= s.logical_bytes);
            assert!(root_node.subtree_allocated_bytes.unwrap() >= 1750);
        } else {
            assert_eq!(s.allocated_unavailable_files, 3);
            assert_eq!(s.known_allocated_bytes, None);
            assert_eq!(root_node.subtree_allocated_bytes, None);
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn zero_byte_file_counts_but_adds_no_bytes() {
        let root = temp_root("zero");
        fs::write(root.join("empty.txt"), b"").unwrap();
        let (_, outcome) = scan(&root, &ScanOptions::default(), &no_cancel(), None);
        assert_eq!(outcome.stats.scanned_files, 1);
        assert_eq!(outcome.stats.logical_bytes, 0);
        assert_eq!(outcome.stats.coverage, Coverage::Complete);
        let root_id = root.to_string_lossy().into_owned();
        assert_eq!(outcome.snapshot.nodes[&root_id].direct_files, 1);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn symlink_is_counted_but_never_followed_so_loops_cannot_explode() {
        let root = temp_root("loop");
        fs::write(root.join("real.txt"), b"x").unwrap();
        fs::create_dir(root.join("inner")).unwrap();
        let made = std::os::windows::fs::symlink_dir(&root, root.join("inner").join("up"));
        if made.is_err() {
            return; // environment refuses unprivileged symlinks
        }
        let (_, outcome) = scan(&root, &ScanOptions::default(), &no_cancel(), None);
        // The link is counted once; the walk never re-enters root.
        assert_eq!(outcome.stats.scanned_symlinks, 1);
        assert_eq!(outcome.stats.scanned_files, 1);
        assert_eq!(outcome.stats.coverage, Coverage::Complete);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn symlink_scan_root_is_rejected_not_followed() {
        let root = temp_root("linkroot");
        fs::create_dir(root.join("real")).unwrap();
        let made = std::os::windows::fs::symlink_dir(root.join("real"), root.join("link"));
        if made.is_err() {
            return; // unprivileged symlinks unavailable
        }
        let err = run_scan_with_throttle(
            root.join("link").to_str().unwrap(),
            &ScanOptions::default(),
            &no_cancel(),
            "du-test",
            None,
            |_| {},
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidPath);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn empty_and_file_roots_are_rejected() {
        // v1: exactly one, non-empty, existing directory root per scan.
        let err = run_scan_with_throttle(
            "",
            &ScanOptions::default(),
            &no_cancel(),
            "du",
            None,
            |_| {},
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidPath);
        let err = run_scan_with_throttle(
            "   ",
            &ScanOptions::default(),
            &no_cancel(),
            "du",
            None,
            |_| {},
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidPath);
        let root = temp_root("fileroot");
        fs::write(root.join("f.txt"), b"x").unwrap();
        let err = run_scan_with_throttle(
            root.join("f.txt").to_str().unwrap(),
            &ScanOptions::default(),
            &no_cancel(),
            "du",
            None,
            |_| {},
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::NotADirectory);
        let err = run_scan_with_throttle(
            root.join("gone").to_str().unwrap(),
            &ScanOptions::default(),
            &no_cancel(),
            "du",
            None,
            |_| {},
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn cancel_yields_done_without_complete() {
        let root = fixture();
        let (_, outcome) = scan(&root, &ScanOptions::default(), &AtomicBool::new(true), None);
        assert!(outcome.stats.cancelled);
        assert_eq!(outcome.stats.coverage, Coverage::Partial);
        // Cancelled before any I/O: nothing was counted.
        assert_eq!(outcome.stats.scanned_files, 0);
        assert_eq!(outcome.stats.scanned_dirs, 0);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn generation_guard_new_scan_never_inherits_old_scan_state() {
        // Scan A is cancelled immediately; a fresh scan B on the same root
        // must run under its own id, own cancel flag, and reach complete.
        let root = fixture();
        let events = std::sync::Mutex::new(Vec::new());
        let a = run_scan_with_throttle(
            root.to_str().unwrap(),
            &ScanOptions::default(),
            &AtomicBool::new(true),
            "du-old",
            None,
            |ev| events.lock().unwrap().push(ev.clone()),
        )
        .unwrap();
        let b = run_scan_with_throttle(
            root.to_str().unwrap(),
            &ScanOptions::default(),
            &no_cancel(),
            "du-new",
            None,
            |ev| events.lock().unwrap().push(ev.clone()),
        )
        .unwrap();
        assert_ne!(a.scan_id, b.scan_id);
        assert!(a.stats.cancelled);
        assert_eq!(b.stats.coverage, Coverage::Complete);
        let evs = events.into_inner().unwrap();
        assert!(
            evs.iter()
                .all(|e| e.scan_id == "du-old" || e.scan_id == "du-new")
        );
        assert_eq!(
            evs.last().map(|e| (e.done, e.cancelled)),
            Some((true, false))
        );
        assert_eq!(evs.last().map(|e| e.scan_id.as_str()), Some("du-new"));

        // Registry isolation: cancelling scan 1 must never touch scan 2.
        let id1 = new_scan_id();
        let id2 = new_scan_id();
        assert_ne!(id1, id2, "ids are unique per start (timestamp+counter)");
        let flag1 = Arc::new(AtomicBool::new(false));
        let flag2 = Arc::new(AtomicBool::new(false));
        active_scans()
            .lock()
            .unwrap()
            .insert(id1.clone(), flag1.clone());
        active_scans()
            .lock()
            .unwrap()
            .insert(id2.clone(), flag2.clone());
        assert!(cancel_scan(&id1));
        assert!(!cancel_scan(&id1), "second cancel finds nothing");
        assert!(!flag2.load(Ordering::Relaxed), "scan 2 unaffected");
        assert!(cancel_scan(&id2));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn depth_cap_stops_descending_and_reports_partial() {
        let root = fixture();
        let opts = ScanOptions { max_depth: Some(0) };
        let (_, outcome) = scan(&root, &opts, &no_cancel(), None);
        assert!(outcome.stats.depth_capped);
        assert_eq!(outcome.stats.coverage, Coverage::Partial);
        // Root's direct file counted; nothing below `sub`/`sub2` entered.
        assert_eq!(outcome.stats.scanned_files, 1);
        assert_eq!(outcome.stats.logical_bytes, 1000);
        assert_eq!(outcome.stats.scanned_dirs, 1);
        let root_id = root.to_string_lossy().into_owned();
        assert!(outcome.snapshot.nodes.contains_key(&root_id));
        let sub_id = root.join("sub").to_string_lossy().into_owned();
        assert!(
            !outcome.snapshot.nodes.contains_key(&sub_id),
            "capped children are never entered"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn unicode_names_aggregate_exactly() {
        let root = temp_root("unicode");
        fs::write(root.join("ø.txt"), b"12345").unwrap();
        let deep = root.join("békér ünicode");
        fs::create_dir(&deep).unwrap();
        fs::write(deep.join("naïve.txt"), vec![0u8; 300]).unwrap();
        let (_, outcome) = scan(&root, &ScanOptions::default(), &no_cancel(), None);
        assert_eq!(outcome.stats.scanned_files, 2);
        assert_eq!(outcome.stats.logical_bytes, 305);
        assert_eq!(outcome.stats.coverage, Coverage::Complete);
        let deep_id = deep.to_string_lossy().into_owned();
        let node = &outcome.snapshot.nodes[&deep_id];
        assert_eq!(node.name, "békér ünicode");
        assert_eq!(node.subtree_logical_bytes, 300);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn volume_guard_recognizes_only_drive_letter_prefixes() {
        // Pure boundary logic (gap G3): same drive stays in scope, other
        // drives and unrecognized prefixes do not cross.
        assert_eq!(volume_prefix_of(r"C:\Users\x"), Some(r"C:\".to_string()));
        assert_eq!(volume_prefix_of(r"d:\data"), Some(r"d:\".to_string()));
        assert_eq!(
            volume_prefix_of(r"\\server\share"),
            None,
            "UNC unrecognized (documented)"
        );
        assert_eq!(
            volume_prefix_of("/usr/local"),
            None,
            "non-Windows roots (documented)"
        );
        assert_eq!(volume_prefix_of("relative"), None);
        let c = volume_prefix_of(r"C:\x").unwrap();
        let d = volume_prefix_of(r"D:\x").unwrap();
        assert!(c.eq_ignore_ascii_case(&c));
        assert!(!c.eq_ignore_ascii_case(&d));
    }

    #[test]
    fn progress_stream_has_running_totals_and_exactly_one_done() {
        let root = fixture();
        let (events, outcome) = scan(
            &root,
            &ScanOptions::default(),
            &no_cancel(),
            Some(Duration::ZERO),
        );
        assert!(events.len() >= 2, "throttle off → progress + final events");
        for ev in &events[..events.len() - 1] {
            assert!(!ev.done, "only the final event may set done");
            assert_eq!(ev.coverage, Coverage::Partial, "mid-scan is partial");
        }
        // Totals are monotonic — progressive batches, no resets.
        let mut last = 0u64;
        for ev in &events {
            assert!(ev.scanned_files >= last);
            last = ev.scanned_files;
        }
        let final_ev = events.last().unwrap();
        assert!(final_ev.done);
        assert!(!final_ev.cancelled);
        assert_eq!(final_ev.coverage, Coverage::Complete);
        assert_eq!(final_ev.scanned_files, outcome.stats.scanned_files);
        assert_eq!(final_ev.known_logical_bytes, outcome.stats.logical_bytes);
        assert_eq!(final_ev.scan_id, outcome.scan_id);
        assert!(
            final_ev.duration_ms >= events[0].duration_ms,
            "duration grows with the walk"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn top_children_are_largest_first_with_stable_tiebreak() {
        let root = temp_root("top");
        for (name, size) in [
            ("m1", 20u64),
            ("big", 50),
            ("t2", 10),
            ("t1", 10),
            ("m2", 30),
        ] {
            fs::create_dir(root.join(name)).unwrap();
            fs::write(root.join(name).join("f.bin"), vec![0u8; size as usize]).unwrap();
        }
        let (events, _) = scan(&root, &ScanOptions::default(), &no_cancel(), None);
        let top = &events.last().unwrap().top_children;
        let names: Vec<&str> = top.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["big", "m2", "m1", "t1", "t2"],
            "subtree bytes desc, id asc for ties"
        );
        assert_eq!(top[0].subtree_logical_bytes, 50);
        assert!(top.len() <= TOP_CHILDREN_LIMIT);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn error_log_aggregates_per_path_and_stays_bounded() {
        let mut st = LiveState::new("root-x");
        st.create_root("root-x", "C:\\x", "x");
        st.create_dir_node("child", "C:\\x\\child", "child", "root-x");
        let boom = FsError::new(ErrorCode::PermissionDenied, "denied");
        st.add_error("child", Path::new(r"C:\x\child\gone.bin"), &boom);
        st.add_error("child", Path::new(r"C:\x\child\gone.bin"), &boom); // dedup
        assert_eq!(st.errors.total, 2);
        assert_eq!(st.errors.ordered.len(), 1, "one entry per path");
        assert_eq!(st.errors.ordered[0].code, ErrorCode::PermissionDenied);
        // Error counts roll up to every ancestor.
        assert_eq!(st.nodes["child"].subtree_errors, 2);
        assert_eq!(st.nodes["root-x"].subtree_errors, 2);
        for i in 0..(MAX_REPORTED_ERRORS + 50) {
            st.add_error(
                "child",
                &PathBuf::from(format!(r"C:\x\child\e{i}.bin")),
                &boom,
            );
        }
        assert_eq!(st.errors.ordered.len(), MAX_REPORTED_ERRORS);
        assert_eq!(st.errors.total, (2 + MAX_REPORTED_ERRORS + 50) as u64);
    }

    #[test]
    fn allocated_merge_is_none_when_any_side_lacks_data() {
        // Gap G2 structural semantics: partial availability poisons to None.
        assert_eq!(merge_allocated(Some(5), Some(7)), Some(12));
        assert_eq!(merge_allocated(Some(5), None), None);
        assert_eq!(merge_allocated(None, Some(7)), None);
        assert_eq!(merge_allocated(None, None), None);
    }

    #[test]
    fn snapshot_store_keeps_bounded_scans_and_evicts_oldest() {
        for i in 0..(MAX_STORED_SCANS + 1) {
            let id = format!("du-store-{i}");
            store_snapshot(DiskUsageSnapshot {
                scan_id: id.clone(),
                root_path: r"C:\x".into(),
                root_id: id.clone(),
                version: 0,
                scanned_at_ms: 0,
                nodes: HashMap::new(),
                files_by_parent: HashMap::new(),
                stats: DiskUsageStats {
                    scanned_files: i as u64,
                    scanned_dirs: 0,
                    scanned_symlinks: 0,
                    skipped_volume_boundaries: 0,
                    logical_bytes: 0,
                    allocated_available_files: 0,
                    allocated_unavailable_files: 0,
                    known_allocated_bytes: None,
                    total_errors: 0,
                    coverage: Coverage::Complete,
                    cancelled: false,
                    depth_capped: false,
                    duration_ms: 0,
                },
            });
        }
        assert!(snapshot("du-store-0").is_none(), "oldest evicted");
        let kept = snapshot(&format!("du-store-{}", MAX_STORED_SCANS)).expect("newest kept");
        assert_eq!(kept.scan_id, format!("du-store-{}", MAX_STORED_SCANS));
    }

    // ── DSK-03 (FER-321): query children / top files / classification ────

    /// 4 child dirs (one file each: 400/300/200/100 B) + two root files
    /// (900/150 B) — mixed kinds, enough for pagination and ordering.
    fn wide_fixture() -> TempDir {
        let root = temp_root("wide");
        for (name, size) in [
            ("d400", 400u64),
            ("d300", 300),
            ("d200", 200),
            ("d100", 100),
        ] {
            fs::create_dir(root.join(name)).unwrap();
            fs::write(root.join(name).join("f.bin"), vec![0u8; size as usize]).unwrap();
        }
        fs::write(root.join("r900.bin"), vec![0u8; 900]).unwrap();
        fs::write(root.join("r150.bin"), vec![0u8; 150]).unwrap();
        root
    }

    fn complete_stats() -> DiskUsageStats {
        DiskUsageStats {
            scanned_files: 0,
            scanned_dirs: 0,
            scanned_symlinks: 0,
            skipped_volume_boundaries: 0,
            logical_bytes: 0,
            allocated_available_files: 0,
            allocated_unavailable_files: 0,
            known_allocated_bytes: None,
            total_errors: 0,
            coverage: Coverage::Complete,
            cancelled: false,
            depth_capped: false,
            duration_ms: 0,
        }
    }

    fn dir_node(
        id: &str,
        name: &str,
        parent: Option<&str>,
        logical: u64,
        allocated: Option<u64>,
    ) -> DiskUsageNode {
        let mut n = LiveState::blank_node(id, id, name, parent.map(|p| p.to_string()));
        n.subtree_logical_bytes = logical;
        n.subtree_allocated_bytes = allocated;
        n
    }

    /// Platform-independent snapshot: root + direct dir children with the
    /// given (logical, allocated) subtree totals, no file entries.
    fn synthetic(
        children: &[(&str, u64, Option<u64>)],
        root_allocated: Option<u64>,
    ) -> DiskUsageSnapshot {
        let root_id = r"C:\synq";
        let mut nodes = HashMap::new();
        let mut root = dir_node(root_id, "synq", None, 0, root_allocated);
        let mut total_logical = 0u64;
        for (name, logical, alloc) in children {
            let id = format!(r"C:\synq\{name}");
            root.children_ids.push(id.clone());
            total_logical = total_logical.saturating_add(*logical);
            nodes.insert(
                id.clone(),
                dir_node(&id, name, Some(root_id), *logical, *alloc),
            );
        }
        root.subtree_logical_bytes = total_logical;
        nodes.insert(root_id.to_string(), root);
        DiskUsageSnapshot {
            scan_id: "du-synthetic".into(),
            root_path: root_id.into(),
            root_id: root_id.into(),
            version: 7,
            scanned_at_ms: 12_345,
            nodes,
            files_by_parent: HashMap::new(),
            stats: complete_stats(),
        }
    }

    #[test]
    fn children_pagination_is_exact_with_total_and_empty_tail() {
        let root = wide_fixture();
        let (_, outcome) = scan(&root, &ScanOptions::default(), &no_cancel(), None);
        let snap = &outcome.snapshot;
        let root_id = root.to_string_lossy().into_owned();
        let filter = DiskUsageFilter::default();

        let page =
            query_children_snapshot(snap, &root_id, 0, 4, DiskUsageSort::LargestFirst, &filter)
                .unwrap();
        assert_eq!(page.total, 6, "4 dirs + 2 files");
        assert_eq!(page.limit, 4);
        assert_eq!(page.items.len(), 4);
        let names: Vec<&str> = page.items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, vec!["r900.bin", "d400", "d300", "d200"]);

        let page2 =
            query_children_snapshot(snap, &root_id, 4, 4, DiskUsageSort::LargestFirst, &filter)
                .unwrap();
        assert_eq!(page2.total, 6);
        assert_eq!(page2.items.len(), 2);
        let names2: Vec<&str> = page2.items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names2, vec!["r150.bin", "d100"]);
        // Pages never overlap.
        assert!(
            page.items
                .iter()
                .all(|i| !page2.items.iter().any(|j| j.id == i.id))
        );

        // Offset past the total: empty page, total intact for the pager.
        let past =
            query_children_snapshot(snap, &root_id, 99, 4, DiskUsageSort::LargestFirst, &filter)
                .unwrap();
        assert!(past.items.is_empty());
        assert_eq!(past.total, 6);

        // Responses carry snapshot identity so the UI can keep selections.
        assert_eq!(page.scan_id, snap.scan_id);
        assert_eq!(page.version, snap.version);
        assert!(page.scanned_at_ms > 0);
        assert_eq!(page.metric, SizeMetric::Logical);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn children_largest_first_with_stable_id_tiebreak_both_directions() {
        let root = temp_root("qorder");
        for (name, size) in [
            ("m1", 20u64),
            ("big", 50),
            ("t2", 10),
            ("t1", 10),
            ("m2", 30),
        ] {
            fs::create_dir(root.join(name)).unwrap();
            fs::write(root.join(name).join("f.bin"), vec![0u8; size as usize]).unwrap();
        }
        fs::write(root.join("file60.bin"), vec![0u8; 60]).unwrap();
        let (_, outcome) = scan(&root, &ScanOptions::default(), &no_cancel(), None);
        let snap = &outcome.snapshot;
        let root_id = root.to_string_lossy().into_owned();
        let filter = DiskUsageFilter::default();

        let page =
            query_children_snapshot(snap, &root_id, 0, 100, DiskUsageSort::LargestFirst, &filter)
                .unwrap();
        let names: Vec<&str> = page.items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["file60.bin", "big", "m2", "m1", "t1", "t2"],
            "metric desc with files+dirs mixed; ties (t1/t2) break by stable id asc"
        );

        let page = query_children_snapshot(
            snap,
            &root_id,
            0,
            100,
            DiskUsageSort::SmallestFirst,
            &filter,
        )
        .unwrap();
        let names: Vec<&str> = page.items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, vec!["t1", "t2", "m1", "m2", "big", "file60.bin"]);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn children_list_dirs_files_and_symlinks_with_correct_kinds() {
        let root = temp_root("qkinds");
        fs::write(root.join("doc.txt"), b"hello").unwrap();
        fs::create_dir(root.join("folder")).unwrap();
        let made = std::os::windows::fs::symlink_file(root.join("doc.txt"), root.join("link.txt"));
        if made.is_err() {
            return; // environment refuses unprivileged symlinks
        }
        let (_, outcome) = scan(&root, &ScanOptions::default(), &no_cancel(), None);
        let snap = &outcome.snapshot;
        let root_id = root.to_string_lossy().into_owned();
        let page = query_children_snapshot(
            snap,
            &root_id,
            0,
            100,
            DiskUsageSort::NameAsc,
            &DiskUsageFilter::default(),
        )
        .unwrap();
        let kinds: Vec<(&str, DiskUsageEntryKind)> = page
            .items
            .iter()
            .map(|i| (i.name.as_str(), i.kind))
            .collect();
        assert_eq!(
            kinds,
            vec![
                ("doc.txt", DiskUsageEntryKind::File),
                ("folder", DiskUsageEntryKind::Dir),
                ("link.txt", DiskUsageEntryKind::Symlink),
            ],
            "name asc, all three kinds listed"
        );
        let link = page.items.iter().find(|i| i.name == "link.txt").unwrap();
        assert_eq!(link.logical_bytes, 0, "links are counted, never followed");
        assert_eq!(link.allocated_bytes, None);
        assert!(!link.allocated_supported);
        assert_eq!(link.category, FileCategory::Other);
        let doc = page.items.iter().find(|i| i.name == "doc.txt").unwrap();
        assert_eq!(doc.category, FileCategory::Document);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn children_filters_threshold_name_extension_and_category() {
        let root = temp_root("qfilter");
        fs::write(root.join("movie.mp4"), vec![0u8; 400]).unwrap();
        fs::write(root.join("photo.JPG"), vec![0u8; 300]).unwrap();
        fs::write(root.join("notes.txt"), vec![0u8; 50]).unwrap();
        fs::write(root.join("song.mp3"), vec![0u8; 200]).unwrap();
        fs::create_dir(root.join("media")).unwrap();
        fs::write(root.join("media").join("clip.mkv"), vec![0u8; 100]).unwrap();
        let (_, outcome) = scan(&root, &ScanOptions::default(), &no_cancel(), None);
        let snap = &outcome.snapshot;
        let root_id = root.to_string_lossy().into_owned();

        let names_of = |page: &DiskUsageChildrenPage| -> Vec<String> {
            page.items.iter().map(|i| i.name.clone()).collect()
        };

        // Threshold under the (logical) metric: media dir (100 B) drops too.
        let f = DiskUsageFilter {
            min_bytes: Some(250),
            ..Default::default()
        };
        let page =
            query_children_snapshot(snap, &root_id, 0, 100, DiskUsageSort::default(), &f).unwrap();
        assert_eq!(names_of(&page), vec!["movie.mp4", "photo.JPG"]);
        assert_eq!(page.total, 2, "total is the FILTERED count");

        // Extension filter: case-insensitive, dot optional.
        let f = DiskUsageFilter {
            extensions: Some(vec![".Jpg".into()]),
            ..Default::default()
        };
        let page =
            query_children_snapshot(snap, &root_id, 0, 100, DiskUsageSort::default(), &f).unwrap();
        assert_eq!(names_of(&page), vec!["photo.JPG"]);

        // Category filter matches files only (dirs never classified).
        let f = DiskUsageFilter {
            categories: Some(vec![FileCategory::Video]),
            ..Default::default()
        };
        let page =
            query_children_snapshot(snap, &root_id, 0, 100, DiskUsageSort::default(), &f).unwrap();
        assert_eq!(names_of(&page), vec!["movie.mp4"]);

        // Name substring, case-insensitive.
        let f = DiskUsageFilter {
            name_substring: Some("OTE".into()),
            ..Default::default()
        };
        let page =
            query_children_snapshot(snap, &root_id, 0, 100, DiskUsageSort::default(), &f).unwrap();
        assert_eq!(names_of(&page), vec!["notes.txt"]);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn allocated_metric_sorts_unknown_lowest_and_threshold_excludes_them() {
        // Synthetic: full control over allocated availability (platform-safe).
        let snap = synthetic(
            &[
                ("a", 100, Some(500)), // biggest allocated, small logical
                ("b", 800, None),      // biggest logical, UNKNOWN allocated
                ("c", 0, Some(300)),
            ],
            Some(800),
        );
        let root_id = snap.root_id.clone();

        let f = DiskUsageFilter {
            metric: SizeMetric::Allocated,
            ..Default::default()
        };
        let page =
            query_children_snapshot(&snap, &root_id, 0, 100, DiskUsageSort::LargestFirst, &f)
                .unwrap();
        let names: Vec<&str> = page.items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["a", "c", "b"],
            "allocated desc; unknown sorts as the smallest side (never substituted)"
        );
        let b = page.items.iter().find(|i| i.name == "b").unwrap();
        assert_eq!(b.allocated_bytes, None, "unknown stays null in the payload");
        assert!(!b.allocated_supported);
        assert_eq!(b.logical_bytes, 800, "logical value still present");
        let a = page.items.iter().find(|i| i.name == "a").unwrap();
        assert!(
            (a.percent_of_parent.unwrap() - 500.0 / 800.0 * 100.0).abs() < 1e-9,
            "percent under allocated uses the allocated denominator"
        );
        assert_eq!(b.percent_of_parent, None, "unknown numerator → null, not 0");

        // Threshold under allocated: unknown bytes cannot prove ≥ min → out.
        let f = DiskUsageFilter {
            metric: SizeMetric::Allocated,
            min_bytes: Some(350),
            ..Default::default()
        };
        let page =
            query_children_snapshot(&snap, &root_id, 0, 100, DiskUsageSort::LargestFirst, &f)
                .unwrap();
        let names: Vec<&str> = page.items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, vec!["a"], "only proven-allocated entries survive");

        // Same tree, root allocated unknown: every allocated percent is null.
        let snap = synthetic(&[("a", 100, Some(500)), ("b", 800, None)], None);
        let f = DiskUsageFilter {
            metric: SizeMetric::Allocated,
            ..Default::default()
        };
        let page = query_children_snapshot(
            &snap,
            &snap.root_id,
            0,
            100,
            DiskUsageSort::LargestFirst,
            &f,
        )
        .unwrap();
        assert!(
            page.items
                .iter()
                .all(|i| i.percent_of_parent.is_none() && i.percent_of_scan_root.is_none()),
            "zero/unknown denominator is never a 0%"
        );
    }

    #[test]
    fn top_files_are_recursive_with_truncation_and_label() {
        let root = temp_root("qtop");
        fs::write(root.join("f0.bin"), vec![0u8; 200]).unwrap();
        fs::create_dir(root.join("d1")).unwrap();
        fs::write(root.join("d1").join("f1.bin"), vec![0u8; 100]).unwrap();
        fs::create_dir(root.join("d1").join("d2")).unwrap();
        fs::write(root.join("d1").join("d2").join("f2.bin"), vec![0u8; 300]).unwrap();
        fs::write(root.join("d1").join("d2").join("f3.bin"), vec![0u8; 50]).unwrap();
        let (_, outcome) = scan(&root, &ScanOptions::default(), &no_cancel(), None);
        let snap = &outcome.snapshot;
        let root_id = root.to_string_lossy().into_owned();

        let top = top_files_snapshot(snap, &root_id, 2, &DiskUsageFilter::default()).unwrap();
        assert_eq!(top.total_matching, 4);
        assert!(top.truncated, "4 matches capped at 2 → truncated");
        assert_eq!(top.label, "Top 2");
        assert_eq!(top.limit, 2);
        let names: Vec<&str> = top.items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["f2.bin", "f0.bin"],
            "largest-first across the WHOLE subtree, not per directory"
        );
        let d2_id = root.join("d1").join("d2").to_string_lossy().into_owned();
        assert_eq!(top.items[0].parent_id, d2_id);
        assert_eq!(top.items[0].category, FileCategory::Other);

        let all = top_files_snapshot(snap, &root_id, 10, &DiskUsageFilter::default()).unwrap();
        assert!(!all.truncated);
        assert_eq!(all.items.len(), 4);
        assert_eq!(all.label, "Top 10");

        // Querying a subtree keeps the SCAN ROOT as percent-of-root base and
        // the file's direct parent as percent-of-parent base (§2.7).
        let d1_id = root.join("d1").to_string_lossy().into_owned();
        let sub = top_files_snapshot(snap, &d1_id, 10, &DiskUsageFilter::default()).unwrap();
        assert_eq!(sub.total_matching, 3, "recursive within the subtree only");
        let f2 = sub.items.iter().find(|i| i.name == "f2.bin").unwrap();
        assert!((f2.percent_of_scan_root.unwrap() - 300.0 / 650.0 * 100.0).abs() < 1e-9);
        assert!((f2.percent_of_parent.unwrap() - 300.0 / 350.0 * 100.0).abs() < 1e-9);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn top_files_honor_filters_and_are_files_only() {
        let root = temp_root("qtopf");
        fs::write(root.join("big.zip"), vec![0u8; 500]).unwrap();
        fs::write(root.join("note.txt"), vec![0u8; 400]).unwrap();
        fs::create_dir(root.join("sub")).unwrap();
        fs::write(root.join("sub").join("pic.jpg"), vec![0u8; 300]).unwrap();
        let made = std::os::windows::fs::symlink_file(root.join("big.zip"), root.join("huge.bin"));
        let (_, outcome) = scan(&root, &ScanOptions::default(), &no_cancel(), None);
        let snap = &outcome.snapshot;
        let root_id = root.to_string_lossy().into_owned();
        let base = top_files_snapshot(snap, &root_id, 100, &DiskUsageFilter::default()).unwrap();
        assert_eq!(
            base.total_matching, 3,
            "symlink huge.bin is excluded from top files by KIND (never counted as a file), symlinked or not (created: {made:?})"
        );

        let f = DiskUsageFilter {
            categories: Some(vec![FileCategory::Document]),
            ..Default::default()
        };
        let page = top_files_snapshot(snap, &root_id, 100, &f).unwrap();
        let names: Vec<&str> = page.items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, vec!["note.txt"]);

        let f = DiskUsageFilter {
            extensions: Some(vec!["zip".into()]),
            ..Default::default()
        };
        let page = top_files_snapshot(snap, &root_id, 100, &f).unwrap();
        assert_eq!(
            page.items
                .iter()
                .map(|i| i.name.as_str())
                .collect::<Vec<_>>(),
            vec!["big.zip"]
        );

        let f = DiskUsageFilter {
            min_bytes: Some(350),
            ..Default::default()
        };
        let page = top_files_snapshot(snap, &root_id, 100, &f).unwrap();
        assert_eq!(page.total_matching, 2, "pic.jpg (300) below threshold");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn percent_of_parent_and_root_are_independent_and_exact() {
        let root = fixture(); // 1000 B at root; sub = 500+250; sub2 empty; total 1750
        let (_, outcome) = scan(&root, &ScanOptions::default(), &no_cancel(), None);
        let snap = &outcome.snapshot;
        let root_id = root.to_string_lossy().into_owned();
        let sub_id = root.join("sub").to_string_lossy().into_owned();

        let page = query_children_snapshot(
            snap,
            &root_id,
            0,
            100,
            DiskUsageSort::default(),
            &DiskUsageFilter::default(),
        )
        .unwrap();
        let a = page.items.iter().find(|i| i.name == "a.bin").unwrap();
        assert!((a.percent_of_parent.unwrap() - 1000.0 / 1750.0 * 100.0).abs() < 1e-9);
        assert!((a.percent_of_scan_root.unwrap() - 1000.0 / 1750.0 * 100.0).abs() < 1e-9);

        let page = query_children_snapshot(
            snap,
            &sub_id,
            0,
            100,
            DiskUsageSort::default(),
            &DiskUsageFilter::default(),
        )
        .unwrap();
        let b = page.items.iter().find(|i| i.name == "b.bin").unwrap();
        assert!(
            (b.percent_of_parent.unwrap() - 500.0 / 750.0 * 100.0).abs() < 1e-9,
            "parent denominator is the queried node, not the scan root"
        );
        assert!((b.percent_of_scan_root.unwrap() - 500.0 / 1750.0 * 100.0).abs() < 1e-9);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn percent_is_null_for_zero_denominator_and_partial_coverage() {
        // Complete coverage but ZERO denominator: empty root, empty child.
        let root = temp_root("qzero");
        fs::create_dir(root.join("empty")).unwrap();
        let (_, outcome) = scan(&root, &ScanOptions::default(), &no_cancel(), None);
        assert_eq!(outcome.stats.coverage, Coverage::Complete);
        let page = query_children_snapshot(
            &outcome.snapshot,
            &outcome.snapshot.root_id,
            0,
            100,
            DiskUsageSort::default(),
            &DiskUsageFilter::default(),
        )
        .unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].percent_of_parent, None, "0 B parent → null");
        assert_eq!(page.items[0].percent_of_scan_root, None, "0 B root → null");

        // Partial coverage (depth cap): percents are null even with bytes.
        let root2 = fixture();
        let opts = ScanOptions { max_depth: Some(0) };
        let (_, outcome) = scan(&root2, &opts, &no_cancel(), None);
        assert_eq!(outcome.stats.coverage, Coverage::Partial);
        let page = query_children_snapshot(
            &outcome.snapshot,
            &outcome.snapshot.root_id,
            0,
            100,
            DiskUsageSort::default(),
            &DiskUsageFilter::default(),
        )
        .unwrap();
        assert_eq!(page.coverage, Coverage::Partial);
        let a = page.items.iter().find(|i| i.name == "a.bin").unwrap();
        assert_eq!(a.logical_bytes, 1000, "data itself is present");
        assert_eq!(a.percent_of_parent, None, "partial coverage → null, not 0%");
        assert_eq!(a.percent_of_scan_root, None);
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&root2);
    }

    #[test]
    fn unknown_node_and_evicted_scan_are_structured_not_found() {
        let snap = synthetic(&[], None);
        for err in [
            query_children_snapshot(
                &snap,
                r"C:\elsewhere",
                0,
                10,
                DiskUsageSort::default(),
                &DiskUsageFilter::default(),
            )
            .unwrap_err(),
            top_files_snapshot(&snap, r"C:\elsewhere", 10, &DiskUsageFilter::default())
                .unwrap_err(),
        ] {
            assert_eq!(err.code, ErrorCode::NotFound, "node outside the snapshot");
        }
        // Unknown/evicted scan id (global store): structured NotFound naming it.
        let missing = format!("du-missing-{}", SEQ.fetch_add(1, Ordering::SeqCst));
        let err = query_children_by_scan(
            &missing,
            r"C:\x",
            0,
            10,
            DiskUsageSort::default(),
            &DiskUsageFilter::default(),
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);
        assert_eq!(err.path.as_deref(), Some(missing.as_str()));
        let err =
            top_files_by_scan(&missing, r"C:\x", 10, &DiskUsageFilter::default()).unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);
    }

    #[test]
    fn snapshot_version_increments_and_responses_carry_it() {
        let root = fixture();
        let (_, o1) = scan(&root, &ScanOptions::default(), &no_cancel(), None);
        let (_, o2) = scan(&root, &ScanOptions::default(), &no_cancel(), None);
        assert!(
            o2.snapshot.version > o1.snapshot.version,
            "monotonic per process"
        );
        assert!(o1.snapshot.scanned_at_ms > 0);
        assert!(o2.snapshot.scanned_at_ms >= o1.snapshot.scanned_at_ms);
        let root_id = root.to_string_lossy().into_owned();
        let page = query_children_snapshot(
            &o2.snapshot,
            &root_id,
            0,
            10,
            DiskUsageSort::default(),
            &DiskUsageFilter::default(),
        )
        .unwrap();
        assert_eq!(page.version, o2.snapshot.version);
        assert_eq!(page.scanned_at_ms, o2.snapshot.scanned_at_ms);
        let top =
            top_files_snapshot(&o2.snapshot, &root_id, 10, &DiskUsageFilter::default()).unwrap();
        assert_eq!(top.version, o2.snapshot.version);
        assert_eq!(top.scanned_at_ms, o2.snapshot.scanned_at_ms);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn category_map_classifies_by_extension_case_insensitively() {
        assert_eq!(category_of("a.mp4"), FileCategory::Video);
        assert_eq!(category_of("B.JPG"), FileCategory::Image);
        assert_eq!(category_of("c.Mp3"), FileCategory::Audio);
        assert_eq!(category_of("d.PDF"), FileCategory::Document);
        assert_eq!(category_of("e.7z"), FileCategory::Archive);
        assert_eq!(category_of("f.unknownxyz"), FileCategory::Other);
        assert_eq!(category_of("noext"), FileCategory::Other);
        assert_eq!(category_of("multi.tar.gz"), FileCategory::Archive);
        assert_eq!(category_of(""), FileCategory::Other);
    }

    // ── DSK-09 (FER-405): parallel pool — parity, cancel, coverage ────────

    /// Deterministic nested tree: `branches × subs` dirs with `leaves` leaf
    /// dirs each, `files` files per leaf, plus one direct file per branch —
    /// mixed depths so subtree rollups are non-trivial.
    fn nested_fixture(
        tag: &str,
        branches: usize,
        subs: usize,
        leaves: usize,
        files: usize,
    ) -> TempDir {
        let root = temp_root(tag);
        let mut seq = 0usize;
        for b in 0..branches {
            let bp = root.join(format!("b{b}"));
            fs::create_dir_all(&bp).unwrap();
            fs::write(bp.join("top.bin"), vec![0u8; seq * 37 % 1000]).unwrap();
            seq += 1;
            for s in 0..subs {
                for l in 0..leaves {
                    let leaf = bp.join(format!("s{s}")).join(format!("l{l}"));
                    fs::create_dir_all(&leaf).unwrap();
                    for f in 0..files {
                        let size = seq.wrapping_mul(37) % 1000;
                        fs::write(leaf.join(format!("f{f}.bin")), vec![0u8; size]).unwrap();
                        seq += 1;
                    }
                }
            }
        }
        root
    }

    fn scan_workers(
        workers: usize,
        root: &Path,
        options: &ScanOptions,
        cancel: &AtomicBool,
        throttle: Option<Duration>,
    ) -> (Vec<DiskUsageEvent>, ScanOutcome) {
        let events = std::sync::Mutex::new(Vec::new());
        let outcome = run_scan_with_workers(
            root.to_str().unwrap(),
            options,
            cancel,
            "du-par",
            throttle,
            workers,
            |ev| events.lock().unwrap().push(ev.clone()),
        )
        .expect("scan runs");
        (events.into_inner().unwrap(), outcome)
    }

    #[test]
    fn parallel_pool_totals_match_serial_reference_node_per_node() {
        let root = nested_fixture("parity", 5, 4, 3, 2);
        let opts = ScanOptions::default();
        let (ser_events, ser) = scan_workers(1, &root, &opts, &no_cancel(), None);
        let (par_events, par) = scan_workers(8, &root, &opts, &no_cancel(), None);

        // Stats identical except wall-clock duration (nondeterministic).
        let (a, b) = (&ser.stats, &par.stats);
        assert_eq!(a.scanned_files, b.scanned_files);
        assert_eq!(a.scanned_dirs, b.scanned_dirs);
        assert_eq!(a.scanned_symlinks, b.scanned_symlinks);
        assert_eq!(a.skipped_volume_boundaries, b.skipped_volume_boundaries);
        assert_eq!(a.logical_bytes, b.logical_bytes);
        assert_eq!(a.allocated_available_files, b.allocated_available_files);
        assert_eq!(a.allocated_unavailable_files, b.allocated_unavailable_files);
        assert_eq!(a.known_allocated_bytes, b.known_allocated_bytes);
        assert_eq!(a.total_errors, b.total_errors);
        assert_eq!(a.coverage, b.coverage);
        assert_eq!(a.coverage, Coverage::Complete);
        assert_eq!(a.cancelled, b.cancelled);
        assert_eq!(a.depth_capped, b.depth_capped);
        assert!(a.scanned_files > 0, "fixture must not be trivial");

        // Node map: same ids, every scalar identical, children SET identical.
        // (children_ids ORDER may differ under parallelism — every consumer
        // sorts deterministically by bytes + stable id.)
        assert_eq!(ser.snapshot.nodes.len(), par.snapshot.nodes.len());
        for (id, sn) in &ser.snapshot.nodes {
            let pn = par
                .snapshot
                .nodes
                .get(id)
                .unwrap_or_else(|| panic!("node {id} missing from the parallel snapshot"));
            assert_eq!(sn.parent_id, pn.parent_id, "parent of {id}");
            assert_eq!(sn.name, pn.name, "name of {id}");
            assert_eq!(sn.direct_files, pn.direct_files, "direct_files of {id}");
            assert_eq!(
                sn.direct_symlinks, pn.direct_symlinks,
                "direct_symlinks of {id}"
            );
            assert_eq!(
                sn.direct_logical_bytes, pn.direct_logical_bytes,
                "direct_logical_bytes of {id}"
            );
            assert_eq!(
                sn.direct_allocated_bytes, pn.direct_allocated_bytes,
                "direct_allocated_bytes of {id}"
            );
            assert_eq!(sn.subtree_files, pn.subtree_files, "subtree_files of {id}");
            assert_eq!(sn.subtree_dirs, pn.subtree_dirs, "subtree_dirs of {id}");
            assert_eq!(
                sn.subtree_symlinks, pn.subtree_symlinks,
                "subtree_symlinks of {id}"
            );
            assert_eq!(
                sn.subtree_logical_bytes, pn.subtree_logical_bytes,
                "subtree_logical_bytes of {id}"
            );
            assert_eq!(
                sn.subtree_allocated_bytes, pn.subtree_allocated_bytes,
                "subtree_allocated_bytes of {id}"
            );
            assert_eq!(
                sn.subtree_errors, pn.subtree_errors,
                "subtree_errors of {id}"
            );
            assert_eq!(
                sn.skipped_volume_boundaries, pn.skipped_volume_boundaries,
                "skipped volumes of {id}"
            );
            assert_eq!(
                sn.subtree_skipped_volumes, pn.subtree_skipped_volumes,
                "subtree skipped volumes of {id}"
            );
            let mut sc = sn.children_ids.clone();
            let mut pc = pn.children_ids.clone();
            sc.sort();
            pc.sort();
            assert_eq!(sc, pc, "children set of {id}");
        }

        // Query index: per-directory listing order is preserved exactly,
        // even though different directories were scanned by other workers.
        assert_eq!(
            ser.snapshot.files_by_parent.len(),
            par.snapshot.files_by_parent.len()
        );
        for (parent, entries) in &ser.snapshot.files_by_parent {
            assert_eq!(
                Some(entries),
                par.snapshot.files_by_parent.get(parent),
                "files of {parent} identical including order"
            );
        }

        // Both streams end with exactly one done event carrying the totals.
        for evs in [&ser_events, &par_events] {
            assert_eq!(evs.last().map(|e| e.done), Some(true));
            assert_eq!(evs.iter().filter(|e| e.done).count(), 1);
        }
        assert_eq!(
            ser_events.last().unwrap().known_logical_bytes,
            par_events.last().unwrap().known_logical_bytes
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn parallel_cancel_mid_scan_stops_pool_without_zombies() {
        // ~7k entries: orders of magnitude more than one chunk, so the walk
        // is nowhere near done when the first progress chunk streams. The
        // cancel is triggered FROM the sink on that very first chunk — the
        // aggregator thread itself, synchronously — which makes "cancel
        // lands mid-walk" deterministic instead of a polling race against
        // fast hardware.
        let root = nested_fixture("parcancel", 25, 25, 2, 5);
        let seen = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let flag = Arc::new(AtomicBool::new(false));
        let root_str = root.to_string_lossy().into_owned();
        let worker = {
            let seen_t = Arc::clone(&seen);
            let flag_t = Arc::clone(&flag);
            std::thread::spawn(move || {
                run_scan_with_workers(
                    &root_str,
                    &ScanOptions::default(),
                    &flag,
                    "du-parcancel",
                    Some(Duration::ZERO),
                    8,
                    move |_| {
                        if seen_t.fetch_add(1, Ordering::SeqCst) == 0 {
                            flag_t.store(true, Ordering::SeqCst);
                        }
                    },
                )
                .expect("cancelled scan returns")
            })
        };
        // join() returning IS the no-zombie proof: std::thread::scope joins
        // every pool worker before run_scan_with_workers can return.
        let outcome = worker.join().expect("worker thread joins");
        assert!(seen.load(Ordering::SeqCst) >= 1, "scan never progressed");
        assert!(outcome.stats.cancelled);
        assert_eq!(outcome.stats.coverage, Coverage::Partial);
        assert!(!outcome.stats.depth_capped);

        // The pool is a per-scan construct: a fresh scan still completes.
        let (_, second) = scan_workers(8, &root, &ScanOptions::default(), &no_cancel(), None);
        assert_eq!(second.stats.coverage, Coverage::Complete);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn depth_cap_coverage_and_counts_consistent_between_serial_and_parallel() {
        let root = nested_fixture("parcap", 3, 3, 3, 2);
        // Cap at depth 2: b* (depth 1) and s* (depth 2) are entered, the
        // leaf dirs (depth 3) are not — a partial-coverage walk that still
        // counts everything it actually reached, identical in both modes.
        let opts = ScanOptions { max_depth: Some(2) };
        let (_, ser) = scan_workers(1, &root, &opts, &no_cancel(), None);
        let (_, par) = scan_workers(4, &root, &opts, &no_cancel(), None);
        for o in [&ser, &par] {
            assert!(o.stats.depth_capped);
            assert_eq!(o.stats.coverage, Coverage::Partial);
        }
        assert_eq!(ser.stats.scanned_dirs, par.stats.scanned_dirs);
        assert_eq!(ser.stats.scanned_files, par.stats.scanned_files);
        assert_eq!(ser.stats.logical_bytes, par.stats.logical_bytes);
        assert_eq!(ser.stats.total_errors, par.stats.total_errors);
        assert_eq!(ser.stats.total_errors, 0);
        assert_eq!(
            ser.snapshot.nodes.len(),
            par.snapshot.nodes.len(),
            "same set of visited nodes under the cap"
        );
        let root_id = root.to_string_lossy().into_owned();
        for snap in [&ser.snapshot, &par.snapshot] {
            let r = &snap.nodes[&root_id];
            assert_eq!(
                r.subtree_files, 3,
                "branch top files counted, capped leaves not entered"
            );
            assert_eq!(r.subtree_dirs, 12, "b* + s* dirs; leaf dirs not entered");
            assert!(r.subtree_logical_bytes > 0);
        }
        for (id, sn) in &ser.snapshot.nodes {
            let pn = &par.snapshot.nodes[id];
            assert_eq!(sn.subtree_files, pn.subtree_files, "files of {id}");
            assert_eq!(
                sn.subtree_logical_bytes, pn.subtree_logical_bytes,
                "bytes of {id}"
            );
            assert_eq!(sn.subtree_errors, pn.subtree_errors, "error rollup of {id}");
        }
        let _ = fs::remove_dir_all(&root);
    }
}
