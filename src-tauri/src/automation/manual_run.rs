//! Manual profile preview and run (Automation design §7.1: `sync_profile_preview`,
//! `sync_profile_run`). Tauri-free: the command wrappers in `commands.rs` supply the window, the
//! event emitter and the native dialog; everything that decides lives here so tests drive it
//! with temp folders and an injected confirmation.
//!
//! - A preview is a read-only engine `dry_run`. It is remembered (id, profile, revision, plan
//!   fingerprint, delete and overwrite counts) for a short time.
//! - A run refuses a missing, expired or foreign preview (`stale`), overwrites that were not
//!   confirmed, and a plan that is not the one previewed (the engine compares fingerprints).
//! - A permanent-delete run asks the user natively, per run (SE-1, design §10.3).
//! - The run holds its own lease over both roots (and the versioning root) from before the first
//!   operation until after the baseline settle.

use std::sync::Mutex;
use std::sync::atomic::{AtomicI64, Ordering};

use serde::Serialize;

use crate::automation::commands::{CommandError, ConfirmAnswer};
use crate::automation::confirm_text::{self, Prompt};
use crate::automation::lease::{GuardOutcome, LeaseEnv, LeaseGuard, MAX_MANUAL_HOLD_MS};
use crate::automation::model::{self, DeletePolicy, ProfilePolicy};
use crate::automation::store::leases::HolderKind;
use crate::automation::store::{Store, StoreError, profiles};
use crate::sync_baseline::SyncBaselineStore;
use crate::sync_engine::delete_policy::check_gate;
use crate::sync_engine::env::EngineEnv;
use crate::sync_engine::execute::{
    EngineOutcome, EngineReport, EngineRequest, EngineSink, SettleState, run,
};
use crate::sync_engine::opset::GuardVerdict;
use crate::sync_engine::plan::PlanSummary;
use std::sync::Arc;

/// A preview stays valid for this long.
pub const PREVIEW_TTL_MS: i64 = 15 * 60 * 1000;
/// Previews kept at once (the oldest is dropped).
pub const MAX_PREVIEWS: usize = 16;
/// Retries of a transient error in a manual run (RQ-7.1 default).
pub const MANUAL_RETRIES: u8 = 2;

// ── Wire shapes ──

/// `sync_profile_preview` answer. `blockers` (closed codes: `truncated`, `cancelled`, `errors`)
/// appears only when the comparison could not be trusted; a run is then refused by the engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewDto {
    pub preview_id: String,
    pub summary: PlanSummary,
    pub conflicts: u32,
    pub guard: GuardVerdict,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blockers: Vec<String>,
}

/// `sync_profile_run` answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunStartedDto {
    pub run_id: i64,
}

// ── Preview board ──

#[derive(Debug, Clone)]
struct PreviewEntry {
    id: String,
    profile_id: String,
    revision: i64,
    fingerprint: String,
    deletes: u32,
    overwrites: u32,
    created_ms: i64,
}

/// The previews of this process. Bounded and short-lived; an id is a random UUID. Cheap to
/// clone (the commands move a handle onto their worker thread).
#[derive(Clone, Default)]
pub struct PreviewBoard {
    entries: Arc<Mutex<Vec<PreviewEntry>>>,
}

impl PreviewBoard {
    fn insert(&self, entry: PreviewEntry) {
        let mut all = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        all.retain(|e| e.profile_id != entry.profile_id || e.revision != entry.revision);
        all.push(entry);
        while all.len() > MAX_PREVIEWS {
            all.remove(0);
        }
    }

    fn get(&self, id: &str, now_ms: i64) -> Option<PreviewEntry> {
        let mut all = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        all.retain(|e| now_ms.saturating_sub(e.created_ms) <= PREVIEW_TTL_MS);
        all.iter().find(|e| e.id == id).cloned()
    }

