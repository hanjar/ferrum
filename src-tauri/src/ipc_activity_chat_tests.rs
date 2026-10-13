// ACT-08 R1: `activity_chat_query` IPC body (`activity_chat_query_on`) against a real
// ActivityService on a temp dir. The wire type is `ai::contract::ActivityQuery`, deserialized
// here with serde_json exactly as Tauri does for the `request` argument.
use super::*;
use crate::activity_chat_tool::ChatResponse;
use crate::activity_service::ActivityService;
use crate::ai::contract::ActivityQuery;
use crate::domain::ErrorCode;
use serde_json::{Value, json};

const DAY: u64 = 86_400_000;

fn open() -> (tempfile::TempDir, ActivityService) {
    let dir = tempfile::tempdir().unwrap();
    let service = ActivityService::open(dir.path()).unwrap();
    service.flush();
    (dir, service)
}

fn visit(service: &ActivityService, path: &str, kind: VisitKind, ago_days: u64) {
    let now = service.chat_context().unwrap().now_utc_ms;
    service
        .record_at_for_test(path, kind, now - ago_days * DAY)
        .unwrap();
}

fn wire(value: Value) -> Result<ActivityQuery, String> {
    serde_json::from_value(value).map_err(|e| e.to_string())
}

fn ask(service: &ActivityService, value: Value) -> Result<ChatResponse, FsError> {
    activity_chat_query_on(Some(service), &wire(value).expect("valid wire"))
}

fn recent(days: u16) -> Value {
    json!({"query":{"kind":"recent","periodDays":days,"entity":"folder"}})
}

fn frequent(days: u16) -> Value {
    json!({"query":{"kind":"frequent","periodDays":days,"entity":"folder"}})
}

fn find(name: &str) -> Value {
    json!({"query":{"kind":"findByName","name":name,"entity":"folder"}})
}

fn entry_count(response: &ChatResponse) -> usize {
    match response {
        ChatResponse::Entities { entries, .. } => entries.len(),
        other => panic!("{other:?}"),
    }
}

#[test]
fn wire_is_strict_unknown_fields_and_wrong_shapes_never_deserialize() {
    for value in [
        json!({"query":{"kind":"recent","periodDays":7,"entity":"folder"},"workspaceRoot":"C:\\"}),
        json!({"query":{"kind":"recent","periodDays":7,"entity":"folder","path":"C:\\Work"}}),
        json!({"query":{"kind":"recent","periodDays":7,"entity":"folder","name":"x"}}),
        json!({"query":{"kind":"findByName","name":"x","entity":"folder","periodDays":7}}),
        json!({"query":{"kind":"overview","periodDays":7,"entity":"folder"}}),
        json!({"query":{"kind":"recent","periodDays":7}}),
        json!({"query":{"kind":"recent","periodDays":7,"entity":"folder"},"limit":null}),
        json!({"query":{"kind":"recent","periodDays":7,"entity":"folder"},"limit":-1}),
        json!({"query":{"kind":"recent","periodDays":7,"entity":"folder"},"limit":300}),
        json!({"query":{"kind":"recent","periodDays":"7","entity":"folder"}}),
        json!({"kind":"recent","periodDays":7,"entity":"folder"}),
        json!({}),
    ] {
        assert!(wire(value.clone()).is_err(), "deserialized {value}");
    }
}

#[test]
fn semantic_rules_reject_before_touching_the_store() {
    let (_dir, service) = open();
    let mut bad = vec![
        json!({"query":{"kind":"recent","periodDays":7,"entity":"folder"},"limit":21}),
        json!({"query":{"kind":"recent","periodDays":7,"entity":"folder"},"limit":0}),
        recent(14),
        frequent(1),
        frequent(365),
    ];
    for name in ["", "  ", r"C:\Work", "Work/Invoice", "C:", "a\u{7}b"] {
        bad.push(find(name));
    }
    bad.push(find(&"n".repeat(256)));
    for value in bad {
        let query = wire(value.clone()).expect("shape is valid, values are not");
        let error = activity_chat_query_on(Some(&service), &query).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidPath, "{value}");
        // Invalid input is rejected even when the store is gone.
        assert!(activity_chat_query_on(None, &query).is_err(), "{value}");
    }
}

