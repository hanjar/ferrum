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

/// Write a versioned selection file (overwrite is the user's explicit
/// choice of target path, same as any editor save).
pub fn save_selection_file(file_path: &str, dir: &str, names: &[String]) -> Result<(), FsError> {
    validate_path(file_path)?;
    validate_path(dir)?;
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
    fs::write(file_path, json).map_err(|e| FsError::from(e).with_path(file_path))?;
    Ok(())
}

/// Read + strictly validate a selection file.
pub fn load_selection_file(file_path: &str) -> Result<SelectionFile, FsError> {
    validate_path(file_path)?;
    let meta = fs::metadata(file_path).map_err(|e| FsError::from(e).with_path(file_path))?;
    if meta.len() > MAX_FILE_BYTES {
        return Err(FsError::new(
            crate::domain::ErrorCode::InvalidPath,
            format!("Selection file too large ({} bytes)", meta.len()),
        )
        .with_path(file_path));
    }
    let raw = fs::read_to_string(file_path).map_err(|e| FsError::from(e).with_path(file_path))?;
    let doc: SelectionFile = serde_json::from_str(&raw).map_err(|e| {
        FsError::new(
            crate::domain::ErrorCode::InvalidPath,
            format!("Not a valid selection file: {e}"),
        )
        .with_path(file_path)
    })?;
    if doc.version != SELECTION_VERSION {
        return Err(FsError::new(
            crate::domain::ErrorCode::InvalidPath,
            format!("Unsupported selection file version {}", doc.version),
        )
        .with_path(file_path));
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
        .with_path(file_path));
    }
    for n in &doc.names {
        validate_name(n).map_err(|e| e.with_path(file_path))?;
    }
    Ok(doc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQ: AtomicU64 = AtomicU64::new(0);

    fn temp_path(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ferrum-f214-{tag}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_file(&dir);
        dir
    }

    #[test]
    fn save_then_load_round_trips() {
        let path = temp_path("roundtrip");
        let path_str = path.to_str().unwrap();
        save_selection_file(
            path_str,
            r"C:\Projects",
            &["békér.txt".into(), "dossier ünicode".into()],
        )
        .expect("save ok");
        let doc = load_selection_file(path_str).expect("load ok");
        assert_eq!(doc.version, 1);
        assert_eq!(doc.path, r"C:\Projects");
        assert_eq!(doc.names, vec!["békér.txt", "dossier ünicode"]);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn saved_json_is_versioned_camel_case() {
        let path = temp_path("shape");
        save_selection_file(path.to_str().unwrap(), "C:\\d", &["a.txt".into()]).unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["version"], 1);
        assert_eq!(v["path"], "C:\\d");
        assert_eq!(v["names"][0], "a.txt");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn rejects_names_that_are_not_basenames() {
        let path = temp_path("badnames");
        let p = path.to_str().unwrap().to_string();
        for bad in ["..", ".", "a\\b", "a/b", "a\0b", ""] {
            let err = save_selection_file(&p, "C:\\d", &[bad.to_string()]).unwrap_err();
            assert_eq!(err.code, crate::domain::ErrorCode::InvalidPath, "{bad:?}");
            assert!(!path.exists(), "nothing written on invalid input");
        }
    }

    #[test]
    fn rejects_loading_hostile_or_foreign_documents() {
        let path = temp_path("hostile");
        // traversal name
        fs::write(
            &path,
            r#"{"version":1,"path":"C:\\d","names":["..\\evil"]}"#,
        )
        .unwrap();
        let err = load_selection_file(path.to_str().unwrap()).unwrap_err();
        assert_eq!(err.code, crate::domain::ErrorCode::InvalidPath);
        // wrong version
        fs::write(&path, r#"{"version":99,"path":"C:\\d","names":[]}"#).unwrap();
        assert!(load_selection_file(path.to_str().unwrap()).is_err());
        // not JSON
        fs::write(&path, "garbage").unwrap();
        assert!(load_selection_file(path.to_str().unwrap()).is_err());
        // absolute path smuggled as a name
        fs::write(
            &path,
            r#"{"version":1,"path":"C:\\d","names":["C:\\Windows\\evil"]}"#,
        )
        .unwrap();
        assert!(load_selection_file(path.to_str().unwrap()).is_err());
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn missing_file_maps_to_not_found() {
        let err = load_selection_file(r"Z:\definitely\not\here.sel").unwrap_err();
        assert_eq!(err.code, crate::domain::ErrorCode::NotFound);
    }

    #[test]
    fn structural_garbage_is_rejected() {
        assert!(save_selection_file("", "C:\\d", &["a".into()]).is_err());
        assert!(save_selection_file("C:\\t\\s.json", "", &["a".into()]).is_err());
        assert!(load_selection_file("  ").is_err());
    }
}
