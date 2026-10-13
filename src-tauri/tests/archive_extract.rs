#[path = "../src/archive_contract.rs"]
#[allow(dead_code)] // contract surface: full use lands with F2-12 integration
mod archive_contract;
#[allow(clippy::duplicate_mod, dead_code)]
#[path = "../src/archive_extract.rs"]
mod archive_extract;

use archive_contract::{ArchiveConflictPolicy as Policy, ArchiveErrorCode as Code, ArchiveLimits};
use archive_extract::{Entry, EntryKind, extract, preflight, stream_checked};
use std::{
    io,
    path::{Path, PathBuf},
};

fn limits() -> ArchiveLimits {
    ArchiveLimits {
        max_entries: 8,
        max_total_bytes: 16,
        max_entry_bytes: 12,
        max_depth: 3,
    }
}
fn file(name: &str, size: Option<u64>) -> Entry<'_> {
    Entry {
        raw_name: name,
        kind: EntryKind::File,
        expanded_size: size,
        compressed_size: Some(1),
        modified_ms: Some(1_000),
    }
}
fn dir(name: &str) -> Entry<'_> {
    Entry {
        raw_name: name,
        kind: EntryKind::Directory,
        expanded_size: None,
        compressed_size: None,
        modified_ms: None,
    }
}

#[test]
fn rejects_malicious_names_and_links_before_disk_write() {
    for name in [
        "../escape",
        "a\\..\\escape",
        "/absolute",
        "\\absolute",
        "C:drive",
        "C:\\drive",
        "\\\\host\\share",
        "a//b",
        "a/./b",
        "a/",
        "a\0b",
        "a:b",
        "CON.txt",
        "LPT1",
        "COM¹",
        "a. ",
        "a<bad",
        "a/b/c/d",
    ] {
        let err = preflight(&[file(name, Some(1))], &limits()).unwrap_err();
        assert!(
            matches!(err.code, Code::UnsafeEntry | Code::LimitExceeded),
            "{name:?}: {err:?}"
        );
    }
    assert_eq!(
        preflight(
            &[Entry {
                raw_name: "link",
                kind: EntryKind::Symlink,
                expanded_size: None,
                compressed_size: None,
                modified_ms: None
            }],
            &limits()
        )
        .unwrap_err()
        .code,
        Code::UnsafeEntry
    );
    assert_eq!(
        preflight(
            &[Entry {
                raw_name: "device",
                kind: EntryKind::Other,
                expanded_size: None,
                compressed_size: None,
                modified_ms: None
            }],
            &limits()
        )
        .unwrap_err()
        .code,
        Code::UnsafeEntry
    );
}

#[test]
fn detects_collisions_and_prefix_conflicts() {
    for entries in [
        vec![file("A.txt", Some(1)), file("a.TXT", Some(1))],
        vec![file("a", Some(1)), file("a/b", Some(1))],
        vec![file("A/b", Some(1)), file("a", Some(1))],
        vec![dir("a"), file("A", Some(1))],
    ] {
        assert_eq!(
            preflight(&entries, &limits()).unwrap_err().code,
            Code::Conflict
        );
    }
    preflight(&[dir("a"), file("a/b", Some(1))], &limits()).unwrap();
}

#[test]
fn enforces_metadata_and_runtime_quotas() {
    let mut small = limits();
    small.max_entries = 1;
    assert_eq!(
        preflight(&[file("a", None), file("b", None)], &small)
            .unwrap_err()
            .code,
        Code::LimitExceeded
    );
    assert_eq!(
        preflight(&[file("a", Some(13))], &limits())
            .unwrap_err()
            .code,
        Code::LimitExceeded
    );
    assert_eq!(
        preflight(&[file("a", Some(10)), file("b", Some(7))], &limits())
            .unwrap_err()
            .code,
        Code::LimitExceeded
    );
    let plan = preflight(&[file("a", Some(1))], &limits()).unwrap();
    let mut output = Vec::new();
    let err = stream_checked(
        &plan,
        &limits(),
        &mut output,
        |_, writer| {
            writer
                .write_all(&[1; 13])
                .map_err(archive_extract::io_error)
        },
        || false,
    )
    .unwrap_err();
    assert_eq!(err.code, Code::LimitExceeded);
    assert!(output.is_empty());
    let plan = preflight(&[file("a", None), file("b", None)], &limits()).unwrap();
    let err = stream_checked(
        &plan,
        &limits(),
        &mut output,
        |_, writer| {
            writer
                .write_all(&[1; 10])
                .map_err(archive_extract::io_error)
        },
        || false,
    )
    .unwrap_err();
    assert_eq!(err.code, Code::LimitExceeded);
    assert_eq!(output.len(), 10); // Caller must discard partial output on failure.
}

