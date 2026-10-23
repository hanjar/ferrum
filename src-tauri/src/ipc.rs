//! Tauri command surface — the ONLY layer that touches Tauri types.
//!
//! Command contract (kept in sync with `src/lib/ipc/types.ts` + docs/ipc.md):
//!
//! - `list_dir(path: string) -> FileEntry[] | FsError`
//!   Runs on a blocking thread so large directories never stall the UI.
//!   Returns default-sorted entries (folders first, name asc).
//! - `list_drives() -> DriveInfo[]` (tree sidebar roots; blocking thread)
//! - `enqueue_dummy_op(duration_ms) -> OperationId` (task 09 queue harness)
//! - `cancel_operation(id) -> boolean`
//! - `list_operations() -> OperationProgress[]`
//!
//! Progress events flow to the frontend as `operations://progress`
//! (≤10/s per operation). Tasks 10–12 replace the dummy with real engines.
//!
//! - `branch_start(id, root, maxEntries?)` (F2-23 / FER-128): streaming Branch View
//!   walk; batches as `branch://progress` events (~10/s), one final `done` event
//!   (summary or error); `branch_cancel(id) -> bool`. No giant sync response.
//! - `disk_usage_start(root, options) -> scanId` (DSK-02): progressive scan
//!   events as `disk_usage://progress` (≤10/s per scan, unique scanId per
//!   start), ending with exactly one `done` event; `disk_usage_cancel(scanId)
//!   -> bool`; `disk_usage_snapshot(scanId) -> DiskUsageSnapshotSummary?`
//!   (bounded: root aggregates + stats, FER-479). Cancelled and partial
//!   scans are never labelled complete.
//! - `disk_usage_query_children(scanId, nodeId, offset, limit, sort?, filter?)`
//!   and `disk_usage_top_files(scanId, nodeId, limit, filter?)` (DSK-03):
//!   largest-first paginated children and recursive top-N files, read PURELY
//!   from the stored snapshot (no re-walk, totals never inflate). Responses
//!   carry `version`/`scannedAtMs` so the UI preserves selection across
//!   progress; unknown/evicted `scanId` or out-of-tree `nodeId` → structured
//!   `NotFound` (the UI answers with a rescan). Percentages are `null` on
//!   partial coverage / zero denominator / unknown allocated — UI renders
//!   "—", never 0%.

use crate::domain::{FileEntry, FsError, OperationId, OperationProgress, validate_path};
use crate::fs_service::{self, DriveInfo};
use crate::ops::OperationQueue;
use ferrum_activity_store::FsChangeKind;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tauri::async_runtime::spawn_blocking;
use tauri::{AppHandle, Emitter, Manager, State};

/// FER-512 P3-3: paths of MUTATING commands pass the same string gate as content reads
/// (`path_guard::check_str`): device namespaces (`\\.\`, `\\?\`), alternate data streams
/// (`C:\x.txt:ads`), control characters and relative paths are refused uniformly. Plain UNC
/// (`\\server\share\...`) and well-formed drive paths stay allowed — Ferrum browses shares.
///
/// FER-515 P3-2: on top of that, no component may be a Windows reserved device name
/// (`C:\x\CON`, `NUL.txt`, `C:\COM1\a`): such a path addresses a device, not a file. This lives
/// here (mutations) and not in `check_str`, so the read paths keep their behaviour.
fn check_mutation_path(path: &str) -> Result<(), FsError> {
    let normalised = crate::path_guard::check_str(path)?;
    if crate::path_guard::has_reserved_device_component(&normalised) {
        return Err(FsError::new(
            crate::domain::ErrorCode::InvalidPath,
            "Path contains a reserved Windows device name",
        ));
    }
    check_not_local_unc(&normalised)
}

/// FER-514 rework (P3-A): a mutation may not reach this machine through a UNC spelling
/// (`\\localhost\C$\...`, `\\127.0.0.1\...`, the computer's own name, any administrative
/// share): that would walk around the protected-folder check and the local-drive rules.
fn check_not_local_unc(normalised: &str) -> Result<(), FsError> {
    if crate::path_guard::is_local_machine_unc(normalised) {
        return Err(FsError::new(
            crate::domain::ErrorCode::InvalidPath,
            "Network path to this computer or an administrative share is not allowed here",
        ));
    }
    Ok(())
}

/// FER-515 rework (P3-F): an EXISTING entry that is renamed, deleted or changed. Same gates as
/// [`check_write_source`], except that a reserved device name is allowed when the entry really is
/// a plain file or folder of that name (made by WSL/bash: `nul`, `aux.c`). Such an entry is
/// reached through its `\\?\` form, which does no device-name parsing, and that form is returned
/// to be used for the operation. A reserved name that does not exist as a plain file or folder
/// (the device itself) is refused.
fn prepare_source(path: &str) -> Result<String, FsError> {
    let normalised = crate::path_guard::check_str(path)?;
    check_not_local_unc(&normalised)?;
    crate::protected_dirs::check_write_source(&protected_probe(&normalised))?;
    if !crate::path_guard::has_reserved_device_component(&normalised) {
        return Ok(path.to_string());
    }
    let verbatim = crate::path_guard::verbatim_path(&normalised);
    match std::fs::symlink_metadata(&verbatim) {
        Ok(meta) if meta.is_file() || meta.is_dir() => Ok(verbatim),
        _ => Err(FsError::new(
            crate::domain::ErrorCode::InvalidPath,
            "Path contains a reserved Windows device name",
        )),
    }
}

/// The spelling handed to `protected_dirs` (which canonicalises, i.e. OPENS the path): a path with
/// a reserved device component goes in its `\?\` form so that checking it never opens a device.
fn protected_probe(normalised: &str) -> String {
    if crate::path_guard::has_reserved_device_component(normalised) {
        crate::path_guard::verbatim_path(normalised)
    } else {
        normalised.to_string()
    }
}

fn prepare_sources<'a>(
    paths: impl IntoIterator<Item = &'a String>,
) -> Result<Vec<String>, FsError> {
    paths.into_iter().map(|p| prepare_source(p)).collect()
}

/// FER-514 rework (point 1) + FER-559 (N5): a SOURCE that is only read (copy / plan / move). The
/// string gate (no pipes, device namespaces, ADS, `\?\GLOBALROOT`). A path with a reserved device
/// name component (`C:\x\nul`, `COM1`) is read through its `\?\` form when it really is a plain
/// file or folder (made by WSL/bash); otherwise it is the device (or missing) and is refused, so the
/// engine never opens a device. Returns the path to hand to the engine.
fn prepare_read_source(path: &str) -> Result<String, FsError> {
    let normalised = crate::path_guard::check_str(path)?;
    if !crate::path_guard::has_reserved_device_component(&normalised) {
        return Ok(path.to_string());
    }
    let verbatim = crate::path_guard::verbatim_path(&normalised);
    match std::fs::symlink_metadata(&verbatim) {
        Ok(meta) if meta.is_file() || meta.is_dir() => Ok(verbatim),
        _ => Err(FsError::new(
            crate::domain::ErrorCode::InvalidPath,
            "Source path contains a reserved Windows device name and is not a plain file or folder",
        )),
    }
}

/// Sources and target folder for copy / move. When any source needs its `\?\` form, the target
/// folder is switched to that form as well: the engines derive destination names from source
/// names (`nul` -> `<target>\nul`), and only the `\?\` spelling makes that a file instead of the
/// device. The target itself already passed the full mutation gate (no reserved component).
fn prepare_copy_io(sources: &[String], target_dir: &str) -> Result<(Vec<String>, String), FsError> {
    let mut prepared = sources
        .iter()
        .map(|s| prepare_read_source(s))
        .collect::<Result<Vec<_>, _>>()?;
    if !prepared.iter().any(|p| p.starts_with(r"\\?\")) {
        return Ok((prepared, target_dir.to_string()));
    }
    // One spelling for the whole operation: with a verbatim target and a plain source the
    // engines' "same path" / "into itself" comparisons would not match (a move onto itself with
    // Overwrite would then delete the source). The gates already ran on the plain spellings.
    for p in &mut prepared {
        if !p.starts_with(r"\\?\") {
            *p = crate::path_guard::verbatim_path(&crate::path_guard::check_str(p)?);
        }
    }
    let normalised = crate::path_guard::check_str(target_dir)?;
    Ok((prepared, crate::path_guard::verbatim_path(&normalised)))
}

/// FER-514 P3-7: a mutation path that is a DESTINATION or a newly created entry: the string gate
/// plus "not inside the app's own state folders" (`protected_dirs`).
fn check_write_target(path: &str) -> Result<(), FsError> {
    check_mutation_path(path)?;
    // FER-559: the file manager writes nothing into a Windows Startup folder.
    crate::startup_guard::check_into(path)?;
    crate::protected_dirs::check_write_target(path)
}

/// FER-514 P3-7: a mutation path that is renamed, moved, deleted or changed in place: the string
/// gate plus "neither inside nor containing the app's own state folders".
fn check_write_source(path: &str) -> Result<(), FsError> {
    check_mutation_path(path)?;
    crate::protected_dirs::check_write_source(path)
}

fn check_write_sources<'a>(paths: impl IntoIterator<Item = &'a String>) -> Result<(), FsError> {
    for p in paths {
        check_write_source(p)?;
    }
    Ok(())
}

fn check_write_targets<'a>(paths: impl IntoIterator<Item = &'a String>) -> Result<(), FsError> {
    for p in paths {
        check_write_target(p)?;
    }
    Ok(())
}

/// FER-515 P3-3: name of a NEW entry (create / rename target). `mutations::validate_name`
/// stays permissive; this adds what is never a legal Windows name: `:` (alternate data stream,
/// `x.txt:ads`) and reserved device names.
fn check_new_name(name: &str) -> Result<(), FsError> {
    crate::mutations::validate_name(name)?;
    if name.contains(':') {
        return Err(FsError::new(
            crate::domain::ErrorCode::InvalidPath,
            "Name must not contain ':'",
        ));
    }
    if crate::path_guard::is_reserved_device_name(name) {
        return Err(FsError::new(
            crate::domain::ErrorCode::InvalidPath,
            "Name is a reserved Windows device name",
        ));
    }
    Ok(())
}

/// Copy / move / archive destinations: the full mutation gate (string gate, reserved names, local
/// UNC) plus the protected folders.
fn check_protected_target(path: &str) -> Result<(), FsError> {
    check_write_target(path)
}

#[tauri::command]
pub async fn list_dir(path: String) -> Result<Vec<FileEntry>, FsError> {
    validate_path(&path)?;
    // Blocking filesystem IO must leave the async runtime's workers free.
    spawn_blocking(move || fs_service::list_dir(&path))
        .await
        .map_err(|e| FsError::new(crate::domain::ErrorCode::Io, e.to_string()))?
}

/// Logical drives for the tree sidebar. Enumeration never fails by design —
/// on the odd chance the blocking task itself is lost, return nothing.
#[tauri::command]
pub async fn list_drives() -> Vec<DriveInfo> {
    spawn_blocking(fs_service::list_drives)
        .await
        .unwrap_or_default()
}

/// Present-drive-letter bitmask (bit 0 = A:). Instant, no media I/O: the
/// sidebar polls this to notice drives added or removed (FER-496).
#[tauri::command]
pub fn drive_letters_mask() -> u32 {
    fs_service::drive_letters_mask()
}

/// FER-456 address bar: subfolder suggestions for what the user typed.
/// Bounded (max 4 workers, 300 ms deadline, slow-folder cache) so a slow
/// share never freezes the bar; unknown UNC hosts are never contacted here.
/// `known_hosts` = UNC paths/hosts from Recent, Hotlist and open panes.
#[tauri::command]
pub async fn path_suggest(
    input: String,
    base: String,
    limit: Option<usize>,
    known_hosts: Option<Vec<String>>,
) -> Result<crate::path_suggest::SuggestResult, FsError> {
    use crate::path_suggest as ps;
    spawn_blocking(move || {
        let known = known_hosts.unwrap_or_default();
        ps::suggest_bounded(&input, &base, limit.unwrap_or(ps::MAX_LIMIT), &known)
    })
    .await
    .map_err(|e| FsError::new(crate::domain::ErrorCode::Io, e.to_string()))?
}

/// FER-456: classify what the user typed (dir / archive / file / missing).
/// Enter is explicit, so an UNC host is contacted here (and remembered).
#[tauri::command]
pub async fn path_resolve(
    input: String,
    base: String,
) -> Result<crate::path_suggest::Resolved, FsError> {
    spawn_blocking(move || crate::path_suggest::resolve_bounded(&input, &base))
        .await
        .map_err(|e| FsError::new(crate::domain::ErrorCode::Io, e.to_string()))?
}

/// FER-512 Info-1: ticks of the queue harness job, capped so the webview can
/// never queue an unbounded sleep (at most 10 s).
fn dummy_ticks(duration_ms: u64) -> u64 {
    duration_ms.min(10_000) / 50
}

#[tauri::command]
pub fn enqueue_dummy_op(
    duration_ms: u64,
    queue: State<'_, Arc<OperationQueue>>,
) -> Result<OperationId, FsError> {
    let ticks = dummy_ticks(duration_ms);
    // F2-26 BTM: retryable so the panel's retry path can be exercised end to
    // end without touching real files.
    let make_body = move || {
        Box::new(
            move |cancel: &crate::ops::CancelToken,
                  report: &dyn Fn(crate::domain::OperationProgress)| {
                for i in 0..ticks.max(1) {
                    if cancel.is_cancelled() {
                        return Err(FsError::new(
                            crate::domain::ErrorCode::Cancelled,
                            "cancelled",
                        ));
                    }
                    report(OperationProgress {
                        operation_id: String::new(),
                        kind: crate::domain::OperationKind::Copy,
                        state: crate::domain::OperationState::Running,
                        processed_bytes: i,
                        total_bytes: Some(ticks),
                        processed_files: i,
                        total_files: Some(ticks),
                        current_path: Some(format!("C:\\dummy\\chunk-{i:04}.bin")),
                        rate_bytes_per_sec: Some(8_000_000),
                        error: None,
                        failures: Vec::new(),
                        queued_at_ms: None,
                        finished_at_ms: None,
                    });
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Ok(())
            },
        ) as crate::ops::JobBody
    };
    Ok(queue.enqueue_retryable(
        crate::domain::OperationKind::Copy,
        make_body(),
        Arc::new(make_body),
    ))
}

#[tauri::command]
pub fn cancel_operation(id: String, queue: State<'_, Arc<OperationQueue>>) -> bool {
    queue.cancel(&id)
}

/// FER-471: live copy controls of the jobs that are copying right now.
#[tauri::command]
pub fn copy_control_snapshot() -> Vec<crate::copy_pool::CopyControlState> {
    crate::copy_pool::running_states()
}

/// FER-471: change threads / auto mode / priority of a RUNNING copy job. Only
/// performance knobs: no file is touched. Unknown or finished ids are rejected.
#[tauri::command]
pub fn copy_control_adjust(
    operation_id: String,
    change: crate::copy_pool::Adjust,
) -> Result<crate::copy_pool::CopyControlState, FsError> {
    match crate::copy_pool::get(&operation_id) {
        Some(control) if control.is_running() => Ok(control.adjust(change)),
        _ => Err(FsError::new(
            crate::domain::ErrorCode::NotFound,
            "No running copy job with this id",
        )),
    }
}

#[tauri::command]
pub fn list_operations(queue: State<'_, Arc<OperationQueue>>) -> Vec<OperationProgress> {
    queue.snapshot()
}

#[tauri::command]
pub fn clear_finished_operations(queue: State<'_, Arc<OperationQueue>>) {
    queue.clear_finished();
}

#[tauri::command]
pub fn create_entry(
    kind: String,
    parent: String,
    name: String,
) -> Result<crate::domain::FileEntry, FsError> {
    check_write_target(&parent)?;
    check_new_name(&name)?;
    // A new folder (or file) named like the Startup folder, in its parent, is Startup itself.
    crate::startup_guard::check_rename_to(&Path::new(&parent).join(&name).to_string_lossy())?;
    match kind.as_str() {
        "file" => crate::mutations::create_file(&parent, &name),
        "dir" => crate::mutations::create_dir(&parent, &name),
        other => Err(FsError::new(
            crate::domain::ErrorCode::InvalidPath,
            format!("unknown entry kind {other:?}"),
        )),
    }
}

#[tauri::command]
pub fn rename_entry(
    path: String,
    new_name: String,
    app: AppHandle,
) -> Result<crate::domain::FileEntry, FsError> {
    let path = prepare_source(&path)?;
    check_new_name(&new_name)?;
    if let Some(parent) = Path::new(&path).parent() {
        crate::startup_guard::check_rename_to(&parent.join(&new_name).to_string_lossy())?;
    }
    let entry = crate::mutations::rename_entry(&path, &new_name)?;
    // The UI and the activity store only ever see the plain spelling.
    let path = crate::path_guard::strip_verbatim(&path);
    note_activity_changes(
        &app,
        FsChangeKind::Rename,
        vec![(path, Some(entry.path.clone()))],
    );
    Ok(entry)
}

/// Identity receipts are best-effort and queued: the file operation has
/// already succeeded and never depends on the activity store.
fn note_activity_changes(
    app: &AppHandle,
    kind: FsChangeKind,
    pairs: Vec<(String, Option<String>)>,
) {
    if let Some(service) = app.state::<ActivityHandle>().0.as_ref() {
        // Paths that went through the `\?\` form are recorded in their plain spelling.
        let pairs: Vec<(String, Option<String>)> = pairs
            .into_iter()
            .map(|(a, b)| {
                (
                    crate::path_guard::strip_verbatim(&a),
                    b.map(|b| crate::path_guard::strip_verbatim(&b)),
                )
            })
            .collect();
        service.note_fs_changes(kind, &pairs);
    }
}

/// Files that are fully gone: every requested path except the ones that failed, were never reached
/// by a cancel, or are folders a cancel left partly deleted. Only these are Activity receipts.
fn deleted_receipts(
    paths: &[String],
    report: &crate::mutations::DeleteReport,
    not_processed: &[String],
    partial: &[String],
) -> Vec<(String, Option<String>)> {
    let skip: std::collections::HashSet<&str> = report
        .failed
        .iter()
        .map(|f| f.path.as_str())
        .chain(not_processed.iter().map(String::as_str))
        .chain(partial.iter().map(String::as_str))
        .collect();
    paths
        .iter()
        .filter(|p| !skip.contains(p.as_str()))
        .map(|p| (p.clone(), None))
        .collect()
}

/// Small / non-interactive delete without progress. The file manager's dialogs use
/// [`delete_start`] (FER-585); this stays as the plain contract, but runs off the UI thread so a
/// large tree can no longer freeze the window.
#[tauri::command]
pub async fn delete_entries(
    paths: Vec<String>,
    permanent: bool,
    app: AppHandle,
) -> Result<crate::mutations::DeleteReport, FsError> {
    let paths = prepare_sources(&paths)?;
    spawn_blocking(move || {
        let report = if permanent {
            crate::mutations::delete_permanent(&paths)
        } else {
            crate::mutations::delete_to_trash(&paths)
        };
        // Only items that did not fail are receipts (Recycle Bin counts: the
        // locator is gone either way).
        let deleted = deleted_receipts(&paths, &report, &[], &[]);
        note_activity_changes(&app, FsChangeKind::Delete, deleted);
        report
    })
    .await
    .map_err(blocking_failed)
}

/// Event: throttled progress of a delete job (`DeleteProgress` plus `requestId`).
const DELETE_PROGRESS_EVENT: &str = "delete://progress";
/// Event: the delete job ended (`DeleteDoneEvent`).
const DELETE_DONE_EVENT: &str = "delete://done";

#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct DeleteProgressEvent<'a> {
    request_id: &'a str,
    #[serde(flatten)]
    progress: &'a crate::delete_jobs::DeleteProgress,
}

#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct DeleteDoneEvent<'a> {
    request_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<&'a crate::delete_jobs::DeleteJobResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<&'a FsError>,
}

/// Everything `delete_start` checks before a job exists: the path gates (identical to the old
/// synchronous delete) and a usable, not yet running request id. Nothing is touched on a refusal.
fn gate_delete_job(
    paths: &[String],
    request_id: &str,
) -> Result<(Vec<String>, crate::archive_cancel::Registration), FsError> {
    let prepared = prepare_sources(paths)?;
    if request_id.is_empty() || request_id.len() > 128 {
        return Err(FsError::new(
            crate::domain::ErrorCode::InvalidPath,
            "Invalid request id",
        ));
    }
    let registration = crate::archive_cancel::begin_unique(Some(request_id))
        .map_err(|m| FsError::new(crate::domain::ErrorCode::InvalidPath, m))?;
    Ok((prepared, registration))
}

/// FER-585: start a delete job and return at once. Same gates as ever (`prepare_sources`: string
/// gate, protected folders, reserved names), checked BEFORE anything starts; a refused path fails
/// this call and nothing runs. The job works on a blocking thread, reports `delete://progress`
/// (throttled) and ends with one `delete://done` carrying the [`DeleteJobResult`]. `request_id` is
/// what [`delete_cancel`] targets. The confirmation (and the permanent/Recycle Bin choice) happens
/// in the UI before this is called, unchanged.
#[tauri::command]
pub fn delete_start(
    paths: Vec<String>,
    permanent: bool,
    request_id: String,
    app: AppHandle,
) -> Result<(), FsError> {
    let (paths, registration) = gate_delete_job(&paths, &request_id)?;
    let job_app = app.clone();
    let job_id = request_id.clone();
    let handle = spawn_blocking(move || {
        let cancelled = || registration.token().is_cancelled();
        let mut throttle =
            crate::archive_jobs::Throttle::new(crate::archive_jobs::PROGRESS_INTERVAL);
        let mut last_sent_done = usize::MAX;
        let mut emit = |p: &crate::delete_jobs::DeleteProgress| {
            // Item boundaries always go out (the first and the last in particular); the inside of
            // a big folder is throttled.
            if p.done != last_sent_done || throttle.due() {
                last_sent_done = p.done;
                let _ = job_app.emit(
                    DELETE_PROGRESS_EVENT,
                    DeleteProgressEvent {
                        request_id: &job_id,
                        progress: p,
                    },
                );
            }
        };
        let result = if permanent {
            crate::delete_jobs::run_permanent(&paths, &cancelled, &mut emit)
        } else {
            crate::delete_jobs::run_trash(&paths, &cancelled, &mut emit)
        };
        let receipts = deleted_receipts(
            &paths,
            &result.report,
            &result.not_processed,
            &result.partial,
        );
        note_activity_changes(&job_app, FsChangeKind::Delete, receipts);
        result
    });
    tauri::async_runtime::spawn(async move {
        match handle.await {
            Ok(result) => {
                let _ = app.emit(
                    DELETE_DONE_EVENT,
                    DeleteDoneEvent {
                        request_id: &request_id,
                        result: Some(&result),
                        error: None,
                    },
                );
            }
            Err(e) => {
                let err = blocking_failed(e);
                let _ = app.emit(
                    DELETE_DONE_EVENT,
                    DeleteDoneEvent {
                        request_id: &request_id,
                        result: None,
                        error: Some(&err),
                    },
                );
            }
        }
    });
    Ok(())
}

