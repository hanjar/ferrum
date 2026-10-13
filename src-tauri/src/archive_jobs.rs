//! Cancellable, progress-reporting archive jobs (FER-79) and the multipart guard (FER-75).
//!
//! `run_extract` / `run_test` wrap the providers and the Test Archive engine with:
//! - a request id registered in [`crate::archive_cancel`] (the same registry that
//!   `archive_cancel_request` flips), so Batal / Esc in the UI stops the job;
//! - a throttled progress callback (bytes, members, current member) that the IPC layer turns into
//!   `archive://progress` events;
//! - the multipart guard: a non-first volume or a first volume with missing volumes is refused
//!   with a structured code before anything is read or written.
//!
//! Nothing here logs or stores a password; it only travels inside the request the provider reads.

use crate::archive_contract::{
    ArchiveCreateFormat, ArchiveCreateRequest, ArchiveError, ArchiveErrorCode,
    ArchiveExtractRequest, ArchiveOperationKind, ArchiveProgress, ArchiveProvider,
    ArchiveTestReport,
};
use crate::archive_multipart::{MultipartStyle, detect, missing_volumes};
use serde::Serialize;
use std::path::Path;
use std::time::{Duration, Instant};

/// Event the UI listens to for extract / test progress.
pub const PROGRESS_EVENT: &str = "archive://progress";

/// Minimum time between two progress events of one job.
pub const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

/// One throttled progress event. Carries no password and no archive content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveProgressEvent {
    pub request_id: String,
    pub kind: ArchiveOperationKind,
    pub archive_path: String,
    pub processed_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_bytes: Option<u64>,
    pub processed_entries: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_entries: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_member: Option<String>,
    /// FER-55: Number of files successfully written during extraction
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub written_entries: Option<u64>,
    /// FER-55: Number of files skipped during extraction
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped_entries: Option<u64>,
}

/// Lets an event through at most once per `interval`; the very first call always passes.
pub struct Throttle {
    interval: Duration,
    last: Option<Instant>,
}

impl Throttle {
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            last: None,
        }
    }

    pub fn due(&mut self) -> bool {
        let now = Instant::now();
        match self.last {
            Some(prev) if now.saturating_duration_since(prev) < self.interval => false,
            _ => {
                self.last = Some(now);
                true
            }
        }
    }
}

fn to_event(request_id: &str, archive_path: &str, p: ArchiveProgress) -> ArchiveProgressEvent {
    ArchiveProgressEvent {
        request_id: request_id.to_owned(),
        kind: p.kind,
        archive_path: archive_path.to_owned(),
        processed_bytes: p.processed_bytes,
        total_bytes: p.total_bytes,
        processed_entries: p.processed_entries,
        total_entries: p.total_entries,
        current_member: p.current_member_path.map(|m| m.as_str().to_owned()),
        written_entries: p.written_entries,
        skipped_entries: p.skipped_entries,
    }
}

/// Extract with cancel + throttled progress. `request_id` is the client-chosen id that
/// `archive_cancel_request` targets; `None` yields a job nobody can cancel.
///
/// A cancel (also one that arrived before the job started) returns `Cancelled`; the extraction
/// pipeline has by then removed its staging file and any directory it created, so the
/// destination holds only members that were completely written.
pub fn run_extract(
    provider: &dyn ArchiveProvider,
    request: &ArchiveExtractRequest,
    request_id: Option<&str>,
    emit: &mut dyn FnMut(ArchiveProgressEvent),
) -> Result<(), ArchiveError> {
    run_extract_every(provider, request, request_id, PROGRESS_INTERVAL, emit)
}

/// [`run_extract`] with an explicit event interval (tests use zero to see every callback).
fn run_extract_every(
    provider: &dyn ArchiveProvider,
    request: &ArchiveExtractRequest,
    request_id: Option<&str>,
    interval: Duration,
    emit: &mut dyn FnMut(ArchiveProgressEvent),
) -> Result<(), ArchiveError> {
    guard_multipart(Path::new(&request.archive_path))?;
    let registration = begin_job(request_id)?;
    let token = registration.token().clone();
    let id = request_id.unwrap_or_default().to_owned();
    let mut throttle = Throttle::new(interval);
    registration.run(|| {
        crate::archive_contract::check_cancelled()?;
        provider.extract(request, &mut |p| {
            if throttle.due() || p.written_entries.is_some() {
                emit(to_event(&id, &request.archive_path, p));
            }
            // false = cancel.
            !token.is_cancelled()
        })
    })
}

