//! WinRAR (Rar.exe) discovery, validation, probing, and execution (FER-505).
//!
//! Ferrum does not distribute or require WinRAR. Users who want to create RAR archives
//! install WinRAR themselves and specify their `Rar.exe` path in Settings. Reading RAR archives
//! remains handled by the safe internal `unrar` crate.
//!
//! Security boundary:
//! - The executable path comes **only** from the prefs file on disk (never from IPC, env or AI).
//! - FER-511: the webview cannot write the path either — `save_prefs` keeps the value on disk,
//!   and the only way to change it is `set_rar_exe_path_in`, which shows a NATIVE confirmation
//!   and stores the file's fingerprint alongside the path.
//! - Absolute local drive paths only (reject UNC `\\...`, device `\\?\`, relative, `..`).
//! - Exact filename `Rar.exe` (case-insensitive).
//! - Regular file only (reject symlinks and junctions).
//! - FER-511: the confirmed fingerprint (`size`, `modified`, sha-256) is re-checked before every
//!   execution — probe and create alike. Unconfirmed or changed files are never run.
//! - FER-510: only Rar.exe 7.00+ is accepted (probed after the fingerprint gate). Older builds
//!   search `rarlng.dll` in the working directory — the user's source folder here — which is a
//!   DLL-planting risk.
//! - FER-514: the path may not contain control or bidi/format characters (spoofed confirmation
//!   text). The file is opened ONCE with a share mode that denies write/delete, hashed through
//!   that very handle, and the handle is held until the process is spawned, so the file cannot be
//!   swapped between hashing and `CreateProcess`. A file larger than [`MAX_RAR_EXE_BYTES`] is
//!   refused before it is hashed.
//! - FER-514 (DLL sideload): the Rar.exe and its folder chain must pass the same
//!   `trusted_location` rule as agent CLIs (FER-490, with conditional ACEs judged by their
//!   rights so the stock Program Files\WinRAR folder passes): writable only by the current user,
//!   Administrators, SYSTEM or TrustedInstaller. Rar.exe loads helper DLLs from its own folder, so
//!   a folder another account can write to is refused (at set_path, probe and before every spawn).
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
use crate::prefs::RarExeFingerprint;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

/// FER-514: a real Rar.exe is a couple of MiB. Anything above this is refused before hashing, so a
/// configured multi-GiB file named `Rar.exe` cannot keep a worker thread busy reading it.
pub const MAX_RAR_EXE_BYTES: u64 = 64 * 1024 * 1024;

/// FER-514 rework (P3-G): characters that can make a path look different from what it is:
/// control, bidi/format characters, ANY whitespace other than a plain space (U+2028/U+2029, NBSP,
/// ideographic space ...) and invisible fillers (Hangul fillers U+115F/U+1160/U+3164/U+FFA0,
/// braille blank U+2800).
fn is_spoofing_char(c: char) -> bool {
    c.is_control()
        || crate::hotlist::model::is_format_char(c)
        || (c.is_whitespace() && c != ' ')
        || matches!(c as u32, 0x115F | 0x1160 | 0x3164 | 0xFFA0 | 0x2800)
}

/// A `Rar.exe` that passed [`validate_confirmed_rar_exe`]. Only that function can construct it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RarTool {
    path: PathBuf,
    version: String,
    /// FER-511: the fingerprint confirmed at `set_path` time and re-verified before every spawn.
    fingerprint: RarExeFingerprint,
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
    /// FER-511: the prefs carry no confirmed fingerprint for this path (never confirmed, or old
    /// prefs from before the fingerprint existed). The file is never executed in this state.
    Unconfirmed(String),
    /// FER-511: the file on disk no longer matches the confirmed fingerprint (content, size or
    /// mtime changed). Refused without running anything.
    FingerprintChanged(String),
    /// FER-510: the exe is a real Rar.exe but older than [`MIN_RAR_VERSION`]. Old builds search
    /// `rarlng.dll` in the working directory (here: the user's source folder) — a DLL-planting
    /// risk. Refused after the fingerprint gate, with the version that was found.
    VersionTooOld(String),
    /// FER-514: control or bidi/format characters in the path (they could make the confirmation
    /// dialog show a different path than the one that is stored and run).
    FormatCharacters,
    /// FER-514: the file is larger than [`MAX_RAR_EXE_BYTES`]; it is never hashed or run.
    TooLarge(u64),
    /// FER-514: the exe or one of its folders can be changed by another account (DLL sideload).
    UntrustedLocation(String),
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
                    "Path is a symlink or junction, which is refused for security: {path}. Choose the real folder (junctions such as Scoop's 'current' are not accepted)"
                )
            }
            Self::NotRegularFile(path) => {
                write!(f, "Path is a directory, not a regular file: {path}")
            }
            Self::Io(err) => write!(f, "I/O error inspecting file: {err}"),
            Self::ProbeFailed(err) => write!(f, "Probe failed: {err}"),
            Self::Unconfirmed(path) => write!(
                f,
                "Path Rar.exe belum dikonfirmasi lewat dialog konfirmasi; atur ulang di Settings ({path})"
            ),
            Self::FingerprintChanged(path) => write!(
                f,
                "WinRAR berubah sejak dikonfirmasi; atur ulang di Settings ({path})"
            ),
            Self::VersionTooOld(found) => write!(
                f,
                "WinRAR 7.00 atau lebih baru diperlukan (ditemukan: {found})"
            ),
            Self::FormatCharacters => write!(
                f,
                "Path berisi karakter kontrol atau karakter format arah (bidi) yang tidak diizinkan"
            ),
            Self::TooLarge(size) => write!(
                f,
                "Rar.exe terlalu besar ({size} byte; batas {MAX_RAR_EXE_BYTES} byte)"
            ),
            Self::UntrustedLocation(why) => write!(
                f,
                "Rar.exe harus berada di folder yang hanya bisa diubah akun Anda, Administrators atau SYSTEM (mis. C:\\Program Files\\WinRAR): {why}"
            ),
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

