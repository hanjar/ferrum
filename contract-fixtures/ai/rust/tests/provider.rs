// Provider tests use fake transport/credentials/clock only: no network, no real secret.
#[allow(dead_code)]
mod ai {
    #[path = "../../../../../src-tauri/src/ai/contract.rs"]
    pub mod contract;
    #[path = "../../../../../src-tauri/src/ai/credential.rs"]
    pub mod credential;
    #[path = "../../../../../src-tauri/src/ai/provider.rs"]
    pub mod provider;
}

use ai::contract::Intent;
use ai::credential::CredentialError;
use ai::provider::*;
use serde_json::{json, Value};
use std::{cell::RefCell, collections::VecDeque, time::Duration};

const SECRET: &str = "sk-test-DUMMY-0123456789";

struct FakeTransport {
    responses: RefCell<VecDeque<Result<HttpResponse, TransportError>>>,
    seen: RefCell<Vec<HttpRequest>>,
    cancel_after_call: RefCell<Option<CancelToken>>,
}
impl FakeTransport {
    fn new(items: Vec<Result<HttpResponse, TransportError>>) -> Self {
        Self {
            responses: RefCell::new(items.into()),
            seen: RefCell::default(),
            cancel_after_call: RefCell::default(),
        }
    }
}
impl HttpTransport for FakeTransport {
    fn post_json(
        &self,
        req: &HttpRequest,
        _c: &CancelToken,
    ) -> Result<HttpResponse, TransportError> {
        self.seen.borrow_mut().push(req.clone());
        if let Some(t) = self.cancel_after_call.borrow().as_ref() {
            t.cancel();
        }
        self.responses
            .borrow_mut()
            .pop_front()
            .expect("unexpected extra request")
    }
}
struct FakeCreds(Result<&'static str, CredentialError>);
impl CredentialSource for FakeCreds {
    fn read(&self, _r: &str) -> Result<Secret, CredentialError> {
        self.0.map(|s| Secret::new(s.to_owned()))
    }
}
#[derive(Default)]
struct FakeClock {
    now: RefCell<Duration>,
    sleeps: RefCell<Vec<Duration>>,
    cancel_on_sleep: RefCell<Option<CancelToken>>,
}
impl Clock for FakeClock {
    fn now(&self) -> Duration {
        *self.now.borrow()
    }
    fn sleep(&self, d: Duration, c: &CancelToken) -> bool {
        self.sleeps.borrow_mut().push(d);
        *self.now.borrow_mut() += d;
        if let Some(t) = self.cancel_on_sleep.borrow().as_ref() {
            t.cancel();
        }
        !c.is_cancelled()
    }
}
#[derive(Default)]
struct VecLog(RefCell<Vec<String>>);
impl LogSink for VecLog {
    fn log(&self, l: &str) {
        self.0.borrow_mut().push(l.to_owned());
    }
}

fn config() -> ProviderConfig {
    ProviderConfig {
        endpoint: "https://api.example.test/api/paas/v4".into(),
        model: "configured-model".into(),
        timeout_ms: 30_000,
        credential_ref: "Ferrum/AI/custom".into(),
        structured: StructuredMode::JsonObject,
    }
}
fn request() -> PlanRequest {
    PlanRequest {
        request_id: "req_1".into(),
        user_text: "Sync both panes".into(),
        capabilities: None,
        context: OutboundContext {
            active_pane: PaneName::Left,
            left_location: LocationKind::Disk,
            right_location: LocationKind::Disk,
            left_count: 3,
            right_count: 4,
            show_hidden: false,
            filter_active: false,
        },
    }
}
fn resp(status: u16, body: Value) -> HttpResponse {
    HttpResponse {
        status,
        retry_after: None,
        body: body.to_string(),
    }
}
fn ok(content: &str) -> Result<HttpResponse, TransportError> {
    Ok(resp(
        200,
        json!({"choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":content}}]}),
    ))
}
fn ok_raw(body: &str) -> Result<HttpResponse, TransportError> {
    Ok(HttpResponse {
        status: 200,
        retry_after: None,
        body: body.to_owned(),
    })
}
fn err(status: u16, body: Value) -> Result<HttpResponse, TransportError> {
    Ok(resp(status, body))
}
fn sync_json() -> String {
    json!({"schemaVersion":1,"requestId":"req_1","intent":{"capability":"directories.sync",
        "leftPane":"left","rightPane":"right","mode":"bidirectional"}})
    .to_string()
}

