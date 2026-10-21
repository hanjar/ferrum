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

// ---- FER-584: the commit never replaces a file that appeared after the exists check ----

#[cfg(windows)]
fn roomy() -> ArchiveLimits {
    ArchiveLimits {
        max_entries: 8,
        max_total_bytes: 1 << 20,
        max_entry_bytes: 1 << 20,
        max_depth: 8,
    }
}

/// Extract `keep.txt` ("NEW") into `dest` under `policy`; the file `racer` is created by a third
/// party while the member is being decoded, i.e. AFTER the exists check and BEFORE the commit.
#[cfg(windows)]
fn extract_with_racer(dest: &Path, policy: Policy, racer: &Path) -> archive_extract::ExtractReport {
    let plan = preflight(&[file("keep.txt", Some(3))], &roomy()).unwrap();
    let mut raced = false;
    extract(
        &plan,
        dest,
        &roomy(),
        policy,
        |_, writer| {
            if !raced {
                raced = true;
                std::fs::write(racer, b"RACE").unwrap();
            }
            writer.write_all(b"NEW").map_err(archive_extract::io_error)
        },
        || false,
    )
    .unwrap()
}

#[cfg(windows)]
fn names_in(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

#[test]
#[cfg(windows)]
fn skip_does_not_overwrite_a_file_created_during_extraction() {
    let base = TempDir::new("race-skip");
    let report = extract_with_racer(&base, Policy::Skip, &base.join("keep.txt"));
    assert_eq!((report.written_files, report.skipped_files), (0, 1));
    assert_eq!(std::fs::read(base.join("keep.txt")).unwrap(), b"RACE");
    assert_eq!(names_in(&base), ["keep.txt"], "staging must be removed");
}

#[test]
#[cfg(windows)]
fn rename_picks_a_new_name_for_a_file_created_during_extraction() {
    let base = TempDir::new("race-rename");
    let report = extract_with_racer(&base, Policy::Rename, &base.join("keep.txt"));
    assert_eq!((report.written_files, report.skipped_files), (1, 0));
    assert_eq!(std::fs::read(base.join("keep.txt")).unwrap(), b"RACE");
    assert_eq!(std::fs::read(base.join("keep (2).txt")).unwrap(), b"NEW");
    assert_eq!(names_in(&base), ["keep (2).txt", "keep.txt"]);
}

#[test]
#[cfg(windows)]
fn rename_skips_slots_taken_during_extraction_too() {
    let base = TempDir::new("race-rename2");
    std::fs::write(base.join("keep.txt"), b"OLD").unwrap();
    // keep.txt exists up front; "keep (2).txt" appears during the decode.
    let report = extract_with_racer(&base, Policy::Rename, &base.join("keep (2).txt"));
    assert_eq!(report.written_files, 1);
    assert_eq!(std::fs::read(base.join("keep.txt")).unwrap(), b"OLD");
    assert_eq!(std::fs::read(base.join("keep (2).txt")).unwrap(), b"RACE");
    assert_eq!(std::fs::read(base.join("keep (3).txt")).unwrap(), b"NEW");
}

#[test]
#[cfg(windows)]
fn chosen_overwrite_replaces_even_a_file_created_during_extraction() {
    let base = TempDir::new("race-overwrite");
    let report = extract_with_racer(&base, Policy::Overwrite, &base.join("keep.txt"));
    assert_eq!(report.written_files, 1);
    assert_eq!(std::fs::read(base.join("keep.txt")).unwrap(), b"NEW");
    assert_eq!(names_in(&base), ["keep.txt"], "no staging or backup left");
}

/// Two extractions of the same member name run at once, both past their exists check before
/// either commits. `a_dest`/`a_member` and `b_dest`/`b_member` must name the same file `sub/f.txt`.
/// The handles the extractor holds on a folder may make one of two overlapping runs fail with a
/// sharing violation (fail closed); whatever succeeds must be intact and nothing may be lost.
#[cfg(windows)]
fn race_two_extractions(
    tag: &str,
    policy: Policy,
    nested: bool,
) -> (u64, Vec<Vec<u8>>, Vec<String>) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let base = TempDir::new(&format!("{tag}-{policy:?}"));
    let sub = base.join("sub");
    std::fs::create_dir(&sub).unwrap();
    // Both decoders inside `stream` before either commits; bounded spin, no sleep.
    let arrived = AtomicUsize::new(0);
    let gate = || {
        arrived.fetch_add(1, Ordering::SeqCst);
        let start = std::time::Instant::now();
        while arrived.load(Ordering::SeqCst) < 2 && start.elapsed().as_secs() < 2 {
            std::thread::yield_now();
        }
    };
    let run = |dest: &Path, member: &'static str, body: &'static [u8]| {
        let plan = preflight(&[file(member, Some(1))], &roomy()).unwrap();
        extract(
            &plan,
            dest,
            &roomy(),
            policy,
            |_, writer| {
                gate();
                writer.write_all(body).map_err(archive_extract::io_error)
            },
            || false,
        )
    };
    let (a, b) = std::thread::scope(|s| {
        let ta = s.spawn(|| {
            if nested {
                run(&base, "sub/f.txt", b"A")
            } else {
                run(&sub, "f.txt", b"A")
            }
        });
        let tb = s.spawn(|| run(&sub, "f.txt", b"B"));
        (ta.join().unwrap(), tb.join().unwrap())
    });
    let mut expected: Vec<Vec<u8>> = Vec::new();
    let mut written = 0u64;
    for (r, body) in [(&a, b"A"), (&b, b"B")] {
        if let Ok(rep) = r {
            written += rep.written_files;
            if rep.written_files == 1 {
                expected.push(body.to_vec());
            }
        }
    }
    expected.sort();
    let names = names_in(&sub);
    let mut contents: Vec<Vec<u8>> = names
        .iter()
        .map(|n| std::fs::read(sub.join(n)).unwrap())
        .collect();
    contents.sort();
    assert_eq!(
        contents, expected,
        "every written file survives, none is lost"
    );
    assert!(
        names.iter().all(|n| !n.contains("ferrum-partial")),
        "no staging left: {names:?}"
    );
    (written, contents, names)
}

