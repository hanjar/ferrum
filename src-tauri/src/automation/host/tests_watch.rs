//! Host tests, part 4 (FER-528 / P2.1): the watch hook. A fake watcher factory (the ONLY source of
//! change events) and a virtual clock drive `Host::tick()`; nothing sleeps for the state machine and
//! no test retries. What is pinned here:
//!
//! - the prototype fixture (events at t=0 and t=6, idle 10: one run at t=16) and the maximum wait,
//! - events during a run: one pending rerun, a fresh comparison afterwards, and no run (with the
//!   "Ignored N changes ... (self-generated)" event) when the comparison finds the folders in sync,
//! - overflow, a root that goes away and comes back (with and without "Location becomes available"),
//! - excluded and staging paths, network roots, no polling, no duplicate runs from a flood,
//! - the merge rules of FER-525, resources released, and the source guard.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use super::test_support::*;
use super::{Host, HostClass, Recompute};
use crate::automation::model::{AutomationPolicy, Trigger, WatchTrigger};
use crate::automation::status::StatusKind;
use crate::automation::store::queue::{self, TriggerKind};
use crate::automation::store::runtime::{self, Attention, AttentionKind, TriggerState};
use crate::automation::store::{StoreError, automations};
use crate::automation::test_support::Fixture;
use crate::automation::watcher::RawEvent;

const SEC: i64 = 1_000;
const MIN: i64 = 60 * SEC;

/// One watch automation on a host, with an engine whose dry runs report `remaining` operations.
struct W {
    rig: Rig,
    host: Host<FakeDeps>,
    id: String,
    profile: String,
    left: String,
    remaining: Arc<AtomicUsize>,
}

fn watch(on_changes: bool, on_available: bool) -> AutomationPolicy {
    let mut p = Fixture::watch();
    p.trigger = Trigger::Watch(WatchTrigger {
        watch_left: true,
        watch_right: false,
        on_changes,
        on_available,
        idle_secs: 10,
        max_wait_secs: 900,
    });
    p
}

fn setup_with(policy: AutomationPolicy, exclude: Option<&str>) -> W {
    setup_full(policy, exclude, None)
}

fn setup_full(policy: AutomationPolicy, exclude: Option<&str>, include: Option<&str>) -> W {
    let rig = Rig::new();
    let remaining = Arc::new(AtomicUsize::new(0));
    let r = remaining.clone();
    rig.world.set_script(script(move |_, call| {
        if call.dry_run {
            dry_run(r.load(Ordering::SeqCst), 0)
        } else {
            r.store(0, Ordering::SeqCst);
            completed(3)
        }
    }));
    let (profile, mut revision) = rig.fx.profile("W");
    if let Some(pattern) = exclude {
        revision = rig.fx.bump_profile(&profile, revision, pattern);
    }
    if let Some(pattern) = include {
        rig.fx.bump_profile_include(&profile, revision, pattern);
    }
    let id = rig.fx.new_automation("W", &profile, policy);
    let left = rig.fx.profile_policy(&profile).left.path;
    let host = rig.host(1, HostClass::App);
    W {
        rig,
        host,
        id,
        profile,
        left,
        remaining,
    }
}

fn setup() -> W {
    setup_with(watch(true, true), None)
}

impl W {
    /// The first look: the host takes the role, compares afresh (WA-7.2) and goes to Watching.
    fn started(&self) {
        tick(&self.host);
        assert_eq!(self.state(), TriggerState::Watching, "start comparison");
    }

    fn emit(&self, n: usize) {
        self.rig.world.factory.emit_files(&self.left, n);
    }

    fn emit_path(&self, rel: &str) {
        self.rig.world.factory.emit(
            &self.left,
            RawEvent::Change(vec![format!("{}\\{rel}", self.left)]),
        );
    }

    fn state(&self) -> TriggerState {
        self.row().trigger_state.expect("known state")
    }

    fn row(&self) -> runtime::RuntimeRow {
        self.rig
            .fx
            .store
            .with_read(|c| runtime::get(c, &self.id))
            .expect("read")
            .expect("row")
    }

    fn kind(&self) -> StatusKind {
        self.rig.status_kind(&self.id)
    }

    fn events(&self) -> Vec<String> {
        self.rig.event_texts(&self.id)
    }

    fn has_event(&self, part: &str) -> bool {
        self.events().iter().any(|t| t.contains(part))
    }

    fn count_events(&self, part: &str) -> usize {
        self.events().iter().filter(|t| t.contains(part)).count()
    }

    fn advance(&self, ms: i64) {
        self.rig.advance(ms);
    }

    fn live(&self) -> usize {
        self.rig.world.factory.live()
    }
}

fn dry_calls(rig: &Rig) -> usize {
    rig.world.calls().iter().filter(|c| c.dry_run).count()
}

fn real_calls(rig: &Rig) -> usize {
    rig.world.calls().iter().filter(|c| !c.dry_run).count()
}

/// A run that blocks inside the engine until the test opens the gate.
struct Held {
    entered: Arc<AtomicBool>,
    gate: Arc<(Mutex<bool>, Condvar)>,
}

impl Held {
    fn install(w: &W) -> Held {
        Self::build(w, false)
    }

    /// The comparison (a dry run) blocks instead of the run.
    fn install_dry(w: &W) -> Held {
        Self::build(w, true)
    }

    fn build(w: &W, block_dry: bool) -> Held {
        let entered = Arc::new(AtomicBool::new(false));
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let (e, g, r) = (entered.clone(), gate.clone(), w.remaining.clone());
        w.rig.world.set_script(script(move |_, call| {
            if call.dry_run != block_dry {
                if call.dry_run {
                    return dry_run(r.load(Ordering::SeqCst), 0);
                }
                r.store(0, Ordering::SeqCst);
                return completed(3);
            }
            e.store(true, Ordering::SeqCst);
            let (lock, cv) = &*g;
            let mut open = lock.lock().unwrap();
            while !*open {
                open = cv.wait(open).unwrap();
            }
            if call.dry_run {
                return dry_run(r.load(Ordering::SeqCst), 0);
            }
            r.store(0, Ordering::SeqCst);
            completed(3)
        }));
        Held { entered, gate }
    }

    /// A gate with no script: the caller's script calls [`Held::block`].
    fn raw() -> Held {
        Held {
            entered: Arc::new(AtomicBool::new(false)),
            gate: Arc::new((Mutex::new(false), Condvar::new())),
        }
    }

    /// Called from inside a script: say so, and wait for [`Held::open`].
    fn block(&self) {
        self.entered.store(true, Ordering::SeqCst);
        let (lock, cv) = &*self.gate;
        let mut open = lock.lock().unwrap();
        while !*open {
            open = cv.wait(open).unwrap();
        }
    }

    /// Wait until the run is inside the engine (a channel-style wait, not a fixed sleep).
    fn wait_entered(&self) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while !self.entered.load(Ordering::SeqCst) {
            assert!(
                std::time::Instant::now() < deadline,
                "the run never started"
            );
            std::thread::yield_now();
        }
    }

    fn open(&self) {
        let (lock, cv) = &*self.gate;
        *lock.lock().unwrap() = true;
        cv.notify_all();
    }
}

// ── WA-1: the quiet countdown ──

