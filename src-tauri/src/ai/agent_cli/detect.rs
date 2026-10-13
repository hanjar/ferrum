//! Finding the agent executables once ("Deteksi ulang" repeats it). The result, with the full
//! path, is stored in the agent settings and shown to the user; runs use the stored path.
//!
//! Only native `.exe` files are ever started. npm installs put `claude.cmd` / `codex.cmd` shims on
//! PATH; those are never run (BatBadBut, CVE-2024-24576, FER-458). Instead the package's own
//! binary is used: Claude Code ships `bin\claude.exe`, Codex ships a vendored `codex.exe`. Only if
//! a package has no native binary is it started as `node.exe <script>` (no shell either).

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::args::{Launch, clean_env};
use super::runner::{RunSpec, run};
use super::{ALL_AGENTS, AgentId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AgentStatus {
    Supported,
    Unsupported,
    NotFound,
}

/// One agent as stored in the settings and shown in Settings → AI → Agent CLI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DetectedAgent {
    pub id: AgentId,
    pub status: AgentStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch: Option<Launch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Why the agent is not supported / not found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// FER-494: may be opened interactively in a terminal (found, valid exe, ACL passed).
    #[serde(default)]
    pub terminal_ready: bool,
    /// FER-494: why "Buka di terminal" is not available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_reason: Option<String>,
}

/// Folders searched: absolute PATH entries (relative ones, e.g. ".", are skipped) and the usual
/// per-user install folders.
pub fn search_dirs(env: &dyn Fn(&str) -> Option<OsString>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut add = |p: PathBuf| {
        if p.is_absolute()
            && !dirs
                .iter()
                .any(|d| d.as_os_str().eq_ignore_ascii_case(p.as_os_str()))
        {
            dirs.push(p);
        }
    };
    if let Some(path) = env("PATH") {
        for p in std::env::split_paths(&path) {
            add(p);
        }
    }
    if let Some(appdata) = env("APPDATA") {
        add(PathBuf::from(appdata).join("npm"));
    }
    if let Some(home) = env("USERPROFILE") {
        let home = PathBuf::from(home);
        add(home.join(".local").join("bin"));
        // FER-494: opencode install script, scoop shims.
        add(home.join(".opencode").join("bin"));
        add(home.join("scoop").join("shims"));
    }
    if let Some(local) = env("LOCALAPPDATA") {
        let local = PathBuf::from(local);
        add(local.join("omp"));
        add(local.join("agy").join("bin"));
        // FER-494: winget command aliases.
        add(local.join("Microsoft").join("WinGet").join("Links"));
    }
    dirs
}

fn node_in(dir: &Path, dirs: &[PathBuf], is_file: &dyn Fn(&Path) -> bool) -> Option<PathBuf> {
    std::iter::once(dir.to_path_buf())
        .chain(dirs.iter().cloned())
        .map(|d| d.join("node.exe"))
        .find(|p| is_file(p))
}

/// Resolve one agent's executable from its definition ([`super::terminal::AGENT_DEFS`]): the
/// native exe in a search folder, else (next to an npm `.cmd` shim) the package's native binary,
/// else `node.exe` + the package's script. Never returns a `.cmd`, `.bat` or `.ps1`.
pub fn resolve(
    agent: AgentId,
    dirs: &[PathBuf],
    is_file: &dyn Fn(&Path) -> bool,
) -> Option<Launch> {
    let def = agent.def();
    for dir in dirs {
        let exe = dir.join(def.exe);
        if is_file(&exe) {
            return Some(Launch::exe(exe));
        }
        let Some(shim) = def.npm_shim else { continue };
        if !is_file(&dir.join(shim)) {
            continue;
        }
        if let Some(native) = (def.npm_native)(dir).into_iter().find(|p| is_file(p)) {
            return Some(Launch::exe(native));
        }
        if let Some(script) = (def.npm_script)(dir).filter(|p| is_file(p))
            && let Some(node) = node_in(dir, dirs, is_file)
        {
            return Some(Launch {
                program: node,
                script: Some(script),
            });
        }
    }
    None
}

fn has_ext(p: &Path, exts: &[&str]) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| exts.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

/// Re-checked before every start: absolute `.exe`, existing; a script only behind `node.exe`
/// (or any `.exe` in the test profile, whose fake agents are scripts too).
pub fn launch_ok(launch: &Launch, test_profile: bool, is_file: &dyn Fn(&Path) -> bool) -> bool {
    let program_ok = launch.program.is_absolute()
        && has_ext(&launch.program, &["exe"])
        && is_file(&launch.program);
    let script_ok = match &launch.script {
        None => true,
        Some(s) => {
            let node = launch
                .program
                .file_name()
                .is_some_and(|n| n.eq_ignore_ascii_case("node.exe"));
            (node || test_profile)
                && s.is_absolute()
                && has_ext(s, &["js", "mjs", "cjs"])
                && is_file(s)
        }
    };
    program_ok && script_ok
}

/// First line of `<agent> --version` (clean env, job object, 15 s, 4 KB).
pub fn version(agent: AgentId, launch: &Launch, cwd: &Path) -> Option<String> {
    let mut args: Vec<OsString> = Vec::new();
    if let Some(s) = &launch.script {
        args.push(s.clone().into_os_string());
    }
    args.push("--version".into());
    let spec = RunSpec {
        program: launch.program.clone(),
        args,
        env: clean_env(agent, &|k| std::env::var_os(k), cwd),
        cwd: cwd.to_path_buf(),
        stdin: Vec::new(),
        timeout: Duration::from_secs(15),
        max_output: 4096,
    };
    let mut first: Option<String> = None;
    let _ = run(&spec, &|| false, &mut |line| {
        let l = line.trim();
        if first.is_none() && !l.is_empty() {
            first = Some(super::parse::short_line(l));
        }
    });
    first
}

