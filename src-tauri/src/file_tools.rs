//! File tools (F2-24): checksums, split/combine, and verification.
//! Kept Tauri-free for unit testing; `ipc.rs` exposes the commands.
//!
//! Honesty rules: SHA-256 is the default and the only algorithm presented
//! as current; SHA-1/MD5/CRC32 exist purely for verifying legacy manifests
//! and are labeled as such (never for new security claims). Everything
//! streams in 1 MiB chunks — file size is never proportional to memory.
//! Split writes deterministic `.001`-style parts plus a `.parts.json`
//! manifest (sizes + SHA-256 of the whole file) through staging names and
//! renames the whole set into place only after every part is written; a
//! cancelled or failed split leaves no part and no manifest behind. Combine
//! validates the contiguous part sequence (and the manifest, when present)
//! BEFORE any output is written, streams through a staging file, and only
//! then commits the final name — a failed combine never leaves a wrong
//! final file behind.
//!
//! Output conflicts (sidecars, parts, the combined file) default to refusing
//! (`OutputPolicy::None`); the copy engine's `ConflictPolicy` can be passed
//! to skip / overwrite / overwrite-older, and (combine output only) to
//! rename. `Ask` cannot prompt from a background job, so it refuses.

use crate::archive_contract::{
    ArchiveError, ArchiveErrorCode, ArchiveProvider, ArchiveReadRequest, Location, MemberPath,
};
use crate::copy_engine::ConflictPolicy;
use crate::domain::{ErrorCode, FsError, OperationKind, OperationProgress, OperationState};
use crate::ops::JobBody;
use serde::{Deserialize, Serialize};
use sha1::Digest as Sha1Digest;
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

const CHUNK: usize = 1024 * 1024;
const STAGING_PREFIX: &str = ".ferrum-combine-";
const SPLIT_STAGING_PREFIX: &str = ".ferrum-split-";
const SUM_STAGING_PREFIX: &str = ".ferrum-sum-";

/// What to do when an output already exists. `None` is the safe default:
/// refuse (AlreadyExists). `Some(Ask)` also refuses — a queued job cannot
/// prompt.
pub type OutputPolicy = Option<ConflictPolicy>;

/// Live progress of one tool run. `total_bytes == 0` means "unknown".
pub struct ToolProgress<'a> {
    pub current: &'a str,
    pub processed_bytes: u64,
    pub total_bytes: u64,
    pub files_done: u64,
    pub files_total: u64,
}

pub type ProgressFn<'a> = &'a dyn Fn(&ToolProgress<'_>);

fn no_progress(_: &ToolProgress<'_>) {}

/// Hash algorithms. `Sha256` is the default; the rest verify legacy data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Algorithm {
    Sha256,
    Sha1,
    Md5,
    Crc32,
}

impl Algorithm {
    pub fn parse(s: &str) -> Result<Algorithm, FsError> {
        match s {
            "sha256" => Ok(Algorithm::Sha256),
            "sha1" => Ok(Algorithm::Sha1),
            "md5" => Ok(Algorithm::Md5),
            "crc32" => Ok(Algorithm::Crc32),
            other => Err(FsError::new(
                ErrorCode::InvalidPath,
                format!("unknown checksum algorithm {other:?}"),
            )),
        }
    }

    /// Sidecar manifest extension for new checksum files.
    pub fn extension(self) -> &'static str {
        match self {
            Algorithm::Sha256 => "sha256",
            Algorithm::Sha1 => "sha1",
            Algorithm::Md5 => "md5",
            Algorithm::Crc32 => "sfv",
        }
    }

    /// True for algorithms kept only to read old manifests.
    pub fn is_legacy(self) -> bool {
        !matches!(self, Algorithm::Sha256)
    }
}

/// One streaming hasher for every algorithm (shared by files and archive
/// members so both stay at one chunk of memory).
enum Hasher {
    Sha256(sha2::Sha256),
    Sha1(sha1::Sha1),
    Md5(md5::Md5),
    Crc32(crc32fast::Hasher),
}

impl Hasher {
    fn new(algorithm: Algorithm) -> Self {
        match algorithm {
            Algorithm::Sha256 => Hasher::Sha256(sha2::Sha256::new()),
            Algorithm::Sha1 => Hasher::Sha1(sha1::Sha1::new()),
            Algorithm::Md5 => Hasher::Md5(md5::Md5::new()),
            Algorithm::Crc32 => Hasher::Crc32(crc32fast::Hasher::new()),
        }
    }

    fn update(&mut self, data: &[u8]) {
        match self {
            Hasher::Sha256(h) => h.update(data),
            Hasher::Sha1(h) => h.update(data),
            Hasher::Md5(h) => h.update(data),
            Hasher::Crc32(h) => h.update(data),
        }
    }

    /// Hex digest (CRC32 as 8 hex digits).
    fn finish(self) -> String {
        match self {
            Hasher::Sha256(h) => hex::encode(h.finalize()),
            Hasher::Sha1(h) => hex::encode(h.finalize()),
            Hasher::Md5(h) => hex::encode(h.finalize()),
            Hasher::Crc32(h) => format!("{:08x}", h.finalize()),
        }
    }
}

fn is_cancelled(cancel: &dyn Fn() -> bool) -> Result<(), FsError> {
    if cancel() {
        return Err(FsError::new(ErrorCode::Cancelled, "cancelled"));
    }
    Ok(())
}

/// Stream `path` through the hasher of `algorithm`; returns the hex digest
/// (CRC32 as 8 hex digits). Memory stays at one 1 MiB chunk.
pub fn hash_file(
    path: &Path,
    algorithm: Algorithm,
    cancel: &dyn Fn() -> bool,
) -> Result<String, FsError> {
    hash_file_with(path, algorithm, cancel, &mut |_| {})
}

/// Same as `hash_file`, reporting the bytes of every chunk through `on_bytes`.
fn hash_file_with(
    path: &Path,
    algorithm: Algorithm,
    cancel: &dyn Fn() -> bool,
    on_bytes: &mut dyn FnMut(u64),
) -> Result<String, FsError> {
    let mut file =
        fs::File::open(path).map_err(|e| FsError::from(e).with_path(path.to_string_lossy()))?;
    hash_stream(&mut file, algorithm, cancel, on_bytes)
}

/// The streaming core of `hash_file`: one reusable `CHUNK`-sized buffer, so
/// memory never depends on the input size.
fn hash_stream<R: Read>(
    reader: &mut R,
    algorithm: Algorithm,
    cancel: &dyn Fn() -> bool,
    on_bytes: &mut dyn FnMut(u64),
) -> Result<String, FsError> {
    let mut buffer = vec![0u8; CHUNK];
    let mut hasher = Hasher::new(algorithm);
    loop {
        is_cancelled(cancel)?;
        let n = read_chunk(reader, &mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
        on_bytes(n as u64);
    }
    Ok(hasher.finish())
}

/// Hash one archive member through `ArchiveProvider::read`, which hands the
/// decoder output over in small chunks — the member is never materialised
/// and nothing is extracted to disk. Providers that buffer a whole member
/// (RAR) must be refused by the caller before this is used.
pub fn hash_archive_member(
    provider: &dyn ArchiveProvider,
    archive_path: &str,
    member_path: &str,
    algorithm: Algorithm,
    password: Option<String>,
    cancel: &dyn Fn() -> bool,
    on_bytes: &mut dyn FnMut(u64),
) -> Result<String, ArchiveError> {
    let member = MemberPath::parse(member_path)?;
    let request = ArchiveReadRequest {
        location: Location::Archive {
            archive_path: archive_path.to_string(),
            member_path: member,
        },
        max_bytes: u64::MAX,
        password,
    };
    let mut hasher = Hasher::new(algorithm);
    let truncated = provider.read(&request, &mut |bytes| {
        if cancel() {
            return Err(ArchiveError::new(ArchiveErrorCode::Cancelled, "cancelled"));
        }
        hasher.update(bytes);
        on_bytes(bytes.len() as u64);
        Ok(())
    })?;
    if truncated {
        return Err(ArchiveError::new(
            ArchiveErrorCode::LimitExceeded,
            "Archive member was truncated while hashing",
        ));
    }
    Ok(hasher.finish())
}

/// Read up to `buf.len()` bytes; 0 at EOF (harmonizes io errors with cancel).
fn read_chunk<R: Read>(file: &mut R, buf: &mut [u8]) -> Result<usize, FsError> {
    loop {
        match file.read(buf) {
            Ok(0) => return Ok(0),
            Ok(n) => return Ok(n),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(FsError::from(e)),
        }
    }
}

// ── Cancellable direct requests (verify, archive-member hash) ────────────

type CancelFlags = Mutex<HashMap<String, Arc<AtomicBool>>>;

fn cancel_flags() -> &'static CancelFlags {
    static FLAGS: OnceLock<CancelFlags> = OnceLock::new();
    FLAGS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Registers a cancel flag under `request_id` for the lifetime of the guard.
pub struct CancelRegistration {
    id: String,
    flag: Arc<AtomicBool>,
}

impl CancelRegistration {
    pub fn new(request_id: &str) -> Self {
        let flag = Arc::new(AtomicBool::new(false));
        cancel_flags()
            .lock()
            .expect("cancel flags")
            .insert(request_id.to_string(), Arc::clone(&flag));
        Self {
            id: request_id.to_string(),
            flag,
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }
}

impl Drop for CancelRegistration {
    fn drop(&mut self) {
        cancel_flags()
            .lock()
            .expect("cancel flags")
            .remove(&self.id);
    }
}

/// Ask a running direct request (verify / archive-member hash) to stop.
/// False when no request with that id is running.
pub fn cancel_request(request_id: &str) -> bool {
    match cancel_flags().lock().expect("cancel flags").get(request_id) {
        Some(flag) => {
            flag.store(true, Ordering::SeqCst);
            true
        }
        None => false,
    }
}

/// `verify_checksums_with` that `cancel_request(request_id)` can stop.
pub fn run_verify_cancellable(
    request_id: &str,
    paths: &[String],
    progress: ProgressFn,
) -> VerifyReport {
    let registration = CancelRegistration::new(request_id);
    verify_checksums_with(paths, &|| registration.is_cancelled(), progress)
}

/// Result of hashing one archive member in place.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveMemberChecksum {
    pub archive_path: String,
    pub member_path: String,
    pub algorithm: Algorithm,
    pub hash: String,
}

/// Per-file outcome of a batch (create / verify).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolOutcome {
    pub path: String,
    /// ok | mismatch | missing | noManifest | failed | created | skipped
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actual: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateChecksumsReport {
    pub requested: usize,
    pub created: usize,
    pub outcomes: Vec<ToolOutcome>,
}

/// One parsed manifest line.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ManifestEntry {
    hash: String,
    name: String,
}

fn is_hex(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Parse `hash  name` (GNU text mode), `hash *name` (GNU binary mode) and
/// SFV's `name hash` single-space style. `#`/`;` comment lines and blanks
/// are skipped.
fn parse_manifest(text: &str) -> Vec<ManifestEntry> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim_end_matches(['\r', '\n']).trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        // GNU/BSD: "<hex> <mode><name>" — mode ' ' (text) or '*' (binary).
        if let Some((hash, rest)) = line.split_once(' ')
            && is_hex(hash)
            && (rest.starts_with(' ') || rest.starts_with('*'))
        {
            let name = rest[1..].trim();
            if !name.is_empty() {
                out.push(ManifestEntry {
                    hash: hash.to_string(),
                    name: name.to_string(),
                });
            }
        } else if let Some((name, hash)) = line.rsplit_once(' ') {
            // SFV fallback: "filename crc32"
            if !hash.trim().is_empty() && !name.trim().is_empty() {
                out.push(ManifestEntry {
                    hash: hash.trim().to_string(),
                    name: name.trim().to_string(),
                });
            }
        }
    }
    out
}

