//! FER-494: agent definitions, detection with fake folders, folder checks, and the exact argv of
//! every terminal launch (wt and console fallback) — the folder never on the command line, no
//! user text, no permission-loosening flag. Nothing real is started: `spawn` is injected.

use std::cell::RefCell;
use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::args::Launch;
use super::detect::{AgentStatus, DetectedAgent, classify, resolve};
use super::settings::AgentSettings;
use super::terminal::*;
use super::{ALL_AGENTS, AgentId};
use crate::domain::{ErrorCode, FsError};
use crate::shell_open::TerminalPref;

fn fs(files: &[&str]) -> impl Fn(&Path) -> bool + use<> {
    let set: HashSet<String> = files.iter().map(|f| f.to_ascii_lowercase()).collect();
    move |p: &Path| set.contains(&p.to_string_lossy().to_ascii_lowercase())
}

fn dirs(d: &[&str]) -> Vec<PathBuf> {
    d.iter().map(PathBuf::from).collect()
}

/// Folder names that would break or inject into wt/cmd if they ever reached a command line.
const NASTY: &[&str] = &[
    r"C:\a;b",
    r"C:\proj; wt -p evil",
    r"C:\100% ^done & more",
    r"C:\x %PATH% !VAR! ^^ && (y) $HOME `q` 's'",
    r"C:\Users\é 漢 字\dir with space",
    r"C:\a ; split-pane ; new-tab cmd",
    r"\\server\share\team;folder",
];

// ── definitions ──────────────────────────────────────────────────────────────────────────────

#[test]
fn every_agent_has_exactly_one_definition_in_display_order() {
    assert_eq!(AGENT_DEFS.len(), ALL_AGENTS.len());
    for (def, id) in AGENT_DEFS.iter().zip(ALL_AGENTS) {
        assert_eq!(def.id, id);
        assert_eq!(id.def().id, id);
        assert_eq!(id.display_name(), def.name);
        assert!(!def.name.is_empty() && !def.vendor.is_empty());
        assert!(def.exe.ends_with(".exe"), "{}", def.exe);
        if let Some(shim) = def.npm_shim {
            assert!(shim.ends_with(".cmd"), "{shim}");
        }
    }
    let wanted = [
        "claude",
        "codex",
        "omp",
        "antigravity",
        "opencode",
        "gemini",
        "aider",
    ];
    let ids: Vec<&str> = ALL_AGENTS.iter().map(|a| a.as_str()).collect();
    assert_eq!(ids, wanted);
}

#[test]
fn interactive_arguments_never_loosen_permissions_or_carry_paths() {
    for def in &AGENT_DEFS {
        for arg in def.terminal_args {
            assert!(!arg.is_empty());
            assert!(!arg.contains('\\') && !arg.contains('/') && !arg.contains(';'));
            for bad in FORBIDDEN_FLAGS {
                assert!(
                    !arg.to_ascii_lowercase().contains(&bad.to_ascii_lowercase()),
                    "{:?}: {arg}",
                    def.id
                );
            }
        }
    }
}

#[test]
fn read_only_mode_is_unchanged_new_agents_are_terminal_only() {
    assert!(AgentId::Claude.read_only_mode().is_ok());
    assert!(AgentId::Codex.read_only_mode().is_ok());
    for id in [
        AgentId::Omp,
        AgentId::Antigravity,
        AgentId::Opencode,
        AgentId::Gemini,
        AgentId::Aider,
    ] {
        assert!(id.read_only_mode().is_err(), "{id:?}");
    }
}

// ── detection with fake folders ──────────────────────────────────────────────────────────────

#[test]
fn new_agents_resolve_from_their_usual_install_folders() {
    let f = fs(&[
        r"C:\npm\opencode.cmd",
        &format!(
            r"C:\npm\node_modules\opencode-ai\node_modules\{}\bin\opencode.exe",
            if cfg!(target_arch = "aarch64") {
                "opencode-windows-arm64"
            } else {
                "opencode-windows-x64"
            }
        ),
        r"C:\npm\gemini.cmd",
        r"C:\npm\node_modules\@google\gemini-cli\dist\index.js",
        r"C:\nodejs\node.exe",
        r"C:\Users\u\.local\bin\aider.exe",
        r"C:\L\agy\bin\agy.exe",
        r"C:\L\omp\omp.exe",
    ]);
    let d = dirs(&[
        r"C:\npm",
        r"C:\nodejs",
        r"C:\Users\u\.local\bin",
        r"C:\L\agy\bin",
        r"C:\L\omp",
    ]);
    let oc = resolve(AgentId::Opencode, &d, &f).unwrap();
    assert!(oc.script.is_none());
    assert!(oc.program.ends_with("opencode.exe"));
    let gm = resolve(AgentId::Gemini, &d, &f).unwrap();
    assert_eq!(gm.program, PathBuf::from(r"C:\nodejs\node.exe"));
    assert_eq!(
        gm.script,
        Some(PathBuf::from(
            r"C:\npm\node_modules\@google\gemini-cli\dist\index.js"
        ))
    );
    assert_eq!(
        resolve(AgentId::Aider, &d, &f),
        Some(Launch::exe(r"C:\Users\u\.local\bin\aider.exe"))
    );
    assert_eq!(
        resolve(AgentId::Antigravity, &d, &f),
        Some(Launch::exe(r"C:\L\agy\bin\agy.exe"))
    );
    assert_eq!(
        resolve(AgentId::Omp, &d, &f),
        Some(Launch::exe(r"C:\L\omp\omp.exe"))
    );
}

