//! Framed wire protocol between `ferrum-scan-helper.exe` (elevated client)
//! and Ferrum (non-elevated pipe server). FER-460.
//!
//! Frame = `len: u32 LE` + `kind: u8` + payload, where `len` counts the kind
//! byte plus the payload and must be in `1..=MAX_FRAME_LEN`. Everything is
//! little-endian. The DECODER is the trust boundary on Ferrum's side: every
//! length, count, enum byte and name is checked, a frame with trailing bytes
//! is rejected, and nothing is allocated before its size has been bounded.
//!
//! Conversation (see `docs/disk-usage-contract.md` §7):
//! 0. the helper verifies the pipe SERVER first (image in the helper's own
//!    directory, same user SID as the helper's token) before writing.
//! 1. client → server `ClientHello` (magic + version only, no data). Ferrum
//!    needs one read before it can impersonate the client; it then checks
//!    the client TOKEN (elevated, same user SID) and PID, and drops the
//!    client otherwise — before sending anything.
//! 2. server → client `ServerHello { nonce }` — the helper compares the
//!    nonce with the one in its argv (constant time) and exits on mismatch,
//!    so a squatting server never receives any data.
//! 3. client → server `Volume` (geometry), then any number of `Progress` /
//!    `Batch`.
//! 4. client → server exactly one `Done` or `Error`, then the helper exits.
//!
//! Cancel = the server closes the pipe; the helper's next write fails and it
//! exits. There is no server→client message after `ServerHello`.

use std::io::{self, Read, Write};

/// `FRSC` — Ferrum Scan.
pub const MAGIC: [u8; 4] = *b"FRSC";
/// v2 (FER-472): every entry also carries `modified` (see [`WireEntry`]).
pub const VERSION: u16 = 2;
/// Hard cap on one frame (kind byte + payload).
pub const MAX_FRAME_LEN: u32 = 1024 * 1024;
/// Size the helper aims for per batch frame (well under the cap).
pub const TARGET_BATCH_BYTES: usize = 256 * 1024;
/// Entries per batch frame, upper bound.
pub const MAX_BATCH_ENTRIES: u32 = 16_384;
/// NTFS file names are at most 255 UTF-16 code units.
pub const MAX_NAME_UNITS: usize = 255;
/// Error message cap (bytes of UTF-8).
pub const MAX_ERROR_MSG: usize = 512;
pub const NONCE_LEN: usize = 32;

const KIND_SERVER_HELLO: u8 = 1;
const KIND_CLIENT_HELLO: u8 = 2;
const KIND_PROGRESS: u8 = 3;
const KIND_BATCH: u8 = 4;
const KIND_DONE: u8 = 5;
const KIND_ERROR: u8 = 6;
const KIND_VOLUME: u8 = 7;

/// Fixed part of one batch entry: frn u64 + parent u64 + kind u8 +
/// logical u64 + allocated u64 + modified u64 + name_units u16.
const ENTRY_FIXED: usize = 8 + 8 + 1 + 8 + 8 + 8 + 2;

/// Wire value meaning "allocation unknown".
pub const ALLOCATED_UNKNOWN: u64 = u64::MAX;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File = 0,
    Dir = 1,
    /// Name-surrogate reparse point (symlink, junction, mount point):
    /// counted, never followed (D07).
    Symlink = 2,
}

impl EntryKind {
    fn from_byte(b: u8) -> Option<Self> {
        match b {
            0 => Some(Self::File),
            1 => Some(Self::Dir),
            2 => Some(Self::Symlink),
            _ => None,
        }
    }
}