fn manifest_extension_of(path: &Path) -> Option<Algorithm> {
    let ext = path.extension()?.to_string_lossy().to_lowercase();
    match ext.as_str() {
        "sha256" => Some(Algorithm::Sha256),
        "sha1" => Some(Algorithm::Sha1),
        "md5" => Some(Algorithm::Md5),
        "sfv" => Some(Algorithm::Crc32),
        _ => None,
    }
}

fn sidecar_for(file: &Path, algorithm: Algorithm) -> PathBuf {
    let mut name = file.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{}", algorithm.extension()));
    file.with_file_name(name)
}

// ── Output conflicts ─────────────────────────────────────────────────────

enum Resolved {
    Write(PathBuf),
    Skip,
}

/// `name (2).ext`, `name (3).ext`, … — same shape the copy engine uses.
fn numbered(target: &Path, n: u32) -> PathBuf {
    let stem = target
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("file");
    let name = match target.extension().and_then(|s| s.to_str()) {
        Some(ext) => format!("{stem} ({n}).{ext}"),
        None => format!("{stem} ({n})"),
    };
    target.with_file_name(name)
}

/// Decide where an output goes when `target` may already exist. Refuses
/// unless the caller opted into a policy. `allow_rename` is false for
/// outputs whose name is a contract (sidecars, parts).
fn resolve_output(
    target: &Path,
    source_modified: Option<SystemTime>,
    policy: OutputPolicy,
    allow_rename: bool,
    what: &str,
) -> Result<Resolved, FsError> {
    let Ok(meta) = fs::symlink_metadata(target) else {
        return Ok(Resolved::Write(target.to_path_buf()));
    };
    let refuse = |detail: String| {
        Err(FsError::new(ErrorCode::AlreadyExists, detail).with_path(target.to_string_lossy()))
    };
    match policy {
        None | Some(ConflictPolicy::Ask) => refuse(format!("{what} already exists")),
        Some(ConflictPolicy::Skip) => Ok(Resolved::Skip),
        Some(ConflictPolicy::Overwrite) | Some(ConflictPolicy::OverwriteOlder) if meta.is_dir() => {
            refuse(format!("{what} already exists and is a folder"))
        }
        Some(ConflictPolicy::Overwrite) => Ok(Resolved::Write(target.to_path_buf())),
        Some(ConflictPolicy::OverwriteOlder) => {
            let older = match (meta.modified().ok(), source_modified) {
                (Some(existing), Some(source)) => existing < source,
                _ => false,
            };
            if older {
                Ok(Resolved::Write(target.to_path_buf()))
            } else {
                Ok(Resolved::Skip)
            }
        }
        Some(ConflictPolicy::RenameAuto) if !allow_rename => refuse(format!(
            "{what} already exists and cannot be auto-renamed (its name is fixed)"
        )),
        Some(ConflictPolicy::RenameAuto) => {
            let mut n = 2;
            let mut candidate = numbered(target, n);
            while fs::symlink_metadata(&candidate).is_ok() {
                n += 1;
                candidate = numbered(target, n);
            }
            Ok(Resolved::Write(candidate))
        }
    }
}

// ── Create ───────────────────────────────────────────────────────────────

/// Create sidecar manifests for the selection. An existing sidecar is never
/// overwritten (AlreadyExists per item) — regenerating is a delete + create.
pub fn create_checksums(
    paths: &[String],
    algorithm: Algorithm,
    cancel: &dyn Fn() -> bool,
) -> CreateChecksumsReport {
    create_checksums_with(paths, algorithm, None, cancel, &no_progress)
}

/// `create_checksums` with an output conflict policy and byte progress. A
/// cancel stops the batch (items not reached get no outcome).
pub fn create_checksums_with(
    paths: &[String],
    algorithm: Algorithm,
    policy: OutputPolicy,
    cancel: &dyn Fn() -> bool,
    progress: ProgressFn,
) -> CreateChecksumsReport {
    let mut report = CreateChecksumsReport {
        requested: paths.len(),
        created: 0,
        outcomes: Vec::new(),
    };
    let total: u64 = paths
        .iter()
        .filter_map(|p| fs::metadata(p).ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .sum();
    let mut processed = 0u64;
    for (index, p) in paths.iter().enumerate() {
        if cancel() {
            break;
        }
        let outcome = create_one(p, algorithm, policy, cancel, &mut |delta| {
            processed += delta;
            progress(&ToolProgress {
                current: p,
                processed_bytes: processed,
                total_bytes: total,
                files_done: index as u64,
                files_total: paths.len() as u64,
            });
        });
        // A hash interrupted by the cancel is not a failure worth reporting.
        if cancel() && outcome.status == "failed" {
            break;
        }
        if outcome.status == "created" {
            report.created += 1;
        }
        report.outcomes.push(outcome);
    }
    report
}

fn create_one(
    p: &str,
    algorithm: Algorithm,
    policy: OutputPolicy,
    cancel: &dyn Fn() -> bool,
    on_bytes: &mut dyn FnMut(u64),
) -> ToolOutcome {
    let file = Path::new(p);
    if let Err(e) = is_cancelled(cancel) {
        return err_outcome(p, &e);
    }
    let meta = match fs::symlink_metadata(file) {
        Ok(meta) => meta,
        Err(e) => return err_outcome(p, &FsError::from(e)),
    };
    if meta.is_dir() || meta.is_symlink() {
        return ToolOutcome {
            path: p.to_string(),
            status: "failed".into(),
            expected: None,
            actual: None,
            message: Some("Only regular files can be checksummed".into()),
        };
    }
    // Decide before hashing: a skipped or refused sidecar costs no read.
    let sidecar = sidecar_for(file, algorithm);
    let target = match resolve_output(
        &sidecar,
        meta.modified().ok(),
        policy,
        false,
        "Checksum file",
    ) {
        Ok(Resolved::Write(target)) => target,
        Ok(Resolved::Skip) => {
            return ToolOutcome {
                path: sidecar.to_string_lossy().into_owned(),
                status: "skipped".into(),
                expected: None,
                actual: None,
                message: Some("Checksum file already exists — skipped".into()),
            };
        }
        Err(e) => return err_outcome(&sidecar.to_string_lossy(), &e),
    };
    let hash = match hash_file_with(file, algorithm, cancel, on_bytes) {
        Ok(hash) => hash,
        Err(e) => return err_outcome(p, &e),
    };
    let name = file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    // Temp file + rename: a crash mid-write never leaves a truncated sidecar.
    let temp = target.with_file_name(format!(
        "{SUM_STAGING_PREFIX}{}.partial",
        std::process::id()
    ));
    if let Err(e) = fs::write(&temp, format!("{hash}  {name}\n")) {
        let _ = fs::remove_file(&temp);
        return err_outcome(&target.to_string_lossy(), &FsError::from(e));
    }
    if let Err(e) = fs::rename(&temp, &target) {
        let _ = fs::remove_file(&temp);
        return err_outcome(&target.to_string_lossy(), &FsError::from(e));
    }
    ToolOutcome {
        path: target.to_string_lossy().into_owned(),
        status: "created".into(),
        expected: None,
        actual: Some(hash),
        message: None,
    }
}

fn err_outcome(path: &str, e: &FsError) -> ToolOutcome {
    ToolOutcome {
        path: path.to_string(),
        status: "failed".into(),
        expected: None,
        actual: None,
        message: Some(e.message.clone()),
    }
}

// ── Verify ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyReport {
    pub requested: usize,
    pub ok: usize,
    pub mismatched: usize,
    pub outcomes: Vec<ToolOutcome>,
    /// True when a cancel stopped the run; `outcomes` then holds only the
    /// items finished before it.
    #[serde(default)]
    pub cancelled: bool,
}

/// Verify the selection. A manifest path verifies every file it references;
/// a regular file looks for a sidecar manifest next to it (any family) and
/// compares against that entry. Honest statuses only — `noManifest` when
/// nothing verifiable exists.
pub fn verify_checksums(paths: &[String], cancel: &dyn Fn() -> bool) -> VerifyReport {
    verify_checksums_with(paths, cancel, &no_progress)
}

/// `verify_checksums` reporting hashed bytes (`total_bytes` stays 0: the
/// full size is not known up front when manifests list the files).
pub fn verify_checksums_with(
    paths: &[String],
    cancel: &dyn Fn() -> bool,
    progress: ProgressFn,
) -> VerifyReport {
    let mut report = VerifyReport {
        requested: paths.len(),
        ok: 0,
        mismatched: 0,
        outcomes: Vec::new(),
        cancelled: false,
    };
    let mut ctx = VerifyCtx {
        cancel,
        progress,
        processed: 0,
        files_done: 0,
        files_total: paths.len() as u64,
    };
    for p in paths {
        if cancel() {
            report.cancelled = true;
            break;
        }
        let path = Path::new(p);
        match manifest_extension_of(path) {
            Some(algorithm) => verify_manifest(path, algorithm, &mut ctx, &mut report),
            None => match verify_against_sidecar(path, &mut ctx) {
                Some(outcome) => {
                    tally(&mut report, &outcome);
                    report.outcomes.push(outcome);
                }
                None => report.cancelled = true,
            },
        }
        if report.cancelled {
            break;
        }
        ctx.files_done += 1;
    }
    report
}

struct VerifyCtx<'a> {
    cancel: &'a dyn Fn() -> bool,
    progress: ProgressFn<'a>,
    processed: u64,
    files_done: u64,
    files_total: u64,
}

impl VerifyCtx<'_> {
    /// None when the check was interrupted by a cancel (no outcome to keep).
    fn check(&mut self, file: &Path, algorithm: Algorithm, expected: &str) -> Option<ToolOutcome> {
        let current = file.to_string_lossy().into_owned();
        let (cancel, progress) = (self.cancel, self.progress);
        let (files_done, files_total) = (self.files_done, self.files_total);
        let processed = &mut self.processed;
        let outcome = check_one(file, algorithm, expected, cancel, &mut |delta| {
            *processed += delta;
            progress(&ToolProgress {
                current: &current,
                processed_bytes: *processed,
                total_bytes: 0,
                files_done,
                files_total,
            });
        });
        if cancel() && outcome.status == "failed" {
            return None;
        }
        Some(outcome)
    }
}

fn verify_manifest(
    manifest: &Path,
    algorithm: Algorithm,
    ctx: &mut VerifyCtx<'_>,
    report: &mut VerifyReport,
) {
    let text = match fs::read_to_string(manifest) {
        Ok(text) => text,
        Err(e) => {
            let o = err_outcome(&manifest.to_string_lossy(), &FsError::from(e));
            tally(report, &o);
            report.outcomes.push(o);
            return;
        }
    };
    let dir = manifest.parent().unwrap_or_else(|| Path::new("."));
    for entry in parse_manifest(&text) {
        if (ctx.cancel)() {
            report.cancelled = true;
            return;
        }
        let target = dir.join(&entry.name);
        let Some(outcome) = ctx.check(&target, algorithm, &entry.hash) else {
            report.cancelled = true;
            return;
        };
        tally(report, &outcome);
        report.outcomes.push(outcome);
    }
}

/// None when a cancel interrupted the hash.
fn verify_against_sidecar(file: &Path, ctx: &mut VerifyCtx<'_>) -> Option<ToolOutcome> {
    let file_str = file.to_string_lossy().into_owned();
    for algorithm in [
        Algorithm::Sha256,
        Algorithm::Sha1,
        Algorithm::Md5,
        Algorithm::Crc32,
    ] {
        let sidecar = sidecar_for(file, algorithm);
        if !sidecar.exists() {
            continue;
        }
        let Ok(text) = fs::read_to_string(&sidecar) else {
            continue;
        };
        let name = file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let entry = parse_manifest(&text).into_iter().find(|e| e.name == name);
        let Some(entry) = entry else {
            return Some(ToolOutcome {
                path: file_str,
                status: "noManifest".into(),
                expected: None,
                actual: None,
                message: Some(format!(
                    "{} exists but lists no entry for this file",
                    sidecar.to_string_lossy()
                )),
            });
        };
        return ctx.check(file, algorithm, &entry.hash);
    }
    Some(ToolOutcome {
        path: file_str,
        status: "noManifest".into(),
        expected: None,
        actual: None,
        message: None,
    })
}

