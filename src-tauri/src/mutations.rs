//! Filesystem mutations: create / rename / delete (D05: trash by default,
//! permanent only on explicit request). Kept Tauri-free for unit testing.
//!
//! MVP (D20): these run as direct commands with per-item reports; they are
//! not queued. Directory trees can make deletes slow — if that bites in
//! practice, they move onto the task-09 queue without contract changes.

use crate::domain::FsError;
use crate::domain::{ErrorCode, FileEntry};
use std::fs;
use std::path::{Path, PathBuf};

/// A name is valid when it is a single path segment: non-empty, no
/// separators, no NUL, not "." or "..". No other policy is imposed.
pub fn validate_name(name: &str) -> Result<(), FsError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(FsError::new(
            ErrorCode::InvalidPath,
            "Name must not be empty",
        ));
    }
    if name.contains('\0') || name.contains('/') || name.contains('\\') {
        return Err(FsError::new(
            ErrorCode::InvalidPath,
            "Name must not contain separators or NUL",
        ));
    }
    if name == "." || name == ".." {
        return Err(FsError::new(
            ErrorCode::InvalidPath,
            "Name must not be '.' or '..'",
        ));
    }
    Ok(())
}

/// Stricter, platform-independent policy for batch renames (F2-20): mirrors
/// `isValidTargetName` in `multiRename.ts` so the executor refuses what the
/// preview already flags. Adds Windows reserved device names (the stem before
/// the first dot, so `CON.tar.gz` is caught too), `<>:"|?*`, control chars and
/// a trailing dot/space. Deliberately NOT applied to `validate_name` itself:
/// create/rename of a single entry must keep accepting names that exist legally
/// on Unix filesystems.
pub fn validate_portable_name(name: &str) -> Result<(), FsError> {
    validate_name(name)?;
    let bad = |msg: &str| Err(FsError::new(ErrorCode::InvalidPath, msg));
    if name
        .chars()
        .any(|c| (c as u32) < 32 || matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*'))
    {
        return bad("Name contains characters that are not allowed on Windows");
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return bad("Name must not end with a dot or space");
    }
    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.len() == 4)
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'));
    if reserved {
        return bad("Name is a reserved Windows device name");
    }
    Ok(())
}

fn join_name(dir: &str, name: &str) -> PathBuf {
    Path::new(dir).join(name)
}

fn io_err(e: std::io::Error, path: &str) -> FsError {
    let mut err = FsError::from(e);
    if err.code == ErrorCode::Io {
        err.code = ErrorCode::Io; // keep granular kind mapping from From
    }
    err.path = Some(path.to_string());
    err
}

/// Create an empty file; fails with AlreadyExists if the name is taken.
pub fn create_file(dir: &str, name: &str) -> Result<FileEntry, FsError> {
    validate_name(name)?;
    let target = join_name(dir, name);
    if target.exists() {
        return Err(
            FsError::new(ErrorCode::AlreadyExists, "A file with this name exists")
                .with_path(target.to_string_lossy()),
        );
    }
    fs::File::create(&target).map_err(|e| io_err(e, &target.to_string_lossy()))?;
    entry_of(&target)
}

/// Create a directory; fails with AlreadyExists if the name is taken.
pub fn create_dir(dir: &str, name: &str) -> Result<FileEntry, FsError> {
    validate_name(name)?;
    let target = join_name(dir, name);
    match fs::create_dir(&target) {
        Ok(()) => entry_of(&target),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(FsError::new(
            ErrorCode::AlreadyExists,
            "Something with this name exists",
        )
        .with_path(target.to_string_lossy())),
        Err(e) => Err(io_err(e, &target.to_string_lossy())),
    }
}

