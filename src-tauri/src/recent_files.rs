//! FER-598: "Recent files" sources that live outside Ferrum's own Activity history.
//!
//! 1. Windows Recent (`FOLDERID_Recent`, `*.lnk`): files the user opened in OTHER apps.
//! 2. Newly added top-level items of chosen folders (Downloads by default), by creation time.
//!
//! SECURITY: a `.lnk` is attacker-influenced data (it can be dropped by any app the user runs).
//! The reader therefore
//! - only calls `IPersistFile::Load` + `IShellLinkW::GetPath`; it NEVER calls
//!   `IShellLinkW::Resolve` (Resolve may search the disk, show UI or touch the network);
//! - runs on a blocking worker thread that initialises and uninitialises COM itself;
//! - skips an item that fails instead of failing the list;
//! - drops empty / relative / device (`\\?\`, `\\.\`) / `shell:` / CLSID / over-long targets and
//!   targets inside Ferrum's own data folders or an Activity exclusion;
//! - never stats a target (a stat of a dead network path can block for a long time); the
//!   directory flag comes from the link's cached find data.
//!
//! The filtering, ordering and window logic is pure and unit-tested; the COM part is
//! `cfg(windows)` with an integration test that writes real `.lnk` files.

use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Hard cap on the number of items any command returns.
pub const MAX_LIMIT: usize = 200;
/// Per watched folder cap for new items.
pub const MAX_PER_FOLDER: usize = 50;
/// At most this many watched folders are scanned per call.
pub const MAX_FOLDERS: usize = 16;
/// A target longer than this (in UTF-16 units) is dropped.
pub const MAX_TARGET_LEN: usize = 1024;
/// A `.lnk` bigger than this is not loaded (real ones are a few KiB).
const MAX_LNK_BYTES: u64 = 256 * 1024;
/// Upper bound on directory entries examined in one folder (bounded work on huge folders).
const MAX_SCAN_ENTRIES: usize = 20_000;
/// Total time budget for the watched-folder scan; a slow folder is skipped.
const SCAN_BUDGET: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentFile {
    pub path: String,
    pub name: String,
    /// `None` when unknown (never derived from a blocking stat).
    pub is_dir: Option<bool>,
    pub last_used_ms: i64,
    /// `"windows"` (Windows Recent) or `"new"` (newly added item of a watched folder).
    pub source: &'static str,
    /// Display label: `"Windows"`, `"Downloads"` or the watched folder's name.
    pub label: String,
}

// ── pure helpers ─────────────────────────────────────────────────────────────

fn norm_key(path: &str) -> String {
    path.replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase()
}

/// True when `path` equals `root` or lies below it (case-insensitive, component boundary).
pub fn path_under(path: &str, root: &str) -> bool {
    let p = norm_key(path);
    let r = norm_key(root);
    if r.is_empty() {
        return false;
    }
    p == r || (p.starts_with(&r) && p.as_bytes().get(r.len()) == Some(&b'\\'))
}

fn is_drive_abs(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/')
}

/// `\\server\share[\...]` with a real server and share (device paths `\\?\`, `\\.\` excluded).
fn is_unc_abs(s: &str) -> bool {
    let Some(rest) = s.strip_prefix("\\\\") else {
        return false;
    };
    let mut parts = rest.split('\\');
    let server = parts.next().unwrap_or("");
    let share = parts.next().unwrap_or("");
    !server.is_empty() && server != "?" && server != "." && !share.is_empty()
}

/// Validate and normalise an absolute path coming from outside (a link target or a Settings
/// entry). `None` = reject. Blocked roots are Ferrum's own data folders and Activity exclusions.
pub fn sanitize_abs_path(raw: &str, blocked_roots: &[String]) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.encode_utf16().count() > MAX_TARGET_LEN {
        return None;
    }
    if trimmed
        .chars()
        .any(|c| c.is_control() || "*?<>|\"".contains(c))
    {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("shell:") || trimmed.starts_with("::") || trimmed.contains("::{") {
        return None;
    }
    if trimmed.starts_with("\\\\?\\")
        || trimmed.starts_with("\\\\.\\")
        || trimmed.starts_with("//?/")
        || trimmed.starts_with("//./")
    {
        return None;
    }
    // An unexpanded environment variable means the link was not resolved to a real path.
    if trimmed.contains('%') {
        return None;
    }
    let unified = trimmed.replace('/', "\\");
    // `:` is only valid as the drive separator (blocks NTFS alternate streams and URL schemes).
    let after_drive = if is_drive_abs(&unified) {
        &unified[2..]
    } else {
        &unified[..]
    };
    if after_drive.contains(':') {
        return None;
    }
    if !(is_drive_abs(&unified) || is_unc_abs(&unified)) {
        return None;
    }
    if unified.split('\\').any(|c| c == "..") {
        return None;
    }
    // Keep a drive root as `C:\`, trim other trailing separators.
    let out = if unified.len() == 3 && is_drive_abs(&unified) {
        unified
    } else {
        unified.trim_end_matches('\\').to_owned()
    };
    if blocked_roots.iter().any(|root| path_under(&out, root)) {
        return None;
    }
    Some(out)
}

