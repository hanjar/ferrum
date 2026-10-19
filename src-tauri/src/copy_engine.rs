//! Copy engine (task 11): streaming file copy, recursive directories,
//! conflict policies (D06/D21), symlink policy (D07), cancellation with
//! partial-output cleanup. UI-free and queue-agnostic — the queue calls
//! `run_copy` inside a job body and forwards progress.

use crate::copy_native::{self, Media, NativeCopyError};
use crate::copy_pool::{
    self, AIMD_WINDOW, Aimd, AimdInput, CopyControl, Priority, SMALL_FILE_LIMIT, Sample,
    ThreadSetting, UNBUFFERED_LIMIT,
};
use crate::domain::{ErrorCode, FsError, OperationProgress};
use crate::ops::CancelToken;
use std::cell::Cell;
use std::collections::{HashSet, VecDeque};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

pub const CHUNK: usize = 1024 * 1024;
pub const MIN_COPY_BUFFER: usize = 64 * 1024;
pub const MAX_COPY_BUFFER: usize = 8 * 1024 * 1024;
const STAGING_NAME_ATTEMPTS: usize = 100;

/// Internal typed controls for copy execution; IPC can map preferences here later.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopyOptions {
    pub buffer_size: usize,
    pub verify_after_copy: bool,
    pub preserve_timestamps: bool,
    pub preserve_permissions: bool,
    /// FER-471: worker count at job start (`Auto` = adaptive).
    pub threads: ThreadSetting,
    /// FER-471: OS priority class of the copy workers at job start.
    pub priority: Priority,
    /// Force the portable buffered copy instead of `CopyFile2` (non-Windows,
    /// tests and the benchmark baseline).
    pub stream_only: bool,
    /// Flush every staged file to disk (FlushFileBuffers) BEFORE it is committed. Move always sets
    /// this (the source is deleted afterwards, so the destination must be on disk first); a plain
    /// copy leaves it off unless Settings "Tulis aman (flush) untuk copy" is on.
    pub durable: bool,
}

impl Default for CopyOptions {
    fn default() -> Self {
        Self {
            buffer_size: CHUNK,
            verify_after_copy: false,
            preserve_timestamps: true,
            preserve_permissions: false,
            threads: ThreadSetting::Auto,
            priority: Priority::Normal,
            stream_only: false,
            durable: false,
        }
    }
}

impl CopyOptions {
    pub fn bounded_buffer_size(self) -> usize {
        self.buffer_size.clamp(MIN_COPY_BUFFER, MAX_COPY_BUFFER)
    }
}

static STAGING_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn create_staging_file(dest: &Path) -> Result<(PathBuf, fs::File), FsError> {
    let name = dest.file_name().unwrap_or_default().to_string_lossy();
    for _ in 0..STAGING_NAME_ATTEMPTS {
        let n = STAGING_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = dest.with_file_name(format!(".{name}.ferrum-stage-{}-{n}", std::process::id()));
        match fs::File::options().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(FsError::from(error).with_path(path.to_string_lossy())),
        }
    }
    Err(err(
        ErrorCode::AlreadyExists,
        "Unable to allocate unique staging file",
    )
    .with_path(dest.to_string_lossy()))
}

pub(crate) fn create_backup_path(dest: &Path) -> Result<PathBuf, FsError> {
    let name = dest.file_name().unwrap_or_default().to_string_lossy();
    for _ in 0..STAGING_NAME_ATTEMPTS {
        let n = STAGING_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = dest.with_file_name(format!(".{name}.ferrum-backup-{}-{n}", std::process::id()));
        match fs::File::options().write(true).create_new(true).open(&path) {
            Ok(file) => {
                drop(file);
                fs::remove_file(&path)
                    .map_err(|e| FsError::from(e).with_path(path.to_string_lossy()))?;
                return Ok(path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(FsError::from(error).with_path(path.to_string_lossy())),
        }
    }
    Err(err(
        ErrorCode::AlreadyExists,
        "Unable to allocate unique backup path",
    )
    .with_path(dest.to_string_lossy()))
}

fn set_copy_metadata(src: &Path, staged: &Path, options: CopyOptions) -> Result<(), FsError> {
    #[cfg(windows)]
    if options.preserve_permissions {
        return Err(err(
            ErrorCode::Unsupported,
            "Permission preservation is unsupported on Windows by this copy engine",
        )
        .with_path(src.to_string_lossy()));
    }
    #[cfg(not(any(unix, windows)))]
    if options.preserve_permissions {
        return Err(err(
            ErrorCode::Unsupported,
            "Permission preservation is unsupported on this platform",
        )
        .with_path(src.to_string_lossy()));
    }
    #[cfg(unix)]
    if options.preserve_permissions {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(src)
            .map_err(|e| FsError::from(e).with_path(src.to_string_lossy()))?
            .permissions()
            .mode();
        fs::set_permissions(staged, fs::Permissions::from_mode(mode))
            .map_err(|e| FsError::from(e).with_path(staged.to_string_lossy()))?;
    }
    if options.preserve_timestamps {
        let modified = fs::metadata(src)
            .map_err(|e| FsError::from(e).with_path(src.to_string_lossy()))?
            .modified()
            .map_err(|e| FsError::from(e).with_path(src.to_string_lossy()))?;
        let file = fs::File::options()
            .write(true)
            .open(staged)
            .map_err(|e| FsError::from(e).with_path(staged.to_string_lossy()))?;
        file.set_times(fs::FileTimes::new().set_modified(modified))
            .map_err(|e| FsError::from(e).with_path(staged.to_string_lossy()))?;
    }
    Ok(())
}

fn files_match(
    source: &Path,
    destination: &Path,
    buffer_size: usize,
    cancel: &CancelToken,
) -> Result<bool, FsError> {
    let mut source = fs::File::open(source).map_err(FsError::from)?;
    let mut destination = fs::File::open(destination).map_err(FsError::from)?;
    let mut left = vec![0; buffer_size];
    let mut right = vec![0; buffer_size];
    loop {
        if cancel.is_cancelled() {
            return Err(err(ErrorCode::Cancelled, "cancelled"));
        }
        let left_n = source.read(&mut left).map_err(FsError::from)?;
        let right_n = destination.read(&mut right).map_err(FsError::from)?;
        if left_n != right_n || left[..left_n] != right[..right_n] {
            return Ok(false);
        }
        if left_n == 0 {
            return Ok(true);
        }
    }
}

/// FER-564: a caller-owned way to replace an EXISTING destination at commit time. With a guard,
/// an overwrite does not use the built-in backup dance: the guard is handed the staged file and
/// the destination and must either publish the staged file over it or fail leaving the
/// destination as it was. The sync engine uses it to re-check the destination's facts on a held
/// handle right before the swap, so a file edited during a long copy is never overwritten.
pub trait ReplaceGuard: Send + Sync {
    fn replace(&self, staged: &Path, dest: &Path) -> Result<(), FsError>;
}

/// Publish a staged file. `replace=false` (new name or auto-renamed target) is a rename that
/// refuses to overwrite: an unexpected collision becomes a per-file conflict error, never a silent
/// overwrite. `replace=true` (Overwrite policies only) keeps the old backup/rollback dance.
fn commit_staged(
    staged: &Path,
    dest: &Path,
    replace: bool,
    guard: Option<&dyn ReplaceGuard>,
) -> Result<(), FsError> {
    #[cfg(test)]
    note_event(dest, "commit");
    if replace && let Some(guard) = guard {
        return guard.replace(staged, dest);
    }
    if !replace {
        return copy_native::rename_no_replace(staged, dest).map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                err(
                    ErrorCode::AlreadyExists,
                    "Destination appeared during the copy (name conflict); nothing was overwritten",
                )
                .with_path(dest.to_string_lossy())
            } else {
                FsError::from(e).with_path(dest.to_string_lossy())
            }
        });
    }
    let backup = if dest.exists() {
        let backup = create_backup_path(dest)?;
        fs::rename(dest, &backup)
            .map_err(|e| FsError::from(e).with_path(dest.to_string_lossy()))?;
        Some(backup)
    } else {
        None
    };
    if let Err(error) = fs::rename(staged, dest) {
        let fe = FsError::from(error).with_path(dest.to_string_lossy());
        if let Some(backup) = backup {
            let _ = fs::rename(backup, dest);
        }
        return Err(fe);
    }
    if let Some(backup) = backup {
        // Destination is committed and verified; stale backup cleanup must not
        // turn a valid final result into a reported copy failure.
        let _ = fs::remove_file(backup);
    }
    Ok(())
}

