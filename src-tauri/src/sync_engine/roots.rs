//! Root canonicalization, identity and readiness probes (Automation design §5.8).
//!
//! - `canonical` applies the `path_guard::check_str` string rules (SP-1.4), refuses control and
//!   format (bidi) characters, resolves a mapped network drive to its UNC form
//!   (`WNetGetUniversalNameW`) and resolves junctions/symlinks via `GetFinalPathNameByHandleW`,
//!   strips the `\\?\` prefix and uppercases the drive letter. For a folder that does not exist
//!   yet, the nearest existing ancestor is resolved the same way and the rest is appended.
//!   The result is the display path stored in a profile.
//! - `identity` records what the path pointed at when the profile was saved (SP-7.1): volume
//!   serial + folder file id for a local root; for UNC the lowercase `\\host\share` prefix plus the
//!   same ids when the share can be opened in time (otherwise unknown, never "verified").
//! - `probe` runs both within a per-root time budget (SP-8): a slower root reports `checking`
//!   instead of blocking the readiness answer.
//!
//! All network I/O goes through [`run_bounded`]: a time budget per step and a cap on workers left
//! behind by hung shares.
//!
//! Tauri-free by design (the runner links this module).

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::time::Duration;

use crate::automation::model::{DriveKind, RootIdentity};
use crate::hotlist::model::is_format_char;

/// Why a root could not be probed. Never carries a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootError {
    /// The string form is refused (SP-1.4: device/NT namespace, ADS, pipe/mailslot/IPC$,
    /// relative, control/format characters).
    Invalid,
    /// The path does not exist (or is not a folder).
    NotFound,
    /// The probe failed for another reason (access denied, network down, unsupported platform).
    Io,
}

/// How a root probe ended. `Checking` means the budget ran out (SP-8), never "treat as empty".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootProbeOutcome {
    Ready {
        canonical: String,
        identity: RootIdentity,
    },
    Checking,
    Missing,
    Invalid,
    Unavailable,
}

/// Filesystem access the profile model needs. Injectable so validation and readiness tests run
/// without a real drive (and so a mapped drive can be simulated). `Send + Sync` because a probe
/// runs on its own worker thread within the readiness budget.
pub trait RootProbe: Send + Sync {
    fn canonical(&self, path: &str) -> Result<String, RootError>;
    fn identity(&self, canonical: &str) -> Result<RootIdentity, RootError>;
}

/// The real probe (Win32). Also the fallback shapes for non-Windows builds, where Ferrum's
/// automation is not shipped.
pub struct FsProbe;

impl RootProbe for FsProbe {
    fn canonical(&self, path: &str) -> Result<String, RootError> {
        canonical(path)
    }
    fn identity(&self, canonical: &str) -> Result<RootIdentity, RootError> {
        identity(canonical)
    }
}

/// Readiness budget per root (SP-8).
pub const READINESS_BUDGET: Duration = Duration::from_secs(2);

/// Budget of one network I/O step (final path, folder identity) on a UNC root. Two steps fit in
/// [`READINESS_BUDGET`], so an unreachable share is reported offline instead of `checking`
/// forever.
pub const UNC_STEP_BUDGET: Duration = Duration::from_millis(900);

/// Workers abandoned after their budget ran out (a hung share keeps its thread until the OS call
/// returns). Past this many, new bounded work is refused at once instead of piling more threads
/// up behind dead shares, so a flood from the webview cannot exhaust threads.
const MAX_ABANDONED_WORKERS: usize = 8;
static ABANDONED_WORKERS: AtomicUsize = AtomicUsize::new(0);

const WORKER_RUNNING: u8 = 0;
const WORKER_ABANDONED: u8 = 1;
const WORKER_DONE: u8 = 2;

/// Run `f` on its own thread and wait at most `budget`. `None` = it did not finish in time (the
/// worker is left to finish and is counted until it does) or too many workers are already stuck.
/// Takes owned data only, so a caller never holds a lock while waiting on I/O.
pub fn run_bounded<T: Send + 'static>(
    budget: Duration,
    f: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    run_bounded_in(&ABANDONED_WORKERS, MAX_ABANDONED_WORKERS, budget, f)
}

fn run_bounded_in<T: Send + 'static>(
    abandoned: &'static AtomicUsize,
    max_abandoned: usize,
    budget: Duration,
    f: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    if abandoned.load(Ordering::SeqCst) >= max_abandoned {
        return None;
    }
    let state = Arc::new(AtomicU8::new(WORKER_RUNNING));
    let (tx, rx) = std::sync::mpsc::channel();
    let worker_state = state.clone();
    let spawned = std::thread::Builder::new().spawn(move || {
        struct Finish(Arc<AtomicU8>, &'static AtomicUsize);
        impl Drop for Finish {
            fn drop(&mut self) {
                if self.0.swap(WORKER_DONE, Ordering::SeqCst) == WORKER_ABANDONED {
                    self.1.fetch_sub(1, Ordering::SeqCst);
                }
            }
        }
        let _finish = Finish(worker_state, abandoned);
        let _ = tx.send(f());
    });
    if spawned.is_err() {
        return None;
    }
    match rx.recv_timeout(budget) {
        Ok(v) => Some(v),
        Err(_) => {
            // Count first, then mark: a worker finishing in between sees the mark and undoes it.
            abandoned.fetch_add(1, Ordering::SeqCst);
            if state
                .compare_exchange(
                    WORKER_RUNNING,
                    WORKER_ABANDONED,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                )
                .is_err()
            {
                abandoned.fetch_sub(1, Ordering::SeqCst);
            }
            None
        }
    }
}