#[test]
#[cfg(windows)]
fn parallel_extractions_into_overlapping_folders_never_overwrite_each_other() {
    // The same folder twice, and D:\X with D:\X\sub. The folder handles may make one run fail
    // closed with a sharing violation; the invariant checked inside is that nothing a run wrote is
    // lost or replaced, and no staging file is left.
    for nested in [false, true] {
        for policy in [Policy::Rename, Policy::Skip] {
            let (written, _, names) = race_two_extractions("par", policy, nested);
            assert!(written >= 1, "nested={nested} {policy:?}: {names:?}");
        }
    }
}

#[test]
#[cfg(windows)]
fn long_paths_are_committed_without_replacing() {
    let base = TempDir::new("long");
    // canonicalize gives the verbatim form, so the path may exceed MAX_PATH.
    let mut deep = base.canonicalize().unwrap();
    for i in 0..5 {
        deep.push(format!("{}{i}", "d".repeat(60)));
    }
    std::fs::create_dir_all(&deep).unwrap();
    assert!(deep.to_string_lossy().len() > 300);
    let report = extract_with_racer(&deep, Policy::Skip, &deep.join("keep.txt"));
    assert_eq!(report.skipped_files, 1);
    assert_eq!(std::fs::read(deep.join("keep.txt")).unwrap(), b"RACE");
    let report = extract_with_racer(&deep, Policy::Rename, &deep.join("keep.txt"));
    assert_eq!(report.written_files, 1);
    assert_eq!(std::fs::read(deep.join("keep (2).txt")).unwrap(), b"NEW");
    let report = extract_with_racer(&deep, Policy::Overwrite, &deep.join("keep.txt"));
    assert_eq!(report.written_files, 1);
    assert_eq!(std::fs::read(deep.join("keep.txt")).unwrap(), b"NEW");
    assert_eq!(names_in(&deep), ["keep (2).txt", "keep.txt"]);
}

// ---- FER-584 R1: replace beyond MAX_PATH, OverwriteOlder race, read-only, in-use, folder ----

/// `extract_with_racer` with a timeout: a hang is a failure, not a stuck test run.
#[cfg(windows)]
fn extract_timed(
    dest: PathBuf,
    policy: Policy,
    member_ms: Option<i64>,
    on_decode: impl FnOnce() + Send + 'static,
) -> Result<archive_extract::ExtractReport, archive_contract::ArchiveError> {
    extract_member_timed(dest, "keep.txt".to_string(), policy, member_ms, on_decode)
}

#[cfg(windows)]
fn extract_member_timed(
    dest: PathBuf,
    member: String,
    policy: Policy,
    member_ms: Option<i64>,
    on_decode: impl FnOnce() + Send + 'static,
) -> Result<archive_extract::ExtractReport, archive_contract::ArchiveError> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut entry = file(&member, Some(3));
        entry.modified_ms = member_ms;
        let plan = preflight(&[entry], &roomy()).unwrap();
        let mut hook = Some(on_decode);
        let result = extract(
            &plan,
            &dest,
            &roomy(),
            policy,
            |_, writer| {
                if let Some(h) = hook.take() {
                    h();
                }
                writer.write_all(b"NEW").map_err(archive_extract::io_error)
            },
            || false,
        );
        let _ = tx.send(result);
    });
    rx.recv_timeout(std::time::Duration::from_secs(60))
        .expect("extract did not finish in time (endless loop?)")
}

#[cfg(windows)]
fn set_mtime(path: &Path, time: std::time::SystemTime) {
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(time)
        .unwrap();
}

/// A short, plain destination and a member deep enough that its full path is > MAX_PATH (the
/// extractor walks it handle-relative, so only path-based calls would fail).
#[cfg(windows)]
fn long_member() -> String {
    (0..5)
        .map(|i| format!("{}{i}", "d".repeat(60)))
        .collect::<Vec<_>>()
        .join("/")
        + "/keep.txt"
}

