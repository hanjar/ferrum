//! FER-49 + FER-50 + FER-57 + FER-65: hostile and unusual archive fixtures for the
//! EXTRACT/LIST surface, built synthetically with the `zip` and `sevenz-rust2`
//! writers (never with Ferrum's create, which has the empty-folder gap, FER-502).
//!
//! Security property under test (FER-49/65): a crafted member name that escapes the
//! destination (`../`, `..\`, absolute, UNC, ADS, reserved) must be refused BEFORE
//! anything is written — not a single byte outside the destination folder, verified
//! by snapshotting the destination's parent before and after.
//!
//! Fixture names that the current writer+feature set cannot produce are marked
//! `#[ignore = "TIDAK BISA DIBUAT: …"]` with the reason — no binary hacks.
//! Product bugs found here are NOT fixed: the test gets `#[ignore = "BUG: …"]`.

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
#[path = "../src/archive_zip.rs"]
#[allow(dead_code)]
mod archive_zip;
#[path = "../src/archive_publish.rs"]
pub mod publish;

use archive_contract::{
    ArchiveConflictPolicy, ArchiveErrorCode as Code, ArchiveExtractRequest, ArchiveLimits,
    ArchiveListRequest, ArchiveListResponse, ArchiveProvider, ArchiveReadRequest, Location,
    MemberPath,
};
use archive_sevenz::SevenZProvider;
use archive_zip::ZipProvider;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

// ---------------------------------------------------------------- helpers ---

/// Owned temp dir under %TEMP% with the `ferrum-hostile-` prefix, removed on Drop
/// (also when an assertion fails).
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "ferrum-hostile-{tag}-{}-{}",
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

