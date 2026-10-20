// FER-576: `pane.navigate`, `items.delete` and the chain grammar. The model cannot name an item to
// delete, cannot navigate to a path the user did not type, cannot make a permanent delete (there is
// no such field), and a chain is a short, ordered, closed sequence.
use super::*;
use serde_json::{Value, json};

const OFFERED: [&str; 4] = [
    "archive.create",
    PANE_NAVIGATE_CAPABILITY,
    ITEMS_DELETE_CAPABILITY,
    "directories.sync",
];
const TEXT: &str = r"d:\Downloads\Telegram Desktop\pc_20250814\pc_202508014.csv -> tolong arsipkan ke paling kecil gunakan 7zip lalu hapus ya";
const FILE: &str = r"D:\Downloads\Telegram Desktop\pc_20250814\pc_202508014.csv";

fn nav(path: &str) -> Value {
    json!({"capability": "pane.navigate", "pane": "activeAtSubmit", "path": path})
}
fn archive() -> Value {
    json!({
        "capability": "archive.create", "sourcePane": "activeAtSubmit",
        "selector": {"kind": "selected"}, "format": "7z", "level": "ultra",
        "target": {"kind": "sourceDirectory"}
    })
}
fn delete() -> Value {
    json!({"capability": "items.delete", "sourcePane": "activeAtSubmit", "selector": {"kind": "selected"}})
}
fn chain(steps: Vec<Value>) -> Value {
    json!({"schemaVersion": 1, "requestId": "req_1", "steps": steps})
}
fn parse_chain(steps: Vec<Value>) -> Result<ChainEnvelope, ContractError> {
    parse_chain_offered(chain(steps), "req_1", &OFFERED, TEXT)
}
fn single(intent: Value) -> Value {
    json!({"schemaVersion": 1, "requestId": "req_1", "intent": intent})
}

#[test]
fn the_owner_example_is_a_valid_three_step_chain() {
    let parsed = parse_chain(vec![nav(FILE), archive(), delete()]).unwrap();
    assert_eq!(parsed.steps.len(), 3);
    assert_eq!(
        serde_json::to_value(&parsed).unwrap(),
        chain(vec![nav(FILE), archive(), delete()])
    );
}

#[test]
fn navigate_then_delete_and_archive_then_delete_are_valid() {
    assert!(parse_chain(vec![nav(FILE), delete()]).is_ok());
    assert!(parse_chain(vec![nav(FILE), archive()]).is_ok());
    assert!(parse_chain(vec![archive(), delete()]).is_ok());
}

#[test]
fn a_chain_must_be_two_to_four_steps_in_order_and_each_once() {
    assert!(parse_chain(vec![]).is_err());
    assert!(parse_chain(vec![nav(FILE)]).is_err());
    assert!(parse_chain(vec![delete(), archive()]).is_err());
    assert!(parse_chain(vec![archive(), nav(FILE)]).is_err());
    assert!(parse_chain(vec![nav(FILE), nav(FILE), archive()]).is_err());
    assert!(parse_chain(vec![nav(FILE), archive(), delete(), delete()]).is_err());
    assert!(parse_chain(vec![nav(FILE), archive(), archive(), delete(), delete()]).is_err());
}

#[test]
fn a_chain_works_on_one_pane() {
    let mut other = delete();
    other["sourcePane"] = json!("left");
    assert!(parse_chain(vec![nav(FILE), archive(), other]).is_err());
}

#[test]
fn archive_then_delete_must_archive_the_selection() {
    let mut all = archive();
    all["selector"] = json!({"kind": "allVisible", "entries": "all"});
    assert!(parse_chain(vec![all, delete()]).is_err());
}

#[test]
fn only_navigate_archive_and_delete_may_be_chained() {
    let sync = json!({"capability": "directories.sync", "leftPane": "left", "rightPane": "right", "mode": "bidirectional"});
    assert!(parse_chain(vec![nav(FILE), sync]).is_err());
}

#[test]
fn a_chain_step_must_be_offered() {
    let offered = ["archive.create", PANE_NAVIGATE_CAPABILITY];
    let value = chain(vec![nav(FILE), archive(), delete()]);
    assert!(parse_chain_offered(value, "req_1", &offered, TEXT).is_err());
}

