//! Tauri commands of "Ask AI agent" and the request flow behind `agent_ask`.
//!
//! The webview names an agent id and attachment paths; it can never choose a program, a flag or
//! a folder the agent may read. Those come from the detection result ([`super::settings`]) and
//! the attachment checks below. Cancel goes through `ai_cancel_plan(requestId)` like `ai_ask`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};

use super::args::{PromptTurn, agent_args, build_prompt, clean_env};
use super::detect::{self, AgentStatus, DetectedAgent, launch_ok};
use super::parse::{AgentEvent, StreamParser, short_line};
use super::runner::{RunEnd, RunSpec, run};
use super::settings::{self, AgentSettings};
use super::snapshot::{self, Limits, Skipped, Snapshot};
use super::{AgentId, DEFAULT_TIMEOUT_SECS, MAX_OUTPUT_BYTES};
use crate::ai::ask::{AskRequest, AskRole, AskTurn};
use crate::ai::attach::{self, AttachmentRef};
use crate::ai::commands::AiState;
use crate::ai::provider::CancelToken;

/// Event name of the answer stream.
pub const EVENT: &str = "agent_cli://event";
/// Folder (inside the data dir) holding one fresh working folder per question.
pub const RUNS_DIR: &str = "agent-runs";

/// Serialises detection and settings writes.
static SETTINGS_LOCK: Mutex<()> = Mutex::new(());

