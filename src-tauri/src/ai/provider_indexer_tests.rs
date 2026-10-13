// FER-476: the FerrumIndexer capabilities are an additive, opt-in prompt section appended after the
// control capability. Older prompt text stays byte-identical (golden in provider_activity_tests.rs).
use super::*;

const ARCHIVE: &str = "archive.create";
const SYNC: &str = "directories.sync";
const QUERY: &str = "activity.query";
const ADJUST: &str = "copy.adjust";
const STATUS: &str = "indexer.status";
const MODE: &str = "indexer.setMode";

fn request(capabilities: Option<Vec<&str>>) -> PlanRequest {
    PlanRequest {
        request_id: "req_1".into(),
        user_text: "indeksnya sudah siap?".into(),
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

const STATUS_INTENT: &str =
    "{\"schemaVersion\":1,\"requestId\":\"req_1\",\"intent\":{\"capability\":\"indexer.status\"}}";
const MODE_INTENT: &str = "{\"schemaVersion\":1,\"requestId\":\"req_1\",\"intent\":{\"capability\":\"indexer.setMode\",\"mode\":\"shared\"}}";

#[test]
fn the_prompt_sections_are_appended_after_the_older_capabilities_only_when_offered() {
    let without = system_prompt(&[ARCHIVE, SYNC, QUERY, ADJUST]);
    let with = system_prompt(&[ARCHIVE, SYNC, QUERY, ADJUST, STATUS, MODE]);
    let tail_start = without.rfind("Follow-up:").unwrap();
    assert_eq!(&with[..tail_start], &without[..tail_start]);
    assert!(with.contains(INDEXER_STATUS_PROMPT));
    assert!(with.contains(INDEXER_SET_MODE_PROMPT));
    assert!(!without.contains("indexer."));
    // Each section stands on its own.
    assert!(system_prompt(&[STATUS]).contains("Intent indexer.status"));
    assert!(!system_prompt(&[STATUS]).contains("Intent indexer.setMode"));
    assert!(system_prompt(&[MODE]).contains("Intent indexer.setMode"));
    // Legacy callers (no list) never see them.
    assert_eq!(offered_capabilities(&request(None)), vec![ARCHIVE, SYNC]);
}

#[test]
fn the_prompt_states_the_privacy_rules_and_forbids_ids_and_paths() {
    for needle in [
        "Intent indexer.status",
        "read-only",
        "it changes nothing",
        "\"capability\":\"indexer.status\"",
    ] {
        assert!(INDEXER_STATUS_PROMPT.contains(needle), "{needle}");
    }
    for needle in [
        "Intent indexer.setMode",
        "\"mode\":\"high_performance\"|\"shared\"",
        "the user must confirm on a card",
        "Windows administrator approval",
        "only for a PC used by one person",
        "Never propose or choose a mode on your own",
        "never name a path, drive or id",
    ] {
        assert!(INDEXER_SET_MODE_PROMPT.contains(needle), "{needle}");
    }
}

#[test]
fn the_indexer_intents_are_accepted_only_when_offered() {
    for (intent, id) in [(STATUS_INTENT, STATUS), (MODE_INTENT, MODE)] {
        let body = reply(intent);
        let Interpreted::Done(PlanOutcome::Intent { envelope, .. }) =
            interpret(&body, "req_1", &[ARCHIVE, STATUS, MODE])
        else {
            panic!("offered {id} must parse")
        };
        assert_eq!(envelope.intent.capability_id(), id);
        assert!(matches!(
            interpret(&body, "req_1", &[ARCHIVE, SYNC, QUERY, ADJUST]),
            Interpreted::Malformed(_)
        ));
    }
    // A model-supplied extra field (path, confirmed, ...) is rejected.
    let bad = MODE_INTENT.replace(
        "\"mode\":\"shared\"",
        "\"mode\":\"shared\",\"confirmed\":true",
    );
    assert!(matches!(
        interpret(&reply(&bad), "req_1", &[MODE]),
        Interpreted::Malformed(_)
    ));
}

#[test]
fn the_capability_list_accepts_the_indexer_ids_and_rejects_unknown_ones() {
    assert!(request(Some(vec![STATUS])).validate().is_ok());
    assert!(
        request(Some(vec![ARCHIVE, SYNC, QUERY, ADJUST, STATUS, MODE]))
            .validate()
            .is_ok()
    );
    assert!(request(Some(vec![MODE, MODE])).validate().is_err());
    assert!(request(Some(vec!["indexer.setmode"])).validate().is_err());
}

#[test]
fn the_indexer_capabilities_are_withdrawn_outside_the_desktop_app() {
    let mut req = request(Some(vec![ARCHIVE, STATUS, MODE]));
    gate_indexer_capabilities(&mut req, true);
    assert_eq!(
        req.capabilities,
        Some(vec![ARCHIVE.to_owned(), STATUS.to_owned(), MODE.to_owned()])
    );
    gate_indexer_capabilities(&mut req, false);
    assert_eq!(req.capabilities, Some(vec![ARCHIVE.to_owned()]));
    let mut legacy = request(None);
    gate_indexer_capabilities(&mut legacy, false);
    assert_eq!(legacy.capabilities, None);
    let mut only = request(Some(vec![STATUS, MODE]));
    gate_indexer_capabilities(&mut only, false);
    assert_eq!(
        only.capabilities,
        Some(Vec::new()),
        "nothing offered: no provider call"
    );
}
