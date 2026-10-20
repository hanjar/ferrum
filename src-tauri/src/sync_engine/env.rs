//! Injection points of the profile engine (design §13.1): everything that touches the outside
//! world is a trait with a production implementation here, so tests drive sharing violations,
//! a vanished root, a second volume, a Recycle Bin and the clock without real devices.

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::archive_extract::publish::{self, DirHandle, MoveError};
use crate::copy_engine::{self, ConflictPolicy, CopyOptions, ReplaceGuard};
use crate::domain::FsError;
use crate::ops::CancelToken;
use crate::sync_engine::errors::{Classified, classify_fs};
use crate::sync_engine::roots::{FsProbe, RootProbe};
use crate::sync_incremental::{ChangesSource, ServiceChanges};

// ── Copying ──

/// What happened to the sources of one copy call.
#[derive(Debug, Default)]
pub struct CopyReport {
    /// Sources that were fully copied.
    pub done: HashSet<String>,
    /// Sources that failed, with their classification. The key is the source path, or the
    /// destination folder when the whole call failed before any source was looked at.
    pub failed: Vec<(String, Classified)>,
    /// Stopped by the cancel token.
    pub cancelled: bool,
}

/// Copies files into one destination folder under their own names, each published through a
/// staging file (a file in progress is complete or absent).
pub trait Copier: Send + Sync {
    /// `overwrite` replaces an existing destination file; otherwise an existing destination
    /// makes that source fail to copy (it is neither done nor failed: the caller treats it as
    /// "target appeared"). `durable` flushes each file to disk before it is published.
    ///
    /// FER-564: with `overwrite` and a `guard`, an existing destination is replaced only through
    /// the guard (which re-checks it right before the swap); without a guard the copy engine's
    /// own replace is used (only the tests of other copiers pass `None` with `overwrite`).
    fn copy(
        &self,
        sources: &[String],
        dest_dir: &str,
        overwrite: bool,
        durable: bool,
        cancel: &CancelToken,
        guard: Option<&dyn ReplaceGuard>,
    ) -> CopyReport;
}

/// Production copier: `copy_engine`.
pub struct EngineCopier;

impl Copier for EngineCopier {
    fn copy(
        &self,
        sources: &[String],
        dest_dir: &str,
        overwrite: bool,
        durable: bool,
        cancel: &CancelToken,
        guard: Option<&dyn ReplaceGuard>,
    ) -> CopyReport {
        let policy = if overwrite {
            ConflictPolicy::Overwrite
        } else {
            ConflictPolicy::Skip
        };
        let (threads, priority) = crate::copy_pool::defaults();
        let options = CopyOptions {
            threads,
            priority,
            preserve_timestamps: true,
            durable,
            ..CopyOptions::default()
        };
        let out = match guard {
            Some(guard) if overwrite => {
                copy_engine::run_copy_with_guard(sources, dest_dir, policy, options, cancel, guard)
            }
            _ => copy_engine::run_copy_with_options(
                sources,
                dest_dir,
                policy,
                options,
                cancel,
                &|_progress| {},
            ),
        };
        let mut report = CopyReport {
            cancelled: out.cancelled,
            ..CopyReport::default()
        };
        for (src, _dest) in out.completed {
            report.done.insert(src);
        }
        for failure in out.failures {
            let fs = FsError::new(failure.code, failure.message);
            report
                .failed
                .push((failure.source, classify_fs(&fs, false)));
        }
        report
    }
}

// ── Recycle Bin ──

/// Moves one file to the Recycle Bin. Only used after the drive gate passed; a failure never
/// turns into a permanent delete.
pub trait Trash: Send + Sync {
    fn trash_file(&self, path: &Path) -> Result<(), FsError>;

    /// Is `final_path` (what the OS reports for the very object that was sent to the bin) a
    /// place inside the Recycle Bin? The engine asks after every call; anything else means the
    /// shell deleted the file for good (`recycle_bypassed`).
    fn is_in_bin(&self, final_path: &str) -> bool;
}

pub struct SystemTrash;

impl Trash for SystemTrash {
    fn trash_file(&self, path: &Path) -> Result<(), FsError> {
        let report = crate::mutations::delete_to_trash(&[path.to_string_lossy().into_owned()]);
        match report.failed.into_iter().next() {
            None if report.deleted == 1 => Ok(()),
            None => Err(FsError::new(crate::domain::ErrorCode::Io, "not trashed")),
            Some(f) => Err(FsError::new(f.code, f.message)),
        }
    }

