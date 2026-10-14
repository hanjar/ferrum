//! Test profile override (FER-457).
//!
//! Tauri's `app_config_dir` / `app_data_dir` / `app_local_data_dir` use the Windows Known-Folder API, which ignores
//! `APPDATA` / `LOCALAPPDATA` overrides, so a smoke run could never be isolated from the real user profile.
//! When `FERRUM_PROFILE_DIR` is set (non-empty), EVERY directory the app persists to is rooted there instead:
//!
//! - `<dir>/config`   prefs + session (`config.json`), AI provider setup
//! - `<dir>/data`     AI execution journal, sync baselines
//! - `<dir>/activity` Activity (Recent/Frequent) database
//! - `<dir>/local`    local (non-roaming) data — the automation store
//! - `<dir>/webview`  WebView2 user-data folder (localStorage, caches)
//!
//! For tests and measurement scripts only. Empty or unset means the normal OS locations.
//! `FERRUM_ACTIVITY_DIR` (older, activity-only) still wins for the activity store when both are set.
//!
//! Fail-closed: a set but invalid value (not an absolute local drive path, or containing UNC, device, WebDAV or `..`)
//! makes the app refuse to start; it never falls back to the real profile. While the profile is active the window
//! title says so and the real AI credential store is read-only (save/clear are refused).

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub const ENV_VAR: &str = "FERRUM_PROFILE_DIR";
pub const ACTIVITY_ENV_VAR: &str = "FERRUM_ACTIVITY_DIR";

/// App identifier used for the OS data dirs (must stay in sync with `tauri.conf.json`).
pub const APP_DIR_ID: &str = "dev.ferrum.app";

/// Validated overrides, fixed once at startup by [`init`].
struct Active {
    root: Option<PathBuf>,
    activity: Option<PathBuf>,
}

static ACTIVE: OnceLock<Active> = OnceLock::new();

/// Names the offending variable only (never its value): the caller logs one line and refuses to start.
#[derive(Debug, PartialEq, Eq)]
pub struct InvalidOverride(pub &'static str);

/// Validate and freeze `FERRUM_PROFILE_DIR` / `FERRUM_ACTIVITY_DIR`. A set but invalid value is an error.
pub fn init() -> Result<(), InvalidOverride> {
    let root = read_env(ENV_VAR)?;
    let activity = read_env(ACTIVITY_ENV_VAR)?;
    let _ = ACTIVE.set(Active { root, activity });
    Ok(())
}

fn read_env(name: &'static str) -> Result<Option<PathBuf>, InvalidOverride> {
    let value = std::env::var_os(name);
    match from_value(value.as_deref()) {
        Ok(Some(dir)) => {
            #[cfg(windows)]
            if !is_local_drive(&dir) {
                return Err(InvalidOverride(name));
            }
            Ok(Some(dir))
        }
        Ok(None) => Ok(None),
        Err(()) => Err(InvalidOverride(name)),
    }
}

/// FER-462 R1: what the user is told when `name` holds an invalid value. Names the variable and
/// the rule, never the value itself.
pub fn invalid_override_message(name: &str) -> String {
    format!(
        "Variabel lingkungan {name} berisi nilai yang tidak valid.\n\n\
         Nilainya harus path folder absolut di drive lokal, misalnya C:\\ferrum-uji. \
         UNC (\\\\server\\share), path device (\\\\?\\, \\\\.\\), WebDAV, drive jaringan, \
         dan komponen \"..\" tidak diterima.\n\n\
         Ferrum tidak dijalankan supaya profil asli tetap tidak tersentuh. \
         Perbaiki atau hapus variabel itu, lalu jalankan Ferrum lagi."
    )
}

/// FER-462 R1: an invalid override stops the app (fail closed, FER-457). The reason always goes to
/// stderr; a release build has no console, so it is also shown in a message box, but only when
/// [`should_show_dialog`] says a person can see and dismiss it.
pub fn report_invalid_override(name: &str) {
    eprintln!(
        "ferrum: {name} must be an absolute local drive path (no UNC, device, WebDAV or `..`); refusing to start"
    );
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            MB_ICONERROR, MB_OK, MB_SETFOREGROUND, MessageBoxW,
        };
        let ci = std::env::var_os("CI").is_some_and(|v| !v.is_empty());
        if !should_show_dialog(interactive_window_station(), ci, stderr_is_redirected()) {
            return;
        }
        let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
        let text = wide(&invalid_override_message(name));
        let title = wide("Ferrum - profil uji tidak valid");
        // SAFETY: both strings are NUL-terminated and outlive the call; no owner window.
        unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                text.as_ptr(),
                title.as_ptr(),
                MB_OK | MB_ICONERROR | MB_SETFOREGROUND,
            );
        }
    }
}

