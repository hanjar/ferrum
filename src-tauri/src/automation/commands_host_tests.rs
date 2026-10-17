//! Wire contract of the control commands and events of the host (P1.3, design §7.2): the request
//! fixtures name exactly the command parameters, the answers and the two events serialize to the
//! fixtures, no control takes anything that could authorize or choose a policy, and the error
//! codes are the closed set.

use super::tests::ipc::{fixture, fixture_keys, wire_keys};
use super::*;
use crate::automation::dto::{ChangedEventDto, QueueResultDto, QueueStatus, RequestedDto};
use crate::automation::host::request::{QueueAnswer, RequestError};
use crate::automation::host::{HostEvent, ProgressEvent};
use crate::automation::store::runs::RunCounts;

const COMMANDS: [(&str, &str); 4] = [
    ("automation_run_now", "automation-run-now.request.json"),
    ("automation_stop_run", "automation-stop-run.request.json"),
    ("automation_recheck", "automation-recheck.request.json"),
    ("automation_retry", "automation-retry.request.json"),
];

#[test]
fn every_request_fixture_names_exactly_the_command_parameters() {
    for (command, name) in COMMANDS {
        assert_eq!(
            wire_keys(command),
            fixture_keys(&fixture(name)),
            "{command} vs {name}"
        );
    }
    assert_eq!(wire_keys("automation_run_now"), ["id"]);
    assert_eq!(wire_keys("automation_stop_run"), ["runId"]);
    assert_eq!(wire_keys("automation_recheck"), ["runId"]);
    assert_eq!(wire_keys("automation_retry"), ["runId"]);
}

/// SE-1 / the security notes of P1.3: a control from the webview only asks. No parameter can
/// carry a confirmation, a policy, a root, a profile, a hash, a lease or a way past the gate.
#[test]
fn controls_take_no_authorization_policy_or_root() {
    for command in [
        "automation_run_now",
        "automation_stop_run",
        "automation_recheck",
        "automation_retry",
        "automation_pause",
    ] {
        for key in wire_keys(command) {
            let lower = key.to_lowercase();
            for forbidden in [
                "confirm", "approved", "force", "native", "skip", "ack", "enabled", "policy",
                "profile", "root", "path", "hash", "lease", "mac", "key", "gate", "token",
            ] {
                assert!(
                    !lower.contains(forbidden),
                    "{command} takes `{key}` which looks like `{forbidden}`"
                );
            }
        }
    }
}

#[test]
fn queue_answers_serialize_to_the_fixtures() {
    let cases = [
        (
            QueueAnswer::Queued,
            "automation-run-now-queued.json",
            QueueStatus::Queued,
        ),
        (
            QueueAnswer::Waiting {
                name: "Nightly photos to NAS".into(),
            },
            "automation-run-now-waiting.json",
            QueueStatus::Waiting,
        ),
        (
            QueueAnswer::Merged,
            "automation-run-now-merged.json",
            QueueStatus::Merged,
        ),
        (
            QueueAnswer::Device,
            "automation-run-now-device.json",
            QueueStatus::Device,
        ),
        (
            QueueAnswer::Requested,
            "automation-run-now-requested.json",
            QueueStatus::Requested,
        ),
    ];
    for (answer, name, status) in cases {
        let dto = queue_result(Ok(answer)).unwrap();
        assert_eq!(dto.status, status, "{name}");
        assert_eq!(serde_json::to_value(&dto).unwrap(), fixture(name), "{name}");
        // And the fixture parses back into the DTO.
        let back: QueueResultDto = serde_json::from_value(fixture(name)).unwrap();
        assert_eq!(back, dto, "{name}");
    }
    assert_eq!(
        serde_json::to_value(RequestedDto::Requested).unwrap(),
        fixture("automation-stop-run.json")
    );
    assert_eq!(
        serde_json::to_value(RequestedDto::Requested).unwrap(),
        fixture("automation-recheck.json")
    );
    assert_eq!(fixture("automation-retry.json")["status"], "queued");
}

#[test]
fn the_events_serialize_to_the_fixtures_and_carry_no_secret() {
    let changed = serde_json::to_value(ChangedEventDto { seq: 57 }).unwrap();
    assert_eq!(changed, fixture("automation-event-changed.json"));
    let progress = ProgressEvent {
        run_id: 1042,
        automation_id: "7d1c2b3a-4e5f-4a6b-8c7d-9e0f1a2b3c4d".into(),
        phase: "sync",
        ops_total: 6,
        counts: RunCounts {
            ok: 3,
            failed: 0,
            not_started: 3,
            held: 0,
            skipped_changed: 0,
        },
    };
    let value = serde_json::to_value(&progress).unwrap();
    assert_eq!(value, fixture("automation-event-progress.json"));
    let text = value.to_string().to_lowercase();
    for forbidden in ["mac", "key", "lease", "path", "hash"] {
        assert!(!text.contains(forbidden), "{forbidden} in {text}");
    }
    // The in-process event enum is what the app turns into the two payloads above.
    let _ = HostEvent::Changed { seq: 1 };
}

#[test]
fn refusals_map_to_the_closed_set_of_codes_without_paths() {
    let cases = [
        (RequestError::NotFound, "not_found"),
        (RequestError::Invalid, "invalid_input"),
        (RequestError::NeedsReview, "needs_review"),
        (RequestError::Held("1 failed".into()), "held"),
        (RequestError::Paused, "paused"),
        (RequestError::RunnerDown, "runner_down"),
        (RequestError::NotReady("recheck_first"), "not_ready"),
        (RequestError::NotReady("not_running"), "not_ready"),
        (RequestError::Busy, "busy"),
        (RequestError::TooMany, "rate_limited"),
        (
            RequestError::Store(StoreError::Unavailable),
            "store_unavailable",
        ),
        (RequestError::Store(StoreError::ReadOnly), "store_read_only"),
    ];
    for (error, code) in cases {
        let mapped = request_error(error);
        assert_eq!(mapped.code, code);
        assert!(
            !mapped.message.contains('\\') && !mapped.message.contains(':'),
            "{}",
            mapped.message
        );
    }
}

#[test]
fn ids_are_validated_before_anything_else() {
    assert!(require_auto_id("7d1c2b3a-4e5f-4a6b-8c7d-9e0f1a2b3c4d").is_ok());
    for bad in [
        "",
        "nope",
        "7D1C2B3A-4E5F-4A6B-8C7D-9E0F1A2B3C4D",
        "../../x",
    ] {
        assert_eq!(
            require_auto_id(bad).unwrap_err().code,
            "invalid_input",
            "{bad}"
        );
    }
    for bad in [0, -1, i64::MIN] {
        assert_eq!(require_run_id(bad).unwrap_err().code, "invalid_input");
    }
    assert!(require_run_id(1).is_ok());
}

/// No panic can cross IPC from the new commands: the file has no `unwrap()`/`expect(` in the
/// command wrappers (tests excepted).
#[test]
fn the_new_command_wrappers_do_not_unwrap() {
    let source = include_str!("commands.rs");
    let start = source.find("pub async fn automation_run_now").unwrap();
    let end = source[start..].find("pub fn stop_host").unwrap() + start;
    let body = &source[start..end];
    assert!(!body.contains(".unwrap()"), "unwrap in a command");
    assert!(!body.contains(".expect("), "expect in a command");
    assert!(!body.contains("panic!"), "panic in a command");
}
