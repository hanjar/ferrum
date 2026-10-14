//! 7z provider (F2-07): browse, bounded read, and safe extraction of 7z
//! archives through the `sevenz-rust2` decoder and the F2-03 no-follow
//! publish pipeline. Mirrors `archive_zip.rs`; the 7z-specific differences
//! are documented per function.
//!
//! Creation lives in `archive_create_test::create_7z` (driven by the
//! `archive_create` command). RAR stays the only read-only format by license.
//!
//! Limitations recorded honestly:
//! - 7z has no central directory: metadata comes from the parsed header, so
//!   header-encrypted archives cannot be listed without the password — the
//!   caller sees a structured `PasswordRequired` and retries with one
//!   (`ArchiveListRequest.password`, FER-504; absent behaves as before).
//! - The decoder walks blocks sequentially. EXTRACTION decodes every block at
//!   most once, in order (FER-437): one worker thread decodes and the safe
//!   pipeline writes the selected members as they arrive, so solid archives
//!   cost O(total size). Single-member READS (preview, `archive_read`,
//!   compare) in a solid block must still decode the members before the
//!   target; that skip is bounded by `ReadGuard` (256 MiB / 5 s).

use std::cell::RefCell;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use sevenz_rust2::{ArchiveReader, Password};

use crate::archive_contract::{
    ArchiveCapabilities, ArchiveCreateRequest, ArchiveEntry, ArchiveError, ArchiveErrorCode,
    ArchiveFormat, ArchiveListRequest, ArchiveListResponse, ArchiveProvider, ArchiveReadRequest,
    ArchiveTestReport, ArchiveTestRequest, Location, MemberPath, dedup_level, level_row,
};

pub struct SevenZProvider;

impl SevenZProvider {
    pub fn new() -> Self {
        Self
    }

    /// 7z signature header `37 7A BC AF 27 1C` (also required for the nonstandard `.7zip`
    /// extension per the archive-support doc), read from the start of an opened source.
    fn sniff_file(file: &mut File) -> Result<(), ArchiveError> {
        use std::io::{Seek, SeekFrom};
        file.seek(SeekFrom::Start(0))
            .map_err(|e| ArchiveError::new(ArchiveErrorCode::Io, e.to_string()))?;
        let mut magic = [0u8; 6];
        let read = file
            .read(&mut magic)
            .map_err(|e| ArchiveError::new(ArchiveErrorCode::Io, e.to_string()))?;
        if read != 6 || magic != SEVENZ_SIGNATURE {
            return Err(ArchiveError::new(
                ArchiveErrorCode::UnsupportedFormat,
                "Not a 7z archive (bad signature)",
            ));
        }
        Ok(())
    }

    fn open_reader(
        path: &Path,
        password: Option<&str>,
    ) -> Result<ArchiveReader<File>, ArchiveError> {
        let file = File::open(path).map_err(|e| {
            let mut err = ArchiveError::new(ArchiveErrorCode::Io, e.to_string());
            err.archive_path = Some(path.to_string_lossy().into_owned());
            err
        })?;
        Self::open_reader_file(file, password).map_err(|mut err| {
            err.archive_path = Some(path.to_string_lossy().into_owned());
            err
        })
    }

    /// FER-488: the decoder reads exactly `file` (opened, and for previews checked, by the
    /// caller); nothing is looked up by path again.
    fn open_reader_file(
        mut file: File,
        password: Option<&str>,
    ) -> Result<ArchiveReader<File>, ArchiveError> {
        use std::io::{Seek, SeekFrom};
        Self::sniff_file(&mut file)?;
        file.seek(SeekFrom::Start(0))
            .map_err(|e| ArchiveError::new(ArchiveErrorCode::Io, e.to_string()))?;
        let mut reader = ArchiveReader::new(file, password_of(password)).map_err(sevenz_err)?;
        // FER-104 N1: the decoder defaults to one LZMA2 worker per core, and
        // each worker decodes a whole work unit (typically the whole block)
        // into an unbounded Vec regardless of how little we read: a 156 KB
        // archive pegged ~1.2 GB; larger ones abort the process. One thread
        // decodes as a stream with constant memory. All paths (preview, read,
        // compare, test, extract) open through here.
        reader.set_thread_count(DECODE_THREADS);
        Ok(reader)
    }
}

impl Default for SevenZProvider {
    fn default() -> Self {
        Self::new()
    }
}

/// Decoder threads for every 7z read: must stay 1 (see `open_reader`).
pub(crate) const DECODE_THREADS: u32 = 1;

const SEVENZ_SIGNATURE: [u8; 6] = [0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C];

fn password_of(password: Option<&str>) -> Password {
    password.map(Password::from).unwrap_or_else(Password::empty)
}

/// Map the decoder's typed errors onto contract codes. `MaybeBadPassword`
/// (a decode failure while a password WAS supplied) means the password is
/// wrong; `PasswordRequired` means encrypted content met an empty password.
fn sevenz_err(e: sevenz_rust2::Error) -> ArchiveError {
    use sevenz_rust2::Error as E;
    let code = match &e {
        E::PasswordRequired => ArchiveErrorCode::PasswordRequired,
        E::MaybeBadPassword(_) => ArchiveErrorCode::BadPassword,
        E::BadSignature(_) => ArchiveErrorCode::UnsupportedFormat,
        E::Unsupported(_) | E::UnsupportedCompressionMethod(_) | E::ExternalUnsupported => {
            ArchiveErrorCode::UnsupportedCodec
        }
        E::MaxMemLimited { .. } => ArchiveErrorCode::LimitExceeded,
        _ => ArchiveErrorCode::CorruptArchive,
    };
    ArchiveError::new(code, format!("7z: {e}"))
}

/// 7z member names may carry Windows separators from the creating tool;
/// normalize to '/' before grammar validation (same rule as the RAR lane).
fn normalized(name: &str) -> String {
    name.replace('\\', "/")
}

/// Decode-level failures are corruption, not transient IO: the CRC layer
/// reports mismatches as io errors, and a garbled stream surfaces as
/// UnexpectedEof when the decoder runs out of (now meaningless) input.
///
/// The single-threaded LZMA2 reader reports damage as an `InvalidInput` error
/// whose text is "corrupted input data" (FER-439): only that exact pairing is
/// corruption. A bare "corrupt" substring is NOT enough, so an unrelated
/// error (for example a Windows `ERROR_FILE_CORRUPT` from the disk) stays Io.
fn decode_io_err(e: std::io::Error) -> ArchiveError {
    let text = e.to_string().to_lowercase();
    let lzma_corrupt =
        e.kind() == std::io::ErrorKind::InvalidInput && text.contains("corrupted input");
    let code = if e.kind() == std::io::ErrorKind::InvalidData
        || e.kind() == std::io::ErrorKind::UnexpectedEof
        || text.contains("checksum")
        || text.contains("crc")
        || lzma_corrupt
    {
        ArchiveErrorCode::CorruptArchive
    } else {
        ArchiveErrorCode::Io
    };
    ArchiveError::new(code, e.to_string())
}

/// Safety budget for READ-ONLY member access (preview, `archive_read`).
/// 7z decodes a solid block sequentially, so reaching a member means
/// decompressing everything before it in the same block; this caps that
/// work, and the codec memory the decoder may be asked to allocate.
#[derive(Debug, Clone, Copy)]
pub struct ReadGuard {
    /// Max decompressed bytes that may be skipped to reach the member.
    pub max_skip_bytes: u64,
    /// Max wall time spent skipping.
    pub max_skip_time: std::time::Duration,
    /// Max dictionary / model memory a block's codec may declare.
    pub max_codec_memory: u64,
}

impl ReadGuard {
    pub const PREVIEW: ReadGuard = ReadGuard {
        max_skip_bytes: crate::archive_contract::MAX_SOLID_SKIP_BYTES,
        max_skip_time: std::time::Duration::from_secs(crate::archive_contract::MAX_SOLID_SKIP_SECS),
        max_codec_memory: 256 * 1024 * 1024,
    };
}

pub use crate::archive_contract::{CODEC_TOO_LARGE, SOLID_TOO_LARGE};

const METHOD_LZMA: [u8; 3] = [0x03, 0x01, 0x01];
const METHOD_LZMA2: [u8; 1] = [0x21];
const METHOD_PPMD: [u8; 3] = [0x03, 0x04, 0x01];

/// Declared decoder memory for a coder, from its method id and properties.
/// `None` for methods whose memory is not attacker-sized (copy, filters...).
fn declared_codec_memory(method: &[u8], props: &[u8]) -> Option<Result<u64, ()>> {
    if method == METHOD_LZMA {
        // props: lc/lp/pb byte + u32 LE dictionary size.
        return Some(if props.len() >= 5 {
            Ok(u32::from_le_bytes([props[1], props[2], props[3], props[4]]) as u64)
        } else {
            Err(())
        });
    }
    if method == METHOD_LZMA2 {
        return Some(match props.first() {
            Some(&b) if b <= 40 => Ok(if b == 40 {
                u32::MAX as u64
            } else {
                (2u64 | (b as u64 & 1)) << (b / 2 + 11)
            }),
            _ => Err(()),
        });
    }
    if method == METHOD_PPMD {
        // props: order byte + u32 LE memory size.
        return Some(if props.len() >= 5 {
            Ok(u32::from_le_bytes([props[1], props[2], props[3], props[4]]) as u64)
        } else {
            Err(())
        });
    }
    None
}

