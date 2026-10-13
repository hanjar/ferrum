// FER-471: `copy.adjust` is a control intent (performance of the running copy job only). It parses
// only through `parse_intent_offered` when the app offered it; the legacy parser rejects it.
// Cases are shared with the TS mirror (contract-fixtures/ai/copy-adjust-cases.json).
use super::*;
use serde_json::{Value, json};

const OFFERED: &[&str] = &["archive.create", "directories.sync", COPY_ADJUST_CAPABILITY];
const MUTATING_ONLY: &[&str] = &["archive.create", "directories.sync"];

fn cases() -> Vec<Value> {
    serde_json::from_str(include_str!(
        "../../../contract-fixtures/ai/copy-adjust-cases.json"
    ))
    .unwrap()
}

#[test]
fn shared_cases_match_when_the_capability_is_offered() {
    let cases = cases();
    assert!(cases.len() >= 30);
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let expected = case["valid"].as_bool().unwrap();
        let actual = parse_intent_offered(case["value"].clone(), "req_1", OFFERED);
        assert_eq!(actual.is_ok(), expected, "{name}: {actual:?}");
        if let Ok(envelope) = actual {
            assert_eq!(envelope.intent.capability_id(), COPY_ADJUST_CAPABILITY);
        }
    }
}

#[test]
fn not_offered_or_legacy_parser_rejects_every_copy_adjust() {
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
fn a_valid_request_round_trips_with_only_its_own_fields() {
    let value = json!({"schemaVersion":1,"requestId":"req_1","intent":{
        "capability":"copy.adjust","action":"set","threads":8}});
    let envelope = parse_intent_offered(value.clone(), "req_1", OFFERED).unwrap();
    let Intent::CopyAdjust(request) = &envelope.intent else {
        panic!("expected copy.adjust")
    };
    assert_eq!(request.action, CopyAdjustAction::Set);
    assert_eq!(request.threads, Some(8));
    assert_eq!(serde_json::to_value(&envelope).unwrap(), value);
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
