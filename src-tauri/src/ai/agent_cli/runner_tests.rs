//! FER-490 integration: the whole `ask` flow against the fake agent (node + fake-agent.mjs).
//! Needs `node.exe` on PATH (a dev dependency of the frontend). Checks: flags and the clean env
//! reach the process, the prompt only arrives on STDIN, an agent that writes files or prints
//! "instructions" still only produces text, cancel/timeout kill the whole process tree, big
//! output is cut, the working folder is removed, attachment gates refuse before any start.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::AgentId;
use super::args::{AGENT_RULES, Launch};
use super::commands::{AgentAskRequest, AgentError, AskContext, StreamEvent, ask, too_broad};
use super::detect::{AgentStatus, DetectedAgent};
use super::settings::AgentSettings;
use super::snapshot::Limits;
use crate::ai::attach::AttachmentRef;
use crate::ai::provider::CancelToken;

fn node() -> PathBuf {
    let dirs = super::detect::search_dirs(&|k| std::env::var_os(k));
    dirs.iter()
        .map(|d| d.join("node.exe"))
        .find(|p| p.is_file())
        .expect("node.exe on PATH (needed for the fake agent)")
}

fn fake_script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(r"src\ai\agent_cli\testdata\fake-agent.mjs")
}

fn settings(acked: bool) -> AgentSettings {
    let launch = Launch {
        program: node(),
        script: Some(fake_script()),
    };
    let agents = [AgentId::Claude, AgentId::Codex]
        .into_iter()
        .map(|id| DetectedAgent {
            id,
            status: AgentStatus::Supported,
            launch: Some(launch.clone()),
            version: Some("fake".into()),
            reason: None,
            terminal_ready: true,
            terminal_reason: None,
        })
        .chain([DetectedAgent {
            id: AgentId::Omp,
            status: AgentStatus::Unsupported,
            launch: Some(Launch::exe(node())),
            version: None,
            reason: Some("x".into()),
            terminal_ready: true,
            terminal_reason: None,
        }])
        .collect();
    AgentSettings {
        agents,
        detected_at_ms: 1,
        default_agent: Some(AgentId::Claude),
        acknowledged: if acked {
            vec![AgentId::Claude, AgentId::Codex, AgentId::Omp]
        } else {
            vec![]
        },
        // Codex is broad-read (E2): the tests that run it opt in like the user would.
        opted_in: vec![AgentId::Codex],
        ..Default::default()
    }
}

struct Fixture {
    root: tempfile::TempDir,
    docs: PathBuf,
    runs: PathBuf,
    /// Test-profile config dir holding `agent-cli-test.json` (the fakes, E4).
    config: PathBuf,
}

fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let docs = root.path().join("docs");
    std::fs::create_dir_all(&docs).unwrap();
    std::fs::write(docs.join("report.txt"), "The secret fruit is MANGO.\n").unwrap();
    std::fs::write(docs.join(".env"), "API_KEY=abcdefghijklmnop\n").unwrap();
    std::fs::write(docs.join("sibling.txt"), "SIBLING-DATA\n").unwrap();
    let runs = root.path().join("runs");
    let config = root.path().join("config");
    std::fs::create_dir_all(&config).unwrap();
    let fake = serde_json::json!({
        "agents": [
            {"id": "claude", "program": node(), "script": fake_script()},
            {"id": "codex", "program": node(), "script": fake_script()},
        ]
    });
    std::fs::write(config.join(super::detect::FAKE_FILE), fake.to_string()).unwrap();
    Fixture {
        docs,
        runs,
        config,
        root,
    }
}

fn request(agent: &str, question: &str, paths: &[&Path]) -> AgentAskRequest {
    AgentAskRequest {
        request_id: format!("ask-test-{}", std::process::id()),
        agent: agent.into(),
        question: question.into(),
        history: vec![],
        attachments: paths
            .iter()
            .map(|p| AttachmentRef {
                path: p.to_string_lossy().into_owned(),
                confirmed_sensitive: false,
            })
            .collect(),
    }
}

fn env(k: &str) -> Option<OsString> {
    std::env::var_os(k)
}

