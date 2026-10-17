//! Format-independent preflight and bounded decoded streaming.
//! Filesystem publication is fail-closed: path-based std::fs checks cannot
//! prevent a concurrent directory-to-symlink/junction swap. Do not expose this
//! as a working extractor until handle-relative, no-follow commit is implemented.

#[path = "archive_publish.rs"]
#[allow(dead_code)] // platform-specific helpers; each host uses a subset
pub mod publish;

use crate::archive_contract::{
    ArchiveConflictPolicy, ArchiveError, ArchiveErrorCode, ArchiveLimits,
};
use publish::{DirHandle, PublishError};
use std::{
    collections::HashMap,
    io::{self, Write},
    path::{Path, PathBuf},
};

const RATIO_WARNING: u64 = 100;
// Request limits can tighten these ceilings, never raise them.
const MAX_ENTRIES: u64 = 10_000;
const MAX_TOTAL_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_ENTRY_BYTES: u64 = 512 * 1024 * 1024;
const MAX_DEPTH: u32 = 32;
const MAX_NAME_BYTES: usize = 4 * 1024;
const MAX_NAME_METADATA_BYTES: usize = 16 * 1024 * 1024;

fn bounded_limits(requested: &ArchiveLimits) -> ArchiveLimits {
    ArchiveLimits {
        max_entries: requested.max_entries.min(MAX_ENTRIES),
        max_total_bytes: requested.max_total_bytes.min(MAX_TOTAL_BYTES),
        max_entry_bytes: requested.max_entry_bytes.min(MAX_ENTRY_BYTES),
        max_depth: requested.max_depth.min(MAX_DEPTH),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Directory,
    Symlink,
    Other,
}

/// Backend metadata; raw archive names are never trusted as OS paths.
#[derive(Debug, Clone)]
pub struct Entry<'a> {
    pub raw_name: &'a str,
    pub kind: EntryKind,
    pub expanded_size: Option<u64>,
    pub compressed_size: Option<u64>,
    /// Source modification time in unix ms, when the format provides it.
    pub modified_ms: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct PlannedEntry {
    pub raw_name: String,
    pub relative_path: PathBuf,
    pub kind: EntryKind,
    pub expanded_size: Option<u64>,
    pub compressed_size: Option<u64>,
    pub modified_ms: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct Plan {
    entries: Vec<PlannedEntry>,
    ratio_warnings: Vec<usize>,
}
impl Plan {
    pub fn entries(&self) -> &[PlannedEntry] {
        &self.entries
    }
    /// Indexes of entries whose declared expansion is at least 100:1.
    pub fn ratio_warnings(&self) -> &[usize] {
        &self.ratio_warnings
    }
    /// Sum of the declared expanded sizes of the file entries; `None` when any size is unknown.
    pub fn total_expanded_bytes(&self) -> Option<u64> {
        self.entries
            .iter()
            .filter(|e| e.kind != EntryKind::Directory)
            .try_fold(0u64, |acc, e| acc.checked_add(e.expanded_size?))
    }
}

/// FER-79: byte and member bookkeeping that the providers feed from their stream callbacks, so
/// the progress callback can report real byte counts and the member being written.
#[derive(Debug, Default)]
pub struct ProgressTrack {
    bytes: std::cell::Cell<u64>,
    member: std::cell::RefCell<Option<String>>,
}
impl ProgressTrack {
    pub fn begin_member(&self, raw_name: &str) {
        *self.member.borrow_mut() = Some(raw_name.to_owned());
    }
    pub fn bytes(&self) -> u64 {
        self.bytes.get()
    }
    pub fn member(&self) -> Option<String> {
        self.member.borrow().clone()
    }
    pub fn add(&self, n: u64) {
        self.bytes.set(self.bytes.get().saturating_add(n));
    }
}

/// `Write` adapter that counts the bytes passing through into a [`ProgressTrack`].
pub struct CountingWriter<'a, 'b> {
    inner: &'a mut dyn Write,
    track: &'b ProgressTrack,
}
impl<'a, 'b> CountingWriter<'a, 'b> {
    pub fn new(inner: &'a mut dyn Write, track: &'b ProgressTrack) -> Self {
        Self { inner, track }
    }
}
impl Write for CountingWriter<'_, '_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(bytes)?;
        self.track.add(n as u64);
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtractReport {
    pub written_files: u64,
    pub skipped_files: u64,
    pub expanded_bytes: u64,
}

fn error(code: ArchiveErrorCode, message: impl Into<String>) -> ArchiveError {
    ArchiveError::new(code, message)
}
pub fn io_error(err: io::Error) -> ArchiveError {
    error(ArchiveErrorCode::Io, err.to_string())
}
fn member_error(code: ArchiveErrorCode, name: &str, message: &str) -> ArchiveError {
    let mut err = error(code, message);
    err.member_path = Some(name.to_owned());
    err
}

fn reserved(part: &str) -> bool {
    let stem = part.split('.').next().unwrap_or("").to_uppercase();
    matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ["COM", "LPT"].iter().any(|prefix| {
        stem.strip_prefix(prefix).is_some_and(|suffix| {
            suffix.chars().count() == 1
                && suffix
                    .chars()
                    .all(|c| matches!(c, '1'..='9' | '¹' | '²' | '³'))
        })
    })
}

// Without Unicode normalization, reject decomposed marks and Hangul Jamo
// rather than accept names that can become equivalent on a target filesystem.
fn ambiguous_unicode(ch: char) -> bool {
    matches!(ch,
        '\u{0300}'..='\u{036f}' | '\u{0483}'..='\u{0489}' |
        '\u{0591}'..='\u{05bd}' | '\u{05bf}' | '\u{05c1}'..='\u{05c2}' |
        '\u{05c4}'..='\u{05c5}' | '\u{0610}'..='\u{061a}' |
        '\u{064b}'..='\u{065f}' | '\u{0670}' |
        '\u{1100}'..='\u{11ff}' | '\u{1ab0}'..='\u{1aff}' |
        '\u{1dc0}'..='\u{1dff}' | '\u{20d0}'..='\u{20ff}' |
        '\u{2126}' | '\u{212a}' | '\u{212b}' |
        '\u{a960}'..='\u{a97f}' | '\u{d7b0}'..='\u{d7ff}' |
        '\u{fe20}'..='\u{fe2f}'
    )
}

fn normalize(
    name: &str,
    kind: EntryKind,
    max_depth: u32,
) -> Result<(PathBuf, String), ArchiveError> {
    let bad = || {
        member_error(
            ArchiveErrorCode::UnsafeEntry,
            name,
            "Unsafe archive member name",
        )
    };
    let normalized = name.replace('\\', "/");
    // One conventional trailing separator belongs to directories only.
    let normalized = if kind == EntryKind::Directory {
        normalized.strip_suffix('/').unwrap_or(&normalized)
    } else {
        &normalized
    };
    if normalized.is_empty()
        || normalized.starts_with('/')
        || normalized.ends_with('/')
        || name.contains('\0')
        || name.chars().any(ambiguous_unicode)
    {
        return Err(bad());
    }
    let parts: Vec<_> = normalized.split('/').collect();
    if parts.len() > max_depth as usize {
        return Err(member_error(
            ArchiveErrorCode::LimitExceeded,
            name,
            "Archive depth limit exceeded",
        ));
    }
    let mut path = PathBuf::new();
    for part in &parts {
        if part.is_empty()
            || *part == "."
            || *part == ".."
            || part.ends_with(['.', ' '])
            || part
                .chars()
                .any(|ch| ch.is_control() || ":<>\"|?*".contains(ch))
            || reserved(part)
        {
            return Err(bad());
        }
        path.push(part);
    }
    Ok((path, parts.join("/").to_lowercase()))
}

/// Validate entire listing, including implicit parent directories, before streaming.
pub fn preflight(entries: &[Entry<'_>], limits: &ArchiveLimits) -> Result<Plan, ArchiveError> {
    let limits = bounded_limits(limits);
    if entries.len() as u64 > limits.max_entries {
        return Err(error(
            ArchiveErrorCode::LimitExceeded,
            "Archive entry count limit exceeded",
        ));
    }
    let mut seen = HashMap::new();
    let mut planned = Vec::with_capacity(entries.len());
    let mut ratio_warnings = Vec::new();
    let mut total = 0u64;
    let mut name_bytes = 0usize;
    for entry in entries {
        name_bytes = name_bytes
            .checked_add(entry.raw_name.len())
            .ok_or_else(|| {
                error(
                    ArchiveErrorCode::LimitExceeded,
                    "Archive name metadata limit exceeded",
                )
            })?;
        if entry.raw_name.len() > MAX_NAME_BYTES || name_bytes > MAX_NAME_METADATA_BYTES {
            return Err(member_error(
                ArchiveErrorCode::LimitExceeded,
                entry.raw_name,
                "Archive name metadata limit exceeded",
            ));
        }
        if !matches!(entry.kind, EntryKind::File | EntryKind::Directory) {
            return Err(member_error(
                ArchiveErrorCode::UnsafeEntry,
                entry.raw_name,
                "Archive links and special entries are unsupported",
            ));
        }
        let (path, key) = normalize(entry.raw_name, entry.kind, limits.max_depth)?;
        if seen.insert(key, entry.kind).is_some() {
            return Err(member_error(
                ArchiveErrorCode::Conflict,
                entry.raw_name,
                "Duplicate archive member",
            ));
        }
        if entry.kind == EntryKind::File {
            let size = entry.expanded_size.unwrap_or(0);
            if size > limits.max_entry_bytes {
                return Err(member_error(
                    ArchiveErrorCode::LimitExceeded,
                    entry.raw_name,
                    "Archive member size limit exceeded",
                ));
            }
            total = total.checked_add(size).ok_or_else(|| {
                error(
                    ArchiveErrorCode::LimitExceeded,
                    "Archive total size overflow",
                )
            })?;
            if total > limits.max_total_bytes {
                return Err(error(
                    ArchiveErrorCode::LimitExceeded,
                    "Archive total size limit exceeded",
                ));
            }
            if let Some(compressed) = entry.compressed_size
                && ((compressed == 0 && size > 0)
                    || (compressed > 0 && size / compressed >= RATIO_WARNING))
            {
                ratio_warnings.push(planned.len());
            }
        } else if entry.expanded_size.is_some_and(|size| size != 0) {
            return Err(member_error(
                ArchiveErrorCode::UnsafeEntry,
                entry.raw_name,
                "Directory declares expanded bytes",
            ));
        }
        planned.push(PlannedEntry {
            raw_name: entry.raw_name.to_owned(),
            relative_path: path,
            kind: entry.kind,
            expanded_size: entry.expanded_size,
            compressed_size: entry.compressed_size,
            modified_ms: entry.modified_ms,
        });
    }
    let keys: HashMap<_, _> = planned
        .iter()
        .map(|entry| {
            (
                entry
                    .relative_path
                    .to_string_lossy()
                    .replace('\\', "/")
                    .to_lowercase(),
                entry.kind,
            )
        })
        .collect();
    for entry in &planned {
        let mut parent = entry.relative_path.parent();
        while let Some(value) = parent {
            if value.as_os_str().is_empty() {
                break;
            }
            if keys.get(&value.to_string_lossy().replace('\\', "/").to_lowercase())
                == Some(&EntryKind::File)
            {
                return Err(member_error(
                    ArchiveErrorCode::Conflict,
                    &entry.raw_name,
                    "File is ancestor of another member",
                ));
            }
            parent = value.parent();
        }
    }
    Ok(Plan {
        entries: planned,
        ratio_warnings,
    })
}

struct QuotaWriter<'a, W: Write, C> {
    output: &'a mut W,
    entry_bytes: u64,
    total_bytes: &'a mut u64,
    limits: &'a ArchiveLimits,
    cancelled: &'a mut C,
    failure: Option<ArchiveErrorCode>,
}
impl<W: Write, C: FnMut() -> bool> Write for QuotaWriter<'_, W, C> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.failure.is_some() {
            return Err(io::Error::other("Earlier extraction write failed"));
        }
        if (self.cancelled)() {
            self.failure = Some(ArchiveErrorCode::Cancelled);
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Extraction cancelled",
            ));
        }
        let length = bytes.len() as u64;
        if self
            .entry_bytes
            .checked_add(length)
            .is_none_or(|n| n > self.limits.max_entry_bytes)
            || self
                .total_bytes
                .checked_add(length)
                .is_none_or(|n| n > self.limits.max_total_bytes)
        {
            self.failure = Some(ArchiveErrorCode::LimitExceeded);
            return Err(io::Error::other("Expanded bytes limit exceeded"));
        }
        let written = self.output.write(bytes).inspect_err(|_| {
            self.failure = Some(ArchiveErrorCode::Io);
        })?;
        self.entry_bytes += written as u64;
        *self.total_bytes += written as u64;
        Ok(written)
    }
    fn flush(&mut self) -> io::Result<()> {
        if self.failure.is_some() {
            return Err(io::Error::other("Earlier extraction write failed"));
        }
        self.output.flush().inspect_err(|_| {
            self.failure = Some(ArchiveErrorCode::Io);
        })
    }
}

