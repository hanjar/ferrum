//! FER-514 P3-7: the app's own state folders (`config.json`, `ai-setup.json`, `hotlist.json`,
//! `agent-cli.json`, the automation store, the activity database ...) must not be overwritten,
//! moved or deleted through the generic file operations a compromised webview can call.
//!
//! The Rust side registers the folders once at startup ([`register`]); the file-op commands in
//! `ipc.rs` then ask [`check_write_target`] (a destination or new entry: refused when it is inside a
//! protected folder) or [`check_write_source`] (something that is renamed, moved, deleted or
//! modified: refused when it is inside a protected folder OR contains one, since removing an
//! ancestor removes the state too).
//!
//! Comparison is on resolved paths: the deepest existing ancestor is canonicalised (junctions and
//! symlinks resolve), the not-yet-existing tail is appended, case is folded, `\\?\` is stripped.
//! Reads (browsing, copying a file OUT of the folder) are deliberately not restricted.

use crate::domain::{ErrorCode, FsError};
use std::path::{Component, Path, PathBuf};
use std::sync::RwLock;

static ROOTS: RwLock<Vec<PathBuf>> = RwLock::new(Vec::new());

/// Replace the protected set. Called once from `lib.rs` setup. A folder that does not exist yet
/// is still protected (matched by its resolved ancestor plus the remaining names), but it is
/// logged, because the match is then only lexical until the folder is created.
pub fn register(dirs: impl IntoIterator<Item = PathBuf>) {
    let resolved: Vec<PathBuf> = dirs
        .into_iter()
        .map(|d| {
            if std::fs::canonicalize(&d).is_err() {
                log::warn!(
                    "protected folder {} cannot be resolved yet; matching it by name only",
                    d.display()
                );
            }
            resolve(&d)
        })
        .collect();
    // A poisoned lock still takes the new value (and is healed by it).
    let mut roots = ROOTS.write().unwrap_or_else(|e| e.into_inner());
    *roots = resolved;
    drop(roots);
    ROOTS.clear_poison();
}

/// Tests that register roots share the process-wide set: they serialise on this lock.
#[cfg(test)]
pub(crate) static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// `None` when the lock is poisoned: the checks then refuse EVERYTHING (fail closed) instead of
/// treating the protected set as empty.
fn roots() -> Option<Vec<PathBuf>> {
    ROOTS.read().ok().map(|r| r.clone())
}

/// Resolved, case-folded, prefix-normalised form of `path` used for comparison.
pub(crate) fn resolve(path: &Path) -> PathBuf {
    let mut existing = path.to_path_buf();
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if let Ok(canon) = std::fs::canonicalize(&existing) {
            let mut out = canon;
            for part in tail.iter().rev() {
                out.push(part);
            }
            return fold(&out);
        }
        match (existing.file_name(), existing.parent()) {
            (Some(name), Some(parent)) => {
                tail.push(name.to_os_string());
                existing = parent.to_path_buf();
            }
            _ => return fold(path),
        }
    }
}

fn fold(path: &Path) -> PathBuf {
    let s = path.to_string_lossy().replace('/', "\\");
    let s = if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        s
    };
    // Drop `.` and resolve `..` lexically on the (already canonical) front part; `check_str`
    // callers never pass `..` through, but a stray one must not hide a protected folder.
    let mut out = PathBuf::new();
    for c in Path::new(&s.to_lowercase()).components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Pure core: does `path` fall inside one of `roots` (or, with `ancestors`, contain one)?
/// `roots` must already be [`resolve`]d.
pub(crate) fn touches(path: &Path, roots: &[PathBuf], ancestors: bool) -> bool {
    let p = resolve(path);
    // FER-514 rework (N4): a path that RESOLVES to a UNC reaching this very machine (a drive
    // letter mapped to `\\localhost\c$`, a junction into an admin share) can hide a protected
    // folder behind a spelling the roots do not match. Treated as touching.
    if crate::path_guard::is_local_machine_unc(&p.to_string_lossy()) {
        return true;
    }
    roots
        .iter()
        .any(|r| p.starts_with(r) || (ancestors && r.starts_with(&p)))
}

fn refused() -> FsError {
    FsError::new(
        ErrorCode::InvalidPath,
        "This folder holds the app's own settings and cannot be changed from the file manager",
    )
}

/// A destination / new entry / output file: refused inside a protected folder.
pub fn check_write_target(path: &str) -> Result<(), FsError> {
    let Some(roots) = roots() else {
        return Err(refused());
    };
    if touches(Path::new(path), &roots, false) {
        return Err(refused());
    }
    Ok(())
}

