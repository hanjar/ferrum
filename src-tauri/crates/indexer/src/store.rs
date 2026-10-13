//! The service's data directory `C:\ProgramData\Ferrum` (FER-474): ACL,
//! trust checks, config read/write, and the bounded log file.
//!
//! Threat (review H1): any user may create folders under `C:\ProgramData`,
//! so a pre-created `Ferrum` folder, a junction named `Ferrum`, or planted
//! children (`indexer.conf` with `mode=high_performance`, a hard/symbolic
//! link named `indexer.log` pointing at a system file) must never be
//! trusted or written through. Rules:
//! - The directory is created ATOMICALLY with its final security descriptor
//!   (`CreateDirectoryW` + SD: owner Administrators, protected DACL SYSTEM +
//!   Administrators), never created first and secured later.
//! - An existing directory is used only if it is a plain directory (not a
//!   reparse point) owned by SYSTEM/Administrators. Otherwise the installer
//!   renames it aside THROUGH THE CHECKED HANDLE and creates a fresh one; if
//!   the name exists again at that point the installer fails — it never
//!   adopts a directory it did not create. The service never renames; it
//!   runs without persistence instead.
//! - While in use, the directory handle is held WITHOUT `FILE_SHARE_DELETE`,
//!   so the verified directory cannot be renamed or replaced underneath.
//! - Every child (`indexer.conf`, `.tmp`, `indexer.log`, `.log.1`) is opened
//!   with `FILE_FLAG_OPEN_REPARSE_POINT` and checked AFTER opening: not a
//!   directory, not a reparse point, link count 1, owner trusted. Untrusted
//!   children are deleted through their own handle (removing a link never
//!   touches its target). All reads/writes/renames go through the verified
//!   handle; the config is written atomically (`CREATE_NEW` temp file with an
//!   explicit SD, then rename-by-handle with replace).

use crate::config::{CONFIG_FILE, Config, ConfigError, DATA_DIR, MAX_CONFIG_BYTES};
use crate::win::{
    OwnedHandle, SID_ADMINISTRATORS, SID_LOCAL_SYSTEM, SecurityDescriptor, current_user_sid,
    sid_bytes, sid_string, wide, win_err,
};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use windows_sys::Win32::Foundation::{
    ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS, GENERIC_READ,
    GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE, LocalFree,
};
use windows_sys::Win32::Security::ACL;
use windows_sys::Win32::Security::Authorization::{
    GetSecurityInfo, SE_FILE_OBJECT, SetSecurityInfo,
};
use windows_sys::Win32::Security::{
    DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, GetSecurityDescriptorOwner,
    OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
    SECURITY_ATTRIBUTES,
};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, CREATE_NEW, CreateDirectoryW, CreateFileW, DELETE,
    FILE_APPEND_DATA, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_DISPOSITION_INFO, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_RENAME_INFO,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FileDispositionInfo, FileRenameInfo,
    FlushFileBuffers, GetFileInformationByHandle, OPEN_ALWAYS, OPEN_EXISTING, READ_CONTROL,
    ReadFile, RemoveDirectoryW, SetFileInformationByHandle, WRITE_DAC, WRITE_OWNER, WriteFile,
};

pub const LOG_FILE: &str = "indexer.log";
pub const LOG_FILE_OLD: &str = "indexer.log.1";
pub const CONFIG_TMP: &str = "indexer.conf.tmp";
pub const MAX_LOG_BYTES: u64 = 1024 * 1024;
/// Every name the service ever creates in the directory.
pub const KNOWN_FILES: [&str; 4] = [CONFIG_FILE, CONFIG_TMP, LOG_FILE, LOG_FILE_OLD];

/// `NT SERVICE\FerrumIndexer` (S-1-5-80 + SHA-1 of the upper-cased service
/// name, FER-475). Granted on the data directory so the service keeps
/// working with a write-restricted token (`sc sidtype ... restricted`);
/// the SID exists only in the service's own token.
pub const SERVICE_SID: &str = "S-1-5-80-401986193-3732369529-2668797022-3469762217-2719989880";

/// `C:\ProgramData\Ferrum`.
pub fn data_dir() -> io::Result<PathBuf> {
    Ok(crate::win::program_data()?.join(DATA_DIR))
}

/// Who may own the directory and its files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trust {
    /// Production: SYSTEM or Administrators only; objects are created with
    /// owner Administrators and a protected SYSTEM + Administrators DACL.
    Service,
    /// Console/dev/tests (non-admin): additionally the current user.
    Dev { user: Vec<u8> },
}

impl Trust {
    pub fn dev_current_user() -> io::Result<Trust> {
        Ok(Trust::Dev {
            user: current_user_sid()?,
        })
    }

    fn owner_ok(&self, sid: &[u8]) -> bool {
        sid == SID_LOCAL_SYSTEM
            || sid == SID_ADMINISTRATORS
            || matches!(self, Trust::Dev { user } if user == sid)
    }

    fn sets_owner(&self) -> bool {
        matches!(self, Trust::Service)
    }

    /// SD for the directory (inheritable to children).
    pub fn dir_sddl(&self) -> String {
        match self {
            Trust::Service => {
                format!("O:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;{SERVICE_SID})")
            }
            // The service SID ACE lets tests emulate the restricted service
            // token in a dev directory (FER-476); it grants nobody else.
            Trust::Dev { user } => format!(
                "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;{})(A;OICI;FA;;;{SERVICE_SID})",
                sid_string(user).unwrap_or_default()
            ),
        }
    }

    /// Explicit SD for files the service creates.
    pub fn file_sddl(&self) -> String {
        match self {
            Trust::Service => format!("O:BAD:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;FA;;;{SERVICE_SID})"),
            Trust::Dev { user } => format!(
                "D:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;FA;;;{})(A;;FA;;;{SERVICE_SID})",
                sid_string(user).unwrap_or_default()
            ),
        }
    }
}

/// What a handle points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Facts {
    pub is_dir: bool,
    pub reparse: bool,
    pub links: u32,
    pub owner_trusted: bool,
    pub size: u64,
}

impl Facts {
    pub fn trusted_dir(&self) -> bool {
        self.is_dir && !self.reparse && self.owner_trusted
    }
    pub fn trusted_file(&self) -> bool {
        !self.is_dir && !self.reparse && self.links == 1 && self.owner_trusted
    }
}

fn not_found(e: &io::Error) -> bool {
    matches!(
        e.raw_os_error().map(|c| c as u32),
        Some(ERROR_FILE_NOT_FOUND) | Some(ERROR_PATH_NOT_FOUND)
    )
}

fn already_exists(e: &io::Error) -> bool {
    e.raw_os_error().map(|c| c as u32) == Some(ERROR_ALREADY_EXISTS)
}

