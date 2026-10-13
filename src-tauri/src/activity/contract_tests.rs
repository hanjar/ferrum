#[path = "contract.rs"]
mod contract;

use contract::{
    ActivityPreferences, ActivityQuery, ActivityReceipt, ActivityResult, ActivityStats,
};
use serde_json::{Value, json};

fn fixture(file: &str) -> Value {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let path = root.join("../../../contract-fixtures/activity").join(file);
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn shared_query_result_receipt_round_trip() {
    let query = fixture("query-v1.json");
    let parsed: ActivityQuery = serde_json::from_value(query.clone()).unwrap();
    parsed.validate().unwrap();
    assert_eq!(serde_json::to_value(parsed).unwrap(), query);

    let result = fixture("result-v1.json");
    let parsed: ActivityResult = serde_json::from_value(result.clone()).unwrap();
    assert_eq!(serde_json::to_value(parsed).unwrap(), result);

    let receipt = fixture("receipt-v1.json");
    let parsed: ActivityReceipt = serde_json::from_value(receipt.clone()).unwrap();
    parsed.validate().unwrap();
    assert_eq!(serde_json::to_value(parsed).unwrap(), receipt);
}

#[test]
fn rejects_unknown_fields_bad_limit_path_and_source() {
    let mut query = fixture("query-v1.json");
    query["extra"] = json!("unknown");
    assert!(serde_json::from_value::<ActivityQuery>(query).is_err());
    let mut query = fixture("query-v1.json");
    query["limit"] = json!(501);
    assert!(
        serde_json::from_value::<ActivityQuery>(query)
            .unwrap()
            .validate()
            .is_err()
    );

    let mut receipt = fixture("receipt-v1.json");
    receipt["entity"]["locator"] = json!("../relative");
    assert!(
        serde_json::from_value::<ActivityReceipt>(receipt)
            .unwrap()
            .validate()
            .is_err()
    );
    let mut receipt = fixture("receipt-v1.json");
    receipt["source"] = json!("openAccepted");
    assert!(
        serde_json::from_value::<ActivityReceipt>(receipt)
            .unwrap()
            .validate()
            .is_err()
    );
}

#[test]
fn shared_privacy_and_stats_fixtures_round_trip() {
    let preferences = fixture("preferences-v1.json");
    let parsed: ActivityPreferences = serde_json::from_value(preferences.clone()).unwrap();
    parsed.validate().unwrap();
    assert_eq!(serde_json::to_value(parsed).unwrap(), preferences);

    let stats = fixture("stats-v1.json");
    let parsed: ActivityStats = serde_json::from_value(stats.clone()).unwrap();
    parsed.validate().unwrap();
    assert_eq!(serde_json::to_value(parsed).unwrap(), stats);
}

#[test]
fn rejects_invalid_privacy_and_inconsistent_stats() {
    let mut preferences = fixture("preferences-v1.json");
    preferences["retentionDays"] = json!(365);
    assert!(
        serde_json::from_value::<ActivityPreferences>(preferences)
            .unwrap()
            .validate()
            .is_err()
    );
    let mut preferences = fixture("preferences-v1.json");
    preferences["excludedRoots"] = json!(["relative"]);
    assert!(
        serde_json::from_value::<ActivityPreferences>(preferences)
            .unwrap()
            .validate()
            .is_err()
    );
    let mut preferences = fixture("preferences-v1.json");
    preferences["hiddenEntityIds"] = json!([""]);
    assert!(
        serde_json::from_value::<ActivityPreferences>(preferences)
            .unwrap()
            .validate()
            .is_err()
    );
    let mut stats = fixture("stats-v1.json");
    stats["episodeCount"] = json!(7);
    assert!(
        serde_json::from_value::<ActivityStats>(stats)
            .unwrap()
            .validate()
            .is_err()
    );
    let mut stats = fixture("stats-v1.json");
    stats["daily"][0]["utcDay"] = json!("not-a-day");
    assert!(
        serde_json::from_value::<ActivityStats>(stats)
            .unwrap()
            .validate()
            .is_err()
    );
}