/// Last path component (a drive root keeps `C:\`).
pub fn display_name(path: &str) -> String {
    let t = path.trim_end_matches('\\');
    match t.rsplit('\\').next() {
        Some(last) if !last.is_empty() && !(last.len() == 2 && last.ends_with(':')) => {
            last.to_owned()
        }
        _ => path.to_owned(),
    }
}

/// A shortcut file candidate: path plus its last-write time.
#[derive(Debug, Clone, PartialEq)]
pub struct LnkCandidate {
    pub path: PathBuf,
    pub modified_ms: i64,
}

/// Newest first (ties by path for a stable order), truncated to `limit`.
pub fn pick_newest(mut candidates: Vec<LnkCandidate>, limit: usize) -> Vec<LnkCandidate> {
    candidates.sort_by(|a, b| {
        b.modified_ms
            .cmp(&a.modified_ms)
            .then_with(|| a.path.cmp(&b.path))
    });
    candidates.truncate(limit.min(MAX_LIMIT));
    candidates
}

/// Collapse items with the same path (case-insensitive), keeping the newest; result newest first.
pub fn dedupe_newest(items: Vec<RecentFile>) -> Vec<RecentFile> {
    let mut best: std::collections::HashMap<String, RecentFile> = std::collections::HashMap::new();
    for item in items {
        let key = norm_key(&item.path);
        match best.get(&key) {
            Some(cur) if cur.last_used_ms >= item.last_used_ms => {}
            _ => {
                best.insert(key, item);
            }
        }
    }
    let mut out: Vec<RecentFile> = best.into_values().collect();
    out.sort_by(|a, b| {
        b.last_used_ms
            .cmp(&a.last_used_ms)
            .then_with(|| a.path.cmp(&b.path))
    });
    out
}

/// `created_ms` is inside the window `[now - days, now + 1 day]` (a small future skew is fine).
pub fn within_window(created_ms: i64, now_ms: i64, max_age_days: u32) -> bool {
    let age_ms = i64::from(max_age_days.clamp(1, 365)) * 86_400_000;
    created_ms >= now_ms - age_ms && created_ms <= now_ms + 86_400_000
}

/// Names never shown as "new items": shell metadata and partial downloads.
pub fn is_noise_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower == "desktop.ini"
        || lower == "thumbs.db"
        || lower.starts_with("~$")
        || [".crdownload", ".part", ".partial", ".tmp", ".download"]
            .iter()
            .any(|ext| lower.ends_with(ext))
}

/// Sort newest first and apply the per-folder cap.
pub fn sort_and_cap(mut items: Vec<RecentFile>, cap: usize) -> Vec<RecentFile> {
    items.sort_by(|a, b| {
        b.last_used_ms
            .cmp(&a.last_used_ms)
            .then_with(|| a.path.cmp(&b.path))
    });
    items.truncate(cap);
    items
}

fn ms_of(t: SystemTime) -> i64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_millis()).unwrap_or(i64::MAX),
        Err(e) => -i64::try_from(e.duration().as_millis()).unwrap_or(i64::MAX),
    }
}

pub fn now_ms() -> i64 {
    ms_of(SystemTime::now())
}

// ── Windows Recent ───────────────────────────────────────────────────────────

/// Enumerate `*.lnk` files (top level only), newest `limit` by last-write time.
pub fn newest_lnk_files(dir: &Path, limit: usize) -> Vec<LnkCandidate> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in read.take(MAX_SCAN_ENTRIES).flatten() {
        let path = entry.path();
        let is_lnk = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("lnk"));
        if !is_lnk {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() || meta.len() > MAX_LNK_BYTES {
            continue;
        }
        let Ok(modified) = meta.modified() else {
            continue;
        };
        out.push(LnkCandidate {
            path,
            modified_ms: ms_of(modified),
        });
    }
    pick_newest(out, limit)
}

