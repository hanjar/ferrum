//! The watch hook of the host (Automation design §9.2; FER-528 / P2.1).
//!
//! Once per pass the host turns what the watchers counted into **queue entries, runtime state and
//! events - nothing else**:
//!
//! ```text
//! Checking --in sync--> Watching          Watching --event--> Quiet --idle / max wait--> queue a run
//! Checking --differences--> queue a run   Watching/Quiet --event during a run--> pending rerun
//! pending rerun --run ended--> Checking   Watching/Quiet --overflow--> Needs recheck --> Checking
//! any --root lost--> Waiting for device --root back--> Checking (or Watching without the trigger)
//! ```
//!
//! **No shortcut to the engine.** This file never builds a `GateOk`, never calls the engine and
//! never starts a run: a burst that went quiet is queued by `request::trigger_tx`, which refuses an
//! automation whose approval does not verify, that is held or paused (the same checks as Run now
//! and as the schedule hook), and every queued entry is gated again by `runner::start_cycle`
//! (verify, folder probes, lease, delete-policy prerequisites). The "Checking" comparison is
//! started through `runner::begin_check`, whose only way to the engine is the gate's read-only
//! `Compare` mode. A source test keeps those names out of this file and out of `watcher.rs`.
//!
//! **Event paths are untrusted strings.** The sinks (`watcher::make_sink`) reduce them to counts
//! before they get here; this file sees numbers only. Nothing a file system event says is ever
//! used to build an operation: the engine compares the folders afresh (WA-2.4).
//!
//! **Self-generated changes (WA-4).** An event that arrives while a run holds overlapping folders
//! - or within a short grace after it ended - is not a reason to start a countdown. It sets the
//!   single `pending_rerun` flag; when the run is over, one fresh comparison decides whether
//!   anything is left to do. If not: "Ignored N changes written by run #id (self-generated) - no
//!   rerun". The window is read from the history (`runs::since`), so a run that started and ended
//!   between two passes is still seen.
//!
//! **No polling.** Change events come from the factory of `HostDeps::watch_factory` and from
//! nothing else. The one timer that looks at a folder is the existence probe of a UNC root WHILE
//! the automation waits for that device (at most every 30 s, WA-5.3); a local drive is looked at
//! when Windows says a volume came or went. A probe opens the folder; it never lists it.
//!
//! **Resources.** A watcher exists only for an automation that is enabled, approved, not held and
//! hosted by this process, and only while its roots are there; it is released when the automation
//! is paused, deleted, held, sent to review or edited, when a root is lost, when this process stops
//! hosting and at shutdown.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rusqlite::Connection;

use super::request::{self, RequestError};
use super::{HostDeps, Inner, runner};
use crate::automation::model::{
    AutomationPolicy, DeletePolicy, Filters, ProfilePolicy, RootSpec, Runtime, Trigger,
    WatchTrigger,
};
use crate::automation::policy::{self, Verdict};
use crate::automation::store::automations::{self, EventKind};
use crate::automation::store::queue::{self, HostClass, TriggerKind};
use crate::automation::store::runs::{self, Outcome};
use crate::automation::store::runtime::{self, TriggerState};
use crate::automation::store::{StoreError, events, profiles};
use crate::automation::watcher::{
    Avail, Burst, Due, EventFilter, Inbox, WatchHandle, make_sink, supports_notifications,
};
use crate::ops::CancelToken;
use crate::sync_engine::roots;

/// Events that arrive this long after a run ended still belong to it (the operating system may
/// deliver them late).
const GRACE_MS: i64 = 3_000;
/// A comparison after a run waits this long after the run ended.
const SETTLE_MS: i64 = 1_500;
/// The watch automations are read and verified again at least this often.
const RECONCILE_EVERY_MS: i64 = 30_000;
/// A UNC root that is offline is looked at this often (WA-5.3). Local drives are not polled.
const DEVICE_PROBE_EVERY_MS: i64 = 30_000;
/// After a comparison that could not be trusted, the next one waits this long.
const RECHECK_BACKOFF_MS: i64 = 60_000;
/// A watching automation's local roots are opened (identity only, never listed) this often to see
/// that they are still the approved folders (S3a). A liveness check, not change detection.
const HEALTH_EVERY_MS: i64 = 30_000;
/// A watch that dies sooner than this after it was made counts as a failed start (S3), and
/// after `MAX_QUICK_DEATHS` in a row the folder is Unsupported.
const QUICK_DEATH_MS: i64 = 10_000;
const MAX_QUICK_DEATHS: u32 = 3;
/// All the probes of ONE pass together take at most this long (plus the one in flight): a pass
/// must not hold up the host's heartbeat, however many shares are offline (S4). What is left over
/// is done in the next passes.
const PROBE_PASS_BUDGET: Duration = Duration::from_millis(1_000);

// ── What the host tells the hook ──

/// What the hook was told since its last pass (kept in the host's state).
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct Flags {
    /// The store changed: read the automations again.
    pub dirty: bool,
    /// A local volume arrived or was removed: look at the roots again.
    pub volume: bool,
    /// The PC resumed from sleep: compare again (WA-7.2).
    pub resumed: bool,
    /// This process just became the host: start from nothing (WA-7.2).
    pub takeover: bool,
}

/// Why a fresh comparison is made (WA-3.2, WA-5.2, WA-6, WA-7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Why {
    /// The host started.
    Started,
    /// Monitoring was resumed (the automation was enabled).
    Resumed,
    /// The PC woke up.
    Slept,
    /// The watcher missed events.
    Overflow,
    /// Events arrived while a run was using the folders.
    Pending,
    /// A folder came back.
    Available,
}

impl Why {
    /// The text of the Checking sub-line and of its event.
    fn text(self, root: Option<&str>) -> String {
        match self {
            Why::Started => "Ferrum started \u{2014} fresh comparison before watching".to_string(),
            Why::Resumed => {
                "Monitoring resumed \u{2014} fresh comparison to catch changes made while paused"
                    .to_string()
            }
            Why::Slept => "PC resumed from sleep \u{2014} fresh comparison".to_string(),
            Why::Overflow => "Watcher missed events \u{2014} full comparison".to_string(),
            Why::Pending => "Pending rerun \u{2192} recompare folders before running".to_string(),
            Why::Available => match root {
                Some(root) => {
                    format!("{root} available again \u{2192} fresh comparison before any run")
                }
                None => {
                    "Folder available again \u{2192} fresh comparison before any run".to_string()
                }
            },
        }
    }

    /// The kind and text of the entry queued when the comparison finds differences.
    fn entry(self) -> (TriggerKind, &'static str) {
        match self {
            Why::Started | Why::Resumed | Why::Slept => {
                (TriggerKind::Startup, "Fresh comparison found differences")
            }
            Why::Overflow => (TriggerKind::Watch, "Changes the watcher missed"),
            Why::Pending => (TriggerKind::PendingRerun, "Files changed during the run"),
            Why::Available => (TriggerKind::Available, "Folder is back"),
        }
    }
}

/// What a read-only comparison found (made by `runner::begin_check`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CheckAnswer {
    /// Nothing to do.
    InSync,
    /// Something to do (counts only).
    Differences { remaining: i64, conflicts: i64 },
    /// The comparison could not be trusted (cap, errors).
    Unreliable,
    /// A folder is not there (or not the approved one).
    Device,
    /// The gate said no (held, needs review, paused, ...): closed code.
    Refused(&'static str),
    /// The rows moved under the comparison: ask again.
    Retry,
}

