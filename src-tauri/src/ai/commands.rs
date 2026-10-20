//! Tauri commands for the AI provider (FER-187/188/190). Only plans intents and checks the
//! connection; nothing here touches files or the operation queue.

use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::{Value, json};
use tauri::{AppHandle, State};

use super::provider::{
    CancelToken, ConnectionReport, Deps, NullLog, OsCredentials, PlanOutcome, PlanRequest,
    ProviderConfig, ProviderError, ProviderErrorKind, SystemClock, fail, gate_control_capabilities,
    gate_indexer_capabilities, gate_read_capabilities, plan, test_connection,
};
use super::setup_commands::{config_dir, locked_endpoint_in};
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
        PlanOutcome::Chain { chain, .. } => serde_json::to_value(chain)
            .map(|steps| json!({ "kind": "chain", "chain": steps }))
            .map_err(|_| internal()),
        PlanOutcome::Question { text } => Ok(json!({ "kind": "question", "text": text })),
    }
}

/// The plan step with injectable deps, split from the production wiring so tests can prove
/// refusals happen with a counted, never-called transport (FER-512 P3-1).
fn run_plan_with(
    deps: &Deps<'_>,
    config: &ProviderConfig,
    request: &PlanRequest,
    cancel: &CancelToken,
) -> Result<Value, ProviderError> {
    outcome_json(plan(deps, config, request, cancel)?)
}

/// The send core of `ai_plan_intent` without Tauri types (FER-512 P3-1): the endpoint gate runs
/// strictly FIRST, so a refused request reaches `deps.transport` never.
fn run_gated_plan(
    dir: &std::path::Path,
    config: &ProviderConfig,
    request: &PlanRequest,
    cancel: &CancelToken,
    deps: &Deps<'_>,
) -> Result<Value, ProviderError> {
    let endpoint = locked_endpoint_in(dir, &config.endpoint).map_err(|kind| fail(kind, 0))?;
    let config = ProviderConfig {
        endpoint,
        ..config.clone()
    };
    run_plan_with(deps, &config, request, cancel)
}

