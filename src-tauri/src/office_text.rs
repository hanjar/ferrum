//! FER-493: plain-text extraction from Office documents (DOCX/XLSX/PPTX, plus ODT/ODS/ODP) for
//! "Add to chat" (FER-486) and "Ask AI agent" (FER-490). The input is UNTRUSTED.
//!
//! Safety rules (design decision + security review O1-O4):
//! - the file is read only through a `path_guard` handle (`GuardedFile::open_read`);
//! - zip: the existing `zip` crate. BEFORE the central directory is parsed, the entry count and
//!   the central-directory size are read from the EOCD (or ZIP64 EOCD) record and refused above
//!   `max_entries` / `max_cd_bytes` (O2). Any encrypted entry refuses the whole document. Every
//!   part is size-checked from the central directory BEFORE inflating (per-part cap, total
//!   inflated cap, compression ratio) and the inflated bytes are capped again while reading;
//! - XML: `quick-xml` streaming reader. It never loads a DTD or external entity; a document that
//!   carries a `<!DOCTYPE` is refused outright, unknown entity references are dropped (only the
//!   five predefined entities and character references are resolved), element depth is capped;
//! - memory (O1): every kept item (shared string, cell, paragraph) is capped at
//!   `MAX_ITEM_BYTES` (and the room left in the output) BEFORE it is copied, with a `[…]` mark;
//!   shared strings are stored once (`Rc<str>`) and referenced, never copied per cell; cells past
//!   `max_cols` and rows past `max_rows` are never copied; ODS repeats are bounded by the column,
//!   row and output limits and stop when the output is full;
//! - a wall-clock deadline, checked every N events AND every 256 KB of text, plus a cancel flag;
//! - nothing is executed: macros (`.docm`, `.xlsm`, `.pptm`) are never looked at, formulas are not
//!   evaluated (the cached value is taken), embedded objects are ignored;
//! - pictures get synthetic names (`image001.png`, ...): no name from the zip reaches the disk (O3).
//!
//! Nothing in this module touches the filesystem by path and nothing is logged.

use std::collections::HashMap;
use std::fmt;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use quick_xml::XmlVersion;
use quick_xml::events::{BytesRef, BytesStart, Event};
use quick_xml::reader::Reader;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Docx,
    Xlsx,
    Pptx,
    Odt,
    Ods,
    Odp,
}

impl Format {
    /// Folder of embedded pictures inside the package (DOCX/PPTX only).
    fn media_prefix(self) -> Option<&'static str> {
        match self {
            Format::Docx => Some("word/media/"),
            Format::Pptx => Some("ppt/media/"),
            _ => None,
        }
    }

    pub fn has_media(self) -> bool {
        self.media_prefix().is_some()
    }
}

/// Office format from the file name (zip-based formats only; legacy `.doc/.xls/.ppt` are not).
pub fn format_by_name(name: &str) -> Option<Format> {
    let (_, ext) = name.rsplit_once('.')?;
    Some(match ext.to_ascii_lowercase().as_str() {
        "docx" | "docm" | "dotx" | "dotm" => Format::Docx,
        "xlsx" | "xlsm" | "xltx" | "xltm" => Format::Xlsx,
        "pptx" | "pptm" | "ppsx" | "ppsm" | "potx" | "potm" => Format::Pptx,
        "odt" | "ott" => Format::Odt,
        "ods" | "ots" => Format::Ods,
        "odp" | "otp" => Format::Odp,
        _ => return None,
    })
}

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Largest source file that is opened at all.
    pub max_file_bytes: u64,
    pub max_entries: usize,
    /// Largest central directory (read from the EOCD before it is parsed).
    pub max_cd_bytes: u64,
    /// Total bytes inflated from the package (all parts together).
    pub max_inflated: u64,
    /// Largest single XML part.
    pub max_part_bytes: u64,
    /// Refuse a part whose uncompressed/compressed ratio is above this...
    pub max_ratio: u64,
    /// ...once it is at least this large (small parts compress well legitimately).
    pub ratio_min_size: u64,
    /// Text produced (bytes, UTF-8) before the output is cut.
    pub max_text_bytes: usize,
    /// Longest single kept item (shared string, cell, paragraph).
    pub max_item_bytes: usize,
    /// Shared strings kept (XLSX).
    pub max_shared: usize,
    pub max_depth: usize,
    pub timeout: Duration,
    /// Rows per sheet/table (XLSX/ODS/ODT tables).
    pub max_rows: usize,
    /// Columns per sheet/table row.
    pub max_cols: usize,
    /// Sheets, slides or header/footer parts read.
    pub max_parts: usize,
    pub max_media: usize,
    pub max_media_bytes: u64,
}

pub const MAX_FILE_BYTES: u64 = 100 * 1024 * 1024;
pub const MAX_INFLATED: u64 = 50 * 1024 * 1024;
pub const MAX_TEXT_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_ITEM_BYTES: usize = 32 * 1024;
pub const MAX_MEDIA: usize = 20;
pub const MAX_MEDIA_BYTES: u64 = 10 * 1024 * 1024;
/// Deadline for a full extraction (send time, agent snapshot).
pub const TIMEOUT: Duration = Duration::from_secs(10);
/// Shorter deadline while the chip is inspected (O4); a document that needs longer is extracted
/// at send time.
pub const INSPECT_TIMEOUT: Duration = Duration::from_secs(3);
/// Mark appended to an item that was cut.
const CUT_MARK: &str = " […]";
/// The deadline is also checked after this much text (a single event can be large).
const TIME_CHECK_BYTES: usize = 256 * 1024;

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_file_bytes: MAX_FILE_BYTES,
            max_entries: 10_000,
            max_cd_bytes: 8 * 1024 * 1024,
            max_inflated: MAX_INFLATED,
            max_part_bytes: 30 * 1024 * 1024,
            max_ratio: 200,
            ratio_min_size: 1024 * 1024,
            max_text_bytes: MAX_TEXT_BYTES,
            max_item_bytes: MAX_ITEM_BYTES,
            max_shared: 500_000,
            max_depth: 256,
            timeout: TIMEOUT,
            max_rows: 2000,
            max_cols: 50,
            max_parts: 500,
            max_media: MAX_MEDIA,
            max_media_bytes: MAX_MEDIA_BYTES,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OfficeError {
    /// Not a zip package: legacy binary format, or a password-protected OOXML (CFB container).
    NotPackage,
    Encrypted,
    Limit(&'static str),
    Timeout,
    Cancelled,
    Doctype,
    Corrupt,
    Unreadable,
}

impl fmt::Display for OfficeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OfficeError::NotPackage => {
                f.write_str("bukan dokumen Office berbasis zip (format lama atau terenkripsi)")
            }
            OfficeError::Encrypted => f.write_str("dokumen terenkripsi"),
            OfficeError::Limit(what) => write!(f, "melewati batas aman ({what})"),
            OfficeError::Timeout => f.write_str("waktu ekstraksi habis"),
            OfficeError::Cancelled => f.write_str("dibatalkan"),
            OfficeError::Doctype => f.write_str("XML memuat DOCTYPE (ditolak)"),
            OfficeError::Corrupt => f.write_str("dokumen rusak atau tidak dikenali"),
            OfficeError::Unreadable => f.write_str("file tidak dapat dibaca"),
        }
    }
}

type Res<T> = Result<T, OfficeError>;

/// Text extracted from one document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extraction {
    pub text: String,
    /// The text hit `max_text_bytes` and was cut.
    pub truncated: bool,
}

/// One embedded picture (PNG/JPEG/GIF, magic bytes checked).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Media {
    /// Synthetic name (`image001.png`); nothing from the zip.
    pub name: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MediaSet {
    pub items: Vec<Media>,
    /// Pictures left out (count/size limit, bad signature, refused part).
    pub skipped: usize,
}

/// Text and pictures from one pass over a package (agent snapshot, O4).
#[derive(Debug)]
pub struct Content {
    pub text: Res<Extraction>,
    pub media: MediaSet,
}

