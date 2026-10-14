//! FER-486 Tauri commands for "Add to chat". `ai_attachment_inspect` returns metadata and the
//! sensitive flag only (never content). `ai_ask` reads the attachments again, enforces the
//! sensitive tick, sends the question through the answer-only path and returns the answer text
//! plus a content-free summary of what was sent. Cancel with `ai_cancel_plan(requestId)`.

use tauri::{AppHandle, State};

use super::ask::{AskError, AskOutcome, AskRequest, ask, check_sensitive, sent_summary};
use super::attach::{self, InspectResult};
use super::commands::AiState;
use super::provider::{
    CancelToken, Deps, NullLog, OsCredentials, ProviderConfig, ProviderErrorKind, SystemClock,
};
use super::setup_commands::{config_dir, locked_endpoint_in};
use super::transport::UreqTransport;
use crate::domain::FsError;

/// FER-493 O4: with a `requestId`, path `i` can be cancelled with `ai_cancel_plan("<id>:<i>")`
/// (the chip was removed): its Office extraction stops at the next check.
#[tauri::command]
pub async fn ai_attachment_inspect(
    state: State<'_, AiState>,
    paths: Vec<String>,
    request_id: Option<String>,
) -> Result<Vec<InspectResult>, FsError> {
    let ids: Vec<String> = match &request_id {
        Some(id) if paths.len() <= attach::MAX_ATTACHMENTS && id.len() <= 128 => {
            (0..paths.len()).map(|i| format!("{id}:{i}")).collect()
        }
        _ => Vec::new(),
    };
    // All or nothing, so flag `i` always belongs to path `i`.
    let tokens: Vec<CancelToken> = ids
        .iter()
        .map(|id| state.register(id))
        .collect::<Result<_, _>>()
        .unwrap_or_default();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let stops: Vec<&std::sync::atomic::AtomicBool> =
            tokens.iter().map(CancelToken::flag).collect();
        attach::inspect_cancellable(&paths, &stops)
    })
    .await
    .map_err(|e| FsError::new(crate::domain::ErrorCode::Io, e.to_string()));
    for id in &ids {
        state.finish(id);
    }
    result?
}

fn run_ask(
    config: &ProviderConfig,
    request: &AskRequest,
    cancel: &CancelToken,
) -> Result<AskOutcome, AskError> {
    // Validate before touching any file.
    config.validate().map_err(|k| AskError::provider(k, 0))?;
    request.validate().map_err(|k| AskError::provider(k, 0))?;
    let items = attach::extract(&request.attachments);
    check_sensitive(&request.attachments, &items)?;
    if cancel.is_cancelled() {
        return Err(AskError::provider(ProviderErrorKind::Cancelled, 0));
    }
    let transport = UreqTransport;
    let clock = SystemClock::default();
    let deps = Deps {
        transport: &transport,
        credentials: &OsCredentials,
        clock: &clock,
        log: &NullLog,
    };
    let answer = ask(&deps, config, request, &items, cancel)?;
    Ok(AskOutcome {
        answer,
        attachments: sent_summary(&items),
    })
}

#[tauri::command]
pub async fn ai_ask(
    state: State<'_, AiState>,
    app: AppHandle,
    config: ProviderConfig,
    request: AskRequest,
) -> Result<AskOutcome, AskError> {
    // FER-512 P3-1: the question (and with it the attachment content and the API key) only ever
    // goes to the SAVED endpoint. A different one is refused before anything is read or sent.
    let endpoint = locked_endpoint_in(&config_dir(&app), &config.endpoint)
        .map_err(|k| AskError::provider(k, 0))?;
    let config = ProviderConfig { endpoint, ..config };
    let token = state
        .register(&request.request_id)
        .map_err(|e| AskError::provider(e.kind, 0))?;
    let id = request.request_id.clone();
    let result = tauri::async_runtime::spawn_blocking(move || run_ask(&config, &request, &token))
        .await
        .unwrap_or_else(|_| Err(AskError::provider(ProviderErrorKind::Network, 0)));
    state.finish(&id);
    result
}
