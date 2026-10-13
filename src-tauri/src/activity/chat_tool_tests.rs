use super::*;
use ferrum_activity_contract::{
    ActivityEntity, ActivityKind, ActivityReceipt, Availability, EntityKind, Provenance,
    ReceiptSource,
};
use ferrum_activity_store::ActivityStore;
use std::collections::HashSet;

const DAY: u64 = 86_400_000;
const NOW: u64 = 200 * DAY;

fn receipt(id: &str, name: &str, parent: &str, at: u64, epoch: u64) -> ActivityReceipt {
    ActivityReceipt {
        version: 1,
        event_id: format!("ev-{id}-{at}"),
        action_id: format!("ac-{id}-{at}"),
        privacy_epoch: epoch,
        source: ReceiptSource::NavigationCommit,
        kind: ActivityKind::FolderVisit,
        provenance: Provenance::Manual,
        entity: ActivityEntity {
            id: id.into(),
            kind: EntityKind::Folder,
            provider: "local".into(),
            volume_id: "C:".into(),
            locator: format!("{parent}\\{name}"),
            parent_path: parent.into(),
            display_name: name.into(),
            availability: Availability::Available,
        },
        occurred_utc_ms: at,
    }
}

fn store_with(rows: &[(&str, &str, &str, u64)]) -> ActivityStore {
    let mut store = ActivityStore::in_memory().unwrap();
    for (id, name, parent, at) in rows {
        store.record(&receipt(id, name, parent, *at, 0)).unwrap();
    }
    store
}

fn ctx<'a>(hidden: &'a HashSet<String>, excluded: &'a [String]) -> ChatContext<'a> {
    ChatContext {
        now_utc_ms: NOW,
        private_session: false,
        expected_epoch: None,
        hidden_entity_ids: hidden,
        excluded_roots: excluded,
    }
}

fn run(store: &mut ActivityStore, request: ChatRequest) -> ChatResponse {
    let hidden = HashSet::new();
    run_chat_tool(store, &request, &ctx(&hidden, &[]))
}

fn entries(response: ChatResponse) -> Vec<ChatEntityRef> {
    match response {
        ChatResponse::Entities { entries, .. } => entries,
        other => panic!("expected entities, got {other:?}"),
    }
}

fn req(kind: ChatRequestKind, limit: Option<u16>) -> ChatRequest {
    ChatRequest { kind, limit }
}

fn recent(days: u16) -> ChatRequestKind {
    ChatRequestKind::Recent { days, file: false }
}

fn entries_of(store: &mut ActivityStore, limit: Option<u16>) -> Vec<ChatEntityRef> {
    entries(run(store, req(recent(7), limit)))
}

fn find(query: &str) -> ChatRequestKind {
    ChatRequestKind::FindByName {
        query: query.into(),
        file: false,
    }
}

#[test]
fn recent_respects_period_and_orders_newest_first() {
    let mut store = store_with(&[
        ("old", "Old", "C:\\Work", NOW - 40 * DAY),
        ("mid", "Mid", "C:\\Work", NOW - 10 * DAY),
        ("new", "New", "C:\\Work", NOW - DAY),
    ]);
    let seven = entries(run(&mut store, req(recent(7), None)));
    assert_eq!(
        seven.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
        ["new"]
    );
    let thirty = entries(run(&mut store, req(recent(30), None)));
    assert_eq!(
        thirty.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
        ["new", "mid"]
    );
    assert_eq!(thirty[0].reasons, vec![ReasonCode::Recent]);
    let ninety = entries(run(&mut store, req(recent(90), None)));
    assert_eq!(ninety.len(), 3);
}

#[test]
fn unsupported_period_is_rejected_not_rounded() {
    let mut store = store_with(&[]);
    let response = run(&mut store, req(recent(14), None));
    assert!(
        matches!(response, ChatResponse::Rejected { .. }),
        "{response:?}"
    );
}

#[test]
fn frequent_needs_two_active_days_and_labels_reason() {
    let mut store = store_with(&[
        ("a", "Alpha", "C:\\Work", NOW - 3 * DAY),
        ("a", "Alpha", "C:\\Work", NOW - 2 * DAY),
        ("b", "Beta", "C:\\Work", NOW - 2 * DAY),
    ]);
    let rows = entries(run(
        &mut store,
        req(
            ChatRequestKind::Frequent {
                days: 30,
                file: false,
            },
            None,
        ),
    ));
    assert_eq!(
        rows.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
        ["a"]
    );
    assert_eq!(rows[0].reasons, vec![ReasonCode::Frequent]);
}

