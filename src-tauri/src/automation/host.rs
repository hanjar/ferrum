//! The host (Automation design §3, §9.4, §10.4; FER-524 / P1.3): the loop that elects one process
//! to run automations, takes controls and queue entries, passes every run through the gate, calls
//! the engine, and records history, attention holds and events.
//!
//! One implementation serves both classes (`Host<D: HostDeps>`): the in-app host
//! ([`HostClass::App`]) wired up by `lib.rs`, and the background runner ([`HostClass::Background`],
//! P3.1 only constructs it). A host of one class executes ONLY automations of that class.
//!
//! ```text
//! tick():  poll change_seq → (re)claim the host role → recover orphan runs (once per takeover)
//!          → take controls → reconcile active runs → start the next queue entry
//! ```
//!
//! What this file guarantees (see also `gate.rs`, `runner.rs`):
//!
//! - **Election (RT-4).** One host row per kind in `hosts`, claimed in one immediate transaction;
//!   a live row (beat ≤ 15 s AND process alive) is never displaced; a dead or silent host is
//!   replaced; a host that finds its row taken stops starting runs.
//! - **Every run goes through the gate.** Triggers, Run now, retries, catch-ups and pending
//!   reruns are queue entries; a recheck is a request. Both reach the engine only through
//!   `runner::run_gated`, which needs a `GateOk`.
//! - **Controls are requests.** The host reads them from the store, answers them (`held`,
//!   `queued`, ...) and applies the same gate. The webview has no parameter that skips it.
//! - **Crash recovery (LC-6).** When a host takes the role, a `Running` row of its class whose
//!   lease is gone becomes `Interrupted` (no attention) and dead leases are deleted.
//! - **One run at a time per host.** Simple and gentle on the machine; folders of different
//!   profiles do not wait for each other across hosts (the lease decides that).
//! - **No store lock held across slow work.** The store mutex is never held while the engine runs,
//!   folders are probed or a lease is taken; the engine is never called from inside a store
//!   closure (`store::inside_closure()` is checked on the way in).
//!
//! Tauri-free: the runner links this module. Events reach the webview through
//! [`HostDeps::emit`], which `commands.rs` wires to the app handle.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use rusqlite::{Connection, TransactionBehavior};
use serde::Serialize;

use crate::automation::dpapi::ApprovalKeys;
use crate::automation::lease::LeaseEnv;
use crate::automation::model::Conditions;
use crate::automation::store::controls::{self, Action, Control};
use crate::automation::store::hosts::{self, Claim, HostRow};
use crate::automation::store::leases::{self, Holder};
use crate::automation::store::queue::{self, HostClass, QueueEntry};
use crate::automation::store::runs::{self, Outcome, RunHost, RunRecord};
use crate::automation::store::{Store, StoreError, automations};
use crate::ops::CancelToken;
use crate::sync_engine::env::EngineEnv;
use crate::sync_engine::execute::{EngineReport, EngineSink};

pub mod engine;
pub mod gate;
pub mod prod;
pub mod request;
mod requeue;
mod runner;
mod schedule;

pub use gate::{GateCtx, GateKind, GateOk, GateRefusal, RunMode, gate};
pub use requeue::{requeue_siblings, requeue_siblings_if};
pub use schedule::Recompute;

#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_flows;
#[cfg(test)]
mod tests_schedule;

/// The host refreshes its row this often (design §3.3: every 5 s).
pub const BEAT_EVERY_MS: i64 = 5_000;
/// **Automatic runs are OFF.** While this is `false` the engine runs only when the user asks at
/// that moment (Run now, Retry). Everything that would start it on its own is parked: pending
/// reruns, the fresh comparison after a folder came back (`Available`), entries of triggers
/// (schedule, watch, catch-up, startup), entries the gate kept waiting (device, conditions,
/// busy folders) that would run later by themselves, and the move of merged triggers to a
/// sibling. A parked entry stays in the queue and says "Waiting - run it with Run now".
///
/// AUTO-P0.2c (P0.2 review P3-2 and P3-3: re-check the target at the commit of an in-place
/// overwrite; restore the old target when a versioning overwrite fails or is cancelled) is
/// REQUIRED before this is switched on.
pub const UNATTENDED_RUNS_ENABLED: bool = false;

