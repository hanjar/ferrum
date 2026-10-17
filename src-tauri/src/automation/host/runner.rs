//! One pass through the queue and one run (Automation design §9.4, RQ-1, RQ-7, RQ-9, RQ-10):
//! gate → start the history row → engine → settle the history row, the attention hold, the
//! events and follow-up entries. Also the read-only recheck, the read-only comparison of a watch
//! trigger and the recovery of orphan runs.
//!
//! Everything that writes goes through [`Inner::write`], one immediate transaction that also
//! bumps `change_seq`. No closure here calls the engine; the engine is called by [`run_gated`]
//! (`engine.rs`) only, with the proof the gate returned.

use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rusqlite::Connection;

use super::gate::{GateCtx, GateKind, GateOk, GateRefusal, RunMode, gate};
use super::watch::{self, CheckAnswer};
use super::{
    Active, EngineCall, HostDeps, HostEvent, Inner, MAX_RERUN_STREAK, ProgressEvent,
    RERUN_DELAY_MS, RETRY_GAP_MS, State, StopKind,
};
use crate::automation::model::{DeletePolicy, ProfilePolicy, Trigger};
use crate::automation::status::review_label;
use crate::automation::store::automations::{self, EventKind};
use crate::automation::store::controls::{self, Action};
use crate::automation::store::events;
use crate::automation::store::queue::{self, NewEntry, QueueEntry, TriggerKind};
use crate::automation::store::runs::{
    self, Finish, NewOp, NewRun, OpKind, OpSide, Outcome, Phase, RecheckRecord, RecheckState,
    RunCounts, RunRecord,
};
use crate::automation::store::runtime::{self, TriggerState};
use crate::automation::store::{StoreError, profiles};
use crate::ops::CancelToken;
use crate::sync_engine::delete_policy::GateError;
use crate::sync_engine::execute::{
    Counts, EngineError, EngineOutcome, EngineReport, EngineSink, Phase as EnginePhase, SettleState,
};
use crate::sync_engine::opset::OpStatus as EngineOpStatus;
use crate::sync_engine::plan::SyncAction;
use crate::sync_engine::{GuardVerdict, InterruptReason};

/// Marks the run slot busy for the lifetime of a worker.
pub(super) struct SlotGuard<'a, D: HostDeps>(&'a Arc<Inner<D>>);

impl<'a, D: HostDeps> SlotGuard<'a, D> {
    pub(super) fn new(inner: &'a Arc<Inner<D>>) -> Self {
        SlotGuard(inner)
    }
}

impl<D: HostDeps> Drop for SlotGuard<'_, D> {
    fn drop(&mut self) {
        self.0.lock().busy = false;
        self.0.nudge();
    }
}

/// Counts a watch comparison while it works and clears its flag, whatever happens.
struct CheckGuard<D: HostDeps> {
    inner: Arc<Inner<D>>,
    shared: Arc<watch::Shared>,
}

impl<D: HostDeps> Drop for CheckGuard<D> {
    fn drop(&mut self) {
        self.shared.set_running(false);
        let mut state = self.inner.lock();
        state.comparing = state.comparing.saturating_sub(1);
        drop(state);
        self.inner.nudge();
    }
}

/// Watch comparisons that may run at the same time (they only read).
const MAX_COMPARES: usize = 2;

/// Start the read-only comparison of a watch trigger on its own thread. `false` = no capacity
/// right now (the hook asks again at its next pass).
pub(super) fn begin_check<D: HostDeps>(
    inner: &Arc<Inner<D>>,
    automation_id: &str,
    shared: &Arc<watch::Shared>,
) -> bool {
    {
        let mut state = inner.lock();
        if state.shutdown || state.comparing >= MAX_COMPARES {
            return false;
        }
        state.comparing += 1;
    }
    let cancel = CancelToken::default();
    shared.set_cancel(cancel.clone());
    shared.set_running(true);
    let guard = CheckGuard {
        inner: inner.clone(),
        shared: shared.clone(),
    };
    let automation_id = automation_id.to_string();
    let started = inner.deps.spawn(
        "ferrum-automation-check",
        Box::new(move || {
            let answer = compare(&guard.inner, &automation_id, cancel);
            watch::finish_check(&guard.inner, &automation_id, &guard.shared, answer);
        }),
    );
    // A job that could not be started is dropped, and the guard it owns undoes what was counted.
    started
}

/// The comparison itself: the gate in read-only `Compare` mode (approval verified, no hold,
/// folders present and the approved ones, NO lease), then a dry run of the engine. Nothing is
/// written to any folder, and no run row is made.
fn compare<D: HostDeps>(
    inner: &Arc<Inner<D>>,
    automation_id: &str,
    cancel: CancelToken,
) -> CheckAnswer {
    let env = inner.deps.engine_env();
    let unmet = |c: &crate::automation::model::Conditions| inner.deps.unmet_condition(c);
    let ctx = GateCtx {
        store: &inner.store,
        keys: inner.deps.keys(),
        class: inner.class,
        lease_env: inner.deps.lease_env(),
        env: &env,
        unmet_condition: &unmet,
    };
    let proof = match gate(&ctx, automation_id, GateKind::Compare) {
        Ok(p) => p,
        Err(GateRefusal::Device(_)) => return CheckAnswer::Device,
        Err(GateRefusal::Changed | GateRefusal::Store(_) | GateRefusal::LeaseBusy(_)) => {
            return CheckAnswer::Retry;
        }
        Err(other) => return CheckAnswer::Refused(other.code()),
    };
    let call = EngineCall {
        run_id: 0,
        retries: 0,
        dry_run: true,
        incremental: inner.deps.incremental(),
        cancel,
        sink: &crate::sync_engine::NoSink,
    };
    let report =
        std::panic::catch_unwind(AssertUnwindSafe(|| inner.deps.run_engine(&proof, &call)))
            .unwrap_or_else(|_| internal_failure());
    drop(proof);
    match report.outcome {
        EngineOutcome::DryRun {
            plan, conflicts, ..
        } => {
            let remaining = (plan.to_right + plan.to_left + plan.deletes) as i64;
            if remaining == 0 && conflicts == 0 {
                CheckAnswer::InSync
            } else {
                CheckAnswer::Differences {
                    remaining,
                    conflicts: i64::from(conflicts),
                }
            }
        }
        // Cap, errors, a folder that went away: the comparison cannot be trusted.
        _ => CheckAnswer::Unreliable,
    }
}

