//! Sync engine core modules: plan, settle, and filters (P0.1).
//!
//! Pure planning and baseline reconciliation code with TypeScript parity,
//! plus profile filters and cap controls for directory compare.

pub mod filters;
pub mod plan;
pub mod settle;

pub use filters::{FilterError, Filters, ProfileFilter, is_builtin_exclude};
pub use plan::{
    Classification, MTIME_TOLERANCE_MS, PROFILE_CAP_BLOCKER_TEXT, PROFILE_ENTRY_CAP, PlanSummary,
    SideChange, SyncAction, SyncConflictReason, SyncDecision, SyncDirection, SyncOptions,
    SyncPlanItem, build_sync_plan, classify_detailed, describe_blockers, describe_profile_blockers,
    is_baseline_trusted, is_plan_stale, is_resolvable, plan_facts, plan_summary,
    sync_plan_blockers,
};
pub use settle::{ItemOutcome, proves_full_sync, reconcile_baseline};
