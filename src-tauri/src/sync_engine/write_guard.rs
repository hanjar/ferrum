//! FER-562: the protected-folder and Startup guards for the sync engine.
//!
//! The file manager refuses to write into Ferrum's own state folders (`protected_dirs`) and into
//! the Windows Startup folders (`startup_guard`). A sync profile writes to its roots without going
//! through those IPC gates, so the same policy is applied here, at two levels:
//!
//! - **Roots** (profile validation, readiness, the run gate): a root that is inside, or equal to,
//!   a protected or Startup folder is refused. A root that CONTAINS such a folder (an ancestor,
//!   for example the user profile) is refused too when the profile can write to it: a two-way or
//!   mirror run over `C:\Users\me` could otherwise write below `AppData\...\Startup`. A source
//!   root that is only read may be an ancestor; the per-path check below still applies to it.
//! - **Operations** (the engine): before every create, overwrite, delete, remove or move into the
//!   versions tree, the FINAL path the OS reports for the folder handle that is about to be
//!   written (`GetFinalPathNameByHandleW`, so 8.3 aliases and any other spelling resolve) plus the
//!   entry name is checked. A refused item fails with `protected_folder`; nothing is written.
//!
//! The policy is behind [`WriteGuard`] so engine tests can protect a folder of their own without
//! touching the process-wide registry.

use std::path::{Path, PathBuf};

use crate::archive_extract::publish::{self, DirHandle};
use crate::sync_engine::errors::{Classified, ErrorClass, Reason};

/// Why a root is refused. The string forms are the validation field-error codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootViolation {
    /// Inside, or equal to, one of Ferrum's state folders.
    InsideProtected,
    /// Contains one of Ferrum's state folders and the profile can write to it.
    ContainsProtected,
    /// Inside, or equal to, a Windows Startup folder.
    InsideStartup,
    /// Contains a Windows Startup folder and the profile can write to it.
    ContainsStartup,
    /// A share that points at this very computer: it can alias any local folder, protected or
    /// not, so it is refused as such (the local path works instead).
    LocalShare,
}

impl RootViolation {
    /// Field-error code (`FieldError.code`).
    pub fn field_code(self) -> &'static str {
        match self {
            RootViolation::InsideProtected => "root_protected_folder",
            RootViolation::ContainsProtected => "root_contains_protected_folder",
            RootViolation::InsideStartup => "root_startup_folder",
            RootViolation::ContainsStartup => "root_contains_startup_folder",
            RootViolation::LocalShare => "root_local_share",
        }
    }
}

/// The write policy of a sync run.
pub trait WriteGuard: Send + Sync {
    /// Is writing, overwriting or removing the entry at `path` refused? `path` is ALREADY a
    /// resolved final path (the final path of a folder handle plus names): implementations
    /// compare it without touching the file system, because the engine asks once per entry.
    fn refuses(&self, path: &Path) -> bool;
    /// Is the root `path` (canonical) refused? `writable`: the profile can change things inside.
    /// Does NO file system access: roots arrive canonical (the probe resolved junctions, mapped
    /// drives and symlinks already, and the gate / readiness compare it with the stored form),
    /// so a slow or dead share can never stall this check.
    fn root_violation(&self, path: &str, writable: bool) -> Option<RootViolation>;
}

/// Production policy: the registered protected folders plus the Windows Startup folders.
pub struct FerrumGuard;

impl WriteGuard for FerrumGuard {
    fn refuses(&self, path: &Path) -> bool {
        let s = path.to_string_lossy();
        (s.starts_with(r"\\")
            && crate::path_guard::is_local_machine_unc(
                &crate::protected_dirs::resolve_lexical(path).to_string_lossy(),
            ))
            || crate::protected_dirs::touches_resolved(path, false)
            || crate::startup_guard::touches_resolved(path, false)
    }

    fn root_violation(&self, path: &str, writable: bool) -> Option<RootViolation> {
        let r = crate::protected_dirs::resolve_lexical(Path::new(path));
        if crate::path_guard::is_local_machine_unc(&r.to_string_lossy()) {
            return Some(RootViolation::LocalShare);
        }
        if crate::protected_dirs::touches_resolved(&r, false) {
            return Some(RootViolation::InsideProtected);
        }
        if crate::startup_guard::touches_resolved(&r, false) {
            return Some(RootViolation::InsideStartup);
        }
        if writable {
            if crate::protected_dirs::touches_resolved(&r, true) {
                return Some(RootViolation::ContainsProtected);
            }
            if crate::startup_guard::touches_resolved(&r, true) {
                return Some(RootViolation::ContainsStartup);
            }
        }
        None
    }
}

