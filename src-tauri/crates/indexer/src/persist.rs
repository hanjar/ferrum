//! On-disk format of a saved volume index (FER-475): pure encode/decode.
//! File I/O (where, ACL, handle checks) lives in `store`.
//!
//! ```text
//! header (76 bytes, little endian, uncompressed)
//!   magic        8  "FRIXIDX\0"
//!   version      u32  = FORMAT_VERSION
//!   flags        u32  = 0
//!   volume_serial u64
//!   journal_id   u64   USN journal the position belongs to
//!   next_usn     i64   replay starts here
//!   scope        u64   fingerprint of mode + exclusions (shared mode)
//!   root_frn     u64
//!   entries      u64
//!   body_len     u64   uncompressed body length
//!   body_crc     u32   CRC-32 of the uncompressed body
//! body (raw deflate), per entry:
//!   frn u64, parent u64, kind u8, size u64, alloc u64, modified u64,
//!   attributes u32, name_len u16, name (UTF-8)
//! ```
//!
//! Decoding is strict and bounded: `body_len` must fit the caller's budget
//! BEFORE anything is inflated, the inflater is capped at `body_len`, the
//! CRC must match, every entry is validated by `VolumeIndex::add_name`
//! (record range, name rules), the entry count must match exactly and no
//! trailing data is accepted. Any failure means "rebuild from the MFT".

use crate::index::{Kind, Meta, VolumeIndex};
use flate2::Compression;
use flate2::read::DeflateDecoder;
use flate2::write::DeflateEncoder;
use std::io::{Read, Write};

pub const MAGIC: [u8; 8] = *b"FRIXIDX\0";
pub const FORMAT_VERSION: u32 = 1;
pub const HEADER_LEN: usize = 76;
/// Fixed part of one body entry.
const ENTRY_FIXED: usize = 8 + 8 + 1 + 8 + 8 + 8 + 4 + 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexHeader {
    pub volume_serial: u64,
    pub journal_id: u64,
    pub next_usn: i64,
    pub scope: u64,
    pub root_frn: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PersistError {
    BadHeader(&'static str),
    TooLarge,
    Corrupt(&'static str),
}

impl std::fmt::Display for PersistError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PersistError::BadHeader(w) => write!(f, "bad index header: {w}"),
            PersistError::TooLarge => write!(f, "saved index exceeds the memory budget"),
            PersistError::Corrupt(w) => write!(f, "corrupt index body: {w}"),
        }
    }
}

/// Serialise `index` with `header`.
pub fn encode(index: &VolumeIndex, header: &IndexHeader) -> Vec<u8> {
    let mut out = vec![0u8; HEADER_LEN];
    let mut crc = crc32fast::Hasher::new();
    let mut body_len = 0u64;
    let mut count = 0u64;
    let mut enc = DeflateEncoder::new(out, Compression::fast());
    let mut rec = Vec::with_capacity(ENTRY_FIXED + 768);
    for e in index.entries() {
        rec.clear();
        rec.extend_from_slice(&e.frn.to_le_bytes());
        rec.extend_from_slice(&e.parent.to_le_bytes());
        rec.push(e.kind as u8);
        rec.extend_from_slice(&e.meta.size.to_le_bytes());
        rec.extend_from_slice(
            &e.meta
                .allocated
                .unwrap_or(crate::index::ALLOC_UNKNOWN)
                .to_le_bytes(),
        );
        rec.extend_from_slice(&e.meta.modified.to_le_bytes());
        rec.extend_from_slice(&e.meta.attributes.to_le_bytes());
        rec.extend_from_slice(&(e.name.len() as u16).to_le_bytes());
        rec.extend_from_slice(e.name.as_bytes());
        crc.update(&rec);
        body_len += rec.len() as u64;
        count += 1;
        // Writing into a Vec cannot fail.
        let _ = enc.write_all(&rec);
    }
    out = enc.finish().unwrap_or_default();
    if out.len() < HEADER_LEN {
        out.resize(HEADER_LEN, 0);
    }
    let h = &mut out[..HEADER_LEN];
    h[0..8].copy_from_slice(&MAGIC);
    h[8..12].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
    h[12..16].copy_from_slice(&0u32.to_le_bytes());
    h[16..24].copy_from_slice(&header.volume_serial.to_le_bytes());
    h[24..32].copy_from_slice(&header.journal_id.to_le_bytes());
    h[32..40].copy_from_slice(&header.next_usn.to_le_bytes());
    h[40..48].copy_from_slice(&header.scope.to_le_bytes());
    h[48..56].copy_from_slice(&header.root_frn.to_le_bytes());
    h[56..64].copy_from_slice(&count.to_le_bytes());
    h[64..72].copy_from_slice(&body_len.to_le_bytes());
    h[72..76].copy_from_slice(&crc.finalize().to_le_bytes());
    out
}

fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap_or([0; 4]))
}
fn u64_at(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap_or([0; 8]))
}

/// Read only the header (cheap check before a full decode).
pub fn decode_header(bytes: &[u8]) -> Result<IndexHeader, PersistError> {
    if bytes.len() < HEADER_LEN {
        return Err(PersistError::BadHeader("truncated"));
    }
    if bytes[0..8] != MAGIC {
        return Err(PersistError::BadHeader("magic"));
    }
    if u32_at(bytes, 8) != FORMAT_VERSION {
        return Err(PersistError::BadHeader("version"));
    }
    if u32_at(bytes, 12) != 0 {
        return Err(PersistError::BadHeader("flags"));
    }
    Ok(IndexHeader {
        volume_serial: u64_at(bytes, 16),
        journal_id: u64_at(bytes, 24),
        next_usn: u64_at(bytes, 32) as i64,
        scope: u64_at(bytes, 40),
        root_frn: u64_at(bytes, 48),
    })
}

/// Fill `buf` from `r`, feeding the CRC and the byte count. Returns false
/// on a clean end of stream before the first byte.
fn read_counted(
    r: &mut impl Read,
    buf: &mut [u8],
    crc: &mut crc32fast::Hasher,
    total: &mut u64,
) -> Result<bool, PersistError> {
    let mut got = 0;
    while got < buf.len() {
        match r.read(&mut buf[got..]) {
            Ok(0) if got == 0 => return Ok(false),
            Ok(0) => return Err(PersistError::Corrupt("truncated entry")),
            Ok(n) => got += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return Err(PersistError::Corrupt("deflate")),
        }
    }
    crc.update(buf);
    *total += buf.len() as u64;
    Ok(true)
}