/// `GetDriveTypeW` value of a network drive (`DRIVE_REMOTE`).
const DRIVE_REMOTE: u32 = 4;

/// FER-514 P3-8: a mapped network drive (`Z:` -> `\\server\share`) looks like a local drive
/// letter; only its drive type gives it away. Pure classification of a `GetDriveTypeW` value.
pub fn drive_type_is_remote(drive_type: u32) -> bool {
    drive_type == DRIVE_REMOTE
}

/// True when the drive letter of `path` is a mapped network drive. Windows only; an unreadable
/// type (`DRIVE_UNKNOWN`, `DRIVE_NO_ROOT_DIR`) is not "remote" (a missing drive fails later as
/// NotFound).
#[cfg(windows)]
fn is_mapped_network_drive(path_str: &str) -> bool {
    use windows_sys::Win32::Storage::FileSystem::GetDriveTypeW;
    let letter = path_str.as_bytes()[0].to_ascii_uppercase();
    let root: [u16; 4] = [u16::from(letter), u16::from(b':'), u16::from(b'\\'), 0];
    // SAFETY: `root` is a NUL-terminated UTF-16 drive root.
    drive_type_is_remote(unsafe { GetDriveTypeW(root.as_ptr()) })
}

#[cfg(not(windows))]
fn is_mapped_network_drive(_path_str: &str) -> bool {
    false
}

/// Checks whether `path` is an absolute local drive path (e.g. `C:\...` or `D:/...`).
/// Specifically rejects UNC (`\\server\share`), device paths (`\\?\...`, `\\.\...`),
/// relative paths, paths containing `..`, and (FER-514) drive letters mapped to a network share.
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
    if is_mapped_network_drive(&path_str) {
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
    if path_str.chars().any(is_spoofing_char) {
        return Err(RarValidationError::FormatCharacters);
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

    // FER-559 (P3-E): no folder above the exe may be a symlink or junction. The ACL walk and the
    // path itself follow reparse points, so a junction in the chain would let the checked chain
    // differ from the one that runs. Refusing is simpler and stricter than resolving the final
    // path: pick the real install folder instead of a link to it. (The exe is then held open with
    // FILE_SHARE_READ, which also keeps its folders from being renamed or replaced.)
    for dir in path.ancestors().skip(1) {
        if dir.parent().is_none() {
            break; // the volume root
        }
        match std::fs::symlink_metadata(dir) {
            Ok(m) if m.file_type().is_symlink() => {
                return Err(RarValidationError::SymlinkRefused(
                    dir.display().to_string(),
                ));
            }
            Ok(_) => {}
            // A folder that is not there: the exe check below reports "not found".
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            // Anything else (access denied, ...): the chain cannot be vetted, so it is refused.
            Err(e) => return Err(RarValidationError::Io(e)),
        }
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

/// FER-514: the exe and its folder chain must not be changeable by another account (same rule as
/// agent CLIs, FER-490). Windows only; elsewhere there is nothing to check (Windows-only scope).
fn check_trusted_location(path: &Path) -> Result<(), RarValidationError> {
    // Unit tests run in %TEMP%, whose ACL depends on the machine (CI vs a developer PC with
    // several accounts). They skip this rule unless a test opts in (`tests::real_trust_check`).
    #[cfg(test)]
    if !tests::real_trust_check() {
        return Ok(());
    }
    #[cfg(windows)]
    {
        crate::ai::agent_cli::acl::trusted_location_callback_aware(path)
            .map_err(RarValidationError::UntrustedLocation)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Ok(())
    }
}

/// FER-514: open the exe ONCE for reading with a share mode that lets others read but not write,
/// rename or delete it (`FILE_SHARE_READ`), without following a reparse point. The returned handle
/// is what gets hashed, and the caller keeps it alive until the process is spawned.
fn open_locked(path: &Path) -> Result<File, RarValidationError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 0x1;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options
            .share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            RarValidationError::NotFound(path.display().to_string())
        } else if path.is_dir() {
            // Windows refuses to open a folder as a plain file: report what it is.
            RarValidationError::NotRegularFile(path.display().to_string())
        } else {
            RarValidationError::Io(e)
        }
    })?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        let meta = file.metadata()?;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(RarValidationError::SymlinkRefused(
                path.display().to_string(),
            ));
        }
    }
    Ok(file)
}

/// Fingerprint computed through an already open (locked) handle: size and mtime come from the
/// handle, the hash is read from it, and never more than [`MAX_RAR_EXE_BYTES`] + 1 bytes.
fn fingerprint_file(file: &File, path: &Path) -> Result<RarExeFingerprint, RarValidationError> {
    use std::io::Read;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(RarValidationError::NotRegularFile(
            path.display().to_string(),
        ));
    }
    if meta.len() > MAX_RAR_EXE_BYTES {
        return Err(RarValidationError::TooLarge(meta.len()));
    }
    let modified = meta
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let mut hasher = Sha256::new();
    let mut reader = file.take(MAX_RAR_EXE_BYTES + 1);
    let copied = std::io::copy(&mut reader, &mut hasher)?;
    if copied > MAX_RAR_EXE_BYTES {
        return Err(RarValidationError::TooLarge(copied));
    }
    let sha256: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok(RarExeFingerprint {
        size: meta.len(),
        modified,
        sha256,
    })
}