    fn is_in_bin(&self, final_path: &str) -> bool {
        final_path.to_lowercase().contains(r"\$recycle.bin\")
    }
}

// ── Recycle Bin configuration ──

/// What the volume's Recycle Bin settings say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinState {
    /// The bin is on with a non-zero size and "delete immediately" is off.
    Usable,
    /// "Don't move files to the Recycle Bin", a bin size of 0, or the no-recycle policy.
    Disabled,
    /// The settings could not be read: treated as unusable (fail closed, SP-3.1).
    Unknown,
}

/// Reads the per-volume Recycle Bin configuration (design §5.5, gate). Injectable for tests.
pub trait BinConfig: Send + Sync {
    /// `root` is a drive-letter volume root such as `D:\`.
    fn state(&self, root: &str) -> BinState;
}

/// Production: the shell's own settings under `HKCU\...\Explorer\BitBucket`.
pub struct RegistryBinConfig;

impl BinConfig for RegistryBinConfig {
    #[cfg(windows)]
    fn state(&self, root: &str) -> BinState {
        bin_state_from_registry(root)
    }

    #[cfg(not(windows))]
    fn state(&self, _root: &str) -> BinState {
        BinState::Unknown
    }
}

/// `\\?\Volume{GUID}\` of a drive-letter volume root -> `{GUID}`; `None` when Windows cannot tell.
#[cfg(windows)]
fn volume_guid_of(root: &str) -> Option<String> {
    use windows_sys::Win32::Storage::FileSystem::GetVolumeNameForVolumeMountPointW;

    let mut mount: Vec<u16> = root.encode_utf16().collect();
    if mount.last() != Some(&(b'\\' as u16)) {
        mount.push(b'\\' as u16);
    }
    mount.push(0);
    let mut buf = vec![0u16; 64];
    // SAFETY: NUL-terminated mount point; `buf` is writable for its length.
    let ok = unsafe {
        GetVolumeNameForVolumeMountPointW(mount.as_ptr(), buf.as_mut_ptr(), buf.len() as u32)
    };
    if ok == 0 {
        return None;
    }
    let len = buf.iter().position(|c| *c == 0).unwrap_or(0);
    let name = String::from_utf16_lossy(&buf[..len]);
    let (a, b) = (name.find('{')?, name.find('}')?);
    Some(name[a..=b].to_owned())
}

/// FER-576: the explicit Recycle Bin size limit of a volume in MB (`MaxCapacity` of its bin settings).
/// `None` = the volume has no explicit limit (the system default applies) or it could not be read.
#[cfg(windows)]
pub fn bin_max_capacity_mb(root: &str) -> Option<u32> {
    use windows_sys::Win32::System::Registry::HKEY_CURRENT_USER;
    let guid = volume_guid_of(root)?;
    let key =
        format!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\BitBucket\Volume\{guid}");
    reg_dword(HKEY_CURRENT_USER, &key, "MaxCapacity")
        .ok()
        .flatten()
}

#[cfg(not(windows))]
pub fn bin_max_capacity_mb(_root: &str) -> Option<u32> {
    None
}

#[cfg(windows)]
fn bin_state_from_registry(root: &str) -> BinState {
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};

    let Some(guid) = volume_guid_of(root) else {
        return BinState::Unknown;
    };
    let guid = guid.as_str();

    let explorer = r"Software\Microsoft\Windows\CurrentVersion\Explorer\BitBucket";
    let policy = r"Software\Microsoft\Windows\CurrentVersion\Policies\Explorer";
    let mut unknown = false;
    let mut read = |hive, key: &str, value: &str| match reg_dword(hive, key, value) {
        Ok(v) => v,
        Err(()) => {
            unknown = true;
            None
        }
    };
    let no_recycle = read(HKEY_CURRENT_USER, policy, "NoRecycleFiles") == Some(1)
        || read(HKEY_LOCAL_MACHINE, policy, "NoRecycleFiles") == Some(1);
    let volume_key = format!(r"{explorer}\Volume\{guid}");
    let vol_nuke = read(HKEY_CURRENT_USER, &volume_key, "NukeOnDelete");
    let vol_max = read(HKEY_CURRENT_USER, &volume_key, "MaxCapacity");
    let global_nuke = read(HKEY_CURRENT_USER, explorer, "NukeOnDelete");
    if no_recycle || vol_nuke == Some(1) || vol_max == Some(0) {
        return BinState::Disabled;
    }
    // The global switch applies where the volume does not say otherwise.
    if vol_nuke.is_none() && global_nuke == Some(1) {
        return BinState::Disabled;
    }
    if unknown {
        BinState::Unknown
    } else {
        BinState::Usable
    }
}

