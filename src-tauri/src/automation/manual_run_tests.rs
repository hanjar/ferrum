//! Tests of the manual preview and run core (Automation P0.2): previews, the per-run native
//! confirmation, leases, stale previews, and the wire shapes against the contract fixtures.

use super::*;
use crate::automation::commands::{ProfileDto, SaveRequest, SaveResultDto, save_in};
use crate::automation::model::{
    Draft, DraftGuard, PolicyDraft, WireCompare, WireDeletePolicy, WireMode, WireSide,
};
use crate::sync_engine::OpRecord;
use crate::sync_engine::env::EngineEnv;
use crate::sync_engine::roots::{FsProbe, RootProbe};
use crate::sync_engine::test_support::{Rig, set_mtime, test_env, tree, write};
use std::cell::Cell;
use std::fs;

fn draft(rig: &Rig, mode: WireMode, policy: WireDeletePolicy) -> Draft {
    Draft {
        name: "Backup".into(),
        policy: PolicyDraft {
            left: rig.left.to_string_lossy().into_owned(),
            right: rig.right.to_string_lossy().into_owned(),
            mode,
            from: (mode != WireMode::TwoWay).then_some(WireSide::Left),
            propagate_deletes: None,
            delete_policy: policy,
            versioning_root: (policy == WireDeletePolicy::Versioning)
                .then(|| rig.versions.to_string_lossy().into_owned()),
            compare: WireCompare::SizeAndDate,
            include: vec![],
            exclude: vec![],
            guard: DraftGuard {
                enabled: true,
                max_count: 50,
                max_percent: 20,
            },
        },
    }
}

fn save(rig: &Rig, d: Draft) -> ProfileDto {
    let probe: Arc<dyn RootProbe> = Arc::new(FsProbe);
    match save_in(
        &rig.auto,
        SaveRequest {
            id: None,
            expected_revision: None,
            draft: d,
        },
        &probe,
        |_| true,
        1_000,
    )
    .unwrap()
    {
        SaveResultDto::Saved { profile } => *profile,
        other => panic!("saved expected: {other:?}"),
    }
}

fn preview(
    rig: &Rig,
    board: &PreviewBoard,
    id: &str,
    now: i64,
) -> Result<PreviewDto, CommandError> {
    preview_in(
        &rig.auto,
        &rig.baselines,
        board,
        &test_env(),
        &crate::sync_engine::NoSink,
        id,
        false,
        now,
    )
}

#[allow(clippy::too_many_arguments)]
fn start(
    rig: &Rig,
    board: &PreviewBoard,
    id: &str,
    preview_id: &str,
    overwrite: bool,
    answer: ConfirmAnswer,
    asked: &Cell<u32>,
    now: i64,
) -> Result<PreparedRun, CommandError> {
    start_run_in(
        &rig.auto,
        board,
        &rig.lease_env,
        &test_env(),
        id,
        preview_id,
        overwrite,
        false,
        |prompt| {
            asked.set(asked.get() + 1);
            assert!(prompt.text.starts_with(confirm_text::PERMANENT_WARNING));
            answer
        },
        now,
    )
}

fn seed(rig: &Rig) {
    write(&rig.left.join("a.txt"), b"new");
    write(&rig.right.join("extra.txt"), b"x");
    write(&rig.left.join("o.txt"), b"left version");
    write(&rig.right.join("o.txt"), b"old");
    set_mtime(&rig.left.join("o.txt"), 1_900_000_500);
    set_mtime(&rig.right.join("o.txt"), 1_900_000_000);
}

fn execute(rig: &Rig, prepared: PreparedRun) -> EngineReport {
    let env: EngineEnv = test_env();
    prepared.execute(&rig.baselines, &env, &crate::sync_engine::NoSink)
}

fn leases_held(rig: &Rig) -> usize {
    rig.auto
        .with_read(crate::automation::store::leases::all)
        .unwrap()
        .len()
}

