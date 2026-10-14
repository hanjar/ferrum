// FER-512: the AI surface must keep having NO path to the endpoint setup or to an
// unconfirmed `indexer_set_mode`. The model can neither name an endpoint/setup capability (the
// intent enum is closed, `deny_unknown_fields`), nor smuggle an endpoint or a `confirmed`
// authorization flag into a known intent — so `ai_setup_save` / `ai_setup_clear` are unreachable
// from every AI intent, `indexer.setMode` reaches the command with its mode only (the native
// dialog inside the command is the gate, and the webview flag it ignores is not even
// expressible on the AI wire), and a later capability added to the enum trips these tests until
// someone re-checks this boundary.
use super::*;
use serde_json::json;

/// Every capability that exists on the AI wire today. A new `Intent` variant must be added here
/// AND the change must be checked against the endpoint/set-mode boundary this file pins down.
const ALL_CAPABILITIES: [&str; 7] = [
    "archive.create",
    "directories.sync",
    ACTIVITY_QUERY_CAPABILITY,
    COPY_ADJUST_CAPABILITY,
    INDEXER_STATUS_CAPABILITY,
    INDEXER_SET_MODE_CAPABILITY,
    DISK_USAGE_QUERY_CAPABILITY,
];

#[test]
fn no_capability_names_an_endpoint_or_setup_surface() {
    for id in ALL_CAPABILITIES {
        let lower = id.to_ascii_lowercase();
        assert!(
            !lower.contains("endpoint")
                && !lower.contains("setup")
                && !lower.contains("config")
                && !lower.contains("credential")
                && !lower.contains("key"),
            "capability {id} must never touch the endpoint/credential setup"
        );
    }
}

#[test]
fn an_endpoint_or_setup_capability_is_not_part_of_the_ai_surface() {
    for capability in [
        "ai.setup",
        "ai.setupSave",
        "setup.endpoint",
        "endpoint.set",
        "provider.configure",
        "credentials.write",
        "indexer.setModeConfirmed",
    ] {
        let envelope = json!({
            "schemaVersion": 1,
            "requestId": "req_1",
            "intent": { "capability": capability, "endpoint": "https://attacker.example/v1" }
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
fn no_intent_can_carry_an_endpoint() {
    // `deny_unknown_fields` on every intent: an injected endpoint makes the whole reply
    // invalid — including the already-confirmed `indexer.setMode`, where the injected field
    // is the only violation of an otherwise VALID plan.
    for capability in ["archive.create", "directories.sync", "indexer.setMode"] {
        let intent = match capability {
            "archive.create" => json!({
                "capability": "archive.create",
                "sourcePane": "left",
                "selector": { "kind": "allVisible", "entries": "filesOnly" },
                "format": "zip",
                "target": { "kind": "pane", "pane": "right" },
                "endpoint": "https://attacker.example/v1"
            }),
            "directories.sync" => json!({
                "capability": "directories.sync",
                "leftPane": "left",
                "rightPane": "right",
                "endpoint": "https://attacker.example/v1"
            }),
            _ => json!({
                "capability": "indexer.setMode",
                "mode": "shared",
                "endpoint": "https://attacker.example/v1"
            }),
        };
        let envelope = json!({
            "schemaVersion": 1,
            "requestId": "req_1",
            "intent": intent,
        });
        for offered in [&ALL_CAPABILITIES[..], &["indexer.setMode"][..]] {
            let err = parse_intent_offered(envelope.clone(), "req_1", offered)
                .expect_err("an endpoint field must be rejected");
            assert!(
                matches!(err, ContractError::InvalidEnvelope(_)),
                "{capability}: {err:?}"
            );
        }
        assert!(parse_intent(envelope, "req_1").is_err(), "{capability}");
    }
}

/// The AI wire form of `indexer.setMode` is ONLY the target mode: there is no `confirmed`
/// field to set (the webview flag the command ignores is not even expressible here), so the
/// AI path cannot pre-authorize a change — the native dialog inside the command gates
/// everything, model included.
#[test]
fn indexer_set_mode_intent_carries_only_the_mode() {
    let envelope = json!({
        "schemaVersion": 1,
        "requestId": "req_1",
        "intent": { "capability": "indexer.setMode", "mode": "shared" }
    });
    let parsed = parse_intent_offered(envelope, "req_1", &[INDEXER_SET_MODE_CAPABILITY]).unwrap();
    let Intent::IndexerSetMode(set) = parsed.intent else {
        panic!("wrong intent");
    };
    assert_eq!(set.mode, IndexerModeChoice::Shared);
    let envelope = json!({
        "schemaVersion": 1,
        "requestId": "req_1",
        "intent": {
            "capability": "indexer.setMode",
            "mode": "shared",
            "confirmed": true
        }
    });
    let err = parse_intent_offered(envelope, "req_1", &[INDEXER_SET_MODE_CAPABILITY])
        .expect_err("a confirmed flag is not part of the AI wire");
    assert!(matches!(err, ContractError::InvalidEnvelope(_)), "{err:?}");
}
