//! Status precedence (design §9.1): one case per row of the table, every pairwise conflict on the
//! precedence chain, and a snapshot of labels and sub-lines (requirements §1/§7).

use super::*;

fn base() -> StatusInput {
    StatusInput {
        review: None,
        kind: AutoKind::Watch,
        background: false,
        runner_live: true,
        enabled: true,
        active: None,
        queued: None,
        attention: None,
        trigger: TriggerState::Watching,
        pending_rerun: false,
        overlap_run_active: false,
        quiet_secs: None,
        idle_secs: 10,
        next_label: None,
    }
}

fn schedule() -> StatusInput {
    StatusInput {
        kind: AutoKind::Schedule,
        trigger: TriggerState::Scheduled,
        ..base()
    }
}

fn run(phase: Phase, done: i64, total: i64) -> ActiveRun {
    ActiveRun {
        phase,
        done,
        total,
        trigger_text: "Scheduled \u{b7} 02:00 WIB".into(),
        stopping: false,
    }
}

fn attention() -> Attention {
    use crate::automation::store::runtime::AttentionKind;
    Attention {
        kind: AttentionKind::Partial,
        run_id: 1043,
        text: "1 failed \u{b7} 1 not started".into(),
    }
}

fn chip(i: &StatusInput) -> (StatusKind, String, Option<String>) {
    let s = compute(i);
    (s.kind, s.label, s.sub)
}

fn sub(s: &str) -> Option<String> {
    Some(s.to_string())
}

// ── One case per row of design §9.1 ──

#[test]
fn row_1_needs_review_with_every_reason() {
    let cases = [
        (
            NeedsReview::ProfileChanged { from: 3, to: 4 },
            "Profile changed (rev 3 \u{2192} 4) \u{b7} held",
        ),
        (
            NeedsReview::PolicyChanged,
            "Settings changed since approval \u{b7} held",
        ),
        (
            NeedsReview::NotApproved,
            "Not approved on this device \u{b7} held",
        ),
        (
            NeedsReview::KeyUnavailable,
            "Approval key unavailable \u{b7} held",
        ),
        (
            NeedsReview::UnsupportedVersion,
            "Approved by a different Ferrum version \u{b7} held",
        ),
    ];
    for (review, text) in cases {
        let i = StatusInput {
            review: Some(review),
            ..base()
        };
        assert_eq!(
            chip(&i),
            (StatusKind::Review, "Needs review".into(), sub(text))
        );
    }
}

#[test]
fn row_2_runner_not_running_only_for_background() {
    let i = StatusInput {
        background: true,
        runner_live: false,
        ..base()
    };
    assert_eq!(
        chip(&i),
        (
            StatusKind::Warn,
            "Runner not running".into(),
            sub("Background automations don't run until the runner starts")
        )
    );
    // The same facts for an app automation: the runner does not matter.
    let app = StatusInput {
        background: false,
        runner_live: false,
        ..base()
    };
    assert_eq!(compute(&app).kind, StatusKind::Watching);
    // A live runner is fine.
    let live = StatusInput {
        background: true,
        runner_live: true,
        ..base()
    };
    assert_eq!(compute(&live).kind, StatusKind::Watching);
}

#[test]
fn row_3_running_labels_and_sub_lines() {
    let comparing = StatusInput {
        active: Some(run(Phase::Compare, 0, 0)),
        ..base()
    };
    assert_eq!(
        chip(&comparing),
        (
            StatusKind::Running,
            "Comparing".into(),
            sub("Scheduled \u{b7} 02:00 WIB")
        )
    );
    for phase in [Phase::Gate, Phase::Plan] {
        let i = StatusInput {
            active: Some(run(phase, 0, 6)),
            ..base()
        };
        assert_eq!(compute(&i).label, "Comparing");
    }
    let syncing = StatusInput {
        active: Some(run(Phase::Sync, 3, 6)),
        ..base()
    };
    assert_eq!(compute(&syncing).label, "Running 3/6");
    let mut stopping = syncing.clone();
    stopping.active.as_mut().unwrap().stopping = true;
    stopping.pending_rerun = true;
    assert_eq!(
        compute(&stopping).sub,
        sub("Stopping after the current file")
    );
    let mut rerun = syncing.clone();
    rerun.pending_rerun = true;
    assert_eq!(compute(&rerun).sub, sub("1 pending rerun after this"));
    let no_ops = StatusInput {
        active: Some(run(Phase::Sync, 0, 0)),
        ..base()
    };
    assert_eq!(compute(&no_ops).label, "Running");
}

