//! Multi-Rename Tool executor (F2-20). Kept Tauri-free for unit testing;
//! `ipc.rs` exposes the thin `rename_batch` command.
//!
//! Contract: one ordered batch of (source path, new bare name) pairs. Every
//! pair is validated up front (target names are portable single segments —
//! no Windows reserved/invalid names, matching the TS preview — sources
//! exist, targets are free — or differ only in case from the source itself,
//! which is the Windows case-only rename). Targets that are another batch
//! member's source path (swap/cycle) are legal: those sources move onto
//! unique temporary names first and take their final names in a second
//! phase. A pair that fails after staging is restored to its original name.
//!
//! Execution is sequential with a per-item report; one failure never aborts
//! the batch. There is deliberately NO automatic rollback — the UI keeps the
//! successful (from → to) journal and offers an explicit session Undo.

use crate::domain::{ErrorCode, FsError};
use crate::mutations::validate_portable_name;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// One requested rename: absolute source path + new bare name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenamePair {
    pub from: String,
    pub to: String,
}

/// Per-item failure (shape mirrors `mutations::FailedItem`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FailedRename {
    pub from: String,
    pub to: String,
    pub code: ErrorCode,
    pub message: String,
}

/// Batch report. `journal` lists the renames that actually happened, in
/// execution order — replaying it reversed is the session Undo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenameBatchReport {
    pub requested: usize,
    pub renamed: usize,
    pub failed: Vec<FailedRename>,
    pub journal: Vec<RenamePair>,
}

/// True when both paths denote the same physical file (canonical compare),
/// or — when canonicalization is impossible — a Windows case-variant of the
/// same string.
fn same_file(a: &Path, b: &Path) -> bool {
    if let (Ok(ca), Ok(cb)) = (fs::canonicalize(a), fs::canonicalize(b)) {
        return ca == cb;
    }
    cfg!(windows)
        && a.to_string_lossy()
            .eq_ignore_ascii_case(&b.to_string_lossy())
}

/// Windows path identity is case-insensitive; other platforms are exact.
fn paths_alias(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    cfg!(windows) && a.eq_ignore_ascii_case(b)
}

/// A temporary name that does not exist yet, in the same directory.
fn free_temp_name(source: &Path, index: usize) -> PathBuf {
    let dir = source.parent().unwrap_or_else(|| Path::new("."));
    for attempt in 0..1000 {
        let candidate = dir.join(format!(".ferrum-mr-tmp-{index}-{attempt}"));
        if fs::symlink_metadata(&candidate).is_err() {
            return candidate;
        }
    }
    dir.join(format!(".ferrum-mr-tmp-{index}-{}", std::process::id()))
}

struct Work<'a> {
    pair: &'a RenamePair,
    target: PathBuf,
    /// Set in phase 1 when the source stepped aside onto a temp name.
    staged: Option<PathBuf>,
    failed: bool,
}