/// One DWORD value: `Ok(None)` = key or value is not there, `Err` = could not be read.
#[cfg(windows)]
fn reg_dword(
    hive: windows_sys::Win32::System::Registry::HKEY,
    key: &str,
    value: &str,
) -> Result<Option<u32>, ()> {
    use windows_sys::Win32::System::Registry::{
        HKEY, KEY_READ, REG_DWORD, RegCloseKey, RegOpenKeyExW, RegQueryValueExW,
    };
    let wide = |s: &str| -> Vec<u16> { s.encode_utf16().chain([0]).collect() };
    let mut h: HKEY = std::ptr::null_mut();
    // SAFETY: NUL-terminated key name; `h` is a writable out parameter.
    let rc = unsafe { RegOpenKeyExW(hive, wide(key).as_ptr(), 0, KEY_READ, &mut h) };
    const NOT_FOUND: u32 = 2;
    if rc == NOT_FOUND {
        return Ok(None);
    }
    if rc != 0 {
        return Err(());
    }
    let mut kind: u32 = 0;
    let mut data: u32 = 0;
    let mut size: u32 = 4;
    // SAFETY: valid open key; `data` is writable for `size` bytes.
    let rc = unsafe {
        RegQueryValueExW(
            h,
            wide(value).as_ptr(),
            std::ptr::null(),
            &mut kind,
            (&mut data as *mut u32).cast(),
            &mut size,
        )
    };
    // SAFETY: closing the key opened above.
    unsafe { RegCloseKey(h) };
    if rc == NOT_FOUND {
        return Ok(None);
    }
    if rc != 0 || kind != REG_DWORD {
        return Err(());
    }
    Ok(Some(data))
}

// ── Moving into the versioning tree ──

/// Renames a file by handle into another folder (never replacing), after `check` accepted the
/// size and last-write time read from the handle itself (`Ok(false)` = not moved, the file is not
/// what was expected). Injectable so a second volume can be simulated (`MoveError::CrossDevice`)
/// when only one exists.
pub trait Mover: Send + Sync {
    fn rename_into(
        &self,
        src_parent: &DirHandle,
        src_name: &std::ffi::OsStr,
        dst_parent: &DirHandle,
        dst_name: &std::ffi::OsStr,
        check: publish::FactsCheck<'_>,
    ) -> Result<bool, MoveError>;

    /// Must this file be moved by copy-then-delete instead of a rename? True for a cloud
    /// placeholder whose bytes are not local: a rename would move only the stub out of the sync
    /// root and leave the version unreadable, so the content is read (hydrated) and copied.
    fn needs_copy(&self, parent: &DirHandle, name: &std::ffi::OsStr) -> bool {
        publish::is_offline_placeholder(parent, name)
    }
}

pub struct HandleMover;

impl Mover for HandleMover {
    fn rename_into(
        &self,
        src_parent: &DirHandle,
        src_name: &std::ffi::OsStr,
        dst_parent: &DirHandle,
        dst_name: &std::ffi::OsStr,
        check: publish::FactsCheck<'_>,
    ) -> Result<bool, MoveError> {
        publish::rename_file_into_if(src_parent, src_name, dst_parent, dst_name, check)
    }
}

// ── Time ──

pub trait Sleeper: Send + Sync {
    /// Wait `d`. `false` = the cancel token fired while waiting.
    fn sleep(&self, d: Duration, cancel: &CancelToken) -> bool;
}

pub struct RealSleeper;

impl Sleeper for RealSleeper {
    fn sleep(&self, d: Duration, cancel: &CancelToken) -> bool {
        let end = std::time::Instant::now() + d;
        while std::time::Instant::now() < end {
            if cancel.is_cancelled() {
                return false;
            }
            std::thread::sleep(Duration::from_millis(50).min(d));
        }
        !cancel.is_cancelled()
    }
}

pub trait Clock: Send + Sync {
    /// `yyyy-MM-dd HHmmss` in the SYSTEM time zone (design §5.5); never contains a character
    /// that is invalid on FAT/exFAT.
    fn local_stamp(&self) -> String;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn local_stamp(&self) -> String {
        crate::sync_engine::versioning::local_stamp_now()
    }
}