/// The decoder crate keeps coder properties private, but they are part of
/// its `Debug` output (`properties: [93, 0, 0, 128, 0]`). The crate is
/// pinned (`=0.20.2`) and a test asserts this parse on a real archive; if
/// the shape ever changes the check fails CLOSED (refuses the block).
fn coder_properties(coder: &sevenz_rust2::Coder) -> Option<Vec<u8>> {
    let dbg = format!("{coder:?}");
    let start = dbg.find("properties: [")? + "properties: [".len();
    let end = start + dbg[start..].find(']')?;
    let body = dbg[start..end].trim();
    if body.is_empty() {
        return Some(Vec::new());
    }
    body.split(',')
        .map(|n| n.trim().parse::<u8>().ok())
        .collect()
}

/// Refuse blocks whose codec would allocate more than `limit` bytes.
fn check_block_codec_memory(
    archive: &sevenz_rust2::Archive,
    block_index: usize,
    limit: u64,
) -> Result<(), ArchiveError> {
    let too_large = || ArchiveError::new(ArchiveErrorCode::LimitExceeded, CODEC_TOO_LARGE);
    let Some(block) = archive.blocks.get(block_index) else {
        return Ok(());
    };
    let mut items: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    for coder in &block.coders {
        let method = coder.encoder_method_id();
        if method != METHOD_LZMA && method != METHOD_LZMA2 && method != METHOD_PPMD {
            continue;
        }
        let props = coder_properties(coder).ok_or_else(too_large)?;
        items.push((method.to_vec(), props));
    }
    if sum_within_limit(&items, limit) {
        Ok(())
    } else {
        Err(too_large())
    }
}

/// Sum over ALL memory-sized coders of one block (a chain such as BCJ2 plus
/// several LZMA holds every dictionary at once). Fails closed on bad props.
fn sum_within_limit(coders: &[(Vec<u8>, Vec<u8>)], limit: u64) -> bool {
    let mut total: u64 = 0;
    for (method, props) in coders {
        match declared_codec_memory(method, props) {
            Some(Ok(mem)) => total = total.saturating_add(mem),
            Some(Err(())) => return false,
            None => {}
        }
        if total > limit {
            return false;
        }
    }
    true
}

/// Read a skipped entry to the end so the shared block decoder is positioned
/// at the next entry; charges `budget` (bytes, wall time) when one is set.
fn drain_entry(
    stream: &mut dyn Read,
    guard: Option<&ReadGuard>,
    skipped: &mut u64,
    started: std::time::Instant,
) -> Result<(), ArchiveError> {
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = stream.read(&mut buf).map_err(decode_io_err)?;
        if n == 0 {
            return Ok(());
        }
        *skipped += n as u64;
        crate::archive_contract::check_cancelled()?;
        if let Some(g) = guard
            && (*skipped > g.max_skip_bytes || started.elapsed() > g.max_skip_time)
        {
            return Err(ArchiveError::new(
                ArchiveErrorCode::LimitExceeded,
                SOLID_TOO_LARGE,
            ));
        }
    }
}

/// Bounded decode of ONE member by name: walks the decoder until the member
/// is met (stopping the walk afterwards), piping at most `max_bytes` through
/// `chunk`. Returns (found, truncated).
///
/// Entries that precede the member IN THE SAME BLOCK share one decoder
/// stream, so they are read to the end first (otherwise the member would be
/// decoded from the wrong offset: checksum failure, or silently another
/// member's bytes when the archive carries no CRC). Entries in other blocks
/// are never decoded. `guard` bounds that skipped work for read-only paths.
fn stream_member<R: Read + std::io::Seek>(
    reader: &mut ArchiveReader<R>,
    wanted: &str,
    max_bytes: u64,
    guard: Option<&ReadGuard>,
    chunk: &mut dyn FnMut(&[u8]) -> Result<(), ArchiveError>,
) -> Result<(bool, bool), ArchiveError> {
    // Locate the target (first entry with that name) and its block up front.
    let (target_index, target_block, file_blocks, files_base, stride) = {
        let archive = reader.archive();
        let Some(idx) = archive
            .files
            .iter()
            .position(|e| normalized(e.name()) == wanted)
        else {
            return Ok((false, false));
        };
        let block = archive
            .stream_map
            .file_block_index
            .get(idx)
            .copied()
            .flatten();
        if let Some(g) = guard {
            // The decoder builds a decode stack for every block it walks
            // (also those after the target before our abort lands).
            for bi in 0..archive.blocks.len() {
                check_block_codec_memory(archive, bi, g.max_codec_memory)?;
            }
        }
        (
            idx,
            block,
            archive.stream_map.file_block_index.clone(),
            archive.files.as_ptr() as usize,
            std::mem::size_of::<sevenz_rust2::ArchiveEntry>(),
        )
    };

    let started = std::time::Instant::now();
    let mut skipped = 0u64;
    let mut remaining = max_bytes;
    let mut truncated = false;
    let mut found = false;
    let mut done = false;
    // The callback must return the decoder's own error type, so failures are
    // carried out through this slot and re-raised after the walk.
    let mut failure: Option<ArchiveError> = None;
    let walk = reader.for_each_entries(|entry, stream| {
        if done {
            // The decoder keeps walking later blocks after we stopped: abort.
            return Err(sevenz_rust2::Error::Other("preview walk finished".into()));
        }
        let index = (entry as *const sevenz_rust2::ArchiveEntry as usize).wrapping_sub(files_base)
            / stride.max(1);
        if index != target_index {
            let same_block = target_block.is_some()
                && file_blocks.get(index).copied().flatten() == target_block
                && index < target_index;
            if same_block && let Err(e) = drain_entry(stream, guard, &mut skipped, started) {
                failure = Some(e);
                done = true;
                return Ok(false);
            }
            return Ok(true);
        }
        // The index came from pointer arithmetic on the crate's own entry
        // slice; confirm by name so a changed crate can never make us serve
        // another member's bytes under this member's label (fail closed).
        if normalized(entry.name()) != wanted {
            failure = Some(ArchiveError::new(
                ArchiveErrorCode::CorruptArchive,
                "7z entry mismatch",
            ));
            done = true;
            return Ok(false);
        }
        found = true;
        done = true;
        let mut buf = [0u8; 64 * 1024];
        loop {
            if let Err(e) = crate::archive_contract::check_cancelled() {
                failure = Some(e);
                return Ok(false);
            }
            let to_read = remaining.min(buf.len() as u64) as usize;
            if to_read == 0 {
                // Confirm truncation only if bytes actually remain.
                let mut probe = [0u8; 1];
                match stream.read(&mut probe) {
                    Ok(1) => truncated = true,
                    Ok(_) => {}
                    Err(e) => failure = Some(decode_io_err(e)),
                }
                return Ok(false);
            }
            match stream.read(&mut buf[..to_read]) {
                Ok(0) => return Ok(false),
                Ok(n) => {
                    if let Err(e) = chunk(&buf[..n]) {
                        failure = Some(e);
                        return Ok(false);
                    }
                    remaining -= n as u64;
                }
                Err(e) => {
                    failure = Some(decode_io_err(e));
                    return Ok(false);
                }
            }
        }
    });
    if let Some(e) = failure {
        return Err(e);
    }
    match walk {
        Ok(()) => {}
        // Our own abort after the member was handled is not a failure.
        Err(_) if done => {}
        Err(e) => return Err(sevenz_err(e)),
    }
    Ok((found, truncated))
}

fn nt_time_to_ms(t: sevenz_rust2::NtTime) -> Option<i64> {
    let st: std::time::SystemTime = t.into();
    st.duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_millis() as i64)
}

