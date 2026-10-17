//! Shared fixtures of the automation approval tests (cfg(test) only): a store in a temp folder,
//! an injectable clock, in-memory or DPAPI keys, real profiles saved through `save_in` on real
//! temp folders (so root identities are genuine), and ready-made drafts.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use crate::automation::approval::{self, ApproveRequest, Deps, TestRequest};
use crate::automation::commands::{self, SaveRequest, SaveResultDto};
use crate::automation::dpapi::{ApprovalKeys, MemoryKeys};
use crate::automation::dto::{ApproveResultDto, TestResultDto};
use crate::automation::model::{
    AutomationDraft, AutomationPolicy, Conditions, Draft, DraftGuard, Freq, MissedPolicy,
    PolicyDraft, ProfilePolicy, Runtime, ScheduleTrigger, Trigger, WatchTrigger, WireCompare,
    WireDeletePolicy, WireMode, WireSide,
};
use crate::automation::store::Store;
use crate::automation::test_run::{DryRun, TestDeps, TestLedger};
use crate::sync_engine::opset::GuardVerdict;
use crate::sync_engine::plan::PlanSummary;
use crate::sync_engine::roots::{FsProbe, RootProbe};

/// Everything a test needs, around one temp directory.
pub(crate) struct Fixture {
    pub tmp: tempfile::TempDir,
    pub store: Arc<Store>,
    /// The keys the flows use (in-memory by default, DPAPI with [`Fixture::with_dpapi`]).
    pub keys: Arc<dyn ApprovalKeys>,
    /// The same keys as a `MemoryKeys` when they are in-memory ones (to flip their state).
    pub memory: Option<Arc<MemoryKeys>>,
    pub ledger: TestLedger,
    pub now: Arc<AtomicI64>,
    clock: Box<dyn Fn() -> i64 + Send + Sync>,
}

/// An instant in epoch milliseconds, far from any boundary.
pub(crate) const T0: i64 = 1_760_000_000_000;

impl Fixture {
    pub fn new() -> Self {
        let memory = Arc::new(MemoryKeys::with_key(9));
        Self::build(memory.clone(), Some(memory), None)
    }

    /// Real DPAPI key file in the store directory.
    pub fn with_dpapi() -> Self {
        Self::build(Arc::new(MemoryKeys::missing()), None, Some(()))
    }

    fn build(
        keys: Arc<dyn ApprovalKeys>,
        memory: Option<Arc<MemoryKeys>>,
        dpapi: Option<()>,
    ) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let store_dir = tmp.path().join("store");
        let store = Arc::new(Store::open(&store_dir, "0.2.0"));
        let keys: Arc<dyn ApprovalKeys> = if dpapi.is_some() {
            Arc::new(crate::automation::dpapi::DpapiKeyFile::new(&store_dir))
        } else {
            keys
        };
        let now = Arc::new(AtomicI64::new(T0));
        let clock_now = now.clone();
        Self {
            tmp,
            store,
            keys,
            memory,
            ledger: TestLedger::new(),
            now,
            clock: Box::new(move || clock_now.load(Ordering::SeqCst)),
        }
    }

    pub fn store_dir(&self) -> std::path::PathBuf {
        self.tmp.path().join("store")
    }

    pub fn advance(&self, ms: i64) {
        self.now.fetch_add(ms, Ordering::SeqCst);
    }

    pub fn deps(&self) -> Deps<'_> {
        Deps {
            keys: self.keys.as_ref(),
            ledger: &self.ledger,
            clock: &*self.clock,
        }
    }

    pub fn clock(&self) -> &(dyn Fn() -> i64 + Send + Sync) {
        &*self.clock
    }

    pub fn probe() -> Arc<dyn RootProbe> {
        Arc::new(FsProbe)
    }

    fn draft(&self, name: &str) -> Draft {
        let left = self.tmp.path().join(format!("{name}-left"));
        let right = self.tmp.path().join(format!("{name}-right"));
        std::fs::create_dir_all(&left).unwrap();
        std::fs::create_dir_all(&right).unwrap();
        Draft {
            name: name.into(),
            policy: PolicyDraft {
                left: left.to_string_lossy().into_owned(),
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
        }
    }

    fn save_profile(&self, request: SaveRequest) -> (String, i64) {
        let out = commands::save_in(
            &self.store,
            request,
            &Self::probe(),
            |_| true,
            self.clock()(),
        )
        .unwrap();
        match out {
            SaveResultDto::Saved { profile } => (profile.id, profile.revision),
            other => panic!("expected saved, got {other:?}"),
        }
    }

    /// A new profile on fresh temp folders. Returns `(id, revision)`.
    pub fn profile(&self, name: &str) -> (String, i64) {
        self.save_profile(SaveRequest {
            id: None,
            expected_revision: None,
            draft: self.draft(name),
        })
    }

    /// A new `permanent` profile (the save is confirmed by the injected dialog).
    pub fn permanent_profile(&self, name: &str) -> (String, i64) {
        let mut draft = self.draft(name);
        draft.policy.delete_policy = WireDeletePolicy::Permanent;
        self.save_profile(SaveRequest {
            id: None,
            expected_revision: None,
            draft,
        })
    }

    /// Save the profile again with one more exclude pattern: a policy change, so the revision
    /// bumps (and the SP-6.4 hook runs). Returns the new revision.
    pub fn bump_profile(&self, id: &str, revision: i64, pattern: &str) -> i64 {
        let current = commands::get_in(&self.store, id, &Self::probe()).unwrap();
        let mut policy = current.policy.clone();
        policy.exclude.push(pattern.into());
        let (_, new_revision) = self.save_profile(SaveRequest {
            id: Some(id.into()),
            expected_revision: Some(revision),
            draft: Draft {
                name: current.name,
                policy,
            },
        });
        new_revision
    }

    pub fn profile_policy(&self, id: &str) -> ProfilePolicy {
        self.store
            .with_read(|c| -> Result<_, crate::automation::store::StoreError> {
                let rec = crate::automation::store::profiles::get(c, id)?.unwrap();
                Ok(serde_json::from_str::<ProfilePolicy>(&rec.policy).unwrap())
            })
            .unwrap()
    }

    pub fn daily() -> AutomationPolicy {
        AutomationPolicy {
            trigger: Trigger::Schedule(ScheduleTrigger {
                freq: Freq::Daily {},
                time: "02:00".into(),
                tz: "Asia/Jakarta".into(),
                missed: MissedPolicy::RunOnce,
            }),
            runtime: Runtime::App,
            retries: 2,
            conditions: Conditions::default(),
        }
    }

    pub fn watch() -> AutomationPolicy {
        AutomationPolicy {
            trigger: Trigger::Watch(WatchTrigger {
                watch_left: true,
                watch_right: false,
                on_changes: true,
                on_available: true,
                idle_secs: 10,
                max_wait_secs: 900,
            }),
            runtime: Runtime::App,
            retries: 2,
            conditions: Conditions::default(),
        }
    }

    pub fn auto_draft(name: &str, profile_id: &str, policy: AutomationPolicy) -> AutomationDraft {
        AutomationDraft {
            name: name.into(),
            profile_id: profile_id.into(),
            policy,
        }
    }
}