#[test]
fn opencode_from_winget_scoop_or_install_script_and_not_found_otherwise() {
    for exe in [
        r"C:\Users\u\AppData\Local\Microsoft\WinGet\Links\opencode.exe",
        r"C:\Users\u\scoop\shims\opencode.exe",
        r"C:\Users\u\.opencode\bin\opencode.exe",
    ] {
        let env = |k: &str| -> Option<OsString> {
            match k {
                "USERPROFILE" => Some(r"C:\Users\u".into()),
                "LOCALAPPDATA" => Some(r"C:\Users\u\AppData\Local".into()),
                _ => None,
            }
        };
        let d = super::detect::search_dirs(&env);
        assert_eq!(
            resolve(AgentId::Opencode, &d, &fs(&[exe])),
            Some(Launch::exe(exe))
        );
    }
    // Only a shim (opencode.cmd) without the package binary: never the .cmd.
    assert_eq!(
        resolve(
            AgentId::Opencode,
            &dirs(&[r"C:\npm"]),
            &fs(&[r"C:\npm\opencode.cmd"])
        ),
        None
    );
    assert_eq!(
        resolve(AgentId::Opencode, &dirs(&[r"C:\npm"]), &fs(&[])),
        None
    );
}

fn ok_trust(_: &Launch) -> Result<(), String> {
    Ok(())
}

fn bad_trust(_: &Launch) -> Result<(), String> {
    Err("folder C:\\x dapat ditulis oleh Everyone".to_owned())
}

#[test]
fn classify_found_agents_are_ready_after_the_acl_check() {
    let f = fs(&[r"C:\L\agy\bin\agy.exe", r"C:\b\claude.exe"]);
    let agy = classify(
        AgentId::Antigravity,
        Some(Launch::exe(r"C:\L\agy\bin\agy.exe")),
        false,
        &f,
        &ok_trust,
    );
    // Terminal yes, read-only chat no (and it is never started for --version).
    assert!(agy.terminal_ready);
    assert_eq!(agy.status, AgentStatus::Unsupported);
    assert!(agy.version.is_none());
    let claude = classify(
        AgentId::Claude,
        Some(Launch::exe(r"C:\b\claude.exe")),
        false,
        &f,
        &ok_trust,
    );
    assert!(claude.terminal_ready);
    assert_eq!(claude.status, AgentStatus::Supported);
}

#[test]
fn classify_acl_failure_blocks_both_modes_with_its_reason() {
    let f = fs(&[r"C:\x\omp.exe", r"C:\x\claude.exe"]);
    let omp = classify(
        AgentId::Omp,
        Some(Launch::exe(r"C:\x\omp.exe")),
        false,
        &f,
        &bad_trust,
    );
    assert!(!omp.terminal_ready);
    assert!(omp.terminal_reason.as_deref().unwrap().contains("Everyone"));
    let claude = classify(
        AgentId::Claude,
        Some(Launch::exe(r"C:\x\claude.exe")),
        false,
        &f,
        &bad_trust,
    );
    assert!(!claude.terminal_ready);
    assert_eq!(claude.status, AgentStatus::Unsupported);
    assert!(claude.reason.as_deref().unwrap().contains("Everyone"));
}

#[test]
fn classify_not_found_and_invalid_launches() {
    let none = classify(AgentId::Opencode, None, false, &fs(&[]), &ok_trust);
    assert_eq!(none.status, AgentStatus::NotFound);
    assert!(!none.terminal_ready);
    assert!(
        none.terminal_reason
            .as_deref()
            .unwrap()
            .contains("tidak ditemukan")
    );
    // A stored launch that is not an existing absolute .exe.
    let bad = classify(
        AgentId::Gemini,
        Some(Launch::exe(r"C:\npm\gemini.cmd")),
        false,
        &fs(&[r"C:\npm\gemini.cmd"]),
        &ok_trust,
    );
    assert!(!bad.terminal_ready);
}

// ── folder checks ────────────────────────────────────────────────────────────────────────────