/// FER-462 R1 (review S1): a modal message box would hang a process nobody can see: a CI runner
/// or service in session 0, a scheduled task "run whether logged on or not", an SSH session, or a
/// smoke script that reads stderr. Show it only on a visible (interactive) window station, outside
/// CI (`CI` set and non-empty), and when stderr is not redirected to a pipe or file.
pub fn should_show_dialog(interactive_station: bool, ci: bool, stderr_redirected: bool) -> bool {
    interactive_station && !ci && !stderr_redirected
}

/// True when the process window station is visible (`WSF_VISIBLE`), i.e. `WinSta0` of a logged-on
/// desktop. Any failure counts as "not interactive" (no dialog, never a hang).
#[cfg(windows)]
fn interactive_window_station() -> bool {
    use windows_sys::Win32::System::StationsAndDesktops::{
        GetProcessWindowStation, GetUserObjectInformationW, UOI_FLAGS, USEROBJECTFLAGS,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::WSF_VISIBLE;
    // SAFETY: the station handle is not owned (no close); `flags` is a valid out buffer of the
    // given size.
    unsafe {
        let station = GetProcessWindowStation();
        if station.is_null() {
            return false;
        }
        let mut flags = USEROBJECTFLAGS::default();
        let mut needed = 0u32;
        let ok = GetUserObjectInformationW(
            station as _,
            UOI_FLAGS,
            (&mut flags as *mut USEROBJECTFLAGS).cast(),
            std::mem::size_of::<USEROBJECTFLAGS>() as u32,
            &mut needed,
        );
        ok != 0 && flags.dwFlags & WSF_VISIBLE as u32 != 0
    }
}

/// True when stderr goes to a pipe or a file (someone reads it). A console (`FILE_TYPE_CHAR`) or
/// no stderr at all (a release GUI build started from Explorer) is not redirected.
#[cfg(windows)]
fn stderr_is_redirected() -> bool {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{FILE_TYPE_DISK, FILE_TYPE_PIPE, GetFileType};
    let handle = std::io::stderr().as_raw_handle();
    if handle.is_null() {
        return false;
    }
    // SAFETY: GetFileType only queries the handle; an invalid one yields FILE_TYPE_UNKNOWN.
    let kind = unsafe { GetFileType(handle as _) };
    kind == FILE_TYPE_DISK || kind == FILE_TYPE_PIPE
}

/// Root of the test profile, or `None` for a normal run.
pub fn profile_dir() -> Option<PathBuf> {
    ACTIVE.get().and_then(|a| a.root.clone())
}

/// True when the test profile is active (the real credential store must then stay untouched).
pub fn is_active() -> bool {
    ACTIVE.get().is_some_and(|a| a.root.is_some())
}

#[tauri::command]
pub fn test_profile_active() -> bool {
    is_active()
}

/// `Ok(None)` for unset/blank; `Err` for anything that is not an absolute local drive path.
fn from_value(value: Option<&OsStr>) -> Result<Option<PathBuf>, ()> {
    let Some(value) = value else {
        return Ok(None);
    };
    let text = value.to_str().ok_or(())?;
    if text.trim().is_empty() {
        return Ok(None);
    }
    validate_syntax(text)?;
    Ok(Some(PathBuf::from(text)))
}

/// `X:\...` or `X:/...` only. Rejects UNC, `\\?\` and `\\.\` device paths, WebDAV, and `..` components.
fn validate_syntax(text: &str) -> Result<(), ()> {
    if text != text.trim() || crate::media_protocol::is_device_path(text) {
        return Err(());
    }
    let b = text.as_bytes();
    let drive =
        b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && matches!(b[2], b'\\' | b'/');
    if !drive {
        return Err(()); // relative, drive-relative, UNC (`\\host`, `//host`), or anything else
    }
    let lower = text.to_ascii_lowercase();
    if lower.contains("davwwwroot") || lower.contains("@ssl") {
        return Err(());
    }
    if text.split(['\\', '/']).any(|c| c == "..") {
        return Err(());
    }
    Ok(())
}

/// The drive must be a local fixed/removable/RAM disk: network and unknown drives are refused.
#[cfg(windows)]
fn is_local_drive(dir: &Path) -> bool {
    use windows_sys::Win32::Storage::FileSystem::GetDriveTypeW;
    let letter = dir.to_string_lossy().chars().next().unwrap_or(' ');
    let root: Vec<u16> = format!("{letter}:\\").encode_utf16().chain([0]).collect();
    // DRIVE_REMOVABLE 2, DRIVE_FIXED 3, DRIVE_RAMDISK 6
    matches!(unsafe { GetDriveTypeW(root.as_ptr()) }, 2 | 3 | 6)
}

pub fn config_in(root: &Path) -> PathBuf {
    root.join("config")
}

pub fn data_in(root: &Path) -> PathBuf {
    root.join("data")
}

pub fn activity_in(root: &Path) -> PathBuf {
    root.join("activity")
}

pub fn webview_in(root: &Path) -> PathBuf {
    root.join("webview")
}

/// Local (non-roaming) data dir under a test-profile root.
pub fn local_in(root: &Path) -> PathBuf {
    root.join("local")
}

/// Config dir (prefs, AI setup): test profile first, else the OS app config dir.
pub fn config_dir<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> Option<PathBuf> {
    use tauri::Manager;
    match profile_dir() {
        Some(root) => Some(config_in(&root)),
        None => app.path().app_config_dir().ok(),
    }
}

/// Config dir without an AppHandle (for background jobs and tests):
/// test profile first, then %APPDATA%\dev.ferrum.app, then system temp.
pub fn resolve_config_dir() -> PathBuf {
    if let Some(root) = profile_dir() {
        return config_in(&root);
    }
    if let Some(appdata) = std::env::var_os("APPDATA") {
        return PathBuf::from(appdata).join("dev.ferrum.app");
    }
    std::env::temp_dir()
}

/// Data dir (AI journal, sync baselines): test profile first, else the OS app data dir.
pub fn data_dir<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> Option<PathBuf> {
    use tauri::Manager;
    match profile_dir() {
        Some(root) => Some(data_in(&root)),
        None => app.path().app_data_dir().ok(),
    }
}

/// Activity store dir: `FERRUM_ACTIVITY_DIR`, then the test profile, else `<app local data>/activity`.
pub fn activity_dir<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> Option<PathBuf> {
    use tauri::Manager;
    if let Some(dir) = ACTIVE.get().and_then(|a| a.activity.clone()) {
        return Some(dir);
    }
    match profile_dir() {
        Some(root) => Some(activity_in(&root)),
        None => app
            .path()
            .app_local_data_dir()
            .ok()
            .map(|dir| dir.join("activity")),
    }
}

/// Local (non-roaming) data dir (automation store): test profile first, else the OS app local
/// data dir. Never falls back to the roaming dir: WAL needs a local disk and the store must stay
/// machine-bound.
pub fn local_data_dir<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> Option<PathBuf> {
    use tauri::Manager;
    match profile_dir() {
        Some(root) => Some(local_in(&root)),
        None => app.path().app_local_data_dir().ok(),
    }
}

/// Local data dir without an `AppHandle` (for code the runner shares): the test profile
/// (`FERRUM_PROFILE_DIR`) first, otherwise the Windows Known Folder `LocalAppData` plus the app
/// identifier - the same composition as Tauri's `app_local_data_dir`, without trusting the
/// environment. `None` when the folder cannot be resolved: there is NO fallback to the temp dir
/// (the store would silently live in a shared, world-writable place).
pub fn resolve_local_data_dir() -> Option<PathBuf> {
    resolve_local_data_dir_in(profile_dir().as_deref(), known_local_app_data().as_deref())
}

/// Pure core of [`resolve_local_data_dir`]: `local_app_data` stands in for the Known Folder
/// lookup so a unit test can inject it.
pub(crate) fn resolve_local_data_dir_in(
    profile: Option<&Path>,
    local_app_data: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(root) = profile {
        return Some(local_in(root));
    }
    local_app_data
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(APP_DIR_ID))
}