/// One (file record, name) pair. A hardlinked file yields one entry per
/// name (footprint semantics, contract §2.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireEntry {
    /// NTFS file reference of this record: record number (low 48 bits) +
    /// sequence number (high 16 bits).
    pub frn: u64,
    /// File reference of the parent directory, as stored in `$FILE_NAME`.
    pub parent: u64,
    pub kind: EntryKind,
    pub logical: u64,
    /// `None` = unknown (never substituted with logical, gap G2).
    pub allocated: Option<u64>,
    /// Last-write time as a Windows FILETIME (100 ns ticks since 1601-01-01
    /// UTC), from `$STANDARD_INFORMATION`. `0` = unknown. Added in protocol
    /// v2 (FER-472) so a sync can compare mtimes without a stat per file.
    pub modified: u64,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeInfo {
    pub bytes_per_cluster: u32,
    pub bytes_per_record: u32,
    pub total_records: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoneInfo {
    /// File reference of the root directory (record 5 + its sequence).
    pub root_frn: u64,
    pub records_scanned: u64,
    /// In-use records whose fixups/structure were invalid, plus names that
    /// were not valid Win32 names (`:`, control chars, …) — all skipped and
    /// reported so Ferrum marks coverage `partial`.
    pub corrupt_records: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    ServerHello {
        nonce: [u8; NONCE_LEN],
    },
    /// Magic + version only; carries no data (see the conversation above).
    ClientHello,
    Volume(VolumeInfo),
    Progress {
        done: u64,
        total: u64,
    },
    Batch(Vec<WireEntry>),
    Done(DoneInfo),
    Error {
        code: u16,
        message: String,
    },
}

/// Helper-side error codes carried by `Message::Error`.
pub mod error_code {
    pub const NOT_NTFS: u16 = 1;
    pub const VOLUME_OPEN: u16 = 2;
    pub const READ: u16 = 3;
    pub const MFT_LAYOUT: u16 = 4;
    pub const INTERNAL: u16 = 5;
}

#[derive(Debug, PartialEq, Eq)]
pub enum ProtocolError {
    /// The peer closed the pipe at a frame boundary.
    Eof,
    Io(String),
    FrameTooLarge(u32),
    EmptyFrame,
    UnknownKind(u8),
    /// Structural problem inside a frame (truncated, trailing bytes, bad
    /// count, bad enum, invalid name, …).
    Malformed(&'static str),
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Eof => write!(f, "pipe closed"),
            Self::Io(e) => write!(f, "pipe I/O: {e}"),
            Self::FrameTooLarge(n) => write!(f, "frame too large ({n} bytes)"),
            Self::EmptyFrame => write!(f, "empty frame"),
            Self::UnknownKind(k) => write!(f, "unknown frame kind {k}"),
            Self::Malformed(why) => write!(f, "malformed frame: {why}"),
        }
    }
}

impl std::error::Error for ProtocolError {}

// ── encoding (helper side, and tests) ────────────────────────────────────

fn put_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn put_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// Encoded size of one entry (for batch sizing).
pub fn entry_wire_len(e: &WireEntry) -> usize {
    ENTRY_FIXED + e.name.encode_utf16().count() * 2
}

/// Serialize one message as a complete frame. Panics only on programmer
/// error (a batch above the caps), which the helper's batcher never builds.
pub fn encode(msg: &Message) -> Vec<u8> {
    let mut body = Vec::new();
    match msg {
        Message::ServerHello { nonce } => {
            body.push(KIND_SERVER_HELLO);
            body.extend_from_slice(&MAGIC);
            put_u16(&mut body, VERSION);
            body.extend_from_slice(nonce);
        }
        Message::ClientHello => {
            body.push(KIND_CLIENT_HELLO);
            body.extend_from_slice(&MAGIC);
            put_u16(&mut body, VERSION);
        }
        Message::Volume(h) => {
            body.push(KIND_VOLUME);
            put_u32(&mut body, h.bytes_per_cluster);
            put_u32(&mut body, h.bytes_per_record);
            put_u64(&mut body, h.total_records);
        }
        Message::Progress { done, total } => {
            body.push(KIND_PROGRESS);
            put_u64(&mut body, *done);
            put_u64(&mut body, *total);
        }
        Message::Batch(entries) => {
            assert!(entries.len() as u64 <= MAX_BATCH_ENTRIES as u64);
            body.push(KIND_BATCH);
            put_u32(&mut body, entries.len() as u32);
            for e in entries {
                put_u64(&mut body, e.frn);
                put_u64(&mut body, e.parent);
                body.push(e.kind as u8);
                put_u64(&mut body, e.logical);
                put_u64(&mut body, e.allocated.unwrap_or(ALLOCATED_UNKNOWN));
                put_u64(&mut body, e.modified);
                let units: Vec<u16> = e.name.encode_utf16().collect();
                assert!(units.len() <= MAX_NAME_UNITS);
                put_u16(&mut body, units.len() as u16);
                for u in units {
                    put_u16(&mut body, u);
                }
            }
        }
        Message::Done(d) => {
            body.push(KIND_DONE);
            put_u64(&mut body, d.root_frn);
            put_u64(&mut body, d.records_scanned);
            put_u64(&mut body, d.corrupt_records);
        }
        Message::Error { code, message } => {
            body.push(KIND_ERROR);
            put_u16(&mut body, *code);
            let mut m = message.as_bytes();
            if m.len() > MAX_ERROR_MSG {
                // Cut on a char boundary.
                let mut cut = MAX_ERROR_MSG;
                while !message.is_char_boundary(cut) {
                    cut -= 1;
                }
                m = &message.as_bytes()[..cut];
            }
            put_u16(&mut body, m.len() as u16);
            body.extend_from_slice(m);
        }
    }
    assert!(body.len() as u64 <= MAX_FRAME_LEN as u64);
    let mut frame = Vec::with_capacity(4 + body.len());
    put_u32(&mut frame, body.len() as u32);
    frame.extend_from_slice(&body);
    frame
}