/// Apply runtime quota to decoded bytes sent to caller-owned sink. On error sink
/// can hold partial bytes; callers must never expose it as completed extraction.
pub fn stream_checked<W, F, C>(
    plan: &Plan,
    limits: &ArchiveLimits,
    output: &mut W,
    mut stream: F,
    mut cancelled: C,
) -> Result<u64, ArchiveError>
where
    W: Write,
    F: FnMut(&PlannedEntry, &mut dyn Write) -> Result<(), ArchiveError>,
    C: FnMut() -> bool,
{
    let metadata: Vec<_> = plan
        .entries
        .iter()
        .map(|entry| Entry {
            raw_name: &entry.raw_name,
            kind: entry.kind,
            expanded_size: entry.expanded_size,
            compressed_size: entry.compressed_size,
            modified_ms: entry.modified_ms,
        })
        .collect();
    let checked = preflight(&metadata, limits)?;
    if checked
        .entries
        .iter()
        .zip(&plan.entries)
        .any(|(a, b)| a.relative_path != b.relative_path)
    {
        return Err(error(
            ArchiveErrorCode::UnsafeEntry,
            "Plan path does not match raw member",
        ));
    }
    let limits = bounded_limits(limits);
    let mut total = 0;
    for entry in &plan.entries {
        if cancelled() {
            return Err(error(ArchiveErrorCode::Cancelled, "Extraction cancelled"));
        }
        if entry.kind == EntryKind::Directory {
            continue;
        }
        let mut writer = QuotaWriter {
            output,
            entry_bytes: 0,
            total_bytes: &mut total,
            limits: &limits,
            cancelled: &mut cancelled,
            failure: None,
        };
        let decoded = stream(entry, &mut writer);
        if let Some(code) = writer.failure {
            return Err(member_error(
                code,
                &entry.raw_name,
                "Extraction write failed",
            ));
        }
        decoded?;
        writer.flush().map_err(io_error)?;
        if entry
            .expanded_size
            .is_some_and(|size| size != writer.entry_bytes)
        {
            return Err(member_error(
                ArchiveErrorCode::UnsafeEntry,
                &entry.raw_name,
                "Expanded size does not match archive metadata",
            ));
        }
    }
    if cancelled() {
        return Err(error(ArchiveErrorCode::Cancelled, "Extraction cancelled"));
    }
    Ok(total)
}

