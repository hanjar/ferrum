//! FER-61 + FER-68: end-to-end archive roundtrip tests (create → list → extract → compare)
//! for ZIP and 7z, driven through the same `ArchiveProvider` surface the frontend uses.
//!
//! Every assertion compares content byte-for-byte or by SHA-256 — "the archive came back"
//! means identical bytes, not just a successful call. Sources and destinations live in
//! unique `ferrum-roundtrip-*` folders under the system temp dir, removed by a `Drop`
//! guard even when an assertion fails.
//!
//! Cases: nested structure (+ empty folders, in their own test), Unicode names, many
//! small files (incl. 0-byte), one large seeded file (hash), destination name conflicts,
//! create failing mid-archive (locked source), cancel during extract, and listing.
//! The 7-Zip CLI cross-check (FER-68) skips itself when `7z.exe` is not installed.
//!
//! Product-code findings are NOT fixed here: an offending test gets
//! `#[ignore = "BUG: …"]` with the observed behaviour in the message.

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
    ArchiveConflictPolicy, ArchiveCreateFormat, ArchiveCreateRequest, ArchiveExtractRequest,
    ArchiveLimits, ArchiveListRequest, ArchiveProvider, Location, MemberPath,
};
use archive_sevenz::SevenZProvider;
use archive_zip::ZipProvider;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

// ---------------------------------------------------------------- helpers ---

/// Owned temp dir under %TEMP%, removed on Drop (also on assertion failure).
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "ferrum-roundtrip-{tag}-{}-{}",
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

#[derive(Clone, Copy)]
enum Fmt {
    Zip,
    Sevenz,
}

fn provider(fmt: Fmt) -> Box<dyn ArchiveProvider> {
    match fmt {
        Fmt::Zip => Box::new(ZipProvider::new()),
        Fmt::Sevenz => Box::new(SevenZProvider::new()),
    }
}

fn fmt_name(fmt: Fmt) -> &'static str {
    match fmt {
        Fmt::Zip => "zip",
        Fmt::Sevenz => "7z",
    }
}

fn limits() -> ArchiveLimits {
    ArchiveLimits {
        max_entries: 100_000,
        max_total_bytes: u64::MAX,
        max_entry_bytes: 512 * 1024 * 1024,
        max_depth: 32,
    }
}

/// Deterministic pseudo-random byte stream (xorshift64*), so a test run is
/// reproducible and the large file is not compressible filler.
struct Seeded(u64);

impl Seeded {
    fn next(&mut self) -> u8 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        (x >> 24) as u8
    }
}

/// Write `len` seeded bytes to `path`, returning the SHA-256 hex of the content.
fn write_seeded(path: &Path, seed: u64, len: u64) -> String {
    let mut file = fs::File::create(path).unwrap();
    let mut rng = Seeded(seed ^ 0x9E37_79B9_7F4A_7C15);
    let mut hasher = Sha256::new();
    let chunk = vec![0u8; 64 * 1024];
    let mut written = 0u64;
    while written < len {
        let n = chunk.len().min((len - written) as usize);
        let buf: Vec<u8> = chunk[..n].iter().map(|_| rng.next()).collect();
        hasher.update(&buf);
        file.write_all(&buf).unwrap();
        written += n as u64;
    }
    file.sync_all().unwrap();
    hex(&hasher.finalize())
}

fn seeded_bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut rng = Seeded(seed ^ 0xA24B_A5C8_1B5C_1E6D);
    (0..len).map(|_| rng.next()).collect()
}

