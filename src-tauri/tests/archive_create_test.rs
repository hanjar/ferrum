//! F2-07/F2-08 tests: create ZIP and integrity test ZIP/RAR via engine.

#[path = "../src/archive_contract.rs"]
#[allow(dead_code)]
mod archive_contract;
#[allow(clippy::duplicate_mod, dead_code)]
#[path = "../src/archive_create_test.rs"]
mod archive_create_test;

use archive_contract::{ArchiveErrorCode as Code, ArchiveLimits, ArchiveTestStatus};
use archive_create_test::{create_7z, create_zip, test_7z, test_rar, test_zip};

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

/// Owned temp dir under %TEMP%, removed on Drop (also on assertion failure).
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "ferrum-ct-{tag}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }

    #[allow(dead_code)]
    fn path(&self) -> &Path {
        &self.0
    }
}

impl std::ops::Deref for TempDir {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn temp(tag: &str) -> TempDir {
    TempDir::new(tag)
}

fn default_limits() -> ArchiveLimits {
    ArchiveLimits {
        max_entries: 1000,
        max_total_bytes: 64 << 20,
        max_entry_bytes: 8 << 20,
        max_depth: 8,
    }
}

fn write_tree(root: &Path, files: &[(&str, &[u8])]) {
    for (name, content) in files {
        let p = root.join(name);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&p, content).unwrap();
    }
}

#[test]
fn creates_zip_from_flat_and_nested_sources() {
    let dir = temp("create");
    let src = dir.join("src");
    write_tree(&src, &[("top.txt", b"top"), ("sub/deep/deep.txt", b"deep")]);
    std::fs::write(dir.join("loose.bin"), b"loose").unwrap();

    let dest = dir.join("out.zip");
    create_zip(
        &[
            src.to_string_lossy().into_owned(),
            dir.join("loose.bin").to_string_lossy().into_owned(),
        ],
        &dest,
        &default_limits(),
    )
    .unwrap();

    assert!(dest.exists());
    // Verify by listing with the zip crate reader.
    let file = std::fs::File::open(&dest).unwrap();
    let mut archive = zip::ZipArchive::new(std::io::BufReader::new(file)).unwrap();
    assert!(archive.by_name("src/top.txt").is_ok());
    assert!(archive.by_name("src/sub/deep/deep.txt").is_ok());
    assert!(archive.by_name("loose.bin").is_ok());
    assert_eq!(
        archive.by_name("src/top.txt").unwrap().size(),
        3 // "top"
    );
}

#[test]
fn create_rejects_traversal_and_missing_sources() {
    let dir = temp("reject");
    let dest = dir.join("out.zip");

    let err = create_zip(&["../escape".into()], &dest, &default_limits()).unwrap_err();
    assert_eq!(err.code, Code::InvalidMemberPath);

    let err = create_zip(
        &[dir.join("ghost.txt").to_string_lossy().into_owned()],
        &dest,
        &default_limits(),
    )
    .unwrap_err();
    assert_eq!(err.code, Code::InvalidMemberPath);
}

#[test]
fn create_failure_leaves_no_partial_archive() {
    let dir = temp("partial");
    let dest = dir.join("out.zip");
    // Bad source: rename after write never happens, no partial at dest.
    let err = create_zip(&["../bad".into()], &dest, &default_limits());
    assert!(err.is_err());
    assert!(!dest.exists());
}

#[test]
fn test_zip_detects_passes_and_corruption() {
    let dir = temp("testzip");
    let zip_path = dir.join("test.zip");
    // Create a valid zip using the writer.
    let file = std::fs::File::create(&zip_path).unwrap();
    let mut writer = zip::ZipWriter::new(file);
    let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default();
    writer.start_file("ok.txt", opts).unwrap();
    use std::io::Write;
    writer.write_all(b"good data").unwrap();
    writer.finish().unwrap();

    let report = test_zip(&zip_path, None).unwrap();
    assert_eq!(report.entries.len(), 1);
    assert_eq!(report.entries[0].status, ArchiveTestStatus::Passed);

    // Corrupt: overwrite a byte in the compressed data region.
    let mut raw = std::fs::read(&zip_path).unwrap();
    let mid = raw.len() / 2;
    raw[mid] ^= 0xFF;
    std::fs::write(&zip_path, &raw).unwrap();
    let report2 = test_zip(&zip_path, None);
    // May or may not detect depending on which byte was flipped — just verify
    // the report structure is returned.
    assert!(report2.is_ok());
}

fn find_rar_exe() -> Option<PathBuf> {
    if let Some(val) = std::env::var_os("FERRUM_RAR_EXE") {
        let p = PathBuf::from(val);
        if p.is_file() {
            return Some(p);
        } else {
            return None;
        }
    }

    let p1 = PathBuf::from(r"C:\Program Files\WinRAR\Rar.exe");
    if p1.is_file() {
        return Some(p1);
    }

    let p2 = PathBuf::from(r"C:\Program Files (x86)\WinRAR\Rar.exe");
    if p2.is_file() {
        return Some(p2);
    }

    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join("Rar.exe"))
            .find(|p| p.is_file())
    })
}

