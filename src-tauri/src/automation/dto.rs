//! IPC shapes of the automation commands (Automation design §7.2, P1.1): camelCase, optional
//! fields omitted, contract fixtures in `contract-fixtures/automation/`. Tauri-free.
//!
//! Nothing here carries a lease id (design §3.4: a lease id never leaves the process in any DTO
//! except the `sync_lease_acquire` answer to its requester); a source test pins that.

use serde::{Deserialize, Serialize};

use crate::automation::model::{AutomationPolicy, FieldError};
use crate::automation::policy::NeedsReview;

/// Why an automation needs a review (design §8.5), as the UI renders it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "camelCase")]
pub enum NeedsReviewDto {
    ProfileChanged { from: i64, to: i64 },
    PolicyChanged,
    NotApproved,
    KeyUnavailable,
    UnsupportedVersion,
}

impl From<NeedsReview> for NeedsReviewDto {
    fn from(r: NeedsReview) -> Self {
        match r {
            NeedsReview::ProfileChanged { from, to } => Self::ProfileChanged { from, to },
            NeedsReview::PolicyChanged => Self::PolicyChanged,
            NeedsReview::NotApproved => Self::NotApproved,
            NeedsReview::KeyUnavailable => Self::KeyUnavailable,
            NeedsReview::UnsupportedVersion => Self::UnsupportedVersion,
        }
    }
}

/// One automation as P1.1 commands return it. The richer list/detail rows (status, last run,
/// pipeline) arrive with P1.2.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationDto {
    pub id: String,
    pub name: String,
    /// `schedule` or `watch`.
    pub kind: String,
    pub profile_id: String,
    pub profile_name: String,
    /// The profile's CURRENT revision.
    pub profile_revision: i64,
    pub policy: AutomationPolicy,
    pub enabled: bool,
    pub config_rev: i64,
    pub created_ms: i64,
    pub updated_ms: i64,
    /// Present when the stored approval no longer verifies (AP-6).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<NeedsReviewDto>,
}

/// `ok`, `warn` or `fail` (AP-4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StepStatus {
    Ok,
    Warn,
    Fail,
}

/// One line of a test run or of the editor's readiness block (AP-2.2, AP-4.2). `text` is built by
/// the backend and may name the profile's folders.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StepDto {
    pub id: String,
    pub status: StepStatus,
    pub text: String,
}

/// One upcoming run of a schedule (SC-1.2). The scheduler (P1.4) fills it; until then the list
/// is empty.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NextRunDto {
    pub at_ms: i64,
    pub label: String,
}

/// `automation_validate` answer (design §7.2): field errors, readiness checks, next runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationValidateDto {
    pub errors: Vec<FieldError>,
    pub checks: Vec<StepDto>,
    pub next: Vec<NextRunDto>,
}

/// `automation_test` answer: read-only, files untouched (AP-4.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestResultDto {
    pub test_id: String,
    /// The id the automation will have if this policy is approved (new automations get theirs
    /// here, because the policy hash covers it); `automation_approve` sends no `id` for them.
    pub automation_id: String,
    pub policy_hash: String,
    pub steps: Vec<StepDto>,
    pub passed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum ApproveResultDto {
    Approved { automation: Box<AutomationDto> },
    Canceled,
    Stale,
}

/// `automation_set_enabled` answer. `needsChoice`: a run is in progress, the UI shows the pause
/// dialog and calls `automation_pause`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum SetEnabledResultDto {
    Enabled,
    Paused,
    Canceled,
    NeedsChoice,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum PauseResultDto {
    Paused,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum DeleteAutomationDto {
    Deleted,
}

/// `finish` lets a running run end first; `stop` stops it (the run part arrives with P1.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PauseMode {
    Finish,
    Stop,
}