fn hex(digest: &[u8]) -> String {
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn sha256_file(path: &Path) -> String {
    let data = fs::read(path).unwrap();
    hex(&Sha256::digest(&data))
}

fn create_request(fmt: Fmt, sources: Vec<String>, dest: &Path) -> ArchiveCreateRequest {
    ArchiveCreateRequest {
        format: match fmt {
            Fmt::Zip => ArchiveCreateFormat::Zip,
            Fmt::Sevenz => ArchiveCreateFormat::Sevenz,
        },
        source_paths: sources,
        destination_path: dest.to_string_lossy().into_owned(),
        conflict: ArchiveConflictPolicy::Overwrite,
        level: None,
    }
}

fn extract_request(
    archive: &Path,
    dest: &Path,
    policy: ArchiveConflictPolicy,
) -> ArchiveExtractRequest {
    ArchiveExtractRequest {
        archive_path: archive.to_string_lossy().into_owned(),
        member_paths: vec![],
        destination_path: dest.to_string_lossy().into_owned(),
        conflict: policy,
        limits: limits(),
        password: None,
    }
}

fn create_archive(
    fmt: Fmt,
    sources: Vec<String>,
    dest: &Path,
) -> Result<PathBuf, archive_contract::ArchiveError> {
    provider(fmt).create(&create_request(fmt, sources, dest), &mut |_| true)
}

fn extract_archive(
    fmt: Fmt,
    archive: &Path,
    dest: &Path,
    policy: ArchiveConflictPolicy,
) -> Result<(), archive_contract::ArchiveError> {
    provider(fmt).extract(&extract_request(archive, dest, policy), &mut |_| true)
}

/// Recursively walk the source tree, returning `relpath` (with `/`) → sha256.
/// `root_name` is stripped: sources are archived as `<root_name>/…`.
fn source_hashes(root: &Path, root_name: &str) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    walk_hashes(root, root_name, &mut out);
    out
}

fn walk_hashes(dir: &Path, prefix: &str, out: &mut std::collections::BTreeMap<String, String>) {
    for child in fs::read_dir(dir).unwrap().filter_map(|e| e.ok()) {
        let p = child.path();
        let rel = format!("{prefix}/{}", child.file_name().to_string_lossy());
        if p.is_dir() {
            walk_hashes(&p, &rel, out);
        } else {
            out.insert(rel, sha256_file(&p));
        }
    }
}

/// Recursively walk the EXTRACTED tree the same way (first component stripped).
fn extracted_hashes(root: &Path) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    for child in fs::read_dir(root).unwrap().filter_map(|e| e.ok()) {
        let p = child.path();
        let base = child.file_name().to_string_lossy().into_owned();
        if p.is_dir() {
            walk_hashes(&p, &base, &mut out);
        } else {
            out.insert(base, sha256_file(&p));
        }
    }
    out
}

/// List an archive recursively (following directory entries), returning
/// `relpath` → (is_dir, size).
fn list_all(fmt: Fmt, archive: &Path) -> std::collections::BTreeMap<String, (bool, u64)> {
    let mut out = std::collections::BTreeMap::new();
    list_into(fmt, archive, MemberPath::parse("").unwrap(), "", &mut out);
    out
}

fn list_into(
    fmt: Fmt,
    archive: &Path,
    member: MemberPath,
    prefix: &str,
    out: &mut std::collections::BTreeMap<String, (bool, u64)>,
) {
    let req = ArchiveListRequest {
        location: Location::Archive {
            archive_path: archive.to_string_lossy().into_owned(),
            member_path: member,
        },
        request_token: SEQ.fetch_add(1, Ordering::SeqCst),
        password: None,
    };
    let response = provider(fmt).list(&req).unwrap();
    for entry in &response.entries {
        let rel = if prefix.is_empty() {
            entry.name.clone()
        } else {
            format!("{prefix}/{}", entry.name)
        };
        if entry.is_dir {
            out.insert(rel.clone(), (true, 0));
            let child = MemberPath::parse(&rel).unwrap();
            list_into(fmt, archive, child, &rel, out);
        } else {
            out.insert(rel, (false, entry.size));
        }
    }
}

/// Leftover artifact names the engine must never leave behind.
fn assert_no_artifacts(dir: &Path, context: &str) {
    let mut offenders = Vec::new();
    collect_artifacts(dir, &mut offenders);
    assert!(
        offenders.is_empty(),
        "{context}: leftover temp/partial artifacts: {offenders:?}"
    );
}