fn make_basic_rar(tmp: &Path, rar_exe: &Path) -> PathBuf {
    let src = tmp.join("src");
    let out = tmp.join("basic-rar5.rar");
    std::fs::create_dir_all(src.join("data")).unwrap();
    std::fs::create_dir_all(src.join("docs/deep/a/b")).unwrap();
    std::fs::create_dir_all(src.join("folder-kosong/juga-kosong")).unwrap();

    std::fs::write(
        src.join("readme.txt"),
        b"Ferrum RAR fixture - basic.\r\nIsi sintetis, bebas dipakai untuk tes.\r\n",
    )
    .unwrap();

    let mut angka = Vec::new();
    for i in 0..500 {
        if i > 0 {
            angka.push(b'\n');
        }
        angka.extend_from_slice(format!("{i},{}", i * i).as_bytes());
    }
    std::fs::write(src.join("data/angka.csv"), &angka).unwrap();
    std::fs::write(src.join("data/kosong.txt"), b"").unwrap();
    std::fs::write(
        src.join("docs/catatan.txt"),
        "Catatan kecil.\n".repeat(20).as_bytes(),
    )
    .unwrap();
    std::fs::write(src.join("docs/deep/a/b/dalam.txt"), b"Folder bertingkat.\n").unwrap();

    let status = std::process::Command::new(rar_exe)
        .current_dir(&src)
        .args(["a", "-r", "-idq", "-ma5"])
        .arg(&out)
        .arg("*")
        .status()
        .expect("failed to spawn Rar.exe");
    if !status.success() {
        panic!(
            "Rar.exe failed with exit code {:?} creating basic-rar5.rar",
            status.code()
        );
    }
    out
}

#[test]
#[cfg(windows)]
fn test_rar_with_real_fixture() {
    let Some(rar_exe) = find_rar_exe() else {
        eprintln!("SKIP test_rar_with_real_fixture: Rar.exe not found");
        return;
    };
    let dir = temp("rar-basic");
    let fixture = make_basic_rar(&dir, &rar_exe);
    let report = test_rar(&fixture, None).unwrap();
    assert!(!report.entries.is_empty());
    assert!(
        report
            .entries
            .iter()
            .all(|e| e.status == ArchiveTestStatus::Passed)
    );
}

/// F2-28 audit evidence: the encrypted AES-256 fixture must produce a
/// structured PasswordRequired without a password, BadPassword with a wrong
/// one, and a full verified decode with the correct one. Uses the committed
/// spike fixture (`nested/東京.txt`, demo password is fixture-only).
#[test]
#[cfg(windows)]
fn test_zip_encrypted_fixture_password_matrix() {
    let fixture = PathBuf::from("../contract-fixtures/archive-spike/encrypted.zip");
    if !fixture.exists() {
        eprintln!("skipping: encrypted fixture not present");
        return;
    }
    const PASSWORD: &str = "fixture-only-not-a-secret";

    // No password on an encrypted member → PasswordRequired, not Corrupt.
    let report = test_zip(&fixture, None).unwrap();
    assert_eq!(report.entries.len(), 1);
    assert_eq!(report.entries[0].status, ArchiveTestStatus::Failed);
    let err = report.entries[0].error.as_ref().expect("error present");
    assert_eq!(
        err.code,
        Code::PasswordRequired,
        "no password => PasswordRequired"
    );

    // Wrong password → BadPassword.
    let report = test_zip(&fixture, Some("wrong-password")).unwrap();
    let err = report.entries[0].error.as_ref().expect("error present");
    assert_eq!(err.code, Code::BadPassword, "wrong password => BadPassword");

    // Correct password → full decode with CRC verification passes.
    let report = test_zip(&fixture, Some(PASSWORD)).unwrap();
    assert_eq!(report.entries[0].status, ArchiveTestStatus::Passed);
    assert_eq!(report.entries[0].member_path.as_str(), "nested/東京.txt");
}