/// FER-511: identity of the executable file: `{ size, modified, sha256 }`. Reading and hashing
/// the file never runs it. FER-514: through a locked handle (see [`open_locked`]); the lock is
/// released when this returns.
pub fn fingerprint_exe(path: &Path) -> Result<RarExeFingerprint, RarValidationError> {
    let file = open_locked(path)?;
    fingerprint_file(&file, path)
}

/// Validate a `Rar.exe` the user wants to RUN (FER-511): static checks of section 3.1, then the
/// fingerprint gate — the stored fingerprint must exist and match the file on disk — and only
/// then the probe (which executes the file). With no stored fingerprint, or on a mismatch,
/// nothing is ever executed.
pub fn validate_confirmed_rar_exe(
    path: &Path,
    stored: Option<&RarExeFingerprint>,
) -> Result<RarTool, RarValidationError> {
    check_rar_exe_file(path)?;
    // FER-514: the handle stays open (write/delete denied) through the probe spawn below.
    let held = open_locked(path)?;
    let actual = fingerprint_file(&held, path)?;
    match stored {
        None => return Err(RarValidationError::Unconfirmed(path.display().to_string())),
        Some(expected) if expected != &actual => {
            return Err(RarValidationError::FingerprintChanged(
                path.display().to_string(),
            ));
        }
        _ => {}
    }
    // After the fingerprint gate (so "unconfirmed"/"changed" keep their own messages) and
    // before the probe, which is the first time the file is executed.
    check_trusted_location(path)?;
    let version = probe_rar_version(path)?;
    drop(held);
    Ok(RarTool {
        path: path.to_path_buf(),
        version,
        fingerprint: actual,
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

    let version = parse_rar_version_output(&stdout_text).ok_or_else(|| {
        RarValidationError::ProbeFailed(
            "Rar.exe did not output a valid 'RAR x.yy' version line".to_string(),
        )
    })?;
    // FER-510: refuse builds older than 7.00 — they search `rarlng.dll` in the working
    // directory, which on this feature is a folder owned by the user (DLL planting).
    enforce_min_rar_version(&version)?;
    Ok(version)
}

/// FER-510: minimum accepted `Rar.exe` version. WinRAR before 7.00 can look for `rarlng.dll`
/// in the working directory (the user's source folder during a create), which enables DLL
/// planting; 7.00+ is required.
pub const MIN_RAR_VERSION: (u32, u32) = (7, 0);

/// Numeric `(major, minor)` out of a version line like `RAR 7.10 beta 1 x64` → `(7, 10)`.
/// `None` when the line carries no `major.minor` pair of ASCII digits.
pub fn rar_version_tuple(version_line: &str) -> Option<(u32, u32)> {
    let token = version_line
        .trim()
        .strip_prefix("RAR ")?
        .split_whitespace()
        .next()?;
    let (major, minor) = token.split_once('.')?;
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(major) || !digits(minor) {
        return None;
    }
    Some((major.parse().ok()?, minor.parse().ok()?))
}

/// FER-510: the pure half of the version gate (no spawn): anything below
/// [`MIN_RAR_VERSION`] is refused with the clear [`RarValidationError::VersionTooOld`] error.
/// Called from [`probe_rar_version`], which only runs from
/// [`validate_confirmed_rar_exe`] — after the fingerprint gate — so an old exe is never
/// executed and the check can never bypass the fingerprint.
fn enforce_min_rar_version(version_line: &str) -> Result<(), RarValidationError> {
    match rar_version_tuple(version_line) {
        Some(v) if v >= MIN_RAR_VERSION => Ok(()),
        Some(_) => Err(RarValidationError::VersionTooOld(version_line.to_owned())),
        None => Err(RarValidationError::ProbeFailed(format!(
            "Rar.exe did not output a valid 'RAR x.yy' version line: {version_line}"
        ))),
    }
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
        // FER-579: refused up front by `check_for` (callers); never map it to another level.
        ArchiveCompressionLevel::UltraText => {
            debug_assert!(false, "ultra-text is 7z only");
            "-m5"
        }
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

/// FER-511: the confirmed `Rar.exe` from the prefs file in `config_dir`: the path AND its
/// stored fingerprint come from disk only, and the fingerprint is re-checked here (and again
/// right before every spawn). Unset → `UnsupportedFormat`; unconfirmed or changed → refused
/// without running anything.
fn configured_rar_tool(config_dir: &Path) -> Result<RarTool, ArchiveError> {
    let prefs = crate::prefs::load_from(config_dir);
    let Some(p) = prefs.rar_exe_path.filter(|p| !p.trim().is_empty()) else {
        return Err(ArchiveError::new(
            ArchiveErrorCode::UnsupportedFormat,
            "WinRAR (Rar.exe) is not configured in Settings",
        ));
    };
    validate_confirmed_rar_exe(Path::new(&p), prefs.rar_exe_fingerprint.as_ref())
        .map_err(|e| ArchiveError::new(ArchiveErrorCode::UnsupportedFormat, e.to_string()))
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

/// Like [`create_rar_progress_with_level`] with an explicit prefs folder: loads the confirmed
/// `Rar.exe` (path + fingerprint) from the prefs in `config_dir`, re-verifies the fingerprint
/// (which runs [`validate_confirmed_rar_exe`], including the probe), then runs
/// [`create_rar_with_tool`]. Unset, unconfirmed or changed → `UnsupportedFormat`, and no
/// archiving process is started.
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
    let tool = configured_rar_tool(config_dir)?;
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
    level.check_for(crate::archive_contract::ArchiveCreateFormat::Rar)?;
    precheck_conflict(destination_path, policy)?;
    crate::archive_contract::check_cancelled()?;
    crate::archive_create_test::clean_stale_staging(destination_path);

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

    // TOCTOU: re-check the executable immediately before spawning it — the static file checks
    // AND the confirmed fingerprint (FER-511). A Rar.exe swapped after confirmation is refused
    // here and never spawned.
    // FER-514: the verified handle (write/delete denied) is held across the spawn.
    let held = reverify_before_spawn(tool)
        .map_err(|e| ArchiveError::new(ArchiveErrorCode::UnsupportedFormat, e.to_string()))?;
    let spawned = cmd.spawn();
    drop(held);
    let mut child = spawned.map_err(|e| {
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

/// FER-511: the last gate before EVERY spawn (probe already gated in
/// [`validate_confirmed_rar_exe`]): the static file checks plus a freshly hashed fingerprint
/// must equal the one confirmed at `set_path` time. Any difference refuses without running
/// anything — a spawn attempt would surface as `ProbeFailed`, so these variants themselves
/// prove no process was started.
///
/// FER-514: returns the locked handle that was hashed; the caller keeps it until `spawn` returns,
/// so the file cannot be replaced between the hash and the process start.
fn reverify_before_spawn(tool: &RarTool) -> Result<File, RarValidationError> {
    check_rar_exe_file(tool.path())?;
    check_trusted_location(tool.path())?;
    let held = open_locked(tool.path())?;
    let actual = fingerprint_file(&held, tool.path())?;
    if actual != tool.fingerprint {
        return Err(RarValidationError::FingerprintChanged(
            tool.path().display().to_string(),
        ));
    }
    Ok(held)
}

/// FER-511: outcome of [`set_rar_exe_path_in`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetPathOutcome {
    /// The path (with its fingerprint) is stored, or the setting was removed.
    Saved,
    /// `None` was requested: the setting is cleared WITHOUT any confirmation — turning the
    /// feature off is always safe and must never be blocked.
    Cleared,
    /// The user answered the native confirmation with No. Nothing is stored.
    Cancelled,
}

/// The text of the native confirmation: the full path plus what Ferrum will do with it.
fn set_path_confirm_text(path: &Path) -> String {
    format!(
        "Ferrum akan MENJALANKAN program berikut setiap kali Anda membuat arsip RAR:\n\n{}\n\nSimpan dan percayai program ini? (Ferrum memverifikasi file ini lagi sebelum setiap eksekusi.)",
        path.display()
    )
}

fn prefs_write_failed(e: crate::domain::FsError) -> RarValidationError {
    RarValidationError::Io(std::io::Error::other(e.message))
}

/// FER-511: the ONLY way the configured Rar.exe path changes. `None` (or blank) clears the
/// setting without asking. `Some(path)` runs the static validation (path shape, name, regular
/// file — never executing it), then asks via `confirm` — production passes a native
/// `MessageBoxW` owned by the Ferrum window, which the webview can neither draw nor answer —
/// and only on Yes stores the path together with the freshly computed fingerprint. The
/// injectable `confirm` seam mirrors the hotlist confirmation; no test-only hooks live in the
/// production path.
pub fn set_rar_exe_path_in(
    config_dir: &Path,
    path: Option<String>,
    confirm: impl FnOnce(&str) -> bool,
) -> Result<SetPathOutcome, RarValidationError> {
    let Some(raw) = path.filter(|p| !p.trim().is_empty()) else {
        crate::prefs::set_rar_exe(config_dir, None, None).map_err(prefs_write_failed)?;
        return Ok(SetPathOutcome::Cleared);
    };
    let candidate = PathBuf::from(raw.trim());
    check_rar_exe_file(&candidate)?;
    check_trusted_location(&candidate)?;
    let fingerprint = fingerprint_exe(&candidate)?;
    if !confirm(&set_path_confirm_text(&candidate)) {
        return Ok(SetPathOutcome::Cancelled);
    }
    crate::prefs::set_rar_exe(
        config_dir,
        Some(candidate.to_string_lossy().into_owned()),
        Some(fingerprint),
    )
    .map_err(prefs_write_failed)?;
    Ok(SetPathOutcome::Saved)
}

/// FER-511: native Windows message box owned by the Ferrum window (hotlist pattern). The
/// webview can neither draw it nor answer it. Cancel is the default button, so a held Enter
/// key cannot confirm running an unknown program.
pub fn native_confirm(owner: isize, text: &str) -> bool {
    // One native dialog at a time across the whole app; a busy guard answers "no" (nothing saved).
    crate::native_dialog::confirm(owner, "Konfirmasi WinRAR", text)
}

/// FER-514: same message box for a caller that ALREADY holds the app-wide `DialogGuard` (the
/// `archive_rar_tool_set_path` command takes it before it validates and hashes anything).
pub fn native_confirm_held(owner: isize, text: &str) -> bool {
    crate::native_dialog::message_box(owner, "Konfirmasi WinRAR", text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    thread_local! {
        static REAL_TRUST: Cell<bool> = const { Cell::new(false) };
    }

    /// Whether `check_trusted_location` runs the real ACL rule on this test thread.
    pub(super) fn real_trust_check() -> bool {
        REAL_TRUST.with(Cell::get)
    }

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
        let err = validate_confirmed_rar_exe(Path::new("Rar.exe"), None).unwrap_err();
        assert!(matches!(err, RarValidationError::NotLocalDrive));

        // UNC path
        let err =
            validate_confirmed_rar_exe(Path::new(r"\\server\share\Rar.exe"), None).unwrap_err();
        assert!(matches!(err, RarValidationError::NotLocalDrive));

        // Device path
        let err =
            validate_confirmed_rar_exe(Path::new(r"\\?\C:\WinRAR\Rar.exe"), None).unwrap_err();
        assert!(matches!(err, RarValidationError::NotLocalDrive));

        // Path with ..
        let err = validate_confirmed_rar_exe(Path::new(r"C:\Programs\..\WinRAR\Rar.exe"), None)
            .unwrap_err();
        assert!(matches!(err, RarValidationError::TraversalComponent));

        // Wrong names
        for bad_name in ["WinRAR.exe", "UnRAR.exe", "rar.dll", "cmd.exe"] {
            let p = format!(r"C:\Program Files\WinRAR\{bad_name}");
            let err = validate_confirmed_rar_exe(Path::new(&p), None).unwrap_err();
            assert!(
                matches!(err, RarValidationError::InvalidFilename(_)),
                "rejected {bad_name}"
            );
        }

        // File not found
        let err =
            validate_confirmed_rar_exe(Path::new(r"C:\does-not-exist-ferrum-test\Rar.exe"), None)
                .unwrap_err();
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

    fn s(p: &Path) -> String {
        p.to_string_lossy().into_owned()
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
        let err = validate_confirmed_rar_exe(&link, None).unwrap_err();
        assert!(
            matches!(err, RarValidationError::SymlinkRefused(_)),
            "junction must be refused, got {err:?}"
        );
    }

    /// FER-559 (P3-E): a junction ANYWHERE above the exe is refused, however deep, in every
    /// entry point (validate, set-path, re-verify before spawn).
    #[cfg(windows)]
    #[test]
    fn a_junction_in_the_folder_chain_is_refused() {
        let s = Scratch::new("junction-chain");
        let real = s.0.join("real");
        let deep = real.join("sub");
        std::fs::create_dir_all(&deep).unwrap();
        let exe_real = deep.join("Rar.exe");
        std::fs::write(&exe_real, b"MZ not really").unwrap();
        let link = s.0.join("link");
        let ok = std::process::Command::new("cmd")
            .arg("/C")
            .arg("mklink")
            .arg("/J")
            .arg(&link)
            .arg(&real)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|st| st.success())
            .unwrap_or(false);
        assert!(ok, "mklink /J must succeed without privileges");
        let via_link = link.join("sub").join("Rar.exe");
        assert!(via_link.is_file(), "reachable through the junction");

        let err = check_rar_exe_file(&via_link).unwrap_err();
        assert!(
            matches!(&err, RarValidationError::SymlinkRefused(p) if p.ends_with("link")),
            "{err:?}"
        );
        assert!(validate_confirmed_rar_exe(&via_link, None).is_err());
        let cfg = s.0.join("cfg");
        std::fs::create_dir_all(&cfg).unwrap();
        let res = set_rar_exe_path_in(&cfg, Some(via_link.to_string_lossy().into_owned()), |_| {
            true
        });
        assert!(
            matches!(res, Err(RarValidationError::SymlinkRefused(_))),
            "{res:?}"
        );
        // The real spelling has no link in its chain.
        assert!(check_rar_exe_file(&exe_real).is_ok());
        let _ = std::process::Command::new("cmd")
            .arg("/C")
            .arg("rmdir")
            .arg(&link)
            .status();
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
        let err = validate_confirmed_rar_exe(&link, None).unwrap_err();
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
        let err = validate_confirmed_rar_exe(&dir, None).unwrap_err();
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
        // FER-511: reach the probe by passing the file's own fingerprint (the state a user
        // reaches through the confirmed set_path flow). A fake binary must still fail there.
        let fp = fingerprint_exe(&fake).unwrap();
        let err = validate_confirmed_rar_exe(&fake, Some(&fp)).unwrap_err();
        assert!(
            matches!(err, RarValidationError::ProbeFailed(_)),
            "fake binary must fail the probe, got {err:?}"
        );
    }

    // ── FER-511: fingerprint gate + the only path-change flow ───────────────

    #[test]
    fn set_path_cancel_stores_nothing_and_yes_stores_path_with_fingerprint() {
        let root = Scratch::new("setpath");
        let cfg = root.0.join("config");
        std::fs::create_dir(&cfg).unwrap();
        let fake = root.0.join("Rar.exe");
        std::fs::write(&fake, b"MZ not a real program").unwrap();

        // "No": nothing stored, even though the file itself is valid (shape/name/regular).
        let out = set_rar_exe_path_in(&cfg, Some(s(&fake)), |_| false).unwrap();
        assert_eq!(out, SetPathOutcome::Cancelled);
        let prefs = crate::prefs::load_from(&cfg);
        assert_eq!(prefs.rar_exe_path, None, "cancel must store nothing");
        assert_eq!(prefs.rar_exe_fingerprint, None);

        // "Yes": path and freshly computed fingerprint stored together; the dialog text
        // showed the full path and what Ferrum will do with it.
        let asked = std::cell::Cell::new(false);
        let out = set_rar_exe_path_in(&cfg, Some(s(&fake)), |text| {
            asked.set(true);
            assert!(text.contains(&s(&fake)), "dialog shows the path: {text}");
            assert!(text.contains("MENJALANKAN"), "dialog warns it runs: {text}");
            true
        })
        .unwrap();
        assert_eq!(out, SetPathOutcome::Saved);
        assert!(asked.get(), "the confirmation must have been asked");
        let prefs = crate::prefs::load_from(&cfg);
        assert_eq!(prefs.rar_exe_path, Some(s(&fake)));
        assert_eq!(
            prefs.rar_exe_fingerprint,
            Some(fingerprint_exe(&fake).unwrap())
        );
    }

    #[test]
    fn set_path_none_clears_without_any_confirmation() {
        let root = Scratch::new("setclear");
        let cfg = root.0.join("config");
        std::fs::create_dir(&cfg).unwrap();
        let fake = root.0.join("Rar.exe");
        std::fs::write(&fake, b"MZ not a real program").unwrap();
        assert_eq!(
            set_rar_exe_path_in(&cfg, Some(s(&fake)), |_| true).unwrap(),
            SetPathOutcome::Saved
        );

        // Clearing (and a blank string) must never ask: disabling the feature is safe.
        let mut asked = false;
        let out = set_rar_exe_path_in(&cfg, None, |_| {
            asked = true;
            true
        })
        .unwrap();
        assert_eq!(out, SetPathOutcome::Cleared);
        assert!(!asked, "removing the setting must not confirm");
        assert_eq!(
            set_rar_exe_path_in(&cfg, Some("   ".into()), |_| {
                asked = true;
                true
            })
            .unwrap(),
            SetPathOutcome::Cleared
        );
        assert!(!asked);
        let prefs = crate::prefs::load_from(&cfg);
        assert_eq!(prefs.rar_exe_path, None);
        assert_eq!(prefs.rar_exe_fingerprint, None);
    }

    #[test]
    fn unconfirmed_and_changed_exes_are_refused_before_any_spawn() {
        let s = Scratch::new("fpgate");
        let fake = s.0.join("Rar.exe");
        std::fs::write(&fake, b"MZ not a real program").unwrap();

        // Legacy prefs (path without fingerprint): refused as unconfirmed.
        let err = validate_confirmed_rar_exe(&fake, None).unwrap_err();
        assert!(matches!(err, RarValidationError::Unconfirmed(_)), "{err:?}");

        // Modified after confirmation (content changed): refused on the fingerprint.
        let fp = fingerprint_exe(&fake).unwrap();
        std::fs::write(&fake, b"MZ not a real program anymore").unwrap();
        let err = validate_confirmed_rar_exe(&fake, Some(&fp)).unwrap_err();
        assert!(
            matches!(err, RarValidationError::FingerprintChanged(_)),
            "{err:?}"
        );
        assert!(err.to_string().contains("berubah sejak dikonfirmasi"));

        // The refusal variants are NOT ProbeFailed. A real spawn attempt on this garbage
        // binary would surface exactly as ProbeFailed (see the fake-binary test above), so
        // the variant itself is the proof that no process was ever started.
    }

    #[test]
    fn fingerprint_tracks_size_modified_and_content() {
        let s = Scratch::new("fpcalc");
        let fake = s.0.join("Rar.exe");
        std::fs::write(&fake, b"same bytes").unwrap();
        let first = fingerprint_exe(&fake).unwrap();
        assert_eq!(first.size, 10);
        assert_eq!(first.sha256.len(), 64);
        assert_eq!(
            first,
            fingerprint_exe(&fake).unwrap(),
            "stable when untouched"
        );

        std::fs::write(&fake, b"same bytes!").unwrap();
        let second = fingerprint_exe(&fake).unwrap();
        assert_ne!(
            first, second,
            "content/size change must change the identity"
        );

        // Not a regular file / missing file.
        let dir = s.0.join("Rar.exe.dir");
        std::fs::create_dir(&dir).unwrap();
        assert!(matches!(
            fingerprint_exe(&dir).unwrap_err(),
            RarValidationError::NotRegularFile(_)
        ));
        assert!(matches!(
            fingerprint_exe(&s.0.join("missing.exe")).unwrap_err(),
            RarValidationError::NotFound(_)
        ));
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

    /// FER-510: the pure version gate — 5.x/6.x refused with the clear message, 7.00/7.10
    /// accepted, unknown output has no version at all.
    #[test]
    fn rar_versions_below_7_00_are_refused_and_7_x_pass_the_gate() {
        // 5.x and 6.x parse into a version line but are refused with the clear error.
        for line in ["RAR 5.91 x64", "RAR 6.24"] {
            let desc = parse_rar_version_output(line).expect("a version line");
            assert_eq!(desc, line);
            let err = enforce_min_rar_version(&desc).unwrap_err();
            assert!(
                matches!(err, RarValidationError::VersionTooOld(_)),
                "{line} must be refused, got {err:?}"
            );
            assert!(
                err.to_string()
                    .contains("WinRAR 7.00 atau lebih baru diperlukan"),
                "clear message: {err}"
            );
            assert!(
                err.to_string().contains(line),
                "message names the version: {err}"
            );
        }
        assert_eq!(rar_version_tuple("RAR 5.91 x64"), Some((5, 91)));
        assert_eq!(rar_version_tuple("RAR 6.24"), Some((6, 24)));

        // 7.00 (the minimum) and 7.10 pass; a beta suffix does not confuse the tuple.
        for line in ["RAR 7.00 x64", "RAR 7.10 beta 1 x64"] {
            let desc = parse_rar_version_output(line).expect("a version line");
            enforce_min_rar_version(&desc).expect("7.x must pass the gate");
        }
        assert_eq!(rar_version_tuple("RAR 7.00 x64"), Some((7, 0)));
        assert_eq!(rar_version_tuple("RAR 7.10 beta 1 x64"), Some((7, 10)));

        // Unknown output: no version line, so the probe fails before any version check.
        assert_eq!(
            parse_rar_version_output("WinRAR 7.00\nNo RAR line here\n"),
            None
        );
        assert_eq!(rar_version_tuple("RAR seven"), None);
    }

    // ---- FER-514 hardening -------------------------------------------------------------

    /// A "Rar.exe" whose bytes are `bytes`, with its recorded tool identity (no probe needed:
    /// `reverify_before_spawn` is the last gate and only hashes).
    fn fake_tool(dir: &Path, bytes: &[u8]) -> RarTool {
        let exe = dir.join("Rar.exe");
        std::fs::write(&exe, bytes).unwrap();
        RarTool {
            fingerprint: fingerprint_exe(&exe).unwrap(),
            path: exe,
            version: "7.00".into(),
        }
    }

    #[test]
    fn reverify_accepts_the_untouched_file_and_returns_the_held_handle() {
        let s = Scratch::new("rv-ok");
        let tool = fake_tool(&s.0, b"MZ original");
        let held = reverify_before_spawn(&tool).expect("unchanged file passes");
        drop(held);
    }

    /// File swapped between validation and spawn: refused as FingerprintChanged (the variant
    /// proves nothing was started - a spawn attempt would surface as Io/ProbeFailed).
    #[test]
    fn reverify_refuses_a_file_swapped_after_validation() {
        let s = Scratch::new("rv-swap");
        let tool = fake_tool(&s.0, b"MZ original");
        std::fs::write(&tool.path, b"MZ replaced!").unwrap();
        let err = reverify_before_spawn(&tool).unwrap_err();
        assert!(
            matches!(err, RarValidationError::FingerprintChanged(_)),
            "{err:?}"
        );
        // Same length, different bytes: the hash (not just the size) catches it.
        let tool = fake_tool(&s.0, b"MZ original");
        std::fs::write(&tool.path, b"MZ ORIGINAL").unwrap();
        let _ = std::fs::File::options()
            .write(true)
            .open(&tool.path)
            .and_then(|f| {
                f.set_modified(
                    std::time::UNIX_EPOCH + Duration::from_nanos(tool.fingerprint.modified),
                )
            });
        assert!(matches!(
            reverify_before_spawn(&tool).unwrap_err(),
            RarValidationError::FingerprintChanged(_)
        ));
    }

    /// Only the modification time changes (content and size identical): still refused.
    #[test]
    fn reverify_refuses_an_mtime_only_change() {
        let s = Scratch::new("rv-mtime");
        let tool = fake_tool(&s.0, b"MZ same bytes");
        let later = std::time::UNIX_EPOCH
            + Duration::from_nanos(tool.fingerprint.modified)
            + Duration::from_secs(3600);
        std::fs::File::options()
            .write(true)
            .open(&tool.path)
            .unwrap()
            .set_modified(later)
            .unwrap();
        let err = reverify_before_spawn(&tool).unwrap_err();
        assert!(
            matches!(err, RarValidationError::FingerprintChanged(_)),
            "{err:?}"
        );
    }

    /// FER-514 P3-2: while the handle returned by the gate is alive nobody can write, rename or
    /// delete the file; once it is dropped (after spawn) they can again.
    #[cfg(windows)]
    #[test]
    fn the_verified_handle_blocks_swaps_until_dropped() {
        let s = Scratch::new("rv-lock");
        let tool = fake_tool(&s.0, b"MZ original");
        let held = reverify_before_spawn(&tool).unwrap();
        assert!(std::fs::write(&tool.path, b"swap").is_err(), "write denied");
        assert!(
            std::fs::rename(&tool.path, s.0.join("moved.exe")).is_err(),
            "rename denied"
        );
        assert!(std::fs::remove_file(&tool.path).is_err(), "delete denied");
        assert_eq!(std::fs::read(&tool.path).unwrap(), b"MZ original");
        drop(held);
        std::fs::write(&tool.path, b"swap").expect("released after drop");
    }

    /// The lock must not stop the process from starting: a real executable can be spawned while
    /// the verified handle is held.
    #[cfg(windows)]
    #[test]
    fn a_real_exe_still_spawns_while_the_handle_is_held() {
        let s = Scratch::new("rv-spawn");
        let root = std::env::var_os("SystemRoot").expect("SystemRoot");
        let src = Path::new(&root).join("System32").join("whoami.exe");
        let exe = s.0.join("Rar.exe");
        std::fs::copy(&src, &exe).unwrap();
        let tool = RarTool {
            fingerprint: fingerprint_exe(&exe).unwrap(),
            path: exe.clone(),
            version: "7.00".into(),
        };
        let held = reverify_before_spawn(&tool).unwrap();
        let mut cmd = std::process::Command::new(&exe);
        cmd.stdout(std::process::Stdio::null());
        let mut child = cmd.spawn().expect("spawn while the handle is held");
        drop(held);
        let _ = child.wait();
    }

    #[test]
    fn bidi_and_control_characters_in_the_path_are_refused() {
        for bad in [
            "C:\\x\u{202E}gpj\\Rar.exe",
            "C:\\x\u{200F}\\Rar.exe",
            "C:\\x\u{2066}\\Rar.exe",
            "C:\\x\u{FEFF}\\Rar.exe",
            "C:\\x\u{0007}\\Rar.exe",
            "C:\\x\u{2028}y\\Rar.exe",
            "C:\\x\u{2029}y\\Rar.exe",
            "C:\\x\u{00A0}y\\Rar.exe",
            "C:\\x\u{3000}y\\Rar.exe",
            "C:\\x\u{3164}y\\Rar.exe",
            "C:\\x\u{115F}y\\Rar.exe",
            "C:\\x\u{2800}y\\Rar.exe",
            "C:\\x\ty\\Rar.exe",
            "C:\\x\nfake line\\Rar.exe",
        ] {
            let err = check_rar_exe_file(Path::new(bad)).unwrap_err();
            assert!(
                matches!(err, RarValidationError::FormatCharacters),
                "{bad:?}: {err:?}"
            );
        }
        // And through the confirmed set_path flow: refused before any dialog.
        let root = Scratch::new("bidi");
        let mut asked = false;
        let err = set_rar_exe_path_in(&root.0, Some("C:\\x\u{202E}\\Rar.exe".to_string()), |_| {
            asked = true;
            true
        })
        .unwrap_err();
        assert!(
            matches!(err, RarValidationError::FormatCharacters),
            "{err:?}"
        );
        assert!(!asked, "no dialog for a spoofed path");
        assert_eq!(crate::prefs::load_from(&root.0).rar_exe_path, None);
    }

    #[test]
    fn an_oversized_exe_is_refused_before_hashing() {
        let s = Scratch::new("big");
        let exe = s.0.join("Rar.exe");
        let f = std::fs::File::create(&exe).unwrap();
        f.set_len(MAX_RAR_EXE_BYTES + 1).unwrap();
        drop(f);
        let started = Instant::now();
        let err = fingerprint_exe(&exe).unwrap_err();
        assert!(matches!(err, RarValidationError::TooLarge(_)), "{err:?}");
        assert!(started.elapsed() < Duration::from_secs(2), "not hashed");
        let at_limit = s.0.join("limit.bin");
        let f = std::fs::File::create(&at_limit).unwrap();
        f.set_len(1024).unwrap();
        drop(f);
        assert!(fingerprint_exe(&at_limit).is_ok());
    }

    /// FER-514 P3-8: classification of `GetDriveTypeW` results; only DRIVE_REMOTE is a network drive.
    #[test]
    fn only_drive_remote_counts_as_a_network_drive() {
        assert!(drive_type_is_remote(4));
        for local in [0u32, 1, 2, 3, 5, 6] {
            assert!(!drive_type_is_remote(local), "{local}");
        }
    }

    /// The system drive is a local drive; the real-network case needs a mapped drive and is run
    /// by hand: `FERRUM_TEST_NETWORK_DRIVE=Z:\ cargo test -- --ignored mapped_network_drive`.
    #[cfg(windows)]
    #[test]
    fn the_system_drive_is_a_local_drive_path() {
        let root = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
        assert!(is_local_drive_path(Path::new(&format!("{root}\\Rar.exe"))));
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "needs a mapped network drive: set FERRUM_TEST_NETWORK_DRIVE=Z:\\"]
    fn mapped_network_drive_is_not_a_local_drive_path() {
        let root = std::env::var("FERRUM_TEST_NETWORK_DRIVE").expect("FERRUM_TEST_NETWORK_DRIVE");
        assert!(!is_local_drive_path(Path::new(&format!("{root}Rar.exe"))));
    }

    /// FER-514 DLL sideload: a folder another account can write to (Everyone: Modify) is
    /// refused at set_path (before the dialog), at the probe gate and before every spawn. Only
    /// the refusing half is asserted: whether a plain %TEMP% folder passes depends on the machine.
    #[cfg(windows)]
    #[test]
    fn a_folder_writable_by_everyone_is_refused() {
        let sc = Scratch::new("sideload");
        let dir = sc.0.join("WinRAR");
        std::fs::create_dir(&dir).unwrap();
        let exe = dir.join("Rar.exe");
        std::fs::write(&exe, b"MZ not a real program").unwrap();
        let cfg = sc.0.join("cfg");
        std::fs::create_dir(&cfg).unwrap();
        let grant = std::process::Command::new("icacls")
            .arg(&dir)
            .args(["/grant", "*S-1-1-0:(OI)(CI)M", "/T", "/C", "/Q"])
            .output();
        if !matches!(grant, Ok(ref o) if o.status.success()) {
            eprintln!("icacls unavailable; skipping the untrusted half");
            return;
        }
        REAL_TRUST.with(|c| c.set(true));
        let mut asked = false;
        let err = set_rar_exe_path_in(&cfg, Some(s(&exe)), |_| {
            asked = true;
            true
        })
        .unwrap_err();
        assert!(
            matches!(err, RarValidationError::UntrustedLocation(_)),
            "{err:?}"
        );
        assert!(!asked, "refused before the dialog");
        // The probe/spawn gates apply the same rule.
        let fp = fingerprint_exe(&exe).unwrap();
        assert!(matches!(
            validate_confirmed_rar_exe(&exe, Some(&fp)).unwrap_err(),
            RarValidationError::UntrustedLocation(_)
        ));
        let tool = RarTool {
            path: exe,
            version: "7.00".into(),
            fingerprint: fp,
        };
        assert!(matches!(
            reverify_before_spawn(&tool).unwrap_err(),
            RarValidationError::UntrustedLocation(_)
        ));
    }
}