pub fn write_message(w: &mut impl Write, msg: &Message) -> io::Result<()> {
    w.write_all(&encode(msg))
}

// ── decoding (Ferrum side: untrusted input) ──────────────────────────────

struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], ProtocolError> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or(ProtocolError::Malformed("length overflow"))?;
        if end > self.buf.len() {
            return Err(ProtocolError::Malformed("truncated"));
        }
        let s = &self.buf[self.pos..end];
        self.pos = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, ProtocolError> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, ProtocolError> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, ProtocolError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, ProtocolError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }
    fn finish(&self) -> Result<(), ProtocolError> {
        if self.remaining() == 0 {
            Ok(())
        } else {
            Err(ProtocolError::Malformed("trailing bytes"))
        }
    }
}

fn check_magic(c: &mut Cursor<'_>) -> Result<(), ProtocolError> {
    if c.take(4)? != MAGIC {
        return Err(ProtocolError::Malformed("bad magic"));
    }
    if c.u16()? != VERSION {
        return Err(ProtocolError::Malformed("unsupported version"));
    }
    Ok(())
}

/// UTF-16LE code units of `raw` (an odd trailing byte is ignored).
pub fn le_units(raw: &[u8]) -> impl Iterator<Item = u16> + '_ {
    raw.as_chunks::<2>()
        .0
        .iter()
        .map(|p| u16::from_le_bytes(*p))
}

/// A name is acceptable when it is non-empty, at most 255 units, and could
/// never be confused with path structure: no `\`, `/` or NUL, and not `.`
/// or `..`. NTFS itself forbids all of these in real entries; seeing one
/// means the stream is not what the helper is supposed to send.
pub fn valid_name_units(units: &[u16]) -> bool {
    if units.is_empty() || units.len() > MAX_NAME_UNITS {
        return false;
    }
    // `:` would turn `C:\dir\a:b` into the ADS `b` of file `a` for every
    // later file action; control characters are never valid Win32 names.
    if units
        .iter()
        .any(|&u| u < 0x20 || u == b'\\' as u16 || u == b'/' as u16 || u == b':' as u16)
    {
        return false;
    }
    !(units == [b'.' as u16] || units == [b'.' as u16, b'.' as u16])
}

