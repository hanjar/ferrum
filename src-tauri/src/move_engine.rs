//! Move engine (task 12): try an atomic `fs::rename` first; when the OS
//! reports a cross-device link, fall back to copy → verify → delete.
//! The SOURCE is deleted only after every copied file verifies (D22:
//! existence + size equality; byte-level verify is a post-MVP option).
//!
//! Volume detection is NOT guessed from drive letters — we attempt the
//! rename and let the OS tell us (ERROR_NOT_SAME_DEVICE / EXDEV).

use crate::copy_engine::{
    ConflictPolicy, CopyOptions, CopyOutcome, FailedCopy, run_copy_with_options,
};
use crate::domain::{ErrorCode, FsError, OperationProgress};
use crate::ops::CancelToken;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveOutcome {
    pub moved: u64,
    /// Cross-device moves executed via copy-verify-delete.
    pub fallback_copied: u64,
    pub skipped: u64,
    pub renamed: u64,
    pub overwritten: u64,
    pub failures: Vec<FailedCopy>,
    pub cancelled: bool,
    /// Per-item (source, final destination) pairs that fully succeeded; feeds
    /// the activity identity hook. Never crosses IPC.
    #[serde(default, skip_serializing)]
    pub completed: Vec<(String, String)>,
}

fn is_cross_device(e: &std::io::Error) -> bool {
    // Windows: raw_os_error 17 (ERROR_NOT_SAME_DEVICE); Unix: 18 (EXDEV).
    matches!(e.raw_os_error(), Some(17) | Some(18))
}

/// Run a move. `force_fallback` exists so the copy-verify-delete path stays
/// testable on hosts with a single filesystem; production always passes false.
pub fn run_move(
    sources: &[String],
    target_dir: &str,
    policy: ConflictPolicy,
    cancel: &CancelToken,
    report: &dyn Fn(OperationProgress),
    force_fallback: bool,
) -> MoveOutcome {
    let mut out = MoveOutcome::default();
    let target = PathBuf::from(target_dir);
    if !target.is_dir() {
        out.failures.push(FailedCopy {
            source: target_dir.to_string(),
            code: ErrorCode::NotADirectory,
            message: "Target is not a directory".into(),
        });
        return out;
    }

    for src in sources {
        if cancel.is_cancelled() {
            out.cancelled = true;
            break;
        }
        let s = PathBuf::from(src);
        let Some(src_name) = s.file_name().map(|n| n.to_os_string()) else {
            out.failures.push(FailedCopy {
                source: src.clone(),
                code: ErrorCode::InvalidPath,
                message: "Source has no file name".into(),
            });
            continue;
        };
        let dest = target.join(&src_name);
        if dest == s || dest.starts_with(&s) || target.starts_with(&s) {
            out.failures.push(FailedCopy {
                source: src.clone(),
                code: ErrorCode::InvalidPath,
                message: "Cannot move a folder into itself".into(),
            });
            continue;
        }
        if dest.exists() && matches!(policy, ConflictPolicy::Ask | ConflictPolicy::Skip) {
            // Ask: planning surfaced it; Skip: explicit skip.
            out.skipped += 1;
            continue;
        }
        // Overwrite-style policies for rename() would clobber; handle via
        // copy-engine resolution instead when a collision exists.
        if dest.exists()
            && matches!(
                policy,
                ConflictPolicy::Overwrite
                    | ConflictPolicy::OverwriteOlder
                    | ConflictPolicy::RenameAuto
            )
        {
            match copy_verify_delete(&s, &dest, target_dir, policy, cancel, report, &mut out) {
                Fallback::Done => out.moved += 1,
                Fallback::NotDone => {}
            }
            continue;
        }

        if force_fallback {
            match copy_verify_delete(&s, &dest, target_dir, policy, cancel, report, &mut out) {
                Fallback::Done => out.moved += 1,
                Fallback::NotDone => {}
            }
            continue;
        }

        match fs::rename(&s, &dest) {
            Ok(()) => {
                out.moved += 1;
                out.completed
                    .push((src.clone(), dest.to_string_lossy().into_owned()));
            }
            Err(e) if is_cross_device(&e) => {
                match copy_verify_delete(&s, &dest, target_dir, policy, cancel, report, &mut out) {
                    Fallback::Done => out.moved += 1,
                    Fallback::NotDone => {}
                }
            }
            Err(e) => {
                let fe = FsError::from(e).with_path(src.clone());
                out.failures.push(FailedCopy {
                    source: src.clone(),
                    code: fe.code,
                    message: fe.message,
                });
            }
        }
    }
    out
}