#[test]
fn streams_normally_and_warns_on_ratio() {
    let plan = preflight(&[dir("docs"), file("docs\\note.txt", Some(5))], &limits()).unwrap();
    assert_eq!(plan.entries()[1].relative_path, Path::new("docs/note.txt"));
    let ratio = preflight(
        &[file("compressed", Some(100))],
        &ArchiveLimits {
            max_entry_bytes: 100,
            max_total_bytes: 100,
            ..limits()
        },
    )
    .unwrap();
    assert_eq!(ratio.ratio_warnings(), &[0]);
    let mut bytes = Vec::new();
    let total = stream_checked(
        &plan,
        &limits(),
        &mut bytes,
        |_, writer| {
            writer
                .write_all(b"hello")
                .map_err(archive_extract::io_error)
        },
        || false,
    )
    .unwrap();
    assert_eq!(total, 5);
    assert_eq!(bytes, b"hello");
}

#[test]
fn cancellation_and_io_fault_remain_structured() {
    let plan = preflight(&[file("a", None)], &limits()).unwrap();
    let mut bytes = Vec::new();
    let err = stream_checked(
        &plan,
        &limits(),
        &mut bytes,
        |_, writer| writer.write_all(b"data").map_err(archive_extract::io_error),
        || true,
    )
    .unwrap_err();
    assert_eq!(err.code, Code::Cancelled);
    assert!(bytes.is_empty());
    let err = stream_checked(
        &plan,
        &limits(),
        &mut bytes,
        |_, _| Err(archive_extract::io_error(io::Error::from_raw_os_error(28))),
        || false,
    )
    .unwrap_err();
    assert_eq!(err.code, Code::Io);
}

#[test]
fn trusted_ceilings_ignore_untrusted_maximums() {
    let raised = ArchiveLimits {
        max_entries: u64::MAX,
        max_total_bytes: u64::MAX,
        max_entry_bytes: u64::MAX,
        max_depth: u32::MAX,
    };
    let entries = vec![file("a", None); 10_001];
    assert_eq!(
        preflight(&entries, &raised).unwrap_err().code,
        Code::LimitExceeded
    );
    assert_eq!(
        preflight(&[file("a", Some(512 * 1024 * 1024 + 1))], &raised)
            .unwrap_err()
            .code,
        Code::LimitExceeded
    );
    let names: Vec<_> = (0..5).map(|i| format!("large{i}")).collect();
    let total: Vec<_> = names
        .iter()
        .map(|n| file(n, Some(500 * 1024 * 1024)))
        .collect();
    assert_eq!(
        preflight(&total, &raised).unwrap_err().code,
        Code::LimitExceeded
    );
    let deep = (0..33).map(|_| "a").collect::<Vec<_>>().join("/");
    assert_eq!(
        preflight(&[file(&deep, None)], &raised).unwrap_err().code,
        Code::LimitExceeded
    );
}

#[test]
fn rejects_oversized_names_and_cumulative_metadata() {
    let raised = ArchiveLimits {
        max_entries: u64::MAX,
        max_total_bytes: u64::MAX,
        max_entry_bytes: u64::MAX,
        max_depth: u32::MAX,
    };
    let huge = "a".repeat(4097);
    assert_eq!(
        preflight(&[file(&huge, None)], &raised).unwrap_err().code,
        Code::LimitExceeded
    );
    let names: Vec<_> = (0..4097)
        .map(|i| format!("{i:04}{}", "a".repeat(4092)))
        .collect();
    let entries: Vec<_> = names.iter().map(|name| file(name, None)).collect();
    assert_eq!(
        preflight(&entries, &raised).unwrap_err().code,
        Code::LimitExceeded
    );
}

