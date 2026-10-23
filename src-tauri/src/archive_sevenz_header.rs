//! FER-587: read-only pre-check of the 7z "encoded header" (`kEncodedHeader`).
//!
//! `sevenz_rust2::ArchiveReader::new` decodes an encoded header itself, before
//! Ferrum can look at the archive. That decode (a) allocates the coder's
//! dictionary / model as declared in the coder properties, (b) allocates
//! `next_header_size` and the declared unpacked header size up front, and
//! (c) sizes several vectors from counts read out of the same bytes. All of
//! those numbers come from the (untrusted) file, so this module parses just
//! enough of the raw bytes to bound them, and refuses the archive before the
//! decoder crate runs.
//!
//! Scope and limits of the parser:
//! * It reads the 32 byte signature header and the start of the next header
//!   only. Reads are bounded (`<= MAX_ENCODED_INFO_BYTES`), every offset and
//!   length is checked against the file size with checked arithmetic, and
//!   every count is capped *before* it is used for a loop or an allocation.
//! * It never recurses: after the encoded header's `StreamsInfo` it stops.
//!   (The decoder crate does not recurse either: the decoded header must be a
//!   plain `kHeader`.)
//! * A plain (`kHeader`) next header is only size-checked here; its content
//!   is parsed by the decoder crate as before.
//! * Anything it cannot parse is refused (fail closed): the decoder crate
//!   would reject such a header too, but possibly after allocating.

use super::{ArchiveError, ArchiveErrorCode, CODEC_TOO_LARGE, declared_codec_memory};
use std::io::{Read, Seek, SeekFrom};

/// Most memory the coders of an encoded header may declare (summed over one folder).
///
/// Headers are small. 7-Zip compresses them with LZMA and a dictionary of at most 1 MiB
/// (`kDictionaryForHeaders = 1 << 20`, shrunk further for small headers), `sevenz-rust2` uses
/// the LZMA default (a few MiB) and an AES-wrapped header adds a coder with no dictionary.
/// 64 MiB is therefore more than 16x what any writer emits, yet small enough that a hostile
/// archive cannot make the open allocate anything noticeable.
pub const HEADER_CODEC_MAX_MEMORY: u64 = 64 * 1024 * 1024;

const SIGNATURE: [u8; 6] = [0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C];
const SIGNATURE_HEADER_SIZE: u64 = 32;

/// Largest next header (plain or encoded, packed size) and largest decoded header we let the
/// decoder crate allocate. 128 MiB is more than a million entries with long names.
const MAX_HEADER_BYTES: u64 = 128 * 1024 * 1024;
/// The encoded header's own `StreamsInfo` (one folder with a few coders) is tiny.
const MAX_ENCODED_INFO_BYTES: u64 = 64 * 1024;
const MAX_PACK_STREAMS: u64 = 256;
const MAX_FOLDERS: u64 = 64;
const MAX_CODERS: u64 = 32;
const MAX_STREAMS_PER_CODER: u64 = 32;
const MAX_STREAMS_PER_FOLDER: u64 = 64;
const MAX_PROP_BYTES: u64 = 256;
const MAX_SUBSTREAMS_TOTAL: u64 = 1 << 16;

const K_END: u8 = 0x00;
const K_HEADER: u8 = 0x01;
const K_PACK_INFO: u8 = 0x06;
const K_UNPACK_INFO: u8 = 0x07;
const K_SUB_STREAMS_INFO: u8 = 0x08;
const K_SIZE: u8 = 0x09;
const K_CRC: u8 = 0x0A;
const K_FOLDER: u8 = 0x0B;
const K_CODERS_UNPACK_SIZE: u8 = 0x0C;
const K_NUM_UNPACK_STREAM: u8 = 0x0D;
const K_ENCODED_HEADER: u8 = 0x17;

fn corrupt(msg: &str) -> ArchiveError {
    ArchiveError::new(ArchiveErrorCode::CorruptArchive, format!("7z: {msg}"))
}