// The preview is read-only and carries the counts the run will be tied to.
#[test]
fn preview_is_read_only_and_counts_deletes_and_overwrites() {
    let rig = Rig::new();
    seed(&rig);
    let p = save(
        &rig,
        draft(&rig, WireMode::Mirror, WireDeletePolicy::Permanent),
    );
    let board = PreviewBoard::default();
    let before = (tree(&rig.left), tree(&rig.right));
    let dto = preview(&rig, &board, &p.id, 10).unwrap();
    assert_eq!(dto.summary.deletes, 1);
    assert_eq!(dto.summary.overwrites, 1);
    assert_eq!(dto.guard, GuardVerdict::Ok);
    assert!(model::is_profile_id(&dto.preview_id));
    assert_eq!(before, (tree(&rig.left), tree(&rig.right)));
    assert_eq!(board.len(), 1);
    // No lease is taken by a preview.
    assert_eq!(leases_held(&rig), 0);
}

// SE-1 / design §10.3: a permanent-delete run asks natively, per run, with the preview's
// delete count; "no" runs nothing and keeps the preview; "yes" runs.
#[test]
fn permanent_run_is_confirmed_natively_per_run() {
    let rig = Rig::new();
    seed(&rig);
    let p = save(
        &rig,
        draft(&rig, WireMode::Mirror, WireDeletePolicy::Permanent),
    );
    let board = PreviewBoard::default();
    let pv = preview(&rig, &board, &p.id, 10).unwrap();
    let asked = Cell::new(0);
    let no = start(
        &rig,
        &board,
        &p.id,
        &pv.preview_id,
        true,
        ConfirmAnswer::No,
        &asked,
        20,
    );
    assert_eq!(no.err().unwrap().code, "canceled");
    assert_eq!(asked.get(), 1);
    let busy = start(
        &rig,
        &board,
        &p.id,
        &pv.preview_id,
        true,
        ConfirmAnswer::Busy,
        &asked,
        20,
    );
    assert_eq!(busy.err().unwrap().code, "busy");
    assert!(rig.right.join("extra.txt").exists(), "nothing ran");
    assert_eq!(
        leases_held(&rig),
        0,
        "refused attempts released their lease"
    );
    // The refused attempts kept the preview: now the answer is yes.
    let yes = start(
        &rig,
        &board,
        &p.id,
        &pv.preview_id,
        true,
        ConfirmAnswer::Yes,
        &asked,
        20,
    )
    .unwrap();
    assert_eq!(asked.get(), 3);
    assert_eq!(leases_held(&rig), 1, "the run holds its lease");
    let report = execute(&rig, yes);
    assert!(
        matches!(report.outcome, EngineOutcome::Completed { .. }),
        "{:?}",
        report.outcome
    );
    assert_eq!(tree(&rig.right), ["a.txt", "o.txt"]);
    // The preview is single use.
    let again = start(
        &rig,
        &board,
        &p.id,
        &pv.preview_id,
        true,
        ConfirmAnswer::Yes,
        &asked,
        30,
    );
    assert_eq!(again.err().unwrap().code, "stale");
    // The lease was released with the run.
    assert_eq!(leases_held(&rig), 0);
}

// A policy that is not `permanent` never opens the native dialog.
#[test]
fn non_permanent_runs_never_ask() {
    let rig = Rig::new();
    seed(&rig);
    let p = save(
        &rig,
        draft(&rig, WireMode::Mirror, WireDeletePolicy::Versioning),
    );
    let board = PreviewBoard::default();
    let pv = preview(&rig, &board, &p.id, 10).unwrap();
    let asked = Cell::new(0);
    let run = start_run_in(
        &rig.auto,
        &board,
        &rig.lease_env,
        &test_env(),
        &p.id,
        &pv.preview_id,
        true,
        false,
        |_| {
            asked.set(asked.get() + 1);
            ConfirmAnswer::Yes
        },
        20,
    )
    .unwrap();
    assert_eq!(asked.get(), 0);
    let report = execute(&rig, run);
    assert!(matches!(report.outcome, EngineOutcome::Completed { .. }));
    assert!(rig.versions.exists());
}