/// Decode a saved index. `max_body` bounds the uncompressed body (the
/// caller passes its remaining memory budget).
///
/// Review L4: the body is inflated as a STREAM, entry by entry, with the
/// CRC computed incrementally — the uncompressed body is never held in
/// memory. Peak = the compressed file + the index being built (+ a few KiB),
/// instead of file + whole body + index (up to ~3x the budget before).
/// Validation is unchanged: length and CRC are checked at the end (a
/// mismatch discards the index), every entry goes through `add_name`.
pub fn decode(bytes: &[u8], max_body: u64) -> Result<(IndexHeader, VolumeIndex), PersistError> {
    let header = decode_header(bytes)?;
    let count = u64_at(bytes, 56);
    let body_len = u64_at(bytes, 64);
    let want_crc = u32_at(bytes, 72);
    if body_len > max_body || body_len > usize::MAX as u64 / 2 {
        return Err(PersistError::TooLarge);
    }
    if count > body_len / ENTRY_FIXED as u64 {
        return Err(PersistError::Corrupt("entry count"));
    }
    // One byte more than announced is read, so a longer stream is caught.
    let inflater = DeflateDecoder::new(&bytes[HEADER_LEN..]).take(body_len + 1);
    let mut r = std::io::BufReader::with_capacity(64 * 1024, inflater);
    let mut crc = crc32fast::Hasher::new();
    let mut total = 0u64;
    let mut fixed = [0u8; ENTRY_FIXED];
    let mut name_buf = [0u8; crate::index::MAX_NAME_BYTES];
    let mut index = VolumeIndex::new(header.root_frn);
    let mut seen = 0u64;
    while read_counted(&mut r, &mut fixed, &mut crc, &mut total)? {
        if total > body_len {
            return Err(PersistError::Corrupt("length"));
        }
        let b = &fixed[..];
        let frn = u64_at(b, 0);
        let parent = u64_at(b, 8);
        let kind = Kind::from_byte(b[16]).ok_or(PersistError::Corrupt("kind"))?;
        let size = u64_at(b, 17);
        let alloc = u64_at(b, 25);
        let modified = u64_at(b, 33);
        let attributes = u32_at(b, 41);
        let n = u16_at(b, 45) as usize;
        if n == 0 || n > name_buf.len() {
            return Err(PersistError::Corrupt("entry"));
        }
        if !read_counted(&mut r, &mut name_buf[..n], &mut crc, &mut total)? {
            return Err(PersistError::Corrupt("truncated name"));
        }
        if total > body_len {
            return Err(PersistError::Corrupt("length"));
        }
        let name =
            std::str::from_utf8(&name_buf[..n]).map_err(|_| PersistError::Corrupt("name"))?;
        let meta = Meta {
            size,
            allocated: (alloc != crate::index::ALLOC_UNKNOWN).then_some(alloc),
            modified,
            attributes,
        };
        // The record table grows to the highest record number (4 bytes per
        // record): refuse a number whose table alone would exceed the
        // budget BEFORE it is allocated.
        let record = frn & crate::index::RECORD_MASK;
        if record.saturating_add(1).saturating_mul(4) > max_body {
            return Err(PersistError::TooLarge);
        }
        index
            .add_name(frn, parent, kind, meta, name)
            .map_err(|_| PersistError::Corrupt("entry"))?;
        // The memory the index itself needs is bounded by the same budget.
        if index.memory_bytes() > max_body {
            return Err(PersistError::TooLarge);
        }
        seen += 1;
        if seen > count {
            return Err(PersistError::Corrupt("entry count"));
        }
    }
    if total != body_len {
        return Err(PersistError::Corrupt("length"));
    }
    if crc.finalize() != want_crc {
        return Err(PersistError::Corrupt("checksum"));
    }
    if seen != count || index.len() as u64 != count {
        return Err(PersistError::Corrupt("entry count"));
    }
    Ok((header, index))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: u64 = 5 | (5 << 48);

    fn sample() -> (VolumeIndex, IndexHeader) {
        let mut ix = VolumeIndex::new(ROOT);
        ix.add_name(20 | (1 << 48), ROOT, Kind::Dir, Meta::default(), "Users")
            .unwrap();
        for i in 0..500u64 {
            ix.add_name(
                (100 + i) | (3 << 48),
                20 | (1 << 48),
                Kind::File,
                Meta {
                    size: i * 10,
                    allocated: if i % 7 == 0 { None } else { Some(i * 4096) },
                    modified: 133_000_000_000_000_000 + i,
                    attributes: 0x20,
                },
                &format!("file-{i}-é.txt"),
            )
            .unwrap();
        }
        let h = IndexHeader {
            volume_serial: 0xDEAD_BEEF,
            journal_id: 0x01D0_0000_0000_0001,
            next_usn: 123_456,
            scope: 42,
            root_frn: ROOT,
        };
        (ix, h)
    }

    fn snapshot(ix: &VolumeIndex) -> Vec<(u64, u64, String, Meta)> {
        let mut v: Vec<_> = ix
            .entries()
            .map(|e| (e.frn, e.parent, e.name.to_string(), e.meta))
            .collect();
        v.sort_by(|a, b| (a.0, &a.2).cmp(&(b.0, &b.2)));
        v
    }

    #[test]
    fn round_trip() {
        let (ix, h) = sample();
        let bytes = encode(&ix, &h);
        let (h2, ix2) = decode(&bytes, 1 << 30).unwrap();
        assert_eq!(h2, h);
        assert_eq!(snapshot(&ix2), snapshot(&ix));
        assert_eq!(decode_header(&bytes).unwrap(), h);
        // Compressed well below the raw body.
        assert!(bytes.len() < 500 * ENTRY_FIXED);
    }

    #[test]
    fn empty_index_round_trips() {
        let ix = VolumeIndex::new(ROOT);
        let (_, h) = sample();
        let (_, ix2) = decode(&encode(&ix, &h), 1 << 20).unwrap();
        assert!(ix2.is_empty());
    }

    #[test]
    fn header_damage_is_detected() {
        let (ix, h) = sample();
        let bytes = encode(&ix, &h);
        for (at, why) in [(0usize, "magic"), (8, "version"), (12, "flags")] {
            let mut b = bytes.clone();
            b[at] ^= 0xFF;
            assert_eq!(
                decode(&b, 1 << 30).err(),
                Some(PersistError::BadHeader(why))
            );
        }
        assert!(decode(&bytes[..10], 1 << 30).is_err());
    }

    #[test]
    fn budget_is_checked_before_inflating() {
        let (ix, h) = sample();
        let bytes = encode(&ix, &h);
        assert_eq!(decode(&bytes, 100).err(), Some(PersistError::TooLarge));
        // A lying body_len (huge) is refused without allocation.
        let mut b = bytes.clone();
        b[64..72].copy_from_slice(&u64::MAX.to_le_bytes());
        assert_eq!(decode(&b, 1 << 30).err(), Some(PersistError::TooLarge));
    }

    #[test]
    fn body_damage_is_detected() {
        let (ix, h) = sample();
        let bytes = encode(&ix, &h);
        // CRC field.
        let mut b = bytes.clone();
        b[72] ^= 1;
        assert_eq!(
            decode(&b, 1 << 30).err(),
            Some(PersistError::Corrupt("checksum"))
        );
        // Count field.
        let mut b = bytes.clone();
        b[56] ^= 1;
        assert!(decode(&b, 1 << 30).is_err());
        // Truncated compressed stream.
        assert!(decode(&bytes[..bytes.len() - 20], 1 << 30).is_err());
    }

    #[test]
    fn hostile_body_with_valid_crc_is_rejected() {
        // Build a body by hand: a bad kind byte, then a bad name, each with
        // a correct CRC and length so only the entry checks can catch them.
        let mk = |kind: u8, name: &[u8], frn: u64| {
            let mut body = Vec::new();
            body.extend_from_slice(&frn.to_le_bytes());
            body.extend_from_slice(&ROOT.to_le_bytes());
            body.push(kind);
            body.extend_from_slice(&[0u8; 8 * 3 + 4]);
            body.extend_from_slice(&(name.len() as u16).to_le_bytes());
            body.extend_from_slice(name);
            let mut out = vec![0u8; HEADER_LEN];
            out[0..8].copy_from_slice(&MAGIC);
            out[8..12].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
            out[48..56].copy_from_slice(&ROOT.to_le_bytes());
            out[56..64].copy_from_slice(&1u64.to_le_bytes());
            out[64..72].copy_from_slice(&(body.len() as u64).to_le_bytes());
            out[72..76].copy_from_slice(&crc32fast::hash(&body).to_le_bytes());
            let mut enc = DeflateEncoder::new(out, Compression::fast());
            enc.write_all(&body).unwrap();
            enc.finish().unwrap()
        };
        assert!(decode(&mk(0, b"ok.txt", 20), 1 << 20).is_ok());
        assert_eq!(
            decode(&mk(9, b"ok.txt", 20), 1 << 20).err(),
            Some(PersistError::Corrupt("kind"))
        );
        for bad in [&b"a\\b"[..], b"..", b"", &[0xC3, 0x28]] {
            assert!(decode(&mk(0, bad, 20), 1 << 20).is_err(), "{bad:?}");
        }
        // Record number out of range.
        assert!(decode(&mk(0, b"x", crate::index::MAX_RECORD + 1), 1 << 20).is_err());
    }

    /// Deterministic fuzz over the whole file: never panics.
    #[test]
    fn decode_survives_garbage() {
        let (ix, h) = sample();
        let seed = encode(&ix, &h);
        let mut s = 0x1234_5678_9ABC_DEF1u64;
        let mut next = || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        };
        for _ in 0..2_000 {
            let mut b = seed.clone();
            for _ in 0..(1 + next() % 4) {
                let at = (next() as usize) % b.len();
                b[at] = next() as u8;
            }
            let _ = decode(&b, 1 << 24);
        }
    }

    /// Review L4: decoding no longer allocates the whole body. A header
    /// announcing a huge body (within the budget) with a tiny stream fails
    /// on length without reserving `body_len` bytes up front.
    #[test]
    fn decode_streams_and_never_reserves_the_body() {
        let (ix, h) = sample();
        let mut bytes = encode(&ix, &h);
        // Announce 1 GiB; budget allows it; the stream is a few hundred B.
        bytes[64..72].copy_from_slice(&(1u64 << 30).to_le_bytes());
        assert_eq!(
            decode(&bytes, 4 << 30).err(),
            Some(PersistError::Corrupt("length"))
        );
        // A longer stream than announced is refused too.
        let mut bytes = encode(&ix, &h);
        let real = u64::from_le_bytes(bytes[64..72].try_into().unwrap());
        bytes[64..72].copy_from_slice(&(real - 1).to_le_bytes());
        assert!(decode(&bytes, 1 << 30).is_err());
    }
}
