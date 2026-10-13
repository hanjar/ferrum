// FER-476: `indexer.status` (read) and `indexer.setMode` (control) are model intents that carry no
// path and no free text: only the target mode. They parse only through `parse_intent_offered` when
// the app offered them; the legacy parser rejects them. Cases are shared with the TS mirror
// (contract-fixtures/ai/indexer-cases.json).
use super::*;
use serde_json::{Value, json};

const OFFERED: &[&str] = &[
    "archive.create",
    "directories.sync",
    INDEXER_STATUS_CAPABILITY,
    INDEXER_SET_MODE_CAPABILITY,
];
const MUTATING_ONLY: &[&str] = &["archive.create", "directories.sync"];

fn cases() -> Vec<Value> {
    serde_json::from_str(include_str!(
        "../../../contract-fixtures/ai/indexer-cases.json"
    ))
    .unwrap()
}

#[test]
fn shared_cases_match_when_the_capabilities_are_offered() {
    let cases = cases();
    assert!(cases.len() >= 25);
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let expected = case["valid"].as_bool().unwrap();
        let actual = parse_intent_offered(case["value"].clone(), "req_1", OFFERED);
        assert_eq!(actual.is_ok(), expected, "{name}: {actual:?}");
        if let Ok(envelope) = actual {
            assert!(
                matches!(
                    envelope.intent.capability_id(),
                    INDEXER_STATUS_CAPABILITY | INDEXER_SET_MODE_CAPABILITY
                ),
                "{name}"
            );
        }
    }
}

#[test]
fn not_offered_or_legacy_parser_rejects_every_case() {
    for case in cases() {
        let name = case["name"].as_str().unwrap();
        assert!(
            parse_intent(case["value"].clone(), "req_1").is_err(),
            "legacy {name}"
        );
        assert!(
            parse_intent_offered(case["value"].clone(), "req_1", MUTATING_ONLY).is_err(),
            "not offered {name}"
        );
    }
}

#[test]
fn offering_only_one_of_the_two_does_not_open_the_other() {
    let status =
        json!({"schemaVersion":1,"requestId":"req_1","intent":{"capability":"indexer.status"}});
    let mode = json!({"schemaVersion":1,"requestId":"req_1","intent":{
        "capability":"indexer.setMode","mode":"shared"}});
    assert!(parse_intent_offered(status.clone(), "req_1", &[INDEXER_SET_MODE_CAPABILITY]).is_err());
    assert!(parse_intent_offered(mode.clone(), "req_1", &[INDEXER_STATUS_CAPABILITY]).is_err());
    assert!(parse_intent_offered(status, "req_1", &[INDEXER_STATUS_CAPABILITY]).is_ok());
    assert!(parse_intent_offered(mode, "req_1", &[INDEXER_SET_MODE_CAPABILITY]).is_ok());
}

#[test]
fn a_valid_request_round_trips_with_only_its_own_fields() {
    let value = json!({"schemaVersion":1,"requestId":"req_1","intent":{
        "capability":"indexer.setMode","mode":"high_performance"}});
    let envelope = parse_intent_offered(value.clone(), "req_1", OFFERED).unwrap();
    let Intent::IndexerSetMode(request) = &envelope.intent else {
        panic!("expected indexer.setMode")
    };
    assert_eq!(request.mode, IndexerModeChoice::HighPerformance);
    assert_eq!(serde_json::to_value(&envelope).unwrap(), value);

    let status =
        json!({"schemaVersion":1,"requestId":"req_1","intent":{"capability":"indexer.status"}});
    let envelope = parse_intent_offered(status.clone(), "req_1", OFFERED).unwrap();
    assert!(matches!(envelope.intent, Intent::IndexerStatus(_)));
    assert_eq!(serde_json::to_value(&envelope).unwrap(), status);
}

#[test]
fn mutating_intents_are_unaffected() {
    let archive = json!({"schemaVersion":1,"requestId":"req_1","intent":{
        "capability":"archive.create","sourcePane":"left",
        "selector":{"kind":"allVisible","entries":"filesOnly"},
        "format":"zip","target":{"kind":"pane","pane":"right"}}});
    assert!(parse_intent(archive.clone(), "req_1").is_ok());
    assert!(parse_intent_offered(archive, "req_1", OFFERED).is_ok());
}