#[cfg(test)]
thread_local! {
    static UNATTENDED_OVERRIDE: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// The switch as the calling code sees it. Tests may set it per thread
/// ([`set_unattended_for_tests`]); production reads the constant.
pub fn unattended_enabled() -> bool {
    #[cfg(test)]
    if let Some(v) = UNATTENDED_OVERRIDE.with(|c| c.get()) {
        return v;
    }
    UNATTENDED_RUNS_ENABLED
}

/// Per-thread override for tests: set it on the test thread BEFORE the host is created.
#[cfg(test)]
pub(crate) fn set_unattended_for_tests(on: bool) {
    UNATTENDED_OVERRIDE.with(|c| c.set(Some(on)));
}

/// Running rows are re-examined for a dead host this often (LC-6).
const RECOVERY_EVERY_MS: i64 = 30_000;
/// A run younger than this is never called an orphan (its worker may be about to register it).
const RECOVERY_GRACE_MS: i64 = 30_000;
/// A holder that is still alive keeps its run this long after its last beat.
const LIVE_HOLDER_GRACE_MS: i64 = 5 * 60 * 1000;
/// A process that does not host asks for the role this often.
pub const CLAIM_RETRY_MS: i64 = 1_000;
/// The host looks at `meta.change_seq` this often (MH-10: views follow within 2 s).
pub const POLL_EVERY_MS: i64 = 2_000;
/// A queue entry that the gate kept waiting is tried again after this long.
pub const RETRY_GAP_MS: i64 = 5_000;
/// A pending rerun (a file changed during the run) waits this long before it starts.
pub const RERUN_DELAY_MS: i64 = 10_000;
/// After this many pending reruns in a row for one automation the host stops chaining them and
/// says so (a folder that changes all the time would otherwise run for ever).
pub const MAX_RERUN_STREAK: u32 = 5;
/// Controls are swept (deleted when expired) this often.
const SWEEP_EVERY_MS: i64 = 30_000;
/// Rechecks (read-only comparisons) that may run at the same time.
const MAX_RECHECKS: usize = 1;

/// One immediate transaction on the store that also bumps `change_seq`. The closure runs inside
/// the store mutex: it must not call the engine or anything that opens the store again (see
/// `Store::with_read`).
pub(crate) fn write_tx<T>(
    store: &Store,
    f: impl FnOnce(&Connection) -> Result<T, StoreError>,
) -> Result<T, StoreError> {
    store.with_write(|conn| -> Result<T, StoreError> {
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| StoreError::Unavailable)?;
        let out = f(&tx)?;
        automations::bump_change_seq(&tx)?;
        tx.commit().map_err(|_| StoreError::Unavailable)?;
        Ok(out)
    })
}

// ── Events ──

/// What the host tells the shell. Carries ids, phase names and counts - never a path, a MAC, a
/// key or a lease id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum HostEvent {
    /// `meta.change_seq` moved (any process wrote): re-read.
    Changed { seq: i64 },
    /// A run's phase or counts changed (≤ 5 per second per run).
    Progress(ProgressEvent),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressEvent {
    pub run_id: i64,
    pub automation_id: String,
    /// `gate | compare | plan | sync | settle | done` (the run's phase column).
    pub phase: &'static str,
    pub ops_total: i64,
    pub counts: runs::RunCounts,
}

// ── Dependencies ──

/// What one engine call needs besides the proof of the gate.
pub struct EngineCall<'a> {
    pub run_id: i64,
    pub retries: u8,
    /// A read-only comparison (recheck): no operation, no lease.
    pub dry_run: bool,
    pub incremental: bool,
    pub cancel: CancelToken,
    pub sink: &'a dyn EngineSink,
}

