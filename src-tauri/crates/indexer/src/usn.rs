//! Parser for `FSCTL_READ_USN_JOURNAL` output (FER-475).
//!
//! The buffer is a `USN` (next start position, 8 bytes) followed by packed
//! `USN_RECORD_V2` / `USN_RECORD_V3` records. The kernel produced it, but the
//! parser still treats it as untrusted: every record length is bounded by
//! the buffer, must be 8-aligned and at least the fixed header, and a zero
//! length (which would loop forever) is an error. Unknown major versions
//! (V4 range records, future formats) are skipped by their length.
//!
//! Only the file reference, the parent reference (FER-477: the change log
//! of incremental sync) and the reason flags are used: the indexer
//! re-reads the file's MFT record for the actual state (names, sizes,
//! times), which makes replay idempotent. The record's file name is not
//! decoded at all.

/// `USN_REASON_*` flags the indexer reacts to (everything else is ignored).
pub mod reason {
    pub const DATA_OVERWRITE: u32 = 0x0000_0001;
    pub const DATA_EXTEND: u32 = 0x0000_0002;
    pub const DATA_TRUNCATION: u32 = 0x0000_0004;
    pub const NAMED_DATA_OVERWRITE: u32 = 0x0000_0010;
    pub const NAMED_DATA_EXTEND: u32 = 0x0000_0020;
    pub const NAMED_DATA_TRUNCATION: u32 = 0x0000_0040;
    pub const FILE_CREATE: u32 = 0x0000_0100;
    pub const FILE_DELETE: u32 = 0x0000_0200;
    pub const EA_CHANGE: u32 = 0x0000_0400;
    pub const SECURITY_CHANGE: u32 = 0x0000_0800;
    pub const RENAME_OLD_NAME: u32 = 0x0000_1000;
    pub const RENAME_NEW_NAME: u32 = 0x0000_2000;
    pub const INDEXABLE_CHANGE: u32 = 0x0000_4000;
    pub const BASIC_INFO_CHANGE: u32 = 0x0000_8000;
    pub const HARD_LINK_CHANGE: u32 = 0x0001_0000;
    pub const COMPRESSION_CHANGE: u32 = 0x0002_0000;
    pub const ENCRYPTION_CHANGE: u32 = 0x0004_0000;
    pub const OBJECT_ID_CHANGE: u32 = 0x0008_0000;
    pub const REPARSE_POINT_CHANGE: u32 = 0x0010_0000;
    pub const STREAM_CHANGE: u32 = 0x0020_0000;
    pub const CLOSE: u32 = 0x8000_0000;

    /// Changes that can alter what the index stores for a record: names,
    /// existence, sizes, times, attributes, reparse/link kind.
    pub const RELEVANT: u32 = DATA_OVERWRITE
        | DATA_EXTEND
        | DATA_TRUNCATION
        | NAMED_DATA_EXTEND
        | NAMED_DATA_TRUNCATION
        | FILE_CREATE
        | FILE_DELETE
        | RENAME_OLD_NAME
        | RENAME_NEW_NAME
        | BASIC_INFO_CHANGE
        | HARD_LINK_CHANGE
        | COMPRESSION_CHANGE
        | ENCRYPTION_CHANGE
        | REPARSE_POINT_CHANGE
        | STREAM_CHANGE
        | CLOSE;

    /// What the journal is read with (`ReasonMask`): the relevant changes
    /// plus `SECURITY_CHANGE` (review I5), which never changes the index
    /// but invalidates the query side's AccessCheck cache.
    pub const READ_MASK: u32 = RELEVANT | SECURITY_CHANGE;
}

/// What the indexer needs from one journal record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsnChange {
    pub usn: i64,
    /// File reference (record number + sequence).
    pub frn: u64,
    /// FER-477: parent directory reference at the time of the record (the
    /// old parent for `RENAME_OLD_NAME`, the new one for `RENAME_NEW_NAME`).
    /// 0 when the record carries a 128-bit parent id that is not NTFS.
    pub parent: u64,
    pub reason: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsnParseError {
    /// Shorter than the 8-byte next-USN header.
    Truncated,
    /// A record header that cannot be valid (length 0, unaligned, past the
    /// buffer, shorter than its version's fixed part).
    BadRecord,
}

impl std::fmt::Display for UsnParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UsnParseError::Truncated => write!(f, "USN buffer truncated"),
            UsnParseError::BadRecord => write!(f, "malformed USN record"),
        }
    }
}

/// One parsed read: the position to continue from and the changes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UsnBatch {
    pub next_usn: i64,
    pub changes: Vec<UsnChange>,
    /// Records of a version this parser does not use (skipped).
    pub skipped: u64,
}

const V2_FIXED: usize = 0x3C;
const V3_FIXED: usize = 0x4C;

fn u16_at(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        b.get(o..o.checked_add(2)?)?.try_into().ok()?,
    ))
}
fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        b.get(o..o.checked_add(4)?)?.try_into().ok()?,
    ))
}
fn u64_at(b: &[u8], o: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        b.get(o..o.checked_add(8)?)?.try_into().ok()?,
    ))
}

