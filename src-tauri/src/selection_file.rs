//! Selection save/load files (F2-14). Format is a versioned JSON document:
//!
//! ```json
//! {"version": 1, "path": "C:\\dir", "names": ["a.txt", "b.txt"]}
//! ```
//!
//! Kept free of Tauri types so it stays unit-testable; `ipc.rs` exposes the
//! two thin commands. Loading validates strictly: names must be plain
//! basenames (no separators, no traversal, no NUL) — a hostile file must be
//! rejected, not sanitized.

use crate::domain::{FsError, validate_path};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// See module docs. `version` guards forward compatibility.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectionFile {
    pub version: u32,
    /// Directory the names were captured in (informational).
    pub path: String,
    /// Basenames only — never absolute paths, never '..'.
    pub names: Vec<String>,
}

const SELECTION_VERSION: u32 = 1;
/// Guards against pathological files: 16 MiB is far beyond any real
/// selection document yet keeps memory bounded on hostile input.
const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_NAMES: usize = 1_000_000;

/// FER-512 P3-4: the ONLY place a selection file may live is `dir` (a full
/// drive/UNC path that passes the same `path_guard::check_str` as content
/// reads) + a single basename (`name`, validated exactly like `create_entry`).
/// The webview can no longer name an arbitrary file path.
fn selection_path(dir: &str, name: &str) -> Result<PathBuf, FsError> {
    let dir = crate::path_guard::check_str(dir)?;
    crate::mutations::validate_name(name)?;
    Ok(Path::new(&dir).join(name))
}

fn validate_name(name: &str) -> Result<(), FsError> {
    if name.is_empty() {
        return Err(FsError::new(
            crate::domain::ErrorCode::InvalidPath,
            "Selection name must not be empty",
        ));
    }
    if name.contains(['/', '\\', '\0']) || name == "." || name == ".." {
        return Err(FsError::new(
            crate::domain::ErrorCode::InvalidPath,
            format!("Selection name must be a plain basename: {name:?}"),
        ));
    }
    Ok(())
}

/// Write a versioned selection file to `check_str(dir)\name` (name = single
/// basename, see `selection_path`; overwriting an existing selection in that
/// folder is the user's explicit choice, same as any editor save).
pub fn save_selection_file(dir: &str, name: &str, names: &[String]) -> Result<(), FsError> {
    let target = selection_path(dir, name)?;
    if names.len() > MAX_NAMES {
        return Err(FsError::new(
            crate::domain::ErrorCode::InvalidPath,
            format!(
                "Too many names in selection ({} > {MAX_NAMES})",
                names.len()
            ),
        ));
    }
    for n in names {
        validate_name(n)?;
    }
    let doc = SelectionFile {
        version: SELECTION_VERSION,
        path: dir.to_string(),
        names: names.to_vec(),
    };
    let json = serde_json::to_string_pretty(&doc)
        .map_err(|e| FsError::new(crate::domain::ErrorCode::Io, e.to_string()))?;
    fs::write(&target, json)
        .map_err(|e| FsError::from(e).with_path(target.to_string_lossy().into_owned()))?;
    Ok(())
}