/// Everything that makes the host depend on the outside world (design §13.1).
pub trait HostDeps: Send + Sync + 'static {
    /// Unix epoch milliseconds.
    fn now_ms(&self) -> i64;
    fn keys(&self) -> &dyn ApprovalKeys;
    /// Clock, process probe and holder identity of this host (also used for its leases).
    fn lease_env(&self) -> &LeaseEnv;
    /// A fresh engine environment (probes, copier, Recycle Bin, ...).
    fn engine_env(&self) -> EngineEnv;
    /// Run the engine. The `GateOk` is the proof that the gate passed; it is the only way to get
    /// here. Implementations call `engine::run_gated` (production) or script a result (tests).
    fn run_engine(&self, proof: &GateOk, call: &EngineCall<'_>) -> EngineReport;
    /// `Some(reason)` while an optional condition (OP-1) is unmet.
    fn unmet_condition(&self, conditions: &Conditions) -> Option<&'static str>;
    /// Run `job` on another thread. `false` = it could not be started.
    fn spawn(&self, name: &'static str, job: Box<dyn FnOnce() + Send>) -> bool;
    fn emit(&self, event: &HostEvent);
    /// The Settings flag "use the indexer journal" (IX-1).
    fn incremental(&self) -> bool;
}

// ── State ──

/// Why a run is being stopped; decides its outcome and note.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StopKind {
    /// The user pressed Stop (RQ-8.1).
    User,
    /// "Pause & stop current run" (RQ-8.2).
    PauseStop,
    /// Ferrum is closing (LC-2): the run ends Interrupted.
    Shutdown,
    /// The automation was deleted.
    Deleted,
}

pub(crate) struct Active {
    pub automation_id: String,
    pub cancel: CancelToken,
    pub stop: Option<StopKind>,
}

#[derive(Default)]
pub(crate) struct State {
    /// This process holds the host role.
    pub hosting: bool,
    pub last_claim_ms: Option<i64>,
    pub last_poll_ms: Option<i64>,
    pub last_sweep_ms: Option<i64>,
    pub last_seq: Option<i64>,
    /// The role was just taken: orphan runs must be recovered before anything starts.
    pub needs_recovery: bool,
    /// A worker owns the single run slot (gating, running, recording).
    pub busy: bool,
    pub rechecking: usize,
    pub active: HashMap<i64, Active>,
    /// Controls already applied but kept visible until their run ends ("Stopping...").
    pub applied_controls: HashSet<i64>,
    /// Queue entry id → earliest time the gate may be asked again.
    pub next_try: HashMap<i64, i64>,
    /// Queue entry id → refusal code already logged for it.
    pub logged_refusal: HashMap<i64, &'static str>,
    /// Automation id → pending reruns chained in a row.
    pub rerun_streak: HashMap<String, u32>,
    /// Entries already announced as parked ("Waiting - run it with Run now").
    pub parked_logged: HashMap<i64, i64>,
    /// The schedule hook's bookkeeping (`schedule.rs`).
    pub sched: schedule::Book,
    pub last_recovery_ms: Option<i64>,
    pub shutdown: bool,
}

pub(crate) struct Inner<D: HostDeps> {
    pub store: Arc<Store>,
    pub deps: D,
    pub class: HostClass,
    pub version: String,
    pub holder: Holder,
    /// [`unattended_enabled`] when the host was made.
    pub unattended: bool,
    pub state: Mutex<State>,
    pub wake: (Mutex<bool>, Condvar),
    pub stopped: AtomicBool,
}

impl<D: HostDeps> Inner<D> {
    pub(crate) fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub(crate) fn kind(&self) -> &'static str {
        match self.class {
            HostClass::App => hosts::KIND_APP,
            HostClass::Background => hosts::KIND_RUNNER,
        }
    }

    pub(crate) fn run_host(&self) -> RunHost {
        match self.class {
            HostClass::App => RunHost::App,
            HostClass::Background => RunHost::Background,
        }
    }

    /// One immediate transaction that also bumps `change_seq`, so the other processes (and this
    /// one's poller) notice. The closure runs inside the store mutex: it must not call the engine
    /// or anything that opens the store (see `Store::with_read`).
    pub(crate) fn write<T>(
        &self,
        f: impl FnOnce(&Connection) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        write_tx(&self.store, f)
    }

    pub(crate) fn nudge(&self) {
        let (flag, cv) = &self.wake;
        *flag.lock().unwrap_or_else(|p| p.into_inner()) = true;
        cv.notify_all();
    }
}