struct Rig {
    t: FakeTransport,
    c: FakeClock,
    l: VecLog,
    creds: FakeCreds,
}
impl Rig {
    fn new(items: Vec<Result<HttpResponse, TransportError>>) -> Self {
        Self {
            t: FakeTransport::new(items),
            c: FakeClock::default(),
            l: VecLog::default(),
            creds: FakeCreds(Ok(SECRET)),
        }
    }
    fn deps(&self) -> Deps<'_> {
        Deps {
            transport: &self.t,
            credentials: &self.creds,
            clock: &self.c,
            log: &self.l,
        }
    }
    fn plan(&self, cancel: &CancelToken) -> Result<PlanOutcome, ProviderError> {
        plan(&self.deps(), &config(), &request(), cancel)
    }
    fn calls(&self) -> usize {
        self.t.seen.borrow().len()
    }
}
fn kind(r: Result<PlanOutcome, ProviderError>) -> ProviderErrorKind {
    r.expect_err("expected error").kind
}

#[test]
fn endpoint_policy_allows_https_and_explicit_localhost_only() {
    for e in [
        "https://api.example.test/api/paas/v4",
        "https://router.example/v1",
        "http://localhost:20128/v1",
        "http://127.0.0.1:1234/v1",
    ] {
        assert!(validate_endpoint(e).is_ok(), "{e}");
    }
    for e in [
        "http://router.test/v1",
        "http://localhost/v1",
        "http://127.0.0.1/v1",
        "http://localhost.evil.test:80/v1",
        "https://user:pw@router.example/v1",
        "https://router.example/v1?x=1",
        "https://router.example/v1#f",
        "https://router.example",
        "https://router.example/",
        "https://router.example/v1/../x",
        "ftp://router.example/v1",
        "http://[::1]:20128/v1",
        "",
        "https://router.example/v1 ",
        "https://rou ter.example/v1",
    ] {
        assert_eq!(
            validate_endpoint(e),
            Err(ProviderErrorKind::UnsafeEndpoint),
            "{e:?}"
        );
    }
}

#[test]
fn invalid_config_is_rejected_before_any_credential_or_network_use() {
    let mutators: Vec<fn(&mut ProviderConfig)> = vec![
        |c| c.model = "".into(),
        |c| c.model = "bad model!".into(),
        |c| c.timeout_ms = 10,
        |c| c.timeout_ms = 500_000,
        |c| c.credential_ref = "other".into(),
    ];
    for mutate in mutators {
        let mut c = config();
        mutate(&mut c);
        assert_eq!(c.validate(), Err(ProviderErrorKind::InvalidConfig));
    }
    let mut c = config();
    c.endpoint = "http://router.test/v1".into();
    assert_eq!(c.validate(), Err(ProviderErrorKind::UnsafeEndpoint));
    let rig = Rig::new(vec![]);
    assert_eq!(
        plan(&rig.deps(), &c, &request(), &CancelToken::default())
            .unwrap_err()
            .kind,
        ProviderErrorKind::UnsafeEndpoint
    );
    assert_eq!(rig.calls(), 0);
}

#[test]
fn builds_minimal_chat_request_without_secret_or_file_data() {
    let rig = Rig::new(vec![ok(&sync_json())]);
    assert!(matches!(
        rig.plan(&CancelToken::default()),
        Ok(PlanOutcome::Intent { .. })
    ));
    let seen = rig.t.seen.borrow();
    let req = &seen[0];
    assert_eq!(
        req.url,
        "https://api.example.test/api/paas/v4/chat/completions"
    );
    assert_eq!(req.bearer.expose(), SECRET);
    assert_eq!(req.body["model"], "configured-model");
    assert_eq!(req.body["stream"], false);
    assert_eq!(req.body["temperature"], 0);
    assert_eq!(req.body["response_format"], json!({"type":"json_object"}));
    assert!(req.body["max_tokens"].as_u64().unwrap() <= 4096);
    assert_eq!(req.timeout, Duration::from_millis(30_000));
    let text = req.body.to_string();
    assert!(!text.contains(SECRET) && !text.contains("Bearer"));
    assert!(text.contains("Sync both panes") && text.contains("req_1"));
    let user: Value =
        serde_json::from_str(req.body["messages"][1]["content"].as_str().unwrap()).unwrap();
    assert_eq!(
        user["context"],
        json!({"activePane":"left","leftLocation":"disk","rightLocation":"disk",
        "leftCount":3,"rightCount":4,"showHidden":false,"filterActive":false})
    );
    assert_eq!(user.as_object().unwrap().len(), 3);
}

