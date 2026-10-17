//! The test run (Automation design §7.2 `automation_test`, AP-4): a read-only walk through the
//! checks an automatic run would make, producing one line per step. Nothing is written to the
//! synced folders; the compare is the engine's own dry run. Steps appear in order and the first
//! `fail` ends the test (AP-4.2).
//!
//! Every external fact (folder probes, access checks, the dry run, runtime readiness) is injected
//! so tests drive each outcome without a disk. Tauri-free.

use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use crate::automation::commands::{ReadinessDto, readiness_dto};
use crate::automation::dto::{StepDto, StepStatus};
use crate::automation::model::{
    AutomationPolicy, ProfilePolicy, RootIdentity, Runtime, Side, SyncMode, Trigger, shorten_middle,
};
use crate::sync_engine::opset::GuardVerdict;
use crate::sync_engine::plan::PlanSummary;
use crate::sync_engine::roots::RootProbe;

/// A passed test stays usable for approval this long (SE-3.2).
pub const TEST_TTL_MS: i64 = 15 * 60 * 1000;
/// Tests remembered at once (the oldest is dropped).
pub const MAX_TESTS: usize = 64;

// ── The test ledger (in memory, per process) ──

/// What a finished test remembers: the hash it covered, whether it passed and when (SE-3.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestRecord {
    pub test_id: String,
    pub automation_id: String,
    pub policy_hash: String,
    pub passed: bool,
    pub at_ms: i64,
}

/// `TestLedger` of design §7.2: lives only in this process's memory, so an approval needs a test
/// run by THIS app instance within the last 15 minutes. Cheap to share through an `Arc`.
#[derive(Default)]
pub struct TestLedger {
    entries: Mutex<Vec<TestRecord>>,
}

impl TestLedger {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&self, rec: TestRecord) {
        let mut all = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        all.retain(|e| e.test_id != rec.test_id);
        all.push(rec);
        while all.len() > MAX_TESTS {
            all.remove(0);
        }
    }

    /// The record for `test_id` if it exists and is at most 15 minutes old at `now_ms`. A test
    /// "from the future" (clock moved back) does not count either.
    pub fn find(&self, test_id: &str, now_ms: i64) -> Option<TestRecord> {
        let mut all = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        all.retain(|e| {
            let age = now_ms.saturating_sub(e.at_ms);
            (0..=TEST_TTL_MS).contains(&age)
        });
        all.iter().find(|e| e.test_id == test_id).cloned()
    }

    /// A test is single-use: once its approval is stored it is forgotten.
    pub fn consume(&self, test_id: &str) {
        self.entries
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .retain(|e| e.test_id != test_id);
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

// ── Injected facts ──

/// Result of the engine's dry run for the test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DryRun {
    Done {
        summary: PlanSummary,
        conflicts: u32,
        guard: GuardVerdict,
    },
    /// The comparison could not be trusted (cap, errors); closed codes from the engine.
    Blocked(Vec<String>),
    /// The engine refused to start (profile invalid, not ready, ...); a closed code.
    Failed(String),
}

/// Everything the steps ask the outside world.
pub struct TestDeps<'a> {
    pub probe: &'a Arc<dyn RootProbe>,
    pub budget: Duration,
    /// The engine dry run of the profile policy.
    pub dry_run: &'a dyn Fn(&ProfilePolicy) -> DryRun,
    /// `access(folder, write)`: may the current user read (or add files to) this folder? Checked
    /// by opening the folder handle with the needed rights - no file is created.
    pub access: &'a dyn Fn(&str, bool) -> bool,
    /// Whether the background runner is set up and usable (P3 wires the real answer).
    pub background_ready: bool,
}

fn step(id: &str, status: StepStatus, text: impl Into<String>) -> StepDto {
    StepDto {
        id: id.into(),
        status,
        text: text.into(),
    }
}

fn path_text(path: &str) -> String {
    shorten_middle(path, 80)
}