/// Read the Windows Recent folder `dir`: resolve the newest `limit` shortcuts (without
/// `Resolve`), filter, de-duplicate. Blocking; call from a worker thread.
pub fn read_windows_recent(dir: &Path, blocked_roots: &[String], limit: usize) -> Vec<RecentFile> {
    let limit = limit.clamp(1, MAX_LIMIT);
    let candidates = newest_lnk_files(dir, limit);
    if candidates.is_empty() {
        return Vec::new();
    }
    let targets = read_link_targets(&candidates);
    let mut items = Vec::new();
    for (cand, target) in candidates.iter().zip(targets) {
        let Some(target) = target else { continue };
        let Some(path) = sanitize_abs_path(&target.path, blocked_roots) else {
            continue;
        };
        items.push(RecentFile {
            name: display_name(&path),
            path,
            is_dir: target.is_dir,
            last_used_ms: cand.modified_ms,
            source: "windows",
            label: "Windows".to_owned(),
        });
    }
    let mut items = dedupe_newest(items);
    items.truncate(limit);
    items
}

/// What a shortcut says about its target (cached data only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkTarget {
    pub path: String,
    pub is_dir: Option<bool>,
}

/// One entry per candidate, `None` where the link could not be read.
#[cfg(windows)]
fn read_link_targets(candidates: &[LnkCandidate]) -> Vec<Option<LinkTarget>> {
    com::with_apartment(|| {
        candidates
            .iter()
            .map(|c| com::read_target(&c.path))
            .collect::<Vec<_>>()
    })
    .unwrap_or_else(|| vec![None; candidates.len()])
}

#[cfg(not(windows))]
fn read_link_targets(candidates: &[LnkCandidate]) -> Vec<Option<LinkTarget>> {
    vec![None; candidates.len()]
}