/// Probe one root within `budget`. A slower answer becomes [`RootProbeOutcome::Checking`]; the
/// worker thread is detached and never treats the root as missing or empty (SP-8: the caller
/// must never block on an unreachable root).
pub fn probe(path: &str, budget: Duration) -> RootProbeOutcome {
    probe_with(Arc::new(FsProbe), path, budget)
}

/// [`probe`] with an injected prober (tests, mapped-drive simulation). The prober is owned by
/// the detached worker so the caller is never held hostage by a slow volume.
pub fn probe_with(prober: Arc<dyn RootProbe>, path: &str, budget: Duration) -> RootProbeOutcome {
    let path = path.to_string();
    run_bounded(budget, move || probe_once(&*prober, &path)).unwrap_or(RootProbeOutcome::Checking)
}

fn probe_once(prober: &dyn RootProbe, path: &str) -> RootProbeOutcome {
    match prober.canonical(path) {
        Err(RootError::Invalid) => RootProbeOutcome::Invalid,
        Err(_) => RootProbeOutcome::Unavailable,
        Ok(c) => match prober.identity(&c) {
            Ok(identity) => RootProbeOutcome::Ready {
                canonical: c,
                identity,
            },
            Err(RootError::Invalid) => RootProbeOutcome::Invalid,
            Err(RootError::NotFound) => RootProbeOutcome::Missing,
            Err(RootError::Io) => RootProbeOutcome::Unavailable,
        },
    }
}

/// Canonical display path of a root (see module docs). A missing folder keeps its normalized
/// spelling after its nearest EXISTING ancestor is resolved to a real path (junctions and
/// symlinks included), so nesting is judged on real locations (SP-1.5, SP-3.2). The part that
/// does not exist may not contain `.`/`..`, empty, reserved or trailing-dot/space components.
/// Control and format (bidi) characters are refused before and after resolution (SE-2, T4).
pub fn canonical(path: &str) -> Result<String, RootError> {
    let p = crate::path_guard::check_str(path).map_err(|_| RootError::Invalid)?;
    if has_unsafe_char(&p) {
        return Err(RootError::Invalid);
    }
    let p = resolve_mapped_drive(&p);
    let out = if is_unc(&p) {
        // Network I/O is bounded; a share that does not answer keeps its normalized spelling.
        let owned = p.clone();
        match run_bounded(UNC_STEP_BUDGET, move || final_dir_path(&owned)).flatten() {
            Some(final_path) => normalize_display(&final_path),
            None => canonical_missing(&p, false)?,
        }
    } else {
        match final_dir_path(&p) {
            Some(final_path) => normalize_display(&final_path),
            None => canonical_missing(&p, true)?,
        }
    };
    if has_unsafe_char(&out) {
        return Err(RootError::Invalid);
    }
    Ok(out)
}

fn has_unsafe_char(s: &str) -> bool {
    s.chars().any(|c| c.is_control() || is_format_char(c))
}

/// Split `C:\a\b` into (`C:\`, [a, b]) and `\\host\share\a` into (`\\host\share`, [a]).
fn split_root(p: &str) -> Option<(String, Vec<String>)> {
    let (prefix, rest) = if let Some(unc) = p.strip_prefix(r"\\") {
        let mut it = unc.splitn(3, '\\');
        let host = it.next()?;
        let share = it.next()?;
        (format!(r"\\{host}\{share}"), it.next().unwrap_or(""))
    } else {
        let b = p.as_bytes();
        if !(b.len() >= 3 && b[1] == b':' && b[2] == b'\\') {
            return None;
        }
        (p[..3].to_string(), &p[3..])
    };
    let rest = rest.trim_end_matches('\\');
    let comps = if rest.is_empty() {
        Vec::new()
    } else {
        rest.split('\\').map(str::to_string).collect()
    };
    Some((prefix, comps))
}

/// A path component Windows would normalize, reinterpret or treat as a device.
fn bad_component(c: &str) -> bool {
    if c.is_empty() || c == "." || c == ".." || c.ends_with('.') || c.ends_with(' ') {
        return true;
    }
    if c.chars()
        .any(|ch| matches!(ch, '<' | '>' | '"' | '|' | '?' | '*'))
    {
        return true;
    }
    let stem = c
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end()
        .to_ascii_uppercase();
    ["CON", "PRN", "AUX", "NUL"].contains(&stem.as_str())
        || ["COM", "LPT"].iter().any(|p| {
            stem.strip_prefix(p)
                .is_some_and(|n| n.len() == 1 && matches!(n.as_bytes()[0], b'1'..=b'9'))
        })
}

