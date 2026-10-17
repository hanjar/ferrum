//! Requests from the commands (and from controls) to the host (Automation design §3.2, §3.5,
//! RQ-4, RQ-10): Run now, Retry, Recheck, Stop, Pause & stop.
//!
//! **A request is not an order.** Everything here only writes a queue entry or a control row. It
//! checks what can be known without touching a folder - the approval verifies, the automation is
//! enabled and not held - so the user gets a clear answer at once, but the host asks the gate
//! again before a single file is touched (`gate.rs`). No request carries a policy, a root, a hash
//! or a "confirmed" flag; ids are validated by the caller.
//!
//! Tauri-free; `commands.rs` wraps these for the webview and the host calls the same functions
//! for the controls other processes left in the store.

use rusqlite::Connection;

use crate::automation::dpapi::ApprovalKeys;
use crate::automation::model::{AutomationPolicy, DeletePolicy, ProfilePolicy, Runtime};
use crate::automation::policy::{self, Verdict};
use crate::automation::store::controls::{self, Action, NewControl};
use crate::automation::store::queue::{self, Enqueued, HostClass, NewEntry, TriggerKind};
use crate::automation::store::runs::{self, Outcome, RecheckState};
use crate::automation::store::runtime::{self, TriggerState};
use crate::automation::store::{Store, StoreError, automations, hosts, profiles};

/// Controls one target may receive per second (threat T5: a flood of requests is refused).
pub const CONTROLS_PER_SECOND: i64 = 10;

/// Why a request was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestError {
    NotFound,
    Invalid,
    /// The approval does not verify (RQ-4: Run now is dimmed and shows the reason).
    NeedsReview,
    /// The automation needs attention first; the text is the hold's own ("1 failed - new runs held").
    Held(String),
    /// The automation is paused.
    Paused,
    /// A background automation while the runner is not running.
    RunnerDown,
    /// The run is not in the state the request needs (`not_held`, `recheck_first`, `not_running`).
    NotReady(&'static str),
    /// Another automation's entry on the same folders is in the way of a retry.
    Busy,
    /// Too many requests for this target in a second.
    TooMany,
    Store(StoreError),
}

impl From<StoreError> for RequestError {
    fn from(e: StoreError) -> Self {
        RequestError::Store(e)
    }
}

/// What a queued request turned into (the toasts of the prototype).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueueAnswer {
    /// A new entry waits for the host.
    Queued,
    /// It waits for another run on the same folders (`name`).
    Waiting { name: String },
    /// Merged into the entry that already waits for these folders (RQ-3.1).
    Merged,
    /// A folder is offline: the entry waits for the device (RQ-3.2).
    Device,
    /// A control was written for the host that owns the automation; it answers later.
    Requested,
}

pub type Answer = Result<QueueAnswer, RequestError>;

/// The closed text a host stores as the result of a control.
pub fn answer_text(answer: &Answer) -> String {
    match answer {
        Ok(QueueAnswer::Queued) => "queued".into(),
        Ok(QueueAnswer::Waiting { .. }) => "waiting".into(),
        Ok(QueueAnswer::Merged) => "merged".into(),
        Ok(QueueAnswer::Device) => "device".into(),
        Ok(QueueAnswer::Requested) => "requested".into(),
        Err(RequestError::NotFound) => "gone".into(),
        Err(RequestError::Invalid) => "invalid".into(),
        Err(RequestError::NeedsReview) => "review".into(),
        Err(RequestError::Held(_)) => "held".into(),
        Err(RequestError::Paused) => "paused".into(),
        Err(RequestError::RunnerDown) => "runnerDown".into(),
        Err(RequestError::NotReady(code)) => (*code).into(),
        Err(RequestError::Busy) => "busy".into(),
        Err(RequestError::TooMany) => "tooMany".into(),
        Err(RequestError::Store(_)) => "store".into(),
    }
}

fn class_of(policy: &AutomationPolicy) -> HostClass {
    match policy.runtime {
        Runtime::App => HostClass::App,
        Runtime::Background => HostClass::Background,
    }
}

fn roots_of(policy: &ProfilePolicy) -> Vec<String> {
    let mut roots = vec![policy.left.path.clone(), policy.right.path.clone()];
    if let DeletePolicy::Versioning { root } = &policy.delete_policy {
        roots.push(root.clone());
    }
    roots
}