pub(super) struct RecheckGuard<'a, D: HostDeps>(&'a Arc<Inner<D>>);

impl<'a, D: HostDeps> RecheckGuard<'a, D> {
    pub(super) fn new(inner: &'a Arc<Inner<D>>) -> Self {
        RecheckGuard(inner)
    }
}

impl<D: HostDeps> Drop for RecheckGuard<'_, D> {
    fn drop(&mut self) {
        let mut state = self.0.lock();
        state.rechecking = state.rechecking.saturating_sub(1);
        drop(state);
        self.0.nudge();
    }
}

/// May the gate be asked about this entry now? (A kept entry waits a few seconds; a pending
/// rerun waits for the folders to settle.)
pub(super) fn eligible(state: &State, entry: &QueueEntry, now: i64, unattended: bool) -> bool {
    if !unattended {
        // Automatic runs are off: only the user's Run now / Retry may start the engine, and only
        // while the request is fresh. (An entry the gate answers with "keep" is REMOVED, so no
        // later host - after a restart or a takeover - can start it unasked.)
        if !is_user_request(entry.kind) || user_request_expired(entry, now) {
            return false;
        }
    }
    if state.next_try.get(&entry.id).is_some_and(|t| now < *t) {
        return false;
    }
    if entry.kind == TriggerKind::PendingRerun
        && now >= entry.enqueued_ms
        && now < entry.enqueued_ms.saturating_add(RERUN_DELAY_MS)
    {
        return false;
    }
    true
}

/// A user request older than this is not carried out any more (the same life as a control).
pub(super) const USER_REQUEST_TTL_MS: i64 = 10 * 60 * 1000;

/// A user request stamped further ahead than this is not believed (the clock was set back since):
/// the same tolerance the controls use.
pub(super) const FUTURE_SKEW_MS: i64 = 60_000;

/// Stale = older than the TTL, or stamped more than [`FUTURE_SKEW_MS`] in the future of the
/// clock. The second half matters when the clock moved back: without it a request would stay
/// "fresh" for as long as the clock needs to catch up with its stamp.
fn user_request_expired(entry: &QueueEntry, now: i64) -> bool {
    let age = now.saturating_sub(entry.enqueued_ms);
    !(-FUTURE_SKEW_MS..=USER_REQUEST_TTL_MS).contains(&age)
}

/// With automatic runs off: a user request that nobody could start in time is dropped with an
/// event, instead of starting hours later when a host finally comes up.
pub(super) fn expire_user_requests<D: HostDeps>(
    inner: &Arc<Inner<D>>,
    entries: &[QueueEntry],
    now: i64,
) {
    let old: Vec<&QueueEntry> = entries
        .iter()
        .filter(|e| is_user_request(e.kind) && user_request_expired(e, now))
        .collect();
    if old.is_empty() {
        return;
    }
    let _ = inner.write(|conn| {
        for e in &old {
            if queue::remove(conn, e.id)? {
                add_event_once(
                    conn,
                    &e.automation_id,
                    now,
                    EventKind::Warn,
                    "Your Run now was not started in time \u{2014} run it again",
                )?;
            }
        }
        Ok(())
    });
    for e in old {
        forget_entry(inner, e.id);
    }
}

/// A trigger the user asked for at that moment.
fn is_user_request(kind: TriggerKind) -> bool {
    matches!(kind, TriggerKind::RunNow | TriggerKind::Retry)
}

/// With automatic runs off, say once per entry that it waits for the user (no engine call).
pub(super) fn park_unattended<D: HostDeps>(
    inner: &Arc<Inner<D>>,
    entries: &[QueueEntry],
    now: i64,
) {
    let fresh: Vec<&QueueEntry> = {
        let state = inner.lock();
        entries
            .iter()
            .filter(|e| {
                !is_user_request(e.kind) && state.parked_logged.get(&e.id) != Some(&e.enqueued_ms)
            })
            .collect()
    };
    if fresh.is_empty() {
        return;
    }
    let _ = inner.write(|conn| {
        for e in &fresh {
            add_event_once(
                conn,
                &e.automation_id,
                now,
                EventKind::Info,
                "Waiting \u{2014} run it with Run now (automatic runs are off)",
            )?;
        }
        Ok(())
    });
    let mut state = inner.lock();
    for e in fresh {
        state.parked_logged.insert(e.id, e.enqueued_ms);
    }
}

/// Drop what the host remembers about an entry that has left the queue (row ids are reused).
fn forget_entry<D: HostDeps>(inner: &Arc<Inner<D>>, id: i64) {
    let mut state = inner.lock();
    state.logged_refusal.remove(&id);
    state.parked_logged.remove(&id);
    state.next_try.remove(&id);
}

enum Attempt {
    /// A run took the slot and has ended.
    Ran,
    /// The entry stays queued.
    Kept,
    /// The entry is gone (refused and removed, or withdrawn meanwhile).
    Gone,
}

/// Walk the queue of this host class in FIFO order and run the first entry the gate lets through.
/// A blocked entry does not hold up a later one (RQ-3.3).
pub(super) fn start_cycle<D: HostDeps>(inner: &Arc<Inner<D>>) {
    let now = inner.deps.now_ms();
    let class = inner.class;
    let Ok(entries) = inner
        .store
        .with_read(|conn| -> Result<Vec<QueueEntry>, StoreError> { queue::for_host(conn, class) })
    else {
        return;
    };
    // Forget bookkeeping of entries that no longer exist.
    {
        let mut state = inner.lock();
        let live: std::collections::HashSet<i64> = entries.iter().map(|e| e.id).collect();
        state.next_try.retain(|id, _| live.contains(id));
        state.logged_refusal.retain(|id, _| live.contains(id));
        state.parked_logged.retain(|id, _| live.contains(id));
    }
    for entry in entries {
        if inner.lock().shutdown {
            return;
        }
        if !eligible(&inner.lock(), &entry, now, inner.unattended) {
            continue;
        }
        match attempt(inner, &entry, now) {
            Attempt::Ran => return,
            Attempt::Kept | Attempt::Gone => {}
        }
    }
}