#[test]
fn the_prototype_fixture_queues_exactly_one_run_at_sixteen_seconds() {
    let w = setup();
    w.started();
    let t0 = w.rig.now();
    w.emit(1); // t = 0
    tick(&w.host);
    assert_eq!(w.kind(), StatusKind::Quiet);
    w.advance(6 * SEC);
    w.emit(1); // t = 6
    tick(&w.host);
    w.advance(9 * SEC); // t = 15
    tick(&w.host);
    assert_eq!(real_calls(&w.rig), 0);
    assert_eq!(w.rig.queue_len(), 0);
    assert_eq!(
        w.kind(),
        StatusKind::Quiet,
        "10 s have not passed since t=6"
    );
    w.advance(SEC); // t = 16
    tick(&w.host);
    let runs = w.rig.runs();
    assert_eq!(runs.len(), 1, "a burst counts as one run");
    assert_eq!(runs[0].trigger_kind, "watch");
    assert_eq!(runs[0].started_ms, t0 + 16 * SEC);
    assert_eq!(real_calls(&w.rig), 1);
    assert_eq!(w.state(), TriggerState::Watching);
    // Nothing else is queued afterwards.
    w.advance(30 * SEC);
    tick(&w.host);
    assert_eq!(w.rig.runs().len(), 1);
}

#[test]
fn events_every_five_seconds_still_queue_one_run_at_the_maximum_wait() {
    let w = setup();
    w.started();
    let t0 = w.rig.now();
    let mut t = 0;
    loop {
        w.emit(1);
        w.host.tick();
        if t < 900 {
            assert_eq!(w.rig.queue_len(), 0, "t={t}");
            assert_eq!(real_calls(&w.rig), 0, "t={t}");
        }
        if t == 900 {
            break;
        }
        w.advance(5 * SEC);
        t += 5;
    }
    settle(&w.host);
    let runs = w.rig.runs();
    assert_eq!(runs.len(), 1, "one run at the maximum wait");
    assert_eq!(runs[0].started_ms, t0 + 15 * MIN);
    assert!(w.has_event("Waited the maximum 15 min"));
    // The countdown restarts after it (WA-1.4).
    w.advance(5 * SEC);
    w.emit(1);
    tick(&w.host);
    assert_eq!(w.kind(), StatusKind::Quiet);
    assert_eq!(w.rig.runs().len(), 1);
}

#[test]
fn a_flood_of_events_is_one_run() {
    let w = setup();
    w.started();
    w.emit(20_000);
    tick(&w.host);
    assert_eq!(w.kind(), StatusKind::Quiet);
    for _ in 0..5 {
        w.advance(SEC);
        w.emit(2_000);
        tick(&w.host);
    }
    w.advance(10 * SEC);
    tick(&w.host);
    assert_eq!(w.rig.runs().len(), 1);
    assert_eq!(real_calls(&w.rig), 1);
    assert_eq!(w.rig.queue_len(), 0);
}

// ── WA-3, WA-4: changes during a run, and the changes a run writes ──

#[test]
fn events_during_a_run_make_one_pending_rerun_and_a_recompare_in_sync_makes_no_run() {
    let w = setup();
    w.started();
    let held = Held::install(&w);
    w.rig
        .enqueue(&w.id, &w.profile, TriggerKind::RunNow, HostClass::App);
    w.host.tick();
    held.wait_entered();
    let dry_before = dry_calls(&w.rig);
    // Five events while the run holds the folders.
    w.advance(SEC);
    w.emit(5);
    w.host.tick();
    assert!(w.row().pending_rerun);
    // The automation's own run is going: the status line says so (the Pending chip is for another
    // automation's run on the same folders).
    assert_eq!(w.kind(), StatusKind::Running);
    assert_eq!(
        w.rig.status_sub(&w.id).as_deref(),
        Some("1 pending rerun after this")
    );
    assert!(w.has_event("5 file events during the active run \u{2192} 1 pending rerun"));
    // More events merge into the same pending rerun: no countdown, no second announcement.
    w.advance(SEC);
    w.emit(3);
    w.host.tick();
    assert_eq!(w.count_events("1 pending rerun"), 1);
    assert_ne!(w.state(), TriggerState::Quiet);
    assert_eq!(
        dry_calls(&w.rig),
        dry_before,
        "no comparison while the run works"
    );
    held.open();
    settle(&w.host);
    assert_eq!(w.rig.runs().len(), 1);
    // The run is over, but the folder gets a moment to settle before the comparison.
    w.advance(SEC);
    tick(&w.host);
    assert_eq!(dry_calls(&w.rig), dry_before);
    w.advance(2 * SEC);
    tick(&w.host);
    assert_eq!(dry_calls(&w.rig), dry_before + 1, "one fresh comparison");
    assert_eq!(w.rig.runs().len(), 1, "in sync: no run");
    assert_eq!(real_calls(&w.rig), 1);
    assert!(!w.row().pending_rerun);
    assert_eq!(w.state(), TriggerState::Watching);
    assert!(
        w.has_event("Ignored 8 changes written by run #1 (self-generated) \u{2014} no rerun"),
        "{:?}",
        w.events()
    );
}

#[test]
fn events_during_a_run_with_differences_afterwards_queue_one_rerun() {
    let w = setup();
    w.started();
    let held = Held::install(&w);
    w.rig
        .enqueue(&w.id, &w.profile, TriggerKind::RunNow, HostClass::App);
    w.host.tick();
    held.wait_entered();
    w.emit(2);
    w.host.tick();
    held.open();
    settle(&w.host);
    // Something really changed while the run worked.
    w.remaining.store(4, Ordering::SeqCst);
    w.advance(3 * SEC);
    tick(&w.host);
    assert_eq!(w.rig.runs().len(), 1, "the entry waits for its delay");
    let queued = w.rig.fx.store.with_read(queue::all).expect("queue");
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].kind, TriggerKind::PendingRerun);
    assert!(w.has_event("Fresh comparison found 4 differences"));
    w.advance(11 * SEC);
    tick(&w.host);
    assert_eq!(w.rig.runs().len(), 2);
    assert_eq!(w.rig.runs()[1].trigger_kind, "pendingRerun");
}

#[test]
fn events_after_a_run_ended_but_inside_the_grace_still_belong_to_it() {
    let w = setup();
    w.started();
    w.rig
        .enqueue(&w.id, &w.profile, TriggerKind::RunNow, HostClass::App);
    tick(&w.host); // the run starts and ends inside this tick
    assert_eq!(w.rig.runs().len(), 1);
    // The operating system delivers the run's own writes a moment later.
    w.advance(SEC);
    w.emit(6);
    tick(&w.host);
    assert_ne!(
        w.state(),
        TriggerState::Quiet,
        "no countdown for the run's own writes"
    );
    assert!(w.row().pending_rerun);
    w.advance(5 * SEC);
    tick(&w.host);
    assert_eq!(w.rig.runs().len(), 1);
    assert!(w.has_event("Ignored 6 changes written by run #1"));
}

#[test]
fn events_while_a_run_is_queued_are_merged_into_it_and_never_change_a_user_request() {
    let w = setup();
    w.started();
    // Run now waits (an unmet condition keeps the entry); then the folder changes.
    *w.rig.world.unmet.lock().unwrap() = Some("Waits for AC power");
    w.rig
        .enqueue(&w.id, &w.profile, TriggerKind::RunNow, HostClass::App);
    tick(&w.host);
    let before = w.rig.fx.store.with_read(queue::all).expect("queue");
    assert_eq!(before.len(), 1, "{:?} {:?}", w.events(), w.rig.runs());
    w.advance(5 * SEC);
    w.emit(3);
    tick(&w.host);
    let after = w.rig.fx.store.with_read(queue::all).expect("queue");
    assert_eq!(after.len(), 1, "one entry per profile");
    assert_eq!(
        after[0].kind,
        TriggerKind::RunNow,
        "FER-525: kind is the user's"
    );
    assert_eq!(
        after[0].enqueued_ms, before[0].enqueued_ms,
        "FER-525: epoch kept"
    );
    assert!(after[0].merged_count > before[0].merged_count);
    assert!(w.has_event("3 changes merged into the queued run"));
    // More events for the same queued entry do not write more lines (NIT-1).
    for _ in 0..3 {
        w.advance(SEC);
        w.emit(2);
        tick(&w.host);
    }
    assert_eq!(w.count_events("merged into the queued run"), 1);
    assert_ne!(w.state(), TriggerState::Quiet);
    // When the condition is met the user's request runs, as the user asked.
    *w.rig.world.unmet.lock().unwrap() = None;
    w.advance(6 * SEC);
    tick(&w.host);
    assert_eq!(w.rig.runs().len(), 1);
    assert_eq!(w.rig.runs()[0].trigger_kind, "runNow");
}