fn join_components(base: &str, parts: &[String]) -> String {
    let mut out = base.trim_end_matches('\\').to_string();
    for part in parts {
        out.push('\\');
        out.push_str(part);
    }
    if parts.is_empty() && !is_unc(base) {
        out.push('\\');
    }
    out
}

/// Canonical form of a path that does not exist (yet). With `resolve_ancestors`, the nearest
/// existing local ancestor is replaced by its real path; network roots keep their spelling (no
/// I/O walk over a share).
fn canonical_missing(p: &str, resolve_ancestors: bool) -> Result<String, RootError> {
    let Some((prefix, comps)) = split_root(p) else {
        return Ok(normalize_display(p));
    };
    if comps.iter().any(|c| bad_component(c)) {
        return Err(RootError::Invalid);
    }
    if resolve_ancestors {
        for keep in (0..comps.len()).rev() {
            let ancestor = join_components(&prefix, &comps[..keep]);
            if let Some(real) = final_dir_path(&ancestor) {
                return Ok(normalize_display(&join_components(
                    &normalize_display(&real),
                    &comps[keep..],
                )));
            }
        }
    }
    Ok(normalize_display(&join_components(&prefix, &comps)))
}

/// Folder ids read from an open handle.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FolderIds {
    volume_serial: String,
    file_id: String,
    fs_name: String,
}

/// Identity of a canonical root (SP-7.1). A local root must exist. A UNC root is opened within a
/// network budget to record the volume serial and folder id; when that is impossible (offline,
/// share missing, no answer in time) the identity is recorded as UNKNOWN (no ids), never as a
/// verified one - readiness reports it not ready (SP-8).
pub fn identity(canonical: &str) -> Result<RootIdentity, RootError> {
    if is_unc(canonical) {
        return identity_unc_with(canonical, open_ids);
    }
    let ids = open_ids(canonical)?;
    Ok(RootIdentity::Local {
        volume_serial: ids.volume_serial,
        file_id: ids.file_id,
        fs_name: ids.fs_name,
        drive: drive_kind(canonical),
    })
}

/// UNC identity with the folder opener injected (tests drive the openable and unopenable paths
/// without a share).
fn identity_unc_with(
    canonical: &str,
    opener: fn(&str) -> Result<FolderIds, RootError>,
) -> Result<RootIdentity, RootError> {
    let prefix = unc_prefix(canonical);
    let owned = canonical.to_string();
    match run_bounded(UNC_STEP_BUDGET, move || opener(&owned)) {
        Some(Ok(ids)) => Ok(RootIdentity::Network {
            unc_prefix: prefix,
            volume_serial: Some(ids.volume_serial),
            file_id: Some(ids.file_id),
        }),
        Some(Err(RootError::Invalid)) => Err(RootError::Invalid),
        Some(Err(_)) | None => Ok(RootIdentity::Network {
            unc_prefix: prefix,
            volume_serial: None,
            file_id: None,
        }),
    }
}

/// True when `a` equals `b` or one is inside the other, component-wise, case-insensitive
/// (ordinal-style folding), after canonicalization (SP-1.5, RQ-2).
pub fn overlap(a: &str, b: &str) -> bool {
    prefix_of(a, b) || prefix_of(b, a)
}

fn prefix_of(a: &str, b: &str) -> bool {
    let pa: Vec<String> = a
        .split('\\')
        .filter(|c| !c.is_empty())
        .map(|c| c.to_lowercase())
        .collect();
    let pb: Vec<String> = b
        .split('\\')
        .filter(|c| !c.is_empty())
        .map(|c| c.to_lowercase())
        .collect();
    pa.len() <= pb.len() && pa.iter().zip(&pb).all(|(x, y)| x == y)
}

pub fn is_unc(path: &str) -> bool {
    path.starts_with(r"\\")
}

/// The lowercase `\\host\share` prefix of a UNC path (SP-7.1).
pub fn unc_prefix(path: &str) -> String {
    let rest = path.trim_start_matches('\\');
    let mut parts = rest.split('\\').filter(|p| !p.is_empty());
    match (parts.next(), parts.next()) {
        (Some(host), Some(share)) => {
            format!(r"\\{}\{}", host.to_lowercase(), share.to_lowercase())
        }
        _ => path.to_lowercase(),
    }
}

fn normalize_display(path: &str) -> String {
    let mut s = path.to_string();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        s = format!(r"\\{rest}");
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        s = rest.to_string();
    }
    let b = s.as_bytes();
    if b.len() >= 3 && b[0].is_ascii_lowercase() && b[1] == b':' {
        s = format!("{}{}", b[0].to_ascii_uppercase() as char, &s[1..]);
    }
    trim_trailing_sep(&s)
}

/// `X:\` keeps its separator, anything longer loses trailing ones.
fn trim_trailing_sep(s: &str) -> String {
    let mut s = s.to_string();
    while s.len() > 3 && s.ends_with('\\') {
        s.pop();
    }
    s
}

// ── Drive kind (SP-3.1) ──