#[test]
fn structured_none_omits_response_format() {
    let rig = Rig::new(vec![ok(&sync_json())]);
    let mut cfg = config();
    cfg.structured = StructuredMode::None;
    plan(&rig.deps(), &cfg, &request(), &CancelToken::default()).unwrap();
    assert!(rig.t.seen.borrow()[0].body.get("response_format").is_none());
}

#[test]
fn parses_intent_and_question() {
    let rig = Rig::new(vec![ok(&format!("  {}\n", sync_json()))]);
    match rig.plan(&CancelToken::default()).unwrap() {
        PlanOutcome::Intent { envelope, repaired } => {
            assert!(!repaired);
            assert!(matches!(envelope.intent, Intent::DirectoriesSync(_)));
        }
        other => panic!("{other:?}"),
    }
    let rig = Rig::new(vec![ok(
        r#"{"kind":"question","text":"Which pane is the destination?"}"#,
    )]);
    assert_eq!(
        rig.plan(&CancelToken::default()).unwrap(),
        PlanOutcome::Question {
            text: "Which pane is the destination?".into()
        }
    );
}

#[test]
fn auth_errors_are_not_retried() {
    for (status, body) in [
        (401, json!({"code":1000,"message":"Authentication Failed"})),
        (403, json!({"error":{"message":"nope"}})),
    ] {
        let rig = Rig::new(vec![err(status, body)]);
        let e = rig.plan(&CancelToken::default()).unwrap_err();
        assert_eq!((e.kind, e.attempts), (ProviderErrorKind::Auth, 1));
        assert_eq!(rig.calls(), 1);
    }
}

#[test]
fn rate_limit_retries_with_bounded_exponential_backoff() {
    let rig = Rig::new(vec![
        err(429, json!({"code":1302,"message":"x"})),
        err(429, json!({"code":1302,"message":"x"})),
        ok(&sync_json()),
    ]);
    assert!(rig.plan(&CancelToken::default()).is_ok());
    assert_eq!(
        *rig.c.sleeps.borrow(),
        vec![Duration::from_millis(500), Duration::from_millis(1000)]
    );
    let rig = Rig::new(vec![
        err(429, json!({})),
        err(429, json!({})),
        err(429, json!({})),
    ]);
    let e = rig.plan(&CancelToken::default()).unwrap_err();
    assert_eq!((e.kind, e.attempts), (ProviderErrorKind::RateLimited, 3));
    assert_eq!(rig.calls(), 3);
}

#[test]
fn retry_after_is_honoured_but_capped() {
    let mut first = resp(429, json!({}));
    first.retry_after = Some(Duration::from_secs(2));
    let mut second = resp(429, json!({}));
    second.retry_after = Some(Duration::from_secs(3600));
    let rig = Rig::new(vec![Ok(first), Ok(second), ok(&sync_json())]);
    rig.plan(&CancelToken::default()).unwrap();
    assert_eq!(
        *rig.c.sleeps.borrow(),
        vec![Duration::from_secs(2), Duration::from_secs(10)]
    );
}

#[test]
fn quota_and_model_errors_are_not_retried() {
    for (status, body, expected) in [
        (
            429,
            json!({"code":1113,"message":"Insufficient balance"}),
            ProviderErrorKind::QuotaExceeded,
        ),
        (
            429,
            json!({"error":{"code":"insufficient_quota","message":"x"}}),
            ProviderErrorKind::QuotaExceeded,
        ),
        (
            400,
            json!({"code":"1211","message":"Unknown Model"}),
            ProviderErrorKind::ModelUnavailable,
        ),
        (
            404,
            json!({"error":{"code":"model_not_found"}}),
            ProviderErrorKind::ModelUnavailable,
        ),
        (
            400,
            json!({"code":1210,"message":"bad"}),
            ProviderErrorKind::BadRequest,
        ),
    ] {
        let rig = Rig::new(vec![err(status, body)]);
        assert_eq!(kind(rig.plan(&CancelToken::default())), expected);
        assert_eq!(rig.calls(), 1);
    }
}

