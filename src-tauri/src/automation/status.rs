//! Automation status (requirements §1, design §9.1): ONE pure function, [`compute`], decides the
//! chip an automation shows. No I/O, no clock, no store - the read model gathers the facts into a
//! [`StatusInput`] and this file only orders them.
//!
//! Precedence (first match wins):
//! `Needs review → Runner not running (background only) → Running → Queued / Waiting for device →
//! Needs attention → Paused → trigger state`.
//!
//! Every chip carries a kind (icon + colour) AND a label (the StatusChip never relies on colour
//! alone). The `enabled` flag only decides `Paused` - it is NOT an authorization: a disabled
//! automation that needs review shows `Needs review`, and nothing here says an automation may run.

use serde::{Deserialize, Serialize};

use crate::automation::policy::NeedsReview;
use crate::automation::store::runs::{Outcome, Phase, RunCounts};
use crate::automation::store::runtime::{Attention, TriggerState};

/// Chip kinds (requirements §1). The webview maps each to an icon and a colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StatusKind {
    Watching,
    Quiet,
    Checking,
    Device,
    Recheck,
    Unsupported,
    Scheduled,
    Queued,
    Pending,
    Running,
    Paused,
    Completed,
    Partial,
    Cancelled,
    Skipped,
    Attention,
    Review,
    Warn,
}

/// A chip: kind, label and an optional sub-line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusDto {
    pub kind: StatusKind,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub: Option<String>,
}