fn refused() -> Classified {
    Classified {
        class: ErrorClass::PerItem,
        reason: Reason::ProtectedFolder,
    }
}

/// The final (resolved, no `\?\`) path of an open folder handle, or an error when the OS cannot
/// tell (fail closed on Windows). One call per folder or batch; the entries below it are then
/// checked with [`check_under`], which does not touch the disk.
pub fn final_base(dir: &DirHandle) -> Result<PathBuf, Classified> {
    match publish::final_path(dir) {
        Some(p) => Ok(PathBuf::from(crate::path_guard::strip_verbatim(&p))),
        #[cfg(windows)]
        None => Err(refused()),
        #[cfg(not(windows))]
        None => Err(Classified {
            class: ErrorClass::PerItem,
            reason: Reason::Unknown,
        }),
    }
}

/// Check the entry `comps` below the resolved folder `base` (see [`final_base`]). Pure string
/// work: no file system access. Use it BEFORE `open_chain(.., create)` makes missing folders
/// (the verdict has to come first); the chain never follows a reparse point, so `base` plus the
/// components is where the walk ends up.
pub fn check_under(guard: &dyn WriteGuard, base: &Path, comps: &[&str]) -> Result<(), Classified> {
    let mut path = base.to_path_buf();
    for c in comps {
        path.push(c);
    }
    if guard.refuses(&path) {
        return Err(refused());
    }
    Ok(())
}

/// Check the entry `name` inside the open folder `dir` (`None`: the folder itself): one handle
/// lookup, for callers that check a single entry.
pub fn check_entry(
    guard: &dyn WriteGuard,
    dir: &DirHandle,
    name: Option<&str>,
) -> Result<(), Classified> {
    let base = final_base(dir)?;
    check_under(guard, &base, name.as_slice())
}

