//! F2-07 tests: 7z listing/read/extract via SevenZProvider against the
//! committed spike fixtures (`contract-fixtures/archive-spike`). Covers:
//! root/implicit dirs, subdirectory, empty archive, `.7zip` alias by
//! signature, bounded/truncated read, missing member, encrypted header +
//! password matrix, extract through the F2-03 pipeline, damaged payload,
//! corrupt header, honest capabilities.

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
mod archive_sevenz;
#[path = "../src/archive_publish.rs"]
pub mod publish;

use archive_contract::{
    ArchiveConflictPolicy, ArchiveErrorCode as Code, ArchiveExtractRequest, ArchiveFormat,
    ArchiveLimits, ArchiveListRequest, ArchiveProvider, ArchiveReadRequest, Location, MemberPath,
};
use archive_sevenz::SevenZProvider;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

/// The committed spike fixtures; demo password is fixture-only (see
/// docs/archive-support.md — never a real secret).
const FIXTURES: &str = "../contract-fixtures/archive-spike";
const PASSWORD: &str = "fixture-only-not-a-secret";
const TEXT: &[u8] = "Ferrum-owned fixture: café/東京\n".as_bytes();

fn fixture(name: &str) -> PathBuf {
    let p = PathBuf::from(FIXTURES).join(name);
    assert!(p.exists(), "fixture missing: {}", p.display());
    p
}

/// Owned temp dir under %TEMP%, removed on Drop (also on assertion failure).
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "ferrum-7z-{tag}-{}-{}",
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

fn list_req(path: &Path, member: &str) -> ArchiveListRequest {
    ArchiveListRequest {
        location: Location::Archive {
            archive_path: path.to_string_lossy().into_owned(),
            member_path: MemberPath::parse(member).unwrap(),
        },
        request_token: 1,
        password: None,
    }
}

fn read_req(
    path: &Path,
    member: &str,
    max_bytes: u64,
    password: Option<&str>,
) -> ArchiveReadRequest {
    ArchiveReadRequest {
        location: Location::Archive {
            archive_path: path.to_string_lossy().into_owned(),
            member_path: MemberPath::parse(member).unwrap(),
        },
        max_bytes,
        password: password.map(str::to_owned),
    }
}

fn extract_req(path: &Path, dest: &Path, password: Option<&str>) -> ArchiveExtractRequest {
    ArchiveExtractRequest {
        archive_path: path.to_string_lossy().into_owned(),
        member_paths: vec![],
        destination_path: dest.to_string_lossy().into_owned(),
        conflict: ArchiveConflictPolicy::Overwrite,
        limits: ArchiveLimits {
            max_entries: 1_000,
            max_total_bytes: u64::MAX,
            max_entry_bytes: u64::MAX,
            max_depth: 32,
        },
        password: password.map(str::to_owned),
    }
}

#[test]
fn lists_root_children_with_implicit_dirs() {
    let owned = fixture("owned.7z");
    let resp = SevenZProvider::new().list(&list_req(&owned, "")).unwrap();
    let names: Vec<(&str, bool)> = resp
        .entries
        .iter()
        .map(|e| (e.name.as_str(), e.is_dir))
        .collect();
    assert_eq!(
        names,
        vec![("nested", true)],
        "implicit dir synthesized from nested/東京.txt"
    );
    assert_eq!(resp.entries[0].member_path.as_str(), "nested");
}

#[test]
fn lists_subdirectory_children() {
    let owned = fixture("owned.7z");
    let resp = SevenZProvider::new()
        .list(&list_req(&owned, "nested"))
        .unwrap();
    assert_eq!(resp.entries.len(), 1);
    let entry = &resp.entries[0];
    assert_eq!(entry.name, "東京.txt");
    assert!(!entry.is_dir);
    assert_eq!(entry.size, TEXT.len() as u64);
    assert_eq!(entry.member_path.as_str(), "nested/東京.txt");
}