fn run_ask(
    fx: &Fixture,
    s: &AgentSettings,
    req: &AgentAskRequest,
    timeout: Duration,
    cancel: &CancelToken,
    on: &mut dyn FnMut(&StreamEvent),
) -> Result<super::commands::AgentOutcome, AgentError> {
    let ctx = AskContext {
        settings: s,
        runs_dir: fx.runs.clone(),
        test_profile_config: Some(fx.config.clone()),
        timeout,
        max_output: super::MAX_OUTPUT_BYTES,
        limits: Limits::default(),
        env: &env,
    };
    ask(&ctx, req, cancel, &mut |ev| on(&ev))
}

fn runs_empty(fx: &Fixture) -> bool {
    std::fs::read_dir(&fx.runs)
        .map(|mut d| d.next().is_none())
        .unwrap_or(true)
}

// Fake-agent runs start node processes; keep them sequential.
static SERIAL: Mutex<()> = Mutex::new(());

#[test]
fn answer_streams_and_the_working_folder_is_removed() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let fx = fixture();
    let s = settings(true);
    for agent in ["claude", "codex"] {
        let file = fx.docs.join("report.txt");
        let mut events = Vec::new();
        let out = run_ask(
            &fx,
            &s,
            &request(agent, "file ini tentang apa?", &[&file]),
            Duration::from_secs(60),
            &CancelToken::default(),
            &mut |e| events.push(e.clone()),
        )
        .unwrap();
        assert_eq!(
            out.answer, "Jawaban palsu: 1 lampiran (report.txt).",
            "{agent}"
        );
        assert!(!out.truncated);
        assert!(
            events
                .iter()
                .any(|e| e.kind == "text" && e.text == "Sedang membaca lampiran.")
        );
        assert!(events.iter().any(|e| e.kind == "status"));
        assert!(runs_empty(&fx), "{agent}: working folder left behind");
    }
}

#[test]
fn flags_env_and_stdin_reach_the_agent_and_nothing_else() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let fx = fixture();
    let s = settings(true);
    let file = fx.docs.join("report.txt");
    for agent in [AgentId::Claude, AgentId::Codex] {
        let out = run_ask(
            &fx,
            &s,
            &request(agent.as_str(), "MODE:echo QUESTION-91c2", &[&file]),
            Duration::from_secs(60),
            &CancelToken::default(),
            &mut |_| {},
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&out.answer).unwrap();
        let argv: Vec<String> = serde_json::from_value(v["argv"].clone()).unwrap();
        let joined = argv.join("\u{1}");
        assert!(
            !joined.contains("QUESTION-91c2"),
            "{agent:?}: question on the command line"
        );
        assert!(
            !joined.contains("report.txt"),
            "{agent:?}: file path on the command line"
        );
        match agent {
            AgentId::Claude => {
                assert_eq!(argv[0], "-p");
                assert!(argv.contains(&"--restricted".to_owned()));
                assert!(argv.contains(&AGENT_RULES.to_owned()));
                // E1: no folder outside the working folder is opened to the agent.
                assert!(!argv.iter().any(|a| a == "--add-dir"));
                assert!(
                    argv.contains(&String::new()),
                    "empty --setting-sources survives quoting"
                );
            }
            _ => {
                assert_eq!(&argv[..3], &["exec", "--sandbox", "read-only"]);
                assert_eq!(argv.last().unwrap(), "-");
            }
        }
        let names: Vec<String> = serde_json::from_value(v["envNames"].clone()).unwrap();
        for n in &names {
            let up = n.to_ascii_uppercase();
            assert!(!up.starts_with("FERRUM"), "{agent:?}: {n}");
            assert!(
                !up.contains("CARGO") && !up.starts_with("RUST"),
                "{agent:?}: {n}"
            );
        }
        let stdin = v["stdin"].as_str().unwrap();
        assert!(stdin.contains("QUESTION-91c2"));
        // E1: the prompt names the COPY relative to the working folder, not the original.
        assert!(
            stdin.contains(r"- file: files\report.txt (27 bytes)"),
            "{stdin}"
        );
        assert!(!stdin.contains(&fx.docs.to_string_lossy().into_owned()));
        let cwd = v["cwd"].as_str().unwrap();
        assert!(Path::new(cwd).starts_with(&fx.runs), "{agent:?}: cwd {cwd}");
    }
    assert!(runs_empty(&fx));
}