fn too_large() -> ArchiveError {
    ArchiveError::new(ArchiveErrorCode::LimitExceeded, CODEC_TOO_LARGE)
}

fn io(e: std::io::Error) -> ArchiveError {
    if e.kind() == std::io::ErrorKind::UnexpectedEof {
        return corrupt("truncated header");
    }
    ArchiveError::new(ArchiveErrorCode::Io, e.to_string())
}

/// Refuse a 7z file whose header would make the decoder crate allocate unreasonable memory
/// while opening it. `codec_limit` bounds the summed dictionary / model memory of the coders
/// of the encoded header (production: [`HEADER_CODEC_MAX_MEMORY`]).
///
/// Position of `r` is not preserved.
pub fn precheck_encoded_header<R: Read + Seek>(
    r: &mut R,
    codec_limit: u64,
) -> Result<(), ArchiveError> {
    let len = r.seek(SeekFrom::End(0)).map_err(io)?;
    if len < SIGNATURE_HEADER_SIZE {
        return Err(corrupt("file too short"));
    }
    r.seek(SeekFrom::Start(0)).map_err(io)?;
    let mut sh = [0u8; SIGNATURE_HEADER_SIZE as usize];
    r.read_exact(&mut sh).map_err(io)?;
    if sh[..6] != SIGNATURE {
        return Err(ArchiveError::new(
            ArchiveErrorCode::UnsupportedFormat,
            "Not a 7z archive (bad signature)",
        ));
    }
    if sh[6] != 0 {
        // Unsupported major version: the decoder crate reports it without reading further.
        return Ok(());
    }
    let start_crc = u32::from_le_bytes([sh[8], sh[9], sh[10], sh[11]]);
    let start = &sh[12..32];
    if start_crc == 0 && start.iter().all(|b| *b == 0) {
        // The decoder crate would now scan the tail for something that looks like a header and
        // decode it without any checksum. Not worth recovering: refuse.
        return Err(corrupt("start header missing"));
    }
    let next_offset = u64::from_le_bytes(start[0..8].try_into().expect("8 bytes"));
    let next_size = u64::from_le_bytes(start[8..16].try_into().expect("8 bytes"));
    if next_size == 0 {
        return Ok(()); // the decoder crate rejects an empty header on its own
    }
    if next_size > MAX_HEADER_BYTES {
        return Err(ArchiveError::new(
            ArchiveErrorCode::LimitExceeded,
            "7z: header too large",
        ));
    }
    let header_start = SIGNATURE_HEADER_SIZE
        .checked_add(next_offset)
        .ok_or_else(|| corrupt("header offset out of range"))?;
    let header_end = header_start
        .checked_add(next_size)
        .ok_or_else(|| corrupt("header offset out of range"))?;
    if header_end > len {
        return Err(corrupt("header lies beyond the end of the file"));
    }
    r.seek(SeekFrom::Start(header_start)).map_err(io)?;
    let mut nid = [0u8; 1];
    r.read_exact(&mut nid).map_err(io)?;
    match nid[0] {
        K_ENCODED_HEADER => {}
        K_HEADER => return Ok(()),
        _ => return Ok(()), // the decoder crate rejects it ("no Header") without allocating
    }
    let info_len = next_size - 1;
    if info_len > MAX_ENCODED_INFO_BYTES {
        return Err(corrupt("encoded header description too large"));
    }
    let mut info = vec![0u8; info_len as usize];
    r.read_exact(&mut info).map_err(io)?;
    check_encoded_streams_info(&info, next_offset, codec_limit)
}