/// A flag that is never raised (no cancel).
static NEVER: AtomicBool = AtomicBool::new(false);

// ---------------------------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------------------------

fn open_checked(
    file: &crate::path_guard::GuardedFile,
    limits: &Limits,
) -> Res<BufReader<std::fs::File>> {
    if file.meta.len() > limits.max_file_bytes {
        return Err(OfficeError::Limit("ukuran file"));
    }
    let handle = file.open_read().map_err(|_| OfficeError::Unreadable)?;
    Ok(BufReader::new(handle))
}

/// Text of a checked disk file (read through the checked handle, never by path).
pub fn extract_file(
    file: &crate::path_guard::GuardedFile,
    format: Format,
    timeout: Duration,
    stop: &AtomicBool,
) -> Res<Extraction> {
    let limits = Limits {
        timeout,
        ..Limits::default()
    };
    extract_with(open_checked(file, &limits)?, format, &limits, stop)
}

/// Text AND pictures of a checked disk file in one pass (one package open, one budget).
pub fn content_file(
    file: &crate::path_guard::GuardedFile,
    format: Format,
    stop: &AtomicBool,
) -> Res<Content> {
    let limits = Limits::default();
    content_with(open_checked(file, &limits)?, format, &limits, stop)
}

pub fn extract_from<R: Read + Seek>(reader: R, format: Format, limits: &Limits) -> Res<Extraction> {
    extract_with(reader, format, limits, &NEVER)
}

pub fn extract_with<R: Read + Seek>(
    reader: R,
    format: Format,
    limits: &Limits,
    stop: &AtomicBool,
) -> Res<Extraction> {
    let mut pkg = Package::open(reader, limits, stop)?;
    text_of(&mut pkg, format)
}

pub fn content_with<R: Read + Seek>(
    reader: R,
    format: Format,
    limits: &Limits,
    stop: &AtomicBool,
) -> Res<Content> {
    let mut pkg = Package::open(reader, limits, stop)?;
    let text = text_of(&mut pkg, format);
    if matches!(text, Err(OfficeError::Timeout | OfficeError::Cancelled)) {
        return Ok(Content {
            text,
            media: MediaSet::default(),
        });
    }
    let media = media_of(&mut pkg, format)?;
    Ok(Content { text, media })
}

pub fn media_from<R: Read + Seek>(reader: R, format: Format, limits: &Limits) -> Res<MediaSet> {
    if !format.has_media() {
        return Ok(MediaSet::default());
    }
    let mut pkg = Package::open(reader, limits, &NEVER)?;
    media_of(&mut pkg, format)
}

fn text_of<R: Read + Seek>(pkg: &mut Package<'_, R>, format: Format) -> Res<Extraction> {
    let limits = pkg.limits;
    let mut sink = Sink::new(limits.max_text_bytes);
    match format {
        Format::Docx => docx(pkg, &mut sink)?,
        Format::Xlsx => xlsx(pkg, &mut sink)?,
        Format::Pptx => pptx(pkg, &mut sink)?,
        Format::Odt | Format::Ods | Format::Odp => odf(pkg, &mut sink, format)?,
    }
    let truncated = sink.full;
    let mut text = sink.out;
    let trimmed = text.trim_end().len();
    text.truncate(trimmed);
    if !text.is_empty() {
        text.push('\n');
    }
    if truncated {
        text.push_str(&format!(
            "[teks dipotong: batas {} KB teks]\n",
            limits.max_text_bytes / 1024
        ));
    }
    Ok(Extraction { text, truncated })
}

fn media_of<R: Read + Seek>(pkg: &mut Package<'_, R>, format: Format) -> Res<MediaSet> {
    let Some(prefix) = format.media_prefix() else {
        return Ok(MediaSet::default());
    };
    let limits = pkg.limits;
    let mut names: Vec<String> = pkg
        .names
        .keys()
        .filter(|n| {
            n.strip_prefix(prefix).is_some_and(|rest| {
                !rest.contains('/')
                    && matches!(
                        rest.rsplit_once('.').map(|(_, e)| e),
                        Some("png" | "jpg" | "jpeg" | "gif")
                    )
            })
        })
        .cloned()
        .collect();
    names.sort_by_key(|n| natural_key(n));
    let mut set = MediaSet::default();
    let mut used: u64 = 0;
    for lower in names {
        pkg.check_time()?;
        if set.items.len() >= limits.max_media {
            set.skipped += 1;
            continue;
        }
        let room = limits.max_media_bytes.saturating_sub(used);
        let bytes = match pkg.read(&lower, room) {
            Ok(Some(b)) => b,
            Ok(None) | Err(OfficeError::Limit(_)) => {
                set.skipped += 1;
                continue;
            }
            Err(e) => return Err(e),
        };
        let Some(ext) = image_ext(&bytes) else {
            set.skipped += 1;
            continue;
        };
        used += bytes.len() as u64;
        // O3: synthetic, ordered name; nothing from the zip (no CON/NUL/COM1, no odd chars).
        let name = format!("image{:03}.{ext}", set.items.len() + 1);
        set.items.push(Media { name, bytes });
    }
    Ok(set)
}

/// Extension from the picture's own signature (`None` = not a PNG/JPEG/GIF).
fn image_ext(b: &[u8]) -> Option<&'static str> {
    if b.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some("png")
    } else if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("jpg")
    } else if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        Some("gif")
    } else {
        None
    }
}

/// Sort key that puts `image10` after `image9`.
fn natural_key(s: &str) -> (String, u64) {
    let stem = s.rsplit_once('.').map_or(s, |(stem, _)| stem);
    let digits = stem
        .bytes()
        .rev()
        .take_while(u8::is_ascii_digit)
        .count()
        .min(18);
    let n = stem[stem.len() - digits..].parse().unwrap_or(0);
    (stem[..stem.len() - digits].to_owned(), n)
}

// ---------------------------------------------------------------------------------------------
// Cache (O4): (final path, mtime, size) -> extraction, small and short-lived
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
struct CacheKey {
    path: String,
    modified: Option<SystemTime>,
    size: u64,
    format: Format,
}

struct CacheEntry {
    key: CacheKey,
    at: Instant,
    value: Res<Arc<Extraction>>,
}

const CACHE_ENTRIES: usize = 8;
const CACHE_TTL: Duration = Duration::from_secs(10 * 60);
static CACHE: Mutex<Vec<CacheEntry>> = Mutex::new(Vec::new());

fn cache_key(file: &crate::path_guard::GuardedFile, format: Format) -> CacheKey {
    let path = file
        .final_path()
        .unwrap_or_else(|| file.path.clone())
        .to_lowercase();
    CacheKey {
        path,
        modified: file.meta.modified().ok(),
        size: file.meta.len(),
        format,
    }
}

/// `extract_file` through the cache. Timeouts, cancels and read errors are never cached (the
/// next attempt, e.g. at send time with the longer deadline, tries again).
pub fn extract_file_cached(
    file: &crate::path_guard::GuardedFile,
    format: Format,
    timeout: Duration,
    stop: &AtomicBool,
) -> Res<Arc<Extraction>> {
    let key = cache_key(file, format);
    if let Ok(mut cache) = CACHE.lock() {
        cache.retain(|e| e.at.elapsed() < CACHE_TTL);
        if let Some(e) = cache.iter().find(|e| e.key == key) {
            return e.value.clone();
        }
    }
    let value = extract_file(file, format, timeout, stop).map(Arc::new);
    if !matches!(
        value,
        Err(OfficeError::Timeout | OfficeError::Cancelled | OfficeError::Unreadable)
    ) && let Ok(mut cache) = CACHE.lock()
    {
        cache.retain(|e| e.key != key);
        if cache.len() >= CACHE_ENTRIES {
            cache.remove(0);
        }
        cache.push(CacheEntry {
            key,
            at: Instant::now(),
            value: value.clone(),
        });
    }
    value
}

// ---------------------------------------------------------------------------------------------
// Zip package with limits
// ---------------------------------------------------------------------------------------------