fn check_one(
    file: &Path,
    algorithm: Algorithm,
    expected: &str,
    cancel: &dyn Fn() -> bool,
    on_bytes: &mut dyn FnMut(u64),
) -> ToolOutcome {
    let path = file.to_string_lossy().into_owned();
    if !file.exists() {
        return ToolOutcome {
            path,
            status: "missing".into(),
            expected: Some(expected.to_string()),
            actual: None,
            message: None,
        };
    }
    match hash_file_with(file, algorithm, cancel, on_bytes) {
        Ok(actual) => {
            let equal = actual.eq_ignore_ascii_case(expected);
            ToolOutcome {
                path,
                status: if equal {
                    "ok".into()
                } else {
                    "mismatch".into()
                },
                expected: Some(expected.to_string()),
                actual: Some(actual),
                message: None,
            }
        }
        Err(e) => err_outcome(&path, &e),
    }
}

fn tally(report: &mut VerifyReport, outcome: &ToolOutcome) {
    match outcome.status.as_str() {
        "ok" => report.ok += 1,
        "mismatch" => report.mismatched += 1,
        _ => {}
    }
}

// ── Split / combine ──────────────────────────────────────────────────────

/// Manifest written next to the parts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SplitManifest {
    /// Original file name (without part suffixes).
    pub name: String,
    /// Logical size of the source file.
    pub size: u64,
    /// Exact size of every part except (possibly) the last.
    pub part_size: u64,
    pub parts: usize,
    /// SHA-256 of the whole original file.
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SplitReport {
    pub source: String,
    pub part_size: u64,
    pub parts: Vec<String>,
    pub manifest: Option<String>,
    /// True when the conflict policy skipped the whole split (nothing written).
    #[serde(default)]
    pub skipped: bool,
}

/// Split `path` into `part_bytes`-sized `.001`, `.002`, … parts in `out_dir`
/// plus a `.parts.json` manifest. Refuses to overwrite existing parts.
pub fn split_file(
    path: &str,
    part_bytes: u64,
    out_dir: &str,
    cancel: &dyn Fn() -> bool,
) -> Result<SplitReport, FsError> {
    split_file_with(path, part_bytes, out_dir, None, cancel, &no_progress)
}

/// Existing members of a part set for `name` in `dir`: `name.<3+ digits>`
/// files plus `name.parts.json`.
fn existing_set(dir: &Path, name: &str) -> Vec<PathBuf> {
    let prefix = format!("{name}.");
    let manifest_name = format!("{name}.parts.json");
    let mut found = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let file_name = entry.file_name().to_string_lossy().into_owned();
        let is_part = file_name
            .strip_prefix(&prefix)
            .is_some_and(|rest| rest.len() >= 3 && rest.bytes().all(|b| b.is_ascii_digit()));
        if is_part || file_name == manifest_name {
            found.push(entry.path());
        }
    }
    found.sort();
    found
}

/// `split_file` with a conflict policy and byte progress. Parts are written
/// to staging names and renamed into place only after the whole set (and
/// the manifest) exists; any cancel/IO error removes the staged files.
pub fn split_file_with(
    path: &str,
    part_bytes: u64,
    out_dir: &str,
    policy: OutputPolicy,
    cancel: &dyn Fn() -> bool,
    progress: ProgressFn,
) -> Result<SplitReport, FsError> {
    if part_bytes == 0 {
        return Err(FsError::new(
            ErrorCode::InvalidPath,
            "Part size must be positive",
        ));
    }
    let source = Path::new(path);
    let meta = fs::metadata(source).map_err(|e| FsError::from(e).with_path(path))?;
    if !meta.is_file() {
        return Err(
            FsError::new(ErrorCode::InvalidPath, "Only regular files can be split").with_path(path),
        );
    }
    let dir = Path::new(out_dir);
    fs::create_dir_all(dir).map_err(|e| FsError::from(e).with_path(out_dir))?;
    let name = source
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| {
            FsError::new(ErrorCode::InvalidPath, "Path has no file name").with_path(path)
        })?;

    let total_parts =
        (meta.len() / part_bytes + u64::from(meta.len() % part_bytes > 0)).max(1) as usize;
    // `.001` up to 999 parts; beyond that every suffix widens together
    // (`.0001`…) so the set stays sortable and `combine_parts` can read it.
    let width = total_parts.to_string().len().max(3);
    let part_path = |index: usize| -> PathBuf { dir.join(format!("{name}.{index:0width$}")) };
    let manifest_path = dir.join(format!("{name}.parts.json"));

    // Settle conflicts for the WHOLE set up front — nothing is written when
    // the policy refuses or skips.
    let existing = existing_set(dir, &name);
    if !existing.is_empty() {
        let skip = match policy {
            None | Some(ConflictPolicy::Ask) => {
                return Err(FsError::new(
                    ErrorCode::AlreadyExists,
                    format!(
                        "Part {} already exists in the target folder",
                        existing[0].to_string_lossy()
                    ),
                )
                .with_path(out_dir));
            }
            Some(ConflictPolicy::RenameAuto) => {
                return Err(FsError::new(
                    ErrorCode::AlreadyExists,
                    "A part set already exists and part names are fixed, so it cannot be auto-renamed",
                )
                .with_path(out_dir));
            }
            Some(ConflictPolicy::Skip) => true,
            Some(ConflictPolicy::Overwrite) => false,
            Some(ConflictPolicy::OverwriteOlder) => {
                let source_modified = meta.modified().ok();
                !existing.iter().all(|p| {
                    match (
                        fs::metadata(p).and_then(|m| m.modified()).ok(),
                        source_modified,
                    ) {
                        (Some(existing), Some(source)) => existing < source,
                        _ => false,
                    }
                })
            }
        };
        if skip {
            return Ok(SplitReport {
                source: path.to_string(),
                part_size: part_bytes,
                parts: Vec::new(),
                manifest: None,
                skipped: true,
            });
        }
    }

    let pid = std::process::id();
    let stage_part = |index: usize| {
        dir.join(format!(
            "{SPLIT_STAGING_PREFIX}{pid}.{index:0width$}.partial"
        ))
    };
    let stage_manifest = dir.join(format!("{SPLIT_STAGING_PREFIX}{pid}.manifest.partial"));
    let mut staged: Vec<PathBuf> = Vec::new();
    let discard = |staged: &[PathBuf]| {
        for p in staged {
            let _ = fs::remove_file(p);
        }
        let _ = fs::remove_file(&stage_manifest);
    };

    let sha256 = match stage_parts(
        source,
        part_bytes,
        &stage_part,
        cancel,
        &mut |bytes, parts_done| {
            progress(&ToolProgress {
                current: path,
                processed_bytes: bytes,
                total_bytes: meta.len(),
                files_done: parts_done as u64,
                files_total: total_parts as u64,
            });
        },
        &mut staged,
    ) {
        Ok(sha256) => sha256,
        Err(e) => {
            discard(&staged);
            return Err(e);
        }
    };
    let manifest = SplitManifest {
        name: name.clone(),
        size: meta.len(),
        part_size: part_bytes,
        parts: staged.len(),
        sha256,
    };
    let staged_manifest = serde_json::to_string_pretty(&manifest)
        .map_err(|e| FsError::new(ErrorCode::Io, e.to_string()))
        .and_then(|json| {
            fs::write(&stage_manifest, json)
                .map_err(|e| FsError::from(e).with_path(stage_manifest.to_string_lossy()))
        })
        .and_then(|()| is_cancelled(cancel));
    if let Err(e) = staged_manifest {
        discard(&staged);
        return Err(e);
    }

    // Commit: rename every staged part, manifest last (its presence marks a
    // complete set). A failure rolls the renamed parts back.
    let mut finals: Vec<PathBuf> = Vec::new();
    let mut failure: Option<FsError> = None;
    for (index, stage) in staged.iter().enumerate() {
        let dest = part_path(index + 1);
        match fs::rename(stage, &dest) {
            Ok(()) => finals.push(dest),
            Err(e) => {
                failure = Some(FsError::from(e).with_path(dest.to_string_lossy()));
                break;
            }
        }
    }
    if failure.is_none()
        && let Err(e) = fs::rename(&stage_manifest, &manifest_path)
    {
        failure = Some(FsError::from(e).with_path(manifest_path.to_string_lossy()));
    }
    if let Some(e) = failure {
        for done in &finals {
            let _ = fs::remove_file(done);
        }
        discard(&staged);
        return Err(e);
    }
    // Overwrite: members of an older, differently sized set would make the
    // new set unreadable (extra parts) — drop the ones not replaced.
    for old in existing {
        if !finals.contains(&old) && old != manifest_path && old.is_file() {
            let _ = fs::remove_file(old);
        }
    }

    Ok(SplitReport {
        source: path.to_string(),
        part_size: part_bytes,
        parts: finals
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect(),
        manifest: Some(manifest_path.to_string_lossy().into_owned()),
        skipped: false,
    })
}