fn attempt<D: HostDeps>(inner: &Arc<Inner<D>>, entry: &QueueEntry, now: i64) -> Attempt {
    let env = inner.deps.engine_env();
    let unmet = |c: &crate::automation::model::Conditions| inner.deps.unmet_condition(c);
    let ctx = GateCtx {
        store: &inner.store,
        keys: inner.deps.keys(),
        class: inner.class,
        lease_env: inner.deps.lease_env(),
        env: &env,
        unmet_condition: &unmet,
    };
    let kind = GateKind::Run {
        trigger: entry.kind,
        retry_of: entry.retry_of,
    };
    match gate(&ctx, &entry.automation_id, kind) {
        Ok(proof) => run_entry(inner, entry, proof),
        Err(refusal) => refuse(inner, entry, refusal, now),
    }
}

// ── Refusals ──

fn gate_error_text(e: GateError) -> &'static str {
    match e {
        GateError::RootUnavailable(_) => "A folder of this profile is not available",
        GateError::IdentityMismatch(_) => {
            "A folder of this profile is not the folder it was created for"
        }
        _ => "A folder of this profile cannot be used",
    }
}

/// Add an event unless the newest one of the automation already says exactly that.
fn add_event_once(
    conn: &Connection,
    automation_id: &str,
    now: i64,
    kind: EventKind,
    text: &str,
) -> Result<(), StoreError> {
    let newest = events::events(conn, automation_id, 1)?;
    if newest
        .first()
        .is_some_and(|e| e.text == events::event_text(text))
    {
        return Ok(());
    }
    events::add_event(conn, automation_id, now, kind, text)
}

/// Record one refusal: one event, the right status, the queue entry removed or kept. Never a
/// file operation.
fn refuse<D: HostDeps>(
    inner: &Arc<Inner<D>>,
    entry: &QueueEntry,
    refusal: GateRefusal,
    now: i64,
) -> Attempt {
    let code = refusal.code();
    // A kept entry is logged once per reason, and asked again after a short gap.
    if refusal.keeps_entry()
        && !inner.unattended
        && is_user_request(entry.kind)
        && !matches!(refusal, GateRefusal::Store(_))
    {
        // Automatic runs are off: the user's request was asked once and the answer is "not now".
        // The entry is REMOVED (not just remembered as asked): the memory dies with the process,
        // and a host that starts later must not run a request nobody repeated.
        forget_entry(inner, entry.id);
        let _ = record_kept(inner, entry, &refusal, now);
        let _ = inner.write(|conn| {
            queue::remove(conn, entry.id)?;
            add_event_once(
                conn,
                &entry.automation_id,
                now,
                EventKind::Info,
                "Waiting \u{2014} run it with Run now (automatic runs are off)",
            )
        });
        return Attempt::Gone;
    }
    if refusal.keeps_entry() {
        let mut state = inner.lock();
        state
            .next_try
            .insert(entry.id, now.saturating_add(RETRY_GAP_MS));
        let first = state.logged_refusal.get(&entry.id) != Some(&code);
        state.logged_refusal.insert(entry.id, code);
        drop(state);
        if first {
            let _ = record_kept(inner, entry, &refusal, now);
        }
        return Attempt::Kept;
    }
    forget_entry(inner, entry.id);
    let result = inner.write(|conn| {
        let removed = queue::remove(conn, entry.id)?;
        let aid = entry.automation_id.as_str();
        match refusal {
            GateRefusal::NotFound => {}
            GateRefusal::Disabled => {
                add_event_once(
                    conn,
                    aid,
                    now,
                    EventKind::Mute,
                    "Not run: the automation is paused",
                )?;
            }
            GateRefusal::WrongRuntime => {
                add_event_once(
                    conn,
                    aid,
                    now,
                    EventKind::Warn,
                    "Not run: this automation belongs to another runtime",
                )?;
            }
            GateRefusal::NeedsReview(reason) => {
                add_event_once(
                    conn,
                    aid,
                    now,
                    EventKind::Warn,
                    &format!("Not run: needs review \u{2014} {}", review_label(&reason)),
                )?;
            }
            GateRefusal::Held => {
                add_event_once(
                    conn,
                    aid,
                    now,
                    EventKind::Mute,
                    "Trigger held: the automation needs attention",
                )?;
            }
            GateRefusal::Blocked(e) => fail_without_running(conn, inner, entry, e, now)?,
            // Kept refusals returned above.
            GateRefusal::Conditions(_)
            | GateRefusal::Device(_)
            | GateRefusal::LeaseBusy(_)
            | GateRefusal::Changed
            | GateRefusal::Store(_) => {}
        }
        // Design §9.5: triggers of other automations that were merged into this entry must not
        // vanish with it when the owner is paused, held or in review.
        if removed
            && matches!(
                refusal,
                GateRefusal::Disabled
                    | GateRefusal::NeedsReview(_)
                    | GateRefusal::Held
                    | GateRefusal::WrongRuntime
            )
        {
            super::requeue::requeue_siblings_if(
                inner.unattended,
                conn,
                inner.deps.keys(),
                std::slice::from_ref(entry),
                now,
            )?;
        }
        Ok(removed)
    });
    match result {
        Ok(_) => Attempt::Gone,
        Err(_) => {
            // The store refused the bookkeeping: try again in a few seconds, not every tick.
            inner
                .lock()
                .next_try
                .insert(entry.id, now.saturating_add(RETRY_GAP_MS));
            Attempt::Kept
        }
    }
}

