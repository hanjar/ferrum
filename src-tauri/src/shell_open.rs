//! F2-25: launching files with their OS association, the Associate With
//! picker, and Open Terminal Here — plus the F2-27 print verb.
//!
//! Safety contract (spec F2-25):
//! - Launching uses platform launch APIs with STRUCTURED arguments only.
//!   Paths never pass through a shell and are never concatenated into a
//!   command string; the terminal command is built as program + arg array +
//!   working directory, so spaces, Unicode, quotes and metacharacters in a
//!   path are passed verbatim as one argument (injection-proof by design).
//! - System associations are never changed silently: "Associate With"
//!   invokes the OS picker verb (`openas`), so the USER decides in the
//!   dialog. Ferrum has no write path into the registry/mimeapps at all.
//! - Unsupported platforms (e.g. the picker on non-Windows) surface a
//!   structured `unsupported` error instead of pretending to work; a
//!   missing terminal executable surfaces `notFound` (the UI prompts).
//! - Internal associations are deliberately NOT a second open path: Enter
//!   always goes to the OS. Ferrum's only "internal" handlers are the
//!   clearly registered preview/editor surfaces (Quick View via the kind
//!   registry in `src/lib/kind.ts`).
//!
//! The pure builders (`to_wide`, `terminal_candidates`, `se_err`) are unit
//! tested; the platform `ShellExecute`/spawn calls are one-liners over them
//! (see docs/verification-matrix.md for what is host-verified).

use crate::domain::{ErrorCode, FsError};

/// Encode as a NUL-terminated UTF-16 buffer for the `*W` launch APIs.
pub fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// ShellExecuteW returns ≤ 32 on failure; these are its SE_ERR_* codes.
/// When ShellExecuteExW fails, GetLastError() returns standard Win32 error codes.
pub fn se_err(code: isize, verb: &str, path: &str) -> FsError {
    let (err_code, message) = match code {
        2 | 3 => (ErrorCode::NotFound, "File or path not found"),
        5 => (ErrorCode::PermissionDenied, "Access denied"),
        27 => (ErrorCode::NotFound, "Association is incomplete or invalid"),
        31 | 1155 => (
            ErrorCode::NotFound,
            "No application is associated with this file for the requested action",
        ),
        1223 => (
            ErrorCode::Cancelled,
            "The operation was canceled by the user",
        ),
        _ => (ErrorCode::Io, "The platform launch API failed"),
    };
    FsError::new(
        err_code,
        format!("{message} (verb {verb:?}, SE_ERR {code})"),
    )
    .with_path(path)
}

/// Terminal choice persisted per OS in the preferences (`auto` resolves to
/// the platform default list; a named program must exist or the launch
/// reports `notFound` so the UI can prompt).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalPref {
    Auto,
    WindowsTerminal,
    Cmd,
    PowerShell,
}

impl TerminalPref {
    /// Parse the preference string; unknown values fall back to Auto.
    pub fn parse(s: &str) -> Self {
        match s {
            "wt" => Self::WindowsTerminal,
            "cmd" => Self::Cmd,
            "powershell" => Self::PowerShell,
            _ => Self::Auto,
        }
    }
}

/// FER-494 T2: Windows Terminal is started by ABSOLUTE path, never by a bare `wt` search (a
/// writable folder early on PATH could plant a `wt.exe`). Candidates: the App Execution alias
/// `%LOCALAPPDATA%\Microsoft\WindowsApps\wt.exe`, then `wt.exe` in absolute PATH folders. The
/// alias is an `IO_REPARSE_TAG_APPEXECLINK` reparse point (not a name surrogate): its own
/// security descriptor is read (owned by the user, in the user's WindowsApps folder) and the
/// package behind it lives under TrustedInstaller-protected `Program Files\WindowsApps`.
pub fn find_wt(
    env: &dyn Fn(&str) -> Option<std::ffi::OsString>,
    exists: &dyn Fn(&std::path::Path) -> bool,
) -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    let mut cands: Vec<PathBuf> = Vec::new();
    if let Some(local) = env("LOCALAPPDATA") {
        cands.push(
            PathBuf::from(local)
                .join("Microsoft")
                .join("WindowsApps")
                .join("wt.exe"),
        );
    }
    if let Some(path) = env("PATH") {
        cands.extend(
            std::env::split_paths(&path)
                .filter(|p| p.is_absolute())
                .map(|p| p.join("wt.exe")),
        );
    }
    cands.into_iter().find(|p| exists(p))
}

/// The `wt.exe` found, if its location passes the agent ACL rules (FER-490 E3); else `None`
/// (then the console route is used).
pub fn trusted_wt(
    found: Option<std::path::PathBuf>,
    trusted: &dyn Fn(&std::path::Path) -> Result<(), String>,
) -> Option<std::path::PathBuf> {
    found.filter(|p| trusted(p).is_ok())
}

/// `wt.exe` of this PC, resolved and ACL-checked now.
pub fn resolve_wt() -> Option<std::path::PathBuf> {
    // `symlink_metadata`: the alias reparse point itself (nothing is followed).
    let found = find_wt(&|k| std::env::var_os(k), &|p| {
        std::fs::symlink_metadata(p).is_ok_and(|m| !m.is_dir())
    });
    trusted_wt(found, &|p| crate::ai::agent_cli::acl::trusted_location(p))
}

/// Ordered launch candidates for the preference, with Windows Terminal resolved and checked now.
pub fn terminal_candidates(pref: TerminalPref, cwd: &str) -> Vec<std::process::Command> {
    terminal_candidates_with(pref, cwd, resolve_wt().as_deref())
}

/// Ordered launch candidates for the preference. `Auto` tries the modern
/// terminal first and falls back to the classic console; every candidate is
/// a structured (program, args, cwd) launch — never a shell command line.
/// The path only ever travels as the process working directory, so
/// spaces/Unicode/quotes/metacharacters survive verbatim. `wt` = the trusted
/// absolute `wt.exe` (`None`: no Windows Terminal candidate).
#[cfg(windows)]
pub fn terminal_candidates_with(
    pref: TerminalPref,
    cwd: &str,
    wt: Option<&std::path::Path>,
) -> Vec<std::process::Command> {
    fn candidate(program: &std::ffi::OsStr, args: &[&str], cwd: &str) -> std::process::Command {
        use std::os::windows::process::CommandExt;
        // Without CREATE_NEW_CONSOLE a console child inherits the parent's
        // console — invisible when Ferrum itself runs attached to one
        // (e.g. `tauri dev` from a terminal). 0x10 = CREATE_NEW_CONSOLE.
        const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
        let mut c = std::process::Command::new(program);
        c.args(args).current_dir(cwd);
        c.creation_flags(CREATE_NEW_CONSOLE);
        c
    }
    // Windows Terminal gets `-d .` and the folder as its working
    // directory: wt splits its command line on `;`, so a folder such as
    // `C:\a;b` must never be one of its arguments (FER-494, verified on
    // the host). `cmd /K` and `powershell -NoExit` keep the window open
    // and inherit the working directory.
    let wt_cmd = || wt.map(|w| candidate(w.as_os_str(), &["-d", "."], cwd));
    let cmd = || candidate("cmd".as_ref(), &["/K"], cwd);
    match pref {
        TerminalPref::Auto => wt_cmd().into_iter().chain([cmd()]).collect(),
        TerminalPref::WindowsTerminal => wt_cmd().into_iter().collect(),
        TerminalPref::Cmd => vec![cmd()],
        TerminalPref::PowerShell => vec![candidate("powershell".as_ref(), &["-NoExit"], cwd)],
    }
}