/// The name of another automation's active run on folders that overlap `automation_id`'s.
pub fn blocker_name(conn: &Connection, automation_id: &str) -> Result<Option<String>, StoreError> {
    let Some(rec) = automations::get(conn, automation_id)? else {
        return Ok(None);
    };
    let roots_for = |profile_id: &str| -> Result<Vec<String>, StoreError> {
        Ok(profiles::get(conn, profile_id)?
            .and_then(|p| serde_json::from_str::<ProfilePolicy>(&p.policy).ok())
            .map(|p| roots_of(&p))
            .unwrap_or_default())
    };
    let mine = roots_for(&rec.profile_id)?;
    if mine.is_empty() {
        return Ok(None);
    }
    for run in runs::active(conn)? {
        if run.automation_id.as_deref() == Some(automation_id) {
            continue;
        }
        if queue::roots_overlap(&mine, &roots_for(&run.profile_id)?) {
            return Ok(Some(
                run.automation_name
                    .clone()
                    .unwrap_or_else(|| "another run".to_string()),
            ));
        }
    }
    Ok(None)
}

fn runner_live(conn: &Connection, now_ms: i64) -> Result<bool, StoreError> {
    Ok(hosts::all(conn)?.iter().any(|h| {
        h.kind == hosts::KIND_RUNNER
            && now_ms.saturating_sub(h.last_beat_ms) <= hosts::LIVE_WINDOW_MS
    }))
}

/// Everything that can be judged from the rows: the automation, its policy, the verdict and the
/// hold. `Ok` only for an automation that may be queued right now.
struct Checked {
    id: String,
    profile_id: String,
    policy: AutomationPolicy,
}

fn check(
    conn: &Connection,
    keys: &dyn ApprovalKeys,
    automation_id: &str,
) -> Result<Checked, RequestError> {
    let rec = automations::get(conn, automation_id)?.ok_or(RequestError::NotFound)?;
    let policy = serde_json::from_str::<AutomationPolicy>(&rec.policy)
        .map_err(|_| RequestError::NeedsReview)?;
    // The approval, not the `enabled` flag, decides (design §10.4).
    if !matches!(policy::verify(conn, keys, automation_id)?, Verdict::Valid) {
        return Err(RequestError::NeedsReview);
    }
    if let Some(attention) = runtime::get(conn, automation_id)?.and_then(|r| r.attention) {
        return Err(RequestError::Held(attention.text));
    }
    if !rec.enabled {
        return Err(RequestError::Paused);
    }
    Ok(Checked {
        id: rec.id,
        profile_id: rec.profile_id,
        policy,
    })
}

fn answer_for(conn: &Connection, enq: Enqueued, automation_id: &str) -> Answer {
    match enq {
        Enqueued::Refused => Err(RequestError::Busy),
        Enqueued::Merged { .. } => Ok(QueueAnswer::Merged),
        Enqueued::Queued { .. } => {
            let device = runtime::get(conn, automation_id)?
                .is_some_and(|r| r.trigger_state == Some(TriggerState::Device));
            if device {
                return Ok(QueueAnswer::Device);
            }
            Ok(match blocker_name(conn, automation_id)? {
                Some(name) => QueueAnswer::Waiting { name },
                None => QueueAnswer::Queued,
            })
        }
    }
}

/// The per-target rate limit (T5). Each action has its own bucket, so a flood of one kind of
/// request (Recheck, say) can never keep a Stop from reaching the host.
fn rate_limited(
    conn: &Connection,
    action: Action,
    automation_id: Option<&str>,
    run_id: Option<i64>,
    now_ms: i64,
) -> Result<bool, StoreError> {
    Ok(controls::recent_count(
        conn,
        action,
        automation_id,
        run_id,
        now_ms.saturating_sub(1_000),
    )? >= CONTROLS_PER_SECOND)
}

