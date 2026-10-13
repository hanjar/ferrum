//! Windows side of the index engine (FER-475): volume discovery, the
//! read-only volume backend (MFT, USN journal, file records), user
//! profiles, memory budget, background priority, saved-index storage.
//!
//! Every volume access is READ-ONLY: the volume is opened exactly like the
//! FER-460 helper (`GENERIC_READ`, full sharing) and only three FSCTLs are
//! issued — `FSCTL_QUERY_USN_JOURNAL`, `FSCTL_READ_USN_JOURNAL`,
//! `FSCTL_GET_NTFS_FILE_RECORD` (plus `FSCTL_GET_NTFS_VOLUME_DATA` inside
//! `ferrum_ntfs_scan`). The journal is never created, resized or deleted.
//! No file on the volume is opened, so nothing is hydrated, locked, or
//! read beyond MFT records.

use crate::engine::{JournalError, JournalInfo, Platform, VolumeBackend, VolumeId};
use crate::proto::VolumeErrorCode;
use crate::store::{self, IndexDir};
use crate::usn::reason;
use crate::win::wide;
use ferrum_ntfs_scan::helper::Volume;
use ferrum_ntfs_scan::mft::{VolumeGeometry, VolumeReader};
use std::io;
use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_HANDLE_EOF, ERROR_INVALID_PARAMETER,
    ERROR_JOURNAL_DELETE_IN_PROGRESS, ERROR_JOURNAL_ENTRY_DELETED, ERROR_JOURNAL_NOT_ACTIVE,
    ERROR_MORE_DATA, ERROR_NO_MORE_ITEMS, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS,
    INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_NAME_NORMALIZED, FILE_READ_ATTRIBUTES,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, GetDriveTypeW, GetFinalPathNameByHandleW,
    GetLogicalDrives, GetVolumeInformationW, GetVolumeNameForVolumeMountPointW,
    GetVolumePathNamesForVolumeNameW, OPEN_EXISTING, VOLUME_NAME_DOS,
};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::{
    FSCTL_GET_NTFS_FILE_RECORD, FSCTL_QUERY_USN_JOURNAL, FSCTL_READ_USN_JOURNAL,
    NTFS_FILE_RECORD_INPUT_BUFFER, READ_USN_JOURNAL_DATA_V0, USN_JOURNAL_DATA_V0,
};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_LOCAL_MACHINE, KEY_READ, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ, RegCloseKey,
    RegEnumKeyExW, RegGetValueW, RegOpenKeyExW,
};
use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows_sys::Win32::System::Threading::{
    GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN,
};

const DRIVE_FIXED: u32 = 3;
/// One journal read (the kernel fills as many whole records as fit).
const JOURNAL_BUFFER: usize = 64 * 1024;
/// Memory budget for all indexes: 1/8 of physical memory, at least
/// 256 MiB, at most 2 GiB (~100 bytes per entry → 2.5M..20M entries).
pub const BUDGET_MIN: u64 = 256 << 20;
pub const BUDGET_MAX: u64 = 2 << 30;

/// Local FIXED NTFS volumes with a drive letter. Removable, network,
/// optical and RAM drives, and other file systems are skipped.
pub fn fixed_ntfs_volumes() -> Vec<VolumeId> {
    let mask = unsafe { GetLogicalDrives() };
    let mut out = Vec::new();
    for i in 0..26u32 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let letter = (b'A' + i as u8) as char;
        let root = wide(format!("{letter}:\\"));
        if unsafe { GetDriveTypeW(root.as_ptr()) } != DRIVE_FIXED {
            continue;
        }
        let mut serial = 0u32;
        let mut fs = [0u16; 32];
        let ok = unsafe {
            GetVolumeInformationW(
                root.as_ptr(),
                std::ptr::null_mut(),
                0,
                &mut serial,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                fs.as_mut_ptr(),
                fs.len() as u32,
            )
        };
        if ok == 0 {
            continue;
        }
        let end = fs.iter().position(|&c| c == 0).unwrap_or(fs.len());
        if String::from_utf16_lossy(&fs[..end]) == "NTFS" {
            out.push(VolumeId {
                letter,
                serial: serial as u64,
            });
        }
    }
    out
}

fn win(code: u32) -> io::Error {
    io::Error::from_raw_os_error(code as i32)
}

/// The volume through the FER-460 helper's read-only handle.
pub struct WinBackend {
    vol: Volume,
    bytes_per_record: Option<usize>,
}

