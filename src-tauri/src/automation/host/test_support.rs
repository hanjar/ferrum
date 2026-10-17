//! Fixtures of the host tests (cfg(test) only): a fake world (virtual clock, scripted engine,
//! counting keys, recorded events) around the shared `Fixture` of the approval tests, so a test
//! drives `Host::tick()` with a fake clock and sees every engine call, event and store row.

use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::gate::{GateOk, RunMode};
use super::{EngineCall, Host, HostDeps, HostEvent};
use crate::automation::approval;
use crate::automation::dpapi::{ApprovalKey, ApprovalKeys, KeyError, KeyLoad};
use crate::automation::dto::ApproveResultDto;
use crate::automation::lease::LeaseEnv;
use crate::automation::model::{AutomationPolicy, Conditions, Runtime};
use crate::automation::store::hosts::{self, HostRow};
use crate::automation::store::leases::Holder;
use crate::automation::store::leases::test_support::FakeProbe;
use crate::automation::store::queue::{self, HostClass, NewEntry, TriggerKind};
use crate::automation::store::{StoreError, runs};
use crate::automation::test_run::{DryRun, TestDeps};
use crate::automation::test_support::Fixture;
use crate::automation::watcher::{Avail, RawEvent, Sink, WatchError, WatchFactory, WatchHandle};
use crate::sync_engine::env::EngineEnv;
use crate::sync_engine::execute::{
    Counts, EngineError, EngineOutcome, EngineReport, OpRecord, SettleState,
};
use crate::sync_engine::opset::{GuardVerdict, OpStatus};
use crate::sync_engine::plan::{PlanSummary, SyncAction};
use crate::sync_engine::roots::FsProbe;
use crate::sync_engine::test_support::test_env;

/// Keys that count how often the approval key is read: the gate reads it once per `verify`.
pub struct CountingKeys {
    inner: Arc<dyn ApprovalKeys>,
    pub loads: AtomicUsize,
}

impl CountingKeys {
    pub fn new(inner: Arc<dyn ApprovalKeys>) -> Arc<Self> {
        Arc::new(Self {
            inner,
            loads: AtomicUsize::new(0),
        })
    }

    pub fn count(&self) -> usize {
        self.loads.load(Ordering::SeqCst)
    }
}

impl ApprovalKeys for CountingKeys {
    fn load(&self) -> KeyLoad {
        self.loads.fetch_add(1, Ordering::SeqCst);
        self.inner.load()
    }

    fn load_or_create(&self) -> Result<ApprovalKey, KeyError> {
        self.inner.load_or_create()
    }

    fn create_if_missing(&self) -> Result<ApprovalKey, KeyError> {
        self.inner.create_if_missing()
    }
}

/// What the scripted engine saw.
#[derive(Debug, Clone)]
pub struct EngineLog {
    pub automation_id: String,
    pub run_id: i64,
    pub dry_run: bool,
    pub mode: RunMode,
    pub holds_lease: bool,
    pub lease_covers: bool,
    pub verify_loads_before: usize,
}

