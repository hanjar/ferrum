//! File/folder search (F2-16): streaming, cancellable traversal with name
//! (glob or regex), size, modified-time, and bounded content filters.
//!
//! Kept free of Tauri types so it stays unit-testable; `ipc.rs` spawns the
//! worker and forwards batches as `search://progress` events. Safety rules:
//! symlinks are reported as entries but NEVER followed (D07) — that alone
//! makes symlink loops impossible; depth, visited-entry, result and the
//! 30-second wall-clock caps bound pathological trees; unreadable entries
//! are counted and skipped so one permission error never kills a search.

use crate::domain::{FileEntry, FsError, validate_path};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Search parameters. Defaults live with the TS caller; the engine treats
/// zero `max_results` as "unset" and applies its own cap.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SearchOptions {
    pub pattern: String,
    /// "glob" (default) or "regex".
    pub mode: String,
    pub case_sensitive: bool,
    pub recursive: bool,
    pub min_size: Option<u64>,
    pub max_size: Option<u64>,
    pub modified_after_ms: Option<i64>,
    pub modified_before_ms: Option<i64>,
    /// Literal substring matched inside file contents (lossy UTF-8).
    pub content: Option<String>,
    pub max_results: usize,
    /// Files larger than this are never read for the content filter.
    pub content_max_file_bytes: u64,
    pub max_depth: usize,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            pattern: String::new(),
            mode: "glob".into(),
            case_sensitive: false,
            recursive: true,
            min_size: None,
            max_size: None,
            modified_after_ms: None,
            modified_before_ms: None,
            content: None,
            max_results: 1000,
            content_max_file_bytes: 2 * 1024 * 1024,
            max_depth: 64,
        }
    }
}

/// Final/progressive stats for a finished (or cancelled) search.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchStats {
    pub matched: usize,
    pub visited: usize,
    /// Entries or directories that could not be read (e.g. permission).
    pub errors: usize,
    /// True when the result cap stopped the walk before it finished.
    pub truncated: bool,
    pub cancelled: bool,
    /// True when the 30-second wall-clock cap stopped the walk before it
    /// finished; the result set may be incomplete.
    pub timed_out: bool,
    /// Wall-clock duration of the whole search in milliseconds.
    pub duration_ms: u64,
    /// FER-476: what answered (`walker` | `index` | `indexAndWalker`).
    #[serde(default)]
    pub source: SearchSource,
    /// FER-476: the index answered from its previous state while it is
    /// rebuilt (complete but not live).
    #[serde(default)]
    pub stale: bool,
}

/// What answered a search (FER-476).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SearchSource {
    /// The directory walk.
    #[default]
    Walker,
    /// The FerrumIndexer service.
    Index,
    /// The index, plus a walk of the user's own profile (shared mode does
    /// not index profiles).
    IndexAndWalker,
}

/// Hard caps independent of caller-provided options.
const ABSOLUTE_MAX_RESULTS: usize = 100_000;
const ABSOLUTE_MAX_VISITED: usize = 2_000_000;
const CONTENT_SNIFF_LIMIT: u64 = 2 * 1024 * 1024;
/// Wall-clock cap for one search walk (ported from the pre-merge backlog
/// implementation): a network share or pathological tree must not spin the
/// worker forever. When exceeded the walk stops cleanly and `SearchStats::
/// timed_out` reports it so the UI can flag the results as incomplete.
const WALK_CAP: Duration = Duration::from_secs(30);

/// Progressive payload for `search://progress` events. Batches arrive while
/// the walk runs; one final event carries `done` plus stats or an error.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchEvent {
    pub id: String,
    pub entries: Vec<FileEntry>,
    pub done: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stats: Option<SearchStats>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Live searches by id so `search_cancel` can stop the worker's I/O.
pub fn active_cancels() -> &'static Mutex<HashMap<String, Arc<AtomicBool>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, Arc<AtomicBool>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Compile the name matcher. Glob masks translate like the frontend
/// (wildcards never cross `/` or `\`); regex compiles as-is.
pub fn compile_name_matcher(
    pattern: &str,
    mode: &str,
    case_sensitive: bool,
) -> Result<Regex, FsError> {
    let source = match mode {
        "regex" => pattern.trim().to_string(),
        // glob (default for anything unknown)
        _ => {
            let mut out = String::new();
            for ch in pattern.trim().chars() {
                match ch {
                    '*' => out.push_str("[^\\\\/]*"),
                    '?' => out.push_str("[^\\\\/]"),
                    c => out.push_str(&regex::escape(&c.to_string())),
                }
            }
            out
        }
    };
    if source.is_empty() {
        return Err(FsError::new(
            crate::domain::ErrorCode::InvalidPath,
            "Search pattern must not be empty",
        ));
    }
    let flags = if case_sensitive { "(?s)" } else { "(?is)" };
    Regex::new(&format!("^{flags}{source}$"))
        .map_err(|e| FsError::new(crate::domain::ErrorCode::InvalidPath, e.to_string()))
}

