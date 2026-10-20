// FER-576: navigation, delete and chain replies through the provider's strict interpreter. The
// user's own request text is the only source of a navigation path.
use super::*;

const ARCHIVE: &str = "archive.create";
const NAV: &str = "pane.navigate";
const DELETE: &str = "items.delete";
const TEXT: &str = r"d:\Downloads\Telegram Desktop\pc_20250814\pc_202508014.csv -> tolong arsipkan ke paling kecil gunakan 7zip lalu hapus ya";

fn reply(content: &str) -> String {
    serde_json::json!({"choices":[{"finish_reason":"stop","message":{"content":content}}]})
        .to_string()
}

const NAV_INTENT: &str = r#"{"schemaVersion":1,"requestId":"req_1","intent":{"capability":"pane.navigate","pane":"activeAtSubmit","path":"D:\\Downloads\\Telegram Desktop\\pc_20250814\\pc_202508014.csv"}}"#;
const CHAIN: &str = r#"{"schemaVersion":1,"requestId":"req_1","steps":[
{"capability":"pane.navigate","pane":"activeAtSubmit","path":"D:\\Downloads\\Telegram Desktop\\pc_20250814\\pc_202508014.csv"},
{"capability":"archive.create","sourcePane":"activeAtSubmit","selector":{"kind":"selected"},"format":"7z","level":"ultra","target":{"kind":"sourceDirectory"}},
{"capability":"items.delete","sourcePane":"activeAtSubmit","selector":{"kind":"selected"}}]}"#;

#[test]
fn a_chain_is_accepted_only_when_every_step_is_offered_and_the_path_was_typed() {
    let body = reply(CHAIN);
    let all = [ARCHIVE, NAV, DELETE];
    let Interpreted::Done(PlanOutcome::Chain { chain, .. }) =
        interpret_for(&body, "req_1", &all, TEXT)
    else {
        panic!("a valid chain must parse")
    };
    assert_eq!(chain.steps.len(), 3);
    // Not typed by the user: the whole reply is rejected (repair request, then failure).
    assert!(matches!(
        interpret_for(&body, "req_1", &all, "arsipkan file itu lalu hapus"),
        Interpreted::Malformed(_)
    ));
    assert!(matches!(
        interpret_for(&body, "req_1", &[ARCHIVE, NAV], TEXT),
        Interpreted::Malformed(_)
    ));
    assert!(matches!(
        interpret_for(&body, "req_1", &[ARCHIVE, DELETE], TEXT),
        Interpreted::Malformed(_)
    ));
}

#[test]
fn a_single_navigation_needs_a_path_from_the_request_text() {
    let body = reply(NAV_INTENT);
    assert!(matches!(
        interpret_for(&body, "req_1", &[NAV], TEXT),
        Interpreted::Done(PlanOutcome::Intent { .. })
    ));
    assert!(matches!(
        interpret_for(&body, "req_1", &[NAV], "buka folder downloads"),
        Interpreted::Malformed(_)
    ));
    assert!(matches!(
        interpret_for(&body, "req_1", &[ARCHIVE], TEXT),
        Interpreted::Malformed(_)
    ));
}

#[test]
fn a_reply_with_steps_plus_an_intent_or_other_keys_is_rejected() {
    let both = r#"{"schemaVersion":1,"requestId":"req_1","intent":{"capability":"items.delete","sourcePane":"left","selector":{"kind":"selected"}},"steps":[]}"#;
    assert!(matches!(
        interpret_for(&reply(both), "req_1", &[ARCHIVE, NAV, DELETE], TEXT),
        Interpreted::Malformed(_)
    ));
}

#[test]
fn prompts_describe_navigation_delete_and_the_chain_only_when_offered() {
    let without = system_prompt(&[ARCHIVE]);
    assert!(!without.contains("pane.navigate"));
    assert!(!without.contains("items.delete"));
    assert!(!without.contains("Chain:"));
    let with = system_prompt(&[ARCHIVE, NAV, DELETE]);
    for needle in [
        "Intent pane.navigate",
        "any path that is not written in the request is rejected",
        "Intent items.delete",
        "Recycle Bin",
        "never name an item or a path",
        "never deletes permanently",
        "Chain:",
        "2 to 4 steps",
        "only after the archive was created and tested",
    ] {
        assert!(with.contains(needle), "{needle}");
    }
    // The older prompt text stays byte-identical before the new sections.
    let tail = without.rfind("Follow-up:").unwrap();
    assert_eq!(&with[..tail], &without[..tail].to_owned()[..tail]);
}

#[test]
fn the_capability_list_knows_navigation_and_delete_but_legacy_callers_do_not_get_them() {
    let request = |caps: Option<Vec<&str>>| PlanRequest {
        request_id: "req_1".into(),
        user_text: TEXT.into(),
        context: OutboundContext {
            active_pane: PaneName::Left,
            left_location: LocationKind::Disk,
            right_location: LocationKind::Disk,
            left_count: 1,
            right_count: 1,
            show_hidden: false,
            filter_active: false,
        },
        capabilities: caps.map(|list| list.into_iter().map(str::to_owned).collect()),
    };
    assert!(request(Some(vec![ARCHIVE, NAV, DELETE])).validate().is_ok());
    assert!(request(Some(vec![NAV, NAV])).validate().is_err());
    assert_eq!(
        offered_capabilities(&request(None)),
        vec![ARCHIVE, "directories.sync"]
    );
}