/// Bounds-checked cursor over the encoded header description.
struct Rd<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Rd<'a> {
    fn u8(&mut self) -> Result<u8, ArchiveError> {
        let v = *self.b.get(self.pos).ok_or_else(|| corrupt("truncated"))?;
        self.pos += 1;
        Ok(v)
    }

    /// 7z variable length number (same encoding as the decoder crate's `read_variable_u64`).
    fn num(&mut self) -> Result<u64, ArchiveError> {
        let first = self.u8()? as u64;
        let mut mask = 0x80_u64;
        let mut value = 0_u64;
        for i in 0..8 {
            if first & mask == 0 {
                return Ok(value | ((first & (mask - 1)) << (8 * i)));
            }
            value |= (self.u8()? as u64) << (8 * i);
            mask >>= 1;
        }
        Ok(value)
    }

    /// A number that must not exceed `max` (checked before the value is used for anything).
    fn num_max(&mut self, max: u64, what: &str) -> Result<u64, ArchiveError> {
        let v = self.num()?;
        if v > max {
            return Err(corrupt(&format!("{what} out of range")));
        }
        Ok(v)
    }

    fn take(&mut self, n: u64) -> Result<&'a [u8], ArchiveError> {
        let n = usize::try_from(n).map_err(|_| corrupt("truncated"))?;
        let end = self
            .pos
            .checked_add(n)
            .filter(|e| *e <= self.b.len())
            .ok_or_else(|| corrupt("truncated"))?;
        let s = &self.b[self.pos..end];
        self.pos = end;
        Ok(s)
    }

    /// "all defined" byte or a bit vector of `n` flags, followed by one u32 per defined flag.
    fn skip_defined_u32s(&mut self, n: u64) -> Result<(), ArchiveError> {
        let all = self.u8()?;
        let defined = if all != 0 {
            n
        } else {
            let bits = self.take(n.div_ceil(8))?;
            (0..n as usize)
                .filter(|i| bits[i / 8] & (0x80 >> (i % 8)) != 0)
                .count() as u64
        };
        self.take(defined.checked_mul(4).ok_or_else(|| corrupt("truncated"))?)?;
        Ok(())
    }
}

