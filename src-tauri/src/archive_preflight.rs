//! FER-368: read-only preflight for the one-ask extraction conflict dialog
//! (D21). Enumerates the members an extract WOULD write and reports which of
//! them already exist at the destination, so the UI can ask ONCE per batch
//! with apply-to-all semantics instead of silently running the `skip` policy.
//!
//! Safety posture: this module never writes or deletes anything. The member
//! enumeration goes through `ArchiveProvider::list`, whose entries are
//! `MemberPath`-validated (no traversal, no absolute paths, no reserved
//! names), so joining them onto the destination is structurally safe. The
//! authoritative per-entry conflict resolution still happens inside the F2-03
//! extract pipeline (`archive_extract::extract`) — this report is advisory
//! for the dialog only.

use crate::archive_contract::{
    ArchiveEntry, ArchiveError, ArchiveErrorCode, ArchiveLimits, ArchiveListRequest, Location,
    MemberPath,
};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// One member that already exists at the destination.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreflightConflict {
    pub member_path: String,
    pub destination_path: String,
}

/// Batch report for the one-ask dialog: totals across the selected members
/// plus the conflicts the chosen policy will resolve.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreflightReport {
    pub total_files: u64,
    pub total_bytes: u64,
    pub conflicts: Vec<PreflightConflict>,
}

/// Same selection semantics as the providers' extract: empty selects every
/// member; otherwise exact paths or whole subtrees.
fn selected(raw: &str, member_paths: &[MemberPath]) -> bool {
    member_paths.is_empty()
        || member_paths
            .iter()
            .any(|mp| raw == mp.as_str() || raw.starts_with(&format!("{}/", mp.as_str())))
}

/// Whether a directory can still contain selected members (prunes the walk).
fn subtree_relevant(raw: &str, member_paths: &[MemberPath]) -> bool {
    member_paths.is_empty()
        || member_paths.iter().any(|mp| {
            mp.as_str() == raw
                || mp.as_str().starts_with(&format!("{raw}/"))
                || raw.starts_with(&format!("{}/", mp.as_str()))
        })
}

/// Pure helper: which of `relative_files` already exist under `destination`?
/// Existence is a non-destructive metadata check; platform case rules apply
/// naturally (Windows lookups are case-insensitive). Symlinks at the
/// destination count as existing — the extract pipeline refuses to overwrite
/// through them regardless of what this report says.
pub fn detect_conflicts(destination: &Path, relative_files: &[String]) -> Vec<PreflightConflict> {
    relative_files
        .iter()
        .filter(|rel| std::fs::symlink_metadata(destination.join(rel)).is_ok())
        .map(|rel| PreflightConflict {
            member_path: rel.clone(),
            destination_path: destination.join(rel).to_string_lossy().into_owned(),
        })
        .collect()
}

