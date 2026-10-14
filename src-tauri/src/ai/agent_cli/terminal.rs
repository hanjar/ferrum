//! FER-494 "Buka di terminal": open an agent CLI INTERACTIVELY in a visible terminal, in a folder
//! the user picked. From then on the user leads, with the agent's own permission prompts and the
//! user's own agent configuration. Ferrum only delivers; it never runs an agent without a visible
//! terminal here.
//!
//! Security model (design decision, FER-494):
//! - The executable is the DETECTED one ([`super::detect`]), re-checked before every start
//!   (`launch_ok` + ACL, [`super::acl`]). Never a `.cmd`/`.bat`/`.ps1`, never through a shell.
//! - NO text from the user or from a file is ever on the command line. The command line holds only
//!   fixed strings and the detected program/script paths ([`AgentDef::terminal_args`] are
//!   constants). A question typed in the composer is copied to the clipboard by the webview.
//! - The folder travels ONLY as the process working directory (`lpCurrentDirectory`):
//!   - Windows Terminal is started as `wt -d . <program> [script]` with the folder as its cwd.
//!     `wt` splits its command line on `;` (verified on the owner's host: `wt -d "C:\a;b"` breaks
//!     the command), so the folder is never given to it as an argument. `.` makes wt use its own
//!     cwd (verified with a folder named `a;b x %PATH% ^&c é 漢`).
//!   - Fallback: the agent itself in a new console window (`CREATE_NEW_CONSOLE`), cwd = folder.
//!     `cmd.exe` is never involved, so `%`, `^`, `&` and quotes have no meaning (no BatBadBut).
//!   - A program/script path that `wt` would split (`;`) or read as an option skips the wt route.
//!   - T2: `wt.exe` is resolved to an absolute path and ACL-checked (`shell_open::resolve_wt`);
//!     an untrusted or missing wt means the console route.
//! - T1/T3: project configuration the agent would load (per agent, [`AgentDef::project_configs`]),
//!   a UNC or linked folder, or a folder owned by another account produce `warnings`.
//!   FER-516: an agent without its own folder-trust dialog ALWAYS needs a NATIVE confirmation
//!   raised by the backend (the scan can be outrun by a file planted afterwards); the findings are
//!   shown in it as information. Claude Code and Codex (own trust prompt) get the native
//!   confirmation only when the scan finds project configuration, otherwise a note. The webview
//!   cannot answer the confirmation and its request carries no flag for it.
//! - The folder is validated with `path_guard::check_str` (no device namespace, no `\\?\`, no odd
//!   UNC host, no ADS) plus: no control characters, no `"`/wildcards, no `.`/`..` segments, must be
//!   an existing directory (a file means its parent folder). Plain UNC shares are allowed.
//! - No flag that loosens permissions (`--dangerously-skip-permissions`, `bypassPermissions`,
//!   `--yolo`, `--approval-mode yolo`, …) is ever added; [`FORBIDDEN_FLAGS`] is checked in tests.
//! - Test profile (`FERRUM_PROFILE_DIR`): nothing is started. The command that WOULD run is
//!   returned (`dryRun`) and appended to `<data>\agent-terminal-dry-run.jsonl` for the smoke.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::AgentId;
use super::args::Launch;
use super::commands::AgentError;
use super::detect::{self, launch_ok};
use super::settings::AgentSettings;
use crate::domain::{ErrorCode, FsError};
use crate::shell_open::TerminalPref;

