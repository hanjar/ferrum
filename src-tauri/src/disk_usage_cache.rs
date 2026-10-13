//! FER-464: on-disk cache of finished Disk usage scans, ONLY for roots the
//! FerrumIndexer does not cover (non-NTFS, network, service missing / dead /
//! degraded). Index-covered NTFS roots use "Instan (indeks)" and never land
//! here.
//!
//! Storage and safety:
//! - Location: `<app local data>/du-cache` (profile override:
//!   `<FERRUM_PROFILE_DIR>/data/du-cache`). That is the CURRENT user's
//!   profile with its default ACL; a fast MFT scan carries file names the
//!   user could not list themselves (contract §7.2), so a shared location is
//!   never acceptable.
//! - File names are derived from a hash of the normalized root, never from
//!   user text. Writes are atomic: a fresh `create_new` temp file in the same
//!   directory, flushed + synced, then renamed over the target. Reads and
//!   the replace step refuse reparse points (symlink / junction) and open with
//!   `FILE_FLAG_OPEN_REPARSE_POINT`, so nothing is ever followed.
//! - Format `FDUC` v1: magic, version, length-prefixed JSON meta (root,
//!   times, engine, stats, counts), then a deflate stream of the tree in DFS
//!   pre-order (varints; child paths rebuilt from the parent) closed by a
//!   CRC-32 trailer. Any damage (truncation, bad CRC, impossible counts or
//!   parents, root mismatch) makes the file ignored: the caller falls back
//!   to a scan.
//! - Bounds: at most [`MAX_CACHE_ROOTS`] roots and [`MAX_CACHE_TOTAL_BYTES`]
//!   in total; the oldest go first. One entry larger than the total is not
//!   kept.
//! - A mutation made inside Ferrum drops a `<key>.mut` marker (timestamp);
//!   a cache whose scan time is not newer than its marker is reported
//!   `mutated` (stale for certain).
//!
//! Data read back from the cache is a snapshot like any other: Move/Delete
//! still go through the DSK-06.2 revalidation gate in the UI.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::disk_usage::{
    DiskUsageFileEntry, DiskUsageNode, DiskUsageSnapshot, DiskUsageStats, ScanEngine, unix_millis,
};

/// Roots kept on disk.
pub const MAX_CACHE_ROOTS: usize = 8;
/// Total bytes of all cache files; the oldest roots are dropped first.
pub const MAX_CACHE_TOTAL_BYTES: u64 = 512 * 1024 * 1024;
/// Default "this cache is old" threshold (Settings can change it).
pub const DEFAULT_WARN_HOURS: u32 = 24;

const MAGIC: &[u8; 4] = b"FDUC";
const FORMAT_VERSION: u8 = 1;
const MAX_META_BYTES: u32 = 1 << 20;
const MAX_STR_BYTES: u64 = 64 * 1024;
const MAX_ENTRIES: u64 = 40_000_000;
/// Hard ceiling for the decoded tree of one cache file.
const MAX_DECODED_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const EXT: &str = "fduc";
const MARKER_EXT: &str = "mut";
const TMP_EXT: &str = "tmp";

const FLAG_HAS_PARENT: u8 = 1;
const FLAG_EXPLICIT: u8 = 2;
const FLAG_DIRECT_ALLOC: u8 = 4;
const FLAG_SUBTREE_ALLOC: u8 = 8;
const FILE_SYMLINK: u8 = 1;
const FILE_ALLOC: u8 = 2;

/// One process-wide lock: saves, deletes and cap enforcement never interleave.
fn io_lock() -> &'static Mutex<()> {
    static LOCK: Mutex<()> = Mutex::new(());
    &LOCK
}

/// Mutations seen in the last minutes: a cache file is written AFTER the scan
/// finished, so a delete made in between must still mark it.
fn recent_mutations() -> &'static Mutex<Vec<(String, u64)>> {
    static RECENT: Mutex<Vec<(String, u64)>> = Mutex::new(Vec::new());
    &RECENT
}
const RECENT_MAX: usize = 64;
const RECENT_WINDOW_MS: u64 = 15 * 60 * 1000;

/// Header of a cache file (everything but the tree).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheMeta {
    pub root_path: String,
    pub scanned_at_ms: u64,
    pub saved_at_ms: u64,
    pub engine: ScanEngine,
    pub stats: DiskUsageStats,
    pub dirs: u64,
    pub files: u64,
}

/// What the UI may list / show about one cached root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheEntryInfo {
    pub root_path: String,
    pub scanned_at_ms: u64,
    pub engine: ScanEngine,
    pub size_bytes: u64,
    pub mutated: bool,
}

/// A decoded cache entry, ready to be stored as a normal snapshot.
pub struct LoadedCache {
    pub snapshot: DiskUsageSnapshot,
    pub meta: CacheMeta,
    pub mutated: bool,
}

/// `<local data>/du-cache` for the current user (profile override aware).
pub fn cache_dir<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> Option<PathBuf> {
    use tauri::Manager;
    match crate::profile::profile_dir() {
        Some(root) => Some(crate::profile::data_in(&root).join("du-cache")),
        None => app
            .path()
            .app_local_data_dir()
            .ok()
            .map(|d| d.join("du-cache")),
    }
}

/// Stable file key of a root: case-insensitive, separator-insensitive.
fn root_key(root: &str) -> String {
    let norm = root
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase();
    let digest = Sha256::digest(norm.as_bytes());
    digest[..16].iter().map(|b| format!("{b:02x}")).collect()
}

fn same_root(a: &str, b: &str) -> bool {
    root_key(a) == root_key(b)
}

fn data_path(dir: &Path, key: &str) -> PathBuf {
    dir.join(format!("{key}.{EXT}"))
}

fn marker_path(dir: &Path, key: &str) -> PathBuf {
    dir.join(format!("{key}.{MARKER_EXT}"))
}

fn is_reparse(m: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        m.file_attributes() & 0x400 != 0 // FILE_ATTRIBUTE_REPARSE_POINT
    }
    #[cfg(not(windows))]
    {
        m.file_type().is_symlink()
    }
}

/// The cache directory must be a real directory (not a link planted there).
fn ensure_dir(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let m = fs::symlink_metadata(dir)?;
    if !m.is_dir() || is_reparse(&m) {
        return Err(io::Error::other("cache directory is not a plain folder"));
    }
    Ok(())
}

