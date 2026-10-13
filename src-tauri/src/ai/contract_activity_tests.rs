// ACT-08 R2: `activity.query` is a read intent. It parses only through
// `parse_intent_offered` when the app offered it; legacy `parse_intent` keeps
// rejecting it exactly as before. Cases are shared with the TS mirror
// (contract-fixtures/ai/activity-query-cases.json).
use super::*;
use serde_json::{Value, json};

const OFFERED: &[&str] = &[
    "archive.create",
    "directories.sync",
    ACTIVITY_QUERY_CAPABILITY,
];
const MUTATING_ONLY: &[&str] = &["archive.create", "directories.sync"];

fn cases() -> Vec<Value> {
    serde_json::from_str(include_str!(
        "../../../contract-fixtures/ai/activity-query-cases.json"
    ))
    .unwrap()
}

fn recent() -> Value {
    json!({"schemaVersion":1,"requestId":"req_1","intent":{
        "capability":"activity.query","query":{"kind":"recent","periodDays":7,"entity":"folder"}}})
}

fn archive() -> Value {
    json!({"schemaVersion":1,"requestId":"req_1","intent":{
        "capability":"archive.create","sourcePane":"left",
        "selector":{"kind":"allVisible","entries":"filesOnly"},
        "format":"zip","target":{"kind":"pane","pane":"right"}}})
}

#[test]
fn shared_cases_match_when_the_capability_is_offered() {
    let cases = cases();
    assert!(cases.len() >= 20);
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let expected = case["valid"].as_bool().unwrap();
        let actual = parse_intent_offered(case["value"].clone(), "req_1", OFFERED);
        assert_eq!(actual.is_ok(), expected, "{name}: {actual:?}");
        if let Ok(envelope) = actual {
            assert_eq!(envelope.intent.capability_id(), ACTIVITY_QUERY_CAPABILITY);
        }
    }
}

#[test]
fn not_offered_or_legacy_parser_rejects_every_activity_query() {
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
        assert!(
            parse_intent_offered(case["value"].clone(), "req_1", &[]).is_err(),
            "{name}"
        );
    }
}

#[test]
fn parsed_query_is_typed_and_round_trips() {
    let envelope = parse_intent_offered(recent(), "req_1", OFFERED).unwrap();
    let Intent::ActivityQuery(query) = &envelope.intent else {
        panic!("{envelope:?}")
    };
    assert_eq!(
        query.query,
        HistoryQuery::Recent {
            period_days: 7,
            entity: HistoryEntity::Folder
        }
    );
    assert_eq!(query.limit, None);
    assert_eq!(serde_json::to_value(&envelope).unwrap(), recent());
    let find = json!({"query":{"kind":"findByName","name":"Invoice","entity":"file"},"limit":3});
    let parsed: ActivityQuery = serde_json::from_value(find.clone()).unwrap();
    assert!(validate_activity_query(&parsed).is_ok());
    assert_eq!(serde_json::to_value(&parsed).unwrap(), find);
}

#[test]
fn mutating_intents_are_unchanged_and_still_need_to_be_offered() {
    let legacy = parse_intent(archive(), "req_1").unwrap();
    assert!(matches!(legacy.intent, Intent::ArchiveCreate(_)));
    assert_eq!(
        parse_intent_offered(archive(), "req_1", OFFERED).unwrap(),
        legacy
    );
    assert_eq!(
        parse_intent_offered(archive(), "req_1", MUTATING_ONLY).unwrap(),
        legacy
    );
    assert!(parse_intent_offered(archive(), "req_1", &[ACTIVITY_QUERY_CAPABILITY]).is_err());
    assert_eq!(legacy.intent.capability_id(), "archive.create");
}

#[test]
fn offered_parser_keeps_envelope_rules() {
    let mut wrong_version = recent();
    wrong_version["schemaVersion"] = json!(2);
    assert_eq!(
        parse_intent_offered(wrong_version, "req_1", OFFERED),
        Err(ContractError::UnsupportedVersion)
    );
    assert_eq!(
        parse_intent_offered(recent(), "req_9", OFFERED),
        Err(ContractError::RequestMismatch)
    );
    assert_eq!(
        parse_intent_offered(recent(), "../req", OFFERED),
        Err(ContractError::InvalidRequestId)
    );
    let mut extra = recent();
    extra["approved"] = json!(true);
    assert!(parse_intent_offered(extra, "req_1", OFFERED).is_err());
}

#[test]
fn wire_validation_bounds() {
    let base = |query: Value, limit: Option<u8>| {
        let mut value = json!({ "query": query });
        if let Some(limit) = limit {
            value["limit"] = json!(limit);
        }
        serde_json::from_value::<ActivityQuery>(value)
            .map_err(|e| e.to_string())
            .and_then(|q| validate_activity_query(&q).map_err(|e| e.to_string()))
    };
    let recent = |days: u16| json!({"kind":"recent","periodDays":days,"entity":"folder"});
    for days in [7, 30, 90] {
        assert!(base(recent(days), None).is_ok(), "{days}");
    }
    for days in [0, 1, 8, 14, 31, 60, 89, 91, 365] {
        assert!(base(recent(days), None).is_err(), "{days}");
    }
    for limit in 1..=MAX_HISTORY_LIMIT {
        assert!(base(recent(7), Some(limit)).is_ok(), "{limit}");
    }
    for limit in [0, 21, 100, 255] {
        assert!(base(recent(7), Some(limit)).is_err(), "{limit}");
    }
    let name = |n: &str| json!({"kind":"findByName","name":n,"entity":"file"});
    assert!(base(name(&"a".repeat(MAX_HISTORY_NAME_BYTES)), None).is_ok());
    assert!(base(name(&"a".repeat(MAX_HISTORY_NAME_BYTES + 1)), None).is_err());
    for bad in ["", " ", "a\\b", "a/b", "C:", "x:y", "a\nb", "\u{0}"] {
        assert!(base(name(bad), None).is_err(), "{bad:?}");
    }
    assert!(base(name("Laporan Keuangan (final) 2026.xlsx"), None).is_ok());
}