fn collect_artifacts(dir: &Path, out: &mut Vec<String>) {
    for child in fs::read_dir(dir).unwrap().filter_map(|e| e.ok()) {
        let name = child.file_name().to_string_lossy().into_owned();
        let p = child.path();
        if p.is_dir() {
            collect_artifacts(&p, out);
        } else if name.contains("ferrum-tmp") || name.contains("ferrum-partial") {
            out.push(name);
        }
    }
}

// ------------------------------------------------------------------ cases ---

/// Source tree for the nested-structure case: 4 levels, files on every level.
fn build_nested(root: &Path) {
    for rel in [
        "payload/level1/level2/level3/deep.txt",
        "payload/level1/mid.txt",
        "payload/level1/level2/leaf.bin",
        "payload/top.txt",
        "payload/level1/level2/level3/wider/inner.dat",
    ] {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, seeded_bytes(rel.len() as u64, 1000 + rel.len() * 7)).unwrap();
    }
}

fn case_nested(fmt: Fmt) {
    let tmp = TempDir::new("nested");
    let src = tmp.path().join("src");
    fs::create_dir_all(&src).unwrap();
    build_nested(&src);
    let archive = tmp.path().join(format!("nested.{}", fmt_name(fmt)));

    create_archive(
        fmt,
        vec![src.join("payload").to_string_lossy().into_owned()],
        &archive,
    )
    .unwrap_or_else(|e| panic!("{}: create failed: {e}", fmt_name(fmt)));
    let out = tmp.path().join("out");
    fs::create_dir_all(&out).unwrap();
    extract_archive(fmt, &archive, &out, ArchiveConflictPolicy::Skip)
        .unwrap_or_else(|e| panic!("{}: extract failed: {e}", fmt_name(fmt)));

    let want = source_hashes(&src.join("payload"), "payload");
    let got = extracted_hashes(&out);
    assert_eq!(got, want, "{}: roundtrip bytes differ", fmt_name(fmt));
    assert_no_artifacts(&out, fmt_name(fmt));
}

/// FER-61: Unicode file and folder names (Indonesian, Japanese, emoji, spaces).
fn build_unicode(root: &Path) {
    for rel in [
        "payload/laporan keuangan 2026.txt",
        "payload/データ/日本語のファイル.txt",
        "payload/emoji 🚀 folder/berkas ✨.txt",
    ] {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, seeded_bytes(rel.len() as u64, 512)).unwrap();
    }
}

fn case_unicode(fmt: Fmt) {
    let tmp = TempDir::new("unicode");
    let src = tmp.path().join("src");
    fs::create_dir_all(&src).unwrap();
    build_unicode(&src);
    let archive = tmp.path().join(format!("unicode.{}", fmt_name(fmt)));

    create_archive(
        fmt,
        vec![src.join("payload").to_string_lossy().into_owned()],
        &archive,
    )
    .unwrap_or_else(|e| panic!("{}: create failed: {e}", fmt_name(fmt)));
    let out = tmp.path().join("out");
    fs::create_dir_all(&out).unwrap();
    extract_archive(fmt, &archive, &out, ArchiveConflictPolicy::Skip)
        .unwrap_or_else(|e| panic!("{}: extract failed: {e}", fmt_name(fmt)));

    let want = source_hashes(&src.join("payload"), "payload");
    let got = extracted_hashes(&out);
    assert_eq!(
        got,
        want,
        "{}: Unicode names/contents differ",
        fmt_name(fmt)
    );
}