/// FER-585: ask a running delete job to stop (idempotent; unknown ids are harmless). The job stops
/// at the next item (and, in a permanent delete of a folder, the next entry) and says what is left.
#[tauri::command]
pub fn delete_cancel(request_id: String) {
    crate::archive_cancel::cancel(&request_id);
}

/// One path's answer of [`ai_recycle_check`].
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecycleCheck {
    pub path: String,
    pub ok: bool,
    /// Stable code (see `recycle_gate::reason`), never a raw OS message.
    pub reason: Option<&'static str>,
    /// The REAL recursive size of the item (links are not followed); 0 when it could not be measured.
    pub bytes: u64,
    /// Files and folders in the item, itself included.
    pub entries: u64,
    /// Symlinks / junctions inside the item (an archive skips them).
    pub links: u64,
}

/// What [`ai_tree_census`] counted over a set of paths.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TreeCensus {
    pub bytes: u64,
    /// Members a 7z / ZIP of these paths lists: every file plus every empty folder.
    pub archive_members: u64,
    pub links: u64,
}

/// Upper bound of one AI delete batch.
const MAX_AI_DELETE_ITEMS: usize = 10_000;

fn ai_batch_check(paths: &[String]) -> Result<(), FsError> {
    if paths.is_empty() || paths.len() > MAX_AI_DELETE_ITEMS {
        return Err(FsError::new(
            crate::domain::ErrorCode::InvalidPath,
            "Invalid number of items",
        ));
    }
    Ok(())
}

fn blocking_failed(e: tauri::Error) -> FsError {
    FsError::new(crate::domain::ErrorCode::Io, e.to_string())
}

/// FER-576: read-only. Is the shell guaranteed to move each path into the Recycle Bin (volume, bin settings,
/// quota against the real size)? Used when the AI delete card is built; the delete asks again.
#[tauri::command]
pub async fn ai_recycle_check(paths: Vec<String>) -> Result<Vec<RecycleCheck>, FsError> {
    ai_batch_check(&paths)?;
    spawn_blocking(move || {
        crate::recycle_gate::assess(&paths, &crate::recycle_gate::SystemRecycleEnv)
            .into_iter()
            .map(|verdict| RecycleCheck {
                path: verdict.path,
                ok: verdict.ok,
                reason: verdict.reason,
                bytes: verdict.census.bytes,
                entries: verdict.census.entries(),
                links: verdict.census.links,
            })
            .collect()
    })
    .await
    .map_err(blocking_failed)
}

/// FER-514 gate for every IPC that reads a path it was handed: the string gate (no device namespaces, pipes,
/// streams, control characters), the reserved-name handling of `prepare_read_source`, and (this census only
/// serves local files) a drive-letter path outside the folders the shell manages.
pub(crate) fn census_sources(paths: &[String]) -> Result<Vec<String>, FsError> {
    paths
        .iter()
        .map(|path| {
            let plain = crate::path_guard::strip_verbatim(path);
            let bytes = plain.as_bytes();
            let local = bytes.len() >= 3
                && bytes[0].is_ascii_alphabetic()
                && bytes[1] == b':'
                && matches!(bytes[2], b'\\' | b'/');
            if !local || crate::recycle_gate::in_system_area(&plain) {
                return Err(FsError::new(
                    crate::domain::ErrorCode::InvalidPath,
                    "Only local drive paths can be measured here",
                ));
            }
            prepare_read_source(path)
        })
        .collect()
}

/// FER-576: read-only size and entry counts of a set of paths (no link is followed). The archive step of a
/// chain uses it to know how many entries the archive MUST hold before the original may be deleted.
#[tauri::command]
pub async fn ai_tree_census(paths: Vec<String>) -> Result<TreeCensus, FsError> {
    ai_batch_check(&paths)?;
    let paths = census_sources(&paths)?;
    spawn_blocking(move || {
        let mut total = TreeCensus {
            bytes: 0,
            archive_members: 0,
            links: 0,
        };
        for path in &paths {
            let census = crate::recycle_gate::census_tree(path).map_err(|_| {
                FsError::new(
                    crate::domain::ErrorCode::Io,
                    "An item could not be measured completely",
                )
            })?;
            total.bytes = total.bytes.saturating_add(census.bytes);
            total.archive_members += census.archive_members();
            total.links += census.links;
        }
        Ok(total)
    })
    .await
    .map_err(blocking_failed)?
}

/// FER-576: the ONLY delete the AI can reach, and it goes to the Recycle Bin or it does not happen.
///
/// - No `permanent` argument; `delete_permanent` is never called from here.
/// - The same source gate as the manual Delete (path gate, protected folders, local-UNC).
/// - `recycle_gate::assess` refuses the WHOLE batch, before anything is touched, when any item is not
///   guaranteed to be recycled: not a fixed local volume, a mount point or redirect, a bin that is
///   disabled/unreadable ("Don't move files to the Recycle Bin", size 0, policy), an item (or the batch)
///   that does not fit half of the volume's bin quota, a link, or a size that cannot be measured.
/// - Items go one at a time; each is checked through a handle opened BEFORE the shell call to be inside
///   `$Recycle.Bin` afterwards. An item that is not is reported in `bypassed` (it was deleted for good)
///   and the batch stops there; it is never reported as a success.
#[tauri::command]
pub async fn ai_delete_to_recycle(
    paths: Vec<String>,
    app: AppHandle,
) -> Result<crate::recycle_gate::RecycleReport, FsError> {
    ai_batch_check(&paths)?;
    let prepared = prepare_sources(&paths)?;
    spawn_blocking(move || {
        let verdicts =
            crate::recycle_gate::assess(&prepared, &crate::recycle_gate::SystemRecycleEnv);
        if crate::recycle_gate::first_refusal(&verdicts).is_some() {
            return Err(FsError::new(
                crate::domain::ErrorCode::Unsupported,
                "At least one item is not guaranteed to go to the Recycle Bin; nothing was deleted",
            ));
        }
        let report =
            crate::recycle_gate::delete_verified(&prepared, &crate::recycle_gate::SystemRecycleIo);
        // Items that left their place (recycled, or for good) are receipts; the failed ones did not.
        let failed: std::collections::HashSet<&str> =
            report.failed.iter().map(|f| f.path.as_str()).collect();
        let gone: Vec<(String, Option<String>)> = prepared
            .iter()
            .take(prepared.len() - report.skipped)
            .filter(|p| !failed.contains(p.as_str()))
            .map(|p| (p.clone(), None))
            .collect();
        note_activity_changes(&app, FsChangeKind::Delete, gone);
        Ok(report)
    })
    .await
    .map_err(blocking_failed)?
}

fn parse_policy(s: &str) -> Result<crate::copy_engine::ConflictPolicy, FsError> {
    match s {
        "ask" => Ok(crate::copy_engine::ConflictPolicy::Ask),
        "skip" => Ok(crate::copy_engine::ConflictPolicy::Skip),
        "overwrite" => Ok(crate::copy_engine::ConflictPolicy::Overwrite),
        "overwriteOlder" => Ok(crate::copy_engine::ConflictPolicy::OverwriteOlder),
        "rename" => Ok(crate::copy_engine::ConflictPolicy::RenameAuto),
        other => Err(FsError::new(
            crate::domain::ErrorCode::InvalidPath,
            format!("unknown conflict policy {other:?}"),
        )),
    }
}

#[tauri::command]
pub fn plan_copy(
    sources: Vec<String>,
    target_dir: String,
) -> Result<crate::copy_engine::CopyPlan, FsError> {
    check_write_target(&target_dir)?;
    let (sources, target_dir) = prepare_copy_io(&sources, &target_dir)?;
    crate::copy_engine::plan_copy(&sources, &target_dir)
}

#[tauri::command]
pub fn enqueue_copy(
    sources: Vec<String>,
    target_dir: String,
    policy: String,
    app: AppHandle,
    queue: State<'_, Arc<OperationQueue>>,
) -> Result<OperationId, FsError> {
    check_protected_target(&target_dir)?;
    let (sources, target_dir) = prepare_copy_io(&sources, &target_dir)?;
    let policy = parse_policy(&policy)?;
    let activity_app = app.clone();
    let prefs = load_prefs(app);
    let options = crate::copy_engine::CopyOptions {
        buffer_size: (prefs.copy_buffer_kib as usize).saturating_mul(1024),
        verify_after_copy: prefs.verify_after_copy,
        preserve_timestamps: prefs.preserve_timestamps,
        preserve_permissions: prefs.preserve_permissions,
        threads: crate::copy_pool::ThreadSetting::from_pref(prefs.copy_threads),
        priority: crate::copy_pool::Priority::parse(&prefs.copy_priority)
            .unwrap_or(crate::copy_pool::Priority::Normal),
        stream_only: false,
        durable: prefs.copy_safe_write,
    };
    // F2-26 BTM: retries reuse the same preference snapshot as original job.
    let make_body = move || {
        let sources = sources.clone();
        let target_dir = target_dir.clone();
        let app = activity_app.clone();
        Box::new(
            move |cancel: &crate::ops::CancelToken,
                  report: &dyn Fn(crate::domain::OperationProgress)| {
                let outcome = crate::copy_engine::run_copy_with_options(
                    &sources,
                    &target_dir,
                    policy,
                    options,
                    cancel,
                    &|p| report(p),
                );
                // Receipts for items that fully succeeded, even in a partial or
                // cancelled batch (what happened on disk is what we record).
                note_activity_changes(
                    &app,
                    FsChangeKind::Copy,
                    outcome
                        .completed
                        .iter()
                        .map(|(from, to)| (from.clone(), Some(to.clone())))
                        .collect(),
                );
                if outcome.cancelled {
                    return Err(FsError::new(
                        crate::domain::ErrorCode::Cancelled,
                        "cancelled",
                    ));
                }
                if !outcome.failures.is_empty() {
                    let summary = format!(
                        "{} of {} item(s) failed: {}",
                        outcome.failures.len(),
                        outcome.failures.len() + outcome.copied_files as usize,
                        outcome.failures[0].message
                    );
                    // Per-file detail for the BTM (F2-26): the final progress
                    // carries every failed path so the history stays traceable.
                    report(OperationProgress {
                        operation_id: String::new(),
                        kind: crate::domain::OperationKind::Copy,
                        state: crate::domain::OperationState::Running,
                        processed_bytes: 0,
                        total_bytes: None,
                        processed_files: outcome.copied_files,
                        total_files: None,
                        current_path: None,
                        rate_bytes_per_sec: None,
                        error: Some(summary.clone()),
                        failures: outcome
                            .failures
                            .iter()
                            .map(|f| crate::domain::OperationFailure {
                                path: f.source.clone(),
                                message: f.message.clone(),
                            })
                            .collect(),
                        queued_at_ms: None,
                        finished_at_ms: None,
                    });
                    return Err(FsError::new(crate::domain::ErrorCode::Io, summary));
                }
                Ok(())
            },
        ) as crate::ops::JobBody
    };
    Ok(queue.enqueue_retryable(
        crate::domain::OperationKind::Copy,
        make_body(),
        Arc::new(make_body),
    ))
}

#[tauri::command]
pub fn enqueue_move(
    sources: Vec<String>,
    target_dir: String,
    policy: String,
    app: AppHandle,
    queue: State<'_, Arc<OperationQueue>>,
) -> Result<OperationId, FsError> {
    check_protected_target(&target_dir)?;
    for source in &sources {
        let normalised = crate::path_guard::check_str(source)?;
        check_not_local_unc(&normalised)?;
        crate::protected_dirs::check_write_source(&protected_probe(&normalised))?;
    }
    let (sources, target_dir) = prepare_copy_io(&sources, &target_dir)?;
    let policy = parse_policy(&policy)?;
    remember_copy_defaults(&load_prefs(app.clone()));
    let make_body = move || {
        let sources = sources.clone();
        let target_dir = target_dir.clone();
        let app = app.clone();
        Box::new(
            move |cancel: &crate::ops::CancelToken,
                  report: &dyn Fn(crate::domain::OperationProgress)| {
                let outcome = crate::move_engine::run_move(
                    &sources,
                    &target_dir,
                    policy,
                    cancel,
                    &|p| report(p),
                    false,
                );
                note_activity_changes(
                    &app,
                    FsChangeKind::Move,
                    outcome
                        .completed
                        .iter()
                        .map(|(from, to)| (from.clone(), Some(to.clone())))
                        .collect(),
                );
                if outcome.cancelled {
                    return Err(FsError::new(
                        crate::domain::ErrorCode::Cancelled,
                        "cancelled",
                    ));
                }
                if !outcome.failures.is_empty() {
                    let summary = format!(
                        "{} item(s) failed: {}",
                        outcome.failures.len(),
                        outcome.failures[0].message
                    );
                    report(OperationProgress {
                        operation_id: String::new(),
                        kind: crate::domain::OperationKind::Move,
                        state: crate::domain::OperationState::Running,
                        processed_bytes: 0,
                        total_bytes: None,
                        processed_files: outcome.moved,
                        total_files: None,
                        current_path: None,
                        rate_bytes_per_sec: None,
                        error: Some(summary.clone()),
                        failures: outcome
                            .failures
                            .iter()
                            .map(|f| crate::domain::OperationFailure {
                                path: f.source.clone(),
                                message: f.message.clone(),
                            })
                            .collect(),
                        queued_at_ms: None,
                        finished_at_ms: None,
                    });
                    return Err(FsError::new(crate::domain::ErrorCode::Io, summary));
                }
                Ok(())
            },
        ) as crate::ops::JobBody
    };
    Ok(queue.enqueue_retryable(
        crate::domain::OperationKind::Move,
        make_body(),
        Arc::new(make_body),
    ))
}

// ── FER-572: file clipboard (copy / cut / paste with Windows Explorer) ───────────────────

/// A source path taken from (or put on) the shell clipboard. The list of a paste comes from
/// ANOTHER process, so it gets the same gates as a manual copy / move source: the string gate
/// (no relative paths, device namespaces, ADS, control characters incl. NUL), no reserved device
/// names, no local-machine UNC spelling, and for a cut the protected app folders. The entry must
/// exist. The normalised spelling is returned.
fn check_clipboard_source(path: &str, cut: bool) -> Result<String, FsError> {
    let normalised = crate::path_guard::check_str(path)?;
    if crate::path_guard::has_reserved_device_component(&normalised) {
        return Err(FsError::new(
            crate::domain::ErrorCode::InvalidPath,
            "Path contains a reserved Windows device name",
        ));
    }
    check_not_local_unc(&normalised)?;
    if cut {
        crate::protected_dirs::check_write_source(&normalised)?;
    }
    if std::fs::symlink_metadata(&normalised).is_err() {
        return Err(FsError::new(
            crate::domain::ErrorCode::NotFound,
            "Item not found on disk (it is gone, or lives inside an archive)",
        ));
    }
    Ok(normalised)
}

/// Owner window for clipboard writes (`SetClipboardData` fails without a real owner window).
fn clipboard_owner_hwnd(window: &tauri::WebviewWindow) -> Result<isize, FsError> {
    #[cfg(windows)]
    {
        window
            .hwnd()
            .map(|h| h.0 as isize)
            .map_err(|e| FsError::new(crate::domain::ErrorCode::Io, e.to_string()))
    }
    #[cfg(not(windows))]
    {
        let _ = window;
        Ok(0)
    }
}

/// Result of a paste read: validated sources and whether the clipboard asked for a move.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipboardPaste {
    pub paths: Vec<String>,
    pub cut: bool,
    pub sequence: u32,
}

/// Ctrl+C / Ctrl+X on files: puts them on the Windows clipboard as `CF_HDROP` plus
/// `Preferred DropEffect` (copy or move), so Explorer can paste them. Returns the clipboard
/// sequence number.
#[tauri::command]
pub async fn clipboard_copy_files(
    paths: Vec<String>,
    cut: bool,
    window: tauri::WebviewWindow,
) -> Result<u32, FsError> {
    let hwnd = clipboard_owner_hwnd(&window)?;
    spawn_blocking(move || {
        let checked = paths
            .iter()
            .map(|p| check_clipboard_source(p, cut))
            .collect::<Result<Vec<_>, _>>()?;
        crate::clipboard_files::write_files(hwnd, &checked, cut)
    })
    .await
    .map_err(|e| FsError::new(crate::domain::ErrorCode::Io, e.to_string()))?
}

/// Ctrl+V: reads the file list from the clipboard. `None` = no files there. Every path is
/// validated; the caller then runs the normal copy / move flow (plan, conflict dialog, queue,
/// all guards) into the active pane's folder.
#[tauri::command]
pub async fn clipboard_read_files(
    window: tauri::WebviewWindow,
) -> Result<Option<ClipboardPaste>, FsError> {
    let hwnd = clipboard_owner_hwnd(&window)?;
    spawn_blocking(move || {
        let Some(found) = crate::clipboard_files::read_files(hwnd)? else {
            return Ok(None);
        };
        let paths = found
            .paths
            .iter()
            .map(|p| check_clipboard_source(p, found.cut))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Some(ClipboardPaste {
            paths,
            cut: found.cut,
            sequence: found.sequence,
        }))
    })
    .await
    .map_err(|e| FsError::new(crate::domain::ErrorCode::Io, e.to_string()))?
}

/// After a pasted CUT completed (the move ran in our own queue): empty the clipboard when it
/// still holds that same content, as Explorer does, so the moved files cannot be pasted again.
/// A clipboard that changed in the meantime is left alone.
#[tauri::command]
pub async fn clipboard_finish_cut(
    sequence: u32,
    window: tauri::WebviewWindow,
) -> Result<bool, FsError> {
    let hwnd = clipboard_owner_hwnd(&window)?;
    spawn_blocking(move || crate::clipboard_files::clear_if_unchanged(hwnd, sequence))
        .await
        .map_err(|e| FsError::new(crate::domain::ErrorCode::Io, e.to_string()))?
}

// ── Archive commands (F2-12) ────────────────────────────────────────────

use crate::archive_contract::{
    ArchiveCreateRequest, ArchiveListRequest, ArchiveListResponse, ArchiveProvider,
    ArchiveTestReport, ArchiveTestRequest, Location,
};
use crate::archive_rar::RarProvider;
use crate::archive_zip::ZipProvider;

fn is_zip(path: &str) -> bool {
    path.to_lowercase().ends_with(".zip")
}
fn is_rar(path: &str) -> bool {
    path.to_lowercase().ends_with(".rar")
}
fn is_7z(path: &str) -> bool {
    let lower = path.to_lowercase();
    lower.ends_with(".7z") || lower.ends_with(".7zip")
}
pub(crate) fn archive_provider(
    path: &str,
) -> Result<Box<dyn ArchiveProvider + Send + Sync>, FsError> {
    if is_zip(path) {
        Ok(Box::new(ZipProvider::new()))
    } else if is_rar(path) {
        Ok(Box::new(RarProvider::new()))
    } else if is_7z(path) {
        Ok(Box::new(crate::archive_sevenz::SevenZProvider::new()))
    } else {
        Err(FsError::new(
            crate::domain::ErrorCode::Unsupported,
            format!("Unsupported archive format: {path}"),
        ))
    }
}

fn archive_to_fs_err(archive_path: &str, e: crate::archive_contract::ArchiveError) -> FsError {
    use crate::archive_contract::ArchiveErrorCode as A;
    let mut fs_err = FsError::new(
        match e.code {
            A::UnsupportedFormat | A::MultipartNotFirst | A::MultipartMissing => {
                crate::domain::ErrorCode::Unsupported
            }
            A::PasswordRequired => crate::domain::ErrorCode::Io,
            A::LimitExceeded => crate::domain::ErrorCode::Io,
            // An existing destination (FER-411): callers can tell it apart from an I/O failure.
            A::Conflict => crate::domain::ErrorCode::AlreadyExists,
            // FER-79: a cancelled job is not an I/O failure.
            A::Cancelled => crate::domain::ErrorCode::Cancelled,
            _ => crate::domain::ErrorCode::Io,
        },
        e.message,
    );
    fs_err.path = e.archive_path.or(Some(archive_path.to_owned()));
    // FER-73: the UI branches on this structured code, never on the message text.
    fs_err.archive_code = Some(e.code);
    // FER-55: offending member path reaches UI
    fs_err.member_path = e.member_path;
    fs_err
}

/// Test seam for the error mapping (FER-73): the structured code must reach the wire.
#[cfg(test)]
pub(crate) fn archive_error_for_test(
    archive_path: &str,
    e: crate::archive_contract::ArchiveError,
) -> FsError {
    archive_to_fs_err(archive_path, e)
}

#[tauri::command]
pub async fn archive_list(
    location: Location,
    request_token: u64,
    // FER-504: optional password for header-encrypted RAR/7z. Old callers
    // send none and behave exactly as before. Memory-only on both sides.
    password: Option<String>,
) -> Result<ArchiveListResponse, FsError> {
    let archive_path = match &location {
        Location::Archive { archive_path, .. } => archive_path.clone(),
        Location::Disk { .. } => {
            return Err(FsError::new(
                crate::domain::ErrorCode::InvalidPath,
                "archive_list requires archive location",
            ));
        }
    };
    let request = ArchiveListRequest {
        location,
        request_token,
        password,
    };
    let provider = archive_provider(&archive_path)?;
    spawn_blocking(move || {
        // FER-75: opening a volume that cannot be read alone (root listing only, so browsing
        // inside an opened archive does not re-scan the folder).
        let at_root = matches!(
            &request.location,
            Location::Archive { member_path, .. } if member_path.as_str().is_empty()
        );
        if at_root {
            crate::archive_jobs::guard_multipart(Path::new(&archive_path))
                .map_err(|e| archive_to_fs_err(&archive_path, e))?;
        }
        provider
            .list(&request)
            .map_err(|e| archive_to_fs_err(&archive_path, e))
    })
    .await
    .map_err(|e| FsError::new(crate::domain::ErrorCode::Io, e.to_string()))?
}