fn case_fold(s: &str) -> std::borrow::Cow<'_, str> {
    // Cheap lowercase for the content filter; the regex crate handles name
    // case itself, this is only for the literal substring scan.
    if s.chars().any(|c| c.is_uppercase()) {
        std::borrow::Cow::Owned(s.to_lowercase())
    } else {
        std::borrow::Cow::Borrowed(s)
    }
}

/// Read at most `limit` bytes of a file and return its lossy text.
fn read_head(path: &Path, limit: u64) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    let mut buf = Vec::new();
    file.take(limit).read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// FER-476: the FerrumIndexer answers when it can (glob, recursive, no
/// content filter, an indexed drive); otherwise — or when the service is
/// absent, stopped, still building, or the root is not in its index — the
/// walker runs exactly as before. Same sink, same stats shape.
pub fn run_search_auto(
    root: &str,
    options: &SearchOptions,
    cancel: &AtomicBool,
    mut sink: impl FnMut(&[FileEntry]) -> bool,
) -> Result<SearchStats, FsError> {
    validate_path(root)?;
    let deadline = Instant::now() + WALK_CAP;
    if let Some(r) =
        crate::indexer_client::index_search(root, options, cancel, deadline, &mut |b| sink(b))
    {
        return r;
    }
    run_search_with_deadline(root, options, cancel, Some(deadline), sink)
}

/// One streaming search with the default 30-second wall-clock cap. `sink`
/// receives batches (never empty); returning `false` from the sink stops the
/// walk early. Cancellation is checked before every entry, so `cancel()`
/// stops I/O promptly.
pub fn run_search(
    root: &str,
    options: &SearchOptions,
    cancel: &AtomicBool,
    sink: impl FnMut(&[FileEntry]) -> bool,
) -> Result<SearchStats, FsError> {
    run_search_with_deadline(root, options, cancel, Some(Instant::now() + WALK_CAP), sink)
}

