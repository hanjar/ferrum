//! F2-27: file comments (`Edit Comment` / `Show > Comments`).
//!
//! Storage contract (recorded in docs/decisions.md, D34):
//! - Comments live in a per-directory sidecar `<dir>/.ferrum/comments.json`,
//!   a versioned JSON map of file NAME → comment text. Not NTFS ADS: ADS
//!   silently disappears on FAT/exFAT/network copies, archive extraction
//!   and most sync tools; a plain file travels wherever the folder goes.
//! - Keys are names WITHIN that directory. Renaming a file migrates its
//!   comment (`migrate_on_rename`, best-effort, hooked into
//!   `mutations::rename_entry`). Moving a file to another directory loses
//!   the comment under the old folder — an honest, documented limitation
//!   (the sidecar cannot follow a file it is not named after).
//! - The sidecar is user data like any other file: it shows up in listings
//!   (a hidden `.ferrum` entry), can be copied/backed up, and never blocks
//!   deleting the commented files themselves.
//!
//! All writes are atomic (temp + rename) and the sidecar is removed when
//! the map empties, so a directory with no comments is left clean.

use crate::domain::{ErrorCode, FsError};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

pub const SIDECAR_DIR: &str = ".ferrum";
pub const FILE_NAME: &str = "comments.json";
const SCHEMA_VERSION: i64 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Sidecar {
    version: i64,
    /// file name → comment text, sorted for stable diffs.
    comments: BTreeMap<String, String>,
}

/// A path is only usable as a sidecar parent when it is structurally sane.
fn sidecar_path(dir: &Path) -> std::path::PathBuf {
    dir.join(SIDECAR_DIR).join(FILE_NAME)
}

/// Split a full path into (parent, file name) for comment keys.
pub fn split_parent(path: &str) -> Option<(String, String)> {
    let trimmed = path.trim_end_matches(['/', '\\']);
    let i = trimmed.rfind(['/', '\\'])?;
    let (parent, name) = (&trimmed[..=i], &trimmed[i + 1..]);
    if name.is_empty() {
        return None;
    }
    Some((parent.to_string(), name.to_string()))
}

/// Read one directory's comment map. A missing (or unreadable/garbage)
/// sidecar means "no comments" — comments are decoration, never truth.
pub fn read_dir_comments(dir: &str) -> Result<BTreeMap<String, String>, FsError> {
    let raw = match fs::read_to_string(sidecar_path(Path::new(dir))) {
        Ok(s) => s,
        Err(_) => return Ok(BTreeMap::new()),
    };
    match serde_json::from_str::<Sidecar>(&raw) {
        Ok(sc) if sc.version == SCHEMA_VERSION => Ok(sc.comments),
        _ => Ok(BTreeMap::new()),
    }
}

/// The comment of one file, looked up in its parent directory's sidecar.
pub fn get_comment(path: &str) -> Result<Option<String>, FsError> {
    let Some((parent, name)) = split_parent(path) else {
        return Err(FsError::new(ErrorCode::InvalidPath, "Path has no file name").with_path(path));
    };
    Ok(read_dir_comments(&parent)?.get(&name).cloned())
}

/// Set or clear one file's comment. `None`/empty clears the entry; an empty
/// map removes the sidecar file so no stray state is left behind.
pub fn set_comment(path: &str, comment: Option<&str>) -> Result<(), FsError> {
    let Some((parent, name)) = split_parent(path) else {
        return Err(FsError::new(ErrorCode::InvalidPath, "Path has no file name").with_path(path));
    };
    if name.contains('\0') {
        return Err(
            FsError::new(ErrorCode::InvalidPath, "Name must not contain NUL").with_path(path),
        );
    }
    let mut comments = read_dir_comments(&parent)?;
    match comment.map(str::trim).filter(|c| !c.is_empty()) {
        Some(text) => {
            comments.insert(name, text.to_string());
        }
        None => {
            if comments.remove(&name).is_none() {
                return Ok(()); // clearing a comment that is not there: done
            }
        }
    }
    let dir = Path::new(&parent).join(SIDECAR_DIR);
    if comments.is_empty() {
        let _ = fs::remove_file(sidecar_path(Path::new(&parent))); // best-effort cleanup
        return Ok(());
    }
    fs::create_dir_all(&dir).map_err(|e| FsError::from(e).with_path(dir.to_string_lossy()))?;
    let body = serde_json::to_string_pretty(&Sidecar {
        version: SCHEMA_VERSION,
        comments,
    })
    .map_err(|e| FsError::new(ErrorCode::Io, e.to_string()))?;
    let tmp = dir.join(format!("{FILE_NAME}.tmp-{}", std::process::id()));
    fs::write(&tmp, &body).map_err(|e| FsError::from(e).with_path(tmp.to_string_lossy()))?;
    fs::rename(&tmp, sidecar_path(Path::new(&parent)))
        .map_err(|e| FsError::from(e).with_path(parent))
}