fn untrusted(what: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!("{what} is not trusted"),
    )
}

/// `CreateFileW` that never follows a reparse point.
fn open_raw(
    path: &Path,
    access: u32,
    share: u32,
    disposition: u32,
    dir: bool,
    sd: Option<&SecurityDescriptor>,
) -> io::Result<OwnedHandle> {
    let w = wide(path);
    let sa = sd.map(|sd| SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.0,
        bInheritHandle: 0,
    });
    let mut flags = FILE_FLAG_OPEN_REPARSE_POINT;
    flags |= if dir {
        FILE_FLAG_BACKUP_SEMANTICS
    } else {
        FILE_ATTRIBUTE_NORMAL
    };
    let h = unsafe {
        CreateFileW(
            w.as_ptr(),
            access,
            share,
            sa.as_ref().map_or(std::ptr::null(), |s| s as *const _),
            disposition,
            flags,
            std::ptr::null_mut(),
        )
    };
    if h == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    Ok(OwnedHandle(h))
}

/// Open a directory to hold it (no `FILE_SHARE_DELETE`), with
/// `WRITE_DAC`/`WRITE_OWNER` when `fix` is wanted. FER-476: under the
/// write-restricted service token (`SERVICE_SID_TYPE_RESTRICTED`) those
/// write rights may be refused (an SD written before the service SID ACE
/// existed); the directory is then held WITHOUT re-applying its SD —
/// `install` (elevated, unrestricted) seals it. Returns whether the SD may
/// be re-applied.
fn open_dir_maybe_fix(path: &Path, trust: &Trust, fix: bool) -> io::Result<(OwnedHandle, bool)> {
    // FILE_LIST_DIRECTORY (data access) makes the share mode take effect: a
    // handle with only attribute/control access does not participate in
    // sharing checks.
    let base = FILE_LIST_DIRECTORY | READ_CONTROL | FILE_READ_ATTRIBUTES;
    let share = FILE_SHARE_READ | FILE_SHARE_WRITE;
    if fix {
        let access = base | WRITE_DAC | if trust.sets_owner() { WRITE_OWNER } else { 0 };
        match open_raw(path, access, share, OPEN_EXISTING, true, None) {
            Ok(h) => return Ok((h, true)),
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {}
            Err(e) => return Err(e),
        }
    }
    Ok((
        open_raw(path, base, share, OPEN_EXISTING, true, None)?,
        false,
    ))
}

/// Re-apply `trust.file_sddl()` to an existing trusted plain file (by
/// handle, never following links). Used by `install` (FER-476) so files
/// written before the service SID ACE existed (S1) stay writable by the
/// write-restricted service token. Absent or untrusted files are skipped.
fn reseal_file(path: &Path, trust: &Trust) -> io::Result<bool> {
    let mut access = READ_CONTROL | FILE_READ_ATTRIBUTES | WRITE_DAC;
    if trust.sets_owner() {
        access |= WRITE_OWNER;
    }
    let h = match open_raw(
        path,
        access,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        OPEN_EXISTING,
        false,
        None,
    ) {
        Ok(h) => h,
        Err(e) if not_found(&e) => return Ok(false),
        Err(e) => return Err(e),
    };
    if !facts(&h, trust)?.trusted_file() {
        return Ok(false);
    }
    apply_sddl(&h, &trust.file_sddl(), trust.sets_owner())?;
    Ok(true)
}