#[test]
fn directory_suffix_normalizes_only_directories() {
    let plan = preflight(&[dir("docs/"), file("docs/note", None)], &limits()).unwrap();
    assert_eq!(plan.entries()[0].relative_path, Path::new("docs"));
    assert_eq!(
        preflight(&[dir("docs/"), dir("DOCS")], &limits())
            .unwrap_err()
            .code,
        Code::Conflict
    );
    for name in ["docs/", "docs\\"] {
        assert_eq!(
            preflight(&[file(name, None)], &limits()).unwrap_err().code,
            Code::UnsafeEntry
        );
    }
    assert_eq!(
        preflight(&[dir("docs//")], &limits()).unwrap_err().code,
        Code::UnsafeEntry
    );
}

#[test]
fn rejects_unicode_decomposition_aliases() {
    for name in ["cafe\u{301}.txt", "\u{1100}\u{1161}.txt", "\u{212b}.txt"] {
        assert_eq!(
            preflight(&[file(name, None)], &limits()).unwrap_err().code,
            Code::UnsafeEntry
        );
    }
}

#[test]
fn rejects_declared_size_mismatch_after_streaming() {
    for (size, written) in [(Some(5), b"four".as_slice()), (Some(3), b"four".as_slice())] {
        let plan = preflight(&[file("note", size)], &limits()).unwrap();
        let mut output = Vec::new();
        assert_eq!(
            stream_checked(
                &plan,
                &limits(),
                &mut output,
                |_, writer| writer.write_all(written).map_err(archive_extract::io_error),
                || false
            )
            .unwrap_err()
            .code,
            Code::UnsafeEntry
        );
    }
}

#[test]
fn swallowed_sink_failure_still_fails_stream() {
    struct Broken;
    impl io::Write for Broken {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("sink failed"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let plan = preflight(&[file("note", None)], &limits()).unwrap();
    let err = stream_checked(
        &plan,
        &limits(),
        &mut Broken,
        |_, writer| {
            let _ = writer.write_all(b"lost");
            Ok(())
        },
        || false,
    )
    .unwrap_err();
    assert_eq!(err.code, Code::Io);
}

#[test]
fn publishing_fails_closed_before_callback_on_missing_destination() {
    let plan = preflight(&[file("sub/escape", None)], &limits()).unwrap();
    let mut called = false;
    // An unusable destination must fail before the backend stream is invoked.
    let error = extract(
        &plan,
        Path::new("Z:/nonexistent/destination"),
        &limits(),
        Policy::Skip,
        |_, _| {
            called = true;
            Ok(())
        },
        || false,
    )
    .unwrap_err();
    assert_ne!(error.code, Code::Cancelled);
    assert!(
        !called,
        "backend must not stream into an unusable destination"
    );
    for policy in [Policy::Overwrite, Policy::Rename, Policy::OverwriteOlder] {
        let mut called = false;
        let _ = extract(
            &plan,
            Path::new("Z:/nonexistent/destination"),
            &limits(),
            policy,
            |_, _| {
                called = true;
                Ok(())
            },
            || false,
        )
        .unwrap_err();
        assert!(!called);
    }
}

#[cfg(windows)]
fn make_junction(link: &Path, target: &Path) -> bool {
    std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            &format!(
                "New-Item -ItemType Junction -Path '{}' -Target '{}' | Out-Null",
                link.display(),
                target.display()
            ),
        ])
        .output()
        .map(|o| {
            if !o.status.success() {
                eprintln!("mklink stderr: {}", String::from_utf8_lossy(&o.stderr));
            }
            o.status.success()
        })
        .unwrap_or(false)
}

/// Owned temp dir under %TEMP%, removed on Drop (also on assertion failure).
#[cfg(windows)]
struct TempDir(PathBuf);

