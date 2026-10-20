// FER-573: the provider's own error message is summarized for display (sanitized, bounded) and a
// size question without Disk Usage yields a fixed hint. Neither path sends or keeps anything else.
use super::*;
use crate::ai::credential::CredentialError;
use std::cell::RefCell;

const SECRET: &str = "sk-live-SECRETVALUE-0123456789";

/// The 9router body from the FER-573 report (an upstream error wrapped inside `error.message`).
const NINE_ROUTER: &str = "{\"error\":{\"message\":\"[antigravity/gemini-4-argon-high] [404]: {\\n \\\"error\\\": {\\n \\\"code\\\": 404,\\n \\\"message\\\": \\\"Requested entity was not found.\\\",\\n \\\"status\\\": \\\"NOT_FOUND\\\"\\n }\\n}\\n (reset after 1m 51s)\"}}";

fn summary(body: &str) -> Option<String> {
    summarize_error_body(body, &[SECRET])
}

#[test]
fn the_nine_router_body_becomes_one_readable_line() {
    let text = summary(NINE_ROUTER).unwrap();
    assert_eq!(
        text,
        "[antigravity/gemini-4-argon-high] [404]: Requested entity was not found. (reset after 1m 51s)"
    );
}

#[test]
fn standard_openai_and_flat_message_bodies_are_read() {
    let openai = r#"{"error":{"message":"The model `gpt-x` does not exist","type":"invalid_request_error","code":"model_not_found","param":null}}"#;
    assert_eq!(summary(openai).unwrap(), "The model `gpt-x` does not exist");
    assert_eq!(
        summary(r#"{"message":"Rate limit hit","code":"1302"}"#).unwrap(),
        "Rate limit hit"
    );
    assert_eq!(
        summary(r#"{"error":"upstream timed out"}"#).unwrap(),
        "upstream timed out"
    );
}

#[test]
fn json_without_a_message_field_shows_nothing_and_extra_fields_never_leak() {
    assert_eq!(
        summary(r#"{"detail":"x","headers":{"authorization":"Bearer abc"}}"#),
        None
    );
    let text =
        summary(r#"{"error":{"message":"bad","headers":{"x-api-key":"zzz"},"request_id":"r-1"}}"#)
            .unwrap();
    assert_eq!(text, "bad");
    assert_eq!(summary("   "), None);
}

#[test]
fn html_and_plain_text_bodies_are_stripped_and_bounded() {
    let html = "<html><head><title>502 Bad Gateway</title></head>\n<body><h1>Bad   Gateway</h1></body></html>";
    let text = summary(html).unwrap();
    assert!(!text.contains('<') && !text.contains('>'));
    assert!(text.contains("502 Bad Gateway"));
    let long = "boom ".repeat(500);
    assert_eq!(summary(&long).unwrap().chars().count(), MAX_DETAIL_CHARS);
    let long_json = format!(r#"{{"error":{{"message":"{}"}}}}"#, "é".repeat(1000));
    assert_eq!(
        summary(&long_json).unwrap().chars().count(),
        MAX_DETAIL_CHARS
    );
}

/// S2: the user's actual key is hidden verbatim even when no heuristic would catch it.
#[test]
fn the_actual_key_is_redacted_verbatim_without_any_heuristic() {
    for key in ["abcdefghijkl", "my-key-abc-123"] {
        let body = format!(r#"{{"error":{{"message":"invalid api key {key} for this route"}}}}"#);
        let text = summarize_error_body(&body, &[key]).unwrap();
        assert!(!text.contains(key), "{key} leaked in {text}");
        assert!(text.contains("[redacted]"));
        // Same key split by zero-width / bidi characters still matches.
        let split = format!(
            r#"{{"message":"bad {}\u200B{}\u202E end"}}"#,
            &key[..3],
            &key[3..]
        );
        let text = summarize_error_body(&split, &[key]).unwrap();
        assert!(!text.contains(key) && !text.contains(&key[3..]), "{text}");
    }
}

/// S1: bidi and other format characters cannot reorder the text (e.g. the "(model X)" suffix).
#[test]
fn bidi_and_format_characters_are_removed() {
    let rlo =
        "\u{202E}txt.exe \u{2066}x\u{2069} \u{200B}\u{200F}\u{061C}\u{FEFF}\u{00AD}ok\u{E0041}";
    let body = serde_json::json!({"error": {"message": rlo}}).to_string();
    let text = summary(&body).unwrap();
    assert_eq!(text, "txt.exe x ok");
    for c in text.chars() {
        assert!(
            !c.is_control() && !crate::hotlist::model::is_format_char(c),
            "{c:?}"
        );
    }
}

#[test]
fn bearer_is_redacted_in_any_letter_case() {
    for marker in ["Bearer", "bearer", "BEARER", "BeArEr"] {
        let text = summary(&format!(
            r#"{{"message":"auth {marker} secrettoken9 failed"}}"#
        ))
        .unwrap();
        assert!(!text.contains("secrettoken9"), "{text}");
    }
}

#[test]
fn keys_tokens_urls_and_control_characters_are_removed() {
    let body = format!(
        "{{\"error\":{{\"message\":\"bad key sk-abcdef0123456789 and {SECRET} Bearer eyJhbGciOi.payload.sig \
         at https://api.example.com/v1/x?api_key=hunter2&b=1#frag token QWxhZGRpbjpvcGVuIHNlc2FtZTEyMzQ1Njc4OTBhYmNkZWY= \
         \\u001b[31mred\\u001b[0m\\u0007 done\"}}}}"
    );
    let text = summary(&body).unwrap();
    for leaked in [
        "sk-abcdef",
        "SECRETVALUE",
        "eyJhbGci",
        "hunter2",
        "api_key",
        "frag",
        "QWxhZGRp",
        "\u{1b}",
        "[31m",
    ] {
        assert!(!text.contains(leaked), "{leaked} leaked in {text}");
    }
    assert!(text.contains("https://api.example.com/v1/x"));
    assert!(text.contains("red") && text.contains("done"));
    assert!(!text.chars().any(char::is_control));
    // A long hex id is hidden; hyphenated lowercase model ids are not.
    let hex = summary(
        r#"{"message":"id 9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08 ok"}"#,
    )
    .unwrap();
    assert!(hex.contains("[redacted]") && !hex.contains("9f86d081"));
    let model =
        summary(r#"{"message":"no route for antigravity/gemini-4-argon-high-preview-x"}"#).unwrap();
    assert!(model.contains("antigravity/gemini-4-argon-high-preview-x"));
}

// ---- flow through send / test_connection with a fake transport -------------------------------

struct Fake {
    status: u16,
    body: String,
    calls: RefCell<u32>,
}
impl HttpTransport for Fake {
    fn post_json(
        &self,
        _req: &HttpRequest,
        _cancel: &CancelToken,
    ) -> Result<HttpResponse, TransportError> {
        *self.calls.borrow_mut() += 1;
        Ok(HttpResponse {
            status: self.status,
            retry_after: None,
            body: self.body.clone(),
        })
    }
}
struct Creds;
impl CredentialSource for Creds {
    fn read(&self, _reference: &str) -> Result<Secret, CredentialError> {
        Ok(Secret::new(SECRET.to_owned()))
    }
}
struct Clk;
impl Clock for Clk {
    fn now(&self) -> Duration {
        Duration::ZERO
    }
    fn sleep(&self, _delay: Duration, _cancel: &CancelToken) -> bool {
        true
    }
}
#[derive(Default)]
struct Lines(std::sync::Mutex<Vec<String>>);
impl LogSink for Lines {
    fn log(&self, line: &str) {
        self.0.lock().unwrap().push(line.to_owned());
    }
}

fn config() -> ProviderConfig {
    ProviderConfig {
        endpoint: "http://127.0.0.1:20128/v1".to_owned(),
        model: "ag/gemini-4-argon-high".to_owned(),
        timeout_ms: 5000,
        credential_ref: "Ferrum/AI/custom".to_owned(),
        structured: StructuredMode::None,
    }
}

#[test]
fn test_connection_carries_the_providers_message_but_keeps_the_kind() {
    let transport = Fake {
        status: 404,
        body: NINE_ROUTER.to_owned(),
        calls: RefCell::new(0),
    };
    let log = Lines::default();
    let deps = Deps {
        transport: &transport,
        credentials: &Creds,
        clock: &Clk,
        log: &log,
    };
    let report = test_connection(&deps, &config(), &CancelToken::default());
    assert!(!report.ok);
    assert_eq!(report.kind, Some(ProviderErrorKind::ModelUnavailable));
    assert!(
        report
            .detail
            .as_deref()
            .unwrap()
            .contains("Requested entity was not found.")
    );
    let wire = serde_json::to_value(&report).unwrap();
    assert_eq!(wire["kind"], "model_unavailable");
    assert!(
        wire["detail"]
            .as_str()
            .unwrap()
            .starts_with("[antigravity/")
    );
    for line in log.0.lock().unwrap().iter() {
        assert!(!line.contains("SECRETVALUE"));
    }
}

#[test]
fn a_retried_server_error_reports_the_last_message() {
    let transport = Fake {
        status: 503,
        body: r#"{"error":{"message":"overloaded, key sk-abcdef0123456789"}}"#.to_owned(),
        calls: RefCell::new(0),
    };
    let deps = Deps {
        transport: &transport,
        credentials: &Creds,
        clock: &Clk,
        log: &NullLog,
    };
    let report = test_connection(&deps, &config(), &CancelToken::default());
    assert_eq!(report.kind, Some(ProviderErrorKind::ServerError));
    let detail = report.detail.unwrap();
    assert!(detail.contains("overloaded") && !detail.contains("sk-abcdef"));
    assert!(*transport.calls.borrow() >= 2, "server errors are retried");
}

#[test]
fn transport_failures_have_no_detail() {
    assert_eq!(fail(ProviderErrorKind::Timeout, 1).detail, None);
    let wire = serde_json::to_value(fail(ProviderErrorKind::Timeout, 1)).unwrap();
    assert!(wire.get("detail").is_none());
}

// ---- Disk Usage hint -------------------------------------------------------------------------

const HINT: &str = r#"{"kind":"hint","hint":"openDiskUsage"}"#;

fn body(content: &str) -> String {
    serde_json::json!({"choices":[{"finish_reason":"stop","message":{"content":content}}]})
        .to_string()
}

#[test]
fn the_hint_prompt_is_appended_last_and_only_when_offered() {
    let older = [
        "archive.create",
        "directories.sync",
        "pane.navigate",
        "items.delete",
    ];
    let without = system_prompt(&older);
    let mut with_list = older.to_vec();
    with_list.push("ui.hint");
    let with = system_prompt(&with_list);
    assert!(with.contains(UI_HINT_PROMPT));
    assert!(!without.contains("openDiskUsage"));
    // Removing the fragment gives back the older prompt byte for byte.
    assert_eq!(with.replace(UI_HINT_PROMPT, ""), without);
}

#[test]
fn the_hint_needs_ui_hint_and_is_refused_when_disk_usage_is_offered() {
    let reply = body(HINT);
    assert!(matches!(
        interpret_for(
            &reply,
            "req_1",
            &["archive.create", "ui.hint"],
            "ukuran folder"
        ),
        Interpreted::Done(PlanOutcome::Hint {
            hint: UiHint::OpenDiskUsage
        })
    ));
    for offered in [
        vec!["archive.create"],
        vec!["archive.create", "ui.hint", "diskUsage.query"],
    ] {
        assert!(matches!(
            interpret_for(&reply, "req_1", &offered, "ukuran folder"),
            Interpreted::Malformed(_)
        ));
    }
}

#[test]
fn shared_hint_cases_match_the_ts_mirror() {
    let cases: Vec<serde_json::Value> = serde_json::from_str(include_str!(
        "../../../contract-fixtures/ai/hint-cases.json"
    ))
    .unwrap();
    assert!(cases.len() >= 10);
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let offered: Vec<&str> = case["offered"]
            .as_array()
            .unwrap()
            .iter()
            .map(|id| id.as_str().unwrap())
            .collect();
        let reply = body(&case["value"].to_string());
        let accepted = matches!(
            interpret_for(&reply, "req_1", &offered, "x"),
            Interpreted::Done(PlanOutcome::Hint { .. })
        );
        assert_eq!(accepted, case["valid"].as_bool().unwrap(), "{name}");
    }
}