fn owner_of(h: &OwnedHandle) -> io::Result<Vec<u8>> {
    let mut owner: PSID = std::ptr::null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    let rc = unsafe {
        GetSecurityInfo(
            h.raw(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut sd,
        )
    };
    if rc != ERROR_SUCCESS {
        return Err(win_err(rc));
    }
    let out = unsafe { sid_bytes(owner) };
    unsafe { LocalFree(sd) };
    out
}

/// Facts of an open handle (needs READ_CONTROL for the owner).
pub fn facts(h: &OwnedHandle, trust: &Trust) -> io::Result<Facts> {
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(h.raw(), &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let owner = owner_of(h)?;
    Ok(Facts {
        is_dir: info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0,
        reparse: info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0,
        links: info.nNumberOfLinks,
        owner_trusted: trust.owner_ok(&owner),
        size: ((info.nFileSizeHigh as u64) << 32) | info.nFileSizeLow as u64,
    })
}

/// Rename the object behind `h` to the absolute path `to`.
fn rename_handle(h: &OwnedHandle, to: &Path, replace: bool) -> io::Result<()> {
    let name: Vec<u16> = wide(to);
    let name = &name[..name.len() - 1]; // without NUL; length is explicit
    let head = std::mem::offset_of!(FILE_RENAME_INFO, FileName);
    let bytes = head + (name.len() + 1) * 2;
    let mut buf = vec![0u64; bytes.div_ceil(8)];
    let info = buf.as_mut_ptr() as *mut FILE_RENAME_INFO;
    unsafe {
        (*info).Anonymous.ReplaceIfExists = replace;
        (*info).RootDirectory = std::ptr::null_mut();
        (*info).FileNameLength = (name.len() * 2) as u32;
        let dst = (buf.as_mut_ptr() as *mut u8).add(head) as *mut u16;
        std::ptr::copy_nonoverlapping(name.as_ptr(), dst, name.len());
    }
    if unsafe {
        SetFileInformationByHandle(h.raw(), FileRenameInfo, buf.as_ptr().cast(), bytes as u32)
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Delete the directory entry behind `h` (a link is removed, never its target).
fn delete_handle(h: &OwnedHandle) -> io::Result<()> {
    let info = FILE_DISPOSITION_INFO { DeleteFile: true };
    if unsafe {
        SetFileInformationByHandle(
            h.raw(),
            FileDispositionInfo,
            (&info as *const FILE_DISPOSITION_INFO).cast(),
            std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn write_handle(h: HANDLE, mut data: &[u8]) -> io::Result<()> {
    while !data.is_empty() {
        let mut n = 0u32;
        let chunk = data.len().min(64 * 1024) as u32;
        if unsafe { WriteFile(h, data.as_ptr(), chunk, &mut n, std::ptr::null_mut()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if n == 0 {
            return Err(io::ErrorKind::WriteZero.into());
        }
        data = &data[n as usize..];
    }
    Ok(())
}

fn read_handle(h: HANDLE, max: u64) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let mut n = 0u32;
        if unsafe {
            ReadFile(
                h,
                buf.as_mut_ptr(),
                buf.len() as u32,
                &mut n,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        if n == 0 {
            return Ok(out);
        }
        out.extend_from_slice(&buf[..n as usize]);
        if out.len() as u64 > max {
            return Ok(out);
        }
    }
}

/// `CreateDirectoryW` with the final security descriptor.
fn create_dir_sd(path: &Path, sd: &SecurityDescriptor) -> io::Result<()> {
    let w = wide(path);
    let sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.0,
        bInheritHandle: 0,
    };
    if unsafe { CreateDirectoryW(w.as_ptr(), &sa) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Apply `sddl` (owner if wanted + protected DACL) through `h`.
fn apply_sddl(h: &OwnedHandle, sddl: &str, set_owner: bool) -> io::Result<()> {
    let sd = SecurityDescriptor::from_sddl(sddl)?;
    let mut present = 0i32;
    let mut defaulted = 0i32;
    let mut dacl: *mut ACL = std::ptr::null_mut();
    if unsafe { GetSecurityDescriptorDacl(sd.0, &mut present, &mut dacl, &mut defaulted) } == 0
        || present == 0
    {
        return Err(io::Error::other("SDDL without DACL"));
    }
    let mut owner: PSID = std::ptr::null_mut();
    let mut info = DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION;
    if set_owner {
        if unsafe { GetSecurityDescriptorOwner(sd.0, &mut owner, &mut defaulted) } == 0
            || owner.is_null()
        {
            return Err(io::Error::other("SDDL without owner"));
        }
        info |= OWNER_SECURITY_INFORMATION;
    }
    let rc = unsafe {
        SetSecurityInfo(
            h.raw(),
            SE_FILE_OBJECT,
            info,
            owner,
            std::ptr::null_mut(),
            dacl,
            std::ptr::null(),
        )
    };
    if rc != ERROR_SUCCESS {
        return Err(win_err(rc));
    }
    Ok(())
}

/// A verified data directory, held open (no `FILE_SHARE_DELETE`) for as long
/// as this value lives.
pub struct DataDir {
    path: PathBuf,
    _hold: OwnedHandle,
    trust: Trust,
}

impl std::fmt::Debug for DataDir {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DataDir").field("path", &self.path).finish()
    }
}

impl DataDir {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Installer (elevated): create or verify the directory, moving an
    /// untrusted one aside. Returns the aside path when that happened.
    pub fn prepare(path: &Path, trust: Trust) -> io::Result<(DataDir, Option<PathBuf>)> {
        Self::prepare_with(path, trust, &mut || {})
    }

    /// `prepare` with a hook between "moved aside" and "create fresh" (tests
    /// use it to plant a directory in that window).
    pub fn prepare_with(
        path: &Path,
        trust: Trust,
        between: &mut dyn FnMut(),
    ) -> io::Result<(DataDir, Option<PathBuf>)> {
        let sd = SecurityDescriptor::from_sddl(&trust.dir_sddl())?;
        let mut aside = None;
        match create_dir_sd(path, &sd) {
            Ok(()) => {}
            Err(e) if already_exists(&e) => {
                let h = open_raw(
                    path,
                    READ_CONTROL | DELETE | FILE_READ_ATTRIBUTES,
                    FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                    OPEN_EXISTING,
                    true,
                    None,
                )?;
                if !facts(&h, &trust)?.trusted_dir() {
                    let secs = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_nanos())
                        .unwrap_or(0);
                    let file = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| DATA_DIR.into());
                    let to = path.with_file_name(format!("{file}.untrusted-{secs}"));
                    rename_handle(&h, &to, false)?;
                    drop(h);
                    aside = Some(to);
                    between();
                    // Phase 2: create fresh. Never adopt something that
                    // appeared at the name in the meantime.
                    create_dir_sd(path, &sd).map_err(|e| {
                        if already_exists(&e) {
                            io::Error::new(
                                io::ErrorKind::PermissionDenied,
                                "data directory re-appeared after it was moved aside; refusing to adopt it",
                            )
                        } else {
                            e
                        }
                    })?;
                }
            }
            Err(e) => return Err(e),
        }
        let dd = Self::hold(path, trust, true)?.ok_or_else(|| untrusted("data directory"))?;
        dd.sanitize()?;
        dd.reseal()?;
        Ok((dd, aside))
    }

    /// Service start: create atomically if missing; `Ok(None)` when present
    /// but untrusted (the service then runs without persistence).
    pub fn open_for_service(path: &Path, trust: Trust) -> io::Result<Option<DataDir>> {
        let sd = SecurityDescriptor::from_sddl(&trust.dir_sddl())?;
        match create_dir_sd(path, &sd) {
            Ok(()) => {}
            Err(e) if already_exists(&e) => {}
            Err(e) => return Err(e),
        }
        let Some(dd) = Self::hold(path, trust, true)? else {
            return Ok(None);
        };
        dd.sanitize()?;
        Ok(Some(dd))
    }

    /// Verify only (no ACL change, no cleanup): `has-config`, `purge`.
    /// `Ok(None)` = missing or untrusted.
    pub fn open_existing(path: &Path, trust: Trust) -> io::Result<Option<DataDir>> {
        match Self::hold(path, trust, false) {
            Err(e) if not_found(&e) => Ok(None),
            other => other,
        }
    }

    fn hold(path: &Path, trust: Trust, fix: bool) -> io::Result<Option<DataDir>> {
        // FILE_LIST_DIRECTORY (data access) makes the share mode below take
        // effect: a handle with only attribute/control access does not
        // participate in sharing checks.
        // No FILE_SHARE_DELETE: nobody can rename/replace the directory while
        // we hold it, so child paths stay inside the verified directory.
        let (h, fix) = open_dir_maybe_fix(path, &trust, fix)?;
        if !facts(&h, &trust)?.trusted_dir() {
            return Ok(None);
        }
        if fix {
            apply_sddl(&h, &trust.dir_sddl(), trust.sets_owner())?;
        }
        Ok(Some(DataDir {
            path: path.to_path_buf(),
            _hold: h,
            trust,
        }))
    }

    fn child(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }

    /// Open an existing child and require it to be a trusted plain file.
    /// `Ok(None)` = absent; `Err(PermissionDenied)` = present but untrusted.
    fn open_verified(
        &self,
        name: &str,
        access: u32,
        share: u32,
    ) -> io::Result<Option<OwnedHandle>> {
        let h = match open_raw(
            &self.child(name),
            access | READ_CONTROL | FILE_READ_ATTRIBUTES,
            share,
            OPEN_EXISTING,
            false,
            None,
        ) {
            Ok(h) => h,
            Err(e) if not_found(&e) => return Ok(None),
            Err(e) => return Err(e),
        };
        if !facts(&h, &self.trust)?.trusted_file() {
            return Err(untrusted(name));
        }
        Ok(Some(h))
    }

    /// Delete every known child that is not a trusted plain file (links and
    /// planted files). Returns the names removed.
    pub fn sanitize(&self) -> io::Result<Vec<&'static str>> {
        let mut removed = Vec::new();
        for name in KNOWN_FILES {
            let h = match open_raw(
                &self.child(name),
                DELETE | READ_CONTROL | FILE_READ_ATTRIBUTES,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                OPEN_EXISTING,
                false,
                None,
            ) {
                Ok(h) => h,
                Err(e) if not_found(&e) => continue,
                Err(e) => return Err(e),
            };
            let ok = facts(&h, &self.trust)
                .map(|f| f.trusted_file())
                .unwrap_or(false);
            if !ok {
                delete_handle(&h)?;
                removed.push(name);
            }
        }
        Ok(removed)
    }

    /// Installer (elevated, unrestricted token): re-apply the current SDs
    /// to the known files and the saved indexes, so the write-restricted
    /// service token can keep writing files created by an older version.
    pub fn reseal(&self) -> io::Result<usize> {
        let mut n = 0;
        for name in KNOWN_FILES {
            n += reseal_file(&self.child(name), &self.trust)? as usize;
        }
        if let Some(ix) = self.index_dir()? {
            for name in ix.our_names()? {
                n += reseal_file(&ix.child(&name), &self.trust)? as usize;
            }
        }
        Ok(n)
    }

    /// Read the config: `Ok(None)` when absent; an untrusted file yields the
    /// default with `ConfigError::Untrusted`.
    pub fn read_config(&self) -> io::Result<Option<(Config, Option<ConfigError>)>> {
        let h = match self.open_verified(CONFIG_FILE, GENERIC_READ, FILE_SHARE_READ) {
            Ok(Some(h)) => h,
            Ok(None) => return Ok(None),
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                return Ok(Some((Config::default(), Some(ConfigError::Untrusted))));
            }
            Err(e) => return Err(e),
        };
        let bytes = read_handle(h.raw(), MAX_CONFIG_BYTES)?;
        if bytes.len() as u64 > MAX_CONFIG_BYTES {
            return Ok(Some((Config::default(), Some(ConfigError::TooLarge))));
        }
        Ok(Some(Config::parse_or_default(&bytes)))
    }

    /// Write the config atomically: fresh temp file (`CREATE_NEW`, explicit
    /// SD) → flush → rename-by-handle over `indexer.conf`.
    pub fn write_config(&self, cfg: &Config) -> io::Result<()> {
        // A leftover temp (crash, or planted link) is removed by its handle.
        if let Ok(h) = open_raw(
            &self.child(CONFIG_TMP),
            DELETE,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            OPEN_EXISTING,
            false,
            None,
        ) {
            delete_handle(&h)?;
        }
        let sd = SecurityDescriptor::from_sddl(&self.trust.file_sddl())?;
        let h = open_raw(
            &self.child(CONFIG_TMP),
            GENERIC_WRITE | DELETE | READ_CONTROL | FILE_READ_ATTRIBUTES,
            0,
            CREATE_NEW,
            false,
            Some(&sd),
        )?;
        if !facts(&h, &self.trust)?.trusted_file() {
            let _ = delete_handle(&h);
            return Err(untrusted(CONFIG_TMP));
        }
        let res = write_handle(h.raw(), cfg.serialize().as_bytes())
            .and_then(|()| {
                if unsafe { FlushFileBuffers(h.raw()) } == 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(())
                }
            })
            .and_then(|()| rename_handle(&h, &self.child(CONFIG_FILE), true));
        if res.is_err() {
            let _ = delete_handle(&h);
        }
        res
    }

    /// Append one line to the log through a verified handle; rotate at
    /// [`MAX_LOG_BYTES`] by renaming the verified handle.
    pub fn append_log(&self, line: &str) -> io::Result<()> {
        let sd = SecurityDescriptor::from_sddl(&self.trust.file_sddl())?;
        let open = || {
            open_raw(
                &self.child(LOG_FILE),
                FILE_APPEND_DATA | DELETE | READ_CONTROL | FILE_READ_ATTRIBUTES,
                FILE_SHARE_READ,
                OPEN_ALWAYS,
                false,
                Some(&sd),
            )
        };
        let mut h = open()?;
        let mut f = facts(&h, &self.trust)?;
        if !f.trusted_file() {
            let _ = delete_handle(&h);
            return Err(untrusted(LOG_FILE));
        }
        if f.size > MAX_LOG_BYTES {
            rename_handle(&h, &self.child(LOG_FILE_OLD), true)?;
            drop(h);
            h = open()?;
            f = facts(&h, &self.trust)?;
            if !f.trusted_file() {
                let _ = delete_handle(&h);
                return Err(untrusted(LOG_FILE));
            }
        }
        write_handle(h.raw(), line.as_bytes())
    }

    /// Uninstall `--purge-data`: delete the known files (by handle), then the
    /// directory if it is empty. Other content is kept.
    pub fn purge(self) -> io::Result<()> {
        self.purge_indexes()?;
        for name in KNOWN_FILES {
            match open_raw(
                &self.child(name),
                DELETE,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                OPEN_EXISTING,
                false,
                None,
            ) {
                Ok(h) => delete_handle(&h)?,
                Err(e) if not_found(&e) => {}
                Err(e) => return Err(e),
            }
        }
        let path = self.path.clone();
        drop(self);
        // Only succeeds on an empty directory; never follows a reparse point.
        let w = wide(&path);
        unsafe { RemoveDirectoryW(w.as_ptr()) };
        Ok(())
    }
}

// ── saved indexes: <data dir>\index (FER-475) ────────────────────────────

/// Subdirectory of the data directory holding saved volume indexes.
pub const INDEX_DIR: &str = "index";
const INDEX_PREFIX: &str = "vol-";
const INDEX_EXT: &str = ".idx";
const INDEX_TMP_EXT: &str = ".idx.tmp";

/// `vol-<serial as 16 hex digits>.idx`.
pub fn index_file_name(serial: u64) -> String {
    format!("{INDEX_PREFIX}{serial:016X}{INDEX_EXT}")
}

/// Is `name` one of ours (`vol-XXXXXXXXXXXXXXXX.idx` or `.idx.tmp`)?
pub fn is_index_file_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix(INDEX_PREFIX) else {
        return false;
    };
    let hex = rest
        .strip_suffix(INDEX_TMP_EXT)
        .or_else(|| rest.strip_suffix(INDEX_EXT));
    hex.is_some_and(|h| h.len() == 16 && h.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// The verified `index` subdirectory, held open without `FILE_SHARE_DELETE`
/// for as long as this value lives — same rules as [`DataDir`]: created
/// atomically with its final SD, every file opened without following
/// reparse points and checked after opening (plain file, link count 1,
/// trusted owner), all I/O through the verified handle, writes atomic
/// (`CREATE_NEW` temp + rename-by-handle), untrusted entries deleted by
/// their own handle.
pub struct IndexDir {
    path: PathBuf,
    _hold: OwnedHandle,
    trust: Trust,
}

impl DataDir {
    /// Open (creating atomically when missing) the `index` subdirectory.
    /// `Ok(None)` = present but untrusted: run without saved indexes.
    pub fn index_dir(&self) -> io::Result<Option<IndexDir>> {
        let path = self.child(INDEX_DIR);
        let sd = SecurityDescriptor::from_sddl(&self.trust.dir_sddl())?;
        match create_dir_sd(&path, &sd) {
            Ok(()) => {}
            Err(e) if already_exists(&e) => {}
            Err(e) => return Err(e),
        }
        let trust = self.trust.clone();
        let (h, fix) = open_dir_maybe_fix(&path, &trust, true)?;
        if !facts(&h, &trust)?.trusted_dir() {
            return Ok(None);
        }
        if fix {
            apply_sddl(&h, &trust.dir_sddl(), trust.sets_owner())?;
        }
        let dir = IndexDir {
            path,
            _hold: h,
            trust,
        };
        dir.sanitize()?;
        Ok(Some(dir))
    }

    /// `--purge-data`: remove the saved indexes and the `index` directory
    /// (only our own file names, by handle; the directory only if empty).
    pub fn purge_indexes(&self) -> io::Result<()> {
        let path = self.child(INDEX_DIR);
        let h = match open_raw(
            &path,
            FILE_LIST_DIRECTORY | READ_CONTROL | FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            OPEN_EXISTING,
            true,
            None,
        ) {
            Ok(h) => h,
            Err(e) if not_found(&e) => return Ok(()),
            Err(e) => return Err(e),
        };
        if !facts(&h, &self.trust)?.trusted_dir() {
            return Err(untrusted("index directory"));
        }
        let dir = IndexDir {
            path: path.clone(),
            _hold: h,
            trust: self.trust.clone(),
        };
        dir.remove_all()?;
        drop(dir);
        let w = wide(&path);
        unsafe { RemoveDirectoryW(w.as_ptr()) };
        Ok(())
    }
}

impl IndexDir {
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn child(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }

    /// Our file names currently present (the directory is held, so the
    /// listing is of the verified directory).
    fn our_names(&self) -> io::Result<Vec<String>> {
        let mut out = Vec::new();
        for e in std::fs::read_dir(&self.path)? {
            let name = e?.file_name().to_string_lossy().into_owned();
            if is_index_file_name(&name) {
                out.push(name);
            }
        }
        Ok(out)
    }

    fn delete_name(&self, name: &str) -> io::Result<()> {
        match open_raw(
            &self.child(name),
            DELETE,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            OPEN_EXISTING,
            false,
            None,
        ) {
            Ok(h) => delete_handle(&h),
            Err(e) if not_found(&e) => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Delete our names that are not trusted plain files, and leftover
    /// temp files. Returns how many were removed.
    pub fn sanitize(&self) -> io::Result<usize> {
        let mut removed = 0;
        for name in self.our_names()? {
            let h = match open_raw(
                &self.child(&name),
                DELETE | READ_CONTROL | FILE_READ_ATTRIBUTES,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                OPEN_EXISTING,
                false,
                None,
            ) {
                Ok(h) => h,
                Err(e) if not_found(&e) => continue,
                Err(e) => return Err(e),
            };
            let ok = facts(&h, &self.trust)
                .map(|f| f.trusted_file())
                .unwrap_or(false);
            if !ok || name.ends_with(INDEX_TMP_EXT) {
                delete_handle(&h)?;
                removed += 1;
            }
        }
        Ok(removed)
    }

    /// Read the saved index of `serial`. `Ok(None)` when absent, larger
    /// than `max` bytes, or untrusted (then it is deleted by its handle).
    pub fn load(&self, serial: u64, max: u64) -> io::Result<Option<Vec<u8>>> {
        let name = index_file_name(serial);
        let h = match open_raw(
            &self.child(&name),
            GENERIC_READ | DELETE | READ_CONTROL | FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ,
            OPEN_EXISTING,
            false,
            None,
        ) {
            Ok(h) => h,
            Err(e) if not_found(&e) => return Ok(None),
            Err(e) => return Err(e),
        };
        let f = facts(&h, &self.trust)?;
        if !f.trusted_file() || f.size > max {
            delete_handle(&h)?;
            return Ok(None);
        }
        let bytes = read_handle(h.raw(), max)?;
        if bytes.len() as u64 > max {
            delete_handle(&h)?;
            return Ok(None);
        }
        Ok(Some(bytes))
    }

    /// Save atomically: fresh temp file (`CREATE_NEW`, explicit SD) →
    /// flush → rename-by-handle over the final name.
    pub fn save(&self, serial: u64, bytes: &[u8]) -> io::Result<()> {
        let name = index_file_name(serial);
        let tmp = format!("{name}.tmp");
        self.delete_name(&tmp)?;
        let sd = SecurityDescriptor::from_sddl(&self.trust.file_sddl())?;
        let h = open_raw(
            &self.child(&tmp),
            GENERIC_WRITE | DELETE | READ_CONTROL | FILE_READ_ATTRIBUTES,
            0,
            CREATE_NEW,
            false,
            Some(&sd),
        )?;
        if !facts(&h, &self.trust)?.trusted_file() {
            let _ = delete_handle(&h);
            return Err(untrusted("index temp file"));
        }
        let res = write_handle(h.raw(), bytes)
            .and_then(|()| {
                if unsafe { FlushFileBuffers(h.raw()) } == 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(())
                }
            })
            .and_then(|()| rename_handle(&h, &self.child(&name), true));
        if res.is_err() {
            let _ = delete_handle(&h);
        }
        res
    }

    /// Delete the saved index of `serial` (by handle).
    pub fn remove(&self, serial: u64) -> io::Result<()> {
        self.delete_name(&index_file_name(serial))
    }

    /// Delete every saved index (privacy mode changed).
    pub fn remove_all(&self) -> io::Result<usize> {
        let names = self.our_names()?;
        for n in &names {
            self.delete_name(n)?;
        }
        Ok(names.len())
    }
}

// ── log ──────────────────────────────────────────────────────────────────

enum Sink {
    Stderr,
    Dir(Arc<DataDir>),
}

static LOG: Mutex<Option<Sink>> = Mutex::new(None);

/// Log to stderr (console mode).
pub fn log_to_stderr() {
    *LOG.lock().unwrap_or_else(|e| e.into_inner()) = Some(Sink::Stderr);
}

/// Log to `<dir>\indexer.log` (service mode), rotated at 1 MiB.
pub fn log_to_dir(dir: Arc<DataDir>) {
    *LOG.lock().unwrap_or_else(|e| e.into_inner()) = Some(Sink::Dir(dir));
}

/// One log line. Never logs request payloads or file names; callers pass
/// short diagnostics (codes, SIDs, modes).
pub fn log(msg: &str) {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let line = format!("{secs} {}\r\n", msg.replace(['\r', '\n'], " "));
    let guard = LOG.lock().unwrap_or_else(|e| e.into_inner());
    match &*guard {
        Some(Sink::Stderr) => {
            use std::io::Write;
            let _ = io::stderr().write_all(line.as_bytes());
        }
        Some(Sink::Dir(d)) => {
            let _ = d.append_log(&line);
        }
        None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::Mode;

    fn tmp(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("ferrum-indexer-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn dev() -> Trust {
        Trust::dev_current_user().unwrap()
    }

    /// A fresh trusted data dir `<tmp>/data` (dev trust).
    fn data(name: &str) -> (PathBuf, DataDir) {
        let root = tmp(name);
        let (dd, aside) = DataDir::prepare(&root.join("data"), dev()).unwrap();
        assert!(aside.is_none());
        (root, dd)
    }

    fn mklink(args: &[&str], link: &Path, target: &Path) -> bool {
        std::process::Command::new("cmd")
            .args(["/C", "mklink"])
            .args(args)
            .arg(link)
            .arg(target)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    #[test]
    fn config_write_read_round_trip() {
        let (root, dd) = data("cfg");
        assert!(dd.read_config().unwrap().is_none());
        dd.write_config(&Config {
            mode: Mode::HighPerformance,
        })
        .unwrap();
        dd.write_config(&Config {
            mode: Mode::HighPerformance,
        })
        .unwrap();
        let (c, e) = dd.read_config().unwrap().unwrap();
        assert_eq!((c.mode, e), (Mode::HighPerformance, None));
        assert!(!dd.path().join(CONFIG_TMP).exists());
        drop(dd);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn oversized_or_garbage_config_falls_back_to_shared() {
        let (root, dd) = data("big");
        std::fs::write(dd.path().join(CONFIG_FILE), vec![b'#'; 10_000]).unwrap();
        let (c, e) = dd.read_config().unwrap().unwrap();
        assert_eq!((c.mode, e), (Mode::Shared, Some(ConfigError::TooLarge)));
        std::fs::write(dd.path().join(CONFIG_FILE), b"mode=high_performance\n").unwrap();
        let (c, e) = dd.read_config().unwrap().unwrap();
        assert_eq!(c.mode, Mode::Shared);
        assert!(e.is_some());
        drop(dd);
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// Under production trust a user-owned directory or file is never
    /// trusted (planted folder / planted config).
    #[test]
    fn user_owned_dir_and_child_are_untrusted_in_service_trust() {
        if crate::win::process_elevated() {
            return;
        }
        let root = tmp("own");
        std::fs::write(
            root.join(CONFIG_FILE),
            b"version=1\nmode=high_performance\n",
        )
        .unwrap();
        assert!(
            DataDir::open_existing(&root, Trust::Service)
                .unwrap()
                .is_none()
        );
        let h = open_raw(
            &root.join(CONFIG_FILE),
            READ_CONTROL | FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ,
            OPEN_EXISTING,
            false,
            None,
        )
        .unwrap();
        let f = facts(&h, &Trust::Service).unwrap();
        assert!(!f.owner_trusted && !f.trusted_file());
        assert!(facts(&h, &dev()).unwrap().trusted_file());
        drop(h);
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// Hard links planted as conf / tmp / log: never read, never written
    /// through; the victim keeps its content.
    #[test]
    fn hardlinked_children_are_removed_not_followed() {
        let (root, dd) = data("hard");
        let victim = root.join("victim.txt");
        std::fs::write(&victim, b"VICTIM").unwrap();
        for name in KNOWN_FILES {
            std::fs::hard_link(&victim, dd.path().join(name)).unwrap();
        }
        // Reading a hardlinked config is refused (link count 2).
        let (c, e) = dd.read_config().unwrap().unwrap();
        assert_eq!((c.mode, e), (Mode::Shared, Some(ConfigError::Untrusted)));
        // Writing the config and the log never touches the victim.
        dd.write_config(&Config {
            mode: Mode::HighPerformance,
        })
        .unwrap();
        assert!(dd.append_log("line\r\n").is_err(), "linked log refused");
        assert!(dd.append_log("line\r\n").is_ok(), "fresh log after removal");
        assert_eq!(std::fs::read(&victim).unwrap(), b"VICTIM");
        // Sanitize removes what is left (the .log.1 link) and keeps the rest.
        let removed = dd.sanitize().unwrap();
        assert_eq!(removed, vec![LOG_FILE_OLD]);
        assert_eq!(std::fs::read(&victim).unwrap(), b"VICTIM");
        let (c, e) = dd.read_config().unwrap().unwrap();
        assert_eq!((c.mode, e), (Mode::HighPerformance, None));
        drop(dd);
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// Symbolic links (need Developer Mode or admin; skipped otherwise).
    #[test]
    fn symlinked_children_are_removed_not_followed() {
        let (root, dd) = data("sym");
        let victim = root.join("victim.txt");
        std::fs::write(&victim, b"VICTIM").unwrap();
        if std::os::windows::fs::symlink_file(&victim, dd.path().join(LOG_FILE)).is_err() {
            drop(dd);
            std::fs::remove_dir_all(&root).unwrap();
            return;
        }
        std::os::windows::fs::symlink_file(&victim, dd.path().join(CONFIG_TMP)).unwrap();
        std::os::windows::fs::symlink_file(&victim, dd.path().join(CONFIG_FILE)).unwrap();
        let (_, e) = dd.read_config().unwrap().unwrap();
        assert_eq!(e, Some(ConfigError::Untrusted));
        dd.write_config(&Config { mode: Mode::Shared }).unwrap();
        let _ = dd.append_log("x\r\n");
        assert_eq!(std::fs::read(&victim).unwrap(), b"VICTIM");
        drop(dd);
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// A junction planted at the data-dir name is moved aside (its target is
    /// untouched) and a fresh directory is created.
    #[test]
    fn planted_junction_is_moved_aside() {
        let root = tmp("junction");
        let target = root.join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("keep.txt"), b"KEEP").unwrap();
        let path = root.join("data");
        if !mklink(&["/J"], &path, &target) {
            return;
        }
        let (dd, aside) = DataDir::prepare(&path, dev()).unwrap();
        let aside = aside.expect("moved aside");
        assert!(
            std::fs::symlink_metadata(&aside).is_ok(),
            "junction renamed"
        );
        assert_eq!(std::fs::read(target.join("keep.txt")).unwrap(), b"KEEP");
        assert!(!dd.path().join("keep.txt").exists(), "fresh directory");
        drop(dd);
        let _ = std::fs::remove_dir(&aside);
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// If something appears at the name after the untrusted entry was moved
    /// aside, the installer fails instead of adopting it.
    #[test]
    fn reappearing_directory_is_never_adopted() {
        let root = tmp("race");
        let target = root.join("target");
        std::fs::create_dir(&target).unwrap();
        let path = root.join("data");
        if !mklink(&["/J"], &path, &target) {
            return;
        }
        let p2 = path.clone();
        let err = DataDir::prepare_with(&path, dev(), &mut || {
            std::fs::create_dir(&p2).unwrap();
            std::fs::write(p2.join(CONFIG_FILE), b"version=1\nmode=high_performance\n").unwrap();
        })
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied, "{err}");
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// The held handle blocks renaming the verified directory.
    #[test]
    fn held_directory_cannot_be_renamed() {
        let (root, dd) = data("hold");
        let moved = root.join("moved");
        assert!(std::fs::rename(dd.path(), &moved).is_err());
        drop(dd);
        std::fs::rename(root.join("data"), &moved).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn purge_removes_only_known_files() {
        let (root, dd) = data("purge");
        dd.write_config(&Config { mode: Mode::Shared }).unwrap();
        dd.append_log("x\r\n").unwrap();
        let other = dd.path().join("other.txt");
        std::fs::write(&other, b"keep").unwrap();
        let path = dd.path().to_path_buf();
        dd.purge().unwrap();
        assert!(other.exists());
        assert!(!path.join(CONFIG_FILE).exists() && !path.join(LOG_FILE).exists());
        std::fs::remove_file(&other).unwrap();
        let dd = DataDir::open_existing(&path, dev()).unwrap().unwrap();
        dd.purge().unwrap();
        assert!(!path.exists(), "empty directory removed");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn sddls_parse_and_are_admin_only() {
        let svc = Trust::Service;
        for s in [svc.dir_sddl(), svc.file_sddl()] {
            SecurityDescriptor::from_sddl(&s).unwrap();
            assert!(s.starts_with("O:BAD:P("), "{s}");
            for other in [";;;WD)", ";;;AU)", ";;;BU)", ";;;IU)", ";;;CO)"] {
                assert!(!s.contains(other), "{other}");
            }
        }
        let d = dev();
        SecurityDescriptor::from_sddl(&d.dir_sddl()).unwrap();
        SecurityDescriptor::from_sddl(&d.file_sddl()).unwrap();
    }

    // ── FER-475: saved indexes ──────────────────────────────────────────

    #[test]
    fn index_file_names_are_strict() {
        assert_eq!(index_file_name(0xAB), "vol-00000000000000AB.idx");
        assert!(is_index_file_name("vol-00000000000000AB.idx"));
        assert!(is_index_file_name("vol-00000000000000ab.idx.tmp"));
        for bad in [
            "vol-AB.idx",
            "vol-00000000000000AB.idx.bak",
            "indexer.conf",
            "vol-0000000000000ZAB.idx",
            "xvol-00000000000000AB.idx",
            "vol-00000000000000AB",
        ] {
            assert!(!is_index_file_name(bad), "{bad}");
        }
    }

    #[test]
    fn index_save_load_remove() {
        let (root, dd) = data("idx");
        let ix = dd.index_dir().unwrap().expect("trusted index dir");
        assert!(ix.load(7, 1 << 20).unwrap().is_none());
        ix.save(7, b"first").unwrap();
        ix.save(7, b"second version").unwrap();
        ix.save(9, b"other").unwrap();
        assert_eq!(ix.load(7, 1 << 20).unwrap().unwrap(), b"second version");
        assert!(!ix.path().join("vol-0000000000000007.idx.tmp").exists());
        // Larger than the caller's budget: refused and deleted.
        assert!(ix.load(9, 3).unwrap().is_none());
        assert!(!ix.path().join(index_file_name(9)).exists());
        ix.remove(7).unwrap();
        assert!(ix.load(7, 1 << 20).unwrap().is_none());
        ix.save(1, b"a").unwrap();
        ix.save(2, b"b").unwrap();
        let other = ix.path().join("keep.txt");
        std::fs::write(&other, b"not ours").unwrap();
        assert_eq!(ix.remove_all().unwrap(), 2);
        assert!(other.exists(), "only our own names are removed");
        std::fs::remove_file(&other).unwrap();
        drop(ix);
        // purge removes the index directory too.
        let path = dd.path().to_path_buf();
        dd.index_dir().unwrap().unwrap().save(3, b"c").unwrap();
        dd.purge().unwrap();
        assert!(!path.join(INDEX_DIR).exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn index_files_are_never_followed() {
        let (root, dd) = data("idxlink");
        let victim = root.join("victim.txt");
        std::fs::write(&victim, b"VICTIM").unwrap();
        let ix = dd.index_dir().unwrap().unwrap();
        let name = index_file_name(5);
        // Hard link planted as the index file and as the temp file.
        std::fs::hard_link(&victim, ix.path().join(&name)).unwrap();
        std::fs::hard_link(&victim, ix.path().join(format!("{name}.tmp"))).unwrap();
        assert!(ix.load(5, 1 << 20).unwrap().is_none(), "link refused");
        assert!(!ix.path().join(&name).exists(), "link removed");
        ix.save(5, b"real").unwrap();
        assert_eq!(std::fs::read(&victim).unwrap(), b"VICTIM");
        assert_eq!(ix.load(5, 1 << 20).unwrap().unwrap(), b"real");
        // A planted link found at open time is sanitized away.
        drop(ix);
        std::fs::hard_link(&victim, dd.path().join(INDEX_DIR).join(index_file_name(6))).unwrap();
        let ix = dd.index_dir().unwrap().unwrap();
        assert!(!ix.path().join(index_file_name(6)).exists());
        assert_eq!(std::fs::read(&victim).unwrap(), b"VICTIM");
        drop(ix);
        drop(dd);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn untrusted_index_dir_is_not_used() {
        if crate::win::process_elevated() {
            return;
        }
        // Service trust on a user-created tree: nothing is trusted, so the
        // service never even gets a DataDir; and an index dir that is a
        // junction is refused under dev trust.
        let (root, dd) = data("idxj");
        let target = root.join("elsewhere");
        std::fs::create_dir(&target).unwrap();
        if mklink(&["/J"], &dd.path().join(INDEX_DIR), &target) {
            assert!(dd.index_dir().unwrap().is_none());
        }
        drop(dd);
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// Minimal SHA-1 (tests only) to check the service SID derivation.
    fn sha1(data: &[u8]) -> [u8; 20] {
        let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
        let mut msg = data.to_vec();
        let bits = (data.len() as u64) * 8;
        msg.push(0x80);
        while msg.len() % 64 != 56 {
            msg.push(0);
        }
        msg.extend_from_slice(&bits.to_be_bytes());
        for block in msg.chunks(64) {
            let mut w = [0u32; 80];
            for i in 0..16 {
                w[i] = u32::from_be_bytes(block[i * 4..i * 4 + 4].try_into().unwrap());
            }
            for i in 16..80 {
                w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
            }
            let [mut a, mut b, mut c, mut d, mut e] = h;
            for (i, wi) in w.iter().enumerate() {
                let (f, k) = match i {
                    0..=19 => ((b & c) | (!b & d), 0x5A827999),
                    20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                    40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                    _ => (b ^ c ^ d, 0xCA62C1D6u32),
                };
                let t = a
                    .rotate_left(5)
                    .wrapping_add(f)
                    .wrapping_add(e)
                    .wrapping_add(k)
                    .wrapping_add(*wi);
                e = d;
                d = c;
                c = b.rotate_left(30);
                b = a;
                a = t;
            }
            for (x, y) in h.iter_mut().zip([a, b, c, d, e]) {
                *x = x.wrapping_add(y);
            }
        }
        let mut out = [0u8; 20];
        for (i, x) in h.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&x.to_be_bytes());
        }
        out
    }

    fn service_sid(name: &str) -> String {
        let units: Vec<u8> = name
            .to_uppercase()
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .collect();
        let d = sha1(&units);
        let parts: Vec<String> = d
            .chunks(4)
            .map(|c| u32::from_le_bytes(c.try_into().unwrap()).to_string())
            .collect();
        format!("S-1-5-80-{}", parts.join("-"))
    }

    #[test]
    fn service_sid_is_derived_from_the_service_name() {
        // Known value (resolvable on every Windows host).
        assert_eq!(
            service_sid("TrustedInstaller"),
            "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464"
        );
        assert_eq!(service_sid(crate::SERVICE_NAME), SERVICE_SID);
        assert!(Trust::Service.dir_sddl().contains(SERVICE_SID));
        assert!(Trust::Service.file_sddl().contains(SERVICE_SID));
    }

    // ── FER-476: restricted service SID ─────────────────────────────────

    fn restricted() -> OwnedHandle {
        crate::win::write_restricted_token(&[
            SERVICE_SID,
            crate::win::SID_EVERYONE_STR,
            crate::win::SID_WRITE_RESTRICTED_STR,
        ])
        .unwrap()
    }

    /// Everything the service writes keeps working under a write-restricted
    /// token whose only useful restricting SID is the service SID: open
    /// (hold) the directory, config write/read, log append + rotation, and
    /// saved index save/load/remove.
    #[test]
    fn restricted_service_token_can_use_the_data_directory() {
        let (root, dd) = data("restricted");
        let path = dd.path().to_path_buf();
        drop(dd);
        let tok = restricted();
        let imp = crate::win::Impersonation::begin(&tok).unwrap();
        let dd = DataDir::open_for_service(&path, dev())
            .expect("open under restricted token")
            .expect("trusted");
        dd.write_config(&Config {
            mode: Mode::HighPerformance,
        })
        .unwrap();
        assert_eq!(
            dd.read_config().unwrap().unwrap().0.mode,
            Mode::HighPerformance
        );
        dd.append_log("restricted line\n").unwrap();
        // Rotation (rename-by-handle) under the restricted token.
        let big = "x".repeat(64 * 1024);
        for _ in 0..17 {
            dd.append_log(&big).unwrap();
        }
        dd.append_log("after rotation\n").unwrap();
        assert!(path.join(LOG_FILE_OLD).exists());
        let ix = dd.index_dir().unwrap().expect("index dir");
        ix.save(7, b"bytes").unwrap();
        assert_eq!(ix.load(7, 100).unwrap().as_deref(), Some(&b"bytes"[..]));
        ix.remove(7).unwrap();
        drop((ix, dd));
        drop(imp);
        let _ = std::fs::remove_dir_all(root);
    }

    /// Negative control: a directory sealed WITHOUT the service-SID ACE (as
    /// S1 created it) refuses writes from the restricted token — which is
    /// why `install` re-applies the SDs (`reseal`) before the service
    /// starts with `SERVICE_SID_TYPE_RESTRICTED`.
    #[test]
    fn restricted_token_needs_the_service_sid_ace() {
        let root = tmp("restricted-legacy");
        let user = sid_string(&current_user_sid().unwrap()).unwrap();
        let sd = SecurityDescriptor::from_sddl(&format!("D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;{user})"))
            .unwrap();
        let legacy = root.join("legacy");
        create_dir_sd(&legacy, &sd).unwrap();
        let tok = restricted();
        let imp = crate::win::Impersonation::begin(&tok).unwrap();
        let e = std::fs::write(legacy.join("x.txt"), b"x").unwrap_err();
        drop(imp);
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
        // The unrestricted token can.
        std::fs::write(legacy.join("x.txt"), b"x").unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    /// `reseal` re-applies the current SD to files created with an older
    /// one, so the restricted token can append to an S1-era log.
    #[test]
    fn reseal_updates_old_file_sds() {
        let (root, dd) = data("reseal");
        let user = sid_string(&current_user_sid().unwrap()).unwrap();
        let old =
            SecurityDescriptor::from_sddl(&format!("D:P(A;;FA;;;SY)(A;;FA;;;{user})")).unwrap();
        drop(
            open_raw(
                &dd.child(LOG_FILE),
                GENERIC_WRITE,
                0,
                CREATE_NEW,
                false,
                Some(&old),
            )
            .unwrap(),
        );
        let tok = restricted();
        {
            let _imp = crate::win::Impersonation::begin(&tok).unwrap();
            assert!(dd.append_log("denied\n").is_err(), "old SD: no service SID");
        }
        assert!(dd.reseal().unwrap() >= 1);
        {
            let _imp = crate::win::Impersonation::begin(&tok).unwrap();
            dd.append_log("allowed\n").unwrap();
        }
        drop(dd);
        let _ = std::fs::remove_dir_all(root);
    }
}
