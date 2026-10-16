//! FER-559 (P3-C): no file that Windows runs at logon may be created in a Startup folder through
//! the generic file operations. A compromised webview could otherwise write a harmless-looking
//! `a.json` there and rename it to `a.bat` (two steps), or copy / extract a script in.
//!
//! "Startup" = the per-user and the all-users Startup folder
//! (`SHGetKnownFolderPath(FOLDERID_Startup / FOLDERID_CommonStartup)`), compared on resolved
//! paths (junctions and symlinks resolve, case is folded; see `protected_dirs::resolve`). A name
//! counts as executable when its extension (trailing dots and spaces, which Windows drops, are
//! ignored) is one of [`EXECUTABLE_EXTENSIONS`].
//!
//! What is refused: create, rename (new name), copy / move (source names), multi-rename (new names)
//! and combine (output file) of an executable name inside a Startup folder, and extraction of any
//! archive into one (the entry names are not known up front). Moving things OUT of Startup,
//! deleting and plain reads stay allowed.

use crate::domain::{ErrorCode, FsError};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub const EXECUTABLE_EXTENSIONS: &[&str] = &[
    "bat", "cmd", "ps1", "psm1", "vbs", "vbe", "js", "jse", "wsf", "wsh", "lnk", "url", "scr",
    "exe", "com", "pif", "hta", "msi", "reg",
];

/// True when the file name ends in an extension Windows runs or interprets.
pub fn has_executable_extension(name: &str) -> bool {
    let trimmed = name.trim_end_matches(['.', ' ']);
    match trimmed.rsplit_once('.') {
        Some((_, ext)) => {
            let ext = ext.trim_end_matches(' ').to_ascii_lowercase();
            EXECUTABLE_EXTENSIONS.contains(&ext.as_str())
        }
        None => false,
    }
}

/// Resolved Startup folders of this machine/user (cached for the process lifetime).
fn startup_dirs() -> &'static [PathBuf] {
    static DIRS: OnceLock<Vec<PathBuf>> = OnceLock::new();
    DIRS.get_or_init(|| {
        #[cfg(windows)]
        {
            use windows_sys::Win32::UI::Shell::{FOLDERID_CommonStartup, FOLDERID_Startup};
            [&FOLDERID_Startup, &FOLDERID_CommonStartup]
                .into_iter()
                .filter_map(crate::profile::known_folder)
                .map(|d| crate::protected_dirs::resolve(&d))
                .collect()
        }
        #[cfg(not(windows))]
        {
            Vec::new()
        }
    })
}

/// Pure core: is `dir` inside (or equal to) one of the resolved `startups`?
pub(crate) fn dir_is_in(dir: &Path, startups: &[PathBuf]) -> bool {
    let d = crate::protected_dirs::resolve(dir);
    startups.iter().any(|s| d.starts_with(s))
}

fn refused() -> FsError {
    FsError::new(
        ErrorCode::InvalidPath,
        "Programs and scripts (.bat, .cmd, .ps1, .lnk, .exe ...) cannot be created in a Startup \
         folder from the file manager, because they would run at every logon",
    )
}

fn refused_extract() -> FsError {
    FsError::new(
        ErrorCode::InvalidPath,
        "Archives cannot be extracted into a Startup folder from the file manager, because \
         programs and scripts there would run at every logon",
    )
}

pub(crate) fn check_name_in(dir: &str, name: &str, startups: &[PathBuf]) -> Result<(), FsError> {
    if has_executable_extension(name) && dir_is_in(Path::new(dir), startups) {
        return Err(refused());
    }
    Ok(())
}

/// `name` is created in / moved into / renamed to inside `dir`.
pub fn check_name(dir: &str, name: &str) -> Result<(), FsError> {
    check_name_in(dir, name, startup_dirs())
}

/// The final component of `full_path` is created in its parent folder.
pub fn check_new_path(full_path: &str) -> Result<(), FsError> {
    let p = Path::new(full_path);
    match (p.parent(), p.file_name()) {
        (Some(parent), Some(name)) => {
            check_name(&parent.to_string_lossy(), &name.to_string_lossy())
        }
        _ => Ok(()),
    }
}