#[cfg(windows)]
pub(crate) mod com {
    use super::LinkTarget;
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use windows_sys::Win32::Storage::FileSystem::WIN32_FIND_DATAW;
    use windows_sys::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
        CoUninitialize,
    };
    use windows_sys::core::GUID;

    // CLSID_ShellLink {00021401-0000-0000-C000-000000000046}
    const CLSID_SHELL_LINK: GUID = GUID {
        data1: 0x0002_1401,
        data2: 0,
        data3: 0,
        data4: [0xC0, 0, 0, 0, 0, 0, 0, 0x46],
    };
    // IID_IShellLinkW {000214F9-0000-0000-C000-000000000046}
    const IID_ISHELL_LINK_W: GUID = GUID {
        data1: 0x0002_14F9,
        data2: 0,
        data3: 0,
        data4: [0xC0, 0, 0, 0, 0, 0, 0, 0x46],
    };
    // IID_IPersistFile {0000010b-0000-0000-C000-000000000046}
    const IID_IPERSIST_FILE: GUID = GUID {
        data1: 0x0000_010B,
        data2: 0,
        data3: 0,
        data4: [0xC0, 0, 0, 0, 0, 0, 0, 0x46],
    };

    const SLGP_RAWPATH: u32 = 0x4;
    const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
    const STGM_READ: u32 = 0;
    const MAX_PATH_BUF: usize = 32_768;

    // vtable slots. IUnknown 0..=2; IShellLinkW follows; IPersistFile follows IPersist.
    const SLOT_RELEASE: usize = 2;
    const SLOT_SL_GET_PATH: usize = 3;
    #[cfg(test)]
    const SLOT_SL_SET_PATH: usize = 20;
    const SLOT_QUERY_INTERFACE: usize = 0;
    const SLOT_PF_LOAD: usize = 5;
    #[cfg(test)]
    const SLOT_PF_SAVE: usize = 6;

    /// An owned COM interface pointer, released on drop.
    pub(crate) struct Iface(*mut c_void);

    impl Iface {
        /// SAFETY: `self.0` is a valid interface pointer and `slot` is a method of that
        /// interface's vtable whose real signature is a `system` fn taking `*mut c_void` first.
        unsafe fn slot(&self, slot: usize) -> *const usize {
            // SAFETY: first word of a COM object is its vtable pointer.
            unsafe {
                let vtbl = *(self.0 as *const *const usize);
                vtbl.add(slot)
            }
        }
    }

    impl Drop for Iface {
        fn drop(&mut self) {
            if self.0.is_null() {
                return;
            }
            // SAFETY: Release is vtable slot 2 of every COM interface.
            unsafe {
                let f: unsafe extern "system" fn(*mut c_void) -> u32 =
                    core::mem::transmute(*self.slot(SLOT_RELEASE));
                f(self.0);
            }
        }
    }

    /// Run `f` with COM initialised on THIS thread (apartment-threaded), then uninitialise.
    /// `None` when COM could not be initialised.
    pub(crate) fn with_apartment<R>(f: impl FnOnce() -> R) -> Option<R> {
        // SAFETY: plain init/uninit pair on the calling thread; Uninitialize only after success
        // (S_OK or S_FALSE both need it; a failure such as RPC_E_CHANGED_MODE does not).
        let hr = unsafe { CoInitializeEx(std::ptr::null_mut(), COINIT_APARTMENTTHREADED as u32) };
        if hr < 0 {
            return None;
        }
        let out = f();
        unsafe { CoUninitialize() };
        Some(out)
    }

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }

    pub(crate) fn new_shell_link() -> Option<Iface> {
        let mut raw: *mut c_void = std::ptr::null_mut();
        // SAFETY: out pointer receives a new reference on success.
        let hr = unsafe {
            CoCreateInstance(
                &CLSID_SHELL_LINK,
                std::ptr::null_mut(),
                CLSCTX_INPROC_SERVER,
                &IID_ISHELL_LINK_W,
                &mut raw,
            )
        };
        (hr >= 0 && !raw.is_null()).then_some(Iface(raw))
    }

    fn persist_file(link: &Iface) -> Option<Iface> {
        let mut raw: *mut c_void = std::ptr::null_mut();
        // SAFETY: QueryInterface is slot 0: (this, riid, out) -> HRESULT.
        let hr = unsafe {
            let f: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> i32 =
                core::mem::transmute(*link.slot(SLOT_QUERY_INTERFACE));
            f(link.0, &IID_IPERSIST_FILE, &mut raw)
        };
        (hr >= 0 && !raw.is_null()).then_some(Iface(raw))
    }

    fn get_path(link: &Iface, flags: u32) -> Option<(String, u32)> {
        let mut buf = vec![0u16; MAX_PATH_BUF];
        // SAFETY: WIN32_FIND_DATAW is plain data; GetPath fills at most `cch` UTF-16 units.
        let mut find: WIN32_FIND_DATAW = unsafe { core::mem::zeroed() };
        let hr = unsafe {
            let f: unsafe extern "system" fn(
                *mut c_void,
                *mut u16,
                i32,
                *mut WIN32_FIND_DATAW,
                u32,
            ) -> i32 = core::mem::transmute(*link.slot(SLOT_SL_GET_PATH));
            f(link.0, buf.as_mut_ptr(), buf.len() as i32, &mut find, flags)
        };
        if hr != 0 {
            return None; // S_FALSE = no path (shell item / special folder), or an error
        }
        let len = buf.iter().position(|&c| c == 0)?;
        if len == 0 {
            return None;
        }
        Some((String::from_utf16(&buf[..len]).ok()?, find.dwFileAttributes))
    }

    /// Load the `.lnk` at `path` and return its stored target. NEVER calls `Resolve`.
    pub(crate) fn read_target(path: &Path) -> Option<LinkTarget> {
        let link = new_shell_link()?;
        let file = persist_file(&link)?;
        let w = wide(path);
        // SAFETY: IPersistFile::Load(this, LPCOLESTR, DWORD) -> HRESULT.
        let hr = unsafe {
            let f: unsafe extern "system" fn(*mut c_void, *const u16, u32) -> i32 =
                core::mem::transmute(*file.slot(SLOT_PF_LOAD));
            f(file.0, w.as_ptr(), STGM_READ)
        };
        if hr < 0 {
            return None;
        }
        // Raw path first (no expansion, no disk access); fall back to the default form, which
        // still only reads stored data (it is NOT Resolve).
        let (path, attrs) = get_path(&link, SLGP_RAWPATH).or_else(|| get_path(&link, 0))?;
        let is_dir = (attrs != 0).then_some(attrs & FILE_ATTRIBUTE_DIRECTORY != 0);
        Some(LinkTarget { path, is_dir })
    }

    /// Test helper: write a `.lnk` pointing at `target`.
    #[cfg(test)]
    pub(crate) fn write_link(lnk: &Path, target: &str) -> bool {
        let Some(link) = new_shell_link() else {
            return false;
        };
        let t: Vec<u16> = target.encode_utf16().chain(Some(0)).collect();
        // SAFETY: SetPath(this, LPCWSTR) -> HRESULT.
        let hr = unsafe {
            let f: unsafe extern "system" fn(*mut c_void, *const u16) -> i32 =
                core::mem::transmute(*link.slot(SLOT_SL_SET_PATH));
            f(link.0, t.as_ptr())
        };
        if hr < 0 {
            return false;
        }
        let Some(file) = persist_file(&link) else {
            return false;
        };
        let w = wide(lnk);
        // SAFETY: IPersistFile::Save(this, LPCOLESTR, BOOL) -> HRESULT.
        let hr = unsafe {
            let f: unsafe extern "system" fn(*mut c_void, *const u16, i32) -> i32 =
                core::mem::transmute(*file.slot(SLOT_PF_SAVE));
            f(file.0, w.as_ptr(), 1)
        };
        hr >= 0
    }
}