/// One agent Ferrum knows: label, where its executable lives, and how it opens interactively.
pub struct AgentDef {
    pub id: AgentId,
    /// Label shown in Settings, the palette and menus.
    pub name: &'static str,
    /// Service that receives what the agent reads.
    pub vendor: &'static str,
    /// Native executable searched in every search folder.
    pub exe: &'static str,
    /// npm shim (`.cmd`) next to which the package's own binary/script is looked up. The shim
    /// itself is never run.
    pub npm_shim: Option<&'static str>,
    /// Native binaries inside the npm package, relative to the folder holding the shim.
    pub npm_native: fn(&Path) -> Vec<PathBuf>,
    /// Script entry run as `node.exe <script>` when the package has no native binary.
    pub npm_script: fn(&Path) -> Option<PathBuf>,
    /// Interactive arguments. Fixed constants only (no user text, no folder), and never a flag
    /// that loosens the agent's permissions: the user's own configuration applies as is.
    pub terminal_args: &'static [&'static str],
    /// Short note shown when the agent is opened (e.g. Codex reads beyond the folder).
    pub terminal_note: Option<&'static str>,
    /// FER-494 T1: project files the agent loads from the folder (or a parent up to the git
    /// root) that can run commands or redirect its API.
    pub project_configs: &'static [ProjectConfig],
    /// The agent asks the user itself before trusting a folder's project configuration (Claude
    /// Code, Codex): a warning is shown but does not block. Otherwise a confirmation is required.
    pub own_trust_prompt: bool,
}