/// Events (and the Device state) for a refusal that leaves the entry queued.
fn record_kept<D: HostDeps>(
    inner: &Arc<Inner<D>>,
    entry: &QueueEntry,
    refusal: &GateRefusal,
    now: i64,
) -> Result<(), StoreError> {
    inner.write(|conn| {
        let aid = entry.automation_id.as_str();
        match refusal {
            GateRefusal::Conditions(why) => {
                events::add_event(conn, aid, now, EventKind::Info, &format!("Waiting: {why}"))?;
            }
            GateRefusal::Device(e) => {
                runtime::set_trigger_state(conn, aid, TriggerState::Device, None, None, None, now)?;
                events::add_event(
                    conn,
                    aid,
                    now,
                    EventKind::Info,
                    &format!("Waiting for device: {}", gate_error_text(*e)),
                )?;
            }
            // Busy folders are the normal "waits for another run" state: shown by the status,
            // not logged. A changed approval or a store hiccup is re-judged at the next try.
            _ => {}
        }
        Ok(())
    })
}

/// A prerequisite of the delete policy (Recycle Bin, versioning root) or a protected-folder rule
/// fails: no operation runs, but the user must know. It is a run that failed before any
/// operation (RQ-9 "Failed"), so it is recorded as one and holds the automation's new runs.
fn fail_without_running<D: HostDeps>(
    conn: &Connection,
    inner: &Arc<Inner<D>>,
    entry: &QueueEntry,
    error: GateError,
    now: i64,
) -> Result<(), StoreError> {
    let aid = entry.automation_id.as_str();
    let Some(rec) = automations::get(conn, aid)? else {
        return Ok(());
    };
    let profile = profiles::get(conn, &rec.profile_id)?;
    let text = profile
        .as_ref()
        .and_then(|p| serde_json::from_str::<ProfilePolicy>(&p.policy).ok())
        .map(|p| error.text(&p))
        .unwrap_or_else(|| gate_error_text(error).to_string());
    let run_id = runs::start(
        conn,
        &NewRun {
            automation_id: Some(aid),
            automation_name: Some(&rec.name),
            profile_id: &rec.profile_id,
            profile_revision: profile.as_ref().map_or(1, |p| p.revision.max(1)),
            policy_hash: None,
            host: inner.run_host(),
            trigger_kind: entry.kind.as_str(),
            trigger_text: &entry.text,
            merged_count: entry.merged_count,
            retry_of: entry.retry_of,
            started_ms: now,
        },
    )?;
    runs::finish(
        conn,
        run_id,
        &Finish {
            ended_ms: now,
            outcome: Outcome::Failed,
            ops_total: 0,
            counts: RunCounts::default(),
            error_text: Some(&text),
            note: None,
        },
    )?;
    events::add_event_for_run(
        conn,
        aid,
        now,
        EventKind::Bad,
        &format!("Run #{run_id} failed before any file was touched: {text}"),
        Some(run_id),
    )
}

// ── Running ──

/// Engine progress → the run row (phase, counts) and the shell. Throttled: the row at most once a
/// second, the event at most five times a second.
struct RunSink<'a, D: HostDeps> {
    inner: &'a Arc<Inner<D>>,
    run_id: i64,
    automation_id: String,
    state: Mutex<SinkState>,
}

#[derive(Default)]
struct SinkState {
    last_row: Option<Instant>,
    last_emit: Option<Instant>,
    phase: Option<Phase>,
    counts: RunCounts,
    total: i64,
}

fn counts_of(c: &Counts) -> RunCounts {
    RunCounts {
        ok: i64::from(c.done),
        failed: i64::from(c.failed),
        not_started: i64::from(c.not_started),
        held: i64::from(c.held),
        skipped_changed: i64::from(c.skipped) + i64::from(c.changed),
    }
}

impl<D: HostDeps> RunSink<'_, D> {
    fn emit(&self, st: &SinkState, force: bool) {
        let due = force
            || st
                .last_emit
                .is_none_or(|t| t.elapsed() >= Duration::from_millis(200));
        if !due {
            return;
        }
        self.inner.deps.emit(&HostEvent::Progress(ProgressEvent {
            run_id: self.run_id,
            automation_id: self.automation_id.clone(),
            phase: st.phase.unwrap_or(Phase::Gate).as_str(),
            ops_total: st.total,
            counts: st.counts,
        }));
    }
}

impl<D: HostDeps> EngineSink for RunSink<'_, D> {
    fn phase(&self, phase: EnginePhase) {
        let mapped = match phase {
            EnginePhase::Checking => Phase::Gate,
            EnginePhase::Comparing => Phase::Compare,
            EnginePhase::Syncing | EnginePhase::Stopping => Phase::Sync,
            EnginePhase::Settling => Phase::Settle,
        };
        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if st.phase == Some(mapped) {
            return;
        }
        st.phase = Some(mapped);
        let _ = self.inner.write(|conn| {
            runs::set_phase(conn, self.run_id, mapped)?;
            Ok(())
        });
        self.emit(&st, true);
        st.last_emit = Some(Instant::now());
    }

    fn counts(&self, counts: &Counts) {
        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        st.counts = counts_of(counts);
        st.total = i64::from(counts.total);
        if st
            .last_row
            .is_none_or(|t| t.elapsed() >= Duration::from_secs(1))
        {
            st.last_row = Some(Instant::now());
            let (total, rc) = (st.total, st.counts);
            let _ = self.inner.write(|conn| {
                runs::set_progress(conn, self.run_id, total, &rc)?;
                Ok(())
            });
        }
        self.emit(&st, false);
        if st
            .last_emit
            .is_none_or(|t| t.elapsed() >= Duration::from_millis(200))
        {
            st.last_emit = Some(Instant::now());
        }
    }
}

