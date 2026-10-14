//! FER-472: where the directory compare (and therefore sync) gets its
//! per-directory listings from.
//!
//! The compare walker (`compare::Walker`) no longer touches the filesystem
//! itself; it asks a [`TreeSource`] for one directory level at a time. Three
//! kinds of source exist or are planned:
//!
//! - [`DiskSource`] (tier A, always on, no admin): one `FindFirstFileExW`
//!   per directory with `FindExInfoBasic` + `FIND_FIRST_EX_LARGE_FETCH`.
//!   Size, mtime and the reparse tag come straight from the listing, so there
//!   is no `stat` per file.
//! - [`MftSource`] (tier B, optional, UAC): the whole subtree is read from an
//!   already-decoded MFT entry stream (`ferrum-scan-helper.exe`, FER-460) and
//!   served from memory. Only used for roots on a local NTFS volume.
//! - Later: a source backed by the FerrumIndexer service (no UAC). It only has
//!   to implement [`TreeSource`]; the walker does not change.
//!
//! Every source must reproduce the legacy walker's semantics exactly: names
//! are reported as found (lossy UTF-16), a symlink/junction is a plain entry
//! (never followed, `is_dir == false`, `is_symlink == true`), a directory has
//! size 0, mtime is milliseconds since the Unix epoch (0 when unknown or
//! before 1970).
//!
//! [`Prefetch`] adds the parallelism: while the walker consumes directories
//! in order, a few worker threads list the directories it will need next, on
//! both sides at once.

use crate::compare::SideInfo;
use ferrum_ntfs_scan::protocol::{EntryKind, WireEntry};
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};

/// Which engine produced a listing (reported to the UI).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanTier {
    /// Tier A: ordinary directory listing.
    Walk,
    /// Tier B: MFT snapshot from the elevated helper.
    Mft,
}

/// One directory level.
#[derive(Debug, Default)]
pub struct DirListing {
    pub infos: HashMap<String, SideInfo>,
    /// Names that exist but could not be read; excluded from entries.
    pub broken: BTreeSet<String>,
    /// `(name, message)` of entries (or of the listing itself, name `?`)
    /// that failed. The walker prefixes the side and the relative path.
    pub item_errors: Vec<(String, String)>,
}

/// A source of directory listings for one compare root.
///
/// `rel` is the `/`-separated path below the root (`""` = the root).
pub trait TreeSource: Send + Sync {
    /// List one directory. `Err` = the directory itself cannot be read.
    fn list(&self, rel: &str) -> io::Result<DirListing>;
    fn tier(&self) -> ScanTier;
    /// True when `list` does real I/O worth running on worker threads.
    fn prefetchable(&self) -> bool {
        false
    }
    /// True when the listed numbers may lag the file itself (the MFT is a
    /// snapshot; a directory index is only refreshed when a writer closes the
    /// file): the walker re-reads every entry it is about to REPORT as
    /// different through a handle (`lstat`), so a stale record can never put
    /// a wrong size/mtime in front of sync.
    fn needs_verify(&self) -> bool {
        false
    }
}

// ── Windows time helpers ─────────────────────────────────────────────────

/// FILETIME ticks (100 ns) between 1601-01-01 and 1970-01-01.
const FILETIME_UNIX_EPOCH: u64 = 116_444_736_000_000_000;

/// FILETIME → ms since the Unix epoch, with the same result as
/// `fs_service::modified_ms` (0 for unknown or pre-1970 times).
pub fn filetime_to_ms(ft: u64) -> i64 {
    if ft < FILETIME_UNIX_EPOCH {
        return 0;
    }
    ((ft - FILETIME_UNIX_EPOCH) / 10_000) as i64
}

// ── Tier A: FindFirstFileExW ─────────────────────────────────────────────

/// Plain directory listing below `root`.
pub struct DiskSource {
    root: PathBuf,
}

impl DiskSource {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

impl TreeSource for DiskSource {
    fn list(&self, rel: &str) -> io::Result<DirListing> {
        let dir = if rel.is_empty() {
            self.root.clone()
        } else {
            self.root.join(rel)
        };
        list_dir_disk(&dir)
    }

    fn tier(&self) -> ScanTier {
        ScanTier::Walk
    }

    fn prefetchable(&self) -> bool {
        true
    }