#[test]
fn limit_is_clamped_and_truncation_flagged() {
    let mut store = ActivityStore::in_memory().unwrap();
    for i in 0..30u64 {
        let id = format!("id{i:02}");
        let name = format!("Dir{i:02}");
        store
            .record(&receipt(&id, &name, "C:\\Work", NOW - 1000 - i, 0))
            .unwrap();
    }
    let response = run(&mut store, req(recent(7), Some(500)));
    let ChatResponse::Entities { entries, truncated } = response else {
        panic!()
    };
    assert_eq!(entries.len(), usize::from(MAX_RESULTS));
    assert!(truncated);
    assert_eq!(entries_of(&mut store, Some(1)).len(), 1);
    let zero = run(&mut store, req(recent(7), Some(0)));
    assert!(matches!(zero, ChatResponse::Rejected { .. }));
}

#[test]
fn name_with_duplicates_returns_ambiguous_with_distinct_parent_hints() {
    let mut store = store_with(&[
        ("r1", "Reports", "C:\\Clients\\Acme", NOW - DAY),
        ("r2", "Reports", "C:\\Clients\\Globex", NOW - 2 * DAY),
        ("other", "Budget", "C:\\Clients\\Acme", NOW - DAY),
    ]);
    let response = run(&mut store, req(find("Reports"), None));
    let ChatResponse::Ambiguous { candidates } = response else {
        panic!("must not pick silently: {response:?}")
    };
    assert_eq!(candidates.len(), 2);
    let hints: HashSet<_> = candidates.iter().map(|c| c.parent_hint.clone()).collect();
    assert_eq!(hints.len(), 2, "parent hints must differ: {candidates:?}");
    assert!(candidates.iter().all(|c| c.display_name == "Reports"));
}

#[test]
fn identical_parent_tails_are_still_distinguished() {
    let shared = "Shared\\Tail";
    let a = format!("C:\\A_{}\\{shared}", "x".repeat(60));
    let b = format!("C:\\B_{}\\{shared}", "x".repeat(60));
    let mut store = store_with(&[
        ("r1", "Reports", &a, NOW - DAY),
        ("r2", "Reports", &b, NOW - 2 * DAY),
    ]);
    let ChatResponse::Ambiguous { candidates } = run(&mut store, req(find("Reports"), None)) else {
        panic!()
    };
    let hints: HashSet<_> = candidates.iter().map(|c| c.parent_hint.clone()).collect();
    assert_eq!(hints.len(), 2, "{candidates:?}");
}

#[test]
fn unique_name_resolves_and_missing_name_is_not_found() {
    let mut store = store_with(&[("r1", "Reports", "C:\\Clients\\Acme", NOW - DAY)]);
    match run(&mut store, req(find("reports"), None)) {
        ChatResponse::Resolved { entry } => {
            assert_eq!(entry.id, "r1");
            assert_eq!(entry.reasons, vec![ReasonCode::ExactName]);
        }
        other => panic!("{other:?}"),
    }
    let none = run(&mut store, req(find("zzzzqqqq"), None));
    assert_eq!(none, ChatResponse::NotFound);
    let empty = run(&mut store, req(find("  "), None));
    assert!(matches!(empty, ChatResponse::Rejected { .. }));
}

#[test]
fn disclosure_is_minimal_and_never_contains_locator_or_full_parent() {
    let long_parent = format!("C:\\{}Deep", "VeryLongSegmentName\\".repeat(10));
    let mut store = store_with(&[("d1", "Secret Plans", &long_parent, NOW - DAY)]);
    let response = run(&mut store, req(recent(7), None));
    let json = serde_json::to_string(&response).unwrap();
    assert!(!json.contains("locator"));
    assert!(!json.contains(&long_parent.replace('\\', "\\\\")), "{json}");
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    let entry = &value["entries"][0];
    let mut keys: Vec<_> = entry.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "availability",
            "displayName",
            "id",
            "kind",
            "parentHint",
            "reasons"
        ]
    );
    let hint = entry["parentHint"].as_str().unwrap();
    assert!(hint.chars().count() <= PARENT_HINT_MAX_CHARS + 1);
    assert!(hint.starts_with('\u{2026}'));
    assert!(hint.ends_with("Deep"));
}

