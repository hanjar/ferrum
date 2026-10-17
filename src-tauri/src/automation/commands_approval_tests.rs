//! Wire contract of the automation commands (FER-522 / P1.1, design §7.2): the request fixtures
//! name exactly the command parameters, every fixture parses into its DTO and serializes back to
//! the very same JSON, nothing that can authorize rides in a request, and no lease id appears in
//! any DTO but the one that hands it to its requester.

use super::tests::ipc::{fixture, fixture_keys, wire_keys};
use super::*;
use crate::automation::approval::{ApproveRequest, TestRequest};
use crate::automation::dto::{
    ApproveResultDto, AutomationDto, AutomationValidateDto, DeleteAutomationDto, PauseMode,
    PauseResultDto, SetEnabledResultDto, TestResultDto,
};
use crate::automation::model::AutomationDraft;
use crate::automation::test_support::Fixture;

const COMMANDS: [(&str, &[&str]); 6] = [
    ("automation_validate", &["automation-validate.request.json"]),
    (
        "automation_test",
        &[
            "automation-test.request.json",
            "automation-test-update.request.json",
        ],
    ),
    (
        "automation_approve",
        &[
            "automation-approve.request.json",
            "automation-approve-update.request.json",
        ],
    ),
    (
        "automation_set_enabled",
        &["automation-set-enabled.request.json"],
    ),
    ("automation_pause", &["automation-pause.request.json"]),
    ("automation_delete", &["automation-delete.request.json"]),
];

#[test]
fn every_request_fixture_names_exactly_the_command_parameters() {
    for (command, names) in COMMANDS {
        for name in names {
            assert_eq!(
                wire_keys(command),
                fixture_keys(&fixture(name)),
                "{command} vs {name}"
            );
        }
    }
}

/// SE-1: no parameter of any automation command is, or sounds like, an authorization flag.
#[test]
fn se_1_no_command_takes_a_confirmation_flag() {
    for (command, _) in COMMANDS {
        for key in wire_keys(command) {
            let lower = key.to_lowercase();
            for forbidden in ["confirm", "approved", "force", "native", "skip", "ack"] {
                assert!(
                    !lower.contains(forbidden),
                    "{command}: parameter {key} looks like an authorization flag"
                );
            }
        }
    }
    // The only booleans: `enabled` (the direction asked for) - and no command takes display text.
    assert_eq!(wire_keys("automation_set_enabled"), ["enabled", "id"]);
    assert_eq!(
        wire_keys("automation_approve"),
        ["draft", "expectedConfigRev", "id", "testId"]
    );
    assert_eq!(wire_keys("automation_test"), ["draft", "id"]);
    assert_eq!(wire_keys("automation_pause"), ["id", "mode"]);
}

#[test]
fn fixture_values_deserialize_into_the_parameter_types() {
    for name in [
        "automation-validate.request.json",
        "automation-test.request.json",
        "automation-test-update.request.json",
        "automation-approve.request.json",
        "automation-approve-update.request.json",
    ] {
        let body = fixture(name);
        let draft: AutomationDraft = serde_json::from_value(body["draft"].clone()).unwrap();
        assert!(model::is_profile_id(&draft.profile_id), "{name}");
    }
    for name in [
        "automation-test.request.json",
        "automation-test-update.request.json",
        "automation-approve.request.json",
        "automation-approve-update.request.json",
    ] {
        let body = fixture(name);
        let id: Option<String> = serde_json::from_value(body["id"].clone()).unwrap();
        if let Some(id) = id {
            assert!(model::is_profile_id(&id));
        }
    }
    for name in [
        "automation-approve.request.json",
        "automation-approve-update.request.json",
    ] {
        let body = fixture(name);
        let id: Option<String> = serde_json::from_value(body["id"].clone()).unwrap();
        let rev: Option<i64> = serde_json::from_value(body["expectedConfigRev"].clone()).unwrap();
        assert_eq!(
            id.is_some(),
            rev.is_some(),
            "{name}: id and revision go together"
        );
        let test: String = serde_json::from_value(body["testId"].clone()).unwrap();
        assert!(model::is_profile_id(&test));
        // The whole flat request also fits the strict request struct once the parameters are
        // assembled the way the command does.
        let assembled = serde_json::json!({
            "id": body["id"], "expectedConfigRev": body["expectedConfigRev"],
            "draft": body["draft"], "testId": body["testId"]
        });
        serde_json::from_value::<ApproveRequest>(assembled).unwrap();
    }
    let test = fixture("automation-test-update.request.json");
    serde_json::from_value::<TestRequest>(test).unwrap();
    let enable = fixture("automation-set-enabled.request.json");
    let _: bool = serde_json::from_value(enable["enabled"].clone()).unwrap();
    let pause = fixture("automation-pause.request.json");
    let mode: PauseMode = serde_json::from_value(pause["mode"].clone()).unwrap();
    assert_eq!(mode, PauseMode::Finish);
    let pause_stop = serde_json::json!("stop");
    assert_eq!(
        serde_json::from_value::<PauseMode>(pause_stop).unwrap(),
        PauseMode::Stop
    );
    assert!(serde_json::from_value::<PauseMode>(serde_json::json!("kill")).is_err());
}