#[test]
fn folder_strings_with_metacharacters_are_accepted_device_paths_refused() {
    for ok in NASTY {
        assert!(check_path_str(ok).is_ok(), "{ok}");
    }
    assert_eq!(check_path_str(r"C:\a\").as_deref(), Ok(r"C:\a"));
    assert_eq!(check_path_str(r"C:\").as_deref(), Ok(r"C:\"));
    for bad in [
        "",
        "relative\\dir",
        r"\\?\C:\x",
        r"\\.\C:\x",
        r"\\.\pipe\x",
        r"\??\C:\x",
        r"\\?\UNC\server\share",
        r"\\server\pipe\x",
        r"C:\x:stream",
        r"C:\a\..\b",
        r"C:\a\.\b",
        r"C:\a\\b",
        "C:\\a\"b",
        "C:\\a\nb",
        "C:\\a\u{0}b",
        r"C:\a*",
        r"C:\a?",
        r"C:\a|b",
        r"C:\a<b",
        r"C:\dots...",
    ] {
        assert_eq!(check_path_str(bad), Err(FolderError::Invalid), "{bad:?}");
    }
}

#[test]
fn working_folder_of_a_file_is_its_parent() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("a;b x %PATH% ^&c é 漢");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("notes;1.txt");
    std::fs::write(&file, "x").unwrap();
    assert_eq!(working_folder(dir.to_str().unwrap()).unwrap(), dir);
    assert_eq!(working_folder(file.to_str().unwrap()).unwrap(), dir);
    assert_eq!(
        working_folder(dir.join("missing").to_str().unwrap()),
        Err(FolderError::Missing)
    );
}

// ── argv composition ─────────────────────────────────────────────────────────────────────────

fn strs(v: &[OsString]) -> Vec<String> {
    v.iter().map(|a| a.to_string_lossy().into_owned()).collect()
}

/// The trusted, absolute `wt.exe` the tests pretend was resolved (FER-494 T2).
const WT: &str = r"C:\Users\u\AppData\Local\Microsoft\WindowsApps\wt.exe";

fn wt() -> Option<&'static Path> {
    Some(Path::new(WT))
}

#[test]
fn wt_and_console_argv_never_contain_the_folder() {
    let launch = Launch::exe(r"C:\Users\u\.local\bin\claude.exe");
    for folder in NASTY {
        let c = candidates(
            TerminalPref::Auto,
            AgentId::Claude,
            &launch,
            Path::new(folder),
            wt(),
        );
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].via, Via::WindowsTerminal);
        // T2: wt by absolute path, never a bare `wt` search.
        assert_eq!(c[0].program, OsString::from(WT));
        assert_eq!(
            strs(&c[0].args),
            vec!["-d", ".", r"C:\Users\u\.local\bin\claude.exe"]
        );
        assert_eq!(c[0].cwd, PathBuf::from(folder));
        assert_eq!(c[1].via, Via::Console);
        assert_eq!(
            c[1].program,
            OsString::from(r"C:\Users\u\.local\bin\claude.exe")
        );
        assert!(c[1].args.is_empty());
        assert_eq!(c[1].cwd, PathBuf::from(folder));
        for cand in &c {
            for a in cand.argv() {
                assert!(!a.contains(folder), "{folder} leaked into {a}");
                assert!(!a.contains(';') && !a.contains('%') && !a.contains('^'));
            }
            // The real std::process::Command carries the same structured argv + cwd.
            let cmd = cand.command();
            assert_eq!(cmd.get_program(), cand.program.as_os_str());
            assert_eq!(cmd.get_current_dir(), Some(Path::new(folder)));
            let args: Vec<OsString> = cmd.get_args().map(OsString::from).collect();
            assert_eq!(args, cand.args);
        }
    }
}

#[test]
fn every_agent_argv_is_program_plus_fixed_parts_only() {
    for def in &AGENT_DEFS {
        let launch = Launch {
            program: PathBuf::from(r"C:\nodejs\node.exe"),
            script: Some(PathBuf::from(r"C:\npm\node_modules\pkg\cli.js")),
        };
        let c = candidates(
            TerminalPref::Auto,
            def.id,
            &launch,
            Path::new(r"C:\w;x"),
            wt(),
        );
        let mut expect = vec![
            "-d".to_owned(),
            ".".to_owned(),
            r"C:\nodejs\node.exe".to_owned(),
            r"C:\npm\node_modules\pkg\cli.js".to_owned(),
        ];
        expect.extend(def.terminal_args.iter().map(|s| s.to_string()));
        assert_eq!(strs(&c[0].args), expect, "{:?}", def.id);
        assert_eq!(strs(&c[1].args), expect[3..].to_vec(), "{:?}", def.id);
        for cand in &c {
            for a in cand.argv() {
                for bad in FORBIDDEN_FLAGS {
                    assert!(!a.contains(bad), "{:?}: {a}", def.id);
                }
            }
        }
    }
}

#[test]
fn a_program_path_wt_would_split_uses_the_console_only() {
    for program in [r"C:\tools;x\claude.exe", r"-C:\claude.exe"] {
        let c = candidates(
            TerminalPref::Auto,
            AgentId::Claude,
            &Launch::exe(program),
            Path::new(r"C:\w"),
            wt(),
        );
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].via, Via::Console);
    }
    assert!(!wt_safe("a;b".as_ref()));
    assert!(!wt_safe("-d".as_ref()));
    assert!(!wt_safe("".as_ref()));
    assert!(wt_safe(r"C:\Program Files\x\claude.exe".as_ref()));
}

#[test]
fn an_untrusted_or_missing_wt_uses_the_console_only() {
    let launch = Launch::exe(r"C:\b\claude.exe");
    for pref in [TerminalPref::Auto, TerminalPref::WindowsTerminal] {
        let c = candidates(pref, AgentId::Claude, &launch, Path::new(r"C:\w"), None);
        assert_eq!(c.len(), 1, "{pref:?}");
        assert_eq!(c[0].via, Via::Console);
        assert_eq!(c[0].program, OsString::from(r"C:\b\claude.exe"));
    }
    // The resolver drops a wt.exe whose location fails the ACL check.
    let evil = PathBuf::from(r"C:\Evil\wt.exe");
    let bad = |_: &Path| Err("ditolak: C:\\Evil bisa diubah akun lain".to_owned());
    assert_eq!(crate::shell_open::trusted_wt(Some(evil), &bad), None);
}

#[test]
fn shell_preferences_never_route_the_agent_through_a_shell() {
    let launch = Launch::exe(r"C:\b\codex.exe");
    for pref in [TerminalPref::Cmd, TerminalPref::PowerShell] {
        let c = candidates(pref, AgentId::Codex, &launch, Path::new(r"C:\w"), wt());
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].via, Via::Console);
        assert_eq!(c[0].program, OsString::from(r"C:\b\codex.exe"));
    }
    let c = candidates(
        TerminalPref::WindowsTerminal,
        AgentId::Codex,
        &launch,
        Path::new(r"C:\w"),
        wt(),
    );
    assert_eq!(c[0].via, Via::WindowsTerminal);
}

// ── untrusted folder warnings (T1/T3) ────────────────────────────────────────────────────────

fn owner_ok(_: &Path) -> Result<(), String> {
    Ok(())
}

fn owner_bad(_: &Path) -> Result<(), String> {
    Err("folder dimiliki akun lain (S-1-5-21-9-9-9-1001)".to_owned())
}