/// Decode one frame body (kind byte + payload). Strict: every field is
/// bounds-checked and trailing bytes are an error.
pub fn decode_body(body: &[u8]) -> Result<Message, ProtocolError> {
    let mut c = Cursor { buf: body, pos: 0 };
    let kind = c.u8().map_err(|_| ProtocolError::EmptyFrame)?;
    let msg = match kind {
        KIND_SERVER_HELLO => {
            check_magic(&mut c)?;
            let mut nonce = [0u8; NONCE_LEN];
            nonce.copy_from_slice(c.take(NONCE_LEN)?);
            Message::ServerHello { nonce }
        }
        KIND_CLIENT_HELLO => {
            check_magic(&mut c)?;
            Message::ClientHello
        }
        KIND_VOLUME => {
            let bytes_per_cluster = c.u32()?;
            let bytes_per_record = c.u32()?;
            let total_records = c.u64()?;
            if !(512..=(2 * 1024 * 1024)).contains(&bytes_per_cluster)
                || !bytes_per_cluster.is_power_of_two()
                || !(256..=65536).contains(&bytes_per_record)
                || !bytes_per_record.is_power_of_two()
            {
                return Err(ProtocolError::Malformed("implausible volume geometry"));
            }
            Message::Volume(VolumeInfo {
                bytes_per_cluster,
                bytes_per_record,
                total_records,
            })
        }
        KIND_PROGRESS => {
            let done = c.u64()?;
            let total = c.u64()?;
            Message::Progress { done, total }
        }
        KIND_BATCH => {
            let count = c.u32()?;
            if count > MAX_BATCH_ENTRIES {
                return Err(ProtocolError::Malformed("batch count above cap"));
            }
            // Every entry needs at least ENTRY_FIXED + 2 bytes: reject a
            // count the payload cannot possibly hold BEFORE allocating.
            if (count as usize).saturating_mul(ENTRY_FIXED + 2) > c.remaining() {
                return Err(ProtocolError::Malformed("batch count exceeds payload"));
            }
            let mut entries = Vec::with_capacity(count as usize);
            let mut units = Vec::with_capacity(MAX_NAME_UNITS);
            for _ in 0..count {
                let frn = c.u64()?;
                let parent = c.u64()?;
                let kind = EntryKind::from_byte(c.u8()?)
                    .ok_or(ProtocolError::Malformed("bad entry kind"))?;
                let logical = c.u64()?;
                let allocated = match c.u64()? {
                    ALLOCATED_UNKNOWN => None,
                    v => Some(v),
                };
                // Any u64 is a valid FILETIME (0 = unknown); the consumer
                // converts it with saturating arithmetic.
                let modified = c.u64()?;
                let n = c.u16()? as usize;
                if n == 0 || n > MAX_NAME_UNITS {
                    return Err(ProtocolError::Malformed("bad name length"));
                }
                let raw = c.take(n * 2)?;
                units.clear();
                units.extend(le_units(raw));
                if !valid_name_units(&units) {
                    return Err(ProtocolError::Malformed("invalid entry name"));
                }
                if kind == EntryKind::Dir && (logical != 0 || allocated.is_some_and(|a| a != 0)) {
                    return Err(ProtocolError::Malformed("directory with bytes"));
                }
                if frn == parent {
                    return Err(ProtocolError::Malformed("entry is its own parent"));
                }
                entries.push(WireEntry {
                    frn,
                    parent,
                    kind,
                    logical,
                    allocated,
                    modified,
                    // Unpaired surrogates become U+FFFD — the same lossy
                    // rule the regular walker applies (`to_string_lossy`).
                    name: String::from_utf16_lossy(&units),
                });
            }
            Message::Batch(entries)
        }
        KIND_DONE => {
            let root_frn = c.u64()?;
            let records_scanned = c.u64()?;
            let corrupt_records = c.u64()?;
            Message::Done(DoneInfo {
                root_frn,
                records_scanned,
                corrupt_records,
            })
        }
        KIND_ERROR => {
            let code = c.u16()?;
            let n = c.u16()? as usize;
            if n > MAX_ERROR_MSG {
                return Err(ProtocolError::Malformed("error message too long"));
            }
            let message = String::from_utf8_lossy(c.take(n)?).into_owned();
            Message::Error { code, message }
        }
        other => return Err(ProtocolError::UnknownKind(other)),
    };
    c.finish()?;
    Ok(msg)
}