#[test]
fn server_and_network_errors_retry_then_fail() {
    let rig = Rig::new(vec![
        err(503, json!({})),
        Err(TransportError::Network),
        ok(&sync_json()),
    ]);
    assert!(rig.plan(&CancelToken::default()).is_ok());
    let rig = Rig::new(vec![
        err(500, json!({})),
        err(500, json!({})),
        err(502, json!({})),
    ]);
    let e = rig.plan(&CancelToken::default()).unwrap_err();
    assert_eq!((e.kind, e.attempts), (ProviderErrorKind::ServerError, 3));
    let rig = Rig::new(vec![
        Err(TransportError::Network),
        Err(TransportError::Network),
        Err(TransportError::Network),
    ]);
    assert_eq!(
        kind(rig.plan(&CancelToken::default())),
        ProviderErrorKind::Network
    );
}

#[test]
fn unreachable_server_fails_fast_without_retry() {
    let rig = Rig::new(vec![Err(TransportError::Unreachable)]);
    let e = rig.plan(&CancelToken::default()).unwrap_err();
    assert_eq!((e.kind, e.attempts), (ProviderErrorKind::Unreachable, 1));
    assert_eq!(rig.calls(), 1);
    assert!(rig.c.sleeps.borrow().is_empty());
}

#[test]
fn timeout_status_and_bad_json_map_to_stable_kinds() {
    let rig = Rig::new(vec![Err(TransportError::Timeout)]);
    assert_eq!(
        kind(rig.plan(&CancelToken::default())),
        ProviderErrorKind::Timeout
    );
    for (status, expected) in [
        (404, ProviderErrorKind::ModelUnavailable),
        (400, ProviderErrorKind::BadRequest),
    ] {
        let rig = Rig::new(vec![err(status, json!({}))]);
        let e = rig.plan(&CancelToken::default()).unwrap_err();
        assert_eq!((e.kind, e.attempts), (expected, 1));
    }
    // 200 with a body that is not JSON: one repair request, then `malformed` (UI: invalid_response).
    let rig = Rig::new(vec![ok_raw("<html>oops</html>"), ok_raw("not json")]);
    assert_eq!(
        kind(rig.plan(&CancelToken::default())),
        ProviderErrorKind::Malformed
    );
}

#[test]
fn malformed_output_gets_exactly_one_repair_request() {
    let fenced = format!("```json\n{}\n```", sync_json());
    let rig = Rig::new(vec![ok(&fenced), ok(&sync_json())]);
    match rig.plan(&CancelToken::default()).unwrap() {
        PlanOutcome::Intent { repaired, .. } => assert!(repaired),
        other => panic!("{other:?}"),
    }
    assert_eq!(rig.calls(), 2);
    {
        let seen = rig.t.seen.borrow();
        assert_eq!(seen[1].body["messages"].as_array().unwrap().len(), 4);
        assert_eq!(seen[1].body["messages"][2]["role"], "assistant");
    }

    let rig = Rig::new(vec![ok("not json"), ok("still not json")]);
    assert_eq!(
        kind(rig.plan(&CancelToken::default())),
        ProviderErrorKind::Malformed
    );
    assert_eq!(rig.calls(), 2);
}

#[test]
fn contract_violations_are_malformed_and_never_partially_accepted() {
    let injected = json!({"schemaVersion":1,"requestId":"req_1","intent":{"capability":"directories.sync",
        "leftPane":"left","rightPane":"right","mode":"bidirectional","shell":"rm -rf"}})
    .to_string();
    let wrong_id = sync_json().replace("req_1", "req_other");
    let unknown_cap =
        json!({"schemaVersion":1,"requestId":"req_1","intent":{"capability":"shell.exec"}})
            .to_string();
    let bad_question = json!({"kind":"question","text":"","extra":1}).to_string();
    let long_question = json!({"kind":"question","text":"a".repeat(513)}).to_string();
    for bad in [
        injected,
        wrong_id,
        unknown_cap,
        bad_question,
        long_question,
        "[]".into(),
        "null".into(),
        "\"x\"".into(),
    ] {
        let rig = Rig::new(vec![ok(&bad), ok(&bad)]);
        assert_eq!(
            kind(rig.plan(&CancelToken::default())),
            ProviderErrorKind::Malformed,
            "{bad}"
        );
        assert_eq!(rig.calls(), 2);
    }
}