fn run_entry<D: HostDeps>(inner: &Arc<Inner<D>>, entry: &QueueEntry, proof: GateOk) -> Attempt {
    let now = inner.deps.now_ms();
    let aid = proof.automation_id().to_string();
    // Not while this process is closing or has lost the host role: the proof (and its lease) is
    // dropped and the entry stays where it is for the next host.
    {
        let state = inner.lock();
        if state.shutdown || !state.hosting {
            return Attempt::Kept;
        }
    }
    // The run row, the lease's run id, the queue entry's removal and the start event are one
    // transaction. An entry that was withdrawn while the gate worked (paused, edited, deleted)
    // starts nothing; a lease that is not ours any more starts nothing either.
    let started = inner.write(|conn| {
        if !queue::remove(conn, entry.id)? {
            return Ok(None);
        }
        let run_id = runs::start(
            conn,
            &NewRun {
                automation_id: Some(&aid),
                automation_name: Some(proof.automation_name()),
                profile_id: proof.profile_id(),
                profile_revision: proof.profile_revision(),
                policy_hash: Some(proof.policy_hash()),
                host: inner.run_host(),
                trigger_kind: entry.kind.as_str(),
                trigger_text: &entry.text,
                merged_count: entry.merged_count,
                retry_of: entry.retry_of,
                started_ms: now,
            },
        )?;
        events::add_event_for_run(
            conn,
            &aid,
            now,
            EventKind::Info,
            &format!("Run #{run_id} started \u{b7} {}", entry.text),
            Some(run_id),
        )?;
        // The folders are there again: leave the Device state.
        if runtime::get(conn, &aid)?.is_some_and(|r| r.trigger_state == Some(TriggerState::Device))
        {
            let back = match proof.policy().trigger {
                Trigger::Schedule(_) => TriggerState::Scheduled,
                Trigger::Watch(_) => TriggerState::Watching,
            };
            runtime::set_trigger_state(conn, &aid, back, None, None, None, now)?;
        }
        if let Some(lease) = proof.lease()
            && !lease.attach_run_in(conn, run_id)?
        {
            return Err(StoreError::Unavailable);
        }
        Ok(Some(run_id))
    });
    forget_entry(inner, entry.id);
    let run_id = match started {
        Ok(Some(id)) => id,
        // Withdrawn meanwhile, or the store failed: nothing started, the lease goes with `proof`.
        Ok(None) => return Attempt::Gone,
        Err(_) => {
            inner
                .lock()
                .next_try
                .insert(entry.id, now.saturating_add(RETRY_GAP_MS));
            return Attempt::Kept;
        }
    };
    let cancel = CancelToken::default();
    {
        // Registered under the same lock `shutdown` uses: a shutdown that came in meanwhile
        // finds the run and stops it (outcome Interrupted), and one that comes later too.
        let mut state = inner.lock();
        let stop = state.shutdown.then_some(StopKind::Shutdown);
        if stop.is_some() {
            cancel.cancel();
        }
        state.active.insert(
            run_id,
            Active {
                automation_id: aid.clone(),
                cancel: cancel.clone(),
                stop,
            },
        );
    }
    let sink = RunSink {
        inner,
        run_id,
        automation_id: aid.clone(),
        state: Mutex::new(SinkState::default()),
    };
    let call = EngineCall {
        run_id,
        retries: proof.policy().retries.min(3),
        dry_run: false,
        incremental: inner.deps.incremental(),
        cancel,
        sink: &sink,
    };
    // A panic in the engine must not leave the row `Running` or the lease held.
    let report =
        std::panic::catch_unwind(AssertUnwindSafe(|| inner.deps.run_engine(&proof, &call)))
            .unwrap_or_else(|_| internal_failure());
    let stop = inner.lock().active.get(&run_id).and_then(|a| a.stop);
    let end = inner.deps.now_ms();
    // The row is final BEFORE the lease is released: the folders stay protected until the
    // history says what happened.
    let _ = settle(inner, entry, &proof, run_id, &report, stop, end);
    inner.lock().active.remove(&run_id);
    drop(proof);
    Attempt::Ran
}

fn internal_failure() -> EngineReport {
    EngineReport {
        outcome: EngineOutcome::Failed {
            error: EngineError {
                code: "internal",
                text: "The run stopped unexpectedly. Check the folders, then Recheck.".to_string(),
            },
        },
        ops: Vec::new(),
        pending_rerun: false,
        settle: SettleState::NotAttempted,
        fingerprint: None,
        incremental: None,
        blockers: Vec::new(),
    }
}

fn plural(n: i64, one: &str, many: &str) -> String {
    if n == 1 {
        format!("{n} {one}")
    } else {
        format!("{n} {many}")
    }
}

fn kept_files(counts: &RunCounts) -> String {
    plural(counts.ok, "completed file stays", "completed files stay")
}

/// What a finished engine report means for the history row.
struct Summary {
    outcome: Outcome,
    counts: RunCounts,
    ops_total: i64,
    error_text: Option<String>,
    note: Option<String>,
    /// A root disappeared: the automation waits for the device instead of asking for attention.
    offline: bool,
    lease_lost: bool,
}