// The overwrite confirmation is required when the preview lists overwrites.
#[test]
fn overwrites_must_be_confirmed() {
    let rig = Rig::new();
    seed(&rig);
    let p = save(
        &rig,
        draft(&rig, WireMode::OneWay, WireDeletePolicy::Permanent),
    );
    let board = PreviewBoard::default();
    let pv = preview(&rig, &board, &p.id, 10).unwrap();
    assert_eq!(pv.summary.overwrites, 1);
    let asked = Cell::new(0);
    let r = start(
        &rig,
        &board,
        &p.id,
        &pv.preview_id,
        false,
        ConfirmAnswer::Yes,
        &asked,
        20,
    );
    assert_eq!(r.err().unwrap().code, "invalid_input");
    assert_eq!(fs::read_to_string(rig.right.join("o.txt")).unwrap(), "old");
}

// A preview that is unknown, expired or malformed is `stale` / `invalid_input`.
#[test]
fn missing_expired_and_malformed_ids_are_refused() {
    let rig = Rig::new();
    seed(&rig);
    let p = save(
        &rig,
        draft(&rig, WireMode::OneWay, WireDeletePolicy::Permanent),
    );
    let board = PreviewBoard::default();
    let asked = Cell::new(0);
    let unknown = model::new_profile_id().unwrap();
    let r = start(
        &rig,
        &board,
        &p.id,
        &unknown,
        true,
        ConfirmAnswer::Yes,
        &asked,
        20,
    );
    assert_eq!(r.err().unwrap().code, "stale");
    let pv = preview(&rig, &board, &p.id, 10).unwrap();
    let late = 10 + PREVIEW_TTL_MS + 1;
    let r = start(
        &rig,
        &board,
        &p.id,
        &pv.preview_id,
        true,
        ConfirmAnswer::Yes,
        &asked,
        late,
    );
    assert_eq!(r.err().unwrap().code, "stale");
    // Garbage ids never reach a lookup.
    let r = start(
        &rig,
        &board,
        "not-an-id",
        &pv.preview_id,
        true,
        ConfirmAnswer::Yes,
        &asked,
        20,
    );
    assert_eq!(r.err().unwrap().code, "invalid_input");
    let r = start(
        &rig,
        &board,
        &p.id,
        "..\\x",
        true,
        ConfirmAnswer::Yes,
        &asked,
        20,
    );
    assert_eq!(r.err().unwrap().code, "invalid_input");
}

// The engine refuses a plan that differs from the previewed one.
#[test]
fn a_plan_that_changed_since_the_preview_is_refused_by_the_engine() {
    let rig = Rig::new();
    seed(&rig);
    let p = save(
        &rig,
        draft(&rig, WireMode::OneWay, WireDeletePolicy::Permanent),
    );
    let board = PreviewBoard::default();
    let pv = preview(&rig, &board, &p.id, 10).unwrap();
    write(
        &rig.left.join("brand-new.txt"),
        b"appeared after the preview",
    );
    let asked = Cell::new(0);
    let run = start(
        &rig,
        &board,
        &p.id,
        &pv.preview_id,
        true,
        ConfirmAnswer::Yes,
        &asked,
        20,
    )
    .unwrap();
    let report = execute(&rig, run);
    match report.outcome {
        EngineOutcome::Failed { error } => assert_eq!(error.code, "plan_changed"),
        other => panic!("expected Failed, got {other:?}"),
    }
    assert!(!rig.right.join("brand-new.txt").exists());
    assert!(!rig.right.join("a.txt").exists(), "nothing ran");
}