/// Listing body shared by `list` (by path) and `list_opened` (checked handle, FER-488).
fn list_from(
    source: Option<File>,
    request: &ArchiveListRequest,
) -> Result<ArchiveListResponse, ArchiveError> {
    let Location::Archive {
        archive_path,
        member_path,
    } = &request.location
    else {
        return Err(ArchiveError::new(
            ArchiveErrorCode::InvalidMemberPath,
            "Listing requires archive location",
        ));
    };
    let prefix = if member_path.as_str().is_empty() {
        String::new()
    } else {
        format!("{}/", member_path.as_str())
    };

    // FER-504: the request may carry a password for header-encrypted archives.
    let password = request.password.as_deref();
    let reader = match source {
        Some(file) => SevenZProvider::open_reader_file(file, password)?,
        None => SevenZProvider::open_reader(Path::new(archive_path), password)?,
    };
    // Direct children of `member_path`; implicit dirs synthesized and
    // deduped by child-name (FER-508, same rules as RAR and ZIP).
    let mut rows: Vec<(String, bool, u64, Option<i64>)> = Vec::new();
    for file in reader.archive().files.iter() {
        let raw = normalized(file.name());
        let mod_time = if file.has_last_modified_date {
            nt_time_to_ms(file.last_modified_date)
        } else {
            None
        };
        let is_dir = file.is_directory();
        let size = if is_dir { 0 } else { file.size() };
        if let Some(row) = level_row(&prefix, &raw, is_dir, size, mod_time) {
            rows.push(row);
        }
    }
    let rows = dedup_level(member_path.as_str(), rows)?;

    let entries: Vec<ArchiveEntry> = rows
        .into_iter()
        .map(|(name, is_dir, size, modified_ms)| {
            let full = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{}/{}", member_path.as_str(), name)
            };
            let member_path = MemberPath::parse(&full)?;
            Ok(ArchiveEntry {
                member_path,
                name,
                is_dir,
                size,
                modified_ms,
            })
        })
        .collect::<Result<Vec<_>, ArchiveError>>()?;
    // Folders-first ordering, then by name — the contract's listing order.
    let mut entries = entries;
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    let response = ArchiveListResponse {
        location: request.location.clone(),
        request_token: request.request_token,
        entries,
    };
    response.validate()?;
    Ok(response)
}

/// Read body shared by `read` (by path) and `read_opened` (checked handle, FER-488).
fn read_from(
    source: Option<File>,
    request: &ArchiveReadRequest,
    chunk: &mut dyn FnMut(&[u8]) -> Result<(), ArchiveError>,
) -> Result<bool, ArchiveError> {
    let Location::Archive {
        archive_path,
        member_path,
    } = &request.location
    else {
        return Err(ArchiveError::new(
            ArchiveErrorCode::InvalidMemberPath,
            "Read requires archive location",
        ));
    };
    let password = request.password.as_deref();
    let mut reader = match source {
        Some(file) => SevenZProvider::open_reader_file(file, password)?,
        None => SevenZProvider::open_reader(Path::new(archive_path), password)?,
    };
    if reader
        .archive()
        .files
        .iter()
        .any(|e| normalized(e.name()) == member_path.as_str() && e.is_directory())
    {
        return Err(ArchiveError::new(
            ArchiveErrorCode::Io,
            "Member is a directory",
        ));
    }
    let (found, truncated) = stream_member(
        &mut reader,
        member_path.as_str(),
        request.max_bytes,
        Some(&ReadGuard::PREVIEW),
        chunk,
    )?;
    if !found {
        let mut err = ArchiveError::new(ArchiveErrorCode::Io, "Member not found");
        err.member_path = Some(member_path.as_str().to_owned());
        return Err(err);
    }
    Ok(truncated)
}

impl ArchiveProvider for SevenZProvider {
    fn capabilities(&self, _format: ArchiveFormat) -> ArchiveCapabilities {
        ArchiveCapabilities {
            format: ArchiveFormat::Sevenz,
            list: true,
            read: true,
            extract: true,        // via F2-03 pipeline, same as ZIP
            create: true,         // F2-11 (create_7z)
            test: true,           // F2-11 (test_7z, full decode + CRC)
            encrypted_read: true, // sevenz-rust2 aes256; header-encrypted
            // archives need the password even to LIST: it travels in
            // `ArchiveListRequest.password` (FER-504); absent, the listing
            // answers PasswordRequired, never a silent empty list.
            encrypted_create: false,
            multi_volume: false,
        }
    }

    fn list(&self, request: &ArchiveListRequest) -> Result<ArchiveListResponse, ArchiveError> {
        list_from(None, request)
    }

    fn read(
        &self,
        request: &ArchiveReadRequest,
        chunk: &mut dyn FnMut(&[u8]) -> Result<(), ArchiveError>,
    ) -> Result<bool, ArchiveError> {
        read_from(None, request, chunk)
    }

    fn reads_opened(&self) -> bool {
        true
    }

    fn list_opened(
        &self,
        file: File,
        request: &ArchiveListRequest,
    ) -> Result<ArchiveListResponse, ArchiveError> {
        list_from(Some(file), request)
    }

    fn read_opened(
        &self,
        file: File,
        request: &ArchiveReadRequest,
        chunk: &mut dyn FnMut(&[u8]) -> Result<(), ArchiveError>,
    ) -> Result<bool, ArchiveError> {
        read_from(Some(file), request, chunk)
    }

    fn extract(
        &self,
        request: &crate::archive_contract::ArchiveExtractRequest,
        progress: &mut dyn FnMut(crate::archive_contract::ArchiveProgress) -> bool,
    ) -> Result<(), ArchiveError> {
        extract_counted(request, progress, &AtomicU64::new(0), CANCEL_POLL)
    }

    fn create(
        &self,
        request: &ArchiveCreateRequest,
        progress: &mut dyn FnMut(crate::archive_contract::ArchiveProgress) -> bool,
    ) -> Result<PathBuf, ArchiveError> {
        let dest = Path::new(&request.destination_path);
        crate::archive_create_test::create_7z_progress_with_level(
            &request.source_paths,
            dest,
            request.conflict,
            request.level(),
            progress,
        )
    }

    fn test(
        &self,
        request: &ArchiveTestRequest,
        _progress: &mut dyn FnMut(crate::archive_contract::ArchiveProgress) -> bool,
    ) -> Result<ArchiveTestReport, ArchiveError> {
        crate::archive_create_test::test_7z(
            Path::new(&request.archive_path),
            request.password.as_deref(),
        )
    }
}

/// FER-437: sequential 7z extraction. Every block is decoded at most once,
/// in archive order, by ONE worker thread; the selected members' bytes are
/// handed to the (unchanged) safe extraction pipeline through a bounded
/// channel while the decoder moves on. `decoded` counts the bytes the decoder
/// produced (test hook for "no repeated decode").
fn extract_counted(
    request: &crate::archive_contract::ArchiveExtractRequest,
    progress: &mut dyn FnMut(crate::archive_contract::ArchiveProgress) -> bool,
    decoded: &AtomicU64,
    poll: std::time::Duration,
) -> Result<(), ArchiveError> {
    use crate::archive_contract::{ArchiveOperationKind, ArchiveProgress};
    use crate::archive_extract::{self, Entry, EntryKind};

    let destination = Path::new(&request.destination_path);
    let mut reader = SevenZProvider::open_reader(
        Path::new(&request.archive_path),
        request.password.as_deref(),
    )?;

    // Metadata pass: owned copies so the header borrow ends before the
    // decoder worker needs the reader.
    let names: Vec<(String, bool, u64, Option<i64>)> = reader
        .archive()
        .files
        .iter()
        .map(|e| {
            let name = normalized(e.name());
            (
                name.clone(),
                e.is_directory(),
                if e.is_directory() { 0 } else { e.size() },
                if e.has_last_modified_date {
                    nt_time_to_ms(e.last_modified_date)
                } else {
                    None
                },
            )
        })
        .collect();
    let mut entries: Vec<Entry<'_>> = Vec::with_capacity(names.len());
    for (name, is_dir, size, modified_ms) in &names {
        MemberPath::parse(name).map_err(|mut e| {
            e.member_path = Some(name.clone());
            e
        })?;
        entries.push(Entry {
            raw_name: name,
            kind: if *is_dir {
                EntryKind::Directory
            } else {
                EntryKind::File
            },
            expanded_size: Some(*size),
            compressed_size: None, // per-member packed size is not exposed
            modified_ms: *modified_ms,
        });
    }

    // Selection: empty = all; nonempty = exact paths or subtrees.
    let selected = |raw: &str| {
        request.member_paths.is_empty()
            || request
                .member_paths
                .iter()
                .any(|mp| raw == mp.as_str() || raw.starts_with(&format!("{}/", mp.as_str())))
    };
    let selected_entries: Vec<Entry<'_>> = entries
        .iter()
        .filter(|e| selected(e.raw_name))
        .cloned()
        .collect();
    let plan = archive_extract::preflight(&selected_entries, &request.limits)?;

    // Archive index of every selected file (preflight rejects duplicate
    // names, so the name identifies the member), and which of them carry a
    // stream. Stream-less files (empty files) need no decoding at all.
    let mut file_index: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    let mut wanted: Vec<bool> = vec![false; names.len()];
    for (i, (name, is_dir, ..)) in names.iter().enumerate() {
        if !*is_dir && selected(name) {
            file_index.entry(name.as_str()).or_insert(i);
            if reader
                .archive()
                .stream_map
                .file_block_index
                .get(i)
                .copied()
                .flatten()
                .is_some()
            {
                wanted[i] = true;
            }
        }
    }
    let drain_upto = drain_horizon(reader.archive(), &wanted);

    let op_id = format!("7z-extract-{}", std::process::id());
    let total = plan
        .entries()
        .iter()
        .filter(|e| e.kind != crate::archive_extract::EntryKind::Directory)
        .count() as u64;
    let done = std::cell::Cell::new(0u64);
    // Both the pipeline's cancel check and the wait for decoded bytes poll
    // the caller's progress callback, so it lives behind a RefCell.
    let progress = RefCell::new(progress);
    let track = crate::archive_extract::ProgressTrack::default();
    let total_bytes = plan.total_expanded_bytes();
    let emit = |done: u64, summary: Option<(u64, u64)>| {
        let mut progress = progress.borrow_mut();
        let member = match track.member().as_deref().map(MemberPath::parse) {
            Some(Ok(m)) => Some(m),
            _ => None,
        };
        progress(ArchiveProgress {
            operation_id: op_id.clone(),
            kind: ArchiveOperationKind::Extract,
            processed_bytes: track.bytes(),
            total_bytes,
            processed_entries: done,
            total_entries: Some(total),
            current_member_path: member,
            written_entries: summary.map(|(w, _)| w),
            skipped_entries: summary.map(|(_, s)| s),
        })
    };

    let (tx, rx) = std::sync::mpsc::sync_channel::<Msg>(4);
    let stop = AtomicBool::new(false);
    let result = std::thread::scope(|scope| {
        let wanted = &wanted;
        let drain_upto = &drain_upto;
        let reader = &mut reader;
        let stop = &stop;
        scope.spawn(move || walk_sequential(reader, wanted, drain_upto, &tx, decoded, stop));

        // FER-498: every cancel check raises the stop flag the moment it sees the cancel,
        // before the pipeline removes its staging file and rolls back directories. Raising it
        // only after `extract` returned left the worker draining unthrottled (skipped bytes
        // are never sent, so there is no channel backpressure) for that whole cleanup.
        let cancel_seen = || {
            let cancelled = !emit(done.get(), None);
            if cancelled {
                stop.store(true, Ordering::SeqCst);
            }
            cancelled
        };
        let result = archive_extract::extract(
            &plan,
            destination,
            &request.limits,
            request.conflict,
            &mut |planned: &crate::archive_extract::PlannedEntry,
                  writer: &mut dyn std::io::Write| {
                let Some(&idx) = file_index.get(planned.raw_name.as_str()) else {
                    return Err(ArchiveError::new(
                        ArchiveErrorCode::Io,
                        "member vanished mid-extract",
                    ));
                };
                track.begin_member(&planned.raw_name);
                if wanted[idx] {
                    let mut writer = crate::archive_extract::CountingWriter::new(writer, &track);
                    receive_member(&rx, idx, &mut writer, poll, stop, &mut || cancel_seen())?;
                }
                done.set(done.get() + 1);
                Ok(())
            },
            &mut || cancel_seen(),
        );
        // Normal end or any failure: stop flag (the worker checks it per 64 KiB chunk, also
        // while draining members nobody asked for), then drop the receiver so a worker blocked
        // in `send` wakes up too.
        stop.store(true, Ordering::SeqCst);
        drop(rx);
        result
    });
    let summary = result
        .as_ref()
        .ok()
        .map(|rep| (rep.written_files, rep.skipped_files));
    emit(done.get(), summary);
    result.map(|_| ())
}