/// Open a regular file for reading without ever following a reparse point.
fn open_regular(path: &Path) -> io::Result<File> {
    let m = fs::symlink_metadata(path)?;
    if !m.is_file() || is_reparse(&m) {
        return Err(io::Error::other("not a regular file"));
    }
    let mut o = OpenOptions::new();
    o.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        o.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
    }
    let f = o.open(path)?;
    let m2 = f.metadata()?;
    if !m2.is_file() || is_reparse(&m2) {
        return Err(io::Error::other("not a regular file"));
    }
    Ok(f)
}

// ── encoding primitives ─────────────────────────────────────────────────

fn put_varint(w: &mut impl Write, mut v: u64) -> io::Result<()> {
    let mut buf = [0u8; 10];
    let mut n = 0;
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            buf[n] = byte;
            n += 1;
            break;
        }
        buf[n] = byte | 0x80;
        n += 1;
    }
    w.write_all(&buf[..n])
}

fn put_str(w: &mut impl Write, s: &str) -> io::Result<()> {
    put_varint(w, s.len() as u64)?;
    w.write_all(s.as_bytes())
}

/// Hashes every byte that passes through (the trailer is written below it).
struct CrcWrite<W> {
    inner: W,
    crc: crc32fast::Hasher,
}

impl<W: Write> Write for CrcWrite<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.crc.update(&buf[..n]);
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Buffered reader that hashes bytes when they are CONSUMED, so the CRC
/// trailer (read unhashed) is not swallowed by read-ahead.
struct Rd<R> {
    inner: R,
    buf: Box<[u8]>,
    pos: usize,
    end: usize,
    crc: crc32fast::Hasher,
}

impl<R: Read> Rd<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            buf: vec![0u8; 256 * 1024].into_boxed_slice(),
            pos: 0,
            end: 0,
            crc: crc32fast::Hasher::new(),
        }
    }

    fn take(&mut self, out: &mut [u8], hashed: bool) -> io::Result<()> {
        let mut done = 0;
        while done < out.len() {
            if self.pos == self.end {
                let n = self.inner.read(&mut self.buf)?;
                if n == 0 {
                    return Err(io::ErrorKind::UnexpectedEof.into());
                }
                self.pos = 0;
                self.end = n;
            }
            let n = (out.len() - done).min(self.end - self.pos);
            out[done..done + n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
            self.pos += n;
            done += n;
        }
        if hashed {
            self.crc.update(out);
        }
        Ok(())
    }

    fn u8(&mut self) -> io::Result<u8> {
        let mut b = [0u8; 1];
        self.take(&mut b, true)?;
        Ok(b[0])
    }

    fn varint(&mut self) -> io::Result<u64> {
        let mut v = 0u64;
        for shift in (0..70).step_by(7) {
            let b = self.u8()?;
            if shift == 63 && b > 1 {
                return Err(io::Error::other("varint overflow"));
            }
            v |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err(io::Error::other("varint too long"))
    }

    fn string(&mut self) -> io::Result<String> {
        let n = self.varint()?;
        if n > MAX_STR_BYTES {
            return Err(io::Error::other("string too long"));
        }
        let mut b = vec![0u8; n as usize];
        self.take(&mut b, true)?;
        String::from_utf8(b).map_err(|_| io::Error::other("invalid utf-8"))
    }
}

// ── save ────────────────────────────────────────────────────────────────

/// Nodes reachable from the root in DFS pre-order: every parent precedes its
/// children and sibling order equals `children_ids` order, so the decoder can
/// rebuild both.
fn preorder(snap: &DiskUsageSnapshot) -> Vec<&DiskUsageNode> {
    let mut order: Vec<&DiskUsageNode> = Vec::with_capacity(snap.nodes.len());
    let mut stack: Vec<&str> = vec![snap.root_id.as_str()];
    while let Some(id) = stack.pop() {
        let Some(n) = snap.nodes.get(id) else {
            continue;
        };
        order.push(n);
        for c in n.children_ids.iter().rev() {
            stack.push(c.as_str());
        }
    }
    order
}

fn write_tree(
    w: &mut impl Write,
    snap: &DiskUsageSnapshot,
    order: &[&DiskUsageNode],
) -> io::Result<()> {
    let index: HashMap<&str, u64> = order
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id.as_str(), i as u64))
        .collect();
    put_varint(w, order.len() as u64)?;
    for (i, n) in order.iter().enumerate() {
        let parent = n
            .parent_id
            .as_deref()
            .and_then(|p| index.get(p).copied())
            .filter(|&p| (p as usize) < i);
        let derived = parent.map(|p| {
            Path::new(&order[p as usize].path)
                .join(&n.name)
                .to_string_lossy()
                .into_owned()
        });
        // The root and any node whose path/id is not the plain join of its
        // parent's path and its name are stored verbatim.
        let explicit = i == 0
            || parent.is_none()
            || derived.as_deref() != Some(n.path.as_str())
            || n.id != n.path;
        let mut flags = 0u8;
        if parent.is_some() {
            flags |= FLAG_HAS_PARENT;
        }
        if explicit {
            flags |= FLAG_EXPLICIT;
        }
        if n.direct_allocated_bytes.is_some() {
            flags |= FLAG_DIRECT_ALLOC;
        }
        if n.subtree_allocated_bytes.is_some() {
            flags |= FLAG_SUBTREE_ALLOC;
        }
        w.write_all(&[flags])?;
        if let Some(p) = parent {
            put_varint(w, p)?;
        }
        put_str(w, &n.name)?;
        if explicit {
            put_str(w, &n.path)?;
            put_str(w, &n.id)?;
        }
        put_varint(w, n.direct_files)?;
        put_varint(w, n.direct_symlinks)?;
        put_varint(w, n.direct_logical_bytes)?;
        if let Some(a) = n.direct_allocated_bytes {
            put_varint(w, a)?;
        }
        put_varint(w, n.subtree_files)?;
        put_varint(w, n.subtree_dirs)?;
        put_varint(w, n.subtree_symlinks)?;
        put_varint(w, n.subtree_logical_bytes)?;
        if let Some(a) = n.subtree_allocated_bytes {
            put_varint(w, a)?;
        }
        put_varint(w, n.subtree_errors)?;
        put_varint(w, n.skipped_volume_boundaries)?;
        put_varint(w, n.subtree_skipped_volumes)?;
        let list: &[DiskUsageFileEntry] = snap
            .files_by_parent
            .get(&n.id)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        put_varint(w, list.len() as u64)?;
        for f in list {
            put_str(w, &f.name)?;
            put_varint(w, f.logical_bytes)?;
            let mut ff = 0u8;
            if f.is_symlink {
                ff |= FILE_SYMLINK;
            }
            if f.allocated_bytes.is_some() {
                ff |= FILE_ALLOC;
            }
            w.write_all(&[ff])?;
            if let Some(a) = f.allocated_bytes {
                put_varint(w, a)?;
            }
        }
    }
    Ok(())
}