/// Parse a fixture into its DTO and serialize it back: the JSON must be identical, so the DTOs
/// are exactly the documented shape (no field renamed, dropped or invented).
fn roundtrip<T: serde::de::DeserializeOwned + Serialize>(name: &str) -> T {
    let value = fixture(name);
    let dto: T = serde_json::from_value(value.clone()).unwrap_or_else(|e| panic!("{name}: {e}"));
    assert_eq!(
        serde_json::to_value(&dto).unwrap(),
        value,
        "{name} round trip"
    );
    dto
}

#[test]
fn result_fixtures_round_trip_through_the_dtos() {
    let v: AutomationValidateDto = roundtrip("automation-validate.json");
    assert!(v.errors.is_empty() && v.next.is_empty());
    let v: AutomationValidateDto = roundtrip("automation-validate-errors.json");
    assert_eq!(v.errors.len(), 2);
    let t: TestResultDto = roundtrip("automation-test.json");
    assert!(t.passed);
    assert_eq!(t.policy_hash.len(), 64);
    assert_eq!(t.steps.len(), 7);
    let t: TestResultDto = roundtrip("automation-test-failed.json");
    assert!(!t.passed);
    let a: ApproveResultDto = roundtrip("automation-approve.json");
    assert!(matches!(a, ApproveResultDto::Approved { .. }));
    assert_eq!(
        roundtrip::<ApproveResultDto>("automation-approve-canceled.json"),
        ApproveResultDto::Canceled
    );
    assert_eq!(
        roundtrip::<ApproveResultDto>("automation-approve-stale.json"),
        ApproveResultDto::Stale
    );
    for (name, expected) in [
        (
            "automation-set-enabled-enabled.json",
            SetEnabledResultDto::Enabled,
        ),
        (
            "automation-set-enabled-paused.json",
            SetEnabledResultDto::Paused,
        ),
        (
            "automation-set-enabled-canceled.json",
            SetEnabledResultDto::Canceled,
        ),
        (
            "automation-set-enabled-needs-choice.json",
            SetEnabledResultDto::NeedsChoice,
        ),
    ] {
        assert_eq!(roundtrip::<SetEnabledResultDto>(name), expected);
    }
    assert_eq!(
        roundtrip::<PauseResultDto>("automation-pause.json"),
        PauseResultDto::Paused
    );
    assert_eq!(
        roundtrip::<DeleteAutomationDto>("automation-delete.json"),
        DeleteAutomationDto::Deleted
    );
    use crate::automation::dto::NeedsReviewDto as R;
    for (name, expected) in [
        (
            "automation-dto-review-profile-changed.json",
            R::ProfileChanged { from: 3, to: 4 },
        ),
        (
            "automation-dto-review-policy-changed.json",
            R::PolicyChanged,
        ),
        ("automation-dto-review-not-approved.json", R::NotApproved),
        (
            "automation-dto-review-key-unavailable.json",
            R::KeyUnavailable,
        ),
        (
            "automation-dto-review-unsupported-version.json",
            R::UnsupportedVersion,
        ),
    ] {
        assert_eq!(roundtrip::<AutomationDto>(name).review, Some(expected));
    }
}