fn le16(b: &[u8], at: usize) -> u64 {
    u64::from(u16::from_le_bytes([b[at], b[at + 1]]))
}

fn le32(b: &[u8], at: usize) -> u64 {
    u64::from(u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]))
}

fn le64(b: &[u8], at: usize) -> u64 {
    let mut x = [0u8; 8];
    x.copy_from_slice(&b[at..at + 8]);
    u64::from_le_bytes(x)
}

fn read_exact_at<R: Read + Seek>(reader: &mut R, at: u64, buf: &mut [u8]) -> Res<()> {
    reader
        .seek(SeekFrom::Start(at))
        .map_err(|_| OfficeError::Unreadable)?;
    reader.read_exact(buf).map_err(|_| OfficeError::Corrupt)
}

/// Calls `f(offset, record)` for every `sig` in `[from, to)` whose `rec_len`-byte record fits
/// before `to` (chunked, so the file is never loaded whole).
fn scan_records<R: Read + Seek>(
    reader: &mut R,
    (from, to): (u64, u64),
    sig: &[u8; 4],
    rec_len: usize,
    deadline: Instant,
    stop: &AtomicBool,
    mut f: impl FnMut(u64, &[u8]),
) -> Res<()> {
    const CHUNK: usize = 1024 * 1024;
    let mut buf = vec![0u8; CHUNK + rec_len];
    let mut pos = from;
    while pos < to {
        check(deadline, stop)?;
        let want = ((to - pos) as usize).min(CHUNK + rec_len - 1);
        read_exact_at(reader, pos, &mut buf[..want])?;
        let data = &buf[..want];
        // Records starting in this chunk (the overlap lets a record cross the chunk end).
        let starts = want.min(CHUNK);
        for i in 0..starts {
            if data[i] == b'P' && i + rec_len <= want && &data[i..i + 4] == sig {
                f(pos + i as u64, &data[i..i + rec_len]);
            }
        }
        pos += starts as u64;
    }
    Ok(())
}

/// O2/O2b: the LARGEST (entries, central directory bytes) over EVERY end-of-central-directory
/// candidate the zip crate could pick. The crate walks all EOCD signatures from the end of the
/// file and skips the ones that fail its checks, so a small fake EOCD (e.g. in the comment) must
/// not hide a big real one: every candidate that passes the same cheap checks counts, ZIP64
/// candidates through every EOCD64 record in the range the crate searches. Nothing of the
/// central directory itself is parsed.
fn eocd_facts<R: Read + Seek>(
    reader: &mut R,
    deadline: Instant,
    stop: &AtomicBool,
) -> Res<(u64, u64)> {
    const EOCD: &[u8; 4] = b"PK\x05\x06";
    const LOCATOR: &[u8; 4] = b"PK\x06\x07";
    const EOCD64: &[u8; 4] = b"PK\x06\x06";
    /// ZIP64 candidates followed (each one costs a scan); more than this is refused.
    const MAX_ZIP64_CANDIDATES: usize = 16;
    let len = reader
        .seek(SeekFrom::End(0))
        .map_err(|_| OfficeError::Unreadable)?;
    let mut best: Option<(u64, u64)> = None;
    let mut count = |entries: u64, cd: u64| {
        let (e, c) = best.unwrap_or((0, 0));
        best = Some((e.max(entries), c.max(cd)));
    };
    let mut zip64: Vec<u64> = Vec::new(); // EOCD offsets with a ZIP64 locator in front
    let mut too_many = false;
    scan_records(reader, (0, len), EOCD, 22, deadline, stop, |off, b| {
        let entries = le16(b, 10);
        let cd_size = le32(b, 12);
        let cd_offset = le32(b, 16);
        let comment = le16(b, 20);
        // Same as the crate: the comment must end inside the file.
        if off + 22 + comment > len {
            return;
        }
        if entries == 0xFFFF || cd_offset == 0xFFFF_FFFF {
            if off >= 20 {
                if zip64.len() < MAX_ZIP64_CANDIDATES {
                    zip64.push(off);
                } else {
                    too_many = true;
                }
            }
            // Without a usable locator the crate reads it as a classic EOCD: count it too.
            count(entries, cd_size);
            return;
        }
        // Same as the crate: a non-empty zip32 CD must start before its EOCD.
        if entries != 0 && cd_offset >= off {
            return;
        }
        count(entries, cd_size);
    })?;
    if too_many {
        return Err(OfficeError::Limit("jumlah entri zip"));
    }
    for eocd_off in zip64 {
        let locator_off = eocd_off - 20;
        let mut loc = [0u8; 20];
        read_exact_at(reader, locator_off, &mut loc)?;
        if &loc[..4] != LOCATOR {
            continue;
        }
        let eocd64_from = le64(&loc, 8);
        if eocd64_from >= locator_off {
            continue;
        }
        scan_records(
            reader,
            (eocd64_from, locator_off),
            EOCD64,
            56,
            deadline,
            stop,
            |_, rec| count(le64(rec, 32), le64(rec, 40)),
        )?;
    }
    best.ok_or(OfficeError::Corrupt)
}

struct Package<'s, R: Read + Seek> {
    zip: zip::ZipArchive<R>,
    /// lowercase part name -> index (OPC part names are case-insensitive).
    names: HashMap<String, usize>,
    limits: Limits,
    inflated: u64,
    deadline: Instant,
    stop: &'s AtomicBool,
}

impl<'s, R: Read + Seek> Package<'s, R> {
    fn open(mut reader: R, limits: &Limits, stop: &'s AtomicBool) -> Res<Self> {
        let deadline = Instant::now() + limits.timeout;
        let mut magic = [0u8; 4];
        let mut got = 0;
        while got < magic.len() {
            match reader.read(&mut magic[got..]) {
                Ok(0) => break,
                Ok(n) => got += n,
                Err(_) => return Err(OfficeError::Unreadable),
            }
        }
        if got == 4 && magic == [0xD0, 0xCF, 0x11, 0xE0] {
            // OLE compound file: legacy .doc/.xls/.ppt or an encrypted OOXML package.
            return Err(OfficeError::NotPackage);
        }
        if got < 4 || magic != *b"PK\x03\x04" {
            return Err(OfficeError::Corrupt);
        }
        let (entries, cd_size) = eocd_facts(&mut reader, deadline, stop)?;
        if entries > limits.max_entries as u64 {
            return Err(OfficeError::Limit("jumlah entri zip"));
        }
        if cd_size > limits.max_cd_bytes {
            return Err(OfficeError::Limit("ukuran central directory"));
        }
        reader
            .seek(SeekFrom::Start(0))
            .map_err(|_| OfficeError::Unreadable)?;
        let mut zip = zip::ZipArchive::new(reader).map_err(|_| OfficeError::Corrupt)?;
        if zip.len() > limits.max_entries {
            return Err(OfficeError::Limit("jumlah entri zip"));
        }
        let mut names = HashMap::with_capacity(zip.len());
        for i in 0..zip.len() {
            if i.is_multiple_of(256) {
                check(deadline, stop)?;
            }
            let raw = zip.by_index_raw(i).map_err(|_| OfficeError::Corrupt)?;
            if raw.encrypted() {
                return Err(OfficeError::Encrypted);
            }
            names.insert(raw.name().to_ascii_lowercase(), i);
        }
        Ok(Self {
            zip,
            names,
            limits: *limits,
            inflated: 0,
            deadline,
            stop,
        })
    }

    fn check_time(&self) -> Res<()> {
        check(self.deadline, self.stop)
    }