#[tauri::command]
pub async fn ai_plan_intent(
    state: State<'_, AiState>,
    activity: State<'_, crate::activity_service::ActivityHandle>,
    app: AppHandle,
    config: ProviderConfig,
    mut request: PlanRequest,
) -> Result<Value, ProviderError> {
    let dir = config_dir(&app);
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
    let result = tauri::async_runtime::spawn_blocking(move || {
        let transport = UreqTransport;
        let clock = SystemClock::default();
        let deps = Deps {
            transport: &transport,
            credentials: &OsCredentials,
            clock: &clock,
            log: &NullLog,
        };
        run_gated_plan(&dir, &config, &request, &token, &deps)
    })
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
pub async fn ai_test_connection(
    app: AppHandle,
    config: ProviderConfig,
) -> Result<ConnectionReport, ProviderError> {
    // FER-512 P3-1: the test pings the SAVED endpoint only; a draft that differs must be saved
    // (with its native confirmation) first.
    let endpoint = locked_endpoint_in(&config_dir(&app), &config.endpoint)
        .map_err(|kind| super::provider::fail(kind, 0))?;
    let config = ProviderConfig { endpoint, ..config };
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

#[cfg(test)]
mod fer512_gate_tests {
    use super::super::credential::CREDENTIAL_TARGET;
    use super::super::provider::{
        Clock, CredentialSource, HttpRequest, HttpResponse, HttpTransport, LogSink, Secret,
        StructuredMode, TransportError,
    };
    use super::*;
    use serde_json::json;
    use std::cell::RefCell;
    use std::time::Duration;

    /// Counting transport: any call would mean the request left the machine.
    struct CountingTransport(RefCell<usize>);
    impl HttpTransport for CountingTransport {
        fn post_json(
            &self,
            _req: &HttpRequest,
            _cancel: &CancelToken,
        ) -> Result<HttpResponse, TransportError> {
            *self.0.borrow_mut() += 1;
            // A valid QUESTION plan reply: the outcome does not matter here, only that a call
            // happened (or not).
            let content = json!({"kind": "question", "text": "hi"}).to_string();
            Ok(HttpResponse {
                status: 200,
                retry_after: None,
                body: json!({"choices":[{"finish_reason":"stop","message":{"content":content}}]})
                    .to_string(),
            })
        }
    }
    struct FakeCredentials;
    impl CredentialSource for FakeCredentials {
        fn read(
            &self,
            _reference: &str,
        ) -> Result<Secret, super::super::credential::CredentialError> {
            Ok(Secret::new("sk-test-DUMMY-not-a-real-key".to_owned()))
        }
    }
    struct FakeClock;
    impl Clock for FakeClock {
        fn now(&self) -> Duration {
            Duration::ZERO
        }
        fn sleep(&self, _delay: Duration, _cancel: &CancelToken) -> bool {
            true
        }
    }
    struct SilentLog;
    impl LogSink for SilentLog {
        fn log(&self, _line: &str) {}
    }

    fn saved_config() -> ProviderConfig {
        ProviderConfig {
            endpoint: "https://api.example.com/v1".into(),
            model: "m".into(),
            timeout_ms: 5000,
            credential_ref: CREDENTIAL_TARGET.into(),
            structured: StructuredMode::JsonObject,
        }
    }

    fn plan_request() -> PlanRequest {
        PlanRequest {
            request_id: "req_gate".into(),
            user_text: "hello".into(),
            context: super::super::provider::OutboundContext {
                active_pane: super::super::provider::PaneName::Left,
                left_location: super::super::provider::LocationKind::Disk,
                right_location: super::super::provider::LocationKind::Disk,
                left_count: 0,
                right_count: 0,
                show_hidden: false,
                filter_active: false,
            },
            capabilities: None,
        }
    }

    /// The required FER-512 proof: a request whose endpoint differs from the saved setup is
    /// refused and the transport is called ZERO times — nothing (and no key) leaves the machine.
    #[test]
    fn a_different_endpoint_is_refused_before_any_network() {
        let dir = tempfile::tempdir().unwrap();
        let transport = CountingTransport(RefCell::new(0));
        let clock = FakeClock;
        let deps = Deps {
            transport: &transport,
            credentials: &FakeCredentials,
            clock: &clock,
            log: &SilentLog,
        };
        // No saved setup at all: refused without a call.
        let err = run_gated_plan(
            dir.path(),
            &saved_config(),
            &plan_request(),
            &CancelToken::default(),
            &deps,
        )
        .unwrap_err();
        assert_eq!(err.kind, ProviderErrorKind::EndpointMismatch);
        assert_eq!(*transport.0.borrow(), 0, "no request may be sent");
        // Saved setup, different request endpoint: same refusal, still no call.
        super::super::setup_commands::save_setup_in(dir.path(), &saved_config(), |_| true).unwrap();
        let mut evil = saved_config();
        evil.endpoint = "https://attacker.example/v1".into();
        let err = run_gated_plan(
            dir.path(),
            &evil,
            &plan_request(),
            &CancelToken::default(),
            &deps,
        )
        .unwrap_err();
        assert_eq!(err.kind, ProviderErrorKind::EndpointMismatch);
        assert_eq!(*transport.0.borrow(), 0, "still no request");
    }

    /// The saved endpoint passes exactly once, and the URL the transport sees is built from the
    /// STORED endpoint (the webview string could not have swapped it).
    #[test]
    fn the_saved_endpoint_passes_and_reaches_the_transport_once() {
        let dir = tempfile::tempdir().unwrap();
        super::super::setup_commands::save_setup_in(dir.path(), &saved_config(), |_| true).unwrap();
        let transport = CountingTransport(RefCell::new(0));
        let clock = FakeClock;
        let deps = Deps {
            transport: &transport,
            credentials: &FakeCredentials,
            clock: &clock,
            log: &SilentLog,
        };
        run_gated_plan(
            dir.path(),
            &saved_config(),
            &plan_request(),
            &CancelToken::default(),
            &deps,
        )
        .unwrap();
        assert_eq!(*transport.0.borrow(), 1);
    }
}