// ---------------------------------------------------------------------------------------------
// Settings view
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentView {
    pub id: AgentId,
    pub name: &'static str,
    pub vendor: &'static str,
    pub status: AgentStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Read-only mode Ferrum forces (supported agents only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<&'static str>,
    pub acknowledged: bool,
    /// FER-490 E2: the agent can read other files on this PC; OFF until `enabled`.
    pub broad_read: bool,
    /// Usable as a chat target now (supported and, for broad read, opted in).
    pub enabled: bool,
    /// FER-494: can be opened interactively in a terminal ("Buka di terminal").
    pub terminal: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_note: Option<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCliView {
    pub agents: Vec<AgentView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_agent: Option<AgentId>,
    pub detected_at_ms: i64,
    pub test_profile: bool,
    /// FER-494: agent of the composer's "Buka di terminal" button.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_default: Option<AgentId>,
}

fn view(s: &AgentSettings) -> AgentCliView {
    AgentCliView {
        agents: s
            .agents
            .iter()
            .map(|a| AgentView {
                id: a.id,
                name: a.id.display_name(),
                vendor: a.id.vendor(),
                status: a.status,
                path: a.launch.as_ref().map(|l| l.display()),
                version: a.version.clone(),
                reason: a.reason.clone(),
                mode: a
                    .id
                    .read_only_mode()
                    .ok()
                    .filter(|_| a.status == AgentStatus::Supported),
                acknowledged: s.acknowledged.contains(&a.id),
                broad_read: a.id.broad_read() && a.status == AgentStatus::Supported,
                enabled: s.supported(a.id),
                terminal: s.terminal_ready(a.id),
                terminal_reason: a.terminal_reason.clone().filter(|_| !a.terminal_ready),
                terminal_note: a.id.def().terminal_note.filter(|_| a.terminal_ready),
            })
            .collect(),
        default_agent: s.effective_default(),
        detected_at_ms: s.detected_at_ms,
        test_profile: crate::profile::is_active(),
        terminal_default: s.effective_terminal_default(),
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn dirs(app: &AppHandle) -> Result<(PathBuf, PathBuf), AgentError> {
    let config =
        crate::profile::config_dir(app).ok_or_else(|| AgentError::new("storage_failure"))?;
    let data = crate::profile::data_dir(app).ok_or_else(|| AgentError::new("storage_failure"))?;
    Ok((config, data))
}

fn detect_into(config: &Path, data: &Path, keep: Option<AgentSettings>) -> AgentSettings {
    let scratch = data.join(RUNS_DIR);
    let _ = std::fs::create_dir_all(&scratch);
    let fake_config = crate::profile::is_active().then_some(config);
    let agents: Vec<DetectedAgent> = detect::detect_all(fake_config, &scratch);
    let keep = keep.unwrap_or_default();
    AgentSettings {
        agents,
        detected_at_ms: now_ms(),
        default_agent: keep.default_agent,
        acknowledged: keep.acknowledged,
        opted_in: keep.opted_in,
        terminal_default: keep.terminal_default,
        schema: settings::SCHEMA,
    }
}

/// Stored settings; the first call detects once and stores the result.
fn load_or_detect(config: &Path, data: &Path) -> AgentSettings {
    let _guard = SETTINGS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let stored = settings::load_from(config);
    if let Some(s) = &stored
        && s.schema >= settings::SCHEMA
    {
        return s.clone();
    }
    // FER-494: an older detection lacks the new agents and the terminal check.
    let s = detect_into(config, data, stored);
    let _ = settings::save_to(config, &s);
    s
}

fn update(
    config: &Path,
    data: &Path,
    f: impl FnOnce(&mut AgentSettings),
) -> Result<AgentSettings, AgentError> {
    let mut s = load_or_detect(config, data);
    let _guard = SETTINGS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut s);
    settings::save_to(config, &s).map_err(|_| AgentError::new("storage_failure"))?;
    Ok(s)
}

fn parse_agent(id: &str) -> Result<AgentId, AgentError> {
    AgentId::parse(id).ok_or_else(|| AgentError::new("agent_unavailable"))
}

#[tauri::command]
pub async fn agent_cli_status(app: AppHandle) -> Result<AgentCliView, AgentError> {
    let (config, data) = dirs(&app)?;
    tauri::async_runtime::spawn_blocking(move || view(&load_or_detect(&config, &data)))
        .await
        .map_err(|_| AgentError::new("backend_failure"))
}

#[tauri::command]
pub async fn agent_cli_detect(app: AppHandle) -> Result<AgentCliView, AgentError> {
    let (config, data) = dirs(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        let keep = settings::load_from(&config);
        let s = detect_into(&config, &data, keep);
        let _guard = SETTINGS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        settings::save_to(&config, &s).map_err(|_| AgentError::new("storage_failure"))?;
        Ok(view(&s))
    })
    .await
    .map_err(|_| AgentError::new("backend_failure"))?
}

#[tauri::command]
pub fn agent_cli_set_default(app: AppHandle, agent: String) -> Result<AgentCliView, AgentError> {
    let id = parse_agent(&agent)?;
    let (config, data) = dirs(&app)?;
    let s = update(&config, &data, |s| {
        if s.supported(id) {
            s.default_agent = Some(id);
        }
    })?;
    Ok(view(&s))
}

/// FER-494: agent of the composer's "Buka di terminal" button.
#[tauri::command]
pub fn agent_cli_set_terminal_default(
    app: AppHandle,
    agent: String,
) -> Result<AgentCliView, AgentError> {
    let id = parse_agent(&agent)?;
    let (config, data) = dirs(&app)?;
    let s = update(&config, &data, |s| {
        if s.terminal_ready(id) {
            s.terminal_default = Some(id);
        }
    })?;
    Ok(view(&s))
}

/// FER-494: open an agent interactively in a visible terminal at a folder (a file means its
/// parent folder). See [`super::terminal`] for the launch rules.
#[tauri::command]
pub async fn agent_terminal_open(
    app: AppHandle,
    window: tauri::Window,
    request: super::terminal::TerminalOpenRequest,
) -> Result<super::terminal::TerminalOpened, AgentError> {
    let (config, data) = dirs(&app)?;
    let owner = owner_hwnd(&window);
    tauri::async_runtime::spawn_blocking(move || {
        let settings = load_or_detect(&config, &data);
        let test_profile = crate::profile::is_active();
        let trusted = |l: &super::args::Launch| detect::launch_trusted(l);
        let ctx = super::terminal::OpenContext {
            settings: &settings,
            test_profile_config: test_profile.then(|| config.clone()),
            dry_run_log: test_profile.then(|| data.join(super::terminal::DRY_RUN_LOG)),
            trusted: &trusted,
            folder_owner: &|p: &Path| super::acl::owner_trusted(p),
            wt: crate::shell_open::resolve_wt(),
            spawn: &super::terminal::spawn_first,
            confirm: &|caption: &str, text: &str| {
                crate::native_dialog::confirm_or_busy(owner, caption, text)
            },
        };
        super::terminal::open(&ctx, &request)
    })
    .await
    .unwrap_or_else(|_| Err(AgentError::new("backend_failure")))
}

#[cfg(windows)]
fn owner_hwnd(window: &tauri::Window) -> isize {
    window.hwnd().map(|h| h.0 as isize).unwrap_or(0)
}

#[cfg(not(windows))]
fn owner_hwnd(_window: &tauri::Window) -> isize {
    0
}

/// The user read the disclosure for this agent ("isi file dikirim ke <vendor>").
#[tauri::command]
pub fn agent_cli_acknowledge(app: AppHandle, agent: String) -> Result<AgentCliView, AgentError> {
    let id = parse_agent(&agent)?;
    let (config, data) = dirs(&app)?;
    let s = update(&config, &data, |s| {
        if s.supported(id) && !s.acknowledged.contains(&id) {
            s.acknowledged.push(id);
        }
    })?;
    Ok(view(&s))
}

/// FER-490 E2: explicit opt-in (or opt-out) for a broad-read agent (Codex). Stored in the backend;
/// switching it off also drops the agent as default.
#[tauri::command]
pub fn agent_cli_set_enabled(
    app: AppHandle,
    agent: String,
    enabled: bool,
) -> Result<AgentCliView, AgentError> {
    let id = parse_agent(&agent)?;
    let (config, data) = dirs(&app)?;
    let s = update(&config, &data, |s| set_enabled(s, id, enabled))?;
    Ok(view(&s))
}

pub fn set_enabled(s: &mut AgentSettings, id: AgentId, enabled: bool) {
    if !id.broad_read() {
        return;
    }
    s.opted_in.retain(|a| *a != id);
    if enabled && s.detected_supported(id) {
        s.opted_in.push(id);
    }
    if !enabled && s.default_agent == Some(id) {
        s.default_agent = None;
    }
}

// ---------------------------------------------------------------------------------------------
// Ask
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentAskRequest {
    pub request_id: String,
    pub agent: String,
    pub question: String,
    #[serde(default)]
    pub history: Vec<AskTurn>,
    pub attachments: Vec<AttachmentRef>,
}

/// Error crossing IPC. `message` is one short plain-text line from the agent (shown as text).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentError {
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl AgentError {
    pub fn new(kind: &str) -> Self {
        Self {
            kind: kind.to_owned(),
            path: None,
            message: None,
        }
    }
    pub(super) fn at(kind: &str, path: &str) -> Self {
        Self {
            path: Some(path.to_owned()),
            ..Self::new(kind)
        }
    }
    pub(super) fn with_message(kind: &str, message: String) -> Self {
        Self {
            message: (!message.is_empty()).then_some(message),
            ..Self::new(kind)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentOutcome {
    pub agent: AgentId,
    pub answer: String,
    /// The agent produced more than the output limit; the answer is what arrived before it.
    pub truncated: bool,
    /// FER-490 E1: items inside attached folders that were NOT copied for the agent.
    pub skipped: Vec<Skipped>,
}

/// Streamed piece of the answer (`agent_cli://event`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamEvent {
    pub request_id: String,
    /// `text` (a piece of the answer) or `status` (fixed app text).
    pub kind: &'static str,
    pub text: String,
}

/// Everything `ask` needs besides the request (injected by tests).
pub struct AskContext<'a> {
    pub settings: &'a AgentSettings,
    pub runs_dir: PathBuf,
    /// FER-490 E4: the test profile's config dir. When set, the launch must equal the entry of
    /// `agent-cli-test.json` read again now; nothing else (e.g. a real `claude.exe` written into
    /// the profile's `agent-cli.json`) is started.
    pub test_profile_config: Option<PathBuf>,
    pub timeout: Duration,
    pub max_output: usize,
    pub limits: Limits,
    pub env: &'a dyn Fn(&str) -> Option<OsString>,
}

fn same_dir(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| {
        p.to_string_lossy()
            .trim_end_matches('\\')
            .to_ascii_lowercase()
    };
    norm(a) == norm(b)
}

/// A folder that must never be attached for an agent: a drive or share root, the user profile
/// root (holds `.ssh`, browser data, …) or its parent, the Windows folder.
pub fn too_broad(dir: &Path, env: &dyn Fn(&str) -> Option<OsString>) -> bool {
    if dir.parent().is_none() {
        return true;
    }
    let mut blocked: Vec<PathBuf> = Vec::new();
    if let Some(home) = env("USERPROFILE").map(PathBuf::from) {
        if let Some(parent) = home.parent() {
            blocked.push(parent.to_path_buf());
        }
        blocked.push(home);
    }
    if let Some(sys) = env("SystemRoot") {
        blocked.push(PathBuf::from(sys));
    }
    blocked.iter().any(|b| same_dir(b, dir))
}

/// No `.`/`..` segments and no segment ending in a dot or space (Windows would rewrite them).
fn plain_segments(path: &str) -> bool {
    path.split('\\')
        .skip(1)
        .all(|seg| seg != "." && seg != ".." && !seg.ends_with('.') && !seg.ends_with(' '))
}

/// String-level checks of one attachment; returns the normalised path the snapshot opens.
/// Type checks (link, placeholder, sensitive) happen in [`snapshot::take`] on the handle.
pub fn gate_attachment(
    r: &AttachmentRef,
    env: &dyn Fn(&str) -> Option<OsString>,
) -> Result<String, AgentError> {
    let raw = r.path.as_str();
    let path = crate::path_guard::check_str(raw)
        .map_err(|_| AgentError::at("attachment_unavailable", raw))?;
    // "C:\" stays as is ("C:" would mean the drive's current folder); it is refused below.
    let path = if path.len() > 3 {
        path.trim_end_matches('\\').to_owned()
    } else {
        path
    };
    if !plain_segments(&path) {
        return Err(AgentError::at("attachment_unavailable", raw));
    }
    let as_path = Path::new(&path);
    if as_path.is_dir() {
        if too_broad(as_path, env) {
            return Err(AgentError::at("folder_too_broad", raw));
        }
        let name = as_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if attach::sensitive_name(&name) && !r.confirmed_sensitive {
            return Err(AgentError::at("sensitive_unconfirmed", raw));
        }
    }
    Ok(path)
}

/// The launch that may run now, or why not.
fn checked_launch<'s>(
    ctx: &'s AskContext<'_>,
    agent: AgentId,
) -> Result<&'s super::args::Launch, AgentError> {
    if !ctx.settings.supported(agent) {
        return Err(AgentError::new("agent_unavailable"));
    }
    let launch = ctx
        .settings
        .agent(agent)
        .and_then(|a| a.launch.as_ref())
        .ok_or_else(|| AgentError::new("agent_unavailable"))?;
    let is_file = |p: &Path| p.is_file();
    match &ctx.test_profile_config {
        Some(config) => {
            // E4: exactly the fake named by the profile, read again now.
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
            // E3: re-checked before every start (the folder's ACL can change).
            detect::launch_trusted(launch)
                .map_err(|reason| AgentError::with_message("agent_untrusted", reason))?;
        }
    }
    Ok(launch)
}

fn random_hex() -> String {
    let mut b = [0u8; 8];
    let _ = getrandom::fill(&mut b);
    hex::encode(b)
}

/// Remove working folders left by an earlier crash (older than an hour). Best effort.
fn sweep_runs(runs_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(runs_dir) else {
        return;
    };
    for e in entries.flatten() {
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > Duration::from_secs(3600));
        if old {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

fn remove_dir(dir: &Path) {
    for _ in 0..10 {
        if std::fs::remove_dir_all(dir).is_ok() || !dir.exists() {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Whole request: validate, check attachments, copy them into a fresh working folder, run the
/// agent there, return text. The working folder (with the copies) is always removed.
pub fn ask(
    ctx: &AskContext<'_>,
    request: &AgentAskRequest,
    cancel: &CancelToken,
    emit: &mut dyn FnMut(StreamEvent),
) -> Result<AgentOutcome, AgentError> {
    AskRequest {
        request_id: request.request_id.clone(),
        question: request.question.clone(),
        history: request.history.clone(),
        attachments: request.attachments.clone(),
    }
    .validate()
    .map_err(|_| AgentError::new("invalid_request"))?;
    let agent = parse_agent(&request.agent)?;
    let launch = checked_launch(ctx, agent)?;
    if !ctx.settings.acknowledged.contains(&agent) {
        return Err(AgentError::new("disclosure_required"));
    }
    let mut sources = Vec::new();
    for r in &request.attachments {
        sources.push((r.clone(), gate_attachment(r, ctx.env)?));
    }
    if cancel.is_cancelled() {
        return Err(AgentError::new("cancelled"));
    }

    let _ = std::fs::create_dir_all(&ctx.runs_dir);
    sweep_runs(&ctx.runs_dir);
    let workdir = ctx
        .runs_dir
        .join(format!("{}-{}", request.request_id, random_hex()));
    let temp = workdir.join("tmp");
    let result = std::fs::create_dir_all(&temp)
        .map_err(|_| AgentError::new("storage_failure"))
        .and_then(|()| {
            emit(StreamEvent {
                request_id: request.request_id.clone(),
                kind: "status",
                text: "menyalin lampiran untuk agent…".to_owned(),
            });
            snapshot::take(&sources, &workdir, ctx.limits)
        })
        .and_then(|snap| {
            if cancel.is_cancelled() {
                return Err(AgentError::new("cancelled"));
            }
            run_in(
                ctx, agent, launch, request, &snap, &workdir, &temp, cancel, emit,
            )
        });
    remove_dir(&workdir);
    result
}

#[allow(clippy::too_many_arguments)]
fn run_in(
    ctx: &AskContext<'_>,
    agent: AgentId,
    launch: &super::args::Launch,
    request: &AgentAskRequest,
    snap: &Snapshot,
    workdir: &Path,
    temp: &Path,
    cancel: &CancelToken,
    emit: &mut dyn FnMut(StreamEvent),
) -> Result<AgentOutcome, AgentError> {
    let args =
        agent_args(agent, launch, workdir).ok_or_else(|| AgentError::new("agent_unavailable"))?;
    let history: Vec<PromptTurn> = request
        .history
        .iter()
        .map(|t| PromptTurn {
            user: t.role == AskRole::User,
            text: t.text.clone(),
        })
        .collect();
    let prompt = build_prompt(&request.question, &history, &snap.items);
    let spec = RunSpec {
        program: launch.program.clone(),
        args,
        env: clean_env(agent, ctx.env, temp),
        cwd: workdir.to_path_buf(),
        stdin: prompt.into_bytes(),
        timeout: ctx.timeout,
        max_output: ctx.max_output,
    };
    let mut parser = StreamParser::new(agent);
    let id = request.request_id.clone();
    emit(StreamEvent {
        request_id: id.clone(),
        kind: "status",
        text: "agent sedang membaca…".to_owned(),
    });
    let result = run(&spec, &|| cancel.is_cancelled(), &mut |line| {
        for ev in parser.line(line) {
            let (kind, text) = match ev {
                AgentEvent::Text(t) => ("text", t),
                AgentEvent::Status(s) => ("status", format!("agent sedang {s}…")),
            };
            emit(StreamEvent {
                request_id: id.clone(),
                kind,
                text,
            });
        }
    })
    .map_err(|_| AgentError::new("agent_start_failed"))?;
    let answer = parser.answer();
    match result.end {
        RunEnd::Cancelled => Err(AgentError::new("cancelled")),
        RunEnd::TimedOut => Err(AgentError::new("timeout")),
        RunEnd::OutputLimit => Ok(AgentOutcome {
            agent,
            answer,
            truncated: true,
            skipped: snap.skipped.clone(),
        }),
        RunEnd::Exited(code) => {
            if let Some(err) = parser.error() {
                return Err(AgentError::with_message("agent_failed", err));
            }
            if code != Some(0) {
                return Err(AgentError::with_message(
                    "agent_failed",
                    short_line(&result.stderr_tail),
                ));
            }
            if answer.trim().is_empty() {
                return Err(AgentError::new("empty_answer"));
            }
            Ok(AgentOutcome {
                agent,
                answer,
                truncated: false,
                skipped: snap.skipped.clone(),
            })
        }
    }
}

#[tauri::command]
pub async fn agent_ask(
    app: AppHandle,
    state: State<'_, AiState>,
    request: AgentAskRequest,
) -> Result<AgentOutcome, AgentError> {
    let (config, data) = dirs(&app)?;
    let token = state
        .register(&request.request_id)
        .map_err(|_| AgentError::new("backend_failure"))?;
    let id = request.request_id.clone();
    let emitter = app.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let settings = load_or_detect(&config, &data);
        let env = |k: &str| std::env::var_os(k);
        let ctx = AskContext {
            settings: &settings,
            runs_dir: data.join(RUNS_DIR),
            test_profile_config: crate::profile::is_active().then(|| config.clone()),
            timeout: Duration::from_secs(DEFAULT_TIMEOUT_SECS),
            max_output: MAX_OUTPUT_BYTES,
            limits: Limits::default(),
            env: &env,
        };
        ask(&ctx, &request, &token, &mut |ev| {
            let _ = emitter.emit(EVENT, ev);
        })
    })
    .await
    .unwrap_or_else(|_| Err(AgentError::new("backend_failure")));
    state.finish(&id);
    result
}