/// Where a project configuration file lives, relative to a folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigPattern {
    /// A file (or anything) at this relative path, e.g. `.gemini\settings.json`.
    Path(&'static str),
    /// Files `<dir>\<prefix>*<suffix>`, e.g. `.claude\settings*.json`.
    Prefixed {
        dir: &'static str,
        prefix: &'static str,
        suffix: &'static str,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProjectConfig {
    pub pattern: ConfigPattern,
    /// What the file can do (shown in the confirmation).
    pub risk: &'static str,
}

const CLAUDE_CONFIGS: &[ProjectConfig] = &[
    ProjectConfig {
        pattern: ConfigPattern::Prefixed {
            dir: ".claude",
            prefix: "settings",
            suffix: ".json",
        },
        risk: "hooks di dalamnya bisa menjalankan perintah",
    },
    ProjectConfig {
        pattern: ConfigPattern::Path(".mcp.json"),
        risk: "server MCP proyek dijalankan sebagai perintah",
    },
];
const CODEX_CONFIGS: &[ProjectConfig] = &[ProjectConfig {
    pattern: ConfigPattern::Path(".codex"),
    risk: "konfigurasi Codex tingkat proyek (MCP, perintah, alamat API)",
}];
const OPENCODE_CONFIGS: &[ProjectConfig] = &[
    ProjectConfig {
        pattern: ConfigPattern::Path("opencode.json"),
        risk: "server MCP lokal dijalankan sebagai perintah saat start, atau alamat API dialihkan",
    },
    ProjectConfig {
        pattern: ConfigPattern::Path("opencode.jsonc"),
        risk: "server MCP lokal dijalankan sebagai perintah saat start, atau alamat API dialihkan",
    },
    ProjectConfig {
        pattern: ConfigPattern::Path(".opencode"),
        risk: "plugin, perintah, dan agent proyek dimuat otomatis",
    },
];
const GEMINI_CONFIGS: &[ProjectConfig] = &[ProjectConfig {
    pattern: ConfigPattern::Path(".gemini\\settings.json"),
    risk: "server MCP dijalankan sebagai perintah saat start",
}];
const AIDER_CONFIGS: &[ProjectConfig] = &[
    ProjectConfig {
        pattern: ConfigPattern::Path(".aider.conf.yml"),
        risk: "bisa menyalakan yes-always / auto-test dan menjalankan perintah otomatis",
    },
    ProjectConfig {
        pattern: ConfigPattern::Path(".env"),
        risk: "bisa mengalihkan API (mis. OPENAI_API_BASE) ke server lain",
    },
];

fn none_native(_: &Path) -> Vec<PathBuf> {
    Vec::new()
}

fn none_script(_: &Path) -> Option<PathBuf> {
    None
}

fn join(base: &Path, segs: &[&str]) -> PathBuf {
    segs.iter().fold(base.to_path_buf(), |p, s| p.join(s))
}

fn claude_native(npm: &Path) -> Vec<PathBuf> {
    vec![join(
        npm,
        &[
            "node_modules",
            "@anthropic-ai",
            "claude-code",
            "bin",
            "claude.exe",
        ],
    )]
}

fn claude_script(npm: &Path) -> Option<PathBuf> {
    Some(join(
        npm,
        &["node_modules", "@anthropic-ai", "claude-code", "cli.js"],
    ))
}

fn codex_native(npm: &Path) -> Vec<PathBuf> {
    let (pkg, triple) = if cfg!(target_arch = "aarch64") {
        ("codex-win32-arm64", "aarch64-pc-windows-msvc")
    } else {
        ("codex-win32-x64", "x86_64-pc-windows-msvc")
    };
    let tail = |base: PathBuf| {
        join(
            &base,
            &["@openai", pkg, "vendor", triple, "bin", "codex.exe"],
        )
    };
    let nm = npm.join("node_modules");
    vec![
        tail(join(&nm, &["@openai", "codex", "node_modules"])),
        tail(nm.clone()),
    ]
}

fn codex_script(npm: &Path) -> Option<PathBuf> {
    Some(join(
        npm,
        &["node_modules", "@openai", "codex", "bin", "codex.js"],
    ))
}

fn opencode_native(npm: &Path) -> Vec<PathBuf> {
    let pkg = if cfg!(target_arch = "aarch64") {
        "opencode-windows-arm64"
    } else {
        "opencode-windows-x64"
    };
    let nm = npm.join("node_modules");
    vec![
        join(
            &nm,
            &["opencode-ai", "node_modules", pkg, "bin", "opencode.exe"],
        ),
        join(&nm, &[pkg, "bin", "opencode.exe"]),
    ]
}

fn gemini_script(npm: &Path) -> Option<PathBuf> {
    Some(join(
        npm,
        &["node_modules", "@google", "gemini-cli", "dist", "index.js"],
    ))
}

/// The single agent definition list (FER-494). Order = display order.
pub const AGENT_DEFS: [AgentDef; 7] = [
    AgentDef {
        id: AgentId::Claude,
        name: "Claude Code",
        vendor: "Anthropic",
        exe: "claude.exe",
        npm_shim: Some("claude.cmd"),
        npm_native: claude_native,
        npm_script: claude_script,
        terminal_args: &[],
        terminal_note: None,
        project_configs: CLAUDE_CONFIGS,
        own_trust_prompt: true,
    },
    AgentDef {
        id: AgentId::Codex,
        name: "Codex",
        vendor: "OpenAI",
        exe: "codex.exe",
        npm_shim: Some("codex.cmd"),
        npm_native: codex_native,
        npm_script: codex_script,
        terminal_args: &[],
        terminal_note: Some(
            "Codex dapat membaca file lain di PC ini, tidak hanya folder ini (persetujuan aksi tetap lewat Codex).",
        ),
        project_configs: CODEX_CONFIGS,
        own_trust_prompt: true,
    },
    AgentDef {
        id: AgentId::Omp,
        name: "omp",
        vendor: "provider model yang dipilih di omp",
        exe: "omp.exe",
        npm_shim: None,
        npm_native: none_native,
        npm_script: none_script,
        terminal_args: &[],
        terminal_note: None,
        project_configs: &[],
        own_trust_prompt: false,
    },
    AgentDef {
        // `agy` is the Antigravity agent CLI (interactive session by default, tool permissions
        // asked unless the user passes --dangerously-skip-permissions, which Ferrum never does).
        // The Antigravity IDE launcher is a `.cmd` and is not used.
        id: AgentId::Antigravity,
        name: "Antigravity",
        vendor: "Google",
        exe: "agy.exe",
        npm_shim: None,
        npm_native: none_native,
        npm_script: none_script,
        terminal_args: &[],
        terminal_note: None,
        project_configs: &[],
        own_trust_prompt: false,
    },
    AgentDef {
        id: AgentId::Opencode,
        name: "opencode",
        vendor: "provider model yang dipilih di opencode",
        exe: "opencode.exe",
        npm_shim: Some("opencode.cmd"),
        npm_native: opencode_native,
        npm_script: none_script,
        terminal_args: &[],
        terminal_note: None,
        project_configs: OPENCODE_CONFIGS,
        own_trust_prompt: false,
    },
    AgentDef {
        id: AgentId::Gemini,
        name: "Gemini CLI",
        vendor: "Google",
        exe: "gemini.exe",
        npm_shim: Some("gemini.cmd"),
        npm_native: none_native,
        npm_script: gemini_script,
        terminal_args: &[],
        terminal_note: None,
        project_configs: GEMINI_CONFIGS,
        own_trust_prompt: false,
    },
    AgentDef {
        id: AgentId::Aider,
        name: "Aider",
        vendor: "provider model yang dipilih di aider",
        exe: "aider.exe",
        npm_shim: None,
        npm_native: none_native,
        npm_script: none_script,
        terminal_args: &[],
        terminal_note: Some(
            "Aider dapat langsung mengubah dan meng-commit file sesuai konfigurasi aider Anda.",
        ),
        project_configs: AIDER_CONFIGS,
        own_trust_prompt: false,
    },
];

/// Flags that loosen an agent's permissions. Never placed on a command line by Ferrum.
pub const FORBIDDEN_FLAGS: &[&str] = &[
    "--dangerously-skip-permissions",
    "--dangerously-bypass-approvals-and-sandbox",
    "bypassPermissions",
    "--permission-mode",
    "--yolo",
    "--approval-mode",
    "--auto-approve",
    "--full-auto",
    "--yes-always",
    "--allowedTools",
];

// ---------------------------------------------------------------------------------------------
// Folder check
// ---------------------------------------------------------------------------------------------

/// Why a folder is refused (shown to the user).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FolderError {
    /// Not a plain local/UNC path (device namespace, odd characters, `..`, relative…).
    Invalid,
    /// Does not exist (or is neither a file nor a folder).
    Missing,
}