#[tauri::command]
pub async fn archive_read(
    location: Location,
    max_bytes: u64,
    password: Option<String>,
    request_id: Option<String>,
) -> Result<crate::archive_contract::ArchiveReadResponse, FsError> {
    let archive_path = match &location {
        Location::Archive { archive_path, .. } => archive_path.clone(),
        _ => {
            return Err(FsError::new(
                crate::domain::ErrorCode::InvalidPath,
                "archive_read requires archive location",
            ));
        }
    };
    let request = crate::archive_contract::ArchiveReadRequest {
        location,
        max_bytes,
        password,
    };
    let provider = archive_provider(&archive_path)?;
    // FER-438: the read runs on a blocking thread so a request the frontend
    // abandoned (`archive_cancel_request`) can stop at the next 64 KiB chunk.
    let (result, bytes) = spawn_blocking(move || {
        let registration = crate::archive_cancel::begin(request_id.as_deref());
        let mut collected = Vec::<u8>::new();
        let result = registration.run(|| {
            crate::archive_contract::check_cancelled()?;
            provider.read(&request, &mut |chunk| {
                crate::archive_contract::check_cancelled()?;
                collected.extend_from_slice(chunk);
                Ok(())
            })
        });
        (result, collected)
    })
    .await
    .map_err(|e| FsError::new(crate::domain::ErrorCode::Io, e.to_string()))?;
    let truncated = result.map_err(|e| archive_to_fs_err(&archive_path, e))?;
    let truncated = truncated || bytes.len() as u64 > max_bytes;
    let cap = std::cmp::min(bytes.len(), max_bytes as usize);
    Ok(crate::archive_contract::ArchiveReadResponse {
        bytes: bytes[..cap].to_vec(),
        truncated,
    })
}

/// Extract with cancel and progress (FER-79). `request_id` is chosen by the client and is what
/// `archive_cancel_request` targets; progress goes out as throttled `archive://progress` events
/// tagged with that id. A cancelled job fails with `FsError { code: cancelled }`.
#[tauri::command]
pub async fn archive_extract(
    request: crate::archive_contract::ArchiveExtractRequest,
    request_id: Option<String>,
    app: tauri::AppHandle,
) -> Result<(), FsError> {
    let archive_path = request.archive_path.clone();
    check_protected_target(&request.destination_path)?;
    let provider = archive_provider(&archive_path)?;
    spawn_blocking(move || {
        crate::archive_jobs::run_extract(
            provider.as_ref(),
            &request,
            request_id.as_deref(),
            &mut |ev| {
                let _ = app.emit(crate::archive_jobs::PROGRESS_EVENT, ev);
            },
        )
    })
    .await
    .map_err(|e| FsError::new(crate::domain::ErrorCode::Io, e.to_string()))?
    .map_err(|e| archive_to_fs_err(&archive_path, e))
}

/// Classify an archive file name against its folder (FER-75): single archive, a non-first volume
/// (with the path of the first one), a first volume with missing volumes, or a complete set.
#[tauri::command]
pub async fn archive_multipart_check(
    path: String,
) -> Result<crate::archive_jobs::MultipartInfo, FsError> {
    // Device / pipe paths are refused before any directory is read.
    let path = crate::path_guard::check_str(&path)?;
    spawn_blocking(move || crate::archive_jobs::inspect_multipart(Path::new(&path)))
        .await
        .map_err(|e| FsError::new(crate::domain::ErrorCode::Io, e.to_string()))
}

// ── FER-368: one-ask extraction preflight (D21) ─────────────────────────
// Read-only advisory pass so the UI can ask ONCE per batch. Nothing here
// writes; the authoritative policy resolution stays inside the F2-03
// extract pipeline. Additive command, same request shape as extract minus
// policy/limits (the backend applies the extract ceilings itself). FER-504:
// it carries the optional password that already unlocked the archive in the
// UI, so header-encrypted RAR/7z can be preflighted too; without it such an
// archive surfaces PasswordRequired. `Debug` is hand-written to mask it.

#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveExtractPreflightRequest {
    pub archive_path: String,
    /// Empty selects every member; otherwise exact paths or subtrees,
    /// identical to `ArchiveExtractRequest.member_paths`.
    #[serde(default)]
    pub member_paths: Vec<crate::archive_contract::MemberPath>,
    pub destination_path: String,
    /// FER-504: the password already unlocked this archive in the UI, so the
    /// preflight can list header-encrypted RAR/7z too. Memory-only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
}

impl std::fmt::Debug for ArchiveExtractPreflightRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArchiveExtractPreflightRequest")
            .field("archive_path", &self.archive_path)
            .field("member_paths", &self.member_paths)
            .field("destination_path", &self.destination_path)
            .field(
                "password",
                &crate::archive_contract::RedactedPassword(&self.password),
            )
            .finish()
    }
}

#[tauri::command]
pub async fn archive_extract_preflight(
    request: ArchiveExtractPreflightRequest,
) -> Result<crate::archive_preflight::PreflightReport, FsError> {
    let archive_path = request.archive_path.clone();
    let job_archive_path = archive_path.clone();
    let provider = archive_provider(&archive_path)?;
    // Same ceilings the extract bridge sends from the TS side.
    let limits = crate::archive_contract::ArchiveLimits {
        max_entries: 100_000,
        max_total_bytes: u64::MAX,
        max_entry_bytes: 512 * 1024 * 1024,
        max_depth: 32,
    };
    spawn_blocking(move || {
        crate::archive_jobs::guard_multipart(Path::new(&job_archive_path))?;
        crate::archive_preflight::preflight_extract(
            provider.as_ref(),
            &job_archive_path,
            &request.member_paths,
            Path::new(&request.destination_path),
            &limits,
            request.password.as_deref(),
        )
    })
    .await
    .map_err(|e| FsError::new(crate::domain::ErrorCode::Io, e.to_string()))?
    .map_err(|e| archive_to_fs_err(&archive_path, e))
}

#[tauri::command]
pub async fn archive_create(
    request: ArchiveCreateRequest,
    request_id: Option<String>,
    app: tauri::AppHandle,
) -> Result<String, FsError> {
    let dest = request.destination_path.clone();
    check_protected_target(&dest)?;
    spawn_blocking(move || {
        crate::archive_jobs::run_create(&request, request_id.as_deref(), &mut |ev| {
            let _ = app.emit(crate::archive_jobs::PROGRESS_EVENT, ev);
        })
    })
    .await
    .map_err(|e| FsError::new(crate::domain::ErrorCode::Io, e.to_string()))?
    .map_err(|e| archive_to_fs_err(&dest, e))
    .map(|written| written.to_string_lossy().into_owned())
}

/// Test Archive for one archive, cancellable and with progress (FER-79). The password in the
/// request lives only for this call: it is not logged, not emitted in events, not stored.
#[tauri::command]
pub async fn archive_test(
    request: ArchiveTestRequest,
    request_id: Option<String>,
    app: tauri::AppHandle,
) -> Result<ArchiveTestReport, FsError> {
    let path = request.archive_path.clone();
    spawn_blocking(move || {
        crate::archive_jobs::run_test(
            &path,
            request.password.as_deref(),
            request_id.as_deref(),
            &mut |ev| {
                let _ = app.emit(crate::archive_jobs::PROGRESS_EVENT, ev);
            },
        )
        .map_err(|e| archive_to_fs_err(&path, e))
    })
    .await
    .map_err(|e| FsError::new(crate::domain::ErrorCode::Io, e.to_string()))?
}

/// Probe Rar.exe tool configured in user preferences (FER-505).
/// FER-511: reads the path AND its confirmed fingerprint from the prefs on disk; the probe only
/// executes the file after the fingerprint matches. Every other state answers `ok: false` with
/// a reason the UI can show as-is.
#[tauri::command]
pub fn archive_rar_tool_probe(
    app: tauri::AppHandle,
) -> Result<crate::archive_rar_tool::RarToolProbeResult, FsError> {
    let dir = crate::profile::config_dir(&app).unwrap_or_else(std::env::temp_dir);
    let prefs = crate::prefs::load_from(&dir);
    let Some(path_str) = prefs.rar_exe_path else {
        return Ok(crate::archive_rar_tool::RarToolProbeResult {
            ok: false,
            version: None,
            reason: Some("Path Rar.exe belum diatur".to_string()),
        });
    };
    match crate::archive_rar_tool::validate_confirmed_rar_exe(
        std::path::Path::new(&path_str),
        prefs.rar_exe_fingerprint.as_ref(),
    ) {
        Ok(tool) => Ok(crate::archive_rar_tool::RarToolProbeResult {
            ok: true,
            version: Some(tool.version().to_string()),
            reason: None,
        }),
        Err(e) => Ok(crate::archive_rar_tool::RarToolProbeResult {
            ok: false,
            version: None,
            reason: Some(e.to_string()),
        }),
    }
}

/// Detect candidate Rar.exe path in standard Program Files locations without saving it (FER-505).
#[tauri::command]
pub fn archive_rar_tool_detect() -> Result<Option<String>, FsError> {
    Ok(crate::archive_rar_tool::detect_rar_exe().map(|p| p.to_string_lossy().into_owned()))
}

/// Result for the UI: `saved: true` once the path (or its removal) is stored.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RarToolSetPathResult {
    pub saved: bool,
}

/// FER-511: the ONLY command that changes the configured Rar.exe path. `None` (or blank) clears
/// the setting without any confirmation. `Some(path)` validates the file WITHOUT executing it,
/// then asks in a NATIVE message box owned by the Ferrum window (the webview can neither draw
/// it nor answer it); only OK stores path + fingerprint. Cancel answers the `Cancelled` error
/// and stores nothing.
#[tauri::command]
pub async fn archive_rar_tool_set_path(
    app: tauri::AppHandle,
    window: tauri::Window,
    path: Option<String>,
) -> Result<RarToolSetPathResult, FsError> {
    let dir = crate::profile::config_dir(&app).unwrap_or_else(std::env::temp_dir);
    let owner = rar_owner_hwnd(&window);
    // FER-514 P3-6: taking the app-wide dialog slot BEFORE any validation or hashing means a
    // webview that floods this command (or races it with another native dialog) gets `Busy`
    // immediately: no stacked dialogs, no repeated hashing. Clearing the setting needs no dialog.
    let slot = if path.as_deref().is_some_and(|p| !p.trim().is_empty()) {
        Some(crate::native_dialog::DialogGuard::try_acquire()?)
    } else {
        None
    };
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let _slot = slot;
        crate::archive_rar_tool::set_rar_exe_path_in(&dir, path, |text| {
            crate::archive_rar_tool::native_confirm_held(owner, text)
        })
    })
    .await
    .map_err(|e| FsError::new(crate::domain::ErrorCode::Io, e.to_string()))?
    .map_err(|e| {
        let code = match &e {
            crate::archive_rar_tool::RarValidationError::Unconfirmed(_)
            | crate::archive_rar_tool::RarValidationError::FingerprintChanged(_) => {
                crate::domain::ErrorCode::Unsupported
            }
            _ => crate::domain::ErrorCode::InvalidPath,
        };
        FsError::new(code, e.to_string())
    })?;
    match outcome {
        crate::archive_rar_tool::SetPathOutcome::Cancelled => Err(FsError::new(
            crate::domain::ErrorCode::Cancelled,
            "Konfirmasi WinRAR dibatalkan; path tidak disimpan",
        )),
        crate::archive_rar_tool::SetPathOutcome::Saved
        | crate::archive_rar_tool::SetPathOutcome::Cleared => {
            Ok(RarToolSetPathResult { saved: true })
        }
    }
}

/// FER-511: read-only status of the configured Rar.exe for the Settings display. Hashes the
/// file but NEVER runs it. `unset` = no path stored; `unconfirmed` = path without a confirmed
/// fingerprint (old prefs — migration, or the file state changed so re-confirmation is due);
/// `ready` = fingerprint matches, the tool may run.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RarToolStatus {
    pub path: Option<String>,
    pub state: &'static str,
}

#[tauri::command]
pub fn archive_rar_tool_status(app: tauri::AppHandle) -> Result<RarToolStatus, FsError> {
    let dir = crate::profile::config_dir(&app).unwrap_or_else(std::env::temp_dir);
    let prefs = crate::prefs::load_from(&dir);
    let Some(path) = prefs.rar_exe_path else {
        return Ok(RarToolStatus {
            path: None,
            state: "unset",
        });
    };
    let state = match prefs.rar_exe_fingerprint.as_ref() {
        None => "unconfirmed",
        Some(fp) => match crate::archive_rar_tool::fingerprint_exe(std::path::Path::new(&path)) {
            Ok(actual) if &actual == fp => "ready",
            _ => "changed",
        },
    };
    Ok(RarToolStatus {
        path: Some(path),
        state,
    })
}

#[cfg(windows)]
fn rar_owner_hwnd(window: &tauri::Window) -> isize {
    window.hwnd().map(|h| h.0 as isize).unwrap_or(0)
}

#[cfg(not(windows))]
fn rar_owner_hwnd(_window: &tauri::Window) -> isize {
    0
}

// ── F2-14: selection save/load files ─────────────────────────────────────

/// FER-512 P3-4: the file's location is `check_str(dir)` + a single basename
/// (`name`, validated like `create_entry`) — the webview can no longer name an
/// arbitrary path to write the selection JSON to.
#[tauri::command]
pub fn save_selection(dir: String, name: String, names: Vec<String>) -> Result<(), FsError> {
    check_protected_target(&dir)?;
    crate::selection_file::save_selection_file(&dir, &name, &names)
}

/// FER-512 P3-4: reads `check_str(dir)` + basename only; the document itself is
/// still validated strictly (version, names = basenames).
#[tauri::command]
pub fn load_selection(
    dir: String,
    name: String,
) -> Result<crate::selection_file::SelectionFile, FsError> {
    crate::selection_file::load_selection_file(&dir, &name)
}

// ── F2-16: streaming file/folder search ──────────────────────────────────

/// Start a search. Returns immediately; results stream to the frontend as
/// `search://progress` events batched by the engine, ending with one final
/// `done` event (stats, or `error` when the search could not run).
#[tauri::command]
pub fn search_start(
    id: String,
    root: String,
    options: crate::search::SearchOptions,
    app: tauri::AppHandle,
) -> Result<(), FsError> {
    use tauri::Emitter;
    if id.trim().is_empty() {
        return Err(FsError::new(
            crate::domain::ErrorCode::InvalidPath,
            "Search id must not be empty",
        ));
    }
    let cancel = Arc::new(AtomicBool::new(false));
    crate::search::active_cancels()
        .lock()
        .expect("search registry")
        .insert(id.clone(), cancel.clone());
    let worker_id = id.clone();
    let emit_id = id;
    tauri::async_runtime::spawn_blocking(move || {
        let stats = crate::search::run_search_auto(&root, &options, &cancel, |batch| {
            let _ = app.emit(
                "search://progress",
                crate::search::SearchEvent {
                    id: emit_id.clone(),
                    entries: batch.to_vec(),
                    done: false,
                    stats: None,
                    error: None,
                },
            );
            true
        });
        crate::search::active_cancels()
            .lock()
            .expect("search registry")
            .remove(&worker_id);
        let final_event = match stats {
            Ok(s) => crate::search::SearchEvent {
                id: emit_id,
                entries: Vec::new(),
                done: true,
                stats: Some(s),
                error: None,
            },
            Err(e) => crate::search::SearchEvent {
                id: emit_id,
                entries: Vec::new(),
                done: true,
                stats: None,
                error: Some(e.to_string()),
            },
        };
        let _ = app.emit("search://progress", final_event);
    });
    Ok(())
}

/// Flag a running search cancelled; the worker stops at the next entry and
/// deregisters itself. Returns false when the id is unknown/already finished.
#[tauri::command]
pub fn search_cancel(id: String) -> bool {
    match crate::search::active_cancels()
        .lock()
        .expect("search registry")
        .remove(&id)
    {
        Some(flag) => {
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
            true
        }
        None => false,
    }
}

// ── DSK-02 (FER-320): progressive disk usage scan ────────────────────────

/// Start a progressive disk-usage scan of ONE local root (v1: a single root
/// per scan). Rust generates a unique `scanId` (timestamp + counter) and
/// returns it immediately; progress streams to the frontend as
/// `disk_usage://progress` events (≤10/s per scan, generation-guarded: a new
/// start never reuses an id, so late events of an old scan are dropped by
/// comparing `scanId`). Every event carries running totals, coverage,
/// bounded per-path errors and the largest top-level children; NO percent
/// and NO ETA (totals are unknown until the walk ends). Exactly one final
/// event sets `done`; a cancelled scan is `done` + `cancelled` + coverage
/// `partial`, never `complete`. The done event is only emitted AFTER the
/// snapshot is stored (FER-407): the frontend queries
/// `disk_usage_snapshot(scanId)` the moment done arrives. Full-tree querying
/// is DSK-03; the finished scan stays addressable via `disk_usage_snapshot`
/// (bounded store).
///
/// FER-460: with `fast: true` the worker first tries the elevated MFT
/// helper (UAC prompt; Ferrum itself never elevates). UAC declined, a
/// non-NTFS/non-local volume, a missing helper or ANY helper/protocol error
/// falls back to this same regular walk under the SAME scanId, and every
/// event of that walk carries a short `notice` saying why. A cancel while
/// the helper runs ends in the usual done + cancelled event.
#[tauri::command]
pub fn disk_usage_start(
    root: String,
    options: crate::disk_usage::ScanOptions,
    fast: Option<bool>,
    source: Option<String>,
    app: tauri::AppHandle,
    window: tauri::Window,
) -> Result<String, FsError> {
    use std::sync::Mutex;
    use tauri::Emitter;
    validate_path(&root)?;
    let scan_id = crate::disk_usage::new_scan_id();
    let cancel = Arc::new(AtomicBool::new(false));
    crate::disk_usage::active_scans()
        .lock()
        .expect("disk usage registry")
        .insert(scan_id.clone(), cancel.clone());
    let worker_id = scan_id.clone();
    let emit_id = scan_id.clone();
    let hwnd = window_hwnd(&window);
    // FER-464: cache of roots outside the index (None = no usable profile dir).
    let cache_dir = crate::disk_usage_cache::cache_dir(&app);
    // FER-407: the final done event is HELD BACK until the snapshot is in the
    // store — the frontend starts querying `disk_usage_snapshot(scanId)` the
    // moment done arrives. Progress events still stream live.
    let final_event: Arc<Mutex<Option<crate::disk_usage::DiskUsageEvent>>> =
        Arc::new(Mutex::new(None));
    tauri::async_runtime::spawn_blocking(move || {
        let mut notice: Option<String> = None;
        // FER-476: "Instan (indeks)" — no UAC; any failure falls back to the
        // regular walk with the reason as a notice.
        if source.as_deref() == Some("index") {
            let done_slot = Arc::clone(&final_event);
            let sink_app = app.clone();
            let index_notice: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
            let note = Arc::clone(&index_notice);
            match run_index_scan(
                &root,
                &options,
                &cancel,
                &emit_id,
                &index_notice,
                move |ev| {
                    let mut ev = ev.clone();
                    ev.engine = crate::disk_usage::ScanEngine::Index;
                    ev.notice = note.lock().expect("notice").clone();
                    if ev.done {
                        *done_slot.lock().expect("done slot") = Some(ev);
                    } else {
                        let _ = sink_app.emit("disk_usage://progress", ev);
                    }
                },
            ) {
                Ok(outcome) => {
                    // Index results are always fresh: never cached (FER-464).
                    finish_scan(&worker_id, Ok(outcome), final_event, move |ev| {
                        let _ = app.emit("disk_usage://progress", ev.clone());
                    });
                    return;
                }
                Err(reason) => {
                    log::info!("disk usage index scan fell back: {reason}");
                    notice = Some(format!(
                        "Indeks tidak dipakai ({reason}); memakai scan biasa."
                    ));
                }
            }
        }
        if fast.unwrap_or(false) {
            let done_slot = Arc::clone(&final_event);
            let sink_app = app.clone();
            match run_fast_scan(&root, &options, &cancel, &emit_id, hwnd, move |ev| {
                let mut ev = ev.clone();
                ev.engine = crate::disk_usage::ScanEngine::NtfsMft;
                if ev.done {
                    *done_slot.lock().expect("done slot") = Some(ev);
                } else {
                    let _ = sink_app.emit("disk_usage://progress", ev);
                }
            }) {
                Ok(outcome) => {
                    let snap = Arc::clone(&outcome.snapshot);
                    finish_scan(&worker_id, Ok(outcome), final_event, move |ev| {
                        let _ = app.emit("disk_usage://progress", ev.clone());
                    });
                    cache_after_scan(
                        cache_dir.as_deref(),
                        &snap,
                        crate::disk_usage::ScanEngine::NtfsMft,
                    );
                    return;
                }
                Err(reason) => {
                    log::info!("disk usage fast scan fell back: {reason}");
                    notice = Some(format!(
                        "Scan cepat tidak dipakai ({reason}); memakai scan biasa."
                    ));
                }
            }
        }
        let done_slot = Arc::clone(&final_event);
        let sink_app = app.clone();
        let walk_notice = notice.clone();
        let result = crate::disk_usage::run_scan_with_throttle(
            &root,
            &options,
            &cancel,
            &emit_id,
            Some(crate::disk_usage::PROGRESS_INTERVAL),
            move |ev| {
                let mut ev = ev.clone();
                ev.notice = walk_notice.clone();
                if ev.done {
                    *done_slot.lock().expect("done slot") = Some(ev);
                } else {
                    let _ = sink_app.emit("disk_usage://progress", ev);
                }
            },
        );
        let snap = result.as_ref().ok().map(|o| Arc::clone(&o.snapshot));
        finish_scan(&worker_id, result, final_event, move |ev| {
            let mut ev = ev.clone();
            if ev.notice.is_none() {
                ev.notice = notice.clone();
            }
            let _ = app.emit("disk_usage://progress", ev);
        });
        if let Some(snap) = snap {
            cache_after_scan(
                cache_dir.as_deref(),
                &snap,
                crate::disk_usage::ScanEngine::Walk,
            );
        }
    });
    Ok(scan_id)
}