/// Move a comment after a rename (same directory only). Best-effort: a
/// failure here must never fail the rename it follows.
pub fn migrate_on_rename(old_path: &str, new_path: &str) {
    let (Some((old_parent, old_name)), Some((new_parent, new_name))) =
        (split_parent(old_path), split_parent(new_path))
    else {
        return;
    };
    if old_parent != new_parent || old_name == new_name {
        return;
    }
    let mut comments = match read_dir_comments(&old_parent) {
        Ok(c) => c,
        Err(_) => return,
    };
    if let Some(text) = comments.remove(&old_name) {
        comments.insert(new_name, text);
    } else {
        return;
    }
    let dir = Path::new(&old_parent).join(SIDECAR_DIR);
    if comments.is_empty() {
        let _ = fs::remove_file(sidecar_path(Path::new(&old_parent)));
        return;
    }
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    let body = match serde_json::to_string_pretty(&Sidecar {
        version: SCHEMA_VERSION,
        comments,
    }) {
        Ok(b) => b,
        Err(_) => return,
    };
    let tmp = dir.join(format!("{FILE_NAME}.tmp-{}", std::process::id()));
    if fs::write(&tmp, body).is_ok() {
        let _ = fs::rename(&tmp, sidecar_path(Path::new(&old_parent)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> String {
        let dir = std::env::temp_dir().join(format!(
            "ferrum-comments-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir.to_string_lossy().to_string()
    }

    fn parent_of(p: &str) -> String {
        let (parent, _) = split_parent(p).unwrap();
        parent
    }

    #[test]
    fn split_parent_handles_windows_unix_and_unc_paths() {
        assert_eq!(
            split_parent(r"C:\dir a\file.txt"),
            Some((r"C:\dir a\".into(), "file.txt".into()))
        );
        assert_eq!(
            split_parent("/home/arif/hé.txt"),
            Some(("/home/arif/".into(), "hé.txt".into()))
        );
        assert_eq!(
            split_parent(r"\\srv\share\my notes\draft v2.md"),
            Some((r"\\srv\share\my notes\".into(), "draft v2.md".into()))
        );
        assert_eq!(split_parent(r"C:\"), None); // no name to key on
        assert_eq!(split_parent("name-only"), None);
    }

    #[test]
    fn missing_sidecar_reads_as_no_comments() {
        let dir = temp("missing");
        assert_eq!(read_dir_comments(&dir).unwrap().len(), 0);
        assert_eq!(get_comment(&format!("{dir}\\a.txt")).unwrap(), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn garbage_sidecar_reads_as_no_comments() {
        let dir = temp("garbage");
        let sc = Path::new(&dir).join(SIDECAR_DIR);
        fs::create_dir_all(&sc).unwrap();
        fs::write(sc.join(FILE_NAME), "{not json").unwrap();
        assert_eq!(read_dir_comments(&dir).unwrap().len(), 0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn set_then_clear_round_trips_and_cleans_up() {
        let dir = temp("roundtrip");
        let file = format!("{dir}\\report draft.txt");
        set_comment(&file, Some("Draft for Q4 — vérify numbers")).unwrap();
        assert_eq!(
            get_comment(&file).unwrap().as_deref(),
            Some("Draft for Q4 — vérify numbers")
        );
        // Written sidecar is valid JSON of the documented shape.
        let raw = fs::read_to_string(sidecar_path(Path::new(&parent_of(&file)))).unwrap();
        assert!(raw.contains("\"version\": 1"));
        assert!(raw.contains("\"report draft.txt\""));

        set_comment(&file, None).unwrap();
        assert_eq!(get_comment(&file).unwrap(), None);
        assert!(!sidecar_path(Path::new(&parent_of(&file))).exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn clearing_a_missing_comment_is_a_no_op() {
        let dir = temp("noop");
        set_comment(&format!("{dir}\\a.txt"), None).unwrap();
        assert!(!Path::new(&dir).join(SIDECAR_DIR).exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_comment_text_clears_instead_of_storing() {
        let dir = temp("empty-text");
        let file = format!("{dir}\\a.txt");
        set_comment(&file, Some("   ")).unwrap();
        assert_eq!(get_comment(&file).unwrap(), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn two_files_share_one_sidecar_and_removal_keeps_the_other() {
        let dir = temp("two");
        let a = format!("{dir}\\a.txt");
        let b = format!("{dir}\\b.txt");
        set_comment(&a, Some("first")).unwrap();
        set_comment(&b, Some("second")).unwrap();
        assert_eq!(read_dir_comments(&dir).unwrap().len(), 2);
        set_comment(&a, None).unwrap();
        assert_eq!(get_comment(&b).unwrap().as_deref(), Some("second"));
        assert_eq!(get_comment(&a).unwrap(), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rename_within_a_directory_migrates_the_comment() {
        let dir = temp("migrate");
        let old = format!("{dir}\\old name.txt");
        let new = format!("{dir}\\new name.txt");
        set_comment(&old, Some("follows the file")).unwrap();
        migrate_on_rename(&old, &new);
        assert_eq!(
            get_comment(&new).unwrap().as_deref(),
            Some("follows the file")
        );
        assert_eq!(get_comment(&old).unwrap(), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rename_without_a_comment_and_across_directories_is_ignored() {
        let dir = temp("ignore");
        let other = format!("{}other", parent_of(&format!("{dir}\\")));
        let _ = fs::create_dir_all(&other);
        let a = format!("{dir}\\a.txt");
        set_comment(&a, Some("stays")).unwrap();
        // No comment on the source: no sidecar write.
        migrate_on_rename(&format!("{dir}\\none.txt"), &format!("{dir}\\renamed.txt"));
        // Cross-directory moves are documented as NOT migrated.
        migrate_on_rename(&a, &format!("{other}\\a.txt"));
        assert_eq!(get_comment(&a).unwrap().as_deref(), Some("stays"));
        assert_eq!(get_comment(&format!("{other}\\a.txt")).unwrap(), None);
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&other);
    }
}