pub type Script = Arc<dyn Fn(&GateOk, &EngineCall<'_>) -> EngineReport + Send + Sync>;

/// The shared, inspectable part of a fake host environment.
pub struct World {
    pub clock: Arc<AtomicI64>,
    pub keys: Arc<CountingKeys>,
    pub probe: Arc<FakeProbe>,
    pub script: Mutex<Script>,
    pub log: Mutex<Vec<EngineLog>>,
    pub events: Mutex<Vec<HostEvent>>,
    pub unmet: Mutex<Option<&'static str>>,
    pub bin: Mutex<crate::sync_engine::env::BinState>,
    /// Runs between the first and the second verification of a gate (outside any store closure):
    /// tests change the world "while the gate works".
    pub between: Mutex<Option<Box<dyn Fn() + Send>>>,
    /// The ONE source of change events of the watch triggers: the tests feed it by hand.
    pub factory: Arc<FakeFactory>,
    /// Roots (lowercase) that `root_available` reports as gone.
    pub offline: Mutex<Vec<String>>,
    /// Every root `root_available` was asked about, in order.
    pub probe_log: Mutex<Vec<String>>,
    /// Real milliseconds every `root_available` call takes (a slow share).
    pub probe_delay_ms: std::sync::atomic::AtomicU64,
}

impl World {
    pub fn calls(&self) -> Vec<EngineLog> {
        self.log.lock().unwrap().clone()
    }

    pub fn set_script(&self, script: Script) {
        *self.script.lock().unwrap() = script;
    }

    pub fn emitted(&self) -> Vec<HostEvent> {
        self.events.lock().unwrap().clone()
    }

    /// Mark a root as gone (`true`) or back (`false`) for the availability probe.
    pub fn set_offline(&self, root: &str, gone: bool) {
        let mut v = self.offline.lock().unwrap();
        v.retain(|r| r != &root.to_lowercase());
        if gone {
            v.push(root.to_lowercase());
        }
    }

    pub fn probes(&self) -> Vec<String> {
        self.probe_log.lock().unwrap().clone()
    }
}

// ── The fake watcher factory ──

struct FakeWatch {
    id: usize,
    root: String,
    sink: Sink,
}

/// The only source of change events in the watch tests (design §9.2). `watch` records the roots it
/// was asked for; `emit` delivers an event to every live watch of a root; dropping a handle
/// removes its watch, so `live()` shows exactly what the host still holds.
#[derive(Default)]
pub struct FakeFactory {
    next: AtomicUsize,
    watches: Arc<Mutex<Vec<FakeWatch>>>,
    asked: Mutex<Vec<String>>,
    /// While `true`, `watch` fails (a root the backend cannot watch).
    pub refuse: std::sync::atomic::AtomicBool,
}

struct FakeHandle {
    id: usize,
    watches: Arc<Mutex<Vec<FakeWatch>>>,
}

impl WatchHandle for FakeHandle {}

impl Drop for FakeHandle {
    fn drop(&mut self) {
        self.watches.lock().unwrap().retain(|w| w.id != self.id);
    }
}

impl WatchFactory for FakeFactory {
    fn watch(&self, root: &str, sink: Sink) -> Result<Box<dyn WatchHandle>, WatchError> {
        self.asked.lock().unwrap().push(root.to_string());
        if self.refuse.load(Ordering::SeqCst) {
            return Err(WatchError::Failed);
        }
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        self.watches.lock().unwrap().push(FakeWatch {
            id,
            root: root.to_string(),
            sink,
        });
        Ok(Box::new(FakeHandle {
            id,
            watches: self.watches.clone(),
        }))
    }
}

impl FakeFactory {
    /// Deliver `event` to every live watch of `root` (case-insensitive).
    pub fn emit(&self, root: &str, event: RawEvent) {
        let sinks: Vec<Sink> = self
            .watches
            .lock()
            .unwrap()
            .iter()
            .filter(|w| w.root.eq_ignore_ascii_case(root))
            .map(|w| w.sink.clone())
            .collect();
        for sink in sinks {
            sink(event.clone());
        }
    }

    /// `n` change events for files directly under `root`.
    pub fn emit_files(&self, root: &str, n: usize) {
        for i in 0..n {
            self.emit(
                root,
                RawEvent::Change(vec![format!("{}\\f{i}.txt", root.trim_end_matches('\\'))]),
            );
        }
    }

    /// Watches the host holds right now.
    pub fn live(&self) -> usize {
        self.watches.lock().unwrap().len()
    }

    /// Every root the host asked to watch, in order.
    pub fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }
}

pub struct FakeDeps {
    pub world: Arc<World>,
    lease_env: LeaseEnv,
}

impl HostDeps for FakeDeps {
    fn now_ms(&self) -> i64 {
        self.world.clock.load(Ordering::SeqCst)
    }

    fn keys(&self) -> &dyn ApprovalKeys {
        self.world.keys.as_ref()
    }

    fn lease_env(&self) -> &LeaseEnv {
        &self.lease_env
    }

    fn engine_env(&self) -> EngineEnv {
        let mut env = test_env();
        env.bin = Arc::new(crate::sync_engine::test_support::FixedBin(
            *self.world.bin.lock().unwrap(),
        ));
        env
    }

