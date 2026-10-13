//! Tauri commands for the AI provider (FER-187/188/190). Only plans intents and checks the
//! connection; nothing here touches files or the operation queue.

use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::{Value, json};
use tauri::State;

use super::provider::{
    CancelToken, ConnectionReport, Deps, NullLog, OsCredentials, PlanOutcome, PlanRequest,
    ProviderConfig, ProviderError, ProviderErrorKind, SystemClock, gate_control_capabilities,
    gate_indexer_capabilities, gate_read_capabilities, plan, test_connection,
};
use super::transport::UreqTransport;

/// Cancel tokens of in-flight plan requests, keyed by request id.
#[derive(Default)]
pub struct AiState {
    cancels: Mutex<HashMap<String, CancelToken>>,
}

impl AiState {
    pub(super) fn register(&self, request_id: &str) -> Result<CancelToken, ProviderError> {
        let mut cancels = self.cancels.lock().map_err(|_| internal())?;
        let token = CancelToken::default();
        cancels.insert(request_id.to_owned(), token.clone());
        Ok(token)
    }

    pub(super) fn finish(&self, request_id: &str) {
        if let Ok(mut cancels) = self.cancels.lock() {
            cancels.remove(request_id);
        }
    }

    fn cancel(&self, request_id: &str) {
        if let Ok(cancels) = self.cancels.lock()
            && let Some(token) = cancels.get(request_id)
        {
            token.cancel();
        }
    }
}

fn internal() -> ProviderError {
    ProviderError {
        kind: ProviderErrorKind::Network,
        attempts: 0,
    }
}

/// Wire form read by `RemoteIntentProvider`: the intent envelope itself, or `{kind:'question',text}`.
fn outcome_json(outcome: PlanOutcome) -> Result<Value, ProviderError> {
    match outcome {
        PlanOutcome::Intent { envelope, .. } => {
            serde_json::to_value(envelope).map_err(|_| internal())
        }
        PlanOutcome::Question { text } => Ok(json!({ "kind": "question", "text": text })),
    }
}

fn run_plan(
    config: &ProviderConfig,
    request: &PlanRequest,
    cancel: &CancelToken,
) -> Result<Value, ProviderError> {
    let transport = UreqTransport;
    let clock = SystemClock::default();
    let deps = Deps {
        transport: &transport,
        credentials: &OsCredentials,
        clock: &clock,
        log: &NullLog,
    };
    outcome_json(plan(&deps, config, request, cancel)?)
}

#[tauri::command]
pub async fn ai_plan_intent(
    state: State<'_, AiState>,
    activity: State<'_, crate::activity_service::ActivityHandle>,
    config: ProviderConfig,
    mut request: PlanRequest,
) -> Result<Value, ProviderError> {
    // ACT-08: the read capability is only offered while the activity store is available and
    // (FER-432) its first-run disclosure was acknowledged.
    gate_read_capabilities(
        &mut request,
        crate::activity_service::chat_offer_ready(activity.0.as_ref()),
    );
    // FER-471: the control capability is only offered while a copy job is running.
    gate_control_capabilities(&mut request, crate::copy_pool::has_running());
    // FER-476: the FerrumIndexer capabilities exist on the Windows desktop app only.
    gate_indexer_capabilities(&mut request, cfg!(windows));
    let token = state.register(&request.request_id)?;
    let id = request.request_id.clone();
    let result = tauri::async_runtime::spawn_blocking(move || run_plan(&config, &request, &token))
        .await
        .unwrap_or_else(|_| Err(internal()));
    state.finish(&id);
    result
}

#[tauri::command]
pub fn ai_cancel_plan(state: State<'_, AiState>, request_id: String) {
    state.cancel(&request_id);
}

#[tauri::command]
pub async fn ai_test_connection(config: ProviderConfig) -> Result<ConnectionReport, ProviderError> {
    tauri::async_runtime::spawn_blocking(move || {
        let transport = UreqTransport;
        let clock = SystemClock::default();
        let deps = Deps {
            transport: &transport,
            credentials: &OsCredentials,
            clock: &clock,
            log: &NullLog,
        };
        test_connection(&deps, &config, &CancelToken::default())
    })
    .await
    .map_err(|_| internal())
}