/// One message from the decode worker to the extraction pipeline.
enum Msg {
    Chunk(usize, Vec<u8>),
    End(usize, Option<ArchiveError>),
    Fatal(ArchiveError),
}

/// For every file: true when it must be decoded (selected) or drained because
/// a later selected file of the SAME block follows it in the shared stream.
/// Files after the last selected file of their block are never touched.
fn drain_horizon(archive: &sevenz_rust2::Archive, wanted: &[bool]) -> Vec<bool> {
    let mut last_in_block: std::collections::HashMap<usize, usize> =
        std::collections::HashMap::new();
    for (i, w) in wanted.iter().enumerate() {
        if *w && let Some(Some(b)) = archive.stream_map.file_block_index.get(i) {
            last_in_block.insert(*b, i);
        }
    }
    (0..wanted.len())
        .map(|i| {
            archive
                .stream_map
                .file_block_index
                .get(i)
                .copied()
                .flatten()
                .and_then(|b| last_in_block.get(&b))
                .is_some_and(|&last| i <= last)
        })
        .collect()
}

/// How often the extraction waits for the decoder before it re-checks for
/// cancellation (a long drain produces no bytes for the pipeline to react to).
const CANCEL_POLL: std::time::Duration = std::time::Duration::from_millis(100);

/// Pull side: copy the bytes of file `idx` from the worker into `writer`,
/// discarding the bytes of earlier files (selected files the pipeline chose
/// to skip, e.g. by conflict policy). Waits in `poll` slices and asks
/// `cancelled` between slices, so cancelling during a long solid drain
/// returns promptly. A failure the worker met on an earlier file is reported
/// as that failure, not hidden behind a generic "decoder stopped".
///
/// FER-498: `stop` is the worker's stop flag. It is raised the instant a cancel is
/// seen, before anything else happens, and on every other failure too, so the
/// worker never keeps decoding for a member nobody will read.
fn receive_member(
    rx: &std::sync::mpsc::Receiver<Msg>,
    idx: usize,
    writer: &mut dyn std::io::Write,
    poll: std::time::Duration,
    stop: &AtomicBool,
    cancelled: &mut dyn FnMut() -> bool,
) -> Result<(), ArchiveError> {
    let result = receive_member_inner(rx, idx, writer, poll, stop, cancelled);
    if result.is_err() {
        stop.store(true, Ordering::SeqCst);
    }
    result
}

fn receive_member_inner(
    rx: &std::sync::mpsc::Receiver<Msg>,
    idx: usize,
    writer: &mut dyn std::io::Write,
    poll: std::time::Duration,
    stop: &AtomicBool,
    cancelled: &mut dyn FnMut() -> bool,
) -> Result<(), ArchiveError> {
    use std::sync::mpsc::RecvTimeoutError;
    let mut cancel_now = || {
        let yes = cancelled();
        if yes {
            stop.store(true, Ordering::SeqCst);
        }
        yes
    };
    let cancel_err = || ArchiveError::new(ArchiveErrorCode::Cancelled, "Extraction cancelled");
    // FER-459: ask at least once per `poll`, on EVERY message too. Polling only when the channel
    // timed out never fired while a fast decoder kept the channel full, so cancelling during a
    // long solid drain was not honoured until the drain ended.
    let mut last_check = std::time::Instant::now();
    loop {
        match rx.recv_timeout(poll) {
            Ok(Msg::Chunk(i, bytes)) if i == idx => {
                writer
                    .write_all(&bytes)
                    .map_err(|e| ArchiveError::new(ArchiveErrorCode::Io, e.to_string()))?;
            }
            Ok(Msg::End(i, err)) if i == idx => return err.map_or(Ok(()), Err),
            Ok(Msg::End(_, Some(err))) => return Err(err), // earlier, skipped file failed
            Ok(Msg::Fatal(e)) => return Err(e),
            Ok(_) => {} // bytes of an earlier, skipped file
            Err(RecvTimeoutError::Timeout) => {
                last_check = std::time::Instant::now();
                if cancel_now() {
                    return Err(cancel_err());
                }
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err(ArchiveError::new(
                    ArchiveErrorCode::Io,
                    "7z decoder stopped before the member was produced",
                ));
            }
        }
        if last_check.elapsed() >= poll {
            last_check = std::time::Instant::now();
            if cancel_now() {
                return Err(cancel_err());
            }
        }
    }
}

/// Decode worker: one pass over the archive in block order. Selected files
/// are sent chunk by chunk; unselected files that precede a selected file in
/// the same block are drained into the void (the stream is shared); the rest
/// is never decoded. Stops as soon as the receiver is gone.
fn walk_sequential(
    reader: &mut ArchiveReader<File>,
    wanted: &[bool],
    drain_upto: &[bool],
    tx: &std::sync::mpsc::SyncSender<Msg>,
    decoded: &AtomicU64,
    stop: &AtomicBool,
) {
    let (files_base, stride, files_len) = {
        let files = &reader.archive().files;
        (
            files.as_ptr() as usize,
            std::mem::size_of::<sevenz_rust2::ArchiveEntry>().max(1),
            files.len(),
        )
    };
    let remaining_wanted = wanted.iter().filter(|w| **w).count();
    let mut seen_wanted = 0usize;
    let mut failure: Option<ArchiveError> = None;
    let mut stopped = false; // receiver gone or a failure was reported
    let walk = reader.for_each_entries(|entry, stream| {
        if stopped || seen_wanted >= remaining_wanted || stop.load(Ordering::SeqCst) {
            // The crate walks the remaining blocks regardless: abort.
            return Err(sevenz_rust2::Error::Other("extract walk finished".into()));
        }
        // The crate hands us a reference into its own entry slice; the
        // address gives the index, and a mismatch fails closed.
        let addr = entry as *const sevenz_rust2::ArchiveEntry as usize;
        let index = addr.wrapping_sub(files_base) / stride;
        if index >= files_len || files_base + index * stride != addr {
            failure = Some(ArchiveError::new(
                ArchiveErrorCode::CorruptArchive,
                "7z entry mismatch",
            ));
            stopped = true;
            return Ok(false);
        }
        if !drain_upto[index] {
            return Ok(true);
        }
        match pump_entry(stream, index, wanted[index], tx, decoded, stop) {
            Pumped::Done => {}
            Pumped::Stopped => {
                stopped = true;
                return Ok(false);
            }
            Pumped::Failed(err) => {
                failure = Some(err);
                stopped = true;
                return Ok(false);
            }
        }
        if wanted[index] {
            seen_wanted += 1;
            if tx.send(Msg::End(index, None)).is_err() {
                stopped = true;
                return Ok(false);
            }
        }
        Ok(true)
    });
    if let Some(e) = failure {
        let _ = tx.send(Msg::Fatal(e));
    } else if let Err(e) = walk
        && !stopped
        && seen_wanted < remaining_wanted
    {
        let _ = tx.send(Msg::Fatal(sevenz_err(e)));
    }
}