enum Fallback {
    Done,
    NotDone,
}

#[allow(clippy::too_many_arguments)]
fn copy_verify_delete(
    src: &Path,
    dest: &Path,
    target_dir: &str,
    policy: ConflictPolicy,
    cancel: &CancelToken,
    report: &dyn Fn(OperationProgress),
    out: &mut MoveOutcome,
) -> Fallback {
    // 1. Copy (conflict policy already decided by caller for collisions;
    //    fresh names go through Ask which is a no-op when no collision).
    // The source is deleted below, so every copied file must be ON DISK first: durable=true flushes
    // each staged file before it is published (a copy-then-delete without it can lose both sides on
    // a power cut). Never relax this for move.
    let (threads, priority) = crate::copy_pool::defaults();
    let copy: CopyOutcome = run_copy_with_options(
        &[src.to_string_lossy().into_owned()],
        target_dir,
        policy,
        CopyOptions {
            threads,
            priority,
            durable: true,
            ..CopyOptions::default()
        },
        cancel,
        report,
    );
    out.skipped += copy.skipped;
    out.renamed += copy.renamed;
    out.overwritten += copy.overwritten;
    if copy.cancelled || cancel.is_cancelled() {
        out.cancelled = true;
        return Fallback::NotDone; // source untouched
    }
    if !copy.failures.is_empty() {
        out.failures.extend(copy.failures);
        return Fallback::NotDone; // source untouched
    }
    // 2. Verify: every source file must exist at dest with equal size.
    let final_dest = if copy.renamed > 0 {
        // Policy RenameAuto produced "name (2)": locate it next to dest.
        find_renamed(dest).unwrap_or_else(|| dest.to_path_buf())
    } else {
        dest.to_path_buf()
    };
    if let Err(fe) = verify_tree(src, &final_dest) {
        out.failures.push(FailedCopy {
            source: src.to_string_lossy().into_owned(),
            code: fe.code,
            message: format!("verify failed, source kept: {}", fe.message),
        });
        return Fallback::NotDone; // verification failed → keep source
    }
    // 3. Delete the source tree only after verification passed.
    if let Err(e) = remove_tree(src) {
        let fe = FsError::from(e).with_path(src.to_string_lossy().into_owned());
        out.failures.push(FailedCopy {
            source: src.to_string_lossy().into_owned(),
            code: fe.code,
            message: format!("copied but source removal failed: {}", fe.message),
        });
        return Fallback::NotDone;
    }
    out.fallback_copied += copy.copied_files.max(1);
    out.completed.push((
        src.to_string_lossy().into_owned(),
        final_dest.to_string_lossy().into_owned(),
    ));
    Fallback::Done
}

fn find_renamed(dest: &Path) -> Option<PathBuf> {
    let stem = dest.file_stem()?.to_str()?;
    let parent = dest.parent()?;
    for entry in fs::read_dir(parent).ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(stem) && name.contains('(') {
            return Some(entry.path());
        }
    }
    None
}

/// Verify: dest must mirror src — every file present with identical size.
pub fn verify_tree(src: &Path, dest: &Path) -> Result<(), FsError> {
    let sm =
        fs::symlink_metadata(src).map_err(|e| FsError::from(e).with_path(src.to_string_lossy()))?;
    if sm.is_dir() {
        for item in fs::read_dir(src)
            .map_err(|e| FsError::from(e).with_path(src.to_string_lossy()))?
            .flatten()
        {
            let child = item.path();
            let name = child.file_name().unwrap_or_default();
            verify_tree(&child, &dest.join(name))?;
        }
        Ok(())
    } else {
        let dm = fs::metadata(dest).map_err(|_| {
            FsError::new(ErrorCode::Io, "destination file missing")
                .with_path(dest.to_string_lossy())
        })?;
        if dm.len() != sm.len() {
            return Err(FsError::new(
                ErrorCode::Io,
                format!("size mismatch: src {} vs dest {}", sm.len(), dm.len()),
            )
            .with_path(dest.to_string_lossy()));
        }
        Ok(())
    }
}