/// Rename one entry within its parent directory (new_name is a bare name).
pub fn rename_entry(path: &str, new_name: &str) -> Result<FileEntry, FsError> {
    validate_name(new_name)?;
    let old = Path::new(path);
    if !old.exists() {
        return Err(FsError::new(ErrorCode::NotFound, "Entry not found").with_path(path));
    }
    let parent = old.parent().ok_or_else(|| {
        FsError::new(ErrorCode::InvalidPath, "Cannot rename a root").with_path(path)
    })?;
    let target = parent.join(new_name);
    if target != old && target.exists() {
        return Err(
            FsError::new(ErrorCode::AlreadyExists, "Target name is taken")
                .with_path(target.to_string_lossy()),
        );
    }
    fs::rename(old, &target).map_err(|e| io_err(e, path))?;
    // F2-27: a file's comment (per-directory sidecar) follows the rename.
    // Best-effort — a comment is decoration and must never fail a rename.
    crate::comments::migrate_on_rename(path, &target.to_string_lossy());
    entry_of(&target)
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteReport {
    pub requested: usize,
    pub deleted: usize,
    pub failed: Vec<FailedItem>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FailedItem {
    pub path: String,
    pub code: ErrorCode,
    pub message: String,
}

/// Delete entries to the Recycle Bin / Trash (D05). Per-item failures never
/// abort the batch.
pub fn delete_to_trash(paths: &[String]) -> DeleteReport {
    delete_impl(paths, &|p| {
        trash::delete(p).map_err(|e| std::io::Error::other(e.to_string()))
    })
}

/// Permanently delete (explicit user intent only — the UI must confirm).
pub fn delete_permanent(paths: &[String]) -> DeleteReport {
    delete_impl(paths, &|p| {
        let meta = fs::symlink_metadata(p)?;
        if meta.is_dir() {
            fs::remove_dir_all(p)
        } else {
            fs::remove_file(p)
        }
    })
}

fn delete_impl(paths: &[String], remover: &dyn Fn(&Path) -> std::io::Result<()>) -> DeleteReport {
    let mut report = DeleteReport {
        requested: paths.len(),
        deleted: 0,
        failed: Vec::new(),
    };
    for p in paths {
        let path = Path::new(p);
        if !path.exists() && fs::symlink_metadata(path).is_err() {
            report.failed.push(FailedItem {
                path: p.clone(),
                code: ErrorCode::NotFound,
                message: "Entry not found".into(),
            });
            continue;
        }
        match remover(path) {
            Ok(()) => report.deleted += 1,
            Err(e) => {
                let fe = FsError::from(e).with_path(p.clone());
                report.failed.push(FailedItem {
                    path: p.clone(),
                    code: fe.code,
                    message: fe.message,
                });
            }
        }
    }
    report
}

fn entry_of(path: &Path) -> Result<FileEntry, FsError> {
    let p = path.to_string_lossy().into_owned();
    let lm = fs::symlink_metadata(path).map_err(|e| io_err(e, &p))?;
    Ok(FileEntry {
        name: path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| p.clone()),
        path: p,
        is_dir: lm.is_dir(),
        is_symlink: lm.is_symlink(),
        size: if lm.is_dir() { 0 } else { lm.len() },
        modified_ms: lm
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0),
        hidden: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQ: AtomicU64 = AtomicU64::new(0);

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ferrum-t10-{tag}-{}-{}",
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

    #[test]
    fn name_validation_rejects_structural_garbage_only() {
        assert!(validate_name("notes v2.txt").is_ok());
        assert!(validate_name("ünïcode").is_ok());
        for bad in ["", "  ", "a/b", "a\\b", ".", "..", "a\0b"] {
            assert!(validate_name(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn portable_name_validation_adds_windows_rules() {
        for ok in [
            "notes v2.txt",
            "ünïcode",
            "日本語.md",
            "🎉.png",
            ".hidden",
            "console.txt",
            "COM10",
            "a b",
        ] {
            assert!(validate_portable_name(ok).is_ok(), "{ok:?} should pass");
        }
        for bad in [
            "",
            "  ",
            "a/b",
            "a\\b",
            ".",
            "..",
            "a\0b",
            "a:b",
            "a<b",
            "a>b",
            "a\"b",
            "a|b",
            "a?b",
            "a*b",
            "a\tb",
            "a\u{1f}b",
            "trail.",
            "trail ",
            "CON",
            "con",
            "Con.txt",
            "PRN.log",
            "AUX",
            "NUL.txt",
            "COM1",
            "com9.txt",
            "LPT1",
            "lpt9.tar.gz",
            "CON.tar.gz",
        ] {
            assert!(
                validate_portable_name(bad).is_err(),
                "{bad:?} should be rejected"
            );
        }
        // The single-entry policy stays permissive (Unix-legal names still work).
        assert!(validate_name("a:b").is_ok());
        assert!(validate_name("CON").is_ok());
    }

    #[test]
    fn create_file_and_dir_happy_path() {
        let root = temp("create");
        let f = create_file(&s(&root), "békér.txt").unwrap();
        assert_eq!(f.name, "békér.txt");
        assert_eq!(f.size, 0);
        assert!(!f.is_dir);
        let d = create_dir(&s(&root), "dossier").unwrap();
        assert!(d.is_dir);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn create_conflicts_map_to_already_exists() {
        let root = temp("conflict");
        create_file(&s(&root), "x.txt").unwrap();
        let err = create_file(&s(&root), "x.txt").unwrap_err();
        assert_eq!(err.code, ErrorCode::AlreadyExists);
        create_dir(&s(&root), "y").unwrap();
        // file vs dir name collision
        let err2 = create_file(&s(&root), "y").unwrap_err();
        assert_eq!(err2.code, ErrorCode::AlreadyExists);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn rename_moves_entry_and_conflicts_are_detected() {
        let root = temp("rename");
        create_file(&s(&root), "a.txt").unwrap();
        create_file(&s(&root), "b.txt").unwrap();
        let renamed = rename_entry(&s(&root.join("a.txt")), "renamed.txt").unwrap();
        assert_eq!(renamed.name, "renamed.txt");
        assert!(!root.join("a.txt").exists());
        assert!(root.join("renamed.txt").exists());
        let err = rename_entry(&s(&root.join("renamed.txt")), "b.txt").unwrap_err();
        assert_eq!(err.code, ErrorCode::AlreadyExists);
        // renaming to its own name is a no-op success
        assert!(rename_entry(&s(&root.join("b.txt")), "b.txt").is_ok());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn rename_missing_maps_to_not_found() {
        let root = temp("renmiss");
        let err = rename_entry(&s(&root.join("ghost.txt")), "new.txt").unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn permanent_delete_removes_files_and_trees_with_report() {
        let root = temp("perm");
        fs::write(root.join("f1.txt"), b"1").unwrap();
        fs::write(root.join("f2.txt"), b"2").unwrap();
        let sub = root.join("tree");
        fs::create_dir_all(sub.join("inner")).unwrap();
        fs::write(sub.join("inner").join("deep.txt"), b"3").unwrap();
        let paths = vec![s(&root.join("f1.txt")), s(&sub), s(&root.join("f2.txt"))];
        let report = delete_permanent(&paths);
        assert_eq!(report.deleted, 3);
        assert!(report.failed.is_empty());
        assert!(!root.join("f1.txt").exists());
        assert!(!sub.exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn delete_batch_survives_missing_items() {
        let root = temp("batch");
        fs::write(root.join("keep.txt"), b"k").unwrap();
        let paths = vec![s(&root.join("missing.txt")), s(&root.join("keep.txt"))];
        let report = delete_permanent(&paths);
        assert_eq!(report.deleted, 1);
        assert_eq!(report.failed.len(), 1);
        assert_eq!(report.failed[0].code, ErrorCode::NotFound);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn trash_delete_removes_the_entry() {
        // Host check (Windows Recycle Bin): we assert the entry leaves the
        // source directory without error; verifying bin CONTENTS needs shell
        // inspection and stays a manual check (docs/decisions.md D05).
        let root = temp("trash");
        fs::write(root.join("gone.txt"), b"x").unwrap();
        let report = delete_to_trash(&[s(&root.join("gone.txt"))]);
        assert_eq!(report.deleted, 1, "failed: {:?}", report.failed);
        assert!(!root.join("gone.txt").exists());
        let _ = fs::remove_dir_all(&root);
    }
}