/// FER-61: ~2000 small files (0–4 KB, including 0-byte files).
fn case_many_small(fmt: Fmt) {
    const COUNT: u32 = 2000;
    let tmp = TempDir::new("many-small");
    let src = tmp.path().join("src");
    fs::create_dir_all(&src).unwrap();
    for i in 0..COUNT {
        // Spread across subfolders so the tree has depth too.
        let dir = src.join("payload").join(format!("d{:02}", i % 20));
        fs::create_dir_all(&dir).unwrap();
        let size = if i % 97 == 0 { 0 } else { (i * 7919) % 4096 };
        fs::write(
            dir.join(format!("f{i:04}.bin")),
            seeded_bytes(i as u64, size as usize),
        )
        .unwrap();
    }
    let archive = tmp.path().join(format!("many.{}", fmt_name(fmt)));

    create_archive(
        fmt,
        vec![src.join("payload").to_string_lossy().into_owned()],
        &archive,
    )
    .unwrap_or_else(|e| panic!("{}: create failed: {e}", fmt_name(fmt)));
    let out = tmp.path().join("out");
    fs::create_dir_all(&out).unwrap();
    extract_archive(fmt, &archive, &out, ArchiveConflictPolicy::Skip)
        .unwrap_or_else(|e| panic!("{}: extract failed: {e}", fmt_name(fmt)));

    let want = source_hashes(&src.join("payload"), "payload");
    assert_eq!(want.len(), COUNT as usize, "fixture must hold all files");
    let got = extracted_hashes(&out);
    assert_eq!(
        got.len(),
        want.len(),
        "{}: file count differs",
        fmt_name(fmt)
    );
    assert_eq!(got, want, "{}: many-small roundtrip differs", fmt_name(fmt));
}

/// FER-61/68: one ~64 MB seeded (pseudo-random, incompressible) file; content
/// identity is proven by SHA-256. If this takes >60 s per format, mark it
/// `#[ignore]` and run with `cargo test --test archive_roundtrip -- --ignored`.
fn case_large_file(fmt: Fmt) {
    const SIZE: u64 = 64 * 1024 * 1024;
    let tmp = TempDir::new("large");
    let src = tmp.path().join("src").join("payload");
    fs::create_dir_all(&src).unwrap();
    let want_hash = write_seeded(&src.join("big.bin"), 42, SIZE);
    let archive = tmp.path().join(format!("large.{}", fmt_name(fmt)));

    create_archive(fmt, vec![src.to_string_lossy().into_owned()], &archive)
        .unwrap_or_else(|e| panic!("{}: create failed: {e}", fmt_name(fmt)));
    let out = tmp.path().join("out");
    fs::create_dir_all(&out).unwrap();
    extract_archive(fmt, &archive, &out, ArchiveConflictPolicy::Skip)
        .unwrap_or_else(|e| panic!("{}: extract failed: {e}", fmt_name(fmt)));

    let got_hash = sha256_file(&out.join("payload").join("big.bin"));
    assert_eq!(
        got_hash,
        want_hash,
        "{}: 64 MiB roundtrip hash differs",
        fmt_name(fmt)
    );
}