impl WinBackend {
    pub fn open(letter: char) -> Result<WinBackend, VolumeErrorCode> {
        match Volume::open(letter) {
            Ok(vol) => Ok(WinBackend {
                vol,
                bytes_per_record: None,
            }),
            Err(e) if e.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) => {
                Err(VolumeErrorCode::AccessDenied)
            }
            Err(_) => Err(VolumeErrorCode::OpenFailed),
        }
    }

    fn ioctl(&self, code: u32, input: &[u8], out: &mut [u8]) -> Result<usize, u32> {
        let mut returned = 0u32;
        let ok = unsafe {
            DeviceIoControl(
                self.vol.raw_handle(),
                code,
                input.as_ptr().cast(),
                input.len() as u32,
                out.as_mut_ptr().cast(),
                out.len() as u32,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error().raw_os_error().unwrap_or(0) as u32);
        }
        Ok((returned as usize).min(out.len()))
    }
}

fn as_bytes<T: Copy>(v: &T) -> &[u8] {
    // SAFETY: plain-old-data FFI structs (no padding read as values).
    unsafe { std::slice::from_raw_parts((v as *const T).cast::<u8>(), std::mem::size_of::<T>()) }
}

fn journal_error(code: u32) -> JournalError {
    match code {
        ERROR_JOURNAL_NOT_ACTIVE => JournalError::NotActive,
        ERROR_JOURNAL_ENTRY_DELETED => JournalError::Wrapped,
        // A stale journal ID is reported as an invalid parameter.
        ERROR_JOURNAL_DELETE_IN_PROGRESS | ERROR_INVALID_PARAMETER => JournalError::Changed,
        c => JournalError::Io(win(c)),
    }
}

impl VolumeBackend for WinBackend {
    fn geometry(&mut self) -> io::Result<VolumeGeometry> {
        let g = self.vol.geometry()?;
        self.bytes_per_record = Some(g.bytes_per_record as usize);
        Ok(g)
    }

    fn reader(&mut self) -> &mut dyn VolumeReader {
        &mut self.vol
    }

    fn query_journal(&mut self) -> Result<JournalInfo, JournalError> {
        let mut out = [0u8; std::mem::size_of::<USN_JOURNAL_DATA_V0>()];
        let n = self
            .ioctl(FSCTL_QUERY_USN_JOURNAL, &[], &mut out)
            .map_err(journal_error)?;
        if n < 24 {
            return Err(JournalError::Io(io::Error::other("short journal data")));
        }
        let rd = |o: usize| u64::from_le_bytes(out[o..o + 8].try_into().unwrap_or([0; 8]));
        Ok(JournalInfo {
            id: rd(0),
            first_usn: rd(8) as i64,
            next_usn: rd(16) as i64,
        })
    }

    fn read_journal(&mut self, journal_id: u64, start: i64) -> Result<Vec<u8>, JournalError> {
        let req = READ_USN_JOURNAL_DATA_V0 {
            StartUsn: start,
            ReasonMask: reason::READ_MASK,
            ReturnOnlyOnClose: 0,
            // Never block: the worker polls (pause/stop stay responsive).
            Timeout: 0,
            BytesToWaitFor: 0,
            UsnJournalID: journal_id,
        };
        let mut out = vec![0u8; JOURNAL_BUFFER];
        match self.ioctl(FSCTL_READ_USN_JOURNAL, as_bytes(&req), &mut out) {
            Ok(n) => {
                out.truncate(n);
                Ok(out)
            }
            // Reading at the very end of the journal.
            Err(ERROR_HANDLE_EOF) => Ok(start.to_le_bytes().to_vec()),
            Err(c) => Err(journal_error(c)),
        }
    }