/// The fixtures drive the real cores with a real profile: the test request, then the approve
/// request, produce an answer with exactly the fixture's shape.
#[test]
fn the_request_fixtures_drive_test_and_approve() {
    let fx = Fixture::new();
    let (profile, _) = fx.profile("Projects backup");
    let mut test_body = fixture("automation-test.request.json");
    test_body["draft"]["profileId"] = profile.clone().into();
    let test_req: TestRequest = serde_json::from_value(test_body).unwrap();
    let result = crate::automation::test_support::with_test_deps(|deps| {
        approval::test_in(&fx.store, &fx.ledger, test_req, deps, fx.clock()).unwrap()
    });
    let shape = |v: &serde_json::Value| fixture_keys(v);
    assert_eq!(
        shape(&serde_json::to_value(&result).unwrap()),
        shape(&fixture("automation-test.json"))
    );

    let mut body = fixture("automation-approve.request.json");
    body["draft"]["profileId"] = profile.into();
    body["testId"] = result.test_id.clone().into();
    let req: ApproveRequest = serde_json::from_value(body).unwrap();
    let out = approval::approve_in(&fx.store, req, &fx.deps(), |_| true).unwrap();
    let ApproveResultDto::Approved { automation } = out else {
        panic!("approved")
    };
    assert_eq!(
        fixture_keys(&serde_json::to_value(&*automation).unwrap()),
        fixture_keys(&fixture("automation-approve.json")["automation"])
    );
}

/// design §3.4: a lease id is handed to its requester by `sync_lease_acquire` and appears in no
/// other fixture or DTO.
#[test]
fn a_lease_id_appears_in_no_other_dto_or_fixture() {
    let dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../contract-fixtures/automation");
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with("sync-lease") {
            continue;
        }
        let text = std::fs::read_to_string(entry.path()).unwrap();
        assert!(!text.contains("leaseId"), "{name}");
        // A lease id is 32 lowercase hex characters standing alone as a JSON string.
        for token in text.split('"') {
            let is_lease_shaped =
                token.len() == 32 && token.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f'));
            // File ids in identities are 32 hex too, but never in these automation fixtures'
            // lease positions: the lease request/answer fixtures are the only 32-hex users.
            if is_lease_shaped
                && !name.starts_with("sync-lease")
                && !name.starts_with("sync-profile")
            {
                panic!("{name} contains a lease-shaped token {token}");
            }
        }
    }
    for source in [
        include_str!("dto.rs"),
        include_str!("approval.rs"),
        include_str!("test_run.rs"),
    ] {
        assert!(!source.contains("lease_id") && !source.contains("leaseId"));
    }
}

/// SE-5: when the store location is a junction, no approval key and no baseline folder is created
/// anywhere - in particular not in the junction's target.
#[cfg(windows)]
#[test]
fn an_untrusted_store_gets_no_key_and_no_baseline_folder() {
    let base = tempfile::tempdir().unwrap();
    let real = base.path().join("real");
    std::fs::create_dir_all(&real).unwrap();
    let link = base.path().join("via-junction");
    assert!(
        std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&link)
            .arg(&real)
            .output()
            .is_ok_and(|o| o.status.success())
    );
    let dir = link.join("automation");
    let store = Arc::new(Store::open(&dir, "0.2.0"));
    assert_eq!(store.check_writable().unwrap_err(), StoreError::Untrusted);
    let state = AutomationState::from_parts(
        store,
        Arc::new(LeaseRegistry::new(LeaseEnv::system())),
        Some(&dir),
    );
    // The key cannot be made, so nothing can be approved; the baseline store is unavailable.
    assert!(state.keys.load_or_create().is_err());
    let pair = crate::sync_baseline::RootPair {
        left_root: base.path().to_string_lossy().into_owned(),
        right_root: real.to_string_lossy().into_owned(),
    };
    let _ = state.baselines.engine_store().load(&pair);
    assert!(
        std::fs::read_dir(&real).unwrap().next().is_none(),
        "nothing was created in the junction target"
    );
}