/// FER-464: keep a finished scan on disk, but ONLY for roots the index does
/// not cover (non-NTFS, network, service missing / dead / degraded). Runs on
/// the scan worker AFTER the done event, so the UI never waits for it.
/// Failures are logged and otherwise ignored: the cache is an optimization.
fn cache_after_scan(
    dir: Option<&std::path::Path>,
    snap: &crate::disk_usage::DiskUsageSnapshot,
    engine: crate::disk_usage::ScanEngine,
) {
    let Some(dir) = dir else { return };
    if snap.stats.cancelled || crate::indexer_client::index_covers(&snap.root_path) {
        return;
    }
    if let Err(e) = crate::disk_usage_cache::save(dir, snap, engine) {
        log::info!("disk usage cache not saved: {e}");
    }
}

/// Owner window for the UAC prompt (keeps it in front of Ferrum).
fn window_hwnd(window: &tauri::Window) -> isize {
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

/// FER-460 fast path. `Ok` = a finished (or cancelled) outcome whose final
/// event went to `sink`; `Err(reason)` = fall back to the regular walk.
fn run_fast_scan(
    root: &str,
    options: &crate::disk_usage::ScanOptions,
    cancel: &AtomicBool,
    scan_id: &str,
    hwnd: isize,
    mut sink: impl FnMut(&crate::disk_usage::DiskUsageEvent),
) -> Result<crate::disk_usage::ScanOutcome, String> {
    use crate::disk_usage::fast;
    use crate::fast_scan::{FastScanError, eligibility, run_helper};
    let start = std::time::Instant::now();
    let elig = eligibility(root);
    let volume = match (&elig.volume, elig.eligible) {
        (Some(v), true) => v.chars().next().ok_or("volume tidak dikenal")?,
        _ => {
            return Err(elig
                .reason
                .unwrap_or_else(|| "volume tidak didukung".into()));
        }
    };
    // Same root rules as the walker (exists, directory, not a link) BEFORE
    // asking for elevation.
    crate::disk_usage::validate_root_for_scan(root).map_err(|e| e.message)?;
    let mut last: Option<std::time::Instant> = None;
    let helper = run_helper(volume, hwnd, cancel, |p| {
        if last.is_none_or(|t| t.elapsed() >= crate::disk_usage::PROGRESS_INTERVAL) {
            last = Some(std::time::Instant::now());
            sink(&fast::progress_event(
                scan_id,
                p.entries,
                p.records_done,
                p.records_total,
                start,
            ));
        }
    });
    let result = match helper {
        Ok(r) => r,
        Err(FastScanError::Cancelled) => {
            return Ok(fast::cancelled_outcome(root, scan_id, start, sink));
        }
        Err(e) => return Err(e.to_string()),
    };
    if cancel.load(std::sync::atomic::Ordering::Relaxed) {
        return Ok(fast::cancelled_outcome(root, scan_id, start, sink));
    }
    // Read phase complete: tell the UI the tree is being built (no percent
    // for this part — it is short and has no meaningful unit).
    let total = result.done.records_scanned;
    sink(&fast::progress_event(
        scan_id,
        result.entries.len() as u64,
        total,
        total,
        start,
    ));
    let info = fast::MftScanInfo {
        root_frn: result.done.root_frn,
        corrupt_records: result.done.corrupt_records,
    };
    let volume_root = format!("{volume}:\\");
    fast::build_outcome(
        root,
        &volume_root,
        scan_id,
        &result.entries,
        &info,
        options,
        start,
        sink,
    )
    .map_err(|e| e.message)
}

/// FER-476 "Instan (indeks)": the subtree streamed by FerrumIndexer, built
/// into the same snapshot as the walker/MFT paths (`fast::build_outcome`).
/// `Err(reason)` = fall back to the regular walk. Notices: stale index,
/// and (shared mode) profile folders inside the root that are not indexed.
fn run_index_scan(
    root: &str,
    options: &crate::disk_usage::ScanOptions,
    cancel: &AtomicBool,
    scan_id: &str,
    notice: &std::sync::Mutex<Option<String>>,
    mut sink: impl FnMut(&crate::disk_usage::DiskUsageEvent),
) -> Result<crate::disk_usage::ScanOutcome, String> {
    use crate::disk_usage::fast;
    let start = std::time::Instant::now();
    crate::disk_usage::validate_root_for_scan(root).map_err(|e| e.message)?;
    let mut last: Option<std::time::Instant> = None;
    let sub = crate::indexer_client::index_subtree(root, cancel, &mut |n| {
        if last.is_none_or(|t| t.elapsed() >= crate::disk_usage::PROGRESS_INTERVAL) {
            last = Some(std::time::Instant::now());
            sink(&fast::progress_event(scan_id, n, 0, 0, start));
        }
    });
    if cancel.load(std::sync::atomic::Ordering::Relaxed) {
        return Ok(fast::cancelled_outcome(root, scan_id, start, sink));
    }
    let sub = sub?;
    let mut notes = Vec::new();
    if sub.stale {
        notes.push("Indeks sedang dibangun ulang; angka dari indeks sebelumnya.");
    }
    if sub.excluded_inside {
        notes.push(
            "Mode Bersama: isi folder profil pengguna tidak diindeks, ukurannya tidak dihitung.",
        );
    }
    if !notes.is_empty() {
        *notice.lock().expect("notice") = Some(notes.join(" "));
    }
    let info = fast::MftScanInfo {
        root_frn: sub.root_frn,
        corrupt_records: 0,
    };
    fast::build_outcome(
        root,
        root,
        scan_id,
        &sub.entries,
        &info,
        options,
        start,
        sink,
    )
    .map_err(|e| e.message)
}

/// FER-460: may `root` be offered the elevated fast scan? Cheap, no
/// elevation: local fixed/removable NTFS drive letter + helper installed.
#[tauri::command]
pub fn disk_usage_fast_eligibility(root: String) -> crate::fast_scan::FastScanEligibility {
    crate::fast_scan::eligibility(&root)
}

/// Worker tail of [`disk_usage_start`] (FER-407): deregisters the scan, then —
/// on success — stores the finished snapshot BEFORE the final done event is
/// emitted. The frontend's refreshSnapshot runs the moment done arrives; the
/// previous order (emit done from inside the walk sink, store afterwards,
/// with a full deep clone in between) left a NotFound window in which the UI
/// declared a finished scan's results gone ("Hasil scan tidak tersedia lagi").
/// Handing the snapshot over by `Arc` also removes that deep clone entirely.
/// `emit` sees exactly the events the worker emitted before, in the same
/// order — only the done emission moved to after the store.
pub(crate) fn finish_scan(
    scan_id: &str,
    result: Result<crate::disk_usage::ScanOutcome, FsError>,
    final_event: Arc<std::sync::Mutex<Option<crate::disk_usage::DiskUsageEvent>>>,
    mut emit: impl FnMut(&crate::disk_usage::DiskUsageEvent),
) {
    crate::disk_usage::active_scans()
        .lock()
        .expect("disk usage registry")
        .remove(scan_id);
    match result {
        Ok(outcome) => {
            crate::disk_usage::store_snapshot_arc(outcome.snapshot);
            if let Some(ev) = final_event.lock().expect("done slot").take() {
                emit(&ev);
            }
        }
        Err(e) => {
            // The scan could not run at all (invalid/missing root): the
            // frontend still needs its one final event.
            emit(&crate::disk_usage::DiskUsageEvent {
                scan_id: scan_id.to_string(),
                scanned_files: 0,
                scanned_dirs: 0,
                scanned_symlinks: 0,
                known_logical_bytes: 0,
                known_allocated_bytes: None,
                allocated_available_files: 0,
                allocated_unavailable_files: 0,
                coverage: crate::disk_usage::Coverage::Partial,
                errors: Vec::new(),
                total_errors: 0,
                top_children: Vec::new(),
                done: true,
                cancelled: false,
                error: Some(e.to_string()),
                duration_ms: 0,
                engine: crate::disk_usage::ScanEngine::Walk,
                notice: None,
                mft_records_done: None,
                mft_records_total: None,
            });
        }
    }
}

/// Flag a running scan cancelled; the worker stops at the next entry. The
/// final event reports `done` with coverage `partial` and `cancelled: true`
/// — never `complete`. Returns false for unknown/finished ids.
#[tauri::command]
pub fn disk_usage_cancel(scan_id: String) -> bool {
    crate::disk_usage::cancel_scan(&scan_id)
}

/// Summary of a finished scan (identity, final stats, scan-root aggregates),
/// `None` when unknown or evicted (bounded store). FER-479: the response is
/// bounded — the node map and file listings stay in Rust and are read level
/// by level via the DSK-03 commands. Async + blocking pool: a sync command
/// runs on the main thread, which also paints the window.
#[tauri::command]
pub async fn disk_usage_snapshot(
    scan_id: String,
) -> Option<crate::disk_usage::DiskUsageSnapshotSummary> {
    spawn_blocking(move || crate::disk_usage::snapshot_summary(&scan_id))
        .await
        .ok()
        .flatten()
}

// ── FER-464: scan cache for roots outside the index ─────────────────────

/// What `disk_usage_cache_load` returns: the cached scan registered as a
/// normal stored snapshot (queries work unchanged) plus where it came from.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskUsageCacheLoad {
    pub summary: crate::disk_usage::DiskUsageSnapshotSummary,
    pub engine: crate::disk_usage::ScanEngine,
    /// An in-app delete/rename/move touched the root after this scan.
    pub mutated: bool,
}

/// Open the cached scan of `root`, if any (and the root is NOT covered by the
/// index: index roots are always scanned fresh). A damaged or unreadable
/// file is treated as "no cache". Never walks the filesystem.
#[tauri::command]
pub async fn disk_usage_cache_load(
    root: String,
    app: tauri::AppHandle,
) -> Option<DiskUsageCacheLoad> {
    let dir = crate::disk_usage_cache::cache_dir(&app)?;
    spawn_blocking(move || {
        if validate_path(&root).is_err() || crate::indexer_client::index_covers(&root) {
            return None;
        }
        let loaded = crate::disk_usage_cache::load(
            &dir,
            &root,
            crate::disk_usage::new_scan_id(),
            crate::disk_usage::next_snapshot_version(),
        )?;
        let snap = Arc::new(loaded.snapshot);
        let summary = snap.summary();
        crate::disk_usage::store_snapshot_arc(snap);
        Some(DiskUsageCacheLoad {
            summary,
            engine: loaded.meta.engine,
            mutated: loaded.mutated,
        })
    })
    .await
    .ok()
    .flatten()
}

/// Cached roots (header data only), for Settings.
#[tauri::command]
pub async fn disk_usage_cache_list(
    app: tauri::AppHandle,
) -> Vec<crate::disk_usage_cache::CacheEntryInfo> {
    let Some(dir) = crate::disk_usage_cache::cache_dir(&app) else {
        return Vec::new();
    };
    spawn_blocking(move || crate::disk_usage_cache::list(&dir))
        .await
        .unwrap_or_default()
}

/// "Hapus cache": one root, or every cached root when `root` is absent.
/// Returns how many cached roots were removed.
#[tauri::command]
pub async fn disk_usage_cache_delete(root: Option<String>, app: tauri::AppHandle) -> usize {
    let Some(dir) = crate::disk_usage_cache::cache_dir(&app) else {
        return 0;
    };
    spawn_blocking(move || match root {
        Some(r) => usize::from(crate::disk_usage_cache::delete(&dir, &r)),
        None => crate::disk_usage_cache::clear(&dir),
    })
    .await
    .unwrap_or(0)
}

/// A delete/rename/move succeeded inside Ferrum: cached roots containing (or
/// inside) any of `paths` are marked mutated. Best effort, never an error.
#[tauri::command]
pub async fn disk_usage_cache_note_mutation(paths: Vec<String>, app: tauri::AppHandle) -> usize {
    let Some(dir) = crate::disk_usage_cache::cache_dir(&app) else {
        return 0;
    };
    spawn_blocking(move || crate::disk_usage_cache::note_mutation(&dir, &paths))
        .await
        .unwrap_or(0)
}

// ── DSK-03 (FER-321): largest-first query + top files ────────────────────

/// Paginated DIRECT children of one node of a stored scan — child dirs
/// (subtree aggregates) + direct files + direct symlinks — largest-first by
/// the selected metric (default logical), stable id-asc tie-break so pages
/// are reproducible (§3.8). Filtering never mutates stored data; unknown
/// allocated bytes stay `null` and only affect ORDERING (smallest side).
/// Unknown/evicted `scanId` or an id outside the scanned tree → structured
/// `NotFound` (UI: rescan). The query never re-walks the filesystem.
#[tauri::command]
pub async fn disk_usage_query_children(
    scan_id: String,
    node_id: String,
    offset: u64,
    limit: u64,
    sort: Option<crate::disk_usage::DiskUsageSort>,
    filter: Option<crate::disk_usage::DiskUsageFilter>,
) -> Result<crate::disk_usage::DiskUsageChildrenPage, FsError> {
    let sort = sort.unwrap_or_default();
    let filter = filter.unwrap_or_default();
    spawn_blocking(move || {
        crate::disk_usage::query_children_by_scan(&scan_id, &node_id, offset, limit, sort, &filter)
    })
    .await
    .map_err(|e| FsError::new(crate::domain::ErrorCode::Io, e.to_string()))?
}

/// The N largest FILES of a whole subtree (recursive, files only — symlinks
/// excluded, never followed D07), largest-first with stable id tie-break.
/// `truncated` + the "Top N" label make the cap explicit: never a claim of
/// all results (§3.8). Same snapshot/NotFound semantics as
/// [`disk_usage_query_children`]; percent-of-parent uses each file's direct
/// parent, percent-of-scan-root the scan root — independent denominators.
#[tauri::command]
pub async fn disk_usage_top_files(
    scan_id: String,
    node_id: String,
    limit: u64,
    filter: Option<crate::disk_usage::DiskUsageFilter>,
) -> Result<crate::disk_usage::DiskUsageTopFiles, FsError> {
    let filter = filter.unwrap_or_default();
    spawn_blocking(move || crate::disk_usage::top_files_by_scan(&scan_id, &node_id, limit, &filter))
        .await
        .map_err(|e| FsError::new(crate::domain::ErrorCode::Io, e.to_string()))?
}

// ── Watcher (src/lib/ipc/watch.ts contract) ─────────────────────────────

/// Global watch hub wired to the `fs://changed` event. The sink emits the
/// coalesced batch: hit roots + capped changed paths (F2-26 — the frontend
/// uses the paths to tell scoped updates apart from ambiguous ones).
fn watch_hub(app: &AppHandle) -> &'static Arc<crate::watch::WatchHub> {
    static HUB: std::sync::OnceLock<Arc<crate::watch::WatchHub>> = std::sync::OnceLock::new();
    HUB.get_or_init(|| {
        let handle = app.clone();
        Arc::new(crate::watch::WatchHub::new(Box::new(
            move |batch: crate::watch::FsChangeBatch| {
                let _ = handle.emit("fs://changed", batch);
            },
        )))
    })
}

#[tauri::command]
pub fn watch_start(path: String, app: AppHandle) -> Result<(), FsError> {
    check_mutation_path(&path)?;
    watch_hub(&app).watch_start(&path)
}

#[tauri::command]
pub fn watch_stop(path: String, app: AppHandle) -> Result<(), FsError> {
    check_mutation_path(&path)?;
    watch_hub(&app).watch_stop(&path)
}

// ── Preferences (src/lib/ipc/prefs.ts contract) ─────────────────────────

use crate::prefs::Prefs;

#[tauri::command]
pub fn load_prefs(app: AppHandle) -> Prefs {
    let dir = crate::profile::config_dir(&app).unwrap_or_else(std::env::temp_dir);
    crate::prefs::load_from(&dir)
}

/// FER-471: Settings defaults for jobs that take no explicit copy options.
fn remember_copy_defaults(prefs: &Prefs) {
    crate::copy_pool::set_defaults(
        crate::copy_pool::ThreadSetting::from_pref(prefs.copy_threads),
        crate::copy_pool::Priority::parse(&prefs.copy_priority)
            .unwrap_or(crate::copy_pool::Priority::Normal),
    );
}

#[tauri::command]
pub fn save_prefs(app: AppHandle, prefs: Prefs) -> Result<(), FsError> {
    let dir = crate::profile::config_dir(&app).unwrap_or_else(std::env::temp_dir);
    remember_copy_defaults(&prefs);
    // FER-511: the webview cannot write the Rar.exe path or its fingerprint — both always keep
    // the value already stored on disk, whatever the request carries.
    crate::prefs::save_from_webview(&dir, &prefs).map(|_| ())
}

// ── Activity (phase-4; renderer never touches SQL) ──────────────────────

use crate::activity_service::{ActivityHandle, SearchScope, VisitKind};
use ferrum_activity_contract::ActivityKind;

fn activity_service_of<'a>(
    handle: &'a State<'_, ActivityHandle>,
) -> Result<&'a crate::activity_service::ActivityService, FsError> {
    handle
        .0
        .as_ref()
        .ok_or_else(|| FsError::new(crate::domain::ErrorCode::Io, "activity store unavailable"))
}

fn visit_kind(raw: &str) -> Result<VisitKind, FsError> {
    match raw {
        "folder" => Ok(VisitKind::Folder),
        "file" => Ok(VisitKind::FileOpen),
        other => Err(FsError::new(
            crate::domain::ErrorCode::InvalidPath,
            format!("unknown activity kind {other:?}"),
        )),
    }
}

fn search_scope(raw: &str) -> Result<SearchScope, FsError> {
    match raw {
        "folder" => Ok(SearchScope::Folder),
        "file" => Ok(SearchScope::File),
        "all" => Ok(SearchScope::All),
        other => Err(FsError::new(
            crate::domain::ErrorCode::InvalidPath,
            format!("unknown search scope {other:?}"),
        )),
    }
}

fn query_kind(raw: &str) -> Result<ActivityKind, FsError> {
    match raw {
        "folder" => Ok(ActivityKind::FolderVisit),
        "file" => Ok(ActivityKind::FileOpenRequested),
        other => Err(FsError::new(
            crate::domain::ErrorCode::InvalidPath,
            format!("unknown query kind {other:?}"),
        )),
    }
}

/// Fire-and-forget from the renderer; failures resolve to a label, never a
/// broken navigation. Recording stays best-effort statistics by design.
#[tauri::command]
pub fn activity_record_visit(
    path: String,
    kind: String,
    activity: State<'_, ActivityHandle>,
) -> Result<String, FsError> {
    let visit = visit_kind(&kind)?;
    activity_service_of(&activity)?
        .record_visit(&path, visit)
        .map_err(fs_io)
}

/// Lazy validation when a suggestion is chosen: stats (with a deadline) only
/// if the entity is stale. Returns the availability label, `None` if the path
/// is not tracked. Never called per keystroke.
#[tauri::command]
pub async fn activity_validate(path: String, app: AppHandle) -> Result<Option<String>, FsError> {
    spawn_blocking(move || {
        let handle = app.state::<ActivityHandle>();
        let service = activity_service_of(&handle)?;
        service
            .validate_if_stale(&path)
            .map(|availability| availability.map(|a| format!("{a:?}").to_lowercase()))
            .map_err(fs_io)
    })
    .await
    .map_err(|e| FsError::new(crate::domain::ErrorCode::Io, e.to_string()))?
}

#[tauri::command]
pub fn activity_recent(
    kind: String,
    limit: u16,
    workspace_root: Option<String>,
    activity: State<'_, ActivityHandle>,
) -> Result<Vec<crate::activity_service::ActivityItem>, FsError> {
    activity_service_of(&activity)?
        .recent_scoped(query_kind(&kind)?, limit, workspace_root.as_deref())
        .map_err(fs_io)
}

#[tauri::command]
pub fn activity_frequent(
    kind: String,
    limit: u16,
    workspace_root: Option<String>,
    activity: State<'_, ActivityHandle>,
) -> Result<Vec<crate::activity_service::ActivityItem>, FsError> {
    activity_service_of(&activity)?
        .frequent_scoped(query_kind(&kind)?, limit, workspace_root.as_deref())
        .map_err(fs_io)
}

#[tauri::command]
pub fn activity_search(
    query: String,
    kind: String,
    limit: u16,
    workspace_root: Option<String>,
    activity: State<'_, ActivityHandle>,
) -> Result<Option<Vec<crate::activity_service::ScoredMatch>>, FsError> {
    activity_service_of(&activity)?
        .search_scoped(
            &query,
            search_scope(&kind)?,
            limit,
            workspace_root.as_deref(),
        )
        .map_err(fs_io)
}

#[tauri::command]
pub fn activity_state(
    activity: State<'_, ActivityHandle>,
) -> Result<crate::activity_service::ActivityStateDto, FsError> {
    activity_service_of(&activity)?.state().map_err(fs_io)
}

/// Every privacy-relevant mutation broadcasts the new state so other windows
/// invalidate cached suggestions immediately (privacy epoch / retention / hide).
fn broadcast_state(app: &AppHandle, service: &crate::activity_service::ActivityService) {
    if let Ok(state) = service.state() {
        let _ = app.emit("activity://changed", state);
    }
}

#[tauri::command]
pub fn activity_set_recording(
    enabled: bool,
    app: AppHandle,
    activity: State<'_, ActivityHandle>,
) -> Result<crate::activity_service::ActivityStateDto, FsError> {
    let service = activity_service_of(&activity)?;
    let state = service.set_recording(enabled).map_err(fs_io)?;
    broadcast_state(&app, service);
    Ok(state)
}

#[tauri::command]
pub fn activity_set_retention(
    days: u32,
    app: AppHandle,
    activity: State<'_, ActivityHandle>,
) -> Result<crate::activity_service::ActivityStateDto, FsError> {
    let service = activity_service_of(&activity)?;
    let state = service.set_retention(days).map_err(fs_io)?;
    broadcast_state(&app, service);
    Ok(state)
}

#[tauri::command]
pub fn activity_set_hidden(
    entity_id: String,
    hidden: bool,
    app: AppHandle,
    activity: State<'_, ActivityHandle>,
) -> Result<crate::activity_service::ActivityStateDto, FsError> {
    let service = activity_service_of(&activity)?;
    let state = service.set_hidden(&entity_id, hidden).map_err(fs_io)?;
    broadcast_state(&app, service);
    Ok(state)
}

#[tauri::command]
pub fn activity_ack_disclosure(
    app: AppHandle,
    activity: State<'_, ActivityHandle>,
) -> Result<crate::activity_service::ActivityStateDto, FsError> {
    let service = activity_service_of(&activity)?;
    let state = service.ack_disclosure().map_err(fs_io)?;
    broadcast_state(&app, service);
    Ok(state)
}

#[tauri::command]
pub fn activity_forget(
    entity_id: String,
    app: AppHandle,
    activity: State<'_, ActivityHandle>,
) -> Result<(), FsError> {
    let service = activity_service_of(&activity)?;
    service.forget(&entity_id).map_err(fs_io)?;
    broadcast_state(&app, service);
    Ok(())
}