/// A git root (so the scan never reaches the real profile folders above the temp dir) with
/// `proj\sub` inside.
fn repo() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("repo");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    let proj = repo.join("proj");
    let sub = proj.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    (root, proj, sub)
}

#[test]
fn every_agent_has_its_project_configuration_table() {
    let has = |id: AgentId, needle: &str| {
        id.def().project_configs.iter().any(|c| match c.pattern {
            ConfigPattern::Path(p) => p == needle,
            ConfigPattern::Prefixed { dir, .. } => dir == needle,
        })
    };
    assert!(has(AgentId::Aider, ".aider.conf.yml") && has(AgentId::Aider, ".env"));
    assert!(has(AgentId::Opencode, "opencode.json") && has(AgentId::Opencode, ".opencode"));
    assert!(has(AgentId::Gemini, r".gemini\settings.json"));
    assert!(has(AgentId::Claude, ".claude") && has(AgentId::Claude, ".mcp.json"));
    assert!(has(AgentId::Codex, ".codex"));
    // Only agents with their own folder-trust dialog skip the confirmation.
    for def in &AGENT_DEFS {
        assert_eq!(
            def.own_trust_prompt,
            matches!(def.id, AgentId::Claude | AgentId::Codex),
            "{:?}",
            def.id
        );
    }
}

#[test]
fn a_clean_owned_local_folder_has_no_warning() {
    let (_root, proj, sub) = repo();
    for def in &AGENT_DEFS {
        assert!(assess(def.id, &sub, &owner_ok).is_empty(), "{:?}", def.id);
        assert!(assess(def.id, &proj, &owner_ok).is_empty(), "{:?}", def.id);
    }
}

#[test]
fn project_configuration_is_found_up_to_the_git_root_only() {
    let (root, proj, sub) = repo();
    std::fs::write(proj.join(".aider.conf.yml"), "yes-always: true").unwrap();
    std::fs::write(sub.join(".env"), "OPENAI_API_BASE=http://x").unwrap();
    std::fs::write(proj.join("opencode.json"), "{}").unwrap();
    std::fs::create_dir_all(sub.join(".gemini")).unwrap();
    std::fs::write(sub.join(".gemini").join("settings.json"), "{}").unwrap();
    std::fs::create_dir_all(proj.join(".claude")).unwrap();
    std::fs::write(proj.join(".claude").join("settings.local.json"), "{}").unwrap();
    std::fs::write(proj.join(".claude").join("notes.md"), "x").unwrap();
    // Above the git root: never looked at.
    std::fs::write(root.path().join(".aider.conf.yml"), "auto-test: true").unwrap();
    std::fs::write(root.path().join("opencode.json"), "{}").unwrap();

    let aider = assess(AgentId::Aider, &sub, &owner_ok);
    assert_eq!(aider.len(), 2, "{aider:?}");
    assert!(aider[0].contains(".env") && aider[0].contains("mengalihkan API"));
    assert!(aider[1].contains(".aider.conf.yml") && aider[1].contains("perintah otomatis"));
    assert!(aider[1].starts_with("Aider akan membaca "));
    let oc = assess(AgentId::Opencode, &sub, &owner_ok);
    assert!(oc.len() == 1 && oc[0].contains("opencode.json"), "{oc:?}");
    let gm = assess(AgentId::Gemini, &sub, &owner_ok);
    assert!(gm.len() == 1 && gm[0].contains("settings.json"), "{gm:?}");
    let cl = assess(AgentId::Claude, &sub, &owner_ok);
    assert!(
        cl.len() == 1 && cl[0].contains("settings.local.json"),
        "{cl:?}"
    );
    // omp / agy: no known project configuration.
    assert!(assess(AgentId::Omp, &sub, &owner_ok).is_empty());
}

#[test]
fn unc_and_foreign_owned_folders_warn() {
    let unc = assess(AgentId::Omp, Path::new(r"\\server\share\team"), &owner_ok);
    assert!(unc.iter().any(|w| w.contains("UNC")), "{unc:?}");
    let (_root, proj, _) = repo();
    let foreign = assess(AgentId::Gemini, &proj, &owner_bad);
    assert_eq!(foreign.len(), 1);
    assert!(foreign[0].contains("dimiliki akun lain") && foreign[0].contains("DLL"));
}

// ── request flow (nothing real is started) ───────────────────────────────────────────────────

