//! FER-585 (FER-577 B5): a cancellable, progress-reporting delete.
//!
//! The IPC layer (`delete_start`) runs [`run_permanent`] / [`run_trash`] on a blocking thread and turns
//! [`DeleteProgress`] snapshots into throttled `delete://progress` events. The path gates
//! (`prepare_sources`: string gate, protected folders, reserved device names) run BEFORE a job starts,
//! exactly as for the old synchronous `delete_entries`; nothing in here widens what can be deleted.
//!
//! Cancel is cooperative and checked at an item boundary and, for a permanent delete of a folder,
//! between the entries inside it. What a cancel leaves behind is never silent:
//! - `not_processed`: top-level items that were not touched at all;
//! - `partial`: top-level folders that lost some entries and still exist (permanent delete only; the
//!   Recycle Bin moves one item atomically, so a trashed folder is either in the bin or untouched).
//!
//! `report.deleted` counts only items that are completely gone, so Activity receipts (built by the
//! caller from `failed`, `not_processed` and `partial`) are written for those alone.

use crate::domain::{ErrorCode, FsError};
use crate::mutations::{DeleteReport, FailedItem};
use std::fs;
use std::path::Path;

/// Live numbers of one delete job. The IPC layer throttles how often they are sent.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteProgress {
    /// Top-level items finished (deleted, failed or not found).
    pub done: usize,
    /// Top-level items in the job.
    pub total: usize,
    /// The top-level item being deleted now (last path segment).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_name: Option<String>,
    /// Files and folders removed so far, inside folders included.
    pub entries: u64,
}

/// What a delete job did. `report` keeps the shape the dialog always had.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteJobResult {
    pub report: DeleteReport,
    /// The user cancelled before every item was processed.
    pub cancelled: bool,
    /// Top-level items a cancel never reached: still exactly where they were.
    pub not_processed: Vec<String>,
    /// Folders a cancel stopped inside: some entries are gone, the folder is still there.
    pub partial: Vec<String>,
}

impl DeleteJobResult {
    /// Top-level paths that still exist because the job was cancelled (for a "continue" retry).
    pub fn remaining(&self) -> Vec<String> {
        self.partial
            .iter()
            .chain(self.not_processed.iter())
            .cloned()
            .collect()
    }
}

type Remover<'a> = &'a dyn Fn(&Path) -> std::io::Result<()>;

/// How one top-level item is removed.
enum Mode<'a> {
    /// Recycle Bin: one call per item. A reserved Windows name cannot go there (FER-559 N4).
    Trash(Remover<'a>),
    Permanent,
}

/// Permanent delete, cancellable between entries. `cancelled` is polled; `progress` sees every step.
pub fn run_permanent(
    paths: &[String],
    cancelled: &dyn Fn() -> bool,
    progress: &mut dyn FnMut(&DeleteProgress),
) -> DeleteJobResult {
    run(paths, &Mode::Permanent, cancelled, progress)
}

/// Delete to the Recycle Bin / Trash, one shell call per item so progress and cancel work between items.
pub fn run_trash(
    paths: &[String],
    cancelled: &dyn Fn() -> bool,
    progress: &mut dyn FnMut(&DeleteProgress),
) -> DeleteJobResult {
    run_trash_with(
        paths,
        &|p| trash::delete(p).map_err(|e| std::io::Error::other(e.to_string())),
        cancelled,
        progress,
    )
}

fn run_trash_with(
    paths: &[String],
    remover: Remover,
    cancelled: &dyn Fn() -> bool,
    progress: &mut dyn FnMut(&DeleteProgress),
) -> DeleteJobResult {
    run(paths, &Mode::Trash(remover), cancelled, progress)
}

fn last_name(p: &str) -> String {
    let trimmed = p.trim_end_matches(['\\', '/']);
    trimmed
        .rsplit(['\\', '/'])
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(trimmed)
        .to_string()
}

fn failed(path: &str, code: ErrorCode, message: String) -> FailedItem {
    FailedItem {
        path: path.to_string(),
        code,
        message,
    }
}

