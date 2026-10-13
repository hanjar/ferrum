//! WinRAR (Rar.exe) discovery, validation, probing, and execution (FER-505).
//!
//! Ferrum does not distribute or require WinRAR. Users who want to create RAR archives
//! install WinRAR themselves and specify their `Rar.exe` path in Settings. Reading RAR archives
//! remains handled by the safe internal `unrar` crate.
//!
//! Security boundary:
//! - The executable path comes **only** from the prefs file on disk (never from IPC, env or AI).
//! - Absolute local drive paths only (reject UNC `\\...`, device `\\?\`, relative, `..`).
//! - Exact filename `Rar.exe` (case-insensitive).
//! - Regular file only (reject symlinks and junctions); re-checked right before every spawn.
//! - Clean execution: `env_remove("RAR")`, `CREATE_NO_WINDOW`, no shell.
//! - Fixed whitelist arguments with `-cfg-`, `-r-` and `--`.
//! - Rar only ever sees the explicit entries produced by `collect_sources` (the same pipeline as
//!   ZIP/7z): symlinks/junctions, staging files and the destination itself never reach it, and
//!   with `-r-` Rar cannot recurse on its own.

use crate::archive_contract::{
    ArchiveCompressionLevel, ArchiveConflictPolicy, ArchiveError, ArchiveErrorCode, ArchiveLimits,
    ArchiveProgress,
};
use crate::archive_create_test::{
    CreateTrack, SourceEntry, collect_sources, precheck_conflict, publish_created,
};
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::fs::OpenOptions;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

/// A `Rar.exe` that passed [`validate_rar_exe`]. Only that function can construct it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RarTool {
    path: PathBuf,
    version: String,
}

impl RarTool {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn version(&self) -> &str {
        &self.version
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RarToolProbeResult {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug)]
pub enum RarValidationError {
    EmptyPath,
    NotLocalDrive,
    TraversalComponent,
    InvalidFilename(String),
    NotFound(String),
    SymlinkRefused(String),
    NotRegularFile(String),
    Io(std::io::Error),
    ProbeFailed(String),
}

impl std::fmt::Display for RarValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyPath => write!(f, "Path must not be empty"),
            Self::NotLocalDrive => write!(
                f,
                "Path must be an absolute local drive path (e.g. C:\\...); UNC, device paths and relative paths are rejected"
            ),
            Self::TraversalComponent => {
                write!(f, "Path contains forbidden '..' traversal components")
            }
            Self::InvalidFilename(name) => {
                write!(
                    f,
                    "Executable name must be exactly 'Rar.exe' (got '{name}')"
                )
            }
            Self::NotFound(path) => write!(f, "File not found: {path}"),
            Self::SymlinkRefused(path) => {
                write!(
                    f,
                    "Path is a symlink or junction, which is refused for security: {path}"
                )
            }
            Self::NotRegularFile(path) => {
                write!(f, "Path is a directory, not a regular file: {path}")
            }
            Self::Io(err) => write!(f, "I/O error inspecting file: {err}"),
            Self::ProbeFailed(err) => write!(f, "Probe failed: {err}"),
        }
    }
}

impl std::error::Error for RarValidationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<std::io::Error> for RarValidationError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

/// Checks whether `path` is an absolute local drive path (e.g. `C:\...` or `D:/...`).
/// Specifically rejects UNC (`\\server\share`), device paths (`\\?\...`, `\\.\...`),
/// relative paths, and paths containing `..`.
pub fn is_local_drive_path(path: &Path) -> bool {
    let path_str = path.to_string_lossy();
    if path_str.trim().is_empty() {
        return false;
    }
    // Reject UNC and device prefixes
    if path_str.starts_with(r"\\") || path_str.starts_with("//") {
        return false;
    }
    // Must start with drive letter e.g. C:\ or C:/
    let bytes = path_str.as_bytes();
    if bytes.len() < 3
        || !bytes[0].is_ascii_alphabetic()
        || bytes[1] != b':'
        || (bytes[2] != b'\\' && bytes[2] != b'/')
    {
        return false;
    }
    // Check components: no parent dirs (..), no device prefixes
    for comp in path.components() {
        match comp {
            Component::Prefix(prefix) => {
                #[cfg(windows)]
                {
                    use std::path::Prefix;
                    match prefix.kind() {
                        Prefix::Disk(_) => {}
                        _ => return false,
                    }
                }
                #[cfg(not(windows))]
                let _ = prefix;
            }
            Component::ParentDir => return false,
            _ => {}
        }
    }
    true
}