/// Stream `source` into staged parts (cancel polled per chunk and per part
/// opened); returns the SHA-256 of the whole source. Every created staging
/// path is pushed to `staged` so the caller can discard them on failure.
fn stage_parts(
    source: &Path,
    part_bytes: u64,
    stage_part: &dyn Fn(usize) -> PathBuf,
    cancel: &dyn Fn() -> bool,
    on_bytes: &mut dyn FnMut(u64, usize),
    staged: &mut Vec<PathBuf>,
) -> Result<String, FsError> {
    let mut file =
        fs::File::open(source).map_err(|e| FsError::from(e).with_path(source.to_string_lossy()))?;
    let mut hasher = sha2::Sha256::new();
    let mut buffer = vec![0u8; CHUNK];
    let mut processed = 0u64;
    // (open handle, bytes written to it so far)
    let mut current: Option<(fs::File, u64)> = None;
    loop {
        is_cancelled(cancel)?;
        let n = read_chunk(&mut file, &mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
        let mut chunk: &[u8] = &buffer[..n];
        while !chunk.is_empty() {
            if current.is_none() {
                is_cancelled(cancel)?;
                let stage = stage_part(staged.len() + 1);
                let handle = fs::OpenOptions::new()
                    .create(true)
                    .write(true)
                    .truncate(true)
                    .open(&stage)
                    .map_err(|e| FsError::from(e).with_path(stage.to_string_lossy()))?;
                staged.push(stage);
                current = Some((handle, 0));
            }
            let (handle, written) = current.as_mut().unwrap();
            let take = (part_bytes - *written).min(chunk.len() as u64) as usize;
            handle.write_all(&chunk[..take]).map_err(|e| {
                FsError::from(e).with_path(staged.last().unwrap().to_string_lossy())
            })?;
            *written += take as u64;
            chunk = &chunk[take..];
            if *written == part_bytes {
                current = None;
            }
        }
        processed += n as u64;
        let finished = staged.len() - usize::from(current.is_some());
        on_bytes(processed, finished);
    }
    // A trailing partial part closes the set.
    if let Some((mut handle, _)) = current.take() {
        handle
            .flush()
            .map_err(|e| FsError::from(e).with_path(staged.last().unwrap().to_string_lossy()))?;
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Load `<name>.parts.json` next to the first part, when present. The stem
/// keeps the original extension: `book.txt.001` → `book.txt.parts.json`.
fn load_manifest(first_part: &Path) -> Option<SplitManifest> {
    let name = first_part
        .file_name()?
        .to_string_lossy()
        .rsplit_once('.')?
        .0
        .to_string();
    let text = fs::read_to_string(first_part.parent()?.join(format!("{name}.parts.json"))).ok()?;
    serde_json::from_str(&text).ok()
}

/// Part index from a `.001` (or wider, `.0001`) suffix; None when it is not numeric.
fn part_index(path: &Path) -> Option<usize> {
    let name = path.file_name()?.to_string_lossy().into_owned();
    let (stem, ext) = name.rsplit_once('.')?;
    if stem.is_empty() || ext.len() < 3 || !ext.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    ext.parse().ok()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CombineReport {
    pub output: String,
    pub parts_used: usize,
    pub bytes: u64,
    /// Set when a `.parts.json` manifest verified the recombined SHA-256.
    pub sha256_verified: Option<bool>,
    /// True when the conflict policy skipped the combine (nothing written).
    #[serde(default)]
    pub skipped: bool,
}

/// Combine `.001`-style parts (first part anywhere in the set) into
/// `out_file`. The whole sequence is validated — contiguous indices, no
/// missing parts, manifest sizes when present — BEFORE anything is written;
/// output streams through a staging file and is committed by rename.
pub fn combine_parts(
    first_part: &str,
    out_file: &str,
    cancel: &dyn Fn() -> bool,
) -> Result<CombineReport, FsError> {
    combine_parts_with(first_part, out_file, None, cancel, &no_progress)
}

/// `combine_parts` with a conflict policy (all of them, including rename)
/// and byte progress.
pub fn combine_parts_with(
    first_part: &str,
    out_file: &str,
    policy: OutputPolicy,
    cancel: &dyn Fn() -> bool,
    progress: ProgressFn,
) -> Result<CombineReport, FsError> {
    let first = Path::new(first_part);
    if !first.exists() {
        return Err(FsError::new(ErrorCode::NotFound, "Part not found").with_path(first_part));
    }
    let stem = first
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .and_then(|n| n.rsplit_once('.').map(|(s, _)| s.to_string()))
        .ok_or_else(|| {
            FsError::new(ErrorCode::InvalidPath, "Not a split part name").with_path(first_part)
        })?;
    let dir = first.parent().unwrap_or_else(|| Path::new("."));
    // Parts are "<stem>.001", "<stem>.002", … next to the first part.
    let prefix = format!("{stem}.");
    let mut part_files: Vec<(usize, PathBuf)> = Vec::new();
    for entry in fs::read_dir(dir).map_err(|e| FsError::from(e).with_path(dir.to_string_lossy()))? {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        let file_name = path.file_name().map(|n| n.to_string_lossy().into_owned());
        if !file_name.as_deref().is_some_and(|n| n.starts_with(&prefix)) {
            continue;
        }
        if let Some(index) = part_index(&path) {
            part_files.push((index, path));
        }
    }
    if part_files.is_empty() {
        return Err(FsError::new(
            ErrorCode::NotFound,
            "No numbered parts found next to the first part",
        )
        .with_path(first_part));
    }
    part_files.sort_by_key(|(index, _)| *index);

    // Validate the FULL sequence before writing anything.
    for (expected, (index, _)) in part_files.iter().enumerate() {
        let want = expected + 1;
        if *index != want {
            return Err(FsError::new(
                ErrorCode::NotFound,
                format!(
                    "Missing part {want:03} of {stem} — found part {index:03} instead; nothing was written"
                ),
            )
            .with_path(first_part));
        }
    }
    let manifest = load_manifest(first);
    if let Some(manifest) = &manifest
        && manifest.parts != part_files.len()
    {
        return Err(FsError::new(
            ErrorCode::NotFound,
            format!(
                "Manifest expects {} parts but {} exist; nothing was written",
                manifest.parts,
                part_files.len()
            ),
        )
        .with_path(first_part));
    }
    let mut total_bytes = 0u64;
    let mut newest_part: Option<SystemTime> = None;
    for (index, path) in &part_files {
        let actual =
            fs::metadata(path).map_err(|e| FsError::from(e).with_path(path.to_string_lossy()))?;
        total_bytes += actual.len();
        newest_part = newest_part.max(actual.modified().ok());
        if let Some(manifest) = &manifest {
            // The last part may legitimately be short.
            let expected_size = (*index != part_files.len()).then_some(manifest.part_size);
            if expected_size.is_some_and(|expected| actual.len() != expected) {
                return Err(FsError::new(
                    ErrorCode::Io,
                    format!(
                        "Part {index:03} is {} bytes but the manifest expects {:?}; nothing was written",
                        actual.len(),
                        expected_size
                    ),
                )
                .with_path(path.to_string_lossy()));
            }
        }
    }

    // Never write over one of the parts being read.
    if part_files.iter().any(|(_, p)| Path::new(out_file) == p) {
        return Err(
            FsError::new(ErrorCode::AlreadyExists, "Output file is one of the parts")
                .with_path(out_file),
        );
    }
    let output = match resolve_output(
        Path::new(out_file),
        newest_part,
        policy,
        true,
        "Output file",
    ) {
        Ok(Resolved::Write(target)) => target,
        Ok(Resolved::Skip) => {
            return Ok(CombineReport {
                output: out_file.to_string(),
                parts_used: 0,
                bytes: 0,
                sha256_verified: None,
                skipped: true,
            });
        }
        Err(e) => return Err(e),
    };
    let output = output.as_path();
    let out_string = output.to_string_lossy().into_owned();
    let out_file = out_string.as_str();
    let staging = output.with_file_name(format!("{STAGING_PREFIX}{}.partial", std::process::id()));
    let commit = |report: CombineReport| -> Result<CombineReport, FsError> {
        fs::rename(&staging, output).map_err(|e| FsError::from(e).with_path(out_file))?;
        Ok(report)
    };
    let cleanup = |staging: &Path| {
        let _ = fs::remove_file(staging);
    };

    let mut out = match fs::File::create(&staging) {
        Ok(f) => f,
        Err(e) => return Err(FsError::from(e).with_path(out_file)),
    };
    let mut hasher = sha2::Sha256::new();
    let mut bytes: u64 = 0;
    let mut buffer = vec![0u8; CHUNK];
    for (done, (_, part)) in part_files.iter().enumerate() {
        if let Err(e) = is_cancelled(cancel) {
            drop(out);
            cleanup(&staging);
            return Err(e);
        }
        let mut file = match fs::File::open(part) {
            Ok(f) => f,
            Err(e) => {
                cleanup(&staging);
                return Err(FsError::from(e).with_path(part.to_string_lossy()));
            }
        };
        loop {
            if let Err(e) = is_cancelled(cancel) {
                drop(out);
                cleanup(&staging);
                return Err(e);
            }
            match file.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    if let Err(e) = out.write_all(&buffer[..n]) {
                        cleanup(&staging);
                        return Err(FsError::from(e).with_path(out_file));
                    }
                    hasher.update(&buffer[..n]);
                    bytes += n as u64;
                    progress(&ToolProgress {
                        current: out_file,
                        processed_bytes: bytes,
                        total_bytes,
                        files_done: done as u64,
                        files_total: part_files.len() as u64,
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    cleanup(&staging);
                    return Err(FsError::from(e).with_path(part.to_string_lossy()));
                }
            }
        }
    }
    out.flush().map_err(|e| {
        cleanup(&staging);
        FsError::from(e).with_path(out_file)
    })?;
    drop(out);

    // Manifest content check happens on the staged bytes, before commit.
    if let Some(manifest) = &manifest {
        if manifest.size != bytes {
            cleanup(&staging);
            return Err(FsError::new(
                ErrorCode::Io,
                format!(
                    "Recombined size {bytes} does not match the manifest size {}; nothing was committed",
                    manifest.size
                ),
            )
            .with_path(out_file));
        }
        let actual = hex::encode(hasher.finalize());
        if actual != manifest.sha256 {
            cleanup(&staging);
            return Err(FsError::new(
                ErrorCode::Io,
                "Recombined content does not match the manifest SHA-256; nothing was committed",
            )
            .with_path(out_file));
        }
        return commit(CombineReport {
            output: out_file.to_string(),
            parts_used: part_files.len(),
            bytes,
            sha256_verified: Some(true),
            skipped: false,
        });
    }
    commit(CombineReport {
        output: out_file.to_string(),
        parts_used: part_files.len(),
        bytes,
        sha256_verified: None,
        skipped: false,
    })
}

// ── Queue job bodies ─────────────────────────────────────────────────────
// Tauri-free so the enqueue paths can be exercised on a real OperationQueue;
// `ipc.rs` only parses arguments and enqueues these.

fn queue_progress(p: &ToolProgress<'_>) -> OperationProgress {
    OperationProgress {
        operation_id: String::new(),
        kind: OperationKind::Create,
        state: OperationState::Running,
        processed_bytes: p.processed_bytes,
        total_bytes: (p.total_bytes > 0).then_some(p.total_bytes),
        processed_files: p.files_done,
        total_files: Some(p.files_total),
        current_path: Some(p.current.to_string()),
        rate_bytes_per_sec: None,
        error: None,
        failures: Vec::new(),
        queued_at_ms: None,
        finished_at_ms: None,
    }
}

fn cancelled_error() -> FsError {
    FsError::new(ErrorCode::Cancelled, "cancelled")
}

/// Job body for sidecar creation (retryable: build a fresh one per attempt).
pub fn checksum_create_job(
    paths: Vec<String>,
    algorithm: Algorithm,
    policy: OutputPolicy,
) -> JobBody {
    Box::new(move |cancel, report| {
        let outcome =
            create_checksums_with(&paths, algorithm, policy, &|| cancel.is_cancelled(), &|p| {
                report(queue_progress(p))
            });
        if cancel.is_cancelled() {
            return Err(cancelled_error());
        }
        let failed: Vec<&ToolOutcome> = outcome
            .outcomes
            .iter()
            .filter(|o| o.status == "failed")
            .collect();
        if !failed.is_empty() {
            return Err(FsError::new(
                ErrorCode::Io,
                format!(
                    "{} of {} checksum file(s) failed: {}",
                    failed.len(),
                    outcome.requested,
                    failed[0].message.as_deref().unwrap_or("error")
                ),
            ));
        }
        Ok(())
    })
}

pub fn split_job(path: String, part_bytes: u64, out_dir: String, policy: OutputPolicy) -> JobBody {
    Box::new(move |cancel, report| {
        let split = split_file_with(
            &path,
            part_bytes,
            &out_dir,
            policy,
            &|| cancel.is_cancelled(),
            &|p| report(queue_progress(p)),
        )?;
        let parts = split.parts.len() as u64;
        report(queue_progress(&ToolProgress {
            current: &path,
            processed_bytes: fs::metadata(&path).map(|m| m.len()).unwrap_or(0),
            total_bytes: 0,
            files_done: parts,
            files_total: parts,
        }));
        Ok(())
    })
}

pub fn combine_job(first_part: String, out_file: String, policy: OutputPolicy) -> JobBody {
    Box::new(move |cancel, report| {
        let done = combine_parts_with(
            &first_part,
            &out_file,
            policy,
            &|| cancel.is_cancelled(),
            &|p| report(queue_progress(p)),
        )?;
        report(queue_progress(&ToolProgress {
            current: &done.output,
            processed_bytes: done.bytes,
            total_bytes: done.bytes,
            files_done: done.parts_used as u64,
            files_total: done.parts_used as u64,
        }));
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive_zip::ZipProvider;
    use crate::ops::{CancelToken, OperationQueue};
    use std::cell::Cell;
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
    use std::time::Duration;

    static SEQ: AtomicU64 = AtomicU64::new(0);

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ferrum-f224-{tag}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn s(p: &Path) -> String {
        p.to_string_lossy().into_owned()
    }

    fn never() -> bool {
        false
    }

    #[test]
    fn known_digest_vectors_are_reproduced() {
        let root = temp("vectors");
        // "abc"
        let abc = root.join("abc.txt");
        fs::write(&abc, b"abc").unwrap();
        assert_eq!(
            hash_file(&abc, Algorithm::Sha256, &never).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hash_file(&abc, Algorithm::Sha1, &never).unwrap(),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        assert_eq!(
            hash_file(&abc, Algorithm::Md5, &never).unwrap(),
            "900150983cd24fb0d6963f7d28e17f72"
        );
        assert_eq!(
            hash_file(&abc, Algorithm::Crc32, &never).unwrap(),
            "352441c2"
        );
        // Empty file
        let empty = root.join("empty.txt");
        fs::write(&empty, b"").unwrap();
        assert_eq!(
            hash_file(&empty, Algorithm::Sha256, &never).unwrap(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn chunked_reading_matches_one_shot_for_multi_chunk_files() {
        let root = temp("chunks");
        let big = root.join("big.bin");
        let payload: Vec<u8> = (0..(2 * CHUNK + 77)).map(|i| (i % 251) as u8).collect();
        fs::write(&big, &payload).unwrap();
        let streamed = hash_file(&big, Algorithm::Sha256, &never).unwrap();
        let mut single = sha2::Sha256::new();
        single.update(&payload);
        assert_eq!(streamed, hex::encode(single.finalize()));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn cancellation_stops_the_hash_with_a_coded_error() {
        let root = temp("cancel");
        let big = root.join("big.bin");
        fs::write(&big, vec![7u8; CHUNK * 3]).unwrap();
        let first = AtomicBool::new(false);
        let err = hash_file(&big, Algorithm::Sha256, &|| {
            first.swap(true, Ordering::SeqCst)
        })
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::Cancelled);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn create_checksums_writes_standard_manifests_and_refuses_overwrite() {
        let root = temp("create");
        let file = root.join("notes ünïcode.txt");
        fs::write(&file, b"payload").unwrap();
        let report = create_checksums(&[s(&file)], Algorithm::Sha256, &never);
        assert_eq!(report.created, 1);
        let sidecar = root.join("notes ünïcode.txt.sha256");
        let content = fs::read_to_string(&sidecar).unwrap();
        let expected_hash = hash_file(&file, Algorithm::Sha256, &never).unwrap();
        assert_eq!(content, format!("{expected_hash}  notes ünïcode.txt\n"));
        // A second run refuses to clobber the sidecar.
        let again = create_checksums(&[s(&file)], Algorithm::Sha256, &never);
        assert_eq!(again.created, 0);
        assert_eq!(again.outcomes[0].status, "failed");
        assert!(
            again.outcomes[0]
                .message
                .as_deref()
                .unwrap()
                .contains("already exists")
        );
        // Directories are refused honestly.
        let dir_report = create_checksums(&[s(&root)], Algorithm::Sha256, &never);
        assert_eq!(dir_report.outcomes[0].status, "failed");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn verify_detects_ok_mismatch_and_missing_from_a_manifest() {
        let root = temp("verify");
        fs::write(root.join("good.txt"), b"one").unwrap();
        fs::write(root.join("bad.txt"), b"two").unwrap();
        let good_hash = hash_file(&root.join("good.txt"), Algorithm::Sha256, &never).unwrap();
        // Deliberately wrong digest planted for bad.txt (content drifted).
        let bad_hash = "0".repeat(64);
        let manifest_text = format!(
            "# comment line\n\n{good_hash}  good.txt\n{bad_hash}  bad.txt\nffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff  ghost.txt\n"
        );
        let manifest = root.join("batch.sha256");
        fs::write(&manifest, manifest_text).unwrap();
        let report = verify_checksums(&[s(&manifest)], &never);
        assert_eq!(report.ok, 1);
        assert_eq!(report.mismatched, 1);
        let statuses: Vec<&str> = report.outcomes.iter().map(|o| o.status.as_str()).collect();
        assert_eq!(statuses, vec!["ok", "mismatch", "missing"]);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn verify_a_file_against_its_sidecar_flags_bit_flips() {
        let root = temp("sidecar");
        let file = root.join("data.bin");
        fs::write(&file, b"stable bytes").unwrap();
        create_checksums(&[s(&file)], Algorithm::Sha256, &never);
        assert_eq!(
            verify_checksums(&[s(&file)], &never).outcomes[0].status,
            "ok"
        );
        // Bit flip: same length, different content.
        fs::write(
            &file,
            b"stable bytes".iter().map(|b| b ^ 1).collect::<Vec<u8>>(),
        )
        .unwrap();
        let outcome = &verify_checksums(&[s(&file)], &never).outcomes[0];
        assert_eq!(outcome.status, "mismatch");
        assert!(outcome.actual.as_deref().unwrap() != outcome.expected.as_deref().unwrap());
        // A file without any sidecar reports noManifest, not failure.
        let orphan = root.join("orphan.txt");
        fs::write(&orphan, b"x").unwrap();
        assert_eq!(
            verify_checksums(&[s(&orphan)], &never).outcomes[0].status,
            "noManifest"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn sfv_manifests_parse_in_the_single_space_format() {
        let root = temp("sfv");
        fs::write(root.join("old.zip"), b"legacy").unwrap();
        let crc = hash_file(&root.join("old.zip"), Algorithm::Crc32, &never).unwrap();
        fs::write(
            root.join("old.sfv"),
            format!("; generated by legacy tool\nold.zip {crc}\n"),
        )
        .unwrap();
        let report = verify_checksums(&[s(&root.join("old.sfv"))], &never);
        assert_eq!(report.ok, 1);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn split_then_combine_round_trips_bytes_exactly() {
        let root = temp("roundtrip");
        let src = root.join("book.txt");
        let payload: Vec<u8> = (0..250_000u32).map(|i| (i % 256) as u8).collect();
        fs::write(&src, &payload).unwrap();
        let parts_dir = root.join("parts");
        let report = split_file(&s(&src), 100_000, &s(&parts_dir), &never).unwrap();
        assert_eq!(report.parts.len(), 3);
        assert!(report.manifest.is_some());
        let combined = root.join("book.restored.txt");
        let done =
            combine_parts(&s(&parts_dir.join("book.txt.001")), &s(&combined), &never).unwrap();
        assert_eq!(done.parts_used, 3);
        assert_eq!(done.bytes, 250_000);
        assert_eq!(done.sha256_verified, Some(true));
        assert_eq!(fs::read(&combined).unwrap(), payload);
        // No staging leftovers.
        assert!(
            !root
                .join(format!("{STAGING_PREFIX}{}.partial", std::process::id()))
                .exists()
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn combine_refuses_missing_parts_before_writing_anything() {
        let root = temp("gap");
        let src = root.join("f.bin");
        fs::write(&src, vec![1u8; 50_000]).unwrap();
        split_file(&s(&src), 10_000, &s(&root), &never).unwrap();
        fs::remove_file(root.join("f.bin.002")).unwrap(); // punch a hole
        let out = root.join("f.out");
        let err = combine_parts(&s(&root.join("f.bin.001")), &s(&out), &never).unwrap_err();
        assert!(err.message.contains("Missing part 002"), "{}", err.message);
        assert!(!out.exists(), "no output on refusal");
        assert!(
            !root
                .join(format!("{STAGING_PREFIX}{}.partial", std::process::id()))
                .exists()
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn combine_refuses_wrong_part_sizes_from_the_manifest() {
        let root = temp("sizes");
        let src = root.join("s.bin");
        fs::write(&src, vec![2u8; 30_000]).unwrap();
        split_file(&s(&src), 10_000, &s(&root), &never).unwrap();
        // Corrupt part 1 by appending bytes — size no longer matches.
        let mut corrupted = fs::read(root.join("s.bin.001")).unwrap();
        corrupted.push(0xAA);
        fs::write(root.join("s.bin.001"), &corrupted).unwrap();
        let out = root.join("s.out");
        let err = combine_parts(&s(&root.join("s.bin.001")), &s(&out), &never).unwrap_err();
        assert!(
            err.message.contains("nothing was written"),
            "{}",
            err.message
        );
        assert!(!out.exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn combine_refuses_an_existing_output_and_reports_part_order() {
        let root = temp("order");
        let src = root.join("o.bin");
        fs::write(&src, vec![3u8; 20_000]).unwrap();
        split_file(&s(&src), 10_000, &s(&root), &never).unwrap();
        let out = root.join("o.bin");
        // Output colliding with a source name is refused outright.
        let err = combine_parts(&s(&root.join("o.bin.001")), &s(&out), &never).unwrap_err();
        assert_eq!(err.code, ErrorCode::AlreadyExists);
        // Zero-length part is not a valid index name.
        assert_eq!(part_index(Path::new("x.abc")), None);
        assert_eq!(part_index(Path::new("x.12")), None);
        assert_eq!(part_index(Path::new("x.007")), Some(7));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn split_refuses_zero_part_size_and_existing_parts() {
        let root = temp("refuse");
        let src = root.join("r.bin");
        fs::write(&src, b"x").unwrap();
        assert!(split_file(&s(&src), 0, &s(&root), &never).is_err());
        split_file(&s(&src), 1, &s(&root), &never).unwrap();
        let err = split_file(&s(&src), 1, &s(&root), &never).unwrap_err();
        assert_eq!(err.code, ErrorCode::AlreadyExists);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn cancel_during_combine_removes_the_staging_file() {
        let root = temp("combinecancel");
        let src = root.join("c.bin");
        fs::write(&src, vec![7u8; 30_000]).unwrap();
        let parts = root.join("parts");
        split_file(&s(&src), 10_000, &s(&parts), &never).unwrap();
        let out = root.join("out").join("c.restored");
        fs::create_dir_all(out.parent().unwrap()).unwrap();
        let polls = AtomicUsize::new(0);
        let err = combine_parts(&s(&parts.join("c.bin.001")), &s(&out), &|| {
            polls.fetch_add(1, Ordering::SeqCst) >= 2
        })
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::Cancelled);
        assert!(!out.exists(), "no final output after a cancel");
        let leftovers: Vec<_> = fs::read_dir(out.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert!(leftovers.is_empty(), "staging leaked: {leftovers:?}");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn more_than_999_parts_widen_the_suffix_and_still_combine() {
        let root = temp("manyparts");
        let src = root.join("many.bin");
        let payload: Vec<u8> = (0..1_050u32).map(|i| (i % 251) as u8).collect();
        fs::write(&src, &payload).unwrap();
        let parts = root.join("parts");
        let report = split_file(&s(&src), 1, &s(&parts), &never).unwrap();
        assert_eq!(report.parts.len(), 1_050);
        assert!(parts.join("many.bin.0001").exists());
        assert!(parts.join("many.bin.1050").exists());
        assert!(!parts.join("many.bin.001").exists());
        let out = root.join("many.restored");
        let done = combine_parts(&s(&parts.join("many.bin.0001")), &s(&out), &never).unwrap();
        assert_eq!(done.parts_used, 1_050);
        assert_eq!(fs::read(&out).unwrap(), payload);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn unicode_names_survive_split_and_combine() {
        let root = temp("unicode");
        let src = root.join("résumé – bö.txt");
        let payload = "ünïcode payload".as_bytes().to_vec();
        fs::write(&src, &payload).unwrap();
        let report = split_file(&s(&src), 4, &s(&root), &never).unwrap();
        // "ünïcode payload" is 17 bytes in UTF-8 → 5 parts of ≤4 bytes.
        assert_eq!(report.parts.len(), 5);
        let combined = root.join("réc.txt");
        combine_parts(&report.parts[0], &s(&combined), &never).unwrap();
        assert_eq!(fs::read(&combined).unwrap(), payload);
        let _ = fs::remove_dir_all(&root);
    }

    // ── helpers for the FER-385 follow-up tests ──────────────────────────

    /// Sorted file names of `dir` (files and folders).
    fn names(dir: &Path) -> Vec<String> {
        let mut out: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        out.sort();
        out
    }

    /// Anything a failed run could leak: staging files of any tool.
    fn staging_leftovers(dir: &Path) -> Vec<String> {
        names(dir)
            .into_iter()
            .filter(|n| n.starts_with(".ferrum-"))
            .collect()
    }

    fn set_mtime(path: &Path, when: SystemTime) {
        fs::OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(when)
            .unwrap();
    }

    /// Distinct, non-repeating-per-part payload.
    fn payload(len: usize) -> Vec<u8> {
        (0..len).map(|i| ((i * 7 + i / 251) % 256) as u8).collect()
    }

    // ── FER-132: split staging ───────────────────────────────────────────

    #[test]
    fn cancel_during_split_leaves_no_partial_final_output() {
        let root = temp("splitcancel");
        let src = root.join("c.bin");
        fs::write(&src, vec![9u8; 30_000]).unwrap();
        let polls = AtomicUsize::new(0);
        let saw_staged_part = AtomicBool::new(false);
        // The one 30 kB read yields 3 parts; polls: 0 = before the read,
        // 1 = opening part 1, 2 = opening part 2 (part 1 is already staged).
        // Cancelling on poll 2 is therefore genuinely mid-split.
        let err = split_file(&s(&src), 10_000, &s(&root), &|| {
            if polls.fetch_add(1, Ordering::SeqCst) < 2 {
                return false;
            }
            let staged = names(&root).iter().any(|n| n.ends_with(".partial"));
            saw_staged_part.store(staged, Ordering::SeqCst);
            true
        })
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::Cancelled);
        assert!(
            saw_staged_part.load(Ordering::SeqCst),
            "the cancel must fire after a part was already staged"
        );
        // Only the source survives: no .00N part, no manifest, no staging.
        assert_eq!(names(&root), vec!["c.bin".to_string()]);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn io_error_mid_split_removes_staged_parts_and_writes_no_manifest() {
        let root = temp("splitio");
        let src = root.join("f.bin");
        fs::write(&src, payload(30_000)).unwrap();
        // A folder squatting on part 2's staging name makes creating it fail
        // after part 1 was staged and written.
        let pid = std::process::id();
        let squat = root.join(format!("{SPLIT_STAGING_PREFIX}{pid}.002.partial"));
        fs::create_dir(&squat).unwrap();
        let err = split_file(&s(&src), 10_000, &s(&root), &never).unwrap_err();
        assert_ne!(err.code, ErrorCode::Cancelled);
        let left = names(&root);
        assert!(
            left.iter().all(|n| !n.starts_with("f.bin.")),
            "no part or manifest may be published: {left:?}"
        );
        assert!(squat.is_dir());
        assert!(
            !root
                .join(format!("{SPLIT_STAGING_PREFIX}{pid}.001.partial"))
                .exists()
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn split_publishes_nothing_until_every_part_is_staged() {
        let root = temp("splitstage");
        let src = root.join("f.bin");
        fs::write(&src, payload(30_000)).unwrap();
        let observed = Cell::new(false);
        // Progress fires after the chunk is fully staged and before commit.
        let report = split_file_with(&s(&src), 10_000, &s(&root), None, &never, &|_| {
            let n = names(&root);
            assert!(n.iter().all(|x| !x.starts_with("f.bin.")), "{n:?}");
            assert_eq!(n.iter().filter(|x| x.ends_with(".partial")).count(), 3);
            observed.set(true);
        })
        .unwrap();
        assert!(observed.get());
        assert_eq!(report.parts.len(), 3);
        assert!(staging_leftovers(&root).is_empty());
        assert!(root.join("f.bin.parts.json").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn commit_failure_rolls_back_already_renamed_parts() {
        let root = temp("splitcommit");
        let src = root.join("f.bin");
        fs::write(&src, payload(30_000)).unwrap();
        // A folder where part 2 must land: its rename fails, after part 1
        // already moved into place.
        fs::create_dir(root.join("f.bin.002")).unwrap();
        let err = split_file_with(
            &s(&src),
            10_000,
            &s(&root),
            Some(ConflictPolicy::Overwrite),
            &never,
            &no_progress,
        )
        .unwrap_err();
        assert_ne!(err.code, ErrorCode::Cancelled);
        assert_eq!(
            names(&root),
            vec!["f.bin".to_string(), "f.bin.002".to_string()]
        );
        assert!(root.join("f.bin.002").is_dir());
        let _ = fs::remove_dir_all(&root);
    }

    // ── FER-132 / FER-134: combine integrity ─────────────────────────────

    /// Split a 30 kB file into three 10 kB parts and return (root, src bytes).
    fn three_parts(tag: &str) -> (PathBuf, Vec<u8>) {
        let root = temp(tag);
        let data = payload(30_000);
        fs::write(root.join("f.bin"), &data).unwrap();
        split_file(&s(&root.join("f.bin")), 10_000, &s(&root), &never).unwrap();
        (root, data)
    }

    fn assert_combine_refused_cleanly(root: &Path, expected_message: &str) {
        let out = root.join("f.out");
        let err = combine_parts(&s(&root.join("f.bin.001")), &s(&out), &never).unwrap_err();
        assert_eq!(err.code, ErrorCode::Io);
        assert!(err.message.contains(expected_message), "{}", err.message);
        assert!(!out.exists(), "a refused combine must not commit an output");
        assert!(
            staging_leftovers(root).is_empty(),
            "staging must be removed"
        );
    }

    #[test]
    fn same_size_flipped_part_fails_the_manifest_sha256_and_commits_nothing() {
        let (root, _) = three_parts("flipwhole");
        // Same length (passes the size checks), every byte inverted.
        let flipped: Vec<u8> = fs::read(root.join("f.bin.002"))
            .unwrap()
            .iter()
            .map(|b| !b)
            .collect();
        assert_eq!(flipped.len(), 10_000);
        fs::write(root.join("f.bin.002"), flipped).unwrap();
        assert_combine_refused_cleanly(&root, "SHA-256");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn single_bit_flip_inside_a_part_is_caught_at_combine() {
        // Part > one chunk so the flip lands in the second streamed chunk.
        let root = temp("bitflip");
        let data = payload(CHUNK * 2 + 4_096);
        fs::write(root.join("f.bin"), &data).unwrap();
        let part_size = (CHUNK + 1_000) as u64;
        split_file(&s(&root.join("f.bin")), part_size, &s(&root), &never).unwrap();
        let mut part = fs::read(root.join("f.bin.001")).unwrap();
        part[CHUNK + 10] ^= 0x01;
        fs::write(root.join("f.bin.001"), part).unwrap();
        assert_combine_refused_cleanly(&root, "SHA-256");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn swapped_equal_size_parts_are_rejected_by_the_manifest_hash() {
        let (root, data) = three_parts("swapped");
        // Parts 1 and 2 swap places: same names, same sizes, wrong order.
        let first = fs::read(root.join("f.bin.001")).unwrap();
        let second = fs::read(root.join("f.bin.002")).unwrap();
        assert_ne!(first, second);
        fs::write(root.join("f.bin.001"), &second).unwrap();
        fs::write(root.join("f.bin.002"), &first).unwrap();
        assert_combine_refused_cleanly(&root, "SHA-256");
        // Restoring the order makes the very same set combine again.
        fs::write(root.join("f.bin.001"), &first).unwrap();
        fs::write(root.join("f.bin.002"), &second).unwrap();
        let out = root.join("f.out");
        combine_parts(&s(&root.join("f.bin.001")), &s(&out), &never).unwrap();
        assert_eq!(fs::read(&out).unwrap(), data);
        let _ = fs::remove_dir_all(&root);
    }

    // ── FER-134: bounded memory ──────────────────────────────────────────

    /// Reader that records the largest buffer it was ever asked to fill.
    struct CountingReader {
        remaining: usize,
        max_request: usize,
        reads: usize,
    }

    impl Read for CountingReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.max_request = self.max_request.max(buf.len());
            self.reads += 1;
            let n = buf.len().min(self.remaining);
            buf[..n].fill(0xAB);
            self.remaining -= n;
            Ok(n)
        }
    }

    #[test]
    fn hashing_a_stream_far_larger_than_the_chunk_uses_one_chunk_buffer() {
        // 64 chunks: if memory scaled with input, the largest read request
        // would be the whole stream, not CHUNK.
        let total = 64 * CHUNK + 123;
        let mut reader = CountingReader {
            remaining: total,
            max_request: 0,
            reads: 0,
        };
        let mut deltas: Vec<u64> = Vec::new();
        let digest = hash_stream(&mut reader, Algorithm::Sha256, &never, &mut |d| {
            deltas.push(d)
        })
        .unwrap();
        assert_eq!(reader.max_request, CHUNK, "one fixed-size buffer");
        assert_eq!(reader.reads, 66, "64 full chunks + tail + EOF probe");
        assert!(deltas.iter().all(|d| *d as usize <= CHUNK));
        assert_eq!(deltas.iter().sum::<u64>() as usize, total);
        let mut expected = sha2::Sha256::new();
        expected.update(vec![0xABu8; total]);
        assert_eq!(digest, hex::encode(expected.finalize()));
    }

    #[test]
    fn split_and_combine_progress_advances_one_chunk_at_a_time() {
        let root = temp("chunked");
        let src = root.join("big.bin");
        let data = payload(3 * CHUNK + 500);
        fs::write(&src, &data).unwrap();
        let split_steps = std::cell::RefCell::new(Vec::<u64>::new());
        let part_size = (CHUNK + 700) as u64;
        split_file_with(&s(&src), part_size, &s(&root), None, &never, &|p| {
            split_steps.borrow_mut().push(p.processed_bytes);
            assert_eq!(p.total_bytes, data.len() as u64);
        })
        .unwrap();
        let steps = split_steps.into_inner();
        assert_eq!(steps.len(), 4, "3 full chunks + tail, not one giant read");
        assert!(steps.windows(2).all(|w| w[1] - w[0] <= CHUNK as u64));
        assert_eq!(*steps.last().unwrap(), data.len() as u64);

        let combine_steps = std::cell::RefCell::new(Vec::<u64>::new());
        let out = root.join("big.out");
        let first = s(&root.join("big.bin.001"));
        combine_parts_with(&first, &s(&out), None, &never, &|p| {
            combine_steps.borrow_mut().push(p.processed_bytes);
            assert_eq!(p.total_bytes, data.len() as u64);
        })
        .unwrap();
        let steps = combine_steps.into_inner();
        assert!(steps.len() >= 4);
        let mut previous = 0;
        for step in &steps {
            assert!(step - previous <= CHUNK as u64, "no read above one chunk");
            previous = *step;
        }
        assert_eq!(previous, data.len() as u64);
        assert_eq!(fs::read(&out).unwrap(), data);
        let _ = fs::remove_dir_all(&root);
    }

    // ── FER-131: manifests, verify cancel, atomic sidecars ───────────────

    #[test]
    fn manifest_parser_reads_gnu_text_gnu_binary_and_sfv_lines() {
        let text = "# header\nd41d8cd98f00b204e9800998ecf8427e  plain.txt\r\n\
                    0123abcd *spaced bin.bin\n; sfv comment\nmy file.zip 352441c2\n";
        let entries = parse_manifest(text);
        let pairs: Vec<(&str, &str)> = entries
            .iter()
            .map(|e| (e.hash.as_str(), e.name.as_str()))
            .collect();
        assert_eq!(
            pairs,
            vec![
                ("d41d8cd98f00b204e9800998ecf8427e", "plain.txt"),
                ("0123abcd", "spaced bin.bin"),
                ("352441c2", "my file.zip"),
            ]
        );
    }

    #[test]
    fn md5_and_sha1_manifests_verify_including_the_gnu_binary_star_form() {
        let root = temp("legacy");
        fs::write(root.join("a.txt"), b"abc").unwrap();
        fs::write(root.join("b.bin"), b"abc").unwrap();
        fs::write(root.join("c.bin"), b"abd").unwrap();
        let md5_abc = "900150983cd24fb0d6963f7d28e17f72";
        fs::write(
            root.join("sums.md5"),
            format!("{md5_abc}  a.txt\n{md5_abc} *b.bin\n{md5_abc} *c.bin\n"),
        )
        .unwrap();
        let md5 = verify_checksums(&[s(&root.join("sums.md5"))], &never);
        let statuses: Vec<&str> = md5.outcomes.iter().map(|o| o.status.as_str()).collect();
        assert_eq!(statuses, vec!["ok", "ok", "mismatch"]);
        assert_eq!((md5.ok, md5.mismatched), (2, 1));
        // The binary-mode name resolves to the real file, not "*b.bin".
        assert!(md5.outcomes[1].path.ends_with("b.bin"));

        let sha1_abc = "a9993e364706816aba3e25717850c26c9cd0d89d";
        fs::write(
            root.join("sums.sha1"),
            format!("{sha1_abc} *b.bin\n{sha1_abc}  a.txt\n"),
        )
        .unwrap();
        let sha1 = verify_checksums(&[s(&root.join("sums.sha1"))], &never);
        assert_eq!((sha1.ok, sha1.mismatched), (2, 0));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn legacy_sidecars_round_trip_through_create_and_verify() {
        let root = temp("legacyrt");
        let file = root.join("data.bin");
        fs::write(&file, b"abc").unwrap();
        for (algorithm, ext) in [(Algorithm::Md5, "md5"), (Algorithm::Sha1, "sha1")] {
            let made = create_checksums(&[s(&file)], algorithm, &never);
            assert_eq!(made.created, 1);
            assert!(root.join(format!("data.bin.{ext}")).exists());
        }
        // verify_against_sidecar tries sha256 → sha1 → md5 → sfv: only the
        // legacy sidecars exist here, and they must be honoured.
        let report = verify_checksums(&[s(&file)], &never);
        assert_eq!(report.outcomes[0].status, "ok");
        let md5 = verify_checksums(&[s(&root.join("data.bin.md5"))], &never);
        assert_eq!(md5.ok, 1);
        assert_eq!(
            md5.outcomes[0].actual.as_deref(),
            Some("900150983cd24fb0d6963f7d28e17f72")
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn verify_can_be_cancelled_mid_hash_by_request_id() {
        let root = temp("verifycancel");
        let big = root.join("big.bin");
        fs::write(&big, payload(3 * CHUNK)).unwrap();
        create_checksums(&[s(&big)], Algorithm::Sha256, &never);
        let id = format!("verify-test-{}", std::process::id());
        let fired = Cell::new(false);
        let report = run_verify_cancellable(&id, &[s(&big)], &|_| {
            if !fired.replace(true) {
                assert!(cancel_request(&id), "the run is registered while hashing");
            }
        });
        assert!(report.cancelled);
        assert!(report.outcomes.is_empty(), "no half-hashed failure entry");
        assert_eq!((report.ok, report.mismatched), (0, 0));
        assert!(!cancel_request(&id), "registration ends with the run");
        // Without a cancel the same request completes normally.
        let done = run_verify_cancellable(&id, &[s(&big)], &|_| {});
        assert!(!done.cancelled);
        assert_eq!(done.ok, 1);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn cancelled_verify_stops_before_the_remaining_files() {
        let root = temp("verifystop");
        let mut lines = String::new();
        for name in ["a.bin", "b.bin", "c.bin"] {
            fs::write(root.join(name), name.as_bytes()).unwrap();
            let hash = hash_file(&root.join(name), Algorithm::Sha256, &never).unwrap();
            lines.push_str(&format!("{hash}  {name}\n"));
        }
        fs::write(root.join("all.sha256"), lines).unwrap();
        let hashed = AtomicUsize::new(0);
        let report = verify_checksums_with(
            &[s(&root.join("all.sha256"))],
            &|| hashed.load(Ordering::SeqCst) >= 2,
            &|_| {
                hashed.fetch_add(1, Ordering::SeqCst);
            },
        );
        assert!(report.cancelled);
        assert_eq!(report.outcomes.len(), 1, "a.bin finished, b/c never ran");
        assert_eq!(report.ok, 1);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn cancelled_create_batch_stops_and_leaves_no_staging() {
        let root = temp("createcancel");
        let files: Vec<String> = (0..3)
            .map(|i| {
                let f = root.join(format!("f{i}.bin"));
                fs::write(&f, [i as u8]).unwrap();
                s(&f)
            })
            .collect();
        let done = AtomicUsize::new(0);
        let report = create_checksums_with(
            &files,
            Algorithm::Sha256,
            None,
            &|| done.load(Ordering::SeqCst) >= 2,
            &|_| {
                done.fetch_add(1, Ordering::SeqCst);
            },
        );
        assert_eq!(report.created, 1);
        assert_eq!(report.outcomes.len(), 1);
        assert!(!root.join("f1.bin.sha256").exists());
        assert!(staging_leftovers(&root).is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn sidecars_are_written_through_a_temp_file_that_never_lingers() {
        let root = temp("atomic");
        let file = root.join("x.bin");
        fs::write(&file, b"x").unwrap();
        let absent_during_hash = Cell::new(false);
        let report = create_checksums_with(&[s(&file)], Algorithm::Sha256, None, &never, &|_| {
            // While hashing, the final sidecar must not exist yet.
            absent_during_hash.set(!root.join("x.bin.sha256").exists());
        });
        assert!(absent_during_hash.get());
        assert_eq!(report.created, 1);
        assert!(root.join("x.bin.sha256").exists());
        assert!(staging_leftovers(&root).is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    // ── FER-133: conflict policy ─────────────────────────────────────────

    #[test]
    fn sidecar_policy_skip_overwrite_and_fixed_names() {
        let root = temp("sidecarpolicy");
        let file = root.join("d.bin");
        fs::write(&file, b"abc").unwrap();
        let sidecar = root.join("d.bin.sha256");
        fs::write(&sidecar, "stale\n").unwrap();
        let run = |policy: OutputPolicy| {
            create_checksums_with(&[s(&file)], Algorithm::Sha256, policy, &never, &no_progress)
        };

        // Default and Ask refuse; the stale sidecar stays.
        for policy in [None, Some(ConflictPolicy::Ask)] {
            let r = run(policy);
            assert_eq!(r.outcomes[0].status, "failed");
            let message = r.outcomes[0].message.as_deref().unwrap();
            assert!(message.contains("already exists"), "{message}");
        }
        // Rename is refused: a sidecar's name is its contract.
        let r = run(Some(ConflictPolicy::RenameAuto));
        assert_eq!(r.outcomes[0].status, "failed");
        let message = r.outcomes[0].message.as_deref().unwrap();
        assert!(message.contains("cannot be auto-renamed"), "{message}");
        assert_eq!(fs::read_to_string(&sidecar).unwrap(), "stale\n");

        let r = run(Some(ConflictPolicy::Skip));
        assert_eq!(r.outcomes[0].status, "skipped");
        assert_eq!(r.created, 0);
        assert_eq!(fs::read_to_string(&sidecar).unwrap(), "stale\n");

        // Overwrite-older: sidecar newer than the file → skipped; older → replaced.
        set_mtime(&file, SystemTime::now() - Duration::from_secs(7200));
        set_mtime(&sidecar, SystemTime::now());
        let r = run(Some(ConflictPolicy::OverwriteOlder));
        assert_eq!(r.outcomes[0].status, "skipped");
        set_mtime(&sidecar, SystemTime::now() - Duration::from_secs(14_400));
        let r = run(Some(ConflictPolicy::OverwriteOlder));
        assert_eq!(r.outcomes[0].status, "created");

        fs::write(&sidecar, "stale again\n").unwrap();
        let r = run(Some(ConflictPolicy::Overwrite));
        assert_eq!(r.outcomes[0].status, "created");
        assert_eq!(
            fs::read_to_string(&sidecar).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad  d.bin\n"
        );
        assert!(staging_leftovers(&root).is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn split_policy_skip_overwrite_and_stale_parts() {
        let root = temp("splitpolicy");
        let src = root.join("s.bin");
        let data = payload(5_000);
        fs::write(&src, &data).unwrap();
        split_file(&s(&src), 1_000, &s(&root), &never).unwrap(); // 5 parts
        let manifest_before = fs::read_to_string(root.join("s.bin.parts.json")).unwrap();
        let run = |part: u64, policy: OutputPolicy| {
            split_file_with(&s(&src), part, &s(&root), policy, &never, &no_progress)
        };

        assert_eq!(run(2_500, None).unwrap_err().code, ErrorCode::AlreadyExists);
        assert_eq!(
            run(2_500, Some(ConflictPolicy::Ask)).unwrap_err().code,
            ErrorCode::AlreadyExists
        );
        let err = run(2_500, Some(ConflictPolicy::RenameAuto)).unwrap_err();
        assert_eq!(err.code, ErrorCode::AlreadyExists);
        assert!(err.message.contains("cannot be auto-renamed"));

        let skipped = run(2_500, Some(ConflictPolicy::Skip)).unwrap();
        assert!(skipped.skipped && skipped.parts.is_empty() && skipped.manifest.is_none());
        assert_eq!(
            fs::read_to_string(root.join("s.bin.parts.json")).unwrap(),
            manifest_before
        );

        // Overwrite-older: existing parts are newer than the source → skipped.
        set_mtime(&src, SystemTime::now() - Duration::from_secs(7200));
        assert!(
            run(2_500, Some(ConflictPolicy::OverwriteOlder))
                .unwrap()
                .skipped
        );
        // Source newer than every existing member → replaced.
        set_mtime(&src, SystemTime::now() + Duration::from_secs(7200));
        assert!(
            !run(2_500, Some(ConflictPolicy::OverwriteOlder))
                .unwrap()
                .skipped
        );

        // Overwrite with a coarser split must not leave the old .003-.005
        // behind, or the new 2-part set could no longer be combined.
        let report = run(2_500, Some(ConflictPolicy::Overwrite)).unwrap();
        assert_eq!(report.parts.len(), 2);
        assert_eq!(
            names(&root),
            vec![
                "s.bin".to_string(),
                "s.bin.001".to_string(),
                "s.bin.002".to_string(),
                "s.bin.parts.json".to_string()
            ]
        );
        let out = root.join("s.out");
        combine_parts(&s(&root.join("s.bin.001")), &s(&out), &never).unwrap();
        assert_eq!(fs::read(&out).unwrap(), data);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn combine_policy_skip_overwrite_rename_and_part_protection() {
        let (root, data) = three_parts("combinepolicy");
        let first = s(&root.join("f.bin.001"));
        let out = root.join("f.out");
        fs::write(&out, b"existing").unwrap();
        let run = |target: &Path, policy: OutputPolicy| {
            combine_parts_with(&first, &s(target), policy, &never, &no_progress)
        };

        for policy in [None, Some(ConflictPolicy::Ask)] {
            assert_eq!(
                run(&out, policy).unwrap_err().code,
                ErrorCode::AlreadyExists
            );
        }
        let skipped = run(&out, Some(ConflictPolicy::Skip)).unwrap();
        assert!(skipped.skipped);
        assert_eq!(fs::read(&out).unwrap(), b"existing");

        // Auto-rename keeps the existing file and writes "f (2).out".
        let renamed = run(&out, Some(ConflictPolicy::RenameAuto)).unwrap();
        assert!(renamed.output.ends_with("f (2).out"), "{}", renamed.output);
        assert_eq!(fs::read(root.join("f (2).out")).unwrap(), data);
        assert_eq!(fs::read(&out).unwrap(), b"existing");
        let again = run(&out, Some(ConflictPolicy::RenameAuto)).unwrap();
        assert!(again.output.ends_with("f (3).out"), "{}", again.output);

        // Overwrite-older: the output is newer than every part → skipped.
        for n in 1..=3 {
            let part = root.join(format!("f.bin.00{n}"));
            set_mtime(&part, SystemTime::now() - Duration::from_secs(7200));
        }
        assert!(
            run(&out, Some(ConflictPolicy::OverwriteOlder))
                .unwrap()
                .skipped
        );
        set_mtime(&out, SystemTime::now() - Duration::from_secs(14_400));
        assert!(
            !run(&out, Some(ConflictPolicy::OverwriteOlder))
                .unwrap()
                .skipped
        );
        assert_eq!(fs::read(&out).unwrap(), data);

        fs::write(&out, b"existing").unwrap();
        let done = run(&out, Some(ConflictPolicy::Overwrite)).unwrap();
        assert_eq!(done.sha256_verified, Some(true));
        assert_eq!(fs::read(&out).unwrap(), data);

        // A part can never be the output, whatever the policy.
        let part = root.join("f.bin.002");
        let err = run(&part, Some(ConflictPolicy::Overwrite)).unwrap_err();
        assert_eq!(err.code, ErrorCode::AlreadyExists);
        assert_eq!(fs::read(&part).unwrap().len(), 10_000);
        assert!(staging_leftovers(&root).is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    // ── FER-133: byte progress ───────────────────────────────────────────

    #[test]
    fn create_and_verify_report_processed_bytes() {
        let root = temp("hashprogress");
        let file = root.join("p.bin");
        let data = payload(2 * CHUNK + 10);
        fs::write(&file, &data).unwrap();
        let seen = std::cell::RefCell::new(Vec::<(u64, u64)>::new());
        create_checksums_with(&[s(&file)], Algorithm::Sha256, None, &never, &|p| {
            seen.borrow_mut().push((p.processed_bytes, p.total_bytes));
        });
        let seen = seen.into_inner();
        assert_eq!(seen.len(), 3);
        assert!(seen.iter().all(|(_, total)| *total == data.len() as u64));
        assert_eq!(seen.last().unwrap().0, data.len() as u64);

        let processed = Cell::new(0u64);
        verify_checksums_with(&[s(&file)], &never, &|p| processed.set(p.processed_bytes));
        assert_eq!(processed.get(), data.len() as u64);
        let _ = fs::remove_dir_all(&root);
    }

    // ── FER-133: archive member checksums (no extraction) ────────────────

    fn make_zip(path: &Path, members: &[(&str, &[u8])]) {
        let mut writer = zip::ZipWriter::new(fs::File::create(path).unwrap());
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, bytes) in members {
            writer.start_file(*name, options).unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer.finish().unwrap();
    }

    #[test]
    fn zip_member_hash_matches_the_file_hash_without_extracting() {
        let root = temp("zipmember");
        let big = payload(3 * CHUNK + 17);
        make_zip(
            &root.join("pack.zip"),
            &[("docs/readme.txt", b"abc"), ("data/big.bin", &big)],
        );
        let provider = ZipProvider::new();
        let archive = s(&root.join("pack.zip"));
        let before = names(&root);

        let mut hashed = 0u64;
        let mut max_delta = 0u64;
        let sha = hash_archive_member(
            &provider,
            &archive,
            "data/big.bin",
            Algorithm::Sha256,
            None,
            &never,
            &mut |d| {
                hashed += d;
                max_delta = max_delta.max(d);
            },
        )
        .unwrap();
        fs::write(root.join("expected.bin"), &big).unwrap();
        let expected = hash_file(&root.join("expected.bin"), Algorithm::Sha256, &never).unwrap();
        assert_eq!(sha, expected);
        assert_eq!(hashed, big.len() as u64);
        assert!(
            max_delta as usize <= CHUNK,
            "chunks stay bounded: {max_delta}"
        );

        let md5 = hash_archive_member(
            &provider,
            &archive,
            "docs/readme.txt",
            Algorithm::Md5,
            None,
            &never,
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(md5, "900150983cd24fb0d6963f7d28e17f72");
        // Nothing was extracted next to the archive.
        let mut after = names(&root);
        after.retain(|n| n != "expected.bin");
        assert_eq!(after, before);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn archive_member_hash_reports_missing_members_and_honours_cancel() {
        let root = temp("zipmembercancel");
        let big = payload(3 * CHUNK);
        make_zip(&root.join("pack.zip"), &[("big.bin", &big)]);
        let provider = ZipProvider::new();
        let archive = s(&root.join("pack.zip"));

        let missing = hash_archive_member(
            &provider,
            &archive,
            "nope.bin",
            Algorithm::Sha256,
            None,
            &never,
            &mut |_| {},
        )
        .unwrap_err();
        assert!(missing.message.contains("not found"), "{}", missing.message);

        let bytes = Cell::new(0u64);
        let err = hash_archive_member(
            &provider,
            &archive,
            "big.bin",
            Algorithm::Sha256,
            None,
            &|| bytes.get() >= CHUNK as u64,
            &mut |d| bytes.set(bytes.get() + d),
        )
        .unwrap_err();
        assert_eq!(err.code, ArchiveErrorCode::Cancelled);
        assert!(bytes.get() < big.len() as u64, "stopped mid-member");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn sevenz_member_hash_streams_through_the_provider() {
        let root = temp("sevenzmember");
        let big = payload(2 * CHUNK + 33);
        let source = root.join("inner.bin");
        fs::write(&source, &big).unwrap();
        let archive = root.join("pack.7z");
        let mut writer = sevenz_rust2::ArchiveWriter::create(&archive).unwrap();
        writer
            .push_archive_entry(
                sevenz_rust2::ArchiveEntry::new_file("inner.bin"),
                Some(fs::File::open(&source).unwrap()),
            )
            .unwrap();
        writer.finish().unwrap();
        let provider = crate::archive_sevenz::SevenZProvider::new();
        let sha = hash_archive_member(
            &provider,
            &s(&archive),
            "inner.bin",
            Algorithm::Sha256,
            None,
            &never,
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(sha, hash_file(&source, Algorithm::Sha256, &never).unwrap());
        let _ = fs::remove_dir_all(&root);
    }

    // ── FER-133: the enqueue paths on a real operation queue ─────────────

    /// Enqueue like the Tauri commands do (retryable) and wait for the end.
    fn run_queued(
        queue: &Arc<OperationQueue>,
        make: impl Fn() -> JobBody + Send + Sync + 'static,
    ) -> (String, OperationProgress) {
        let id = queue.enqueue_retryable(OperationKind::Create, make(), Arc::new(make));
        wait_terminal(queue, &id)
    }

    fn wait_terminal(queue: &Arc<OperationQueue>, id: &str) -> (String, OperationProgress) {
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            let snap = queue.snapshot();
            if let Some(p) = snap.iter().find(|p| p.operation_id == id)
                && !matches!(p.state, OperationState::Queued | OperationState::Running)
            {
                return (id.to_string(), p.clone());
            }
            assert!(
                std::time::Instant::now() < deadline,
                "queue timed out: {snap:?}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn test_queue() -> Arc<OperationQueue> {
        OperationQueue::new(Box::new(|_| {}))
    }

    #[test]
    fn queued_checksum_create_reports_bytes_and_fails_per_policy_then_retries() {
        let root = temp("jobcreate");
        let file = root.join("j.bin");
        let data = payload(CHUNK + 5);
        fs::write(&file, &data).unwrap();
        let queue = test_queue();
        let paths = vec![s(&file)];

        let p = paths.clone();
        let (_, done) = run_queued(&queue, move || {
            checksum_create_job(p.clone(), Algorithm::Sha256, None)
        });
        assert_eq!(done.state, OperationState::Completed);
        assert_eq!(done.processed_bytes, data.len() as u64);
        assert_eq!(done.total_bytes, Some(data.len() as u64));
        assert!(root.join("j.bin.sha256").exists());

        // Same default job again: the sidecar exists → Failed with the reason.
        let p = paths.clone();
        let (failed_id, failed) = run_queued(&queue, move || {
            checksum_create_job(p.clone(), Algorithm::Sha256, None)
        });
        assert_eq!(failed.state, OperationState::Failed);
        let reason = failed.error.as_deref().unwrap();
        assert!(reason.contains("1 of 1"), "{failed:?}");
        // After removing the blocker, the BTM retry (factory) succeeds as a new op.
        fs::remove_file(root.join("j.bin.sha256")).unwrap();
        let retry_id = queue.retry(&failed_id).expect("retryable");
        assert_ne!(retry_id, failed_id);
        let (_, retried) = wait_terminal(&queue, &retry_id);
        assert_eq!(retried.state, OperationState::Completed);

        // A skip policy turns the same conflict into a clean completion.
        let p = paths.clone();
        let (_, skipped) = run_queued(&queue, move || {
            checksum_create_job(p.clone(), Algorithm::Sha256, Some(ConflictPolicy::Skip))
        });
        assert_eq!(skipped.state, OperationState::Completed);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn queued_split_and_combine_complete_with_byte_progress() {
        let root = temp("jobsplit");
        let src = root.join("q.bin");
        let data = payload(2 * CHUNK + 1_234);
        fs::write(&src, &data).unwrap();
        let queue = test_queue();

        let (path, dir) = (s(&src), s(&root));
        let (_, split) = run_queued(&queue, move || {
            split_job(path.clone(), (CHUNK + 100) as u64, dir.clone(), None)
        });
        assert_eq!(split.state, OperationState::Completed);
        assert_eq!(split.processed_bytes, data.len() as u64);
        assert_eq!(split.processed_files, 3);
        assert_eq!(split.total_files, Some(3));
        assert!(staging_leftovers(&root).is_empty());

        let (first, out) = (s(&root.join("q.bin.001")), s(&root.join("q.out")));
        let (f, o) = (first.clone(), out.clone());
        let (_, combined) = run_queued(&queue, move || combine_job(f.clone(), o.clone(), None));
        assert_eq!(combined.state, OperationState::Completed);
        assert_eq!(combined.processed_bytes, data.len() as u64);
        assert_eq!(fs::read(&out).unwrap(), data);

        // Existing output + default policy → Failed, nothing replaced.
        let (f, o) = (first.clone(), out.clone());
        let (_, refused) = run_queued(&queue, move || combine_job(f.clone(), o.clone(), None));
        assert_eq!(refused.state, OperationState::Failed);
        assert!(refused.error.as_deref().unwrap().contains("already exists"));

        // Rename policy keeps both outputs.
        let (f, o) = (first, out);
        let (_, renamed) = run_queued(&queue, move || {
            combine_job(f.clone(), o.clone(), Some(ConflictPolicy::RenameAuto))
        });
        assert_eq!(renamed.state, OperationState::Completed);
        assert_eq!(fs::read(root.join("q (2).out")).unwrap(), data);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn queued_jobs_exit_with_cancelled_when_the_token_is_tripped() {
        let root = temp("jobcancel");
        let src = root.join("k.bin");
        fs::write(&src, payload(50_000)).unwrap();
        let token = CancelToken::default();
        token.cancel();
        let noop = |_: OperationProgress| {};

        let split = split_job(s(&src), 10_000, s(&root), None);
        assert_eq!(split(&token, &noop).unwrap_err().code, ErrorCode::Cancelled);
        let create = checksum_create_job(vec![s(&src)], Algorithm::Sha256, None);
        assert_eq!(
            create(&token, &noop).unwrap_err().code,
            ErrorCode::Cancelled
        );
        assert_eq!(names(&root), vec!["k.bin".to_string()]);
        let _ = fs::remove_dir_all(&root);
    }
}