// ── WA-6: overflow ──

#[test]
fn overflow_needs_a_recheck_and_the_full_comparison_waits_for_the_run_to_end() {
    let w = setup();
    w.started();
    let held = Held::install(&w);
    w.rig
        .enqueue(&w.id, &w.profile, TriggerKind::RunNow, HostClass::App);
    w.host.tick();
    held.wait_entered();
    let dry_before = dry_calls(&w.rig);
    w.advance(SEC);
    w.rig.world.factory.emit(&w.left, RawEvent::Overflow);
    w.host.tick();
    assert_eq!(w.state(), TriggerState::Recheck);
    assert_eq!(
        w.kind(),
        StatusKind::Running,
        "the run's own chip comes first"
    );
    assert!(w.has_event("Watcher missed events \u{2014} full comparison when safe"));
    w.advance(5 * SEC);
    w.host.tick();
    assert_eq!(
        dry_calls(&w.rig),
        dry_before,
        "no comparison while a run uses the folders"
    );
    held.open();
    settle(&w.host);
    // Differences are found by the full comparison: ONE run is queued.
    w.remaining.store(2, Ordering::SeqCst);
    w.advance(3 * SEC);
    tick(&w.host);
    assert_eq!(dry_calls(&w.rig), dry_before + 1);
    let queued = w.rig.fx.store.with_read(queue::all).expect("queue");
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].kind, TriggerKind::Watch);
    tick(&w.host);
    assert_eq!(w.rig.runs().len(), 2);
}

#[test]
fn a_failed_watch_is_a_lost_watch() {
    let w = setup();
    w.started();
    w.rig.world.factory.emit(&w.left, RawEvent::Failed);
    tick(&w.host);
    // Nothing to wait for: the comparison runs, finds the folders in sync, and watching goes on.
    w.advance(2 * SEC);
    tick(&w.host);
    assert_eq!(w.state(), TriggerState::Watching);
    assert!(w.has_event("Watcher missed events"));
    assert_eq!(w.rig.runs().len(), 0);
}

/// FER-565: a watch whose worker reports that it is gone, while the folder is still the approved
/// one, is put back at once and the events it may have missed are compared.
#[test]
fn a_watch_that_reports_its_death_is_replaced_and_a_recheck_follows() {
    let w = setup();
    w.started();
    assert_eq!(w.live(), 1);
    let asked = w.rig.world.factory.asked().len();
    let dry = dry_calls(&w.rig);
    w.rig.world.factory.die(&w.left, true);
    w.host.tick();
    assert_eq!(
        w.rig.world.factory.asked().len(),
        asked + 1,
        "a new watch was asked for"
    );
    assert_eq!(w.live(), 1, "the dead one is gone, a live one is in place");
    assert!(w.has_event("Watcher missed events"));
    assert!(matches!(
        w.state(),
        TriggerState::Recheck | TriggerState::Checking
    ));
    w.advance(3 * SEC);
    tick(&w.host);
    assert_eq!(dry_calls(&w.rig), dry + 1, "then a full comparison");
    assert_eq!(w.state(), TriggerState::Watching);
    // The new watch works.
    w.emit(1);
    w.host.tick();
    assert_eq!(w.state(), TriggerState::Quiet);
}

/// FER-565: a watch that died without a word (its worker is gone, nothing was reported) is found
/// by the 30 s look and replaced, with a full comparison.
#[test]
fn a_watch_that_is_no_longer_alive_is_found_by_the_health_check() {
    let w = setup();
    w.started();
    let dry = dry_calls(&w.rig);
    w.rig.world.factory.die(&w.left, false);
    // Still held: the handle is there, only dead.
    assert_eq!(w.live(), 1);
    w.advance(31 * SEC);
    w.host.tick();
    assert!(w.has_event("Watcher restarted"));
    assert_eq!(w.live(), 1);
    assert!(matches!(
        w.state(),
        TriggerState::Recheck | TriggerState::Checking
    ));
    w.advance(3 * SEC);
    tick(&w.host);
    assert_eq!(dry_calls(&w.rig), dry + 1, "then a full comparison");
    assert_eq!(w.state(), TriggerState::Watching);
}

/// FER-565: the watch died because the folder is gone: no new watch, the folder is waited for.
#[test]
fn a_watch_that_dies_with_its_folder_waits_for_the_device() {
    let w = setup();
    w.started();
    w.rig.world.set_offline(&w.left, true);
    w.rig.world.factory.die(&w.left, true);
    w.host.tick();
    assert_eq!(w.state(), TriggerState::Device);
    assert_eq!(w.live(), 0);
    w.rig.world.set_offline(&w.left, false);
    w.host.volume_changed();
    tick(&w.host);
    assert_eq!(w.state(), TriggerState::Watching);
    assert_eq!(w.live(), 1);
}

/// S3: a watch that dies soon after it was made is not put back in a loop. The first quick death
/// is replaced at once, the second waits for the backoff and the 30 s look, the third makes the
/// folder Unsupported. A watch that lived long enough starts the count again.
#[test]
fn a_watch_that_keeps_dying_at_once_is_not_rebuilt_in_a_loop() {
    let w = setup();
    w.started();
    w.advance(20 * SEC);
    // Dies after a while: replaced at once, and it is not counted as quick.
    w.rig.world.factory.die(&w.left, true);
    w.host.tick();
    assert_eq!(w.live(), 1);
    // First quick death: replaced at once.
    let asked = w.rig.world.factory.asked().len();
    w.advance(SEC);
    w.rig.world.factory.die(&w.left, true);
    w.host.tick();
    assert_eq!(w.live(), 1, "the first quick death is replaced");
    assert_eq!(w.rig.world.factory.asked().len(), asked + 1);
    // Second quick death: no new watch now.
    w.advance(SEC);
    w.rig.world.factory.die(&w.left, true);
    w.host.tick();
    assert_eq!(w.live(), 0, "the second one waits");
    assert_eq!(w.rig.world.factory.asked().len(), asked + 1);
    w.advance(2 * SEC);
    w.host.tick();
    assert_eq!(w.rig.world.factory.asked().len(), asked + 1, "no loop");
    // After the backoff the comparison runs, and the 30 s look makes the watch again.
    let mut rebuilt = false;
    for _ in 0..12 {
        w.advance(10 * SEC);
        tick(&w.host);
        if w.live() == 1 {
            rebuilt = true;
            break;
        }
    }
    assert!(rebuilt, "the look put the watch back");
    // Third quick death in a row: Unsupported, with a plain text.
    w.rig.world.factory.die(&w.left, true);
    w.host.tick();
    assert_eq!(w.state(), TriggerState::Unsupported);
    assert!(w.has_event("keep stopping"));
    assert_eq!(w.live(), 0);
}