/// Create an archive with cancel + throttled progress (FER-60/FER-67). `request_id` is the
/// client-chosen id that `archive_cancel_request` targets; `None` yields a job nobody can cancel.
///
/// A cancel (also one that arrived before the job started) returns `Cancelled`; the creation
/// pipeline has by then removed its `.ferrum-tmp` staging file and leaves no partial file at the
/// final destination. Under `Overwrite`, an existing destination is left untouched.
pub fn run_create(
    request: &ArchiveCreateRequest,
    request_id: Option<&str>,
    emit: &mut dyn FnMut(ArchiveProgressEvent),
) -> Result<std::path::PathBuf, ArchiveError> {
    run_create_every(request, request_id, PROGRESS_INTERVAL, emit)
}

/// [`run_create`] with an explicit event interval (tests use zero to see every callback).
pub fn run_create_every(
    request: &ArchiveCreateRequest,
    request_id: Option<&str>,
    interval: Duration,
    emit: &mut dyn FnMut(ArchiveProgressEvent),
) -> Result<std::path::PathBuf, ArchiveError> {
    let registration = begin_job(request_id)?;
    let token = registration.token().clone();
    let id = request_id.unwrap_or_default().to_owned();
    let mut throttle = Throttle::new(interval);
    let dest_path = request.destination_path.clone();
    registration.run(|| {
        crate::archive_contract::check_cancelled()?;
        let mut progress = |p: ArchiveProgress| {
            if throttle.due() {
                emit(to_event(&id, &dest_path, p));
            }
            !token.is_cancelled()
        };
        match request.format {
            ArchiveCreateFormat::Zip => crate::archive_create_test::create_zip_progress_with_level(
                &request.source_paths,
                Path::new(&request.destination_path),
                &crate::archive_contract::ArchiveLimits {
                    max_entries: 100_000,
                    max_total_bytes: u64::MAX,
                    max_entry_bytes: 512 * 1024 * 1024,
                    max_depth: 32,
                },
                request.conflict,
                request.level(),
                &mut progress,
            ),
            ArchiveCreateFormat::Sevenz => {
                crate::archive_create_test::create_7z_progress_with_level(
                    &request.source_paths,
                    Path::new(&request.destination_path),
                    request.conflict,
                    request.level(),
                    &mut progress,
                )
            }
            ArchiveCreateFormat::Rar => crate::archive_rar_tool::create_rar_progress_with_level(
                &request.source_paths,
                Path::new(&request.destination_path),
                request.conflict,
                request.level(),
                &mut progress,
            ),
        }
    })
}

/// Test Archive for one archive, with cancel + throttled progress (member + bytes).
pub fn run_test(
    archive_path: &str,
    password: Option<&str>,
    request_id: Option<&str>,
    emit: &mut dyn FnMut(ArchiveProgressEvent),
) -> Result<ArchiveTestReport, ArchiveError> {
    guard_multipart(Path::new(archive_path))?;
    let registration = begin_job(request_id)?;
    let id = request_id.unwrap_or_default().to_owned();
    let mut throttle = Throttle::new(PROGRESS_INTERVAL);
    let path = Path::new(archive_path);
    let lower = archive_path.to_lowercase();
    registration.run(|| {
        let mut progress = |p: ArchiveProgress| {
            if throttle.due() {
                emit(to_event(&id, archive_path, p));
            }
        };
        if lower.ends_with(".zip") {
            crate::archive_create_test::test_zip_progress(path, password, &mut progress)
        } else if lower.ends_with(".7z") || lower.ends_with(".7zip") {
            crate::archive_create_test::test_7z_progress(path, password, &mut progress)
        } else {
            crate::archive_create_test::test_rar_progress(path, password, &mut progress)
        }
    })
}

/// Register a job, refusing a request id another running job still holds (it would otherwise be
/// able to cancel that job, or hide from its own cancel).
fn begin_job(
    request_id: Option<&str>,
) -> Result<crate::archive_cancel::Registration, ArchiveError> {
    crate::archive_cancel::begin_unique(request_id)
        .map_err(|msg| ArchiveError::new(ArchiveErrorCode::Io, msg))
}

// ── FER-75: multipart detection against the folder ───────────────────────

/// What the folder says about an archive file name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum MultipartInfo {
    /// An ordinary single archive.
    Single,
    /// Volume `part` (> 1) of a multipart set; the first volume is `first_name`.
    NotFirst {
        part: u32,
        first_name: String,
        first_path: String,
        /// Whether the first volume exists next to this file.
        first_exists: bool,
    },
    /// The first volume, but volumes between 1 and the highest present one are absent.
    Missing { missing: Vec<String> },
    /// First volume with every volume up to the highest present one in place. Volumes after the
    /// highest present one cannot be known from names alone.
    Complete { volumes: u32 },
}

