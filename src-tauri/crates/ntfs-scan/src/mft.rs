//! Read-only NTFS Master File Table parser (FER-460).
//!
//! Pure Rust over a [`VolumeReader`] (raw volume bytes at an offset), so the
//! whole pipeline is testable on a synthetic in-memory image. It reads ONLY
//! MFT records — never file payload — and yields one [`WireEntry`] per
//! (record, name) pair with the sizes from the unnamed `$DATA` attribute.
//!
//! Why raw MFT and not `FSCTL_ENUM_USN_DATA`: the USN enumeration returns
//! names + parent references but no sizes, and only the primary name of a
//! hardlinked file. Getting sizes would need one `FSCTL_GET_NTFS_FILE_RECORD`
//! (or one open) per file, i.e. millions of syscalls — the very cost this
//! feature removes. Sequentially reading the MFT gives names, ALL hardlink
//! names, sizes and reparse tags in one pass (see docs/disk-usage-contract.md §7).
//!
//! Semantics mirror the regular walker so both engines agree:
//! - logical = unnamed `$DATA` data size (what `st_len` reports); alternate
//!   data streams are ignored, exactly like the walker.
//! - allocated = what `GetCompressedFileSizeW` reports: the data size for an
//!   ordinary file, the compressed/allocated cluster total for a compressed
//!   or sparse stream, and the `WofCompressedData` stream for WOF-compressed
//!   (CompactOS) files.
//! - name-surrogate reparse points (symlink, junction, mount point) are
//!   `Symlink` entries: counted, never followed (D07). Other reparse points
//!   (cloud placeholders, dedup, WOF) stay files/directories.
//! - DOS 8.3 names are skipped; every other name of a record is an entry
//!   (hardlink footprint semantics, contract §2.1).
//! - records 0..=15 (metafiles: `$MFT`, `$Bitmap`, `$Extend`, …) are never
//!   emitted; the regular walker cannot see them either.

use crate::protocol::{EntryKind, WireEntry};
use std::collections::{HashMap, HashSet};
use std::io;

pub const ROOT_RECORD: u64 = 5;
/// Records below this are NTFS metafiles (never emitted).
pub const FIRST_USER_RECORD: u64 = 16;
const REF_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;
/// NTFS update-sequence stride (fixed by the on-disk format).
const USA_STRIDE: usize = 512;

const ATTR_STANDARD_INFORMATION: u32 = 0x10;
const ATTR_ATTRIBUTE_LIST: u32 = 0x20;
const ATTR_FILE_NAME: u32 = 0x30;
const ATTR_DATA: u32 = 0x80;
const ATTR_REPARSE_POINT: u32 = 0xC0;
const ATTR_END: u32 = 0xFFFF_FFFF;

const ATTR_FLAG_COMPRESSED: u16 = 0x0001;
const ATTR_FLAG_SPARSE: u16 = 0x8000;

const RECORD_IN_USE: u16 = 0x0001;
const RECORD_IS_DIRECTORY: u16 = 0x0002;

const NAMESPACE_DOS: u8 = 2;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
/// Reparse tags with this bit are name surrogates (links) — Rust's
/// `FileType::is_symlink` uses the same rule, which the walker relies on.
const REPARSE_NAME_SURROGATE: u32 = 0x2000_0000;
pub const IO_REPARSE_TAG_WOF: u32 = 0x8000_0017;

/// Records read per I/O (the chunk is `RECORDS_PER_CHUNK * record size`).
const RECORDS_PER_CHUNK: u64 = 4096;
/// FER-463 L4: most `$MFT` extension records the bootstrap follows. Real
/// volumes need a handful even when the MFT is badly fragmented; a crafted
/// attribute list (up to 16 MiB, ~645k entries) must not make the elevated
/// helper do quadratic work.
pub const MAX_MFT_EXTENSIONS: usize = 4096;
/// FER-463 L4: most records held back during a scan (base records with an
/// attribute list plus extensions seen before their base). Real volumes keep
/// a few thousand; a crafted volume where every record is an orphan
/// extension must hit a clear error instead of exhausting memory.
pub const MAX_HELD_RECORDS: usize = 1_000_000;

/// Random-access read of the raw volume.
pub trait VolumeReader {
    /// Fill `buf` from absolute volume byte `offset`. Offsets and lengths
    /// passed by this parser are always multiples of 512.
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()>;
}

/// The subset of `NTFS_VOLUME_DATA_BUFFER` the parser needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VolumeGeometry {
    pub bytes_per_sector: u32,
    pub bytes_per_cluster: u32,
    pub bytes_per_record: u32,
    pub mft_start_lcn: u64,
    pub mft_valid_data_length: u64,
    /// Clusters on the volume (`NTFS_VOLUME_DATA_BUFFER.TotalClusters`):
    /// every MFT run must lie inside it.
    pub total_clusters: u64,
}

impl VolumeGeometry {
    pub fn validate(&self) -> Result<(), MftError> {
        let pow2 = |v: u32, lo: u32, hi: u32| v.is_power_of_two() && (lo..=hi).contains(&v);
        if !pow2(self.bytes_per_sector, 512, 4096)
            || !pow2(self.bytes_per_cluster, 512, 2 * 1024 * 1024)
            || !pow2(self.bytes_per_record, 512, 65536)
            || self.bytes_per_cluster < self.bytes_per_sector
            || self.mft_start_lcn >= self.total_clusters
            || self
                .total_clusters
                .checked_mul(self.bytes_per_cluster as u64)
                .is_none()
        {
            return Err(MftError::Layout("implausible volume geometry"));
        }
        Ok(())
    }
}

#[derive(Debug)]
pub enum MftError {
    Io(io::Error),
    /// The MFT itself could not be located/decoded.
    Layout(&'static str),
    Cancelled,
    /// The sink refused more data (pipe closed).
    Sink(io::Error),
}

impl std::fmt::Display for MftError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "volume read failed: {e}"),
            Self::Layout(why) => write!(f, "unexpected MFT layout: {why}"),
            Self::Cancelled => write!(f, "cancelled"),
            Self::Sink(e) => write!(f, "output closed: {e}"),
        }
    }
}

impl From<io::Error> for MftError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// What the scan reports to its caller.
pub enum ScanEvent {
    Entry(WireEntry),
    Progress { done: u64, total: u64 },
}

/// Per-record metadata from `$STANDARD_INFORMATION` that the pipe protocol
/// does not carry (FER-475: the FerrumIndexer service keeps it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RecordMeta {
    /// Last-write ("altered") time, Windows FILETIME; 0 = unknown.
    pub modified: u64,
    /// `FILE_ATTRIBUTE_*` flags; 0 when `$STANDARD_INFORMATION` is absent.
    pub attributes: u32,
}

/// [`ScanEvent`] plus the record's [`RecordMeta`] ([`scan_detailed`]).
pub enum DetailedEvent {
    Entry(WireEntry, RecordMeta),
    Progress { done: u64, total: u64 },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanSummary {
    pub root_frn: u64,
    pub records_scanned: u64,
    pub corrupt_records: u64,
}

// ── little-endian field access with bounds checks ────────────────────────

fn rd_u8(b: &[u8], off: usize) -> Option<u8> {
    b.get(off).copied()
}
fn rd_u16(b: &[u8], off: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        b.get(off..off.checked_add(2)?)?.try_into().ok()?,
    ))
}
fn rd_u32(b: &[u8], off: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        b.get(off..off.checked_add(4)?)?.try_into().ok()?,
    ))
}
fn rd_u64(b: &[u8], off: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        b.get(off..off.checked_add(8)?)?.try_into().ok()?,
    ))
}

/// Apply the update-sequence fixups in place. `false` = torn/corrupt record.
pub fn apply_fixups(rec: &mut [u8]) -> bool {
    let (Some(usa_off), Some(usa_count)) = (rd_u16(rec, 4), rd_u16(rec, 6)) else {
        return false;
    };
    let (usa_off, usa_count) = (usa_off as usize, usa_count as usize);
    let sectors = rec.len() / USA_STRIDE;
    if usa_count != sectors + 1 || usa_off + usa_count * 2 > rec.len() || usa_off < 0x28 {
        return false;
    }
    let usn = [rec[usa_off], rec[usa_off + 1]];
    // Verify every sector BEFORE changing any (FER-475): a failed check
    // leaves the record untouched, so a caller may still use it as-is.
    if (0..sectors).any(|i| {
        let end = (i + 1) * USA_STRIDE - 2;
        rec[end..end + 2] != usn
    }) {
        return false;
    }
    for i in 0..sectors {
        let end = (i + 1) * USA_STRIDE - 2;
        let src = usa_off + 2 + i * 2;
        let (a, b) = (rec[src], rec[src + 1]);
        rec[end] = a;
        rec[end + 1] = b;
    }
    true
}

/// One run of the `$DATA` mapping: `clusters` clusters starting at virtual
/// cluster `vcn`, stored at `lcn` (`None` = sparse).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Run {
    pub vcn: u64,
    pub clusters: u64,
    pub lcn: Option<u64>,
}

/// Decode NTFS mapping pairs starting at `first_vcn`.
pub fn decode_runs(pairs: &[u8], first_vcn: u64) -> Option<Vec<Run>> {
    let mut runs = Vec::new();
    let mut pos = 0usize;
    let mut vcn = first_vcn;
    let mut lcn: i64 = 0;
    loop {
        let header = *pairs.get(pos)?;
        if header == 0 {
            break;
        }
        pos += 1;
        let len_size = (header & 0x0F) as usize;
        let off_size = (header >> 4) as usize;
        if len_size == 0 || len_size > 8 || off_size > 8 {
            return None;
        }
        let len_bytes = pairs.get(pos..pos + len_size)?;
        pos += len_size;
        let mut clusters: u64 = 0;
        for (i, b) in len_bytes.iter().enumerate() {
            clusters |= (*b as u64) << (8 * i);
        }
        if clusters == 0 {
            return None;
        }
        let run_lcn = if off_size == 0 {
            None
        } else {
            let off_bytes = pairs.get(pos..pos + off_size)?;
            pos += off_size;
            let mut delta: i64 = 0;
            for (i, b) in off_bytes.iter().enumerate() {
                delta |= (*b as i64) << (8 * i);
            }
            // Sign-extend from off_size bytes.
            let shift = 64 - 8 * off_size as u32;
            delta = (delta << shift) >> shift;
            lcn = lcn.checked_add(delta)?;
            if lcn < 0 {
                return None;
            }
            Some(lcn as u64)
        };
        runs.push(Run {
            vcn,
            clusters,
            lcn: run_lcn,
        });
        vcn = vcn.checked_add(clusters)?;
    }
    Some(runs)
}

/// Sizes of one data stream instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct StreamSizes {
    logical: u64,
    allocated: u64,
}

#[derive(Debug, Clone, Default)]
struct NameRec {
    parent: u64,
    units: Vec<u16>,
}