    fn run_engine(&self, proof: &GateOk, call: &EngineCall<'_>) -> EngineReport {
        self.world.log.lock().unwrap().push(EngineLog {
            automation_id: proof.automation_id().to_string(),
            run_id: call.run_id,
            dry_run: call.dry_run,
            mode: proof.mode(),
            holds_lease: proof.lease().is_some(),
            lease_covers: proof
                .lease()
                .is_some_and(|l| l.covers(&proof.roots().lease_roots())),
            verify_loads_before: self.world.keys.count(),
        });
        let script = self.world.script.lock().unwrap().clone();
        script(proof, call)
    }

    fn unmet_condition(&self, _: &Conditions) -> Option<&'static str> {
        if let Some(hook) = self.world.between.lock().unwrap().as_ref() {
            hook();
        }
        *self.world.unmet.lock().unwrap()
    }

    fn spawn(&self, name: &'static str, job: Box<dyn FnOnce() + Send>) -> bool {
        std::thread::Builder::new()
            .name(name.into())
            .spawn(job)
            .is_ok()
    }

    fn emit(&self, event: &HostEvent) {
        self.world.events.lock().unwrap().push(event.clone());
    }

    fn incremental(&self) -> bool {
        false
    }

    fn watch_factory(&self) -> &dyn WatchFactory {
        self.world.factory.as_ref()
    }

    fn root_available(&self, root: &crate::automation::model::RootSpec) -> Avail {
        self.world.probe_log.lock().unwrap().push(root.path.clone());
        let delay = self.world.probe_delay_ms.load(Ordering::SeqCst);
        if delay > 0 {
            std::thread::sleep(Duration::from_millis(delay));
        }
        if self
            .world
            .offline
            .lock()
            .unwrap()
            .contains(&root.path.to_lowercase())
        {
            Avail::No
        } else {
            Avail::Yes
        }
    }
}

// ── Reports ──

pub fn op(index: usize, name: &str, action: SyncAction, status: OpStatus) -> OpRecord {
    OpRecord {
        index,
        name: name.to_string(),
        action,
        status,
        reason: None,
    }
}

fn counts(total: u32, done: u32, failed: u32, not_started: u32, held: u32) -> Counts {
    Counts {
        total,
        done,
        failed,
        not_started,
        held,
        ..Counts::default()
    }
}

fn report(outcome: EngineOutcome, ops: Vec<OpRecord>) -> EngineReport {
    EngineReport {
        outcome,
        ops,
        pending_rerun: false,
        settle: SettleState::NotAttempted,
        fingerprint: None,
        incremental: None,
        blockers: Vec::new(),
    }
}

pub fn completed(n: u32) -> EngineReport {
    let ops = (0..n as usize)
        .map(|i| {
            op(
                i,
                &format!("file{i}.txt"),
                SyncAction::CopyToRight,
                OpStatus::Done,
            )
        })
        .collect();
    report(
        EngineOutcome::Completed {
            counts: counts(n, n, 0, 0, 0),
        },
        ops,
    )
}

pub fn nothing_to_do() -> EngineReport {
    report(EngineOutcome::NothingToDo, vec![])
}

/// 6 operations: `done` succeed, `failed` fail, the rest are not started (RQ-7.2).
pub fn partial(done: u32, failed: u32, not_started: u32) -> EngineReport {
    let total = done + failed + not_started;
    let mut ops = Vec::new();
    for i in 0..total {
        let status = if i < done {
            OpStatus::Done
        } else if i < done + failed {
            OpStatus::Failed
        } else {
            OpStatus::NotStarted
        };
        ops.push(op(
            i as usize,
            &format!("f{i}.txt"),
            SyncAction::CopyToRight,
            status,
        ));
    }
    report(
        EngineOutcome::Partial {
            counts: counts(total, done, failed, not_started, 0),
            error: EngineError {
                code: "access_denied",
                text: "Not retried automatically \u{2014} permission errors need you. Fix access, then Recheck."
                    .to_string(),
            },
        },
        ops,
    )
}

pub fn conflict_held(held: u32) -> EngineReport {
    let ops = (0..held as usize)
        .map(|i| {
            op(
                i,
                &format!("c{i}.txt"),
                SyncAction::Conflict,
                OpStatus::Held,
            )
        })
        .collect();
    report(
        EngineOutcome::ConflictHeld {
            counts: counts(held, 0, 0, 0, held),
        },
        ops,
    )
}

pub fn cancelled(done: u32, total: u32) -> EngineReport {
    report(
        EngineOutcome::Cancelled {
            counts: counts(total, done, 0, total - done, 0),
        },
        vec![],
    )
}