#[test]
fn a_chain_cannot_carry_extra_fields_or_the_wrong_request() {
    let mut value = chain(vec![nav(FILE), archive()]);
    value["approved"] = json!(true);
    assert!(parse_chain_offered(value, "req_1", &OFFERED, TEXT).is_err());
    let value = chain(vec![nav(FILE), archive()]);
    assert!(parse_chain_offered(value, "req_2", &OFFERED, TEXT).is_err());
    let mut value = chain(vec![nav(FILE), archive()]);
    value["schemaVersion"] = json!(2);
    assert!(parse_chain_offered(value, "req_1", &OFFERED, TEXT).is_err());
}

#[test]
fn a_navigation_path_the_user_did_not_type_is_rejected() {
    for path in [
        r"D:\Windows\System32",
        r"C:\Users\someone\Documents",
        r"D:\Downloads\Telegram Desktop\pc_20250814\other.csv",
        r"D:\Downloads\Telegram Desktop\pc_20250814\pc_2025080",
    ] {
        assert!(
            parse_chain(vec![nav(path), archive()]).is_err(),
            "accepted {path}"
        );
    }
}

#[test]
fn typed_path_matching_ignores_case_slashes_quotes_and_a_parent_prefix() {
    assert!(path_typed_by_user(FILE, TEXT));
    assert!(path_typed_by_user(
        "d:/downloads/telegram desktop/pc_20250814/pc_202508014.csv",
        TEXT
    ));
    assert!(path_typed_by_user(
        r"D:\Downloads\Telegram Desktop\pc_20250814\",
        TEXT
    ));
    assert!(path_typed_by_user(r"D:\Downloads\Telegram Desktop", TEXT));
    assert!(path_typed_by_user(
        r"D:\x\a.csv",
        r#"buka "D:\x\a.csv", lalu hapus"#
    ));
    assert!(path_typed_by_user(r"D:\x\a.csv", "buka (D:\\x\\a.csv)"));
    // Not at a token start, or cut in the middle of a name.
    assert!(!path_typed_by_user(r"D:\x\a.csv", r"abcD:\x\a.csv"));
    assert!(!path_typed_by_user(r"D:\x\a", r"buka D:\x\a.csv"));
    assert!(!path_typed_by_user(r"D:\", "tidak ada path di sini"));
    assert!(!path_typed_by_user("", TEXT));
}

#[test]
fn navigate_paths_must_be_absolute_local_and_clean() {
    for good in [
        r"D:\a\b.csv",
        r"d:/a/b",
        r"C:",
        r"C:\",
        r"E:\Folder With Spaces\x",
    ] {
        assert!(safe_navigate_path(good), "{good}");
    }
    for bad in [
        "",
        "relative\\path",
        r"\\server\share\x",
        r"\\?\C:\x",
        r"\\.\C:\x",
        r"C:\x\..\y",
        r"C:\x\.\y",
        r"C:\x:ads",
        r"C:\nul",
        r"C:\a\con.txt",
        r"C:\a\b ",
        r"C:\a\b.",
        r"C:\a\\b",
        "C:\\a\u{0}b",
        "C:x",
        "1:\\x",
    ] {
        assert!(!safe_navigate_path(bad), "{bad}");
    }
    assert!(!safe_navigate_path(&format!(r"C:\{}", "a".repeat(2000))));
}

#[test]
fn navigate_and_delete_are_rejected_by_the_legacy_parser() {
    assert!(parse_intent(single(nav(FILE)), "req_1").is_err());
    assert!(parse_intent(single(delete()), "req_1").is_err());
    assert!(parse_intent_offered(single(nav(FILE)), "req_1", &OFFERED).is_ok());
    assert!(parse_intent_offered(single(delete()), "req_1", &OFFERED).is_ok());
    assert!(parse_intent_offered(single(delete()), "req_1", &["archive.create"]).is_err());
}

#[test]
fn delete_has_no_way_to_name_an_item_or_make_it_permanent() {
    for extra in [
        ("permanent", json!(true)),
        ("paths", json!([r"C:\x"])),
        ("path", json!(r"C:\x")),
        ("names", json!(["a.txt"])),
        ("recycle", json!(false)),
        ("force", json!(true)),
    ] {
        let mut intent = delete();
        intent[extra.0] = extra.1;
        assert!(
            parse_intent_offered(single(intent), "req_1", &OFFERED).is_err(),
            "accepted {}",
            extra.0
        );
    }
    for selector in [
        json!({"kind": "allVisible", "entries": "all"}),
        json!({"kind": "directoryContents", "includeHidden": true, "entries": "all"}),
        json!({"kind": "selected", "extensions": ["csv"]}),
        json!({"kind": "selected", "names": ["a.txt"]}),
        json!({"kind": "entity", "entityId": "x"}),
    ] {
        let mut intent = delete();
        intent["selector"] = selector.clone();
        assert!(
            parse_intent_offered(single(intent), "req_1", &OFFERED).is_err(),
            "accepted {selector}"
        );
    }
    let mut missing = delete();
    missing.as_object_mut().unwrap().remove("selector");
    assert!(parse_intent_offered(single(missing), "req_1", &OFFERED).is_err());
}

#[test]
fn navigate_has_no_way_to_open_or_run_anything() {
    for extra in [
        ("open", json!(true)),
        ("run", json!(true)),
        ("args", json!(["x"])),
        ("select", json!("a.txt")),
    ] {
        let mut intent = nav(FILE);
        intent[extra.0] = extra.1;
        assert!(parse_intent_offered(single(intent), "req_1", &OFFERED).is_err());
    }
}

#[test]
fn a_plan_for_delete_round_trips_and_a_conflict_is_refused() {
    let snapshot = json!({"schemaVersion":1,"requestId":"req_1","activePaneAtSubmit":"left","panes":[
        {"paneId":"left","tabId":"tab_left","location":{"kind":"disk","path":"C:\\left"},
         "contextRevision":2,"selectionRevision":3,"selectionHandle":"sel_left","filter":"",
         "showHidden":false,"sort":{"key":"name","dir":1}},
        {"paneId":"right","tabId":"tab_right","location":{"kind":"disk","path":"C:\\right"},
         "contextRevision":4,"selectionRevision":1,"selectionHandle":"sel_right","filter":"",
         "showHidden":false,"sort":{"key":"modified","dir":-1}}]});
    let plan = |conflicts: u64| {
        json!({"schemaVersion":1,"planId":"plan_1","requestId":"req_1","revision":1,
            "capability":"items.delete","sourceSnapshot":snapshot,
            "operation":{"kind":"items.delete","sourceManifest":"manifest_1","sourceDirectory":"C:\\left"},
            "preview":{"itemCount":2,"conflictCount":conflicts,"totalBytes":0},
            "steps":[{"id":"step_1","capability":"items.delete","dependsOn":[]}],
            "previewDigest":"digest_1","risk":"mutating","requiresApproval":true,"stepCount":1})
    };
    let value = plan(0);
    let parsed = parse_resolved_plan(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(parsed).unwrap(), value);
    assert!(parse_resolved_plan(plan(1)).is_err());
    let mut wrong = plan(0);
    wrong["capability"] = json!("archive.create");
    assert!(parse_resolved_plan(wrong).is_err());
    let mut unapproved = plan(0);
    unapproved["requiresApproval"] = json!(false);
    assert!(parse_resolved_plan(unapproved).is_err());
}

fn offered_of(case: &Value) -> Vec<String> {
    case["offered"]
        .as_array()
        .unwrap()
        .iter()
        .map(|id| id.as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn shared_chain_fixtures_match_the_rust_contract() {
    let cases: Vec<Value> = serde_json::from_str(include_str!(
        "../../../contract-fixtures/ai/chain-cases.json"
    ))
    .unwrap();
    assert!(cases.len() >= 30);
    for case in cases {
        let offered = offered_of(&case);
        let refs: Vec<&str> = offered.iter().map(String::as_str).collect();
        let value = case["value"].clone();
        let parsed = match case["kind"].as_str().unwrap() {
            "chain" => {
                parse_chain_offered(value, "req_1", &refs, case["userText"].as_str().unwrap())
                    .map(|chain| serde_json::to_value(chain).unwrap())
            }
            "intent" => parse_intent_offered(value, "req_1", &refs)
                .map(|envelope| serde_json::to_value(envelope).unwrap()),
            other => panic!("unknown kind {other}"),
        };
        assert_eq!(
            parsed.is_ok(),
            case["valid"].as_bool().unwrap(),
            "{}: {parsed:?}",
            case["name"]
        );
        if let Ok(round_trip) = parsed {
            assert_eq!(round_trip, case["value"], "{}", case["name"]);
        }
    }
}

#[test]
fn shared_navigate_fixtures_match_the_rust_matcher() {
    let cases: Vec<Value> = serde_json::from_str(include_str!(
        "../../../contract-fixtures/ai/navigate-cases.json"
    ))
    .unwrap();
    assert!(cases.len() >= 15);
    for case in cases {
        assert_eq!(
            path_typed_by_user(
                case["path"].as_str().unwrap(),
                case["text"].as_str().unwrap()
            ),
            case["typed"].as_bool().unwrap(),
            "{}",
            case["name"]
        );
    }
}