    /// FER-472 review M2: the directory index copy of size/mtime is only
    /// refreshed when a writer closes the file, so a file open for writing
    /// can look unchanged. Reported differences are re-read by handle.
    fn needs_verify(&self) -> bool {
        true
    }
}

/// `FindFirstFileExW` fails with FILE_NOT_FOUND on a directory that has no
/// entries at all, not even `.`/`..` (the root of an empty FAT/exFAT volume).
/// That is an empty folder, as `read_dir` reports it; any other failure on a
/// path that is not a directory stays an error.
pub(crate) fn empty_dir_not_error(os_code: i32, is_dir: bool) -> bool {
    const ERROR_FILE_NOT_FOUND: i32 = 2;
    os_code == ERROR_FILE_NOT_FOUND && is_dir
}

#[cfg(windows)]
fn list_dir_disk(dir: &Path) -> io::Result<DirListing> {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use windows_sys::Win32::Foundation::{ERROR_NO_MORE_FILES, GetLastError, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FIND_FIRST_EX_LARGE_FETCH,
        FindClose, FindExInfoBasic, FindExSearchNameMatch, FindFirstFileExW, FindNextFileW,
        WIN32_FIND_DATAW,
    };
    /// Reparse tags with this bit are name surrogates (symlink, junction):
    /// exactly what std's `FileType::is_symlink` checks.
    const NAME_SURROGATE: u32 = 0x2000_0000;

    let mut pattern: Vec<u16> = long_path(dir).encode_wide().collect();
    if pattern.last() != Some(&(b'\\' as u16)) {
        pattern.push(b'\\' as u16);
    }
    pattern.push(b'*' as u16);
    pattern.push(0);

    // SAFETY: WIN32_FIND_DATAW is plain data; zeroed is a valid value.
    let mut data: WIN32_FIND_DATAW = unsafe { std::mem::zeroed() };
    // SAFETY: `pattern` is NUL-terminated and outlives the call; `data` is a
    // valid out-buffer of the type FindExInfoBasic expects.
    let handle = unsafe {
        FindFirstFileExW(
            pattern.as_ptr(),
            FindExInfoBasic,
            (&mut data as *mut WIN32_FIND_DATAW).cast(),
            FindExSearchNameMatch,
            std::ptr::null(),
            FIND_FIRST_EX_LARGE_FETCH,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        let err = io::Error::last_os_error();
        if let Some(code) = err.raw_os_error()
            && empty_dir_not_error(code, dir.is_dir())
        {
            return Ok(DirListing::default());
        }
        return Err(err);
    }
    let mut out = DirListing::default();
    loop {
        let len = data
            .cFileName
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(data.cFileName.len());
        let units = &data.cFileName[..len];
        let dot = |n: u16| units.len() == n as usize && units.iter().all(|&c| c == b'.' as u16);
        if !(dot(1) || dot(2)) {
            let name = std::ffi::OsString::from_wide(units)
                .to_string_lossy()
                .into_owned();
            let attrs = data.dwFileAttributes;
            let is_link =
                attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0 && data.dwReserved0 & NAME_SURROGATE != 0;
            let is_dir = attrs & FILE_ATTRIBUTE_DIRECTORY != 0 && !is_link;
            let ft = ((data.ftLastWriteTime.dwHighDateTime as u64) << 32)
                | data.ftLastWriteTime.dwLowDateTime as u64;
            let size = ((data.nFileSizeHigh as u64) << 32) | data.nFileSizeLow as u64;
            out.infos.insert(
                name,
                SideInfo {
                    is_dir,
                    is_symlink: is_link,
                    size: if is_dir { 0 } else { size },
                    modified_ms: filetime_to_ms(ft),
                },
            );
        }
        // SAFETY: `handle` is a live find handle, `data` a valid buffer.
        if unsafe { FindNextFileW(handle, &mut data) } == 0 {
            // SAFETY: plain thread-local error read.
            let code = unsafe { GetLastError() };
            if code != ERROR_NO_MORE_FILES {
                out.item_errors.push((
                    "?".into(),
                    io::Error::from_raw_os_error(code as i32).to_string(),
                ));
            }
            break;
        }
    }
    // SAFETY: closing the handle opened above, exactly once.
    unsafe { FindClose(handle) };
    Ok(out)
}

/// Past MAX_PATH the Win32 API needs the `\\?\` form (std does this for
/// `fs::read_dir` implicitly; the raw Find API does not).
#[cfg(windows)]
fn long_path(p: &Path) -> std::ffi::OsString {
    let s = p.as_os_str().to_string_lossy();
    if s.len() < 240 || s.starts_with(r"\\?\") {
        return p.as_os_str().to_os_string();
    }
    let s = s.replace('/', "\\");
    if let Some(unc) = s.strip_prefix(r"\\") {
        format!(r"\\?\UNC\{unc}").into()
    } else {
        format!(r"\\?\{s}").into()
    }
}

#[cfg(not(windows))]
fn list_dir_disk(dir: &Path) -> io::Result<DirListing> {
    let mut out = DirListing::default();
    for item in std::fs::read_dir(dir)? {
        let item = match item {
            Ok(i) => i,
            Err(e) => {
                out.item_errors.push(("?".into(), e.to_string()));
                continue;
            }
        };
        let name = item.file_name().to_string_lossy().into_owned();
        match crate::compare::side_info(&item.path()) {
            Ok(info) => {
                out.infos.insert(name, info);
            }
            Err(e) => {
                out.item_errors.push((name.clone(), e.to_string()));
                out.broken.insert(name);
            }
        }
    }
    Ok(out)
}

// ── Tier B: MFT snapshot ─────────────────────────────────────────────────

/// A subtree of one volume's MFT, indexed by relative directory path.
///
/// Built from untrusted-but-decoded protocol entries with the same rules as
/// the Disk Usage builder: a directory is linked under its parent at most
/// once (first entry wins, so the graph is a forest and no cycle is
/// reachable from the root), entries with an unknown parent are dropped, and
/// symlinks/junctions are entries that are never descended.
pub struct MftSource {
    /// rel path (`""` = root, children `a/b`) → index into `dirs`.
    index: HashMap<String, usize>,
    dirs: Vec<Vec<(String, SideInfo)>>,
}

impl MftSource {
    /// Number of directories in the subtree (including the root).
    pub fn dir_count(&self) -> usize {
        self.dirs.len()
    }

    /// Build the subtree for `root` (`C:\data\x`) from the volume's entries.
    /// `volume_root` is `C:\`. `NotFound` when `root` is not in the MFT
    /// (caller falls back to the ordinary listing).
    pub fn build(
        root: &str,
        volume_root: &str,
        entries: &[WireEntry],
        root_frn: u64,
    ) -> Result<Self, String> {
        use crate::disk_usage::fast::{components_below, same_name};
        let mut dir_slot: HashMap<u64, usize> = HashMap::new();
        let mut dir_entry: Vec<usize> = vec![usize::MAX]; // slot 0 = volume root
        dir_slot.insert(root_frn, 0);
        for (i, e) in entries.iter().enumerate() {
            if e.kind == EntryKind::Dir && !dir_slot.contains_key(&e.frn) {
                dir_slot.insert(e.frn, dir_entry.len());
                dir_entry.push(i);
            }
        }
        let mut child_dirs: Vec<Vec<usize>> = vec![Vec::new(); dir_entry.len()];
        let mut others: Vec<Vec<usize>> = vec![Vec::new(); dir_entry.len()];
        for (i, e) in entries.iter().enumerate() {
            if e.frn == root_frn {
                continue;
            }
            let Some(&parent) = dir_slot.get(&e.parent) else {
                continue; // orphan / metafile child: dropped
            };
            match e.kind {
                EntryKind::Dir => {
                    let slot = dir_slot[&e.frn];
                    if dir_entry[slot] == i {
                        child_dirs[parent].push(slot);
                    }
                }
                EntryKind::File | EntryKind::Symlink => others[parent].push(i),
            }
        }
        let not_found = || "folder tidak ditemukan di MFT volumenya".to_string();
        let mut cur = 0usize;
        for comp in components_below(volume_root, root).ok_or_else(not_found)? {
            // Exact spelling first. Without one, a single case-insensitive
            // candidate is fine; several (a case-sensitive folder holding
            // `Data` and `data`) are ambiguous, so fall back to the walk.
            let kids = &child_dirs[cur];
            cur = match kids.iter().find(|&&s| entries[dir_entry[s]].name == comp) {
                Some(&s) => s,
                None => {
                    let mut hits = kids
                        .iter()
                        .filter(|&&s| same_name(&entries[dir_entry[s]].name, &comp));
                    match (hits.next(), hits.next()) {
                        (Some(&s), None) => s,
                        _ => return Err(not_found()),
                    }
                }
            };
        }

        let info_of = |e: &WireEntry| -> SideInfo {
            let (is_dir, is_symlink) = match e.kind {
                EntryKind::Dir => (true, false),
                EntryKind::Symlink => (false, true),
                EntryKind::File => (false, false),
            };
            SideInfo {
                is_dir,
                is_symlink,
                size: if is_dir || is_symlink { 0 } else { e.logical },
                modified_ms: filetime_to_ms(e.modified),
            }
        };
        let mut out = MftSource {
            index: HashMap::new(),
            dirs: Vec::new(),
        };
        // (slot, rel). Each slot is reachable once (forest), so this ends.
        let mut stack: Vec<(usize, String)> = vec![(cur, String::new())];
        while let Some((slot, rel)) = stack.pop() {
            let mut kids: Vec<(String, SideInfo)> =
                Vec::with_capacity(others[slot].len() + child_dirs[slot].len());
            for &fi in &others[slot] {
                kids.push((entries[fi].name.clone(), info_of(&entries[fi])));
            }
            for &cs in &child_dirs[slot] {
                let ent = &entries[dir_entry[cs]];
                kids.push((ent.name.clone(), info_of(ent)));
                let crel = if rel.is_empty() {
                    ent.name.clone()
                } else {
                    format!("{rel}/{}", ent.name)
                };
                stack.push((cs, crel));
            }
            out.index.insert(rel, out.dirs.len());
            out.dirs.push(kids);
        }
        Ok(out)
    }
}

impl TreeSource for MftSource {
    fn list(&self, rel: &str) -> io::Result<DirListing> {
        let Some(&i) = self.index.get(rel) else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "folder tidak ada di snapshot MFT",
            ));
        };
        let mut out = DirListing::default();
        out.infos.reserve(self.dirs[i].len());
        for (name, info) in &self.dirs[i] {
            out.infos.insert(name.clone(), *info);
        }
        Ok(out)
    }

    fn tier(&self) -> ScanTier {
        ScanTier::Mft
    }

    fn needs_verify(&self) -> bool {
        true
    }
}

