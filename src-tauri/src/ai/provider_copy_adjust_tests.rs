// FER-471: `copy.adjust` is an additive, opt-in prompt section appended after the read
// capability. The mutating prompt text stays byte-identical (golden in provider_activity_tests.rs).
use super::*;

const ARCHIVE: &str = "archive.create";
const SYNC: &str = "directories.sync";
const QUERY: &str = "activity.query";
const ADJUST: &str = "copy.adjust";

fn request(capabilities: Option<Vec<&str>>) -> PlanRequest {
    PlanRequest {
        request_id: "req_1".into(),
        user_text: "copy-nya lebih cepat dong".into(),
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

const ADJUST_INTENT: &str = r#"{"schemaVersion":1,"requestId":"req_1","intent":{"capability":"copy.adjust","action":"increase","by":2}}"#;

#[test]
fn the_prompt_section_is_appended_after_the_older_capabilities_only_when_offered() {
    let without = system_prompt(&[ARCHIVE, SYNC, QUERY]);
    let with = system_prompt(&[ARCHIVE, SYNC, QUERY, ADJUST]);
    let tail_start = without.rfind("Follow-up:").unwrap();
    assert_eq!(&with[..tail_start], &without[..tail_start]);
    assert!(with.contains(COPY_ADJUST_PROMPT));
    assert!(!without.contains("copy.adjust"));
    // Legacy callers (no list) never see it.
    assert_eq!(offered_capabilities(&request(None)), vec![ARCHIVE, SYNC]);
}

#[test]
fn the_prompt_describes_every_action_and_forbids_ids_and_paths() {
    for needle in [
        "Intent copy.adjust",
        "\"action\":\"increase\"|\"decrease\"",
        "\"by\":optional 1..8",
        "\"action\":\"set\",\"threads\":1..16",
        "\"action\":\"auto\"",
        "\"priority\":\"normal\"|\"background\"",
        "the user confirms on a card",
        "changes no file",
        "Never name a job, path or id",
    ] {
        assert!(COPY_ADJUST_PROMPT.contains(needle), "{needle}");
    }
}

#[test]
fn copy_adjust_is_accepted_only_when_offered() {
    let body = reply(ADJUST_INTENT);
    let Interpreted::Done(PlanOutcome::Intent { envelope, .. }) =
        interpret(&body, "req_1", &[ARCHIVE, ADJUST])
    else {
        panic!("offered copy.adjust must parse")
    };
    assert_eq!(envelope.intent.capability_id(), ADJUST);
    assert!(matches!(
        interpret(&body, "req_1", &[ARCHIVE, SYNC, QUERY]),
        Interpreted::Malformed(_)
    ));
    let bad = ADJUST_INTENT.replace("\"by\":2", "\"by\":99");
    assert!(matches!(
        interpret(&reply(&bad), "req_1", &[ADJUST]),
        Interpreted::Malformed(_)
    ));
}

#[test]
fn the_capability_list_accepts_the_control_capability_and_rejects_unknown_ones() {
    assert!(request(Some(vec![ADJUST])).validate().is_ok());
    assert!(
        request(Some(vec![ARCHIVE, SYNC, QUERY, ADJUST]))
            .validate()
            .is_ok()
    );
    assert!(request(Some(vec![ADJUST, ADJUST])).validate().is_err());
    assert!(request(Some(vec!["copy.delete"])).validate().is_err());
}

#[test]
fn the_control_capability_is_withdrawn_while_no_copy_job_runs() {
    let mut req = request(Some(vec![ARCHIVE, ADJUST]));
    gate_control_capabilities(&mut req, true);
    assert_eq!(
        req.capabilities,
        Some(vec![ARCHIVE.to_owned(), ADJUST.to_owned()])
    );
    gate_control_capabilities(&mut req, false);
    assert_eq!(req.capabilities, Some(vec![ARCHIVE.to_owned()]));
    let mut legacy = request(None);
    gate_control_capabilities(&mut legacy, false);
    assert_eq!(legacy.capabilities, None);
    let mut only = request(Some(vec![ADJUST]));
    gate_control_capabilities(&mut only, false);
    assert_eq!(
        only.capabilities,
        Some(Vec::new()),
        "nothing offered: no provider call"
    );
}

#[test]
fn archive_prompt_offers_7z_levels_and_still_forbids_rar_and_paths() {
    let prompt = system_prompt(&[ARCHIVE]);
    for needle in [
        "\"format\":\"zip\"|\"7z\"",
        "\"level\":optional \"store\"|\"fast\"|\"normal\"|\"maximum\"|\"ultra\"",
        "\"paling kecil\"",
        "-> \"ultra\"",
        "RAR and other formats are not available",
        "Never put a path in the intent; outputName is a basename only",
    ] {
        assert!(prompt.contains(needle), "{needle}");
    }
}