/// Drive class of the VOLUME a path lives on: the volume root (`GetVolumePathNameW`, which is a
/// mount folder for a volume mounted into a folder) is asked for its `GetDriveTypeW`. Only
/// `DRIVE_FIXED` on a drive-letter root is `Fixed`; anything else, or an unanswered question, is
/// `Removable` so the
/// Recycle Bin policy fails closed. Both lookups are injected for tests.
pub(crate) fn drive_kind_with(
    canonical: &str,
    volume_root_of: impl Fn(&str) -> Option<String>,
    drive_type_of: impl Fn(&str) -> u32,
) -> DriveKind {
    const DRIVE_FIXED: u32 = 3;
    match volume_root_of(canonical) {
        // Only a drive-letter volume root (`X:\`) can have a reliable Recycle Bin; a volume that
        // is merely mounted into a folder never qualifies, whatever its drive type.
        Some(root) if is_drive_letter_root(&root) && drive_type_of(&root) == DRIVE_FIXED => {
            DriveKind::Fixed
        }
        _ => DriveKind::Removable,
    }
}

fn is_drive_letter_root(root: &str) -> bool {
    let b = root.as_bytes();
    b.len() >= 2
        && b[0].is_ascii_alphabetic()
        && b[1] == b':'
        && root[2..].chars().all(|c| c == '\\' || c == '/')
}

#[cfg(windows)]
fn drive_kind(canonical: &str) -> DriveKind {
    drive_kind_with(canonical, volume_root_of, drive_type_of)
}

#[cfg(not(windows))]
fn drive_kind(_canonical: &str) -> DriveKind {
    DriveKind::Removable
}

#[cfg(windows)]
fn volume_root_of(path: &str) -> Option<String> {
    use windows_sys::Win32::Storage::FileSystem::GetVolumePathNameW;
    let wide: Vec<u16> = path.encode_utf16().chain([0]).collect();
    let mut buf = vec![0u16; 1024];
    // SAFETY: NUL-terminated input; `buf` is writable for `buf.len()` u16s.
    let ok = unsafe { GetVolumePathNameW(wide.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) };
    if ok == 0 {
        return None;
    }
    let len = buf.iter().position(|c| *c == 0)?;
    Some(String::from_utf16_lossy(&buf[..len]))
}

#[cfg(windows)]
fn drive_type_of(root: &str) -> u32 {
    use windows_sys::Win32::Storage::FileSystem::GetDriveTypeW;
    let mut wide: Vec<u16> = root.encode_utf16().collect();
    if wide.last() != Some(&(b'\\' as u16)) {
        wide.push(b'\\' as u16);
    }
    wide.push(0);
    // SAFETY: NUL-terminated root path.
    unsafe { GetDriveTypeW(wide.as_ptr()) }
}

// ── Windows implementation ──

#[cfg(windows)]
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
#[cfg(windows)]
const DRIVE_REMOTE: u32 = 4;

#[cfg(windows)]
fn open_dir_following(path: &str) -> std::io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .access_mode(0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
}

/// Existing directory → final path with junctions/symlinks resolved; `None` on any failure.
#[cfg(windows)]
fn final_dir_path(path: &str) -> Option<String> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::GetFinalPathNameByHandleW;
    let handle = open_dir_following(path).ok()?;
    let mut buf = vec![0u16; 1024];
    let n = loop {
        // SAFETY: valid handle; `buf` is writable for `buf.len()` u16s.
        let n = unsafe {
            GetFinalPathNameByHandleW(
                handle.as_raw_handle() as _,
                buf.as_mut_ptr(),
                buf.len() as u32,
                0,
            )
        } as usize;
        if n == 0 {
            return None;
        }
        if n < buf.len() {
            break n;
        }
        if n > 32 * 1024 {
            return None;
        }
        buf.resize(n + 1, 0);
    };
    Some(String::from_utf16_lossy(&buf[..n]))
}

/// A mapped network drive (`X:\` with `GetDriveTypeW == DRIVE_REMOTE`) is rewritten to its UNC
/// form so a profile survives the mapping and cannot alias the same share as two roots. The
/// name lookup talks to the network, so it is bounded; no answer keeps the drive-letter form.
#[cfg(windows)]
fn resolve_mapped_drive(path: &str) -> String {
    let b = path.as_bytes();
    if !(b.len() >= 2 && b[1] == b':') {
        return path.to_string();
    }
    let drive_root = format!("{}\\", path[..2].to_ascii_uppercase());
    if drive_type_of(&drive_root) != DRIVE_REMOTE {
        return path.to_string();
    }
    let owned = path.to_string();
    run_bounded(UNC_STEP_BUDGET, move || universal_name(&owned))
        .flatten()
        .unwrap_or_else(|| path.to_string())
}