/// `SHGetKnownFolderPath(FOLDERID_LocalAppData)`: the user's local (non-roaming) app data folder.
#[cfg(windows)]
pub(crate) fn known_local_app_data() -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::{FOLDERID_LocalAppData, SHGetKnownFolderPath};
    let mut raw: *mut u16 = std::ptr::null_mut();
    // SAFETY: `raw` receives a CoTaskMem-allocated, NUL-terminated wide string on success; it is
    // read once and freed below. A null token means the current user.
    let hr =
        unsafe { SHGetKnownFolderPath(&FOLDERID_LocalAppData, 0, std::ptr::null_mut(), &mut raw) };
    if hr != 0 || raw.is_null() {
        if !raw.is_null() {
            // SAFETY: allocated by the API above.
            unsafe { CoTaskMemFree(raw.cast()) };
        }
        return None;
    }
    // SAFETY: NUL-terminated per the API contract.
    let path = unsafe {
        let mut len = 0usize;
        while *raw.add(len) != 0 {
            len += 1;
        }
        let wide = std::slice::from_raw_parts(raw, len);
        let os = std::ffi::OsString::from_wide(wide);
        CoTaskMemFree(raw.cast());
        PathBuf::from(os)
    };
    (!path.as_os_str().is_empty()).then_some(path)
}