fn remove_tree(p: &Path) -> std::io::Result<()> {
    let meta = fs::symlink_metadata(p)?;
    if meta.is_dir() {
        fs::remove_dir_all(p)
    } else {
        fs::remove_file(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQ: AtomicU64 = AtomicU64::new(0);

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ferrum-t12-{tag}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn silent(_p: OperationProgress) {}

    fn mv(
        sources: &[PathBuf],
        target: &Path,
        policy: ConflictPolicy,
        force_fallback: bool,
    ) -> MoveOutcome {
        let srcs: Vec<String> = sources
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        run_move(
            &srcs,
            &target.to_string_lossy(),
            policy,
            &CancelToken::default(),
            &silent,
            force_fallback,
        )
    }

    fn write(p: &Path, bytes: &[u8]) {
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(p, bytes).unwrap();
    }

    #[test]
    fn same_volume_move_is_atomic_rename() {
        let root = temp("atomic");
        write(&root.join("src/file.txt"), b"content-123");
        let dest = root.join("out");
        fs::create_dir(&dest).unwrap();
        let out = mv(
            &[root.join("src/file.txt")],
            &dest,
            ConflictPolicy::Ask,
            false,
        );
        assert_eq!(out.moved, 1);
        assert!(!root.join("src/file.txt").exists()); // gone from source
        assert_eq!(fs::read(dest.join("file.txt")).unwrap(), b"content-123");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn same_volume_directory_move() {
        let root = temp("dirmove");
        write(&root.join("src/sub/deep/x.txt"), b"x");
        write(&root.join("src/y.txt"), b"y");
        let dest = root.join("out");
        fs::create_dir(&dest).unwrap();
        let out = mv(&[root.join("src")], &dest, ConflictPolicy::Ask, false);
        assert_eq!(out.moved, 1);
        assert!(!root.join("src").exists());
        assert_eq!(fs::read(dest.join("src/sub/deep/x.txt")).unwrap(), b"x");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn fallback_moves_and_deletes_source_after_verify() {
        let root = temp("fallback");
        write(&root.join("src/big.bin"), &vec![9u8; 3_000_000]);
        write(&root.join("src/small.txt"), b"s");
        let dest = root.join("out");
        fs::create_dir(&dest).unwrap();
        let out = mv(&[root.join("src")], &dest, ConflictPolicy::Ask, true);
        assert_eq!(out.moved, 1);
        assert!(out.fallback_copied >= 2);
        assert!(!root.join("src").exists()); // deleted only after verify
        assert_eq!(fs::read(dest.join("src/small.txt")).unwrap(), b"s");
        assert_eq!(fs::read(dest.join("src/big.bin")).unwrap().len(), 3_000_000);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn verify_failure_keeps_the_source() {
        let root = temp("verifyfail");
        let src = root.join("file.bin");
        write(&src, b"0123456789");
        // Pre-create a destination with the WRONG size; policy Overwrite
        // would replace it — use Ask so the copy skips and nothing moves.
        let dest = root.join("out");
        fs::create_dir(&dest).unwrap();
        write(&dest.join("file.bin"), b"short");
        let srcs = [src.clone()];
        let out = mv(&srcs, &dest, ConflictPolicy::Ask, true);
        assert_eq!(out.moved, 0);
        assert!(src.exists(), "source must survive any failure mode");
        // Direct verify check: mismatched trees are detected.
        let v = verify_tree(&src, &dest.join("file.bin"));
        assert!(v.is_err());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn self_move_and_nested_target_rejected() {
        let root = temp("selfmove");
        write(&root.join("src/a.txt"), b"a");
        let inner = root.join("src/inner");
        fs::create_dir_all(&inner).unwrap();
        let out = mv(&[root.join("src")], &inner, ConflictPolicy::Ask, false);
        assert_eq!(out.moved, 0);
        assert_eq!(out.failures.len(), 1);
        assert_eq!(out.failures[0].code, ErrorCode::InvalidPath);
        assert!(root.join("src/a.txt").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn conflicts_ask_and_skip_leave_destination() {
        let root = temp("mconflict");
        write(&root.join("src/file.txt"), b"NEW");
        let dest = root.join("out");
        fs::create_dir(&dest).unwrap();
        write(&dest.join("file.txt"), b"OLD");
        for policy in [ConflictPolicy::Ask, ConflictPolicy::Skip] {
            let out = mv(&[root.join("src/file.txt")], &dest, policy, false);
            assert_eq!(out.skipped, 1, "{policy:?}");
            assert_eq!(fs::read(dest.join("file.txt")).unwrap(), b"OLD");
            assert!(root.join("src/file.txt").exists(), "source kept on skip");
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn overwrite_policy_via_fallback_replaces_and_removes_source() {
        let root = temp("mover");
        write(&root.join("src/file.txt"), b"NEW");
        let dest = root.join("out");
        fs::create_dir(&dest).unwrap();
        write(&dest.join("file.txt"), b"OLD");
        let out = mv(
            &[root.join("src/file.txt")],
            &dest,
            ConflictPolicy::Overwrite,
            true,
        );
        assert_eq!(out.moved, 1);
        assert_eq!(fs::read(dest.join("file.txt")).unwrap(), b"NEW");
        assert!(!root.join("src/file.txt").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn cancel_during_fallback_keeps_source() {
        let root = temp("mcancel");
        let src = root.join("big.bin");
        write(&src, &vec![5u8; 4 * 1024 * 1024]);
        let dest = root.join("out");
        fs::create_dir(&dest).unwrap();

        let token = CancelToken::default();
        let tok2 = token.clone();
        let cancel_report = move |p: OperationProgress| {
            if p.processed_bytes >= crate::copy_engine::CHUNK as u64 {
                tok2.cancel();
            }
        };
        let out = run_move(
            &[src.to_string_lossy().into_owned()],
            &dest.to_string_lossy(),
            ConflictPolicy::Ask,
            &token,
            &cancel_report,
            true, // force the copy-verify-delete path
        );
        assert!(out.cancelled);
        assert!(src.exists(), "cancelled move must never delete the source");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_source_reports_failure() {
        let root = temp("mmissing");
        let dest = root.join("out");
        fs::create_dir(&dest).unwrap();
        let out = mv(&[root.join("ghost.txt")], &dest, ConflictPolicy::Ask, false);
        assert_eq!(out.moved, 0);
        assert_eq!(out.failures.len(), 1);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn fer471_move_flushes_every_copied_file_before_commit_and_only_then_deletes_the_source() {
        let root = temp("durable-move");
        let src = root.join("tree");
        for i in 0..6 {
            write(&src.join(format!("f{i}.bin")), &vec![i as u8; 40_000 + i]);
        }
        let dst = root.join("out");
        fs::create_dir(&dst).unwrap();
        let out = mv(std::slice::from_ref(&src), &dst, ConflictPolicy::Ask, true);
        assert!(out.failures.is_empty(), "{:?}", out.failures);
        assert_eq!(out.fallback_copied, 6);
        assert!(!src.exists(), "source removed after the verified copy");
        let events = crate::copy_engine::events_under(&dst);
        for i in 0..6 {
            let file = dst.join("tree").join(format!("f{i}.bin"));
            let mine: Vec<_> = events
                .iter()
                .filter(|(p, _)| *p == file)
                .map(|e| e.1)
                .collect();
            assert_eq!(
                mine,
                vec!["flush", "commit", "dirflush"],
                "file {i}: flush, commit, directory flush"
            );
            assert_eq!(fs::read(&file).unwrap(), vec![i as u8; 40_000 + i]);
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn fer471_move_onto_an_existing_destination_is_durable_too() {
        let root = temp("durable-move-existing");
        write(&root.join("a").join("x.txt"), b"new content");
        let dst = root.join("out");
        write(&dst.join("x.txt"), b"old");
        let out = mv(
            &[root.join("a").join("x.txt")],
            &dst,
            ConflictPolicy::Overwrite,
            false,
        );
        assert!(out.failures.is_empty(), "{:?}", out.failures);
        assert_eq!(fs::read(dst.join("x.txt")).unwrap(), b"new content");
        let ev: Vec<_> = crate::copy_engine::events_under(&dst)
            .iter()
            .map(|e| e.1)
            .collect();
        assert_eq!(ev, vec!["flush", "commit", "dirflush"]);
        assert!(!root.join("a").join("x.txt").exists());
        let _ = fs::remove_dir_all(&root);
    }
}