/// Outcome of removing one folder tree.
enum Tree {
    Done,
    /// Cancelled between entries.
    Cancelled,
    Failed(std::io::Error),
}

struct Walk<'a> {
    cancelled: &'a dyn Fn() -> bool,
    progress: &'a mut dyn FnMut(&DeleteProgress),
    state: &'a mut DeleteProgress,
    /// Entries removed inside the current top-level item.
    removed_here: u64,
}

impl Walk<'_> {
    fn removed_one(&mut self) {
        self.removed_here += 1;
        self.state.entries += 1;
        (self.progress)(self.state);
    }
}

/// A file, symlink or junction. A read-only file is made writable once and retried.
fn remove_leaf(path: &Path, is_symlink: bool) -> std::io::Result<()> {
    let attempt = |p: &Path| {
        if is_symlink {
            // A directory symlink / junction is removed as a link, never followed.
            fs::remove_file(p).or_else(|_| fs::remove_dir(p))
        } else {
            fs::remove_file(p)
        }
    };
    match attempt(path) {
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            if let Ok(meta) = fs::symlink_metadata(path) {
                let mut perms = meta.permissions();
                if perms.readonly() {
                    #[allow(clippy::permissions_set_readonly_false)]
                    perms.set_readonly(false);
                    if fs::set_permissions(path, perms).is_ok() {
                        return attempt(path);
                    }
                }
            }
            Err(e)
        }
        other => other,
    }
}

/// Remove `dir` and everything below it, checking for a cancel before each entry.
fn remove_tree(dir: &Path, w: &mut Walk) -> Tree {
    let children: Vec<fs::DirEntry> = match fs::read_dir(dir) {
        Ok(rd) => match rd.collect::<Result<Vec<_>, _>>() {
            Ok(v) => v,
            Err(e) => return Tree::Failed(e),
        },
        Err(e) => return Tree::Failed(e),
    };
    for child in children {
        if (w.cancelled)() {
            return Tree::Cancelled;
        }
        let path = child.path();
        let ft = match child.file_type() {
            Ok(ft) => ft,
            Err(e) => return Tree::Failed(e),
        };
        // `is_dir` is false for a symlink / junction: those are removed as links.
        if ft.is_dir() {
            match remove_tree(&path, w) {
                Tree::Done => {}
                other => return other,
            }
        } else if let Err(e) = remove_leaf(&path, ft.is_symlink()) {
            return Tree::Failed(e);
        } else {
            w.removed_one();
        }
    }
    if (w.cancelled)() {
        return Tree::Cancelled;
    }
    match fs::remove_dir(dir) {
        Ok(()) => {
            w.removed_one();
            Tree::Done
        }
        Err(e) => Tree::Failed(e),
    }
}

