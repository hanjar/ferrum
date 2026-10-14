//! Sync engine for stored Sync Profiles (Automation design §5).
//!
//! - [`plan`], [`settle`] and [`filters`] (P0.1): pure planning and baseline reconciliation code
//!   with TypeScript parity, plus profile filters and cap controls for directory compare.
//! - [`roots`] (P0.3): root canonicalization, identity and readiness probes (§5.8).
//! - [`execute`] (P0.2): the run itself (`run`): gate, fresh compare, plan, delete guard,
//!   operations with just-in-time checks, retries, cancel, lease-guarded settle.
//!   [`delete_policy`], [`versioning`], [`nofollow`], [`errors`], [`opset`] and [`env`] are its
//!   parts; `env` holds every injection point.

pub mod delete_policy;
pub mod env;
pub mod errors;
pub mod execute;
pub mod filters;
pub mod nofollow;
pub mod opset;
pub mod plan;
pub mod roots;
pub mod settle;
pub mod versioning;

pub use execute::{
    Counts, EngineError, EngineOutcome, EngineReport, EngineRequest, EngineSink, GuardAck,
    InterruptReason, NoSink, OpRecord, Phase, SettleState, run, sync_options,
};
pub use filters::{FilterError, Filters, ProfileFilter, is_builtin_exclude};
pub use opset::{GuardVerdict, OpStatus};
pub use plan::{
    Classification, MTIME_TOLERANCE_MS, PROFILE_CAP_BLOCKER_TEXT, PROFILE_ENTRY_CAP, PlanMode,
    PlanSummary, SideChange, SyncAction, SyncConflictReason, SyncDecision, SyncDirection,
    SyncOptions, SyncPlanItem, Violation, apply_decisions, assert_profile_plan_invariants,
    build_profile_sync_plan, build_sync_plan, classify_detailed, describe_blockers,
    describe_profile_blockers, is_baseline_trusted, is_plan_stale, is_resolvable, plan_facts,
    plan_summary, profile_plan_violations, sync_plan_blockers, validate_profile_plan,
};
pub use settle::{ItemOutcome, proves_full_sync, reconcile_baseline};

#[cfg(test)]
pub(crate) mod test_support;