    fn file_record(&mut self, record: u64) -> io::Result<Option<Vec<u8>>> {
        let bpr = match self.bytes_per_record {
            Some(b) => b,
            None => self.geometry()?.bytes_per_record as usize,
        };
        let req = NTFS_FILE_RECORD_INPUT_BUFFER {
            FileReferenceNumber: (record & crate::index::RECORD_MASK) as i64,
        };
        // Output: FileReferenceNumber i64, FileRecordLength u32, record.
        let mut out = vec![0u8; 16 + bpr];
        let n = match self.ioctl(FSCTL_GET_NTFS_FILE_RECORD, as_bytes(&req), &mut out) {
            Ok(n) => n,
            Err(ERROR_MORE_DATA) => return Ok(None),
            Err(c) => return Err(win(c)),
        };
        if n < 12 {
            return Ok(None);
        }
        let got = u64::from_le_bytes(out[0..8].try_into().unwrap_or([0; 8]));
        let len = u32::from_le_bytes(out[8..12].try_into().unwrap_or([0; 4])) as usize;
        // NTFS returns the nearest LOWER in-use record when `record` is free:
        // that is not our record.
        if got & crate::index::RECORD_MASK != record & crate::index::RECORD_MASK
            || len == 0
            || len > bpr
            || 12 + len > n
        {
            return Ok(None);
        }
        Ok(Some(out[12..12 + len].to_vec()))
    }
}

/// `HKLM\...\ProfileList`.
const PROFILE_LIST: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\ProfileList";

/// Review L1: change notification on `ProfileList` (a new profile is
/// excluded at the next worker poll, ~1 s, instead of the next 5-minute
/// discovery). Thread-agnostic registration, re-armed after every signal.
/// When it cannot be set up, `changed` stays false and the periodic
/// discovery plus the re-read before every save still apply.
pub struct ProfileWatch {
    key: HKEY,
    event: crate::win::OwnedHandle,
}

// SAFETY: the registry key handle is only used by the owning worker.
unsafe impl Send for ProfileWatch {}

impl ProfileWatch {
    pub fn new() -> Option<ProfileWatch> {
        use windows_sys::Win32::System::Registry::KEY_NOTIFY;
        use windows_sys::Win32::System::Threading::CreateEventW;
        let mut key: HKEY = std::ptr::null_mut();
        let path = wide(PROFILE_LIST);
        if unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, path.as_ptr(), 0, KEY_NOTIFY, &mut key) }
            != ERROR_SUCCESS
        {
            return None;
        }
        let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        if event.is_null() {
            unsafe { RegCloseKey(key) };
            return None;
        }
        let w = ProfileWatch {
            key,
            event: crate::win::OwnedHandle(event),
        };
        w.arm().then_some(w)
    }

    fn arm(&self) -> bool {
        use windows_sys::Win32::System::Registry::{
            REG_NOTIFY_CHANGE_LAST_SET, REG_NOTIFY_CHANGE_NAME, REG_NOTIFY_THREAD_AGNOSTIC,
            RegNotifyChangeKeyValue,
        };
        let rc = unsafe {
            RegNotifyChangeKeyValue(
                self.key,
                1,
                REG_NOTIFY_CHANGE_NAME | REG_NOTIFY_CHANGE_LAST_SET | REG_NOTIFY_THREAD_AGNOSTIC,
                self.event.raw(),
                1,
            )
        };
        rc == ERROR_SUCCESS
    }

    /// Non-blocking: has `ProfileList` changed since the last call?
    pub fn changed(&mut self) -> bool {
        use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
        use windows_sys::Win32::System::Threading::{ResetEvent, WaitForSingleObject};
        if unsafe { WaitForSingleObject(self.event.raw(), 0) } != WAIT_OBJECT_0 {
            return false;
        }
        unsafe { ResetEvent(self.event.raw()) };
        // Re-arm; if that fails, report a change every time (safe side:
        // more rediscoveries, never a missed profile).
        if !self.arm() {
            unsafe { windows_sys::Win32::System::Threading::SetEvent(self.event.raw()) };
        }
        true
    }
}

impl Drop for ProfileWatch {
    fn drop(&mut self) {
        unsafe { RegCloseKey(self.key) };
    }
}

/// Longest profile path accepted (UTF-16 units, the Win32 path limit).
const MAX_PROFILE_PATH: usize = 32_767;
/// Most `ProfileList` entries read (more = fail closed).
const MAX_PROFILES: u32 = 4096;