/// Publish extracted entries under `destination` using handle-relative,
/// no-follow filesystem operations (see [`publish`]).
///
/// Failure semantics: the entry being written when an error/cancel occurs is
/// removed through its own handle, so no partial file that *looks* complete
/// survives. Directories created by this run are removed (empty, innermost
/// first) on failure. Entries already fully written stay — they are complete.
/// Extra vetoes over where an extraction writes (FER-559: the Windows Startup folder), installed
/// once by the application (`install_extract_guard` in `lib.rs`). `Err(message)` refuses the job.
/// - `entries`: lexical fail-fast over destination + planned relative paths, before anything is
///   written;
/// - `final_dir`: the FINAL path the OS reports for every folder the job opens or creates
///   (`GetFinalPathNameByHandleW`), which catches 8.3 short names and any other alias the lexical
///   join cannot see. A folder created before the veto is removed by the normal rollback.
pub struct ExtractGuard {
    pub entries: fn(&Path, &[&Path]) -> Result<(), String>,
    pub final_dir: fn(&Path) -> Result<(), String>,
}
static GUARD: std::sync::OnceLock<ExtractGuard> = std::sync::OnceLock::new();

pub fn set_entry_guard(guard: ExtractGuard) {
    let _ = GUARD.set(guard);
}

/// This file is also compiled on its own by the archive test crates, which have no such policy.
/// In the application crate (`ferrum_lib`, outside its unit tests, which extract into temp folders
/// without installing it) a missing guard means "refuse": an extraction can never run unguarded because installation was forgotten.
fn app_build() -> bool {
    option_env!("CARGO_CRATE_NAME") == Some("ferrum_lib")
}