/// Execute the batch. See the module docs for phases and failure policy.
pub fn rename_batch(pairs: &[RenamePair]) -> RenameBatchReport {
    let mut report = RenameBatchReport {
        requested: pairs.len(),
        renamed: 0,
        failed: Vec::new(),
        journal: Vec::new(),
    };

    // ── Validate up front so an invalid pair never executes half a batch ──
    let mut work: Vec<Work<'_>> = Vec::with_capacity(pairs.len());
    for pair in pairs {
        if let Err(e) = validate_portable_name(&pair.to) {
            report.failed.push(FailedRename {
                from: pair.from.clone(),
                to: pair.to.clone(),
                code: e.code,
                message: e.message,
            });
            continue;
        }
        if !fs::symlink_metadata(&pair.from).is_ok() {
            report.failed.push(FailedRename {
                from: pair.from.clone(),
                to: pair.to.clone(),
                code: ErrorCode::NotFound,
                message: "Entry not found".into(),
            });
            continue;
        }
        work.push(Work {
            pair,
            target: Path::new(&pair.from)
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join(&pair.to),
            staged: None,
            failed: false,
        });
    }

    let sources: Vec<&str> = work.iter().map(|w| w.pair.from.as_str()).collect();
    let is_batch_source = |p: &Path| sources.iter().any(|s| paths_alias(s, &p.to_string_lossy()));

    // ── Phase 1: swap/cycle participants step aside onto temporary names ──
    // Staging happens whenever the target is another member's source path,
    // even if that name is currently free — the occupant pair may move it
    // away later in this same batch, and staging makes the order irrelevant.
    for (index, w) in work.iter_mut().enumerate() {
        let original = Path::new(&w.pair.from);
        if w.target == original {
            continue; // no-op rename
        }
        if w.target.exists() && same_file(&w.target, original) {
            continue; // case-only rename of the same physical file
        }
        if is_batch_source(&w.target) {
            let temp = free_temp_name(original, index);
            match fs::rename(original, &temp) {
                Ok(()) => w.staged = Some(temp),
                Err(e) => {
                    let fe = FsError::from(e).with_path(w.pair.from.clone());
                    report.failed.push(FailedRename {
                        from: w.pair.from.clone(),
                        to: w.pair.to.clone(),
                        code: fe.code,
                        message: fe.message,
                    });
                    w.failed = true;
                }
            }
        }
    }

    // ── Phase 2: everything moves to its final name (staged from temp) ────
    for w in work.iter_mut() {
        if w.failed {
            continue;
        }
        let current: &Path = w.staged.as_deref().unwrap_or(Path::new(&w.pair.from));
        if current == w.target.as_path() {
            continue; // nothing to do
        }
        // Refuse to overwrite anything (fs::rename would silently overwrite
        // on Unix); a same-file target is the legal case-only rename.
        if w.target.exists() && !same_file(&w.target, current) {
            restore_staged(w);
            report.failed.push(FailedRename {
                from: w.pair.from.clone(),
                to: w.pair.to.clone(),
                code: ErrorCode::AlreadyExists,
                message: "Target name is taken".into(),
            });
            w.failed = true;
            continue;
        }
        match fs::rename(current, &w.target) {
            Ok(()) => {
                report.renamed += 1;
                report.journal.push(RenamePair {
                    from: w.pair.from.clone(),
                    to: w.pair.to.clone(),
                });
            }
            Err(e) => {
                restore_staged(w);
                let fe = FsError::from(e).with_path(w.pair.from.clone());
                report.failed.push(FailedRename {
                    from: w.pair.from.clone(),
                    to: w.pair.to.clone(),
                    code: fe.code,
                    message: fe.message,
                });
                w.failed = true;
            }
        }
    }
    report
}