#[cfg(not(windows))]
pub(crate) fn known_local_app_data() -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(v: &str) -> Result<Option<PathBuf>, ()> {
        from_value(Some(OsStr::new(v)))
    }

    #[test]
    fn unset_or_blank_means_no_override() {
        assert_eq!(from_value(None), Ok(None));
        assert_eq!(check(""), Ok(None));
        assert_eq!(check("   "), Ok(None));
    }

    #[test]
    fn accepts_absolute_local_drive_paths() {
        assert_eq!(
            check("C:\\probe\\p1"),
            Ok(Some(PathBuf::from("C:\\probe\\p1")))
        );
        assert!(check("D:/probe").is_ok());
        assert!(
            check("X:\\Temp\\a..b").is_ok(),
            "`..` inside a name is fine"
        );
    }

    #[test]
    fn rejects_everything_that_is_not_an_absolute_local_drive_path() {
        for bad in [
            "probe",
            "..\\probe",
            "C:",
            "C:probe",
            "\\\\server\\share\\p",
            "//server/share/p",
            "\\\\?\\UNC\\server\\share\\p",
            "\\\\?\\C:\\probe",
            "\\\\.\\pipe\\x",
            "\\??\\C:\\x",
            "C:\\a\\..\\b",
            "C:/a/../b",
            "C:\\x\\DavWWWRoot\\y",
            "C:\\x@SSL\\y",
            " C:\\probe",
        ] {
            assert_eq!(check(bad), Err(()), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn invalid_override_names_the_variable_not_the_value() {
        assert_eq!(InvalidOverride(ENV_VAR).0, "FERRUM_PROFILE_DIR");
    }

    /// FER-462 R1 (review S1): the dialog only on a visible station, outside CI, with stderr not
    /// redirected; every other combination writes stderr only.
    #[test]
    fn the_dialog_is_shown_only_when_someone_can_dismiss_it() {
        assert!(should_show_dialog(true, false, false));
        for (station, ci, redirected) in [
            (false, false, false),
            (true, true, false),
            (true, false, true),
            (false, true, true),
            (true, true, true),
            (false, false, true),
            (false, true, false),
        ] {
            assert!(
                !should_show_dialog(station, ci, redirected),
                "{station} {ci} {redirected}"
            );
        }
    }

    /// FER-462 R1: the message box text names the variable and the rule, never a value.
    #[test]
    fn invalid_override_message_explains_the_rule_without_a_value() {
        for name in [ENV_VAR, ACTIVITY_ENV_VAR] {
            let text = invalid_override_message(name);
            assert!(text.contains(name), "{text}");
            assert!(text.contains(r"C:\ferrum-uji"), "{text}");
            assert!(text.contains(r"\\server\share"), "{text}");
            assert!(text.contains("tidak dijalankan"), "{text}");
        }
    }

    #[test]
    fn set_value_roots_every_store() {
        let root = PathBuf::from("X:/probe");
        assert_eq!(config_in(&root), PathBuf::from("X:/probe").join("config"));
        assert_eq!(data_in(&root), PathBuf::from("X:/probe").join("data"));
        assert_eq!(
            activity_in(&root),
            PathBuf::from("X:/probe").join("activity")
        );
        assert_eq!(local_in(&root), PathBuf::from("X:/probe").join("local"));
        assert_eq!(webview_in(&root), PathBuf::from("X:/probe").join("webview"));
    }

    /// Automation P0.3: `resolve_local_data_dir` honours `FERRUM_PROFILE_DIR` - the store then
    /// lives under `<profile>\local`, never in the real user profile.
    #[test]
    fn local_data_dir_prefers_the_test_profile() {
        let root = Path::new(r"X:\probe");
        assert_eq!(
            resolve_local_data_dir_in(Some(root), Some(Path::new(r"F:\lad"))),
            Some(PathBuf::from(r"X:\probe\local"))
        );
        assert_eq!(
            resolve_local_data_dir_in(Some(root), None),
            Some(PathBuf::from(r"X:\probe\local"))
        );
    }

    /// Without a profile the result is the known local-app-data folder plus the app identifier
    /// (Tauri's `app_local_data_dir` composition). When the folder cannot be resolved (or is not
    /// absolute) there is no result at all - never the temp dir.
    #[test]
    fn local_data_dir_has_no_silent_fallback() {
        assert_eq!(
            resolve_local_data_dir_in(None, Some(Path::new(r"F:\lad"))),
            Some(PathBuf::from(r"F:\lad").join(APP_DIR_ID))
        );
        assert_eq!(resolve_local_data_dir_in(None, None), None);
        assert_eq!(resolve_local_data_dir_in(None, Some(Path::new(""))), None);
        assert_eq!(
            resolve_local_data_dir_in(None, Some(Path::new("relative"))),
            None
        );
    }

    /// The Known Folder API names an absolute folder that is the one the environment also names
    /// (the environment is only used here as a cross-check, never by the code under test).
    #[cfg(windows)]
    #[test]
    fn the_known_folder_is_the_local_app_data_folder() {
        let known = known_local_app_data().expect("LocalAppData resolves");
        assert!(known.is_absolute());
        assert!(known.is_dir());
        if let Some(env) = std::env::var_os("LOCALAPPDATA") {
            assert_eq!(
                known.to_string_lossy().to_lowercase(),
                PathBuf::from(env).to_string_lossy().to_lowercase()
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn the_system_drive_is_local() {
        assert!(is_local_drive(Path::new("C:\\x")));
    }
}