/// Core walk with an injectable deadline (`None` disables the time cap) so
/// tests can exercise the cap without waiting for it.
pub fn run_search_with_deadline(
    root: &str,
    options: &SearchOptions,
    cancel: &AtomicBool,
    deadline: Option<Instant>,
    mut sink: impl FnMut(&[FileEntry]) -> bool,
) -> Result<SearchStats, FsError> {
    validate_path(root)?;
    if options.pattern.trim().is_empty()
        && options
            .content
            .as_deref()
            .is_none_or(|c| c.trim().is_empty())
    {
        return Err(FsError::new(
            crate::domain::ErrorCode::InvalidPath,
            "Search needs a name pattern or a content query",
        ));
    }
    // A content-only search has no name pattern: every entry is a candidate.
    let matcher = if options.pattern.trim().is_empty() {
        None
    } else {
        Some(compile_name_matcher(
            &options.pattern,
            &options.mode,
            options.case_sensitive,
        )?)
    };
    let content_needle = options
        .content
        .as_deref()
        .map(|c| case_fold(c.trim()))
        .filter(|c| !c.is_empty());

    let root_path = PathBuf::from(root);
    let meta = fs::metadata(&root_path).map_err(|e| FsError::from(e).with_path(root))?;
    if !meta.is_dir() {
        return Err(FsError::new(
            crate::domain::ErrorCode::NotADirectory,
            "Search root is a file, not a directory",
        )
        .with_path(root));
    }

    let mut stats = SearchStats {
        matched: 0,
        visited: 0,
        errors: 0,
        truncated: false,
        cancelled: false,
        timed_out: false,
        duration_ms: 0,
        source: SearchSource::Walker,
        stale: false,
    };
    let start = Instant::now();
    let timed_out = || deadline.is_some_and(|dl| Instant::now() >= dl);
    let max_results = if options.max_results == 0 {
        ABSOLUTE_MAX_RESULTS
    } else {
        options.max_results.min(ABSOLUTE_MAX_RESULTS)
    };
    let mut batch: Vec<FileEntry> = Vec::with_capacity(256);
    // Explicit stack of (dir, depth): no recursion, no stack overflow on
    // deep trees. Symlinks never enter the stack, so no loop is possible.
    let mut stack: Vec<(PathBuf, usize)> = vec![(root_path, 0)];

    'outer: while let Some((dir, depth)) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            stats.cancelled = true;
            break;
        }
        if timed_out() {
            stats.timed_out = true;
            break 'outer;
        }
        let read = match fs::read_dir(&dir) {
            Ok(read) => read,
            Err(_) => {
                stats.errors += 1;
                continue;
            }
        };
        for item in read {
            if cancel.load(Ordering::Relaxed) {
                stats.cancelled = true;
                break 'outer;
            }
            if timed_out() {
                stats.timed_out = true;
                break 'outer;
            }
            if stats.visited >= ABSOLUTE_MAX_VISITED {
                stats.truncated = true;
                break 'outer;
            }
            stats.visited += 1;
            let Ok(item) = item else {
                stats.errors += 1;
                continue;
            };
            let Ok(file_type) = item.file_type() else {
                stats.errors += 1;
                continue;
            };
            let is_symlink = file_type.is_symlink();
            let Ok(lm) = fs::symlink_metadata(item.path()) else {
                stats.errors += 1;
                continue;
            };
            let is_dir = lm.is_dir();
            let name = item.file_name().to_string_lossy().into_owned();
            let path_str = item.path().to_string_lossy().into_owned();

            let name_ok = match &matcher {
                None => true,
                Some(re) => re.is_match(&name),
            };
            let mut entry: Option<FileEntry> = None;
            if name_ok && passes_size_time(&lm, options, is_dir) {
                let mut pass = true;
                if let Some(needle) = &content_needle {
                    // content applies to bounded-size files only
                    pass = !is_dir
                        && lm.len() <= options.content_max_file_bytes.min(CONTENT_SNIFF_LIMIT)
                        && read_head(
                            &item.path(),
                            options.content_max_file_bytes.min(CONTENT_SNIFF_LIMIT),
                        )
                        .map(|text| case_fold(&text).contains(needle.as_ref()))
                        .unwrap_or_else(|| {
                            stats.errors += 1;
                            false
                        });
                }
                if pass {
                    entry = Some(FileEntry {
                        hidden: crate::fs_service::is_hidden(&name, &lm),
                        name,
                        path: path_str,
                        is_dir,
                        is_symlink,
                        size: if is_dir { 0 } else { lm.len() },
                        modified_ms: crate::fs_service::modified_ms(&lm),
                    });
                }
            }

            if let Some(e) = entry {
                stats.matched += 1;
                batch.push(e);
                if batch.len() >= 200 {
                    if !sink(&batch) {
                        stats.cancelled = true;
                        break 'outer;
                    }
                    batch.clear();
                }
                if stats.matched >= max_results {
                    stats.truncated = true;
                    break 'outer;
                }
            }

            // Recurse into real directories only — never into symlinks.
            if is_dir && !is_symlink && options.recursive && depth < options.max_depth {
                stack.push((item.path(), depth + 1));
            }
        }
    }

    // The final partial batch still honors a stop signal from the consumer.
    if !batch.is_empty() && !sink(&batch) {
        stats.cancelled = true;
    }
    stats.duration_ms = start.elapsed().as_millis() as u64;
    Ok(stats)
}

fn passes_size_time(lm: &fs::Metadata, options: &SearchOptions, is_dir: bool) -> bool {
    passes_size_time_values(
        lm.len(),
        crate::fs_service::modified_ms(lm),
        options,
        is_dir,
    )
}