pub fn interrupted(
    done: u32,
    total: u32,
    why: crate::sync_engine::InterruptReason,
) -> EngineReport {
    report(
        EngineOutcome::Interrupted {
            counts: counts(total, done, 0, total - done, 0),
            why,
        },
        vec![],
    )
}

pub fn failed(code: &'static str, text: &str) -> EngineReport {
    report(
        EngineOutcome::Failed {
            error: EngineError {
                code,
                text: text.to_string(),
            },
        },
        vec![],
    )
}

/// A recheck answer: `remaining` operations are still to do, `conflicts` are held.
pub fn dry_run(remaining: usize, conflicts: u32) -> EngineReport {
    report(
        EngineOutcome::DryRun {
            plan: PlanSummary {
                total: remaining + conflicts as usize,
                to_right: remaining,
                conflicts: conflicts as usize,
                ..PlanSummary::default()
            },
            conflicts,
            guard: GuardVerdict::Ok,
        },
        vec![],
    )
}

pub fn script(
    f: impl Fn(&GateOk, &EngineCall<'_>) -> EngineReport + Send + Sync + 'static,
) -> Script {
    Arc::new(f)
}

/// The default engine: reports a little progress and completes 3 operations.
pub fn default_script() -> Script {
    script(|_, call| {
        call.sink
            .phase(crate::sync_engine::execute::Phase::Comparing);
        call.sink.phase(crate::sync_engine::execute::Phase::Syncing);
        completed(3)
    })
}

// ── The rig ──

pub struct Rig {
    pub fx: Fixture,
    pub world: Arc<World>,
}

impl Rig {
    /// A rig with automatic runs ON (the behaviour the host tests describe); a test of the
    /// default (off) calls `set_unattended_for_tests(false)` before it makes its host.
    pub fn new() -> Rig {
        crate::automation::host::set_unattended_for_tests(true);
        let fx = Fixture::new();
        let keys = CountingKeys::new(fx.keys.clone());
        let world = Arc::new(World {
            clock: fx.now.clone(),
            keys,
            probe: Arc::new(FakeProbe::default()),
            script: Mutex::new(default_script()),
            log: Mutex::new(Vec::new()),
            events: Mutex::new(Vec::new()),
            unmet: Mutex::new(None),
            bin: Mutex::new(crate::sync_engine::env::BinState::Usable),
            between: Mutex::new(None),
            factory: Arc::new(FakeFactory::default()),
            offline: Mutex::new(Vec::new()),
            probe_log: Mutex::new(Vec::new()),
            probe_delay_ms: std::sync::atomic::AtomicU64::new(0),
        });
        Rig { fx, world }
    }

    /// Lease identity and clock of a process named `pid` (its own heartbeat on the fake clock).
    pub fn lease_env(&self, pid: i64) -> LeaseEnv {
        let probe: Arc<dyn crate::automation::store::leases::ProcessProbe> =
            self.world.probe.clone();
        let clock = self.world.clock.clone();
        LeaseEnv {
            clock: Arc::new(move || clock.load(Ordering::SeqCst)),
            probe,
            holder: Holder {
                pid,
                created: pid * 10,
            },
            beat_every: Duration::from_millis(10),
            max_manual_hold_ms: crate::automation::lease::MAX_MANUAL_HOLD_MS,
        }
    }

    /// A host of `class` for a process named `pid`. The loop is driven by the test through
    /// `tick()`.
    pub fn host(&self, pid: i64, class: HostClass) -> Host<FakeDeps> {
        Host::new(
            self.fx.store.clone(),
            FakeDeps {
                world: self.world.clone(),
                lease_env: self.lease_env(pid),
            },
            class,
            "0.2.0",
        )
    }

    /// A host over another store (a read-only one, for example).
    pub fn host_on(
        &self,
        store: Arc<crate::automation::store::Store>,
        pid: i64,
        class: HostClass,
    ) -> Host<FakeDeps> {
        Host::new(
            store,
            FakeDeps {
                world: self.world.clone(),
                lease_env: self.lease_env(pid),
            },
            class,
            "0.2.0",
        )
    }

    pub fn now(&self) -> i64 {
        self.world.clock.load(Ordering::SeqCst)
    }

    pub fn advance(&self, ms: i64) {
        self.world.clock.fetch_add(ms, Ordering::SeqCst);
    }

    /// A new approved app automation on a new profile. Returns `(automation id, profile id)`.
    pub fn automation(&self, name: &str) -> (String, String) {
        let (profile, _) = self.fx.profile(&format!("Profile of {name}"));
        let id = self.fx.new_automation(name, &profile, Fixture::daily());
        (id, profile)
    }

    /// A new approved automation on a MIRROR profile (it deletes extras on the right).
    pub fn mirror_automation(&self, name: &str) -> (String, String) {
        use crate::automation::commands::{SaveRequest, SaveResultDto};
        use crate::automation::model::{
            Draft, DraftGuard, PolicyDraft, WireCompare, WireDeletePolicy, WireMode, WireSide,
        };
        let left = self.fx.tmp.path().join(format!("{name}-mirror-left"));
        let right = self.fx.tmp.path().join(format!("{name}-mirror-right"));
        std::fs::create_dir_all(&left).unwrap();
        std::fs::create_dir_all(&right).unwrap();
        let probe: Arc<dyn crate::sync_engine::roots::RootProbe> = Arc::new(FsProbe);
        let saved = crate::automation::commands::save_in(
            &self.fx.store,
            SaveRequest {
                id: None,
                expected_revision: None,
                draft: Draft {
                    name: format!("Mirror of {name}"),
                    policy: PolicyDraft {
                        left: left.to_string_lossy().into_owned(),
                        right: right.to_string_lossy().into_owned(),
                        mode: WireMode::Mirror,
                        from: Some(WireSide::Left),
                        propagate_deletes: None,
                        delete_policy: WireDeletePolicy::RecycleBin,
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
                },
            },
            &probe,
            |_| true,
            self.now(),
        )
        .unwrap();
        let profile = match saved {
            SaveResultDto::Saved { profile } => profile.id,
            other => panic!("expected saved, got {other:?}"),
        };
        let id = self.fx.new_automation(name, &profile, Fixture::daily());
        (id, profile)
    }

    /// A new approved automation on a profile that shares its LEFT folder with another one (the
    /// folders overlap, so their runs wait for each other).
    pub fn overlapping_automation(&self, name: &str, left: &str) -> (String, String) {
        use crate::automation::commands::{SaveRequest, SaveResultDto};
        use crate::automation::model::{
            Draft, DraftGuard, PolicyDraft, WireCompare, WireDeletePolicy, WireMode, WireSide,
        };
        let right = self.fx.tmp.path().join(format!("{name}-overlap-right"));
        std::fs::create_dir_all(&right).unwrap();
        let probe: Arc<dyn crate::sync_engine::roots::RootProbe> = Arc::new(FsProbe);
        let saved = crate::automation::commands::save_in(
            &self.fx.store,
            SaveRequest {
                id: None,
                expected_revision: None,
                draft: Draft {
                    name: format!("Overlap of {name}"),
                    policy: PolicyDraft {
                        left: left.to_string(),
                        right: right.to_string_lossy().into_owned(),
                        mode: WireMode::OneWay,
                        from: Some(WireSide::Left),
                        propagate_deletes: None,
                        delete_policy: WireDeletePolicy::RecycleBin,
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
                },
            },
            &probe,
            |_| true,
            self.now(),
        )
        .unwrap();
        let profile = match saved {
            SaveResultDto::Saved { profile } => profile.id,
            other => panic!("expected saved, got {other:?}"),
        };
        let id = self.fx.new_automation(name, &profile, Fixture::daily());
        (id, profile)
    }

    /// A second automation on an existing profile.
    pub fn automation_on(&self, name: &str, profile: &str) -> String {
        self.fx.new_automation(name, profile, Fixture::watch())
    }

    /// A new approved BACKGROUND automation (the test is run with a ready runner).
    pub fn background_automation(&self, name: &str) -> (String, String) {
        let (profile, _) = self.fx.profile(&format!("Profile of {name}"));
        let mut policy: AutomationPolicy = Fixture::daily();
        policy.runtime = Runtime::Background;
        let draft = Fixture::auto_draft(name, &profile, policy);
        let probe: Arc<dyn crate::sync_engine::roots::RootProbe> = Arc::new(FsProbe);
        let dry = |_: &crate::automation::model::ProfilePolicy| DryRun::Done {
            summary: PlanSummary::default(),
            conflicts: 0,
            guard: GuardVerdict::Ok,
        };
        let access = |_: &str, _: bool| true;
        let deps = TestDeps {
            probe: &probe,
            budget: Duration::from_secs(2),
            dry_run: &dry,
            access: &access,
            background_ready: true,
        };
        let test = approval::test_in(
            &self.fx.store,
            &self.fx.ledger,
            approval::TestRequest {
                id: None,
                draft: draft.clone(),
            },
            &deps,
            self.fx.clock(),
        )
        .unwrap();
        assert!(test.passed, "{:?}", test.steps);
        match self.fx.approve(None, None, &draft, &test.test_id, true) {
            ApproveResultDto::Approved { automation } => (automation.id, profile),
            other => panic!("expected approved, got {other:?}"),
        }
    }

    /// Put a trigger of `kind` in the queue exactly as a trigger source would.
    pub fn enqueue(&self, automation: &str, profile: &str, kind: TriggerKind, class: HostClass) {
        self.fx
            .store
            .with_write(|conn| -> Result<(), StoreError> {
                queue::enqueue(
                    conn,
                    &NewEntry {
                        automation_id: automation,
                        profile_id: profile,
                        host_class: class,
                        kind,
                        text: "Test trigger",
                        retry_of: (kind == TriggerKind::Retry).then_some(1),
                    },
                    self.now(),
                )?;
                Ok(())
            })
            .unwrap();
    }

    pub fn queue_len(&self) -> usize {
        self.fx
            .store
            .with_read(queue::all)
            .map(|v| v.len())
            .unwrap()
    }

    pub fn sql(&self, sql: &str) {
        self.fx
            .store
            .with_write(|conn| -> Result<(), StoreError> {
                conn.execute_batch(sql).map_err(|_| StoreError::Unavailable)
            })
            .unwrap();
    }

    pub fn runs(&self) -> Vec<runs::RunRecord> {
        let mut all = self
            .fx
            .store
            .with_read(|c| -> Result<Vec<runs::RunRecord>, StoreError> {
                runs::history(c, runs::HistoryFilter::All, None, None, 100)
            })
            .unwrap();
        all.reverse();
        all
    }

    pub fn event_texts(&self, automation: &str) -> Vec<String> {
        self.fx
            .store
            .with_read(|c| -> Result<Vec<String>, StoreError> {
                Ok(
                    crate::automation::store::events::events(c, automation, 200)?
                        .into_iter()
                        .rev()
                        .map(|e| e.text)
                        .collect(),
                )
            })
            .unwrap()
    }

    pub fn status_kind(&self, automation: &str) -> crate::automation::status::StatusKind {
        let rows =
            crate::automation::read::list_in(&self.fx.store, self.fx.keys.as_ref(), self.now())
                .unwrap();
        rows.into_iter()
            .find(|r| r.id == automation)
            .unwrap()
            .status
            .kind
    }

    pub fn status_label(&self, automation: &str) -> String {
        let rows =
            crate::automation::read::list_in(&self.fx.store, self.fx.keys.as_ref(), self.now())
                .unwrap();
        rows.into_iter()
            .find(|r| r.id == automation)
            .unwrap()
            .status
            .label
    }

    pub fn status_sub(&self, automation: &str) -> Option<String> {
        let rows =
            crate::automation::read::list_in(&self.fx.store, self.fx.keys.as_ref(), self.now())
                .unwrap();
        rows.into_iter()
            .find(|r| r.id == automation)
            .unwrap()
            .status
            .sub
    }

    pub fn host_rows(&self) -> Vec<HostRow> {
        self.fx.store.with_read(hosts::all).unwrap()
    }
}

/// Wait (real time) until the host has nothing in flight.
pub fn settle(host: &Host<FakeDeps>) {
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    // Two passes: a worker may still be about to start when the first check runs.
    for _ in 0..2 {
        while !host.is_idle() {
            assert!(std::time::Instant::now() < deadline, "host did not go idle");
            std::thread::sleep(Duration::from_millis(5));
        }
        std::thread::sleep(Duration::from_millis(15));
    }
}

/// Tick and wait for whatever it started.
pub fn tick(host: &Host<FakeDeps>) {
    host.tick();
    settle(host);
}