// RQ-2: folders in use by another lease: `lease_busy`, nothing touched.
#[test]
fn folders_in_use_refuse_the_run() {
    let rig = Rig::new();
    seed(&rig);
    let p = save(
        &rig,
        draft(&rig, WireMode::OneWay, WireDeletePolicy::Permanent),
    );
    let board = PreviewBoard::default();
    let pv = preview(&rig, &board, &p.id, 10).unwrap();
    let profile: ProfilePolicy =
        serde_json::from_str(&load(&rig.auto, &p.id).unwrap().0.policy).unwrap();
    let GuardOutcome::Acquired(_other) = LeaseGuard::acquire(
        &rig.auto,
        &rig.lease_env,
        HolderKind::Runner,
        Some(9),
        &[profile.left.path.clone(), profile.right.path.clone()],
        None,
    )
    .unwrap() else {
        panic!("other lease");
    };
    let asked = Cell::new(0);
    let r = start(
        &rig,
        &board,
        &p.id,
        &pv.preview_id,
        true,
        ConfirmAnswer::Yes,
        &asked,
        20,
    );
    assert_eq!(r.err().unwrap().code, "lease_busy");
    assert!(!rig.right.join("a.txt").exists());
}

// A profile whose root is gone: `not_ready`, no path in the message.
#[test]
fn a_missing_root_is_not_ready_without_leaking_the_path() {
    let rig = Rig::new();
    seed(&rig);
    let p = save(
        &rig,
        draft(&rig, WireMode::OneWay, WireDeletePolicy::Permanent),
    );
    fs::remove_dir_all(&rig.right).unwrap();
    let board = PreviewBoard::default();
    let err = preview(&rig, &board, &p.id, 10).err().unwrap();
    assert_eq!(err.code, "not_ready");
    assert!(!err.message.contains('\\'), "{}", err.message);
}

// LC-3 / design §7: an unusable store runs nothing.
#[test]
fn an_unavailable_store_refuses_the_run() {
    let rig = Rig::new();
    let dead = Arc::new(Store::unavailable());
    let board = PreviewBoard::default();
    let id = model::new_profile_id().unwrap();
    let r = start_run_in(
        &dead,
        &board,
        &rig.lease_env,
        &test_env(),
        &id,
        &id,
        true,
        false,
        |_| ConfirmAnswer::Yes,
        1,
    );
    assert_eq!(r.err().unwrap().code, "store_unavailable");
}

#[test]
fn run_ids_strictly_increase() {
    let a = next_run_id(5_000);
    let b = next_run_id(5_000);
    let c = next_run_id(1);
    assert!(a < b && b < c);
}

// ── Wire shapes: the fixtures are the contract (read by Rust and TypeScript) ──

fn fixture(name: &str) -> serde_json::Value {
    let dir = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../contract-fixtures/automation/"
    );
    let text =
        std::fs::read_to_string(format!("{dir}{name}")).unwrap_or_else(|e| panic!("{name}: {e}"));
    serde_json::from_str(&text).unwrap()
}

#[test]
fn preview_answers_serialize_to_the_fixtures() {
    let ok = PreviewDto {
        preview_id: "7c1d2e3f-4a5b-4c6d-8e9f-0a1b2c3d4e5f".into(),
        summary: PlanSummary {
            total: 14,
            to_right: 9,
            to_left: 0,
            overwrites: 2,
            deletes: 3,
            conflicts: 1,
            skipped: 1,
            user_skipped: 0,
        },
        conflicts: 1,
        guard: GuardVerdict::Ok,
        blockers: vec![],
    };
    assert_eq!(
        serde_json::to_value(&ok).unwrap(),
        fixture("sync-profile-preview.json")
    );
    let tripped = PreviewDto {
        preview_id: "0b9a8c7d-6e5f-4a3b-9c2d-1e0f9a8b7c6d".into(),
        summary: PlanSummary {
            total: 52,
            to_right: 1,
            deletes: 51,
            ..PlanSummary::default()
        },
        conflicts: 0,
        guard: GuardVerdict::Tripped {
            side: model::Side::Right,
            deletes: 51,
            entries: 100,
            percent: 51,
        },
        blockers: vec![],
    };
    assert_eq!(
        serde_json::to_value(&tripped).unwrap(),
        fixture("sync-profile-preview-tripped.json")
    );
    let blocked = PreviewDto {
        preview_id: String::new(),
        summary: PlanSummary::default(),
        conflicts: 0,
        guard: GuardVerdict::Ok,
        blockers: vec!["truncated".into()],
    };
    assert_eq!(
        serde_json::to_value(&blocked).unwrap(),
        fixture("sync-profile-preview-blocked.json")
    );
    assert_eq!(
        serde_json::to_value(RunStartedDto {
            run_id: 1_790_000_000_123
        })
        .unwrap(),
        fixture("sync-profile-run.json")
    );
}

