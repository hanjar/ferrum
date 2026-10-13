// FER-466: `diskUsage.query` is a read intent. The model only picks the kind of question (and a
// scope / category / limit from closed sets); no path, name or size is ever part of it. It parses
// only through `parse_intent_offered` when the app offered it; the legacy parser rejects it.
// Cases are shared with the TS mirror (contract-fixtures/ai/disk-usage-cases.json).
use super::*;
use serde_json::{Value, json};

const OFFERED: &[&str] = &[
    "archive.create",
    "directories.sync",
    DISK_USAGE_QUERY_CAPABILITY,
];
const MUTATING_ONLY: &[&str] = &["archive.create", "directories.sync"];

fn cases() -> Vec<Value> {
    serde_json::from_str(include_str!(
        "../../../contract-fixtures/ai/disk-usage-cases.json"
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
            assert_eq!(
                envelope.intent.capability_id(),
                DISK_USAGE_QUERY_CAPABILITY,
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
fn a_valid_query_round_trips_with_only_its_own_fields() {
    let value = json!({"schemaVersion":1,"requestId":"req_1","intent":{
        "capability":"diskUsage.query",
        "query":{"kind":"largestFiles","scope":"root","category":"video","limit":5}}});
    let envelope = parse_intent_offered(value.clone(), "req_1", OFFERED).unwrap();
    let Intent::DiskUsageQuery(request) = &envelope.intent else {
        panic!("expected diskUsage.query")
    };
    assert_eq!(
        request.query,
        DiskUsageQueryKind::LargestFiles {
            scope: Some(DiskUsageScope::Root),
            category: Some(DiskUsageCategory::Video),
            limit: Some(5),
        }
    );
    assert_eq!(serde_json::to_value(&envelope).unwrap(), value);

    let summary = json!({"schemaVersion":1,"requestId":"req_1","intent":{
        "capability":"diskUsage.query","query":{"kind":"summary"}}});
    let envelope = parse_intent_offered(summary.clone(), "req_1", OFFERED).unwrap();
    assert_eq!(serde_json::to_value(&envelope).unwrap(), summary);
}

#[test]
fn the_limit_bound_matches_the_constant() {
    assert_eq!(MAX_DISK_USAGE_LIMIT, 20);
}

#[test]
fn mutating_intents_are_unaffected() {
    let archive = json!({"schemaVersion":1,"requestId":"req_1","intent":{
        "capability":"archive.create","sourcePane":"left","selector":{"kind":"selected"},
        "format":"zip","target":{"kind":"sourceDirectory"}}});
    assert!(parse_intent(archive.clone(), "req_1").is_ok());
    assert!(parse_intent_offered(archive, "req_1", OFFERED).is_ok());
}