#[test]
fn the_request_has_no_room_for_a_question() {
    let ok: TerminalOpenRequest =
        serde_json::from_str(r#"{"agent":"claude","path":"C:\\x","program":"auto"}"#).unwrap();
    assert_eq!(ok.agent, "claude");
    for extra in [
        // FER-516: the webview has no flag that could authorize an untrusted folder any more.
        r#"{"agent":"opencode","path":"C:\\x","confirmedUntrusted":true}"#,
        r#"{"agent":"claude","path":"C:\\x","question":"hapus semua file"}"#,
        r#"{"agent":"claude","path":"C:\\x","args":["--dangerously-skip-permissions"]}"#,
        r#"{"agent":"claude","path":"C:\\x","prompt":"x"}"#,
    ] {
        assert!(
            serde_json::from_str::<TerminalOpenRequest>(extra).is_err(),
            "{extra}"
        );
    }
}

struct Fx {
    _root: tempfile::TempDir,
    exe: PathBuf,
    folder: PathBuf,
    config: PathBuf,
    data: PathBuf,
}

fn fx() -> Fx {
    let root = tempfile::tempdir().unwrap();
    // A git root: the configuration scan stops here.
    std::fs::create_dir_all(root.path().join(".git")).unwrap();
    let bin = root.path().join("bin");
    let folder = root.path().join("proj; wt -p x %PATH% ^&c é");
    let config = root.path().join("config");
    let data = root.path().join("data");
    for d in [&bin, &folder, &config, &data] {
        std::fs::create_dir_all(d).unwrap();
    }
    // A placeholder file: launch_ok needs an existing `.exe`; it is never started.
    let exe = bin.join("claude.exe");
    std::fs::write(&exe, b"not a program").unwrap();
    Fx {
        _root: root,
        exe,
        folder,
        config,
        data,
    }
}

fn settings_for(launch: &Launch, ready: bool) -> AgentSettings {
    AgentSettings {
        agents: ALL_AGENTS
            .into_iter()
            .map(|id| DetectedAgent {
                id,
                status: AgentStatus::Supported,
                launch: Some(launch.clone()),
                version: None,
                reason: None,
                terminal_ready: ready,
                terminal_reason: (!ready).then(|| "ACL gagal".to_owned()),
            })
            .collect(),
        ..Default::default()
    }
}

fn req(path: &Path) -> TerminalOpenRequest {
    TerminalOpenRequest {
        agent: "claude".into(),
        path: path.to_string_lossy().into_owned(),
        program: Some("auto".into()),
    }
}

fn req_for(agent: &str, path: &Path) -> TerminalOpenRequest {
    TerminalOpenRequest {
        agent: agent.into(),
        ..req(path)
    }
}

type Confirm<'a> = dyn Fn(&str, &str) -> Result<bool, FsError> + 'a;

/// A confirmation that must never be asked (trusted folders, refusals before the dialog).
fn no_dialog(_: &str, _: &str) -> Result<bool, FsError> {
    panic!("no native confirmation expected")
}

type Trust<'a> = dyn Fn(&Launch) -> Result<(), String> + 'a;
type Owner<'a> = dyn Fn(&Path) -> Result<(), String> + 'a;
type Spawn<'a> = dyn Fn(Vec<TerminalCommand>) -> std::io::Result<TerminalCommand> + 'a;

fn mk<'a>(
    s: &'a AgentSettings,
    trusted: &'a Trust<'a>,
    folder_owner: &'a Owner<'a>,
    spawn: &'a Spawn<'a>,
    confirm: &'a Confirm<'a>,
) -> OpenContext<'a> {
    OpenContext {
        settings: s,
        test_profile_config: None,
        dry_run_log: None,
        trusted,
        folder_owner,
        wt: wt().map(Path::to_path_buf),
        spawn,
        confirm,
    }
}

#[test]
fn open_starts_the_composed_command_once_with_the_folder_as_cwd() {
    let f = fx();
    let launch = Launch::exe(&f.exe);
    let s = settings_for(&launch, true);
    let seen: RefCell<Vec<Vec<TerminalCommand>>> = RefCell::new(Vec::new());
    let spawn = |c: Vec<TerminalCommand>| {
        seen.borrow_mut().push(c.clone());
        Ok(c[0].clone())
    };
    let ctx = mk(&s, &ok_trust, &owner_ok, &spawn, &no_dialog);
    let file = f.folder.join("readme.md");
    std::fs::write(&file, "x").unwrap();
    let out = open(&ctx, &req(&file)).unwrap();
    assert!(!out.dry_run);
    assert!(out.warnings.is_empty());
    assert_eq!(out.via, Some(Via::WindowsTerminal));
    assert_eq!(
        out.argv,
        vec![
            WT.to_owned(),
            "-d".into(),
            ".".into(),
            f.exe.to_string_lossy().into_owned()
        ]
    );
    assert_eq!(PathBuf::from(&out.cwd), f.folder);
    let seen = seen.borrow();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0][1].via, Via::Console);
    assert_eq!(seen[0][1].cwd, f.folder);
}

#[test]
fn a_clean_folder_asks_only_for_agents_without_their_own_trust_prompt() {
    let f = fx();
    let s = settings_for(&Launch::exe(&f.exe), true);
    let count = RefCell::new(0);
    let spawn = |c: Vec<TerminalCommand>| {
        *count.borrow_mut() += 1;
        Ok(c[0].clone())
    };
    let asked = RefCell::new(0);
    let yes = |_: &str, _: &str| {
        *asked.borrow_mut() += 1;
        Ok(true)
    };
    let ctx = mk(&s, &ok_trust, &owner_ok, &spawn, &yes);
    for id in ALL_AGENTS {
        let out = open(&ctx, &req_for(id.as_str(), &f.folder)).unwrap();
        assert!(out.via.is_some(), "{id:?}");
    }
    assert_eq!(*count.borrow(), ALL_AGENTS.len());
    let expected = ALL_AGENTS
        .into_iter()
        .filter(|id| !id.def().own_trust_prompt)
        .count();
    assert_eq!(*asked.borrow(), expected);
}

/// Build a folder holding project configuration the given agent loads.
fn untrusted_case(f: &Fx, agent: &str, file: &str, sub: &[&str]) -> PathBuf {
    let dir = f.folder.join(format!("case-{agent}"));
    let cfg_dir = sub.iter().fold(dir.clone(), |p, s| p.join(s));
    std::fs::create_dir_all(&cfg_dir).unwrap();
    std::fs::write(cfg_dir.join(file), "x").unwrap();
    dir
}

const CASES: [(&str, &str, &[&str]); 3] = [
    ("aider", ".aider.conf.yml", &[]),
    ("opencode", "opencode.json", &[]),
    ("gemini", "settings.json", &[".gemini"]),
];