/// No `.`/`..` segments and no segment ending in a dot or space (Windows would rewrite them).
fn plain_segments(path: &str) -> bool {
    path.split('\\')
        .skip(1)
        .filter(|s| !s.is_empty())
        .all(|seg| seg != "." && seg != ".." && !seg.ends_with('.') && !seg.ends_with(' '))
}

/// String-level check of the folder (or file) the user picked; returns the normalised path.
pub fn check_path_str(raw: &str) -> Result<String, FolderError> {
    if raw.is_empty()
        || raw
            .chars()
            .any(|c| c.is_control() || matches!(c, '"' | '*' | '?' | '<' | '>' | '|'))
    {
        return Err(FolderError::Invalid);
    }
    let path = crate::path_guard::check_str(raw).map_err(|_| FolderError::Invalid)?;
    // Doubled separators after the drive / UNC prefix.
    if path.get(2..).is_some_and(|rest| rest.contains(r"\\")) {
        return Err(FolderError::Invalid);
    }
    let path = if path.len() > 3 {
        path.trim_end_matches('\\').to_owned()
    } else {
        path
    };
    if !plain_segments(&path) {
        return Err(FolderError::Invalid);
    }
    Ok(path)
}

/// The working folder for `raw`: the folder itself, or the parent folder of a file.
pub fn working_folder(raw: &str) -> Result<PathBuf, FolderError> {
    let path = PathBuf::from(check_path_str(raw)?);
    if path.is_dir() {
        return Ok(path);
    }
    if path.is_file() {
        return path
            .parent()
            .filter(|p| p.is_dir())
            .map(Path::to_path_buf)
            .ok_or(FolderError::Missing);
    }
    Err(FolderError::Missing)
}