/// Check the name fields (`len` bytes at `off`) lie inside the record —
/// the name is not decoded, but a record whose own fields disagree with its
/// length is malformed.
fn name_fits(rec: &[u8], fixed: usize, len_at: usize, off_at: usize) -> bool {
    let (Some(len), Some(off)) = (u16_at(rec, len_at), u16_at(rec, off_at)) else {
        return false;
    };
    let (len, off) = (len as usize, off as usize);
    len % 2 == 0 && off >= fixed && off + len <= rec.len()
}

/// Parse one `FSCTL_READ_USN_JOURNAL` output buffer (exactly the bytes the
/// call returned).
pub fn parse_read_buffer(buf: &[u8]) -> Result<UsnBatch, UsnParseError> {
    let next_usn = u64_at(buf, 0).ok_or(UsnParseError::Truncated)? as i64;
    let mut out = UsnBatch {
        next_usn,
        ..Default::default()
    };
    let mut pos = 8usize;
    while pos < buf.len() {
        let len = u32_at(buf, pos).ok_or(UsnParseError::BadRecord)? as usize;
        let major = u16_at(buf, pos + 4).ok_or(UsnParseError::BadRecord)?;
        if len < 8 || !len.is_multiple_of(8) || pos + len > buf.len() {
            return Err(UsnParseError::BadRecord);
        }
        let rec = &buf[pos..pos + len];
        match major {
            2 => {
                if len < V2_FIXED || !name_fits(rec, V2_FIXED, 0x38, 0x3A) {
                    return Err(UsnParseError::BadRecord);
                }
                out.changes.push(UsnChange {
                    frn: u64_at(rec, 0x08).ok_or(UsnParseError::BadRecord)?,
                    parent: u64_at(rec, 0x10).ok_or(UsnParseError::BadRecord)?,
                    usn: u64_at(rec, 0x18).ok_or(UsnParseError::BadRecord)? as i64,
                    reason: u32_at(rec, 0x28).ok_or(UsnParseError::BadRecord)?,
                });
            }
            3 => {
                if len < V3_FIXED || !name_fits(rec, V3_FIXED, 0x48, 0x4A) {
                    return Err(UsnParseError::BadRecord);
                }
                // 128-bit FILE_ID_128; NTFS only uses the low 64 bits. A
                // non-zero high half is not an NTFS reference: skip it.
                let lo = u64_at(rec, 0x08).ok_or(UsnParseError::BadRecord)?;
                let hi = u64_at(rec, 0x10).ok_or(UsnParseError::BadRecord)?;
                if hi != 0 {
                    out.skipped += 1;
                } else {
                    let plo = u64_at(rec, 0x18).ok_or(UsnParseError::BadRecord)?;
                    let phi = u64_at(rec, 0x20).ok_or(UsnParseError::BadRecord)?;
                    out.changes.push(UsnChange {
                        frn: lo,
                        parent: if phi == 0 { plo } else { 0 },
                        usn: u64_at(rec, 0x28).ok_or(UsnParseError::BadRecord)? as i64,
                        reason: u32_at(rec, 0x38).ok_or(UsnParseError::BadRecord)?,
                    });
                }
            }
            _ => out.skipped += 1,
        }
        pos += len;
    }
    Ok(out)
}

/// Distinct record numbers touched by `changes` with a relevant reason, in
/// order of first appearance, with the union of their reasons.
pub fn touched_records(changes: &[UsnChange]) -> Vec<(u64, u32)> {
    let mut order: Vec<(u64, u32)> = Vec::new();
    let mut at: std::collections::HashMap<u64, usize> = std::collections::HashMap::new();
    for c in changes {
        if c.reason & reason::RELEVANT == 0 {
            continue;
        }
        let rec = c.frn & crate::index::RECORD_MASK;
        match at.get(&rec) {
            Some(&i) => order[i].1 |= c.reason,
            None => {
                at.insert(rec, order.len());
                order.push((rec, c.reason));
            }
        }
    }
    order
}

/// Test/fixture helper: encode records the way the kernel lays them out.
#[cfg(test)]
pub(crate) mod fixture {
    /// One `USN_RECORD_V2` with a name.
    pub fn v2(frn: u64, parent: u64, usn: i64, reason: u32, name: &str) -> Vec<u8> {
        let units: Vec<u16> = name.encode_utf16().collect();
        let len = (0x3C + units.len() * 2).next_multiple_of(8);
        let mut r = vec![0u8; len];
        r[0..4].copy_from_slice(&(len as u32).to_le_bytes());
        r[4..6].copy_from_slice(&2u16.to_le_bytes());
        r[0x08..0x10].copy_from_slice(&frn.to_le_bytes());
        r[0x10..0x18].copy_from_slice(&parent.to_le_bytes());
        r[0x18..0x20].copy_from_slice(&usn.to_le_bytes());
        r[0x28..0x2C].copy_from_slice(&reason.to_le_bytes());
        r[0x38..0x3A].copy_from_slice(&((units.len() * 2) as u16).to_le_bytes());
        r[0x3A..0x3C].copy_from_slice(&0x3Cu16.to_le_bytes());
        for (i, u) in units.iter().enumerate() {
            r[0x3C + 2 * i..0x3E + 2 * i].copy_from_slice(&u.to_le_bytes());
        }
        r
    }