/// Read exactly one frame from `r`. A clean EOF before the first header
/// byte is `ProtocolError::Eof`; EOF inside a frame is `Malformed`.
pub fn read_message(r: &mut impl Read) -> Result<Message, ProtocolError> {
    let mut hdr = [0u8; 4];
    let mut got = 0;
    while got < 4 {
        match r.read(&mut hdr[got..]) {
            Ok(0) if got == 0 => return Err(ProtocolError::Eof),
            Ok(0) => return Err(ProtocolError::Malformed("truncated header")),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) if e.kind() == io::ErrorKind::BrokenPipe && got == 0 => {
                return Err(ProtocolError::Eof);
            }
            Err(e) => return Err(ProtocolError::Io(e.to_string())),
        }
    }
    let len = u32::from_le_bytes(hdr);
    if len == 0 {
        return Err(ProtocolError::EmptyFrame);
    }
    if len > MAX_FRAME_LEN {
        return Err(ProtocolError::FrameTooLarge(len));
    }
    let mut body = vec![0u8; len as usize];
    r.read_exact(&mut body).map_err(|e| {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            ProtocolError::Malformed("truncated body")
        } else {
            ProtocolError::Io(e.to_string())
        }
    })?;
    decode_body(&body)
}

/// Constant-time comparison for the hello nonce.
pub fn nonce_eq(a: &[u8; NONCE_LEN], b: &[u8; NONCE_LEN]) -> bool {
    let mut diff = 0u8;
    for i in 0..NONCE_LEN {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str) -> WireEntry {
        WireEntry {
            frn: 100 | (3 << 48),
            parent: 5 | (5 << 48),
            kind: EntryKind::File,
            logical: 1234,
            allocated: Some(4096),
            modified: 133_000_000_000_000_000,
            name: name.into(),
        }
    }

    fn roundtrip(m: Message) {
        let frame = encode(&m);
        let got = read_message(&mut &frame[..]).expect("decode");
        assert_eq!(got, m);
    }

    #[test]
    fn all_messages_roundtrip() {
        roundtrip(Message::ServerHello { nonce: [7; 32] });
        roundtrip(Message::ClientHello);
        roundtrip(Message::Volume(VolumeInfo {
            bytes_per_cluster: 4096,
            bytes_per_record: 1024,
            total_records: 4_500_000,
        }));
        roundtrip(Message::Progress {
            done: 10,
            total: 20,
        });
        let mut unknown = entry("ünïcode 名前.txt");
        unknown.allocated = None;
        roundtrip(Message::Batch(vec![entry("a.txt"), unknown]));
        roundtrip(Message::Batch(Vec::new()));
        roundtrip(Message::Done(DoneInfo {
            root_frn: 5 | (5 << 48),
            records_scanned: 9,
            corrupt_records: 1,
        }));
        roundtrip(Message::Error {
            code: error_code::NOT_NTFS,
            message: "nope".into(),
        });
    }

    #[test]
    fn oversized_frame_is_rejected_before_allocation() {
        let mut frame = (MAX_FRAME_LEN + 1).to_le_bytes().to_vec();
        frame.push(KIND_PROGRESS);
        assert_eq!(
            read_message(&mut &frame[..]),
            Err(ProtocolError::FrameTooLarge(MAX_FRAME_LEN + 1))
        );
        let huge = u32::MAX.to_le_bytes();
        assert!(matches!(
            read_message(&mut &huge[..]),
            Err(ProtocolError::FrameTooLarge(_))
        ));
    }

    #[test]
    fn empty_and_unknown_frames_are_rejected() {
        let zero = 0u32.to_le_bytes();
        assert_eq!(read_message(&mut &zero[..]), Err(ProtocolError::EmptyFrame));
        let mut unk = 1u32.to_le_bytes().to_vec();
        unk.push(99);
        assert_eq!(
            read_message(&mut &unk[..]),
            Err(ProtocolError::UnknownKind(99))
        );
    }

    #[test]
    fn eof_cases() {
        assert_eq!(read_message(&mut &[][..]), Err(ProtocolError::Eof));
        assert!(matches!(
            read_message(&mut &[1u8, 0][..]),
            Err(ProtocolError::Malformed(_))
        ));
        let mut frame = encode(&Message::Progress { done: 1, total: 2 });
        frame.truncate(frame.len() - 3);
        assert_eq!(
            read_message(&mut &frame[..]),
            Err(ProtocolError::Malformed("truncated body"))
        );
    }

    #[test]
    fn trailing_bytes_are_rejected() {
        let mut body = encode(&Message::Progress { done: 1, total: 2 })[4..].to_vec();
        body.push(0);
        assert_eq!(
            decode_body(&body),
            Err(ProtocolError::Malformed("trailing bytes"))
        );
    }

    #[test]
    fn batch_count_lies_are_rejected() {
        // Count far above what the payload holds.
        let mut body = vec![KIND_BATCH];
        body.extend_from_slice(&10_000u32.to_le_bytes());
        assert_eq!(
            decode_body(&body),
            Err(ProtocolError::Malformed("batch count exceeds payload"))
        );
        // Count above the cap.
        let mut body = vec![KIND_BATCH];
        body.extend_from_slice(&(MAX_BATCH_ENTRIES + 1).to_le_bytes());
        assert_eq!(
            decode_body(&body),
            Err(ProtocolError::Malformed("batch count above cap"))
        );
    }

    fn batch_body_with_name(units: &[u16], kind: u8) -> Vec<u8> {
        let mut body = vec![KIND_BATCH];
        body.extend_from_slice(&1u32.to_le_bytes());
        body.extend_from_slice(&100u64.to_le_bytes());
        body.extend_from_slice(&5u64.to_le_bytes());
        body.push(kind);
        body.extend_from_slice(&0u64.to_le_bytes()); // logical
        body.extend_from_slice(&0u64.to_le_bytes()); // allocated
        body.extend_from_slice(&0u64.to_le_bytes()); // modified (v2)
        body.extend_from_slice(&(units.len() as u16).to_le_bytes());
        for u in units {
            body.extend_from_slice(&u.to_le_bytes());
        }
        body
    }

    #[test]
    fn hostile_names_are_rejected() {
        let enc = |s: &str| s.encode_utf16().collect::<Vec<_>>();
        for bad in [
            "..",
            ".",
            "a\\b",
            "a/b",
            "a\0b",
            "file.txt:secret",
            "tab\there",
            "\u{1f}x",
        ] {
            assert_eq!(
                decode_body(&batch_body_with_name(&enc(bad), 0)),
                Err(ProtocolError::Malformed("invalid entry name")),
                "{bad:?}"
            );
        }
        // Zero-length name (padded so the count pre-check passes).
        let mut empty = batch_body_with_name(&[], 0);
        empty.extend_from_slice(&[0, 0]);
        assert_eq!(
            decode_body(&empty),
            Err(ProtocolError::Malformed("bad name length"))
        );
        let long = vec![b'a' as u16; MAX_NAME_UNITS + 1];
        assert_eq!(
            decode_body(&batch_body_with_name(&long, 0)),
            Err(ProtocolError::Malformed("bad name length"))
        );
        assert!(decode_body(&batch_body_with_name(&enc("ok"), 0)).is_ok());
        // Unpaired surrogate: accepted, decoded lossily.
        let Message::Batch(b) = decode_body(&batch_body_with_name(&[0xD800, 0x41], 0)).unwrap()
        else {
            panic!()
        };
        assert_eq!(b[0].name, "\u{FFFD}A");
    }

    #[test]
    fn bad_kind_and_dir_bytes_are_rejected() {
        let units: Vec<u16> = "x".encode_utf16().collect();
        assert_eq!(
            decode_body(&batch_body_with_name(&units, 7)),
            Err(ProtocolError::Malformed("bad entry kind"))
        );
        let mut e = entry("d");
        e.kind = EntryKind::Dir;
        let body = encode(&Message::Batch(vec![e]))[4..].to_vec();
        assert_eq!(
            decode_body(&body),
            Err(ProtocolError::Malformed("directory with bytes"))
        );
    }

    #[test]
    fn modified_time_roundtrips_and_any_value_is_accepted() {
        for m in [0u64, 1, 133_000_000_000_000_000, u64::MAX] {
            let mut e = entry("t.txt");
            e.modified = m;
            roundtrip(Message::Batch(vec![e]));
        }
    }

    #[test]
    fn an_old_v1_peer_is_refused_at_the_hello() {
        let mut body = encode(&Message::ClientHello)[4..].to_vec();
        body[5..7].copy_from_slice(&1u16.to_le_bytes());
        assert!(decode_body(&body).is_err());
    }

    #[test]
    fn self_parent_is_rejected() {
        let mut e = entry("loop");
        e.parent = e.frn;
        let body = encode(&Message::Batch(vec![e]))[4..].to_vec();
        assert_eq!(
            decode_body(&body),
            Err(ProtocolError::Malformed("entry is its own parent"))
        );
    }

    #[test]
    fn hello_checks_magic_version_and_geometry() {
        let mut body = encode(&Message::ServerHello { nonce: [1; 32] })[4..].to_vec();
        body[1] = b'X';
        assert_eq!(
            decode_body(&body),
            Err(ProtocolError::Malformed("bad magic"))
        );
        let mut body = encode(&Message::ServerHello { nonce: [1; 32] })[4..].to_vec();
        body[5] = 9;
        assert_eq!(
            decode_body(&body),
            Err(ProtocolError::Malformed("unsupported version"))
        );
        let body = encode(&Message::Volume(VolumeInfo {
            bytes_per_cluster: 3000,
            bytes_per_record: 1024,
            total_records: 1,
        }))[4..]
            .to_vec();
        assert_eq!(
            decode_body(&body),
            Err(ProtocolError::Malformed("implausible volume geometry"))
        );
    }

    #[test]
    fn error_message_is_bounded() {
        let long = "é".repeat(1000);
        let frame = encode(&Message::Error {
            code: 1,
            message: long,
        });
        let Message::Error { message, .. } = read_message(&mut &frame[..]).unwrap() else {
            panic!()
        };
        assert!(message.len() <= MAX_ERROR_MSG);
        assert!(message.chars().all(|c| c == 'é'));
    }

    #[test]
    fn nonce_compare() {
        assert!(nonce_eq(&[3; 32], &[3; 32]));
        let mut b = [3; 32];
        b[31] = 4;
        assert!(!nonce_eq(&[3; 32], &b));
    }

    #[test]
    fn many_frames_in_sequence() {
        let mut stream = Vec::new();
        for i in 0..50u64 {
            stream.extend(encode(&Message::Progress { done: i, total: 50 }));
        }
        let mut r = &stream[..];
        for i in 0..50u64 {
            assert_eq!(
                read_message(&mut r).unwrap(),
                Message::Progress { done: i, total: 50 }
            );
        }
        assert_eq!(read_message(&mut r), Err(ProtocolError::Eof));
    }

    /// FER-463 I3 (deterministic fuzz): `decode_body` on mutated, truncated
    /// and random bodies never panics, and whatever it accepts is stable
    /// (re-encoding it decodes to the same message).
    #[test]
    fn decode_body_survives_garbage() {
        let seeds: Vec<Vec<u8>> = [
            Message::ServerHello { nonce: [7; 32] },
            Message::ClientHello,
            Message::Volume(VolumeInfo {
                bytes_per_cluster: 4096,
                bytes_per_record: 1024,
                total_records: 4_500_000,
            }),
            Message::Progress {
                done: 10,
                total: 20,
            },
            Message::Batch(vec![entry("a.txt"), entry("b \u{540d}.bin")]),
            Message::Done(DoneInfo {
                root_frn: 5 | (5 << 48),
                records_scanned: 9,
                corrupt_records: 1,
            }),
            Message::Error {
                code: error_code::NOT_NTFS,
                message: "nope".into(),
            },
        ]
        .iter()
        .map(|m| encode(m)[4..].to_vec())
        .collect();
        let mut s = 0xA076_1D64_78BD_642Fu64;
        let mut next = move || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        };
        for _ in 0..50_000 {
            let mut body = seeds[(next() % seeds.len() as u64) as usize].clone();
            match next() % 4 {
                0 => body = (0..next() % 96).map(|_| next() as u8).collect(),
                1 => body.truncate((next() as usize) % (body.len() + 1)),
                2 => body.extend((0..1 + next() % 8).map(|_| next() as u8)),
                _ => {
                    for _ in 0..(1 + next() % 6) {
                        let at = (next() as usize) % body.len();
                        body[at] = next() as u8;
                    }
                }
            }
            // Text (names, the error message) is decoded lossily by design
            // (an unpaired surrogate becomes U+FFFD), so the check is that a
            // decoded message is stable: encoding it decodes to itself.
            if let Ok(msg) = decode_body(&body) {
                assert_eq!(decode_body(&encode(&msg)[4..]), Ok(msg.clone()), "{msg:?}");
            }
        }
    }
}