#[test]
fn empty_archive_lists_nothing() {
    let empty = fixture("empty.7z");
    let resp = SevenZProvider::new().list(&list_req(&empty, "")).unwrap();
    assert!(resp.entries.is_empty());
}

#[test]
fn alias_7zip_extension_lists_with_signature() {
    let alias = fixture("alias.7zip");
    let resp = SevenZProvider::new().list(&list_req(&alias, "")).unwrap();
    assert_eq!(resp.entries.len(), 1);
    assert_eq!(resp.entries[0].name, "nested");
}

#[test]
fn non_7z_signature_is_unsupported_format() {
    let dir = temp("sniff");
    let fake = dir.join("fake.7z");
    std::fs::write(&fake, b"PK\x03\x04 not a 7z at all").unwrap();
    let err = SevenZProvider::new()
        .list(&list_req(&fake, ""))
        .unwrap_err();
    assert_eq!(err.code, Code::UnsupportedFormat);
}

#[test]
fn reads_member_bounded_and_truncated() {
    let owned = fixture("owned.7z");
    let provider = SevenZProvider::new();

    let mut full = Vec::new();
    let truncated = provider
        .read(
            &read_req(&owned, "nested/東京.txt", u64::MAX, None),
            &mut |c| {
                full.extend_from_slice(c);
                Ok(())
            },
        )
        .unwrap();
    assert!(!truncated);
    assert_eq!(full, TEXT);

    let mut head = Vec::new();
    let truncated = provider
        .read(&read_req(&owned, "nested/東京.txt", 10, None), &mut |c| {
            head.extend_from_slice(c);
            Ok(())
        })
        .unwrap();
    assert!(truncated);
    assert_eq!(head, &TEXT[..10]);
}

#[test]
fn missing_member_read_is_not_found() {
    let owned = fixture("owned.7z");
    let err = SevenZProvider::new()
        .read(
            &read_req(&owned, "nested/absent.txt", 1024, None),
            &mut |_| Ok(()),
        )
        .unwrap_err();
    assert_eq!(err.code, Code::Io);
    assert_eq!(err.member_path.as_deref(), Some("nested/absent.txt"));
}

#[test]
fn directory_member_read_is_rejected() {
    let owned = fixture("owned.7z");
    let err = SevenZProvider::new()
        .read(&read_req(&owned, "nested", 1024, None), &mut |_| Ok(()))
        .unwrap_err();
    assert_eq!(err.code, Code::Io);
}

#[test]
fn encrypted_header_needs_password_even_to_list() {
    let encrypted = fixture("encrypted.7z");
    let err = SevenZProvider::new()
        .list(&list_req(&encrypted, ""))
        .unwrap_err();
    assert_eq!(
        err.code,
        Code::PasswordRequired,
        "header-encrypted 7z cannot list anonymously"
    );

    // FER-504: the password rides the request — the listing succeeds, and a
    // wrong password is a structured BadPassword (re-prompt), not corruption.
    let mut req = list_req(&encrypted, "");
    req.password = Some(PASSWORD.to_owned());
    let resp = SevenZProvider::new().list(&req).unwrap();
    assert!(!resp.entries.is_empty());

    let mut wrong = list_req(&encrypted, "");
    wrong.password = Some("wrong".to_owned());
    let err = SevenZProvider::new().list(&wrong).unwrap_err();
    assert_eq!(err.code, Code::BadPassword);
}