    fn ctl(&self) -> Ctl<'s> {
        Ctl {
            deadline: self.deadline,
            max_depth: self.limits.max_depth,
            stop: self.stop,
        }
    }

    fn has(&self, name: &str) -> bool {
        self.names.contains_key(&name.to_ascii_lowercase())
    }

    fn part(&mut self, name: &str) -> Res<Option<Vec<u8>>> {
        let max = self.limits.max_part_bytes;
        self.read(name, max)
    }

    /// Inflates one part (at most `max` bytes, and within the package-wide budget).
    fn read(&mut self, name: &str, max: u64) -> Res<Option<Vec<u8>>> {
        self.check_time()?;
        let Some(&index) = self.names.get(&name.to_ascii_lowercase()) else {
            return Ok(None);
        };
        let (compressed, size) = {
            let raw = self
                .zip
                .by_index_raw(index)
                .map_err(|_| OfficeError::Corrupt)?;
            if raw.encrypted() {
                return Err(OfficeError::Encrypted);
            }
            (raw.compressed_size(), raw.size())
        };
        let room = self.limits.max_inflated.saturating_sub(self.inflated);
        let cap = max.min(room);
        if size > max {
            return Err(OfficeError::Limit("ukuran bagian dokumen"));
        }
        if size > room {
            return Err(OfficeError::Limit("total data terurai"));
        }
        let (ratio_min, max_ratio) = (self.limits.ratio_min_size, self.limits.max_ratio);
        let ratio_bad =
            |bytes: u64| bytes >= ratio_min && (compressed == 0 || bytes / compressed > max_ratio);
        if ratio_bad(size) {
            return Err(OfficeError::Limit("rasio kompresi"));
        }
        let file = self.zip.by_index(index).map_err(|_| OfficeError::Corrupt)?;
        let mut out = Vec::with_capacity(size.min(cap) as usize);
        file.take(cap + 1)
            .read_to_end(&mut out)
            .map_err(|_| OfficeError::Corrupt)?;
        let got = out.len() as u64;
        if got > cap {
            // Declared size lied.
            return Err(OfficeError::Limit("total data terurai"));
        }
        if ratio_bad(got) {
            return Err(OfficeError::Limit("rasio kompresi"));
        }
        self.inflated += got;
        self.check_time()?;
        Ok(Some(out))
    }
}