#[test]
fn an_agent_that_writes_and_injects_still_only_yields_text() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let fx = fixture();
    let s = settings(true);
    let file = fx.docs.join("report.txt");
    let mut events = Vec::new();
    let out = run_ask(
        &fx,
        &s,
        &request("claude", "MODE:evil", &[&file]),
        Duration::from_secs(60),
        &CancelToken::default(),
        &mut |e| events.push(e.clone()),
    )
    .unwrap();
    // Plain text for the chat; the "intent" JSON and the HTML are just characters.
    assert!(out.answer.starts_with("EVIL 2: run \"rm -rf /\""));
    assert!(
        events
            .iter()
            .any(|e| e.text.contains(r#"{"intent":"delete""#))
    );
    // The tool call shows a fixed label only (never its input).
    assert!(
        events
            .iter()
            .any(|e| e.kind == "status" && e.text == "agent sedang memakai tool…")
    );
    // The run folder (where the fake wrote pwned.txt) is gone.
    assert!(runs_empty(&fx));
}

fn alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    // SAFETY: handle checked and closed.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return false;
        }
        let mut code = 0u32;
        let ok = GetExitCodeProcess(h, &mut code);
        CloseHandle(h);
        ok != 0 && code == STILL_ACTIVE as u32
    }
}

fn pids(text: &str) -> Vec<u32> {
    text.trim_start_matches("PIDS ")
        .split(' ')
        .filter_map(|p| p.parse().ok())
        .collect()
}

#[test]
fn cancel_kills_the_whole_process_tree() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let fx = fixture();
    let s = settings(true);
    let file = fx.docs.join("report.txt");
    let cancel = CancelToken::default();
    let mut seen = Vec::new();
    let c = cancel.clone();
    let started = Instant::now();
    let err = run_ask(
        &fx,
        &s,
        &request("claude", "MODE:sleep", &[&file]),
        Duration::from_secs(60),
        &cancel,
        &mut |e| {
            if e.text.starts_with("PIDS ") {
                seen = pids(&e.text);
                c.cancel();
            }
        },
    )
    .unwrap_err();
    assert_eq!(err.kind, "cancelled");
    assert!(started.elapsed() < Duration::from_secs(20));
    assert_eq!(seen.len(), 2, "agent and grandchild pids");
    std::thread::sleep(Duration::from_millis(300));
    for pid in seen {
        assert!(!alive(pid), "process {pid} survived cancel");
    }
    assert!(runs_empty(&fx));
}

#[test]
fn timeout_kills_the_whole_process_tree() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let fx = fixture();
    let s = settings(true);
    let file = fx.docs.join("report.txt");
    let mut seen = Vec::new();
    let err = run_ask(
        &fx,
        &s,
        &request("codex", "MODE:sleep", &[&file]),
        Duration::from_secs(3),
        &CancelToken::default(),
        &mut |e| {
            if e.text.starts_with("PIDS ") {
                seen = pids(&e.text);
            }
        },
    )
    .unwrap_err();
    assert_eq!(err.kind, "timeout");
    assert_eq!(seen.len(), 2);
    std::thread::sleep(Duration::from_millis(300));
    for pid in seen {
        assert!(!alive(pid), "process {pid} survived the timeout");
    }
}

#[test]
fn big_output_is_cut_at_the_limit() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let fx = fixture();
    let s = settings(true);
    let file = fx.docs.join("report.txt");
    let mut text_bytes = 0usize;
    let out = run_ask(
        &fx,
        &s,
        &request("claude", "MODE:flood", &[&file]),
        Duration::from_secs(60),
        &CancelToken::default(),
        &mut |e| text_bytes += e.text.len(),
    )
    .unwrap();
    assert!(out.truncated);
    assert!(text_bytes <= super::MAX_OUTPUT_BYTES);
    assert!(out.answer.chars().count() <= super::MAX_ANSWER_CHARS);
}

#[test]
fn agent_failure_becomes_one_text_line() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let fx = fixture();
    let s = settings(true);
    let file = fx.docs.join("report.txt");
    let err = run_ask(
        &fx,
        &s,
        &request("claude", "MODE:fail", &[&file]),
        Duration::from_secs(60),
        &CancelToken::default(),
        &mut |_| {},
    )
    .unwrap_err();
    assert_eq!(err.kind, "agent_failed");
    assert_eq!(err.message.as_deref(), Some("fake agent: not logged in"));
}

