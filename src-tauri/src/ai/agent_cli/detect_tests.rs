//! FER-490: executable resolution never yields a shell script; launch re-check before a run.

use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::AgentId;
use super::args::Launch;
use super::detect::*;

fn fs(files: &[&str]) -> impl Fn(&Path) -> bool + use<> {
    let set: HashSet<String> = files.iter().map(|f| f.to_ascii_lowercase()).collect();
    move |p: &Path| set.contains(&p.to_string_lossy().to_ascii_lowercase())
}

fn dirs(d: &[&str]) -> Vec<PathBuf> {
    d.iter().map(PathBuf::from).collect()
}

#[test]
fn claude_npm_shim_resolves_to_the_native_binary() {
    let f = fs(&[
        r"C:\npm\claude.cmd",
        r"C:\npm\claude.ps1",
        r"C:\npm\node_modules\@anthropic-ai\claude-code\bin\claude.exe",
    ]);
    let l = resolve(AgentId::Claude, &dirs(&[r"C:\npm"]), &f).unwrap();
    assert_eq!(
        l,
        Launch::exe(r"C:\npm\node_modules\@anthropic-ai\claude-code\bin\claude.exe")
    );
}

#[test]
fn claude_without_native_binary_runs_node_with_the_script() {
    let f = fs(&[
        r"C:\npm\claude.cmd",
        r"C:\npm\node_modules\@anthropic-ai\claude-code\cli.js",
        r"C:\nodejs\node.exe",
    ]);
    let l = resolve(AgentId::Claude, &dirs(&[r"C:\npm", r"C:\nodejs"]), &f).unwrap();
    assert_eq!(l.program, PathBuf::from(r"C:\nodejs\node.exe"));
    assert_eq!(
        l.script,
        Some(PathBuf::from(
            r"C:\npm\node_modules\@anthropic-ai\claude-code\cli.js"
        ))
    );
}

#[test]
fn claude_native_install_is_used_directly() {
    let f = fs(&[r"C:\Users\u\.local\bin\claude.exe"]);
    let l = resolve(AgentId::Claude, &dirs(&[r"C:\Users\u\.local\bin"]), &f).unwrap();
    assert_eq!(l, Launch::exe(r"C:\Users\u\.local\bin\claude.exe"));
}

#[test]
fn codex_npm_shim_resolves_to_the_vendored_binary() {
    let triple = if cfg!(target_arch = "aarch64") {
        r"codex-win32-arm64\vendor\aarch64-pc-windows-msvc"
    } else {
        r"codex-win32-x64\vendor\x86_64-pc-windows-msvc"
    };
    let native =
        format!(r"C:\npm\node_modules\@openai\codex\node_modules\@openai\{triple}\bin\codex.exe");
    let f = fs(&[r"C:\npm\codex.cmd", &native]);
    let l = resolve(AgentId::Codex, &dirs(&[r"C:\npm"]), &f).unwrap();
    assert_eq!(l, Launch::exe(native));
}

#[test]
fn codex_falls_back_to_node_and_codex_js() {
    let f = fs(&[
        r"C:\npm\codex.cmd",
        r"C:\npm\node.exe",
        r"C:\npm\node_modules\@openai\codex\bin\codex.js",
    ]);
    let l = resolve(AgentId::Codex, &dirs(&[r"C:\npm"]), &f).unwrap();
    assert_eq!(l.program, PathBuf::from(r"C:\npm\node.exe"));
    assert!(l.script.unwrap().ends_with("codex.js"));
}

#[test]
fn shims_alone_never_resolve() {
    let f = fs(&[
        r"C:\npm\claude.cmd",
        r"C:\npm\claude.ps1",
        r"C:\npm\codex.cmd",
        r"C:\npm\codex.bat",
        r"C:\npm\omp.cmd",
        r"C:\npm\agy.cmd",
    ]);
    for a in super::ALL_AGENTS {
        assert!(resolve(a, &dirs(&[r"C:\npm"]), &f).is_none(), "{a:?}");
    }
}

#[test]
fn omp_and_antigravity_resolve_to_their_exe() {
    let f = fs(&[r"C:\L\omp\omp.exe", r"C:\L\agy\bin\agy.exe"]);
    let d = dirs(&[r"C:\L\omp", r"C:\L\agy\bin"]);
    assert_eq!(
        resolve(AgentId::Omp, &d, &f).unwrap(),
        Launch::exe(r"C:\L\omp\omp.exe")
    );
    assert_eq!(
        resolve(AgentId::Antigravity, &d, &f).unwrap(),
        Launch::exe(r"C:\L\agy\bin\agy.exe")
    );
}

#[test]
fn relative_path_entries_are_skipped() {
    let env = |k: &str| -> Option<OsString> {
        match k {
            "PATH" => Some(OsString::from(r".;bin;C:\Tools;C:\tools")),
            "APPDATA" => Some(OsString::from(r"C:\Users\u\AppData\Roaming")),
            _ => None,
        }
    };
    let d = search_dirs(&env);
    assert_eq!(d, dirs(&[r"C:\Tools", r"C:\Users\u\AppData\Roaming\npm"]));
}

#[test]
fn launch_recheck_refuses_scripts_and_relative_paths() {
    let f = fs(&[
        r"C:\a\claude.cmd",
        r"C:\a\claude.exe",
        r"C:\a\node.exe",
        r"C:\a\x.exe",
        r"C:\a\cli.js",
        r"C:\a\run.ps1",
    ]);
    assert!(launch_ok(&Launch::exe(r"C:\a\claude.exe"), false, &f));
    assert!(!launch_ok(&Launch::exe(r"C:\a\claude.cmd"), false, &f));
    assert!(!launch_ok(&Launch::exe(r"a\claude.exe"), false, &f));
    assert!(!launch_ok(&Launch::exe(r"C:\a\missing.exe"), false, &f));
    let node = Launch {
        program: PathBuf::from(r"C:\a\node.exe"),
        script: Some(PathBuf::from(r"C:\a\cli.js")),
    };
    assert!(launch_ok(&node, false, &f));
    let other = Launch {
        program: PathBuf::from(r"C:\a\x.exe"),
        script: Some(PathBuf::from(r"C:\a\cli.js")),
    };
    assert!(!launch_ok(&other, false, &f));
    assert!(launch_ok(&other, true, &f));
    let ps = Launch {
        program: PathBuf::from(r"C:\a\node.exe"),
        script: Some(PathBuf::from(r"C:\a\run.ps1")),
    };
    assert!(!launch_ok(&ps, true, &f));
}