/// The host. Cheap to clone (a handle); the loop thread and the run workers share the state.
pub struct Host<D: HostDeps> {
    inner: Arc<Inner<D>>,
}

impl<D: HostDeps> Clone for Host<D> {
    fn clone(&self) -> Self {
        Host {
            inner: self.inner.clone(),
        }
    }
}

impl<D: HostDeps> Host<D> {
    pub fn new(store: Arc<Store>, deps: D, class: HostClass, version: &str) -> Self {
        let holder = deps.lease_env().holder;
        Host {
            inner: Arc::new(Inner {
                store,
                deps,
                class,
                version: version.to_string(),
                holder,
                unattended: unattended_enabled(),
                state: Mutex::new(State::default()),
                wake: (Mutex::new(false), Condvar::new()),
                stopped: AtomicBool::new(false),
            }),
        }
    }

    pub fn class(&self) -> HostClass {
        self.inner.class
    }

    /// Whether this process holds the host role (as of the last tick).
    pub fn is_hosting(&self) -> bool {
        self.inner.lock().hosting
    }

    /// No run, no recheck and no worker in flight.
    pub fn is_idle(&self) -> bool {
        let state = self.inner.lock();
        !state.busy && state.active.is_empty() && state.rechecking == 0
    }

    /// Ask the loop to look again now (a command wrote to the store).
    pub fn wake(&self) {
        self.inner.nudge();
    }

    /// The system clock, the time zone or the power state changed (`power.rs`): the schedules are
    /// looked at again at the next tick, and occurrences that passed meanwhile are handled as
    /// missed (SC-3.3). Safe to call from any thread.
    pub fn recompute_schedules(&self, why: Recompute) {
        self.inner.lock().sched.ask(why);
        self.inner.nudge();
    }

    /// Run ids this process is executing right now.
    pub fn active_runs(&self) -> Vec<i64> {
        let mut ids: Vec<i64> = self.inner.lock().active.keys().copied().collect();
        ids.sort_unstable();
        ids
    }

    /// One pass of the loop. Idempotent, never panics on store trouble (it just tries again at
    /// the next tick), and cheap when nothing is queued.
    pub fn tick(&self) {
        let inner = &self.inner;
        let now = inner.deps.now_ms();
        self.poll_changes(now);
        if inner.lock().shutdown {
            return;
        }
        if !self.elect(now) {
            return;
        }
        self.recover_orphans(now);
        self.handle_controls(now);
        self.reconcile_active();
        schedule::pass(inner, now);
        self.start_next(now);
    }

    /// Loop until [`Host::shutdown`]: tick, then wait for a wake-up or one second.
    pub fn run_loop(&self) {
        while !self.inner.stopped.load(Ordering::SeqCst) {
            self.tick();
            let (flag, cv) = &self.inner.wake;
            let guard = flag.lock().unwrap_or_else(|p| p.into_inner());
            let (mut guard, _) = cv
                .wait_timeout_while(guard, Duration::from_secs(1), |woken| {
                    !*woken && !self.inner.stopped.load(Ordering::SeqCst)
                })
                .unwrap_or_else(|p| p.into_inner());
            *guard = false;
        }
    }

    /// Start the loop on its own thread. `None` if the thread could not be created.
    pub fn start(&self) -> Option<std::thread::JoinHandle<()>> {
        let host = self.clone();
        std::thread::Builder::new()
            .name("ferrum-automation-host".into())
            .spawn(move || host.run_loop())
            .ok()
    }