/// Text for a not-ready profile (SP-8 reason codes) in the test.
fn not_ready_text(reason: &str) -> &'static str {
    match reason {
        "rootMissing" | "rootOffline" => {
            "A sync folder is unavailable - it is not treated as empty. Reconnect it and test again."
        }
        "identityMismatch" | "identityUnknown" => {
            "A sync folder is no longer the folder this profile was saved for. Review the profile."
        }
        "recycleBinUnavailable" => {
            "The Recycle Bin is not available for this folder. Change the profile's delete policy."
        }
        "versioningRootInvalid" => {
            "The profile's versioning folder is not valid. Review the profile."
        }
        "rootProtected" | "rootLocalShare" => {
            "A sync folder is not allowed for automatic syncing. Review the profile."
        }
        _ => "The sync profile is not valid. Review it and test again.",
    }
}

/// Sides whose files are read by a run: the source side of one-way and mirror, both of two-way.
fn read_sides(mode: &SyncMode) -> Vec<Side> {
    match mode {
        SyncMode::OneWay { from } | SyncMode::Mirror { from } => vec![*from],
        SyncMode::TwoWay { .. } => vec![Side::Left, Side::Right],
    }
}

fn root_of(profile: &ProfilePolicy, side: Side) -> &str {
    match side {
        Side::Left => &profile.left.path,
        Side::Right => &profile.right.path,
    }
}

/// The ordered steps of one test. Stops after the first `fail`.
pub fn run_steps(
    profile_name: &str,
    profile_revision: i64,
    profile: &ProfilePolicy,
    automation: &AutomationPolicy,
    deps: &TestDeps<'_>,
) -> Vec<StepDto> {
    let mut steps: Vec<StepDto> = Vec::new();
    macro_rules! push {
        ($s:expr) => {{
            let s: StepDto = $s;
            let failed = s.status == StepStatus::Fail;
            steps.push(s);
            if failed {
                return steps;
            }
        }};
    }

    // 1. The profile at its current revision is valid and its folders are the saved ones.
    match readiness_dto(profile, deps.probe, deps.budget) {
        ReadinessDto::Ready => push!(step(
            "profile",
            StepStatus::Ok,
            format!(
                "Sync profile \"{}\" rev {profile_revision} is valid and ready.",
                shorten_middle(profile_name, 60)
            )
        )),
        ReadinessDto::Checking => push!(step(
            "profile",
            StepStatus::Fail,
            "The profile's folders are still being checked. Test again in a moment."
        )),
        ReadinessDto::NotReady { reason } => {
            push!(step("profile", StepStatus::Fail, not_ready_text(&reason)))
        }
    }

    // 2. Source folders are readable.
    for side in read_sides(&profile.mode) {
        let path = root_of(profile, side);
        if (deps.access)(path, false) {
            push!(step(
                "source",
                StepStatus::Ok,
                format!("{} is readable.", path_text(path))
            ));
        } else {
            push!(step(
                "source",
                StepStatus::Fail,
                format!(
                    "{} cannot be read with your account. Check its permissions and test again.",
                    path_text(path)
                )
            ));
        }
    }

    // 3. Target folders are writable (an access check; nothing is written).
    for side in crate::automation::model::written_sides(&profile.mode) {
        let path = root_of(profile, side);
        if (deps.access)(path, true) {
            push!(step(
                "target",
                StepStatus::Ok,
                format!("{} accepts changes.", path_text(path))
            ));
        } else {
            push!(step(
                "target",
                StepStatus::Fail,
                format!(
                    "{} cannot be changed with your account. Check its permissions and test again.",
                    path_text(path)
                )
            ));
        }
    }

    // 4-6. The engine's dry run: comparison, conflicts, delete guard.
    match (deps.dry_run)(profile) {
        DryRun::Done {
            summary,
            conflicts,
            guard,
        } => {
            let held = summary.conflicts + summary.skipped + summary.user_skipped;
            let operations = summary.total.saturating_sub(held);
            let created = operations
                .saturating_sub(summary.overwrites)
                .saturating_sub(summary.deletes);
            push!(step(
                "compare",
                StepStatus::Ok,
                format!(
                    "Comparison: {operations} operations ({} update, {created} new, {} delete).",
                    summary.overwrites, summary.deletes
                )
            ));
            if conflicts > 0 {
                push!(step(
                    "conflicts",
                    StepStatus::Warn,
                    format!(
                        "{conflicts} conflict{} found - would be held for review, never overwritten.",
                        if conflicts == 1 { "" } else { "s" }
                    )
                ));
            } else {
                push!(step("conflicts", StepStatus::Ok, "No conflicts found."));
            }
            match guard {
                GuardVerdict::Off => push!(step(
                    "guard",
                    StepStatus::Warn,
                    "The delete safety guard is off for this profile."
                )),
                GuardVerdict::Ok => push!(step(
                    "guard",
                    StepStatus::Ok,
                    "The delete safety guard would allow this comparison."
                )),
                GuardVerdict::Tripped {
                    deletes, percent, ..
                } => push!(step(
                    "guard",
                    StepStatus::Warn,
                    format!(
                        "This comparison would delete {deletes} items ({percent}%) - over the safety limit. An automatic run would stop and wait for you."
                    )
                )),
                GuardVerdict::EmptySource { .. } => push!(step(
                    "guard",
                    StepStatus::Warn,
                    "A source folder is empty while deletions are planned. An automatic run would refuse to run."
                )),
            }
        }
        DryRun::Blocked(reasons) => push!(step(
            "compare",
            StepStatus::Fail,
            format!(
                "The comparison could not be trusted ({}). Fix the problem and test again.",
                reasons.join(", ")
            )
        )),
        DryRun::Failed(code) => push!(step(
            "compare",
            StepStatus::Fail,
            format!("The comparison could not start ({code}). Review the profile and test again.")
        )),
    }

    // 7. Runtime readiness (RT-1).
    match automation.runtime {
        Runtime::App => push!(step(
            "runtime",
            StepStatus::Ok,
            "Runs while Ferrum is open."
        )),
        Runtime::Background if deps.background_ready => push!(step(
            "runtime",
            StepStatus::Ok,
            "The background runner is ready."
        )),
        Runtime::Background => push!(step(
            "runtime",
            StepStatus::Fail,
            "The background runner is not set up yet. Choose \"Only while Ferrum is open\" or set it up first."
        )),
    }

    // 8. Change notifications for each watched root (WA-2.3).
    if let Trigger::Watch(w) = &automation.trigger {
        for (watched, spec, side) in [
            (w.watch_left, &profile.left, "left"),
            (w.watch_right, &profile.right, "right"),
        ] {
            if !watched {
                continue;
            }
            if matches!(spec.identity, RootIdentity::Network { .. }) {
                push!(step(
                    "watch",
                    StepStatus::Fail,
                    format!(
                        "Change notifications are not available for the {side} folder (network share)."
                    )
                ));
            } else {
                push!(step(
                    "watch",
                    StepStatus::Ok,
                    format!("Change notifications are available for the {side} folder.")
                ));
            }
        }
    }
    steps
}

