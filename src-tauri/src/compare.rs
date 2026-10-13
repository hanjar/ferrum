//! Compare engines (F2-18, FER-389): binary file comparison streamed in
//! chunks with a bounded text diff, and directory comparison (one level or
//! recursive) between the two pane roots. No function here writes to the
//! filesystem — compare is strictly read-only. See `docs/compare-sync.md`.
//!
//! Decisions: a 2-second timestamp tolerance absorbs FAT/exFAT 2-second
//! mtime resolution so re-comparing after a plain copy does not report
//! phantom differences; symlinks/junctions are entries, never followed
//! (D07), so a recursive walk cannot loop; entries that cannot be read are
//! reported in `errors`, never silently skipped.

use crate::domain::{ErrorCode, FsError, validate_path};
use crate::tree_source::{DirListing, DiskSource, FastScanInfo, Prefetch, TreeSide, TreeSource};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, UNIX_EPOCH};

const CHUNK: usize = 64 * 1024;
/// mtime tolerance in ms (FAT stores mtimes with 2 s resolution).
const MTIME_TOLERANCE_MS: i64 = 2000;
/// Line numbers are only scanned within this bound of the first mismatch.
const LINE_SCAN_LIMIT: u64 = 8 * 1024 * 1024;
/// Text diff bounds, per side.
const DIFF_MAX_LINES: usize = 2000;
const DIFF_MAX_BYTES: usize = 256 * 1024;
const DEFAULT_DIFF_CONTEXT: u32 = 3;
const MAX_DIFF_CONTEXT: u32 = 100;
/// Hard cap on directory-compare entries.
const DIR_ENTRY_CAP: usize = 50_000;
/// Recursion guard for the directory walk.
const MAX_DEPTH: usize = 128;
/// Verdict cache bound.
const CACHE_CAP: usize = 4096;
/// Progress events are throttled to roughly this interval.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

pub const PROGRESS_EVENT: &str = "compare://progress";

// ── Progress / cancel ────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProgressPhase {
    Scan,
    Content,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompareProgress {
    pub op_id: String,
    pub phase: ProgressPhase,
    pub done: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current: Option<String>,
}

pub type ProgressSink = Arc<dyn Fn(CompareProgress) + Send + Sync>;

fn registry() -> &'static Mutex<HashMap<String, Arc<AtomicBool>>> {
    static REG: OnceLock<Mutex<HashMap<String, Arc<AtomicBool>>>> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Request cooperative cancellation of the compare registered under `op_id`.
/// Unknown ids are a no-op (the compare may already have finished).
pub fn cancel(op_id: &str) -> bool {
    match registry().lock() {
        Ok(reg) => match reg.get(op_id) {
            Some(flag) => {
                flag.store(true, Ordering::SeqCst);
                true
            }
            None => false,
        },
        Err(_) => false,
    }
}

/// Number of in-flight registered operations (registry is cleaned on finish).
pub fn active_ops() -> usize {
    registry().lock().map(|r| r.len()).unwrap_or(0)
}

struct OpGuard {
    id: String,
    flag: Arc<AtomicBool>,
}

impl Drop for OpGuard {
    fn drop(&mut self) {
        if let Ok(mut reg) = registry().lock() {
            // Only remove our own registration (a newer op may reuse the id).
            if reg
                .get(&self.id)
                .is_some_and(|f| Arc::ptr_eq(f, &self.flag))
            {
                reg.remove(&self.id);
            }
        }
    }
}

/// Per-operation control: cancel flag + throttled progress emitter.
pub struct Ctl {
    op_id: String,
    cancel: Arc<AtomicBool>,
    sink: Option<ProgressSink>,
    last_emit: Mutex<Option<Instant>>,
    _guard: Option<OpGuard>,
    /// Bypass the verdict cache (sync must re-read bytes, never trust a cached `same`).
    pub no_cache: bool,
    /// FER-477b: skip the content read of file pairs that provably did not
    /// change since a baseline that verified them by content (journal change
    /// lists of the FerrumIndexer; see `sync_incremental`). `None` = read all.
    pub content_hint: Option<Arc<crate::sync_incremental::ContentHint>>,
    /// Test hook: trip the cancel flag after this many `cancelled()` polls.
    #[cfg(test)]
    trip_after: std::sync::atomic::AtomicUsize,
}

impl Ctl {
    /// No cancel registration, no progress.
    pub fn none() -> Self {
        Self::new(None, None)
    }

    pub fn new(op_id: Option<String>, sink: Option<ProgressSink>) -> Self {
        let flag = Arc::new(AtomicBool::new(false));
        let guard = op_id.as_ref().map(|id| {
            if let Ok(mut reg) = registry().lock() {
                reg.insert(id.clone(), flag.clone());
            }
            OpGuard {
                id: id.clone(),
                flag: flag.clone(),
            }
        });
        Self {
            op_id: op_id.unwrap_or_default(),
            cancel: flag,
            sink,
            no_cache: false,
            content_hint: None,
            last_emit: Mutex::new(None),
            _guard: guard,
            #[cfg(test)]
            trip_after: std::sync::atomic::AtomicUsize::new(usize::MAX),
        }
    }

    /// The raw cancel flag (for the helper launcher and the look-ahead workers,
    /// which must not go through the test trip hook of [`Ctl::cancelled`]).
    pub fn cancel_flag(&self) -> &AtomicBool {
        &self.cancel
    }

    pub fn cancelled(&self) -> bool {
        #[cfg(test)]
        {
            let left = self.trip_after.load(Ordering::SeqCst);
            if left != usize::MAX {
                if left == 0 {
                    self.cancel.store(true, Ordering::SeqCst);
                } else {
                    self.trip_after.store(left - 1, Ordering::SeqCst);
                }
            }
        }
        self.cancel.load(Ordering::SeqCst)
    }

    pub(crate) fn report(
        &self,
        phase: ProgressPhase,
        done: u64,
        total: Option<u64>,
        current: &str,
        force: bool,
    ) {
        let Some(sink) = &self.sink else { return };
        let Ok(mut last) = self.last_emit.lock() else {
            return;
        };
        let now = Instant::now();
        if !force && last.is_some_and(|t| now.duration_since(t) < PROGRESS_INTERVAL) {
            return;
        }
        *last = Some(now);
        drop(last);
        sink(CompareProgress {
            op_id: self.op_id.clone(),
            phase,
            done,
            total,
            current: Some(current.to_string()),
        });
    }
}

// ── Verdict cache ────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CacheKey {
    path_a: String,
    path_b: String,
    size_a: u64,
    size_b: u64,
    mtime_a: u128,
    mtime_b: u128,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Verdict {
    equal: bool,
    first_mismatch: Option<u64>,
}

/// Bounded FIFO-evicting verdict store. The key embeds size and mtime, so a
/// changed file simply misses (the stale entry ages out).
struct VerdictCache {
    cap: usize,
    map: HashMap<CacheKey, Verdict>,
    order: VecDeque<CacheKey>,
}

impl VerdictCache {
    fn new(cap: usize) -> Self {
        Self {
            cap,
            map: HashMap::new(),
            order: VecDeque::new(),
        }
    }
    fn get(&self, k: &CacheKey) -> Option<Verdict> {
        self.map.get(k).copied()
    }
    fn put(&mut self, k: CacheKey, v: Verdict) {
        if self.map.insert(k.clone(), v).is_none() {
            self.order.push_back(k);
            while self.map.len() > self.cap {
                match self.order.pop_front() {
                    Some(old) => {
                        self.map.remove(&old);
                    }
                    None => break,
                }
            }
        }
    }
    fn clear(&mut self) {
        self.map.clear();
        self.order.clear();
    }
}

fn cache() -> &'static Mutex<VerdictCache> {
    static C: OnceLock<Mutex<VerdictCache>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(VerdictCache::new(CACHE_CAP)))
}

/// Drop every cached content verdict.
pub fn cache_clear() {
    if let Ok(mut c) = cache().lock() {
        c.clear();
    }
}

fn mtime_ns(meta: &fs::Metadata) -> Option<u128> {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
}