/// Sorted file+dir names directly inside `dir` (the snapshot for the
/// "nothing appeared outside the destination" check).
fn snapshot(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn limits() -> ArchiveLimits {
    ArchiveLimits {
        max_entries: 100_000,
        max_total_bytes: u64::MAX,
        max_entry_bytes: 512 * 1024 * 1024,
        max_depth: 32,
    }
}

fn extract_with_limits(
    zip: bool,
    archive: &Path,
    dest: &Path,
    policy: ArchiveConflictPolicy,
    limits: ArchiveLimits,
    members: Vec<MemberPath>,
) -> Result<(), archive_contract::ArchiveError> {
    let request = ArchiveExtractRequest {
        archive_path: archive.to_string_lossy().into_owned(),
        member_paths: members,
        destination_path: dest.to_string_lossy().into_owned(),
        conflict: policy,
        limits,
        password: None,
    };
    if zip {
        ZipProvider::new().extract(&request, &mut |_| true)
    } else {
        SevenZProvider::new().extract(&request, &mut |_| true)
    }
}

fn list_archive(zip: bool, archive: &Path, member: &str) -> ArchiveListResponse {
    let request = ArchiveListRequest {
        location: Location::Archive {
            archive_path: archive.to_string_lossy().into_owned(),
            member_path: MemberPath::parse(member).unwrap(),
        },
        request_token: SEQ.fetch_add(1, Ordering::SeqCst),
        password: None,
    };
    if zip {
        ZipProvider::new().list(&request).unwrap()
    } else {
        SevenZProvider::new().list(&request).unwrap()
    }
}

fn list_archive_ok_or_err(
    zip: bool,
    archive: &Path,
) -> Result<ArchiveListResponse, archive_contract::ArchiveError> {
    let request = ArchiveListRequest {
        location: Location::Archive {
            archive_path: archive.to_string_lossy().into_owned(),
            member_path: MemberPath::parse("").unwrap(),
        },
        request_token: SEQ.fetch_add(1, Ordering::SeqCst),
        password: None,
    };
    if zip {
        ZipProvider::new().list(&request)
    } else {
        SevenZProvider::new().list(&request)
    }
}

fn read_member(zip: bool, archive: &Path, member: &str) -> Vec<u8> {
    let request = ArchiveReadRequest {
        location: Location::Archive {
            archive_path: archive.to_string_lossy().into_owned(),
            member_path: MemberPath::parse(member).unwrap(),
        },
        max_bytes: 1024 * 1024,
        password: None,
    };
    let collected = std::cell::RefCell::new(Vec::<u8>::new());
    let result = if zip {
        ZipProvider::new().read(&request, &mut |chunk| {
            collected.borrow_mut().extend_from_slice(chunk);
            Ok(())
        })
    } else {
        SevenZProvider::new().read(&request, &mut |chunk| {
            collected.borrow_mut().extend_from_slice(chunk);
            Ok(())
        })
    };
    result.unwrap_or_else(|e| panic!("read {member} failed: {e}"));
    collected.into_inner()
}

fn assert_no_partial_artifacts(dir: &Path, context: &str) {
    if !dir.exists() {
        return;
    }
    for child in fs::read_dir(dir).unwrap().filter_map(|e| e.ok()) {
        let name = child.file_name().to_string_lossy().into_owned();
        assert!(
            !name.contains("ferrum-partial") && !name.contains("ferrum-tmp"),
            "{context}: leftover staging artifact {name:?}"
        );
        if child.path().is_dir() {
            assert_no_partial_artifacts(&child.path(), context);
        }
    }
}

// ------------------------------------------------------- 7z fixture writer ---

struct SevenzItem {
    entry: sevenz_rust2::ArchiveEntry,
    content: Option<Vec<u8>>,
}

fn sevenz_file(name: &str, content: &[u8]) -> SevenzItem {
    SevenzItem {
        entry: sevenz_rust2::ArchiveEntry::new_file(name),
        content: Some(content.to_vec()),
    }
}

fn sevenz_dir(name: &str) -> SevenzItem {
    SevenzItem {
        entry: sevenz_rust2::ArchiveEntry::new_directory(name),
        content: None,
    }
}

/// 7z entry marked as a UNIX symlink: FILE_ATTRIBUTE_UNIX_EXTENSION with S_IFLNK
/// mode bits, link target as the (uncompressed) content — the standard 7z encoding.
fn sevenz_symlink(name: &str, target: &str) -> SevenzItem {
    const FILE_ATTRIBUTE_UNIX_EXTENSION: u32 = 0x8000;
    const S_IFLNK: u32 = 0o120000; // symlink file type in the unix mode bits
    let mut entry = sevenz_rust2::ArchiveEntry::new_file(name);
    entry.has_windows_attributes = true;
    entry.windows_attributes = FILE_ATTRIBUTE_UNIX_EXTENSION | S_IFLNK | 0o777;
    SevenzItem {
        entry,
        content: Some(target.as_bytes().to_vec()),
    }
}

fn write_7z(path: &Path, items: &[SevenzItem]) {
    let mut writer = sevenz_rust2::ArchiveWriter::create(path).unwrap();
    for item in items {
        writer
            .push_archive_entry(item.entry.clone(), item.content.as_deref())
            .unwrap();
    }
    writer.finish().unwrap();
}

// ------------------------------------------------------- ZIP fixture writer ---

enum ZipItem {
    Dir(&'static str),
    File(&'static str, &'static [u8]),
}

fn write_zip(path: &Path, items: &[ZipItem], large_file: bool) {
    let file = fs::File::create(path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let base = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .large_file(large_file);
    for item in items {
        match item {
            ZipItem::Dir(name) => zip.add_directory(*name, base).unwrap(),
            ZipItem::File(name, content) => {
                zip.start_file(*name, base).unwrap();
                zip.write_all(content).unwrap();
            }
        }
    }
    zip.finish().unwrap();
}

// --------------------------------------------- FER-49: 7z hostile names ---

/// Every escaping/unspeakable member name must be refused by list AND extract,
/// and the extraction must not write a single byte outside the destination
/// (parent-of-destination snapshot identical, destination empty).
#[test]
fn sevenz_hostile_names_rejected_before_any_write() {
    const HOSTILE: [&str; 7] = [
        "../keluar.txt",
        "..\\keluar.txt",
        "C:\\abs.txt",
        "\\\\server\\share\\x.txt",
        "a.txt:stream", // NTFS alternate data stream
        "CON",          // reserved device name
        "nul.txt",      // reserved stem
    ];
    let tmp = TempDir::new("traversal");
    for (i, name) in HOSTILE.iter().enumerate() {
        let archive = tmp.path().join(format!("hostile-{i}.7z"));
        write_7z(
            &archive,
            &[
                sevenz_file("benign.txt", b"aman"),
                sevenz_file(name, b"melarikan diri"),
            ],
        );
        let dest = tmp.path().join(format!("dest-{i}"));
        let before = snapshot(tmp.path());

        // A hostile name may never reach a listing. Most names fail the list
        // with InvalidMemberPath; a UNC name (`//server/...` after separator
        // normalization) is instead silently OMITTED from the root listing by
        // the implicit-dir logic (its first component is empty). Both are
        // acceptable as long as the hostile name never shows up.
        let list_result = list_archive_ok_or_err(false, &archive);
        match list_result {
            Err(e) => assert_eq!(
                e.code,
                Code::InvalidMemberPath,
                "listing must reject {name:?}"
            ),
            Ok(response) => {
                let names: Vec<&str> = response.entries.iter().map(|e| e.name.as_str()).collect();
                assert_eq!(
                    names,
                    vec!["benign.txt"],
                    "hostile {name:?} must not appear in the listing"
                );
            }
        }

        let err = extract_with_limits(
            false,
            &archive,
            &dest,
            ArchiveConflictPolicy::Overwrite,
            limits(),
            vec![],
        )
        .expect_err("extract must fail for a hostile member name");
        assert_eq!(
            err.code,
            Code::InvalidMemberPath,
            "extract must reject {name:?} with InvalidMemberPath"
        );

        // SECURITY: parent-of-destination gained NOTHING except (at most) the
        // empty destination directory itself.
        let mut after = snapshot(tmp.path());
        let dest_name = format!("dest-{i}");
        after.retain(|n| *n != dest_name);
        assert_eq!(
            after, before,
            "escape {name:?}: files appeared outside the destination"
        );
        assert!(
            !dest.exists() || fs::read_dir(&dest).unwrap().next().is_none(),
            "escape {name:?}: destination is not clean"
        );
        assert_no_partial_artifacts(tmp.path(), name);
    }
}

/// FER-49: a 7z symlink chain (`a → b`, `b → ../luar`) must never be followed:
/// whatever the provider does with the entries, nothing may appear outside the
/// destination, and extracted "links" must land as plain files, not re-parsed
/// symlinks.
#[test]
fn sevenz_symlink_chain_is_not_followed() {
    let tmp = TempDir::new("symlink");
    let archive = tmp.path().join("links.7z");
    write_7z(
        &archive,
        &[
            sevenz_symlink("chain/a", "b"),
            sevenz_symlink("chain/b", "../luar"),
            sevenz_file("chain/ok.txt", b"aman"),
        ],
    );
    let dest = tmp.path().join("dest");
    fs::create_dir_all(&dest).unwrap();
    let before = snapshot(tmp.path());

    let result = extract_with_limits(
        false,
        &archive,
        &dest,
        ArchiveConflictPolicy::Overwrite,
        limits(),
        vec![],
    );
    match result {
        Ok(()) => {
            // Extracted "links" must be plain files holding the literal target
            // text — never re-parsed or followed.
            for (name, target) in [("a", "b"), ("b", "../luar")] {
                let p = dest.join("chain").join(name);
                let meta =
                    fs::symlink_metadata(&p).unwrap_or_else(|e| panic!("link {name} missing: {e}"));
                assert!(
                    meta.is_file(),
                    "{name} must land as a regular file, not a symlink/reparse point"
                );
                assert_eq!(fs::read(&p).unwrap(), target.as_bytes());
            }
        }
        Err(e) => {
            // Refusal is equally acceptable; it must be a structured error.
            eprintln!("symlink chain rejected with code {:?}", e.code);
            let known = matches!(
                e.code,
                Code::UnsafeEntry | Code::InvalidMemberPath | Code::UnsupportedFormat
            );
            assert!(known, "unexpected error class for symlink chain: {e:?}");
        }
    }

    // SECURITY in both branches: nothing outside the destination, no partials.
    // (A refused extract may also clean up the destination dir it created.)
    let mut after = snapshot(tmp.path());
    after.retain(|n| *n != "dest");
    let mut expected = before.clone();
    expected.retain(|n| *n != "dest");
    assert_eq!(after, expected, "symlink chain escaped the destination");
    assert_no_partial_artifacts(&dest, "symlink");
}

// --------------------------------------------- FER-65: 7z quota enforcement ---

#[test]
fn sevenz_quota_max_entries_rejected() {
    let tmp = TempDir::new("quota-entries");
    let archive = tmp.path().join("many.7z");
    write_7z(
        &archive,
        &(0..5)
            .map(|i| sevenz_file(&format!("f{i}.txt"), b"x"))
            .collect::<Vec<_>>(),
    );
    let mut small = limits();
    small.max_entries = 2;
    let dest = tmp.path().join("dest");
    let err = extract_with_limits(
        false,
        &archive,
        &dest,
        ArchiveConflictPolicy::Overwrite,
        small,
        vec![],
    )
    .expect_err("entry quota must reject the archive");
    assert_eq!(err.code, Code::LimitExceeded);
    assert!(
        !dest.exists() || fs::read_dir(&dest).unwrap().next().is_none(),
        "destination must stay clean on quota rejection"
    );
}

#[test]
fn sevenz_quota_max_total_bytes_rejected() {
    let tmp = TempDir::new("quota-total");
    let archive = tmp.path().join("total.7z");
    write_7z(
        &archive,
        &[
            sevenz_file("a.bin", &[0u8; 512]),
            sevenz_file("b.bin", &[0u8; 512]),
        ],
    );
    let mut small = limits();
    small.max_total_bytes = 512; // archive declares 1024
    let dest = tmp.path().join("dest");
    let err = extract_with_limits(
        false,
        &archive,
        &dest,
        ArchiveConflictPolicy::Overwrite,
        small,
        vec![],
    )
    .expect_err("total-byte quota must reject the archive");
    assert_eq!(err.code, Code::LimitExceeded);
    assert!(!dest.exists() || fs::read_dir(&dest).unwrap().next().is_none());
}

#[test]
fn sevenz_quota_max_entry_bytes_rejected() {
    let tmp = TempDir::new("quota-entry");
    let archive = tmp.path().join("entry.7z");
    write_7z(&archive, &[sevenz_file("big.bin", &[0u8; 512])]);
    let mut small = limits();
    small.max_entry_bytes = 64;
    let dest = tmp.path().join("dest");
    let err = extract_with_limits(
        false,
        &archive,
        &dest,
        ArchiveConflictPolicy::Overwrite,
        small,
        vec![],
    )
    .expect_err("per-entry quota must reject the archive");
    assert_eq!(err.code, Code::LimitExceeded);
    assert!(!dest.exists() || fs::read_dir(&dest).unwrap().next().is_none());
}

#[test]
fn sevenz_quota_max_depth_rejected() {
    let tmp = TempDir::new("quota-depth");
    let archive = tmp.path().join("deep.7z");
    write_7z(&archive, &[sevenz_file("a/b/c/d/e.txt", b"deep")]);
    let mut small = limits();
    small.max_depth = 3; // fixture nests 5 levels
    let dest = tmp.path().join("dest");
    let err = extract_with_limits(
        false,
        &archive,
        &dest,
        ArchiveConflictPolicy::Overwrite,
        small,
        vec![],
    )
    .expect_err("depth quota must reject the archive");
    assert_eq!(err.code, Code::LimitExceeded);
    assert!(!dest.exists() || fs::read_dir(&dest).unwrap().next().is_none());
}

// ------------------------------------- FER-65: unsupported 7z codec (E2E) ---

/// FER-65: an archive encoded with a codec the provider does not support must
/// fail list/extract with `UnsupportedCodec` (or equivalent), without panic
/// and without leftovers.
///
/// TIDAK BISA DIBUAT with the current dependency features: Ferrum compiles
/// sevenz-rust2 with `default-features = false, features = ["compress",
/// "aes256"]`. The WRITER can then only produce Copy / Delta / BCJ* / LZMA /
/// LZMA2 — and the DECODER supports exactly that same set. Every other codec
/// (bzip2, deflate, zstd, brotli, lz4, ppmd) is behind crate features that are
/// switched OFF, so the writer cannot emit such an archive either, and the
/// handover forbids binary header hacks.
#[test]
#[ignore = "TIDAK BISA DIBUAT: writer sevenz-rust2 dengan fitur yang diaktifkan Ferrum \
(compress+aes256) hanya bisa menulis Copy/Delta/BCJ/LZMA/LZMA2, dan semuanya didukung decoder; \
codec lain (bzip2/deflate/zstd/ppmd) butuh fitur crate yang Ferrum matikan, dan hack biner dilarang."]
fn sevenz_unsupported_codec_end_to_end() {
    unreachable!("cannot be built with the available writer features; see ignore reason")
}

// ------------------------------------------------- FER-50: ZIP64 handling ---

/// More than 65,535 entries forces a ZIP64 EOCD. Sniff+list (via the provider's
/// archive open path) must succeed with the exact entry count, and extracting
/// one subtree must work. Zero-byte files, Stored, so the fixture stays small.
#[test]
fn zip64_many_entries_list_and_subtree_extract() {
    const COUNT: u32 = 65_600;
    let tmp = TempDir::new("zip64-many");
    let archive = tmp.path().join("many.zip");
    let file = fs::File::create(&archive).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .large_file(true);
    for i in 0..COUNT {
        zip.start_file(format!("f{i:05}.txt"), opts).unwrap();
    }
    zip.finish().unwrap();

    // Sniff+list through the provider: exact child count at the root.
    let response = list_archive(true, &archive, "");
    assert_eq!(
        response.entries.len(),
        COUNT as usize,
        "ZIP64 central directory must list every entry"
    );

    // Extract a single subtree member and verify the (empty) bytes.
    let dest = tmp.path().join("dest");
    fs::create_dir_all(&dest).unwrap();
    extract_with_limits(
        true,
        &archive,
        &dest,
        ArchiveConflictPolicy::Overwrite,
        limits(),
        vec![MemberPath::parse("f00042.txt").unwrap()],
    )
    .unwrap_or_else(|e| panic!("ZIP64 subtree extract failed: {e}"));
    assert_eq!(fs::read(dest.join("f00042.txt")).unwrap(), b"");
}

/// A single small entry written with the ZIP64 extra field (`large_file(true)`):
/// list, read, and extract must all agree on the exact bytes.
#[test]
fn zip64_single_entry_list_read_extract_identical() {
    let tmp = TempDir::new("zip64-one");
    let archive = tmp.path().join("one.zip");
    let content: Vec<u8> = (0..1024u32).map(|i| (i * 31 % 251) as u8).collect();
    // Written inline: the ZIP64 flag (`large_file(true)`) is forced on this one
    // entry, while the bytes stay dynamic.
    let file = fs::File::create(&archive).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .large_file(true);
    zip.start_file("satu.bin", opts).unwrap();
    zip.write_all(&content).unwrap();
    zip.finish().unwrap();

    let listed = list_archive(true, &archive, "");
    let entry = listed
        .entries
        .iter()
        .find(|e| e.name == "satu.bin")
        .unwrap_or_else(|| panic!("member missing from ZIP64 listing"));
    assert_eq!(entry.size, content.len() as u64);

    let read = read_member(true, &archive, "satu.bin");
    assert_eq!(read, content, "ZIP64 read bytes differ");

    let dest = tmp.path().join("dest");
    fs::create_dir_all(&dest).unwrap();
    extract_with_limits(
        true,
        &archive,
        &dest,
        ArchiveConflictPolicy::Overwrite,
        limits(),
        vec![],
    )
    .unwrap_or_else(|e| panic!("ZIP64 extract failed: {e}"));
    assert_eq!(fs::read(dest.join("satu.bin")).unwrap(), content);
}

// --------------------------- FER-57: Unicode names + explicit empty dirs ---

/// ZIP with EXPLICIT directory entries for empty (and nested-empty) folders,
/// plus Unicode names. After extract the folders must exist and names must be
/// byte-exact. (Fixture written with the `zip` crate — Ferrum create cannot
/// emit empty dirs, FER-502.)
#[test]
fn zip_unicode_and_explicit_empty_dirs_extract() {
    let tmp = TempDir::new("zip-unicode-dirs");
    let archive = tmp.path().join("unicode.zip");
    write_zip(
        &archive,
        &[
            ZipItem::Dir("payload/kosong"),
            ZipItem::Dir("payload/kosong/lebih_dalam"),
            ZipItem::Dir("payload/emoji ✨"),
            ZipItem::File("payload/laporan keuangan 2026.txt", b"rp1"),
            ZipItem::File("payload/データ/日本語のファイル.txt", b"jp"),
            ZipItem::File("payload/emoji ✨/berkas 🚀.txt", b"emo"),
        ],
        false,
    );

    let dest = tmp.path().join("dest");
    fs::create_dir_all(&dest).unwrap();
    extract_with_limits(
        true,
        &archive,
        &dest,
        ArchiveConflictPolicy::Overwrite,
        limits(),
        vec![],
    )
    .unwrap_or_else(|e| panic!("extract failed: {e}"));

    assert!(dest.join("payload/kosong").is_dir(), "empty folder missing");
    assert!(
        dest.join("payload/kosong/lebih_dalam").is_dir(),
        "nested empty folder missing"
    );
    assert!(dest.join("payload/emoji ✨").is_dir());
    assert_eq!(
        fs::read(dest.join("payload/laporan keuangan 2026.txt")).unwrap(),
        b"rp1"
    );
    assert_eq!(
        fs::read(dest.join("payload/データ/日本語のファイル.txt")).unwrap(),
        b"jp"
    );
    assert_eq!(
        fs::read(dest.join("payload/emoji ✨/berkas 🚀.txt")).unwrap(),
        b"emo"
    );
}

/// Same as the ZIP case, via the 7z writer's explicit directory entries.
#[test]
fn sevenz_unicode_and_explicit_empty_dirs_extract() {
    let tmp = TempDir::new("7z-unicode-dirs");
    let archive = tmp.path().join("unicode.7z");
    write_7z(
        &archive,
        &[
            sevenz_dir("payload/kosong"),
            sevenz_dir("payload/kosong/lebih_dalam"),
            sevenz_dir("payload/emoji ✨"),
            sevenz_file("payload/laporan keuangan 2026.txt", b"rp1"),
            sevenz_file("payload/データ/日本語のファイル.txt", b"jp"),
            sevenz_file("payload/emoji ✨/berkas 🚀.txt", b"emo"),
        ],
    );

    let dest = tmp.path().join("dest");
    fs::create_dir_all(&dest).unwrap();
    extract_with_limits(
        false,
        &archive,
        &dest,
        ArchiveConflictPolicy::Overwrite,
        limits(),
        vec![],
    )
    .unwrap_or_else(|e| panic!("extract failed: {e}"));

    assert!(dest.join("payload/kosong").is_dir(), "empty folder missing");
    assert!(
        dest.join("payload/kosong/lebih_dalam").is_dir(),
        "nested empty folder missing"
    );
    assert!(dest.join("payload/emoji ✨").is_dir());
    assert_eq!(
        fs::read(dest.join("payload/データ/日本語のファイル.txt")).unwrap(),
        b"jp"
    );
    assert_eq!(
        fs::read(dest.join("payload/emoji ✨/berkas 🚀.txt")).unwrap(),
        b"emo"
    );
}

/// NFC names extract byte-exact; the canonically-equivalent NFD spelling is
/// refused with `UnsafeEntry` — this is the DOCUMENTED alias policy
/// (`rejects_unicode_decomposition_aliases`), asserted here end-to-end through
/// both providers, not treated as a bug.
#[test]
fn unicode_nfc_extracts_and_nfd_alias_is_documented_rejection() {
    let nfc = "kafe\u{e9}.txt"; // precomposed é
    let nfd = "cafe\u{301}.txt"; // e + combining acute (canonically é)

    // NFC: extracts fine through both formats.
    let tmp = TempDir::new("unicode-nfc");
    let zip_archive = tmp.path().join("nfc.zip");
    write_zip(&zip_archive, &[ZipItem::File(nfc, b"nfc")], false);
    let dest = tmp.path().join("dest");
    fs::create_dir_all(&dest).unwrap();
    extract_with_limits(
        true,
        &zip_archive,
        &dest,
        ArchiveConflictPolicy::Overwrite,
        limits(),
        vec![],
    )
    .unwrap_or_else(|e| panic!("NFC ZIP extract failed: {e}"));
    assert_eq!(fs::read(dest.join(nfc)).unwrap(), b"nfc");

    let sevenz_archive = tmp.path().join("nfc.7z");
    write_7z(&sevenz_archive, &[sevenz_file(nfc, b"nfc")]);
    let dest7 = tmp.path().join("dest7");
    fs::create_dir_all(&dest7).unwrap();
    extract_with_limits(
        false,
        &sevenz_archive,
        &dest7,
        ArchiveConflictPolicy::Overwrite,
        limits(),
        vec![],
    )
    .unwrap_or_else(|e| panic!("NFC 7z extract failed: {e}"));
    assert_eq!(fs::read(dest7.join(nfc)).unwrap(), b"nfc");

    // NFD alias: documented refusal, both formats.
    let nfd_zip = tmp.path().join("nfd.zip");
    write_zip(&nfd_zip, &[ZipItem::File(nfd, b"nfd")], false);
    let err = extract_with_limits(
        true,
        &nfd_zip,
        &tmp.path().join("d1"),
        ArchiveConflictPolicy::Overwrite,
        limits(),
        vec![],
    )
    .expect_err("NFD alias must be refused (documented policy)");
    assert_eq!(err.code, Code::UnsafeEntry);

    let nfd_7z = tmp.path().join("nfd.7z");
    write_7z(&nfd_7z, &[sevenz_file(nfd, b"nfd")]);
    let err = extract_with_limits(
        false,
        &nfd_7z,
        &tmp.path().join("d2"),
        ArchiveConflictPolicy::Overwrite,
        limits(),
        vec![],
    )
    .expect_err("NFD alias must be refused (documented policy)");
    assert_eq!(err.code, Code::UnsafeEntry);
}