/// Write `snap` as the cache of its root (atomic replace), then apply the
/// caps. Returns the size of the file written. Cancelled scans are refused.
pub fn save(dir: &Path, snap: &DiskUsageSnapshot, engine: ScanEngine) -> io::Result<u64> {
    if snap.stats.cancelled {
        return Err(io::Error::other("cancelled scans are not cached"));
    }
    let _guard = io_lock().lock().unwrap_or_else(|e| e.into_inner());
    ensure_dir(dir)?;
    let key = root_key(&snap.root_path);
    let dest = data_path(dir, &key);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = dir.join(format!("{key}.{}.{nanos}.{TMP_EXT}", std::process::id()));
    let result = write_file(&tmp, snap, engine).and_then(|()| {
        // Never rename over something that is not a plain file we wrote.
        if let Ok(m) = fs::symlink_metadata(&dest)
            && (!m.is_file() || is_reparse(&m))
        {
            return Err(io::Error::other("cache target is not a plain file"));
        }
        fs::rename(&tmp, &dest)
    });
    if let Err(e) = result {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    let size = fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
    // A mutation after the scan but before this write marks the new file.
    let touched = recent_mutations()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .any(|(p, at)| *at >= snap.scanned_at_ms && touches(p, &snap.root_path));
    if touched {
        let _ = write_marker(dir, &key);
    }
    enforce_caps_locked(dir);
    Ok(size)
}

fn write_file(tmp: &Path, snap: &DiskUsageSnapshot, engine: ScanEngine) -> io::Result<()> {
    use flate2::{Compression, write::DeflateEncoder};
    let file = OpenOptions::new().write(true).create_new(true).open(tmp)?;
    let mut out = BufWriter::with_capacity(1 << 20, file);
    let order = preorder(snap);
    let dirs = order.len() as u64;
    let files: u64 = order
        .iter()
        .map(|n| {
            snap.files_by_parent
                .get(&n.id)
                .map_or(0, |v| v.len() as u64)
        })
        .sum();
    let meta = CacheMeta {
        root_path: snap.root_path.clone(),
        scanned_at_ms: snap.scanned_at_ms,
        saved_at_ms: unix_millis(),
        engine,
        stats: snap.stats.clone(),
        dirs,
        files,
    };
    let meta_json = serde_json::to_vec(&meta).map_err(io::Error::other)?;
    out.write_all(MAGIC)?;
    out.write_all(&[FORMAT_VERSION])?;
    out.write_all(&(meta_json.len() as u32).to_le_bytes())?;
    out.write_all(&meta_json)?;
    let enc = DeflateEncoder::new(out, Compression::new(3));
    let mut body = BufWriter::with_capacity(
        1 << 20,
        CrcWrite {
            inner: enc,
            crc: crc32fast::Hasher::new(),
        },
    );
    write_tree(&mut body, snap, &order)?;
    let CrcWrite { mut inner, crc } = body.into_inner().map_err(|e| e.into_error())?;
    inner.write_all(&crc.finalize().to_le_bytes())?;
    let out = inner.finish()?;
    let file = out.into_inner().map_err(|e| e.into_error())?;
    file.sync_all()
}

// ── load ────────────────────────────────────────────────────────────────

fn read_header(f: &mut File) -> io::Result<CacheMeta> {
    let mut head = [0u8; 9];
    f.read_exact(&mut head)?;
    if &head[..4] != MAGIC || head[4] != FORMAT_VERSION {
        return Err(io::Error::other("unknown cache format"));
    }
    let len = u32::from_le_bytes([head[5], head[6], head[7], head[8]]);
    if len == 0 || len > MAX_META_BYTES {
        return Err(io::Error::other("bad meta length"));
    }
    let mut buf = vec![0u8; len as usize];
    f.read_exact(&mut buf)?;
    serde_json::from_slice(&buf).map_err(io::Error::other)
}

fn marker_ms(dir: &Path, key: &str) -> Option<u64> {
    let f = open_regular(&marker_path(dir, key)).ok()?;
    let mut s = String::new();
    f.take(32).read_to_string(&mut s).ok()?;
    s.trim().parse().ok()
}

fn is_mutated(dir: &Path, key: &str, scanned_at_ms: u64) -> bool {
    marker_ms(dir, key).is_some_and(|m| m >= scanned_at_ms)
}

/// Decoded bytes allowed for a cache file of `file_len` bytes (C1): 8x the
/// file, never more than 2 GiB. A real tree compresses 3-5x; a decompression
/// bomb is stopped inside the inflate stream.
fn decoded_limit(file_len: u64) -> u64 {
    file_len.saturating_mul(8).min(MAX_DECODED_BYTES)
}

/// Reader that fails (not EOF) once more than `left` bytes were produced.
struct Limited<R> {
    inner: R,
    left: u64,
}

impl<R: Read> Read for Limited<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.left == 0 {
            return Err(io::Error::other("decoded size over the limit"));
        }
        let max = buf.len().min(self.left.min(usize::MAX as u64) as usize);
        let n = self.inner.read(&mut buf[..max])?;
        self.left -= n as u64;
        Ok(n)
    }
}

/// A file or folder name from the cache (C2): one plain path component.
fn valid_name(s: &str) -> bool {
    !s.is_empty()
        && s != "."
        && s != ".."
        && !s
            .chars()
            .any(|c| c.is_control() || matches!(c, '\\' | '/' | ':'))
}