    /// A full read buffer.
    pub fn buffer(next: i64, records: &[Vec<u8>]) -> Vec<u8> {
        let mut b = next.to_le_bytes().to_vec();
        for r in records {
            b.extend_from_slice(r);
        }
        b
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::*;
    use super::*;

    #[test]
    fn parses_v2_and_v3_and_skips_others() {
        let mut v3 = vec![0u8; 0x50];
        v3[0..4].copy_from_slice(&0x50u32.to_le_bytes());
        v3[4..6].copy_from_slice(&3u16.to_le_bytes());
        v3[0x08..0x10].copy_from_slice(&77u64.to_le_bytes());
        v3[0x18..0x20].copy_from_slice(&6u64.to_le_bytes());
        v3[0x28..0x30].copy_from_slice(&500i64.to_le_bytes());
        v3[0x38..0x3C].copy_from_slice(&reason::FILE_DELETE.to_le_bytes());
        v3[0x4A..0x4C].copy_from_slice(&0x4Cu16.to_le_bytes());
        let mut v3_hi = v3.clone();
        v3_hi[0x10] = 1;
        let mut v4 = vec![0u8; 0x40];
        v4[0..4].copy_from_slice(&0x40u32.to_le_bytes());
        v4[4..6].copy_from_slice(&4u16.to_le_bytes());
        let buf = buffer(
            999,
            &[v2(42, 5, 100, reason::FILE_CREATE, "a.txt"), v3, v3_hi, v4],
        );
        let b = parse_read_buffer(&buf).unwrap();
        assert_eq!(b.next_usn, 999);
        assert_eq!(
            b.changes,
            vec![
                UsnChange {
                    usn: 100,
                    frn: 42,
                    parent: 5,
                    reason: reason::FILE_CREATE
                },
                UsnChange {
                    usn: 500,
                    frn: 77,
                    parent: 6,
                    reason: reason::FILE_DELETE
                },
            ]
        );
        assert_eq!(b.skipped, 2);
    }

    #[test]
    fn header_only_is_an_empty_batch() {
        let b = parse_read_buffer(&123i64.to_le_bytes()).unwrap();
        assert_eq!((b.next_usn, b.changes.len()), (123, 0));
        assert_eq!(parse_read_buffer(&[1, 2, 3]), Err(UsnParseError::Truncated));
    }

    #[test]
    fn malformed_records_are_rejected() {
        let good = v2(42, 5, 100, reason::FILE_CREATE, "a.txt");
        // Zero length (would loop), unaligned, past the end, too short.
        for (at, val) in [(0usize, 0u32), (0, 0x41), (0, 0x1000), (0, 0x38)] {
            let mut r = good.clone();
            r[at..at + 4].copy_from_slice(&val.to_le_bytes());
            assert_eq!(
                parse_read_buffer(&buffer(1, &[r])),
                Err(UsnParseError::BadRecord),
                "{val:#x}"
            );
        }
        // Name past the record, odd name length, name inside the header.
        for (at, val) in [(0x38usize, 0x200u16), (0x38, 3), (0x3A, 0x10)] {
            let mut r = good.clone();
            r[at..at + 2].copy_from_slice(&val.to_le_bytes());
            assert_eq!(
                parse_read_buffer(&buffer(1, &[r])),
                Err(UsnParseError::BadRecord)
            );
        }
        // Trailing bytes too short for a record header.
        let mut b = buffer(1, &[good]);
        b.extend_from_slice(&[0, 0, 0]);
        assert_eq!(parse_read_buffer(&b), Err(UsnParseError::BadRecord));
    }

    #[test]
    fn touched_records_dedupe_in_order() {
        let ch = |frn: u64, reason: u32| UsnChange {
            usn: 0,
            frn,
            parent: 5,
            reason,
        };
        let t = touched_records(&[
            ch(30 | (1 << 48), reason::FILE_CREATE),
            ch(31, reason::DATA_EXTEND),
            ch(30 | (2 << 48), reason::CLOSE),
            ch(32, reason::SECURITY_CHANGE | reason::INDEXABLE_CHANGE),
        ]);
        assert_eq!(
            t,
            vec![
                (30, reason::FILE_CREATE | reason::CLOSE),
                (31, reason::DATA_EXTEND)
            ]
        );
    }

    /// Deterministic fuzz: garbage never panics or loops.
    #[test]
    fn survives_garbage() {
        let seed = buffer(
            7,
            &[
                v2(42, 5, 100, reason::FILE_CREATE, "a.txt"),
                v2(43, 5, 101, reason::FILE_DELETE, "bb"),
            ],
        );
        let mut s = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        };
        for _ in 0..20_000 {
            let mut b = seed.clone();
            for _ in 0..(1 + next() % 6) {
                let at = (next() as usize) % b.len();
                b[at] = next() as u8;
            }
            let cut = (next() as usize) % (b.len() + 1);
            let _ = parse_read_buffer(&b[..cut]);
        }
    }
}