// ---------------------------------------------------------------------------------------------
// Untrusted folder warnings (FER-494 T1/T3)
// ---------------------------------------------------------------------------------------------

/// Folders scanned upwards for project configuration (the folder itself included).
pub const CONFIG_SCAN_LEVELS: usize = 8;

/// `symlink_metadata` only: nothing is followed. A reparse point (symlink, junction) counts as
/// present.
fn present(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok()
}

fn is_reparse(p: &Path) -> bool {
    match std::fs::symlink_metadata(p) {
        Ok(m) => {
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
                m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
            }
            #[cfg(not(windows))]
            {
                m.file_type().is_symlink()
            }
        }
        Err(_) => false,
    }
}

/// Project configuration files of `def` in one folder.
fn configs_in(dir: &Path, def: &AgentDef) -> Vec<(PathBuf, &'static str)> {
    let mut out = Vec::new();
    for cfg in def.project_configs {
        match cfg.pattern {
            ConfigPattern::Path(rel) => {
                let p = dir.join(rel);
                if present(&p) {
                    out.push((p, cfg.risk));
                }
            }
            ConfigPattern::Prefixed {
                dir: sub,
                prefix,
                suffix,
            } => {
                let d = dir.join(sub);
                if is_reparse(&d) {
                    // Not listed through a link; its presence alone is reported.
                    out.push((d, cfg.risk));
                    continue;
                }
                let Ok(entries) = std::fs::read_dir(&d) else {
                    continue;
                };
                let mut names: Vec<String> = entries
                    .flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|n| {
                        let l = n.to_ascii_lowercase();
                        l.starts_with(prefix) && l.ends_with(suffix)
                    })
                    .collect();
                names.sort();
                out.extend(names.into_iter().map(|n| (d.join(n), cfg.risk)));
            }
        }
    }
    out
}

/// Warnings for opening `agent` in `folder` (empty = nothing found): project configuration in
/// the folder or a parent (up to the git root, at most [`CONFIG_SCAN_LEVELS`] folders, the volume
/// root at the latest), a network (UNC) folder, a folder that is a link, a folder owned by
/// another account (files and DLLs there can be planted, T3).
pub fn assess(
    agent: AgentId,
    folder: &Path,
    folder_owner: &dyn Fn(&Path) -> Result<(), String>,
) -> Vec<String> {
    let def = agent.def();
    let mut warnings = Vec::new();
    let mut dir = Some(folder);
    for _ in 0..CONFIG_SCAN_LEVELS {
        let Some(d) = dir else { break };
        for (path, risk) in configs_in(d, def) {
            warnings.push(format!(
                "{} akan membaca {}: {risk}.",
                def.name,
                path.display()
            ));
        }
        if present(&d.join(".git")) {
            break;
        }
        dir = d.parent();
    }
    if folder.to_string_lossy().starts_with(r"\\") {
        warnings.push(
            "Folder ada di jaringan (UNC): file konfigurasi dan DLL di sana bisa diubah orang lain."
                .to_owned(),
        );
    }
    if is_reparse(folder) {
        warnings.push("Folder ini adalah tautan/junction ke lokasi lain.".to_owned());
    }
    if let Err(reason) = folder_owner(folder) {
        warnings.push(format!(
            "{reason}: file konfigurasi dan DLL di folder ini bisa ditanam akun itu."
        ));
    }
    warnings
}

// ---------------------------------------------------------------------------------------------
// Command composition
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Via {
    /// `wt -d . <agent…>` with cwd = folder.
    WindowsTerminal,
    /// The agent itself in a new console window, cwd = folder.
    Console,
}

/// One structured launch: program + argv + working directory. Never a command string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalCommand {
    pub via: Via,
    pub program: OsString,
    pub args: Vec<OsString>,
    pub cwd: PathBuf,
}