/// [`check_under`] with one handle lookup, for the callers that check a single path.
pub fn check_planned(
    guard: &dyn WriteGuard,
    root: &DirHandle,
    comps: &[&str],
) -> Result<(), Classified> {
    check_under(guard, &final_base(root)?, comps)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protected_dirs;
    use crate::sync_engine::test_support::ReservedTempDir;

    // FER-568: removed when the test ends, so no `ferrum-wg-*` folder stays in %TEMP%.
    fn temp(tag: &str) -> ReservedTempDir {
        ReservedTempDir::new(&format!("ferrum-wg-{tag}-"))
    }

    #[test]
    fn roots_inside_equal_or_above_a_protected_folder() {
        let _g = protected_dirs::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let base = temp("roots");
        let cfg = base.join("state");
        let other = base.join("other");
        std::fs::create_dir_all(cfg.join("sub")).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        protected_dirs::register([cfg.clone()]);
        // Roots reach the guard in canonical form (long names); a CI temp dir may be 8.3.
        let s = |p: &Path| protected_dirs::resolve(p).to_string_lossy().into_owned();
        let g = FerrumGuard;
        for writable in [false, true] {
            assert_eq!(
                g.root_violation(&s(&cfg), writable),
                Some(RootViolation::InsideProtected)
            );
            assert_eq!(
                g.root_violation(&s(&cfg.join("sub")), writable),
                Some(RootViolation::InsideProtected)
            );
            assert_eq!(g.root_violation(&s(&other), writable), None);
        }
        // An ancestor is fine to READ from and refused when it can be written.
        assert_eq!(g.root_violation(&s(&base), false), None);
        assert_eq!(
            g.root_violation(&s(&base), true),
            Some(RootViolation::ContainsProtected)
        );
        protected_dirs::register(Vec::<PathBuf>::new());
    }

    /// A share that points at this computer has its own verdict, read-only or not; the per-entry
    /// check is pure string work on an already resolved path.
    #[test]
    fn local_shares_and_resolved_entry_checks() {
        let _g = protected_dirs::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let g = FerrumGuard;
        for writable in [false, true] {
            assert_eq!(
                g.root_violation(r"\\localhost\c$\Users\x", writable),
                Some(RootViolation::LocalShare)
            );
            assert_eq!(
                g.root_violation(r"\\127.0.0.1\share", writable),
                Some(RootViolation::LocalShare)
            );
        }
        assert_eq!(g.root_violation(r"\\fileserver\data", true), None);
        protected_dirs::register([PathBuf::from(r"C:\Ferrum\State")]);
        assert!(g.refuses(Path::new(r"C:\Ferrum\State\config.json")));
        assert!(g.refuses(Path::new(r"c:\ferrum\STATE")));
        assert!(!g.refuses(Path::new(r"C:\Ferrum\State2\x")));
        assert!(!g.refuses(Path::new(r"C:\Ferrum")));
        assert!(!g.refuses(Path::new(r"\\fileserver\data\x")));
        // A folder base plus names, no disk access: `check_under` joins and asks the policy.
        assert!(check_under(&g, Path::new(r"C:\Ferrum"), &["State", "a.txt"]).is_err());
        assert!(check_under(&g, Path::new(r"C:\Ferrum"), &["Other", "a.txt"]).is_ok());
        protected_dirs::register(Vec::<PathBuf>::new());
    }

    #[cfg(windows)]
    #[test]
    fn startup_roots_and_their_ancestors() {
        let _g = crate::protected_dirs::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let g = FerrumGuard;
        let startup =
            std::env::var("APPDATA").unwrap() + r"\Microsoft\Windows\Start Menu\Programs\Startup";
        let programs = Path::new(&startup)
            .parent()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        for writable in [false, true] {
            assert_eq!(
                g.root_violation(&startup, writable),
                Some(RootViolation::InsideStartup)
            );
            assert_eq!(
                g.root_violation(&format!(r"{startup}\sub"), writable),
                Some(RootViolation::InsideStartup)
            );
        }
        assert_eq!(g.root_violation(&programs, false), None);
        assert_eq!(
            g.root_violation(&programs, true),
            Some(RootViolation::ContainsStartup)
        );
        assert_eq!(
            g.root_violation(&std::env::var("USERPROFILE").unwrap(), true),
            Some(RootViolation::ContainsStartup)
        );
        assert_eq!(g.root_violation(r"C:\Users\nobody\Documents", true), None);
    }

    #[cfg(windows)]
    #[test]
    fn an_entry_below_a_protected_folder_is_refused_by_its_final_path() {
        let _g = protected_dirs::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let base = temp("entry");
        let cfg = base.join("state");
        let plain = base.join("plain");
        std::fs::create_dir_all(&cfg).unwrap();
        std::fs::create_dir_all(&plain).unwrap();
        protected_dirs::register([cfg.clone()]);
        let g = FerrumGuard;
        let h_cfg = publish::open_root(&cfg).unwrap();
        let h_plain = publish::open_root(&plain).unwrap();
        assert_eq!(
            check_entry(&g, &h_cfg, Some("config.json"))
                .unwrap_err()
                .reason,
            Reason::ProtectedFolder
        );
        assert!(check_entry(&g, &h_cfg, None).is_err());
        assert!(check_entry(&g, &h_plain, Some("a.txt")).is_ok());
        drop(h_cfg);
        // A handle opened through a short (8.3) spelling still reports the long final path.
        if let Some(short) = short_name(&cfg) {
            let h = publish::open_root(Path::new(&short)).unwrap();
            assert!(check_entry(&g, &h, Some("x")).is_err(), "{short}");
        }
        protected_dirs::register(Vec::<PathBuf>::new());
    }

    /// The real Startup folder, opened by handle (read-only: nothing is written), is refused as
    /// a place to write, and so is a parent folder of it by planned path.
    #[cfg(windows)]
    #[test]
    fn the_real_startup_folder_is_refused_by_its_handle() {
        let _g = crate::protected_dirs::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let startup =
            std::env::var("APPDATA").unwrap() + r"\Microsoft\Windows\Start Menu\Programs\Startup";
        let Ok(h) = publish::open_root_read(Path::new(&startup)) else {
            return; // no Startup folder on this machine
        };
        let g = FerrumGuard;
        assert_eq!(
            check_entry(&g, &h, Some("evil.bat")).unwrap_err().reason,
            Reason::ProtectedFolder
        );
        let programs = publish::open_root_read(Path::new(&startup).parent().unwrap()).unwrap();
        assert!(check_planned(&g, &programs, &["Startup", "evil.bat"]).is_err());
        assert!(check_planned(&g, &programs, &["Startup2", "ok.bat"]).is_ok());
    }

    #[cfg(windows)]
    fn short_name(p: &Path) -> Option<String> {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::GetShortPathNameW;
        let wide: Vec<u16> = p.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut buf = vec![0u16; 1024];
        // SAFETY: valid NUL-terminated input and a writable buffer of the stated length.
        let n = unsafe { GetShortPathNameW(wide.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) };
        (n > 0 && (n as usize) < buf.len()).then(|| String::from_utf16_lossy(&buf[..n as usize]))
    }
}