/// A test passes when no step failed (warnings are allowed).
pub fn passed(steps: &[StepDto]) -> bool {
    !steps.is_empty() && steps.iter().all(|s| s.status != StepStatus::Fail)
}

// ── Production access check ──

/// May the current user read (`write = false`) or add files and folders to (`write = true`) the
/// folder? Opens the folder with exactly those access rights - the file system's own access
/// check - and closes it; nothing is created or modified.
#[cfg(windows)]
pub fn fs_access(path: &str, write: bool) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ADD_FILE, FILE_ADD_SUBDIRECTORY, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_LIST_DIRECTORY, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    let wide = crate::shell_open::to_wide(path);
    let rights = if write {
        FILE_ADD_FILE | FILE_ADD_SUBDIRECTORY
    } else {
        FILE_LIST_DIRECTORY
    };
    // SAFETY: `wide` is a NUL-terminated UTF-16 string that outlives the call; the security
    // attributes and template handle are null; the returned handle is closed below when valid.
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            rights,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE || handle.is_null() {
        return false;
    }
    // SAFETY: `handle` is a valid handle returned by CreateFileW above and is closed once.
    unsafe {
        CloseHandle(handle);
    }
    true
}

#[cfg(not(windows))]
pub fn fs_access(_path: &str, _write: bool) -> bool {
    false
}

#[cfg(test)]
mod tests;
