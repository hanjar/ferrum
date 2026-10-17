//! The profile run (Automation design §5.1, §5.4, §9.4).
//!
//! `run` = gate → fresh compare (profile filter, entry cap) → plan (`build_profile_sync_plan`,
//! `apply_decisions(.., Profile)`) → `validate_profile_plan` on the FINAL plan → delete guard →
//! operations → settle (baseline saved only while the lease is alive).
//!
//! Safety properties, in order of importance:
//! - A plan that breaks the profile contract (a folder with a recursive action) is refused in
//!   release builds too; the engine never calls anything recursive on user data.
//! - Folders only ever get `mkdir` (non-recursive) or `remove_dir` (non-recursive, empty only).
//! - Parents are created and walked without following reparse points; a file where a folder is
//!   needed fails the item and overwrites nothing.
//! - Every operation is re-checked against the plan's facts right before it runs (RQ-7.5).
//! - The lease is checked before every operation; a lost lease stops the run.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::archive_extract::publish::{self, DirHandle, PublishError};
use crate::automation::lease::LeaseGuard;
use crate::automation::model::{CompareMode, DeletePolicy, ProfilePolicy, Side, SyncMode};
use crate::compare::{
    CompareDirsReport, Ctl, DirDiffEntry, DirDiffState, DirOpts, compare_dirs_filtered_with,
};
use crate::ops::CancelToken;
use crate::sync_baseline::{BaselineDto, LoadResult, RootPair, save_with_guard};
use crate::sync_engine::delete_policy::{Deleter, Removal, RunRoots, check_gate};
use crate::sync_engine::env::EngineEnv;
use crate::sync_engine::errors::{
    Classified, ErrorClass, Reason, UNKNOWN_STREAK_LIMIT, backoff, classify_io,
};
use crate::sync_engine::filters::ProfileFilter;
use crate::sync_engine::nofollow::{self, ChainError, classify_publish, open_chain, split_rel};
use crate::sync_engine::opset::{
    GuardVerdict, Op, OpKind, OpStatus, build_ops, evaluate_guard, guard_text, plan_fingerprint,
};
use crate::sync_engine::plan::{
    MTIME_TOLERANCE_MS, PROFILE_ENTRY_CAP, PlanMode, PlanSummary, SyncAction, SyncDirection,
    SyncOptions, apply_decisions, build_profile_sync_plan, describe_profile_blockers, plan_summary,
    sync_plan_blockers, validate_profile_plan,
};
use crate::sync_engine::settle::{ItemOutcome, reconcile_baseline};
use crate::sync_engine::versioning::{Expect, VersionStore};
use crate::sync_engine::write_guard::{check_under, final_base};
use crate::sync_incremental::{IncrementalInfo, prepare};

/// Operations per copy batch and the bytes after which a batch is closed (the just-in-time check
/// of a file is never older than one batch).
const BATCH_MAX_ITEMS: usize = 256;
const BATCH_MAX_BYTES: u64 = 64 * 1024 * 1024;
/// How often counts are pushed to the sink (design §5.1: at most 10 per second).
const COUNTS_EVERY: Duration = Duration::from_millis(100);

// ── Public surface ──

/// Where a run is, for the sink and the UI ("Comparing folders…", "Syncing 3 of 6").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Phase {
    Checking,
    Comparing,
    Syncing,
    Stopping,
    Settling,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Counts {
    pub total: u32,
    pub done: u32,
    pub failed: u32,
    pub skipped: u32,
    pub changed: u32,
    pub held: u32,
    pub not_started: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpRecord {
    pub index: usize,
    pub name: String,
    pub action: SyncAction,
    pub status: OpStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<Reason>,
}

/// Receives progress. All methods default to no-ops.
pub trait EngineSink: Send + Sync {
    fn phase(&self, _phase: Phase) {}
    /// A status change of one operation.
    fn op(&self, _record: &OpRecord) {}
    /// Counts so far; the engine calls this at most 10 times per second (plus the final one).
    fn counts(&self, _counts: &Counts) {}
}

pub struct NoSink;
impl EngineSink for NoSink {}

/// A run error: a closed code and the documented run error text (design §7: the only text that
/// may carry the profile's own root path; it is what history stores).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EngineError {
    pub code: &'static str,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum InterruptReason {
    /// A root (or the versioning root) disappeared (RQ-7.4).
    RootOffline,
    /// The run's lease was lost: another holder may be working on the folders.
    LeaseLost,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineOutcome {
    Completed {
        counts: Counts,
    },
    Partial {
        counts: Counts,
        error: EngineError,
    },
    ConflictHeld {
        counts: Counts,
    },
    /// Before any op succeeded, or a guard refused.
    Failed {
        error: EngineError,
    },
    Cancelled {
        counts: Counts,
    },
    Interrupted {
        counts: Counts,
        why: InterruptReason,
    },
    NothingToDo,
    DryRun {
        plan: PlanSummary,
        conflicts: u32,
        guard: GuardVerdict,
    },
}

/// What the baseline settle did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum SettleState {
    NotAttempted,
    Saved {
        entries: u32,
    },
    Unchanged {
        entries: u32,
    },
    /// The verifying compare was incomplete or failed; the baseline stays as it was.
    Skipped {
        reason: &'static str,
    },
    /// The save was refused or failed (for example the lease was lost); the baseline stays.
    SaveFailed {
        code: &'static str,
    },
}

#[derive(Debug)]
pub struct EngineReport {
    pub outcome: EngineOutcome,
    pub ops: Vec<OpRecord>,
    /// A source or target changed during the run: another comparison is due (RQ-7.5).
    pub pending_rerun: bool,
    pub settle: SettleState,
    /// Composition of the final plan (names + actions), for tying a run to its preview.
    pub fingerprint: Option<String>,
    pub incremental: Option<IncrementalInfo>,
    /// Compare blockers as closed codes (`truncated`, `cancelled`, `errors`).
    pub blockers: Vec<&'static str>,
}

impl EngineReport {
    fn bare(outcome: EngineOutcome) -> Self {
        EngineReport {
            outcome,
            ops: Vec::new(),
            pending_rerun: false,
            settle: SettleState::NotAttempted,
            fingerprint: None,
            incremental: None,
            blockers: Vec::new(),
        }
    }

    fn failed(code: &'static str, text: impl Into<String>) -> Self {
        Self::bare(EngineOutcome::Failed {
            error: EngineError {
                code,
                text: text.into(),
            },
        })
    }
}

/// The user's NATIVE answer "yes, run it" to a tripped delete guard, for ONE plan and ONE delete
/// count. Only `automation::manual_run` creates it (after the native dialog); a source test keeps
/// every other module - in particular the future host - from constructing one. Without it a
/// tripped guard ends the run as `Failed("guard_tripped")` (SP-5.1, design §14 Q4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardAck {
    fingerprint: String,
    deletes: u32,
}

impl GuardAck {
    pub(crate) fn from_native_yes(fingerprint: String, deletes: u32) -> Self {
        GuardAck {
            fingerprint,
            deletes,
        }
    }

    fn covers(&self, fingerprint: &str, verdict: &GuardVerdict) -> bool {
        matches!(verdict, GuardVerdict::Tripped { deletes, .. }
            if *deletes == self.deletes && self.fingerprint == fingerprint)
    }
}