fn check(deadline: Instant, stop: &AtomicBool) -> Res<()> {
    if stop.load(Ordering::Relaxed) {
        return Err(OfficeError::Cancelled);
    }
    if Instant::now() > deadline {
        return Err(OfficeError::Timeout);
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// XML walker
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Ctl<'s> {
    deadline: Instant,
    max_depth: usize,
    stop: &'s AtomicBool,
}

enum Node<'a, 'b> {
    Open(&'a BytesStart<'b>),
    Close(&'a [u8]),
    Text(&'a str),
}

#[derive(PartialEq, Eq)]
enum Flow {
    Go,
    Stop,
}

fn predefined(name: &str) -> Option<char> {
    Some(match name {
        "lt" => '<',
        "gt" => '>',
        "amp" => '&',
        "apos" => '\'',
        "quot" => '"',
        _ => return None,
    })
}

/// Character references and the five predefined entities; anything else is dropped (no DTD is
/// ever read, so no other entity can be defined).
fn resolve_ref(r: &BytesRef<'_>) -> Option<char> {
    if r.is_char_ref() {
        return r.resolve_char_ref().ok().flatten();
    }
    predefined(&r.xml10_content())
}

/// Streams `xml`, calling `f` for element starts/ends and text. Empty elements arrive as a start
/// plus an end. `<!DOCTYPE` refuses the document. The deadline and the cancel flag are checked
/// every 512 events and after every `TIME_CHECK_BYTES` of text.
fn walk(xml: &[u8], ctl: Ctl<'_>, mut f: impl FnMut(Node<'_, '_>) -> Flow) -> Res<()> {
    let mut reader = Reader::from_reader(xml);
    {
        let cfg = reader.config_mut();
        cfg.expand_empty_elements = true;
        cfg.check_end_names = true;
    }
    let mut buf = Vec::new();
    let mut depth = 0usize;
    let mut events = 0u32;
    let mut text_bytes = 0usize;
    loop {
        events = events.wrapping_add(1);
        if events.is_multiple_of(512) {
            check(ctl.deadline, ctl.stop)?;
        }
        let event = reader
            .read_event_into(&mut buf)
            .map_err(|_| OfficeError::Corrupt)?;
        let flow = match &event {
            Event::Start(e) => {
                depth += 1;
                if depth > ctl.max_depth {
                    return Err(OfficeError::Limit("kedalaman XML"));
                }
                f(Node::Open(e))
            }
            Event::End(e) => {
                depth = depth.saturating_sub(1);
                f(Node::Close(e.local_name().into_inner().as_bytes()))
            }
            Event::Text(t) => {
                text_bytes += t.len();
                if text_bytes >= TIME_CHECK_BYTES {
                    text_bytes = 0;
                    check(ctl.deadline, ctl.stop)?;
                }
                f(Node::Text(&t.xml10_content()))
            }
            Event::CData(c) => {
                text_bytes += c.len();
                if text_bytes >= TIME_CHECK_BYTES {
                    text_bytes = 0;
                    check(ctl.deadline, ctl.stop)?;
                }
                f(Node::Text(&c.xml10_content()))
            }
            Event::GeneralRef(r) => match resolve_ref(r) {
                Some(ch) => {
                    let mut tmp = [0u8; 4];
                    f(Node::Text(ch.encode_utf8(&mut tmp)))
                }
                None => Flow::Go,
            },
            Event::DocType(_) => return Err(OfficeError::Doctype),
            Event::Eof => return Ok(()),
            _ => Flow::Go,
        };
        if flow == Flow::Stop {
            return Ok(());
        }
        buf.clear();
    }
}

fn local<'a>(e: &'a BytesStart<'_>) -> &'a [u8] {
    e.local_name().into_inner().as_bytes()
}

/// Longest attribute value kept (names, ids, numbers are short; a giant value is cut).
const MAX_ATTR_BYTES: usize = 1024;

/// Attribute by LOCAL name (prefix ignored), predefined entities resolved, at most
/// `MAX_ATTR_BYTES`.
fn attr(e: &BytesStart<'_>, name: &[u8]) -> Option<String> {
    let mut attrs = e.attributes();
    attrs.with_checks(false);
    for a in attrs.flatten() {
        if a.key.local_name().as_ref().as_bytes() == name {
            let value = a.normalized_value(XmlVersion::Implicit1_0).ok()?;
            let mut end = value.len().min(MAX_ATTR_BYTES);
            while !value.is_char_boundary(end) {
                end -= 1;
            }
            return Some(value[..end].to_owned());
        }
    }
    None
}

// ---------------------------------------------------------------------------------------------
// Bounded buffers
// ---------------------------------------------------------------------------------------------

/// One kept item (paragraph, cell, shared string): never grows past the cap it is given.
#[derive(Default)]
struct Item {
    text: String,
    cut: bool,
}

impl Item {
    fn push(&mut self, s: &str, cap: usize) {
        if self.cut {
            return;
        }
        let room = cap.saturating_sub(self.text.len());
        if s.len() <= room {
            self.text.push_str(s);
            return;
        }
        let mut end = room;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        self.text.push_str(&s[..end]);
        self.cut = true;
    }

    fn finish(self) -> String {
        if self.cut {
            let mut t = self.text;
            t.push_str(CUT_MARK);
            t
        } else {
            self.text
        }
    }
}

struct Sink {
    out: String,
    cap: usize,
    item_cap: usize,
    full: bool,
}

impl Sink {
    fn new(cap: usize) -> Self {
        Self::with_item_cap(cap, MAX_ITEM_BYTES)
    }

    fn with_item_cap(cap: usize, item_cap: usize) -> Self {
        Self {
            out: String::new(),
            cap,
            item_cap,
            full: false,
        }
    }

    fn push(&mut self, s: &str) {
        if self.full {
            return;
        }
        let room = self.cap.saturating_sub(self.out.len());
        if s.len() <= room {
            self.out.push_str(s);
            return;
        }
        let mut end = room;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        self.out.push_str(&s[..end]);
        self.full = true;
    }

    fn blank(&mut self) {
        if !self.out.is_empty() && !self.out.ends_with("\n\n") {
            self.push("\n");
        }
    }

    fn line(&mut self, s: &str) {
        let s = s.trim_end();
        if s.trim().is_empty() {
            self.blank();
            return;
        }
        self.push(s);
        self.push("\n");
    }

    fn heading(&mut self, s: &str) {
        self.blank();
        self.line(s);
    }

    fn room(&self) -> usize {
        self.cap.saturating_sub(self.out.len())
    }

    /// Cap for one item kept for this sink: the item limit, and never more than the room left.
    fn item(&self) -> usize {
        self.item_cap.min(self.room() + 1)
    }
}

/// Appends a finished line to a cell, within the cell cap.
fn into_cell(cell: &mut Item, line: &str, cap: usize) {
    let line = line.trim();
    if line.is_empty() {
        return;
    }
    if !cell.text.is_empty() {
        cell.push(" ", cap);
    }
    for (i, part) in line.split(['\n', '\t']).enumerate() {
        if i > 0 {
            cell.push(" ", cap);
        }
        cell.push(part, cap);
    }
}

// ---------------------------------------------------------------------------------------------
// Paragraph/table flow (WordprocessingML and DrawingML share the local names p/t/br/tbl/tr/tc)
// ---------------------------------------------------------------------------------------------

#[derive(Default)]
struct DocxCtx {
    /// styleId -> heading level (1..=6)
    styles: HashMap<String, usize>,
    /// numId -> abstractNumId
    nums: HashMap<String, String>,
    /// (abstractNumId, ilvl) -> bullet?
    levels: HashMap<(String, u32), bool>,
    /// numId -> counters per level
    counters: HashMap<String, Vec<u32>>,
}

/// Entries kept in each style/numbering map (a hostile styles.xml cannot grow them unbounded).
const MAX_DEFS: usize = 10_000;

impl DocxCtx {
    fn heading_level(&self, style: &str) -> Option<usize> {
        if let Some(l) = self.styles.get(style) {
            return Some(*l);
        }
        let lower = style.to_ascii_lowercase();
        if lower == "title" {
            return Some(1);
        }
        lower
            .strip_prefix("heading")
            .and_then(|n| n.parse::<usize>().ok())
            .filter(|n| (1..=9).contains(n))
            .map(|n| n.min(6))
    }

    fn list_prefix(&mut self, num_id: &str, ilvl: u32) -> String {
        let ilvl = ilvl.min(8);
        let indent = "  ".repeat(ilvl as usize);
        let bullet = self
            .nums
            .get(num_id)
            .and_then(|a| self.levels.get(&(a.clone(), ilvl)))
            .copied()
            .unwrap_or(true);
        if bullet {
            return format!("{indent}- ");
        }
        if !self.counters.contains_key(num_id) && self.counters.len() >= MAX_DEFS {
            return format!("{indent}- ");
        }
        let counters = self.counters.entry(num_id.to_owned()).or_default();
        counters.resize(9, 0);
        counters[ilvl as usize] = counters[ilvl as usize].saturating_add(1);
        for c in counters.iter_mut().skip(ilvl as usize + 1) {
            *c = 0;
        }
        format!("{indent}{}. ", counters[ilvl as usize])
    }
}

#[derive(Default)]
struct Para {
    text: Item,
    style: Option<String>,
    num_id: Option<String>,
    ilvl: u32,
}

#[derive(Default)]
struct Table {
    row: Vec<String>,
    /// Cells of this row past `max_cols` (never copied).
    extra_cells: bool,
    cell: Option<Item>,
}

#[derive(Clone, Copy, Default)]
struct FlowOpts {
    /// Skip `fld` text (slide numbers/dates in notes pages).
    skip_fields: bool,
}

/// Delivers a finished line: into the innermost open table cell (bounded), or to the sink.
fn deliver(tables: &mut [Table], sink: &mut Sink, line: &str, heading: bool) {
    let cap = sink.item();
    if let Some(cell) = tables.last_mut().and_then(|t| t.cell.as_mut()) {
        into_cell(cell, line, cap);
        return;
    }
    if heading {
        sink.heading(line);
    } else {
        sink.line(line);
    }
}

/// Paragraphs, headings, lists and tables of one WordprocessingML/DrawingML part.
fn flow_part(
    xml: &[u8],
    ctl: Ctl<'_>,
    limits: &Limits,
    sink: &mut Sink,
    mut docx: Option<&mut DocxCtx>,
    opts: FlowOpts,
) -> Res<()> {
    let mut paras: Vec<Para> = Vec::new();
    let mut tables: Vec<Table> = Vec::new();
    let mut in_t = 0usize;
    let mut ppr = 0usize;
    let mut skip = 0usize;

    walk(xml, ctl, |node| {
        match node {
            Node::Open(e) => {
                if skip > 0 {
                    skip += 1;
                    return Flow::Go;
                }
                match local(e) {
                    b"Fallback" => skip = 1,
                    b"fld" if opts.skip_fields => skip = 1,
                    b"p" => paras.push(Para::default()),
                    b"pPr" => ppr += 1,
                    b"pStyle" if ppr > 0 => {
                        if let Some(p) = paras.last_mut() {
                            p.style = attr(e, b"val");
                        }
                    }
                    b"ilvl" if ppr > 0 => {
                        if let Some(p) = paras.last_mut() {
                            p.ilvl = attr(e, b"val").and_then(|v| v.parse().ok()).unwrap_or(0);
                        }
                    }
                    b"numId" if ppr > 0 => {
                        if let Some(p) = paras.last_mut() {
                            p.num_id = attr(e, b"val");
                        }
                    }
                    b"t" => in_t += 1,
                    b"tab" if ppr == 0 && in_t == 0 => {
                        let cap = sink.item();
                        if let Some(p) = paras.last_mut() {
                            p.text.push("\t", cap);
                        }
                    }
                    b"br" | b"cr" if ppr == 0 => {
                        let cap = sink.item();
                        if let Some(p) = paras.last_mut() {
                            p.text.push("\n", cap);
                        }
                    }
                    b"tbl" => tables.push(Table::default()),
                    b"tc" => {
                        if let Some(t) = tables.last_mut() {
                            if t.row.len() < limits.max_cols {
                                t.cell = Some(Item::default());
                            } else {
                                // O1: a cell past the column limit is never collected.
                                t.extra_cells = true;
                                skip = 1;
                            }
                        }
                    }
                    _ => {}
                }
            }
            Node::Close(name) => {
                if skip > 0 {
                    skip -= 1;
                    return Flow::Go;
                }
                match name {
                    b"t" => in_t = in_t.saturating_sub(1),
                    b"pPr" => ppr = ppr.saturating_sub(1),
                    b"p" => {
                        if let Some(p) = paras.pop() {
                            let text = p.text.finish();
                            let text = text.trim_end();
                            let mut heading = false;
                            let mut line = text.to_owned();
                            if let Some(ctx) = docx.as_deref_mut()
                                && !text.trim().is_empty()
                            {
                                if let Some(level) =
                                    p.style.as_deref().and_then(|s| ctx.heading_level(s))
                                {
                                    heading = true;
                                    line = format!("{} {}", "#".repeat(level), text.trim());
                                } else if let Some(num) = p.num_id.as_deref().filter(|n| *n != "0")
                                {
                                    line =
                                        format!("{}{}", ctx.list_prefix(num, p.ilvl), text.trim());
                                }
                            }
                            deliver(&mut tables, sink, &line, heading);
                        }
                    }
                    b"tc" => {
                        if let Some(t) = tables.last_mut()
                            && let Some(c) = t.cell.take()
                        {
                            t.row.push(c.finish().trim().to_owned());
                        }
                    }
                    b"tr" => {
                        let (row, extra) = tables
                            .last_mut()
                            .map(|t| {
                                (
                                    std::mem::take(&mut t.row),
                                    std::mem::take(&mut t.extra_cells),
                                )
                            })
                            .unwrap_or_default();
                        if row.iter().any(|c| !c.is_empty()) {
                            let more = if extra { " …" } else { "" };
                            let line = format!("| {} |{more}", row.join(" | "));
                            // A nested table goes into the parent cell (bounded there).
                            let n = tables.len();
                            deliver(&mut tables[..n - 1], sink, &line, false);
                        }
                    }
                    b"tbl" => {
                        tables.pop();
                        if tables.is_empty() {
                            sink.blank();
                        }
                    }
                    _ => {}
                }
            }
            Node::Text(s) => {
                if skip == 0 && in_t > 0 {
                    let cap = sink.item();
                    if let Some(p) = paras.last_mut() {
                        p.text.push(s, cap);
                    }
                }
            }
        }
        if sink.full { Flow::Stop } else { Flow::Go }
    })
}

// ---------------------------------------------------------------------------------------------
// DOCX
// ---------------------------------------------------------------------------------------------

fn docx_styles(xml: &[u8], ctl: Ctl<'_>, ctx: &mut DocxCtx) -> Res<()> {
    let mut id: Option<String> = None;
    let mut level: Option<usize> = None;
    walk(xml, ctl, |node| {
        match node {
            Node::Open(e) => match local(e) {
                b"style" => {
                    id = attr(e, b"styleId");
                    level = None;
                }
                b"name" if id.is_some() => {
                    let name = attr(e, b"val").unwrap_or_default().to_ascii_lowercase();
                    if name == "title" {
                        level = Some(1);
                    } else if let Some(n) = name
                        .strip_prefix("heading ")
                        .and_then(|n| n.trim().parse::<usize>().ok())
                        .filter(|n| (1..=9).contains(n))
                    {
                        level = Some(n.min(6));
                    }
                }
                b"outlineLvl" if id.is_some() && level.is_none() => {
                    level = attr(e, b"val")
                        .and_then(|v| v.parse::<usize>().ok())
                        .filter(|n| *n < 9)
                        .map(|n| (n + 1).min(6));
                }
                _ => {}
            },
            Node::Close(b"style") => {
                if let (Some(i), Some(l)) = (id.take(), level.take())
                    && ctx.styles.len() < MAX_DEFS
                {
                    ctx.styles.insert(i, l);
                }
            }
            _ => {}
        }
        Flow::Go
    })
}

fn docx_numbering(xml: &[u8], ctl: Ctl<'_>, ctx: &mut DocxCtx) -> Res<()> {
    let mut abs: Option<String> = None;
    let mut lvl: Option<u32> = None;
    let mut num: Option<String> = None;
    walk(xml, ctl, |node| {
        match node {
            Node::Open(e) => match local(e) {
                b"abstractNum" => abs = attr(e, b"abstractNumId"),
                b"lvl" if abs.is_some() => {
                    lvl = attr(e, b"ilvl").and_then(|v| v.parse().ok());
                }
                b"numFmt" => {
                    if let (Some(a), Some(l)) = (&abs, lvl)
                        && ctx.levels.len() < MAX_DEFS
                    {
                        let bullet = attr(e, b"val").is_some_and(|v| v == "bullet");
                        ctx.levels.insert((a.clone(), l), bullet);
                    }
                }
                b"num" => num = attr(e, b"numId"),
                b"abstractNumId" => {
                    if let (Some(n), Some(a)) = (&num, attr(e, b"val"))
                        && ctx.nums.len() < MAX_DEFS
                    {
                        ctx.nums.insert(n.clone(), a);
                    }
                }
                _ => {}
            },
            Node::Close(name) => match name {
                b"abstractNum" => abs = None,
                b"lvl" => lvl = None,
                b"num" => num = None,
                _ => {}
            },
            Node::Text(_) => {}
        }
        Flow::Go
    })
}

/// One extra part (header, footnotes, ...) rendered on its own, within the remaining room.
fn side_part<R: Read + Seek>(
    pkg: &mut Package<'_, R>,
    name: &str,
    sink: &Sink,
    ctx: Option<&mut DocxCtx>,
    opts: FlowOpts,
) -> Res<Option<String>> {
    let Some(xml) = pkg.part(name)? else {
        return Ok(None);
    };
    let mut part = Sink::with_item_cap(sink.room(), sink.item_cap);
    let limits = pkg.limits;
    flow_part(&xml, pkg.ctl(), &limits, &mut part, ctx, opts)?;
    let text = part.out.trim().to_owned();
    Ok((!text.is_empty()).then_some(text))
}

fn sorted_parts<R: Read + Seek>(pkg: &Package<'_, R>, prefix: &str, suffix: &str) -> Vec<String> {
    let mut parts: Vec<String> = pkg
        .names
        .keys()
        .filter(|n| {
            n.strip_prefix(prefix)
                .and_then(|r| r.strip_suffix(suffix))
                .is_some_and(|mid| mid.chars().all(|c| c.is_ascii_digit()))
        })
        .cloned()
        .collect();
    parts.sort_by_key(|n| natural_key(n));
    parts
}

fn docx<R: Read + Seek>(pkg: &mut Package<'_, R>, sink: &mut Sink) -> Res<()> {
    let ctl = pkg.ctl();
    let limits = pkg.limits;
    let mut ctx = DocxCtx::default();
    if let Some(xml) = pkg.part("word/styles.xml")? {
        docx_styles(&xml, ctl, &mut ctx)?;
    }
    if let Some(xml) = pkg.part("word/numbering.xml")? {
        docx_numbering(&xml, ctl, &mut ctx)?;
    }
    let main = pkg.part("word/document.xml")?.ok_or(OfficeError::Corrupt)?;
    flow_part(
        &main,
        ctl,
        &limits,
        sink,
        Some(&mut ctx),
        FlowOpts::default(),
    )?;
    drop(main);

    for (prefix, title) in [("word/header", "Header"), ("word/footer", "Footer")] {
        let mut seen: Vec<String> = Vec::new();
        for name in sorted_parts(pkg, prefix, ".xml")
            .into_iter()
            .take(limits.max_parts)
        {
            if sink.full {
                return Ok(());
            }
            if let Some(text) = side_part(pkg, &name, sink, None, FlowOpts::default())?
                && !seen.contains(&text)
            {
                seen.push(text);
            }
        }
        if !seen.is_empty() {
            sink.heading(&format!("## {title}"));
            for text in seen {
                sink.line(&text);
            }
        }
    }
    for (name, title) in [
        ("word/footnotes.xml", "Catatan kaki"),
        ("word/endnotes.xml", "Catatan akhir"),
        ("word/comments.xml", "Komentar"),
    ] {
        if sink.full {
            return Ok(());
        }
        if let Some(text) = side_part(pkg, name, sink, Some(&mut ctx), FlowOpts::default())? {
            sink.heading(&format!("## {title}"));
            sink.line(&text);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Relationships
// ---------------------------------------------------------------------------------------------

/// Relationship id -> (type, resolved part name). External targets are dropped.
fn relationships(
    xml: &[u8],
    ctl: Ctl<'_>,
    base_dir: &str,
) -> Res<HashMap<String, (String, String)>> {
    let mut out = HashMap::new();
    walk(xml, ctl, |node| {
        if let Node::Open(e) = node
            && local(e) == b"Relationship"
            && out.len() < MAX_DEFS
            && !attr(e, b"TargetMode").is_some_and(|m| m.eq_ignore_ascii_case("external"))
            && let (Some(id), Some(target)) = (attr(e, b"Id"), attr(e, b"Target"))
        {
            let kind = attr(e, b"Type").unwrap_or_default();
            out.insert(id, (kind, resolve_target(base_dir, &target)));
        }
        Flow::Go
    })?;
    Ok(out)
}

/// Part name of a relationship target, inside the package only (`..` cannot climb above root).
fn resolve_target(base_dir: &str, target: &str) -> String {
    let joined = match target.strip_prefix('/') {
        Some(abs) => abs.to_owned(),
        None => format!("{base_dir}{target}"),
    };
    let mut parts: Vec<&str> = Vec::new();
    for seg in joined.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

// ---------------------------------------------------------------------------------------------
// XLSX
// ---------------------------------------------------------------------------------------------

/// Shared strings, each capped at `max_item_bytes` and stored ONCE (cells reference them).
fn shared_strings(xml: &[u8], ctl: Ctl<'_>, limits: &Limits) -> Res<Vec<Rc<str>>> {
    let empty: Rc<str> = Rc::from("");
    let mut out: Vec<Rc<str>> = Vec::new();
    let mut cur: Option<Item> = None;
    let mut in_t = false;
    let mut rph = 0usize;
    let cap = limits.max_item_bytes;
    walk(xml, ctl, |node| {
        match node {
            Node::Open(e) => match local(e) {
                b"si" => cur = Some(Item::default()),
                b"rPh" => rph += 1,
                b"t" => in_t = true,
                _ => {}
            },
            Node::Close(name) => match name {
                b"si" => {
                    let s = cur.take().map(Item::finish).unwrap_or_default();
                    out.push(if s.is_empty() {
                        empty.clone()
                    } else {
                        Rc::from(s)
                    });
                    if out.len() >= limits.max_shared {
                        return Flow::Stop;
                    }
                }
                b"rPh" => rph = rph.saturating_sub(1),
                b"t" => in_t = false,
                _ => {}
            },
            Node::Text(s) => {
                if in_t
                    && rph == 0
                    && let Some(c) = cur.as_mut()
                {
                    c.push(s, cap);
                }
            }
        }
        Flow::Go
    })?;
    Ok(out)
}

/// 0-based column of a cell reference (`AB12` -> 27); `None` when it has no letters.
fn column_of(reference: &str) -> Option<usize> {
    let mut col: usize = 0;
    let mut any = false;
    for c in reference.chars() {
        if !c.is_ascii_alphabetic() {
            break;
        }
        any = true;
        col = col
            .saturating_mul(26)
            .saturating_add((c.to_ascii_uppercase() as u8 - b'A' + 1) as usize);
    }
    any.then(|| col - 1)
}

fn cell_clean(s: &str) -> String {
    s.replace(['\t', '\r', '\n'], " ").trim().to_owned()
}

fn rows_cut_notes(sink: &mut Sink, limits: &Limits, rows_cut: bool, cols_cut: bool) {
    if rows_cut {
        sink.line(&format!(
            "[dipotong: hanya {} baris pertama]",
            limits.max_rows
        ));
    }
    if cols_cut {
        sink.line(&format!(
            "[kolom setelah kolom ke-{} tidak disertakan]",
            limits.max_cols
        ));
    }
}

struct XCell {
    col: usize,
    kind: Option<String>,
    raw: Item,
    /// Past the column limit or in a row past the row limit: nothing is copied.
    ignored: bool,
    ignored_content: bool,
}

fn sheet_tsv(
    xml: &[u8],
    shared: &[Rc<str>],
    limits: &Limits,
    ctl: Ctl<'_>,
    sink: &mut Sink,
) -> Res<()> {
    let mut row: Vec<(usize, String)> = Vec::new();
    let mut row_over = false;
    let mut row_over_content = false;
    let mut next_col = 0usize;
    let mut cell: Option<XCell> = None;
    let mut in_v = false;
    let mut in_t = false;
    let mut rph = 0usize;
    let mut rows_out = 0usize;
    let mut rows_cut = false;
    let mut cols_cut = false;
    walk(xml, ctl, |node| {
        match node {
            Node::Open(e) => match local(e) {
                b"row" => {
                    row.clear();
                    next_col = 0;
                    // O1: rows past the limit are never copied.
                    row_over = rows_out >= limits.max_rows;
                    row_over_content = false;
                }
                b"c" => {
                    let col = attr(e, b"r")
                        .and_then(|r| column_of(&r))
                        .unwrap_or(next_col);
                    next_col = col.saturating_add(1);
                    // O1: the column limit is checked BEFORE anything is copied.
                    let ignored = row_over || col >= limits.max_cols;
                    cell = Some(XCell {
                        col,
                        kind: if ignored { None } else { attr(e, b"t") },
                        raw: Item::default(),
                        ignored,
                        ignored_content: false,
                    });
                }
                b"v" => in_v = true,
                b"t" => in_t = true,
                b"rPh" => rph += 1,
                _ => {}
            },
            Node::Close(name) => match name {
                b"v" => in_v = false,
                b"t" => in_t = false,
                b"rPh" => rph = rph.saturating_sub(1),
                b"c" => {
                    if let Some(c) = cell.take() {
                        if c.ignored {
                            if c.ignored_content {
                                if row_over {
                                    row_over_content = true;
                                } else {
                                    cols_cut = true;
                                }
                            }
                            return Flow::Go;
                        }
                        let raw = c.raw.finish();
                        let value = match c.kind.as_deref() {
                            // Referenced, not copied: only the cleaned (<= item cap) text is new.
                            Some("s") => raw
                                .trim()
                                .parse::<usize>()
                                .ok()
                                .and_then(|i| shared.get(i))
                                .map(|s| cell_clean(s))
                                .unwrap_or_default(),
                            Some("b") => match raw.trim() {
                                "1" => "TRUE".to_owned(),
                                "0" => "FALSE".to_owned(),
                                other => other.to_owned(),
                            },
                            _ => cell_clean(&raw),
                        };
                        if !value.is_empty() {
                            row.push((c.col, value));
                        }
                    }
                }
                b"row" => {
                    if row_over && row_over_content {
                        rows_cut = true;
                        return Flow::Stop;
                    }
                    if !row.is_empty() {
                        rows_out += 1;
                        let width = row.iter().map(|(c, _)| c + 1).max().unwrap_or(0);
                        let mut cells = vec![String::new(); width];
                        for (c, v) in row.drain(..) {
                            cells[c] = v;
                        }
                        sink.line(&cells.join("\t"));
                    }
                }
                _ => {}
            },
            Node::Text(s) => {
                if (in_v || in_t)
                    && rph == 0
                    && let Some(c) = cell.as_mut()
                {
                    if c.ignored {
                        c.ignored_content |= !s.trim().is_empty();
                    } else {
                        c.raw.push(s, limits.max_item_bytes);
                    }
                }
            }
        }
        if sink.full { Flow::Stop } else { Flow::Go }
    })?;
    rows_cut_notes(sink, limits, rows_cut, cols_cut);
    Ok(())
}

fn xlsx<R: Read + Seek>(pkg: &mut Package<'_, R>, sink: &mut Sink) -> Res<()> {
    let ctl = pkg.ctl();
    let limits = pkg.limits;
    let shared = match pkg.part("xl/sharedStrings.xml")? {
        Some(xml) => shared_strings(&xml, ctl, &limits)?,
        None => Vec::new(),
    };
    let workbook = pkg.part("xl/workbook.xml")?.ok_or(OfficeError::Corrupt)?;
    let mut sheets: Vec<(String, Option<String>, bool)> = Vec::new();
    walk(&workbook, ctl, |node| {
        if let Node::Open(e) = node
            && local(e) == b"sheet"
            && sheets.len() < limits.max_parts
        {
            let hidden = attr(e, b"state").is_some_and(|s| s != "visible");
            sheets.push((attr(e, b"name").unwrap_or_default(), attr(e, b"id"), hidden));
        }
        Flow::Go
    })?;
    drop(workbook);
    let rels = match pkg.part("xl/_rels/workbook.xml.rels")? {
        Some(xml) => relationships(&xml, ctl, "xl/")?,
        None => HashMap::new(),
    };
    for (i, (name, rid, hidden)) in sheets.into_iter().enumerate() {
        if sink.full {
            break;
        }
        let target = rid
            .and_then(|r| rels.get(&r).map(|(_, t)| t.clone()))
            .unwrap_or_else(|| format!("xl/worksheets/sheet{}.xml", i + 1));
        if !target.to_ascii_lowercase().starts_with("xl/") || !pkg.has(&target) {
            continue;
        }
        let Some(xml) = pkg.part(&target)? else {
            continue;
        };
        let name = cell_clean(&name);
        sink.heading(&format!(
            "## Sheet: {name}{}",
            if hidden { " (tersembunyi)" } else { "" }
        ));
        sheet_tsv(&xml, &shared, &limits, ctl, sink)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// PPTX
// ---------------------------------------------------------------------------------------------

fn pptx<R: Read + Seek>(pkg: &mut Package<'_, R>, sink: &mut Sink) -> Res<()> {
    let ctl = pkg.ctl();
    let limits = pkg.limits;
    let slides = sorted_parts(pkg, "ppt/slides/slide", ".xml");
    for name in slides.into_iter().take(limits.max_parts) {
        if sink.full {
            break;
        }
        let number = natural_key(&name).1;
        let Some(xml) = pkg.part(&name)? else {
            continue;
        };
        sink.heading(&format!("## Slide {number}"));
        flow_part(&xml, ctl, &limits, sink, None, FlowOpts::default())?;
        drop(xml);
        let rels_name = format!("ppt/slides/_rels/slide{number}.xml.rels");
        let Some(rels) = pkg.part(&rels_name)? else {
            continue;
        };
        let notes: Vec<String> = relationships(&rels, ctl, "ppt/slides/")?
            .into_values()
            .filter(|(kind, target)| {
                kind.ends_with("/notesSlide") && target.starts_with("ppt/notesSlides/")
            })
            .map(|(_, target)| target)
            .collect();
        for target in notes.into_iter().take(1) {
            let opts = FlowOpts { skip_fields: true };
            if let Some(text) = side_part(pkg, &target, sink, None, opts)? {
                sink.line("Catatan pembicara:");
                sink.line(&text);
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// OpenDocument (content.xml)
// ---------------------------------------------------------------------------------------------

#[derive(Default)]
struct OdfTable {
    row: Vec<String>,
    row_repeat: usize,
    cell: Option<Item>,
    cell_repeat: usize,
    rows_out: usize,
    rows_cut: bool,
    cols_cut: bool,
}

fn repeat_attr(e: &BytesStart<'_>, name: &[u8]) -> usize {
    attr(e, name)
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(1)
        .max(1)
}

fn odf<R: Read + Seek>(pkg: &mut Package<'_, R>, sink: &mut Sink, format: Format) -> Res<()> {
    let ctl = pkg.ctl();
    let limits = pkg.limits;
    if let Some(manifest) = pkg.part("META-INF/manifest.xml")?
        && manifest
            .windows(b"encryption-data".len())
            .any(|w| w == b"encryption-data")
    {
        return Err(OfficeError::Encrypted);
    }
    let content = pkg.part("content.xml")?.ok_or(OfficeError::Corrupt)?;
    let sheet = format == Format::Ods;
    let mut paras: Vec<(Option<usize>, Item)> = Vec::new();
    let mut lists = 0usize;
    let mut tables: Vec<OdfTable> = Vec::new();
    let mut skip = 0usize;
    let mut slide = 0usize;

    walk(&content, ctl, |node| {
        let cap = sink.item();
        match node {
            Node::Open(e) => {
                if skip > 0 {
                    skip += 1;
                    return Flow::Go;
                }
                match local(e) {
                    b"annotation" | b"tracked-changes" | b"notes" | b"note-citation" => skip = 1,
                    b"h" => {
                        let level = attr(e, b"outline-level")
                            .and_then(|v| v.parse::<usize>().ok())
                            .unwrap_or(1)
                            .clamp(1, 6);
                        paras.push((Some(level), Item::default()));
                    }
                    b"p" => paras.push((None, Item::default())),
                    b"s" => {
                        if let Some((_, t)) = paras.last_mut() {
                            let n = attr(e, b"c")
                                .and_then(|v| v.parse::<usize>().ok())
                                .unwrap_or(1)
                                .min(64);
                            t.push(&" ".repeat(n), cap);
                        }
                    }
                    b"tab" => {
                        if let Some((_, t)) = paras.last_mut() {
                            t.push("\t", cap);
                        }
                    }
                    b"line-break" => {
                        if let Some((_, t)) = paras.last_mut() {
                            t.push("\n", cap);
                        }
                    }
                    b"list" => lists += 1,
                    b"table" => {
                        if sheet {
                            let name = cell_clean(&attr(e, b"name").unwrap_or_default());
                            sink.heading(&format!("## Sheet: {name}"));
                        }
                        tables.push(OdfTable::default());
                    }
                    b"table-row" => {
                        if let Some(t) = tables.last_mut() {
                            t.row.clear();
                            t.row_repeat = repeat_attr(e, b"number-rows-repeated");
                        }
                    }
                    b"table-cell" | b"covered-table-cell" => {
                        if let Some(t) = tables.last_mut() {
                            if t.row.len() < limits.max_cols && t.rows_out < limits.max_rows {
                                t.cell = Some(Item::default());
                                t.cell_repeat = repeat_attr(e, b"number-columns-repeated");
                            } else {
                                // O1: past the column/row limit: never collected.
                                if t.rows_out >= limits.max_rows {
                                    t.rows_cut = true;
                                }
                                skip = 1;
                            }
                        }
                    }
                    b"page" if format == Format::Odp => {
                        slide += 1;
                        sink.heading(&format!("## Slide {slide}"));
                    }
                    _ => {}
                }
            }
            Node::Close(name) => {
                if skip > 0 {
                    skip -= 1;
                    return Flow::Go;
                }
                match name {
                    b"h" | b"p" => {
                        if let Some((level, text)) = paras.pop() {
                            let text = text.finish();
                            let text = text.trim_end();
                            let line = match level {
                                Some(l) if !text.trim().is_empty() => {
                                    format!("{} {}", "#".repeat(l), text.trim())
                                }
                                _ if lists > 0 && !text.trim().is_empty() => {
                                    format!("{}- {}", "  ".repeat((lists - 1).min(8)), text.trim())
                                }
                                _ => text.to_owned(),
                            };
                            if let Some(cell) = tables.last_mut().and_then(|t| t.cell.as_mut()) {
                                into_cell(cell, &line, cap);
                            } else if let Some((_, p)) = paras.last_mut() {
                                // Paragraph nested in a paragraph (frame/text box): keep its text.
                                p.push(" ", cap);
                                p.push(line.trim(), cap);
                            } else if level.is_some() {
                                sink.heading(&line);
                            } else {
                                sink.line(&line);
                            }
                        }
                    }
                    b"list" => lists = lists.saturating_sub(1),
                    b"table-cell" | b"covered-table-cell" => {
                        if let Some(t) = tables.last_mut()
                            && let Some(c) = t.cell.take()
                        {
                            let c = c.finish().trim().to_owned();
                            // O1: the repeat is bounded by the columns left (<= max_cols copies of
                            // a <= item-cap cell).
                            let room = limits.max_cols.saturating_sub(t.row.len());
                            if t.cell_repeat > room && !c.is_empty() {
                                t.cols_cut = true;
                            }
                            for _ in 0..t.cell_repeat.min(room) {
                                t.row.push(c.clone());
                            }
                        }
                    }
                    b"table-row" => {
                        if let Some(t) = tables.last_mut() {
                            while t.row.last().is_some_and(String::is_empty) {
                                t.row.pop();
                            }
                            if !t.row.is_empty() {
                                let line = if sheet {
                                    t.row.join("\t")
                                } else {
                                    format!("| {} |", t.row.join(" | "))
                                };
                                // O1: bounded by the rows left and by the output.
                                for _ in 0..t.row_repeat {
                                    if t.rows_out >= limits.max_rows {
                                        t.rows_cut = true;
                                        break;
                                    }
                                    if sink.full {
                                        break;
                                    }
                                    t.rows_out += 1;
                                    sink.line(&line);
                                }
                            }
                            t.row.clear();
                        }
                    }
                    b"table" => {
                        if let Some(t) = tables.pop() {
                            rows_cut_notes(sink, &limits, t.rows_cut, t.cols_cut);
                        }
                        sink.blank();
                    }
                    _ => {}
                }
            }
            Node::Text(s) => {
                if skip == 0
                    && let Some((_, t)) = paras.last_mut()
                {
                    t.push(s, cap);
                }
            }
        }
        if sink.full { Flow::Stop } else { Flow::Go }
    })
}

#[cfg(test)]
#[path = "office_text_tests.rs"]
mod tests;
