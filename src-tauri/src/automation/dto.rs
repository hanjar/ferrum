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

// ── Controls (P1.3, design §7.2): Run now, Stop, Recheck, Retry ──

/// What a request to run turned into (the toasts of RQ-4). `requested` = a control was written
/// for the host that owns the automation (the background runner); it answers through the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum QueueStatus {
    Queued,
    Waiting,
    Merged,
    Device,
    Requested,
}

/// The answer of `automation_run_now` and `automation_retry`. `waits_for` names the automation
/// whose run uses the same folders, for `waiting`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueResultDto {
    pub status: QueueStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waits_for: Option<String>,
}

/// The answer of `automation_stop_run` and `automation_recheck`: the request was recorded and the
/// host that owns the run will act on it. The outcome shows up in the run and in
/// `automation://changed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum RequestedDto {
    Requested,
}

/// Payload of the event `automation://changed`: `meta.change_seq` moved - re-read what is on
/// screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangedEventDto {
    pub seq: i64,
}

// ── Read side (P1.2, design §7.2): list, detail, history, operations, events ──

pub use crate::automation::status::{StatusDto, StatusKind};
use crate::automation::store::runs::{RecheckRecord, RunCounts};
use crate::automation::store::runtime::Attention;

/// Counts of file operations (never filesystem events) of one run.
pub type CountsDto = RunCounts;
/// The hold that keeps new runs of an automation back (RQ-9.1).
pub type AttentionDto = Attention;
/// What a recheck found (RQ-10).
pub type RecheckDto = RecheckRecord;

/// The result chip of an automation's last finished run (MH-2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LastRunDto {
    pub run_id: i64,
    pub started_ms: i64,
    pub ended_ms: i64,
    pub result: StatusDto,
}

/// One row of `automation_list` (MH-2): status, last result and the flags the row's buttons need.
/// `canRunNow` is a UI hint only - the gate checks the approval again on every run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationRowDto {
    pub id: String,
    pub name: String,
    /// `schedule` or `watch`.
    pub kind: String,
    pub profile_id: String,
    pub profile_name: String,
    /// The profile's CURRENT revision.
    pub profile_revision: i64,
    /// The revision the stored approval covers, when there is an approval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_revision: Option<i64>,
    pub enabled: bool,
    pub config_rev: i64,
    pub status: StatusDto,
    /// "Daily at 02:00 (Asia/Jakarta)", "Watches the left folder, runs after 10 s without changes".
    pub trigger: String,
    /// `app` or `background`.
    pub runtime: String,
    /// "Only while open" or "Background runner".
    pub runtime_note: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run: Option<LastRunDto>,
    pub can_run_now: bool,
    /// Why Run now is dimmed (needs review, needs attention, paused).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_now_reason: Option<String>,
    /// The row offers "Review changes" instead of Resolve.
    pub show_review: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<NeedsReviewDto>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention: Option<AttentionDto>,
}

/// One run (history row, current run, recent run).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunRowDto {
    pub id: i64,
    /// Absent for a manual profile run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub automation_id: Option<String>,
    /// The name when the run started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub automation_name: Option<String>,
    pub profile_id: String,
    pub profile_revision: i64,
    /// `app`, `background` or `manual`.
    pub host: String,
    pub trigger_kind: String,
    pub trigger_text: String,
    pub merged_count: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_of: Option<i64>,
    pub started_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_ms: Option<i64>,
    /// `gate`, `compare`, `plan`, `sync`, `settle` or `done`.
    pub phase: String,
    /// `completed`, `partial`, `conflict`, `failed`, `cancelled`, `interrupted`, `skipped`;
    /// absent while the run is going.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    pub ops_total: i64,
    pub counts: CountsDto,
    pub result: StatusDto,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_by: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recheck: Option<RecheckDto>,
    /// The automation's hold names this run right now.
    pub needs_attention: bool,
}

/// `automation_history` answer: newest first; `next` is the cursor for the following page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryDto {
    pub runs: Vec<RunRowDto>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<i64>,
}

/// One operation of a run (RQ-5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunOpDto {
    pub seq: i64,
    pub kind: String,
    /// The target side: `left` or `right`.
    pub side: String,
    pub rel_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<i64>,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_text: Option<String>,
}

/// `automation_run_ops` answer. `total` is the run's true number of operations; `stored` how many
/// of them have rows (the first 5 000, MH-8) - paging runs over `stored`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunOpsDto {
    pub ops: Vec<RunOpDto>,
    pub total: i64,
    pub stored: i64,
}

/// One line of the event log (MH-7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventDto {
    pub id: i64,
    pub at_ms: i64,
    /// `ok`, `warn`, `bad`, `mute` or `info`.
    pub kind: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<i64>,
}

/// `automation_events` answer: newest first; `next` is the cursor for the following page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventsPageDto {
    pub events: Vec<EventDto>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<i64>,
}

/// A queued entry (MH-4): what queued it, how many triggers merged into it and what it waits for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueEntryDto {
    pub id: i64,
    pub automation_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub automation_name: Option<String>,
    pub trigger_kind: String,
    pub trigger_text: String,
    pub merged_count: i64,
    pub merged_reasons: Vec<String>,
    pub enqueued_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_of: Option<i64>,
    /// `app` or `background`.
    pub host_class: String,
    /// "Nightly photos to NAS" when another run uses the same folders.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waits_for: Option<String>,
}

/// The lifecycle pipeline of the detail page (MH-5): step labels and the current step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PipelineDto {
    pub steps: Vec<String>,
    /// Absent while the automation is held (review, attention, paused, runner not running).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<usize>,
}

/// The quiet countdown of a watch (MH-5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuietDto {
    pub until_ms: i64,
    pub remaining_secs: i64,
    pub idle_secs: u32,
}

/// The profile an automation runs, as the detail page summarizes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileBriefDto {
    pub id: String,
    pub name: String,
    pub revision: i64,
    pub left: String,
    pub right: String,
}

/// `automation_get` answer (MH-5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationDetailDto {
    pub automation: AutomationRowDto,
    pub policy: AutomationPolicy,
    pub profile: ProfileBriefDto,
    pub pipeline: PipelineDto,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quiet: Option<QuietDto>,
    pub pending_rerun: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_run: Option<RunRowDto>,
    pub waiting: Vec<QueueEntryDto>,
    pub recent_runs: Vec<RunRowDto>,
    pub events: EventsPageDto,
}