fn summarize(report: &EngineReport, trigger: TriggerKind, stop: Option<StopKind>) -> Summary {
    let nothing_is_ok = matches!(
        trigger,
        TriggerKind::Retry | TriggerKind::PendingRerun | TriggerKind::CatchUp
    );
    let mk = |outcome, counts: &Counts, error: Option<&EngineError>| Summary {
        outcome,
        counts: counts_of(counts),
        ops_total: i64::from(counts.total),
        error_text: error.map(|e| e.text.clone()),
        note: None,
        offline: false,
        lease_lost: false,
    };
    let mut s = match &report.outcome {
        EngineOutcome::Completed { counts } => {
            let mut s = mk(Outcome::Completed, counts, None);
            if counts.total == 0 {
                s.note = Some("Already in sync \u{b7} 0 operations".to_string());
            }
            s
        }
        EngineOutcome::Partial { counts, error } => mk(Outcome::Partial, counts, Some(error)),
        EngineOutcome::ConflictHeld { counts } => mk(Outcome::Conflict, counts, None),
        EngineOutcome::Failed { error } => {
            let mut s = mk(Outcome::Failed, &Counts::default(), Some(error));
            s.counts.not_started = 0;
            s
        }
        EngineOutcome::Cancelled { counts } => mk(Outcome::Cancelled, counts, None),
        EngineOutcome::Interrupted { counts, why } => {
            let mut s = mk(Outcome::Interrupted, counts, None);
            match why {
                InterruptReason::RootOffline => {
                    s.offline = true;
                    s.note = Some(
                        "A folder disconnected during the run \u{2014} completed files stay"
                            .to_string(),
                    );
                }
                InterruptReason::LeaseLost => {
                    s.lease_lost = true;
                    s.note = Some(
                        "The folders were taken by another sync during the run \u{2014} completed files stay"
                            .to_string(),
                    );
                }
            }
            s
        }
        EngineOutcome::NothingToDo => {
            if nothing_is_ok {
                let mut s = mk(Outcome::Skipped, &Counts::default(), None);
                s.note = Some("Fresh comparison found nothing to do".to_string());
                s
            } else {
                let mut s = mk(Outcome::Completed, &Counts::default(), None);
                s.note = Some("Already in sync \u{b7} 0 operations".to_string());
                s
            }
        }
        EngineOutcome::DryRun { .. } => {
            // A real run never ends as a dry run; if it does, say so instead of "completed".
            let mut s = mk(Outcome::Failed, &Counts::default(), None);
            s.error_text = Some("The run did not execute".to_string());
            s
        }
    };
    match (stop, s.outcome) {
        (Some(StopKind::User), Outcome::Cancelled) => {
            s.note = Some(format!(
                "Stopped by you. {} as synced \u{2014} no automatic rollback.",
                kept_files(&s.counts)
            ));
        }
        (Some(StopKind::PauseStop), Outcome::Cancelled) => {
            s.note = Some(format!(
                "Stopped because you paused the automation. {} as synced \u{2014} no automatic rollback.",
                kept_files(&s.counts)
            ));
        }
        (Some(StopKind::Deleted), Outcome::Cancelled) => {
            s.note = Some(format!(
                "Stopped because the automation was deleted. {} as synced.",
                kept_files(&s.counts)
            ));
        }
        (Some(StopKind::Shutdown), Outcome::Cancelled) => {
            s.outcome = Outcome::Interrupted;
            s.note = Some(format!(
                "Stopped because Ferrum was updated or closed. {} as synced.",
                kept_files(&s.counts)
            ));
        }
        _ => {}
    }
    s
}

fn op_kind(action: SyncAction, policy: &ProfilePolicy) -> Option<(OpKind, OpSide)> {
    let delete = if matches!(policy.delete_policy, DeletePolicy::Versioning { .. }) {
        OpKind::DeleteToVersions
    } else {
        OpKind::Delete
    };
    Some(match action {
        SyncAction::CopyToRight => (OpKind::CopyNew, OpSide::Right),
        SyncAction::CopyToLeft => (OpKind::CopyNew, OpSide::Left),
        SyncAction::UpdateRight => (OpKind::Update, OpSide::Right),
        SyncAction::UpdateLeft => (OpKind::Update, OpSide::Left),
        SyncAction::DeleteRight | SyncAction::RemoveEmptyDirRight => (delete, OpSide::Right),
        SyncAction::DeleteLeft | SyncAction::RemoveEmptyDirLeft => (delete, OpSide::Left),
        SyncAction::Conflict => (OpKind::Conflict, OpSide::Right),
        SyncAction::CreateDirRight => (OpKind::CreateFolder, OpSide::Right),
        SyncAction::CreateDirLeft => (OpKind::CreateFolder, OpSide::Left),
        SyncAction::Skip => return None,
    })
}

fn op_status(status: EngineOpStatus) -> runs::OpStatus {
    match status {
        EngineOpStatus::NotStarted => runs::OpStatus::NotStarted,
        EngineOpStatus::Running => runs::OpStatus::InProgress,
        EngineOpStatus::Done | EngineOpStatus::Skipped => runs::OpStatus::Done,
        EngineOpStatus::Changed => runs::OpStatus::Changed,
        EngineOpStatus::Failed => runs::OpStatus::Failed,
        EngineOpStatus::Held => runs::OpStatus::Held,
    }
}

fn outcome_event(run_id: i64, s: &Summary) -> (EventKind, String) {
    let ops = plural(s.ops_total, "operation", "operations");
    match s.outcome {
        Outcome::Completed => (
            EventKind::Ok,
            format!("Run #{run_id} completed \u{b7} {ops}"),
        ),
        Outcome::Partial => (
            EventKind::Warn,
            format!(
                "Run #{run_id} partly done \u{b7} {} failed \u{b7} {} not started",
                s.counts.failed, s.counts.not_started
            ),
        ),
        Outcome::Conflict => (
            EventKind::Warn,
            format!(
                "Run #{run_id} held {} for review",
                plural(s.counts.held, "conflict", "conflicts")
            ),
        ),
        Outcome::Failed => (
            EventKind::Bad,
            format!(
                "Run #{run_id} failed{}",
                s.error_text
                    .as_deref()
                    .map(|t| format!(": {t}"))
                    .unwrap_or_default()
            ),
        ),
        Outcome::Cancelled => (
            EventKind::Mute,
            format!("Run #{run_id} stopped \u{b7} {}", kept_files(&s.counts)),
        ),
        Outcome::Interrupted => (
            EventKind::Warn,
            format!(
                "Run #{run_id} interrupted{}",
                s.note
                    .as_deref()
                    .map(|t| format!(": {t}"))
                    .unwrap_or_default()
            ),
        ),
        Outcome::Skipped => (
            EventKind::Mute,
            format!("Run #{run_id} skipped \u{b7} fresh comparison found nothing to do"),
        ),
    }
}