// ── State shared with the comparison thread ──

#[derive(Default)]
struct CheckCtx {
    why: Option<Why>,
    /// Events that arrived while the comparison ran.
    noted: u64,
    noted_logged: bool,
    /// Events that fell into a run's window since the last comparison.
    self_events: u64,
    run_id: Option<i64>,
}

/// What the hook and the comparison thread both touch. Everything is behind atomics or its own
/// small lock; neither side holds it across a store call.
pub(crate) struct Shared {
    inbox: Arc<Inbox>,
    running: AtomicBool,
    /// `0` = no backoff.
    retry_at_ms: AtomicI64,
    ctx: Mutex<CheckCtx>,
    cancel: Mutex<CancelToken>,
    /// The watcher of this generation was released (edited, paused, held, stopped). A comparison
    /// that was started for it must not write anything any more (S3b): the next generation of
    /// the same automation owns the row.
    retired: AtomicBool,
    /// Changes noted during a comparison that found the folders in sync: the hook starts the
    /// quiet countdown with them (S1).
    carry: AtomicU64,
    /// The idle delay in ms, for the countdown `finish_check` starts.
    idle_ms: AtomicI64,
    /// Pending-rerun comparisons in a row that found differences (bounded like `settle`'s).
    streak: AtomicU32,
}

impl Shared {
    fn new(inbox: Arc<Inbox>, idle_ms: i64) -> Self {
        Shared {
            inbox,
            running: AtomicBool::new(false),
            retry_at_ms: AtomicI64::new(0),
            ctx: Mutex::new(CheckCtx::default()),
            cancel: Mutex::new(CancelToken::default()),
            retired: AtomicBool::new(false),
            carry: AtomicU64::new(0),
            idle_ms: AtomicI64::new(idle_ms),
            streak: AtomicU32::new(0),
        }
    }

    fn take_carry(&self) -> u64 {
        self.carry.swap(0, Ordering::SeqCst)
    }

    fn retire(&self) {
        self.retired.store(true, Ordering::SeqCst);
        self.cancel_check();
    }

    fn is_retired(&self) -> bool {
        self.retired.load(Ordering::SeqCst)
    }

    pub(crate) fn set_running(&self, on: bool) {
        self.running.store(on, Ordering::SeqCst);
    }

    fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    pub(crate) fn set_cancel(&self, token: CancelToken) {
        *self.cancel.lock().unwrap_or_else(|p| p.into_inner()) = token;
    }

    fn cancel_check(&self) {
        self.cancel
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .cancel();
    }

    fn ctx(&self) -> std::sync::MutexGuard<'_, CheckCtx> {
        self.ctx.lock().unwrap_or_else(|p| p.into_inner())
    }
}

// ── The watched automations ──

/// What the approval and the profile looked like when the watchers were made.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Sig {
    policy: String,
    config_rev: i64,
    profile_revision: i64,
}

struct Desired {
    id: String,
    profile_id: String,
    sig: Sig,
    trigger: WatchTrigger,
    left: RootSpec,
    right: RootSpec,
    filters: Filters,
    versioning: Option<String>,
}

struct Tracked {
    sig: Sig,
    profile_id: String,
    trigger: WatchTrigger,
    left: RootSpec,
    right: RootSpec,
    filters: Filters,
    versioning: Option<String>,
    /// The running watches (empty while a root is offline or nothing can be watched).
    handles: Vec<Box<dyn WatchHandle>>,
    shared: Arc<Shared>,
    burst: Option<Burst>,
    last_drain_ms: i64,
    window_end_ms: Option<i64>,
    window_announced: bool,
    device_seen: bool,
    next_probe_ms: Option<i64>,
    /// The root that was not there when the device state began (for the event text).
    missing_path: Option<String>,
    /// Looks at the roots that were asked for but not done yet (the pass ran out of budget).
    want: Want,
    /// The roots have not been looked at since this watcher was made.
    initial_probe: bool,
    next_health_ms: i64,
    /// When the watches were last made, and how many in a row died soon after (S3).
    made_ms: i64,
    quick_deaths: u32,
    /// The queue entry the "merged into the queued run" event was written for.
    merge_logged: Option<i64>,
}

/// Which roots to look at: the local drives, the shares, and whether the folders may be declared
/// back (a device that was missing).
#[derive(Debug, Default, Clone, Copy)]
struct Want {
    local: bool,
    unc: bool,
}

impl Want {
    fn any(self) -> bool {
        self.local || self.unc
    }

    fn merge(&mut self, other: Want) {
        self.local |= other.local;
        self.unc |= other.unc;
    }
}

/// The time left for the probes of this pass.
struct Budget(Instant);

impl Budget {
    fn new() -> Self {
        Budget(Instant::now() + PROBE_PASS_BUDGET)
    }

    fn left(&self) -> bool {
        Instant::now() < self.0
    }
}

impl Drop for Tracked {
    fn drop(&mut self) {
        // Whatever released this watcher, a comparison it started is moot.
        self.shared.retire();
    }
}

impl Tracked {
    /// How many watches this automation should hold while its folders are there.
    fn expected_handles(&self) -> usize {
        if !self.trigger.on_changes {
            return 0;
        }
        self.sides()
            .iter()
            .filter(|(watched, spec)| *watched && supports_notifications(spec))
            .count()
    }

    fn sides(&self) -> [(bool, &RootSpec); 2] {
        [
            (self.trigger.watch_left, &self.left),
            (self.trigger.watch_right, &self.right),
        ]
    }

    fn all_roots(&self) -> Vec<String> {
        let mut v = vec![self.left.path.clone(), self.right.path.clone()];
        if let Some(root) = &self.versioning {
            v.push(root.clone());
        }
        v
    }
}

/// The watchers of this host, one per watch automation it hosts.
#[derive(Default)]
pub(crate) struct Book {
    tracked: HashMap<String, Tracked>,
    last_reconcile_ms: Option<i64>,
    last_verified_ms: i64,
    /// The next watcher made is the first of this host's life (reason "Ferrum started").
    startup: bool,
}

impl Book {
    fn release_all(&mut self) {
        for (_, mut t) in self.tracked.drain() {
            t.shared.cancel_check();
            t.handles.clear();
        }
        self.last_reconcile_ms = None;
    }
}

/// Release every watcher (host shutdown, or this process stopped hosting).
pub(super) fn release_all<D: HostDeps>(inner: &Arc<Inner<D>>) {
    inner
        .watch
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .release_all();
}

fn class_of(policy: &AutomationPolicy) -> HostClass {
    match policy.runtime {
        Runtime::App => HostClass::App,
        Runtime::Background => HostClass::Background,
    }
}

fn roots_of(profile: &ProfilePolicy) -> Vec<String> {
    let mut v = vec![profile.left.path.clone(), profile.right.path.clone()];
    if let DeletePolicy::Versioning { root } = &profile.delete_policy {
        v.push(root.clone());
    }
    v
}

// ── Reading what should be watched ──