/// Read + strictly validate the selection file at `check_str(dir)\name`.
pub fn load_selection_file(dir: &str, name: &str) -> Result<SelectionFile, FsError> {
    let target = selection_path(dir, name)?;
    let target = target.to_string_lossy().into_owned();
    let meta = fs::metadata(&target).map_err(|e| FsError::from(e).with_path(&target))?;
    if meta.len() > MAX_FILE_BYTES {
        return Err(FsError::new(
            crate::domain::ErrorCode::InvalidPath,
            format!("Selection file too large ({} bytes)", meta.len()),
        )
        .with_path(&target));
    }
    let raw = fs::read_to_string(&target).map_err(|e| FsError::from(e).with_path(&target))?;
    let doc: SelectionFile = serde_json::from_str(&raw).map_err(|e| {
        FsError::new(
            crate::domain::ErrorCode::InvalidPath,
            format!("Not a valid selection file: {e}"),
        )
        .with_path(&target)
    })?;
    if doc.version != SELECTION_VERSION {
        return Err(FsError::new(
            crate::domain::ErrorCode::InvalidPath,
            format!("Unsupported selection file version {}", doc.version),
        )
        .with_path(&target));
    }
    validate_path(&doc.path)?;
    if doc.names.len() > MAX_NAMES {
        return Err(FsError::new(
            crate::domain::ErrorCode::InvalidPath,
            format!(
                "Too many names in selection ({} > {MAX_NAMES})",
                doc.names.len()
            ),
        )
        .with_path(&target));
    }
    for n in &doc.names {
        validate_name(n).map_err(|e| e.with_path(&target))?;
    }
    Ok(doc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQ: AtomicU64 = AtomicU64::new(0);

    /// Fresh temp folder; the selection file inside it is always
    /// `<dir>\selection.json`.
    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ferrum-f214-{tag}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn dir_str(tag: &str) -> String {
        temp_dir(tag).to_str().unwrap().to_string()
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = dir_str("roundtrip");
        save_selection_file(
            &dir,
            "selection.json",
            &["békér.txt".into(), "dossier ünicode".into()],
        )
        .expect("save ok");
        let doc = load_selection_file(&dir, "selection.json").expect("load ok");
        assert_eq!(doc.version, 1);
        assert_eq!(doc.path, dir);
        assert_eq!(doc.names, vec!["békér.txt", "dossier ünicode"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn saved_json_is_versioned_camel_case() {
        let dir = dir_str("shape");
        save_selection_file(&dir, "selection.json", &["a.txt".into()]).unwrap();
        let raw = fs::read_to_string(Path::new(&dir).join("selection.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["version"], 1);
        assert_eq!(v["path"], dir);
        assert_eq!(v["names"][0], "a.txt");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_names_that_are_not_basenames() {
        let dir = dir_str("badnames");
        for bad in ["..", ".", "a\\b", "a/b", "a\0b", ""] {
            let err = save_selection_file(&dir, bad, &["a.txt".into()]).unwrap_err();
            assert_eq!(err.code, crate::domain::ErrorCode::InvalidPath, "{bad:?}");
            assert!(
                !Path::new(&dir).join("selection.json").exists(),
                "nothing written on invalid input ({bad:?})"
            );
        }
        let _ = fs::remove_dir_all(&dir);
    }

    /// FER-512 P3-4: traversal and separator names for the FILE are refused, so
    /// the target is always `dir\<basename>`.
    #[test]
    fn rejects_traversal_file_names() {
        let dir = dir_str("traversal");
        for bad in [r"..\evil.json", r"sub\evil.json", "..", "."] {
            let err = save_selection_file(&dir, bad, &["a.txt".into()]).unwrap_err();
            assert_eq!(err.code, crate::domain::ErrorCode::InvalidPath, "{bad:?}");
            assert!(load_selection_file(&dir, bad).is_err(), "{bad:?}");
        }
        // Nothing was written anywhere in (or above) the folder.
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
        let _ = fs::remove_dir_all(&dir);
    }

    /// FER-512 P3-4: the folder itself passes `path_guard::check_str` — device
    /// namespaces and ADS spellings are refused before anything is touched, a
    /// plain UNC folder is NOT refused by the guard (a share is a legitimate
    /// place to work).
    #[test]
    fn rejects_unsafe_dirs_but_keeps_unc() {
        for bad_dir in [r"\\.\C:", r"\\?\C:\x", r"C:\data.txt:ads", "relative", ""] {
            let err = save_selection_file(bad_dir, "s.json", &["a".into()]).unwrap_err();
            assert_eq!(
                err.code,
                crate::domain::ErrorCode::InvalidPath,
                "{bad_dir:?}"
            );
        }
        for good in [r"\\server\share\sel.json", r"C:\Users\u\sel.json"] {
            assert!(
                crate::path_guard::check_str(good).is_ok(),
                "the guard must keep {good}"
            );
        }
        // A guarded but missing folder fails on the filesystem, not on the guard.
        let base = temp_dir("missing");
        let missing = base
            .parent()
            .unwrap()
            .join("ferrum-f214-not-here-512")
            .to_string_lossy()
            .into_owned();
        let _ = fs::remove_dir_all(&base);
        let err = save_selection_file(&missing, "s.json", &["a".into()]).unwrap_err();
        assert_ne!(err.code, crate::domain::ErrorCode::InvalidPath, "{err:?}");
    }

    #[test]
    fn rejects_loading_hostile_or_foreign_documents() {
        let dir = dir_str("hostile");
        let path = Path::new(&dir).join("selection.json");
        // traversal name
        fs::write(
            &path,
            r#"{"version":1,"path":"C:\\d","names":["..\\evil"]}"#,
        )
        .unwrap();
        let err = load_selection_file(&dir, "selection.json").unwrap_err();
        assert_eq!(err.code, crate::domain::ErrorCode::InvalidPath);
        // wrong version
        fs::write(&path, r#"{"version":99,"path":"C:\\d","names":[]}"#).unwrap();
        assert!(load_selection_file(&dir, "selection.json").is_err());
        // not JSON
        fs::write(&path, "garbage").unwrap();
        assert!(load_selection_file(&dir, "selection.json").is_err());
        // absolute path smuggled as a name
        fs::write(
            &path,
            r#"{"version":1,"path":"C:\\d","names":["C:\\Windows\\evil"]}"#,
        )
        .unwrap();
        assert!(load_selection_file(&dir, "selection.json").is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_maps_to_not_found() {
        let err = load_selection_file(r"Z:\definitely\not", "here.sel").unwrap_err();
        assert_eq!(err.code, crate::domain::ErrorCode::NotFound);
    }

    #[test]
    fn structural_garbage_is_rejected() {
        assert!(save_selection_file("", "s.json", &["a".into()]).is_err());
        assert!(save_selection_file("C:\\t", "", &["a".into()]).is_err());
        assert!(load_selection_file("  ", "s.json").is_err());
        assert!(load_selection_file("C:\\t", "  ").is_err());
    }
}