#[test]
fn missing_content_truncation_and_refusal() {
    let rig = Rig::new(vec![
        Ok(resp(200, json!({"choices":[]}))),
        Ok(resp(200, json!({"choices":[]}))),
    ]);
    assert_eq!(
        kind(rig.plan(&CancelToken::default())),
        ProviderErrorKind::Malformed
    );
    let truncated = Ok(resp(
        200,
        json!({"choices":[{"finish_reason":"length","message":{"content":"{\"schema"}}]}),
    ));
    let rig = Rig::new(vec![truncated, ok(&sync_json())]);
    assert!(rig.plan(&CancelToken::default()).is_ok());
    for body in [
        json!({"choices":[{"finish_reason":"sensitive","message":{"content":""}}]}),
        json!({"choices":[{"finish_reason":"stop","message":{"content":null,"refusal":"I cannot"}}]}),
    ] {
        let rig = Rig::new(vec![Ok(resp(200, body))]);
        assert_eq!(
            kind(rig.plan(&CancelToken::default())),
            ProviderErrorKind::Refused
        );
        assert_eq!(rig.calls(), 1);
    }
    let html = || {
        Ok(HttpResponse {
            status: 200,
            retry_after: None,
            body: "<html>".into(),
        })
    };
    let rig = Rig::new(vec![html(), html()]);
    assert_eq!(
        kind(rig.plan(&CancelToken::default())),
        ProviderErrorKind::Malformed
    );
}

#[test]
fn oversized_response_content_is_malformed() {
    let big = format!("{{\"k\":\"{}\"}}", "a".repeat(40_000));
    let rig = Rig::new(vec![ok(&big), ok(&big)]);
    assert_eq!(
        kind(rig.plan(&CancelToken::default())),
        ProviderErrorKind::Malformed
    );
}

#[test]
fn cancellation_never_yields_a_late_result() {
    let cancel = CancelToken::default();
    cancel.cancel();
    let rig = Rig::new(vec![]);
    assert_eq!(kind(rig.plan(&cancel)), ProviderErrorKind::Cancelled);
    assert_eq!(rig.calls(), 0);

    let cancel = CancelToken::default();
    let rig = Rig::new(vec![ok(&sync_json())]);
    *rig.t.cancel_after_call.borrow_mut() = Some(cancel.clone());
    assert_eq!(kind(rig.plan(&cancel)), ProviderErrorKind::Cancelled);

    let cancel = CancelToken::default();
    let rig = Rig::new(vec![err(429, json!({})), ok(&sync_json())]);
    *rig.c.cancel_on_sleep.borrow_mut() = Some(cancel.clone());
    assert_eq!(kind(rig.plan(&cancel)), ProviderErrorKind::Cancelled);
    assert_eq!(rig.calls(), 1);

    let rig = Rig::new(vec![Err(TransportError::Cancelled)]);
    assert_eq!(
        kind(rig.plan(&CancelToken::default())),
        ProviderErrorKind::Cancelled
    );
}

#[test]
fn timeout_is_terminal_and_total_budget_is_enforced() {
    let rig = Rig::new(vec![Err(TransportError::Timeout)]);
    let e = rig.plan(&CancelToken::default()).unwrap_err();
    assert_eq!((e.kind, e.attempts), (ProviderErrorKind::Timeout, 1));
    // Backoff that would exceed the overall deadline stops with Timeout, without another request.
    let rig = Rig::new(vec![err(500, json!({})), err(500, json!({}))]);
    let mut cfg = config();
    cfg.timeout_ms = 400;
    let e = plan(&rig.deps(), &cfg, &request(), &CancelToken::default()).unwrap_err();
    assert_eq!(e.kind, ProviderErrorKind::Timeout);
    assert_eq!(rig.calls(), 1);
    assert!(rig.t.seen.borrow()[0].timeout <= Duration::from_millis(400));
}

#[test]
fn credential_failures_stop_before_network() {
    for cred in [CredentialError::Missing, CredentialError::Unavailable] {
        let mut rig = Rig::new(vec![]);
        rig.creds = FakeCreds(Err(cred));
        assert_eq!(
            kind(rig.plan(&CancelToken::default())),
            ProviderErrorKind::Credential
        );
        assert_eq!(rig.calls(), 0);
    }
}