#[test]
fn row_4_queued_and_waiting_for_device() {
    let device = StatusInput {
        queued: Some(QueueWait::Device),
        ..base()
    };
    assert_eq!(
        chip(&device),
        (StatusKind::Device, "Waiting for device".into(), None)
    );
    let waits = StatusInput {
        queued: Some(QueueWait::WaitsFor("Nightly photos to NAS".into())),
        ..base()
    };
    assert_eq!(
        chip(&waits),
        (
            StatusKind::Queued,
            "Queued".into(),
            sub("Waits for \"Nightly photos to NAS\" \u{2014} same folders")
        )
    );
    let starting = StatusInput {
        queued: Some(QueueWait::Starting),
        ..base()
    };
    assert_eq!(compute(&starting).sub, sub("Starting\u{2026}"));
    let battery = StatusInput {
        queued: Some(QueueWait::Battery),
        ..base()
    };
    assert_eq!(compute(&battery).sub, sub("Waits for AC power"));
    let metered = StatusInput {
        queued: Some(QueueWait::Metered),
        ..base()
    };
    assert_eq!(compute(&metered).sub, sub("Waits for an unmetered network"));
}

#[test]
fn row_5_needs_attention_holds_new_runs() {
    let i = StatusInput {
        attention: Some(attention()),
        ..base()
    };
    assert_eq!(
        chip(&i),
        (
            StatusKind::Attention,
            "Needs attention".into(),
            sub("1 failed \u{b7} 1 not started \u{b7} new runs held")
        )
    );
}

#[test]
fn row_6_paused_for_watch_and_schedule() {
    let watch = StatusInput {
        enabled: false,
        ..base()
    };
    assert_eq!(
        chip(&watch),
        (StatusKind::Paused, "Paused".into(), sub("Not monitoring"))
    );
    let sched = StatusInput {
        enabled: false,
        ..schedule()
    };
    assert_eq!(
        chip(&sched),
        (StatusKind::Paused, "Paused".into(), sub("No new runs"))
    );
}

#[test]
fn row_7_schedule_trigger_states() {
    let next = StatusInput {
        next_label: Some("Tue 29 Sep, 18:00 WIB".into()),
        ..schedule()
    };
    assert_eq!(
        chip(&next),
        (
            StatusKind::Scheduled,
            "Scheduled".into(),
            sub("Next Tue 29 Sep, 18:00 WIB")
        )
    );
    assert_eq!(
        chip(&schedule()),
        (
            StatusKind::Scheduled,
            "Scheduled".into(),
            sub("No upcoming run")
        )
    );
    let device = StatusInput {
        trigger: TriggerState::Device,
        ..schedule()
    };
    assert_eq!(
        chip(&device),
        (StatusKind::Device, "Waiting for device".into(), None)
    );
    // A watch-only state on a schedule reads as scheduled.
    let odd = StatusInput {
        trigger: TriggerState::Quiet,
        ..schedule()
    };
    assert_eq!(compute(&odd).kind, StatusKind::Scheduled);
}

#[test]
fn row_8_watch_trigger_states() {
    let quiet = StatusInput {
        trigger: TriggerState::Quiet,
        quiet_secs: Some(7),
        ..base()
    };
    assert_eq!(
        chip(&quiet),
        (
            StatusKind::Quiet,
            "Waiting for quiet \u{b7} 7 s".into(),
            sub("Runs after 10 s without changes")
        )
    );
    let cases = [
        (TriggerState::Checking, StatusKind::Checking, "Checking"),
        (
            TriggerState::Device,
            StatusKind::Device,
            "Waiting for device",
        ),
        (TriggerState::Recheck, StatusKind::Recheck, "Needs recheck"),
        (
            TriggerState::Unsupported,
            StatusKind::Unsupported,
            "Unsupported",
        ),
        (TriggerState::Watching, StatusKind::Watching, "Watching"),
    ];
    for (trigger, kind, label) in cases {
        let i = StatusInput { trigger, ..base() };
        assert_eq!(chip(&i), (kind, label.into(), None), "{trigger:?}");
    }
    // `scheduled` is not a watch state.
    let odd = StatusInput {
        trigger: TriggerState::Scheduled,
        ..base()
    };
    assert_eq!(compute(&odd).kind, StatusKind::Watching);
}