    /// Stop hosting (LC-2): no new run starts, active runs are asked to stop after the current
    /// file (they end `Interrupted`), the loop ends, and - once the runs are done or `wait` has
    /// passed - the host row is released so another instance takes over at once.
    pub fn shutdown(&self, wait: Duration) {
        let inner = &self.inner;
        {
            let mut state = inner.lock();
            state.shutdown = true;
            for active in state.active.values_mut() {
                active.stop = Some(StopKind::Shutdown);
                active.cancel.cancel();
            }
        }
        inner.stopped.store(true, Ordering::SeqCst);
        inner.nudge();
        let deadline = std::time::Instant::now() + wait;
        while !self.is_idle() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = inner.store.with_write(|conn| -> Result<(), StoreError> {
            hosts::release(conn, inner.kind(), inner.holder)?;
            Ok(())
        });
        inner.lock().hosting = false;
    }

    // ── change events (every instance, host or not: MH-10) ──

    fn poll_changes(&self, now: i64) {
        let inner = &self.inner;
        {
            let mut state = inner.lock();
            if state
                .last_poll_ms
                .is_some_and(|t| now.saturating_sub(t) < POLL_EVERY_MS && now >= t)
            {
                return;
            }
            state.last_poll_ms = Some(now);
        }
        let Ok(seq) = inner.store.with_read(automations::change_seq) else {
            return;
        };
        let changed = {
            let mut state = inner.lock();
            let changed = state.last_seq != Some(seq);
            state.last_seq = Some(seq);
            changed
        };
        if changed {
            // An approval, an edit, a pause or a resume: the schedules are read again.
            inner.lock().sched.ask(Recompute::Edited);
            inner.deps.emit(&HostEvent::Changed { seq });
        }
    }

    // ── election ──

    /// Claim or refresh the host role. Returns whether this process hosts now.
    fn elect(&self, now: i64) -> bool {
        let inner = &self.inner;
        {
            let state = inner.lock();
            // The host refreshes its row every 5 s; a process that does not host looks again
            // every second, so a dead host is replaced within a second of its 15 s window.
            let gap = if state.hosting {
                BEAT_EVERY_MS
            } else {
                CLAIM_RETRY_MS
            };
            if state
                .last_claim_ms
                .is_some_and(|t| now.saturating_sub(t) < gap && now >= t)
            {
                return state.hosting;
            }
        }
        let row = HostRow {
            kind: inner.kind().to_string(),
            pid: inner.holder.pid,
            created: inner.holder.created,
            version: inner.version.clone(),
            schema_version: crate::automation::store::SCHEMA_VERSION,
            session_id: i64::from(std::process::id()),
            last_beat_ms: now,
        };
        let probe = inner.deps.lease_env().probe.clone();
        let claim = inner
            .store
            .with_write(|conn| hosts::claim(conn, &row, probe.as_ref()));
        let mut state = inner.lock();
        state.last_claim_ms = Some(now);
        match claim {
            Ok(Claim::Refreshed) => {
                state.hosting = true;
                // Periodic recovery: a run left Running by a host that died while another one
                // already hosts (or whose takeover came too early to see it as dead).
                if state
                    .last_recovery_ms
                    .is_none_or(|t| now.saturating_sub(t) >= RECOVERY_EVERY_MS || now < t)
                {
                    state.needs_recovery = true;
                }
            }
            Ok(Claim::Acquired { .. }) => {
                state.hosting = true;
                state.needs_recovery = true;
                // Whatever came due while no host was here is found now (SC-3.3).
                state.sched.ask(Recompute::Started);
            }
            // Another live process hosts, or the store cannot say (read-only, unavailable):
            // this process does not execute.
            Ok(Claim::Held { .. }) | Err(_) => state.hosting = false,
        }
        state.hosting && !state.shutdown
    }

    // ── crash recovery (LC-6) ──