fn bad(msg: &'static str) -> io::Error {
    io::Error::other(msg)
}

fn decode(
    f: File,
    file_len: u64,
    meta: &CacheMeta,
    scan_id: String,
    version: u64,
) -> io::Result<DiskUsageSnapshot> {
    use flate2::read::DeflateDecoder;
    if meta.dirs == 0 || meta.dirs > MAX_ENTRIES || meta.files > MAX_ENTRIES {
        return Err(io::Error::other("implausible counts"));
    }
    let mut rd = Rd::new(Limited {
        inner: DeflateDecoder::new(f),
        left: decoded_limit(file_len),
    });
    let n = rd.varint()?;
    if n != meta.dirs {
        return Err(io::Error::other("dir count mismatch"));
    }
    let mut nodes: Vec<DiskUsageNode> = Vec::with_capacity((n as usize).min(1 << 16));
    let mut files_by_parent: HashMap<String, Vec<DiskUsageFileEntry>> = HashMap::new();
    let mut total_files = 0u64;
    for i in 0..n as usize {
        let flags = rd.u8()?;
        if flags & !(FLAG_HAS_PARENT | FLAG_EXPLICIT | FLAG_DIRECT_ALLOC | FLAG_SUBTREE_ALLOC) != 0
        {
            return Err(io::Error::other("bad flags"));
        }
        let parent = if flags & FLAG_HAS_PARENT != 0 {
            let p = rd.varint()? as usize;
            if p >= i {
                return Err(io::Error::other("parent after child"));
            }
            Some(p)
        } else if i == 0 {
            None
        } else {
            return Err(io::Error::other("orphan node"));
        };
        let name = rd.string()?;
        if parent.is_some() && !valid_name(&name) {
            return Err(bad("invalid folder name"));
        }
        let (path, id) = if flags & FLAG_EXPLICIT != 0 {
            let (path, id) = (rd.string()?, rd.string()?);
            if parent.is_some()
                && (id != path
                    || !crate::indexer_client::path_within(&path, &meta.root_path)
                    || path.contains('\0'))
            {
                return Err(bad("node outside the cached root"));
            }
            (path, id)
        } else {
            let p = parent.ok_or_else(|| io::Error::other("root must be explicit"))?;
            let path = Path::new(&nodes[p].path)
                .join(&name)
                .to_string_lossy()
                .into_owned();
            (path.clone(), path)
        };
        let direct_files = rd.varint()?;
        let direct_symlinks = rd.varint()?;
        let direct_logical_bytes = rd.varint()?;
        let direct_allocated_bytes = if flags & FLAG_DIRECT_ALLOC != 0 {
            Some(rd.varint()?)
        } else {
            None
        };
        let subtree_files = rd.varint()?;
        let subtree_dirs = rd.varint()?;
        let subtree_symlinks = rd.varint()?;
        let subtree_logical_bytes = rd.varint()?;
        let subtree_allocated_bytes = if flags & FLAG_SUBTREE_ALLOC != 0 {
            Some(rd.varint()?)
        } else {
            None
        };
        let subtree_errors = rd.varint()?;
        let skipped_volume_boundaries = rd.varint()?;
        let subtree_skipped_volumes = rd.varint()?;
        let count = rd.varint()?;
        total_files = total_files.saturating_add(count);
        if total_files > MAX_ENTRIES {
            return Err(io::Error::other("too many files"));
        }
        if count > 0 {
            let mut list = Vec::with_capacity((count as usize).min(1 << 16));
            for _ in 0..count {
                let fname = rd.string()?;
                if !valid_name(&fname) {
                    return Err(bad("invalid file name"));
                }
                let logical_bytes = rd.varint()?;
                let ff = rd.u8()?;
                if ff & !(FILE_SYMLINK | FILE_ALLOC) != 0 {
                    return Err(io::Error::other("bad file flags"));
                }
                let allocated_bytes = if ff & FILE_ALLOC != 0 {
                    Some(rd.varint()?)
                } else {
                    None
                };
                list.push(DiskUsageFileEntry {
                    name: fname,
                    logical_bytes,
                    allocated_bytes,
                    is_symlink: ff & FILE_SYMLINK != 0,
                });
            }
            files_by_parent.insert(id.clone(), list);
        }
        let parent_id = parent.map(|p| nodes[p].id.clone());
        if let Some(p) = parent {
            nodes[p].children_ids.push(id.clone());
        }
        nodes.push(DiskUsageNode {
            id,
            path,
            name,
            parent_id,
            children_ids: Vec::new(),
            direct_files,
            direct_symlinks,
            direct_logical_bytes,
            direct_allocated_bytes,
            subtree_files,
            subtree_dirs,
            subtree_symlinks,
            subtree_logical_bytes,
            subtree_allocated_bytes,
            subtree_errors,
            skipped_volume_boundaries,
            subtree_skipped_volumes,
        });
    }
    if total_files != meta.files {
        return Err(io::Error::other("file count mismatch"));
    }
    let expected = std::mem::take(&mut rd.crc).finalize();
    let mut tail = [0u8; 4];
    rd.take(&mut tail, false)?;
    if u32::from_le_bytes(tail) != expected {
        return Err(io::Error::other("checksum mismatch"));
    }
    let root_id = nodes[0].id.clone();
    if nodes[0].path != meta.root_path && !same_root(&nodes[0].path, &meta.root_path) {
        return Err(io::Error::other("root mismatch"));
    }
    let map: HashMap<String, DiskUsageNode> =
        nodes.into_iter().map(|n| (n.id.clone(), n)).collect();
    if map.len() as u64 != n {
        return Err(io::Error::other("duplicate node ids"));
    }
    Ok(DiskUsageSnapshot {
        scan_id,
        root_path: meta.root_path.clone(),
        root_id,
        version,
        scanned_at_ms: meta.scanned_at_ms,
        nodes: map,
        files_by_parent,
        stats: meta.stats.clone(),
    })
}