#[test]
fn run_events_serialize_to_the_fixtures() {
    use crate::sync_engine::{Counts, OpStatus, Phase, SyncAction};
    let phase = RunEventDto {
        run_id: 1_790_000_000_123,
        phase: Some(Phase::Comparing),
        counts: None,
        result: None,
    };
    assert_eq!(
        serde_json::to_value(&phase).unwrap(),
        fixture("sync-profile-run-event-phase.json")
    );
    let counts = RunEventDto {
        run_id: 1_790_000_000_123,
        phase: None,
        counts: Some(Counts {
            total: 6,
            done: 3,
            failed: 0,
            skipped: 0,
            changed: 0,
            held: 0,
            not_started: 3,
        }),
        result: None,
    };
    assert_eq!(
        serde_json::to_value(&counts).unwrap(),
        fixture("sync-profile-run-event-counts.json")
    );
    let rec = |index, name: &str, action, status, reason| OpRecord {
        index,
        name: name.into(),
        action,
        status,
        reason,
    };
    let result = RunEventDto {
        run_id: 1_790_000_000_123,
        phase: None,
        counts: None,
        result: Some(RunResultDto {
            kind: "partial",
            counts: Some(Counts {
                total: 6,
                done: 3,
                failed: 1,
                skipped: 0,
                changed: 1,
                held: 0,
                not_started: 1,
            }),
            error_code: Some("access_denied"),
            error_text: Some(
                "Not retried automatically — permission errors need you. Fix access, then Recheck."
                    .into(),
            ),
            interrupted: None,
            pending_rerun: true,
            settle: SettleState::Saved { entries: 12 },
            problems: vec![
                rec(
                    2,
                    "docs/report.docx",
                    SyncAction::UpdateRight,
                    OpStatus::Changed,
                    None,
                ),
                rec(
                    3,
                    "photos/img1.jpg",
                    SyncAction::CopyToRight,
                    OpStatus::Failed,
                    Some(crate::sync_engine::errors::Reason::AccessDenied),
                ),
                rec(
                    5,
                    "photos/img2.jpg",
                    SyncAction::CopyToRight,
                    OpStatus::NotStarted,
                    None,
                ),
            ],
        }),
    };
    assert_eq!(
        serde_json::to_value(&result).unwrap(),
        fixture("sync-profile-run-event-result.json")
    );
}

// No field of any answer carries a path or an operating-system message except the documented
// run error text.
#[test]
fn result_dto_keeps_paths_out_except_the_run_error_text() {
    let rig = Rig::new();
    write(&rig.right.join("precious.txt"), b"p");
    let p = save(
        &rig,
        draft(&rig, WireMode::Mirror, WireDeletePolicy::Permanent),
    );
    let board = PreviewBoard::default();
    let pv = preview(&rig, &board, &p.id, 10).unwrap();
    assert!(matches!(pv.guard, GuardVerdict::EmptySource { .. }));
    let asked = Cell::new(0);
    let run = start(
        &rig,
        &board,
        &p.id,
        &pv.preview_id,
        true,
        ConfirmAnswer::Yes,
        &asked,
        20,
    )
    .unwrap();
    let report = execute(&rig, run);
    let mut dto = serde_json::to_value(result_dto(&report)).unwrap();
    assert_eq!(dto["errorCode"], "empty_source");
    let text = dto.as_object_mut().unwrap().remove("errorText").unwrap();
    assert_eq!(text, "Source folder is empty — nothing deleted");
    assert!(!dto.to_string().contains('\\'), "{dto}");
    assert!(rig.right.join("precious.txt").exists());
}