#[test]
fn hidden_and_excluded_entities_never_appear() {
    let mut store = store_with(&[
        ("keep", "Keep", "C:\\Work", NOW - DAY),
        ("hid", "Hidden Dir", "C:\\Work", NOW - DAY),
        ("exc", "Excluded Dir", "C:\\Private\\Stuff", NOW - DAY),
    ]);
    let hidden: HashSet<String> = ["hid".to_owned()].into();
    let excluded = vec!["C:\\Private".to_owned()];
    let context = ctx(&hidden, &excluded);
    let recent_rows = run_chat_tool(&mut store, &req(recent(7), None), &context);
    let ids: Vec<_> = entries(recent_rows).into_iter().map(|e| e.id).collect();
    assert_eq!(ids, ["keep"]);
    for name in ["Hidden Dir", "Excluded Dir"] {
        let found = run_chat_tool(&mut store, &req(find(name), None), &context);
        assert_eq!(found, ChatResponse::NotFound, "{name}");
    }
    // Subtree boundary: a sibling sharing only a name prefix is not excluded.
    assert!(is_excluded("C:\\Private\\Stuff", &excluded));
    assert!(is_excluded("c:\\private", &excluded));
    assert!(!is_excluded("C:\\PrivateOther\\x", &excluded));
}

#[test]
fn hidden_entries_do_not_starve_the_limit() {
    let mut store = ActivityStore::in_memory().unwrap();
    for i in 0..5u64 {
        let id = format!("id{i}");
        store
            .record(&receipt(&id, &id, "C:\\Work", NOW - 1000 - i, 0))
            .unwrap();
    }
    let hidden: HashSet<String> = ["id0".to_owned(), "id1".to_owned()].into();
    let context = ctx(&hidden, &[]);
    let rows = entries(run_chat_tool(
        &mut store,
        &req(recent(7), Some(2)),
        &context,
    ));
    assert_eq!(
        rows.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
        ["id2", "id3"]
    );
}

#[test]
fn deleted_history_is_not_returned_and_private_session_withholds() {
    let mut store = store_with(&[("a", "Alpha", "C:\\Work", NOW - DAY)]);
    let hidden = HashSet::new();
    let mut context = ctx(&hidden, &[]);
    context.private_session = true;
    let withheld = run_chat_tool(&mut store, &req(recent(7), None), &context);
    assert_eq!(withheld, ChatResponse::Withheld);
    assert_eq!(entries_of(&mut store, None).len(), 1);
    store.forget_entity("a").unwrap();
    assert!(entries_of(&mut store, None).is_empty());
    assert_eq!(
        run(&mut store, req(find("Alpha"), None)),
        ChatResponse::NotFound
    );
    let _ = store.clear_all().unwrap();
    assert!(entries_of(&mut store, None).is_empty());
}

#[test]
fn ai_queries_do_not_change_episodes_or_ranking() {
    let mut store = store_with(&[
        ("a", "Alpha", "C:\\Work", NOW - 3 * DAY),
        ("a", "Alpha", "C:\\Work", NOW - 2 * DAY),
        ("b", "Beta", "C:\\Work", NOW - DAY),
        ("b2", "Beta", "C:\\Other", NOW - DAY),
    ]);
    let frequent = ChatRequestKind::Frequent {
        days: 30,
        file: false,
    };
    let before_count = store.episode_count().unwrap();
    let before_freq = run(&mut store, req(frequent.clone(), None));
    let before_recent = entries_of(&mut store, None);
    for _ in 0..25 {
        let _ = run(&mut store, req(frequent.clone(), None));
        let _ = run(&mut store, req(find("Beta"), None));
        let _ = run(&mut store, req(recent(90), None));
    }
    assert_eq!(store.episode_count().unwrap(), before_count);
    assert_eq!(run(&mut store, req(frequent, None)), before_freq);
    assert_eq!(entries_of(&mut store, None), before_recent);
}

#[test]
fn followup_is_reference_only_and_requires_prepared_plan() {
    let mut store = store_with(&[("a", "Alpha", "C:\\Work", NOW - DAY)]);
    let entry = entries_of(&mut store, None).remove(0);
    let reference = follow_up_reference(&entry);
    assert_eq!(reference.entity_id, "a");
    assert!(reference.requires_prepared_plan);
    assert!(FOLLOW_UP_POLICY.contains("prepared plan"));
    let json = serde_json::to_string(&reference).unwrap();
    assert!(!json.contains("C:\\"));
}