/// The automations this host should watch: enabled, hosted here, approved, not held. An
/// automation whose signature is the one already watched is not verified again (unless `full`).
fn read_desired(
    conn: &Connection,
    keys: &dyn crate::automation::dpapi::ApprovalKeys,
    class: HostClass,
    known: &HashMap<String, Sig>,
    full: bool,
) -> Result<Vec<Desired>, StoreError> {
    let mut out = Vec::new();
    for rec in automations::list(conn)? {
        if rec.kind != "watch" || !rec.enabled {
            continue;
        }
        let Ok(policy) = serde_json::from_str::<AutomationPolicy>(&rec.policy) else {
            continue;
        };
        let Trigger::Watch(trigger) = policy.trigger.clone() else {
            continue;
        };
        if class_of(&policy) != class {
            continue;
        }
        // A hold freezes the automation: no watcher while it lasts (the recheck or retry that
        // resolves it is what brings the watcher back).
        if runtime::get(conn, &rec.id)?.is_none_or(|r| r.attention.is_some()) {
            continue;
        }
        let Some(profile_rec) = profiles::get(conn, &rec.profile_id)? else {
            continue;
        };
        let Ok(profile) = serde_json::from_str::<ProfilePolicy>(&profile_rec.policy) else {
            continue;
        };
        let sig = Sig {
            policy: rec.policy.clone(),
            config_rev: rec.config_rev,
            profile_revision: profile_rec.revision,
        };
        let trusted = !full && known.get(&rec.id) == Some(&sig);
        if !trusted && !matches!(policy::verify(conn, keys, &rec.id)?, Verdict::Valid) {
            continue;
        }
        let versioning = match &profile.delete_policy {
            DeletePolicy::Versioning { root } => Some(root.clone()),
            _ => None,
        };
        out.push(Desired {
            id: rec.id,
            profile_id: rec.profile_id,
            sig,
            trigger,
            left: profile.left,
            right: profile.right,
            filters: profile.filters,
            versioning,
        });
    }
    Ok(out)
}

// ── Watchers ──

fn make_handles<D: HostDeps>(inner: &Arc<Inner<D>>, t: &mut Tracked) -> (usize, usize) {
    if !t.trigger.on_changes {
        return (0, 0);
    }
    let weak = Arc::downgrade(inner);
    let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        if let Some(inner) = weak.upgrade() {
            inner.nudge();
        }
    });
    let skip: Vec<String> = t.versioning.iter().cloned().collect();
    t.made_ms = inner.deps.now_ms();
    let mut started = 0;
    let mut failed = 0;
    let wanted: Vec<RootSpec> = t
        .sides()
        .into_iter()
        .filter(|(watched, spec)| *watched && supports_notifications(spec))
        .map(|(_, spec)| spec.clone())
        .collect();
    for spec in wanted {
        let sink = make_sink(
            EventFilter::new(&spec.path, &skip, &t.filters),
            t.shared.inbox.clone(),
            wake.clone(),
        );
        match inner.deps.watch_factory().watch(&spec.path, sink) {
            Ok(handle) => {
                t.handles.push(handle);
                started += 1;
            }
            Err(_) => failed += 1,
        }
    }
    (started, failed)
}

/// What a look at the roots found. Runs outside any store closure (a probe may take a moment).
struct Report {
    /// The first root that is not there (or not the approved folder).
    missing: Option<String>,
    /// That root is there, but is another folder than the approved one.
    changed: bool,
    /// Every root that was asked is there.
    all_there: bool,
    /// Both roots were asked.
    probed_all: bool,
}

/// Look at the roots: `local` the local drives, `unc` the shares. A probe opens the folder to read
/// its identity; it never lists it.
fn probe_roots<D: HostDeps>(inner: &Arc<Inner<D>>, t: &Tracked, local: bool, unc: bool) -> Report {
    let mut missing = None;
    let mut changed = false;
    let mut all_there = true;
    let mut probed_all = true;
    for spec in [&t.left, &t.right] {
        let is_unc = roots::is_unc(&spec.path);
        if (is_unc && !unc) || (!is_unc && !local) {
            probed_all = false;
            continue;
        }
        let avail = inner.deps.root_available(spec);
        match avail {
            Avail::Yes => {}
            Avail::No | Avail::Changed => {
                all_there = false;
                if missing.is_none() {
                    missing = Some(spec.path.clone());
                    changed = matches!(avail, Avail::Changed);
                }
            }
            Avail::Unknown => all_there = false,
        }
    }
    Report {
        missing,
        changed,
        all_there,
        probed_all,
    }
}

/// [`probe_roots`] for a device that is missing: what was asked, and - when that part is fine - the
/// rest, before the folders are declared back.
fn probe_for_return<D: HostDeps>(
    inner: &Arc<Inner<D>>,
    t: &Tracked,
    local: bool,
    unc: bool,
) -> Report {
    let first = probe_roots(inner, t, local, unc);
    // Only a share's own timer completes the picture with the local drives. A volume event never
    // asks a share ahead of its cadence: the folders are declared back at the next timer.
    if first.missing.is_some() || !first.all_there || first.probed_all || local || !unc {
        return first;
    }
    let rest = probe_roots(inner, t, true, false);
    Report {
        missing: rest.missing,
        changed: rest.changed,
        all_there: rest.all_there,
        probed_all: true,
    }
}

// ── The pass ──

/// One pass. Cheap when nothing happened: it takes the flags, and looks at the counters.
pub(super) fn pass<D: HostDeps>(inner: &Arc<Inner<D>>, now: i64) {
    let (active, flags) = {
        let mut state = inner.lock();
        let active = state.hosting && !state.shutdown && inner.unattended;
        (active, std::mem::take(&mut state.watch_flags))
    };
    let mut book = inner.watch.lock().unwrap_or_else(|p| p.into_inner());
    if !active {
        // Not hosting (another process does), shutting down, or automatic runs are off: no
        // watcher is kept.
        book.release_all();
        return;
    }
    if flags.takeover {
        book.release_all();
        book.startup = true;
    }
    let due = book
        .last_reconcile_ms
        .is_none_or(|t| flags.dirty || now.saturating_sub(t) >= RECONCILE_EVERY_MS || now < t);
    let budget = Budget::new();
    if due {
        reconcile(inner, &mut book, now, &budget);
    }
    // Automations with looks left over from the last pass go first, so nobody starves.
    let mut ids: Vec<String> = book.tracked.keys().cloned().collect();
    ids.sort_by_key(|id| {
        book.tracked
            .get(id)
            .is_none_or(|t| !(t.want.any() || t.initial_probe))
    });
    if ids.is_empty() {
        return;
    }
    let Ok(rows) = inner.store.with_read(runtime::all) else {
        // The store cannot say: try again at the next pass.
        return;
    };
    for id in ids {
        let Some(t) = book.tracked.get_mut(&id) else {
            continue;
        };
        let row = rows.get(&id);
        step(inner, t, &id, row, now, &flags, &budget);
    }
}