#[test]
#[cfg(windows)]
fn overwrite_beyond_max_path_with_a_plain_destination_finishes() {
    for policy in [Policy::Overwrite, Policy::OverwriteOlder] {
        let base = TempDir::new(&format!("longplain-{policy:?}"));
        let member = long_member();
        let deep = base.join(member.replace('/', "\\"));
        assert!(deep.to_string_lossy().len() > 300);
        // Build the existing file through the verbatim form (plain std paths stop at MAX_PATH).
        let verbatim = base.canonicalize().unwrap().join(member.replace('/', "\\"));
        std::fs::create_dir_all(verbatim.parent().unwrap()).unwrap();
        std::fs::write(&verbatim, b"OLD").unwrap();
        set_mtime(&verbatim, std::time::SystemTime::UNIX_EPOCH);
        let report =
            extract_member_timed(base.to_path_buf(), member, policy, Some(1_000), || {}).unwrap();
        assert_eq!(report.written_files, 1, "{policy:?}");
        assert_eq!(std::fs::read(&verbatim).unwrap(), b"NEW");
        assert_eq!(names_in(verbatim.parent().unwrap()), ["keep.txt"]);
    }
}

#[test]
#[cfg(windows)]
fn overwrite_older_keeps_a_target_updated_during_extraction() {
    let base = TempDir::new("older-race");
    let keep = base.join("keep.txt");
    std::fs::write(&keep, b"OLD").unwrap();
    set_mtime(&keep, std::time::SystemTime::UNIX_EPOCH);
    // The user saves the file (newer than the member) after the pre-check, before the swap.
    let racer = keep.clone();
    let report = extract_timed(
        base.to_path_buf(),
        Policy::OverwriteOlder,
        Some(1_000),
        move || {
            std::fs::write(&racer, b"EDITED").unwrap();
        },
    )
    .unwrap();
    assert_eq!((report.written_files, report.skipped_files), (0, 1));
    assert_eq!(std::fs::read(&keep).unwrap(), b"EDITED");
    assert_eq!(names_in(&base), ["keep.txt"]);
}

#[test]
#[cfg(windows)]
fn overwrite_does_not_replace_a_read_only_file() {
    let base = TempDir::new("readonly");
    let keep = base.join("keep.txt");
    std::fs::write(&keep, b"OLD").unwrap();
    let mut perm = std::fs::metadata(&keep).unwrap().permissions();
    perm.set_readonly(true);
    std::fs::set_permissions(&keep, perm).unwrap();
    let error =
        extract_timed(base.to_path_buf(), Policy::Overwrite, Some(1_000), || {}).unwrap_err();
    assert_eq!(error.code, Code::Conflict);
    assert!(error.message.contains("read-only"), "{}", error.message);
    assert_eq!(std::fs::read(&keep).unwrap(), b"OLD");
    assert_eq!(names_in(&base), ["keep.txt"], "staging removed");
    let mut perm = std::fs::metadata(&keep).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perm.set_readonly(false);
    std::fs::set_permissions(&keep, perm).unwrap();
}

#[test]
#[cfg(windows)]
fn overwrite_of_a_file_in_use_fails_clearly_and_keeps_the_original() {
    use std::os::windows::fs::OpenOptionsExt;
    let base = TempDir::new("inuse");
    let keep = base.join("keep.txt");
    std::fs::write(&keep, b"OLD").unwrap();
    // Open for writing and deny sharing (FILE_SHARE_READ only): the replace cannot take it.
    let _held = std::fs::OpenOptions::new()
        .write(true)
        .share_mode(1)
        .open(&keep)
        .unwrap();
    let error =
        extract_timed(base.to_path_buf(), Policy::Overwrite, Some(1_000), || {}).unwrap_err();
    assert_eq!(error.code, Code::Conflict);
    assert!(error.message.contains("in use"), "{}", error.message);
    drop(_held);
    assert_eq!(std::fs::read(&keep).unwrap(), b"OLD");
    assert_eq!(names_in(&base), ["keep.txt"], "staging removed");
}

#[test]
#[cfg(windows)]
fn a_folder_appearing_at_the_final_name_is_never_replaced() {
    for policy in [Policy::Skip, Policy::Rename, Policy::Overwrite] {
        let base = TempDir::new(&format!("folder-race-{policy:?}"));
        let racer = base.join("keep.txt");
        let hook_path = racer.clone();
        let result = extract_timed(base.to_path_buf(), policy, Some(1_000), move || {
            std::fs::create_dir(&hook_path).unwrap();
            std::fs::write(hook_path.join("inner.txt"), b"KEEP").unwrap();
        });
        assert!(racer.is_dir(), "{policy:?}");
        assert_eq!(std::fs::read(racer.join("inner.txt")).unwrap(), b"KEEP");
        match policy {
            Policy::Skip => assert_eq!(result.unwrap().skipped_files, 1),
            Policy::Rename => {
                assert_eq!(result.unwrap().written_files, 1);
                assert_eq!(std::fs::read(base.join("keep (2).txt")).unwrap(), b"NEW");
            }
            _ => {
                assert_eq!(result.unwrap_err().code, Code::Conflict);
                assert_eq!(names_in(&base), ["keep.txt"], "staging removed");
            }
        }
    }
}