#[test]
fn module_source_has_no_mutation_calls() {
    let source = include_str!("chat_tool.rs");
    for forbidden in [
        ".record(",
        "forget_entity",
        "exclude_root",
        "clear_all",
        "set_recording_enabled",
        "add_favorite",
        "remove_favorite",
        "purge_before",
        ".execute(",
        "std::fs",
        "Command::new",
    ] {
        assert!(
            !source.contains(forbidden),
            "chat_tool must stay read-only: {forbidden}"
        );
    }
}

// ---- real adapter over ActivityService (ServiceReader + chat_context) ----

mod service_adapter {
    use super::*;
    use crate::activity_service::{ActivityService, VisitKind};
    use ferrum_activity_store::FsChangeKind;

    fn open() -> (tempfile::TempDir, ActivityService) {
        let dir = tempfile::tempdir().unwrap();
        let service = ActivityService::open(dir.path()).unwrap();
        service.flush();
        (dir, service)
    }

    fn visit(service: &ActivityService, path: &str, ago_days: u64) {
        let now = service.chat_context().unwrap().now_utc_ms;
        service
            .record_at_for_test(path, VisitKind::Folder, now - ago_days * DAY)
            .unwrap();
    }

    fn id_of(service: &ActivityService, name: &str) -> String {
        service
            .recent(ActivityKind::FolderVisit, 50)
            .unwrap()
            .into_iter()
            .find(|item| item.entity.display_name == name)
            .unwrap_or_else(|| panic!("{name} not in recent"))
            .entity
            .id
    }

    fn ask(service: &ActivityService, kind: ChatRequestKind) -> ChatResponse {
        let snapshot = service.chat_context().unwrap();
        let mut reader = service.chat_reader(None);
        run_chat_tool(&mut reader, &req(kind, None), &snapshot.context())
    }

    fn ids(response: ChatResponse) -> Vec<String> {
        entries(response).into_iter().map(|e| e.id).collect()
    }