/// Write the end of a run: outcome, operations, attention, resolution of a retried run, events,
/// follow-up entries, and the answer to Stop controls - all in ONE transaction.
fn settle<D: HostDeps>(
    inner: &Arc<Inner<D>>,
    entry: &QueueEntry,
    proof: &GateOk,
    run_id: i64,
    report: &EngineReport,
    stop: Option<StopKind>,
    now: i64,
) -> Result<(), StoreError> {
    let summary = summarize(report, entry.kind, stop);
    let aid = proof.automation_id().to_string();
    let mapped: Vec<(usize, NewOp<'_>)> = report
        .ops
        .iter()
        .enumerate()
        .filter_map(|(i, op)| {
            let (kind, side) = op_kind(op.action, proof.profile())?;
            Some((
                i,
                NewOp {
                    kind,
                    side,
                    rel_path: op.name.as_str(),
                    size: None,
                    status: op_status(op.status),
                },
            ))
        })
        .collect();
    let new_ops: Vec<NewOp<'_>> = mapped.iter().map(|(_, o)| o.clone()).collect();
    let result = inner.write(|conn| {
        let attention = runs::finish(
            conn,
            run_id,
            &Finish {
                ended_ms: now,
                outcome: summary.outcome,
                ops_total: summary.ops_total,
                counts: summary.counts,
                error_text: summary.error_text.as_deref(),
                note: summary.note.as_deref(),
            },
        )?;
        let added = runs::add_ops(conn, run_id, &new_ops)?;
        for (seq, (idx, op)) in mapped.iter().enumerate().take(added.stored) {
            if op.status == runs::OpStatus::Failed
                && let Some(reason) = report.ops[*idx].reason
            {
                runs::set_op_status(
                    conn,
                    run_id,
                    seq as i64,
                    runs::OpStatus::Failed,
                    Some(reason.code()),
                    Some(reason.text()),
                )?;
            }
        }
        let (kind, text) = outcome_event(run_id, &summary);
        events::add_event_for_run(conn, &aid, now, kind, &text, Some(run_id))?;

        // RQ-10.4: a successful retry resolves the run it continued.
        if let RunMode::Retry { of } = proof.mode()
            && matches!(summary.outcome, Outcome::Completed | Outcome::Skipped)
        {
            let text = format!("Resolved by run #{run_id}");
            runs::resolve(conn, of, Some(run_id), &text, now)?;
            events::add_event_for_run(conn, &aid, now, EventKind::Ok, &text, Some(run_id))?;
        }

        // The Stop controls of this run are answered now that it has ended.
        for control in controls::pending_for(conn, &aid, &[run_id], now)? {
            if matches!(control.action, Action::Stop | Action::PauseStop) {
                controls::mark_handled(conn, control.id, "stopped", now)?;
            }
        }

        // Follow-ups. None while the automation needs attention, is paused or was deleted, or
        // when the user or Ferrum stopped the run.
        let still = automations::get(conn, &aid)?;
        let wanted = still.as_ref().is_some_and(|a| a.enabled)
            && attention.is_none()
            && stop.is_none()
            && matches!(
                summary.outcome,
                Outcome::Completed | Outcome::Skipped | Outcome::Interrupted
            );
        let mut rerun = report.pending_rerun || summary.lease_lost;
        if summary.offline && wanted {
            runtime::set_trigger_state(conn, &aid, TriggerState::Device, None, None, None, now)?;
            if matches!(proof.policy().trigger, Trigger::Watch(_)) {
                // The watch hook looks at the folders again when they are back (WA-5.2) and
                // compares before it queues anything: no entry that would run unasked.
            } else if inner.unattended {
                queue::enqueue(
                    conn,
                    &NewEntry {
                        automation_id: &aid,
                        profile_id: proof.profile_id(),
                        host_class: inner.class,
                        kind: TriggerKind::Available,
                        text: "Folder is back",
                        retry_of: None,
                    },
                    now,
                )?;
            } else {
                events::add_event(
                    conn,
                    &aid,
                    now,
                    EventKind::Info,
                    "Waiting for the folder \u{2014} run it with Run now when it is back",
                )?;
            }
            rerun = false;
        }
        if rerun && wanted && !inner.unattended {
            events::add_event(
                conn,
                &aid,
                now,
                EventKind::Info,
                "Files changed during the run \u{2014} run it again with Run now",
            )?;
        } else if rerun && wanted {
            let streak = {
                let mut state = inner.lock();
                let n = state.rerun_streak.entry(aid.clone()).or_insert(0);
                *n += 1;
                *n
            };
            if streak > MAX_RERUN_STREAK {
                inner.lock().rerun_streak.remove(&aid);
                events::add_event(
                    conn,
                    &aid,
                    now,
                    EventKind::Warn,
                    "Files keep changing during the runs \u{2014} the next run waits for the next trigger",
                )?;
            } else {
                queue::enqueue(
                    conn,
                    &NewEntry {
                        automation_id: &aid,
                        profile_id: proof.profile_id(),
                        host_class: inner.class,
                        kind: TriggerKind::PendingRerun,
                        text: "Files changed during the run",
                        retry_of: None,
                    },
                    now,
                )?;
            }
        } else if !report.pending_rerun {
            inner.lock().rerun_streak.remove(&aid);
        }
        Ok(())
    });
    // Last progress snapshot so a listening view shows the end state.
    inner.deps.emit(&HostEvent::Progress(ProgressEvent {
        run_id,
        automation_id: aid,
        phase: Phase::Done.as_str(),
        ops_total: summary.ops_total,
        counts: summary.counts,
    }));
    result
}

// ── Orphans (LC-6) ──

/// A `Running` row whose host died becomes `Interrupted`. No attention (RQ-9.2): the next
/// trigger decides with a fresh comparison.
pub(super) fn interrupt_orphan(
    conn: &Connection,
    run: &RunRecord,
    now: i64,
) -> Result<(), StoreError> {
    let note = "Interrupted: Ferrum closed or the PC stopped during this run \u{2014} completed files stay";
    runs::finish(
        conn,
        run.id,
        &Finish {
            ended_ms: now,
            outcome: Outcome::Interrupted,
            ops_total: run.ops_total,
            counts: run.counts,
            error_text: None,
            note: Some(note),
        },
    )?;
    if let Some(aid) = &run.automation_id {
        events::add_event_for_run(
            conn,
            aid,
            now,
            EventKind::Warn,
            &format!("Run #{} was interrupted: {note}", run.id),
            Some(run.id),
        )?;
    }
    Ok(())
}

// ── Recheck (RQ-10.1, RQ-10.3, RQ-10.5) ──

/// A fresh, read-only comparison for the run an attention hold names. The result is stored on the
/// run (`recheck`); a clean answer resolves the hold. The gate applies as for any other run
/// (`GateKind::Recheck`): the approval must verify, even though no file is changed.
pub(super) fn recheck<D: HostDeps>(inner: &Arc<Inner<D>>, automation_id: &str, run_id: i64) {
    let now = inner.deps.now_ms();
    let env = inner.deps.engine_env();
    let unmet = |c: &crate::automation::model::Conditions| inner.deps.unmet_condition(c);
    let ctx = GateCtx {
        store: &inner.store,
        keys: inner.deps.keys(),
        class: inner.class,
        lease_env: inner.deps.lease_env(),
        env: &env,
        unmet_condition: &unmet,
    };
    let proof = match gate(&ctx, automation_id, GateKind::Recheck { run_id }) {
        Ok(p) => p,
        Err(refusal) => {
            let text = match refusal {
                GateRefusal::NeedsReview(r) => {
                    format!(
                        "Recheck not done: needs review \u{2014} {}",
                        review_label(&r)
                    )
                }
                GateRefusal::Held => {
                    "Recheck not done: this run is not the one that needs attention".to_string()
                }
                GateRefusal::Device(e) | GateRefusal::Blocked(e) => {
                    format!("Recheck not done: {}", gate_error_text(e))
                }
                other => format!("Recheck not done ({})", other.code()),
            };
            let _ = inner.write(|conn| {
                events::add_event_for_run(
                    conn,
                    automation_id,
                    now,
                    EventKind::Warn,
                    &text,
                    Some(run_id),
                )
            });
            return;
        }
    };
    // The original run, for its outcome (a conflict is rechecked differently).
    let original: Option<RunRecord> = inner
        .store
        .with_read(|conn| runs::get(conn, run_id))
        .ok()
        .flatten();
    let was_conflict = original
        .as_ref()
        .is_some_and(|r| r.outcome == Some(Outcome::Conflict));
    let _ = inner.write(|conn| {
        runs::set_recheck(
            conn,
            run_id,
            Some(&RecheckRecord {
                state: RecheckState::Running,
                remaining: 0,
                text: "Rechecking\u{2026}".to_string(),
                at_ms: now,
            }),
        )?;
        Ok(())
    });
    let call = EngineCall {
        run_id,
        retries: 0,
        dry_run: true,
        incremental: inner.deps.incremental(),
        cancel: CancelToken::default(),
        sink: &crate::sync_engine::NoSink,
    };
    let report =
        std::panic::catch_unwind(AssertUnwindSafe(|| inner.deps.run_engine(&proof, &call)))
            .unwrap_or_else(|_| internal_failure());
    let end = inner.deps.now_ms();
    let outcome = match &report.outcome {
        EngineOutcome::DryRun {
            plan,
            conflicts,
            guard,
        } => {
            let remaining = (plan.to_right + plan.to_left + plan.deletes) as i64;
            Some((remaining, i64::from(*conflicts), *guard))
        }
        _ => None,
    };
    let _ = inner.write(|conn| {
        let Some((remaining, conflicts, guard)) = outcome else {
            // The comparison could not be trusted (cap, errors, a folder offline): the state
            // is cleared so the row does not claim to be rechecking, and one event says why.
            runs::set_recheck(conn, run_id, None)?;
            events::add_event_for_run(
                conn,
                automation_id,
                end,
                EventKind::Warn,
                "Recheck could not complete: the folders could not be compared reliably",
                Some(run_id),
            )?;
            return Ok(());
        };
        let earlier = original
            .as_ref()
            .map(|r| (r.ops_total - remaining).max(0))
            .unwrap_or(0);
        let (state, count, text) = if was_conflict && conflicts > 0 {
            (
                RecheckState::Conflict,
                conflicts,
                "The file still changed on both sides. Decide in Sync \u{2014} Ferrum won't choose a version."
                    .to_string(),
            )
        } else if remaining == 0 && conflicts == 0 {
            let text = if was_conflict {
                "Resolved in Sync by you".to_string()
            } else {
                "Resolved \u{2014} folders are in sync".to_string()
            };
            (RecheckState::InSync, 0, text)
        } else if conflicts > 0 {
            (
                RecheckState::Conflict,
                conflicts,
                format!(
                    "{} held for review. Decide in Sync \u{2014} Ferrum won't choose a version.",
                    plural(conflicts, "conflict", "conflicts")
                ),
            )
        } else {
            (
                RecheckState::Remaining,
                remaining,
                format!(
                    "Fresh comparison: {} in sync and won't be repeated \u{b7} {} remaining.",
                    plural(earlier, "earlier operation is", "earlier operations are"),
                    plural(remaining, "operation", "operations"),
                ),
            )
        };
        runs::set_recheck(
            conn,
            run_id,
            Some(&RecheckRecord {
                state,
                remaining: count,
                text: text.clone(),
                at_ms: end,
            }),
        )?;
        if state == RecheckState::InSync {
            // RQ-10.3 / RQ-10.5: a clean recheck releases the hold.
            runs::resolve(conn, run_id, None, &text, end)?;
        }
        let kind = if state == RecheckState::InSync {
            EventKind::Ok
        } else {
            EventKind::Info
        };
        events::add_event_for_run(
            conn,
            automation_id,
            end,
            kind,
            &format!("Recheck of run #{run_id}: {text}"),
            Some(run_id),
        )?;
        if matches!(guard, GuardVerdict::Tripped { .. }) && state == RecheckState::Remaining {
            events::add_event_for_run(
                conn,
                automation_id,
                end,
                EventKind::Warn,
                "A retry would delete more than the safety limit allows \u{2014} review in Sync",
                Some(run_id),
            )?;
        }
        Ok(())
    });
    drop(proof);
}