// ── Parallel prefetch ────────────────────────────────────────────────────

/// Extra listing threads next to the walker thread (so at most 4 listings
/// run at once; scans stay polite on a shared PC).
pub const MAX_PREFETCH_WORKERS: usize = 3;
/// Finished-but-unconsumed listings the workers may hold. Bounds memory when
/// one level has tens of thousands of sibling folders.
const MAX_READY: usize = 2000;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum TreeSide {
    Left = 0,
    Right = 1,
}

type Key = (TreeSide, String);

enum Slot {
    Queued,
    Running,
    Done(io::Result<DirListing>),
}

#[derive(Default)]
struct State {
    queue: VecDeque<Key>,
    slots: HashMap<Key, Slot>,
    ready: usize,
    stop: bool,
}

/// Look-ahead listing for the walker. Create it, run [`Prefetch::worker`]
/// on up to [`MAX_PREFETCH_WORKERS`] scoped threads, ask for directories with
/// [`Prefetch::request`] and read them with [`Prefetch::take`].
///
/// With no workers it degrades to plain sequential listing (`take` lists
/// inline), so correctness never depends on the threads.
pub struct Prefetch<'s> {
    sources: [&'s dyn TreeSource; 2],
    state: Mutex<State>,
    cv: Condvar,
    cancel: &'s AtomicBool,
}

impl<'s> Prefetch<'s> {
    pub fn new(
        left: &'s dyn TreeSource,
        right: &'s dyn TreeSource,
        cancel: &'s AtomicBool,
    ) -> Self {
        Self {
            sources: [left, right],
            state: Mutex::new(State::default()),
            cv: Condvar::new(),
            cancel,
        }
    }

    /// How many worker threads are worth starting.
    pub fn wanted_workers(&self) -> usize {
        if !self.sources.iter().any(|s| s.prefetchable()) {
            return 0;
        }
        std::thread::available_parallelism()
            .map(|n| n.get().saturating_sub(1))
            .unwrap_or(1)
            .min(MAX_PREFETCH_WORKERS)
    }

    fn src(&self, side: TreeSide) -> &dyn TreeSource {
        self.sources[side as usize]
    }

    pub fn source(&self, side: TreeSide) -> &dyn TreeSource {
        self.src(side)
    }

    /// Queue `rel` on both sides (only sides whose source benefits).
    pub fn request(&self, rel: &str) {
        let mut st = self.state.lock().expect("prefetch state");
        for side in [TreeSide::Left, TreeSide::Right] {
            if !self.src(side).prefetchable() {
                continue;
            }
            let key = (side, rel.to_string());
            if st.slots.contains_key(&key) {
                continue;
            }
            st.slots.insert(key.clone(), Slot::Queued);
            st.queue.push_back(key);
        }
        drop(st);
        self.cv.notify_all();
    }

    /// Queue `rel` on a specific side (only if source benefits).
    pub fn request_side(&self, side: TreeSide, rel: &str) {
        if !self.src(side).prefetchable() {
            return;
        }
        let key = (side, rel.to_string());
        let mut st = self.state.lock().expect("prefetch state");
        if st.slots.contains_key(&key) {
            return;
        }
        st.slots.insert(key.clone(), Slot::Queued);
        st.queue.push_back(key);
        drop(st);
        self.cv.notify_all();
    }

    /// The listing of `rel` on `side`: from the look-ahead when ready, else
    /// listed right here (stealing a queued item, or waiting for a running one).
    pub fn take(&self, side: TreeSide, rel: &str) -> io::Result<DirListing> {
        let key = (side, rel.to_string());
        let mut st = self.state.lock().expect("prefetch state");
        loop {
            match st.slots.get(&key) {
                Some(Slot::Done(_)) => {
                    let Some(Slot::Done(res)) = st.slots.remove(&key) else {
                        unreachable!()
                    };
                    st.ready -= 1;
                    drop(st);
                    self.cv.notify_all(); // a worker may be waiting for room
                    return res;
                }
                Some(Slot::Running) => {
                    st = self.cv.wait(st).expect("prefetch state");
                }
                Some(Slot::Queued) => {
                    st.slots.remove(&key); // steal; the worker skips it
                    drop(st);
                    return self.src(side).list(rel);
                }
                None => {
                    drop(st);
                    return self.src(side).list(rel);
                }
            }
        }
    }

    /// Worker loop: run on a scoped thread; returns after [`Prefetch::finish`].
    pub fn worker(&self) {
        let mut st = self.state.lock().expect("prefetch state");
        loop {
            if st.stop || self.cancel.load(Ordering::Relaxed) {
                return;
            }
            if st.ready >= MAX_READY {
                st = self.cv.wait(st).expect("prefetch state");
                continue;
            }
            let Some(key) = st.queue.pop_front() else {
                st = self.cv.wait(st).expect("prefetch state");
                continue;
            };
            if !matches!(st.slots.get(&key), Some(Slot::Queued)) {
                continue; // stolen by the walker
            }
            st.slots.insert(key.clone(), Slot::Running);
            drop(st);
            // A panic inside a source must not leave the slot `Running`
            // forever (the walker would wait on it): it becomes an error.
            let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                self.src(key.0).list(&key.1)
            }))
            .unwrap_or_else(|_| Err(io::Error::other("listing worker panicked")));
            st = self.state.lock().expect("prefetch state");
            st.slots.insert(key, Slot::Done(res));
            st.ready += 1;
            self.cv.notify_all();
        }
    }

    /// Tell the workers to stop (call before leaving the thread scope).
    pub fn finish(&self) {
        if let Ok(mut st) = self.state.lock() {
            st.stop = true;
        }
        self.cv.notify_all();
    }

    /// Calls [`Prefetch::finish`] when dropped, also while unwinding, so a
    /// panicking walker cannot leave workers waiting and the scope hanging.
    pub fn finish_on_drop(&self) -> FinishGuard<'_, 's> {
        FinishGuard(self)
    }
}