/// Static (no-spawn) checks of section 3.1: path shape, exact `Rar.exe` name, regular file that
/// is not a symlink/junction. Also run right before every spawn to narrow the TOCTOU window.
fn check_rar_exe_file(path: &Path) -> Result<(), RarValidationError> {
    let path_str = path.to_string_lossy();
    if path_str.trim().is_empty() {
        return Err(RarValidationError::EmptyPath);
    }
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(RarValidationError::TraversalComponent);
    }
    if !is_local_drive_path(path) {
        return Err(RarValidationError::NotLocalDrive);
    }

    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| RarValidationError::InvalidFilename(path_str.into_owned()))?;

    if !file_name.eq_ignore_ascii_case("rar.exe") {
        return Err(RarValidationError::InvalidFilename(file_name.to_owned()));
    }

    let meta = std::fs::symlink_metadata(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            RarValidationError::NotFound(path.display().to_string())
        } else {
            RarValidationError::Io(e)
        }
    })?;

    // Rust reports every name-surrogate reparse point (symlink *and* junction) as a symlink.
    if meta.file_type().is_symlink() {
        return Err(RarValidationError::SymlinkRefused(
            path.display().to_string(),
        ));
    }
    if !meta.is_file() {
        return Err(RarValidationError::NotRegularFile(
            path.display().to_string(),
        ));
    }
    Ok(())
}

/// Validate `Rar.exe` path according to section 3.1:
/// - Absolute local path with drive letter (reject UNC, device, relative, `..`).
/// - Filename is exactly `Rar.exe` (case-insensitive).
/// - File exists and is a regular file, not a symlink or junction.
/// - Probe: runs `Rar.exe` without arguments (timeout 5s), parses version line `RAR x.yy`.
pub fn validate_rar_exe(path: &Path) -> Result<RarTool, RarValidationError> {
    check_rar_exe_file(path)?;
    let version = probe_rar_version(path)?;
    Ok(RarTool {
        path: path.to_path_buf(),
        version,
    })
}

/// Probe `Rar.exe` by running it without arguments, timing out after 5 seconds,
/// and parsing the version line `RAR x.yy` from stdout.
fn probe_rar_version(rar_path: &Path) -> Result<String, RarValidationError> {
    let mut cmd = std::process::Command::new(rar_path);
    cmd.env_remove("RAR");
    cmd.current_dir(std::env::temp_dir());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| RarValidationError::ProbeFailed(format!("Failed to spawn Rar.exe: {e}")))?;

    let mut stdout_pipe = child.stdout.take();
    let stdout_reader = std::thread::spawn(move || {
        use std::io::Read;
        let mut text = String::new();
        if let Some(mut pipe) = stdout_pipe.take() {
            let _ = pipe.read_to_string(&mut text);
        }
        text
    });

    let mut stderr_pipe = child.stderr.take();
    let stderr_reader = std::thread::spawn(move || {
        use std::io::Read;
        let mut text = String::new();
        if let Some(mut pipe) = stderr_pipe.take() {
            let _ = pipe.read_to_string(&mut text);
        }
        text
    });

    let start = Instant::now();
    let timeout = Duration::from_secs(5);
    let mut exited = false;

    while start.elapsed() < timeout {
        match child.try_wait() {
            Ok(Some(_)) => {
                exited = true;
                break;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(30)),
            Err(e) => {
                return Err(RarValidationError::ProbeFailed(format!(
                    "Process wait error: {e}"
                )));
            }
        }
    }

    if !exited {
        let _ = child.kill();
        let _ = child.wait();
        return Err(RarValidationError::ProbeFailed(
            "Rar.exe probe timed out after 5 seconds".to_string(),
        ));
    }

    let stdout_text = stdout_reader.join().unwrap_or_default();
    let _ = stderr_reader.join();

    parse_rar_version_output(&stdout_text).ok_or_else(|| {
        RarValidationError::ProbeFailed(
            "Rar.exe did not output a valid 'RAR x.yy' version line".to_string(),
        )
    })
}