/// Parse the `StreamsInfo` that follows `kEncodedHeader` and apply the limits.
/// `pack_area` is the number of packed bytes available before the next header.
fn check_encoded_streams_info(
    buf: &[u8],
    pack_area: u64,
    codec_limit: u64,
) -> Result<(), ArchiveError> {
    let mut r = Rd { b: buf, pos: 0 };
    let mut nid = r.u8()?;

    if nid == K_PACK_INFO {
        let pack_pos = r.num()?;
        let num_pack = r.num_max(MAX_PACK_STREAMS, "pack stream count")?;
        let mut total = 0u64;
        nid = r.u8()?;
        if nid == K_SIZE {
            for _ in 0..num_pack {
                total = total
                    .checked_add(r.num()?)
                    .ok_or_else(|| corrupt("pack size overflow"))?;
            }
            nid = r.u8()?;
        }
        if nid == K_CRC {
            r.skip_defined_u32s(num_pack)?;
            nid = r.u8()?;
        }
        if nid != K_END {
            return Err(corrupt("bad pack info"));
        }
        let pack_end = pack_pos
            .checked_add(total)
            .ok_or_else(|| corrupt("pack range overflow"))?;
        if pack_end > pack_area {
            return Err(corrupt("packed header lies beyond the archive body"));
        }
        nid = r.u8()?;
    }

    if nid != K_UNPACK_INFO {
        return Err(corrupt("encoded header without folder"));
    }
    if r.u8()? != K_FOLDER {
        return Err(corrupt("expected folder list"));
    }
    let num_folders = r.num_max(MAX_FOLDERS, "folder count")?;
    if num_folders == 0 {
        return Err(corrupt("encoded header without folder"));
    }
    if r.u8()? != 0 {
        return Err(corrupt("external folder data unsupported"));
    }

    let mut out_streams: Vec<u64> = Vec::with_capacity(num_folders as usize);
    for _ in 0..num_folders {
        let num_coders = r.num_max(MAX_CODERS, "coder count")?;
        if num_coders == 0 {
            return Err(corrupt("folder without coder"));
        }
        let (mut total_in, mut total_out) = (0u64, 0u64);
        let mut memory = 0u64;
        for _ in 0..num_coders {
            let bits = r.u8()?;
            let id_size = (bits & 0x0F) as u64;
            let is_simple = bits & 0x10 == 0;
            let has_props = bits & 0x20 != 0;
            if bits & 0x80 != 0 {
                return Err(corrupt("alternative methods unsupported"));
            }
            if id_size > 4 {
                return Err(corrupt("method id too long"));
            }
            let id = r.take(id_size)?;
            let (n_in, n_out) = if is_simple {
                (1, 1)
            } else {
                (
                    r.num_max(MAX_STREAMS_PER_CODER, "coder streams")?,
                    r.num_max(MAX_STREAMS_PER_CODER, "coder streams")?,
                )
            };
            total_in += n_in; // both <= 32 * 32 coders: cannot overflow
            total_out += n_out;
            let props = if has_props {
                let n = r.num_max(MAX_PROP_BYTES, "coder properties")?;
                r.take(n)?
            } else {
                &[]
            };
            match declared_codec_memory(id, props) {
                Some(Ok(m)) => memory = memory.saturating_add(m),
                Some(Err(())) => return Err(too_large()), // unparsable props: fail closed
                None => {}
            }
            if memory > codec_limit {
                return Err(too_large());
            }
        }
        if total_out == 0 || total_out > MAX_STREAMS_PER_FOLDER || total_in > MAX_STREAMS_PER_FOLDER
        {
            return Err(corrupt("bad stream counts"));
        }
        let bind_pairs = total_out - 1;
        for _ in 0..bind_pairs {
            r.num()?;
            r.num()?;
        }
        let packed = total_in
            .checked_sub(bind_pairs)
            .ok_or_else(|| corrupt("bad stream counts"))?;
        if packed != 1 {
            for _ in 0..packed {
                r.num()?;
            }
        }
        out_streams.push(total_out);
    }

    if r.u8()? != K_CODERS_UNPACK_SIZE {
        return Err(corrupt("expected unpack sizes"));
    }
    for outs in &out_streams {
        for _ in 0..*outs {
            if r.num()? > MAX_HEADER_BYTES {
                return Err(ArchiveError::new(
                    ArchiveErrorCode::LimitExceeded,
                    "7z: header too large",
                ));
            }
        }
    }
    nid = r.u8()?;
    if nid == K_CRC {
        r.skip_defined_u32s(num_folders)?;
        nid = r.u8()?;
    }
    if nid != K_END {
        return Err(corrupt("bad unpack info"));
    }

    // Optional SubStreamsInfo: the decoder sizes vectors from the stream counts.
    nid = r.u8()?;
    if nid == K_SUB_STREAMS_INFO {
        let mut next = r.u8()?;
        if next == K_NUM_UNPACK_STREAM {
            let mut total = 0u64;
            for _ in 0..num_folders {
                total += r.num_max(MAX_SUBSTREAMS_TOTAL, "stream count")?;
                if total > MAX_SUBSTREAMS_TOTAL {
                    return Err(corrupt("stream count out of range"));
                }
            }
            next = r.u8()?;
        }
        // Remaining digests / sizes are bounded by the counts checked above.
        let _ = next;
        return Ok(());
    }
    if nid != K_END {
        return Err(corrupt("bad streams info"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const LZMA_ID: [u8; 3] = [0x03, 0x01, 0x01];
    const LZMA2_ID: [u8; 1] = [0x21];
    const AES_ID: [u8; 4] = [0x06, 0xF1, 0x07, 0x01];

    fn num(v: u64) -> Vec<u8> {
        // 7z number: `extra` leading 1 bits in the first byte announce `extra` low bytes (LE);
        // the first byte's remaining bits hold the high part.
        for extra in 0..8u32 {
            if v < 1u64 << (7 * extra + 7) {
                let prefix = (0xFF00u16 >> extra) as u8;
                let mut out = vec![prefix | (v >> (8 * extra)) as u8];
                out.extend((0..extra).map(|i| (v >> (8 * i)) as u8));
                return out;
            }
        }
        let mut out = vec![0xFF];
        out.extend_from_slice(&v.to_le_bytes());
        out
    }

    #[test]
    fn number_encoding_helper_round_trips_through_the_parser() {
        for v in [
            0u64,
            1,
            127,
            128,
            300,
            16383,
            16384,
            1 << 30,
            1 << 40,
            u64::MAX,
        ] {
            let bytes = num(v);
            let mut rd = Rd { b: &bytes, pos: 0 };
            assert_eq!(rd.num().unwrap(), v, "{v}");
            assert_eq!(rd.pos, bytes.len(), "{v}");
        }
    }

    /// `kEncodedHeader` description with one folder of simple coders chained in order.
    fn encoded_info(coders: &[(&[u8], &[u8])], pack_pos: u64, pack: u64, unpack: u64) -> Vec<u8> {
        let mut b = vec![K_PACK_INFO];
        b.extend(num(pack_pos));
        b.extend(num(1));
        b.push(K_SIZE);
        b.extend(num(pack));
        b.push(K_END);
        b.push(K_UNPACK_INFO);
        b.push(K_FOLDER);
        b.extend(num(1));
        b.push(0);
        b.extend(num(coders.len() as u64));
        for (id, props) in coders {
            let mut bits = id.len() as u8;
            if !props.is_empty() {
                bits |= 0x20;
            }
            b.push(bits);
            b.extend_from_slice(id);
            if !props.is_empty() {
                b.extend(num(props.len() as u64));
                b.extend_from_slice(props);
            }
        }
        for i in 0..coders.len().saturating_sub(1) {
            b.extend(num(i as u64 + 1));
            b.extend(num(i as u64));
        }
        b.push(K_CODERS_UNPACK_SIZE);
        for _ in coders {
            b.extend(num(unpack));
        }
        b.push(K_END);
        b.push(K_END);
        b
    }

    /// A whole file: signature header, `pack` filler bytes, then `kEncodedHeader` + `info`.
    fn file_with_encoded_header(info: &[u8], pack: usize) -> Vec<u8> {
        let mut next = vec![K_ENCODED_HEADER];
        next.extend_from_slice(info);
        let mut start = Vec::new();
        start.extend_from_slice(&(pack as u64).to_le_bytes());
        start.extend_from_slice(&(next.len() as u64).to_le_bytes());
        start.extend_from_slice(&0u32.to_le_bytes());
        let mut f = SIGNATURE.to_vec();
        f.extend_from_slice(&[0, 4]);
        f.extend_from_slice(&crc32fast::hash(&start).to_le_bytes());
        f.extend_from_slice(&start);
        f.extend(std::iter::repeat_n(0xAB, pack));
        f.extend_from_slice(&next);
        f
    }

    fn lzma_props(dict: u32) -> Vec<u8> {
        let mut p = vec![0x5D];
        p.extend_from_slice(&dict.to_le_bytes());
        p
    }

    fn check(bytes: Vec<u8>, limit: u64) -> Result<(), ArchiveError> {
        precheck_encoded_header(&mut Cursor::new(bytes), limit)
    }

    fn assert_codec_too_large(r: Result<(), ArchiveError>) {
        let e = r.unwrap_err();
        assert_eq!(e.code, ArchiveErrorCode::LimitExceeded, "{e:?}");
        assert_eq!(e.message, CODEC_TOO_LARGE);
    }

    #[test]
    fn small_hand_built_encoded_header_passes() {
        let info = encoded_info(&[(&LZMA_ID, &lzma_props(1 << 20))], 0, 10, 100);
        check(file_with_encoded_header(&info, 10), HEADER_CODEC_MAX_MEMORY).unwrap();
    }

    #[test]
    fn aes_wrapped_header_counts_only_the_lzma_dictionary() {
        let aes = [0x07u8, 0x00, 0x11, 0x22]; // not parsed: AES has no dictionary
        let info = encoded_info(
            &[(&AES_ID, &aes), (&LZMA_ID, &lzma_props(1 << 20))],
            0,
            10,
            100,
        );
        check(file_with_encoded_header(&info, 10), HEADER_CODEC_MAX_MEMORY).unwrap();
    }

    #[test]
    fn hostile_lzma_dictionary_in_the_header_is_refused() {
        for dict in [u32::MAX, 1 << 30, 128 << 20, (64 << 20) + 1] {
            let info = encoded_info(&[(&LZMA_ID, &lzma_props(dict))], 0, 10, 100);
            assert_codec_too_large(check(
                file_with_encoded_header(&info, 10),
                HEADER_CODEC_MAX_MEMORY,
            ));
        }
        // exactly at the limit is fine
        let info = encoded_info(&[(&LZMA_ID, &lzma_props(64 << 20))], 0, 10, 100);
        check(file_with_encoded_header(&info, 10), HEADER_CODEC_MAX_MEMORY).unwrap();
    }

    #[test]
    fn hostile_lzma2_dictionary_in_the_header_is_refused() {
        // prop 40 = 4 GiB - 1, prop 30 = 64 MiB * 2 = 128 MiB
        for prop in [40u8, 30] {
            let info = encoded_info(&[(&LZMA2_ID, &[prop])], 0, 10, 100);
            assert_codec_too_large(check(
                file_with_encoded_header(&info, 10),
                HEADER_CODEC_MAX_MEMORY,
            ));
        }
        // prop 41 and a missing prop do not parse: fail closed
        let bad = encoded_info(&[(&LZMA2_ID, &[41])], 0, 10, 100);
        assert_codec_too_large(check(
            file_with_encoded_header(&bad, 10),
            HEADER_CODEC_MAX_MEMORY,
        ));
        let none = encoded_info(&[(&LZMA2_ID, &[])], 0, 10, 100);
        assert_codec_too_large(check(
            file_with_encoded_header(&none, 10),
            HEADER_CODEC_MAX_MEMORY,
        ));
    }

    #[test]
    fn coder_memory_is_summed_over_the_folder() {
        let p = lzma_props(1 << 20);
        let info = encoded_info(
            &[(&LZMA_ID, &p), (&LZMA_ID, &p), (&LZMA_ID, &p)],
            0,
            10,
            100,
        );
        let f = file_with_encoded_header(&info, 10);
        check(f.clone(), 3 << 20).unwrap();
        assert_codec_too_large(check(f, (3 << 20) - 1));
    }

    #[test]
    fn unpacked_header_size_and_header_size_are_capped() {
        let info = encoded_info(&[(&LZMA_ID, &lzma_props(1 << 20))], 0, 10, 1 << 40);
        let e = check(file_with_encoded_header(&info, 10), HEADER_CODEC_MAX_MEMORY).unwrap_err();
        assert_eq!(e.code, ArchiveErrorCode::LimitExceeded);

        // next_header_size = u64::MAX must be refused without trying to read or allocate it.
        let mut f = file_with_encoded_header(
            &encoded_info(&[(&LZMA_ID, &lzma_props(1 << 20))], 0, 10, 100),
            10,
        );
        f[20..28].copy_from_slice(&u64::MAX.to_le_bytes());
        let e = check(f, HEADER_CODEC_MAX_MEMORY).unwrap_err();
        assert_eq!(e.code, ArchiveErrorCode::LimitExceeded);
    }

    #[test]
    fn offsets_and_sizes_are_validated_against_the_file() {
        let good = file_with_encoded_header(
            &encoded_info(&[(&LZMA_ID, &lzma_props(1 << 20))], 0, 10, 100),
            10,
        );
        // offset overflow
        let mut f = good.clone();
        f[12..20].copy_from_slice(&u64::MAX.to_le_bytes());
        assert_eq!(
            check(f, HEADER_CODEC_MAX_MEMORY).unwrap_err().code,
            ArchiveErrorCode::CorruptArchive
        );
        // header beyond the end of the file
        let mut f = good.clone();
        f[12..20].copy_from_slice(&(1u64 << 40).to_le_bytes());
        assert_eq!(
            check(f, HEADER_CODEC_MAX_MEMORY).unwrap_err().code,
            ArchiveErrorCode::CorruptArchive
        );
        // truncated file
        let f = good[..good.len() - 3].to_vec();
        assert_eq!(
            check(f, HEADER_CODEC_MAX_MEMORY).unwrap_err().code,
            ArchiveErrorCode::CorruptArchive
        );
        // packed range reaching past the packed area
        let info = encoded_info(&[(&LZMA_ID, &lzma_props(1 << 20))], 0, 11, 100);
        assert_eq!(
            check(file_with_encoded_header(&info, 10), HEADER_CODEC_MAX_MEMORY)
                .unwrap_err()
                .code,
            ArchiveErrorCode::CorruptArchive
        );
        let info = encoded_info(&[(&LZMA_ID, &lzma_props(1 << 20))], u64::MAX, 10, 100);
        assert_eq!(
            check(file_with_encoded_header(&info, 10), HEADER_CODEC_MAX_MEMORY)
                .unwrap_err()
                .code,
            ArchiveErrorCode::CorruptArchive
        );
        // too short, zero start header
        assert!(check(vec![0x37, 0x7A], HEADER_CODEC_MAX_MEMORY).is_err());
        let mut f = SIGNATURE.to_vec();
        f.extend_from_slice(&[0u8; 26]);
        assert!(check(f, HEADER_CODEC_MAX_MEMORY).is_err());
    }

    #[test]
    fn absurd_counts_are_refused_before_any_allocation() {
        let mut huge_folders = vec![K_UNPACK_INFO, K_FOLDER];
        huge_folders.extend(num(1 << 50));
        let mut huge_coders = vec![K_UNPACK_INFO, K_FOLDER];
        huge_coders.extend(num(1));
        huge_coders.push(0);
        huge_coders.extend(num(1 << 50));
        let mut huge_pack = vec![K_PACK_INFO, 0];
        huge_pack.extend(num(1 << 50));
        let mut huge_props = vec![K_UNPACK_INFO, K_FOLDER];
        huge_props.extend(num(1));
        huge_props.push(0);
        huge_props.extend(num(1));
        huge_props.push(0x23); // id size 3 + has props
        huge_props.extend_from_slice(&LZMA_ID);
        huge_props.extend(num(1 << 50));
        for info in [huge_folders, huge_coders, huge_pack, huge_props] {
            let e = check(file_with_encoded_header(&info, 0), HEADER_CODEC_MAX_MEMORY).unwrap_err();
            assert_eq!(e.code, ArchiveErrorCode::CorruptArchive, "{e:?}");
        }
        // many substreams
        let mut info = encoded_info(&[(&LZMA_ID, &lzma_props(1 << 20))], 0, 10, 100);
        info.pop(); // drop the final kEnd
        info.push(K_SUB_STREAMS_INFO);
        info.push(K_NUM_UNPACK_STREAM);
        info.extend(num(1 << 40));
        info.push(K_END);
        info.push(K_END);
        assert_eq!(
            check(file_with_encoded_header(&info, 10), HEADER_CODEC_MAX_MEMORY)
                .unwrap_err()
                .code,
            ArchiveErrorCode::CorruptArchive
        );
    }

    #[test]
    fn plain_header_is_only_size_checked() {
        let mut next = vec![K_HEADER, K_END];
        let mut start = Vec::new();
        start.extend_from_slice(&0u64.to_le_bytes());
        start.extend_from_slice(&(next.len() as u64).to_le_bytes());
        start.extend_from_slice(&0u32.to_le_bytes());
        let mut f = SIGNATURE.to_vec();
        f.extend_from_slice(&[0, 4]);
        f.extend_from_slice(&crc32fast::hash(&start).to_le_bytes());
        f.extend_from_slice(&start);
        f.append(&mut next);
        check(f, 1).unwrap();
    }

    /// Archive written by sevenz-rust2 with default settings and enough entries that the
    /// writer chooses an encoded header. Returns the raw bytes.
    fn sevenz_rust2_archive_with_encoded_header() -> Vec<u8> {
        use sevenz_rust2::{ArchiveEntry, ArchiveWriter};
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("many.7z");
        let mut w = ArchiveWriter::create(&p).unwrap();
        for i in 0..300 {
            w.push_archive_entry(
                ArchiveEntry::new_file(&format!("folder/some-long-file-name-{i:04}.txt")),
                Some(&b"x"[..]),
            )
            .unwrap();
        }
        w.finish().unwrap();
        std::fs::read(&p).unwrap()
    }

    fn next_header_nid(bytes: &[u8]) -> u8 {
        let off = u64::from_le_bytes(bytes[12..20].try_into().unwrap()) as usize;
        bytes[32 + off]
    }

    #[test]
    fn sevenz_rust2_default_encoded_header_still_opens() {
        let bytes = sevenz_rust2_archive_with_encoded_header();
        assert_eq!(
            next_header_nid(&bytes),
            K_ENCODED_HEADER,
            "test needs an encoded header"
        );
        check(bytes.clone(), HEADER_CODEC_MAX_MEMORY).unwrap();
        // ... and the decoder crate itself opens it.
        let reader = sevenz_rust2::ArchiveReader::new(
            Cursor::new(bytes.clone()),
            sevenz_rust2::Password::empty(),
        )
        .unwrap();
        assert_eq!(reader.archive().files.len(), 300);
        // The same bytes are refused by a limit below the header's own dictionary.
        assert_codec_too_large(check(bytes, 1024));
    }

    struct XorShift(u64);
    impl XorShift {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
    }

    #[test]
    fn mutated_headers_never_panic_and_finish_quickly() {
        let seeds = [
            sevenz_rust2_archive_with_encoded_header(),
            file_with_encoded_header(
                &encoded_info(
                    &[
                        (&AES_ID, &[7, 0, 1, 2, 3]),
                        (&LZMA_ID, &lzma_props(1 << 20)),
                    ],
                    0,
                    10,
                    100,
                ),
                10,
            ),
            file_with_encoded_header(&encoded_info(&[(&LZMA2_ID, &[20])], 4, 6, 50), 10),
        ];
        let started = std::time::Instant::now();
        let mut rng = XorShift(0x9E37_79B9_7F4A_7C15);
        let mut accepted = 0u32;
        let mut refused = 0u32;
        for round in 0..40_000u32 {
            let mut bytes = seeds[round as usize % seeds.len()].clone();
            // The interesting bytes are the signature header and the tail (next header).
            let tail_start = bytes.len().saturating_sub(300);
            for _ in 0..1 + rng.below(4) {
                let at = if rng.below(3) == 0 {
                    rng.below(bytes.len().min(32))
                } else {
                    tail_start + rng.below(bytes.len() - tail_start)
                };
                match rng.below(4) {
                    0 => bytes[at] = rng.next() as u8,
                    1 => bytes[at] ^= 1 << rng.below(8),
                    2 => bytes[at] = 0xFF,
                    _ => bytes[at] = 0,
                }
            }
            match rng.below(6) {
                0 => {
                    let cut = rng.below(bytes.len());
                    bytes.truncate(cut);
                }
                1 => {
                    let at = tail_start + rng.below(bytes.len() - tail_start);
                    bytes.splice(at..at, std::iter::repeat_n(0xFFu8, 1 + rng.below(12)));
                }
                _ => {}
            }
            match check(bytes, HEADER_CODEC_MAX_MEMORY) {
                Ok(()) => accepted += 1,
                Err(_) => refused += 1,
            }
        }
        assert!(accepted > 0 && refused > 0, "{accepted}/{refused}");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(20),
            "{:?}",
            started.elapsed()
        );
    }
}