pub(crate) fn passing_dry_run() -> impl Fn(&ProfilePolicy) -> DryRun {
    |_| DryRun::Done {
        summary: PlanSummary {
            total: 6,
            to_right: 6,
            overwrites: 2,
            ..Default::default()
        },
        conflicts: 0,
        guard: GuardVerdict::Ok,
    }
}

/// Run `f` with test deps whose dry run, access check and runtime readiness are the defaults of
/// a healthy machine (every folder accessible, passing dry run, no background runner).
pub(crate) fn with_test_deps<T>(f: impl FnOnce(&TestDeps<'_>) -> T) -> T {
    let probe = Fixture::probe();
    let dry = passing_dry_run();
    let access = |_: &str, _: bool| true;
    let deps = TestDeps {
        probe: &probe,
        budget: std::time::Duration::from_secs(2),
        dry_run: &dry,
        access: &access,
        background_ready: false,
    };
    f(&deps)
}

impl Fixture {
    /// Test a draft with healthy defaults; `id` = the automation being edited.
    pub fn test(&self, id: Option<&str>, draft: &AutomationDraft) -> TestResultDto {
        with_test_deps(|deps| {
            approval::test_in(
                &self.store,
                &self.ledger,
                TestRequest {
                    id: id.map(str::to_string),
                    draft: draft.clone(),
                },
                deps,
                self.clock(),
            )
            .unwrap()
        })
    }

    /// Approve with a dialog that answers `yes`.
    pub fn approve(
        &self,
        id: Option<&str>,
        expected_config_rev: Option<i64>,
        draft: &AutomationDraft,
        test_id: &str,
        yes: bool,
    ) -> ApproveResultDto {
        approval::approve_in(
            &self.store,
            ApproveRequest {
                id: id.map(str::to_string),
                expected_config_rev,
                draft: draft.clone(),
                test_id: test_id.into(),
            },
            &self.deps(),
            |_| yes,
        )
        .unwrap()
    }

    /// Test and approve a NEW automation (dialog answers yes); returns its id.
    pub fn new_automation(&self, name: &str, profile_id: &str, policy: AutomationPolicy) -> String {
        let draft = Self::auto_draft(name, profile_id, policy);
        let test = self.test(None, &draft);
        assert!(test.passed, "{:?}", test.steps);
        match self.approve(None, None, &draft, &test.test_id, true) {
            ApproveResultDto::Approved { automation } => automation.id,
            other => panic!("expected approved, got {other:?}"),
        }
    }
}
