//! F2-04 tests: ZIP listing/read via ZipProvider against in-test fixtures
//! (zip crate writer). Covers: basic listing, implicit directories, Unicode,
//! empty archive, nested paths, read bounded/truncated, signature sniff.

#[path = "../src/archive_contract.rs"]
#[allow(dead_code)] // contract surface: full use lands with F2-12 integration
mod archive_contract;
#[allow(clippy::duplicate_mod, dead_code)]
#[path = "../src/archive_create_test.rs"]
mod archive_create_test;
#[allow(clippy::duplicate_mod, dead_code)]
#[path = "../src/archive_extract.rs"]
mod archive_extract;
#[path = "../src/archive_zip.rs"]
mod archive_zip;
#[path = "../src/archive_publish.rs"]
pub mod publish;

use archive_contract::{
    ArchiveErrorCode as Code, ArchiveFormat, ArchiveLimits, ArchiveListRequest, ArchiveProvider,
    Location, MemberPath,
};
use archive_zip::ZipProvider;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

/// Owned temp dir under %TEMP%, removed on Drop (also on assertion failure).
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "ferrum-zip-{tag}-{}-{}",
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

fn write_zip(path: &Path, entries: &[(&str, Option<&[u8]>)]) {
    let file = std::fs::File::create(path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default();
    for (name, content) in entries {
        match content {
            Some(bytes) => {
                zip.start_file(*name, opts).unwrap();
                zip.write_all(bytes).unwrap();
            }
            None => zip.add_directory(*name, opts).unwrap(),
        }
    }
    zip.finish().unwrap();
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

#[test]
fn lists_root_children_with_implicit_dirs() {
    let dir = temp("implicit");
    let zip_path = dir.join("a.zip");
    write_zip(
        &zip_path,
        &[
            ("top.txt", Some(b"hello")),
            ("docs/readme.md", Some(b"readme")),
            ("docs/deep/x.txt", Some(b"x")),
        ],
    );

    let provider = ZipProvider::new();
    let resp = provider.list(&list_req(&zip_path, "")).unwrap();
    let names: Vec<(&str, bool)> = resp
        .entries
        .iter()
        .map(|e| (e.name.as_str(), e.is_dir))
        .collect();
    assert_eq!(
        names,
        vec![("docs", true), ("top.txt", false)],
        "implicit dir synthesized; folders first"
    );
    assert_eq!(resp.entries[0].member_path.as_str(), "docs");
}

#[test]
fn lists_subdirectory_children() {
    let dir = temp("sub");
    let zip_path = dir.join("a.zip");
    write_zip(
        &zip_path,
        &[
            ("docs/readme.md", Some(b"readme")),
            ("docs/deep/x.txt", Some(b"x")),
            ("other.txt", Some(b"o")),
        ],
    );

    let provider = ZipProvider::new();
    let resp = provider.list(&list_req(&zip_path, "docs")).unwrap();
    let names: Vec<&str> = resp.entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, vec!["deep", "readme.md"]);
    assert_eq!(resp.entries[0].member_path.as_str(), "docs/deep");
}

#[test]
fn unicode_names_and_sizes() {
    let dir = temp("unicode");
    let zip_path = dir.join("a.zip");
    write_zip(
        &zip_path,
        &[
            ("b\u{e9}k\u{e9}r.txt", Some(b"12345")),
            ("data.bin", Some(&[7u8; 1000])),
        ],
    );

    let provider = ZipProvider::new();
    let resp = provider.list(&list_req(&zip_path, "")).unwrap();
    let by_name = |n: &str| {
        resp.entries
            .iter()
            .find(|e| e.name == n)
            .unwrap_or_else(|| panic!("{n} missing"))
            .clone()
    };
    assert_eq!(by_name("b\u{e9}k\u{e9}r.txt").size, 5);
    assert_eq!(by_name("data.bin").size, 1000);
}

#[test]
fn empty_archive_lists_nothing() {
    let dir = temp("empty");
    let zip_path = dir.join("empty.zip");
    write_zip(&zip_path, &[]);

    let provider = ZipProvider::new();
    let resp = provider.list(&list_req(&zip_path, "")).unwrap();
    assert!(resp.entries.is_empty());
}

#[test]
fn non_zip_signature_is_unsupported_format() {
    let dir = temp("bad");
    let zip_path = dir.join("fake.zip");
    std::fs::write(&zip_path, b"definitely not a zip file").unwrap();

    let provider = ZipProvider::new();
    let err = provider.list(&list_req(&zip_path, "")).unwrap_err();
    assert_eq!(err.code, Code::UnsupportedFormat);
}

#[test]
fn read_member_bounded_and_truncated() {
    let dir = temp("read");
    let zip_path = dir.join("a.zip");
    let content: Vec<u8> = (0..10_000u32).map(|i| i as u8).collect();
    write_zip(&zip_path, &[("data.bin", Some(&content))]);

    let provider = ZipProvider::new();
    let location = Location::Archive {
        archive_path: zip_path.to_string_lossy().into_owned(),
        member_path: MemberPath::parse("data.bin").unwrap(),
    };

    // Full read within max_bytes.
    let mut out = Vec::new();
    let truncated = provider
        .read(
            &archive_contract::ArchiveReadRequest {
                location: location.clone(),
                max_bytes: 20_000,
                password: None,
            },
            &mut |chunk| {
                out.extend_from_slice(chunk);
                Ok(())
            },
        )
        .unwrap();
    assert!(!truncated);
    assert_eq!(out, content);

    // Bounded read truncates at max_bytes.
    out.clear();
    let truncated = provider
        .read(
            &archive_contract::ArchiveReadRequest {
                location,
                max_bytes: 100,
                password: None,
            },
            &mut |chunk| {
                out.extend_from_slice(chunk);
                Ok(())
            },
        )
        .unwrap();
    assert!(truncated);
    assert_eq!(out.len(), 100);
    assert_eq!(&out[..50], &content[..50]);
}

#[test]
fn missing_member_read_is_not_found() {
    let dir = temp("missing");
    let zip_path = dir.join("a.zip");
    write_zip(&zip_path, &[("real.txt", Some(b"x"))]);

    let provider = ZipProvider::new();
    let err = provider
        .read(
            &archive_contract::ArchiveReadRequest {
                location: Location::Archive {
                    archive_path: zip_path.to_string_lossy().into_owned(),
                    member_path: MemberPath::parse("ghost.txt").unwrap(),
                },
                max_bytes: 100,
                password: None,
            },
            &mut |_| Ok(()),
        )
        .unwrap_err();
    assert_eq!(err.code, Code::Io);
}

#[test]
fn large_directory_listing_is_flat_and_sorted() {
    let dir = temp("large");
    let zip_path = dir.join("big.zip");
    let n = 10_000;
    let mut entries: Vec<(String, Option<&[u8]>)> = Vec::with_capacity(n + 1);
    let one = b"x";
    for i in 0..n {
        entries.push((format!("f{i:05}.txt"), Some(one)));
    }
    entries.push(("z-dir".into(), None));
    let refs: Vec<(&str, Option<&[u8]>)> = entries
        .iter()
        .map(|(name, content)| (name.as_str(), *content))
        .collect();
    write_zip(&zip_path, &refs);

    let provider = ZipProvider::new();
    let resp = provider.list(&list_req(&zip_path, "")).unwrap();
    assert_eq!(resp.entries.len(), n + 1);
    // All direct children: flat, no nested paths.
    assert!(
        resp.entries
            .iter()
            .all(|e| !e.member_path.as_str().contains('/'))
    );
    // Folders first, then case-insensitive ascending.
    assert!(resp.entries[0].is_dir);
    assert_eq!(resp.entries[1].name, "f00000.txt");
}

#[test]
fn capabilities_are_honest() {
    let caps = ZipProvider::new().capabilities(ArchiveFormat::Zip);
    assert!(caps.list && caps.read && caps.extract && caps.test);
    assert!(caps.encrypted_read);
    assert!(!caps.encrypted_create);
    assert!(!caps.multi_volume);
    caps.validate().unwrap();
    let _ = ArchiveLimits {
        max_entries: 100,
        max_total_bytes: 1000,
        max_entry_bytes: 100,
        max_depth: 4,
    };
}

// ── F2-05: extraction through the shared safe pipeline ──────────────────

fn extract_req(
    path: &Path,
    dest: &Path,
    policy: archive_contract::ArchiveConflictPolicy,
) -> archive_contract::ArchiveExtractRequest {
    archive_contract::ArchiveExtractRequest {
        archive_path: path.to_string_lossy().into_owned(),
        member_paths: vec![],
        destination_path: dest.to_string_lossy().into_owned(),
        conflict: policy,
        password: None,
        limits: ArchiveLimits {
            max_entries: 100,
            max_total_bytes: 1 << 20,
            max_entry_bytes: 1 << 20,
            max_depth: 8,
        },
    }
}

#[test]
fn extracts_full_archive_via_pipeline() {
    let dir = temp("x-full");
    let zip_path = dir.join("a.zip");
    write_zip(
        &zip_path,
        &[
            ("docs/note.txt", Some(b"hello note")),
            ("top.bin", Some(b"abc")),
        ],
    );
    let dest = dir.join("out");
    std::fs::create_dir_all(&dest).unwrap();

    let provider = ZipProvider::new();
    provider
        .extract(
            &extract_req(
                &zip_path,
                &dest,
                archive_contract::ArchiveConflictPolicy::Skip,
            ),
            &mut |_| true,
        )
        .unwrap();

    assert_eq!(
        std::fs::read(dest.join("docs").join("note.txt")).unwrap(),
        b"hello note"
    );
    assert_eq!(std::fs::read(dest.join("top.bin")).unwrap(), b"abc");
    // No leftover staging names.
    let leftovers: Vec<_> = std::fs::read_dir(&dest)
        .unwrap()
        .filter_map(|e| e.ok())
        .flat_map(|e| {
            if e.path().is_dir() {
                std::fs::read_dir(e.path())
                    .unwrap()
                    .filter_map(|x| x.ok())
                    .map(|x| x.path())
                    .collect::<Vec<_>>()
            } else {
                vec![e.path()]
            }
        })
        .filter(|p| p.to_string_lossy().contains("ferrum-partial"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[test]
fn extraction_conflict_policies_vary_outcome() {
    let dir = temp("x-conflict");
    let zip_path = dir.join("a.zip");
    write_zip(&zip_path, &[("keep.txt", Some(b"NEW"))]);
    let dest = dir.join("out");
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(dest.join("keep.txt"), b"OLD").unwrap();

    let provider = ZipProvider::new();

    // Skip preserves the old content.
    provider
        .extract(
            &extract_req(
                &zip_path,
                &dest,
                archive_contract::ArchiveConflictPolicy::Skip,
            ),
            &mut |_| true,
        )
        .unwrap();
    assert_eq!(std::fs::read(dest.join("keep.txt")).unwrap(), b"OLD");

    // Rename writes a sibling slot.
    provider
        .extract(
            &extract_req(
                &zip_path,
                &dest,
                archive_contract::ArchiveConflictPolicy::Rename,
            ),
            &mut |_| true,
        )
        .unwrap();
    assert_eq!(std::fs::read(dest.join("keep (2).txt")).unwrap(), b"NEW");
    assert_eq!(std::fs::read(dest.join("keep.txt")).unwrap(), b"OLD");

    // Overwrite replaces content.
    provider
        .extract(
            &extract_req(
                &zip_path,
                &dest,
                archive_contract::ArchiveConflictPolicy::Overwrite,
            ),
            &mut |_| true,
        )
        .unwrap();
    assert_eq!(std::fs::read(dest.join("keep.txt")).unwrap(), b"NEW");
}

#[test]
fn partial_selection_extracts_only_subtree() {
    let dir = temp("x-partial");
    let zip_path = dir.join("a.zip");
    write_zip(
        &zip_path,
        &[("docs/note.txt", Some(b"n")), ("other.txt", Some(b"o"))],
    );
    let dest = dir.join("out");
    std::fs::create_dir_all(&dest).unwrap();

    let mut req = extract_req(
        &zip_path,
        &dest,
        archive_contract::ArchiveConflictPolicy::Skip,
    );
    req.member_paths = vec![MemberPath::parse("docs").unwrap()];
    let provider = ZipProvider::new();
    provider.extract(&req, &mut |_| true).unwrap();

    assert_eq!(
        std::fs::read(dest.join("docs").join("note.txt")).unwrap(),
        b"n"
    );
    assert!(!dest.join("other.txt").exists());
}

#[test]
fn cancellation_midstream_leaves_no_partial_final_name() {
    let dir = temp("x-cancel");
    let zip_path = dir.join("a.zip");
    write_zip(&zip_path, &[("big.bin", Some(&[9u8; 100_000]))]);
    let dest = dir.join("out");
    std::fs::create_dir_all(&dest).unwrap();

    let provider = ZipProvider::new();
    let mut calls = 0;
    let err = provider
        .extract(
            &extract_req(
                &zip_path,
                &dest,
                archive_contract::ArchiveConflictPolicy::Skip,
            ),
            &mut |_| {
                calls += 1;
                calls > 1 // cancel after the first progress tick
            },
        )
        .unwrap_err();
    assert_eq!(err.code, Code::Cancelled);
    // The FINAL name must never exist with partial bytes; staging is
    // cleaned up on rollback.
    assert!(!dest.join("big.bin").exists());
    let leftovers: Vec<_> = std::fs::read_dir(&dest)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains("ferrum-partial"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}