pub struct EngineRequest<'a> {
    pub profile: &'a ProfilePolicy,
    /// Baselines are read from, and (under the lease) written to, this store.
    pub baselines: &'a crate::sync_baseline::SyncBaselineStore,
    pub cancel: CancelToken,
    pub sink: &'a dyn EngineSink,
    /// Retries of a transient error (0-3, RQ-7.1).
    pub retries: u8,
    pub run_id: i64,
    /// Compare and plan only: touches nothing, needs no lease.
    pub dry_run: bool,
    /// Compare entry cap (design §5.3); `PROFILE_ENTRY_CAP` for profile runs.
    pub entry_cap: usize,
    /// Use the indexer journal to skip content reads (IX-1). Needs compare by content.
    pub incremental: bool,
    /// The run's lease (required unless `dry_run`). It must cover both roots and the
    /// versioning root.
    pub lease: Option<&'a LeaseGuard>,
    /// A manual run: the fingerprint of the plan the user looked at. A different plan is
    /// refused before any operation.
    pub expect_plan: Option<&'a str>,
    /// The native confirmation of a tripped guard (manual runs only). The empty-source rule
    /// can never be acknowledged.
    pub guard_ack: Option<GuardAck>,
    pub env: &'a EngineEnv,
}

impl<'a> EngineRequest<'a> {
    /// A request with the defaults of a profile run.
    pub fn new(
        profile: &'a ProfilePolicy,
        baselines: &'a crate::sync_baseline::SyncBaselineStore,
        sink: &'a dyn EngineSink,
        env: &'a EngineEnv,
    ) -> Self {
        EngineRequest {
            profile,
            baselines,
            cancel: CancelToken::default(),
            sink,
            retries: 2,
            run_id: 0,
            dry_run: false,
            entry_cap: PROFILE_ENTRY_CAP,
            incremental: false,
            lease: None,
            expect_plan: None,
            guard_ack: None,
            env,
        }
    }
}

// ── Plan options ──

/// `OneWay{from:Left}` = leftToRight, no delete; `Mirror` adds `deleteExtras`; `TwoWay` = both
/// (design §5.2).
pub fn sync_options(mode: &SyncMode) -> SyncOptions {
    match mode {
        SyncMode::OneWay { from } => SyncOptions {
            direction: direction_of(*from),
            delete_extras: false,
            propagate_deletes: false,
            profile: true,
        },
        SyncMode::Mirror { from } => SyncOptions {
            direction: direction_of(*from),
            delete_extras: true,
            propagate_deletes: false,
            profile: true,
        },
        SyncMode::TwoWay { propagate_deletes } => SyncOptions {
            direction: SyncDirection::Both,
            delete_extras: false,
            propagate_deletes: *propagate_deletes,
            profile: true,
        },
    }
}

fn direction_of(from: Side) -> SyncDirection {
    match from {
        Side::Left => SyncDirection::LeftToRight,
        Side::Right => SyncDirection::RightToLeft,
    }
}

// ── The run ──

/// Execute one profile run. Never panics on filesystem errors; every failure ends in an outcome.
pub fn run(req: &EngineRequest<'_>) -> EngineReport {
    let profile = req.profile;
    req.sink.phase(Phase::Checking);

    if !req.dry_run && req.lease.is_none() {
        return EngineReport::failed(
            "lease_required",
            "The run needs the folder lease before it can start. Nothing was changed.",
        );
    }

    // Gate (design §10.4 step 7-8): roots exist and are the recorded folders, Recycle Bin and
    // versioning prerequisites. Nothing has been touched yet.
    let roots = match check_gate(profile, req.env) {
        Ok(r) => r,
        Err(e) => return EngineReport::failed(e.code(), e.text(profile)),
    };
    if let Some(lease) = req.lease
        && !lease.covers(&roots.lease_roots())
    {
        return EngineReport::failed(
            "lease_required",
            "The run's folder lease does not cover its folders. Nothing was changed.",
        );
    }

    // T8: right after the gate (which looked at the roots by path) both roots are opened and
    // proven by volume serial + folder id ON THE HANDLE, and held until the run ends. A folder
    // swapped in at the same path from here on is refused, and one that is swapped later cannot
    // be renamed or replaced while the pin is held.
    req.env.hooks.after_gate();
    let _pins = match pin_roots(profile, &roots) {
        Ok(p) => p,
        Err(ChainError::Identity) => {
            return EngineReport::failed(
                "identity_mismatch",
                "A folder of this profile is not the folder it was created for. Nothing was changed.",
            );
        }
        Err(_) => {
            return EngineReport::failed(
                "root_unavailable",
                "A folder of this profile could not be opened. Nothing was changed.",
            );
        }
    };

    let filter = match ProfileFilter::new(&profile.filters.include, &profile.filters.exclude) {
        Ok(f) => f,
        Err(_) => {
            return EngineReport::failed(
                "profile_invalid",
                "The profile's filters are not valid. Nothing was changed.",
            );
        }
    };
    let by_content = profile.compare == CompareMode::Content;

    // Fresh compare, always (design §5.4 step 1).
    req.sink.phase(Phase::Comparing);
    let (report, incremental) =
        match compare_tree(req, &roots, &filter, by_content, req.incremental) {
            Ok(x) => x,
            Err(CompareFailure::Cancelled) => {
                return EngineReport::bare(EngineOutcome::Cancelled {
                    counts: Counts::default(),
                });
            }
            Err(CompareFailure::Failed(code, text)) => return EngineReport::failed(code, text),
        };
    let mut blockers = Vec::new();
    if report.truncated {
        blockers.push("truncated");
    }
    if report.cancelled {
        blockers.push("cancelled");
    }
    if !report.errors.is_empty() {
        blockers.push("errors");
    }
    if !sync_plan_blockers(&report).is_empty() {
        if req.cancel.is_cancelled() {
            return EngineReport::bare(EngineOutcome::Cancelled {
                counts: Counts::default(),
            });
        }
        let code = if report.truncated {
            "cap_exceeded"
        } else {
            "blocked"
        };
        let mut r = EngineReport::failed(code, describe_profile_blockers(&report));
        r.blockers = blockers;
        r.incremental = incremental;
        return r;
    }

    let pair = RootPair {
        left_root: roots.left.clone(),
        right_root: roots.right.clone(),
    };
    let previous = match req.baselines.load(&pair) {
        Ok(LoadResult::Ok { baseline }) => Some(baseline),
        _ => None,
    };

    // Plan: only through the profile entry points (design §5.4 contract item 4).
    let opts = sync_options(&profile.mode);
    let built = build_profile_sync_plan(&report, &opts, previous.as_ref(), &filter);
    // Unattended runs hold conflicts; this call is the contract's second entry point and
    // re-asserts the profile invariants.
    let mut final_plan = apply_decisions(&built, &HashMap::new(), PlanMode::Profile);
    req.env.hooks.on_final_plan(&mut final_plan);
    // Contract item 6: always on, on the FINAL plan.
    if let Err(violations) = validate_profile_plan(&final_plan) {
        let mut r = EngineReport::failed(
            "plan_invalid",
            format!(
                "The plan broke the folder safety rules ({} items). Nothing was changed.",
                violations.len()
            ),
        );
        r.incremental = incremental;
        return r;
    }

    let ops = build_ops(&final_plan, &report);
    // Defence in depth: files are deleted only on the side that RECEIVES in this mode (the
    // Recycle Bin gate and the guard reason about exactly those sides).
    let receiving = crate::automation::model::receiving_sides(&profile.mode);
    if ops.iter().any(|o| {
        !o.held
            && matches!(o.kind, OpKind::DeleteFile | OpKind::RemoveEmptyDir)
            && !receiving.contains(&o.target)
    }) {
        return EngineReport::failed(
            "plan_invalid",
            "The plan would delete on a side that does not receive in this mode. Nothing was changed.",
        );
    }
    let fingerprint = plan_fingerprint(&final_plan);
    let verdict = evaluate_guard(&profile.guard, &report, &ops);
    let held = ops.iter().filter(|o| o.held).count() as u32;
    let records = initial_records(&ops);

    let finish =
        |outcome: EngineOutcome, settle: SettleState, records: Vec<OpRecord>| EngineReport {
            outcome,
            ops: records,
            pending_rerun: false,
            settle,
            fingerprint: Some(fingerprint.clone()),
            incremental: incremental.clone(),
            blockers: Vec::new(),
        };

    if req.dry_run {
        return finish(
            EngineOutcome::DryRun {
                plan: plan_summary(&final_plan),
                conflicts: held,
                guard: verdict,
            },
            SettleState::NotAttempted,
            records,
        );
    }

    let acknowledged = req
        .guard_ack
        .as_ref()
        .is_some_and(|ack| ack.covers(&fingerprint, &verdict));
    if verdict.trips() && !acknowledged {
        return finish(
            EngineOutcome::Failed {
                error: EngineError {
                    code: match verdict {
                        GuardVerdict::EmptySource { .. } => "empty_source",
                        _ => "guard_tripped",
                    },
                    text: guard_text(&verdict, &profile.left.path, &profile.right.path),
                },
            },
            SettleState::NotAttempted,
            records,
        );
    }
    if let Some(expected) = req.expect_plan
        && expected != fingerprint
    {
        return EngineReport::failed(
            "plan_changed",
            "The folders changed since the preview. Preview again before running.",
        );
    }

    let actionable = ops.iter().filter(|o| !o.held).count();
    if actionable == 0 {
        // Nothing to copy or delete. The verified state may still be worth recording (first
        // baseline of an already identical pair); the report just taken is the verification.
        let settle = settle_with(
            req,
            &roots,
            &previous,
            &report,
            &final_plan,
            &HashMap::new(),
        );
        let outcome = if held > 0 {
            let counts = Counts {
                total: held,
                held,
                ..Counts::default()
            };
            EngineOutcome::ConflictHeld { counts }
        } else {
            EngineOutcome::NothingToDo
        };
        return finish(outcome, settle, records);
    }

    req.env.hooks.after_plan();
    req.sink.phase(Phase::Syncing);

    let handles = match open_handles(profile, &roots, written_sides(&ops)) {
        Ok(h) => h,
        Err(ChainError::Identity) => {
            return EngineReport::failed(
                "identity_mismatch",
                "A folder of this profile is not the folder it was created for. Nothing was changed.",
            );
        }
        Err(e) => {
            return EngineReport::failed(
                "root_unavailable",
                format!(
                    "A folder could not be opened ({}). Nothing was changed.",
                    e.classify().reason.text()
                ),
            );
        }
    };
    let stamp = req.env.clock.local_stamp();
    let versions = match (&profile.delete_policy, &roots.versioning) {
        (DeletePolicy::Versioning { .. }, Some(root)) => {
            match VersionStore::open(root, &stamp, req.run_id) {
                Ok(v) => Some(v),
                Err(_) => {
                    return EngineReport::failed(
                        "versioning_root_invalid",
                        "The versioning folder could not be opened. Nothing was deleted or overwritten.",
                    );
                }
            }
        }
        _ => None,
    };

    let mut runner = Runner::new(req, &roots, handles, versions, ops, records);
    runner.execute();
    let outcome = runner.outcome();
    let after_outcomes = runner.outcomes_for_settle();

    // Settle: after any run that changed something, while the lease is still held.
    let settle = if runner.counts.done > 0 {
        req.sink.phase(Phase::Settling);
        settle_after_run(
            req,
            &roots,
            &previous,
            &final_plan,
            &after_outcomes,
            by_content,
            &filter,
        )
    } else {
        SettleState::NotAttempted
    };
    req.sink.counts(&runner.counts);
    EngineReport {
        outcome,
        ops: runner.records.clone(),
        pending_rerun: runner.pending_rerun,
        settle,
        fingerprint: Some(fingerprint),
        incremental,
        blockers: Vec::new(),
    }
}