fn active_guard() -> Result<Option<&'static ExtractGuard>, ArchiveError> {
    match GUARD.get() {
        Some(g) => Ok(Some(g)),
        None if app_build() && !cfg!(test) => Err(error(
            ArchiveErrorCode::UnsafeEntry,
            "Extraction is refused: the destination guard is not installed",
        )),
        None => Ok(None),
    }
}

/// Veto on the folder a handle really points at.
fn check_final_dir(guard: Option<&ExtractGuard>, dir: &DirHandle) -> Result<(), ArchiveError> {
    let Some(g) = guard else { return Ok(()) };
    match publish::final_path(dir) {
        Some(p) => {
            (g.final_dir)(Path::new(&p)).map_err(|m| error(ArchiveErrorCode::UnsafeEntry, m))
        }
        #[cfg(windows)]
        None => Err(error(
            ArchiveErrorCode::UnsafeEntry,
            "Extraction is refused: the destination folder cannot be verified",
        )),
        #[cfg(not(windows))]
        None => Ok(()),
    }
}

pub fn extract<F, C>(
    plan: &Plan,
    destination: &Path,
    limits: &ArchiveLimits,
    policy: ArchiveConflictPolicy,
    mut stream: F,
    mut cancelled: C,
) -> Result<ExtractReport, ArchiveError>
where
    F: FnMut(&PlannedEntry, &mut dyn Write) -> Result<(), ArchiveError>,
    C: FnMut() -> bool,
{
    use std::collections::HashMap as StdMap;

    let limits = bounded_limits(limits);
    // Re-validate the whole listing against trusted ceilings before touching
    // the destination.
    let metadata: Vec<_> = plan
        .entries
        .iter()
        .map(|entry| Entry {
            raw_name: &entry.raw_name,
            kind: entry.kind,
            expanded_size: entry.expanded_size,
            compressed_size: entry.compressed_size,
            modified_ms: entry.modified_ms,
        })
        .collect();
    let checked = preflight(&metadata, &limits)?;
    if checked
        .entries
        .iter()
        .zip(&plan.entries)
        .any(|(a, b)| a.relative_path != b.relative_path)
    {
        return Err(error(
            ArchiveErrorCode::UnsafeEntry,
            "Plan path does not match raw member",
        ));
    }

    // FER-559: nothing is extracted into a Windows Startup folder, whether the destination is
    // Startup itself or an archive entry such as `Startup/a.bat` reaches it from above.
    let guard = active_guard()?;
    if let Some(g) = guard {
        let rels: Vec<&Path> = plan
            .entries
            .iter()
            .map(|e| e.relative_path.as_path())
            .collect();
        (g.entries)(destination, &rels).map_err(|m| error(ArchiveErrorCode::UnsafeEntry, m))?;
    }

    let mut report = ExtractReport::default();
    let mut dirs: StdMap<PathBuf, DirHandle> = StdMap::new();
    let mut created_dirs: Vec<PathBuf> = Vec::new(); // deepest insert order

    // The validated destination root anchors the relative namespace.
    let root = publish::open_root(destination).map_err(|e| {
        let mut err = match e {
            PublishError::Reparse => error(
                ArchiveErrorCode::UnsafeEntry,
                "Destination is a symlink/junction",
            ),
            other => error(ArchiveErrorCode::Io, format!("destination: {other:?}")),
        };
        err.archive_path = Some(destination.to_string_lossy().into_owned());
        err
    })?;
    check_final_dir(guard, &root)?;
    dirs.insert(PathBuf::new(), root);

    let ensure_dir = |dirs: &mut StdMap<PathBuf, DirHandle>,
                      created: &mut Vec<PathBuf>,
                      rel: &Path|
     -> Result<(), ArchiveError> {
        let mut acc = PathBuf::new();
        for part in rel.components() {
            acc.push(part);
            if dirs.contains_key(&acc) {
                continue;
            }
            let parent_rel = acc.parent().map(|p| p.to_path_buf()).unwrap_or_default();
            let parent = dirs
                .get(&parent_rel)
                .ok_or_else(|| error(ArchiveErrorCode::Io, "missing parent dir handle"))?;
            match publish::create_dir_new(parent, part.as_os_str()) {
                Ok(dir) => {
                    created.push(acc.clone());
                    let verdict = check_final_dir(guard, &dir);
                    dirs.insert(acc.clone(), dir);
                    verdict?;
                }
                Err(PublishError::Exists) => match publish::child_dir(parent, part.as_os_str()) {
                    Ok(dir) => {
                        let verdict = check_final_dir(guard, &dir);
                        dirs.insert(acc.clone(), dir);
                        verdict?;
                    }
                    Err(e) => return Err(publish_err(e, &acc)),
                },
                Err(e) => return Err(publish_err(e, &acc)),
            }
        }
        Ok(())
    };

    let result = (|| -> Result<(), ArchiveError> {
        'entries: for entry in &plan.entries {
            if cancelled() {
                return Err(error(ArchiveErrorCode::Cancelled, "Extraction cancelled"));
            }
            let rel = entry.relative_path.clone();
            if entry.kind == EntryKind::Directory {
                ensure_dir(&mut dirs, &mut created_dirs, &rel)?;
                continue;
            }
            let file_rel = rel.parent().map(|p| p.to_path_buf()).unwrap_or_default();
            ensure_dir(&mut dirs, &mut created_dirs, &file_rel)?;
            let dir = dirs
                .get(&file_rel)
                .ok_or_else(|| error(ArchiveErrorCode::Io, "missing dir handle"))?;

            // Pick the final name under the conflict policy. The exists check
            // is a non-destructive no-follow open; nothing is written yet.
            let mut attempt: u32 = 0;
            let final_name: std::ffi::OsString = loop {
                let candidate: std::ffi::OsString = if attempt == 0 {
                    file_name_of(&rel).to_os_string()
                } else {
                    renamed_candidate(&rel, attempt)
                };
                let exists =
                    publish::entry_exists(dir, &candidate).map_err(|e| publish_err(e, &rel))?;
                if !exists {
                    break candidate;
                }
                match policy {
                    ArchiveConflictPolicy::Skip => {
                        report.skipped_files += 1;
                        continue 'entries;
                    }
                    ArchiveConflictPolicy::Overwrite => break candidate,
                    ArchiveConflictPolicy::Rename => {
                        attempt += 1;
                        if attempt == 1 {
                            attempt = 2; // TC convention: original counts as (1)
                        }
                        if attempt > 1000 {
                            return Err(member_error(
                                ArchiveErrorCode::Conflict,
                                &entry.raw_name,
                                "No free rename slot for member",
                            ));
                        }
                    }
                    ArchiveConflictPolicy::OverwriteOlder => {
                        let dest_newer = publish::modified_ms(dir, &candidate)
                            .is_some_and(|dest| entry.modified_ms.is_some_and(|src| dest > src));
                        if dest_newer {
                            report.skipped_files += 1;
                            continue 'entries;
                        }
                        break candidate; // commit will replace it
                    }
                }
            };

            // Write to a private staging name (no-follow, no-clobber) so a
            // partial write can never be mistaken for a complete file.
            let staging = format!(
                "{name}.ferrum-partial-{pid}-{seq}",
                name = final_name.to_string_lossy(),
                pid = std::process::id(),
                seq = attempt as u64 + STAGING_SEQ.fetch_add(1, Ordering::SeqCst),
            );
            let mut file = match publish::create_file_new(dir, OsStr::new(&staging)) {
                Ok(f) => f,
                Err(PublishError::Exists) => {
                    return Err(member_error(
                        ArchiveErrorCode::Conflict,
                        &entry.raw_name,
                        "Staging name collision",
                    ));
                }
                Err(e) => return Err(publish_err(e, &rel)),
            };

            let quota_failure = {
                let mut total = 0u64;
                let mut writer = QuotaWriter {
                    output: &mut file,
                    entry_bytes: 0,
                    total_bytes: &mut total,
                    limits: &limits,
                    cancelled: &mut cancelled,
                    failure: None,
                };
                let decoded = stream(entry, &mut writer);
                let failure = writer.failure;
                if failure.is_none() {
                    match (decoded, writer.flush()) {
                        (Err(err), _) => Some(err.code),
                        (_, Err(_)) => Some(ArchiveErrorCode::Io),
                        (Ok(()), Ok(())) => None,
                    }
                } else {
                    failure
                }
            };
            if let Some(code) = quota_failure {
                drop(file);
                remove_staging(destination, &file_rel, &staging);
                return Err(member_error(
                    code,
                    &entry.raw_name,
                    "Expanded byte limit, cancellation, or write failure",
                ));
            }
            if cancelled() {
                drop(file);
                remove_staging(destination, &file_rel, &staging);
                return Err(error(ArchiveErrorCode::Cancelled, "Extraction cancelled"));
            }

            // Commit: close the staging handle (Windows rename fails with
            // ERROR_SHARING_VIOLATION while our own handle is open), then
            // rename staging onto the final name inside the same directory.
            // A symlink at the final name is refused (never overwritten
            // through). The rename is the last step, so a partial write is
            // always under the clearly-named staging file, never under the
            // final name.
            drop(file);
            let final_full = dir_full_path(destination, &file_rel).join(&final_name);
            if let Ok(meta) = std::fs::symlink_metadata(&final_full)
                && meta.file_type().is_symlink()
            {
                remove_staging(destination, &file_rel, &staging);
                return Err(member_error(
                    ArchiveErrorCode::UnsafeEntry,
                    &entry.raw_name,
                    "Final name is a symlink at commit time",
                ));
            }
            let staging_full = dir_full_path(destination, &file_rel).join(&staging);
            if let Err(rename_err) = std::fs::rename(&staging_full, &final_full) {
                let detail = format!(
                    "Commit rename failed ({} <- {}): {rename_err}; original preserved",
                    final_full.display(),
                    staging_full.display()
                );
                remove_staging(destination, &file_rel, &staging);
                return Err(member_error(
                    ArchiveErrorCode::Conflict,
                    &entry.raw_name,
                    &detail,
                ));
            }
            report.written_files += 1;
        }
        Ok(())
    })();

    if let Err(err) = result {
        // Rollback directories this run created, innermost first; POSIX
        // disposition only removes empty directories.
        for rel in created_dirs.iter().rev() {
            // Our own open handle on the directory (no delete sharing) must be closed before
            // the directory can be opened for DELETE again; its children are already gone.
            drop(dirs.remove(rel));
            if let Some(parent_rel) = rel.parent().map(|p| p.to_path_buf())
                && let (Some(parent), Some(name)) = (
                    dirs.get(&parent_rel),
                    rel.file_name().map(|n| n.to_os_string()),
                )
                && let Err(e) = publish::unlink_entry(parent, &name, true)
            {
                log::warn!("extraction rollback left {rel:?} in place: {e:?}");
            }
        }
        return Err(err);
    }
    Ok(report)
}