/// Everything one record (base or extension) contributes.
#[derive(Debug, Clone, Default)]
struct RecordInfo {
    seq: u16,
    base_ref: u64,
    is_dir: bool,
    has_attr_list: bool,
    names: Vec<NameRec>,
    /// Unnamed `$DATA`, from the instance with lowest VCN 0.
    data: Option<StreamSizes>,
    /// `WofCompressedData` named stream (CompactOS).
    wof: Option<StreamSizes>,
    reparse_tag: Option<u32>,
    /// `$STANDARD_INFORMATION` (FER-475): last write + attributes.
    std_info: Option<RecordMeta>,
    /// Unnamed `$DATA` runs (only collected for record 0 bootstrap).
    data_runs: Vec<Run>,
    /// Attribute list value (only collected for record 0 bootstrap).
    attr_list: Option<AttrListValue>,
}

#[derive(Debug, Clone)]
enum AttrListValue {
    Resident(Vec<u8>),
    NonResident { runs: Vec<Run>, size: u64 },
}

impl RecordInfo {
    fn merge(&mut self, ext: RecordInfo) {
        self.names.extend(ext.names);
        if self.data.is_none() {
            self.data = ext.data;
        }
        if self.wof.is_none() {
            self.wof = ext.wof;
        }
        if self.reparse_tag.is_none() {
            self.reparse_tag = ext.reparse_tag;
        }
        if self.std_info.is_none() {
            self.std_info = ext.std_info;
        }
        self.data_runs.extend(ext.data_runs);
    }
}

fn stream_sizes(attr: &[u8], non_resident: bool, flags: u16) -> Option<(StreamSizes, u64)> {
    if !non_resident {
        let len = rd_u32(attr, 0x10)? as u64;
        return Some((
            StreamSizes {
                logical: len,
                allocated: len,
            },
            0,
        ));
    }
    let lowest_vcn = rd_u64(attr, 0x10)?;
    let allocated = rd_u64(attr, 0x28)?;
    let data_size = rd_u64(attr, 0x30)?;
    let on_disk = if flags & (ATTR_FLAG_COMPRESSED | ATTR_FLAG_SPARSE) != 0 {
        // Compressed/sparse streams carry the really-allocated total at
        // 0x40 (the header is 0x48 long in that case).
        rd_u64(attr, 0x40).unwrap_or(allocated)
    } else {
        data_size
    };
    Some((
        StreamSizes {
            logical: data_size,
            allocated: on_disk,
        },
        lowest_vcn,
    ))
}

fn attr_name(attr: &[u8]) -> Option<Vec<u16>> {
    let n = rd_u8(attr, 9)? as usize;
    if n == 0 {
        return Some(Vec::new());
    }
    let off = rd_u16(attr, 10)? as usize;
    let raw = attr.get(off..off + n * 2)?;
    Some(crate::protocol::le_units(raw).collect())
}

fn wof_stream_name() -> Vec<u16> {
    "WofCompressedData".encode_utf16().collect()
}

/// Parse one fixed-up FILE record. `None` = not a usable record (free,
/// never used, or structurally invalid → `Err(())` counted as corrupt).
/// What [`parse_record`] collects beyond the entry fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Want {
    /// Full scan: entry fields only.
    Scan,
    /// Live re-read (FER-475): also the attribute list value.
    Live,
    /// Record 0: attribute list + unnamed `$DATA` runs.
    Bootstrap,
}

fn parse_record(rec: &[u8], want: Want) -> Result<Option<RecordInfo>, ()> {
    let bootstrap = want == Want::Bootstrap;
    if rec.get(0..4) != Some(b"FILE") {
        return Ok(None); // never-used / zeroed slot ("BAAD" too: skip)
    }
    let flags = rd_u16(rec, 0x16).ok_or(())?;
    if flags & RECORD_IN_USE == 0 {
        return Ok(None);
    }
    let seq = rd_u16(rec, 0x10).ok_or(())?;
    let first_attr = rd_u16(rec, 0x14).ok_or(())? as usize;
    let used = rd_u32(rec, 0x18).ok_or(())? as usize;
    let base_ref = rd_u64(rec, 0x20).ok_or(())? & REF_MASK;
    if used > rec.len() || first_attr < 0x30 || first_attr >= used {
        return Err(());
    }
    let mut info = RecordInfo {
        seq,
        base_ref,
        is_dir: flags & RECORD_IS_DIRECTORY != 0,
        ..Default::default()
    };
    let wof_name = wof_stream_name();
    let mut pos = first_attr;
    loop {
        let ty = rd_u32(rec, pos).ok_or(())?;
        if ty == ATTR_END {
            break;
        }
        let len = rd_u32(rec, pos + 4).ok_or(())? as usize;
        if len < 0x18 || pos + len > used || !len.is_multiple_of(8) {
            return Err(());
        }
        let attr = &rec[pos..pos + len];
        let non_resident = rd_u8(attr, 8).ok_or(())? != 0;
        let aflags = rd_u16(attr, 0x0C).ok_or(())?;
        let resident_value = || -> Option<&[u8]> {
            let vlen = rd_u32(attr, 0x10)? as usize;
            let voff = rd_u16(attr, 0x14)? as usize;
            attr.get(voff..voff.checked_add(vlen)?)
        };
        match ty {
            ATTR_ATTRIBUTE_LIST => {
                info.has_attr_list = true;
                if want != Want::Scan {
                    info.attr_list = if non_resident {
                        let pairs_off = rd_u16(attr, 0x20).ok_or(())? as usize;
                        let size = rd_u64(attr, 0x30).ok_or(())?;
                        let runs = decode_runs(attr.get(pairs_off..).ok_or(())?, 0).ok_or(())?;
                        Some(AttrListValue::NonResident { runs, size })
                    } else {
                        Some(AttrListValue::Resident(
                            resident_value().ok_or(())?.to_vec(),
                        ))
                    };
                }
            }
            ATTR_STANDARD_INFORMATION if !non_resident => {
                // FER-475: altered (last write) time at 0x08, file
                // attributes at 0x20 — not `$FILE_NAME`'s copies, which are
                // only refreshed on rename. A value too short for both is
                // ignored (unknown), never a structural error.
                if info.std_info.is_none()
                    && let Some(v) = resident_value()
                    && let (Some(modified), Some(attributes)) = (rd_u64(v, 0x08), rd_u32(v, 0x20))
                {
                    info.std_info = Some(RecordMeta {
                        modified,
                        attributes,
                    });
                }
            }
            ATTR_FILE_NAME if !non_resident => {
                let v = resident_value().ok_or(())?;
                let parent = rd_u64(v, 0).ok_or(())?;
                let n = rd_u8(v, 0x40).ok_or(())? as usize;
                let ns = rd_u8(v, 0x41).ok_or(())?;
                let raw = v.get(0x42..0x42 + n * 2).ok_or(())?;
                if ns != NAMESPACE_DOS && n > 0 {
                    info.names.push(NameRec {
                        parent,
                        units: crate::protocol::le_units(raw).collect(),
                    });
                }
                if info.reparse_tag.is_none() {
                    let fattrs = rd_u32(v, 0x38).ok_or(())?;
                    if fattrs & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                        info.reparse_tag = rd_u32(v, 0x3C).filter(|t| *t != 0);
                    }
                }
            }
            ATTR_DATA => {
                let name = attr_name(attr).ok_or(())?;
                let (sizes, lowest_vcn) = stream_sizes(attr, non_resident, aflags).ok_or(())?;
                if name.is_empty() {
                    if lowest_vcn == 0 {
                        info.data = Some(sizes);
                    }
                    if bootstrap && non_resident {
                        let pairs_off = rd_u16(attr, 0x20).ok_or(())? as usize;
                        let runs =
                            decode_runs(attr.get(pairs_off..).ok_or(())?, lowest_vcn).ok_or(())?;
                        info.data_runs.extend(runs);
                    }
                } else if name == wof_name && lowest_vcn == 0 {
                    info.wof = Some(sizes);
                }
            }
            ATTR_REPARSE_POINT => {
                if !non_resident && let Some(tag) = resident_value().and_then(|v| rd_u32(v, 0)) {
                    info.reparse_tag = Some(tag);
                }
            }
            _ => {}
        }
        pos += len;
    }
    Ok(Some(info))
}

/// Translate a finished base record into wire entries. Returns how many of
/// its names were REJECTED (invalid for Win32 / self-parented): the caller
/// counts them as corrupt, so coverage turns `partial` instead of silently
/// shrinking the totals.
fn emit_entries(
    record_no: u64,
    info: &RecordInfo,
    sink: &mut dyn FnMut(DetailedEvent) -> Result<(), MftError>,
) -> Result<u64, MftError> {
    let meta = info.std_info.unwrap_or_default();
    if record_no < FIRST_USER_RECORD || info.names.is_empty() {
        return Ok(0);
    }
    let link = info
        .reparse_tag
        .is_some_and(|t| t & REPARSE_NAME_SURROGATE != 0);
    let kind = if link {
        EntryKind::Symlink
    } else if info.is_dir {
        EntryKind::Dir
    } else {
        EntryKind::File
    };
    let (logical, allocated) = match kind {
        EntryKind::File => {
            let d = info.data.unwrap_or_default();
            let alloc = if info.reparse_tag == Some(IO_REPARSE_TAG_WOF) {
                info.wof.map(|w| w.allocated).unwrap_or(d.allocated)
            } else {
                d.allocated
            };
            (d.logical, Some(alloc))
        }
        // Directories carry no bytes; links are never followed and carry
        // no allocation data (walker parity: 0 bytes, null allocation).
        EntryKind::Dir => (0, Some(0)),
        EntryKind::Symlink => (0, None),
    };
    let frn = record_no | ((info.seq as u64) << 48);
    let mut rejected = 0u64;
    let mut seen: Vec<(u64, &[u16])> = Vec::with_capacity(info.names.len());
    for n in &info.names {
        if seen
            .iter()
            .any(|(p, u)| *p == n.parent && *u == n.units.as_slice())
        {
            continue; // the same name listed twice (base + extension)
        }
        seen.push((n.parent, &n.units));
        if !crate::protocol::valid_name_units(&n.units) || n.parent == frn {
            rejected += 1; // never forward something the decoder must reject
            continue;
        }
        sink(DetailedEvent::Entry(
            WireEntry {
                frn,
                parent: n.parent,
                kind,
                logical,
                allocated,
                // FER-472 protocol v2: `modified` comes from the FER-475
                // `$STANDARD_INFORMATION` capture (0 when unknown).
                modified: meta.modified,
                name: String::from_utf16_lossy(&n.units),
            },
            meta,
        ))?;
    }
    Ok(rejected)
}

