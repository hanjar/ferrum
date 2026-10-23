//! FER-588: counts in a 7z header must never size an allocation without a bound.
//!
//! Before FER-588 a 42-byte archive whose plain kHeader declares 2^36 files passed Ferrum's
//! encoded-header pre-check (FER-587) and then made `sevenz-rust2` run `vec![..; 2^36]`: the
//! allocation failed and the whole process ABORTED (not a panic, so nothing could catch it).
//! The vendored, patched crate (`vendor/sevenz-rust2`, see FERRUM-PATCHES.md there) bounds every
//! header count against the unread header bytes and an absolute cap, and allocates fallibly.
//!
//! This binary installs a counting global allocator: it records the largest single allocation
//! made on the current test thread while "armed", and refuses (returns null for) anything above
//! `CEILING`, so a regression shows up as a failed test or an abort of this test binary only.
//!
//! Every hostile archive is built byte by byte here with valid start-header and next-header
//! CRCs, so it is the count check that rejects it and not a checksum.

#[path = "../src/archive_contract.rs"]
#[allow(dead_code)]
mod archive_contract;
#[allow(clippy::duplicate_mod, dead_code)]
#[path = "../src/archive_create_test.rs"]
mod archive_create_test;
#[allow(clippy::duplicate_mod, dead_code)]
#[path = "../src/archive_extract.rs"]
mod archive_extract;
#[path = "../src/archive_sevenz.rs"]
#[allow(dead_code)]
mod archive_sevenz;
#[path = "../src/archive_publish.rs"]
pub mod publish;

use archive_contract::{
    ArchiveConflictPolicy, ArchiveError, ArchiveErrorCode as Code, ArchiveExtractRequest,
    ArchiveLimits, ArchiveListRequest, ArchiveProvider, ArchiveReadRequest, Location, MemberPath,
};
use archive_sevenz::SevenZProvider;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

// ------------------------------------------------------ counting allocator ---

/// Largest single allocation allowed while armed. The patched crate never needs more than the
/// (capped) header buffers; FER-587 caps raw and decoded headers at 128 MiB in Ferrum and the
/// crate itself at 256 MiB, so anything bigger is a count-driven allocation.
const CEILING: usize = 256 << 20;

thread_local! {
    static ARMED: Cell<bool> = const { Cell::new(false) };
    static MAX_ONE: Cell<usize> = const { Cell::new(0) };
}

struct Counting;