#[test]
fn an_untrusted_folder_starts_only_after_the_native_yes() {
    let f = fx();
    let s = settings_for(&Launch::exe(&f.exe), true);
    let started = RefCell::new(Vec::<PathBuf>::new());
    let spawn = |c: Vec<TerminalCommand>| {
        started.borrow_mut().push(c[0].cwd.clone());
        Ok(c[0].clone())
    };
    for (agent, file, sub) in CASES {
        let dir = untrusted_case(&f, agent, file, sub);
        let asked = RefCell::new(Vec::<(String, String)>::new());
        // "No": nothing is spawned.
        let no = |c: &str, t: &str| {
            asked.borrow_mut().push((c.to_owned(), t.to_owned()));
            Ok(false)
        };
        let ctx = mk(&s, &ok_trust, &owner_ok, &spawn, &no);
        let e = open(&ctx, &req_for(agent, &dir)).unwrap_err();
        assert_eq!(e.kind, "cancelled", "{agent}");
        assert!(started.borrow().is_empty(), "{agent}: started after No");
        assert_eq!(asked.borrow().len(), 1, "{agent}");
        // The text names the agent and the full folder path and warns about project config.
        let (caption, text) = asked.borrow()[0].clone();
        assert_eq!(caption, CONFIRM_CAPTION);
        let name = AgentId::parse(agent).unwrap().def().name;
        assert!(text.contains(name), "{agent}: {text}");
        assert!(text.contains(dir.to_string_lossy().as_ref()), "{text}");
        assert!(text.contains("menjalankan program"), "{text}");
        assert!(text.contains(file), "{text}");
        // "Yes": started exactly once, in the same folder.
        let yes = |_: &str, _: &str| Ok(true);
        let ctx = mk(&s, &ok_trust, &owner_ok, &spawn, &yes);
        let out = open(&ctx, &req_for(agent, &dir)).unwrap();
        assert!(out.warnings[0].contains(file), "{:?}", out.warnings);
        assert_eq!(started.borrow().len(), 1, "{agent}");
        assert_eq!(started.borrow_mut().remove(0), dir);
    }
}

#[test]
fn a_busy_dialog_guard_or_a_failed_dialog_never_starts_anything() {
    let f = fx();
    let s = settings_for(&Launch::exe(&f.exe), true);
    let spawn =
        |_: Vec<TerminalCommand>| -> std::io::Result<TerminalCommand> { panic!("must not start") };
    let dir = untrusted_case(&f, "opencode", "opencode.json", &[]);
    // Real guard: another native dialog is open, so the real helper answers Busy without a window.
    let _serial = crate::native_dialog::GUARD_TESTS
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let held = crate::native_dialog::DialogGuard::try_acquire().unwrap();
    let busy = |c: &str, t: &str| crate::native_dialog::confirm_or_busy(0, c, t);
    let ctx = mk(&s, &ok_trust, &owner_ok, &spawn, &busy);
    assert_eq!(
        open(&ctx, &req_for("opencode", &dir)).unwrap_err().kind,
        "busy"
    );
    drop(held);
    // Any other dialog failure is a plain refusal, never a start.
    let broken = |_: &str, _: &str| Err(FsError::new(ErrorCode::Io, "x"));
    let ctx = mk(&s, &ok_trust, &owner_ok, &spawn, &broken);
    assert_eq!(
        open(&ctx, &req_for("opencode", &dir)).unwrap_err().kind,
        "cancelled"
    );
}