#[test]
fn secret_never_reaches_logs_errors_or_debug_output() {
    let leaking = json!({"code":1000,"message":format!("bad key {SECRET} Bearer {SECRET}")});
    let rig = Rig::new(vec![err(401, leaking)]);
    let e = rig.plan(&CancelToken::default()).unwrap_err();
    let rendered = format!("{e} {e:?}");
    assert!(!rendered.contains(SECRET));
    for line in rig.l.0.borrow().iter() {
        assert!(!line.contains(SECRET), "{line}");
        assert!(!line.contains("Bearer sk-"), "{line}");
    }
    assert!(!rig.l.0.borrow().is_empty());
    assert!(!format!("{:?}", Secret::new(SECRET.into())).contains(SECRET));
    assert!(!format!("{:?}", rig.t.seen.borrow()[0]).contains(SECRET));
    assert_eq!(
        redact(
            "Authorization: Bearer abc.def-123 and key sk-live-ABCDEF123456 end",
            &[]
        ),
        "Authorization: Bearer [redacted] and key [redacted] end"
    );
    assert_eq!(
        redact("token=hunter2hunter2", &["hunter2hunter2"]),
        "token=[redacted]"
    );
}

#[test]
fn test_connection_reports_status_and_sends_no_user_data() {
    let rig = Rig::new(vec![ok("pong")]);
    let r = test_connection(&rig.deps(), &config(), &CancelToken::default());
    assert_eq!(r, ConnectionReport { ok: true, kind: None });
    {
        let seen = rig.t.seen.borrow();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].body["max_tokens"], 8);
        let all = seen[0].body.to_string();
        assert!(!all.contains("req_") && !all.contains("Sync") && !all.contains("left"));
    }

    let rig = Rig::new(vec![err(401, json!({}))]);
    assert_eq!(
        test_connection(&rig.deps(), &config(), &CancelToken::default()),
        ConnectionReport {
            ok: false,
            kind: Some(ProviderErrorKind::Auth)
        }
    );
    // Transient errors are retried at most once for connection tests (2 attempts, not 3).
    let rig = Rig::new(vec![err(500, json!({})), err(500, json!({}))]);
    assert_eq!(
        test_connection(&rig.deps(), &config(), &CancelToken::default()).kind,
        Some(ProviderErrorKind::ServerError)
    );
    assert_eq!(rig.calls(), 2);

    let mut bad = config();
    bad.endpoint = "http://router.test/v1".into();
    let rig = Rig::new(vec![]);
    assert_eq!(
        test_connection(&rig.deps(), &bad, &CancelToken::default()).kind,
        Some(ProviderErrorKind::UnsafeEndpoint)
    );
    assert_eq!(rig.calls(), 0);
}

#[test]
fn context_validation_and_user_text_limits() {
    let mut r = request();
    r.user_text = "a".repeat(8193);
    assert_eq!(r.validate(), Err(ProviderErrorKind::InvalidConfig));
    let mut r = request();
    r.request_id = "../x".into();
    assert_eq!(r.validate(), Err(ProviderErrorKind::InvalidConfig));
    let mut r = request();
    r.context.left_count = 2_000_000;
    assert_eq!(r.validate(), Err(ProviderErrorKind::InvalidConfig));
    let bad: Result<OutboundContext, _> = serde_json::from_value(json!({"activePane":"left",
        "leftLocation":"disk","rightLocation":"disk","leftCount":1,"rightCount":1,
        "showHidden":false,"filterActive":false,"paths":["C:\\x"]}));
    assert!(bad.is_err());
    let rig = Rig::new(vec![]);
    let mut r = request();
    r.user_text = "a".repeat(9000);
    assert_eq!(
        plan(&rig.deps(), &config(), &r, &CancelToken::default())
            .unwrap_err()
            .kind,
        ProviderErrorKind::InvalidConfig
    );
    assert_eq!(rig.calls(), 0);
}