/// FER-565 follow-up: a watch that stays alive for more than the quick-death window starts the
/// count again, even when no death follows it. Without that, two quick deaths far apart in time
/// (a watch made again after the folder came back, dying at once) would add up to Unsupported.
#[test]
fn a_watch_that_lived_long_enough_resets_the_quick_death_count() {
    let w = setup();
    w.started();
    w.advance(20 * SEC);
    // A first quick death: replaced at once (count 1).
    w.rig.world.factory.die(&w.left, true);
    w.host.tick();
    w.advance(SEC);
    w.rig.world.factory.die(&w.left, true);
    w.host.tick();
    assert_eq!(w.live(), 1, "the first quick death is replaced");
    // The new watch lives well past the window; a pass sees it alive and resets the count.
    w.advance(15 * SEC);
    w.host.tick();
    for _ in 0..12 {
        if w.state() == TriggerState::Watching {
            break;
        }
        w.advance(5 * SEC);
        tick(&w.host);
    }
    assert_eq!(w.live(), 1);
    assert_eq!(
        w.state(),
        TriggerState::Watching,
        "the comparison after the death is done"
    );
    // The watch dies without a word much later; the 30 s look makes it again.
    let asked = w.rig.world.factory.asked().len();
    w.rig.world.factory.die(&w.left, false);
    w.advance(31 * SEC);
    w.host.tick();
    assert_eq!(w.rig.world.factory.asked().len(), asked + 1, "made again");
    assert_eq!(w.live(), 1);
    // And that one dies at once.
    w.advance(SEC);
    w.rig.world.factory.die(&w.left, true);
    w.host.tick();
    // Counted as the FIRST quick death again: replaced at once, not held back by a backoff.
    assert_eq!(w.live(), 1, "the count started again");
    assert_ne!(w.state(), TriggerState::Unsupported);
}

// ── WA-5: offline roots ──

#[test]
fn a_root_that_goes_away_waits_for_the_device_and_comes_back_with_a_comparison() {
    let w = setup();
    w.started();
    assert_eq!(w.live(), 1);
    let dry_before = dry_calls(&w.rig);
    w.rig.world.set_offline(&w.left, true);
    w.host.volume_changed();
    tick(&w.host);
    assert_eq!(w.state(), TriggerState::Device);
    assert_eq!(w.kind(), StatusKind::Device);
    assert!(w.has_event(&format!("{} not connected \u{2014} no sync", w.left)));
    assert_eq!(
        w.live(),
        0,
        "the watch is released while the folder is away"
    );
    // Events that arrive meanwhile mean nothing; no comparison, no run, nothing deleted.
    assert_eq!(dry_calls(&w.rig), dry_before);
    assert_eq!(w.rig.runs().len(), 0);
    // WA-5.3: while the device is missing the folder is opened (identity only) at most every
    // 30 s; there is no tighter timer, and nothing is listed.
    let count = |w: &W| {
        w.rig
            .world
            .probes()
            .iter()
            .filter(|p| p.eq_ignore_ascii_case(&w.left))
            .count()
    };
    let mut seen = count(&w);
    let mut stamps = Vec::new();
    for _ in 0..40 {
        w.advance(15 * SEC);
        w.host.tick();
        let n = count(&w);
        if n > seen {
            stamps.push(w.rig.now());
            seen = n;
        }
    }
    assert!(
        stamps.len() >= 15 && stamps.len() <= 21,
        "{} probes",
        stamps.len()
    );
    for pair in stamps.windows(2) {
        assert!(pair[1] - pair[0] >= 30 * SEC, "probed twice within 30 s");
    }
    assert_eq!(w.state(), TriggerState::Device);
    // It is back (the volume arrival tells the host).
    w.rig.world.set_offline(&w.left, false);
    w.host.volume_changed();
    tick(&w.host);
    assert_eq!(w.state(), TriggerState::Watching);
    assert_eq!(dry_calls(&w.rig), dry_before + 1, "compare first");
    assert_eq!(w.rig.runs().len(), 0, "in sync: no run");
    assert!(w.has_event("available again \u{2192} fresh comparison before any run"));
    assert_eq!(w.live(), 1, "watching again");
}

#[test]
fn a_root_that_comes_back_with_differences_queues_one_run() {
    let w = setup();
    w.started();
    w.rig.world.set_offline(&w.left, true);
    w.host.volume_changed();
    tick(&w.host);
    w.remaining.store(3, Ordering::SeqCst);
    w.rig.world.set_offline(&w.left, false);
    w.host.volume_changed();
    tick(&w.host);
    tick(&w.host);
    let runs = w.rig.runs();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].trigger_kind, "available");
}

#[test]
fn without_location_becomes_available_the_watch_just_goes_on() {
    let w = setup_with(watch(true, false), None);
    w.started();
    w.rig.world.set_offline(&w.left, true);
    w.host.volume_changed();
    tick(&w.host);
    assert_eq!(w.state(), TriggerState::Device);
    let dry_before = dry_calls(&w.rig);
    w.remaining.store(3, Ordering::SeqCst);
    w.rig.world.set_offline(&w.left, false);
    w.host.volume_changed();
    tick(&w.host);
    assert_eq!(w.state(), TriggerState::Watching);
    assert_eq!(
        dry_calls(&w.rig),
        dry_before,
        "no comparison without the trigger"
    );
    assert_eq!(w.rig.runs().len(), 0);
    assert_eq!(w.live(), 1);
}

#[test]
fn a_device_that_is_missing_when_the_host_starts_is_waited_for() {
    let w = setup();
    w.rig.world.set_offline(&w.left, true);
    tick(&w.host);
    assert_eq!(w.state(), TriggerState::Device);
    assert_eq!(dry_calls(&w.rig), 0, "never compared as if empty");
    assert_eq!(w.live(), 0);
}

// ── WA-2.5: what does not count ──

#[test]
fn events_on_excluded_and_staging_paths_do_not_start_the_countdown() {
    let w = setup_with(watch(true, true), Some("*.tmp"));
    w.started();
    w.emit_path("scratch.tmp");
    w.emit_path("deep\\er\\scratch.tmp");
    w.emit_path(".a.txt.ferrum-stage-12-1");
    w.emit_path("sub\\.b.txt.ferrum-backup-12-2");
    w.emit_path("$RECYCLE.BIN\\S-1\\x");
    w.rig.world.factory.emit(
        &w.left,
        RawEvent::Change(vec!["D:\\Elsewhere\\x.txt".to_string()]),
    );
    tick(&w.host);
    assert_eq!(w.state(), TriggerState::Watching);
    assert_eq!(w.kind(), StatusKind::Watching);
    // The event that counts does.
    w.emit_path("real.txt");
    tick(&w.host);
    assert_eq!(w.kind(), StatusKind::Quiet);
}

// ── WA-7: start and resume ──

#[test]
fn a_host_start_and_a_resume_from_sleep_each_compare_afresh() {
    let w = setup();
    tick(&w.host);
    assert_eq!(dry_calls(&w.rig), 1, "one comparison at start");
    assert!(w.has_event("Ferrum started"));
    w.advance(5 * SEC);
    w.host.recompute_schedules(Recompute::Resumed);
    tick(&w.host);
    assert_eq!(dry_calls(&w.rig), 2, "one more after the PC woke up");
    assert!(w.has_event("PC resumed from sleep"));
    assert_eq!(w.state(), TriggerState::Watching);
    assert_eq!(w.rig.runs().len(), 0);
}

#[test]
fn start_with_differences_queues_one_run_through_the_gate() {
    let w = setup();
    w.remaining.store(2, Ordering::SeqCst);
    tick(&w.host);
    tick(&w.host);
    let runs = w.rig.runs();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].trigger_kind, "startup");
    let calls = w.rig.world.calls();
    let real = calls.iter().find(|c| !c.dry_run).expect("a real run");
    assert!(real.holds_lease && real.lease_covers && real.verify_loads_before > 0);
}