    /// When this process has just taken the role: every run of this class that is still `Running`
    /// and whose lease is not alive belonged to a host that died. It becomes `Interrupted` (no
    /// attention, RQ-9.2) and dead leases are removed. A run whose lease is still alive (the old
    /// host only stalled) is left alone: the engine's lease keeps protecting its folders.
    fn recover_orphans(&self, now: i64) {
        let inner = &self.inner;
        {
            let mut state = inner.lock();
            if !state.needs_recovery {
                return;
            }
            state.needs_recovery = false;
            state.last_recovery_ms = Some(now);
        }
        let probe = inner.deps.lease_env().probe.clone();
        let mine: Vec<i64> = inner.lock().active.keys().copied().collect();
        let _ = inner.write(|conn| {
            for run in runs::active(conn)? {
                if run.host != inner.run_host() || mine.contains(&run.id) {
                    continue;
                }
                // A run that just started may not be registered yet; a holder that is still alive
                // keeps its run (a stalled host is not a dead one).
                if now >= run.started_ms && now - run.started_ms < RECOVERY_GRACE_MS {
                    continue;
                }
                if leases::held_by_living(conn, run.id, now, probe.as_ref(), LIVE_HOLDER_GRACE_MS)?
                {
                    continue;
                }
                runner::interrupt_orphan(conn, &run, now)?;
            }
            // Only after the runs were judged: a stale row is the evidence that a holder still
            // exists, and deleting it first would erase that.
            // A lease older than the 30 s staleness window is reclaimable by any acquirer (design
            // 3.4) and its stalled holder stops at its next cover check; the recovery pass itself
            // only deletes the rows of dead processes and of holders silent for more than the
            // grace, so the evidence for a run claim survives that long.
            leases::reclaim_dead(conn, now, probe.as_ref(), LIVE_HOLDER_GRACE_MS)?;
            Ok(())
        });
    }

    // ── controls ──

    fn handle_controls(&self, now: i64) {
        let inner = &self.inner;
        let sweep = {
            let mut state = inner.lock();
            let due = state
                .last_sweep_ms
                .is_none_or(|t| now.saturating_sub(t) >= SWEEP_EVERY_MS || now < t);
            if due {
                state.last_sweep_ms = Some(now);
            }
            due
        };
        if sweep {
            // A Recheck or Retry nobody answered in time is announced before it is swept away.
            let _ = inner.write(|conn| {
                for c in controls::expired_unhandled(conn, now)? {
                    if !matches!(c.action, Action::Recheck | Action::Retry) {
                        continue;
                    }
                    let Some(run_id) = c.run_id else { continue };
                    let Some(run) = runs::get(conn, run_id)? else {
                        continue;
                    };
                    if let Some(aid) = &run.automation_id {
                        crate::automation::store::events::add_event_for_run(
                            conn,
                            aid,
                            now,
                            crate::automation::store::automations::EventKind::Warn,
                            &format!(
                                "{} of run #{run_id} was not done in time \u{2014} ask again",
                                if c.action == Action::Recheck {
                                    "Recheck"
                                } else {
                                    "Retry"
                                }
                            ),
                            Some(run_id),
                        )?;
                    }
                }
                controls::sweep(conn, now)?;
                Ok(())
            });
        }
        let Ok(pending) = inner
            .store
            .with_read(|conn| -> Result<Vec<Control>, StoreError> { controls::pending(conn, now) })
        else {
            return;
        };
        for control in pending {
            if inner.lock().applied_controls.contains(&control.id) {
                continue;
            }
            self.apply_control(&control, now);
        }
        // Forget applied controls that are no longer pending (handled, swept or expired).
        let live: HashSet<i64> = inner
            .store
            .with_read(|conn| -> Result<Vec<Control>, StoreError> { controls::pending(conn, now) })
            .map(|v| v.into_iter().map(|c| c.id).collect())
            .unwrap_or_default();
        inner.lock().applied_controls.retain(|id| live.contains(id));
    }

