//! FER-559 (P3-C, reworked): the file manager writes NOTHING into a Windows Startup folder
//! (per-user and all-users): programs and scripts there run at every logon, and a denylist of
//! extensions is bypassed by renaming in two steps, by archives, by folder trees and by `.lnk`
//! targets. Reading, opening and DELETING inside Startup stay allowed (the user can clean it up).
//!
//! "Startup" = `SHGetKnownFolderPath(FOLDERID_Startup / FOLDERID_CommonStartup)`, plus the standard
//! `%APPDATA%` / `%ProgramData%` spellings. The union is used, so a failing known-folder call can
//! never leave the list empty (fail closed). Comparison is on resolved paths (junctions and
//! symlinks resolve, case is folded; see `protected_dirs::resolve`).
//!
//! Enforced at: the IPC gate for every destination / new entry (create, copy, move, extract,
//! archive create, split, combine, encode, decode, checksum, selection save); rename (the new path
//! may not be inside Startup, nor Startup or one of its ancestors); the copy and move engines per
//! created directory and for the final path of a moved entry; the extractor for every planned entry.

use crate::domain::{ErrorCode, FsError};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

const MESSAGE: &str = "Ferrum does not write into the Windows Startup folder.";

/// Resolved Startup folders of this machine/user (cached for the process lifetime).
fn startup_dirs() -> &'static [PathBuf] {
    static DIRS: OnceLock<Vec<PathBuf>> = OnceLock::new();
    DIRS.get_or_init(|| {
        let mut raw: Vec<PathBuf> = Vec::new();
        #[cfg(windows)]
        {
            use windows_sys::Win32::UI::Shell::{FOLDERID_CommonStartup, FOLDERID_Startup};
            for id in [&FOLDERID_Startup, &FOLDERID_CommonStartup] {
                match crate::profile::known_folder(id) {
                    Some(d) => raw.push(d),
                    None => log::warn!(
                        "a Startup known folder could not be resolved; using the standard path"
                    ),
                }
            }
        }
        let tail = r"Microsoft\Windows\Start Menu\Programs\Startup";
        for var in ["APPDATA", "ProgramData"] {
            if let Some(base) = std::env::var_os(var).filter(|v| !v.is_empty()) {
                raw.push(PathBuf::from(base).join(tail));
            }
        }
        let mut out: Vec<PathBuf> = Vec::new();
        for d in raw {
            let r = crate::protected_dirs::resolve(&d);
            if !out.contains(&r) {
                out.push(r);
            }
        }
        if out.is_empty() {
            log::error!("no Startup folder could be determined");
        }
        out
    })
}

fn refused() -> FsError {
    FsError::new(ErrorCode::InvalidPath, MESSAGE)
}

/// Pure core: is `dir` (resolved) inside, or equal to, one of the resolved `startups`?
pub(crate) fn dir_is_in(dir: &Path, startups: &[PathBuf]) -> bool {
    let d = crate::protected_dirs::resolve(dir);
    startups.iter().any(|s| d.starts_with(s))
}

/// Pure core: is `path` (resolved) a Startup folder or one of its ancestors?
pub(crate) fn is_startup_or_ancestor(path: &Path, startups: &[PathBuf]) -> bool {
    let p = crate::protected_dirs::resolve(path);
    startups.iter().any(|s| s.starts_with(&p))
}

/// Pure core: would any entry `root`/`rel` land inside Startup? `root` is resolved once; the
/// relative parts are appended lexically (the extractor and the copy engine never follow links).
pub(crate) fn tree_hits(
    root: &Path,
    rels: impl IntoIterator<Item = impl AsRef<Path>>,
    startups: &[PathBuf],
) -> bool {
    let root = crate::protected_dirs::resolve(root);
    rels.into_iter().any(|rel| {
        let joined = crate::protected_dirs::resolve_lexical(&root.join(rel.as_ref()));
        startups.iter().any(|s| joined.starts_with(s))
    })
}

/// True when `path` is a Startup folder or inside one.
pub fn is_inside(path: &Path) -> bool {
    dir_is_in(path, startup_dirs())
}

/// FER-562: [`is_inside`] (or, with `ancestors`, also "is a Startup folder or an ancestor") for a
/// path that is ALREADY a resolved final path: no file system access.
pub fn touches_resolved(path: &Path, ancestors: bool) -> bool {
    let d = crate::protected_dirs::resolve_lexical(path);
    startup_dirs()
        .iter()
        .any(|s| d.starts_with(s) || (ancestors && s.starts_with(&d)))
}

/// A destination / new entry / output path: refused inside Startup.
pub fn check_into(path: &str) -> Result<(), FsError> {
    if is_inside(Path::new(path)) {
        return Err(refused());
    }
    Ok(())
}

