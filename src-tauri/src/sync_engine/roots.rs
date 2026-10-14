//! Root canonicalization, identity and readiness probes (Automation design §5.8).
//!
//! - `canonical` applies the `path_guard::check_str` string rules (SP-1.4), resolves a mapped
//!   network drive to its UNC form (`WNetGetUniversalNameW`) and, for an existing directory,
//!   resolves junctions/symlinks via `GetFinalPathNameByHandleW`, strips the `\\?\` prefix and
//!   uppercases the drive letter. The result is the display path stored in a profile.
//! - `identity` records what the path pointed at when the profile was saved (SP-7.1): volume
//!   serial + folder file id for a local root, the lowercase `\\host\share` prefix for UNC.
//! - `probe` runs both within a per-root time budget (SP-8): a slower root reports `checking`
//!   instead of blocking the readiness answer.
//!
//! Tauri-free by design (the runner links this module).

use std::sync::Arc;
use std::time::Duration;

use crate::automation::model::{DriveKind, RootIdentity};

/// Why a root could not be probed. Never carries a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootError {
    /// The string form is refused (SP-1.4: device/NT namespace, ADS, pipe/mailslot/IPC$,
    /// relative).
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

/// Probe one root within `budget`. A slower answer becomes [`RootProbeOutcome::Checking`]; the
/// worker thread is detached and never treats the root as missing or empty (SP-8: the caller
/// must never block on an unreachable root).
pub fn probe(path: &str, budget: Duration) -> RootProbeOutcome {
    probe_with(Arc::new(FsProbe), path, budget)
}

/// [`probe`] with an injected prober (tests, mapped-drive simulation). The prober is owned by
/// the detached worker so the caller is never held hostage by a slow volume.
pub fn probe_with(prober: Arc<dyn RootProbe>, path: &str, budget: Duration) -> RootProbeOutcome {
    let (tx, rx) = std::sync::mpsc::channel();
    let path = path.to_string();
    std::thread::spawn(move || {
        let _ = tx.send(probe_once(&*prober, &path));
    });
    rx.recv_timeout(budget)
        .unwrap_or(RootProbeOutcome::Checking)
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

/// Canonical display path of a root (see module docs). Never requires the folder to exist: a
/// missing folder keeps its normalized spelling so nesting can still be judged, and the identity
/// probe reports it as missing.
pub fn canonical(path: &str) -> Result<String, RootError> {
    let p = crate::path_guard::check_str(path).map_err(|_| RootError::Invalid)?;
    let p = resolve_mapped_drive(&p);
    Ok(match final_dir_path(&p) {
        Some(final_path) => normalize_display(&final_path),
        None => normalize_display(&p),
    })
}

/// Identity of a canonical root (SP-7.1). Requires the folder to exist.
pub fn identity(canonical: &str) -> Result<RootIdentity, RootError> {
    if is_unc(canonical) {
        return Ok(RootIdentity::Network {
            unc_prefix: unc_prefix(canonical),
        });
    }
    identity_local(canonical)
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
/// form so a profile survives the mapping and cannot alias the same share as two roots.
#[cfg(windows)]
fn resolve_mapped_drive(path: &str) -> String {
    use windows_sys::Win32::NetworkManagement::WNet::{
        UNIVERSAL_NAME_INFO_LEVEL, WNetGetUniversalNameW,
    };
    let b = path.as_bytes();
    if !(b.len() >= 2 && b[1] == b':') {
        return path.to_string();
    }
    let drive_root: Vec<u16> = format!("{}\\", path[..2].to_ascii_uppercase())
        .encode_utf16()
        .chain([0])
        .collect();
    // SAFETY: NUL-terminated root path.
    if unsafe { windows_sys::Win32::Storage::FileSystem::GetDriveTypeW(drive_root.as_ptr()) }
        != DRIVE_REMOTE
    {
        return path.to_string();
    }
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
        return path.to_string();
    }
    // SAFETY: the API wrote a UNIVERSAL_NAME_INFOW at the start of `buf`.
    let info = unsafe {
        &*buf
            .as_ptr()
            .cast::<windows_sys::Win32::NetworkManagement::WNet::UNIVERSAL_NAME_INFOW>()
    };
    if info.lpUniversalName.is_null() {
        return path.to_string();
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
    String::from_utf16_lossy(unc)
}

#[cfg(windows)]
fn identity_local(canonical: &str) -> Result<RootIdentity, RootError> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_ID_INFO, FileIdInfo, GetFileInformationByHandle,
        GetFileInformationByHandleEx, GetVolumeInformationByHandleW,
    };
    let handle = open_dir_following(canonical).map_err(io_root_error)?;
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
    let drive = drive_kind(canonical);
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
    Ok(RootIdentity::Local {
        volume_serial: format!("{serial:08x}"),
        file_id,
        fs_name,
        drive,
    })
}

/// Drive kind from `GetDriveTypeW`. Anything that is not clearly a fixed or removable disk is
/// reported as removable, so the Recycle Bin policy fails closed (SP-3.1).
#[cfg(windows)]
fn drive_kind(canonical: &str) -> DriveKind {
    use windows_sys::Win32::Storage::FileSystem::GetDriveTypeW;
    let b = canonical.as_bytes();
    if !(b.len() >= 2 && b[1] == b':') {
        return DriveKind::Removable;
    }
    let root: Vec<u16> = format!("{}\\", canonical[..2].to_ascii_uppercase())
        .encode_utf16()
        .chain([0])
        .collect();
    // SAFETY: NUL-terminated root path. DRIVE_FIXED = 3, DRIVE_REMOVABLE = 2.
    match unsafe { GetDriveTypeW(root.as_ptr()) } {
        3 => DriveKind::Fixed,
        _ => DriveKind::Removable,
    }
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
fn identity_local(_canonical: &str) -> Result<RootIdentity, RootError> {
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
            RootIdentity::Network { unc_prefix } => {
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
}