#[test]
fn pending_rerun_shows_only_while_an_overlapping_run_is_active() {
    let pending = StatusInput {
        pending_rerun: true,
        overlap_run_active: true,
        trigger: TriggerState::Watching,
        ..base()
    };
    assert_eq!(
        chip(&pending),
        (
            StatusKind::Pending,
            "Pending rerun".into(),
            sub("Another run is using these folders")
        )
    );
    // Pending beats every other watch state.
    for trigger in [
        TriggerState::Quiet,
        TriggerState::Checking,
        TriggerState::Device,
        TriggerState::Recheck,
        TriggerState::Unsupported,
        TriggerState::Watching,
    ] {
        let i = StatusInput {
            trigger,
            ..pending.clone()
        };
        assert_eq!(compute(&i).kind, StatusKind::Pending, "{trigger:?}");
    }
    // Without an overlapping run the flag is not shown.
    let idle = StatusInput {
        overlap_run_active: false,
        ..pending.clone()
    };
    assert_eq!(compute(&idle).kind, StatusKind::Watching);
    // A schedule never shows a pending rerun.
    let sched = StatusInput {
        pending_rerun: true,
        overlap_run_active: true,
        ..schedule()
    };
    assert_eq!(compute(&sched).kind, StatusKind::Scheduled);
}

// ── Pairwise conflicts on the precedence chain ──

/// The chain, strongest first. `set` turns the level's fact on in an input that has none of the
/// others; `kind` is the chip a level shows when it wins.
struct Level {
    name: &'static str,
    kind: StatusKind,
    set: fn(&mut StatusInput),
}

fn chain() -> Vec<Level> {
    vec![
        Level {
            name: "review",
            kind: StatusKind::Review,
            set: |i| i.review = Some(NeedsReview::PolicyChanged),
        },
        Level {
            name: "runner not running",
            kind: StatusKind::Warn,
            set: |i| {
                i.background = true;
                i.runner_live = false;
            },
        },
        Level {
            name: "running",
            kind: StatusKind::Running,
            set: |i| i.active = Some(run(Phase::Sync, 1, 4)),
        },
        Level {
            name: "queued (device)",
            kind: StatusKind::Device,
            set: |i| i.queued = Some(QueueWait::Device),
        },
        Level {
            name: "needs attention",
            kind: StatusKind::Attention,
            set: |i| i.attention = Some(attention()),
        },
        Level {
            name: "paused",
            kind: StatusKind::Paused,
            set: |i| i.enabled = false,
        },
        Level {
            name: "trigger state",
            kind: StatusKind::Quiet,
            set: |i| {
                i.trigger = TriggerState::Quiet;
                i.quiet_secs = Some(5);
            },
        },
    ]
}

#[test]
fn every_pair_on_the_precedence_chain_is_won_by_the_stronger_level() {
    let levels = chain();
    for (a, strong) in levels.iter().enumerate() {
        for weak in &levels[a + 1..] {
            let mut i = base();
            (weak.set)(&mut i);
            (strong.set)(&mut i);
            assert_eq!(
                compute(&i).kind,
                strong.kind,
                "{} must beat {}",
                strong.name,
                weak.name
            );
        }
    }
}

#[test]
fn every_level_alone_shows_its_own_chip() {
    for level in chain() {
        let mut i = base();
        (level.set)(&mut i);
        assert_eq!(compute(&i).kind, level.kind, "{}", level.name);
    }
}