/// Something that is renamed, moved, deleted or modified in place: refused inside a protected
/// folder or when it contains one.
pub fn check_write_source(path: &str) -> Result<(), FsError> {
    let Some(roots) = roots() else {
        return Err(refused());
    };
    if touches(Path::new(path), &roots, true) {
        return Err(refused());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roots_of(dirs: &[&str]) -> Vec<PathBuf> {
        dirs.iter().map(|d| resolve(Path::new(d))).collect()
    }

    #[test]
    fn target_inside_is_refused_but_siblings_and_ancestors_are_fine() {
        let roots = roots_of(&[r"C:\Users\u\AppData\Roaming\dev.ferrum.app"]);
        for inside in [
            r"C:\Users\u\AppData\Roaming\dev.ferrum.app",
            r"C:\Users\u\AppData\Roaming\dev.ferrum.app\config.json",
            r"c:/users/U/appdata/roaming/DEV.FERRUM.APP/x/y.txt",
            r"C:\Users\u\AppData\Roaming\dev.ferrum.app\sub\..\config.json",
            r"\\?\C:\Users\u\AppData\Roaming\dev.ferrum.app\config.json",
        ] {
            assert!(touches(Path::new(inside), &roots, false), "{inside}");
        }
        for outside in [
            r"C:\Users\u\AppData\Roaming",
            r"C:\Users\u\AppData\Roaming\dev.ferrum.app2\x",
            r"C:\Users\u\Documents\a.txt",
            r"D:\Users\u\AppData\Roaming\dev.ferrum.app\x",
        ] {
            assert!(!touches(Path::new(outside), &roots, false), "{outside}");
        }
    }

    /// The already resolved form decides: a UNC that reaches this machine is refused, a UNC to
    /// another server is left alone. (A real mapped drive resolves to such a UNC.)
    #[test]
    fn a_resolved_unc_to_this_machine_counts_as_touching() {
        let roots = roots_of(&[r"C:\Users\u\AppData\Roaming\dev.ferrum.app"]);
        assert!(touches(
            Path::new(r"\\localhost\c$\Users\u\x"),
            &roots,
            false
        ));
        assert!(touches(Path::new(r"\\127.0.0.1\share\x"), &roots, false));
        assert!(!touches(Path::new(r"\\fileserver\data\x"), &roots, false));
    }

    #[test]
    fn source_also_refuses_an_ancestor() {
        let roots = roots_of(&[r"C:\Users\u\AppData\Roaming\dev.ferrum.app"]);
        for anc in [r"C:\Users\u\AppData\Roaming", r"C:\Users\u", r"C:\"] {
            assert!(touches(Path::new(anc), &roots, true), "{anc}");
            assert!(!touches(Path::new(anc), &roots, false), "{anc}");
        }
        assert!(!touches(Path::new(r"C:\Users\u\Documents"), &roots, true));
    }

    #[test]
    fn real_junction_into_the_folder_is_resolved() {
        let base = std::env::temp_dir().join(format!("ferrum-pd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let cfg = base.join("cfg");
        std::fs::create_dir_all(&cfg).unwrap();
        std::fs::write(cfg.join("config.json"), "{}").unwrap();
        let roots = vec![resolve(&cfg)];
        // Not-yet-existing file in the folder.
        assert!(touches(&cfg.join("new.txt"), &roots, false));
        // A symlink/junction elsewhere that points into it (skip when the OS refuses).
        let link = base.join("link");
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_dir(&cfg, &link).is_ok();
        #[cfg(not(windows))]
        let made = std::os::unix::fs::symlink(&cfg, &link).is_ok();
        if made {
            assert!(touches(&link.join("config.json"), &roots, false));
            assert!(touches(&link.join("fresh.txt"), &roots, false));
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A poisoned lock must refuse every write (not behave like "nothing is protected"), and a
    /// new `register` heals it.
    #[test]
    fn a_poisoned_lock_fails_closed() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        register(Vec::<PathBuf>::new());
        let poisoner = std::thread::spawn(|| {
            let _w = ROOTS.write().unwrap();
            panic!("poison the protected-dirs lock on purpose");
        });
        assert!(poisoner.join().is_err());
        assert!(check_write_target(r"C:\anything\at\all.txt").is_err());
        assert!(check_write_source(r"C:\anything").is_err());
        register(Vec::<PathBuf>::new());
        assert!(check_write_target(r"C:\anything\at\all.txt").is_ok());
    }

    #[test]
    fn registered_roots_drive_the_public_checks() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("ferrum-pd-reg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        register([dir.clone()]);
        let inside = dir.join("config.json").to_string_lossy().into_owned();
        let err = check_write_target(&inside).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidPath);
        assert!(check_write_source(&dir.to_string_lossy()).is_err());
        assert!(check_write_source(&dir.parent().unwrap().to_string_lossy()).is_err());
        assert!(check_write_target(&dir.parent().unwrap().to_string_lossy()).is_ok());
        register(Vec::<PathBuf>::new());
        assert!(check_write_target(&inside).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