/// Fake agents of the test profile: `<profile>/config/agent-cli-test.json`
/// `{"agents":[{"id":"claude","program":"C:\\…\\node.exe","script":"C:\\…\\fake-agent.mjs"}]}`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FakeFile {
    agents: Vec<FakeAgent>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FakeAgent {
    id: AgentId,
    program: PathBuf,
    #[serde(default)]
    script: Option<PathBuf>,
}

pub const FAKE_FILE: &str = "agent-cli-test.json";

fn fake_launches(config_dir: &Path) -> Vec<(AgentId, Launch)> {
    let Ok(raw) = std::fs::read_to_string(config_dir.join(FAKE_FILE)) else {
        return Vec::new();
    };
    let Ok(file) = serde_json::from_str::<FakeFile>(&raw) else {
        return Vec::new();
    };
    file.agents
        .into_iter()
        .map(|a| {
            (
                a.id,
                Launch {
                    program: a.program,
                    script: a.script,
                },
            )
        })
        .collect()
}

/// FER-490 E4: the fake agent the test profile names for `agent`, read again from disk now.
pub fn fake_launch(config_dir: &Path, agent: AgentId) -> Option<Launch> {
    fake_launches(config_dir)
        .into_iter()
        .find(|(id, _)| *id == agent)
        .map(|(_, l)| l)
}

/// FER-490 E3: the executable (and a script entry) and their folders must be writable only by
/// the current user, Administrators, SYSTEM or TrustedInstaller.
pub fn launch_trusted(launch: &Launch) -> Result<(), String> {
    super::acl::trusted_location(&launch.program)?;
    if let Some(script) = &launch.script {
        super::acl::trusted_location(script)?;
    }
    Ok(())
}

fn entry(agent: AgentId, launch: Option<Launch>) -> DetectedAgent {
    match (launch, agent.read_only_mode()) {
        (None, _) => DetectedAgent {
            id: agent,
            status: AgentStatus::NotFound,
            launch: None,
            version: None,
            reason: Some("tidak ditemukan di PATH atau folder instalasi biasa".to_owned()),
            terminal_ready: false,
            terminal_reason: None,
        },
        (Some(launch), Ok(_)) => DetectedAgent {
            id: agent,
            status: AgentStatus::Supported,
            launch: Some(launch),
            version: None,
            reason: None,
            terminal_ready: false,
            terminal_reason: None,
        },
        // Found, but never started (not even `--version`).
        (Some(launch), Err(reason)) => DetectedAgent {
            id: agent,
            status: AgentStatus::Unsupported,
            launch: Some(launch),
            version: None,
            reason: Some(reason.to_owned()),
            terminal_ready: false,
            terminal_reason: None,
        },
    }
}

/// One agent's entry for a resolved launch (pure, unit-tested with fake folders and a fake ACL).
/// FER-494: the ACL check runs for EVERY found agent, it only reads security descriptors and
/// starts nothing (no `--version` here). A failed check blocks both modes and is shown with its
/// reason. In the test profile the fake agents are not ACL-checked.
pub fn classify(
    agent: AgentId,
    launch: Option<Launch>,
    test_profile: bool,
    is_file: &dyn Fn(&Path) -> bool,
    trusted: &dyn Fn(&Launch) -> Result<(), String>,
) -> DetectedAgent {
    let mut e = entry(agent, launch);
    if test_profile && e.status == AgentStatus::NotFound {
        e.reason = Some("profil uji: tidak ada agent palsu untuk agent ini".to_owned());
    }
    let check = e.launch.as_ref().map(|l| {
        if !launch_ok(l, test_profile, is_file) {
            Err("lokasi exe tidak valid (harus .exe absolut yang ada)".to_owned())
        } else if test_profile {
            Ok(())
        } else {
            trusted(l)
        }
    });
    match check {
        None => e.terminal_reason = e.reason.clone(),
        Some(Ok(())) => e.terminal_ready = true,
        Some(Err(reason)) => {
            if e.status == AgentStatus::Supported {
                e.status = AgentStatus::Unsupported;
                e.reason = Some(reason.clone());
            }
            e.terminal_reason = Some(reason);
        }
    }
    e
}

/// Detect every agent. In the test profile only the fake agents named by the profile exist.
/// `--version` runs only for a supported agent whose location passed the ACL check.
pub fn detect_all(test_profile_config: Option<&Path>, scratch: &Path) -> Vec<DetectedAgent> {
    let is_file = |p: &Path| p.is_file();
    let fakes = test_profile_config.map(fake_launches);
    let dirs = search_dirs(&|k| std::env::var_os(k));
    ALL_AGENTS
        .into_iter()
        .map(|agent| {
            let launch = match &fakes {
                Some(f) => f
                    .iter()
                    .find(|(id, _)| *id == agent)
                    .map(|(_, l)| l.clone())
                    .filter(|l| launch_ok(l, true, &is_file)),
                None => resolve(agent, &dirs, &is_file),
            };
            let trusted = |l: &Launch| launch_trusted(l);
            let mut e = classify(agent, launch, fakes.is_some(), &is_file, &trusted);
            if e.status == AgentStatus::Supported {
                e.version = e.launch.as_ref().and_then(|l| version(agent, l, scratch));
            }
            e
        })
        .collect()
}