impl StatusDto {
    fn new(kind: StatusKind, label: impl Into<String>, sub: Option<String>) -> Self {
        Self {
            kind,
            label: label.into(),
            sub,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoKind {
    Schedule,
    Watch,
}

/// The automation's run in progress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveRun {
    pub phase: Phase,
    /// Operations dealt with so far / planned (file operations, never filesystem events).
    pub done: i64,
    pub total: i64,
    /// How the run was triggered ("Scheduled · 02:00 WIB").
    pub trigger_text: String,
    /// A stop request is pending for this run.
    pub stopping: bool,
}

/// Why a queued entry is not running yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueueWait {
    /// A root is offline: nothing runs, nothing is treated as empty.
    Device,
    /// Another run uses overlapping folders; carries that run's name.
    WaitsFor(String),
    /// OP-1: on battery.
    Battery,
    /// OP-1: on a metered network.
    Metered,
    /// Nothing blocks it, but automatic runs are switched off (`host::UNATTENDED_RUNS_ENABLED`,
    /// on since FER-525; kept for a build or a test that turns it off):
    /// only the user's Run now starts it.
    RunNowOnly,
    /// Nothing blocks it; the host starts it next.
    Starting,
}

/// Everything [`compute`] looks at.
#[derive(Debug, Clone)]
pub struct StatusInput {
    /// The verdict of `policy::verify`; `None` = valid.
    pub review: Option<NeedsReview>,
    pub kind: AutoKind,
    /// The automation runs in the background runner.
    pub background: bool,
    pub runner_live: bool,
    pub enabled: bool,
    pub active: Option<ActiveRun>,
    pub queued: Option<QueueWait>,
    pub attention: Option<Attention>,
    pub trigger: TriggerState,
    pub pending_rerun: bool,
    /// Another run is active on overlapping folders (a pending rerun is only shown then).
    pub overlap_run_active: bool,
    /// Seconds left of the quiet countdown.
    pub quiet_secs: Option<u64>,
    /// The watch's idle time in seconds.
    pub idle_secs: u32,
    /// Why the watch is Checking ("Monitoring resumed \u{2014} fresh comparison ..."), as the host
    /// wrote it; `None` reads as the generic text.
    pub check_reason: Option<String>,
    /// The next schedule occurrence, formatted ("Tue 29 Sep, 18:00 WIB"); `None` = no upcoming run.
    pub next_label: Option<String>,
}

/// The sub-line of a Needs review chip ("Profile changed (rev 3 → 4) · held").
pub fn review_label(review: &NeedsReview) -> String {
    match review {
        NeedsReview::ProfileChanged { from, to } => {
            format!("Profile changed (rev {from} \u{2192} {to}) \u{b7} held")
        }
        NeedsReview::PolicyChanged => "Settings changed since approval \u{b7} held".to_string(),
        NeedsReview::NotApproved => "Not approved on this device \u{b7} held".to_string(),
        NeedsReview::KeyUnavailable => "Approval key unavailable \u{b7} held".to_string(),
        NeedsReview::UnsupportedVersion => {
            "Approved by a different Ferrum version \u{b7} held".to_string()
        }
    }
}

/// The label of a run in progress: "Comparing" until operations start, then "Running d/n".
pub fn running_label(phase: Phase, done: i64, total: i64) -> String {
    match phase {
        Phase::Gate | Phase::Compare | Phase::Plan => "Comparing".to_string(),
        Phase::Sync | Phase::Settle | Phase::Done if total > 0 => format!("Running {done}/{total}"),
        Phase::Sync | Phase::Settle | Phase::Done => "Running".to_string(),
    }
}

/// Design §9.1 as one function.
pub fn compute(i: &StatusInput) -> StatusDto {
    // 1. Needs review: the stored approval no longer verifies.
    if let Some(review) = &i.review {
        return StatusDto::new(
            StatusKind::Review,
            "Needs review",
            Some(review_label(review)),
        );
    }
    // 2. A background automation does not run while the runner is not.
    if i.background && !i.runner_live {
        return StatusDto::new(
            StatusKind::Warn,
            "Runner not running",
            Some("Background automations don't run until the runner starts".to_string()),
        );
    }
    // 3. Running.
    if let Some(run) = &i.active {
        let sub = if run.stopping {
            "Stopping after the current file".to_string()
        } else if i.pending_rerun {
            "1 pending rerun after this".to_string()
        } else {
            run.trigger_text.clone()
        };
        return StatusDto::new(
            StatusKind::Running,
            running_label(run.phase, run.done, run.total),
            Some(sub),
        );
    }
    // 4. Queued / waiting for the device.
    if let Some(wait) = &i.queued {
        return match wait {
            QueueWait::Device => StatusDto::new(StatusKind::Device, "Waiting for device", None),
            QueueWait::WaitsFor(name) => StatusDto::new(
                StatusKind::Queued,
                "Queued",
                Some(format!("Waits for \"{name}\" \u{2014} same folders")),
            ),
            QueueWait::Battery => StatusDto::new(
                StatusKind::Queued,
                "Queued",
                Some("Waits for AC power".to_string()),
            ),
            QueueWait::Metered => StatusDto::new(
                StatusKind::Queued,
                "Queued",
                Some("Waits for an unmetered network".to_string()),
            ),
            QueueWait::RunNowOnly => StatusDto::new(
                StatusKind::Queued,
                "Queued",
                Some("Waiting \u{2014} run it with Run now".to_string()),
            ),
            QueueWait::Starting => StatusDto::new(
                StatusKind::Queued,
                "Queued",
                Some("Starting\u{2026}".to_string()),
            ),
        };
    }
    // 5. Needs attention: new runs are held until the run is resolved (RQ-9.1).
    if let Some(a) = &i.attention {
        return StatusDto::new(
            StatusKind::Attention,
            "Needs attention",
            Some(format!("{} \u{b7} new runs held", a.text)),
        );
    }
    // 6. Paused.
    if !i.enabled {
        let sub = match i.kind {
            AutoKind::Watch => "Not monitoring",
            AutoKind::Schedule => "No new runs",
        };
        return StatusDto::new(StatusKind::Paused, "Paused", Some(sub.to_string()));
    }
    // 7. Trigger state.
    match i.kind {
        AutoKind::Schedule => {
            if i.trigger == TriggerState::Device {
                return StatusDto::new(StatusKind::Device, "Waiting for device", None);
            }
            let sub = match &i.next_label {
                Some(label) => format!("Next {label}"),
                None => "No upcoming run".to_string(),
            };
            StatusDto::new(StatusKind::Scheduled, "Scheduled", Some(sub))
        }
        AutoKind::Watch => {
            if i.pending_rerun && i.overlap_run_active {
                return StatusDto::new(
                    StatusKind::Pending,
                    "Pending rerun",
                    Some("Another run is using these folders".to_string()),
                );
            }
            match i.trigger {
                TriggerState::Quiet => StatusDto::new(
                    StatusKind::Quiet,
                    match i.quiet_secs {
                        Some(s) => format!("Waiting for quiet \u{b7} {s} s"),
                        None => "Waiting for quiet".to_string(),
                    },
                    Some(format!("Runs after {} s without changes", i.idle_secs)),
                ),
                TriggerState::Checking => StatusDto::new(
                    StatusKind::Checking,
                    "Checking",
                    Some(
                        i.check_reason
                            .clone()
                            .unwrap_or_else(|| "Fresh comparison before any run".to_string()),
                    ),
                ),
                TriggerState::Device => StatusDto::new(
                    StatusKind::Device,
                    "Waiting for device",
                    Some("A watched folder is not connected \u{2014} no sync".to_string()),
                ),
                TriggerState::Recheck => StatusDto::new(
                    StatusKind::Recheck,
                    "Needs recheck",
                    Some("Watcher missed events \u{2014} full comparison when safe".to_string()),
                ),
                TriggerState::Unsupported => StatusDto::new(
                    StatusKind::Unsupported,
                    "Unsupported",
                    Some(
                        "No change notifications on this provider \u{2014} can't be watched"
                            .to_string(),
                    ),
                ),
                // `scheduled` is not a watch state; it reads as the idle default.
                TriggerState::Watching | TriggerState::Scheduled => {
                    StatusDto::new(StatusKind::Watching, "Watching", None)
                }
            }
        }
    }
}

// ── Run chips (requirements §1 "Run outcomes") ──

/// "4 succeeded · 1 failed · 1 not started" - the counts of a run, zero parts left out.
pub fn counts_summary(counts: &RunCounts) -> String {
    let mut parts = Vec::new();
    if counts.ok > 0 {
        parts.push(format!("{} succeeded", counts.ok));
    }
    if counts.failed > 0 {
        parts.push(format!("{} failed", counts.failed));
    }
    if counts.not_started > 0 {
        parts.push(format!("{} not started", counts.not_started));
    }
    if counts.held > 0 {
        parts.push(format!("{} held", counts.held));
    }
    if counts.skipped_changed > 0 {
        parts.push(format!("{} changed during the run", counts.skipped_changed));
    }
    parts.join(" \u{b7} ")
}

/// The chip of a finished run: Completed → `completed`; Partial → `partial`; Conflict held →
/// `attention` "Conflict held"; Failed → `attention` "Failed"; Cancelled → `cancelled`;
/// Interrupted → `cancelled` "Interrupted"; Skipped → `skipped`.
pub fn outcome_chip(
    outcome: Outcome,
    counts: &RunCounts,
    ops_total: i64,
    error_text: Option<&str>,
) -> StatusDto {
    let some = |s: String| if s.is_empty() { None } else { Some(s) };
    match outcome {
        Outcome::Completed => {
            let sub = if ops_total == 0 {
                "Already in sync \u{b7} 0 operations".to_string()
            } else {
                format!("{} of {}", counts.ok, ops_total)
            };
            StatusDto::new(StatusKind::Completed, "Completed", Some(sub))
        }
        Outcome::Partial => {
            StatusDto::new(StatusKind::Partial, "Partial", some(counts_summary(counts)))
        }
        Outcome::Conflict => StatusDto::new(
            StatusKind::Attention,
            "Conflict held",
            Some(match counts.held {
                1 => "1 conflict held for review".to_string(),
                n if n > 1 => format!("{n} conflicts held for review"),
                _ => "Conflict held for review".to_string(),
            }),
        ),
        Outcome::Failed => StatusDto::new(
            StatusKind::Attention,
            "Failed",
            error_text
                .map(str::trim)
                .filter(|e| !e.is_empty())
                .map(str::to_string),
        ),
        Outcome::Cancelled => StatusDto::new(
            StatusKind::Cancelled,
            "Cancelled",
            some(counts_summary(counts)),
        ),
        Outcome::Interrupted => StatusDto::new(
            StatusKind::Cancelled,
            "Interrupted",
            some(counts_summary(counts)),
        ),
        Outcome::Skipped => StatusDto::new(StatusKind::Skipped, "Skipped", None),
    }
}

/// The chip of a run that has not ended.
pub fn running_chip(phase: Phase, counts: &RunCounts, ops_total: i64) -> StatusDto {
    StatusDto::new(
        StatusKind::Running,
        running_label(phase, counts.processed(), ops_total),
        None,
    )
}

// ── Time ──

const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// "Tue 29 Sep, 18:00 UTC" for an epoch-millisecond instant. The zone-aware label ("WIB") belongs
/// to the scheduler (P1.4, which owns the time-zone data); until it supplies one this is the
/// unambiguous form.
pub fn format_utc_short(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Days since 1970-01-01 → civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let _year = yoe + era * 400 + i64::from(month <= 2);
    // 1970-01-01 was a Thursday.
    let weekday = (days + 4).rem_euclid(7) as usize;
    format!(
        "{} {} {}, {:02}:{:02} UTC",
        WEEKDAYS[weekday],
        day,
        MONTHS[(month - 1) as usize],
        rem / 3_600,
        (rem % 3_600) / 60
    )
}

#[cfg(test)]
mod tests;