/// FER-61: extract into a destination that already holds same-named files —
/// behaviour must follow the conflict policy and untouched files must stay.
fn case_conflict(fmt: Fmt) {
    let tmp = TempDir::new("conflict");
    let src = tmp.path().join("src");
    fs::create_dir_all(src.join("payload").join("sub")).unwrap();
    fs::write(src.join("payload").join("catatan.txt"), b"BARU").unwrap();
    fs::write(src.join("payload").join("sub").join("lain.bin"), b"xyz").unwrap();
    let archive = tmp.path().join(format!("conflict.{}", fmt_name(fmt)));
    create_archive(
        fmt,
        vec![src.join("payload").to_string_lossy().into_owned()],
        &archive,
    )
    .unwrap_or_else(|e| panic!("{}: create failed: {e}", fmt_name(fmt)));

    // Destination pre-populated: same-named file with OLD content (at the
    // member path "payload/catatan.txt"), plus an unrelated file at the root
    // that no policy may touch.
    let out = tmp.path().join("out");
    fs::create_dir_all(out.join("payload")).unwrap();
    fs::write(out.join("payload").join("catatan.txt"), b"LAMA").unwrap();
    fs::write(out.join("punya_lain.txt"), b"jangan diubah").unwrap();

    // Skip: keeps the old content and reports the skip.
    extract_archive(fmt, &archive, &out, ArchiveConflictPolicy::Skip)
        .unwrap_or_else(|e| panic!("{}: skip extract failed: {e}", fmt_name(fmt)));
    assert_eq!(
        fs::read(out.join("payload").join("catatan.txt")).unwrap(),
        b"LAMA"
    );
    assert_eq!(
        fs::read(out.join("punya_lain.txt")).unwrap(),
        b"jangan diubah"
    );

    // Overwrite: replaces content, unrelated file still untouched.
    extract_archive(fmt, &archive, &out, ArchiveConflictPolicy::Overwrite)
        .unwrap_or_else(|e| panic!("{}: overwrite extract failed: {e}", fmt_name(fmt)));
    assert_eq!(
        fs::read(out.join("payload").join("catatan.txt")).unwrap(),
        b"BARU"
    );
    assert_eq!(
        fs::read(out.join("punya_lain.txt")).unwrap(),
        b"jangan diubah"
    );
    assert_eq!(
        fs::read(out.join("payload").join("sub").join("lain.bin")).unwrap(),
        b"xyz",
        "{}: unrelated archive member must land unchanged",
        fmt_name(fmt)
    );

    // Rename: old file stays, the new one lands under a different name with
    // the new content (TC convention: "name (n).ext").
    let out2 = tmp.path().join("out2");
    fs::create_dir_all(out2.join("payload")).unwrap();
    fs::write(out2.join("payload").join("catatan.txt"), b"LAMA").unwrap();
    extract_archive(fmt, &archive, &out2, ArchiveConflictPolicy::Rename)
        .unwrap_or_else(|e| panic!("{}: rename extract failed: {e}", fmt_name(fmt)));
    assert_eq!(
        fs::read(out2.join("payload").join("catatan.txt")).unwrap(),
        b"LAMA"
    );
    let renamed = fs::read_dir(out2.join("payload"))
        .unwrap()
        .filter_map(|e| e.ok())
        .find(|e| {
            e.file_name().to_string_lossy() != "catatan.txt"
                && e.file_name().to_string_lossy().starts_with("catatan")
                && e.path().is_file()
        })
        .unwrap_or_else(|| panic!("{}: rename policy produced no second file", fmt_name(fmt)));
    assert_eq!(
        fs::read(renamed.path()).unwrap(),
        b"BARU",
        "{}: renamed file must hold the new content",
        fmt_name(fmt)
    );
}

/// FER-61/68: create fails mid-archive (a source file is locked with no
/// sharing). No final archive and no `.ferrum-tmp` sibling may survive.
/// Note: create has no cancel support yet (audit FER-60/67) — so no cancel here.
#[test]
fn create_failure_midway_leaves_no_artifacts_zip() {
    case_create_failure(Fmt::Zip);
}

#[test]
#[cfg(windows)]
fn create_failure_midway_leaves_no_artifacts_sevenz() {
    case_create_failure(Fmt::Sevenz);
}

#[cfg(windows)]
fn case_create_failure(fmt: Fmt) {
    use std::os::windows::fs::OpenOptionsExt;

    let tmp = TempDir::new("create-fail");
    let src = tmp.path().join("src").join("payload");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("a_first.bin"), b"first").unwrap();
    let victim = src.join("b_locked.bin");
    fs::write(&victim, b"locked").unwrap();
    for i in 0..8 {
        fs::write(
            src.join(format!("c_after_{i}.bin")),
            seeded_bytes(i as u64, 2048),
        )
        .unwrap();
    }

    // share_mode(0) = FILE_SHARE_NONE: every other open fails while held.
    let _lock = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&victim)
        .unwrap();

    let archive = tmp.path().join(format!("broken.{}", fmt_name(fmt)));
    let result = create_archive(fmt, vec![src.to_string_lossy().into_owned()], &archive);
    assert!(
        result.is_err(),
        "{}: create must fail on a locked source",
        fmt_name(fmt)
    );
    assert!(
        !archive.exists(),
        "{}: a failed create must not leave the final archive",
        fmt_name(fmt)
    );
    assert_no_artifacts(tmp.path(), fmt_name(fmt));
}