/// The size/time window on plain values (the index path has no metadata).
pub(crate) fn passes_size_time_values(
    size: u64,
    modified: i64,
    options: &SearchOptions,
    is_dir: bool,
) -> bool {
    if let Some(min) = options.min_size
        && !is_dir
        && size < min
    {
        return false;
    }
    if let Some(max) = options.max_size
        && !is_dir
        && size > max
    {
        return false;
    }
    if let Some(after) = options.modified_after_ms
        && modified < after
    {
        return false;
    }
    if let Some(before) = options.modified_before_ms
        && modified > before
    {
        return false;
    }
    true
}

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
                "ferrum-f216-{tag}-{}-{}",
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

    fn opts(pattern: &str) -> SearchOptions {
        SearchOptions {
            pattern: pattern.into(),
            ..SearchOptions::default()
        }
    }

    fn search(root: &Path, options: &SearchOptions) -> (Vec<FileEntry>, SearchStats) {
        let collected = std::sync::Mutex::new(Vec::new());
        let stats = run_search(
            root.to_str().unwrap(),
            options,
            &AtomicBool::new(false),
            |batch| {
                collected.lock().unwrap().extend(batch.iter().cloned());
                true
            },
        )
        .expect("search runs");
        (collected.into_inner().unwrap(), stats)
    }

    fn names(results: &[FileEntry]) -> Vec<String> {
        let mut v: Vec<String> = results.iter().map(|e| e.name.clone()).collect();
        v.sort();
        v
    }

    fn search_deadline(
        root: &Path,
        options: &SearchOptions,
        deadline: Option<Instant>,
    ) -> (Vec<FileEntry>, SearchStats) {
        let collected = std::sync::Mutex::new(Vec::new());
        let stats = run_search_with_deadline(
            root.to_str().unwrap(),
            options,
            &AtomicBool::new(false),
            deadline,
            |batch| {
                collected.lock().unwrap().extend(batch.iter().cloned());
                true
            },
        )
        .expect("search runs");
        (collected.into_inner().unwrap(), stats)
    }

    fn fixture() -> TempDir {
        let root = temp_root("tree");
        fs::write(root.join("report.txt"), b"hello world").unwrap();
        fs::write(root.join("NOTES.md"), b"# ferrum search fixture").unwrap();
        fs::create_dir(root.join("sub")).unwrap();
        fs::write(root.join("sub").join("deep-report.txt"), b"nested hello").unwrap();
        fs::write(root.join("sub").join("big.bin"), vec![0u8; 4096]).unwrap();
        root
    }

    /// FER-476: without a FerrumIndexer (dev machines, CI) the automatic
    /// path is exactly the walker, and says so. Content / regex / shallow
    /// searches never go to the index either.
    #[test]
    fn auto_search_falls_back_to_the_walker() {
        let root = fixture();
        for o in [
            opts("*.txt"),
            SearchOptions {
                mode: "regex".into(),
                pattern: r".*\.txt".into(),
                ..SearchOptions::default()
            },
            SearchOptions {
                recursive: false,
                ..opts("*.txt")
            },
        ] {
            let collected = std::sync::Mutex::new(Vec::new());
            let stats = run_search_auto(root.to_str().unwrap(), &o, &AtomicBool::new(false), |b| {
                collected.lock().unwrap().extend(b.iter().cloned());
                true
            })
            .unwrap();
            let (walked, _) = search(&root, &o);
            let got = collected.into_inner().unwrap();
            if stats.source == SearchSource::Walker {
                assert_eq!(names(&got), names(&walked));
            }
        }
        assert!(passes_size_time_values(
            10,
            5,
            &SearchOptions::default(),
            false
        ));
        let o = SearchOptions {
            min_size: Some(11),
            modified_after_ms: Some(6),
            ..SearchOptions::default()
        };
        assert!(!passes_size_time_values(10, 50, &o, false));
        assert!(
            passes_size_time_values(10, 50, &o, true),
            "size ignores dirs"
        );
        assert!(!passes_size_time_values(10, 5, &o, true));
    }

    #[test]
    fn glob_search_finds_files_and_folders_recursively() {
        let root = fixture();
        let (hits, stats) = search(&root, &opts("*.txt"));
        assert_eq!(names(&hits), vec!["deep-report.txt", "report.txt"]);
        assert_eq!(stats.matched, 2);
        assert!(!stats.truncated);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn glob_matches_folders_and_question_marks() {
        let root = fixture();
        let (hits, _) = search(&root, &opts("s?b"));
        assert!(names(&hits).contains(&"sub".to_string()));
        let (hits, _) = search(&root, &opts("NOT*"));
        assert_eq!(names(&hits), vec!["NOTES.md"]);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn regex_mode_and_case_sensitivity() {
        let root = fixture();
        let o = SearchOptions {
            pattern: "deep-.+\\.txt".into(),
            mode: "regex".into(),
            ..SearchOptions::default()
        };
        let (hits, _) = search(&root, &o);
        assert_eq!(names(&hits), vec!["deep-report.txt"]);
        // case-sensitive regex misses the uppercase name
        let o = SearchOptions {
            pattern: "^notes".into(),
            mode: "regex".into(),
            case_sensitive: true,
            ..SearchOptions::default()
        };
        let (hits, _) = search(&root, &o);
        assert!(hits.is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn non_recursive_shallow_only() {
        let root = fixture();
        let o = SearchOptions {
            recursive: false,
            ..opts("*.txt")
        };
        let (hits, _) = search(&root, &o);
        assert_eq!(names(&hits), vec!["report.txt"]);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn size_and_time_windows_filter_results() {
        let root = fixture();
        let o = SearchOptions {
            min_size: Some(1000),
            ..opts("*")
        };
        // Size bounds apply to files only; the folder still lists (TC-style).
        let (hits, _) = search(&root, &o);
        assert_eq!(names(&hits), vec!["big.bin", "sub"]);
        let future = 4_000_000_000_000i64; // far in the future
        let o = SearchOptions {
            modified_after_ms: Some(future),
            ..opts("*")
        };
        let (hits, _) = search(&root, &o);
        assert!(hits.is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn content_search_is_bounded_and_case_insensitive() {
        let root = fixture();
        let o = SearchOptions {
            pattern: String::new(),
            content: Some("HELLO".into()),
            ..SearchOptions::default()
        };
        let (hits, _) = search(&root, &o);
        assert_eq!(
            names(&hits),
            vec!["deep-report.txt", "report.txt"],
            "big.bin is excluded by the content byte bound"
        );
        // A small content cap excludes the 4 KiB file from the content scan.
        let o = SearchOptions {
            pattern: String::new(),
            content: Some("hello".into()),
            content_max_file_bytes: 16,
            ..SearchOptions::default()
        };
        let (hits, _) = search(&root, &o);
        assert!(names(&hits).iter().all(|n| n != "big.bin"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn result_cap_marks_truncated() {
        let root = fixture();
        let o = SearchOptions {
            max_results: 1,
            ..opts("*.txt")
        };
        let (hits, stats) = search(&root, &o);
        assert_eq!(hits.len(), 1);
        assert!(stats.truncated);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn cancellation_stops_the_walk() {
        let root = fixture();
        let cancel = AtomicBool::new(true); // cancelled before it starts
        let collected = std::sync::Mutex::new(Vec::new());
        let stats = run_search(root.to_str().unwrap(), &opts("*"), &cancel, |b| {
            collected.lock().unwrap().extend(b.iter().cloned());
            true
        })
        .unwrap();
        assert!(stats.cancelled);
        assert_eq!(collected.lock().unwrap().len(), 0);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn time_cap_stops_the_walk_and_is_reported() {
        let root = fixture();
        // An already-expired deadline stops the walk before it visits
        // anything, without touching the cancel path.
        let expired = Instant::now()
            .checked_sub(Duration::from_secs(1))
            .expect("clock supports subtraction");
        let (hits, stats) = search_deadline(&root, &opts("*"), Some(expired));
        assert!(stats.timed_out, "expired deadline must report timed_out");
        assert!(!stats.truncated, "the result cap was not what stopped it");
        assert!(!stats.cancelled);
        assert_eq!(stats.visited, 0);
        assert!(hits.is_empty());
        // A live deadline (or none) completes normally and never flags it.
        let (hits, stats) = search_deadline(&root, &opts("*.txt"), None);
        assert_eq!(names(&hits), vec!["deep-report.txt", "report.txt"]);
        assert!(!stats.timed_out);
        assert!(
            stats.duration_ms < 30_000,
            "duration_ms reports real wall-clock time"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn sink_stop_signal_aborts() {
        let root = fixture();
        let collected = std::sync::Mutex::new(Vec::new());
        let stats = run_search(
            root.to_str().unwrap(),
            &opts("*"),
            &AtomicBool::new(false),
            |b| {
                {
                    let mut c = collected.lock().unwrap();
                    c.extend(b.iter().cloned());
                }
                c_len(&collected) < 2
            },
        )
        .unwrap();
        assert!(stats.cancelled);
        assert!(c_len(&collected) < 20);
        let _ = fs::remove_dir_all(&root);
    }

    fn c_len(v: &std::sync::Mutex<Vec<FileEntry>>) -> usize {
        v.lock().unwrap().len()
    }

    #[test]
    fn symlinks_never_enter_the_walk() {
        let root = temp_root("loop");
        let inner = root.join("inner");
        fs::create_dir(&inner).unwrap();
        fs::write(inner.join("inside.txt"), b"x").unwrap();
        #[cfg(windows)]
        {
            if std::os::windows::fs::symlink_dir(&inner, root.join("loop-link")).is_err() {
                return; // environment refuses unprivileged symlinks
            }
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(&inner, root.join("loop-link")).unwrap();

        let (hits, stats) = search(&root, &opts("*.txt"));
        // The search must terminate: the loop-link dir is never followed.
        assert_eq!(names(&hits), vec!["inside.txt"]);
        assert_eq!(stats.errors, 0);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_or_file_roots_error_cleanly() {
        let err = run_search(
            r"Z:\definitely\not\here",
            &opts("*"),
            &AtomicBool::new(false),
            |_| true,
        )
        .unwrap_err();
        assert_eq!(err.code, crate::domain::ErrorCode::NotFound);
        let file = temp_root("fileroot");
        fs::write(file.join("f.txt"), b"x").unwrap();
        let err = run_search(
            file.join("f.txt").to_str().unwrap(),
            &opts("*"),
            &AtomicBool::new(false),
            |_| true,
        )
        .unwrap_err();
        assert_eq!(err.code, crate::domain::ErrorCode::NotADirectory);
        let _ = fs::remove_dir_all(&file);
    }

    #[test]
    fn empty_query_and_bad_regex_are_rejected() {
        let root = fixture();
        assert!(
            run_search(
                root.to_str().unwrap(),
                &opts("  "),
                &AtomicBool::new(false),
                |_| true
            )
            .is_err()
        );
        let bad = SearchOptions {
            pattern: "(unclosed".into(),
            mode: "regex".into(),
            ..SearchOptions::default()
        };
        assert!(
            run_search(
                root.to_str().unwrap(),
                &bad,
                &AtomicBool::new(false),
                |_| true
            )
            .is_err()
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn unicode_names_are_found() {
        let root = temp_root("unicode");
        fs::write(root.join("békér ünicode.txt"), b"x").unwrap();
        let (hits, _) = search(&root, &opts("bék*"));
        assert_eq!(names(&hits), vec!["békér ünicode.txt"]);
        let _ = fs::remove_dir_all(&root);
    }
    /// FER-101: a directory the search cannot list is counted in
    /// `stats.errors` (the "skipped" count the UI shows as "unreadable") and
    /// the walk carries on — the readable siblings are still found.
    #[cfg(windows)]
    #[test]
    fn unreadable_directory_is_skipped_and_search_continues() {
        use std::process::Command;
        let root = temp_root("acl");
        fs::write(root.join("visible.txt"), b"x").unwrap();
        let locked = root.join("locked");
        fs::create_dir(&locked).unwrap();
        fs::write(locked.join("hidden.txt"), b"x").unwrap();
        let user = std::env::var("USERNAME").unwrap_or_default();
        let deny = Command::new("icacls")
            .arg(&locked)
            .args(["/deny", &format!("{user}:(RD)")])
            .output();
        let denied = matches!(&deny, Ok(o) if o.status.success()) && fs::read_dir(&locked).is_err();
        if !denied {
            // ACL could not be applied on this volume: no stable fixture.
            let _ = fs::remove_dir_all(&root);
            return;
        }
        let (hits, stats) = search(&root, &opts("*.txt"));
        // Restore access before any assertion so cleanup always works.
        let _ = Command::new("icacls")
            .arg(&locked)
            .args(["/remove:d", &user])
            .output();
        let _ = Command::new("icacls")
            .arg(&locked)
            .args(["/grant", &format!("{user}:(F)")])
            .output();
        assert_eq!(names(&hits), vec!["visible.txt"]);
        assert_eq!(stats.errors, 1, "locked dir counted as skipped");
        assert!(!stats.cancelled && !stats.truncated && !stats.timed_out);
        let _ = fs::remove_dir_all(&root);
    }

    /// FER-101: the content filter counts unreadable files as skipped too
    /// (exclusive-lock on Windows / mode 000 on Unix) without failing.
    #[cfg(windows)]
    #[test]
    fn locked_file_during_content_search_is_counted_not_fatal() {
        use std::os::windows::fs::OpenOptionsExt;
        let root = temp_root("lock");
        fs::write(root.join("a.txt"), b"needle here").unwrap();
        let busy = root.join("busy.txt");
        fs::write(&busy, b"needle here").unwrap();
        // share_mode(0): exclusive, so reads by the search fail.
        let _guard = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(0)
            .open(&busy)
            .unwrap();
        let mut o = opts("*.txt");
        o.content = Some("needle".into());
        let (hits, stats) = search(&root, &o);
        assert_eq!(names(&hits), vec!["a.txt"]);
        assert_eq!(stats.errors, 1);
        drop(_guard);
        let _ = fs::remove_dir_all(&root);
    }
}