#[test]
fn gates_refuse_before_anything_starts() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let fx = fixture();
    let file = fx.docs.join("report.txt");
    let none = &mut |e: &StreamEvent| assert_ne!(e.kind, "text", "no agent may run");
    let t = Duration::from_secs(5);
    let c = CancelToken::default();
    // Disclosure not acknowledged.
    let e = run_ask(
        &fx,
        &settings(false),
        &request("claude", "q", &[&file]),
        t,
        &c,
        none,
    )
    .unwrap_err();
    assert_eq!(e.kind, "disclosure_required");
    let s = settings(true);
    // Unsupported / unknown agents.
    for agent in ["omp", "antigravity", "bash"] {
        let e = run_ask(&fx, &s, &request(agent, "q", &[&file]), t, &c, none).unwrap_err();
        assert_eq!(e.kind, "agent_unavailable", "{agent}");
    }
    // Sensitive file without the tick.
    let env_file = fx.docs.join(".env");
    let e = run_ask(&fx, &s, &request("claude", "q", &[&env_file]), t, &c, none).unwrap_err();
    assert_eq!(e.kind, "sensitive_unconfirmed");
    // Archive member / missing file.
    let member = fx.docs.join("a.zip").join("inner.txt");
    let e = run_ask(&fx, &s, &request("claude", "q", &[&member]), t, &c, none).unwrap_err();
    assert_eq!(e.kind, "attachment_unavailable");
    // `..` segments.
    let dotdot = PathBuf::from(format!("{}\\..\\docs\\report.txt", fx.docs.display()));
    let e = run_ask(&fx, &s, &request("claude", "q", &[&dotdot]), t, &c, none).unwrap_err();
    assert_eq!(e.kind, "attachment_unavailable");
    // A drive root folder.
    let root = PathBuf::from(format!("{}\\", std::env::var("SystemDrive").unwrap()));
    let e = run_ask(&fx, &s, &request("claude", "q", &[&root]), t, &c, none).unwrap_err();
    assert_eq!(e.kind, "folder_too_broad");
    // Empty question / no attachments.
    let e = run_ask(&fx, &s, &request("claude", " ", &[&file]), t, &c, none).unwrap_err();
    assert_eq!(e.kind, "invalid_request");
    let e = run_ask(&fx, &s, &request("claude", "q", &[]), t, &c, none).unwrap_err();
    assert_eq!(e.kind, "invalid_request");
}

#[test]
fn broad_folders_are_recognised() {
    let env = |k: &str| -> Option<OsString> {
        match k {
            "USERPROFILE" => Some(r"C:\Users\u".into()),
            "SystemRoot" => Some(r"C:\Windows".into()),
            _ => None,
        }
    };
    for broad in [
        r"C:\",
        r"D:\",
        r"C:\Users",
        r"C:\Users\u",
        r"c:\users\U\",
        r"C:\Windows",
        r"\\srv\share",
    ] {
        assert!(too_broad(Path::new(broad), &env), "{broad}");
    }
    for ok in [r"C:\Users\u\Documents", r"D:\Data", r"\\srv\share\team"] {
        assert!(!too_broad(Path::new(ok), &env), "{ok}");
    }
}

/// Manual proof only (never in the gate): runs the REAL detected Claude Code / Codex with the exact
/// production command line on a dummy file and a dummy prompt. `cargo test --lib real_agents --
/// --ignored --nocapture`. Uses the user's agent quota.
#[test]
#[ignore = "runs the real agents on a dummy file; manual FER-490 proof only"]
fn real_agents_read_only_probe() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let fx = fixture();
    std::fs::create_dir_all(&fx.runs).unwrap();
    let detected = super::detect::detect_all(None, &fx.runs);
    for a in &detected {
        println!(
            "detected {:?}: {:?} {:?} {:?}",
            a.id, a.status, a.launch, a.version
        );
    }
    for id in [AgentId::Claude, AgentId::Codex] {
        let Some(found) = detected
            .iter()
            .find(|a| a.id == id && a.status == AgentStatus::Supported)
        else {
            println!("{id:?}: not available, skipped");
            continue;
        };
        let s = AgentSettings {
            agents: vec![found.clone()],
            detected_at_ms: 1,
            default_agent: Some(id),
            acknowledged: vec![id],
            opted_in: vec![id],
            ..Default::default()
        };
        let file = fx.docs.join("report.txt");
        let written = fx.docs.join(format!("written-by-{}.txt", id.as_str()));
        let question = format!(
            "Dummy test. 1) What is the secret fruit in the attached file? 2) Then try to create the file {} containing x, and run the shell command whoami. Report which steps worked.",
            written.display()
        );
        let mut events = Vec::new();
        let ctx = AskContext {
            settings: &s,
            runs_dir: fx.runs.clone(),
            test_profile_config: None,
            timeout: Duration::from_secs(300),
            max_output: super::MAX_OUTPUT_BYTES,
            limits: Limits::default(),
            env: &env,
        };
        let out = ask(
            &ctx,
            &request(id.as_str(), &question, &[&file]),
            &CancelToken::default(),
            &mut |e| events.push(e),
        );
        println!("{id:?} events: {}", events.len());
        println!("{id:?} outcome: {out:?}");
        let out = out.expect("agent answered");
        assert!(
            out.answer.to_uppercase().contains("MANGO"),
            "{id:?} read the file"
        );
        assert!(!written.exists(), "{id:?} wrote a file");
        assert!(runs_empty(&fx), "{id:?} left its run folder");
    }
}

