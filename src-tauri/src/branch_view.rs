//! Branch View (F2-23): every file of a subtree as one flat listing.
//! Kept Tauri-free for unit testing; `ipc.rs` exposes `list_branch`.
//!
//! Traversal rules: iterative explicit stack (deep trees cannot overflow),
//! entries described via lstat, and symlinks/reparse points are listed as
//! entries but NEVER descended into (decision D07) — which also makes
//! symlink loops structurally impossible. Per-directory read failures are
//! recorded and the walk continues. `truncated` reports that the
//! `max_entries` cap cut the listing short rather than pretending the
//! subtree was fully covered.

use crate::domain::{ErrorCode, FileEntry, FsError};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// One flattened subtree listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchResult {
    /// The root the branch was built from.
    pub root: String,
    /// Files of the subtree (directories excluded, links never followed).
    pub entries: Vec<FileEntry>,
    /// True when max_entries stopped the walk before the subtree ended.
    pub truncated: bool,
    /// Directories that could not be read, with the OS reason.
    pub failed: Vec<FailedDir>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FailedDir {
    pub path: String,
    pub code: ErrorCode,
    pub message: String,
}

/// Default safety cap (matches the 100k virtualized-render budget).
pub const DEFAULT_MAX_ENTRIES: u64 = 100_000;

/// FER-128: streaming tunables. A batch is flushed when `flush_interval` has
/// passed since the last flush (so the first rows show up within ~100 ms)
/// or when `batch_size` entries piled up (bounds one IPC payload to a few
/// hundred KB). Together they cap the event rate at ~10/s per walk.
#[derive(Debug, Clone, Copy)]
pub struct StreamOptions {
    pub max_entries: u64,
    pub batch_size: usize,
    pub flush_interval: Duration,
}

impl StreamOptions {
    pub fn with_max(max_entries: u64) -> Self {
        Self {
            max_entries,
            batch_size: DEFAULT_BATCH_SIZE,
            flush_interval: DEFAULT_FLUSH_INTERVAL,
        }
    }
}

pub const DEFAULT_BATCH_SIZE: usize = 5_000;
pub const DEFAULT_FLUSH_INTERVAL: Duration = Duration::from_millis(100);

/// One streamed slice of the walk.
#[derive(Debug, Default)]
pub struct BranchBatch {
    pub entries: Vec<FileEntry>,
    /// Directories that failed since the previous batch.
    pub failed: Vec<FailedDir>,
}

/// Final tally of a walk (the entries themselves were streamed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchSummary {
    pub root: String,
    pub total: u64,
    pub truncated: bool,
    pub cancelled: bool,
    pub failed: Vec<FailedDir>,
    pub duration_ms: u64,
}

/// `branch://progress` payload. Batches carry `entries`/`failed`; the single
/// final event has `done = true` plus `summary` (or `error` when the root
/// itself could not be walked).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchEvent {
    pub id: String,
    pub entries: Vec<FileEntry>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub failed: Vec<FailedDir>,
    pub done: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<BranchDone>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<FsError>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchDone {
    pub total: u64,
    pub truncated: bool,
    pub cancelled: bool,
    pub failed_count: u64,
    pub duration_ms: u64,
}

impl BranchEvent {
    pub fn batch(id: &str, batch: BranchBatch) -> Self {
        Self {
            id: id.to_string(),
            entries: batch.entries,
            failed: batch.failed,
            done: false,
            summary: None,
            error: None,
        }
    }

    pub fn finished(id: &str, result: Result<BranchSummary, FsError>) -> Self {
        let (summary, error) = match result {
            Ok(s) => (
                Some(BranchDone {
                    total: s.total,
                    truncated: s.truncated,
                    cancelled: s.cancelled,
                    failed_count: s.failed.len() as u64,
                    duration_ms: s.duration_ms,
                }),
                None,
            ),
            Err(e) => (None, Some(e)),
        };
        Self {
            id: id.to_string(),
            entries: Vec::new(),
            failed: Vec::new(),
            done: true,
            summary,
            error,
        }
    }
}