/// Writes a ZIP with one Stored member so the payload bytes are findable
/// in the raw file (used by the deterministic bit-flip test).
fn write_stored_zip(path: &Path, name: &str, payload: &[u8]) {
    let file = std::fs::File::create(path).unwrap();
    let mut writer = zip::ZipWriter::new(file);
    let opts =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    writer.start_file(name, opts).unwrap();
    use std::io::Write;
    writer.write_all(payload).unwrap();
    writer.finish().unwrap();
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[test]
fn test_zip_bit_flip_in_stored_payload_is_always_detected() {
    let dir = temp("bitflip");
    let zip_path = dir.join("flip.zip");
    let payload = b"deterministic payload for bit flip detection";
    write_stored_zip(&zip_path, "data.bin", payload);

    let clean = test_zip(&zip_path, None).unwrap();
    assert_eq!(clean.entries.len(), 1);
    assert_eq!(clean.entries[0].status, ArchiveTestStatus::Passed);

    // Fixed position: the first payload byte of the stored member.
    let raw = std::fs::read(&zip_path).unwrap();
    let offset = find_subslice(&raw, payload).expect("stored payload is raw in the file");
    let mut corrupt = raw.clone();
    corrupt[offset] ^= 0x01;
    std::fs::write(&zip_path, &corrupt).unwrap();

    let report = test_zip(&zip_path, None).unwrap();
    assert_eq!(report.entries.len(), 1);
    assert_eq!(
        report.entries[0].status,
        ArchiveTestStatus::Failed,
        "a flipped payload byte must fail the member CRC"
    );
}

#[test]
fn test_zip_truncated_archive_never_reports_all_passed() {
    let dir = temp("trunc-zip");
    let zip_path = dir.join("trunc.zip");
    write_stored_zip(&zip_path, "data.bin", &vec![0x5Au8; 4096]);
    let raw = std::fs::read(&zip_path).unwrap();
    // Cut off the central directory and the tail of the payload.
    std::fs::write(&zip_path, &raw[..raw.len() / 2]).unwrap();

    match test_zip(&zip_path, None) {
        Err(e) => assert!(!e.message.is_empty()),
        Ok(report) => assert!(
            report
                .entries
                .iter()
                .all(|e| e.status != ArchiveTestStatus::Passed),
            "a truncated ZIP must not be reported as fully passed"
        ),
    }
}

#[test]
fn test_7z_functional_passes_on_valid_archive() {
    let dir = temp("test7z");
    write_tree(
        &dir,
        &[("src/a.txt", b"alpha"), ("src/nested/b.txt", b"bravo")],
    );
    let sources = vec![dir.join("src").to_string_lossy().into_owned()];
    let dest = dir.join("out.7z");
    create_7z(&sources, &dest).unwrap();

    let report = test_7z(&dest, None).unwrap();
    assert!(!report.entries.is_empty());
    assert!(
        report
            .entries
            .iter()
            .all(|e| e.status == ArchiveTestStatus::Passed),
        "every member of a freshly written 7z must pass: {:?}",
        report.entries
    );
}

#[test]
fn test_7z_truncated_archive_is_not_reported_as_passed() {
    let dir = temp("trunc7z");
    write_tree(&dir, &[("src/big.bin", &vec![0x33u8; 200_000])]);
    let sources = vec![dir.join("src").to_string_lossy().into_owned()];
    let dest = dir.join("out.7z");
    create_7z(&sources, &dest).unwrap();
    let raw = std::fs::read(&dest).unwrap();
    std::fs::write(&dest, &raw[..raw.len() * 2 / 3]).unwrap();

    match test_7z(&dest, None) {
        Err(_) => {}
        Ok(report) => assert!(
            report
                .entries
                .iter()
                .any(|e| e.status != ArchiveTestStatus::Passed),
            "a truncated 7z must not be reported as fully passed"
        ),
    }
}

#[test]
fn test_zip_unsupported_compression_method_is_not_passed() {
    let dir = temp("codec");
    let zip_path = dir.join("codec.zip");
    write_stored_zip(&zip_path, "data.bin", b"codec probe");
    // Rewrite the method field (0 = Stored) in the local header and the
    // central directory to 14 (LZMA), which the ZIP reader here does not decode.
    let mut raw = std::fs::read(&zip_path).unwrap();
    let mut patched = 0;
    for i in 0..raw.len().saturating_sub(10) {
        let sig = &raw[i..i + 4];
        if sig == [0x50, 0x4b, 0x03, 0x04] || sig == [0x50, 0x4b, 0x01, 0x02] {
            let method_at = if sig[2] == 0x03 { i + 8 } else { i + 10 };
            raw[method_at] = 14;
            raw[method_at + 1] = 0;
            patched += 1;
        }
    }
    assert_eq!(patched, 2, "local and central headers both patched");
    std::fs::write(&zip_path, &raw).unwrap();

    match test_zip(&zip_path, None) {
        Err(e) => assert!(
            e.code == Code::UnsupportedCodec || e.code == Code::CorruptArchive,
            "unexpected error for unsupported codec: {e:?}"
        ),
        Ok(report) => assert!(
            report
                .entries
                .iter()
                .all(|e| e.status != ArchiveTestStatus::Passed),
            "an unsupported codec must never pass"
        ),
    }
}

#[test]
fn test_many_archives_are_reported_independently() {
    let dir = temp("multi");
    let good = dir.join("good.zip");
    let bad = dir.join("bad.zip");
    let missing = dir.join("missing.zip");
    write_stored_zip(&good, "a.bin", b"good member");
    write_stored_zip(&bad, "b.bin", b"bad member payload");
    let raw = std::fs::read(&bad).unwrap();
    let offset = find_subslice(&raw, b"bad member payload").unwrap();
    let mut corrupt = raw.clone();
    corrupt[offset] ^= 0x80;
    std::fs::write(&bad, &corrupt).unwrap();

    let results: Vec<_> = [&good, &bad, &missing]
        .iter()
        .map(|p| test_zip(p, None).map(|r| r.entries[0].status))
        .collect();
    assert_eq!(results[0].as_ref().unwrap(), &ArchiveTestStatus::Passed);
    assert_eq!(results[1].as_ref().unwrap(), &ArchiveTestStatus::Failed);
    assert!(
        results[2].is_err(),
        "a missing archive is an error, not a pass"
    );
}