#[tauri::command]
pub fn activity_exclude(
    path: String,
    app: AppHandle,
    activity: State<'_, ActivityHandle>,
) -> Result<u64, FsError> {
    check_mutation_path(&path)?;
    let service = activity_service_of(&activity)?;
    let removed = service.exclude(&path).map_err(fs_io)?;
    broadcast_state(&app, service);
    Ok(removed)
}

#[tauri::command]
pub fn activity_clear(app: AppHandle, activity: State<'_, ActivityHandle>) -> Result<u64, FsError> {
    let service = activity_service_of(&activity)?;
    let removed = service.clear().map_err(fs_io)?;
    broadcast_state(&app, service);
    Ok(removed)
}

#[tauri::command]
pub fn activity_overview(
    days: u32,
    kind: String,
    sort: String,
    limit: u16,
    activity: State<'_, ActivityHandle>,
) -> Result<crate::activity_service::ActivityOverview, FsError> {
    let by_frequency = match sort.as_str() {
        "recent" => false,
        "frequent" => true,
        other => {
            return Err(FsError::new(
                crate::domain::ErrorCode::InvalidPath,
                format!("unknown sort {other:?}"),
            ));
        }
    };
    activity_service_of(&activity)?
        .overview(days, query_kind(&kind)?, by_frequency, limit)
        .map_err(fs_io)
}

#[tauri::command]
pub fn activity_preview(
    scope: String,
    target: Option<String>,
    activity: State<'_, ActivityHandle>,
) -> Result<crate::activity_service::ScopePreview, FsError> {
    activity_service_of(&activity)?
        .preview(&scope, target.as_deref().unwrap_or(""))
        .map_err(fs_io)
}

#[tauri::command]
pub fn activity_favorites(
    activity: State<'_, ActivityHandle>,
) -> Result<Vec<crate::activity_service::FavoriteDto>, FsError> {
    activity_service_of(&activity)?.favorites().map_err(fs_io)
}

#[tauri::command]
pub fn activity_add_favorite(
    path: String,
    kind: String,
    activity: State<'_, ActivityHandle>,
) -> Result<bool, FsError> {
    activity_service_of(&activity)?
        .add_favorite(&path, &kind)
        .map_err(fs_io)
}

#[tauri::command]
pub fn activity_remove_favorite(
    path: String,
    activity: State<'_, ActivityHandle>,
) -> Result<bool, FsError> {
    activity_service_of(&activity)?
        .remove_favorite(&path)
        .map_err(fs_io)
}

/// ACT-08 R1: read-only history lookup for chat. The request is the strict
/// `ai::contract::ActivityQuery` wire (deny_unknown_fields; kind recent/frequent/findByName,
/// periodDays 7/30/90, plain name, limit 1..20, default 10). The answer is the chat tool's
/// `ChatResponse` (id, displayName, parentHint, kind, availability, reasons only; no locator,
/// counts, scores or timestamps). It stays local: nothing here talks to an AI provider, and no
/// receipt/episode is recorded.
#[tauri::command]
pub fn activity_chat_query(
    request: crate::ai::contract::ActivityQuery,
    activity: State<'_, ActivityHandle>,
) -> Result<crate::activity_chat_tool::ChatResponse, FsError> {
    activity_chat_query_on(activity.0.as_ref(), &request)
}

pub(crate) fn activity_chat_query_on(
    service: Option<&crate::activity_service::ActivityService>,
    request: &crate::ai::contract::ActivityQuery,
) -> Result<crate::activity_chat_tool::ChatResponse, FsError> {
    let chat = chat_request_of(request)?;
    Ok(match service {
        Some(service) => service.chat_query(&chat),
        None => crate::activity_chat_tool::ChatResponse::Unavailable,
    })
}

/// ACT-08 R7 v1: locator of one entity a chat answer listed, for the card's explicit
/// "Open in panel" button. Read-only; `null` when hidden, paused, unknown or not openable.
#[tauri::command]
pub fn activity_chat_locate(
    entity_id: String,
    activity: State<'_, ActivityHandle>,
) -> Option<crate::activity_service::ChatLocation> {
    activity_chat_locate_on(activity.0.as_ref(), &entity_id)
}

pub(crate) fn activity_chat_locate_on(
    service: Option<&crate::activity_service::ActivityService>,
    entity_id: &str,
) -> Option<crate::activity_service::ChatLocation> {
    service.and_then(|service| service.chat_locate(entity_id))
}

/// Validates the wire (same rules as the AI contract) and maps it 1:1 to the tool request.
fn chat_request_of(
    request: &crate::ai::contract::ActivityQuery,
) -> Result<crate::activity_chat_tool::ChatRequest, FsError> {
    use crate::activity_chat_tool::{ChatRequest, ChatRequestKind};
    use crate::ai::contract::{HistoryEntity, HistoryQuery, validate_activity_query};
    validate_activity_query(request).map_err(|_| {
        FsError::new(
            crate::domain::ErrorCode::InvalidPath,
            "invalid activity query",
        )
    })?;
    let file = |entity: &HistoryEntity| *entity == HistoryEntity::File;
    let kind = match &request.query {
        HistoryQuery::Recent {
            period_days,
            entity,
        } => ChatRequestKind::Recent {
            days: *period_days,
            file: file(entity),
        },
        HistoryQuery::Frequent {
            period_days,
            entity,
        } => ChatRequestKind::Frequent {
            days: *period_days,
            file: file(entity),
        },
        HistoryQuery::FindByName { name, entity } => ChatRequestKind::FindByName {
            query: name.clone(),
            file: file(entity),
        },
    };
    Ok(ChatRequest {
        kind,
        limit: request.limit.map(u16::from),
    })
}

#[cfg(test)]
#[path = "ipc_activity_chat_tests.rs"]
mod activity_chat_query_tests;

fn fs_io(message: String) -> FsError {
    FsError::new(crate::domain::ErrorCode::Io, message)
}

// ── Explicit OS file open (OpenAccepted receipt source) ─────────────────

#[tauri::command]
pub async fn shell_open(
    window: tauri::Window,
    path: String,
    record: Option<bool>,
    activity: State<'_, ActivityHandle>,
) -> Result<bool, FsError> {
    validate_path(&path)?;
    let owner = window_hwnd(&window);
    let accepted = open_through_shell(owner, &path).await?;

    if shell_open_records(accepted, record) {
        // Receipt only after the OS accepted the request; failures are quiet.
        if let Ok(service) = activity_service_of(&activity) {
            let _ = service.record_visit(&path, VisitKind::FileOpen);
        }
    }
    Ok(accepted)
}

/// The one launch path for an explicit "open this file": native confirmation for executables,
/// scripts and installers, then the OS default handler.
async fn open_through_shell(owner: isize, path: &str) -> Result<bool, FsError> {
    let path = path.to_owned();
    spawn_blocking(move || {
        crate::shell_open::shell_open_with(
            owner,
            &path,
            |o, text| {
                crate::shell_open::native_confirm_with_guard(
                    o,
                    text,
                    crate::shell_open::native_confirm,
                )
            },
            crate::shell_open::shell_execute_target,
        )
    })
    .await
    .map_err(|e| FsError::new(crate::domain::ErrorCode::Io, e.to_string()))?
}

/// FER-575: open one member of an archive. The backend parses the archive/member pair itself,
/// extracts only that member through the fail-closed extract pipeline into Ferrum's own temp
/// folder, and launches the extracted file through the same confirmed shell path as `shell_open`.
/// Returns whether the OS accepted the launch. No activity receipt: the file lives in temp.
#[tauri::command]
pub async fn archive_open_member(
    window: tauri::Window,
    archive_path: String,
    member_path: String,
    password: Option<String>,
    request_id: Option<String>,
    app: tauri::AppHandle,
) -> Result<bool, FsError> {
    validate_path(&archive_path)?;
    let archive_path = crate::path_guard::check_str(&archive_path)?;
    let provider = archive_provider(&archive_path)?;
    let temp = app
        .try_state::<crate::archive_open::OpenTemp>()
        .map(|t| t.inner().clone())
        .ok_or_else(|| {
            FsError::new(
                crate::domain::ErrorCode::Io,
                "The temporary folder for opened archive files is not available",
            )
        })?;
    let owner = window_hwnd(&window);
    let job_archive = archive_path.clone();
    let extracted = spawn_blocking(move || {
        crate::archive_open::extract_member(
            provider.as_ref(),
            &temp,
            &job_archive,
            &member_path,
            password.as_deref(),
            request_id.as_deref(),
            &mut |ev| {
                let _ = app.emit(crate::archive_jobs::PROGRESS_EVENT, ev);
            },
        )
    })
    .await
    .map_err(|e| FsError::new(crate::domain::ErrorCode::Io, e.to_string()))?
    .map_err(|e| archive_to_fs_err(&archive_path, e))?;
    let extracted = extracted.to_string_lossy().into_owned();
    validate_path(&extracted)?;
    open_through_shell(owner, &extracted).await
}

/// FER-432 fail-closed: a receipt is written only when the OS accepted the
/// open AND the renderer explicitly asked for one (`record: true`, i.e. the
/// window's `recordingActive`). A missing flag means "do not record", so a
/// private-session/paused window can never leak a receipt through a caller
/// that forgot to forward it.
fn shell_open_records(accepted: bool, record: Option<bool>) -> bool {
    accepted && record.unwrap_or(false)
}

#[cfg(test)]
mod shell_open_record_tests {
    use super::shell_open_records;

    #[test]
    fn missing_record_flag_never_records_fail_closed() {
        // FER-432: an older/forgetful caller that omits `record` must not
        // write a receipt (private session lives only in the renderer).
        assert!(!shell_open_records(true, None));
        assert!(!shell_open_records(true, Some(false)));
        assert!(shell_open_records(true, Some(true)));
        // A declined OS open never records, whatever the flag.
        assert!(!shell_open_records(false, Some(true)));
        assert!(!shell_open_records(false, None));
    }
}

// ── F2-17: bounded safe preview ──────────────────────────────────────────

/// Read at most `max_bytes` of a file for the Quick View panel. Runs on a
/// blocking thread so large files never stall the UI.
#[tauri::command]
pub async fn read_preview(
    path: String,
    max_bytes: u64,
) -> Result<crate::preview::PreviewData, FsError> {
    spawn_blocking(move || crate::preview::read_preview(&path, max_bytes))
        .await
        .map_err(|e| FsError::new(crate::domain::ErrorCode::Io, e.to_string()))?
}

/// FER-103: header-checked, bomb-guarded, downsampled image preview. Small
/// images come back as `downsampled = false` (the webview loads them itself).
#[tauri::command]
pub async fn read_image_preview(
    path: String,
) -> Result<crate::preview_image::ImagePreview, FsError> {
    spawn_blocking(move || crate::preview_image::read_image_preview(&path))
        .await
        .map_err(|e| FsError::new(crate::domain::ErrorCode::Io, e.to_string()))?
}

/// FER-453: raw bytes of one PDF (capped at 50 MiB, magic-checked) for the sandboxed
/// pdf.js viewer. Returned as a binary response, not JSON, so no base64 round-trip.
#[tauri::command]
pub async fn read_pdf_bytes(path: String) -> Result<tauri::ipc::Response, FsError> {
    spawn_blocking(move || crate::preview::read_pdf_bytes(&path))
        .await
        .map_err(|e| FsError::new(crate::domain::ErrorCode::Io, e.to_string()))?
        .map(tauri::ipc::Response::new)
}

/// Bounded in-memory preview of one archive member (FER-104). Never extracts.
#[tauri::command]
pub async fn read_archive_member_preview(
    archive_path: String,
    member_path: String,
    max_bytes: u64,
    request_id: Option<String>,
) -> Result<crate::preview::PreviewData, FsError> {
    spawn_blocking(move || {
        let registration = crate::archive_cancel::begin(request_id.as_deref());
        registration.run(|| {
            crate::preview_archive::read_member_preview(&archive_path, &member_path, max_bytes)
        })
    })
    .await
    .map_err(|e| FsError::new(crate::domain::ErrorCode::Io, e.to_string()))?
}

/// Cancel an in-flight (or about-to-start) `read_archive_member_preview` /
/// `archive_read` by request id (FER-438). Cooperative: the blocking thread
/// stops at its next 64 KiB chunk; unknown ids are harmless.
#[tauri::command]
pub fn archive_cancel_request(request_id: String) {
    crate::archive_cancel::cancel(&request_id);
}

/// EXIF / media metadata (PRV-06). Read-only, ≤256 KiB head, never logged.
#[tauri::command]
pub async fn read_media_metadata(
    path: String,
) -> Result<crate::media_meta::MediaMetadata, FsError> {
    spawn_blocking(move || crate::media_meta::read_media_metadata(&path))
        .await
        .map_err(|e| FsError::new(crate::domain::ErrorCode::Io, e.to_string()))?
}

// ── F2-18: read-only compare engines ─────────────────────────────────────

/// Stream-compare two files by content (binary-safe, 64 KiB chunks) plus a
/// bounded text diff for text files. Cancel via `compare_cancel(op_id)`.
#[tauri::command]
pub async fn compare_files(
    app: tauri::AppHandle,
    path_a: String,
    path_b: String,
    op_id: Option<String>,
    encoding: Option<String>,
    diff_context: Option<u32>,
) -> Result<crate::compare::CompareResult, FsError> {
    spawn_blocking(move || {
        let ctl = crate::compare::Ctl::new(op_id, Some(compare_sink(app)));
        let opts = crate::compare::FileOpts {
            encoding,
            diff_context,
        };
        crate::compare::compare_files_with(&path_a, &path_b, &opts, &ctl)
    })
    .await
    .map_err(|e| FsError::new(crate::domain::ErrorCode::Io, e.to_string()))?
}

/// Directory comparison between the two pane roots (read-only); one level or
/// recursive, with progress events and cooperative cancel.
///
/// FER-472: `fast_scan = true` lets roots on a local NTFS volume be read
/// through the elevated MFT helper (one UAC prompt per volume). Declined UAC,
/// a non-NTFS/remote root, a missing helper or a damaged MFT fall back to the
/// ordinary scan; `report.fastScan` says what was used and why not.
///
/// FER-477b: `incremental = true` (a recursive compare by content for sync)
/// asks the FerrumIndexer which directories changed since the journal marks
/// of the pair's stored baseline, skips content reads it proves unnecessary,
/// and returns fresh marks in `report.journal` (`report.incremental` says
/// what was used). Any problem = the ordinary compare.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn compare_dirs(
    app: tauri::AppHandle,
    window: tauri::Window,
    baselines: tauri::State<'_, crate::sync_baseline::SyncBaselineStore>,
    left: String,
    right: String,
    by_content: bool,
    recursive: Option<bool>,
    op_id: Option<String>,
    no_cache: Option<bool>,
    fast_scan: Option<bool>,
    incremental: Option<bool>,
) -> Result<crate::compare::CompareDirsReport, FsError> {
    let hwnd = window_hwnd(&window);
    let store = baselines.inner().clone();
    // FER-477 review: skipping content reads is opt-in (Settings, default
    // off). The pref is checked HERE too, so the frontend flag alone never
    // turns it on; off = exactly the compare from before FER-477.
    let journal_skip = crate::profile::config_dir(&app)
        .map(|d| crate::prefs::load_from(&d).sync_journal_skip)
        .unwrap_or(false);
    spawn_blocking(move || {
        let mut ctl = crate::compare::Ctl::new(op_id, Some(compare_sink(app)));
        ctl.no_cache = no_cache.unwrap_or(false);
        let opts = crate::compare::DirOpts {
            by_content,
            recursive: recursive.unwrap_or(false),
            max_entries: 0,
        };
        // FER-477b: marks and change lists BEFORE anything is walked.
        let prepared =
            if journal_skip && incremental.unwrap_or(false) && by_content && opts.recursive {
                crate::compare::validate_dir_roots(&left, &right)?;
                let p = crate::sync_incremental::prepare(
                    &left,
                    &right,
                    &store,
                    &mut crate::sync_incremental::ServiceChanges,
                );
                ctl.content_hint = p.hint.clone();
                Some(p)
            } else {
                None
            };
        let finish = |mut report: crate::compare::CompareDirsReport| {
            if let Some(p) = prepared {
                let mut info = p.info;
                info.content_skipped = p.hint.as_ref().map_or(0, |h| h.skipped());
                report.journal = p.marks;
                report.incremental = Some(info);
            }
            report
        };
        if !fast_scan.unwrap_or(false) {
            return crate::compare::compare_dirs_with(&left, &right, opts, &ctl).map(finish);
        }
        // Same root rules as the walker, BEFORE asking for elevation.
        crate::compare::validate_dir_roots(&left, &right)?;
        let prep = crate::tree_source::prepare_fast_with_helper(&left, &right, hwnd, &ctl);
        let disk_l = crate::tree_source::DiskSource::new(&left);
        let disk_r = crate::tree_source::DiskSource::new(&right);
        let ls: &dyn crate::tree_source::TreeSource = match &prep.left {
            Some(s) => s,
            None => &disk_l,
        };
        let rs: &dyn crate::tree_source::TreeSource = match &prep.right {
            Some(s) => s,
            None => &disk_r,
        };
        let mut report =
            crate::compare::compare_dirs_with_sources(&left, &right, opts, &ctl, ls, rs)?;
        report.fast_scan = Some(prep.info);
        Ok(finish(report))
    })
    .await
    .map_err(|e| FsError::new(crate::domain::ErrorCode::Io, e.to_string()))?
}

/// Ask an in-flight compare to stop (no-op for unknown / finished ids).
#[tauri::command]
pub fn compare_cancel(op_id: String) -> Result<(), FsError> {
    crate::compare::cancel(&op_id);
    Ok(())
}

/// Drop all cached content verdicts.
#[tauri::command]
pub fn compare_cache_clear() -> Result<(), FsError> {
    crate::compare::cache_clear();
    Ok(())
}

fn compare_sink(app: tauri::AppHandle) -> crate::compare::ProgressSink {
    use tauri::Emitter;
    std::sync::Arc::new(move |p| {
        let _ = app.emit(crate::compare::PROGRESS_EVENT, p);
    })
}

// ── F2-20..F2-24: multi-rename, properties, branch view, file tools ─────
/// Multi-Rename Tool executor (F2-20): one ordered batch with swap/cycle
/// handling and a per-item report. Fast (metadata-only renames), so it runs
/// as a direct command instead of queueing.
#[tauri::command]
pub fn rename_batch(
    pairs: Vec<crate::rename_batch::RenamePair>,
) -> Result<crate::rename_batch::RenameBatchReport, FsError> {
    check_write_sources(pairs.iter().map(|p| &p.from))?;
    for pair in &pairs {
        // `to` is a bare name or a path; either way its final component is what gets created.
        if let Some(parent) = Path::new(&pair.from).parent() {
            let name = Path::new(&pair.to)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| pair.to.clone());
            crate::startup_guard::check_rename_to(&parent.join(&name).to_string_lossy())?;
        }
    }
    Ok(crate::rename_batch::rename_batch(&pairs))
}

/// Properties detail sheet for one entry (F2-21, Alt+Enter).
#[tauri::command]
pub async fn entry_properties(path: String) -> Result<crate::properties::EntryProperties, FsError> {
    validate_path(&path)?;
    spawn_blocking(move || crate::properties::entry_properties(&path))
        .await
        .map_err(|e| FsError::new(crate::domain::ErrorCode::Io, e.to_string()))?
}

/// Recursive occupied space (F2-21, Ctrl+L). Heavy trees run on a blocking
/// thread. The walk is cancellable via `occupied_space_cancel(scanId)` and
/// emits throttled `properties://occupied-progress` events (`scanId` + running
/// totals); a cancelled walk still resolves, with `cancelled = true`.
#[tauri::command]
pub async fn occupied_space(
    app: tauri::AppHandle,
    scan_id: String,
    paths: Vec<String>,
) -> Result<crate::properties::OccupiedReport, FsError> {
    use tauri::Emitter;
    for p in &paths {
        validate_path(p)?;
    }
    let token = crate::properties::register_scan(&scan_id);
    let id = scan_id.clone();
    let result = spawn_blocking(move || {
        let mut last = std::time::Instant::now();
        let report = crate::properties::occupied_space_with(&paths, &token, &mut |p| {
            if last.elapsed() >= std::time::Duration::from_millis(100) {
                last = std::time::Instant::now();
                let _ = app.emit(
                    "properties://occupied-progress",
                    serde_json::json!({
                        "scanId": id,
                        "files": p.files,
                        "dirs": p.dirs,
                        "symlinks": p.symlinks,
                        "logicalBytes": p.logical_bytes,
                    }),
                );
            }
        });
        crate::properties::finish_scan(&id);
        report
    })
    .await;
    crate::properties::finish_scan(&scan_id);
    result.map_err(|e| FsError::new(crate::domain::ErrorCode::Io, e.to_string()))
}

/// Cancels a running occupied-space walk; false when it already ended.
#[tauri::command]
pub fn occupied_space_cancel(scan_id: String) -> bool {
    crate::properties::cancel_scan(&scan_id)
}

/// Host OS/arch/name from Rust std for System Information (F2-21).
#[tauri::command]
pub fn system_info() -> crate::properties::HostInfo {
    crate::properties::host_info()
}

/// Read-only volume info for the drive holding `path` (F2-21).
#[tauri::command]
pub async fn volume_info(path: String) -> Result<crate::properties::VolumeInfo, FsError> {
    validate_path(&path)?;
    spawn_blocking(move || crate::properties::volume_info(&path))
        .await
        .map_err(|e| FsError::new(crate::domain::ErrorCode::Io, e.to_string()))?
}

/// Toggle READONLY/HIDDEN (F2-21). Windows only; `None` keeps an attribute.
#[tauri::command]
pub fn set_attributes(
    path: String,
    read_only: Option<bool>,
    hidden: Option<bool>,
) -> Result<(), FsError> {
    let path = prepare_source(&path)?;
    crate::properties::set_attributes(&path, read_only, hidden)
}