    /// The automation a control is about and the host class that owns it; `None` for a control
    /// about nothing that exists any more.
    fn control_target(&self, control: &Control) -> Option<(String, Option<RunRecord>)> {
        let inner = &self.inner;
        inner
            .store
            .with_read(
                |conn| -> Result<Option<(String, Option<RunRecord>)>, StoreError> {
                    if let Some(run_id) = control.run_id {
                        let Some(run) = runs::get(conn, run_id)? else {
                            return Ok(None);
                        };
                        let Some(id) = run.automation_id.clone() else {
                            return Ok(None);
                        };
                        return Ok(Some((id, Some(run))));
                    }
                    Ok(control.automation_id.clone().map(|id| (id, None)))
                },
            )
            .ok()
            .flatten()
    }

    /// Does this host own the automation (its runtime class)? Controls of the other class are
    /// left for that host.
    fn owns(&self, automation_id: &str) -> Option<bool> {
        let inner = &self.inner;
        inner
            .store
            .with_read(|conn| -> Result<Option<bool>, StoreError> {
                Ok(automations::get(conn, automation_id)?.and_then(|rec| {
                    serde_json::from_str::<crate::automation::model::AutomationPolicy>(&rec.policy)
                        .ok()
                        .map(|p| {
                            let class = match p.runtime {
                                crate::automation::model::Runtime::App => HostClass::App,
                                crate::automation::model::Runtime::Background => {
                                    HostClass::Background
                                }
                            };
                            class == inner.class
                        })
                }))
            })
            .ok()
            .flatten()
    }

    fn mark(&self, control: &Control, result: &str, now: i64) {
        let _ = self.inner.write(|conn| {
            controls::mark_handled(conn, control.id, result, now)?;
            Ok(())
        });
    }

    fn apply_control(&self, control: &Control, now: i64) {
        let inner = &self.inner;
        match control.action {
            // Informational: the store was re-read by this very pass.
            Action::Reload => return self.mark(control, "ok", now),
            // Only the background runner restarts itself (P3.1).
            Action::RestartRunner => return,
            _ => {}
        }
        let Some((automation_id, run)) = self.control_target(control) else {
            return self.mark(control, "gone", now);
        };
        // Stop and Recheck belong to the host that RAN the run (`run.host`), whatever the
        // automation's runtime says today; the rest follows the automation's runtime.
        let owned = match (&run, control.action) {
            (Some(r), Action::Stop | Action::Recheck) => Some(match r.host {
                RunHost::App => inner.class == HostClass::App,
                RunHost::Background => inner.class == HostClass::Background,
                RunHost::Manual => false,
            }),
            _ => self.owns(&automation_id),
        };
        match owned {
            None => return self.mark(control, "gone", now),
            Some(false) => return, // the other host answers it
            Some(true) => {}
        }
        match control.action {
            Action::RunNow => {
                let answer =
                    request::run_now_in(&inner.store, inner.deps.keys(), &automation_id, now);
                self.mark(control, &request::answer_text(&answer), now);
            }
            Action::Retry => {
                let answer = match control.run_id {
                    Some(run_id) => request::retry_in(&inner.store, inner.deps.keys(), run_id, now),
                    None => Err(request::RequestError::Invalid),
                };
                self.mark(control, &request::answer_text(&answer), now);
            }
            Action::Stop | Action::PauseStop => {
                let kind = if control.action == Action::Stop {
                    StopKind::User
                } else {
                    StopKind::PauseStop
                };
                let mut state = inner.lock();
                let target = state
                    .active
                    .iter_mut()
                    .find(|(id, a)| {
                        a.automation_id == automation_id && control.run_id.is_none_or(|r| r == **id)
                    })
                    .map(|(_, a)| a);
                match target {
                    Some(active) => {
                        active.stop.get_or_insert(kind);
                        active.cancel.cancel();
                        // Stay pending until the run ends: the status line reads the pending
                        // control as "Stopping after the current file".
                        state.applied_controls.insert(control.id);
                    }
                    None => {
                        drop(state);
                        self.mark(control, "notRunning", now);
                    }
                }
            }
            Action::PauseFinish => self.mark(control, "finish", now),
            Action::Recheck => {
                let Some(run) = run else {
                    return self.mark(control, "gone", now);
                };
                // Not while a run works on the same folders: the comparison would describe a
                // tree in motion. The control stays pending and is tried again.
                let busy = inner
                    .store
                    .with_read(|c| request::folders_in_use(c, &automation_id))
                    .unwrap_or(true);
                if busy || !self.try_begin_recheck(&automation_id, run.id) {
                    // No capacity right now: stays pending, tried again at the next tick.
                    return;
                }
                inner.lock().applied_controls.insert(control.id);
                self.mark(control, "started", now);
            }
            Action::Reload | Action::RestartRunner => {}
        }
    }