/// Enumerate the selected members of `archive_path` through `provider.list`
/// (recursive, bounded by `limits.max_entries` total listing rows) and report
/// destination collisions. Listing errors propagate typed: `PasswordRequired`
/// lets the UI run its password-retry flow before the conflict ask.
///
/// FER-504: `password` (the one the user already typed for this archive)
/// travels in every `ArchiveListRequest`, so header-encrypted RAR/7z are
/// enumerated like any other archive. Absent, the listing of such an archive
/// answers `PasswordRequired` and the UI keeps its retry flow.
pub fn preflight_extract(
    provider: &dyn crate::archive_contract::ArchiveProvider,
    archive_path: &str,
    member_paths: &[MemberPath],
    destination: &Path,
    limits: &ArchiveLimits,
    password: Option<&str>,
) -> Result<PreflightReport, ArchiveError> {
    let mut report = PreflightReport::default();
    let mut listed: usize = 0usize;
    let mut files: Vec<String> = Vec::new();
    let mut queue: Vec<MemberPath> = vec![MemberPath::parse("")?];

    while let Some(dir) = queue.pop() {
        let request = ArchiveListRequest {
            location: Location::Archive {
                archive_path: archive_path.to_owned(),
                member_path: dir,
            },
            request_token: 0,
            password: password.map(str::to_owned),
        };
        for entry in provider.list(&request)?.entries {
            listed += 1;
            if listed > limits.max_entries as usize {
                return Err(ArchiveError::new(
                    ArchiveErrorCode::LimitExceeded,
                    "Archive entry count limit exceeded",
                ));
            }
            let ArchiveEntry {
                member_path,
                is_dir,
                size,
                ..
            } = entry;
            let raw = member_path.as_str().to_owned();
            if is_dir {
                if subtree_relevant(&raw, member_paths) {
                    queue.push(member_path);
                }
            } else if selected(&raw, member_paths) {
                report.total_files += 1;
                report.total_bytes = report.total_bytes.saturating_add(size);
                files.push(raw);
            }
        }
    }

    report.conflicts = detect_conflicts(destination, &files);
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive_zip::ZipProvider;
    use std::fs;
    use std::path::PathBuf;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ferrum-archive-preflight-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn limits() -> ArchiveLimits {
        ArchiveLimits {
            max_entries: 10_000,
            max_total_bytes: u64::MAX,
            max_entry_bytes: 512 * 1024 * 1024,
            max_depth: 32,
        }
    }

    #[test]
    fn detect_conflicts_reports_only_existing_destinations() {
        let dir = temp_dir("detect");
        fs::write(dir.join("a.txt"), b"old").unwrap();
        let conflicts = detect_conflicts(&dir, &["a.txt".into(), "b.txt".into()]);
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].member_path, "a.txt");
        assert!(conflicts[0].destination_path.ends_with("a.txt"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn preflight_lists_all_members_and_finds_destination_collisions() {
        let base = temp_dir("zip");
        let src = base.join("Sample");
        fs::create_dir_all(src.join("sub")).unwrap();
        fs::write(src.join("alpha.txt"), b"alpha").unwrap();
        fs::write(src.join("sub").join("inner.txt"), b"inner").unwrap();
        let archive = base.join("sample.zip");
        crate::archive_create_test::create_zip(
            &[src.to_string_lossy().into_owned()],
            &archive,
            &limits(),
        )
        .unwrap();

        let dest = base.join("out");
        fs::create_dir_all(dest.join("Sample")).unwrap();
        fs::write(dest.join("Sample").join("alpha.txt"), b"existing").unwrap();

        let report = preflight_extract(
            &ZipProvider::new(),
            archive.to_string_lossy().as_ref(),
            &[],
            &dest,
            &limits(),
            None,
        )
        .unwrap();
        assert_eq!(report.total_files, 2);
        assert_eq!(report.total_bytes, 10);
        assert_eq!(report.conflicts.len(), 1);
        assert_eq!(report.conflicts[0].member_path, "Sample/alpha.txt");
        assert!(report.conflicts[0].destination_path.contains("Sample"));

        // Subtree selection: only the `sub` members are planned.
        let scoped = preflight_extract(
            &ZipProvider::new(),
            archive.to_string_lossy().as_ref(),
            &[MemberPath::parse("Sample/sub").unwrap()],
            &dest,
            &limits(),
            None,
        )
        .unwrap();
        assert_eq!(scoped.total_files, 1);
        assert!(scoped.conflicts.is_empty());

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn preflight_lists_header_encrypted_archive_with_password() {
        // FER-504: the committed header-encrypted 7z fixture (demo password,
        // never a real secret — see docs/archive-support.md).
        let archive = PathBuf::from("../contract-fixtures/archive-spike/encrypted.7z");
        assert!(archive.exists(), "fixture missing: {}", archive.display());
        let dest = temp_dir("encrypted-pw");

        // Without a password the listing answers PasswordRequired (the UI's
        // retry-flow trigger, unchanged).
        let err = preflight_extract(
            &crate::archive_sevenz::SevenZProvider::new(),
            archive.to_string_lossy().as_ref(),
            &[],
            &dest,
            &limits(),
            None,
        )
        .unwrap_err();
        assert_eq!(err.code, ArchiveErrorCode::PasswordRequired);

        // With the password the members enumerate like any other archive.
        let report = preflight_extract(
            &crate::archive_sevenz::SevenZProvider::new(),
            archive.to_string_lossy().as_ref(),
            &[],
            &dest,
            &limits(),
            Some("fixture-only-not-a-secret"),
        )
        .unwrap();
        assert!(report.total_files > 0);
        assert!(report.conflicts.is_empty());

        let _ = fs::remove_dir_all(&dest);
    }

    #[test]
    fn preflight_honors_the_entry_limit() {
        let base = temp_dir("limit");
        let src = base.join("Many");
        fs::create_dir_all(&src).unwrap();
        for i in 0..4 {
            fs::write(src.join(format!("f{i}.txt")), b"x").unwrap();
        }
        let archive = base.join("many.zip");
        crate::archive_create_test::create_zip(
            &[src.to_string_lossy().into_owned()],
            &archive,
            &limits(),
        )
        .unwrap();

        let tight = ArchiveLimits {
            max_entries: 2,
            ..limits()
        };
        let err = preflight_extract(
            &ZipProvider::new(),
            archive.to_string_lossy().as_ref(),
            &[],
            &base.join("out"),
            &tight,
            None,
        )
        .unwrap_err();
        assert_eq!(err.code, ArchiveErrorCode::LimitExceeded);

        let _ = fs::remove_dir_all(&base);
    }
}