#[test]
fn named_conflicts_of_the_acceptance_criteria() {
    // review beats running
    let i = StatusInput {
        review: Some(NeedsReview::NotApproved),
        active: Some(run(Phase::Sync, 1, 2)),
        ..base()
    };
    assert_eq!(compute(&i).kind, StatusKind::Review);
    // queued-device beats attention
    let i = StatusInput {
        queued: Some(QueueWait::Device),
        attention: Some(attention()),
        ..base()
    };
    assert_eq!(compute(&i).kind, StatusKind::Device);
    // queued (waits) beats attention
    let i = StatusInput {
        queued: Some(QueueWait::Starting),
        attention: Some(attention()),
        ..base()
    };
    assert_eq!(compute(&i).kind, StatusKind::Queued);
    // attention beats paused
    let i = StatusInput {
        attention: Some(attention()),
        enabled: false,
        ..base()
    };
    assert_eq!(compute(&i).kind, StatusKind::Attention);
    // running beats queued
    let i = StatusInput {
        active: Some(run(Phase::Sync, 1, 2)),
        queued: Some(QueueWait::Starting),
        ..base()
    };
    assert_eq!(compute(&i).kind, StatusKind::Running);
    // paused beats the trigger state (watch and schedule)
    for i in [
        StatusInput {
            enabled: false,
            trigger: TriggerState::Quiet,
            ..base()
        },
        StatusInput {
            enabled: false,
            ..schedule()
        },
    ] {
        assert_eq!(compute(&i).kind, StatusKind::Paused);
    }
}

/// `enabled` is not an authorization: a paused automation whose approval no longer verifies still
/// reads Needs review, and an enabled one with a valid approval reads from its facts only.
#[test]
fn the_enabled_flag_never_hides_a_review() {
    for enabled in [true, false] {
        let i = StatusInput {
            enabled,
            review: Some(NeedsReview::ProfileChanged { from: 1, to: 2 }),
            ..base()
        };
        assert_eq!(compute(&i).kind, StatusKind::Review);
    }
}

// ── Run chips ──

#[test]
fn run_outcome_chips_follow_requirements_section_1() {
    let counts = RunCounts {
        ok: 4,
        failed: 1,
        not_started: 1,
        ..Default::default()
    };
    let c = |o, counts: &RunCounts, total, err| {
        let s = outcome_chip(o, counts, total, err);
        (s.kind, s.label, s.sub)
    };
    let done = RunCounts {
        ok: 6,
        ..Default::default()
    };
    assert_eq!(
        c(Outcome::Completed, &done, 6, None),
        (StatusKind::Completed, "Completed".into(), sub("6 of 6"))
    );
    assert_eq!(
        c(Outcome::Completed, &RunCounts::default(), 0, None),
        (
            StatusKind::Completed,
            "Completed".into(),
            sub("Already in sync \u{b7} 0 operations")
        )
    );
    assert_eq!(
        c(Outcome::Partial, &counts, 6, None),
        (
            StatusKind::Partial,
            "Partial".into(),
            sub("4 succeeded \u{b7} 1 failed \u{b7} 1 not started")
        )
    );
    let held = RunCounts {
        held: 1,
        ..Default::default()
    };
    assert_eq!(
        c(Outcome::Conflict, &held, 1, None),
        (
            StatusKind::Attention,
            "Conflict held".into(),
            sub("1 conflict held for review")
        )
    );
    assert_eq!(
        c(
            Outcome::Failed,
            &RunCounts::default(),
            0,
            Some("Access denied")
        ),
        (StatusKind::Attention, "Failed".into(), sub("Access denied"))
    );
    let three = RunCounts {
        ok: 3,
        not_started: 3,
        ..Default::default()
    };
    assert_eq!(
        c(Outcome::Cancelled, &three, 6, None),
        (
            StatusKind::Cancelled,
            "Cancelled".into(),
            sub("3 succeeded \u{b7} 3 not started")
        )
    );
    assert_eq!(
        c(Outcome::Interrupted, &three, 6, None),
        (
            StatusKind::Cancelled,
            "Interrupted".into(),
            sub("3 succeeded \u{b7} 3 not started")
        )
    );
    assert_eq!(
        c(Outcome::Skipped, &RunCounts::default(), 0, None),
        (StatusKind::Skipped, "Skipped".into(), None)
    );
}