#[cfg(windows)]
fn universal_name(path: &str) -> Option<String> {
    use windows_sys::Win32::NetworkManagement::WNet::{
        UNIVERSAL_NAME_INFO_LEVEL, WNetGetUniversalNameW,
    };
    let wide: Vec<u16> = path.encode_utf16().chain([0]).collect();
    let mut buf = vec![0u16; 1024];
    let mut size = (buf.len() * 2) as u32;
    // SAFETY: `wide` is NUL-terminated; `buf` is a writable out buffer of `size` bytes.
    let ok = unsafe {
        WNetGetUniversalNameW(
            wide.as_ptr(),
            UNIVERSAL_NAME_INFO_LEVEL,
            buf.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if ok != 0 {
        return None;
    }
    // SAFETY: the API wrote a UNIVERSAL_NAME_INFOW at the start of `buf`.
    let info = unsafe {
        &*buf
            .as_ptr()
            .cast::<windows_sys::Win32::NetworkManagement::WNet::UNIVERSAL_NAME_INFOW>()
    };
    if info.lpUniversalName.is_null() {
        return None;
    }
    // SAFETY: the pointer refers to a NUL-terminated string inside `buf`.
    let mut len = 0usize;
    unsafe {
        while *info.lpUniversalName.add(len) != 0 {
            len += 1;
        }
    }
    // SAFETY: `len` UTF-16 units follow the pointer, inside `buf`.
    let unc = unsafe { std::slice::from_raw_parts(info.lpUniversalName, len) };
    Some(String::from_utf16_lossy(unc))
}

/// Open the folder (following junctions) and read the volume serial, folder id and file system
/// name from the handle. `NotFound` = it is not there; `Invalid` = it is not a folder.
#[cfg(windows)]
fn open_ids(path: &str) -> Result<FolderIds, RootError> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_ID_INFO, FileIdInfo, GetFileInformationByHandle,
        GetFileInformationByHandleEx, GetVolumeInformationByHandleW,
    };
    let handle = open_dir_following(path).map_err(io_root_error)?;
    let meta = handle.metadata().map_err(io_root_error)?;
    if !meta.is_dir() {
        return Err(RootError::Invalid);
    }
    let raw = handle.as_raw_handle() as _;
    let mut fs_name = [0u16; 64];
    let mut serial: u32 = 0;
    // SAFETY: valid handle; both out buffers are writable for their size.
    let ok = unsafe {
        GetVolumeInformationByHandleW(
            raw,
            std::ptr::null_mut(),
            0,
            &mut serial,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            fs_name.as_mut_ptr(),
            fs_name.len() as u32,
        )
    };
    if ok == 0 {
        return Err(io_root_error(std::io::Error::last_os_error()));
    }
    let len = fs_name.iter().position(|c| *c == 0).unwrap_or(0);
    let fs_name = String::from_utf16_lossy(&fs_name[..len]);
    // 128-bit file id (NTFS/ReFS); on volumes without it fall back to the 64-bit index.
    let mut id_info: FILE_ID_INFO = unsafe { std::mem::zeroed() };
    // SAFETY: valid handle; `id_info` is a writable out buffer of the given size.
    let file_id = if unsafe {
        GetFileInformationByHandleEx(
            raw,
            FileIdInfo,
            (&mut id_info as *mut FILE_ID_INFO).cast(),
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        )
    } != 0
    {
        hex(&id_info.FileId.Identifier)
    } else {
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: valid handle; zeroed POD out-parameter.
        if unsafe { GetFileInformationByHandle(raw, &mut info) } == 0 {
            return Err(io_root_error(std::io::Error::last_os_error()));
        }
        let index = (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow);
        format!("{index:016x}{index:016x}")
    };
    Ok(FolderIds {
        volume_serial: format!("{serial:08x}"),
        file_id,
        fs_name,
    })
}