fn run(
    paths: &[String],
    mode: &Mode,
    cancelled: &dyn Fn() -> bool,
    progress: &mut dyn FnMut(&DeleteProgress),
) -> DeleteJobResult {
    let total = paths.len();
    let mut report = DeleteReport {
        requested: total,
        deleted: 0,
        failed: Vec::new(),
    };
    let mut not_processed: Vec<String> = Vec::new();
    let mut partial: Vec<String> = Vec::new();
    let mut was_cancelled = false;
    let mut state = DeleteProgress {
        done: 0,
        total,
        current_name: None,
        entries: 0,
    };
    progress(&state);

    for (i, p) in paths.iter().enumerate() {
        if cancelled() {
            was_cancelled = true;
            not_processed.extend(paths[i..].iter().cloned());
            break;
        }
        state.done = i;
        state.current_name = Some(last_name(p));
        progress(&state);

        // FER-559 (N4): a reserved Windows name cannot go to the Recycle Bin; say so (no existence
        // probe: the name alone decides, and it is never retried through the plain spelling).
        if matches!(mode, Mode::Trash(_))
            && crate::path_guard::has_reserved_device_component(&crate::path_guard::strip_verbatim(
                p,
            ))
        {
            report.failed.push(failed(
                p,
                ErrorCode::Unsupported,
                "The Recycle Bin cannot hold an item with a reserved Windows name (nul, con, aux ...). Use permanent delete (Shift+Delete) instead."
                    .into(),
            ));
            continue;
        }
        let path = Path::new(p);
        let meta = fs::symlink_metadata(path);
        if !path.exists() && meta.is_err() {
            report
                .failed
                .push(failed(p, ErrorCode::NotFound, "Entry not found".into()));
            continue;
        }
        match mode {
            Mode::Trash(remover) => match remover(path) {
                Ok(()) => {
                    report.deleted += 1;
                    state.entries += 1;
                }
                Err(e) => {
                    let fe = crate::domain::FsError::from(e).with_path(p.clone());
                    report.failed.push(failed(p, fe.code, fe.message));
                }
            },
            Mode::Permanent => {
                let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
                if !is_dir {
                    let is_link = meta.as_ref().map(|m| m.is_symlink()).unwrap_or(false);
                    match remove_leaf(path, is_link) {
                        Ok(()) => {
                            report.deleted += 1;
                            state.entries += 1;
                        }
                        Err(e) => {
                            let fe = FsError::from(e).with_path(p.clone());
                            report.failed.push(failed(p, fe.code, fe.message));
                        }
                    }
                    continue;
                }
                let mut walk = Walk {
                    cancelled,
                    progress: &mut *progress,
                    state: &mut state,
                    removed_here: 0,
                };
                let outcome = remove_tree(path, &mut walk);
                let removed_here = walk.removed_here;
                match outcome {
                    Tree::Done => report.deleted += 1,
                    Tree::Cancelled => {
                        was_cancelled = true;
                        if removed_here > 0 {
                            partial.push(p.clone());
                        } else {
                            not_processed.push(p.clone());
                        }
                        not_processed.extend(paths[i + 1..].iter().cloned());
                        break;
                    }
                    Tree::Failed(e) => {
                        let fe = FsError::from(e).with_path(p.clone());
                        let message = if removed_here > 0 {
                            format!(
                                "{} ({removed_here} entries inside were already deleted; the folder was left partly deleted)",
                                fe.message
                            )
                        } else {
                            fe.message
                        };
                        report.failed.push(failed(p, fe.code, message));
                    }
                }
            }
        }
    }
    state.done = if was_cancelled {
        total - not_processed.len() - partial.len()
    } else {
        total
    };
    state.current_name = None;
    progress(&state);
    DeleteJobResult {
        report,
        cancelled: was_cancelled,
        not_processed,
        partial,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::path::PathBuf;

    /// Declaration order matters: fields drop in order, so `root` (a view into the tempdir) goes
    /// first and the `TempDir` last.
    struct Fixture {
        root: PathBuf,
        _dir: tempfile::TempDir,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        // CI keeps %TEMP% under an 8.3 name (RUNNER~1): work with the canonical long form.
        let root = fs::canonicalize(dir.path()).unwrap();
        let root = root
            .to_string_lossy()
            .strip_prefix(r"\\?\")
            .map(PathBuf::from)
            .unwrap_or(root);
        Fixture { root, _dir: dir }
    }

    fn s(p: &Path) -> String {
        p.to_string_lossy().into_owned()
    }

    fn tree_with(root: &Path, name: &str, files: usize) -> PathBuf {
        let d = root.join(name);
        fs::create_dir_all(d.join("inner")).unwrap();
        for i in 0..files {
            fs::write(d.join(format!("f{i}.txt")), b"x").unwrap();
            fs::write(d.join("inner").join(format!("g{i}.txt")), b"y").unwrap();
        }
        d
    }

    fn never() -> bool {
        false
    }

    #[test]
    fn permanent_delete_reports_progress_and_removes_everything() {
        let fx = fixture();
        let file = fx.root.join("a.txt");
        fs::write(&file, b"a").unwrap();
        let dir = tree_with(&fx.root, "tree", 3);
        let mut seen: Vec<DeleteProgress> = Vec::new();
        let r = run_permanent(&[s(&file), s(&dir)], &never, &mut |p| seen.push(p.clone()));
        assert_eq!(r.report.requested, 2);
        assert_eq!(r.report.deleted, 2);
        assert!(r.report.failed.is_empty());
        assert!(!r.cancelled && r.not_processed.is_empty() && r.partial.is_empty());
        assert!(!file.exists() && !dir.exists());
        let first = seen.first().unwrap();
        assert_eq!((first.done, first.total), (0, 2));
        let last = seen.last().unwrap();
        assert_eq!((last.done, last.total), (2, 2));
        // 1 file + (3 + 3 files + `inner` + `tree`) entries.
        assert_eq!(last.entries, 1 + 3 + 3 + 2);
        assert!(
            seen.iter()
                .any(|p| p.current_name.as_deref() == Some("tree"))
        );
        assert!(seen.windows(2).all(|w| w[0].entries <= w[1].entries));
    }

    #[test]
    fn cancel_in_the_middle_of_a_big_folder_stops_and_reports_the_folder_as_partial() {
        let fx = fixture();
        let before = fx.root.join("before.txt");
        fs::write(&before, b"b").unwrap();
        let big = tree_with(&fx.root, "big", 20);
        let after = fx.root.join("after.txt");
        fs::write(&after, b"c").unwrap();
        let removed = Cell::new(0u64);
        let mut seen_entries = 0u64;
        let r = run_permanent(
            &[s(&before), s(&big), s(&after)],
            // Cancel once 10 entries are gone: deterministic, no timing.
            &|| removed.get() >= 10,
            &mut |p| {
                removed.set(p.entries);
                seen_entries = p.entries;
            },
        );
        assert!(r.cancelled);
        assert_eq!(
            r.report.deleted, 1,
            "only the first file is completely gone"
        );
        assert!(!before.exists());
        assert!(big.exists(), "the folder is only partly deleted");
        assert!(
            after.exists(),
            "an item the cancel never reached is untouched"
        );
        assert_eq!(r.partial, vec![s(&big)]);
        assert_eq!(r.not_processed, vec![s(&after)]);
        assert!(r.report.failed.is_empty());
        assert!((10..1 + 20 + 20 + 2).contains(&seen_entries));
        assert_eq!(r.remaining(), vec![s(&big), s(&after)]);
        // Nothing vanished without a report: every request is deleted, partial or not processed.
        assert_eq!(
            r.report.deleted + r.partial.len() + r.not_processed.len(),
            r.report.requested
        );
    }

    #[test]
    fn cancel_before_anything_leaves_every_item_untouched() {
        let fx = fixture();
        let a = fx.root.join("a.txt");
        let b = tree_with(&fx.root, "b", 2);
        fs::write(&a, b"a").unwrap();
        let r = run_permanent(&[s(&a), s(&b)], &|| true, &mut |_| {});
        assert!(r.cancelled);
        assert_eq!(r.report.deleted, 0);
        assert!(r.partial.is_empty());
        assert_eq!(r.not_processed, vec![s(&a), s(&b)]);
        assert!(a.exists() && b.exists());
    }

    #[test]
    fn a_cancel_right_at_the_start_of_a_folder_is_not_called_partial() {
        let fx = fixture();
        let a = fx.root.join("a.txt");
        fs::write(&a, b"a").unwrap();
        let big = tree_with(&fx.root, "big", 3);
        // Let the first item pass, cancel as soon as the folder starts (no entry removed yet).
        let polls = Cell::new(0);
        let r = run_permanent(
            &[s(&a), s(&big)],
            &|| {
                polls.set(polls.get() + 1);
                polls.get() >= 3
            },
            &mut |_| {},
        );
        assert!(r.cancelled);
        assert!(r.partial.is_empty());
        assert_eq!(r.not_processed, vec![s(&big)]);
        assert!(big.join("f0.txt").exists());
    }

    #[test]
    fn missing_items_are_failures_and_the_batch_goes_on() {
        let fx = fixture();
        let keep = fx.root.join("k.txt");
        fs::write(&keep, b"k").unwrap();
        let r = run_permanent(
            &[s(&fx.root.join("ghost.txt")), s(&keep)],
            &never,
            &mut |_| {},
        );
        assert_eq!(r.report.deleted, 1);
        assert_eq!(r.report.failed.len(), 1);
        assert_eq!(r.report.failed[0].code, ErrorCode::NotFound);
        assert!(!keep.exists());
    }

    #[test]
    fn read_only_files_inside_a_folder_are_deleted() {
        let fx = fixture();
        let dir = tree_with(&fx.root, "ro", 1);
        let f = dir.join("f0.txt");
        let mut perms = fs::metadata(&f).unwrap().permissions();
        perms.set_readonly(true);
        fs::set_permissions(&f, perms).unwrap();
        let r = run_permanent(&[s(&dir)], &never, &mut |_| {});
        assert_eq!(r.report.deleted, 1, "failed: {:?}", r.report.failed);
        assert!(!dir.exists());
    }

    #[test]
    fn trash_goes_item_by_item_and_cancel_stops_between_items() {
        let fx = fixture();
        let a = fx.root.join("a.txt");
        let b = fx.root.join("b.txt");
        let c = fx.root.join("c.txt");
        for p in [&a, &b, &c] {
            fs::write(p, b"x").unwrap();
        }
        let calls = Cell::new(0);
        let r = run_trash_with(
            &[s(&a), s(&b), s(&c)],
            &|p| {
                calls.set(calls.get() + 1);
                fs::remove_file(p) // stands in for the Recycle Bin
            },
            &|| calls.get() >= 2,
            &mut |_| {},
        );
        assert!(r.cancelled);
        assert_eq!(
            calls.get(),
            2,
            "one remover call per item, none after the cancel"
        );
        assert_eq!(r.report.deleted, 2);
        assert_eq!(r.not_processed, vec![s(&c)]);
        assert!(r.partial.is_empty());
        assert!(c.exists());
    }

    #[test]
    fn trash_reports_a_remover_failure_and_never_falls_back_to_permanent() {
        let fx = fixture();
        let a = fx.root.join("a.txt");
        fs::write(&a, b"x").unwrap();
        let r = run_trash_with(
            &[s(&a)],
            &|_| Err(std::io::Error::other("bin full")),
            &never,
            &mut |_| {},
        );
        assert_eq!(r.report.deleted, 0);
        assert_eq!(r.report.failed.len(), 1);
        assert!(
            a.exists(),
            "a failed Recycle Bin move must not delete for good"
        );
    }

    #[test]
    fn trash_refuses_a_reserved_windows_name_with_the_permanent_hint() {
        let fx = fixture();
        let reserved = format!("{}{}nul", fx.root.display(), std::path::MAIN_SEPARATOR);
        let called = Cell::new(false);
        let r = run_trash_with(
            &[reserved],
            &|_| {
                called.set(true);
                Ok(())
            },
            &never,
            &mut |_| {},
        );
        assert!(!called.get(), "the Recycle Bin is never asked");
        assert_eq!(r.report.deleted, 0);
        assert_eq!(r.report.failed.len(), 1);
        assert_eq!(r.report.failed[0].code, ErrorCode::Unsupported);
        assert!(r.report.failed[0].message.contains("permanent delete"));
    }

    #[test]
    fn real_trash_call_moves_a_file_out_of_the_folder() {
        // Host check (Windows Recycle Bin), like mutations::trash_delete_removes_the_entry.
        let fx = fixture();
        let a = fx.root.join("gone.txt");
        fs::write(&a, b"x").unwrap();
        let r = run_trash(&[s(&a)], &never, &mut |_| {});
        assert_eq!(r.report.deleted, 1, "failed: {:?}", r.report.failed);
        assert!(!a.exists());
    }
}
