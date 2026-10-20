// FER-511: the AI surface must keep having NO path to the Rar.exe settings. The model can
// neither name a rar/prefs capability (the intent enum is closed, `deny_unknown_fields`), nor
// smuggle a `rarExePath` into a known intent, and `archive.create` rejects format "rar" (zip only) — so
// `archive_rar_tool_set_path` / `save_prefs` are unreachable from every AI intent, and a later
// capability added to the enum trips these tests until someone re-checks this boundary.
use super::*;
use serde_json::json;

/// Every capability that exists on the AI wire today. A new `Intent` variant must be added here
/// AND the change must be checked against the Rar.exe boundary this file pins down.
const ALL_CAPABILITIES: [&str; 9] = [
    "archive.create",
    "directories.sync",
    ACTIVITY_QUERY_CAPABILITY,
    COPY_ADJUST_CAPABILITY,
    INDEXER_STATUS_CAPABILITY,
    INDEXER_SET_MODE_CAPABILITY,
    DISK_USAGE_QUERY_CAPABILITY,
    PANE_NAVIGATE_CAPABILITY,
    ITEMS_DELETE_CAPABILITY,
];

#[test]
fn no_capability_names_a_rar_or_prefs_surface() {
    for id in ALL_CAPABILITIES {
        let lower = id.to_ascii_lowercase();
        assert!(
            !lower.contains("rar") && !lower.contains("pref") && !lower.contains("setting"),
            "capability {id} must never touch the Rar.exe settings"
        );
    }
}

#[test]
fn a_rar_path_capability_is_not_part_of_the_ai_surface() {
    for capability in [
        "rar.setPath",
        "archive.setRarPath",
        "archive_rar_tool_set_path",
        "prefs.save",
        "settings.write",
    ] {
        let envelope = json!({
            "schemaVersion": 1,
            "requestId": "req_1",
            "intent": { "capability": capability, "path": "C:\\evil\\Rar.exe" }
        });
        let err = parse_intent_offered(envelope, "req_1", &ALL_CAPABILITIES)
            .expect_err("unknown capability must be rejected");
        assert!(
            matches!(err, ContractError::InvalidEnvelope(_)),
            "{capability}: {err:?}"
        );
    }
}

#[test]
fn archive_create_intent_cannot_carry_a_rar_exe_path() {
    // `deny_unknown_fields` on the intent: an injected rarExePath makes the whole reply invalid
    // (the rest of the envelope is a VALID zip plan — the injected field is the only violation).
    let envelope = json!({
        "schemaVersion": 1,
        "requestId": "req_1",
        "intent": {
            "capability": "archive.create",
            "sourcePane": "left",
            "selector": { "kind": "allVisible", "entries": "filesOnly" },
            "format": "zip",
            "target": { "kind": "pane", "pane": "right" },
            "rarExePath": "C:\\evil\\Rar.exe"
        }
    });
    let err = parse_intent(envelope, "req_1").expect_err("extra field must be rejected");
    assert!(
        matches!(err, ContractError::InvalidEnvelope(_)),
        "rarExePath injection: {err:?}"
    );
}

#[test]
fn archive_create_rejects_format_rar() {
    // No path is injected here (that case is the test above). The legacy mutating parser rejects
    // format "rar", so the model can only ask for zip and `archive.create` never reaches the
    // Rar.exe code path at all.
    let envelope = json!({
        "schemaVersion": 1,
        "requestId": "req_1",
        "intent": {
            "capability": "archive.create",
            "sourcePane": "left",
            "selector": { "kind": "allVisible", "entries": "filesOnly" },
            "format": "rar",
            "target": { "kind": "pane", "pane": "right" }
        }
    });
    assert!(parse_intent(envelope, "req_1").is_err());
}