#[cfg(windows)]
impl TempDir {
    fn new(tag: &str) -> Self {
        let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
        let dir = base.join(format!("ferrum-x-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir(dir.join("dest").join("sub"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }

    #[allow(dead_code)]
    fn path(&self) -> &Path {
        &self.0
    }
}

#[cfg(windows)]
impl std::ops::Deref for TempDir {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

#[cfg(windows)]
impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

#[cfg(windows)]
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir(self.0.join("dest").join("sub"));
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
#[cfg(windows)]
fn junction_swapped_member_dir_is_refused_without_outside_writes() {
    let base = TempDir::new("junction");
    std::fs::create_dir_all(base.join("dest")).unwrap();
    let outside = base.join("outside");
    std::fs::create_dir_all(&outside).unwrap();

    // Member directory pre-exists as a junction pointing outside.
    assert!(
        make_junction(&base.join("dest/sub"), &outside),
        "junction creation must work unprivileged"
    );

    let plan = preflight(&[file("sub/escape.txt", Some(5))], &limits()).unwrap();
    let error = extract(
        &plan,
        &base.join("dest"),
        &limits(),
        Policy::Skip,
        |_, writer| writer.write_all(b"data").map_err(archive_extract::io_error),
        || false,
    )
    .unwrap_err();
    assert_eq!(error.code, Code::UnsafeEntry);
    // Nothing may appear outside the destination through the junction.
    assert!(
        std::fs::read_dir(&outside).unwrap().next().is_none(),
        "junction traversal must not leak writes"
    );
}

#[test]
#[cfg(windows)]
fn extraction_writes_nested_files_with_real_handles() {
    let base = TempDir::new("ok");

    let plan = preflight(
        &[
            dir("docs"),
            file("docs/note.txt", Some(5)),
            file("top.bin", Some(3)),
        ],
        &limits(),
    )
    .unwrap();
    let report = extract(
        &plan,
        &base,
        &limits(),
        Policy::Skip,
        |entry, writer| {
            let content = if entry.raw_name.ends_with("note.txt") {
                &b"hello"[..]
            } else {
                &b"abc"[..]
            };
            writer.write_all(content).map_err(archive_extract::io_error)
        },
        || false,
    )
    .unwrap();
    assert_eq!(report.written_files, 2);
    assert_eq!(
        std::fs::read(base.join("docs").join("note.txt")).unwrap(),
        b"hello"
    );
    assert_eq!(std::fs::read(base.join("top.bin")).unwrap(), b"abc");
}

#[test]
#[cfg(windows)]
fn cancel_midstream_removes_partial_file_via_handle() {
    let base = TempDir::new("cancel");

    let big_limits = ArchiveLimits {
        max_total_bytes: 1024,
        max_entry_bytes: 1024,
        ..limits()
    };
    let plan = preflight(&[file("big.bin", Some(100))], &big_limits).unwrap();
    let error = extract(
        &plan,
        &base,
        &big_limits,
        Policy::Skip,
        |_, writer| {
            writer
                .write_all(&[0u8; 12])
                .map_err(archive_extract::io_error)?;
            Err(archive_extract::io_error(io::Error::from_raw_os_error(28)))
        },
        || false,
    )
    .unwrap_err();
    assert_eq!(error.code, Code::Io);
    // The partial file must not survive as a seemingly-complete artifact.
    let leftovers: Vec<_> = std::fs::read_dir(&base)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file())
        .collect();
    assert!(
        leftovers.is_empty(),
        "partial file must be removed: {leftovers:?}"
    );
}

#[test]
#[cfg(windows)]
fn conflict_policies_publish_differently() {
    let base = TempDir::new("conflict");
    std::fs::write(base.join("keep.txt"), b"OLD").unwrap();

    let plan = preflight(&[file("keep.txt", Some(3))], &limits()).unwrap();
    fn stream(
        _: &archive_extract::PlannedEntry,
        writer: &mut dyn std::io::Write,
    ) -> Result<(), archive_contract::ArchiveError> {
        writer.write_all(b"NEW").map_err(archive_extract::io_error)
    }

    // Skip keeps the old content.
    let report = extract(&plan, &base, &limits(), Policy::Skip, stream, || false).unwrap();
    assert_eq!(report.skipped_files, 1);
    assert_eq!(std::fs::read(base.join("keep.txt")).unwrap(), b"OLD");

    // Rename writes a sibling slot.
    let report = extract(&plan, &base, &limits(), Policy::Rename, stream, || false).unwrap();
    assert_eq!(report.written_files, 1);
    assert_eq!(std::fs::read(base.join("keep (2).txt")).unwrap(), b"NEW");
    assert_eq!(std::fs::read(base.join("keep.txt")).unwrap(), b"OLD");

    // Overwrite replaces the content through the validated handle.
    let report = extract(&plan, &base, &limits(), Policy::Overwrite, stream, || false).unwrap();
    assert_eq!(report.written_files, 1);
    assert_eq!(std::fs::read(base.join("keep.txt")).unwrap(), b"NEW");
}