fn reconcile<D: HostDeps>(inner: &Arc<Inner<D>>, book: &mut Book, now: i64, budget: &Budget) {
    let full = now.saturating_sub(book.last_verified_ms) >= RECONCILE_EVERY_MS
        || now < book.last_verified_ms;
    let known: HashMap<String, Sig> = book
        .tracked
        .iter()
        .map(|(id, t)| (id.clone(), t.sig.clone()))
        .collect();
    let class = inner.class;
    let Ok(desired) = inner
        .store
        .with_read(|conn| read_desired(conn, inner.deps.keys(), class, &known, full))
    else {
        return;
    };
    book.last_reconcile_ms = Some(now);
    if full {
        book.last_verified_ms = now;
    }
    // Release what is no longer wanted, or whose approval or profile changed.
    let wanted: HashMap<&str, &Sig> = desired.iter().map(|d| (d.id.as_str(), &d.sig)).collect();
    let stale: Vec<String> = book
        .tracked
        .iter()
        .filter(|(id, t)| wanted.get(id.as_str()).is_none_or(|sig| **sig != t.sig))
        .map(|(id, _)| id.clone())
        .collect();
    for id in stale {
        if let Some(mut t) = book.tracked.remove(&id) {
            t.shared.cancel_check();
            t.handles.clear();
        }
    }
    let startup = book.startup;
    let mut made_any = false;
    for d in desired {
        if book.tracked.contains_key(&d.id) {
            continue;
        }
        made_any = true;
        let id = d.id.clone();
        let t = create(
            inner,
            d,
            now,
            if startup { Why::Started } else { Why::Resumed },
            budget,
        );
        book.tracked.insert(id, t);
    }
    if made_any || startup {
        book.startup = false;
    }
}

/// A new watcher: decide where the machine starts and write it down.
fn create<D: HostDeps>(
    inner: &Arc<Inner<D>>,
    d: Desired,
    now: i64,
    why: Why,
    budget: &Budget,
) -> Tracked {
    let id = d.id.clone();
    let idle_ms = i64::from(d.trigger.idle_secs) * 1_000;
    let inbox = Arc::new(Inbox::default());
    let mut t = Tracked {
        sig: d.sig,
        profile_id: d.profile_id,
        trigger: d.trigger,
        left: d.left,
        right: d.right,
        filters: d.filters,
        versioning: d.versioning,
        handles: Vec::new(),
        shared: Arc::new(Shared::new(inbox, idle_ms)),
        burst: None,
        last_drain_ms: now,
        window_end_ms: None,
        window_announced: false,
        device_seen: false,
        next_probe_ms: None,
        missing_path: None,
        want: Want::default(),
        initial_probe: false,
        next_health_ms: now.saturating_add(HEALTH_EVERY_MS),
        made_ms: now,
        quick_deaths: 0,
        merge_logged: None,
    };
    // Which watched roots can be watched at all (WA-2.3).
    let watchable = t
        .sides()
        .iter()
        .filter(|(watched, spec)| *watched && supports_notifications(spec))
        .count();
    // The first look at the roots; when this pass has no time left for it, the first step does it
    // and the comparison waits for it.
    let report = if budget.left() {
        probe_roots(inner, &t, true, true)
    } else {
        t.initial_probe = true;
        Report {
            missing: None,
            changed: false,
            all_there: true,
            probed_all: true,
        }
    };
    let (state, text, check_reason, kind) = if t.trigger.on_changes && watchable == 0 {
        (
            TriggerState::Unsupported,
            "No change notifications on this provider \u{2014} can't be watched".to_string(),
            None,
            EventKind::Warn,
        )
    } else if let Some(path) = &report.missing {
        t.device_seen = true;
        t.missing_path = Some(path.clone());
        t.next_probe_ms = Some(now.saturating_add(DEVICE_PROBE_EVERY_MS));
        (
            TriggerState::Device,
            lost_text(path, report.changed),
            None,
            EventKind::Warn,
        )
    } else {
        // When the first look is still to come, no watch is made on a guess: the first step
        // decides between Device and making the watches.
        let (started, failed) = if t.initial_probe {
            (0, 0)
        } else {
            make_handles(inner, &mut t)
        };
        if t.trigger.on_changes && started == 0 && failed > 0 {
            (
                TriggerState::Unsupported,
                "Change notifications could not be started for this folder \u{2014} can't be watched"
                    .to_string(),
                None,
                EventKind::Warn,
            )
        } else {
            t.shared.ctx().why = Some(why);
            let text = why.text(None);
            (
                TriggerState::Checking,
                text.clone(),
                Some(text),
                EventKind::Info,
            )
        }
    };
    let _ = inner.write(|conn| {
        runtime::set_pending_rerun(conn, &id, false, now)?;
        runtime::set_trigger_state(conn, &id, state, None, None, check_reason.as_deref(), now)?;
        events::add_event(conn, &id, now, kind, &text)?;
        Ok(())
    });
    t
}

// ── The window of a run (WA-3, WA-4) ──

/// How the batch of events just drained relates to the runs on overlapping folders.
#[derive(Debug, Clone, Copy, Default)]
struct Window {
    /// A run holds overlapping folders right now.
    active: bool,
    /// The batch falls into a run's window: a run was active at some point since the previous
    /// look, or ended less than the grace ago (the operating system delivers late).
    in_window: bool,
    /// The newest run of that window.
    run_id: Option<i64>,
    /// No run is active and the last one ended long enough ago to compare.
    settled: bool,
}

fn window_of(conn: &Connection, t: &mut Tracked, now: i64) -> Result<Window, StoreError> {
    let since = t.last_drain_ms.saturating_sub(GRACE_MS);
    let mine = t.all_roots();
    let mut profile_roots: HashMap<String, Vec<String>> = HashMap::new();
    let mut w = Window::default();
    for run in runs::since(conn, since)? {
        // A history entry that touched nothing (a skipped schedule, a refused start) is no window.
        if run.outcome == Some(Outcome::Skipped) || (run.ended_ms.is_some() && run.ops_total == 0) {
            continue;
        }
        let theirs = match profile_roots.get(&run.profile_id) {
            Some(v) => v.clone(),
            None => {
                let v = profiles::get(conn, &run.profile_id)?
                    .and_then(|p| serde_json::from_str::<ProfilePolicy>(&p.policy).ok())
                    .map(|p| roots_of(&p))
                    .unwrap_or_default();
                profile_roots.insert(run.profile_id.clone(), v.clone());
                v
            }
        };
        if !queue::roots_overlap(&mine, &theirs) {
            continue;
        }
        w.run_id = w.run_id.max(Some(run.id));
        match run.ended_ms {
            None => {
                w.active = true;
                w.in_window = true;
            }
            Some(end) => {
                t.window_end_ms = t.window_end_ms.max(Some(end));
                // The run ended after the previous look (the batch may hold events it caused),
                // or ended so recently that its own writes may still be arriving.
                if end > t.last_drain_ms || now < end.saturating_add(GRACE_MS) {
                    w.in_window = true;
                }
            }
        }
    }
    if w.active {
        t.window_end_ms = None;
    }
    w.settled = !w.active
        && t.window_end_ms
            .is_none_or(|end| now >= end.saturating_add(SETTLE_MS));
    Ok(w)
}

// ── One automation, one pass ──

fn state_of(row: &runtime::RuntimeRow) -> TriggerState {
    match row.trigger_state {
        // An unknown stored state re-checks rather than claiming everything is quiet.
        None => TriggerState::Checking,
        Some(TriggerState::Scheduled) => TriggerState::Watching,
        Some(s) => s,
    }
}