#[test]
fn a_bidi_or_format_character_in_the_folder_is_refused_without_a_dialog() {
    let f = fx();
    let s = settings_for(&Launch::exe(&f.exe), true);
    let spawn =
        |_: Vec<TerminalCommand>| -> std::io::Result<TerminalCommand> { panic!("must not start") };
    let ctx = mk(&s, &ok_trust, &owner_ok, &spawn, &no_dialog);
    for ch in ['\u{202E}', '\u{2066}', '\u{200B}', '\u{FEFF}'] {
        let dir = f.folder.join(format!("rep{ch}ort"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("opencode.json"), "x").unwrap();
        let e = open(&ctx, &req_for("opencode", &dir)).unwrap_err();
        assert_eq!(e.kind, "folder_invalid", "U+{:04X}", ch as u32);
    }
    // The text builder refuses the same characters in a finding, and control characters.
    assert!(confirm_text("x", Path::new(r"C:\a"), &["bad \u{202E}".to_owned()]).is_none());
    assert!(confirm_text("x", Path::new("C:\\a\nb"), &[]).is_none());
    assert!(confirm_text("x", Path::new(r"C:\a"), &["ok".to_owned()]).is_some());
}

#[test]
fn an_agent_without_a_trust_prompt_always_asks_even_in_a_clean_folder() {
    let f = fx();
    let s = settings_for(&Launch::exe(&f.exe), true);
    for agent in ["opencode", "aider", "gemini", "omp", "antigravity"] {
        let asked = RefCell::new(0);
        let started = RefCell::new(0);
        let spawn = |c: Vec<TerminalCommand>| {
            *started.borrow_mut() += 1;
            Ok(c[0].clone())
        };
        // "No" in a folder with no configuration: nothing starts.
        let no = |_: &str, text: &str| {
            *asked.borrow_mut() += 1;
            assert!(text.contains("Buka "), "{text}");
            Ok(false)
        };
        let ctx = mk(&s, &ok_trust, &owner_ok, &spawn, &no);
        let e = open(&ctx, &req_for(agent, &f.folder)).unwrap_err();
        assert_eq!(e.kind, "cancelled", "{agent}");
        assert_eq!((*asked.borrow(), *started.borrow()), (1, 0), "{agent}");
        // "Yes": started once.
        let yes = |_: &str, _: &str| Ok(true);
        let ctx = mk(&s, &ok_trust, &owner_ok, &spawn, &yes);
        open(&ctx, &req_for(agent, &f.folder)).unwrap();
        assert_eq!(*started.borrow(), 1, "{agent}");
    }
}

#[test]
fn a_configuration_planted_after_the_scan_cannot_skip_the_dialog() {
    // The scan sees a clean folder (owner check says fine, nothing on disk yet); the file appears
    // while the dialog is open (the confirmation closure plants it). The dialog was still asked
    // and the answer "No" stops everything.
    let f = fx();
    let s = settings_for(&Launch::exe(&f.exe), true);
    let started = RefCell::new(0);
    let spawn = |c: Vec<TerminalCommand>| {
        *started.borrow_mut() += 1;
        Ok(c[0].clone())
    };
    let dir = f.folder.join("late");
    std::fs::create_dir_all(&dir).unwrap();
    let plant_then_no = |_: &str, _: &str| {
        std::fs::write(dir.join("opencode.json"), "{}").unwrap();
        Ok(false)
    };
    let ctx = mk(&s, &ok_trust, &owner_ok, &spawn, &plant_then_no);
    assert_eq!(
        open(&ctx, &req_for("opencode", &dir)).unwrap_err().kind,
        "cancelled"
    );
    assert_eq!(*started.borrow(), 0);
    // Planted between the request and the scan outcome is the same: a clean scan still asks.
    let other = f.folder.join("late2");
    std::fs::create_dir_all(&other).unwrap();
    let asked = RefCell::new(false);
    let no = |_: &str, _: &str| {
        *asked.borrow_mut() = true;
        Ok(false)
    };
    let ctx = mk(&s, &ok_trust, &owner_ok, &spawn, &no);
    assert!(open(&ctx, &req_for("gemini", &other)).is_err());
    assert!(*asked.borrow());
}

#[test]
fn the_dialog_text_is_tidy_and_bounded_but_the_cwd_stays_complete() {
    let long = format!(r"C:\{}", "segment\\".repeat(60));
    let text = confirm_text("Aider", Path::new(&long), &[format!("{long} x")]).unwrap();
    assert!(!text.contains("  "), "no runs of spaces: {text}");
    assert!(text.contains('\u{2026}'), "long path is cut in the middle");
    assert!(text.chars().count() < 900, "{}", text.chars().count());
    assert!(text.starts_with("Buka Aider di folder ini?"));
    // Short paths are shown whole.
    let short = confirm_text("Aider", Path::new(r"C:\a\b"), &[]).unwrap();
    assert!(short.contains(r"C:\a\b") && !short.contains("Temuan"));
}

#[test]
fn the_executable_is_rechecked_after_a_yes() {
    let f = fx();
    let s = settings_for(&Launch::exe(&f.exe), true);
    let spawn =
        |_: Vec<TerminalCommand>| -> std::io::Result<TerminalCommand> { panic!("must not start") };
    let calls = RefCell::new(0);
    let flaky_trust = |_: &Launch| {
        *calls.borrow_mut() += 1;
        if *calls.borrow() >= 2 {
            Err("lokasi berubah".to_owned())
        } else {
            Ok(())
        }
    };
    let yes = |_: &str, _: &str| Ok(true);
    let ctx = mk(&s, &flaky_trust, &owner_ok, &spawn, &yes);
    let e = open(&ctx, &req_for("opencode", &f.folder)).unwrap_err();
    assert_eq!(e.kind, "agent_untrusted");
}

/// FER-516 (structural): no AI path reaches `agent_terminal_open` / `terminal::open`, and inside
/// `open` the native confirmation comes before the spawn.
#[test]
fn no_ai_adapter_reaches_the_terminal_open_and_confirm_precedes_spawn() {
    let ai_sources = [
        ("contract.rs", include_str!("../contract.rs")),
        ("executor.rs", include_str!("../executor.rs")),
        ("exec_commands.rs", include_str!("../exec_commands.rs")),
        ("approval.rs", include_str!("../approval.rs")),
        (
            "approval_commands.rs",
            include_str!("../approval_commands.rs"),
        ),
        ("provider.rs", include_str!("../provider.rs")),
        ("ask.rs", include_str!("../ask.rs")),
        ("attach_commands.rs", include_str!("../attach_commands.rs")),
        ("commands.rs", include_str!("../commands.rs")),
    ];
    for (name, src) in ai_sources {
        assert!(
            !src.contains("agent_terminal_open") && !src.contains("terminal::open"),
            "ai/{name} must not reach the agent terminal"
        );
    }
    // Inside agent_cli only commands.rs calls the flow (the one command with the dialog seam).
    for (name, src) in [
        ("runner.rs", include_str!("runner.rs")),
        ("snapshot.rs", include_str!("snapshot.rs")),
        ("detect.rs", include_str!("detect.rs")),
    ] {
        assert!(!src.contains("terminal::open"), "agent_cli/{name}");
    }
    let commands = include_str!("commands.rs");
    assert!(commands.contains("confirm_or_busy(owner, caption, text)"));
    let src = include_str!("terminal.rs");
    let body = &src[src.find("pub fn open(").unwrap()..];
    assert!(body.find("(ctx.confirm)").unwrap() < body.find("(ctx.spawn)").unwrap());
}

#[test]
fn claude_and_codex_ask_natively_only_when_the_scan_finds_configuration() {
    let f = fx();
    let s = settings_for(&Launch::exe(&f.exe), true);
    let started = RefCell::new(0);
    let spawn = |c: Vec<TerminalCommand>| {
        *started.borrow_mut() += 1;
        Ok(c[0].clone())
    };
    // Clean folder: no dialog (their own trust prompt covers the rest).
    let ctx = mk(&s, &ok_trust, &owner_ok, &spawn, &no_dialog);
    for agent in ["claude", "codex"] {
        assert!(
            open(&ctx, &req_for(agent, &f.folder))
                .unwrap()
                .via
                .is_some()
        );
    }
    // Project configuration found: the native dialog decides.
    std::fs::write(f.folder.join(".mcp.json"), "{}").unwrap();
    std::fs::create_dir_all(f.folder.join(".codex")).unwrap();
    *started.borrow_mut() = 0;
    let no = |_: &str, _: &str| Ok(false);
    let ctx = mk(&s, &ok_trust, &owner_ok, &spawn, &no);
    for agent in ["claude", "codex"] {
        assert_eq!(
            open(&ctx, &req_for(agent, &f.folder)).unwrap_err().kind,
            "cancelled",
            "{agent}"
        );
    }
    assert_eq!(*started.borrow(), 0);
    let yes = |_: &str, _: &str| Ok(true);
    let ctx = mk(&s, &ok_trust, &owner_ok, &spawn, &yes);
    for agent in ["claude", "codex"] {
        let out = open(&ctx, &req_for(agent, &f.folder)).unwrap();
        assert_eq!(out.warnings.len(), 1, "{agent}: {:?}", out.warnings);
    }
    assert_eq!(*started.borrow(), 2);
    // A claude hook file counts too.
    let hook = f.folder.join("hooked");
    std::fs::create_dir_all(hook.join(".claude")).unwrap();
    std::fs::write(hook.join(".claude").join("settings.local.json"), "{}").unwrap();
    let no = |_: &str, _: &str| Ok(false);
    let ctx = mk(&s, &ok_trust, &owner_ok, &spawn, &no);
    assert_eq!(
        open(&ctx, &req_for("claude", &hook)).unwrap_err().kind,
        "cancelled"
    );
}

#[test]
fn open_without_a_trusted_wt_starts_the_console_route() {
    let f = fx();
    let s = settings_for(&Launch::exe(&f.exe), true);
    let spawn = |c: Vec<TerminalCommand>| {
        assert_eq!(c.len(), 1);
        Ok(c[0].clone())
    };
    let mut ctx = mk(&s, &ok_trust, &owner_ok, &spawn, &no_dialog);
    ctx.wt = None;
    let out = open(&ctx, &req(&f.folder)).unwrap();
    assert_eq!(out.via, Some(Via::Console));
    assert_eq!(out.argv, vec![f.exe.to_string_lossy().into_owned()]);
}

#[test]
fn open_refuses_unready_untrusted_and_invalid_folders_without_starting() {
    let f = fx();
    let launch = Launch::exe(&f.exe);
    let spawn =
        |_: Vec<TerminalCommand>| -> std::io::Result<TerminalCommand> { panic!("must not start") };
    let unready = settings_for(&launch, false);
    let ready = settings_for(&launch, true);
    let ctx = |s, t| mk(s, t, &owner_ok, &spawn, &no_dialog);
    let e = open(&ctx(&unready, &ok_trust), &req(&f.folder)).unwrap_err();
    assert_eq!(e.kind, "agent_unavailable");
    assert_eq!(e.message.as_deref(), Some("ACL gagal"));
    let e = open(&ctx(&ready, &bad_trust), &req(&f.folder)).unwrap_err();
    assert_eq!(e.kind, "agent_untrusted");
    let e = open(&ctx(&ready, &ok_trust), &req(Path::new(r"\\?\C:\Windows"))).unwrap_err();
    assert_eq!(e.kind, "folder_invalid");
    let e = open(&ctx(&ready, &ok_trust), &req(&f.folder.join("nope"))).unwrap_err();
    assert_eq!(e.kind, "folder_missing");
    let only_claude = AgentSettings {
        agents: ready.agents[..1].to_vec(),
        ..Default::default()
    };
    let mut other = req(&f.folder);
    other.agent = "opencode".into();
    assert_eq!(
        open(&ctx(&only_claude, &ok_trust), &other)
            .unwrap_err()
            .kind,
        "agent_unavailable"
    );
}

#[test]
fn test_profile_records_a_dry_run_and_starts_nothing() {
    let f = fx();
    let launch = Launch::exe(&f.exe);
    std::fs::write(
        f.config.join(super::detect::FAKE_FILE),
        serde_json::json!({"agents":[{"id":"claude","program": f.exe}]}).to_string(),
    )
    .unwrap();
    let s = settings_for(&launch, true);
    let spawn = |_: Vec<TerminalCommand>| -> std::io::Result<TerminalCommand> {
        panic!("test profile must not start anything")
    };
    let log = f.data.join(DRY_RUN_LOG);
    let mut ctx = mk(&s, &bad_trust, &owner_ok, &spawn, &no_dialog); // trust not consulted for fakes
    ctx.test_profile_config = Some(f.config.clone());
    ctx.dry_run_log = Some(log.clone());
    let out = open(&ctx, &req(&f.folder)).unwrap();
    assert!(out.dry_run);
    assert_eq!(out.argv[..3], [WT, "-d", "."]);
    let line = std::fs::read_to_string(&log).unwrap();
    assert!(line.contains("\"dryRun\":true"));

    // A launch other than the profile's fake (e.g. a real exe written into the settings) is refused.
    let other_exe = f.folder.join("real.exe");
    std::fs::write(&other_exe, b"x").unwrap();
    let s2 = settings_for(&Launch::exe(&other_exe), true);
    let ctx2 = OpenContext {
        settings: &s2,
        ..ctx
    };
    assert_eq!(
        open(&ctx2, &req(&f.folder)).unwrap_err().kind,
        "agent_unavailable"
    );
}