#[test]
fn the_comparison_is_read_only_and_takes_no_lease() {
    let w = setup();
    w.started();
    let dry: Vec<_> = w
        .rig
        .world
        .calls()
        .into_iter()
        .filter(|c| c.dry_run)
        .collect();
    assert_eq!(dry.len(), 1);
    assert!(!dry[0].holds_lease, "a comparison holds no lease");
    assert!(matches!(dry[0].mode, super::RunMode::Compare));
    assert!(
        dry[0].verify_loads_before > 0,
        "the gate verified the approval"
    );
}

// ── Network roots, backends that refuse ──

#[test]
fn a_backend_that_refuses_makes_the_automation_unsupported_and_nothing_polls() {
    let w = setup();
    w.rig.world.factory.refuse.store(true, Ordering::SeqCst);
    tick(&w.host);
    assert_eq!(w.state(), TriggerState::Unsupported);
    assert_eq!(w.kind(), StatusKind::Unsupported);
    assert!(w.has_event("can't be watched"));
    for _ in 0..20 {
        w.advance(MIN);
        w.host.tick();
    }
    assert_eq!(w.rig.runs().len(), 0);
    assert_eq!(dry_calls(&w.rig), 0);
    assert_eq!(
        w.rig.world.factory.asked().len(),
        1,
        "asked once, never again"
    );
}

#[test]
fn only_the_watched_local_root_is_given_to_the_factory() {
    let w = setup();
    w.started();
    let asked = w.rig.world.factory.asked();
    assert_eq!(asked.len(), 1);
    assert!(asked[0].eq_ignore_ascii_case(&w.left));
}

#[test]
fn a_watch_that_only_waits_for_the_device_starts_no_watcher() {
    let w = setup_with(watch(false, true), None);
    w.started();
    assert!(w.rig.world.factory.asked().is_empty());
    assert_eq!(w.live(), 0);
}

// ── Resources (the watcher is released whenever the automation is not watching) ──

fn sql_bump(w: &W, sql: &str) {
    w.rig
        .fx
        .store
        .with_write(|c| -> Result<(), StoreError> {
            c.execute_batch(sql).map_err(|_| StoreError::Unavailable)?;
            automations::bump_change_seq(c)
        })
        .expect("write");
}

#[test]
fn the_watcher_is_released_when_the_automation_is_paused_held_or_deleted() {
    let w = setup();
    w.started();
    assert_eq!(w.live(), 1);

    // Hold (needs attention): released; the resolved hold brings it back with a comparison.
    w.rig
        .fx
        .store
        .with_write(|c| -> Result<(), StoreError> {
            runtime::set_attention(
                c,
                &w.id,
                Some(&Attention {
                    kind: AttentionKind::Failed,
                    run_id: 1,
                    text: "1 failed".into(),
                }),
                w.rig.now(),
            )?;
            automations::bump_change_seq(c)
        })
        .expect("hold");
    w.advance(3 * SEC);
    tick(&w.host);
    assert_eq!(w.live(), 0, "held");
    w.emit(3);
    tick(&w.host);
    assert_eq!(w.kind(), StatusKind::Attention);
    assert_eq!(
        w.rig.runs().len(),
        0,
        "events cannot start anything while held"
    );
    w.rig
        .fx
        .store
        .with_write(|c| -> Result<(), StoreError> {
            runtime::set_attention(c, &w.id, None, w.rig.now())?;
            automations::bump_change_seq(c)
        })
        .expect("resolve");
    w.advance(3 * SEC);
    tick(&w.host);
    assert_eq!(w.live(), 1, "watching again");
    assert!(w.has_event("fresh comparison"));

    // Paused (disabled): released.
    sql_bump(
        &w,
        &format!("UPDATE automations SET enabled = 0 WHERE id = '{}'", w.id),
    );
    w.advance(3 * SEC);
    tick(&w.host);
    assert_eq!(w.live(), 0, "disabled");
    sql_bump(
        &w,
        &format!("UPDATE automations SET enabled = 1 WHERE id = '{}'", w.id),
    );
    w.advance(3 * SEC);
    tick(&w.host);
    assert_eq!(w.live(), 1, "enabled again");

    // Deleted: released.
    sql_bump(
        &w,
        &format!("DELETE FROM automations WHERE id = '{}'", w.id),
    );
    w.advance(3 * SEC);
    tick(&w.host);
    assert_eq!(w.live(), 0, "deleted");
}

#[test]
fn the_watcher_is_released_at_shutdown_and_when_another_process_hosts() {
    let w = setup();
    w.started();
    assert_eq!(w.live(), 1);
    w.host.shutdown(std::time::Duration::from_secs(2));
    assert_eq!(w.live(), 0, "no handle outlives the host");

    // A process that does not host holds no watcher at all.
    let w2 = setup();
    let other = w2.rig.host(2, HostClass::App);
    other.tick();
    assert!(other.is_hosting());
    let second = w2.rig.host(3, HostClass::App);
    tick(&second);
    assert!(!second.is_hosting());
    assert_eq!(w2.rig.world.factory.asked().len(), 1, "only the host asked");
}

// ── Source guards ──