fn copy_failure(out: &mut CopyOutcome, source: &Path, error: FsError) {
    out.failures.push(FailedCopy {
        source: source.to_string_lossy().into_owned(),
        code: error.code,
        message: error.message,
    });
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictPolicy {
    /// Report conflicts without touching anything (planning / ask-first UI).
    Ask,
    Skip,
    Overwrite,
    /// Overwrite only when the source is newer than the destination.
    OverwriteOlder,
    /// Keep both: destination gets "name (2).ext".
    RenameAuto,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Conflict {
    pub source: String,
    pub dest: String,
    pub source_modified_ms: i64,
    pub dest_modified_ms: i64,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CopyPlan {
    pub total_files: u64,
    pub total_bytes: u64,
    pub conflicts: Vec<Conflict>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CopyOutcome {
    pub copied_files: u64,
    pub copied_bytes: u64,
    pub skipped: u64,
    pub renamed: u64,
    pub overwritten: u64,
    pub failures: Vec<FailedCopy>,
    pub cancelled: bool,
    /// Per-item (source, destination) pairs that fully succeeded; feeds the
    /// activity identity hook. Never crosses IPC.
    #[serde(default, skip_serializing)]
    pub completed: Vec<(String, String)>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FailedCopy {
    pub source: String,
    pub code: ErrorCode,
    pub message: String,
}

fn err(code: ErrorCode, msg: impl Into<String>) -> FsError {
    FsError::new(code, msg)
}

fn modified_ms(p: &Path) -> i64 {
    fs::symlink_metadata(p)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Plain and `\?\` spellings, case and `/` do not matter (FER-559: a verbatim target next to a
/// plain source must still be recognised as "into itself").
fn is_within(child: &Path, ancestor: &Path) -> bool {
    crate::path_guard::key_is_within(
        &crate::path_guard::path_key(child),
        &crate::path_guard::path_key(ancestor),
    )
}

const STARTUP_MSG: &str = "Ferrum does not write into the Windows Startup folder.";
const ITSELF_MSG: &str = "Cannot copy a folder into itself";

const RESERVED_NAME_MSG: &str = "Name is a reserved Windows device name (nul, con, com1 ...): rename it first, a copy would address the device";

use crate::path_guard::is_reserved_device_name;

/// Plan a copy: totals + conflicts. Performs the D06 "ask first" scan.
pub fn plan_copy(sources: &[String], target_dir: &str) -> Result<CopyPlan, FsError> {
    let target = Path::new(target_dir);
    if !target.is_dir() {
        return Err(
            err(ErrorCode::NotADirectory, "Target is not a directory").with_path(target_dir)
        );
    }
    let mut plan = CopyPlan::default();
    for src in sources {
        let s = PathBuf::from(src);
        if !s.exists() {
            return Err(err(ErrorCode::NotFound, "Source not found").with_path(src));
        }
        // Rejections that make no sense to ask about (task 11 tests).
        // FER-559: plain/verbatim spellings, case and `/` (path keys); FER-563: also aliases
        // (8.3 names, ...) of the source folder, by file identity.
        if is_within(target, &s)
            || (s.is_dir() && crate::file_identity::is_same_or_inside(target, &s))
        {
            return Err(
                err(ErrorCode::InvalidPath, "Cannot copy a folder into itself").with_path(src),
            );
        }
        let dest = target.join(s.file_name().unwrap_or_default());
        if dest.exists() {
            plan.conflicts.push(Conflict {
                source: src.clone(),
                dest: dest.to_string_lossy().into_owned(),
                source_modified_ms: modified_ms(&s),
                dest_modified_ms: modified_ms(&dest),
            });
        }
        accumulate(&s, &mut plan)?;
    }
    Ok(plan)
}

fn accumulate(p: &Path, plan: &mut CopyPlan) -> Result<(), FsError> {
    let meta =
        fs::symlink_metadata(p).map_err(|e| FsError::from(e).with_path(p.to_string_lossy()))?;
    if meta.is_symlink() {
        return Ok(()); // links count as one file-sized entry for planning
    }
    if meta.is_dir() {
        for item in fs::read_dir(p).map_err(|e| FsError::from(e).with_path(p.to_string_lossy()))? {
            let item = item.map_err(FsError::from)?;
            let child = item.path();
            // A reserved device name in the plain spelling is never opened (it fails at run time).
            if is_reserved_device_name(&item.file_name().to_string_lossy())
                && !crate::path_guard::is_verbatim_spelling(p)
            {
                continue;
            }
            accumulate(&child, plan)?;
        }
    } else {
        plan.total_files += 1;
        plan.total_bytes += meta.len();
    }
    Ok(())
}

/// Resolve `name.ext` against a policy when the destination exists.
/// Returns (final_path, action) where action describes what happened.
enum Resolved {
    Fresh(PathBuf), // no collision
    Skip,
    Overwrite(PathBuf),
    Rename(PathBuf),
    /// Ask policy: surfaced by `plan_copy`, not copied at run time.
    Conflict,
}

/// NTFS compares names case-insensitively: the claim key is the upper-cased path.
fn claim_key(path: &Path) -> String {
    copy_native::upper_invariant(&path.to_string_lossy().replace('/', "\\"))
}

/// `planned` holds the claim keys of every destination this job already assigned but may not have
/// written yet. A planned name counts as existing, for the base name AND every numbered candidate.
fn resolve_dest(
    dest: PathBuf,
    src_modified: i64,
    policy: ConflictPolicy,
    planned: &HashSet<String>,
) -> Resolved {
    let taken = |p: &Path| p.exists() || planned.contains(&claim_key(p));
    if !taken(&dest) {
        return Resolved::Fresh(dest);
    }
    let dest_modified = modified_ms(&dest);
    match policy {
        ConflictPolicy::Skip => Resolved::Skip,
        ConflictPolicy::Overwrite => Resolved::Overwrite(dest),
        ConflictPolicy::OverwriteOlder => {
            if src_modified > dest_modified {
                Resolved::Overwrite(dest)
            } else {
                Resolved::Skip
            }
        }
        ConflictPolicy::RenameAuto => {
            let mut candidate = numbered(&dest, 2);
            let mut n = 2;
            while taken(&candidate) {
                n += 1;
                candidate = numbered(&dest, n);
            }
            Resolved::Rename(candidate)
        }
        ConflictPolicy::Ask => Resolved::Conflict,
    }
}

fn numbered(dest: &Path, n: u32) -> PathBuf {
    let stem = dest.file_stem().and_then(|s| s.to_str()).unwrap_or("file");
    let ext = dest.extension().and_then(|s| s.to_str());
    let name = match ext {
        Some(e) => format!("{stem} ({n}).{e}"),
        None => format!("{stem} ({n})"),
    };
    dest.with_file_name(name)
}

/// Run the copy. Per-item failures never abort the batch; cancellation
/// removes the partial file being written and returns early.
pub fn run_copy(
    sources: &[String],
    target_dir: &str,
    policy: ConflictPolicy,
    cancel: &CancelToken,
    report: &dyn Fn(OperationProgress),
) -> CopyOutcome {
    let (threads, priority) = copy_pool::defaults();
    run_copy_with_options(
        sources,
        target_dir,
        policy,
        CopyOptions {
            threads,
            priority,
            ..CopyOptions::default()
        },
        cancel,
        report,
    )
}

/// Same as `run_copy_with_options`, with the live control supplied by the
/// caller (tests, benchmarks). The queue path derives it from the job id.
pub fn run_copy_with_options(
    sources: &[String],
    target_dir: &str,
    policy: ConflictPolicy,
    options: CopyOptions,
    cancel: &CancelToken,
    report: &dyn Fn(OperationProgress),
) -> CopyOutcome {
    let control = match cancel.operation_id() {
        Some(id) => copy_pool::control_for(id, options.threads, options.priority),
        None => CopyControl::new(None, options.threads, options.priority),
    };
    run_copy_controlled(
        sources, target_dir, policy, options, &control, cancel, report,
    )
}

/// Queue depth the walker may run ahead of the workers (bounds memory on
/// folders with millions of files).
const QUEUE_CAP: usize = 4096;
const TICK: Duration = Duration::from_millis(50);
const EMIT_EVERY: Duration = Duration::from_millis(100);
const STATE_EVERY: Duration = Duration::from_millis(500);

/// One top-level source of the job; tracks whether all of its files landed.
struct Group {
    src: String,
    dest: String,
    pending: AtomicUsize,
    failed: AtomicBool,
    walked: AtomicBool,
    /// Overwrite policies may replace an existing destination; every other resolution never does.
    replace: bool,
}

struct Task {
    src: PathBuf,
    dest: PathBuf,
    size: u64,
    group: Arc<Group>,
    /// Claim key held in `Shared::inflight` while this (replacing) task is queued or running.
    claim: Option<String>,
}

#[derive(Default)]
struct TaskQueue {
    small: VecDeque<Task>,
    large: VecDeque<Task>,
    large_running: usize,
    /// Workers currently holding a task.
    busy: usize,
    walker_done: bool,
}

impl TaskQueue {
    fn len(&self) -> usize {
        self.small.len() + self.large.len()
    }

    /// Tasks a free worker could start right now (large files wait for a large slot).
    fn takeable(&self, large_cap: usize) -> usize {
        self.small.len()
            + self
                .large
                .len()
                .min(large_cap.saturating_sub(self.large_running))
    }
}

struct Shared<'a> {
    cancel: &'a CancelToken,
    options: CopyOptions,
    /// FER-564: replaces the built-in overwrite commit (None for every other caller).
    replace_guard: Option<&'a dyn ReplaceGuard>,
    control: &'a CopyControl,
    large_cap: usize,
    queue: Mutex<TaskQueue>,
    cv: Condvar,
    out: Mutex<CopyOutcome>,
    done_files: AtomicU64,
    done_bytes: AtomicU64,
    current: Mutex<String>,
    aborted: AtomicBool,
    groups: Mutex<Vec<Arc<Group>>>,
    /// Destinations of replacing tasks that are queued or running: two sources that overwrite the
    /// same path run strictly one after the other, in source order (last source wins, as before).
    inflight: Mutex<HashSet<String>>,
    inflight_cv: Condvar,
}

impl Shared<'_> {
    fn fail(&self, group: &Group, source: &Path, error: FsError) {
        group.failed.store(true, Ordering::SeqCst);
        copy_failure(&mut self.out.lock().unwrap(), source, error);
    }

    fn abort(&self) {
        self.aborted.store(true, Ordering::SeqCst);
        self.cv.notify_all();
    }
}

thread_local! {
    static BACKGROUND_ON: Cell<bool> = const { Cell::new(false) };
}

/// Keep this thread's OS priority in step with the live setting.
fn apply_priority(background: bool) {
    BACKGROUND_ON.with(|on| {
        if on.get() != background && copy_native::set_thread_background(background) {
            on.set(background);
        }
    });
}

#[cfg(test)]
type ChunkHook = (PathBuf, Arc<dyn Fn(u64) + Send + Sync>);
#[cfg(test)]
static CHUNK_HOOKS: Mutex<Vec<ChunkHook>> = Mutex::new(Vec::new());
#[cfg(test)]
type StagedHook = (PathBuf, Arc<dyn Fn(&Path) + Send + Sync>);
/// Called with the staged path after the bytes are copied and before verification/commit.
#[cfg(test)]
static AFTER_COPY_HOOKS: Mutex<Vec<StagedHook>> = Mutex::new(Vec::new());
#[cfg(test)]
static EVENTS: Mutex<Vec<(PathBuf, &'static str)>> = Mutex::new(Vec::new());

/// Test-only: record that `what` ("flush" | "commit") happened for destination `dest`.
#[cfg(test)]
fn note_event(dest: &Path, what: &'static str) {
    EVENTS.lock().unwrap().push((dest.to_path_buf(), what));
}

/// Test-only: events recorded for destinations under `dir`, in order.
#[cfg(test)]
pub(crate) fn events_under(dir: &Path) -> Vec<(PathBuf, &'static str)> {
    EVENTS
        .lock()
        .unwrap()
        .iter()
        .filter(|(p, _)| p.starts_with(dir))
        .cloned()
        .collect()
}

/// Calls `hook(chunk_len)` for every chunk written under `dir` (test-only seam: lets a test act
/// in the middle of a file deterministically, without a big file and a racing watcher thread).
#[cfg(test)]
pub(crate) fn hook_chunks(dir: &Path, hook: impl Fn(u64) + Send + Sync + 'static) {
    CHUNK_HOOKS
        .lock()
        .unwrap()
        .push((dir.to_path_buf(), Arc::new(hook)));
}

#[cfg(test)]
pub(crate) fn unhook_chunks(dir: &Path) {
    CHUNK_HOOKS.lock().unwrap().retain(|(d, _)| d != dir);
}

/// Test-only: runs for every chunk of BOTH copy paths (CopyFile2 callback and stream loop).
#[cfg(test)]
fn call_chunk_hooks(staged: &Path, n: u64) {
    let hooks: Vec<ChunkHook> = CHUNK_HOOKS.lock().unwrap().clone();
    for (prefix, hook) in hooks {
        if staged.starts_with(&prefix) {
            hook(n);
        }
    }
}

#[cfg(test)]
fn call_after_copy_hooks(staged: &Path) {
    let hooks: Vec<StagedHook> = AFTER_COPY_HOOKS.lock().unwrap().clone();
    for (prefix, hook) in hooks {
        if staged.starts_with(&prefix) {
            hook(staged);
        }
    }
}

pub fn run_copy_controlled(
    sources: &[String],
    target_dir: &str,
    policy: ConflictPolicy,
    options: CopyOptions,
    control: &Arc<CopyControl>,
    cancel: &CancelToken,
    report: &dyn Fn(OperationProgress),
) -> CopyOutcome {
    run_copy_guarded(
        sources, target_dir, policy, options, control, cancel, report, None,
    )
}

/// FER-564: [`run_copy_with_options`] whose overwrites commit through `guard`.
pub fn run_copy_with_guard(
    sources: &[String],
    target_dir: &str,
    policy: ConflictPolicy,
    options: CopyOptions,
    cancel: &CancelToken,
    guard: &dyn ReplaceGuard,
) -> CopyOutcome {
    let control = match cancel.operation_id() {
        Some(id) => copy_pool::control_for(id, options.threads, options.priority),
        None => CopyControl::new(None, options.threads, options.priority),
    };
    run_copy_guarded(
        sources,
        target_dir,
        policy,
        options,
        &control,
        cancel,
        &|_progress| {},
        Some(guard),
    )
}

#[allow(clippy::too_many_arguments)]
fn run_copy_guarded(
    sources: &[String],
    target_dir: &str,
    policy: ConflictPolicy,
    options: CopyOptions,
    control: &Arc<CopyControl>,
    cancel: &CancelToken,
    report: &dyn Fn(OperationProgress),
    replace_guard: Option<&dyn ReplaceGuard>,
) -> CopyOutcome {
    let options = CopyOptions {
        buffer_size: options.bounded_buffer_size(),
        ..options
    };
    let target = PathBuf::from(target_dir);
    let (total_files, total_bytes) = match plan_copy(sources, target_dir) {
        Ok(p) => (p.total_files, p.total_bytes),
        Err(e) => {
            let mut out = CopyOutcome::default();
            out.failures.push(FailedCopy {
                source: target_dir.to_string(),
                code: e.code,
                message: e.message,
            });
            return out;
        }
    };

    // Media of the slower endpoint decides the starting point and the auto ceiling.
    let media = sources
        .first()
        .map(|s| copy_native::media_of(Path::new(s)))
        .unwrap_or(Media::Unknown)
        .slower(copy_native::media_of(&target));
    let same_volume = sources
        .first()
        .is_some_and(|s| same_volume(Path::new(s), &target));
    let (start, ceiling) = copy_pool::media_profile(media, same_volume);
    control.begin_run(media, start, ceiling);
    let large_cap = if media == Media::Hdd { 1 } else { 2 };

    let sh = Shared {
        cancel,
        options,
        replace_guard,
        control,
        large_cap,
        queue: Mutex::new(TaskQueue::default()),
        cv: Condvar::new(),
        out: Mutex::new(CopyOutcome::default()),
        done_files: AtomicU64::new(0),
        done_bytes: AtomicU64::new(0),
        current: Mutex::new(String::new()),
        aborted: AtomicBool::new(false),
        groups: Mutex::new(Vec::new()),
        inflight: Mutex::new(HashSet::new()),
        inflight_cv: Condvar::new(),
    };

    let emit = |sh: &Shared| {
        report(OperationProgress {
            operation_id: String::new(),
            kind: crate::domain::OperationKind::Copy,
            state: crate::domain::OperationState::Running,
            processed_bytes: sh.done_bytes.load(Ordering::SeqCst),
            total_bytes: Some(total_bytes),
            processed_files: sh.done_files.load(Ordering::SeqCst),
            total_files: Some(total_files),
            current_path: Some(sh.current.lock().unwrap().clone()),
            rate_bytes_per_sec: None,
            error: None,
            failures: Vec::new(),
            queued_at_ms: None,
            finished_at_ms: None,
        });
    };

    std::thread::scope(|scope| {
        let sh = &sh;
        std::thread::Builder::new()
            .name("ferrum-copy-walker".into())
            .spawn_scoped(scope, move || {
                struct Done<'x, 'y>(&'x Shared<'y>);
                impl Drop for Done<'_, '_> {
                    fn drop(&mut self) {
                        self.0.queue.lock().unwrap().walker_done = true;
                        self.0.cv.notify_all();
                    }
                }
                let _done = Done(sh);
                walk_sources(sh, sources, &target, policy);
            })
            .expect("spawn copy walker");

        let mut aimd = Aimd::default();
        let mut window_start = Instant::now();
        let (mut win_bytes, mut win_files) = (0u64, 0u64);
        let mut last_emit = Instant::now() - EMIT_EVERY;
        let mut last_state_emit = Instant::now() - STATE_EVERY;
        let mut last_state = control.state();
        let mut worker_seq = 0usize;
        loop {
            let (drained, takeable, busy) = {
                let q = sh.queue.lock().unwrap();
                (
                    q.walker_done && q.len() == 0,
                    q.takeable(sh.large_cap.min(control.target().max(1))),
                    q.busy,
                )
            };
            let idle = control.active().saturating_sub(busy);
            // 1. Bring the worker count to the live target, but only while there is
            //    work for another worker (idle workers would just wait).
            if !drained && !sh.aborted.load(Ordering::SeqCst) && !cancel.is_cancelled() {
                let mut idle = idle;
                while control.active() < control.target()
                    && (control.active() == 0 || takeable > idle)
                {
                    let first = control.active() == 0;
                    idle += 1;
                    if !copy_pool::global_budget().try_acquire(first) {
                        break;
                    }
                    control.worker_started();
                    worker_seq += 1;
                    let spawned = std::thread::Builder::new()
                        .name(format!("ferrum-copy-{worker_seq}"))
                        .spawn_scoped(scope, move || worker(sh));
                    if spawned.is_err() {
                        control.worker_stopped();
                        copy_pool::global_budget().release();
                        break;
                    }
                }
            }
            // 2. Auto mode: judge throughput once per window.
            if window_start.elapsed() >= AIMD_WINDOW {
                let secs = window_start.elapsed().as_secs_f64();
                let bytes = sh.done_bytes.load(Ordering::SeqCst);
                let files = sh.done_files.load(Ordering::SeqCst);
                if control.is_auto() {
                    let backlog = takeable > idle;
                    let next = aimd.step(AimdInput {
                        sample: Sample {
                            bytes_per_sec: (bytes - win_bytes) as f64 / secs,
                            files_per_sec: (files - win_files) as f64 / secs,
                        },
                        target: control.target(),
                        ceiling: control.auto_max(),
                        backlog,
                    });
                    control.set_auto_target(next);
                } else {
                    aimd = Aimd::default(); // re-enabling auto starts from a fresh baseline
                }
                (win_bytes, win_files) = (bytes, files);
                window_start = Instant::now();
            }
            // 3. Progress for the queue (≤10/s; the queue throttles again).
            if last_emit.elapsed() >= EMIT_EVERY {
                last_emit = Instant::now();
                emit(sh);
            }
            let state = control.state();
            if state != last_state && last_state_emit.elapsed() >= Duration::from_millis(100)
                || last_state_emit.elapsed() >= STATE_EVERY
            {
                last_state_emit = Instant::now();
                last_state = state.clone();
                copy_pool::emit_state(&state);
            }
            // 4. Finished?
            if cancel.is_cancelled() {
                sh.abort();
            }
            if (drained || sh.aborted.load(Ordering::SeqCst)) && control.active() == 0 {
                break;
            }
            control.wait(TICK);
        }
    });

    // Workers and walker are joined: the counters are final.
    emit(&sh);
    control.end_run();
    copy_pool::emit_state(&control.state());
    let mut out = sh.out.into_inner().unwrap();
    // Workers finish in any order; report failures in a stable one.
    out.failures.sort_by(|a, b| a.source.cmp(&b.source));
    out.cancelled = sh.aborted.load(Ordering::SeqCst);
    for group in sh.groups.into_inner().unwrap() {
        if group.walked.load(Ordering::SeqCst)
            && group.pending.load(Ordering::SeqCst) == 0
            && !group.failed.load(Ordering::SeqCst)
        {
            out.completed.push((group.src.clone(), group.dest.clone()));
        }
    }
    out
}

fn same_volume(a: &Path, b: &Path) -> bool {
    fn root(p: &Path) -> Option<String> {
        match p.components().next()? {
            std::path::Component::Prefix(prefix) => {
                Some(prefix.as_os_str().to_string_lossy().to_ascii_uppercase())
            }
            _ => None,
        }
    }
    matches!((root(a), root(b)), (Some(x), Some(y)) if x == y)
}

/// Walker: resolves conflicts per top-level source, creates directories, and
/// feeds files to the pool. Runs on its own thread so workers start at once.
fn walk_sources(sh: &Shared, sources: &[String], target: &Path, policy: ConflictPolicy) {
    // Destinations already claimed by this job (two sources with the same name).
    let mut planned: HashSet<String> = HashSet::new();
    for src in sources {
        if sh.cancel.is_cancelled() {
            sh.abort();
            return;
        }
        let s = PathBuf::from(src);
        let src_name = match s.file_name() {
            Some(n) => n.to_os_string(),
            None => {
                sh.out.lock().unwrap().failures.push(FailedCopy {
                    source: src.clone(),
                    code: ErrorCode::InvalidPath,
                    message: "Source has no file name".into(),
                });
                continue;
            }
        };
        if crate::path_guard::reserved_name_is_unsafe(&src_name, &s, target) {
            sh.out.lock().unwrap().failures.push(FailedCopy {
                source: src.clone(),
                code: ErrorCode::InvalidPath,
                message: RESERVED_NAME_MSG.into(),
            });
            continue;
        }
        // A folder cannot be copied into itself (also when the target is spelled `\?\`).
        if s.is_dir() && is_within(target, &s) {
            sh.out.lock().unwrap().failures.push(FailedCopy {
                source: src.clone(),
                code: ErrorCode::InvalidPath,
                message: ITSELF_MSG.into(),
            });
            continue;
        }
        // A plain file lands directly in `target`; folders are checked entry by entry in `walk`.
        if !s.is_dir() && crate::startup_guard::is_inside(target) {
            sh.out.lock().unwrap().failures.push(FailedCopy {
                source: src.clone(),
                code: ErrorCode::InvalidPath,
                message: STARTUP_MSG.into(),
            });
            continue;
        }
        let src_modified = modified_ms(&s);
        let wanted = target.join(&src_name);
        let resolved = resolve_dest(wanted, src_modified, policy, &planned);
        let (dest, replace) = match resolved {
            Resolved::Skip | Resolved::Conflict => {
                // Ask policy at run time: leave untouched; planning already
                // surfaced the conflict to the UI (D21).
                sh.out.lock().unwrap().skipped += 1;
                continue;
            }
            Resolved::Rename(dest) => {
                sh.out.lock().unwrap().renamed += 1;
                (dest, false)
            }
            Resolved::Overwrite(p) if crate::file_identity::same_entry(&s, &p) => {
                // FER-563: replacing a file with itself (alias spelling of the same path or a
                // hard link) is never meaningful; a move built on this deletes the source next.
                sh.out.lock().unwrap().failures.push(FailedCopy {
                    source: src.clone(),
                    code: ErrorCode::InvalidPath,
                    message: "Source and destination are the same file".into(),
                });
                continue;
            }
            Resolved::Overwrite(p) => {
                sh.out.lock().unwrap().overwritten += 1;
                (p, true)
            }
            Resolved::Fresh(p) => (p, false),
        };
        planned.insert(claim_key(&dest));
        let group = Arc::new(Group {
            src: src.clone(),
            dest: dest.to_string_lossy().into_owned(),
            pending: AtomicUsize::new(0),
            failed: AtomicBool::new(false),
            walked: AtomicBool::new(false),
            replace,
        });
        sh.groups.lock().unwrap().push(group.clone());
        if walk(sh, &s, &dest, &group).is_err() {
            sh.abort();
            return;
        }
        group.walked.store(true, Ordering::SeqCst);
    }
}

fn cancelled_err() -> FsError {
    err(ErrorCode::Cancelled, "cancelled")
}

/// Depth-first walk of one source tree. Directories and links are handled
/// here; files become pool tasks. Err only for cancellation.
fn walk(sh: &Shared, src: &Path, dest: &Path, group: &Arc<Group>) -> Result<(), FsError> {
    let meta = match fs::symlink_metadata(src) {
        Ok(m) => m,
        Err(e) => {
            sh.fail(group, src, FsError::from(e));
            return Ok(()); // per-item failure recorded; batch continues
        }
    };
    if meta.is_symlink() {
        return copy_link(sh, src, dest, group);
    }
    if meta.is_dir() {
        // FER-559: no directory is created inside a Startup folder, however deep in the tree.
        if crate::startup_guard::is_inside(dest) {
            sh.fail(group, src, err(ErrorCode::InvalidPath, STARTUP_MSG));
            return Ok(());
        }
        if let Err(e) = fs::create_dir_all(dest) {
            sh.fail(group, src, FsError::from(e));
            return Ok(());
        }
        for item in fs::read_dir(src).into_iter().flatten().flatten() {
            if sh.cancel.is_cancelled() {
                return Err(cancelled_err());
            }
            let child = item.path();
            let child_name = child.file_name().unwrap_or_default();
            if crate::path_guard::reserved_name_is_unsafe(child_name, src, dest) {
                sh.fail(
                    group,
                    &child,
                    err(ErrorCode::InvalidPath, RESERVED_NAME_MSG),
                );
                continue;
            }
            walk(sh, &child, &dest.join(child_name), group)?;
        }
        return Ok(());
    }
    // File: hand over to the pool (bounded queue = backpressure).
    // EVERY file task claims its destination: two tasks for the same normalized path never commit
    // in parallel (replacing ones run in source order; a non-replacing duplicate then fails cleanly).
    let claim = {
        let key = claim_key(dest);
        let mut inflight = sh.inflight.lock().unwrap();
        while inflight.contains(&key) {
            if sh.cancel.is_cancelled() {
                return Err(cancelled_err());
            }
            inflight = sh.inflight_cv.wait_timeout(inflight, TICK).unwrap().0;
        }
        inflight.insert(key.clone());
        Some(key)
    };
    group.pending.fetch_add(1, Ordering::SeqCst);
    let task = Task {
        src: src.to_path_buf(),
        dest: dest.to_path_buf(),
        size: meta.len(),
        group: group.clone(),
        claim,
    };
    let mut q = sh.queue.lock().unwrap();
    while q.len() >= QUEUE_CAP {
        if sh.cancel.is_cancelled() {
            return Err(cancelled_err());
        }
        q = sh.cv.wait_timeout(q, TICK).unwrap().0;
    }
    if task.size >= SMALL_FILE_LIMIT {
        q.large.push_back(task);
    } else {
        q.small.push_back(task);
    }
    drop(q);
    sh.cv.notify_all();
    Ok(())
}

fn copy_link(sh: &Shared, src: &Path, dest: &Path, group: &Arc<Group>) -> Result<(), FsError> {
    // D07: copy the link itself, never follow into the target.
    #[cfg(windows)]
    {
        let _ = dest;
        sh.fail(
            group,
            src,
            err(
                ErrorCode::Unsupported,
                "Copying symlinks requires Windows developer mode",
            ),
        );
        Ok(())
    }
    #[cfg(unix)]
    {
        let options = sh.options;
        let unsupported = |why: &str| sh.fail(group, src, err(ErrorCode::Unsupported, why));
        if options.verify_after_copy {
            unsupported("Symlink verification is unsupported");
        } else if options.preserve_permissions {
            unsupported("Symlink permission preservation is unsupported");
        } else if options.preserve_timestamps {
            unsupported("Symlink timestamp preservation is unsupported");
        } else {
            match fs::read_link(src) {
                Ok(target) => {
                    if let Err(error) = std::os::unix::fs::symlink(&target, dest) {
                        sh.fail(group, src, FsError::from(error));
                    }
                }
                Err(error) => sh.fail(group, src, FsError::from(error)),
            }
        }
        Ok(())
    }
}

/// Take the next task, preferring a large file while a large slot is free so
/// big copies overlap with the small-file stream.
fn pop_task(sh: &Shared, q: &mut TaskQueue) -> Option<(Task, bool)> {
    let large_cap = sh.large_cap.min(sh.control.target().max(1));
    if q.large_running < large_cap
        && let Some(t) = q.large.pop_front()
    {
        q.large_running += 1;
        q.busy += 1;
        return Some((t, true));
    }
    let task = q.small.pop_front()?;
    q.busy += 1;
    Some((task, false))
}

/// Releases worker accounting on every exit path.
struct WorkerGuard<'a> {
    control: &'a CopyControl,
    retired: bool,
}

impl Drop for WorkerGuard<'_> {
    fn drop(&mut self) {
        if !self.retired {
            self.control.worker_stopped();
        }
        copy_pool::global_budget().release();
        apply_priority(false);
    }
}

fn worker(sh: &Shared) {
    let mut guard = WorkerGuard {
        control: sh.control,
        retired: false,
    };
    loop {
        if sh.cancel.is_cancelled() || sh.aborted.load(Ordering::SeqCst) {
            sh.abort();
            return;
        }
        // [-]: finish at a file boundary, then stop.
        if sh.control.should_retire() {
            guard.retired = true;
            return;
        }
        apply_priority(sh.control.is_background());
        let picked = {
            let mut q = sh.queue.lock().unwrap();
            loop {
                if let Some(p) = pop_task(sh, &mut q) {
                    break Some(p);
                }
                if q.walker_done && q.len() == 0 {
                    break None;
                }
                if sh.cancel.is_cancelled() || sh.aborted.load(Ordering::SeqCst) {
                    break None;
                }
                let (g, _) = sh.cv.wait_timeout(q, TICK).unwrap();
                q = g;
                // A lowered target or a cancel must not wait for new work.
                if sh.control.active() > sh.control.target() {
                    break None;
                }
            }
        };
        let Some((task, was_large)) = picked else {
            if sh.control.active() > sh.control.target() {
                continue; // loop back to retire cleanly
            }
            if sh.cancel.is_cancelled() {
                sh.abort();
            }
            return;
        };
        let group = task.group.clone();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_task(sh, &task)));
        let result = result.unwrap_or_else(|_| Err(err(ErrorCode::Io, "copy worker panicked")));
        match result {
            Ok(()) => {}
            Err(e) if e.code == ErrorCode::Cancelled => sh.abort(),
            Err(e) => sh.fail(&group, &task.src, e),
        }
        group.pending.fetch_sub(1, Ordering::SeqCst);
        if let Some(key) = &task.claim {
            sh.inflight.lock().unwrap().remove(key);
            sh.inflight_cv.notify_all();
        }
        {
            let mut q = sh.queue.lock().unwrap();
            q.busy -= 1;
            if was_large {
                q.large_running -= 1;
            }
        }
        sh.cv.notify_all();
    }
}

fn run_task(sh: &Shared, task: &Task) -> Result<(), FsError> {
    *sh.current.lock().unwrap() = task.src.to_string_lossy().into_owned();
    copy_one(sh, &task.src, &task.dest, task.size, task.group.replace)?;
    sh.done_files.fetch_add(1, Ordering::SeqCst);
    let mut out = sh.out.lock().unwrap();
    out.copied_files += 1;
    out.copied_bytes += task.size;
    Ok(())
}

/// Removes the staged file on every exit path (error, cancel, panic) unless disarmed after commit.
struct StagedGuard(Option<PathBuf>);

impl StagedGuard {
    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for StagedGuard {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            remove_staged(&path);
        }
    }
}

/// Delete a staged/backup file even when CopyFile2 carried the read-only attribute over.
fn remove_staged(path: &Path) {
    if fs::remove_file(path).is_ok() {
        return;
    }
    if let Ok(meta) = fs::metadata(path) {
        let mut permissions = meta.permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        let _ = fs::set_permissions(path, permissions);
        let _ = fs::remove_file(path);
    }
}

/// FlushFileBuffers on a staged file, releasing then restoring the read-only attribute that
/// CopyFile2 may have copied from the source.
fn flush_staged(staged: &Path, dest: &Path) -> Result<(), FsError> {
    let path_err = |e: std::io::Error| FsError::from(e).with_path(staged.to_string_lossy());
    let mut permissions = fs::metadata(staged).map_err(path_err)?.permissions();
    let was_readonly = permissions.readonly();
    if was_readonly {
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        fs::set_permissions(staged, permissions.clone()).map_err(path_err)?;
    }
    let file = fs::File::options()
        .write(true)
        .open(staged)
        .map_err(path_err)?;
    let synced = file.sync_all();
    drop(file);
    if was_readonly {
        permissions.set_readonly(true);
        fs::set_permissions(staged, permissions).map_err(path_err)?;
    }
    synced.map_err(path_err)?;
    #[cfg(test)]
    note_event(dest, "flush");
    let _ = dest;
    Ok(())
}

/// (length, modified) of a source: a change between two reads means it was written while copied.
fn source_stamp(src: &Path) -> Option<(u64, Option<std::time::SystemTime>)> {
    let meta = fs::metadata(src).ok()?;
    Some((meta.len(), meta.modified().ok()))
}

/// Copy one file through a sibling staging file: a failed or cancelled copy
/// never exposes a partial destination, and replace is an atomic rename.
fn copy_one(sh: &Shared, src: &Path, dest: &Path, size: u64, replace: bool) -> Result<(), FsError> {
    let options = sh.options;
    #[cfg(windows)]
    if options.preserve_permissions {
        return Err(err(
            ErrorCode::Unsupported,
            "Permission preservation is unsupported on Windows by this copy engine",
        )
        .with_path(src.to_string_lossy()));
    }
    let (staged, file) = create_staging_file(dest)?;
    let mut guard = StagedGuard(Some(staged.clone()));
    let counted = AtomicU64::new(0);
    let on_bytes = |n: u64| {
        counted.fetch_add(n, Ordering::Relaxed);
        sh.done_bytes.fetch_add(n, Ordering::Relaxed);
        #[cfg(test)]
        call_chunk_hooks(&staged, n);
    };
    drop(file);
    let native_done = if copy_native::NATIVE_COPY && !options.stream_only {
        let abort = || {
            apply_priority(sh.control.is_background());
            sh.cancel.is_cancelled() || sh.aborted.load(Ordering::Relaxed)
        };
        match copy_native::copy_file(src, &staged, size >= UNBUFFERED_LIMIT, &abort, &on_bytes) {
            Ok(()) => {
                finish_native_times(src, &staged, options)?;
                if options.durable {
                    flush_staged(&staged, dest)?;
                }
                true
            }
            Err(NativeCopyError::Cancelled) => return Err(cancelled_err()),
            Err(NativeCopyError::Io(e)) => {
                return Err(FsError::from(e).with_path(src.to_string_lossy()));
            }
            Err(NativeCopyError::SharingViolation) => {
                // Locked by another writer: the portable copy shares everything.
                sh.done_bytes
                    .fetch_sub(counted.swap(0, Ordering::Relaxed), Ordering::Relaxed);
                false
            }
        }
    } else {
        false
    };
    if !native_done {
        copy_stream(sh, src, &staged, dest, &on_bytes)?;
        set_copy_metadata(src, &staged, options)?;
    }
    #[cfg(test)]
    call_after_copy_hooks(&staged);
    if sh.cancel.is_cancelled() {
        return Err(cancelled_err());
    }
    if options.verify_after_copy && !files_match(src, &staged, options.buffer_size, sh.cancel)? {
        return Err(err(ErrorCode::Io, "Post-copy verification failed"));
    }
    if sh.cancel.is_cancelled() {
        return Err(cancelled_err());
    }
    commit_staged(&staged, dest, replace, sh.replace_guard)?;
    guard.disarm();
    if options.durable {
        // The staged data is already on disk; record the rename itself before the caller may delete a source.
        if let Some(parent) = dest.parent() {
            copy_native::flush_dir(parent);
        }
        #[cfg(test)]
        note_event(dest, "dirflush");
    }
    Ok(())
}

/// CopyFile2 already carries timestamps and attributes over. Only the
/// "do not preserve timestamps" preference needs work: stamp the copy as new.
fn finish_native_times(_src: &Path, staged: &Path, options: CopyOptions) -> Result<(), FsError> {
    if options.preserve_timestamps {
        return Ok(());
    }
    let path_err = |e: std::io::Error| FsError::from(e).with_path(staged.to_string_lossy());
    let mut permissions = fs::metadata(staged).map_err(path_err)?.permissions();
    let was_readonly = permissions.readonly();
    if was_readonly {
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        fs::set_permissions(staged, permissions.clone()).map_err(path_err)?;
    }
    let file = fs::File::options()
        .write(true)
        .open(staged)
        .map_err(path_err)?;
    file.set_times(fs::FileTimes::new().set_modified(std::time::SystemTime::now()))
        .map_err(path_err)?;
    drop(file);
    if was_readonly {
        permissions.set_readonly(true);
        fs::set_permissions(staged, permissions).map_err(path_err)?;
    }
    Ok(())
}

/// Portable buffered copy into the (already created) staging file.
fn copy_stream(
    sh: &Shared,
    src: &Path,
    staged: &Path,
    dest: &Path,
    on_bytes: &dyn Fn(u64),
) -> Result<(), FsError> {
    let before = source_stamp(src);
    let mut src_file =
        fs::File::open(src).map_err(|e| FsError::from(e).with_path(src.to_string_lossy()))?;
    let mut dest_file = fs::File::options()
        .write(true)
        .truncate(true)
        .open(staged)
        .map_err(|e| FsError::from(e).with_path(staged.to_string_lossy()))?;
    let mut buf = vec![0u8; sh.options.buffer_size];
    loop {
        if sh.cancel.is_cancelled() || sh.aborted.load(Ordering::Relaxed) {
            return Err(cancelled_err());
        }
        apply_priority(sh.control.is_background());
        let n = src_file
            .read(&mut buf)
            .map_err(|e| FsError::from(e).with_path(src.to_string_lossy()))?;
        if n == 0 {
            break;
        }
        dest_file
            .write_all(&buf[..n])
            .map_err(|e| FsError::from(e).with_path(staged.to_string_lossy()))?;
        on_bytes(n as u64);
    }
    dest_file
        .sync_all()
        .map_err(|e| FsError::from(e).with_path(staged.to_string_lossy()))?;
    #[cfg(test)]
    note_event(dest, "flush");
    let _ = dest;
    // A source written by someone else while we read it (this path shares everything) can give a
    // torn copy: fail the item instead of publishing it, so a move never deletes such a source.
    if source_stamp(src) != before {
        return Err(err(
            ErrorCode::Io,
            "Sumber berubah selama disalin; salinan dibuang dan sumber dibiarkan",
        )
        .with_path(src.to_string_lossy()));
    }
    Ok(())
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
                "ferrum-t11-{tag}-{}-{}",
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

    fn no_cancel() -> CancelToken {
        CancelToken::default()
    }

    fn silent(_p: OperationProgress) {}

    fn write(p: &Path, bytes: &[u8]) {
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(p, bytes).unwrap();
    }

    fn read(p: &Path) -> Vec<u8> {
        fs::read(p).unwrap()
    }

    fn run(sources: &[PathBuf], target: &Path, policy: ConflictPolicy) -> CopyOutcome {
        let srcs: Vec<String> = sources
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        run_copy(
            &srcs,
            &target.to_string_lossy(),
            policy,
            &no_cancel(),
            &silent,
        )
    }

    #[test]
    fn last_progress_counts_every_copied_file() {
        // FER-382: the queue keeps the engine's LAST progress as the job's final count, so the last
        // report must already include the final file (was N-1: a single-file job read 0/1).
        for n in [1usize, 3] {
            let root = temp("lastprog");
            let src = root.join("src");
            let dst = root.join("dst");
            fs::create_dir_all(&dst).unwrap();
            let mut srcs = Vec::new();
            for i in 0..n {
                let p = src.join(format!("f{i}.txt"));
                write(&p, b"hello");
                srcs.push(p.to_string_lossy().into_owned());
            }
            let last = std::sync::Mutex::new(None::<OperationProgress>);
            let out = run_copy(
                &srcs,
                &dst.to_string_lossy(),
                ConflictPolicy::Skip,
                &no_cancel(),
                &|p| *last.lock().unwrap() = Some(p),
            );
            assert_eq!(out.copied_files, n as u64);
            let last = last.lock().unwrap().clone().expect("progress reported");
            assert_eq!(last.processed_files, n as u64, "n={n}");
            assert_eq!(last.total_files, Some(n as u64));
            assert_eq!(last.processed_bytes, 5 * n as u64);
        }
    }

    #[test]
    fn copies_files_byte_identical() {
        let root = temp("bytes");
        let data: Vec<u8> = (0..250_000u32).map(|i| (i % 251) as u8).collect();
        write(&root.join("a.bin"), &data);
        write(&root.join("b.bin"), &[1, 2, 3]);
        let dest = root.join("out");
        fs::create_dir(&dest).unwrap();
        let out = run(
            &[root.join("a.bin"), root.join("b.bin")],
            &dest,
            ConflictPolicy::Ask,
        );
        assert_eq!(out.copied_files, 2);
        assert!(out.failures.is_empty());
        assert_eq!(read(&dest.join("a.bin")), data);
        assert_eq!(read(&dest.join("b.bin")), vec![1, 2, 3]);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn copies_directories_recursively() {
        let root = temp("recurse");
        write(&root.join("src/sub/deep/x.txt"), b"deep");
        write(&root.join("src/sub/y.txt"), b"y");
        write(&root.join("src/top.txt"), b"top");
        let dest = root.join("out");
        fs::create_dir(&dest).unwrap();
        let out = run(&[root.join("src")], &dest, ConflictPolicy::Ask);
        assert_eq!(out.copied_files, 3);
        assert_eq!(read(&dest.join("src/sub/deep/x.txt")), b"deep");
        assert_eq!(read(&dest.join("src/top.txt")), b"top");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn many_small_files() {
        let root = temp("many");
        let src = root.join("files");
        fs::create_dir_all(&src).unwrap();
        for i in 0..100 {
            write(
                &src.join(format!("f{i:03}.txt")),
                format!("content-{i}").as_bytes(),
            );
        }
        let dest = root.join("out");
        fs::create_dir(&dest).unwrap();
        let out = run(&[src], &dest, ConflictPolicy::Ask);
        assert_eq!(out.copied_files, 100);
        assert_eq!(read(&dest.join("files/f042.txt")), b"content-42");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn conflicts_are_detected_by_plan() {
        let root = temp("conflict");
        write(&root.join("src/keep.txt"), b"new");
        let dest = root.join("out");
        write(&dest.join("keep.txt"), b"old");
        let plan = plan_copy(
            &[root.join("src/keep.txt").to_string_lossy().into_owned()],
            &dest.to_string_lossy(),
        )
        .unwrap();
        assert_eq!(plan.conflicts.len(), 1);
        assert_eq!(plan.total_files, 1);
        // Ask policy copies nothing for the conflicted item.
        let out = run(&[root.join("src/keep.txt")], &dest, ConflictPolicy::Ask);
        assert_eq!(out.copied_files, 0);
        assert_eq!(read(&dest.join("keep.txt")), b"old"); // untouched
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn conflict_policies_skip_overwrite_and_rename() {
        let mk = |tag: &str| {
            let root = temp(tag);
            write(&root.join("src/file.txt"), b"NEW-CONTENT");
            let dest = root.join("out");
            write(&dest.join("file.txt"), b"OLD");
            (root, dest)
        };

        let (r, d) = mk("skip");
        let out = run(&[r.join("src/file.txt")], &d, ConflictPolicy::Skip);
        assert_eq!(out.skipped, 1);
        assert_eq!(read(&d.join("file.txt")), b"OLD");

        let (r, d) = mk("over");
        let out = run(&[r.join("src/file.txt")], &d, ConflictPolicy::Overwrite);
        assert_eq!(read(&d.join("file.txt")), b"NEW-CONTENT");
        assert_eq!(out.overwritten, 1);

        let (r, d) = mk("rename");
        let out = run(&[r.join("src/file.txt")], &d, ConflictPolicy::RenameAuto);
        assert_eq!(read(&d.join("file.txt")), b"OLD"); // kept
        assert_eq!(read(&d.join("file (2).txt")), b"NEW-CONTENT");
        assert_eq!(out.renamed, 1);

        // OverwriteOlder: older source does NOT replace newer dest.
        let (r, d) = mk("older");
        let old_time = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        let f = fs::File::options()
            .write(true)
            .open(r.join("src/file.txt"))
            .unwrap();
        f.set_modified(old_time).unwrap();
        drop(f);
        let _out = run(
            &[r.join("src/file.txt")],
            &d,
            ConflictPolicy::OverwriteOlder,
        );
        assert_eq!(read(&d.join("file.txt")), b"OLD");
        drop(r);
    }

    #[test]
    fn self_copy_and_nested_target_are_rejected() {
        let root = temp("self");
        write(&root.join("src/a.txt"), b"x");
        let dest = root.join("src/inner");
        fs::create_dir_all(&dest).unwrap();
        // target inside source
        let e = plan_copy(
            &[root.join("src").to_string_lossy().into_owned()],
            &dest.to_string_lossy(),
        )
        .unwrap_err();
        assert_eq!(e.code, ErrorCode::InvalidPath);
        // target not a directory
        let e2 = plan_copy(
            &[root.join("src/a.txt").to_string_lossy().into_owned()],
            &root.join("src/a.txt").to_string_lossy(),
        )
        .unwrap_err();
        assert_eq!(e2.code, ErrorCode::NotADirectory);
        // run_copy surfaces the failure per-item without panic
        let out = run(&[root.join("src")], &dest, ConflictPolicy::Ask);
        assert_eq!(out.copied_files, 0);
        assert!(!out.failures.is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn cancel_removes_partial_output_and_keeps_source() {
        let root = temp("cancel");
        // 5 MB → at least 5 chunks; cancelling mid-way exercises cleanup.
        let big: Vec<u8> = vec![7u8; 5 * CHUNK];
        write(&root.join("big.bin"), &big);
        let dest = root.join("out");
        fs::create_dir(&dest).unwrap();

        let token = CancelToken::default();
        // Deterministic: cancel from inside the first inter-chunk progress
        // report instead of racing a timer.
        let tok2 = token.clone();
        hook_chunks(&dest, move |_| tok2.cancel());
        let srcs = [root.join("big.bin")];
        let srcs_ref: Vec<String> = srcs
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        let out = run_copy_with_options(
            &srcs_ref,
            &dest.to_string_lossy(),
            ConflictPolicy::Ask,
            CopyOptions {
                stream_only: true,
                ..CopyOptions::default()
            },
            &token,
            &silent,
        );
        unhook_chunks(&dest);
        assert!(out.cancelled);
        assert_eq!(out.copied_files, 0);
        assert!(staging_leftovers(&dest).is_empty());
        // Partial file must be gone; source intact.
        assert!(!dest.join("big.bin").exists());
        assert_eq!(read(&root.join("big.bin")).len(), 5 * CHUNK);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn failure_to_create_target_is_reported_per_item() {
        let root = temp("failtarget");
        write(&root.join("file.txt"), b"x");
        // target "dir" is actually a FILE -> create_dir_all fails per item
        write(&root.join("not-a-dir"), b"");
        let out = run(
            &[root.join("file.txt")],
            &root.join("not-a-dir"),
            ConflictPolicy::Ask,
        );
        assert_eq!(out.copied_files, 0);
        assert!(!out.failures.is_empty());
        assert_eq!(read(&root.join("file.txt")), b"x"); // source untouched
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn copy_buffer_is_bounded_to_configured_limits() {
        assert_eq!(
            CopyOptions {
                buffer_size: 0,
                ..CopyOptions::default()
            }
            .bounded_buffer_size(),
            MIN_COPY_BUFFER
        );
        assert_eq!(
            CopyOptions {
                buffer_size: usize::MAX,
                ..CopyOptions::default()
            }
            .bounded_buffer_size(),
            MAX_COPY_BUFFER
        );
        assert_eq!(
            CopyOptions {
                buffer_size: 128 * 1024,
                ..CopyOptions::default()
            }
            .bounded_buffer_size(),
            128 * 1024
        );
    }

    #[test]
    fn verification_succeeds_for_identical_copy() {
        let root = temp("verify");
        write(&root.join("input.bin"), &vec![0x5a; 3 * 1024 * 1024]);
        let dest = root.join("out");
        fs::create_dir(&dest).unwrap();
        let sources = [root.join("input.bin").to_string_lossy().into_owned()];
        let options = CopyOptions {
            verify_after_copy: true,
            buffer_size: MIN_COPY_BUFFER,
            ..CopyOptions::default()
        };
        let out = run_copy_with_options(
            &sources,
            &dest.to_string_lossy(),
            ConflictPolicy::Ask,
            options,
            &no_cancel(),
            &silent,
        );
        assert_eq!(out.copied_files, 1);
        assert!(out.failures.is_empty());
        assert_eq!(read(&dest.join("input.bin")), read(&root.join("input.bin")));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn verification_detects_content_mismatch() {
        let root = temp("verify-mismatch");
        write(&root.join("source.bin"), b"source");
        write(&root.join("staged.bin"), b"tamper");
        assert!(
            !files_match(
                &root.join("source.bin"),
                &root.join("staged.bin"),
                MIN_COPY_BUFFER,
                &no_cancel()
            )
            .unwrap()
        );
        let _ = fs::remove_dir_all(&root);
    }

    fn staging_leftovers(dir: &Path) -> Vec<String> {
        let mut found = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in fs::read_dir(&d).into_iter().flatten().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.to_string_lossy().contains(".ferrum-stage-") {
                    found.push(p.to_string_lossy().into_owned());
                }
            }
        }
        found
    }

    #[test]
    fn failed_or_cancelled_staged_copy_keeps_old_destination_and_source() {
        let root = temp("staged-failure");
        write(&root.join("input.bin"), &vec![0x33; 5 * MIN_COPY_BUFFER]);
        let dest = root.join("out");
        write(&dest.join("input.bin"), b"old destination");
        let token = CancelToken::default();
        let tok2 = token.clone();
        let written = std::sync::atomic::AtomicU64::new(0);
        hook_chunks(&dest, move |n| {
            if written.fetch_add(n, Ordering::SeqCst) + n >= MIN_COPY_BUFFER as u64 {
                tok2.cancel();
            }
        });
        let sources = [root.join("input.bin").to_string_lossy().into_owned()];
        let options = CopyOptions {
            buffer_size: MIN_COPY_BUFFER,
            stream_only: true,
            ..CopyOptions::default()
        };
        let out = run_copy_with_options(
            &sources,
            &dest.to_string_lossy(),
            ConflictPolicy::Overwrite,
            options,
            &token,
            &silent,
        );
        unhook_chunks(&dest);
        assert!(out.cancelled);
        assert_eq!(out.copied_files, 0);
        assert_eq!(read(&dest.join("input.bin")), b"old destination");
        assert_eq!(read(&root.join("input.bin")).len(), 5 * MIN_COPY_BUFFER);
        assert_eq!(fs::read_dir(&dest).unwrap().count(), 1);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn preserves_modified_timestamp_when_requested() {
        let root = temp("timestamps");
        write(&root.join("input.txt"), b"timestamp");
        let expected = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        fs::File::options()
            .write(true)
            .open(root.join("input.txt"))
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(expected))
            .unwrap();
        let dest = root.join("out");
        fs::create_dir(&dest).unwrap();
        let sources = [root.join("input.txt").to_string_lossy().into_owned()];
        let options = CopyOptions {
            preserve_timestamps: true,
            ..CopyOptions::default()
        };
        let out = run_copy_with_options(
            &sources,
            &dest.to_string_lossy(),
            ConflictPolicy::Ask,
            options,
            &no_cancel(),
            &silent,
        );
        assert_eq!(out.copied_files, 1);
        let source_time = fs::metadata(root.join("input.txt"))
            .unwrap()
            .modified()
            .unwrap();
        let dest_time = fs::metadata(dest.join("input.txt"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(dest_time, source_time);
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn preserves_unix_permissions_when_requested() {
        use std::os::unix::fs::PermissionsExt;
        let root = temp("permissions");
        write(&root.join("input.txt"), b"permissions");
        fs::set_permissions(root.join("input.txt"), fs::Permissions::from_mode(0o640)).unwrap();
        let dest = root.join("out");
        fs::create_dir(&dest).unwrap();
        let sources = [root.join("input.txt").to_string_lossy().into_owned()];
        let options = CopyOptions {
            preserve_permissions: true,
            ..CopyOptions::default()
        };
        let out = run_copy_with_options(
            &sources,
            &dest.to_string_lossy(),
            ConflictPolicy::Ask,
            options,
            &no_cancel(),
            &silent,
        );
        assert_eq!(out.copied_files, 1);
        assert_eq!(
            fs::metadata(dest.join("input.txt"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o640
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(windows)]
    #[test]
    fn windows_permissions_preservation_is_explicitly_unsupported() {
        let root = temp("unsupported-permissions");
        write(&root.join("input.txt"), b"permissions");
        let dest = root.join("out");
        fs::create_dir(&dest).unwrap();
        let sources = [root.join("input.txt").to_string_lossy().into_owned()];
        let options = CopyOptions {
            preserve_permissions: true,
            ..CopyOptions::default()
        };
        let out = run_copy_with_options(
            &sources,
            &dest.to_string_lossy(),
            ConflictPolicy::Ask,
            options,
            &no_cancel(),
            &silent,
        );
        assert_eq!(out.copied_files, 0);
        assert_eq!(out.failures[0].code, ErrorCode::Unsupported);
        assert!(!dest.join("input.txt").exists());
        let _ = fs::remove_dir_all(&root);
    }

    // ───────────── FER-471: parallel pool ─────────────

    fn make_tree(root: &Path, files: usize, big: usize) {
        for i in 0..files {
            let size = 200 + (i * 7919) % 30_000;
            let data: Vec<u8> = (0..size).map(|j| ((i + j) % 251) as u8).collect();
            write(
                &root.join(format!("d{}", i % 7)).join(format!("f{i}.bin")),
                &data,
            );
        }
        for i in 0..big {
            let data: Vec<u8> = (0..5 * 1024 * 1024 + i * 13)
                .map(|j| ((i * 3 + j) % 253) as u8)
                .collect();
            write(&root.join(format!("big{i}.bin")), &data);
        }
    }

    fn assert_same_tree(a: &Path, b: &Path) {
        let mut stack = vec![a.to_path_buf()];
        let mut checked = 0;
        while let Some(d) = stack.pop() {
            for e in fs::read_dir(&d).unwrap().flatten() {
                let p = e.path();
                let rel = p.strip_prefix(a).unwrap();
                if p.is_dir() {
                    assert!(b.join(rel).is_dir(), "missing dir {rel:?}");
                    stack.push(p);
                } else {
                    assert_eq!(read(&p), read(&b.join(rel)), "content differs: {rel:?}");
                    checked += 1;
                }
            }
        }
        assert!(checked > 0);
    }

    fn opts(threads: ThreadSetting) -> CopyOptions {
        CopyOptions {
            threads,
            ..CopyOptions::default()
        }
    }

    #[test]
    fn parallel_copy_of_a_tree_is_byte_identical_for_every_thread_mode() {
        for threads in [
            ThreadSetting::Fixed(1),
            ThreadSetting::Fixed(8),
            ThreadSetting::Fixed(16),
            ThreadSetting::Auto,
        ] {
            let root = temp("parallel");
            let src = root.join("tree");
            make_tree(&src, 260, 3);
            fs::create_dir_all(src.join("empty-dir")).unwrap();
            let dest = root.join("out");
            fs::create_dir(&dest).unwrap();
            let out = run_copy_with_options(
                &[src.to_string_lossy().into_owned()],
                &dest.to_string_lossy(),
                ConflictPolicy::Ask,
                opts(threads),
                &no_cancel(),
                &silent,
            );
            assert!(out.failures.is_empty(), "{threads:?}: {:?}", out.failures);
            assert_eq!(out.copied_files, 263, "{threads:?}");
            assert_eq!(out.completed.len(), 1);
            assert!(dest.join("tree").join("empty-dir").is_dir());
            assert_same_tree(&src, &dest.join("tree"));
            assert!(staging_leftovers(&dest).is_empty());
            let _ = fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn stream_only_path_matches_the_native_path() {
        let root = temp("stream-only");
        let src = root.join("tree");
        make_tree(&src, 40, 1);
        let dest = root.join("out");
        fs::create_dir(&dest).unwrap();
        let out = run_copy_with_options(
            &[src.to_string_lossy().into_owned()],
            &dest.to_string_lossy(),
            ConflictPolicy::Ask,
            CopyOptions {
                stream_only: true,
                ..opts(ThreadSetting::Fixed(4))
            },
            &no_cancel(),
            &silent,
        );
        assert!(out.failures.is_empty());
        assert_eq!(out.copied_files, 41);
        assert_same_tree(&src, &dest.join("tree"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn live_shrink_finishes_files_cleanly_and_leaves_no_staging() {
        let root = temp("shrink");
        let src = root.join("tree");
        make_tree(&src, 160, 0);
        let dest = root.join("out");
        fs::create_dir(&dest).unwrap();
        // Slow every chunk a little so the run lasts several progress ticks.
        hook_chunks(&dest, |_| std::thread::sleep(Duration::from_millis(8)));
        let control = CopyControl::new(None, ThreadSetting::Fixed(8), Priority::Normal);
        let seen = Mutex::new((false, Vec::<usize>::new()));
        let out = {
            let report = |p: OperationProgress| {
                let mut seen = seen.lock().unwrap();
                if p.processed_files >= 5 && !seen.0 {
                    seen.0 = true;
                    control.adjust(copy_pool::Adjust::Down { by: 7 });
                }
                if seen.0 {
                    seen.1.push(control.active());
                }
            };
            run_copy_controlled(
                &[src.to_string_lossy().into_owned()],
                &dest.to_string_lossy(),
                ConflictPolicy::Ask,
                CopyOptions {
                    stream_only: true,
                    ..opts(ThreadSetting::Fixed(8))
                },
                &control,
                &no_cancel(),
                &report,
            )
        };
        unhook_chunks(&dest);
        assert!(out.failures.is_empty(), "{:?}", out.failures);
        assert_eq!(out.copied_files, 160);
        assert_eq!(control.target(), 1);
        let (_, actives) = seen.into_inner().unwrap();
        assert!(
            actives.contains(&1),
            "pool never shrank to one worker: {actives:?}"
        );
        assert_same_tree(&src, &dest.join("tree"));
        assert!(staging_leftovers(&dest).is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn live_grow_adds_workers_while_work_remains() {
        let root = temp("grow");
        let src = root.join("tree");
        make_tree(&src, 200, 0);
        let dest = root.join("out");
        fs::create_dir(&dest).unwrap();
        hook_chunks(&dest, |_| std::thread::sleep(Duration::from_millis(8)));
        let control = CopyControl::new(None, ThreadSetting::Fixed(1), Priority::Normal);
        let peak = std::sync::atomic::AtomicUsize::new(0);
        let out = {
            let report = |p: OperationProgress| {
                if p.processed_files >= 3 {
                    control.adjust(copy_pool::Adjust::Set { threads: 6 });
                }
                peak.fetch_max(control.active(), Ordering::SeqCst);
            };
            run_copy_controlled(
                &[src.to_string_lossy().into_owned()],
                &dest.to_string_lossy(),
                ConflictPolicy::Ask,
                CopyOptions {
                    stream_only: true,
                    ..opts(ThreadSetting::Fixed(1))
                },
                &control,
                &no_cancel(),
                &report,
            )
        };
        unhook_chunks(&dest);
        assert!(out.failures.is_empty());
        assert_eq!(out.copied_files, 200);
        assert!(peak.load(Ordering::SeqCst) >= 4, "peak {peak:?}");
        assert_same_tree(&src, &dest.join("tree"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn cancel_in_the_middle_of_a_parallel_copy_leaves_only_whole_files() {
        let root = temp("cancel-mid");
        let src = root.join("tree");
        make_tree(&src, 200, 0);
        let dest = root.join("out");
        fs::create_dir(&dest).unwrap();
        hook_chunks(&dest, |_| std::thread::sleep(Duration::from_millis(6)));
        let token = CancelToken::default();
        let tok = token.clone();
        let report = move |p: OperationProgress| {
            if p.processed_files >= 20 {
                tok.cancel();
            }
        };
        let out = run_copy_with_options(
            &[src.to_string_lossy().into_owned()],
            &dest.to_string_lossy(),
            ConflictPolicy::Ask,
            CopyOptions {
                stream_only: true,
                ..opts(ThreadSetting::Fixed(6))
            },
            &token,
            &report,
        );
        unhook_chunks(&dest);
        assert!(out.cancelled);
        assert!(
            out.copied_files >= 20 && out.copied_files < 200,
            "{}",
            out.copied_files
        );
        assert!(
            out.completed.is_empty(),
            "a cancelled source is not complete"
        );
        assert!(
            staging_leftovers(&dest).is_empty(),
            "{:?}",
            staging_leftovers(&dest)
        );
        // Every file that exists is complete and identical.
        let copied = dest.join("tree");
        let mut stack = vec![copied.clone()];
        while let Some(d) = stack.pop() {
            for e in fs::read_dir(&d).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else {
                    let rel = p.strip_prefix(&copied).unwrap();
                    assert_eq!(read(&p), read(&src.join(rel)));
                }
            }
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn same_name_from_two_sources_follows_the_conflict_policy() {
        let root = temp("dup-names");
        write(&root.join("a").join("same.txt"), b"first");
        write(&root.join("b").join("same.txt"), b"second");
        let dest = root.join("out");
        fs::create_dir(&dest).unwrap();
        let srcs = [
            root.join("a")
                .join("same.txt")
                .to_string_lossy()
                .into_owned(),
            root.join("b")
                .join("same.txt")
                .to_string_lossy()
                .into_owned(),
        ];
        let out = run_copy_with_options(
            &srcs,
            &dest.to_string_lossy(),
            ConflictPolicy::RenameAuto,
            opts(ThreadSetting::Fixed(4)),
            &no_cancel(),
            &silent,
        );
        assert_eq!(out.copied_files, 2);
        assert_eq!(out.renamed, 1);
        assert_eq!(read(&dest.join("same.txt")), b"first");
        assert_eq!(read(&dest.join("same (2).txt")), b"second");
        let out = run_copy_with_options(
            &srcs,
            &dest.to_string_lossy(),
            ConflictPolicy::Skip,
            opts(ThreadSetting::Fixed(4)),
            &no_cancel(),
            &silent,
        );
        assert_eq!(out.skipped, 2);
        assert_eq!(out.copied_files, 0);
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(windows)]
    #[test]
    fn one_locked_file_fails_alone_and_the_rest_still_copy() {
        use std::os::windows::fs::OpenOptionsExt;
        let root = temp("locked");
        let src = root.join("tree");
        make_tree(&src, 30, 0);
        let locked = src.join("d0").join("f0.bin");
        // Exclusive handle (share mode 0): nobody else can read this file.
        let hold = fs::File::options()
            .read(true)
            .share_mode(0)
            .open(&locked)
            .unwrap();
        let dest = root.join("out");
        fs::create_dir(&dest).unwrap();
        let out = run_copy_with_options(
            &[src.to_string_lossy().into_owned()],
            &dest.to_string_lossy(),
            ConflictPolicy::Ask,
            opts(ThreadSetting::Fixed(4)),
            &no_cancel(),
            &silent,
        );
        assert_eq!(out.failures.len(), 1, "{:?}", out.failures);
        assert!(out.failures[0].source.ends_with("f0.bin"));
        assert_eq!(out.copied_files, 29);
        assert!(
            out.completed.is_empty(),
            "a source with a failure is not complete"
        );
        assert!(!dest.join("tree").join("d0").join("f0.bin").exists());
        assert!(staging_leftovers(&dest).is_empty());
        drop(hold);
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(windows)]
    #[test]
    fn source_open_for_writing_by_another_process_still_copies() {
        use std::os::windows::fs::OpenOptionsExt;
        let root = temp("write-shared");
        write(&root.join("live.log"), b"being appended");
        // Writer that allows readers/writers/deleters (like a log file in use).
        let _writer = fs::File::options()
            .write(true)
            .share_mode(7)
            .open(root.join("live.log"))
            .unwrap();
        let dest = root.join("out");
        fs::create_dir(&dest).unwrap();
        let out = run(&[root.join("live.log")], &dest, ConflictPolicy::Ask);
        assert!(out.failures.is_empty(), "{:?}", out.failures);
        assert_eq!(read(&dest.join("live.log")), b"being appended");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn background_priority_copy_completes() {
        let root = temp("background");
        let src = root.join("tree");
        make_tree(&src, 30, 1);
        let dest = root.join("out");
        fs::create_dir(&dest).unwrap();
        let out = run_copy_with_options(
            &[src.to_string_lossy().into_owned()],
            &dest.to_string_lossy(),
            ConflictPolicy::Ask,
            CopyOptions {
                priority: Priority::Background,
                ..opts(ThreadSetting::Fixed(3))
            },
            &no_cancel(),
            &silent,
        );
        assert!(out.failures.is_empty());
        assert_same_tree(&src, &dest.join("tree"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn job_control_is_shared_through_the_operation_id() {
        let root = temp("opid");
        write(&root.join("a.txt"), b"x");
        let dest = root.join("out");
        fs::create_dir(&dest).unwrap();
        let token = CancelToken::for_operation("op_test_engine_ctl");
        let out = run_copy_with_options(
            &[root.join("a.txt").to_string_lossy().into_owned()],
            &dest.to_string_lossy(),
            ConflictPolicy::Ask,
            opts(ThreadSetting::Fixed(3)),
            &token,
            &silent,
        );
        assert_eq!(out.copied_files, 1);
        let control = copy_pool::get("op_test_engine_ctl").expect("registered under the job id");
        assert_eq!(control.target(), 3);
        assert!(!control.is_running(), "run ended");
        copy_pool::forget("op_test_engine_ctl");
        let _ = fs::remove_dir_all(&root);
    }

    // ───────────── FER-471 review fixes (H1 H2 M1 M2 L1 L2 L3) ─────────────

    fn hook_staged(dir: &Path, hook: impl Fn(&Path) + Send + Sync + 'static) {
        AFTER_COPY_HOOKS
            .lock()
            .unwrap()
            .push((dir.to_path_buf(), Arc::new(hook)));
    }

    fn unhook_staged(dir: &Path) {
        AFTER_COPY_HOOKS.lock().unwrap().retain(|(d, _)| d != dir);
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    fn strings(paths: &[&Path]) -> Vec<String> {
        paths
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect()
    }

    fn copy_with(
        srcs: &[String],
        dest: &Path,
        policy: ConflictPolicy,
        options: CopyOptions,
    ) -> CopyOutcome {
        run_copy_with_options(
            srcs,
            &dest.to_string_lossy(),
            policy,
            options,
            &no_cancel(),
            &silent,
        )
    }

    #[test]
    fn h1_a_plain_copy_does_not_flush_but_durable_flushes_before_commit() {
        let root = temp("durable");
        write(&root.join("a.bin"), &vec![5u8; 300_000]);
        let plain = root.join("plain");
        let safe = root.join("safe");
        fs::create_dir_all(&plain).unwrap();
        fs::create_dir_all(&safe).unwrap();
        let src = strings(&[&root.join("a.bin")]);
        let out = copy_with(&src, &plain, ConflictPolicy::Ask, CopyOptions::default());
        assert_eq!(out.copied_files, 1);
        let ev: Vec<_> = events_under(&plain).iter().map(|e| e.1).collect();
        assert_eq!(ev, vec!["commit"], "no flush without durable");
        let out = copy_with(
            &src,
            &safe,
            ConflictPolicy::Ask,
            CopyOptions {
                durable: true,
                ..CopyOptions::default()
            },
        );
        assert_eq!(out.copied_files, 1);
        let ev: Vec<_> = events_under(&safe).iter().map(|e| e.1).collect();
        assert_eq!(
            ev,
            vec!["flush", "commit", "dirflush"],
            "flush happens before the commit"
        );
        assert_eq!(read(&safe.join("a.bin")), read(&root.join("a.bin")));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn h1_durable_flush_works_for_a_read_only_source_and_keeps_the_attribute() {
        let root = temp("durable-ro");
        write(&root.join("ro.txt"), b"read only data");
        let mut perm = fs::metadata(root.join("ro.txt")).unwrap().permissions();
        perm.set_readonly(true);
        fs::set_permissions(root.join("ro.txt"), perm).unwrap();
        let dest = root.join("out");
        fs::create_dir(&dest).unwrap();
        let out = copy_with(
            &strings(&[&root.join("ro.txt")]),
            &dest,
            ConflictPolicy::Ask,
            CopyOptions {
                durable: true,
                ..CopyOptions::default()
            },
        );
        assert!(out.failures.is_empty(), "{:?}", out.failures);
        assert_eq!(read(&dest.join("ro.txt")), b"read only data");
        assert!(
            fs::metadata(dest.join("ro.txt"))
                .unwrap()
                .permissions()
                .readonly(),
            "attribute restored after the flush"
        );
        assert!(staging_leftovers(&dest).is_empty());
        for p in [root.join("ro.txt"), dest.join("ro.txt")] {
            let mut perm = fs::metadata(&p).unwrap().permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            perm.set_readonly(false);
            fs::set_permissions(&p, perm).unwrap();
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn h2_numbered_candidates_respect_the_names_planned_in_this_job() {
        let root = temp("h2");
        write(&root.join("a").join("x.txt"), b"first");
        write(&root.join("b").join("x.txt"), b"second");
        let dest = root.join("out");
        write(&dest.join("x.txt"), b"existing");
        let srcs = strings(&[&root.join("a").join("x.txt"), &root.join("b").join("x.txt")]);
        for threads in [ThreadSetting::Fixed(1), ThreadSetting::Fixed(8)] {
            let out = copy_with(&srcs, &dest, ConflictPolicy::RenameAuto, opts(threads));
            assert!(out.failures.is_empty(), "{:?}", out.failures);
            assert_eq!(out.renamed, 2);
            assert_eq!(out.copied_files, 2);
            assert_eq!(read(&dest.join("x.txt")), b"existing");
            assert_eq!(read(&dest.join("x (2).txt")), b"first");
            assert_eq!(read(&dest.join("x (3).txt")), b"second");
            assert_eq!(names(&dest).len(), 3, "{:?}", names(&dest));
            fs::remove_file(dest.join("x (2).txt")).unwrap();
            fs::remove_file(dest.join("x (3).txt")).unwrap();
        }
        // Skip twice: nothing is touched.
        let out = copy_with(
            &srcs,
            &dest,
            ConflictPolicy::Skip,
            opts(ThreadSetting::Fixed(4)),
        );
        assert_eq!(out.skipped, 2);
        assert_eq!(out.copied_files, 0);
        assert_eq!(read(&dest.join("x.txt")), b"existing");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn m1_names_that_differ_only_in_case_are_the_same_destination() {
        let root = temp("m1");
        write(&root.join("X").join("Report.txt"), b"upper");
        write(&root.join("Y").join("report.txt"), b"lower");
        let srcs = strings(&[
            &root.join("X").join("Report.txt"),
            &root.join("Y").join("report.txt"),
        ]);
        // RenameAuto keeps both.
        let dest = root.join("out1");
        fs::create_dir(&dest).unwrap();
        let out = copy_with(
            &srcs,
            &dest,
            ConflictPolicy::RenameAuto,
            opts(ThreadSetting::Fixed(4)),
        );
        assert!(out.failures.is_empty(), "{:?}", out.failures);
        assert_eq!(out.renamed, 1);
        assert_eq!(names(&dest), vec!["Report.txt", "report (2).txt"]);
        assert_eq!(read(&dest.join("Report.txt")), b"upper");
        assert_eq!(read(&dest.join("report (2).txt")), b"lower");
        // Skip keeps the first only.
        let dest = root.join("out2");
        fs::create_dir(&dest).unwrap();
        let out = copy_with(
            &srcs,
            &dest,
            ConflictPolicy::Skip,
            opts(ThreadSetting::Fixed(4)),
        );
        assert_eq!((out.copied_files, out.skipped), (1, 1));
        assert_eq!(read(&dest.join("Report.txt")), b"upper");
        // Overwrite: the later source wins, deterministically, round after round.
        for round in 0..24 {
            let dest = root.join(format!("out3-{round}"));
            fs::create_dir(&dest).unwrap();
            let out = copy_with(
                &srcs,
                &dest,
                ConflictPolicy::Overwrite,
                opts(ThreadSetting::Fixed(8)),
            );
            assert!(out.failures.is_empty(), "round {round}: {:?}", out.failures);
            assert_eq!(names(&dest).len(), 1, "round {round}: {:?}", names(&dest));
            let only = names(&dest).remove(0);
            assert_eq!(
                read(&dest.join(only)),
                b"lower",
                "round {round}: last source wins"
            );
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn m1_a_new_destination_is_never_overwritten_by_the_commit() {
        let root = temp("m1-net");
        write(&root.join("staged"), b"new");
        write(&root.join("dest"), b"appeared meanwhile");
        let e = commit_staged(&root.join("staged"), &root.join("dest"), false, None).unwrap_err();
        assert_eq!(e.code, ErrorCode::AlreadyExists);
        assert_eq!(read(&root.join("dest")), b"appeared meanwhile");
        assert!(
            root.join("staged").exists(),
            "the caller's guard removes it"
        );
        commit_staged(&root.join("staged"), &root.join("dest"), true, None).unwrap();
        assert_eq!(read(&root.join("dest")), b"new");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn m1_a_name_that_appears_after_planning_fails_that_file_only() {
        let root = temp("m1-race");
        let src = root.join("tree");
        write(&src.join("a.txt"), b"A");
        write(&src.join("b.txt"), b"B");
        let dest = root.join("out");
        fs::create_dir(&dest).unwrap();
        // The destination's tree appears between planning and commit.
        let late = dest.join("tree").join("a.txt");
        hook_staged(&dest, {
            let late = late.clone();
            move |_| {
                if !late.exists() {
                    write(&late, b"squatter");
                }
            }
        });
        let out = copy_with(
            &strings(&[&src]),
            &dest,
            ConflictPolicy::Ask,
            opts(ThreadSetting::Fixed(1)),
        );
        unhook_staged(&dest);
        assert_eq!(read(&late), b"squatter", "never overwritten");
        assert!(
            out.failures
                .iter()
                .any(|f| f.code == ErrorCode::AlreadyExists),
            "{:?}",
            out.failures
        );
        assert!(out.completed.is_empty(), "the source is not complete");
        assert!(staging_leftovers(&dest).is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn m2_overwrite_of_one_destination_is_ordered_and_last_source_wins() {
        let root = temp("m2");
        let mut srcs = Vec::new();
        for i in 0..6 {
            let p = root.join(format!("s{i}")).join("same.bin");
            write(&p, &vec![i as u8; 3000 + i]);
            srcs.push(p.to_string_lossy().into_owned());
        }
        for round in 0..15 {
            let dest = root.join(format!("out{round}"));
            write(&dest.join("same.bin"), b"old");
            let out = copy_with(
                &srcs,
                &dest,
                ConflictPolicy::Overwrite,
                opts(ThreadSetting::Fixed(8)),
            );
            assert!(out.failures.is_empty(), "round {round}: {:?}", out.failures);
            assert_eq!(
                read(&dest.join("same.bin")),
                vec![5u8; 3005],
                "round {round}"
            );
            assert_eq!(names(&dest), vec!["same.bin"], "no backup or staging left");
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn l1_overwriting_a_read_only_destination_works_and_leaves_nothing_behind() {
        let root = temp("l1-overwrite");
        write(&root.join("f.txt"), b"fresh");
        let dest = root.join("out");
        write(&dest.join("f.txt"), b"old read-only");
        let mut perm = fs::metadata(dest.join("f.txt")).unwrap().permissions();
        perm.set_readonly(true);
        fs::set_permissions(dest.join("f.txt"), perm).unwrap();
        let out = copy_with(
            &strings(&[&root.join("f.txt")]),
            &dest,
            ConflictPolicy::Overwrite,
            CopyOptions::default(),
        );
        assert!(out.failures.is_empty(), "{:?}", out.failures);
        assert_eq!(read(&dest.join("f.txt")), b"fresh");
        assert_eq!(names(&dest), vec!["f.txt"], "no backup or staging left");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn l1_read_only_source_with_failed_verification_leaves_no_staging() {
        let root = temp("l1-verify");
        write(&root.join("ro.bin"), &vec![9u8; 100_000]);
        let mut perm = fs::metadata(root.join("ro.bin")).unwrap().permissions();
        perm.set_readonly(true);
        fs::set_permissions(root.join("ro.bin"), perm).unwrap();
        let dest = root.join("out");
        fs::create_dir(&dest).unwrap();
        // Corrupt the staged copy (which carries the read-only attribute) before verification.
        hook_staged(&dest, |staged| {
            let mut perm = fs::metadata(staged).unwrap().permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            perm.set_readonly(false);
            fs::set_permissions(staged, perm.clone()).unwrap();
            fs::write(staged, b"tampered").unwrap();
            perm.set_readonly(true);
            fs::set_permissions(staged, perm).unwrap();
        });
        let out = copy_with(
            &strings(&[&root.join("ro.bin")]),
            &dest,
            ConflictPolicy::Ask,
            CopyOptions {
                verify_after_copy: true,
                ..CopyOptions::default()
            },
        );
        unhook_staged(&dest);
        assert_eq!(out.failures.len(), 1, "{:?}", out.failures);
        assert!(out.failures[0].message.contains("verification"));
        assert!(names(&dest).is_empty(), "{:?}", names(&dest));
        let mut perm = fs::metadata(root.join("ro.bin")).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perm.set_readonly(false);
        fs::set_permissions(root.join("ro.bin"), perm).unwrap();
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(windows)]
    #[test]
    fn l2_a_source_that_changes_during_the_shared_fallback_copy_fails_the_item() {
        use std::io::Write as _;
        use std::os::windows::fs::OpenOptionsExt;
        let root = temp("l2");
        write(&root.join("live.log"), &vec![1u8; 50_000]);
        let writer = Arc::new(Mutex::new(
            fs::File::options()
                .append(true)
                .share_mode(7)
                .open(root.join("live.log"))
                .unwrap(),
        ));
        let dest = root.join("out");
        fs::create_dir(&dest).unwrap();
        let appended = std::sync::atomic::AtomicBool::new(false);
        hook_chunks(&dest, {
            let writer = writer.clone();
            move |_| {
                if !appended.swap(true, Ordering::SeqCst) {
                    let mut w = writer.lock().unwrap();
                    w.write_all(b"appended while copying").unwrap();
                    w.sync_all().unwrap();
                }
            }
        });
        let out = copy_with(
            &strings(&[&root.join("live.log")]),
            &dest,
            ConflictPolicy::Ask,
            CopyOptions {
                stream_only: true,
                ..CopyOptions::default()
            },
        );
        unhook_chunks(&dest);
        assert_eq!(out.failures.len(), 1, "{:?}", out.failures);
        assert!(out.failures[0].message.contains("berubah"));
        assert!(out.completed.is_empty());
        assert!(names(&dest).is_empty(), "{:?}", names(&dest));
        drop(writer);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn cancel_during_an_overwrite_on_the_production_copyfile2_path_keeps_the_old_target() {
        let root = temp("cancel-native-overwrite");
        write(&root.join("big.bin"), &vec![7u8; 64 * 1024 * 1024]);
        let dest = root.join("out");
        write(&dest.join("big.bin"), b"OLD TARGET");
        let token = CancelToken::default();
        let tok = token.clone();
        hook_chunks(&dest, move |_| tok.cancel());
        // Default options: the native CopyFile2 path (its progress callback calls the hook).
        let out = run_copy_with_options(
            &strings(&[&root.join("big.bin")]),
            &dest.to_string_lossy(),
            ConflictPolicy::Overwrite,
            CopyOptions::default(),
            &token,
            &silent,
        );
        unhook_chunks(&dest);
        assert!(out.cancelled);
        assert_eq!(out.copied_files, 0);
        assert_eq!(read(&dest.join("big.bin")), b"OLD TARGET");
        assert_eq!(names(&dest), vec!["big.bin"], "no staging or backup left");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn upper_invariant_keys_treat_case_variants_as_one_destination_but_not_sharp_s() {
        assert_eq!(
            claim_key(Path::new("C:\\x\\Report.txt")),
            claim_key(Path::new("c:/X/report.TXT"))
        );
        assert_ne!(
            claim_key(Path::new("C:\\x\\stra\u{df}e")),
            claim_key(Path::new("C:\\x\\STRASSE"))
        );
    }

    #[test]
    fn l3_the_staged_guard_cleans_up_on_drop_unless_disarmed() {
        let root = temp("l3");
        let (a, b, c) = (root.join("a"), root.join("b"), root.join("c"));
        for p in [&a, &b, &c] {
            write(p, b"x");
        }
        drop(StagedGuard(Some(a.clone())));
        assert!(!a.exists());
        let mut guard = StagedGuard(Some(b.clone()));
        guard.disarm();
        drop(guard);
        assert!(b.exists());
        let mut perm = fs::metadata(&c).unwrap().permissions();
        perm.set_readonly(true);
        fs::set_permissions(&c, perm).unwrap();
        drop(StagedGuard(Some(c.clone())));
        assert!(!c.exists(), "a read-only staged file is removed too");
        // A panic unwinding through the guard still cleans up.
        let d = root.join("d");
        write(&d, b"x");
        let d2 = d.clone();
        let _ = std::panic::catch_unwind(move || {
            let _guard = StagedGuard(Some(d2));
            panic!("worker bug");
        });
        assert!(!d.exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn fer563_copy_onto_itself_via_case_alias_or_hardlink_is_refused_and_keeps_the_data() {
        let root = temp("fer563-copy");
        let file = root.join("MixedCase").join("a.txt");
        write(&file, b"ONLY-COPY");
        let alias = root.join("mixedcase");
        let hl_dir = root.join("other");
        fs::create_dir(&hl_dir).unwrap();
        fs::hard_link(&file, hl_dir.join("a.txt")).unwrap();
        for target in [&alias, &hl_dir] {
            let out = run_copy(
                &[file.to_string_lossy().into_owned()],
                &target.to_string_lossy(),
                ConflictPolicy::Overwrite,
                &no_cancel(),
                &silent,
            );
            assert_eq!(out.failures.len(), 1, "{out:?}");
            assert_eq!(out.copied_files, 0, "{out:?}");
            assert_eq!(fs::read(&file).unwrap(), b"ONLY-COPY");
        }
        // Duplicating next to itself with auto-rename must keep working.
        let out = run_copy(
            &[file.to_string_lossy().into_owned()],
            &file.parent().unwrap().to_string_lossy(),
            ConflictPolicy::RenameAuto,
            &no_cancel(),
            &silent,
        );
        assert!(out.failures.is_empty(), "{out:?}");
        assert_eq!(
            fs::read(file.with_file_name("a (2).txt")).unwrap(),
            b"ONLY-COPY"
        );
    }
}