/// Decode the cache of `root`, or `None` when there is none or the file is
/// damaged in any way (the caller then simply scans).
pub fn load(dir: &Path, root: &str, scan_id: String, version: u64) -> Option<LoadedCache> {
    let key = root_key(root);
    let path = data_path(dir, &key);
    let mut f = open_regular(&path).ok()?;
    if f.metadata().ok()?.len() > MAX_CACHE_TOTAL_BYTES {
        return None;
    }
    let meta = read_header(&mut f).ok()?;
    if !same_root(&meta.root_path, root) {
        return None;
    }
    let file_len = f.metadata().ok()?.len();
    let snapshot = match decode(f, file_len, &meta, scan_id, version) {
        Ok(s) => s,
        Err(e) => {
            log::info!("disk usage cache ignored: {e}");
            return None;
        }
    };
    let mutated = is_mutated(dir, &key, meta.scanned_at_ms);
    Some(LoadedCache {
        snapshot,
        meta,
        mutated,
    })
}

// ── list / delete / mutation markers / caps ─────────────────────────────

/// Header-only listing of every readable cache file.
pub fn list(dir: &Path) -> Vec<CacheEntryInfo> {
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(dir) else {
        return out;
    };
    for ent in rd.flatten() {
        let path = ent.path();
        if path.extension().and_then(|e| e.to_str()) != Some(EXT) {
            continue;
        }
        let Ok(mut f) = open_regular(&path) else {
            continue;
        };
        let size = f.metadata().map(|m| m.len()).unwrap_or(0);
        let Ok(meta) = read_header(&mut f) else {
            continue;
        };
        let key = root_key(&meta.root_path);
        if path.file_stem().and_then(|s| s.to_str()) != Some(key.as_str()) {
            continue; // a file that does not belong to the root it claims
        }
        out.push(CacheEntryInfo {
            mutated: is_mutated(dir, &key, meta.scanned_at_ms),
            root_path: meta.root_path,
            scanned_at_ms: meta.scanned_at_ms,
            engine: meta.engine,
            size_bytes: size,
        });
    }
    out.sort_by(|a, b| a.root_path.cmp(&b.root_path));
    out
}

/// Remove the cache of one root (and its mutation marker). True if a cache
/// file existed.
pub fn delete(dir: &Path, root: &str) -> bool {
    let _guard = io_lock().lock().unwrap_or_else(|e| e.into_inner());
    let key = root_key(root);
    let existed = fs::remove_file(data_path(dir, &key)).is_ok();
    let _ = fs::remove_file(marker_path(dir, &key));
    existed
}

/// Remove every cache file; returns how many roots were dropped.
pub fn clear(dir: &Path) -> usize {
    let _guard = io_lock().lock().unwrap_or_else(|e| e.into_inner());
    let mut removed = 0;
    let Ok(rd) = fs::read_dir(dir) else {
        return 0;
    };
    for ent in rd.flatten() {
        let path = ent.path();
        match path.extension().and_then(|e| e.to_str()) {
            Some(EXT) => {
                if fs::remove_file(&path).is_ok() {
                    removed += 1;
                }
            }
            Some(MARKER_EXT) | Some(TMP_EXT) => {
                let _ = fs::remove_file(&path);
            }
            _ => {}
        }
    }
    removed
}

/// A mutation (delete / rename / move) touched `paths`: every cached root
/// that contains one of them, or lies inside one, is now certainly stale.
/// Returns how many roots were marked.
pub fn note_mutation(dir: &Path, paths: &[String]) -> usize {
    let _guard = io_lock().lock().unwrap_or_else(|e| e.into_inner());
    {
        let now = unix_millis();
        let mut recent = recent_mutations().lock().unwrap_or_else(|e| e.into_inner());
        recent.retain(|(_, at)| now.saturating_sub(*at) <= RECENT_WINDOW_MS);
        for p in paths {
            recent.push((p.clone(), now));
        }
        let excess = recent.len().saturating_sub(RECENT_MAX);
        recent.drain(..excess);
    }
    let mut marked = 0;
    for entry in list(dir) {
        if !paths.iter().any(|p| touches(p, &entry.root_path)) {
            continue;
        }
        let key = root_key(&entry.root_path);
        if write_marker(dir, &key).is_ok() {
            marked += 1;
        }
    }
    marked
}

/// `path` is the root, inside it, or contains it (a parent was deleted).
fn touches(path: &str, root: &str) -> bool {
    use crate::indexer_client::path_within;
    path_within(path, root) || path_within(root, path)
}