    /// Cancel runs whose automation was deleted (their history stays).
    fn reconcile_active(&self) {
        let inner = &self.inner;
        let ids: Vec<(i64, String)> = inner
            .lock()
            .active
            .iter()
            .map(|(id, a)| (*id, a.automation_id.clone()))
            .collect();
        for (run_id, automation_id) in ids {
            let exists = inner
                .store
                .with_read(|conn| -> Result<bool, StoreError> {
                    Ok(automations::get(conn, &automation_id)?.is_some())
                })
                .unwrap_or(true);
            if !exists && let Some(active) = inner.lock().active.get_mut(&run_id) {
                active.stop.get_or_insert(StopKind::Deleted);
                active.cancel.cancel();
            }
        }
    }

    // ── starting work ──

    fn start_next(&self, now: i64) {
        let inner = &self.inner;
        {
            let state = inner.lock();
            if state.busy || state.shutdown || !state.hosting {
                return;
            }
        }
        // Cheap look first: nothing eligible, nothing to spawn.
        let class = inner.class;
        let Ok(entries) = inner
            .store
            .with_read(|conn| -> Result<Vec<QueueEntry>, StoreError> {
                queue::for_host(conn, class)
            })
        else {
            return;
        };
        {
            // Bookkeeping of entries that are gone (a row id can be reused by a later entry).
            let mut state = inner.lock();
            let live: HashSet<i64> = entries.iter().map(|e| e.id).collect();
            state.parked_logged.retain(|id, _| live.contains(id));
        }
        if !inner.unattended {
            runner::expire_user_requests(inner, &entries, now);
            runner::park_unattended(inner, &entries, now);
        }
        {
            let state = inner.lock();
            if !entries
                .iter()
                .any(|e| runner::eligible(&state, e, now, inner.unattended))
            {
                return;
            }
        }
        {
            let mut state = inner.lock();
            if state.busy {
                return;
            }
            state.busy = true;
        }
        let worker = self.clone();
        let started = inner.deps.spawn(
            "ferrum-automation-run",
            Box::new(move || {
                let _slot = runner::SlotGuard::new(&worker.inner);
                runner::start_cycle(&worker.inner);
            }),
        );
        if !started {
            inner.lock().busy = false;
        }
    }

    /// Reserve capacity for a recheck and run it on its own thread.
    fn try_begin_recheck(&self, automation_id: &str, run_id: i64) -> bool {
        let inner = &self.inner;
        {
            let mut state = inner.lock();
            if state.shutdown || state.rechecking >= MAX_RECHECKS {
                return false;
            }
            state.rechecking += 1;
        }
        let worker = self.clone();
        let automation_id = automation_id.to_string();
        let started = inner.deps.spawn(
            "ferrum-automation-recheck",
            Box::new(move || {
                let _slot = runner::RecheckGuard::new(&worker.inner);
                runner::recheck(&worker.inner, &automation_id, run_id);
            }),
        );
        if !started {
            inner.lock().rechecking -= 1;
        }
        started
    }

    /// Test and diagnostics: the outcome of a run row.
    pub fn outcome_of(&self, run_id: i64) -> Option<Outcome> {
        self.inner
            .store
            .with_read(|conn| runs::get(conn, run_id))
            .ok()
            .flatten()
            .and_then(|r| r.outcome)
    }
}