#[test]
fn running_chip_counts_file_operations() {
    let counts = RunCounts {
        ok: 2,
        failed: 1,
        ..Default::default()
    };
    let s = running_chip(Phase::Sync, &counts, 6);
    assert_eq!(
        (s.kind, s.label.as_str()),
        (StatusKind::Running, "Running 3/6")
    );
    assert_eq!(
        running_chip(Phase::Compare, &RunCounts::default(), 0).label,
        "Comparing"
    );
}

// ── Snapshot ──

/// The whole `StatusDto` wire form for the strings of the prototype: kind, label and sub-line.
#[test]
fn status_dto_snapshot_matches_the_prototype_strings() {
    let quiet = StatusInput {
        trigger: TriggerState::Quiet,
        quiet_secs: Some(7),
        ..base()
    };
    let queued = StatusInput {
        queued: Some(QueueWait::WaitsFor("Nightly photos to NAS".into())),
        ..base()
    };
    let attention_in = StatusInput {
        attention: Some(attention()),
        ..base()
    };
    let review = StatusInput {
        review: Some(NeedsReview::ProfileChanged { from: 3, to: 4 }),
        ..base()
    };
    let scheduled = StatusInput {
        next_label: Some("Tue 29 Sep, 18:00 WIB".into()),
        ..schedule()
    };
    let snapshot: Vec<serde_json::Value> =
        [&base(), &quiet, &queued, &attention_in, &review, &scheduled]
            .iter()
            .map(|i| serde_json::to_value(compute(i)).unwrap())
            .collect();
    let expected = serde_json::json!([
        { "kind": "watching", "label": "Watching" },
        { "kind": "quiet", "label": "Waiting for quiet \u{b7} 7 s",
          "sub": "Runs after 10 s without changes" },
        { "kind": "queued", "label": "Queued",
          "sub": "Waits for \"Nightly photos to NAS\" \u{2014} same folders" },
        { "kind": "attention", "label": "Needs attention",
          "sub": "1 failed \u{b7} 1 not started \u{b7} new runs held" },
        { "kind": "review", "label": "Needs review",
          "sub": "Profile changed (rev 3 \u{2192} 4) \u{b7} held" },
        { "kind": "scheduled", "label": "Scheduled", "sub": "Next Tue 29 Sep, 18:00 WIB" },
    ]);
    assert_eq!(serde_json::Value::Array(snapshot), expected);
}

#[test]
fn every_kind_serializes_as_camel_case_word() {
    let all = [
        (StatusKind::Watching, "watching"),
        (StatusKind::Quiet, "quiet"),
        (StatusKind::Checking, "checking"),
        (StatusKind::Device, "device"),
        (StatusKind::Recheck, "recheck"),
        (StatusKind::Unsupported, "unsupported"),
        (StatusKind::Scheduled, "scheduled"),
        (StatusKind::Queued, "queued"),
        (StatusKind::Pending, "pending"),
        (StatusKind::Running, "running"),
        (StatusKind::Paused, "paused"),
        (StatusKind::Completed, "completed"),
        (StatusKind::Partial, "partial"),
        (StatusKind::Cancelled, "cancelled"),
        (StatusKind::Skipped, "skipped"),
        (StatusKind::Attention, "attention"),
        (StatusKind::Review, "review"),
        (StatusKind::Warn, "warn"),
    ];
    for (kind, word) in all {
        assert_eq!(serde_json::to_value(kind).unwrap(), word);
    }
}

#[test]
fn utc_labels_are_exact() {
    assert_eq!(format_utc_short(0), "Thu 1 Jan, 00:00 UTC");
    assert_eq!(format_utc_short(1_760_000_000_000), "Thu 9 Oct, 08:53 UTC");
    // A leap day and the weekday after it.
    assert_eq!(format_utc_short(1_709_164_800_000), "Thu 29 Feb, 00:00 UTC");
    // Before the epoch still formats (no panic).
    assert_eq!(format_utc_short(-1), "Wed 31 Dec, 23:59 UTC");
}