fn write_marker(dir: &Path, key: &str) -> io::Result<()> {
    let dest = marker_path(dir, key);
    if let Ok(m) = fs::symlink_metadata(&dest)
        && (!m.is_file() || is_reparse(&m))
    {
        return Err(io::Error::other("marker target is not a plain file"));
    }
    let tmp = dir.join(format!("{key}.{}.m.{TMP_EXT}", std::process::id()));
    let _ = fs::remove_file(&tmp);
    let mut f = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
    let r = f
        .write_all(unix_millis().to_string().as_bytes())
        .and_then(|()| f.sync_all());
    drop(f);
    match r.and_then(|()| fs::rename(&tmp, &dest)) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Count and size caps, oldest first; also sweeps orphan markers and
/// leftover temp files. Caller holds the io lock.
fn enforce_caps_locked(dir: &Path) {
    struct Item {
        path: PathBuf,
        key: String,
        saved: u64,
        size: u64,
    }
    let mut items: Vec<Item> = Vec::new();
    let Ok(rd) = fs::read_dir(dir) else {
        return;
    };
    let now = std::time::SystemTime::now();
    for ent in rd.flatten() {
        let path = ent.path();
        match path.extension().and_then(|e| e.to_str()) {
            Some(EXT) => {
                let Ok(mut f) = open_regular(&path) else {
                    let _ = fs::remove_file(&path);
                    continue;
                };
                let size = f.metadata().map(|m| m.len()).unwrap_or(0);
                match read_header(&mut f) {
                    Ok(meta) => items.push(Item {
                        key: root_key(&meta.root_path),
                        path,
                        saved: meta.saved_at_ms,
                        size,
                    }),
                    Err(_) => {
                        drop(f);
                        let _ = fs::remove_file(&path); // unreadable: not worth keeping
                    }
                }
            }
            Some(TMP_EXT) => {
                let old = ent
                    .metadata()
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| now.duration_since(t).ok())
                    .is_some_and(|d| d.as_secs() > 600);
                if old {
                    let _ = fs::remove_file(&path);
                }
            }
            _ => {}
        }
    }
    items.sort_by_key(|i| i.saved); // oldest first
    let mut total: u64 = items.iter().map(|i| i.size).sum();
    let mut count = items.len();
    for it in &items {
        if count <= MAX_CACHE_ROOTS && total <= MAX_CACHE_TOTAL_BYTES {
            break;
        }
        if fs::remove_file(&it.path).is_ok() {
            let _ = fs::remove_file(marker_path(dir, &it.key));
            total = total.saturating_sub(it.size);
            count -= 1;
        }
    }
    // Orphan markers (their cache is gone).
    if let Ok(rd) = fs::read_dir(dir) {
        for ent in rd.flatten() {
            let p = ent.path();
            if p.extension().and_then(|e| e.to_str()) == Some(MARKER_EXT)
                && !p.with_extension(EXT).exists()
            {
                let _ = fs::remove_file(&p);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disk_usage::Coverage;

    fn node(id: &str, name: &str, parent: Option<&str>) -> DiskUsageNode {
        DiskUsageNode {
            id: id.into(),
            path: id.into(),
            name: name.into(),
            parent_id: parent.map(Into::into),
            children_ids: Vec::new(),
            direct_files: 0,
            direct_symlinks: 0,
            direct_logical_bytes: 0,
            direct_allocated_bytes: Some(0),
            subtree_files: 0,
            subtree_dirs: 0,
            subtree_symlinks: 0,
            subtree_logical_bytes: 0,
            subtree_allocated_bytes: Some(0),
            subtree_errors: 0,
            skipped_volume_boundaries: 0,
            subtree_skipped_volumes: 0,
        }
    }

    fn stats(files: u64, dirs: u64) -> DiskUsageStats {
        DiskUsageStats {
            scanned_files: files,
            scanned_dirs: dirs,
            scanned_symlinks: 1,
            skipped_volume_boundaries: 0,
            logical_bytes: 600,
            allocated_available_files: files,
            allocated_unavailable_files: 0,
            known_allocated_bytes: Some(1200),
            total_errors: 0,
            coverage: Coverage::Complete,
            cancelled: false,
            depth_capped: false,
            duration_ms: 5,
        }
    }

    /// root -> a (2 files + 1 symlink, alloc unknown on one) , b (empty).
    fn sample(root: &str) -> DiskUsageSnapshot {
        let a = format!("{root}\\a");
        let b = format!("{root}\\b");
        let mut r = node(root, root, None);
        let mut na = node(&a, "a", Some(root));
        let nb = node(&b, "b", Some(root));
        r.children_ids = vec![a.clone(), b.clone()];
        na.direct_files = 2;
        na.direct_symlinks = 1;
        na.direct_logical_bytes = 600;
        na.direct_allocated_bytes = None; // null semantics must survive
        na.subtree_files = 2;
        na.subtree_logical_bytes = 600;
        na.subtree_allocated_bytes = None;
        r.subtree_files = 2;
        r.subtree_dirs = 2;
        r.subtree_logical_bytes = 600;
        r.subtree_allocated_bytes = None;
        let mut files = HashMap::new();
        files.insert(
            a.clone(),
            vec![
                DiskUsageFileEntry {
                    name: "x.bin".into(),
                    logical_bytes: 400,
                    allocated_bytes: Some(4096),
                    is_symlink: false,
                },
                DiskUsageFileEntry {
                    name: "ünï.txt".into(),
                    logical_bytes: 200,
                    allocated_bytes: None,
                    is_symlink: false,
                },
                DiskUsageFileEntry {
                    name: "link".into(),
                    logical_bytes: 0,
                    allocated_bytes: None,
                    is_symlink: true,
                },
            ],
        );
        let mut nodes = HashMap::new();
        for n in [r, na, nb] {
            nodes.insert(n.id.clone(), n);
        }
        DiskUsageSnapshot {
            scan_id: "scan-1".into(),
            root_path: root.into(),
            root_id: root.into(),
            version: 7,
            scanned_at_ms: 1_700_000_000_000,
            nodes,
            files_by_parent: files,
            stats: stats(2, 3),
        }
    }

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "ferrum-du-cache-{tag}-{}-{}",
            std::process::id(),
            unix_millis()
        ));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn round_trip_keeps_tree_files_stats_engine_and_null_allocation() {
        let dir = tmp("rt");
        let snap = sample("E:\\data");
        let size = save(&dir, &snap, ScanEngine::NtfsMft).unwrap();
        assert!(size > 0);
        let got = load(&dir, "e:/DATA/", "scan-2".into(), 99).expect("loads");
        assert_eq!(got.meta.engine, ScanEngine::NtfsMft);
        assert!(!got.mutated);
        let s = got.snapshot;
        assert_eq!(s.scan_id, "scan-2");
        assert_eq!(s.version, 99);
        assert_eq!(s.scanned_at_ms, snap.scanned_at_ms);
        assert_eq!(s.root_id, snap.root_id);
        assert_eq!(s.stats, snap.stats);
        assert_eq!(s.nodes, snap.nodes, "children order, paths, null alloc");
        assert_eq!(s.files_by_parent, snap.files_by_parent);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn nodes_with_odd_paths_are_stored_verbatim() {
        let dir = tmp("odd");
        let mut snap = sample("E:\\data");
        // id/path not equal to join(parent, name): must not be "derived".
        let mut odd = snap.nodes.remove("E:\\data\\b").unwrap();
        odd.id = "E:\\data\\B-renamed".into();
        odd.path = odd.id.clone();
        let root = snap.nodes.get_mut("E:\\data").unwrap();
        root.children_ids[1] = odd.id.clone();
        snap.nodes.insert(odd.id.clone(), odd);
        let got = save(&dir, &snap, ScanEngine::Walk)
            .ok()
            .and_then(|_| load(&dir, "E:\\data", "s".into(), 1))
            .expect("loads");
        assert_eq!(got.snapshot.nodes, snap.nodes);
        let _ = fs::remove_dir_all(&dir);
    }

    /// Build a cache file from a raw (uncompressed) body, with a valid CRC.
    fn forge(dir: &Path, root: &str, dirs: u64, files: u64, body: &[u8]) -> PathBuf {
        use flate2::{Compression, write::DeflateEncoder};
        fs::create_dir_all(dir).unwrap();
        let mut snap = sample(root);
        snap.stats.cancelled = false;
        let meta = CacheMeta {
            root_path: root.into(),
            scanned_at_ms: 1,
            saved_at_ms: 1,
            engine: ScanEngine::Walk,
            stats: snap.stats.clone(),
            dirs,
            files,
        };
        let json = serde_json::to_vec(&meta).unwrap();
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.push(FORMAT_VERSION);
        out.extend_from_slice(&(json.len() as u32).to_le_bytes());
        out.extend_from_slice(&json);
        let mut enc = DeflateEncoder::new(out, Compression::new(6));
        enc.write_all(body).unwrap();
        let mut crc = crc32fast::Hasher::new();
        crc.update(body);
        enc.write_all(&crc.finalize().to_le_bytes()).unwrap();
        let bytes = enc.finish().unwrap();
        let path = data_path(dir, &root_key(root));
        fs::write(&path, bytes).unwrap();
        path
    }

    fn vs(out: &mut Vec<u8>, s: &str) {
        put_varint(out, s.len() as u64).unwrap();
        out.extend_from_slice(s.as_bytes());
    }

    /// Body: root plus one child dir `name` (explicit path/id when given),
    /// one file `fname` in the child.
    fn body(name: &str, explicit: Option<(&str, &str)>, fname: &str) -> Vec<u8> {
        let mut b = Vec::new();
        put_varint(&mut b, 2).unwrap();
        // root: explicit, no parent, all-zero numbers
        b.push(FLAG_EXPLICIT);
        vs(&mut b, "E:\\r");
        vs(&mut b, "E:\\r");
        vs(&mut b, "E:\\r");
        for _ in 0..10 {
            put_varint(&mut b, 0).unwrap();
        }
        put_varint(&mut b, 0).unwrap(); // no files in root
        // child
        b.push(FLAG_HAS_PARENT | if explicit.is_some() { FLAG_EXPLICIT } else { 0 });
        put_varint(&mut b, 0).unwrap();
        vs(&mut b, name);
        if let Some((p, i)) = explicit {
            vs(&mut b, p);
            vs(&mut b, i);
        }
        for _ in 0..10 {
            put_varint(&mut b, 0).unwrap();
        }
        put_varint(&mut b, 1).unwrap(); // one file
        vs(&mut b, fname);
        put_varint(&mut b, 5).unwrap();
        b.push(0);
        b
    }

    fn loads(dir: &Path, b: &[u8]) -> bool {
        forge(dir, "E:\\r", 2, 1, b);
        load(dir, "E:\\r", "s".into(), 1).is_some()
    }

    #[test]
    fn forged_file_helper_is_valid_when_names_are_sane() {
        let dir = tmp("forge-ok");
        assert!(loads(&dir, &body("sub", None, "a.txt")));
        assert!(loads(
            &dir,
            &body("sub", Some(("E:\\r\\sub", "E:\\r\\sub")), "a.txt")
        ));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn names_that_are_not_one_plain_component_are_rejected() {
        let dir = tmp("names");
        for bad_name in [
            "",
            ".",
            "..",
            "a\\b",
            "a/b",
            "c:d",
            "x\u{0}y",
            "tab\there",
            "e\u{7f}",
        ] {
            assert!(
                !loads(&dir, &body(bad_name, None, "a.txt")),
                "dir {bad_name:?}"
            );
            assert!(
                !loads(&dir, &body("sub", None, bad_name)),
                "file {bad_name:?}"
            );
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn explicit_paths_must_stay_under_the_root_with_id_equal_to_path() {
        let dir = tmp("paths");
        // Escapes the cached root.
        assert!(!loads(
            &dir,
            &body("sub", Some(("F:\\other", "F:\\other")), "a")
        ));
        assert!(!loads(
            &dir,
            &body("sub", Some(("E:\\rx\\sub", "E:\\rx\\sub")), "a")
        ));
        // The root's own path is not "below" via a sibling prefix or parent.
        assert!(!loads(&dir, &body("sub", Some(("E:\\", "E:\\")), "a")));
        // id differs from path.
        assert!(!loads(
            &dir,
            &body("sub", Some(("E:\\r\\sub", "E:\\r\\other")), "a")
        ));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn decompression_bomb_is_stopped_by_the_decoded_size_limit() {
        let dir = tmp("bomb");
        // 1 dir, 600 files with 60 KB names of one repeated byte: ~36 MB
        // decoded from a few tens of KB on disk.
        let long = "a".repeat(60_000);
        let mut b = Vec::new();
        put_varint(&mut b, 1).unwrap();
        b.push(FLAG_EXPLICIT);
        vs(&mut b, "E:\\r");
        vs(&mut b, "E:\\r");
        vs(&mut b, "E:\\r");
        for _ in 0..10 {
            put_varint(&mut b, 0).unwrap();
        }
        put_varint(&mut b, 600).unwrap();
        for _ in 0..600 {
            vs(&mut b, &long);
            put_varint(&mut b, 1).unwrap();
            b.push(0);
        }
        let path = forge(&dir, "E:\\r", 1, 600, &b);
        let len = fs::metadata(&path).unwrap().len();
        assert!(
            b.len() as u64 > decoded_limit(len),
            "fixture must exceed the limit"
        );
        assert!(load(&dir, "E:\\r", "s".into(), 1).is_none());
        // Same body with a file big enough for it is accepted: the limit,
        // not the content, is what refused it.
        let mut r = Limited {
            inner: &b[..],
            left: b.len() as u64 + 1,
        };
        let mut sink = Vec::new();
        assert!(r.read_to_end(&mut sink).is_ok());
        let mut r = Limited {
            inner: &b[..],
            left: 10,
        };
        assert!(r.read_to_end(&mut sink).is_err());
        assert_eq!(decoded_limit(10), 80);
        assert_eq!(decoded_limit(u64::MAX), MAX_DECODED_BYTES);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn damaged_files_are_ignored() {
        let dir = tmp("bad");
        let snap = sample("E:\\data");
        save(&dir, &snap, ScanEngine::Walk).unwrap();
        let path = data_path(&dir, &root_key("E:\\data"));
        let good = fs::read(&path).unwrap();
        // Truncated.
        fs::write(&path, &good[..good.len() - 7]).unwrap();
        assert!(load(&dir, "E:\\data", "s".into(), 1).is_none());
        // Bit flip in the body.
        let mut flipped = good.clone();
        let mid = flipped.len() - 10;
        flipped[mid] ^= 0xff;
        fs::write(&path, &flipped).unwrap();
        assert!(load(&dir, "E:\\data", "s".into(), 1).is_none());
        // Garbage / wrong magic / empty.
        fs::write(&path, b"not a cache at all").unwrap();
        assert!(load(&dir, "E:\\data", "s".into(), 1).is_none());
        fs::write(&path, b"").unwrap();
        assert!(load(&dir, "E:\\data", "s".into(), 1).is_none());
        // And a good file still loads afterwards.
        fs::write(&path, &good).unwrap();
        assert!(load(&dir, "E:\\data", "s".into(), 1).is_some());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_that_claims_another_root_is_refused() {
        let dir = tmp("claim");
        save(&dir, &sample("E:\\data"), ScanEngine::Walk).unwrap();
        // Copy E:'s cache under F:'s file name.
        let from = data_path(&dir, &root_key("E:\\data"));
        let to = data_path(&dir, &root_key("F:\\other"));
        fs::copy(&from, &to).unwrap();
        assert!(load(&dir, "F:\\other", "s".into(), 1).is_none());
        assert!(list(&dir).iter().all(|e| e.root_path == "E:\\data"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cancelled_scans_are_not_cached() {
        let dir = tmp("cancel");
        let mut snap = sample("E:\\data");
        snap.stats.cancelled = true;
        assert!(save(&dir, &snap, ScanEngine::Walk).is_err());
        assert!(list(&dir).is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn root_count_is_capped_and_oldest_goes_first() {
        let dir = tmp("cap");
        for i in 0..(MAX_CACHE_ROOTS + 3) {
            let root = format!("E:\\r{i}");
            let mut snap = sample(&root);
            snap.scanned_at_ms += i as u64;
            save(&dir, &snap, ScanEngine::Walk).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(3)); // distinct savedAt
        }
        let roots: Vec<String> = list(&dir).into_iter().map(|e| e.root_path).collect();
        assert_eq!(roots.len(), MAX_CACHE_ROOTS);
        assert!(!roots.contains(&"E:\\r0".to_string()));
        assert!(roots.contains(&format!("E:\\r{}", MAX_CACHE_ROOTS + 2)));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn mutation_marks_cache_and_a_newer_scan_clears_it() {
        let dir = tmp("mut");
        // A root of its own: the recent-mutation list is process-wide.
        let root = "E:\\mutroot";
        let mut snap = sample(root);
        snap.scanned_at_ms = unix_millis().saturating_sub(5_000);
        save(&dir, &snap, ScanEngine::Walk).unwrap();
        assert!(!load(&dir, root, "s".into(), 1).unwrap().mutated);
        // Unrelated path: untouched. A path inside the root: marked.
        assert_eq!(note_mutation(&dir, &["F:\\elsewhere\\x".into()]), 0);
        assert_eq!(note_mutation(&dir, &["e:/mutroot/a/x.bin".into()]), 1);
        assert!(load(&dir, root, "s".into(), 1).unwrap().mutated);
        assert!(list(&dir)[0].mutated);
        // A scan finished after the mutation is fresh again.
        let mut again = sample(root);
        again.scanned_at_ms = unix_millis() + 2_000;
        save(&dir, &again, ScanEngine::Walk).unwrap();
        assert!(!load(&dir, root, "s".into(), 1).unwrap().mutated);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_mutation_between_scan_end_and_cache_write_still_marks_the_new_file() {
        let dir = tmp("race");
        let root = "E:\\raceroot";
        let mut snap = sample(root);
        snap.scanned_at_ms = unix_millis().saturating_sub(1_000);
        // The user deletes something right after the scan finished, before
        // the (async) cache file exists.
        assert_eq!(note_mutation(&dir, &["E:\\raceroot\\a\\x".into()]), 0);
        save(&dir, &snap, ScanEngine::Walk).unwrap();
        assert!(load(&dir, root, "s".into(), 1).unwrap().mutated);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn delete_and_clear_remove_files_and_markers() {
        let dir = tmp("del");
        save(&dir, &sample("E:\\a"), ScanEngine::Walk).unwrap();
        save(&dir, &sample("E:\\b"), ScanEngine::Walk).unwrap();
        note_mutation(&dir, &["E:\\a\\x".into()]);
        assert!(delete(&dir, "e:\\A"));
        assert!(!delete(&dir, "E:\\a"), "already gone");
        assert!(!marker_path(&dir, &root_key("E:\\a")).exists());
        assert_eq!(list(&dir).len(), 1);
        assert_eq!(clear(&dir), 1);
        assert!(list(&dir).is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_leaves_no_temp_files_and_replaces_atomically() {
        let dir = tmp("atomic");
        save(&dir, &sample("E:\\data"), ScanEngine::Walk).unwrap();
        save(&dir, &sample("E:\\data"), ScanEngine::NtfsMft).unwrap();
        let names: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 1, "{names:?}");
        assert!(names[0].ends_with(".fduc"));
        assert_eq!(list(&dir)[0].engine, ScanEngine::NtfsMft);
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn a_link_in_place_of_the_cache_file_is_never_followed() {
        let dir = tmp("link");
        fs::create_dir_all(&dir).unwrap();
        let secret = dir.join("secret.txt");
        fs::write(&secret, b"top secret").unwrap();
        let target = data_path(&dir, &root_key("E:\\data"));
        // Needs the symlink privilege / developer mode; skip quietly if absent.
        if std::os::windows::fs::symlink_file(&secret, &target).is_err() {
            let _ = fs::remove_dir_all(&dir);
            return;
        }
        assert!(
            load(&dir, "E:\\data", "s".into(), 1).is_none(),
            "load refuses links"
        );
        assert!(
            save(&dir, &sample("E:\\data"), ScanEngine::Walk).is_err(),
            "save refuses to replace a link"
        );
        assert_eq!(fs::read(&secret).unwrap(), b"top secret");
        let _ = fs::remove_dir_all(&dir);
    }
}