/// FER-68: cancel during extract — returning `false` from the progress
/// callback aborts; the in-flight member is removed, so no half-written file
/// survives, and every file still present is complete and byte-identical.
fn case_cancel_extract(fmt: Fmt) {
    const TOTAL: u32 = 6;
    let tmp = TempDir::new("cancel");
    let src = tmp.path().join("src").join("payload");
    fs::create_dir_all(&src).unwrap();
    for i in 0..TOTAL {
        fs::write(
            src.join(format!("m{i}.bin")),
            seeded_bytes(i as u64, 300_000),
        )
        .unwrap();
    }
    let archive = tmp.path().join(format!("cancel.{}", fmt_name(fmt)));
    create_archive(fmt, vec![src.to_string_lossy().into_owned()], &archive)
        .unwrap_or_else(|e| panic!("{}: create failed: {e}", fmt_name(fmt)));

    let out = tmp.path().join("out");
    fs::create_dir_all(&out).unwrap();
    let mut calls = 0u32;
    let result = provider(fmt).extract(
        &extract_request(&archive, &out, ArchiveConflictPolicy::Skip),
        &mut |_| {
            calls += 1;
            calls <= 1 // cancel right after the first progress report
        },
    );
    let err = match result {
        Ok(()) => panic!("{}: extract should have been cancelled", fmt_name(fmt)),
        Err(e) => e,
    };
    assert_eq!(
        err.code,
        archive_contract::ArchiveErrorCode::Cancelled,
        "{}: cancel must surface the Cancelled code",
        fmt_name(fmt)
    );

    // Whatever survived must be complete; nothing may be partial, and the run
    // must have stopped before writing everything.
    let mut survivors = 0u32;
    for (rel, want) in source_hashes(&tmp.path().join("src").join("payload"), "payload") {
        let p = out.join(&rel);
        if p.is_file() {
            survivors += 1;
            assert_eq!(
                sha256_file(&p),
                want,
                "{}: surviving file {rel} is not byte-identical (partial write?)",
                fmt_name(fmt)
            );
        }
    }
    assert!(
        survivors < TOTAL,
        "{}: cancel after first progress must stop before all {TOTAL} members",
        fmt_name(fmt)
    );
    assert_no_artifacts(&out, fmt_name(fmt));
}

/// FER-61: listing without extraction — members, sizes and folders must match
/// the source tree exactly.
fn case_list(fmt: Fmt) {
    let tmp = TempDir::new("list");
    let src = tmp.path().join("src").join("payload");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("root.txt"), b"root").unwrap();
    fs::create_dir_all(src.join("docs")).unwrap();
    fs::write(src.join("docs").join("a.txt"), seeded_bytes(1, 4096)).unwrap();
    fs::create_dir_all(src.join("docs").join("deep")).unwrap();
    fs::write(
        src.join("docs").join("deep").join("b.bin"),
        seeded_bytes(2, 123_456),
    )
    .unwrap();

    let archive = tmp.path().join(format!("list.{}", fmt_name(fmt)));
    create_archive(fmt, vec![src.to_string_lossy().into_owned()], &archive)
        .unwrap_or_else(|e| panic!("{}: create failed: {e}", fmt_name(fmt)));

    let listed = list_all(fmt, &archive);
    // Every source file must be listed with its exact size.
    for (rel, _) in source_hashes(&src, "payload") {
        let entry = listed
            .get(&rel)
            .unwrap_or_else(|| panic!("{}: member {rel} missing from listing", fmt_name(fmt)));
        assert!(
            !entry.0,
            "{}: {rel} must be listed as a file",
            fmt_name(fmt)
        );
    }
    // Every listed folder must exist in the source; every listed file size
    // must equal the source size.
    let sizes: std::collections::BTreeMap<String, u64> = source_hashes(&src, "payload")
        .keys()
        .map(|rel| {
            (
                rel.clone(),
                fs::metadata(tmp.path().join("src").join(rel))
                    .unwrap()
                    .len(),
            )
        })
        .collect();
    for (rel, (is_dir, size)) in &listed {
        if *is_dir {
            assert!(
                tmp.path().join("src").join(rel).is_dir(),
                "{}: listed folder {rel} does not exist in source",
                fmt_name(fmt)
            );
        } else {
            let want = sizes
                .get(rel)
                .unwrap_or_else(|| panic!("{}: listing has unknown file {rel}", fmt_name(fmt)));
            assert_eq!(size, want, "{}: size mismatch for {rel}", fmt_name(fmt));
        }
    }
}