impl Counting {
    /// Records `size`; false when the request must be refused.
    fn note(size: usize) -> bool {
        ARMED
            .try_with(|armed| {
                if !armed.get() {
                    return true;
                }
                let _ = MAX_ONE.try_with(|m| m.set(m.get().max(size)));
                size <= CEILING
            })
            .unwrap_or(true)
    }
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if !Self::note(layout.size()) {
            return std::ptr::null_mut();
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if !Self::note(layout.size()) {
            return std::ptr::null_mut();
        }
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if !Self::note(new_size) {
            return std::ptr::null_mut();
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

/// Runs `f` with the allocator armed on this thread; returns its result and the largest single
/// allocation it made.
fn armed<T>(f: impl FnOnce() -> T) -> (T, usize) {
    MAX_ONE.with(|m| m.set(0));
    ARMED.with(|a| a.set(true));
    let out = f();
    ARMED.with(|a| a.set(false));
    (out, MAX_ONE.with(|m| m.get()))
}

// ------------------------------------------------------------ 7z builder ---

const K_END: u8 = 0x00;
const K_HEADER: u8 = 0x01;
const K_MAIN_STREAMS_INFO: u8 = 0x04;
const K_FILES_INFO: u8 = 0x05;
const K_PACK_INFO: u8 = 0x06;
const K_UNPACK_INFO: u8 = 0x07;
const K_SUB_STREAMS_INFO: u8 = 0x08;
const K_SIZE: u8 = 0x09;
const K_CRC: u8 = 0x0A;
const K_FOLDER: u8 = 0x0B;
const K_CODERS_UNPACK_SIZE: u8 = 0x0C;
const K_NUM_UNPACK_STREAM: u8 = 0x0D;
const K_EMPTY_STREAM: u8 = 0x0E;
const K_NAME: u8 = 0x11;
const K_ENCODED_HEADER: u8 = 0x17;

/// 7z variable-length number, exactly as 7-Zip's `WriteNumber`.
fn num(value: u64) -> Vec<u8> {
    let mut first = 0u8;
    let mut mask = 0x80u8;
    let mut extra = 0usize;
    while extra < 8 {
        if value < (1u64 << (7 * (extra + 1))) {
            first |= (value >> (8 * extra)) as u8;
            break;
        }
        first |= mask;
        mask >>= 1;
        extra += 1;
    }
    let mut out = vec![first];
    out.extend_from_slice(&value.to_le_bytes()[..extra]);
    out
}

/// Concatenate header fragments.
fn cat(parts: &[&[u8]]) -> Vec<u8> {
    parts.concat()
}

/// Full archive: signature header, `packed` data, then `next_header`, all CRCs valid.
fn archive(packed: &[u8], next_header: &[u8]) -> Vec<u8> {
    let mut out = vec![0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C, 0, 4, 0, 0, 0, 0];
    out.extend_from_slice(&(packed.len() as u64).to_le_bytes());
    out.extend_from_slice(&(next_header.len() as u64).to_le_bytes());
    out.extend_from_slice(&crc32fast::hash(next_header).to_le_bytes());
    out.extend_from_slice(packed);
    out.extend_from_slice(next_header);
    fix_crcs(&mut out);
    out
}

/// Recompute the next-header CRC and the start-header CRC (after a mutation).
fn fix_crcs(b: &mut [u8]) {
    if b.len() < 32 {
        return;
    }
    let off = u64::from_le_bytes(b[12..20].try_into().unwrap());
    let size = u64::from_le_bytes(b[20..28].try_into().unwrap());
    if let Some(end) = 32u64.checked_add(off).and_then(|s| s.checked_add(size))
        && end <= b.len() as u64
    {
        let crc = crc32fast::hash(&b[(32 + off) as usize..end as usize]);
        b[28..32].copy_from_slice(&crc.to_le_bytes());
    }
    let crc = crc32fast::hash(&b[12..32]);
    b[8..12].copy_from_slice(&crc.to_le_bytes());
}

/// Streams info for `packed_len` bytes stored with the Copy coder in one block, with
/// `sub_streams` (if any) appended before its kEnd.
fn copy_streams_info(packed_len: u64, unpack_len: u64, sub_streams: &[u8]) -> Vec<u8> {
    cat(&[
        &[K_PACK_INFO],
        &num(0),
        &num(1),
        &[K_SIZE],
        &num(packed_len),
        &[K_END],
        &[K_UNPACK_INFO, K_FOLDER],
        &num(1),
        &[0x00],             // not external
        &[0x01, 0x01, 0x00], // one coder; simple, id size 1; id 00 = Copy
        &[K_CODERS_UNPACK_SIZE],
        &num(unpack_len),
        &[K_END],
        sub_streams,
        &[K_END],
    ])
}

/// UTF-16LE names with terminators, as kName stores them.
fn names(list: &[&str]) -> Vec<u8> {
    let mut out = vec![0x00]; // not external
    for name in list {
        for unit in name.encode_utf16() {
            out.extend_from_slice(&unit.to_le_bytes());
        }
        out.extend_from_slice(&[0, 0]);
    }
    out
}

/// Plain header of a well-formed archive: one stored file `a.txt` ("hello") and one empty
/// directory `d`. The sane twin of the hostile builders below.
fn sane_plain() -> Vec<u8> {
    let data = b"hello";
    let name_prop = names(&["a.txt", "d"]);
    let header = cat(&[
        &[K_HEADER, K_MAIN_STREAMS_INFO],
        &copy_streams_info(
            data.len() as u64,
            data.len() as u64,
            &[K_SUB_STREAMS_INFO, K_END],
        ),
        &[K_FILES_INFO],
        &num(2),
        &[K_EMPTY_STREAM],
        &num(1),
        &[0b0100_0000], // file 1 (`d`) has no stream
        &[K_NAME],
        &num(name_prop.len() as u64),
        &name_prop,
        &[K_END, K_END],
    ]);
    archive(data, &header)
}

/// kHeader declaring `count` files and nothing else (the FER-587 review's 42-byte archive for
/// 2^36).
fn plain_huge_files(count: u64) -> Vec<u8> {
    archive(
        &[],
        &cat(&[&[K_HEADER, K_FILES_INFO], &num(count), &[K_END, K_END]]),
    )
}

/// kPackInfo declaring `count` pack streams.
fn plain_huge_pack_streams(count: u64) -> Vec<u8> {
    archive(
        &[],
        &cat(&[
            &[K_HEADER, K_MAIN_STREAMS_INFO, K_PACK_INFO],
            &num(0),
            &num(count),
            &[K_SIZE, 0x01, K_END, K_END, K_END],
        ]),
    )
}

/// kPackInfo declaring `count` pack streams with an all-defined CRC vector (no kSize).
fn plain_huge_pack_crcs(count: u64) -> Vec<u8> {
    archive(
        &[],
        &cat(&[
            &[K_HEADER, K_MAIN_STREAMS_INFO, K_PACK_INFO],
            &num(0),
            &num(count),
            &[K_CRC, 0x01, 0, 0, 0, 0, K_END, K_END, K_END],
        ]),
    )
}

/// kFolder declaring `count` folders.
fn plain_huge_folders(count: u64) -> Vec<u8> {
    archive(
        &[],
        &cat(&[
            &[K_HEADER, K_MAIN_STREAMS_INFO, K_UNPACK_INFO, K_FOLDER],
            &num(count),
            &[
                0x00,
                0x01,
                0x01,
                0x00,
                K_CODERS_UNPACK_SIZE,
                0x00,
                K_END,
                K_END,
                K_END,
            ],
        ]),
    )
}

/// One folder declaring `count` coders.
fn plain_huge_coders(count: u64) -> Vec<u8> {
    archive(
        &[],
        &cat(&[
            &[K_HEADER, K_MAIN_STREAMS_INFO, K_UNPACK_INFO, K_FOLDER],
            &num(1),
            &[0x00],
            &num(count),
            &[0x01, 0x00, K_CODERS_UNPACK_SIZE, 0x00, K_END, K_END, K_END],
        ]),
    )
}

/// One complex coder declaring `count` in and out streams (bind pairs / packed streams).
fn plain_huge_coder_streams(count: u64) -> Vec<u8> {
    archive(
        &[],
        &cat(&[
            &[K_HEADER, K_MAIN_STREAMS_INFO, K_UNPACK_INFO, K_FOLDER],
            &num(1),
            &[0x00, 0x01, 0x11, 0x00], // not external, one coder: complex, id size 1, id 00
            &num(count),
            &num(count),
            &[K_CODERS_UNPACK_SIZE, 0x00, K_END, K_END, K_END],
        ]),
    )
}

/// One coder whose properties claim `count` bytes.
fn plain_huge_coder_props(count: u64) -> Vec<u8> {
    archive(
        &[],
        &cat(&[
            &[K_HEADER, K_MAIN_STREAMS_INFO, K_UNPACK_INFO, K_FOLDER],
            &num(1),
            &[0x00, 0x01],
            &[0x21, 0x21], // simple coder with properties, id 21 = LZMA2
            &num(count),
            &[0x10, K_CODERS_UNPACK_SIZE, 0x00, K_END, K_END, K_END],
        ]),
    )
}

/// One stored block split into `count` sub-streams.
fn plain_huge_substreams(count: u64) -> Vec<u8> {
    let data = b"hello";
    let sub = cat(&[
        &[K_SUB_STREAMS_INFO, K_NUM_UNPACK_STREAM],
        &num(count),
        &[K_END],
    ]);
    archive(
        data,
        &cat(&[
            &[K_HEADER, K_MAIN_STREAMS_INFO],
            &copy_streams_info(5, 5, &sub),
            &[K_END],
        ]),
    )
}

/// kEmptyStream bit vector shorter than the declared file count.
fn plain_short_empty_stream_bits() -> Vec<u8> {
    archive(
        &[],
        &cat(&[
            &[K_HEADER, K_FILES_INFO],
            &num(32), // 32 bits fit in the 5 bytes left, but the vector itself has 1 byte
            &[K_EMPTY_STREAM, 0x01, 0xFF, K_END, K_END],
        ]),
    )
}

/// Encoded header (Copy coder) whose decoded kHeader is `inner` (a complete next header).
fn encoded(inner: &[u8]) -> Vec<u8> {
    let len = inner.len() as u64;
    let header = cat(&[&[K_ENCODED_HEADER], &copy_streams_info(len, len, &[])]);
    archive(inner, &header)
}

/// The next-header bytes of an archive built above.
fn next_header_of(full: &[u8]) -> Vec<u8> {
    let off = u64::from_le_bytes(full[12..20].try_into().unwrap()) as usize;
    full[32 + off..].to_vec()
}

// -------------------------------------------------------- Ferrum surfaces ---

static SEQ: AtomicU64 = AtomicU64::new(0);

/// Owned temp dir, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "ferrum-7zcounts-{tag}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn list(path: &Path) -> Result<usize, ArchiveError> {
    SevenZProvider::new()
        .list(&ArchiveListRequest {
            location: Location::Archive {
                archive_path: path.to_string_lossy().into_owned(),
                member_path: MemberPath::parse("").unwrap(),
            },
            request_token: SEQ.fetch_add(1, Ordering::SeqCst),
            password: None,
        })
        .map(|r| r.entries.len())
}

fn read(path: &Path, member: &str) -> Result<Vec<u8>, ArchiveError> {
    let mut out = Vec::new();
    SevenZProvider::new()
        .read(
            &ArchiveReadRequest {
                location: Location::Archive {
                    archive_path: path.to_string_lossy().into_owned(),
                    member_path: MemberPath::parse(member).unwrap(),
                },
                max_bytes: 1 << 20,
                password: None,
            },
            &mut |chunk| {
                out.extend_from_slice(chunk);
                Ok(())
            },
        )
        .map(|_| out)
}

fn extract(path: &Path, dest: &Path) -> Result<(), ArchiveError> {
    SevenZProvider::new().extract(
        &ArchiveExtractRequest {
            archive_path: path.to_string_lossy().into_owned(),
            member_paths: Vec::new(),
            destination_path: dest.to_string_lossy().into_owned(),
            conflict: ArchiveConflictPolicy::Overwrite,
            limits: ArchiveLimits {
                max_entries: 100_000,
                max_total_bytes: u64::MAX,
                max_entry_bytes: 512 * 1024 * 1024,
                max_depth: 32,
            },
            password: None,
        },
        &mut |_| true,
    )
}

fn test(path: &Path) -> Result<usize, ArchiveError> {
    archive_create_test::test_7z(path, None).map(|r| r.entries.len())
}

/// `bytes` must be refused as a corrupt archive by list, read, extract and test, without any
/// allocation above the ceiling and with nothing written to the destination.
fn assert_refused_everywhere(tag: &str, bytes: &[u8], expect_in_message: &str) {
    let tmp = TempDir::new(tag);
    let path = tmp.path().join(format!("{tag}.7z"));
    fs::write(&path, bytes).unwrap();
    let dest = tmp.path().join("out");
    fs::create_dir_all(&dest).unwrap();

    let ((l, r, x, t), max_one) = armed(|| {
        (
            list(&path).map(|_| ()),
            read(&path, "a.txt").map(|_| ()),
            extract(&path, &dest),
            test(&path).map(|_| ()),
        )
    });
    for (surface, result) in [("list", l), ("read", r), ("extract", x), ("test", t)] {
        let err = result.expect_err(&format!("{tag}: {surface} must fail"));
        assert_eq!(
            err.code,
            Code::CorruptArchive,
            "{tag}: {surface} code ({})",
            err.message
        );
        assert!(
            err.message.contains(expect_in_message),
            "{tag}: {surface} must be refused by the count check, got {:?}",
            err.message
        );
    }
    assert!(
        max_one <= 1 << 20,
        "{tag}: largest allocation {max_one} bytes"
    );
    assert!(
        fs::read_dir(&dest).unwrap().next().is_none(),
        "{tag}: nothing may be extracted"
    );
}

// ------------------------------------------------------------------ tests ---

#[test]
fn builder_produces_an_archive_ferrum_opens() {
    // Guards the builder: the hostile archives below differ from this one only in a count.
    let tmp = TempDir::new("sane");
    let path = tmp.path().join("sane.7z");
    fs::write(&path, sane_plain()).unwrap();
    assert_eq!(list(&path).unwrap(), 2);
    assert_eq!(read(&path, "a.txt").unwrap(), b"hello");
    assert_eq!(test(&path).unwrap(), 2);

    let enc = tmp.path().join("sane-encoded.7z");
    fs::write(&enc, encoded(&next_header_of(&sane_plain()))).unwrap();
    // The encoded variant points its files at the header's own pack data, so only the
    // listing is meaningful: the decoded header must parse.
    assert_eq!(list(&enc).unwrap(), 2);
}

#[test]
fn review_42_byte_archive_with_2_pow_36_files_is_an_error_not_an_abort() {
    let bytes = plain_huge_files(1 << 36);
    assert_eq!(bytes.len(), 42, "the exact archive from the FER-587 review");
    assert_refused_everywhere("files-2p36", &bytes, "num files");
}

#[test]
fn huge_file_counts_are_refused() {
    for (i, count) in [1u64 << 20, 1 << 28, u32::MAX as u64, 1 << 62, u64::MAX]
        .into_iter()
        .enumerate()
    {
        assert_refused_everywhere(&format!("files-{i}"), &plain_huge_files(count), "num files");
    }
    // Within the absolute cap but far beyond what the header bytes can describe.
    assert_refused_everywhere("files-short", &plain_huge_files(1000), "num files");
}

#[test]
fn huge_pack_stream_counts_are_refused() {
    assert_refused_everywhere(
        "pack-sizes",
        &plain_huge_pack_streams(1 << 40),
        "num pack streams",
    );
    assert_refused_everywhere(
        "pack-sizes-small",
        &plain_huge_pack_streams(100),
        "num pack streams",
    );
    assert_refused_everywhere(
        "pack-crcs",
        &plain_huge_pack_crcs(1 << 40),
        "num pack streams",
    );
}

#[test]
fn huge_folder_and_coder_counts_are_refused() {
    assert_refused_everywhere("folders", &plain_huge_folders(1 << 40), "num blocks");
    assert_refused_everywhere("folders-small", &plain_huge_folders(5000), "num blocks");
    assert_refused_everywhere("coders", &plain_huge_coders(1 << 40), "num coders");
    assert_refused_everywhere("coders-65", &plain_huge_coders(65), "num coders");
    assert_refused_everywhere(
        "coder-streams",
        &plain_huge_coder_streams(1 << 40),
        "coder streams",
    );
    assert_refused_everywhere(
        "coder-props",
        &plain_huge_coder_props(1 << 40),
        "properties size",
    );
}

#[test]
fn huge_substream_counts_are_refused() {
    assert_refused_everywhere("substreams", &plain_huge_substreams(1 << 40), "numStreams");
    assert_refused_everywhere(
        "substreams-max",
        &plain_huge_substreams(u64::MAX),
        "numStreams",
    );
}

#[test]
fn short_bit_vector_is_refused() {
    assert_refused_everywhere(
        "empty-stream-bits",
        &plain_short_empty_stream_bits(),
        "bit vector",
    );
}

#[test]
fn encoded_header_with_huge_counts_is_refused_after_decoding() {
    // The FER-587 pre-check accepts these (the encoded header itself is small and stored);
    // the counts only appear in the decoded kHeader, which the patched crate bounds.
    let cases: [(&str, Vec<u8>, &str); 4] = [
        ("enc-files", plain_huge_files(1 << 36), "num files"),
        (
            "enc-pack",
            plain_huge_pack_streams(1 << 40),
            "num pack streams",
        ),
        ("enc-folders", plain_huge_folders(1 << 40), "num blocks"),
        (
            "enc-substreams",
            plain_huge_substreams(1 << 40),
            "numStreams",
        ),
    ];
    for (tag, plain, expect) in cases {
        let bytes = encoded(&next_header_of(&plain));
        let pre = archive_contract::sevenz_header::precheck_encoded_header(
            &mut Cursor::new(&bytes),
            archive_contract::sevenz_header::HEADER_CODEC_MAX_MEMORY,
        );
        assert!(pre.is_ok(), "{tag}: the pre-check is not what refuses it");
        assert_refused_everywhere(tag, &bytes, expect);
    }
}

#[test]
fn oversized_next_header_is_refused_by_the_crate_without_allocating_it() {
    // Defence in depth below the FER-587 pre-check: the crate itself caps the next header and
    // only buffers bytes that exist.
    let mut bytes = sane_plain();
    bytes[20..28].copy_from_slice(&(1u64 << 40).to_le_bytes());
    fix_crcs(&mut bytes);
    let (res, max_one) = armed(|| {
        sevenz_rust2::ArchiveReader::new(Cursor::new(bytes), sevenz_rust2::Password::empty())
            .map(|_| ())
    });
    assert!(res.is_err());
    assert!(max_one <= 1 << 20, "largest allocation {max_one}");

    // Declared larger than the file: an EOF error after reading what is there.
    let mut bytes = sane_plain();
    let real = u64::from_le_bytes(bytes[20..28].try_into().unwrap());
    bytes[20..28].copy_from_slice(&(real + (64 << 20)).to_le_bytes());
    fix_crcs(&mut bytes);
    let (res, max_one) = armed(|| {
        sevenz_rust2::ArchiveReader::new(Cursor::new(bytes), sevenz_rust2::Password::empty())
            .map(|_| ())
    });
    assert!(res.is_err());
    assert!(max_one <= 1 << 20, "largest allocation {max_one}");
}

// ------------------------------------------------------- differential fuzz ---

struct XorShift(u64);

impl XorShift {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn spike(name: &str) -> Vec<u8> {
    fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../contract-fixtures/archive-spike")
            .join(name),
    )
    .unwrap()
}

fn written_7z(dir: &Path, name: &str, files: usize, solid: bool) -> Vec<u8> {
    use sevenz_rust2::{ArchiveEntry, ArchiveWriter};
    let path = dir.join(name);
    let mut w = ArchiveWriter::create(&path).unwrap();
    if solid {
        let entries: Vec<ArchiveEntry> = (0..files)
            .map(|i| ArchiveEntry::new_file(&format!("d/f{i:04}.txt")))
            .collect();
        let readers: Vec<sevenz_rust2::SourceReader<Cursor<Vec<u8>>>> = (0..files)
            .map(|i| Cursor::new(format!("data {i}").into_bytes()).into())
            .collect();
        w.push_archive_entries(entries, readers).unwrap();
    } else {
        for i in 0..files {
            w.push_archive_entry(
                ArchiveEntry::new_file(&format!("d/f{i:04}.txt")),
                Some(format!("data {i}").as_bytes()),
            )
            .unwrap();
        }
        w.push_archive_entry::<&[u8]>(ArchiveEntry::new_directory("d/empty"), None)
            .unwrap();
    }
    w.finish().unwrap();
    fs::read(&path).unwrap()
}

/// Mutate bytes of the next header (CRCs re-fixed so the parser, not the checksum, sees the
/// damage), run the production order — FER-587 pre-check, then the patched crate — and require
/// no panic and no allocation above the ceiling. Deterministic seed, bounded rounds and time.
#[test]
fn differential_fuzz_of_header_counts_never_panics_or_over_allocates() {
    let tmp = TempDir::new("fuzz");
    let seeds: Vec<Vec<u8>> = vec![
        sane_plain(),
        spike("owned.7z"),
        spike("repeat.7z"),
        spike("empty.7z"),
        written_7z(tmp.path(), "multi.7z", 40, false), // encoded (LZMA) header
        written_7z(tmp.path(), "solid.7z", 40, true),  // encoded header, solid block
        encoded(&next_header_of(&sane_plain())),       // stored encoded header
    ];
    for (i, seed) in seeds.iter().enumerate() {
        let open = sevenz_rust2::ArchiveReader::new(
            Cursor::new(seed.clone()),
            sevenz_rust2::Password::empty(),
        );
        assert!(open.is_ok(), "seed {i} must open: {:?}", open.err());
    }

    // FER588_FUZZ_ROUNDS raises the round count for a deeper local run.
    let rounds: usize = std::env::var("FER588_FUZZ_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30_000);
    // FER588_FUZZ_SEED picks another deterministic sequence.
    let seed = std::env::var("FER588_FUZZ_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0x5EED_F588_7A7A_0001u64);
    let mut rng = XorShift(seed | 1);
    let started = std::time::Instant::now();
    let (mut accepted, mut opened, mut max_all) = (0usize, 0usize, 0usize);
    let mut panics = Vec::new();
    for round in 0..rounds {
        let mut b = seeds[round % seeds.len()].clone();
        let off = u64::from_le_bytes(b[12..20].try_into().unwrap()) as usize;
        let start = 32 + off;
        if b.len() <= start {
            continue;
        }
        for _ in 0..1 + rng.below(4) {
            let at = start + rng.below(b.len() - start);
            b[at] = match rng.below(6) {
                0 => rng.next() as u8,
                1 => b[at] ^ (1 << rng.below(8)),
                2 => 0xFF,
                3 => 0x80 | (rng.next() as u8 & 0x7F),
                4 => 0xE0 | (rng.next() as u8 & 0x1F),
                _ => rng.below(4) as u8,
            };
        }
        if rng.below(8) == 0 {
            // Insert a long number (0xFF + 8 bytes) somewhere and fix the header size.
            let at = start + rng.below(b.len() - start);
            let mut big = vec![0xFF];
            big.extend_from_slice(&(rng.next() >> rng.below(64)).to_le_bytes());
            b.splice(at..at, big);
            let size = (b.len() - start) as u64;
            b[20..28].copy_from_slice(&size.to_le_bytes());
        }
        fix_crcs(&mut b);
        if archive_contract::sevenz_header::precheck_encoded_header(
            &mut Cursor::new(&b),
            archive_contract::sevenz_header::HEADER_CODEC_MAX_MEMORY,
        )
        .is_err()
        {
            continue;
        }
        accepted += 1;
        let (outcome, max_one) = armed(|| {
            std::panic::catch_unwind(|| {
                let Ok(mut reader) = sevenz_rust2::ArchiveReader::new(
                    Cursor::new(&b),
                    sevenz_rust2::Password::empty(),
                ) else {
                    return false;
                };
                // Production order: codec memory is checked before anything is decoded.
                reader.set_thread_count(1);
                // Decode only every 32nd input, at most 256 KiB of output each: decode-time CPU is
                // bounded elsewhere (ReadGuard, cancel); this only looks for panics.
                if round % 32 == 0
                    && archive_contract::check_codec_memory(reader.archive(), 64 << 20).is_ok()
                {
                    let mut sink = [0u8; 4096];
                    let mut budget = 256usize << 10;
                    let _ = reader.for_each_entries(|_, r| {
                        loop {
                            let n = r.read(&mut sink)?;
                            if n == 0 {
                                return Ok(true);
                            }
                            budget = budget.saturating_sub(n);
                            if budget == 0 {
                                return Ok(false);
                            }
                        }
                    });
                }
                true
            })
        });
        max_all = max_all.max(max_one);
        match outcome {
            Ok(true) => opened += 1,
            Ok(false) => {}
            Err(_) => panics.push((round, b.clone())),
        }
        assert!(
            max_one <= CEILING,
            "round {round}: allocation of {max_one} bytes"
        );
    }
    if let Some((round, bytes)) = panics.first() {
        let keep = std::env::temp_dir().join(format!("ferrum-fer588-panic-{round}.7z"));
        let _ = fs::write(&keep, bytes);
        panic!(
            "{} of {accepted} accepted inputs panicked in the crate (first: round {round}, kept at {})",
            panics.len(),
            keep.display()
        );
    }
    let elapsed = started.elapsed();
    eprintln!(
        "FER-588 fuzz: {rounds} rounds, {accepted} past the pre-check, {opened} opened, \
         largest allocation {max_all} bytes, {elapsed:?}"
    );
    assert!(accepted > rounds / 4, "fuzz must exercise the crate");
    // ~10 s for the default rounds in a debug build; a hang shows up here.
    assert!(
        elapsed < std::time::Duration::from_secs(120) * (rounds.div_ceil(30_000) as u32),
        "fuzz too slow: {elapsed:?}"
    );
}