/// `ProfileImagePath` of every `ProfileList` entry (expanded). Fails
/// (review M2) when the key cannot be opened, an entry cannot be
/// enumerated or read, a path is too long, or there are too many entries —
/// never a silently shortened list. Errors carry no path.
pub fn user_profiles() -> Result<Vec<String>, &'static str> {
    let mut key: HKEY = std::ptr::null_mut();
    let path = wide(PROFILE_LIST);
    if unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, path.as_ptr(), 0, KEY_READ, &mut key) }
        != ERROR_SUCCESS
    {
        return Err("ProfileList not readable");
    }
    struct Key(HKEY);
    impl Drop for Key {
        fn drop(&mut self) {
            unsafe { RegCloseKey(self.0) };
        }
    }
    let key = Key(key);
    let mut out = Vec::new();
    let value = wide("ProfileImagePath");
    let mut buf = vec![0u16; MAX_PROFILE_PATH + 1];
    for i in 0..=MAX_PROFILES {
        let mut name = [0u16; 256];
        let mut len = name.len() as u32;
        let rc = unsafe {
            RegEnumKeyExW(
                key.0,
                i,
                name.as_mut_ptr(),
                &mut len,
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if rc == ERROR_NO_MORE_ITEMS {
            return Ok(out);
        }
        if rc != ERROR_SUCCESS {
            return Err("ProfileList entry not enumerable");
        }
        if i == MAX_PROFILES {
            return Err("too many ProfileList entries");
        }
        let mut sub: Vec<u16> = name[..len as usize].to_vec();
        sub.push(0);
        let mut bytes = (buf.len() * 2) as u32;
        let rc = unsafe {
            RegGetValueW(
                key.0,
                sub.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ,
                std::ptr::null_mut(),
                buf.as_mut_ptr().cast(),
                &mut bytes,
            )
        };
        if rc != ERROR_SUCCESS {
            return Err("ProfileImagePath not readable");
        }
        let units = (bytes as usize / 2).min(buf.len());
        let end = buf[..units].iter().position(|&c| c == 0).unwrap_or(units);
        if end == 0 {
            return Err("empty ProfileImagePath");
        }
        out.push(String::from_utf16_lossy(&buf[..end]));
    }
    Err("too many ProfileList entries")
}

/// Final path of an existing directory (junctions, symlinks and mount
/// points followed), e.g. `D:\Users\bob` for `C:\Users\bob` when
/// `C:\Users` is a junction. `Ok(None)` = the path does not exist.
/// Only attributes are opened (`FILE_READ_ATTRIBUTES`); nothing is read.
pub fn final_path(path: &str) -> io::Result<Option<String>> {
    let w = wide(path);
    let h = unsafe {
        CreateFileW(
            w.as_ptr(),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    };
    if h == INVALID_HANDLE_VALUE {
        let e = io::Error::last_os_error();
        return match e.raw_os_error().map(|c| c as u32) {
            Some(ERROR_FILE_NOT_FOUND) | Some(ERROR_PATH_NOT_FOUND) => Ok(None),
            _ => Err(e),
        };
    }
    let h = crate::win::OwnedHandle(h);
    let mut buf = vec![0u16; MAX_PROFILE_PATH + 1];
    let n = unsafe {
        GetFinalPathNameByHandleW(
            h.raw(),
            buf.as_mut_ptr(),
            buf.len() as u32,
            FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
        )
    } as usize;
    if n == 0 || n >= buf.len() {
        return Err(io::Error::last_os_error());
    }
    let s = String::from_utf16_lossy(&buf[..n]);
    Ok(Some(s.strip_prefix(r"\\?\").unwrap_or(&s).to_string()))
}

/// Every DOS path a lettered volume is mounted at (`E:\`, `C:\mnt\e\`).
fn mount_paths(letter: char) -> io::Result<Vec<String>> {
    let root = wide(format!("{letter}:\\"));
    let mut name = [0u16; 64];
    if unsafe { GetVolumeNameForVolumeMountPointW(root.as_ptr(), name.as_mut_ptr(), 64) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut buf = vec![0u16; 64 * 1024];
    let mut len = 0u32;
    if unsafe {
        GetVolumePathNamesForVolumeNameW(
            name.as_ptr(),
            buf.as_mut_ptr(),
            buf.len() as u32,
            &mut len,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let used = (len as usize).min(buf.len());
    Ok(buf[..used]
        .split(|&c| c == 0)
        .filter(|p| !p.is_empty())
        .map(String::from_utf16_lossy)
        .collect())
}

/// Is `path` equal to or below `dir` (case-insensitive, component-wise)?
pub fn is_under(path: &str, dir: &str) -> bool {
    let p = path.trim_end_matches('\\').to_lowercase();
    let d = dir.trim_end_matches('\\').to_lowercase();
    !d.is_empty() && (p == d || p.starts_with(&format!("{d}\\")))
}

/// Shared-mode exclusion list (review M2/M3): every literal profile path,
/// its final path, and the root of each fixed volume that is mounted
/// somewhere inside a profile. Fails closed on any read/resolve error.
pub fn profile_exclusions(volumes: &[VolumeId]) -> Result<Vec<String>, &'static str> {
    let literal = user_profiles()?;
    let mut out = Vec::with_capacity(literal.len() * 2);
    for p in literal {
        match final_path(&p) {
            // A stale ProfileList entry: nothing on disk to protect; the
            // literal path stays excluded should it be created later.
            Ok(None) => {}
            Ok(Some(f)) => {
                if !is_under(&f, &p) || !is_under(&p, &f) {
                    out.push(f);
                }
            }
            Err(_) => return Err("profile path not resolvable"),
        }
        out.push(p);
    }
    for v in volumes {
        let mounts = mount_paths(v.letter).map_err(|_| "volume mount points not readable")?;
        let root = format!("{}:\\", v.letter);
        if mounts
            .iter()
            .filter(|m| !m.eq_ignore_ascii_case(&root))
            .any(|m| out.iter().any(|p| is_under(m, p)))
        {
            out.push(root);
        }
    }
    Ok(out)
}

/// 1/8 of physical memory, clamped to [`BUDGET_MIN`]..=[`BUDGET_MAX`].
pub fn memory_budget() -> u64 {
    let mut m: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    m.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
    let total = if unsafe { GlobalMemoryStatusEx(&mut m) } != 0 {
        m.ullTotalPhys
    } else {
        0
    };
    (total / 8).clamp(BUDGET_MIN, BUDGET_MAX)
}

/// The production platform. `index_dir` = `None` → no saved indexes (data
/// directory unavailable/untrusted, or console mode without one).
pub struct WinPlatform {
    pub index_dir: Option<IndexDir>,
    pub budget: u64,
    profiles: Option<ProfileWatch>,
}

impl WinPlatform {
    pub fn new(index_dir: Option<IndexDir>) -> WinPlatform {
        let profiles = ProfileWatch::new();
        if profiles.is_none() {
            store::log("ProfileList change notification unavailable; periodic discovery only");
        }
        WinPlatform {
            index_dir,
            budget: memory_budget(),
            profiles,
        }
    }
}

impl Platform for WinPlatform {
    fn volumes(&mut self) -> Vec<VolumeId> {
        fixed_ntfs_volumes()
    }

    fn open(&mut self, v: &VolumeId) -> Result<Box<dyn VolumeBackend>, VolumeErrorCode> {
        WinBackend::open(v.letter).map(|b| Box::new(b) as Box<dyn VolumeBackend>)
    }

    fn profiles(&mut self) -> Result<Vec<String>, &'static str> {
        profile_exclusions(&fixed_ntfs_volumes())
    }

    fn profiles_changed(&mut self) -> bool {
        self.profiles.as_mut().is_some_and(ProfileWatch::changed)
    }

    fn load(&mut self, v: &VolumeId) -> Option<Vec<u8>> {
        let dir = self.index_dir.as_ref()?;
        match dir.load(v.serial, self.budget) {
            Ok(b) => b,
            Err(e) => {
                store::log(&format!("{}: index load failed: {e}", v.letter));
                None
            }
        }
    }

    fn save(&mut self, v: &VolumeId, bytes: &[u8]) -> io::Result<()> {
        match &self.index_dir {
            Some(d) => d.save(v.serial, bytes),
            None => Ok(()),
        }
    }

    fn remove(&mut self, v: &VolumeId) -> bool {
        match &self.index_dir {
            Some(d) => d.remove(v.serial).is_ok(),
            None => true,
        }
    }

    fn remove_all(&mut self) {
        if let Some(d) = &self.index_dir
            && let Err(e) = d.remove_all()
        {
            store::log(&format!("removing saved indexes failed: {e}"));
        }
    }

    fn memory_budget(&self) -> u64 {
        self.budget
    }

    fn enter_background(&mut self) {
        // Lowest CPU, I/O and memory priority for this thread.
        unsafe { SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN) };
    }

    fn log(&mut self, msg: &str) {
        store::log(msg);
    }
}

/// Start the index worker thread. `data` = the verified data directory
/// (saved indexes go to its `index` subdirectory); `None` = memory only.
pub fn start_worker(
    data: Option<&store::DataDir>,
    shared: std::sync::Arc<crate::engine::IndexShared>,
) -> io::Result<std::thread::JoinHandle<()>> {
    let index_dir = match data.map(|d| d.index_dir()) {
        Some(Ok(Some(d))) => Some(d),
        Some(Ok(None)) => {
            store::log("index directory not trusted; indexes are not saved");
            None
        }
        Some(Err(e)) => {
            store::log(&format!(
                "index directory unusable ({e}); indexes are not saved"
            ));
            None
        }
        None => None,
    };
    std::thread::Builder::new()
        .name("ferrum-indexer-index".into())
        .spawn(move || {
            let mut platform = WinPlatform::new(index_dir);
            crate::engine::run(&mut platform, &shared, crate::engine::Timings::default());
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_finds_only_fixed_ntfs() {
        // The system drive of a dev/CI machine is fixed NTFS.
        let vols = fixed_ntfs_volumes();
        let sys = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
        let letter = sys.chars().next().unwrap().to_ascii_uppercase();
        assert!(vols.iter().any(|v| v.letter == letter), "{vols:?}");
    }

    #[test]
    fn budget_is_clamped() {
        let b = memory_budget();
        assert!((BUDGET_MIN..=BUDGET_MAX).contains(&b));
    }

    #[test]
    fn profiles_are_read_without_admin() {
        // ProfileList is readable by Users; the current user's profile is
        // listed, and the full exclusion list resolves.
        let p = user_profiles().expect("ProfileList");
        let me = std::env::var("USERPROFILE")
            .unwrap_or_default()
            .to_lowercase();
        assert!(p.iter().any(|x| x.to_lowercase() == me), "{p:?}");
        // The own profile always resolves.
        let mine = final_path(&me).unwrap().unwrap();
        assert!(is_under(&mine, &me) && is_under(&me, &mine), "{mine}");
        // The full list resolves as SYSTEM (the service). A standard user is
        // denied e.g. `system32\config\systemprofile`: then the whole list
        // FAILS (closed) instead of skipping that profile.
        match profile_exclusions(&fixed_ntfs_volumes()) {
            Ok(ex) => assert!(ex.iter().any(|x| x.to_lowercase() == me), "{ex:?}"),
            Err(why) => {
                assert!(!crate::win::running_as_system());
                assert_eq!(why, "profile path not resolvable");
            }
        }
    }

    #[test]
    fn final_path_follows_junctions() {
        let root = std::env::temp_dir().join(format!("ferrum-idx-fp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let target = root.join("real").join("bob");
        std::fs::create_dir_all(&target).unwrap();
        let link = root.join("Users");
        let ok = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(root.join("real"))
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        assert!(ok, "mklink /J needs no privilege");
        let via = link.join("bob");
        let f = final_path(via.to_str().unwrap()).unwrap().unwrap();
        let real = final_path(target.to_str().unwrap()).unwrap().unwrap();
        assert!(f.eq_ignore_ascii_case(&real), "{f} vs {real}");
        assert!(f.to_lowercase().ends_with(r"\real\bob"), "{f}");
        assert!(!f.starts_with(r"\\?\"), "{f}");
        assert_eq!(
            final_path(root.join("missing").to_str().unwrap()).unwrap(),
            None
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn is_under_is_component_wise() {
        assert!(is_under(r"C:\Users\bob\Data\", r"C:\Users\bob"));
        assert!(is_under(r"c:\users\BOB", r"C:\Users\bob\"));
        assert!(!is_under(r"C:\Users\bobby", r"C:\Users\bob"));
        assert!(!is_under(r"C:\Users", r"C:\Users\bob"));
        assert!(!is_under(r"C:\x", ""));
    }

    #[test]
    fn mount_paths_list_the_drive_root() {
        let sys = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
        let letter = sys.chars().next().unwrap().to_ascii_uppercase();
        let m = mount_paths(letter).unwrap();
        let root = format!("{letter}:\\");
        assert!(m.iter().any(|p| p.eq_ignore_ascii_case(&root)), "{m:?}");
    }

    #[test]
    fn profile_watch_arms_without_admin() {
        // KEY_NOTIFY on ProfileList is granted to Users; nothing changes
        // during the test, so no change is reported.
        let mut w = ProfileWatch::new().expect("ProfileList notification");
        assert!(!w.changed());
    }

    #[test]
    fn non_admin_cannot_open_the_volume() {
        if crate::win::process_elevated() {
            return;
        }
        let sys = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
        let letter = sys.chars().next().unwrap();
        assert_eq!(
            WinBackend::open(letter).err(),
            Some(VolumeErrorCode::AccessDenied)
        );
    }
}