#[test]
fn error_kinds_serialize_as_stable_snake_case_ids() {
    assert_eq!(
        serde_json::to_value(ProviderErrorKind::RateLimited).unwrap(),
        json!("rate_limited")
    );
    assert_eq!(
        serde_json::to_value(ProviderErrorKind::ModelUnavailable).unwrap(),
        json!("model_unavailable")
    );
    assert_eq!(
        serde_json::to_value(ProviderErrorKind::UnsafeEndpoint).unwrap(),
        json!("unsafe_endpoint")
    );
    assert_eq!(
        serde_json::to_value(ProviderErrorKind::Unreachable).unwrap(),
        json!("unreachable")
    );
}

// ---- dynamic capability offering ----------------------------------------------------------------

fn request_offering(capabilities: Option<Vec<&str>>) -> PlanRequest {
    let mut r = request();
    r.capabilities = capabilities.map(|c| c.into_iter().map(str::to_owned).collect());
    r
}
fn system_text(rig: &Rig) -> String {
    rig.t.seen.borrow()[0].body["messages"][0]["content"]
        .as_str()
        .expect("system prompt")
        .to_owned()
}
fn plan_with(rig: &Rig, req: &PlanRequest) -> Result<PlanOutcome, ProviderError> {
    plan(&rig.deps(), &config(), req, &CancelToken::default())
}

#[test]
fn prompt_lists_only_the_capabilities_the_app_offers() {
    let rig = Rig::new(vec![ok(&sync_json())]);
    plan_with(&rig, &request_offering(Some(vec!["directories.sync"]))).expect("plan");
    let prompt = system_text(&rig);
    assert!(prompt.contains("Intent directories.sync"));
    assert!(!prompt.contains("Intent archive.create"));

    let both = Rig::new(vec![ok(&sync_json())]);
    plan_with(&both, &request_offering(None)).expect("legacy caller");
    let prompt = system_text(&both);
    assert!(prompt.contains("Intent directories.sync") && prompt.contains("Intent archive.create"));
}

#[test]
fn a_capability_that_was_not_offered_is_rejected_even_if_well_formed() {
    // The model answers with a valid sync intent although only archive.create was offered:
    // rejected locally, one repair request, then malformed. No intent leaves the provider layer.
    let rig = Rig::new(vec![ok(&sync_json()), ok(&sync_json())]);
    let result = plan_with(&rig, &request_offering(Some(vec!["archive.create"])));
    assert_eq!(kind(result), ProviderErrorKind::Malformed);
    assert_eq!(rig.calls(), 2);
}

#[test]
fn offering_nothing_makes_no_provider_request() {
    let rig = Rig::new(vec![]);
    let outcome = plan_with(&rig, &request_offering(Some(vec![]))).expect("local answer");
    assert!(matches!(outcome, PlanOutcome::Question { .. }));
    assert_eq!(rig.calls(), 0);
}

#[test]
fn unknown_or_duplicate_capability_ids_are_invalid_before_any_network_use() {
    for caps in [vec!["shell.exec"], vec!["archive.create", "archive.create"]] {
        let rig = Rig::new(vec![]);
        let result = plan_with(&rig, &request_offering(Some(caps)));
        assert_eq!(kind(result), ProviderErrorKind::InvalidConfig);
        assert_eq!(rig.calls(), 0);
    }
}

#[test]
fn archive_prompt_says_all_files_in_a_folder_means_every_entry() {
    // Smoke finding (desktop): the model read "semua file" as files only and left subfolders out.
    let rig = Rig::new(vec![ok(&sync_json())]);
    plan_with(&rig, &request_offering(Some(vec!["archive.create", "directories.sync"]))).expect("plan");
    let prompt = system_text(&rig);
    assert!(prompt.contains("DEFAULT is entries \"all\""));
    assert!(prompt.contains("Use entries \"filesOnly\" ONLY when the user explicitly says to skip folders"));
}

#[test]
fn archive_prompt_describes_extension_filter_and_follow_up_rules() {
    // AI-09: follow-up keeps the same envelope contract; the model never sees paths or file names.
    let rig = Rig::new(vec![ok(&sync_json())]);
    plan_with(&rig, &request_offering(Some(vec!["archive.create", "directories.sync"]))).expect("plan");
    let prompt = system_text(&rig);
    assert!(prompt.contains("\"extensions\":[lowercase extensions without dot, at most 8]"));
    assert!(prompt.contains("Follow-up to the previous plan"));
    assert!(prompt.contains("ONE complete new intent envelope"));
    assert!(prompt.contains("Never add paths or file names"));
}
