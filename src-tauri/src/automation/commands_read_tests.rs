//! Wire contract of the read commands (P1.2, design §7.2): the request fixtures name exactly the
//! command parameters, every result fixture parses into its DTO and serializes back to the very
//! same JSON, the labels in the fixtures are the ones `status` produces, and no read command takes
//! anything that could authorize.

use super::tests::ipc::{fixture, fixture_keys, wire_keys};
use super::*;
use crate::automation::dto::{
    AutomationDetailDto, AutomationRowDto, EventsPageDto, HistoryDto, RunOpsDto, StatusKind,
};
use crate::automation::read::{EventsRequest, HistoryFilterWire, HistoryRequest, RunOpsRequest};
use crate::automation::status;
use crate::automation::store::runs::{Outcome, RunCounts};

const COMMANDS: [(&str, &str); 4] = [
    ("automation_get", "automation-get.request.json"),
    ("automation_history", "automation-history.request.json"),
    ("automation_run_ops", "automation-run-ops.request.json"),
    ("automation_events", "automation-events.request.json"),
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
    // `automation_list` takes nothing.
    assert!(wire_keys("automation_list").is_empty());
    assert_eq!(wire_keys("automation_history"), ["automationId", "before", "filter", "limit"]);
    assert_eq!(wire_keys("automation_run_ops"), ["limit", "offset", "runId"]);
    assert_eq!(wire_keys("automation_events"), ["before", "id", "limit"]);
    assert_eq!(wire_keys("automation_get"), ["id"]);
}

/// SE-1: no read command takes a flag that could stand for a confirmation or an authorization.
#[test]
fn read_commands_take_no_authorization_flag() {
    for command in [
        "automation_list",
        "automation_get",
        "automation_history",
        "automation_run_ops",
        "automation_events",
    ] {
        for key in wire_keys(command) {
            let lower = key.to_lowercase();
            for forbidden in ["confirm", "approved", "force", "native", "skip", "ack", "enabled"] {
                assert!(
                    !lower.contains(forbidden),
                    "{command}: parameter {key} looks like an authorization flag"
                );
            }
        }
    }
}

#[test]
fn request_fixture_values_deserialize_into_the_strict_request_types() {
    let h = fixture("automation-history.request.json");
    let req: HistoryRequest = serde_json::from_value(h.clone()).unwrap();
    assert_eq!(req.filter, HistoryFilterWire::Attention);
    assert!(model::is_profile_id(req.automation_id.as_deref().unwrap()));
    assert_eq!((req.before, req.limit), (Some(1100), 50));
    let o = fixture("automation-run-ops.request.json");
    let req: RunOpsRequest = serde_json::from_value(o).unwrap();
    assert_eq!((req.run_id, req.offset, req.limit), (1043, 0, 200));
    let e = fixture("automation-events.request.json");
    let req: EventsRequest = serde_json::from_value(e).unwrap();
    assert!(model::is_profile_id(&req.id));
    assert_eq!((req.before, req.limit), (Some(31), 14));
    let g = fixture("automation-get.request.json");
    assert!(model::is_profile_id(g["id"].as_str().unwrap()));
    // An extra key on the assembled request is refused.
    let mut extra = h;
    extra["force"] = true.into();
    assert!(serde_json::from_value::<HistoryRequest>(extra).is_err());
}

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
    let rows: Vec<AutomationRowDto> = roundtrip("automation-list.json");
    assert_eq!(rows.len(), 3);
    let kinds: Vec<StatusKind> = rows.iter().map(|r| r.status.kind).collect();
    assert_eq!(
        kinds,
        [StatusKind::Scheduled, StatusKind::Review, StatusKind::Attention]
    );
    assert!(rows.iter().all(|r| model::is_profile_id(&r.id)));
    // Run now is dimmed exactly when something holds the automation.
    assert_eq!(
        rows.iter().map(|r| r.can_run_now).collect::<Vec<_>>(),
        [true, false, false]
    );
    assert!(rows[1].show_review && rows[1].review.is_some());
    assert!(rows[2].attention.is_some());
    let d: AutomationDetailDto = roundtrip("automation-get.json");
    assert_eq!(d.pipeline.current, Some(4));
    assert_eq!(d.recent_runs.len(), 2);
    assert_eq!(d.waiting[0].merged_count, 3);
    let h: HistoryDto = roundtrip("automation-history.json");
    assert!(h.runs[0].needs_attention && h.next == Some(1043));
    let o: RunOpsDto = roundtrip("automation-run-ops.json");
    assert_eq!((o.ops.len(), o.total, o.stored), (3, 6, 6));
    let e: EventsPageDto = roundtrip("automation-events.json");
    assert_eq!(e.events.len(), 2);
}

/// The labels the fixtures show are the ones the backend produces: a drift between the documented
/// example and `status` fails here.
#[test]
fn fixture_labels_are_what_status_produces() {
    let rows: Vec<AutomationRowDto> = roundtrip("automation-list.json");
    let last = rows[0].last_run.as_ref().unwrap();
    let ok6 = RunCounts {
        ok: 6,
        ..Default::default()
    };
    assert_eq!(last.result, status::outcome_chip(Outcome::Completed, &ok6, 6, None));
    let partial = RunCounts {
        ok: 4,
        failed: 1,
        not_started: 1,
        ..Default::default()
    };
    assert_eq!(
        rows[2].last_run.as_ref().unwrap().result,
        status::outcome_chip(Outcome::Partial, &partial, 6, None)
    );
    let att = rows[2].attention.as_ref().unwrap();
    assert_eq!(
        att.text,
        crate::automation::store::runs::attention_text(Outcome::Partial, &partial, None)
    );
    assert_eq!(
        rows[2].status.sub.as_deref(),
        Some(format!("{} \u{b7} new runs held", att.text).as_str())
    );
    assert_eq!(
        rows[1].status.sub.as_deref(),
        Some(status::review_label(&crate::automation::policy::NeedsReview::ProfileChanged { from: 3, to: 4 }).as_str())
    );
    let d: AutomationDetailDto = roundtrip("automation-get.json");
    assert_eq!(
        d.current_run.as_ref().unwrap().result.label,
        status::running_label(
            crate::automation::store::runs::Phase::Sync,
            2,
            6
        )
    );
}

#[test]
fn the_read_side_sources_carry_no_lease_id_and_no_writes() {
    for source in [
        include_str!("read.rs"),
        include_str!("status.rs"),
        include_str!("store/queue.rs"),
        include_str!("store/runs.rs"),
        include_str!("store/runtime.rs"),
        include_str!("store/controls.rs"),
    ] {
        assert!(!source.contains("lease_id") && !source.contains("leaseId"));
    }
    // A read takes a read lock only: `read.rs` never opens a write transaction.
    let read = include_str!("read.rs");
    let code = read.split("#[cfg(test)]").next().unwrap();
    assert!(!code.contains("with_write"));
    // The read side verifies approvals but never signs, creates or replaces the key.
    for forbidden in ["load_or_create", "create_if_missing(", "compute_mac("] {
        let uses = code.lines().filter(|l| !l.trim_start().starts_with("//")).any(|l| {
            l.contains(forbidden) && !l.contains("fn ") && !l.trim_start().starts_with("///")
        });
        assert!(!uses, "read.rs must not call {forbidden}");
    }
}