/// Branch View (F2-23 / FER-128, Ctrl+B): start a streaming, cancellable walk
/// of the subtree below `root`. Returns immediately; batches of entries (with
/// original paths) arrive as `branch://progress` events (about 10/s at most)
/// and one final `done` event carries the summary or the error. The whole
/// listing is never one IPC response (FER-479).
#[tauri::command]
pub fn branch_start(
    id: String,
    root: String,
    max_entries: Option<u64>,
    app: tauri::AppHandle,
) -> Result<(), FsError> {
    use tauri::Emitter;
    if id.trim().is_empty() {
        return Err(FsError::new(
            crate::domain::ErrorCode::InvalidPath,
            "Branch id must not be empty",
        ));
    }
    validate_path(&root)?;
    let limit = max_entries.unwrap_or(crate::branch_view::DEFAULT_MAX_ENTRIES);
    let cancel = Arc::new(AtomicBool::new(false));
    crate::branch_view::active_cancels()
        .lock()
        .expect("branch registry")
        .insert(id.clone(), cancel.clone());
    tauri::async_runtime::spawn_blocking(move || {
        let result = crate::branch_view::walk_branch(
            &root,
            crate::branch_view::StreamOptions::with_max(limit),
            &cancel,
            &mut |batch| {
                let _ = app.emit(
                    "branch://progress",
                    crate::branch_view::BranchEvent::batch(&id, batch),
                );
            },
        );
        crate::branch_view::active_cancels()
            .lock()
            .expect("branch registry")
            .remove(&id);
        let _ = app.emit(
            "branch://progress",
            crate::branch_view::BranchEvent::finished(&id, result),
        );
    });
    Ok(())
}

/// Stop a running branch walk at its next entry; it still sends its final
/// `done` event (`cancelled = true`). False when the id is unknown/finished.
#[tauri::command]
pub fn branch_cancel(id: String) -> bool {
    match crate::branch_view::active_cancels()
        .lock()
        .expect("branch registry")
        .remove(&id)
    {
        Some(flag) => {
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
            true
        }
        None => false,
    }
}

/// Conflict policy for tool outputs: absent or "refuse" keeps the safe
/// default (refuse an existing output); anything else is a copy-engine policy.
fn parse_output_policy(policy: Option<String>) -> Result<crate::file_tools::OutputPolicy, FsError> {
    match policy.as_deref() {
        None | Some("refuse") => Ok(None),
        Some(other) => parse_policy(other).map(Some),
    }
}

#[cfg(test)]
mod output_policy_tests {
    use super::*;
    use crate::copy_engine::ConflictPolicy;

    #[test]
    fn refuse_is_the_default_and_policies_map_through() {
        assert_eq!(parse_output_policy(None).unwrap(), None);
        assert_eq!(parse_output_policy(Some("refuse".into())).unwrap(), None);
        assert_eq!(
            parse_output_policy(Some("skip".into())).unwrap(),
            Some(ConflictPolicy::Skip)
        );
        assert_eq!(
            parse_output_policy(Some("overwriteOlder".into())).unwrap(),
            Some(ConflictPolicy::OverwriteOlder)
        );
        assert!(parse_output_policy(Some("bogus".into())).is_err());
    }
}

/// Verify checksums for the selection (F2-24). Read-only and report-shaped,
/// so it stays a direct command; the UI shows the per-item outcomes. With a
/// `request_id` the run registers a cancel flag that `cancel_checksum_request`
/// can trip (the report then carries `cancelled: true`).
#[tauri::command]
pub async fn verify_checksums(
    app: tauri::AppHandle,
    paths: Vec<String>,
    request_id: Option<String>,
) -> Result<crate::file_tools::VerifyReport, FsError> {
    for p in &paths {
        validate_path(p)?;
    }
    spawn_blocking(move || match request_id {
        Some(id) => {
            let throttle = ProgressThrottle::new();
            crate::file_tools::run_verify_cancellable(&id, &paths, &|p| {
                if throttle.due() {
                    emit_checksum_progress(
                        &app,
                        &id,
                        p.processed_bytes,
                        p.files_done,
                        p.files_total,
                        p.current,
                    );
                }
            })
        }
        None => crate::file_tools::verify_checksums(&paths, &|| false),
    })
    .await
    .map_err(|e| FsError::new(crate::domain::ErrorCode::Io, e.to_string()))
}

/// Event carrying live hashing progress for a `verify_checksums` /
/// `checksum_archive_member` call that passed a `request_id`.
const CHECKSUM_PROGRESS_EVENT: &str = "filetools://checksum-progress";

/// Payload of `CHECKSUM_PROGRESS_EVENT` (camelCase keys, same style as the
/// occupied-space event). `total_bytes` is not known up front, so it is not sent.
fn checksum_progress_payload(
    request_id: &str,
    processed_bytes: u64,
    files_done: u64,
    files_total: u64,
    current: &str,
) -> serde_json::Value {
    serde_json::json!({
        "requestId": request_id,
        "processedBytes": processed_bytes,
        "filesDone": files_done,
        "filesTotal": files_total,
        "current": current,
    })
}

fn emit_checksum_progress(
    app: &tauri::AppHandle,
    request_id: &str,
    processed_bytes: u64,
    files_done: u64,
    files_total: u64,
    current: &str,
) {
    use tauri::Emitter;
    let _ = app.emit(
        CHECKSUM_PROGRESS_EVENT,
        checksum_progress_payload(
            request_id,
            processed_bytes,
            files_done,
            files_total,
            current,
        ),
    );
}

/// Lets at most one progress event through per 100 ms (hash chunks are small).
struct ProgressThrottle(std::cell::Cell<std::time::Instant>);