// ------------------------------------------------- generated tests (2x) ---

#[test]
fn roundtrip_nested_structure_zip() {
    case_nested(Fmt::Zip);
}
#[test]
fn roundtrip_nested_structure_sevenz() {
    case_nested(Fmt::Sevenz);
}

#[test]
fn roundtrip_unicode_names_zip() {
    case_unicode(Fmt::Zip);
}
#[test]
fn roundtrip_unicode_names_sevenz() {
    case_unicode(Fmt::Sevenz);
}

#[test]
fn roundtrip_many_small_files_zip() {
    case_many_small(Fmt::Zip);
}
#[test]
fn roundtrip_many_small_files_sevenz() {
    case_many_small(Fmt::Sevenz);
}

// Measured on the dev machine: the pair takes ~65 s (7z/LZMA dominates), above the
// 60 s budget from the handover — so they run on demand:
//   cargo test --test archive_roundtrip -- --ignored roundtrip_large_file
#[test]
#[ignore = "berat: 64 MiB pseudo-acak, ZIP+7z bersama-sama ±65 dtk (7z/LZMA dominan). Jalankan: \
cargo test --test archive_roundtrip -- --ignored roundtrip_large_file"]
fn roundtrip_large_file_hash_zip() {
    case_large_file(Fmt::Zip);
}
#[test]
#[ignore = "berat: 64 MiB pseudo-acak, ZIP+7z bersama-sama ±65 dtk (7z/LZMA dominan). Jalankan: \
cargo test --test archive_roundtrip -- --ignored roundtrip_large_file"]
fn roundtrip_large_file_hash_sevenz() {
    case_large_file(Fmt::Sevenz);
}

#[test]
fn extract_conflict_policy_zip() {
    case_conflict(Fmt::Zip);
}
#[test]
fn extract_conflict_policy_sevenz() {
    case_conflict(Fmt::Sevenz);
}

#[test]
fn cancel_extract_midstream_zip() {
    case_cancel_extract(Fmt::Zip);
}
#[test]
fn cancel_extract_midstream_sevenz() {
    case_cancel_extract(Fmt::Sevenz);
}

#[test]
fn list_matches_source_zip() {
    case_list(Fmt::Zip);
}
#[test]
fn list_matches_source_sevenz() {
    case_list(Fmt::Sevenz);
}