    #[test]
    fn hidden_forgotten_excluded_and_missing_items_never_appear() {
        let (_dir, service) = open();
        for path in [
            r"C:\Work\Keep",
            r"C:\Work\Hid Dir",
            r"C:\Work\Forgotten Dir",
            r"C:\Private\Excluded Dir",
            r"C:\Work\Gone Dir",
        ] {
            visit(&service, path, 1);
        }
        service
            .set_hidden(&id_of(&service, "Hid Dir"), true)
            .unwrap();
        service.forget(&id_of(&service, "Forgotten Dir")).unwrap();
        service.exclude(r"C:\Private").unwrap();
        service.note_fs_changes(FsChangeKind::Delete, &[(r"C:\Work\Gone Dir".into(), None)]);
        service.flush();

        let keep = id_of(&service, "Keep");
        assert_eq!(ids(ask(&service, recent(7))), std::slice::from_ref(&keep));
        for name in ["Hid Dir", "Forgotten Dir", "Excluded Dir", "Gone Dir"] {
            assert_eq!(ask(&service, find(name)), ChatResponse::NotFound, "{name}");
        }
        match ask(&service, find("Keep")) {
            ChatResponse::Resolved { entry } => assert_eq!(entry.id, keep),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn snapshot_carries_hidden_ids_pause_flag_and_epoch() {
        let (_dir, service) = open();
        visit(&service, r"C:\Work\A", 1);
        let id = id_of(&service, "A");
        service.set_hidden(&id, true).unwrap();
        let snapshot = service.chat_context().unwrap();
        assert!(snapshot.hidden_entity_ids.contains(&id));
        assert!(!snapshot.private_session);
        assert_eq!(
            snapshot.privacy_epoch,
            service.state().unwrap().privacy_epoch
        );
        assert_eq!(
            snapshot.context().expected_epoch,
            Some(snapshot.privacy_epoch)
        );
        service.set_recording(false).unwrap();
        assert!(service.chat_context().unwrap().private_session);
    }

    #[test]
    fn paused_recording_is_withheld_until_resumed() {
        let (_dir, service) = open();
        visit(&service, r"C:\Work\A", 1);
        service.set_recording(false).unwrap();
        for kind in [
            recent(7),
            ChatRequestKind::Frequent {
                days: 30,
                file: false,
            },
            find("A"),
        ] {
            assert_eq!(ask(&service, kind), ChatResponse::Withheld);
        }
        service.set_recording(true).unwrap();
        assert_eq!(ids(ask(&service, recent(7))).len(), 1);
    }

    #[test]
    fn snapshot_from_before_a_privacy_change_is_stale() {
        let (_dir, service) = open();
        visit(&service, r"C:\Work\A", 1);
        let old = service.chat_context().unwrap();
        service.clear().unwrap();
        let mut reader = service.chat_reader(None);
        for kind in [
            recent(7),
            ChatRequestKind::Frequent {
                days: 30,
                file: false,
            },
            find("A"),
        ] {
            let response = run_chat_tool(&mut reader, &req(kind, None), &old.context());
            assert_eq!(response, ChatResponse::Stale);
        }
        // A fresh snapshot works again and sees no history.
        assert!(ids(ask(&service, recent(7))).is_empty());
    }

    #[test]
    fn duplicate_names_stay_ambiguous_until_one_is_hidden() {
        let (_dir, service) = open();
        visit(&service, r"C:\Clients\Acme\Reports", 1);
        visit(&service, r"C:\Clients\Globex\Reports", 2);
        let ChatResponse::Ambiguous { candidates } = ask(&service, find("Reports")) else {
            panic!("must not pick silently")
        };
        assert_eq!(candidates.len(), 2);
        let hints: HashSet<_> = candidates.iter().map(|c| c.parent_hint.clone()).collect();
        assert_eq!(hints.len(), 2, "{candidates:?}");
        service.set_hidden(&candidates[0].id, true).unwrap();
        match ask(&service, find("Reports")) {
            ChatResponse::Resolved { entry } => assert_eq!(entry.id, candidates[1].id),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn workspace_filter_is_optional_and_applies_to_every_query() {
        let (_dir, service) = open();
        visit(&service, r"C:\Ws\Inside", 1);
        visit(&service, r"C:\Elsewhere\Outside", 1);
        assert_eq!(ids(ask(&service, recent(7))).len(), 2);
        let snapshot = service.chat_context().unwrap();
        let mut scoped = service.chat_reader(Some(r"C:\Ws"));
        let response = run_chat_tool(&mut scoped, &req(recent(7), None), &snapshot.context());
        assert_eq!(entries(response).len(), 1);
        let response = run_chat_tool(
            &mut scoped,
            &req(find("Outside"), None),
            &snapshot.context(),
        );
        assert_eq!(response, ChatResponse::NotFound);
    }

    #[test]
    fn real_adapter_requests_do_not_change_episodes_or_ranking() {
        let (_dir, service) = open();
        visit(&service, r"C:\Work\Alpha", 3);
        visit(&service, r"C:\Work\Alpha", 2);
        visit(&service, r"C:\Work\Beta", 1);
        visit(&service, r"C:\Other\Beta", 1);
        service.flush();
        let frequent = ChatRequestKind::Frequent {
            days: 30,
            file: false,
        };
        let items = |service: &ActivityService| {
            let freq: Vec<_> = service
                .frequent(ActivityKind::FolderVisit, 20)
                .unwrap()
                .into_iter()
                .map(|i| (i.entity.id, i.episode_count, i.active_days, i.score))
                .collect();
            let rec: Vec<_> = service
                .recent(ActivityKind::FolderVisit, 20)
                .unwrap()
                .into_iter()
                .map(|i| (i.entity.id, i.last_utc_ms, i.episode_count))
                .collect();
            (freq, rec)
        };
        let before_count = service.episode_count_for_test().unwrap();
        let before_items = items(&service);
        let before_chat = ask(&service, frequent.clone());
        assert_eq!(
            ids(before_chat.clone()).len(),
            1,
            "only Alpha has 2 active days"
        );
        for _ in 0..25 {
            let _ = ask(&service, frequent.clone());
            let _ = ask(&service, find("Beta"));
            let _ = ask(&service, recent(90));
        }
        service.flush();
        assert_eq!(service.episode_count_for_test().unwrap(), before_count);
        assert_eq!(items(&service), before_items);
        assert_eq!(ask(&service, frequent), before_chat);
    }

    #[test]
    fn service_reader_source_has_no_mutation_calls() {
        let source = include_str!("../activity_service.rs");
        let begin = source.find("// chat-reader:begin").expect("begin marker");
        let end = source.find("// chat-reader:end").expect("end marker");
        let block = &source[begin..end];
        for forbidden in [
            ".record(",
            ".record_batch(",
            "forget_entity",
            "exclude_root",
            "clear_all",
            "set_recording_enabled",
            "purge_before",
            "apply_",
            "update_settings",
            ".execute(",
            "std::fs",
            "Command::new",
            "jobs.",
        ] {
            assert!(
                !block.contains(forbidden),
                "reader must stay read-only: {forbidden}"
            );
        }
    }
}