/// Copy / move of `sources` into `target_dir`: refused when a source name is executable and the
/// target is a Startup folder.
pub fn check_sources_into(target_dir: &str, sources: &[String]) -> Result<(), FsError> {
    if !dir_is_in(Path::new(target_dir), startup_dirs()) {
        return Ok(());
    }
    for s in sources {
        if let Some(name) = Path::new(s).file_name()
            && has_executable_extension(&name.to_string_lossy())
        {
            return Err(refused());
        }
    }
    Ok(())
}

/// Extraction destination (output names are not known up front).
pub fn check_extract_into(dir: &str) -> Result<(), FsError> {
    if dir_is_in(Path::new(dir), startup_dirs()) {
        return Err(refused_extract());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn startups() -> Vec<PathBuf> {
        vec![
            crate::protected_dirs::resolve(Path::new(
                r"C:\Users\u\AppData\Roaming\Microsoft\Windows\Start Menu\Programs\Startup",
            )),
            crate::protected_dirs::resolve(Path::new(
                r"C:\ProgramData\Microsoft\Windows\Start Menu\Programs\StartUp",
            )),
        ]
    }

    #[test]
    fn classifies_executable_extensions() {
        for yes in [
            "a.bat",
            "A.CMD",
            "x.ps1",
            "m.psm1",
            "a.vbs",
            "a.vbe",
            "a.js",
            "a.jse",
            "a.wsf",
            "a.wsh",
            "a.lnk",
            "a.url",
            "a.scr",
            "a.exe",
            "a.com",
            "a.pif",
            "a.hta",
            "a.msi",
            "a.reg",
            "a.bat.",
            "a.bat ",
            "a.b.c.Bat",
            "a.json.cmd",
        ] {
            assert!(has_executable_extension(yes), "{yes}");
        }
        for no in [
            "a.json",
            "a.ferrum-selection",
            "bat",
            ".gitignore",
            "a.txt",
            "a.bat.txt",
            "a.jsx",
            "exe",
            "",
            "a.",
        ] {
            assert!(!has_executable_extension(no), "{no}");
        }
    }

    #[test]
    fn executable_names_are_refused_only_inside_startup() {
        let s = startups();
        let user = r"C:\Users\u\AppData\Roaming\Microsoft\Windows\Start Menu\Programs\Startup";
        let common = r"c:/programdata/microsoft/windows/start menu/programs/startup";
        for dir in [
            user.to_string(),
            common.to_string(),
            format!(r"{user}\sub"),
            format!(r"\\?\{user}"),
        ] {
            assert!(check_name_in(&dir, "a.bat", &s).is_err(), "{dir}");
            assert!(check_name_in(&dir, "link.LNK", &s).is_err(), "{dir}");
            assert!(check_name_in(&dir, "a.json", &s).is_ok(), "{dir}");
        }
        for dir in [
            r"C:\Users\u\Documents",
            r"C:\Users\u\AppData\Roaming\Microsoft\Windows\Start Menu\Programs",
            r"C:\Users\u\AppData\Roaming\Microsoft\Windows\Start Menu\Programs\Startup2",
        ] {
            assert!(check_name_in(dir, "a.bat", &s).is_ok(), "{dir}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn the_real_startup_folders_resolve_and_gate() {
        let dirs = startup_dirs();
        assert_eq!(dirs.len(), 2, "{dirs:?}");
        for d in dirs {
            let s = d.to_string_lossy();
            assert!(s.ends_with("startup"), "{s}");
        }
        let user = dirs[0].to_string_lossy().into_owned();
        assert!(check_name(&user, "x.bat").is_err());
        assert!(check_new_path(&format!(r"{user}\x.cmd")).is_err());
        assert!(check_new_path(&format!(r"{user}\x.txt")).is_ok());
        assert!(check_sources_into(&user, &[r"C:\t\x.ps1".into()]).is_err());
        assert!(check_sources_into(&user, &[r"C:\t\x.txt".into()]).is_ok());
        assert!(check_sources_into(r"C:\t", &[r"C:\t\x.ps1".into()]).is_ok());
        assert!(check_extract_into(&user).is_err());
        assert!(check_extract_into(r"C:\t").is_ok());
    }
}