#[cfg(windows)]
pub fn windows_recent_dir() -> Option<PathBuf> {
    crate::profile::known_folder(&windows_sys::Win32::UI::Shell::FOLDERID_Recent)
}

#[cfg(not(windows))]
pub fn windows_recent_dir() -> Option<PathBuf> {
    None
}

#[cfg(windows)]
pub fn downloads_dir() -> Option<PathBuf> {
    crate::profile::known_folder(&windows_sys::Win32::UI::Shell::FOLDERID_Downloads)
}

#[cfg(not(windows))]
pub fn downloads_dir() -> Option<PathBuf> {
    None
}

// ── newly added items of watched folders ────────────────────────────────────

/// Scan one folder's top level. Pure I/O on a worker; no recursion, no stat of children beyond
/// the directory entry's own cached metadata.
pub fn scan_new_items(
    folder: &str,
    label: &str,
    now_ms: i64,
    max_age_days: u32,
    blocked_roots: &[String],
) -> Vec<RecentFile> {
    let Ok(read) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in read.take(MAX_SCAN_ENTRIES).flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if is_noise_name(&name) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        #[cfg(windows)]
        let attrs = std::os::windows::fs::MetadataExt::file_attributes(&meta);
        #[cfg(not(windows))]
        let attrs = 0u32;
        // hidden (0x2) / system (0x4)
        if attrs & 0x6 != 0 {
            continue;
        }
        let ft = meta.file_type();
        let is_dir = ft.is_dir();
        // A junction / symlink to a folder is never presented (we never follow reparse points).
        if ft.is_symlink() || (is_dir && attrs & 0x400 != 0) {
            continue;
        }
        let Some(created) = meta.created().or_else(|_| meta.modified()).ok() else {
            continue;
        };
        let created_ms = ms_of(created);
        if !within_window(created_ms, now_ms, max_age_days) {
            continue;
        }
        let full = entry.path().to_string_lossy().into_owned();
        let Some(path) = sanitize_abs_path(&full, blocked_roots) else {
            continue;
        };
        out.push(RecentFile {
            name,
            path,
            is_dir: Some(is_dir),
            last_used_ms: created_ms,
            source: "new",
            label: label.to_owned(),
        });
    }
    sort_and_cap(out, MAX_PER_FOLDER)
}

/// Scan every folder on its own thread with one shared time budget; a folder that does not
/// answer in time (dead network share) is skipped and its thread left to finish on its own.
pub fn scan_watched_folders(
    folders: Vec<(String, String)>, // (validated path, label)
    max_age_days: u32,
    limit: usize,
    blocked_roots: Vec<String>,
    budget: Option<Duration>,
) -> Vec<RecentFile> {
    let limit = limit.clamp(1, MAX_LIMIT);
    let now = now_ms();
    let (tx, rx) = std::sync::mpsc::channel();
    let count = folders.len().min(MAX_FOLDERS);
    for (path, label) in folders.into_iter().take(MAX_FOLDERS) {
        let tx = tx.clone();
        let blocked = blocked_roots.clone();
        std::thread::spawn(move || {
            let items = scan_new_items(&path, &label, now, max_age_days, &blocked);
            let _ = tx.send(items);
        });
    }
    drop(tx);
    let deadline = std::time::Instant::now() + budget.unwrap_or(SCAN_BUDGET);
    let mut all = Vec::new();
    for _ in 0..count {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        match rx.recv_timeout(left) {
            Ok(items) => all.extend(items),
            Err(_) => break,
        }
    }
    let mut all = dedupe_newest(all);
    all.truncate(limit);
    all
}

/// Display label of a watched folder: `Downloads` for the known folder, else its name.
pub fn folder_label(path: &str, downloads: Option<&str>) -> String {
    if downloads.is_some_and(|d| norm_key(d) == norm_key(path)) {
        return "Downloads".to_owned();
    }
    display_name(path)
}

/// Validate Settings-supplied watch folders: absolute, not a device path, not Ferrum's own
/// data, de-duplicated, capped.
pub fn validate_watch_folders(raw: &[String], blocked_roots: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for r in raw {
        let Some(p) = sanitize_abs_path(r, blocked_roots) else {
            continue;
        };
        if !out.iter().any(|o| norm_key(o) == norm_key(&p)) {
            out.push(p);
        }
        if out.len() >= MAX_FOLDERS {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests;