/// The MFT's own data runs, as a byte-addressable virtual stream.
///
/// Built only through [`MftStream::new`], which rejects any run whose
/// virtual or volume extent overflows or lies outside the volume — a crafted
/// (e.g. USB) NTFS image must never make the elevated helper wrap around or
/// spin. `read` repeats the checks with checked arithmetic and refuses a
/// zero-progress step instead of looping.
struct MftStream {
    runs: Vec<Run>,
    bytes_per_cluster: u64,
    total_clusters: u64,
}

fn layout_io(why: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why)
}

impl MftStream {
    fn new(runs: Vec<Run>, bytes_per_cluster: u64, total_clusters: u64) -> Result<Self, MftError> {
        let s = Self {
            runs,
            bytes_per_cluster,
            total_clusters,
        };
        s.check()?;
        Ok(s)
    }

    fn check(&self) -> Result<(), MftError> {
        for r in &self.runs {
            r.vcn
                .checked_add(r.clusters)
                .and_then(|v| v.checked_mul(self.bytes_per_cluster))
                .ok_or(MftError::Layout("MFT run overflows"))?;
            if let Some(lcn) = r.lcn {
                let end = lcn
                    .checked_add(r.clusters)
                    .ok_or(MftError::Layout("MFT run overflows"))?;
                if end > self.total_clusters {
                    return Err(MftError::Layout("MFT run outside the volume"));
                }
            }
        }
        Ok(())
    }

    fn extend(&mut self, runs: Vec<Run>) -> Result<(), MftError> {
        self.runs.extend(runs);
        self.runs.sort_by_key(|r| r.vcn);
        self.check()
    }

    fn read(&self, vol: &mut dyn VolumeReader, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let bpc = self.bytes_per_cluster;
        let mut done = 0usize;
        while done < buf.len() {
            let pos = offset
                .checked_add(done as u64)
                .ok_or_else(|| layout_io("MFT offset overflows"))?;
            let vcn = pos / bpc;
            let run = self
                .runs
                .iter()
                .find(|r| vcn >= r.vcn && vcn - r.vcn < r.clusters)
                .ok_or_else(|| io::Error::other("MFT offset outside its runs"))?;
            let run_start = run
                .vcn
                .checked_mul(bpc)
                .ok_or_else(|| layout_io("MFT run overflows"))?;
            let run_end = run
                .vcn
                .checked_add(run.clusters)
                .and_then(|v| v.checked_mul(bpc))
                .ok_or_else(|| layout_io("MFT run overflows"))?;
            let left = run_end
                .checked_sub(pos)
                .ok_or_else(|| layout_io("MFT run arithmetic"))?;
            let n = usize::try_from(left)
                .unwrap_or(usize::MAX)
                .min(buf.len() - done);
            if n == 0 {
                return Err(layout_io("MFT read made no progress"));
            }
            let out = &mut buf[done..done + n];
            match run.lcn {
                Some(lcn) => {
                    let at = lcn
                        .checked_mul(bpc)
                        .and_then(|b| b.checked_add(pos - run_start))
                        .ok_or_else(|| layout_io("MFT run overflows"))?;
                    vol.read_at(at, out)?
                }
                None => out.fill(0),
            }
            done += n;
        }
        Ok(())
    }

    fn covered_bytes(&self) -> u64 {
        self.runs
            .iter()
            .map(|r| r.vcn.saturating_add(r.clusters))
            .max()
            .unwrap_or(0)
            .saturating_mul(self.bytes_per_cluster)
    }
}

/// Distinct segment (record) numbers named by an `$ATTRIBUTE_LIST` value,
/// for the entries `want(type, name_len)` selects. Stops at the first
/// malformed entry (bounded by the value's length).
fn attr_list_segments(bytes: &[u8], want: impl Fn(u32, u8) -> bool) -> Vec<u64> {
    let mut pos = 0usize;
    let mut out: Vec<u64> = Vec::new();
    // FER-463 L4: O(1) dedup (a list can name ~645k entries).
    let mut seen: HashSet<u64> = HashSet::new();
    while pos + 0x1A <= bytes.len() {
        let ty = rd_u32(bytes, pos).unwrap_or(ATTR_END);
        let len = rd_u16(bytes, pos + 4).unwrap_or(0) as usize;
        if len < 0x1A || pos + len > bytes.len() {
            break;
        }
        let name_len = bytes[pos + 6];
        let seg = rd_u64(bytes, pos + 0x10).unwrap_or(0) & REF_MASK;
        if want(ty, name_len) && seen.insert(seg) {
            out.push(seg);
        }
        pos += len;
    }
    out
}

/// Extension records holding more of `$MFT`'s unnamed `$DATA`, named by record
/// 0's attribute list (record 0 itself excluded), at most
/// [`MAX_MFT_EXTENSIONS`] of them (FER-463 L4).
fn wanted_mft_segments(list: &[u8]) -> Result<Vec<u64>, MftError> {
    let mut wanted = attr_list_segments(list, |ty, name_len| ty == ATTR_DATA && name_len == 0);
    wanted.retain(|s| *s != 0);
    if wanted.len() > MAX_MFT_EXTENSIONS {
        return Err(MftError::Layout("too many $MFT extension records"));
    }
    Ok(wanted)
}

/// Locate the complete `$MFT` data run list: record 0's own unnamed `$DATA`
/// plus, for a heavily fragmented MFT, the continuation instances that live
/// in extension records named by record 0's `$ATTRIBUTE_LIST`.
fn bootstrap_mft(vol: &mut dyn VolumeReader, g: &VolumeGeometry) -> Result<MftStream, MftError> {
    let bpr = g.bytes_per_record as usize;
    let bpc = g.bytes_per_cluster as u64;
    let mut rec0 = vec![0u8; bpr];
    // validate() guarantees mft_start_lcn < total_clusters and that
    // total_clusters * bpc fits, so this cannot overflow.
    vol.read_at(g.mft_start_lcn * bpc, &mut rec0)?;
    if !apply_fixups(&mut rec0) {
        return Err(MftError::Layout("record 0 fixup mismatch"));
    }
    let info = parse_record(&rec0, Want::Bootstrap)
        .map_err(|_| MftError::Layout("record 0 malformed"))?
        .ok_or(MftError::Layout("record 0 is not a FILE record"))?;
    let mut stream = MftStream::new(info.data_runs.clone(), bpc, g.total_clusters)?;
    if stream.runs.is_empty() {
        return Err(MftError::Layout("no $MFT data runs"));
    }
    if let Some(list) = &info.attr_list {
        let bytes = match list {
            AttrListValue::Resident(v) => v.clone(),
            AttrListValue::NonResident { runs, size } => {
                if *size > 16 * 1024 * 1024 {
                    return Err(MftError::Layout("attribute list too large"));
                }
                let s = MftStream::new(runs.clone(), bpc, g.total_clusters)?;
                let alloc = (*size).div_ceil(512) * 512;
                let mut v = vec![0u8; alloc as usize];
                // Not inside the MFT: the list's runs are volume LCNs.
                s.read(vol, 0, &mut v)?;
                v.truncate(*size as usize);
                v
            }
        };
        let mut wanted = wanted_mft_segments(&bytes)?;
        // Resolve extension records in VCN order: each one we read can add
        // runs that make the next one reachable.
        let mut progress = true;
        while !wanted.is_empty() && progress {
            progress = false;
            let mut i = 0;
            while i < wanted.len() {
                let rec_no = wanted[i];
                let off = rec_no.saturating_mul(bpr as u64);
                if off.saturating_add(bpr as u64) > stream.covered_bytes() {
                    i += 1;
                    continue;
                }
                let mut buf = vec![0u8; bpr];
                stream.read(vol, off, &mut buf)?;
                if !apply_fixups(&mut buf) {
                    return Err(MftError::Layout("$MFT extension record fixup mismatch"));
                }
                let ext = parse_record(&buf, Want::Bootstrap)
                    .map_err(|_| MftError::Layout("$MFT extension record malformed"))?
                    .ok_or(MftError::Layout("$MFT extension record unused"))?;
                stream.extend(ext.data_runs)?;
                wanted.swap_remove(i);
                progress = true;
            }
        }
        if !wanted.is_empty() {
            return Err(MftError::Layout("unreachable $MFT extension record"));
        }
    }
    stream.runs.sort_by_key(|r| r.vcn);
    Ok(stream)
}

/// Scan the whole MFT. `cancelled` is polled once per chunk; `sink` gets
/// entries and progress and may abort the scan by returning an error.
pub fn scan(
    vol: &mut dyn VolumeReader,
    g: &VolumeGeometry,
    cancelled: &dyn Fn() -> bool,
    sink: &mut dyn FnMut(ScanEvent) -> Result<(), MftError>,
) -> Result<ScanSummary, MftError> {
    scan_detailed(vol, g, cancelled, &mut |ev| {
        sink(match ev {
            DetailedEvent::Entry(e, _) => ScanEvent::Entry(e),
            DetailedEvent::Progress { done, total } => ScanEvent::Progress { done, total },
        })
    })
}

/// [`scan`] that also reports each record's [`RecordMeta`] (FER-475).
/// Same records, same names, same order, same hardening.
pub fn scan_detailed(
    vol: &mut dyn VolumeReader,
    g: &VolumeGeometry,
    cancelled: &dyn Fn() -> bool,
    sink: &mut dyn FnMut(DetailedEvent) -> Result<(), MftError>,
) -> Result<ScanSummary, MftError> {
    scan_capped(vol, g, cancelled, sink, MAX_HELD_RECORDS)
}