/// How [`pump_entry`] left one member.
enum Pumped {
    /// Read to the end.
    Done,
    /// Stop flag raised, receiver gone, or a decode failure already sent to the receiver.
    Stopped,
    /// Decode failure on a member nobody reads; the walk reports it as fatal.
    Failed(ArchiveError),
}

/// Worker side of ONE member: read it in 64 KiB chunks, sending the chunks when the member
/// is `wanted` and discarding them when it is only drained to reach a later member of the
/// same solid block. FER-498: the stop flag is checked before EVERY chunk on both paths and
/// again before a chunk is handed over, so once the receiver raised it at most the chunk
/// already being decoded is finished. Skipped bytes go nowhere, so the drain path gets no
/// backpressure from the channel; the flag is its only brake.
fn pump_entry(
    stream: &mut dyn Read,
    index: usize,
    wanted: bool,
    tx: &std::sync::mpsc::SyncSender<Msg>,
    decoded: &AtomicU64,
    stop: &AtomicBool,
) -> Pumped {
    let mut buf = [0u8; 64 * 1024];
    loop {
        if stop.load(Ordering::SeqCst) {
            return Pumped::Stopped;
        }
        match stream.read(&mut buf) {
            Ok(0) => return Pumped::Done,
            Ok(n) => {
                decoded.fetch_add(n as u64, Ordering::Relaxed);
                if wanted
                    && (stop.load(Ordering::SeqCst)
                        || tx.send(Msg::Chunk(index, buf[..n].to_vec())).is_err())
                {
                    return Pumped::Stopped;
                }
            }
            Err(e) => {
                let err = decode_io_err(e);
                if wanted {
                    let _ = tx.send(Msg::End(index, Some(err)));
                    return Pumped::Stopped;
                }
                return Pumped::Failed(err);
            }
        }
    }
}

#[cfg(test)]
mod solid_tests {
    use super::*;
    use sevenz_rust2::{ArchiveEntry as SzEntry, ArchiveWriter, SourceReader};
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQ: AtomicU64 = AtomicU64::new(0);