/// FER-61 (separate on purpose): empty folders must survive a roundtrip.
/// Created via `fs::create_dir_all` only, so nothing but the folder itself
/// can make this pass or fail.
fn case_empty_folders(fmt: Fmt) {
    let tmp = TempDir::new("empty-folders");
    let src = tmp.path().join("src").join("payload");
    fs::create_dir_all(src.join("kosong")).unwrap();
    fs::create_dir_all(src.join("ada_isi")).unwrap();
    fs::write(src.join("ada_isi").join("isi.txt"), b"isi").unwrap();
    fs::create_dir_all(src.join("kosong").join("juga_kosong")).unwrap();
    let archive = tmp.path().join(format!("empty.{}", fmt_name(fmt)));
    create_archive(fmt, vec![src.to_string_lossy().into_owned()], &archive)
        .unwrap_or_else(|e| panic!("{}: create failed: {e}", fmt_name(fmt)));

    // The LISTING must already contain the empty folders.
    let listed = list_all(fmt, &archive);
    for folder in ["kosong", "kosong/juga_kosong"] {
        let entry = listed
            .get(&format!("payload/{folder}"))
            .or_else(|| listed.get(folder))
            .unwrap_or_else(|| {
                panic!(
                    "{}: empty folder {folder} missing from listing",
                    fmt_name(fmt)
                )
            });
        assert!(
            entry.0,
            "{}: {folder} must be listed as a directory",
            fmt_name(fmt)
        );
    }

    // And the EXTRACTED tree must have them back as real folders.
    let out = tmp.path().join("out");
    fs::create_dir_all(&out).unwrap();
    extract_archive(fmt, &archive, &out, ArchiveConflictPolicy::Skip)
        .unwrap_or_else(|e| panic!("{}: extract failed: {e}", fmt_name(fmt)));
    for folder in ["kosong", "kosong/juga_kosong"] {
        assert!(
            out.join("payload").join(folder).is_dir(),
            "{}: empty folder {folder} did not survive the roundtrip",
            fmt_name(fmt)
        );
    }
}

#[test]
fn roundtrip_empty_folders_zip() {
    case_empty_folders(Fmt::Zip);
}
#[test]
fn roundtrip_empty_folders_sevenz() {
    case_empty_folders(Fmt::Sevenz);
}

// --------------------------------------------------- 7-Zip cross-check ---

/// FER-68: cross-tool check. Skipped (with a clear message, not a failure)
/// when 7-Zip is not installed. Never installs anything.
#[test]
fn sevenzip_cross_tool_compatibility() {
    let Some(sevenz) = find_7z_exe() else {
        eprintln!("SKIP sevenzip_cross_tool_compatibility: 7z.exe not found (not installed)");
        return;
    };

    let tmp = TempDir::new("7z-cross");
    let src = tmp.path().join("src").join("payload");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("teks.txt"), b"dibaca 7-Zip").unwrap();
    fs::create_dir_all(src.join("sub")).unwrap();
    fs::write(src.join("sub").join("data.bin"), seeded_bytes(7, 50_000)).unwrap();

    // 1. A Ferrum-made 7z must pass `7z t`.
    let ferrum_archive = tmp.path().join("ferrum-made.7z");
    create_archive(
        Fmt::Sevenz,
        vec![src.to_string_lossy().into_owned()],
        &ferrum_archive,
    )
    .unwrap_or_else(|e| panic!("create failed: {e}"));
    let out = Command::new(&sevenz)
        .arg("t")
        .arg(&ferrum_archive)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "7z t rejected Ferrum's archive: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // 2. A `7z a`-made archive must extract byte-identically through Ferrum.
    let sevenz_archive = tmp.path().join("7z-made.7z");
    let out = Command::new(&sevenz)
        .args(["a", "-y"])
        .arg(&sevenz_archive)
        .arg(src.join("payload"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "7z a failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out_dir = tmp.path().join("out");
    fs::create_dir_all(&out_dir).unwrap();
    extract_archive(
        Fmt::Sevenz,
        &sevenz_archive,
        &out_dir,
        ArchiveConflictPolicy::Skip,
    )
    .unwrap_or_else(|e| panic!("Ferrum could not extract 7z-made archive: {e}"));
    let want = source_hashes(&src, "payload");
    let got = extracted_hashes(&out_dir);
    assert_eq!(
        got, want,
        "7z-made archive did not roundtrip through Ferrum"
    );
}

use std::process::Command;

fn find_7z_exe() -> Option<PathBuf> {
    let program_files = PathBuf::from(r"C:\Program Files\7-Zip\7z.exe");
    if program_files.is_file() {
        return Some(program_files);
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join("7z.exe"))
            .find(|p| p.is_file())
    })
}