#[test]
fn limit_defaults_to_ten_and_caps_at_twenty() {
    let (_dir, service) = open();
    for i in 0..25 {
        visit(
            &service,
            &format!(r"C:\Work\Dir{i:02}"),
            VisitKind::Folder,
            1,
        );
    }
    let response = ask(&service, recent(7)).unwrap();
    assert_eq!(entry_count(&response), 10);
    assert!(matches!(
        response,
        ChatResponse::Entities {
            truncated: true,
            ..
        }
    ));
    let mut twenty = recent(30);
    twenty["limit"] = json!(20);
    assert_eq!(entry_count(&ask(&service, twenty).unwrap()), 20);
    let mut three = recent(90);
    three["limit"] = json!(3);
    assert_eq!(entry_count(&ask(&service, three).unwrap()), 3);
}

#[test]
fn file_and_folder_entities_are_separate() {
    let (_dir, service) = open();
    visit(&service, r"C:\Docs\Plan", VisitKind::Folder, 1);
    visit(&service, r"C:\Docs\report.pdf", VisitKind::FileOpen, 1);
    let files = json!({"query":{"kind":"recent","periodDays":7,"entity":"file"}});
    let ChatResponse::Entities { entries, .. } = ask(&service, files).unwrap() else {
        panic!()
    };
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].display_name, "report.pdf");
    match ask(&service, find("Plan")).unwrap() {
        ChatResponse::Resolved { entry } => assert_eq!(entry.display_name, "Plan"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn paused_history_is_withheld_and_missing_store_is_unavailable() {
    let (_dir, service) = open();
    visit(&service, r"C:\Work\A", VisitKind::Folder, 1);
    service.set_recording(false).unwrap();
    for value in [recent(7), frequent(30), find("A")] {
        let response = ask(&service, value).unwrap();
        assert_eq!(response, ChatResponse::Withheld);
        assert_eq!(
            serde_json::to_value(&response).unwrap(),
            json!({"status":"withheld"})
        );
    }
    service.set_recording(true).unwrap();
    assert_eq!(entry_count(&ask(&service, recent(7)).unwrap()), 1);
    let query = wire(recent(7)).unwrap();
    assert_eq!(
        activity_chat_query_on(None, &query).unwrap(),
        ChatResponse::Unavailable
    );
}

#[test]
fn ipc_queries_do_not_change_episodes_or_ranking() {
    let (_dir, service) = open();
    visit(&service, r"C:\Work\Alpha", VisitKind::Folder, 3);
    visit(&service, r"C:\Work\Alpha", VisitKind::Folder, 2);
    visit(&service, r"C:\Work\Beta", VisitKind::Folder, 1);
    visit(&service, r"C:\Other\Beta", VisitKind::Folder, 1);
    visit(&service, r"C:\Work\note.txt", VisitKind::FileOpen, 1);
    service.flush();
    let snapshot = |service: &ActivityService| {
        let mut out = Vec::new();
        for kind in [ActivityKind::FolderVisit, ActivityKind::FileOpenRequested] {
            // Full serialized items: counts, active days, scores and timestamps included.
            out.push(serde_json::to_string(&service.frequent(kind.clone(), 20).unwrap()).unwrap());
            out.push(serde_json::to_string(&service.recent(kind, 20).unwrap()).unwrap());
        }
        out
    };
    let before_count = service.episode_count_for_test().unwrap();
    let before = snapshot(&service);
    let before_state = service.state().unwrap();
    let first = ask(&service, frequent(30)).unwrap();
    assert_eq!(entry_count(&first), 1, "only Alpha has two active days");
    for _ in 0..25 {
        for value in [
            frequent(30),
            recent(90),
            find("Beta"),
            find("Alpha"),
            json!({"query":{"kind":"recent","periodDays":7,"entity":"file"}}),
        ] {
            let _ = ask(&service, value).unwrap();
        }
    }
    service.flush();
    assert_eq!(service.episode_count_for_test().unwrap(), before_count);
    assert_eq!(snapshot(&service), before);
    assert_eq!(
        service.state().unwrap().privacy_epoch,
        before_state.privacy_epoch
    );
    assert_eq!(ask(&service, frequent(30)).unwrap(), first);
}

#[test]
fn locate_gives_the_local_path_of_a_listed_entity_only_and_records_nothing() {
    let (_dir, service) = open();
    visit(&service, r"C:\Work\Alpha", VisitKind::Folder, 1);
    visit(&service, r"C:\Work\note.txt", VisitKind::FileOpen, 1);
    service.flush();
    let ChatResponse::Resolved { entry } = ask(&service, find("Alpha")).unwrap() else {
        panic!()
    };
    let before = service.episode_count_for_test().unwrap();
    let epoch = service.state().unwrap().privacy_epoch;
    let location = activity_chat_locate_on(Some(&service), &entry.id).expect("listed entity");
    assert_eq!(location.path, r"C:\Work\Alpha");
    assert_eq!(location.parent_path.trim_end_matches('\\'), r"C:\Work");
    assert_eq!(location.kind, ferrum_activity_contract::EntityKind::Folder);
    let value = serde_json::to_value(&location).unwrap();
    let mut keys: Vec<&str> = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(keys, ["kind", "parentPath", "path"]);
    // Unknown, blank and oversized ids, and a missing store, give nothing.
    for id in ["", "  ", "nope", &"x".repeat(129)] {
        assert!(
            activity_chat_locate_on(Some(&service), id).is_none(),
            "{id}"
        );
    }
    assert!(activity_chat_locate_on(None, &entry.id).is_none());
    // Hidden and paused history are not located.
    service.set_hidden(&entry.id, true).unwrap();
    assert!(activity_chat_locate_on(Some(&service), &entry.id).is_none());
    service.set_hidden(&entry.id, false).unwrap();
    service.set_recording(false).unwrap();
    assert!(activity_chat_locate_on(Some(&service), &entry.id).is_none());
    service.set_recording(true).unwrap();
    for _ in 0..10 {
        assert!(activity_chat_locate_on(Some(&service), &entry.id).is_some());
    }
    service.flush();
    assert_eq!(service.episode_count_for_test().unwrap(), before);
    // Only the pause/resume above moved the epoch; locating itself never does.
    let after = service.state().unwrap().privacy_epoch;
    service.flush();
    let _ = activity_chat_locate_on(Some(&service), &entry.id);
    assert_eq!(service.state().unwrap().privacy_epoch, after);
    assert!(after >= epoch);
}

#[test]
fn json_output_has_pinned_keys_and_no_locator_count_score_or_timestamp() {
    let (_dir, service) = open();
    let locators = [
        r"C:\Clients\Acme\Quarterly Reports\Reports",
        r"C:\Clients\Globex\Quarterly Reports\Reports",
        r"D:\Very Long Parent Folder Name For Testing\Second Level Directory\Third\Invoices",
        r"C:\Work\budget.xlsx",
    ];
    visit(&service, locators[0], VisitKind::Folder, 1);
    visit(&service, locators[1], VisitKind::Folder, 2);
    visit(&service, locators[2], VisitKind::Folder, 2);
    visit(&service, locators[2], VisitKind::Folder, 3);
    visit(&service, locators[3], VisitKind::FileOpen, 1);
    let responses = [
        ask(&service, recent(7)).unwrap(),
        ask(&service, frequent(30)).unwrap(),
        ask(&service, find("Reports")).unwrap(),
        ask(&service, find("Invoices")).unwrap(),
        ask(
            &service,
            json!({"query":{"kind":"recent","periodDays":7,"entity":"file"}}),
        )
        .unwrap(),
    ];
    assert!(matches!(responses[2], ChatResponse::Ambiguous { .. }));
    assert!(matches!(responses[3], ChatResponse::Resolved { .. }));
    let allowed = [
        "id",
        "displayName",
        "parentHint",
        "kind",
        "availability",
        "reasons",
    ];
    for response in &responses {
        let value = serde_json::to_value(response).unwrap();
        let text = value.to_string();
        for locator in locators {
            assert!(!text.contains(locator), "raw locator in {text}");
            let escaped = serde_json::to_string(locator).unwrap();
            assert!(
                !text.contains(escaped.trim_matches('"')),
                "locator in {text}"
            );
        }
        for forbidden in [
            "locator",
            "episode",
            "count",
            "score",
            "lastUtc",
            "UtcMs",
            "activeDays",
            "path\"",
        ] {
            assert!(!text.contains(forbidden), "{forbidden} in {text}");
        }
        let entities: Vec<&Value> = match value["status"].as_str().unwrap() {
            "entities" => value["entries"].as_array().unwrap().iter().collect(),
            "ambiguous" => value["candidates"].as_array().unwrap().iter().collect(),
            "resolved" => vec![&value["entry"]],
            other => panic!("{other}"),
        };
        assert!(!entities.is_empty());
        for entity in entities {
            let keys: Vec<&str> = entity
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            assert_eq!(keys.len(), allowed.len(), "{entity}");
            assert!(keys.iter().all(|k| allowed.contains(k)), "{entity}");
        }
    }
    // Listed entries carry a tail-truncated parent hint, never the long parent path.
    let ChatResponse::Entities { entries, .. } = &responses[1] else {
        panic!()
    };
    let invoices = entries
        .iter()
        .find(|e| e.display_name == "Invoices")
        .unwrap();
    assert!(invoices.parent_hint.starts_with('\u{2026}'));
    assert!(invoices.parent_hint.chars().count() <= 49);
}