    fn solid_archive(members: &[(&str, Vec<u8>)]) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "ferrum-fer104-solid-{}-{}.7z",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        let f = File::create(&path).unwrap();
        let mut w = ArchiveWriter::new(f).unwrap();
        let entries: Vec<SzEntry> = members.iter().map(|(n, _)| SzEntry::new_file(n)).collect();
        let readers: Vec<SourceReader<&[u8]>> = members
            .iter()
            .map(|(_, b)| SourceReader::new(b.as_slice()))
            .collect();
        w.push_archive_entries(entries, readers).unwrap();
        w.finish().unwrap();
        path
    }

    fn read_member(
        path: &Path,
        name: &str,
        max: u64,
        guard: Option<&ReadGuard>,
    ) -> Result<(Vec<u8>, bool, bool), ArchiveError> {
        let mut reader = SevenZProvider::open_reader(path, None)?;
        let mut out = Vec::new();
        let (found, truncated) = stream_member(&mut reader, name, max, guard, &mut |c| {
            out.extend_from_slice(c);
            Ok(())
        })?;
        Ok((out, found, truncated))
    }

    #[test]
    fn solid_block_members_read_with_correct_content() {
        let members = [
            ("a.txt", b"AAAA first member AAAA".to_vec()),
            ("b.txt", b"BBBB second member BBBB".to_vec()),
            ("c.txt", b"CCCC third member CCCC".to_vec()),
        ];
        let p = solid_archive(&members);
        {
            let r = SevenZProvider::open_reader(&p, None).unwrap();
            assert!(r.archive().is_solid, "fixture must be a solid archive");
        }
        for (name, body) in &members {
            let (got, found, truncated) =
                read_member(&p, name, 1024, Some(&ReadGuard::PREVIEW)).unwrap();
            assert!(found && !truncated, "{name}");
            assert_eq!(&got, body, "{name}");
        }
        // Unbounded path (extraction) is fixed the same way.
        let (got, _, _) = read_member(&p, "c.txt", u64::MAX, None).unwrap();
        assert_eq!(got, members[2].1);
        // Missing member
        let (_, found, _) = read_member(&p, "nope.txt", 10, None).unwrap();
        assert!(!found);
        // Truncation still works mid-block
        let (got, _, truncated) = read_member(&p, "b.txt", 4, None).unwrap();
        assert_eq!(got, b"BBBB");
        assert!(truncated);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn drain_budget_refuses_a_solid_block_that_is_too_large_to_skip() {
        let members = [
            ("big.bin", vec![b'x'; 2 * 1024 * 1024]),
            ("tail.txt", b"tail".to_vec()),
        ];
        let p = solid_archive(&members);
        let tight = ReadGuard {
            max_skip_bytes: 1024 * 1024,
            ..ReadGuard::PREVIEW
        };
        let err = read_member(&p, "tail.txt", 100, Some(&tight)).unwrap_err();
        assert_eq!(err.code, ArchiveErrorCode::LimitExceeded);
        assert_eq!(err.message, SOLID_TOO_LARGE);
        // Within budget it works; reading the FIRST member skips nothing.
        let (got, _, _) = read_member(&p, "tail.txt", 100, Some(&ReadGuard::PREVIEW)).unwrap();
        assert_eq!(got, b"tail");
        let (_, found, _) = read_member(&p, "big.bin", 10, Some(&tight)).unwrap();
        assert!(found);
        // Time budget
        let instant = ReadGuard {
            max_skip_time: std::time::Duration::ZERO,
            ..ReadGuard::PREVIEW
        };
        let err = read_member(&p, "tail.txt", 100, Some(&instant)).unwrap_err();
        assert_eq!(err.message, SOLID_TOO_LARGE);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn codec_memory_math() {
        // LZMA: props byte + dict u32 LE
        let lzma = |d: u32| {
            let mut p = vec![0x5D];
            p.extend_from_slice(&d.to_le_bytes());
            p
        };
        assert_eq!(
            declared_codec_memory(&METHOD_LZMA, &lzma(8 << 20)),
            Some(Ok(8 << 20))
        );
        assert_eq!(
            declared_codec_memory(&METHOD_LZMA, &lzma(u32::MAX - 15)),
            Some(Ok((u32::MAX - 15) as u64))
        );
        assert_eq!(declared_codec_memory(&METHOD_LZMA, &[0x5D]), Some(Err(())));
        // LZMA2: one byte; 18 -> 2<<20 = 2 MiB.. 24 -> 2<<23 = 16 MiB; 40 = 4 GiB-1
        assert_eq!(
            declared_codec_memory(&METHOD_LZMA2, &[24]),
            Some(Ok(16 << 20))
        );
        assert_eq!(
            declared_codec_memory(&METHOD_LZMA2, &[25]),
            Some(Ok(24 << 20))
        );
        assert_eq!(
            declared_codec_memory(&METHOD_LZMA2, &[40]),
            Some(Ok(u32::MAX as u64))
        );
        assert!(
            declared_codec_memory(&METHOD_LZMA2, &[41])
                .unwrap()
                .is_err()
        );
        assert!(declared_codec_memory(&METHOD_LZMA2, &[]).unwrap().is_err());
        // PPMd: order + memory u32
        assert_eq!(
            declared_codec_memory(&METHOD_PPMD, &[6, 0, 0, 0, 1]),
            Some(Ok(1 << 24))
        );
        // Unrelated method: not our concern
        assert_eq!(declared_codec_memory(&[0x00], &[]), None);
    }

    #[test]
    fn real_archive_coder_properties_parse_and_limit_is_enforced() {
        let p = solid_archive(&[("a.txt", b"hello".to_vec())]);
        let reader = SevenZProvider::open_reader(&p, None).unwrap();
        let archive = reader.archive();
        // The writer's default LZMA2 dictionary is far below the cap: parse works.
        let coder = &archive.blocks[0].coders[0];
        let props = coder_properties(coder).expect("Debug shape of Coder must stay parseable");
        assert!(!props.is_empty());
        let mem = declared_codec_memory(coder.encoder_method_id(), &props)
            .expect("writer default is LZMA2")
            .expect("valid props");
        assert!(mem > 0 && mem <= 256 * 1024 * 1024, "{mem}");
        assert!(check_block_codec_memory(archive, 0, 256 * 1024 * 1024).is_ok());
        // A limit below the declared dictionary is refused with the safe message.
        let err = check_block_codec_memory(archive, 0, mem - 1).unwrap_err();
        assert_eq!(err.code, ArchiveErrorCode::LimitExceeded);
        assert_eq!(err.message, CODEC_TOO_LARGE);
        drop(reader);
        // And end-to-end through stream_member with a tiny codec cap.
        let g = ReadGuard {
            max_codec_memory: 1,
            ..ReadGuard::PREVIEW
        };
        let err = read_member(&p, "a.txt", 10, Some(&g)).unwrap_err();
        assert_eq!(err.message, CODEC_TOO_LARGE);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn decoder_threads_stay_single_for_every_7z_read() {
        // N1 regression. The crate exposes no getter for the thread count and
        // a real memory test needs a process-wide peak measurement over a
        // block of hundreds of MiB (slow in debug builds, flaky next to
        // parallel tests), so pin the configuration instead: the constant is 1
        // and open_reader applies it right where the reader is built.
        assert_eq!(DECODE_THREADS, 1);
        let src = include_str!("archive_sevenz.rs");
        let production = src.split("#[cfg(test)]").next().unwrap();
        // FER-488: `open_reader` (by path) delegates to `open_reader_file` (opened handle),
        // the one place where a reader is built.
        let by_path = production
            .find("fn open_reader(")
            .expect("open_reader exists");
        assert!(production[by_path..by_path + 600].contains("Self::open_reader_file(file"));
        let open = production
            .find("fn open_reader_file(")
            .expect("open_reader_file exists");
        let body = &production[open..open + 1400];
        assert!(
            body.contains("reader.set_thread_count(DECODE_THREADS)"),
            "open_reader_file must pin the decoder to one thread"
        );
        assert_eq!(production.matches("ArchiveReader::new(").count(), 1);
        assert!(!production.contains("ArchiveReader::open("));
        // And the test_7z path (archive_create_test) must not bypass it.
        let ct = include_str!("archive_create_test.rs");
        assert!(ct.contains("set_thread_count(1)"));
    }

    #[test]
    fn codec_memory_is_summed_over_the_coders_of_a_block() {
        let lzma2_16m = (METHOD_LZMA2.to_vec(), vec![24u8]); // 16 MiB each
        let two = vec![lzma2_16m.clone(), lzma2_16m.clone()];
        assert!(sum_within_limit(&two, 32 << 20));
        assert!(!sum_within_limit(&two, (32 << 20) - 1));
        let one = vec![lzma2_16m.clone()];
        assert!(sum_within_limit(&one, 16 << 20));
        // Non-sized methods add nothing; malformed props fail closed.
        let with_filter = vec![(vec![0x03u8], vec![]), lzma2_16m.clone()];
        assert!(sum_within_limit(&with_filter, 16 << 20));
        let bad = vec![(METHOD_LZMA2.to_vec(), vec![99u8])];
        assert!(!sum_within_limit(&bad, u64::MAX));
        // The real-archive path still passes at its single coder's size.
        let p = solid_archive(&[("a.txt", b"hello".to_vec())]);
        let reader = SevenZProvider::open_reader(&p, None).unwrap();
        let archive = reader.archive();
        let c = &archive.blocks[0].coders[0];
        let mem = declared_codec_memory(c.encoder_method_id(), &coder_properties(c).unwrap())
            .unwrap()
            .unwrap();
        assert!(check_block_codec_memory(archive, 0, mem).is_ok());
        assert!(check_block_codec_memory(archive, 0, mem - 1).is_err());
        drop(reader);
        let _ = std::fs::remove_file(&p);
    }

    fn extract_req(
        archive: &Path,
        dest: &Path,
        members: &[&str],
        conflict: crate::archive_contract::ArchiveConflictPolicy,
    ) -> crate::archive_contract::ArchiveExtractRequest {
        crate::archive_contract::ArchiveExtractRequest {
            archive_path: archive.to_string_lossy().into_owned(),
            member_paths: members
                .iter()
                .map(|m| MemberPath::parse(m).unwrap())
                .collect(),
            destination_path: dest.to_string_lossy().into_owned(),
            conflict,
            limits: crate::archive_contract::ArchiveLimits {
                max_entries: 10_000,
                max_total_bytes: u64::MAX,
                max_entry_bytes: u64::MAX,
                max_depth: 32,
            },
            password: None,
        }
    }

    fn many_members(n: usize) -> Vec<(String, Vec<u8>)> {
        (0..n)
            .map(|i| {
                let body: Vec<u8> = format!("member-{i}-")
                    .into_bytes()
                    .into_iter()
                    .cycle()
                    .take(10_000 + i * 37)
                    .collect();
                (format!("dir{}/file{i:03}.txt", i % 3), body)
            })
            .collect()
    }

    fn solid_of(members: &[(String, Vec<u8>)]) -> PathBuf {
        let refs: Vec<(&str, Vec<u8>)> = members
            .iter()
            .map(|(n, b)| (n.as_str(), b.clone()))
            .collect();
        solid_archive(&refs)
    }

    fn scratch_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "ferrum-fer437-{tag}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn solid_extract_decodes_each_byte_once_and_writes_every_member() {
        use crate::archive_contract::ArchiveConflictPolicy;
        let members = many_members(60);
        let p = solid_of(&members);
        {
            let r = SevenZProvider::open_reader(&p, None).unwrap();
            assert!(r.archive().is_solid);
            assert_eq!(r.archive().blocks.len(), 1, "one solid block");
        }
        let dest = scratch_dir("all");
        let decoded = AtomicU64::new(0);
        extract_counted(
            &extract_req(&p, &dest, &[], ArchiveConflictPolicy::Overwrite),
            &mut |_| true,
            &decoded,
            CANCEL_POLL,
        )
        .unwrap();
        for (name, body) in &members {
            let got = std::fs::read(dest.join(name)).unwrap_or_else(|_| panic!("{name}"));
            assert_eq!(&got, body, "{name}");
        }
        let total: u64 = members.iter().map(|(_, b)| b.len() as u64).sum();
        // O(n): the decoder produced every byte exactly once. The old
        // per-member re-drain would have produced ~n/2 times this.
        assert_eq!(decoded.load(Ordering::SeqCst), total);
        let _ = std::fs::remove_dir_all(&dest);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn solid_extract_of_a_selection_stops_after_the_last_selected_member() {
        use crate::archive_contract::ArchiveConflictPolicy;
        let members = many_members(30);
        let p = solid_of(&members);
        let dest = scratch_dir("sel");
        let decoded = AtomicU64::new(0);
        // file004 and file010 only (names sort by index, archive order is index order)
        let picks = [members[4].0.as_str(), members[10].0.as_str()];
        extract_counted(
            &extract_req(&p, &dest, &picks, ArchiveConflictPolicy::Overwrite),
            &mut |_| true,
            &decoded,
            CANCEL_POLL,
        )
        .unwrap();
        assert_eq!(
            std::fs::read(dest.join(&members[4].0)).unwrap(),
            members[4].1
        );
        assert_eq!(
            std::fs::read(dest.join(&members[10].0)).unwrap(),
            members[10].1
        );
        assert!(!dest.join(&members[11].0).exists());
        assert!(!dest.join(&members[5].0).exists());
        let upto_last: u64 = members[..=10].iter().map(|(_, b)| b.len() as u64).sum();
        assert_eq!(decoded.load(Ordering::SeqCst), upto_last);
        let _ = std::fs::remove_dir_all(&dest);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn solid_extract_conflict_skip_keeps_the_stream_aligned() {
        use crate::archive_contract::ArchiveConflictPolicy;
        let members = many_members(9);
        let p = solid_of(&members);
        let dest = scratch_dir("skip");
        // Pre-existing files make the pipeline skip members 2 and 5.
        for i in [2usize, 5] {
            let f = dest.join(&members[i].0);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(&f, b"keep me").unwrap();
        }
        extract_counted(
            &extract_req(&p, &dest, &[], ArchiveConflictPolicy::Skip),
            &mut |_| true,
            &AtomicU64::new(0),
            CANCEL_POLL,
        )
        .unwrap();
        for (i, (name, body)) in members.iter().enumerate() {
            let got = std::fs::read(dest.join(name)).unwrap();
            if i == 2 || i == 5 {
                assert_eq!(got, b"keep me", "{name}");
            } else {
                assert_eq!(&got, body, "{name}");
            }
        }
        let _ = std::fs::remove_dir_all(&dest);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn solid_extract_reports_a_corrupt_payload_and_leaves_no_partial_file() {
        use crate::archive_contract::ArchiveConflictPolicy;
        let members = many_members(6);
        let p = solid_of(&members);
        let mut bytes = std::fs::read(&p).unwrap();
        let mid = 32 + bytes.len() / 3;
        bytes[mid] ^= 0xFF;
        bytes[mid + 1] ^= 0xFF;
        std::fs::write(&p, &bytes).unwrap();
        let dest = scratch_dir("corrupt");
        let err = extract_counted(
            &extract_req(&p, &dest, &[], ArchiveConflictPolicy::Overwrite),
            &mut |_| true,
            &AtomicU64::new(0),
            CANCEL_POLL,
        );
        assert!(err.is_err(), "damaged payload must fail");
        let mut stack = vec![dest.clone()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap().flatten() {
                let path = e.path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    let n = path.file_name().unwrap().to_string_lossy().into_owned();
                    assert!(!n.contains("ferrum-partial"), "partial left: {n}");
                }
            }
        }
        let _ = std::fs::remove_dir_all(&dest);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn a_cancelled_token_stops_the_solid_skip_within_one_chunk() {
        use crate::archive_contract::{CancelToken, with_cancel};
        // A reader that never ends and cancels the token on its first read:
        // the skip loop must stop after that one chunk, not run on.
        struct Endless<'a> {
            token: &'a CancelToken,
            reads: u32,
        }
        impl Read for Endless<'_> {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                self.reads += 1;
                self.token.cancel();
                buf.fill(0);
                Ok(buf.len())
            }
        }
        let token = CancelToken::new();
        let mut src = Endless {
            token: &token,
            reads: 0,
        };
        let mut skipped = 0u64;
        let err = with_cancel(&token, || {
            drain_entry(&mut src, None, &mut skipped, std::time::Instant::now())
        })
        .unwrap_err();
        assert_eq!(err.code, ArchiveErrorCode::Cancelled);
        assert_eq!(src.reads, 1);
        assert_eq!(skipped, 64 * 1024);
        // Without a token the same call is not cancellable (finite source).
        let mut data: &[u8] = &[0u8; 1000];
        let mut skipped = 0u64;
        drain_entry(&mut data, None, &mut skipped, std::time::Instant::now()).unwrap();
        assert_eq!(skipped, 1000);
    }

    #[test]
    fn a_cancelled_token_aborts_a_member_read_of_a_solid_archive() {
        use crate::archive_contract::{CancelToken, with_cancel};
        let members = [
            ("big.bin", vec![b'x'; 1024 * 1024]),
            ("tail.txt", b"tail".to_vec()),
        ];
        let p = solid_archive(&members);
        let token = CancelToken::new();
        token.cancel();
        let err = with_cancel(&token, || read_member(&p, "tail.txt", 100, None)).unwrap_err();
        assert_eq!(err.code, ArchiveErrorCode::Cancelled);
        // The same read without a token still succeeds.
        let (got, _, _) = read_member(&p, "tail.txt", 100, None).unwrap();
        assert_eq!(got, b"tail");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn decode_io_err_maps_only_real_corruption_to_corrupt_archive() {
        use std::io::{Error, ErrorKind};
        let code = |e: Error| decode_io_err(e).code;
        // The single-thread LZMA2 reader's damage report.
        assert_eq!(
            code(Error::new(
                ErrorKind::InvalidInput,
                "corrupted input data (LZMA2:3)"
            )),
            ArchiveErrorCode::CorruptArchive
        );
        // Existing structural signals still map to corruption.
        assert_eq!(
            code(Error::new(ErrorKind::InvalidData, "bad")),
            ArchiveErrorCode::CorruptArchive
        );
        assert_eq!(
            code(Error::new(ErrorKind::UnexpectedEof, "eof")),
            ArchiveErrorCode::CorruptArchive
        );
        assert_eq!(
            code(Error::other("ChecksumVerificationFailed")),
            ArchiveErrorCode::CorruptArchive
        );
        // A bare "corrupt" text, or the phrase on the wrong kind, is just IO.
        assert_eq!(
            code(Error::other(
                "The file or directory is corrupted and unreadable."
            )),
            ArchiveErrorCode::Io
        );
        assert_eq!(
            code(Error::new(ErrorKind::InvalidInput, "corrupt")),
            ArchiveErrorCode::Io
        );
        assert_eq!(
            code(Error::new(
                ErrorKind::PermissionDenied,
                "corrupted input data"
            )),
            ArchiveErrorCode::Io
        );
        assert_eq!(
            code(Error::new(ErrorKind::BrokenPipe, "pipe")),
            ArchiveErrorCode::Io
        );
    }

    #[test]
    fn cancelling_during_a_long_solid_drain_returns_promptly() {
        use crate::archive_contract::ArchiveConflictPolicy;
        // 12 MiB member nobody asked for, then the selected one behind it.
        let big = 12 * 1024 * 1024u64;
        let p = solid_archive(&[
            ("big.bin", vec![0u8; big as usize]),
            ("tail.txt", b"tail".to_vec()),
        ]);
        // FER-459: the decoder drains the unwanted member at full speed, unthrottled by the
        // channel, so on a busy PC the first cancel poll can come after the whole drain is done
        // and there is nothing left to stop. That attempt proves nothing about the product, so
        // it is retried (the archive is reused). What is asserted is the product contract: once
        // the cancel is observed the worker stops within a few chunks instead of finishing.
        let mut conclusive = false;
        for _ in 0..5 {
            let dest = scratch_dir("cancel");
            let decoded = AtomicU64::new(0);
            let at_cancel = AtomicU64::new(0);
            let mut calls = 0u32;
            let err = extract_counted(
                &extract_req(&p, &dest, &["tail.txt"], ArchiveConflictPolicy::Overwrite),
                // First call is the pipeline's own pre-entry check; every later
                // poll (made while waiting for the drain) reports "cancel".
                &mut |_| {
                    calls += 1;
                    if calls == 2 {
                        at_cancel.store(decoded.load(Ordering::SeqCst), Ordering::SeqCst);
                    }
                    calls < 2
                },
                &decoded,
                std::time::Duration::from_millis(1),
            )
            .unwrap_err();
            assert_eq!(err.code, ArchiveErrorCode::Cancelled);
            assert!(!dest.join("tail.txt").exists());
            let _ = std::fs::remove_dir_all(&dest);
            let (seen, last) = (
                at_cancel.load(Ordering::SeqCst),
                decoded.load(Ordering::SeqCst),
            );
            if seen >= big {
                continue; // drain was already over when the cancel was first polled
            }
            // The worker stopped at its next 64 KiB chunk (the scope joined it before we got
            // here); a worker that ignored the cancel would have decoded the remaining bytes.
            assert!(
                last - seen < (big - seen).min(big / 2),
                "drain kept running after cancel: {seen} -> {last} of {big} bytes"
            );
            conclusive = true;
            break;
        }
        let _ = std::fs::remove_file(&p);
        assert!(
            conclusive,
            "the drain always finished before the first cancel poll"
        );
    }

    #[test]
    fn cancel_is_honoured_while_a_fast_decoder_never_lets_the_channel_time_out() {
        // FER-459: the producer always has the next chunk ready, so `recv_timeout` never
        // expires; cancel used to be asked only on expiry and so was never seen.
        use std::sync::atomic::AtomicBool;
        use std::time::{Duration, Instant};
        let (tx, rx) = std::sync::mpsc::sync_channel::<Msg>(4);
        let producer = std::thread::spawn(move || {
            let until = Instant::now() + Duration::from_secs(5);
            while Instant::now() < until {
                if tx.send(Msg::Chunk(0, vec![0u8; 4096])).is_err() {
                    return; // receiver gone
                }
            }
        });
        let cancel = AtomicBool::new(false);
        let cancelled_at = std::sync::Mutex::new(None::<Instant>);
        let mut polls = 0u32;
        let mut sink = Vec::new();
        let err = std::thread::scope(|s| {
            s.spawn(|| {
                std::thread::sleep(Duration::from_millis(150));
                *cancelled_at.lock().unwrap() = Some(Instant::now());
                cancel.store(true, Ordering::SeqCst);
            });
            receive_member(
                &rx,
                1,
                &mut sink,
                Duration::from_millis(100),
                &AtomicBool::new(false),
                &mut || {
                    polls += 1;
                    cancel.load(Ordering::SeqCst)
                },
            )
            .unwrap_err()
        });
        let returned = Instant::now();
        drop(rx);
        producer.join().unwrap();
        assert_eq!(err.code, ArchiveErrorCode::Cancelled);
        let after = returned.duration_since(cancelled_at.lock().unwrap().unwrap());
        assert!(after < Duration::from_millis(300), "cancel took {after:?}");
        assert!(polls >= 2, "cancel must be polled while data keeps flowing");
        assert!(sink.is_empty(), "skipped bytes are never written");
    }

    #[test]
    fn a_failure_on_a_skipped_member_keeps_its_own_error() {
        let (tx, rx) = std::sync::mpsc::sync_channel::<Msg>(4);
        tx.send(Msg::End(
            0,
            Some(ArchiveError::new(
                ArchiveErrorCode::CorruptArchive,
                "crc mismatch",
            )),
        ))
        .unwrap();
        drop(tx);
        let mut sink = Vec::new();
        let stop = std::sync::atomic::AtomicBool::new(false);
        let err = receive_member(
            &rx,
            1,
            &mut sink,
            std::time::Duration::from_millis(1),
            &stop,
            &mut || false,
        )
        .unwrap_err();
        assert_eq!(err.code, ArchiveErrorCode::CorruptArchive);
        assert!(
            stop.load(Ordering::SeqCst),
            "any failure stops the worker too"
        );
        assert_eq!(err.message, "crc mismatch");
    }

    /// FER-498 harness. A fake decoder that never blocks and never ends feeds member 0
    /// through [`pump_entry`] on a worker thread; the test thread runs [`receive_member`]
    /// for member 1 (so member 0 is only drained) or member 0 (so it is sent).
    /// On read `CANCEL_AT` the decoder raises the user's cancel and then holds that chunk
    /// until the stop flag is up (bounded, so a missing stop fails instead of hanging).
    /// Nothing here depends on timing: the counts are exact.
    const CANCEL_AT: u64 = 5;

    struct EndlessDecoder<'a> {
        reads: &'a AtomicU64,
        reads_after_stop: &'a AtomicU64,
        user_cancel: &'a AtomicBool,
        stop: &'a AtomicBool,
    }

    impl Read for EndlessDecoder<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.stop.load(Ordering::SeqCst) {
                self.reads_after_stop.fetch_add(1, Ordering::SeqCst);
            }
            if self.reads.fetch_add(1, Ordering::SeqCst) + 1 == CANCEL_AT {
                self.user_cancel.store(true, Ordering::SeqCst);
                let give_up = std::time::Instant::now() + std::time::Duration::from_secs(10);
                while !self.stop.load(Ordering::SeqCst) && std::time::Instant::now() < give_up {
                    std::thread::yield_now();
                }
            }
            buf.fill(7);
            Ok(buf.len())
        }
    }

    struct CancelRun {
        err: ArchiveError,
        stop_raised_by_receiver: bool,
        reads: u64,
        reads_after_stop: u64,
        written: usize,
    }

    fn run_cancel_harness(receive_idx: usize) -> CancelRun {
        let (tx, rx) = std::sync::mpsc::sync_channel::<Msg>(4);
        let (reads, reads_after_stop) = (AtomicU64::new(0), AtomicU64::new(0));
        let (user_cancel, stop) = (AtomicBool::new(false), AtomicBool::new(false));
        let decoded = AtomicU64::new(0);
        let mut sink = Vec::new();
        let (err, stop_raised_by_receiver) = std::thread::scope(|s| {
            let worker = s.spawn(|| {
                let mut decoder = EndlessDecoder {
                    reads: &reads,
                    reads_after_stop: &reads_after_stop,
                    user_cancel: &user_cancel,
                    stop: &stop,
                };
                let wanted = receive_idx == 0;
                let outcome = pump_entry(&mut decoder, 0, wanted, &tx, &decoded, &stop);
                assert!(matches!(outcome, Pumped::Stopped), "the worker must stop");
            });
            let err = receive_member(
                &rx,
                receive_idx,
                &mut sink,
                std::time::Duration::from_millis(1),
                &stop,
                &mut || user_cancel.load(Ordering::SeqCst),
            )
            .unwrap_err();
            // Read the flag BEFORE the caller's own end-of-extract store below.
            let raised = stop.load(Ordering::SeqCst);
            stop.store(true, Ordering::SeqCst);
            drop(rx);
            worker.join().unwrap();
            (err, raised)
        });
        CancelRun {
            err,
            stop_raised_by_receiver,
            reads: reads.load(Ordering::SeqCst),
            reads_after_stop: reads_after_stop.load(Ordering::SeqCst),
            written: sink.len(),
        }
    }

    #[test]
    fn cancel_stops_the_drain_of_an_unwanted_member_within_one_chunk() {
        let run = run_cancel_harness(1);
        assert_eq!(run.err.code, ArchiveErrorCode::Cancelled);
        assert!(
            run.stop_raised_by_receiver,
            "the receiver must raise the stop flag the moment it sees the cancel"
        );
        // Chunk CANCEL_AT was in flight when the cancel came; not one more is decoded.
        assert_eq!(run.reads, CANCEL_AT, "drain went on after the cancel");
        assert_eq!(run.reads_after_stop, 0);
        assert_eq!(run.written, 0, "drained bytes are never written");
    }

    #[test]
    fn cancel_stops_the_decode_of_a_wanted_member_within_one_chunk() {
        let run = run_cancel_harness(0);
        assert_eq!(run.err.code, ArchiveErrorCode::Cancelled);
        assert!(run.stop_raised_by_receiver);
        assert!(
            run.reads <= CANCEL_AT + 1 && run.reads_after_stop <= 1,
            "decode went on after the cancel: {} reads, {} after stop",
            run.reads,
            run.reads_after_stop
        );
        assert!(run.written as u64 <= (CANCEL_AT - 1) * 64 * 1024);
    }

    #[test]
    fn the_pipeline_cancel_check_raises_the_stop_flag_before_cleanup() {
        // The end-to-end path: once `extract_counted` sees the cancel, the decoder must not
        // run on while the pipeline removes its staging file and rolls back directories.
        // Bytes decoded after the cancel are bounded by a few chunks, independent of how long
        // that cleanup takes, because the flag is raised inside the cancel check itself.
        let src = include_str!("archive_sevenz.rs");
        let production = src.split("#[cfg(test)]").next().unwrap();
        let body = &production[production.find("fn extract_counted(").unwrap()..];
        let body = &body[..body.find("\n}").unwrap()];
        let check = body
            .find("let cancel_seen = ||")
            .expect("shared cancel check");
        let check_body = &body[check..check + body[check..].find("};").unwrap()];
        assert!(
            check_body.contains("stop.store(true"),
            "cancel check must raise the stop flag itself"
        );
        assert!(
            !body.contains("!emit(done.get(), None))"),
            "every cancel check goes through cancel_seen"
        );
    }

    use crate::archive_contract::RawLevelRow;

    fn row(name: &str, is_dir: bool, size: u64) -> RawLevelRow {
        (name.to_owned(), is_dir, size, None)
    }

    fn names(rows: &[RawLevelRow]) -> Vec<(&str, bool)> {
        rows.iter().map(|(n, d, _, _)| (n.as_str(), *d)).collect()
    }

    fn list_level(
        dir: &str,
        members: &[(&str, bool, u64)],
    ) -> Result<Vec<RawLevelRow>, ArchiveError> {
        let prefix = if dir.is_empty() {
            String::new()
        } else {
            format!("{dir}/")
        };
        let rows = members
            .iter()
            .filter_map(|(raw, d, s)| level_row(&prefix, raw, *d, *s, None))
            .collect();
        dedup_level(dir, rows)
    }

    #[test]
    fn sevenz_synthetic_duplicate_files_conflict() {
        let err = dedup_level(
            "",
            vec![row("file.txt", false, 10), row("file.txt", false, 20)],
        )
        .unwrap_err();
        assert_eq!(err.code, ArchiveErrorCode::Conflict);
        assert_eq!(err.member_path.as_deref(), Some("file.txt"));
    }

    #[test]
    fn sevenz_synthetic_case_variant_files_conflict() {
        let err = dedup_level(
            "",
            vec![row("file.txt", false, 10), row("FILE.TXT", false, 20)],
        )
        .unwrap_err();
        assert_eq!(err.code, ArchiveErrorCode::Conflict);
        assert_eq!(err.member_path.as_deref(), Some("FILE.TXT"));
    }

    #[test]
    fn sevenz_synthetic_file_and_folder_conflict() {
        let err =
            dedup_level("", vec![row("items", false, 10), row("items", true, 0)]).unwrap_err();
        assert_eq!(err.code, ArchiveErrorCode::Conflict);
        assert_eq!(err.member_path.as_deref(), Some("items"));
    }

    #[test]
    fn sevenz_synthetic_folder_case_variants_stay_separate() {
        let members = [("docs/a.txt", false, 5), ("Docs/b.txt", false, 10)];
        let root = list_level("", &members).unwrap();
        assert_eq!(names(&root), vec![("docs", true), ("Docs", true)]);
        let lower = list_level("docs", &members).unwrap();
        assert_eq!(names(&lower), vec![("a.txt", false)]);
        let upper = list_level("Docs", &members).unwrap();
        assert_eq!(names(&upper), vec![("b.txt", false)]);
    }

    #[test]
    fn sevenz_synthetic_identical_folders_merge() {
        let merged = dedup_level("", vec![row("docs", true, 0), row("docs", true, 0)]).unwrap();
        assert_eq!(names(&merged), vec![("docs", true)]);
    }
}