fn initial_records(ops: &[Op]) -> Vec<OpRecord> {
    ops.iter()
        .map(|o| OpRecord {
            index: o.index,
            name: o.name.clone(),
            action: o.action,
            status: if o.held {
                OpStatus::Held
            } else {
                OpStatus::NotStarted
            },
            reason: None,
        })
        .collect()
}

// ── Compare ──

enum CompareFailure {
    Cancelled,
    Failed(&'static str, String),
}

fn compare_tree(
    req: &EngineRequest<'_>,
    roots: &RunRoots,
    filter: &ProfileFilter,
    by_content: bool,
    incremental: bool,
) -> Result<(CompareDirsReport, Option<IncrementalInfo>), CompareFailure> {
    let mut ctl = Ctl::none();
    // Sync must read bytes, never trust a cached verdict.
    ctl.no_cache = true;
    let prepared = if incremental && by_content {
        let mut source = req
            .env
            .changes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let p = prepare(&roots.left, &roots.right, req.baselines, &mut **source);
        ctl.content_hint = p.hint.clone();
        Some(p)
    } else {
        None
    };
    let opts = DirOpts {
        by_content,
        recursive: true,
        max_entries: req.entry_cap,
    };
    let done = AtomicBool::new(false);
    let token = req.cancel.clone();
    let flag = ctl.cancel_flag();
    let result = std::thread::scope(|scope| {
        scope.spawn(|| {
            while !done.load(Ordering::SeqCst) {
                if token.is_cancelled() {
                    flag.store(true, Ordering::SeqCst);
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        });
        let r = compare_dirs_filtered_with(
            &roots.left,
            &roots.right,
            opts,
            filter,
            &ctl,
            req.entry_cap,
        );
        done.store(true, Ordering::SeqCst);
        r
    });
    let mut report = match result {
        Ok(r) => r,
        Err(e) => {
            if req.cancel.is_cancelled() {
                return Err(CompareFailure::Cancelled);
            }
            let class = crate::sync_engine::errors::classify_fs(&e, true);
            return Err(CompareFailure::Failed(
                "root_unavailable",
                format!(
                    "The folders could not be compared ({}). Nothing was changed.",
                    class.reason.text()
                ),
            ));
        }
    };
    let info = prepared.map(|p| {
        let mut info = p.info;
        info.content_skipped = p.hint.as_ref().map_or(0, |h| h.skipped());
        report.journal = p.marks;
        report.incremental = Some(info.clone());
        info
    });
    Ok((report, info))
}

// ── Settle ──

fn outcome_map(records: &[OpRecord]) -> HashMap<String, ItemOutcome> {
    let mut map = HashMap::new();
    for r in records {
        match r.status {
            OpStatus::Done => {
                map.insert(r.name.clone(), ItemOutcome::Ok);
            }
            OpStatus::Failed => {
                map.insert(r.name.clone(), ItemOutcome::Failed);
            }
            _ => {}
        }
    }
    map
}

/// As TypeScript `verifiedByContent`: after a by-content compare a pair that is still
/// `newerLeft`/`newerRight` has identical bytes and differs only in mtime.
fn verified_by_content(report: &CompareDirsReport) -> CompareDirsReport {
    let mut out = report.clone();
    for e in &mut out.entries {
        if !e.is_dir && matches!(e.state, DirDiffState::NewerLeft | DirDiffState::NewerRight) {
            e.state = DirDiffState::Same;
        }
    }
    out
}

fn settle_after_run(
    req: &EngineRequest<'_>,
    roots: &RunRoots,
    previous: &Option<BaselineDto>,
    plan: &[crate::sync_engine::plan::SyncPlanItem],
    outcomes: &HashMap<String, ItemOutcome>,
    by_content: bool,
    filter: &ProfileFilter,
) -> SettleState {
    let Some(lease) = req.lease else {
        return SettleState::NotAttempted;
    };
    if lease.is_lost() {
        return SettleState::SaveFailed {
            code: "lease_required",
        };
    }
    // A fresh verifying compare in the same mode. It is not tied to the cancel token: a stopped
    // run still records what it completed.
    let settle_req = EngineRequest {
        cancel: CancelToken::default(),
        ..clone_request(req)
    };
    let after = match compare_tree(&settle_req, roots, filter, by_content, req.incremental) {
        Ok((r, _)) => r,
        Err(_) => {
            return SettleState::Skipped {
                reason: "verifyFailed",
            };
        }
    };
    if !sync_plan_blockers(&after).is_empty() {
        return SettleState::Skipped {
            reason: "notVerified",
        };
    }
    settle_with(req, roots, previous, &after, plan, outcomes)
}

fn clone_request<'a>(req: &EngineRequest<'a>) -> EngineRequest<'a> {
    EngineRequest {
        profile: req.profile,
        baselines: req.baselines,
        cancel: req.cancel.clone(),
        sink: req.sink,
        retries: req.retries,
        run_id: req.run_id,
        dry_run: req.dry_run,
        entry_cap: req.entry_cap,
        incremental: req.incremental,
        lease: req.lease,
        expect_plan: req.expect_plan,
        guard_ack: req.guard_ack.clone(),
        env: req.env,
    }
}

fn settle_with(
    req: &EngineRequest<'_>,
    roots: &RunRoots,
    previous: &Option<BaselineDto>,
    after: &CompareDirsReport,
    plan: &[crate::sync_engine::plan::SyncPlanItem],
    outcomes: &HashMap<String, ItemOutcome>,
) -> SettleState {
    let Some(lease) = req.lease else {
        // A dry run (or a run without a lease) never writes a baseline.
        return SettleState::NotAttempted;
    };
    let by_content = req.profile.compare == CompareMode::Content;
    let verified = if by_content {
        verified_by_content(after)
    } else {
        after.clone()
    };
    let mut next = reconcile_baseline(previous.as_ref(), &verified, plan, outcomes);
    // A folder that was removed and is gone from both sides leaves no baseline entry behind.
    let present: std::collections::HashSet<&str> =
        after.entries.iter().map(|e| e.name.as_str()).collect();
    for item in plan {
        if matches!(
            item.action,
            SyncAction::RemoveEmptyDirLeft | SyncAction::RemoveEmptyDirRight
        ) && outcomes.get(&item.name) == Some(&ItemOutcome::Ok)
            && !present.contains(item.name.as_str())
        {
            next.entries.remove(&item.name);
        }
    }
    let entries = next.entries.len() as u32;
    let same = previous
        .as_ref()
        .is_some_and(|p| p.entries == next.entries && p.journal == next.journal)
        || (previous.is_none() && next.entries.is_empty() && next.journal.is_none());
    if same {
        return SettleState::Unchanged { entries };
    }
    let pair = RootPair {
        left_root: roots.left.clone(),
        right_root: roots.right.clone(),
    };
    match save_with_guard(
        req.baselines,
        lease,
        &|p| req.env.probe.canonical(p),
        &pair,
        &next,
    ) {
        Ok(()) => SettleState::Saved { entries },
        Err(e) => SettleState::SaveFailed { code: e.code },
    }
}

// ── Executor ──

/// Opened root folders. Only a side that is WRITTEN (an operation targets it) is opened: a
/// read-only source (a protected drive, a share without write access) must keep working.
struct Handles {
    left: Option<DirHandle>,
    right: Option<DirHandle>,
}

impl Handles {
    fn root(&self, side: Side) -> Option<&DirHandle> {
        match side {
            Side::Left => self.left.as_ref(),
            Side::Right => self.right.as_ref(),
        }
    }
}

/// Which sides the executable operations write to.
fn written_sides(ops: &[Op]) -> [bool; 2] {
    let mut sides = [false, false];
    for op in ops.iter().filter(|o| !o.held) {
        match op.target {
            Side::Left => sides[0] = true,
            Side::Right => sides[1] = true,
        }
    }
    sides
}

/// Read-only, identity-verified handles of both roots, held for the whole run.
fn pin_roots(profile: &ProfilePolicy, roots: &RunRoots) -> Result<[DirHandle; 2], ChainError> {
    Ok([
        nofollow::open_verified_root(&roots.left, &profile.left.identity, false)?,
        nofollow::open_verified_root(&roots.right, &profile.right.identity, false)?,
    ])
}

/// Write-capable, identity-verified handles of the sides the run writes to.
fn open_handles(
    profile: &ProfilePolicy,
    roots: &RunRoots,
    sides: [bool; 2],
) -> Result<Handles, ChainError> {
    Ok(Handles {
        left: if sides[0] {
            Some(nofollow::open_verified_root(
                &roots.left,
                &profile.left.identity,
                true,
            )?)
        } else {
            None
        },
        right: if sides[1] {
            Some(nofollow::open_verified_root(
                &roots.right,
                &profile.right.identity,
                true,
            )?)
        } else {
            None
        },
    })
}

/// A target side without an opened root cannot happen (the sides come from the operations);
/// if it ever does, the operation fails closed.
fn no_root() -> Classified {
    Classified {
        class: ErrorClass::PerItem,
        reason: Reason::Unknown,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stop {
    Cancelled,
    Interrupted(InterruptReason),
    Systemic(Reason),
}

enum OpResult {
    Done,
    Skipped(Option<Reason>),
    Changed,
    Failed(Classified),
    Cancelled,
}

/// What a just-in-time look at one path saw.
#[derive(Debug, Clone, Copy)]
struct Fact {
    is_dir: bool,
    link: bool,
    size: u64,
    mtime: i64,
}

fn fact_of(path: &Path) -> Result<Option<Fact>, std::io::Error> {
    match fs::symlink_metadata(path) {
        Ok(m) => {
            let link = is_link(&m);
            let is_dir = m.is_dir() && !link;
            Ok(Some(Fact {
                is_dir,
                link,
                size: if is_dir { 0 } else { m.len() },
                mtime: crate::fs_service::modified_ms(&m),
            }))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

#[cfg(windows)]
fn is_link(m: &fs::Metadata) -> bool {
    // `FileType::is_symlink` is true for name-surrogate reparse tags only (symlink, junction):
    // other reparse data (cloud placeholders, deduplication) is an ordinary file or folder here.
    m.file_type().is_symlink()
}

#[cfg(not(windows))]
fn is_link(m: &fs::Metadata) -> bool {
    m.file_type().is_symlink()
}

/// What the plan saw on `side` for this entry.
fn expected(entry: &DirDiffEntry, side: Side) -> Option<Fact> {
    let present = match side {
        Side::Left => entry.state != DirDiffState::RightOnly,
        Side::Right => entry.state != DirDiffState::LeftOnly,
    };
    present.then_some(Fact {
        is_dir: entry.is_dir,
        link: false,
        size: match side {
            Side::Left => entry.left_size,
            Side::Right => entry.right_size,
        },
        mtime: match side {
            Side::Left => entry.left_modified_ms,
            Side::Right => entry.right_modified_ms,
        },
    })
}

#[derive(Debug, PartialEq, Eq)]
enum Jit {
    Match,
    Changed,
    Link,
}

fn compare_fact(expected: &Option<Fact>, actual: &Option<Fact>) -> Jit {
    match (expected, actual) {
        (None, None) => Jit::Match,
        (Some(_), None) | (None, Some(_)) => Jit::Changed,
        (Some(e), Some(a)) => {
            if a.link {
                return Jit::Link;
            }
            if e.is_dir != a.is_dir {
                return Jit::Changed;
            }
            if e.is_dir {
                return Jit::Match;
            }
            if e.size == a.size && (e.mtime - a.mtime).abs() <= MTIME_TOLERANCE_MS {
                Jit::Match
            } else {
                Jit::Changed
            }
        }
    }
}

struct Runner<'a> {
    req: &'a EngineRequest<'a>,
    roots: &'a RunRoots,
    handles: Handles,
    versions: Option<VersionStore>,
    ops: Vec<Op>,
    records: Vec<OpRecord>,
    /// Versioning overwrites whose old target is already in the versions tree.
    aside: Vec<bool>,
    counts: Counts,
    stop: Option<Stop>,
    unknown_streak: u32,
    pending_rerun: bool,
    copy_failed: bool,
    first_failure: Option<Reason>,
    last_counts: Instant,
    /// Fired by the run's watcher on user cancel or lost lease.
    child: CancelToken,
    /// FER-562: final path of each pinned root folder, looked up once per run.
    bases: [Option<PathBuf>; 2],
}

impl<'a> Runner<'a> {
    fn new(
        req: &'a EngineRequest<'a>,
        roots: &'a RunRoots,
        handles: Handles,
        versions: Option<VersionStore>,
        ops: Vec<Op>,
        records: Vec<OpRecord>,
    ) -> Self {
        let mut counts = Counts {
            total: ops.len() as u32,
            ..Counts::default()
        };
        for r in &records {
            match r.status {
                OpStatus::Held => counts.held += 1,
                _ => counts.not_started += 1,
            }
        }
        let aside = vec![false; ops.len()];
        Runner {
            req,
            roots,
            handles,
            versions,
            ops,
            records,
            aside,
            counts,
            stop: None,
            unknown_streak: 0,
            pending_rerun: false,
            copy_failed: false,
            first_failure: None,
            last_counts: Instant::now() - COUNTS_EVERY,
            child: CancelToken::default(),
            bases: [None, None],
        }
    }

    /// FER-562: the final path the OS reports for the opened root of `side`, cached: every
    /// operation is then checked against it by string work only (the chain below a root never
    /// follows a reparse point, so root + relative name is where an operation lands).
    fn base(&mut self, side: Side) -> Result<PathBuf, Classified> {
        let i = usize::from(side == Side::Right);
        if let Some(b) = &self.bases[i] {
            return Ok(b.clone());
        }
        let root = self.handles.root(side).ok_or_else(no_root)?;
        let b = final_base(root)?;
        self.bases[i] = Some(b.clone());
        Ok(b)
    }

    fn root_path(&self, side: Side) -> &str {
        match side {
            Side::Left => &self.roots.left,
            Side::Right => &self.roots.right,
        }
    }

    fn abs_path(&self, side: Side, comps: &[&str]) -> PathBuf {
        let mut p = PathBuf::from(self.root_path(side));
        for c in comps {
            p.push(c);
        }
        p
    }

    // ── bookkeeping ──

    fn set(&mut self, idx: usize, status: OpStatus, reason: Option<Reason>) {
        let old = self.records[idx].status;
        if old != status {
            match old {
                OpStatus::NotStarted | OpStatus::Running => {
                    self.counts.not_started = self.counts.not_started.saturating_sub(1);
                }
                _ => {}
            }
            match status {
                OpStatus::Done => self.counts.done += 1,
                OpStatus::Failed => self.counts.failed += 1,
                OpStatus::Skipped => self.counts.skipped += 1,
                OpStatus::Changed => self.counts.changed += 1,
                OpStatus::NotStarted | OpStatus::Running => self.counts.not_started += 1,
                OpStatus::Held => {}
            }
        }
        self.records[idx].status = status;
        self.records[idx].reason = reason;
        self.req.sink.op(&self.records[idx]);
        if self.last_counts.elapsed() >= COUNTS_EVERY {
            self.last_counts = Instant::now();
            self.req.sink.counts(&self.counts);
        }
    }

    /// Checks before every operation. `false` = the run stops (the reason is in `self.stop`).
    fn may_continue(&mut self) -> bool {
        if self.stop.is_some() {
            return false;
        }
        if self.req.cancel.is_cancelled() {
            self.stop = Some(Stop::Cancelled);
            self.req.sink.phase(Phase::Stopping);
            return false;
        }
        if self.req.lease.is_none_or(|l| l.is_lost()) {
            self.stop = Some(Stop::Interrupted(InterruptReason::LeaseLost));
            return false;
        }
        if !self.roots_alive() {
            self.stop = Some(Stop::Interrupted(InterruptReason::RootOffline));
            return false;
        }
        true
    }

    fn roots_alive(&self) -> bool {
        let live = &self.req.env.liveness;
        live.alive(&self.roots.left)
            && live.alive(&self.roots.right)
            && self.roots.versioning.as_deref().is_none_or(|v| {
                // Created on first use: only a root that exists must still be a folder.
                !Path::new(v).exists() || live.alive(v)
            })
    }

    fn begin(&mut self, idx: usize) {
        self.req
            .env
            .hooks
            .before_op(idx, &self.ops[idx].name.clone());
        self.set(idx, OpStatus::Running, None);
    }

    fn finish(&mut self, idx: usize, result: OpResult) {
        match result {
            OpResult::Done => {
                self.unknown_streak = 0;
                self.set(idx, OpStatus::Done, None);
            }
            OpResult::Skipped(reason) => self.set(idx, OpStatus::Skipped, reason),
            OpResult::Changed => {
                self.pending_rerun = true;
                self.set(idx, OpStatus::Changed, None);
            }
            OpResult::Cancelled => {
                self.set(idx, OpStatus::NotStarted, None);
                // A copy stopped because the lease was lost is an interruption, not a user stop.
                let lost = self.req.lease.is_none_or(|l| l.is_lost());
                self.stop.get_or_insert(if lost {
                    Stop::Interrupted(InterruptReason::LeaseLost)
                } else {
                    Stop::Cancelled
                });
            }
            OpResult::Failed(c) => {
                if matches!(self.ops[idx].kind, OpKind::CopyNew | OpKind::Overwrite) {
                    self.copy_failed = true;
                }
                self.first_failure.get_or_insert(c.reason);
                self.set(idx, OpStatus::Failed, Some(c.reason));
                match c.class {
                    ErrorClass::Systemic => {
                        self.stop.get_or_insert(Stop::Systemic(c.reason));
                    }
                    ErrorClass::DeviceLost => {
                        self.stop
                            .get_or_insert(Stop::Interrupted(InterruptReason::RootOffline));
                    }
                    ErrorClass::Unknown => {
                        self.unknown_streak += 1;
                        if self.unknown_streak >= UNKNOWN_STREAK_LIMIT {
                            self.stop.get_or_insert(Stop::Systemic(c.reason));
                        }
                    }
                    ErrorClass::Transient | ErrorClass::PerItem => self.unknown_streak = 0,
                }
                // A failure with a vanished root is a device loss, not a per-item problem.
                if self.stop.is_none() && !self.roots_alive() {
                    self.stop = Some(Stop::Interrupted(InterruptReason::RootOffline));
                }
            }
        }
    }

    // ── just-in-time check (RQ-7.5, T10) ──

    fn jit(&self, op: &Op, comps: &[&str]) -> Result<Jit, Classified> {
        for side in [Side::Left, Side::Right] {
            let mut want = expected(&op.entry, side);
            if side == op.target && self.aside[op.index] {
                want = None;
            }
            let have = fact_of(&self.abs_path(side, comps)).map_err(|e| classify_io(&e, false))?;
            match compare_fact(&want, &have) {
                Jit::Match => {}
                other => return Ok(other),
            }
        }
        Ok(Jit::Match)
    }

    /// The size and time the plan saw of the target-side file of `op` (checked again on the
    /// file's own handle at the moment of the delete or move).
    fn expect_of(&self, op: &Op) -> Expect {
        let fact = expected(&op.entry, op.target);
        Expect {
            size: fact.map_or(0, |f| f.size),
            mtime_ms: fact.map_or(0, |f| f.mtime),
        }
    }

    // ── phases ──

    /// Run all operations. A single watcher thread lives for the whole run and fires the
    /// run's token when the user cancels or the lease is lost.
    fn execute(&mut self) {
        let done = AtomicBool::new(false);
        let (user, lease, child) = (self.req.cancel.clone(), self.req.lease, self.child.clone());
        std::thread::scope(|scope| {
            scope.spawn(|| {
                while !done.load(Ordering::SeqCst) {
                    if user.is_cancelled() || lease.is_none_or(|l| l.is_lost()) {
                        child.cancel();
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
            });
            self.execute_phases();
            done.store(true, Ordering::SeqCst);
        });
    }

    fn execute_phases(&mut self) {
        // 1. folders, shallow to deep
        let mut dirs: Vec<usize> = self
            .ops
            .iter()
            .filter(|o| !o.held && o.kind == OpKind::CreateDir)
            .map(|o| o.index)
            .collect();
        dirs.sort_by_cached_key(|&i| (self.ops[i].depth(), self.ops[i].name.clone()));
        for idx in dirs {
            if !self.may_continue() {
                return;
            }
            self.run_single(idx);
        }

        // 2. copies and updates, toRight first, grouped by (target side, destination folder)
        let mut copies: Vec<usize> = self
            .ops
            .iter()
            .filter(|o| !o.held && matches!(o.kind, OpKind::CopyNew | OpKind::Overwrite))
            .map(|o| o.index)
            .collect();
        copies.sort_by_cached_key(|&i| {
            let o = &self.ops[i];
            (
                o.target != Side::Right,
                parent_of(&o.name).to_string(),
                o.name.clone(),
            )
        });
        let mut at = 0;
        while at < copies.len() {
            if !self.may_continue() {
                return;
            }
            let first = copies[at];
            if self.ops[first].kind == OpKind::Overwrite {
                self.run_single(first);
                at += 1;
                continue;
            }
            // A batch of new files for one folder.
            let key = (
                self.ops[first].target,
                parent_of(&self.ops[first].name).to_string(),
            );
            let mut batch = vec![first];
            let mut bytes = self.ops[first].entry_size_of_source();
            let mut next = at + 1;
            while next < copies.len() && batch.len() < BATCH_MAX_ITEMS && bytes < BATCH_MAX_BYTES {
                let o = &self.ops[copies[next]];
                if o.kind != OpKind::CopyNew || o.target != key.0 || parent_of(&o.name) != key.1 {
                    break;
                }
                bytes += o.entry_size_of_source();
                batch.push(copies[next]);
                next += 1;
            }
            self.run_copy_batch(&batch);
            at = next;
        }

        // 3-4. deletes and folder removal only when every copy went well (same rule as
        // `runSyncPlan`).
        if self.copy_failed || self.stop.is_some() {
            return;
        }
        let mut deletes: Vec<usize> = self
            .ops
            .iter()
            .filter(|o| !o.held && o.kind == OpKind::DeleteFile)
            .map(|o| o.index)
            .collect();
        deletes.sort_by_cached_key(|&i| self.ops[i].name.clone());
        for idx in deletes {
            if !self.may_continue() {
                return;
            }
            self.run_single(idx);
        }
        let mut rmdirs: Vec<usize> = self
            .ops
            .iter()
            .filter(|o| !o.held && o.kind == OpKind::RemoveEmptyDir)
            .map(|o| o.index)
            .collect();
        rmdirs.sort_by_cached_key(|&i| {
            (
                std::cmp::Reverse(self.ops[i].depth()),
                self.ops[i].name.clone(),
            )
        });
        for idx in rmdirs {
            if !self.may_continue() {
                return;
            }
            self.run_single(idx);
        }
    }

    /// One operation with transient retries (RQ-7.1): each retry waits (2/10/30 s), then the
    /// just-in-time check runs again before the attempt.
    fn run_single(&mut self, idx: usize) {
        self.begin(idx);
        let mut attempt = 0u8;
        loop {
            let res = self.attempt(idx);
            match res {
                OpResult::Failed(c)
                    if c.class == ErrorClass::Transient && attempt < self.req.retries.min(3) =>
                {
                    attempt += 1;
                    if !self
                        .req
                        .env
                        .sleeper
                        .sleep(backoff(attempt), &self.req.cancel)
                    {
                        self.finish(idx, OpResult::Cancelled);
                        return;
                    }
                    if !self.may_continue() {
                        self.finish(idx, OpResult::Cancelled);
                        return;
                    }
                }
                other => {
                    self.finish(idx, other);
                    return;
                }
            }
        }
    }

    fn attempt(&mut self, idx: usize) -> OpResult {
        let op = self.ops[idx].clone();
        let comps = match split_rel(&op.name) {
            Ok(c) => c,
            Err(c) => return OpResult::Failed(c),
        };
        match op.kind {
            OpKind::CreateDir => self.attempt_create_dir(&op, &comps),
            OpKind::RemoveEmptyDir => self.attempt_remove_dir(&op, &comps),
            OpKind::DeleteFile => self.attempt_delete(&op, &comps),
            OpKind::CopyNew | OpKind::Overwrite => {
                let mut results = self.attempt_copy(&[idx]);
                results
                    .pop()
                    .map(|(_, r)| r)
                    .unwrap_or(OpResult::Failed(Classified {
                        class: ErrorClass::PerItem,
                        reason: Reason::Unknown,
                    }))
            }
        }
    }

    fn attempt_create_dir(&mut self, op: &Op, comps: &[&str]) -> OpResult {
        match self.jit(op, comps) {
            Err(c) => return OpResult::Failed(c),
            Ok(Jit::Changed) => return OpResult::Changed,
            Ok(Jit::Link) => return OpResult::Skipped(Some(Reason::LinkUnsupported)),
            Ok(Jit::Match) => {}
        }
        let (last, parents) = comps.split_last().expect("validated, non-empty");
        // FER-562: the folder being created (and the parents made on the way) must not land in
        // a protected or Startup folder. Checked before anything is created, then again against
        // the final path of the folder that was really opened.
        let base = match self.base(op.target) {
            Ok(b) => b,
            Err(c) => return OpResult::Failed(c),
        };
        if let Err(c) = check_under(&*self.req.env.guard, &base, comps) {
            return OpResult::Failed(c);
        }
        let Some(root) = self.handles.root(op.target) else {
            return OpResult::Failed(no_root());
        };
        let held = match open_chain(root, parents, true) {
            Ok(h) => h,
            Err(e) => return OpResult::Failed(e.classify()),
        };
        let held_base = match final_base(held.dir()) {
            Ok(b) => b,
            Err(c) => return OpResult::Failed(c),
        };
        if let Err(c) = check_under(&*self.req.env.guard, &held_base, &[last]) {
            return OpResult::Failed(c);
        }
        let name = std::ffi::OsStr::new(last);
        match publish::create_dir_new(held.dir(), name) {
            Ok(_) => OpResult::Done,
            // The folder is already there (idempotent): fine if it is a real folder.
            Err(PublishError::Exists) => match publish::child_dir(held.dir(), name) {
                Ok(_) => OpResult::Done,
                Err(PublishError::Reparse) => OpResult::Failed(Classified {
                    class: ErrorClass::PerItem,
                    reason: Reason::LinkInPath,
                }),
                Err(e) => OpResult::Failed(classify_publish(&e)),
            },
            Err(e) => OpResult::Failed(classify_publish(&e)),
        }
    }

    fn attempt_remove_dir(&mut self, op: &Op, comps: &[&str]) -> OpResult {
        // Only the OTHER side is checked: the folder itself may be gone or not empty, both are
        // normal skips (design §5.4 contract item 3).
        let other = op.source();
        let have = match fact_of(&self.abs_path(other, comps)) {
            Ok(h) => h,
            Err(e) => return OpResult::Failed(classify_io(&e, false)),
        };
        if have.is_some() {
            return OpResult::Changed;
        }
        let (last, parents) = comps.split_last().expect("validated, non-empty");
        // FER-562: checked against the root's final path before the chain is opened.
        let base = match self.base(op.target) {
            Ok(b) => b,
            Err(c) => return OpResult::Failed(c),
        };
        if let Err(c) = check_under(&*self.req.env.guard, &base, comps) {
            return OpResult::Failed(c);
        }
        let Some(root) = self.handles.root(op.target) else {
            return OpResult::Failed(no_root());
        };
        let held = match open_chain(root, parents, false) {
            Ok(h) => h,
            Err(ChainError::NotFound) => return OpResult::Skipped(Some(Reason::NotFound)),
            Err(ChainError::Link) => return OpResult::Skipped(Some(Reason::LinkInPath)),
            Err(e) => return OpResult::Failed(e.classify()),
        };
        match publish::remove_empty_dir(held.dir(), std::ffi::OsStr::new(last)) {
            Ok(true) => OpResult::Done,
            Ok(false) => OpResult::Skipped(Some(Reason::NotEmpty)),
            Err(PublishError::NotFound) => OpResult::Skipped(Some(Reason::NotFound)),
            Err(PublishError::Reparse) => OpResult::Skipped(Some(Reason::LinkInPath)),
            Err(e) => OpResult::Failed(classify_publish(&e)),
        }
    }

    fn deleter<'t>(&'t self, cancel: &'t CancelToken) -> Deleter<'t> {
        Deleter {
            policy: &self.req.profile.delete_policy,
            versions: self.versions.as_ref(),
            env: self.req.env,
            cancel,
        }
    }

    /// Run `f` with the run's token, which fires when the user cancels OR the lease is lost: a
    /// copy that is already in flight (a big batch, a cross-volume move) must stop as soon as
    /// another holder may be working on the folders. One watcher per RUN feeds this token
    /// (see `execute`); an operation costs no thread.
    fn guarded<T>(&self, f: impl FnOnce(&CancelToken) -> T) -> T {
        f(&self.child)
    }

    fn attempt_delete(&mut self, op: &Op, comps: &[&str]) -> OpResult {
        match self.jit(op, comps) {
            Err(c) => return OpResult::Failed(c),
            Ok(Jit::Changed) => return OpResult::Changed,
            Ok(Jit::Link) => return OpResult::Skipped(Some(Reason::LinkUnsupported)),
            Ok(Jit::Match) => {}
        }
        let (last, parents) = comps.split_last().expect("validated, non-empty");
        // FER-562: a delete (or the move into the versions tree) inside a protected or Startup
        // folder is refused before the file is touched.
        let base = match self.base(op.target) {
            Ok(b) => b,
            Err(c) => return OpResult::Failed(c),
        };
        if let Err(c) = check_under(&*self.req.env.guard, &base, comps) {
            return OpResult::Failed(c);
        }
        let Some(root) = self.handles.root(op.target) else {
            return OpResult::Failed(no_root());
        };
        self.req.env.hooks.after_jit(op.index, &op.name);
        let expect = self.expect_of(op);
        let res = self.guarded(|cancel| {
            self.deleter(cancel).delete_file(
                op.target,
                root,
                self.root_path(op.target),
                parents,
                last,
                expect,
            )
        });
        match res {
            Ok(Removal::Done) => OpResult::Done,
            Ok(Removal::Changed) => OpResult::Changed,
            Err(c) => self.after_move_error(c),
        }
    }

    /// A cancelled cross-volume copy comes back as a failure; the token says what it was.
    fn after_move_error(&self, c: Classified) -> OpResult {
        let stopped = self.req.cancel.is_cancelled() || self.req.lease.is_none_or(|l| l.is_lost());
        if stopped && c.reason == Reason::VerifyFailed {
            return OpResult::Cancelled;
        }
        OpResult::Failed(c)
    }

    fn run_copy_batch(&mut self, batch: &[usize]) {
        for &idx in batch {
            self.begin(idx);
        }
        let mut results = self.attempt_copy(batch);
        // A call that failed as a whole (for example one source vanished before the copy
        // engine's own scan) is retried per file, so one bad file cannot sink its neighbours.
        let whole_failure = batch.len() > 1
            && results
                .iter()
                .all(|(_, r)| matches!(r, OpResult::Failed(c) if c.class == ErrorClass::PerItem));
        if whole_failure {
            results = batch
                .iter()
                .map(|&i| (i, self.attempt_single_copy(i)))
                .collect();
        }
        for (idx, res) in results {
            match res {
                OpResult::Failed(c) if c.class == ErrorClass::Transient && self.req.retries > 0 => {
                    self.retry_copy(idx, c);
                }
                other => self.finish(idx, other),
            }
        }
    }

    fn attempt_single_copy(&mut self, idx: usize) -> OpResult {
        let mut r = self.attempt_copy(&[idx]);
        r.pop().map(|(_, x)| x).unwrap_or(OpResult::Cancelled)
    }

    /// Retry one file after a transient failure inside a batch.
    fn retry_copy(&mut self, idx: usize, first: Classified) {
        let mut attempt = 0u8;
        let mut last = first;
        while attempt < self.req.retries.min(3) {
            attempt += 1;
            if !self
                .req
                .env
                .sleeper
                .sleep(backoff(attempt), &self.req.cancel)
                || !self.may_continue()
            {
                self.finish(idx, OpResult::Cancelled);
                return;
            }
            match self.attempt_single_copy(idx) {
                OpResult::Failed(c) if c.class == ErrorClass::Transient => last = c,
                other => {
                    self.finish(idx, other);
                    return;
                }
            }
        }
        self.finish(idx, OpResult::Failed(last));
    }

    /// Copy the files of `batch` (same target side and destination folder; one file for an
    /// overwrite). Returns one result per op.
    fn attempt_copy(&mut self, batch: &[usize]) -> Vec<(usize, OpResult)> {
        let mut out: Vec<(usize, OpResult)> = Vec::new();
        let mut ready: Vec<(usize, String)> = Vec::new();
        let first = self.ops[batch[0]].clone();
        let target = first.target;
        let source = first.source();
        let overwrite = first.kind == OpKind::Overwrite;
        let versioning = matches!(
            self.req.profile.delete_policy,
            DeletePolicy::Versioning { .. }
        );

        let mut comps_of: HashMap<usize, Vec<String>> = HashMap::new();
        for &idx in batch {
            let op = self.ops[idx].clone();
            let comps = match split_rel(&op.name) {
                Ok(c) => c.into_iter().map(String::from).collect::<Vec<_>>(),
                Err(c) => {
                    out.push((idx, OpResult::Failed(c)));
                    continue;
                }
            };
            let refs: Vec<&str> = comps.iter().map(String::as_str).collect();
            match self.jit(&op, &refs) {
                Err(c) => out.push((idx, OpResult::Failed(c))),
                Ok(Jit::Changed) => out.push((idx, OpResult::Changed)),
                Ok(Jit::Link) => out.push((idx, OpResult::Skipped(Some(Reason::LinkUnsupported)))),
                Ok(Jit::Match) => {
                    let src = self.abs_path(source, &refs).to_string_lossy().into_owned();
                    comps_of.insert(idx, comps);
                    ready.push((idx, src));
                }
            }
        }
        if ready.is_empty() {
            return out;
        }

        // Parent folders of the destination: created no-follow, and held open for the copy.
        let sample: Vec<&str> = comps_of[&ready[0].0].iter().map(String::as_str).collect();
        let (_, parents) = sample.split_last().expect("validated, non-empty");
        // FER-562: before any parent folder is created (string work against the cached root).
        let base = match self.base(target) {
            Ok(b) => b,
            Err(c) => {
                for (idx, _) in ready {
                    out.push((idx, OpResult::Failed(c)));
                }
                return out;
            }
        };
        for (idx, _) in &ready {
            let refs: Vec<&str> = comps_of[idx].iter().map(String::as_str).collect();
            if let Err(c) = check_under(&*self.req.env.guard, &base, &refs) {
                for (i, _) in &ready {
                    out.push((*i, OpResult::Failed(c)));
                }
                return out;
            }
        }
        let Some(target_root) = self.handles.root(target) else {
            for (idx, _) in ready {
                out.push((idx, OpResult::Failed(no_root())));
            }
            return out;
        };
        let held = match open_chain(target_root, parents, true) {
            Ok(h) => h,
            Err(e) => {
                let c = e.classify();
                for (idx, _) in ready {
                    out.push((idx, OpResult::Failed(c)));
                }
                return out;
            }
        };

        // FER-562: the final path of the destination folder, looked up once for the batch, and
        // every entry of the batch below it.
        let held_base = match final_base(held.dir()) {
            Ok(b) => b,
            Err(c) => {
                for (idx, _) in &ready {
                    out.push((*idx, OpResult::Failed(c)));
                }
                return out;
            }
        };
        for (idx, _) in &ready {
            let name: Vec<&str> = comps_of[idx]
                .last()
                .map(String::as_str)
                .into_iter()
                .collect();
            if let Err(c) = check_under(&*self.req.env.guard, &held_base, &name) {
                for (i, _) in &ready {
                    out.push((*i, OpResult::Failed(c)));
                }
                return out;
            }
        }

        // Versioning keeps the previous version of an overwritten file: move it aside first.
        if overwrite && versioning {
            let idx = ready[0].0;
            if !self.aside[idx] {
                let comps = &comps_of[&idx];
                let refs: Vec<&str> = comps.iter().map(String::as_str).collect();
                let (last, parents) = refs.split_last().expect("validated, non-empty");
                self.req.env.hooks.after_jit(idx, &self.ops[idx].name);
                let expect = self.expect_of(&self.ops[idx]);
                let moved = self.guarded(|cancel| {
                    self.deleter(cancel).keep_overwritten(
                        target,
                        target_root,
                        self.root_path(target),
                        parents,
                        last,
                        expect,
                    )
                });
                match moved {
                    Ok(Removal::Done) => self.aside[idx] = true,
                    Ok(Removal::Changed) => {
                        out.push((idx, OpResult::Changed));
                        return out;
                    }
                    Err(c) => {
                        out.push((idx, self.after_move_error(c)));
                        return out;
                    }
                }
            }
        }

        let dest_dir = self
            .abs_path(target, parents)
            .to_string_lossy()
            .into_owned();
        let sources: Vec<String> = ready.iter().map(|(_, s)| s.clone()).collect();
        // Overwrite in place under recycleBin / permanent (SP-3.4); under versioning the old file
        // is already aside, so an existing target means it appeared meanwhile and must fail.
        let in_place = overwrite && !versioning;
        let report = self.guarded(|cancel| {
            self.req
                .env
                .copier
                .copy(&sources, &dest_dir, in_place, false, cancel)
        });
        drop(held);

        let whole = report
            .failed
            .iter()
            .find(|(k, _)| k == &dest_dir)
            .map(|(_, c)| *c);
        for (idx, src) in ready {
            let result = if report.done.contains(&src) {
                OpResult::Done
            } else if let Some((_, c)) = report.failed.iter().find(|(k, _)| k == &src) {
                OpResult::Failed(*c)
            } else if let Some(c) = whole {
                OpResult::Failed(c)
            } else if report.cancelled
                || self.req.cancel.is_cancelled()
                || self.req.lease.is_none_or(|l| l.is_lost())
            {
                OpResult::Cancelled
            } else {
                // Skipped by the copy engine: the destination appeared after the plan.
                OpResult::Failed(Classified {
                    class: ErrorClass::PerItem,
                    reason: Reason::TargetAppeared,
                })
            };
            out.push((idx, result));
        }
        out
    }

    // ── outcome ──

    fn outcome(&self) -> EngineOutcome {
        let counts = self.counts;
        match self.stop {
            Some(Stop::Cancelled) => EngineOutcome::Cancelled { counts },
            Some(Stop::Interrupted(why)) => EngineOutcome::Interrupted { counts, why },
            Some(Stop::Systemic(reason)) => {
                let error = systemic_error(reason);
                if counts.done > 0 {
                    EngineOutcome::Partial { counts, error }
                } else {
                    EngineOutcome::Failed { error }
                }
            }
            None => {
                if counts.failed > 0 {
                    let reason = self.first_failure.unwrap_or(Reason::Unknown);
                    let error = EngineError {
                        code: reason.code(),
                        text: format!(
                            "{} operation{} failed: {}.",
                            counts.failed,
                            if counts.failed == 1 { "" } else { "s" },
                            reason.text()
                        ),
                    };
                    if counts.done > 0 {
                        EngineOutcome::Partial { counts, error }
                    } else {
                        EngineOutcome::Failed { error }
                    }
                } else if counts.held > 0 {
                    EngineOutcome::ConflictHeld { counts }
                } else {
                    EngineOutcome::Completed { counts }
                }
            }
        }
    }

    fn outcomes_for_settle(&self) -> HashMap<String, ItemOutcome> {
        outcome_map(&self.records)
    }
}

impl Op {
    /// Size of the file this op reads (for batch sizing).
    fn entry_size_of_source(&self) -> u64 {
        match self.source() {
            Side::Left => self.entry.left_size,
            Side::Right => self.entry.right_size,
        }
    }
}

fn parent_of(name: &str) -> &str {
    name.rsplit_once('/').map_or("", |(p, _)| p)
}

/// The run error for a systemic stop (RQ-7.2).
fn systemic_error(reason: Reason) -> EngineError {
    let text = match reason {
        Reason::AccessDenied => {
            "Not retried automatically — permission errors need you. Fix access, then Recheck."
                .to_string()
        }
        Reason::DiskFull => "The destination is full. Free some space, then Recheck.".to_string(),
        other => format!("Stopped: {}. Fix the cause, then Recheck.", other.text()),
    };
    EngineError {
        code: reason.code(),
        text,
    }
}

#[cfg(test)]
#[path = "execute_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "execute_tests2.rs"]
mod tests2;

#[cfg(test)]
#[path = "execute_tests3.rs"]
mod tests3;

#[cfg(test)]
#[path = "execute_tests4.rs"]
mod tests4;