// ── Compare by Content ───────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiffLineKind {
    Ctx,
    Del,
    Add,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextDiffLine {
    pub kind: DiffLineKind,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextDiffHunk {
    /// 1-based first line covered on each side.
    pub left_start: u64,
    pub right_start: u64,
    pub lines: Vec<TextDiffLine>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextDiff {
    pub hunks: Vec<TextDiffHunk>,
    pub truncated: bool,
    pub encoding: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompareResult {
    pub equal: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cancelled: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub from_cache: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_diff: Option<TextDiff>,
    /// Byte offset of the first difference (when unequal).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_mismatch: Option<u64>,
    pub left_size: u64,
    pub right_size: u64,
    /// 1-based differing line numbers when both files look like text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub left_line: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub right_line: Option<u64>,
    pub both_text: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Enc {
    Utf8,
    Utf16Le,
    Utf16Be,
    Latin1,
}

impl Enc {
    fn name(self) -> &'static str {
        match self {
            Enc::Utf8 => "utf-8",
            Enc::Utf16Le => "utf-16le",
            Enc::Utf16Be => "utf-16be",
            Enc::Latin1 => "latin1",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EncPref {
    Auto,
    Fixed(Enc),
}

fn parse_encoding(s: Option<&str>) -> Result<EncPref, FsError> {
    match s.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
        None | Some("") | Some("auto") => Ok(EncPref::Auto),
        Some("utf-8") | Some("utf8") => Ok(EncPref::Fixed(Enc::Utf8)),
        Some("utf-16le") | Some("utf16le") => Ok(EncPref::Fixed(Enc::Utf16Le)),
        Some("utf-16be") | Some("utf16be") => Ok(EncPref::Fixed(Enc::Utf16Be)),
        Some("latin1") | Some("latin-1") | Some("iso-8859-1") => Ok(EncPref::Fixed(Enc::Latin1)),
        Some(other) => Err(FsError::new(
            ErrorCode::Unsupported,
            format!(
                "Unsupported encoding '{other}' (use auto, utf-8, utf-16le, utf-16be or latin1)"
            ),
        )),
    }
}

fn io_err(e: std::io::Error, path: &str) -> FsError {
    crate::compare_archive::io_to_fs(e, path)
}

/// One side of a file compare: a disk file, or a member of an archive that is
/// read in place (FER-413, never extracted).
struct Side {
    path: String,
    size: u64,
    is_dir: bool,
    /// Disk mtime (ns); unused for members.
    mtime: Option<u128>,
    member: Option<crate::compare_archive::MemberRef>,
}

impl Side {
    fn disk(path: &str, meta: &fs::Metadata) -> Self {
        Self {
            path: path.to_string(),
            size: meta.len(),
            is_dir: meta.is_dir(),
            mtime: mtime_ns(meta),
            member: None,
        }
    }

    /// Plain files win; only a path that does not exist on disk is tried as
    /// an archive member address.
    fn resolve(path: &str) -> Result<Self, FsError> {
        match fs::metadata(path) {
            Ok(m) => Ok(Self::disk(path, &m)),
            Err(e) => match crate::compare_archive::resolve(path)? {
                Some(m) => Ok(Self {
                    path: path.to_string(),
                    size: m.size,
                    is_dir: m.is_dir,
                    mtime: None,
                    member: Some(m),
                }),
                None => Err(io_err(e, path)),
            },
        }
    }

    fn open(&self) -> Result<Box<dyn Read + Send>, FsError> {
        match &self.member {
            Some(m) => m.open(),
            None => fs::File::open(&self.path)
                .map(|f| Box::new(f) as Box<dyn Read + Send>)
                .map_err(|e| io_err(e, &self.path)),
        }
    }

    /// (identity string, mtime). `None` = not reliably identifiable, never cached.
    fn cache_ident(&self) -> Option<(String, u128)> {
        match &self.member {
            Some(m) => m.cache_ident(),
            None => self.mtime.map(|t| (self.path.clone(), t)),
        }
    }
}

/// Read up to the diff byte limit (+1 to learn whether the file is longer).
fn read_bounded(side: &Side) -> Result<(Vec<u8>, bool), FsError> {
    let path = side.path.as_str();
    let f = side.open()?;
    let mut buf = Vec::new();
    f.take(DIFF_MAX_BYTES as u64 + 1)
        .read_to_end(&mut buf)
        .map_err(|e| io_err(e, path))?;
    let clipped = buf.len() > DIFF_MAX_BYTES;
    buf.truncate(DIFF_MAX_BYTES);
    Ok((buf, clipped))
}

/// `None` = looks binary. `clipped` tolerates a UTF-8 sequence cut at the limit.
fn detect_encoding(bytes: &[u8], pref: EncPref, clipped: bool) -> Option<Enc> {
    match pref {
        EncPref::Fixed(Enc::Utf16Le) => Some(Enc::Utf16Le),
        EncPref::Fixed(Enc::Utf16Be) => Some(Enc::Utf16Be),
        EncPref::Fixed(e) => (!bytes.contains(&0)).then_some(e),
        EncPref::Auto => {
            if bytes.starts_with(&[0xFF, 0xFE]) {
                return Some(Enc::Utf16Le);
            }
            if bytes.starts_with(&[0xFE, 0xFF]) {
                return Some(Enc::Utf16Be);
            }
            if bytes.contains(&0) {
                return None;
            }
            let valid = match std::str::from_utf8(bytes) {
                Ok(_) => true,
                Err(e) => clipped && e.error_len().is_none(),
            };
            Some(if valid { Enc::Utf8 } else { Enc::Latin1 })
        }
    }
}

fn decode(bytes: &[u8], enc: Enc) -> String {
    match enc {
        Enc::Utf8 => {
            let b = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
            String::from_utf8_lossy(b).into_owned()
        }
        Enc::Latin1 => bytes.iter().map(|&b| b as char).collect(),
        Enc::Utf16Le | Enc::Utf16Be => {
            let bom: [u8; 2] = if enc == Enc::Utf16Le {
                [0xFF, 0xFE]
            } else {
                [0xFE, 0xFF]
            };
            let b = bytes.strip_prefix(&bom).unwrap_or(bytes);
            let (pairs, _) = b.as_chunks::<2>();
            let units: Vec<u16> = pairs
                .iter()
                .map(|c| {
                    if enc == Enc::Utf16Le {
                        u16::from_le_bytes([c[0], c[1]])
                    } else {
                        u16::from_be_bytes([c[0], c[1]])
                    }
                })
                .collect();
            String::from_utf16_lossy(&units)
        }
    }
}

/// Split into lines keeping a trailing '\r' (so CRLF-vs-LF shows as a
/// difference); an unterminated final line counts as a line.
fn split_lines(text: &str) -> Vec<&str> {
    let mut v: Vec<&str> = text.split('\n').collect();
    if v.last().is_some_and(|l| l.is_empty()) {
        v.pop();
    }
    v
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OpKind {
    Ctx,
    Del,
    Add,
}

/// Line diff: trim common prefix/suffix, LCS table on the (bounded) middle.
fn diff_ops(a: &[&str], b: &[&str]) -> Vec<(OpKind, usize)> {
    let mut pre = 0;
    while pre < a.len() && pre < b.len() && a[pre] == b[pre] {
        pre += 1;
    }
    let mut suf = 0;
    while suf < a.len() - pre && suf < b.len() - pre && a[a.len() - 1 - suf] == b[b.len() - 1 - suf]
    {
        suf += 1;
    }
    let ma = &a[pre..a.len() - suf];
    let mb = &b[pre..b.len() - suf];
    let (n, m) = (ma.len(), mb.len());
    let w = m + 1;
    // dp[i*w+j] = LCS length of ma[i..], mb[j..]. n,m <= DIFF_MAX_LINES.
    let mut dp = vec![0u16; (n + 1) * w];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i * w + j] = if ma[i] == mb[j] {
                dp[(i + 1) * w + j + 1] + 1
            } else {
                dp[(i + 1) * w + j].max(dp[i * w + j + 1])
            };
        }
    }
    let mut ops = Vec::with_capacity(pre + n + m + suf);
    for i in 0..pre {
        ops.push((OpKind::Ctx, i));
    }
    let (mut i, mut j) = (0, 0);
    while i < n || j < m {
        if i < n && j < m && ma[i] == mb[j] {
            ops.push((OpKind::Ctx, pre + i));
            i += 1;
            j += 1;
        } else if j < m && (i == n || dp[i * w + j + 1] >= dp[(i + 1) * w + j]) {
            ops.push((OpKind::Add, pre + j));
            j += 1;
        } else {
            ops.push((OpKind::Del, pre + i));
            i += 1;
        }
    }
    for k in 0..suf {
        ops.push((OpKind::Ctx, a.len() - suf + k));
    }
    ops
}

fn shown(line: &str) -> String {
    line.strip_suffix('\r').unwrap_or(line).to_string()
}

fn build_hunks(a: &[&str], b: &[&str], context: usize) -> Vec<TextDiffHunk> {
    let ops = diff_ops(a, b);
    // 1-based left/right line number "about to be consumed" before each op.
    let mut pos = Vec::with_capacity(ops.len());
    let (mut l, mut r) = (1u64, 1u64);
    for (k, _) in &ops {
        pos.push((l, r));
        match k {
            OpKind::Ctx => {
                l += 1;
                r += 1;
            }
            OpKind::Del => l += 1,
            OpKind::Add => r += 1,
        }
    }
    let changed: Vec<usize> = ops
        .iter()
        .enumerate()
        .filter(|(_, (k, _))| *k != OpKind::Ctx)
        .map(|(i, _)| i)
        .collect();
    let mut hunks = Vec::new();
    let mut idx = 0;
    while idx < changed.len() {
        let start = changed[idx].saturating_sub(context);
        let mut end = (changed[idx] + context).min(ops.len() - 1);
        idx += 1;
        while idx < changed.len() && changed[idx].saturating_sub(context) <= end + 1 {
            end = (changed[idx] + context).min(ops.len() - 1);
            idx += 1;
        }
        let lines = ops[start..=end]
            .iter()
            .map(|(k, i)| match k {
                OpKind::Ctx => TextDiffLine {
                    kind: DiffLineKind::Ctx,
                    text: shown(a[*i]),
                },
                OpKind::Del => TextDiffLine {
                    kind: DiffLineKind::Del,
                    text: shown(a[*i]),
                },
                OpKind::Add => TextDiffLine {
                    kind: DiffLineKind::Add,
                    text: shown(b[*i]),
                },
            })
            .collect();
        hunks.push(TextDiffHunk {
            left_start: pos[start].0,
            right_start: pos[start].1,
            lines,
        });
    }
    hunks
}

struct TextSide {
    enc: Enc,
    bytes: Vec<u8>,
    text: String,
    clipped: bool,
}

impl TextSide {
    fn redecode(&mut self, enc: Enc) {
        self.enc = enc;
        self.text = decode(&self.bytes, enc);
    }
}

fn load_text_side(side: &Side, pref: EncPref) -> Result<Option<TextSide>, FsError> {
    let (bytes, clipped) = read_bounded(side)?;
    Ok(detect_encoding(&bytes, pref, clipped).map(|enc| TextSide {
        enc,
        text: decode(&bytes, enc),
        bytes,
        clipped,
    }))
}

/// Cheap text sniff used when no diff is needed (equal files).
fn is_text_head(side: &Side, pref: EncPref) -> bool {
    let Ok(f) = side.open() else {
        return false;
    };
    let mut head = Vec::new();
    if f.take(8 * 1024).read_to_end(&mut head).is_err() {
        return false;
    }
    detect_encoding(&head, pref, true).is_some()
}

/// Clip a decoded side to the line limit; drops the (possibly partial) last
/// line when the byte limit cut the file. Returns (lines, clipped).
fn clip_lines(side: &TextSide) -> (Vec<&str>, bool) {
    let mut lines = split_lines(&side.text);
    let mut clipped = side.clipped;
    if clipped {
        lines.pop();
    }
    if lines.len() > DIFF_MAX_LINES {
        lines.truncate(DIFF_MAX_LINES);
        clipped = true;
    }
    (lines, clipped)
}

/// Count '\n' in the first `limit` bytes; used for 1-based line numbers.
fn count_newlines(side: &Side, limit: u64) -> Option<u64> {
    let mut f = side.open().ok()?;
    let mut remaining = limit;
    let mut nl = 0u64;
    let mut buf = [0u8; CHUNK];
    while remaining > 0 {
        let want = remaining.min(buf.len() as u64) as usize;
        let got = f.read(&mut buf[..want]).ok()?;
        if got == 0 {
            break;
        }
        nl += buf[..got].iter().filter(|&&b| b == b'\n').count() as u64;
        remaining -= got as u64;
    }
    Some(nl)
}

fn read_chunk<R: Read + ?Sized>(f: &mut R, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match f.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

/// Stream two files and report the first differing byte. `Ok(None)` when
/// cancelled. Equal prefix but different sizes is UNEQUAL.
fn stream_compare(a: &Side, b: &Side, ctl: &Ctl) -> Result<Option<Verdict>, FsError> {
    let (path_a, path_b) = (a.path.as_str(), b.path.as_str());
    let (size_a, size_b) = (a.size, b.size);
    let mut fa = a.open()?;
    let mut fb = b.open()?;
    let mut buf_a = vec![0u8; CHUNK];
    let mut buf_b = vec![0u8; CHUNK];
    let mut pos = 0u64;
    let mut first_mismatch: Option<u64> = None;
    let total = size_a.max(size_b);
    loop {
        if ctl.cancelled() {
            return Ok(None);
        }
        let na = read_chunk(&mut *fa, &mut buf_a).map_err(|e| io_err(e, path_a))?;
        let nb = read_chunk(&mut *fb, &mut buf_b).map_err(|e| io_err(e, path_b))?;
        let common = na.min(nb);
        if let Some(i) = (0..common).find(|&i| buf_a[i] != buf_b[i]) {
            first_mismatch = Some(pos + i as u64);
            break;
        }
        if na != nb || na == 0 {
            break;
        }
        pos += na as u64;
        ctl.report(ProgressPhase::Content, pos, Some(total), path_a, false);
    }
    let equal = first_mismatch.is_none() && size_a == size_b;
    let mismatch = first_mismatch.or(if equal {
        None
    } else {
        Some(size_a.min(size_b))
    });
    Ok(Some(Verdict {
        equal,
        first_mismatch: mismatch,
    }))
}

/// Cache-aware verdict for two disk files (directory compare/sync path).
fn content_verdict(
    path_a: &str,
    path_b: &str,
    meta_a: &fs::Metadata,
    meta_b: &fs::Metadata,
    ctl: &Ctl,
) -> Result<Option<(Verdict, bool)>, FsError> {
    content_verdict_sides(
        &Side::disk(path_a, meta_a),
        &Side::disk(path_b, meta_b),
        ctl,
    )
}

/// Cache-aware verdict. `Ok(None)` = cancelled. Second value: from cache.
/// A member's cache key carries the archive identity (path + size + mtime)
/// plus the member path; unidentifiable sides are never cached.
fn content_verdict_sides(
    a: &Side,
    b: &Side,
    ctl: &Ctl,
) -> Result<Option<(Verdict, bool)>, FsError> {
    let key = match (a.cache_ident(), b.cache_ident()) {
        (Some((pa, ma)), Some((pb, mb))) => Some(CacheKey {
            path_a: pa,
            path_b: pb,
            size_a: a.size,
            size_b: b.size,
            mtime_a: ma,
            mtime_b: mb,
        }),
        _ => None, // no reliable mtime -> never cache
    };
    if let Some(k) = key.as_ref().filter(|_| !ctl.no_cache)
        && let Some(v) = cache().lock().ok().and_then(|c| c.get(k))
    {
        return Ok(Some((v, true)));
    }
    let Some(v) = stream_compare(a, b, ctl)? else {
        return Ok(None);
    };
    if let (Some(k), Ok(mut c)) = (key.filter(|_| !ctl.no_cache), cache().lock()) {
        c.put(k, v);
    }
    Ok(Some((v, false)))
}

/// Options for [`compare_files_with`].
#[derive(Debug, Clone, Default)]
pub struct FileOpts {
    pub encoding: Option<String>,
    pub diff_context: Option<u32>,
}

/// Compare two files by content (binary-safe) and, for text, produce a
/// bounded line diff. Plain wrapper without cancel/progress/options.
pub fn compare_files(path_a: &str, path_b: &str) -> Result<CompareResult, FsError> {
    compare_files_with(path_a, path_b, &FileOpts::default(), &Ctl::none())
}

pub fn compare_files_with(
    path_a: &str,
    path_b: &str,
    opts: &FileOpts,
    ctl: &Ctl,
) -> Result<CompareResult, FsError> {
    validate_path(path_a)?;
    validate_path(path_b)?;
    let pref = parse_encoding(opts.encoding.as_deref())?;
    let context = opts
        .diff_context
        .unwrap_or(DEFAULT_DIFF_CONTEXT)
        .min(MAX_DIFF_CONTEXT) as usize;
    let meta_a = Side::resolve(path_a)?;
    let meta_b = Side::resolve(path_b)?;
    if meta_a.is_dir || meta_b.is_dir {
        return Err(FsError::new(
            ErrorCode::NotADirectory,
            "Content comparison needs two files, not folders",
        )
        .with_path(if meta_a.is_dir { path_a } else { path_b }));
    }
    let mut result = CompareResult {
        equal: false,
        cancelled: false,
        from_cache: false,
        text_diff: None,
        first_mismatch: None,
        left_size: meta_a.size,
        right_size: meta_b.size,
        left_line: None,
        right_line: None,
        both_text: false,
    };
    let Some((verdict, from_cache)) = content_verdict_sides(&meta_a, &meta_b, ctl)? else {
        result.cancelled = true;
        return Ok(result);
    };
    result.equal = verdict.equal;
    result.first_mismatch = verdict.first_mismatch;
    result.from_cache = from_cache;

    if verdict.equal {
        result.both_text = is_text_head(&meta_a, pref) && is_text_head(&meta_b, pref);
        return Ok(result);
    }
    if ctl.cancelled() {
        result.cancelled = true;
        return Ok(result);
    }

    let (side_a, side_b) = (
        load_text_side(&meta_a, pref)?,
        load_text_side(&meta_b, pref)?,
    );
    let (Some(mut sa), Some(mut sb)) = (side_a, side_b) else {
        return Ok(result); // binary: no text diff, no line numbers
    };
    // Auto-detect disagreement between the two single-byte families (one side
    // valid UTF-8, the other not): decode both as latin1 so one encoding applies.
    let single = |e: Enc| matches!(e, Enc::Utf8 | Enc::Latin1);
    if sa.enc != sb.enc && single(sa.enc) && single(sb.enc) {
        sa.redecode(Enc::Latin1);
        sb.redecode(Enc::Latin1);
    }
    result.both_text = true;

    let (lines_a, clip_a) = clip_lines(&sa);
    let (lines_b, clip_b) = clip_lines(&sb);
    let hunks = build_hunks(&lines_a, &lines_b, context);
    let encoding = if sa.enc == sb.enc {
        sa.enc.name().to_string()
    } else {
        format!("{}+{}", sa.enc.name(), sb.enc.name())
    };

    // Line numbers: byte-exact scan for single-byte-newline encodings, else
    // derived from the first changed line of the diff.
    let wide = |e: Enc| matches!(e, Enc::Utf16Le | Enc::Utf16Be);
    match verdict.first_mismatch {
        Some(off) if !wide(sa.enc) && !wide(sb.enc) && off < LINE_SCAN_LIMIT => {
            result.left_line = Some(count_newlines(&meta_a, off).unwrap_or(0) + 1);
            result.right_line = Some(count_newlines(&meta_b, off).unwrap_or(0) + 1);
        }
        _ => {
            if let Some(h) = hunks.first() {
                let lead = h
                    .lines
                    .iter()
                    .take_while(|l| l.kind == DiffLineKind::Ctx)
                    .count() as u64;
                result.left_line = Some(h.left_start + lead);
                result.right_line = Some(h.right_start + lead);
            }
        }
    }
    result.text_diff = Some(TextDiff {
        hunks,
        truncated: clip_a || clip_b,
        encoding,
    });
    Ok(result)
}

// ── Compare Directories ──────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DirDiffState {
    LeftOnly,
    RightOnly,
    Same,
    NewerLeft,
    NewerRight,
    SizeDifferent,
    ContentDifferent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirDiffEntry {
    /// Name, or '/'-separated path relative to both roots when recursive.
    pub name: String,
    /// True for a directory entry (a one-sided directory is ONE entry).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_dir: bool,
    pub state: DirDiffState,
    #[serde(skip_serializing_if = "is_zero_u64")]
    pub left_size: u64,
    #[serde(skip_serializing_if = "is_zero_i64")]
    pub left_modified_ms: i64,
    #[serde(skip_serializing_if = "is_zero_u64")]
    pub right_size: u64,
    #[serde(skip_serializing_if = "is_zero_i64")]
    pub right_modified_ms: i64,
}

fn is_zero_u64(v: &u64) -> bool {
    *v == 0
}

fn is_zero_i64(v: &i64) -> bool {
    *v == 0
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompareIssue {
    pub name: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompareDirsReport {
    pub left_root: String,
    pub right_root: String,
    pub entries: Vec<DirDiffEntry>,
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub recursive: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cancelled: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<CompareIssue>,
    /// FER-472: set (by the IPC layer) only when the fast scan was requested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fast_scan: Option<FastScanInfo>,
    /// FER-477b: journal marks of both roots taken BEFORE this compare
    /// walked (set by the IPC layer for an incremental compare). A baseline
    /// may keep them only if this compare proves the pair fully in sync.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub journal: Option<crate::sync_incremental::JournalMarksDto>,
    /// FER-477b: whether the change lists were used, and how many content
    /// reads they saved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incremental: Option<crate::sync_incremental::IncrementalInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SideInfo {
    pub is_dir: bool,
    /// Symlink/junction: an entry, never followed.
    #[serde(default)]
    pub is_symlink: bool,
    pub size: u64,
    pub modified_ms: i64,
}

pub(crate) fn side_info(path: &Path) -> std::io::Result<SideInfo> {
    // lstat semantics: the entry itself, never its target.
    let lm = fs::symlink_metadata(path)?;
    let is_dir = lm.is_dir();
    Ok(SideInfo {
        is_dir,
        is_symlink: lm.file_type().is_symlink(),
        size: if is_dir { 0 } else { lm.len() },
        modified_ms: crate::fs_service::modified_ms(&lm),
    })
}

/// Options for [`compare_dirs_with`].
#[derive(Debug, Clone, Copy, Default)]
pub struct DirOpts {
    pub by_content: bool,
    pub recursive: bool,
    /// 0 = default cap (50 000).
    pub max_entries: usize,
}

fn rel_name(rel: &str, name: &str) -> String {
    if rel.is_empty() {
        name.to_string()
    } else {
        format!("{rel}/{name}")
    }
}

struct Pending {
    idx: usize,
    left: PathBuf,
    right: PathBuf,
    meta_state: DirDiffState,
    name: String,
}

/// What a pair of listings says about one name.
struct Class {
    state: DirDiffState,
    is_dir: bool,
    descend: bool,
    needs_content: bool,
}

fn classify(l: Option<&SideInfo>, r: Option<&SideInfo>, opts: &DirOpts) -> Option<Class> {
    let mut is_dir = false;
    let mut descend = false;
    let mut needs_content = false;
    let state = match (l, r) {
        (Some(l), None) => {
            is_dir = l.is_dir;
            DirDiffState::LeftOnly
        }
        (None, Some(r)) => {
            is_dir = r.is_dir;
            DirDiffState::RightOnly
        }
        (Some(l), Some(r)) => {
            if l.is_dir != r.is_dir {
                // A folder and a file sharing a name can never "match";
                // flag isDir so sync treats it as a type mismatch (never overwrite).
                is_dir = true;
                DirDiffState::SizeDifferent
            } else if l.is_dir {
                is_dir = true;
                descend = opts.recursive;
                DirDiffState::Same
            } else if l.size != r.size {
                DirDiffState::SizeDifferent
            } else {
                needs_content = opts.by_content && !l.is_symlink && !r.is_symlink;
                if (l.modified_ms - r.modified_ms).abs() <= MTIME_TOLERANCE_MS {
                    DirDiffState::Same
                } else if l.modified_ms > r.modified_ms {
                    DirDiffState::NewerLeft
                } else {
                    DirDiffState::NewerRight
                }
            }
        }
        (None, None) => return None,
    };
    Some(Class {
        state,
        is_dir,
        descend,
        needs_content,
    })
}

struct Walker<'a> {
    left_root: &'a Path,
    right_root: &'a Path,
    opts: DirOpts,
    cap: usize,
    ctl: &'a Ctl,
    /// FER-472: listings come from here (look-ahead on worker threads).
    pf: &'a Prefetch<'a>,
    /// Ask the look-ahead for the folders the walk will enter next.
    prefetch: bool,
    /// A side is a point-in-time snapshot (MFT): entries about to be
    /// reported as different are re-read from the live filesystem first.
    verify: [bool; 2],
    entries: Vec<DirDiffEntry>,
    errors: Vec<CompareIssue>,
    pending: Vec<Pending>,
    truncated: bool,
    cancelled: bool,
}

impl Walker<'_> {
    fn push_item_errors(&mut self, rel: &str, side: &str, listing: &DirListing) {
        for (name, msg) in &listing.item_errors {
            self.errors.push(CompareIssue {
                name: rel_name(rel, name),
                message: format!("{side}: {msg}"),
            });
        }
    }

    /// Re-read one entry from disk on the snapshot sides. `Err(())` = an
    /// error was recorded and the entry's state is unknown.
    fn reverify(
        &mut self,
        full: &str,
        l: Option<SideInfo>,
        r: Option<SideInfo>,
    ) -> Result<(Option<SideInfo>, Option<SideInfo>), ()> {
        let mut out = [l, r];
        for (i, (root, label)) in [(self.left_root, "left"), (self.right_root, "right")]
            .into_iter()
            .enumerate()
        {
            // Only a side that LISTED the name is re-read. A side that did not
            // list it stays absent: on a case-insensitive volume an lstat of
            // `Report.TXT` would find `report.txt` and hide a case collision.
            if !self.verify[i] || out[i].is_none() {
                continue;
            }
            out[i] = match side_info(&root.join(full)) {
                Ok(info) => Some(info),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => {
                    self.errors.push(CompareIssue {
                        name: full.to_string(),
                        message: format!("{label}: {e}"),
                    });
                    return Err(());
                }
            };
        }
        Ok((out[0], out[1]))
    }

    fn walk(&mut self, rel: &str, depth: usize) -> std::io::Result<()> {
        let lres = self.pf.take(TreeSide::Left, rel);
        let rres = self.pf.take(TreeSide::Right, rel);
        let (left, right) = if depth == 0 {
            // Root failures are fatal for the whole compare.
            let left = lres?;
            let right = rres?;
            self.push_item_errors(rel, "left", &left);
            self.push_item_errors(rel, "right", &right);
            (left, right)
        } else {
            if let Ok(l) = &lres {
                self.push_item_errors(rel, "left", l);
            }
            if let Ok(r) = &rres {
                self.push_item_errors(rel, "right", r);
            }
            match (lres, rres) {
                (Ok(l), Ok(r)) => (l, r),
                (l, r) => {
                    for (side, res) in [("left", l.err()), ("right", r.err())] {
                        if let Some(e) = res {
                            self.errors.push(CompareIssue {
                                name: rel.to_string(),
                                message: format!("{side}: {e}"),
                            });
                        }
                    }
                    return Ok(());
                }
            }
        };

        let mut names: Vec<&String> = left
            .infos
            .keys()
            .chain(right.infos.keys())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        names.sort_by_key(|n| n.to_lowercase());

        if self.prefetch && self.opts.recursive && depth + 1 < MAX_DEPTH {
            for name in &names {
                let both_dirs = matches!(
                    (left.infos.get(*name), right.infos.get(*name)),
                    (Some(l), Some(r)) if l.is_dir && r.is_dir
                );
                if both_dirs && !left.broken.contains(*name) && !right.broken.contains(*name) {
                    self.pf.request(&rel_name(rel, name));
                }
            }
        }

        for name in names {
            if self.ctl.cancelled() {
                self.cancelled = true;
                return Ok(());
            }
            if self.entries.len() >= self.cap {
                self.truncated = true;
                return Ok(());
            }
            if left.broken.contains(name) || right.broken.contains(name) {
                continue; // already in errors; state unknown
            }
            let full = rel_name(rel, name);
            self.ctl.report(
                ProgressPhase::Scan,
                self.entries.len() as u64,
                None,
                &full,
                false,
            );
            let mut l = left.infos.get(name).copied();
            let mut r = right.infos.get(name).copied();
            let Some(mut class) = classify(l.as_ref(), r.as_ref(), &self.opts) else {
                continue;
            };
            if (self.verify[0] || self.verify[1]) && class.state != DirDiffState::Same {
                // The MFT is a snapshot: whatever is about to be shown as a
                // difference is read again from the live tree, so a stale
                // record can never put a wrong size/mtime in front of sync.
                match self.reverify(&full, l, r) {
                    Ok((nl, nr)) => {
                        l = nl;
                        r = nr;
                    }
                    Err(()) => continue,
                }
                match classify(l.as_ref(), r.as_ref(), &self.opts) {
                    Some(c) => class = c,
                    None => continue,
                }
            }
            let Class {
                state,
                is_dir,
                descend,
                needs_content,
            } = class;
            let idx = self.entries.len();
            self.entries.push(DirDiffEntry {
                name: full.clone(),
                is_dir,
                state,
                left_size: l.map(|i| i.size).unwrap_or(0),
                left_modified_ms: l.map(|i| i.modified_ms).unwrap_or(0),
                right_size: r.map(|i| i.size).unwrap_or(0),
                right_modified_ms: r.map(|i| i.modified_ms).unwrap_or(0),
            });
            // FER-477b: a pair that the journal proves unchanged since a
            // content-verified baseline keeps its metadata verdict, exactly
            // like a content read that found the bytes equal.
            let skip_content = needs_content
                && match (&self.ctl.content_hint, l.as_ref(), r.as_ref()) {
                    (Some(h), Some(li), Some(ri)) => h.may_skip(&full, li, ri),
                    _ => false,
                };
            if needs_content && !skip_content {
                self.pending.push(Pending {
                    idx,
                    left: self.left_root.join(&full),
                    right: self.right_root.join(&full),
                    meta_state: state,
                    name: full.clone(),
                });
            }
            if descend {
                if depth + 1 >= MAX_DEPTH {
                    self.errors.push(CompareIssue {
                        name: full,
                        message: "depth limit reached; subtree not compared".into(),
                    });
                } else {
                    self.walk(&full, depth + 1)?;
                    if self.cancelled || self.truncated {
                        return Ok(());
                    }
                }
            }
        }
        Ok(())
    }
}

/// Compare the DIRECT CONTENTS (one level) of two directories. Plain
/// wrapper kept for the original signature; `max_entries == 0` = default cap.
pub fn compare_dirs(
    left_root: &str,
    right_root: &str,
    by_content: bool,
    max_entries: usize,
) -> Result<CompareDirsReport, FsError> {
    compare_dirs_with(
        left_root,
        right_root,
        DirOpts {
            by_content,
            recursive: false,
            max_entries,
        },
        &Ctl::none(),
    )
}

fn check_root(root: &str) -> Result<(), FsError> {
    let meta = fs::metadata(root).map_err(|e| io_err(e, root))?;
    if !meta.is_dir() {
        return Err(FsError::new(
            ErrorCode::NotADirectory,
            "Compare root is a file, not a directory",
        )
        .with_path(root));
    }
    Ok(())
}

/// Directory compare with optional recursion, cancel and progress.
/// `by_content` additionally streams common same-size files (cache-aware) to
/// catch equal-size different-content cases. Read-only.
///
/// Both sides use the ordinary directory listing (FER-472 tier A).
pub fn compare_dirs_with(
    left_root: &str,
    right_root: &str,
    opts: DirOpts,
    ctl: &Ctl,
) -> Result<CompareDirsReport, FsError> {
    validate_dir_roots(left_root, right_root)?;
    let left = DiskSource::new(left_root);
    let right = DiskSource::new(right_root);
    compare_dirs_with_sources(left_root, right_root, opts, ctl, &left, &right)
}

/// Roots must be plain valid paths of existing folders. Cheap and
/// elevation-free: callers run it BEFORE asking for a fast scan.
pub fn validate_dir_roots(left_root: &str, right_root: &str) -> Result<(), FsError> {
    validate_path(left_root)?;
    validate_path(right_root)?;
    check_root(left_root)?;
    check_root(right_root)
}

/// [`compare_dirs_with`] with explicit listing sources (FER-472). The report
/// is identical whichever source serves a side; sources only differ in speed.
pub fn compare_dirs_with_sources(
    left_root: &str,
    right_root: &str,
    opts: DirOpts,
    ctl: &Ctl,
    left_src: &dyn TreeSource,
    right_src: &dyn TreeSource,
) -> Result<CompareDirsReport, FsError> {
    validate_dir_roots(left_root, right_root)?;
    let pf = Prefetch::new(left_src, right_src, ctl.cancel_flag());
    let workers = pf.wanted_workers();
    let mut w = Walker {
        left_root: Path::new(left_root),
        right_root: Path::new(right_root),
        opts,
        cap: if opts.max_entries == 0 {
            DIR_ENTRY_CAP
        } else {
            opts.max_entries
        },
        ctl,
        pf: &pf,
        prefetch: workers > 0,
        verify: [left_src.needs_verify(), right_src.needs_verify()],
        entries: Vec::new(),
        errors: Vec::new(),
        pending: Vec::new(),
        truncated: false,
        cancelled: false,
    };
    let walked = std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| pf.worker());
        }
        let _stop = pf.finish_on_drop();
        w.walk("", 0)
    });
    walked.map_err(|e| io_err(e, left_root))?;
    ctl.report(
        ProgressPhase::Scan,
        w.entries.len() as u64,
        Some(w.entries.len() as u64),
        "",
        true,
    );

    // Content phase: only files that already matched on size.
    let pending = std::mem::take(&mut w.pending);
    let total = pending.len() as u64;
    for (n, p) in pending.into_iter().enumerate() {
        if w.cancelled || ctl.cancelled() {
            w.cancelled = true;
            break;
        }
        ctl.report(
            ProgressPhase::Content,
            n as u64,
            Some(total),
            &p.name,
            false,
        );
        let (pa, pb) = (p.left.to_string_lossy(), p.right.to_string_lossy());
        let metas = fs::metadata(&p.left)
            .map_err(|e| io_err(e, &pa))
            .and_then(|a| {
                fs::metadata(&p.right)
                    .map_err(|e| io_err(e, &pb))
                    .map(|b| (a, b))
            });
        let outcome = metas.and_then(|(a, b)| content_verdict(&pa, &pb, &a, &b, ctl));
        match outcome {
            Ok(Some((v, _))) => {
                w.entries[p.idx].state = if v.equal {
                    p.meta_state
                } else {
                    DirDiffState::ContentDifferent
                };
            }
            Ok(None) => {
                w.cancelled = true;
                break;
            }
            Err(e) => {
                w.errors.push(CompareIssue {
                    name: p.name,
                    message: e.message,
                });
                w.entries[p.idx].state = DirDiffState::SizeDifferent; // placeholder, removed below
                w.entries[p.idx].name.clear();
            }
        }
    }
    // Entries whose content could not be read are unknown → listed in errors only.
    let entries: Vec<DirDiffEntry> = w
        .entries
        .into_iter()
        .filter(|e| !e.name.is_empty())
        .collect();
    ctl.report(ProgressPhase::Content, total, Some(total), "", true);
    Ok(CompareDirsReport {
        left_root: left_root.to_string(),
        right_root: right_root.to_string(),
        entries,
        truncated: w.truncated,
        recursive: opts.recursive,
        cancelled: w.cancelled,
        errors: w.errors,
        fast_scan: None,
        journal: None,
        incremental: None,
    })
}