/// Parses the RAR version line from stdout text.
/// Matches lines containing "RAR x.yy" and returns a clean version descriptor (e.g. "RAR 7.00 x64").
pub fn parse_rar_version_output(stdout: &str) -> Option<String> {
    for line in stdout.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("RAR ") {
            // Check version number (first token)
            let first_token = rest.split_whitespace().next().unwrap_or("");
            let is_version = first_token
                .split_once('.')
                .map(|(major, minor)| {
                    !major.is_empty()
                        && major.chars().all(|c| c.is_ascii_digit())
                        && !minor.is_empty()
                        && minor.chars().all(|c| c.is_ascii_digit())
                })
                .unwrap_or(false);
            if is_version {
                // Clean up: stop before "Copyright" if present
                if let Some(idx) = trimmed.find("Copyright") {
                    let clean = trimmed[..idx].trim();
                    if !clean.is_empty() {
                        return Some(clean.to_string());
                    }
                }
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

/// Only searches candidate paths in Program Files:
/// 1. `C:\Program Files\WinRAR\Rar.exe`
/// 2. `C:\Program Files (x86)\WinRAR\Rar.exe`
///
/// Returns the first path found without saving it.
pub fn detect_rar_exe() -> Option<PathBuf> {
    let p1 = PathBuf::from(r"C:\Program Files\WinRAR\Rar.exe");
    if p1.is_file() {
        return Some(p1);
    }
    let p2 = PathBuf::from(r"C:\Program Files (x86)\WinRAR\Rar.exe");
    if p2.is_file() {
        return Some(p2);
    }
    None
}

/// Fixed whitelist arguments builder for Rar.exe (FER-505 Section 3.3, review fixes):
/// ```text
/// a -cfg- -idc -ma5 -m<level> -r- -y -o- -scul -- <job>\archive.rar @<job>\list-<n>.txt
/// ```
/// - `-r-` forbids any recursion: Rar archives exactly the entries written to the listfile
///   (files, plus empty directories as bare directory entries), nothing it finds by itself.
/// - No `-ep*`: listfile names are relative to the working directory (the base folder of the
///   selection), so Rar stores them exactly as ZIP names them (`src\sub\x.txt`).
/// - `-scul`: the listfile is read as **UTF-16** (little endian), which is how it is written.
pub fn build_rar_args(
    level: ArchiveCompressionLevel,
    archive_path: &Path,
    listfile_path: &Path,
) -> Vec<OsString> {
    let m_switch = match level {
        ArchiveCompressionLevel::Store => "-m0",
        ArchiveCompressionLevel::Fast => "-m1",
        ArchiveCompressionLevel::Normal => "-m3",
        ArchiveCompressionLevel::Max => "-m5",
    };
    let mut list_arg = OsString::from("@");
    list_arg.push(listfile_path.as_os_str());
    let mut args: Vec<OsString> = [
        "a", "-cfg-", "-idc", "-ma5", m_switch, "-r-", "-y", "-o-", "-scul", "--",
    ]
    .iter()
    .map(OsString::from)
    .collect();
    args.push(archive_path.as_os_str().to_owned());
    args.push(list_arg);
    args
}

/// Exclusive per-job working folder next to the destination:
/// `.<dest name>.<pid>-<random>.ferrum-tmp` (a staging name, so concurrent creates skip it).
/// Created with `create_dir` (fails if anything already exists there), so the folder and all it
/// contains are ours. Rar writes `archive.rar` into it and the listfiles live in it; the finished
/// archive is renamed out of it by `publish_created` (same volume, atomic). `Drop` removes the
/// folder on every path (success, error, cancel, panic).
struct JobDir(PathBuf);

impl JobDir {
    fn create(dest: &Path) -> Result<Self, ArchiveError> {
        let parent = match dest.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => {
                return Err(ArchiveError::new(
                    ArchiveErrorCode::Io,
                    "Destination has no parent folder",
                ));
            }
        };
        let name = dest
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "archive".to_owned());
        for _ in 0..16 {
            let mut b = [0u8; 8];
            getrandom::fill(&mut b).map_err(|_| {
                ArchiveError::new(ArchiveErrorCode::Io, "OS random source unavailable")
            })?;
            let candidate = parent.join(format!(
                ".{name}.{}-{}.ferrum-tmp",
                std::process::id(),
                u64::from_le_bytes(b)
            ));
            match std::fs::create_dir(&candidate) {
                Ok(()) => return Ok(JobDir(candidate)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(io_error(e, &candidate)),
            }
        }
        Err(ArchiveError::new(
            ArchiveErrorCode::Io,
            "Could not allocate a job folder",
        ))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for JobDir {
    fn drop(&mut self) {
        // std's remove_dir_all does not follow symlinks/junctions inside the folder.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn io_error(e: std::io::Error, path: &Path) -> ArchiveError {
    let mut err = ArchiveError::new(ArchiveErrorCode::Io, e.to_string());
    err.archive_path = Some(path.to_string_lossy().into_owned());
    err
}

fn cancelled() -> ArchiveError {
    ArchiveError::new(ArchiveErrorCode::Cancelled, "archive create cancelled")
}

/// Entries of one base folder: Rar runs with that folder as working directory and the entries'
/// archive names (relative to it) in the listfile.
struct RarGroup<'a> {
    base: PathBuf,
    entries: Vec<&'a SourceEntry>,
}

/// Group `collect_sources` entries by base folder (disk path minus its archive name), keeping
/// order. A selection from one folder (the normal case) is one group; sources from different
/// folders become one Rar run each, all appending to the same job archive.
fn group_by_base(files: &[SourceEntry]) -> Result<Vec<RarGroup<'_>>, ArchiveError> {
    let mut groups: Vec<RarGroup<'_>> = Vec::new();
    for entry in files {
        let depth = entry.archive_name.split('/').count();
        let base = entry
            .disk_path
            .ancestors()
            .nth(depth)
            .filter(|b| !b.as_os_str().is_empty())
            .ok_or_else(|| {
                ArchiveError::new(
                    ArchiveErrorCode::InvalidMemberPath,
                    "Source has no parent folder",
                )
            })?
            .to_path_buf();
        match groups.iter_mut().find(|g| g.base == base) {
            Some(g) => g.entries.push(entry),
            None => groups.push(RarGroup {
                base,
                entries: vec![entry],
            }),
        }
    }
    Ok(groups)
}

/// Write the listfile for one group: one archive name per line (`\` separators, CRLF), UTF-16LE
/// as `-scul` expects. Created exclusively inside the job folder.
fn write_listfile(path: &Path, entries: &[&SourceEntry]) -> Result<(), ArchiveError> {
    use std::io::Write;
    let mut text = String::new();
    for e in entries {
        let name = &e.archive_name;
        if name.is_empty() || name.contains(['\r', '\n', '\0', '*', '?']) {
            return Err(ArchiveError::new(
                ArchiveErrorCode::InvalidMemberPath,
                format!("Unsupported file name for RAR listfile: {name:?}"),
            ));
        }
        text.push_str(&name.replace('/', "\\"));
        text.push_str("\r\n");
    }
    let bytes: Vec<u8> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| io_error(e, path))?;
    f.write_all(&bytes).map_err(|e| io_error(e, path))?;
    f.sync_all().map_err(|e| io_error(e, path))
}

/// Reads the configured `Rar.exe` path from the prefs file in `config_dir`.
fn configured_rar_path(config_dir: &Path) -> Result<PathBuf, ArchiveError> {
    match crate::prefs::load_from(config_dir).rar_exe_path {
        Some(p) if !p.trim().is_empty() => Ok(PathBuf::from(p)),
        _ => Err(ArchiveError::new(
            ArchiveErrorCode::UnsupportedFormat,
            "WinRAR (Rar.exe) is not configured in Settings",
        )),
    }
}

/// RAR archive creation driven by the user's Rar.exe (FER-505), production entry point.
/// The executable path is read from the prefs file on disk only.
pub fn create_rar_progress_with_level(
    sources: &[String],
    destination_path: &Path,
    policy: ArchiveConflictPolicy,
    level: ArchiveCompressionLevel,
    progress: &mut dyn FnMut(ArchiveProgress) -> bool,
) -> Result<PathBuf, ArchiveError> {
    create_rar_from_prefs_dir(
        &crate::profile::resolve_config_dir(),
        sources,
        destination_path,
        policy,
        level,
        progress,
    )
}

/// Like [`create_rar_progress_with_level`] with an explicit prefs folder: loads `rar_exe_path`
/// from the prefs in `config_dir`, validates it (including the probe), then runs
/// [`create_rar_with_tool`]. Unset or invalid path → `UnsupportedFormat`, and no archiving
/// process is started.
pub fn create_rar_from_prefs_dir(
    config_dir: &Path,
    sources: &[String],
    destination_path: &Path,
    policy: ArchiveConflictPolicy,
    level: ArchiveCompressionLevel,
    progress: &mut dyn FnMut(ArchiveProgress) -> bool,
) -> Result<PathBuf, ArchiveError> {
    precheck_conflict(destination_path, policy)?;
    crate::archive_contract::check_cancelled()?;
    let rar_path = configured_rar_path(config_dir)?;
    let tool = validate_rar_exe(&rar_path).map_err(|e| {
        ArchiveError::new(
            ArchiveErrorCode::UnsupportedFormat,
            format!("Invalid Rar.exe path: {e}"),
        )
    })?;
    create_rar_with_tool(&tool, sources, destination_path, policy, level, progress)
}

/// Core of RAR creation with an already validated [`RarTool`].
///
/// Same safe pipeline as ZIP/7z:
/// 1. Prechecks the destination conflict.
/// 2. `collect_sources` (skips symlinks/junctions per D07, staging files and the destination).
/// 3. Creates an exclusive job folder next to the destination; listfiles hold exactly the
///    collected entries (files, and empty folders as bare entries).
/// 4. Runs Rar.exe (fixed whitelist args with `-r-`, `CREATE_NO_WINDOW`, `env_remove("RAR")`,
///    working dir = base folder of the entries) once per base folder, re-checking the
///    executable right before each spawn.
/// 5. Cooperative cancellation kills the child (by handle) and waits for it.
/// 6. Publishes `archive.rar` from the job folder with the shared conflict policy; the job
///    folder guard removes everything left on every path.
pub fn create_rar_with_tool(
    tool: &RarTool,
    sources: &[String],
    destination_path: &Path,
    policy: ArchiveConflictPolicy,
    level: ArchiveCompressionLevel,
    progress: &mut dyn FnMut(ArchiveProgress) -> bool,
) -> Result<PathBuf, ArchiveError> {
    precheck_conflict(destination_path, policy)?;
    crate::archive_contract::check_cancelled()?;

    let default_limits = ArchiveLimits {
        max_entries: 100_000,
        max_total_bytes: u64::MAX,
        max_entry_bytes: 512 * 1024 * 1024,
        max_depth: 32,
    };
    let files = collect_sources(sources, destination_path, policy, &default_limits)?;
    if files.is_empty() {
        return Err(ArchiveError::new(
            ArchiveErrorCode::InvalidMemberPath,
            "No files to archive",
        ));
    }
    let groups = group_by_base(&files)?;
    crate::archive_contract::check_cancelled()?;

    let job = JobDir::create(destination_path)?;
    let archive_path = job.path().join("archive.rar");

    let size_of = |e: &&SourceEntry| -> u64 {
        if e.is_dir {
            0
        } else {
            e.disk_path.metadata().map(|m| m.len()).unwrap_or(0)
        }
    };
    let total_bytes: u64 = files.iter().map(|e| size_of(&e)).sum();
    let mut track = CreateTrack {
        bytes: 0,
        entries: 0,
        total_bytes: Some(total_bytes),
        total_entries: Some(files.len() as u64),
    };

    let mut done_bytes = 0u64;
    for (i, group) in groups.iter().enumerate() {
        let list_path = job.path().join(format!("list-{i}.txt"));
        write_listfile(&list_path, &group.entries)?;
        let group_bytes: u64 = group.entries.iter().map(size_of).sum();
        let args = build_rar_args(level, &archive_path, &list_path);
        run_rar(
            tool,
            &args,
            &group.base,
            &mut track,
            done_bytes,
            group_bytes,
            progress,
        )?;
        done_bytes += group_bytes;
        track.entries += group.entries.len() as u64;
    }

    if !archive_path.is_file() {
        return Err(ArchiveError::new(
            ArchiveErrorCode::Io,
            "Rar.exe reported success but produced no archive",
        ));
    }

    // Final 100% progress event
    track.bytes = total_bytes;
    track.entries = files.len() as u64;
    let _ = track.emit(progress, None);

    // `job` is dropped after publishing: the folder (and anything left in it) is removed.
    publish_created(&archive_path, destination_path, policy)
}

/// One Rar.exe invocation. Progress maps Rar's percentage into `[done, done + group_bytes]`.
fn run_rar(
    tool: &RarTool,
    args: &[OsString],
    cwd: &Path,
    track: &mut CreateTrack,
    done_bytes: u64,
    group_bytes: u64,
    progress: &mut dyn FnMut(ArchiveProgress) -> bool,
) -> Result<(), ArchiveError> {
    crate::archive_contract::check_cancelled()?;

    let mut cmd = std::process::Command::new(tool.path());
    cmd.args(args);
    cmd.env_remove("RAR");
    cmd.current_dir(cwd);
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }

    // TOCTOU: re-check the executable immediately before spawning it.
    check_rar_exe_file(tool.path()).map_err(|e| {
        ArchiveError::new(
            ArchiveErrorCode::UnsupportedFormat,
            format!("Invalid Rar.exe path: {e}"),
        )
    })?;
    let mut child = cmd.spawn().map_err(|e| {
        ArchiveError::new(
            ArchiveErrorCode::Io,
            format!("Failed to spawn Rar.exe: {e}"),
        )
    })?;

    let mut stderr_pipe = child.stderr.take();
    let stderr_reader = std::thread::spawn(move || {
        use std::io::Read;
        let mut text = Vec::new();
        if let Some(mut pipe) = stderr_pipe.take() {
            let mut chunk = [0u8; 1024];
            while let Ok(n) = pipe.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                if text.len() < 2048 {
                    let to_take = n.min(2048 - text.len());
                    text.extend_from_slice(&chunk[..to_take]);
                }
            }
        }
        String::from_utf8_lossy(&text).into_owned()
    });

    let (stdout_tx, stdout_rx) = std::sync::mpsc::channel::<u64>();
    let mut stdout_pipe = child.stdout.take();
    let stdout_reader = std::thread::spawn(move || {
        use std::io::Read;
        if let Some(mut pipe) = stdout_pipe.take() {
            let mut buf = [0u8; 256];
            let mut num_acc = Vec::new();
            while let Ok(n) = pipe.read(&mut buf) {
                if n == 0 {
                    break;
                }
                for &b in &buf[..n] {
                    if b.is_ascii_digit() {
                        num_acc.push(b);
                    } else if b == b'%' {
                        if let Some(pct) = std::str::from_utf8(&num_acc)
                            .ok()
                            .and_then(|s| s.parse::<u64>().ok())
                        {
                            let _ = stdout_tx.send(pct.min(100));
                        }
                        num_acc.clear();
                    } else {
                        num_acc.clear();
                    }
                }
            }
        }
    });

    let mut current_pct = 0u64;
    let wait_result = loop {
        while let Ok(pct) = stdout_rx.try_recv() {
            current_pct = current_pct.max(pct);
        }
        track.bytes = done_bytes + group_bytes.saturating_mul(current_pct) / 100;
        if crate::archive_contract::is_cancelled() || !track.emit(progress, None) {
            let _ = child.kill();
            let _ = child.wait();
            break Err(cancelled());
        }
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => std::thread::sleep(Duration::from_millis(30)),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(ArchiveError::new(
                    ArchiveErrorCode::Io,
                    format!("Rar process error: {e}"),
                ));
            }
        }
    };

    let stderr_msg = stderr_reader.join().unwrap_or_default();
    let _ = stdout_reader.join();
    let status = wait_result?;

    if status.code() == Some(255) {
        return Err(cancelled());
    }
    if !status.success() {
        let msg = if !stderr_msg.trim().is_empty() {
            stderr_msg.trim().to_string()
        } else {
            format!("Rar failed with exit code {:?}", status.code())
        };
        return Err(ArchiveError::new(ArchiveErrorCode::Io, msg));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_rar_args_exact_order_and_whitelist() {
        let archive = Path::new(r"C:\Dest\.my.rar.123-9.ferrum-tmp\archive.rar");
        let list = Path::new(r"C:\Dest\.my.rar.123-9.ferrum-tmp\list-0.txt");

        for (level, m_flag) in [
            (ArchiveCompressionLevel::Store, "-m0"),
            (ArchiveCompressionLevel::Fast, "-m1"),
            (ArchiveCompressionLevel::Normal, "-m3"),
            (ArchiveCompressionLevel::Max, "-m5"),
        ] {
            let args: Vec<String> = build_rar_args(level, archive, list)
                .into_iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            let mut expected: Vec<String> = [
                "a", "-cfg-", "-idc", "-ma5", m_flag, "-r-", "-y", "-o-", "-scul", "--",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect();
            expected.push(archive.to_string_lossy().into_owned());
            expected.push(format!("@{}", list.to_string_lossy()));
            assert_eq!(args, expected);
            assert_eq!(args[1], "-cfg-", "-cfg- is mandatory for security");
            assert_eq!(args[9], "--", "-- must be immediately before archive name");

            // Forbidden switches check (incl. any recursion switch other than `-r-`).
            for arg in &args[..10] {
                let forbidden = [
                    "-df", "-dr", "-ac", "-ow", "-sfx", "-k", "-av", "-p", "-hp", "-ilog", "-ep",
                ];
                for bad in forbidden {
                    assert!(
                        !arg.starts_with(bad),
                        "forbidden switch {bad} must not appear in args"
                    );
                }
                assert!(
                    !arg.starts_with("-r") || arg == "-r-",
                    "recursion must stay disabled ({arg})"
                );
            }
        }
    }

    #[test]
    fn test_validate_rar_exe_rejects_invalid_paths() {
        // Relative path
        let err = validate_rar_exe(Path::new("Rar.exe")).unwrap_err();
        assert!(matches!(err, RarValidationError::NotLocalDrive));

        // UNC path
        let err = validate_rar_exe(Path::new(r"\\server\share\Rar.exe")).unwrap_err();
        assert!(matches!(err, RarValidationError::NotLocalDrive));

        // Device path
        let err = validate_rar_exe(Path::new(r"\\?\C:\WinRAR\Rar.exe")).unwrap_err();
        assert!(matches!(err, RarValidationError::NotLocalDrive));

        // Path with ..
        let err = validate_rar_exe(Path::new(r"C:\Programs\..\WinRAR\Rar.exe")).unwrap_err();
        assert!(matches!(err, RarValidationError::TraversalComponent));

        // Wrong names
        for bad_name in ["WinRAR.exe", "UnRAR.exe", "rar.dll", "cmd.exe"] {
            let p = format!(r"C:\Program Files\WinRAR\{bad_name}");
            let err = validate_rar_exe(Path::new(&p)).unwrap_err();
            assert!(
                matches!(err, RarValidationError::InvalidFilename(_)),
                "rejected {bad_name}"
            );
        }

        // File not found
        let err =
            validate_rar_exe(Path::new(r"C:\does-not-exist-ferrum-test\Rar.exe")).unwrap_err();
        assert!(matches!(err, RarValidationError::NotFound(_)));
    }

    /// Owned temp folder for the link tests, removed on Drop.
    struct Scratch(PathBuf);
    impl Scratch {
        fn new(tag: &str) -> Self {
            let mut b = [0u8; 8];
            getrandom::fill(&mut b).unwrap();
            let p = std::env::temp_dir().join(format!(
                "ferrum-rartool-{tag}-{}-{}",
                std::process::id(),
                u64::from_le_bytes(b)
            ));
            std::fs::create_dir(&p).unwrap();
            Scratch(p)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[cfg(windows)]
    #[test]
    fn test_validate_rar_exe_rejects_junction_named_rar_exe() {
        let s = Scratch::new("junction");
        let target = s.0.join("real");
        std::fs::create_dir(&target).unwrap();
        let link = s.0.join("Rar.exe");
        // Junctions need no privilege (unlike symlinks).
        let ok = std::process::Command::new("cmd")
            .arg("/C")
            .arg("mklink")
            .arg("/J")
            .arg(&link)
            .arg(&target)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|st| st.success())
            .unwrap_or(false);
        assert!(ok, "mklink /J must succeed without privileges");
        let err = validate_rar_exe(&link).unwrap_err();
        assert!(
            matches!(err, RarValidationError::SymlinkRefused(_)),
            "junction must be refused, got {err:?}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn test_validate_rar_exe_rejects_file_symlink_named_rar_exe() {
        let s = Scratch::new("symlink");
        let target = s.0.join("target.exe");
        std::fs::write(&target, b"not really an exe").unwrap();
        let link = s.0.join("Rar.exe");
        if let Err(e) = std::os::windows::fs::symlink_file(&target, &link) {
            eprintln!(
                "SKIP: creating a file symlink needs SeCreateSymbolicLinkPrivilege / Developer Mode ({e}); junction test covers reparse points"
            );
            return;
        }
        let err = validate_rar_exe(&link).unwrap_err();
        assert!(
            matches!(err, RarValidationError::SymlinkRefused(_)),
            "symlink must be refused, got {err:?}"
        );
    }

    #[test]
    fn test_validate_rar_exe_rejects_directory_named_rar_exe() {
        let s = Scratch::new("dir");
        let dir = s.0.join("Rar.exe");
        std::fs::create_dir(&dir).unwrap();
        let err = validate_rar_exe(&dir).unwrap_err();
        assert!(
            matches!(err, RarValidationError::NotRegularFile(_)),
            "directory must be refused, got {err:?}"
        );
    }

    #[test]
    fn test_validate_rar_exe_rejects_non_rar_binary_named_rar_exe() {
        let s = Scratch::new("fake");
        let fake = s.0.join("Rar.exe");
        std::fs::write(&fake, b"MZ not a real program").unwrap();
        let err = validate_rar_exe(&fake).unwrap_err();
        assert!(
            matches!(err, RarValidationError::ProbeFailed(_)),
            "fake binary must fail the probe, got {err:?}"
        );
    }

    #[test]
    fn test_group_by_base_and_listfile_names() {
        let entries = vec![
            SourceEntry {
                disk_path: PathBuf::from(r"C:\a\src\sub\x.txt"),
                archive_name: "src/sub/x.txt".into(),
                is_dir: false,
            },
            SourceEntry {
                disk_path: PathBuf::from(r"C:\a\x.txt"),
                archive_name: "x.txt".into(),
                is_dir: false,
            },
            SourceEntry {
                disk_path: PathBuf::from(r"C:\b\y.txt"),
                archive_name: "y.txt".into(),
                is_dir: false,
            },
        ];
        let groups = group_by_base(&entries).unwrap();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].base, PathBuf::from(r"C:\a"));
        assert_eq!(groups[0].entries.len(), 2);
        assert_eq!(groups[1].base, PathBuf::from(r"C:\b"));

        let s = Scratch::new("list");
        let list = s.0.join("list-0.txt");
        write_listfile(&list, &groups[0].entries).unwrap();
        let raw = std::fs::read(&list).unwrap();
        let (pairs, rest) = raw.as_chunks::<2>();
        assert!(rest.is_empty());
        let units: Vec<u16> = pairs.iter().map(|c| u16::from_le_bytes(*c)).collect();
        assert_eq!(
            String::from_utf16(&units).unwrap(),
            "src\\sub\\x.txt\r\nx.txt\r\n"
        );
        // Exclusive: never overwrites an existing listfile.
        assert!(write_listfile(&list, &groups[0].entries).is_err());
    }

    #[test]
    fn test_job_dir_is_exclusive_staging_and_removed_on_drop() {
        let s = Scratch::new("job");
        let dest = s.0.join("out.rar");
        let job = JobDir::create(&dest).unwrap();
        let p = job.path().to_path_buf();
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with(".out.rar.") && name.ends_with(".ferrum-tmp"));
        std::fs::write(p.join("archive.rar"), b"x").unwrap();
        let job2 = JobDir::create(&dest).unwrap();
        assert_ne!(job2.path(), p.as_path());
        drop(job);
        drop(job2);
        assert!(!p.exists(), "job dir removed on drop");
        assert_eq!(std::fs::read_dir(&s.0).unwrap().count(), 0);
    }

    #[test]
    fn test_parse_rar_version_output() {
        let sample = "\nRAR 7.00 x64   Copyright (c) 1993-2024 Alexander Roshal   26 Feb 2024\nTrial version\n";
        assert_eq!(
            parse_rar_version_output(sample),
            Some("RAR 7.00 x64".to_string())
        );

        let sample_older = "RAR 6.24   Copyright (c) 1993-2023 Alexander Roshal\n";
        assert_eq!(
            parse_rar_version_output(sample_older),
            Some("RAR 6.24".to_string())
        );

        let bad_sample = "WinRAR 7.00\nNo RAR line here\n";
        assert_eq!(parse_rar_version_output(bad_sample), None);
    }
}