#[derive(Debug, Default)]
struct Out {
    /// Start the comparison (the state is `Checking`).
    start: bool,
    /// The roots are gone: release the watchers.
    drop_handles: bool,
    /// The roots are back: make the watchers again.
    rebuild_handles: bool,
    /// The next comparison waits (a watch that keeps dying).
    backoff: bool,
}

#[allow(clippy::too_many_arguments)]
fn step<D: HostDeps>(
    inner: &Arc<Inner<D>>,
    t: &mut Tracked,
    id: &str,
    row: Option<&runtime::RuntimeRow>,
    now: i64,
    flags: &Flags,
    budget: &Budget,
) {
    let Some(row) = row else { return };
    let state = state_of(row);
    if state == TriggerState::Unsupported {
        let _ = t.shared.inbox.take();
        t.last_drain_ms = now;
        return;
    }
    let idle_ms = i64::from(t.trigger.idle_secs) * 1_000;
    let max_ms = i64::from(t.trigger.max_wait_secs) * 1_000;
    let running = t.shared.is_running();

    // Which folders to look at. After a comparison that found a folder missing although the probe
    // said it was there, the next look waits.
    let retry_due = t.shared.retry_at_ms.load(Ordering::SeqCst) <= now;
    let first_look = state == TriggerState::Device && !t.device_seen && retry_due;
    let timer_probe =
        state == TriggerState::Device && retry_due && t.next_probe_ms.is_some_and(|d| now >= d);
    let device_probe = state == TriggerState::Device
        && (first_look || timer_probe || ((flags.volume || flags.resumed) && retry_due));
    let slept = flags.resumed
        && matches!(
            state,
            TriggerState::Watching | TriggerState::Quiet | TriggerState::Recheck
        );
    // S3a: a watching automation's local roots are opened now and then (identity only).
    let health =
        matches!(state, TriggerState::Watching | TriggerState::Quiet) && now >= t.next_health_ms;
    // What may have to be looked at (events are counted below, after the budget decision).
    let peek = t.shared.inbox.peek();
    let live_probe = state != TriggerState::Device
        && (flags.volume || peek.root_touched || peek.failed || health);
    let mut want = std::mem::take(&mut t.want);
    if t.initial_probe {
        want.merge(Want {
            local: true,
            unc: true,
        });
    }
    if device_probe {
        want.merge(Want {
            local: first_look || timer_probe || flags.volume || flags.resumed,
            unc: first_look || timer_probe,
        });
    }
    if live_probe || slept {
        want.merge(Want {
            local: true,
            unc: false,
        });
    }
    if want.any() && !budget.left() {
        // S4: this pass has used its time for probes; what is wanted waits for the next pass.
        t.want = want;
        return;
    }

    let drained = t.shared.inbox.take();
    // A countdown that a comparison started (changes noted while it ran, S1).
    if state == TriggerState::Quiet && t.burst.is_none() {
        let carried = t.shared.take_carry();
        if carried > 0 {
            t.burst = Some(Burst::start(now, idle_ms, carried));
        }
    }
    let burst_due = state == TriggerState::Quiet
        && match t.burst.as_mut() {
            Some(b) => b.due(now, idle_ms, max_ms).is_some(),
            None => true,
        };

    // The window is read (never written) first; the store is written only when there is work.
    let need_window = drained.changes > 0
        || drained.overflow
        || drained.failed
        || row.pending_rerun
        || slept
        || device_probe
        || matches!(
            state,
            TriggerState::Quiet | TriggerState::Checking | TriggerState::Recheck
        );
    let win = if need_window {
        match inner.store.with_read(|conn| window_of(conn, t, now)) {
            Ok(w) => w,
            Err(_) => {
                // The store cannot say: what was counted waits for the next pass.
                t.shared.inbox.restore(drained);
                t.want = want;
                return;
            }
        }
    } else {
        Window {
            settled: true,
            ..Window::default()
        }
    };

    let can_compare = runner::has_capacity(inner);
    let work = !drained.is_empty()
        || want.any()
        || burst_due
        || (state == TriggerState::Quiet && win.in_window)
        || (state == TriggerState::Watching && row.pending_rerun && win.settled && can_compare)
        || (state == TriggerState::Checking
            && !running
            && win.settled
            && can_compare
            && !t.initial_probe)
        || (state == TriggerState::Recheck && retry_due && win.settled && can_compare);
    if !work {
        t.last_drain_ms = now;
        return;
    }

    // The probes open the roots; they run before the store is touched.
    let mut was_initial = false;
    let report = if want.any() {
        let r = if state == TriggerState::Device {
            // A local drive is looked at when Windows says a volume came or went; a share on its
            // own cadence (WA-5.3). Timer and volume events never ask about the other kind.
            probe_for_return(inner, t, want.local, want.unc)
        } else {
            probe_roots(inner, t, want.local, want.unc)
        };
        if state == TriggerState::Device {
            t.device_seen = true;
            if first_look || timer_probe {
                // WA-5.3: while the device is missing it is looked at again at most every 30 s
                // (a share, and a local folder that comes back without any volume event, such
                // as one restored from the Recycle Bin).
                t.next_probe_ms = Some(now.saturating_add(DEVICE_PROBE_EVERY_MS));
            }
        }
        if health {
            t.next_health_ms = now.saturating_add(HEALTH_EVERY_MS);
        }
        was_initial = t.initial_probe;
        t.initial_probe = false;
        Some(r)
    } else {
        None
    };

    let keys = inner.deps.keys();
    let handles_short =
        t.handles.len() < t.expected_handles() || t.handles.iter().any(|h| !h.alive());
    let result = inner.write_quiet(|conn| {
        apply(
            conn,
            keys,
            t,
            id,
            &Inputs {
                drained,
                win,
                report,
                now,
                idle_ms,
                max_ms,
                running,
                slept,
                health,
                handles_short,
                initial: was_initial,
            },
        )
    });
    t.last_drain_ms = now;
    let Ok(out) = result else {
        // The transaction was rolled back: what was counted waits for the next pass.
        t.shared.inbox.restore(drained);
        return;
    };
    if out.drop_handles {
        t.handles.clear();
    }
    if out.backoff {
        t.shared
            .retry_at_ms
            .store(now.saturating_add(RECHECK_BACKOFF_MS), Ordering::SeqCst);
    }
    if out.rebuild_handles {
        t.handles.clear();
        let (started, failed) = make_handles(inner, t);
        if t.trigger.on_changes && started == 0 && failed > 0 {
            // The notifications could not be (re)started. Right after the first look nothing
            // ever worked: unsupported. Otherwise: a comparison after a pause for trust.
            let first = was_initial;
            let _ = inner.write(|conn| {
                let (state, text) = if first {
                    (
                        TriggerState::Unsupported,
                        "Change notifications could not be started for this folder \u{2014} can't be watched",
                    )
                } else {
                    (
                        TriggerState::Recheck,
                        "Change notifications could not be restarted \u{2014} full comparison when safe",
                    )
                };
                runtime::set_trigger_state(conn, id, state, None, None, None, now)?;
                events::add_event(conn, id, now, EventKind::Warn, text)?;
                Ok(())
            });
            t.shared
                .retry_at_ms
                .store(now.saturating_add(RECHECK_BACKOFF_MS), Ordering::SeqCst);
            return;
        }
    }
    if out.start {
        runner::begin_check(inner, id, &t.shared);
    }
}

