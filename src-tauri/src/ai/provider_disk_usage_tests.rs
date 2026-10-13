// FER-466: the `diskUsage.query` prompt section is additive and opt-in, appended after every older
// capability so older prompt text stays byte-identical. The request that reaches the provider
// carries no Disk Usage data at all.
use super::*;

const ARCHIVE: &str = "archive.create";
const SYNC: &str = "directories.sync";
const QUERY: &str = "activity.query";
const ADJUST: &str = "copy.adjust";
const STATUS: &str = "indexer.status";
const MODE: &str = "indexer.setMode";
const DISK: &str = "diskUsage.query";

fn request(capabilities: Option<Vec<&str>>) -> PlanRequest {
    PlanRequest {
        request_id: "req_1".into(),
        user_text: "paling gede itu apa ya".into(),
        context: OutboundContext {
            active_pane: PaneName::Left,
            left_location: LocationKind::Disk,
            right_location: LocationKind::Disk,
            left_count: 3,
            right_count: 4,
            show_hidden: false,
            filter_active: false,
        },
        capabilities: capabilities.map(|list| list.into_iter().map(str::to_owned).collect()),
    }
}

fn reply(content: &str) -> String {
    serde_json::json!({"choices":[{"finish_reason":"stop","message":{"content":content}}]})
        .to_string()
}

const DISK_INTENT: &str = "{\"schemaVersion\":1,\"requestId\":\"req_1\",\"intent\":{\"capability\":\"diskUsage.query\",\"query\":{\"kind\":\"largestFiles\",\"limit\":5}}}";

#[test]
fn the_section_is_appended_last_and_only_when_offered() {
    let older = [ARCHIVE, SYNC, QUERY, ADJUST, STATUS, MODE];
    let without = system_prompt(&older);
    let mut all = older.to_vec();
    all.push(DISK);
    let with = system_prompt(&all);
    let tail_start = without.rfind("Follow-up:").unwrap();
    assert_eq!(&with[..tail_start], &without[..tail_start]);
    assert!(with.contains(DISK_USAGE_QUERY_PROMPT));
    assert!(!without.contains("diskUsage"));
    assert!(system_prompt(&[DISK]).contains("Intent diskUsage.query"));
    // Legacy callers (no list) never see it.
    assert_eq!(offered_capabilities(&request(None)), vec![ARCHIVE, SYNC]);
}

#[test]
fn the_prompt_says_the_model_never_sees_the_result_and_names_nothing() {
    for needle in [
        "Intent diskUsage.query",
        "read-only",
        "the app answers locally",
        "\"capability\":\"diskUsage.query\"",
        "You never see the result",
        "Never name a path, file or folder",
    ] {
        assert!(DISK_USAGE_QUERY_PROMPT.contains(needle), "{needle}");
    }
}

#[test]
fn the_intent_is_accepted_only_when_offered() {
    let body = reply(DISK_INTENT);
    let Interpreted::Done(PlanOutcome::Intent { envelope, .. }) =
        interpret(&body, "req_1", &[ARCHIVE, DISK])
    else {
        panic!("offered diskUsage.query must parse")
    };
    assert_eq!(envelope.intent.capability_id(), DISK);
    assert!(matches!(
        interpret(
            &body,
            "req_1",
            &[ARCHIVE, SYNC, QUERY, ADJUST, STATUS, MODE]
        ),
        Interpreted::Malformed(_)
    ));
    // A model-supplied path is rejected.
    let bad = DISK_INTENT.replace("\"limit\":5", "\"limit\":5,\"path\":\"C:\\Users\\me\"");
    assert!(matches!(
        interpret(&reply(&bad), "req_1", &[DISK]),
        Interpreted::Malformed(_)
    ));
}

#[test]
fn the_capability_list_accepts_the_id_and_rejects_unknown_or_duplicate_ones() {
    assert!(request(Some(vec![DISK])).validate().is_ok());
    assert!(
        request(Some(vec![ARCHIVE, SYNC, QUERY, ADJUST, STATUS, MODE, DISK]))
            .validate()
            .is_ok()
    );
    assert!(request(Some(vec![DISK, DISK])).validate().is_err());
    assert!(request(Some(vec!["diskusage.query"])).validate().is_err());
}

#[test]
fn the_outbound_request_carries_no_disk_usage_data() {
    // The only per-request data are the user's words, two pane kinds and two counts: there is no
    // field a scan result (path, name, size, count) could travel in.
    let json = serde_json::to_value(request(Some(vec![DISK]))).ok();
    let text = json.map(|v| v.to_string()).unwrap_or_default();
    for needle in ["path", "size", "bytes", "scan", "snapshot"] {
        assert!(!text.contains(needle), "{needle} in {text}");
    }
}