/// Best-effort restore when a staged source cannot take its final name:
/// the file goes back to its original name instead of being left on a
/// hidden temporary path.
fn restore_staged(w: &mut Work<'_>) {
    if let Some(temp) = w.staged.take() {
        let original = Path::new(&w.pair.from);
        if fs::symlink_metadata(original).is_err() {
            let _ = fs::rename(&temp, original);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQ: AtomicU64 = AtomicU64::new(0);

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ferrum-f220-{tag}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn s(p: &Path) -> String {
        p.to_string_lossy().into_owned()
    }

    fn pair(from: &Path, to: &str) -> RenamePair {
        RenamePair {
            from: s(from),
            to: to.to_string(),
        }
    }

    #[test]
    fn renames_a_plain_batch_in_order() {
        let root = temp("plain");
        fs::write(root.join("a.txt"), b"1").unwrap();
        fs::write(root.join("b.txt"), b"2").unwrap();
        let report = rename_batch(&[
            pair(&root.join("a.txt"), "one.txt"),
            pair(&root.join("b.txt"), "two.txt"),
        ]);
        assert_eq!(report.renamed, 2, "failed: {:?}", report.failed);
        assert_eq!(report.journal.len(), 2);
        assert!(root.join("one.txt").exists() && root.join("two.txt").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn swaps_two_files_via_temporary_names() {
        let root = temp("swap");
        fs::write(root.join("a.txt"), b"A").unwrap();
        fs::write(root.join("b.txt"), b"B").unwrap();
        let report = rename_batch(&[
            pair(&root.join("a.txt"), "b.txt"),
            pair(&root.join("b.txt"), "a.txt"),
        ]);
        assert_eq!(report.renamed, 2, "failed: {:?}", report.failed);
        assert_eq!(fs::read(root.join("a.txt")).unwrap(), b"B");
        assert_eq!(fs::read(root.join("b.txt")).unwrap(), b"A");
        assert_eq!(fs::read_dir(&root).unwrap().count(), 2, "no temp leftovers");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn rotates_three_files_without_loss() {
        let root = temp("cycle3");
        for (name, body) in [("f1", &b"1"[..]), ("f2", &b"2"[..]), ("f3", &b"3"[..])] {
            fs::write(root.join(format!("{name}.txt")), body).unwrap();
        }
        let report = rename_batch(&[
            pair(&root.join("f1.txt"), "f2.txt"),
            pair(&root.join("f2.txt"), "f3.txt"),
            pair(&root.join("f3.txt"), "f1.txt"),
        ]);
        assert_eq!(report.renamed, 3, "failed: {:?}", report.failed);
        assert_eq!(fs::read(root.join("f1.txt")).unwrap(), b"3");
        assert_eq!(fs::read(root.join("f2.txt")).unwrap(), b"1");
        assert_eq!(fs::read(root.join("f3.txt")).unwrap(), b"2");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn case_only_rename_is_allowed_on_the_same_file() {
        let root = temp("caseonly");
        fs::write(root.join("notes.txt"), b"x").unwrap();
        let report = rename_batch(&[pair(&root.join("notes.txt"), "NOTES.TXT")]);
        assert_eq!(report.renamed, 1, "failed: {:?}", report.failed);
        assert_eq!(report.journal.len(), 1);
        // Case-insensitive hosts still resolve the old spelling; assert the
        // new name resolves and the directory holds exactly one entry.
        assert!(root.join("NOTES.TXT").exists());
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn failing_items_never_abort_the_batch_and_leave_no_temp_files() {
        let root = temp("partial");
        fs::write(root.join("keep.txt"), b"k").unwrap();
        fs::create_dir(root.join("sub")).unwrap(); // rename onto a dir fails
        let report = rename_batch(&[
            pair(&root.join("missing.txt"), "x.txt"), // source missing
            pair(&root.join("keep.txt"), "sub"),      // target taken by a dir
            pair(&root.join("keep.txt"), "moved.txt"),
        ]);
        assert_eq!(report.requested, 3);
        assert_eq!(report.renamed, 1);
        assert_eq!(report.failed.len(), 2);
        assert_eq!(report.failed[0].code, ErrorCode::NotFound);
        assert_eq!(report.failed[1].code, ErrorCode::AlreadyExists);
        assert!(root.join("moved.txt").exists());
        assert_eq!(
            fs::read_dir(&root).unwrap().count(),
            2, // moved.txt + sub — no temp leftovers
            "no temp leftovers"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn separator_targets_are_rejected_structurally() {
        let root = temp("invalid");
        fs::write(root.join("a.txt"), b"1").unwrap();
        let report = rename_batch(&[pair(&root.join("a.txt"), "sub/dir.txt")]);
        assert_eq!(report.renamed, 0);
        assert_eq!(report.failed.len(), 1);
        assert_eq!(report.failed[0].code, ErrorCode::InvalidPath);
        assert!(root.join("a.txt").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn staged_source_is_restored_when_its_final_target_is_still_occupied() {
        let root = temp("restore");
        fs::write(root.join("a.txt"), b"A").unwrap();
        fs::write(root.join("b.txt"), b"B").unwrap();
        // a→b is cross (b is a batch source) so `a` stages onto a temp name;
        // because the list order runs a→b BEFORE b moves away, the target is
        // still occupied: a must be reported as failed and RESTORED to its
        // original name, while the second pair still succeeds.
        let report = rename_batch(&[
            pair(&root.join("a.txt"), "b.txt"),
            pair(&root.join("b.txt"), "free.txt"),
        ]);
        assert_eq!(report.renamed, 1);
        assert_eq!(report.failed.len(), 1);
        assert_eq!(report.failed[0].from, s(&root.join("a.txt")));
        assert_eq!(report.failed[0].code, ErrorCode::AlreadyExists);
        assert!(root.join("a.txt").exists(), "a restored to original name");
        assert_eq!(fs::read(root.join("a.txt")).unwrap(), b"A");
        assert!(root.join("free.txt").exists());
        assert_eq!(
            fs::read_dir(&root).unwrap().count(),
            2, // a.txt (restored) + free.txt — no temp leftovers
            "no temp leftovers"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn undo_journal_replays_renames_reversed() {
        let root = temp("undo");
        fs::write(root.join("a.txt"), b"1").unwrap();
        let report = rename_batch(&[pair(&root.join("a.txt"), "z.txt")]);
        // Undo = swap journal direction (dir stays the same in this tool).
        let undo: Vec<RenamePair> = report
            .journal
            .iter()
            .rev()
            .map(|p| RenamePair {
                from: Path::new(&p.from)
                    .parent()
                    .unwrap()
                    .join(&p.to)
                    .to_string_lossy()
                    .into_owned(),
                to: Path::new(&p.from)
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
            })
            .collect();
        let undone = rename_batch(&undo);
        assert_eq!(undone.renamed, 1, "failed: {:?}", undone.failed);
        assert!(root.join("a.txt").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn thousand_file_batch_stays_ordered_and_complete() {
        let root = temp("kilo");
        for i in 0..1000 {
            fs::write(root.join(format!("old-{i:04}.txt")), b"x").unwrap();
        }
        let pairs: Vec<RenamePair> = (0..1000)
            .map(|i| {
                pair(
                    &root.join(format!("old-{i:04}.txt")),
                    &format!("new-{i:04}.txt"),
                )
            })
            .collect();
        let report = rename_batch(&pairs);
        assert_eq!(
            report.renamed,
            1000,
            "first failure: {:?}",
            report.failed.first()
        );
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1000);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn duplicate_targets_fail_per_item_without_losing_files() {
        let root = temp("dupes");
        fs::write(root.join("a.txt"), b"1").unwrap();
        fs::write(root.join("b.txt"), b"2").unwrap();
        let report = rename_batch(&[
            pair(&root.join("a.txt"), "same.txt"),
            pair(&root.join("b.txt"), "same.txt"),
        ]);
        assert_eq!(report.renamed, 1);
        assert_eq!(report.failed.len(), 1);
        assert_eq!(report.failed[0].code, ErrorCode::AlreadyExists);
        assert!(root.join("same.txt").exists());
        assert!(root.join("b.txt").exists());
        let _ = fs::remove_dir_all(&root);
    }

    fn names_in(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn windows_reserved_and_invalid_targets_are_rejected_without_touching_files() {
        let root = temp("reserved");
        fs::write(root.join("a.txt"), b"1").unwrap();
        for bad in [
            "CON",
            "nul.txt",
            "COM1",
            "LPT9.log",
            "a:b.txt",
            "a?.txt",
            "a*.txt",
            "a<b",
            "a|b",
            "q\"x",
            "tab\there",
            "trail.",
            "trail ",
        ] {
            let report = rename_batch(&[pair(&root.join("a.txt"), bad)]);
            assert_eq!(report.renamed, 0, "{bad:?} must not execute");
            assert_eq!(report.failed.len(), 1, "{bad:?}");
            assert_eq!(report.failed[0].code, ErrorCode::InvalidPath, "{bad:?}");
        }
        assert_eq!(names_in(&root), vec!["a.txt".to_string()]);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn unicode_names_round_trip_through_the_executor() {
        let root = temp("unicode");
        let targets = [
            "café.txt",            // NFC é
            "日本語のファイル.md", // CJK
            "🎉party🎈.png",       // astral-plane emoji
            "Ünï-çödé (2).txt",
            "STRASSE.TXT",  // ß expands under upper-casing
            "İstanbul.txt", // dotted capital I
        ];
        let sources = ["s0", "s1", "s2", "s3", "straße.txt", "istanbul.txt"];
        for src in sources {
            fs::write(root.join(src), src.as_bytes()).unwrap();
        }
        let pairs: Vec<RenamePair> = sources
            .iter()
            .zip(targets)
            .map(|(from, to)| pair(&root.join(from), to))
            .collect();
        let report = rename_batch(&pairs);
        assert_eq!(report.renamed, 6, "failed: {:?}", report.failed);
        let mut want: Vec<String> = targets.iter().map(|t| t.to_string()).collect();
        want.sort();
        assert_eq!(names_in(&root), want);
        // Content followed its file through the rename.
        assert_eq!(
            fs::read(root.join("STRASSE.TXT")).unwrap(),
            "straße.txt".as_bytes()
        );
        assert_eq!(fs::read(root.join("日本語のファイル.md")).unwrap(), b"s1");
        let _ = fs::remove_dir_all(&root);
    }

    // macOS normalizes NFC/NFD to the same name, so the two spellings are
    // only distinct entries on Windows/Linux filesystems.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn nfc_and_nfd_spellings_stay_distinct_targets() {
        let root = temp("nfd");
        let nfd = "cafe\u{301}.txt"; // e + combining acute
        let nfc = "caf\u{e9}.txt";
        fs::write(root.join("a"), b"1").unwrap();
        fs::write(root.join("b"), b"2").unwrap();
        let report = rename_batch(&[pair(&root.join("a"), nfd), pair(&root.join("b"), nfc)]);
        assert_eq!(report.renamed, 2, "failed: {:?}", report.failed);
        assert_eq!(fs::read(root.join(nfd)).unwrap(), b"1");
        assert_eq!(fs::read(root.join(nfc)).unwrap(), b"2");
        assert_eq!(names_in(&root).len(), 2);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn unicode_case_only_rename_keeps_the_new_spelling() {
        let root = temp("ucase");
        fs::write(root.join("ÉCOLE.txt"), b"x").unwrap();
        let report = rename_batch(&[pair(&root.join("ÉCOLE.txt"), "école.txt")]);
        assert_eq!(report.renamed, 1, "failed: {:?}", report.failed);
        assert_eq!(names_in(&root), vec!["école.txt".to_string()]);
        let _ = fs::remove_dir_all(&root);
    }

    /// Shared with `src/lib/multiRename.contract.test.ts`: the TS preview must
    /// produce `expectedTargets`, and this executor must turn the same inputs
    /// into exactly those names on disk (preview == actual).
    #[test]
    fn executor_realises_the_shared_preview_fixture() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("contract-fixtures")
            .join("multi-rename")
            .join("cases.json");
        let cases: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
        let cases = cases.as_array().unwrap();
        assert!(!cases.is_empty());
        for case in cases {
            let label = case["name"].as_str().unwrap();
            let strs = |k: &str| -> Vec<String> {
                case[k]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_str().unwrap().to_string())
                    .collect()
            };
            let (inputs, expected) = (strs("inputs"), strs("expectedTargets"));
            assert_eq!(inputs.len(), expected.len(), "{label}");
            let root = temp("fixture");
            for name in &inputs {
                fs::write(root.join(name), name.as_bytes()).unwrap();
            }
            // Same filter as TS `executablePairs`: unchanged names are skipped.
            let pairs: Vec<RenamePair> = inputs
                .iter()
                .zip(&expected)
                .filter(|(from, to)| from != to)
                .map(|(from, to)| pair(&root.join(from), to))
                .collect();
            let report = rename_batch(&pairs);
            assert_eq!(report.renamed, pairs.len(), "{label}: {:?}", report.failed);
            let mut want = expected.clone();
            want.sort();
            assert_eq!(names_in(&root), want, "{label}");
            for (from, to) in inputs.iter().zip(&expected) {
                assert_eq!(
                    fs::read(root.join(to)).unwrap(),
                    from.as_bytes(),
                    "{label}: content must follow {from} -> {to}"
                );
            }
            let _ = fs::remove_dir_all(&root);
        }
    }
}