fn code_of(path: &str) -> String {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(path);
    let text = std::fs::read_to_string(path).expect("source");
    text.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The hook only queues and the watcher only counts: neither has a way to the gate, the proof,
/// the engine or a user's request.
#[test]
fn the_watcher_and_its_hook_have_no_way_to_the_engine() {
    for file in [
        "src/automation/watcher.rs",
        "src/automation/rdcw.rs",
        "src/automation/host/watch.rs",
    ] {
        let code = code_of(file);
        // The test module of watcher.rs is not part of the guard.
        let code = code.split("#[cfg(test)]").next().unwrap_or("").to_string();
        // The profile's filters and the root helpers live in `sync_engine`; the ENGINE (the host's
        // `engine` module, `execute::run`) is what must stay out of reach.
        let code = code
            .replace("sync_engine::filters", "")
            .replace("sync_engine::roots", "");
        for needle in [
            "run_gated",
            "GateKind",
            "gate(",
            "GateCtx",
            "run_engine",
            "engine::",
            "execute::",
            "GateOk",
            "dry_run",
            "TriggerKind::RunNow",
            "TriggerKind::Retry",
            "KeyedAuth",
            "EngineRequest",
        ] {
            assert!(!code.contains(needle), "{file} mentions {needle}");
        }
    }
}

/// No code path lists a folder: the availability probe opens it, change events come from the
/// factory, and no timer reads a directory.
#[test]
fn nothing_in_the_watch_code_lists_a_directory() {
    for file in [
        "src/automation/watcher.rs",
        "src/automation/rdcw.rs",
        "src/automation/host/watch.rs",
    ] {
        let code = code_of(file);
        let code = code.split("#[cfg(test)]").next().unwrap_or("").to_string();
        for needle in [
            "read_dir",
            "WalkDir",
            "walkdir",
            "std::fs::",
            "fs::metadata",
            "FindFirstFile",
            "PollWatcher",
            "poll_interval",
            "thread::sleep",
        ] {
            assert!(!code.contains(needle), "{file} mentions {needle}");
        }
    }
}

/// The hook queues only the kinds a trigger source may queue.
#[test]
fn the_hook_queues_only_trigger_kinds() {
    // `request::trigger_tx` refuses every user kind, whatever the caller passes.
    let rig = Rig::new();
    let (a, _) = rig.automation("Kinds");
    let keys = rig.fx.keys.clone();
    let now = rig.now();
    for kind in [
        TriggerKind::RunNow,
        TriggerKind::Retry,
        TriggerKind::Recheck,
    ] {
        let answer = rig
            .fx
            .store
            .with_write(|c| -> Result<_, StoreError> {
                Ok(super::request::trigger_tx(
                    c,
                    keys.as_ref(),
                    &a,
                    kind,
                    "x",
                    now,
                ))
            })
            .expect("write");
        assert!(
            matches!(answer, Err(super::request::RequestError::Invalid)),
            "{kind:?} was accepted"
        );
    }
    assert_eq!(rig.queue_len(), 0);
}

// ── A share that is not watched: its availability is the one thing that is looked at on a timer ──

/// A probe that knows `\nas\backup\...` as a reachable share and leaves local folders to the file
/// system, so a profile with a network destination can be saved and approved in a test.
struct ShareProbe;

impl crate::sync_engine::roots::RootProbe for ShareProbe {
    fn canonical(&self, path: &str) -> Result<String, crate::sync_engine::roots::RootError> {
        if path.starts_with(r"\\") {
            Ok(path.to_string())
        } else {
            crate::sync_engine::roots::FsProbe.canonical(path)
        }
    }

    fn identity(
        &self,
        canonical: &str,
    ) -> Result<crate::automation::model::RootIdentity, crate::sync_engine::roots::RootError> {
        if canonical.starts_with(r"\\") {
            Ok(crate::automation::model::RootIdentity::Network {
                unc_prefix: crate::sync_engine::roots::unc_prefix(canonical),
                volume_serial: Some("1".into()),
                file_id: Some("2".into()),
            })
        } else {
            crate::sync_engine::roots::FsProbe.identity(canonical)
        }
    }
}

/// A watch automation on a local folder that syncs to `\nas\backup\proj` (approved with the
/// share probe). Returns `(rig, host, automation id, local root, share root)`.
fn share_automation(rig: &Rig, n: usize) -> (String, String, String) {
    use crate::automation::commands::{self, SaveRequest, SaveResultDto};
    use crate::automation::model::{
        Draft, DraftGuard, PolicyDraft, WireCompare, WireDeletePolicy, WireMode, WireSide,
    };
    let probe: Arc<dyn crate::sync_engine::roots::RootProbe> = Arc::new(ShareProbe);
    let left = rig.fx.tmp.path().join(format!("share-left-{n}"));
    std::fs::create_dir_all(&left).expect("left");
    let share = format!(r"\\nas\backup\proj{n}");
    let draft = Draft {
        name: format!("Share {n}"),
        policy: PolicyDraft {
            left: left.to_string_lossy().into_owned(),
            right: share.clone(),
            mode: WireMode::OneWay,
            from: Some(WireSide::Left),
            propagate_deletes: None,
            delete_policy: WireDeletePolicy::Permanent,
            versioning_root: None,
            compare: WireCompare::SizeAndDate,
            include: vec![],
            exclude: vec![],
            guard: DraftGuard {
                enabled: true,
                max_count: 50,
                max_percent: 20,
            },
        },
    };
    let saved = commands::save_in(
        &rig.fx.store,
        SaveRequest {
            id: None,
            expected_revision: None,
            draft,
        },
        &probe,
        |_| true,
        rig.now(),
    )
    .expect("save");
    let SaveResultDto::Saved { profile } = saved else {
        panic!("expected saved");
    };
    let auto = Fixture::auto_draft(&format!("Share {n}"), &profile.id, watch(true, true));
    let dry =
        |_: &crate::automation::model::ProfilePolicy| crate::automation::test_run::DryRun::Done {
            summary: crate::sync_engine::plan::PlanSummary::default(),
            conflicts: 0,
            guard: crate::sync_engine::opset::GuardVerdict::Ok,
        };
    let access = |_: &str, _: bool| true;
    let deps = crate::automation::test_run::TestDeps {
        probe: &probe,
        budget: std::time::Duration::from_secs(2),
        dry_run: &dry,
        access: &access,
        background_ready: false,
    };
    let test = crate::automation::approval::test_in(
        &rig.fx.store,
        &rig.fx.ledger,
        crate::automation::approval::TestRequest {
            id: None,
            draft: auto.clone(),
        },
        &deps,
        rig.fx.clock(),
    )
    .expect("test");
    assert!(test.passed, "{:?}", test.steps);
    let id = match rig.fx.approve(None, None, &auto, &test.test_id, true) {
        crate::automation::dto::ApproveResultDto::Approved { automation } => automation.id,
        other => panic!("expected approved, got {other:?}"),
    };
    let local = rig.fx.profile_policy(&profile.id).left.path;
    (id, local, share)
}

/// One automation with a share destination, on a rig and host of its own.
fn share_destination() -> (Rig, Host<FakeDeps>, String, String, String) {
    let rig = Rig::new();
    let (id, local, share) = share_automation(&rig, 0);
    let host = rig.host(1, HostClass::App);
    (rig, host, id, local, share)
}

#[test]
fn an_offline_share_is_probed_at_most_every_thirty_seconds_and_never_listed() {
    let (rig, host, id, local, share) = share_destination();
    rig.world.set_offline(&share, true);
    tick(&host);
    let row = rig
        .fx
        .store
        .with_read(|c| runtime::get(c, &id))
        .expect("read")
        .expect("row");
    assert_eq!(row.trigger_state, Some(TriggerState::Device));
    assert!(
        rig.event_texts(&id)
            .iter()
            .any(|t| t.contains(&format!("{share} not connected \u{2014} no sync")))
    );
    // Only the watched LOCAL folder could have a watch, and none is held while the share is away.
    assert!(rig.world.factory.asked().is_empty());
    assert_eq!(rig.world.factory.live(), 0);
    let share_probes = |rig: &Rig| {
        rig.world
            .probes()
            .iter()
            .filter(|p| p.eq_ignore_ascii_case(&share))
            .count()
    };
    let local_probes = |rig: &Rig| {
        rig.world
            .probes()
            .iter()
            .filter(|p| p.eq_ignore_ascii_case(&local))
            .count()
    };
    let (share0, local0) = (share_probes(&rig), local_probes(&rig));
    assert_eq!(share0, 1);
    // Five minutes of ticks, one every 5 s: the share and the local folder are asked at most
    // every 30 s.
    let mut stamps = vec![rig.now()];
    let mut seen = share0;
    for _ in 0..60 {
        rig.advance(5 * SEC);
        host.tick();
        let n = share_probes(&rig);
        if n > seen {
            stamps.push(rig.now());
            seen = n;
        }
    }
    assert!((9..=11).contains(&seen), "{seen} probes in 5 minutes");
    for pair in stamps.windows(2) {
        assert!(pair[1] - pair[0] >= 30 * SEC, "probed twice within 30 s");
    }
    // The local folder is opened on the same 30 s cadence, not more often.
    assert!(
        local_probes(&rig) - local0 <= 11,
        "local folder probed too often"
    );
    // Nothing was compared or run meanwhile, and the engine never saw the share.
    assert_eq!(rig.world.calls().len(), 0);
    assert_eq!(rig.runs().len(), 0);
}

#[test]
fn a_share_that_is_back_is_compared_before_anything_runs() {
    let (rig, host, id, _local, share) = share_destination();
    rig.world.set_offline(&share, true);
    tick(&host);
    rig.world.set_offline(&share, false);
    rig.advance(31 * SEC);
    host.tick();
    settle(&host);
    let texts = rig.event_texts(&id);
    assert!(
        texts
            .iter()
            .any(|t| t.contains("available again \u{2192} fresh comparison before any run")),
        "{texts:?}"
    );
    assert_eq!(rig.runs().len(), 0, "nothing runs without a comparison");
}

// ── The read-only comparison is a gate mode, not a way around the gate ──

#[test]
fn the_compare_gate_takes_no_lease_and_refuses_a_held_or_paused_automation() {
    use super::{GateCtx, GateKind, GateRefusal, RunMode, gate};
    let w = setup();
    let env = crate::sync_engine::test_support::test_env();
    let lease_env = w.rig.lease_env(7);
    let unmet = |_: &crate::automation::model::Conditions| None;
    let ctx = GateCtx {
        store: &w.rig.fx.store,
        keys: w.rig.fx.keys.as_ref(),
        class: HostClass::App,
        lease_env: &lease_env,
        env: &env,
        unmet_condition: &unmet,
    };
    let ok = gate(&ctx, &w.id, GateKind::Compare).expect("a healthy automation compares");
    assert!(ok.lease().is_none(), "a comparison holds no lease");
    assert!(matches!(ok.mode(), RunMode::Compare));
    drop(ok);

    // A hold is NOT resolved by a comparison (a recheck of the run it names is).
    let set = |a: Option<&Attention>| {
        w.rig
            .fx
            .store
            .with_write(|c| runtime::set_attention(c, &w.id, a, w.rig.now()))
            .expect("write");
    };
    set(Some(&Attention {
        kind: AttentionKind::Failed,
        run_id: 1,
        text: "1 failed".into(),
    }));
    assert!(matches!(
        gate(&ctx, &w.id, GateKind::Compare),
        Err(GateRefusal::Held)
    ));
    set(None);
    // A paused automation compares nothing either (unlike a recheck, which may look).
    sql_bump(
        &w,
        &format!("UPDATE automations SET enabled = 0 WHERE id = '{}'", w.id),
    );
    assert!(matches!(
        gate(&ctx, &w.id, GateKind::Compare),
        Err(GateRefusal::Disabled)
    ));
    // The approval is verified: a key that is gone refuses.
    sql_bump(
        &w,
        &format!("UPDATE automations SET enabled = 1 WHERE id = '{}'", w.id),
    );
    w.rig
        .fx
        .memory
        .as_ref()
        .expect("memory keys")
        .set(crate::automation::dpapi::KeyLoad::Missing);
    assert!(matches!(
        gate(&ctx, &w.id, GateKind::Compare),
        Err(GateRefusal::NeedsReview(_))
    ));
}

// ── Review R1 ──

/// S1: changes noted while the comparison ran are not thrown away when it says "in sync".
#[test]
fn changes_noted_during_a_comparison_start_a_countdown_when_it_finds_sync() {
    let w = setup();
    let held = Held::install_dry(&w);
    w.host.tick(); // the start comparison begins and blocks inside the engine
    held.wait_entered();
    assert_eq!(w.state(), TriggerState::Checking);
    w.advance(SEC);
    w.emit(4);
    w.host.tick();
    assert!(w.has_event("4 changes noted"));
    assert_eq!(
        w.state(),
        TriggerState::Checking,
        "no countdown while comparing"
    );
    held.open();
    settle(&w.host);
    // The comparison saw the folders in sync, but the walk may have passed the changes already.
    assert_eq!(w.state(), TriggerState::Quiet);
    assert_eq!(w.kind(), StatusKind::Quiet);
    assert_eq!(w.rig.runs().len(), 0);
    w.advance(SEC);
    tick(&w.host);
    assert_eq!(
        w.state(),
        TriggerState::Quiet,
        "the idle delay has not passed"
    );
    w.advance(9 * SEC);
    tick(&w.host);
    assert_eq!(w.state(), TriggerState::Quiet);
    w.advance(2 * SEC);
    tick(&w.host);
    let runs = w.rig.runs();
    assert_eq!(runs.len(), 1, "one run, which compares again");
    assert_eq!(runs[0].trigger_kind, "watch");
    // And a comparison with nothing noted still goes straight to Watching.
    let w2 = setup();
    w2.started();
    assert_eq!(w2.state(), TriggerState::Watching);
}

/// S2: with an include list, renaming a folder is one event for the folder itself.
#[test]
fn a_renamed_folder_counts_even_when_the_profile_includes_only_markdown() {
    let w = setup_full(watch(true, true), None, Some("*.md"));
    w.started();
    w.emit_path("docs-old");
    tick(&w.host);
    assert_eq!(w.kind(), StatusKind::Quiet);
}

/// S3b: the answer of a comparison started for a released watcher does not touch the row of the
/// next one (edited and approved again, paused and resumed).
#[test]
fn a_comparison_of_a_released_watcher_cannot_overwrite_the_next_generation() {
    let w = setup();
    let first = Arc::new(Held::raw());
    let second = Arc::new(Held::raw());
    let calls = Arc::new(AtomicUsize::new(0));
    let (f, s2, c) = (first.clone(), second.clone(), calls.clone());
    w.rig.world.set_script(script(move |_, call| {
        if call.dry_run {
            let n = c.fetch_add(1, Ordering::SeqCst);
            match n {
                0 => f.block(),
                1 => s2.block(),
                _ => {}
            }
            return dry_run(0, 0);
        }
        completed(3)
    }));
    w.host.tick(); // comparison of generation 1 blocks
    first.wait_entered();
    super::watch::release_all(&w.host.inner);
    w.advance(SEC);
    w.host.tick(); // generation 2 is made and compares
    second.wait_entered();
    assert_eq!(w.state(), TriggerState::Checking);
    let events_before = w.events().len();
    // The old comparison ends: it says "in sync", and writes nothing.
    first.open();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while w.host.inner.lock().comparing > 1 {
        assert!(
            std::time::Instant::now() < deadline,
            "old comparison never ended"
        );
        std::thread::yield_now();
    }
    assert_eq!(
        w.state(),
        TriggerState::Checking,
        "generation 2 still compares"
    );
    assert_eq!(w.events().len(), events_before);
    second.open();
    settle(&w.host);
    assert_eq!(w.state(), TriggerState::Watching);
    assert_eq!(w.live(), 1);
}

/// S3a: a watching automation's local roots are opened now and then; a root that is gone or is
/// not the approved folder any more is found without any event, and a watch that died is put back.
#[test]
fn the_health_check_finds_a_vanished_root_and_a_dead_watch() {
    let w = setup();
    w.started();
    let asked = w.rig.world.probes().len();
    for _ in 0..5 {
        w.advance(5 * SEC);
        w.host.tick();
    }
    assert_eq!(w.rig.world.probes().len(), asked, "not within 30 s");
    // The folder is replaced behind the watcher's back: no volume event, no file event.
    w.rig.world.set_offline(&w.left, true);
    w.advance(10 * SEC);
    w.host.tick();
    assert_eq!(w.state(), TriggerState::Device);
    assert!(w.has_event("not connected"));
    assert_eq!(w.live(), 0, "the dead watch is released");
    w.rig.world.set_offline(&w.left, false);
    w.host.volume_changed();
    tick(&w.host);
    assert_eq!(w.state(), TriggerState::Watching);
    assert_eq!(w.live(), 1, "a watch is put back");

    // A watch that died while the folder is still the approved one.
    super::watch::kill_watches(&w.host.inner, &w.id);
    assert_eq!(w.live(), 0);
    w.advance(31 * SEC);
    w.host.tick();
    assert!(w.has_event("Watcher restarted"));
    assert_eq!(w.live(), 1);
    let dry = dry_calls(&w.rig);
    w.advance(3 * SEC);
    tick(&w.host);
    assert_eq!(dry_calls(&w.rig), dry + 1, "then a full comparison");
    assert_eq!(w.state(), TriggerState::Watching);
}

/// S4: many automations waiting for an offline share do not hold the host loop up.
#[test]
fn twenty_offline_shares_do_not_stall_a_pass() {
    let rig = Rig::new();
    let ids: Vec<(String, String, String)> = (0..20).map(|n| share_automation(&rig, n)).collect();
    for (_, _, share) in &ids {
        rig.world.set_offline(share, true);
    }
    rig.world.probe_delay_ms.store(150, Ordering::SeqCst);
    let host = rig.host(1, HostClass::App);
    let device = |rig: &Rig| {
        ids.iter()
            .filter(|(id, _, _)| {
                rig.fx
                    .store
                    .with_read(|c| runtime::get(c, id))
                    .ok()
                    .flatten()
                    .and_then(|r| r.trigger_state)
                    == Some(TriggerState::Device)
            })
            .count()
    };
    let mut slowest = std::time::Duration::ZERO;
    for _ in 0..40 {
        rig.advance(SEC);
        let started = std::time::Instant::now();
        host.tick();
        slowest = slowest.max(started.elapsed());
        if device(&rig) == 20 {
            break;
        }
    }
    assert!(
        slowest < std::time::Duration::from_millis(2_500),
        "a pass took {slowest:?}"
    );
    assert_eq!(
        device(&rig),
        20,
        "all of them get there, spread over passes"
    );
    // The host's own beat is on time: its row is fresh.
    let row = rig.host_rows().into_iter().next().expect("host row");
    assert!(rig.now() - row.last_beat_ms <= 5_000);
}

/// NIT-2: with every comparison slot taken the hook does not write every second.
#[test]
fn a_full_comparison_slot_does_not_make_the_hook_write_every_second() {
    let w = setup();
    w.host.inner.lock().comparing = 2;
    w.host.tick();
    let before = w
        .rig
        .fx
        .store
        .with_read(automations::change_seq)
        .expect("seq");
    for _ in 0..4 {
        w.advance(SEC);
        w.host.tick();
    }
    let after = w
        .rig
        .fx
        .store
        .with_read(automations::change_seq)
        .expect("seq");
    assert!(after - before <= 1, "{} writes in 4 s", after - before);
    w.host.inner.lock().comparing = 0;
    tick(&w.host);
    assert_eq!(
        w.state(),
        TriggerState::Watching,
        "and it goes on when a slot is free"
    );
}

/// NIT-4: a watch that keeps finding differences right after its own runs stops chaining them.
#[test]
fn chained_pending_reruns_are_bounded_like_the_engines() {
    let w = setup();
    w.started();
    let factory = w.rig.world.factory.clone();
    let left = w.left.clone();
    w.rig.world.set_script(script(move |_, call| {
        if call.dry_run {
            // Always something left to do.
            return dry_run(1, 0);
        }
        // Every run writes files, and the operating system reports them.
        factory.emit_files(&left, 2);
        completed(3)
    }));
    w.rig
        .enqueue(&w.id, &w.profile, TriggerKind::RunNow, HostClass::App);
    for _ in 0..12 {
        tick(&w.host);
        w.advance(SEC);
        tick(&w.host);
        w.advance(3 * SEC);
        tick(&w.host);
        w.advance(11 * SEC);
        tick(&w.host);
    }
    assert!(
        w.has_event("Files keep changing during the runs"),
        "{:?}",
        w.events()
    );
    let runs = w.rig.runs().len();
    assert!(runs <= 8, "{runs} runs: the chain was not cut");
}

// ── Review R2 ──

/// R2-1: an automation made after the probe budget ran out is not given watches on a guess, and is
/// not left Unsupported when its folder turns out to be away (a USB drive that was pulled).
#[test]
fn an_automation_made_after_the_probe_budget_ran_out_waits_for_the_device_instead() {
    let rig = Rig::new();
    rig.world.set_script(script(|_, call| {
        if call.dry_run {
            dry_run(0, 0)
        } else {
            completed(3)
        }
    }));
    let mut made = Vec::new();
    for name in ["X", "Y"] {
        let (profile, _) = rig.fx.profile(name);
        let id = rig.fx.new_automation(name, &profile, watch(true, true));
        made.push((id, rig.fx.profile_policy(&profile).left.path));
    }
    // Both USB drives are out, and a watch of a folder that is not there cannot be made.
    for (_, left) in &made {
        rig.world.set_offline(left, true);
    }
    rig.world.factory.refuse.store(true, Ordering::SeqCst);
    rig.world.probe_delay_ms.store(1_200, Ordering::SeqCst);
    let host = rig.host(1, HostClass::App);
    host.tick(); // the first look of one automation uses the whole budget; the other waits
    rig.world.probe_delay_ms.store(0, Ordering::SeqCst);
    let state = |id: &str| {
        rig.fx
            .store
            .with_read(|c| runtime::get(c, id))
            .expect("read")
            .expect("row")
            .trigger_state
    };
    for _ in 0..4 {
        rig.advance(SEC);
        tick(&host);
    }
    for (id, _) in &made {
        assert_eq!(state(id), Some(TriggerState::Device), "{id}");
    }
    assert!(rig.world.factory.asked().is_empty(), "no watch on a guess");
    // The drives are back: both recover (nothing is stuck Unsupported).
    rig.world.factory.refuse.store(false, Ordering::SeqCst);
    for (_, left) in &made {
        rig.world.set_offline(left, false);
    }
    host.volume_changed();
    for _ in 0..4 {
        rig.advance(SEC);
        tick(&host);
    }
    for (id, _) in &made {
        assert_eq!(state(id), Some(TriggerState::Watching), "{id}");
    }
    assert_eq!(rig.world.factory.live(), 2);
}

/// R2-2 and the replaced folder: the 30 s look also applies to a local root that is waiting, so a
/// folder restored from the Recycle Bin (same identity, no volume event) is found; a folder that
/// was replaced says so.
#[test]
fn a_waiting_local_root_is_found_again_without_a_volume_event_and_a_replaced_one_says_so() {
    let w = setup();
    w.started();
    // Replaced behind the watcher's back: the health look finds it.
    w.rig.world.set_swapped(&w.left, true);
    w.advance(31 * SEC);
    w.host.tick();
    assert_eq!(w.state(), TriggerState::Device);
    assert!(
        w.has_event(&format!(
            "{} is not the approved folder \u{2014} edit the profile to use it",
            w.left
        )),
        "{:?}",
        w.events()
    );
    assert!(!w.has_event("not connected"));
    // The approved folder is restored (rename back): no event of any kind arrives.
    w.rig.world.set_swapped(&w.left, false);
    w.advance(10 * SEC);
    w.host.tick();
    assert_eq!(
        w.state(),
        TriggerState::Device,
        "not within 30 s of the last look"
    );
    w.advance(25 * SEC);
    tick(&w.host);
    assert_eq!(w.state(), TriggerState::Watching);
    assert_eq!(w.live(), 1);
    assert!(w.has_event("available again"));
}

/// A pass that finds nothing to change writes nothing for the automation: its row and its log
/// are as they were (the host's own 30 s housekeeping may still bump the counter).
#[test]
fn a_health_look_that_finds_nothing_writes_nothing_for_the_automation() {
    let w = setup();
    w.started();
    w.advance(5 * SEC);
    w.host.tick();
    let row = w.row();
    let events = w.events().len();
    let probes = w.rig.world.probes().len();
    w.advance(31 * SEC);
    w.host.tick();
    assert!(w.rig.world.probes().len() > probes, "the health look ran");
    assert_eq!(w.row(), row, "the row was not touched");
    assert_eq!(w.events().len(), events);
}