use std::ffi::OsStr;
use std::sync::atomic::{AtomicU64, Ordering};

static STAGING_SEQ: AtomicU64 = AtomicU64::new(0);

/// Remove a staging file we created. The staging name is minted by this
/// module (contains `.ferrum-partial-`), the symlink check refuses to follow
/// a swapped link, and removal failing is non-fatal: the leftover name is
/// unmistakably a partial, never the final name.
fn remove_staging(destination: &Path, rel_dir: &Path, staging: &str) {
    let full = dir_full_path(destination, rel_dir).join(staging);
    if let Ok(meta) = std::fs::symlink_metadata(&full)
        && !meta.file_type().is_symlink()
    {
        let _ = std::fs::remove_file(&full);
    }
}

/// Full on-disk path of `rel` under the destination root. Used ONLY for the
/// final commit rename of a staging file we just wrote; the root itself was
/// validated (non-reparse) at open time. Documented residual race: an
/// attacker with write access to the destination can swap the root in the
/// last window — recorded in docs/archive-support.md.
fn dir_full_path(destination: &Path, rel: &Path) -> PathBuf {
    let mut p = destination.to_path_buf();
    for part in rel.components() {
        p.push(part);
    }
    p
}

fn file_name_of(rel: &Path) -> &OsStr {
    rel.file_name().unwrap_or_default()
}