#[cfg(not(windows))]
pub fn terminal_candidates_with(
    pref: TerminalPref,
    cwd: &str,
    _wt: Option<&std::path::Path>,
) -> Vec<std::process::Command> {
    fn candidate(program: &str, args: &[&str], cwd: &str) -> std::process::Command {
        let mut c = std::process::Command::new(program);
        c.args(args).current_dir(cwd);
        c
    }
    // Untested on this host (Windows-only verification, decision D01) but
    // honest: structured args, no shell, first-found wins.
    match pref {
        TerminalPref::Auto => {
            if cfg!(target_os = "macos") {
                vec![candidate("open", &["-a", "Terminal", cwd], cwd)]
            } else {
                vec![
                    candidate(
                        "gnome-terminal",
                        &[&format!("--working-directory={cwd}")],
                        cwd,
                    ),
                    candidate("konsole", &["--workdir", cwd], cwd),
                    candidate("xterm", &[], cwd),
                ]
            }
        }
        TerminalPref::WindowsTerminal => vec![candidate("wt", &["-d", cwd], cwd)],
        TerminalPref::Cmd => vec![candidate("cmd", &["/K"], cwd)],
        TerminalPref::PowerShell => vec![candidate("pwsh", &["-NoExit"], cwd)],
    }
}

/// Spawn the first candidate that starts. A missing executable (`notFound`)
/// tries the next fallback; when none starts, the LAST structured error is
/// reported so the UI can prompt about the missing terminal.
pub fn spawn_terminal(pref: TerminalPref, cwd: &str) -> Result<(), FsError> {
    let mut last: Option<FsError> = None;
    for mut cmd in terminal_candidates(pref, cwd) {
        match cmd.spawn() {
            Ok(_) => return Ok(()),
            Err(e) => last = Some(FsError::from(e).with_path(cwd)),
        }
    }
    Err(last.unwrap_or_else(|| {
        FsError::new(
            ErrorCode::NotFound,
            "No terminal is available (Windows Terminal not found or its location is not trusted)",
        )
        .with_path(cwd)
    }))
}

// ── Platform launch (ShellExecute) ───────────────────────────────────────

/// The verb actually used for a launch. `open` (default association),
/// `openas` (OS picker — the USER chooses, nothing is changed silently),
/// and `print` (F2-27 hand-off to the associated application).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchVerb {
    Open,
    OpenAs,
    Print,
}

impl LaunchVerb {
    pub fn as_str(self) -> &'static str {
        match self {
            LaunchVerb::Open => "open",
            LaunchVerb::OpenAs => "openas",
            LaunchVerb::Print => "print",
        }
    }
}

#[cfg(windows)]
#[derive(Debug)]
pub struct Pidl(*mut windows_sys::Win32::UI::Shell::Common::ITEMIDLIST);

#[cfg(windows)]
// Pidl owns an exclusive pointer to an ITEMIDLIST allocated on the COM task allocator.
// It is safe to transfer ownership across threads (Send) because the memory is exclusively owned
// and freed once on drop via CoTaskMemFree (which is safe to call from any thread).
// Sync is deliberately not implemented because shared concurrent access across threads is not supported.
unsafe impl Send for Pidl {}

#[cfg(windows)]
impl Pidl {
    /// Takes ownership of a raw `ITEMIDLIST` pointer allocated via COM / shell allocator.
    ///
    /// # Safety
    /// The caller must guarantee that `ptr` is a valid pointer allocated by `CoTaskMemAlloc`
    /// (or null). Ownership is transferred to this `Pidl` instance, which will free it via
    /// `CoTaskMemFree` exactly once when dropped.
    pub unsafe fn from_raw(ptr: *mut windows_sys::Win32::UI::Shell::Common::ITEMIDLIST) -> Self {
        Self(ptr)
    }

    /// Borrows the raw `ITEMIDLIST` pointer.
    pub fn as_ptr(&self) -> *mut windows_sys::Win32::UI::Shell::Common::ITEMIDLIST {
        self.0
    }
}

#[cfg(windows)]
impl Drop for Pidl {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                windows_sys::Win32::System::Com::CoTaskMemFree(self.0 as _);
            }
            self.0 = std::ptr::null_mut();
        }
    }
}

#[derive(Debug)]
pub struct LaunchTarget {
    #[cfg(windows)]
    pub pidl: Pidl,
    pub resolved_path: String,
    pub item_name: String,
}

impl LaunchTarget {
    pub fn resolved_path(&self) -> &str {
        &self.resolved_path
    }

    pub fn item_name(&self) -> &str {
        &self.item_name
    }

    #[cfg(windows)]
    pub fn pidl(&self) -> &Pidl {
        &self.pidl
    }
}

impl std::ops::Deref for LaunchTarget {
    type Target = str;
    fn deref(&self) -> &str {
        &self.resolved_path
    }
}

impl AsRef<str> for LaunchTarget {
    fn as_ref(&self) -> &str {
        &self.resolved_path
    }
}

impl AsRef<std::path::Path> for LaunchTarget {
    fn as_ref(&self) -> &std::path::Path {
        std::path::Path::new(&self.resolved_path)
    }
}

#[cfg(windows)]
pub fn ensure_com_initialized() {
    use windows_sys::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
    unsafe {
        let _ = CoInitializeEx(std::ptr::null_mut(), COINIT_APARTMENTTHREADED as u32);
    }
}

#[cfg(windows)]
pub fn parse_pidl(path: &str) -> Result<Pidl, FsError> {
    use windows_sys::Win32::UI::Shell::SHParseDisplayName;
    let wide = to_wide(path);
    let mut pidl = std::ptr::null_mut();
    let hr = unsafe {
        SHParseDisplayName(
            wide.as_ptr(),
            std::ptr::null_mut(),
            &mut pidl,
            0,
            std::ptr::null_mut(),
        )
    };
    if hr != 0 || pidl.is_null() {
        return Err(FsError::new(
            ErrorCode::NotFound,
            format!("Gagal me-resolve target '{path}': HRESULT 0x{hr:08X}"),
        )
        .with_path(path));
    }
    Ok(unsafe { Pidl::from_raw(pidl) })
}

#[cfg(windows)]
pub fn get_resolved_path_from_pidl(pidl: &Pidl) -> Result<String, FsError> {
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::{SHGetNameFromIDList, SIGDN_FILESYSPATH};

    let mut name_ptr = std::ptr::null_mut();
    let hr = unsafe { SHGetNameFromIDList(pidl.as_ptr() as _, SIGDN_FILESYSPATH, &mut name_ptr) };
    if hr != 0 || name_ptr.is_null() {
        return Err(FsError::new(
            ErrorCode::InvalidPath,
            format!("Gagal mendapatkan path dari IDList: HRESULT 0x{hr:08X}"),
        ));
    }
    let mut len = 0;
    unsafe {
        while *name_ptr.add(len) != 0 {
            len += 1;
        }
    }
    let slice = unsafe { std::slice::from_raw_parts(name_ptr, len) };
    let path = String::from_utf16_lossy(slice);
    unsafe {
        CoTaskMemFree(name_ptr as _);
    }
    Ok(path)
}

#[cfg(windows)]
pub fn get_item_name_from_pidl(pidl: &Pidl) -> Result<String, FsError> {
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::{SHGetNameFromIDList, SIGDN_PARENTRELATIVEPARSING};

    let mut name_ptr = std::ptr::null_mut();
    let hr = unsafe {
        SHGetNameFromIDList(
            pidl.as_ptr() as _,
            SIGDN_PARENTRELATIVEPARSING,
            &mut name_ptr,
        )
    };
    if hr != 0 || name_ptr.is_null() {
        return Err(FsError::new(
            ErrorCode::InvalidPath,
            format!("Gagal mendapatkan nama item dari IDList: HRESULT 0x{hr:08X}"),
        ));
    }
    let mut len = 0;
    unsafe {
        while *name_ptr.add(len) != 0 {
            len += 1;
        }
    }
    let slice = unsafe { std::slice::from_raw_parts(name_ptr, len) };
    let name = String::from_utf16_lossy(slice);
    unsafe {
        CoTaskMemFree(name_ptr as _);
    }
    Ok(name)
}

#[cfg(windows)]
pub fn resolve_target(path: &str) -> Result<LaunchTarget, FsError> {
    ensure_com_initialized();
    let checked = external_target(path)?;
    let pidl = parse_pidl(&checked)?;
    let resolved = get_resolved_path_from_pidl(&pidl)?;
    let item_name = get_item_name_from_pidl(&pidl)?;
    if resolved
        .chars()
        .any(|c| c.is_control() || crate::hotlist::model::is_format_char(c))
        || item_name
            .chars()
            .any(|c| c.is_control() || crate::hotlist::model::is_format_char(c))
    {
        return Err(refused_target());
    }
    if has_ads(&resolved) || has_ads(&item_name) {
        return Err(refused_target());
    }
    Ok(LaunchTarget {
        pidl,
        resolved_path: resolved,
        item_name,
    })
}