/// Registry of in-flight streaming walks: id -> cancel flag.
pub fn active_cancels() -> &'static Mutex<HashMap<String, Arc<AtomicBool>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, Arc<AtomicBool>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Walk `root` and return every regular file (plus non-dir symlinks
/// themselves) as one flat list. See module docs for the rules. Collects the
/// stream; the app itself uses [`walk_branch`] so no giant payload exists.
pub fn list_branch(root: &str, max_entries: u64) -> Result<BranchResult, FsError> {
    let mut entries = Vec::new();
    let cancel = AtomicBool::new(false);
    let summary = walk_branch(
        root,
        StreamOptions::with_max(max_entries),
        &cancel,
        &mut |batch| entries.extend(batch.entries),
    )?;
    Ok(BranchResult {
        root: summary.root,
        entries,
        truncated: summary.truncated,
        failed: summary.failed,
    })
}

/// Streaming, cancellable traversal. Batches go to `sink` as they fill (see
/// [`StreamOptions`]); the returned summary reports truncation, cancel and
/// per-directory failures. A cancelled walk still returns `Ok` with
/// `cancelled = true` and whatever was already streamed stays valid.
pub fn walk_branch(
    root: &str,
    opts: StreamOptions,
    cancel: &AtomicBool,
    sink: &mut dyn FnMut(BranchBatch),
) -> Result<BranchSummary, FsError> {
    let started = Instant::now();
    let root_path = Path::new(root);
    let meta = fs::metadata(root_path).map_err(|e| FsError::from(e).with_path(root))?;
    if !meta.is_dir() {
        return Err(
            FsError::new(ErrorCode::NotADirectory, "Branch View needs a directory").with_path(root),
        );
    }
    let mut total: u64 = 0;
    let mut truncated = false;
    let mut cancelled = false;
    let mut failed_all: Vec<FailedDir> = Vec::new();
    let mut batch = BranchBatch::default();
    let mut last_flush = Instant::now();
    let mut flush = |batch: &mut BranchBatch, last_flush: &mut Instant| {
        if !batch.entries.is_empty() || !batch.failed.is_empty() {
            sink(std::mem::take(batch));
        }
        *last_flush = Instant::now();
    };
    let mut stack: Vec<PathBuf> = vec![root_path.to_path_buf()];
    let mut since_clock_check = 0u32;
    'walk: while let Some(dir) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            cancelled = true;
            break;
        }
        if total >= opts.max_entries {
            truncated = true;
            break;
        }
        let read = match fs::read_dir(&dir) {
            Ok(read) => read,
            Err(e) => {
                let fe = FsError::from(e);
                // The root itself failing is fatal; subdirectories degrade.
                if dir == root_path {
                    return Err(fe.with_path(dir.to_string_lossy()));
                }
                let failed = FailedDir {
                    path: dir.to_string_lossy().into_owned(),
                    code: fe.code,
                    message: fe.message,
                };
                batch.failed.push(failed.clone());
                failed_all.push(failed);
                continue;
            }
        };
        for item in read {
            if cancel.load(Ordering::Relaxed) {
                cancelled = true;
                break 'walk;
            }
            let Ok(item) = item else {
                continue; // iterator-level failure: skip, keep the walk alive
            };
            let Ok(file_type) = item.file_type() else {
                continue;
            };
            let Ok(lm) = fs::symlink_metadata(item.path()) else {
                continue;
            };
            let is_link = file_type.is_symlink();
            if lm.is_dir() {
                if !is_link {
                    stack.push(item.path()); // real directory: descend later
                }
                continue; // real directories are not listed
            }
            if total >= opts.max_entries {
                truncated = true;
                break 'walk;
            }
            batch.entries.push(FileEntry {
                name: item.file_name().to_string_lossy().into_owned(),
                path: item.path().to_string_lossy().into_owned(),
                is_dir: false,
                is_symlink: is_link,
                size: lm.len(),
                modified_ms: modified_ms(&lm),
                hidden: is_hidden(&item.file_name().to_string_lossy(), &lm),
            });
            total += 1;
            since_clock_check += 1;
            if batch.entries.len() >= opts.batch_size {
                flush(&mut batch, &mut last_flush);
                since_clock_check = 0;
            } else if since_clock_check >= 64 {
                since_clock_check = 0;
                if last_flush.elapsed() >= opts.flush_interval {
                    flush(&mut batch, &mut last_flush);
                }
            }
        }
        // Directory boundary: also flush on time (slow directories, failures).
        if last_flush.elapsed() >= opts.flush_interval {
            flush(&mut batch, &mut last_flush);
        }
    }
    flush(&mut batch, &mut last_flush);
    Ok(BranchSummary {
        root: root.to_string(),
        total,
        truncated,
        cancelled,
        failed: failed_all,
        duration_ms: started.elapsed().as_millis() as u64,
    })
}