/// The agent's own argv (after the program): the script entry if any, then the fixed
/// interactive arguments of its definition. Built from the detected launch only.
pub fn agent_argv(agent: AgentId, launch: &Launch) -> Vec<OsString> {
    let mut args: Vec<OsString> = Vec::new();
    if let Some(script) = &launch.script {
        args.push(script.clone().into_os_string());
    }
    args.extend(agent.def().terminal_args.iter().map(OsString::from));
    args
}

/// `wt` splits its command line on `;` and reads a leading `-` as its own option; such an
/// argument must not be given to it.
pub fn wt_safe(arg: &OsStr) -> bool {
    let s = arg.to_string_lossy();
    !s.is_empty() && !s.contains(';') && !s.starts_with('-') && !s.chars().any(char::is_control)
}

/// Ordered launch candidates. `Auto`/`wt` try Windows Terminal first, then the console; `cmd`
/// and `powershell` preferences use the console route (the agent is never started through a
/// shell). The folder is never an argument.
pub fn candidates(
    pref: TerminalPref,
    agent: AgentId,
    launch: &Launch,
    folder: &Path,
    wt: Option<&Path>,
) -> Vec<TerminalCommand> {
    let agent_args = agent_argv(agent, launch);
    let mut out = Vec::new();
    let wt_ok = std::iter::once(launch.program.as_os_str())
        .chain(agent_args.iter().map(OsString::as_os_str))
        .all(wt_safe);
    if let Some(wt) = wt.filter(|_| wt_ok)
        && matches!(pref, TerminalPref::Auto | TerminalPref::WindowsTerminal)
    {
        let mut args: Vec<OsString> = vec!["-d".into(), ".".into()];
        args.push(launch.program.clone().into_os_string());
        args.extend(agent_args.iter().cloned());
        out.push(TerminalCommand {
            via: Via::WindowsTerminal,
            program: wt.as_os_str().to_owned(),
            args,
            cwd: folder.to_path_buf(),
        });
    }
    out.push(TerminalCommand {
        via: Via::Console,
        program: launch.program.clone().into_os_string(),
        args: agent_args,
        cwd: folder.to_path_buf(),
    });
    out
}