#[cfg(not(windows))]
pub fn resolve_target(path: &str) -> Result<LaunchTarget, FsError> {
    let checked = external_target(path)?;
    if checked
        .chars()
        .any(|c| c.is_control() || crate::hotlist::model::is_format_char(c))
    {
        return Err(refused_target());
    }
    if has_ads(&checked) {
        return Err(refused_target());
    }
    let item_name = std::path::Path::new(&checked)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();
    Ok(LaunchTarget {
        resolved_path: checked,
        item_name,
    })
}

#[cfg(windows)]
pub fn shell_execute_pidl(verb: LaunchVerb, target: &LaunchTarget) -> Result<(), FsError> {
    use windows_sys::Win32::UI::Shell::{
        SEE_MASK_FLAG_NO_UI, SEE_MASK_INVOKEIDLIST, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW,
        ShellExecuteExW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let verb_wide = to_wide(verb.as_str());
    let mut sei: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    sei.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    let mut mask = SEE_MASK_INVOKEIDLIST | SEE_MASK_NOASYNC;
    if verb != LaunchVerb::OpenAs {
        mask |= SEE_MASK_FLAG_NO_UI;
    }
    sei.fMask = mask;
    sei.hwnd = std::ptr::null_mut();
    sei.lpVerb = verb_wide.as_ptr();
    sei.lpFile = std::ptr::null();
    sei.lpParameters = std::ptr::null();
    sei.lpDirectory = std::ptr::null();
    sei.nShow = SW_SHOWNORMAL;
    sei.lpIDList = target.pidl.as_ptr() as _;

    let ok = unsafe { ShellExecuteExW(&mut sei) };
    if ok != 0 {
        Ok(())
    } else {
        let err = std::io::Error::last_os_error();
        let code = err.raw_os_error().unwrap_or(0);
        Err(se_err(code as isize, verb.as_str(), target.resolved_path()))
    }
}

#[cfg(not(windows))]
pub fn shell_execute_pidl(verb: LaunchVerb, target: &LaunchTarget) -> Result<(), FsError> {
    match verb {
        LaunchVerb::OpenAs | LaunchVerb::Print => Err(FsError::new(
            ErrorCode::Unsupported,
            format!("The {} action is only available on Windows", verb.as_str()),
        )
        .with_path(target.resolved_path())),
        LaunchVerb::Open => {
            let mut cmd = if cfg!(target_os = "macos") {
                let mut c = std::process::Command::new("open");
                c.arg(target.resolved_path());
                c
            } else {
                let mut c = std::process::Command::new("xdg-open");
                c.arg(target.resolved_path());
                c
            };
            cmd.spawn()
                .map(|_| ())
                .map_err(|e| FsError::from(e).with_path(target.resolved_path()))
        }
    }
}

pub fn shell_execute(verb: LaunchVerb, path: &str) -> Result<(), FsError> {
    let target = resolve_target(path)?;
    shell_execute_pidl(verb, &target)
}

pub fn shell_execute_target(target: &LaunchTarget) -> Result<bool, FsError> {
    shell_execute_pidl(LaunchVerb::Open, target).map(|_| true)
}

/// FER-467 R5: the only things `open_external`, Associate With, print and `shell_open` hand to
/// ShellExecute: an EXISTING file or folder on a drive (`X:\...`) or on a network share
/// (`\\host\share\...`, Ferrum browses shares), as accepted by `path_guard::check_str`.
/// Refused before anything is launched: URI schemes and protocol handlers (`https:`, `ms-msdt:`,
/// `search-ms:`, `file:`, `shell:`, ...), bare or relative names (App Paths / PATH lookups),
/// device and NT namespaces, the `pipe` / `mailslot` / `IPC$` shares, alternate data streams, and
/// paths that do not exist. Returns the normalised path (`/` becomes `\`).
///
/// Long-path spellings (FER-467): `\\?\X:\...` is turned into `X:\...` and `\\?\UNC\host\share\...`
/// into `\\host\share\...` BEFORE the guard, so files deep in long paths still open while every
/// other `\\?\` / `\\.\` form (GLOBALROOT, volumes, devices) stays refused.
pub fn external_target(path: &str) -> Result<String, FsError> {
    let unprefixed = strip_long_prefix(path);
    if unprefixed
        .chars()
        .any(|c| c.is_control() || crate::hotlist::model::is_format_char(c))
    {
        return Err(refused_target());
    }
    let checked = crate::path_guard::check_str(&unprefixed).map_err(|_| refused_target())?;
    if std::fs::symlink_metadata(&checked).is_err() {
        return Err(FsError::new(ErrorCode::NotFound, "File or path not found").with_path(&checked));
    }
    Ok(checked)
}

/// `\\?\X:\rest` → `X:\rest`, `\\?\UNC\host\rest` → `\\host\rest` (either separator); anything
/// else is returned unchanged (and judged by the guard as it is).
fn strip_long_prefix(path: &str) -> String {
    let p = path.replace('/', "\\");
    let Some(rest) = p.strip_prefix(r"\\?\") else {
        return path.to_string();
    };
    let b = rest.as_bytes();
    if b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\' {
        return rest.to_string();
    }
    if rest.len() > 4 && rest[..4].eq_ignore_ascii_case(r"UNC\") {
        return format!(r"\\{}", &rest[4..]);
    }
    path.to_string()
}

fn refused_target() -> FsError {
    FsError::new(
        ErrorCode::InvalidPath,
        "Only existing files and folders can be opened",
    )
}

/// Fixed list of executable and risky target file extensions (FER-513).
/// Includes Windows default PATHEXT and additional script/executable types.
pub const RISKY_EXTENSIONS: &[&str] = &[
    // Windows default PATHEXT
    "com",
    "exe",
    "bat",
    "cmd",
    "vbs",
    "vbe",
    "js",
    "jse",
    "wsf",
    "wsh",
    "msc",
    // Additional risky extensions
    "lnk",
    "url",
    "pif",
    "scr",
    "cpl",
    "msi",
    "msp",
    "msix",
    "msixbundle",
    "appx",
    "appxbundle",
    "appref-ms",
    "application",
    "hta",
    "ps1",
    "psm1",
    "psd1",
    "reg",
    "inf",
    "jar",
    "chm",
    "settingcontent-ms",
    "library-ms",
    "search-ms",
    "scf",
    "gadget",
    "diagcab",
    "vb",
    "wsc",
    "sct",
    "xll",
    "xlam",
    "ppam",
    "iqy",
    "slk",
    "website",
    // Python / script associations (FER-513 Rework 1)
    "py",
    "pyw",
    "pyc",
    "pyo",
    "pyz",
    "pyzw",
    // Shell & deployment associations (FER-513 Rework 1)
    "wsb",
    "theme",
    "themepack",
    "deskthemepack",
    "msu",
    "appinstaller",
    "jnlp",
    // Database / Access / Office executable associations (FER-513 Rework 1)
    "mdb",
    "mde",
    "accdb",
    "accde",
    "accda",
    "ade",
    "adp",
    // XAML / ClickOnce / VSTO (FER-513 Rework 1)
    "xbap",
    "vsto",
];

fn has_ads(path: &str) -> bool {
    let p = strip_long_prefix(path);
    let b = p.as_bytes();
    let is_drive = b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\';
    if is_drive {
        p[2..].contains(':')
    } else {
        p.contains(':')
    }
}