impl ProgressThrottle {
    fn new() -> Self {
        Self(std::cell::Cell::new(std::time::Instant::now()))
    }
    fn due(&self) -> bool {
        if self.0.get().elapsed() >= std::time::Duration::from_millis(100) {
            self.0.set(std::time::Instant::now());
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod checksum_progress_tests {
    use super::*;

    #[test]
    fn payload_uses_camel_case_keys_and_the_request_id() {
        let v = checksum_progress_payload("verify-1", 42, 1, 3, "a.bin");
        assert_eq!(
            v,
            serde_json::json!({
                "requestId": "verify-1",
                "processedBytes": 42,
                "filesDone": 1,
                "filesTotal": 3,
                "current": "a.bin",
            })
        );
    }

    #[test]
    fn throttle_lets_the_first_burst_through_only_after_the_interval() {
        let t = ProgressThrottle::new();
        assert!(!t.due(), "an immediate event is held back");
        std::thread::sleep(std::time::Duration::from_millis(120));
        assert!(t.due());
        assert!(!t.due(), "the window restarts after an event");
    }
}

/// Stop a running `verify_checksums` / `checksum_archive_member` request.
/// False when nothing with that id is running.
#[tauri::command]
pub fn cancel_checksum_request(request_id: String) -> bool {
    crate::file_tools::cancel_request(&request_id)
}

/// Checksum of one archive member, streamed through the provider without
/// extracting anything. RAR is refused: its provider materialises the whole
/// member in memory, which would break the bounded-memory guarantee.
#[tauri::command]
pub async fn checksum_archive_member(
    app: tauri::AppHandle,
    archive_path: String,
    member_path: String,
    algorithm: String,
    password: Option<String>,
    request_id: Option<String>,
) -> Result<crate::file_tools::ArchiveMemberChecksum, FsError> {
    validate_path(&archive_path)?;
    let algorithm = crate::file_tools::Algorithm::parse(&algorithm)?;
    if is_rar(&archive_path) {
        return Err(FsError::new(
            crate::domain::ErrorCode::Unsupported,
            "RAR members are buffered whole in memory by the RAR reader, so they are not hashed in place",
        )
        .with_path(archive_path));
    }
    let provider = archive_provider(&archive_path)?;
    spawn_blocking(move || {
        let registration = request_id
            .as_deref()
            .map(crate::file_tools::CancelRegistration::new);
        let cancelled = || registration.as_ref().is_some_and(|r| r.is_cancelled());
        let throttle = ProgressThrottle::new();
        let mut processed = 0u64;
        let hash = crate::file_tools::hash_archive_member(
            provider.as_ref(),
            &archive_path,
            &member_path,
            algorithm,
            password,
            &cancelled,
            &mut |delta| {
                processed += delta;
                if let Some(id) = request_id.as_deref()
                    && throttle.due()
                {
                    emit_checksum_progress(&app, id, processed, 0, 1, &member_path);
                }
            },
        )
        .map_err(|e| {
            if e.code == crate::archive_contract::ArchiveErrorCode::Cancelled {
                FsError::new(crate::domain::ErrorCode::Cancelled, "cancelled")
            } else {
                archive_to_fs_err(&archive_path, e)
            }
        })?;
        Ok(crate::file_tools::ArchiveMemberChecksum {
            archive_path,
            member_path,
            algorithm,
            hash,
        })
    })
    .await
    .map_err(|e| FsError::new(crate::domain::ErrorCode::Io, e.to_string()))?
}

/// Queue sidecar checksum creation (F2-24). File-producing work rides the
/// existing operation queue for progress and cancellation; F2-26 makes it
/// retryable from the BTM. `policy` handles an existing sidecar (default:
/// refuse).
#[tauri::command]
pub fn enqueue_checksum_create(
    paths: Vec<String>,
    algorithm: String,
    policy: Option<String>,
    queue: State<'_, Arc<OperationQueue>>,
) -> Result<OperationId, FsError> {
    enqueue_checksum_create_on(&queue, paths, &algorithm, policy)
}

/// Body of `enqueue_checksum_create` against a plain queue, so the validation
/// and job wiring are testable without a Tauri `State`.
fn enqueue_checksum_create_on(
    queue: &OperationQueue,
    paths: Vec<String>,
    algorithm: &str,
    policy: Option<String>,
) -> Result<OperationId, FsError> {
    check_write_targets(&paths)?;
    let algorithm = crate::file_tools::Algorithm::parse(algorithm)?;
    let policy = parse_output_policy(policy)?;
    let make_body =
        move || crate::file_tools::checksum_create_job(paths.clone(), algorithm, policy);
    Ok(queue.enqueue_retryable(
        crate::domain::OperationKind::Create,
        make_body(),
        Arc::new(make_body),
    ))
}

/// Queue a file split (F2-24), retryable via the BTM (F2-26).
#[tauri::command]
pub fn enqueue_split_file(
    path: String,
    part_bytes: u64,
    out_dir: String,
    policy: Option<String>,
    queue: State<'_, Arc<OperationQueue>>,
) -> Result<OperationId, FsError> {
    enqueue_split_file_on(&queue, path, part_bytes, out_dir, policy)
}

/// Body of `enqueue_split_file` against a plain queue (see `enqueue_checksum_create_on`).
fn enqueue_split_file_on(
    queue: &OperationQueue,
    path: String,
    part_bytes: u64,
    out_dir: String,
    policy: Option<String>,
) -> Result<OperationId, FsError> {
    validate_path(&path)?;
    check_write_target(&out_dir)?;
    let policy = parse_output_policy(policy)?;
    let make_body =
        move || crate::file_tools::split_job(path.clone(), part_bytes, out_dir.clone(), policy);
    Ok(queue.enqueue_retryable(
        crate::domain::OperationKind::Create,
        make_body(),
        Arc::new(make_body),
    ))
}

/// Queue a part combine (F2-24), retryable via the BTM (F2-26).
#[tauri::command]
pub fn enqueue_combine_parts(
    first_part: String,
    out_file: String,
    policy: Option<String>,
    queue: State<'_, Arc<OperationQueue>>,
) -> Result<OperationId, FsError> {
    enqueue_combine_parts_on(&queue, first_part, out_file, policy)
}

/// Body of `enqueue_combine_parts` against a plain queue (see `enqueue_checksum_create_on`).
fn enqueue_combine_parts_on(
    queue: &OperationQueue,
    first_part: String,
    out_file: String,
    policy: Option<String>,
) -> Result<OperationId, FsError> {
    validate_path(&first_part)?;
    check_write_target(&out_file)?;
    let policy = parse_output_policy(policy)?;
    let make_body =
        move || crate::file_tools::combine_job(first_part.clone(), out_file.clone(), policy);
    Ok(queue.enqueue_retryable(
        crate::domain::OperationKind::Create,
        make_body(),
        Arc::new(make_body),
    ))
}

fn preview(
    o: &crate::file_tools::ToolOutcome,
    kind: crate::domain::OperationKind,
    processed: usize,
    total: usize,
) -> crate::domain::OperationProgress {
    crate::domain::OperationProgress {
        operation_id: String::new(),
        kind,
        state: crate::domain::OperationState::Running,
        processed_bytes: 0,
        total_bytes: None,
        processed_files: processed as u64,
        total_files: Some(total as u64),
        current_path: Some(o.path.clone()),
        rate_bytes_per_sec: None,
        error: None,
        failures: Vec::new(),
        queued_at_ms: None,
        finished_at_ms: None,
    }
}

// ── F2-25: open external, terminal here, Associate With (F2-27: print) ──
// Every command validates its paths at the boundary and then delegates to
// `shell_open`, which launches through structured platform APIs only.

/// Open a file or folder with its OS default association (F2-25).
#[tauri::command]
pub async fn open_external(window: tauri::Window, path: String) -> Result<(), FsError> {
    validate_path(&path)?;
    let owner = window_hwnd(&window);
    spawn_blocking(move || crate::shell_open::open_external_in(owner, &path))
        .await
        .map_err(|e| FsError::new(crate::domain::ErrorCode::Io, e.to_string()))?
}

/// File name of the third-party notices installed next to the app (FER-554, `bundle.resources`).
const THIRD_PARTY_NOTICES_FILE: &str = "THIRD-PARTY-NOTICES.txt";

/// Location of the installed third-party notices inside `resource_dir`. The path comes only from
/// the backend's own resource directory and a fixed file name; the webview supplies nothing.
fn bundled_notices_path(resource_dir: &Path) -> Result<String, FsError> {
    let path = resource_dir.join(THIRD_PARTY_NOTICES_FILE);
    if !path.is_file() {
        return Err(FsError::new(
            crate::domain::ErrorCode::NotFound,
            "The third-party notices file is not installed with this build",
        ));
    }
    let text = path.to_string_lossy().into_owned();
    // The resource dir can come back in extended-length form; the launch layer wants a plain path.
    if text.starts_with(r"\\?\UNC\") {
        return Ok(text);
    }
    Ok(text
        .strip_prefix(r"\\?\")
        .map(str::to_owned)
        .unwrap_or(text))
}

/// Open the installed third-party notices (a plain text file) with the OS default association
/// (FER-554). Takes no path: it is fixed by the backend, so the webview cannot use this command
/// to open anything else.
#[tauri::command]
pub async fn open_third_party_notices(
    app: AppHandle,
    window: tauri::Window,
) -> Result<(), FsError> {
    let dir = app
        .path()
        .resource_dir()
        .map_err(|e| FsError::new(crate::domain::ErrorCode::Io, e.to_string()))?;
    let path = bundled_notices_path(&dir)?;
    let owner = window_hwnd(&window);
    spawn_blocking(move || crate::shell_open::open_external_in(owner, &path))
        .await
        .map_err(|e| FsError::new(crate::domain::ErrorCode::Io, e.to_string()))?
}

/// Open a terminal at `cwd` (F2-25). `program` is the persisted per-OS
/// preference (`auto` | `wt` | `cmd` | `powershell`); a missing executable
/// surfaces as a structured `notFound` error for the UI to prompt.
#[tauri::command]
pub async fn open_terminal(cwd: String, program: Option<String>) -> Result<(), FsError> {
    validate_path(&cwd)?;
    let pref = crate::shell_open::TerminalPref::parse(program.as_deref().unwrap_or("auto"));
    spawn_blocking(move || crate::shell_open::spawn_terminal(pref, &cwd))
        .await
        .map_err(|e| FsError::new(crate::domain::ErrorCode::Io, e.to_string()))?
}

/// Open the OS "Associate With…" picker for one file (F2-25). The picker is
/// the explicit user action; Ferrum never changes a system association on
/// its own. Unsupported platforms report `unsupported`.
#[tauri::command]
pub async fn associate_with(path: String) -> Result<(), FsError> {
    validate_path(&path)?;
    spawn_blocking(move || crate::shell_open::associate_with(&path))
        .await
        .map_err(|e| FsError::new(crate::domain::ErrorCode::Io, e.to_string()))?
}

/// Print via the OS association's print verb (F2-27). No custom print
/// renderer: the associated application owns printing.
#[tauri::command]
pub async fn print_file(path: String) -> Result<(), FsError> {
    validate_path(&path)?;
    spawn_blocking(move || crate::shell_open::print_file(&path))
        .await
        .map_err(|e| FsError::new(crate::domain::ErrorCode::Io, e.to_string()))?
}

// ── F2-27: file comments (per-directory sidecar, see comments.rs) ───────

/// One file's comment, `None` when it has none.
#[tauri::command]
pub async fn comment_get(path: String) -> Result<Option<String>, FsError> {
    validate_path(&path)?;
    spawn_blocking(move || crate::comments::get_comment(&path))
        .await
        .map_err(|e| FsError::new(crate::domain::ErrorCode::Io, e.to_string()))?
}

/// The whole directory's comment map (name → text) for the Comments view.
#[tauri::command]
pub async fn comments_for_dir(
    dir: String,
) -> Result<std::collections::BTreeMap<String, String>, FsError> {
    validate_path(&dir)?;
    spawn_blocking(move || crate::comments::read_dir_comments(&dir))
        .await
        .map_err(|e| FsError::new(crate::domain::ErrorCode::Io, e.to_string()))?
}

// ── F2-27: Encode/Decode files (MIME base64, UUE/XXE — codecs.rs) ───────
/// File-producing work rides the operation queue for progress/cancel,
/// exactly like checksums and split/combine.

#[tauri::command]
pub fn enqueue_encode_files(
    paths: Vec<String>,
    queue: State<'_, Arc<OperationQueue>>,
) -> Result<OperationId, FsError> {
    check_write_targets(&paths)?;
    Ok(queue.enqueue(
        crate::domain::OperationKind::Create,
        Box::new(move |cancel, report| {
            let outcome = crate::codecs::encode_files(&paths, &|| cancel.is_cancelled());
            for (index, o) in outcome.outcomes.iter().enumerate() {
                report(preview(
                    o,
                    crate::domain::OperationKind::Create,
                    index + 1,
                    outcome.requested,
                ));
            }
            if cancel.is_cancelled() {
                return Err(FsError::new(
                    crate::domain::ErrorCode::Cancelled,
                    "cancelled",
                ));
            }
            report_failures(&outcome)
        }),
    ))
}

#[tauri::command]
pub fn enqueue_decode_files(
    paths: Vec<String>,
    queue: State<'_, Arc<OperationQueue>>,
) -> Result<OperationId, FsError> {
    check_write_targets(&paths)?;
    Ok(queue.enqueue(
        crate::domain::OperationKind::Create,
        Box::new(move |cancel, report| {
            let outcome = crate::codecs::decode_files(&paths, &|| cancel.is_cancelled());
            for (index, o) in outcome.outcomes.iter().enumerate() {
                report(preview(
                    o,
                    crate::domain::OperationKind::Create,
                    index + 1,
                    outcome.requested,
                ));
            }
            if cancel.is_cancelled() {
                return Err(FsError::new(
                    crate::domain::ErrorCode::Cancelled,
                    "cancelled",
                ));
            }
            report_failures(&outcome)
        }),
    ))
}

/// Shared failure summary for codec batches (first error surfaces).
fn report_failures(outcome: &crate::codecs::CodecReport) -> Result<(), FsError> {
    let failed: Vec<&crate::file_tools::ToolOutcome> = outcome
        .outcomes
        .iter()
        .filter(|o| o.status == "failed")
        .collect();
    if failed.is_empty() {
        return Ok(());
    }
    Err(FsError::new(
        crate::domain::ErrorCode::Io,
        format!(
            "{} of {} file(s) failed: {}",
            failed.len(),
            outcome.requested,
            failed[0].message.as_deref().unwrap_or("error")
        ),
    ))
}

/// Set (Some) or clear (None/empty) one file's comment. Sidecar writes are
/// atomic; an empty map removes the sidecar file.
#[tauri::command]
pub async fn comment_set(path: String, comment: Option<String>) -> Result<(), FsError> {
    let path = prepare_source(&path)?;
    // The comment is stored in a sidecar (descript.ion) next to the item: a write into its folder.
    if let Some(parent) = Path::new(&crate::path_guard::strip_verbatim(&path)).parent() {
        crate::startup_guard::check_into(&parent.to_string_lossy())?;
    }
    spawn_blocking(move || crate::comments::set_comment(&path, comment.as_deref()))
        .await
        .map_err(|e| FsError::new(crate::domain::ErrorCode::Io, e.to_string()))?
}

// ── F2-26: Background Transfer Manager ───────────────────────────────────

/// Retry a FAILED operation (F2-26 BTM): re-enqueues a fresh copy of the job
/// via its stored factory under a NEW operation id; the failed entry stays
/// in the history. Returns None for unknown/active/non-failed/non-retryable
/// ids — the frontend renders no retry control for those (no fake buttons).
#[tauri::command]
pub fn retry_operation(id: String, queue: State<'_, Arc<OperationQueue>>) -> Option<OperationId> {
    queue.retry(&id)
}

// ── FER-407 regression: snapshot queryable the moment `done` arrives ──────

#[cfg(test)]
mod fer407_tests {
    use super::*;
    use crate::disk_usage::{self, DiskUsageEvent, ScanOptions};
    use std::sync::Mutex;

    /// Small temp fixture with one file — enough to finish a scan.
    fn fixture() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ferrum-fer407-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("a.txt"), b"x").expect("write");
        dir
    }

    /// FER-407 regression (failed before the fix): the final done event must
    /// only reach the frontend AFTER the finished snapshot is queryable via
    /// `disk_usage_snapshot`. Pre-fix order — done emitted from inside the
    /// walk sink, snapshot stored afterwards (with a full deep clone between
    /// the two) — let the frontend's refreshSnapshot query land in that
    /// window, get NotFound, and declare a finished scan's results gone
    /// (needsRescan). The window widens with tree size (worst on 100k+ entry
    /// roots like `C:\Users\you`), which is exactly the reported bug.
    #[test]
    fn finish_scan_stores_snapshot_before_done_is_emitted() {
        let dir = fixture();
        let root = dir.to_string_lossy().into_owned();
        let scan_id = disk_usage::new_scan_id();
        let final_event: Arc<Mutex<Option<DiskUsageEvent>>> = Arc::new(Mutex::new(None));
        let slot = Arc::clone(&final_event);
        // Same capture the command's sink does: hold done back, drop progress.
        let result = disk_usage::run_scan_with_throttle(
            &root,
            &ScanOptions::default(),
            &AtomicBool::new(false),
            &scan_id,
            None,
            move |ev: &DiskUsageEvent| {
                if ev.done {
                    *slot.lock().expect("done slot") = Some(ev.clone());
                }
            },
        );
        // Registered like the command does, to also prove deregistration.
        disk_usage::active_scans()
            .lock()
            .expect("registry")
            .insert(scan_id.clone(), Arc::new(AtomicBool::new(false)));

        let mut queryable_at_done = false;
        let mut done_emitted = false;
        finish_scan(&scan_id, result, final_event, |ev: &DiskUsageEvent| {
            if ev.done {
                done_emitted = true;
                // What the frontend's refreshSnapshot sees the moment done
                // arrives — this is the invariant FER-407 is about.
                queryable_at_done = disk_usage::snapshot(&scan_id).is_some();
            }
        });

        assert!(done_emitted, "the one final done event is still emitted");
        assert!(
            queryable_at_done,
            "FER-407: the snapshot must already be queryable when done reaches the frontend"
        );
        assert!(
            disk_usage::snapshot(&scan_id).is_some(),
            "stored after finish_scan"
        );
        assert!(
            disk_usage::active_scans()
                .lock()
                .expect("registry")
                .get(&scan_id)
                .is_none(),
            "the worker deregisters the finished scan"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The error path (scan could not run at all) still emits exactly one
    /// final done event carrying the error — unchanged by the FER-407 fix.
    #[test]
    fn finish_scan_err_path_emits_one_final_done_with_error() {
        let scan_id = disk_usage::new_scan_id();
        let events: Mutex<Vec<DiskUsageEvent>> = Mutex::new(Vec::new());
        let err = FsError::new(crate::domain::ErrorCode::NotADirectory, "not a directory");
        finish_scan(
            &scan_id,
            Err(err),
            Arc::new(Mutex::new(None)),
            |ev: &DiskUsageEvent| {
                events.lock().expect("events").push(ev.clone());
            },
        );
        let events = events.into_inner().expect("events");
        assert_eq!(events.len(), 1, "exactly one final event");
        assert!(events[0].done);
        assert!(events[0].error.is_some());
        assert_eq!(events[0].scan_id, scan_id);
        assert!(
            disk_usage::snapshot(&scan_id).is_none(),
            "a failed scan stores no snapshot"
        );
    }
}

/// Root-identity gate for Synchronize Directories: rejects same, nested or
/// aliased roots before any plan is built or executed (read-only).
#[tauri::command]
pub async fn validate_sync_roots(left: String, right: String) -> Result<(), FsError> {
    spawn_blocking(move || crate::compare::validate_sync_roots(&left, &right))
        .await
        .map_err(|e| FsError::new(crate::domain::ErrorCode::Io, e.to_string()))?
}

/// Tests for the `enqueue_*` command bodies against a real `OperationQueue`.
/// The `#[tauri::command]` wrappers only deref `State`; building a genuine
/// `State` needs tauri's `test` feature, which makes the Windows lib test
/// binary fail to start (STATUS_ENTRYPOINT_NOT_FOUND, no manifest), so the
/// logic lives in `*_on` functions that take the queue directly.
#[cfg(test)]
mod enqueue_command_tests {
    use super::*;
    use crate::domain::{ErrorCode, OperationProgress, OperationState};

    fn wait_terminal(queue: &OperationQueue, id: &str) -> OperationProgress {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            if let Some(p) = queue.snapshot().into_iter().find(|p| p.operation_id == id)
                && !matches!(p.state, OperationState::Queued | OperationState::Running)
            {
                return p;
            }
            assert!(std::time::Instant::now() < deadline, "queue timed out");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ferrum-ipc-enqueue-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn lossy(p: &std::path::Path) -> String {
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn checksum_create_validates_input_before_anything_is_queued() {
        let queue = OperationQueue::new(Box::new(|_| {}));
        let bad_path = enqueue_checksum_create_on(&queue, vec![" ".into()], "sha256", None);
        assert_eq!(bad_path.unwrap_err().code, ErrorCode::InvalidPath);
        assert!(enqueue_checksum_create_on(&queue, vec!["a".into()], "sha999", None).is_err());
        let bad_policy =
            enqueue_checksum_create_on(&queue, vec!["a".into()], "sha256", Some("bogus".into()));
        assert!(bad_policy.is_err());
        assert!(
            queue.snapshot().is_empty(),
            "rejected calls must not enqueue"
        );
    }

    #[test]
    fn checksum_create_enqueues_a_job_that_completes_and_writes_the_sidecar() {
        let queue = OperationQueue::new(Box::new(|_| {}));
        let dir = temp_dir("create");
        let file = dir.join("f.bin");
        std::fs::write(&file, b"hello").unwrap();
        let id = enqueue_checksum_create_on(&queue, vec![lossy(&file)], "sha256", None).unwrap();
        let done = wait_terminal(&queue, &id);
        assert_eq!(done.state, OperationState::Completed, "{done:?}");
        assert!(dir.join("f.bin.sha256").exists());
        // The sidecar now exists: the default policy fails the job, and the
        // wrapper registered a factory so the BTM can retry it.
        let again = enqueue_checksum_create_on(&queue, vec![lossy(&file)], "sha256", None).unwrap();
        assert_eq!(wait_terminal(&queue, &again).state, OperationState::Failed);
        assert!(
            queue.retry(&again).is_some(),
            "wrapper must register a factory"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn split_and_combine_reject_bad_input_without_queueing() {
        let queue = OperationQueue::new(Box::new(|_| {}));
        let split = enqueue_split_file_on(&queue, "".into(), 10, "d".into(), None);
        assert_eq!(split.unwrap_err().code, ErrorCode::InvalidPath);
        let split_policy =
            enqueue_split_file_on(&queue, "f".into(), 10, "d".into(), Some("x".into()));
        assert!(split_policy.is_err());
        let combine = enqueue_combine_parts_on(&queue, "f.001".into(), "".into(), None);
        assert_eq!(combine.unwrap_err().code, ErrorCode::InvalidPath);
        assert!(queue.snapshot().is_empty());
    }

    #[test]
    fn split_then_combine_round_trip_through_the_command_bodies() {
        let queue = OperationQueue::new(Box::new(|_| {}));
        let dir = temp_dir("split");
        let file = dir.join("s.bin");
        let data: Vec<u8> = (0..3000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&file, &data).unwrap();
        let id = enqueue_split_file_on(&queue, lossy(&file), 1024, lossy(&dir), None).unwrap();
        let done = wait_terminal(&queue, &id);
        assert_eq!(done.state, OperationState::Completed, "{done:?}");
        assert!(dir.join("s.bin.001").exists());

        let out = dir.join("s.out");
        let id = enqueue_combine_parts_on(&queue, lossy(&dir.join("s.bin.001")), lossy(&out), None)
            .unwrap();
        let combined = wait_terminal(&queue, &id);
        assert_eq!(combined.state, OperationState::Completed, "{combined:?}");
        assert_eq!(std::fs::read(&out).unwrap(), data);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod preflight_request_debug_tests {
    use super::ArchiveExtractPreflightRequest;

    #[test]
    fn debug_masks_the_password() {
        let mut request = ArchiveExtractPreflightRequest {
            archive_path: r"C:\a\enc.rar".into(),
            member_paths: Vec::new(),
            destination_path: r"C:\out".into(),
            password: Some("hunter2-secret".into()),
        };
        let shown = format!("{request:?}");
        assert!(!shown.contains("hunter2-secret"), "{shown}");
        assert!(shown.contains(r#"password: Some("***")"#), "{shown}");
        request.password = None;
        assert!(format!("{request:?}").contains("password: None"));
    }
}

/// FER-512 P3-3 / Info-1: the mutation guard and the queue-harness cap.
#[cfg(test)]
mod fer512_ipc_gate_tests {
    use super::{check_clipboard_source, check_mutation_path, check_write_sources, dummy_ticks};
    use crate::domain::ErrorCode;

    /// The exact audit cases: an ADS spelling and a device namespace are
    /// refused at the command boundary.
    #[test]
    fn clipboard_sources_are_validated_like_manual_sources() {
        // FER-572: a path list taken from the clipboard comes from another process.
        let dir = std::env::temp_dir().join(format!("ferrum-clip-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.txt");
        std::fs::write(&file, b"x").unwrap();
        let ok = check_clipboard_source(file.to_str().unwrap(), false).unwrap();
        assert!(ok.ends_with("a.txt"));
        // Cut applies the protected-folder source gate as well; a plain temp file passes it.
        assert!(check_clipboard_source(file.to_str().unwrap(), true).is_ok());

        for bad in [
            r"relative\path.txt",
            r"\\.\PhysicalDrive0",
            r"\\?\GLOBALROOT\Device\X",
            r"\\?\C:\Windows\notepad.exe",
            r"C:\Windows\notepad.exe:ads",
            "C:\\Windows\\a\0b",
            r"C:\Windows\CON",
            r"C:\x\NUL.txt",
            r"\\localhost\C$\Windows",
            r"\\127.0.0.1\C$\Windows",
            "",
        ] {
            assert!(
                check_clipboard_source(bad, false).is_err(),
                "should be refused: {bad:?}"
            );
        }
        // Well-formed but missing.
        assert_eq!(
            check_clipboard_source(dir.join("missing.txt").to_str().unwrap(), false)
                .unwrap_err()
                .code,
            crate::domain::ErrorCode::NotFound
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mutation_paths_refuse_ads_and_device_namespaces() {
        for bad in [
            r"C:\x.txt:ads",
            r"\\.\C:",
            r"\\?\C:\x",
            r"C:\a:b\c",
            "relative",
            "",
        ] {
            let err = check_mutation_path(bad).unwrap_err();
            assert_eq!(err.code, ErrorCode::InvalidPath, "{bad:?}");
        }
        assert!(check_write_sources(&[r"C:\ok.txt".into()]).is_ok());
        let err = check_write_sources(&[r"C:\ok.txt".into(), r"C:\x.txt:ads".into()]).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidPath);
    }

    /// Plain UNC and long-but-well-formed drive paths are NOT refused by the
    /// guard (Ferrum browses shares; `\\?\` long paths stay the concern of the
    /// read guard that already accepts what the UI produces).
    #[test]
    fn mutation_paths_keep_plain_unc_and_drive_paths() {
        for good in [
            r"\\server\share\file.txt",
            r"\\SERVER\share\folder\a b.txt",
            r"C:\Users\u\x.txt",
            r"c:/mixed/slashes.txt",
        ] {
            assert!(check_mutation_path(good).is_ok(), "{good}");
        }
    }

    /// FER-515 P3-2: reserved device names are refused anywhere in a mutation path (any case,
    /// with extension, trailing dot/space), while look-alikes stay allowed.
    #[test]
    fn mutation_paths_refuse_reserved_device_names() {
        for bad in [
            r"C:\x\CON",
            r"C:\x\NUL.txt",
            r"C:\x\nul.tar.gz",
            r"C:\AUX\a.txt",
            r"C:\x\com1",
            r"C:\x\LPT9.log",
            r"C:\x\PRN.",
            r"C:\x\CON .txt",
            r"C:\x\CONIN$",
            r"\\server\share\dir\CON",
            r"c:/x/Aux",
        ] {
            let err = check_mutation_path(bad).unwrap_err();
            assert_eq!(err.code, ErrorCode::InvalidPath, "{bad:?}");
        }
        for good in [
            r"C:\x\console.txt",
            r"C:\x\com10",
            r"C:\x\COM0",
            r"C:\x\nulled",
            r"C:\x\auxiliary\a.txt",
            r"\\CON\share\file.txt",
            r"C:\x\lpt",
        ] {
            assert!(check_mutation_path(good).is_ok(), "{good}");
        }
    }

    /// FER-515 P3-3: names of new entries cannot carry an ADS or be a device name.
    #[test]
    fn new_entry_names_refuse_streams_and_devices() {
        for bad in ["a.txt:ads", "x:y", "CON", "nul.txt", "a/b", "..", ""] {
            assert!(super::check_new_name(bad).is_err(), "{bad:?}");
        }
        for good in ["a.txt", "new folder", "console", ".gitignore"] {
            assert!(super::check_new_name(good).is_ok(), "{good}");
        }
    }

    /// FER-514 P3-7: the write gates refuse the app's own folders (target: inside; source:
    /// inside or containing) and keep everything else.
    #[test]
    fn write_gates_protect_the_app_state_folder() {
        let _guard = crate::protected_dirs::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let base = std::env::temp_dir().join(format!("ferrum-ipc-pd-{}", std::process::id()));
        let cfg = base.join("config");
        let other = base.join("other");
        std::fs::create_dir_all(&cfg).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        crate::protected_dirs::register([cfg.clone()]);
        let s = |p: &std::path::Path| p.to_string_lossy().into_owned();
        let in_cfg = s(&cfg.join("config.json"));
        // New entry / destination.
        assert!(super::check_write_target(&in_cfg).is_err());
        assert!(super::check_write_target(&s(&cfg)).is_err());
        assert!(super::check_write_target(&s(&other)).is_ok());
        assert!(super::check_protected_target(&s(&cfg)).is_err());
        // Rename / delete / move source.
        assert!(super::check_write_source(&in_cfg).is_err());
        assert!(super::check_write_source(&s(&base)).is_err(), "ancestor");
        assert!(super::check_write_source(&s(&other.join("a.txt"))).is_ok());
        assert!(crate::protected_dirs::check_write_source(&s(&base)).is_err());
        assert!(super::check_write_sources(&[s(&other), in_cfg.clone()]).is_err());
        assert!(super::check_write_targets(&[s(&other), s(&base)]).is_ok());
        // The string gate still applies first.
        assert!(super::check_write_target(r"C:\x.txt:ads").is_err());
        crate::protected_dirs::register(Vec::<std::path::PathBuf>::new());
        let _ = std::fs::remove_dir_all(&base);
    }

    /// FER-562: the manual "Synchronize Directories" flow writes through `enqueue_copy` (one call per
    /// destination folder of the plan) and the delete dialog (`delete_entries`). Both start with the gates
    /// below, so a plan whose destination folder lands in the app's state or in Startup, however
    /// deep below the chosen root, is refused before anything is queued.
    #[test]
    fn manual_synchronize_destinations_go_through_the_same_gates() {
        let _guard = crate::protected_dirs::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let base = std::env::temp_dir().join(format!("ferrum-ipc-sync-{}", std::process::id()));
        let cfg = base.join("state");
        std::fs::create_dir_all(cfg.join("sub")).unwrap();
        std::fs::create_dir_all(base.join("right")).unwrap();
        crate::protected_dirs::register([cfg.clone()]);
        let s = |p: &std::path::Path| p.to_string_lossy().into_owned();
        // `right` is the sync root; the plan has batches for `right\...` folders and, when the root
        // is the common parent, for the protected folder below it.
        assert!(super::check_protected_target(&s(&base.join("right").join("a"))).is_ok());
        for dest in [
            s(&cfg),
            s(&cfg.join("sub")),
            s(&cfg.join("new").join("deep")),
        ] {
            assert!(super::check_protected_target(&dest).is_err(), "{dest}");
        }
        assert!(super::check_write_sources(&[s(&cfg.join("sub").join("f.txt"))]).is_err());
        #[cfg(windows)]
        {
            let startup = std::env::var("APPDATA").unwrap()
                + r"\Microsoft\Windows\Start Menu\Programs\Startup";
            assert!(super::check_protected_target(&startup).is_err());
            assert!(super::check_protected_target(&format!(r"{startup}\sub")).is_err());
        }
        crate::protected_dirs::register(Vec::<std::path::PathBuf>::new());
        let _ = std::fs::remove_dir_all(&base);
    }

    /// FER-585: the async delete starts behind the same gates as the old synchronous one.
    #[test]
    fn delete_job_gate_keeps_the_path_guards() {
        let _guard = crate::protected_dirs::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        for bad in [
            r"\\.\pipe\x",
            r"\\?\GLOBALROOT\Device\NamedPipe\x",
            r"C:\x:ads",
            "relative",
            r"\\localhost\C$\x",
        ] {
            assert!(
                super::gate_delete_job(&[bad.to_string()], "t-gate-bad").is_err(),
                "{bad}"
            );
        }
        // One refused path refuses the whole job, even behind valid ones.
        assert!(
            super::gate_delete_job(
                &[r"C:\Users\u\a.txt".to_string(), "relative".to_string()],
                "t-gate-mixed"
            )
            .is_err()
        );
        // A reserved device name that is not a plain existing entry is the device: refused.
        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().to_string_lossy().into_owned();
        assert!(
            super::gate_delete_job(&[format!(r"{plain}\prn")], "t-gate-prn").is_err(),
            "reserved name that is not a plain entry"
        );
        // The app's own state folders are protected: neither inside nor containing them.
        let cfg = dir.path().join("cfg");
        std::fs::create_dir_all(cfg.join("sub")).unwrap();
        crate::protected_dirs::register(vec![cfg.clone()]);
        let inside = cfg.join("sub").to_string_lossy().into_owned();
        let own = cfg.to_string_lossy().into_owned();
        let refused =
            [inside, own, plain].map(|p| super::gate_delete_job(&[p], "t-gate-prot").is_err());
        crate::protected_dirs::register(Vec::<std::path::PathBuf>::new());
        assert_eq!(refused, [true, true, true]);
    }

    /// The wire shape the frontend listens for.
    #[test]
    fn delete_events_have_the_documented_shape() {
        let progress = crate::delete_jobs::DeleteProgress {
            done: 1,
            total: 4,
            current_name: Some("big".into()),
            entries: 7,
            bytes: 4096,
        };
        let v = serde_json::to_value(super::DeleteProgressEvent {
            request_id: "del-1",
            progress: &progress,
        })
        .unwrap();
        assert_eq!(
            v,
            serde_json::json!({"requestId":"del-1","done":1,"total":4,"currentName":"big","entries":7,"bytes":4096})
        );
        let result = crate::delete_jobs::run_permanent(&[], &|| false, &mut |_| {});
        let v = serde_json::to_value(super::DeleteDoneEvent {
            request_id: "del-1",
            result: Some(&result),
            error: None,
        })
        .unwrap();
        assert_eq!(v["requestId"], "del-1");
        assert_eq!(v["result"]["cancelled"], false);
        assert_eq!(v["result"]["report"]["requested"], 0);
        assert!(v["result"]["notProcessed"].is_array() && v["result"]["partial"].is_array());
        assert!(v.get("error").is_none());
    }

    #[test]
    fn delete_job_gate_refuses_a_request_id_that_is_in_use_or_unusable() {
        let ok = tempfile::tempdir().unwrap();
        let path = ok.path().join("x.txt").to_string_lossy().into_owned();
        let first =
            super::gate_delete_job(std::slice::from_ref(&path), "t-gate-dup").expect("free id");
        assert!(super::gate_delete_job(std::slice::from_ref(&path), "t-gate-dup").is_err());
        // Cancel reaches the registered job.
        super::delete_cancel("t-gate-dup".to_string());
        assert!(first.1.token().is_cancelled());
        drop(first);
        assert!(super::gate_delete_job(std::slice::from_ref(&path), "t-gate-dup").is_ok());
        assert!(super::gate_delete_job(std::slice::from_ref(&path), "").is_err());
        assert!(super::gate_delete_job(std::slice::from_ref(&path), &"x".repeat(129)).is_err());
    }

    /// Rework point 1: pipes, device namespaces, ADS and GLOBALROOT are refused for every source
    /// and destination of copy / move / archive; ordinary paths still pass.
    #[test]
    fn copy_and_move_paths_refuse_pipes_ads_and_device_namespaces() {
        let _guard = crate::protected_dirs::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        for bad in [
            r"\\.\pipe\x",
            r"\\?\GLOBALROOT\Device\NamedPipe\x",
            r"C:\x:ads",
            r"\\?\C:\x",
            "relative",
        ] {
            assert!(super::prepare_read_source(bad).is_err(), "source {bad}");
            assert!(super::check_protected_target(bad).is_err(), "target {bad}");
            assert!(super::prepare_source(bad).is_err(), "move source {bad}");
        }
        for good in [r"C:\Users\u\a.txt", r"\\server\share\dir"] {
            assert!(super::prepare_read_source(good).is_ok(), "{good}");
            assert!(super::check_protected_target(good).is_ok(), "{good}");
        }
    }

    /// Rework point 3: UNC spellings of this machine and administrative shares are classified.
    #[test]
    fn unc_paths_to_this_machine_are_classified() {
        use crate::path_guard::unc_reaches_local_machine as local;
        let me = &[
            "DESKTOP-ABC".to_string(),
            "desktop-abc.corp.example".to_string(),
        ][..];
        for bad in [
            r"\\localhost\C$\x",
            r"\\LOCALHOST\share\x",
            r"\\127.0.0.1\share\x",
            r"\\127.1.2.3\share",
            r"\\0.0.0.0\share",
            r"\\desktop-abc\share\x",
            r"\\DESKTOP-ABC.corp.example\share\x",
            r"\\otherhost\C$\x",
            r"\\otherhost\ADMIN$\x",
            r"\\otherhost\ipc$",
            r"\\otherhost\PRINT$\x",
            r"\\server\d$",
            r"\\localhost.localdomain\share",
            r"\\.\C:\x",
            r"\\2130706433\share",
            r"\\0x7f000001\share",
            r"\\0177.0.0.1\share",
            r"\\0x7f.1\share",
            r"\\017700000001\share",
            r"\\0\share",
            r"\\fe80--1.ipv6-literal.net\share",
            r"\\localhost\home$\x",
            r"\\desktop-abc\backup$",
        ] {
            assert!(local(bad, me), "{bad}");
        }
        for good in [
            r"\\server\share\x",
            r"\\desktop-abcd\share",
            r"\\10.0.0.5\share\dollar",
            r"C:\Users\u",
            r"\\fileserver.corp\data$like\x",
            r"\\fs\home$\user",
            r"\\nas\backup$",
            r"\\nas\ab$",
            r"\\192.168.1.9\share",
            r"\\3232235777\share",
            r"\\cafe\share",
        ] {
            assert!(!local(good, me), "{good}");
        }
        assert!(
            !local(r"\\desktop-abc\share", &[]),
            "no computer name known"
        );
        // Wired into the mutation gate.
        assert!(check_mutation_path(r"\\localhost\C$\Users\u\a.txt").is_err());
        assert!(check_mutation_path(r"\\server\share\a.txt").is_ok());
    }

    /// Rework point 2: an EXISTING plain file or folder with a reserved name (made by WSL/bash)
    /// can still be renamed and deleted, through its `\\?\` form; the device itself, a missing
    /// entry and a NEW entry with such a name are refused.
    #[cfg(windows)]
    #[test]
    fn existing_reserved_name_entries_can_be_deleted_and_renamed() {
        let _guard = crate::protected_dirs::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("ferrum-ipc-nul-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let plain = dir.to_string_lossy().into_owned();
        let verbatim_dir = crate::path_guard::verbatim_path(&plain);
        let make = |name: &str| {
            let p = format!(r"{verbatim_dir}\{name}");
            std::fs::write(&p, b"made by bash").unwrap();
        };
        make("nul");
        make("con.txt");
        make("aux.c");

        // Never created: still refused (it would be the device).
        assert!(super::prepare_source(&format!(r"{plain}\prn")).is_err());
        // The device itself is not a plain file: the bare drive-relative NUL is refused too.
        assert!(super::check_mutation_path(&format!(r"{plain}\nul")).is_err());

        // delete (permanent) through the prepared path
        let nul = super::prepare_source(&format!(r"{plain}\nul")).expect("existing nul allowed");
        assert!(nul.starts_with(r"\\?\"), "{nul}");
        let report = crate::mutations::delete_permanent(&[nul]);
        assert_eq!(report.deleted, 1, "{:?}", report.failed);
        assert!(!std::path::Path::new(&format!(r"{verbatim_dir}\nul")).exists());

        // N2: a verbatim source applies the portable-name rules to the NEW name.
        let aux = super::prepare_source(&format!(r"{plain}\aux.c")).unwrap();
        for bad in ["x. ", "x.", "x ", "a<b", "q?", "CON", "w|z"] {
            assert!(
                crate::mutations::rename_entry(&aux, bad).is_err(),
                "{bad:?} must be refused"
            );
        }
        assert!(std::path::Path::new(&format!(r"{verbatim_dir}\aux.c")).exists());

        // rename source with a reserved name to an ordinary name
        let con = super::prepare_source(&format!(r"{plain}\con.txt")).unwrap();
        let entry = crate::mutations::rename_entry(&con, "renamed.txt").expect("rename");
        // N3: the result is the plain spelling, never `\\?\`.
        assert!(!entry.path.starts_with(r"\\?\"), "{}", entry.path);
        assert!(entry.path.ends_with("renamed.txt"), "{}", entry.path);
        assert!(std::path::Path::new(&format!(r"{plain}\renamed.txt")).exists());

        // the new name may not be reserved; creating `con.txt` fresh is refused
        assert!(super::check_new_name("con.txt").is_err());
        assert!(super::check_new_name("aux.c").is_err());
        let _ = std::fs::remove_dir_all(&verbatim_dir);
    }

    /// FER-559 N5: a plain file named like a device (made by WSL/bash) is copied and moved through
    /// the `\?\` form with its content; the device itself, a missing entry and a reserved child
    /// inside a plainly spelled tree are refused and no device is ever opened.
    #[cfg(windows)]
    #[test]
    fn copy_and_move_read_existing_reserved_name_files_through_verbatim_form() {
        use crate::copy_engine::{ConflictPolicy, run_copy};
        let _guard = crate::protected_dirs::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // FER-568: removed when the test ends, `nul` entries included.
        let tmp = crate::sync_engine::test_support::ReservedTempDir::new("ferrum-ipc-n5-");
        let root = tmp.to_path_buf();
        let src_dir = root.join("src");
        let dst_dir = root.join("dst");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::create_dir_all(&dst_dir).unwrap();
        let src = src_dir.to_string_lossy().into_owned();
        let dst = dst_dir.to_string_lossy().into_owned();
        let vsrc = crate::path_guard::verbatim_path(&src);
        std::fs::write(format!(r"{vsrc}\nul"), b"real file named nul").unwrap();
        std::fs::write(format!(r"{vsrc}\plain.txt"), b"plain").unwrap();
        let cancel = crate::ops::CancelToken::default();

        // Never created: the device. Refused, nothing opened.
        assert!(super::prepare_copy_io(&[format!(r"{src}\COM1")], &dst).is_err());
        assert!(super::prepare_copy_io(&[format!(r"{src}\CONIN$")], &dst).is_err());
        assert!(super::prepare_copy_io(&[format!(r"{src}\prn.txt")], &dst).is_err());

        // copy: the content arrives, in a file really named nul (verbatim target).
        let (srcs, target) = super::prepare_copy_io(&[format!(r"{src}\nul")], &dst).unwrap();
        assert!(srcs[0].starts_with(r"\\?\") && target.starts_with(r"\\?\"));
        let out = run_copy(&srcs, &target, ConflictPolicy::Overwrite, &cancel, &|_| {});
        assert!(out.failures.is_empty(), "{:?}", out.failures);
        assert_eq!(
            std::fs::read(format!(r"{}\nul", crate::path_guard::verbatim_path(&dst))).unwrap(),
            b"real file named nul"
        );

        // The engine itself refuses a reserved name in the plain spelling (no device write).
        let plain_src = format!(r"{src}\nul");
        let out = run_copy(
            std::slice::from_ref(&plain_src),
            &dst,
            ConflictPolicy::Overwrite,
            &cancel,
            &|_| {},
        );
        assert_eq!(out.failures.len(), 1, "{:?}", out.failures);

        // A reserved child inside a plain tree is a per-item failure; its siblings still copy.
        let tree = root.join("tree");
        std::fs::create_dir_all(&tree).unwrap();
        let vtree = crate::path_guard::verbatim_path(&tree.to_string_lossy());
        std::fs::write(format!(r"{vtree}\nul"), b"x").unwrap();
        std::fs::write(format!(r"{vtree}\ok.txt"), b"y").unwrap();
        let out = run_copy(
            &[tree.to_string_lossy().into_owned()],
            &dst,
            ConflictPolicy::Overwrite,
            &cancel,
            &|_| {},
        );
        assert_eq!(out.failures.len(), 1, "{:?}", out.failures);
        assert!(dst_dir.join("tree").join("ok.txt").exists());

        // move: same-volume rename keeps the verbatim name.
        let moved = root.join("moved");
        std::fs::create_dir_all(&moved).unwrap();
        let (srcs, target) =
            super::prepare_copy_io(&[format!(r"{src}\nul")], &moved.to_string_lossy()).unwrap();
        let mv = crate::move_engine::run_move(
            &srcs,
            &target,
            ConflictPolicy::Overwrite,
            &cancel,
            &|_| {},
            false,
        );
        assert!(mv.failures.is_empty(), "{:?}", mv.failures);
        assert_eq!(
            std::fs::read(format!(
                r"{}\nul",
                crate::path_guard::verbatim_path(&moved.to_string_lossy())
            ))
            .unwrap(),
            b"real file named nul"
        );
        assert!(!std::path::Path::new(&format!(r"{vsrc}\nul")).exists());
    }

    /// FER-559 N4: the Recycle Bin cannot take an entry with a reserved name via `\?\`
    /// (probed: "device name is invalid"). The entry is reported as failed, with the way out, and
    /// stays untouched; permanent delete of the same entry works.
    #[cfg(windows)]
    #[test]
    fn trash_of_reserved_name_entry_is_refused_with_advice_and_permanent_delete_works() {
        let _guard = crate::protected_dirs::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("ferrum-n4-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let plain = dir.to_string_lossy().into_owned();
        let v = crate::path_guard::verbatim_path(&plain);
        std::fs::write(format!(r"{v}\nul"), b"x").unwrap();
        let prepared = super::prepare_source(&format!(r"{plain}\nul")).unwrap();
        let r = crate::mutations::delete_to_trash(std::slice::from_ref(&prepared));
        assert_eq!(r.deleted, 0);
        assert_eq!(r.failed.len(), 1);
        assert!(
            r.failed[0].message.contains("permanent delete"),
            "{:?}",
            r.failed
        );
        assert!(std::path::Path::new(&prepared).exists(), "untouched");
        let r = crate::mutations::delete_permanent(std::slice::from_ref(&prepared));
        assert_eq!(r.deleted, 1, "{:?}", r.failed);
        assert!(!std::path::Path::new(&prepared).exists());
        let _ = std::fs::remove_dir_all(&v);
    }

    /// FER-559 P3-C (reworked): the file manager writes NOTHING into the Startup folders. Every
    /// attempt below is refused before anything is created; the directory is left as it was.
    #[cfg(windows)]
    #[test]
    fn nothing_is_written_into_the_startup_folder() {
        use crate::copy_engine::{ConflictPolicy, run_copy};
        let _guard = crate::protected_dirs::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let startup =
            crate::profile::known_folder(&windows_sys::Win32::UI::Shell::FOLDERID_Startup)
                .expect("startup folder");
        let programs = startup.parent().unwrap().to_path_buf();
        let startup_s = startup.to_string_lossy().into_owned();
        let programs_s = programs.to_string_lossy().into_owned();
        let list = || -> Vec<_> {
            std::fs::read_dir(&startup)
                .unwrap()
                .flatten()
                .map(|e| e.file_name())
                .collect()
        };
        let before = list();
        let probe = "ferrum-test-do-not-keep.txt";

        // create (a plain text file too) and a folder named like Startup next to it
        for name in ["x.bat", "y.CMD", "z.lnk", "w.ps1.", probe] {
            let err =
                super::create_entry("file".into(), startup_s.clone(), name.into()).unwrap_err();
            assert!(err.message.contains("Startup"), "{name}: {}", err.message);
        }
        let err = super::create_entry("dir".into(), programs_s.clone(), "Startup".into());
        assert!(err.is_err(), "a folder named Startup in Programs");

        // gates for destinations: copy / move / extract / archive / split / selection
        assert!(super::check_protected_target(&startup_s).is_err());
        assert!(super::check_protected_target(&format!(r"{startup_s}\sub\{probe}")).is_err());
        assert!(super::check_write_target(&format!(r"{startup_s}\a.zip")).is_err());

        // rename: into / to Startup, or onto an ancestor
        assert!(crate::startup_guard::check_rename_to(&format!(r"{startup_s}\{probe}")).is_err());
        assert!(crate::startup_guard::check_rename_to(&startup_s).is_err());
        assert!(crate::startup_guard::check_rename_to(&programs_s).is_err());
        assert!(crate::startup_guard::check_rename_to(&format!(r"{programs_s}\Other")).is_ok());

        // a folder tree `Startup\<probe>` copied or moved into Programs
        let root = std::env::temp_dir().join(format!("ferrum-startup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let tree = root.join("Startup");
        std::fs::create_dir_all(&tree).unwrap();
        std::fs::write(tree.join(probe), b"x").unwrap();
        let cancel = crate::ops::CancelToken::default();
        let src = vec![tree.to_string_lossy().into_owned()];
        let out = run_copy(
            &src,
            &programs_s,
            ConflictPolicy::Overwrite,
            &cancel,
            &|_| {},
        );
        assert!(!out.failures.is_empty(), "copy into Programs must fail");
        let mv = crate::move_engine::run_move(
            &src,
            &programs_s,
            ConflictPolicy::Overwrite,
            &cancel,
            &|_| {},
            false,
        );
        assert!(!mv.failures.is_empty(), "move into Programs must fail");
        assert!(tree.join(probe).exists(), "the source is untouched");
        // a single file straight into Startup
        let file = vec![tree.join(probe).to_string_lossy().into_owned()];
        let out = run_copy(
            &file,
            &startup_s,
            ConflictPolicy::Overwrite,
            &cancel,
            &|_| {},
        );
        assert!(!out.failures.is_empty());

        // reading, opening and deleting stay allowed: a source inside Startup passes the gate
        assert!(super::prepare_source(&format!(r"{startup_s}\{probe}")).is_ok());

        assert_eq!(before, list(), "Startup was not changed");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// FER-559: an archive entry `Startup/...` extracted into `Programs` would land in the
    /// Startup folder: refused before anything is written; an unrelated entry is not.
    #[cfg(windows)]
    #[test]
    fn archive_entries_that_reach_the_startup_folder_are_refused() {
        use crate::archive_contract::{ArchiveConflictPolicy, ArchiveErrorCode, ArchiveLimits};
        use crate::archive_extract::{Entry, EntryKind, extract, preflight};
        crate::install_extract_guard();
        let startup =
            crate::profile::known_folder(&windows_sys::Win32::UI::Shell::FOLDERID_Startup)
                .expect("startup folder");
        let programs = startup.parent().unwrap().to_path_buf();
        let limits = ArchiveLimits {
            max_entries: 100,
            max_total_bytes: 1 << 20,
            max_entry_bytes: 1 << 20,
            max_depth: 8,
        };
        let plan_for = |names: &[(&str, EntryKind)]| {
            let entries: Vec<Entry> = names
                .iter()
                .map(|(n, kind)| Entry {
                    raw_name: n,
                    kind: *kind,
                    expanded_size: None,
                    compressed_size: None,
                    modified_ms: None,
                })
                .collect();
            preflight(&entries, &limits).unwrap()
        };
        let run = |plan: &crate::archive_extract::Plan, dest: &std::path::Path| {
            extract(
                plan,
                dest,
                &limits,
                ArchiveConflictPolicy::Skip,
                |_, _| {
                    Err(crate::archive_contract::ArchiveError::new(
                        ArchiveErrorCode::Io,
                        "stream reached",
                    ))
                },
                || false,
            )
        };
        let before = std::fs::read_dir(&startup).unwrap().count();
        let plan = plan_for(&[
            ("Startup/", EntryKind::Directory),
            ("Startup/ferrum-test-do-not-keep.txt", EntryKind::File),
        ]);
        let err = run(&plan, &programs).unwrap_err();
        assert_eq!(err.code, ArchiveErrorCode::UnsafeEntry, "{err:?}");
        assert!(err.message.contains("Startup"), "{}", err.message);
        assert!(!startup.join("ferrum-test-do-not-keep.txt").exists());
        // Straight into Startup, whatever the entry.
        let plan_txt = plan_for(&[("ferrum-test-do-not-keep.txt", EntryKind::File)]);
        assert_eq!(
            run(&plan_txt, &startup).unwrap_err().code,
            ArchiveErrorCode::UnsafeEntry
        );
        assert_eq!(std::fs::read_dir(&startup).unwrap().count(), before);
        // Not touching Startup: gets as far as the (failing) stream.
        let tmp = tempfile::tempdir().unwrap();
        let plan = plan_for(&[("Startup/f.txt", EntryKind::File)]);
        let err = run(&plan, tmp.path()).unwrap_err();
        assert!(!err.message.contains("Startup"), "{}", err.message);
    }

    /// FER-559 rework 2: an 8.3 short name hides the Startup folder from the lexical check
    /// (`STARTM~1/Programs/Startup/...` extracted into `...\Windows`). The final path of every
    /// folder the extractor opens is vetoed too. When the volume has no short name for
    /// `Start Menu`, only the final-path veto function is exercised.
    #[cfg(windows)]
    #[test]
    fn a_short_name_alias_of_the_startup_folder_is_refused_by_the_final_path_check() {
        use crate::archive_contract::{ArchiveConflictPolicy, ArchiveErrorCode, ArchiveLimits};
        use crate::archive_extract::{Entry, EntryKind, extract, preflight};
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        crate::install_extract_guard();
        let startup =
            crate::profile::known_folder(&windows_sys::Win32::UI::Shell::FOLDERID_Startup)
                .expect("startup folder");
        let start_menu = startup.parent().unwrap().parent().unwrap().to_path_buf();
        let windows_dir = start_menu.parent().unwrap().to_path_buf();
        let short_of = |p: &std::path::Path| -> Option<String> {
            let wide: Vec<u16> = p.as_os_str().encode_wide().chain([0]).collect();
            let mut buf = vec![0u16; 1024];
            // SAFETY: NUL-terminated input, writable output buffer of the given length.
            let n = unsafe {
                windows_sys::Win32::Storage::FileSystem::GetShortPathNameW(
                    wide.as_ptr(),
                    buf.as_mut_ptr(),
                    buf.len() as u32,
                )
            } as usize;
            (n > 0 && n < buf.len()).then(|| {
                std::path::PathBuf::from(std::ffi::OsString::from_wide(&buf[..n]))
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
        };
        // The veto function itself: the final path of Startup is refused, a sibling is not.
        let guard_dir = |p: &str| {
            let mut ok = true;
            crate::startup_guard::check_into(p).unwrap_or_else(|_| ok = false);
            ok
        };
        assert!(!guard_dir(&startup.to_string_lossy()));
        // A comment is a descript.ion sidecar write into the item's folder.
        let item = startup
            .join("ferrum-test-do-not-keep.txt")
            .to_string_lossy()
            .into_owned();
        let err =
            tauri::async_runtime::block_on(super::comment_set(item, Some("c".into()))).unwrap_err();
        assert!(err.message.contains("Startup"), "{}", err.message);
        assert!(!startup.join("descript.ion").exists());
        assert!(guard_dir(&start_menu.to_string_lossy()));

        let limits = ArchiveLimits {
            max_entries: 100,
            max_total_bytes: 1 << 20,
            max_entry_bytes: 1 << 20,
            max_depth: 8,
        };
        let alias = short_of(&start_menu).filter(|s| s.contains('~'));
        let Some(alias) = alias else {
            eprintln!("8.3 names are off on this volume: only the final-path veto was exercised");
            return;
        };
        let name = format!("{alias}/Programs/Startup/ferrum-test-do-not-keep.txt");
        let entries = [Entry {
            raw_name: &name,
            kind: EntryKind::File,
            expanded_size: None,
            compressed_size: None,
            modified_ms: None,
        }];
        let plan = preflight(&entries, &limits).unwrap();
        // The lexical check cannot see it ...
        assert!(
            crate::startup_guard::check_extraction(
                &windows_dir,
                plan.entries().iter().map(|e| e.relative_path.as_path())
            )
            .is_ok(),
            "the alias is invisible to the lexical check, which is the point of the final-path veto"
        );
        // ... the extractor's per-folder final-path veto does.
        let err = extract(
            &plan,
            &windows_dir,
            &limits,
            ArchiveConflictPolicy::Skip,
            |_, _| {
                Err(crate::archive_contract::ArchiveError::new(
                    ArchiveErrorCode::Io,
                    "stream reached",
                ))
            },
            || false,
        )
        .unwrap_err();
        assert_eq!(err.code, ArchiveErrorCode::UnsafeEntry, "{err:?}");
        assert!(err.message.contains("Startup"), "{}", err.message);
        assert!(!startup.join("ferrum-test-do-not-keep.txt").exists());
    }

    /// FER-559 S1: a plain source next to a verbatim target must not defeat the "same path" and
    /// "into itself" checks. Moving `[a\nul, a\x]` onto `a` with Overwrite is refused and nothing
    /// is deleted; copying `[a\nul, a]` into `a\sub` is refused as "into itself".
    #[cfg(windows)]
    #[test]
    fn mixed_spellings_cannot_turn_a_move_onto_itself_into_data_loss() {
        use crate::copy_engine::{ConflictPolicy, run_copy};
        let _guard = crate::protected_dirs::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let root = std::env::temp_dir().join(format!("ferrum-s1-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(format!(r"\\?\{}", root.display()));
        let a = root.join("a");
        std::fs::create_dir_all(a.join("sub")).unwrap();
        let va = crate::path_guard::verbatim_path(&a.to_string_lossy());
        std::fs::write(format!(r"{va}\nul"), b"reserved").unwrap();
        std::fs::write(a.join("x"), b"precious").unwrap();
        let a_s = a.to_string_lossy().into_owned();
        let cancel = crate::ops::CancelToken::default();

        let (srcs, target) =
            super::prepare_copy_io(&[format!(r"{a_s}\nul"), format!(r"{a_s}\x")], &a_s).unwrap();
        assert!(srcs.iter().all(|s| s.starts_with(r"\\?\")) && target.starts_with(r"\\?\"));
        for policy in [
            ConflictPolicy::Overwrite,
            ConflictPolicy::RenameAuto,
            ConflictPolicy::OverwriteOlder,
        ] {
            let mv = crate::move_engine::run_move(&srcs, &target, policy, &cancel, &|_| {}, false);
            assert_eq!(mv.moved, 0, "{policy:?}");
            assert_eq!(mv.failures.len(), 2, "{policy:?}: {:?}", mv.failures);
            assert_eq!(std::fs::read(a.join("x")).unwrap(), b"precious");
            assert_eq!(std::fs::read(format!(r"{va}\nul")).unwrap(), b"reserved");
        }
        // The engines also compare normalised paths when handed MIXED spellings directly.
        let mixed = vec![format!(r"{va}\x")];
        let mv = crate::move_engine::run_move(
            &mixed,
            &a_s,
            ConflictPolicy::Overwrite,
            &cancel,
            &|_| {},
            false,
        );
        assert_eq!(mv.moved, 0);
        assert_eq!(std::fs::read(a.join("x")).unwrap(), b"precious");

        // copy [a\nul, a] into a\sub: "into itself"
        let sub = a.join("sub").to_string_lossy().into_owned();
        let (srcs, target) =
            super::prepare_copy_io(&[format!(r"{a_s}\nul"), a_s.clone()], &sub).unwrap();
        let plan = crate::copy_engine::plan_copy(&srcs, &target);
        assert!(plan.unwrap_err().message.contains("into itself"));
        let out = run_copy(&srcs, &target, ConflictPolicy::Overwrite, &cancel, &|_| {});
        assert!(
            out.failures
                .iter()
                .any(|f| f.message.contains("into itself")),
            "{:?}",
            out.failures
        );
        assert!(!a.join("sub").join("a").exists());
        let _ = std::fs::remove_dir_all(format!(r"\\?\{}", root.display()));
    }

    /// Info-1: the harness job can never sleep longer than 10 s.
    #[test]
    fn dummy_op_ticks_are_capped() {
        assert_eq!(dummy_ticks(0), 0);
        assert_eq!(dummy_ticks(5_000), 100);
        assert_eq!(dummy_ticks(10_000), 200);
        assert_eq!(dummy_ticks(60_000), 200, "capped at 10 s");
        assert_eq!(dummy_ticks(u64::MAX), 200);
    }
}

#[cfg(test)]
mod third_party_notices_tests {
    use super::*;

    #[test]
    fn notices_path_is_fixed_name_in_resource_dir() {
        let dir = tempfile::tempdir().unwrap();
        let err = bundled_notices_path(dir.path()).unwrap_err();
        assert_eq!(err.code, crate::domain::ErrorCode::NotFound);

        std::fs::write(dir.path().join(THIRD_PARTY_NOTICES_FILE), "notices").unwrap();
        let path = bundled_notices_path(dir.path()).unwrap();
        assert!(path.ends_with(THIRD_PARTY_NOTICES_FILE));
        assert!(!path.starts_with(r"\\?\"));
    }
}