impl TerminalCommand {
    /// Full argv (program first) for display, logs and the dry run.
    pub fn argv(&self) -> Vec<String> {
        std::iter::once(&self.program)
            .chain(self.args.iter())
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    /// The `std::process::Command` for this launch: structured argv, cwd, new console window,
    /// the user's environment without Ferrum's own `FERRUM_*` variables.
    pub fn command(&self) -> std::process::Command {
        let mut c = std::process::Command::new(&self.program);
        c.args(&self.args).current_dir(&self.cwd);
        for (k, _) in std::env::vars_os() {
            if k.to_string_lossy()
                .to_ascii_uppercase()
                .starts_with("FERRUM_")
            {
                c.env_remove(k);
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
            c.creation_flags(CREATE_NEW_CONSOLE);
        }
        c
    }
}

/// Start the first candidate that starts. Returns the one used.
pub fn spawn_first(cands: Vec<TerminalCommand>) -> std::io::Result<TerminalCommand> {
    let mut last: Option<std::io::Error> = None;
    for cand in cands {
        match cand.command().spawn() {
            Ok(_child) => return Ok(cand),
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("no terminal candidate")))
}

// ---------------------------------------------------------------------------------------------
// Request flow
// ---------------------------------------------------------------------------------------------

/// What the webview may send: an agent id, a path and the terminal preference. There is no field
/// for a question or any other text (`deny_unknown_fields`): it cannot reach the command line.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TerminalOpenRequest {
    pub agent: String,
    pub path: String,
    #[serde(default)]
    pub program: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalOpened {
    pub agent: AgentId,
    /// Always set on success (nothing is returned when nothing started).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub via: Option<Via>,
    /// The argv that was started (or, in the test profile, would have been).
    pub argv: Vec<String>,
    pub cwd: String,
    /// Test profile: nothing was started.
    pub dry_run: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<&'static str>,
    /// FER-494 T1: project configuration, UNC or foreign-owned folder (one line each).
    pub warnings: Vec<String>,
}

/// Everything [`open`] needs besides the request (injected by tests).
pub struct OpenContext<'a> {
    pub settings: &'a AgentSettings,
    /// Test profile config dir: only the fake agent named there may be used, and nothing starts.
    pub test_profile_config: Option<PathBuf>,
    /// Test profile: JSON lines of the commands that would have started.
    pub dry_run_log: Option<PathBuf>,
    pub trusted: &'a dyn Fn(&Launch) -> Result<(), String>,
    /// FER-494 T1/T3: the folder's owner is this user / SYSTEM / Administrators /
    /// TrustedInstaller (`Err(reason)` = warning).
    pub folder_owner: &'a dyn Fn(&Path) -> Result<(), String>,
    /// FER-494 T2: absolute, ACL-checked `wt.exe` (`None` = console route only).
    pub wt: Option<PathBuf>,
    pub spawn: &'a dyn Fn(Vec<TerminalCommand>) -> std::io::Result<TerminalCommand>,
    /// FER-516: the native confirmation for an untrusted folder, called with (caption, text).
    /// Production: `native_dialog::confirm_or_busy` (one dialog at a time app-wide; `Err(Busy)`
    /// shows nothing). Never answered by the webview.
    pub confirm: &'a dyn Fn(&str, &str) -> Result<bool, FsError>,
}

pub const CONFIRM_CAPTION: &str = "Konfirmasi membuka agent";

/// Longest folder path / finding shown in the dialog; longer text is cut in the middle for
/// display only (the full path is still the one used as the working directory).
const SHOWN_PATH_MAX: usize = 180;
const SHOWN_FINDING_MAX: usize = 240;

/// `s` cut in the middle with an ellipsis when longer than `max` characters.
fn shorten(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_owned();
    }
    let head = (max - 1) / 2;
    let tail = max - 1 - head;
    let start: String = s.chars().take(head).collect();
    let end: String = s.chars().skip(n - tail).collect();
    format!("{start}\u{2026}{end}")
}

/// The native dialog text, built by the backend only: agent, the folder path (cut in the middle
/// when very long) and a short warning, plus up to five scan findings as information. `None` when
/// any character could reorder or hide text (control or format characters, e.g. U+202E) in the
/// folder or in a finding: then no dialog is shown at all and nothing starts.
pub fn confirm_text(agent_name: &str, folder: &Path, warnings: &[String]) -> Option<String> {
    let path = folder.to_string_lossy();
    let findings: Vec<&String> = warnings.iter().take(5).collect();
    let bad = |c: char| c.is_control() || crate::hotlist::model::is_format_char(c);
    if path.chars().any(bad) || findings.iter().any(|w| w.chars().any(bad)) {
        return None;
    }
    let mut text = format!(
        "Buka {agent_name} di folder ini?\n\n{}\n\n",
        shorten(&path, SHOWN_PATH_MAX)
    );
    text.push_str(
        "Konfigurasi proyek di folder ini (mis. server MCP atau hook) bisa menjalankan program \
         di komputer ini saat agent dimulai, dan folder bisa berubah sesudah pemeriksaan. \
         Lanjutkan hanya jika Anda memercayai folder dan isinya.",
    );
    if !findings.is_empty() {
        text.push_str("\n\nTemuan pemeriksaan:");
    }
    for w in findings {
        text.push_str("\n- ");
        text.push_str(&shorten(w, SHOWN_FINDING_MAX));
    }
    Some(text)
}

pub const DRY_RUN_LOG: &str = "agent-terminal-dry-run.jsonl";

fn append_line(path: &Path, line: &str) {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{line}");
    }
}