struct Inputs {
    drained: crate::automation::watcher::Drained,
    win: Window,
    report: Option<Report>,
    now: i64,
    idle_ms: i64,
    max_ms: i64,
    running: bool,
    slept: bool,
    /// The periodic look at the roots was part of this pass.
    health: bool,
    /// The watcher holds fewer watches than it should.
    handles_short: bool,
    /// This was the first look at the roots since the watcher was made.
    initial: bool,
}

/// The event for a root that is not usable. A folder that was replaced is not "disconnected":
/// the way out is the profile, not the cable.
fn lost_text(path: &str, changed: bool) -> String {
    if changed {
        format!("{path} is not the approved folder \u{2014} edit the profile to use it")
    } else {
        format!("{path} not connected \u{2014} no sync")
    }
}

fn plural(n: u64, one: &str, many: &str) -> String {
    if n == 1 {
        format!("{n} {one}")
    } else {
        format!("{n} {many}")
    }
}

/// All of one automation's decisions for this pass, inside ONE transaction that re-reads the row.
fn apply(
    conn: &Connection,
    keys: &dyn crate::automation::dpapi::ApprovalKeys,
    t: &mut Tracked,
    id: &str,
    i: &Inputs,
) -> Result<Out, StoreError> {
    let mut out = Out::default();
    let now = i.now;
    let Some(row) = runtime::get(conn, id)? else {
        return Ok(out);
    };
    let mut state = state_of(&row);
    let mut pending = row.pending_rerun;
    let changes = i.drained.changes;

    // ── 1. Are the folders there? (WA-5) ──
    if let Some(report) = &i.report {
        if let Some(path) = &report.missing {
            if state != TriggerState::Device {
                t.burst = None;
                t.device_seen = true;
                t.missing_path = Some(path.clone());
                t.next_probe_ms = Some(now.saturating_add(DEVICE_PROBE_EVERY_MS));
                runtime::set_trigger_state(conn, id, TriggerState::Device, None, None, None, now)?;
                events::add_event(
                    conn,
                    id,
                    now,
                    EventKind::Warn,
                    &lost_text(path, report.changed),
                )?;
                out.drop_handles = true;
            }
            // Nothing is compared as if the folder were empty, and nothing is deleted.
            return Ok(out);
        }
        if state == TriggerState::Device && report.all_there && report.probed_all {
            // WA-5.2: back. With "Location becomes available" the folders are compared before any
            // run; without it the watch simply goes on.
            out.rebuild_handles = true;
            t.device_seen = false;
            t.next_probe_ms = None;
            let back = t.missing_path.take();
            let queued = queue::by_profile(conn, &t.profile_id)?.is_some();
            if t.trigger.on_available && !queued {
                let text = Why::Available.text(back.as_deref());
                t.shared.ctx().why = Some(Why::Available);
                runtime::set_trigger_state(
                    conn,
                    id,
                    TriggerState::Checking,
                    None,
                    None,
                    Some(&text),
                    now,
                )?;
                events::add_event(conn, id, now, EventKind::Info, &text)?;
                // Started now when no run uses the folders; otherwise at the next pass.
                out.start = i.win.settled;
            } else {
                runtime::set_trigger_state(
                    conn,
                    id,
                    TriggerState::Watching,
                    None,
                    None,
                    None,
                    now,
                )?;
                events::add_event(
                    conn,
                    id,
                    now,
                    EventKind::Info,
                    "Folders available again \u{2014} watching",
                )?;
            }
            return Ok(out);
        }
    }
    if state == TriggerState::Device {
        // Still waiting; whatever the watchers said while a folder was missing means nothing.
        return Ok(out);
    }
    if i.drained.failed && i.report.as_ref().is_some_and(|r| r.missing.is_none()) {
        // A watch reported that it is gone while the folders are there and are the approved ones
        // (FER-565: the own backend says so; `notify` could not). One that lived a while is put
        // back now; one that dies right after it was made is treated like a start that failed
        // (S3): no rebuild loop, the health look tries again after the backoff, and a folder
        // whose watch keeps dying at once is Unsupported. The events it may have missed are
        // compared below (section 2), or by the comparison that is running.
        if now.saturating_sub(t.made_ms) < QUICK_DEATH_MS {
            t.quick_deaths += 1;
            if t.quick_deaths >= MAX_QUICK_DEATHS {
                out.drop_handles = true;
                runtime::set_trigger_state(
                    conn,
                    id,
                    TriggerState::Unsupported,
                    None,
                    None,
                    None,
                    now,
                )?;
                events::add_event(
                    conn,
                    id,
                    now,
                    EventKind::Warn,
                    "Change notifications keep stopping for this folder \u{2014} can't be watched",
                )?;
                return Ok(out);
            } else if t.quick_deaths >= 2 {
                // The second quick death in a row: wait (the 30 s look makes the next watch).
                out.drop_handles = true;
                out.backoff = true;
            } else {
                out.rebuild_handles = true;
            }
        } else {
            t.quick_deaths = 0;
            out.rebuild_handles = true;
        }
    }
    if i.initial && i.handles_short && i.report.as_ref().is_some_and(|r| r.missing.is_none()) {
        // The first look found the folders; the watches were not made on a guess (S4): make them.
        out.rebuild_handles = true;
    }
    if i.health
        && i.handles_short
        && matches!(state, TriggerState::Watching | TriggerState::Quiet)
        && i.report.as_ref().is_some_and(|r| r.missing.is_none())
    {
        // The folders are there and are the approved ones, but a watch is gone (it died, or an
        // earlier restart failed): put it back, and compare because events may have been lost.
        t.burst = None;
        state = TriggerState::Recheck;
        t.shared.retry_at_ms.store(0, Ordering::SeqCst);
        out.rebuild_handles = true;
        runtime::set_trigger_state(conn, id, TriggerState::Recheck, None, None, None, now)?;
        events::add_event(
            conn,
            id,
            now,
            EventKind::Warn,
            "Watcher restarted \u{2014} full comparison when safe",
        )?;
    }

    // ── 2. The watcher lost events (WA-6) ──
    let lost = i.drained.overflow || i.drained.failed;
    if lost && !matches!(state, TriggerState::Checking | TriggerState::Recheck) {
        t.burst = None;
        state = TriggerState::Recheck;
        t.shared.retry_at_ms.store(0, Ordering::SeqCst);
        runtime::set_trigger_state(conn, id, TriggerState::Recheck, None, None, None, now)?;
        events::add_event(
            conn,
            id,
            now,
            EventKind::Warn,
            "Watcher missed events \u{2014} full comparison when safe",
        )?;
    } else if lost && state == TriggerState::Checking {
        // The comparison that is running may have walked past what was lost: count it as a noted
        // change, so an "in sync" answer goes through one more countdown and comparison (S1).
        let mut ctx = t.shared.ctx();
        ctx.noted = ctx.noted.saturating_add(1);
    }

    // ── 3. The PC woke up (WA-7.2): one fresh comparison, the countdown is moot ──
    if i.slept && state != TriggerState::Recheck {
        t.burst = None;
        state = TriggerState::Checking;
        t.shared.ctx().why = Some(Why::Slept);
        let text = Why::Slept.text(None);
        runtime::set_trigger_state(
            conn,
            id,
            TriggerState::Checking,
            None,
            None,
            Some(&text),
            now,
        )?;
        events::add_event(conn, id, now, EventKind::Info, &text)?;
    }

    // ── 4. Changes (WA-1, WA-3, WA-4) ──
    if changes > 0 {
        if matches!(state, TriggerState::Checking | TriggerState::Recheck) {
            // WA-3.4: noted, no countdown.
            let mut ctx = t.shared.ctx();
            ctx.noted = ctx.noted.saturating_add(changes);
            if !ctx.noted_logged {
                ctx.noted_logged = true;
                drop(ctx);
                events::add_event(
                    conn,
                    id,
                    now,
                    EventKind::Mute,
                    &format!(
                        "{} \u{2014} the comparison will include them",
                        plural(changes, "change noted", "changes noted")
                    ),
                )?;
            }
        } else if i.win.in_window {
            // WA-3.1 / WA-4.1: a run uses (or just used) these folders. One pending rerun.
            {
                let mut ctx = t.shared.ctx();
                ctx.self_events = ctx.self_events.saturating_add(changes);
                ctx.run_id = ctx.run_id.max(i.win.run_id);
            }
            if state == TriggerState::Quiet {
                t.burst = None;
                state = TriggerState::Watching;
                runtime::set_trigger_state(
                    conn,
                    id,
                    TriggerState::Watching,
                    None,
                    None,
                    None,
                    now,
                )?;
            }
            if !pending {
                pending = true;
                runtime::set_pending_rerun(conn, id, true, now)?;
            }
            if !t.window_announced {
                t.window_announced = true;
                events::add_event(
                    conn,
                    id,
                    now,
                    EventKind::Info,
                    &format!(
                        "{} during the active run \u{2192} 1 pending rerun",
                        plural(changes, "file event", "file events")
                    ),
                )?;
            }
        } else if let Some(entry) = queue::by_profile(conn, &t.profile_id)? {
            // WA-3.3: a run for these folders already waits; the changes are part of it. The
            // event is written only when the trigger really merged, and once per queued entry.
            if t.trigger.on_changes {
                let merged = matches!(
                    request::trigger_tx(
                        conn,
                        keys,
                        id,
                        TriggerKind::Watch,
                        "Changes while a run waits",
                        now,
                    ),
                    Ok(request::QueueAnswer::Merged)
                );
                if merged && t.merge_logged != Some(entry.id) {
                    t.merge_logged = Some(entry.id);
                    events::add_event(
                        conn,
                        id,
                        now,
                        EventKind::Mute,
                        &format!(
                            "{} merged into the queued run",
                            plural(changes, "change", "changes")
                        ),
                    )?;
                }
            }
        } else if t.trigger.on_changes
            && matches!(state, TriggerState::Watching | TriggerState::Quiet)
        {
            match t.burst.as_mut() {
                Some(b) if state == TriggerState::Quiet => b.event(now, i.idle_ms, changes),
                _ => {
                    t.burst = Some(Burst::start(now, i.idle_ms, changes));
                    events::add_event(
                        conn,
                        id,
                        now,
                        EventKind::Info,
                        &format!(
                            "Changes detected \u{2014} waiting for {} s without changes",
                            t.trigger.idle_secs
                        ),
                    )?;
                }
            }
            state = TriggerState::Quiet;
            if let Some(b) = &t.burst {
                runtime::set_trigger_state(
                    conn,
                    id,
                    TriggerState::Quiet,
                    Some(b.until_ms),
                    Some(b.started_ms),
                    None,
                    now,
                )?;
            }
        }
    } else if state == TriggerState::Quiet && i.win.in_window {
        // A run took the folders while the countdown ran: it will include the changes, and one
        // comparison afterwards decides whether anything is left.
        t.burst = None;
        state = TriggerState::Watching;
        runtime::set_trigger_state(conn, id, TriggerState::Watching, None, None, None, now)?;
        if !pending {
            pending = true;
            runtime::set_pending_rerun(conn, id, true, now)?;
        }
    }
    if !i.win.in_window && !i.win.active {
        t.window_announced = false;
    }
    if queue::by_profile(conn, &t.profile_id)?.is_none() {
        t.merge_logged = None;
    }

    // ── 5. The countdown is over (WA-1.1, WA-1.4): queue ONE run ──
    if state == TriggerState::Quiet {
        let due = t
            .burst
            .as_mut()
            .and_then(|b| b.due(now, i.idle_ms, i.max_ms).map(|d| (d, b.count)));
        let (why, count) = match due {
            Some((d, c)) => (Some(d), c),
            None => (None, 0),
        };
        if t.burst.is_none() {
            // A countdown without its burst (the row was changed under us): nothing is counting.
            state = TriggerState::Watching;
            runtime::set_trigger_state(conn, id, TriggerState::Watching, None, None, None, now)?;
        }
        if let Some(why) = why {
            t.burst = None;
            t.shared.streak.store(0, Ordering::SeqCst);
            let text = match why {
                Due::Idle => format!("Changes \u{b7} quiet for {} s", t.trigger.idle_secs),
                Due::MaxWait => format!(
                    "Changes \u{b7} waited {} min at most",
                    (t.trigger.max_wait_secs / 60).max(1)
                ),
            };
            let line = match (why, count) {
                (Due::Idle, 0) => "Quiet again \u{2014} run queued".to_string(),
                (Due::Idle, n) => format!(
                    "Quiet for {} s \u{2014} run queued ({})",
                    t.trigger.idle_secs,
                    plural(n, "change", "changes")
                ),
                (Due::MaxWait, n) => format!(
                    "Waited the maximum {} min \u{2014} run queued ({})",
                    (t.trigger.max_wait_secs / 60).max(1),
                    plural(n, "change", "changes")
                ),
            };
            state = TriggerState::Watching;
            runtime::set_trigger_state(conn, id, TriggerState::Watching, None, None, None, now)?;
            match request::trigger_tx(conn, keys, id, TriggerKind::Watch, &text, now) {
                Ok(_) => events::add_event(conn, id, now, EventKind::Info, &line)?,
                Err(refusal) => {
                    events::add_event(conn, id, now, EventKind::Warn, refused_line(&refusal))?
                }
            }
        }
    }

    // ── 6. A comparison is due (WA-3.2, WA-6, WA-7) ──
    if state == TriggerState::Recheck
        && t.shared.retry_at_ms.load(Ordering::SeqCst) <= now
        && i.win.settled
    {
        state = TriggerState::Checking;
        t.shared.ctx().why = Some(Why::Overflow);
        let text = Why::Overflow.text(None);
        runtime::set_trigger_state(
            conn,
            id,
            TriggerState::Checking,
            None,
            None,
            Some(&text),
            now,
        )?;
        events::add_event(conn, id, now, EventKind::Info, &text)?;
    } else if state == TriggerState::Watching && pending && i.win.settled {
        state = TriggerState::Checking;
        t.shared.ctx().why = Some(Why::Pending);
        let text = Why::Pending.text(None);
        runtime::set_trigger_state(
            conn,
            id,
            TriggerState::Checking,
            None,
            None,
            Some(&text),
            now,
        )?;
        events::add_event(conn, id, now, EventKind::Info, &text)?;
    }
    if state == TriggerState::Checking && !i.running && i.win.settled {
        // A run is already queued for these folders: it compares first, nothing to add.
        if queue::by_profile(conn, &t.profile_id)?.is_some() {
            runtime::set_trigger_state(conn, id, TriggerState::Watching, None, None, None, now)?;
            runtime::set_pending_rerun(conn, id, false, now)?;
            events::add_event(
                conn,
                id,
                now,
                EventKind::Mute,
                "A run is already queued for these folders \u{2014} it compares first",
            )?;
        } else {
            out.start = true;
        }
    }
    Ok(out)
}