fn modified_ms(meta: &fs::Metadata) -> i64 {
    use std::time::UNIX_EPOCH;
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Hidden = dot-prefix (Unix convention) or the Windows hidden attribute.
fn is_hidden(name: &str, lm: &fs::Metadata) -> bool {
    if name.starts_with('.') {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        lm.file_attributes() & 0x2 != 0
    }
    #[cfg(not(windows))]
    {
        let _ = (name, lm);
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQ: AtomicU64 = AtomicU64::new(0);

    /// Owned temp dir under %TEMP%, removed on Drop (also on assertion failure or early return).
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "ferrum-f223-{tag}-{}-{}",
                std::process::id(),
                SEQ.fetch_add(1, Ordering::SeqCst)
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
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

    fn temp(tag: &str) -> TempDir {
        TempDir::new(tag)
    }

    fn s(p: &Path) -> String {
        p.to_string_lossy().into_owned()
    }

    fn paths(result: &BranchResult) -> Vec<String> {
        result.entries.iter().map(|e| e.name.clone()).collect()
    }

    #[test]
    fn flattens_a_nested_subtree_with_all_files() {
        let root = temp("flat");
        fs::write(root.join("top.txt"), b"1").unwrap();
        fs::create_dir_all(root.join("a/b")).unwrap();
        fs::write(root.join("a").join("mid.txt"), b"22").unwrap();
        fs::write(root.join("a").join("b").join("deep.txt"), b"333").unwrap();
        let result = list_branch(&s(&root), DEFAULT_MAX_ENTRIES).unwrap();
        let mut names = paths(&result);
        names.sort();
        assert_eq!(names, vec!["deep.txt", "mid.txt", "top.txt"]);
        assert!(!result.truncated);
        assert!(result.failed.is_empty());
        // Entries carry their ORIGINAL deep paths, not bare names.
        let deep = result
            .entries
            .iter()
            .find(|e| e.name == "deep.txt")
            .unwrap();
        assert_eq!(deep.path, s(&root.join("a").join("b").join("deep.txt")));
        assert_eq!(deep.size, 3);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn directories_are_not_listed_and_dir_symlinks_are_never_followed() {
        let root = temp("links");
        fs::create_dir(root.join("real")).unwrap();
        fs::write(root.join("real").join("inside.txt"), b"x").unwrap();
        fs::write(root.join("outside.txt"), b"y").unwrap();
        let link = root.join("link");
        let made = std::os::windows::fs::symlink_dir(root.join("real"), &link);
        if made.is_err() {
            return;
        }
        let result = list_branch(&s(&root), DEFAULT_MAX_ENTRIES).unwrap();
        let mut names = paths(&result);
        names.sort();
        // `inside.txt` appears once (via `real`), never a second time via
        // `link`; the dir link itself is listed as a symlink entry (D07).
        assert_eq!(names, vec!["inside.txt", "link", "outside.txt"]);
        let link_entry = result.entries.iter().find(|e| e.name == "link").unwrap();
        assert!(link_entry.is_symlink);
        assert!(!link_entry.is_dir);
        assert!(result.failed.is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_dir_symlink_loop_cannot_explode_the_walk() {
        let root = temp("loop");
        fs::write(root.join("f.txt"), b"x").unwrap();
        let inner = root.join("inner");
        fs::create_dir(&inner).unwrap();
        let made = std::os::windows::fs::symlink_dir(&root, inner.join("up"));
        if made.is_err() {
            return;
        }
        let result = list_branch(&s(&root), DEFAULT_MAX_ENTRIES).unwrap();
        // The loop link is listed but never descended into, so `f.txt`
        // appears exactly once.
        let mut names = paths(&result);
        names.sort();
        assert_eq!(names, vec!["f.txt", "up"]);
        assert!(
            result
                .entries
                .iter()
                .find(|e| e.name == "up")
                .unwrap()
                .is_symlink
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn file_symlinks_are_listed_without_reading_their_targets() {
        let root = temp("filelink");
        let target = root.join("target.txt");
        fs::write(&target, b"hello").unwrap();
        let link = root.join("link.txt");
        let made = std::os::windows::fs::symlink_file(&target, &link);
        if made.is_err() {
            return;
        }
        let result = list_branch(&s(&root), DEFAULT_MAX_ENTRIES).unwrap();
        assert_eq!(result.entries.len(), 2);
        let link_entry = result
            .entries
            .iter()
            .find(|e| e.name == "link.txt")
            .unwrap();
        assert!(link_entry.is_symlink);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn truncation_is_reported_honestly() {
        let root = temp("trunc");
        for i in 0..50 {
            fs::write(root.join(format!("f{i:02}.txt")), b"").unwrap();
        }
        let result = list_branch(&s(&root), 10).unwrap();
        assert_eq!(result.entries.len(), 10);
        assert!(result.truncated);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn duplicate_basenames_keep_distinct_original_paths() {
        let root = temp("dupes");
        fs::create_dir_all(root.join("x")).unwrap();
        fs::create_dir_all(root.join("y")).unwrap();
        fs::write(root.join("x").join("same.txt"), b"1").unwrap();
        fs::write(root.join("y").join("same.txt"), b"2").unwrap();
        let result = list_branch(&s(&root), DEFAULT_MAX_ENTRIES).unwrap();
        assert_eq!(result.entries.len(), 2);
        let mut p: Vec<&str> = result.entries.iter().map(|e| e.path.as_str()).collect();
        p.sort();
        assert_ne!(p[0], p[1]);
        assert!(p[0].contains("x") || p[0].contains("y"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn deep_nesting_does_not_overflow_the_stack() {
        let root = temp("deep");
        let mut dir = root.to_path_buf();
        for i in 0..100 {
            dir = dir.join(format!("d{i}"));
        }
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("bottom.txt"), b"end").unwrap();
        let result = list_branch(&s(&root), DEFAULT_MAX_ENTRIES).unwrap();
        assert_eq!(paths(&result), vec!["bottom.txt"]);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_root_is_not_found_and_a_file_root_is_not_a_directory() {
        let err = list_branch(r"Z:\nope", DEFAULT_MAX_ENTRIES).unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);
        let root = temp("filecase");
        fs::write(root.join("plain.txt"), b"x").unwrap();
        let err = list_branch(&s(&root.join("plain.txt")), DEFAULT_MAX_ENTRIES).unwrap_err();
        assert_eq!(err.code, ErrorCode::NotADirectory);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn result_serializes_camel_case() {
        let root = temp("serde");
        fs::write(root.join("a.txt"), b"1").unwrap();
        let result = list_branch(&s(&root), DEFAULT_MAX_ENTRIES).unwrap();
        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["root"], s(&root));
        assert_eq!(json["truncated"], serde_json::Value::Bool(false));
        assert!(json["entries"].as_array().unwrap()[0]["modifiedMs"].is_i64());
        let _ = fs::remove_dir_all(&root);
    }

    // ── FER-128 / FER-130: streaming, cancel, scale, access denied ───────

    /// `dirs` directories with `per_dir` empty files each (`d000/f00000.txt`...).
    fn make_fixture(tag: &str, dirs: usize, per_dir: usize) -> TempDir {
        let root = temp(tag);
        for d in 0..dirs {
            let dir = root.join(format!("d{d:03}"));
            fs::create_dir(&dir).unwrap();
            for f in 0..per_dir {
                fs::File::create(dir.join(format!("f{f:05}.txt"))).unwrap();
            }
        }
        root
    }

    fn walk_collect(
        root: &Path,
        opts: StreamOptions,
        cancel: &AtomicBool,
    ) -> (Vec<BranchBatch>, Result<BranchSummary, FsError>) {
        let mut batches = Vec::new();
        let summary = walk_branch(&s(root), opts, cancel, &mut |b| batches.push(b));
        (batches, summary)
    }

    #[test]
    fn results_stream_in_bounded_batches_and_equal_the_collected_listing() {
        let root = make_fixture("stream", 10, 105); // 1050 files
        let opts = StreamOptions {
            max_entries: DEFAULT_MAX_ENTRIES,
            batch_size: 100,
            flush_interval: Duration::from_secs(3600),
        };
        let (batches, summary) = walk_collect(&root, opts, &AtomicBool::new(false));
        let summary = summary.unwrap();
        assert_eq!(summary.total, 1050);
        assert!(!summary.truncated && !summary.cancelled);
        assert!(batches.len() >= 10, "got {} batches", batches.len());
        assert!(batches.iter().all(|b| b.entries.len() <= 100));
        let streamed: usize = batches.iter().map(|b| b.entries.len()).sum();
        assert_eq!(streamed, 1050);
        let mut paths: Vec<String> = batches
            .iter()
            .flat_map(|b| b.entries.iter().map(|e| e.path.clone()))
            .collect();
        paths.sort();
        paths.dedup();
        assert_eq!(paths.len(), 1050, "no entry streamed twice");
        let collected = list_branch(&s(&root), DEFAULT_MAX_ENTRIES).unwrap();
        assert_eq!(collected.entries.len(), 1050);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn time_based_flush_delivers_partial_batches_before_the_size_cap() {
        let root = make_fixture("timeflush", 4, 200);
        let opts = StreamOptions {
            max_entries: DEFAULT_MAX_ENTRIES,
            batch_size: 1_000_000,
            flush_interval: Duration::ZERO, // every clock check flushes
        };
        let (batches, summary) = walk_collect(&root, opts, &AtomicBool::new(false));
        assert_eq!(summary.unwrap().total, 800);
        assert!(batches.len() > 1, "interval flush must split the walk");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn the_item_cap_applies_to_the_stream_and_reports_truncation() {
        let root = make_fixture("capstream", 5, 100);
        let mut opts = StreamOptions::with_max(250);
        opts.batch_size = 50;
        let (batches, summary) = walk_collect(&root, opts, &AtomicBool::new(false));
        let summary = summary.unwrap();
        assert!(summary.truncated);
        assert_eq!(summary.total, 250);
        let streamed: usize = batches.iter().map(|b| b.entries.len()).sum();
        assert_eq!(streamed, 250);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn cancel_in_the_middle_stops_the_walk_and_keeps_streamed_rows_valid() {
        let root = make_fixture("cancel", 20, 100); // 2000 files
        let cancel = AtomicBool::new(false);
        let mut seen = 0usize;
        let mut opts = StreamOptions::with_max(DEFAULT_MAX_ENTRIES);
        opts.batch_size = 100;
        let summary = walk_branch(&s(&root), opts, &cancel, &mut |b| {
            seen += b.entries.len();
            if seen >= 300 {
                cancel.store(true, Ordering::Relaxed); // the user pressed Esc
            }
        })
        .unwrap();
        assert!(summary.cancelled, "cancel is reported, not hidden");
        assert!(!summary.truncated);
        assert!((300..2000).contains(&seen), "stopped mid-walk, saw {seen}");
        assert_eq!(
            summary.total as usize, seen,
            "nothing dropped after the flush"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn cancel_before_the_first_entry_streams_nothing() {
        let root = make_fixture("precancel", 3, 10);
        let (batches, summary) = walk_collect(
            &root,
            StreamOptions::with_max(DEFAULT_MAX_ENTRIES),
            &AtomicBool::new(true),
        );
        let summary = summary.unwrap();
        assert!(summary.cancelled);
        assert_eq!(summary.total, 0);
        assert!(batches.is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn the_registry_cancel_flag_round_trips() {
        let flag = Arc::new(AtomicBool::new(false));
        active_cancels()
            .lock()
            .unwrap()
            .insert("t-reg".into(), flag.clone());
        let taken = active_cancels().lock().unwrap().remove("t-reg").unwrap();
        taken.store(true, Ordering::Relaxed);
        assert!(flag.load(Ordering::Relaxed));
        assert!(active_cancels().lock().unwrap().remove("t-reg").is_none());
    }

    /// FER-130: a REAL ACL-denied directory. The current user is denied
    /// "List folder" (RD) on `locked` through the Windows ACL tool (icacls
    /// ships with Windows); no admin is needed because the owner of a folder
    /// can always edit its DACL, and that same call lifts the deny again.
    #[cfg(windows)]
    #[test]
    fn access_denied_directory_is_reported_and_the_walk_continues() {
        let root = temp("acl");
        fs::write(root.join("visible.txt"), b"1").unwrap();
        let locked = root.join("locked");
        fs::create_dir(&locked).unwrap();
        fs::write(locked.join("secret.txt"), b"2").unwrap();
        fs::create_dir(root.join("open")).unwrap();
        fs::write(root.join("open").join("fine.txt"), b"3").unwrap();

        let user = std::env::var("USERNAME").expect("USERNAME");
        let domain = std::env::var("USERDOMAIN").unwrap_or_default();
        let who = if domain.is_empty() {
            user
        } else {
            format!("{domain}\\{user}")
        };
        let icacls = |args: &[&str]| {
            std::process::Command::new("icacls")
                .arg(&locked)
                .args(args)
                .output()
                .expect("icacls")
        };
        let denied = icacls(&["/deny", &format!("{who}:(RD)")]);
        assert!(
            denied.status.success(),
            "icacls deny failed: {}",
            String::from_utf8_lossy(&denied.stdout)
        );
        let outcome = list_branch(&s(&root), DEFAULT_MAX_ENTRIES);
        let _ = icacls(&["/remove:d", &who]); // always lift the deny first
        let result = outcome.unwrap();
        let mut names = paths(&result);
        names.sort();
        assert_eq!(names, vec!["fine.txt", "visible.txt"]);
        assert_eq!(result.failed.len(), 1, "{:?}", result.failed);
        assert_eq!(result.failed[0].path, s(&locked));
        assert_eq!(result.failed[0].code, ErrorCode::PermissionDenied);
        // The streaming API reports the same failure in a batch.
        let _ = icacls(&["/deny", &format!("{who}:(RD)")]);
        let (batches, summary) = walk_collect(
            &root,
            StreamOptions::with_max(DEFAULT_MAX_ENTRIES),
            &AtomicBool::new(false),
        );
        let _ = icacls(&["/remove:d", &who]);
        assert_eq!(summary.unwrap().failed.len(), 1);
        assert_eq!(
            batches.iter().map(|b| b.failed.len()).sum::<usize>(),
            1,
            "the failure also streams"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn ten_thousand_entries_stream_completely() {
        let root = make_fixture("10k", 50, 200);
        let (batches, summary) = walk_collect(
            &root,
            StreamOptions::with_max(DEFAULT_MAX_ENTRIES),
            &AtomicBool::new(false),
        );
        let summary = summary.unwrap();
        assert_eq!(summary.total, 10_000);
        assert!(!summary.truncated);
        assert_eq!(
            batches.iter().map(|b| b.entries.len()).sum::<usize>(),
            10_000
        );
        let _ = fs::remove_dir_all(&root);
    }

    /// FER-130 scale test (slow to create: ~100k files on NTFS), run with
    /// `cargo test --lib branch_view -- --ignored --nocapture`. 100 500 files
    /// so the 100 000 cap must fire; prints time-to-first-batch and total time.
    #[test]
    #[ignore = "creates 100 500 files; run explicitly for the FER-128/130 numbers"]
    fn hundred_thousand_entries_stream_with_a_fast_first_batch() {
        let create = Instant::now();
        let root = make_fixture("100k", 100, 1005);
        let created = create.elapsed();
        let started = Instant::now();
        let mut first: Option<Duration> = None;
        let mut batches = 0usize;
        let mut max_batch = 0usize;
        let mut count = 0usize;
        let summary = walk_branch(
            &s(&root),
            StreamOptions::with_max(DEFAULT_MAX_ENTRIES),
            &AtomicBool::new(false),
            &mut |b| {
                first.get_or_insert_with(|| started.elapsed());
                batches += 1;
                max_batch = max_batch.max(b.entries.len());
                count += b.entries.len();
            },
        )
        .unwrap();
        let total = started.elapsed();
        eprintln!(
            "FER-128 100k: create {created:?}, first batch {:?}, total {total:?}, batches {batches}, max batch {max_batch}",
            first.unwrap()
        );
        assert_eq!(count, 100_000);
        assert_eq!(summary.total, 100_000);
        assert!(summary.truncated, "100 500 files must hit the 100k cap");
        assert!(max_batch <= DEFAULT_BATCH_SIZE);
        assert!(first.unwrap() < Duration::from_secs(1));
        let _ = fs::remove_dir_all(&root);
    }
}