// ── Roots ──

/// Is a sync root still there? (RQ-7.4: a root that disappears mid-run stops the run.)
pub trait Liveness: Send + Sync {
    fn alive(&self, root: &str) -> bool;
}

pub struct FsLiveness;

impl Liveness for FsLiveness {
    fn alive(&self, root: &str) -> bool {
        std::fs::metadata(root).map(|m| m.is_dir()).unwrap_or(false)
    }
}

// ── Test hooks ──

/// Observation points between the steps of a run. All no-ops in production; tests use them to
/// change the world "between plan and move".
pub trait Hooks: Send + Sync {
    /// The final plan, before it is validated (tests tamper with it to prove that a plan that
    /// breaks the profile contract is refused in every build).
    fn on_final_plan(&self, _plan: &mut Vec<crate::sync_engine::plan::SyncPlanItem>) {}
    /// The gate passed; the roots are about to be opened and proven by identity on the handle.
    fn after_gate(&self) {}
    /// The plan is final and validated, no operation has started.
    fn after_plan(&self) {}
    /// Operation `index` passed its just-in-time check and is about to act (the gap the
    /// handle-level checks of delete and move close).
    fn after_jit(&self, _index: usize, _name: &str) {}
    /// FER-564: an in-place overwrite has copied its staged file and is about to re-check the
    /// target and swap it in (the gap the commit-time check closes).
    fn before_commit(&self, _index: usize, _name: &str) {}
    /// Called right before operation `index` (after the stop checks, before its just-in-time check).
    fn before_op(&self, _index: usize, _name: &str) {}
}

pub struct NoHooks;

impl Hooks for NoHooks {}

// ── Bundle ──

/// Every injectable dependency of one run.
pub struct EngineEnv {
    pub probe: Arc<dyn RootProbe>,
    pub copier: Arc<dyn Copier>,
    pub trash: Arc<dyn Trash>,
    pub bin: Arc<dyn BinConfig>,
    pub mover: Arc<dyn Mover>,
    pub sleeper: Arc<dyn Sleeper>,
    pub clock: Arc<dyn Clock>,
    pub liveness: Arc<dyn Liveness>,
    pub hooks: Arc<dyn Hooks>,
    /// FER-562: protected folders and Startup (production), or a test policy.
    pub guard: Arc<dyn crate::sync_engine::write_guard::WriteGuard>,
    /// The FerrumIndexer journal (IX-1); tests answer `complete` / `rescan` here.
    pub changes: Mutex<Box<dyn ChangesSource + Send>>,
}

impl EngineEnv {
    pub fn production() -> Self {
        EngineEnv {
            probe: Arc::new(FsProbe),
            copier: Arc::new(EngineCopier),
            trash: Arc::new(SystemTrash),
            bin: Arc::new(RegistryBinConfig),
            mover: Arc::new(HandleMover),
            sleeper: Arc::new(RealSleeper),
            clock: Arc::new(SystemClock),
            liveness: Arc::new(FsLiveness),
            hooks: Arc::new(NoHooks),
            guard: Arc::new(crate::sync_engine::write_guard::FerrumGuard),
            changes: Mutex::new(Box::new(ServiceChanges)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real Recycle Bin, on a temp file owned by this test. Ignored by default because it
    /// leaves one entry (`ferrum-p02-trash-test-<pid>.txt`) in the current user's Recycle Bin;
    /// run it by hand with `cargo test system_trash -- --ignored` to verify the shell path.
    #[test]
    #[ignore = "puts one temp file into the real Recycle Bin"]
    fn system_trash_moves_one_file_to_the_recycle_bin() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir
            .path()
            .join(format!("ferrum-p02-trash-test-{}.txt", std::process::id()));
        std::fs::write(&file, b"temp").unwrap();
        SystemTrash.trash_file(&file).unwrap();
        assert!(!file.exists());
    }

    #[test]
    fn production_env_wires_the_real_implementations() {
        let env = EngineEnv::production();
        assert_eq!(env.clock.local_stamp().len(), "2026-01-02 030405".len());
        assert!(env.liveness.alive(&std::env::temp_dir().to_string_lossy()));
        assert!(!env.liveness.alive(r"Z:\definitely\not\here\ferrum"));
    }
}