// -- Sync root identity (AI-10 / FER-213) ---------------------------------

/// Gerbang sebelum sync dua arah menulis apa pun: kedua root harus berupa
/// folder yang BERBEDA, tidak bersarang, dan tidak menunjuk lokasi sama lewat
/// alias (symlink/junction, `.`/`..`, beda huruf besar-kecil pada volume
/// case-insensitive). Dibandingkan pada path kanonik. Read-only.
pub fn validate_sync_roots(left_root: &str, right_root: &str) -> Result<(), FsError> {
    validate_path(left_root)?;
    validate_path(right_root)?;
    let left = canonical_dir(left_root)?;
    let right = canonical_dir(right_root)?;
    if left == right {
        return Err(FsError::new(
            ErrorCode::InvalidPath,
            "Both panes point to the same folder; nothing to synchronize",
        )
        .with_path(left_root));
    }
    // Path::starts_with compares whole components, so `data` vs `data-backup`
    // is NOT nested.
    if left.starts_with(&right) || right.starts_with(&left) {
        return Err(FsError::new(
            ErrorCode::InvalidPath,
            "One folder is inside the other; synchronizing them would copy a folder into itself",
        )
        .with_path(left_root));
    }
    Ok(())
}

fn canonical_dir(root: &str) -> Result<std::path::PathBuf, FsError> {
    let meta = fs::metadata(root).map_err(|e| FsError::from(e).with_path(root))?;
    if !meta.is_dir() {
        return Err(FsError::new(
            ErrorCode::NotADirectory,
            "Sync root is a file, not a directory",
        )
        .with_path(root));
    }
    fs::canonicalize(root).map_err(|e| FsError::from(e).with_path(root))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQ: AtomicU64 = AtomicU64::new(0);

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ferrum-f218-{tag}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp root");
        dir
    }

    fn set_mtime(path: &Path, unix_secs: i64) {
        use std::fs::FileTimes;
        use std::time::{Duration, UNIX_EPOCH};
        let f = fs::File::options().write(true).open(path).unwrap();
        let t = UNIX_EPOCH + Duration::from_secs(unix_secs.max(0) as u64);
        f.set_times(FileTimes::new().set_modified(t)).unwrap();
    }

    #[test]
    fn identical_files_are_equal_with_no_mismatch() {
        let root = temp_root("same");
        fs::write(root.join("a.bin"), [1u8, 2, 3, 4, 5]).unwrap();
        fs::write(root.join("b.bin"), [1u8, 2, 3, 4, 5]).unwrap();
        let r = compare_files(
            root.join("a.bin").to_str().unwrap(),
            root.join("b.bin").to_str().unwrap(),
        )
        .unwrap();
        assert!(r.equal);
        assert_eq!(r.first_mismatch, None);
        assert_eq!(r.left_size, 5);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn same_size_different_content_is_detected_at_the_exact_byte() {
        let root = temp_root("flip");
        let mut a = vec![0u8; 200_000]; // larger than one 64 KiB chunk
        a[150_000] = 7;
        let mut b = a.clone();
        b[150_000] = 9;
        fs::write(root.join("a.bin"), &a).unwrap();
        fs::write(root.join("b.bin"), &b).unwrap();
        let r = compare_files(
            root.join("a.bin").to_str().unwrap(),
            root.join("b.bin").to_str().unwrap(),
        )
        .unwrap();
        assert!(!r.equal);
        assert_eq!(r.first_mismatch, Some(150_000));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn equal_prefix_but_different_sizes_is_unequal() {
        let root = temp_root("sizes");
        fs::write(root.join("short.txt"), b"hello").unwrap();
        fs::write(root.join("long.txt"), b"hello world").unwrap();
        let r = compare_files(
            root.join("short.txt").to_str().unwrap(),
            root.join("long.txt").to_str().unwrap(),
        )
        .unwrap();
        assert!(!r.equal);
        assert_eq!(r.first_mismatch, Some(5)); // first byte beyond the short file
        assert_eq!(r.left_size, 5);
        assert_eq!(r.right_size, 11);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn text_files_report_line_numbers_binary_do_not() {
        let root = temp_root("text");
        fs::write(root.join("a.txt"), "one\ntwo\nTHREE\nfour\n").unwrap();
        fs::write(root.join("b.txt"), "one\ntwo\nthree\nfour\n").unwrap();
        let r = compare_files(
            root.join("a.txt").to_str().unwrap(),
            root.join("b.txt").to_str().unwrap(),
        )
        .unwrap();
        assert!(!r.equal);
        assert!(r.both_text);
        assert_eq!(r.left_line, Some(3));
        assert_eq!(r.right_line, Some(3));

        fs::write(root.join("a.bin"), [1u8, 0, 3]).unwrap();
        fs::write(root.join("b.bin"), [1u8, 2, 3]).unwrap();
        let r = compare_files(
            root.join("a.bin").to_str().unwrap(),
            root.join("b.bin").to_str().unwrap(),
        )
        .unwrap();
        assert!(!r.equal);
        assert!(!r.both_text);
        assert_eq!(r.left_line, None);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn folders_are_rejected_for_content_compare() {
        let root = temp_root("folder");
        let err = compare_files(root.to_str().unwrap(), root.to_str().unwrap()).unwrap_err();
        assert_eq!(err.code, ErrorCode::NotADirectory);
        let _ = fs::remove_dir_all(&root);
    }

    fn write(root: &Path, name: &str, body: &[u8], mtime: Option<i64>) {
        let p = root.join(name);
        fs::write(&p, body).unwrap();
        if let Some(secs) = mtime {
            set_mtime(&p, secs);
        }
    }

    #[test]
    fn directory_compare_classifies_presence_size_and_time() {
        let left = temp_root("dleft");
        let right = temp_root("dright");
        // both same
        write(&left, "same.txt", b"data", None);
        write(&right, "same.txt", b"data", None);
        // size different
        write(&left, "size.txt", b"12345", None);
        write(&right, "size.txt", b"123456789", None);
        // left newer
        write(&left, "newer-left.txt", b"x", Some(1_800_000_000));
        write(&right, "newer-left.txt", b"x", Some(1_700_000_000));
        // right newer
        write(&left, "newer-right.txt", b"x", Some(1_700_000_000));
        write(&right, "newer-right.txt", b"x", Some(1_800_000_000));
        // only on one side
        write(&left, "left-only.txt", b"x", None);
        write(&right, "right-only.txt", b"x", None);
        // unicode name
        write(&left, "békér.txt", "ünïcode".as_bytes(), None);
        write(&right, "békér.txt", "ünïcode".as_bytes(), None);

        let report =
            compare_dirs(left.to_str().unwrap(), right.to_str().unwrap(), false, 0).unwrap();
        let state_of = |n: &str| {
            report
                .entries
                .iter()
                .find(|e| e.name == n)
                .map(|e| e.state)
                .unwrap()
        };
        assert_eq!(state_of("same.txt"), DirDiffState::Same);
        assert_eq!(state_of("size.txt"), DirDiffState::SizeDifferent);
        assert_eq!(state_of("newer-left.txt"), DirDiffState::NewerLeft);
        assert_eq!(state_of("newer-right.txt"), DirDiffState::NewerRight);
        assert_eq!(state_of("left-only.txt"), DirDiffState::LeftOnly);
        assert_eq!(state_of("right-only.txt"), DirDiffState::RightOnly);
        assert_eq!(state_of("békér.txt"), DirDiffState::Same);
        assert!(!report.truncated);
        let _ = fs::remove_dir_all(&left);
        let _ = fs::remove_dir_all(&right);
    }

    #[test]
    fn two_second_mtime_skew_is_tolerated_but_three_is_not() {
        let left = temp_root("skewl");
        let right = temp_root("skewr");
        write(&left, "fat.txt", b"x", Some(1_700_000_000));
        write(&right, "fat.txt", b"x", Some(1_700_000_002)); // +2s → Same
        write(&left, "fresh.txt", b"x", Some(1_700_000_000));
        write(&right, "fresh.txt", b"x", Some(1_700_000_003)); // +3s → NewerRight
        let report =
            compare_dirs(left.to_str().unwrap(), right.to_str().unwrap(), false, 0).unwrap();
        let state_of = |n: &str| {
            report
                .entries
                .iter()
                .find(|e| e.name == n)
                .map(|e| e.state)
                .unwrap()
        };
        assert_eq!(state_of("fat.txt"), DirDiffState::Same);
        assert_eq!(state_of("fresh.txt"), DirDiffState::NewerRight);
        let _ = fs::remove_dir_all(&left);
        let _ = fs::remove_dir_all(&right);
    }

    #[test]
    fn content_mode_catches_equal_size_different_bytes() {
        let left = temp_root("cleft");
        let right = temp_root("cright");
        write(&left, "twin.bin", &[1u8, 2, 3], Some(1_700_000_000));
        write(&right, "twin.bin", &[1u8, 2, 4], Some(1_700_000_000));
        // name-only mode sees Same (size + time within tolerance)…
        let report =
            compare_dirs(left.to_str().unwrap(), right.to_str().unwrap(), false, 0).unwrap();
        assert_eq!(report.entries[0].state, DirDiffState::Same);
        // …content mode catches it.
        let report =
            compare_dirs(left.to_str().unwrap(), right.to_str().unwrap(), true, 0).unwrap();
        assert_eq!(report.entries[0].state, DirDiffState::ContentDifferent);
        let _ = fs::remove_dir_all(&left);
        let _ = fs::remove_dir_all(&right);
    }

    #[test]
    fn folder_vs_file_same_name_counts_as_different() {
        let left = temp_root("kleft");
        let right = temp_root("kright");
        fs::create_dir(left.join("thing")).unwrap();
        write(&right, "thing", b"file", None);
        let report =
            compare_dirs(left.to_str().unwrap(), right.to_str().unwrap(), false, 0).unwrap();
        assert_eq!(report.entries[0].state, DirDiffState::SizeDifferent);
        // sync relies on isDir to classify this as a type mismatch (never overwrite)
        assert!(report.entries[0].is_dir);
        let _ = fs::remove_dir_all(&left);
        let _ = fs::remove_dir_all(&right);
    }

    #[test]
    fn entry_cap_sets_truncated_and_errors_are_structured() {
        let left = temp_root("capl");
        let right = temp_root("capr");
        for i in 0..10 {
            write(&left, &format!("f{i:02}.txt"), b"x", None);
            write(&right, &format!("f{i:02}.txt"), b"x", None);
        }
        let report =
            compare_dirs(left.to_str().unwrap(), right.to_str().unwrap(), false, 4).unwrap();
        assert_eq!(report.entries.len(), 4);
        assert!(report.truncated);
        let err = compare_dirs(r"Z:\nope", right.to_str().unwrap(), false, 0).unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);
        let _ = fs::remove_dir_all(&left);
        let _ = fs::remove_dir_all(&right);
    }

    // ── validate_sync_roots (FER-213) ──────────────────────────────────────

    fn root_err(l: &Path, r: &Path) -> Option<FsError> {
        validate_sync_roots(l.to_str().unwrap(), r.to_str().unwrap()).err()
    }

    #[test]
    fn sync_roots_must_differ() {
        let a = temp_root("sameroot");
        let err = root_err(&a, &a).expect("same root rejected");
        assert_eq!(err.code, ErrorCode::InvalidPath);
        // same folder spelled through a `.` component
        let dotted = a.join(".");
        assert!(root_err(&a, &dotted).is_some());
        let _ = fs::remove_dir_all(&a);
    }

    #[test]
    fn nested_sync_roots_are_rejected_in_both_orders() {
        let outer = temp_root("outer");
        let inner = outer.join("inner");
        fs::create_dir(&inner).unwrap();
        assert_eq!(
            root_err(&outer, &inner).map(|e| e.code),
            Some(ErrorCode::InvalidPath)
        );
        assert_eq!(
            root_err(&inner, &outer).map(|e| e.code),
            Some(ErrorCode::InvalidPath)
        );
        let _ = fs::remove_dir_all(&outer);
    }

    #[test]
    fn sibling_roots_with_a_shared_name_prefix_are_accepted() {
        let base = temp_root("prefix");
        let a = base.join("data");
        let b = base.join("data-backup");
        fs::create_dir(&a).unwrap();
        fs::create_dir(&b).unwrap();
        assert!(root_err(&a, &b).is_none());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn roots_must_exist_and_be_folders() {
        let base = temp_root("exist");
        let file = base.join("f.txt");
        fs::write(&file, b"x").unwrap();
        let dir = base.join("d");
        fs::create_dir(&dir).unwrap();
        assert_eq!(
            root_err(&base.join("missing"), &dir).map(|e| e.code),
            Some(ErrorCode::NotFound)
        );
        assert_eq!(
            root_err(&file, &dir).map(|e| e.code),
            Some(ErrorCode::NotADirectory)
        );
        assert_eq!(
            validate_sync_roots("", dir.to_str().unwrap()).map_err(|e| e.code),
            Err(ErrorCode::InvalidPath)
        );
        let _ = fs::remove_dir_all(&base);
    }

    #[cfg(windows)]
    #[test]
    fn case_variant_of_the_same_root_is_rejected_on_windows() {
        let a = temp_root("casealias");
        let upper = a.to_string_lossy().to_uppercase();
        assert!(validate_sync_roots(a.to_str().unwrap(), &upper).is_err());
        let _ = fs::remove_dir_all(&a);
    }

    /// Symlink/junction alias to the same folder. Symlink creation needs a
    /// privilege on Windows; when unavailable the test says so on stderr and
    /// returns instead of pretending to have verified it.
    #[test]
    fn symlink_alias_of_a_root_is_rejected() {
        let base = temp_root("alias");
        let real = base.join("real");
        let alias = base.join("alias");
        fs::create_dir(&real).unwrap();
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(&real, &alias).is_ok();
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_dir(&real, &alias).is_ok()
            // a junction needs no privilege and is an alias just the same
            || std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(&alias)
                .arg(&real)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
        if !made {
            eprintln!("SKIPPED symlink_alias_of_a_root_is_rejected: cannot create symlink here");
            let _ = fs::remove_dir_all(&base);
            return;
        }
        assert_eq!(
            root_err(&real, &alias).map(|e| e.code),
            Some(ErrorCode::InvalidPath)
        );
        let _ = fs::remove_dir_all(&base);
    }

    // ── dry-run is read-only; case and metadata edge cases (FER-213/214) ──

    fn snapshot(root: &Path) -> Vec<(String, Vec<u8>, i64)> {
        let mut out: Vec<(String, Vec<u8>, i64)> = fs::read_dir(root)
            .unwrap()
            .flatten()
            .map(|e| {
                let p = e.path();
                let lm = fs::symlink_metadata(&p).unwrap();
                let body = if lm.is_file() {
                    fs::read(&p).unwrap()
                } else {
                    Vec::new()
                };
                (
                    e.file_name().to_string_lossy().into_owned(),
                    body,
                    crate::fs_service::modified_ms(&lm),
                )
            })
            .collect();
        out.sort();
        out
    }

    #[test]
    fn compare_dirs_never_mutates_either_tree() {
        let left = temp_root("ro-l");
        let right = temp_root("ro-r");
        write(&left, "a.txt", b"left", Some(1_700_000_000));
        write(&right, "a.txt", b"rite", Some(1_700_000_000));
        write(&left, "only-l.txt", b"1", None);
        write(&right, "only-r.txt", b"2", None);
        let before = (snapshot(&left), snapshot(&right));
        for by_content in [false, true] {
            compare_dirs(
                left.to_str().unwrap(),
                right.to_str().unwrap(),
                by_content,
                0,
            )
            .unwrap();
        }
        assert_eq!((snapshot(&left), snapshot(&right)), before);
        let _ = fs::remove_dir_all(&left);
        let _ = fs::remove_dir_all(&right);
    }

    #[test]
    fn same_size_and_mtime_but_different_bytes_needs_content_mode() {
        let left = temp_root("meta-l");
        let right = temp_root("meta-r");
        write(&left, "doc.bin", b"AAAA", Some(1_700_000_000));
        write(&right, "doc.bin", b"AAAB", Some(1_700_000_000));
        let by_meta =
            compare_dirs(left.to_str().unwrap(), right.to_str().unwrap(), false, 0).unwrap();
        // Honest limitation: metadata alone says Same; callers that must
        // decide on content (sync) have to request content mode.
        assert_eq!(by_meta.entries[0].state, DirDiffState::Same);
        let by_bytes =
            compare_dirs(left.to_str().unwrap(), right.to_str().unwrap(), true, 0).unwrap();
        assert_eq!(by_bytes.entries[0].state, DirDiffState::ContentDifferent);
        let _ = fs::remove_dir_all(&left);
        let _ = fs::remove_dir_all(&right);
    }

    #[test]
    fn names_differing_only_by_case_surface_as_two_one_sided_entries() {
        // Compare keys are exact-case: the SYNC PLANNER (syncPlan.ts) turns
        // this pair into a caseCollision conflict. Pin the report shape it
        // relies on.
        let left = temp_root("case-l");
        let right = temp_root("case-r");
        write(&left, "Report.TXT", b"1", None);
        write(&right, "report.txt", b"2", None);
        let report =
            compare_dirs(left.to_str().unwrap(), right.to_str().unwrap(), false, 0).unwrap();
        let mut states: Vec<(String, DirDiffState)> = report
            .entries
            .iter()
            .map(|e| (e.name.clone(), e.state))
            .collect();
        states.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            states,
            vec![
                ("Report.TXT".to_string(), DirDiffState::LeftOnly),
                ("report.txt".to_string(), DirDiffState::RightOnly),
            ]
        );
        let _ = fs::remove_dir_all(&left);
        let _ = fs::remove_dir_all(&right);
    }

    #[test]
    fn symlink_entries_are_compared_as_entries_never_followed() {
        let left = temp_root("sym-l");
        let right = temp_root("sym-r");
        let target = temp_root("sym-target");
        fs::write(target.join("big.txt"), vec![b'x'; 4096]).unwrap();
        let link = left.join("link");
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(target.join("big.txt"), &link).is_ok();
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_file(target.join("big.txt"), &link).is_ok();
        if !made {
            eprintln!("SKIPPED symlink_entries_are_compared_as_entries_never_followed");
        } else {
            let report =
                compare_dirs(left.to_str().unwrap(), right.to_str().unwrap(), false, 0).unwrap();
            let entry = report.entries.iter().find(|e| e.name == "link").unwrap();
            assert_eq!(entry.state, DirDiffState::LeftOnly);
            // lstat size = the link itself, not the 4096-byte target
            assert_ne!(entry.left_size, 4096);
        }
        let _ = fs::remove_dir_all(&left);
        let _ = fs::remove_dir_all(&right);
        let _ = fs::remove_dir_all(&target);
    }

    // ── FER-389: recursive, errors, cancel, cache, text diff ───────────────

    fn s(p: &Path) -> &str {
        p.to_str().unwrap()
    }

    fn rec(by_content: bool) -> DirOpts {
        DirOpts {
            by_content,
            recursive: true,
            max_entries: 0,
        }
    }

    fn state_of(r: &CompareDirsReport, n: &str) -> DirDiffState {
        r.entries
            .iter()
            .find(|e| e.name == n)
            .unwrap_or_else(|| panic!("no entry {n}: {:?}", r.entries))
            .state
    }

    #[test]
    fn recursive_descends_only_into_dirs_present_on_both_sides() {
        let l = temp_root("rec-l");
        let r = temp_root("rec-r");
        fs::create_dir_all(l.join("both/deep")).unwrap();
        fs::create_dir_all(r.join("both/deep")).unwrap();
        write(&l.join("both/deep"), "x.txt", b"1", None);
        write(&r.join("both/deep"), "x.txt", b"22", None);
        write(&l.join("both"), "only-l.txt", b"1", None);
        // one-sided directory with content: must stay ONE entry
        fs::create_dir_all(l.join("lonely/sub")).unwrap();
        write(&l.join("lonely/sub"), "f.txt", b"1", None);

        let rep = compare_dirs_with(s(&l), s(&r), rec(false), &Ctl::none()).unwrap();
        assert!(rep.recursive && !rep.cancelled && rep.errors.is_empty());
        assert_eq!(
            state_of(&rep, "both/deep/x.txt"),
            DirDiffState::SizeDifferent
        );
        assert_eq!(state_of(&rep, "both/only-l.txt"), DirDiffState::LeftOnly);
        let both = rep.entries.iter().find(|e| e.name == "both").unwrap();
        assert!(both.is_dir && both.state == DirDiffState::Same);
        let lonely = rep.entries.iter().find(|e| e.name == "lonely").unwrap();
        assert!(lonely.is_dir && lonely.state == DirDiffState::LeftOnly);
        assert!(
            !rep.entries.iter().any(|e| e.name.starts_with("lonely/")),
            "one-sided dir must not be expanded"
        );
        // non-recursive stays one level
        let flat = compare_dirs(s(&l), s(&r), false, 0).unwrap();
        assert!(!flat.recursive && !flat.entries.iter().any(|e| e.name.contains('/')));
        let _ = fs::remove_dir_all(&l);
        let _ = fs::remove_dir_all(&r);
    }

    #[test]
    fn recursive_content_mode_finds_nested_equal_size_difference() {
        let l = temp_root("recc-l");
        let r = temp_root("recc-r");
        fs::create_dir_all(l.join("a")).unwrap();
        fs::create_dir_all(r.join("a")).unwrap();
        write(&l.join("a"), "t.bin", b"AAAA", Some(1_700_000_000));
        write(&r.join("a"), "t.bin", b"AAAB", Some(1_700_000_000));
        let rep = compare_dirs_with(s(&l), s(&r), rec(true), &Ctl::none()).unwrap();
        assert_eq!(state_of(&rep, "a/t.bin"), DirDiffState::ContentDifferent);
        let _ = fs::remove_dir_all(&l);
        let _ = fs::remove_dir_all(&r);
    }

    #[cfg(windows)]
    #[test]
    fn unreadable_file_lands_in_errors_not_skipped_windows() {
        use std::os::windows::fs::OpenOptionsExt;
        let l = temp_root("err-l");
        let r = temp_root("err-r");
        write(&l, "locked.txt", b"same", Some(1_700_000_000));
        write(&r, "locked.txt", b"same", Some(1_700_000_000));
        write(&l, "ok.txt", b"same", Some(1_700_000_000));
        write(&r, "ok.txt", b"same", Some(1_700_000_000));
        // exclusive handle (share_mode 0) → opening for read fails
        let _hold = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(l.join("locked.txt"))
            .unwrap();
        let rep = compare_dirs_with(s(&l), s(&r), rec(true), &Ctl::none()).unwrap();
        assert_eq!(rep.errors.len(), 1, "{:?}", rep.errors);
        assert_eq!(rep.errors[0].name, "locked.txt");
        assert!(!rep.entries.iter().any(|e| e.name == "locked.txt"));
        assert_eq!(state_of(&rep, "ok.txt"), DirDiffState::Same);
        drop(_hold);
        let _ = fs::remove_dir_all(&l);
        let _ = fs::remove_dir_all(&r);
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_subdir_lands_in_errors_not_skipped_unix() {
        use std::os::unix::fs::PermissionsExt;
        let l = temp_root("uerr-l");
        let r = temp_root("uerr-r");
        fs::create_dir_all(l.join("locked")).unwrap();
        fs::create_dir_all(r.join("locked")).unwrap();
        fs::set_permissions(l.join("locked"), fs::Permissions::from_mode(0o000)).unwrap();
        let readable = fs::read_dir(l.join("locked")).is_ok(); // running as root?
        let rep = compare_dirs_with(s(&l), s(&r), rec(false), &Ctl::none()).unwrap();
        fs::set_permissions(l.join("locked"), fs::Permissions::from_mode(0o755)).unwrap();
        if readable {
            eprintln!("SKIPPED unix permission test: running as root");
        } else {
            assert!(
                rep.errors.iter().any(|e| e.name == "locked"),
                "{:?}",
                rep.errors
            );
        }
        let _ = fs::remove_dir_all(&l);
        let _ = fs::remove_dir_all(&r);
    }

    #[test]
    fn vanished_or_missing_root_is_an_error_not_a_panic() {
        let r = temp_root("miss-r");
        let e = compare_dirs_with(r"Z:\definitely\missing", s(&r), rec(false), &Ctl::none())
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::NotFound);
        let _ = fs::remove_dir_all(&r);
    }

    #[test]
    fn symlinks_are_not_followed_in_recursive_walk() {
        let l = temp_root("rs-l");
        let r = temp_root("rs-r");
        let target = temp_root("rs-target");
        write(&target, "secret.txt", b"should not be listed", None);
        let (ll, rl) = (l.join("lnk"), r.join("lnk"));
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(&target, &ll).is_ok()
            && std::os::unix::fs::symlink(&target, &rl).is_ok();
        #[cfg(windows)]
        let made = {
            // symlink_dir needs a privilege; a junction needs none and is
            // likewise a reparse point that must not be followed.
            let mk = |link: &Path| {
                std::os::windows::fs::symlink_dir(&target, link).is_ok()
                    || std::process::Command::new("cmd")
                        .args(["/C", "mklink", "/J"])
                        .arg(link)
                        .arg(&target)
                        .output()
                        .map(|o| o.status.success())
                        .unwrap_or(false)
            };
            mk(&ll) && mk(&rl)
        };
        if !made {
            eprintln!("SKIPPED symlinks_are_not_followed_in_recursive_walk: cannot create symlink");
        } else {
            // also a cycle: link pointing back at the root
            let rep = compare_dirs_with(s(&l), s(&r), rec(true), &Ctl::none()).unwrap();
            assert!(
                !rep.entries.iter().any(|e| e.name.starts_with("lnk/")),
                "followed a symlink: {:?}",
                rep.entries
            );
            let e = rep.entries.iter().find(|e| e.name == "lnk").unwrap();
            assert!(!e.is_dir, "a link is an entry, not a directory");
        }
        let _ = fs::remove_dir_all(&l);
        let _ = fs::remove_dir_all(&r);
        let _ = fs::remove_dir_all(&target);
    }

    #[test]
    fn cancel_midway_returns_partial_cancelled_dirs_report() {
        let l = temp_root("can-l");
        let r = temp_root("can-r");
        for i in 0..30 {
            write(&l, &format!("f{i:02}.txt"), b"x", None);
            write(&r, &format!("f{i:02}.txt"), b"x", None);
        }
        let ctl = Ctl::none();
        ctl.trip_after.store(10, Ordering::SeqCst);
        let rep = compare_dirs_with(s(&l), s(&r), rec(false), &ctl).unwrap();
        assert!(rep.cancelled);
        assert!(
            !rep.entries.is_empty() && rep.entries.len() < 30,
            "{}",
            rep.entries.len()
        );
        let _ = fs::remove_dir_all(&l);
        let _ = fs::remove_dir_all(&r);
    }

    #[test]
    fn cancel_midway_returns_partial_cancelled_file_result() {
        let root = temp_root("canf");
        let big = vec![0u8; CHUNK * 20];
        fs::write(root.join("a.bin"), &big).unwrap();
        fs::write(root.join("b.bin"), &big).unwrap();
        let ctl = Ctl::none();
        ctl.trip_after.store(3, Ordering::SeqCst);
        let res = compare_files_with(
            s(&root.join("a.bin")),
            s(&root.join("b.bin")),
            &FileOpts::default(),
            &ctl,
        )
        .unwrap();
        assert!(res.cancelled && !res.equal && res.text_diff.is_none());
        assert_eq!(res.left_size, big.len() as u64);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn cancel_registry_flags_and_cleans_up() {
        let id = format!("op-{}", std::process::id());
        {
            let ctl = Ctl::new(Some(id.clone()), None);
            assert!(!ctl.cancelled());
            assert!(cancel(&id), "registered while running");
            assert!(ctl.cancelled());
        }
        // (global count is shared with parallel tests; probe our own id)
        assert!(!cancel(&id), "registry must be cleaned on finish");
        assert!(!cancel(&id), "unknown op id is a no-op");
    }

    #[test]
    fn progress_is_throttled() {
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let c2 = count.clone();
        let sink: ProgressSink = Arc::new(move |p: CompareProgress| {
            assert_eq!(p.op_id, "t");
            c2.fetch_add(1, Ordering::SeqCst);
        });
        let ctl = Ctl::new(Some("t".into()), Some(sink));
        for i in 0..10_000 {
            ctl.report(ProgressPhase::Scan, i, None, "x", false);
        }
        let n = count.load(Ordering::SeqCst);
        assert!((1..=50).contains(&n), "emitted {n} of 10000 calls");
        ctl.report(ProgressPhase::Scan, 1, None, "x", true);
        assert_eq!(count.load(Ordering::SeqCst), n + 1);
    }

    #[test]
    fn dir_progress_reports_scan_and_content_phases() {
        let l = temp_root("pg-l");
        let r = temp_root("pg-r");
        write(&l, "a.txt", b"same", Some(1_700_000_000));
        write(&r, "a.txt", b"same", Some(1_700_000_000));
        let seen = Arc::new(Mutex::new(Vec::<ProgressPhase>::new()));
        let s2 = seen.clone();
        let sink: ProgressSink = Arc::new(move |p| s2.lock().unwrap().push(p.phase));
        let ctl = Ctl::new(None, Some(sink));
        compare_dirs_with(s(&l), s(&r), rec(true), &ctl).unwrap();
        let seen = seen.lock().unwrap();
        assert!(seen.contains(&ProgressPhase::Scan) && seen.contains(&ProgressPhase::Content));
        let _ = fs::remove_dir_all(&l);
        let _ = fs::remove_dir_all(&r);
    }

    #[test]
    fn verdict_cache_hits_and_invalidates_on_mtime_and_size_change() {
        let root = temp_root("cache");
        let (a, b) = (root.join("a.bin"), root.join("b.bin"));
        write(&root, "a.bin", b"AAAA", Some(1_700_000_000));
        write(&root, "b.bin", b"AAAB", Some(1_700_000_000));
        let run = || compare_files(s(&a), s(&b)).unwrap();
        let first = run();
        assert!(!first.equal && !first.from_cache);
        let second = run();
        assert!(second.from_cache && !second.equal);
        assert_eq!(second.first_mismatch, first.first_mismatch);
        // mtime changes (same bytes) → miss
        set_mtime(&b, 1_700_000_100);
        assert!(!run().from_cache);
        assert!(run().from_cache);
        // content + size change → miss and the new verdict is correct
        fs::write(&b, b"AAAAA").unwrap();
        set_mtime(&b, 1_700_000_100);
        let third = run();
        assert!(!third.from_cache && !third.equal && third.right_size == 5);
        // dir-compare shares the cache (content mode hit after file compare)
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn verdict_cache_is_bounded_and_clearable() {
        let mut c = VerdictCache::new(3);
        let key = |i: u64| CacheKey {
            path_a: "a".into(),
            path_b: "b".into(),
            size_a: i,
            size_b: i,
            mtime_a: 0,
            mtime_b: 0,
        };
        let v = Verdict {
            equal: true,
            first_mismatch: None,
        };
        for i in 0..10 {
            c.put(key(i), v);
        }
        assert_eq!(c.map.len(), 3);
        assert!(c.get(&key(0)).is_none() && c.get(&key(9)).is_some());
        c.clear();
        assert!(c.map.is_empty() && c.order.is_empty());
        // FER-459: do not call the global `cache_clear()` here. The process-wide cache is shared
        // with every other test, so clearing it made `verdict_cache_hits_and_invalidates_on_*`
        // see a miss when the two ran in parallel. The IPC wrapper is a one-line call to it.
    }

    fn lines(n: usize) -> String {
        (1..=n).map(|i| format!("line {i}\n")).collect()
    }

    #[test]
    fn text_diff_produces_hunks_with_context_and_line_starts() {
        let root = temp_root("td");
        let mut a = lines(30);
        a = a.replace("line 5\n", "line FIVE\n");
        a = a.replace("line 25\n", "");
        fs::write(root.join("a.txt"), &a).unwrap();
        fs::write(root.join("b.txt"), lines(30)).unwrap();
        let r = compare_files(s(&root.join("a.txt")), s(&root.join("b.txt"))).unwrap();
        assert!(!r.equal && r.both_text);
        let td = r.text_diff.expect("text diff");
        assert_eq!(td.encoding, "utf-8");
        assert!(!td.truncated);
        assert_eq!(td.hunks.len(), 2, "{td:?}");
        let h0 = &td.hunks[0];
        assert_eq!((h0.left_start, h0.right_start), (2, 2)); // 3 ctx before line 5
        assert!(
            h0.lines
                .iter()
                .any(|l| l.kind == DiffLineKind::Del && l.text == "line FIVE")
        );
        assert!(
            h0.lines
                .iter()
                .any(|l| l.kind == DiffLineKind::Add && l.text == "line 5")
        );
        let h1 = &td.hunks[1];
        assert!(
            h1.lines
                .iter()
                .any(|l| l.kind == DiffLineKind::Add && l.text == "line 25")
        );
        assert_eq!(r.left_line, Some(5));
        // diff_context = 0 → only the changed lines
        let r0 = compare_files_with(
            s(&root.join("a.txt")),
            s(&root.join("b.txt")),
            &FileOpts {
                encoding: None,
                diff_context: Some(0),
            },
            &Ctl::none(),
        )
        .unwrap();
        let t0 = r0.text_diff.unwrap();
        assert!(
            t0.hunks
                .iter()
                .all(|h| h.lines.iter().all(|l| l.kind != DiffLineKind::Ctx))
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn text_diff_is_truncated_at_line_limit_and_absent_for_binary() {
        let root = temp_root("tdt");
        let mut big_a = lines(DIFF_MAX_LINES + 500);
        let big_b = big_a.clone();
        big_a.push_str("tail only in a\n");
        fs::write(root.join("a.txt"), &big_a).unwrap();
        fs::write(root.join("b.txt"), format!("changed\n{big_b}")).unwrap();
        let r = compare_files(s(&root.join("a.txt")), s(&root.join("b.txt"))).unwrap();
        let td = r.text_diff.unwrap();
        assert!(td.truncated);
        let total: usize = td.hunks.iter().map(|h| h.lines.len()).sum();
        assert!(total <= DIFF_MAX_LINES * 2 + 10);

        // byte limit
        let wide_line = "x".repeat(1000) + "\n";
        fs::write(root.join("c.txt"), wide_line.repeat(400)).unwrap(); // ~400 KB
        fs::write(root.join("d.txt"), "different\n").unwrap();
        let r = compare_files(s(&root.join("c.txt")), s(&root.join("d.txt"))).unwrap();
        assert!(r.text_diff.unwrap().truncated);

        // binary (NUL, no BOM) → no text diff, both_text false
        fs::write(root.join("x.bin"), [1u8, 0, 3]).unwrap();
        fs::write(root.join("y.bin"), [1u8, 2, 3]).unwrap();
        let r = compare_files(s(&root.join("x.bin")), s(&root.join("y.bin"))).unwrap();
        assert!(r.text_diff.is_none() && !r.both_text);
        let _ = fs::remove_dir_all(&root);
    }

    fn utf16(text: &str, le: bool, bom: bool) -> Vec<u8> {
        let mut out = Vec::new();
        if bom {
            out.extend(if le { [0xFF, 0xFE] } else { [0xFE, 0xFF] });
        }
        for u in text.encode_utf16() {
            out.extend(if le { u.to_le_bytes() } else { u.to_be_bytes() });
        }
        out
    }

    fn diff_with(root: &Path, enc: Option<&str>) -> CompareResult {
        compare_files_with(
            s(&root.join("a.txt")),
            s(&root.join("b.txt")),
            &FileOpts {
                encoding: enc.map(String::from),
                diff_context: None,
            },
            &Ctl::none(),
        )
        .unwrap()
    }

    #[test]
    fn utf16_with_bom_is_auto_detected_and_decoded() {
        let root = temp_root("u16");
        fs::write(root.join("a.txt"), utf16("alpha\nbéta\n", true, true)).unwrap();
        fs::write(root.join("b.txt"), utf16("alpha\ngamma\n", true, true)).unwrap();
        let r = diff_with(&root, None);
        assert!(r.both_text);
        let td = r.text_diff.unwrap();
        assert_eq!(td.encoding, "utf-16le");
        let all: Vec<_> = td.hunks.iter().flat_map(|h| h.lines.iter()).collect();
        assert!(
            all.iter()
                .any(|l| l.kind == DiffLineKind::Del && l.text == "béta")
        );
        assert!(
            all.iter()
                .any(|l| l.kind == DiffLineKind::Add && l.text == "gamma")
        );
        assert_eq!(r.left_line, Some(2));

        // big-endian BOM
        fs::write(root.join("a.txt"), utf16("one\ntwo\n", false, true)).unwrap();
        fs::write(root.join("b.txt"), utf16("one\nTWO\n", false, true)).unwrap();
        assert_eq!(
            diff_with(&root, None).text_diff.unwrap().encoding,
            "utf-16be"
        );

        // explicit utf-16le without BOM
        fs::write(root.join("a.txt"), utf16("one\ntwo\n", true, false)).unwrap();
        fs::write(root.join("b.txt"), utf16("one\nTWO\n", true, false)).unwrap();
        assert!(
            diff_with(&root, None).text_diff.is_none(),
            "no BOM + NULs = binary under auto"
        );
        let td = diff_with(&root, Some("utf-16le")).text_diff.unwrap();
        assert_eq!(td.encoding, "utf-16le");
        assert!(td.hunks[0].lines.iter().any(|l| l.text == "TWO"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn latin1_fallback_and_explicit_choice_decode_bytes() {
        let root = temp_root("l1");
        // 0xE9 = é in latin1, invalid as UTF-8
        fs::write(root.join("a.txt"), b"caf\xE9\nsame\n").unwrap();
        fs::write(root.join("b.txt"), b"cafe\nsame\n").unwrap();
        let td = diff_with(&root, None).text_diff.unwrap();
        assert_eq!(td.encoding, "latin1");
        assert!(
            td.hunks[0]
                .lines
                .iter()
                .any(|l| l.kind == DiffLineKind::Del && l.text == "café")
        );
        let td = diff_with(&root, Some("latin1")).text_diff.unwrap();
        assert_eq!(td.encoding, "latin1");
        // valid UTF-8 stays utf-8 under auto
        fs::write(root.join("a.txt"), "café\n").unwrap();
        fs::write(root.join("b.txt"), "cafe\n").unwrap();
        assert_eq!(diff_with(&root, None).text_diff.unwrap().encoding, "utf-8");
        // unknown encoding is a clear error
        let e = compare_files_with(
            s(&root.join("a.txt")),
            s(&root.join("b.txt")),
            &FileOpts {
                encoding: Some("ebcdic".into()),
                diff_context: None,
            },
            &Ctl::none(),
        )
        .unwrap_err();
        assert_eq!(e.code, ErrorCode::Unsupported);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn crlf_versus_lf_is_visible_not_hidden() {
        let root = temp_root("crlf");
        fs::write(root.join("a.txt"), "one\r\ntwo\r\n").unwrap();
        fs::write(root.join("b.txt"), "one\ntwo\n").unwrap();
        let r = diff_with(&root, None);
        assert!(!r.equal);
        let td = r.text_diff.unwrap();
        assert!(
            !td.hunks.is_empty(),
            "line-ending-only change must show hunks"
        );
        let _ = fs::remove_dir_all(&root);
    }

    // ── Archive members (FER-413) ────────────────────────────────────────

    fn mkzip(path: &Path, members: &[(&str, &[u8])]) {
        use std::io::Write;
        let f = fs::File::create(path).unwrap();
        let mut w = zip::ZipWriter::new(f);
        let opts = zip::write::SimpleFileOptions::default();
        for (name, body) in members {
            w.start_file(*name, opts).unwrap();
            w.write_all(body).unwrap();
        }
        w.finish().unwrap();
    }

    fn snap(p: &Path) -> Vec<(String, Vec<u8>, i64)> {
        let mut v = Vec::new();
        deep_snapshot(p, &mut v, p);
        v
    }

    #[test]
    fn plain_bad_signature_zip_member_is_unsupported_and_missing_file_not_found() {
        let root = temp_root("arch");
        fs::write(root.join("pack.zip"), b"PK").unwrap();
        fs::write(root.join("real.txt"), b"x").unwrap();
        let member = root.join("pack.zip").join("inner.txt");
        let e = compare_files(s(&member), s(&root.join("real.txt"))).unwrap_err();
        assert_eq!(e.code, ErrorCode::Unsupported);
        let e = compare_files(s(&root.join("nope.txt")), s(&root.join("real.txt"))).unwrap_err();
        assert_eq!(e.code, ErrorCode::NotFound);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn zip_members_same_and_different_content() {
        let root = temp_root("zmm");
        let mut big = vec![0u8; 200_000]; // spans several 64 KiB chunks
        big[150_000] = 7;
        let mut big2 = big.clone();
        big2[150_000] = 9; // same size, different byte
        mkzip(
            &root.join("a.zip"),
            &[("d/one.bin", &big), ("same.txt", b"hello\n")],
        );
        mkzip(
            &root.join("b.zip"),
            &[("one.bin", &big2), ("same.txt", b"hello\n")],
        );
        let (a, b) = (root.join("a.zip"), root.join("b.zip"));
        let eq = compare_files(s(&a.join("same.txt")), s(&b.join("same.txt"))).unwrap();
        assert!(eq.equal && eq.both_text);
        assert_eq!((eq.left_size, eq.right_size), (6, 6));
        let ne = compare_files(s(&a.join("d").join("one.bin")), s(&b.join("one.bin"))).unwrap();
        assert!(!ne.equal);
        assert_eq!(ne.first_mismatch, Some(150_000));
        assert_eq!((ne.left_size, ne.right_size), (200_000, 200_000));
        // identical member inside one archive vs itself
        let same = compare_files(s(&a.join("same.txt")), s(&a.join("same.txt"))).unwrap();
        assert!(same.equal);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn zip_text_members_get_a_text_diff_and_line_numbers() {
        let root = temp_root("ztxt");
        mkzip(
            &root.join("a.zip"),
            &[("t.txt", b"one\ntwo\nTHREE\nfour\n")],
        );
        mkzip(
            &root.join("b.zip"),
            &[("t.txt", b"one\ntwo\nthree\nfour\n")],
        );
        let r = compare_files(
            s(&root.join("a.zip").join("t.txt")),
            s(&root.join("b.zip").join("t.txt")),
        )
        .unwrap();
        assert!(!r.equal && r.both_text);
        assert_eq!((r.left_line, r.right_line), (Some(3), Some(3)));
        assert!(!r.text_diff.unwrap().hunks.is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn zip_member_versus_disk_file() {
        let root = temp_root("zdisk");
        mkzip(&root.join("a.zip"), &[("f.txt", b"abcdef")]);
        fs::write(root.join("same.txt"), b"abcdef").unwrap();
        fs::write(root.join("diff.txt"), b"abcxef").unwrap();
        let m = root.join("a.zip").join("f.txt");
        let eq = compare_files(s(&m), s(&root.join("same.txt"))).unwrap();
        assert!(eq.equal);
        let ne = compare_files(s(&root.join("diff.txt")), s(&m)).unwrap();
        assert!(!ne.equal);
        assert_eq!(ne.first_mismatch, Some(3));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn zip_member_errors_missing_directory_corrupt_traversal() {
        let root = temp_root("zerr");
        mkzip(&root.join("a.zip"), &[("d/f.txt", b"x")]);
        fs::write(root.join("real.txt"), b"x").unwrap();
        let real = s(&root.join("real.txt")).to_string();
        // member that does not exist
        let e =
            compare_files(s(&root.join("a.zip").join("d").join("nope.txt")), &real).unwrap_err();
        assert_eq!(e.code, ErrorCode::NotFound);
        // a directory member is not comparable
        let e = compare_files(s(&root.join("a.zip").join("d")), &real).unwrap_err();
        assert_eq!(e.code, ErrorCode::NotADirectory);
        // path traversal out of the member namespace
        let trav = format!("{}\\..\\nope.txt", s(&root.join("a.zip")));
        let e = compare_files(&trav, &real).unwrap_err();
        assert_eq!(e.code, ErrorCode::InvalidPath);
        // corrupt archive: valid local-header signature, truncated body
        let bytes = fs::read(root.join("a.zip")).unwrap();
        fs::write(root.join("bad.zip"), &bytes[..bytes.len() / 2]).unwrap();
        let e = compare_files(s(&root.join("bad.zip").join("d").join("f.txt")), &real).unwrap_err();
        assert_eq!(e.code, ErrorCode::Io, "corrupt archive is an error: {e:?}");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn rar_and_other_archive_formats_stay_unsupported_with_a_reason() {
        let root = temp_root("zunsup");
        fs::write(root.join("real.txt"), b"x").unwrap();
        let real = s(&root.join("real.txt")).to_string();
        for (name, needle) in [("a.rar", "RAR"), ("a.tar", ".tar"), ("a.gz", ".gz")] {
            fs::write(root.join(name), b"not really").unwrap();
            let e = compare_files(s(&root.join(name).join("m.txt")), &real).unwrap_err();
            assert_eq!(e.code, ErrorCode::Unsupported, "{name}");
            assert!(e.message.contains(needle), "{name}: {}", e.message);
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn sevenz_members_are_compared_in_place() {
        let root = temp_root("z7");
        let src = root.join("src");
        fs::create_dir_all(src.join("x")).unwrap();
        fs::create_dir_all(src.join("y")).unwrap();
        fs::write(src.join("x").join("m.txt"), b"seven same\n").unwrap();
        fs::write(src.join("y").join("m.txt"), b"seven same\n").unwrap();
        fs::write(src.join("y").join("n.txt"), b"seven diff\n").unwrap();
        fs::write(src.join("x").join("n.txt"), b"seven DIFF\n").unwrap();
        let mk = |dir: &str, out: &str| {
            crate::archive_create_test::create_7z(
                &[s(&src.join(dir)).to_string()],
                &root.join(out),
            )
            .unwrap();
        };
        mk("x", "x.7z");
        mk("y", "y.7z");
        let (x, y) = (root.join("x.7z"), root.join("y.7z"));
        assert!(
            compare_files(s(&x.join("x").join("m.txt")), s(&y.join("y").join("m.txt")))
                .unwrap()
                .equal
        );
        let r =
            compare_files(s(&x.join("x").join("n.txt")), s(&y.join("y").join("n.txt"))).unwrap();
        assert!(!r.equal);
        let d = compare_files(
            s(&x.join("x").join("m.txt")),
            s(&src.join("y").join("m.txt")),
        )
        .unwrap();
        assert!(d.equal);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn archive_compare_writes_nothing_to_disk() {
        let root = temp_root("zro");
        let tmp_probe = temp_root("zro-tmp");
        mkzip(&root.join("a.zip"), &[("f.txt", b"one\ntwo\n")]);
        mkzip(&root.join("b.zip"), &[("f.txt", b"one\nTWO\n")]);
        fs::write(root.join("d.txt"), b"one\nthree\n").unwrap();
        let before = (snap(&root), snap(&tmp_probe));
        let (a, b) = (
            root.join("a.zip").join("f.txt"),
            root.join("b.zip").join("f.txt"),
        );
        compare_files(s(&a), s(&b)).unwrap();
        compare_files(s(&a), s(&root.join("d.txt"))).unwrap();
        assert_eq!((snap(&root), snap(&tmp_probe)), before);
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&tmp_probe);
    }

    #[test]
    fn archive_member_compare_can_be_cancelled_mid_stream() {
        let root = temp_root("zcancel");
        let big = vec![0xABu8; 4 * 1024 * 1024]; // 64 chunks per side
        mkzip(&root.join("a.zip"), &[("f.bin", &big)]);
        mkzip(&root.join("b.zip"), &[("f.bin", &big)]);
        let ctl = Ctl::none();
        ctl.trip_after.store(3, Ordering::SeqCst);
        let r = compare_files_with(
            s(&root.join("a.zip").join("f.bin")),
            s(&root.join("b.zip").join("f.bin")),
            &FileOpts::default(),
            &ctl,
        )
        .unwrap();
        assert!(r.cancelled);
        // a cancelled compare is never cached: the next run streams for real
        let again = compare_files(
            s(&root.join("a.zip").join("f.bin")),
            s(&root.join("b.zip").join("f.bin")),
        )
        .unwrap();
        assert!(again.equal && !again.cancelled && !again.from_cache);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn archive_member_progress_is_reported() {
        let root = temp_root("zprog");
        let big = vec![1u8; 1024 * 1024];
        mkzip(&root.join("a.zip"), &[("f.bin", &big)]);
        mkzip(&root.join("b.zip"), &[("f.bin", &big)]);
        let seen = Arc::new(Mutex::new(Vec::<CompareProgress>::new()));
        let sink_seen = seen.clone();
        let sink: ProgressSink = Arc::new(move |p| sink_seen.lock().unwrap().push(p));
        let ctl = Ctl::new(Some("zprog-op".into()), Some(sink));
        let r = compare_files_with(
            s(&root.join("a.zip").join("f.bin")),
            s(&root.join("b.zip").join("f.bin")),
            &FileOpts::default(),
            &ctl,
        )
        .unwrap();
        assert!(r.equal);
        let got = seen.lock().unwrap();
        assert!(!got.is_empty());
        assert!(
            got.iter()
                .all(|p| p.op_id == "zprog-op" && p.phase == ProgressPhase::Content)
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn member_verdicts_are_cached_by_archive_identity_and_invalidated_on_change() {
        let root = temp_root("zcache");
        mkzip(&root.join("a.zip"), &[("f.txt", b"same-size-1")]);
        mkzip(&root.join("b.zip"), &[("f.txt", b"same-size-1")]);
        let (a, b) = (
            root.join("a.zip").join("f.txt"),
            root.join("b.zip").join("f.txt"),
        );
        let first = compare_files(s(&a), s(&b)).unwrap();
        assert!(first.equal && !first.from_cache);
        let second = compare_files(s(&a), s(&b)).unwrap();
        assert!(second.equal && second.from_cache);
        // rewrite the archive with different content of the same member size,
        // and force a different archive mtime: the old verdict must not apply
        mkzip(&root.join("b.zip"), &[("f.txt", b"same-size-2")]);
        set_mtime(&root.join("b.zip"), 1_600_000_000);
        let third = compare_files(s(&a), s(&b)).unwrap();
        assert!(!third.equal && !third.from_cache);
        let _ = fs::remove_dir_all(&root);
    }

    fn deep_snapshot(root: &Path, out: &mut Vec<(String, Vec<u8>, i64)>, base: &Path) {
        for e in fs::read_dir(root).unwrap().flatten() {
            let p = e.path();
            let lm = fs::symlink_metadata(&p).unwrap();
            let rel = p.strip_prefix(base).unwrap().to_string_lossy().into_owned();
            let body = if lm.is_file() {
                fs::read(&p).unwrap()
            } else {
                Vec::new()
            };
            out.push((rel, body, crate::fs_service::modified_ms(&lm)));
            if lm.is_dir() {
                deep_snapshot(&p, out, base);
            }
        }
        out.sort();
    }

    #[test]
    fn recursive_compare_and_file_compare_never_mutate_trees() {
        let l = temp_root("rro-l");
        let r = temp_root("rro-r");
        fs::create_dir_all(l.join("d/e")).unwrap();
        fs::create_dir_all(r.join("d/e")).unwrap();
        write(&l.join("d/e"), "a.txt", b"left\n", Some(1_700_000_000));
        write(&r.join("d/e"), "a.txt", b"rite\n", Some(1_700_000_000));
        write(&l, "only.txt", b"1", None);
        let snap = |p: &Path| {
            let mut v = Vec::new();
            deep_snapshot(p, &mut v, p);
            v
        };
        let before = (snap(&l), snap(&r));
        for by_content in [false, true] {
            compare_dirs_with(s(&l), s(&r), rec(by_content), &Ctl::none()).unwrap();
        }
        compare_files(s(&l.join("d/e/a.txt")), s(&r.join("d/e/a.txt"))).unwrap();
        assert_eq!((snap(&l), snap(&r)), before);
        let _ = fs::remove_dir_all(&l);
        let _ = fs::remove_dir_all(&r);
    }

    #[test]
    fn wire_shape_matches_the_ts_contract() {
        let r = CompareResult {
            equal: false,
            cancelled: false,
            from_cache: true,
            text_diff: Some(TextDiff {
                hunks: vec![TextDiffHunk {
                    left_start: 1,
                    right_start: 1,
                    lines: vec![TextDiffLine {
                        kind: DiffLineKind::Add,
                        text: "x".into(),
                    }],
                }],
                truncated: false,
                encoding: "utf-8".into(),
            }),
            first_mismatch: Some(0),
            left_size: 1,
            right_size: 2,
            left_line: None,
            right_line: None,
            both_text: true,
        };
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["fromCache"], true);
        assert!(v.get("cancelled").is_none());
        assert_eq!(v["textDiff"]["hunks"][0]["leftStart"], 1);
        assert_eq!(v["textDiff"]["hunks"][0]["lines"][0]["kind"], "add");
        let p = serde_json::to_value(CompareProgress {
            op_id: "o".into(),
            phase: ProgressPhase::Content,
            done: 1,
            total: None,
            current: None,
        })
        .unwrap();
        assert_eq!(p["opId"], "o");
        assert_eq!(p["phase"], "content");
    }

    // ── FER-477b: content hint from the journal change lists ──────────────

    /// Same size and mtime, different bytes: only a content read can tell.
    /// The hint skips that read ONLY for a pair whose directory is unchanged
    /// on both sides and whose facts equal the content-verified baseline.
    #[test]
    fn content_hint_skips_reads_only_when_the_journal_proves_no_change() {
        use crate::sync_baseline::{BaselineDto, EntryDto, FactsDto};
        use crate::sync_incremental::{ChangedSet, ContentHint};
        use ferrum_indexer::query::ChangedDir;
        let l = temp_root("hint-l");
        let r = temp_root("hint-r");
        for root in [&l, &r] {
            fs::create_dir_all(root.join("sub")).unwrap();
        }
        write(&l, "a.txt", b"AAAA", Some(1_600_000_000));
        write(&r, "a.txt", b"BBBB", Some(1_600_000_000));
        write(&l.join("sub"), "b.txt", b"CCCC", Some(1_600_000_000));
        write(&r.join("sub"), "b.txt", b"DDDD", Some(1_600_000_000));
        let plain = compare_dirs_with(s(&l), s(&r), rec(true), &Ctl::none()).unwrap();
        assert_eq!(state_of(&plain, "a.txt"), DirDiffState::ContentDifferent);
        let facts = |e: &DirDiffEntry| EntryDto {
            left: FactsDto {
                size: e.left_size,
                modified_ms: e.left_modified_ms,
            },
            right: FactsDto {
                size: e.right_size,
                modified_ms: e.right_modified_ms,
            },
        };
        let entry = |n: &str| plain.entries.iter().find(|e| e.name == n).unwrap();
        let mut entries = std::collections::BTreeMap::new();
        entries.insert("a.txt".to_string(), facts(entry("a.txt")));
        entries.insert("sub/b.txt".to_string(), facts(entry("sub/b.txt")));
        let baseline = BaselineDto {
            version: crate::sync_baseline::BASELINE_VERSION,
            left_root: s(&l).into(),
            right_root: s(&r).into(),
            entries,
            journal: None,
        };
        // `sub` changed on the right: its pair is read, the root's is not.
        let hint = Arc::new(ContentHint::new(
            ChangedSet::default(),
            ChangedSet::from_dirs(&[ChangedDir {
                rel: "sub".into(),
                recursive: false,
            }]),
            &baseline,
        ));
        let mut ctl = Ctl::none();
        ctl.content_hint = Some(hint.clone());
        let rep = compare_dirs_with(s(&l), s(&r), rec(true), &ctl).unwrap();
        assert_eq!(
            state_of(&rep, "a.txt"),
            DirDiffState::Same,
            "proved unchanged since a content-verified baseline"
        );
        assert_eq!(
            state_of(&rep, "sub/b.txt"),
            DirDiffState::ContentDifferent,
            "changed directory: read"
        );
        assert_eq!(hint.skipped(), 1);
        // Facts that moved since the baseline (here: mtime) → read again.
        set_mtime(&l.join("a.txt"), 1_600_000_100);
        set_mtime(&r.join("a.txt"), 1_600_000_100);
        let rep = compare_dirs_with(s(&l), s(&r), rec(true), &ctl).unwrap();
        assert_eq!(state_of(&rep, "a.txt"), DirDiffState::ContentDifferent);
        let _ = fs::remove_dir_all(&l);
        let _ = fs::remove_dir_all(&r);
    }
}