    fn remove(&self, id: &str) {
        self.entries
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .retain(|e| e.id != id);
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.lock().unwrap().len()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

// ── Run ids ──

static LAST_RUN_ID: AtomicI64 = AtomicI64::new(0);

/// Process-unique run id (it names the `#<run>` folder of the versioning tree). Based on the
/// clock and strictly increasing; history assigns real ids from P1.2 on.
pub fn next_run_id(now_ms: i64) -> i64 {
    let mut last = LAST_RUN_ID.load(Ordering::SeqCst);
    loop {
        let next = now_ms.max(last + 1);
        match LAST_RUN_ID.compare_exchange(last, next, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => return next,
            Err(seen) => last = seen,
        }
    }
}

// ── Helpers ──

fn require_id(id: &str) -> Result<(), CommandError> {
    if model::is_profile_id(id) {
        Ok(())
    } else {
        Err(CommandError::new(
            "invalid_input",
            "The id is not a valid identifier",
        ))
    }
}

fn not_ready(code: &str) -> CommandError {
    CommandError::new(
        "not_ready",
        format!("The profile is not ready to run ({code}). Check its folders and delete policy"),
    )
}

fn load(store: &Store, id: &str) -> Result<(profiles::ProfileRecord, ProfilePolicy), CommandError> {
    let rec = store.with_read(|conn| -> Result<_, StoreError> {
        profiles::get(conn, id)?.ok_or(StoreError::NotFound)
    })?;
    let policy: ProfilePolicy = serde_json::from_str(&rec.policy).map_err(|_| {
        CommandError::new(
            "needs_review",
            "The stored profile is invalid and needs review",
        )
    })?;
    Ok((rec, policy))
}

// ── Preview ──

/// `sync_profile_preview` core: a read-only engine dry run. Nothing is touched and no lease is
/// needed.
#[allow(clippy::too_many_arguments)]
pub fn preview_in(
    store: &Store,
    baselines: &SyncBaselineStore,
    board: &PreviewBoard,
    env: &EngineEnv,
    sink: &dyn EngineSink,
    id: &str,
    incremental: bool,
    now_ms: i64,
) -> Result<PreviewDto, CommandError> {
    require_id(id)?;
    let (rec, policy) = load(store, id)?;
    let mut req = EngineRequest::new(&policy, baselines, sink, env);
    req.dry_run = true;
    req.incremental = incremental;
    let report = run(&req);
    match report.outcome {
        EngineOutcome::DryRun {
            plan,
            conflicts,
            guard,
        } => {
            let preview_id = model::new_profile_id().ok_or_else(|| {
                CommandError::new("store_unavailable", "Could not create a preview id")
            })?;
            board.insert(PreviewEntry {
                id: preview_id.clone(),
                profile_id: rec.id.clone(),
                revision: rec.revision,
                fingerprint: report.fingerprint.clone().unwrap_or_default(),
                deletes: plan.deletes as u32,
                overwrites: plan.overwrites as u32,
                created_ms: now_ms,
            });
            Ok(PreviewDto {
                preview_id,
                summary: plan,
                conflicts,
                guard,
                blockers: Vec::new(),
            })
        }
        EngineOutcome::Failed { error } => match error.code {
            // The comparison could not be trusted (cap, errors): reported, never an error; the
            // preview id is not valid for a run.
            "blocked" | "cap_exceeded" => Ok(PreviewDto {
                preview_id: String::new(),
                summary: PlanSummary::default(),
                conflicts: 0,
                guard: GuardVerdict::Ok,
                blockers: report.blockers.iter().map(|b| (*b).to_string()).collect(),
            }),
            "plan_invalid" | "profile_invalid" => Err(CommandError::new(
                "needs_review",
                "The profile or its plan is not valid and needs review",
            )),
            code => Err(not_ready(code)),
        },
        _ => Err(CommandError::new(
            "store_unavailable",
            "The preview did not complete",
        )),
    }
}

// ── Run ──

/// Everything a started run needs. Created by [`start_run_in`] once every check passed and the
/// lease is held; [`PreparedRun::execute`] does the blocking work (the wrapper calls it on its own
/// thread).
pub struct PreparedRun {
    pub run_id: i64,
    profile: ProfilePolicy,
    guard: LeaseGuard,
    fingerprint: String,
    incremental: bool,
    cancel: crate::ops::CancelToken,
}

impl PreparedRun {
    /// Run to the end and release the lease (dropping the guard).
    pub fn execute(
        self,
        baselines: &SyncBaselineStore,
        env: &EngineEnv,
        sink: &dyn EngineSink,
    ) -> EngineReport {
        let mut req = EngineRequest::new(&self.profile, baselines, sink, env);
        req.run_id = self.run_id;
        req.retries = MANUAL_RETRIES;
        req.incremental = self.incremental;
        req.lease = Some(&self.guard);
        req.expect_plan = Some(&self.fingerprint);
        req.cancel = self.cancel.clone();
        run(&req)
    }

    /// The token that stops this run (kept by the wrapper for a future stop control).
    pub fn cancel_token(&self) -> crate::ops::CancelToken {
        self.cancel.clone()
    }
}

/// `sync_profile_run` core. Checks, in this order, everything that does not need the user:
/// ids, writable store, the profile, the preview (same profile, same revision, fresh), overwrite
/// confirmation, readiness, the lease. Only then the native confirmation for permanent deletes is
/// shown, so nobody is asked about a run that cannot start.
#[allow(clippy::too_many_arguments)]
pub fn start_run_in<A: Into<ConfirmAnswer>>(
    store: &Arc<Store>,
    board: &PreviewBoard,
    lease_env: &LeaseEnv,
    env: &EngineEnv,
    id: &str,
    preview_id: &str,
    confirm_overwrite: bool,
    incremental: bool,
    confirm: impl FnOnce(&Prompt) -> A,
    now_ms: i64,
) -> Result<PreparedRun, CommandError> {
    require_id(id)?;
    require_id(preview_id)?;
    // LC-3: a read-only or unusable store runs nothing (no lease can be taken either).
    store.check_writable()?;
    let (rec, policy) = load(store, id)?;
    let preview = board
        .get(preview_id, now_ms)
        .filter(|p| p.profile_id == rec.id && p.revision == rec.revision)
        .ok_or_else(|| {
            CommandError::new(
                "stale",
                "The preview is out of date. Preview the profile again",
            )
        })?;
    if preview.overwrites > 0 && !confirm_overwrite {
        return Err(CommandError::new(
            "invalid_input",
            "Overwrites must be confirmed before the run",
        ));
    }
    // Readiness now: roots are the recorded folders, Recycle Bin and versioning prerequisites.
    let roots = check_gate(&policy, &*env.probe).map_err(|e| not_ready(e.code()))?;
    let run_id = next_run_id(now_ms);
    let guard = match LeaseGuard::acquire(
        store,
        lease_env,
        HolderKind::Manual,
        Some(run_id),
        &roots.lease_roots(),
        Some(MAX_MANUAL_HOLD_MS),
    )? {
        GuardOutcome::Acquired(g) => g,
        GuardOutcome::Busy(_) => {
            return Err(CommandError::new(
                "lease_busy",
                "These folders are in use by another sync - wait or stop it",
            ));
        }
    };
    // SE-1 / design §10.3: a permanent-delete run is confirmed natively, per run, with the
    // number of deletes the engine itself previewed.
    if matches!(policy.delete_policy, DeletePolicy::Permanent) && preview.deletes > 0 {
        let prompt = confirm_text::run_permanent_prompt(&rec.name, &policy, preview.deletes);
        match confirm(&prompt).into() {
            ConfirmAnswer::Yes => {}
            ConfirmAnswer::No => {
                return Err(CommandError::new(
                    "canceled",
                    "The run was canceled; nothing was changed",
                ));
            }
            ConfirmAnswer::Busy => {
                return Err(CommandError::new(
                    "busy",
                    "Another confirmation dialog is open; nothing was changed",
                ));
            }
        }
    }
    board.remove(preview_id);
    Ok(PreparedRun {
        run_id,
        profile: policy,
        guard,
        fingerprint: preview.fingerprint,
        incremental,
        cancel: crate::ops::CancelToken::default(),
    })
}

// ── Events (the run reports through these, design §7: no polling command in P0.2) ──

/// Name of the event a manual run reports through.
pub const RUN_EVENT: &str = "sync-profile://run";

/// One event of a manual run: a phase change, throttled counts (at most 10 per second), or the
/// final result. Carries ids, codes and counts - never a path (the documented run error text in
/// the final result is the one exception).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunEventDto {
    pub run_id: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<crate::sync_engine::Phase>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub counts: Option<crate::sync_engine::Counts>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<RunResultDto>,
}

// ── Final answer shape (event payload) ──

/// How a finished run is reported to the webview. `error_text` is the documented run error text
/// (the one text that may name the profile's own folder); everything else is codes and counts.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunResultDto {
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub counts: Option<crate::sync_engine::Counts>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interrupted: Option<crate::sync_engine::InterruptReason>,
    pub pending_rerun: bool,
    pub settle: SettleState,
    /// Operations that did not end `done` (failed, changed, held, not started), at most 200.
    pub problems: Vec<crate::sync_engine::OpRecord>,
}