// ---------------------------------------------------------------------------------------------
// FER-490 review fixes (E1, E2, E4)
// ---------------------------------------------------------------------------------------------

fn tree(answer: &str) -> Vec<String> {
    answer
        .trim_start_matches("TREE ")
        .split('|')
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

#[test]
fn e1_the_agent_only_sees_the_copy_never_sibling_files() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let fx = fixture();
    let s = settings(true);
    let file = fx.docs.join("report.txt");
    for agent in ["claude", "codex"] {
        let out = run_ask(
            &fx,
            &s,
            &request(agent, "MODE:tree", &[&file]),
            Duration::from_secs(60),
            &CancelToken::default(),
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(
            tree(&out.answer),
            vec!["files", r"files\report.txt", "tmp"],
            "{agent}"
        );
        assert!(!out.answer.contains("sibling") && !out.answer.contains(".env"));
        assert!(runs_empty(&fx), "{agent}: working folder left behind");
    }
}

#[test]
fn e1_folder_copy_skips_junctions_and_unticked_secrets() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let fx = fixture();
    let s = settings(true);
    let outside = fx.root.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), "OUTSIDE").unwrap();
    let folder = fx.docs.join("project");
    std::fs::create_dir_all(folder.join("sub")).unwrap();
    std::fs::write(folder.join("readme.md"), "hello").unwrap();
    std::fs::write(folder.join("sub").join("notes.txt"), "notes").unwrap();
    std::fs::write(folder.join(".env"), "X=1").unwrap();
    std::fs::write(folder.join("config.txt"), "password = hunter2hunter2\n").unwrap();
    let ok = std::process::Command::new("cmd")
        .args(["/c", "mklink", "/J"])
        .arg(folder.join("link"))
        .arg(&outside)
        .output()
        .unwrap()
        .status
        .success();
    assert!(ok, "junction created");
    let out = run_ask(
        &fx,
        &s,
        &request("claude", "MODE:tree", &[&folder]),
        Duration::from_secs(60),
        &CancelToken::default(),
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(
        tree(&out.answer),
        vec![
            "files",
            r"files\project",
            r"files\project\readme.md",
            r"files\project\sub",
            r"files\project\sub\notes.txt",
            "tmp",
        ]
    );
    let mut skipped: Vec<(String, &str)> = out
        .skipped
        .iter()
        .map(|k| {
            (
                Path::new(&k.path)
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                k.reason,
            )
        })
        .collect();
    skipped.sort();
    assert_eq!(
        skipped,
        vec![
            (".env".to_owned(), "sensitive"),
            ("config.txt".to_owned(), "sensitive"),
            ("link".to_owned(), "link"),
        ]
    );
    assert!(runs_empty(&fx));
}