fn folder_names(folder: &Path) -> Vec<String> {
    std::fs::read_dir(folder)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .take(200_000)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// Classify `archive_path` against the files next to it. Reads one directory listing, only for
/// names that can be volumes (`.partN.rar`, `.rNN`, `.7z.NNN`, or `.rar` which may head a
/// legacy `.r00` set); ordinary names return `Single` without touching the disk.
pub fn inspect_multipart(archive_path: &Path) -> MultipartInfo {
    // Network shares are not scanned: listing a UNC folder would open another SMB connection.
    if archive_path.to_string_lossy().starts_with(r"\\") {
        return MultipartInfo::Single;
    }
    let Some(file_name) = archive_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
    else {
        return MultipartInfo::Single;
    };
    let lower = file_name.to_ascii_lowercase();
    let detected = detect(&file_name);
    if detected.is_none() && !lower.ends_with(".rar") {
        return MultipartInfo::Single;
    }
    let folder = archive_path.parent().unwrap_or_else(|| Path::new("."));
    let names = folder_names(folder);
    let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();

    // `stem.rar` is volume 1 of a legacy set only when `stem.r00` sits next to it.
    let (info, seed): (_, String) = match detected {
        Some(info) => (info, file_name.clone()),
        None => {
            let stem = &file_name[..file_name.len() - ".rar".len()];
            let sibling = names.iter().find(|n| {
                detect(n).is_some_and(|d| {
                    d.style == MultipartStyle::RarLegacy && d.stem.eq_ignore_ascii_case(stem)
                })
            });
            match sibling {
                Some(sib) => {
                    let mut d = detect(sib).expect("sibling detected above");
                    d.index = 1;
                    (d, sib.clone())
                }
                None => return MultipartInfo::Single,
            }
        }
    };

    if !info.is_first() {
        let first_name = info.first_name();
        let first_exists = names.iter().any(|n| n.eq_ignore_ascii_case(&first_name));
        return MultipartInfo::NotFirst {
            part: info.index,
            first_path: folder.join(&first_name).to_string_lossy().into_owned(),
            first_name,
            first_exists,
        };
    }
    let missing = missing_volumes(&seed, &name_refs);
    if !missing.is_empty() {
        return MultipartInfo::Missing { missing };
    }
    let volumes = names
        .iter()
        .filter_map(|n| detect(n))
        .filter(|d| d.style == info.style && d.stem.eq_ignore_ascii_case(&info.stem))
        .map(|d| d.index)
        .max()
        .unwrap_or(1)
        .max(1);
    MultipartInfo::Complete { volumes }
}

fn list_names(names: &[String]) -> String {
    const SHOWN: usize = 5;
    let mut out = names
        .iter()
        .take(SHOWN)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if names.len() > SHOWN {
        out.push_str(&format!(" dan {} lainnya", names.len() - SHOWN));
    }
    out
}

/// Refuse a volume that cannot be handled alone. `Single` and `Complete` pass: the format
/// backends then decide (RAR rejects entries split across volumes; see docs/archive-support.md).
pub fn guard_multipart(archive_path: &Path) -> Result<(), ArchiveError> {
    let path_text = archive_path.to_string_lossy().into_owned();
    // Device, pipe and odd paths are refused before anything is listed.
    crate::path_guard::check_str(&path_text).map_err(|e| {
        let mut err = ArchiveError::new(ArchiveErrorCode::Io, e.message);
        err.archive_path = Some(path_text.clone());
        err
    })?;
    match inspect_multipart(archive_path) {
        MultipartInfo::Single | MultipartInfo::Complete { .. } => Ok(()),
        MultipartInfo::NotFirst {
            part, first_name, ..
        } => {
            let mut e = ArchiveError::new(
                ArchiveErrorCode::MultipartNotFirst,
                format!(
                    "Ini bagian {part} dari arsip multipart. Buka bagian pertama: {first_name}"
                ),
            );
            e.archive_path = Some(path_text);
            Err(e)
        }
        MultipartInfo::Missing { missing } => {
            let mut e = ArchiveError::new(
                ArchiveErrorCode::MultipartMissing,
                format!(
                    "Bagian arsip multipart hilang: {}. Ekstrak dan tes ditolak supaya tidak ada hasil sebagian.",
                    list_names(&missing)
                ),
            );
            e.archive_path = Some(path_text);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let d =
            std::env::temp_dir().join(format!("ferrum-jobs-{tag}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn touch(d: &Path, names: &[&str]) {
        for n in names {
            fs::write(d.join(n), b"x").unwrap();
        }
    }

    #[test]
    fn throttle_lets_the_first_event_through_then_waits() {
        let mut t = Throttle::new(Duration::from_secs(3600));
        assert!(t.due());
        assert!(!t.due());
        let mut zero = Throttle::new(Duration::ZERO);
        assert!(zero.due());
        assert!(zero.due());
    }

    #[test]
    fn plain_archive_is_single_and_never_lists_the_folder() {
        let d = dir("single");
        // The file does not even exist: a non-volume name must not touch the disk.
        assert_eq!(
            inspect_multipart(&d.join("photos.7z")),
            MultipartInfo::Single
        );
        assert_eq!(
            inspect_multipart(&d.join("photos.zip")),
            MultipartInfo::Single
        );
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn second_part_points_at_the_first_part() {
        let d = dir("notfirst");
        touch(&d, &["a.part1.rar", "a.part2.rar", "a.part3.rar"]);
        match inspect_multipart(&d.join("a.part2.rar")) {
            MultipartInfo::NotFirst {
                part,
                first_name,
                first_exists,
                first_path,
            } => {
                assert_eq!(part, 2);
                assert_eq!(first_name, "a.part1.rar");
                assert!(first_exists);
                assert!(first_path.ends_with("a.part1.rar"));
            }
            other => panic!("expected NotFirst, got {other:?}"),
        }
        let e = guard_multipart(&d.join("a.part2.rar")).unwrap_err();
        assert_eq!(e.code, ArchiveErrorCode::MultipartNotFirst);
        assert!(e.message.contains("bagian 2"), "{}", e.message);
        assert!(e.message.contains("a.part1.rar"), "{}", e.message);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn non_first_part_whose_first_part_is_absent_says_so() {
        let d = dir("nofirst");
        touch(&d, &["b.7z.002"]);
        match inspect_multipart(&d.join("b.7z.002")) {
            MultipartInfo::NotFirst {
                first_name,
                first_exists,
                ..
            } => {
                assert_eq!(first_name, "b.7z.001");
                assert!(!first_exists);
            }
            other => panic!("expected NotFirst, got {other:?}"),
        }
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn first_part_with_a_gap_reports_the_missing_volumes_and_is_refused() {
        let d = dir("missing");
        touch(&d, &["c.part1.rar", "c.part3.rar"]);
        match inspect_multipart(&d.join("c.part1.rar")) {
            MultipartInfo::Missing { missing } => assert_eq!(missing, vec!["c.part2.rar"]),
            other => panic!("expected Missing, got {other:?}"),
        }
        let e = guard_multipart(&d.join("c.part1.rar")).unwrap_err();
        assert_eq!(e.code, ArchiveErrorCode::MultipartMissing);
        assert!(e.message.contains("c.part2.rar"), "{}", e.message);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn legacy_rar_head_with_a_gap_is_recognised_through_its_r_volumes() {
        let d = dir("legacy");
        touch(&d, &["d.rar", "d.r00", "d.r02"]);
        match inspect_multipart(&d.join("d.rar")) {
            MultipartInfo::Missing { missing } => assert_eq!(missing, vec!["d.r01"]),
            other => panic!("expected Missing, got {other:?}"),
        }
        // A lone .rar with no .rNN neighbour is just an archive.
        let lone = dir("lone");
        touch(&lone, &["e.rar"]);
        assert_eq!(
            inspect_multipart(&lone.join("e.rar")),
            MultipartInfo::Single
        );
        let _ = fs::remove_dir_all(&d);
        let _ = fs::remove_dir_all(&lone);
    }

    #[test]
    fn complete_set_passes_the_guard() {
        let d = dir("complete");
        touch(&d, &["f.7z.001", "f.7z.002", "f.7z.003"]);
        assert_eq!(
            inspect_multipart(&d.join("f.7z.001")),
            MultipartInfo::Complete { volumes: 3 }
        );
        assert!(guard_multipart(&d.join("f.7z.001")).is_ok());
        let _ = fs::remove_dir_all(&d);
    }

    // ── FER-79: cancel ───────────────────────────────────────────────────

    use crate::archive_contract::{ArchiveConflictPolicy, ArchiveLimits, MemberPath};
    use crate::archive_create_test::{
        create_7z, create_7z_progress, create_zip, create_zip_progress, test_7z_progress,
        test_zip_progress,
    };

    /// Incompressible-ish bytes (xorshift) so the archive really is large and decode takes
    /// many 64 KiB chunks.
    fn noise(len: usize) -> Vec<u8> {
        let mut x = 0x9E37_79B9_7F4A_7C15u64;
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x >> 24) as u8
            })
            .collect()
    }

    fn limits() -> ArchiveLimits {
        ArchiveLimits {
            max_entries: 1000,
            max_total_bytes: u64::MAX,
            max_entry_bytes: 64 * 1024 * 1024,
            max_depth: 8,
        }
    }

    /// `<dir>/src/{a-small.txt, b-big.bin}`; returns (dir, source folder as String).
    fn tree(tag: &str, big: usize) -> (PathBuf, String) {
        let d = dir(tag);
        let src = d.join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("a-small.txt"), b"small member").unwrap();
        fs::write(src.join("b-big.bin"), noise(big)).unwrap();
        (d, src.to_string_lossy().into_owned())
    }

    fn leftovers(root: &Path) -> Vec<String> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(p) = stack.pop() {
            for e in fs::read_dir(&p).into_iter().flatten().flatten() {
                let path = e.path();
                if path.is_dir() {
                    stack.push(path.clone());
                }
                out.push(
                    path.strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
        out.sort();
        out
    }

    #[test]
    fn cancel_in_the_middle_of_test_7z_stops_within_a_few_chunks() {
        let (d, src) = tree("c7z", 6 * 1024 * 1024);
        let archive = d.join("out.7z");
        create_7z(&[src], &archive).unwrap();

        let token = crate::archive_contract::CancelToken::new();
        let mut last_bytes = 0u64;
        let mut total = None;
        let result = crate::archive_contract::with_cancel(&token, || {
            test_7z_progress(&archive, None, &mut |p| {
                last_bytes = p.processed_bytes;
                total = p.total_bytes;
                if p.processed_bytes >= 256 * 1024 {
                    token.cancel();
                }
            })
        });
        let e = result.unwrap_err();
        assert_eq!(e.code, ArchiveErrorCode::Cancelled);
        let total = total.expect("7z reports a total");
        assert!(total >= 6 * 1024 * 1024);
        assert!(
            last_bytes < 1024 * 1024,
            "decoded {last_bytes} of {total} bytes: the cancel was not honoured per chunk"
        );
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn cancel_in_the_middle_of_test_zip_stops_within_a_few_chunks() {
        let (d, src) = tree("czip", 6 * 1024 * 1024);
        let archive = d.join("out.zip");
        create_zip(&[src], &archive, &limits()).unwrap();

        let token = crate::archive_contract::CancelToken::new();
        let mut last_bytes = 0u64;
        let result = crate::archive_contract::with_cancel(&token, || {
            test_zip_progress(&archive, None, &mut |p| {
                last_bytes = p.processed_bytes;
                if p.processed_bytes >= 256 * 1024 {
                    token.cancel();
                }
            })
        });
        assert_eq!(result.unwrap_err().code, ArchiveErrorCode::Cancelled);
        assert!(
            last_bytes < 1024 * 1024,
            "decoded {last_bytes} bytes after the cancel"
        );
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_test_cancelled_before_it_starts_reads_nothing() {
        let (d, src) = tree("cearly", 1024 * 1024);
        let archive = d.join("out.zip");
        create_zip(&[src], &archive, &limits()).unwrap();
        let path = archive.to_string_lossy().into_owned();

        // The cancel reached the registry before the job began (blocking pool was busy).
        crate::archive_cancel::cancel("jobs-test-early");
        let mut events = 0;
        let e = run_test(&path, None, Some("jobs-test-early"), &mut |_| events += 1).unwrap_err();
        assert_eq!(e.code, ArchiveErrorCode::Cancelled);
        assert_eq!(
            events, 0,
            "no progress after a cancel that was already pending"
        );
        let _ = fs::remove_dir_all(&d);
    }

    fn extract_request(archive: &Path, dest: &Path) -> ArchiveExtractRequest {
        ArchiveExtractRequest {
            archive_path: archive.to_string_lossy().into_owned(),
            member_paths: Vec::<MemberPath>::new(),
            destination_path: dest.to_string_lossy().into_owned(),
            conflict: ArchiveConflictPolicy::Skip,
            limits: limits(),
            password: None,
        }
    }

    /// Cancels from the progress event of the big member and checks the destination: no
    /// `.ferrum-partial-*` staging file, no half-written big member, and what is there is whole.
    fn assert_cancelled_extract_leaves_no_partial_files(archive: &Path, d: &Path, tag: &str) {
        let dest = d.join(format!("dest-{tag}"));
        fs::create_dir_all(&dest).unwrap();
        let provider: Box<dyn ArchiveProvider> = if tag == "7z" {
            Box::new(crate::archive_sevenz::SevenZProvider::new())
        } else {
            Box::new(crate::archive_zip::ZipProvider::new())
        };
        let id = format!("jobs-test-extract-{tag}");
        let mut saw_progress = false;
        let result = run_extract_every(
            provider.as_ref(),
            &extract_request(archive, &dest),
            Some(&id),
            Duration::ZERO,
            &mut |ev| {
                saw_progress = true;
                if ev.processed_bytes >= 128 * 1024 {
                    crate::archive_cancel::cancel(&id);
                }
            },
        );
        let e = result.unwrap_err();
        assert_eq!(e.code, ArchiveErrorCode::Cancelled, "{}", e.message);
        assert!(saw_progress);
        let left = leftovers(&dest);
        assert!(
            left.iter().all(|n| !n.contains("ferrum-partial")),
            "staging file left behind: {left:?}"
        );
        assert!(
            left.iter().all(|n| !n.ends_with("b-big.bin")),
            "half-written big member left behind: {left:?}"
        );
        for name in &left {
            let full = dest.join(name);
            if full.is_file() {
                assert_eq!(fs::read(&full).unwrap(), b"small member", "{name} is whole");
            }
        }
    }

    #[test]
    fn a_cancelled_zip_extract_leaves_no_partial_file() {
        let (d, src) = tree("xzip", 4 * 1024 * 1024);
        let archive = d.join("out.zip");
        create_zip(&[src], &archive, &limits()).unwrap();
        assert_cancelled_extract_leaves_no_partial_files(&archive, &d, "zip");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_cancelled_7z_extract_leaves_no_partial_file() {
        let (d, src) = tree("x7z", 4 * 1024 * 1024);
        let archive = d.join("out.7z");
        create_7z(&[src], &archive).unwrap();
        assert_cancelled_extract_leaves_no_partial_files(&archive, &d, "7z");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn cancel_in_the_middle_of_create_zip_stops_within_a_few_chunks() {
        let (d, src) = tree("czip", 6 * 1024 * 1024);
        let archive = d.join("out.zip");
        let token = crate::archive_contract::CancelToken::new();
        let mut last_bytes = 0u64;
        let mut total = None;
        let result = crate::archive_contract::with_cancel(&token, || {
            create_zip_progress(
                &[src],
                &archive,
                &limits(),
                ArchiveConflictPolicy::Overwrite,
                &mut |p| {
                    last_bytes = p.processed_bytes;
                    total = p.total_bytes;
                    if p.processed_bytes >= 256 * 1024 {
                        token.cancel();
                    }
                    !token.is_cancelled()
                },
            )
        });
        let e = result.unwrap_err();
        assert_eq!(e.code, ArchiveErrorCode::Cancelled);
        let total = total.expect("zip create reports a total");
        assert!(total >= 6 * 1024 * 1024);
        assert!(
            last_bytes < 1024 * 1024,
            "processed {last_bytes} of {total} bytes: the cancel was not honoured per chunk"
        );
        assert!(!archive.exists(), "final archive must not exist on cancel");
        let left = leftovers(&d);
        assert!(
            left.iter().all(|n| !n.contains(".ferrum-tmp")),
            "temporary staging file left behind: {left:?}"
        );
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn cancel_in_the_middle_of_create_7z_stops_within_a_few_chunks() {
        let (d, src) = tree("c7z", 6 * 1024 * 1024);
        let archive = d.join("out.7z");
        let token = crate::archive_contract::CancelToken::new();
        let mut last_bytes = 0u64;
        let mut total = None;
        let result = crate::archive_contract::with_cancel(&token, || {
            create_7z_progress(
                &[src],
                &archive,
                ArchiveConflictPolicy::Overwrite,
                &mut |p| {
                    last_bytes = p.processed_bytes;
                    total = p.total_bytes;
                    if p.processed_bytes >= 256 * 1024 {
                        token.cancel();
                    }
                    !token.is_cancelled()
                },
            )
        });
        let e = result.unwrap_err();
        assert_eq!(e.code, ArchiveErrorCode::Cancelled);
        let total = total.expect("7z create reports a total");
        assert!(total >= 6 * 1024 * 1024);
        assert!(
            last_bytes < 1024 * 1024,
            "processed {last_bytes} of {total} bytes: the cancel was not honoured per chunk"
        );
        assert!(!archive.exists(), "final archive must not exist on cancel");
        let left = leftovers(&d);
        assert!(
            left.iter().all(|n| !n.contains(".ferrum-tmp")),
            "temporary staging file left behind: {left:?}"
        );
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_create_cancelled_before_it_starts_writes_nothing() {
        let (d, src) = tree("cearly", 1024 * 1024);
        let archive = d.join("out.zip");
        let req = ArchiveCreateRequest {
            format: ArchiveCreateFormat::Zip,
            source_paths: vec![src],
            destination_path: archive.to_string_lossy().into_owned(),
            conflict: ArchiveConflictPolicy::Overwrite,
            level: None,
        };
        crate::archive_cancel::cancel("jobs-test-create-early");
        let mut events = 0;
        let e = run_create(&req, Some("jobs-test-create-early"), &mut |_| events += 1).unwrap_err();
        assert_eq!(e.code, ArchiveErrorCode::Cancelled);
        assert_eq!(
            events, 0,
            "no progress after a cancel that was already pending"
        );
        assert!(!archive.exists(), "final archive must not exist");
        let left = leftovers(&d);
        assert!(
            left.iter().all(|n| !n.contains(".ferrum-tmp")),
            "temporary staging file left behind: {left:?}"
        );
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_cancelled_create_with_overwrite_leaves_existing_destination_intact() {
        for format in [ArchiveCreateFormat::Zip, ArchiveCreateFormat::Sevenz] {
            let ext = match format {
                ArchiveCreateFormat::Zip => "zip",
                ArchiveCreateFormat::Sevenz => "7z",
                ArchiveCreateFormat::Rar => "rar",
            };
            let (d, src) = tree(&format!("crepl-{ext}"), 6 * 1024 * 1024);
            let archive = d.join(format!("existing.{ext}"));
            let original_data = b"original intact destination file content";
            fs::write(&archive, original_data).unwrap();

            let req = ArchiveCreateRequest {
                format,
                source_paths: vec![src],
                destination_path: archive.to_string_lossy().into_owned(),
                conflict: ArchiveConflictPolicy::Overwrite,
                level: None,
            };
            let id = format!("jobs-test-create-replace-{ext}");
            let res = run_create_every(&req, Some(&id), Duration::ZERO, &mut |ev| {
                if ev.processed_bytes >= 256 * 1024 {
                    crate::archive_cancel::cancel(&id);
                }
            });
            let e = res.unwrap_err();
            assert_eq!(e.code, ArchiveErrorCode::Cancelled);
            assert!(
                archive.exists(),
                "existing destination file must still exist"
            );
            assert_eq!(
                fs::read(&archive).unwrap(),
                original_data,
                "existing destination file content must remain completely intact"
            );
            let left = leftovers(&d);
            assert!(
                left.iter().all(|n| !n.contains(".ferrum-tmp")),
                "no temporary staging file left behind: {left:?}"
            );
            let _ = fs::remove_dir_all(&d);
        }
    }

    #[test]
    fn create_progress_reports_monotonically_increasing_bytes_and_entries() {
        for format in [ArchiveCreateFormat::Zip, ArchiveCreateFormat::Sevenz] {
            let ext = match format {
                ArchiveCreateFormat::Zip => "zip",
                ArchiveCreateFormat::Sevenz => "7z",
                ArchiveCreateFormat::Rar => "rar",
            };
            let (d, src) = tree(&format!("cprog-{ext}"), 512 * 1024);
            let archive = d.join(format!("out.{ext}"));
            let req = ArchiveCreateRequest {
                format,
                source_paths: vec![src],
                destination_path: archive.to_string_lossy().into_owned(),
                conflict: ArchiveConflictPolicy::Overwrite,
                level: None,
            };
            let mut events = Vec::new();
            let written = run_create_every(
                &req,
                Some("jobs-test-create-prog"),
                Duration::ZERO,
                &mut |ev| {
                    events.push(ev);
                },
            )
            .unwrap();
            assert_eq!(written, archive);
            assert!(!events.is_empty(), "must emit progress events");

            let mut prev_bytes = 0u64;
            let mut prev_entries = 0u64;
            for ev in &events {
                assert_eq!(ev.kind, ArchiveOperationKind::Create);
                assert_eq!(ev.request_id, "jobs-test-create-prog");
                assert!(
                    ev.processed_bytes >= prev_bytes,
                    "bytes must be monotonically non-decreasing"
                );
                assert!(
                    ev.processed_entries >= prev_entries,
                    "entries must be monotonically non-decreasing"
                );
                prev_bytes = ev.processed_bytes;
                prev_entries = ev.processed_entries;
                assert!(ev.total_bytes.is_some());
                assert!(ev.total_entries.is_some());
            }
            let last = events.last().unwrap();
            assert_eq!(last.processed_entries, last.total_entries.unwrap());
            assert_eq!(last.processed_bytes, last.total_bytes.unwrap());
            assert!(archive.exists());
            let _ = fs::remove_dir_all(&d);
        }
    }

    #[test]
    fn extract_progress_reports_bytes_and_the_current_member() {
        let (d, src) = tree("xprog", 512 * 1024);
        let archive = d.join("out.zip");
        create_zip(&[src], &archive, &limits()).unwrap();
        let dest = d.join("dest");
        fs::create_dir_all(&dest).unwrap();
        let mut last: Option<ArchiveProgressEvent> = None;
        let mut first: Option<ArchiveProgressEvent> = None;
        // Zero interval is not exposed; the first event always passes, the last is the final emit.
        run_extract(
            &crate::archive_zip::ZipProvider::new(),
            &extract_request(&archive, &dest),
            Some("jobs-test-progress"),
            &mut |ev| {
                if first.is_none() {
                    first = Some(ev.clone());
                }
                last = Some(ev);
            },
        )
        .unwrap();
        let ev = first.expect("at least one throttled event");
        assert_eq!(ev.request_id, "jobs-test-progress");
        assert!(ev.total_bytes.is_some());
        assert!(ev.processed_entries <= ev.total_entries.unwrap());
        assert!(last.is_some());
        let _ = fs::remove_dir_all(&d);
    }

    // ── FER-73: wrong password and a damaged archive are different codes ──

    const SPIKE: &str = "../contract-fixtures/archive-spike";
    const FIXTURE_PASSWORD: &str = "fixture-only-not-a-secret";

    fn code_of(report: Result<ArchiveTestReport, ArchiveError>) -> Option<ArchiveErrorCode> {
        match report {
            Err(e) => Some(e.code),
            Ok(r) => r
                .entries
                .iter()
                .find_map(|en| en.error.as_ref().map(|er| er.code)),
        }
    }

    #[test]
    fn test_7z_tells_missing_password_wrong_password_and_damage_apart() {
        let enc = Path::new(SPIKE).join("encrypted.7z");
        assert_eq!(
            code_of(crate::archive_create_test::test_7z(&enc, None)),
            Some(ArchiveErrorCode::PasswordRequired)
        );
        assert_eq!(
            code_of(crate::archive_create_test::test_7z(&enc, Some("wrong"))),
            Some(ArchiveErrorCode::BadPassword)
        );
        assert_eq!(
            code_of(crate::archive_create_test::test_7z(
                &enc,
                Some(FIXTURE_PASSWORD)
            )),
            None,
            "right password passes"
        );
        // Damage with no password involved must not look like a password problem.
        let damaged = Path::new(SPIKE).join("damaged-payload.7z");
        let code = code_of(crate::archive_create_test::test_7z(&damaged, None));
        assert_eq!(code, Some(ArchiveErrorCode::CorruptArchive));
    }

    #[test]
    fn test_zip_tells_missing_password_wrong_password_and_damage_apart() {
        let enc = Path::new(SPIKE).join("encrypted.zip");
        assert_eq!(
            code_of(crate::archive_create_test::test_zip(&enc, None)),
            Some(ArchiveErrorCode::PasswordRequired)
        );
        assert_eq!(
            code_of(crate::archive_create_test::test_zip(&enc, Some("wrong"))),
            Some(ArchiveErrorCode::BadPassword)
        );
        let damaged = Path::new(SPIKE).join("damaged-payload.zip");
        assert_eq!(
            code_of(crate::archive_create_test::test_zip(&damaged, None)),
            Some(ArchiveErrorCode::CorruptArchive)
        );
    }

    #[test]
    fn the_ipc_error_carries_the_structured_code() {
        let e = ArchiveError::new(ArchiveErrorCode::BadPassword, "wrong archive password");
        let fs_err = crate::ipc::archive_error_for_test("x.zip", e);
        assert_eq!(fs_err.archive_code, Some(ArchiveErrorCode::BadPassword));
        let json = serde_json::to_value(&fs_err).unwrap();
        assert_eq!(json["archiveCode"], "badPassword");
        let c = crate::ipc::archive_error_for_test(
            "x.zip",
            ArchiveError::new(ArchiveErrorCode::Cancelled, "x"),
        );
        assert_eq!(c.code, crate::domain::ErrorCode::Cancelled);
    }

    // ── review follow-ups: id reuse, device paths, network shares ────────

    #[test]
    fn a_job_cannot_start_with_an_id_a_running_job_holds() {
        let (d, src) = tree("dupid", 64 * 1024);
        let archive = d.join("out.zip");
        create_zip(&[src], &archive, &limits()).unwrap();
        let path = archive.to_string_lossy().into_owned();
        let running = crate::archive_cancel::begin_unique(Some("jobs-test-dup")).unwrap();
        let e = run_test(&path, None, Some("jobs-test-dup"), &mut |_| {}).unwrap_err();
        assert!(e.message.contains("already in use"), "{}", e.message);
        // The refused call did not disturb the running job's registration.
        crate::archive_cancel::cancel("jobs-test-dup");
        assert!(running.token().is_cancelled());
        drop(running);
        assert!(run_test(&path, None, Some("jobs-test-dup"), &mut |_| {}).is_ok());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn device_pipe_and_odd_paths_are_refused_before_any_listing() {
        for bad in [
            r"\\.\pipe\x.part2.rar",
            r"\\?\C:\x.part2.rar",
            r"\??\C:\x.part2.rar",
            r"\\srv\pipe\x.part2.rar",
            r"\\srv\IPC$\x.part2.rar",
            r"\\srv\mailslot\x.part2.rar",
            r"C:\dir\x.part2.rar:stream",
        ] {
            let e = guard_multipart(Path::new(bad)).unwrap_err();
            assert_eq!(e.code, ArchiveErrorCode::Io, "{bad}");
        }
    }

    #[test]
    fn a_unc_share_is_not_scanned_for_volumes() {
        // Would be NotFirst for a local path; on a share the folder is never listed.
        assert_eq!(
            inspect_multipart(Path::new(r"\\server\share\x.part2.rar")),
            MultipartInfo::Single
        );
        assert!(guard_multipart(Path::new(r"\\server\share\x.part2.rar")).is_ok());
    }
}