/// [`scan_detailed`] with the held-record cap as a parameter (tests use a
/// small one).
fn scan_capped(
    vol: &mut dyn VolumeReader,
    g: &VolumeGeometry,
    cancelled: &dyn Fn() -> bool,
    sink: &mut dyn FnMut(DetailedEvent) -> Result<(), MftError>,
    max_held: usize,
) -> Result<ScanSummary, MftError> {
    g.validate()?;
    let stream = bootstrap_mft(vol, g)?;
    let bpr = g.bytes_per_record as u64;
    let mft_bytes = g.mft_valid_data_length.min(stream.covered_bytes());
    let total = mft_bytes / bpr;
    let mut summary = ScanSummary {
        root_frn: ROOT_RECORD | (ROOT_RECORD << 48),
        ..Default::default()
    };
    // Base records with an attribute list wait until the end (their
    // extensions may come later); extensions seen before their base wait
    // here too. Both maps stay small on real volumes.
    let mut pending: HashMap<u64, RecordInfo> = HashMap::new();
    let mut early_ext: HashMap<u64, Vec<RecordInfo>> = HashMap::new();
    // FER-463 L4: records currently held in `pending` + `early_ext`.
    let mut held = 0usize;
    let mut buf = vec![0u8; (RECORDS_PER_CHUNK * bpr) as usize];
    let mut rec_no = 0u64;
    while rec_no < total {
        if cancelled() {
            return Err(MftError::Cancelled);
        }
        let count = RECORDS_PER_CHUNK.min(total - rec_no);
        let chunk = &mut buf[..(count * bpr) as usize];
        stream.read(vol, rec_no * bpr, chunk)?;
        for (i, rec) in chunk.chunks_exact_mut(bpr as usize).enumerate() {
            let this = rec_no + i as u64;
            summary.records_scanned += 1;
            if rec.get(0..4) != Some(b"FILE") {
                continue;
            }
            if !apply_fixups(rec) {
                if rd_u16(rec, 0x16).is_some_and(|f| f & RECORD_IN_USE != 0) {
                    summary.corrupt_records += 1;
                }
                continue;
            }
            let info = match parse_record(rec, Want::Scan) {
                Ok(Some(info)) => info,
                Ok(None) => continue,
                Err(()) => {
                    summary.corrupt_records += 1;
                    continue;
                }
            };
            if this == ROOT_RECORD {
                summary.root_frn = ROOT_RECORD | ((info.seq as u64) << 48);
            }
            if info.base_ref != 0 {
                match pending.get_mut(&info.base_ref) {
                    Some(base) => base.merge(info),
                    None => {
                        held += 1;
                        if held > max_held {
                            return Err(MftError::Layout("too many records held back"));
                        }
                        early_ext.entry(info.base_ref).or_default().push(info)
                    }
                }
            } else if info.has_attr_list || early_ext.contains_key(&this) {
                let mut info = info;
                let exts = early_ext.remove(&this).unwrap_or_default();
                // The extensions merge into this one held record.
                held = held.saturating_sub(exts.len()) + 1;
                if held > max_held {
                    return Err(MftError::Layout("too many records held back"));
                }
                for ext in exts {
                    info.merge(ext);
                }
                pending.insert(this, info);
            } else {
                summary.corrupt_records += emit_entries(this, &info, sink)?;
            }
        }
        rec_no += count;
        sink(DetailedEvent::Progress {
            done: rec_no,
            total,
        })?;
    }
    let mut rest: Vec<(u64, RecordInfo)> = pending.into_iter().collect();
    rest.sort_by_key(|(n, _)| *n);
    for (n, info) in &rest {
        summary.corrupt_records += emit_entries(*n, info, sink)?;
    }
    Ok(summary)
}

// ── single-record parse for live updates (FER-475) ───────────────────────

/// Most extension records one live record may pull in through its
/// `$ATTRIBUTE_LIST` (a hostile list cannot make the caller issue an
/// unbounded number of reads).
pub const MAX_LIVE_EXTENSIONS: usize = 64;

/// The current state of one FILE record, re-read after a USN change
/// (`FSCTL_GET_NTFS_FILE_RECORD`), translated exactly like [`scan`] would.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveRecord {
    /// Free / never used / an extension record / a metafile: no user
    /// file lives under this record number now.
    Gone,
    /// In use. `entries` holds one entry per valid name (possibly none);
    /// `complete` is false when part of the record lives in extension
    /// records that could not all be read (sizes or names may be missing).
    Live {
        seq: u16,
        entries: Vec<WireEntry>,
        meta: RecordMeta,
        rejected: u64,
        complete: bool,
    },
    /// In use but structurally invalid.
    Corrupt,
}

/// Parse record `record_no` from `rec` (one file record, as returned by
/// `FSCTL_GET_NTFS_FILE_RECORD`). `fetch(n)` must return record `n` (or
/// `None`); it is only called for extension records named by a RESIDENT
/// attribute list, at most [`MAX_LIVE_EXTENSIONS`] times.
///
/// Update-sequence fixups are applied when they verify; when they do not,
/// the record is taken as already fixed up (records from the NTFS cache
/// are). Applying verified fixups to an already fixed-up record is a no-op
/// (each sector tail already holds its saved value), so this is safe either
/// way; every field access stays bounds-checked regardless.
pub fn parse_live_record(
    record_no: u64,
    rec: &mut [u8],
    fetch: &mut dyn FnMut(u64) -> Option<Vec<u8>>,
) -> LiveRecord {
    if record_no < FIRST_USER_RECORD {
        return LiveRecord::Gone;
    }
    let _ = apply_fixups(rec);
    let mut info = match parse_record(rec, Want::Live) {
        Ok(Some(info)) => info,
        Ok(None) => return LiveRecord::Gone,
        Err(()) => return LiveRecord::Corrupt,
    };
    if info.base_ref != 0 {
        return LiveRecord::Gone;
    }
    let mut complete = true;
    if info.has_attr_list {
        match info.attr_list.take() {
            Some(AttrListValue::Resident(bytes)) => {
                let segs = attr_list_segments(&bytes, |_, _| true);
                let segs: Vec<u64> = segs
                    .into_iter()
                    .filter(|s| *s != record_no && *s >= FIRST_USER_RECORD)
                    .collect();
                if segs.len() > MAX_LIVE_EXTENSIONS {
                    complete = false;
                }
                for seg in segs.into_iter().take(MAX_LIVE_EXTENSIONS) {
                    let ext = fetch(seg).and_then(|mut b| {
                        let _ = apply_fixups(&mut b);
                        parse_record(&b, Want::Scan).ok().flatten()
                    });
                    match ext {
                        Some(ext) if ext.base_ref == record_no => info.merge(ext),
                        _ => complete = false,
                    }
                }
            }
            // A non-resident list (extremely fragmented file) would need
            // raw cluster reads; report what the base record holds.
            _ => complete = false,
        }
    }
    let seq = info.seq;
    let meta = info.std_info.unwrap_or_default();
    let mut entries = Vec::new();
    let rejected = match emit_entries(record_no, &info, &mut |ev| {
        if let DetailedEvent::Entry(e, _) = ev {
            entries.push(e);
        }
        Ok(())
    }) {
        Ok(r) => r,
        Err(_) => return LiveRecord::Corrupt,
    };
    LiveRecord::Live {
        seq,
        entries,
        meta,
        rejected,
        complete,
    }
}

// ── synthetic image builder (tests + the helper's self-test fixture) ─────

#[cfg(any(test, feature = "fixture"))]
pub mod fixture {
    //! Builds byte-exact FILE records and a tiny in-memory "volume" so the
    //! parser can be tested without admin rights or a real NTFS image.
    //! Exported behind the `fixture` feature so the FerrumIndexer tests
    //! (FER-475) reuse it instead of a second builder. The feature is on in
    //! normal builds too (see Cargo.toml); nothing outside tests calls it.
    use super::*;

    /// Record header flags.
    pub const FLAG_IN_USE: u16 = RECORD_IN_USE;
    pub const FLAG_DIR: u16 = RECORD_IN_USE | RECORD_IS_DIRECTORY;
    pub const ATTR_TYPE_DATA: u32 = ATTR_DATA;
    pub const ATTR_TYPE_ATTRIBUTE_LIST: u32 = ATTR_ATTRIBUTE_LIST;

    /// `$STANDARD_INFORMATION` with a last-write time and attributes.
    pub fn std_info_meta(modified: u64, fattrs: u32) -> Vec<u8> {
        let mut v = vec![0u8; 0x48];
        v[0x08..0x10].copy_from_slice(&modified.to_le_bytes());
        v[0x20..0x24].copy_from_slice(&fattrs.to_le_bytes());
        resident_attr(ATTR_STANDARD_INFORMATION, &[], &v)
    }

    pub const BPS: u32 = 512;
    pub const BPC: u32 = 4096;
    pub const BPR: u32 = 1024;

    pub struct MemVolume {
        pub bytes: Vec<u8>,
        pub reads: usize,
    }