#[cfg(windows)]
fn io_root_error(e: std::io::Error) -> RootError {
    match e.kind() {
        std::io::ErrorKind::NotFound => RootError::NotFound,
        _ => RootError::Io,
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

// ── Non-Windows shapes (compile-only; Ferrum automation is Windows-only) ──

#[cfg(not(windows))]
fn final_dir_path(_path: &str) -> Option<String> {
    None
}

#[cfg(not(windows))]
fn resolve_mapped_drive(path: &str) -> String {
    path.to_string()
}

#[cfg(not(windows))]
fn open_ids(_path: &str) -> Result<FolderIds, RootError> {
    Err(RootError::Io)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn junction(link: &std::path::Path, target: &std::path::Path) -> bool {
        std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .is_ok_and(|o| o.status.success())
    }

    #[test]
    fn refused_string_forms_map_to_invalid() {
        for bad in [
            r"\\?\C:\x",
            r"\\.\C:\x",
            r"\??\C:\x",
            r"\\.\pipe\x",
            r"\\nas\pipe\x",
            r"\\nas\IPC$\x",
            r"\\nas\mailslot\x",
            "Photos",
            "C:relative",
            r"C:\x:stream",
        ] {
            assert_eq!(canonical(bad), Err(RootError::Invalid), "{bad:?}");
        }
    }

    #[test]
    fn accepted_forms_normalize_drive_case_and_separators() {
        assert_eq!(canonical("c:/temp/a"), Ok("C:\\temp\\a".into()));
        assert_eq!(canonical(r"D:\Photos\"), Ok(r"D:\Photos".into()));
        assert_eq!(canonical(r"\\Nas\Share"), Ok(r"\\Nas\Share".into()));
    }

    #[test]
    fn existing_directory_resolves_to_its_final_path() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        std::fs::create_dir(&real).unwrap();
        // Lowercase spelling of the same folder must canonicalize to the real case.
        let lower = real.to_string_lossy().to_lowercase();
        assert_eq!(canonical(&lower).unwrap(), normalize_case(&real));
    }

    /// SP-1.5: a junction alias of a root canonicalizes to the same folder, so "same folder after
    /// resolving junctions" is detected.
    #[cfg(windows)]
    #[test]
    fn junction_alias_canonicalizes_to_the_target() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = tmp.path().join("via-junction");
        assert!(junction(&link, &real), "junction created");
        assert_eq!(
            canonical(&link.to_string_lossy()).unwrap(),
            canonical(&real.to_string_lossy()).unwrap()
        );
    }

    /// SP-7.1: an existing local root yields a volume serial and a file id; a UNC root yields the
    /// lowercase `\\host\share` prefix.
    #[test]
    fn identity_records_volume_serial_and_file_id() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("r")).unwrap();
        let c = canonical(&tmp.path().join("r").to_string_lossy()).unwrap();
        match identity(&c).unwrap() {
            RootIdentity::Local {
                volume_serial,
                file_id,
                fs_name,
                ..
            } => {
                assert_eq!(volume_serial.len(), 8);
                assert_eq!(file_id.len(), 32);
                assert!(!fs_name.is_empty());
            }
            other => panic!("local identity expected, got {other:?}"),
        }
        match identity(r"\\NAS\Share\Deep\Path").unwrap() {
            RootIdentity::Network { unc_prefix, .. } => {
                assert_eq!(unc_prefix, r"\\nas\share");
            }
            other => panic!("network identity expected, got {other:?}"),
        }
    }

    /// SP-7.2/SP-7.3: a folder deleted and re-created at the same path must produce a different
    /// identity, so a stored profile is never silently re-pointed at a new folder.
    #[test]
    fn a_recreated_folder_gets_a_new_identity() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("r");
        std::fs::create_dir(&dir).unwrap();
        let c = canonical(&dir.to_string_lossy()).unwrap();
        let before = identity(&c).unwrap();
        std::fs::remove_dir(&dir).unwrap();
        std::fs::create_dir(&dir).unwrap();
        let after = identity(&c).unwrap();
        assert_ne!(before, after, "re-created folder must change identity");
    }

    #[test]
    fn overlap_detects_equal_nested_and_case_folded_but_not_siblings() {
        assert!(overlap(r"C:\a\b", r"C:\a\b"));
        assert!(overlap(r"C:\a\b", r"c:\A\b\deep"));
        assert!(overlap(r"C:\a\b", r"C:\a\b\deep"));
        assert!(overlap(r"C:\a\b\deep", r"C:\a\b"));
        assert!(!overlap(r"C:\a\b", r"C:\a\bb"));
        assert!(!overlap(r"C:\a\b", r"C:\a2\b"));
    }

    /// SP-8: a probe that cannot answer within the budget reports `checking`, never empty/missing.
    #[test]
    fn a_slow_root_reports_checking() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("r")).unwrap();
        let c = canonical(&tmp.path().join("r").to_string_lossy()).unwrap();
        // The real answer needs a few ms; an impossible budget forces the checking path.
        assert_eq!(
            probe(&c, Duration::from_nanos(1)),
            RootProbeOutcome::Checking
        );
        assert_eq!(
            probe(&c, Duration::from_secs(5)),
            RootProbeOutcome::Ready {
                canonical: c.clone(),
                identity: identity(&c).unwrap(),
            }
        );
    }

    #[test]
    fn a_missing_root_reports_missing_not_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let gone = tmp.path().join("gone");
        assert_eq!(
            probe(&gone.to_string_lossy(), Duration::from_secs(5)),
            RootProbeOutcome::Missing
        );
    }

    /// Display normalization is shared by the canonical fallback for missing folders.
    #[test]
    fn normalization_is_stable_for_missing_folders() {
        assert_eq!(normalize_display(r"\\?\C:\a\b\"), r"C:\a\b");
        assert_eq!(normalize_display(r"\\?\UNC\nas\share\a"), r"\\nas\share\a");
        assert_eq!(normalize_display(r"d:\x"), r"D:\x");
        assert_eq!(normalize_display(r"C:\"), r"C:\");
    }

    fn normalize_case(p: &std::path::Path) -> String {
        // The final path keeps the on-disk case; compare against a probe of the exact spelling.
        canonical(&p.to_string_lossy()).unwrap()
    }

    // ── Rework 1 ──

    /// P2-1: a UNC root that cannot be opened is recorded with UNKNOWN identity (no ids), never as
    /// a verified one; the answer comes within the network step budget.
    #[test]
    fn p2_1_an_unopenable_unc_root_has_an_unknown_identity() {
        let started = std::time::Instant::now();
        let id = identity(r"\\localhost\no-such-share-ferrum-p03\x").unwrap();
        assert_eq!(
            id,
            RootIdentity::Network {
                unc_prefix: r"\\localhost\no-such-share-ferrum-p03".into(),
                volume_serial: None,
                file_id: None,
            }
        );
        assert!(started.elapsed() < Duration::from_secs(3));
        // The readiness probe still answers "ready with unknown identity"; the command layer maps
        // that to not ready (see commands tests).
        assert!(matches!(
            probe(r"\\localhost\no-such-share-ferrum-p03\x", READINESS_BUDGET),
            RootProbeOutcome::Ready {
                identity: RootIdentity::Network {
                    volume_serial: None,
                    ..
                },
                ..
            }
        ));
    }

    /// P2-1: a UNC root that CAN be opened records the volume serial and folder id. Uses the
    /// administrative share of the local drive; skipped (with a note) where that share is not
    /// reachable for the current user.
    #[cfg(windows)]
    #[test]
    fn p2_1_an_openable_unc_root_records_its_ids() {
        let tmp = tempfile::tempdir().unwrap();
        let local = tmp.path().to_string_lossy().into_owned();
        let b = local.as_bytes();
        if !(b.len() > 3 && b[1] == b':') {
            eprintln!("SKIP: the temp dir is not on a drive-letter path");
            return;
        }
        let unc = format!(r"\\localhost\{}${}", b[0] as char, &local[2..]);
        let Ok(c) = canonical(&unc) else {
            eprintln!("SKIP: the administrative share path was refused by canonical()");
            return;
        };
        match identity(&c) {
            Ok(RootIdentity::Network {
                volume_serial: Some(serial),
                file_id: Some(file_id),
                ..
            }) => {
                assert_eq!(serial.len(), 8);
                assert_eq!(file_id.len(), 32);
            }
            Ok(RootIdentity::Network {
                volume_serial: None,
                ..
            }) => eprintln!("SKIP: the administrative share is not reachable for this user"),
            other => panic!("network identity expected, got {other:?}"),
        }
    }

    /// P2-1 (deterministic, no share needed): the openable path records the ids, an unopenable
    /// or slow one records unknown, a non-folder is invalid.
    #[test]
    fn p2_1_unc_identity_paths_via_the_injected_opener() {
        fn ok(_: &str) -> Result<FolderIds, RootError> {
            Ok(FolderIds {
                volume_serial: "0000000a".into(),
                file_id: "b".repeat(32),
                fs_name: "NTFS".into(),
            })
        }
        fn gone(_: &str) -> Result<FolderIds, RootError> {
            Err(RootError::NotFound)
        }
        fn not_a_folder(_: &str) -> Result<FolderIds, RootError> {
            Err(RootError::Invalid)
        }
        fn hangs(_: &str) -> Result<FolderIds, RootError> {
            std::thread::sleep(Duration::from_millis(1500));
            Err(RootError::Io)
        }
        let unc = r"\\Nas\Share\x";
        assert_eq!(
            identity_unc_with(unc, ok).unwrap(),
            RootIdentity::Network {
                unc_prefix: r"\\nas\share".into(),
                volume_serial: Some("0000000a".into()),
                file_id: Some("b".repeat(32)),
            }
        );
        let unknown = RootIdentity::Network {
            unc_prefix: r"\\nas\share".into(),
            volume_serial: None,
            file_id: None,
        };
        assert_eq!(identity_unc_with(unc, gone).unwrap(), unknown);
        assert_eq!(identity_unc_with(unc, hangs).unwrap(), unknown);
        assert_eq!(
            identity_unc_with(unc, not_a_folder),
            Err(RootError::Invalid)
        );
    }

    /// P2-2: `.`/`..`, empty, reserved and trailing-dot/space components of a path that does not
    /// exist are refused; so are control and bidi characters anywhere.
    #[test]
    fn p2_2_unnormalized_components_of_a_missing_path_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().to_string_lossy().into_owned();
        for bad in [
            format!(r"{base}\a\..\b"),
            format!(r"{base}\.\b"),
            format!(r"{base}\a\\b"),
            format!(r"{base}\a.\b"),
            format!(r"{base}\a \b"),
            format!(r"{base}\NUL"),
            format!(r"{base}\com1.txt"),
            format!(r"{base}\LPT9"),
            format!(r"{base}\a?b"),
            format!("{base}\\a\u{202E}b"),
            format!("{base}\\a\u{200E}b"),
            format!("{base}\\a\tb"),
        ] {
            assert_eq!(canonical(&bad), Err(RootError::Invalid), "{bad:?}");
        }
        assert!(canonical(&format!(r"{base}\fine\sub")).is_ok());
        assert!(
            canonical(&format!(r"{base}\COM10")).is_ok(),
            "COM10 is not a device name"
        );
    }

    /// P2-2: the nearest existing ancestor is resolved to its real path (case and junctions), the
    /// rest is appended.
    #[test]
    fn p2_2_a_missing_path_is_anchored_at_its_real_existing_ancestor() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("Real");
        std::fs::create_dir(&real).unwrap();
        let lower = real.to_string_lossy().to_lowercase();
        let got = canonical(&format!(r"{lower}\new\deep")).unwrap();
        let want = format!(r"{}\new\deep", canonical(&real.to_string_lossy()).unwrap());
        assert_eq!(got, want);
    }

    #[cfg(windows)]
    #[test]
    fn p2_2_a_junction_on_the_parent_resolves_to_its_target() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = tmp.path().join("alias");
        assert!(junction(&link, &real), "junction created");
        let via = canonical(&format!(r"{}\child", link.display())).unwrap();
        let direct = canonical(&format!(r"{}\child", real.display())).unwrap();
        assert_eq!(via, direct);
        assert!(overlap(&via, &canonical(&real.to_string_lossy()).unwrap()));
    }

    /// P2-5: only the VOLUME's own drive type counts. A volume mounted into a folder is judged by
    /// its mount root, not by the drive letter in front of it; unknown answers fail closed.
    #[test]
    fn p2_5_drive_kind_follows_the_volume_not_the_drive_letter() {
        const FIXED: u32 = 3;
        const REMOVABLE: u32 = 2;
        const REMOTE: u32 = 4;
        let volume_of = |path: &str| -> Option<String> {
            if path.to_lowercase().starts_with(r"c:\mount\usb") {
                Some(r"C:\Mount\USB\".to_string())
            } else if path.starts_with(r"C:\") {
                Some(r"C:\".to_string())
            } else {
                None
            }
        };
        let types = |root: &str| match root {
            r"C:\" => FIXED,
            r"C:\Mount\USB\" => REMOVABLE,
            _ => REMOTE,
        };
        assert_eq!(
            drive_kind_with(r"C:\Data\x", volume_of, types),
            DriveKind::Fixed
        );
        // Same drive letter C:, but a removable volume is mounted at C:\Mount\USB.
        assert_eq!(
            drive_kind_with(r"C:\Mount\USB\backup", volume_of, types),
            DriveKind::Removable
        );
        // A FIXED volume that is only mounted into a folder (volume root is not `X:\`) has no
        // reliable Recycle Bin, so it is never eligible, whatever its drive type.
        assert_eq!(
            drive_kind_with(r"C:\Mount\USB\backup", volume_of, |_| FIXED),
            DriveKind::Removable
        );
        // No volume answer, network and unknown types fail closed.
        assert_eq!(
            drive_kind_with(r"Z:\x", volume_of, types),
            DriveKind::Removable
        );
        assert_eq!(
            drive_kind_with(r"C:\Data", volume_of, |_| REMOTE),
            DriveKind::Removable
        );
        assert_eq!(
            drive_kind_with(r"C:\Data", volume_of, |_| 0),
            DriveKind::Removable
        );
    }

    /// The real lookups agree with the seam for the system drive.
    #[cfg(windows)]
    #[test]
    fn p2_5_the_system_volume_is_a_fixed_volume() {
        let tmp = tempfile::tempdir().unwrap();
        let c = canonical(&tmp.path().to_string_lossy()).unwrap();
        let kind = drive_kind(&c);
        // The temp dir lives on a fixed disk on any machine this suite runs on.
        assert_eq!(kind, DriveKind::Fixed, "{c}");
        assert!(volume_root_of(&c).is_some());
    }

    /// P3 item 5 (e): a bounded run times out, counts the abandoned worker until it ends, refuses
    /// new work once too many workers are stuck, and recovers when they finish.
    #[test]
    fn bounded_runs_time_out_and_cap_stuck_workers() {
        static STUCK: AtomicUsize = AtomicUsize::new(0);
        let gate = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        let hang = |gate: Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>| {
            move || {
                let (lock, cv) = &*gate;
                let mut open = lock.lock().unwrap();
                while !*open {
                    open = cv.wait(open).unwrap();
                }
            }
        };
        // A fast job answers.
        assert_eq!(
            run_bounded_in(&STUCK, 2, Duration::from_secs(5), || 7),
            Some(7)
        );
        // Two jobs that hang past their budget are abandoned and counted.
        for _ in 0..2 {
            assert_eq!(
                run_bounded_in(&STUCK, 2, Duration::from_millis(30), hang(gate.clone())),
                None
            );
        }
        assert_eq!(STUCK.load(Ordering::SeqCst), 2);
        // At the cap, new work is refused at once (no thread, no wait).
        let started = std::time::Instant::now();
        assert_eq!(
            run_bounded_in(&STUCK, 2, Duration::from_secs(5), || 1),
            None
        );
        assert!(started.elapsed() < Duration::from_millis(500));
        // The hung workers finish: the counter drains and work is accepted again.
        {
            let (lock, cv) = &*gate;
            *lock.lock().unwrap() = true;
            cv.notify_all();
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while STUCK.load(Ordering::SeqCst) != 0 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(STUCK.load(Ordering::SeqCst), 0);
        assert_eq!(
            run_bounded_in(&STUCK, 2, Duration::from_secs(5), || 2),
            Some(2)
        );
    }
}