#[test]
fn encrypted_password_matrix_for_read() {
    let encrypted = fixture("encrypted.7z");
    let provider = SevenZProvider::new();

    // No password → structured PasswordRequired (never a silent empty read).
    let mut sink = Vec::new();
    let err = provider
        .read(
            &read_req(&encrypted, "nested/東京.txt", u64::MAX, None),
            &mut |c| {
                sink.extend_from_slice(c);
                Ok(())
            },
        )
        .unwrap_err();
    assert_eq!(err.code, Code::PasswordRequired, "no password");
    assert!(sink.is_empty(), "no plaintext may leak without password");

    // Wrong password → BadPassword.
    let err = provider
        .read(
            &read_req(&encrypted, "nested/東京.txt", u64::MAX, Some("wrong")),
            &mut |_| Ok(()),
        )
        .unwrap_err();
    assert_eq!(err.code, Code::BadPassword, "wrong password");

    // Correct password → the exact fixture payload.
    let mut out = Vec::new();
    let truncated = provider
        .read(
            &read_req(&encrypted, "nested/東京.txt", u64::MAX, Some(PASSWORD)),
            &mut |c| {
                out.extend_from_slice(c);
                Ok(())
            },
        )
        .unwrap();
    assert!(!truncated);
    assert_eq!(out, TEXT);
}

#[test]
fn extract_publishes_full_archive_through_pipeline() {
    let owned = fixture("owned.7z");
    let dest = temp("extract");
    std::fs::create_dir_all(&dest).unwrap();

    SevenZProvider::new()
        .extract(&extract_req(&owned, &dest, None), &mut |_| true)
        .unwrap();

    let published = dest.join("nested").join("東京.txt");
    assert_eq!(std::fs::read(&published).unwrap(), TEXT);
    // No staging residue next to the published tree.
    let residue: Vec<_> = std::fs::read_dir(&dest)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains("ferrum-partial"))
        .collect();
    assert!(residue.is_empty(), "staging leftovers: {residue:?}");
}

#[test]
fn extract_member_selection_respects_subtree() {
    let owned = fixture("owned.7z");
    let dest = temp("extract-select");
    std::fs::create_dir_all(&dest).unwrap();

    let mut req = extract_req(&owned, &dest, None);
    req.member_paths = vec![MemberPath::parse("nested").unwrap()];
    SevenZProvider::new().extract(&req, &mut |_| true).unwrap();

    assert_eq!(
        std::fs::read(dest.join("nested").join("東京.txt")).unwrap(),
        TEXT
    );
}

#[test]
fn damaged_payload_fails_structured() {
    let damaged = fixture("damaged-payload.7z");
    let provider = SevenZProvider::new();

    // Discover the first file member dynamically — the fixture's internal
    // naming is the spike's business, not this test's.
    let root = provider.list(&list_req(&damaged, "")).unwrap();
    let mut target: Option<String> = root
        .entries
        .iter()
        .find(|e| !e.is_dir)
        .map(|e| e.member_path.as_str().to_owned());
    if target.is_none() {
        let sub = provider
            .list(&list_req(&damaged, &root.entries[0].name))
            .unwrap();
        target = sub
            .entries
            .iter()
            .find(|e| !e.is_dir)
            .map(|e| e.member_path.as_str().to_owned());
    }
    let target = target.expect("damaged fixture has a file member");

    let mut sink = Vec::new();
    let err = provider
        .read(&read_req(&damaged, &target, u64::MAX, None), &mut |c| {
            sink.extend_from_slice(c);
            Ok(())
        })
        .unwrap_err();
    assert_eq!(
        err.code,
        Code::CorruptArchive,
        "CRC verification must reject flipped payload bytes (got {:?} for {target})",
        err
    );
}

#[test]
fn corrupt_header_fails_at_open() {
    let corrupt = fixture("corrupt.7z");
    let err = SevenZProvider::new()
        .list(&list_req(&corrupt, ""))
        .unwrap_err();
    assert_eq!(err.code, Code::CorruptArchive);
}

#[test]
fn capabilities_are_honest() {
    let caps = SevenZProvider::new().capabilities(ArchiveFormat::Sevenz);
    assert!(caps.list && caps.read && caps.extract && caps.test && caps.create);
    assert!(caps.encrypted_read);
    assert!(!caps.encrypted_create, "create_7z does not encrypt");
    assert!(!caps.multi_volume);
}