/// FER-513: Pure function classifying whether a target path is an executable or risky file.
/// Case-insensitive, trailing dots and spaces trimmed from file name.
/// - Risky extension: Ok(true)
/// - Normal document / no extension / folder: Ok(false)
/// - ADS or format/bidi/control character: Err(InvalidPath)
pub fn is_risky_target(path: impl AsRef<std::path::Path>) -> Result<bool, FsError> {
    let p = path.as_ref();
    let s = p.to_string_lossy();
    if s.chars()
        .any(|c| c.is_control() || crate::hotlist::model::is_format_char(c))
    {
        return Err(refused_target());
    }
    if has_ads(&s) {
        return Err(refused_target());
    }
    let file_name = match p.file_name().and_then(|n| n.to_str()) {
        Some(name) => name,
        None => return Ok(false),
    };
    let trimmed = file_name.trim_end_matches(['.', ' ']);
    if trimmed.is_empty() {
        return Ok(false);
    }
    if let Some(dot_idx) = trimmed.rfind('.') {
        let ext = &trimmed[dot_idx + 1..];
        if ext.is_empty() {
            return Ok(false);
        }
        let ext_lower = ext.to_ascii_lowercase();
        if RISKY_EXTENSIONS.contains(&ext_lower.as_str()) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// FER-513 / FER-513 Rework 2: Classify whether a `LaunchTarget` is executable or risky.
///
/// Rules:
/// 1. Target is risky if EITHER the long item name (SIGDN_PARENTRELATIVEPARSING) OR the last component
///    of the filesystem path (SIGDN_FILESYSPATH) has a risky extension.
/// 2. Fail-closed: if the filesystem component differs from the long item name (case-insensitive)
///    and the target is NOT risky according to the long name, it is STILL considered risky
///    (e.g. 8.3 short name aliasing or path truncation).
/// 3. Control characters, bidi/format characters, or ADS in either resolved path or item name
///    are rejected immediately with `ErrorCode::InvalidPath`.
pub fn is_risky_launch_target(target: &LaunchTarget) -> Result<bool, FsError> {
    if target
        .resolved_path
        .chars()
        .any(|c| c.is_control() || crate::hotlist::model::is_format_char(c))
        || target
            .item_name
            .chars()
            .any(|c| c.is_control() || crate::hotlist::model::is_format_char(c))
    {
        return Err(refused_target());
    }
    if has_ads(&target.resolved_path) || has_ads(&target.item_name) {
        return Err(refused_target());
    }

    let item_name_risky = is_risky_target(&target.item_name)?;

    let fs_component = std::path::Path::new(&target.resolved_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    let fs_component_risky = if fs_component.is_empty() {
        false
    } else {
        is_risky_target(fs_component)?
    };

    if item_name_risky || fs_component_risky {
        return Ok(true);
    }

    // Fail-closed: kalau komponen terakhir path filesystem berbeda dari nama panjang
    // (case-insensitive) dan target tidak berisiko menurut nama panjang, target tetap dianggap berisiko.
    let fs_trimmed = fs_component.trim_end_matches(['.', ' ']);
    let item_trimmed = target.item_name.trim_end_matches(['.', ' ']);
    if !fs_trimmed.is_empty()
        && !item_trimmed.is_empty()
        && !fs_trimmed.eq_ignore_ascii_case(item_trimmed)
    {
        return Ok(true);
    }

    Ok(false)
}

/// Text for the native confirmation dialog before launching risky targets.
/// Displays the long item name and extension on dedicated lines above the full path.
pub fn confirm_prompt_text(path: &str, item_name: &str) -> String {
    let name = if !item_name.is_empty() {
        item_name
    } else {
        std::path::Path::new(path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
    };
    let trimmed = name.trim_end_matches(['.', ' ']);
    let ext = if let Some(dot_idx) = trimmed.rfind('.') {
        &trimmed[dot_idx + 1..]
    } else {
        ""
    };
    format!(
        "Jalankan program ini?\n\nNama: {name}\nJenis: .{ext}\nPath: {path}\n\nProgram dapat mengubah sistem atau file Anda. Jalankan hanya jika Anda memercayai file ini."
    )
}

/// The single app-wide one-dialog guard now lives in `native_dialog` (shared by every native
/// confirmation: hotlist, RAR path, AI endpoint, indexer mode, shell open, automation).
pub use crate::native_dialog::DialogGuard;

pub fn native_confirm_with_guard<F>(
    owner: isize,
    text: &str,
    confirm_fn: F,
) -> Result<bool, FsError>
where
    F: FnOnce(isize, &str) -> bool,
{
    let _guard = DialogGuard::try_acquire()?;
    Ok(confirm_fn(owner, text))
}

/// The raw message box (the caller already holds the guard, see [`native_confirm_with_guard`]).
pub fn native_confirm(owner: isize, text: &str) -> bool {
    crate::native_dialog::message_box(owner, "Konfirmasi menjalankan program", text)
}

/// Core launch function with injectable confirmation and launcher seams.
pub fn shell_open_with<C, L>(
    owner: isize,
    path: &str,
    confirm: C,
    launcher: L,
) -> Result<bool, FsError>
where
    C: FnOnce(isize, &str) -> Result<bool, FsError>,
    L: FnOnce(&LaunchTarget) -> Result<bool, FsError>,
{
    let target = resolve_target(path)?;
    let is_risky = is_risky_launch_target(&target)?;
    if is_risky {
        let prompt_text = confirm_prompt_text(target.resolved_path(), target.item_name());
        let confirmed = confirm(owner, &prompt_text)?;
        if !confirmed {
            return Err(
                FsError::new(ErrorCode::Cancelled, "Operasi dibatalkan oleh pengguna")
                    .with_path(target.resolved_path()),
            );
        }
    }
    launcher(&target)
}

/// Open a file or folder with injectable confirmation and launcher seams.
pub fn open_external_with<C, L>(
    owner: isize,
    path: &str,
    confirm: C,
    launcher: L,
) -> Result<(), FsError>
where
    C: FnOnce(isize, &str) -> Result<bool, FsError>,
    L: FnOnce(LaunchVerb, &LaunchTarget) -> Result<(), FsError>,
{
    shell_open_with(owner, path, confirm, |target| {
        launcher(LaunchVerb::Open, target).map(|_| true)
    })
    .map(|_| ())
}

/// Open a file or folder with its OS default association (window-bound).
pub fn open_external_in(owner: isize, path: &str) -> Result<(), FsError> {
    open_external_with(
        owner,
        path,
        |o, text| native_confirm_with_guard(o, text, native_confirm),
        shell_execute_pidl,
    )
}

/// Open a file or folder with its OS default association.
pub fn open_external(path: &str) -> Result<(), FsError> {
    open_external_in(0, path)
}

/// Quote one argument for a Windows command line (CommandLineToArgvW rules), so an argument with
/// spaces, quotes or trailing backslashes reaches the program as exactly one argument.
pub fn quote_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.chars().any(|c| c.is_whitespace() || c == '"') {
        return arg.to_string();
    }
    let mut out = String::from("\"");
    let mut backslashes = 0usize;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.push_str(&"\\".repeat(backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.push_str(&"\\".repeat(backslashes * 2));
    out.push('"');
    out
}

/// FER-458: launch a program/document with separate arguments (Directory Hotlist command items).
/// Windows: ShellExecuteW "open" with a quoted parameter string; other OSes: argv spawn. Never a
/// shell string.
#[cfg(windows)]
pub fn launch_program(command: &str, args: &[String]) -> Result<(), FsError> {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let verb = to_wide("open");
    let file = to_wide(command);
    let params = args
        .iter()
        .map(|a| quote_arg(a))
        .collect::<Vec<_>>()
        .join(" ");
    let params_wide = to_wide(&params);
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            if args.is_empty() {
                std::ptr::null()
            } else {
                params_wide.as_ptr()
            },
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    let code = result as isize;
    if code > 32 {
        Ok(())
    } else {
        Err(se_err(code, "open", command))
    }
}

#[cfg(not(windows))]
pub fn launch_program(command: &str, args: &[String]) -> Result<(), FsError> {
    std::process::Command::new(command)
        .args(args)
        .spawn()
        .map(|_| ())
        .map_err(|e| FsError::from(e).with_path(command))
}

/// Open the OS "Associate With…" picker for one file. The picker is the
/// explicit user action — Ferrum itself never writes an association.
pub fn associate_with(path: &str) -> Result<(), FsError> {
    let target = resolve_target(path)?;
    shell_execute_pidl(LaunchVerb::OpenAs, &target)
}

/// Hand a file to its associated application's print verb (F2-27).
pub fn print_file(path: &str) -> Result<(), FsError> {
    let target = resolve_target(path)?;
    if is_risky_launch_target(&target)? {
        return Err(FsError::new(
            ErrorCode::Unsupported,
            "Printing executable or risky targets is not supported",
        )
        .with_path(target.resolved_path()));
    }
    shell_execute_pidl(LaunchVerb::Print, &target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_arg_follows_command_line_to_argv_rules() {
        assert_eq!(quote_arg("plain"), "plain");
        assert_eq!(quote_arg(""), "\"\"");
        assert_eq!(quote_arg("a b"), "\"a b\"");
        assert_eq!(quote_arg("say \"hi\""), "\"say \\\"hi\\\"\"");
        assert_eq!(quote_arg(r"C:\dir with space\"), r#""C:\dir with space\\""#);
        assert_eq!(quote_arg(r"C:\nospace\"), r"C:\nospace\");
    }

    /// Same vectors as the frontend (`src/lib/hotlist/model.test.ts`): the confirmation dialog shows
    /// exactly the command line this function builds.
    #[test]
    fn quote_arg_matches_the_shared_contract_vectors() {
        let cases: Vec<serde_json::Value> = serde_json::from_str(include_str!(
            "../../contract-fixtures/hotlist/quote-arg.json"
        ))
        .unwrap();
        assert!(cases.len() >= 9);
        for case in cases {
            let arg = case["arg"].as_str().unwrap();
            assert_eq!(quote_arg(arg), case["quoted"].as_str().unwrap(), "{arg:?}");
        }
    }

    #[cfg(windows)]
    const WT: &str = r"C:\Users\u\AppData\Local\Microsoft\WindowsApps\wt.exe";

    /// FER-494 T2: wt only by absolute path, only when its location is trusted; otherwise the
    /// console route (or nothing for an explicit wt preference).
    #[cfg(windows)]
    #[test]
    fn wt_is_resolved_absolutely_and_skipped_when_untrusted() {
        use std::ffi::OsString;
        let env = |k: &str| -> Option<OsString> {
            match k {
                "LOCALAPPDATA" => Some(r"C:\Users\u\AppData\Local".into()),
                "PATH" => Some(r"relative;C:\Evil;C:\Tools".into()),
                _ => None,
            }
        };
        let alias = std::path::PathBuf::from(WT);
        assert_eq!(find_wt(&env, &|p| p == alias), Some(alias.clone()));
        // No alias: the first absolute PATH folder with wt.exe (relative entries skipped).
        let evil = std::path::PathBuf::from(r"C:\Evil\wt.exe");
        assert_eq!(find_wt(&env, &|p| p == evil), Some(evil.clone()));
        assert_eq!(find_wt(&env, &|p| p.starts_with("relative")), None);
        assert_eq!(find_wt(&env, &|_| false), None);
        // The ACL check decides.
        let bad = |_: &std::path::Path| Err("ditolak".to_owned());
        let ok = |_: &std::path::Path| Ok(());
        assert_eq!(trusted_wt(Some(evil.clone()), &bad), None);
        assert_eq!(trusted_wt(Some(alias.clone()), &ok), Some(alias));
        // Untrusted wt: Auto uses cmd only; an explicit wt preference has no candidate.
        let auto = terminal_candidates_with(TerminalPref::Auto, r"C:\w", None);
        assert_eq!(auto.len(), 1);
        assert_eq!(program_of(&auto[0]), "cmd");
        assert!(terminal_candidates_with(TerminalPref::WindowsTerminal, r"C:\w", None).is_empty());
    }

    #[cfg(windows)]
    fn program_of(cmd: &std::process::Command) -> String {
        cmd.get_program().to_string_lossy().to_string()
    }

    #[cfg(windows)]
    fn args_of(cmd: &std::process::Command) -> Vec<String> {
        cmd.get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect()
    }

    #[test]
    fn to_wide_appends_a_terminator_and_keeps_unicode() {
        assert_eq!(to_wide("ab"), vec![b'a' as u16, b'b' as u16, 0]);
        assert_eq!(to_wide("hé"), vec![0x68, 0xE9, 0]);
        assert_eq!(to_wide(""), vec![0]);
    }

    #[cfg(windows)]
    #[test]
    fn wt_candidate_passes_cwd_only_as_the_working_directory() {
        // Spaces, Unicode, quote/metacharacters and UNC: the folder is the
        // process working directory only, never a wt argument (wt splits
        // its command line on `;`).
        for cwd in [
            r"C:\Program Files (x86)\tools",
            r"D:\béhind thé scenes\dir",
            r#"C:\weird &|;^%$ path'"x"#,
            r"\\server\share\some folder",
        ] {
            let cmds = terminal_candidates_with(TerminalPref::Auto, cwd, Some(WT.as_ref()));
            assert!(cmds.len() >= 2, "auto resolves to wt + cmd fallback");
            let wt = &cmds[0];
            assert_eq!(program_of(wt), WT);
            assert_eq!(args_of(wt), vec!["-d".to_string(), ".".to_string()]);
            assert_eq!(wt.get_current_dir(), Some(std::path::Path::new(cwd)));
            let fallback = &cmds[1];
            assert_eq!(program_of(fallback), "cmd");
            assert_eq!(args_of(fallback), vec!["/K"]);
            assert_eq!(fallback.get_current_dir(), Some(std::path::Path::new(cwd)));
        }
    }

    #[cfg(windows)]
    #[test]
    fn explicit_terminal_preferences_build_their_documented_commands() {
        let cwd = r"C:\some dir\é";
        let wt =
            &terminal_candidates_with(TerminalPref::WindowsTerminal, cwd, Some(WT.as_ref()))[0];
        assert_eq!(program_of(wt), WT);
        assert_eq!(args_of(wt), vec!["-d", "."]);
        assert_eq!(wt.get_current_dir(), Some(std::path::Path::new(cwd)));

        let cmd = &terminal_candidates_with(TerminalPref::Cmd, cwd, Some(WT.as_ref()))[0];
        assert_eq!(program_of(cmd), "cmd");
        assert_eq!(args_of(cmd), vec!["/K"]);
        assert_eq!(cmd.get_current_dir(), Some(std::path::Path::new(cwd)));

        let ps = &terminal_candidates_with(TerminalPref::PowerShell, cwd, Some(WT.as_ref()))[0];
        assert_eq!(program_of(ps), "powershell");
        assert_eq!(args_of(ps), vec!["-NoExit"]);
    }

    #[cfg(windows)]
    #[test]
    fn unknown_preference_falls_back_to_auto() {
        let cmds = terminal_candidates_with(
            TerminalPref::parse("gnome-terminal"),
            r"C:\t",
            Some(WT.as_ref()),
        );
        // Falls back to the Auto list (wt first, cmd fallback) instead of
        // launching an executable that cannot exist on Windows.
        assert_eq!(program_of(&cmds[0]), WT);
    }

    #[cfg(not(windows))]
    #[test]
    fn auto_candidates_are_structured_on_unix_like_hosts() {
        let cwd = "/home/arif/some dir é";
        let cmds = terminal_candidates_with(TerminalPref::Auto, cwd, None);
        assert!(!cmds.is_empty());
        for c in &cmds {
            assert_eq!(c.get_current_dir(), Some(std::path::Path::new(cwd)));
        }
    }

    #[test]
    fn shell_errors_map_to_structured_codes() {
        let e = se_err(2, "open", r"C:\missing.txt");
        assert_eq!(e.code, ErrorCode::NotFound);
        assert_eq!(e.path.as_deref(), Some(r"C:\missing.txt"));
        assert_eq!(se_err(5, "print", "p").code, ErrorCode::PermissionDenied);
        assert_eq!(se_err(31, "open", "p").code, ErrorCode::NotFound);
        assert_eq!(
            se_err(31, "open", "p").message,
            "No application is associated with this file for the requested action (verb \"open\", SE_ERR 31)"
        );
        assert_eq!(se_err(1155, "open", "p").code, ErrorCode::NotFound);
        assert_eq!(
            se_err(1155, "open", "p").message,
            "No application is associated with this file for the requested action (verb \"open\", SE_ERR 1155)"
        );
        assert_eq!(se_err(1223, "openas", "p").code, ErrorCode::Cancelled);
        assert_eq!(
            se_err(1223, "openas", "p").message,
            "The operation was canceled by the user (verb \"openas\", SE_ERR 1223)"
        );
        assert_eq!(se_err(0, "open", "p").code, ErrorCode::Io);
        assert_eq!(se_err(-1, "openas", "p").code, ErrorCode::Io);
    }

    #[test]
    fn launch_verbs_use_their_platform_strings() {
        assert_eq!(LaunchVerb::Open.as_str(), "open");
        assert_eq!(LaunchVerb::OpenAs.as_str(), "openas");
        assert_eq!(LaunchVerb::Print.as_str(), "print");
    }

    #[test]
    fn terminal_pref_parse_is_total() {
        assert_eq!(TerminalPref::parse("auto"), TerminalPref::Auto);
        assert_eq!(TerminalPref::parse("wt"), TerminalPref::WindowsTerminal);
        assert_eq!(TerminalPref::parse("cmd"), TerminalPref::Cmd);
        assert_eq!(TerminalPref::parse("powershell"), TerminalPref::PowerShell);
        assert_eq!(TerminalPref::parse("whatever"), TerminalPref::Auto);
    }

    #[test]
    fn missing_terminal_reports_structured_not_found() {
        // A directory that does not exist makes even an existing program
        // fail to start; the reported error must be structured, with the
        // cwd attached, so the UI can prompt (spec: prompt when the
        // executable/config is missing) instead of dying silently.
        let bogus = if cfg!(windows) {
            r"C:\ferrum-does-not-exist-2f1b\dir"
        } else {
            "/ferrum-does-not-exist-2f1b/dir"
        };
        let err = spawn_terminal(TerminalPref::Auto, bogus).expect_err("must fail");
        assert!(matches!(
            err.code,
            ErrorCode::NotFound | ErrorCode::InvalidPath | ErrorCode::Io
        ));
        assert_eq!(err.path.as_deref(), Some(bogus));
    }

    /// FER-467 R5: `open_external` (and Associate With / print / `shell_open`) only take existing
    /// drive or share paths; every other scheme or spelling is refused before ShellExecute.
    #[cfg(windows)]
    #[test]
    fn open_external_refuses_other_schemes_and_targets() {
        for bad in [
            "https://example.com/",
            "http://example.com/x.exe",
            "file:///C:/Windows/System32/calc.exe",
            "ms-msdt:/id PCWDiagnostic",
            "search-ms:query=x&crumb=location:\\\\evil\\share",
            "shell:startup",
            "mailto:a@b.c",
            "calc",
            "calc.exe",
            r"Windows\System32\calc.exe",
            r"\\.\pipe\x",
            r"\\?\GLOBALROOT\Device\Mup\server\share\x",
            r"\\.\GLOBALROOT\Device\Mup\server\share\x",
            r"\\?\Volume{00000000-0000-0000-0000-000000000000}\x",
            r"\\.\UNC\server\share\x",
            r"\\.\C:\Windows\win.ini",
            r"\\?\UNC\server\IPC$\x",
            r"\\?\UNC\server\pipe\x",
            r"\\?\C:\Windows\win.ini:stream",
            r"\??\C:\Windows\win.ini",
            r"\\server\IPC$\x",
            r"C:\Windows\win.ini:stream",
            "",
        ] {
            let e = external_target(bad).expect_err(bad);
            assert_eq!(e.code, ErrorCode::InvalidPath, "{bad}");
            // The launchers refuse it too (never reaching ShellExecute).
            assert_eq!(open_external(bad).unwrap_err().code, ErrorCode::InvalidPath);
            assert_eq!(print_file(bad).unwrap_err().code, ErrorCode::InvalidPath);
            assert_eq!(
                associate_with(bad).unwrap_err().code,
                ErrorCode::InvalidPath
            );
        }
        // Allowed shapes that do not exist are reported as not found, still not launched.
        assert_eq!(
            external_target(r"C:\ferrum-does-not-exist-2f1b\a.txt")
                .unwrap_err()
                .code,
            ErrorCode::NotFound
        );
        // Existing files and folders pass, `/` is normalised.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a b.txt");
        std::fs::write(&file, b"x").unwrap();
        let s = file.to_string_lossy().into_owned();
        assert_eq!(external_target(&s).unwrap(), s);
        assert_eq!(external_target(&s.replace('\\', "/")).unwrap(), s);
        assert!(external_target(&dir.path().to_string_lossy()).is_ok());
        // FER-467: the local long-path spelling is normalised and allowed (any separator mix).
        assert_eq!(external_target(&format!(r"\\?\{s}")).unwrap(), s);
        assert_eq!(
            external_target(&format!(r"\\?\{s}").replace('\\', "/")).unwrap(),
            s
        );
        assert_eq!(
            external_target(r"\\?\C:\ferrum-does-not-exist-2f1b\a.txt")
                .unwrap_err()
                .code,
            ErrorCode::NotFound
        );
        assert_eq!(strip_long_prefix(r"\\?\UNC\srv\share\x"), r"\\srv\share\x");
        assert_eq!(strip_long_prefix(r"\\?\unc\srv\s"), r"\\srv\s");
        assert_eq!(strip_long_prefix(r"\\.\UNC\srv\s"), r"\\.\UNC\srv\s");
    }

    #[test]
    fn test_is_risky_target_table() {
        // Table test for all 47 risky extensions and their uppercase variants
        for ext in RISKY_EXTENSIONS {
            let lower = format!("run.{ext}");
            assert!(
                is_risky_target(&lower).unwrap(),
                "expected {lower} to be risky"
            );
            let upper = format!("run.{}", ext.to_uppercase());
            assert!(
                is_risky_target(&upper).unwrap(),
                "expected {upper} to be risky"
            );
        }

        // Trailing dots and spaces stripped before checking
        assert!(is_risky_target("run.exe.").unwrap());
        assert!(is_risky_target("run.exe ").unwrap());
        assert!(is_risky_target("run.exe...   ").unwrap());
        assert!(is_risky_target("run.bat. . ").unwrap());

        // Multi-dot filenames
        assert!(is_risky_target("notes.txt.exe").unwrap());
        assert!(!is_risky_target("archive.tar.gz").unwrap());
        assert!(!is_risky_target("payload.exe.txt").unwrap());

        // Ordinary documents and images
        for safe in ["doc.pdf", "read.txt", "photo.png", "calc.xlsx", "doc.docx"] {
            assert!(
                !is_risky_target(safe).unwrap(),
                "expected {safe} not to be risky"
            );
        }

        // Files without extension
        assert!(!is_risky_target("README").unwrap());
        assert!(!is_risky_target(r"C:\tools\noext").unwrap());

        // Directories (even if named with an extension)
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_risky_target(dir.path()).unwrap());
        let folder_exe = dir.path().join("folder.exe");
        std::fs::create_dir(&folder_exe).unwrap();
        // FER-513 Rework 1: folders named with risky extensions (like *.exe) are risky
        assert!(is_risky_target(&folder_exe).unwrap());

        // ADS is completely rejected
        for ads in [
            "file.txt:stream",
            r"C:\dir\file.txt:stream",
            r"\\server\share\file.txt:stream",
            r"\\?\C:\dir\file.txt:stream",
        ] {
            let err = is_risky_target(ads).expect_err("ADS must be rejected");
            assert_eq!(err.code, ErrorCode::InvalidPath, "{ads}");
        }

        // Bidi formatting and control characters are rejected
        let bidi = "test\u{202E}exe.txt";
        let err = is_risky_target(bidi).expect_err("bidi must be rejected");
        assert_eq!(err.code, ErrorCode::InvalidPath);

        let ctrl = "test\x07.exe";
        let err = is_risky_target(ctrl).expect_err("control char must be rejected");
        assert_eq!(err.code, ErrorCode::InvalidPath);
    }

    /// The long, final form of a test path. CI runners keep `%TEMP%` under an 8.3 short
    /// name (`RUNNER~1`); the launcher and the dialog use the resolved long path.
    fn long_path(p: &std::path::Path) -> String {
        let full = std::fs::canonicalize(p)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        full.strip_prefix(r"\\?\")
            .map(str::to_owned)
            .unwrap_or(full)
    }

    #[test]
    fn test_shell_open_exe_declined_never_calls_launcher() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("program.exe");
        std::fs::write(&exe, b"MZ").unwrap();
        let exe_str = exe.to_string_lossy().into_owned();
        let exe_long = long_path(&exe);

        let asked = std::cell::Cell::new(false);
        let launched = std::cell::Cell::new(false);

        let res = shell_open_with(
            0,
            &exe_str,
            |_owner, text| {
                asked.set(true);
                assert!(text.contains("Jalankan program ini?"));
                assert!(text.contains("Nama: program.exe"));
                assert!(text.contains("Jenis: .exe"));
                assert!(text.contains(&exe_long));
                Ok(false) // user cancelled
            },
            |_target| {
                launched.set(true);
                Ok(true)
            },
        );

        assert_eq!(res.unwrap_err().code, ErrorCode::Cancelled);
        assert!(asked.get(), "confirmation must be asked");
        assert!(
            !launched.get(),
            "launcher must NOT be called when user cancels"
        );
    }

    #[test]
    fn test_shell_open_exe_accepted_calls_launcher_once() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("program.exe");
        std::fs::write(&exe, b"MZ").unwrap();
        let exe_str = exe.to_string_lossy().into_owned();

        let asked = std::cell::Cell::new(false);
        let launched_path = std::cell::RefCell::new(None);

        let res = shell_open_with(
            0,
            &exe_str,
            |_owner, text| {
                asked.set(true);
                assert!(text.contains("Jalankan program ini?"));
                assert!(text.contains("Nama: program.exe"));
                assert!(text.contains("Jenis: .exe"));
                Ok(true) // user confirmed
            },
            |target| {
                *launched_path.borrow_mut() = Some(target.resolved_path().to_string());
                Ok(true)
            },
        );

        assert!(res.unwrap());
        assert!(asked.get(), "confirmation must be asked");
        assert_eq!(
            launched_path.borrow().as_deref(),
            Some(long_path(std::path::Path::new(&exe_str)).as_str()),
            "launcher must be called with the target path"
        );
    }

    #[test]
    fn test_shell_open_pdf_no_confirm_calls_launcher() {
        let dir = tempfile::tempdir().unwrap();
        let pdf = dir.path().join("document.pdf");
        std::fs::write(&pdf, b"%PDF").unwrap();
        let pdf_str = pdf.to_string_lossy().into_owned();

        let launched_path = std::cell::RefCell::new(None);

        let res = shell_open_with(
            0,
            &pdf_str,
            |_owner, _text| {
                panic!("non-risky file must never trigger confirmation dialog");
            },
            |target| {
                *launched_path.borrow_mut() = Some(target.resolved_path().to_string());
                Ok(true)
            },
        );

        assert!(res.unwrap());
        assert_eq!(
            launched_path.borrow().as_deref(),
            Some(long_path(std::path::Path::new(&pdf_str)).as_str()),
            "launcher must be called directly for safe targets"
        );
    }

    #[test]
    fn test_shell_open_bidi_override_rejected_without_confirm_or_launcher() {
        let bad_path = "C:\\tools\\evil\u{202E}txt.exe";
        let res = shell_open_with(
            0,
            bad_path,
            |_owner, _text| {
                panic!("bidi path must never trigger confirmation dialog");
            },
            |_target| {
                panic!("bidi path must never call launcher");
            },
        );

        assert_eq!(res.unwrap_err().code, ErrorCode::InvalidPath);
    }

    #[test]
    fn test_dialog_guard_busy_when_already_open() {
        // The guard is process-wide: serialize with the other tests that take it.
        let _serial = crate::native_dialog::GUARD_TESTS
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let guard1 = DialogGuard::try_acquire().expect("first acquire should succeed");
        let err = DialogGuard::try_acquire().expect_err("second acquire must fail");
        assert_eq!(err.code, ErrorCode::Busy);
        assert_eq!(err.message, "Konfirmasi lain masih terbuka");

        drop(guard1);
        let guard2 = DialogGuard::try_acquire().expect("subsequent acquire should succeed");
        drop(guard2);
    }

    #[test]
    fn test_print_file_risky_target_rejected_with_unsupported() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("malicious.exe");
        std::fs::write(&exe, b"MZ").unwrap();
        let exe_str = exe.to_string_lossy().into_owned();

        let err = print_file(&exe_str).expect_err("print verb on risky target must fail");
        assert_eq!(err.code, ErrorCode::Unsupported);
    }

    #[test]
    fn test_shell_open_nonexistent_target_with_sibling_bat_fails_parse() {
        let dir = tempfile::tempdir().unwrap();
        let bat = dir.path().join("foo.bat");
        std::fs::write(&bat, b"@echo off\r\n").unwrap();
        let foo_str = dir.path().join("foo").to_string_lossy().into_owned();

        let asked = std::cell::Cell::new(false);
        let launched = std::cell::Cell::new(false);

        let res = shell_open_with(
            0,
            &foo_str,
            |_owner, _text| {
                asked.set(true);
                Ok(true)
            },
            |_target| {
                launched.set(true);
                Ok(true)
            },
        );

        assert!(res.is_err(), "nonexistent target must fail to parse");
        assert!(!asked.get(), "confirm must not be called when parse fails");
        assert!(
            !launched.get(),
            "launcher must not be called when parse fails"
        );
    }

    #[test]
    fn test_shell_open_nonexistent_trailing_dot_with_sibling_bat_fails_parse() {
        let dir = tempfile::tempdir().unwrap();
        let bat = dir.path().join("foo.bat");
        std::fs::write(&bat, b"@echo off\r\n").unwrap();
        let foo_dot = format!("{}.", dir.path().join("foo").to_string_lossy());

        let asked = std::cell::Cell::new(false);
        let launched = std::cell::Cell::new(false);

        let res = shell_open_with(
            0,
            &foo_dot,
            |_owner, _text| {
                asked.set(true);
                Ok(true)
            },
            |_target| {
                launched.set(true);
                Ok(true)
            },
        );

        assert!(res.is_err(), "trailing-dot nonexistent target must fail");
        assert!(!asked.get(), "confirm must not be called");
        assert!(!launched.get(), "launcher must not be called");
    }

    #[test]
    fn test_shell_open_folder_with_sibling_bat_resolves_to_folder() {
        let dir = tempfile::tempdir().unwrap();
        let bar_dir = dir.path().join("bar");
        std::fs::create_dir(&bar_dir).unwrap();
        let bat = dir.path().join("bar.bat");
        std::fs::write(&bat, b"@echo off\r\n").unwrap();
        let bar_str = bar_dir.to_string_lossy().into_owned();

        let asked = std::cell::Cell::new(false);
        let launched_target = std::cell::RefCell::new(None);

        let res = shell_open_with(
            0,
            &bar_str,
            |_owner, _text| {
                asked.set(true);
                Ok(true)
            },
            |target| {
                #[cfg(windows)]
                assert!(!target.pidl().as_ptr().is_null());
                *launched_target.borrow_mut() = Some(target.resolved_path().to_string());
                Ok(true)
            },
        );

        assert!(res.unwrap());
        assert!(!asked.get(), "plain folder must never ask confirmation");
        let launched = launched_target.borrow().clone().unwrap();
        assert!(
            launched.ends_with("bar") && !launched.ends_with("bar.bat"),
            "resolved target must be the folder 'bar', not 'bar.bat': got {launched}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn test_shell_open_short_name_8dot3_resolves_to_long_path_and_is_risky() {
        use windows_sys::Win32::Storage::FileSystem::GetShortPathNameW;

        let dir = tempfile::tempdir().unwrap();
        let long_file = dir.path().join("longname.appref-ms");
        std::fs::write(&long_file, b"test").unwrap();
        let long_str = long_file.to_string_lossy().into_owned();

        let long_wide = to_wide(&long_str);
        let mut short_buf = vec![0u16; 1024];
        let len = unsafe {
            GetShortPathNameW(
                long_wide.as_ptr(),
                short_buf.as_mut_ptr(),
                short_buf.len() as u32,
            )
        };
        if len == 0 || (len as usize) >= short_buf.len() {
            println!("Skipping 8.3 short name test: GetShortPathNameW failed");
            return;
        }
        let short_str = String::from_utf16_lossy(&short_buf[..len as usize]);
        if short_str.eq_ignore_ascii_case(&long_str) {
            println!("Skipping 8.3 short name test: volume does not generate short 8.3 names");
            return;
        }

        let asked = std::cell::Cell::new(false);
        let launched_target = std::cell::RefCell::new(None);

        let res = shell_open_with(
            0,
            &short_str,
            |_owner, text| {
                asked.set(true);
                assert!(
                    text.contains("longname.appref-ms"),
                    "dialog must display the resolved long name: {text}"
                );
                assert!(
                    text.contains("Jenis: .appref-ms"),
                    "dialog must display the resolved extension: {text}"
                );
                Ok(true)
            },
            |target| {
                *launched_target.borrow_mut() = Some(target.resolved_path().to_string());
                Ok(true)
            },
        );

        assert!(res.unwrap());
        assert!(
            asked.get(),
            "8.3 short name resolving to risky target must trigger dialog"
        );
        let launched = launched_target.borrow().clone().unwrap();
        assert!(
            launched.ends_with("longname.appref-ms"),
            "launcher must receive resolved long path: got {launched}"
        );
    }

    #[test]
    fn test_shell_open_folder_named_exe_triggers_dialog() {
        let dir = tempfile::tempdir().unwrap();
        let folder_exe = dir.path().join("x.exe");
        std::fs::create_dir(&folder_exe).unwrap();
        let folder_str = folder_exe.to_string_lossy().into_owned();

        let asked = std::cell::Cell::new(false);
        let launched = std::cell::Cell::new(false);

        // Declined by user
        let res = shell_open_with(
            0,
            &folder_str,
            |_owner, text| {
                asked.set(true);
                assert!(text.contains("Nama: x.exe"));
                assert!(text.contains("Jenis: .exe"));
                Ok(false) // user cancelled
            },
            |_target| {
                launched.set(true);
                Ok(true)
            },
        );

        assert_eq!(res.unwrap_err().code, ErrorCode::Cancelled);
        assert!(
            asked.get(),
            "folder named x.exe must trigger confirmation dialog"
        );
        assert!(
            !launched.get(),
            "launcher must not be called when cancelled"
        );

        // Accepted by user
        let res_ok = shell_open_with(
            0,
            &folder_str,
            |_owner, _text| Ok(true),
            |_target| {
                launched.set(true);
                Ok(true)
            },
        );

        assert!(res_ok.unwrap());
        assert!(launched.get(), "launcher must be called when confirmed");
    }

    #[test]
    fn test_confirm_prompt_text_format() {
        let sample = r"C:\path\to\malicious.exe";
        let prompt = confirm_prompt_text(sample, "malicious.exe");
        assert!(prompt.contains("Jalankan program ini?"));
        assert!(prompt.contains("Nama: malicious.exe"));
        assert!(prompt.contains("Jenis: .exe"));
        assert!(prompt.contains(r"Path: C:\path\to\malicious.exe"));
        assert!(prompt.contains("Program dapat mengubah sistem atau file Anda. Jalankan hanya jika Anda memercayai file ini."));
    }

    #[test]
    fn test_shell_open_existing_target_without_ext_with_sibling_bat_resolves_to_target() {
        let dir = tempfile::tempdir().unwrap();
        let foo = dir.path().join("foo");
        std::fs::write(&foo, b"binary or text content without extension").unwrap();
        let bat = dir.path().join("foo.bat");
        std::fs::write(&bat, b"@echo off\r\n").unwrap();
        let foo_str = foo.to_string_lossy().into_owned();

        let asked = std::cell::Cell::new(false);
        let launched_target = std::cell::RefCell::new(None);

        let res = shell_open_with(
            0,
            &foo_str,
            |_owner, _text| {
                asked.set(true);
                Ok(true)
            },
            |target| {
                #[cfg(windows)]
                assert!(!target.pidl().as_ptr().is_null());
                *launched_target.borrow_mut() = Some(target.resolved_path().to_string());
                Ok(true)
            },
        );

        assert!(res.unwrap());
        assert!(
            !asked.get(),
            "file without extension must not ask confirmation"
        );
        let launched = launched_target.borrow().clone().unwrap();
        assert!(
            launched.ends_with("foo") && !launched.ends_with("foo.bat"),
            "resolved target must be 'foo', not sibling 'foo.bat': got {launched}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn test_shell_open_long_path_over_260_chars_is_risky_and_shows_long_name() {
        ensure_com_initialized();
        let dir = tempfile::tempdir().unwrap();
        let sub = dir
            .path()
            .join("a".repeat(70))
            .join("b".repeat(70))
            .join("c".repeat(70));
        if std::fs::create_dir_all(&sub).is_err() {
            println!(
                "Skipping long path test: OS refused to create directory hierarchy >260 chars"
            );
            return;
        }
        let file = sub.join("x.appref-ms");
        if std::fs::write(&file, b"test content").is_err() {
            println!("Skipping long path test: OS refused to write file >260 chars");
            return;
        }
        let file_str = file.to_string_lossy().into_owned();
        if file_str.len() <= 260 {
            println!(
                "Skipping long path test: generated path length {} is not > 260",
                file_str.len()
            );
            return;
        }

        let target = match resolve_target(&file_str) {
            Ok(t) => t,
            Err(e) => {
                println!("Skipping long path test: resolve_target failed on long path: {e:?}");
                return;
            }
        };

        assert!(
            is_risky_launch_target(&target).expect("classification should succeed"),
            "target >260 chars with .appref-ms extension must be classified as risky"
        );

        let asked = std::cell::Cell::new(false);
        let prompt_captured = std::cell::RefCell::new(String::new());

        let res = shell_open_with(
            0,
            &file_str,
            |_owner, text| {
                asked.set(true);
                *prompt_captured.borrow_mut() = text.to_string();
                Ok(true)
            },
            |_target| Ok(true),
        );

        assert!(res.unwrap());
        assert!(
            asked.get(),
            "dialog must be shown for risky target in long path"
        );
        let prompt = prompt_captured.borrow().clone();
        assert!(
            prompt.contains("Nama: x.appref-ms"),
            "dialog must display long item name 'x.appref-ms', got:\n{prompt}"
        );
        assert!(
            prompt.contains("Jenis: .appref-ms"),
            "dialog must display extension '.appref-ms', got:\n{prompt}"
        );
    }

    #[test]
    fn test_is_risky_launch_target_fail_closed_on_divergent_names() {
        let target = LaunchTarget {
            #[cfg(windows)]
            pidl: unsafe { Pidl::from_raw(std::ptr::null_mut()) },
            resolved_path: r"C:\path\to\DOCUME~1.XYZ".to_string(),
            item_name: "document.xyz".to_string(),
        };
        let risky = is_risky_launch_target(&target).unwrap();
        assert!(
            risky,
            "divergence between 8.3 path and item name must fail-closed as risky"
        );
    }

    #[cfg(windows)]
    #[test]
    fn test_get_item_name_from_pidl_behavior() {
        ensure_com_initialized();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("my-test-file.txt");
        std::fs::write(&file, b"hello").unwrap();
        let pidl = parse_pidl(&file.to_string_lossy()).unwrap();
        let name = get_item_name_from_pidl(&pidl).unwrap();
        assert_eq!(name, "my-test-file.txt");

        let root_pidl = parse_pidl(r"C:\").unwrap();
        let root_name = get_item_name_from_pidl(&root_pidl).unwrap();
        assert_eq!(root_name, "C:");
    }
}