/// "name.ext" + attempt 2 -> "name (2).ext"; extensionless -> "name (2)".
fn renamed_candidate(rel: &Path, attempt: u32) -> std::ffi::OsString {
    let name = rel
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    let stem = Path::new(&name).file_stem().map(|s| s.to_os_string());
    let ext = Path::new(&name).extension().map(|s| s.to_os_string());
    match (stem, ext) {
        (Some(stem), Some(ext)) => {
            let mut s = stem;
            s.push(format!(" ({attempt})."));
            s.push(ext);
            s
        }
        (Some(stem), None) => {
            let mut s = stem;
            s.push(format!(" ({attempt})"));
            s
        }
        _ => {
            let mut s = name;
            s.push(format!(" ({attempt})"));
            s
        }
    }
}

fn publish_err(e: PublishError, rel: &Path) -> ArchiveError {
    let code = match e {
        PublishError::NotFound => ArchiveErrorCode::Io,
        PublishError::Exists => ArchiveErrorCode::Conflict,
        PublishError::Reparse => ArchiveErrorCode::UnsafeEntry,
        PublishError::WrongKind => ArchiveErrorCode::Conflict,
        PublishError::Io(m) => {
            let mut err = error(ArchiveErrorCode::Io, m);
            err.member_path = Some(rel.to_string_lossy().into_owned());
            return err;
        }
    };
    let mut err = error(code, "Publication refused");
    err.member_path = Some(rel.to_string_lossy().into_owned());
    err
}