/// Most problem rows one result carries (RQ-5: longer lists are paged by the UI later).
pub const MAX_PROBLEM_ROWS: usize = 200;

pub fn result_dto(report: &EngineReport) -> RunResultDto {
    use crate::sync_engine::OpStatus;
    let (kind, counts, error, interrupted) = match &report.outcome {
        EngineOutcome::Completed { counts } => ("completed", Some(*counts), None, None),
        EngineOutcome::Partial { counts, error } => ("partial", Some(*counts), Some(error), None),
        EngineOutcome::ConflictHeld { counts } => ("conflictHeld", Some(*counts), None, None),
        EngineOutcome::Failed { error } => ("failed", None, Some(error), None),
        EngineOutcome::Cancelled { counts } => ("cancelled", Some(*counts), None, None),
        EngineOutcome::Interrupted { counts, why } => {
            ("interrupted", Some(*counts), None, Some(*why))
        }
        EngineOutcome::NothingToDo => ("nothingToDo", None, None, None),
        EngineOutcome::DryRun { .. } => ("dryRun", None, None, None),
    };
    RunResultDto {
        kind,
        counts,
        error_code: error.map(|e| e.code),
        error_text: error.map(|e| e.text.clone()),
        interrupted,
        pending_rerun: report.pending_rerun,
        settle: report.settle,
        problems: report
            .ops
            .iter()
            .filter(|o| !matches!(o.status, OpStatus::Done | OpStatus::Skipped))
            .take(MAX_PROBLEM_ROWS)
            .cloned()
            .collect(),
    }
}

#[cfg(test)]
#[path = "manual_run_tests.rs"]
mod tests;