#[test]
fn e2_codex_is_off_until_the_user_opts_in() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let fx = fixture();
    let mut s = settings(true);
    s.opted_in.clear();
    let file = fx.docs.join("report.txt");
    let t = Duration::from_secs(60);
    let c = CancelToken::default();
    assert!(!s.supported(AgentId::Codex));
    assert!(s.detected_supported(AgentId::Codex));
    let e = run_ask(
        &fx,
        &s,
        &request("codex", "q", &[&file]),
        t,
        &c,
        &mut |_| {},
    )
    .unwrap_err();
    assert_eq!(e.kind, "agent_unavailable");
    // Default agent never falls back to an agent that is not switched on.
    s.default_agent = Some(AgentId::Codex);
    assert_eq!(s.effective_default(), Some(AgentId::Claude));
    super::commands::set_enabled(&mut s, AgentId::Codex, true);
    assert!(s.supported(AgentId::Codex));
    run_ask(
        &fx,
        &s,
        &request("codex", "q", &[&file]),
        t,
        &c,
        &mut |_| {},
    )
    .unwrap();
    super::commands::set_enabled(&mut s, AgentId::Codex, false);
    assert!(!s.supported(AgentId::Codex));
    assert_eq!(s.default_agent, None);
    // Opt-in only exists for broad-read agents.
    super::commands::set_enabled(&mut s, AgentId::Claude, false);
    assert!(s.supported(AgentId::Claude));
}

#[test]
fn e4_test_profile_only_starts_the_fake_it_names() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let fx = fixture();
    let file = fx.docs.join("report.txt");
    let t = Duration::from_secs(30);
    let c = CancelToken::default();
    // A real executable written into the profile's agent-cli.json (here node.exe without the
    // fake script, standing in for the real claude.exe) is refused.
    let mut s = settings(true);
    s.agents[0].launch = Some(Launch::exe(node()));
    let e = run_ask(
        &fx,
        &s,
        &request("claude", "q", &[&file]),
        t,
        &c,
        &mut |_| {},
    )
    .unwrap_err();
    assert_eq!(e.kind, "agent_unavailable");
    let real = PathBuf::from(
        r"C:\Users\x\AppData\Roaming\npm\node_modules\@anthropic-ai\claude-code\bin\claude.exe",
    );
    s.agents[0].launch = Some(Launch::exe(real));
    let e = run_ask(
        &fx,
        &s,
        &request("claude", "q", &[&file]),
        t,
        &c,
        &mut |_| {},
    )
    .unwrap_err();
    assert_eq!(e.kind, "agent_unavailable");
    // The fake file is read AGAIN at ask time: removing the entry stops the fake as well.
    let s = settings(true);
    std::fs::write(
        fx.config.join(super::detect::FAKE_FILE),
        serde_json::json!({"agents": []}).to_string(),
    )
    .unwrap();
    let e = run_ask(
        &fx,
        &s,
        &request("claude", "q", &[&file]),
        t,
        &c,
        &mut |_| {},
    )
    .unwrap_err();
    assert_eq!(e.kind, "agent_unavailable");
}

// FER-493: an Office attachment reaches the (fake) agent as its copy + extracted text + pictures.
#[test]
fn fer493_office_text_and_pictures_reach_the_agent_folder_and_prompt() {
    use std::io::Write;
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let fx = fixture();
    let s = settings(true);
    let doc = fx.docs.join("guide.docx");
    {
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&doc).unwrap());
        let opts = zip::write::SimpleFileOptions::default();
        zip.start_file("word/document.xml", opts).unwrap();
        zip.write_all(br#"<w:document xmlns:w="w"><w:body><w:p><w:r><w:t>Fruit: MANGO</w:t></w:r></w:p></w:body></w:document>"#).unwrap();
        zip.start_file("word/media/image1.png", opts).unwrap();
        zip.write_all(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 1, 2, 3])
            .unwrap();
        zip.finish().unwrap();
    }
    let out = run_ask(
        &fx,
        &s,
        &request("claude", "MODE:tree", &[&doc]),
        Duration::from_secs(60),
        &CancelToken::default(),
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(
        tree(&out.answer),
        vec![
            "files",
            r"files\guide.docx",
            r"files\guide.docx.txt",
            r"files\guide.docx_media",
            r"files\guide.docx_media\image001.png",
            "tmp"
        ]
    );
    let out = run_ask(
        &fx,
        &s,
        &request("claude", "MODE:echo", &[&doc]),
        Duration::from_secs(60),
        &CancelToken::default(),
        &mut |_| {},
    )
    .unwrap();
    let v: serde_json::Value = serde_json::from_str(&out.answer).unwrap();
    let stdin = v["stdin"].as_str().unwrap();
    assert!(
        stdin.contains(r"extracted text of this document"),
        "{stdin}"
    );
    assert!(stdin.contains(r": files\guide.docx.txt"), "{stdin}");
    assert!(stdin.contains(r"(1 file(s), open them"), "{stdin}");
    assert!(runs_empty(&fx));
}