/// Is any run active on folders that overlap this automation's (its own run included)? A recheck
/// compares read-only, but against a tree a run is changing it would report a half-synced state.
pub fn folders_in_use(conn: &Connection, automation_id: &str) -> Result<bool, StoreError> {
    let Some(rec) = automations::get(conn, automation_id)? else {
        return Ok(false);
    };
    let roots_for = |profile_id: &str| -> Result<Vec<String>, StoreError> {
        Ok(profiles::get(conn, profile_id)?
            .and_then(|p| serde_json::from_str::<ProfilePolicy>(&p.policy).ok())
            .map(|p| roots_of(&p))
            .unwrap_or_default())
    };
    let mine = roots_for(&rec.profile_id)?;
    for run in runs::active(conn)? {
        if queue::roots_overlap(&mine, &roots_for(&run.profile_id)?) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Run now, host side: the entry is queued here (also what a host does with a Run now control).
pub fn run_now_in(
    store: &Store,
    keys: &dyn ApprovalKeys,
    automation_id: &str,
    now_ms: i64,
) -> Answer {
    super::write_tx(store, |conn| {
        Ok(run_now_tx(conn, keys, automation_id, now_ms, false))
    })?
}

/// RQ-4 Run now, command side. For an app automation the entry is queued here; for a background
/// automation a control is written and the runner enqueues (design §3.2).
pub fn run_now_command(
    store: &Store,
    keys: &dyn ApprovalKeys,
    automation_id: &str,
    now_ms: i64,
) -> Answer {
    super::write_tx(store, |conn| {
        Ok(run_now_tx(conn, keys, automation_id, now_ms, true))
    })?
}

fn run_now_tx(
    conn: &Connection,
    keys: &dyn ApprovalKeys,
    automation_id: &str,
    now_ms: i64,
    via_command: bool,
) -> Answer {
    let checked = check(conn, keys, automation_id)?;
    let class = class_of(&checked.policy);
    if via_command && class == HostClass::Background {
        // The runner enqueues; the app only asks (and says so when nobody is there to answer).
        if !runner_live(conn, now_ms)? {
            return Err(RequestError::RunnerDown);
        }
        if rate_limited(conn, Action::RunNow, Some(&checked.id), None, now_ms)? {
            return Err(RequestError::TooMany);
        }
        controls::post(
            conn,
            &NewControl {
                action: Action::RunNow,
                automation_id: Some(&checked.id),
                run_id: None,
            },
            now_ms,
        )?;
        return Ok(QueueAnswer::Requested);
    }
    let enq = queue::enqueue(
        conn,
        &NewEntry {
            automation_id: &checked.id,
            profile_id: &checked.profile_id,
            host_class: class,
            kind: TriggerKind::RunNow,
            text: "Run now",
            retry_of: None,
        },
        now_ms,
    )?;
    answer_for(conn, enq, &checked.id)
}

/// A trigger source (the schedule hook: an on-time occurrence or a catch-up) asks for a run, host
/// side. The same checks as Run now - the approval VERIFIES (the `enabled` flag is not an
/// authorization), no attention hold, not paused - and then only a queue entry is written. The
/// gate asks all of it again, and more, before any file is touched. A user kind is refused: a
/// trigger source can never pass for the user's own request.
pub(super) fn trigger_tx(
    conn: &Connection,
    keys: &dyn ApprovalKeys,
    automation_id: &str,
    kind: TriggerKind,
    text: &str,
    now_ms: i64,
) -> Answer {
    if !matches!(kind, TriggerKind::Schedule | TriggerKind::CatchUp) {
        return Err(RequestError::Invalid);
    }
    let checked = check(conn, keys, automation_id)?;
    let class = class_of(&checked.policy);
    let enq = queue::enqueue(
        conn,
        &NewEntry {
            automation_id: &checked.id,
            profile_id: &checked.profile_id,
            host_class: class,
            kind,
            text,
            retry_of: None,
        },
        now_ms,
    )?;
    answer_for(conn, enq, &checked.id)
}

/// RQ-10.2 Retry, host side: queue a run with `retryOf`. Needs a recheck that found N > 0
/// remaining and the run not to be a conflict.
pub fn retry_in(store: &Store, keys: &dyn ApprovalKeys, run_id: i64, now_ms: i64) -> Answer {
    super::write_tx(store, |conn| {
        Ok(retry_tx(conn, keys, run_id, now_ms, false))
    })?
}

/// RQ-10.2 Retry, command side.
pub fn retry_command(store: &Store, keys: &dyn ApprovalKeys, run_id: i64, now_ms: i64) -> Answer {
    super::write_tx(store, |conn| Ok(retry_tx(conn, keys, run_id, now_ms, true)))?
}

fn retry_tx(
    conn: &Connection,
    keys: &dyn ApprovalKeys,
    run_id: i64,
    now_ms: i64,
    via_command: bool,
) -> Answer {
    if run_id <= 0 {
        return Err(RequestError::Invalid);
    }
    let run = runs::get(conn, run_id)?.ok_or(RequestError::NotFound)?;
    let automation_id = run.automation_id.clone().ok_or(RequestError::NotFound)?;
    if run.ended_ms.is_none() {
        return Err(RequestError::NotReady("not_held"));
    }
    // The hold must name THIS run, and a recheck must have found something left to do.
    let rt = runtime::get(conn, &automation_id)?.ok_or(RequestError::NotFound)?;
    if rt.attention.as_ref().map(|a| a.run_id) != Some(run_id) {
        return Err(RequestError::NotReady("not_held"));
    }
    let remaining = matches!(
        &run.recheck,
        Some(r) if r.state == RecheckState::Remaining && r.remaining > 0
    );
    if !remaining || run.outcome == Some(Outcome::Conflict) {
        return Err(RequestError::NotReady("recheck_first"));
    }
    let rec = automations::get(conn, &automation_id)?.ok_or(RequestError::NotFound)?;
    let policy = serde_json::from_str::<AutomationPolicy>(&rec.policy)
        .map_err(|_| RequestError::NeedsReview)?;
    if !matches!(policy::verify(conn, keys, &automation_id)?, Verdict::Valid) {
        return Err(RequestError::NeedsReview);
    }
    if !rec.enabled {
        return Err(RequestError::Paused);
    }
    let class = class_of(&policy);
    if via_command && class == HostClass::Background {
        if !runner_live(conn, now_ms)? {
            return Err(RequestError::RunnerDown);
        }
        if rate_limited(conn, Action::Retry, None, Some(run_id), now_ms)? {
            return Err(RequestError::TooMany);
        }
        controls::post(
            conn,
            &NewControl {
                action: Action::Retry,
                automation_id: None,
                run_id: Some(run_id),
            },
            now_ms,
        )?;
        return Ok(QueueAnswer::Requested);
    }
    let enq = queue::enqueue(
        conn,
        &NewEntry {
            automation_id: &automation_id,
            profile_id: &rec.profile_id,
            host_class: class,
            kind: TriggerKind::Retry,
            text: &format!("Retry of run #{run_id}"),
            retry_of: Some(run_id),
        },
        now_ms,
    )?;
    answer_for(conn, enq, &automation_id)
}

/// RQ-10.1 Recheck: a control for the host that owns the automation. Only for the run an
/// attention hold names.
pub fn recheck_command(store: &Store, run_id: i64, now_ms: i64) -> Answer {
    if run_id <= 0 {
        return Err(RequestError::Invalid);
    }
    super::write_tx(store, |conn| {
        Ok((|| -> Answer {
            let run = runs::get(conn, run_id)?.ok_or(RequestError::NotFound)?;
            let automation_id = run.automation_id.clone().ok_or(RequestError::NotFound)?;
            let held = runtime::get(conn, &automation_id)?
                .and_then(|r| r.attention)
                .is_some_and(|a| a.run_id == run_id);
            if run.ended_ms.is_none() || !held {
                return Err(RequestError::NotReady("not_held"));
            }
            if rate_limited(conn, Action::Recheck, None, Some(run_id), now_ms)? {
                return Err(RequestError::TooMany);
            }
            controls::post(
                conn,
                &NewControl {
                    action: Action::Recheck,
                    automation_id: None,
                    run_id: Some(run_id),
                },
                now_ms,
            )?;
            Ok(QueueAnswer::Requested)
        })())
    })?
}

/// RQ-8.1 Stop: a control for the host that runs it. Only a run that has not ended.
pub fn stop_command(store: &Store, run_id: i64, now_ms: i64) -> Answer {
    if run_id <= 0 {
        return Err(RequestError::Invalid);
    }
    super::write_tx(store, |conn| {
        Ok((|| -> Answer {
            let run = runs::get(conn, run_id)?.ok_or(RequestError::NotFound)?;
            if run.automation_id.is_none() || run.ended_ms.is_some() {
                return Err(RequestError::NotReady("not_running"));
            }
            if rate_limited(conn, Action::Stop, None, Some(run_id), now_ms)? {
                return Err(RequestError::TooMany);
            }
            controls::post(
                conn,
                &NewControl {
                    action: Action::Stop,
                    automation_id: None,
                    run_id: Some(run_id),
                },
                now_ms,
            )?;
            Ok(QueueAnswer::Requested)
        })())
    })?
}

/// RQ-8.2 "Pause & stop current run" / "Pause & let it finish": after the configuration part
/// (`approval::pause_in`) tells the host what to do with the run that is going. Nothing is
/// written when no run is in progress.
pub fn pause_run_part(
    store: &Store,
    automation_id: &str,
    stop: bool,
    now_ms: i64,
) -> Result<(), RequestError> {
    super::write_tx(store, |conn| {
        Ok((|| -> Result<(), RequestError> {
            if !automations::has_running_run(conn, automation_id)? {
                return Ok(());
            }
            // No rate limit here: "Pause & stop" is the user's one chance to stop the run, and
            // the pause itself already happened. The control table is bounded by its 10 minute
            // life, and a flood of other requests has its own bucket.
            controls::post(
                conn,
                &NewControl {
                    action: if stop {
                        Action::PauseStop
                    } else {
                        Action::PauseFinish
                    },
                    automation_id: Some(automation_id),
                    run_id: None,
                },
                now_ms,
            )?;
            Ok(())
        })())
    })?
}