pub struct FinishGuard<'a, 's>(&'a Prefetch<'s>);

impl Drop for FinishGuard<'_, '_> {
    fn drop(&mut self) {
        self.0.finish();
    }
}

// ── Fast-scan (tier B) orchestration ─────────────────────────────────────

/// What the fast-scan attempt did, for the report and the UI.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FastScanInfo {
    /// Left side was served from the MFT.
    pub left: bool,
    /// Right side was served from the MFT.
    pub right: bool,
    /// Why (part of) the fast scan was not used. User-facing, Indonesian.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// MFT sources prepared before the compare starts.
pub struct FastPrep {
    pub left: Option<MftSource>,
    pub right: Option<MftSource>,
    pub info: FastScanInfo,
}

/// Everything the helper delivered for one volume.
pub struct VolumeScan {
    pub volume: char,
    pub entries: Vec<WireEntry>,
    pub root_frn: u64,
    pub corrupt_records: u64,
}

/// Build the MFT source for `root` from a volume scan, refusing an
/// untrustworthy snapshot. Pure; unit-tested without elevation.
pub fn source_from_scan(root: &str, scan: &VolumeScan) -> Result<MftSource, String> {
    if scan.corrupt_records > 0 {
        // Damaged records may hide entries anywhere; a sync must not be
        // driven from a possibly incomplete listing.
        return Err(format!(
            "{} record MFT rusak, hasil scan cepat tidak dipercaya",
            scan.corrupt_records
        ));
    }
    let volume_root = format!("{}:\\", scan.volume);
    MftSource::build(root, &volume_root, &scan.entries, scan.root_frn)
}

/// Decide, per side, whether the MFT may serve it, run the helper (one UAC
/// per distinct volume) and build the sources. Never fails: every problem
/// becomes a fallback to the ordinary listing plus a note.
///
/// `run` performs one helper scan for a volume letter (injected so the
/// fallback rules are testable without elevation).
pub fn prepare_fast(
    left_root: &str,
    right_root: &str,
    mut volume_of: impl FnMut(&str) -> Result<char, String>,
    mut run: impl FnMut(char) -> Result<VolumeScan, crate::fast_scan::FastScanError>,
) -> FastPrep {
    let mut notes: Vec<String> = Vec::new();
    let mut vol = [None, None];
    for (i, root) in [left_root, right_root].into_iter().enumerate() {
        match volume_of(root) {
            Ok(v) => vol[i] = Some(v),
            Err(why) => notes.push(why),
        }
    }
    let mut scans: Vec<Result<VolumeScan, String>> = Vec::new();
    let mut letters: Vec<char> = Vec::new();
    for v in vol.iter().flatten() {
        if !letters.contains(v) {
            letters.push(*v);
        }
    }
    for v in &letters {
        scans.push(run(*v).map_err(|e| e.to_string()));
    }
    let mut srcs: [Option<MftSource>; 2] = [None, None];
    for (i, root) in [left_root, right_root].into_iter().enumerate() {
        let Some(v) = vol[i] else { continue };
        let pos = letters.iter().position(|l| *l == v).expect("letter listed");
        match &scans[pos] {
            Ok(scan) => match source_from_scan(root, scan) {
                Ok(s) => srcs[i] = Some(s),
                Err(why) => notes.push(why),
            },
            Err(why) => {
                if !notes.contains(why) {
                    notes.push(why.clone());
                }
            }
        }
    }
    let note = if notes.is_empty() {
        None
    } else {
        Some(format!("Scan cepat dilewati: {}", notes.join("; ")))
    };
    let [left, right] = srcs;
    FastPrep {
        info: FastScanInfo {
            left: left.is_some(),
            right: right.is_some(),
            note,
        },
        left,
        right,
    }
}