/// Validate, re-check the agent, compose and start (or, in the test profile, only record).
pub fn open(
    ctx: &OpenContext<'_>,
    req: &TerminalOpenRequest,
) -> Result<TerminalOpened, AgentError> {
    let agent = AgentId::parse(&req.agent).ok_or_else(|| AgentError::new("agent_unavailable"))?;
    let entry = ctx
        .settings
        .agent(agent)
        .ok_or_else(|| AgentError::new("agent_unavailable"))?;
    let launch = match (&entry.launch, entry.terminal_ready) {
        (Some(l), true) => l,
        _ => {
            return Err(AgentError::with_message(
                "agent_unavailable",
                entry.terminal_reason.clone().unwrap_or_default(),
            ));
        }
    };
    let is_file = |p: &Path| p.is_file();
    match &ctx.test_profile_config {
        Some(config) => {
            if detect::fake_launch(config, agent).as_ref() != Some(launch)
                || !launch_ok(launch, true, &is_file)
            {
                return Err(AgentError::new("agent_unavailable"));
            }
        }
        None => {
            if !launch_ok(launch, false, &is_file) {
                return Err(AgentError::new("agent_unavailable"));
            }
            // Re-checked before every start (the folder's ACL can change).
            (ctx.trusted)(launch)
                .map_err(|reason| AgentError::with_message("agent_untrusted", reason))?;
        }
    }
    let folder = working_folder(&req.path).map_err(|e| {
        AgentError::at(
            match e {
                FolderError::Invalid => "folder_invalid",
                FolderError::Missing => "folder_missing",
            },
            &req.path,
        )
    })?;
    let warnings = assess(agent, &folder, ctx.folder_owner);
    let note = agent.def().terminal_note;
    // FER-516 (rework 1): an agent WITHOUT its own folder-trust prompt (the explicit allowlist is
    // `AgentDef::own_trust_prompt`) always needs the native confirmation, whatever the scan found:
    // the folder can change between the scan and the moment the agent reads its configuration.
    // Claude Code / Codex (own trust prompt) are asked only when the scan finds project
    // configuration; a plant after the scan is left to their own prompt.
    if !agent.def().own_trust_prompt || !warnings.is_empty() {
        // The only authorization is a native dialog the backend raises itself; nothing the
        // webview sends can stand in for it. A refusal, a busy guard or a text that cannot be
        // shown safely all mean nothing starts.
        let text = confirm_text(agent.def().name, &folder, &warnings)
            .ok_or_else(|| AgentError::at("folder_invalid", &req.path))?;
        match (ctx.confirm)(CONFIRM_CAPTION, &text) {
            Ok(true) => {}
            Err(e) if e.code == ErrorCode::Busy => return Err(AgentError::new("busy")),
            Ok(false) | Err(_) => return Err(AgentError::new("cancelled")),
        }
        // The user may have taken a while: the executable's location is checked once more.
        if ctx.test_profile_config.is_none() {
            (ctx.trusted)(launch)
                .map_err(|reason| AgentError::with_message("agent_untrusted", reason))?;
        }
    }
    let pref = TerminalPref::parse(req.program.as_deref().unwrap_or("auto"));
    let cands = candidates(pref, agent, launch, &folder, ctx.wt.as_deref());
    let done = |cmd: &TerminalCommand, dry_run: bool| TerminalOpened {
        agent,
        via: Some(cmd.via),
        argv: cmd.argv(),
        cwd: cmd.cwd.to_string_lossy().into_owned(),
        dry_run,
        note,
        warnings: warnings.clone(),
    };
    if ctx.test_profile_config.is_some() {
        let first = cands
            .first()
            .ok_or_else(|| AgentError::new("terminal_start_failed"))?;
        let out = done(first, true);
        if let Some(log) = &ctx.dry_run_log
            && let Ok(line) = serde_json::to_string(&out)
        {
            append_line(log, &line);
        }
        return Ok(out);
    }
    let used = (ctx.spawn)(cands).map_err(|_| AgentError::new("terminal_start_failed"))?;
    Ok(done(&used, false))
}