/// The new path of a rename: refused inside Startup and when it IS Startup or one of its
/// ancestors (renaming a folder to `Startup`, or a case-only rename of an ancestor).
pub fn check_rename_to(new_path: &str) -> Result<(), FsError> {
    let p = Path::new(new_path);
    if is_inside(p) || is_startup_or_ancestor(p, startup_dirs()) {
        return Err(refused());
    }
    Ok(())
}

/// Extraction: refused when any planned entry would land inside Startup.
pub fn check_extraction<'a>(
    destination: &Path,
    rels: impl IntoIterator<Item = &'a Path>,
) -> Result<(), FsError> {
    if tree_hits(destination, rels, startup_dirs()) {
        return Err(refused());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const USER: &str = r"C:\Users\u\AppData\Roaming\Microsoft\Windows\Start Menu\Programs\Startup";
    const PROGRAMS: &str = r"C:\Users\u\AppData\Roaming\Microsoft\Windows\Start Menu\Programs";

    fn startups() -> Vec<PathBuf> {
        vec![
            crate::protected_dirs::resolve(Path::new(USER)),
            crate::protected_dirs::resolve(Path::new(
                r"C:\ProgramData\Microsoft\Windows\Start Menu\Programs\StartUp",
            )),
        ]
    }

    #[test]
    fn anything_inside_startup_counts_siblings_do_not() {
        let s = startups();
        for dir in [
            USER.to_string(),
            r"c:/programdata/microsoft/windows/start menu/programs/startup/x.txt".to_string(),
            format!(r"{USER}\sub\a.txt"),
            format!(r"\\?\{USER}\a.bat"),
        ] {
            assert!(dir_is_in(Path::new(&dir), &s), "{dir}");
        }
        for dir in [
            r"C:\Users\u\Documents".to_string(),
            PROGRAMS.to_string(),
            format!(r"{PROGRAMS}\Startup2\a.bat"),
        ] {
            assert!(!dir_is_in(Path::new(&dir), &s), "{dir}");
        }
    }

    #[test]
    fn startup_and_its_ancestors_are_recognised() {
        let s = startups();
        for p in [USER, PROGRAMS, r"C:\Users\u", r"C:\"] {
            assert!(is_startup_or_ancestor(Path::new(p), &s), "{p}");
        }
        for p in [format!(r"{PROGRAMS}\Startup2"), r"C:\Users\v".to_string()] {
            assert!(!is_startup_or_ancestor(Path::new(&p), &s), "{p}");
        }
    }

    /// A tree whose own structure contains `Startup\a.bat` lands in Startup when it is copied or
    /// extracted into `Programs`; the same tree elsewhere does not.
    #[test]
    fn a_tree_that_reaches_startup_below_its_destination_is_detected() {
        let s = startups();
        let rels = [Path::new("Startup"), Path::new("Startup/a.bat")];
        assert!(tree_hits(Path::new(PROGRAMS), rels, &s));
        assert!(tree_hits(
            Path::new(PROGRAMS),
            [Path::new("startup\\a.bat")],
            &s
        ));
        assert!(tree_hits(Path::new(USER), [Path::new("a.txt")], &s));
        assert!(!tree_hits(
            Path::new(PROGRAMS),
            [Path::new("Startup2/a.bat")],
            &s
        ));
        assert!(!tree_hits(Path::new(r"C:\Users\u\Documents"), rels, &s));
    }

    #[cfg(windows)]
    #[test]
    fn the_real_startup_folders_resolve_and_gate() {
        let dirs = startup_dirs();
        assert!(dirs.len() >= 2, "{dirs:?}");
        for d in dirs {
            assert!(d.to_string_lossy().ends_with("startup"), "{d:?}");
        }
        let user = dirs[0].to_string_lossy().into_owned();
        assert!(check_into(&user).is_err());
        assert!(
            check_into(&format!(r"{user}\x.txt")).is_err(),
            "even a text file"
        );
        assert!(check_into(r"C:\t\x.txt").is_ok());
        assert!(check_rename_to(&user).is_err());
        let programs = Path::new(&user)
            .parent()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(check_rename_to(&programs).is_err(), "an ancestor");
        assert!(check_rename_to(&format!(r"{programs}\Other")).is_ok());
        assert!(check_extraction(Path::new(&programs), [Path::new("Startup/a.bat")]).is_err());
        assert!(check_extraction(Path::new(&programs), [Path::new("Other/a.bat")]).is_ok());
        let err = check_into(&user).unwrap_err();
        assert_eq!(
            err.message,
            "Ferrum does not write into the Windows Startup folder."
        );
    }
}