/// The real thing: eligibility check + UAC helper per distinct volume.
/// Reports the helper's progress through `ctl` (scan phase) and honours its
/// cancel flag. Never fails (see [`prepare_fast`]).
pub fn prepare_fast_with_helper(
    left_root: &str,
    right_root: &str,
    hwnd: isize,
    ctl: &crate::compare::Ctl,
) -> FastPrep {
    use crate::compare::ProgressPhase;
    use crate::fast_scan::{eligibility, run_helper};
    prepare_fast(
        left_root,
        right_root,
        |root| {
            let e = eligibility(root);
            match (
                e.eligible,
                e.volume.as_deref().and_then(|v| v.chars().next()),
            ) {
                (true, Some(v)) => Ok(v.to_ascii_uppercase()),
                _ => Err(e
                    .reason
                    .unwrap_or_else(|| "bukan drive NTFS lokal".to_string())),
            }
        },
        |volume| {
            let r = run_helper(volume, hwnd, ctl.cancel_flag(), |p| {
                ctl.report(
                    ProgressPhase::Scan,
                    p.records_done,
                    Some(p.records_total),
                    &format!("MFT {volume}:"),
                    false,
                );
            })?;
            Ok(VolumeScan {
                volume,
                entries: r.entries,
                root_frn: r.done.root_frn,
                corrupt_records: r.done.corrupt_records,
            })
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fast_scan::FastScanError;

    const ROOT: u64 = 5 | (5 << 48);

    fn ft(unix_secs: u64) -> u64 {
        FILETIME_UNIX_EPOCH + unix_secs * 10_000_000
    }

    fn ent(frn: u64, parent: u64, kind: EntryKind, size: u64, secs: u64, name: &str) -> WireEntry {
        WireEntry {
            frn,
            parent,
            kind,
            logical: size,
            allocated: None,
            modified: ft(secs),
            name: name.into(),
        }
    }

    #[test]
    fn filetime_matches_unix_ms_and_clamps_before_1970() {
        assert_eq!(filetime_to_ms(0), 0);
        assert_eq!(filetime_to_ms(FILETIME_UNIX_EPOCH - 1), 0);
        assert_eq!(filetime_to_ms(FILETIME_UNIX_EPOCH), 0);
        assert_eq!(
            filetime_to_ms(ft(1_700_000_000) + 12_345),
            1_700_000_000_001
        );
        assert_eq!(
            filetime_to_ms(u64::MAX),
            ((u64::MAX - FILETIME_UNIX_EPOCH) / 10_000) as i64
        );
    }

    fn sample() -> Vec<WireEntry> {
        // C:\ -> data -> {a.txt, sub -> {b.txt, link}, junk}; other -> x.txt
        let data = 100 | (1 << 48);
        let sub = 101 | (1 << 48);
        let other = 102 | (1 << 48);
        vec![
            ent(data, ROOT, EntryKind::Dir, 0, 10, "Data"),
            ent(other, ROOT, EntryKind::Dir, 0, 10, "other"),
            ent(sub, data, EntryKind::Dir, 0, 20, "sub"),
            ent(200, data, EntryKind::File, 5, 30, "a.txt"),
            ent(201, sub, EntryKind::File, 7, 40, "b.txt"),
            ent(202, sub, EntryKind::Symlink, 0, 50, "link"),
            ent(203, other, EntryKind::File, 9, 60, "x.txt"),
            // orphan: parent unknown
            ent(204, 9999, EntryKind::File, 1, 1, "orphan.txt"),
        ]
    }

    #[test]
    fn mft_source_serves_only_the_subtree_with_walker_semantics() {
        let src = MftSource::build("c:\\data", "C:\\", &sample(), ROOT).unwrap();
        assert_eq!(src.dir_count(), 2);
        let root = src.list("").unwrap();
        let mut names: Vec<_> = root.infos.keys().cloned().collect();
        names.sort();
        assert_eq!(names, ["a.txt", "sub"]);
        let a = root.infos["a.txt"];
        assert_eq!((a.size, a.modified_ms, a.is_dir), (5, 30_000, false));
        assert!(root.infos["sub"].is_dir && root.infos["sub"].size == 0);
        let sub = src.list("sub").unwrap();
        let link = sub.infos["link"];
        assert!(link.is_symlink && !link.is_dir && link.size == 0);
        assert!(src.list("missing").is_err());
        assert!(src.needs_verify());
    }

    #[test]
    fn mft_source_root_not_in_mft_is_an_error() {
        assert!(MftSource::build("C:\\nope", "C:\\", &sample(), ROOT).is_err());
        assert!(MftSource::build("D:\\data", "C:\\", &sample(), ROOT).is_err());
    }

    #[test]
    fn mft_source_ignores_cycles_and_duplicate_directories() {
        let a = 100 | (1 << 48);
        let b = 101 | (1 << 48);
        let entries = vec![
            ent(a, ROOT, EntryKind::Dir, 0, 1, "a"),
            // b under a, and a again under b (cycle), plus a duplicate b under root
            ent(b, a, EntryKind::Dir, 0, 1, "b"),
            ent(a, b, EntryKind::Dir, 0, 1, "a"),
            ent(b, ROOT, EntryKind::Dir, 0, 1, "b"),
            ent(300, b, EntryKind::File, 1, 1, "f"),
        ];
        let src = MftSource::build("C:\\a", "C:\\", &entries, ROOT).unwrap();
        assert_eq!(src.dir_count(), 2);
        assert!(src.list("b").unwrap().infos.contains_key("f"));
    }

    fn scan(corrupt: u64) -> VolumeScan {
        VolumeScan {
            volume: 'C',
            entries: sample(),
            root_frn: ROOT,
            corrupt_records: corrupt,
        }
    }

    #[test]
    fn corrupt_mft_records_mean_fallback() {
        assert!(source_from_scan("C:\\data", &scan(0)).is_ok());
        assert!(source_from_scan("C:\\data", &scan(3)).is_err());
    }

    #[test]
    fn prepare_runs_one_helper_per_volume_and_uses_it_for_both_sides() {
        let mut runs = 0;
        let p = prepare_fast(
            "C:\\data",
            "C:\\other",
            |_| Ok('C'),
            |_| {
                runs += 1;
                Ok(scan(0))
            },
        );
        assert_eq!(runs, 1);
        assert!(p.info.left && p.info.right && p.info.note.is_none());
    }

    #[test]
    fn declined_uac_falls_back_with_a_note() {
        let p = prepare_fast(
            "C:\\data",
            "C:\\other",
            |_| Ok('C'),
            |_| Err(FastScanError::Declined),
        );
        assert!(p.left.is_none() && p.right.is_none());
        let note = p.info.note.unwrap();
        assert!(note.contains("izin admin ditolak"), "{note}");
        // The same failure is reported once, not once per side.
        assert_eq!(note.matches("izin admin ditolak").count(), 1);
    }

    #[test]
    fn non_ntfs_side_is_walked_and_the_other_side_still_uses_the_mft() {
        let p = prepare_fast(
            "C:\\data",
            "\\\\srv\\share",
            |r| {
                if r.starts_with('C') {
                    Ok('C')
                } else {
                    Err("bukan NTFS lokal".into())
                }
            },
            |_| Ok(scan(0)),
        );
        assert!(p.info.left && !p.info.right);
        assert!(p.info.note.unwrap().contains("bukan NTFS lokal"));
    }

    #[test]
    fn nothing_eligible_never_runs_the_helper() {
        let p = prepare_fast(
            "\\\\a\\b",
            "\\\\c\\d",
            |_| Err("bukan NTFS lokal".into()),
            |_| panic!("helper must not run"),
        );
        assert!(!p.info.left && !p.info.right);
    }

    struct Fixed(
        HashMap<String, Vec<(String, u64)>>,
        std::sync::atomic::AtomicUsize,
    );
    impl TreeSource for Fixed {
        fn list(&self, rel: &str) -> io::Result<DirListing> {
            self.1.fetch_add(1, Ordering::SeqCst);
            let kids = self
                .0
                .get(rel)
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "nf"))?;
            let mut d = DirListing::default();
            for (n, s) in kids {
                d.infos.insert(
                    n.clone(),
                    SideInfo {
                        is_dir: false,
                        is_symlink: false,
                        size: *s,
                        modified_ms: 0,
                    },
                );
            }
            Ok(d)
        }
        fn tier(&self) -> ScanTier {
            ScanTier::Walk
        }
        fn prefetchable(&self) -> bool {
            true
        }
    }

    #[test]
    fn prefetch_delivers_every_requested_listing_exactly_once_with_workers() {
        let mut m = HashMap::new();
        for i in 0..200 {
            m.insert(format!("d{i}"), vec![(format!("f{i}"), i as u64)]);
        }
        let l = Fixed(m.clone(), 0.into());
        let r = Fixed(m, 0.into());
        let cancel = AtomicBool::new(false);
        let pf = Prefetch::new(&l, &r, &cancel);
        std::thread::scope(|s| {
            for _ in 0..3 {
                s.spawn(|| pf.worker());
            }
            for i in 0..200 {
                pf.request(&format!("d{i}"));
            }
            for i in 0..200 {
                for side in [TreeSide::Left, TreeSide::Right] {
                    let d = pf.take(side, &format!("d{i}")).unwrap();
                    assert_eq!(d.infos[&format!("f{i}")].size, i as u64);
                }
            }
            pf.finish();
        });
        // A listing is never produced twice for the same (side, dir).
        assert_eq!(l.1.load(Ordering::SeqCst), 200);
        assert_eq!(r.1.load(Ordering::SeqCst), 200);
    }

    #[test]
    fn prefetch_without_workers_lists_inline_and_errors_pass_through() {
        let l = Fixed(HashMap::new(), 0.into());
        let r = Fixed(HashMap::new(), 0.into());
        let cancel = AtomicBool::new(false);
        let pf = Prefetch::new(&l, &r, &cancel);
        pf.request("x");
        assert!(pf.take(TreeSide::Left, "x").is_err());
        assert!(pf.take(TreeSide::Right, "never-requested").is_err());
    }

    #[cfg(windows)]
    #[test]
    fn disk_source_lists_like_std_for_files_dirs_and_times() {
        let dir = std::env::temp_dir().join(format!("ferrum-ts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.txt"), b"hello").unwrap();
        std::fs::write(dir.join("ünï 名.bin"), b"").unwrap();
        let got = DiskSource::new(&dir).list("").unwrap();
        for e in std::fs::read_dir(&dir).unwrap() {
            let e = e.unwrap();
            let want = crate::compare::side_info(&e.path()).unwrap();
            let name = e.file_name().to_string_lossy().into_owned();
            assert_eq!(got.infos[&name], want, "{name}");
        }
        assert_eq!(got.infos.len(), 3);
        assert!(got.item_errors.is_empty());
        // an empty dir yields no "." / ".." entries
        assert!(DiskSource::new(&dir).list("sub").unwrap().infos.is_empty());
        assert!(DiskSource::new(&dir).list("nope").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn disk_source_reaches_paths_beyond_max_path() {
        let mut dir = std::env::temp_dir().join(format!("ferrum-ts-long-{}", std::process::id()));
        let base = dir.clone();
        let _ = std::fs::remove_dir_all(&base);
        for _ in 0..6 {
            dir.push("d".repeat(50));
        }
        let verbatim = format!(r"\\?\{}", dir.display());
        std::fs::create_dir_all(&verbatim).unwrap();
        std::fs::write(format!(r"{verbatim}\deep.txt"), b"x").unwrap();
        let got = DiskSource::new(&dir).list("").unwrap();
        assert_eq!(got.infos["deep.txt"].size, 1);
        let _ = std::fs::remove_dir_all(format!(r"\\?\{}", base.display()));
    }

    // ── parity with the ordinary listing (the legacy walker semantics) ───

    fn temp_tree(tag: &str) -> (PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("ferrum-ts-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("tree");
        std::fs::create_dir_all(&root).unwrap();
        (base, root)
    }

    /// Fake "volume" whose root is `base`: one WireEntry per name under `base`.
    fn wire_from_disk(base: &Path) -> (Vec<WireEntry>, u64) {
        fn walk(dir: &Path, parent: u64, next: &mut u64, out: &mut Vec<WireEntry>) {
            for item in std::fs::read_dir(dir).unwrap() {
                let item = item.unwrap();
                let md = std::fs::symlink_metadata(item.path()).unwrap();
                let frn = *next;
                *next += 1;
                let kind = if md.file_type().is_symlink() {
                    EntryKind::Symlink
                } else if md.is_dir() {
                    EntryKind::Dir
                } else {
                    EntryKind::File
                };
                let since = md
                    .modified()
                    .unwrap()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap();
                out.push(WireEntry {
                    frn,
                    parent,
                    kind,
                    logical: if kind == EntryKind::File { md.len() } else { 0 },
                    allocated: None,
                    modified: FILETIME_UNIX_EPOCH + (since.as_nanos() / 100) as u64,
                    name: item.file_name().to_string_lossy().into_owned(),
                });
                if kind == EntryKind::Dir {
                    walk(&item.path(), frn, next, out);
                }
            }
        }
        let mut out = Vec::new();
        let mut next = 100;
        walk(base, ROOT, &mut next, &mut out);
        (out, ROOT)
    }

    fn put(root: &Path, rel: &str, body: &[u8], secs: u64) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, body).unwrap();
        let f = std::fs::OpenOptions::new().write(true).open(&p).unwrap();
        f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs))
            .unwrap();
    }

    fn fill(root: &Path, variant: bool) {
        put(root, "same.txt", b"abc", 1_700_000_000);
        put(
            root,
            "size.txt",
            if variant { b"abcd" } else { b"abc" },
            1_700_000_000,
        );
        put(
            root,
            "time.txt",
            b"abc",
            1_700_000_000 + if variant { 60 } else { 0 },
        );
        put(root, "ünï 名.bin", b"x", 1_700_000_100);
        put(root, "d/inner/deep.txt", b"deep", 1_700_000_200);
        put(root, "d/inner/more.txt", b"more", 1_700_000_300);
        std::fs::create_dir_all(root.join("emptydir")).unwrap();
        if variant {
            put(root, "only-right.txt", b"r", 1_700_000_400);
            put(root, "d/new/n.txt", b"n", 1_700_000_500);
        } else {
            put(root, "only-left.txt", b"l", 1_700_000_400);
        }
    }

    fn as_dirs(
        l: &str,
        r: &str,
        ls: &dyn TreeSource,
        rs: &dyn TreeSource,
    ) -> crate::compare::CompareDirsReport {
        norm(
            crate::compare::compare_dirs_with_sources(
                l,
                r,
                crate::compare::DirOpts {
                    by_content: false,
                    recursive: true,
                    max_entries: 0,
                },
                &crate::compare::Ctl::none(),
                ls,
                rs,
            )
            .unwrap(),
        )
    }

    /// A directory's own mtime in a listing comes from its parent's index, which
    /// NTFS refreshes lazily, so it can trail an lstat by moments. It is
    /// informational only (the sync plan ignores it), so parity checks blank it.
    fn norm(mut r: crate::compare::CompareDirsReport) -> crate::compare::CompareDirsReport {
        for e in &mut r.entries {
            if e.is_dir {
                e.left_modified_ms = 0;
                e.right_modified_ms = 0;
            }
        }
        r
    }

    fn mft_of(base: &Path, root: &Path) -> MftSource {
        let (entries, root_frn) = wire_from_disk(base);
        MftSource::build(
            root.to_str().unwrap(),
            base.to_str().unwrap(),
            &entries,
            root_frn,
        )
        .unwrap()
    }

    #[test]
    fn mft_listing_equals_disk_listing_for_every_directory() {
        let (base, root) = temp_tree("parity");
        fill(&root, false);
        let mft = mft_of(&base, &root);
        let disk = DiskSource::new(&root);
        for rel in ["", "d", "d/inner", "emptydir"] {
            let mut a = disk.list(rel).unwrap();
            let mut b = mft.list(rel).unwrap();
            for l in [&mut a, &mut b] {
                for i in l.infos.values_mut().filter(|i| i.is_dir) {
                    i.modified_ms = 0; // see `norm`
                }
            }
            assert_eq!(a.infos, b.infos, "dir {rel:?}");
            assert!(a.item_errors.is_empty() && b.broken.is_empty());
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn compare_report_is_identical_for_disk_and_mft_sources() {
        let (lbase, lroot) = temp_tree("rep-l");
        let (rbase, rroot) = temp_tree("rep-r");
        fill(&lroot, false);
        fill(&rroot, true);
        let lm = mft_of(&lbase, &lroot);
        let rm = mft_of(&rbase, &rroot);
        let (l, r) = (lroot.to_str().unwrap(), rroot.to_str().unwrap());
        let (ld, rd) = (DiskSource::new(&lroot), DiskSource::new(&rroot));
        let want = as_dirs(l, r, &ld, &rd);
        for (a, b) in [
            (&lm as &dyn TreeSource, &rm as &dyn TreeSource),
            (&lm, &rd),
            (&ld, &rm),
        ] {
            let got = as_dirs(l, r, a, b);
            assert_eq!(got.entries, want.entries);
            assert_eq!(got.errors, want.errors);
            assert_eq!(got.truncated, want.truncated);
        }
        // The fixture really differs in every way a sync cares about.
        use crate::compare::DirDiffState::*;
        let st = |n: &str| want.entries.iter().find(|e| e.name == n).unwrap().state;
        assert_eq!(st("same.txt"), Same);
        assert_eq!(st("size.txt"), SizeDifferent);
        assert_eq!(st("time.txt"), NewerRight);
        assert_eq!(st("only-left.txt"), LeftOnly);
        assert_eq!(st("only-right.txt"), RightOnly);
        assert_eq!(st("d/new"), RightOnly);
        let _ = std::fs::remove_dir_all(&lbase);
        let _ = std::fs::remove_dir_all(&rbase);
    }

    #[test]
    fn stale_snapshot_never_puts_old_numbers_on_a_reported_difference() {
        let (lbase, lroot) = temp_tree("stale-l");
        let (rbase, rroot) = temp_tree("stale-r");
        fill(&lroot, false);
        fill(&rroot, true);
        let lm = mft_of(&lbase, &lroot);
        let rm = mft_of(&rbase, &rroot);
        let (l, r) = (lroot.to_str().unwrap(), rroot.to_str().unwrap());
        // After the snapshot: a differing file grows, a left-only file is
        // deleted, a new right file appears.
        std::fs::write(rroot.join("size.txt"), b"0123456789").unwrap();
        std::fs::remove_file(lroot.join("only-left.txt")).unwrap();
        put(&rroot, "late.txt", b"late", 1_700_000_999);
        let rep = as_dirs(l, r, &lm, &rm);
        // Reported differences carry the live numbers, not the snapshot's.
        let size = rep.entries.iter().find(|e| e.name == "size.txt").unwrap();
        assert_eq!(size.right_size, 10);
        // A file deleted after the snapshot never shows up as a one-sided entry.
        assert!(rep.entries.iter().all(|e| e.name != "only-left.txt"));
        // A file created after the snapshot is simply not seen yet (documented limit).
        assert!(rep.entries.iter().all(|e| e.name != "late.txt"));
        for e in &rep.entries {
            if e.state != crate::compare::DirDiffState::Same && !e.is_dir {
                let l = crate::compare::side_info(&lroot.join(&e.name)).ok();
                let rr = crate::compare::side_info(&rroot.join(&e.name)).ok();
                assert_eq!(e.left_size, l.map_or(0, |i| i.size), "{}", e.name);
                assert_eq!(e.right_size, rr.map_or(0, |i| i.size), "{}", e.name);
            }
        }
        let _ = std::fs::remove_dir_all(&lbase);
        let _ = std::fs::remove_dir_all(&rbase);
    }

    #[test]
    fn symlink_is_a_plain_entry_in_both_sources_when_the_os_allows_links() {
        let (base, root) = temp_tree("link");
        put(&root, "target/f.txt", b"x", 1_700_000_000);
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_dir(root.join("target"), root.join("lnk")).is_ok();
        #[cfg(not(windows))]
        let made = std::os::unix::fs::symlink(root.join("target"), root.join("lnk")).is_ok();
        if made {
            let mft = mft_of(&base, &root);
            let a = DiskSource::new(&root).list("").unwrap();
            let b = mft.list("").unwrap();
            assert!(a.infos["lnk"].is_symlink && !a.infos["lnk"].is_dir);
            assert_eq!(a.infos["lnk"].is_symlink, b.infos["lnk"].is_symlink);
            assert_eq!(a.infos["lnk"].is_dir, b.infos["lnk"].is_dir);
            assert_eq!(a.infos["lnk"].size, b.infos["lnk"].size);
            // never descended
            assert!(mft.list("lnk").is_err());
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn parallel_prefetch_gives_the_same_report_as_sequential_listing() {
        let (lbase, lroot) = temp_tree("par-l");
        let (rbase, rroot) = temp_tree("par-r");
        for i in 0..60u64 {
            let rel = format!("g{}/s{i}/f.txt", i % 7);
            put(&lroot, &rel, b"a", 1_700_000_000 + i);
            put(
                &rroot,
                &rel,
                if i % 5 == 0 { b"aa" } else { b"a" },
                1_700_000_000 + i,
            );
        }
        let (l, r) = (lroot.to_str().unwrap(), rroot.to_str().unwrap());
        let (ld, rd) = (DiskSource::new(&lroot), DiskSource::new(&rroot));
        let par = as_dirs(l, r, &ld, &rd);
        // Same walk with look-ahead disabled (sources that are not prefetchable).
        struct Seq<'a>(&'a DiskSource);
        impl TreeSource for Seq<'_> {
            fn list(&self, rel: &str) -> io::Result<DirListing> {
                self.0.list(rel)
            }
            fn tier(&self) -> ScanTier {
                ScanTier::Walk
            }
        }
        let seq = as_dirs(l, r, &Seq(&ld), &Seq(&rd));
        assert_eq!(par.entries.len(), seq.entries.len());
        for (a, b) in par.entries.iter().zip(&seq.entries) {
            assert_eq!(a, b);
        }
        assert_eq!(par.errors, seq.errors);
        let diff = par
            .entries
            .iter()
            .filter(|e| e.state == crate::compare::DirDiffState::SizeDifferent)
            .count();
        assert_eq!(diff, 12);
        let _ = std::fs::remove_dir_all(&lbase);
        let _ = std::fs::remove_dir_all(&rbase);
    }

    // ── review fixes (M1, M2, L1, L2) ────────────────────────────────────

    #[test]
    fn m1_file_not_found_on_an_existing_folder_is_an_empty_listing() {
        assert!(empty_dir_not_error(2, true));
        assert!(!empty_dir_not_error(2, false)); // really missing
        assert!(!empty_dir_not_error(5, true)); // access denied stays an error
        assert!(!empty_dir_not_error(3, true)); // path not found stays an error
    }

    #[cfg(windows)]
    #[test]
    fn m1_missing_or_denied_folders_are_still_errors() {
        let (base, root) = temp_tree("m1");
        assert!(DiskSource::new(&root).list("").unwrap().infos.is_empty());
        assert!(DiskSource::new(&root).list("nope").is_err());
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Serves the listing a directory index would show BEFORE a writer closed
    /// its handle: the numbers of `before`, the names of the live tree.
    struct Lagging {
        live: DiskSource,
        stale: HashMap<String, SideInfo>,
    }
    impl TreeSource for Lagging {
        fn list(&self, rel: &str) -> io::Result<DirListing> {
            let mut l = self.live.list(rel)?;
            if rel.is_empty() {
                for (n, i) in l.infos.iter_mut() {
                    if let Some(s) = self.stale.get(n) {
                        *i = *s;
                    }
                }
            }
            Ok(l)
        }
        fn tier(&self) -> ScanTier {
            ScanTier::Walk
        }
        fn needs_verify(&self) -> bool {
            true
        }
    }

    #[test]
    fn m2_a_file_that_looks_unchanged_in_the_index_is_reported_with_live_numbers() {
        // Baseline world: both sides "same.txt" = 3 bytes. The right file is
        // then edited by a writer that has not closed its handle, so the
        // index still says 3 bytes while the file really holds 10.
        let (lbase, lroot) = temp_tree("m2-l");
        let (rbase, rroot) = temp_tree("m2-r");
        put(&lroot, "f.txt", b"abcd", 1_700_000_100); // left really changed
        put(&rroot, "f.txt", b"abc", 1_700_000_000);
        let before = crate::compare::side_info(&rroot.join("f.txt")).unwrap();
        put(&rroot, "f.txt", b"0123456789", 1_700_000_000);
        let stale_right: HashMap<String, SideInfo> = [("f.txt".to_string(), before)].into();
        let rs = Lagging {
            live: DiskSource::new(&rroot),
            stale: stale_right,
        };
        let ls = DiskSource::new(&lroot);
        let rep = as_dirs(lroot.to_str().unwrap(), rroot.to_str().unwrap(), &ls, &rs);
        let e = rep.entries.iter().find(|e| e.name == "f.txt").unwrap();
        // Index-only numbers would say right=3 bytes (a plain "left newer"
        // copy over an edited right); the verified report carries 10.
        assert_eq!(e.right_size, 10);
        assert_eq!(e.left_size, 4);
        assert_eq!(e.state, crate::compare::DirDiffState::SizeDifferent);
        let _ = std::fs::remove_dir_all(&lbase);
        let _ = std::fs::remove_dir_all(&rbase);
    }

    #[cfg(windows)]
    #[test]
    fn m2_a_file_held_open_for_writing_is_reported_with_its_real_size() {
        use std::io::Write;
        let (lbase, lroot) = temp_tree("m2o-l");
        let (rbase, rroot) = temp_tree("m2o-r");
        put(&lroot, "log.txt", b"x", 1_700_000_000);
        put(&rroot, "log.txt", b"y", 1_700_000_500);
        let mut h = std::fs::OpenOptions::new()
            .append(true)
            .open(rroot.join("log.txt"))
            .unwrap();
        h.write_all(&[b'z'; 5000]).unwrap(); // handle stays open
        let live = crate::compare::side_info(&rroot.join("log.txt")).unwrap();
        let (ld, rd) = (DiskSource::new(&lroot), DiskSource::new(&rroot));
        let rep = as_dirs(lroot.to_str().unwrap(), rroot.to_str().unwrap(), &ld, &rd);
        let e = rep.entries.iter().find(|e| e.name == "log.txt").unwrap();
        assert_eq!(e.right_size, live.size, "reported size is the handle's");
        drop(h);
        let _ = std::fs::remove_dir_all(&lbase);
        let _ = std::fs::remove_dir_all(&rbase);
    }

    #[test]
    fn l1_exact_name_wins_and_an_ambiguous_case_match_falls_back() {
        let a = 100 | (1 << 48);
        let b = 101 | (1 << 48);
        let entries = vec![
            ent(a, ROOT, EntryKind::Dir, 0, 1, "Data"),
            ent(b, ROOT, EntryKind::Dir, 0, 1, "data"),
            ent(300, a, EntryKind::File, 1, 1, "from-upper"),
            ent(301, b, EntryKind::File, 1, 1, "from-lower"),
        ];
        // Exact spelling picks its own folder, whichever comes first.
        let lower = MftSource::build(r"C:\data", r"C:\", &entries, ROOT).unwrap();
        assert!(lower.list("").unwrap().infos.contains_key("from-lower"));
        let upper = MftSource::build(r"C:\Data", r"C:\", &entries, ROOT).unwrap();
        assert!(upper.list("").unwrap().infos.contains_key("from-upper"));
        // No exact spelling and two case-insensitive candidates: ambiguous.
        assert!(MftSource::build(r"C:\DATA", r"C:\", &entries, ROOT).is_err());
        // One candidate: case-insensitive is fine (NTFS default).
        let one = vec![ent(a, ROOT, EntryKind::Dir, 0, 1, "Data")];
        assert!(MftSource::build(r"C:\data", r"C:\", &one, ROOT).is_ok());
    }

    #[test]
    fn l2_a_panicking_listing_becomes_an_error_instead_of_a_hang() {
        struct Boom;
        impl TreeSource for Boom {
            fn list(&self, _rel: &str) -> io::Result<DirListing> {
                panic!("boom");
            }
            fn tier(&self) -> ScanTier {
                ScanTier::Walk
            }
            fn prefetchable(&self) -> bool {
                true
            }
        }
        let cancel = AtomicBool::new(false);
        let pf = Prefetch::new(&Boom, &Boom, &cancel);
        std::thread::scope(|s| {
            let _stop = pf.finish_on_drop();
            s.spawn(|| pf.worker());
            pf.request("x");
            // The worker may take it (panic -> Err) or the walker steals it
            // (panic here, caught): either way it must not hang, so give the
            // worker the first go and treat a stolen item as the panic it is.
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                pf.take(TreeSide::Left, "x")
            }));
            if let Ok(res) = r {
                assert!(res.is_err());
            }
        });
    }

    #[test]
    fn l2_the_finish_guard_stops_workers_even_when_the_walker_panics() {
        let l = Fixed(HashMap::new(), 0.into());
        let r = Fixed(HashMap::new(), 0.into());
        let cancel = AtomicBool::new(false);
        let pf = Prefetch::new(&l, &r, &cancel);
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            std::thread::scope(|s| {
                let _stop = pf.finish_on_drop();
                s.spawn(|| pf.worker());
                s.spawn(|| pf.worker());
                panic!("walker died");
            })
        }));
        assert!(res.is_err()); // scope joined (no hang) and the panic propagated
    }
}