    impl VolumeReader for MemVolume {
        fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
            self.reads += 1;
            let o = offset as usize;
            let src = self
                .bytes
                .get(o..o + buf.len())
                .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "past end"))?;
            buf.copy_from_slice(src);
            Ok(())
        }
    }

    pub fn utf16(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    /// One attribute, already 8-aligned.
    pub fn resident_attr(ty: u32, name: &[u16], value: &[u8]) -> Vec<u8> {
        let name_off = 0x18usize;
        let value_off = (name_off + name.len() * 2).next_multiple_of(8);
        let len = (value_off + value.len()).next_multiple_of(8);
        let mut a = vec![0u8; len];
        a[0..4].copy_from_slice(&ty.to_le_bytes());
        a[4..8].copy_from_slice(&(len as u32).to_le_bytes());
        a[8] = 0;
        a[9] = name.len() as u8;
        a[10..12].copy_from_slice(&(name_off as u16).to_le_bytes());
        a[0x10..0x14].copy_from_slice(&(value.len() as u32).to_le_bytes());
        a[0x14..0x16].copy_from_slice(&(value_off as u16).to_le_bytes());
        for (i, u) in name.iter().enumerate() {
            a[name_off + 2 * i..name_off + 2 * i + 2].copy_from_slice(&u.to_le_bytes());
        }
        a[value_off..value_off + value.len()].copy_from_slice(value);
        a
    }

    #[allow(clippy::too_many_arguments)]
    pub fn nonresident_attr(
        ty: u32,
        name: &[u16],
        flags: u16,
        lowest_vcn: u64,
        allocated: u64,
        data_size: u64,
        compressed: Option<u64>,
        pairs: &[u8],
    ) -> Vec<u8> {
        let hdr = if compressed.is_some() { 0x48 } else { 0x40 };
        let name_off = hdr;
        let pairs_off = (name_off + name.len() * 2).next_multiple_of(8);
        let len = (pairs_off + pairs.len() + 1).next_multiple_of(8);
        let mut a = vec![0u8; len];
        a[0..4].copy_from_slice(&ty.to_le_bytes());
        a[4..8].copy_from_slice(&(len as u32).to_le_bytes());
        a[8] = 1;
        a[9] = name.len() as u8;
        a[10..12].copy_from_slice(&(name_off as u16).to_le_bytes());
        a[0x0C..0x0E].copy_from_slice(&flags.to_le_bytes());
        a[0x10..0x18].copy_from_slice(&lowest_vcn.to_le_bytes());
        a[0x20..0x22].copy_from_slice(&(pairs_off as u16).to_le_bytes());
        a[0x28..0x30].copy_from_slice(&allocated.to_le_bytes());
        a[0x30..0x38].copy_from_slice(&data_size.to_le_bytes());
        a[0x38..0x40].copy_from_slice(&data_size.to_le_bytes());
        if let Some(c) = compressed {
            a[0x40..0x48].copy_from_slice(&c.to_le_bytes());
        }
        for (i, u) in name.iter().enumerate() {
            a[name_off + 2 * i..name_off + 2 * i + 2].copy_from_slice(&u.to_le_bytes());
        }
        a[pairs_off..pairs_off + pairs.len()].copy_from_slice(pairs);
        a
    }

    pub fn file_name_value(parent: u64, name: &str, ns: u8, fattrs: u32, tag: u32) -> Vec<u8> {
        let units = utf16(name);
        let mut v = vec![0u8; 0x42 + units.len() * 2];
        v[0..8].copy_from_slice(&parent.to_le_bytes());
        v[0x38..0x3C].copy_from_slice(&fattrs.to_le_bytes());
        v[0x3C..0x40].copy_from_slice(&tag.to_le_bytes());
        v[0x40] = units.len() as u8;
        v[0x41] = ns;
        for (i, u) in units.iter().enumerate() {
            v[0x42 + 2 * i..0x44 + 2 * i].copy_from_slice(&u.to_le_bytes());
        }
        v
    }

    pub fn std_info(fattrs: u32) -> Vec<u8> {
        let mut v = vec![0u8; 0x48];
        v[0x20..0x24].copy_from_slice(&fattrs.to_le_bytes());
        resident_attr(ATTR_STANDARD_INFORMATION, &[], &v)
    }

    pub fn name_attr(parent: u64, name: &str, ns: u8) -> Vec<u8> {
        resident_attr(
            ATTR_FILE_NAME,
            &[],
            &file_name_value(parent, name, ns, 0, 0),
        )
    }

    pub fn data_resident(len: usize) -> Vec<u8> {
        resident_attr(ATTR_DATA, &[], &vec![0xAB; len])
    }

    /// Mapping pairs for a single run of `clusters` at `lcn` (both < 2^24).
    pub fn single_run(clusters: u64, lcn: u64) -> Vec<u8> {
        let mut p = vec![0x33];
        p.extend_from_slice(&clusters.to_le_bytes()[..3]);
        p.extend_from_slice(&lcn.to_le_bytes()[..3]);
        p.push(0);
        p
    }

    /// Assemble a FILE record with valid update-sequence protection.
    pub fn record(seq: u16, flags: u16, base_ref: u64, attrs: &[Vec<u8>]) -> Vec<u8> {
        let mut r = vec![0u8; BPR as usize];
        r[0..4].copy_from_slice(b"FILE");
        let usa_off = 0x30u16;
        let usa_count = (BPR as usize / USA_STRIDE + 1) as u16;
        r[4..6].copy_from_slice(&usa_off.to_le_bytes());
        r[6..8].copy_from_slice(&usa_count.to_le_bytes());
        r[0x10..0x12].copy_from_slice(&seq.to_le_bytes());
        r[0x12..0x14].copy_from_slice(&1u16.to_le_bytes());
        let first = 0x38u16;
        r[0x14..0x16].copy_from_slice(&first.to_le_bytes());
        r[0x16..0x18].copy_from_slice(&flags.to_le_bytes());
        r[0x20..0x28].copy_from_slice(&base_ref.to_le_bytes());
        let mut pos = first as usize;
        for a in attrs {
            r[pos..pos + a.len()].copy_from_slice(a);
            pos += a.len();
        }
        r[pos..pos + 4].copy_from_slice(&ATTR_END.to_le_bytes());
        pos += 8;
        r[0x18..0x1C].copy_from_slice(&(pos as u32).to_le_bytes());
        r[0x1C..0x20].copy_from_slice(&BPR.to_le_bytes());
        // Update sequence: USN 0x0001; stash each sector's last 2 bytes.
        let usn = [0x01u8, 0x00];
        r[0x30..0x32].copy_from_slice(&usn);
        for i in 0..(BPR as usize / USA_STRIDE) {
            let end = (i + 1) * USA_STRIDE - 2;
            let saved = [r[end], r[end + 1]];
            r[0x32 + 2 * i..0x34 + 2 * i].copy_from_slice(&saved);
            r[end..end + 2].copy_from_slice(&usn);
        }
        r
    }

    pub fn fref(rec: u64, seq: u16) -> u64 {
        rec | ((seq as u64) << 48)
    }

    /// A volume whose MFT starts at `mft_lcn` and holds `records` (index =
    /// record number; `None` = zeroed, never-used slot). Record 0 is built
    /// automatically with a single run covering the MFT.
    pub fn volume(mft_lcn: u64, mut records: Vec<Option<Vec<u8>>>) -> (MemVolume, VolumeGeometry) {
        let per_cluster = (BPC / BPR) as usize;
        while !records.len().is_multiple_of(per_cluster) || records.len() < 16 {
            records.push(None);
        }
        let clusters = (records.len() / per_cluster) as u64;
        let bytes = (clusters * BPC as u64) as usize;
        let pairs = single_run(clusters, mft_lcn);
        records[0] = Some(record(
            1,
            RECORD_IN_USE,
            0,
            &[
                name_attr(fref(5, 5), "$MFT", 3),
                nonresident_attr(
                    ATTR_DATA,
                    &[],
                    0,
                    0,
                    bytes as u64,
                    bytes as u64,
                    None,
                    &pairs,
                ),
            ],
        ));
        let mut img = vec![0u8; (mft_lcn * BPC as u64) as usize + bytes];
        let base = (mft_lcn * BPC as u64) as usize;
        for (i, r) in records.iter().enumerate() {
            if let Some(r) = r {
                img[base + i * BPR as usize..base + (i + 1) * BPR as usize].copy_from_slice(r);
            }
        }
        let img_len = img.len();
        (
            MemVolume {
                bytes: img,
                reads: 0,
            },
            VolumeGeometry {
                bytes_per_sector: BPS,
                bytes_per_cluster: BPC,
                bytes_per_record: BPR,
                mft_start_lcn: mft_lcn,
                mft_valid_data_length: bytes as u64,
                total_clusters: (img_len / BPC as usize) as u64,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::*;
    use super::*;

    const IN_USE: u16 = RECORD_IN_USE;
    const DIR: u16 = RECORD_IN_USE | RECORD_IS_DIRECTORY;

    fn root_record() -> Vec<u8> {
        record(5, DIR, 0, &[std_info(0x16), name_attr(fref(5, 5), ".", 3)])
    }

    fn collect(vol: &mut MemVolume, g: &VolumeGeometry) -> (Vec<WireEntry>, ScanSummary, u64) {
        let mut out = Vec::new();
        let mut progress = 0;
        let summary = scan(vol, g, &|| false, &mut |ev| {
            match ev {
                ScanEvent::Entry(e) => out.push(e),
                ScanEvent::Progress { done, .. } => progress = done,
            }
            Ok(())
        })
        .expect("scan");
        (out, summary, progress)
    }

    fn base_volume(extra: Vec<(usize, Vec<u8>)>) -> (MemVolume, VolumeGeometry) {
        let mut recs: Vec<Option<Vec<u8>>> = vec![None; 40];
        recs[5] = Some(root_record());
        // A metafile that must never surface.
        recs[11] = Some(record(1, DIR, 0, &[name_attr(fref(5, 5), "$Extend", 3)]));
        for (i, r) in extra {
            recs[i] = Some(r);
        }
        volume(10, recs)
    }

    #[test]
    fn fixups_detect_torn_records() {
        let mut r = record(1, IN_USE, 0, &[name_attr(fref(5, 5), "a", 1)]);
        let mut torn = r.clone();
        assert!(apply_fixups(&mut r));
        torn[511] ^= 0xFF;
        assert!(!apply_fixups(&mut torn));
    }

    #[test]
    fn failed_fixups_leave_the_record_untouched() {
        // Sector 2 torn: sector 1 must not have been rewritten either.
        let mut torn = record(1, IN_USE, 0, &[name_attr(fref(5, 5), "a", 1)]);
        torn[1022] ^= 0xFF;
        let before = torn.clone();
        assert!(!apply_fixups(&mut torn));
        assert_eq!(torn, before);
    }

    #[test]
    fn runs_decode_with_negative_offsets_and_sparse() {
        // 0x21: len 1 byte, off 2 bytes. Run1: 16 clusters @ 0x1000.
        // Run2: 8 clusters, delta -0x800 → 0x800. Run3: sparse 4 clusters.
        let pairs = [
            0x21, 0x10, 0x00, 0x10, 0x21, 0x08, 0x00, 0xF8, 0x01, 0x04, 0x00,
        ];
        let runs = decode_runs(&pairs, 0).unwrap();
        assert_eq!(
            runs,
            vec![
                Run {
                    vcn: 0,
                    clusters: 16,
                    lcn: Some(0x1000)
                },
                Run {
                    vcn: 16,
                    clusters: 8,
                    lcn: Some(0x800)
                },
                Run {
                    vcn: 24,
                    clusters: 4,
                    lcn: None
                },
            ]
        );
        assert!(decode_runs(&[0x21, 0x10], 0).is_none(), "truncated");
        assert!(
            decode_runs(&[0x21, 0x10, 0x00, 0x80], 0).is_none(),
            "negative lcn"
        );
        assert!(decode_runs(&[0x90, 0x00], 0).is_none(), "oversized fields");
    }

    #[test]
    fn scans_files_dirs_and_skips_metafiles() {
        let (mut vol, g) = base_volume(vec![
            (
                20,
                record(
                    2,
                    DIR,
                    0,
                    &[std_info(0x10), name_attr(fref(5, 5), "Users", 1)],
                ),
            ),
            (
                21,
                record(
                    1,
                    IN_USE,
                    0,
                    &[
                        std_info(0x20),
                        name_attr(fref(20, 2), "notes.txt", 3),
                        data_resident(300),
                    ],
                ),
            ),
            (
                22,
                record(
                    7,
                    IN_USE,
                    0,
                    &[
                        name_attr(fref(20, 2), "big.bin", 1),
                        name_attr(fref(20, 2), "BIG~1.BIN", NAMESPACE_DOS),
                        nonresident_attr(
                            ATTR_DATA,
                            &[],
                            0,
                            0,
                            16384,
                            10_000,
                            None,
                            &single_run(4, 100),
                        ),
                    ],
                ),
            ),
            // Child of $Extend: parent never emitted → Ferrum drops it.
            (
                30,
                record(
                    1,
                    IN_USE,
                    0,
                    &[name_attr(fref(11, 1), "$UsnJrnl", 3), data_resident(8)],
                ),
            ),
            // Free (not in use) record: ignored.
            (
                31,
                record(1, 0, 0, &[name_attr(fref(5, 5), "deleted.txt", 1)]),
            ),
        ]);
        let (entries, summary, progress) = collect(&mut vol, &g);
        assert_eq!(summary.root_frn, fref(5, 5));
        assert_eq!(summary.corrupt_records, 0);
        assert_eq!(progress, summary.records_scanned);
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["Users", "notes.txt", "big.bin", "$UsnJrnl"]);
        let users = &entries[0];
        assert_eq!(users.kind, EntryKind::Dir);
        assert_eq!(users.frn, fref(20, 2));
        assert_eq!((users.logical, users.allocated), (0, Some(0)));
        let notes = &entries[1];
        assert_eq!(notes.parent, fref(20, 2));
        assert_eq!((notes.logical, notes.allocated), (300, Some(300)));
        let big = &entries[2];
        // Ordinary stream: allocated mirrors GetCompressedFileSizeW = size.
        assert_eq!((big.logical, big.allocated), (10_000, Some(10_000)));
    }

    #[test]
    fn hardlinks_yield_one_entry_per_name() {
        let (mut vol, g) = base_volume(vec![(
            20,
            record(
                3,
                IN_USE,
                0,
                &[
                    name_attr(fref(5, 5), "a.txt", 1),
                    name_attr(fref(5, 5), "b.txt", 0),
                    // Duplicate listing of the same name is collapsed.
                    name_attr(fref(5, 5), "a.txt", 1),
                    data_resident(10),
                ],
            ),
        )]);
        let (entries, ..) = collect(&mut vol, &g);
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["a.txt", "b.txt"]);
        assert!(
            entries
                .iter()
                .all(|e| e.frn == fref(20, 3) && e.logical == 10)
        );
    }

    #[test]
    fn compressed_sparse_and_wof_allocation() {
        let wof = utf16("WofCompressedData");
        let (mut vol, g) = base_volume(vec![
            (
                20,
                record(
                    1,
                    IN_USE,
                    0,
                    &[
                        name_attr(fref(5, 5), "comp.log", 1),
                        nonresident_attr(
                            ATTR_DATA,
                            &[],
                            ATTR_FLAG_COMPRESSED,
                            0,
                            65536,
                            60_000,
                            Some(8192),
                            &single_run(2, 200),
                        ),
                    ],
                ),
            ),
            (
                21,
                record(
                    1,
                    IN_USE,
                    0,
                    &[
                        name_attr(fref(5, 5), "sparse.vhd", 1),
                        nonresident_attr(
                            ATTR_DATA,
                            &[],
                            ATTR_FLAG_SPARSE,
                            0,
                            1 << 30,
                            1 << 30,
                            Some(4096),
                            &[0x01, 0x10, 0x00],
                        ),
                    ],
                ),
            ),
            (
                22,
                record(
                    1,
                    IN_USE,
                    0,
                    &[
                        std_info(FILE_ATTRIBUTE_REPARSE_POINT),
                        name_attr(fref(5, 5), "app.exe", 1),
                        nonresident_attr(
                            ATTR_DATA,
                            &[],
                            ATTR_FLAG_SPARSE,
                            0,
                            40960,
                            40000,
                            Some(0),
                            &[0x01, 0x0A, 0x00],
                        ),
                        nonresident_attr(
                            ATTR_DATA,
                            &wof,
                            0,
                            0,
                            12288,
                            11000,
                            None,
                            &single_run(3, 300),
                        ),
                        resident_attr(ATTR_REPARSE_POINT, &[], &IO_REPARSE_TAG_WOF.to_le_bytes()),
                    ],
                ),
            ),
        ]);
        let (entries, ..) = collect(&mut vol, &g);
        let by = |n: &str| entries.iter().find(|e| e.name == n).unwrap().clone();
        assert_eq!(
            (by("comp.log").logical, by("comp.log").allocated),
            (60_000, Some(8192))
        );
        assert_eq!(
            (by("sparse.vhd").logical, by("sparse.vhd").allocated),
            (1 << 30, Some(4096))
        );
        let app = by("app.exe");
        // WOF is not a name surrogate: still a file, allocation from the
        // compressed stream (what GetCompressedFileSizeW reports).
        assert_eq!(app.kind, EntryKind::File);
        assert_eq!((app.logical, app.allocated), (40000, Some(11000)));
    }

    #[test]
    fn last_write_time_comes_from_standard_information_not_file_name() {
        // $STANDARD_INFORMATION: created 0x00, altered (last write) 0x08.
        let std_with_times = |created: u64, altered: u64| {
            let mut v = vec![0u8; 0x48];
            v[0x00..0x08].copy_from_slice(&created.to_le_bytes());
            v[0x08..0x10].copy_from_slice(&altered.to_le_bytes());
            resident_attr(ATTR_STANDARD_INFORMATION, &[], &v)
        };
        let mut fname = file_name_value(fref(5, 5), "t.txt", 1, 0, 0);
        // A stale altered time inside $FILE_NAME (offset 0x18) must be ignored.
        fname[0x18..0x20].copy_from_slice(&7u64.to_le_bytes());
        let (mut vol, g) = base_volume(vec![
            (
                20,
                record(
                    1,
                    IN_USE,
                    0,
                    &[
                        std_with_times(111, 133_000_000_000_000_123),
                        resident_attr(ATTR_FILE_NAME, &[], &fname),
                        data_resident(4),
                    ],
                ),
            ),
            // No $STANDARD_INFORMATION at all: unknown (0), never a guess.
            (
                21,
                record(1, IN_USE, 0, &[name_attr(fref(5, 5), "u.txt", 1)]),
            ),
        ]);
        let (entries, ..) = collect(&mut vol, &g);
        let m = |n: &str| entries.iter().find(|e| e.name == n).unwrap().modified;
        assert_eq!(m("t.txt"), 133_000_000_000_000_123);
        assert_eq!(m("u.txt"), 0);
    }

    #[test]
    fn name_surrogate_reparse_is_a_symlink() {
        const SYMLINK: u32 = 0xA000_000C;
        const JUNCTION: u32 = 0xA000_0003;
        const CLOUD: u32 = 0x9000_001A;
        let reparse = |tag: u32| resident_attr(ATTR_REPARSE_POINT, &[], &tag.to_le_bytes());
        let (mut vol, g) = base_volume(vec![
            (
                20,
                record(
                    1,
                    DIR,
                    0,
                    &[
                        std_info(FILE_ATTRIBUTE_REPARSE_POINT),
                        name_attr(fref(5, 5), "junction", 1),
                        reparse(JUNCTION),
                    ],
                ),
            ),
            (
                21,
                record(
                    1,
                    IN_USE,
                    0,
                    &[
                        std_info(FILE_ATTRIBUTE_REPARSE_POINT),
                        name_attr(fref(5, 5), "link.txt", 1),
                        reparse(SYMLINK),
                    ],
                ),
            ),
            (
                22,
                record(
                    1,
                    IN_USE,
                    0,
                    &[
                        std_info(FILE_ATTRIBUTE_REPARSE_POINT),
                        name_attr(fref(5, 5), "cloud.docx", 1),
                        data_resident(5),
                        reparse(CLOUD),
                    ],
                ),
            ),
        ]);
        let (entries, ..) = collect(&mut vol, &g);
        let kind = |n: &str| entries.iter().find(|e| e.name == n).unwrap().kind;
        assert_eq!(kind("junction"), EntryKind::Symlink);
        assert_eq!(kind("link.txt"), EntryKind::Symlink);
        assert_eq!(kind("cloud.docx"), EntryKind::File);
        let link = entries.iter().find(|e| e.name == "link.txt").unwrap();
        assert_eq!((link.logical, link.allocated), (0, None));
    }

    #[test]
    fn extension_records_merge_in_either_order() {
        let attr_list = resident_attr(ATTR_ATTRIBUTE_LIST, &[], &[0u8; 0x20]);
        let (mut vol, g) = base_volume(vec![
            // Extension BEFORE its base (record 20 → base 25).
            (
                20,
                record(
                    1,
                    IN_USE,
                    fref(25, 4),
                    &[
                        name_attr(fref(5, 5), "second-link.dat", 1),
                        nonresident_attr(
                            ATTR_DATA,
                            &[],
                            0,
                            0,
                            8192,
                            7777,
                            None,
                            &single_run(2, 400),
                        ),
                    ],
                ),
            ),
            (
                25,
                record(
                    4,
                    IN_USE,
                    0,
                    &[attr_list.clone(), name_attr(fref(5, 5), "first.dat", 1)],
                ),
            ),
            // Base first, extension after (26 → ext 27), sizes in ext.
            (
                26,
                record(
                    2,
                    IN_USE,
                    0,
                    &[attr_list, name_attr(fref(5, 5), "frag.iso", 1)],
                ),
            ),
            (
                27,
                record(
                    1,
                    IN_USE,
                    fref(26, 2),
                    &[nonresident_attr(
                        ATTR_DATA,
                        &[],
                        0,
                        0,
                        4096,
                        4000,
                        None,
                        &single_run(1, 500),
                    )],
                ),
            ),
            // Orphan extension of a base that never appears: dropped.
            (
                28,
                record(1, IN_USE, fref(39, 1), &[name_attr(fref(5, 5), "ghost", 1)]),
            ),
        ]);
        let (entries, ..) = collect(&mut vol, &g);
        let mut names: Vec<(&str, u64, u64)> = entries
            .iter()
            .map(|e| (e.name.as_str(), e.frn, e.logical))
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                ("first.dat", fref(25, 4), 7777),
                ("frag.iso", fref(26, 2), 4000),
                ("second-link.dat", fref(25, 4), 7777),
            ]
        );
    }

    #[test]
    fn corrupt_records_are_counted_and_skipped() {
        let mut bad = record(1, IN_USE, 0, &[name_attr(fref(5, 5), "torn.txt", 1)]);
        bad[1022] ^= 0x55; // breaks the second sector's USN
        let mut bogus = record(1, IN_USE, 0, &[name_attr(fref(5, 5), "bogus", 1)]);
        // Attribute length pointing past the record end.
        bogus[0x38 + 4..0x38 + 8].copy_from_slice(&5000u32.to_le_bytes());
        let (mut vol, g) = base_volume(vec![
            (20, bad),
            (21, bogus),
            (
                22,
                record(1, IN_USE, 0, &[name_attr(fref(5, 5), "ok.txt", 1)]),
            ),
        ]);
        let (entries, summary, _) = collect(&mut vol, &g);
        assert_eq!(summary.corrupt_records, 2);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "ok.txt");
        assert_eq!(entries[0].logical, 0, "no $DATA → 0 bytes");
    }

    #[test]
    fn hostile_names_are_never_forwarded() {
        let (mut vol, g) = base_volume(vec![
            (20, record(1, IN_USE, 0, &[name_attr(fref(5, 5), "..", 0)])),
            (
                21,
                record(1, IN_USE, 0, &[name_attr(fref(5, 5), "a\\b", 0)]),
            ),
            (
                22,
                record(1, IN_USE, 0, &[name_attr(fref(22, 1), "self", 1)]),
            ),
            (
                23,
                record(1, IN_USE, 0, &[name_attr(fref(5, 5), "fine", 0)]),
            ),
        ]);
        let (entries, summary, _) = collect(&mut vol, &g);
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["fine"]);
        // Rejected names are counted, never silently dropped (re-review).
        assert_eq!(summary.corrupt_records, 3);
    }

    #[test]
    fn cancel_and_sink_errors_stop_the_scan() {
        let (mut vol, g) = base_volume(vec![]);
        let r = scan(&mut vol, &g, &|| true, &mut |_| Ok(()));
        assert!(matches!(r, Err(MftError::Cancelled)));
        let (mut vol, g) = base_volume(vec![(
            20,
            record(1, IN_USE, 0, &[name_attr(fref(5, 5), "x", 1)]),
        )]);
        let r = scan(&mut vol, &g, &|| false, &mut |_| {
            Err(MftError::Sink(io::Error::other("closed")))
        });
        assert!(matches!(r, Err(MftError::Sink(_))));
    }

    #[test]
    fn rejects_bad_geometry_and_bad_record_zero() {
        let (mut vol, mut g) = base_volume(vec![]);
        g.bytes_per_record = 1000;
        assert!(matches!(
            scan(&mut vol, &g, &|| false, &mut |_| Ok(())),
            Err(MftError::Layout(_))
        ));
        let (mut vol, mut g) = base_volume(vec![]);
        g.mft_start_lcn = 0; // record 0 is zeros there
        assert!(matches!(
            scan(&mut vol, &g, &|| false, &mut |_| Ok(())),
            Err(MftError::Layout(_))
        ));
    }

    /// Record 0 whose `$DATA` mapping is replaced by `pairs` (crafted volume).
    fn volume_with_mft_pairs(pairs: &[u8]) -> (MemVolume, VolumeGeometry) {
        let (mut vol, g) = base_volume(vec![]);
        let base = (g.mft_start_lcn * BPC as u64) as usize;
        let rec0 = record(
            1,
            RECORD_IN_USE,
            0,
            &[nonresident_attr(
                ATTR_DATA,
                &[],
                0,
                0,
                g.mft_valid_data_length,
                g.mft_valid_data_length,
                None,
                pairs,
            )],
        );
        vol.bytes[base..base + BPR as usize].copy_from_slice(&rec0);
        (vol, g)
    }

    fn layout_err(vol: &mut MemVolume, g: &VolumeGeometry) -> &'static str {
        match scan(vol, g, &|| false, &mut |_| Ok(())) {
            Err(MftError::Layout(why)) => why,
            other => panic!("expected a layout error, got {:?}", other.map(|_| ())),
        }
    }

    #[test]
    fn crafted_runs_are_rejected_not_wrapped() {
        // A run far outside the volume (LCN 2^40 with 8-byte offset).
        let mut far = vec![0x81, 0x01];
        far.extend_from_slice(&(1u64 << 40).to_le_bytes());
        far.push(0);
        let (mut vol, g) = volume_with_mft_pairs(&far);
        assert_eq!(layout_err(&mut vol, &g), "MFT run outside the volume");

        // A gigantic cluster count whose byte extent overflows u64.
        let mut huge = vec![0x18];
        huge.extend_from_slice(&u64::MAX.to_le_bytes()[..8]);
        huge.push(0x0A); // LCN 10
        huge.push(0);
        let (mut vol, g) = volume_with_mft_pairs(&huge);
        let why = layout_err(&mut vol, &g);
        assert!(
            why == "MFT run overflows" || why == "MFT run outside the volume",
            "{why}"
        );
    }

    #[test]
    fn geometry_outside_the_volume_is_rejected() {
        let (mut vol, mut g) = base_volume(vec![]);
        g.mft_start_lcn = g.total_clusters;
        assert_eq!(layout_err(&mut vol, &g), "implausible volume geometry");
        let (mut vol, mut g) = base_volume(vec![]);
        g.total_clusters = u64::MAX;
        assert_eq!(layout_err(&mut vol, &g), "implausible volume geometry");
    }

    #[test]
    fn stream_read_refuses_zero_progress_and_overflow() {
        let s = MftStream {
            runs: vec![Run {
                vcn: 0,
                clusters: 1,
                lcn: Some(0),
            }],
            bytes_per_cluster: 4096,
            total_clusters: 1,
        };
        let mut vol = MemVolume {
            bytes: vec![0; 4096],
            reads: 0,
        };
        let mut buf = vec![0u8; 512];
        assert!(s.read(&mut vol, u64::MAX - 10, &mut buf).is_err());
        assert!(s.read(&mut vol, 4096, &mut buf).is_err(), "past the runs");
        assert!(s.read(&mut vol, 0, &mut buf).is_ok());
    }

    #[test]
    fn fragmented_mft_via_attribute_list() {
        // MFT in two extents: records 0..8 at LCN 10 (2 clusters), records
        // 8..16+ at LCN 50. Record 0 holds the first extent; its attribute
        // list points at extension record 3, which holds the second.
        let (mut vol, g0) = base_volume(vec![]);
        let first_bytes = 2 * BPC as usize; // records 0..8
        let img = std::mem::take(&mut vol.bytes);
        let mft_base = 10 * BPC as usize;
        let total_recs = (img.len() - mft_base) / BPR as usize;
        let total_clusters = (total_recs * BPR as usize / BPC as usize) as u64;
        let mut new_img =
            vec![0u8; 50 * BPC as usize + (total_clusters as usize - 2) * BPC as usize];
        new_img[mft_base..mft_base + first_bytes]
            .copy_from_slice(&img[mft_base..mft_base + first_bytes]);
        new_img[50 * BPC as usize..].copy_from_slice(
            &img[mft_base + first_bytes..mft_base + total_clusters as usize * BPC as usize],
        );
        let total_bytes = total_clusters * BPC as u64;
        // Attribute list entry: type DATA, len 0x20, name_len 0, lowest VCN 2,
        // segment = record 3.
        let mut ale = vec![0u8; 0x20];
        ale[0..4].copy_from_slice(&ATTR_DATA.to_le_bytes());
        ale[4..6].copy_from_slice(&0x20u16.to_le_bytes());
        ale[6] = 0;
        ale[7] = 0x1A;
        ale[8..16].copy_from_slice(&2u64.to_le_bytes());
        ale[0x10..0x18].copy_from_slice(&fref(3, 1).to_le_bytes());
        let rec0 = record(
            1,
            RECORD_IN_USE,
            0,
            &[
                resident_attr(ATTR_ATTRIBUTE_LIST, &[], &ale),
                nonresident_attr(
                    ATTR_DATA,
                    &[],
                    0,
                    0,
                    total_bytes,
                    total_bytes,
                    None,
                    &single_run(2, 10),
                ),
            ],
        );
        // Second extent run, relative LCN 50 (fresh mapping pairs).
        let rec3 = record(
            1,
            RECORD_IN_USE,
            0,
            &[nonresident_attr(
                ATTR_DATA,
                &[],
                0,
                2,
                0,
                0,
                None,
                &single_run(total_clusters - 2, 50),
            )],
        );
        new_img[mft_base..mft_base + BPR as usize].copy_from_slice(&rec0);
        new_img[mft_base + 3 * BPR as usize..mft_base + 4 * BPR as usize].copy_from_slice(&rec3);
        // A user file in the SECOND extent (record 20).
        let user = record(
            1,
            IN_USE,
            0,
            &[name_attr(fref(5, 5), "far.txt", 1), data_resident(42)],
        );
        let off = 50 * BPC as usize + (20 - 8) * BPR as usize;
        new_img[off..off + BPR as usize].copy_from_slice(&user);
        let mut vol = MemVolume {
            bytes: new_img,
            reads: 0,
        };
        let g = VolumeGeometry {
            mft_valid_data_length: total_bytes,
            total_clusters: (vol.bytes.len() / BPC as usize) as u64,
            ..g0
        };
        let (entries, summary, _) = collect(&mut vol, &g);
        assert_eq!(summary.records_scanned, total_recs as u64);
        let far = entries
            .iter()
            .find(|e| e.name == "far.txt")
            .expect("far.txt");
        assert_eq!(far.logical, 42);
    }

    // ── FER-475: metadata + live single-record parse ────────────────────

    #[test]
    fn detailed_scan_reports_standard_information() {
        let (mut vol, g) = base_volume(vec![
            (
                20,
                record(
                    1,
                    IN_USE,
                    0,
                    &[
                        std_info_meta(133_000_000_000_000_123, 0x21),
                        name_attr(fref(5, 5), "t.txt", 1),
                        data_resident(4),
                    ],
                ),
            ),
            (
                21,
                record(1, IN_USE, 0, &[name_attr(fref(5, 5), "u.txt", 1)]),
            ),
        ]);
        let mut got = Vec::new();
        scan_detailed(&mut vol, &g, &|| false, &mut |ev| {
            if let DetailedEvent::Entry(e, m) = ev {
                got.push((e.name, m));
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(
            got,
            vec![
                (
                    "t.txt".to_string(),
                    RecordMeta {
                        modified: 133_000_000_000_000_123,
                        attributes: 0x21
                    }
                ),
                ("u.txt".to_string(), RecordMeta::default()),
            ]
        );
        // The plain scan still yields the same entries.
        let (entries, ..) = collect(&mut vol, &g);
        assert_eq!(entries.len(), 2);
    }

    fn no_fetch(_: u64) -> Option<Vec<u8>> {
        None
    }

    #[test]
    fn live_record_raw_and_already_fixed_up_parse_the_same() {
        let raw = record(
            3,
            IN_USE,
            0,
            &[
                std_info_meta(77, 0x20),
                name_attr(fref(5, 5), "a.txt", 1),
                name_attr(fref(5, 5), "b.txt", 0),
                data_resident(10),
            ],
        );
        let mut fixed = raw.clone();
        assert!(apply_fixups(&mut fixed));
        let mut a = raw.clone();
        let mut b = fixed.clone();
        let ra = parse_live_record(20, &mut a, &mut no_fetch);
        let rb = parse_live_record(20, &mut b, &mut no_fetch);
        assert_eq!(ra, rb);
        match ra {
            LiveRecord::Live {
                seq,
                entries,
                meta,
                rejected,
                complete,
            } => {
                assert_eq!(seq, 3);
                assert_eq!(meta.modified, 77);
                assert_eq!(rejected, 0);
                assert!(complete);
                let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
                assert_eq!(names, vec!["a.txt", "b.txt"]);
                assert!(
                    entries
                        .iter()
                        .all(|e| e.frn == fref(20, 3) && e.logical == 10)
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn live_record_gone_and_corrupt() {
        let mut free = record(1, 0, 0, &[name_attr(fref(5, 5), "x", 1)]);
        assert_eq!(
            parse_live_record(20, &mut free, &mut no_fetch),
            LiveRecord::Gone
        );
        let mut zero = vec![0u8; BPR as usize];
        assert_eq!(
            parse_live_record(20, &mut zero, &mut no_fetch),
            LiveRecord::Gone
        );
        let mut ext = record(1, IN_USE, fref(30, 1), &[name_attr(fref(5, 5), "e", 1)]);
        assert_eq!(
            parse_live_record(20, &mut ext, &mut no_fetch),
            LiveRecord::Gone,
            "an extension record is never a user file"
        );
        let mut meta = record(1, IN_USE, 0, &[name_attr(fref(5, 5), "$Secure", 1)]);
        assert_eq!(
            parse_live_record(9, &mut meta, &mut no_fetch),
            LiveRecord::Gone
        );
        let mut bogus = record(1, IN_USE, 0, &[name_attr(fref(5, 5), "bogus", 1)]);
        bogus[0x38 + 4..0x38 + 8].copy_from_slice(&5000u32.to_le_bytes());
        assert_eq!(
            parse_live_record(20, &mut bogus, &mut no_fetch),
            LiveRecord::Corrupt
        );
        let mut short = b"FILE".to_vec();
        assert_eq!(
            parse_live_record(20, &mut short, &mut no_fetch),
            LiveRecord::Corrupt
        );
    }

    fn attr_list(items: &[(u32, u64)]) -> Vec<u8> {
        let mut ale = vec![0u8; 0x20 * items.len()];
        for (i, (ty, seg)) in items.iter().enumerate() {
            let o = i * 0x20;
            ale[o..o + 4].copy_from_slice(&ty.to_le_bytes());
            ale[o + 4..o + 6].copy_from_slice(&0x20u16.to_le_bytes());
            ale[o + 0x10..o + 0x18].copy_from_slice(&fref(*seg, 1).to_le_bytes());
        }
        resident_attr(ATTR_ATTRIBUTE_LIST, &[], &ale)
    }

    #[test]
    fn live_record_merges_resident_attribute_list_extensions() {
        // Base 25 (seq 4) lists extension 27, which holds the second name
        // and the data size.
        let mut base = record(
            4,
            IN_USE,
            0,
            &[
                attr_list(&[(ATTR_FILE_NAME, 27), (ATTR_DATA, 27)]),
                name_attr(fref(5, 5), "first.dat", 1),
            ],
        );
        let ext = record(
            1,
            IN_USE,
            fref(25, 4),
            &[
                name_attr(fref(5, 5), "second.dat", 1),
                nonresident_attr(ATTR_DATA, &[], 0, 0, 8192, 7777, None, &single_run(2, 400)),
            ],
        );
        let mut fetched = Vec::new();
        let r = parse_live_record(25, &mut base.clone(), &mut |n| {
            fetched.push(n);
            (n == 27).then(|| ext.clone())
        });
        assert_eq!(fetched, vec![27]);
        match r {
            LiveRecord::Live {
                entries, complete, ..
            } => {
                assert!(complete);
                let mut v: Vec<(&str, u64)> = entries
                    .iter()
                    .map(|e| (e.name.as_str(), e.logical))
                    .collect();
                v.sort();
                assert_eq!(v, vec![("first.dat", 7777), ("second.dat", 7777)]);
            }
            other => panic!("{other:?}"),
        }
        // Extension unavailable or belonging to another base: incomplete.
        for wrong in [None, Some(record(1, IN_USE, fref(99, 1), &[]))] {
            match parse_live_record(25, &mut base, &mut |_| wrong.clone()) {
                LiveRecord::Live {
                    entries, complete, ..
                } => {
                    assert!(!complete);
                    assert_eq!(entries.len(), 1);
                }
                other => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn live_record_extension_reads_are_bounded() {
        // 20 distinct segments (what fits in a 1 KiB record): each one is
        // fetched exactly once, never more than MAX_LIVE_EXTENSIONS reads.
        let items: Vec<(u32, u64)> = (0..20).map(|i| (ATTR_DATA, 100 + i)).collect();
        let mut base = record(
            1,
            IN_USE,
            0,
            &[attr_list(&items), name_attr(fref(5, 5), "f", 1)],
        );
        let mut calls = 0;
        let _ = parse_live_record(20, &mut base, &mut |_| {
            calls += 1;
            None
        });
        assert_eq!(calls, 20);
        assert!(calls <= MAX_LIVE_EXTENSIONS);
    }

    /// Deterministic fuzz: mutated/truncated live records never panic.
    #[test]
    fn live_parse_survives_garbage() {
        let seed_rec = record(
            2,
            IN_USE,
            0,
            &[
                std_info_meta(5, 1),
                attr_list(&[(ATTR_DATA, 30), (ATTR_FILE_NAME, 31)]),
                name_attr(fref(5, 5), "seed.txt", 1),
                data_resident(33),
            ],
        );
        let mut s = 0x2545_F491_4F6C_DD1Du64;
        let mut next = || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        };
        for _ in 0..5_000 {
            let mut r = seed_rec.clone();
            for _ in 0..(1 + next() % 8) {
                let at = (next() as usize) % r.len();
                r[at] = next() as u8;
            }
            let cut = if next() % 5 == 0 {
                (next() as usize) % r.len()
            } else {
                r.len()
            };
            let mut r = r[..cut].to_vec();
            let rec_no = 20 + next() % 4;
            let _ = parse_live_record(rec_no, &mut r, &mut |_| Some(seed_rec.clone()));
        }
    }
    /// xorshift64: deterministic pseudo-random bytes for the fuzz-style tests.
    fn rng(mut s: u64) -> impl FnMut() -> u64 {
        move || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        }
    }

    fn data_list_entry(seg: u64) -> Vec<u8> {
        let mut e = vec![0u8; 0x20];
        e[0..4].copy_from_slice(&ATTR_DATA.to_le_bytes());
        e[4..6].copy_from_slice(&0x20u16.to_le_bytes());
        e[0x10..0x18].copy_from_slice(&fref(seg, 1).to_le_bytes());
        e
    }

    /// FER-463 L4: a crafted `$MFT` attribute list is deduplicated in O(1)
    /// per entry and may name at most `MAX_MFT_EXTENSIONS` extension records.
    #[test]
    fn mft_extension_list_is_deduplicated_and_capped() {
        // 100k entries naming only records 0..4: record 0 dropped, order kept.
        let list: Vec<u8> = (0..100_000u64)
            .flat_map(|i| data_list_entry(i % 4))
            .collect();
        let t0 = std::time::Instant::now();
        assert_eq!(wanted_mft_segments(&list).unwrap(), vec![1, 2, 3]);
        assert!(t0.elapsed() < std::time::Duration::from_secs(5));
        let at_cap: Vec<u8> = (1..=MAX_MFT_EXTENSIONS as u64)
            .flat_map(data_list_entry)
            .collect();
        assert_eq!(
            wanted_mft_segments(&at_cap).unwrap().len(),
            MAX_MFT_EXTENSIONS
        );
        let over: Vec<u8> = (1..=MAX_MFT_EXTENSIONS as u64 + 1)
            .flat_map(data_list_entry)
            .collect();
        assert!(matches!(
            wanted_mft_segments(&over),
            Err(MftError::Layout("too many $MFT extension records"))
        ));
    }

    /// FER-463 L4: extension records whose base never shows up are held back
    /// only up to the cap; past it the scan stops with a layout error instead
    /// of growing without bound.
    #[test]
    fn records_held_back_are_capped() {
        let orphan = |base: u64| record(1, IN_USE, fref(base, 1), &[name_attr(fref(5, 5), "x", 1)]);
        let (mut vol, g) = base_volume(vec![(20, orphan(30)), (21, orphan(31)), (22, orphan(32))]);
        let mut run = |cap: usize| scan_capped(&mut vol, &g, &|| false, &mut |_| Ok(()), cap);
        assert!(matches!(
            run(2),
            Err(MftError::Layout("too many records held back"))
        ));
        assert!(run(3).is_ok());
        // A base with an attribute list absorbs its early extensions: the
        // held count drops back (two extensions + base = one held record).
        let attr_list = resident_attr(ATTR_ATTRIBUTE_LIST, &[], &[0u8; 0x20]);
        let ext = || record(1, IN_USE, fref(25, 4), &[name_attr(fref(5, 5), "e", 1)]);
        let (mut vol, g) = base_volume(vec![
            (20, ext()),
            (21, ext()),
            (
                25,
                record(4, IN_USE, 0, &[attr_list, name_attr(fref(5, 5), "b", 1)]),
            ),
            (30, orphan(39)),
        ]);
        assert!(scan_capped(&mut vol, &g, &|| false, &mut |_| Ok(()), 2).is_ok());
    }

    /// FER-463 I3 (deterministic fuzz, runs on stable in every test run):
    /// mutated and truncated FILE records and random mapping pairs never
    /// panic `apply_fixups`, `parse_record` or `decode_runs` (debug builds
    /// also trap any arithmetic overflow here).
    #[test]
    fn record_and_runlist_parsers_survive_garbage() {
        let seeds = [
            root_record(),
            record(
                1,
                IN_USE,
                0,
                &[
                    std_info(0x20),
                    name_attr(fref(5, 5), "seed.bin", 1),
                    nonresident_attr(ATTR_DATA, &[], 0, 0, 8192, 7777, None, &single_run(2, 400)),
                ],
            ),
            record(
                4,
                IN_USE,
                0,
                &[
                    attr_list(&[(ATTR_DATA, 27), (ATTR_FILE_NAME, 28)]),
                    name_attr(fref(5, 5), "frag.iso", 1),
                ],
            ),
            record(1, IN_USE, fref(25, 4), &[data_resident(40)]),
        ];
        let mut next = rng(0x9E37_79B9_7F4A_7C15);
        for _ in 0..20_000 {
            let mut r = seeds[(next() % seeds.len() as u64) as usize].clone();
            for _ in 0..(1 + next() % 12) {
                let at = (next() as usize) % r.len();
                r[at] = next() as u8;
            }
            if next().is_multiple_of(4) {
                r.truncate((next() as usize) % r.len());
            }
            let _ = apply_fixups(&mut r);
            let _ = parse_record(&r, Want::Scan);
            let _ = parse_record(&r, Want::Bootstrap);
            let pairs: Vec<u8> = (0..next() % 48).map(|_| next() as u8).collect();
            let _ = decode_runs(&pairs, next() % 4096);
        }
    }

    /// FER-463 I3: `bootstrap_mft` + the full scan over volumes whose MFT
    /// bytes (record 0 included) are mutated always end in `Ok` or `Err`:
    /// no panic, no endless loop, no unbounded read.
    #[test]
    fn bootstrap_and_scan_survive_mutated_volumes() {
        let user = record(
            1,
            IN_USE,
            0,
            &[
                name_attr(fref(5, 5), "user.dat", 1),
                nonresident_attr(ATTR_DATA, &[], 0, 0, 8192, 7777, None, &single_run(2, 400)),
            ],
        );
        let (vol0, g) = base_volume(vec![(20, user)]);
        let mft = (g.mft_start_lcn * BPC as u64) as usize;
        let mft_len = vol0.bytes.len() - mft;
        let mut next = rng(0xD1B5_4A32_D192_ED03);
        for round in 0..400 {
            let mut vol = MemVolume {
                bytes: vol0.bytes.clone(),
                reads: 0,
            };
            // Every other round hits record 0 (the bootstrap) on purpose.
            let span = if round % 2 == 0 {
                BPR as usize
            } else {
                mft_len
            };
            for _ in 0..(1 + next() % 16) {
                let at = mft + (next() as usize) % span;
                vol.bytes[at] = next() as u8;
            }
            let _ = scan(&mut vol, &g, &|| false, &mut |_| Ok(()));
            assert!(vol.reads < 10_000, "round {round}: {} reads", vol.reads);
        }
    }
}