fn refused_line(refusal: &RequestError) -> &'static str {
    match refusal {
        RequestError::Held(_) => "Run not queued \u{2014} new runs are held (needs attention)",
        RequestError::NeedsReview => "Run not queued \u{2014} the automation needs review",
        RequestError::Paused => "Run not queued \u{2014} the automation is paused",
        _ => "Run not queued \u{2014} it cannot be queued right now",
    }
}

// ── The end of a comparison ──

/// Record what a read-only comparison found. Runs on the comparison's thread; one transaction.
pub(super) fn finish_check<D: HostDeps>(
    inner: &Arc<Inner<D>>,
    automation_id: &str,
    shared: &Shared,
    answer: CheckAnswer,
) {
    let now = inner.deps.now_ms();
    let (why, noted, self_events, run_id) = {
        let mut ctx = shared.ctx();
        let taken = (
            ctx.why.unwrap_or(Why::Resumed),
            ctx.noted,
            ctx.self_events,
            ctx.run_id,
        );
        if !matches!(answer, CheckAnswer::Retry) {
            ctx.noted = 0;
            ctx.noted_logged = false;
            ctx.self_events = 0;
            ctx.run_id = None;
        }
        taken
    };
    let keys = inner.deps.keys();
    let id = automation_id;
    let _ = inner.write(|conn| {
        // S3b: a comparison that belongs to a watcher that was released since (edited and
        // approved again, paused, held) writes nothing: the row is the next generation's.
        if shared.is_retired() {
            return Ok(());
        }
        let Some(row) = runtime::get(conn, id)? else {
            return Ok(());
        };
        // Paused, reset or sent elsewhere while it ran: the answer no longer applies.
        if row.trigger_state != Some(TriggerState::Checking) {
            return Ok(());
        }
        match answer {
            CheckAnswer::InSync => {
                shared.streak.store(0, Ordering::SeqCst);
                runtime::set_pending_rerun(conn, id, false, now)?;
                if noted > 0 {
                    // S1: changes arrived while the folders were being compared; the walk may
                    // have passed them already. The comparison said "in sync" about the folders
                    // as it saw them, so a quiet countdown follows and one run (which compares
                    // again) comes out of it.
                    let idle = shared.idle_ms.load(Ordering::SeqCst);
                    shared.carry.store(noted, Ordering::SeqCst);
                    runtime::set_trigger_state(
                        conn,
                        id,
                        TriggerState::Quiet,
                        Some(now.saturating_add(idle)),
                        Some(now),
                        None,
                        now,
                    )?;
                    events::add_event(
                        conn,
                        id,
                        now,
                        EventKind::Info,
                        &format!(
                            "Folders in sync, but {} while it compared \u{2014} waiting for {} s without changes",
                            plural(noted, "change arrived", "changes arrived"),
                            idle / 1_000
                        ),
                    )?;
                    return Ok(());
                }
                runtime::set_trigger_state(conn, id, TriggerState::Watching, None, None, None, now)?;
                let line = if self_events > 0 {
                    match run_id {
                        Some(run) => format!(
                            "Ignored {} written by run #{run} (self-generated) \u{2014} no rerun",
                            plural(self_events, "change", "changes")
                        ),
                        None => format!(
                            "Ignored {} (self-generated) \u{2014} no rerun",
                            plural(self_events, "change", "changes")
                        ),
                    }
                } else {
                    "Fresh comparison: folders are in sync".to_string()
                };
                events::add_event(conn, id, now, EventKind::Mute, &line)?;
            }
            CheckAnswer::Differences { remaining, conflicts } => {
                runtime::set_trigger_state(conn, id, TriggerState::Watching, None, None, None, now)?;
                runtime::set_pending_rerun(conn, id, false, now)?;
                let (kind, text) = why.entry();
                // A watch that keeps finding differences right after its own runs stops chaining
                // them, as the engine's pending reruns do (MAX_RERUN_STREAK).
                if why == Why::Pending {
                    let n = shared.streak.fetch_add(1, Ordering::SeqCst) + 1;
                    if n > super::MAX_RERUN_STREAK {
                        shared.streak.store(0, Ordering::SeqCst);
                        events::add_event(
                            conn,
                            id,
                            now,
                            EventKind::Warn,
                            "Files keep changing during the runs \u{2014} the next run waits for the next trigger",
                        )?;
                        return Ok(());
                    }
                }
                match request::trigger_tx(conn, keys, id, kind, text, now) {
                    Ok(_) => events::add_event(
                        conn,
                        id,
                        now,
                        EventKind::Info,
                        &format!(
                            "Fresh comparison found {} \u{2014} run queued",
                            if remaining + conflicts == 1 {
                                "1 difference".to_string()
                            } else {
                                format!("{} differences", remaining + conflicts)
                            }
                        ),
                    )?,
                    Err(refusal) => {
                        events::add_event(conn, id, now, EventKind::Warn, refused_line(&refusal))?
                    }
                }
            }
            CheckAnswer::Unreliable => {
                runtime::set_trigger_state(conn, id, TriggerState::Recheck, None, None, None, now)?;
                shared
                    .retry_at_ms
                    .store(now.saturating_add(RECHECK_BACKOFF_MS), Ordering::SeqCst);
                events::add_event(
                    conn,
                    id,
                    now,
                    EventKind::Warn,
                    "Fresh comparison could not complete \u{2014} the folders could not be compared reliably; trying again later",
                )?;
            }
            CheckAnswer::Device => {
                shared
                    .retry_at_ms
                    .store(now.saturating_add(DEVICE_PROBE_EVERY_MS), Ordering::SeqCst);
                runtime::set_trigger_state(conn, id, TriggerState::Device, None, None, None, now)?;
                events::add_event(
                    conn,
                    id,
                    now,
                    EventKind::Warn,
                    "A watched folder is not connected \u{2014} no sync",
                )?;
            }
            CheckAnswer::Refused(code) => {
                runtime::set_trigger_state(conn, id, TriggerState::Watching, None, None, None, now)?;
                events::add_event(
                    conn,
                    id,
                    now,
                    EventKind::Mute,
                    &format!("Fresh comparison not done ({code})"),
                )?;
            }
            // The rows moved under the comparison: it is asked again at the next pass.
            CheckAnswer::Retry => {}
        }
        Ok(())
    });
    inner.nudge();
}

/// Test: the watches die silently (as `notify` 7 on Windows can let them), the way a health check
/// has to find out.
#[cfg(test)]
pub(super) fn kill_watches<D: HostDeps>(inner: &Arc<Inner<D>>, id: &str) {
    let mut book = inner.watch.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(t) = book.tracked.get_mut(id) {
        t.handles.clear();
    }
}