#[cfg(all(test, windows))]
mod rollback_tests {
    use super::*;

    fn plan_for(names: &[(&str, EntryKind)]) -> Plan {
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
        let limits = ArchiveLimits {
            max_entries: 100,
            max_total_bytes: 1 << 20,
            max_entry_bytes: 1 << 20,
            max_depth: 8,
        };
        preflight(&entries, &limits).unwrap()
    }

    fn failing_extract(plan: &Plan, dest: &Path) -> Result<ExtractReport, ArchiveError> {
        let limits = ArchiveLimits {
            max_entries: 100,
            max_total_bytes: 1 << 20,
            max_entry_bytes: 1 << 20,
            max_depth: 8,
        };
        extract(
            plan,
            dest,
            &limits,
            ArchiveConflictPolicy::Skip,
            |_, _| Err(error(ArchiveErrorCode::Io, "boom")),
            || false,
        )
    }

    #[test]
    fn failed_extraction_removes_the_directories_and_staging_it_created() {
        let tmp = tempfile::tempdir().unwrap();
        let plan = plan_for(&[
            ("a/", EntryKind::Directory),
            ("a/b/", EntryKind::Directory),
            ("a/b/f.txt", EntryKind::File),
        ]);
        assert!(failing_extract(&plan, tmp.path()).is_err());
        let left: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert!(left.is_empty(), "rollback left behind: {left:?}");
    }

    #[test]
    fn failed_extraction_keeps_preexisting_directories_and_their_files() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("a")).unwrap();
        std::fs::write(tmp.path().join("a").join("old.txt"), b"old").unwrap();
        let plan = plan_for(&[("a/b/f.txt", EntryKind::File)]);
        assert!(failing_extract(&plan, tmp.path()).is_err());
        assert_eq!(
            std::fs::read(tmp.path().join("a").join("old.txt")).unwrap(),
            b"old"
        );
        assert!(!tmp.path().join("a").join("b").exists());
    }
}
